//! Timer takeover and alarm ringing (DEVELOPMENT.md section 7).
//!
//! Each tick decides which overlay is up, fills the `Ring` global and drives the sound.
//! An alarm outranks a timer takeover; among timers, the one closest to finishing wins.

use crate::dashboard::DashController;
use crate::sound::{Pattern, Sound};
use crate::{AlarmPhase, AppWindow, Overlay, Ring};
use chrono::{DateTime, Local, NaiveDateTime, Timelike, Utc};
use clock_core::model::{countdown_label, time12, Alarm};
use clock_core::Store;
use slint::ComponentHandle;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

/// A timer takes over the screen for its last 10 seconds.
const TAKEOVER: Duration = Duration::from_secs(10);
/// Volume ramps from 20% to 100% over this long.
const RAMP: Duration = Duration::from_secs(30);
/// The Dismissed screen closes itself after this long.
const DISMISSED_CLOSE: Duration = Duration::from_secs(120);
/// An alarm found this late (the app was closed or asleep) is skipped, not rung.
const MISSED_AFTER: chrono::TimeDelta = chrono::TimeDelta::minutes(5);

enum Phase {
    Ringing { since: Instant },
    Snoozed { until: DateTime<Utc>, hidden: bool },
    Dismissed { since: Instant, note: String },
}

struct Active {
    alarm: Alarm,
    phase: Phase,
}

struct Prefs {
    snooze: Duration,
    autostop: Duration,
    ramp: bool,
    volume: f32,
}

pub struct Ringer {
    store: Rc<Store>,
    dash: Rc<DashController>,
    sound: RefCell<Sound>,
    active: RefCell<Option<Active>>,
    timer_id: Cell<Option<i64>>,
}

fn now() -> (DateTime<Utc>, NaiveDateTime) {
    let utc = Utc::now();
    (utc, utc.with_timezone(&Local).naive_local())
}

fn mm_ss(d: Duration) -> String {
    let s = d.as_secs();
    format!("{}:{:02}", s / 60, s % 60)
}

fn greeting(local: NaiveDateTime) -> &'static str {
    match local.hour() {
        0..=11 => "Good morning",
        12..=16 => "Good afternoon",
        _ => "Good evening",
    }
}

impl Ringer {
    pub fn install(ui: &AppWindow, store: Rc<Store>, dash: Rc<DashController>) -> Rc<Self> {
        let this = Rc::new(Self {
            store,
            dash,
            sound: RefCell::new(Sound::new()),
            active: RefCell::new(None),
            timer_id: Cell::new(None),
        });
        let ring = ui.global::<Ring>();

        macro_rules! handler {
            ($on:ident, |$r:ident, $ui:ident| $body:block) => {{
                let r = this.clone();
                let weak = ui.as_weak();
                ring.$on(move || {
                    let Some($ui) = weak.upgrade() else { return };
                    let $r = &r;
                    let run = || -> anyhow::Result<()> {
                        $body;
                        Ok(())
                    };
                    if let Err(e) = run() {
                        tracing::error!("{e:#}");
                    }
                    $r.dash.refresh(&$ui);
                    $r.tick(&$ui);
                });
            }};
        }

        handler!(on_timer_add_minute, |r, ui| {
            if let Some(id) = r.timer_id.get() {
                r.store
                    .add_timer_time(id, Duration::from_secs(60), Utc::now())?;
            }
        });
        handler!(on_timer_stop, |r, ui| {
            if let Some(id) = r.timer_id.get() {
                r.store.delete_timer(id)?;
            }
        });
        handler!(on_timer_dismiss, |r, ui| {
            if let Some(id) = r.timer_id.get() {
                r.store.delete_timer(id)?;
            }
        });
        handler!(on_timer_restart, |r, ui| {
            if let Some(id) = r.timer_id.get() {
                r.store.restart_timer(id, Utc::now())?;
            }
        });
        handler!(on_alarm_snooze, |r, ui| {
            let snooze = r.prefs()?.snooze;
            if let Some(a) = r.active.borrow_mut().as_mut() {
                let until = Utc::now() + chrono::Duration::from_std(snooze)?;
                a.phase = Phase::Snoozed {
                    until,
                    hidden: false,
                };
            }
        });
        handler!(on_alarm_dismiss, |r, ui| {
            if let Some(a) = r.active.borrow_mut().as_mut() {
                let note = Local::now().format("%A, %-d %B").to_string().to_uppercase();
                a.phase = Phase::Dismissed {
                    since: Instant::now(),
                    note,
                };
            }
        });
        handler!(on_alarm_close, |r, ui| {
            let mut active = r.active.borrow_mut();
            match active.as_mut().map(|a| &mut a.phase) {
                Some(Phase::Snoozed { until, hidden }) => {
                    *hidden = true;
                    let at = time12(until.with_timezone(&Local).time());
                    r.dash
                        .notify(&ui, &format!("SNOOZED · RINGS AGAIN AT {at}"));
                }
                Some(Phase::Dismissed { .. }) => *active = None,
                _ => {}
            }
        });
        this
    }

    fn prefs(&self) -> anyhow::Result<Prefs> {
        Ok(Prefs {
            snooze: Duration::from_secs(60 * self.store.setting_or("snooze_min", 9u64)?),
            autostop: Duration::from_secs(60 * self.store.setting_or("autostop_min", 10u64)?),
            ramp: self.store.setting_or("ramp", true)?,
            volume: self.store.setting_or("volume", 80u32)?.min(100) as f32 / 100.0,
        })
    }

    pub fn tick(&self, ui: &AppWindow) {
        if let Err(e) = self.try_tick(ui) {
            tracing::error!("ringing tick failed: {e:#}");
        }
    }

