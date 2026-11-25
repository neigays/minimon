//! 月流量与到期日：管理员视图、live 事件过滤、配置变更后的即时重算、输入校验。

mod common;

use common::{Fixture, http, login, start_at};
use futures_util::StreamExt;
use minimon_common::protocol::Report;
use minimon_server::db::Db;
use minimon_server::period::{parse_tz, period_bounds};
use serde_json::{Value, json};
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

fn report(ts_ms: i64, rx: u64, tx: u64) -> Report {
    Report {
        ts_ms,
        cpu: 1.0,
        mem_used: 1,
        swap_used: 0,
        disk_used: 1,
        disk_total: 2,
        net_rx_bytes: rx,
        net_tx_bytes: tx,
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

/// 一个节点，带 3 条 raw（累计计数 1000 -> 3000 -> 6000，rx 增量 5000）。返回 (Fixture, node_id)。
async fn boot() -> (Fixture, i64) {
    let id = std::sync::Arc::new(std::sync::atomic::AtomicI64::new(0));
    let id2 = id.clone();
    let f = start_at(
        tempfile::tempdir().unwrap(),
        move |db: &Db| {
            let (n, _) = db.add_node("n0").unwrap();
            let now = now_ms();
            db.insert_reports(&[
                (n.id, report(now - 20_000, 1000, 100)),
                (n.id, report(now - 15_000, 3000, 200)),
                (n.id, report(now - 10_000, 6000, 400)),
            ])
            .unwrap();
            id2.store(n.id, std::sync::atomic::Ordering::SeqCst);
        },
        |_| {},
    )
    .await;
    let id = id.load(std::sync::atomic::Ordering::SeqCst);
    (f, id)
}

async fn nodes(port: u16, cookie: Option<&str>) -> Value {
    let (c, _, b) = http(port, "GET", "/api/nodes", cookie, None).await;
    assert_eq!(c, 200, "{b}");
    serde_json::from_str(&b).unwrap()
}

async fn wait_traffic(port: u16, cookie: &str, ok: impl Fn(&Value) -> bool) -> Value {
    for _ in 0..50 {
        let v = nodes(port, Some(cookie)).await;
        if ok(&v[0]["traffic"]) {
            return v;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("等待 traffic 超时");
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

async fn patch(port: u16, cookie: &str, id: i64, body: Value) -> (u16, String) {
    let (c, _, b) = http(
        port,
        "PATCH",
        &format!("/api/admin/nodes/{id}"),
        Some(cookie),
        Some(&body.to_string()),
    )
    .await;
    (c, b)
}

#[tokio::test]
async fn admin_sees_traffic_guest_does_not() {
    let (f, id) = boot().await;
    let cookie = login(f.port).await;
    assert_eq!(
        patch(
            f.port,
            &cookie,
            id,
            json!({"traffic_quota": 500_000_000_000i64, "expire_date": "2099-01-01"})
        )
        .await
        .0,
        200
    );
    let v = wait_traffic(f.port, &cookie, |t| t["rx"] == 5000).await;
    let t = &v[0]["traffic"];
    assert_eq!(t["tx"], 300);
    assert_eq!(t["used"], 5300);
    assert_eq!(t["quota"], 500_000_000_000i64);
    assert_eq!(t["mode"], "sum");
    assert_eq!(t["reset_day"], 1);
    assert!(t["period_start_ms"].as_i64().unwrap() < t["period_end_ms"].as_i64().unwrap());
    assert_eq!(v[0]["expire_date"], "2099-01-01");
    assert!(v[0]["expire_days"].as_i64().unwrap() > 0);

    let guest = nodes(f.port, None).await;
    let obj = guest[0].as_object().unwrap();
    for k in ["traffic", "expire_date", "expire_days"] {
        assert!(!obj.contains_key(k), "访客不应有 {k}");
    }
    let (_, _, body) = http(f.port, "GET", "/api/nodes", None, None).await;
    assert!(!body.contains("2099-01-01") && !body.contains("quota"));
}

#[tokio::test]
async fn patch_reset_day_recomputes_immediately() {
    let (f, id) = boot().await;
    let cookie = login(f.port).await;
    let mut admin = live(f.port, Some(&cookie)).await;
    let mut guest = live(f.port, None).await;
    let tz = parse_tz("Asia/Shanghai").unwrap();
    let want = period_bounds(15, &tz, now_ms()).0;
    assert_ne!(want, period_bounds(1, &tz, now_ms()).0);
    assert_eq!(
        patch(f.port, &cookie, id, json!({"traffic_reset_day": 15}))
            .await
            .0,
        200
    );

    let got = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let Some(Ok(Message::Text(t))) = admin.next().await else {
                panic!("admin live 结束");
            };
            let v: Value = serde_json::from_str(&t).unwrap();
            if v["type"] == "traffic" && v["period_start_ms"] == want {
                return v;
            }
        }
    })
    .await
    .expect("2 秒内应收到新账期的 traffic 事件");
    assert_eq!(got["node_id"], id);
    for k in ["used", "rx", "tx", "quota", "period_end_ms"] {
        assert!(got.get(k).is_some(), "缺少 {k}");
    }

    // 访客同期绝不收到 traffic（先看到 admin 的事件之后，再给访客一小段时间）
    let _ = tokio::time::timeout(Duration::from_millis(500), async {
        while let Some(Ok(m)) = guest.next().await {
            if let Message::Text(t) = m {
                let v: Value = serde_json::from_str(&t).unwrap();
                assert_ne!(v["type"], "traffic", "访客收到了 traffic: {v}");
            }
        }
    })
    .await;
}

#[tokio::test]
async fn timezone_change_recomputes_all() {
    let (f, _) = boot().await;
    let cookie = login(f.port).await;
    let (c, _, b) = http(f.port, "GET", "/api/admin/settings", Some(&cookie), None).await;
    assert_eq!(c, 200);
    assert_eq!(
        serde_json::from_str::<Value>(&b).unwrap()["timezone"],
        "Asia/Shanghai"
    );
    let v = wait_traffic(f.port, &cookie, |t| t.is_object()).await;
    let sh = v[0]["traffic"]["period_start_ms"].as_i64().unwrap();
    let (c, _, b) = http(
        f.port,
        "PUT",
        "/api/admin/settings",
        Some(&cookie),
        Some(r#"{"timezone":"UTC"}"#),
    )
    .await;
    assert_eq!(c, 200, "{b}");
    assert_eq!(
        serde_json::from_str::<Value>(&b).unwrap()["timezone"],
        "UTC"
    );
    let v = wait_traffic(f.port, &cookie, |t| t["period_start_ms"] != sh).await;
    let utc = v[0]["traffic"]["period_start_ms"].as_i64().unwrap();
    assert_eq!(utc - sh, 8 * 3_600_000);
}

#[tokio::test]
async fn patch_tri_state_quota_and_expire() {
    let (f, id) = boot().await;
    let cookie = login(f.port).await;
    let (_, b) = patch(
        f.port,
        &cookie,
        id,
        json!({"traffic_quota": 100, "expire_date": "2099-03-04"}),
    )
    .await;
    let v: Value = serde_json::from_str(&b).unwrap();
    assert_eq!(v["expire_date"], "2099-03-04");
    // 缺省 = 不改
    let (_, b) = patch(f.port, &cookie, id, json!({"note": "x"})).await;
    let v: Value = serde_json::from_str(&b).unwrap();
    assert_eq!(v["expire_date"], "2099-03-04");
    let v = wait_traffic(f.port, &cookie, |t| t["quota"] == 100).await;
    assert_eq!(v[0]["traffic"]["quota"], 100);
    // null = 清除
    let (c, b) = patch(
        f.port,
        &cookie,
        id,
        json!({"traffic_quota": null, "expire_date": null}),
    )
    .await;
    assert_eq!(c, 200, "{b}");
    let v: Value = serde_json::from_str(&b).unwrap();
    assert!(v["expire_date"].is_null() && v["expire_days"].is_null());
    let v = wait_traffic(f.port, &cookie, |t| t["quota"].is_null()).await;
    assert!(v[0]["traffic"]["quota"].is_null());
}

#[tokio::test]
async fn invalid_inputs_400() {
    let (f, id) = boot().await;
    let cookie = login(f.port).await;
    for body in [
        json!({"traffic_reset_day": 0}),
        json!({"traffic_reset_day": 32}),
        json!({"traffic_mode": "foo"}),
        json!({"expire_date": "2027-02-30"}),
        json!({"expire_date": "2027-2-3"}),
        json!({"traffic_quota": -1}),
    ] {
        let (c, b) = patch(f.port, &cookie, id, body.clone()).await;
        assert_eq!(c, 400, "{body} -> {b}");
    }
    let (c, _, b) = http(
        f.port,
        "PUT",
        "/api/admin/settings",
        Some(&cookie),
        Some(r#"{"timezone":"Mars/X"}"#),
    )
    .await;
    assert_eq!(c, 400);
    assert!(b.contains("未知时区"), "{b}");
    // 非法请求不留下副作用
    let v = nodes(f.port, Some(&cookie)).await;
    assert_eq!(v[0]["traffic"]["reset_day"].as_i64().unwrap_or(1), 1);
}
