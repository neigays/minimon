//! `GET /install.sh`：公开提供内嵌的 agent 安装脚本（不做登录检查）。

use axum::http::header;
use axum::response::IntoResponse;

const SCRIPT: &str = include_str!("../../../deploy/install.sh");

pub async fn get() -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "text/x-shellscript; charset=utf-8"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        SCRIPT,
    )
}
