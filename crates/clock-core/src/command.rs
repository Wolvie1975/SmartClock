//! Offline clock-command parser (DEVELOPMENT.md section 9).
//!
//! Runs before any model. `None` means "not a clock command" → send to the AI router.
//!
//! Time rules:
//! - An hour with no AM/PM ("at 5", "wake me at 7") means the *next* time that
//!   o'clock comes round: 5 AM if it's before 5 AM now, otherwise 5 PM, otherwise
//!   tomorrow 5 AM. "12am" is midnight, "12pm" is noon.
//! - On an explicit future date ("tomorrow at 3"), an hour with no AM/PM is read as
//!   daytime: 7–11 → AM, 12 and 1–6 → PM.
//! - A repeating alarm with no AM/PM ("alarm 6:45 weekdays") reads 4–11 as AM and
//!   12 and 1–3 as PM.
//! - Two-digit hours with a leading zero ("07:00") and hours 0 or 13–23 are 24-hour.
//! - An alarm with no days is one-off (`days == ONCE`).

use chrono::{Datelike, NaiveDate, NaiveDateTime, NaiveTime, Timelike, Weekday};
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    StartTimer {
        name: Option<String>,
        duration: Duration,
    },
    AddAlarm {
        time: NaiveTime,
        days: u8,
        label: Option<String>,
    },
    AddReminder {
        text: String,
        date: Option<NaiveDate>,
        time: Option<NaiveTime>,
    },
    CancelTimer {
        target: TimerTarget,
    },
    Brief,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TimerTarget {
    /// "cancel timer": the most recently started one.
    Latest,
    Named(String),
    All,
}

/// Alarm day bitmask, Mon = 1 … Sun = 64 (matches the `alarms.days` column).
pub mod days {
    pub const ONCE: u8 = 0;
    pub const MON: u8 = 1;
    pub const TUE: u8 = 2;
    pub const WED: u8 = 4;
    pub const THU: u8 = 8;
    pub const FRI: u8 = 16;
    pub const SAT: u8 = 32;
    pub const SUN: u8 = 64;
    pub const WEEKDAYS: u8 = 31;
    pub const WEEKENDS: u8 = 96;
    pub const EVERY_DAY: u8 = 127;
}

/// Parse one line of command-bar / assistant input.
pub fn parse(input: &str, now: NaiveDateTime) -> Option<Command> {
    let (orig, lower) = tokenize(input);
    if lower.is_empty() {
        return None;
    }
    if is_brief(&lower) {
        return Some(Command::Brief);
    }
    if starts_with(&lower, &["remind", "me"]) {
        return parse_reminder(&orig, &lower, now);
    }
    if let Some(plen) = ALARM_PREFIXES
        .iter()
        .find(|p| starts_with(&lower, p))
        .map(|p| p.len())
    {
        return parse_alarm(&orig, &lower, plen, now);
    }
    if CANCEL_VERBS.contains(&lower[0].as_str()) {
        return parse_cancel(&orig, &lower);
    }
    parse_timer(&orig, &lower)
}

// ---------------------------------------------------------------------------
// Tokens

/// Original-case tokens (for names and reminder text) and normalized lowercase
/// tokens (for matching), index-aligned.
fn tokenize(input: &str) -> (Vec<&str>, Vec<String>) {
    let trimmed = input.trim().trim_end_matches(['.', '!', '?']);
    let orig: Vec<&str> = trimmed.split_whitespace().collect();
    let lower = orig
        .iter()
        .map(|t| {
            t.to_lowercase()
                .trim_end_matches(',')
                .replace("a.m.", "am")
                .replace("p.m.", "pm")
                .replace("a.m", "am")
                .replace("p.m", "pm")
        })
        .collect();
    (orig, lower)
}

fn starts_with(lower: &[String], prefix: &[&str]) -> bool {
    lower.len() >= prefix.len() && lower.iter().zip(prefix).all(|(a, b)| a == b)
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c).collect(),
        None => String::new(),
    }
}

fn join(orig: &[&str], idx: impl IntoIterator<Item = usize>) -> String {
    idx.into_iter()
        .map(|k| orig[k])
        .collect::<Vec<_>>()
        .join(" ")
}

