//! agent 与 server 之间的协议消息（JSON 文本帧）。

use serde::{Deserialize, Serialize};

/// 协议大版本。不兼容改动必须递增。
pub const PROTOCOL_VERSION: u32 = 1;

/// agent 发往 server 的消息。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentMsg {
    Hello(Hello),
    Report(Report),
    ProbeResult(ProbeResult),
}

/// server 发往 agent 的消息。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMsg {
    Welcome { interval_secs: u32 },
    ProbeTasks { tasks: Vec<ProbeTask> },
    Error { code: ErrorCode, message: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    Unauthorized,
    IncompatibleVersion,
    Replaced,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Hello {
    pub v: u32,
    pub agent_version: String,
    pub hostname: String,
    pub os: String,
    pub kernel: String,
    pub arch: String,
    pub cpu_model: String,
    pub cpu_cores: u32,
    pub mem_total: u64,
    pub swap_total: u64,
    pub disk_total: u64,
    pub virtualization: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Report {
    pub ts_ms: i64,
    pub cpu: f32,
    pub mem_used: u64,
    pub swap_used: u64,
    pub disk_used: u64,
    pub disk_total: u64,
    pub net_rx_bytes: u64,
    pub net_tx_bytes: u64,
    pub net_rx_rate: u64,
    pub net_tx_rate: u64,
    pub load1: f64,
    pub load5: f64,
    pub load15: f64,
    pub uptime_secs: u64,
    pub process_count: u32,
    pub tcp_count: u32,
    pub udp_count: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProbeKind {
    Icmp,
    Tcp,
    Http,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProbeTask {
    pub id: i64,
    pub kind: ProbeKind,
    pub target: String,
    pub interval_secs: u32,
    pub samples: u32,
    pub timeout_ms: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProbeResult {
    pub task_id: i64,
    pub ts_ms: i64,
    pub sent: u32,
    pub lost: u32,
    pub avg_ms: Option<f64>,
    pub min_ms: Option<f64>,
    pub max_ms: Option<f64>,
    pub error: Option<String>,
}

/// 校验对端协议版本。
pub fn check_version(v: u32) -> Result<(), ErrorCode> {
    if v == PROTOCOL_VERSION {
        Ok(())
    } else {
        Err(ErrorCode::IncompatibleVersion)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_report() -> Report {
        Report {
            ts_ms: 1_700_000_000_000,
            cpu: 12.5,
            mem_used: 1,
            swap_used: 2,
            disk_used: 3,
            disk_total: 4,
            net_rx_bytes: 5,
            net_tx_bytes: 6,
            net_rx_rate: 7,
            net_tx_rate: 8,
            load1: 0.1,
            load5: 0.2,
            load15: 0.3,
            uptime_secs: 99,
            process_count: 10,
            tcp_count: 11,
            udp_count: 12,
        }
    }

    #[test]
    fn report_roundtrip_has_type_tag() {
        let msg = AgentMsg::Report(sample_report());
        let s = serde_json::to_string(&msg).unwrap();
        assert!(s.starts_with(r#"{"type":"report""#));
        assert_eq!(serde_json::from_str::<AgentMsg>(&s).unwrap(), msg);
    }

    #[test]
    fn server_error_serializes_snake_case() {
        let s = serde_json::to_string(&ServerMsg::Error {
            code: ErrorCode::IncompatibleVersion,
            message: "x".into(),
        })
        .unwrap();
        assert_eq!(
            s,
            r#"{"type":"error","code":"incompatible_version","message":"x"}"#
        );
    }

    #[test]
    fn unknown_type_is_rejected() {
        assert!(serde_json::from_str::<AgentMsg>(r#"{"type":"exec","cmd":"rm"}"#).is_err());
    }

    #[test]
    fn version_check() {
        assert!(check_version(PROTOCOL_VERSION).is_ok());
        assert_eq!(
            check_version(PROTOCOL_VERSION + 1),
            Err(ErrorCode::IncompatibleVersion)
        );
    }
}
