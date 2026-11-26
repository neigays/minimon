//! 告警存储：规则、状态、去重标记、事件与设置。
//!
//! 表之间没有外键约束生效，级联删除在这里用代码完成。

use super::{Db, DbError};
use crate::alert::model::*;
use rusqlite::{Connection, params};

const DAY_MS: i64 = 86_400_000;
const EVENT_KEEP_MS: i64 = 90 * DAY_MS;
const MARK_KEEP_MS: i64 = 400 * DAY_MS;

const RULE_COLS: &str = "id, name, enabled, metric, op, threshold, duration_secs, \
     cooldown_secs, all_nodes, probe_task_id";

fn bad<T: std::fmt::Display>(idx: usize, what: &str, v: T) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        idx,
        rusqlite::types::Type::Text,
        format!("未知{what} {v}").into(),
    )
}

fn load_rules(c: &Connection, only: Option<i64>) -> Result<Vec<AlertRule>, DbError> {
    let sql = format!(
        "SELECT {RULE_COLS} FROM alert_rules {} ORDER BY id",
        if only.is_some() { "WHERE id = ?1" } else { "" }
    );
    let mut stmt = c.prepare(&sql)?;
    let map = |r: &rusqlite::Row<'_>| -> rusqlite::Result<AlertRule> {
        let metric: String = r.get(3)?;
        let op: String = r.get(4)?;
        Ok(AlertRule {
            id: r.get(0)?,
            name: r.get(1)?,
            enabled: r.get::<_, i64>(2)? != 0,
            metric: Metric::parse(&metric).ok_or_else(|| bad(3, "指标", &metric))?,
            op: Op::parse(&op).ok_or_else(|| bad(4, "比较符", &op))?,
            threshold: r.get(5)?,
            duration_secs: r.get(6)?,
            cooldown_secs: r.get(7)?,
            all_nodes: r.get::<_, i64>(8)? != 0,
            node_ids: Vec::new(),
            probe_task_id: r.get(9)?,
        })
    };
    let mut rows: Vec<AlertRule> = match only {
        Some(id) => stmt.query_map([id], map)?.collect::<Result<_, _>>()?,
        None => stmt.query_map([], map)?.collect::<Result<_, _>>()?,
    };
    let mut links = c.prepare("SELECT rule_id, node_id FROM alert_rule_nodes ORDER BY node_id")?;
    let pairs: Vec<(i64, i64)> = links
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_, _>>()?;
    for (rule, n) in pairs {
        if let Some(row) = rows.iter_mut().find(|row| row.id == rule) {
            row.node_ids.push(n);
        }
    }
    Ok(rows)
}

fn replace_links(c: &Connection, rule_id: i64, node_ids: &[i64]) -> Result<(), DbError> {
    c.execute("DELETE FROM alert_rule_nodes WHERE rule_id = ?1", [rule_id])?;
    let mut ins =
        c.prepare("INSERT OR IGNORE INTO alert_rule_nodes(rule_id, node_id) VALUES (?1, ?2)")?;
    for n in node_ids {
        ins.execute(params![rule_id, n])?;
    }
    Ok(())
}

fn row_to_event(r: &rusqlite::Row<'_>) -> rusqlite::Result<AlertEvent> {
    let kind: String = r.get(4)?;
    let sent: String = r.get(7)?;
    Ok(AlertEvent {
        id: r.get(0)?,
        ts_ms: r.get(1)?,
        node_id: r.get(2)?,
        node_name: r.get(3)?,
        kind: EventKind::parse(&kind).ok_or_else(|| bad(4, "事件类型", &kind))?,
        title: r.get(5)?,
        body: r.get(6)?,
        sent: SendStatus::parse(&sent).ok_or_else(|| bad(7, "发送状态", &sent))?,
    })
}

fn get_bool(db: &Db, key: &str, default: bool) -> Result<bool, DbError> {
    Ok(db
        .get_setting(key)?
        .map_or(default, |v| v == "1" || v == "true"))
}

impl Db {
    pub fn list_alert_rules(&self) -> Result<Vec<AlertRule>, DbError> {
        self.read(|c| load_rules(c, None))
    }

