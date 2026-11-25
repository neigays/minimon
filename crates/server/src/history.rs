//! `GET /api/nodes/{id}/history?range=1h|24h|7d|30d`。

use crate::auth::Viewer;
use crate::db::history::{History, Range};
use crate::db::now_ms;
use crate::error::ApiError;
use crate::live::gate;
use crate::state::AppState;
use axum::Json;
use axum::extract::rejection::PathRejection;
use axum::extract::{Path, Query, State};
use serde::Deserialize;

#[derive(Deserialize)]
pub struct Params {
    range: Option<String>,
}

pub async fn get(
    State(state): State<AppState>,
    viewer: Viewer,
    id: Result<Path<i64>, PathRejection>,
    Query(p): Query<Params>,
) -> Result<Json<History>, ApiError> {
    gate(&state, &viewer)?;
    let Path(id) = id.map_err(|_| ApiError::not_found("节点不存在或未公开"))?;
    let admin = matches!(viewer, Viewer::Admin { .. });
    if !state
        .nodes()
        .iter()
        .any(|n| n.id == id && (admin || !n.hidden))
    {
        return Err(ApiError::not_found("节点不存在或未公开"));
    }
    let range: Range = p
        .range
        .as_deref()
        .and_then(|r| r.parse().ok())
        .ok_or_else(|| ApiError::bad_request("range 须为 1h、24h、7d 或 30d"))?;
    let db = state.db.clone();
    let h = tokio::task::spawn_blocking(move || db.history(id, range, now_ms()))
        .await
        .map_err(|_| ApiError::internal())??;
    Ok(Json(h))
}
