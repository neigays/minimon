use minimon_common::protocol::Hello;
use rusqlite::{OptionalExtension, params};
use sha2::{Digest, Sha256};
use std::fmt::Write;

use super::{Db, DbError, now_ms};

/// 月流量计费口径。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrafficMode {
    Rx,
    Tx,
    Sum,
    Max,
}

impl TrafficMode {
    pub fn as_str(self) -> &'static str {
        match self {
            TrafficMode::Rx => "rx",
            TrafficMode::Tx => "tx",
            TrafficMode::Sum => "sum",
            TrafficMode::Max => "max",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "rx" => Some(TrafficMode::Rx),
            "tx" => Some(TrafficMode::Tx),
            "sum" => Some(TrafficMode::Sum),
            "max" => Some(TrafficMode::Max),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Node {
    pub id: i64,
    pub name: String,
    pub sort: i64,
    pub hidden: bool,
    /// 最近一次 Hello（info_json）；从未上线或解析失败时为 None。
    pub info: Option<Hello>,
    pub group: String,
    pub region: String,
    pub note: String,
    pub last_ip: Option<String>,
    pub last_seen_ms: Option<i64>,
    pub created_ms: i64,
    /// 月流量额度（字节，十进制单位换算后）；None 表示不限。
    pub traffic_quota: Option<i64>,
    /// 账期重置日 1–31。
    pub traffic_reset_day: u8,
    pub traffic_mode: TrafficMode,
    /// 到期日 `YYYY-MM-DD`（按全局时区理解）。
    pub expire_date: Option<String>,
    /// 是否参与离线告警。
    pub alert_offline: bool,
    /// 规范化后的标签串（见 `crate::tags`）。
    pub tags: String,
    /// agent 自查的公网地址与国家（大写 2 位代码）。
    pub ipv4: Option<String>,
    pub ipv6: Option<String>,
    pub country: Option<String>,
}

const NODE_COLS: &str = "id, name, sort, hidden, info_json, grp, region, note, last_ip, \
     last_seen_ms, created_ms, traffic_quota, traffic_reset_day, traffic_mode, expire_date, \
     alert_offline, tags, ipv4, ipv6, country";

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        // 写入 String 不会失败
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// sha256 十六进制小写。
pub fn hash_token(token: &str) -> String {
    hex(&Sha256::digest(token.as_bytes()))
}

/// 32 字节随机数的十六进制（64 字符）。
pub fn generate_token() -> String {
    let mut buf = [0u8; 32];
    rand::fill(&mut buf);
    hex(&buf)
}

fn parse_info(id: i64, json: Option<String>) -> Option<Hello> {
    match serde_json::from_str(&json?) {
        Ok(h) => Some(h),
        Err(e) => {
            tracing::warn!(node_id = id, error = %e, "info_json 解析失败，忽略");
            None
        }
    }
}

fn row_to_node(r: &rusqlite::Row<'_>) -> rusqlite::Result<Node> {
    let id: i64 = r.get(0)?;
    Ok(Node {
        id,
        name: r.get(1)?,
        sort: r.get(2)?,
        hidden: r.get::<_, i64>(3)? != 0,
        info: parse_info(r.get(0)?, r.get::<_, Option<String>>(4)?),
        group: r.get(5)?,
        region: r.get(6)?,
        note: r.get(7)?,
        last_ip: r.get(8)?,
        last_seen_ms: r.get(9)?,
        created_ms: r.get(10)?,
        traffic_quota: r.get(11)?,
        traffic_reset_day: {
            let d: i64 = r.get(12)?;
            if !(1..=31).contains(&d) {
                tracing::warn!(
                    node_id = id,
                    reset_day = d,
                    "traffic_reset_day 超出 1-31，已钳制"
                );
            }
            d.clamp(1, 31) as u8
        },
        traffic_mode: {
            let m: String = r.get(13)?;
            TrafficMode::parse(&m).unwrap_or_else(|| {
                tracing::warn!(node_id = id, mode = %m, "未知 traffic_mode，按 sum 处理");
                TrafficMode::Sum
            })
        },
        expire_date: r.get(14)?,
        alert_offline: r.get::<_, i64>(15)? != 0,
        tags: r.get(16)?,
        ipv4: r.get(17)?,
        ipv6: r.get(18)?,
        country: r.get(19)?,
    })
}

/// 部分更新；`None` 表示不改。
#[derive(Debug, Clone, Default)]
pub struct NodePatch {
    pub name: Option<String>,
    pub group: Option<String>,
    pub region: Option<String>,
    pub note: Option<String>,
    pub hidden: Option<bool>,
    pub traffic_quota: Option<Option<i64>>,
    pub traffic_reset_day: Option<u8>,
    pub traffic_mode: Option<TrafficMode>,
    pub expire_date: Option<Option<String>>,
    pub alert_offline: Option<bool>,
    /// 已规范化的标签串。
    pub tags: Option<String>,
}

impl Db {
    /// 后台新增节点：带分组等字段，排在现有节点之后。返回节点与明文 token（仅此一次）。
    pub fn add_node_with(
        &self,
        name: &str,
        group: &str,
        region: &str,
        note: &str,
    ) -> Result<(Node, String), DbError> {
        let token = generate_token();
        let hash = hash_token(&token);
        let created_ms = now_ms();
        let (id, sort) = self.write(|c| {
            let tx = c.transaction()?;
            let sort: i64 =
                tx.query_row("SELECT coalesce(max(sort) + 1, 0) FROM nodes", [], |r| {
                    r.get(0)
                })?;
            tx.execute(
                "INSERT INTO nodes (name, token_hash, token, created_ms, sort, grp, region, note) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![name, hash, token, created_ms, sort, group, region, note],
            )?;
            let id = tx.last_insert_rowid();
            tx.commit()?;
            Ok((id, sort))
        })?;
        let node = Node {
            id,
            name: name.to_string(),
            sort,
            hidden: false,
            info: None,
            group: group.to_string(),
            region: region.to_string(),
            note: note.to_string(),
            last_ip: None,
            last_seen_ms: None,
            created_ms,
            traffic_quota: None,
            traffic_reset_day: 1,
            traffic_mode: TrafficMode::Sum,
            expire_date: None,
            alert_offline: true,
            tags: String::new(),
            ipv4: None,
            ipv6: None,
            country: None,
        };
        Ok((node, token))
    }

    /// 部分更新节点；节点不存在返回 false。
    pub fn update_node(&self, id: i64, p: &NodePatch) -> Result<bool, DbError> {
        self.write(|c| {
            let n = c.execute(
                "UPDATE nodes SET name = coalesce(?1, name), grp = coalesce(?2, grp), \
                 region = coalesce(?3, region), note = coalesce(?4, note), \
                 hidden = coalesce(?5, hidden), \
                 traffic_quota = CASE WHEN ?6 THEN ?7 ELSE traffic_quota END, \
                 traffic_reset_day = coalesce(?8, traffic_reset_day), \
                 traffic_mode = coalesce(?9, traffic_mode), \
                 expire_date = CASE WHEN ?10 THEN ?11 ELSE expire_date END, \
                 alert_offline = coalesce(?12, alert_offline), \
                 tags = coalesce(?14, tags) \
                 WHERE id = ?13",
                params![
                    p.name,
                    p.group,
                    p.region,
                    p.note,
                    p.hidden.map(i64::from),
                    p.traffic_quota.is_some(),
                    p.traffic_quota.flatten(),
                    p.traffic_reset_day.map(i64::from),
                    p.traffic_mode.map(TrafficMode::as_str),
                    p.expire_date.is_some(),
                    p.expire_date.clone().flatten(),
                    p.alert_offline.map(i64::from),
                    id,
                    p.tags
                ],
            )?;
            Ok(n > 0)
        })
    }

    /// 保存 agent 上报的公网信息（已净化）；None 字段保持原值。
    pub fn set_netinfo(
        &self,
        id: i64,
        n: &minimon_common::protocol::NetInfo,
    ) -> Result<(), DbError> {
        self.write(|c| {
            c.execute(
                "UPDATE nodes SET ipv4 = coalesce(?1, ipv4), ipv6 = coalesce(?2, ipv6), \
                 country = coalesce(?3, country) WHERE id = ?4",
                params![n.ipv4, n.ipv6, n.country, id],
            )?;
            Ok(())
        })
    }

    /// 删除节点及其全部指标（同一事务）；节点不存在返回 false。
    pub fn delete_node(&self, id: i64) -> Result<bool, DbError> {
        self.write(|c| {
            let tx = c.transaction()?;
            for t in ["metrics_raw", "metrics_1m", "metrics_15m"] {
                tx.execute(&format!("DELETE FROM {t} WHERE node_id = ?1"), [id])?;
            }
            // 无外键：探测任务关联与节点行同一事务删除。
            tx.execute("DELETE FROM probe_task_nodes WHERE node_id = ?1", [id])?;
            for t in ["alert_state", "alert_marks", "alert_rule_nodes"] {
                tx.execute(&format!("DELETE FROM {t} WHERE node_id = ?1"), [id])?;
            }
            let n = tx.execute("DELETE FROM nodes WHERE id = ?1", [id])?;
            tx.commit()?;
            Ok(n > 0)
        })
    }

    /// 重置 token，返回新明文 token；节点不存在返回 None。
    pub fn reset_token(&self, id: i64) -> Result<Option<String>, DbError> {
        let token = generate_token();
        let hash = hash_token(&token);
        self.write(|c| {
            let n = c.execute(
                "UPDATE nodes SET token_hash = ?1, token = ?2 WHERE id = ?3",
                params![hash, token, id],
            )?;
            Ok((n > 0).then_some(token))
        })
    }

    /// 节点的 token 原文：外层 None = 节点不存在；内层 None = 旧版本创建、只存了哈希。
    pub fn node_token(&self, id: i64) -> Result<Option<Option<String>>, DbError> {
        self.read(|c| {
            Ok(
                c.query_row("SELECT token FROM nodes WHERE id = ?1", [id], |r| {
                    r.get::<_, Option<String>>(0)
                })
                .optional()?,
            )
        })
    }

    /// 按数组下标写 sort（单事务）。
    pub fn reorder(&self, ids: &[i64]) -> Result<(), DbError> {
        self.write(|c| {
            let tx = c.transaction()?;
            {
                let mut stmt = tx.prepare("UPDATE nodes SET sort = ?1 WHERE id = ?2")?;
                for (i, id) in ids.iter().enumerate() {
                    stmt.execute(params![i as i64, id])?;
                }
            }
            tx.commit()?;
            Ok(())
        })
    }

    /// 新增节点，返回节点与明文 token（仅此一次）。
    pub fn add_node(&self, name: &str) -> Result<(Node, String), DbError> {
        let token = generate_token();
        let hash = hash_token(&token);
        let created_ms = now_ms();
        let (id, sort) = self.write(|c| {
            let tx = c.transaction()?;
            let sort: i64 =
                tx.query_row("SELECT coalesce(max(sort) + 1, 0) FROM nodes", [], |r| {
                    r.get(0)
                })?;
            tx.execute(
                "INSERT INTO nodes (name, token_hash, token, created_ms, sort) \
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![name, hash, token, created_ms, sort],
            )?;
            let id = tx.last_insert_rowid();
            tx.commit()?;
            Ok((id, sort))
        })?;
        let node = Node {
            id,
            name: name.to_string(),
            sort,
            hidden: false,
            info: None,
            group: String::new(),
            region: String::new(),
            note: String::new(),
            last_ip: None,
            last_seen_ms: None,
            created_ms,
            traffic_quota: None,
            traffic_reset_day: 1,
            traffic_mode: TrafficMode::Sum,
            expire_date: None,
            alert_offline: true,
            tags: String::new(),
            ipv4: None,
            ipv6: None,
            country: None,
        };
        Ok((node, token))
    }

    pub fn list_nodes(&self) -> Result<Vec<Node>, DbError> {
        self.read(|c| {
            let mut stmt =
                c.prepare(&format!("SELECT {NODE_COLS} FROM nodes ORDER BY sort, id"))?;
            let rows = stmt.query_map([], row_to_node)?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
    }

    pub fn node_by_token(&self, token: &str) -> Result<Option<Node>, DbError> {
        let hash = hash_token(token);
        self.read(|c| {
            Ok(c.query_row(
                &format!("SELECT {NODE_COLS} FROM nodes WHERE token_hash = ?1"),
                [hash],
                row_to_node,
            )
            .optional()?)
        })
    }

    pub fn set_node_ip(&self, id: i64, ip: &str) -> Result<(), DbError> {
        self.write(|c| {
            c.execute(
                "UPDATE nodes SET last_ip = ?1 WHERE id = ?2",
                params![ip, id],
            )?;
            Ok(())
        })
    }

    pub fn save_node_info(&self, id: i64, hello: &Hello) -> Result<(), DbError> {
        let json = serde_json::to_string(hello)?;
        self.write(|c| {
            c.execute(
                "UPDATE nodes SET info_json = ?1 WHERE id = ?2",
                params![json, id],
            )?;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hello() -> Hello {
        Hello {
            v: 1,
            agent_version: "0.1.0".into(),
            hostname: "h".into(),
            os: "linux".into(),
            kernel: "6".into(),
            arch: "x86_64".into(),
            cpu_model: "cpu".into(),
            cpu_cores: 2,
            mem_total: 1,
            swap_total: 0,
            disk_total: 2,
            virtualization: None,
        }
    }

    #[test]
    fn add_and_lookup_by_token() {
        let db = Db::open_in_memory().unwrap();
        let (n, tok) = db.add_node("hk-1").unwrap();
        assert_eq!(tok.len(), 64);
        assert_eq!(db.node_by_token(&tok).unwrap().unwrap().id, n.id);
        assert!(db.node_by_token("wrong").unwrap().is_none());
    }

    #[test]
    fn token_not_stored_in_plaintext() {
        let db = Db::open_in_memory().unwrap();
        let (_, tok) = db.add_node("a").unwrap();
        let stored: String = db
            .raw_conn_for_test()
            .query_row("SELECT token_hash FROM nodes", [], |r| r.get(0))
            .unwrap();
        assert_ne!(stored, tok);
        assert_eq!(stored, hash_token(&tok));
    }

    #[test]
    fn migrations_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("m.db");
        Db::open(&p).unwrap();
        Db::open(&p).unwrap();
    }

    #[test]
    fn hash_is_sha256_hex() {
        assert_eq!(
            hash_token("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn list_reads_what_write_conn_wrote() {
        let db = Db::open_in_memory().unwrap();
        db.add_node("a").unwrap();
        db.add_node("b").unwrap();
        let names: Vec<_> = db
            .list_nodes()
            .unwrap()
            .into_iter()
            .map(|n| n.name)
            .collect();
        assert_eq!(names, ["a", "b"]);
    }

    #[test]
    fn save_node_info_stores_json() {
        let db = Db::open_in_memory().unwrap();
        let (n, _) = db.add_node("a").unwrap();
        db.save_node_info(n.id, &hello()).unwrap();
        let json: String = db
            .raw_conn_for_test()
            .query_row("SELECT info_json FROM nodes WHERE id = ?1", [n.id], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(serde_json::from_str::<Hello>(&json).unwrap(), hello());
    }

    #[test]
    fn migration_0002_adds_node_columns_with_defaults() {
        let db = Db::open_in_memory().unwrap();
        let (n, _) = db.add_node("a").unwrap();
        let got = db
            .list_nodes()
            .unwrap()
            .into_iter()
            .find(|x| x.id == n.id)
            .unwrap();
        assert_eq!(
            (got.group.as_str(), got.region.as_str(), got.note.as_str()),
            ("", "", "")
        );
        assert!(got.last_ip.is_none() && got.last_seen_ms.is_none() && got.created_ms > 0);
    }

    #[test]
    fn set_ip_and_last_seen_via_insert() {
        let db = Db::open_in_memory().unwrap();
        let (n, _) = db.add_node("a").unwrap();
        db.set_node_ip(n.id, "1.2.3.4").unwrap();
        let mut r = crate::state::tests::report();
        r.ts_ms = 500;
        db.insert_reports(&[(n.id, r.clone())]).unwrap();
        r.ts_ms = 300;
        db.insert_reports(&[(n.id, r)]).unwrap();
        let got = &db.list_nodes().unwrap()[0];
        assert_eq!(got.last_ip.as_deref(), Some("1.2.3.4"));
        assert_eq!(got.last_seen_ms, Some(500));
    }

    #[test]
    fn last_seen_clamped_to_server_time() {
        let db = Db::open_in_memory().unwrap();
        let (n, _) = db.add_node("a").unwrap();
        let mut r = crate::state::tests::report();
        r.ts_ms = now_ms() + 3_600_000;
        let before = now_ms();
        db.insert_reports(&[(n.id, r)]).unwrap();
        let seen = db.list_nodes().unwrap()[0].last_seen_ms.unwrap();
        assert!(seen >= before && seen <= now_ms(), "{seen}");
    }

    #[test]
    fn add_update_reset_reorder_delete() {
        let db = Db::open_in_memory().unwrap();
        let (a, ta) = db.add_node_with("a", "g", "HK", "n").unwrap();
        let (b, _) = db.add_node_with("b", "", "", "").unwrap();
        assert_eq!((a.sort, b.sort), (0, 1));
        let patch = NodePatch {
            name: Some("a2".into()),
            hidden: Some(true),
            ..Default::default()
        };
        assert!(db.update_node(a.id, &patch).unwrap());
        assert!(!db.update_node(999, &patch).unwrap());
        let got = &db.list_nodes().unwrap()[0];
        assert_eq!(
            (got.name.as_str(), got.group.as_str(), got.hidden),
            ("a2", "g", true)
        );
        let new = db.reset_token(a.id).unwrap().unwrap();
        assert!(db.node_by_token(&ta).unwrap().is_none());
        assert_eq!(db.node_by_token(&new).unwrap().unwrap().id, a.id);
        assert!(db.reset_token(999).unwrap().is_none());
        db.reorder(&[b.id, a.id]).unwrap();
        let ids: Vec<_> = db.list_nodes().unwrap().iter().map(|n| n.id).collect();
        assert_eq!(ids, [b.id, a.id]);
        let mut r = crate::state::tests::report();
        r.ts_ms = 1;
        db.insert_reports(&[(a.id, r.clone()), (b.id, r)]).unwrap();
        assert!(db.delete_node(a.id).unwrap());
        assert!(!db.delete_node(a.id).unwrap());
        let n: i64 = db
            .raw_conn_for_test()
            .query_row(
                "SELECT count(*) FROM metrics_raw WHERE node_id = ?1",
                [a.id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 0);
        assert_eq!(db.list_nodes().unwrap().len(), 1);
    }

    #[test]
    fn ids_are_not_reused_after_delete() {
        let db = Db::open_in_memory().unwrap();
        let (a, _) = db.add_node("a").unwrap();
        db.delete_node(a.id).unwrap();
        let (b, _) = db.add_node("b").unwrap();
        assert_ne!(a.id, b.id);
        let (c, _) = db.add_node_with("c", "", "", "").unwrap();
        assert!(c.id > b.id);
    }

    #[test]
    fn insert_reports_skips_missing_nodes_without_failing_batch() {
        let db = Db::open_in_memory().unwrap();
        let (a, _) = db.add_node("a").unwrap();
        let mut r = crate::state::tests::report();
        r.ts_ms = 10;
        let n = db
            .insert_reports(&[(9999, r.clone()), (a.id, r.clone())])
            .unwrap();
        assert_eq!(n, 1);
        let rows: i64 = db
            .raw_conn_for_test()
            .query_row("SELECT count(*) FROM metrics_raw", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, 1);
        assert_eq!(db.insert_reports(&[(9999, r)]).unwrap(), 0);
    }

    #[test]
    fn m2_tables_exist() {
        let db = Db::open_in_memory().unwrap();
        let c = db.raw_conn_for_test();
        for t in ["metrics_1m", "metrics_15m", "admin", "sessions", "settings"] {
            let n: i64 = c
                .query_row(
                    "SELECT count(*) FROM sqlite_master WHERE type='table' AND name=?1",
                    [t],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(n, 1, "{t}");
        }
    }

    #[test]
    fn add_node_sort_is_max_plus_one() {
        let db = Db::open_in_memory().unwrap();
        let (a, _) = db.add_node("a").unwrap();
        let (b, _) = db.add_node("b").unwrap();
        let (c, _) = db.add_node_with("c", "", "", "").unwrap();
        assert_eq!((a.sort, b.sort, c.sort), (0, 1, 2));
        let sorts: Vec<_> = db.list_nodes().unwrap().iter().map(|n| n.sort).collect();
        assert_eq!(sorts, vec![0, 1, 2]);
    }

    #[test]
    fn m1_database_migrates_with_data_intact() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("m1.db");
        let info = r#"{"v":1,"agent_version":"0.1.0","hostname":"old-host","os":"linux","kernel":"6","arch":"x86_64","cpu_model":"cpu","cpu_cores":2,"mem_total":1,"swap_total":0,"disk_total":2,"virtualization":null}"#;
        {
            let c = rusqlite::Connection::open(&path).unwrap();
            c.execute_batch(include_str!("migrations/0001_init.sql"))
                .unwrap();
            // rusqlite_migration 以 PRAGMA user_version 记录版本
            c.pragma_update(None, "user_version", 1).unwrap();
            c.execute(
                "INSERT INTO nodes (id, name, token_hash, sort, created_ms, info_json) VALUES (7, 'old', ?1, 3, 1000, ?2)",
                params![hash_token("old-token"), info],
            )
            .unwrap();
            c.execute(
                "INSERT INTO nodes (id, name, token_hash, created_ms) VALUES (9, 'old2', ?1, 1001)",
                params![hash_token("t2")],
            )
            .unwrap();
            c.execute(
                "INSERT INTO metrics_raw (node_id, ts_ms, cpu) VALUES (7, 5000, 12.5)",
                [],
            )
            .unwrap();
        }
        let db = Db::open(&path).unwrap();
        let nodes = db.list_nodes().unwrap();
        assert_eq!(nodes.len(), 2);
        let n = db.node_by_token("old-token").unwrap().unwrap();
        assert_eq!((n.id, n.name.as_str(), n.sort), (7, "old", 3));
        assert_eq!(
            n.info.as_ref().map(|i| i.hostname.as_str()),
            Some("old-host")
        );
        // 新增列取默认值
        assert!(!n.hidden);
        assert_eq!(
            (n.group.as_str(), n.region.as_str(), n.note.as_str()),
            ("", "", "")
        );
        assert_eq!(
            (n.traffic_quota, n.traffic_reset_day, n.traffic_mode),
            (None, 1, TrafficMode::Sum)
        );
        assert_eq!(n.expire_date, None);
        assert!(n.last_ip.is_none() && n.last_seen_ms.is_none());
        let cpu: f64 = db
            .raw_conn_for_test()
            .query_row(
                "SELECT cpu FROM metrics_raw WHERE node_id = 7 AND ts_ms = 5000",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(cpu, 12.5);
        let (new, _) = db.add_node("new").unwrap();
        assert!(new.id > 9, "新节点 id {} 应大于旧最大 id", new.id);
        assert_eq!(new.sort, 4);
    }

    #[test]
    fn node_m3_defaults() {
        let db = Db::open_in_memory().unwrap();
        let (n, _) = db.add_node("a").unwrap();
        assert_eq!(n.traffic_quota, None);
        assert_eq!(n.traffic_reset_day, 1);
        assert_eq!(n.traffic_mode, TrafficMode::Sum);
        assert_eq!(n.expire_date, None);
        assert_eq!(db.list_nodes().unwrap()[0], n);
        let (g, _) = db.add_node_with("b", "g", "r", "x").unwrap();
        assert_eq!(db.list_nodes().unwrap()[1], g);
    }

    #[test]
    fn traffic_mode_serde_and_db_strings() {
        for (m, s) in [
            (TrafficMode::Rx, "rx"),
            (TrafficMode::Tx, "tx"),
            (TrafficMode::Sum, "sum"),
            (TrafficMode::Max, "max"),
        ] {
            assert_eq!(m.as_str(), s);
            assert_eq!(TrafficMode::parse(s), Some(m));
            assert_eq!(serde_json::to_string(&m).unwrap(), format!("\"{s}\""));
        }
        assert_eq!(TrafficMode::parse("bogus"), None);
    }

    #[test]
    fn migration_0004_creates_probe_tables_and_keeps_data() {
        let db = Db::open_in_memory().unwrap();
        let (n, _) = db.add_node("a").unwrap();
        let c = db.raw_conn_for_test();
        for t in ["probe_tasks", "probe_task_nodes", "probe_raw", "probe_5m"] {
            let cnt: i64 = c
                .query_row(&format!("SELECT count(*) FROM {t}"), [], |r| r.get(0))
                .unwrap();
            assert_eq!(cnt, 0, "{t}");
        }
        assert_eq!(n.id, 1);
    }

    #[test]
    fn node_alert_offline_patch() {
        let db = Db::open_in_memory().unwrap();
        let (n, _) = db.add_node("a").unwrap();
        assert!(n.alert_offline);
        assert!(db.list_nodes().unwrap()[0].alert_offline);
        let p = NodePatch {
            alert_offline: Some(false),
            ..Default::default()
        };
        assert!(db.update_node(n.id, &p).unwrap());
        assert!(!db.list_nodes().unwrap()[0].alert_offline);
    }

    #[test]
    fn node_delete_cleans_alert_rows() {
        use crate::alert::model::*;
        let db = Db::open_in_memory().unwrap();
        let (n, _) = db.add_node("a").unwrap();
        let rule = db
            .create_alert_rule(&NewAlertRule {
                name: "r".into(),
                enabled: true,
                metric: Metric::Cpu,
                op: Op::Gt,
                threshold: 90.0,
                duration_secs: 300,
                cooldown_secs: 1800,
                all_nodes: false,
                node_ids: vec![n.id],
                probe_task_id: None,
            })
            .unwrap();
        let key = AlertKey {
            kind: StateKind::Rule,
            rule_id: rule.id,
            node_id: n.id,
            task_id: 0,
        };
        let st = StateRow {
            key,
            firing: false,
            since_ms: 1,
            fired_ms: None,
            notified: false,
            value: None,
            last_notified_ms: None,
        };
        let mark = Mark {
            node_id: n.id,
            kind: MarkKind::Traffic,
            mark: "2026-11:80".into(),
        };
        db.apply_alert_effects(&[st], &[], &[mark]).unwrap();
        db.insert_alert_events(&[NewEvent {
            ts_ms: 1,
            node_id: Some(n.id),
            node_name: Some("a".into()),
            kind: EventKind::Firing,
            title: "t".into(),
            body: "b".into(),
            sent: SendStatus::Pending,
        }])
        .unwrap();
        assert!(db.delete_node(n.id).unwrap());
        assert!(db.load_alert_states().unwrap().is_empty());
        assert!(db.load_alert_marks().unwrap().is_empty());
        assert!(db.list_alert_rules().unwrap()[0].node_ids.is_empty());
        assert_eq!(db.list_alert_events(None, 10).unwrap().len(), 1);
    }
}
