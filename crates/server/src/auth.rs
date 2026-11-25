//! 登录、会话、限流、CSRF 与真实客户端 IP。

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::extract::rejection::JsonRejection;
use axum::extract::{FromRequest, FromRequestParts, Request};
use axum::http::header::{CONTENT_TYPE, COOKIE};
use axum::http::request::Parts;
use axum::http::{HeaderMap, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use serde::de::DeserializeOwned;

use crate::db::now_ms;
use crate::error::ApiError;
use crate::state::AppState;

pub const SESSION_COOKIE: &str = "minimon_session";

// ---------------------------------------------------------------- 真实 IP

/// 取客户端 IP。未配置 `real_ip_header`（或为空白）时用 TCP 对端；配置后取该头部最后一个
/// 逗号分隔项（离 server 最近的可信代理追加的那个）。该项不是合法 IP 时退回对端，
/// 不会去找前面的项（前面的项可被客户端伪造）。
pub fn client_ip(headers: &HeaderMap, peer: SocketAddr, real_ip_header: Option<&str>) -> IpAddr {
    let Some(name) = real_ip_header.map(str::trim).filter(|h| !h.is_empty()) else {
        return peer.ip();
    };
    last_item(headers, name)
        .and_then(|v| parse_ip(v.trim()))
        .unwrap_or_else(|| peer.ip())
}

fn parse_ip(s: &str) -> Option<IpAddr> {
    s.parse::<IpAddr>()
        .ok()
        .or_else(|| s.parse::<SocketAddr>().ok().map(|a| a.ip()))
        .or_else(|| {
            s.strip_prefix('[')
                .and_then(|r| r.strip_suffix(']'))
                .and_then(|r| r.parse().ok())
        })
}

/// 头部（可能多行）所有值按 `,` 展开后的最后一项。
fn last_item<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get_all(name)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .next_back()
}

/// 仅在配置了 `real_ip_header`（即确认处于可信反向代理之后）时才信任 `X-Forwarded-Proto: https`。
pub fn is_https(headers: &HeaderMap, real_ip_header: Option<&str>) -> bool {
    if real_ip_header.is_none_or(|h| h.trim().is_empty()) {
        return false;
    }
    last_item(headers, "x-forwarded-proto").is_some_and(|v| v.trim().eq_ignore_ascii_case("https"))
}

// ---------------------------------------------------------------- 限流

const LIMIT_FAILS: usize = 5;
const LIMIT_WINDOW: Duration = Duration::from_secs(5 * 60);
const LIMIT_MAX_IPS: usize = 10_000;

/// 同一 IP 5 分钟内失败 5 次后拒绝。内存计数，重启清零。
#[derive(Default)]
pub struct LoginLimiter {
    fails: Mutex<HashMap<IpAddr, Vec<Instant>>>,
}

impl LoginLimiter {
    fn map(&self) -> std::sync::MutexGuard<'_, HashMap<IpAddr, Vec<Instant>>> {
        self.fails.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 返回 true 表示允许尝试。
    pub fn check(&self, ip: IpAddr) -> bool {
        self.check_at(ip, Instant::now())
    }

    pub fn fail(&self, ip: IpAddr) {
        self.fail_at(ip, Instant::now());
    }

    pub fn clear(&self, ip: IpAddr) {
        self.map().remove(&ip);
    }

    fn check_at(&self, ip: IpAddr, now: Instant) -> bool {
        let mut m = self.map();
        let Some(v) = m.get_mut(&ip) else {
            return true;
        };
        v.retain(|t| now.saturating_duration_since(*t) < LIMIT_WINDOW);
        if v.is_empty() {
            m.remove(&ip);
            return true;
        }
        v.len() < LIMIT_FAILS
    }

    fn fail_at(&self, ip: IpAddr, now: Instant) {
        let mut m = self.map();
        if m.len() >= LIMIT_MAX_IPS {
            m.retain(|_, v| {
                v.retain(|t| now.saturating_duration_since(*t) < LIMIT_WINDOW);
                !v.is_empty()
            });
        }
        if m.len() < LIMIT_MAX_IPS || m.contains_key(&ip) {
            m.entry(ip).or_default().push(now);
        }
    }
}

// ---------------------------------------------------------------- 身份

pub(crate) fn session_token(headers: &HeaderMap) -> Option<String> {
    headers
        .get_all(COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|kv| kv.trim().split_once('='))
        .find(|(k, _)| *k == SESSION_COOKIE)
        .map(|(_, v)| v.to_string())
        .filter(|v| !v.is_empty())
}

pub enum Viewer {
    Guest,
    Admin { token: String },
}

impl FromRequestParts<AppState> for Viewer {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let Some(token) = session_token(&parts.headers) else {
            return Ok(Viewer::Guest);
        };
        // 不在内存缓存会话：每次查库，CLI 重置密码后立即生效。
        let db = state.db.clone();
        let t = token.clone();
        let valid = tokio::task::spawn_blocking(move || db.session_valid(&t, now_ms()))
            .await
            .map_err(|_| ApiError::internal())??;
        Ok(if valid {
            Viewer::Admin { token }
        } else {
            Viewer::Guest
        })
    }
}

/// 仅管理员；携带会话 token。
pub struct AdminOnly(pub String);

impl FromRequestParts<AppState> for AdminOnly {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        match Viewer::from_request_parts(parts, state).await? {
            Viewer::Admin { token } => Ok(AdminOnly(token)),
            Viewer::Guest => Err(ApiError::unauthorized("请先登录")),
        }
    }
}

// ---------------------------------------------------------------- JSON / CSRF

