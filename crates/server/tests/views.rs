//! 按身份输出节点视图、最后已知数据、IP 记录与整站登录。

mod common;

use common::{Fixture, login, start_at};
use futures_util::{SinkExt, StreamExt};
use minimon_common::protocol::{AgentMsg, Hello, NetInfo, PROTOCOL_VERSION, Report};
use minimon_server::db::Db;
use serde_json::Value;
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

const PUBLIC_KEYS: &[&str] = &[
    "id",
    "name",
    "tags",
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
];
const ADMIN_EXTRA: &[&str] = &[
    "hidden",
    "ipv4",
    "ipv6",
    "tags_raw",
    "hostname",
    "kernel",
    "agent_version",
    "ip",
    "note",
    "created_ms",
    "traffic_quota",
    "traffic_reset_day",
    "traffic_mode",
    "traffic",
    "expire_date",
    "expire_days",
    "alert_offline",
];

fn hello() -> Hello {
    Hello {
        v: PROTOCOL_VERSION,
        agent_version: "9.9.9-secret".into(),
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

fn sorted(v: &[&str]) -> Vec<String> {
    let mut k: Vec<String> = v.iter().map(|s| s.to_string()).collect();
    k.sort();
    k
}

fn keys(v: &Value) -> Vec<String> {
    let mut k: Vec<String> = v.as_object().unwrap().keys().cloned().collect();
    k.sort();
    k
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

async fn agent(port: u16, token: &str) -> Ws {
    let mut req = format!("ws://127.0.0.1:{port}/api/agent/ws")
        .into_client_request()
        .unwrap();
    req.headers_mut()
        .insert("Authorization", format!("Bearer {token}").parse().unwrap());
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    let h = serde_json::to_string(&AgentMsg::Hello(hello())).unwrap();
    ws.send(Message::text(h)).await.unwrap();
    assert_eq!(next_json(&mut ws).await["type"], "welcome");
    ws
}

async fn send_report(ws: &mut Ws, r: Report) {
    let t = serde_json::to_string(&AgentMsg::Report(r)).unwrap();
    ws.send(Message::text(t)).await.unwrap();
}

async fn live(
    port: u16,
    cookie: Option<&str>,
) -> Result<Ws, tokio_tungstenite::tungstenite::Error> {
    let mut req = format!("ws://127.0.0.1:{port}/api/live")
        .into_client_request()
        .unwrap();
    if let Some(c) = cookie {
        req.headers_mut().insert("Cookie", c.parse().unwrap());
    }
    Ok(tokio_tungstenite::connect_async(req).await?.0)
}

async fn live_origin(port: u16, cookie: Option<&str>, origin: Option<&str>) -> Ws {
    let mut req = format!("ws://127.0.0.1:{port}/api/live")
        .into_client_request()
        .unwrap();
    if let Some(c) = cookie {
        req.headers_mut().insert("Cookie", c.parse().unwrap());
    }
    if let Some(o) = origin {
        req.headers_mut().insert("Origin", o.parse().unwrap());
    }
    tokio_tungstenite::connect_async(req).await.unwrap().0
}

/// 预建数据目录：`n` 个节点；node 0 设为 hidden 与否由参数决定，note 带标记。返回 (目录, [(id, token)])。
fn seed(n: usize, hide_first: bool) -> (tempfile::TempDir, Vec<(i64, String)>) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.db");
    let db = Db::open(&path).unwrap();
    let toks: Vec<_> = (0..n)
        .map(|i| {
            let (node, t) = db.add_node(&format!("n{i}")).unwrap();
            (node.id, t)
        })
        .collect();
    drop(db);
    let c = rusqlite::Connection::open(&path).unwrap();
    c.execute("UPDATE nodes SET note = 'secret-note'", [])
        .unwrap();
    if hide_first {
        c.execute("UPDATE nodes SET hidden = 1 WHERE name = 'n0'", [])
            .unwrap();
    }
    (dir, toks)
}

async fn boot(dir: tempfile::TempDir) -> Fixture {
    start_at(dir, |_| {}, |_| {}).await
}

async fn nodes(port: u16, cookie: Option<&str>) -> (u16, String) {
    let (c, _, b) = common::http(port, "GET", "/api/nodes", cookie, None).await;
    (c, b)
}

async fn wait_nodes(port: u16, cookie: Option<&str>, ok: impl Fn(&Value) -> bool) -> Value {
    let mut v = Value::Null;
    for _ in 0..50 {
        let (_, b) = nodes(port, cookie).await;
        v = serde_json::from_str(&b).unwrap_or(Value::Null);
        if ok(&v) {
            return v;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("等待条件超时: {v}");
}

#[tokio::test]
async fn guest_and_admin_key_sets() {
    let (dir, toks) = seed(1, false);
    let f = boot(dir).await;
    let mut a = agent(f.port, &toks[0].1).await;
    send_report(&mut a, report(now_ms(), 1.0)).await;
    wait_nodes(f.port, None, |v| v[0]["latest"].is_object()).await;
    let (_, body) = nodes(f.port, None).await;
    for banned in [
        "secret-host",
        "secret-kernel",
        "secret-note",
        "9.9.9",
        "127.0.0.1",
    ] {
        assert!(!body.contains(banned), "访客泄露 {banned}: {body}");
    }
    let v: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(keys(&v[0]), sorted(PUBLIC_KEYS));
    let cookie = login(f.port).await;
    let (_, body) = nodes(f.port, Some(&cookie)).await;
    let v: Value = serde_json::from_str(&body).unwrap();
    let mut all = PUBLIC_KEYS.to_vec();
    all.extend_from_slice(ADMIN_EXTRA);
    assert_eq!(keys(&v[0]), sorted(&all));
    assert_eq!(v[0]["hostname"], "secret-host");
    assert_eq!(v[0]["note"], "secret-note");
    assert_eq!(v[0]["hidden"], false);
    let mut adm = live(f.port, Some(&cookie)).await.unwrap();
    let snap = next_json(&mut adm).await;
    assert_eq!(keys(&snap), sorted(&["type", "nodes"]));
    assert_eq!(keys(&snap["nodes"][0]), sorted(&all));
}

#[tokio::test]
async fn hidden_visible_to_admin_only() {
    let (dir, _) = seed(2, true);
    let f = boot(dir).await;
    let (_, b) = nodes(f.port, None).await;
    let v: Value = serde_json::from_str(&b).unwrap();
    assert_eq!(v.as_array().unwrap().len(), 1);
    assert_eq!(v[0]["name"], "n1");
    let cookie = login(f.port).await;
    let (_, b) = nodes(f.port, Some(&cookie)).await;
    let v: Value = serde_json::from_str(&b).unwrap();
    assert_eq!(v.as_array().unwrap().len(), 2);
    assert_eq!(v[0]["hidden"], true);
}

#[tokio::test]
async fn live_snapshot_per_viewer() {
    let (dir, toks) = seed(2, true);
    let f = boot(dir).await;
    let mut g = live(f.port, None).await.unwrap();
    let snap = next_json(&mut g).await;
    let text = snap.to_string();
    assert_eq!(snap["nodes"].as_array().unwrap().len(), 1);
    assert_eq!(keys(&snap["nodes"][0]), sorted(PUBLIC_KEYS));
    for banned in ["hostname", "secret-note", "kernel"] {
        assert!(!text.contains(banned), "{text}");
    }
    let cookie = login(f.port).await;
    let mut adm = live(f.port, Some(&cookie)).await.unwrap();
    let snap = next_json(&mut adm).await;
    assert_eq!(snap["nodes"].as_array().unwrap().len(), 2);
    assert!(snap["nodes"][0].get("hostname").is_some());
    // 隐藏节点的事件：管理员收到，访客收不到
    let _h = agent(f.port, &toks[0].1).await;
    assert_eq!(next_json(&mut adm).await["type"], "online");
    let _s = agent(f.port, &toks[1].1).await;
    assert_eq!(next_json(&mut adm).await["node_id"], toks[1].0);
    assert_eq!(next_json(&mut g).await["node_id"], toks[1].0);
}

#[tokio::test]
async fn offline_node_keeps_last_report_across_restart() {
    let (dir, toks) = seed(1, false);
    let f = boot(dir).await;
    let ts = now_ms();
    let mut a = agent(f.port, &toks[0].1).await;
    send_report(&mut a, report(ts, 42.0)).await;
    wait_nodes(f.port, None, |v| v[0]["latest"]["cpu"] == 42.0).await;
    a.close(None).await.unwrap();
    drop(a);
    let v = wait_nodes(f.port, None, |v| v[0]["online"] == false).await;
    assert_eq!(v[0]["latest"]["cpu"], 42.0);
    assert!(v[0]["last_seen_ms"].as_i64().unwrap() >= ts);
    let dir = f.stop().await;
    let f2 = boot(dir).await;
    let (_, b) = nodes(f2.port, None).await;
    let v: Value = serde_json::from_str(&b).unwrap();
    assert_eq!(v[0]["online"], false);
    assert_eq!(v[0]["latest"]["cpu"], 42.0);
    assert_eq!(v[0]["last_seen_ms"].as_i64().unwrap(), ts);
}

#[tokio::test]
async fn last_report_falls_back_to_aggregates() {
    let (dir, toks) = seed(2, false);
    {
        let c = rusqlite::Connection::open(dir.path().join("t.db")).unwrap();
        c.execute(
            "INSERT INTO metrics_15m (node_id, ts_ms, samples, cpu, mem_used) VALUES (?1, 900000, 3, 33.0, 10.0)",
            [toks[0].0],
        )
        .unwrap();
    }
    let f = boot(dir).await;
    let (_, b) = nodes(f.port, None).await;
    let v: Value = serde_json::from_str(&b).unwrap();
    assert_eq!(v[0]["latest"]["cpu"], 33.0);
    assert!(v[1]["latest"].is_null());
}

#[tokio::test]
async fn agent_ip_recorded() {
    let (dir, toks) = seed(1, false);
    let f = boot(dir).await;
    let cookie = login(f.port).await;
    let v = wait_nodes(f.port, Some(&cookie), |v| v[0]["ip"].is_null()).await;
    assert!(v[0]["ip"].is_null());
    let _a = agent(f.port, &toks[0].1).await;
    let v = wait_nodes(f.port, Some(&cookie), |v| v[0]["ip"] == "127.0.0.1").await;
    assert_eq!(v[0]["ip"], "127.0.0.1");
}

#[tokio::test]
async fn require_login_blocks_guest() {
    let (dir, _) = seed(1, false);
    {
        let db = Db::open(&dir.path().join("t.db")).unwrap();
        db.set_setting("require_login", "1").unwrap();
    }
    let f = boot(dir).await;
    assert_eq!(nodes(f.port, None).await.0, 401);
    match live(f.port, None).await {
        Err(tokio_tungstenite::tungstenite::Error::Http(r)) => assert_eq!(r.status(), 401),
        other => panic!("访客 live 应在升级前 401: {:?}", other.map(|_| ())),
    }
    let cookie = login(f.port).await;
    assert_eq!(nodes(f.port, Some(&cookie)).await.0, 200);
    let mut adm = live(f.port, Some(&cookie)).await.unwrap();
    assert_eq!(next_json(&mut adm).await["type"], "snapshot");
}

#[tokio::test]
async fn missing_require_login_setting_means_off() {
    let (dir, _) = seed(1, false);
    let f = boot(dir).await;
    assert_eq!(nodes(f.port, None).await.0, 200);
}

#[tokio::test]
async fn resync_closes_guest_when_login_required_and_resends_for_admin() {
    let (dir, _) = seed(1, false);
    let f = boot(dir).await;
    let cookie = login(f.port).await;
    let mut g = live(f.port, None).await.unwrap();
    let mut adm = live(f.port, Some(&cookie)).await.unwrap();
    assert_eq!(next_json(&mut g).await["type"], "snapshot");
    assert_eq!(next_json(&mut adm).await["type"], "snapshot");
    // 未开启整站登录：Resync 只重发快照
    f.state.publish_resync();
    assert_eq!(next_json(&mut g).await["type"], "snapshot");
    assert_eq!(next_json(&mut adm).await["type"], "snapshot");
    f.state.set_require_login(true);
    f.state.publish_resync();
    assert_eq!(next_json(&mut adm).await["type"], "snapshot");
    let closed = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match g.next().await {
                None | Some(Err(_)) | Some(Ok(Message::Close(_))) => return,
                Some(Ok(m)) => assert!(
                    !matches!(m, Message::Text(_)),
                    "关闭前不得再收到数据: {m:?}"
                ),
            }
        }
    })
    .await;
    assert!(closed.is_ok(), "访客连接应被关闭");
}

#[tokio::test]
async fn live_origin_check_downgrades_cross_site_admin() {
    let (dir, _) = seed(2, true);
    let f = boot(dir).await;
    let cookie = login(f.port).await;
    let mut all = PUBLIC_KEYS.to_vec();
    all.extend_from_slice(ADMIN_EXTRA);
    // 跨站 Origin：降级为访客（公开字段集，无隐藏节点）
    let mut w = live_origin(f.port, Some(&cookie), Some("https://evil.example.com")).await;
    let snap = next_json(&mut w).await;
    assert_eq!(snap["nodes"].as_array().unwrap().len(), 1);
    assert_eq!(keys(&snap["nodes"][0]), sorted(PUBLIC_KEYS));
    // 同源 Origin（Host 为 127.0.0.1:port，大小写不敏感）：管理员
    let same = format!("http://127.0.0.1:{}", f.port);
    let mut w = live_origin(f.port, Some(&cookie), Some(&same)).await;
    let snap = next_json(&mut w).await;
    assert_eq!(snap["nodes"].as_array().unwrap().len(), 2);
    assert_eq!(keys(&snap["nodes"][0]), sorted(&all));
    // 无 Origin（非浏览器）：管理员
    let mut w = live_origin(f.port, Some(&cookie), None).await;
    let snap = next_json(&mut w).await;
    assert_eq!(keys(&snap["nodes"][0]), sorted(&all));
}

#[tokio::test]
async fn live_origin_accepts_public_url_host() {
    let (dir, _) = seed(2, true);
    let f = start_at(
        dir,
        |_| {},
        |c| {
            c.public_url = Some("https://Mon.Example.com".into());
        },
    )
    .await;
    let cookie = login(f.port).await;
    let mut w = live_origin(f.port, Some(&cookie), Some("https://mon.example.com")).await;
    let snap = next_json(&mut w).await;
    assert_eq!(snap["nodes"].as_array().unwrap().len(), 2);
}

async fn send_netinfo(ws: &mut Ws, n: NetInfo) {
    let t = serde_json::to_string(&AgentMsg::NetInfo(n)).unwrap();
    ws.send(Message::text(t)).await.unwrap();
}

#[tokio::test]
async fn netinfo_saved_and_shown() {
    let (dir, toks) = seed(1, false);
    let f = boot(dir).await;
    let mut a = agent(f.port, &toks[0].1).await;
    send_netinfo(
        &mut a,
        NetInfo {
            ipv4: Some("203.0.113.5".into()),
            ipv6: Some("2001:db8::5".into()),
            country: Some("jp".into()),
        },
    )
    .await;
    let cookie = login(f.port).await;
    let v = wait_nodes(f.port, Some(&cookie), |v| v[0]["ipv4"] == "203.0.113.5").await;
    assert_eq!(v[0]["ipv6"], "2001:db8::5");
    assert_eq!(v[0]["region"], "JP");
    // guest_sees_country_not_ip
    let (_, body) = nodes(f.port, None).await;
    assert!(
        !body.contains("203.0.113.5") && !body.contains("2001:db8::5"),
        "{body}"
    );
    let g: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(g[0]["region"], "JP");
    assert_eq!(keys(&g[0]), sorted(PUBLIC_KEYS));
    let mut l = live(f.port, None).await.unwrap();
    let snap = next_json(&mut l).await;
    assert_eq!(keys(&snap["nodes"][0]), sorted(PUBLIC_KEYS));
    assert_eq!(snap["nodes"][0]["region"], "JP");
}

#[tokio::test]
async fn netinfo_invalid_fields_dropped() {
    let (dir, toks) = seed(1, false);
    let f = boot(dir).await;
    let mut a = agent(f.port, &toks[0].1).await;
    send_netinfo(
        &mut a,
        NetInfo {
            ipv4: Some("::1".into()),
            ipv6: Some("1.2.3.4".into()),
            country: Some("usa".into()),
        },
    )
    .await;
    // 连接仍在：随后的上报照常被接受
    send_report(&mut a, report(now_ms(), 7.0)).await;
    let cookie = login(f.port).await;
    let v = wait_nodes(f.port, Some(&cookie), |v| v[0]["latest"].is_object()).await;
    assert!(v[0]["ipv4"].is_null() && v[0]["ipv6"].is_null(), "{v}");
    assert_eq!(v[0]["region"], "");
}
