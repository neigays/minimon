//! 节点视图：按身份输出 `PublicNode` / `AdminNode`。
//!
//! 隐私：视图逐字段构造，绝不直接序列化 `Hello` 或 `Node`。
//! `PublicNode` 里不得出现 IP、主机名、内核、备注、agent 版本、hidden、created_ms。

use crate::state::AppState;
use minimon_common::protocol::{Hello, Report};
use serde::Serialize;
use std::collections::HashSet;

#[derive(Debug, Clone, Serialize)]
pub struct PublicNode {
    pub id: i64,
    pub name: String,
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
}

/// 管理员视图：公开字段加管理字段。
#[derive(Debug, Clone, Serialize)]
pub struct AdminNode {
    #[serde(flatten)]
    pub base: PublicNode,
    pub hidden: bool,
    pub hostname: Option<String>,
    pub kernel: Option<String>,
    pub agent_version: Option<String>,
    pub ip: Option<String>,
    pub note: String,
    pub created_ms: i64,
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

fn base(state: &AppState, n: &crate::db::Node, snap: &Snapshot) -> (PublicNode, Option<Hello>) {
    // 在线时优先用本次连接的最新 Hello，否则用库里保存的。
    let info = state.registry().info(n.id).or_else(|| n.info.clone());
    let latest = snap.last.get(&n.id).cloned();
    let last_seen_ms = match (n.last_seen_ms, latest.as_ref().map(|r| r.ts_ms)) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (a, b) => a.or(b),
    };
    let p = PublicNode {
        id: n.id,
        name: n.name.clone(),
        online: snap.online.contains(&n.id),
        sort: n.sort,
        group: n.group.clone(),
        region: n.region.clone(),
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
        .map(|n| {
            let (base, info) = base(state, n, &snap);
            AdminNode {
                base,
                hidden: n.hidden,
                hostname: info.as_ref().map(|h| h.hostname.clone()),
                kernel: info.as_ref().map(|h| h.kernel.clone()),
                agent_version: info.as_ref().map(|h| h.agent_version.clone()),
                ip: n.last_ip.clone(),
                note: n.note.clone(),
                created_ms: n.created_ms,
            }
        })
        .collect()
}