/// 修改类请求必须是 `application/json`（配合 SameSite=Lax 防 CSRF）。
pub async fn require_json(req: Request, next: Next) -> Response {
    if matches!(
        *req.method(),
        Method::POST | Method::PUT | Method::PATCH | Method::DELETE
    ) {
        let is_json = req
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split(';').next())
            .is_some_and(|m| m.trim().eq_ignore_ascii_case("application/json"));
        if !is_json {
            return ApiError::new(StatusCode::UNSUPPORTED_MEDIA_TYPE, "请求需为 JSON")
                .into_response();
        }
    }
    next.run(req).await
}

/// `axum::Json` 的包装：解析失败时返回 JSON 错误而不是纯文本。
pub struct AppJson<T>(pub T);

impl<S: Send + Sync, T: DeserializeOwned> FromRequest<S> for AppJson<T> {
    type Rejection = ApiError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        match axum::Json::<T>::from_request(req, state).await {
            Ok(axum::Json(v)) => Ok(AppJson(v)),
            Err(JsonRejection::MissingJsonContentType(_)) => Err(ApiError::new(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "请求需为 JSON",
            )),
            Err(JsonRejection::BytesRejection(r)) => {
                Err(ApiError::new(r.status(), "请求体无法读取"))
            }
            Err(_) => Err(ApiError::bad_request("请求体不是有效的 JSON 或缺少字段")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.append(
                axum::http::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                HeaderValue::from_str(v).unwrap(),
            );
        }
        h
    }

    const PEER: &str = "10.0.0.2:5000";

    #[test]
    fn client_ip_rightmost_only_when_configured() {
        let peer: SocketAddr = PEER.parse().unwrap();
        let h = headers(&[("x-forwarded-for", "1.1.1.1, 2.2.2.2")]);
        assert_eq!(client_ip(&h, peer, None).to_string(), "10.0.0.2");
        assert_eq!(
            client_ip(&h, peer, Some("X-Forwarded-For")).to_string(),
            "2.2.2.2"
        );
        assert_eq!(
            client_ip(
                &headers(&[("x-forwarded-for", "garbage")]),
                peer,
                Some("X-Forwarded-For")
            )
            .to_string(),
            "10.0.0.2"
        );
    }

    #[test]
    fn client_ip_edge_cases() {
        let peer: SocketAddr = PEER.parse().unwrap();
        let x = |h: &HeaderMap, cfg| client_ip(h, peer, cfg).to_string();
        // 空 / 空白配置等同未配置
        let h = headers(&[("x-forwarded-for", "2.2.2.2")]);
        assert_eq!(x(&h, Some("")), "10.0.0.2");
        assert_eq!(x(&h, Some("  ")), "10.0.0.2");
        // 最后一项不合法时不回头取前面可伪造的项
        let h = headers(&[("x-forwarded-for", "1.1.1.1, garbage")]);
        assert_eq!(x(&h, Some("x-forwarded-for")), "10.0.0.2");
        // 多行头部取最后一行的最后一项
        let h = headers(&[
            ("x-forwarded-for", "1.1.1.1"),
            ("x-forwarded-for", "3.3.3.3, 4.4.4.4"),
        ]);
        assert_eq!(x(&h, Some("x-forwarded-for")), "4.4.4.4");
        // 头缺失、IPv6、带端口
        assert_eq!(x(&HeaderMap::new(), Some("x-forwarded-for")), "10.0.0.2");
        let h = headers(&[("cf-connecting-ip", "2001:db8::1")]);
        assert_eq!(x(&h, Some("CF-Connecting-IP")), "2001:db8::1");
        let h = headers(&[("x-real-ip", "5.6.7.8:1234")]);
        assert_eq!(x(&h, Some("X-Real-IP")), "5.6.7.8");
        // 非法头名不 panic
        assert_eq!(x(&h, Some("bad name")), "10.0.0.2");
    }

    #[test]
    fn https_trusted_only_with_real_ip_header() {
        let h = headers(&[("x-forwarded-proto", "https")]);
        assert!(!is_https(&h, None));
        assert!(!is_https(&h, Some("  ")));
        assert!(is_https(&h, Some("X-Forwarded-For")));
        assert!(!is_https(
            &headers(&[("x-forwarded-proto", "http")]),
            Some("x")
        ));
        assert!(is_https(
            &headers(&[("x-forwarded-proto", "HTTPS")]),
            Some("x")
        ));
        assert!(!is_https(&HeaderMap::new(), Some("x")));
    }

    #[test]
    fn limiter_blocks_after_five_failures() {
        let l = LoginLimiter::default();
        let ip: IpAddr = "1.2.3.4".parse().unwrap();
        let other: IpAddr = "1.2.3.5".parse().unwrap();
        for _ in 0..5 {
            assert!(l.check(ip));
            l.fail(ip);
        }
        assert!(!l.check(ip));
        assert!(l.check(other));
        l.clear(ip);
        assert!(l.check(ip));
    }

    #[test]
    fn limiter_window_expires() {
        let l = LoginLimiter::default();
        let ip: IpAddr = "1.2.3.4".parse().unwrap();
        let t0 = Instant::now();
        for _ in 0..5 {
            l.fail_at(ip, t0);
        }
        assert!(!l.check_at(ip, t0 + LIMIT_WINDOW - Duration::from_secs(1)));
        assert!(l.check_at(ip, t0 + LIMIT_WINDOW));
        assert!(l.map().is_empty());
    }

    #[test]
    fn cookie_parsing() {
        let h = headers(&[("cookie", "a=1; minimon_session=abc; b=2")]);
        assert_eq!(session_token(&h).as_deref(), Some("abc"));
        assert_eq!(
            session_token(&headers(&[("cookie", "xminimon_session=abc")])),
            None
        );
        assert_eq!(
            session_token(&headers(&[("cookie", "minimon_session=")])),
            None
        );
        assert_eq!(session_token(&HeaderMap::new()), None);
    }
}
