//! `/api/admin/probes*`：探测任务管理与三网模板（全部 `AdminOnly`）。

use crate::admin::blocking;
use crate::auth::{AdminOnly, AppJson};
use crate::db::{NewProbeTask, ProbeTaskPatch, ProbeTaskRow};
use crate::error::ApiError;
use crate::state::AppState;
use axum::extract::{FromRequestParts, Path, State};
use axum::http::StatusCode;
use axum::http::request::Parts;
use axum::routing::{get, post};
use axum::{Json, Router};
use minimon_common::protocol::ProbeKind;
use serde::Deserialize;
use std::net::{IpAddr, Ipv6Addr};

const MSG_NO_TASK: &str = "探测任务不存在";

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/admin/probes", get(list).post(create))
        .route("/api/admin/probes/isp-template", post(isp_template))
        .route(
            "/api/admin/probes/{id}",
            axum::routing::patch(update).delete(remove),
        )
}

/// 路径里的任务 id；无法解析时返回 JSON 404。
struct TaskId(i64);

impl FromRequestParts<AppState> for TaskId {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        Path::<i64>::from_request_parts(parts, state)
            .await
            .map(|Path(id)| TaskId(id))
            .map_err(|_| ApiError::not_found(MSG_NO_TASK))
    }
}

async fn refresh(state: &AppState) -> Result<(), ApiError> {
    state.reload_probe_tasks().await?;
    state.publish_resync();
    Ok(())
}

// ---------------------------------------------------------------- 校验

fn valid_domain(h: &str) -> bool {
    if h.is_empty() || h.len() > 253 {
        return false;
    }
    let labels: Vec<&str> = h.split('.').collect();
    let ok = labels.iter().all(|l| {
        !l.is_empty()
            && l.len() <= 63
            && !l.starts_with('-')
            && !l.ends_with('-')
            && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
    });
    // 纯数字的末段（如 999.1.1.1）不是合法域名。
    ok && !labels
        .last()
        .is_some_and(|l| l.bytes().all(|b| b.is_ascii_digit()))
}

/// IPv4 / 域名主机（不含 IPv6 方括号形式）。
fn valid_host(h: &str) -> bool {
    h.parse::<IpAddr>().is_ok_and(|ip| ip.is_ipv4()) || valid_domain(h)
}

/// `host` 或 `[v6]`，可选 `:port`。返回 (主机, 端口文本)。
fn split_host_port(s: &str) -> Option<(&str, Option<&str>)> {
    if let Some(rest) = s.strip_prefix('[') {
        let (h, after) = rest.split_once(']')?;
        h.parse::<Ipv6Addr>().ok()?;
        return match after {
            "" => Some((h, None)),
            _ => Some((h, Some(after.strip_prefix(':')?))),
        };
    }
    let (h, port) = match s.split_once(':') {
        Some((h, p)) => (h, Some(p)),
        None => (s, None),
    };
    valid_host(h).then_some((h, port))
}

fn valid_port(p: &str) -> bool {
    !p.is_empty()
        && p.len() <= 5
        && p.bytes().all(|b| b.is_ascii_digit())
        && p.parse::<u32>().is_ok_and(|n| (1..=65535).contains(&n))
}

fn validate_target(kind: ProbeKind, target: &str) -> Result<(), ApiError> {
    if target.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(ApiError::bad_request("目标地址不能包含空白字符"));
    }
    let ok = match kind {
        ProbeKind::Icmp => target.parse::<IpAddr>().is_ok() || valid_domain(target),
        ProbeKind::Tcp => split_host_port(target).is_some_and(|(_, p)| p.is_some_and(valid_port)),
        ProbeKind::Http => {
            let rest = target
                .strip_prefix("http://")
                .or_else(|| target.strip_prefix("https://"));
            rest.is_some_and(|r| {
                let auth = r.split(['/', '?', '#']).next().unwrap_or("");
                split_host_port(auth).is_some_and(|(_, p)| p.is_none_or(valid_port))
            })
        }
    };
    if ok {
        return Ok(());
    }
    Err(ApiError::bad_request(match kind {
        ProbeKind::Icmp => "ICMP 目标需为 IP 或域名",
        ProbeKind::Tcp => "TCP 目标需为 host:port（端口 1 到 65535，IPv6 写作 [::1]:80）",
        ProbeKind::Http => "HTTP 目标需为 http:// 或 https:// 开头的 URL",
    }))
}

