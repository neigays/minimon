//! 聚合纯函数与行类型（不接触数据库）。
//!
//! 聚合在 Rust 里完成（纯函数），以 `INSERT OR REPLACE` 写入，因此幂等。

pub const ROLLUP_LAG_MS: i64 = 30_000;
pub const MINUTE_MS: i64 = 60_000;
pub const QUARTER_MS: i64 = 15 * MINUTE_MS;

#[derive(Debug, Clone, Default)]
pub struct RawSample {
    pub ts_ms: i64,
    pub cpu: f64,
    pub mem_used: i64,
    pub swap_used: i64,
    pub disk_used: i64,
    pub disk_total: i64,
    pub net_rx_bytes: i64,
    pub net_tx_bytes: i64,
    pub net_rx_rate: i64,
    pub net_tx_rate: i64,
    pub load1: f64,
    pub load5: f64,
    pub load15: f64,
    pub uptime_secs: i64,
    pub process_count: i64,
    pub tcp_count: i64,
    pub udp_count: i64,
}

impl RawSample {
    fn avg_fields(&self) -> [f64; 13] {
        [
            self.cpu,
            self.mem_used as f64,
            self.swap_used as f64,
            self.disk_used as f64,
            self.disk_total as f64,
            self.net_rx_rate as f64,
            self.net_tx_rate as f64,
            self.load1,
            self.load5,
            self.load15,
            self.process_count as f64,
            self.tcp_count as f64,
            self.udp_count as f64,
        ]
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct AggRow {
    pub node_id: i64,
    pub ts_ms: i64,
    pub samples: i64,
    pub cpu: f64,
    pub mem_used: f64,
    pub swap_used: f64,
    pub disk_used: f64,
    pub disk_total: f64,
    pub net_rx_rate: f64,
    pub net_tx_rate: f64,
    pub load1: f64,
    pub load5: f64,
    pub load15: f64,
    pub process_count: f64,
    pub tcp_count: f64,
    pub udp_count: f64,
    pub cpu_max: f64,
    pub mem_used_max: i64,
    pub net_rx_rate_max: i64,
    pub net_tx_rate_max: i64,
    pub net_rx_delta: i64,
    pub net_tx_delta: i64,
    pub net_rx_bytes: i64,
    pub net_tx_bytes: i64,
    pub uptime_secs: i64,
}

impl AggRow {
    fn avg_fields(&self) -> [f64; 13] {
        [
            self.cpu,
            self.mem_used,
            self.swap_used,
            self.disk_used,
            self.disk_total,
            self.net_rx_rate,
            self.net_tx_rate,
            self.load1,
            self.load5,
            self.load15,
            self.process_count,
            self.tcp_count,
            self.udp_count,
        ]
    }

    fn set_avgs(&mut self, a: [f64; 13]) {
        [
            self.cpu,
            self.mem_used,
            self.swap_used,
            self.disk_used,
            self.disk_total,
            self.net_rx_rate,
            self.net_tx_rate,
            self.load1,
            self.load5,
            self.load15,
            self.process_count,
            self.tcp_count,
            self.udp_count,
        ] = a;
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RollupStats {
    pub m1: usize,
    pub m15: usize,
}

/// 累计计数器增量：首次（None）为 0；回落视为重启，增量取当前值。
pub fn counter_delta(prev: Option<i64>, cur: i64) -> i64 {
    match prev {
        None => 0,
        Some(p) if cur >= p => cur - p,
        Some(_) => cur,
    }
}

pub(super) fn bucket_start(ts_ms: i64, bucket_ms: i64) -> i64 {
    ts_ms - ts_ms.rem_euclid(bucket_ms)
}

/// samples 按 ts 升序；`prev_counters` 为第一个样本之前的 (rx, tx) 累计值。
pub fn aggregate_raw(
    node_id: i64,
    prev_counters: Option<(i64, i64)>,
    samples: &[RawSample],
    bucket_ms: i64,
) -> Vec<AggRow> {
    let mut out: Vec<AggRow> = Vec::new();
    let mut sums = [0f64; 13];
    let mut prev = prev_counters;
    for s in samples {
        let start = bucket_start(s.ts_ms, bucket_ms);
        if out.last().map(|r| r.ts_ms) != Some(start) {
            if let Some(last) = out.last_mut() {
                finish_avgs(last, &sums);
            }
            sums = [0f64; 13];
            out.push(AggRow {
                node_id,
                ts_ms: start,
                cpu_max: f64::MIN,
                ..Default::default()
            });
        }
        let Some(row) = out.last_mut() else { continue };
        for (acc, v) in sums.iter_mut().zip(s.avg_fields()) {
            *acc += v;
        }
        row.samples += 1;
        row.cpu_max = row.cpu_max.max(s.cpu);
        row.mem_used_max = row.mem_used_max.max(s.mem_used);
        row.net_rx_rate_max = row.net_rx_rate_max.max(s.net_rx_rate);
        row.net_tx_rate_max = row.net_tx_rate_max.max(s.net_tx_rate);
        row.net_rx_delta += counter_delta(prev.map(|p| p.0), s.net_rx_bytes);
        row.net_tx_delta += counter_delta(prev.map(|p| p.1), s.net_tx_bytes);
        row.net_rx_bytes = s.net_rx_bytes;
        row.net_tx_bytes = s.net_tx_bytes;
        row.uptime_secs = s.uptime_secs;
        prev = Some((s.net_rx_bytes, s.net_tx_bytes));
    }
    if let Some(last) = out.last_mut() {
        finish_avgs(last, &sums);
    }
    out
}

fn finish_avgs(row: &mut AggRow, sums: &[f64; 13]) {
    let n = row.samples.max(1) as f64;
    row.set_avgs(sums.map(|v| v / n));
}

/// rows 按 ts 升序（1m 行）；合并为更粗的桶。平均按 samples 加权。
pub fn merge_buckets(node_id: i64, rows: &[AggRow], bucket_ms: i64) -> Vec<AggRow> {
    let mut out: Vec<AggRow> = Vec::new();
    let mut sums = [0f64; 13];
    for r in rows {
        let start = bucket_start(r.ts_ms, bucket_ms);
        if out.last().map(|o| o.ts_ms) != Some(start) {
            if let Some(last) = out.last_mut() {
                finish_avgs(last, &sums);
            }
            sums = [0f64; 13];
            out.push(AggRow {
                node_id,
                ts_ms: start,
                cpu_max: f64::MIN,
                ..Default::default()
            });
        }
        let Some(row) = out.last_mut() else { continue };
        for (acc, v) in sums.iter_mut().zip(r.avg_fields()) {
            *acc += v * r.samples as f64;
        }
        row.samples += r.samples;
        row.cpu_max = row.cpu_max.max(r.cpu_max);
        row.mem_used_max = row.mem_used_max.max(r.mem_used_max);
        row.net_rx_rate_max = row.net_rx_rate_max.max(r.net_rx_rate_max);
        row.net_tx_rate_max = row.net_tx_rate_max.max(r.net_tx_rate_max);
        row.net_rx_delta += r.net_rx_delta;
        row.net_tx_delta += r.net_tx_delta;
        row.net_rx_bytes = r.net_rx_bytes;
        row.net_tx_bytes = r.net_tx_bytes;
        row.uptime_secs = r.uptime_secs;
    }
    if let Some(last) = out.last_mut() {
        finish_avgs(last, &sums);
    }
    out
}
