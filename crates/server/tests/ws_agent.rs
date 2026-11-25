use futures_util::{SinkExt, StreamExt};
use minimon_common::protocol::{AgentMsg, ErrorCode, Hello, PROTOCOL_VERSION, Report, ServerMsg};
use minimon_server::config::ServerConfig;
use minimon_server::db::Db;
use minimon_server::state::{AppState, LiveEvent};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

struct Fixture {
    state: AppState,
    port: u16,
    token: String,
    node_id: i64,
}

async fn start() -> Fixture {
    let db = Arc::new(Db::open_in_memory().unwrap());
    let (node, token) = db.add_node("n1").unwrap();
    let cfg = ServerConfig::default();
    let (state, ingest) = AppState::new(db, cfg).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(minimon_server::serve_on(
        listener,
        state.clone(),
        ingest,
        std::future::pending::<()>(),
    ));
    Fixture {
        state,
        port,
        token,
        node_id: node.id,
    }
}

async fn connect(port: u16, token: &str) -> Ws {
    let mut req = format!("ws://127.0.0.1:{port}/api/agent/ws")
        .into_client_request()
        .unwrap();
    req.headers_mut()
        .insert("Authorization", format!("Bearer {token}").parse().unwrap());
    tokio_tungstenite::connect_async(req).await.unwrap().0
}

fn hello(v: u32) -> Hello {
    Hello {
        v,
        agent_version: "t".into(),
        hostname: "h".into(),
        os: "linux".into(),
        kernel: "k".into(),
        arch: "x86_64".into(),
        cpu_model: "cpu".into(),
        cpu_cores: 2,
        mem_total: 1,
        swap_total: 0,
        disk_total: 1,
        virtualization: None,
    }
}

fn report() -> Report {
    Report {
        ts_ms: 1,
        cpu: 5.0,
        mem_used: 1,
        swap_used: 0,
        disk_used: 1,
        disk_total: 2,
        net_rx_bytes: 0,
        net_tx_bytes: 0,
        net_rx_rate: 0,
        net_tx_rate: 0,
        load1: 0.0,
        load5: 0.0,
        load15: 0.0,
        uptime_secs: 1,
        process_count: 1,
        tcp_count: 0,
        udp_count: 0,
    }
}

async fn send(ws: &mut Ws, msg: &AgentMsg) {
    ws.send(Message::text(serde_json::to_string(msg).unwrap()))
        .await
        .unwrap();
}

/// 读下一条文本帧并解析（跳过 ping/pong），流结束返回 None。
async fn recv(ws: &mut Ws) -> Option<ServerMsg> {
    loop {
        let m = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("等待服务端消息超时");
        match m {
            Some(Ok(Message::Text(t))) => return Some(serde_json::from_str(&t).unwrap()),
            Some(Ok(Message::Close(_))) | None | Some(Err(_)) => return None,
            Some(Ok(_)) => continue,
        }
    }
}

async fn next_event(rx: &mut broadcast::Receiver<LiveEvent>) -> LiveEvent {
    tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("等待 live 事件超时")
        .unwrap()
}

#[tokio::test]
async fn bad_token_gets_unauthorized() {
    let fx = start().await;
    let mut ws = connect(fx.port, "nope").await;
    assert!(matches!(
        recv(&mut ws).await,
        Some(ServerMsg::Error {
            code: ErrorCode::Unauthorized,
            ..
        })
    ));
    assert!(recv(&mut ws).await.is_none());
}

#[tokio::test]
async fn incompatible_version_rejected() {
    let fx = start().await;
    let mut ws = connect(fx.port, &fx.token).await;
    send(&mut ws, &AgentMsg::Hello(hello(99))).await;
    assert!(matches!(
        recv(&mut ws).await,
        Some(ServerMsg::Error {
            code: ErrorCode::IncompatibleVersion,
            ..
        })
    ));
    assert!(recv(&mut ws).await.is_none());
    assert!(fx.state.registry().snapshot().is_empty());
}

