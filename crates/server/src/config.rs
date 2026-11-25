use serde::Deserialize;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("读取配置文件 {0} 失败: {1}")]
    Read(PathBuf, std::io::Error),
    #[error("解析配置文件失败: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("MINIMON_LISTEN 不是有效的监听地址: {0}")]
    BadListen(#[from] std::net::AddrParseError),
    #[error("report_interval_secs 必须大于 0")]
    BadInterval,
    #[error("public_url 必须以 http:// 或 https:// 开头")]
    BadPublicUrl,
    #[error("保留期（retention_*）必须大于 0")]
    BadRetention,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ServerConfig {
    pub listen: SocketAddr,
    pub data_dir: PathBuf,
    pub report_interval_secs: u32,
    /// 对外访问地址（http:// 或 https://，末尾 `/` 会被去掉）。
    pub public_url: Option<String>,
    /// 反向代理传递真实客户端 IP 的请求头；配置后才信任代理头。
    pub real_ip_header: Option<String>,
    pub retention_raw_hours: u32,
    pub retention_1m_days: u32,
    pub retention_15m_days: u32,
    pub retention_probe_raw_hours: u32,
    pub retention_probe_5m_days: u32,
}

impl Default for ServerConfig {
    fn default() -> Self {
        ServerConfig {
            listen: SocketAddr::from(([0, 0, 0, 0], 25780)),
            data_dir: PathBuf::from("./data"),
            report_interval_secs: 3,
            public_url: None,
            real_ip_header: None,
            retention_raw_hours: 24,
            retention_1m_days: 7,
            retention_15m_days: 90,
            retention_probe_raw_hours: 24,
            retention_probe_5m_days: 30,
        }
    }
}

impl ServerConfig {
    /// 加载 TOML（可选），再应用环境变量
    /// `MINIMON_LISTEN` / `MINIMON_DATA_DIR` / `MINIMON_PUBLIC_URL` / `MINIMON_REAL_IP_HEADER`。
    pub fn load(path: Option<&Path>) -> Result<Self, ConfigError> {
        let cfg = match path {
            Some(p) => {
                let text = std::fs::read_to_string(p)
                    .map_err(|e| ConfigError::Read(p.to_path_buf(), e))?;
                toml::from_str(&text)?
            }
            None => ServerConfig::default(),
        };
        let mut cfg = cfg.with_overrides(
            std::env::var("MINIMON_LISTEN").ok().as_deref(),
            std::env::var("MINIMON_DATA_DIR").ok().as_deref(),
        )?;
        if let Ok(v) = std::env::var("MINIMON_PUBLIC_URL") {
            cfg.public_url = Some(v);
        }
        if let Ok(v) = std::env::var("MINIMON_REAL_IP_HEADER") {
            cfg.real_ip_header = Some(v);
        }
        cfg.validated()
    }

    /// 校验并规范化（public_url 去掉末尾 `/`）。
    pub fn validated(mut self) -> Result<Self, ConfigError> {
        if self.report_interval_secs == 0 {
            return Err(ConfigError::BadInterval);
        }
        if self.retention_raw_hours == 0
            || self.retention_1m_days == 0
            || self.retention_15m_days == 0
            || self.retention_probe_raw_hours == 0
            || self.retention_probe_5m_days == 0
        {
            return Err(ConfigError::BadRetention);
        }
        // 空或纯空白的代理头视为未配置
        self.real_ip_header = self
            .real_ip_header
            .take()
            .map(|h| h.trim().to_string())
            .filter(|h| !h.is_empty());
        if let Some(u) = &self.public_url {
            if !(u.starts_with("http://") || u.starts_with("https://")) {
                return Err(ConfigError::BadPublicUrl);
            }
            self.public_url = Some(u.trim_end_matches('/').to_string());
        }
        Ok(self)
    }

    fn with_overrides(
        mut self,
        listen: Option<&str>,
        data_dir: Option<&str>,
    ) -> Result<Self, ConfigError> {
        if let Some(l) = listen {
            self.listen = l.parse()?;
        }
        if let Some(d) = data_dir {
            self.data_dir = PathBuf::from(d);
        }
        Ok(self)
    }

    pub fn db_path(&self) -> PathBuf {
        self.data_dir.join("minimon.db")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults() {
        let c = ServerConfig::default();
        assert_eq!(c.listen.to_string(), "0.0.0.0:25780");
        assert_eq!(c.data_dir, PathBuf::from("./data"));
        assert_eq!(c.report_interval_secs, 3);
    }

    #[test]
    fn toml_partial_uses_defaults() {
        let c: ServerConfig = toml::from_str("listen = \"127.0.0.1:1234\"").unwrap();
        assert_eq!(c.listen.port(), 1234);
        assert_eq!(c.report_interval_secs, 3);
    }

    #[test]
    fn unknown_field_rejected() {
        assert!(toml::from_str::<ServerConfig>("listn = \"127.0.0.1:1\"").is_err());
    }

    #[test]
    fn zero_interval_rejected() {
        let c: ServerConfig = toml::from_str("report_interval_secs = 0").unwrap();
        assert!(matches!(c.validated(), Err(ConfigError::BadInterval)));
        assert!(ServerConfig::default().validated().is_ok());
    }

    #[test]
    fn overrides_win() {
        let c = ServerConfig::default()
            .with_overrides(Some("127.0.0.1:9"), Some("/x"))
            .unwrap();
        assert_eq!(c.listen.to_string(), "127.0.0.1:9");
        assert_eq!(c.data_dir, PathBuf::from("/x"));
        assert!(
            ServerConfig::default()
                .with_overrides(Some("nope"), None)
                .is_err()
        );
    }

    #[test]
    fn m2_defaults() {
        let c = ServerConfig::default();
        assert_eq!(
            (
                c.retention_raw_hours,
                c.retention_1m_days,
                c.retention_15m_days
            ),
            (24, 7, 90)
        );
        assert!(c.public_url.is_none() && c.real_ip_header.is_none());
    }

    #[test]
    fn m3_probe_retention_defaults_and_validation() {
        let c = ServerConfig::default();
        assert_eq!(
            (c.retention_probe_raw_hours, c.retention_probe_5m_days),
            (24, 30)
        );
        for t in [
            "retention_probe_raw_hours = 0",
            "retention_probe_5m_days = 0",
        ] {
            let c: ServerConfig = toml::from_str(t).unwrap();
            assert!(
                matches!(c.validated(), Err(ConfigError::BadRetention)),
                "{t}"
            );
        }
    }

    #[test]
    fn public_url_validated_and_trimmed() {
        let c: ServerConfig = toml::from_str(r#"public_url = "https://m.example.com/""#).unwrap();
        assert_eq!(
            c.validated().unwrap().public_url.as_deref(),
            Some("https://m.example.com")
        );
        let bad: ServerConfig = toml::from_str(r#"public_url = "m.example.com""#).unwrap();
        assert!(matches!(bad.validated(), Err(ConfigError::BadPublicUrl)));
    }

    #[test]
    fn blank_real_ip_header_is_none() {
        for t in ["", "   "] {
            let c = ServerConfig {
                real_ip_header: Some(t.into()),
                ..ServerConfig::default()
            };
            assert_eq!(c.validated().unwrap().real_ip_header, None);
        }
        let c = ServerConfig {
            real_ip_header: Some(" X-Real-IP ".into()),
            ..ServerConfig::default()
        };
        assert_eq!(
            c.validated().unwrap().real_ip_header.as_deref(),
            Some("X-Real-IP")
        );
    }

    #[test]
    fn zero_retention_rejected() {
        for t in [
            "retention_raw_hours = 0",
            "retention_1m_days = 0",
            "retention_15m_days = 0",
        ] {
            let c: ServerConfig = toml::from_str(t).unwrap();
            assert!(
                matches!(c.validated(), Err(ConfigError::BadRetention)),
                "{t}"
            );
        }
    }
}
