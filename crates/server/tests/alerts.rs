//! 告警运行时：评估循环接入 server 生命周期（离线事件、宽限期内重连、删除节点清状态）。

mod common;

use common::{Fixture, http, login, start_at};
use futures_util::{SinkExt, StreamExt};
use minimon_common::protocol::{AgentMsg, Hello, PROTOCOL_VERSION, Report, ServerMsg};
use minimon_server::alert::model::{AlertEvent, EventKind, SendStatus, StateKind};
use minimon_server::db::Db;
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

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
        mem_total: 100,
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

/// 一个节点，离线宽限期 `grace` 秒，评估间隔 200 ms。返回 (Fixture, node_id, token)。
async fn boot(grace: i64) -> (Fixture, i64, String) {
    let out = std::sync::Arc::new(std::sync::Mutex::new((0, String::new())));
    let out2 = out.clone();
    let f = start_at(
        tempfile::tempdir().unwrap(),
        move |db: &Db| {
            db.set_setting("alert.offline_grace_secs", &grace.to_string())
                .unwrap();
            let (n, token) = db.add_node("n1").unwrap();
            *out2.lock().unwrap() = (n.id, token);
        },
        |c| c.alert_tick_ms = 200,
    )
    .await;
    let (id, token) = out.lock().unwrap().clone();
    (f, id, token)
}

/// 连接、握手并上报一次，等到 registry 里有上报才返回。
async fn connect(f: &Fixture, node_id: i64, token: &str) -> Ws {
    let mut req = format!("ws://127.0.0.1:{}/api/agent/ws", f.port)
        .into_client_request()
        .unwrap();
    req.headers_mut()
        .insert("Authorization", format!("Bearer {token}").parse().unwrap());
    let mut ws = tokio_tungstenite::connect_async(req).await.unwrap().0;
    let send = |m: AgentMsg| Message::text(serde_json::to_string(&m).unwrap());
    ws.send(send(AgentMsg::Hello(hello()))).await.unwrap();
    loop {
        let m = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("等待 Welcome 超时")
            .unwrap()
            .unwrap();
        if let Message::Text(t) = m
            && let ServerMsg::Welcome { .. } = serde_json::from_str(&t).unwrap()
        {
            break;
        }
    }
    ws.send(send(AgentMsg::Report(report()))).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !f
            .state
            .registry()
            .snapshot()
            .iter()
            .any(|(id, r)| *id == node_id && r.is_some())
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("等待上报进入在线表超时");
    ws
}

async fn disconnect(f: &Fixture, node_id: i64, mut ws: Ws) {
    let _ = ws.close(None).await;
    drop(ws);
    tokio::time::timeout(Duration::from_secs(5), async {
        while f
            .state
            .registry()
            .snapshot()
            .iter()
            .any(|(id, _)| *id == node_id)
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("等待节点离线超时");
}

fn events(f: &Fixture) -> Vec<AlertEvent> {
    Db::open(&f.db_path)
        .unwrap()
        .list_alert_events(None, 100)
        .unwrap()
}

/// 最多轮询 `secs` 秒，等出现离线 firing 事件且发送状态已落定。
async fn wait_offline_event(f: &Fixture, secs: u64) -> Option<AlertEvent> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);
    while tokio::time::Instant::now() < deadline {
        if let Some(e) = events(f).into_iter().find(|e| {
            e.kind == EventKind::Firing
                && e.title.starts_with("【离线】")
                && e.sent != SendStatus::Pending
        }) {
            return Some(e);
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    None
}

#[tokio::test]
async fn offline_event_after_grace() {
    let (f, id, token) = boot(1).await;
    let ws = connect(&f, id, &token).await;
    disconnect(&f, id, ws).await;
    // 合并窗口 10 秒，事件先记 pending，发送状态在窗口结束后落定。
    let ev = wait_offline_event(&f, 20)
        .await
        .unwrap_or_else(|| panic!("未产生离线事件: {:?}", events(&f)));
    assert_eq!(ev.node_id, Some(id));
    assert_eq!(ev.node_name.as_deref(), Some("n1"));
    assert_eq!(ev.sent, SendStatus::Skipped, "未配 SendKey 应记为 skipped");
    let active = f.state.alert().active();
    assert!(
        active
            .iter()
            .any(|r| r.key.kind == StateKind::Offline && r.key.node_id == id && r.firing),
        "{active:?}"
    );
    f.stop().await;
}

#[tokio::test]
async fn reconnect_within_grace_no_event() {
    let (f, id, token) = boot(30).await;
    let ws = connect(&f, id, &token).await;
    disconnect(&f, id, ws).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let _ws = connect(&f, id, &token).await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(events(&f).is_empty(), "{:?}", events(&f));
    assert!(f.state.alert().active().is_empty());
    f.stop().await;
}

#[tokio::test]
async fn deleted_node_state_forgotten() {
    let (f, id, token) = boot(1).await;
    let ws = connect(&f, id, &token).await;
    disconnect(&f, id, ws).await;
    let fired = tokio::time::timeout(Duration::from_secs(10), async {
        while !f.state.alert().active().iter().any(|r| r.key.node_id == id) {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await;
    assert!(fired.is_ok(), "离线状态未进入 firing");

    let cookie = login(f.port).await;
    let (code, _, body) = http(
        f.port,
        "DELETE",
        &format!("/api/admin/nodes/{id}"),
        Some(&cookie),
        Some("{}"),
    )
    .await;
    assert_eq!(code, 204, "{body}");
    assert!(f.state.alert().active().is_empty());
    // 再跑几轮评估，状态不会被旧快照写回（内存与库）。
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert!(f.state.alert().active().is_empty());
    let states = Db::open(&f.db_path).unwrap().load_alert_states().unwrap();
    assert!(states.iter().all(|s| s.key.node_id != id), "{states:?}");
    f.stop().await;
}
