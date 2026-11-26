//! 端到端：连上 server（收到 Welcome）后 agent 查询 trace 地址并上报 NetInfo。

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use minimon_agent::{AgentConfig, run};
use minimon_common::protocol::{AgentMsg, ServerMsg};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;

/// 本地 HTTP mock：`/v4` 返回 trace 正文，其余路径 404。
async fn trace_mock() -> String {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    tokio::spawn(async move {
        while let Ok((mut s, _)) = l.accept().await {
            tokio::spawn(async move {
                let mut buf = [0u8; 2048];
                let n = s.read(&mut buf).await.unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                let resp = if req.starts_with("GET /v4 ") {
                    let body = "fl=1\nip=203.0.113.5\nloc=JP\n";
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\n\r\n{body}",
                        body.len()
                    )
                } else {
                    "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n".to_string()
                };
                let _ = s.write_all(resp.as_bytes()).await;
            });
        }
    });
    base
}

#[tokio::test]
async fn agent_sends_netinfo_after_welcome() {
    let base = trace_mock().await;
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    let mut cfg = AgentConfig::new(format!("ws://{addr}"), "tok");
    cfg.netinfo_urls = Some((format!("{base}/v4"), format!("{base}/v6")));
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let agent = tokio::spawn(run(cfg, async {
        stop_rx.await.ok();
    }));
    let (s, _) = l.accept().await.unwrap();
    let mut ws = tokio_tungstenite::accept_async(s).await.unwrap();
    let mut got = None;
    let r = tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(Ok(m)) = ws.next().await {
            let Message::Text(t) = m else { continue };
            match serde_json::from_str::<AgentMsg>(&t).unwrap() {
                AgentMsg::Hello(_) => {
                    let w =
                        serde_json::to_string(&ServerMsg::Welcome { interval_secs: 1 }).unwrap();
                    ws.send(Message::text(w)).await.unwrap();
                }
                AgentMsg::NetInfo(n) => {
                    got = Some(n);
                    break;
                }
                _ => {}
            }
        }
    })
    .await;
    assert!(r.is_ok(), "10 秒内未收到 net_info");
    let n = got.unwrap();
    assert_eq!(n.ipv4.as_deref(), Some("203.0.113.5"));
    assert_eq!(n.ipv6, None);
    assert_eq!(n.country.as_deref(), Some("JP"));
    let _ = stop_tx.send(());
    let _ = agent.await;
}
