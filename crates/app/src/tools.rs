//! Tools the assistant's model can call (DEVELOPMENT.md section 10.2).
//!
//! Every action goes through `clock_core::actions::apply`, the same path as the
//! command bar, so it behaves the same and can be undone the same way. Times and
//! dates are text resolved by the offline parser's rules, and numbers may arrive
//! as strings (small models often send `"5"`), so arguments are read leniently.

use ai::{ToolCall, ToolSpec};
use chrono::{DateTime, Local, NaiveDateTime, Utc};
use clock_core::actions::{self, day_word, Applied, Undo};
use clock_core::command::{self, parse_clock_time, parse_date_text, Command, TimerTarget};
use clock_core::model::{countdown_label, days_label, next_fire_local, time12};
use clock_core::Store;
use serde_json::{json, Value};
use std::time::Duration;

pub struct Outcome {
    /// What the model reads back.
    pub result: String,
    /// Set when the clock changed (shown with UNDO).
    pub applied: Option<Applied>,
    /// A finished confirmation for the user. When every call in a step has one,
    /// the assistant shows these instead of asking the model to word a reply.
    pub say: Option<String>,
}

impl Outcome {
    fn info(text: impl Into<String>) -> Self {
        Self {
            result: text.into(),
            applied: None,
            say: None,
        }
    }
    fn error(text: impl Into<String>) -> Self {
        Self {
            say: None,
            result: format!("error: {}", text.into()),
            applied: None,
        }
    }
}

pub fn specs() -> Vec<ToolSpec> {
    let t = |name: &str, description: &str, parameters: Value| ToolSpec {
        name: name.into(),
        description: description.into(),
        parameters,
    };
    vec![
        t(
            "set_timer",
            "Start a countdown timer.",
            json!({
                "type": "object",
                "properties": {
                    "minutes": {"type": "number", "description": "Length in minutes (may be fractional)"},
                    "seconds": {"type": "number", "description": "Extra seconds, optional"},
                    "name": {"type": "string", "description": "Short label such as \"pasta\", optional"}
                },
                "required": ["minutes"]
            }),
        ),
        t(
            "cancel_timer",
            "Cancel a running timer by name, the most recent one, or all of them.",
            json!({
                "type": "object",
                "properties": {
                    "name": {"type": "string", "description": "Timer label; omit for the most recent timer"},
                    "all": {"type": "boolean", "description": "true to cancel every timer"}
                }
            }),
        ),
        t(
            "add_alarm",
            "Set an alarm.",
            json!({
                "type": "object",
                "properties": {
                    "time": {"type": "string", "description": "Time with AM or PM, e.g. \"7:00 AM\""},
                    "days": {"type": "string", "description": "\"weekdays\", \"weekends\", \"daily\" or day names like \"mon wed fri\"; omit for a one-off alarm"},
                    "label": {"type": "string", "description": "Optional label"}
                },
                "required": ["time"]
            }),
        ),
        t(
            "add_reminder",
            "Add a reminder, optionally for a date and time.",
            json!({
                "type": "object",
                "properties": {
                    "text": {"type": "string", "description": "What to be reminded about"},
                    "date": {"type": "string", "description": "\"today\", \"tomorrow\", a weekday name, or YYYY-MM-DD; optional"},
                    "time": {"type": "string", "description": "Time with AM or PM, e.g. \"5:00 PM\"; optional"}
                },
                "required": ["text"]
            }),
        ),
        t(
            "list_today",
            "Get the current time and today's timers, alarms, reminders and weather.",
            json!({"type": "object", "properties": {}}),
        ),
        t(
            "get_weather",
            "Get the current weather and the forecast for the next hours and days.",
            json!({"type": "object", "properties": {}}),
        ),
    ]
}

fn num(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s
            .trim()
            .trim_end_matches(|c: char| c.is_alphabetic())
            .trim()
            .parse()
            .ok(),
        _ => None,
    }
}

