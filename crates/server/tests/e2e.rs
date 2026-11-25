use futures_util::StreamExt;
use minimon_agent::AgentConfig;
use minimon_server::config::ServerConfig;
use minimon_server::db::Db;
use minimon_server::state::AppState;
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_tungstenite::tungstenite::Message;

async fn next_json<S>(ws: &mut S) -> Value
where
    S: futures_util::Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    loop {
        let m = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("等待 live 消息超时")
            .unwrap()
            .unwrap();
        if let Message::Text(t) = m {
            return serde_json::from_str(&t).unwrap();
        }
    }
}

async fn http_get(port: u16, path: &str) -> String {
    let mut s = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .unwrap();
    let req = format!("GET {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n");
    s.write_all(req.as_bytes()).await.unwrap();
    let mut buf = Vec::new();
    s.read_to_end(&mut buf).await.unwrap();
    let text = String::from_utf8(buf).unwrap();
    text.split_once("\r\n\r\n").unwrap().1.to_owned()
}

#[tokio::test]
async fn agent_to_server_end_to_end() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("e2e.db");
    let db = Arc::new(Db::open(&path).unwrap());
    let (node, token) = db.add_node("e2e").unwrap();
    let cfg = ServerConfig {
        report_interval_secs: 1,
        ..ServerConfig::default()
    };
    let (state, ingest) = AppState::new(db, cfg).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(minimon_server::serve_on(listener, state, ingest, async {
        let _ = stop_rx.await;
    }));

    let (mut live, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}/api/live"))
        .await
        .unwrap();
    assert_eq!(next_json(&mut live).await["type"], "snapshot");

    let mut acfg = AgentConfig::new(format!("ws://127.0.0.1:{port}"), token);
    acfg.backoff_base = Duration::from_millis(100);
    acfg.backoff_max = Duration::from_millis(500);
    let (agent_stop_tx, agent_stop_rx) = tokio::sync::oneshot::channel::<()>();
    let agent = tokio::spawn(minimon_agent::run(acfg, async {
        let _ = agent_stop_rx.await;
    }));

    let mut last = String::new();
    let mut latest = Value::Null;
    let ok = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            last = http_get(port, "/api/nodes").await;
            if let Ok(v) = serde_json::from_str::<Value>(&last)
                && v[0]["online"] == true
                && v[0]["latest"].is_object()
            {
                latest = v[0]["latest"].clone();
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await;
    assert!(ok.is_ok(), "5 秒内节点未在线并带有 latest: {last}");

    let cpu = latest["cpu"].as_f64().expect("cpu 应为数字");
    assert!((0.0..=100.0).contains(&cpu), "cpu 越界: {cpu}");
    assert!(latest["mem_used"].as_u64().unwrap() > 0, "{latest}");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    let ts = latest["ts_ms"].as_i64().unwrap();
    assert!((now - ts).abs() < 30_000, "ts_ms 不新鲜: {ts} vs {now}");

    // 卡片墙走的 /api/live：必须收到该节点的 report
    loop {
        let m = next_json(&mut live).await;
        if m["type"] == "report" {
            assert_eq!(m["node_id"], node.id);
            assert!(m["report"]["mem_used"].as_u64().unwrap() > 0);
            break;
        }
    }

    agent_stop_tx.send(()).unwrap();
    let _ = tokio::time::timeout(Duration::from_secs(5), agent)
        .await
        .expect("agent 未在时限内退出");

    stop_tx.send(()).unwrap();
    let res = tokio::time::timeout(Duration::from_secs(10), server)
        .await
        .expect("serve_on 未在时限内返回")
        .unwrap();
    assert!(res.is_ok());

    let c = rusqlite::Connection::open(&path).unwrap();
    let n: i64 = c
        .query_row(
            "SELECT count(*) FROM metrics_raw WHERE node_id=?1",
            [node.id],
            |r| r.get(0),
        )
        .unwrap();
    assert!(n >= 1, "metrics_raw 无数据");
}

mod common;

use common::{http, login, start};
use minimon_server::db::ROLLUP_LAG_MS;

async fn json_of(
    port: u16,
    method: &str,
    path: &str,
    cookie: Option<&str>,
    body: Option<&str>,
) -> (u16, Value) {
    let (code, _, text) = http(port, method, path, cookie, body).await;
    (code, serde_json::from_str(&text).unwrap_or(Value::Null))
}

fn find(list: &Value, id: i64) -> Option<&Value> {
    list.as_array()?.iter().find(|n| n["id"] == id)
}

