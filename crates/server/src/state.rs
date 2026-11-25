//! 共享状态：在线表、实时事件广播、入库句柄。

use crate::config::ServerConfig;
use crate::db::{Db, DbError, Node};
pub use crate::ingest::IngestHandle;
use crate::ingest::{DEFAULT_FLUSH_EVERY, DEFAULT_MAX_BATCH, IngestJoin};
use minimon_common::protocol::{Hello, Report};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};
use tokio::sync::{Notify, broadcast, oneshot, watch};

#[derive(Debug, thiserror::Error)]
pub enum StateError {
    #[error(transparent)]
    Db(#[from] DbError),
    #[error("无法启动 ingest 线程: {0}")]
    Ingest(#[from] std::io::Error),
}

const LIVE_CAPACITY: usize = 256;

#[derive(Clone)]
pub struct AppState {
    pub(crate) db: Arc<Db>,
    registry: Registry,
    live: LiveHub,
    /// 节点列表缓存；启动时加载，运行中遇到未知节点时由 `reload_nodes` 刷新。
    nodes: Arc<RwLock<Arc<Vec<Node>>>>,
    pub(crate) ingest: IngestHandle,
    pub(crate) cfg: Arc<ServerConfig>,
    shutdown: Arc<watch::Sender<bool>>,
    sessions: Arc<SessionTracker>,
    pub(crate) login_limiter: Arc<crate::auth::LoginLimiter>,
}

/// 活跃 WS 会话计数。upgrade 后的连接不受 axum 优雅停机管理，需自行等待。
#[derive(Default)]
struct SessionTracker {
    active: AtomicUsize,
    idle: Notify,
}

/// 会话存活期间持有；drop 时计数减一。
pub struct SessionGuard(Arc<SessionTracker>);

impl Drop for SessionGuard {
    fn drop(&mut self) {
        if self.0.active.fetch_sub(1, Ordering::SeqCst) == 1 {
            self.0.idle.notify_waiters();
        }
    }
}

impl AppState {
    /// 创建状态并启动入库线程；`IngestJoin` 交给负责关停的一方（`serve_on`）。
    pub fn new(db: Arc<Db>, cfg: ServerConfig) -> Result<(Self, IngestJoin), StateError> {
        let nodes = Arc::new(RwLock::new(Arc::new(db.list_nodes()?)));
        let live = LiveHub::new();
        let (ingest, join) =
            IngestHandle::spawn(db.clone(), DEFAULT_FLUSH_EVERY, DEFAULT_MAX_BATCH)?;
        let state = AppState {
            db,
            registry: Registry::new(live.clone()),
            live,
            nodes,
            ingest,
            cfg: Arc::new(cfg),
            shutdown: Arc::new(watch::channel(false).0),
            sessions: Arc::default(),
            login_limiter: Arc::default(),
        };
        Ok((state, join))
    }

    /// 当前节点缓存快照（锁只在克隆 Arc 期间持有）。
    pub(crate) fn nodes(&self) -> Arc<Vec<Node>> {
        self.nodes.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// 缓存中是否已有该节点。
    pub(crate) fn has_node(&self, id: i64) -> bool {
        self.nodes().iter().any(|n| n.id == id)
    }

    /// 从库重新加载节点缓存（阻塞 IO 放到 spawn_blocking，锁不跨 await）。
    pub(crate) async fn reload_nodes(&self) -> Result<(), DbError> {
        let db = self.db.clone();
        let list = tokio::task::spawn_blocking(move || db.list_nodes())
            .await
            .map_err(|_| DbError::Poisoned)??;
        *self.nodes.write().unwrap_or_else(|e| e.into_inner()) = Arc::new(list);
        Ok(())
    }

    pub(crate) fn session_guard(&self) -> SessionGuard {
        self.sessions.active.fetch_add(1, Ordering::SeqCst);
        SessionGuard(self.sessions.clone())
    }

    pub(crate) fn shutdown_rx(&self) -> watch::Receiver<bool> {
        self.shutdown.subscribe()
    }

    /// 通知所有 WS 会话关闭。
    pub(crate) fn begin_shutdown(&self) {
        self.shutdown.send_replace(true);
    }

    /// 等待活跃会话归零，最多 `max`；返回是否已全部结束。
    pub(crate) async fn wait_sessions(&self, max: std::time::Duration) -> bool {
        let wait = async {
            loop {
                let notified = self.sessions.idle.notified();
                if self.sessions.active.load(Ordering::SeqCst) == 0 {
                    return;
                }
                notified.await;
            }
        };
        tokio::time::timeout(max, wait).await.is_ok()
    }

    pub fn registry(&self) -> &Registry {
        &self.registry
    }

    pub fn live(&self) -> &LiveHub {
        &self.live
    }
}

struct Session {
    conn_id: u64,
    latest: Option<Report>,
    info: Hello,
    kick: oneshot::Sender<()>,
}

/// 在线节点表。锁只在内存操作期间持有，不跨 await。
///
/// Online/Offline/Report 事件在持锁期间发布，保证事件顺序与表状态一致。
#[derive(Clone)]
pub struct Registry {
    live: LiveHub,
    sessions: Arc<RwLock<HashMap<i64, Session>>>,
    next_conn: Arc<AtomicU64>,
}

impl Registry {
    pub fn new(live: LiveHub) -> Self {
        Registry {
            live,
            sessions: Arc::default(),
            next_conn: Arc::default(),
        }
    }

    /// 注册新会话（发布 Online）。已有会话时先通知其退出再替换。
    pub fn connect(&self, node_id: i64, info: Hello) -> (u64, oneshot::Receiver<()>) {
        let conn_id = self.next_conn.fetch_add(1, Ordering::Relaxed) + 1;
        let (kick, kick_rx) = oneshot::channel();
        let session = Session {
            conn_id,
            latest: None,
            info,
            kick,
        };
        let mut map = self.sessions.write().unwrap_or_else(|e| e.into_inner());
        if let Some(old) = map.insert(node_id, session) {
            let _ = old.kick.send(());
        }
        self.live.publish(LiveEvent::Online { node_id });
        drop(map);
        (conn_id, kick_rx)
    }

    /// 更新最新上报并发布 Report；会话已被替换时返回 false。
    pub fn update(&self, node_id: i64, conn_id: u64, report: Report) -> bool {
        let mut map = self.sessions.write().unwrap_or_else(|e| e.into_inner());
        match map.get_mut(&node_id) {
            Some(s) if s.conn_id == conn_id => {
                s.latest = Some(report.clone());
                self.live.publish(LiveEvent::Report { node_id, report });
                true
            }
            _ => false,
        }
    }

    /// 仅当 conn_id 匹配时移除并发布 Offline；返回是否真的移除了（被踢的旧连接返回 false）。
    pub fn disconnect(&self, node_id: i64, conn_id: u64) -> bool {
        let mut map = self.sessions.write().unwrap_or_else(|e| e.into_inner());
        if map.get(&node_id).is_some_and(|s| s.conn_id == conn_id) {
            map.remove(&node_id);
            self.live.publish(LiveEvent::Offline { node_id });
            true
        } else {
            false
        }
    }

    pub fn snapshot(&self) -> Vec<(i64, Option<Report>)> {
        let map = self.sessions.read().unwrap_or_else(|e| e.into_inner());
        let mut v: Vec<_> = map.iter().map(|(id, s)| (*id, s.latest.clone())).collect();
        v.sort_by_key(|(id, _)| *id);
        v
    }

    pub fn info(&self, node_id: i64) -> Option<Hello> {
        let map = self.sessions.read().unwrap_or_else(|e| e.into_inner());
        map.get(&node_id).map(|s| s.info.clone())
    }
}

#[derive(Debug, Clone)]
pub enum LiveEvent {
    Online { node_id: i64 },
    Offline { node_id: i64 },
    Report { node_id: i64, report: Report },
}

#[derive(Clone)]
pub struct LiveHub {
    tx: broadcast::Sender<LiveEvent>,
}

impl LiveHub {
    fn new() -> Self {
        LiveHub {
            tx: broadcast::channel(LIVE_CAPACITY).0,
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<LiveEvent> {
        self.tx.subscribe()
    }

    fn publish(&self, ev: LiveEvent) {
        // 没有订阅者时 send 返回 Err，属正常情况。
        let _ = self.tx.send(ev);
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn hello() -> Hello {
        Hello {
            v: 1,
            agent_version: "t".into(),
            hostname: "h".into(),
            os: "linux".into(),
            kernel: "k".into(),
            arch: "x".into(),
            cpu_model: "c".into(),
            cpu_cores: 1,
            mem_total: 1,
            swap_total: 0,
            disk_total: 1,
            virtualization: None,
        }
    }

    #[tokio::test]
    async fn stale_conn_cannot_remove_or_update_new_session() {
        let r = Registry::new(LiveHub::new());
        let (c1, mut k1) = r.connect(7, hello());
        let (c2, _k2) = r.connect(7, hello());
        assert!(k1.try_recv().is_ok(), "旧会话应收到 kick");
        assert!(!r.disconnect(7, c1));
        assert_eq!(r.snapshot().len(), 1);
        assert!(r.info(7).is_some());
        assert!(r.disconnect(7, c2));
        assert!(r.snapshot().is_empty());
    }

    #[tokio::test]
    async fn events_follow_table_state_in_order() {
        let live = LiveHub::new();
        let mut rx = live.subscribe();
        let r = Registry::new(live);
        let (c1, _k1) = r.connect(7, hello());
        let (c2, _k2) = r.connect(7, hello());
        assert!(!r.disconnect(7, c1));
        assert!(r.update(7, c2, report()));
        assert!(r.disconnect(7, c2));
        let evs: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert!(matches!(
            evs.as_slice(),
            [
                LiveEvent::Online { .. },
                LiveEvent::Online { .. },
                LiveEvent::Report { .. },
                LiveEvent::Offline { .. }
            ]
        ));
    }

    pub(crate) fn report() -> Report {
        Report {
            ts_ms: 1,
            cpu: 0.0,
            mem_used: 0,
            swap_used: 0,
            disk_used: 0,
            disk_total: 0,
            net_rx_bytes: 0,
            net_tx_bytes: 0,
            net_rx_rate: 0,
            net_tx_rate: 0,
            load1: 0.0,
            load5: 0.0,
            load15: 0.0,
            uptime_secs: 0,
            process_count: 0,
            tcp_count: 0,
            udp_count: 0,
        }
    }
}
