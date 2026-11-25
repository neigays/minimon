//! 探测任务：任务表、节点关联与探测数据的清理。
//!
//! 表之间没有外键，级联删除全在这里用代码完成。

use super::{Db, DbError, now_ms};
use minimon_common::protocol::ProbeKind;
use rusqlite::{Connection, params};
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ProbeTaskRow {
    pub id: i64,
    pub name: String,
    pub kind: ProbeKind,
    pub target: String,
    pub interval_secs: u32,
    pub samples: u32,
    pub timeout_ms: u32,
    pub all_nodes: bool,
    /// 仅 `all_nodes = false` 时有内容（升序、去重）。
    pub node_ids: Vec<i64>,
    pub show_on_card: bool,
    pub sort: i64,
}

#[derive(Debug, Clone)]
pub struct NewProbeTask {
    pub name: String,
    pub kind: ProbeKind,
    pub target: String,
    pub interval_secs: u32,
    pub samples: u32,
    pub timeout_ms: u32,
    pub all_nodes: bool,
    pub node_ids: Vec<i64>,
    pub show_on_card: bool,
    /// `None` = 当前最大值 + 1。
    pub sort: Option<i64>,
}

/// 字段为 `None` 表示不改；`node_ids = Some(..)` 表示整体替换关联（空 = 清空）。
#[derive(Debug, Clone, Default)]
pub struct ProbeTaskPatch {
    pub name: Option<String>,
    pub kind: Option<ProbeKind>,
    pub target: Option<String>,
    pub interval_secs: Option<u32>,
    pub samples: Option<u32>,
    pub timeout_ms: Option<u32>,
    pub all_nodes: Option<bool>,
    pub node_ids: Option<Vec<i64>>,
    pub show_on_card: Option<bool>,
    pub sort: Option<i64>,
}

pub fn kind_str(k: ProbeKind) -> &'static str {
    match k {
        ProbeKind::Icmp => "icmp",
        ProbeKind::Tcp => "tcp",
        ProbeKind::Http => "http",
    }
}

fn parse_kind(s: &str) -> Option<ProbeKind> {
    match s {
        "icmp" => Some(ProbeKind::Icmp),
        "tcp" => Some(ProbeKind::Tcp),
        "http" => Some(ProbeKind::Http),
        _ => None,
    }
}

/// 每个事务清理的 (task, node) 组数。
const CLEAN_BATCH: usize = 10;

const TASK_COLS: &str =
    "id, name, kind, target, interval_secs, samples, timeout_ms, all_nodes, show_on_card, sort";

fn load_tasks(c: &Connection, only: Option<i64>) -> Result<Vec<ProbeTaskRow>, DbError> {
    let sql = format!(
        "SELECT {TASK_COLS} FROM probe_tasks {} ORDER BY sort, id",
        if only.is_some() { "WHERE id = ?1" } else { "" }
    );
    let mut stmt = c.prepare(&sql)?;
    let map = |r: &rusqlite::Row<'_>| -> rusqlite::Result<ProbeTaskRow> {
        let kind: String = r.get(2)?;
        let kind = parse_kind(&kind).ok_or_else(|| {
            rusqlite::Error::FromSqlConversionFailure(
                2,
                rusqlite::types::Type::Text,
                format!("未知探测类型 {kind}").into(),
            )
        })?;
        Ok(ProbeTaskRow {
            id: r.get(0)?,
            name: r.get(1)?,
            kind,
            target: r.get(3)?,
            interval_secs: r.get(4)?,
            samples: r.get(5)?,
            timeout_ms: r.get(6)?,
            all_nodes: r.get::<_, i64>(7)? != 0,
            node_ids: Vec::new(),
            show_on_card: r.get::<_, i64>(8)? != 0,
            sort: r.get(9)?,
        })
    };
    let mut rows: Vec<ProbeTaskRow> = match only {
        Some(id) => stmt.query_map([id], map)?.collect::<Result<_, _>>()?,
        None => stmt.query_map([], map)?.collect::<Result<_, _>>()?,
    };
    let mut links = c.prepare("SELECT task_id, node_id FROM probe_task_nodes ORDER BY node_id")?;
    let pairs: Vec<(i64, i64)> = links
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_, _>>()?;
    for (t, n) in pairs {
        if let Some(row) = rows.iter_mut().find(|row| row.id == t) {
            row.node_ids.push(n);
        }
    }
    Ok(rows)
}

fn replace_links(c: &Connection, task_id: i64, node_ids: &[i64]) -> Result<(), DbError> {
    c.execute("DELETE FROM probe_task_nodes WHERE task_id = ?1", [task_id])?;
    let mut ins =
        c.prepare("INSERT OR IGNORE INTO probe_task_nodes(task_id, node_id) VALUES (?1, ?2)")?;
    for n in node_ids {
        ins.execute(params![task_id, n])?;
    }
    Ok(())
}

/// 逐 (task, node) 分批删除探测数据；`filter` 取 `(task_id, node_id)` 对。
fn purge_pairs(db: &Db, pairs: Vec<(i64, i64)>) -> Result<(), DbError> {
    for chunk in pairs.chunks(CLEAN_BATCH) {
        db.write(|c| {
            let tx = c.transaction()?;
            for &(t, n) in chunk {
                for table in ["probe_raw", "probe_5m"] {
                    tx.execute(
                        &format!("DELETE FROM {table} WHERE task_id = ?1 AND node_id = ?2"),
                        params![t, n],
                    )?;
                }
            }
            tx.commit()?;
            Ok(())
        })?;
    }
    Ok(())
}

impl Db {
    pub fn list_probe_tasks(&self) -> Result<Vec<ProbeTaskRow>, DbError> {
        self.read(|c| load_tasks(c, None))
    }

