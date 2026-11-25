//! 探测调度：按任务周期执行 TCP/HTTP/ICMP 探测，经 mpsc 交付结果。
//!
//! agent 只发起出站连接，不执行任何外部命令。错误文案只使用固定短语，
//! 绝不带上目标地址（server 会对访客隐藏 target）。

mod http;
mod icmp;
mod tcp;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use minimon_common::protocol::{ProbeKind, ProbeResult, ProbeTask};
use tokio::sync::{Semaphore, mpsc};
use tokio::task::JoinHandle;
use tracing::debug;

pub use http::probe_http;
pub use icmp::{IcmpState, probe_icmp};
pub use tcp::probe_tcp;

/// 全局并发探测上限。
const MAX_CONCURRENCY: usize = 8;

pub const ERR_ICMP: &str = "ICMP 不可用";
pub const ERR_RESOLVE: &str = "解析失败";
pub const ERR_TIMEOUT: &str = "超时";
pub const ERR_CONNECT: &str = "连接失败";
pub const ERR_STATUS: &str = "HTTP 状态异常";
pub const ERR_OTHER: &str = "探测失败";

/// 一轮探测的汇总。
#[derive(Debug, Clone, PartialEq)]
pub struct ProbeOutcome {
    pub sent: u32,
    pub lost: u32,
    pub avg_ms: Option<f64>,
    pub min_ms: Option<f64>,
    pub max_ms: Option<f64>,
    pub error: Option<&'static str>,
}

/// 汇总样本（`None` 表示丢失）。不设置 `error`。
pub fn summarize(samples: &[Option<f64>]) -> ProbeOutcome {
    let ok: Vec<f64> = samples.iter().flatten().copied().collect();
    let avg_ms = (!ok.is_empty()).then(|| ok.iter().sum::<f64>() / ok.len() as f64);
    ProbeOutcome {
        sent: samples.len() as u32,
        lost: (samples.len() - ok.len()) as u32,
        avg_ms,
        min_ms: ok.iter().copied().reduce(f64::min),
        max_ms: ok.iter().copied().reduce(f64::max),
        error: None,
    }
}

/// 单个样本失败的原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Fail {
    Timeout,
    Connect,
    Status,
    Other,
}

/// 全部样本失败时挑选对外错误短语；只要有任一样本成功则不报错。
pub(crate) fn finish(samples: &[Option<f64>], fails: &[Fail]) -> ProbeOutcome {
    let mut o = summarize(samples);
    if o.sent > 0 && o.lost == o.sent {
        let pick = |f: Fail| fails.contains(&f);
        o.error = Some(if pick(Fail::Status) {
            ERR_STATUS
        } else if pick(Fail::Connect) {
            ERR_CONNECT
        } else if pick(Fail::Other) {
            ERR_OTHER
        } else {
            ERR_TIMEOUT
        });
    }
    o
}

/// 解析失败：全部样本丢失。
pub(crate) fn resolve_failed(samples: u32) -> ProbeOutcome {
    ProbeOutcome {
        error: Some(ERR_RESOLVE),
        ..summarize(&vec![None; samples as usize])
    }
}

/// 每轮解析一次；解析出错或超时都算「解析失败」。
pub(crate) async fn resolve(target: &str, timeout: Duration) -> Option<std::net::SocketAddr> {
    match tokio::time::timeout(timeout, tokio::net::lookup_host(target)).await {
        Ok(Ok(mut it)) => it.next(),
        _ => None,
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

async fn run_once(task: &ProbeTask, icmp: &IcmpState) -> ProbeOutcome {
    let timeout = Duration::from_millis(u64::from(task.timeout_ms));
    match task.kind {
        ProbeKind::Tcp => probe_tcp(&task.target, task.samples, timeout).await,
        ProbeKind::Http => probe_http(&task.target, task.samples, timeout).await,
        ProbeKind::Icmp => probe_icmp(&task.target, task.samples, timeout, icmp).await,
    }
}

fn task_loop(
    task: ProbeTask,
    tx: mpsc::Sender<ProbeResult>,
    sem: Arc<Semaphore>,
    icmp: Arc<IcmpState>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let period = Duration::from_secs(u64::from(task.interval_secs.max(1)));
        let delay = period.mul_f64(rand::random_range(0.0..1.0));
        // 轮次在本循环内串行执行，不会并发两轮。上一轮超时占用的触发点直接跳过，
        // 而不是（像 interval 的 Skip 那样）结束后立刻补跑一轮。
        let mut next = tokio::time::Instant::now() + delay;
        loop {
            tokio::time::sleep_until(next).await;
            let Ok(_permit) = sem.acquire().await else {
                return;
            };
            let ts_ms = now_ms();
            let o = run_once(&task, &icmp).await;
            next += period;
            while next <= tokio::time::Instant::now() {
                next += period;
            }
            let res = ProbeResult {
                task_id: task.id,
                ts_ms,
                sent: o.sent,
                lost: o.lost,
                avg_ms: o.avg_ms,
                min_ms: o.min_ms,
                max_ms: o.max_ms,
                error: o.error.map(str::to_owned),
            };
            drop(_permit);
            if tx.send(res).await.is_err() {
                debug!("探测结果通道已关闭，任务退出");
                return;
            }
        }
    })
}

