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

/// 在已绑定的 listener 上提供服务（集成测试可绑定端口 0）。
///
/// 关停顺序：先优雅停止 axum，再 flush 并回收入库线程。
pub async fn serve_on(
    listener: tokio::net::TcpListener,
    state: AppState,
    ingest: IngestJoin,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> anyhow::Result<()> {
    let served = axum::serve(listener, router(state))
        .with_graceful_shutdown(shutdown)
        .await
        .context("HTTP 服务异常退出");
    // shutdown 会阻塞（写库 + join），放到阻塞线程池。
    if tokio::task::spawn_blocking(move || ingest.shutdown())
        .await
        .is_err()
    {
        tracing::error!("ingest 关停任务异常");
    }
    served
}
