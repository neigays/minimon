//! 端到端：server 下发 ProbeTasks → agent 经 WebSocket 回 ProbeResult；
//! 重连后需重新下发任务才会继续探测。

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use minimon_agent::{AgentConfig, run};
use minimon_common::protocol::{AgentMsg, ProbeKind, ProbeTask, ServerMsg};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;
use tokio_tungstenite::tungstenite::Message;

type Ws = tokio_tungstenite::WebSocketStream<TcpStream>;

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

async fn handshake(l: &TcpListener) -> Ws {
    let (s, _) = l.accept().await.unwrap();
    let mut ws = tokio_tungstenite::accept_async(s).await.unwrap();
    assert!(matches!(next_msg(&mut ws).await, Some(AgentMsg::Hello(_))));
    ws.send(send_json(&ServerMsg::Welcome { interval_secs: 1 }))
        .await
        .unwrap();
    ws
}

async fn wait_probe_result(
    ws: &mut Ws,
    secs: u64,
) -> Option<minimon_common::protocol::ProbeResult> {
    tokio::time::timeout(Duration::from_secs(secs), async {
        loop {
            if let AgentMsg::ProbeResult(r) = next_msg(ws).await? {
                return Some(r);
            }
        }
    })
    .await
    .ok()
    .flatten()
}

#[tokio::test]
async fn probe_tasks_produce_results_and_reset_on_reconnect() {
    let target = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let target_addr = target.local_addr().unwrap();
    tokio::spawn(async move { while target.accept().await.is_ok() {} });
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    let mut cfg = AgentConfig::new(format!("ws://{addr}"), "tok");
    cfg.backoff_base = Duration::from_millis(200);
    cfg.backoff_max = Duration::from_millis(500);
    let (stop_tx, stop_rx) = oneshot::channel::<()>();
    let agent = tokio::spawn(run(cfg, async {
        stop_rx.await.ok();
    }));

    let mut ws = handshake(&l).await;
    ws.send(send_json(&ServerMsg::ProbeTasks {
        tasks: vec![ProbeTask {
            id: 5,
            kind: ProbeKind::Tcp,
            target: target_addr.to_string(),
            interval_secs: 1,
            samples: 1,
            timeout_ms: 1000,
        }],
    }))
    .await
    .unwrap();
    let r = wait_probe_result(&mut ws, 5).await.expect("应收到探测结果");
    assert_eq!((r.task_id, r.sent, r.lost), (5, 1, 0));

    // 断线重连：未重新下发任务则不应再有探测结果
    drop(ws);
    let mut ws = tokio::time::timeout(Duration::from_secs(5), handshake(&l))
        .await
        .unwrap();
    assert!(wait_probe_result(&mut ws, 3).await.is_none());

    stop_tx.send(()).unwrap();
    let res = tokio::time::timeout(Duration::from_secs(5), agent)
        .await
        .unwrap()
        .unwrap();
    assert!(res.is_ok());
}
