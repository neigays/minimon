//! `GET /api/nodes/{id}/probes?range=1h|24h|7d|30d`：探测历史（访客可读，不含目标地址）。

use crate::auth::Viewer;
use crate::db::history::Range;
use crate::db::now_ms;
use crate::db::probe_rollup::{ProbeGranularity, ProbeSeries};
use crate::error::ApiError;
use crate::live::gate;
use crate::probe::applies;
use crate::state::AppState;
use axum::Json;
use axum::extract::rejection::PathRejection;
use axum::extract::{Path, Query, State};
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
pub struct Params {
    range: Option<String>,
}

#[derive(Serialize)]
pub struct TaskSeries {
    name: String,
    #[serde(flatten)]
    series: ProbeSeries,
}

#[derive(Serialize)]
pub struct ProbeHistory {
    granularity: ProbeGranularity,
    tasks: Vec<TaskSeries>,
}

pub async fn get(
    State(state): State<AppState>,
    viewer: Viewer,
    id: Result<Path<i64>, PathRejection>,
    Query(p): Query<Params>,
) -> Result<Json<ProbeHistory>, ApiError> {
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
    let gran = if range == Range::H1 {
        ProbeGranularity::Raw
    } else {
        ProbeGranularity::M5
    };
    // 只返回当前适用于该节点的任务（sort, id 序）；只取 id 与名称，不碰 target。
    let tasks: Vec<(i64, String)> = state
        .probe_tasks()
        .iter()
        .filter(|t| applies(t, id))
        .map(|t| (t.id, t.name.clone()))
        .collect();
    let ids: Vec<i64> = tasks.iter().map(|t| t.0).collect();
    let db = state.db.clone();
    let now = now_ms();
    let series = tokio::task::spawn_blocking(move || {
        db.probe_history(id, &ids, gran, now - range.span_ms(), now)
    })
    .await
    .map_err(|_| ApiError::internal())??;
    let tasks = tasks
        .into_iter()
        .zip(series)
        .map(|((_, name), series)| TaskSeries { name, series })
        .collect();
    Ok(Json(ProbeHistory {
        granularity: gran,
        tasks,
    }))
}
