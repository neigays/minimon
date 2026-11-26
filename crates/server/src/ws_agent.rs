//! `GET /api/agent/ws`：agent 接入。

use crate::clock::TsNormalizer;
use crate::db::now_ms;
use crate::probe::tasks_for_node;
use crate::state::AppState;
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderMap, header};
use axum::response::Response;
use minimon_common::protocol::{
    AgentMsg, ErrorCode, Hello, NetInfo, ProbeResult, ServerMsg, check_version,
};
use std::net::SocketAddr;
use std::time::Duration;
use tokio::sync::{oneshot, watch};
use tokio::time::{Instant, MissedTickBehavior, interval, timeout};

/// agent 消息（Hello/Report）都很小，64 KiB 绰绰有余。
const AGENT_MAX_MSG: usize = 64 << 10;
const HELLO_TIMEOUT: Duration = Duration::from_secs(10);
const SEND_TIMEOUT: Duration = Duration::from_secs(10);
const PING_INTERVAL: Duration = Duration::from_secs(15);
const DEAD_AFTER: Duration = Duration::from_secs(45);

fn bearer(headers: &HeaderMap) -> Option<String> {
    let v = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, token) = v.split_once(' ')?;
    scheme
        .eq_ignore_ascii_case("bearer")
        .then(|| token.trim().to_owned())
}

pub async fn handler(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    let ip = crate::auth::client_ip(&headers, peer, state.cfg.real_ip_header.as_deref());
    let token = bearer(&headers);
    // 认证之前读取吊销纪元：认证到注册之间被重置 / 删除的节点不得注册。
    let epoch = state.registry().epoch();
    let node_id = match token {
        Some(t) => {
            let db = state.db.clone();
            match tokio::task::spawn_blocking(move || db.node_by_token(&t)).await {
                Ok(Ok(node)) => node.map(|n| n.id),
                Ok(Err(e)) => {
                    tracing::error!(error = %e, "查询节点 token 失败");
                    None
                }
                Err(e) => {
                    tracing::error!(error = %e, "查询节点任务失败");
                    None
                }
            }
        }
        None => None,
    };
    // 运行中新增的节点：先刷新缓存，保证注册前它已出现在 /api/nodes 与 live 过滤里。
    if let Some(id) = node_id
        && !state.has_node(id)
        && let Err(e) = state.reload_nodes().await
    {
        tracing::error!(error = %e, "刷新节点缓存失败");
    }
    // guard 在 upgrade 之前取得，确保停机时不会漏掉刚接入的会话。
    let guard = state.session_guard();
    ws.max_message_size(AGENT_MAX_MSG)
        .max_frame_size(AGENT_MAX_MSG)
        .on_upgrade(move |socket| async move {
            let _guard = guard;
            match node_id {
                Some(id) => session(state, id, epoch, ip, socket).await,
                None => {
                    let mut socket = socket;
                    reject(&mut socket, ErrorCode::Unauthorized, "token 无效").await;
                }
            }
        })
}

async fn send_msg(socket: &mut WebSocket, msg: &ServerMsg) -> bool {
    let Ok(text) = serde_json::to_string(msg) else {
        return false;
    };
    matches!(
        timeout(SEND_TIMEOUT, socket.send(Message::text(text))).await,
        Ok(Ok(()))
    )
}

async fn close(socket: &mut WebSocket) {
    let frame = CloseFrame {
        code: 1000,
        reason: "".into(),
    };
    let _ = timeout(SEND_TIMEOUT, socket.send(Message::Close(Some(frame)))).await;
}

/// 发送错误消息后关闭连接。
async fn reject(socket: &mut WebSocket, code: ErrorCode, message: &str) {
    let msg = ServerMsg::Error {
        code,
        message: message.to_owned(),
    };
    if send_msg(socket, &msg).await {
        close(socket).await;
    }
}

enum HelloError {
    /// 不是（可解析的）Hello。
    Invalid,
    /// 是 Hello，但大版本不同，连字段形状都可能变了。
    IncompatibleVersion,
}

fn parse_hello(text: &str) -> Result<Hello, HelloError> {
    match serde_json::from_str::<AgentMsg>(text) {
        Ok(AgentMsg::Hello(h)) => Ok(h),
        Ok(_) => Err(HelloError::Invalid),
        Err(_) => {
            // 完整解析失败：只探测 type 与 v，大版本变化时仍能回报 incompatible_version。
            #[derive(serde::Deserialize)]
            struct Probe {
                #[serde(rename = "type")]
                ty: String,
                v: u32,
            }
            match serde_json::from_str::<Probe>(text) {
                Ok(p) if p.ty == "hello" && check_version(p.v).is_err() => {
                    Err(HelloError::IncompatibleVersion)
                }
                _ => Err(HelloError::Invalid),
            }
        }
    }
}