    fn try_tick(&self, ui: &AppWindow) -> anyhow::Result<()> {
        let (utc, local) = now();
        let prefs = self.prefs()?;
        let ring = ui.global::<Ring>();

        // 1. Alarms that have come due.
        let due = self.store.due_alarms(utc)?;
        if !due.is_empty() {
            for a in due {
                self.store.alarm_fired(a.id, local)?;
                let late = a.next_fire_utc.is_some_and(|at| utc - at > MISSED_AFTER);
                let busy = matches!(
                    self.active.borrow().as_ref().map(|a| &a.phase),
                    Some(Phase::Ringing { .. } | Phase::Snoozed { .. })
                );
                if late {
                    tracing::warn!(
                        alarm = a.id,
                        "missed alarm skipped (app was closed or asleep)"
                    );
                } else if busy {
                    tracing::warn!(alarm = a.id, "alarm came due while another is ringing");
                } else {
                    tracing::info!(alarm = a.id, "alarm ringing");
                    *self.active.borrow_mut() = Some(Active {
                        alarm: a,
                        phase: Phase::Ringing {
                            since: Instant::now(),
                        },
                    });
                }
            }
            self.dash.refresh(ui); // one-off alarms switched off, next fire moved
        }

        // 2. Alarm phase changes over time.
        {
            let mut active = self.active.borrow_mut();
            let next = match active.as_ref().map(|a| &a.phase) {
                Some(Phase::Ringing { since }) if since.elapsed() >= prefs.autostop => {
                    let mins = prefs.autostop.as_secs() / 60;
                    Some(Some(Phase::Dismissed {
                        since: Instant::now(),
                        note: format!("STOPPED AFTER {mins} MIN"),
                    }))
                }
                Some(Phase::Snoozed { until, .. }) if utc >= *until => Some(Some(Phase::Ringing {
                    since: Instant::now(),
                })),
                Some(Phase::Dismissed { since, .. }) if since.elapsed() >= DISMISSED_CLOSE => {
                    Some(None)
                }
                _ => None,
            };
            match next {
                Some(Some(p)) => active.as_mut().expect("active").phase = p,
                Some(None) => *active = None,
                None => {}
            }
        }

        // 3. Timer closest to finishing, within its last 10 s (or already finished).
        let timers = self.store.timers()?;
        let timer = timers
            .iter()
            .filter(|t| !t.is_paused() && t.remaining(utc) <= TAKEOVER)
            .min_by_key(|t| t.ends_utc);
        self.timer_id.set(timer.map(|t| t.id));

        // 4. Which overlay, and what it shows.
        let active = self.active.borrow();
        let alarm_visible = active
            .as_ref()
            .is_some_and(|a| !matches!(a.phase, Phase::Snoozed { hidden: true, .. }));
        ring.set_pulse(local.second() % 2 == 0);

        let mut sound: Option<(Pattern, f32)> = None;
        if let (true, Some(a)) = (alarm_visible, active.as_ref()) {
            ui.set_overlay(Overlay::Alarm);
            ring.set_alarm_label(
                if a.alarm.label.is_empty() {
                    format!("ALARM · {}", time12(a.alarm.time_local))
                } else {
                    format!("ALARM · {}", a.alarm.label.to_uppercase())
                }
                .into(),
            );
            ring.set_alarm_time(local.format("%-I:%M").to_string().into());
            ring.set_alarm_ampm(local.format("%p").to_string().into());
            ring.set_snooze_label(format!("SNOOZE · {} MIN", prefs.snooze.as_secs() / 60).into());
            match &a.phase {
                Phase::Ringing { since } => {
                    let el = since.elapsed();
                    let vol = if prefs.ramp {
                        0.2 + 0.8 * (el.as_secs_f32() / RAMP.as_secs_f32()).min(1.0)
                    } else {
                        1.0
                    };
                    ring.set_alarm_phase(AlarmPhase::Ringing);
                    ring.set_alarm_volume(vol);
                    ring.set_alarm_elapsed(format!("RINGING {}", mm_ss(el)).into());
                    sound = Some((Pattern::Alarm, prefs.volume * vol));
                }
                Phase::Snoozed { until, .. } => {
                    let left = (*until - utc).to_std().unwrap_or_default();
                    ring.set_alarm_phase(AlarmPhase::Snoozed);
                    ring.set_snooze_countdown(countdown_label(left).into());
                    let at = time12(until.with_timezone(&Local).time());
                    ring.set_snooze_again(format!("RINGS AGAIN AT {at}").into());
                }
                Phase::Dismissed { note, .. } => {
                    ring.set_alarm_phase(AlarmPhase::Dismissed);
                    ring.set_greeting(greeting(local).into());
                    ring.set_dismissed_note(note.as_str().into());
                }
            }
        } else if let Some(t) = timer {
            ui.set_overlay(Overlay::Timer);
            let left = t.remaining(utc);
            let secs = left.as_secs() + u64::from(left.subsec_nanos() > 0); // round up
            ring.set_timer_name(t.name.to_uppercase().into());
            ring.set_timer_count(secs.to_string().into());
            ring.set_timer_blocks(secs.min(10) as i32);
            ring.set_timer_up(t.is_finished(utc));
            let over_for = t.ends_utc.map(|e| utc - e).unwrap_or_default();
            if t.is_finished(utc) && over_for.to_std().unwrap_or_default() < prefs.autostop {
                sound = Some((Pattern::Timer, prefs.volume));
            }
        } else {
            ui.set_overlay(Overlay::None);
        }

        let mut s = self.sound.borrow_mut();
        match sound {
            Some((p, v)) => s.play(p, v),
            None => s.stop(),
        }
        Ok(())
    }
}
