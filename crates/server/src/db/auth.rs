//! 管理员账号、会话与设置。密码哈希 / 校验是 CPU 密集操作，调用方应放进 `spawn_blocking`。

use argon2::Argon2;
use argon2::password_hash::{PasswordHasher, PasswordVerifier};
use rusqlite::{OptionalExtension, params};
use std::sync::OnceLock;

use super::nodes::{generate_token, hash_token};
use super::{Db, DbError};

/// 会话有效期：30 天。
pub const SESSION_TTL_MS: i64 = 30 * 24 * 3_600_000;

const ALNUM: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";

fn random_password() -> String {
    (0..16)
        .map(|_| ALNUM[rand::random_range(..ALNUM.len())] as char)
        .collect()
}

fn hash_password(pw: &str) -> Result<String, DbError> {
    Argon2::default()
        .hash_password(pw.as_bytes())
        .map(|h| h.to_string())
        .map_err(|e| DbError::Hash(e.to_string()))
}

fn verify_hash(pw: &str, phc: &str) -> bool {
    Argon2::default()
        .verify_password(pw.as_bytes(), phc)
        .is_ok()
}

/// 用户名不存在时也跑一次等价的校验，避免耗时差异泄露用户名是否存在。
fn dummy_hash() -> &'static str {
    static DUMMY: OnceLock<String> = OnceLock::new();
    DUMMY.get_or_init(|| hash_password("minimon-dummy-password").unwrap_or_default())
}

impl Db {
    /// 管理员表为空时创建。未给 `preset` 则生成 16 位随机密码并返回明文；表非空返回 `None`。
    pub fn ensure_admin(
        &self,
        username: &str,
        preset: Option<&str>,
    ) -> Result<Option<String>, DbError> {
        let exists: i64 =
            self.read(|c| Ok(c.query_row("SELECT COUNT(*) FROM admin", [], |r| r.get(0))?))?;
        if exists > 0 {
            return Ok(None);
        }
        let (plain, generated) = match preset {
            Some(p) => (p.to_string(), false),
            None => (random_password(), true),
        };
        let hash = hash_password(&plain)?;
        let now = super::now_ms();
        let inserted = self.write(|c| {
            Ok(c.execute(
                "INSERT OR IGNORE INTO admin (id, username, password_hash, updated_ms) VALUES (1, ?1, ?2, ?3)",
                params![username, hash, now],
            )?)
        })?;
        Ok((inserted > 0 && generated).then_some(plain))
    }

    pub fn admin_username(&self) -> Result<String, DbError> {
        self.read(|c| Ok(c.query_row("SELECT username FROM admin WHERE id = 1", [], |r| r.get(0))?))
    }

