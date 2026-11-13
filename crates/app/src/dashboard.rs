//! Dashboard glue: keeps the Slint `Dash` global in sync with the store and
//! handles the command bar, row buttons and UNDO.

use crate::{AlarmRow, AppWindow, Dash, Google, Marker, ReminderRow, TimerRow};
use chrono::{DateTime, Local, NaiveDateTime, Timelike, Utc};
use clock_core::actions::{self, day_word, Undo};
use clock_core::model::{countdown_label, days_label, duration_label, time12, Source, Timer};
use clock_core::{command, Store};
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

type LocationHook = Box<dyn Fn(&str)>;
type AskHook = Box<dyn Fn(&AppWindow, &str)>;
type GoogleSyncHook = Box<dyn Fn()>;

pub struct DashController {
    store: Rc<Store>,
    timers: Rc<VecModel<TimerRow>>,
    alarms: Rc<VecModel<AlarmRow>>,
    reminders: Rc<VecModel<ReminderRow>>,
    markers: Rc<VecModel<Marker>>,
    undo: RefCell<Option<Undo>>,
    info_strip: RefCell<String>,
    last_minute: Cell<u32>,
    location_hook: RefCell<Option<LocationHook>>,
    ask_hook: RefCell<Option<AskHook>>,
    google_sync_hook: RefCell<Option<GoogleSyncHook>>,
}

fn now() -> (DateTime<Utc>, NaiveDateTime) {
    let utc = Utc::now();
    (utc, utc.with_timezone(&Local).naive_local())
}

fn day_fraction(t: NaiveDateTime) -> f32 {
    t.num_seconds_from_midnight() as f32 / 86_400.0
}

fn timer_row(t: &Timer, now: DateTime<Utc>) -> TimerRow {
    TimerRow {
        id: t.id as i32,
        name: t.name.as_str().into(),
        total: duration_label(t.total).into(),
        remaining: countdown_label(t.remaining(now)).into(),
        progress: t.progress(now),
        paused: t.is_paused(),
        finished: t.is_finished(now),
    }
}

impl DashController {
    pub fn install(ui: &AppWindow, store: Rc<Store>, on_activity: impl Fn() + 'static) -> Rc<Self> {
        let this = Rc::new(Self {
            store,
            timers: Rc::new(VecModel::default()),
            alarms: Rc::new(VecModel::default()),
            reminders: Rc::new(VecModel::default()),
            markers: Rc::new(VecModel::default()),
            undo: RefCell::new(None),
            info_strip: RefCell::new(String::new()),
            last_minute: Cell::new(u32::MAX),
            location_hook: RefCell::new(None),
            ask_hook: RefCell::new(None),
            google_sync_hook: RefCell::new(None),
        });

        let dash = ui.global::<Dash>();
        dash.set_timers(ModelRc::from(this.timers.clone()));
        dash.set_alarms(ModelRc::from(this.alarms.clone()));
        dash.set_reminders(ModelRc::from(this.reminders.clone()));
        dash.set_markers(ModelRc::from(this.markers.clone()));
        dash.on_activity(on_activity);

        // Every handler: run the action, report errors in the command bar, refresh.
        macro_rules! handler {
            ($on:ident, |$c:ident, $ui:ident $(, $arg:ident : $ty:ty)*| $body:block) => {{
                let c = this.clone();
                let weak = ui.as_weak();
                dash.$on(move |$($arg: $ty),*| {
                    let Some($ui) = weak.upgrade() else { return };
                    let $c = &c;
                    let run = || -> anyhow::Result<()> { $body; Ok(()) };
                    let res = run();
                    if let Err(e) = res {
                        tracing::error!("{e:#}");
                        $c.show_result(&$ui, &format!("ERROR · {e}"), false, false);
                    }
                    $c.refresh(&$ui);
                });
            }};
        }

        handler!(on_submit, |c, ui, text: SharedString| {
            c.submit(&ui, &text)?
        });
        handler!(on_undo, |c, ui| {
            if let Some(u) = c.undo.borrow_mut().take() {
                actions::undo(&c.store, &u)?;
                c.request_google_sync();
                c.show_result(&ui, "UNDONE", false, false);
            }
        });
        handler!(on_timer_pause_toggle, |c, ui, id: i32| {
            let (utc, _) = now();
            if let Some(t) = c.store.timer(id.into())? {
                if t.is_paused() {
                    c.store.resume_timer(t.id, utc)?;
                } else {
                    c.store.pause_timer(t.id, utc)?;
                }
            }
        });
        handler!(on_timer_remove, |c, ui, id: i32| {
            if let Some(t) = c.store.delete_timer(id.into())? {
                let msg = format!("CANCELLED · {}", t.name.to_uppercase());
                c.offer_undo(&ui, &msg, Undo::RestoreTimers(vec![t]));
            }
        });
        handler!(on_alarm_toggle, |c, ui, id: i32| {
            let (_, local) = now();
            if let Some(a) = c.store.alarm(id.into())? {
                c.store.set_alarm_enabled(a.id, !a.enabled, local)?;
            }
        });
        handler!(on_alarm_remove, |c, ui, id: i32| {
            if let Some(a) = c.store.delete_alarm(id.into())? {
                let msg = format!("REMOVED · ALARM {}", time12(a.time_local));
                c.offer_undo(&ui, &msg, Undo::RestoreAlarm(a));
            }
        });
        handler!(on_reminder_toggle, |c, ui, id: i32| {
            let (utc, _) = now();
            if let Some(r) = c.store.reminder(id.into())? {
                c.store.set_reminder_done(r.id, !r.done, utc)?;
                c.request_google_sync();
            }
        });
        handler!(on_reminder_remove, |c, ui, id: i32| {
            if let Some(r) = c.store.delete_reminder(id.into())? {
                let msg = format!("REMOVED · {}", r.text.to_uppercase());
                c.offer_undo(&ui, &msg, Undo::RestoreReminder(r));
                c.request_google_sync();
            }
        });
        handler!(on_play_briefing, |c, ui| {
            c.show_result(&ui, "BRIEFING · VOICE ARRIVES IN PHASE 4", false, false);
        });

        this.refresh(ui);
        this
    }

