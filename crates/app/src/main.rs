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
mod google_controller;
mod inhibit;
mod news_task;
mod ringing;
mod secrets;
mod sound;
mod tools;
mod weather;

slint::include_modules!();

const DEFAULT_IDLE_MINUTES: u64 = 5; // Settings → Idle screen
const DEFAULT_OLLAMA_URL: &str = "http://127.0.0.1:11434";
const DEFAULT_OLLAMA_MODEL: &str = "llama3.2:3b";
const DEFAULT_CLOUD_PROVIDER: &str = "ANTHROPIC";
const DEFAULT_CLOUD_MODEL: &str = "claude-opus-5-5";
const DEFAULT_CLOCK_FACE: &str = "digital";
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
    let saved_clock_face = store.get_setting("clock_face")?;
    let clock_face = match saved_clock_face.as_deref() {
        Some(face @ ("digital" | "chronograph" | "split-flap" | "tape" | "dot-matrix")) => {
            face.to_string()
        }
        None => DEFAULT_CLOCK_FACE.to_string(),
        Some(invalid) => {
            tracing::warn!("unknown saved clock face {invalid:?}; using digital");
            DEFAULT_CLOCK_FACE.to_string()
        }
    };
    ui.set_clock_face(clock_face.into());
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
    let google = google_controller::GoogleController::install(&ui, store.clone())?;
    let news = news_task::NewsController::install(&ui, store.clone())?;
    dash.set_google_sync_hook({
        let google = google.clone();
        move || google.request_sync()
    });
    let ringer = ringing::Ringer::install(&ui, store.clone(), dash.clone());
    let weather = weather::WeatherController::install(&ui, store.clone());
    dash.set_location_hook({
        let w = weather.clone();
        move |q| w.set_location(q)
    });
    let assistant =
        assistant::AssistantController::install(&ui, store.clone(), weather.clone(), dash.clone())?;
    let local_url = store
        .get_setting("ollama_url")?
        .unwrap_or_else(|| DEFAULT_OLLAMA_URL.to_string());
    let local_model = store
        .get_setting("ollama_model")?
        .unwrap_or_else(|| DEFAULT_OLLAMA_MODEL.to_string());
    let cloud_provider = store
        .get_setting("cloud_provider")?
        .unwrap_or_else(|| DEFAULT_CLOUD_PROVIDER.to_string());
    let cloud_model = store
        .get_setting("cloud_model")?
        .unwrap_or_else(|| DEFAULT_CLOUD_MODEL.to_string());
    let legacy_api_key = store.get_setting("cloud_api_key")?;
    let keyring_api_key = secrets::get_blocking("cloud-api-key");
    let api_key = match keyring_api_key {
        Ok(Some(key)) => key,
        keyring_result => {
            if let Err(e) = keyring_result {
                tracing::warn!("loading cloud API key from the system keyring failed: {e:#}");
            }
            match legacy_api_key.as_deref().filter(|key| !key.is_empty()) {
                Some(key) => match secrets::set_blocking("cloud-api-key", Some(key)) {
                    Ok(()) => key.to_owned(),
                    Err(e) => {
                        tracing::warn!(
                            "migrating cloud API key to the system keyring failed: {e:#}"
                        );
                        String::new()
                    }
                },
                None => String::new(),
            }
        }
    };
    if let Err(e) = store.delete_setting("cloud_api_key") {
        tracing::warn!("removing legacy cloud API key from settings failed: {e}");
    }
    ui.set_ai_local_url(local_url.clone().into());
    ui.set_ai_local_model(local_model.clone().into());
    ui.set_ai_cloud_provider(cloud_provider.clone().into());
    ui.set_ai_cloud_model(cloud_model.clone().into());
    ui.set_ai_api_key(api_key.clone().into());
    ui.set_ai_local_status(ui.global::<Chat>().get_local_status());
    ui.set_ai_cloud_status(ui.global::<Chat>().get_cloud_status());
    dash.set_ask_hook({
        let a = assistant.clone();
        move |ui, q| a.ask(ui, q)
    });
    ui.on_save_settings({
        let weak = ui.as_weak();
        let store = store.clone();
        let google = google.clone();
        move |local_url: slint::SharedString,
              local_model: slint::SharedString,
              cloud_provider: slint::SharedString,
              cloud_model: slint::SharedString,
              api_key: slint::SharedString,
              google_client_id: slint::SharedString,
              google_tasks: bool| {
            let local_url = local_url.as_str().trim();
            let local_model = local_model.as_str().trim();
            let cloud_provider = cloud_provider.as_str().trim().to_uppercase();
            let cloud_model = cloud_model.as_str().trim();
            let api_key = api_key.as_str().trim();
            let google_client_id = google_client_id.as_str().trim();
            let mut save_succeeded = true;
            if let Err(e) = store.set_setting("ollama_url", local_url) {
                tracing::warn!("saving ollama_url failed: {e}");
                save_succeeded = false;
            }
            if let Err(e) = store.set_setting("ollama_model", local_model) {
                tracing::warn!("saving ollama_model failed: {e}");
                save_succeeded = false;
            }
            if let Err(e) = store.set_setting("cloud_provider", &cloud_provider) {
                tracing::warn!("saving cloud_provider failed: {e}");
                save_succeeded = false;
            }
            if let Err(e) = store.set_setting("cloud_model", cloud_model) {
                tracing::warn!("saving cloud_model failed: {e}");
                save_succeeded = false;
            }
            let key_saved = match secrets::set_blocking(
                "cloud-api-key",
                (!api_key.is_empty()).then_some(api_key),
            ) {
                Ok(()) => true,
                Err(e) => {
                    tracing::warn!("saving cloud API key to the system keyring failed: {e:#}");
                    false
                }
            };
            if !key_saved {
                save_succeeded = false;
            }
            let reconnect_required = match google.save_preferences(google_client_id, google_tasks) {
                Ok(reconnect_required) => reconnect_required,
                Err(e) => {
                    tracing::warn!("saving Google settings failed: {e:#}");
                    save_succeeded = false;
                    false
                }
            };
            if let Some(ui) = weak.upgrade() {
                let chat = ui.global::<Chat>();
                chat.set_local_model(local_model.into());
                chat.set_local_address(local_url.into());
                if key_saved && !cloud_provider.is_empty() && !api_key.is_empty() {
                    chat.set_cloud_status("READY".into());
                    ui.set_ai_cloud_status("READY".into());
                } else {
                    chat.set_cloud_status("NOT SET UP".into());
                    ui.set_ai_cloud_status("NOT SET UP".into());
                }
                ui.set_ai_local_url(local_url.into());
                ui.set_ai_local_model(local_model.into());
                ui.set_ai_cloud_provider(cloud_provider.into());
                ui.set_ai_cloud_model(cloud_model.into());
                ui.set_ai_api_key(api_key.into());
                ui.set_google_client_id(google_client_id.into());
                ui.set_google_tasks(google_tasks);
                ui.set_settings_save_status(
                    if save_succeeded {
                        "SAVED"
                    } else {
                        "SAVE FAILED"
                    }
                    .into(),
                );
                if reconnect_required {
                    ui.set_google_status("RECONNECT TO APPLY SETTINGS".into());
                }
            }
        }
    });

    ui.on_clock_face_changed({
        let weak = ui.as_weak();
        let store = store.clone();
        move |face: slint::SharedString| {
            let face = face.as_str();
            if !matches!(
                face,
                "digital" | "chronograph" | "split-flap" | "tape" | "dot-matrix"
            ) {
                tracing::error!("ignoring unknown clock face selection {face:?}");
                if let Some(ui) = weak.upgrade() {
                    ui.set_settings_save_status("SAVE FAILED".into());
                }
                return;
            }
            match store.set_setting("clock_face", face) {
                Ok(()) => {
                    if let Some(ui) = weak.upgrade() {
                        ui.set_clock_face(face.into());
                        ui.set_settings_save_status("SAVED".into());
                    }
                }
                Err(e) => {
                    tracing::warn!("saving clock face failed: {e}");
                    if let Some(ui) = weak.upgrade() {
                        ui.set_settings_save_status("SAVE FAILED".into());
                    }
                }
            }
        }
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
            ui.set_clock_hour(h12 as i32);
            ui.set_clock_minute(now.minute() as i32);
            ui.set_clock_second(now.second() as i32);
            ui.set_hour_label(format!("{h12:02}").into());
            ui.set_minute_label(format!("{:02}", now.minute()).into());
            ui.set_hour_tens((h12 / 10).to_string().into());
            ui.set_hour_ones((h12 % 10).to_string().into());
            ui.set_minute_tens((now.minute() / 10).to_string().into());
            ui.set_minute_ones((now.minute() % 10).to_string().into());
            ui.set_previous_minute(format!("{:02}", (now.minute() + 59) % 60).into());
            ui.set_next_minute(format!("{:02}", (now.minute() + 1) % 60).into());
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
            google.tick(&ui, &dash);
            news.tick(&ui);
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