// ---------------------------------------------------------------------------
// Brief

fn is_brief(lower: &[String]) -> bool {
    const PHRASES: &[&str] = &[
        "brief me",
        "briefing",
        "daily briefing",
        "play briefing",
        "play the briefing",
        "play my briefing",
        "give me my briefing",
    ];
    PHRASES.contains(&lower.join(" ").as_str())
}

// ---------------------------------------------------------------------------
// Durations

fn unit_secs(u: &str) -> Option<f64> {
    match u {
        "s" | "sec" | "secs" | "second" | "seconds" => Some(1.0),
        "m" | "min" | "mins" | "minute" | "minutes" => Some(60.0),
        "h" | "hr" | "hrs" | "hour" | "hours" => Some(3600.0),
        _ => None,
    }
}

fn is_number(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit() || b == b'.') && s.parse::<f64>().is_ok()
}

/// "20m", "90s", "1h15m", "1.5h" → seconds.
fn parse_glued(tok: &str) -> Option<f64> {
    let mut total = 0.0;
    let mut rest = tok;
    while !rest.is_empty() {
        let n_end = rest.find(|c: char| !(c.is_ascii_digit() || c == '.'))?;
        if !is_number(&rest[..n_end]) {
            return None;
        }
        let n: f64 = rest[..n_end].parse().ok()?;
        let after = &rest[n_end..];
        let u_end = after
            .find(|c: char| !c.is_ascii_alphabetic())
            .unwrap_or(after.len());
        total += n * unit_secs(&after[..u_end])?;
        rest = &after[u_end..];
    }
    (total > 0.0 || tok.starts_with('0')).then_some(total)
}

/// One duration part at `t[0]`: glued ("20m") or split ("20 min"). Returns (secs, tokens used).
fn duration_part(t: &[String]) -> Option<(f64, usize)> {
    let first = t.first()?;
    if let Some(s) = parse_glued(first) {
        return Some((s, 1));
    }
    if is_number(first) {
        let u = unit_secs(t.get(1)?)?;
        return Some((first.parse::<f64>().ok()? * u, 2));
    }
    None
}

/// Every duration part anywhere in the tokens, plus which tokens they used.
fn find_duration(lower: &[String]) -> (f64, Vec<bool>) {
    let mut used = vec![false; lower.len()];
    let mut total = 0.0;
    let mut i = 0;
    while i < lower.len() {
        match duration_part(&lower[i..]) {
            Some((s, n)) => {
                total += s;
                used[i..i + n].iter_mut().for_each(|u| *u = true);
                i += n;
            }
            None => i += 1,
        }
    }
    (total, used)
}

/// Consecutive duration parts at the start ("1 hour and 15 minutes"). Returns (secs, tokens used).
fn duration_prefix(t: &[String]) -> Option<(f64, usize)> {
    let (mut total, mut i) = duration_part(t)?;
    loop {
        let skip = usize::from(t.get(i).is_some_and(|s| s == "and"));
        match duration_part(&t[(i + skip).min(t.len())..]) {
            Some((s, n)) => {
                total += s;
                i += skip + n;
            }
            None => return Some((total, i)),
        }
    }
}

fn to_duration(secs: f64) -> Option<Duration> {
    if secs <= 0.0 {
        return None;
    }
    Duration::try_from_secs_f64(secs).ok()
}

// ---------------------------------------------------------------------------
// Times and dates

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TimeSpec {
    Exact(NaiveTime),
    /// 1–12 with no AM/PM.
    Ambiguous {
        hour12: u32,
        minute: u32,
    },
}

fn hm(h: u32, m: u32) -> NaiveTime {
    NaiveTime::from_hms_opt(h, m, 0).expect("valid time")
}

