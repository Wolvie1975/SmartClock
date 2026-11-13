use crate::secrets;
use crate::{AppWindow, CalendarEventRow, Google};
use anyhow::{Context, Result};
use chrono::{DateTime, Local, NaiveDate, NaiveTime, TimeZone, Timelike, Utc};
use clock_core::{
    model::{GoogleTaskUpdate, Reminder},
    Store,
};
use google::{oauth, CalendarEvent, EventStart, GoogleApi, Task, TaskWrite, TASKS_SCOPE};
use slint::{ComponentHandle, ModelRc, VecModel};
use std::{
    cell::{Cell, RefCell},
    collections::HashSet,
    rc::Rc,
    sync::mpsc,
    time::{Duration, Instant},
};
use tokio::{
    runtime::{Builder, Runtime},
    task::JoinHandle,
};
use zbus::{Connection, Proxy};

const REFRESH_INTERVAL: Duration = Duration::from_secs(5 * 60);
const CLIENT_ID_SETTING: &str = "google_client_id";
const TASKS_SETTING: &str = "google_tasks_enabled";
const TASK_LIST_SETTING: &str = "google_task_list_id";
const RECONNECT_SETTING: &str = "google_reconnect_required";

enum Event {
    Authorization(Result<(), String>),
    Disconnected(Result<(), String>),
    Calendar(Result<Vec<CalendarEvent>, String>),
    Synced(Result<TaskSync, String>),
}

struct LocalTask {
    id: i64,
    text: String,
    date: Option<NaiveDate>,
    done: bool,
    task_id: Option<String>,
    list_id: Option<String>,
    updated: DateTime<Utc>,
}

struct TaskChange {
    local_id: Option<i64>,
    task: Task,
}

struct TaskSync {
    list_id: String,
    changes: Vec<TaskChange>,
    deleted_local_ids: Vec<i64>,
    cleared_tombstones: Vec<String>,
}

pub struct GoogleController {
    store: Rc<Store>,
    runtime: Runtime,
    tx: mpsc::Sender<Event>,
    rx: mpsc::Receiver<Event>,
    job: RefCell<Option<JoinHandle<()>>>,
    connected: Cell<bool>,
    tasks_enabled: Cell<bool>,
    busy: Cell<bool>,
    calendar_enabled: Cell<bool>,
    next_sync: Cell<Instant>,
    next_calendar: Cell<Instant>,
    sync_requested: Cell<bool>,
}

impl GoogleController {
    pub fn install(ui: &AppWindow, store: Rc<Store>) -> Result<Rc<Self>> {
        let runtime = Builder::new_multi_thread()
            .worker_threads(1)
            .thread_name("google")
            .enable_all()
            .build()
            .context("building Google runtime")?;
        let (tx, rx) = mpsc::channel();
        let tasks_enabled = store.setting_or(TASKS_SETTING, true)?;
        store.delete_setting("google_mail_enabled")?;
        store.delete_setting("google_mail_history_id")?;
        let calendar_enabled = store.setting_or("google_calendar_enabled", false)?;
        let reconnect_required = store.setting_or(RECONNECT_SETTING, false)?;
        let client_id = store.get_setting(CLIENT_ID_SETTING)?.unwrap_or_default();
        let this = Rc::new(Self {
            store,
            runtime,
            tx,
            rx,
            job: RefCell::new(None),
            connected: Cell::new(false),
            tasks_enabled: Cell::new(tasks_enabled),
            busy: Cell::new(false),
            calendar_enabled: Cell::new(calendar_enabled),
            next_sync: Cell::new(Instant::now()),
            next_calendar: Cell::new(Instant::now()),
            sync_requested: Cell::new(false),
        });

        ui.set_google_client_id(client_id.into());
        ui.set_google_tasks(tasks_enabled);
        ui.set_google_calendar_status(
            if calendar_enabled {
                "CHECKING…"
            } else {
                "NOT CONNECTED"
            }
            .into(),
        );
        {
            let global = ui.global::<Google>();
            global.set_events(ModelRc::from(Rc::new(VecModel::default())));
            global.set_tasks_status("NOT CONNECTED".into());
            global.set_calendar_status(
                if calendar_enabled {
                    "CHECKING…"
                } else {
                    "NOT CONNECTED"
                }
                .into(),
            );
        }

        if calendar_enabled {
            this.start_calendar_fetch();
        }
        match secrets::get_blocking("google-refresh-token") {
            Ok(Some(_)) if reconnect_required => {
                ui.set_google_status("RECONNECT TO APPLY SETTINGS".into());
                ui.global::<Google>()
                    .set_tasks_status("RECONNECT REQUIRED".into());
            }
            Ok(Some(_)) => {
                this.connected.set(true);
                ui.set_google_status("CONNECTED".into());
                ui.global::<Google>().set_tasks_status("CONNECTED".into());
                this.sync_requested.set(true);
            }
            Ok(None) => ui.set_google_status("NOT CONNECTED".into()),
            Err(error) => {
                tracing::warn!("loading Google credentials failed: {error:#}");
                ui.set_google_status("KEYRING UNAVAILABLE".into());
            }
        }

        let weak = ui.as_weak();
        let controller = this.clone();
        ui.on_connect_google(move |client_id, tasks| {
            if let Some(ui) = weak.upgrade() {
                controller.connect(&ui, &client_id, tasks);
            }
        });
        let weak = ui.as_weak();
        let controller = this.clone();
        ui.on_disconnect_google(move || {
            if let Some(ui) = weak.upgrade() {
                controller.disconnect(&ui);
            }
        });
        let weak = ui.as_weak();
        let controller = this.clone();
        ui.on_connect_calendar(move || {
            if let Some(ui) = weak.upgrade() {
                controller.connect_calendar(&ui);
            }
        });
        Ok(this)
    }

