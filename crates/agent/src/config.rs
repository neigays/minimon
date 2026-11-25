//! agent 配置。优先级：命令行参数 > 环境变量（MINIMON_SERVER / MINIMON_TOKEN）> `--config` TOML 文件。

use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, bail};
use clap::Parser;
use serde::Deserialize;

#[derive(Clone)]
pub struct AgentConfig {
    /// server 基址（`ws://` 或 `wss://`），客户端自动拼接 `/api/agent/ws`。
    pub server: String,
    pub token: String,
    /// 若设置则覆盖 server 下发的上报间隔（秒）。
    pub interval_override: Option<u32>,
    /// 非空时只统计这些网卡，覆盖默认排除规则。
    pub iface_include: Vec<String>,
    /// 重连退避起点与上限（测试可调小，不暴露为命令行参数）。
    pub backoff_base: Duration,
    pub backoff_max: Duration,
}

impl AgentConfig {
    pub fn new(server: impl Into<String>, token: impl Into<String>) -> Self {
        Self {
            server: server.into(),
            token: token.into(),
            interval_override: None,
            iface_include: Vec::new(),
            backoff_base: Duration::from_secs(1),
            backoff_max: Duration::from_secs(60),
        }
    }
}

// 手写 Debug，避免 token 进入日志。
impl fmt::Debug for AgentConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AgentConfig")
            .field("server", &self.server)
            .field("token", &"<redacted>")
            .field("interval_override", &self.interval_override)
            .field("iface_include", &self.iface_include)
            .field("backoff_base", &self.backoff_base)
            .field("backoff_max", &self.backoff_max)
            .finish()
    }
}

#[derive(Parser, Debug)]
#[command(name = "minimon-agent", version, about = "minimon 探针")]
pub struct Cli {
    /// server 基址，如 ws://host:25780 或 wss://host
    #[arg(long, env = "MINIMON_SERVER")]
    pub server: Option<String>,
    /// 节点 token
    #[arg(long, env = "MINIMON_TOKEN", hide_env_values = true)]
    pub token: Option<String>,
    /// TOML 配置文件（字段：server、token、interval、iface_include）
    #[arg(long)]
    pub config: Option<PathBuf>,
    /// 覆盖 server 下发的上报间隔（秒）
    #[arg(long)]
    pub interval: Option<u32>,
    /// 只统计指定网卡（可重复、可逗号分隔）
    #[arg(long = "iface", value_delimiter = ',')]
    pub iface_include: Vec<String>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct FileConfig {
    server: Option<String>,
    token: Option<String>,
    interval: Option<u32>,
    #[serde(default)]
    iface_include: Vec<String>,
}

impl Cli {
    /// 合并命令行/环境变量与配置文件。缺少 server 或 token 时报错（启动阶段校验）。
    pub fn resolve(self) -> anyhow::Result<AgentConfig> {
        let file = match &self.config {
            Some(p) => {
                let text = std::fs::read_to_string(p)
                    .with_context(|| format!("读取配置文件失败: {}", p.display()))?;
                toml::from_str::<FileConfig>(&text)
                    .with_context(|| format!("解析配置文件失败: {}", p.display()))?
            }
            None => FileConfig::default(),
        };
        let Some(server) = self.server.or(file.server).filter(|s| !s.is_empty()) else {
            bail!("缺少 server：请用 --server、MINIMON_SERVER 或配置文件指定");
        };
        let Some(token) = self.token.or(file.token).filter(|s| !s.is_empty()) else {
            bail!("缺少 token：请用 --token、MINIMON_TOKEN 或配置文件指定");
        };
        let mut cfg = AgentConfig::new(server, token);
        cfg.interval_override = self.interval.or(file.interval);
        cfg.iface_include = if self.iface_include.is_empty() {
            file.iface_include
        } else {
            self.iface_include
        };
        Ok(cfg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cli(server: Option<&str>, token: Option<&str>, config: Option<PathBuf>) -> Cli {
        Cli {
            server: server.map(Into::into),
            token: token.map(Into::into),
            config,
            interval: None,
            iface_include: vec![],
        }
    }

    #[test]
    fn debug_redacts_token() {
        let c = AgentConfig::new("ws://x", "supersecret");
        assert!(!format!("{c:?}").contains("supersecret"));
    }

    #[test]
    fn missing_server_or_token_errors() {
        assert!(cli(None, Some("t"), None).resolve().is_err());
        assert!(cli(Some("ws://x"), None, None).resolve().is_err());
    }

    #[test]
    fn file_fills_gaps_and_cli_wins() {
        let dir = std::env::temp_dir().join(format!("minimon-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("agent.toml");
        std::fs::write(
            &p,
            "server = \"ws://file\"\ntoken = \"ft\"\ninterval = 5\niface_include = [\"eth0\"]\n",
        )
        .unwrap();
        let c = cli(Some("ws://cli"), None, Some(p)).resolve().unwrap();
        assert_eq!(c.server, "ws://cli");
        assert_eq!(c.token, "ft");
        assert_eq!(c.interval_override, Some(5));
        assert_eq!(c.iface_include, vec!["eth0".to_string()]);
        std::fs::remove_dir_all(dir).ok();
    }
}
