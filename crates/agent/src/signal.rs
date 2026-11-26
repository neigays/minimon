//! 退出信号：Unix 下 SIGTERM（systemd stop）或 Ctrl-C 任一到达即完成。

use std::future::pending;

/// 等待 Ctrl-C；注册失败则记录警告并永不完成。
async fn ctrl_c() {
    if let Err(e) = tokio::signal::ctrl_c().await {
        tracing::warn!("监听 Ctrl-C 失败: {e}");
        pending::<()>().await;
    }
}

/// 等待 SIGTERM；注册失败则记录警告并永不完成。
#[cfg(unix)]
async fn sigterm() {
    use tokio::signal::unix::{SignalKind, signal};
    match signal(SignalKind::terminate()) {
        Ok(mut s) => {
            s.recv().await;
        }
        Err(e) => {
            tracing::warn!("监听 SIGTERM 失败: {e}");
            pending::<()>().await;
        }
    }
}

/// SIGTERM 或 Ctrl-C 任一到达即完成；非 Unix 仅 Ctrl-C。
pub async fn shutdown_signal() {
    #[cfg(unix)]
    tokio::select! {
        _ = ctrl_c() => {}
        _ = sigterm() => {}
    }
    #[cfg(not(unix))]
    ctrl_c().await;
}
