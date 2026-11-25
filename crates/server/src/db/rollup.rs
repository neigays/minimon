//! 三级降采样：raw -> metrics_1m -> metrics_15m，以及保留期清理。
//!
//! 聚合在 Rust 里完成（纯函数），以 `INSERT OR REPLACE` 写入，因此幂等。

use rusqlite::{Connection, OptionalExtension, Row, params};

pub use super::aggregate::{
    AggRow, MINUTE_MS, QUARTER_MS, ROLLUP_LAG_MS, RawSample, RollupStats, counter_delta,
};
use super::aggregate::{aggregate_raw, merge_buckets};
use super::{Db, DbError};

use super::aggregate::bucket_start;

/// prune 每个事务处理的节点数。
const PRUNE_BATCH_NODES: usize = 10;

// ---------------------------------------------------------------- DB 胶水

const AGG_COLS: &str = "node_id, ts_ms, samples, cpu, mem_used, swap_used, disk_used, disk_total, \
     net_rx_rate, net_tx_rate, load1, load5, load15, process_count, tcp_count, udp_count, \
     cpu_max, mem_used_max, net_rx_rate_max, net_tx_rate_max, net_rx_delta, net_tx_delta, \
     net_rx_bytes, net_tx_bytes, uptime_secs";

fn agg_from_row(r: &Row<'_>) -> rusqlite::Result<AggRow> {
    let f = |i: usize| -> rusqlite::Result<f64> { Ok(r.get::<_, Option<f64>>(i)?.unwrap_or(0.0)) };
    let n = |i: usize| -> rusqlite::Result<i64> { Ok(r.get::<_, Option<i64>>(i)?.unwrap_or(0)) };
    Ok(AggRow {
        node_id: r.get(0)?,
        ts_ms: r.get(1)?,
        samples: r.get(2)?,
        cpu: f(3)?,
        mem_used: f(4)?,
        swap_used: f(5)?,
        disk_used: f(6)?,
        disk_total: f(7)?,
        net_rx_rate: f(8)?,
        net_tx_rate: f(9)?,
        load1: f(10)?,
        load5: f(11)?,
        load15: f(12)?,
        process_count: f(13)?,
        tcp_count: f(14)?,
        udp_count: f(15)?,
        cpu_max: f(16)?,
        mem_used_max: n(17)?,
        net_rx_rate_max: n(18)?,
        net_tx_rate_max: n(19)?,
        net_rx_delta: n(20)?,
        net_tx_delta: n(21)?,
        net_rx_bytes: n(22)?,
        net_tx_bytes: n(23)?,
        uptime_secs: n(24)?,
    })
}

const RAW_COLS: &str = "ts_ms, cpu, mem_used, swap_used, disk_used, disk_total, net_rx_bytes, \
     net_tx_bytes, net_rx_rate, net_tx_rate, load1, load5, load15, uptime_secs, process_count, \
     tcp_count, udp_count";

fn raw_from_row(r: &Row<'_>) -> rusqlite::Result<RawSample> {
    let f = |i: usize| -> rusqlite::Result<f64> { Ok(r.get::<_, Option<f64>>(i)?.unwrap_or(0.0)) };
    let n = |i: usize| -> rusqlite::Result<i64> { Ok(r.get::<_, Option<i64>>(i)?.unwrap_or(0)) };
    Ok(RawSample {
        ts_ms: r.get(0)?,
        cpu: f(1)?,
        mem_used: n(2)?,
        swap_used: n(3)?,
        disk_used: n(4)?,
        disk_total: n(5)?,
        net_rx_bytes: n(6)?,
        net_tx_bytes: n(7)?,
        net_rx_rate: n(8)?,
        net_tx_rate: n(9)?,
        load1: f(10)?,
        load5: f(11)?,
        load15: f(12)?,
        uptime_secs: n(13)?,
        process_count: n(14)?,
        tcp_count: n(15)?,
        udp_count: n(16)?,
    })
}

