//! 历史曲线查询：按 range 选择 raw / 1m / 15m 表，返回列式数组。

use rusqlite::params;
use serde::Serialize;
use std::str::FromStr;

use super::{Db, DbError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Range {
    H1,
    H24,
    D7,
    D30,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Granularity {
    #[serde(rename = "raw")]
    Raw,
    #[serde(rename = "1m")]
    M1,
    #[serde(rename = "15m")]
    M15,
}

impl FromStr for Range {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, ()> {
        match s {
            "1h" => Ok(Range::H1),
            "24h" => Ok(Range::H24),
            "7d" => Ok(Range::D7),
            "30d" => Ok(Range::D30),
            _ => Err(()),
        }
    }
}

impl Range {
    pub fn span_ms(&self) -> i64 {
        const HOUR: i64 = 3_600_000;
        match self {
            Range::H1 => HOUR,
            Range::H24 => 24 * HOUR,
            Range::D7 => 7 * 24 * HOUR,
            Range::D30 => 30 * 24 * HOUR,
        }
    }

    pub fn granularity(&self) -> Granularity {
        match self {
            Range::H1 => Granularity::Raw,
            Range::H24 => Granularity::M1,
            Range::D7 | Range::D30 => Granularity::M15,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct History {
    pub granularity: Granularity,
    pub ts: Vec<i64>,
    pub cpu: Vec<f64>,
    pub mem_used: Vec<i64>,
    pub swap_used: Vec<i64>,
    pub disk_used: Vec<i64>,
    pub disk_total: Vec<i64>,
    pub net_rx_rate: Vec<i64>,
    pub net_tx_rate: Vec<i64>,
    pub load1: Vec<f64>,
    pub load5: Vec<f64>,
    pub load15: Vec<f64>,
    pub tcp_count: Vec<i64>,
    pub udp_count: Vec<i64>,
}

impl Db {
    /// 取 `[now - span, now]` 内的点，按 ts 升序；不补点。
    /// 聚合粒度下整数列的平均值四舍五入为整数。
    pub fn history(&self, node_id: i64, range: Range, now_ms: i64) -> Result<History, DbError> {
        let gran = range.granularity();
        let table = match gran {
            Granularity::Raw => "metrics_raw",
            Granularity::M1 => "metrics_1m",
            Granularity::M15 => "metrics_15m",
        };
        let int = |c: &str| format!("CAST(ROUND(COALESCE({c}, 0)) AS INTEGER)");
        let sql = format!(
            "SELECT ts_ms, COALESCE(cpu, 0), {}, {}, {}, {}, {}, {}, \
             COALESCE(load1, 0), COALESCE(load5, 0), COALESCE(load15, 0), {}, {} \
             FROM {table} WHERE node_id = ?1 AND ts_ms >= ?2 AND ts_ms <= ?3 ORDER BY ts_ms",
            int("mem_used"),
            int("swap_used"),
            int("disk_used"),
            int("disk_total"),
            int("net_rx_rate"),
            int("net_tx_rate"),
            int("tcp_count"),
            int("udp_count"),
        );
        self.read(|c| {
            let mut stmt = c.prepare(&sql)?;
            let mut rows = stmt.query(params![node_id, now_ms - range.span_ms(), now_ms])?;
            let mut h = History {
                granularity: gran,
                ts: vec![],
                cpu: vec![],
                mem_used: vec![],
                swap_used: vec![],
                disk_used: vec![],
                disk_total: vec![],
                net_rx_rate: vec![],
                net_tx_rate: vec![],
                load1: vec![],
                load5: vec![],
                load15: vec![],
                tcp_count: vec![],
                udp_count: vec![],
            };
            while let Some(r) = rows.next()? {
                h.ts.push(r.get(0)?);
                h.cpu.push(r.get(1)?);
                h.mem_used.push(r.get(2)?);
                h.swap_used.push(r.get(3)?);
                h.disk_used.push(r.get(4)?);
                h.disk_total.push(r.get(5)?);
                h.net_rx_rate.push(r.get(6)?);
                h.net_tx_rate.push(r.get(7)?);
                h.load1.push(r.get(8)?);
                h.load5.push(r.get(9)?);
                h.load15.push(r.get(10)?);
                h.tcp_count.push(r.get(11)?);
                h.udp_count.push(r.get(12)?);
            }
            Ok(h)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn range_parse_and_granularity() {
        assert_eq!(
            "1h".parse::<Range>().unwrap().granularity(),
            Granularity::Raw
        );
        assert_eq!(
            "24h".parse::<Range>().unwrap().granularity(),
            Granularity::M1
        );
        assert_eq!(
            "7d".parse::<Range>().unwrap().granularity(),
            Granularity::M15
        );
        assert_eq!(
            "30d".parse::<Range>().unwrap().granularity(),
            Granularity::M15
        );
        assert!("2h".parse::<Range>().is_err());
        assert!("".parse::<Range>().is_err());
    }

    const NOW: i64 = 100 * 24 * 3_600_000;

    fn seed(db: &Db) {
        db.write(|c| {
            // 窗口：1h / 24h / 30d 均以各自表的 span 判定，窗口外行比最大 span(30d) 更早。
            let old = NOW - 31 * 24 * 3_600_000;
            for ts in [NOW - 10, NOW - 50, old] {
                c.execute(
                    "INSERT INTO metrics_raw (node_id, ts_ms, cpu, mem_used, swap_used, disk_used, \
                     disk_total, net_rx_rate, net_tx_rate, load1, load5, load15, tcp_count, udp_count) \
                     VALUES (1, ?1, 1.5, 10, 0, 5, 9, 3, 4, 0.1, 0.2, 0.3, 7, 8)",
                    [ts],
                )?;
            }
            for t in ["metrics_1m", "metrics_15m"] {
                for ts in [NOW - 10, NOW - 50, old] {
                    c.execute(
                        &format!(
                            "INSERT INTO {t} (node_id, ts_ms, samples, cpu, mem_used, swap_used, \
                             disk_used, disk_total, net_rx_rate, net_tx_rate, load1, load5, load15, \
                             tcp_count, udp_count) \
                             VALUES (1, ?1, 4, 2.5, 10.5, 0.4, 5, 9, 2.5, 3.4, 1, 2, 3, 6.6, 7)"
                        ),
                        [ts],
                    )?;
                }
            }
            // 另一个节点的数据不得混入
            c.execute(
                "INSERT INTO metrics_raw (node_id, ts_ms, cpu) VALUES (2, ?1, 9)",
                [NOW - 5],
            )?;
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn history_reads_right_table_and_window() {
        let db = Db::open_in_memory().unwrap();
        seed(&db);
        for (range, gran) in [
            (Range::H1, Granularity::Raw),
            (Range::H24, Granularity::M1),
            (Range::D30, Granularity::M15),
        ] {
            let h = db.history(1, range, NOW).unwrap();
            assert_eq!(h.granularity, gran);
            assert_eq!(h.ts, vec![NOW - 50, NOW - 10]);
            let n = h.ts.len();
            for len in [
                h.cpu.len(),
                h.mem_used.len(),
                h.swap_used.len(),
                h.disk_used.len(),
                h.disk_total.len(),
                h.net_rx_rate.len(),
                h.net_tx_rate.len(),
                h.load1.len(),
                h.load5.len(),
                h.load15.len(),
                h.tcp_count.len(),
                h.udp_count.len(),
            ] {
                assert_eq!(len, n);
            }
        }
        // 聚合表平均值四舍五入为整数列
        let h = db.history(1, Range::H24, NOW).unwrap();
        assert_eq!(h.cpu[0], 2.5);
        assert_eq!(h.mem_used[0], 11);
        assert_eq!(h.swap_used[0], 0);
        assert_eq!(h.net_rx_rate[0], 3);
        assert_eq!(h.net_tx_rate[0], 3);
        assert_eq!(h.tcp_count[0], 7);
        // raw 原样
        let h = db.history(1, Range::H1, NOW).unwrap();
        assert_eq!(h.mem_used[0], 10);
        assert!((h.load5[0] - 0.2).abs() < 1e-9);
        // 窗口外 / 他人节点为空
        assert!(db.history(3, Range::H1, NOW).unwrap().ts.is_empty());
        assert_eq!(
            db.history(1, Range::H1, NOW + 2 * 3_600_000)
                .unwrap()
                .ts
                .len(),
            0
        );
    }
}
