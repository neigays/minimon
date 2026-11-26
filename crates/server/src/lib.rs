mod admin;
pub mod alert;
mod auth;
mod auth_api;
pub mod clock;
pub mod config;
pub mod db;
mod error;
mod history;
mod ingest;
mod live;
pub mod notify;
pub mod period;
pub mod probe;
mod probe_api;
mod probe_history;
mod rollup;
pub mod state;
pub mod traffic;
mod views;
mod web;
mod ws_agent;

use anyhow::Context;
use axum::Router;
use axum::routing::get;
use ingest::IngestJoin;
use state::AppState;
use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

async fn health() -> axum::Json<serde_json::Value> {
    axum::Json(serde_json::json!({"ok": true}))
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/agent/ws", get(ws_agent::handler))
        .route("/api/health", get(health))
        .route("/api/nodes", get(live::nodes))
        .route("/api/live", get(live::ws))
        .route("/api/nodes/{id}/history", get(history::get))
        .route("/api/nodes/{id}/probes", get(probe_history::get))
        .merge(auth_api::routes())
        .merge(admin::routes())
        .merge(probe_api::routes())
        .fallback(web::fallback)
        .layer(axum::middleware::from_fn(auth::require_json))
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
    let admin_user = std::env::var("MINIMON_ADMIN_USER")
        .ok()
        .filter(|u| !u.trim().is_empty())
        .unwrap_or_else(|| "admin".to_string());
    let admin_pw = std::env::var("MINIMON_ADMIN_PASSWORD")
        .ok()
        .filter(|p| !p.is_empty());
    if let Some(pw) = db
        .ensure_admin(&admin_user, admin_pw.as_deref())
        .context("初始化管理员失败")?
    {
        // 唯一允许把密码写进日志的地方：仅首次创建时出现一次。
        tracing::warn!("已创建管理员 {admin_user}，初始密码: {pw}（请登录后尽快修改）");
    }
    let (state, ingest) = AppState::new(db, cfg).context("加载节点列表失败")?;
    serve_on(listener, state, ingest, shutdown).await
}

/// 停机时等待活跃 WS 会话 / HTTP 连接收尾的上限。
const SESSION_DRAIN: Duration = Duration::from_secs(5);
const HTTP_DRAIN: Duration = Duration::from_secs(10);
const ROLLUP_DRAIN: Duration = Duration::from_secs(10);

/// 在已绑定的 listener 上提供服务（集成测试可绑定端口 0）。
///
/// 关停顺序：信号到达 -> 通知 WS 会话（发 Close 并退出）-> axum 优雅停止
/// -> 等待 WS 会话结束（有上限）-> flush 并回收入库线程 -> 等待 rollup 任务。每一步都有上限。
pub async fn serve_on(
    listener: tokio::net::TcpListener,
    state: AppState,
    ingest: IngestJoin,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> anyhow::Result<()> {
    let rollup = rollup::spawn(state.clone(), state.shutdown_rx());
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
    let serve = axum::serve(
        listener,
        router(state.clone()).into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(signal);
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
    if tokio::time::timeout(ROLLUP_DRAIN, rollup).await.is_err() {
        tracing::warn!("rollup 任务未在时限内结束");
    }
    served
}