    fn connect(&self, ui: &AppWindow, client_id: &str, tasks: bool) {
        let client_id = client_id.trim().to_owned();
        if client_id.is_empty() {
            ui.set_google_status("CLIENT ID REQUIRED".into());
            return;
        }
        if !tasks {
            ui.set_google_status("SELECT TASKS".into());
            return;
        }
        if let Err(error) = self.save_preferences(&client_id, tasks) {
            tracing::error!("saving Google settings failed: {error:#}");
            ui.set_google_status("SETTINGS ERROR".into());
            return;
        }
        if let Err(error) = self.store.set_setting(RECONNECT_SETTING, "true") {
            tracing::error!("saving Google reconnect state failed: {error}");
            ui.set_google_status("SETTINGS ERROR".into());
            return;
        }
        self.abort_job();
        self.connected.set(false);
        self.sync_requested.set(false);
        self.busy.set(true);
        ui.set_google_busy(true);
        ui.set_google_status("OPENING GOOGLE…".into());
        let tx = self.tx.clone();
        let handle = self.runtime.spawn(async move {
            let result = async {
                let tokens = oauth::authorize_loopback(&client_id, None, &[TASKS_SCOPE]).await?;
                let refresh_token = tokens.refresh_token.context(
                    "Google did not return a refresh token; revoke access and reconnect",
                )?;
                secrets::set("google-refresh-token", Some(&refresh_token)).await?;
                Ok::<_, anyhow::Error>(())
            }
            .await
            .map_err(|error| format!("{error:#}"));
            let _ = tx.send(Event::Authorization(result));
        });
        *self.job.borrow_mut() = Some(handle);
    }

    pub fn save_preferences(&self, client_id: &str, tasks: bool) -> Result<bool> {
        let previous_client_id = self
            .store
            .get_setting(CLIENT_ID_SETTING)?
            .unwrap_or_default();
        let previous_tasks = self.store.setting_or(TASKS_SETTING, true)?;
        let changed = previous_client_id != client_id || previous_tasks != tasks;
        let reconnect_required = self.connected.get() && changed;
        if reconnect_required {
            self.store.set_setting(RECONNECT_SETTING, "true")?;
            self.connected.set(false);
            self.sync_requested.set(false);
        }
        self.store.set_setting(CLIENT_ID_SETTING, client_id)?;
        self.store
            .set_setting(TASKS_SETTING, if tasks { "true" } else { "false" })?;
        self.tasks_enabled.set(tasks);
        Ok(reconnect_required)
    }

