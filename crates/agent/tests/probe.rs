//! 探测集成测试：全部使用本机假服务，不依赖外部网络。

use std::time::{Duration, Instant};

use minimon_agent::probe::{ProbeScheduler, probe_http, probe_tcp};
use minimon_common::protocol::{ProbeKind, ProbeTask};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::mpsc;

const T: Duration = Duration::from_secs(2);

async fn listener() -> (TcpListener, String) {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let a = l.local_addr().unwrap();
    (l, format!("127.0.0.1:{}", a.port()))
}

/// 对每个连接读完请求头后回复 `reply`（然后可选地持续慢速发 body）。
fn serve(l: TcpListener, reply: &'static str, drip: bool) {
    tokio::spawn(async move {
        loop {
            let Ok((mut s, _)) = l.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let mut buf = [0u8; 1024];
                let _ = s.read(&mut buf).await;
                let _ = s.write_all(reply.as_bytes()).await;
                if !drip {
                    return;
                }
                loop {
                    tokio::time::sleep(Duration::from_millis(200)).await;
                    if s.write_all(b"x").await.is_err() {
                        return;
                    }
                }
            });
        }
    });
}

#[tokio::test]
async fn tcp_open_port() {
    let (l, addr) = listener().await;
    tokio::spawn(async move { while l.accept().await.is_ok() {} });
    let o = probe_tcp(&addr, 3, T).await;
    assert_eq!((o.sent, o.lost, o.error), (3, 0, None));
    assert!(o.avg_ms.unwrap() > 0.0);
}

#[tokio::test]
async fn tcp_closed_port() {
    let (l, addr) = listener().await;
    drop(l);
    let o = probe_tcp(&addr, 3, T).await;
    assert_eq!((o.sent, o.lost), (3, 3));
    assert_eq!(o.error, Some("连接失败"));
    assert_eq!(o.avg_ms, None);
}

#[tokio::test]
async fn tcp_bad_host() {
    let o = probe_tcp("no-such-host.invalid:80", 3, T).await;
    assert_eq!((o.sent, o.lost), (3, 3));
    assert_eq!(o.error, Some("解析失败"));
}

#[tokio::test]
async fn tcp_malformed_target_is_resolve_failure() {
    let o = probe_tcp("no-port-here", 1, T).await;
    assert_eq!(o.error, Some("解析失败"));
}

#[tokio::test]
async fn http_200_and_500() {
    let (l, a200) = listener().await;
    serve(l, "HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n", false);
    let o = probe_http(&format!("http://{a200}/x"), 2, T).await;
    assert_eq!((o.sent, o.lost, o.error), (2, 0, None));
    assert!(o.avg_ms.unwrap() >= 0.0);

    let (l, a500) = listener().await;
    serve(l, "HTTP/1.1 500 Oops\r\nContent-Length: 0\r\n\r\n", false);
    let o = probe_http(&format!("http://{a500}/"), 2, T).await;
    assert_eq!((o.sent, o.lost), (2, 2));
    assert_eq!(o.error, Some("HTTP 状态异常"));
}

#[tokio::test]
async fn http_stops_at_first_byte() {
    let (l, addr) = listener().await;
    serve(l, "HTTP/1.1 200 OK\r\n", true);
    let t0 = Instant::now();
    let o = probe_http(&format!("http://{addr}/"), 1, Duration::from_secs(3)).await;
    assert_eq!((o.lost, o.error), (0, None));
    assert!(t0.elapsed() < Duration::from_secs(1));
}

#[tokio::test]
async fn http_redirect_not_followed_counts_success() {
    let (l, addr) = listener().await;
    serve(
        l,
        "HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:1/\r\n\r\n",
        false,
    );
    let o = probe_http(&format!("http://{addr}/"), 1, T).await;
    assert_eq!((o.lost, o.error), (0, None));
}

