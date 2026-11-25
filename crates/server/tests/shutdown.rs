use futures_util::{SinkExt, StreamExt};
use minimon_common::protocol::{AgentMsg, Hello, PROTOCOL_VERSION, Report, ServerMsg};
use minimon_server::config::ServerConfig;
use minimon_server::db::Db;
use minimon_server::state::AppState;
use std::sync::Arc;
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

fn hello() -> Hello {
    Hello {
        v: PROTOCOL_VERSION,
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
        ts_ms: 42,
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

#[tokio::test]
async fn shutdown_with_connected_agent_closes_it_and_flushes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.db");
    let db = Arc::new(Db::open(&path).unwrap());
    let (node, token) = db.add_node("n1").unwrap();
    let (state, ingest) = AppState::new(db, ServerConfig::default());
    let probe = state.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(minimon_server::serve_on(listener, state, ingest, async {
        let _ = stop_rx.await;
    }));

    let mut req = format!("ws://127.0.0.1:{port}/api/agent/ws")
        .into_client_request()
        .unwrap();
    req.headers_mut()
        .insert("Authorization", format!("Bearer {token}").parse().unwrap());
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    let send = |m: AgentMsg| Message::text(serde_json::to_string(&m).unwrap());
    ws.send(send(AgentMsg::Hello(hello()))).await.unwrap();
    // 等 welcome，确保会话已建立
    loop {
        let m = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        if let Message::Text(t) = m {
            assert!(matches!(
                serde_json::from_str::<ServerMsg>(&t).unwrap(),
                ServerMsg::Welcome { .. }
            ));
            break;
        }
    }
    ws.send(send(AgentMsg::Report(report()))).await.unwrap();
    // 轮询等待服务端处理该上报后再停机
    tokio::time::timeout(Duration::from_secs(5), async {
        while !probe.registry().snapshot().iter().any(|(_, r)| r.is_some()) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("服务端未处理上报");

    stop_tx.send(()).unwrap();
    // agent 保持连接：应收到 Close 或流结束
    let closed = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match ws.next().await {
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => return,
                Some(Ok(_)) => continue,
            }
        }
    })
    .await;
    assert!(closed.is_ok(), "agent 未收到 Close");
    let res = tokio::time::timeout(Duration::from_secs(10), server)
        .await
        .expect("serve_on 未在时限内返回")
        .unwrap();
    assert!(res.is_ok());

    let c = rusqlite::Connection::open(&path).unwrap();
    let n: i64 = c
        .query_row(
            "SELECT count(*) FROM metrics_raw WHERE node_id=?1 AND ts_ms=42",
            [node.id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 1);
}
