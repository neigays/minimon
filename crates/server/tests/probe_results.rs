//! 探测结果：入库、卡片最新值、live 推送与目标隐私。

mod common;

use common::{Fixture, http, login, start};
use futures_util::{SinkExt, StreamExt};
use minimon_common::protocol::{AgentMsg, Hello, PROTOCOL_VERSION, ProbeResult, ServerMsg};
use serde_json::{Value, json};
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

const SECRET: &str = "secret-target.example.com";

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

async fn read_server(ws: &mut Ws) -> ServerMsg {
    loop {
        match tokio::time::timeout(Duration::from_secs(5), ws.next()).await {
            Ok(Some(Ok(Message::Text(t)))) => return serde_json::from_str(&t).unwrap(),
            Ok(Some(Ok(_))) => continue,
            other => panic!("等待消息失败: {other:?}"),
        }
    }
}

/// 接入、读完 Welcome 与首个 ProbeTasks。
async fn agent(port: u16, token: &str) -> Ws {
    let mut req = format!("ws://127.0.0.1:{port}/api/agent/ws")
        .into_client_request()
        .unwrap();
    req.headers_mut()
        .insert("Authorization", format!("Bearer {token}").parse().unwrap());
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    let text = serde_json::to_string(&AgentMsg::Hello(hello())).unwrap();
    ws.send(Message::text(text)).await.unwrap();
    assert!(matches!(
        read_server(&mut ws).await,
        ServerMsg::Welcome { .. }
    ));
    assert!(matches!(
        read_server(&mut ws).await,
        ServerMsg::ProbeTasks { .. }
    ));
    ws
}

async fn send_result(ws: &mut Ws, r: ProbeResult) {
    let text = serde_json::to_string(&AgentMsg::ProbeResult(r)).unwrap();
    ws.send(Message::text(text)).await.unwrap();
}

