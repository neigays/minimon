//! `/api/admin/*`：节点管理与系统设置（全部 `AdminOnly`）。
//!
//! 每次变更成功后：重载节点缓存，再 `publish_resync()`，让已打开的页面立即同步。

use crate::auth::{AdminOnly, AppJson};
use crate::db::{DbError, NodePatch};
use crate::error::ApiError;
use crate::state::AppState;
use crate::views::{self, AdminNode};
use axum::extract::{FromRequestParts, Path, State};
use axum::http::StatusCode;
use axum::http::request::Parts;
use axum::routing::{get, patch, post, put};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

const MSG_NO_NODE: &str = "节点不存在";

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/admin/nodes", get(list).post(create))
        .route("/api/admin/nodes/order", put(reorder))
        .route("/api/admin/nodes/{id}", patch(update).delete(remove))
        .route("/api/admin/nodes/{id}/token", post(reset_token))
        .route("/api/admin/settings", get(get_settings).put(put_settings))
}

/// 路径里的节点 id；无法解析时返回 JSON 404 而不是 axum 的纯文本 400。
pub struct NodeId(pub i64);

impl FromRequestParts<AppState> for NodeId {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        Path::<i64>::from_request_parts(parts, state)
            .await
            .map(|Path(id)| NodeId(id))
            .map_err(|_| ApiError::not_found(MSG_NO_NODE))
    }
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, DbError> + Send + 'static,
) -> Result<T, ApiError> {
    match tokio::task::spawn_blocking(f).await {
        Ok(r) => Ok(r?),
        Err(_) => Err(ApiError::internal()),
    }
}

/// 变更成功后的收尾：重载缓存并通知所有 live 会话重新同步。
async fn refresh(state: &AppState) -> Result<(), ApiError> {
    state.reload_nodes().await?;
    state.publish_resync();
    Ok(())
}

// ---------------------------------------------------------------- 校验

/// 校验并规范化后的字段；`None` 表示请求里没给。
#[derive(Default)]
struct Fields {
    name: Option<String>,
    group: Option<String>,
    region: Option<String>,
    note: Option<String>,
}

fn validate(
    name: Option<String>,
    group: Option<String>,
    region: Option<String>,
    note: Option<String>,
) -> Result<Fields, ApiError> {
    let name = name
        .map(|n| {
            let n = n.trim().to_string();
            match n.chars().count() {
                1..=64 => Ok(n),
                _ => Err(ApiError::bad_request("名称需为 1 到 64 个字符")),
            }
        })
        .transpose()?;
    let group = group
        .map(|g| {
            let g = g.trim().to_string();
            if g.chars().count() > 32 {
                return Err(ApiError::bad_request("分组不能超过 32 个字符"));
            }
            Ok(g)
        })
        .transpose()?;
    let region = region
        .map(|r| {
            let r = r.trim();
            if r.is_empty() {
                Ok(String::new())
            } else if r.chars().count() == 2 && r.chars().all(|c| c.is_ascii_alphabetic()) {
                Ok(r.to_ascii_uppercase())
            } else {
                Err(ApiError::bad_request("地区需为空或 2 位字母代码"))
            }
        })
        .transpose()?;
    let note = note
        .map(|n| {
            if n.chars().count() > 500 {
                return Err(ApiError::bad_request("备注不能超过 500 个字符"));
            }
            Ok(n)
        })
        .transpose()?;
    Ok(Fields {
        name,
        group,
        region,
        note,
    })
}

// ---------------------------------------------------------------- 节点

pub async fn list(_: AdminOnly, State(state): State<AppState>) -> Json<Vec<AdminNode>> {
    Json(views::admin_nodes(&state))
}

#[derive(Deserialize)]
struct CreateReq {
    name: String,
    group: Option<String>,
    region: Option<String>,
    note: Option<String>,
}

#[derive(Serialize)]
struct Created {
    node: AdminNode,
    token: String,
}

async fn create(
    _: AdminOnly,
    State(state): State<AppState>,
    AppJson(req): AppJson<CreateReq>,
) -> Result<(StatusCode, Json<Created>), ApiError> {
    let f = validate(Some(req.name), req.group, req.region, req.note)?;
    let db = state.db.clone();
    let (name, group, region, note) = (
        f.name.unwrap_or_default(),
        f.group.unwrap_or_default(),
        f.region.unwrap_or_default(),
        f.note.unwrap_or_default(),
    );
    let (node, token) = blocking(move || db.add_node_with(&name, &group, &region, &note)).await?;
    // 库已提交、token 只会返回这一次：缓存刷新失败只记日志（下次重载自愈），不丢 token。
    if let Err(e) = state.reload_nodes().await {
        tracing::error!(error = %e, "新增节点后刷新缓存失败");
    }
    state.publish_resync();
    let node =
        views::admin_node(&state, node.id).unwrap_or_else(|| views::admin_node_of(&state, &node));
    Ok((StatusCode::CREATED, Json(Created { node, token })))
}

#[derive(Deserialize)]
struct PatchReq {
    name: Option<String>,
    group: Option<String>,
    region: Option<String>,
    note: Option<String>,
    hidden: Option<bool>,
}