fn text(v: &Value) -> Option<String> {
    match v {
        Value::String(s) if !s.trim().is_empty() => Some(s.trim().to_string()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn truthy(v: &Value) -> bool {
    matches!(v, Value::Bool(true)) || matches!(v.as_str(), Some("true" | "yes"))
}

pub fn run(
    store: &Store,
    weather: Option<String>,
    call: &ToolCall,
    now_utc: DateTime<Utc>,
    now_local: NaiveDateTime,
) -> Outcome {
    let a = &call.args;
    let apply = |cmd: Command| {
        let say = confirm(&cmd, now_local);
        match actions::apply(store, cmd, now_utc, now_local) {
            Ok(applied) => {
                let say = match &applied.undo {
                    Some(Undo::RestoreTimers(ts)) => Some(match ts.as_slice() {
                        [one] => format!("Cancelled the {} timer.", one.name),
                        many => format!("Cancelled {} timers.", many.len()),
                    }),
                    None => Some(format!("{}.", sentence_case(&applied.summary))), // e.g. "No timer named rice"
                    _ => say,
                };
                Outcome {
                    result: format!("done: {}", applied.summary.to_lowercase()),
                    applied: Some(applied),
                    say,
                }
            }
            Err(e) => Outcome::error(e.to_string()),
        }
    };

    match call.name.as_str() {
        "set_timer" => {
            let secs = num(&a["minutes"]).unwrap_or(0.0) * 60.0 + num(&a["seconds"]).unwrap_or(0.0);
            match Duration::try_from_secs_f64(secs)
                .ok()
                .filter(|d| !d.is_zero())
            {
                Some(duration) => apply(Command::StartTimer {
                    name: text(&a["name"]).map(|n| capitalize(&n)),
                    duration,
                }),
                None => Outcome::error("the timer needs a length in minutes"),
            }
        }
        "cancel_timer" => {
            let target = if truthy(&a["all"]) {
                TimerTarget::All
            } else if let Some(n) = text(&a["name"]) {
                TimerTarget::Named(n)
            } else {
                TimerTarget::Latest
            };
            apply(Command::CancelTimer { target })
        }
        "add_alarm" => {
            let Some(time) = text(&a["time"]) else {
                return Outcome::error("the alarm needs a time");
            };
            let days = text(&a["days"]).unwrap_or_default();
            match command::parse(&format!("alarm {time} {days}"), now_local) {
                Some(Command::AddAlarm {
                    time,
                    days,
                    label: parsed,
                }) => {
                    let label = text(&a["label"]).or(parsed).map(|l| capitalize(&l));
                    apply(Command::AddAlarm { time, days, label })
                }
                _ => Outcome::error(format!(
                    "couldn't understand the time \"{time}\" / days \"{days}\""
                )),
            }
        }
        "add_reminder" => {
            let Some(body) = text(&a["text"]) else {
                return Outcome::error("the reminder needs text");
            };
            let date = match text(&a["date"]) {
                Some(d) => match parse_date_text(&d, now_local) {
                    Some(d) => Some(d),
                    None => return Outcome::error(format!("couldn't understand the date \"{d}\"")),
                },
                None => None,
            };
            let (date, time) = match text(&a["time"]) {
                Some(t) => match parse_clock_time(&t, date, now_local) {
                    Some((d, t)) => (Some(d), Some(t)),
                    None => return Outcome::error(format!("couldn't understand the time \"{t}\"")),
                },
                None => (date, None),
            };
            apply(Command::AddReminder {
                text: body,
                date,
                time,
            })
        }
        "list_today" => match today_facts(store, weather, now_utc, now_local) {
            Ok(s) => Outcome::info(s),
            Err(e) => Outcome::error(e.to_string()),
        },
        "get_weather" => Outcome::info(weather.unwrap_or_else(|| {
            "Weather isn't available: no location is set, or no forecast has loaded yet.".into()
        })),
        other => Outcome::error(format!("unknown tool {other}")),
    }
}

/// "10 minutes", "1 hour 15 minutes", "30 seconds".
fn spoken_duration(d: Duration) -> String {
    let s = d.as_secs();
    let (h, m, sec) = (s / 3600, (s % 3600) / 60, s % 60);
    let unit = |n: u64, one: &str| format!("{n} {one}{}", if n == 1 { "" } else { "s" });
    let mut parts = Vec::new();
    if h > 0 {
        parts.push(unit(h, "hour"));
    }
    if m > 0 {
        parts.push(unit(m, "minute"));
    }
    if sec > 0 || parts.is_empty() {
        parts.push(unit(sec, "second"));
    }
    parts.join(" ")
}

fn spoken_days(days: u8, time: chrono::NaiveTime, now_local: NaiveDateTime) -> String {
    use clock_core::command::days::*;
    match days & 0x7F {
        ONCE => {
            let next = next_fire_local(time, ONCE, now_local);
            day_word(next.date(), now_local).to_lowercase()
        }
        EVERY_DAY => "every day".into(),
        WEEKDAYS => "on weekdays".into(),
        WEEKENDS => "on weekends".into(),
        d => {
            let names: Vec<String> = days_label(d).split(' ').map(sentence_case).collect();
            format!("on {}", names.join(", "))
        }
    }
}

/// The sentence shown after an action succeeds (no model involved).
fn confirm(cmd: &Command, now_local: NaiveDateTime) -> Option<String> {
    Some(match cmd {
        Command::StartTimer {
            name: Some(n),
            duration,
        } => {
            format!("{n} timer set for {}.", spoken_duration(*duration))
        }
        Command::StartTimer {
            name: None,
            duration,
        } => {
            format!("Timer set for {}.", spoken_duration(*duration))
        }
        Command::AddAlarm { time, days, label } => {
            let label = label
                .as_ref()
                .map(|l| format!(" ({l})"))
                .unwrap_or_default();
            format!(
                "Alarm set for {} {}{label}.",
                time12(*time),
                spoken_days(*days, *time, now_local)
            )
        }
        Command::AddReminder { text, date, time } => {
            let when = match (date, time) {
                (Some(d), Some(t)) => {
                    format!(
                        ", {} at {}",
                        day_word(*d, now_local).to_lowercase(),
                        time12(*t)
                    )
                }
                (Some(d), None) => format!(", {}", day_word(*d, now_local).to_lowercase()),
                _ => String::new(),
            };
            format!("Reminder added: {text}{when}.")
        }
        Command::CancelTimer { .. } | Command::Brief => return None,
    })
}

fn sentence_case(s: &str) -> String {
    capitalize(&s.to_lowercase())
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    c.next()
        .map(|f| f.to_uppercase().chain(c).collect())
        .unwrap_or_default()
}

/// Facts about today, as plain text. Also the input for the briefing (Phase 4).
pub fn today_facts(
    store: &Store,
    weather: Option<String>,
    now_utc: DateTime<Utc>,
    now_local: NaiveDateTime,
) -> anyhow::Result<String> {
    let mut out = vec![format!(
        "Now: {}.",
        now_local.format("%A %-d %B %Y, %-I:%M %p")
    )];

    let timers = store.timers()?;
    if timers.is_empty() {
        out.push("Timers: none.".into());
    } else {
        let list: Vec<String> = timers
            .iter()
            .map(|t| {
                let state = if t.is_paused() {
                    "paused".to_string()
                } else if t.is_finished(now_utc) {
                    "finished".into()
                } else {
                    format!("{} left", countdown_label(t.remaining(now_utc)))
                };
                match t.ends_utc.filter(|_| !t.is_finished(now_utc)) {
                    Some(end) => format!(
                        "{} ({state}, rings at {})",
                        t.name,
                        time12(end.with_timezone(&Local).time())
                    ),
                    None => format!("{} ({state})", t.name),
                }
            })
            .collect();
        out.push(format!("Timers: {}.", list.join("; ")));
    }

    let alarms: Vec<String> = store
        .alarms()?
        .iter()
        .filter(|a| a.enabled)
        .filter_map(|a| {
            let next = a.next_fire_utc?.with_timezone(&Local).naive_local();
            let label = if a.label.is_empty() {
                String::new()
            } else {
                format!(" \"{}\"", a.label)
            };
            Some(format!(
                "{} {}{label}, next rings {} at {}",
                time12(a.time_local),
                days_label(a.days).to_lowercase(),
                day_word(next.date(), now_local).to_lowercase(),
                time12(next.time())
            ))
        })
        .collect();
    out.push(if alarms.is_empty() {
        "Alarms: none on.".into()
    } else {
        format!("Alarms: {}.", alarms.join("; "))
    });

    let reminders: Vec<String> = store
        .reminders()?
        .iter()
        .filter(|r| !r.done)
        .map(|r| match (r.due_date, r.due_time) {
            (Some(d), Some(t)) => format!(
                "{} ({} {})",
                r.text,
                day_word(d, now_local).to_lowercase(),
                time12(t)
            ),
            (Some(d), None) => format!("{} ({})", r.text, day_word(d, now_local).to_lowercase()),
            _ => r.text.clone(),
        })
        .collect();
    out.push(if reminders.is_empty() {
        "Open reminders: none.".into()
    } else {
        format!("Open reminders: {}.", reminders.join("; "))
    });

    out.push("Calendar: not connected yet.".into());
    if let Some(w) = weather {
        out.push(w);
    }
    Ok(out.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{NaiveDate, TimeZone};

    fn now() -> (DateTime<Utc>, NaiveDateTime) {
        let l = NaiveDate::from_ymd_opt(2026, 11, 13)
            .unwrap()
            .and_hms_opt(10, 30, 0)
            .unwrap();
        (Utc.from_utc_datetime(&l), l)
    }
    fn run_t(s: &Store, name: &str, args: Value) -> Outcome {
        let (u, l) = now();
        let call = ToolCall {
            id: "c".into(),
            name: name.into(),
            args,
        };
        run(s, None, &call, u, l)
    }
    fn say(o: &Outcome) -> &str {
        o.say.as_deref().unwrap_or("")
    }

    #[test]
    fn timer_tools() {
        let s = Store::open_in_memory().unwrap();
        // What llama3.2:3b actually sent: minutes as a string.
        let o = run_t(&s, "set_timer", json!({"minutes": "5", "name": "pasta"}));
        assert_eq!(o.result, "done: timer · pasta · 5 min");
        assert_eq!(say(&o), "Pasta timer set for 5 minutes.");
        assert!(o.applied.unwrap().undo.is_some());

        assert_eq!(
            say(&run_t(&s, "set_timer", json!({"minutes": 75}))),
            "Timer set for 1 hour 15 minutes."
        );
        run_t(&s, "cancel_timer", json!({}));
        run_t(&s, "set_timer", json!({"minutes": 0.5}));
        assert_eq!(s.timers().unwrap()[1].total, Duration::from_secs(30));

        let bad = run_t(&s, "set_timer", json!({}));
        assert!(bad.result.starts_with("error") && bad.say.is_none());

        assert_eq!(
            say(&run_t(&s, "cancel_timer", json!({"name": "pasta"}))),
            "Cancelled the Pasta timer."
        );
        assert_eq!(
            say(&run_t(&s, "cancel_timer", json!({"name": "rice"}))),
            "No timer named rice."
        );
        assert_eq!(
            say(&run_t(&s, "cancel_timer", json!({"all": "true"}))),
            "Cancelled the 30 S timer."
        );
    }

    #[test]
    fn alarm_tools() {
        let s = Store::open_in_memory().unwrap();
        let o = run_t(
            &s,
            "add_alarm",
            json!({"time": "6:45 AM", "days": "weekdays", "label": "gym"}),
        );
        assert_eq!(o.result, "done: alarm · 6:45 am · weekdays · gym");
        assert_eq!(say(&o), "Alarm set for 6:45 AM on weekdays (Gym).");
        assert_eq!(
            say(&run_t(&s, "add_alarm", json!({"time": "7:00 AM"}))),
            "Alarm set for 7:00 AM tomorrow."
        );
        assert_eq!(
            say(&run_t(
                &s,
                "add_alarm",
                json!({"time": "8 am", "days": "mon wed fri"})
            )),
            "Alarm set for 8:00 AM on Mon, Wed, Fri."
        );
        assert!(run_t(&s, "add_alarm", json!({"time": "soonish"}))
            .result
            .starts_with("error"));
    }

    #[test]
    fn reminder_tools_and_facts() {
        let s = Store::open_in_memory().unwrap();
        let o = run_t(
            &s,
            "add_reminder",
            json!({"text": "call the bank", "date": "tomorrow", "time": "9:00 AM"}),
        );
        assert_eq!(
            o.result,
            "done: reminder · call the bank · tomorrow 9:00 am"
        );
        assert_eq!(
            say(&o),
            "Reminder added: call the bank, tomorrow at 9:00 AM."
        );
        assert_eq!(
            say(&run_t(
                &s,
                "add_reminder",
                json!({"text": "check the mail", "time": "5"})
            )),
            "Reminder added: check the mail, today at 5:00 PM."
        );
        assert_eq!(
            say(&run_t(
                &s,
                "add_reminder",
                json!({"text": "renew registration"})
            )),
            "Reminder added: renew registration."
        );
        assert!(
            run_t(&s, "add_reminder", json!({"text": "x", "date": "someday"}))
                .result
                .starts_with("error")
        );

        run_t(
            &s,
            "add_alarm",
            json!({"time": "6:45 AM", "days": "weekdays", "label": "gym"}),
        );
        let (u, l) = now();
        let facts = today_facts(&s, None, u, l).unwrap();
        assert!(
            facts.starts_with("Now: Friday 13 November 2026, 10:30 AM."),
            "{facts}"
        );
        assert!(
            facts.contains("6:45 AM weekdays \"Gym\", next rings mon 16 nov at 6:45 AM"),
            "{facts}"
        );
        assert!(facts.contains("check the mail (today 5:00 PM)"), "{facts}");
    }
}