    /// 用户名与密码都正确才返回 true；用户名错误时仍做一次哈希校验。
    pub fn verify_admin(&self, username: &str, password: &str) -> Result<bool, DbError> {
        let row: Option<(String, String)> = self.read(|c| {
            Ok(c.query_row(
                "SELECT username, password_hash FROM admin WHERE id = 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?)
        })?;
        match row {
            Some((name, phc)) => {
                let pw_ok = verify_hash(password, &phc);
                Ok(pw_ok && name == username)
            }
            None => {
                verify_hash(password, dummy_hash());
                Ok(false)
            }
        }
    }

    pub fn set_admin_password(&self, new: &str) -> Result<(), DbError> {
        self.change_admin_password(new, None)
    }

    /// 改密码；`keep` 给出时在同一事务里删除除该会话外的所有会话。
    pub fn change_admin_password(&self, new: &str, keep: Option<&str>) -> Result<(), DbError> {
        let hash = hash_password(new)?;
        let now = super::now_ms();
        self.write(|c| {
            let tx = c.transaction()?;
            tx.execute(
                "UPDATE admin SET password_hash = ?1, updated_ms = ?2 WHERE id = 1",
                params![hash, now],
            )?;
            if let Some(keep) = keep {
                tx.execute(
                    "DELETE FROM sessions WHERE token_hash <> ?1",
                    [hash_token(keep)],
                )?;
            }
            tx.commit()?;
            Ok(())
        })
    }

    /// 生成新随机密码并作废全部会话，返回明文。
    pub fn reset_admin_password(&self) -> Result<String, DbError> {
        let plain = random_password();
        let hash = hash_password(&plain)?;
        let now = super::now_ms();
        self.write(|c| {
            let tx = c.transaction()?;
            let n = tx.execute(
                "UPDATE admin SET password_hash = ?1, updated_ms = ?2 WHERE id = 1",
                params![hash, now],
            )?;
            if n == 0 {
                return Err(DbError::NoAdmin);
            }
            tx.execute("DELETE FROM sessions", [])?;
            tx.commit()?;
            Ok(())
        })?;
        Ok(plain)
    }

    /// 创建会话，返回明文 token（库里只存 sha256）。
    pub fn create_session(&self, now_ms: i64) -> Result<String, DbError> {
        let token = generate_token();
        let hash = hash_token(&token);
        self.write(|c| {
            c.execute(
                "INSERT INTO sessions (token_hash, created_ms, expires_ms) VALUES (?1, ?2, ?3)",
                params![hash, now_ms, now_ms + SESSION_TTL_MS],
            )?;
            Ok(())
        })?;
        Ok(token)
    }

    pub fn session_valid(&self, token: &str, now_ms: i64) -> Result<bool, DbError> {
        let hash = hash_token(token);
        self.read(|c| {
            Ok(c.query_row(
                "SELECT 1 FROM sessions WHERE token_hash = ?1 AND expires_ms > ?2",
                params![hash, now_ms],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
        })
    }

    pub fn delete_session(&self, token: &str) -> Result<(), DbError> {
        let hash = hash_token(token);
        self.write(|c| {
            c.execute("DELETE FROM sessions WHERE token_hash = ?1", [hash])?;
            Ok(())
        })
    }

    pub fn delete_sessions_except(&self, keep: &str) -> Result<(), DbError> {
        let hash = hash_token(keep);
        self.write(|c| {
            c.execute("DELETE FROM sessions WHERE token_hash <> ?1", [hash])?;
            Ok(())
        })
    }

    /// 删除已过期会话，返回删除条数。
    pub fn prune_sessions(&self, now_ms: i64) -> Result<usize, DbError> {
        self.write(|c| Ok(c.execute("DELETE FROM sessions WHERE expires_ms <= ?1", [now_ms])?))
    }

    pub fn get_setting(&self, key: &str) -> Result<Option<String>, DbError> {
        self.read(|c| {
            Ok(
                c.query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| {
                    r.get(0)
                })
                .optional()?,
            )
        })
    }

    pub fn set_setting(&self, key: &str, value: &str) -> Result<(), DbError> {
        self.write(|c| {
            c.execute(
                "INSERT INTO settings (key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![key, value],
            )?;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ensure_admin_generates_once() {
        let db = Db::open_in_memory().unwrap();
        let pw = db.ensure_admin("admin", None).unwrap().unwrap();
        assert_eq!(pw.len(), 16);
        assert!(pw.chars().all(|c| c.is_ascii_alphanumeric()));
        assert!(db.ensure_admin("admin", None).unwrap().is_none());
        assert!(db.verify_admin("admin", &pw).unwrap());
        assert!(!db.verify_admin("admin", "wrong").unwrap());
        assert!(!db.verify_admin("other", &pw).unwrap());
        assert_eq!(db.admin_username().unwrap(), "admin");

        let db = Db::open_in_memory().unwrap();
        assert!(
            db.ensure_admin("boss", Some("preset-pass"))
                .unwrap()
                .is_none()
        );
        assert!(db.verify_admin("boss", "preset-pass").unwrap());
        assert!(db.ensure_admin("x", Some("y")).unwrap().is_none());
        assert_eq!(db.admin_username().unwrap(), "boss");
    }

    #[test]
    fn password_stored_as_argon2id_phc() {
        let db = Db::open_in_memory().unwrap();
        db.ensure_admin("a", Some("preset-pass")).unwrap();
        let stored: String = db
            .raw_conn_for_test()
            .query_row("SELECT password_hash FROM admin", [], |r| r.get(0))
            .unwrap();
        assert!(stored.starts_with("$argon2id$"), "{stored}");
        assert!(!stored.contains("preset-pass"));
    }

    #[test]
    fn sessions_hashed_and_expire() {
        let db = Db::open_in_memory().unwrap();
        let t = db.create_session(1_000).unwrap();
        let stored: String = db
            .raw_conn_for_test()
            .query_row("SELECT token_hash FROM sessions", [], |r| r.get(0))
            .unwrap();
        assert_ne!(stored, t);
        assert_eq!(stored, hash_token(&t));
        assert!(db.session_valid(&t, 2_000).unwrap());
        assert!(!db.session_valid("other", 2_000).unwrap());
        let exp = 1_000 + SESSION_TTL_MS;
        assert!(db.session_valid(&t, exp - 1).unwrap());
        assert!(!db.session_valid(&t, exp).unwrap());
        assert_eq!(db.prune_sessions(exp - 1).unwrap(), 0);
        assert_eq!(db.prune_sessions(exp).unwrap(), 1);
        assert!(!db.session_valid(&t, 2_000).unwrap());
    }

    #[test]
    fn delete_session_and_except() {
        let db = Db::open_in_memory().unwrap();
        let (a, b, c) = (
            db.create_session(0).unwrap(),
            db.create_session(0).unwrap(),
            db.create_session(0).unwrap(),
        );
        db.delete_session(&c).unwrap();
        assert!(!db.session_valid(&c, 1).unwrap());
        db.delete_sessions_except(&a).unwrap();
        assert!(db.session_valid(&a, 1).unwrap());
        assert!(!db.session_valid(&b, 1).unwrap());
    }

    #[test]
    fn change_password_keeps_only_given_session() {
        let db = Db::open_in_memory().unwrap();
        db.ensure_admin("a", Some("old-password")).unwrap();
        let (a, b) = (db.create_session(0).unwrap(), db.create_session(0).unwrap());
        db.change_admin_password("new-password", Some(&a)).unwrap();
        assert!(db.session_valid(&a, 1).unwrap() && !db.session_valid(&b, 1).unwrap());
        assert!(db.verify_admin("a", "new-password").unwrap());
        assert!(!db.verify_admin("a", "old-password").unwrap());
    }

    #[test]
    fn reset_password_kills_sessions() {
        let db = Db::open_in_memory().unwrap();
        db.ensure_admin("a", Some("old-password")).unwrap();
        let t = db.create_session(0).unwrap();
        let pw = db.reset_admin_password().unwrap();
        assert_eq!(pw.len(), 16);
        assert!(!db.session_valid(&t, 1).unwrap());
        assert!(db.verify_admin("a", &pw).unwrap());
        assert!(!db.verify_admin("a", "old-password").unwrap());
    }

    #[test]
    fn reset_without_admin_errors() {
        let db = Db::open_in_memory().unwrap();
        assert!(matches!(db.reset_admin_password(), Err(DbError::NoAdmin)));
    }

    #[test]
    fn settings_roundtrip() {
        let db = Db::open_in_memory().unwrap();
        assert_eq!(db.get_setting("k").unwrap(), None);
        db.set_setting("k", "v1").unwrap();
        db.set_setting("k", "v2").unwrap();
        assert_eq!(db.get_setting("k").unwrap().as_deref(), Some("v2"));
    }
}