/// 探测调度器。Drop 时取消全部任务。
pub struct ProbeScheduler {
    tx: mpsc::Sender<ProbeResult>,
    sem: Arc<Semaphore>,
    icmp: Arc<IcmpState>,
    tasks: HashMap<i64, (ProbeTask, JoinHandle<()>)>,
}

impl ProbeScheduler {
    pub fn new(results: mpsc::Sender<ProbeResult>) -> Self {
        Self::with_icmp(results, Arc::new(IcmpState::new()))
    }

    /// 共享进程级的 ICMP 状态（跨重连保留「不可用」缓存）。
    pub fn with_icmp(results: mpsc::Sender<ProbeResult>, icmp: Arc<IcmpState>) -> Self {
        Self {
            tx: results,
            sem: Arc::new(Semaphore::new(MAX_CONCURRENCY)),
            icmp,
            tasks: HashMap::new(),
        }
    }

    /// 全量替换：删除的任务取消，参数不变的任务保持原节奏，新增或变更的任务重新起调度。
    pub fn replace(&mut self, tasks: Vec<ProbeTask>) {
        let mut old = std::mem::take(&mut self.tasks);
        // 同 id 重复时最后一个生效，避免覆盖 map 项而遗留无法取消的 JoinHandle
        let mut by_id: HashMap<i64, ProbeTask> = HashMap::new();
        for t in tasks {
            by_id.insert(t.id, t);
        }
        for t in by_id.into_values() {
            let entry = match old.remove(&t.id) {
                Some((prev, h)) if prev == t => (prev, h),
                Some((_, h)) => {
                    h.abort();
                    let h = task_loop(
                        t.clone(),
                        self.tx.clone(),
                        self.sem.clone(),
                        self.icmp.clone(),
                    );
                    (t.clone(), h)
                }
                None => {
                    let h = task_loop(
                        t.clone(),
                        self.tx.clone(),
                        self.sem.clone(),
                        self.icmp.clone(),
                    );
                    (t.clone(), h)
                }
            };
            self.tasks.insert(t.id, entry);
        }
        for (_, (_, h)) in old {
            h.abort();
        }
    }

    /// 取消全部任务。
    pub fn shutdown(&mut self) {
        for (_, (_, h)) in self.tasks.drain() {
            h.abort();
        }
    }
}

impl Drop for ProbeScheduler {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summarize_mixed() {
        let o = summarize(&[Some(10.0), None, Some(30.0)]);
        assert_eq!((o.sent, o.lost), (3, 1));
        assert_eq!(o.avg_ms, Some(20.0));
        assert_eq!((o.min_ms, o.max_ms), (Some(10.0), Some(30.0)));
        assert_eq!(o.error, None);
    }

    #[test]
    fn summarize_all_lost() {
        let o = summarize(&[None, None]);
        assert_eq!(o.avg_ms, None);
        assert_eq!((o.min_ms, o.max_ms), (None, None));
        assert_eq!(o.lost, 2);
    }

    #[test]
    fn finish_picks_fixed_phrases() {
        let l = [None, None];
        assert_eq!(
            finish(&l, &[Fail::Timeout, Fail::Timeout]).error,
            Some("超时")
        );
        assert_eq!(
            finish(&l, &[Fail::Timeout, Fail::Connect]).error,
            Some("连接失败")
        );
        assert_eq!(
            finish(&l, &[Fail::Status, Fail::Timeout]).error,
            Some("HTTP 状态异常")
        );
        assert_eq!(finish(&l, &[Fail::Other]).error, Some("探测失败"));
        // 部分成功不报错
        assert_eq!(finish(&[Some(1.0), None], &[Fail::Timeout]).error, None);
    }

    #[tokio::test]
    async fn schedulers_share_process_level_icmp_state() {
        let icmp = Arc::new(IcmpState::new());
        let (tx, _rx) = mpsc::channel(1);
        let a = ProbeScheduler::with_icmp(tx.clone(), icmp.clone());
        let b = ProbeScheduler::with_icmp(tx, icmp.clone());
        assert!(Arc::ptr_eq(&a.icmp, &icmp) && Arc::ptr_eq(&b.icmp, &icmp));
    }
}
