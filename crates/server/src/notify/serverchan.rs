//! Server酱³：`POST https://<uid>.push.ft07.com/send/<sendkey>.send`。

use super::{BoxFuture, Notifier, NotifyError, truncate_chars};
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
        // reqwest 以 rustls-no-provider 编译：这里确保 ring 已被安装为进程默认后端。
        // 已安装时返回 Err，忽略即可。
        let _ = rustls::crypto::ring::default_provider().install_default();
        let client = reqwest::Client::builder()
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
