use futures_util::{SinkExt, StreamExt};
use minimon_common::protocol::{AgentMsg, Hello, PROTOCOL_VERSION, Report};
use minimon_server::config::ServerConfig;
use minimon_server::db::Db;
use minimon_server::state::AppState;
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

struct Fixture {
    port: u16,
    tokens: Vec<(i64, String)>,
    _dir: tempfile::TempDir,
}

/// 先用 `prepare` 直接改库，再构造 AppState 并启动。
async fn start(nodes: usize, prepare: impl FnOnce(&rusqlite::Connection)) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.db");
    let db = Db::open(&path).unwrap();
    let tokens: Vec<_> = (0..nodes)
        .map(|i| {
            let (n, t) = db.add_node(&format!("n{i}")).unwrap();
            (n.id, t)
        })
        .collect();
    drop(db);
    {
        let c = rusqlite::Connection::open(&path).unwrap();
        prepare(&c);
    }
    let db = Arc::new(Db::open(&path).unwrap());
    let (state, ingest) = AppState::new(db, ServerConfig::default()).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(minimon_server::serve_on(
        listener,
        state,
        ingest,
        std::future::pending::<()>(),
    ));
    Fixture {
        port,
        tokens,
        _dir: dir,
    }
}

async fn http_get(port: u16, path: &str) -> (u16, String) {
    let mut s = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .unwrap();
    let req = format!("GET {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n");
    s.write_all(req.as_bytes()).await.unwrap();
    let mut buf = Vec::new();
    s.read_to_end(&mut buf).await.unwrap();
    let text = String::from_utf8(buf).unwrap();
    let (head, body) = text.split_once("\r\n\r\n").unwrap();
    let status = head.split(' ').nth(1).unwrap().parse().unwrap();
    (status, body.to_owned())
}

fn hello() -> Hello {
    Hello {
        v: PROTOCOL_VERSION,
        agent_version: "t".into(),
        hostname: "secret-host".into(),
        os: "linux".into(),
        kernel: "secret-kernel".into(),
        arch: "x86_64".into(),
        cpu_model: "cpu".into(),
        cpu_cores: 2,
        mem_total: 1000,
        swap_total: 0,
        disk_total: 5000,
        virtualization: None,
    }
}

