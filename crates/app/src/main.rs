use chrono::{Datelike, Local, Timelike};
use clock_core::Store;
use slint::{ComponentHandle, Timer, TimerMode};
use std::{
    cell::Cell,
    rc::Rc,
    time::{Duration, Instant},
};

mod inhibit;

slint::include_modules!();

const DEFAULT_IDLE_MINUTES: u64 = 5; // Settings → Idle screen
/// Burn-in drift cycle on the idle screen, one step per minute (px).
const DRIFT: [(f32, f32); 6] = [(0., 0.), (3., 2.), (-2., 3.), (-3., -1.), (2., -3.), (1., 1.)];

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                // arboard warns about GNOME lacking wlr-data-control; harmless.
                .unwrap_or_else(|_| "info,arboard=error".into()),
        )
        .init();
    let windowed = std::env::args().any(|a| a == "--windowed");

    let store = Rc::new(Store::open(&Store::default_path())?);
    // `--idle-minutes N` overrides the saved setting for this run (handy for testing).
    let args: Vec<String> = std::env::args().collect();
    let cli_idle = args
        .iter()
        .position(|a| a == "--idle-minutes")
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.parse::<f64>().ok());
    let saved_idle = store
        .get_setting("idle_minutes")?
        .and_then(|v| v.parse::<f64>().ok());
    let idle_timeout = Duration::from_secs_f64(
        60.0 * cli_idle.or(saved_idle).unwrap_or(DEFAULT_IDLE_MINUTES as f64),
    );

    // Keep GNOME from blanking the screen. Not fatal if unavailable (e.g. non-GNOME session).
    let _inhibit = inhibit::IdleInhibit::acquire()
        .inspect_err(|e| tracing::warn!("could not inhibit idle: {e}"))
        .ok();

    let ui = AppWindow::new()?;
    let theme = ui.global::<Theme>();
    theme.set_dark(store.get_setting("theme")?.as_deref() != Some("light"));
    if store.get_setting("start_on")?.as_deref() == Some("idle") {
        ui.set_screen(Screen::Idle);
    }
    ui.set_tz_label(Local::now().format("%Z").to_string().into());

    let last_activity = Rc::new(Cell::new(Instant::now()));

    // Pointer movement → dashboard + reset idle timer
    {
        let weak = ui.as_weak();
        let last = last_activity.clone();
        ui.on_activity(move || {
            last.set(Instant::now());
            if let Some(ui) = weak.upgrade() {
                if ui.get_screen() == Screen::Idle {
                    ui.set_screen(Screen::Dashboard);
                }
            }
        });
    }

    {
        let weak = ui.as_weak();
        let store = store.clone();
        ui.on_toggle_theme(move || {
            let Some(ui) = weak.upgrade() else { return };
            let theme = ui.global::<Theme>();
            let dark = !theme.get_dark();
            theme.set_dark(dark);
            if let Err(e) = store.set_setting("theme", if dark { "dark" } else { "light" }) {
                tracing::warn!("saving theme failed: {e}");
            }
        });
    }

    // 4 Hz tick: clock text + idle check (+ timer/alarm checks later)
    let tick = Timer::default();
    {
        let weak = ui.as_weak();
        let last = last_activity.clone();
        tick.start(TimerMode::Repeated, Duration::from_millis(250), move || {
            let Some(ui) = weak.upgrade() else { return };
            let now = Local::now();
            let h12 = match now.hour() % 12 {
                0 => 12,
                h => h,
            };
            ui.set_time_hm(format!("{}:{:02}", h12, now.minute()).into());
            ui.set_time_ss(format!("{:02}", now.second()).into());
            ui.set_ampm(if now.hour() >= 12 { "PM" } else { "AM" }.into());
            ui.set_date_label(now.format("%A, %-d %B %Y").to_string().to_uppercase().into());
            ui.set_week_label(format!("WEEK {}", now.iso_week().week()).into());
            ui.set_day_progress(now.num_seconds_from_midnight() as f32 / 86_400.0);
            ui.set_info_strip("NO ALARMS · NO TIMERS · NO REMINDERS".into());

            let (dx, dy) = DRIFT[(now.minute() as usize) % DRIFT.len()];
            ui.set_drift_x(dx);
            ui.set_drift_y(dy);

            if ui.get_screen() == Screen::Dashboard && last.get().elapsed() >= idle_timeout {
                ui.set_screen(Screen::Idle);
            }
        });
    }

    if !windowed {
        ui.window().set_fullscreen(true);
    }
    ui.run()?;
    Ok(())
}
