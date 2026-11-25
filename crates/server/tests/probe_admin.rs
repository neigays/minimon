//! 探测任务管理：校验、三网模板、级联删除、鉴权。

mod common;

use common::{Fixture, http, login, start};
use serde_json::{Value, json};

async fn post(f: &Fixture, ck: &str, path: &str, body: Value) -> (u16, Value) {
    let (c, _, b) = http(f.port, "POST", path, Some(ck), Some(&body.to_string())).await;
    (c, serde_json::from_str(&b).unwrap_or(Value::Null))
}

async fn get_list(f: &Fixture, ck: &str) -> Vec<Value> {
    let (c, _, b) = http(f.port, "GET", "/api/admin/probes", Some(ck), None).await;
    assert_eq!(c, 200, "{b}");
    serde_json::from_str(&b).unwrap()
}

fn count(f: &Fixture, sql: &str) -> i64 {
    let c = rusqlite::Connection::open(&f.db_path).unwrap();
    c.query_row(sql, [], |r| r.get(0)).unwrap()
}

fn exec(f: &Fixture, sql: &str) {
    let c = rusqlite::Connection::open(&f.db_path).unwrap();
    c.execute_batch(sql).unwrap();
}

async fn add_node(f: &Fixture, ck: &str, name: &str) -> i64 {
    let (c, v) = post(f, ck, "/api/admin/nodes", json!({ "name": name })).await;
    assert_eq!(c, 201);
    v["node"]["id"].as_i64().unwrap()
}

#[tokio::test]
async fn isp_template_creates_three_icmp_tasks() {
    let f = start(|_| {}).await;
    let ck = login(f.port).await;
    let (c, v) = post(
        &f,
        &ck,
        "/api/admin/probes/isp-template",
        json!({"telecom": "1.1.1.1", "unicom": "example.com", "mobile": "8.8.8.8"}),
    )
    .await;
    assert_eq!(c, 201, "{v}");
    let tasks = v.as_array().unwrap();
    let names: Vec<&str> = tasks.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["电信", "联通", "移动"]);
    for t in tasks {
        assert_eq!(t["kind"], "icmp");
        assert_eq!(t["samples"], 5);
        assert_eq!(t["interval_secs"], 60);
        assert_eq!(t["timeout_ms"], 5000);
        assert_eq!(t["all_nodes"], true);
        assert_eq!(t["show_on_card"], true);
    }
    assert_eq!(get_list(&f, &ck).await.len(), 3);

    let (c, v) = post(
        &f,
        &ck,
        "/api/admin/probes/isp-template",
        json!({"telecom": "  ", "unicom": "9.9.9.9", "mobile": "8.8.4.4"}),
    )
    .await;
    assert_eq!(c, 201, "{v}");
    let names: Vec<&str> = v
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["联通", "移动"]);

    let (c, _) = post(
        &f,
        &ck,
        "/api/admin/probes/isp-template",
        json!({"telecom": "", "unicom": " ", "mobile": ""}),
    )
    .await;
    assert_eq!(c, 400);
    let (c, _) = post(
        &f,
        &ck,
        "/api/admin/probes/isp-template",
        json!({"telecom": "http://x"}),
    )
    .await;
    assert_eq!(c, 400);
    assert_eq!(get_list(&f, &ck).await.len(), 5);
}

