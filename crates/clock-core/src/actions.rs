//! Turns a parsed [`Command`] into a store change, with an UNDO.
//!
//! The command bar, the assistant's tool calls (Phase 3) and voice (Phase 4) all go
//! through [`apply`], so every path behaves the same and can be undone the same way.

use crate::command::{Command, TimerTarget};
use crate::model::{days_label, duration_label, time12, Alarm, Reminder, Timer};
use crate::store::{Result, Store};
use chrono::{DateTime, NaiveDateTime, Utc};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Undo {
    DeleteTimer(i64),
    DeleteAlarm(i64),
    DeleteReminder(i64),
    RestoreTimers(Vec<Timer>),
    RestoreAlarm(Alarm),
    RestoreReminder(Reminder),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Applied {
    /// Short uppercase confirmation for the command bar / transcript.
    pub summary: String,
    pub undo: Option<Undo>,
    /// The caller should play the briefing (Phase 4).
    pub brief: bool,
}

impl Applied {
    fn new(summary: String, undo: Option<Undo>) -> Self {
        Self {
            summary,
            undo,
            brief: false,
        }
    }
}

/// `now_local` is the same instant as `now_utc`, as local wall-clock time.
pub fn apply(
    store: &Store,
    cmd: Command,
    now_utc: DateTime<Utc>,
    now_local: NaiveDateTime,
) -> Result<Applied> {
    Ok(match cmd {
        Command::StartTimer { name, duration } => {
            let name = name.unwrap_or_else(|| duration_label(duration));
            let t = store.add_timer(&name, duration, now_utc)?;
            Applied::new(
                format!(
                    "TIMER · {} · {}",
                    t.name.to_uppercase(),
                    duration_label(duration)
                ),
                Some(Undo::DeleteTimer(t.id)),
            )
        }
        Command::AddAlarm { time, days, label } => {
            let a = store.add_alarm(time, days, label.as_deref().unwrap_or(""), now_local)?;
            let label = if a.label.is_empty() {
                String::new()
            } else {
                format!(" · {}", a.label.to_uppercase())
            };
            Applied::new(
                format!("ALARM · {} · {}{label}", time12(time), days_label(days)),
                Some(Undo::DeleteAlarm(a.id)),
            )
        }
        Command::AddReminder { text, date, time } => {
            let r = store.add_reminder(&text, date, time, now_utc)?;
            let when = match (date, time) {
                (Some(d), Some(t)) => format!(" · {} {}", day_word(d, now_local), time12(t)),
                (Some(d), None) => format!(" · {}", day_word(d, now_local)),
                _ => String::new(),
            };
            Applied::new(
                format!("REMINDER · {}{when}", text.to_uppercase()),
                Some(Undo::DeleteReminder(r.id)),
            )
        }
        Command::CancelTimer { target } => {
            let timers = store.timers()?;
            let chosen: Vec<&Timer> = match &target {
                TimerTarget::All => timers.iter().collect(),
                TimerTarget::Latest => timers.iter().max_by_key(|t| t.id).into_iter().collect(),
                TimerTarget::Named(n) => {
                    let n = n.to_lowercase();
                    let exact: Vec<_> = timers
                        .iter()
                        .filter(|t| t.name.to_lowercase() == n)
                        .collect();
                    if exact.is_empty() {
                        timers
                            .iter()
                            .filter(|t| t.name.to_lowercase().contains(&n))
                            .collect()
                    } else {
                        exact
                    }
                }
            };
            if chosen.is_empty() {
                let what = match target {
                    TimerTarget::Named(n) => format!("NO TIMER NAMED {}", n.to_uppercase()),
                    _ => "NO TIMERS RUNNING".into(),
                };
                return Ok(Applied::new(what, None));
            }
            let mut removed = Vec::new();
            for t in chosen {
                removed.extend(store.delete_timer(t.id)?);
            }
            let summary = match removed.as_slice() {
                [one] => format!("CANCELLED · {}", one.name.to_uppercase()),
                many => format!("CANCELLED · {} TIMERS", many.len()),
            };
            Applied::new(summary, Some(Undo::RestoreTimers(removed)))
        }
        Command::Brief => Applied {
            summary: "BRIEFING".into(),
            undo: None,
            brief: true,
        },
    })
}

pub fn undo(store: &Store, undo: &Undo) -> Result<()> {
    match undo {
        Undo::DeleteTimer(id) => drop(store.delete_timer(*id)?),
        Undo::DeleteAlarm(id) => drop(store.delete_alarm(*id)?),
        Undo::DeleteReminder(id) => drop(store.delete_reminder(*id)?),
        Undo::RestoreTimers(ts) => ts.iter().try_for_each(|t| store.restore_timer(t))?,
        Undo::RestoreAlarm(a) => store.restore_alarm(a)?,
        Undo::RestoreReminder(r) => store.restore_reminder(r)?,
    }
    Ok(())
}

/// "TODAY", "TOMORROW", "MON 16 NOV".
pub fn day_word(d: chrono::NaiveDate, now_local: NaiveDateTime) -> String {
    let today = now_local.date();
    if d == today {
        "TODAY".into()
    } else if d == today + chrono::Duration::days(1) {
        "TOMORROW".into()
    } else {
        d.format("%a %-d %b").to_string().to_uppercase()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::parse;
    use chrono::{NaiveDate, TimeZone};

    fn now() -> (DateTime<Utc>, NaiveDateTime) {
        let local = NaiveDate::from_ymd_opt(2026, 11, 13)
            .unwrap()
            .and_hms_opt(10, 30, 0)
            .unwrap();
        (Utc.from_utc_datetime(&local), local)
    }

    fn run(store: &Store, input: &str) -> Applied {
        let (u, l) = now();
        apply(store, parse(input, l).expect("parses"), u, l).unwrap()
    }

    #[test]
    fn timer_apply_and_undo() {
        let s = Store::open_in_memory().unwrap();
        let a = run(&s, "tea timer 4m");
        assert_eq!(a.summary, "TIMER · TEA · 4 MIN");
        assert_eq!(s.timers().unwrap().len(), 1);
        undo(&s, a.undo.as_ref().unwrap()).unwrap();
        assert!(s.timers().unwrap().is_empty());

        let b = run(&s, "timer 20m");
        assert_eq!(s.timers().unwrap()[0].name, "20 MIN");
        assert_eq!(b.summary, "TIMER · 20 MIN · 20 MIN");
    }

    #[test]
    fn cancel_and_restore() {
        let s = Store::open_in_memory().unwrap();
        run(&s, "tea timer 4m");
        run(&s, "pasta timer 9m");
        run(&s, "egg timer 7m");

        let c = run(&s, "cancel tea timer");
        assert_eq!(c.summary, "CANCELLED · TEA");
        assert_eq!(s.timers().unwrap().len(), 2);

        let latest = run(&s, "cancel timer");
        assert_eq!(latest.summary, "CANCELLED · EGG");

        let all = run(&s, "stop all timers");
        assert_eq!(all.summary, "CANCELLED · PASTA");
        assert!(s.timers().unwrap().is_empty());

        undo(&s, c.undo.as_ref().unwrap()).unwrap();
        undo(&s, latest.undo.as_ref().unwrap()).unwrap();
        let names: Vec<_> = s.timers().unwrap().into_iter().map(|t| t.name).collect();
        assert_eq!(names, ["Tea", "Egg"]);

        assert_eq!(run(&s, "cancel rice timer").summary, "NO TIMER NAMED RICE");
    }

    #[test]
    fn alarm_and_reminder_summaries() {
        let s = Store::open_in_memory().unwrap();
        assert_eq!(
            run(&s, "alarm 6:45 weekdays").summary,
            "ALARM · 6:45 AM · WEEKDAYS"
        );
        assert_eq!(
            run(&s, "alarm 5:30am gym mon wed").summary,
            "ALARM · 5:30 AM · MON WED · GYM"
        );
        assert_eq!(
            run(&s, "remind me at 5 to check the mail").summary,
            "REMINDER · CHECK THE MAIL · TODAY 5:00 PM"
        );
        assert_eq!(
            run(&s, "remind me on monday to file taxes").summary,
            "REMINDER · FILE TAXES · MON 16 NOV"
        );
        let r = run(&s, "remind me to renew car registration");
        undo(&s, r.undo.as_ref().unwrap()).unwrap();
        assert_eq!(s.reminders().unwrap().len(), 2);
        assert!(run(&s, "brief me").brief);
    }
}
