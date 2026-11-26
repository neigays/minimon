//! 告警引擎：输入内存快照，输出状态变更、提醒标记与通知。纯逻辑，无 IO。

use std::collections::{HashMap, HashSet};

use minimon_common::protocol::Report;

use super::machine::{Tracked, Transition, step};
use super::message;
use super::metric::{ProbeSample, host_value, probe_value};
use super::model::{
    AlertKey, AlertRule, AlertSettings, EventKind, Mark, MarkKind, StateKind, StateRow,
};
use super::oneshot;
use crate::db::ProbeTaskRow;
use crate::period::Tz;
use crate::probe::tasks_for_node;
use crate::traffic::TrafficUsage;

#[derive(Debug, Clone)]
pub struct NodeSnap {
    pub id: i64,
    pub name: String,
    pub alert_offline: bool,
    pub last_seen_ms: Option<i64>,
    pub online: bool,
    pub report: Option<Report>,
    pub mem_total: u64,
    pub swap_total: u64,
    pub traffic: Option<TrafficUsage>,
    pub expire_date: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Snapshot {
    pub now_ms: i64,
    pub server_start_ms: i64,
    pub tz: Tz,
    pub nodes: Vec<NodeSnap>,
    pub tasks: Vec<ProbeTaskRow>,
    /// 键 `(node_id, task_id)`。
    pub probes: HashMap<(i64, i64), ProbeSample>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Notice {
    pub node_id: Option<i64>,
    pub node_name: Option<String>,
    pub kind: EventKind,
    pub title: String,
    pub body: String,
}

impl Notice {
    pub(super) fn for_node(
        node: &NodeSnap,
        kind: EventKind,
        (title, body): (String, String),
    ) -> Self {
        Notice {
            node_id: Some(node.id),
            node_name: Some(node.name.clone()),
            kind,
            title,
            body,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Effects {
    pub upserts: Vec<StateRow>,
    pub deletes: Vec<AlertKey>,
    pub marks: Vec<Mark>,
    pub notices: Vec<Notice>,
}

impl Effects {
    pub fn is_empty(&self) -> bool {
        self.upserts.is_empty()
            && self.deletes.is_empty()
            && self.marks.is_empty()
            && self.notices.is_empty()
    }
}

/// 已发送过的流量 / 到期提醒标记。
pub(super) type MarkSet = HashSet<(i64, MarkKind, String)>;

pub struct Engine {
    rules: Vec<AlertRule>,
    settings: AlertSettings,
    tracked: HashMap<AlertKey, Tracked>,
    marks: MarkSet,
}

impl Engine {
    pub fn new(
        rules: Vec<AlertRule>,
        settings: AlertSettings,
        states: Vec<StateRow>,
        marks: Vec<Mark>,
    ) -> Engine {
        Engine {
            rules,
            settings,
            tracked: states
                .into_iter()
                .map(|r| (r.key, Tracked::restore(r)))
                .collect(),
            marks: marks
                .into_iter()
                .map(|m| (m.node_id, m.kind, m.mark))
                .collect(),
        }
    }

    /// 替换规则；语义变化（删除、禁用、改条件 / 范围 / 绑定任务）的规则清除全部状态。
    /// 返回需从库删除的键。
    pub fn set_rules(&mut self, rules: Vec<AlertRule>) -> Vec<AlertKey> {
        let keep: HashSet<i64> = rules
            .iter()
            .filter(|n| n.enabled)
            .filter(|n| {
                self.rules
                    .iter()
                    .find(|o| o.id == n.id)
                    .is_none_or(|o| same_semantics(o, n))
            })
            .map(|n| n.id)
            .collect();
        self.rules = rules;
        self.drop_keys(|k| k.kind == StateKind::Rule && !keep.contains(&k.rule_id))
    }

    /// 替换设置；关闭离线告警时清除全部离线状态并返回其键。
    pub fn set_settings(&mut self, s: AlertSettings) -> Vec<AlertKey> {
        let offline_off = !s.offline_enabled;
        self.settings = s;
        if offline_off {
            self.drop_keys(|k| k.kind == StateKind::Offline)
        } else {
            Vec::new()
        }
    }

    /// 节点删除：丢弃其全部状态与提醒标记（库中由删除节点一并清理）。
    pub fn forget_node(&mut self, id: i64) {
        self.tracked.retain(|k, _| k.node_id != id);
        self.marks.retain(|(n, _, _)| *n != id);
    }

    /// 当前 firing 的状态。
    pub fn active(&self) -> Vec<StateRow> {
        let mut rows: Vec<StateRow> = self
            .tracked
            .values()
            .filter_map(|t| t.row.as_ref().filter(|r| r.firing))
            .cloned()
            .collect();
        rows.sort_by_key(|r| sort_key(&r.key));
        rows
    }

    pub fn evaluate(&mut self, snap: &Snapshot) -> Effects {
        let mut fx = Effects::default();
        self.eval_rules(snap, &mut fx);
        self.eval_offline(snap, &mut fx);
        if self.settings.traffic_enabled {
            oneshot::traffic(&mut self.marks, snap, &mut fx);
        }
        if self.settings.expire_enabled {
            oneshot::expire(&mut self.marks, snap, &mut fx);
        }
        fx
    }

    /// 移除满足条件的键；返回其中在库里有行的键。
    fn drop_keys(&mut self, pred: impl Fn(&AlertKey) -> bool) -> Vec<AlertKey> {
        let doomed: Vec<AlertKey> = self.tracked.keys().filter(|k| pred(k)).copied().collect();
        let mut out: Vec<AlertKey> = doomed
            .into_iter()
            .filter(|k| self.tracked.remove(k).is_some_and(|t| t.row.is_some()))
            .collect();
        out.sort_by_key(sort_key);
        out
    }

    fn eval_rules(&mut self, snap: &Snapshot, fx: &mut Effects) {
        for rule in self.rules.iter().filter(|r| r.enabled) {
            let nodes = snap
                .nodes
                .iter()
                .filter(|n| rule.all_nodes || rule.node_ids.contains(&n.id));
            for node in nodes {
                // 离线或尚无上报：冻结（不评估、不恢复）
                let report = node.report.as_ref().filter(|_| node.online);
                if rule.metric.is_probe() {
                    let tasks = tasks_for_node(&snap.tasks, node.id);
                    let tasks = tasks
                        .iter()
                        .filter(|t| rule.probe_task_id.is_none_or(|id| id == t.id));
                    for t in tasks {
                        let value = report.and(snap.probes.get(&(node.id, t.id))).and_then(|s| {
                            probe_value(rule.metric, s, i64::from(t.interval_secs), snap.now_ms)
                        });
                        step_rule(&mut self.tracked, rule, node, t.id, value, snap, fx);
                    }
                } else {
                    let value = report
                        .and_then(|r| host_value(rule.metric, r, node.mem_total, node.swap_total));
                    step_rule(&mut self.tracked, rule, node, 0, value, snap, fx);
                }
            }
        }
    }

    fn eval_offline(&mut self, snap: &Snapshot, fx: &mut Effects) {
        for node in &snap.nodes {
            let key = AlertKey {
                kind: StateKind::Offline,
                rule_id: 0,
                node_id: node.id,
                task_id: 0,
            };
            let applies = self.settings.offline_enabled && node.alert_offline;
            let Some(last_seen) = node.last_seen_ms.filter(|_| applies) else {
                if self.tracked.remove(&key).is_some_and(|t| t.row.is_some()) {
                    fx.deletes.push(key);
                }
                continue;
            };
            if node.online && !self.tracked.contains_key(&key) {
                continue;
            }
            let t = self.tracked.entry(key).or_default();
            let tr = step(
                t,
                key,
                Some(!node.online),
                None,
                Some(last_seen.max(snap.server_start_ms)),
                snap.now_ms,
                self.settings.offline_grace_secs.saturating_mul(1000),
                0,
            );
            let Some(row) = t.row.as_ref().filter(|_| tr != Transition::None) else {
                continue;
            };
            fx.upserts.push(row.clone());
            match tr {
                Transition::Fired { notify: true } => fx.notices.push(Notice::for_node(
                    node,
                    EventKind::Firing,
                    message::offline(&node.name, row.since_ms, &snap.tz),
                )),
                Transition::Resolved {
                    notify: true,
                    since_ms,
                } => {
                    let from = row.fired_ms.unwrap_or(since_ms);
                    fx.notices.push(Notice::for_node(
                        node,
                        EventKind::Resolved,
                        message::online(&node.name, snap.now_ms - from),
                    ));
                }
                _ => {}
            }
        }
    }
}

fn sort_key(k: &AlertKey) -> (bool, i64, i64, i64) {
    (
        k.kind == StateKind::Offline,
        k.rule_id,
        k.node_id,
        k.task_id,
    )
}

/// 只改名称或冷却时间不算语义变化。
fn same_semantics(a: &AlertRule, b: &AlertRule) -> bool {
    a.metric == b.metric
        && a.op == b.op
        && a.threshold == b.threshold
        && a.duration_secs == b.duration_secs
        && a.all_nodes == b.all_nodes
        && a.node_ids == b.node_ids
        && a.probe_task_id == b.probe_task_id
}

fn step_rule(
    tracked: &mut HashMap<AlertKey, Tracked>,
    rule: &AlertRule,
    node: &NodeSnap,
    task_id: i64,
    value: Option<f64>,
    snap: &Snapshot,
    fx: &mut Effects,
) {
    let key = AlertKey {
        kind: StateKind::Rule,
        rule_id: rule.id,
        node_id: node.id,
        task_id,
    };
    let cond = value.map(|v| rule.op.holds(v, rule.threshold));
    if cond != Some(true) && !tracked.contains_key(&key) {
        return;
    }
    let t = tracked.entry(key).or_default();
    let tr = step(
        t,
        key,
        cond,
        value,
        None,
        snap.now_ms,
        rule.duration_secs.saturating_mul(1000),
        rule.cooldown_secs.saturating_mul(1000),
    );
    let Some(row) = t.row.as_ref().filter(|_| tr != Transition::None) else {
        return;
    };
    fx.upserts.push(row.clone());
    let task = (task_id != 0)
        .then(|| snap.tasks.iter().find(|t| t.id == task_id))
        .flatten()
        .map(|t| t.name.as_str());
    match tr {
        Transition::Fired { notify: true } => fx.notices.push(Notice::for_node(
            node,
            EventKind::Firing,
            message::firing(
                rule,
                &node.name,
                task,
                row.value.unwrap_or_default(),
                row.since_ms,
                &snap.tz,
            ),
        )),
        Transition::Resolved {
            notify: true,
            since_ms,
        } => fx.notices.push(Notice::for_node(
            node,
            EventKind::Resolved,
            message::resolved(rule, &node.name, task, value, snap.now_ms - since_ms),
        )),
        _ => {}
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::super::model::{Metric, Op};
    use super::*;
    use minimon_common::protocol::ProbeKind;

    pub(in crate::alert) fn sh() -> Tz {
        crate::period::parse_tz("Asia/Shanghai").unwrap()
    }

    pub(in crate::alert) fn report(cpu: f32) -> Report {
        Report {
            ts_ms: 0,
            cpu,
            mem_used: 0,
            swap_used: 0,
            disk_used: 0,
            disk_total: 100,
            net_rx_bytes: 0,
            net_tx_bytes: 0,
            net_rx_rate: 0,
            net_tx_rate: 0,
            load1: 0.0,
            load5: 0.0,
            load15: 0.0,
            uptime_secs: 0,
            process_count: 0,
            tcp_count: 0,
            udp_count: 0,
        }
    }

    /// `cpu = None` 表示离线且无上报。
    pub(in crate::alert) fn node(id: i64, cpu: Option<f32>) -> NodeSnap {
        NodeSnap {
            id,
            name: format!("节点{id}"),
            alert_offline: true,
            last_seen_ms: Some(0),
            online: cpu.is_some(),
            report: cpu.map(report),
            mem_total: 1000,
            swap_total: 100,
            traffic: None,
            expire_date: None,
        }
    }

    pub(in crate::alert) fn snap(now_ms: i64, nodes: Vec<NodeSnap>) -> Snapshot {
        Snapshot {
            now_ms,
            server_start_ms: 0,
            tz: sh(),
            nodes,
            tasks: Vec::new(),
            probes: HashMap::new(),
        }
    }

    fn rule(id: i64, metric: Metric, threshold: f64) -> AlertRule {
        AlertRule {
            id,
            name: format!("规则{id}"),
            enabled: true,
            metric,
            op: Op::Gt,
            threshold,
            duration_secs: 10,
            cooldown_secs: 0,
            all_nodes: true,
            node_ids: Vec::new(),
            probe_task_id: None,
        }
    }

    fn task(id: i64) -> ProbeTaskRow {
        ProbeTaskRow {
            id,
            name: format!("任务{id}"),
            kind: ProbeKind::Tcp,
            target: "1.1.1.1:443".into(),
            interval_secs: 60,
            samples: 1,
            timeout_ms: 1000,
            all_nodes: true,
            node_ids: Vec::new(),
            show_on_card: false,
            sort: 0,
        }
    }

    pub(in crate::alert) fn engine(rules: Vec<AlertRule>) -> Engine {
        Engine::new(rules, AlertSettings::default(), Vec::new(), Vec::new())
    }

    fn rule_key(rule_id: i64, node_id: i64, task_id: i64) -> AlertKey {
        AlertKey {
            kind: StateKind::Rule,
            rule_id,
            node_id,
            task_id,
        }
    }

    fn offline_key(node_id: i64) -> AlertKey {
        AlertKey {
            kind: StateKind::Offline,
            rule_id: 0,
            node_id,
            task_id: 0,
        }
    }

    fn fire_cpu(e: &mut Engine) {
        e.evaluate(&snap(0, vec![node(1, Some(95.0))]));
        let fx = e.evaluate(&snap(10_000, vec![node(1, Some(95.0))]));
        assert_eq!(fx.notices.len(), 1);
    }

    #[test]
    fn cpu_rule_fires_and_resolves() {
        let mut e = engine(vec![rule(1, Metric::Cpu, 90.0)]);
        let fx = e.evaluate(&snap(0, vec![node(1, Some(95.0))]));
        assert!(fx.notices.is_empty());
        assert!(fx.upserts.is_empty());

        let fx = e.evaluate(&snap(10_000, vec![node(1, Some(95.0))]));
        assert_eq!(fx.notices.len(), 1);
        let n = &fx.notices[0];
        assert_eq!(n.kind, EventKind::Firing);
        assert!(n.title.starts_with("【告警】"), "{}", n.title);
        assert!(n.title.contains("节点1"));
        assert_eq!(n.node_id, Some(1));
        assert_eq!(fx.upserts.len(), 1);
        assert!(fx.upserts[0].firing);
        assert_eq!(fx.upserts[0].key, rule_key(1, 1, 0));
        assert_eq!(fx.upserts[0].last_notified_ms, Some(10_000));
        assert_eq!(e.active().len(), 1);

        // firing 期间不重复提醒
        let fx = e.evaluate(&snap(15_000, vec![node(1, Some(96.0))]));
        assert!(fx.notices.is_empty());

        let fx = e.evaluate(&snap(20_000, vec![node(1, Some(50.0))]));
        assert_eq!(fx.notices.len(), 1);
        assert_eq!(fx.notices[0].kind, EventKind::Resolved);
        assert!(fx.notices[0].title.starts_with("【恢复】"));
        assert_eq!(fx.upserts.len(), 1);
        assert!(!fx.upserts[0].firing);
        assert_eq!(fx.upserts[0].last_notified_ms, Some(10_000));
        assert!(e.active().is_empty());
    }

    #[test]
    fn offline_node_freezes_host_rules() {
        let mut e = engine(vec![rule(1, Metric::Cpu, 90.0)]);
        fire_cpu(&mut e);
        let mut n = node(1, Some(50.0));
        n.online = false;
        let fx = e.evaluate(&snap(20_000, vec![n]));
        assert!(fx.notices.is_empty());
        assert!(fx.upserts.is_empty());
        assert_eq!(e.active().len(), 1);
    }

    #[test]
    fn offline_grace_and_recovery() {
        let mut e = engine(Vec::new());
        let fx = e.evaluate(&snap(179_999, vec![node(1, None)]));
        assert!(fx.notices.is_empty());

        let fx = e.evaluate(&snap(180_000, vec![node(1, None)]));
        assert_eq!(fx.notices.len(), 1);
        assert_eq!(fx.notices[0].kind, EventKind::Firing);
        assert_eq!(fx.notices[0].title, "【离线】节点1");
        assert_eq!(fx.upserts.len(), 1);
        assert_eq!(fx.upserts[0].key, offline_key(1));
        assert_eq!(fx.upserts[0].since_ms, 0);

        let fx = e.evaluate(&snap(300_000, vec![node(1, Some(1.0))]));
        assert_eq!(fx.notices.len(), 1);
        let n = &fx.notices[0];
        assert_eq!(n.kind, EventKind::Resolved);
        assert_eq!(n.title, "【上线】节点1");
        assert!(n.body.contains("2 分钟"), "{}", n.body);
        assert!(e.active().is_empty());
    }

    #[test]
    fn offline_after_server_restart_uses_start_time() {
        let mut e = engine(Vec::new());
        let mut s = snap(1_100_000, vec![node(1, None)]);
        s.server_start_ms = 1_000_000;
        assert!(e.evaluate(&s).notices.is_empty());
        s.now_ms = 1_180_000;
        assert_eq!(e.evaluate(&s).notices.len(), 1);
    }

    #[test]
    fn offline_disabled_per_node_clears_silently() {
        let mut e = engine(Vec::new());
        e.evaluate(&snap(0, vec![node(1, None)]));
        assert_eq!(
            e.evaluate(&snap(180_000, vec![node(1, None)]))
                .notices
                .len(),
            1
        );
        let mut n = node(1, None);
        n.alert_offline = false;
        let fx = e.evaluate(&snap(190_000, vec![n.clone()]));
        assert!(fx.notices.is_empty());
        assert_eq!(fx.deletes, vec![offline_key(1)]);
        assert!(e.active().is_empty());
        n.online = true;
        assert!(e.evaluate(&snap(200_000, vec![n])).is_empty());
    }

    #[test]
    fn never_seen_node_no_offline() {
        let mut e = engine(Vec::new());
        let mut n = node(1, None);
        n.last_seen_ms = None;
        for now in [0, 180_000, 1_000_000] {
            assert!(e.evaluate(&snap(now, vec![n.clone()])).is_empty());
        }
        assert!(e.active().is_empty());
    }

    fn restored_offline() -> Engine {
        let row = StateRow {
            key: offline_key(1),
            firing: true,
            since_ms: 0,
            fired_ms: Some(180_000),
            notified: true,
            value: None,
            last_notified_ms: Some(180_000),
        };
        Engine::new(Vec::new(), AlertSettings::default(), vec![row], Vec::new())
    }

    #[test]
    fn restored_firing_still_offline_no_duplicate() {
        let mut e = restored_offline();
        assert_eq!(e.active().len(), 1);
        let fx = e.evaluate(&snap(500_000, vec![node(1, None)]));
        assert!(fx.notices.is_empty());
        assert!(fx.upserts.is_empty());
        assert_eq!(e.active().len(), 1);
    }

    #[test]
    fn restored_firing_recovered_sends_resolved() {
        let mut e = restored_offline();
        let fx = e.evaluate(&snap(500_000, vec![node(1, Some(1.0))]));
        assert_eq!(fx.notices.len(), 1);
        assert_eq!(fx.notices[0].title, "【上线】节点1");
        assert_eq!(fx.notices[0].kind, EventKind::Resolved);
    }

    fn probe_snap(now: i64) -> Snapshot {
        let mut s = snap(now, vec![node(1, Some(1.0))]);
        s.tasks = vec![task(1), task(2)];
        for t in [1, 2] {
            s.probes.insert(
                (1, t),
                ProbeSample {
                    ts_ms: now,
                    avg_ms: Some(200.0),
                    loss: Some(0.0),
                },
            );
        }
        s
    }

    fn keys(fx: &Effects) -> Vec<AlertKey> {
        let mut k: Vec<AlertKey> = fx.upserts.iter().map(|r| r.key).collect();
        k.sort_by_key(|k| (k.rule_id, k.node_id, k.task_id));
        k
    }

    #[test]
    fn probe_rule_per_task_and_bound() {
        let mut bound = rule(2, Metric::ProbeLatency, 100.0);
        bound.probe_task_id = Some(2);
        let mut e = engine(vec![rule(1, Metric::ProbeLatency, 100.0), bound]);
        e.evaluate(&probe_snap(0));
        let fx = e.evaluate(&probe_snap(10_000));
        assert_eq!(
            keys(&fx),
            vec![rule_key(1, 1, 1), rule_key(1, 1, 2), rule_key(2, 1, 2)]
        );
        assert_eq!(fx.notices.len(), 3);
        assert!(fx.notices.iter().all(|n| n.body.contains("任务")));
    }

    #[test]
    fn stale_probe_is_no_data() {
        let mut e = engine(vec![rule(1, Metric::ProbeLatency, 100.0)]);
        e.evaluate(&probe_snap(0));
        let mut s = probe_snap(10_000);
        for p in s.probes.values_mut() {
            p.ts_ms = 10_000 - 180_001;
        }
        assert!(e.evaluate(&s).is_empty());
    }

    #[test]
    fn deleted_bound_task_yields_no_keys() {
        let mut r = rule(1, Metric::ProbeLatency, 100.0);
        r.probe_task_id = Some(9);
        let mut e = engine(vec![r.clone()]);
        for now in [0, 10_000, 20_000] {
            assert!(e.evaluate(&probe_snap(now)).is_empty());
        }

        // 任务存在时触发，任务被删后不再产生变化；改绑后旧键被删除
        let mut s = probe_snap(0);
        s.tasks.push(task(9));
        s.probes.insert(
            (1, 9),
            ProbeSample {
                ts_ms: 0,
                avg_ms: Some(500.0),
                loss: Some(0.0),
            },
        );
        e.evaluate(&s);
        s.now_ms = 10_000;
        assert_eq!(e.evaluate(&s).notices.len(), 1);
        let fx = e.evaluate(&probe_snap(20_000));
        assert!(fx.upserts.is_empty() && fx.notices.is_empty());
        r.probe_task_id = None;
        assert_eq!(e.set_rules(vec![r]), vec![rule_key(1, 1, 9)]);
        assert!(e.active().is_empty());
    }

    #[test]
    fn rename_keeps_state_threshold_change_clears() {
        let mut r = rule(1, Metric::Cpu, 90.0);
        let mut e = engine(vec![r.clone()]);
        fire_cpu(&mut e);
        r.name = "改名".into();
        r.cooldown_secs = 600;
        assert!(e.set_rules(vec![r.clone()]).is_empty());
        assert_eq!(e.active().len(), 1);

        r.threshold = 80.0;
        assert_eq!(e.set_rules(vec![r.clone()]), vec![rule_key(1, 1, 0)]);
        assert!(e.active().is_empty());
        // 重新从 pending 开始计时
        assert!(
            e.evaluate(&snap(20_000, vec![node(1, Some(95.0))]))
                .notices
                .is_empty()
        );

        // 禁用与删除同样清除
        let fx = e.evaluate(&snap(30_000, vec![node(1, Some(95.0))]));
        assert_eq!(fx.notices.len(), 1);
        r.enabled = false;
        assert_eq!(e.set_rules(vec![r]), vec![rule_key(1, 1, 0)]);
        assert!(e.set_rules(Vec::new()).is_empty());
    }

    #[test]
    fn set_settings_offline_off_clears_offline_keys() {
        let mut e = engine(Vec::new());
        e.evaluate(&snap(180_000, vec![node(1, None), node(2, None)]));
        assert_eq!(e.active().len(), 2);
        let s = AlertSettings {
            offline_enabled: false,
            ..AlertSettings::default()
        };
        let mut del = e.set_settings(s);
        del.sort_by_key(|k| k.node_id);
        assert_eq!(del, vec![offline_key(1), offline_key(2)]);
        assert!(e.active().is_empty());
        assert!(e.evaluate(&snap(200_000, vec![node(1, None)])).is_empty());
    }

    #[test]
    fn forget_node_drops_tracking() {
        let mut e = engine(vec![rule(1, Metric::Cpu, 90.0)]);
        fire_cpu(&mut e);
        e.forget_node(1);
        assert!(e.active().is_empty());
        // 节点缺失的键不再评估
        assert!(e.evaluate(&snap(20_000, Vec::new())).is_empty());
    }
}
