//! SQLite storage (DEVELOPMENT.md section 8).
//!
//! Every method that depends on the clock takes `now` explicitly so tests are
//! deterministic. Text columns: wall-clock times "HH:MM", dates "YYYY-MM-DD";
//! UTC instants use rusqlite's chrono encoding.

use crate::model::{
    local_to_utc, next_fire_local, Alarm, CachedNewsStory, ChatMessage, Conversation,
    GoogleTaskUpdate, Reminder, Source, Timer,
};
use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, Utc};
use rusqlite::{params, types::Type, Connection, OptionalExtension, Row};
use std::path::{Path, PathBuf};
use std::time::Duration;

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
    // 2: timers, alarms, reminders
    "CREATE TABLE alarms (
        id            INTEGER PRIMARY KEY,
        time_local    TEXT    NOT NULL,
        days          INTEGER NOT NULL DEFAULT 127,
        label         TEXT    NOT NULL DEFAULT '',
        enabled       INTEGER NOT NULL DEFAULT 1,
        sound         TEXT    NOT NULL DEFAULT 'chime',
        next_fire_utc TEXT
     );
     CREATE TABLE reminders (
        id             INTEGER PRIMARY KEY,
        text           TEXT    NOT NULL,
        due_date       TEXT,
        due_time       TEXT,
        done           INTEGER NOT NULL DEFAULT 0,
        source         TEXT    NOT NULL DEFAULT 'local',
        google_task_id TEXT,
        google_list_id TEXT,
        google_etag    TEXT,
        updated_utc    TEXT    NOT NULL
     );
     CREATE TABLE timers (
        id             INTEGER PRIMARY KEY,
        name           TEXT    NOT NULL,
        total_ms       INTEGER NOT NULL,
        ends_utc       TEXT,
        paused_left_ms INTEGER
     );",
    // 3: assistant conversations
    "CREATE TABLE conversations (
        id          INTEGER PRIMARY KEY,
        title       TEXT    NOT NULL,
        mode        TEXT    NOT NULL DEFAULT 'local',
        created_utc TEXT    NOT NULL,
        updated_utc TEXT    NOT NULL
     );
     CREATE TABLE messages (
        id              INTEGER PRIMARY KEY,
        conversation_id INTEGER NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
        role            TEXT    NOT NULL,
        content         TEXT    NOT NULL,
        route           TEXT    NOT NULL DEFAULT '',
        cloud           INTEGER NOT NULL DEFAULT 0,
        actions         TEXT    NOT NULL DEFAULT '',
        created_utc     TEXT    NOT NULL
     );
     CREATE INDEX messages_by_conversation ON messages(conversation_id, id);",
    // 4: keep Google task deletions until the next successful sync.
    "CREATE TABLE google_task_tombstones (
        task_id     TEXT PRIMARY KEY,
        list_id     TEXT NOT NULL,
        deleted_utc TEXT NOT NULL
     );",
    // 5: initially keep up to three News stories for offline display.
    "CREATE TABLE news_cache (
        position      INTEGER PRIMARY KEY CHECK(position BETWEEN 0 AND 2),
        title         TEXT NOT NULL,
        source        TEXT NOT NULL,
        url           TEXT NOT NULL,
        published_utc TEXT,
        fetched_utc   TEXT NOT NULL
     );",
    // 6: expand the News cache from three to five stories.
    "ALTER TABLE news_cache RENAME TO news_cache_v5;
     CREATE TABLE news_cache (
        position      INTEGER PRIMARY KEY CHECK(position BETWEEN 0 AND 4),
        title         TEXT NOT NULL,
        source        TEXT NOT NULL,
        url           TEXT NOT NULL,
        published_utc TEXT,
        fetched_utc   TEXT NOT NULL
     );
     INSERT INTO news_cache
     SELECT position, title, source, url, published_utc, fetched_utc
     FROM news_cache_v5 ORDER BY position;
     DROP TABLE news_cache_v5;",
];

const TIME_FMT: &str = "%H:%M";
const DATE_FMT: &str = "%Y-%m-%d";

fn conv_err(idx: usize, e: impl std::error::Error + Send + Sync + 'static) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(idx, Type::Text, Box::new(e))
}

fn get_time(row: &Row, idx: usize) -> rusqlite::Result<Option<NaiveTime>> {
    row.get::<_, Option<String>>(idx)?
        .map(|s| NaiveTime::parse_from_str(&s, TIME_FMT).map_err(|e| conv_err(idx, e)))
        .transpose()
}