    pub fn create_alert_rule(&self, r: &NewAlertRule) -> Result<AlertRule, DbError> {
        self.write(|c| {
            let tx = c.transaction()?;
            tx.execute(
                "INSERT INTO alert_rules (name, enabled, metric, op, threshold, duration_secs, \
                 cooldown_secs, all_nodes, probe_task_id) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    r.name,
                    i64::from(r.enabled),
                    r.metric.as_str(),
                    r.op.as_str(),
                    r.threshold,
                    r.duration_secs,
                    r.cooldown_secs,
                    i64::from(r.all_nodes),
                    r.probe_task_id
                ],
            )?;
            let id = tx.last_insert_rowid();
            replace_links(&tx, id, &r.node_ids)?;
            let mut v = load_rules(&tx, Some(id))?;
            tx.commit()?;
            v.pop()
                .ok_or_else(|| DbError::Other("创建告警规则后未读到记录".into()))
        })
    }

    /// 部分更新；规则不存在返回 None。
    pub fn update_alert_rule(
        &self,
        id: i64,
        p: &AlertRulePatch,
    ) -> Result<Option<AlertRule>, DbError> {
        self.write(|c| {
            let tx = c.transaction()?;
            let n = tx.execute(
                "UPDATE alert_rules SET name = coalesce(?1, name), enabled = coalesce(?2, enabled), \
                 metric = coalesce(?3, metric), op = coalesce(?4, op), \
                 threshold = coalesce(?5, threshold), duration_secs = coalesce(?6, duration_secs), \
                 cooldown_secs = coalesce(?7, cooldown_secs), all_nodes = coalesce(?8, all_nodes), \
                 probe_task_id = CASE WHEN ?9 THEN ?10 ELSE probe_task_id END WHERE id = ?11",
                params![
                    p.name,
                    p.enabled.map(i64::from),
                    p.metric.map(Metric::as_str),
                    p.op.map(Op::as_str),
                    p.threshold,
                    p.duration_secs,
                    p.cooldown_secs,
                    p.all_nodes.map(i64::from),
                    p.probe_task_id.is_some(),
                    p.probe_task_id.flatten(),
                    id
                ],
            )?;
            if n == 0 {
                return Ok(None);
            }
            if let Some(ids) = &p.node_ids {
                replace_links(&tx, id, ids)?;
            }
            let mut v = load_rules(&tx, Some(id))?;
            tx.commit()?;
            Ok(v.pop())
        })
    }

    pub fn delete_alert_rule(&self, id: i64) -> Result<bool, DbError> {
        self.write(|c| {
            let tx = c.transaction()?;
            tx.execute("DELETE FROM alert_rule_nodes WHERE rule_id = ?1", [id])?;
            tx.execute(
                "DELETE FROM alert_state WHERE kind = 'rule' AND rule_id = ?1",
                [id],
            )?;
            let n = tx.execute("DELETE FROM alert_rules WHERE id = ?1", [id])?;
            tx.commit()?;
            Ok(n > 0)
        })
    }

    pub fn load_alert_states(&self) -> Result<Vec<StateRow>, DbError> {
        self.read(|c| {
            let mut stmt = c.prepare(
                "SELECT kind, rule_id, node_id, task_id, status, since_ms, fired_ms, notified, \
                 value, last_notified_ms FROM alert_state",
            )?;
            let rows = stmt.query_map([], |r| {
                let kind: String = r.get(0)?;
                let status: String = r.get(4)?;
                Ok(StateRow {
                    key: AlertKey {
                        kind: StateKind::parse(&kind).ok_or_else(|| bad(0, "状态类型", &kind))?,
                        rule_id: r.get(1)?,
                        node_id: r.get(2)?,
                        task_id: r.get(3)?,
                    },
                    firing: status == "firing",
                    since_ms: r.get(5)?,
                    fired_ms: r.get(6)?,
                    notified: r.get::<_, i64>(7)? != 0,
                    value: r.get(8)?,
                    last_notified_ms: r.get(9)?,
                })
            })?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
    }

    /// 单事务应用状态机副作用：状态 upsert、状态删除、去重标记。
    pub fn apply_alert_effects(
        &self,
        upserts: &[StateRow],
        deletes: &[AlertKey],
        marks: &[Mark],
    ) -> Result<(), DbError> {
        let now = super::now_ms();
        self.write(|c| {
            let tx = c.transaction()?;
            {
                let mut up = tx.prepare(
                    "INSERT OR REPLACE INTO alert_state (kind, rule_id, node_id, task_id, status, \
                     since_ms, fired_ms, notified, value, last_notified_ms) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                )?;
                for s in upserts {
                    up.execute(params![
                        s.key.kind.as_str(),
                        s.key.rule_id,
                        s.key.node_id,
                        s.key.task_id,
                        if s.firing { "firing" } else { "ok" },
                        s.since_ms,
                        s.fired_ms,
                        i64::from(s.notified),
                        s.value,
                        s.last_notified_ms
                    ])?;
                }
                let mut del = tx.prepare(
                    "DELETE FROM alert_state WHERE kind = ?1 AND rule_id = ?2 \
                     AND node_id = ?3 AND task_id = ?4",
                )?;
                for k in deletes {
                    del.execute(params![k.kind.as_str(), k.rule_id, k.node_id, k.task_id])?;
                }
                let mut mk = tx.prepare(
                    "INSERT OR IGNORE INTO alert_marks (node_id, kind, mark, created_ms) \
                     VALUES (?1, ?2, ?3, ?4)",
                )?;
                for m in marks {
                    mk.execute(params![m.node_id, m.kind.as_str(), m.mark, now])?;
                }
            }
            tx.commit()?;
            Ok(())
        })
    }

    pub fn load_alert_marks(&self) -> Result<Vec<Mark>, DbError> {
        self.read(|c| {
            let mut stmt = c.prepare("SELECT node_id, kind, mark FROM alert_marks")?;
            let rows = stmt.query_map([], |r| {
                let kind: String = r.get(1)?;
                Ok(Mark {
                    node_id: r.get(0)?,
                    kind: MarkKind::parse(&kind).ok_or_else(|| bad(1, "标记类型", &kind))?,
                    mark: r.get(2)?,
                })
            })?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
    }

    /// 单事务插入事件，返回按输入顺序的新 id。
    pub fn insert_alert_events(&self, events: &[NewEvent]) -> Result<Vec<i64>, DbError> {
        self.write(|c| {
            let tx = c.transaction()?;
            let mut ids = Vec::with_capacity(events.len());
            {
                let mut ins = tx.prepare(
                    "INSERT INTO alert_events (ts_ms, node_id, node_name, kind, title, body, sent) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                )?;
                for e in events {
                    ins.execute(params![
                        e.ts_ms,
                        e.node_id,
                        e.node_name,
                        e.kind.as_str(),
                        e.title,
                        e.body,
                        e.sent.as_str()
                    ])?;
                    ids.push(tx.last_insert_rowid());
                }
            }
            tx.commit()?;
            Ok(ids)
        })
    }

    pub fn set_events_sent(&self, ids: &[i64], sent: SendStatus) -> Result<(), DbError> {
        self.write(|c| {
            let tx = c.transaction()?;
            {
                let mut up = tx.prepare("UPDATE alert_events SET sent = ?1 WHERE id = ?2")?;
                for id in ids {
                    up.execute(params![sent.as_str(), id])?;
                }
            }
            tx.commit()?;
            Ok(())
        })
    }

    /// id 倒序分页；`before` 为上一页最后一条的 id。
    pub fn list_alert_events(
        &self,
        before: Option<i64>,
        limit: i64,
    ) -> Result<Vec<AlertEvent>, DbError> {
        self.read(|c| {
            let mut stmt = c.prepare(
                "SELECT id, ts_ms, node_id, node_name, kind, title, body, sent FROM alert_events \
                 WHERE id < coalesce(?1, 9223372036854775807) ORDER BY id DESC LIMIT ?2",
            )?;
            let rows = stmt.query_map(params![before, limit], row_to_event)?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
    }

    /// 清理过期事件（90 天）与标记（400 天），返回删除行数。
    pub fn prune_alerts(&self, now_ms: i64) -> Result<usize, DbError> {
        self.write(|c| {
            let tx = c.transaction()?;
            let a = tx.execute(
                "DELETE FROM alert_events WHERE ts_ms < ?1",
                [now_ms - EVENT_KEEP_MS],
            )?;
            let b = tx.execute(
                "DELETE FROM alert_marks WHERE created_ms < ?1",
                [now_ms - MARK_KEEP_MS],
            )?;
            tx.commit()?;
            Ok(a + b)
        })
    }

    pub fn alert_settings(&self) -> Result<AlertSettings, DbError> {
        let d = AlertSettings::default();
        let grace = match self.get_setting("alert.offline_grace_secs")? {
            Some(v) => v.parse().unwrap_or(d.offline_grace_secs),
            None => d.offline_grace_secs,
        };
        Ok(AlertSettings {
            offline_enabled: get_bool(self, "alert.offline_enabled", d.offline_enabled)?,
            offline_grace_secs: grace,
            traffic_enabled: get_bool(self, "alert.traffic_enabled", d.traffic_enabled)?,
            expire_enabled: get_bool(self, "alert.expire_enabled", d.expire_enabled)?,
        })
    }

    pub fn set_alert_settings(&self, s: &AlertSettings) -> Result<(), DbError> {
        let b = |v: bool| if v { "1" } else { "0" };
        self.set_setting("alert.offline_enabled", b(s.offline_enabled))?;
        self.set_setting(
            "alert.offline_grace_secs",
            &s.offline_grace_secs.to_string(),
        )?;
        self.set_setting("alert.traffic_enabled", b(s.traffic_enabled))?;
        self.set_setting("alert.expire_enabled", b(s.expire_enabled))
    }

    pub fn sendkey(&self) -> Result<Option<String>, DbError> {
        Ok(self.get_setting("alert.sendkey")?.filter(|k| !k.is_empty()))
    }

    pub fn set_sendkey(&self, key: Option<&str>) -> Result<(), DbError> {
        match key {
            Some(k) => self.set_setting("alert.sendkey", k),
            None => self.write(|c| {
                c.execute("DELETE FROM settings WHERE key = 'alert.sendkey'", [])?;
                Ok(())
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn new_rule(node_ids: Vec<i64>) -> NewAlertRule {
        NewAlertRule {
            name: "cpu高".into(),
            enabled: true,
            metric: Metric::Cpu,
            op: Op::Gt,
            threshold: 90.0,
            duration_secs: 300,
            cooldown_secs: 1800,
            all_nodes: false,
            node_ids,
            probe_task_id: None,
        }
    }

    fn state(rule_id: i64) -> StateRow {
        StateRow {
            key: AlertKey {
                kind: StateKind::Rule,
                rule_id,
                node_id: 1,
                task_id: 0,
            },
            firing: true,
            since_ms: 10,
            fired_ms: Some(20),
            notified: true,
            value: Some(95.5),
            last_notified_ms: Some(30),
        }
    }

    fn ev(ts: i64) -> NewEvent {
        NewEvent {
            ts_ms: ts,
            node_id: Some(1),
            node_name: Some("n".into()),
            kind: EventKind::Firing,
            title: "t".into(),
            body: "b".into(),
            sent: SendStatus::Pending,
        }
    }

    #[test]
    fn rule_crud_roundtrip() {
        let db = Db::open_in_memory().unwrap();
        let r = db.create_alert_rule(&new_rule(vec![2, 1])).unwrap();
        assert_eq!(db.list_alert_rules().unwrap(), vec![r.clone()]);
        assert_eq!(r.node_ids, [1, 2]);
        let p = AlertRulePatch {
            threshold: Some(95.0),
            node_ids: Some(vec![3]),
            ..Default::default()
        };
        let u = db.update_alert_rule(r.id, &p).unwrap().unwrap();
        assert_eq!((u.threshold, u.node_ids.clone()), (95.0, vec![3]));
        assert_eq!(u.name, "cpu高");
        assert!(db.update_alert_rule(999, &p).unwrap().is_none());
        assert!(db.delete_alert_rule(r.id).unwrap());
        assert!(!db.delete_alert_rule(r.id).unwrap());
        assert!(db.list_alert_rules().unwrap().is_empty());
    }

    #[test]
    fn delete_rule_drops_its_states() {
        let db = Db::open_in_memory().unwrap();
        let r = db.create_alert_rule(&new_rule(vec![1])).unwrap();
        db.apply_alert_effects(&[state(r.id)], &[], &[]).unwrap();
        assert_eq!(db.load_alert_states().unwrap().len(), 1);
        db.delete_alert_rule(r.id).unwrap();
        assert!(db.load_alert_states().unwrap().is_empty());
    }

    #[test]
    fn effects_are_atomic_upsert_delete_mark() {
        let db = Db::open_in_memory().unwrap();
        let s1 = state(5);
        let m = Mark {
            node_id: 1,
            kind: MarkKind::Expire,
            mark: "2026-12-01:7".into(),
        };
        db.apply_alert_effects(std::slice::from_ref(&s1), &[], std::slice::from_ref(&m))
            .unwrap();
        assert_eq!(db.load_alert_states().unwrap(), vec![s1.clone()]);
        assert_eq!(db.load_alert_marks().unwrap(), vec![m.clone()]);
        db.apply_alert_effects(&[], &[s1.key], &[]).unwrap();
        assert!(db.load_alert_states().unwrap().is_empty());
        assert_eq!(db.load_alert_marks().unwrap(), vec![m]);
    }

    #[test]
    fn events_paginate_desc() {
        let db = Db::open_in_memory().unwrap();
        let ids = db.insert_alert_events(&[ev(1), ev(2), ev(3)]).unwrap();
        assert_eq!(ids.len(), 3);
        let p1 = db.list_alert_events(None, 2).unwrap();
        assert_eq!(
            p1.iter().map(|e| e.id).collect::<Vec<_>>(),
            [ids[2], ids[1]]
        );
        let p2 = db.list_alert_events(Some(p1[1].id), 2).unwrap();
        assert_eq!(p2.iter().map(|e| e.id).collect::<Vec<_>>(), [ids[0]]);
        db.set_events_sent(&ids[..2], SendStatus::Ok).unwrap();
        let all = db.list_alert_events(None, 10).unwrap();
        assert_eq!(
            all.iter().map(|e| e.sent).collect::<Vec<_>>(),
            [SendStatus::Pending, SendStatus::Ok, SendStatus::Ok]
        );
    }

    #[test]
    fn prune_by_age() {
        let db = Db::open_in_memory().unwrap();
        let now = 1_000 * DAY_MS;
        db.insert_alert_events(&[ev(now - 91 * DAY_MS), ev(now - 89 * DAY_MS)])
            .unwrap();
        {
            let c = db.write.lock().unwrap();
            for (mark, age) in [("old", 401), ("new", 399)] {
                c.execute(
                    "INSERT INTO alert_marks (node_id, kind, mark, created_ms) VALUES (1, 'traffic', ?1, ?2)",
                    params![mark, now - age * DAY_MS],
                )
                .unwrap();
            }
        }
        assert_eq!(db.prune_alerts(now).unwrap(), 2);
        assert_eq!(db.list_alert_events(None, 10).unwrap().len(), 1);
        let marks = db.load_alert_marks().unwrap();
        assert_eq!(marks.len(), 1);
        assert_eq!(marks[0].mark, "new");
    }

    #[test]
    fn settings_defaults_and_sendkey() {
        let db = Db::open_in_memory().unwrap();
        assert_eq!(db.alert_settings().unwrap(), AlertSettings::default());
        let s = AlertSettings {
            offline_enabled: false,
            offline_grace_secs: 60,
            traffic_enabled: false,
            expire_enabled: true,
        };
        db.set_alert_settings(&s).unwrap();
        assert_eq!(db.alert_settings().unwrap(), s);
        assert_eq!(db.sendkey().unwrap(), None);
        db.set_sendkey(Some("SCT1tAbc")).unwrap();
        assert_eq!(db.sendkey().unwrap().as_deref(), Some("SCT1tAbc"));
        db.set_sendkey(None).unwrap();
        assert_eq!(db.sendkey().unwrap(), None);
    }

    #[test]
    fn non_firing_state_roundtrips_as_ok() {
        let db = Db::open_in_memory().unwrap();
        let mut s = state(7);
        s.firing = false;
        s.last_notified_ms = Some(123);
        db.apply_alert_effects(std::slice::from_ref(&s), &[], &[])
            .unwrap();
        let got = db.load_alert_states().unwrap();
        assert!(!got[0].firing);
        assert_eq!(got[0].last_notified_ms, Some(123));
        let status: String = db
            .raw_conn_for_test()
            .query_row("SELECT status FROM alert_state", [], |r| r.get(0))
            .unwrap();
        assert_eq!(status, "ok");
    }
}