    fn disconnect(&self, ui: &AppWindow) {
        self.abort_job();
        self.busy.set(true);
        self.connected.set(false);
        self.sync_requested.set(false);
        if let Err(error) = self.store.set_setting(RECONNECT_SETTING, "false") {
            tracing::error!("clearing Google reconnect state failed: {error}");
        }
        ui.set_google_busy(true);
        ui.set_google_status("DISCONNECTING…".into());
        let tx = self.tx.clone();
        let handle = self.runtime.spawn(async move {
            let result = secrets::set("google-refresh-token", None)
                .await
                .map_err(|error| format!("{error:#}"));
            let _ = tx.send(Event::Disconnected(result));
        });
        *self.job.borrow_mut() = Some(handle);
    }

    fn connect_calendar(&self, ui: &AppWindow) {
        self.calendar_enabled.set(true);
        if let Err(error) = self.store.set_setting("google_calendar_enabled", "true") {
            tracing::error!("saving Google Calendar setting failed: {error}");
            ui.set_google_calendar_status("SETTINGS ERROR".into());
            return;
        }
        self.start_calendar_fetch();
        ui.set_google_calendar_status("CONNECTING…".into());
        ui.global::<Google>()
            .set_calendar_status("CONNECTING…".into());
    }

    pub fn request_sync(&self) {
        self.sync_requested.set(true);
        self.next_sync.set(Instant::now());
    }

    fn start_calendar_fetch(&self) {
        if self.busy.replace(true) {
            return;
        }
        let tx = self.tx.clone();
        let handle = self.runtime.spawn(async move {
            let result = goa_calendar_today()
                .await
                .map_err(|error| format!("{error:#}"));
            let _ = tx.send(Event::Calendar(result));
        });
        *self.job.borrow_mut() = Some(handle);
    }

    fn abort_job(&self) {
        if let Some(handle) = self.job.borrow_mut().take() {
            handle.abort();
        }
        self.busy.set(false);
    }

    pub fn tick(&self, ui: &AppWindow, dash: &crate::dashboard::DashController) {
        while let Ok(event) = self.rx.try_recv() {
            match event {
                Event::Authorization(result) => {
                    self.busy.set(false);
                    ui.set_google_busy(false);
                    match result {
                        Ok(()) => {
                            if let Err(error) = self.store.set_setting(RECONNECT_SETTING, "false") {
                                tracing::error!("clearing Google reconnect state failed: {error}");
                                ui.set_google_status("SETTINGS ERROR".into());
                                return;
                            }
                            self.connected.set(true);
                            ui.set_google_status("CONNECTED".into());
                            ui.global::<Google>().set_tasks_status("CONNECTED".into());
                            self.sync_requested.set(true);
                            self.next_sync.set(Instant::now());
                        }
                        Err(error) => {
                            tracing::warn!("Google authorization failed: {error}");
                            ui.set_google_status("CONNECTION FAILED".into());
                        }
                    }
                }
                Event::Disconnected(result) => {
                    self.busy.set(false);
                    ui.set_google_busy(false);
                    match result {
                        Ok(()) => {
                            ui.set_google_status("NOT CONNECTED".into());
                            ui.global::<Google>()
                                .set_tasks_status("NOT CONNECTED".into());
                        }
                        Err(error) => {
                            tracing::error!("Google disconnect failed: {error}");
                            ui.set_google_status("DISCONNECT FAILED".into());
                        }
                    }
                }
                Event::Calendar(result) => {
                    self.busy.set(false);
                    self.next_calendar.set(Instant::now() + REFRESH_INTERVAL);
                    match result {
                        Ok(events) => {
                            ui.set_google_calendar_status("READY".into());
                            ui.global::<Google>().set_calendar_status("READY".into());
                            let rows = calendar_rows(events);
                            ui.global::<Google>()
                                .set_events(ModelRc::from(Rc::new(VecModel::from(rows))));
                            dash.refresh(ui);
                        }
                        Err(error) => {
                            tracing::warn!("Google Calendar fetch failed: {error}");
                            let status = if error.contains("No Google Calendar account") {
                                "NO GOOGLE CALENDAR ACCOUNT"
                            } else {
                                "UNAVAILABLE"
                            };
                            ui.set_google_calendar_status(status.into());
                            ui.global::<Google>().set_calendar_status(status.into());
                        }
                    }
                }
                Event::Synced(result) => {
                    self.busy.set(false);
                    self.apply_sync(ui, dash, result);
                    self.next_sync.set(Instant::now() + REFRESH_INTERVAL);
                }
            }
        }

        if self.calendar_enabled.get()
            && !self.busy.get()
            && Instant::now() >= self.next_calendar.get()
        {
            self.start_calendar_fetch();
        }
        if self.connected.get()
            && !self.busy.get()
            && (self.sync_requested.get() || Instant::now() >= self.next_sync.get())
        {
            self.sync_requested.set(false);
            self.start_sync();
        }
    }

