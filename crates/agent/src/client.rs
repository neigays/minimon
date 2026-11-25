//! WebSocket 客户端：连接、握手、周期上报、退避重连。
//!
//! 只处理 `ServerMsg` 中的固定消息；任何无法解析的帧都只记录日志后忽略，绝不执行。

use std::future::Future;
use std::time::Duration;

use anyhow::Context;
use futures_util::{SinkExt, StreamExt};
use minimon_common::protocol::{AgentMsg, ServerMsg};
use tokio::time::{Instant, MissedTickBehavior, interval, interval_at, timeout};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::{HeaderValue, Request, header::AUTHORIZATION};
use tracing::{debug, info, warn};

use crate::backoff::Backoff;
use crate::collect::Collector;
use crate::config::AgentConfig;

const WS_PATH: &str = "/api/agent/ws";
const PING_INTERVAL: Duration = Duration::from_secs(15);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const WELCOME_TIMEOUT: Duration = Duration::from_secs(10);
/// 超过该时长没有收到任何帧（含 pong）则认为连接已死。
const IDLE_LIMIT: Duration = Duration::from_secs(45);
/// 单次发送的上限，防止半开连接上发送缓冲写满后永久阻塞。
const SEND_TIMEOUT: Duration = Duration::from_secs(10);
const CLOSE_TIMEOUT: Duration = Duration::from_secs(2);

/// 一次连接的结局。
enum Outcome {
    Shutdown,
    /// 连接失败或断开；`healthy` 表示会话已证明健康（收到 Welcome 且成功发出首条 report，
    /// 且不是以 server 的 Error 帧结束），只有健康会话之后才重置退避。
    Lost {
        healthy: bool,
        reason: String,
    },
}

fn build_request(cfg: &AgentConfig) -> anyhow::Result<Request<()>> {
    let url = format!("{}{WS_PATH}", cfg.server.trim_end_matches('/'));
    let mut req = url
        .into_client_request()
        .context("server 地址无效（应为 ws:// 或 wss:// 开头的基址）")?;
    let mut auth = HeaderValue::from_str(&format!("Bearer {}", cfg.token))
        .map_err(|_| anyhow::anyhow!("token 含有非法字符"))?;
    auth.set_sensitive(true);
    req.headers_mut().insert(AUTHORIZATION, auth);
    Ok(req)
}

/// 运行 agent 主循环，直到 `shutdown` 完成。连接失败或被拒绝时按退避重连，不返回错误。
pub async fn run(cfg: AgentConfig, shutdown: impl Future<Output = ()>) -> anyhow::Result<()> {
    let req = build_request(&cfg)?;
    let mut collector = Collector::new().with_iface_include(cfg.iface_include.clone());
    let mut backoff = Backoff::new(cfg.backoff_base, cfg.backoff_max);
    tokio::pin!(shutdown);
    info!(server = %cfg.server, "agent 启动");
    loop {
        match session(&cfg, req.clone(), &mut collector, &mut shutdown).await {
            Outcome::Shutdown => {
                info!("收到退出信号，agent 退出");
                return Ok(());
            }
            Outcome::Lost { healthy, reason } => {
                if healthy {
                    backoff.reset();
                }
                let wait = backoff.next();
                warn!("连接结束: {reason}；{:.1} 秒后重连", wait.as_secs_f64());
                tokio::select! {
                    _ = &mut shutdown => {
                        info!("收到退出信号，agent 退出");
                        return Ok(());
                    }
                    _ = tokio::time::sleep(wait) => {}
                }
            }
        }
    }
}

fn lost(healthy: bool, reason: impl Into<String>) -> Outcome {
    Outcome::Lost {
        healthy,
        reason: reason.into(),
    }
}

enum SendErr {
    Shutdown,
    Failed(String),
}

/// 带超时地发送一帧，且发送期间 shutdown 仍可抢占。
async fn send_frame<S, W>(
    ws: &mut W,
    msg: Message,
    shutdown: &mut std::pin::Pin<&mut S>,
) -> Result<(), SendErr>
where
    S: Future<Output = ()>,
    W: futures_util::Sink<Message, Error = tokio_tungstenite::tungstenite::Error> + Unpin,
{
    tokio::select! {
        _ = &mut *shutdown => Err(SendErr::Shutdown),
        r = timeout(SEND_TIMEOUT, ws.send(msg)) => match r {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(SendErr::Failed(e.to_string())),
            Err(_) => Err(SendErr::Failed("发送超时".into())),
        },
    }
}

async fn session<S>(
    cfg: &AgentConfig,
    req: Request<()>,
    collector: &mut Collector,
    shutdown: &mut std::pin::Pin<&mut S>,
) -> Outcome
where
    S: Future<Output = ()>,
{
    let connect = timeout(CONNECT_TIMEOUT, tokio_tungstenite::connect_async(req));
    let mut ws = tokio::select! {
        _ = &mut *shutdown => return Outcome::Shutdown,
        r = connect => match r {
            Ok(Ok((ws, _))) => ws,
            Ok(Err(e)) => return lost(false, format!("连接失败: {e}")),
            Err(_) => return lost(false, "连接超时"),
        },
    };
    debug!("WebSocket 已连接");

    let outcome = drive(cfg, &mut ws, collector, shutdown).await;
    if matches!(outcome, Outcome::Shutdown) {
        let _ = timeout(CLOSE_TIMEOUT, ws.close(None)).await;
    }
    outcome
}

