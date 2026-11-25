//! 系统指标采集。

pub mod conn;
pub mod filter;
pub mod rate;

use std::collections::HashSet;
use std::time::{SystemTime, UNIX_EPOCH};

use minimon_common::protocol::{Hello, PROTOCOL_VERSION, Report};
use sysinfo::{
    CpuRefreshKind, DiskRefreshKind, Disks, MemoryRefreshKind, Networks, ProcessRefreshKind,
    ProcessesToUpdate, RefreshKind, System,
};

use filter::{is_counted_fs, is_counted_iface};
use rate::RateTracker;
use tracing::warn;

/// 采集器。sysinfo 某部分初始化失败时，对应指标退化为 0，不影响其余部分。
pub struct Collector {
    sys: Option<System>,
    disks: Option<Disks>,
    nets: Option<Networks>,
    rate: RateTracker,
    /// load_average / uptime 失败只告警一次，避免每个上报周期刷屏。
    warned_runtime: bool,
}

impl Default for Collector {
    fn default() -> Self {
        Self::new()
    }
}

fn disk_kind() -> DiskRefreshKind {
    DiskRefreshKind::nothing().with_storage()
}

impl Collector {
    pub fn new() -> Self {
        let sys = System::new_with_specifics(
            RefreshKind::nothing()
                .with_cpu(CpuRefreshKind::everything())
                .with_memory(MemoryRefreshKind::everything()),
        )
        .map_err(|e| warn!("sysinfo System 初始化失败，CPU/内存指标将为 0: {e}"))
        .ok();
        let disks = Disks::new_with_refreshed_list_specifics(disk_kind())
            .map_err(|e| warn!("sysinfo Disks 初始化失败，磁盘指标将为 0: {e}"))
            .ok();
        let nets = Networks::new_with_refreshed_list()
            .map_err(|e| warn!("sysinfo Networks 初始化失败，网络指标将为 0: {e}"))
            .ok();
        Self {
            sys,
            disks,
            nets,
            rate: RateTracker::new(),
            warned_runtime: false,
        }
    }

    /// (已用, 总量)，仅统计计入的文件系统，并按设备名去重。
    fn disk_usage(&self) -> (u64, u64) {
        sum_disks(self.disks.iter().flat_map(|ds| ds.iter()).map(|d| {
            (
                d.file_system().to_string_lossy().into_owned(),
                d.name().to_string_lossy().into_owned(),
                d.total_space(),
                d.available_space(),
            )
        }))
    }

    pub fn hello(&mut self) -> Hello {
        if let Some(sys) = self.sys.as_mut() {
            sys.refresh_cpu_all();
            sys.refresh_memory();
        }
        if let Some(disks) = self.disks.as_mut() {
            disks.refresh_specifics(true, disk_kind());
        }
        let sys = self.sys.as_ref();
        let cpu_model = sys
            .and_then(|s| s.cpus().first())
            .map(|c| c.brand().trim().to_string())
            .unwrap_or_default();
        Hello {
            v: PROTOCOL_VERSION,
            agent_version: env!("CARGO_PKG_VERSION").to_string(),
            hostname: System::host_name().unwrap_or_else(|e| {
                warn!("读取主机名失败: {e}");
                String::new()
            }),
            os: System::long_os_version()
                .or_else(|_| System::name())
                .unwrap_or_default(),
            kernel: System::kernel_version().unwrap_or_default(),
            arch: std::env::consts::ARCH.to_string(),
            cpu_model,
            cpu_cores: sys.map_or(1, |s| s.cpus().len().max(1)) as u32,
            mem_total: sys.map_or(0, |s| s.total_memory()),
            swap_total: sys.map_or(0, |s| s.total_swap()),
            disk_total: self.disk_usage().1,
            virtualization: detect_virtualization(),
        }
    }

