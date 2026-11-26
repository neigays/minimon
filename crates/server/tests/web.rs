mod common;

use minimon_server::config::ServerConfig;
use minimon_server::db::Db;
use minimon_server::state::AppState;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// 返回 (状态码, 小写头部文本, body)。
async fn get(port: u16, path: &str) -> (u16, String, String) {
    let mut s = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .unwrap();
    let req = format!("GET {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n");
    s.write_all(req.as_bytes()).await.unwrap();
    let mut buf = Vec::new();
    s.read_to_end(&mut buf).await.unwrap();
    let text = String::from_utf8_lossy(&buf).into_owned();
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    let code = head.split_whitespace().nth(1).unwrap().parse().unwrap();
    (code, head.to_lowercase(), body.to_string())
}

async fn start() -> (u16, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let db = Arc::new(Db::open(&dir.path().join("t.db")).unwrap());
    let (state, ingest) = AppState::new(db, ServerConfig::default()).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(minimon_server::serve_on(
        listener,
        state,
        ingest,
        std::future::pending(),
    ));
    (port, dir)
}

#[tokio::test]
async fn spa_fallback_and_api_not_swallowed() {
    let (port, _dir) = start().await;

    let (code, head, body) = get(port, "/some/route").await;
    assert_eq!(code, 200);
    assert!(head.contains("content-type: text/html"), "{head}");
    assert!(head.contains("cache-control: no-cache"), "{head}");
    assert!(
        body.contains("<html") || body.contains("<!doctype"),
        "{body}"
    );

    let (code, head, _) = get(port, "/index.html").await;
    assert_eq!(code, 200);
    assert!(head.contains("text/html"));

    let (code, head, _) = get(port, "/api/nonexistent").await;
    assert_eq!(code, 404);
    assert!(!head.contains("text/html"), "{head}");

    // 已有 API 路由不受影响
    let (code, head, _) = get(port, "/api/nodes").await;
    assert_eq!(code, 200);
    assert!(head.contains("application/json"));
}

#[tokio::test]
async fn install_script_served() {
    let (port, _dir) = start().await;
    let (code, head, body) = get(port, "/install.sh").await;
    assert_eq!(code, 200);
    assert!(head.contains("content-type: text/x-shellscript"), "{head}");
    assert!(head.contains("cache-control: no-cache"), "{head}");
    assert!(body.starts_with("#!/"));
    assert!(body.contains("neigays/minimon"));
}

#[tokio::test]
async fn install_script_public_even_when_login_required() {
    let fx = common::start(|_| {}).await;
    let cookie = common::login(fx.port).await;
    let (code, _, b) = common::http(
        fx.port,
        "PUT",
        "/api/admin/settings",
        Some(&cookie),
        Some(r#"{"require_login": true}"#),
    )
    .await;
    assert_eq!(code, 200, "{b}");
    // 确认整站确实已要求登录
    let (code, _, _) = common::http(fx.port, "GET", "/api/nodes", None, None).await;
    assert_eq!(code, 401);
    for path in ["/install.sh", "/install.sh?x=1"] {
        let (code, _, body) = common::http(fx.port, "GET", path, None, None).await;
        assert_eq!(code, 200, "{path}");
        assert!(body.starts_with("#!/"));
    }
    let (code, _, _) = common::http(fx.port, "HEAD", "/install.sh", None, None).await;
    assert_eq!(code, 200);
}