#[tokio::test]
async fn create_defaults_and_crud() {
    let f = start(|_| {}).await;
    let ck = login(f.port).await;
    let (c, a) = post(
        &f,
        &ck,
        "/api/admin/probes",
        json!({"name": " 网关 ", "kind": "icmp", "target": "1.1.1.1"}),
    )
    .await;
    assert_eq!(c, 201, "{a}");
    assert_eq!(a["name"], "网关");
    assert_eq!(a["interval_secs"], 60);
    assert_eq!(a["timeout_ms"], 5000);
    assert_eq!(a["samples"], 5);
    assert_eq!(a["all_nodes"], true);
    assert_eq!(a["show_on_card"], true);
    let (_, b) = post(
        &f,
        &ck,
        "/api/admin/probes",
        json!({"name": "web", "kind": "http", "target": "https://example.com/x?y=1"}),
    )
    .await;
    assert_eq!(b["samples"], 1);
    assert_eq!(b["sort"].as_i64().unwrap(), a["sort"].as_i64().unwrap() + 1);
    let (_, t) = post(
        &f,
        &ck,
        "/api/admin/probes",
        json!({"name": "v6", "kind": "tcp", "target": "[::1]:80"}),
    )
    .await;
    assert_eq!(t["samples"], 1);

    // 指定节点
    let n1 = add_node(&f, &ck, "n1").await;
    let id = a["id"].as_i64().unwrap();
    let path = format!("/api/admin/probes/{id}");
    let body = json!({"all_nodes": false, "node_ids": [n1, n1]}).to_string();
    let (c, _, r) = http(f.port, "PATCH", &path, Some(&ck), Some(&body)).await;
    assert_eq!(c, 200, "{r}");
    let r: Value = serde_json::from_str(&r).unwrap();
    assert_eq!(r["all_nodes"], false);
    assert_eq!(r["node_ids"], json!([n1]));
    // 改回全部节点：关联清空
    let (c, _, r) = http(
        f.port,
        "PATCH",
        &path,
        Some(&ck),
        Some(r#"{"all_nodes": true, "name": "新名"}"#),
    )
    .await;
    assert_eq!(c, 200, "{r}");
    let r: Value = serde_json::from_str(&r).unwrap();
    assert_eq!(r["node_ids"], json!([]));
    assert_eq!(r["name"], "新名");
    assert_eq!(count(&f, "SELECT COUNT(*) FROM probe_task_nodes"), 0);
    // 不存在
    let (c, _, _) = http(
        f.port,
        "PATCH",
        "/api/admin/probes/999",
        Some(&ck),
        Some("{}"),
    )
    .await;
    assert_eq!(c, 404);
    let (c, _, _) = http(
        f.port,
        "DELETE",
        "/api/admin/probes/999",
        Some(&ck),
        Some("{}"),
    )
    .await;
    assert_eq!(c, 404);
    let (c, _, _) = http(
        f.port,
        "DELETE",
        "/api/admin/probes/abc",
        Some(&ck),
        Some("{}"),
    )
    .await;
    assert_eq!(c, 404);
    let (c, _, _) = http(f.port, "DELETE", &path, Some(&ck), Some("{}")).await;
    assert_eq!(c, 204);
    assert_eq!(get_list(&f, &ck).await.len(), 2);
}

#[tokio::test]
async fn probe_validation_400() {
    let f = start(|_| {}).await;
    let ck = login(f.port).await;
    let n1 = add_node(&f, &ck, "n1").await;
    let long = "a".repeat(33);
    let cases = [
        json!({"name": "", "kind": "icmp", "target": "1.1.1.1"}),
        json!({"name": "  ", "kind": "icmp", "target": "1.1.1.1"}),
        json!({"name": long, "kind": "icmp", "target": "1.1.1.1"}),
        json!({"name": "x", "kind": "tcp", "target": "example.com"}),
        json!({"name": "x", "kind": "tcp", "target": "example.com:0"}),
        json!({"name": "x", "kind": "tcp", "target": "example.com:65536"}),
        json!({"name": "x", "kind": "tcp", "target": "::1:80"}),
        json!({"name": "x", "kind": "http", "target": "example.com"}),
        json!({"name": "x", "kind": "http", "target": "ftp://example.com"}),
        json!({"name": "x", "kind": "http", "target": "http://"}),
        json!({"name": "x", "kind": "http", "target": "http://a b.com"}),
        json!({"name": "x", "kind": "icmp", "target": "http://1.1.1.1"}),
        json!({"name": "x", "kind": "icmp", "target": "1.1.1.1:80"}),
        json!({"name": "x", "kind": "icmp", "target": "a b"}),
        json!({"name": "x", "kind": "icmp", "target": ""}),
        json!({"name": "x", "kind": "icmp", "target": "1.1.1.1", "interval_secs": 5}),
        json!({"name": "x", "kind": "icmp", "target": "1.1.1.1", "interval_secs": 3601}),
        json!({"name": "x", "kind": "icmp", "target": "1.1.1.1", "samples": 0}),
        json!({"name": "x", "kind": "icmp", "target": "1.1.1.1", "samples": 21}),
        json!({"name": "x", "kind": "icmp", "target": "1.1.1.1", "timeout_ms": 100}),
        json!({"name": "x", "kind": "icmp", "target": "1.1.1.1", "timeout_ms": 10001}),
        json!({"name": "x", "kind": "bogus", "target": "1.1.1.1"}),
        json!({"name": "x", "kind": "icmp", "target": "1.1.1.1", "all_nodes": false, "node_ids": []}),
        json!({"name": "x", "kind": "icmp", "target": "1.1.1.1", "all_nodes": false}),
        json!({"name": "x", "kind": "icmp", "target": "1.1.1.1", "all_nodes": false, "node_ids": [n1 + 100]}),
    ];
    for body in cases {
        let (c, v) = post(&f, &ck, "/api/admin/probes", body.clone()).await;
        assert_eq!(c, 400, "{body} -> {v}");
    }
    let (_, v) = post(
        &f,
        &ck,
        "/api/admin/probes",
        json!({"name": "x", "kind": "icmp", "target": "1.1.1.1", "all_nodes": false, "node_ids": []}),
    )
    .await;
    assert_eq!(v["error"], "请选择至少一个节点");
    // 合法边界
    for body in [
        json!({"name": "x", "kind": "tcp", "target": "example.com:65535", "interval_secs": 10, "samples": 20, "timeout_ms": 500}),
        json!({"name": "x", "kind": "tcp", "target": "1.2.3.4:1", "interval_secs": 3600, "timeout_ms": 10000}),
        json!({"name": "x", "kind": "http", "target": "http://localhost:8080"}),
        json!({"name": "x", "kind": "icmp", "target": "2001:db8::1"}),
        json!({"name": "我".repeat(32), "kind": "icmp", "target": "a-b.example.com"}),
    ] {
        let (c, v) = post(&f, &ck, "/api/admin/probes", body.clone()).await;
        assert_eq!(c, 201, "{body} -> {v}");
    }
    // PATCH 校验：改 kind 后原 target 不合法
    let (_, t) = post(
        &f,
        &ck,
        "/api/admin/probes",
        json!({"name": "p", "kind": "icmp", "target": "1.1.1.1"}),
    )
    .await;
    let path = format!("/api/admin/probes/{}", t["id"]);
    for body in [
        r#"{"kind":"tcp"}"#,
        r#"{"interval_secs":1}"#,
        r#"{"name":""}"#,
        r#"{"all_nodes":false}"#,
        r#"{"target":"a b"}"#,
    ] {
        let (c, _, r) = http(f.port, "PATCH", &path, Some(&ck), Some(body)).await;
        assert_eq!(c, 400, "{body} -> {r}");
    }
}

#[tokio::test]
async fn delete_task_removes_its_data() {
    let f = start(|_| {}).await;
    let ck = login(f.port).await;
    let n1 = add_node(&f, &ck, "n1").await;
    let mk = |name: &str| json!({"name": name, "kind": "icmp", "target": "1.1.1.1", "all_nodes": false, "node_ids": [n1]});
    let (_, a) = post(&f, &ck, "/api/admin/probes", mk("a")).await;
    let (_, b) = post(&f, &ck, "/api/admin/probes", mk("b")).await;
    let (a, b) = (a["id"].as_i64().unwrap(), b["id"].as_i64().unwrap());
    let mut sql = String::new();
    for t in [a, b] {
        for ts in 0..5 {
            sql += &format!(
                "INSERT INTO probe_raw(task_id,node_id,ts_ms,sent,lost) VALUES({t},{n1},{ts},5,0);\
                 INSERT INTO probe_5m(task_id,node_id,ts_ms,sent,lost,samples) VALUES({t},{n1},{ts},5,0,1);"
            );
        }
    }
    exec(&f, &sql);
    let (c, _, _) = http(
        f.port,
        "DELETE",
        &format!("/api/admin/probes/{a}"),
        Some(&ck),
        Some("{}"),
    )
    .await;
    assert_eq!(c, 204);
    for t in ["probe_raw", "probe_5m"] {
        assert_eq!(
            count(&f, &format!("SELECT COUNT(*) FROM {t} WHERE task_id={a}")),
            0
        );
        assert_eq!(
            count(&f, &format!("SELECT COUNT(*) FROM {t} WHERE task_id={b}")),
            5
        );
    }
    assert_eq!(
        count(
            &f,
            &format!("SELECT COUNT(*) FROM probe_task_nodes WHERE task_id={a}")
        ),
        0
    );
    assert_eq!(
        count(
            &f,
            &format!("SELECT COUNT(*) FROM probe_task_nodes WHERE task_id={b}")
        ),
        1
    );
}

#[tokio::test]
async fn delete_node_removes_probe_data_and_links() {
    let f = start(|_| {}).await;
    let ck = login(f.port).await;
    let n1 = add_node(&f, &ck, "n1").await;
    let n2 = add_node(&f, &ck, "n2").await;
    let (_, a) = post(
        &f,
        &ck,
        "/api/admin/probes",
        json!({"name": "a", "kind": "icmp", "target": "1.1.1.1", "all_nodes": false, "node_ids": [n1, n2]}),
    )
    .await;
    let a = a["id"].as_i64().unwrap();
    let mut sql = String::new();
    for n in [n1, n2] {
        for ts in 0..4 {
            sql += &format!(
                "INSERT INTO probe_raw(task_id,node_id,ts_ms,sent,lost) VALUES({a},{n},{ts},5,0);\
                 INSERT INTO probe_5m(task_id,node_id,ts_ms,sent,lost,samples) VALUES({a},{n},{ts},5,0,1);"
            );
        }
    }
    exec(&f, &sql);
    let (c, _, _) = http(
        f.port,
        "DELETE",
        &format!("/api/admin/nodes/{n1}"),
        Some(&ck),
        Some("{}"),
    )
    .await;
    assert_eq!(c, 204);
    for t in ["probe_raw", "probe_5m"] {
        assert_eq!(
            count(&f, &format!("SELECT COUNT(*) FROM {t} WHERE node_id={n1}")),
            0
        );
        assert_eq!(
            count(&f, &format!("SELECT COUNT(*) FROM {t} WHERE node_id={n2}")),
            4
        );
    }
    assert_eq!(
        count(
            &f,
            &format!("SELECT COUNT(*) FROM probe_task_nodes WHERE node_id={n1}")
        ),
        0
    );
    assert_eq!(
        count(
            &f,
            &format!("SELECT COUNT(*) FROM probe_task_nodes WHERE node_id={n2}")
        ),
        1
    );
    let list = get_list(&f, &ck).await;
    assert_eq!(list[0]["node_ids"], json!([n2]));
}

#[tokio::test]
async fn probe_admin_requires_login() {
    let f = start(|_| {}).await;
    for (m, p, b) in [
        ("GET", "/api/admin/probes", None),
        ("POST", "/api/admin/probes", Some("{}")),
        ("PATCH", "/api/admin/probes/1", Some("{}")),
        ("DELETE", "/api/admin/probes/1", Some("{}")),
        ("POST", "/api/admin/probes/isp-template", Some("{}")),
    ] {
        let (c, _, body) = http(f.port, m, p, None, b).await;
        assert_eq!(c, 401, "{m} {p}: {body}");
    }
}

#[tokio::test]
async fn target_length_and_sort_caps() {
    let f = start(|_| {}).await;
    let ck = login(f.port).await;
    // icmp/tcp 上限 255：用多段合法域名凑长度
    let dom = |n: usize| vec!["a".repeat(50); n].join(".");
    for (kind, target) in [
        ("icmp", dom(6)),
        ("tcp", format!("{}:80", dom(6))),
        ("http", format!("http://example.com/{}", "a".repeat(2048))),
    ] {
        let (c, v) = post(
            &f,
            &ck,
            "/api/admin/probes",
            json!({"name": "x", "kind": kind, "target": target}),
        )
        .await;
        assert_eq!(c, 400, "{kind}");
        assert_eq!(v["error"], "目标地址过长", "{kind}");
    }
    let (c, _) = post(
        &f,
        &ck,
        "/api/admin/probes",
        json!({"name": "x", "kind": "http", "target": format!("http://example.com/{}", "a".repeat(2000))}),
    )
    .await;
    assert_eq!(c, 201);
    let (_, t) = post(
        &f,
        &ck,
        "/api/admin/probes",
        json!({"name": "p", "kind": "icmp", "target": "1.1.1.1"}),
    )
    .await;
    let path = format!("/api/admin/probes/{}", t["id"]);
    for body in [r#"{"sort": -1}"#, r#"{"sort": 9223372036854775807}"#] {
        let (c, _, _) = http(f.port, "PATCH", &path, Some(&ck), Some(body)).await;
        assert_eq!(c, 400, "{body}");
    }
    let (c, _, _) = http(
        f.port,
        "PATCH",
        &path,
        Some(&ck),
        Some(r#"{"sort": 1000000}"#),
    )
    .await;
    assert_eq!(c, 200);
}

#[tokio::test]
async fn delete_task_succeeds_and_refreshes_cache_when_purge_fails() {
    let f = start(|_| {}).await;
    let ck = login(f.port).await;
    let n1 = add_node(&f, &ck, "n1").await;
    let (_, a) = post(
        &f,
        &ck,
        "/api/admin/probes",
        json!({"name": "a", "kind": "icmp", "target": "1.1.1.1"}),
    )
    .await;
    let a = a["id"].as_i64().unwrap();
    exec(
        &f,
        &format!(
            "INSERT INTO probe_raw(task_id,node_id,ts_ms,sent,lost) VALUES({a},{n1},1,5,0); DROP TABLE probe_5m;"
        ),
    );
    let (c, _, b) = http(
        f.port,
        "DELETE",
        &format!("/api/admin/probes/{a}"),
        Some(&ck),
        Some("{}"),
    )
    .await;
    assert_eq!(c, 204, "{b}");
    assert!(get_list(&f, &ck).await.is_empty());
    assert!(f.state.probe_tasks().is_empty());
    assert_eq!(
        count(
            &f,
            &format!("SELECT COUNT(*) FROM probe_tasks WHERE id={a}")
        ),
        0
    );
}
