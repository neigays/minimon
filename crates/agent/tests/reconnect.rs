use std::net::SocketAddr;
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use minimon_agent::{AgentConfig, run};
use minimon_common::protocol::{AgentMsg, ErrorCode, ServerMsg};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};

type Ws = tokio_tungstenite::WebSocketStream<TcpStream>;

/// 接受一个连接，返回 (ws, Authorization 头, 请求路径)。
async fn accept(l: &TcpListener) -> (Ws, Option<String>, String) {
    let (s, _) = l.accept().await.unwrap();
    let mut auth = None;
    let mut path = String::new();
    #[allow(clippy::result_large_err)] // 回调签名由 tungstenite 决定
    let cb = |req: &Request, resp: Response| {
        auth = req
            .headers()
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        path = req.uri().path().to_owned();
        Ok(resp)
    };
    let ws = tokio_tungstenite::accept_hdr_async(s, cb).await.unwrap();
    (ws, auth, path)
}

async fn next_msg(ws: &mut Ws) -> Option<AgentMsg> {
    loop {
        match ws.next().await? {
            Ok(Message::Text(t)) => return serde_json::from_str(&t).ok(),
            Ok(Message::Close(_)) | Err(_) => return None,
            Ok(_) => {}
        }
    }
}

fn send_json(msg: &ServerMsg) -> Message {
    Message::text(serde_json::to_string(msg).unwrap())
}

fn cfg(addr: SocketAddr) -> AgentConfig {
    let mut c = AgentConfig::new(format!("ws://{addr}"), "tok-123");
    c.backoff_base = Duration::from_secs(1);
    c.backoff_max = Duration::from_secs(2);
    c
}

#[tokio::test]
async fn reconnects_after_rejection_then_reports() {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    let (stop_tx, stop_rx) = oneshot::channel::<()>();
    let agent = tokio::spawn(run(cfg(addr), async {
        stop_rx.await.ok();
    }));

    let (mut ws, auth, path) = accept(&l).await;
    let t1 = Instant::now();
    assert_eq!(auth.as_deref(), Some("Bearer tok-123"));
    assert_eq!(path, "/api/agent/ws");
    assert!(matches!(next_msg(&mut ws).await, Some(AgentMsg::Hello(_))));
    ws.send(send_json(&ServerMsg::Error {
        code: ErrorCode::Unauthorized,
        message: "bad token".into(),
    }))
    .await
    .unwrap();
    ws.close(None).await.ok();
    drop(ws);

    let (mut ws, auth, _) = tokio::time::timeout(Duration::from_secs(15), accept(&l))
        .await
        .unwrap();
    assert!(
        t1.elapsed() >= Duration::from_millis(800),
        "无退避: {:?}",
        t1.elapsed()
    );
    assert_eq!(auth.as_deref(), Some("Bearer tok-123"));
    assert!(matches!(next_msg(&mut ws).await, Some(AgentMsg::Hello(_))));
    ws.send(send_json(&ServerMsg::Welcome { interval_secs: 1 }))
        .await
        .unwrap();
    let got = tokio::time::timeout(Duration::from_secs(10), next_msg(&mut ws))
        .await
        .unwrap();
    assert!(matches!(got, Some(AgentMsg::Report(_))));

    stop_tx.send(()).unwrap();
    // 优雅退出：agent 关闭 WS，且 run 返回 Ok。
    let res = tokio::time::timeout(Duration::from_secs(5), agent)
        .await
        .unwrap()
        .unwrap();
    assert!(res.is_ok());
}

#[tokio::test]
async fn survives_server_absent_at_startup() {
    let addr = {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        l.local_addr().unwrap()
    };
    let (stop_tx, stop_rx) = oneshot::channel::<()>();
    let mut c = cfg(addr);
    c.backoff_base = Duration::from_millis(300);
    c.backoff_max = Duration::from_secs(1);
    let agent = tokio::spawn(run(c, async {
        stop_rx.await.ok();
    }));

    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(!agent.is_finished(), "run 不应在连接失败时返回");
    let l = TcpListener::bind(addr).await.unwrap();
    let (tx, mut rx) = mpsc::channel(1);
    tokio::spawn(async move {
        let (mut ws, _, _) = accept(&l).await;
        if let Some(m) = next_msg(&mut ws).await {
            tx.send(m).await.ok();
        }
        ws.close(None).await.ok();
    });
    let m = tokio::time::timeout(Duration::from_secs(10), rx.recv())
        .await
        .unwrap();
    assert!(matches!(m, Some(AgentMsg::Hello(_))));
    assert!(!agent.is_finished());

    stop_tx.send(()).unwrap();
    let res = tokio::time::timeout(Duration::from_secs(5), agent)
        .await
        .unwrap()
        .unwrap();
    assert!(res.is_ok());
}
