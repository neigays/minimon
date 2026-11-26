#[test]
fn version_flag() {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_minimon-server"))
        .arg("--version")
        .output()
        .unwrap();
    assert!(out.status.success());
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains(env!("CARGO_PKG_VERSION")), "输出: {s}");
}
