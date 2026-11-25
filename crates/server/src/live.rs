//! `GET /api/nodes` 与 `GET /api/live`（WebSocket）：按身份输出视图（见 `views`）。

use crate::auth::Viewer;
use crate::error::ApiError;
use crate::probe::CardProbe;
use crate::state::{AppState, LiveEvent};
use crate::views::{self, NodeList};
use axum::Json;
use axum::extract::State;
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::http::{HeaderMap, header};
use axum::response::{IntoResponse, Response};
use minimon_common::protocol::Report;
use serde::Serialize;
use std::time::Duration;
use tokio::sync::broadcast::error::RecvError;
use tokio::time::timeout;

/// 公开端点不接受有意义的客户端数据，帧上限压到 4 KiB 防内存滥用。
const LIVE_MAX_MSG: usize = 4 << 10;
const SEND_TIMEOUT: Duration = Duration::from_secs(10);

/// 发给浏览器的实时消息。
#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum LiveMsg {
    Snapshot {
        nodes: NodeList,
    },
    Report {
        node_id: i64,
        report: Report,
    },
    Online {
        node_id: i64,
    },
    Offline {
        node_id: i64,
    },
    Probe {
        node_id: i64,
        #[serde(flatten)]
        probe: CardProbe,
    },
    Traffic {
        node_id: i64,
        used: i64,
        rx: i64,
        tx: i64,
        quota: Option<i64>,
        period_start_ms: i64,
        period_end_ms: i64,
    },
}

/// 整站登录开启时访客不得读取任何节点数据。
pub(crate) fn gate(state: &AppState, viewer: &Viewer) -> Result<(), ApiError> {
    if matches!(viewer, Viewer::Guest) && state.require_login() {
        return Err(ApiError::unauthorized("请先登录"));
    }
    Ok(())
}

pub async fn nodes(State(state): State<AppState>, viewer: Viewer) -> Result<Response, ApiError> {
    gate(&state, &viewer)?;
    Ok(match viewer {
        Viewer::Admin { .. } => Json(views::admin_nodes(&state)).into_response(),
        Viewer::Guest => Json(views::public_nodes(&state)).into_response(),
    })
}

/// 请求带 Origin 但与 Host（或 public_url）不一致时，视为跨站发起的连接。
fn origin_allowed(state: &AppState, headers: &HeaderMap) -> bool {
    let Some(origin) = headers.get(header::ORIGIN) else {
        return true; // 非浏览器客户端不带 Origin，仍需 cookie
    };
    let Some(origin_host) = origin
        .to_str()
        .ok()
        .and_then(|o| o.split_once("://").map(|(_, h)| h))
        .map(|h| h.trim_end_matches('/').to_ascii_lowercase())
    else {
        return false;
    };
    let host_ok = headers
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .is_some_and(|h| h.eq_ignore_ascii_case(&origin_host));
    let public_ok = state.cfg.public_url.as_deref().is_some_and(|u| {
        u.split_once("://")
            .is_some_and(|(_, h)| h.trim_end_matches('/').eq_ignore_ascii_case(&origin_host))
    });
    host_ok || public_ok
}

pub async fn ws(
    State(state): State<AppState>,
    viewer: Viewer,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Result<Response, ApiError> {
    // 跨站 Origin 一律降级为访客视图，防止跨站 WebSocket 劫持读取管理数据。
    let viewer = if matches!(viewer, Viewer::Admin { .. }) && !origin_allowed(&state, &headers) {
        tracing::debug!("/api/live Origin 与 Host 不一致，按访客处理");
        Viewer::Guest
    } else {
        viewer
    };
    // 升级前拒绝：访客在整站登录开启时直接 401。
    gate(&state, &viewer)?;
    let admin = matches!(viewer, Viewer::Admin { .. });
    // guard 在 upgrade 之前取得，停机时不会漏掉刚接入的会话。
    let guard = state.session_guard();
    Ok(ws
        .max_message_size(LIVE_MAX_MSG)
        .max_frame_size(LIVE_MAX_MSG)
        .on_upgrade(move |socket| async move {
            let _guard = guard;
            session(state, admin, socket).await;
        }))
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

async fn send_snapshot(state: &AppState, admin: bool, socket: &mut WebSocket) -> bool {
    let nodes = views::nodes_for(state, admin);
    send(socket, &LiveMsg::Snapshot { nodes }).await
}

/// 事件转消息；缓存中不存在的节点、访客看不到的隐藏节点返回 None。
fn to_msg(state: &AppState, admin: bool, ev: LiveEvent) -> Option<LiveMsg> {
    let id = match &ev {
        LiveEvent::Online { node_id }
        | LiveEvent::Offline { node_id }
        | LiveEvent::Report { node_id, .. }
        | LiveEvent::Probe { node_id, .. } => *node_id,
        LiveEvent::Traffic { node_id, .. } => {
            // 流量字段仅管理员可见：访客连接一律丢弃。
            if !admin {
                return None;
            }
            *node_id
        }
        LiveEvent::Resync => return None,
    };
    if !state
        .nodes()
        .iter()
        .any(|n| n.id == id && (admin || !n.hidden))
    {
        return None;
    }
    Some(match ev {
        LiveEvent::Online { node_id } => LiveMsg::Online { node_id },
        LiveEvent::Offline { node_id } => LiveMsg::Offline { node_id },
        LiveEvent::Report { node_id, report } => LiveMsg::Report { node_id, report },
        LiveEvent::Probe { node_id, probe } => LiveMsg::Probe { node_id, probe },
        LiveEvent::Traffic { node_id, usage } => LiveMsg::Traffic {
            node_id,
            used: usage.used,
            rx: usage.rx,
            tx: usage.tx,
            quota: usage.quota,
            period_start_ms: usage.period_start_ms,
            period_end_ms: usage.period_end_ms,
        },
        LiveEvent::Resync => return None,
    })
}

async fn close_normal(socket: &mut WebSocket) {
    let frame = CloseFrame {
        code: 1000,
        reason: "".into(),
    };
    let _ = timeout(SEND_TIMEOUT, socket.send(Message::Close(Some(frame)))).await;
}

/// 重新同步：访客在整站登录开启后被关闭，其余重发当前身份的快照。返回会话是否继续。
async fn resync(state: &AppState, admin: bool, socket: &mut WebSocket) -> bool {
    if !admin && state.require_login() {
        close_normal(socket).await;
        return false;
    }
    send_snapshot(state, admin, socket).await
}

async fn session(state: AppState, admin: bool, mut socket: WebSocket) {
    let mut shutdown = state.shutdown_rx();
    // 先订阅再取快照：二者之间发生的事件不会丢（可能重复，前端幂等）。
    let mut rx = state.live().subscribe();
    if !resync(&state, admin, &mut socket).await {
        return;
    }
    loop {
        tokio::select! {
            _ = async { let _ = shutdown.wait_for(|v| *v).await; } => {
                close_normal(&mut socket).await;
                return;
            }
            ev = rx.recv() => match ev {
                Ok(LiveEvent::Resync) => {
                    if !resync(&state, admin, &mut socket).await {
                        return;
                    }
                }
                Ok(ev) => {
                    if let Some(msg) = to_msg(&state, admin, ev) && !send(&mut socket, &msg).await {
                        return;
                    }
                }
                Err(RecvError::Lagged(n)) => {
                    tracing::debug!(skipped = n, "live 订阅滞后，重发快照");
                    if !resync(&state, admin, &mut socket).await {
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
