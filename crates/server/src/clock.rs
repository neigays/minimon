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
}