fn validate_name(name: &str) -> Result<String, ApiError> {
    let n = name.trim();
    match n.chars().count() {
        1..=32 => Ok(n.to_string()),
        _ => Err(ApiError::bad_request("名称需为 1 到 32 个字符")),
    }
}

fn ranged(v: i64, lo: i64, hi: i64, msg: &str) -> Result<u32, ApiError> {
    if (lo..=hi).contains(&v) {
        u32::try_from(v).map_err(|_| ApiError::bad_request(msg))
    } else {
        Err(ApiError::bad_request(msg))
    }
}

fn validate_interval(v: i64) -> Result<u32, ApiError> {
    ranged(v, 10, 3600, "探测间隔需为 10 到 3600 秒")
}
fn validate_samples(v: i64) -> Result<u32, ApiError> {
    ranged(v, 1, 20, "每轮样本数需为 1 到 20")
}
fn validate_timeout(v: i64) -> Result<u32, ApiError> {
    ranged(v, 500, 10000, "超时需为 500 到 10000 毫秒")
}

/// 去重升序，并确认都是现有节点。
fn validate_nodes(state: &AppState, ids: &[i64]) -> Result<Vec<i64>, ApiError> {
    let mut ids = ids.to_vec();
    ids.sort_unstable();
    ids.dedup();
    if ids.is_empty() {
        return Err(ApiError::bad_request("请选择至少一个节点"));
    }
    let nodes = state.nodes();
    if ids.iter().any(|id| !nodes.iter().any(|n| n.id == *id)) {
        return Err(ApiError::bad_request("所选节点不存在"));
    }
    Ok(ids)
}

// ---------------------------------------------------------------- 接口

async fn list(_: AdminOnly, State(state): State<AppState>) -> Json<Vec<ProbeTaskRow>> {
    Json(state.probe_tasks().as_ref().clone())
}

#[derive(Deserialize)]
struct CreateReq {
    name: String,
    kind: ProbeKind,
    target: String,
    interval_secs: Option<i64>,
    samples: Option<i64>,
    timeout_ms: Option<i64>,
    all_nodes: Option<bool>,
    node_ids: Option<Vec<i64>>,
    show_on_card: Option<bool>,
}

async fn create(
    _: AdminOnly,
    State(state): State<AppState>,
    AppJson(req): AppJson<CreateReq>,
) -> Result<(StatusCode, Json<ProbeTaskRow>), ApiError> {
    let target = req.target.trim().to_string();
    validate_target(req.kind, &target)?;
    let all_nodes = req.all_nodes.unwrap_or(true);
    let node_ids = if all_nodes {
        Vec::new()
    } else {
        validate_nodes(&state, &req.node_ids.unwrap_or_default())?
    };
    let default_samples = if req.kind == ProbeKind::Icmp { 5 } else { 1 };
    let new = NewProbeTask {
        name: validate_name(&req.name)?,
        kind: req.kind,
        target,
        interval_secs: req.interval_secs.map_or(Ok(60), validate_interval)?,
        samples: req.samples.map_or(Ok(default_samples), validate_samples)?,
        timeout_ms: req.timeout_ms.map_or(Ok(5000), validate_timeout)?,
        all_nodes,
        node_ids,
        show_on_card: req.show_on_card.unwrap_or(true),
        sort: None,
    };
    let db = state.db.clone();
    let row = blocking(move || db.create_probe_task(&new)).await?;
    refresh(&state).await?;
    Ok((StatusCode::CREATED, Json(row)))
}