fn get_date(row: &Row, idx: usize) -> rusqlite::Result<Option<NaiveDate>> {
    row.get::<_, Option<String>>(idx)?
        .map(|s| NaiveDate::parse_from_str(&s, DATE_FMT).map_err(|e| conv_err(idx, e)))
        .transpose()
}

fn ms(d: Duration) -> i64 {
    i64::try_from(d.as_millis()).unwrap_or(i64::MAX)
}

fn from_ms(v: i64) -> Duration {
    Duration::from_millis(v.max(0) as u64)
}

fn utc_after(now: DateTime<Utc>, d: Duration) -> DateTime<Utc> {
    now + chrono::Duration::from_std(d).unwrap_or(chrono::Duration::MAX)
}

const TIMER_COLS: &str = "id, name, total_ms, ends_utc, paused_left_ms";
const ALARM_COLS: &str = "id, time_local, days, label, enabled, sound, next_fire_utc";
const REMINDER_COLS: &str = "id, text, due_date, due_time, done, source, google_task_id, \
                             google_list_id, google_etag, updated_utc";

fn timer_row(r: &Row) -> rusqlite::Result<Timer> {
    Ok(Timer {
        id: r.get(0)?,
        name: r.get(1)?,
        total: from_ms(r.get(2)?),
        ends_utc: r.get(3)?,
        paused_left: r.get::<_, Option<i64>>(4)?.map(from_ms),
    })
}

fn alarm_row(r: &Row) -> rusqlite::Result<Alarm> {
    Ok(Alarm {
        id: r.get(0)?,
        time_local: get_time(r, 1)?.ok_or_else(|| conv_err(1, std::fmt::Error))?,
        days: r.get(2)?,
        label: r.get(3)?,
        enabled: r.get(4)?,
        sound: r.get(5)?,
        next_fire_utc: r.get(6)?,
    })
}

fn reminder_row(r: &Row) -> rusqlite::Result<Reminder> {
    Ok(Reminder {
        id: r.get(0)?,
        text: r.get(1)?,
        due_date: get_date(r, 2)?,
        due_time: get_time(r, 3)?,
        done: r.get(4)?,
        source: Source::parse(&r.get::<_, String>(5)?),
        google_task_id: r.get(6)?,
        google_list_id: r.get(7)?,
        google_etag: r.get(8)?,
        updated_utc: r.get(9)?,
    })
}

const MESSAGE_COLS: &str = "id, conversation_id, role, content, route, cloud, actions, created_utc";

fn conversation_row(r: &Row) -> rusqlite::Result<Conversation> {
    Ok(Conversation {
        id: r.get(0)?,
        title: r.get(1)?,
        mode: r.get(2)?,
        created_utc: r.get(3)?,
        updated_utc: r.get(4)?,
    })
}