/// "7", "7am", "7 am", "7:30pm", "17:00", "noon", "midnight". Returns (spec, tokens used).
fn parse_time(t: &[String]) -> Option<(TimeSpec, usize)> {
    let first = t.first()?.as_str();
    match first {
        "noon" | "midday" => return Some((TimeSpec::Exact(hm(12, 0)), 1)),
        "midnight" => return Some((TimeSpec::Exact(hm(0, 0)), 1)),
        _ => {}
    }
    let split = first
        .find(|c: char| c.is_ascii_alphabetic())
        .unwrap_or(first.len());
    let (num, mut suffix) = first.split_at(split);
    let mut used = 1;
    if suffix.is_empty() {
        if let Some(next) = t.get(1).filter(|n| matches!(n.as_str(), "am" | "pm")) {
            suffix = next;
            used = 2;
        }
    }
    let pm = match suffix {
        "" => None,
        "am" | "a" => Some(false),
        "pm" | "p" => Some(true),
        _ => return None,
    };

    let (h_str, m_str) = match num.split_once(':') {
        Some((h, m)) => (h, Some(m)),
        None => (num, None),
    };
    let digits = |s: &str, max_len: usize| {
        !s.is_empty() && s.len() <= max_len && s.bytes().all(|b| b.is_ascii_digit())
    };
    if !digits(h_str, 2) {
        return None;
    }
    let h: u32 = h_str.parse().ok()?;
    let m: u32 = match m_str {
        Some(m) if m.len() == 2 && digits(m, 2) => m.parse().ok()?,
        Some(_) => return None,
        None => 0,
    };
    if m > 59 {
        return None;
    }
    let spec = match pm {
        Some(pm) if (1..=12).contains(&h) => {
            TimeSpec::Exact(hm(h % 12 + if pm { 12 } else { 0 }, m))
        }
        Some(_) => return None,
        None if h > 23 => return None,
        None if h == 0 || h >= 13 || h_str.starts_with('0') => TimeSpec::Exact(hm(h, m)),
        None => TimeSpec::Ambiguous {
            hour12: h,
            minute: m,
        },
    };
    Some((spec, used))
}

fn next_at(t: NaiveTime, now: NaiveDateTime) -> NaiveDateTime {
    let today = now.date().and_time(t);
    if today > now {
        today
    } else {
        today + chrono::Duration::days(1)
    }
}

fn am_pm(hour12: u32, minute: u32) -> (NaiveTime, NaiveTime) {
    (hm(hour12 % 12, minute), hm(hour12 % 12 + 12, minute))
}

/// The next moment this time comes round.
fn resolve_next(spec: TimeSpec, now: NaiveDateTime) -> NaiveDateTime {
    match spec {
        TimeSpec::Exact(t) => next_at(t, now),
        TimeSpec::Ambiguous { hour12, minute } => {
            let (am, pm) = am_pm(hour12, minute);
            next_at(am, now).min(next_at(pm, now))
        }
    }
}

/// The time on a given date.
fn resolve_on(spec: TimeSpec, date: NaiveDate, now: NaiveDateTime) -> NaiveTime {
    match spec {
        TimeSpec::Exact(t) => t,
        TimeSpec::Ambiguous { hour12, minute } => {
            let (am, pm) = am_pm(hour12, minute);
            if date == now.date() {
                if now.date().and_time(am) > now {
                    am
                } else {
                    pm
                }
            } else if (7..=11).contains(&hour12) {
                am
            } else {
                pm
            }
        }
    }
}

fn weekday(tok: &str) -> Option<Weekday> {
    Some(match tok.trim_end_matches('s') {
        "mon" | "monday" => Weekday::Mon,
        "tue" | "tuesday" => Weekday::Tue,
        "wed" | "wednesday" => Weekday::Wed,
        "thu" | "thur" | "thursday" => Weekday::Thu,
        "fri" | "friday" => Weekday::Fri,
        "sat" | "saturday" => Weekday::Sat,
        "sun" | "sunday" => Weekday::Sun,
        _ => return None,
    })
}

/// The next date with this weekday, strictly after today.
fn next_weekday(today: NaiveDate, wd: Weekday) -> NaiveDate {
    let ahead = (wd.num_days_from_monday() + 7 - today.weekday().num_days_from_monday()) % 7;
    today + chrono::Duration::days(if ahead == 0 { 7 } else { ahead.into() })
}

// ---------------------------------------------------------------------------
// Helpers for structured input (assistant tool calls)