#[derive(Deserialize)]
struct PatchReq {
    name: Option<String>,
    kind: Option<ProbeKind>,
    target: Option<String>,
    interval_secs: Option<i64>,
    samples: Option<i64>,
    timeout_ms: Option<i64>,
    all_nodes: Option<bool>,
    node_ids: Option<Vec<i64>>,
    show_on_card: Option<bool>,
    sort: Option<i64>,
}

async fn update(
    _: AdminOnly,
    State(state): State<AppState>,
    TaskId(id): TaskId,
    AppJson(req): AppJson<PatchReq>,
) -> Result<Json<ProbeTaskRow>, ApiError> {
    let tasks = state.probe_tasks();
    let cur = tasks
        .iter()
        .find(|t| t.id == id)
        .ok_or_else(|| ApiError::not_found(MSG_NO_TASK))?;
    // 按合并后的有效值整体校验（改 kind 不改 target 时，旧 target 也要符合新 kind）。
    let kind = req.kind.unwrap_or(cur.kind);
    let target = req.target.as_deref().map(str::trim);
    validate_target(kind, target.unwrap_or(&cur.target))?;
    let all_nodes = req.all_nodes.unwrap_or(cur.all_nodes);
    let links = if all_nodes {
        // 改为（或保持）全部节点：关联清空。
        (req.all_nodes.is_some() || req.node_ids.is_some()).then(Vec::new)
    } else {
        let ids = req.node_ids.as_deref().unwrap_or(&cur.node_ids);
        let ids = validate_nodes(&state, ids)?;
        req.node_ids.is_some().then_some(ids)
    };
    let patch = ProbeTaskPatch {
        name: req.name.as_deref().map(validate_name).transpose()?,
        kind: req.kind,
        target: target.map(str::to_string),
        interval_secs: req.interval_secs.map(validate_interval).transpose()?,
        samples: req.samples.map(validate_samples).transpose()?,
        timeout_ms: req.timeout_ms.map(validate_timeout).transpose()?,
        all_nodes: req.all_nodes,
        node_ids: links,
        show_on_card: req.show_on_card,
        sort: req.sort,
    };
    let db = state.db.clone();
    if !blocking(move || db.update_probe_task(id, &patch)).await? {
        return Err(ApiError::not_found(MSG_NO_TASK));
    }
    refresh(&state).await?;
    state
        .probe_tasks()
        .iter()
        .find(|t| t.id == id)
        .cloned()
        .map(Json)
        .ok_or_else(|| ApiError::not_found(MSG_NO_TASK))
}

async fn remove(
    _: AdminOnly,
    State(state): State<AppState>,
    TaskId(id): TaskId,
) -> Result<StatusCode, ApiError> {
    let db = state.db.clone();
    if !blocking(move || db.delete_probe_task(id)).await? {
        return Err(ApiError::not_found(MSG_NO_TASK));
    }
    refresh(&state).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct IspReq {
    telecom: Option<String>,
    unicom: Option<String>,
    mobile: Option<String>,
}

async fn isp_template(
    _: AdminOnly,
    State(state): State<AppState>,
    AppJson(req): AppJson<IspReq>,
) -> Result<(StatusCode, Json<Vec<ProbeTaskRow>>), ApiError> {
    let mut new = Vec::new();
    for (name, target) in [
        ("电信", req.telecom),
        ("联通", req.unicom),
        ("移动", req.mobile),
    ] {
        let target = target.unwrap_or_default().trim().to_string();
        if target.is_empty() {
            continue;
        }
        validate_target(ProbeKind::Icmp, &target)?;
        new.push(NewProbeTask {
            name: name.to_string(),
            kind: ProbeKind::Icmp,
            target,
            interval_secs: 60,
            samples: 5,
            timeout_ms: 5000,
            all_nodes: true,
            node_ids: Vec::new(),
            show_on_card: true,
            sort: None,
        });
    }
    if new.is_empty() {
        return Err(ApiError::bad_request("请至少填写一家运营商的目标地址"));
    }
    let db = state.db.clone();
    let rows = blocking(move || db.create_probe_tasks(&new)).await?;
    refresh(&state).await?;
    Ok((StatusCode::CREATED, Json(rows)))
}
