//! 后台接口：节点增删改、重置 token、排序、设置，以及变更后的 live 同步。

mod common;

use common::{Fixture, http, login, start};
use futures_util::{SinkExt, StreamExt};
use minimon_common::protocol::{AgentMsg, Hello, PROTOCOL_VERSION, Report};
use minimon_server::db::Db;
use minimon_server::db::nodes::hash_token;
use serde_json::{Value, json};
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

fn hello() -> Hello {
    Hello {
        v: PROTOCOL_VERSION,
        agent_version: "1.0.0".into(),
        hostname: "host".into(),
        os: "linux".into(),
        kernel: "k".into(),
        arch: "x86_64".into(),
        cpu_model: "cpu".into(),
        cpu_cores: 2,
        mem_total: 1000,
        swap_total: 0,
        disk_total: 5000,
        virtualization: None,
    }
}

fn report(ts_ms: i64, cpu: f32) -> Report {
    Report {
        ts_ms,
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

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

async fn next_json(ws: &mut Ws) -> Value {
    loop {
        let m = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("等待消息超时")
            .expect("连接已结束")
            .unwrap();
        if let Message::Text(t) = m {
            return serde_json::from_str(&t).unwrap();
        }
    }
}

/// 读到 `snapshot` 为止（跳过 online/offline/report）。
async fn next_snapshot(ws: &mut Ws) -> Value {
    loop {
        let v = next_json(ws).await;
        if v["type"] == "snapshot" {
            return v;
        }
    }
}

async fn connect_agent(port: u16, token: &str) -> Result<Ws, String> {
    let mut req = format!("ws://127.0.0.1:{port}/api/agent/ws")
        .into_client_request()
        .unwrap();
    req.headers_mut()
        .insert("Authorization", format!("Bearer {token}").parse().unwrap());
    let (mut ws, _) = tokio_tungstenite::connect_async(req)
        .await
        .map_err(|e| e.to_string())?;
    let h = serde_json::to_string(&AgentMsg::Hello(hello())).unwrap();
    ws.send(Message::text(h)).await.unwrap();
    let first = next_json(&mut ws).await;
    if first["type"] == "welcome" {
        Ok(ws)
    } else {
        Err(first.to_string())
    }
}

async fn send_report(ws: &mut Ws, r: Report) {
    let t = serde_json::to_string(&AgentMsg::Report(r)).unwrap();
    ws.send(Message::text(t)).await.unwrap();
}

async fn live(port: u16, cookie: Option<&str>) -> Ws {
    let mut req = format!("ws://127.0.0.1:{port}/api/live")
        .into_client_request()
        .unwrap();
    if let Some(c) = cookie {
        req.headers_mut().insert("Cookie", c.parse().unwrap());
    }
    tokio_tungstenite::connect_async(req).await.unwrap().0
}

/// 期望下一条是 `Error{code}`，随后连接结束。
async fn expect_error_then_closed(ws: &mut Ws, code: &str) {
    let mut v = next_json(ws).await;
    // Welcome 之后会先收到初始 probe_tasks，不属于被测内容。
    while v["type"] == "probe_tasks" {
        v = next_json(ws).await;
    }
    assert_eq!(v["type"], "error", "{v}");
    assert_eq!(v["code"], code, "{v}");
    let end = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match ws.next().await {
                None | Some(Err(_)) | Some(Ok(Message::Close(_))) => return,
                Some(Ok(_)) => {}
            }
        }
    })
    .await;
    assert!(end.is_ok(), "连接应被关闭");
}

async fn expect_closed(ws: &mut Ws) {
    let end = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match ws.next().await {
                None | Some(Err(_)) | Some(Ok(Message::Close(_))) => return,
                Some(Ok(Message::Text(t))) => panic!("关闭前不得再收到数据: {t}"),
                Some(Ok(_)) => {}
            }
        }
    })
    .await;
    assert!(end.is_ok(), "连接应被关闭");
}

