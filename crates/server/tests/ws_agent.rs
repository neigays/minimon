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
            Some(Ok(Message::Text(t))) => match serde_json::from_str(&t).unwrap() {
                // Welcome 之后的初始任务下发由 probe_dispatch 测试覆盖，这里忽略。
                ServerMsg::ProbeTasks { .. } => continue,
                msg => return Some(msg),
            },
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
    // report() 的 ts_ms = 1 偏差远超 10 秒，应被替换为 server 接收时间，其余字段不变。
    let latest = snap[0].1.clone().unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    assert!((latest.ts_ms - now).abs() < 2_000);
    assert_eq!(Report { ts_ms: 1, ..latest }, report());
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
                LiveEvent::Online { .. }
                | LiveEvent::Resync
                | LiveEvent::Traffic { .. }
                | LiveEvent::Probe { .. } => {}
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
    ws.send(Message::text(r#"{"type":"hello","v":3}"#))
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

/// 基于磁盘库的夹具：停机时 flush 入库，之后可直接读 metrics_raw。
async fn start_on_disk(
    dir: &tempfile::TempDir,
) -> (
    AppState,
    u16,
    String,
    i64,
    tokio::sync::oneshot::Sender<()>,
    tokio::task::JoinHandle<anyhow::Result<()>>,
) {
    let db = Arc::new(Db::open(&dir.path().join("t.db")).unwrap());
    let (node, token) = db.add_node("n1").unwrap();
    let (state, ingest) = AppState::new(db, ServerConfig::default()).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let handle = tokio::spawn(minimon_server::serve_on(
        listener,
        state.clone(),
        ingest,
        async move {
            let _ = rx.await;
        },
    ));
    (state, port, token, node.id, tx, handle)
}

#[tokio::test]
async fn skewed_report_is_stored_with_server_time() {
    let dir = tempfile::tempdir().unwrap();
    let (state, port, token, node_id, shutdown, handle) = start_on_disk(&dir).await;
    let mut live = state.live().subscribe();
    let mut ws = connect(port, &token).await;
    send(&mut ws, &AgentMsg::Hello(hello(PROTOCOL_VERSION))).await;
    assert!(matches!(
        recv(&mut ws).await,
        Some(ServerMsg::Welcome { .. })
    ));
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    let skewed = Report {
        ts_ms: now - 3_600_000,
        ..report()
    };
    send(&mut ws, &AgentMsg::Report(skewed)).await;
    assert!(matches!(
        next_event(&mut live).await,
        LiveEvent::Online { .. }
    ));
    assert!(matches!(
        next_event(&mut live).await,
        LiveEvent::Report { node_id: id, .. } if id == node_id
    ));
    let latest = state.registry().snapshot()[0].1.clone().unwrap();
    assert!(
        (latest.ts_ms - now).abs() < 2_000,
        "registry ts 应为 server 时间，实际 {}",
        latest.ts_ms
    );

    // 停机会 flush 入库队列。
    let _ = shutdown.send(());
    let _ = tokio::time::timeout(Duration::from_secs(15), handle).await;
    let conn = rusqlite::Connection::open(dir.path().join("t.db")).unwrap();
    let stored: i64 = conn
        .query_row(
            "SELECT ts_ms FROM metrics_raw WHERE node_id = ?1",
            [node_id],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        (stored - now).abs() < 2_000,
        "入库 ts 应为 server 时间，实际 {stored}"
    );
}

#[tokio::test]
async fn v1_agent_still_accepted() {
    let fx = start().await;
    let mut ws = connect(fx.port, &fx.token).await;
    send(&mut ws, &AgentMsg::Hello(hello(1))).await;
    assert!(matches!(
        recv(&mut ws).await,
        Some(ServerMsg::Welcome { .. })
    ));
}
