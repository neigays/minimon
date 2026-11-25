//! 周期性降采样与保留期清理任务。

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::config::ServerConfig;
use crate::db::Db;
use crate::db::rollup::ROLLUP_LAG_MS;

const INTERVAL: Duration = Duration::from_secs(60);
const BATCH_NODES: usize = 10;
const HOUR_MS: i64 = 3_600_000;
const DAY_MS: i64 = 24 * HOUR_MS;

/// 启动时立即跑一轮（补上停机期间的数据），此后每 60 秒一轮；收到 shutdown 退出。
pub(crate) fn spawn(
    db: Arc<Db>,
    cfg: &ServerConfig,
    mut shutdown: watch::Receiver<bool>,
) -> JoinHandle<()> {
    let raw_ms = i64::from(cfg.retention_raw_hours) * HOUR_MS;
    let m1_ms = i64::from(cfg.retention_1m_days) * DAY_MS;
    let m15_ms = i64::from(cfg.retention_15m_days) * DAY_MS;
    tokio::spawn(async move {
        loop {
            if *shutdown.borrow() {
                return;
            }
            let db = db.clone();
            let round = tokio::task::spawn_blocking(move || {
                let now = crate::db::now_ms();
                let stats = db.rollup(now, ROLLUP_LAG_MS, BATCH_NODES)?;
                let pruned = db.prune(now, raw_ms, m1_ms, m15_ms)?;
                Ok::<_, crate::db::DbError>((stats, pruned))
            })
            .await;
            match round {
                Ok(Ok((stats, pruned))) => {
                    tracing::debug!(m1 = stats.m1, m15 = stats.m15, pruned, "rollup 完成");
                }
                Ok(Err(e)) => tracing::error!(error = %e, "rollup 失败"),
                Err(e) => tracing::error!(error = %e, "rollup 任务异常"),
            }
            tokio::select! {
                _ = tokio::time::sleep(INTERVAL) => {}
                _ = shutdown.wait_for(|v| *v) => return,
            }
        }
    })
}