    fn start_sync(&self) {
        if self.busy.replace(true) {
            self.sync_requested.set(true);
            return;
        }
        let tasks = self.tasks_enabled.get();
        if !tasks {
            self.busy.set(false);
            self.next_sync.set(Instant::now() + REFRESH_INTERVAL);
            return;
        }
        let client_id = match self.store.get_setting(CLIENT_ID_SETTING) {
            Ok(Some(value)) => value,
            Ok(None) => {
                self.busy.set(false);
                self.next_sync.set(Instant::now() + REFRESH_INTERVAL);
                return;
            }
            Err(error) => {
                tracing::error!("reading Google client ID failed: {error}");
                self.busy.set(false);
                self.next_sync.set(Instant::now() + REFRESH_INTERVAL);
                return;
            }
        };
        let preferred_list = match self.store.get_setting(TASK_LIST_SETTING) {
            Ok(value) => value,
            Err(error) => {
                tracing::error!("reading Google task list failed: {error}");
                self.busy.set(false);
                self.next_sync.set(Instant::now() + REFRESH_INTERVAL);
                return;
            }
        };
        let local = match self.store.reminders() {
            Ok(reminders) => reminders
                .into_iter()
                .filter(|reminder| reminder.due_time.is_none())
                .map(local_task)
                .collect(),
            Err(error) => {
                tracing::error!("reading reminders for Google sync failed: {error}");
                self.busy.set(false);
                self.next_sync.set(Instant::now() + REFRESH_INTERVAL);
                return;
            }
        };
        let tombstones = match self.store.google_task_tombstones() {
            Ok(value) => value,
            Err(error) => {
                tracing::error!("reading Google task deletions failed: {error}");
                self.busy.set(false);
                self.next_sync.set(Instant::now() + REFRESH_INTERVAL);
                return;
            }
        };
        let tx = self.tx.clone();
        let handle = self.runtime.spawn(async move {
            let result = sync_google(&client_id, preferred_list, local, tombstones)
                .await
                .map_err(|error| format!("{error:#}"));
            let _ = tx.send(Event::Synced(result));
        });
        *self.job.borrow_mut() = Some(handle);
    }

    fn apply_sync(
        &self,
        ui: &AppWindow,
        dash: &crate::dashboard::DashController,
        result: Result<TaskSync, String>,
    ) {
        match result {
            Ok(sync) => {
                if let Err(error) = self.store.set_setting(TASK_LIST_SETTING, &sync.list_id) {
                    tracing::error!("saving Google task list failed: {error}");
                }
                let mut apply_failed = false;
                for local_id in sync.deleted_local_ids {
                    if let Err(error) = self.store.delete_google_reminder(local_id) {
                        tracing::error!("removing deleted Google reminder failed: {error}");
                        apply_failed = true;
                    }
                }
                for change in sync.changes {
                    if let Err(error) = self.apply_task(change, &sync.list_id) {
                        tracing::error!("applying Google task failed: {error:#}");
                        apply_failed = true;
                    }
                }
                for task_id in sync.cleared_tombstones {
                    if let Err(error) = self.store.clear_google_task_tombstone(&task_id) {
                        tracing::error!("clearing Google task deletion failed: {error}");
                        apply_failed = true;
                    }
                }
                let status = if apply_failed {
                    "SYNC NEEDS ATTENTION"
                } else {
                    "SYNCED"
                };
                ui.global::<Google>().set_tasks_status(status.into());
                ui.set_google_status(status.into());
                dash.refresh(ui);
            }
            Err(error) => {
                tracing::warn!("Google Tasks sync failed: {error}");
                ui.global::<Google>().set_tasks_status("SYNC ERROR".into());
                ui.set_google_status("SYNC ERROR".into());
            }
        }
    }

