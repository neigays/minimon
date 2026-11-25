pub mod config;
pub mod db;
pub mod state;
mod ws_agent;

use anyhow::Context;
use axum::Router;
use axum::routing::get;
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
    serve_on(listener, AppState::new(db, cfg), shutdown).await
}

/// 在已绑定的 listener 上提供服务（集成测试可绑定端口 0）。
pub async fn serve_on(
    listener: tokio::net::TcpListener,
    state: AppState,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> anyhow::Result<()> {
    axum::serve(listener, router(state))
        .with_graceful_shutdown(shutdown)
        .await
        .context("HTTP 服务异常退出")
}