#[tokio::test]
async fn admin_flow_end_to_end() {
    let f = start(|c| c.report_interval_secs = 1).await;
    let port = f.port;
    let cookie = login(port).await;

    // 创建节点拿 token，用真实 agent 连接
    let (code, v) = json_of(
        port,
        "POST",
        "/api/admin/nodes",
        Some(&cookie),
        Some(r#"{"name":"e2e-admin"}"#),
    )
    .await;
    assert_eq!(code, 201, "{v}");
    let id = v["node"]["id"].as_i64().unwrap();
    let token = v["token"].as_str().unwrap().to_string();

    let mut acfg = AgentConfig::new(format!("ws://127.0.0.1:{port}"), token);
    acfg.backoff_base = Duration::from_millis(100);
    acfg.backoff_max = Duration::from_millis(500);
    let (agent_stop_tx, agent_stop_rx) = tokio::sync::oneshot::channel::<()>();
    let agent = tokio::spawn(minimon_agent::run(acfg, async {
        let _ = agent_stop_rx.await;
    }));

    // 管理员视图：online 且有 ip
    let mut last = Value::Null;
    let ok = tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let (_, v) = json_of(port, "GET", "/api/nodes", Some(&cookie), None).await;
            if let Some(n) = find(&v, id)
                && n["online"] == true
                && n["latest"].is_object()
                && n["ip"].is_string()
            {
                return;
            }
            last = v;
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await;
    assert!(ok.is_ok(), "管理员视图未见在线且带 ip: {last}");

    // 访客视图：无 ip 键
    let (_, guest) = json_of(port, "GET", "/api/nodes", None, None).await;
    let gn = find(&guest, id).expect("访客应看到该节点");
    assert!(gn.get("ip").is_none(), "访客不得看到 ip: {gn}");

    // 入库每 10 秒 flush 一次，最多等 15 秒
    let path = format!("/api/nodes/{id}/history?range=1h");
    let mut hist = Value::Null;
    let ok = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let (_, v) = json_of(port, "GET", &path, None, None).await;
            if v["ts"].as_array().is_some_and(|a| !a.is_empty()) {
                hist = v;
                return;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    })
    .await;
    assert!(ok.is_ok(), "历史 ts 一直为空");
    let n = hist["ts"].as_array().unwrap().len();
    assert_eq!(hist["cpu"].as_array().unwrap().len(), n, "cpu 与 ts 应等长");

    // 手动触发降采样（测试专用：第二个 Db 连接同一文件）
    let db2 = Db::open(&f.db_path).unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    db2.rollup(now + 120_000, ROLLUP_LAG_MS, 50).unwrap();
    let (code, v) = json_of(
        port,
        "GET",
        &format!("/api/nodes/{id}/history?range=24h"),
        None,
        None,
    )
    .await;
    assert_eq!(code, 200, "{v}");
    assert_eq!(v["granularity"], "1m");
    assert!(
        !v["ts"].as_array().unwrap().is_empty(),
        "1m 聚合应非空: {v}"
    );

    // 隐藏 → 访客不可见
    let (code, _) = json_of(
        port,
        "PATCH",
        &format!("/api/admin/nodes/{id}"),
        Some(&cookie),
        Some(r#"{"hidden":true}"#),
    )
    .await;
    assert_eq!(code, 200);
    let (_, guest) = json_of(port, "GET", "/api/nodes", None, None).await;
    assert!(find(&guest, id).is_none(), "隐藏节点不应对访客可见");

    // 删除 → 会话被踢：先确认 agent 确实有活动会话，再订阅 live 事件
    assert!(
        f.state.registry().snapshot().iter().any(|(i, _)| *i == id),
        "删除前 agent 应有在线会话"
    );
    let mut events = f.state.live().subscribe();
    let (code, _, body) = http(
        port,
        "DELETE",
        &format!("/api/admin/nodes/{id}"),
        Some(&cookie),
        Some("{}"),
    )
    .await;
    assert_eq!(code, 204, "{body}");
    let got_offline = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match events.recv().await {
                Ok(minimon_server::state::LiveEvent::Offline { node_id }) if node_id == id => {
                    return;
                }
                Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(_) => panic!("live 通道已关闭"),
            }
        }
    })
    .await;
    assert!(got_offline.is_ok(), "删除后应收到该节点的 Offline 事件");
    let (_, admin) = json_of(port, "GET", "/api/nodes", Some(&cookie), None).await;
    assert!(find(&admin, id).is_none());
    // agent 会按退避重连（100~500ms），但旧 token 已失效，会话不得重新出现
    let deadline = tokio::time::Instant::now() + Duration::from_millis(3000);
    while tokio::time::Instant::now() < deadline {
        assert!(
            !f.state.registry().snapshot().iter().any(|(i, _)| *i == id),
            "删除后 agent 不应重新建立会话"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    let _ = agent_stop_tx.send(());
    let _ = tokio::time::timeout(Duration::from_secs(5), agent)
        .await
        .expect("agent 未退出");
    f.stop().await;
}

#[tokio::test]
async fn probe_and_traffic_end_to_end() {
    let f = start(|c| c.report_interval_secs = 1).await;
    let port = f.port;
    let cookie = login(port).await;

    // 本地 TCP 监听作为探测目标
    let target_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let target = format!("127.0.0.1:{}", target_listener.local_addr().unwrap().port());
    let accept = tokio::spawn(async move {
        loop {
            if target_listener.accept().await.is_err() {
                return;
            }
        }
    });

    let (code, v) = json_of(
        port,
        "POST",
        "/api/admin/nodes",
        Some(&cookie),
        Some(r#"{"name":"e2e-probe"}"#),
    )
    .await;
    assert_eq!(code, 201, "{v}");
    let id = v["node"]["id"].as_i64().unwrap();
    let token = v["token"].as_str().unwrap().to_string();

    let body = format!(
        r#"{{"name":"本地TCP","kind":"tcp","target":"{target}","interval_secs":10,"samples":1,"timeout_ms":2000}}"#
    );
    let (code, v) = json_of(
        port,
        "POST",
        "/api/admin/probes",
        Some(&cookie),
        Some(&body),
    )
    .await;
    assert_eq!(code, 201, "{v}");
    let task_id = v["id"]
        .as_i64()
        .unwrap_or_else(|| panic!("无法取得任务 id: {v}"));

    let mut acfg = AgentConfig::new(format!("ws://127.0.0.1:{port}"), token);
    acfg.backoff_base = Duration::from_millis(100);
    acfg.backoff_max = Duration::from_millis(500);
    let (agent_stop_tx, agent_stop_rx) = tokio::sync::oneshot::channel::<()>();
    let agent = tokio::spawn(minimon_agent::run(acfg, async {
        let _ = agent_stop_rx.await;
    }));

    // 首轮探测在 [0, 10s) 内随机触发，最多等 30 秒
    let mut guest_text = String::new();
    let ok = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let (_, _, text) = http(port, "GET", "/api/nodes", None, None).await;
            if let Ok(v) = serde_json::from_str::<Value>(&text)
                && let Some(n) = find(&v, id)
                && let Some(p) = n["probes"]
                    .as_array()
                    .and_then(|a| a.iter().find(|p| p["task_id"] == task_id))
                && p["avg_ms"].is_number()
            {
                assert_eq!(p["loss"].as_f64().unwrap(), 0.0, "{p}");
                guest_text = text;
                return;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    })
    .await;
    assert!(ok.is_ok(), "30 秒内访客视图未见探测结果");
    assert!(
        !guest_text.contains(&target),
        "访客响应不得包含探测目标: {guest_text}"
    );

    // 流量配置
    let (code, v) = json_of(
        port,
        "PATCH",
        &format!("/api/admin/nodes/{id}"),
        Some(&cookie),
        Some(r#"{"traffic_quota":1000000000000,"traffic_reset_day":15}"#),
    )
    .await;
    assert_eq!(code, 200, "{v}");
    let (_, admin) = json_of(port, "GET", "/api/nodes", Some(&cookie), None).await;
    let n = find(&admin, id).expect("管理员应看到节点");
    assert_eq!(n["traffic_quota"].as_i64(), Some(1_000_000_000_000), "{n}");
    assert_eq!(n["traffic_reset_day"].as_i64(), Some(15), "{n}");
    let t = &n["traffic"];
    assert!(t["used"].as_i64().unwrap() >= 0, "{t}");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    assert!(t["period_start_ms"].as_i64().unwrap() <= now, "{t}");
    assert!(now < t["period_end_ms"].as_i64().unwrap(), "{t}");
    let (_, guest) = json_of(port, "GET", "/api/nodes", None, None).await;
    let gn = find(&guest, id).unwrap();
    assert!(gn.get("traffic").is_none(), "访客不得看到流量: {gn}");

    // 探测历史（入库每 10 秒 flush，最多等 15 秒）
    let path = format!("/api/nodes/{id}/probes?range=1h");
    let mut hist = Value::Null;
    let ok = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let (code, text) = {
                let (c, _, t) = http(port, "GET", &path, None, None).await;
                (c, t)
            };
            assert_eq!(code, 200, "{text}");
            assert!(!text.contains(&target), "历史响应不得含目标: {text}");
            let v: Value = serde_json::from_str(&text).unwrap();
            if v["tasks"].as_array().is_some_and(|a| {
                a.iter().any(|t| {
                    t["task_id"] == task_id && t["ts"].as_array().is_some_and(|x| !x.is_empty())
                })
            }) {
                hist = v;
                return;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    })
    .await;
    assert!(ok.is_ok(), "探测历史一直无数据点");
    assert_eq!(hist["granularity"], "raw", "{hist}");

    accept.abort();
    let _ = agent_stop_tx.send(());
    let _ = tokio::time::timeout(Duration::from_secs(5), agent).await;
    f.stop().await;
}
