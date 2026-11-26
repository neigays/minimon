//! 通知通道：`Notifier` 抽象、Server酱³ 实现、合并发送队列。
//!
//! SendKey 与完整推送 URL 不得出现在日志或错误文本中。

mod queue;
mod serverchan;

use crate::alert::engine::Notice;
use crate::db::Db;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

pub use queue::NotifyQueue;
pub use serverchan::{ServerChan, normalize_sendkey, parse_uid};

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

#[derive(Debug, thiserror::Error)]
pub enum NotifyError {
    #[error("{0}")]
    Config(&'static str),
    #[error("推送请求失败：{0}")]
    Http(String),
    #[error("推送服务返回错误 {code}：{message}")]
    Api { code: i64, message: String },
    /// 推送服务返回了非 JSON 内容；文本已含状态码并去除 SendKey。
    #[error("{0}")]
    Response(String),
}

/// 通知类型标签（Server酱³ `tags`），数组顺序即合并后的排列顺序。
pub mod tag {
    pub const OFFLINE: &str = "离线";
    pub const ONLINE: &str = "上线";
    pub const FIRING: &str = "告警";
    pub const RESOLVED: &str = "恢复";
    pub const TRAFFIC: &str = "流量";
    pub const EXPIRE: &str = "到期";
    pub const TEST: &str = "测试";
    pub const ORDER: [&str; 7] = [OFFLINE, ONLINE, FIRING, RESOLVED, TRAFFIC, EXPIRE, TEST];
}

const MAX_PUSH_TAGS: usize = 5;
const MAX_PUSH_TAG_CHARS: usize = 16;

/// 规范化全局推送标签：`|` 分隔，去空白、去空段、去重。
pub fn normalize_push_tags(raw: &str) -> Result<String, &'static str> {
    let mut out: Vec<&str> = Vec::new();
    for t in raw.split('|').map(str::trim).filter(|t| !t.is_empty()) {
        if t.chars().count() > MAX_PUSH_TAG_CHARS {
            return Err("每个推送标签最多 16 个字符");
        }
        if !out.contains(&t) {
            out.push(t);
        }
    }
    if out.len() > MAX_PUSH_TAGS {
        return Err("推送标签最多 5 个");
    }
    Ok(out.join("|"))
}

/// 最终 tags：全局标签 + 通知类型标签（按 [`tag::ORDER`]），去重。
pub fn tags_for(global: &str, notices: &[Notice]) -> String {
    tags_with(global, |t| notices.iter().any(|n| n.tag == t))
}

fn tags_with(global: &str, present: impl Fn(&str) -> bool) -> String {
    let mut out: Vec<&str> = global.split('|').filter(|t| !t.is_empty()).collect();
    for t in tag::ORDER {
        if present(t) && !out.contains(&t) {
            out.push(t);
        }
    }
    out.join("|")
}

/// 读出规范化后的 SendKey（兼容旧值带 `.send` 等）与全局推送标签。
pub(crate) fn load_target(db: &Db) -> Result<Option<(String, String)>, NotifyError> {
    let key = db
        .sendkey()
        .map_err(|_| NotifyError::Http("读取 SendKey 失败".into()))?;
    let Some(key) = key else {
        return Ok(None);
    };
    let key = normalize_sendkey(&key).ok_or(NotifyError::Config("SendKey 格式不正确"))?;
    let tags = db
        .push_tags()
        .map_err(|_| NotifyError::Http("读取推送标签失败".into()))?;
    Ok(Some((key, tags)))
}

pub trait Notifier: Send + Sync {
    fn send<'a>(
        &'a self,
        title: &'a str,
        body: &'a str,
        tags: &'a str,
    ) -> BoxFuture<'a, Result<(), NotifyError>>;
}

/// 由 SendKey 构造 notifier 的工厂。用 `Arc<dyn Fn>` 而非裸 `fn` 指针，
/// 这样测试可以注入带计数器的 notifier；生产用 [`serverchan_factory`]。
pub type MakeNotifier = Arc<dyn Fn(&str) -> Result<Box<dyn Notifier>, NotifyError> + Send + Sync>;

pub fn serverchan_factory() -> MakeNotifier {
    Arc::new(|key| Ok(Box::new(ServerChan::new(key)?) as Box<dyn Notifier>))
}

const MAX_ITEMS: usize = 20;
const MAX_BODY_CHARS: usize = 4000;

fn truncate_chars(s: &mut String, max: usize) {
    if let Some((idx, _)) = s.char_indices().nth(max) {
        s.truncate(idx);
    }
}

/// 把多条通知合成一条：1 条原样，多条合并为 `minimon：N 条告警`。
pub fn merge(notices: &[Notice]) -> (String, String) {
    match notices {
        [] => (String::new(), String::new()),
        [one] => (one.title.clone(), one.body.clone()),
        many => {
            let mut body = String::new();
            for n in many.iter().take(MAX_ITEMS) {
                if !body.is_empty() {
                    body.push_str("\n\n");
                }
                body.push_str("### ");
                body.push_str(&n.title);
                body.push('\n');
                body.push_str(&n.body);
            }
            if many.len() > MAX_ITEMS {
                body.push_str(&format!("\n\n…另有 {} 条", many.len() - MAX_ITEMS));
            }
            truncate_chars(&mut body, MAX_BODY_CHARS);
            (format!("minimon：{} 条告警", many.len()), body)
        }
    }
}

/// 直接发送测试消息，不经队列、不写事件。
pub async fn send_test(db: &Arc<Db>, make: &MakeNotifier) -> Result<(), NotifyError> {
    let d = db.clone();
    let (key, global) = tokio::task::spawn_blocking(move || load_target(&d))
        .await
        .map_err(|_| NotifyError::Http("内部错误".into()))??
        .ok_or(NotifyError::Config("尚未配置 SendKey"))?;
    let notifier = make(&key)?;
    let tags = tags_with(&global, |t| t == tag::TEST);
    notifier
        .send(
            "minimon 测试消息",
            "这是一条来自 minimon 的测试消息。",
            &tags,
        )
        .await
}

#[cfg(test)]
mod tests;
