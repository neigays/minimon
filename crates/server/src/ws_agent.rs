//! `GET /api/agent/ws`：agent 接入。

use crate::state::AppState;
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderMap, header};
use axum::response::Response;
use minimon_common::protocol::{AgentMsg, ErrorCode, Hello, ServerMsg, check_version};
use std::net::SocketAddr;
use std::time::Duration;
use tokio::sync::oneshot;
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
                Some(id) => session(state, id, ip, socket).await,
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

async fn session(state: AppState, node_id: i64, ip: std::net::IpAddr, mut socket: WebSocket) {
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

    let (conn_id, kick_rx) = state.registry().connect(node_id, hello);
    tracing::info!(node_id, conn_id, "agent 已连接");

    run(&state, node_id, conn_id, kick_rx, &mut socket).await;

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
    socket: &mut WebSocket,
) {
    let mut shutdown = state.shutdown_rx();
    let mut ping = interval(PING_INTERVAL);
    ping.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut last_seen = Instant::now();
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
                        Ok(AgentMsg::Report(report)) => {
                            if !state.registry().update(node_id, conn_id, report.clone()) {
                                // 已被新连接替换（kick 与 recv 同时就绪）。
                                reject(socket, ErrorCode::Replaced, "同一 token 有新连接，当前连接被替换").await;
                                return;
                            }
                            state.ingest.push(node_id, report);
                        }
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
