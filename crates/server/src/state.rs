//! 共享状态：在线表、实时事件广播、入库句柄。

use crate::config::ServerConfig;
use crate::db::{Db, DbError, Node};
pub use crate::ingest::IngestHandle;
use crate::ingest::{DEFAULT_FLUSH_EVERY, DEFAULT_MAX_BATCH, IngestJoin};
use crate::period::{DEFAULT_TZ, Tz, parse_tz};
use minimon_common::protocol::{ErrorCode, Hello, Report};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};
use tokio::sync::{Notify, broadcast, oneshot, watch};

#[derive(Debug, thiserror::Error)]
pub enum StateError {
    #[error(transparent)]
    Db(#[from] DbError),
    #[error("无法启动 ingest 线程: {0}")]
    Ingest(#[from] std::io::Error),
}

const HASH_CONCURRENCY: usize = 2;
const LIVE_CAPACITY: usize = 256;

#[derive(Clone)]
pub struct AppState {
    pub(crate) db: Arc<Db>,
    registry: Registry,
    live: LiveHub,
    /// 节点列表缓存；启动时加载，运行中遇到未知节点时由 `reload_nodes` 刷新。
    nodes: Arc<RwLock<Arc<Vec<Node>>>>,
    /// 每节点最后一条已知上报（在线与离线都用它）；启动时从库恢复。
    last_known: LastKnown,
    /// 串行化节点缓存重载，避免并发重载乱序完成留下旧缓存。
    reload_lock: Arc<tokio::sync::Mutex<()>>,
    require_login: Arc<AtomicBool>,
    /// 全局时区（账期与到期日计算）；启动时从 `settings.timezone` 加载。
    timezone: Arc<RwLock<Tz>>,
    /// 每节点本期流量用量缓存（仅管理员视图使用）。
    traffic: Arc<RwLock<HashMap<i64, crate::traffic::TrafficUsage>>>,
    traffic_lock: Arc<tokio::sync::Mutex<()>>,
    /// 探测任务缓存（增删改后重载）。
    pub(crate) probe: Arc<crate::probe::ProbeCache>,
    pub(crate) ingest: IngestHandle,
    pub(crate) cfg: Arc<ServerConfig>,
    shutdown: Arc<watch::Sender<bool>>,
    sessions: Arc<SessionTracker>,
    pub(crate) login_limiter: Arc<crate::auth::LoginLimiter>,
    /// 限制并发 argon2 哈希 / 校验数（每个约 19 MiB）。
    pub(crate) hash_gate: Arc<tokio::sync::Semaphore>,
    /// 告警引擎与评估串行锁；启动时从库恢复。评估任务与通知队列由 `serve_on` 启动。
    alert: Arc<crate::alert::runtime::AlertShared>,
}

fn default_tz() -> Tz {
    // DEFAULT_TZ 是内置库中的常量；万一缺失退化为 UTC，不 panic。
    parse_tz(DEFAULT_TZ).unwrap_or(Tz::UTC)
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
        let probe_tasks = db.list_probe_tasks()?;
        let live = LiveHub::new();
        let last_known: LastKnown = Arc::new(RwLock::new(db.last_reports()?));
        // 缺省（未设置）表示关闭。
        let require_login = db.get_setting("require_login")?.as_deref() == Some("1");
        let timezone = match db.get_setting("timezone")? {
            None => default_tz(),
            Some(name) => parse_tz(&name).unwrap_or_else(|| {
                tracing::warn!(timezone = %name, "settings.timezone 无效，使用默认时区");
                default_tz()
            }),
        };
        let alert = Arc::new(crate::alert::runtime::AlertShared::load(&db)?);
        let (ingest, join) =
            IngestHandle::spawn(db.clone(), DEFAULT_FLUSH_EVERY, DEFAULT_MAX_BATCH)?;
        let state = AppState {
            db,
            registry: Registry::with_last_known(live.clone(), last_known.clone()),
            live,
            nodes,
            last_known,
            reload_lock: Arc::default(),
            require_login: Arc::new(AtomicBool::new(require_login)),
            timezone: Arc::new(RwLock::new(timezone)),
            traffic: Arc::default(),
            traffic_lock: Arc::default(),
            probe: Arc::new(crate::probe::ProbeCache::with(probe_tasks)),
            ingest,
            cfg: Arc::new(cfg),
            shutdown: Arc::new(watch::channel(false).0),
            sessions: Arc::default(),
            login_limiter: Arc::default(),
            hash_gate: Arc::new(tokio::sync::Semaphore::new(HASH_CONCURRENCY)),
            alert,
        };
        Ok((state, join))
    }

    /// 当前节点缓存快照（锁只在克隆 Arc 期间持有）。
    pub(crate) fn nodes(&self) -> Arc<Vec<Node>> {
        self.nodes.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// 原地修改缓存中的某个节点（写时复制，读者持有的旧快照不受影响）。
    pub(crate) fn modify_node(&self, id: i64, f: impl FnOnce(&mut Node)) {
        let mut guard = self.nodes.write().unwrap_or_else(|e| e.into_inner());
        let mut list = Vec::clone(&guard);
        if let Some(n) = list.iter_mut().find(|n| n.id == id) {
            f(n);
            *guard = Arc::new(list);
        }
    }

    /// 最后已知上报的快照副本。
    pub(crate) fn last_known(&self) -> HashMap<i64, Report> {
        self.last_known
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// 节点被删除：丢弃其内存中的最后已知上报（SQLite 的 id 会被复用，不能让新节点继承）。
    pub(crate) fn forget_node(&self, id: i64) {
        self.last_known
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&id);
    }

    /// 整站登录是否开启。
    pub fn require_login(&self) -> bool {
        self.require_login.load(Ordering::SeqCst)
    }

    pub fn set_require_login(&self, on: bool) {
        self.require_login.store(on, Ordering::SeqCst);
    }

    /// 当前全局时区。
    pub fn timezone(&self) -> Tz {
        self.timezone
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn set_timezone(&self, tz: Tz) {
        *self.timezone.write().unwrap_or_else(|e| e.into_inner()) = tz;
    }

    pub(crate) fn traffic_cache(&self) -> &RwLock<HashMap<i64, crate::traffic::TrafficUsage>> {
        &self.traffic
    }

    pub(crate) fn traffic_lock(&self) -> &tokio::sync::Mutex<()> {
        &self.traffic_lock
    }

    /// 要求所有 live 会话按当前身份与设置重新同步（重发快照或关闭）。
    pub fn publish_resync(&self) {
        self.live.publish(LiveEvent::Resync);
    }

    /// 缓存中是否已有该节点。
    pub(crate) fn has_node(&self, id: i64) -> bool {
        self.nodes().iter().any(|n| n.id == id)
    }

    /// 立即把入库缓冲写入库（删除节点前用，避免残留上报写回已删节点）。
    pub(crate) async fn flush_ingest(&self) {
        let _ = self.ingest.flush().await;
    }

    /// 从库重新加载节点缓存（阻塞 IO 放到 spawn_blocking，锁不跨 await）。
    pub(crate) async fn reload_nodes(&self) -> Result<(), DbError> {
        // 锁跨 读库 + 写缓存：后开始的重载一定读到不早于先开始者的数据。
        let _serial = self.reload_lock.lock().await;
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

    /// 告警运行时共享状态。
    pub fn alert(&self) -> &crate::alert::runtime::AlertShared {
        &self.alert
    }

    pub fn registry(&self) -> &Registry {
        &self.registry
    }

    pub fn live(&self) -> &LiveHub {
        &self.live
    }
}

type LastKnown = Arc<RwLock<HashMap<i64, Report>>>;

struct Session {
    conn_id: u64,
    latest: Option<Report>,
    info: Hello,
    kick: oneshot::Sender<ErrorCode>,
    /// 探测任务脏标记：代数递增即通知会话按当前缓存重发（只保留最新，不排队）。
    dirty: watch::Sender<u64>,
}

/// 注册结果：连接号、kick 通道、探测任务脏标记接收端。
pub type Connected = (u64, oneshot::Receiver<ErrorCode>, watch::Receiver<u64>);

/// 在线节点表。锁只在内存操作期间持有，不跨 await。
///
/// Online/Offline/Report 事件在持锁期间发布，保证事件顺序与表状态一致。
#[derive(Clone)]
pub struct Registry {
    live: LiveHub,
    last_known: LastKnown,
    sessions: Arc<RwLock<HashMap<i64, Session>>>,
    /// 全局吊销序号；`epochs` 记录每个节点最近一次被吊销时的序号。
    /// 握手开始时（认证之前）记下全局序号，注册时（持 sessions 锁）核对。
    revoke_seq: Arc<AtomicU64>,
    epochs: Arc<RwLock<HashMap<i64, u64>>>,
    next_conn: Arc<AtomicU64>,
}

impl Registry {
    pub fn new(live: LiveHub) -> Self {
        Self::with_last_known(live, Arc::default())
    }

    fn with_last_known(live: LiveHub, last_known: LastKnown) -> Self {
        Registry {
            live,
            last_known,
            sessions: Arc::default(),
            revoke_seq: Arc::default(),
            epochs: Arc::default(),
            next_conn: Arc::default(),
        }
    }

    /// 当前吊销序号（认证 token 之前读取）。
    pub fn epoch(&self) -> u64 {
        self.revoke_seq.load(Ordering::SeqCst)
    }

    /// 以当前序号注册（测试与无握手场景）。
    pub fn connect(&self, node_id: i64, info: Hello) -> Connected {
        match self.connect_at(node_id, info, self.epoch()) {
            Ok(c) => c,
            // 与并发 revoke 撞上：返回一个已被吊销、无会话的连接 id。
            Err(_) => (0, oneshot::channel().1, watch::channel(0).1),
        }
    }

    /// 注册新会话（发布 Online）。已有会话时先通知其退出再替换。
    /// 握手期间节点被吊销（纪元变化）时拒绝，调用方应回复 `Unauthorized`。
    pub fn connect_at(
        &self,
        node_id: i64,
        info: Hello,
        epoch: u64,
    ) -> Result<Connected, ErrorCode> {
        let conn_id = self.next_conn.fetch_add(1, Ordering::Relaxed) + 1;
        let (kick, kick_rx) = oneshot::channel();
        let (dirty, dirty_rx) = watch::channel(0);
        let session = Session {
            conn_id,
            latest: None,
            info,
            kick,
            dirty,
        };
        let mut map = self.sessions.write().unwrap_or_else(|e| e.into_inner());
        // 锁顺序固定：sessions -> epochs。
        let revoked_at = self
            .epochs
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&node_id)
            .copied()
            .unwrap_or(0);
        if revoked_at > epoch {
            return Err(ErrorCode::Unauthorized);
        }
        if let Some(old) = map.insert(node_id, session) {
            let _ = old.kick.send(ErrorCode::Replaced);
        }
        self.live.publish(LiveEvent::Online { node_id });
        drop(map);
        Ok((conn_id, kick_rx, dirty_rx))
    }

    /// 在线节点 id。
    pub(crate) fn online_ids(&self) -> Vec<i64> {
        let map = self.sessions.read().unwrap_or_else(|e| e.into_inner());
        map.keys().copied().collect()
    }

    /// 标记在线节点的探测任务已变化；不在线返回 false。会话按发送时的缓存重算，
    /// 多次标记自然合并，不存在旧快照后到或队列满丢新的问题。
    pub(crate) fn mark_dirty(&self, node_id: i64) -> bool {
        let map = self.sessions.read().unwrap_or_else(|e| e.into_inner());
        match map.get(&node_id) {
            Some(s) => {
                s.dirty.send_modify(|g| *g = g.wrapping_add(1));
                true
            }
            None => false,
        }
    }

    /// 吊销节点会话（重置 token / 删除节点）：通知连接以 `Unauthorized` 关闭，
    /// 移除会话并发布 Offline（持锁，顺序与表状态一致）。返回是否有会话被移除。
    pub fn revoke(&self, node_id: i64) -> bool {
        let mut map = self.sessions.write().unwrap_or_else(|e| e.into_inner());
        let seq = self.revoke_seq.fetch_add(1, Ordering::SeqCst) + 1;
        self.epochs
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(node_id, seq);
        match map.remove(&node_id) {
            Some(s) => {
                let _ = s.kick.send(ErrorCode::Unauthorized);
                self.live.publish(LiveEvent::Offline { node_id });
                true
            }
            None => false,
        }
    }

    /// 更新最新上报并发布 Report；会话已被替换时返回 false。
    pub fn update(&self, node_id: i64, conn_id: u64, report: Report) -> bool {
        let mut map = self.sessions.write().unwrap_or_else(|e| e.into_inner());
        match map.get_mut(&node_id) {
            Some(s) if s.conn_id == conn_id => {
                s.latest = Some(report.clone());
                // 与事件同在会话锁内更新，视图读到的 latest 不会落后于已发布的事件。
                self.last_known
                    .write()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(node_id, report.clone());
                self.live.publish(LiveEvent::Report { node_id, report });
                true
            }
            _ => false,
        }
    }

    /// 仅当该连接仍是节点的当前会话时，在持有会话锁期间执行 `f`；否则返回 None。
    /// 吊销（持写锁）与之互斥：吊销之后不会再有 `f` 执行。
    pub fn with_owner<T>(&self, node_id: i64, conn_id: u64, f: impl FnOnce() -> T) -> Option<T> {
        let map = self.sessions.read().unwrap_or_else(|e| e.into_inner());
        map.get(&node_id)
            .is_some_and(|s| s.conn_id == conn_id)
            .then(f)
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
    Online {
        node_id: i64,
    },
    Offline {
        node_id: i64,
    },
    Report {
        node_id: i64,
        report: Report,
    },
    /// 本期流量用量更新（仅管理员连接收到）。
    Traffic {
        node_id: i64,
        usage: crate::traffic::TrafficUsage,
    },
    /// 探测任务最新结果（仅 `show_on_card` 的任务发布；不含目标地址）。
    Probe {
        node_id: i64,
        probe: crate::probe::CardProbe,
    },
    /// 可见性或整站登录设置变化：订阅者应重新同步。
    Resync,
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

    pub(crate) fn publish(&self, ev: LiveEvent) {
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
        let (c1, mut k1, _o1) = r.connect(7, hello());
        let (c2, _k2, _o2) = r.connect(7, hello());
        assert!(k1.try_recv().is_ok(), "旧会话应收到 kick");
        assert!(!r.disconnect(7, c1));
        assert_eq!(r.snapshot().len(), 1);
        assert!(r.info(7).is_some());
        assert!(r.disconnect(7, c2));
        assert!(r.snapshot().is_empty());
    }

    #[tokio::test]
    async fn revoke_kicks_with_unauthorized_and_publishes_offline() {
        let live = LiveHub::new();
        let mut rx = live.subscribe();
        let r = Registry::new(live);
        let (c, mut k, _o) = r.connect(7, hello());
        assert!(r.revoke(7));
        assert_eq!(k.try_recv().unwrap(), ErrorCode::Unauthorized);
        assert!(r.snapshot().is_empty());
        assert!(!r.disconnect(7, c), "会话已移除，旧连接不得再发 Offline");
        assert!(!r.revoke(7));
        let evs: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert!(matches!(
            evs.as_slice(),
            [LiveEvent::Online { .. }, LiveEvent::Offline { .. }]
        ));
    }

    #[tokio::test]
    async fn with_owner_requires_current_session() {
        let r = Registry::new(LiveHub::new());
        let (c1, _k1, _o1) = r.connect(7, hello());
        assert_eq!(r.with_owner(7, c1, || 1), Some(1));
        assert_eq!(r.with_owner(8, c1, || 1), None);
        let (c2, _k2, _o2) = r.connect(7, hello());
        assert_eq!(r.with_owner(7, c1, || 1), None, "被替换的旧连接");
        assert_eq!(r.with_owner(7, c2, || 2), Some(2));
        r.revoke(7);
        assert_eq!(r.with_owner(7, c2, || 2), None, "吊销后");
    }

    fn app_state() -> AppState {
        let db = Arc::new(Db::open_in_memory().unwrap());
        AppState::new(db, ServerConfig::default()).unwrap().0
    }

    #[tokio::test]
    async fn timezone_defaults_and_persists_across_restart() {
        let db = Arc::new(Db::open_in_memory().unwrap());
        let (st, _j) = AppState::new(db.clone(), ServerConfig::default()).unwrap();
        assert_eq!(st.timezone(), crate::period::parse_tz(DEFAULT_TZ).unwrap());
        let utc = crate::period::parse_tz("UTC").unwrap();
        st.set_timezone(utc.clone());
        assert_eq!(st.timezone(), utc);
        db.set_setting("timezone", "UTC").unwrap();
        let (st2, _j2) = AppState::new(db.clone(), ServerConfig::default()).unwrap();
        assert_eq!(st2.timezone(), utc);
        // 非法值回落到默认
        db.set_setting("timezone", "Mars/Olympus").unwrap();
        let (st3, _j3) = AppState::new(db, ServerConfig::default()).unwrap();
        assert_eq!(st3.timezone(), crate::period::parse_tz(DEFAULT_TZ).unwrap());
    }

    #[tokio::test]
    async fn forget_node_drops_last_known() {
        let st = app_state();
        st.last_known.write().unwrap().insert(5, report());
        st.forget_node(5);
        assert!(st.last_known().is_empty());
    }

    #[tokio::test]
    async fn concurrent_reloads_end_with_cache_matching_db() {
        let st = app_state();
        let mut tasks = Vec::new();
        for i in 0..8 {
            let st = st.clone();
            tasks.push(tokio::spawn(async move {
                let db = st.db.clone();
                let name = format!("n{i}");
                tokio::task::spawn_blocking(move || db.add_node(&name))
                    .await
                    .unwrap()
                    .unwrap();
                st.reload_nodes().await.unwrap();
            }));
        }
        for t in tasks {
            t.await.unwrap();
        }
        let in_db = st.db.list_nodes().unwrap();
        assert_eq!(in_db.len(), 8);
        assert_eq!(*st.nodes(), in_db);
    }

    #[tokio::test]
    async fn stale_epoch_connect_is_rejected_after_revoke() {
        let r = Registry::new(LiveHub::new());
        let stale = r.epoch();
        assert!(!r.revoke(7), "无会话也要推进纪元");
        assert_eq!(
            r.connect_at(7, hello(), stale).err(),
            Some(ErrorCode::Unauthorized)
        );
        assert!(r.snapshot().is_empty());
        let fresh = r.epoch();
        assert!(r.connect_at(7, hello(), fresh).is_ok());
        assert_eq!(r.snapshot().len(), 1);
        // 其他节点不受影响
        assert!(r.connect_at(8, hello(), 0).is_ok());
    }

    #[tokio::test]
    async fn events_follow_table_state_in_order() {
        let live = LiveHub::new();
        let mut rx = live.subscribe();
        let r = Registry::new(live);
        let (c1, _k1, _o1) = r.connect(7, hello());
        let (c2, _k2, _o2) = r.connect(7, hello());
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
