//! 公开只读视图：`GET /api/nodes` 与 `GET /api/live`（WebSocket）。
//!
//! 隐私：`PublicNode` 逐字段从 Hello 取值，绝不直接序列化 Hello
//! （hostname / kernel 等不得出现在任何公开输出里）。

use crate::state::{AppState, LiveEvent};
use axum::Json;
use axum::extract::State;
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::response::Response;
use minimon_common::protocol::Report;
use serde::Serialize;
use std::collections::HashMap;
use std::time::Duration;
use tokio::sync::broadcast::error::RecvError;
use tokio::time::timeout;

const SEND_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Serialize)]
pub struct PublicNode {
    pub id: i64,
    pub name: String,
    pub online: bool,
    pub sort: i64,
    pub os: Option<String>,
    pub arch: Option<String>,
    pub cpu_cores: Option<u32>,
    pub mem_total: Option<u64>,
    pub swap_total: Option<u64>,
    pub disk_total: Option<u64>,
    pub latest: Option<Report>,
}

/// 发给浏览器的实时消息。
#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum LiveMsg {
    Snapshot { nodes: Vec<PublicNode> },
    Report { node_id: i64, report: Report },
    Online { node_id: i64 },
    Offline { node_id: i64 },
}

/// 当前可见（非 hidden）节点的公开视图，按 (sort, id) 排序（缓存已按此排序）。
fn public_nodes(state: &AppState) -> Vec<PublicNode> {
    let online: HashMap<i64, Option<Report>> = state.registry().snapshot().into_iter().collect();
    state
        .nodes
        .iter()
        .filter(|n| !n.hidden)
        .map(|n| {
            // 在线时优先用本次连接的最新 Hello，否则用库里保存的。
            let info = state.registry().info(n.id).or_else(|| n.info.clone());
            let live = online.get(&n.id);
            PublicNode {
                id: n.id,
                name: n.name.clone(),
                online: live.is_some(),
                sort: n.sort,
                os: info.as_ref().map(|h| h.os.clone()),
                arch: info.as_ref().map(|h| h.arch.clone()),
                cpu_cores: info.as_ref().map(|h| h.cpu_cores),
                mem_total: info.as_ref().map(|h| h.mem_total),
                swap_total: info.as_ref().map(|h| h.swap_total),
                disk_total: info.as_ref().map(|h| h.disk_total),
                latest: live.cloned().flatten(),
            }
        })
        .collect()
}

pub async fn nodes(State(state): State<AppState>) -> Json<Vec<PublicNode>> {
    Json(public_nodes(&state))
}

pub async fn ws(State(state): State<AppState>, ws: WebSocketUpgrade) -> Response {
    // guard 在 upgrade 之前取得，停机时不会漏掉刚接入的会话。
    let guard = state.session_guard();
    ws.on_upgrade(move |socket| async move {
        let _guard = guard;
        session(state, socket).await;
    })
}

async fn send(socket: &mut WebSocket, msg: &LiveMsg) -> bool {
    let Ok(text) = serde_json::to_string(msg) else {
        return false;
    };
    matches!(
        timeout(SEND_TIMEOUT, socket.send(Message::text(text))).await,
        Ok(Ok(()))
    )
}

async fn send_snapshot(state: &AppState, socket: &mut WebSocket) -> bool {
    let nodes = public_nodes(state);
    send(socket, &LiveMsg::Snapshot { nodes }).await
}

/// 事件转消息；缓存中不存在或 hidden 的节点返回 None。
fn to_msg(state: &AppState, ev: LiveEvent) -> Option<LiveMsg> {
    let id = match &ev {
        LiveEvent::Online { node_id }
        | LiveEvent::Offline { node_id }
        | LiveEvent::Report { node_id, .. } => *node_id,
    };
    if !state.nodes.iter().any(|n| n.id == id && !n.hidden) {
        return None;
    }
    Some(match ev {
        LiveEvent::Online { node_id } => LiveMsg::Online { node_id },
        LiveEvent::Offline { node_id } => LiveMsg::Offline { node_id },
        LiveEvent::Report { node_id, report } => LiveMsg::Report { node_id, report },
    })
}

async fn session(state: AppState, mut socket: WebSocket) {
    let mut shutdown = state.shutdown_rx();
    // 先订阅再取快照：二者之间发生的事件不会丢（可能重复，前端幂等）。
    let mut rx = state.live().subscribe();
    if !send_snapshot(&state, &mut socket).await {
        return;
    }
    loop {
        tokio::select! {
            _ = async { let _ = shutdown.wait_for(|v| *v).await; } => {
                let frame = CloseFrame { code: 1000, reason: "".into() };
                let _ = timeout(SEND_TIMEOUT, socket.send(Message::Close(Some(frame)))).await;
                return;
            }
            ev = rx.recv() => match ev {
                Ok(ev) => {
                    if let Some(msg) = to_msg(&state, ev) && !send(&mut socket, &msg).await {
                        return;
                    }
                }
                Err(RecvError::Lagged(n)) => {
                    tracing::debug!(skipped = n, "live 订阅滞后，重发快照");
                    if !send_snapshot(&state, &mut socket).await {
                        return;
                    }
                }
                Err(RecvError::Closed) => return,
            },
            // 读取并丢弃客户端帧（ping 由底层自动回 pong）；关闭或出错则退出。
            msg = socket.recv() => match msg {
                None | Some(Err(_)) | Some(Ok(Message::Close(_))) => return,
                Some(Ok(_)) => {}
            },
        }
    }
}
