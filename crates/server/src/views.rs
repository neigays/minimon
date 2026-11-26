//! 节点视图：按身份输出 `PublicNode` / `AdminNode`。
//!
//! 隐私：视图逐字段构造，绝不直接序列化 `Hello` 或 `Node`。
//! `PublicNode` 里不得出现 IP、主机名、内核、备注、agent 版本、hidden、created_ms。

use crate::probe::CardProbe;
use crate::state::AppState;
use crate::traffic::TrafficUsage;
use minimon_common::protocol::{Hello, Report};
use serde::Serialize;
use std::collections::HashSet;

#[derive(Debug, Clone, Serialize)]
pub struct PublicNode {
    pub id: i64,
    pub name: String,
    pub tags: Vec<crate::tags::Tag>,
    pub online: bool,
    pub sort: i64,
    pub group: String,
    pub region: String,
    pub os: Option<String>,
    pub arch: Option<String>,
    pub cpu_model: Option<String>,
    pub cpu_cores: Option<u32>,
    pub virtualization: Option<String>,
    pub mem_total: Option<u64>,
    pub swap_total: Option<u64>,
    pub disk_total: Option<u64>,
    pub last_seen_ms: Option<i64>,
    pub latest: Option<Report>,
    pub probes: Vec<CardProbe>,
}

/// 管理员视图：公开字段加管理字段。
#[derive(Debug, Clone, Serialize)]
pub struct AdminNode {
    #[serde(flatten)]
    pub base: PublicNode,
    pub hidden: bool,
    /// 原始标签串，供编辑。
    pub tags_raw: String,
    pub hostname: Option<String>,
    pub kernel: Option<String>,
    pub agent_version: Option<String>,
    /// 连接来源地址（反代 / Docker 后可能是内网地址）。
    pub ip: Option<String>,
    /// agent 自查的公网地址。
    pub ipv4: Option<String>,
    pub ipv6: Option<String>,
    pub note: String,
    pub created_ms: i64,
    /// 已存的计费配置（与用量缓存无关，编辑表单以此为准）。
    pub traffic_quota: Option<i64>,
    pub traffic_reset_day: u8,
    pub traffic_mode: crate::db::TrafficMode,
    pub traffic: Option<TrafficUsage>,
    pub expire_date: Option<String>,
    pub expire_days: Option<i64>,
    pub alert_offline: bool,
}

/// 快照里的节点列表，序列化为同一个 JSON 数组。
#[derive(Serialize)]
#[serde(untagged)]
pub enum NodeList {
    Public(Vec<PublicNode>),
    Admin(Vec<AdminNode>),
}

pub fn nodes_for(state: &AppState, admin: bool) -> NodeList {
    if admin {
        NodeList::Admin(admin_nodes(state))
    } else {
        NodeList::Public(public_nodes(state))
    }
}

struct Snapshot {
    online: HashSet<i64>,
    last: std::collections::HashMap<i64, Report>,
}

impl Snapshot {
    fn take(state: &AppState) -> Self {
        Snapshot {
            online: state
                .registry()
                .snapshot()
                .into_iter()
                .map(|(id, _)| id)
                .collect(),
            last: state.last_known(),
        }
    }
}

/// 合并库里与最近上报的时间；不得晚于服务器当前时间（agent 时钟超前时不外泄未来时间）。
pub(crate) fn merge_last_seen(db: Option<i64>, report: Option<i64>, now: i64) -> Option<i64> {
    let merged = match (db, report) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (a, b) => a.or(b),
    };
    merged.map(|t| t.min(now))
}

