//! 月流量汇总：完整 15m 桶 + 未被 15m 覆盖的 1m 桶 + 尾部 raw 样本，不重不漏。

use rusqlite::{OptionalExtension, params};

use super::aggregate::{MINUTE_MS, QUARTER_MS, counter_delta};
use super::{Db, DbError};

impl Db {
    /// 节点在 `[start_ms, now_ms)` 内的 (rx, tx) 字节增量。
    ///
    /// 1. 15m 桶（只存在于已结束的桶）：`ts >= start`；
    /// 2. 1m 桶中不属于上述 15m 桶的部分（含起点不在 15m 边界时起点所在的 15m 桶）；
    /// 3. 最后一个 1m 桶之后的 raw 样本，逐样本 `counter_delta`，首个样本的 prev 取其之前最近的计数值。
    pub fn traffic_rx_tx(
        &self,
        node_id: i64,
        start_ms: i64,
        now_ms: i64,
    ) -> Result<(i64, i64), DbError> {
        self.read(|c| {
            let (mut rx, mut tx): (i64, i64) = c.query_row(
                "SELECT coalesce(sum(net_rx_delta), 0), coalesce(sum(net_tx_delta), 0) \
                 FROM metrics_15m WHERE node_id = ?1 AND ts_ms >= ?2 AND ts_ms < ?3",
                params![node_id, start_ms, now_ms],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            let (rx1, tx1): (i64, i64) = c.query_row(
                "SELECT coalesce(sum(m.net_rx_delta), 0), coalesce(sum(m.net_tx_delta), 0) \
                 FROM metrics_1m m WHERE m.node_id = ?1 AND m.ts_ms >= ?2 AND m.ts_ms < ?3 \
                 AND NOT EXISTS (SELECT 1 FROM metrics_15m q WHERE q.node_id = m.node_id \
                   AND q.ts_ms = m.ts_ms - (m.ts_ms % ?4) AND q.ts_ms >= ?2)",
                params![node_id, start_ms, now_ms, QUARTER_MS],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            rx += rx1;
            tx += tx1;
            // raw 尾巴：rollup 已处理到最后一个 1m 桶（含）。
            let last_1m: Option<i64> = c.query_row(
                "SELECT max(ts_ms) FROM metrics_1m WHERE node_id = ?1",
                [node_id],
                |r| r.get(0),
            )?;
            let lo = last_1m.map_or(start_ms, |t| (t + MINUTE_MS).max(start_ms));
            if lo >= now_ms {
                return Ok((rx, tx));
            }
            let mut prev: Option<(i64, i64)> = c
                .query_row(
                    "SELECT net_rx_bytes, net_tx_bytes FROM metrics_raw \
                     WHERE node_id = ?1 AND ts_ms < ?2 ORDER BY ts_ms DESC LIMIT 1",
                    params![node_id, lo],
                    |r| {
                        Ok((
                            r.get::<_, Option<i64>>(0)?.unwrap_or(0),
                            r.get::<_, Option<i64>>(1)?.unwrap_or(0),
                        ))
                    },
                )
                .optional()?;
            if prev.is_none() {
                prev = c
                    .query_row(
                        "SELECT net_rx_bytes, net_tx_bytes FROM metrics_1m \
                         WHERE node_id = ?1 AND ts_ms < ?2 ORDER BY ts_ms DESC LIMIT 1",
                        params![node_id, lo],
                        |r| {
                            Ok((
                                r.get::<_, Option<i64>>(0)?.unwrap_or(0),
                                r.get::<_, Option<i64>>(1)?.unwrap_or(0),
                            ))
                        },
                    )
                    .optional()?;
            }
            let mut stmt = c.prepare_cached(
                "SELECT net_rx_bytes, net_tx_bytes FROM metrics_raw \
                 WHERE node_id = ?1 AND ts_ms >= ?2 AND ts_ms < ?3 ORDER BY ts_ms",
            )?;
            let rows = stmt.query_map(params![node_id, lo, now_ms], |r| {
                Ok((
                    r.get::<_, Option<i64>>(0)?.unwrap_or(0),
                    r.get::<_, Option<i64>>(1)?.unwrap_or(0),
                ))
            })?;
            for row in rows {
                let (r_b, t_b) = row?;
                rx += counter_delta(prev.map(|p| p.0), r_b);
                tx += counter_delta(prev.map(|p| p.1), t_b);
                prev = Some((r_b, t_b));
            }
            Ok((rx, tx))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::aggregate::ROLLUP_LAG_MS;
    use rusqlite::params;

    const MIN: i64 = 60_000;

    /// 2 小时、每 5 秒一个样本的 raw；每 5 秒 +1000，第 50 分钟计数器回落。
    fn seeded() -> (Db, i64, Vec<(i64, i64, i64)>) {
        let db = Db::open_in_memory().expect("db");
        let (n, _) = db.add_node("t").expect("node");
        let mut samples = Vec::new();
        let (mut rx, mut tx) = (0i64, 0i64);
        let mut t = 0;
        while t < 120 * MIN {
            if t == 50 * MIN {
                rx = 300;
                tx = 100;
            } else {
                rx += 1000;
                tx += 400 + (t / 5000) % 7;
            }
            samples.push((t, rx, tx));
            t += 5_000;
        }
        db.write(|c| {
            let txn = c.transaction()?;
            for (t, rx, tx) in &samples {
                txn.execute(
                    "INSERT INTO metrics_raw (node_id, ts_ms, net_rx_bytes, net_tx_bytes) \
                     VALUES (?1, ?2, ?3, ?4)",
                    params![n.id, t, rx, tx],
                )?;
            }
            txn.commit()?;
            Ok(())
        })
        .expect("seed");
        (db, n.id, samples)
    }

    fn expected(samples: &[(i64, i64, i64)], start: i64, now: i64) -> (i64, i64) {
        let mut prev: Option<(i64, i64)> = None;
        let (mut rx, mut tx) = (0, 0);
        for &(t, r, x) in samples {
            if t >= start && t < now {
                rx += counter_delta(prev.map(|p| p.0), r);
                tx += counter_delta(prev.map(|p| p.1), x);
            }
            prev = Some((r, x));
        }
        (rx, tx)
    }

    /// rollup 到 `now`（留下最近约 3 分钟 raw 未聚合）。
    fn rolled(now: i64) -> (Db, i64, Vec<(i64, i64, i64)>) {
        let (db, id, s) = seeded();
        // 只保留 now 之前的 raw：模拟「现在」就是 now
        db.write(|c| {
            c.execute("DELETE FROM metrics_raw WHERE ts_ms >= ?1", [now])?;
            Ok(())
        })
        .expect("trim");
        db.rollup(now - 2 * MIN, ROLLUP_LAG_MS, 10).expect("rollup");
        let s = s.into_iter().filter(|x| x.0 < now).collect();
        (db, id, s)
    }

    #[test]
    fn sum_spans_three_granularities_without_gaps_or_overlap() {
        let now = 120 * MIN;
        let (db, id, s) = rolled(now);
        let n15: i64 = db
            .read(|c| Ok(c.query_row("SELECT count(*) FROM metrics_15m", [], |r| r.get(0))?))
            .expect("count");
        assert!(n15 > 0);
        assert_eq!(
            db.traffic_rx_tx(id, 0, now).expect("sum"),
            expected(&s, 0, now)
        );
        // 计数器回落确实发生在范围内
        assert!(expected(&s, 0, now).0 > 0);
    }

    #[test]
    fn period_start_not_on_15m_boundary() {
        let now = 120 * MIN;
        let (db, id, s) = rolled(now);
        for start in [7 * MIN, 22 * MIN, 52 * MIN, 118 * MIN] {
            assert_eq!(
                db.traffic_rx_tx(id, start, now).expect("sum"),
                expected(&s, start, now),
                "start={start}"
            );
        }
    }

    #[test]
    fn raw_only_and_empty() {
        let (db, id, s) = seeded();
        assert_eq!(
            db.traffic_rx_tx(id, 10 * MIN, 30 * MIN).expect("sum"),
            expected(&s, 10 * MIN, 30 * MIN)
        );
        assert_eq!(
            db.traffic_rx_tx(id, 500 * MIN, 600 * MIN).expect("sum"),
            (0, 0)
        );
    }
}