    fn apply_task(&self, change: TaskChange, list_id: &str) -> Result<()> {
        let task = change.task;
        let due_date = task.due_date()?;
        let updated = task.updated.unwrap_or_else(Utc::now);
        match change.local_id {
            Some(id) => {
                if self
                    .store
                    .reminder(id)?
                    .is_some_and(|reminder| reminder.due_time.is_none())
                {
                    self.store.update_google_reminder(
                        id,
                        GoogleTaskUpdate {
                            text: &task.title,
                            due_date,
                            done: task.done(),
                            task_id: &task.id,
                            list_id,
                            etag: task.etag.as_deref(),
                            updated_utc: updated,
                        },
                    )?;
                }
            }
            None => {
                if self.store.reminder_by_google_task(&task.id)?.is_none() {
                    self.store.add_google_reminder(GoogleTaskUpdate {
                        text: &task.title,
                        due_date,
                        done: task.done(),
                        task_id: &task.id,
                        list_id,
                        etag: task.etag.as_deref(),
                        updated_utc: updated,
                    })?;
                }
            }
        }
        Ok(())
    }
}

fn local_task(reminder: Reminder) -> LocalTask {
    LocalTask {
        id: reminder.id,
        text: reminder.text,
        date: reminder.due_date,
        done: reminder.done,
        task_id: reminder.google_task_id,
        list_id: reminder.google_list_id,
        updated: reminder.updated_utc,
    }
}

async fn sync_google(
    client_id: &str,
    preferred_list: Option<String>,
    local: Vec<LocalTask>,
    tombstones: Vec<(String, String)>,
) -> Result<TaskSync> {
    let token = async {
        let refresh_token = secrets::get("google-refresh-token")
            .await?
            .context("Google refresh token is missing")?;
        let tokens = oauth::refresh_access_token(client_id, None, &refresh_token).await?;
        if let Some(refreshed) = tokens.refresh_token.as_deref() {
            secrets::set("google-refresh-token", Some(refreshed)).await?;
        }
        Ok::<_, anyhow::Error>(tokens.access_token)
    }
    .await;
    let access_token = token?;
    let api = GoogleApi::new(&access_token)?;
    sync_tasks(&api, preferred_list.as_deref(), local, tombstones).await
}

async fn sync_tasks(
    api: &GoogleApi,
    preferred_list: Option<&str>,
    local: Vec<LocalTask>,
    tombstones: Vec<(String, String)>,
) -> Result<TaskSync> {
    let lists = api.task_lists().await?;
    let list = if let Some(preferred) = preferred_list {
        lists
            .iter()
            .find(|list| list.id == preferred)
            .with_context(|| format!("configured Google Tasks list {preferred} is unavailable"))?
    } else {
        lists.first().context("Google account has no task lists")?
    };
    let list_id = list.id.clone();
    let remote = api.tasks(&list_id).await?;
    let mut claimed = HashSet::new();
    let mut changes = Vec::new();
    let mut deleted_local_ids = Vec::new();
    let mut cleared_tombstones = Vec::new();

    for (task_id, tombstone_list) in tombstones {
        if tombstone_list == list_id {
            api.delete_task(&list_id, &task_id).await?;
            claimed.insert(task_id.clone());
            cleared_tombstones.push(task_id);
        }
    }

    for item in &local {
        if item.list_id.as_deref().is_some_and(|id| id != list_id) {
            continue;
        }
        let Some(task_id) = item.task_id.as_deref() else {
            let task = api
                .create_task(
                    &list_id,
                    &TaskWrite::from_local(&item.text, item.date, item.done),
                )
                .await?;
            claimed.insert(task.id.clone());
            changes.push(TaskChange {
                local_id: Some(item.id),
                task,
            });
            continue;
        };
        let Some(task) = remote.iter().find(|task| task.id == task_id) else {
            let task = api
                .create_task(
                    &list_id,
                    &TaskWrite::from_local(&item.text, item.date, item.done),
                )
                .await?;
            claimed.insert(task.id.clone());
            changes.push(TaskChange {
                local_id: Some(item.id),
                task,
            });
            continue;
        };
        claimed.insert(task.id.clone());
        if task.deleted {
            if item.updated > task.updated.unwrap_or(DateTime::<Utc>::MIN_UTC) {
                let replacement = api
                    .create_task(
                        &list_id,
                        &TaskWrite::from_local(&item.text, item.date, item.done),
                    )
                    .await?;
                claimed.insert(replacement.id.clone());
                changes.push(TaskChange {
                    local_id: Some(item.id),
                    task: replacement,
                });
            } else {
                deleted_local_ids.push(item.id);
            }
        } else if item.updated > task.updated.unwrap_or(DateTime::<Utc>::MIN_UTC) {
            let updated = api
                .update_task(
                    &list_id,
                    &task.id,
                    task.etag.as_deref(),
                    &TaskWrite::from_local(&item.text, item.date, item.done),
                )
                .await?;
            changes.push(TaskChange {
                local_id: Some(item.id),
                task: updated,
            });
        } else {
            changes.push(TaskChange {
                local_id: Some(item.id),
                task: task.clone(),
            });
        }
    }

    for task in remote {
        if !task.deleted && !claimed.contains(&task.id) {
            changes.push(TaskChange {
                local_id: None,
                task,
            });
        }
    }

    Ok(TaskSync {
        list_id,
        changes,
        deleted_local_ids,
        cleared_tombstones,
    })
}

