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

/// 规范化 SendKey：去空白；整段 API 地址取 `/send/` 之后；去 `?` 起的查询串与末尾 `.send`；
/// 校验 `^sctp\d+t[A-Za-z0-9_-]+$`。不合法返回 None。
pub fn normalize_sendkey(raw: &str) -> Option<String> {
    let mut k = raw.trim();
    if let Some((_, rest)) = k.split_once("/send/") {
        k = rest;
    }
    if let Some((head, _)) = k.split_once('?') {
        k = head;
    }
    let k = k.strip_suffix(".send").unwrap_or(k);
    let uid = parse_uid(k)?;
    let rest = &k[4 + uid.len() + 1..];
    let ok = !rest.is_empty()
        && rest
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    ok.then(|| k.to_string())
}

pub struct ServerChan {
    client: reqwest::Client,
    url: String,
    /// 错误原文里要替换掉的 SendKey。
    key: String,
}

const BODY_SNIPPET_CHARS: usize = 100;

impl ServerChan {
    pub fn new(sendkey: &str) -> Result<Self, NotifyError> {
        let key = normalize_sendkey(sendkey).ok_or(NotifyError::Config("SendKey 格式不正确"))?;
        let uid = parse_uid(&key).ok_or(NotifyError::Config("SendKey 格式不正确"))?;
        let url = format!("https://{uid}.push.ft07.com/send/{key}.send");
        let mut sc = Self::build(url)?;
        sc.key = key;
        Ok(sc)
    }

    #[cfg(test)]
    pub fn with_endpoint(url: String) -> Result<Self, NotifyError> {
        Self::build(url)
    }

    #[cfg(test)]
    pub fn redacting(mut self, key: &str) -> Self {
        self.key = key.to_string();
        self
    }

    fn redact(&self, s: &str) -> String {
        if self.key.is_empty() {
            s.to_string()
        } else {
            s.replace(&self.key, "***")
        }
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
        Ok(Self {
            client,
            url,
            key: String::new(),
        })
    }
}

impl Notifier for ServerChan {
    fn send<'a>(
        &'a self,
        title: &'a str,
        body: &'a str,
        tags: &'a str,
    ) -> BoxFuture<'a, Result<(), NotifyError>> {
        Box::pin(async move {
            let mut t = title.to_string();
            truncate_chars(&mut t, TITLE_MAX_CHARS);
            let mut form = vec![("title", t.as_str()), ("desp", body)];
            if !tags.is_empty() {
                form.push(("tags", tags));
            }
            let resp = self
                .client
                .post(&self.url)
                .form(&form)
                .send()
                .await
                .map_err(|e| NotifyError::Http(self.redact(&e.without_url().to_string())))?;
            let status = resp.status();
            let text = resp
                .text()
                .await
                .map_err(|e| NotifyError::Http(self.redact(&e.without_url().to_string())))?;
            let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
                let mut snippet = self.redact(text.trim());
                truncate_chars(&mut snippet, BODY_SNIPPET_CHARS);
                return Err(NotifyError::Response(format!(
                    "推送服务返回 HTTP {}：{snippet}",
                    status.as_u16()
                )));
            };
            match v.get("code").and_then(|c| c.as_i64()) {
                Some(0) => Ok(()),
                code => Err(NotifyError::Api {
                    code: code.unwrap_or(-1),
                    message: self.redact(
                        v.get("message")
                            .or_else(|| v.get("error"))
                            .and_then(|m| m.as_str())
                            .unwrap_or("未知错误"),
                    ),
                }),
            }
        })
    }
}
