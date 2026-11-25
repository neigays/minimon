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

#[cfg(test)]
mod ts_order_tests {
    use super::*;
    use crate::clock::{TsNormalizer, normalize_ts};

    /// 返回 (采用的时间戳, 累计计数) 序列：agent 时钟偏差在 +9.9s / +10.1s 之间交替。
    fn reports() -> Vec<(i64, i64, i64)> {
        // (agent_ts, server_now, counter)
        (0..40)
            .map(|k| {
                let now = 10_000_000 + k * 5_000;
                let skew = if k % 2 == 0 { 9_900 } else { 10_100 };
                (
                    now + skew,
                    now + 100 * (k % 3),
                    3_000_000_000_000 + k * 1_000,
                )
            })
            .collect()
    }

    fn total_delta(mut samples: Vec<RawSample>) -> i64 {
        samples.sort_by_key(|s| s.ts_ms);
        let first = samples[0].net_rx_bytes;
        let rows = aggregate_raw(1, Some((first, 0)), &samples, MINUTE_MS);
        rows.iter().map(|r| r.net_rx_delta).sum()
    }

    fn sample(ts: i64, c: i64) -> RawSample {
        RawSample {
            ts_ms: ts,
            net_rx_bytes: c,
            ..Default::default()
        }
    }

    #[test]
    fn old_normalization_inflates_traffic() {
        let s: Vec<_> = reports()
            .into_iter()
            .map(|(a, now, c)| sample(normalize_ts(a, now).0, c))
            .collect();
        let true_delta = 39 * 1_000;
        assert!(
            total_delta(s) > true_delta * 1_000,
            "无修复时应出现整段累计计数被计入"
        );
    }

    #[test]
    fn sticky_monotonic_keeps_traffic_exact() {
        let mut n = TsNormalizer::new(None, 0);
        let mut last = i64::MIN;
        let s: Vec<_> = reports()
            .into_iter()
            .map(|(a, now, c)| {
                let ts = n.apply(a, now).0;
                assert!(ts > last, "时间戳必须严格递增");
                last = ts;
                sample(ts, c)
            })
            .collect();
        assert_eq!(total_delta(s), 39 * 1_000);
    }
}