async fn call(
    f: &Fixture,
    method: &str,
    path: &str,
    cookie: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let body = body.map(|b| b.to_string());
    let (code, _, text) = http(
        f.port,
        method,
        path,
        Some(cookie),
        body.as_deref().or(Some("{}")),
    )
    .await;
    (code, serde_json::from_str(&text).unwrap_or(Value::Null))
}

async fn create(f: &Fixture, cookie: &str, name: &str) -> (i64, String) {
    let (code, v) = call(
        f,
        "POST",
        "/api/admin/nodes",
        cookie,
        Some(json!({ "name": name })),
    )
    .await;
    assert_eq!(code, 201, "{v}");
    (
        v["node"]["id"].as_i64().unwrap(),
        v["token"].as_str().unwrap().to_string(),
    )
}

async fn nodes(f: &Fixture, cookie: Option<&str>) -> (u16, Value) {
    let (c, _, b) = http(f.port, "GET", "/api/nodes", cookie, None).await;
    (c, serde_json::from_str(&b).unwrap_or(Value::Null))
}

async fn wait_nodes(f: &Fixture, cookie: Option<&str>, ok: impl Fn(&Value) -> bool) -> Value {
    let mut v = Value::Null;
    for _ in 0..50 {
        v = nodes(f, cookie).await.1;
        if ok(&v) {
            return v;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("等待条件超时: {v}");
}

#[tokio::test]
async fn admin_routes_require_login() {
    let f = start(|_| {}).await;
    let routes = [
        ("GET", "/api/admin/nodes"),
        ("POST", "/api/admin/nodes"),
        ("PATCH", "/api/admin/nodes/1"),
        ("DELETE", "/api/admin/nodes/1"),
        ("POST", "/api/admin/nodes/1/token"),
        ("PUT", "/api/admin/nodes/order"),
        ("GET", "/api/admin/settings"),
        ("PUT", "/api/admin/settings"),
        ("PATCH", "/api/admin/nodes/abc"),
    ];
    for (m, p) in routes {
        let (code, _, body) = http(f.port, m, p, None, Some("{}")).await;
        assert_eq!(code, 401, "{m} {p}: {body}");
    }
}

#[tokio::test]
async fn create_node_returns_token_once() {
    let f = start(|_| {}).await;
    let cookie = login(f.port).await;
    let (code, v) = call(
        &f,
        "POST",
        "/api/admin/nodes",
        &cookie,
        Some(json!({"name": " hk-1 ", "region": "hk", "group": "g"})),
    )
    .await;
    assert_eq!(code, 201);
    let token = v["token"].as_str().unwrap().to_string();
    assert_eq!(token.len(), 64);
    assert_eq!(v["node"]["name"], "hk-1");
    assert_eq!(v["node"]["region"], "HK");
    assert!(v["node"].get("token").is_none());
    let stored: String = rusqlite::Connection::open(&f.db_path)
        .unwrap()
        .query_row("SELECT token_hash FROM nodes", [], |r| r.get(0))
        .unwrap();
    assert_eq!(stored, hash_token(&token));
    let (_, list) = call(&f, "GET", "/api/admin/nodes", &cookie, None).await;
    assert!(!list.to_string().contains(&token) && !list.to_string().contains(&stored));
    assert!(list[0].get("token").is_none() && list[0].get("token_hash").is_none());
    let _a = connect_agent(f.port, &token).await.unwrap();
    wait_nodes(&f, Some(&cookie), |v| v[0]["online"] == true).await;
}

#[tokio::test]
async fn validation_errors_are_chinese_400() {
    let f = start(|_| {}).await;
    let cookie = login(f.port).await;
    let bad = [
        json!({"name": "   "}),
        json!({"name": "x".repeat(65)}),
        json!({"name": "a", "region": "USA"}),
        json!({"name": "a", "group": "g".repeat(33)}),
        json!({"name": "a", "note": "n".repeat(501)}),
    ];
    for b in bad {
        let (code, v) = call(&f, "POST", "/api/admin/nodes", &cookie, Some(b.clone())).await;
        assert_eq!(code, 400, "{b}");
        let msg = v["error"].as_str().unwrap();
        assert!(!msg.is_ascii(), "应为中文: {msg}");
    }
    let (id, _) = create(&f, &cookie, "ok").await;
    let (code, _) = call(
        &f,
        "PATCH",
        &format!("/api/admin/nodes/{id}"),
        &cookie,
        Some(json!({"region": "USA"})),
    )
    .await;
    assert_eq!(code, 400);
    let (code, _) = call(
        &f,
        "PATCH",
        "/api/admin/nodes/9999",
        &cookie,
        Some(json!({"name": "x"})),
    )
    .await;
    assert_eq!(code, 404);
}

#[tokio::test]
async fn patch_updates_fields() {
    let f = start(|_| {}).await;
    let cookie = login(f.port).await;
    let (id, _) = create(&f, &cookie, "a").await;
    let (code, v) = call(
        &f,
        "PATCH",
        &format!("/api/admin/nodes/{id}"),
        &cookie,
        Some(json!({"name": "b", "note": "hi", "region": "us"})),
    )
    .await;
    assert_eq!(code, 200);
    assert_eq!(
        (v["name"].as_str(), v["note"].as_str(), v["region"].as_str()),
        (Some("b"), Some("hi"), Some("US"))
    );
    let (_, list) = nodes(&f, Some(&cookie)).await;
    assert_eq!(list[0]["name"], "b");
}

#[tokio::test]
async fn reset_token_kicks_agent() {
    let f = start(|_| {}).await;
    let cookie = login(f.port).await;
    let (id, old) = create(&f, &cookie, "a").await;
    let mut a = connect_agent(f.port, &old).await.unwrap();
    wait_nodes(&f, Some(&cookie), |v| v[0]["online"] == true).await;
    let (code, v) = call(
        &f,
        "POST",
        &format!("/api/admin/nodes/{id}/token"),
        &cookie,
        None,
    )
    .await;
    assert_eq!(code, 200);
    let new = v["token"].as_str().unwrap().to_string();
    assert_ne!(new, old);
    expect_error_then_closed(&mut a, "unauthorized").await;
    wait_nodes(&f, Some(&cookie), |v| v[0]["online"] == false).await;
    assert!(connect_agent(f.port, &old).await.is_err());
    connect_agent(f.port, &new).await.unwrap();
    let (code, _) = call(&f, "POST", "/api/admin/nodes/9999/token", &cookie, None).await;
    assert_eq!(code, 404);
}

#[tokio::test]
async fn delete_node_removes_data_and_kicks() {
    let f = start(|_| {}).await;
    let cookie = login(f.port).await;
    let (id, token) = create(&f, &cookie, "a").await;
    let (id2, _) = create(&f, &cookie, "keep").await;
    let c = rusqlite::Connection::open(&f.db_path).unwrap();
    for t in ["metrics_1m", "metrics_15m"] {
        for nid in [id, id2] {
            c.execute(
                &format!("INSERT INTO {t} (node_id, ts_ms, samples) VALUES (?1, 60000, 1)"),
                [nid],
            )
            .unwrap();
        }
    }
    Db::open(&f.db_path)
        .unwrap()
        .insert_reports(&[(id, report(1, 1.0)), (id2, report(1, 1.0))])
        .unwrap();
    let mut a = connect_agent(f.port, &token).await.unwrap();
    let mut adm = live(f.port, Some(&cookie)).await;
    assert_eq!(
        next_snapshot(&mut adm).await["nodes"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let (code, _) = call(
        &f,
        "DELETE",
        &format!("/api/admin/nodes/{id}"),
        &cookie,
        None,
    )
    .await;
    assert_eq!(code, 204);
    for t in ["metrics_raw", "metrics_1m", "metrics_15m"] {
        let n: i64 = c
            .query_row(
                &format!("SELECT count(*) FROM {t} WHERE node_id = ?1"),
                [id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 0, "{t}");
        let n: i64 = c
            .query_row(
                &format!("SELECT count(*) FROM {t} WHERE node_id = ?1"),
                [id2],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1, "{t} 其他节点数据应保留");
    }
    expect_error_then_closed(&mut a, "unauthorized").await;
    let snap = next_snapshot(&mut adm).await;
    let ids: Vec<_> = snap["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["id"].as_i64().unwrap())
        .collect();
    assert_eq!(ids, [id2]);
    assert!(connect_agent(f.port, &token).await.is_err());
    let (code, _) = call(
        &f,
        "DELETE",
        &format!("/api/admin/nodes/{id}"),
        &cookie,
        None,
    )
    .await;
    assert_eq!(code, 404);
}

#[tokio::test]
async fn recreated_node_does_not_inherit_last_known() {
    let f = start(|_| {}).await;
    let cookie = login(f.port).await;
    let (id, token) = create(&f, &cookie, "a").await;
    let mut a = connect_agent(f.port, &token).await.unwrap();
    send_report(&mut a, report(now_ms(), 7.0)).await;
    wait_nodes(&f, None, |v| v[0]["latest"]["cpu"] == 7.0).await;
    let (code, _) = call(
        &f,
        "DELETE",
        &format!("/api/admin/nodes/{id}"),
        &cookie,
        None,
    )
    .await;
    assert_eq!(code, 204);
    let (id_b, _) = create(&f, &cookie, "b").await;
    assert_ne!(id_b, id, "AUTOINCREMENT：id 不得复用");
    for ck in [None, Some(cookie.as_str())] {
        let (_, v) = nodes(&f, ck).await;
        assert_eq!(v.as_array().unwrap().len(), 1);
        assert!(v[0]["latest"].is_null(), "{v}");
        assert!(v[0]["last_seen_ms"].is_null(), "{v}");
        assert_eq!(v[0]["online"], false);
    }
}

#[tokio::test]
async fn hide_triggers_guest_resync() {
    let f = start(|_| {}).await;
    let cookie = login(f.port).await;
    let (id, _) = create(&f, &cookie, "a").await;
    let (_id2, _) = create(&f, &cookie, "b").await;
    let mut g = live(f.port, None).await;
    assert_eq!(
        next_snapshot(&mut g).await["nodes"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let (code, _, _) = http(
        f.port,
        "GET",
        &format!("/api/nodes/{id}/history?range=1h"),
        None,
        None,
    )
    .await;
    assert_eq!(code, 200);
    let (code, _) = call(
        &f,
        "PATCH",
        &format!("/api/admin/nodes/{id}"),
        &cookie,
        Some(json!({"hidden": true})),
    )
    .await;
    assert_eq!(code, 200);
    let snap = next_snapshot(&mut g).await;
    let names: Vec<_> = snap["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(names, ["b"]);
    let (code, _, b) = http(
        f.port,
        "GET",
        &format!("/api/nodes/{id}/history?range=1h"),
        None,
        None,
    )
    .await;
    assert_eq!(code, 404, "{b}");
}

#[tokio::test]
async fn reorder_changes_order() {
    let f = start(|_| {}).await;
    let cookie = login(f.port).await;
    let (a, _) = create(&f, &cookie, "a").await;
    let (b, _) = create(&f, &cookie, "b").await;
    let (c, _) = create(&f, &cookie, "c").await;
    let mut g = live(f.port, None).await;
    next_snapshot(&mut g).await;
    let (code, _) = call(
        &f,
        "PUT",
        "/api/admin/nodes/order",
        &cookie,
        Some(json!({"ids": [c, a, b]})),
    )
    .await;
    assert_eq!(code, 204);
    let snap = next_snapshot(&mut g).await;
    let ids: Vec<_> = snap["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["id"].as_i64().unwrap())
        .collect();
    assert_eq!(ids, [c, a, b]);
    let (_, v) = nodes(&f, None).await;
    assert_eq!(v[0]["id"], c);
    // 未列出的节点排在后面；未知 / 重复 id 被拒绝
    let (code, _) = call(
        &f,
        "PUT",
        "/api/admin/nodes/order",
        &cookie,
        Some(json!({"ids": [b]})),
    )
    .await;
    assert_eq!(code, 204);
    let (_, v) = nodes(&f, None).await;
    assert_eq!(v[0]["id"], b);
    for bad in [json!({"ids": [a, a]}), json!({"ids": [9999]})] {
        let (code, _) = call(&f, "PUT", "/api/admin/nodes/order", &cookie, Some(bad)).await;
        assert_eq!(code, 400);
    }
}

#[tokio::test]
async fn require_login_closes_guest_live() {
    let f = start(|_| {}).await;
    let cookie = login(f.port).await;
    create(&f, &cookie, "a").await;
    let (code, v) = call(&f, "GET", "/api/admin/settings", &cookie, None).await;
    assert_eq!(
        (code, &v["require_login"], v["public_url"].is_null()),
        (200, &json!(false), true)
    );
    let mut g = live(f.port, None).await;
    let mut adm = live(f.port, Some(&cookie)).await;
    next_snapshot(&mut g).await;
    next_snapshot(&mut adm).await;
    let (code, v) = call(
        &f,
        "PUT",
        "/api/admin/settings",
        &cookie,
        Some(json!({"require_login": true})),
    )
    .await;
    assert_eq!((code, &v["require_login"]), (200, &json!(true)));
    next_snapshot(&mut adm).await;
    expect_closed(&mut g).await;
    assert_eq!(nodes(&f, None).await.0, 401);
    assert_eq!(nodes(&f, Some(&cookie)).await.0, 200);
    assert_eq!(
        Db::open(&f.db_path)
            .unwrap()
            .get_setting("require_login")
            .unwrap()
            .as_deref(),
        Some("1")
    );
    // 管理员连接保持：再变更一次仍能收到快照
    call(
        &f,
        "PUT",
        "/api/admin/settings",
        &cookie,
        Some(json!({"require_login": false})),
    )
    .await;
    next_snapshot(&mut adm).await;
    assert_eq!(nodes(&f, None).await.0, 200);
    let (code, _) = call(
        &f,
        "PUT",
        "/api/admin/settings",
        &cookie,
        Some(json!({"require_login": true, "x": 1})),
    )
    .await;
    assert_eq!(code, 400);
}

#[tokio::test]
async fn non_numeric_ids_return_json_404() {
    let f = start(|_| {}).await;
    let cookie = login(f.port).await;
    let (code, _, b) = http(
        f.port,
        "PATCH",
        "/api/admin/nodes/abc",
        Some(&cookie),
        Some("{}"),
    )
    .await;
    assert_eq!(code, 404);
    assert_eq!(
        serde_json::from_str::<Value>(&b).unwrap()["error"],
        "节点不存在"
    );
    let (code, _, b) = http(f.port, "GET", "/api/nodes/abc/history?range=1h", None, None).await;
    assert_eq!(code, 404);
    assert_eq!(
        serde_json::from_str::<Value>(&b).unwrap()["error"],
        "节点不存在或未公开"
    );
}

#[tokio::test]
async fn health_is_200_even_with_require_login() {
    let f = start(|_| {}).await;
    let cookie = login(f.port).await;
    let (code, _) = call(
        &f,
        "PUT",
        "/api/admin/settings",
        &cookie,
        Some(json!({"require_login": true})),
    )
    .await;
    assert_eq!(code, 200);
    assert_eq!(nodes(&f, None).await.0, 401);
    let (code, _, body) = http(f.port, "GET", "/api/health", None, None).await;
    assert_eq!((code, body.as_str()), (200, r#"{"ok":true}"#));
}

#[tokio::test]
async fn tags_patch_and_views() {
    let f = start(|_| {}).await;
    let cookie = login(f.port).await;
    let (id, _) = create(&f, &cookie, "a").await;
    // existing_nodes_default_empty：新建节点无标签
    let (_, list) = nodes(&f, None).await;
    assert_eq!(list[0]["tags"], json!([]));
    let (code, v) = call(
        &f,
        "PATCH",
        &format!("/api/admin/nodes/{id}"),
        &cookie,
        Some(json!({"tags": "1Gbps<yellow>;CN2 GIA<red>"})),
    )
    .await;
    assert_eq!(code, 200, "{v}");
    assert_eq!(v["tags_raw"], "1Gbps<yellow>;CN2 GIA<red>");
    let (_, list) = nodes(&f, None).await;
    assert_eq!(
        list[0]["tags"],
        json!([{"text": "1Gbps", "color": "yellow"}, {"text": "CN2 GIA", "color": "red"}])
    );
    assert!(list[0].get("tags_raw").is_none());
    let (_, list) = nodes(&f, Some(&cookie)).await;
    assert_eq!(list[0]["tags_raw"], "1Gbps<yellow>;CN2 GIA<red>");
    let (code, v) = call(
        &f,
        "PATCH",
        &format!("/api/admin/nodes/{id}"),
        &cookie,
        Some(json!({"tags": "x<rainbow>"})),
    )
    .await;
    assert_eq!(code, 400);
    assert!(v.to_string().contains("rainbow"), "{v}");
    let (_, list) = nodes(&f, Some(&cookie)).await;
    assert_eq!(list[0]["tags_raw"], "1Gbps<yellow>;CN2 GIA<red>");
}

async fn get_token(f: &Fixture, cookie: &str, id: i64) -> (u16, Value) {
    let (c, _, b) = http(
        f.port,
        "GET",
        &format!("/api/admin/nodes/{id}/token"),
        Some(cookie),
        None,
    )
    .await;
    (c, serde_json::from_str(&b).unwrap_or(Value::Null))
}

#[tokio::test]
async fn token_endpoint_returns_created_token() {
    let f = start(|_| {}).await;
    let cookie = login(f.port).await;
    let (id, token) = create(&f, &cookie, "a").await;
    let (code, v) = get_token(&f, &cookie, id).await;
    assert_eq!(code, 200, "{v}");
    assert_eq!(v["token"], token.as_str());
    // token_not_in_lists
    for (path, ck) in [
        ("/api/nodes", None),
        ("/api/admin/nodes", Some(cookie.as_str())),
    ] {
        let (_, _, b) = http(f.port, "GET", path, ck, None).await;
        assert!(!b.contains(&token), "{path} 泄露 token");
    }
    let (code, _, _) = http(
        f.port,
        "GET",
        &format!("/api/admin/nodes/{id}/token"),
        None,
        None,
    )
    .await;
    assert_eq!(code, 401);
    let (code, _) = get_token(&f, &cookie, 9999).await;
    assert_eq!(code, 404);
}

#[tokio::test]
async fn token_endpoint_legacy_then_reset() {
    let f = start(|_| {}).await;
    let cookie = login(f.port).await;
    let (id, _) = create(&f, &cookie, "a").await;
    // 模拟旧版本创建的节点：只存了哈希
    let c = rusqlite::Connection::open(&f.db_path).unwrap();
    c.execute("UPDATE nodes SET token = NULL", []).unwrap();
    drop(c);
    let (code, v) = get_token(&f, &cookie, id).await;
    assert_eq!(code, 200);
    assert!(v["token"].is_null(), "{v}");
    let (code, r) = call(
        &f,
        "POST",
        &format!("/api/admin/nodes/{id}/token"),
        &cookie,
        None,
    )
    .await;
    assert_eq!(code, 200, "{r}");
    let (_, v) = get_token(&f, &cookie, id).await;
    assert_eq!(v["token"], r["token"]);
}
