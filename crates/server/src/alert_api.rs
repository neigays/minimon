//! `/api/admin/alert-*`、`/api/admin/alerts/active`：告警规则、设置、测试发送与事件（全部 `AdminOnly`）。
//!
//! SendKey 只进不出：响应里只有 `sendkey_set` 与末 4 位，错误文本也不含它。

use crate::admin::{blocking, tri_state};
use crate::alert::metric::format_value;
use crate::alert::model::{
    AlertEvent, AlertRule, AlertRulePatch, AlertSettings, EventKind, Metric, NewAlertRule,
    NewEvent, SendStatus, StateKind,
};
use crate::auth::{AdminOnly, AppJson};
use crate::db::now_ms;
use crate::error::ApiError;
use crate::notify::{
    NotifyError, normalize_push_tags, normalize_sendkey, send_test, serverchan_factory,
};
use crate::state::AppState;
use axum::extract::{FromRequestParts, Path, Query, State};
use axum::http::StatusCode;
use axum::http::request::Parts;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

const MSG_NO_RULE: &str = "告警规则不存在";
const DEFAULT_DURATION: i64 = 300;
const DEFAULT_COOLDOWN: i64 = 1800;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/admin/alert-rules", get(list_rules).post(create_rule))
        .route(
            "/api/admin/alert-rules/{id}",
            axum::routing::patch(update_rule).delete(remove_rule),
        )
        .route(
            "/api/admin/alert-settings",
            get(get_settings).put(put_settings),
        )
        .route("/api/admin/alert-settings/test", post(test_send))
        .route("/api/admin/alert-events", get(events))
        .route("/api/admin/alerts/active", get(active))
}

struct RuleId(i64);

impl FromRequestParts<AppState> for RuleId {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        Path::<i64>::from_request_parts(parts, state)
            .await
            .map(|Path(id)| RuleId(id))
            .map_err(|_| ApiError::not_found(MSG_NO_RULE))
    }
}

// ---------------------------------------------------------------- 规则校验

fn is_percent(m: Metric) -> bool {
    matches!(
        m,
        Metric::Cpu | Metric::Mem | Metric::Swap | Metric::Disk | Metric::ProbeLoss
    )
}

/// 校验完整规则；返回规范化后的名称、节点列表。
/// `check_task` / `check_nodes` 为 false 时跳过对应引用的存在性与非空检查
/// （PATCH 未触及的陈旧引用不应阻止其他字段的修改）。
fn validate_rule(
    state: &AppState,
    r: &NewAlertRule,
    check_task: bool,
    check_nodes: bool,
) -> Result<(String, Vec<i64>), ApiError> {
    let name = r.name.trim().to_string();
    if !(1..=64).contains(&name.chars().count()) {
        return Err(ApiError::bad_request("名称需为 1 到 64 个字符"));
    }
    if !(10..=86400).contains(&r.duration_secs) {
        return Err(ApiError::bad_request("持续时间需为 10 到 86400 秒"));
    }
    if !(0..=86400).contains(&r.cooldown_secs) {
        return Err(ApiError::bad_request("冷却时间需为 0 到 86400 秒"));
    }
    if !r.threshold.is_finite() || r.threshold < 0.0 {
        return Err(ApiError::bad_request("阈值需为非负有限数"));
    }
    if is_percent(r.metric) && r.threshold > 100.0 {
        return Err(ApiError::bad_request("百分比阈值需为 0 到 100"));
    }
    if let Some(tid) = r.probe_task_id {
        if !r.metric.is_probe() {
            return Err(ApiError::bad_request("只有探测类指标可以绑定任务"));
        }
        if check_task && !state.probe_tasks().iter().any(|t| t.id == tid) {
            return Err(ApiError::bad_request("所选探测任务不存在"));
        }
    }
    let node_ids = if r.all_nodes {
        Vec::new()
    } else {
        let mut ids = r.node_ids.clone();
        ids.sort_unstable();
        ids.dedup();
        if check_nodes && ids.is_empty() {
            return Err(ApiError::bad_request("请选择至少一个节点"));
        }
        let nodes = state.nodes();
        if check_nodes && ids.iter().any(|id| !nodes.iter().any(|n| n.id == *id)) {
            return Err(ApiError::bad_request("所选节点不存在"));
        }
        ids
    };
    Ok((name, node_ids))
}