/// 等待第一条文本消息并要求它是 Hello。
async fn read_hello(socket: &mut WebSocket) -> Result<Hello, HelloError> {
    let deadline = Instant::now() + HELLO_TIMEOUT;
    loop {
        let msg = timeout(
            deadline.saturating_duration_since(Instant::now()),
            socket.recv(),
        )
        .await
        .ok()
        .flatten()
        .and_then(|r| r.ok())
        .ok_or(HelloError::Invalid)?;
        match msg {
            Message::Text(t) => return parse_hello(&t),
            Message::Ping(_) | Message::Pong(_) => continue,
            _ => return Err(HelloError::Invalid),
        }
    }
}

async fn session(
    state: AppState,
    node_id: i64,
    epoch: u64,
    ip: std::net::IpAddr,
    mut socket: WebSocket,
) {
    let hello = match read_hello(&mut socket).await {
        Ok(h) => h,
        Err(HelloError::IncompatibleVersion) => {
            reject(
                &mut socket,
                ErrorCode::IncompatibleVersion,
                "协议版本不兼容",
            )
            .await;
            return;
        }
        Err(HelloError::Invalid) => {
            tracing::debug!(node_id, "首条消息不是有效 Hello，断开");
            close(&mut socket).await;
            return;
        }
    };
    if let Err(code) = check_version(hello.v) {
        reject(&mut socket, code, "协议版本不兼容").await;
        return;
    }
    {
        let db = state.db.clone();
        let info = hello.clone();
        let ip_s = ip.to_string();
        let saved = tokio::task::spawn_blocking(move || {
            db.save_node_info(node_id, &info)?;
            db.set_node_ip(node_id, &ip_s)
        })
        .await;
        match saved {
            Ok(Ok(())) => {
                // 同步缓存：离线后视图仍能给出最近的 Hello 与 IP。
                let (h, ip_s) = (hello.clone(), ip.to_string());
                state.modify_node(node_id, move |n| {
                    n.info = Some(h);
                    n.last_ip = Some(ip_s);
                });
            }
            Ok(Err(e)) => tracing::warn!(node_id, error = %e, "保存节点信息失败"),
            Err(e) => tracing::warn!(node_id, error = %e, "保存节点信息任务失败"),
        }
    }
    let welcome = ServerMsg::Welcome {
        interval_secs: state.cfg.report_interval_secs,
    };
    if !send_msg(&mut socket, &welcome).await {
        return;
    }

    let (conn_id, kick_rx, dirty_rx) = match state.registry().connect_at(node_id, hello, epoch) {
        Ok(c) => c,
        Err(code) => {
            // 握手期间 token 被重置或节点被删除。
            reject(&mut socket, code, "token 已失效或节点已删除").await;
            return;
        }
    };
    tracing::info!(node_id, conn_id, "agent 已连接");
    // Welcome 已发出、会话已注册（信号先于计算）：立即下发当前任务（即使为空）。
    state.dispatch_probe_tasks(Some(vec![node_id]));

    run(&state, node_id, conn_id, kick_rx, dirty_rx, &mut socket).await;

    // 被替换的旧连接 disconnect 返回 false：不发 Offline，也不动新会话。
    if state.registry().disconnect(node_id, conn_id) {
        tracing::info!(node_id, conn_id, "agent 已离线");
    }
}

