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

/// 采集器。sysinfo 某部分初始化失败时，对应指标退化为 0，不影响其余部分。
pub struct Collector {
    sys: Option<System>,
    disks: Option<Disks>,
    nets: Option<Networks>,
    rate: RateTracker,
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
        .ok();
        Self {
            sys,
            disks: Disks::new_with_refreshed_list_specifics(disk_kind()).ok(),
            nets: Networks::new_with_refreshed_list().ok(),
            rate: RateTracker::new(),
        }
    }

    /// (已用, 总量)，仅统计计入的文件系统，并按设备名去重。
    fn disk_usage(&self) -> (u64, u64) {
        let mut seen = HashSet::new();
        let (mut used, mut total) = (0u64, 0u64);
        for d in self.disks.iter().flat_map(|ds| ds.iter()) {
            if !is_counted_fs(&d.file_system().to_string_lossy()) {
                continue;
            }
            if !seen.insert(d.name().to_os_string()) {
                continue;
            }
            total += d.total_space();
            used += d.total_space().saturating_sub(d.available_space());
        }
        (used, total)
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
            hostname: System::host_name().unwrap_or_default(),
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
        let load = System::load_average().ok();
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
            uptime_secs: System::uptime().unwrap_or(0),
            process_count: sys.map_or(0, |s| s.processes().len()) as u32,
            tcp_count,
            udp_count,
        }
    }
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