    /// "location Austin, TX" in the command bar calls this.
    pub fn set_location_hook(&self, hook: impl Fn(&str) + 'static) {
        *self.location_hook.borrow_mut() = Some(Box::new(hook));
    }

    /// Anything that isn't a clock command goes here (the assistant).
    pub fn set_ask_hook(&self, hook: impl Fn(&AppWindow, &str) + 'static) {
        *self.ask_hook.borrow_mut() = Some(Box::new(hook));
    }

    pub fn set_google_sync_hook(&self, hook: impl Fn() + 'static) {
        *self.google_sync_hook.borrow_mut() = Some(Box::new(hook));
    }

    pub fn request_google_sync(&self) {
        if let Some(hook) = self.google_sync_hook.borrow().as_ref() {
            hook();
        }
    }

    fn submit(&self, ui: &AppWindow, text: &str) -> anyhow::Result<()> {
        let (utc, local) = now();
        let trimmed = text.trim();
        let lower = trimmed.to_lowercase();
        let prefix = ["set location ", "weather location ", "location "]
            .into_iter()
            .find(|p| lower.starts_with(p));
        if let (Some(p), Some(hook)) = (prefix, self.location_hook.borrow().as_ref()) {
            let query = trimmed[p.len()..].trim();
            hook(query);
            self.show_result(
                ui,
                &format!("LOOKING UP {} …", query.to_uppercase()),
                false,
                false,
            );
            ui.global::<Dash>().set_command(SharedString::new());
            return Ok(());
        }
        let Some(cmd) = command::parse(text, local) else {
            if trimmed.is_empty() {
                return Ok(());
            }
            match self.ask_hook.borrow().as_ref() {
                Some(ask) => {
                    self.show_result(ui, "", false, false);
                    ui.global::<Dash>().set_command(SharedString::new());
                    ask(ui, trimmed);
                }
                None => self.show_result(ui, "NOT A CLOCK COMMAND", false, false),
            }
            return Ok(());
        };
        let sync_google = matches!(&cmd, command::Command::AddReminder { time: None, .. });
        let applied = actions::apply(&self.store, cmd, utc, local)?;
        if sync_google {
            self.request_google_sync();
        }
        let summary = if applied.brief {
            "BRIEFING · VOICE ARRIVES IN PHASE 4".to_string()
        } else {
            applied.summary
        };
        let can_undo = applied.undo.is_some();
        *self.undo.borrow_mut() = applied.undo;
        self.show_result(ui, &summary, true, can_undo);
        ui.global::<Dash>().set_command(SharedString::new());
        Ok(())
    }

    /// A one-line message in the command bar (no UNDO).
    pub fn notify(&self, ui: &AppWindow, text: &str) {
        self.show_result(ui, text, false, false);
    }

    fn offer_undo(&self, ui: &AppWindow, msg: &str, undo: Undo) {
        *self.undo.borrow_mut() = Some(undo);
        self.show_result(ui, msg, false, true);
    }

    fn show_result(&self, ui: &AppWindow, text: &str, builtin: bool, can_undo: bool) {
        let dash = ui.global::<Dash>();
        dash.set_result(text.into());
        dash.set_result_builtin(builtin);
        dash.set_can_undo(can_undo);
        if !can_undo {
            self.undo.borrow_mut().take();
        }
    }

    /// Reload everything from the store.
    pub fn refresh(&self, ui: &AppWindow) {
        if let Err(e) = self.try_refresh(ui) {
            tracing::error!("refresh failed: {e:#}");
        }
    }