async fn run(
    state: &AppState,
    node_id: i64,
    conn_id: u64,
    mut kick_rx: oneshot::Receiver<ErrorCode>,
    mut dirty_rx: watch::Receiver<u64>,
    socket: &mut WebSocket,
) {
    let mut shutdown = state.shutdown_rx();
    let mut ping = interval(PING_INTERVAL);
    ping.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut last_seen = Instant::now();
    // 时间偏差告警每个连接只记录一次。
    let mut skew_warned = false;
    // 以节点最后已知上报播种，保证跨重连也不回退。
    let seed = state.last_known().get(&node_id).map(|r| r.ts_ms);
    let mut norm = TsNormalizer::new(seed, now_ms());
    let mut probe_norm = TsNormalizer::new(None, now_ms());
    loop {
        tokio::select! {
            _ = async { let _ = shutdown.wait_for(|v| *v).await; } => {
                close(socket).await;
                return;
            }
            kick = &mut kick_rx => {
                match kick.unwrap_or(ErrorCode::Replaced) {
                    ErrorCode::Unauthorized => reject(socket, ErrorCode::Unauthorized, "token 已失效或节点已删除").await,
                    code => reject(socket, code, "同一 token 有新连接，当前连接被替换").await,
                }
                return;
            }
            Ok(()) = dirty_rx.changed() => {
                // 发送时才按当前缓存计算：期间再有变化会再次触发，始终以最新为准。
                let msg = ServerMsg::ProbeTasks {
                    tasks: tasks_for_node(&state.probe_tasks(), node_id),
                };
                if !send_msg(socket, &msg).await {
                    return;
                }
            }
            _ = ping.tick() => {
                if last_seen.elapsed() >= DEAD_AFTER {
                    tracing::info!(node_id, conn_id, "长时间未收到数据，断开");
                    return;
                }
                if timeout(SEND_TIMEOUT, socket.send(Message::Ping(Vec::new().into())))
                    .await
                    .map_or(true, |r| r.is_err())
                {
                    return;
                }
            }
            msg = socket.recv() => {
                let Some(Ok(msg)) = msg else { return };
                last_seen = Instant::now();
                match msg {
                    Message::Text(t) => match serde_json::from_str::<AgentMsg>(&t) {
                        Ok(AgentMsg::Report(mut report)) => {
                            let (ts, replaced) = norm.apply(report.ts_ms, now_ms());
                            if replaced {
                                if !skew_warned && report.ts_ms.abs_diff(ts) > 10_000 {
                                    skew_warned = true;
                                    tracing::warn!(
                                        node_id,
                                        skew_secs = report.ts_ms.saturating_sub(ts) / 1000,
                                        "agent 时间与服务器相差超过 10 秒（agent − server），改用接收时间"
                                    );
                                }
                                report.ts_ms = ts;
                            }
                            if !state.registry().update(node_id, conn_id, report.clone()) {
                                // 会话已不在表中（kick 与 recv 同时就绪）：以 kick 带的原因为准，
                                // 吊销与替换的提示不同。
                                match kick_rx.try_recv() {
                                    Ok(ErrorCode::Unauthorized) => reject(socket, ErrorCode::Unauthorized, "token 已失效或节点已删除").await,
                                    _ => reject(socket, ErrorCode::Replaced, "同一 token 有新连接，当前连接被替换").await,
                                }
                                return;
                            }
                            state.ingest.push(node_id, report);
                        }
                        Ok(AgentMsg::ProbeResult(r)) => {
                            let task_id = r.task_id;
                            let accepted = handle_probe_result(state, node_id, conn_id, r, &mut probe_norm);
                            if !accepted {
                                tracing::debug!(node_id, task_id, "丢弃无效或不属于该节点的探测结果");
                            }
                        }
                        Ok(AgentMsg::NetInfo(n)) => handle_netinfo(state, node_id, n).await,
                        Ok(_) => {}
                        Err(e) => tracing::debug!(node_id, error = %e, "忽略无法解析的消息"),
                    },
                    Message::Close(_) => return,
                    _ => {}
                }
            }
        }
    }
}

/// 丢弃非法字段：ipv4/ipv6 必须能解析为对应族；country 必须是 2 位 ASCII 字母（转大写）。
pub(crate) fn sanitize_netinfo(n: NetInfo) -> NetInfo {
    let ip_of = |s: Option<String>, v4: bool| {
        s.filter(|s| match s.parse::<std::net::IpAddr>() {
            Ok(std::net::IpAddr::V4(_)) => v4,
            Ok(std::net::IpAddr::V6(_)) => !v4,
            Err(_) => false,
        })
    };
    NetInfo {
        ipv4: ip_of(n.ipv4, true),
        ipv6: ip_of(n.ipv6, false),
        country: n
            .country
            .filter(|c| c.len() == 2 && c.bytes().all(|b| b.is_ascii_alphabetic()))
            .map(|c| c.to_ascii_uppercase()),
    }
}

/// 保存 NetInfo 并同步缓存与 live；全部字段非法时忽略。
async fn handle_netinfo(state: &AppState, node_id: i64, n: NetInfo) {
    let n = sanitize_netinfo(n);
    if n == NetInfo::default() {
        tracing::debug!(node_id, "NetInfo 无有效字段，忽略");
        return;
    }
    let db = state.db.clone();
    let saved = n.clone();
    match tokio::task::spawn_blocking(move || db.set_netinfo(node_id, &saved)).await {
        Ok(Ok(())) => {
            state.modify_node(node_id, move |node| {
                if n.ipv4.is_some() {
                    node.ipv4 = n.ipv4;
                }
                if n.ipv6.is_some() {
                    node.ipv6 = n.ipv6;
                }
                if n.country.is_some() {
                    node.country = n.country;
                }
            });
            state.publish_resync();
        }
        Ok(Err(e)) => tracing::warn!(node_id, error = %e, "保存 NetInfo 失败"),
        Err(e) => tracing::warn!(node_id, error = %e, "保存 NetInfo 任务失败"),
    }
}