async fn update(
    _: AdminOnly,
    State(state): State<AppState>,
    NodeId(id): NodeId,
    AppJson(req): AppJson<PatchReq>,
) -> Result<Json<AdminNode>, ApiError> {
    let f = validate(req.name, req.group, req.region, req.note)?;
    let patch = NodePatch {
        name: f.name,
        group: f.group,
        region: f.region,
        note: f.note,
        hidden: req.hidden,
    };
    let db = state.db.clone();
    if !blocking(move || db.update_node(id, &patch)).await? {
        return Err(ApiError::not_found(MSG_NO_NODE));
    }
    refresh(&state).await?;
    views::admin_node(&state, id)
        .map(Json)
        .ok_or_else(|| ApiError::not_found(MSG_NO_NODE))
}

async fn remove(
    _: AdminOnly,
    State(state): State<AppState>,
    NodeId(id): NodeId,
) -> Result<StatusCode, ApiError> {
    if !state.has_node(id) {
        return Err(ApiError::not_found(MSG_NO_NODE));
    }
    // 先踢掉会话（之后不再有新上报入队），再冲刷已入队的上报，最后删库。
    state.registry().revoke(id);
    state.flush_ingest().await;
    let db = state.db.clone();
    if !blocking(move || db.delete_node(id)).await? {
        return Err(ApiError::not_found(MSG_NO_NODE));
    }
    // id 会被 SQLite 复用：内存里的最后已知上报必须一并清掉。
    state.forget_node(id);
    // 再吊销一次：删除前刚通过认证、尚在握手的 agent 也要被拒绝。
    state.registry().revoke(id);
    if let Err(e) = state.reload_nodes().await {
        tracing::error!(error = %e, "删除节点后刷新缓存失败");
    }
    state.publish_resync();
    Ok(StatusCode::NO_CONTENT)
}

async fn reset_token(
    _: AdminOnly,
    State(state): State<AppState>,
    NodeId(id): NodeId,
) -> Result<Json<serde_json::Value>, ApiError> {
    let db = state.db.clone();
    let token = blocking(move || db.reset_token(id))
        .await?
        .ok_or_else(|| ApiError::not_found(MSG_NO_NODE))?;
    // 旧 token 已失效：踢掉在线的 agent（以 Error{unauthorized} 关闭）。
    state.registry().revoke(id);
    // token 只返回这一次：缓存刷新失败不得让它丢失。
    if let Err(e) = state.reload_nodes().await {
        tracing::error!(error = %e, "重置 token 后刷新缓存失败");
    }
    state.publish_resync();
    Ok(Json(serde_json::json!({ "token": token })))
}

#[derive(Deserialize)]
struct OrderReq {
    ids: Vec<i64>,
}

async fn reorder(
    _: AdminOnly,
    State(state): State<AppState>,
    AppJson(req): AppJson<OrderReq>,
) -> Result<StatusCode, ApiError> {
    let current: Vec<i64> = state.nodes().iter().map(|n| n.id).collect();
    let mut seen = std::collections::HashSet::new();
    for id in &req.ids {
        if !current.contains(id) {
            return Err(ApiError::bad_request("排序列表包含不存在的节点"));
        }
        if !seen.insert(*id) {
            return Err(ApiError::bad_request("排序列表包含重复的节点"));
        }
    }
    // 未列出的节点保持原相对顺序，排在后面。
    let mut ids = req.ids;
    ids.extend(current.into_iter().filter(|id| !seen.contains(id)));
    let db = state.db.clone();
    blocking(move || db.reorder(&ids)).await?;
    refresh(&state).await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------- 设置

pub async fn get_settings(_: AdminOnly, State(state): State<AppState>) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "require_login": state.require_login(),
        "public_url": state.cfg.public_url,
    }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SettingsReq {
    require_login: bool,
}

async fn put_settings(
    _: AdminOnly,
    State(state): State<AppState>,
    AppJson(req): AppJson<SettingsReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let db = state.db.clone();
    let v = if req.require_login { "1" } else { "0" };
    blocking(move || db.set_setting("require_login", v)).await?;
    state.set_require_login(req.require_login);
    state.publish_resync();
    Ok(Json(serde_json::json!({
        "require_login": req.require_login,
        "public_url": state.cfg.public_url,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(name: &str, region: &str) -> Result<Fields, ApiError> {
        validate(Some(name.into()), None, Some(region.into()), None)
    }

    #[test]
    fn validation_rules() {
        assert_eq!(v("  hk  ", "").unwrap().name.as_deref(), Some("hk"));
        assert!(v("   ", "").is_err());
        assert!(v(&"a".repeat(65), "").is_err());
        assert!(v(&"节".repeat(64), "").is_ok());
        assert_eq!(v("a", "hk").unwrap().region.as_deref(), Some("HK"));
        assert!(v("a", "USA").is_err());
        assert!(v("a", "1a").is_err());
        assert!(validate(None, Some("g".repeat(33)), None, None).is_err());
        assert!(validate(None, None, None, Some("n".repeat(501))).is_err());
        assert!(validate(None, None, None, Some("n".repeat(500))).is_ok());
    }
}
