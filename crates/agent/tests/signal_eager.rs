//! 退出信号必须在调用 `shutdown_signal()` 时立即注册，而不是等到首次 poll：
//! 否则启动阶段（采集器初始化等）收到的 SIGTERM 会按默认动作直接杀死进程。
//! 单独成一个测试二进制：修复前本测试进程会被 SIGTERM 杀死。

#[cfg(unix)]
#[tokio::test]
async fn sigterm_handled_before_first_poll() {
    let fut = minimon_agent::signal::shutdown_signal();
    let st = std::process::Command::new("kill")
        .args(["-TERM", &std::process::id().to_string()])
        .status()
        .unwrap();
    assert!(st.success());
    tokio::time::timeout(std::time::Duration::from_secs(2), fut)
        .await
        .expect("注册后应收到 SIGTERM");
}