async fn reload_rules(state: &AppState) -> Result<(), ApiError> {
    state.alert().reload_rules(&state.db).await.map_err(|e| {
        tracing::error!("重载告警规则失败: {e:?}");
        ApiError::internal()
    })
}

// ---------------------------------------------------------------- 规则接口

async fn list_rules(
    _: AdminOnly,
    State(state): State<AppState>,
) -> Result<Json<Vec<AlertRule>>, ApiError> {
    let db = state.db.clone();
    Ok(Json(blocking(move || db.list_alert_rules()).await?))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateRuleReq {
    name: String,
    enabled: bool,
    metric: Metric,
    op: crate::alert::model::Op,
    threshold: f64,
    duration_secs: Option<i64>,
    cooldown_secs: Option<i64>,
    all_nodes: bool,
    #[serde(default)]
    node_ids: Vec<i64>,
    probe_task_id: Option<i64>,
}

async fn create_rule(
    _: AdminOnly,
    State(state): State<AppState>,
    AppJson(req): AppJson<CreateRuleReq>,
) -> Result<(StatusCode, Json<AlertRule>), ApiError> {
    let mut new = NewAlertRule {
        name: req.name,
        enabled: req.enabled,
        metric: req.metric,
        op: req.op,
        threshold: req.threshold,
        duration_secs: req.duration_secs.unwrap_or(DEFAULT_DURATION),
        cooldown_secs: req.cooldown_secs.unwrap_or(DEFAULT_COOLDOWN),
        all_nodes: req.all_nodes,
        node_ids: req.node_ids,
        probe_task_id: req.probe_task_id,
    };
    (new.name, new.node_ids) = validate_rule(&state, &new, true, true)?;
    let db = state.db.clone();
    let rule = blocking(move || db.create_alert_rule(&new)).await?;
    reload_rules(&state).await?;
    Ok((StatusCode::CREATED, Json(rule)))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PatchRuleReq {
    name: Option<String>,
    enabled: Option<bool>,
    metric: Option<Metric>,
    op: Option<crate::alert::model::Op>,
    threshold: Option<f64>,
    duration_secs: Option<i64>,
    cooldown_secs: Option<i64>,
    all_nodes: Option<bool>,
    node_ids: Option<Vec<i64>>,
    #[serde(default, deserialize_with = "tri_state")]
    probe_task_id: Option<Option<i64>>,
}

async fn update_rule(
    _: AdminOnly,
    State(state): State<AppState>,
    RuleId(id): RuleId,
    AppJson(req): AppJson<PatchRuleReq>,
) -> Result<Json<AlertRule>, ApiError> {
    let db = state.db.clone();
    let cur = blocking(move || db.list_alert_rules())
        .await?
        .into_iter()
        .find(|r| r.id == id)
        .ok_or_else(|| ApiError::not_found(MSG_NO_RULE))?;
    // 改成非探测指标而没提 probe_task_id 时，顺带解绑，避免遗留无效组合。
    let probe_task_id = match req.probe_task_id {
        Some(v) => v,
        None if req.metric.is_some_and(|m| !m.is_probe()) => None,
        None => cur.probe_task_id,
    };
    let merged = NewAlertRule {
        name: req.name.clone().unwrap_or(cur.name),
        enabled: req.enabled.unwrap_or(cur.enabled),
        metric: req.metric.unwrap_or(cur.metric),
        op: req.op.unwrap_or(cur.op),
        threshold: req.threshold.unwrap_or(cur.threshold),
        duration_secs: req.duration_secs.unwrap_or(cur.duration_secs),
        cooldown_secs: req.cooldown_secs.unwrap_or(cur.cooldown_secs),
        all_nodes: req.all_nodes.unwrap_or(cur.all_nodes),
        node_ids: req.node_ids.clone().unwrap_or(cur.node_ids),
        probe_task_id,
    };
    let (name, node_ids) = validate_rule(
        &state,
        &merged,
        matches!(req.probe_task_id, Some(Some(_))),
        req.all_nodes.is_some() || req.node_ids.is_some(),
    )?;
    let patch = AlertRulePatch {
        name: req.name.map(|_| name),
        enabled: req.enabled,
        metric: req.metric,
        op: req.op,
        threshold: req.threshold,
        duration_secs: req.duration_secs,
        cooldown_secs: req.cooldown_secs,
        all_nodes: req.all_nodes,
        // 切到「全部节点」时清空关联；否则仅在显式给出时替换。
        node_ids: if merged.all_nodes && req.all_nodes == Some(true) {
            Some(Vec::new())
        } else {
            req.node_ids.map(|_| node_ids)
        },
        probe_task_id: (req.probe_task_id.is_some() || req.metric.is_some())
            .then_some(merged.probe_task_id),
    };
    let db = state.db.clone();
    let rule = blocking(move || db.update_alert_rule(id, &patch))
        .await?
        .ok_or_else(|| ApiError::not_found(MSG_NO_RULE))?;
    reload_rules(&state).await?;
    Ok(Json(rule))
}

async fn remove_rule(
    _: AdminOnly,
    State(state): State<AppState>,
    RuleId(id): RuleId,
) -> Result<StatusCode, ApiError> {
    let db = state.db.clone();
    if !blocking(move || db.delete_alert_rule(id)).await? {
        return Err(ApiError::not_found(MSG_NO_RULE));
    }
    reload_rules(&state).await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------- 设置

#[derive(Serialize)]
struct SettingsView {
    offline_enabled: bool,
    offline_grace_secs: i64,
    traffic_enabled: bool,
    expire_enabled: bool,
    sendkey_set: bool,
    sendkey_tail: Option<String>,
    push_tags: String,
}

async fn settings_view(state: &AppState) -> Result<SettingsView, ApiError> {
    let db = state.db.clone();
    let (s, key, push_tags) =
        blocking(move || Ok((db.alert_settings()?, db.sendkey()?, db.push_tags()?))).await?;
    Ok(SettingsView {
        offline_enabled: s.offline_enabled,
        offline_grace_secs: s.offline_grace_secs,
        traffic_enabled: s.traffic_enabled,
        expire_enabled: s.expire_enabled,
        push_tags,
        sendkey_set: key.is_some(),
        sendkey_tail: key.map(|k| {
            let n = k.chars().count().saturating_sub(4);
            k.chars().skip(n).collect()
        }),
    })
}

async fn get_settings(
    _: AdminOnly,
    State(state): State<AppState>,
) -> Result<Json<SettingsView>, ApiError> {
    Ok(Json(settings_view(&state).await?))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PutSettingsReq {
    offline_enabled: Option<bool>,
    offline_grace_secs: Option<i64>,
    traffic_enabled: Option<bool>,
    expire_enabled: Option<bool>,
    sendkey: Option<String>,
    push_tags: Option<String>,
}

async fn put_settings(
    _: AdminOnly,
    State(state): State<AppState>,
    AppJson(req): AppJson<PutSettingsReq>,
) -> Result<Json<SettingsView>, ApiError> {
    if req.offline_enabled.is_none()
        && req.offline_grace_secs.is_none()
        && req.traffic_enabled.is_none()
        && req.expire_enabled.is_none()
        && req.sendkey.is_none()
        && req.push_tags.is_none()
    {
        return Err(ApiError::bad_request("没有要修改的设置"));
    }
    if let Some(g) = req.offline_grace_secs
        && !(30..=3600).contains(&g)
    {
        return Err(ApiError::bad_request("离线宽限期需为 30 到 3600 秒"));
    }
    // 空串 = 清除；否则规范化（去 `.send`、从整段 API 地址提取）并校验。
    let key = match req.sendkey.as_deref().map(str::trim) {
        None => None,
        Some("") => Some(String::new()),
        Some(k) => {
            Some(normalize_sendkey(k).ok_or_else(|| ApiError::bad_request("SendKey 格式不正确"))?)
        }
    };
    let push_tags = req
        .push_tags
        .as_deref()
        .map(normalize_push_tags)
        .transpose()
        .map_err(ApiError::bad_request)?;
    let db = state.db.clone();
    blocking(move || {
        let cur = db.alert_settings()?;
        let settings = AlertSettings {
            offline_enabled: req.offline_enabled.unwrap_or(cur.offline_enabled),
            offline_grace_secs: req.offline_grace_secs.unwrap_or(cur.offline_grace_secs),
            traffic_enabled: req.traffic_enabled.unwrap_or(cur.traffic_enabled),
            expire_enabled: req.expire_enabled.unwrap_or(cur.expire_enabled),
        };
        let sendkey = key.as_deref().map(|k| Some(k).filter(|k| !k.is_empty()));
        db.save_alert_settings(&settings, sendkey, push_tags.as_deref())
    })
    .await?;
    state
        .alert()
        .reload_settings(&state.db)
        .await
        .map_err(|e| {
            tracing::error!("重载告警设置失败: {e:?}");
            ApiError::internal()
        })?;
    Ok(Json(settings_view(&state).await?))
}

/// 发送一条测试消息并记一条 `test` 事件（成功 / 失败）。
async fn test_send(_: AdminOnly, State(state): State<AppState>) -> Result<StatusCode, ApiError> {
    let res = send_test(&state.db, &serverchan_factory()).await;
    let db = state.db.clone();
    let key = blocking(move || db.sendkey()).await?;
    let (sent, err) = match res {
        Ok(()) => (SendStatus::Ok, None),
        Err(e) => {
            let mut msg = match e {
                NotifyError::Config(m) => m.to_string(),
                other => other.to_string(),
            };
            if let Some(k) = key.as_deref().filter(|k| !k.is_empty()) {
                msg = msg.replace(k, "***");
                if let Some(n) = normalize_sendkey(k) {
                    msg = msg.replace(&n, "***");
                }
            }
            (SendStatus::Failed, Some(msg))
        }
    };
    let ev = NewEvent {
        ts_ms: now_ms(),
        node_id: None,
        node_name: None,
        kind: EventKind::Test,
        title: "minimon 测试消息".into(),
        body: match &err {
            None => "测试消息已发送".into(),
            Some(m) => format!("发送失败：{m}"),
        },
        sent,
    };
    let db = state.db.clone();
    blocking(move || db.insert_alert_events(&[ev]).map(|_| ())).await?;
    match err {
        None => Ok(StatusCode::NO_CONTENT),
        Some(m) => Err(ApiError::bad_request(format!("发送失败：{m}"))),
    }
}

// ---------------------------------------------------------------- 事件与活动告警

#[derive(Deserialize)]
struct EventsQuery {
    before: Option<i64>,
    limit: Option<i64>,
}

async fn events(
    _: AdminOnly,
    State(state): State<AppState>,
    Query(q): Query<EventsQuery>,
) -> Result<Json<Vec<AlertEvent>>, ApiError> {
    let limit = q.limit.unwrap_or(50);
    if !(1..=100).contains(&limit) {
        return Err(ApiError::bad_request("limit 需为 1 到 100"));
    }
    let db = state.db.clone();
    Ok(Json(
        blocking(move || db.list_alert_events(q.before, limit)).await?,
    ))
}

#[derive(Serialize)]
struct ActiveAlert {
    kind: &'static str,
    rule_id: i64,
    rule_name: Option<String>,
    node_id: i64,
    node_name: Option<String>,
    task_id: Option<i64>,
    task_name: Option<String>,
    since_ms: i64,
    fired_ms: Option<i64>,
    value: Option<f64>,
    value_text: Option<String>,
}

async fn active(
    _: AdminOnly,
    State(state): State<AppState>,
) -> Result<Json<Vec<ActiveAlert>>, ApiError> {
    let db = state.db.clone();
    let rules = blocking(move || db.list_alert_rules()).await?;
    let nodes = state.nodes();
    let tasks = state.probe_tasks();
    let mut out: Vec<ActiveAlert> = state
        .alert()
        .active()
        .into_iter()
        .map(|s| {
            let k = s.key;
            let rule = (k.kind == StateKind::Rule)
                .then(|| rules.iter().find(|r| r.id == k.rule_id))
                .flatten();
            let task_name = (k.task_id > 0)
                .then(|| tasks.iter().find(|t| t.id == k.task_id))
                .flatten()
                .map(|t| t.name.clone());
            ActiveAlert {
                kind: k.kind.as_str(),
                rule_id: k.rule_id,
                rule_name: rule.map(|r| r.name.clone()),
                node_id: k.node_id,
                node_name: nodes
                    .iter()
                    .find(|n| n.id == k.node_id)
                    .map(|n| n.name.clone()),
                task_id: (k.task_id > 0).then_some(k.task_id),
                task_name,
                since_ms: s.since_ms,
                fired_ms: s.fired_ms,
                value: s.value,
                value_text: rule.zip(s.value).map(|(r, v)| format_value(r.metric, v)),
            }
        })
        .collect();
    out.sort_by_key(|a| (a.since_ms, a.node_id, a.rule_id));
    Ok(Json(out))
}