fn base(state: &AppState, n: &crate::db::Node, snap: &Snapshot) -> (PublicNode, Option<Hello>) {
    // 在线时优先用本次连接的最新 Hello，否则用库里保存的。
    let info = state.registry().info(n.id).or_else(|| n.info.clone());
    let latest = snap.last.get(&n.id).cloned();
    let last_seen_ms = merge_last_seen(
        n.last_seen_ms,
        latest.as_ref().map(|r| r.ts_ms),
        crate::db::now_ms(),
    );
    let p = PublicNode {
        id: n.id,
        name: n.name.clone(),
        tags: crate::tags::tags_for_view(&n.tags),
        online: snap.online.contains(&n.id),
        sort: n.sort,
        group: n.group.clone(),
        // 显示用地区：agent 上报的国家优先，旧的手填地区作过渡
        region: n.country.clone().unwrap_or_else(|| n.region.clone()),
        os: info.as_ref().map(|h| h.os.clone()),
        arch: info.as_ref().map(|h| h.arch.clone()),
        cpu_model: info.as_ref().map(|h| h.cpu_model.clone()),
        cpu_cores: info.as_ref().map(|h| h.cpu_cores),
        virtualization: info.as_ref().and_then(|h| h.virtualization.clone()),
        mem_total: info.as_ref().map(|h| h.mem_total),
        swap_total: info.as_ref().map(|h| h.swap_total),
        disk_total: info.as_ref().map(|h| h.disk_total),
        last_seen_ms,
        latest,
        probes: state.latest_probe(n.id),
    };
    (p, info)
}

/// 非隐藏节点的公开视图，按 (sort, id) 排序（缓存已按此排序）。
pub fn public_nodes(state: &AppState) -> Vec<PublicNode> {
    let snap = Snapshot::take(state);
    state
        .nodes()
        .iter()
        .filter(|n| !n.hidden)
        .map(|n| base(state, n, &snap).0)
        .collect()
}

/// 全部节点（含隐藏）的管理员视图。
pub fn admin_nodes(state: &AppState) -> Vec<AdminNode> {
    let snap = Snapshot::take(state);
    state
        .nodes()
        .iter()
        .map(|n| admin_view(state, n, &snap))
        .collect()
}

/// 单个节点的管理员视图。
pub fn admin_node(state: &AppState, id: i64) -> Option<AdminNode> {
    let snap = Snapshot::take(state);
    let nodes = state.nodes();
    let n = nodes.iter().find(|n| n.id == id)?;
    Some(admin_view(state, n, &snap))
}

/// 不依赖缓存的单节点管理员视图（缓存刷新失败时的兜底）。
pub fn admin_node_of(state: &AppState, n: &crate::db::Node) -> AdminNode {
    admin_view(state, n, &Snapshot::take(state))
}

fn admin_view(state: &AppState, n: &crate::db::Node, snap: &Snapshot) -> AdminNode {
    let (base, info) = base(state, n, snap);
    AdminNode {
        base,
        hidden: n.hidden,
        tags_raw: n.tags.clone(),
        hostname: info.as_ref().map(|h| h.hostname.clone()),
        kernel: info.as_ref().map(|h| h.kernel.clone()),
        agent_version: info.as_ref().map(|h| h.agent_version.clone()),
        ip: n.last_ip.clone(),
        ipv4: n.ipv4.clone(),
        ipv6: n.ipv6.clone(),
        note: n.note.clone(),
        created_ms: n.created_ms,
        traffic_quota: n.traffic_quota,
        traffic_reset_day: n.traffic_reset_day,
        traffic_mode: n.traffic_mode,
        traffic: state.traffic(n.id),
        alert_offline: n.alert_offline,
        expire_date: n.expire_date.clone(),
        expire_days: n
            .expire_date
            .as_deref()
            .and_then(|d| crate::period::expire_days(d, &state.timezone(), crate::db::now_ms())),
    }
}

#[cfg(test)]
mod tests {
    use super::merge_last_seen;

    #[test]
    fn last_seen_is_clamped_to_server_time() {
        assert_eq!(merge_last_seen(Some(100), Some(500), 1000), Some(500));
        assert_eq!(merge_last_seen(Some(100), Some(9_000), 1000), Some(1000));
        assert_eq!(merge_last_seen(Some(9_000), None, 1000), Some(1000));
        assert_eq!(merge_last_seen(None, None, 1000), None);
    }
}
