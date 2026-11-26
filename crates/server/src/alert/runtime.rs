//! 告警运行时：共享引擎、周期评估任务、外部变更（删节点 / 改规则）接线。
//!
//! 一轮评估 = 构建内存快照 -> 锁内 `evaluate` -> 锁外写库 -> 入通知队列。
//! `serial` 串行化「一整轮评估（含写库）」与外部变更，避免旧快照的结果
//! 在节点删除或规则重载之后写回内存或库。引擎的 `std::sync::Mutex` 从不跨 await。

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use minimon_common::protocol::Report;
use tokio::sync::watch;
use tokio::task::JoinHandle;

use super::engine::{Effects, Engine, NodeSnap, Snapshot};
use super::model::{NewEvent, SendStatus, StateRow};
use crate::db::{Db, DbError, now_ms};
use crate::notify::NotifyQueue;
use crate::state::AppState;

/// 合并发送窗口与失败重试间隔（规格固定值）。
pub(crate) const NOTIFY_WINDOW: Duration = Duration::from_secs(10);
pub(crate) const NOTIFY_RETRY: [Duration; 2] = [Duration::from_secs(5), Duration::from_secs(30)];

pub struct AlertShared {
    engine: Mutex<Engine>,
    serial: tokio::sync::Mutex<()>,
    started_ms: i64,
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, DbError> + Send + 'static,
) -> Result<T, DbError> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|_| DbError::Poisoned)?
}

impl AlertShared {
    /// 从库恢复规则、设置、状态与提醒标记；`started_ms` 取当前时间。
    pub(crate) fn load(db: &Db) -> Result<Self, DbError> {
        let engine = Engine::new(
            db.list_alert_rules()?,
            db.alert_settings()?,
            db.load_alert_states()?,
            db.load_alert_marks()?,
        );
        Ok(AlertShared {
            engine: Mutex::new(engine),
            serial: tokio::sync::Mutex::new(()),
            started_ms: now_ms(),
        })
    }

    fn engine(&self) -> MutexGuard<'_, Engine> {
        self.engine.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 暂停评估：持有期间不会有新一轮开始，进行中的一轮（含写库）已结束。
    /// 删除节点时从删库持有到 `forget_node` 之后。
    pub(crate) async fn pause(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.serial.lock().await
    }

    /// 重新读取规则；语义变化的规则状态从内存与库中清除。
    pub(crate) async fn reload_rules(&self, db: &Arc<Db>) -> Result<(), DbError> {
        let _serial = self.serial.lock().await;
        let d = db.clone();
        let rules = blocking(move || d.list_alert_rules()).await?;
        let doomed = self.engine().set_rules(rules);
        delete_states(db, doomed).await
    }

    /// 重新读取告警设置；关闭离线告警时清除离线状态。
    // 告警设置接口（Task 6）接入后去掉。
    #[allow(dead_code)]
    pub(crate) async fn reload_settings(&self, db: &Arc<Db>) -> Result<(), DbError> {
        let _serial = self.serial.lock().await;
        let d = db.clone();
        let settings = blocking(move || d.alert_settings()).await?;
        let doomed = self.engine().set_settings(settings);
        delete_states(db, doomed).await
    }

    /// 节点删除：丢弃其内存状态（库中由删除节点一并清理）。调用方应持有 [`Self::pause`]。
    pub(crate) fn forget_node(&self, id: i64) {
        self.engine().forget_node(id);
    }

    /// 当前 firing 的状态。
    pub fn active(&self) -> Vec<StateRow> {
        self.engine().active()
    }
}

async fn delete_states(db: &Arc<Db>, keys: Vec<super::model::AlertKey>) -> Result<(), DbError> {
    if keys.is_empty() {
        return Ok(());
    }
    let d = db.clone();
    blocking(move || d.apply_alert_effects(&[], &keys, &[])).await
}

/// 由内存缓存构建评估快照；节点缓存中的每个节点都在内（含离线与从未上线）。
fn snapshot(state: &AppState, started_ms: i64) -> Snapshot {
    let now = now_ms();
    let online: HashMap<i64, Option<Report>> = state.registry().snapshot().into_iter().collect();
    let last = state.last_known();
    let nodes = state
        .nodes()
        .iter()
        .map(|n| {
            let session = online.get(&n.id);
            let info = match session {
                Some(_) => state.registry().info(n.id).or_else(|| n.info.clone()),
                None => n.info.clone(),
            };
            NodeSnap {
                id: n.id,
                name: n.name.clone(),
                alert_offline: n.alert_offline,
                last_seen_ms: crate::views::merge_last_seen(
                    n.last_seen_ms,
                    last.get(&n.id).map(|r| r.ts_ms),
                    now,
                ),
                online: session.is_some(),
                report: session.cloned().flatten(),
                mem_total: info.as_ref().map_or(0, |h| h.mem_total),
                swap_total: info.as_ref().map_or(0, |h| h.swap_total),
                traffic: state.traffic(n.id),
                expire_date: n.expire_date.clone(),
            }
        })
        .collect();
    Snapshot {
        now_ms: now,
        server_start_ms: started_ms,
        tz: state.timezone(),
        nodes,
        tasks: state.probe_tasks().as_ref().clone(),
        probes: state.latest_samples(),
    }
}

/// 一轮评估。错误只记日志。
async fn run_tick(state: &AppState, queue: &NotifyQueue) {
    let shared = state.alert();
    let _serial = shared.serial.lock().await;
    let snap = snapshot(state, shared.started_ms);
    let fx = shared.engine().evaluate(&snap);
    if fx.is_empty() {
        return;
    }
    let Effects {
        upserts,
        deletes,
        marks,
        notices,
    } = fx;
    let events: Vec<NewEvent> = notices
        .iter()
        .map(|n| NewEvent {
            ts_ms: snap.now_ms,
            node_id: n.node_id,
            node_name: n.node_name.clone(),
            kind: n.kind,
            title: n.title.clone(),
            body: n.body.clone(),
            sent: SendStatus::Pending,
        })
        .collect();
    let db = state.db.clone();
    // 状态与事件各自一个事务：状态写失败不影响事件记录与通知。
    let res = tokio::task::spawn_blocking(move || {
        let applied = if upserts.is_empty() && deletes.is_empty() && marks.is_empty() {
            Ok(())
        } else {
            db.apply_alert_effects(&upserts, &deletes, &marks)
        };
        let ids = if events.is_empty() {
            Ok(Vec::new())
        } else {
            db.insert_alert_events(&events)
        };
        (applied, ids)
    })
    .await;
    match res {
        Ok((applied, ids)) => {
            if let Err(e) = applied {
                tracing::error!(error = %e, "写入告警状态失败");
            }
            match ids {
                Ok(ids) => queue.enqueue(ids, notices),
                Err(e) => tracing::error!(error = %e, "写入告警事件失败"),
            }
        }
        Err(e) => tracing::error!(error = %e, "告警写库任务异常"),
    }
}

/// 启动周期评估任务（首轮立即执行）；收到 shutdown 退出。
pub(crate) fn spawn(
    state: AppState,
    queue: NotifyQueue,
    mut shutdown: watch::Receiver<bool>,
    tick: Duration,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(tick);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = interval.tick() => {}
                _ = shutdown.wait_for(|v| *v) => return,
            }
            tokio::select! {
                _ = run_tick(&state, &queue) => {}
                _ = shutdown.wait_for(|v| *v) => return,
            }
        }
    })
}