async fn drive<S, W>(
    cfg: &AgentConfig,
    ws: &mut W,
    collector: &mut Collector,
    shutdown: &mut std::pin::Pin<&mut S>,
) -> Outcome
where
    S: Future<Output = ()>,
    W: futures_util::Sink<Message, Error = tokio_tungstenite::tungstenite::Error>
        + futures_util::Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>>
        + Unpin,
{
    let hello = match serde_json::to_string(&AgentMsg::Hello(collector.hello())) {
        Ok(s) => s,
        Err(e) => return lost(false, format!("序列化 hello 失败: {e}")),
    };
    match send_frame(ws, Message::text(hello), shutdown).await {
        Ok(()) => {}
        Err(SendErr::Shutdown) => return Outcome::Shutdown,
        Err(SendErr::Failed(e)) => return lost(false, format!("发送 hello 失败: {e}")),
    }

    // 等待 Welcome（忽略期间的其它非致命帧）。
    let welcome_deadline = Instant::now() + WELCOME_TIMEOUT;
    let interval_secs = loop {
        let frame = tokio::select! {
            _ = &mut *shutdown => return Outcome::Shutdown,
            r = tokio::time::timeout_at(welcome_deadline, ws.next()) => match r {
                Ok(f) => f,
                Err(_) => return lost(false, "等待 welcome 超时"),
            },
        };
        match frame {
            None => return lost(false, "连接在 welcome 之前关闭"),
            Some(Err(e)) => return lost(false, format!("读取失败: {e}")),
            Some(Ok(Message::Text(t))) => match serde_json::from_str::<ServerMsg>(&t) {
                Ok(ServerMsg::Welcome { interval_secs }) => break interval_secs,
                Ok(ServerMsg::Error { code, message }) => {
                    return lost(false, format!("server 拒绝连接: {code:?} {message}"));
                }
                Ok(other) => debug!("welcome 之前收到 {other:?}，忽略"),
                Err(e) => warn!("无法解析的 server 消息，已忽略: {e}"),
            },
            Some(Ok(Message::Close(_))) => return lost(false, "server 关闭了连接"),
            Some(Ok(_)) => {}
        }
    };
    info!(interval_secs, "已连接 server");

    let period = Duration::from_secs(u64::from(
        cfg.interval_override.unwrap_or(interval_secs).max(1),
    ));
    let mut report_tick = interval_at(Instant::now() + period, period);
    report_tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut ping_tick = interval(PING_INTERVAL);
    ping_tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    ping_tick.tick().await; // 第一次立即触发，丢弃
    let mut last_rx = Instant::now();
    let mut healthy = false;

    loop {
        tokio::select! {
            _ = &mut *shutdown => return Outcome::Shutdown,
            _ = report_tick.tick() => {
                let msg = match serde_json::to_string(&AgentMsg::Report(collector.report())) {
                    Ok(s) => s,
                    Err(e) => return lost(healthy, format!("序列化 report 失败: {e}")),
                };
                match send_frame(ws, Message::text(msg), shutdown).await {
                    Ok(()) => healthy = true,
                    Err(SendErr::Shutdown) => return Outcome::Shutdown,
                    Err(SendErr::Failed(e)) => return lost(healthy, format!("发送 report 失败: {e}")),
                }
            }
            _ = ping_tick.tick() => {
                if last_rx.elapsed() > IDLE_LIMIT {
                    return lost(healthy, "长时间未收到 server 数据");
                }
                match send_frame(ws, Message::Ping(Default::default()), shutdown).await {
                    Ok(()) => {}
                    Err(SendErr::Shutdown) => return Outcome::Shutdown,
                    Err(SendErr::Failed(e)) => return lost(healthy, format!("发送 ping 失败: {e}")),
                }
            }
            frame = ws.next() => {
                last_rx = Instant::now();
                match frame {
                    None => return lost(healthy, "连接已断开"),
                    Some(Err(e)) => return lost(healthy, format!("读取失败: {e}")),
                    Some(Ok(Message::Close(_))) => return lost(healthy, "server 关闭了连接"),
                    Some(Ok(Message::Text(t))) => match serde_json::from_str::<ServerMsg>(&t) {
                        Ok(ServerMsg::Error { code, message }) => {
                            return lost(false, format!("server 报错: {code:?} {message}"));
                        }
                        Ok(ServerMsg::ProbeTasks { tasks }) => {
                            info!(count = tasks.len(), "收到探测任务（M3 才执行，已忽略）");
                        }
                        Ok(ServerMsg::Welcome { .. }) => debug!("重复的 welcome，忽略"),
                        Err(e) => warn!("无法解析的 server 消息，已忽略: {e}"),
                    },
                    Some(Ok(_)) => {}
                }
            }
        }
    }
}
