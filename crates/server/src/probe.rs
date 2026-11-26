//! 探测任务内存缓存与按节点筛选。

use crate::alert::metric::ProbeSample;
use crate::db::{DbError, ProbeTaskRow};
use crate::state::{AppState, LiveEvent};
use minimon_common::protocol::{ProbeResult, ProbeTask};
use serde::Serialize;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

/// agent 允许上报的错误短语；其余一律改为「探测失败」，避免错误文案带出目标地址。
pub const PROBE_ERRORS: &[&str] = &[
    "ICMP 不可用",
    "解析失败",
    "超时",
    "连接失败",
    "HTTP 状态异常",
    "探测失败",
];

/// 不在白名单的错误改写为「探测失败」。
pub fn sanitize_probe_error(e: Option<&str>) -> Option<String> {
    e.map(|e| {
        if PROBE_ERRORS.contains(&e) {
            e.to_owned()
        } else {
            "探测失败".to_owned()
        }
    })
}

/// 卡片上展示的某任务最新结果（访客可见字段仅此五项）。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CardProbe {
    pub task_id: i64,
    pub name: String,
    pub avg_ms: Option<f64>,
    pub loss: Option<f64>,
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
struct Latest {
    ts_ms: i64,
    avg_ms: Option<f64>,
    loss: Option<f64>,
    error: Option<String>,
}

/// 任务缓存；`lock` 串行化重载，避免乱序完成留下旧缓存。
#[derive(Default)]
pub(crate) struct ProbeCache {
    tasks: RwLock<Arc<Vec<ProbeTaskRow>>>,
    lock: tokio::sync::Mutex<()>,
    /// (node_id, task_id) -> 最新结果。
    latest: RwLock<HashMap<(i64, i64), Latest>>,
}

impl ProbeCache {
    pub(crate) fn with(tasks: Vec<ProbeTaskRow>) -> Self {
        ProbeCache {
            tasks: RwLock::new(Arc::new(tasks)),
            lock: Default::default(),
            latest: Default::default(),
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

    /// 卡片用的最新探测：`show_on_card` 且适用于该节点的任务（按 sort、id），无结果的数值为 None。
    pub fn latest_probe(&self, node_id: i64) -> Vec<CardProbe> {
        let tasks = self.probe_tasks();
        let latest = self.probe.latest.read().unwrap_or_else(|e| e.into_inner());
        tasks
            .iter()
            .filter(|t| t.show_on_card && applies(t, node_id))
            .map(|t| card(t, latest.get(&(node_id, t.id))))
            .collect()
    }

    /// 处理 agent 上报的探测结果：校验归属、净化、记最新值、推 live、入库。返回是否被接受。
    pub(crate) fn accept_probe_result(&self, node_id: i64, mut r: ProbeResult, now: i64) -> bool {
        if r.sent == 0 && r.error.is_none() {
            return false;
        }
        r.ts_ms = crate::clock::normalize_ts(r.ts_ms, now).0;
        r.error = sanitize_probe_error(r.error.as_deref());
        r.lost = r.lost.min(r.sent);
        for v in [&mut r.avg_ms, &mut r.min_ms, &mut r.max_ms] {
            if v.is_some_and(|x| !x.is_finite() || x < 0.0) {
                *v = None;
            }
        }
        let loss = (r.sent > 0).then(|| f64::from(r.lost) / f64::from(r.sent));
        let ev = {
            // 归属校验与写最新值在同一把锁内：删除路径先重载任务缓存再清理，
            // 清理之后到达的结果必然在校验处被丢弃。
            let mut latest = self.probe.latest.write().unwrap_or_else(|e| e.into_inner());
            let tasks = self.probe_tasks();
            let Some(t) = tasks.iter().find(|t| t.id == r.task_id) else {
                return false;
            };
            if !applies(t, node_id) {
                return false;
            }
            let new = Latest {
                ts_ms: r.ts_ms,
                avg_ms: r.avg_ms,
                loss,
                error: r.error.clone(),
            };
            let key = (node_id, t.id);
            if latest.get(&key).is_some_and(|old| old.ts_ms > new.ts_ms) {
                None
            } else {
                let cp = card(t, Some(&new));
                latest.insert(key, new);
                t.show_on_card.then_some(cp)
            }
        };
        if let Some(probe) = ev {
            self.live().publish(LiveEvent::Probe { node_id, probe });
        }
        self.ingest.push_probe(node_id, r);
        true
    }

    /// 每个 (node_id, task_id) 的最新探测样本（告警评估用）。
    pub(crate) fn latest_samples(&self) -> HashMap<(i64, i64), ProbeSample> {
        self.probe
            .latest
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|(k, l)| {
                let s = ProbeSample {
                    ts_ms: l.ts_ms,
                    avg_ms: l.avg_ms,
                    loss: l.loss,
                };
                (*k, s)
            })
            .collect()
    }

    /// 任务被删除：清它的最新结果。
    pub(crate) fn forget_probe_task(&self, task_id: i64) {
        self.probe
            .latest
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|(_, t), _| *t != task_id);
    }

    /// 节点被删除：清它的最新结果。
    pub(crate) fn forget_probe_node(&self, node_id: i64) {
        self.probe
            .latest
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|(n, _), _| *n != node_id);
    }

    /// 通知受影响的在线节点重发任务；`only` 为 `None` 时为全部在线节点。
    /// 只打脏标记，真正的列表由会话在发送时按当时的缓存生成（只发最新）。
    pub fn dispatch_probe_tasks(&self, only: Option<Vec<i64>>) {
        let reg = self.registry();
        for id in only.unwrap_or_else(|| reg.online_ids()) {
            reg.mark_dirty(id);
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

pub(crate) fn applies(t: &ProbeTaskRow, node_id: i64) -> bool {
    t.all_nodes || t.node_ids.contains(&node_id)
}

fn card(t: &ProbeTaskRow, l: Option<&Latest>) -> CardProbe {
    CardProbe {
        task_id: t.id,
        name: t.name.clone(),
        avg_ms: l.and_then(|l| l.avg_ms),
        loss: l.and_then(|l| l.loss),
        error: l.and_then(|l| l.error.clone()),
    }
}

/// 该节点应执行的任务（`all_nodes` 或显式包含）。
pub fn tasks_for_node(tasks: &[ProbeTaskRow], node_id: i64) -> Vec<ProbeTask> {
    tasks
        .iter()
        .filter(|t| applies(t, node_id))
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

    #[test]
    fn sanitize() {
        assert_eq!(
            sanitize_probe_error(Some("解析失败")).as_deref(),
            Some("解析失败")
        );
        assert_eq!(
            sanitize_probe_error(Some("lookup secret.example.com failed")).as_deref(),
            Some("探测失败")
        );
        assert_eq!(sanitize_probe_error(None), None);
        for e in PROBE_ERRORS {
            assert_eq!(sanitize_probe_error(Some(e)).as_deref(), Some(*e));
        }
    }

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
