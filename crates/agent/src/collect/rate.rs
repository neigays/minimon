//! 由累计计数器计算每秒速率。

#[derive(Debug, Default)]
pub struct RateTracker {
    prev: Option<(u64, u64, i64)>,
}

impl RateTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// 返回每秒 (rx, tx) 速率。首次调用、时间未前进时返回 0；
    /// 某方向计数器回落（网卡重建、回绕）时，该方向记 0。
    pub fn update(&mut self, rx: u64, tx: u64, now_ms: i64) -> (u64, u64) {
        let prev = self.prev.replace((rx, tx, now_ms));
        let Some((prx, ptx, pms)) = prev else {
            return (0, 0);
        };
        let dt = now_ms - pms;
        if dt <= 0 {
            return (0, 0);
        }
        let rate = |cur: u64, old: u64| {
            if cur < old {
                0
            } else {
                ((cur - old) as u128 * 1000 / dt as u128) as u64
            }
        };
        (rate(rx, prx), rate(tx, ptx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_update_is_zero() {
        assert_eq!(RateTracker::new().update(1000, 1000, 0), (0, 0));
    }

    #[test]
    fn rate_per_second() {
        let mut t = RateTracker::new();
        t.update(0, 0, 0);
        assert_eq!(t.update(3000, 6000, 3000), (1000, 2000));
    }

    #[test]
    fn counter_drop_gives_zero_not_huge() {
        let mut t = RateTracker::new();
        t.update(10_000, 10_000, 0);
        assert_eq!(t.update(500, 20_000, 1000), (0, 10_000));
    }

    #[test]
    fn zero_elapsed_does_not_divide_by_zero() {
        let mut t = RateTracker::new();
        t.update(0, 0, 1000);
        assert_eq!(t.update(100, 100, 1000), (0, 0));
    }
}
