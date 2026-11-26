//! 告警领域模型：规则、状态键、事件与设置。

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Metric {
    Cpu,
    Mem,
    Swap,
    Disk,
    Load1,
    NetRxRate,
    NetTxRate,
    ProbeLatency,
    ProbeLoss,
}

impl Metric {
    pub fn as_str(self) -> &'static str {
        match self {
            Metric::Cpu => "cpu",
            Metric::Mem => "mem",
            Metric::Swap => "swap",
            Metric::Disk => "disk",
            Metric::Load1 => "load1",
            Metric::NetRxRate => "net_rx_rate",
            Metric::NetTxRate => "net_tx_rate",
            Metric::ProbeLatency => "probe_latency",
            Metric::ProbeLoss => "probe_loss",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "cpu" => Metric::Cpu,
            "mem" => Metric::Mem,
            "swap" => Metric::Swap,
            "disk" => Metric::Disk,
            "load1" => Metric::Load1,
            "net_rx_rate" => Metric::NetRxRate,
            "net_tx_rate" => Metric::NetTxRate,
            "probe_latency" => Metric::ProbeLatency,
            "probe_loss" => Metric::ProbeLoss,
            _ => return None,
        })
    }

    pub fn is_probe(self) -> bool {
        matches!(self, Metric::ProbeLatency | Metric::ProbeLoss)
    }

    pub fn label(self) -> &'static str {
        match self {
            Metric::Cpu => "CPU",
            Metric::Mem => "内存",
            Metric::Swap => "Swap",
            Metric::Disk => "磁盘",
            Metric::Load1 => "负载",
            Metric::NetRxRate => "下行速率",
            Metric::NetTxRate => "上行速率",
            Metric::ProbeLatency => "探测延迟",
            Metric::ProbeLoss => "探测丢包",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Op {
    Gt,
    Lt,
}

impl Op {
    pub fn as_str(self) -> &'static str {
        match self {
            Op::Gt => "gt",
            Op::Lt => "lt",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "gt" => Some(Op::Gt),
            "lt" => Some(Op::Lt),
            _ => None,
        }
    }

    pub fn holds(self, value: f64, threshold: f64) -> bool {
        match self {
            Op::Gt => value > threshold,
            Op::Lt => value < threshold,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AlertRule {
    pub id: i64,
    pub name: String,
    pub enabled: bool,
    pub metric: Metric,
    pub op: Op,
    pub threshold: f64,
    pub duration_secs: i64,
    pub cooldown_secs: i64,
    pub all_nodes: bool,
    /// 仅 `all_nodes = false` 时有内容（升序、去重）。
    pub node_ids: Vec<i64>,
    pub probe_task_id: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct NewAlertRule {
    pub name: String,
    pub enabled: bool,
    pub metric: Metric,
    pub op: Op,
    pub threshold: f64,
    pub duration_secs: i64,
    pub cooldown_secs: i64,
    pub all_nodes: bool,
    pub node_ids: Vec<i64>,
    pub probe_task_id: Option<i64>,
}

/// 字段为 `None` 表示不改；`node_ids = Some(..)` 整体替换关联。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AlertRulePatch {
    pub name: Option<String>,
    pub enabled: Option<bool>,
    pub metric: Option<Metric>,
    pub op: Option<Op>,
    pub threshold: Option<f64>,
    pub duration_secs: Option<i64>,
    pub cooldown_secs: Option<i64>,
    pub all_nodes: Option<bool>,
    pub node_ids: Option<Vec<i64>>,
    pub probe_task_id: Option<Option<i64>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StateKind {
    Rule,
    Offline,
}

impl StateKind {
    pub fn as_str(self) -> &'static str {
        match self {
            StateKind::Rule => "rule",
            StateKind::Offline => "offline",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "rule" => Some(StateKind::Rule),
            "offline" => Some(StateKind::Offline),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AlertKey {
    pub kind: StateKind,
    pub rule_id: i64,
    pub node_id: i64,
    pub task_id: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StateRow {
    pub key: AlertKey,
    /// false = pending，true = firing。
    pub firing: bool,
    pub since_ms: i64,
    pub fired_ms: Option<i64>,
    pub notified: bool,
    pub value: Option<f64>,
    pub last_notified_ms: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkKind {
    Traffic,
    Expire,
}

impl MarkKind {
    pub fn as_str(self) -> &'static str {
        match self {
            MarkKind::Traffic => "traffic",
            MarkKind::Expire => "expire",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "traffic" => Some(MarkKind::Traffic),
            "expire" => Some(MarkKind::Expire),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mark {
    pub node_id: i64,
    pub kind: MarkKind,
    pub mark: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    Firing,
    Resolved,
    Traffic,
    Expire,
    Test,
}

impl EventKind {
    pub fn as_str(self) -> &'static str {
        match self {
            EventKind::Firing => "firing",
            EventKind::Resolved => "resolved",
            EventKind::Traffic => "traffic",
            EventKind::Expire => "expire",
            EventKind::Test => "test",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "firing" => EventKind::Firing,
            "resolved" => EventKind::Resolved,
            "traffic" => EventKind::Traffic,
            "expire" => EventKind::Expire,
            "test" => EventKind::Test,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SendStatus {
    Pending,
    Ok,
    Failed,
    Skipped,
}

impl SendStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            SendStatus::Pending => "pending",
            SendStatus::Ok => "ok",
            SendStatus::Failed => "failed",
            SendStatus::Skipped => "skipped",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "pending" => SendStatus::Pending,
            "ok" => SendStatus::Ok,
            "failed" => SendStatus::Failed,
            "skipped" => SendStatus::Skipped,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AlertEvent {
    pub id: i64,
    pub ts_ms: i64,
    pub node_id: Option<i64>,
    pub node_name: Option<String>,
    pub kind: EventKind,
    pub title: String,
    pub body: String,
    pub sent: SendStatus,
}

/// 尚未入库的事件（`AlertEvent` 去掉 `id`）。
#[derive(Debug, Clone, PartialEq)]
pub struct NewEvent {
    pub ts_ms: i64,
    pub node_id: Option<i64>,
    pub node_name: Option<String>,
    pub kind: EventKind,
    pub title: String,
    pub body: String,
    pub sent: SendStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlertSettings {
    pub offline_enabled: bool,
    pub offline_grace_secs: i64,
    pub traffic_enabled: bool,
    pub expire_enabled: bool,
}

impl Default for AlertSettings {
    fn default() -> Self {
        AlertSettings {
            offline_enabled: true,
            offline_grace_secs: 180,
            traffic_enabled: true,
            expire_enabled: true,
        }
    }
}
