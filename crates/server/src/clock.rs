//! agent 上报时间戳的归一化：偏差过大时改用 server 接收时间。

/// agent 时间与 server 时间允许的最大偏差（毫秒），等于该值时保留 agent 时间。
pub const TS_SKEW_MAX_MS: i64 = 10_000;

/// 返回 `(采用的时间戳, 是否被替换)`。
///
/// `|agent_ts_ms - server_now_ms| > TS_SKEW_MAX_MS` 时使用 `server_now_ms`。
pub fn normalize_ts(agent_ts_ms: i64, server_now_ms: i64) -> (i64, bool) {
    if agent_ts_ms.abs_diff(server_now_ms) > TS_SKEW_MAX_MS as u64 {
        (server_now_ms, true)
    } else {
        (agent_ts_ms, false)
    }
}

/// 每个连接一个：在 [`normalize_ts`] 之上保证时间戳严格单调递增。
///
/// - 粘性：一旦某条上报被替换为接收时间，本连接后续一律用接收时间，
///   避免 agent 在阈值附近时「保留 agent 时间」与「替换为接收时间」交替而乱序；
/// - 前向单调：`ts = max(ts, last_ts + 1)`，`last_ts` 以该节点最后已知上报的时间戳播种，
///   乱序样本会让累计计数器看起来「回落」，把整个自启动以来的计数加进流量增量。
#[derive(Debug, Clone, Copy, Default)]
pub struct TsNormalizer {
    sticky: bool,
    last_ts: i64,
}

impl TsNormalizer {
    /// `seed` 为该节点最后已知的时间戳（没有则 `None`）；晚于 `now` 的种子视为无效，避免永久前置。
    pub fn new(seed: Option<i64>, now: i64) -> Self {
        Self {
            sticky: false,
            last_ts: seed.map_or(i64::MIN, |s| s.min(now)),
        }
    }

    /// 返回 `(采用的时间戳, 是否与 agent 时间不同)`。
    pub fn apply(&mut self, agent_ts: i64, now: i64) -> (i64, bool) {
        let (mut ts, replaced) = normalize_ts(agent_ts, now);
        if replaced {
            self.sticky = true;
        }
        if self.sticky {
            ts = now;
        }
        ts = ts.max(self.last_ts.saturating_add(1));
        self.last_ts = ts;
        (ts, ts != agent_ts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_small_skew() {
        assert_eq!(normalize_ts(1_000_000 - 5_000, 1_000_000), (995_000, false));
    }

    #[test]
    fn replaces_slow_clock() {
        assert_eq!(
            normalize_ts(1_000_000 - 60_000, 1_000_000),
            (1_000_000, true)
        );
    }

    #[test]
    fn replaces_fast_clock() {
        assert_eq!(
            normalize_ts(1_000_000 + 7_200_000, 1_000_000),
            (1_000_000, true)
        );
    }

    #[test]
    fn boundary_is_kept() {
        assert!(!normalize_ts(1_000_000 - 10_000, 1_000_000).1);
        assert!(!normalize_ts(1_000_000 + 10_000, 1_000_000).1);
        assert!(normalize_ts(1_000_000 + 10_001, 1_000_000).1);
    }

    #[test]
    fn sticky_and_monotonic() {
        let mut n = TsNormalizer::new(None, 0);
        let now = 1_000_000;
        // 第一条偏差 9.9s 保留；第二条 10.1s 被替换，其时间戳不得早于第一条。
        assert_eq!(n.apply(now + 9_900, now), (now + 9_900, false));
        let (t2, r2) = n.apply(now + 10_100, now);
        assert!(r2 && t2 > now + 9_900);
        // 粘性：后续即使偏差回到阈值内也不再用 agent 时间，且严格递增。
        let (t3, _) = n.apply(now + 100, now + 3_000);
        assert!(t3 > t2);
    }

    #[test]
    fn seeded_forward_only() {
        let mut n = TsNormalizer::new(Some(500), 1_000);
        assert_eq!(n.apply(400, 1_000), (501, true));
        assert_eq!(n.apply(400, 1_000), (502, true));
        // 未来种子被限制到 now。
        let mut f = TsNormalizer::new(Some(9_999_999), 1_000);
        assert_eq!(f.apply(1_000, 1_000), (1_001, true));
    }
}