fn write_agg(conn: &Connection, table: &str, rows: &[AggRow]) -> Result<(), DbError> {
    let mut stmt = conn.prepare_cached(&format!(
        "INSERT OR REPLACE INTO {table} ({AGG_COLS}) VALUES \
         (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,?23,?24,?25)"
    ))?;
    for r in rows {
        stmt.execute(params![
            r.node_id,
            r.ts_ms,
            r.samples,
            r.cpu,
            r.mem_used,
            r.swap_used,
            r.disk_used,
            r.disk_total,
            r.net_rx_rate,
            r.net_tx_rate,
            r.load1,
            r.load5,
            r.load15,
            r.process_count,
            r.tcp_count,
            r.udp_count,
            r.cpu_max,
            r.mem_used_max,
            r.net_rx_rate_max,
            r.net_tx_rate_max,
            r.net_rx_delta,
            r.net_tx_delta,
            r.net_rx_bytes,
            r.net_tx_bytes,
            r.uptime_secs,
        ])?;
    }
    Ok(())
}

/// 目标表中该节点的最后一行（桶起点最大）。
fn last_agg(conn: &Connection, table: &str, node_id: i64) -> Result<Option<AggRow>, DbError> {
    Ok(conn
        .query_row(
            &format!(
                "SELECT {AGG_COLS} FROM {table} WHERE node_id = ?1 ORDER BY ts_ms DESC LIMIT 1"
            ),
            [node_id],
            agg_from_row,
        )
        .optional()?)
}

/// raw -> 1m：处理 [lo, end) 内的样本，返回写入行数。
fn rollup_raw_node(conn: &Connection, node_id: i64, end: i64) -> Result<usize, DbError> {
    let last = last_agg(conn, "metrics_1m", node_id)?;
    let lo = last.as_ref().map_or(i64::MIN, |r| r.ts_ms + MINUTE_MS);
    let samples: Vec<RawSample> = {
        let mut stmt = conn.prepare_cached(&format!(
            "SELECT {RAW_COLS} FROM metrics_raw WHERE node_id = ?1 AND ts_ms >= ?2 AND ts_ms < ?3 \
             ORDER BY ts_ms"
        ))?;
        stmt.query_map(params![node_id, lo, end], raw_from_row)?
            .collect::<Result<_, _>>()?
    };
    if samples.is_empty() {
        return Ok(0);
    }
    // 无 1m 行时 lo 不设下限，samples 已含全部 raw，不存在更早的样本：首个增量记为 0。
    let prev = last.map(|r| (r.net_rx_bytes, r.net_tx_bytes));
    let rows = aggregate_raw(node_id, prev, &samples, MINUTE_MS);
    write_agg(conn, "metrics_1m", &rows)?;
    Ok(rows.len())
}

/// 1m -> 15m。
fn rollup_1m_node(conn: &Connection, node_id: i64, end: i64) -> Result<usize, DbError> {
    let lo = last_agg(conn, "metrics_15m", node_id)?.map_or(i64::MIN, |r| r.ts_ms + QUARTER_MS);
    let src: Vec<AggRow> = {
        let mut stmt = conn.prepare_cached(&format!(
            "SELECT {AGG_COLS} FROM metrics_1m WHERE node_id = ?1 AND ts_ms >= ?2 AND ts_ms < ?3 \
             ORDER BY ts_ms"
        ))?;
        stmt.query_map(params![node_id, lo, end], agg_from_row)?
            .collect::<Result<_, _>>()?
    };
    let rows = merge_buckets(node_id, &src, QUARTER_MS);
    write_agg(conn, "metrics_15m", &rows)?;
    Ok(rows.len())
}