async fn goa_calendar_today() -> Result<Vec<CalendarEvent>> {
    let connection = Connection::session()
        .await
        .context("connecting to the session D-Bus")?;
    let object_manager = Proxy::new(
        &connection,
        "org.gnome.OnlineAccounts",
        "/org/gnome/OnlineAccounts",
        "org.freedesktop.DBus.ObjectManager",
    )
    .await
    .context("connecting to the GNOME Online Accounts object manager")?;
    let objects: zbus::fdo::ManagedObjects = object_manager
        .call("GetManagedObjects", &())
        .await
        .context("listing GNOME Online Accounts")?;
    let mut token = None;
    for (path, interfaces) in objects {
        if !interfaces
            .keys()
            .any(|interface| interface.as_str() == "org.gnome.OnlineAccounts.Account")
        {
            continue;
        }
        let account = Proxy::new(
            &connection,
            "org.gnome.OnlineAccounts",
            path,
            "org.gnome.OnlineAccounts.Account",
        )
        .await
        .context("connecting to a GNOME Online Account")?;
        let provider: String = account
            .get_property("ProviderType")
            .await
            .context("reading GNOME account provider")?;
        if !provider.eq_ignore_ascii_case("google") {
            continue;
        }
        let calendar_disabled: bool = account
            .get_property("CalendarDisabled")
            .await
            .context("reading GNOME calendar setting")?;
        if calendar_disabled {
            continue;
        }
        let oauth = Proxy::new(
            &connection,
            "org.gnome.OnlineAccounts",
            account.path(),
            "org.gnome.OnlineAccounts.OAuth2Based",
        )
        .await
        .context("connecting to Google account token service")?;
        let (access_token, _expires_in): (String, i32) = oauth
            .call("GetAccessToken", &())
            .await
            .context("requesting the GNOME Google access token")?;
        token = Some(access_token);
        break;
    }
    let token = token.context("No Google Calendar account is enabled in GNOME Online Accounts")?;
    let now = Local::now();
    let today = now.date_naive();
    let start = local_boundary(today)?;
    let end = local_boundary(today.succ_opt().context("date overflow")?)?;
    GoogleApi::new(&token)?.calendar_today(start, end).await
}

fn local_boundary(date: NaiveDate) -> Result<DateTime<Utc>> {
    let local = date.and_time(NaiveTime::MIN);
    Local
        .from_local_datetime(&local)
        .earliest()
        .map(|date| date.with_timezone(&Utc))
        .context("local midnight does not exist")
}

fn calendar_rows(events: Vec<CalendarEvent>) -> Vec<CalendarEventRow> {
    events
        .into_iter()
        .map(|event| match event.start {
            EventStart::Timed(start) => {
                let local = start.with_timezone(&Local);
                CalendarEventRow {
                    title: event.title.into(),
                    when: local.format("%-I:%M %p").to_string().into(),
                    position: local.time().num_seconds_from_midnight() as f32 / 86_400.0,
                    timed: true,
                    birthday: event.birthday,
                }
            }
            EventStart::AllDay(_) => CalendarEventRow {
                title: event.title.into(),
                when: "ALL DAY".into(),
                position: 0.0,
                timed: false,
                birthday: event.birthday,
            },
        })
        .collect()
}
