//! 探测任务下发：Welcome 之后的全量任务、变更只推给受影响的在线节点。

mod common;

use common::{Fixture, http, login, start};
use futures_util::{SinkExt, StreamExt};
use minimon_common::protocol::{AgentMsg, Hello, PROTOCOL_VERSION, ProbeTask, ServerMsg};
use serde_json::{Value, json};
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
        mem_total: 1,
        swap_total: 0,
        disk_total: 1,
        virtualization: None,
    }
}

async fn next(ws: &mut Ws, wait: Duration) -> Option<ServerMsg> {
    loop {
        match tokio::time::timeout(wait, ws.next()).await {
            Ok(Some(Ok(Message::Text(t)))) => return Some(serde_json::from_str(&t).unwrap()),
            Ok(Some(Ok(_))) => continue,
            _ => return None,
        }
    }
}

async fn tasks(ws: &mut Ws) -> Vec<ProbeTask> {
    match next(ws, Duration::from_secs(5)).await {
        Some(ServerMsg::ProbeTasks { tasks }) => tasks,
        other => panic!("期望 probe_tasks，实际 {other:?}"),
    }
}

/// 以 token 接入并读完 Welcome。
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
        next(&mut ws, Duration::from_secs(5)).await,
        Some(ServerMsg::Welcome { .. })
    ));
    ws
}

async fn api(f: &Fixture, ck: &str, method: &str, path: &str, body: Option<Value>) -> Value {
    let b = body.map(|v| v.to_string());
    let (c, _, text) = http(f.port, method, path, Some(ck), b.as_deref()).await;
    assert!(c == 200 || c == 201 || c == 204, "{c} {text}");
    serde_json::from_str(&text).unwrap_or(Value::Null)
}

async fn add_node(f: &Fixture, ck: &str, name: &str) -> (i64, String) {
    let v = api(
        f,
        ck,
        "POST",
        "/api/admin/nodes",
        Some(json!({"name": name})),
    )
    .await;
    (
        v["node"]["id"].as_i64().unwrap(),
        v["token"].as_str().unwrap().to_string(),
    )
}

fn task_body(name: &str, all: bool, nodes: &[i64]) -> Value {
    json!({"name": name, "kind": "tcp", "target": "example.com:80",
           "all_nodes": all, "node_ids": nodes})
}

#[tokio::test]
async fn welcome_is_followed_by_probe_tasks() {
    let f = start(|_| {}).await;
    let ck = login(f.port).await;
    let (_, token) = add_node(&f, &ck, "a").await;
    let t = api(
        &f,
        &ck,
        "POST",
        "/api/admin/probes",
        Some(task_body("t1", true, &[])),
    )
    .await;
    let id = t["id"].as_i64().unwrap();

    let mut ws = agent(f.port, &token).await;
    let got = tasks(&mut ws).await;
    assert_eq!(got.iter().map(|t| t.id).collect::<Vec<_>>(), [id]);
    assert_eq!(got[0].target, "example.com:80");
    drop(ws);

    let mut ws = agent(f.port, &token).await;
    assert_eq!(tasks(&mut ws).await.len(), 1);
}

#[tokio::test]
async fn empty_list_sent_when_no_tasks() {
    let f = start(|_| {}).await;
    let ck = login(f.port).await;
    let (_, token) = add_node(&f, &ck, "a").await;
    let mut ws = agent(f.port, &token).await;
    assert!(tasks(&mut ws).await.is_empty());
}

#[tokio::test]
async fn task_change_pushes_to_affected_online_nodes_only() {
    let f = start(|_| {}).await;
    let ck = login(f.port).await;
    let (a, ta) = add_node(&f, &ck, "a").await;
    let (_, tb) = add_node(&f, &ck, "b").await;
    let mut wa = agent(f.port, &ta).await;
    let mut wb = agent(f.port, &tb).await;
    assert!(tasks(&mut wa).await.is_empty());
    assert!(tasks(&mut wb).await.is_empty());

    let t = api(
        &f,
        &ck,
        "POST",
        "/api/admin/probes",
        Some(task_body("t", false, &[a])),
    )
    .await;
    let id = t["id"].as_i64().unwrap();
    assert_eq!(
        tasks(&mut wa)
            .await
            .iter()
            .map(|t| t.id)
            .collect::<Vec<_>>(),
        [id]
    );
    assert!(next(&mut wb, Duration::from_secs(1)).await.is_none());

    api(
        &f,
        &ck,
        "PATCH",
        &format!("/api/admin/probes/{id}"),
        Some(json!({"all_nodes": true})),
    )
    .await;
    assert_eq!(tasks(&mut wa).await.len(), 1);
    assert_eq!(tasks(&mut wb).await.len(), 1);
}

#[tokio::test]
async fn delete_task_pushes_shrunk_list() {
    let f = start(|_| {}).await;
    let ck = login(f.port).await;
    let (_, token) = add_node(&f, &ck, "a").await;
    let t1 = api(
        &f,
        &ck,
        "POST",
        "/api/admin/probes",
        Some(task_body("t1", true, &[])),
    )
    .await;
    let t2 = api(
        &f,
        &ck,
        "POST",
        "/api/admin/probes",
        Some(task_body("t2", true, &[])),
    )
    .await;
    let mut ws = agent(f.port, &token).await;
    assert_eq!(tasks(&mut ws).await.len(), 2);
    let id1 = t1["id"].as_i64().unwrap();
    api(
        &f,
        &ck,
        "DELETE",
        &format!("/api/admin/probes/{id1}"),
        Some(json!({})),
    )
    .await;
    let got = tasks(&mut ws).await;
    assert_eq!(
        got.iter().map(|t| t.id).collect::<Vec<_>>(),
        [t2["id"].as_i64().unwrap()]
    );
}

#[tokio::test]
async fn isp_template_pushes_to_online_nodes() {
    let f = start(|_| {}).await;
    let ck = login(f.port).await;
    let (_, token) = add_node(&f, &ck, "a").await;
    let mut ws = agent(f.port, &token).await;
    assert!(tasks(&mut ws).await.is_empty());
    api(
        &f,
        &ck,
        "POST",
        "/api/admin/probes/isp-template",
        Some(json!({"telecom": "1.1.1.1"})),
    )
    .await;
    assert_eq!(tasks(&mut ws).await.len(), 1);
}