impl Db {
    /// 聚合所有已结束（桶终点 <= now - lag）且尚未聚合的桶；先 raw->1m 再 1m->15m。
    /// 每 `batch_nodes` 个节点提交一次事务，避免长时间占用写连接。
    pub fn rollup(
        &self,
        now_ms: i64,
        lag_ms: i64,
        batch_nodes: usize,
    ) -> Result<RollupStats, DbError> {
        let horizon = now_ms - lag_ms;
        let end_1m = bucket_start(horizon, MINUTE_MS);
        let end_15m = bucket_start(horizon, QUARTER_MS);
        let ids: Vec<i64> = self.read(|c| {
            let mut stmt = c.prepare("SELECT id FROM nodes ORDER BY id")?;
            Ok(stmt
                .query_map([], |r| r.get(0))?
                .collect::<Result<_, _>>()?)
        })?;
        let mut stats = RollupStats::default();
        for chunk in ids.chunks(batch_nodes.max(1)) {
            let (m1, m15) = self.write(|c| {
                let tx = c.transaction()?;
                let (mut m1, mut m15) = (0, 0);
                for &id in chunk {
                    m1 += rollup_raw_node(&tx, id, end_1m)?;
                    m15 += rollup_1m_node(&tx, id, end_15m)?;
                }
                tx.commit()?;
                Ok((m1, m15))
            })?;
            stats.m1 += m1;
            stats.m15 += m15;
        }
        Ok(stats)
    }