fn report(cpu: f32) -> Report {
    Report {
        ts_ms: 1,
        cpu,
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

async fn agent(port: u16, token: &str) -> Ws {
    let mut req = format!("ws://127.0.0.1:{port}/api/agent/ws")
        .into_client_request()
        .unwrap();
    req.headers_mut()
        .insert("Authorization", format!("Bearer {token}").parse().unwrap());
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    send(&mut ws, &AgentMsg::Hello(hello())).await;
    let welcome = next_json(&mut ws).await;
    assert_eq!(welcome["type"], "welcome");
    ws
}

async fn send(ws: &mut Ws, m: &AgentMsg) {
    ws.send(Message::text(serde_json::to_string(m).unwrap()))
        .await
        .unwrap();
}

async fn next_json(ws: &mut Ws) -> Value {
    loop {
        let m = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("等待消息超时")
            .unwrap()
            .unwrap();
        if let Message::Text(t) = m {
            return serde_json::from_str(&t).unwrap();
        }
    }
}

async fn live(port: u16) -> Ws {
    tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}/api/live"))
        .await
        .unwrap()
        .0
}

#[tokio::test]
async fn nodes_empty_is_empty_array() {
    let f = start(0, |_| {}).await;
    let (status, body) = http_get(f.port, "/api/nodes").await;
    assert_eq!(status, 200);
    assert_eq!(body, "[]");
}

#[tokio::test]
async fn public_node_has_no_hostname() {
    let f = start(1, |_| {}).await;
    let mut a = agent(f.port, &f.tokens[0].1).await;
    send(&mut a, &AgentMsg::Report(report(42.5))).await;
    let mut body = String::new();
    for _ in 0..50 {
        body = http_get(f.port, "/api/nodes").await.1;
        if body.contains("\"cpu\"") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    for banned in [
        "secret-host",
        "secret-kernel",
        "hostname",
        "kernel",
        "token",
    ] {
        assert!(!body.contains(banned), "泄露 {banned}: {body}");
    }
    let v: Value = serde_json::from_str(&body).unwrap();
    let n = &v[0];
    assert_eq!(n["online"], true);
    assert_eq!(n["latest"]["cpu"], 42.5);
    assert_eq!(n["os"], "linux");
    assert_eq!(n["cpu_cores"], 2);
    assert_eq!(n["mem_total"], 1000);
}

#[tokio::test]
async fn disconnected_node_shows_offline() {
    let f = start(1, |_| {}).await;
    let mut a = agent(f.port, &f.tokens[0].1).await;
    send(&mut a, &AgentMsg::Report(report(5.0))).await;
    for _ in 0..50 {
        if http_get(f.port, "/api/nodes").await.1.contains("\"cpu\"") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    a.close(None).await.unwrap();
    drop(a);
    let mut body = String::new();
    for _ in 0..50 {
        body = http_get(f.port, "/api/nodes").await.1;
        if body.contains("\"online\":false") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(body.contains("\"online\":false"), "{body}");
    // 最后已知数据：离线后保留 latest（M2 语义）。
    let v: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v[0]["latest"]["cpu"], 5.0, "{body}");
}

#[tokio::test]
async fn offline_node_info_loaded_from_db() {
    let json = serde_json::to_string(&hello()).unwrap();
    let f = start(1, |c| {
        c.execute("UPDATE nodes SET info_json = ?1", [json])
            .unwrap();
    })
    .await;
    let body = http_get(f.port, "/api/nodes").await.1;
    let v: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v[0]["online"], false);
    assert_eq!(v[0]["arch"], "x86_64");
    assert_eq!(v[0]["disk_total"], 5000);
    assert!(!body.contains("secret"), "{body}");
}

#[tokio::test]
async fn live_sends_snapshot_then_report() {
    let f = start(1, |_| {}).await;
    let mut l = live(f.port).await;
    let snap = next_json(&mut l).await;
    assert_eq!(snap["type"], "snapshot");
    assert_eq!(snap["nodes"].as_array().unwrap().len(), 1);
    let mut a = agent(f.port, &f.tokens[0].1).await;
    let id = f.tokens[0].0;
    let online = next_json(&mut l).await;
    assert_eq!(online["type"], "online");
    assert_eq!(online["node_id"], id);
    send(&mut a, &AgentMsg::Report(report(7.0))).await;
    let r = next_json(&mut l).await;
    assert_eq!(r["type"], "report");
    assert_eq!(r["node_id"], id);
    assert_eq!(r["report"]["cpu"], 7.0);
    a.close(None).await.unwrap();
    let off = next_json(&mut l).await;
    assert_eq!(off["type"], "offline");
    assert_eq!(off["node_id"], id);
}

#[tokio::test]
async fn hidden_node_excluded() {
    let f = start(2, |c| {
        c.execute("UPDATE nodes SET hidden = 1 WHERE name = 'n0'", [])
            .unwrap();
    })
    .await;
    let (hidden_id, hidden_token) = f.tokens[0].clone();
    let (shown_id, shown_token) = f.tokens[1].clone();
    let body = http_get(f.port, "/api/nodes").await.1;
    let v: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v.as_array().unwrap().len(), 1);
    assert_eq!(v[0]["id"], shown_id);

    let mut l = live(f.port).await;
    let snap = next_json(&mut l).await;
    assert_eq!(snap["nodes"].as_array().unwrap().len(), 1);
    let mut h = agent(f.port, &hidden_token).await;
    send(&mut h, &AgentMsg::Report(report(1.0))).await;
    let _s = agent(f.port, &shown_token).await;
    // 第一条事件必须来自可见节点；隐藏节点的 online/report 都不得出现。
    let ev = next_json(&mut l).await;
    assert_eq!(ev["node_id"], shown_id, "{ev}");
    assert_ne!(ev["node_id"], hidden_id);
    // 隐藏节点在线也不出现在 /api/nodes
    let body = http_get(f.port, "/api/nodes").await.1;
    assert!(!body.contains(&format!("\"id\":{hidden_id},")), "{body}");
}

fn keys(v: &Value) -> Vec<String> {
    let mut k: Vec<String> = v.as_object().unwrap().keys().cloned().collect();
    k.sort();
    k
}

fn sorted(v: &[&str]) -> Vec<String> {
    let mut k: Vec<String> = v.iter().map(|s| s.to_string()).collect();
    k.sort();
    k
}

/// 公开输出的键白名单：新增字段（流量、额度等）必须显式改这里，防止无意泄露。
#[tokio::test]
async fn public_output_key_allowlist() {
    let node_keys = sorted(&[
        "id",
        "name",
        "online",
        "sort",
        "group",
        "region",
        "os",
        "arch",
        "cpu_model",
        "cpu_cores",
        "virtualization",
        "mem_total",
        "swap_total",
        "disk_total",
        "last_seen_ms",
        "latest",
        "probes",
    ]);
    let report_keys = sorted(&[
        "ts_ms",
        "cpu",
        "mem_used",
        "swap_used",
        "disk_used",
        "disk_total",
        "net_rx_bytes",
        "net_tx_bytes",
        "net_rx_rate",
        "net_tx_rate",
        "load1",
        "load5",
        "load15",
        "uptime_secs",
        "process_count",
        "tcp_count",
        "udp_count",
    ]);
    let f = start(1, |_| {}).await;
    let mut l = live(f.port).await;
    let snap = next_json(&mut l).await;
    assert_eq!(keys(&snap["nodes"][0]), node_keys);
    let mut a = agent(f.port, &f.tokens[0].1).await;
    let online = next_json(&mut l).await;
    assert_eq!(keys(&online), sorted(&["type", "node_id"]));
    assert_eq!(online["type"], "online");
    send(&mut a, &AgentMsg::Report(report(3.0))).await;
    let r = next_json(&mut l).await;
    assert_eq!(keys(&r), sorted(&["type", "node_id", "report"]));
    assert_eq!(keys(&r["report"]), report_keys);
    let mut v = Value::Null;
    for _ in 0..50 {
        let body = http_get(f.port, "/api/nodes").await.1;
        v = serde_json::from_str(&body).unwrap();
        if !v[0]["latest"].is_null() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(keys(&v[0]), node_keys);
    assert_eq!(keys(&v[0]["latest"]), report_keys);
    a.close(None).await.unwrap();
    drop(a);
    let off = next_json(&mut l).await;
    assert_eq!(off["type"], "offline");
    assert_eq!(keys(&off), sorted(&["type", "node_id"]));
}

#[tokio::test]
async fn node_added_while_running_appears() {
    let f = start(0, |_| {}).await;
    let db = Db::open(&f._dir.path().join("t.db")).unwrap();
    let (node, token) = db.add_node("late").unwrap();
    let mut l = live(f.port).await;
    assert_eq!(
        next_json(&mut l).await["nodes"].as_array().unwrap().len(),
        0
    );
    let _a = agent(f.port, &token).await;
    let mut v = Value::Null;
    for _ in 0..50 {
        let body = http_get(f.port, "/api/nodes").await.1;
        v = serde_json::from_str(&body).unwrap();
        if v.as_array().is_some_and(|a| !a.is_empty()) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(v[0]["id"], node.id);
    assert_eq!(v[0]["online"], true);
    // live 事件也能通过过滤
    let ev = next_json(&mut l).await;
    assert_eq!(ev["type"], "online");
    assert_eq!(ev["node_id"], node.id);
}

#[tokio::test]
async fn oversized_live_frame_closes_connection() {
    let f = start(0, |_| {}).await;
    let mut l = live(f.port).await;
    let _snap = next_json(&mut l).await;
    let _ = l.send(Message::binary(vec![0u8; 1 << 20])).await;
    let closed = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match l.next().await {
                None | Some(Err(_)) | Some(Ok(Message::Close(_))) => return,
                Some(Ok(_)) => {}
            }
        }
    })
    .await;
    assert!(closed.is_ok(), "超大帧应使连接关闭");
    // server 仍可用
    let (status, _) = http_get(f.port, "/api/nodes").await;
    assert_eq!(status, 200);
}
