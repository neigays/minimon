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
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ServerConfig {
    pub listen: SocketAddr,
    pub data_dir: PathBuf,
    pub report_interval_secs: u32,
}

impl Default for ServerConfig {
    fn default() -> Self {
        ServerConfig {
            listen: SocketAddr::from(([0, 0, 0, 0], 25780)),
            data_dir: PathBuf::from("./data"),
            report_interval_secs: 3,
        }
    }
}

impl ServerConfig {
    /// 加载 TOML（可选），再应用环境变量 `MINIMON_LISTEN` / `MINIMON_DATA_DIR`。
    pub fn load(path: Option<&Path>) -> Result<Self, ConfigError> {
        let cfg = match path {
            Some(p) => {
                let text = std::fs::read_to_string(p)
                    .map_err(|e| ConfigError::Read(p.to_path_buf(), e))?;
                toml::from_str(&text)?
            }
            None => ServerConfig::default(),
        };
        let cfg = cfg.with_overrides(
            std::env::var("MINIMON_LISTEN").ok().as_deref(),
            std::env::var("MINIMON_DATA_DIR").ok().as_deref(),
        )?;
        cfg.validate()?;
        Ok(cfg)
    }

    fn validate(&self) -> Result<(), ConfigError> {
        if self.report_interval_secs == 0 {
            return Err(ConfigError::BadInterval);
        }
        Ok(())
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
        assert!(matches!(c.validate(), Err(ConfigError::BadInterval)));
        assert!(ServerConfig::default().validate().is_ok());
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
}