    /// 按保留期删除过期数据，返回删除的总行数。
    ///
    /// 表主键为 (node_id, ts_ms)，没有 ts_ms 索引；按节点删除才能走主键范围，
    /// 避免全表扫描占住写连接。节点 id 包含已不在 nodes 表中的残留数据。
    pub fn prune(
        &self,
        now_ms: i64,
        raw_ms: i64,
        m1_ms: i64,
        m15_ms: i64,
    ) -> Result<usize, DbError> {
        let ids: Vec<i64> = self.read(|c| {
            // 递归 CTE 做松散索引扫描：每个 node_id 只探一次索引。
            let mut stmt = c.prepare(
                "WITH RECURSIVE \
                 a(n) AS (SELECT MIN(node_id) FROM metrics_raw UNION ALL \
                          SELECT (SELECT MIN(node_id) FROM metrics_raw WHERE node_id > a.n) \
                          FROM a WHERE n IS NOT NULL), \
                 b(n) AS (SELECT MIN(node_id) FROM metrics_1m UNION ALL \
                          SELECT (SELECT MIN(node_id) FROM metrics_1m WHERE node_id > b.n) \
                          FROM b WHERE n IS NOT NULL), \
                 c(n) AS (SELECT MIN(node_id) FROM metrics_15m UNION ALL \
                          SELECT (SELECT MIN(node_id) FROM metrics_15m WHERE node_id > c.n) \
                          FROM c WHERE n IS NOT NULL) \
                 SELECT n FROM a WHERE n IS NOT NULL \
                 UNION SELECT n FROM b WHERE n IS NOT NULL \
                 UNION SELECT n FROM c WHERE n IS NOT NULL \
                 UNION SELECT id FROM nodes ORDER BY 1",
            )?;
            Ok(stmt
                .query_map([], |r| r.get(0))?
                .collect::<Result<_, _>>()?)
        })?;
        let mut total = 0;
        for chunk in ids.chunks(PRUNE_BATCH_NODES) {
            total += self.write(|c| {
                let tx = c.transaction()?;
                let mut n = 0;
                for &id in chunk {
                    for (table, keep) in [
                        ("metrics_raw", raw_ms),
                        ("metrics_1m", m1_ms),
                        ("metrics_15m", m15_ms),
                    ] {
                        n += tx
                            .prepare_cached(&format!(
                                "DELETE FROM {table} WHERE node_id = ?1 AND ts_ms < ?2"
                            ))?
                            .execute(params![id, now_ms - keep])?;
                    }
                }
                tx.commit()?;
                Ok(n)
            })?;
        }
        Ok(total)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: i64 = MINUTE_MS;

    fn s(ts_ms: i64, cpu: f64, rx: i64) -> RawSample {
        RawSample {
            ts_ms,
            cpu,
            net_rx_bytes: rx,
            mem_used: 100,
            ..Default::default()
        }
    }

    fn agg(ts_ms: i64, samples: i64, cpu: f64, delta: i64, cpu_max: f64) -> AggRow {
        AggRow {
            node_id: 1,
            ts_ms,
            samples,
            cpu,
            cpu_max,
            net_rx_delta: delta,
            net_rx_bytes: ts_ms,
            ..Default::default()
        }
    }

    fn db_with_raw(from_ms: i64, to_ms: i64, step_ms: i64) -> (Db, i64) {
        let db = Db::open_in_memory().expect("db");
        let (node, _) = db.add_node("n1").expect("node");
        db.write(|c| {
            let tx = c.transaction()?;
            let mut t = from_ms;
            while t < to_ms {
                tx.execute(
                    "INSERT INTO metrics_raw (node_id, ts_ms, cpu, mem_used, net_rx_bytes, \
                     net_tx_bytes) VALUES (?1, ?2, 10.0, 100, ?3, 0)",
                    params![node.id, t, t / 1000],
                )?;
                t += step_ms;
            }
            tx.commit()?;
            Ok(())
        })
        .expect("seed");
        (db, node.id)
    }

    fn count(db: &Db, table: &str) -> i64 {
        db.read(|c| Ok(c.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))?))
            .expect("count")
    }

    #[test]
    fn delta_rules() {
        assert_eq!(counter_delta(None, 500), 0);
        assert_eq!(counter_delta(Some(100), 350), 250);
        assert_eq!(counter_delta(Some(10_000), 300), 300);
    }

    #[test]
    fn raw_to_1m_avg_max_delta_last() {
        let rows = aggregate_raw(
            1,
            Some((900, 0)),
            &[
                s(60_000, 10., 1000),
                s(63_000, 20., 1500),
                s(66_000, 60., 1700),
            ],
            60_000,
        );
        assert_eq!(rows.len(), 1);
        let r = &rows[0];
        assert_eq!((r.ts_ms, r.samples), (60_000, 3));
        assert!((r.cpu - 30.0).abs() < 1e-9);
        assert_eq!(r.cpu_max, 60.0);
        assert_eq!(r.mem_used_max, 100);
        assert!((r.mem_used - 100.0).abs() < 1e-9);
        assert_eq!(r.net_rx_delta, 800);
        assert_eq!(r.net_rx_bytes, 1700);
    }

    #[test]
    fn counter_drop_inside_bucket() {
        let rows = aggregate_raw(
            1,
            None,
            &[s(60_000, 1., 1000), s(63_000, 1., 200), s(66_000, 1., 500)],
            60_000,
        );
        assert_eq!(rows[0].net_rx_delta, 200 + 300);
        assert_eq!(rows[0].net_rx_bytes, 500);
    }

    #[test]
    fn splits_buckets_on_boundary() {
        let rows = aggregate_raw(1, None, &[s(119_999, 1., 10), s(120_000, 1., 30)], 60_000);
        assert_eq!(rows.len(), 2);
        assert_eq!((rows[0].ts_ms, rows[1].ts_ms), (60_000, 120_000));
        // 桶首样本与上一桶最后样本比较
        assert_eq!(rows[1].net_rx_delta, 20);
    }

    #[test]
    fn merge_is_weighted_by_samples() {
        let rows = merge_buckets(
            1,
            &[agg(0, 10, 10.0, 5, 20.0), agg(60_000, 30, 50.0, 7, 90.0)],
            QUARTER_MS,
        );
        assert_eq!(rows.len(), 1);
        let r = &rows[0];
        assert_eq!(r.samples, 40);
        assert!((r.cpu - 40.0).abs() < 1e-9);
        assert_eq!(r.cpu_max, 90.0);
        assert_eq!(r.net_rx_delta, 12);
        assert_eq!(r.net_rx_bytes, 60_000);
    }

    #[test]
    fn rollup_only_closed_buckets_and_idempotent() {
        let (db, _) = db_with_raw(0, 5 * MIN, 5_000);
        let now = 5 * MIN + ROLLUP_LAG_MS - 1;
        let st = db.rollup(now, ROLLUP_LAG_MS, 10).expect("rollup");
        assert_eq!(st.m1, 4);
        assert_eq!(count(&db, "metrics_1m"), 4);
        let snapshot = |db: &Db| -> Vec<(i64, i64, f64, i64)> {
            db.read(|c| {
                let mut stmt = c.prepare(
                    "SELECT ts_ms, samples, cpu, net_rx_delta FROM metrics_1m ORDER BY ts_ms",
                )?;
                Ok(stmt
                    .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
                    .collect::<Result<_, _>>()?)
            })
            .expect("snap")
        };
        let before = snapshot(&db);
        assert_eq!(before[0].1, 12);
        // 首桶无 prev：delta 为桶内相邻样本差 11 * 5
        assert_eq!(before[0].3, 55);
        // 之后的桶接上一桶最后值：12 个样本 * 5
        assert_eq!(before[1].3, 60);
        let st = db.rollup(now, ROLLUP_LAG_MS, 10).expect("again");
        assert_eq!(st.m1, 0);
        assert_eq!(snapshot(&db), before);
        db.rollup(5 * MIN + ROLLUP_LAG_MS, ROLLUP_LAG_MS, 10)
            .expect("advance");
        let after = snapshot(&db);
        assert_eq!(after.len(), 5);
        assert_eq!(&after[..4], &before[..]);
        assert_eq!(after[4].3, 60);
    }

    #[test]
    fn catch_up_after_downtime() {
        for batch in [1, 50] {
            let h = 3 * 60 * MIN;
            let (db, _) = db_with_raw(0, h, 5_000);
            db.rollup(h + ROLLUP_LAG_MS, ROLLUP_LAG_MS, batch)
                .expect("rollup");
            assert_eq!(count(&db, "metrics_1m"), 180, "batch={batch}");
            assert_eq!(count(&db, "metrics_15m"), 12, "batch={batch}");
        }
    }

    /// 3 个节点、各 1 小时 raw（cpu 值互不相同）；节点 id 由 add_node 分配。
    fn three_node_db() -> (Db, Vec<i64>) {
        let db = Db::open_in_memory().expect("db");
        let mut ids = Vec::new();
        for i in 0..3 {
            let (n, _) = db.add_node(&format!("n{i}")).expect("node");
            ids.push(n.id);
        }
        db.write(|c| {
            let tx = c.transaction()?;
            for (i, id) in ids.iter().enumerate() {
                let mut t = 0;
                while t < 60 * MIN {
                    tx.execute(
                        "INSERT INTO metrics_raw (node_id, ts_ms, cpu, mem_used, net_rx_bytes, \
                         net_tx_bytes) VALUES (?1, ?2, ?3, 100, ?4, 0)",
                        params![id, t, 10.0 * (i as f64 + 1.0), t / 1000],
                    )?;
                    t += 5_000;
                }
            }
            tx.commit()?;
            Ok(())
        })
        .expect("seed");
        (db, ids)
    }

    fn dump(db: &Db, table: &str) -> Vec<(i64, i64, i64, f64, i64)> {
        db.read(|c| {
            let mut stmt = c.prepare(&format!(
                "SELECT node_id, ts_ms, samples, cpu, net_rx_delta FROM {table} \
                 ORDER BY node_id, ts_ms"
            ))?;
            Ok(stmt
                .query_map([], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
                })?
                .collect::<Result<_, _>>()?)
        })
        .expect("dump")
    }

    #[test]
    fn batch_of_one_equals_batch_of_many() {
        let now = 60 * MIN + ROLLUP_LAG_MS;
        let (a, ids) = three_node_db();
        let (b, _) = three_node_db();
        let sa = a.rollup(now, ROLLUP_LAG_MS, 1).expect("batch 1");
        let sb = b.rollup(now, ROLLUP_LAG_MS, 50).expect("batch 50");
        assert_eq!(sa, sb);
        assert_eq!(sa, RollupStats { m1: 180, m15: 12 });
        let (d1, d15) = (dump(&a, "metrics_1m"), dump(&a, "metrics_15m"));
        assert_eq!(d1, dump(&b, "metrics_1m"));
        assert_eq!(d15, dump(&b, "metrics_15m"));
        for (i, id) in ids.iter().enumerate() {
            let m1: Vec<_> = d1.iter().filter(|r| r.0 == *id).collect();
            let m15: Vec<_> = d15.iter().filter(|r| r.0 == *id).collect();
            assert_eq!((m1.len(), m15.len()), (60, 4));
            assert!((m1[0].3 - 10.0 * (i as f64 + 1.0)).abs() < 1e-9);
            assert!((m15[3].3 - 10.0 * (i as f64 + 1.0)).abs() < 1e-9);
            assert_eq!(m1[0].2, 12);
            assert_eq!(m15[0].2, 180);
            // 首桶无 prev：11 个相邻差；之后每桶 12 个
            assert_eq!((m1[0].4, m1[1].4), (55, 60));
        }
    }

    #[test]
    fn prune_covers_orphaned_nodes() {
        let db = Db::open_in_memory().expect("db");
        let now = 1_000_000_000i64;
        // 99 不在 nodes 表中
        db.write(|c| {
            for table in ["metrics_raw", "metrics_1m", "metrics_15m"] {
                for t in [now - 10_000, now - 10] {
                    let extra = if table == "metrics_raw" {
                        ""
                    } else {
                        ", samples"
                    };
                    let val = if table == "metrics_raw" { "" } else { ", 1" };
                    c.execute(
                        &format!(
                            "INSERT INTO {table} (node_id, ts_ms{extra}) VALUES (99, {t}{val})"
                        ),
                        [],
                    )?;
                }
            }
            Ok(())
        })
        .expect("seed");
        assert_eq!(db.prune(now, 1000, 1000, 1000).expect("prune"), 3);
        for t in ["metrics_raw", "metrics_1m", "metrics_15m"] {
            assert_eq!(count(&db, t), 1, "{t}");
        }
    }

    #[test]
    fn fifteen_min_waits_for_closed_bucket() {
        let (db, _) = db_with_raw(0, 20 * MIN, 5_000);
        // 15m 桶终点 = 15min；now - lag = 14min -> 未结束
        db.rollup(14 * MIN + ROLLUP_LAG_MS, ROLLUP_LAG_MS, 10)
            .expect("r1");
        assert_eq!(count(&db, "metrics_1m"), 14);
        assert_eq!(count(&db, "metrics_15m"), 0);
        db.rollup(15 * MIN + ROLLUP_LAG_MS, ROLLUP_LAG_MS, 10)
            .expect("r2");
        assert_eq!(count(&db, "metrics_15m"), 1);
        let samples: i64 = db
            .read(|c| Ok(c.query_row("SELECT samples FROM metrics_15m", [], |r| r.get(0))?))
            .expect("q");
        assert_eq!(samples, 15 * 12);
    }

    #[test]
    fn prune_by_retention() {
        let db = Db::open_in_memory().expect("db");
        let (node, _) = db.add_node("n").expect("node");
        let now = 1_000_000_000i64;
        let (raw, m1, m15) = (1000, 2000, 3000);
        db.write(|c| {
            for t in [now - raw - 1, now - raw + 1] {
                c.execute(
                    "INSERT INTO metrics_raw (node_id, ts_ms) VALUES (?1, ?2)",
                    params![node.id, t],
                )?;
            }
            for (table, keep) in [("metrics_1m", m1), ("metrics_15m", m15)] {
                for t in [now - keep - 1, now - keep + 1] {
                    c.execute(
                        &format!(
                            "INSERT INTO {table} (node_id, ts_ms, samples) VALUES (?1, ?2, 1)"
                        ),
                        params![node.id, t],
                    )?;
                }
            }
            Ok(())
        })
        .expect("seed");
        assert_eq!(db.prune(now, raw, m1, m15).expect("prune"), 3);
        for t in ["metrics_raw", "metrics_1m", "metrics_15m"] {
            assert_eq!(count(&db, t), 1, "{t}");
        }
    }
}
