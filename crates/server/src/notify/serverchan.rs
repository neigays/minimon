//! Server酱³：`POST https://<uid>.push.ft07.com/send/<sendkey>.send`。

use super::{BoxFuture, Notifier, NotifyError, truncate_chars};
use std::sync::Arc;
use std::time::Duration;

const TITLE_MAX_CHARS: usize = 32;
const TIMEOUT: Duration = Duration::from_secs(10);

/// 从 SendKey 解析 uid：匹配 `^sctp(\d+)t`。
pub fn parse_uid(sendkey: &str) -> Option<&str> {
    let rest = sendkey.strip_prefix("sctp")?;
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 || rest.as_bytes().get(digits) != Some(&b't') {
        return None;
    }
    Some(&rest[..digits])
}

pub struct ServerChan {
    client: reqwest::Client,
    url: String,
}

impl ServerChan {
    pub fn new(sendkey: &str) -> Result<Self, NotifyError> {
        let uid = parse_uid(sendkey).ok_or(NotifyError::Config("SendKey 格式不正确"))?;
        let url = format!("https://{uid}.push.ft07.com/send/{sendkey}.send");
        Self::build(url)
    }

    #[cfg(test)]
    pub fn with_endpoint(url: String) -> Result<Self, NotifyError> {
        Self::build(url)
    }

    fn build(url: String) -> Result<Self, NotifyError> {
        // 静态 musl 部署没有系统根证书：内置 webpki-roots，加密后端显式用 ring，
        // 不依赖进程级默认 provider。
        let roots = rustls::RootCertStore {
            roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
        };
        let tls = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .map_err(|_| NotifyError::Config("TLS 初始化失败"))?
        .with_root_certificates(roots)
        .with_no_client_auth();
        let client = reqwest::Client::builder()
            .use_preconfigured_tls(tls)
            .timeout(TIMEOUT)
            .build()
            .map_err(|e| NotifyError::Http(e.without_url().to_string()))?;
        Ok(Self { client, url })
    }
}

impl Notifier for ServerChan {
    fn send<'a>(&'a self, title: &'a str, body: &'a str) -> BoxFuture<'a, Result<(), NotifyError>> {
        Box::pin(async move {
            let mut t = title.to_string();
            truncate_chars(&mut t, TITLE_MAX_CHARS);
            let resp = self
                .client
                .post(&self.url)
                .form(&[("title", t.as_str()), ("desp", body)])
                .send()
                .await
                .map_err(|e| NotifyError::Http(e.without_url().to_string()))?;
            let v: serde_json::Value = resp
                .json()
                .await
                .map_err(|e| NotifyError::Http(e.without_url().to_string()))?;
            match v.get("code").and_then(|c| c.as_i64()) {
                Some(0) => Ok(()),
                code => Err(NotifyError::Api {
                    code: code.unwrap_or(-1),
                    message: v
                        .get("message")
                        .and_then(|m| m.as_str())
                        .unwrap_or("未知错误")
                        .to_string(),
                }),
            }
        })
    }
}
