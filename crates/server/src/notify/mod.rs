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
pub use serverchan::{ServerChan, parse_uid};

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

#[derive(Debug, thiserror::Error)]
pub enum NotifyError {
    #[error("{0}")]
    Config(&'static str),
    #[error("推送请求失败：{0}")]
    Http(String),
    #[error("推送服务返回错误 {code}：{message}")]
    Api { code: i64, message: String },
}

pub trait Notifier: Send + Sync {
    fn send<'a>(&'a self, title: &'a str, body: &'a str) -> BoxFuture<'a, Result<(), NotifyError>>;
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
    let key = tokio::task::spawn_blocking(move || d.sendkey())
        .await
        .map_err(|_| NotifyError::Http("内部错误".into()))?
        .map_err(|_| NotifyError::Http("读取 SendKey 失败".into()))?
        .ok_or(NotifyError::Config("尚未配置 SendKey"))?;
    let notifier = make(&key)?;
    notifier
        .send("minimon 测试消息", "这是一条来自 minimon 的测试消息。")
        .await
}

#[cfg(test)]
mod tests;