    fn try_refresh(&self, ui: &AppWindow) -> anyhow::Result<()> {
        let (utc, local) = now();
        let dash = ui.global::<Dash>();

        let timers = self.store.timers()?;
        self.timers
            .set_vec(timers.iter().map(|t| timer_row(t, utc)).collect::<Vec<_>>());

        let alarms = self.store.alarms()?;
        self.alarms.set_vec(
            alarms
                .iter()
                .map(|a| AlarmRow {
                    id: a.id as i32,
                    time: a.time_local.format("%-I:%M").to_string().into(),
                    ampm: a.time_local.format("%p").to_string().into(),
                    days: days_label(a.days).into(),
                    label: a.label.as_str().into(),
                    enabled: a.enabled,
                })
                .collect::<Vec<_>>(),
        );

        let reminders = self.store.reminders()?;
        self.reminders.set_vec(
            reminders
                .iter()
                .map(|r| ReminderRow {
                    id: r.id as i32,
                    text: r.text.as_str().into(),
                    when: match (r.due_date, r.due_time) {
                        (Some(d), Some(t)) => format!("{} {}", day_word(d, local), time12(t)),
                        (Some(d), None) => day_word(d, local),
                        _ => String::new(),
                    }
                    .into(),
                    tag: match (r.source, r.due_time) {
                        (Source::Google, None) => "GOOGLE",
                        (_, Some(_)) => "LOCAL",
                        _ => "",
                    }
                    .into(),
                    done: r.done,
                })
                .collect::<Vec<_>>(),
        );

        // Briefing facts + idle info strip
        let next = alarms
            .iter()
            .filter(|a| a.enabled)
            .filter_map(|a| a.next_fire_utc)
            .min()
            .map(|n| n.with_timezone(&Local).naive_local());
        let next_label = next.map(|n| {
            let day = day_word(n.date(), local);
            if day == "TODAY" {
                time12(n.time())
            } else {
                format!("{day} {}", time12(n.time()))
            }
        });
        let open = reminders.iter().filter(|r| !r.done).count();
        dash.set_next_alarm(next_label.clone().unwrap_or_else(|| "NONE".into()).into());
        dash.set_open_reminders(open.to_string().into());

        let mut strip = vec![match &next_label {
            Some(l) => format!("NEXT ALARM {l}"),
            None => "NO ALARMS".into(),
        }];
        strip.push(match timers.len() {
            0 => "NO TIMERS".into(),
            1 => "1 TIMER".into(),
            n => format!("{n} TIMERS"),
        });
        strip.push(match open {
            0 => "NO REMINDERS".into(),
            1 => "1 REMINDER".into(),
            n => format!("{n} REMINDERS"),
        });
        *self.info_strip.borrow_mut() = strip.join(" · ");

        // Timeline markers for today
        let today = local.date();
        let mut markers = Vec::new();
        // Every enabled alarm due in the next 24 hours, at its time of day;
        // ones whose time today has passed fire tomorrow and are drawn dimmer.
        for a in alarms.iter().filter(|a| a.enabled) {
            if let Some(n) = a
                .next_fire_utc
                .map(|n| n.with_timezone(&Local).naive_local())
            {
                if n >= local && n - local < chrono::Duration::hours(24) {
                    markers.push(Marker {
                        pos: day_fraction(n),
                        kind: 0,
                        label: time12(n.time()).into(),
                        later: n.date() != today,
                    });
                }
            }
        }
        for r in reminders.iter().filter(|r| !r.done) {
            if let Some(at) = r.fires_at().filter(|at| at.date() == today) {
                markers.push(Marker {
                    pos: day_fraction(at),
                    kind: 1,
                    ..Default::default()
                });
            }
        }
        for t in &timers {
            if let Some(end) = t.ends_utc.map(|e| e.with_timezone(&Local).naive_local()) {
                if end.date() == today && end > local {
                    markers.push(Marker {
                        pos: day_fraction(end),
                        kind: 2,
                        ..Default::default()
                    });
                }
            }
        }
        for event in ui.global::<Google>().get_events().iter() {
            if event.timed {
                markers.push(Marker {
                    pos: event.position,
                    kind: 3,
                    label: event.title,
                    later: false,
                });
            }
        }
        self.markers.set_vec(markers);
        self.last_minute.set(local.minute());
        Ok(())
    }

    /// Called from the 4 Hz tick: update timer countdowns in place.
    pub fn tick(&self, ui: &AppWindow) {
        let (utc, local) = now();
        if local.minute() != self.last_minute.get() {
            self.refresh(ui); // markers, "TODAY"/"TOMORROW" labels
            return;
        }
        let Ok(timers) = self.store.timers() else {
            return;
        };
        if timers.len() != self.timers.row_count() {
            return self.refresh(ui);
        }
        for (i, t) in timers.iter().enumerate() {
            let row = timer_row(t, utc);
            if self.timers.row_data(i).as_ref() != Some(&row) {
                self.timers.set_row_data(i, row);
            }
        }
    }

    pub fn info_strip(&self) -> String {
        self.info_strip.borrow().clone()
    }
}
