//! 指标取值：从 Report / 探测样本取规范单位的值，并格式化展示。

use minimon_common::protocol::Report;

use super::model::Metric;

fn pct(used: u64, total: u64) -> Option<f64> {
    (total > 0).then(|| used as f64 / total as f64 * 100.0)
}

/// 主机类指标（规范单位）；total 为 0 或探测类指标返回 `None`。
pub fn host_value(metric: Metric, r: &Report, mem_total: u64, swap_total: u64) -> Option<f64> {
    match metric {
        Metric::Cpu => Some(f64::from(r.cpu)),
        Metric::Mem => pct(r.mem_used, mem_total),
        Metric::Swap => pct(r.swap_used, swap_total),
        Metric::Disk => pct(r.disk_used, r.disk_total),
        Metric::Load1 => Some(r.load1),
        Metric::NetRxRate => Some(r.net_rx_rate as f64),
        Metric::NetTxRate => Some(r.net_tx_rate as f64),
        Metric::ProbeLatency | Metric::ProbeLoss => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProbeSample {
    pub ts_ms: i64,
    pub avg_ms: Option<f64>,
    /// 0.0–1.0
    pub loss: Option<f64>,
}

/// 探测类指标；超过 3 个 interval 未更新视为无数据。
pub fn probe_value(
    metric: Metric,
    s: &ProbeSample,
    interval_secs: i64,
    now_ms: i64,
) -> Option<f64> {
    if now_ms - s.ts_ms > 3 * interval_secs * 1000 {
        return None;
    }
    match metric {
        Metric::ProbeLatency => s.avg_ms,
        Metric::ProbeLoss => s.loss.map(|l| l * 100.0),
        _ => None,
    }
}

pub fn format_value(metric: Metric, v: f64) -> String {
    match metric {
        Metric::Cpu | Metric::Mem | Metric::Swap | Metric::Disk | Metric::ProbeLoss => {
            format!("{v:.1}%")
        }
        Metric::Load1 => format!("{v:.2}"),
        Metric::NetRxRate | Metric::NetTxRate => format!("{:.1} Mbps", v / 125_000.0),
        Metric::ProbeLatency => format!("{v:.0} ms"),
    }
}

#[cfg(test)]
mod tests {
    use super::super::model::Op;
    use super::*;

    fn report() -> Report {
        Report {
            ts_ms: 0,
            cpu: 50.0,
            mem_used: 500,
            swap_used: 10,
            disk_used: 30,
            disk_total: 120,
            net_rx_bytes: 0,
            net_tx_bytes: 0,
            net_rx_rate: 250_000,
            net_tx_rate: 125_000,
            load1: 1.25,
            load5: 0.0,
            load15: 0.0,
            uptime_secs: 0,
            process_count: 0,
            tcp_count: 0,
            udp_count: 0,
        }
    }

    #[test]
    fn host_values() {
        let r = report();
        assert_eq!(host_value(Metric::Cpu, &r, 1000, 100), Some(50.0));
        assert_eq!(host_value(Metric::Mem, &r, 1000, 100), Some(50.0));
        assert_eq!(host_value(Metric::Swap, &r, 1000, 100), Some(10.0));
        assert_eq!(host_value(Metric::Disk, &r, 1000, 100), Some(25.0));
        assert_eq!(host_value(Metric::Load1, &r, 1000, 100), Some(1.25));
        assert_eq!(
            host_value(Metric::NetRxRate, &r, 1000, 100),
            Some(250_000.0)
        );
        assert_eq!(
            host_value(Metric::NetTxRate, &r, 1000, 100),
            Some(125_000.0)
        );
        assert_eq!(host_value(Metric::ProbeLoss, &r, 1000, 100), None);
    }

    #[test]
    fn mem_without_total_is_no_data() {
        let mut r = report();
        assert_eq!(host_value(Metric::Mem, &r, 0, 100), None);
        assert_eq!(host_value(Metric::Swap, &r, 100, 0), None);
        r.disk_total = 0;
        assert_eq!(host_value(Metric::Disk, &r, 1000, 100), None);
    }

    #[test]
    fn probe_stale_is_no_data() {
        let now = 1_000_000;
        let mk = |ts| ProbeSample {
            ts_ms: ts,
            avg_ms: Some(10.0),
            loss: Some(0.0),
        };
        assert_eq!(
            probe_value(Metric::ProbeLatency, &mk(now - 180_001), 60, now),
            None
        );
        assert_eq!(
            probe_value(Metric::ProbeLatency, &mk(now - 180_000), 60, now),
            Some(10.0)
        );
    }

    #[test]
    fn probe_loss_percent_and_latency_none() {
        let s = ProbeSample {
            ts_ms: 0,
            avg_ms: None,
            loss: Some(1.0),
        };
        assert_eq!(probe_value(Metric::ProbeLoss, &s, 60, 0), Some(100.0));
        assert_eq!(probe_value(Metric::ProbeLatency, &s, 60, 0), None);
        let s2 = ProbeSample { loss: None, ..s };
        assert_eq!(probe_value(Metric::ProbeLoss, &s2, 60, 0), None);
    }

    #[test]
    fn op_holds() {
        assert!(!Op::Gt.holds(90.0, 90.0));
        assert!(Op::Gt.holds(90.1, 90.0));
        assert!(!Op::Lt.holds(90.0, 90.0));
        assert!(Op::Lt.holds(89.9, 90.0));
    }

    #[test]
    fn formats() {
        assert_eq!(format_value(Metric::Cpu, 92.34), "92.3%");
        assert_eq!(format_value(Metric::Load1, 1.25), "1.25");
        assert_eq!(format_value(Metric::NetRxRate, 1_562_500.0), "12.5 Mbps");
        assert_eq!(format_value(Metric::ProbeLatency, 183.2), "183 ms");
    }
}
