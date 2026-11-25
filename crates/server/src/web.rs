//! 内嵌前端静态资源与 SPA 回退。

use axum::http::{HeaderValue, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "../../web/dist"]
struct Assets;

const INDEX: &str = "index.html";

/// router fallback：先按路径找文件，找不到回退 index.html；`/api` 下未知路径返回 404。
pub async fn fallback(uri: Uri) -> Response {
    let path = uri.path();
    if path == "/api" || path.starts_with("/api/") {
        return StatusCode::NOT_FOUND.into_response();
    }
    let rel = path.trim_start_matches('/');
    if !rel.is_empty()
        && let Some(r) = serve(rel)
    {
        return r;
    }
    serve(INDEX).unwrap_or_else(|| StatusCode::NOT_FOUND.into_response())
}

/// 带 hash 的 `assets/` 下文件可永久缓存，其余（含 index.html）每次校验。
fn cache_control(path: &str) -> &'static str {
    if path.starts_with("assets/") {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    }
}

fn serve(path: &str) -> Option<Response> {
    let file = Assets::get(path)?;
    let cache = cache_control(path);
    let mime = file.metadata.mimetype().to_string();
    let mut resp = file.data.into_owned().into_response();
    let h = resp.headers_mut();
    if let Ok(v) = HeaderValue::from_str(&mime) {
        h.insert(header::CONTENT_TYPE, v);
    }
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    Some(resp)
}

#[cfg(test)]
mod tests {
    use super::cache_control;

    #[test]
    fn cache_control_branches() {
        let immutable = "public, max-age=31536000, immutable";
        assert_eq!(cache_control("assets/index-abc.js"), immutable);
        assert_eq!(cache_control("assets/sub/x.css"), immutable);
        assert_eq!(cache_control("index.html"), "no-cache");
        assert_eq!(cache_control("favicon.ico"), "no-cache");
        assert_eq!(cache_control("assetsx/a.js"), "no-cache");
        assert_eq!(cache_control("sub/assets/a.js"), "no-cache");
    }
}