#[tokio::test]
async fn first_message_not_hello_closes() {
    let fx = start().await;
    let mut ws = connect(fx.port, &fx.token).await;
    send(&mut ws, &AgentMsg::Report(report())).await;
    assert!(recv(&mut ws).await.is_none());
    assert!(fx.state.registry().snapshot().is_empty());
}

#[tokio::test]
async fn hello_gets_welcome_and_report_updates_registry() {
    let fx = start().await;
    let mut live = fx.state.live().subscribe();
    let mut ws = connect(fx.port, &fx.token).await;
    send(&mut ws, &AgentMsg::Hello(hello(PROTOCOL_VERSION))).await;
    assert_eq!(
        recv(&mut ws).await,
        Some(ServerMsg::Welcome { interval_secs: 3 })
    );
    assert!(
        matches!(next_event(&mut live).await, LiveEvent::Online { node_id } if node_id == fx.node_id)
    );
    send(&mut ws, &AgentMsg::Report(report())).await;
    assert!(matches!(
        next_event(&mut live).await,
        LiveEvent::Report { node_id, .. } if node_id == fx.node_id
    ));
    let snap = fx.state.registry().snapshot();
    assert_eq!(snap.len(), 1);
    assert_eq!(snap[0].0, fx.node_id);
    assert_eq!(snap[0].1, Some(report()));
}

#[tokio::test]
async fn second_connection_kicks_first_without_offline() {
    let fx = start().await;
    let mut live = fx.state.live().subscribe();
    let mut a = connect(fx.port, &fx.token).await;
    send(&mut a, &AgentMsg::Hello(hello(PROTOCOL_VERSION))).await;
    assert!(matches!(
        recv(&mut a).await,
        Some(ServerMsg::Welcome { .. })
    ));
    let mut b = connect(fx.port, &fx.token).await;
    send(&mut b, &AgentMsg::Hello(hello(PROTOCOL_VERSION))).await;
    assert!(matches!(
        recv(&mut b).await,
        Some(ServerMsg::Welcome { .. })
    ));
    assert!(matches!(
        recv(&mut a).await,
        Some(ServerMsg::Error {
            code: ErrorCode::Replaced,
            ..
        })
    ));
    assert!(recv(&mut a).await.is_none());
    // B 仍然可用：连续两次上报都必须被接受（若旧连接误删新会话，上报会被拒绝而超时）。
    // 收集 kick 之后直到 B 的 Report 之间的所有事件，其中不得有 Offline。
    for _ in 0..2 {
        send(&mut b, &AgentMsg::Report(report())).await;
        loop {
            match next_event(&mut live).await {
                LiveEvent::Offline { .. } => panic!("不应出现 Offline"),
                LiveEvent::Report { .. } => break,
                LiveEvent::Online { .. } => {}
            }
        }
    }
    let snap = fx.state.registry().snapshot();
    assert_eq!(snap.len(), 1);
    assert!(snap[0].1.is_some());
}

#[tokio::test]
async fn disconnect_emits_offline() {
    let fx = start().await;
    let mut live = fx.state.live().subscribe();
    let mut ws = connect(fx.port, &fx.token).await;
    send(&mut ws, &AgentMsg::Hello(hello(PROTOCOL_VERSION))).await;
    assert!(matches!(
        recv(&mut ws).await,
        Some(ServerMsg::Welcome { .. })
    ));
    assert!(matches!(
        next_event(&mut live).await,
        LiveEvent::Online { .. }
    ));
    ws.close(None).await.unwrap();
    assert!(matches!(
        next_event(&mut live).await,
        LiveEvent::Offline { node_id } if node_id == fx.node_id
    ));
    assert!(fx.state.registry().snapshot().is_empty());
}

#[tokio::test]
async fn incompatible_version_with_unparseable_hello_shape() {
    let fx = start().await;
    let mut ws = connect(fx.port, &fx.token).await;
    ws.send(Message::text(r#"{"type":"hello","v":2}"#))
        .await
        .unwrap();
    assert!(matches!(
        recv(&mut ws).await,
        Some(ServerMsg::Error {
            code: ErrorCode::IncompatibleVersion,
            ..
        })
    ));
    assert!(recv(&mut ws).await.is_none());
}
