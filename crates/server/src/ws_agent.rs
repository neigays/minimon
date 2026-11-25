//! `GET /api/agent/ws`：agent 接入。

use crate::state::AppState;
use axum::extract::State;
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::http::{HeaderMap, header};
use axum::response::Response;
use minimon_common::protocol::{AgentMsg, ErrorCode, Hello, ServerMsg, check_version};
use std::time::Duration;
use tokio::sync::oneshot;
use tokio::time::{Instant, MissedTickBehavior, interval, timeout};

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
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
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
    // guard 在 upgrade 之前取得，确保停机时不会漏掉刚接入的会话。
    let guard = state.session_guard();
    ws.on_upgrade(move |socket| async move {
        let _guard = guard;
        match node_id {
            Some(id) => session(state, id, socket).await,
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

/// 等待第一条文本消息并要求它是 Hello。
async fn read_hello(socket: &mut WebSocket) -> Option<Hello> {
    let deadline = Instant::now() + HELLO_TIMEOUT;
    loop {
        let msg = timeout(
            deadline.saturating_duration_since(Instant::now()),
            socket.recv(),
        )
        .await
        .ok()??
        .ok()?;
        match msg {
            Message::Text(t) => {
                return match serde_json::from_str::<AgentMsg>(&t) {
                    Ok(AgentMsg::Hello(h)) => Some(h),
                    _ => None,
                };
            }
            Message::Ping(_) | Message::Pong(_) => continue,
            _ => return None,
        }
    }
}

async fn session(state: AppState, node_id: i64, mut socket: WebSocket) {
    let Some(hello) = read_hello(&mut socket).await else {
        tracing::debug!(node_id, "首条消息不是有效 Hello，断开");
        close(&mut socket).await;
        return;
    };
    if let Err(code) = check_version(hello.v) {
        reject(&mut socket, code, "协议版本不兼容").await;
        return;
    }
    {
        let db = state.db.clone();
        let info = hello.clone();
        match tokio::task::spawn_blocking(move || db.save_node_info(node_id, &info)).await {
            Ok(Ok(())) => {}
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
    mut kick_rx: oneshot::Receiver<()>,
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
            _ = &mut kick_rx => {
                reject(socket, ErrorCode::Replaced, "同一 token 有新连接，当前连接被替换").await;
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
