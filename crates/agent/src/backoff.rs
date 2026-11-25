//! 指数退避：base、2×base、4×base…封顶 max，再乘以 [0.8, 1.2] 的随机抖动。

use std::time::Duration;

#[derive(Debug, Clone)]
pub struct Backoff {
    base: Duration,
    max: Duration,
    attempt: u32,
}

impl Backoff {
    pub fn new(base: Duration, max: Duration) -> Self {
        Self {
            base,
            max,
            attempt: 0,
        }
    }

    #[allow(clippy::should_implement_trait)] // 接口名由任务约定
    pub fn next(&mut self) -> Duration {
        let factor = 1u32.checked_shl(self.attempt).unwrap_or(u32::MAX);
        self.attempt = self.attempt.saturating_add(1).min(32);
        let raw = self.base.saturating_mul(factor).min(self.max);
        raw.mul_f64(rand::random_range(0.8..=1.2))
    }

    pub fn reset(&mut self) {
        self.attempt = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_and_caps() {
        let mut b = Backoff::new(Duration::from_secs(1), Duration::from_secs(60));
        let s: Vec<f64> = (0..10).map(|_| b.next().as_secs_f64()).collect();
        assert!((0.8..=1.2).contains(&s[0]));
        assert!((1.6..=2.4).contains(&s[1]));
        assert!(s[9] <= 72.0 && s[9] >= 48.0);
    }

    #[test]
    fn reset_goes_back_to_base() {
        let mut b = Backoff::new(Duration::from_secs(1), Duration::from_secs(60));
        for _ in 0..5 {
            b.next();
        }
        b.reset();
        assert!(b.next() <= Duration::from_millis(1200));
    }

    #[test]
    fn many_attempts_do_not_overflow() {
        let mut b = Backoff::new(Duration::from_secs(1), Duration::from_secs(60));
        for _ in 0..100 {
            assert!(b.next() <= Duration::from_secs(73));
        }
    }
}
