//! SQLite storage. Phase 1 only needs the `settings` table; the rest of the
//! schema (DEVELOPMENT.md section 8) is added in Phase 2 as new migrations.

use rusqlite::{params, Connection, OptionalExtension};
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("database error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, StoreError>;

/// Ordered list of migrations. Append only; never edit an applied one.
const MIGRATIONS: &[&str] = &[
    // 1: settings
    "CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);",
];

pub struct Store {
    conn: Connection,
}

impl Store {
    /// `~/.local/share/smart-clock/clock.db` (respects `$XDG_DATA_HOME`).
    pub fn default_path() -> PathBuf {
        let base = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .unwrap_or_else(|| {
                PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/share")
            });
        base.join("smart-clock").join("clock.db")
    }

    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        Self::init(Connection::open(path)?)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        let mut store = Self { conn };
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&mut self) -> Result<()> {
        let version: i64 = self
            .conn
            .pragma_query_value(None, "user_version", |r| r.get(0))?;
        let tx = self.conn.transaction()?;
        for (i, sql) in MIGRATIONS.iter().enumerate().skip(version as usize) {
            tracing::info!(migration = i + 1, "applying migration");
            tx.execute_batch(sql)?;
        }
        tx.pragma_update(None, "user_version", MIGRATIONS.len() as i64)?;
        tx.commit()?;
        Ok(())
    }

    pub fn get_setting(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| {
                r.get(0)
            })
            .optional()?)
    }

    pub fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_round_trip() {
        let s = Store::open_in_memory().unwrap();
        assert_eq!(s.get_setting("theme").unwrap(), None);
        s.set_setting("theme", "dark").unwrap();
        s.set_setting("theme", "light").unwrap();
        assert_eq!(s.get_setting("theme").unwrap().as_deref(), Some("light"));
    }

    #[test]
    fn migrate_is_idempotent() {
        let dir = std::env::temp_dir().join(format!("smart-clock-test-{}", std::process::id()));
        let path = dir.join("clock.db");
        Store::open(&path).unwrap().set_setting("k", "v").unwrap();
        let s = Store::open(&path).unwrap();
        assert_eq!(s.get_setting("k").unwrap().as_deref(), Some("v"));
        let _ = std::fs::remove_dir_all(dir);
    }
}
