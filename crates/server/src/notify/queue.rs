//! 合并发送队列：窗口内合并、读 SendKey、失败重试、写回 `sent` 状态。

use super::{MakeNotifier, NotifyError, merge};
use crate::alert::engine::Notice;
use crate::alert::model::SendStatus;
use crate::db::Db;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

struct Batch {
    ids: Vec<i64>,
    notices: Vec<Notice>,
}

#[derive(Clone)]
pub struct NotifyQueue {
    tx: mpsc::UnboundedSender<Batch>,
}

impl NotifyQueue {
    /// `window`：首批到达后等待合并的时长；`retry`：失败后两次重试前的等待。
    pub fn spawn(
        db: Arc<Db>,
        shutdown: watch::Receiver<bool>,
        window: Duration,
        retry: [Duration; 2],
        make: MakeNotifier,
    ) -> (NotifyQueue, JoinHandle<()>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let handle = tokio::spawn(run(db, shutdown, window, retry, make, rx));
        (NotifyQueue { tx }, handle)
    }

    /// 非阻塞入队；队列已停止时静默丢弃。
    pub fn enqueue(&self, event_ids: Vec<i64>, notices: Vec<Notice>) {
        if event_ids.is_empty() && notices.is_empty() {
            return;
        }
        let _ = self.tx.send(Batch {
            ids: event_ids,
            notices,
        });
    }
}

async fn run(
    db: Arc<Db>,
    mut shutdown: watch::Receiver<bool>,
    window: Duration,
    retry: [Duration; 2],
    make: MakeNotifier,
    mut rx: mpsc::UnboundedReceiver<Batch>,
) {
    loop {
        let first = tokio::select! {
            b = rx.recv() => match b { Some(b) => b, None => return },
            _ = shutdown.wait_for(|v| *v) => return,
        };
        let mut ids = first.ids;
        let mut notices = first.notices;
        let deadline = tokio::time::Instant::now() + window;
        let mut stopped = false;
        loop {
            tokio::select! {
                b = rx.recv() => match b {
                    Some(b) => { ids.extend(b.ids); notices.extend(b.notices); }
                    None => break,
                },
                _ = tokio::time::sleep_until(deadline) => break,
                _ = shutdown.wait_for(|v| *v) => { stopped = true; break; }
            }
        }
        if stopped {
            drop_pending(&mut rx, ids.len());
            return;
        }
        let n = ids.len();
        tokio::select! {
            _ = deliver(&db, &make, retry, ids, notices) => {}
            _ = shutdown.wait_for(|v| *v) => {
                drop_pending(&mut rx, n);
                return;
            }
        }
    }
}

fn drop_pending(rx: &mut mpsc::UnboundedReceiver<Batch>, mut count: usize) {
    while let Ok(b) = rx.try_recv() {
        count += b.ids.len();
    }
    tracing::warn!(count, "停机时丢弃未发送的通知");
}

async fn deliver(
    db: &Arc<Db>,
    make: &MakeNotifier,
    retry: [Duration; 2],
    ids: Vec<i64>,
    notices: Vec<Notice>,
) {
    let d = db.clone();
    let key = match tokio::task::spawn_blocking(move || d.sendkey()).await {
        Ok(Ok(k)) => k,
        _ => {
            tracing::error!("读取 SendKey 失败");
            return mark(db, ids, SendStatus::Failed).await;
        }
    };
    let Some(key) = key else {
        return mark(db, ids, SendStatus::Skipped).await;
    };
    let status = match send_with_retry(make, &key, retry, &notices).await {
        Ok(()) => SendStatus::Ok,
        Err(e) => {
            tracing::warn!(error = %e, count = ids.len(), "通知发送失败");
            SendStatus::Failed
        }
    };
    mark(db, ids, status).await;
}

async fn send_with_retry(
    make: &MakeNotifier,
    key: &str,
    retry: [Duration; 2],
    notices: &[Notice],
) -> Result<(), NotifyError> {
    let notifier = make(key)?;
    let (title, body) = merge(notices);
    let mut res = notifier.send(&title, &body).await;
    for wait in retry {
        if res.is_ok() {
            break;
        }
        tokio::time::sleep(wait).await;
        res = notifier.send(&title, &body).await;
    }
    res
}

async fn mark(db: &Arc<Db>, ids: Vec<i64>, status: SendStatus) {
    if ids.is_empty() {
        return;
    }
    let d = db.clone();
    match tokio::task::spawn_blocking(move || d.set_events_sent(&ids, status)).await {
        Ok(Ok(())) => {}
        _ => tracing::error!("写回通知状态失败"),
    }
}