fn result(task_id: i64, error: Option<&str>) -> ProbeResult {
    ProbeResult {
        task_id,
        ts_ms: now(),
        sent: 5,
        lost: 1,
        avg_ms: Some(32.5),
        min_ms: Some(30.0),
        max_ms: Some(40.0),
        error: error.map(str::to_owned),
    }
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

async fn api(f: &Fixture, ck: &str, method: &str, path: &str, body: Option<Value>) -> Value {
    let b = body.map(|v| v.to_string());
    let (c, _, text) = http(f.port, method, path, Some(ck), b.as_deref()).await;
    assert!(c == 200 || c == 201 || c == 204, "{c} {text}");
    serde_json::from_str(&text).unwrap_or(Value::Null)
}

async fn add_node(f: &Fixture, ck: &str) -> (i64, String) {
    let v = api(
        f,
        ck,
        "POST",
        "/api/admin/nodes",
        Some(json!({"name": "a"})),
    )
    .await;
    (
        v["node"]["id"].as_i64().unwrap(),
        v["token"].as_str().unwrap().to_string(),
    )
}

async fn add_task(f: &Fixture, ck: &str, node: i64, all: bool, card: bool) -> i64 {
    let v = api(
        f,
        ck,
        "POST",
        "/api/admin/probes",
        Some(json!({"name": "电信", "kind": "icmp", "target": SECRET,
                    "all_nodes": all, "node_ids": [node], "show_on_card": card})),
    )
    .await;
    v["id"].as_i64().unwrap()
}

async fn guest_nodes(f: &Fixture) -> String {
    http(f.port, "GET", "/api/nodes", None, None).await.2
}

async fn wait_guest(f: &Fixture, ok: impl Fn(&Value) -> bool) -> Value {
    for _ in 0..100 {
        let v: Value = serde_json::from_str(&guest_nodes(f).await).unwrap();
        if ok(&v) {
            return v;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("等待条件超时");
}

async fn live(port: u16) -> Ws {
    tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}/api/live"))
        .await
        .unwrap()
        .0
}

async fn next_json(ws: &mut Ws) -> Value {
    next_json_within(ws, Duration::from_secs(5)).await.unwrap()
}

async fn next_json_within(ws: &mut Ws, d: Duration) -> Option<Value> {
    loop {
        match tokio::time::timeout(d, ws.next()).await {
            Ok(Some(Ok(Message::Text(t)))) => return Some(serde_json::from_str(&t).unwrap()),
            Ok(Some(Ok(_))) => continue,
            _ => return None,
        }
    }
}

fn raw_count(path: &std::path::Path) -> i64 {
    rusqlite::Connection::open(path)
        .unwrap()
        .query_row("SELECT count(*) FROM probe_raw", [], |r| r.get(0))
        .unwrap()
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

#[tokio::test]
async fn result_flows_to_card_and_live() {
    let f = start(|_| {}).await;
    let ck = login(f.port).await;
    let (_, token) = add_node(&f, &ck).await;
    let task = add_task(&f, &ck, 0, true, true).await;
    let mut g = live(f.port).await;
    assert_eq!(next_json(&mut g).await["type"], "snapshot");
    let mut ws = agent(f.port, &token).await;
    // 无结果时也列出任务。
    let v = wait_guest(&f, |v| {
        v[0]["probes"].as_array().is_some_and(|a| a.len() == 1)
    })
    .await;
    assert!(v[0]["probes"][0]["avg_ms"].is_null());

    send_result(&mut ws, result(task, None)).await;
    let v = wait_guest(&f, |v| v[0]["probes"][0]["avg_ms"] == 32.5).await;
    assert_eq!(v[0]["probes"][0]["loss"], 0.2);
    assert_eq!(v[0]["probes"][0]["name"], "电信");
    loop {
        let m = next_json(&mut g).await;
        if m["type"] == "probe" {
            assert_eq!(m["task_id"], task);
            assert_eq!(m["avg_ms"], 32.5);
            assert_eq!(m["loss"], 0.2);
            break;
        }
    }
    // 入库。
    let path = f.db_path.clone();
    let _dir = f.stop().await;
    assert_eq!(raw_count(&path), 1);
}

#[tokio::test]
async fn foreign_task_result_dropped() {
    let f = start(|_| {}).await;
    let ck = login(f.port).await;
    let (_, token) = add_node(&f, &ck).await;
    // 任务只属于另一个节点；也发送完全未知的 id。
    let (other, _) = add_node(&f, &ck).await;
    let foreign = add_task(&f, &ck, other, false, true).await;
    let mut ws = agent(f.port, &token).await;
    send_result(&mut ws, result(foreign, None)).await;
    send_result(&mut ws, result(424242, None)).await;
    send_result(
        &mut ws,
        ProbeResult {
            sent: 0,
            ..result(foreign, None)
        },
    )
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let v: Value = serde_json::from_str(&guest_nodes(&f).await).unwrap();
    assert!(v[0]["probes"].as_array().unwrap().is_empty());
    let path = f.db_path.clone();
    let _dir = f.stop().await;
    assert_eq!(raw_count(&path), 0);
}

#[tokio::test]
async fn target_never_in_guest_outputs() {
    let f = start(|_| {}).await;
    let ck = login(f.port).await;
    let (_, token) = add_node(&f, &ck).await;
    let task = add_task(&f, &ck, 0, true, true).await;
    let mut g = live(f.port).await;
    let mut ws = agent(f.port, &token).await;
    send_result(&mut ws, result(task, Some(&format!("解析失败 {SECRET}")))).await;
    let v = wait_guest(&f, |v| v[0]["probes"][0]["error"].is_string()).await;
    assert_eq!(v[0]["probes"][0]["error"], "探测失败");
    assert!(!guest_nodes(&f).await.contains("secret-target"));
    let mut saw_probe = false;
    while let Some(m) = next_json_within(&mut g, Duration::from_millis(500)).await {
        assert!(!m.to_string().contains("secret-target"), "{m}");
        if m["type"] == "probe" {
            saw_probe = true;
            assert_eq!(m["error"], "探测失败");
        }
    }
    assert!(saw_probe);
    // 管理员视图也不带 target（探测卡片字段与访客一致）。
    let (_, _, admin) = http(f.port, "GET", "/api/nodes", Some(&ck), None).await;
    let a: Value = serde_json::from_str(&admin).unwrap();
    assert!(!a[0]["probes"][0].to_string().contains("secret-target"));
}

#[tokio::test]
async fn probe_key_sets_exact() {
    let f = start(|_| {}).await;
    let ck = login(f.port).await;
    let (_, token) = add_node(&f, &ck).await;
    let task = add_task(&f, &ck, 0, true, true).await;
    let hidden = add_task(&f, &ck, 0, true, false).await;
    let mut g = live(f.port).await;
    let mut ws = agent(f.port, &token).await;
    send_result(&mut ws, result(hidden, None)).await;
    send_result(&mut ws, result(task, None)).await;
    let v = wait_guest(&f, |v| v[0]["probes"][0]["avg_ms"] == 32.5).await;
    // show_on_card = false 的任务不出现在卡片。
    assert_eq!(v[0]["probes"].as_array().unwrap().len(), 1);
    let want = sorted(&["task_id", "name", "avg_ms", "loss", "error"]);
    assert_eq!(keys(&v[0]["probes"][0]), want);
    let mut probes = 0;
    while let Some(m) = next_json_within(&mut g, Duration::from_millis(500)).await {
        if m["type"] == "probe" {
            probes += 1;
            assert_eq!(m["task_id"], task);
            assert_eq!(
                keys(&m),
                sorted(&[
                    "type", "node_id", "task_id", "name", "avg_ms", "loss", "error"
                ])
            );
        }
    }
    assert_eq!(probes, 1);
}

#[tokio::test]
async fn deleting_task_clears_latest() {
    let f = start(|_| {}).await;
    let ck = login(f.port).await;
    let (_, token) = add_node(&f, &ck).await;
    let task = add_task(&f, &ck, 0, true, true).await;
    let mut ws = agent(f.port, &token).await;
    send_result(&mut ws, result(task, None)).await;
    wait_guest(&f, |v| v[0]["probes"][0]["avg_ms"] == 32.5).await;
    api(
        &f,
        &ck,
        "DELETE",
        &format!("/api/admin/probes/{task}"),
        Some(json!({})),
    )
    .await;
    let v: Value = serde_json::from_str(&guest_nodes(&f).await).unwrap();
    assert!(v[0]["probes"].as_array().unwrap().is_empty());
    // 迟到的结果被丢弃。
    send_result(&mut ws, result(task, None)).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let v: Value = serde_json::from_str(&guest_nodes(&f).await).unwrap();
    assert!(v[0]["probes"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn changing_target_clears_latest_but_renaming_keeps_it() {
    let f = start(|_| {}).await;
    let ck = login(f.port).await;
    let (_, token) = add_node(&f, &ck).await;
    let task = add_task(&f, &ck, 0, true, true).await;
    let mut ws = agent(f.port, &token).await;
    send_result(&mut ws, result(task, None)).await;
    wait_guest(&f, |v| v[0]["probes"][0]["avg_ms"] == 32.5).await;
    let path = format!("/api/admin/probes/{task}");
    api(&f, &ck, "PATCH", &path, Some(json!({"name": "联通"}))).await;
    let v: Value = serde_json::from_str(&guest_nodes(&f).await).unwrap();
    assert_eq!(v[0]["probes"][0]["avg_ms"], 32.5, "仅改名不应清空");
    api(
        &f,
        &ck,
        "PATCH",
        &path,
        Some(json!({"target": "other.example.org"})),
    )
    .await;
    let v: Value = serde_json::from_str(&guest_nodes(&f).await).unwrap();
    assert!(
        v[0]["probes"][0]["avg_ms"].is_null(),
        "改目标后旧结果应清空: {v}"
    );
}

#[tokio::test]
async fn card_affecting_task_edits_resync_live_clients() {
    let f = start(|_| {}).await;
    let ck = login(f.port).await;
    add_node(&f, &ck).await;
    let task = add_task(&f, &ck, 0, true, true).await;
    let mut g = live(f.port).await;
    assert_eq!(next_json(&mut g).await["type"], "snapshot");
    // 依次改名、改 show_on_card、再删除：每次都要有新的快照反映当前卡片。
    let steps: [(&str, Option<Value>, usize, Option<&str>); 3] = [
        ("PATCH", Some(json!({"name": "新名"})), 1, Some("新名")),
        ("PATCH", Some(json!({"show_on_card": false})), 0, None),
        ("DELETE", Some(json!({})), 0, None),
    ];
    for (method, body, n, name) in steps {
        api(&f, &ck, method, &format!("/api/admin/probes/{task}"), body).await;
        let snap = loop {
            let m = next_json(&mut g).await;
            if m["type"] == "snapshot" {
                break m;
            }
        };
        let probes = snap["nodes"][0]["probes"].as_array().unwrap().clone();
        assert_eq!(probes.len(), n, "{snap}");
        if let Some(name) = name {
            assert_eq!(probes[0]["name"], name);
        }
    }
}
