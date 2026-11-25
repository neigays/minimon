//! 月流量：按账期汇总、内存缓存、每分钟（rollup 之后）与配置变更后重算。

use crate::db::{Db, Node, TrafficMode};
use crate::period::{Tz, period_bounds};
use crate::state::{AppState, LiveEvent};
use serde::Serialize;
use std::collections::HashSet;

/// 按计费口径折算已用流量。
pub fn apply_mode(mode: TrafficMode, rx: i64, tx: i64) -> i64 {
    match mode {
        TrafficMode::Rx => rx,
        TrafficMode::Tx => tx,
        TrafficMode::Sum => rx.saturating_add(tx),
        TrafficMode::Max => rx.max(tx),
    }
}

/// 本期流量用量（仅管理员可见）。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TrafficUsage {
    pub quota: Option<i64>,
    pub reset_day: u8,
    pub mode: TrafficMode,
    pub used: i64,
    pub rx: i64,
    pub tx: i64,
    pub period_start_ms: i64,
    pub period_end_ms: i64,
}

/// 计算单个节点的本期用量；账期无效或查库失败返回 None（并记日志，不当作 0）。
fn compute(db: &Db, n: &Node, tz: &Tz, now_ms: i64) -> Option<TrafficUsage> {
    let (start, end) = period_bounds(n.traffic_reset_day, tz, now_ms);
    if start >= end {
        tracing::warn!(node_id = n.id, "账期计算失败（起止相同），跳过流量统计");
        return None;
    }
    match db.traffic_rx_tx(n.id, start, now_ms) {
        Ok((rx, tx)) => Some(TrafficUsage {
            quota: n.traffic_quota,
            reset_day: n.traffic_reset_day,
            mode: n.traffic_mode,
            used: apply_mode(n.traffic_mode, rx, tx),
            rx,
            tx,
            period_start_ms: start,
            period_end_ms: end,
        }),
        Err(e) => {
            tracing::warn!(node_id = n.id, error = %e, "流量汇总失败");
            None
        }
    }
}

impl AppState {
    /// 缓存中的本期用量。
    pub fn traffic(&self, node_id: i64) -> Option<TrafficUsage> {
        self.traffic_cache()
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&node_id)
            .cloned()
    }

    /// 重算流量并写回缓存，随后向管理员发布 `LiveEvent::Traffic`。
    /// `only = None` 为全部节点（同时清掉已删除节点的缓存）。
    pub async fn recompute_traffic(&self, only: Option<Vec<i64>>) {
        // 串行化：节点快照在持锁后读取，后来者一定基于不早于先来者的配置。
        let _serial = self.traffic_lock().lock().await;
        let nodes = self.nodes();
        let wanted: Option<HashSet<i64>> = only.map(|v| v.into_iter().collect());
        let targets: Vec<Node> = nodes
            .iter()
            .filter(|n| wanted.as_ref().is_none_or(|w| w.contains(&n.id)))
            .cloned()
            .collect();
        let db = self.db.clone();
        let tz = self.timezone();
        let now = crate::db::now_ms();
        let computed = tokio::task::spawn_blocking(move || {
            targets
                .into_iter()
                .map(|n| (n.id, compute(&db, &n, &tz, now)))
                .collect::<Vec<_>>()
        })
        .await;
        let computed = match computed {
            Ok(c) => c,
            Err(e) => {
                tracing::error!(error = %e, "流量重算任务异常");
                return;
            }
        };
        {
            let mut cache = self
                .traffic_cache()
                .write()
                .unwrap_or_else(|e| e.into_inner());
            if wanted.is_none() {
                let live: HashSet<i64> = nodes.iter().map(|n| n.id).collect();
                cache.retain(|id, _| live.contains(id));
            }
            for (id, usage) in &computed {
                match usage {
                    Some(u) => {
                        cache.insert(*id, u.clone());
                    }
                    None => {
                        cache.remove(id);
                    }
                }
            }
        }
        for (node_id, usage) in computed {
            if let Some(usage) = usage {
                self.live().publish(LiveEvent::Traffic { node_id, usage });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modes() {
        assert_eq!(apply_mode(TrafficMode::Max, 3, 5), 5);
        assert_eq!(apply_mode(TrafficMode::Sum, 3, 5), 8);
        assert_eq!(apply_mode(TrafficMode::Rx, 3, 5), 3);
        assert_eq!(apply_mode(TrafficMode::Tx, 3, 5), 5);
    }
}