/// 处理探测结果帧：仅当 `conn_id` 仍是该节点的当前会话时才接受
/// （会话被吊销或替换后，在途结果不写最新值、不发事件、不入库）。
pub(crate) fn handle_probe_result(
    state: &AppState,
    node_id: i64,
    conn_id: u64,
    mut r: ProbeResult,
    norm: &mut TsNormalizer,
) -> bool {
    let now = now_ms();
    r.ts_ms = norm.apply(r.ts_ms, now).0;
    state
        .registry()
        .with_owner(node_id, conn_id, || {
            state.accept_probe_result(node_id, r, now)
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ServerConfig;
    use crate::db::{Db, NewProbeTask};
    use minimon_common::protocol::ProbeKind;
    use std::sync::Arc;

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

    fn result(task_id: i64) -> ProbeResult {
        ProbeResult {
            task_id,
            ts_ms: now_ms(),
            sent: 1,
            lost: 0,
            avg_ms: Some(5.0),
            min_ms: None,
            max_ms: None,
            error: None,
        }
    }

    #[tokio::test]
    async fn probe_result_from_non_owning_session_is_dropped() {
        let db = Arc::new(Db::open_in_memory().unwrap());
        let (node, _) = db.add_node("n").unwrap();
        let task = db
            .create_probe_task(&NewProbeTask {
                name: "t".into(),
                kind: ProbeKind::Tcp,
                target: "h:1".into(),
                interval_secs: 60,
                samples: 1,
                timeout_ms: 1000,
                all_nodes: true,
                node_ids: vec![],
                show_on_card: true,
                sort: None,
            })
            .unwrap();
        let (state, _j) = AppState::new(db, ServerConfig::default()).unwrap();
        let (old, _k1, _o1) = state.registry().connect(node.id, hello());
        let mut nm = TsNormalizer::default();
        let mut rx = state.live().subscribe();
        let (new, _k2, _o2) = state.registry().connect(node.id, hello());
        while rx.try_recv().is_ok() {}

        // 被替换的旧连接：丢弃，不写最新值、不发事件。
        assert!(!handle_probe_result(
            &state,
            node.id,
            old,
            result(task.id),
            &mut nm
        ));
        assert!(
            state
                .latest_probe(node.id)
                .iter()
                .all(|p| p.avg_ms.is_none())
        );
        assert!(rx.try_recv().is_err(), "不得发布探测事件");

        // 吊销之后连当前连接也被丢弃。
        state.registry().revoke(node.id);
        while rx.try_recv().is_ok() {}
        assert!(!handle_probe_result(
            &state,
            node.id,
            new,
            result(task.id),
            &mut nm
        ));
        assert!(
            state
                .latest_probe(node.id)
                .iter()
                .all(|p| p.avg_ms.is_none())
        );
        assert!(rx.try_recv().is_err());

        // 对照：当前会话的结果被接受。
        let (cur, _k3, _o3) = state.registry().connect(node.id, hello());
        assert!(handle_probe_result(
            &state,
            node.id,
            cur,
            result(task.id),
            &mut nm
        ));
        assert_eq!(state.latest_probe(node.id)[0].avg_ms, Some(5.0));
    }
}

#[cfg(test)]
mod netinfo_tests {
    use super::*;

    fn ni(v4: &str, v6: &str, c: &str) -> NetInfo {
        let o = |s: &str| (!s.is_empty()).then(|| s.to_string());
        NetInfo {
            ipv4: o(v4),
            ipv6: o(v6),
            country: o(c),
        }
    }

    #[test]
    fn sanitize_cases() {
        assert_eq!(
            sanitize_netinfo(ni("1.2.3.4", "2001:db8::1", "us")),
            ni("1.2.3.4", "2001:db8::1", "US")
        );
        assert_eq!(
            sanitize_netinfo(ni("::1", "1.2.3.4", "usa")),
            NetInfo::default()
        );
        for bad in ["中国", "X1", "u", ""] {
            assert_eq!(sanitize_netinfo(ni("", "", bad)).country, None, "{bad}");
        }
        assert_eq!(sanitize_netinfo(ni("1.2.3", "", "")).ipv4, None);
    }
}
