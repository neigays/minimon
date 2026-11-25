//! 探测任务内存缓存与按节点筛选。

use crate::db::{DbError, ProbeTaskRow};
use crate::state::AppState;
use minimon_common::protocol::{ProbeTask, ServerMsg};
use std::sync::{Arc, RwLock};

/// 任务缓存；`lock` 串行化重载，避免乱序完成留下旧缓存。
#[derive(Default)]
pub(crate) struct ProbeCache {
    tasks: RwLock<Arc<Vec<ProbeTaskRow>>>,
    lock: tokio::sync::Mutex<()>,
}

impl ProbeCache {
    pub(crate) fn with(tasks: Vec<ProbeTaskRow>) -> Self {
        ProbeCache {
            tasks: RwLock::new(Arc::new(tasks)),
            lock: Default::default(),
        }
    }
}

impl AppState {
    /// 当前任务缓存快照。
    pub fn probe_tasks(&self) -> Arc<Vec<ProbeTaskRow>> {
        self.probe
            .tasks
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// 向在线节点下发当前全量任务；`only` 为受影响节点，`None` 为全部在线节点。
    /// 任务列表在会话表锁内按当前缓存生成，保证同一节点的消息顺序与缓存新旧一致。
    pub fn dispatch_probe_tasks(&self, only: Option<Vec<i64>>) {
        let reg = self.registry();
        let ids = only.unwrap_or_else(|| reg.online_ids());
        for id in ids {
            reg.send_with(id, || ServerMsg::ProbeTasks {
                tasks: tasks_for_node(&self.probe_tasks(), id),
            });
        }
    }

    /// 从库重载任务缓存（读库 + 写缓存在同一把锁内）。
    pub(crate) async fn reload_probe_tasks(&self) -> Result<(), DbError> {
        let _serial = self.probe.lock.lock().await;
        let db = self.db.clone();
        let list = tokio::task::spawn_blocking(move || db.list_probe_tasks())
            .await
            .map_err(|_| DbError::Poisoned)??;
        *self.probe.tasks.write().unwrap_or_else(|e| e.into_inner()) = Arc::new(list);
        Ok(())
    }
}

/// 该节点应执行的任务（`all_nodes` 或显式包含）。
pub fn tasks_for_node(tasks: &[ProbeTaskRow], node_id: i64) -> Vec<ProbeTask> {
    tasks
        .iter()
        .filter(|t| t.all_nodes || t.node_ids.contains(&node_id))
        .map(|t| ProbeTask {
            id: t.id,
            kind: t.kind,
            target: t.target.clone(),
            interval_secs: t.interval_secs,
            samples: t.samples,
            timeout_ms: t.timeout_ms,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use minimon_common::protocol::ProbeKind;

    fn row(id: i64, all: bool, nodes: &[i64]) -> ProbeTaskRow {
        ProbeTaskRow {
            id,
            name: format!("t{id}"),
            kind: ProbeKind::Tcp,
            target: "h:80".into(),
            interval_secs: 30,
            samples: 2,
            timeout_ms: 1000,
            all_nodes: all,
            node_ids: nodes.to_vec(),
            show_on_card: true,
            sort: id,
        }
    }

    #[test]
    fn tasks_for_node_filters() {
        let rows = [
            row(1, true, &[]),
            row(2, false, &[7, 8]),
            row(3, false, &[9]),
        ];
        let got = tasks_for_node(&rows, 7);
        assert_eq!(got.iter().map(|t| t.id).collect::<Vec<_>>(), [1, 2]);
        assert_eq!(
            got[1],
            ProbeTask {
                id: 2,
                kind: ProbeKind::Tcp,
                target: "h:80".into(),
                interval_secs: 30,
                samples: 2,
                timeout_ms: 1000
            }
        );
        assert_eq!(tasks_for_node(&rows, 9).len(), 2);
        assert_eq!(tasks_for_node(&rows, 1).len(), 1);
        assert!(tasks_for_node(&[], 1).is_empty());
    }
}
