//! 历史查询接口的访问规则。

mod common;

use common::{http, login, start_at};
use minimon_server::db::Db;
use serde_json::Value;

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

/// n0 可见（id 1）、n1 隐藏（id 2）；n0 有一行 raw 数据。
fn seed() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.db");
    let db = Db::open(&path).unwrap();
    db.add_node("n0").unwrap();
    db.add_node("n1").unwrap();
    drop(db);
    let c = rusqlite::Connection::open(&path).unwrap();
    c.execute("UPDATE nodes SET hidden = 1 WHERE name = 'n1'", [])
        .unwrap();
    c.execute(
        "INSERT INTO metrics_raw (node_id, ts_ms, cpu) VALUES (1, ?1, 12.5)",
        [now_ms() - 1000],
    )
    .unwrap();
    dir
}

#[tokio::test]
async fn history_http_rules() {
    let f = start_at(seed(), |_| {}, |_| {}).await;
    let p = f.port;

    let (c, _, b) = http(p, "GET", "/api/nodes/1/history?range=1h", None, None).await;
    assert_eq!(c, 200, "{b}");
    let v: Value = serde_json::from_str(&b).unwrap();
    assert_eq!(v["granularity"], "raw");
    assert_eq!(v["ts"].as_array().unwrap().len(), 1);
    assert_eq!(v["cpu"][0], 12.5);
    let (c, _, b) = http(p, "GET", "/api/nodes/1/history?range=30d", None, None).await;
    assert_eq!(c, 200);
    assert!(b.contains(r#""granularity":"15m""#));

    let (hc, _, hb) = http(p, "GET", "/api/nodes/2/history?range=1h", None, None).await;
    let (uc, _, ub) = http(p, "GET", "/api/nodes/999/history?range=1h", None, None).await;
    assert_eq!((hc, uc), (404, 404));
    assert_eq!(hb, ub);
    assert!(hb.contains("节点不存在或未公开"));

    for q in ["range=bad", "", "range="] {
        let (c, _, _) = http(p, "GET", &format!("/api/nodes/1/history?{q}"), None, None).await;
        assert_eq!(c, 400, "{q}");
    }

    let cookie = login(p).await;
    let (c, _, _) = http(
        p,
        "GET",
        "/api/nodes/2/history?range=1h",
        Some(&cookie),
        None,
    )
    .await;
    assert_eq!(c, 200);

    f.state.set_require_login(true);
    let (c, _, _) = http(p, "GET", "/api/nodes/1/history?range=1h", None, None).await;
    assert_eq!(c, 401);
    let (c, _, _) = http(
        p,
        "GET",
        "/api/nodes/1/history?range=1h",
        Some(&cookie),
        None,
    )
    .await;
    assert_eq!(c, 200);
}
