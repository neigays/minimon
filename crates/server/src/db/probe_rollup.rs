//! 探测数据聚合（probe_raw -> probe_5m）、保留期清理与历史查询。
//!
//! 与 M2 的 rollup 同一套规则：水位线取目标表中该 (task, node) 的最大桶，
//! 只聚合已结束的桶，`INSERT OR REPLACE` 写入因此幂等。

use super::aggregate::bucket_start;
use super::{Db, DbError};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

pub const PROBE_BUCKET_MS: i64 = 300_000;
/// 每个事务处理的 (task, node) 组数。
const BATCH_PAIRS: usize = 10;

/// 一个 (task, node) 的一列历史点；`avg_ms` 无成功样本为 None，`loss` 无发送为 None。
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ProbeSeries {
    pub task_id: i64,
    pub ts: Vec<i64>,
    pub avg_ms: Vec<Option<f64>>,
    pub loss: Vec<Option<f64>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum ProbeGranularity {
    #[serde(rename = "raw")]
    Raw,
    #[serde(rename = "5m")]
    M5,
}

/// 表中全部 (task_id, node_id) 键（按主键前缀逐个探测，不全表扫描）。
fn pairs(c: &Connection, table: &str) -> Result<Vec<(i64, i64)>, DbError> {
    let mut stmt = c.prepare(&format!(
        "SELECT task_id, node_id FROM {table} WHERE (task_id, node_id) > (?1, ?2) \
         ORDER BY task_id, node_id LIMIT 1"
    ))?;
    let mut out = Vec::new();
    let mut cur = (i64::MIN, i64::MIN);
    while let Some(next) = stmt
        .query_row(params![cur.0, cur.1], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()?
    {
        out.push(next);
        cur = next;
    }
    Ok(out)
}

struct Bucket {
    ts: i64,
    sent: i64,
    lost: i64,
    ok_n: i64,
    avg_sum: f64,
    min: Option<f64>,
    max: Option<f64>,
    rounds: i64,
}

fn rollup_pair(c: &Connection, task: i64, node: i64, end: i64) -> Result<usize, DbError> {
    let lo: i64 = c
        .query_row(
            "SELECT MAX(ts_ms) FROM probe_5m WHERE task_id = ?1 AND node_id = ?2",
            [task, node],
            |r| r.get::<_, Option<i64>>(0),
        )?
        .map_or(i64::MIN, |t| t + PROBE_BUCKET_MS);
    let mut stmt = c.prepare_cached(
        "SELECT ts_ms, sent, lost, avg_ms, min_ms, max_ms FROM probe_raw \
         WHERE task_id = ?1 AND node_id = ?2 AND ts_ms >= ?3 AND ts_ms < ?4 ORDER BY ts_ms",
    )?;
    let mut rows = stmt.query(params![task, node, lo, end])?;
    let mut buckets: Vec<Bucket> = Vec::new();
    while let Some(r) = rows.next()? {
        let ts: i64 = r.get(0)?;
        let (sent, lost): (i64, i64) = (r.get(1)?, r.get(2)?);
        let avg: Option<f64> = r.get(3)?;
        let (min, max): (Option<f64>, Option<f64>) = (r.get(4)?, r.get(5)?);
        let start = bucket_start(ts, PROBE_BUCKET_MS);
        if buckets.last().map(|b| b.ts) != Some(start) {
            buckets.push(Bucket {
                ts: start,
                sent: 0,
                lost: 0,
                ok_n: 0,
                avg_sum: 0.0,
                min: None,
                max: None,
                rounds: 0,
            });
        }
        let Some(b) = buckets.last_mut() else {
            continue;
        };
        b.sent += sent;
        b.lost += lost;
        b.rounds += 1;
        // 按成功样本数加权；全部丢失的轮次没有 avg，不参与。
        let ok = sent - lost;
        if let (Some(a), true) = (avg, ok > 0) {
            b.ok_n += ok;
            b.avg_sum += a * ok as f64;
        }
        b.min = [b.min, min].into_iter().flatten().reduce(f64::min);
        b.max = [b.max, max].into_iter().flatten().reduce(f64::max);
    }
    drop(rows);
    let mut ins = c.prepare_cached(
        "INSERT OR REPLACE INTO probe_5m \
         (task_id, node_id, ts_ms, sent, lost, avg_ms, min_ms, max_ms, samples) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
    )?;
    for b in &buckets {
        let avg = (b.ok_n > 0).then(|| b.avg_sum / b.ok_n as f64);
        ins.execute(params![
            task, node, b.ts, b.sent, b.lost, avg, b.min, b.max, b.rounds
        ])?;
    }
    Ok(buckets.len())
}

impl Db {
    /// 聚合所有已结束（桶终点 <= now - lag）且尚未聚合的桶，返回写入的 5m 行数。
    /// 每 `batch` 个 (task, node) 提交一次事务。
    pub fn rollup_probes(&self, now_ms: i64, lag_ms: i64, batch: usize) -> Result<usize, DbError> {
        let end = bucket_start(now_ms - lag_ms, PROBE_BUCKET_MS);
        let keys = self.read(|c| pairs(c, "probe_raw"))?;
        let mut total = 0;
        for chunk in keys.chunks(batch.max(1)) {
            total += self.write(|c| {
                let tx = c.transaction()?;
                let mut n = 0;
                for &(t, node) in chunk {
                    n += rollup_pair(&tx, t, node, end)?;
                }
                tx.commit()?;
                Ok(n)
            })?;
        }
        Ok(total)
    }

    /// 按保留期删除过期探测数据（含任务/节点已不存在的残留），返回删除行数。
    pub fn prune_probes(&self, now_ms: i64, raw_ms: i64, m5_ms: i64) -> Result<usize, DbError> {
        let mut jobs: Vec<(&str, i64, i64, i64)> = Vec::new();
        for (table, keep) in [("probe_raw", raw_ms), ("probe_5m", m5_ms)] {
            for (t, n) in self.read(|c| pairs(c, table))? {
                jobs.push((table, t, n, now_ms - keep));
            }
        }
        let mut total = 0;
        for chunk in jobs.chunks(BATCH_PAIRS) {
            total += self.write(|c| {
                let tx = c.transaction()?;
                let mut n = 0;
                for &(table, t, node, cutoff) in chunk {
                    n += tx
                        .prepare_cached(&format!(
                            "DELETE FROM {table} WHERE task_id = ?1 AND node_id = ?2 AND ts_ms < ?3"
                        ))?
                        .execute(params![t, node, cutoff])?;
                }
                tx.commit()?;
                Ok(n)
            })?;
        }
        Ok(total)
    }

    /// 取 `[from, to]` 内指定任务在该节点的点（升序）；每个传入任务各返回一个序列（可为空）。
    pub fn probe_history(
        &self,
        node_id: i64,
        task_ids: &[i64],
        gran: ProbeGranularity,
        from_ms: i64,
        to_ms: i64,
    ) -> Result<Vec<ProbeSeries>, DbError> {
        let table = match gran {
            ProbeGranularity::Raw => "probe_raw",
            ProbeGranularity::M5 => "probe_5m",
        };
        self.read(|c| {
            let mut stmt = c.prepare(&format!(
                "SELECT ts_ms, sent, lost, avg_ms FROM {table} \
                 WHERE task_id = ?1 AND node_id = ?2 AND ts_ms >= ?3 AND ts_ms <= ?4 \
                 ORDER BY ts_ms"
            ))?;
            let mut out = Vec::with_capacity(task_ids.len());
            for &task_id in task_ids {
                let mut s = ProbeSeries {
                    task_id,
                    ts: vec![],
                    avg_ms: vec![],
                    loss: vec![],
                };
                let mut rows = stmt.query(params![task_id, node_id, from_ms, to_ms])?;
                while let Some(r) = rows.next()? {
                    let (sent, lost): (i64, i64) = (r.get(1)?, r.get(2)?);
                    s.ts.push(r.get(0)?);
                    s.avg_ms.push(r.get(3)?);
                    s.loss.push((sent > 0).then(|| lost as f64 / sent as f64));
                }
                out.push(s);
            }
            Ok(out)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const M: i64 = PROBE_BUCKET_MS;

    fn put(db: &Db, t: i64, n: i64, ts: i64, sent: i64, lost: i64, avg: Option<f64>) {
        db.write(|c| {
            c.execute(
                "INSERT OR REPLACE INTO probe_raw(task_id,node_id,ts_ms,sent,lost,avg_ms,min_ms,max_ms) \
                 VALUES (?1,?2,?3,?4,?5,?6,?6,?6)",
                params![t, n, ts, sent, lost, avg],
            )?;
            Ok(())
        })
        .unwrap();
    }

    type Row5 = (i64, i64, i64, Option<f64>, Option<f64>, Option<f64>, i64);

    fn rows5(db: &Db) -> Vec<Row5> {
        db.read(|c| {
            let mut s = c.prepare(
                "SELECT ts_ms,sent,lost,avg_ms,min_ms,max_ms,samples FROM probe_5m \
                 ORDER BY task_id,node_id,ts_ms",
            )?;
            Ok(s.query_map([], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                ))
            })?
            .collect::<Result<_, _>>()?)
        })
        .unwrap()
    }

    fn count(db: &Db, table: &str) -> i64 {
        db.read(|c| Ok(c.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))?))
            .unwrap()
    }

    #[test]
    fn probe_5m_weights_by_successful_samples() {
        let db = Db::open_in_memory().unwrap();
        put(&db, 1, 1, 10_000, 5, 0, Some(10.0));
        put(&db, 1, 1, 70_000, 5, 4, Some(50.0));
        assert_eq!(db.rollup_probes(M + 1, 0, 10).unwrap(), 1);
        let r = rows5(&db);
        assert_eq!(r.len(), 1);
        let (ts, sent, lost, avg, min, max, samples) = r[0];
        assert_eq!((ts, sent, lost, samples), (0, 10, 4, 2));
        assert!((avg.unwrap() - (10.0 * 5.0 + 50.0) / 6.0).abs() < 1e-9);
        assert_eq!((min, max), (Some(10.0), Some(50.0)));
    }

    #[test]
    fn all_lost_round_excluded_from_avg() {
        let db = Db::open_in_memory().unwrap();
        put(&db, 1, 1, 10_000, 5, 5, None);
        put(&db, 1, 1, 70_000, 5, 0, Some(20.0));
        put(&db, 1, 1, M + 10_000, 5, 5, None);
        db.rollup_probes(2 * M + 1, 0, 10).unwrap();
        let r = rows5(&db);
        assert_eq!(r.len(), 2);
        assert_eq!((r[0].1, r[0].2), (10, 5));
        assert_eq!(r[0].3, Some(20.0));
        // 整桶全部丢失：avg 为空，不是 0。
        assert_eq!((r[1].2, r[1].3, r[1].4, r[1].5), (5, None, None, None));
    }

    #[test]
    fn rollup_probes_idempotent_and_closed_only() {
        let db = Db::open_in_memory().unwrap();
        put(&db, 1, 1, 10_000, 1, 0, Some(5.0));
        put(&db, 1, 1, M + 10_000, 1, 0, Some(7.0));
        // 第二个桶 [M, 2M) 尚未结束（lag 之内）。
        assert_eq!(db.rollup_probes(2 * M - 1, 0, 10).unwrap(), 1);
        assert_eq!(rows5(&db).len(), 1);
        assert_eq!(db.rollup_probes(2 * M - 1, 0, 10).unwrap(), 0);
        let before = rows5(&db);
        assert_eq!(db.rollup_probes(2 * M + 1, 0, 10).unwrap(), 1);
        assert_eq!(rows5(&db).len(), 2);
        assert_eq!(rows5(&db)[0], before[0]);
        assert_eq!(db.rollup_probes(2 * M + 1, 0, 10).unwrap(), 0);
        // lag 把桶推迟。
        put(&db, 2, 1, 10_000, 1, 0, Some(1.0));
        assert_eq!(db.rollup_probes(M + 5, 10_000, 10).unwrap(), 0);
        // batch 大小不影响结果。
        assert_eq!(db.rollup_probes(M + 20_000, 10_000, 1).unwrap(), 1);
    }

    #[test]
    fn prune_probes_by_retention() {
        let db = Db::open_in_memory().unwrap();
        let now = 100 * M;
        for (t, n) in [(1, 1), (2, 1), (1, 2)] {
            put(&db, t, n, now - 5_000, 1, 0, Some(1.0));
            put(&db, t, n, now - 50_000, 1, 0, Some(1.0));
        }
        // 已不存在的任务/节点残留数据同样被清理。
        db.write(|c| {
            for ts in [now - 5_000, now - 50_000] {
                c.execute(
                    "INSERT INTO probe_5m(task_id,node_id,ts_ms,sent,lost,samples) VALUES (9,9,?1,1,0,1)",
                    [ts],
                )?;
            }
            Ok(())
        })
        .unwrap();
        assert_eq!(db.prune_probes(now, 20_000, 20_000).unwrap(), 4);
        assert_eq!(count(&db, "probe_raw"), 3);
        assert_eq!(count(&db, "probe_5m"), 1);
        assert_eq!(db.prune_probes(now, 20_000, 20_000).unwrap(), 0);
    }

    #[test]
    fn probe_history_returns_columns_with_nulls() {
        let db = Db::open_in_memory().unwrap();
        put(&db, 1, 1, 1_000, 5, 1, Some(10.0));
        put(&db, 1, 1, 2_000, 5, 5, None);
        put(&db, 1, 1, 3_000, 0, 0, None);
        put(&db, 1, 2, 1_500, 5, 0, Some(1.0));
        put(&db, 2, 1, 1_500, 5, 0, Some(2.0));
        let s = db
            .probe_history(1, &[1, 3], ProbeGranularity::Raw, 0, 10_000)
            .unwrap();
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].task_id, 1);
        assert_eq!(s[0].ts, vec![1_000, 2_000, 3_000]);
        assert_eq!(s[0].avg_ms, vec![Some(10.0), None, None]);
        assert_eq!(s[0].loss, vec![Some(0.2), Some(1.0), None]);
        assert!(s[1].ts.is_empty());
        let none = db
            .probe_history(1, &[1], ProbeGranularity::Raw, 1_500, 2_500)
            .unwrap();
        assert_eq!(none[0].ts, vec![2_000]);
    }
}