/// A time of day as text ("5:00 PM", "17:00", "noon", "5") resolved with the same
/// rules as typed commands. With `date`, uses that day's rules; without, the next
/// time it comes round (which may be tomorrow).
pub fn parse_clock_time(
    text: &str,
    date: Option<NaiveDate>,
    now: NaiveDateTime,
) -> Option<(NaiveDate, NaiveTime)> {
    let (_, lower) = tokenize(text);
    let (spec, used) = parse_time(&lower)?;
    if used != lower.len() {
        return None;
    }
    Some(match date {
        Some(d) => (d, resolve_on(spec, d, now)),
        None => {
            let dt = resolve_next(spec, now);
            (dt.date(), dt.time())
        }
    })
}

/// "2026-11-20", "today", "tomorrow", "friday" / "on friday".
pub fn parse_date_text(text: &str, now: NaiveDateTime) -> Option<NaiveDate> {
    let t = text.trim().to_lowercase();
    if let Ok(d) = NaiveDate::parse_from_str(&t, "%Y-%m-%d") {
        return Some(d);
    }
    match t.as_str() {
        "today" | "tonight" => Some(now.date()),
        "tomorrow" => Some(now.date() + chrono::Duration::days(1)),
        other => {
            weekday(other.trim_start_matches("on ").trim()).map(|w| next_weekday(now.date(), w))
        }
    }
}

// ---------------------------------------------------------------------------
// Reminders

#[derive(Default)]
struct When {
    date: Option<NaiveDate>,
    time: Option<TimeSpec>,
    /// "in 20 minutes"
    at: Option<NaiveDateTime>,
}

/// One "when" phrase at `t[0]`: today, tomorrow, (on) <weekday>, at <time>, in <duration>.
/// Returns tokens used; `None` if it isn't one or that field is already set.
fn when_phrase(t: &[String], now: NaiveDateTime, w: &mut When) -> Option<usize> {
    let first = t.first()?.as_str();
    let set_date = |w: &mut When, d: NaiveDate, n: usize| {
        if w.date.is_some() {
            return None;
        }
        w.date = Some(d);
        Some(n)
    };
    match first {
        "today" => set_date(w, now.date(), 1),
        "tomorrow" => set_date(w, now.date() + chrono::Duration::days(1), 1),
        "on" => set_date(w, next_weekday(now.date(), weekday(t.get(1)?)?), 2),
        "at" if w.time.is_none() => {
            let (spec, n) = parse_time(&t[1..])?;
            w.time = Some(spec);
            Some(1 + n)
        }
        "in" if w.at.is_none() => {
            let (secs, n) = duration_prefix(&t[1..])?;
            let at = now + chrono::Duration::from_std(to_duration(secs)?).ok()?;
            w.at = Some(at.with_nanosecond(0)?);
            Some(1 + n)
        }
        _ => set_date(w, next_weekday(now.date(), weekday(first)?), 1),
    }
}

fn parse_reminder(orig: &[&str], lower: &[String], now: NaiveDateTime) -> Option<Command> {
    let mut w = When::default();

    // Leading phrases up to "to" / "about" / "that".
    let mut start = 2;
    loop {
        match lower.get(start)?.as_str() {
            "to" | "about" | "that" => {
                start += 1;
                break;
            }
            _ => start += when_phrase(&lower[start..], now, &mut w)?,
        }
    }

    // Trailing phrases: "… at 5", "… tomorrow", "… on friday".
    let mut end = lower.len();
    // Longest phrase first, so "on sunday" wins over "sunday".
    while let Some((s, probe)) = (end.saturating_sub(4).max(start + 1)..end).find_map(|s| {
        let mut probe = When {
            date: w.date,
            time: w.time,
            at: w.at,
        };
        (when_phrase(&lower[s..end], now, &mut probe) == Some(end - s)).then_some((s, probe))
    }) {
        w = probe;
        end = s;
    }

    if start >= end {
        return None;
    }
    let text = join(orig, start..end);

    let (date, time) = match (w.at, w.date, w.time) {
        (Some(at), _, _) => (Some(at.date()), Some(at.time())),
        (None, None, Some(spec)) => {
            let dt = resolve_next(spec, now);
            (Some(dt.date()), Some(dt.time()))
        }
        (None, Some(d), Some(spec)) => (Some(d), Some(resolve_on(spec, d, now))),
        (None, d, None) => (d, None),
    };
    Some(Command::AddReminder { text, date, time })
}

