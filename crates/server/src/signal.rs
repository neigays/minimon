//! server 退出信号：Unix 下 SIGTERM（systemd / docker stop）或 SIGINT（Ctrl-C）任一到达即完成。
//!
//! 信号处理器在调用 [`shutdown_signal`] 时立即注册，而不是等到首次 poll：
//! 启动阶段（采集器初始化等）收到的 SIGTERM 也能优雅退出，不会按默认动作被杀死。

use std::future::{Future, pending};

/// 立即注册退出信号并返回等待它的 future。必须在 tokio 运行时上下文中调用。
pub fn shutdown_signal() -> impl Future<Output = ()> {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{Signal, SignalKind, signal};
        let reg = |kind: SignalKind, name: &'static str| -> Option<Signal> {
            signal(kind)
                .map_err(|e| tracing::warn!("监听 {name} 失败: {e}"))
                .ok()
        };
        let term = reg(SignalKind::terminate(), "SIGTERM");
        let int = reg(SignalKind::interrupt(), "SIGINT");
        async move {
            let wait = |s: Option<Signal>| async move {
                match s {
                    Some(mut s) => {
                        s.recv().await;
                    }
                    None => pending::<()>().await,
                }
            };
            tokio::select! {
                _ = wait(term) => {}
                _ = wait(int) => {}
            }
        }
    }
    #[cfg(not(unix))]
    {
        async {
            if let Err(e) = tokio::signal::ctrl_c().await {
                tracing::warn!("监听 Ctrl-C 失败: {e}");
                pending::<()>().await;
            }
        }
    }
}
