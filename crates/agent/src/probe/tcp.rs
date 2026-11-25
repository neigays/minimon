//! TCP 探测：每个样本一次 connect 计时。

use std::time::{Duration, Instant};

use tokio::net::TcpStream;
use tokio::time::timeout;

use super::{Fail, ProbeOutcome, finish, resolve, resolve_failed};

/// `target` 为 `host:port`。
pub async fn probe_tcp(target: &str, samples: u32, limit: Duration) -> ProbeOutcome {
    let Some(addr) = resolve(target, limit).await else {
        return resolve_failed(samples);
    };
    let mut results = Vec::with_capacity(samples as usize);
    let mut fails = Vec::new();
    for _ in 0..samples {
        let t0 = Instant::now();
        match timeout(limit, TcpStream::connect(addr)).await {
            Ok(Ok(_)) => results.push(Some(t0.elapsed().as_secs_f64() * 1000.0)),
            Ok(Err(_)) => {
                results.push(None);
                fails.push(Fail::Connect);
            }
            Err(_) => {
                results.push(None);
                fails.push(Fail::Timeout);
            }
        }
    }
    finish(&results, &fails)
}
