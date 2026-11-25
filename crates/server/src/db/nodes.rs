use minimon_common::protocol::Hello;
use rusqlite::{OptionalExtension, params};
use sha2::{Digest, Sha256};
use std::fmt::Write;

use super::{Db, DbError, now_ms};

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
}

const NODE_COLS: &str =
    "id, name, sort, hidden, info_json, grp, region, note, last_ip, last_seen_ms, created_ms";

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
    Ok(Node {
        id: r.get(0)?,
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
                "INSERT INTO nodes (name, token_hash, created_ms, sort, grp, region, note) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![name, hash, created_ms, sort, group, region, note],
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
        };
        Ok((node, token))
    }

    /// 部分更新节点；节点不存在返回 false。
    pub fn update_node(&self, id: i64, p: &NodePatch) -> Result<bool, DbError> {
        self.write(|c| {
            let n = c.execute(
                "UPDATE nodes SET name = coalesce(?1, name), grp = coalesce(?2, grp), \
                 region = coalesce(?3, region), note = coalesce(?4, note), \
                 hidden = coalesce(?5, hidden) WHERE id = ?6",
                params![
                    p.name,
                    p.group,
                    p.region,
                    p.note,
                    p.hidden.map(i64::from),
                    id
                ],
            )?;
            Ok(n > 0)
        })
    }

    /// 删除节点及其全部指标（同一事务）；节点不存在返回 false。
    pub fn delete_node(&self, id: i64) -> Result<bool, DbError> {
        self.write(|c| {
            let tx = c.transaction()?;
            for t in ["metrics_raw", "metrics_1m", "metrics_15m"] {
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
                "UPDATE nodes SET token_hash = ?1 WHERE id = ?2",
                params![hash, id],
            )?;
            Ok((n > 0).then_some(token))
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
        let id = self.write(|c| {
            c.execute(
                "INSERT INTO nodes (name, token_hash, created_ms) VALUES (?1, ?2, ?3)",
                params![name, hash, created_ms],
            )?;
            Ok(c.last_insert_rowid())
        })?;
        let node = Node {
            id,
            name: name.to_string(),
            sort: 0,
            hidden: false,
            info: None,
            group: String::new(),
            region: String::new(),
            note: String::new(),
            last_ip: None,
            last_seen_ms: None,
            created_ms,
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
}
