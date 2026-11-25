//! 探测历史接口的访问规则与返回结构。

mod common;

use common::{http, login, start_at};
use minimon_server::db::Db;
use serde_json::{Value, json};

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

const SECRET: &str = "secret-target.example.com";

/// n0 可见（id 1）、n1 隐藏（id 2）；任务 1 全部节点、任务 2 只属于 n1、任务 3 只属于 n0（排序在前）。
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
    for (id, name, all, sort) in [(1, "电信", 1, 2), (2, "联通", 0, 3), (3, "移动", 0, 1)] {
        c.execute(
            "INSERT INTO probe_tasks(id,name,kind,target,interval_secs,samples,timeout_ms,all_nodes,show_on_card,sort,created_ms) \
             VALUES (?1,?2,'icmp',?3,60,5,5000,?4,1,?5,0)",
            rusqlite::params![id, name, SECRET, all, sort],
        )
        .unwrap();
    }
    c.execute("INSERT INTO probe_task_nodes VALUES (2, 2), (3, 1)", [])
        .unwrap();
    let now = now_ms();
    for (t, n, ts, sent, lost, avg) in [
        (1, 1, now - 2000, 5, 1, Some(12.5)),
        (1, 1, now - 1000, 5, 5, None),
        (2, 1, now - 1000, 5, 0, Some(1.0)),
        (1, 2, now - 1000, 5, 0, Some(9.0)),
    ] {
        c.execute(
            "INSERT INTO probe_raw(task_id,node_id,ts_ms,sent,lost,avg_ms) VALUES (?1,?2,?3,?4,?5,?6)",
            rusqlite::params![t, n, ts, sent, lost, avg],
        )
        .unwrap();
    }
    c.execute(
        "INSERT INTO probe_5m(task_id,node_id,ts_ms,sent,lost,avg_ms,samples) VALUES (1,1,?1,10,0,20.0,2)",
        [now - 600_000],
    )
    .unwrap();
    dir
}

#[tokio::test]
async fn probes_history_rules() {
    let f = start_at(seed(), |_| {}, |_| {}).await;
    let p = f.port;

    let (c, _, b) = http(p, "GET", "/api/nodes/1/probes?range=1h", None, None).await;
    assert_eq!(c, 200, "{b}");
    assert!(!b.contains("secret-target") && !b.contains("target"), "{b}");
    let v: Value = serde_json::from_str(&b).unwrap();
    assert_eq!(v["granularity"], "raw");
    // 只含适用任务，按 sort,id：移动(3, sort1)、电信(1, sort2)；任务 2 不属于 n0。
    let tasks = v["tasks"].as_array().unwrap();
    assert_eq!(tasks.len(), 2);
    assert_eq!(
        (tasks[0]["task_id"].as_i64(), tasks[0]["name"].as_str()),
        (Some(3), Some("移动"))
    );
    assert_eq!(tasks[0]["ts"].as_array().unwrap().len(), 0);
    let t1 = &tasks[1];
    assert_eq!(t1["task_id"], 1);
    assert_eq!(t1["avg_ms"], json!([12.5, null]));
    assert_eq!(t1["loss"], json!([0.2, 1.0]));
    let mut ks: Vec<_> = t1.as_object().unwrap().keys().cloned().collect();
    ks.sort();
    assert_eq!(ks, ["avg_ms", "loss", "name", "task_id", "ts"]);

    let (c, _, b) = http(p, "GET", "/api/nodes/1/probes?range=24h", None, None).await;
    assert_eq!(c, 200);
    let v: Value = serde_json::from_str(&b).unwrap();
    assert_eq!(v["granularity"], "5m");
    assert_eq!(v["tasks"][1]["avg_ms"], json!([20.0]));
    assert_eq!(v["tasks"][1]["loss"], json!([0.0]));

    let (hc, _, hb) = http(p, "GET", "/api/nodes/2/probes?range=1h", None, None).await;
    let (uc, _, ub) = http(p, "GET", "/api/nodes/999/probes?range=1h", None, None).await;
    let (nc, _, nb) = http(p, "GET", "/api/nodes/abc/probes?range=1h", None, None).await;
    assert_eq!((hc, uc, nc), (404, 404, 404));
    assert_eq!(hb, ub);
    assert_eq!(hb, nb);
    assert!(hb.contains("节点不存在或未公开"));

    for q in ["range=bad", "", "range="] {
        let (c, _, _) = http(p, "GET", &format!("/api/nodes/1/probes?{q}"), None, None).await;
        assert_eq!(c, 400, "{q}");
    }

    // 管理员可看隐藏节点，且只含适用于它的任务（1 与 2）。
    let ck = login(p).await;
    let (c, _, b) = http(p, "GET", "/api/nodes/2/probes?range=1h", Some(&ck), None).await;
    assert_eq!(c, 200);
    let v: Value = serde_json::from_str(&b).unwrap();
    let ids: Vec<i64> = v["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["task_id"].as_i64().unwrap())
        .collect();
    assert_eq!(ids, [1, 2]);

    f.state.set_require_login(true);
    let (c, _, _) = http(p, "GET", "/api/nodes/1/probes?range=1h", None, None).await;
    assert_eq!(c, 401);
    let (c, _, _) = http(p, "GET", "/api/nodes/1/probes?range=1h", Some(&ck), None).await;
    assert_eq!(c, 200);
}
