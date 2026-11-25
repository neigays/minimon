use minimon_common::protocol::Report;
use rusqlite::params;

use super::{Db, DbError};

impl Db {
    /// 单事务批量写入原始指标；同一 (node_id, ts_ms) 以后写入者为准。返回实际写入行数（已删除节点的行被跳过）。
    pub fn insert_reports(&self, rows: &[(i64, Report)]) -> Result<usize, DbError> {
        if rows.is_empty() {
            return Ok(0);
        }
        self.write(|c| {
            let tx = c.transaction()?;
            let mut written = 0;
            {
                let mut stmt = tx.prepare_cached(
                    "INSERT OR REPLACE INTO metrics_raw (node_id, ts_ms, cpu, mem_used, swap_used, \
                     disk_used, disk_total, net_rx_bytes, net_tx_bytes, net_rx_rate, net_tx_rate, \
                     load1, load5, load15, uptime_secs, process_count, tcp_count, udp_count) \
                     SELECT ?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18 \
                     WHERE EXISTS (SELECT 1 FROM nodes WHERE id = ?1)",
                )?;
                for (node_id, r) in rows {
                    // u64 -> i64：SQLite 只有 64 位有符号整数，实际取值远小于上限。
                    // 节点已被删除时 EXISTS 为假，该行静默跳过（不会写成孤儿数据）。
                    written += stmt.execute(params![
                        node_id,
                        r.ts_ms,
                        r.cpu,
                        r.mem_used as i64,
                        r.swap_used as i64,
                        r.disk_used as i64,
                        r.disk_total as i64,
                        r.net_rx_bytes as i64,
                        r.net_tx_bytes as i64,
                        r.net_rx_rate as i64,
                        r.net_tx_rate as i64,
                        r.load1,
                        r.load5,
                        r.load15,
                        r.uptime_secs as i64,
                        r.process_count,
                        r.tcp_count,
                        r.udp_count,
                    ])?;
                }
            }
            // 同事务内推进 last_seen_ms（每节点取批内最大 ts）。
            // 钳到服务器当前时间：agent 时钟超前不得让 last_seen_ms 落在未来。
            let now = super::now_ms();
            let mut newest: std::collections::HashMap<i64, i64> = Default::default();
            for (node_id, r) in rows {
                let ts = r.ts_ms.min(now);
                let e = newest.entry(*node_id).or_insert(ts);
                *e = (*e).max(ts);
            }
            for (node_id, ts) in newest {
                tx.prepare_cached(
                    "UPDATE nodes SET last_seen_ms = max(coalesce(last_seen_ms, 0), ?1) WHERE id = ?2",
                )?
                .execute(params![ts, node_id])?;
            }
            tx.commit()?;
            Ok(written)
        })
    }
}
