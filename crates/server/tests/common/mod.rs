//! 集成测试共享夹具：启动内置 server（端口 0）、原始 HTTP 请求、登录。
#![allow(dead_code)]

use minimon_server::config::ServerConfig;
use minimon_server::db::Db;
use minimon_server::state::AppState;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::oneshot;

pub const ADMIN_PASSWORD: &str = "test-password";

pub struct Fixture {
    pub port: u16,
    pub dir: tempfile::TempDir,
    pub db_path: PathBuf,
    pub shutdown: Option<oneshot::Sender<()>>,
    pub state: AppState,
    pub handle: Option<tokio::task::JoinHandle<anyhow::Result<()>>>,
}

impl Fixture {
    /// 停机并等待 serve_on 返回（含入库 flush），返回数据目录供重启复用。
    pub async fn stop(mut self) -> tempfile::TempDir {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        if let Some(h) = self.handle.take() {
            let _ = tokio::time::timeout(std::time::Duration::from_secs(15), h).await;
        }
        self.dir
    }
}

pub type Headers = axum::http::HeaderMap;

/// 用默认配置启动，`prepare` 可在启动前改配置。
pub async fn start(prepare: impl FnOnce(&mut ServerConfig)) -> Fixture {
    start_at(tempfile::tempdir().unwrap(), |_| {}, prepare).await
}

/// 在给定数据目录启动（可重启复用）；`prepare_db` 在 server 启动前直接改库。
pub async fn start_at(
    dir: tempfile::TempDir,
    prepare_db: impl FnOnce(&Db),
    prepare: impl FnOnce(&mut ServerConfig),
) -> Fixture {
    let db_path = dir.path().join("t.db");
    let db = Arc::new(Db::open(&db_path).unwrap());
    db.ensure_admin("admin", Some(ADMIN_PASSWORD)).unwrap();
    prepare_db(&db);
    let mut cfg = ServerConfig::default();
    prepare(&mut cfg);
    let (state, ingest) = AppState::new(db, cfg).unwrap();
    let state_out = state.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = oneshot::channel::<()>();
    let handle = tokio::spawn(minimon_server::serve_on(
        listener,
        state,
        ingest,
        async move {
            let _ = rx.await;
        },
    ));
    Fixture {
        port,
        dir,
        db_path,
        shutdown: Some(tx),
        state: state_out,
        handle: Some(handle),
    }
}

/// 发送原始请求，返回 (状态码, 头部, body)。`headers` 原样写入。
pub async fn raw(
    port: u16,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Option<&str>,
) -> (u16, Headers, String) {
    let mut s = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .unwrap();
    let mut req = format!("{method} {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n");
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    let body = body.unwrap_or("");
    if !body.is_empty() || method != "GET" {
        req.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    req.push_str("\r\n");
    req.push_str(body);
    s.write_all(req.as_bytes()).await.unwrap();
    let mut buf = Vec::new();
    s.read_to_end(&mut buf).await.unwrap();
    let text = String::from_utf8_lossy(&buf).into_owned();
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    let mut lines = head.lines();
    let code = lines
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|c| c.parse().ok())
        .unwrap();
    let mut map = Headers::new();
    for l in lines {
        if let Some((k, v)) = l.split_once(':')
            && let (Ok(k), Ok(v)) = (
                axum::http::HeaderName::from_bytes(k.trim().as_bytes()),
                axum::http::HeaderValue::from_str(v.trim()),
            )
        {
            map.append(k, v);
        }
    }
    (code, map, body.to_string())
}

/// JSON 请求（给了 json 就带 `Content-Type: application/json`）。
pub async fn http(
    port: u16,
    method: &str,
    path: &str,
    cookie: Option<&str>,
    json: Option<&str>,
) -> (u16, Headers, String) {
    let mut h: Vec<(&str, &str)> = Vec::new();
    if json.is_some() {
        h.push(("Content-Type", "application/json"));
    }
    if let Some(c) = cookie {
        h.push(("Cookie", c));
    }
    raw(port, method, path, &h, json).await
}

/// 从 Set-Cookie 取出 `minimon_session=<token>`（用于回传 Cookie 头）。
pub fn cookie_pair(headers: &Headers) -> Option<String> {
    headers
        .get_all("set-cookie")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find(|v| v.starts_with("minimon_session="))
        .and_then(|v| v.split(';').next())
        .map(str::to_string)
}

/// 用已知密码登录，返回可直接放进 `Cookie` 头的字符串。
pub async fn login(port: u16) -> String {
    let body = format!(r#"{{"username":"admin","password":"{ADMIN_PASSWORD}"}}"#);
    let (code, h, b) = http(port, "POST", "/api/auth/login", None, Some(&body)).await;
    assert_eq!(code, 200, "{b}");
    cookie_pair(&h).unwrap()
}
