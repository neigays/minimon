//! 告警状态机：ok → pending → firing → ok，含冷却。纯逻辑，无 IO。

use super::model::{AlertKey, StateRow};

#[derive(Debug, Clone, Default)]
pub struct Tracked {
    pub pending_since: Option<i64>,
    /// `Some` 且 `firing` 才算 firing；恢复后保留 `firing=false` 的行以记住 `last_notified_ms`。
    pub row: Option<StateRow>,
    pub last_notified_ms: Option<i64>,
}

impl Tracked {
    pub fn restore(row: StateRow) -> Tracked {
        Tracked {
            pending_since: None,
            last_notified_ms: row.last_notified_ms,
            row: Some(row),
        }
    }

    pub fn is_firing(&self) -> bool {
        self.row.as_ref().is_some_and(|r| r.firing)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transition {
    None,
    Fired { notify: bool },
    Resolved { notify: bool, since_ms: i64 },
}

/// 推进一次状态。`cond == None` 表示无数据：清 pending，firing 保持。
#[allow(clippy::too_many_arguments)]
pub fn step(
    t: &mut Tracked,
    key: AlertKey,
    cond: Option<bool>,
    value: Option<f64>,
    since_hint: Option<i64>,
    now_ms: i64,
    duration_ms: i64,
    cooldown_ms: i64,
) -> Transition {
    match cond {
        None => {
            t.pending_since = None;
            Transition::None
        }
        Some(true) => {
            if let Some(row) = t.row.as_mut().filter(|r| r.firing) {
                row.value = value;
                return Transition::None;
            }
            let since = *t.pending_since.get_or_insert(since_hint.unwrap_or(now_ms));
            if now_ms - since < duration_ms {
                return Transition::None;
            }
            let notify = t.last_notified_ms.is_none_or(|l| now_ms - l >= cooldown_ms);
            if notify {
                t.last_notified_ms = Some(now_ms);
            }
            t.pending_since = None;
            t.row = Some(StateRow {
                key,
                firing: true,
                since_ms: since,
                fired_ms: Some(now_ms),
                notified: notify,
                value,
                last_notified_ms: t.last_notified_ms,
            });
            Transition::Fired { notify }
        }
        Some(false) => {
            t.pending_since = None;
            match t.row.as_mut().filter(|r| r.firing) {
                Some(row) => {
                    row.firing = false;
                    row.last_notified_ms = t.last_notified_ms;
                    Transition::Resolved {
                        notify: row.notified,
                        since_ms: row.since_ms,
                    }
                }
                None => Transition::None,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::model::StateKind;
    use super::*;

    const KEY: AlertKey = AlertKey {
        kind: StateKind::Rule,
        rule_id: 1,
        node_id: 1,
        task_id: 0,
    };
    const DUR: i64 = 300_000;

    fn go(t: &mut Tracked, c: Option<bool>, now: i64, cd: i64) -> Transition {
        step(t, KEY, c, Some(1.0), None, now, DUR, cd)
    }

    #[test]
    fn ok_pending_firing_ok() {
        let mut t = Tracked::default();
        assert_eq!(go(&mut t, Some(true), 0, 0), Transition::None);
        assert_eq!(go(&mut t, Some(true), 299_999, 0), Transition::None);
        assert_eq!(
            go(&mut t, Some(true), 300_000, 0),
            Transition::Fired { notify: true }
        );
        assert!(t.is_firing());
        assert_eq!(
            go(&mut t, Some(false), 310_000, 0),
            Transition::Resolved {
                notify: true,
                since_ms: 0
            }
        );
        assert!(!t.is_firing());
        assert!(t.row.is_some());
    }

    #[test]
    fn pending_recovers_silently() {
        let mut t = Tracked::default();
        go(&mut t, Some(true), 0, 0);
        assert_eq!(go(&mut t, Some(false), 100_000, 0), Transition::None);
        assert_eq!(go(&mut t, Some(true), 400_000, 0), Transition::None);
        assert_eq!(go(&mut t, Some(true), 699_999, 0), Transition::None);
        assert_eq!(
            go(&mut t, Some(true), 700_000, 0),
            Transition::Fired { notify: true }
        );
    }

    #[test]
    fn cooldown_silences_fire_and_resolve() {
        let cd = 1_800_000;
        let mut t = Tracked::default();
        go(&mut t, Some(true), 0, cd);
        assert_eq!(
            go(&mut t, Some(true), 300_000, cd),
            Transition::Fired { notify: true }
        );
        go(&mut t, Some(false), 400_000, cd);
        go(&mut t, Some(true), 600_000, cd);
        assert_eq!(
            go(&mut t, Some(true), 900_000, cd),
            Transition::Fired { notify: false }
        );
        assert!(matches!(
            go(&mut t, Some(false), 1_000_000, cd),
            Transition::Resolved { notify: false, .. }
        ));
        go(&mut t, Some(true), 1_800_000, cd);
        assert_eq!(
            go(&mut t, Some(true), 2_100_000, cd),
            Transition::Fired { notify: true }
        );
    }

    #[test]
    fn cooldown_zero_always_notifies() {
        let mut t = Tracked::default();
        for base in [0, 1_000_000] {
            go(&mut t, Some(true), base, 0);
            assert_eq!(
                go(&mut t, Some(true), base + DUR, 0),
                Transition::Fired { notify: true }
            );
            go(&mut t, Some(false), base + DUR + 1, 0);
        }
    }

    #[test]
    fn no_data_freezes() {
        let mut t = Tracked::default();
        go(&mut t, Some(true), 0, 0);
        assert_eq!(go(&mut t, None, 100_000, 0), Transition::None);
        assert_eq!(t.pending_since, None);
        go(&mut t, Some(true), 200_000, 0);
        go(&mut t, Some(true), 500_000, 0);
        assert!(t.is_firing());
        assert_eq!(go(&mut t, None, 600_000, 0), Transition::None);
        assert!(t.is_firing());
    }

    #[test]
    fn since_hint_starts_pending_earlier() {
        let mut t = Tracked::default();
        let r = step(
            &mut t,
            KEY,
            Some(true),
            Some(1.0),
            Some(-200_000),
            0,
            180_000,
            0,
        );
        assert_eq!(r, Transition::Fired { notify: true });
        assert_eq!(t.row.as_ref().map(|r| r.since_ms), Some(-200_000));
    }

    #[test]
    fn restored_row_resolves_with_notified_flag() {
        let row = StateRow {
            key: KEY,
            firing: true,
            since_ms: 5,
            fired_ms: Some(10),
            notified: false,
            value: None,
            last_notified_ms: Some(3),
        };
        let mut t = Tracked::restore(row);
        assert!(t.is_firing());
        assert_eq!(t.last_notified_ms, Some(3));
        assert_eq!(
            go(&mut t, Some(false), 100, 0),
            Transition::Resolved {
                notify: false,
                since_ms: 5
            }
        );
    }
}