// ---------------------------------------------------------------------------
// Alarms

const ALARM_PREFIXES: &[&[&str]] = &[
    &["set", "an", "alarm"],
    &["set", "alarm"],
    &["add", "an", "alarm"],
    &["add", "alarm"],
    &["new", "alarm"],
    &["alarm"],
    &["wake", "me", "up"],
    &["wake", "me"],
];

fn parse_alarm(
    orig: &[&str],
    lower: &[String],
    plen: usize,
    now: NaiveDateTime,
) -> Option<Command> {
    let mut i = plen;
    while matches!(lower.get(i).map(String::as_str), Some("for" | "at")) {
        i += 1;
    }
    let (spec, n) = parse_time(lower.get(i..)?)?;
    i += n;

    let mut d = days::ONCE;
    let mut label = Vec::new();
    let mut k = i;
    while k < lower.len() {
        match lower[k].as_str() {
            "on" | "and" | "&" | "," | "every" if lower.get(k + 1).is_none_or(|n| n != "day") => {}
            "every" => {
                d = days::EVERY_DAY;
                k += 1;
            }
            "daily" | "everyday" => d |= days::EVERY_DAY,
            "weekdays" | "weekday" => d |= days::WEEKDAYS,
            "weekends" | "weekend" => d |= days::WEEKENDS,
            t => match weekday(t) {
                Some(wd) => d |= 1 << wd.num_days_from_monday(),
                None => label.push(k),
            },
        }
        k += 1;
    }

    let time = match spec {
        TimeSpec::Exact(t) => t,
        // A repeating alarm has no single "next" time: read 4–11 as AM, 12–3 as PM.
        TimeSpec::Ambiguous { hour12, minute } if d != days::ONCE => {
            let (am, pm) = am_pm(hour12, minute);
            if (4..=11).contains(&hour12) {
                am
            } else {
                pm
            }
        }
        amb => resolve_next(amb, now).time(),
    };
    let label = (!label.is_empty()).then(|| capitalize(&join(orig, label)));
    Some(Command::AddAlarm {
        time,
        days: d,
        label,
    })
}

// ---------------------------------------------------------------------------
// Timers

const CANCEL_VERBS: &[&str] = &["cancel", "stop", "delete", "remove", "clear", "kill", "end"];

fn parse_cancel(orig: &[&str], lower: &[String]) -> Option<Command> {
    let plural = lower.iter().any(|t| t == "timers");
    if !plural && !lower.iter().any(|t| t == "timer") {
        return None;
    }
    let words: Vec<usize> = (1..lower.len())
        .filter(|&k| {
            !matches!(
                lower[k].as_str(),
                "timer" | "timers" | "the" | "my" | "a" | "all"
            )
        })
        .collect();
    let target = if lower.iter().any(|t| t == "all") || (plural && words.is_empty()) {
        TimerTarget::All
    } else if words.is_empty() {
        TimerTarget::Latest
    } else {
        TimerTarget::Named(capitalize(&join(orig, words)))
    };
    Some(Command::CancelTimer { target })
}

const QUESTION_WORDS: &[&str] = &[
    "what", "what's", "whats", "how", "why", "when", "who", "where", "which", "is", "are", "can",
    "could", "would", "should", "does", "do", "did", "will",
];
const TIMER_FILLER: &[&str] = &[
    "timer", "set", "start", "a", "an", "for", "the", "me", "my", "and", "new",
];

fn parse_timer(orig: &[&str], lower: &[String]) -> Option<Command> {
    if !lower.iter().any(|t| t == "timer") || QUESTION_WORDS.contains(&lower[0].as_str()) {
        return None;
    }
    let (secs, used) = find_duration(lower);
    let duration = to_duration(secs)?;
    let name: Vec<usize> = (0..lower.len())
        .filter(|&k| !used[k] && !TIMER_FILLER.contains(&lower[k].as_str()))
        .collect();
    if name.len() > 3 {
        return None; // reads like a sentence, not a timer name
    }
    let name = (!name.is_empty()).then(|| capitalize(&join(orig, name)));
    Some(Command::StartTimer { name, duration })
}

// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::days::*;
    use super::*;

    /// Friday 13 November 2026.
    fn day(d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 11, d).unwrap()
    }
    fn at(h: u32, m: u32) -> NaiveDateTime {
        day(13).and_time(hm(h, m))
    }
    /// Parse at 10:30 AM on Friday 13 Nov 2026.
    fn p(s: &str) -> Option<Command> {
        parse(s, at(10, 30))
    }
    fn mins(m: u64) -> Duration {
        Duration::from_secs(m * 60)
    }
    fn timer(name: Option<&str>, duration: Duration) -> Option<Command> {
        Some(Command::StartTimer {
            name: name.map(Into::into),
            duration,
        })
    }
    fn alarm(h: u32, m: u32, days: u8, label: Option<&str>) -> Option<Command> {
        Some(Command::AddAlarm {
            time: hm(h, m),
            days,
            label: label.map(Into::into),
        })
    }
    fn reminder(text: &str, date: Option<NaiveDate>, time: Option<(u32, u32)>) -> Option<Command> {
        Some(Command::AddReminder {
            text: text.into(),
            date,
            time: time.map(|(h, m)| hm(h, m)),
        })
    }
    fn cancel(target: TimerTarget) -> Option<Command> {
        Some(Command::CancelTimer { target })
    }

    // --- timers (table in section 9) ---

    #[test]
    fn timer_basic_forms() {
        assert_eq!(p("timer 20m"), timer(None, mins(20)));
        assert_eq!(p("timer 90s"), timer(None, Duration::from_secs(90)));
        assert_eq!(p("20 min timer"), timer(None, mins(20)));
        assert_eq!(p("timer 1h 15m"), timer(None, mins(75)));
    }

    #[test]
    fn timer_more_forms() {
        assert_eq!(p("timer 1h15m"), timer(None, mins(75)));
        assert_eq!(p("timer 1.5h"), timer(None, mins(90)));
        assert_eq!(p("set a timer for 10 minutes"), timer(None, mins(10)));
        assert_eq!(p("set a 5 minute timer"), timer(None, mins(5)));
        assert_eq!(p("timer for 1 hour and 15 minutes"), timer(None, mins(75)));
        assert_eq!(p("Timer 20m."), timer(None, mins(20)));
    }

    #[test]
    fn named_timers() {
        assert_eq!(p("tea timer 4m"), timer(Some("Tea"), mins(4)));
        assert_eq!(p("Pasta timer for 9 min"), timer(Some("Pasta"), mins(9)));
        assert_eq!(p("egg timer 7 minutes"), timer(Some("Egg"), mins(7)));
    }

    #[test]
    fn not_timers() {
        assert_eq!(p("timer"), None);
        assert_eq!(p("timer 0m"), None);
        assert_eq!(p("what is a 5 minute timer"), None);
        assert_eq!(p("how long is a timer in rust"), None);
        assert_eq!(p("20m"), None);
    }

    // --- alarms ---

    #[test]
    fn alarm_basic_forms() {
        assert_eq!(p("alarm 7am"), alarm(7, 0, ONCE, None));
        assert_eq!(p("alarm 6:45 weekdays"), alarm(6, 45, WEEKDAYS, None));
        assert_eq!(p("alarm 7 am"), alarm(7, 0, ONCE, None));
        assert_eq!(p("alarm 7 a.m."), alarm(7, 0, ONCE, None));
        assert_eq!(p("alarm 17:30"), alarm(17, 30, ONCE, None));
        assert_eq!(p("alarm 07:00"), alarm(7, 0, ONCE, None));
    }

    #[test]
    fn alarm_noon_midnight_12() {
        assert_eq!(p("alarm 12am"), alarm(0, 0, ONCE, None));
        assert_eq!(p("alarm 12pm"), alarm(12, 0, ONCE, None));
        assert_eq!(p("alarm midnight"), alarm(0, 0, ONCE, None));
        assert_eq!(p("alarm noon"), alarm(12, 0, ONCE, None));
    }

    #[test]
    fn alarm_ambiguous_hour_is_next_occurrence() {
        // 10:30 AM now → next 7 o'clock is 7 PM.
        assert_eq!(p("wake me at 7"), alarm(19, 0, ONCE, None));
        assert_eq!(p("alarm 7:30"), alarm(19, 30, ONCE, None));
        // 10:30 PM now → next 7 o'clock is 7 AM.
        assert_eq!(
            parse("wake me up at 7", at(22, 30)),
            alarm(7, 0, ONCE, None)
        );
    }

    #[test]
    fn alarm_days_and_labels() {
        assert_eq!(
            p("set an alarm for 6:30 am on weekends"),
            alarm(6, 30, WEEKENDS, None)
        );
        assert_eq!(
            p("alarm 7am mon wed fri"),
            alarm(7, 0, MON | WED | FRI, None)
        );
        assert_eq!(
            p("alarm 7am mon, wed, fri"),
            alarm(7, 0, MON | WED | FRI, None)
        );
        assert_eq!(
            p("alarm 7am on tuesdays and thursdays"),
            alarm(7, 0, TUE | THU, None)
        );
        assert_eq!(p("alarm 7am daily"), alarm(7, 0, EVERY_DAY, None));
        assert_eq!(p("alarm 7am every day"), alarm(7, 0, EVERY_DAY, None));
        assert_eq!(p("alarm 7am every sunday"), alarm(7, 0, SUN, None));
        assert_eq!(
            p("alarm 5:30am gym weekdays"),
            alarm(5, 30, WEEKDAYS, Some("Gym"))
        );
        assert_eq!(p("alarm 2 weekends"), alarm(14, 0, WEEKENDS, None));
    }

    #[test]
    fn not_alarms() {
        assert_eq!(p("alarm"), None);
        assert_eq!(p("alarm 25:00"), None);
        assert_eq!(p("alarm 7:75"), None);
        assert_eq!(p("alarm 13pm"), None);
        assert_eq!(p("alarm soon"), None);
    }

    // --- reminders ---

    #[test]
    fn reminder_table_cases() {
        assert_eq!(
            p("remind me at 5 to check the mail"),
            reminder("check the mail", Some(day(13)), Some((17, 0)))
        );
        assert_eq!(
            p("remind me to renew car registration"),
            reminder("renew car registration", None, None)
        );
        assert_eq!(
            p("remind me tomorrow to call the bank"),
            reminder("call the bank", Some(day(14)), None)
        );
    }

    #[test]
    fn reminder_at_5_is_next_5_oclock() {
        let r = |h, m| parse("remind me at 5 to check the mail", at(h, m));
        assert_eq!(
            r(4, 0),
            reminder("check the mail", Some(day(13)), Some((5, 0)))
        );
        assert_eq!(
            r(10, 30),
            reminder("check the mail", Some(day(13)), Some((17, 0)))
        );
        assert_eq!(
            r(18, 0),
            reminder("check the mail", Some(day(14)), Some((5, 0)))
        );
    }

    #[test]
    fn reminder_explicit_times() {
        assert_eq!(
            p("remind me at 5pm to stretch"),
            reminder("stretch", Some(day(13)), Some((17, 0)))
        );
        assert_eq!(
            p("remind me at 9am to stretch"),
            reminder("stretch", Some(day(14)), Some((9, 0)))
        );
        assert_eq!(
            p("remind me at 12am to stretch"),
            reminder("stretch", Some(day(14)), Some((0, 0)))
        );
        assert_eq!(
            p("remind me in 20 minutes to flip the laundry"),
            reminder("flip the laundry", Some(day(13)), Some((10, 50)))
        );
        assert_eq!(
            p("remind me in 1 hour and 45 minutes to leave"),
            reminder("leave", Some(day(13)), Some((12, 15)))
        );
    }

    #[test]
    fn reminder_future_date_daytime_heuristic() {
        assert_eq!(
            p("remind me tomorrow at 9 to call Sam"),
            reminder("call Sam", Some(day(14)), Some((9, 0)))
        );
        assert_eq!(
            p("remind me tomorrow at 3 to call Sam"),
            reminder("call Sam", Some(day(14)), Some((15, 0)))
        );
        assert_eq!(
            p("remind me tomorrow at 12 to call Sam"),
            reminder("call Sam", Some(day(14)), Some((12, 0)))
        );
        assert_eq!(
            p("remind me tomorrow at 7pm to call Sam"),
            reminder("call Sam", Some(day(14)), Some((19, 0)))
        );
    }

    #[test]
    fn reminder_weekdays() {
        // Today is Friday.
        assert_eq!(
            p("remind me on monday to file taxes"),
            reminder("file taxes", Some(day(16)), None)
        );
        assert_eq!(
            p("remind me friday to file taxes"),
            reminder("file taxes", Some(day(20)), None)
        );
    }

    #[test]
    fn reminder_trailing_phrases() {
        assert_eq!(
            p("remind me to call the bank tomorrow"),
            reminder("call the bank", Some(day(14)), None)
        );
        assert_eq!(
            p("remind me to call mom on sunday"),
            reminder("call mom", Some(day(15)), None)
        );
        assert_eq!(
            p("remind me to check the oven at 5"),
            reminder("check the oven", Some(day(13)), Some((17, 0)))
        );
        assert_eq!(
            p("remind me to check the oven at 5 pm"),
            reminder("check the oven", Some(day(13)), Some((17, 0)))
        );
        assert_eq!(
            p("remind me to call the bank tomorrow at 9"),
            reminder("call the bank", Some(day(14)), Some((9, 0)))
        );
    }

    #[test]
    fn reminder_text_kept_verbatim() {
        assert_eq!(
            p("Remind me to email Sam, then Alex."),
            reminder("email Sam, then Alex", None, None)
        );
        assert_eq!(
            p("remind me to meet at the park"),
            reminder("meet at the park", None, None)
        );
        assert_eq!(
            p("remind me about the dentist"),
            reminder("the dentist", None, None)
        );
    }

    #[test]
    fn not_reminders() {
        assert_eq!(p("remind me"), None);
        assert_eq!(p("remind me to"), None);
        assert_eq!(p("remind me at to go"), None);
        assert_eq!(p("remind me 5 min timer"), None);
    }

    // --- cancel ---

    #[test]
    fn cancel_timers() {
        assert_eq!(
            p("cancel tea timer"),
            cancel(TimerTarget::Named("Tea".into()))
        );
        assert_eq!(
            p("cancel the tea timer"),
            cancel(TimerTarget::Named("Tea".into()))
        );
        assert_eq!(p("stop all timers"), cancel(TimerTarget::All));
        assert_eq!(p("clear timers"), cancel(TimerTarget::All));
        assert_eq!(p("cancel timer"), cancel(TimerTarget::Latest));
        assert_eq!(p("stop the timer"), cancel(TimerTarget::Latest));
        assert_eq!(p("cancel all"), None);
        assert_eq!(p("stop"), None);
    }

    // --- brief / other ---

    #[test]
    fn brief() {
        assert_eq!(p("brief me"), Some(Command::Brief));
        assert_eq!(p("Brief me."), Some(Command::Brief));
        assert_eq!(p("play briefing"), Some(Command::Brief));
    }

    #[test]
    fn structured_helpers() {
        let now = at(10, 30);
        assert_eq!(
            parse_clock_time("5:00 PM", None, now),
            Some((day(13), hm(17, 0)))
        );
        assert_eq!(
            parse_clock_time("9am", None, now),
            Some((day(14), hm(9, 0)))
        );
        assert_eq!(parse_clock_time("5", None, now), Some((day(13), hm(17, 0))));
        assert_eq!(
            parse_clock_time("3", Some(day(14)), now),
            Some((day(14), hm(15, 0)))
        );
        assert_eq!(
            parse_clock_time("noon", None, now),
            Some((day(13), hm(12, 0)))
        );
        assert_eq!(parse_clock_time("5 to go", None, now), None);
        assert_eq!(parse_date_text("2026-11-20", now), Some(day(20)));
        assert_eq!(parse_date_text("Tomorrow", now), Some(day(14)));
        assert_eq!(parse_date_text("on monday", now), Some(day(16)));
        assert_eq!(parse_date_text("someday", now), None);
    }

    #[test]
    fn not_commands() {
        assert_eq!(p(""), None);
        assert_eq!(p("   "), None);
        assert_eq!(p("what's the weather like"), None);
        assert_eq!(p("write me a haiku about clocks"), None);
    }
}
