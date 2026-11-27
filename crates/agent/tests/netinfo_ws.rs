//! 端到端：连上 server（收到 Welcome）后 agent 查询 trace 地址并上报 NetInfo。

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use minimon_agent::{AgentConfig, NetinfoUrls, run};
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

type Hits = std::sync::Arc<std::sync::atomic::AtomicUsize>;

static HITS: std::sync::Mutex<Vec<(String, Hits)>> = std::sync::Mutex::new(Vec::new());

/// `flaky_mock` 返回地址对应的请求计数。
fn flaky_hits(base: &str) -> Hits {
    let all = HITS.lock().unwrap();
    all.iter().find(|(b, _)| b == base).unwrap().1.clone()
}

/// 前 `fail_first` 次请求返回 404，之后返回 trace 正文；请求次数可用 `flaky_hits` 查询。
async fn flaky_mock(fail_first: usize) -> String {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    let hits: Hits = Default::default();
    HITS.lock().unwrap().push((base.clone(), hits.clone()));
    tokio::spawn(async move {
        while let Ok((mut s, _)) = l.accept().await {
            let hits = hits.clone();
            tokio::spawn(async move {
                let mut buf = [0u8; 2048];
                let _ = s.read(&mut buf).await;
                let n = hits.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let resp = if n >= fail_first {
                    let body = "ip=203.0.113.9\nloc=CN\n";
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{body}",
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

/// 首次查询全部失败时不等 6 小时，按 `netinfo_retry` 很快重试。
#[tokio::test]
async fn agent_retries_netinfo_soon_after_failure() {
    let base = flaky_mock(1).await;
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    let mut cfg = AgentConfig::new(format!("ws://{addr}"), "tok");
    cfg.netinfo_urls = Some(NetinfoUrls {
        v4: vec![format!("{base}/v4")],
        v6: vec![],
    });
    cfg.netinfo_retry = Duration::from_millis(300);
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let agent = tokio::spawn(run(cfg, async {
        stop_rx.await.ok();
    }));
    let (s, _) = l.accept().await.unwrap();
    let mut ws = tokio_tungstenite::accept_async(s).await.unwrap();
    let r = tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(Ok(m)) = ws.next().await {
            let Message::Text(t) = m else { continue };
            match serde_json::from_str::<AgentMsg>(&t).unwrap() {
                AgentMsg::Hello(_) => {
                    let w =
                        serde_json::to_string(&ServerMsg::Welcome { interval_secs: 1 }).unwrap();
                    ws.send(Message::text(w)).await.unwrap();
                }
                AgentMsg::NetInfo(n) => return n,
                _ => {}
            }
        }
        panic!("连接提前结束");
    })
    .await;
    let n = r.expect("10 秒内未收到重试后的 net_info");
    assert_eq!(n.ipv4.as_deref(), Some("203.0.113.9"));
    let _ = stop_tx.send(());
    let _ = agent.await;
}

#[tokio::test]
async fn agent_sends_netinfo_after_welcome() {
    let base = trace_mock().await;
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    let mut cfg = AgentConfig::new(format!("ws://{addr}"), "tok");
    cfg.netinfo_urls = Some(NetinfoUrls {
        v4: vec![format!("{base}/v4")],
        v6: vec![format!("{base}/v6")],
    });
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

/// 断线重连时，若距上次成功查询不足 `netinfo_min_gap`，直接重发缓存结果，不再访问 trace 地址。
#[tokio::test]
async fn reconnect_reuses_recent_netinfo() {
    let base = flaky_mock(0).await;
    let hits = flaky_hits(&base);
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    let mut cfg = AgentConfig::new(format!("ws://{addr}"), "tok");
    cfg.netinfo_urls = Some(NetinfoUrls {
        v4: vec![format!("{base}/v4")],
        v6: vec![],
    });
    cfg.backoff_base = Duration::from_millis(50);
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let agent = tokio::spawn(run(cfg, async {
        stop_rx.await.ok();
    }));
    for round in 0..2 {
        let (s, _) = l.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(s).await.unwrap();
        let n = tokio::time::timeout(Duration::from_secs(10), async {
            while let Some(Ok(m)) = ws.next().await {
                let Message::Text(t) = m else { continue };
                match serde_json::from_str::<AgentMsg>(&t).unwrap() {
                    AgentMsg::Hello(_) => {
                        let w = serde_json::to_string(&ServerMsg::Welcome { interval_secs: 1 })
                            .unwrap();
                        ws.send(Message::text(w)).await.unwrap();
                    }
                    AgentMsg::NetInfo(n) => return n,
                    _ => {}
                }
            }
            panic!("连接提前结束");
        })
        .await
        .unwrap_or_else(|_| panic!("第 {round} 次连接未收到 net_info"));
        assert_eq!(n.ipv4.as_deref(), Some("203.0.113.9"));
        let _ = ws.close(None).await;
    }
    assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 1);
    let _ = stop_tx.send(());
    let _ = agent.await;
}
