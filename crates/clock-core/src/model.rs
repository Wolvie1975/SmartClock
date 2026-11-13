//! Domain types for timers, alarms and reminders, and the pure time logic around them.
//!
//! Wall-clock values (alarm times, reminder due dates) are local and naive. Instants
//! (timer end, alarm next fire) are UTC. Convert at the edges with [`local_to_utc`].

use chrono::{DateTime, Datelike, Local, NaiveDate, NaiveDateTime, NaiveTime, TimeZone, Utc};
use std::time::Duration;

// ---------------------------------------------------------------------------
// Timers

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Timer {
    pub id: i64,
    pub name: String,
    pub total: Duration,
    /// `None` while paused.
    pub ends_utc: Option<DateTime<Utc>>,
    /// Set while paused.
    pub paused_left: Option<Duration>,
}

impl Timer {
    pub fn is_paused(&self) -> bool {
        self.ends_utc.is_none()
    }

    pub fn remaining(&self, now: DateTime<Utc>) -> Duration {
        match self.ends_utc {
            Some(end) => (end - now).to_std().unwrap_or(Duration::ZERO),
            None => self.paused_left.unwrap_or(Duration::ZERO),
        }
    }

    pub fn is_finished(&self, now: DateTime<Utc>) -> bool {
        !self.is_paused() && self.remaining(now).is_zero()
    }

    /// 0.0 (just started) → 1.0 (done).
    pub fn progress(&self, now: DateTime<Utc>) -> f32 {
        if self.total.is_zero() {
            return 1.0;
        }
        1.0 - (self.remaining(now).as_secs_f32() / self.total.as_secs_f32()).clamp(0.0, 1.0)
    }
}

/// "20m", "1h 15m", "90s" → "20 MIN", "1 H 15 MIN", "1 MIN 30 S". Used for default timer names.
pub fn duration_label(d: Duration) -> String {
    let s = d.as_secs();
    let (h, m, sec) = (s / 3600, (s % 3600) / 60, s % 60);
    let mut parts = Vec::new();
    if h > 0 {
        parts.push(format!("{h} H"));
    }
    if m > 0 {
        parts.push(format!("{m} MIN"));
    }
    if sec > 0 || parts.is_empty() {
        parts.push(format!("{sec} S"));
    }
    parts.join(" ")
}

/// Countdown text: "4:59", "1:02:03".
pub fn countdown_label(d: Duration) -> String {
    let s = d.as_secs() + u64::from(d.subsec_nanos() > 0); // round up: never show 0:00 early
    let (h, m, sec) = (s / 3600, (s % 3600) / 60, s % 60);
    if h > 0 {
        format!("{h}:{m:02}:{sec:02}")
    } else {
        format!("{m}:{sec:02}")
    }
}

// ---------------------------------------------------------------------------
// Alarms

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Alarm {
    pub id: i64,
    pub time_local: NaiveTime,
    /// Mon = 1 … Sun = 64; 0 = one-off. See [`crate::command::days`].
    pub days: u8,
    pub label: String,
    pub enabled: bool,
    pub sound: String,
    pub next_fire_utc: Option<DateTime<Utc>>,
}

pub fn day_bit(date: NaiveDate) -> u8 {
    1 << date.weekday().num_days_from_monday()
}

/// The next local time strictly after `after` when an alarm with these settings rings.
pub fn next_fire_local(time: NaiveTime, days: u8, after: NaiveDateTime) -> NaiveDateTime {
    let days = days & 0x7F;
    (0..=7)
        .map(|off| (after.date() + chrono::Duration::days(off)).and_time(time))
        .find(|dt| *dt > after && (days == 0 || days & day_bit(dt.date()) != 0))
        .expect("a matching day exists within 8 days")
}

/// Local wall-clock → UTC. In a DST gap (the time doesn't exist) it moves forward an hour;
/// in an overlap it takes the earlier instant.
pub fn local_to_utc(local: NaiveDateTime) -> DateTime<Utc> {
    Local
        .from_local_datetime(&local)
        .earliest()
        .or_else(|| {
            Local
                .from_local_datetime(&(local + chrono::Duration::hours(1)))
                .earliest()
        })
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or_else(|| Utc.from_utc_datetime(&local))
}

/// "WEEKDAYS", "DAILY", "ONCE", "MON WED FRI".
pub fn days_label(days: u8) -> String {
    use crate::command::days::*;
    match days & 0x7F {
        ONCE => "ONCE".into(),
        EVERY_DAY => "DAILY".into(),
        WEEKDAYS => "WEEKDAYS".into(),
        WEEKENDS => "WEEKENDS".into(),
        d => ["MON", "TUE", "WED", "THU", "FRI", "SAT", "SUN"]
            .iter()
            .enumerate()
            .filter(|(i, _)| d & (1 << i) != 0)
            .map(|(_, n)| *n)
            .collect::<Vec<_>>()
            .join(" "),
    }
}

/// 12-hour "7:05 AM".
pub fn time12(t: NaiveTime) -> String {
    t.format("%-I:%M %p").to_string()
}