fn message_row(r: &Row) -> rusqlite::Result<ChatMessage> {
    Ok(ChatMessage {
        id: r.get(0)?,
        conversation_id: r.get(1)?,
        role: r.get(2)?,
        content: r.get(3)?,
        route: r.get(4)?,
        cloud: r.get(5)?,
        actions: r.get(6)?,
        created_utc: r.get(7)?,
    })
}

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
        conn.pragma_update(None, "foreign_keys", true)?;
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

    // -----------------------------------------------------------------------
    // Settings

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

    pub fn delete_setting(&self, key: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM settings WHERE key = ?1", [key])?;
        Ok(())
    }

    /// A setting parsed as `T`, or `default` if missing or unparsable.
    pub fn setting_or<T: std::str::FromStr>(&self, key: &str, default: T) -> Result<T> {
        Ok(self
            .get_setting(key)?
            .and_then(|v| v.parse().ok())
            .unwrap_or(default))
    }

    // -----------------------------------------------------------------------
    // Timers

    pub fn add_timer(&self, name: &str, total: Duration, now: DateTime<Utc>) -> Result<Timer> {
        self.conn.execute(
            "INSERT INTO timers (name, total_ms, ends_utc) VALUES (?1, ?2, ?3)",
            params![name, ms(total), utc_after(now, total)],
        )?;
        Ok(self
            .timer(self.conn.last_insert_rowid())?
            .expect("just inserted"))
    }

    pub fn timer(&self, id: i64) -> Result<Option<Timer>> {
        Ok(self
            .conn
            .query_row(
                &format!("SELECT {TIMER_COLS} FROM timers WHERE id = ?1"),
                [id],
                timer_row,
            )
            .optional()?)
    }

    /// Oldest first.
    pub fn timers(&self) -> Result<Vec<Timer>> {
        let mut st = self
            .conn
            .prepare(&format!("SELECT {TIMER_COLS} FROM timers ORDER BY id"))?;
        let rows = st
            .query_map([], timer_row)?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    pub fn pause_timer(&self, id: i64, now: DateTime<Utc>) -> Result<()> {
        if let Some(t) = self.timer(id)?.filter(|t| !t.is_paused()) {
            self.conn.execute(
                "UPDATE timers SET ends_utc = NULL, paused_left_ms = ?2 WHERE id = ?1",
                params![id, ms(t.remaining(now))],
            )?;
        }
        Ok(())
    }

    pub fn resume_timer(&self, id: i64, now: DateTime<Utc>) -> Result<()> {
        if let Some(t) = self.timer(id)?.filter(Timer::is_paused) {
            self.conn.execute(
                "UPDATE timers SET ends_utc = ?2, paused_left_ms = NULL WHERE id = ?1",
                params![id, utc_after(now, t.remaining(now))],
            )?;
        }
        Ok(())
    }

    /// +1 MIN. On a finished timer this counts from now.
    pub fn add_timer_time(&self, id: i64, extra: Duration, now: DateTime<Utc>) -> Result<()> {
        let Some(t) = self.timer(id)? else {
            return Ok(());
        };
        let total = ms(t.total) + ms(extra);
        match t.ends_utc {
            Some(end) => self.conn.execute(
                "UPDATE timers SET ends_utc = ?2, total_ms = ?3 WHERE id = ?1",
                params![id, utc_after(end.max(now), extra), total],
            )?,
            None => self.conn.execute(
                "UPDATE timers SET paused_left_ms = ?2, total_ms = ?3 WHERE id = ?1",
                params![id, ms(t.remaining(now) + extra), total],
            )?,
        };
        Ok(())
    }

    pub fn restart_timer(&self, id: i64, now: DateTime<Utc>) -> Result<()> {
        if let Some(t) = self.timer(id)? {
            self.conn.execute(
                "UPDATE timers SET ends_utc = ?2, paused_left_ms = NULL WHERE id = ?1",
                params![id, utc_after(now, t.total)],
            )?;
        }
        Ok(())
    }

    /// Returns the deleted timer (for UNDO).
    pub fn delete_timer(&self, id: i64) -> Result<Option<Timer>> {
        let t = self.timer(id)?;
        self.conn
            .execute("DELETE FROM timers WHERE id = ?1", [id])?;
        Ok(t)
    }

    /// Re-insert a deleted timer with its original id and end time.
    pub fn restore_timer(&self, t: &Timer) -> Result<()> {
        self.conn.execute(
            &format!("INSERT OR REPLACE INTO timers ({TIMER_COLS}) VALUES (?1, ?2, ?3, ?4, ?5)"),
            params![t.id, t.name, ms(t.total), t.ends_utc, t.paused_left.map(ms)],
        )?;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Alarms

    pub fn add_alarm(
        &self,
        time: NaiveTime,
        days: u8,
        label: &str,
        now: NaiveDateTime,
    ) -> Result<Alarm> {
        let sound = self
            .get_setting("alarm_sound")?
            .unwrap_or_else(|| "chime".into());
        let next = local_to_utc(next_fire_local(time, days, now));
        self.conn.execute(
            "INSERT INTO alarms (time_local, days, label, enabled, sound, next_fire_utc)
             VALUES (?1, ?2, ?3, 1, ?4, ?5)",
            params![time.format(TIME_FMT).to_string(), days, label, sound, next],
        )?;
        Ok(self
            .alarm(self.conn.last_insert_rowid())?
            .expect("just inserted"))
    }

    pub fn alarm(&self, id: i64) -> Result<Option<Alarm>> {
        Ok(self
            .conn
            .query_row(
                &format!("SELECT {ALARM_COLS} FROM alarms WHERE id = ?1"),
                [id],
                alarm_row,
            )
            .optional()?)
    }

    /// Ordered by time of day.
    pub fn alarms(&self) -> Result<Vec<Alarm>> {
        let mut st = self.conn.prepare(&format!(
            "SELECT {ALARM_COLS} FROM alarms ORDER BY time_local, id"
        ))?;
        let rows = st
            .query_map([], alarm_row)?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    pub fn set_alarm_enabled(&self, id: i64, enabled: bool, now: NaiveDateTime) -> Result<()> {
        let Some(a) = self.alarm(id)? else {
            return Ok(());
        };
        let next = enabled.then(|| local_to_utc(next_fire_local(a.time_local, a.days, now)));
        self.conn.execute(
            "UPDATE alarms SET enabled = ?2, next_fire_utc = ?3 WHERE id = ?1",
            params![id, enabled, next],
        )?;
        Ok(())
    }

    /// After an alarm rings: one-off alarms switch off, repeating ones move to the next day.
    pub fn alarm_fired(&self, id: i64, now: NaiveDateTime) -> Result<()> {
        let Some(a) = self.alarm(id)? else {
            return Ok(());
        };
        if a.days == 0 {
            self.conn.execute(
                "UPDATE alarms SET enabled = 0, next_fire_utc = NULL WHERE id = ?1",
                [id],
            )?;
        } else {
            let next = local_to_utc(next_fire_local(a.time_local, a.days, now));
            self.conn.execute(
                "UPDATE alarms SET next_fire_utc = ?2 WHERE id = ?1",
                params![id, next],
            )?;
        }
        Ok(())
    }

    /// Enabled alarms whose next fire time has arrived.
    pub fn due_alarms(&self, now: DateTime<Utc>) -> Result<Vec<Alarm>> {
        let mut st = self.conn.prepare(&format!(
            "SELECT {ALARM_COLS} FROM alarms
             WHERE enabled = 1 AND next_fire_utc IS NOT NULL AND next_fire_utc <= ?1
             ORDER BY next_fire_utc"
        ))?;
        let rows = st
            .query_map([now], alarm_row)?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    pub fn delete_alarm(&self, id: i64) -> Result<Option<Alarm>> {
        let a = self.alarm(id)?;
        self.conn
            .execute("DELETE FROM alarms WHERE id = ?1", [id])?;
        Ok(a)
    }

    pub fn restore_alarm(&self, a: &Alarm) -> Result<()> {
        self.conn.execute(
            &format!(
                "INSERT OR REPLACE INTO alarms ({ALARM_COLS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)"
            ),
            params![
                a.id,
                a.time_local.format(TIME_FMT).to_string(),
                a.days,
                a.label,
                a.enabled,
                a.sound,
                a.next_fire_utc
            ],
        )?;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Reminders

    pub fn add_reminder(
        &self,
        text: &str,
        date: Option<NaiveDate>,
        time: Option<NaiveTime>,
        now: DateTime<Utc>,
    ) -> Result<Reminder> {
        self.conn.execute(
            "INSERT INTO reminders (text, due_date, due_time, updated_utc) VALUES (?1, ?2, ?3, ?4)",
            params![
                text,
                date.map(|d| d.format(DATE_FMT).to_string()),
                time.map(|t| t.format(TIME_FMT).to_string()),
                now
            ],
        )?;
        Ok(self
            .reminder(self.conn.last_insert_rowid())?
            .expect("just inserted"))
    }

    pub fn reminder(&self, id: i64) -> Result<Option<Reminder>> {
        Ok(self
            .conn
            .query_row(
                &format!("SELECT {REMINDER_COLS} FROM reminders WHERE id = ?1"),
                [id],
                reminder_row,
            )
            .optional()?)
    }

    pub fn reminder_by_google_task(&self, task_id: &str) -> Result<Option<Reminder>> {
        Ok(self
            .conn
            .query_row(
                &format!("SELECT {REMINDER_COLS} FROM reminders WHERE google_task_id = ?1"),
                [task_id],
                reminder_row,
            )
            .optional()?)
    }

    /// Open first; then by due date (undated last), due time, and creation order.
    pub fn reminders(&self) -> Result<Vec<Reminder>> {
        let mut st = self.conn.prepare(&format!(
            "SELECT {REMINDER_COLS} FROM reminders
             ORDER BY done, due_date IS NULL, due_date, due_time IS NULL, due_time, id"
        ))?;
        let rows = st
            .query_map([], reminder_row)?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    pub fn set_reminder_done(&self, id: i64, done: bool, now: DateTime<Utc>) -> Result<()> {
        self.conn.execute(
            "UPDATE reminders SET done = ?2, updated_utc = ?3 WHERE id = ?1",
            params![id, done, now],
        )?;
        Ok(())
    }

    pub fn update_reminder(
        &self,
        id: i64,
        text: &str,
        due_date: Option<NaiveDate>,
        done: bool,
        now: DateTime<Utc>,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE reminders
             SET text = ?2, due_date = ?3, done = ?4, updated_utc = ?5
             WHERE id = ?1 AND due_time IS NULL",
            params![
                id,
                text,
                due_date.map(|d| d.format(DATE_FMT).to_string()),
                done,
                now
            ],
        )?;
        Ok(())
    }

    pub fn set_google_task(
        &self,
        id: i64,
        task_id: &str,
        list_id: &str,
        etag: Option<&str>,
        updated: DateTime<Utc>,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE reminders
             SET source = 'google', google_task_id = ?2, google_list_id = ?3,
                 google_etag = ?4, updated_utc = ?5
             WHERE id = ?1 AND due_time IS NULL",
            params![id, task_id, list_id, etag, updated],
        )?;
        Ok(())
    }

    pub fn update_google_reminder(&self, id: i64, update: GoogleTaskUpdate<'_>) -> Result<()> {
        self.conn.execute(
            "UPDATE reminders
             SET text = ?2, due_date = ?3, done = ?4, source = 'google',
                 google_task_id = ?5, google_list_id = ?6, google_etag = ?7,
                 updated_utc = ?8
             WHERE id = ?1 AND due_time IS NULL",
            params![
                id,
                update.text,
                update.due_date.map(|d| d.format(DATE_FMT).to_string()),
                update.done,
                update.task_id,
                update.list_id,
                update.etag,
                update.updated_utc
            ],
        )?;
        Ok(())
    }

    pub fn add_google_reminder(&self, update: GoogleTaskUpdate<'_>) -> Result<Reminder> {
        self.conn.execute(
            "INSERT INTO reminders
             (text, due_date, done, source, google_task_id, google_list_id, google_etag, updated_utc)
             VALUES (?1, ?2, ?3, 'google', ?4, ?5, ?6, ?7)",
            params![
                update.text,
                update.due_date.map(|d| d.format(DATE_FMT).to_string()),
                update.done,
                update.task_id,
                update.list_id,
                update.etag,
                update.updated_utc
            ],
        )?;
        Ok(self
            .reminder(self.conn.last_insert_rowid())?
            .expect("just inserted"))
    }

    pub fn delete_reminder(&self, id: i64) -> Result<Option<Reminder>> {
        let r = self.reminder(id)?;
        if let Some(task_id) = r.as_ref().and_then(|r| r.google_task_id.as_deref()) {
            let list_id = r
                .as_ref()
                .and_then(|r| r.google_list_id.as_deref())
                .unwrap_or_default();
            self.conn.execute(
                "INSERT INTO google_task_tombstones (task_id, list_id, deleted_utc)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(task_id) DO UPDATE SET list_id = excluded.list_id,
                     deleted_utc = excluded.deleted_utc",
                params![task_id, list_id, Utc::now()],
            )?;
        }
        self.conn
            .execute("DELETE FROM reminders WHERE id = ?1", [id])?;
        Ok(r)
    }

    pub fn delete_google_reminder(&self, id: i64) -> Result<()> {
        self.conn
            .execute("DELETE FROM reminders WHERE id = ?1", [id])?;
        Ok(())
    }

    pub fn google_task_tombstones(&self) -> Result<Vec<(String, String)>> {
        let mut st = self
            .conn
            .prepare("SELECT task_id, list_id FROM google_task_tombstones ORDER BY deleted_utc")?;
        let rows = st
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    pub fn clear_google_task_tombstone(&self, task_id: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM google_task_tombstones WHERE task_id = ?1",
            [task_id],
        )?;
        Ok(())
    }

    pub fn news_cache(&self) -> Result<Vec<CachedNewsStory>> {
        let mut st = self.conn.prepare(
            "SELECT title, source, url, published_utc, fetched_utc
             FROM news_cache ORDER BY position",
        )?;
        let rows = st
            .query_map([], |row| {
                Ok(CachedNewsStory {
                    title: row.get(0)?,
                    source: row.get(1)?,
                    url: row.get(2)?,
                    published_utc: row.get(3)?,
                    fetched_utc: row.get(4)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    pub fn save_news_cache(&self, stories: &[CachedNewsStory]) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute("DELETE FROM news_cache", [])?;
        for (position, story) in stories.iter().take(5).enumerate() {
            tx.execute(
                "INSERT INTO news_cache
                 (position, title, source, url, published_utc, fetched_utc)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    position as i64,
                    story.title,
                    story.source,
                    story.url,
                    story.published_utc,
                    story.fetched_utc
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn restore_reminder(&self, r: &Reminder) -> Result<()> {
        self.conn.execute(
            &format!(
                "INSERT OR REPLACE INTO reminders ({REMINDER_COLS})
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)"
            ),
            params![
                r.id,
                r.text,
                r.due_date.map(|d| d.format(DATE_FMT).to_string()),
                r.due_time.map(|t| t.format(TIME_FMT).to_string()),
                r.done,
                r.source.as_str(),
                r.google_task_id,
                r.google_list_id,
                r.google_etag,
                r.updated_utc
            ],
        )?;
        if let Some(task_id) = r.google_task_id.as_deref() {
            self.clear_google_task_tombstone(task_id)?;
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Assistant conversations

    pub fn create_conversation(
        &self,
        title: &str,
        mode: &str,
        now: DateTime<Utc>,
    ) -> Result<Conversation> {
        self.conn.execute(
            "INSERT INTO conversations (title, mode, created_utc, updated_utc) VALUES (?1, ?2, ?3, ?3)",
            params![title, mode, now],
        )?;
        Ok(self
            .conversation(self.conn.last_insert_rowid())?
            .expect("just inserted"))
    }

    pub fn conversation(&self, id: i64) -> Result<Option<Conversation>> {
        Ok(self
            .conn
            .query_row(
                "SELECT id, title, mode, created_utc, updated_utc FROM conversations WHERE id = ?1",
                [id],
                conversation_row,
            )
            .optional()?)
    }

    /// Most recently active first.
    pub fn conversations(&self) -> Result<Vec<Conversation>> {
        let mut st = self.conn.prepare(
            "SELECT id, title, mode, created_utc, updated_utc FROM conversations
             ORDER BY updated_utc DESC, id DESC",
        )?;
        let rows = st
            .query_map([], conversation_row)?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    pub fn set_conversation_mode(&self, id: i64, mode: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE conversations SET mode = ?2 WHERE id = ?1",
            params![id, mode],
        )?;
        Ok(())
    }

    /// Deletes the conversation and its messages.
    pub fn delete_conversation(&self, id: i64) -> Result<()> {
        self.conn
            .execute("DELETE FROM conversations WHERE id = ?1", [id])?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn add_message(
        &self,
        conversation_id: i64,
        role: &str,
        content: &str,
        route: &str,
        cloud: bool,
        actions: &str,
        now: DateTime<Utc>,
    ) -> Result<ChatMessage> {
        self.conn.execute(
            "INSERT INTO messages (conversation_id, role, content, route, cloud, actions, created_utc)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![conversation_id, role, content, route, cloud, actions, now],
        )?;
        let id = self.conn.last_insert_rowid();
        self.conn.execute(
            "UPDATE conversations SET updated_utc = ?2 WHERE id = ?1",
            params![conversation_id, now],
        )?;
        Ok(self.conn.query_row(
            &format!("SELECT {MESSAGE_COLS} FROM messages WHERE id = ?1"),
            [id],
            message_row,
        )?)
    }

    /// Oldest first.
    pub fn messages(&self, conversation_id: i64) -> Result<Vec<ChatMessage>> {
        let mut st = self.conn.prepare(&format!(
            "SELECT {MESSAGE_COLS} FROM messages WHERE conversation_id = ?1 ORDER BY id"
        ))?;
        let rows = st
            .query_map([conversation_id], message_row)?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::days::*;
    use chrono::TimeZone;

    fn utc(h: u32, m: u32, s: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 13, h, m, s).unwrap()
    }
    fn local(d: u32, h: u32, m: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 11, d)
            .unwrap()
            .and_hms_opt(h, m, 0)
            .unwrap()
    }
    fn t(h: u32, m: u32) -> NaiveTime {
        NaiveTime::from_hms_opt(h, m, 0).unwrap()
    }

    #[test]
    fn settings_round_trip() {
        let s = Store::open_in_memory().unwrap();
        assert_eq!(s.get_setting("theme").unwrap(), None);
        s.set_setting("theme", "dark").unwrap();
        s.set_setting("theme", "light").unwrap();
        assert_eq!(s.get_setting("theme").unwrap().as_deref(), Some("light"));
        s.set_setting("snooze_min", "9").unwrap();
        assert_eq!(s.setting_or("snooze_min", 5u32).unwrap(), 9);
        assert_eq!(s.setting_or("missing", 5u32).unwrap(), 5);
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

    #[test]
    fn timer_lifecycle() {
        let s = Store::open_in_memory().unwrap();
        let t0 = utc(12, 0, 0);
        let tea = s.add_timer("Tea", Duration::from_secs(240), t0).unwrap();
        assert_eq!(tea.remaining(t0), Duration::from_secs(240));

        // Pause at 1 min in, wait an hour, resume: 3 min left.
        s.pause_timer(tea.id, utc(12, 1, 0)).unwrap();
        let paused = s.timer(tea.id).unwrap().unwrap();
        assert!(paused.is_paused());
        assert_eq!(paused.remaining(utc(13, 0, 0)), Duration::from_secs(180));
        s.resume_timer(tea.id, utc(13, 0, 0)).unwrap();
        let resumed = s.timer(tea.id).unwrap().unwrap();
        assert_eq!(resumed.ends_utc, Some(utc(13, 3, 0)));

        // Finished, then +1 MIN counts from now.
        s.add_timer_time(tea.id, Duration::from_secs(60), utc(13, 10, 0))
            .unwrap();
        assert_eq!(
            s.timer(tea.id).unwrap().unwrap().ends_utc,
            Some(utc(13, 11, 0))
        );

        s.restart_timer(tea.id, utc(14, 0, 0)).unwrap();
        assert_eq!(
            s.timer(tea.id).unwrap().unwrap().ends_utc,
            Some(utc(14, 5, 0))
        );

        let deleted = s.delete_timer(tea.id).unwrap().unwrap();
        assert!(s.timers().unwrap().is_empty());
        s.restore_timer(&deleted).unwrap();
        assert_eq!(s.timers().unwrap(), vec![deleted]);
    }

    #[test]
    fn alarm_lifecycle() {
        let s = Store::open_in_memory().unwrap();
        // Friday 10:30 local.
        let once = s.add_alarm(t(19, 0), ONCE, "", local(13, 10, 30)).unwrap();
        let wk = s
            .add_alarm(t(7, 0), WEEKDAYS, "Work", local(13, 10, 30))
            .unwrap();
        assert_eq!(wk.next_fire_utc, Some(local_to_utc(local(16, 7, 0))));
        assert_eq!(
            s.alarms().unwrap().iter().map(|a| a.id).collect::<Vec<_>>(),
            [wk.id, once.id]
        );

        let fire = local_to_utc(local(13, 19, 0));
        assert!(s
            .due_alarms(fire - chrono::Duration::seconds(1))
            .unwrap()
            .is_empty());
        assert_eq!(s.due_alarms(fire).unwrap()[0].id, once.id);

        s.alarm_fired(once.id, local(13, 19, 0)).unwrap();
        let after = s.alarm(once.id).unwrap().unwrap();
        assert!(!after.enabled && after.next_fire_utc.is_none());

        s.alarm_fired(wk.id, local(16, 7, 0)).unwrap();
        assert_eq!(
            s.alarm(wk.id).unwrap().unwrap().next_fire_utc,
            Some(local_to_utc(local(17, 7, 0)))
        );

        s.set_alarm_enabled(wk.id, false, local(16, 8, 0)).unwrap();
        assert!(s
            .due_alarms(local_to_utc(local(30, 0, 0)))
            .unwrap()
            .is_empty());

        let gone = s.delete_alarm(wk.id).unwrap().unwrap();
        s.restore_alarm(&gone).unwrap();
        assert_eq!(s.alarm(wk.id).unwrap(), Some(gone));
    }

    #[test]
    fn reminder_order_and_done() {
        let s = Store::open_in_memory().unwrap();
        let now = utc(12, 0, 0);
        let d = |day| NaiveDate::from_ymd_opt(2026, 11, day);
        let undated = s
            .add_reminder("renew registration", None, None, now)
            .unwrap();
        let tomorrow = s.add_reminder("call the bank", d(14), None, now).unwrap();
        let today5 = s
            .add_reminder("check the mail", d(13), Some(t(17, 0)), now)
            .unwrap();
        let today9 = s
            .add_reminder("stretch", d(13), Some(t(9, 0)), now)
            .unwrap();

        let ids = |s: &Store| {
            s.reminders()
                .unwrap()
                .iter()
                .map(|r| r.id)
                .collect::<Vec<_>>()
        };
        assert_eq!(ids(&s), [today9.id, today5.id, tomorrow.id, undated.id]);

        s.set_reminder_done(today9.id, true, now).unwrap();
        assert_eq!(ids(&s), [today5.id, tomorrow.id, undated.id, today9.id]);

        assert!(undated.syncs_to_google() && !today5.syncs_to_google());
        assert_eq!(today5.fires_at(), Some(local(13, 17, 0)));

        let gone = s.delete_reminder(tomorrow.id).unwrap().unwrap();
        s.restore_reminder(&gone).unwrap();
        assert_eq!(s.reminder(tomorrow.id).unwrap(), Some(gone));
    }

    #[test]
    fn google_task_deletions_survive_until_sync_and_undo_clears_them() {
        let s = Store::open_in_memory().unwrap();
        let task = s
            .add_google_reminder(GoogleTaskUpdate {
                text: "renew registration",
                due_date: None,
                done: false,
                task_id: "remote-task",
                list_id: "default-list",
                etag: Some("etag"),
                updated_utc: utc(12, 0, 0),
            })
            .unwrap();

        let deleted = s.delete_reminder(task.id).unwrap().unwrap();
        assert_eq!(
            s.google_task_tombstones().unwrap(),
            [("remote-task".into(), "default-list".into())]
        );
        s.restore_reminder(&deleted).unwrap();
        assert!(s.google_task_tombstones().unwrap().is_empty());

        s.delete_reminder(task.id).unwrap();
        s.clear_google_task_tombstone("remote-task").unwrap();
        assert!(s.google_task_tombstones().unwrap().is_empty());
    }

    #[test]
    fn news_cache_replaces_stories_in_order_and_keeps_five() {
        let s = Store::open_in_memory().unwrap();
        let fetched = utc(12, 0, 0);
        let stories = (0..6)
            .map(|index| CachedNewsStory {
                title: format!("Story {index}"),
                source: "Example".into(),
                url: format!("https://example.test/{index}"),
                published_utc: Some(utc(11, index as u32, 0)),
                fetched_utc: fetched,
            })
            .collect::<Vec<_>>();

        s.save_news_cache(&stories).unwrap();
        let cached = s.news_cache().unwrap();
        assert_eq!(cached.len(), 5);
        assert_eq!(cached[0].title, "Story 0");
        assert_eq!(cached[4].title, "Story 4");
        assert_eq!(cached[0].fetched_utc, fetched);

        s.save_news_cache(&stories[1..2]).unwrap();
        assert_eq!(s.news_cache().unwrap(), stories[1..2]);
    }

    #[test]
    fn news_cache_migration_expands_capacity_and_preserves_cached_story() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE news_cache (
                position      INTEGER PRIMARY KEY CHECK(position BETWEEN 0 AND 2),
                title         TEXT NOT NULL,
                source        TEXT NOT NULL,
                url           TEXT NOT NULL,
                published_utc TEXT,
                fetched_utc   TEXT NOT NULL
             );
             PRAGMA user_version = 5;",
        )
        .unwrap();
        let fetched = utc(12, 0, 0);
        conn.execute(
            "INSERT INTO news_cache
             (position, title, source, url, published_utc, fetched_utc)
             VALUES (0, 'Cached', 'Example', 'https://example.test/old', ?1, ?2)",
            params![Some(utc(11, 0, 0)), fetched],
        )
        .unwrap();

        let s = Store::init(conn).unwrap();
        assert_eq!(s.news_cache().unwrap()[0].title, "Cached");
        let stories = (0..5)
            .map(|index| CachedNewsStory {
                title: format!("Story {index}"),
                source: "Example".into(),
                url: format!("https://example.test/{index}"),
                published_utc: None,
                fetched_utc: fetched,
            })
            .collect::<Vec<_>>();
        s.save_news_cache(&stories).unwrap();
        assert_eq!(s.news_cache().unwrap().len(), 5);
    }

    #[test]
    fn conversations_and_messages() {
        let s = Store::open_in_memory().unwrap();
        let a = s
            .create_conversation("Weather", "local", utc(9, 0, 0))
            .unwrap();
        let b = s
            .create_conversation("Alarms", "local", utc(9, 5, 0))
            .unwrap();
        assert_eq!(
            s.conversations()
                .unwrap()
                .iter()
                .map(|c| c.id)
                .collect::<Vec<_>>(),
            [b.id, a.id]
        );

        s.add_message(a.id, "user", "rain later?", "", false, "", utc(9, 10, 0))
            .unwrap();
        let reply = s
            .add_message(
                a.id,
                "assistant",
                "No rain today.",
                "LOCAL · LLAMA3.2:3B",
                false,
                "",
                utc(9, 10, 5),
            )
            .unwrap();
        assert_eq!(reply.role, "assistant");
        // A new message moves the conversation to the top.
        assert_eq!(s.conversations().unwrap()[0].id, a.id);
        assert_eq!(s.messages(a.id).unwrap().len(), 2);

        s.set_conversation_mode(a.id, "cloud").unwrap();
        assert_eq!(s.conversation(a.id).unwrap().unwrap().mode, "cloud");

        s.delete_conversation(a.id).unwrap();
        assert!(s.conversation(a.id).unwrap().is_none());
        assert!(s.messages(a.id).unwrap().is_empty(), "messages cascade");
    }
}
