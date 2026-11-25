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
