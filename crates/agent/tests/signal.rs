//! 进程级测试：SIGTERM 优雅退出与 --version。

#[cfg(unix)]
#[test]
fn sigterm_during_backoff_exits() {
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    // 取一个未监听的端口：绑定后立刻释放。
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let mut child = Command::new(env!("CARGO_BIN_EXE_minimon-agent"))
        .args([
            "--server",
            &format!("ws://127.0.0.1:{port}"),
            "--token",
            "t",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_secs(1));
    let status = Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status()
        .unwrap();
    assert!(status.success());
    let start = Instant::now();
    loop {
        if let Some(st) = child.try_wait().unwrap() {
            assert_eq!(st.code(), Some(0), "应以 0 退出，实际 {st:?}");
            return;
        }
        if start.elapsed() > Duration::from_secs(5) {
            let _ = child.kill();
            panic!("SIGTERM 后 5 秒内未退出");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn version_flag() {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_minimon-agent"))
        .arg("--version")
        .output()
        .unwrap();
    assert!(out.status.success());
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains(env!("CARGO_PKG_VERSION")), "输出: {s}");
}