#[tokio::test]
async fn http_bad_url_and_timeout() {
    let o = probe_http("ftp://x/", 1, T).await;
    assert_eq!(o.error, Some("解析失败"));
    // 接受连接但永不响应
    let (l, addr) = listener().await;
    tokio::spawn(async move {
        let mut held = vec![];
        while let Ok(c) = l.accept().await {
            held.push(c);
        }
    });
    let o = probe_http(&format!("http://{addr}/"), 1, Duration::from_millis(300)).await;
    assert_eq!((o.lost, o.error), (1, Some("超时")));
}

fn tcp_task(id: i64, target: &str, interval_secs: u32) -> ProbeTask {
    ProbeTask {
        id,
        kind: ProbeKind::Tcp,
        target: target.into(),
        interval_secs,
        samples: 1,
        timeout_ms: 2000,
    }
}

#[tokio::test]
async fn scheduler_replace_and_remove() {
    let (l, addr) = listener().await;
    tokio::spawn(async move { while l.accept().await.is_ok() {} });
    let (tx, mut rx) = mpsc::channel(16);
    let mut s = ProbeScheduler::new(tx);
    s.replace(vec![tcp_task(7, &addr, 1)]);
    let r = tokio::time::timeout(Duration::from_millis(2500), rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!((r.task_id, r.sent, r.lost, r.error), (7, 1, 0, None));
    assert!(r.ts_ms > 1_600_000_000_000);
    s.replace(vec![]);
    while rx.try_recv().is_ok() {}
    assert!(
        tokio::time::timeout(Duration::from_millis(2200), rx.recv())
            .await
            .is_err()
    );
    s.shutdown();
}

#[tokio::test]
async fn scheduler_skips_overlapping_rounds() {
    // 服务端接受后不响应：每轮 http 探测耗时 ~2.5s，interval 1s，不得并发两轮
    let (l, addr) = listener().await;
    let (cnt_tx, mut cnt_rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        let mut held = vec![];
        while let Ok(c) = l.accept().await {
            let _ = cnt_tx.send(());
            held.push(c);
        }
    });
    let (tx, rx) = mpsc::channel(16);
    let mut s = ProbeScheduler::new(tx);
    let mut t = tcp_task(1, &format!("http://{addr}/"), 1);
    t.kind = ProbeKind::Http;
    t.timeout_ms = 2500;
    s.replace(vec![t]);
    tokio::time::sleep(Duration::from_millis(2900)).await;
    let mut conns = 0;
    while cnt_rx.try_recv().is_ok() {
        conns += 1;
    }
    // 首次延迟 d∈[0,1s) + 一轮 2.5s：第 2 轮最早在 d+3s 开始；若会并发则 d+1s 就有第 2 个连接
    assert!(conns <= 1, "conns={conns}");
    s.shutdown();
    drop(rx);
}

#[tokio::test]
async fn scheduler_keeps_unchanged_task_cadence() {
    let (l, addr) = listener().await;
    tokio::spawn(async move { while l.accept().await.is_ok() {} });
    let (tx, mut rx) = mpsc::channel(16);
    let mut s = ProbeScheduler::new(tx);
    let t = tcp_task(1, &addr, 1);
    s.replace(vec![t.clone()]);
    rx.recv().await.unwrap();
    let t0 = Instant::now();
    // 相同参数再次下发：不重启，下一结果应在 ~1s 后而非 [0,1s) 随机延迟后重新开始
    s.replace(vec![t]);
    rx.recv().await.unwrap();
    let dt = t0.elapsed();
    assert!(dt > Duration::from_millis(700), "dt={dt:?}");
    s.shutdown();
}

#[tokio::test]
async fn icmp_placeholder_reports_unavailable() {
    let (tx, mut rx) = mpsc::channel(4);
    let mut s = ProbeScheduler::new(tx);
    let mut t = tcp_task(3, "127.0.0.1", 1);
    t.kind = ProbeKind::Icmp;
    s.replace(vec![t]);
    let r = tokio::time::timeout(Duration::from_millis(2500), rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!((r.sent, r.error.as_deref()), (0, Some("ICMP 不可用")));
    s.shutdown();
}
