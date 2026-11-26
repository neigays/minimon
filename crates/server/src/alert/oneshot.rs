//! 一次性提醒：月流量档位与到期提醒（无恢复，靠 mark 去重）。

use jiff::Timestamp;

use super::engine::{Effects, MarkSet, Notice, Snapshot};
use super::message;
use super::model::{EventKind, Mark, MarkKind};
use crate::period::expire_days;

/// 流量档位：≥80%、≥100% 各提醒一次（按账期起点去重）；同轮越过两档只发 100%。
pub(super) fn traffic(marks: &mut MarkSet, snap: &Snapshot, fx: &mut Effects) {
    for node in &snap.nodes {
        let Some(u) = node.traffic.as_ref() else {
            continue;
        };
        let Some(quota) = u.quota.filter(|q| *q > 0) else {
            continue;
        };
        let pct = i128::from(u.used) * 100 / i128::from(quota);
        let mut notice_level = None;
        for level in LEVELS.into_iter().filter(|l| pct >= i128::from(*l)) {
            let mark = format!("{}:{level}", u.period_start_ms);
            if marks.insert((node.id, MarkKind::Traffic, mark.clone())) {
                fx.marks.push(Mark {
                    node_id: node.id,
                    kind: MarkKind::Traffic,
                    mark,
                });
                notice_level = Some(level);
            }
        }
        if let Some(level) = notice_level {
            fx.notices.push(Notice::for_node(
                node,
                EventKind::Traffic,
                crate::notify::tag::TRAFFIC,
                message::traffic(&node.name, level, u.used, quota),
            ));
        }
    }
}

/// 档位升序；循环中后写入的档位覆盖先写入的，故只发最高档。
const LEVELS: [i64; 2] = [80, 100];
const EXPIRE_DAYS: [i64; 3] = [7, 3, 1];
const EXPIRE_HOUR: i8 = 9;

