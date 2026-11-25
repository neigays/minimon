//! `/api/auth/*` 接口：登录、退出、当前用户、改密码。

use std::net::SocketAddr;

use axum::Router;
use axum::extract::{ConnectInfo, State};
use axum::http::header::SET_COOKIE;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use serde::Deserialize;

use crate::auth::{AdminOnly, AppJson, SESSION_COOKIE, Viewer, client_ip, is_https};
use crate::db::auth::SESSION_TTL_MS;
use crate::db::now_ms;
use crate::error::ApiError;
use crate::state::AppState;

const MIN_PASSWORD_CHARS: usize = 8;
const MAX_PASSWORD_BYTES: usize = 256;
const MSG_BAD_CREDENTIALS: &str = "用户名或密码错误";

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/auth/login", post(login))
        .route("/api/auth/logout", post(logout))
        .route("/api/auth/me", get(me))
        .route("/api/auth/password", put(change_password))
}

#[derive(Deserialize)]
struct LoginReq {
    username: String,
    password: String,
}

#[derive(Deserialize)]
struct PasswordReq {
    old_password: String,
    new_password: String,
}

fn session_cookie(token: &str, max_age_secs: i64, secure: bool) -> Result<HeaderValue, ApiError> {
    let mut c =
        format!("{SESSION_COOKIE}={token}; HttpOnly; SameSite=Lax; Path=/; Max-Age={max_age_secs}");
    if secure {
        c.push_str("; Secure");
    }
    HeaderValue::from_str(&c).map_err(|_| ApiError::internal())
}

fn username_json(name: &str) -> axum::Json<serde_json::Value> {
    axum::Json(serde_json::json!({ "username": name }))
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, crate::db::DbError> + Send + 'static,
) -> Result<T, ApiError> {
    match tokio::task::spawn_blocking(f).await {
        Ok(r) => Ok(r?),
        Err(e) => {
            tracing::error!(error = %e, "阻塞任务异常");
            Err(ApiError::internal())
        }
    }
}

/// argon2 每次约占 19 MiB：先拿许可再进阻塞线程池，许可持有到任务结束，限制并发内存。
async fn hashing<T: Send + 'static>(
    st: &AppState,
    f: impl FnOnce() -> Result<T, crate::db::DbError> + Send + 'static,
) -> Result<T, ApiError> {
    let permit = st
        .hash_gate
        .clone()
        .acquire_owned()
        .await
        .map_err(|_| ApiError::internal())?;
    blocking(move || {
        let _permit = permit;
        f()
    })
    .await
}

fn too_many() -> ApiError {
    ApiError::new(StatusCode::TOO_MANY_REQUESTS, "尝试次数过多，请稍后再试")
}

async fn login(
    State(st): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    AppJson(req): AppJson<LoginReq>,
) -> Result<Response, ApiError> {
    if req.username.is_empty() || req.password.is_empty() {
        return Err(ApiError::bad_request("用户名和密码不能为空"));
    }
    let real = st.cfg.real_ip_header.as_deref();
    let ip = client_ip(&headers, peer, real);
    let secure = is_https(&headers, real);
    // 检查与占位在同一把锁内完成，成功后清零；并发猜测也不能突破次数上限。
    if !st.login_limiter.try_reserve(ip) {
        return Err(too_many());
    }
    if req.password.len() > MAX_PASSWORD_BYTES {
        return Err(ApiError::unauthorized(MSG_BAD_CREDENTIALS));
    }
    let db = st.db.clone();
    let ok = hashing(&st, move || db.verify_admin(&req.username, &req.password)).await?;
    if !ok {
        return Err(ApiError::unauthorized(MSG_BAD_CREDENTIALS));
    }
    st.login_limiter.clear(ip);
    let db = st.db.clone();
    let (token, name) =
        blocking(move || Ok((db.create_session(now_ms())?, db.admin_username()?))).await?;
    let cookie = session_cookie(&token, SESSION_TTL_MS / 1000, secure)?;
    let mut resp = username_json(&name).into_response();
    resp.headers_mut().append(SET_COOKIE, cookie);
    Ok(resp)
}

async fn logout(
    State(st): State<AppState>,
    headers: HeaderMap,
    viewer: Viewer,
) -> Result<Response, ApiError> {
    if let Viewer::Admin { token } = viewer {
        let db = st.db.clone();
        blocking(move || db.delete_session(&token)).await?;
    }
    let secure = is_https(&headers, st.cfg.real_ip_header.as_deref());
    let mut resp = axum::Json(serde_json::json!({ "ok": true })).into_response();
    resp.headers_mut()
        .append(SET_COOKIE, session_cookie("", 0, secure)?);
    Ok(resp)
}

async fn me(State(st): State<AppState>, _admin: AdminOnly) -> Result<Response, ApiError> {
    let db = st.db.clone();
    let name = blocking(move || db.admin_username()).await?;
    Ok(username_json(&name).into_response())
}

async fn change_password(
    State(st): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    AdminOnly(token): AdminOnly,
    AppJson(req): AppJson<PasswordReq>,
) -> Result<Response, ApiError> {
    if req.new_password.chars().count() < MIN_PASSWORD_CHARS {
        return Err(ApiError::bad_request("新密码至少 8 位"));
    }
    if req.new_password.len() > MAX_PASSWORD_BYTES {
        return Err(ApiError::bad_request("新密码不能超过 256 字节"));
    }
    // 旧密码校验与登录共用限流，防止被盗会话用来猜密码。
    let ip = client_ip(&headers, peer, st.cfg.real_ip_header.as_deref());
    if !st.login_limiter.try_reserve(ip) {
        return Err(too_many());
    }
    if req.old_password.is_empty() || req.old_password.len() > MAX_PASSWORD_BYTES {
        return Err(ApiError::bad_request("原密码错误"));
    }
    let db = st.db.clone();
    let new = req.new_password;
    let old = req.old_password;
    let changed = hashing(&st, move || {
        let name = db.admin_username()?;
        if !db.verify_admin(&name, &old)? {
            return Ok(false);
        }
        db.change_admin_password(&new, Some(&token))?;
        Ok(true)
    })
    .await?;
    if !changed {
        return Err(ApiError::bad_request("原密码错误"));
    }
    st.login_limiter.clear(ip);
    Ok(axum::Json(serde_json::json!({ "ok": true })).into_response())
}
