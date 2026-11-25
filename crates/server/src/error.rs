//! 统一 JSON 错误：`{"error": "<中文说明>"}`。

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    msg: String,
}

impl ApiError {
    pub fn new(status: StatusCode, msg: impl Into<String>) -> Self {
        ApiError {
            status,
            msg: msg.into(),
        }
    }

    pub fn bad_request(msg: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, msg)
    }

    pub fn unauthorized(msg: impl Into<String>) -> Self {
        Self::new(StatusCode::UNAUTHORIZED, msg)
    }

    /// 后续任务（节点历史等）使用。
    #[allow(dead_code)]
    pub fn not_found(msg: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, msg)
    }

    /// 内部错误：细节只写日志，不返回给客户端。
    pub fn internal() -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "服务器内部错误")
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(serde_json::json!({ "error": self.msg }))).into_response()
    }
}

impl From<crate::db::DbError> for ApiError {
    fn from(e: crate::db::DbError) -> Self {
        tracing::error!(error = %e, "数据库操作失败");
        ApiError::internal()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn renders_json_body() {
        let r = ApiError::bad_request("坏了").into_response();
        assert_eq!(r.status(), StatusCode::BAD_REQUEST);
        let b = axum::body::to_bytes(r.into_body(), 1024).await.unwrap();
        assert_eq!(&b[..], r#"{"error":"坏了"}"#.as_bytes());
    }
}