// ---------------------------------------------------------------------------
// Reminders

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Local,
    Google,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Local => "local",
            Source::Google => "google",
        }
    }
    pub fn parse(s: &str) -> Self {
        if s == "google" {
            Source::Google
        } else {
            Source::Local
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reminder {
    pub id: i64,
    pub text: String,
    pub due_date: Option<NaiveDate>,
    pub due_time: Option<NaiveTime>,
    pub done: bool,
    pub source: Source,
    pub google_task_id: Option<String>,
    pub google_list_id: Option<String>,
    pub google_etag: Option<String>,
    pub updated_utc: DateTime<Utc>,
}

impl Reminder {
    /// Timed reminders stay local; the Tasks API can't store a time of day (section 12.4).
    pub fn syncs_to_google(&self) -> bool {
        self.due_time.is_none()
    }

    /// When a timed reminder fires (local). Untimed reminders never fire.
    pub fn fires_at(&self) -> Option<NaiveDateTime> {
        Some(self.due_date?.and_time(self.due_time?))
    }
}

// ---------------------------------------------------------------------------
// Assistant conversations

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conversation {
    pub id: i64,
    pub title: String,
    /// "local" or "cloud".
    pub mode: String,
    pub created_utc: DateTime<Utc>,
    pub updated_utc: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatMessage {
    pub id: i64,
    pub conversation_id: i64,
    /// "user", "assistant" or "notice" (app messages, never sent to a model).
    pub role: String,
    pub content: String,
    /// "LOCAL · LLAMA3.2:3B", "BUILT-IN · NO AI USED", "CLOUD · SENT OFF DEVICE", …
    pub route: String,
    pub cloud: bool,
    /// What the reply changed, e.g. "TIMER · TEA · 4 MIN".
    pub actions: String,
    pub created_utc: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::days::*;

    fn dt(d: u32, h: u32, m: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 11, d)
            .unwrap()
            .and_hms_opt(h, m, 0)
            .unwrap()
    }
    fn t(h: u32, m: u32) -> NaiveTime {
        NaiveTime::from_hms_opt(h, m, 0).unwrap()
    }

    // 2026-11-13 is a Friday.

    #[test]
    fn next_fire_one_off() {
        assert_eq!(
            next_fire_local(t(19, 0), ONCE, dt(13, 10, 30)),
            dt(13, 19, 0)
        );
        assert_eq!(next_fire_local(t(7, 0), ONCE, dt(13, 10, 30)), dt(14, 7, 0));
        // exactly now → tomorrow
        assert_eq!(
            next_fire_local(t(10, 30), ONCE, dt(13, 10, 30)),
            dt(14, 10, 30)
        );
    }

    #[test]
    fn next_fire_repeating() {
        // Friday 10:30, weekdays 7:00 → Monday 16th.
        assert_eq!(
            next_fire_local(t(7, 0), WEEKDAYS, dt(13, 10, 30)),
            dt(16, 7, 0)
        );
        // Friday 6:00, weekdays 7:00 → today.
        assert_eq!(
            next_fire_local(t(7, 0), WEEKDAYS, dt(13, 6, 0)),
            dt(13, 7, 0)
        );
        // Weekends → Saturday.
        assert_eq!(
            next_fire_local(t(9, 0), WEEKENDS, dt(13, 6, 0)),
            dt(14, 9, 0)
        );
        // Only Friday, already past today → next Friday.
        assert_eq!(next_fire_local(t(7, 0), FRI, dt(13, 10, 30)), dt(20, 7, 0));
    }

    #[test]
    fn labels() {
        assert_eq!(days_label(WEEKDAYS), "WEEKDAYS");
        assert_eq!(days_label(MON | WED | FRI), "MON WED FRI");
        assert_eq!(days_label(ONCE), "ONCE");
        assert_eq!(time12(t(0, 5)), "12:05 AM");
        assert_eq!(time12(t(17, 30)), "5:30 PM");
        assert_eq!(duration_label(Duration::from_secs(75 * 60)), "1 H 15 MIN");
        assert_eq!(duration_label(Duration::from_secs(90)), "1 MIN 30 S");
        assert_eq!(countdown_label(Duration::from_millis(299_400)), "5:00");
        assert_eq!(countdown_label(Duration::from_secs(3723)), "1:02:03");
    }

    #[test]
    fn timer_remaining_and_progress() {
        let now = Utc.with_ymd_and_hms(2026, 11, 13, 12, 0, 0).unwrap();
        let mut timer = Timer {
            id: 1,
            name: "Tea".into(),
            total: Duration::from_secs(240),
            ends_utc: Some(now + chrono::Duration::seconds(60)),
            paused_left: None,
        };
        assert_eq!(timer.remaining(now), Duration::from_secs(60));
        assert!((timer.progress(now) - 0.75).abs() < 1e-6);
        assert!(!timer.is_finished(now));
        assert!(timer.is_finished(now + chrono::Duration::seconds(61)));

        timer.ends_utc = None;
        timer.paused_left = Some(Duration::from_secs(30));
        assert!(timer.is_paused());
        assert_eq!(
            timer.remaining(now + chrono::Duration::hours(5)),
            Duration::from_secs(30)
        );
    }
}