/// 到期提醒：剩 7 / 3 / 1 天、全局时区 09:00 之后各一次；错过的档位不补发。
pub(super) fn expire(marks: &mut MarkSet, snap: &Snapshot, fx: &mut Effects) {
    let Ok(ts) = Timestamp::from_millisecond(snap.now_ms) else {
        return;
    };
    if snap.tz.to_datetime(ts).hour() < EXPIRE_HOUR {
        return;
    }
    for node in &snap.nodes {
        let Some(date) = node.expire_date.as_deref() else {
            continue;
        };
        let Some(days) = expire_days(date, &snap.tz, snap.now_ms) else {
            continue;
        };
        if !EXPIRE_DAYS.contains(&days) {
            continue;
        }
        let mark = format!("{date}:{days}");
        if marks.insert((node.id, MarkKind::Expire, mark.clone())) {
            fx.marks.push(Mark {
                node_id: node.id,
                kind: MarkKind::Expire,
                mark,
            });
            fx.notices.push(Notice::for_node(
                node,
                EventKind::Expire,
                crate::notify::tag::EXPIRE,
                message::expire(&node.name, days, date),
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::engine::tests::{engine, node, snap};
    use super::super::engine::{Effects, NodeSnap};
    use super::super::model::{EventKind, Mark, MarkKind};
    use crate::db::TrafficMode;
    use crate::traffic::TrafficUsage;
    use jiff::Timestamp;

    fn usage(used: i64, quota: Option<i64>, start: i64) -> TrafficUsage {
        TrafficUsage {
            quota,
            reset_day: 1,
            mode: TrafficMode::Sum,
            used,
            rx: used,
            tx: 0,
            period_start_ms: start,
            period_end_ms: start + 1_000_000,
        }
    }

    fn tnode(u: Option<TrafficUsage>) -> NodeSnap {
        let mut n = node(1, Some(1.0));
        n.traffic = u;
        n
    }

    fn mark(s: &str) -> Mark {
        Mark {
            node_id: 1,
            kind: MarkKind::Traffic,
            mark: s.into(),
        }
    }

    fn only_notice(fx: &Effects) -> &str {
        assert_eq!(fx.notices.len(), 1, "{fx:?}");
        &fx.notices[0].title
    }

    #[test]
    fn traffic_levels_once_per_period() {
        let mut e = engine(Vec::new());
        let fx = e.evaluate(&snap(0, vec![tnode(Some(usage(85, Some(100), 1000)))]));
        assert_eq!(only_notice(&fx), "【流量】节点1 已用 80%");
        assert_eq!(fx.notices[0].kind, EventKind::Traffic);
        assert_eq!(fx.marks, vec![mark("1000:80")]);

        let fx = e.evaluate(&snap(10_000, vec![tnode(Some(usage(90, Some(100), 1000)))]));
        assert!(fx.is_empty());

        let fx = e.evaluate(&snap(
            20_000,
            vec![tnode(Some(usage(100, Some(100), 1000)))],
        ));
        assert_eq!(only_notice(&fx), "【流量】节点1 已用 100%");
        assert_eq!(fx.marks, vec![mark("1000:100")]);
        assert!(
            e.evaluate(&snap(
                30_000,
                vec![tnode(Some(usage(110, Some(100), 1000)))]
            ))
            .is_empty()
        );

        // 新账期重新提醒
        let fx = e.evaluate(&snap(40_000, vec![tnode(Some(usage(85, Some(100), 2000)))]));
        assert_eq!(only_notice(&fx), "【流量】节点1 已用 80%");
        assert_eq!(fx.marks, vec![mark("2000:80")]);
    }

    #[test]
    fn traffic_jump_past_both_sends_only_100() {
        let mut e = engine(Vec::new());
        let fx = e.evaluate(&snap(0, vec![tnode(Some(usage(120, Some(100), 1000)))]));
        assert_eq!(only_notice(&fx), "【流量】节点1 已用 100%");
        let mut marks = fx.marks.clone();
        marks.sort_by(|a, b| a.mark.cmp(&b.mark));
        assert_eq!(marks, vec![mark("1000:100"), mark("1000:80")]);
        assert!(
            e.evaluate(&snap(
                10_000,
                vec![tnode(Some(usage(120, Some(100), 1000)))]
            ))
            .is_empty()
        );
    }

    #[test]
    fn traffic_without_quota_or_usage_silent() {
        let mut e = engine(Vec::new());
        for u in [
            None,
            Some(usage(1 << 40, None, 0)),
            Some(usage(1 << 40, Some(0), 0)),
        ] {
            assert!(e.evaluate(&snap(0, vec![tnode(u)])).is_empty());
        }
    }

    #[test]
    fn traffic_marks_restored_suppress() {
        let mut e = super::super::engine::Engine::new(
            Vec::new(),
            Default::default(),
            Vec::new(),
            vec![mark("1000:80")],
        );
        assert!(
            e.evaluate(&snap(0, vec![tnode(Some(usage(85, Some(100), 1000)))]))
                .is_empty()
        );
    }

    fn local_ms(s: &str) -> i64 {
        // 上海无夏令时，固定 +08:00
        format!("{s}+08:00")
            .parse::<Timestamp>()
            .unwrap()
            .as_millisecond()
    }

    fn enode(date: &str) -> NodeSnap {
        let mut n = node(1, Some(1.0));
        n.expire_date = Some(date.into());
        n
    }

    #[test]
    fn expire_stages_after_9am() {
        let mut e = engine(Vec::new());
        let early = local_ms("2026-11-26T08:59:59");
        let nine = local_ms("2026-11-26T09:00:00");
        assert!(
            e.evaluate(&snap(early, vec![enode("2026-11-29")]))
                .is_empty()
        );

        let fx = e.evaluate(&snap(nine, vec![enode("2026-11-29")]));
        assert_eq!(only_notice(&fx), "【到期】节点1 还剩 3 天");
        assert_eq!(fx.notices[0].kind, EventKind::Expire);
        assert_eq!(
            fx.marks,
            vec![Mark {
                node_id: 1,
                kind: MarkKind::Expire,
                mark: "2026-11-29:3".into()
            }]
        );
        assert!(
            e.evaluate(&snap(nine + 10_000, vec![enode("2026-11-29")]))
                .is_empty()
        );

        // 改到期日：新日期重新提醒
        let fx = e.evaluate(&snap(nine + 20_000, vec![enode("2026-11-27")]));
        assert_eq!(only_notice(&fx), "【到期】节点1 还剩 1 天");

        // 当天到期与非档位天数不提醒
        assert!(
            e.evaluate(&snap(nine, vec![enode("2026-11-26")]))
                .is_empty()
        );
        assert!(
            e.evaluate(&snap(nine, vec![enode("2026-11-28")]))
                .is_empty()
        );
        assert!(
            e.evaluate(&snap(nine, vec![enode("2026-11-20")]))
                .is_empty()
        );
    }

    #[test]
    fn disabled_settings_silence_oneshots() {
        let s = crate::alert::model::AlertSettings {
            traffic_enabled: false,
            expire_enabled: false,
            ..Default::default()
        };
        let mut e = super::super::engine::Engine::new(Vec::new(), s, Vec::new(), Vec::new());
        let mut n = enode("2026-11-29");
        n.traffic = Some(usage(120, Some(100), 0));
        assert!(
            e.evaluate(&snap(local_ms("2026-11-26T10:00:00"), vec![n]))
                .is_empty()
        );
    }
}
