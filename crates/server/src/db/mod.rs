mod aggregate;
pub mod alert;
pub mod auth;
pub mod history;
mod last_report;
mod metrics;
pub mod nodes;
pub mod probe;
pub mod probe_rollup;
pub mod rollup;
mod traffic;

use rusqlite::{Connection, OpenFlags};
use rusqlite_migration::{M, Migrations};
use std::path::Path;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

pub use aggregate::ROLLUP_LAG_MS;
pub use nodes::{Node, NodePatch, TrafficMode};
pub use probe::{NewProbeTask, ProbeTaskPatch, ProbeTaskRow};
pub use probe_rollup::{ProbeGranularity, ProbeSeries};

#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("migration: {0}")]
    Migration(#[from] rusqlite_migration::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("密码哈希失败: {0}")]
    Hash(String),
    #[error("管理员账号尚未创建")]
    NoAdmin,
    #[error("{0}")]
    Other(String),
    #[error("database lock poisoned")]
    Poisoned,
}

const BUSY_TIMEOUT: Duration = Duration::from_millis(5000);

/// 一个写连接（所有写入）加一个读连接。
pub struct Db {
    write: Mutex<Connection>,
    read: Mutex<Connection>,
}

pub(crate) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn lock(m: &Mutex<Connection>) -> Result<MutexGuard<'_, Connection>, DbError> {
    m.lock().map_err(|_| DbError::Poisoned)
}

fn migrate(conn: &mut Connection) -> Result<(), DbError> {
    let migrations = Migrations::new(vec![
        M::up(include_str!("migrations/0001_init.sql")),
        M::up(include_str!("migrations/0002_m2.sql")),
        M::up(include_str!("migrations/0003_nodes_autoincrement.sql")),
        M::up(include_str!("migrations/0004_m3.sql")),
        M::up(include_str!("migrations/0005_m4a.sql")),
    ]);
    migrations.to_latest(conn)?;
    Ok(())
}

impl Db {
    pub fn open(path: &Path) -> Result<Db, DbError> {
        let mut write = Connection::open(path)?;
        write.busy_timeout(BUSY_TIMEOUT)?;
        write.pragma_update(None, "journal_mode", "WAL")?;
        write.pragma_update(None, "synchronous", "NORMAL")?;
        migrate(&mut write)?;
        let read = Connection::open(path)?;
        read.busy_timeout(BUSY_TIMEOUT)?;
        read.pragma_update(None, "query_only", true)?;
        Ok(Db {
            write: Mutex::new(write),
            read: Mutex::new(read),
        })
    }

    /// 内存库：两个连接通过唯一的共享缓存 URI 看到同一份数据。
    pub fn open_in_memory() -> Result<Db, DbError> {
        let uri = format!(
            "file:minimon-mem-{:016x}?mode=memory&cache=shared",
            rand::random::<u64>()
        );
        let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_URI
            | OpenFlags::SQLITE_OPEN_NO_MUTEX;
        let mut write = Connection::open_with_flags(&uri, flags)?;
        write.busy_timeout(BUSY_TIMEOUT)?;
        migrate(&mut write)?;
        let read = Connection::open_with_flags(&uri, flags)?;
        read.busy_timeout(BUSY_TIMEOUT)?;
        // 共享缓存下读连接不阻塞于写事务的表锁
        read.pragma_update(None, "read_uncommitted", true)?;
        read.pragma_update(None, "query_only", true)?;
        Ok(Db {
            write: Mutex::new(write),
            read: Mutex::new(read),
        })
    }

    /// 所有写入的唯一入口（批量写入在闭包内自行开事务）。
    pub(crate) fn write<T>(
        &self,
        f: impl FnOnce(&mut Connection) -> Result<T, DbError>,
    ) -> Result<T, DbError> {
        let mut conn = lock(&self.write)?;
        f(&mut conn)
    }

    pub(crate) fn read<T>(
        &self,
        f: impl FnOnce(&Connection) -> Result<T, DbError>,
    ) -> Result<T, DbError> {
        let conn = lock(&self.read)?;
        f(&conn)
    }

    #[cfg(test)]
    pub(crate) fn raw_conn_for_test(&self) -> MutexGuard<'_, Connection> {
        self.read.lock().expect("test lock")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn db_is_send_sync() {
        fn assert<T: Send + Sync>() {}
        assert::<Db>();
    }
}
