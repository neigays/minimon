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

// ------------------------------------------------------------ 管理 API

async fn admin_fixture() -> (Fixture, String) {
    let f = common::start(|_| {}).await;
    let cookie = login(f.port).await;
    (f, cookie)
}

fn json(body: &str) -> serde_json::Value {
    serde_json::from_str(body).unwrap()
}

#[tokio::test]
async fn rules_crud_and_validation() {
    let (f, c) = admin_fixture().await;
    let p = f.port;
    let rule = r#"{"name":"cpu高","enabled":true,"metric":"cpu","op":"gt","threshold":90,"all_nodes":true,"node_ids":[]}"#;
    let (code, _, body) = http(p, "POST", "/api/admin/alert-rules", Some(&c), Some(rule)).await;
    assert_eq!(code, 201, "{body}");
    let v = json(&body);
    assert_eq!(v["duration_secs"], 300);
    assert_eq!(v["cooldown_secs"], 1800);
    let id = v["id"].as_i64().unwrap();

    let bad = |extra: &str| {
        format!(
            r#"{{"name":"x","enabled":true,"metric":"cpu","op":"gt","threshold":90,"all_nodes":true,"node_ids":[]{extra}}}"#
        )
    };
    for (extra, what) in [
        (r#","duration_secs":5"#, "duration"),
        (r#","probe_task_id":1"#, "probe"),
        (r#","threshold":150"#, "threshold"),
        (r#","cooldown_secs":-1"#, "cooldown"),
    ] {
        // 后出现的重复键会让 serde 报错，所以用替换而不是追加。
        let body = if extra.contains("threshold") {
            bad("").replace(r#""threshold":90"#, r#""threshold":150"#)
        } else {
            bad(extra)
        };
        let (code, _, b) = http(p, "POST", "/api/admin/alert-rules", Some(&c), Some(&body)).await;
        assert_eq!(code, 400, "{what}: {b}");
    }
    let (_, _, b) = http(
        p,
        "POST",
        "/api/admin/alert-rules",
        Some(&c),
        Some(&bad(r#","probe_task_id":1"#)),
    )
    .await;
    assert!(b.contains("只有探测类指标可以绑定任务"), "{b}");
    // all_nodes=false 需要节点
    let (code, _, _) = http(
        p,
        "POST",
        "/api/admin/alert-rules",
        Some(&c),
        Some(&bad("").replace(r#""all_nodes":true"#, r#""all_nodes":false"#)),
    )
    .await;
    assert_eq!(code, 400);

    let (code, _, b) = http(
        p,
        "PATCH",
        &format!("/api/admin/alert-rules/{id}"),
        Some(&c),
        Some(r#"{"threshold":80}"#),
    )
    .await;
    assert_eq!(code, 200, "{b}");
    assert_eq!(json(&b)["threshold"], 80.0);
    let (code, _, b) = http(p, "GET", "/api/admin/alert-rules", Some(&c), None).await;
    assert_eq!(code, 200);
    assert_eq!(json(&b).as_array().unwrap().len(), 1);
    let (code, _, _) = http(
        p,
        "DELETE",
        &format!("/api/admin/alert-rules/{id}"),
        Some(&c),
        Some("{}"),
    )
    .await;
    assert_eq!(code, 204);
    let (code, _, _) = http(
        p,
        "DELETE",
        &format!("/api/admin/alert-rules/{id}"),
        Some(&c),
        Some("{}"),
    )
    .await;
    assert_eq!(code, 404);
    f.stop().await;
}

#[tokio::test]
async fn sendkey_never_echoed() {
    let (f, c) = admin_fixture().await;
    let p = f.port;
    let put = |b: &'static str| {
        let c = c.clone();
        async move { http(p, "PUT", "/api/admin/alert-settings", Some(&c), Some(b)).await }
    };
    let (code, _, b) = put(r#"{"sendkey":"sctp42tSECRETKEY"}"#).await;
    assert_eq!(code, 200, "{b}");
    assert!(!b.contains("SECRETKEY"));
    let v = json(&b);
    assert_eq!(v["sendkey_set"], true);
    assert_eq!(v["sendkey_tail"], "TKEY");
    let (code, _, b) = http(p, "GET", "/api/admin/alert-settings", Some(&c), None).await;
    assert_eq!(code, 200);
    assert!(!b.contains("SECRETKEY"));
    assert_eq!(json(&b)["sendkey_tail"], "TKEY");
    // 其他字段不影响 SendKey
    let (code, _, b) = put(r#"{"offline_grace_secs":60}"#).await;
    assert_eq!(code, 200, "{b}");
    assert_eq!(json(&b)["sendkey_set"], true);
    assert_eq!(json(&b)["offline_grace_secs"], 60);
    let (code, _, _) = put(r#"{"offline_grace_secs":5}"#).await;
    assert_eq!(code, 400);
    let (code, _, _) = put(r#"{}"#).await;
    assert_eq!(code, 400);
    let (code, _, b) = put(r#"{"sendkey":"bad"}"#).await;
    assert_eq!(code, 400);
    assert!(b.contains("SendKey 格式不正确"));
    let (code, _, b) = put(r#"{"sendkey":""}"#).await;
    assert_eq!(code, 200, "{b}");
    assert_eq!(json(&b)["sendkey_set"], false);
    assert!(json(&b)["sendkey_tail"].is_null());
    f.stop().await;
}

#[tokio::test]
async fn alert_endpoints_require_admin() {
    let f = common::start(|_| {}).await;
    let p = f.port;
    for (m, path, body) in [
        ("GET", "/api/admin/alert-rules", None),
        ("POST", "/api/admin/alert-rules", Some("{}")),
        ("PATCH", "/api/admin/alert-rules/1", Some("{}")),
        ("DELETE", "/api/admin/alert-rules/1", Some("{}")),
        ("GET", "/api/admin/alert-settings", None),
        ("PUT", "/api/admin/alert-settings", Some("{}")),
        ("POST", "/api/admin/alert-settings/test", Some("{}")),
        ("GET", "/api/admin/alert-events", None),
        ("GET", "/api/admin/alerts/active", None),
    ] {
        let (code, _, b) = http(p, m, path, None, body).await;
        assert_eq!(code, 401, "{m} {path}: {b}");
    }
    f.stop().await;
}

#[tokio::test]
async fn node_patch_alert_offline() {
    let out = std::sync::Arc::new(std::sync::Mutex::new(0));
    let o2 = out.clone();
    let f = start_at(
        tempfile::tempdir().unwrap(),
        move |db: &Db| *o2.lock().unwrap() = db.add_node("n1").unwrap().0.id,
        |_| {},
    )
    .await;
    let id = *out.lock().unwrap();
    let c = login(f.port).await;
    let (code, _, b) = http(
        f.port,
        "PATCH",
        &format!("/api/admin/nodes/{id}"),
        Some(&c),
        Some(r#"{"alert_offline":false}"#),
    )
    .await;
    assert_eq!(code, 200, "{b}");
    assert_eq!(json(&b)["alert_offline"], false);
    let (_, _, b) = http(f.port, "GET", "/api/admin/nodes", Some(&c), None).await;
    assert_eq!(json(&b)[0]["alert_offline"], false);
    let (_, _, b) = http(f.port, "GET", "/api/nodes", None, None).await;
    assert!(!b.contains("alert_offline"), "{b}");
    f.stop().await;
}

#[tokio::test]
async fn test_send_without_sendkey_is_400() {
    let (f, c) = admin_fixture().await;
    let (code, _, b) = http(
        f.port,
        "POST",
        "/api/admin/alert-settings/test",
        Some(&c),
        Some("{}"),
    )
    .await;
    assert_eq!(code, 400, "{b}");
    assert!(b.contains("发送失败"), "{b}");
    // 失败也记一条 test 事件
    let (code, _, b) = http(f.port, "GET", "/api/admin/alert-events", Some(&c), None).await;
    assert_eq!(code, 200);
    let v = json(&b);
    assert_eq!(v.as_array().unwrap().len(), 1, "{b}");
    assert_eq!(v[0]["kind"], "test");
    assert_eq!(v[0]["sent"], "failed");
    let (code, _, b) = http(f.port, "GET", "/api/admin/alerts/active", Some(&c), None).await;
    assert_eq!(code, 200);
    assert_eq!(b.trim(), "[]");
    f.stop().await;
}

#[tokio::test]
async fn patch_ignores_stale_references() {
    let out = std::sync::Arc::new(std::sync::Mutex::new(0));
    let o2 = out.clone();
    let f = start_at(
        tempfile::tempdir().unwrap(),
        move |db: &Db| *o2.lock().unwrap() = db.add_node("n1").unwrap().0.id,
        |_| {},
    )
    .await;
    let nid = *out.lock().unwrap();
    let (p, c) = (f.port, login(f.port).await);
    // (a) 绑定的探测任务被删除
    let (code, _, b) = http(
        p,
        "POST",
        "/api/admin/probes",
        Some(&c),
        Some(r#"{"name":"t","kind":"tcp","target":"1.1.1.1:80"}"#),
    )
    .await;
    assert_eq!(code, 201, "{b}");
    let tid = json(&b)["id"].as_i64().unwrap();
    let rule = format!(
        r#"{{"name":"r","enabled":true,"metric":"probe_loss","op":"gt","threshold":50,"all_nodes":true,"probe_task_id":{tid}}}"#
    );
    let (code, _, b) = http(p, "POST", "/api/admin/alert-rules", Some(&c), Some(&rule)).await;
    assert_eq!(code, 201, "{b}");
    let rid = json(&b)["id"].as_i64().unwrap();
    let (code, _, b) = http(
        p,
        "DELETE",
        &format!("/api/admin/probes/{tid}"),
        Some(&c),
        Some("{}"),
    )
    .await;
    assert!(code == 204 || code == 200, "{code} {b}");
    let (code, _, b) = http(
        p,
        "PATCH",
        &format!("/api/admin/alert-rules/{rid}"),
        Some(&c),
        Some(r#"{"enabled":false}"#),
    )
    .await;
    assert_eq!(code, 200, "{b}");
    assert_eq!(json(&b)["probe_task_id"], tid);
    assert_eq!(json(&b)["enabled"], false);
    // 显式给出不存在的任务仍然 400
    let (code, _, _) = http(
        p,
        "PATCH",
        &format!("/api/admin/alert-rules/{rid}"),
        Some(&c),
        Some(&format!(r#"{{"probe_task_id":{tid}}}"#)),
    )
    .await;
    assert_eq!(code, 400);
    // (b) 关联节点被删除
    let rule = format!(
        r#"{{"name":"r2","enabled":true,"metric":"cpu","op":"gt","threshold":50,"all_nodes":false,"node_ids":[{nid}]}}"#
    );
    let (code, _, b) = http(p, "POST", "/api/admin/alert-rules", Some(&c), Some(&rule)).await;
    assert_eq!(code, 201, "{b}");
    let rid = json(&b)["id"].as_i64().unwrap();
    let (code, _, b) = http(
        p,
        "DELETE",
        &format!("/api/admin/nodes/{nid}"),
        Some(&c),
        Some("{}"),
    )
    .await;
    assert!(code == 204 || code == 200, "{code} {b}");
    let (code, _, b) = http(
        p,
        "PATCH",
        &format!("/api/admin/alert-rules/{rid}"),
        Some(&c),
        Some(r#"{"name":"x"}"#),
    )
    .await;
    assert_eq!(code, 200, "{b}");
    assert_eq!(json(&b)["name"], "x");
    f.stop().await;
}
