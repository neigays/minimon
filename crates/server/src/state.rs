//! 共享状态：在线表、实时事件广播、入库句柄。

use crate::config::ServerConfig;
use crate::db::Db;
use minimon_common::protocol::{Hello, Report};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use tokio::sync::{broadcast, oneshot};

const LIVE_CAPACITY: usize = 256;

#[derive(Clone)]
pub struct AppState {
    pub(crate) db: Arc<Db>,
    registry: Registry,
    live: LiveHub,
    pub(crate) ingest: IngestHandle,
    pub(crate) cfg: Arc<ServerConfig>,
}

impl AppState {
    pub fn new(db: Arc<Db>, cfg: ServerConfig) -> Self {
        AppState {
            db,
            registry: Registry::default(),
            live: LiveHub::new(),
            ingest: IngestHandle,
            cfg: Arc::new(cfg),
        }
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
#[derive(Clone, Default)]
pub struct Registry {
    sessions: Arc<RwLock<HashMap<i64, Session>>>,
    next_conn: Arc<AtomicU64>,
}

impl Registry {
    /// 注册新会话。已有会话时先通知其退出再替换。
    pub fn connect(&self, node_id: i64, info: Hello) -> (u64, oneshot::Receiver<()>) {
        let conn_id = self.next_conn.fetch_add(1, Ordering::Relaxed) + 1;
        let (kick, kick_rx) = oneshot::channel();
        let session = Session {
            conn_id,
            latest: None,
            info,
            kick,
        };
        let old = self
            .sessions
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(node_id, session);
        if let Some(old) = old {
            let _ = old.kick.send(());
        }
        (conn_id, kick_rx)
    }

    /// 更新最新上报；会话已被替换时返回 false。
    pub fn update(&self, node_id: i64, conn_id: u64, report: Report) -> bool {
        let mut map = self.sessions.write().unwrap_or_else(|e| e.into_inner());
        match map.get_mut(&node_id) {
            Some(s) if s.conn_id == conn_id => {
                s.latest = Some(report);
                true
            }
            _ => false,
        }
    }

    /// 仅当 conn_id 匹配时移除；返回是否真的移除了（被踢的旧连接返回 false）。
    pub fn disconnect(&self, node_id: i64, conn_id: u64) -> bool {
        let mut map = self.sessions.write().unwrap_or_else(|e| e.into_inner());
        if map.get(&node_id).is_some_and(|s| s.conn_id == conn_id) {
            map.remove(&node_id);
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

    pub fn publish(&self, ev: LiveEvent) {
        // 没有订阅者时 send 返回 Err，属正常情况。
        let _ = self.tx.send(ev);
    }
}

/// 入库句柄。Task 6 替换为真正的批量写入；目前丢弃数据。
#[derive(Clone)]
pub struct IngestHandle;

impl IngestHandle {
    pub fn push(&self, node_id: i64, report: Report) {
        let _ = (node_id, report);
    }
}

#[cfg(test)]
mod tests {
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
        let r = Registry::default();
        let (c1, mut k1) = r.connect(7, hello());
        let (c2, _k2) = r.connect(7, hello());
        assert!(k1.try_recv().is_ok(), "旧会话应收到 kick");
        assert!(!r.disconnect(7, c1));
        assert_eq!(r.snapshot().len(), 1);
        assert!(r.info(7).is_some());
        assert!(r.disconnect(7, c2));
        assert!(r.snapshot().is_empty());
    }
}
