//! 批量入库：专用线程聚合 Report，定时或达到批量上限时在单事务内写入。
//!
//! 使用 `std::sync::mpsc` + `recv_timeout`：线程内无需 tokio 运行时，
//! 且天然支持“等到下一次 flush 截止时间”的阻塞等待，不会忙等。
//! 写入失败时记录错误并丢弃该批（M1 取舍：避免坏数据无限重试）。

use crate::db::Db;
use minimon_common::protocol::Report;
use std::sync::Arc;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use tokio::sync::oneshot;

pub const DEFAULT_FLUSH_EVERY: Duration = Duration::from_secs(10);
pub const DEFAULT_MAX_BATCH: usize = 500;

enum Cmd {
    Push(i64, Box<Report>),
    Flush(oneshot::Sender<usize>),
    Close,
}

#[derive(Clone)]
pub struct IngestHandle {
    tx: Sender<Cmd>,
}

pub struct IngestJoin {
    tx: Sender<Cmd>,
    thread: JoinHandle<()>,
}

impl IngestHandle {
    pub fn spawn(
        db: Arc<Db>,
        flush_every: Duration,
        max_batch: usize,
    ) -> (IngestHandle, IngestJoin) {
        let (tx, rx) = mpsc::channel();
        let max_batch = max_batch.max(1);
        let thread = std::thread::Builder::new()
            .name("ingest".into())
            .spawn(move || run(db, rx, flush_every, max_batch))
            .unwrap_or_else(|e| panic!("无法启动 ingest 线程: {e}")); // 启动阶段
        (IngestHandle { tx: tx.clone() }, IngestJoin { tx, thread })
    }

    /// 入队一条上报。线程已退出（关停之后）时忽略。
    pub fn push(&self, node_id: i64, report: Report) {
        if self.tx.send(Cmd::Push(node_id, Box::new(report))).is_err() {
            tracing::warn!(node_id, "ingest 已关闭，丢弃上报");
        }
    }

    /// 立即 flush，返回本次写入行数（失败或已关闭时为 0 / 通道关闭）。
    pub fn flush(&self) -> oneshot::Receiver<usize> {
        let (tx, rx) = oneshot::channel();
        // 发送失败时 tx 随之丢弃，接收端得到 Err。
        let _ = self.tx.send(Cmd::Flush(tx));
        rx
    }
}

impl IngestJoin {
    /// 发送 Close，flush 剩余数据后等待线程退出（阻塞调用）。
    pub fn shutdown(self) {
        let _ = self.tx.send(Cmd::Close);
        if self.thread.join().is_err() {
            tracing::error!("ingest 线程异常退出");
        }
    }
}

fn flush_buf(db: &Db, buf: &mut Vec<(i64, Report)>) -> usize {
    if buf.is_empty() {
        return 0;
    }
    let rows = std::mem::take(buf);
    match db.insert_reports(&rows) {
        Ok(n) => n,
        Err(e) => {
            tracing::error!(error = %e, rows = rows.len(), "批量入库失败，已丢弃该批数据");
            0
        }
    }
}

fn run(db: Arc<Db>, rx: mpsc::Receiver<Cmd>, flush_every: Duration, max_batch: usize) {
    let mut buf: Vec<(i64, Report)> = Vec::new();
    let mut deadline = Instant::now() + flush_every;
    loop {
        let wait = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(wait) {
            Ok(Cmd::Push(id, r)) => {
                buf.push((id, *r));
                if buf.len() >= max_batch {
                    flush_buf(&db, &mut buf);
                    deadline = Instant::now() + flush_every;
                }
            }
            Ok(Cmd::Flush(reply)) => {
                let n = flush_buf(&db, &mut buf);
                deadline = Instant::now() + flush_every;
                let _ = reply.send(n);
            }
            Ok(Cmd::Close) | Err(RecvTimeoutError::Disconnected) => {
                flush_buf(&db, &mut buf);
                return;
            }
            Err(RecvTimeoutError::Timeout) => {
                flush_buf(&db, &mut buf);
                deadline = Instant::now() + flush_every;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::tests::report;

    struct Fx {
        _dir: tempfile::TempDir,
        db: Arc<Db>,
        check: rusqlite::Connection,
    }

    fn fx() -> Fx {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.db");
        let db = Arc::new(Db::open(&path).unwrap());
        let check = rusqlite::Connection::open(&path).unwrap();
        Fx {
            _dir: dir,
            db,
            check,
        }
    }

    fn count(c: &rusqlite::Connection) -> i64 {
        c.query_row("SELECT count(*) FROM metrics_raw", [], |r| r.get(0))
            .unwrap()
    }

    fn wait_count(c: &rusqlite::Connection, want: i64, within: Duration) -> bool {
        let end = Instant::now() + within;
        while Instant::now() < end {
            if count(c) == want {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        count(c) == want
    }

    fn rep(ts: i64) -> Report {
        Report {
            ts_ms: ts,
            ..report()
        }
    }

    #[test]
    fn flushes_on_batch_size() {
        let f = fx();
        let (h, j) = IngestHandle::spawn(f.db.clone(), Duration::from_secs(3600), 3);
        for i in 0..3 {
            h.push(1, rep(i));
        }
        assert!(wait_count(&f.check, 3, Duration::from_secs(1)));
        j.shutdown();
    }

    #[test]
    fn does_not_flush_below_batch_size() {
        let f = fx();
        let (h, j) = IngestHandle::spawn(f.db.clone(), Duration::from_secs(3600), 3);
        h.push(1, rep(1));
        h.push(1, rep(2));
        let n = h.flush().blocking_recv().unwrap();
        assert_eq!(n, 2);
        assert_eq!(count(&f.check), 2);
        j.shutdown();
    }

    #[test]
    fn flushes_on_interval() {
        let f = fx();
        let (h, j) = IngestHandle::spawn(f.db.clone(), Duration::from_millis(200), 1000);
        h.push(1, rep(1));
        assert!(wait_count(&f.check, 1, Duration::from_millis(1500)));
        j.shutdown();
    }

    #[test]
    fn shutdown_flushes_pending() {
        let f = fx();
        let (h, j) = IngestHandle::spawn(f.db.clone(), Duration::from_secs(3600), 1000);
        for i in 0..5 {
            h.push(1, rep(i));
        }
        j.shutdown();
        assert_eq!(count(&f.check), 5);
        // 关停后 push 不 panic
        h.push(1, rep(99));
    }

    #[test]
    fn duplicate_ts_does_not_fail_batch() {
        let f = fx();
        let (h, j) = IngestHandle::spawn(f.db.clone(), Duration::from_secs(3600), 1000);
        h.push(1, rep(5));
        h.push(1, rep(5));
        assert_eq!(h.flush().blocking_recv().unwrap(), 2);
        assert_eq!(count(&f.check), 1);
        h.push(1, rep(6));
        assert_eq!(h.flush().blocking_recv().unwrap(), 1);
        assert_eq!(count(&f.check), 2);
        j.shutdown();
    }
}
