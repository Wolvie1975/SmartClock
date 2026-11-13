use chrono::{Datelike, Local, Timelike};
use clock_core::Store;
use slint::{ComponentHandle, Timer, TimerMode};
use std::{
    cell::Cell,
    rc::Rc,
    time::{Duration, Instant},
};

mod assistant;
mod dashboard;
mod inhibit;
mod ringing;
mod sound;
mod tools;
mod weather;

slint::include_modules!();

const DEFAULT_IDLE_MINUTES: u64 = 5; // Settings → Idle screen
/// Burn-in drift cycle on the idle screen, one step per minute (px).
const DRIFT: [(f32, f32); 6] = [
    (0., 0.),
    (3., 2.),
    (-2., 3.),
    (-3., -1.),
    (2., -3.),
    (1., 1.),
];

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
        60.0 * cli_idle
            .or(saved_idle)
            .unwrap_or(DEFAULT_IDLE_MINUTES as f64),
    );

    // Keep GNOME from blanking the screen. Not fatal if unavailable (e.g. non-GNOME session).
    let _inhibit = inhibit::IdleInhibit::acquire()
        .inspect_err(|e| tracing::warn!("could not inhibit idle: {e}"))
        .ok();

    let ui = AppWindow::new()?;
    // Matches the launcher's file name and StartupWMClass, so GNOME shows the
    // right name and icon for the window. Needs the platform (created with the
    // window) and must happen before the window is shown.
    if let Err(e) = slint::set_xdg_app_id("smart-clock") {
        tracing::warn!("couldn't set the app id: {e}");
    }
    let theme = ui.global::<Theme>();
    theme.set_dark(store.get_setting("theme")?.as_deref() != Some("light"));
    if store.get_setting("start_on")?.as_deref() == Some("idle") {
        ui.set_screen(Screen::Idle);
    }
    ui.set_tz_label(Local::now().format("%Z").to_string().into());

    let last_activity = Rc::new(Cell::new(Instant::now()));

    // Pointer movement or typing → dashboard + reset idle timer
    let activity: Rc<dyn Fn()> = {
        let weak = ui.as_weak();
        let last = last_activity.clone();
        Rc::new(move || {
            last.set(Instant::now());
            if let Some(ui) = weak.upgrade() {
                if ui.get_screen() == Screen::Idle {
                    ui.set_screen(Screen::Dashboard);
                }
            }
        })
    };
    ui.on_activity({
        let a = activity.clone();
        move || a()
    });
    let dash = dashboard::DashController::install(&ui, store.clone(), {
        let a = activity.clone();
        move || a()
    });
    let ringer = ringing::Ringer::install(&ui, store.clone(), dash.clone());
    let weather = weather::WeatherController::install(&ui, store.clone());
    dash.set_location_hook({
        let w = weather.clone();
        move |q| w.set_location(q)
    });
    let assistant =
        assistant::AssistantController::install(&ui, store.clone(), weather.clone(), dash.clone())?;
    dash.set_ask_hook({
        let a = assistant.clone();
        move |ui, q| a.ask(ui, q)
    });

    {
        let weak = ui.as_weak();
        let store = store.clone();
        // CLOSE in the header (after its confirm step). Leaving the event loop
        // ends `ui.run()`, so the idle inhibit and any sound are released normally.
        ui.on_quit(|| {
            if let Err(e) = slint::quit_event_loop() {
                tracing::error!("quit failed: {e}");
            }
        });
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
            ui.set_date_label(
                now.format("%A, %-d %B %Y")
                    .to_string()
                    .to_uppercase()
                    .into(),
            );
            ui.set_week_label(format!("WEEK {}", now.iso_week().week()).into());
            ui.set_day_progress(now.num_seconds_from_midnight() as f32 / 86_400.0);
            dash.tick(&ui);
            ringer.tick(&ui);
            weather.tick(&ui);
            assistant.tick(&ui);
            if let Some(note) = weather.located.borrow_mut().take() {
                dash.notify(&ui, &note);
            }
            ui.set_info_strip(dash.info_strip().into());

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
