//! 重启后恢复“最后已知数据”：每个节点依次取 raw、1m、15m 的最后一行还原成 `Report`。

use minimon_common::protocol::Report;
use rusqlite::{Connection, OptionalExtension};
use std::collections::HashMap;

use super::aggregate::{AggRow, RawSample};
use super::rollup::{AGG_COLS, RAW_COLS, agg_from_row, raw_from_row};
use super::{Db, DbError};

fn nz(v: i64) -> u64 {
    v.max(0) as u64
}

fn from_raw(s: RawSample) -> Report {
    Report {
        ts_ms: s.ts_ms,
        cpu: s.cpu as f32,
        mem_used: nz(s.mem_used),
        swap_used: nz(s.swap_used),
        disk_used: nz(s.disk_used),
        disk_total: nz(s.disk_total),
        net_rx_bytes: nz(s.net_rx_bytes),
        net_tx_bytes: nz(s.net_tx_bytes),
        net_rx_rate: nz(s.net_rx_rate),
        net_tx_rate: nz(s.net_tx_rate),
        load1: s.load1,
        load5: s.load5,
        load15: s.load15,
        uptime_secs: nz(s.uptime_secs),
        process_count: nz(s.process_count) as u32,
        tcp_count: nz(s.tcp_count) as u32,
        udp_count: nz(s.udp_count) as u32,
    }
}

/// 聚合行：平均值取整，计数器与运行时间取桶内最后值。
fn from_agg(a: AggRow) -> Report {
    let r = |v: f64| v.max(0.0).round() as u64;
    Report {
        ts_ms: a.ts_ms,
        cpu: a.cpu as f32,
        mem_used: r(a.mem_used),
        swap_used: r(a.swap_used),
        disk_used: r(a.disk_used),
        disk_total: r(a.disk_total),
        net_rx_bytes: nz(a.net_rx_bytes),
        net_tx_bytes: nz(a.net_tx_bytes),
        net_rx_rate: r(a.net_rx_rate),
        net_tx_rate: r(a.net_tx_rate),
        load1: a.load1,
        load5: a.load5,
        load15: a.load15,
        uptime_secs: nz(a.uptime_secs),
        process_count: r(a.process_count) as u32,
        tcp_count: r(a.tcp_count) as u32,
        udp_count: r(a.udp_count) as u32,
    }
}

fn last_for(c: &Connection, id: i64) -> Result<Option<Report>, DbError> {
    if let Some(s) = c
        .query_row(
            &format!(
                "SELECT {RAW_COLS} FROM metrics_raw WHERE node_id = ?1 ORDER BY ts_ms DESC LIMIT 1"
            ),
            [id],
            raw_from_row,
        )
        .optional()?
    {
        return Ok(Some(from_raw(s)));
    }
    for table in ["metrics_1m", "metrics_15m"] {
        if let Some(a) = c
            .query_row(
                &format!(
                    "SELECT {AGG_COLS} FROM {table} WHERE node_id = ?1 ORDER BY ts_ms DESC LIMIT 1"
                ),
                [id],
                agg_from_row,
            )
            .optional()?
        {
            return Ok(Some(from_agg(a)));
        }
    }
    Ok(None)
}

impl Db {
    /// 每个现存节点的最后一条已知上报；没有任何数据的节点不在结果里。
    pub fn last_reports(&self) -> Result<HashMap<i64, Report>, DbError> {
        self.read(|c| {
            let ids: Vec<i64> = c
                .prepare("SELECT id FROM nodes")?
                .query_map([], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            let mut out = HashMap::new();
            for id in ids {
                if let Some(r) = last_for(c, id)? {
                    out.insert(id, r);
                }
            }
            Ok(out)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rep(ts: i64, cpu: f32) -> Report {
        Report {
            ts_ms: ts,
            cpu,
            mem_used: 5,
            swap_used: 0,
            disk_used: 1,
            disk_total: 2,
            net_rx_bytes: 10,
            net_tx_bytes: 20,
            net_rx_rate: 1,
            net_tx_rate: 2,
            load1: 0.5,
            load5: 0.5,
            load15: 0.5,
            uptime_secs: 9,
            process_count: 3,
            tcp_count: 4,
            udp_count: 5,
        }
    }

    #[test]
    fn raw_wins_and_roundtrips() {
        let db = Db::open_in_memory().unwrap();
        let (n, _) = db.add_node("a").unwrap();
        db.insert_reports(&[(n.id, rep(1, 1.0)), (n.id, rep(2, 42.0))])
            .unwrap();
        assert_eq!(db.last_reports().unwrap().get(&n.id), Some(&rep(2, 42.0)));
    }

    #[test]
    fn falls_back_to_15m_and_none_when_empty() {
        let db = Db::open_in_memory().unwrap();
        let (a, _) = db.add_node("a").unwrap();
        let (b, _) = db.add_node("b").unwrap();
        db.write(|c| {
            c.execute(
                "INSERT INTO metrics_15m (node_id, ts_ms, samples, cpu, mem_used, net_rx_bytes) \
                 VALUES (?1, 900000, 3, 33.0, 100.4, 777)",
                [a.id],
            )?;
            Ok(())
        })
        .unwrap();
        let m = db.last_reports().unwrap();
        let r = m.get(&a.id).unwrap();
        assert_eq!(
            (r.ts_ms, r.cpu, r.mem_used, r.net_rx_bytes),
            (900000, 33.0, 100, 777)
        );
        assert!(!m.contains_key(&b.id));
    }
}