    pub fn create_probe_task(&self, t: &NewProbeTask) -> Result<ProbeTaskRow, DbError> {
        let mut v = self.create_probe_tasks(std::slice::from_ref(t))?;
        v.pop()
            .ok_or_else(|| DbError::Other("创建探测任务后未读到记录".into()))
    }

    /// 单事务创建多个任务（三网模板用）。
    pub fn create_probe_tasks(&self, tasks: &[NewProbeTask]) -> Result<Vec<ProbeTaskRow>, DbError> {
        self.write(|c| {
            let tx = c.transaction()?;
            let mut ids = Vec::with_capacity(tasks.len());
            for t in tasks {
                let sort = match t.sort {
                    Some(s) => s,
                    None => tx.query_row(
                        "SELECT COALESCE(MAX(sort), 0) + 1 FROM probe_tasks",
                        [],
                        |r| r.get(0),
                    )?,
                };
                tx.execute(
                    "INSERT INTO probe_tasks(name, kind, target, interval_secs, samples, \
                     timeout_ms, all_nodes, show_on_card, sort, created_ms) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                    params![
                        t.name,
                        kind_str(t.kind),
                        t.target,
                        t.interval_secs,
                        t.samples,
                        t.timeout_ms,
                        t.all_nodes,
                        t.show_on_card,
                        sort,
                        now_ms()
                    ],
                )?;
                let id = tx.last_insert_rowid();
                if !t.all_nodes {
                    replace_links(&tx, id, &t.node_ids)?;
                }
                ids.push(id);
            }
            let out = ids
                .into_iter()
                .map(|id| Ok(load_tasks(&tx, Some(id))?.pop()))
                .collect::<Result<Vec<_>, DbError>>()?
                .into_iter()
                .flatten()
                .collect();
            tx.commit()?;
            Ok(out)
        })
    }

    /// 任务不存在返回 false。
    pub fn update_probe_task(&self, id: i64, p: &ProbeTaskPatch) -> Result<bool, DbError> {
        self.write(|c| {
            let tx = c.transaction()?;
            let exists: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM probe_tasks WHERE id = ?1)",
                [id],
                |r| r.get(0),
            )?;
            if !exists {
                return Ok(false);
            }
            let kind = p.kind.map(kind_str);
            tx.execute(
                "UPDATE probe_tasks SET \
                 name = COALESCE(?2, name), kind = COALESCE(?3, kind), \
                 target = COALESCE(?4, target), interval_secs = COALESCE(?5, interval_secs), \
                 samples = COALESCE(?6, samples), timeout_ms = COALESCE(?7, timeout_ms), \
                 all_nodes = COALESCE(?8, all_nodes), show_on_card = COALESCE(?9, show_on_card), \
                 sort = COALESCE(?10, sort) WHERE id = ?1",
                params![
                    id,
                    p.name,
                    kind,
                    p.target,
                    p.interval_secs,
                    p.samples,
                    p.timeout_ms,
                    p.all_nodes,
                    p.show_on_card,
                    p.sort
                ],
            )?;
            if let Some(ids) = &p.node_ids {
                replace_links(&tx, id, ids)?;
            }
            tx.commit()?;
            Ok(true)
        })
    }

    /// 删除任务行与它的节点关联（单事务）；不存在返回 false。数据清理见 `purge_probe_task_data`。
    pub fn delete_probe_task(&self, id: i64) -> Result<bool, DbError> {
        self.write(|c| {
            let tx = c.transaction()?;
            tx.execute("DELETE FROM probe_task_nodes WHERE task_id = ?1", [id])?;
            let n = tx.execute("DELETE FROM probe_tasks WHERE id = ?1", [id])?;
            tx.commit()?;
            Ok(n > 0)
        })
    }

    /// 分批清除某任务在 `probe_raw/probe_5m` 的全部数据。
    pub fn purge_probe_task_data(&self, id: i64) -> Result<(), DbError> {
        let nodes: Vec<i64> = self.read(|c| {
            let mut stmt = c.prepare(
                "SELECT node_id FROM probe_raw WHERE task_id = ?1 \
                 UNION SELECT node_id FROM probe_5m WHERE task_id = ?1",
            )?;
            Ok(stmt
                .query_map([id], |r| r.get(0))?
                .collect::<Result<_, _>>()?)
        })?;
        purge_pairs(self, nodes.into_iter().map(|n| (id, n)).collect())
    }

    /// 节点被删除：清它的全部探测数据（任务关联已在 `delete_node` 事务内删除）。
    pub fn purge_probe_data_for_node(&self, node_id: i64) -> Result<(), DbError> {
        let tasks: Vec<i64> = self.read(|c| {
            // 递归 CTE 松散扫描主键前缀：每个 task_id 只探一次。
            let mut stmt = c.prepare(
                "WITH RECURSIVE \
                 a(n) AS (SELECT MIN(task_id) FROM probe_raw UNION ALL \
                          SELECT (SELECT MIN(task_id) FROM probe_raw WHERE task_id > a.n) \
                          FROM a WHERE n IS NOT NULL), \
                 b(n) AS (SELECT MIN(task_id) FROM probe_5m UNION ALL \
                          SELECT (SELECT MIN(task_id) FROM probe_5m WHERE task_id > b.n) \
                          FROM b WHERE n IS NOT NULL) \
                 SELECT n FROM a WHERE n IS NOT NULL UNION SELECT n FROM b WHERE n IS NOT NULL",
            )?;
            Ok(stmt
                .query_map([], |r| r.get(0))?
                .collect::<Result<_, _>>()?)
        })?;
        purge_pairs(self, tasks.into_iter().map(|t| (t, node_id)).collect())
    }
}