    pub fn report(&mut self) -> Report {
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        if let Some(sys) = self.sys.as_mut() {
            sys.refresh_cpu_usage();
            sys.refresh_memory();
            sys.refresh_processes_specifics(
                ProcessesToUpdate::All,
                true,
                ProcessRefreshKind::nothing(),
            );
        }
        if let Some(disks) = self.disks.as_mut() {
            disks.refresh_specifics(true, disk_kind());
        }
        if let Some(nets) = self.nets.as_mut() {
            nets.refresh(true);
        }

        let (disk_used, disk_total) = self.disk_usage();
        let (rx, tx) = self
            .nets
            .iter()
            .flat_map(|n| n.iter())
            .filter(|(name, _)| is_counted_iface(&name.to_string_lossy()))
            .fold((0u64, 0u64), |(r, t), (_, d)| {
                (r + d.total_received(), t + d.total_transmitted())
            });
        let (net_rx_rate, net_tx_rate) = self.rate.update(rx, tx, now_ms);
        let load = System::load_average();
        let uptime = System::uptime();
        if !self.warned_runtime && (load.is_err() || uptime.is_err()) {
            self.warned_runtime = true;
            warn!("读取负载或运行时间失败，相关指标将为 0（仅提示一次）");
        }
        let load = load.ok();
        let (tcp_count, udp_count) = conn::tcp_udp_counts();
        let sys = self.sys.as_ref();

        Report {
            ts_ms: now_ms,
            cpu: sys.map_or(0.0, |s| s.global_cpu_usage()).clamp(0.0, 100.0),
            mem_used: sys.map_or(0, |s| s.used_memory()),
            swap_used: sys.map_or(0, |s| s.used_swap()),
            disk_used,
            disk_total,
            net_rx_bytes: rx,
            net_tx_bytes: tx,
            net_rx_rate,
            net_tx_rate,
            load1: load.as_ref().map_or(0.0, |l| l.one),
            load5: load.as_ref().map_or(0.0, |l| l.five),
            load15: load.as_ref().map_or(0.0, |l| l.fifteen),
            uptime_secs: uptime.unwrap_or(0),
            process_count: sys.map_or(0, |s| s.processes().len()) as u32,
            tcp_count,
            udp_count,
        }
    }
}

/// 汇总磁盘用量。输入为 (文件系统类型, 设备名, 总量, 可用)；
/// 排除不计入的文件系统，按设备名去重，返回 (已用, 总量)。
fn sum_disks(disks: impl IntoIterator<Item = (String, String, u64, u64)>) -> (u64, u64) {
    let mut seen = HashSet::new();
    let (mut used, mut total) = (0u64, 0u64);
    for (fs, dev, size, avail) in disks {
        if !is_counted_fs(&fs) || !seen.insert(dev) {
            continue;
        }
        total += size;
        used += size.saturating_sub(avail);
    }
    (used, total)
}

/// 尽力而为地识别虚拟化环境：只读文件，不执行任何命令。
#[cfg(target_os = "linux")]
fn detect_virtualization() -> Option<String> {
    if std::path::Path::new("/.dockerenv").exists() {
        return Some("docker".to_string());
    }
    let name = std::fs::read_to_string("/sys/class/dmi/id/product_name").ok()?;
    let name = name.trim();
    (!name.is_empty()).then(|| name.to_string())
}

#[cfg(not(target_os = "linux"))]
fn detect_virtualization() -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(fs: &str, dev: &str, total: u64, avail: u64) -> (String, String, u64, u64) {
        (fs.into(), dev.into(), total, avail)
    }

    #[test]
    fn sum_disks_filters_dedups_and_saturates() {
        let disks = [
            d("ext4", "/dev/sda1", 100, 40),
            d("ext4", "/dev/sda1", 100, 40), // 同设备 bind mount，只算一次
            d("tmpfs", "tmpfs", 999, 0),     // 被排除
            d("xfs", "/dev/sdb1", 50, 80),   // available > total，已用饱和为 0
        ];
        assert_eq!(sum_disks(disks), (60, 150));
        assert_eq!(sum_disks([]), (0, 0));
    }

    #[test]
    fn report_is_sane() {
        let mut c = Collector::new();
        let h = c.hello();
        let r = c.report();
        assert_eq!(h.v, PROTOCOL_VERSION);
        assert!(h.cpu_cores >= 1);
        assert!(h.mem_total > 0);
        assert!((0.0..=100.0).contains(&r.cpu));
        assert!(r.mem_used <= h.mem_total);
        assert!(r.ts_ms > 1_700_000_000_000);
    }
}
