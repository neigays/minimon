pub mod config;
pub mod db;
mod ingest;
pub mod state;
mod ws_agent;

use anyhow::Context;
use axum::Router;
use axum::routing::get;
use ingest::IngestJoin;
use state::AppState;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/agent/ws", get(ws_agent::handler))
        .with_state(state)
}

/// 打开数据库并监听 `cfg.listen`。
pub async fn serve(
    cfg: config::ServerConfig,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> anyhow::Result<()> {
    std::fs::create_dir_all(&cfg.data_dir)
        .with_context(|| format!("创建数据目录 {} 失败", cfg.data_dir.display()))?;
    let db = Arc::new(db::Db::open(&cfg.db_path()).context("打开数据库失败")?);
    let listener = tokio::net::TcpListener::bind(cfg.listen)
        .await
        .with_context(|| format!("监听 {} 失败", cfg.listen))?;
    tracing::info!(listen = %cfg.listen, "server 已启动");
    let (state, ingest) = AppState::new(db, cfg);
    serve_on(listener, state, ingest, shutdown).await
}

/// 停机时等待活跃 WS 会话 / HTTP 连接收尾的上限。
const SESSION_DRAIN: Duration = Duration::from_secs(5);
const HTTP_DRAIN: Duration = Duration::from_secs(10);

/// 在已绑定的 listener 上提供服务（集成测试可绑定端口 0）。
///
/// 关停顺序：信号到达 -> 通知 WS 会话（发 Close 并退出）-> axum 优雅停止
/// -> 等待 WS 会话结束（有上限）-> flush 并回收入库线程。每一步都有上限。
pub async fn serve_on(
    listener: tokio::net::TcpListener,
    state: AppState,
    ingest: IngestJoin,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> anyhow::Result<()> {
    let mut stopping = state.shutdown_rx();
    let sig_state = state.clone();
    let signal = async move {
        shutdown.await;
        sig_state.begin_shutdown();
    };
    // axum 的优雅停机不管理 upgrade 后的连接；HTTP 连接迟迟不退出时也不能卡死进程。
    let bounded_http = async move {
        let _ = stopping.wait_for(|v| *v).await;
        tokio::time::sleep(HTTP_DRAIN).await;
    };
    let serve = axum::serve(listener, router(state.clone())).with_graceful_shutdown(signal);
    let served = tokio::select! {
        r = serve.into_future() => r.context("HTTP 服务异常退出"),
        _ = bounded_http => {
            tracing::warn!("HTTP 连接未在时限内结束，强制继续停机");
            Ok(())
        }
    };
    state.begin_shutdown();
    if !state.wait_sessions(SESSION_DRAIN).await {
        tracing::warn!("部分 WS 会话未在时限内结束");
    }
    // shutdown 会阻塞（写库 + join），放到阻塞线程池。
    if tokio::task::spawn_blocking(move || ingest.shutdown())
        .await
        .is_err()
    {
        tracing::error!("ingest 关停任务异常");
    }
    served
}
