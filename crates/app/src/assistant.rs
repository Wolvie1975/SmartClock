//! The assistant: the command-bar answer card and the Assistant screen's
//! conversations, both answered by the local model (Ollama) with the clock tools.
//!
//! The model runs on a background tokio runtime. Tool calls come back to the UI
//! thread over a channel (the store isn't `Send`), run through `tools::run`, and
//! the result goes back to the model. Clock commands typed into a conversation are
//! handled by the offline parser first, with no model involved.

use crate::dashboard::DashController;
use crate::tools;
use crate::weather::WeatherController;
use crate::{AppWindow, Chat, ChatMsg, ConvRow, Dash};
use ai::openai::OpenAiCompatible;
use ai::{Chunk, Message, Provider, ToolCall};
use chrono::{DateTime, Local, NaiveDateTime, Utc};
use clock_core::actions::{self, day_word, Undo};
use clock_core::model::{time12, ChatMessage};
use clock_core::{command, Store};
use futures::StreamExt;
use slint::{ComponentHandle, Model, ModelRc, VecModel};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::mpsc;
use std::time::{Duration, Instant};
use tokio::sync::oneshot;

/// Model ↔ tool round trips per question.
const MAX_STEPS: usize = 4;
/// Earlier messages sent with each question in a conversation.
const HISTORY: usize = 12;
const STATUS_EVERY: Duration = Duration::from_secs(120);
const DEFAULT_URL: &str = "http://127.0.0.1:11434";
const DEFAULT_MODEL: &str = "llama3.2:3b";
const BUILT_IN: &str = "BUILT-IN · NO AI USED";
const CLOUD_NOT_SET_UP: &str = "Cloud isn't set up yet, so nothing was sent. Add a cloud provider in Settings → AI & Models, or switch this conversation back to LOCAL.";

/// What a tool call sends back to the model loop.
struct ToolReply {
    result: String,
    say: Option<String>,
}

enum Event {
    Text(String),
    /// Throw away the text streamed so far (it was a malformed tool call).
    Reset,
    Tool {
        call: ToolCall,
        reply: oneshot::Sender<ToolReply>,
    },
    Done,
    Error(String),
    /// Background reachability check of the local model.
    Status(bool),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Target {
    /// The command bar's answer card.
    Card,
    /// A conversation on the Assistant screen; `row` is the reply's row in the transcript.
    Chat { conv: i64, row: usize },
}

struct Job {
    id: u64,
    target: Target,
    route: String,
    text: String,
    actions: Vec<String>,
    handle: tokio::task::JoinHandle<()>,
}

pub struct AssistantController {
    store: Rc<Store>,
    weather: Rc<WeatherController>,
    dash: Rc<DashController>,
    rt: tokio::runtime::Runtime,
    tx: mpsc::Sender<(u64, Event)>,
    rx: mpsc::Receiver<(u64, Event)>,
    job: RefCell<Option<Job>>,
    next_id: Cell<u64>,
    /// UNDO for the most recent reply, and where it was shown.
    undos: RefCell<Vec<Undo>>,
    undo_target: Cell<Option<Target>>,
    // Assistant screen
    conversations: Rc<VecModel<ConvRow>>,
    messages: Rc<VecModel<ChatMsg>>,
    current: Cell<Option<i64>>,
    new_is_cloud: Cell<bool>,
    next_status: Cell<Instant>,
    /// The answer card's exchange so far, so follow-ups work while it stays open.
    card_history: RefCell<Vec<Message>>,
}

fn system_prompt() -> String {
    let now = Local::now();
    format!(
        "You are the assistant built into a desk clock. You help with the time, alarms, timers, \
         reminders, today's schedule, the weather, and quick everyday questions.\n\
         It is {}.\n\
         Use the tools to set timers, alarms and reminders, and to look up today's details or the \
         weather; never claim you did something without calling the tool. Only set a timer, alarm \
         or reminder when the user clearly asks for one; if a request is unclear, ask a short \
         question instead. Give times with AM or PM.\n\
         Only state facts the tools gave you. Don't work out times or dates yourself, and don't \
         describe buttons or features you weren't told about.\n\
         Reply in one to three short sentences of plain text, with no markdown or lists.",
        now.format("%A %-d %B %Y, %-I:%M %p")
    )
}

fn now() -> (DateTime<Utc>, NaiveDateTime) {
    let utc = Utc::now();
    (utc, utc.with_timezone(&Local).naive_local())
}

fn clock_label(t: DateTime<Utc>) -> String {
    time12(t.with_timezone(&Local).time())
}

fn conv_title(text: &str) -> String {
    const MAX: usize = 40;
    let t = text.trim();
    if t.chars().count() <= MAX {
        return t.to_string();
    }
    let cut: String = t.chars().take(MAX).collect();
    format!("{}…", cut.trim_end())
}

fn msg_row(m: &ChatMessage) -> ChatMsg {
    ChatMsg {
        id: m.id as i32,
        role: m.role.as_str().into(),
        text: m.content.as_str().into(),
        route: m.route.as_str().into(),
        cloud: m.cloud,
        actions: m.actions.as_str().into(),
        can_undo: false,
        pending: false,
        when: clock_label(m.created_utc).into(),
    }
}

impl AssistantController {
    pub fn install(
        ui: &AppWindow,
        store: Rc<Store>,
        weather: Rc<WeatherController>,
        dash: Rc<DashController>,
    ) -> anyhow::Result<Rc<Self>> {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .thread_name("assistant")
            .enable_all()
            .build()?;
        let (tx, rx) = mpsc::channel();
        let this = Rc::new(Self {
            store,
            weather,
            dash,
            rt,
            tx,
            rx,
            job: RefCell::new(None),
            next_id: Cell::new(1),
            undos: RefCell::new(Vec::new()),
            undo_target: Cell::new(None),
            conversations: Rc::new(VecModel::default()),
            messages: Rc::new(VecModel::default()),
            current: Cell::new(None),
            new_is_cloud: Cell::new(false),
            next_status: Cell::new(Instant::now()),
            card_history: RefCell::new(Vec::new()),
        });

        let chat = ui.global::<Chat>();
        chat.set_conversations(ModelRc::from(this.conversations.clone()));
        chat.set_messages(ModelRc::from(this.messages.clone()));

        macro_rules! on {
            ($global:ident, $cb:ident, |$c:ident, $ui:ident $(, $arg:ident : $ty:ty)*| $body:expr) => {{
                let (c, weak) = (this.clone(), ui.as_weak());
                ui.global::<$global>().$cb(move |$($arg: $ty),*| {
                    if let Some($ui) = weak.upgrade() {
                        let $c = &c;
                        $body;
                    }
                });
            }};
        }
        on!(Dash, on_answer_close, |c, ui| c.close_card(&ui));
        on!(Dash, on_answer_undo, |c, ui| c.undo(&ui));
        on!(Chat, on_new_conversation, |c, ui| c.new_conversation(&ui));
        on!(Chat, on_select, |c, ui, id: i32| c.select(&ui, id.into()));
        on!(Chat, on_delete, |c, ui, id: i32| c.delete(&ui, id.into()));
        on!(Chat, on_send, |c, ui, text: slint::SharedString| c
            .send(&ui, &text));
        on!(Chat, on_set_cloud, |c, ui, cloud: bool| c
            .set_cloud(&ui, cloud));
        on!(Chat, on_undo, |c, ui| c.undo(&ui));
        on!(Chat, on_stop, |c, ui| c.stop(&ui));

        let (url, model) = this.settings();
        chat.set_local_model(model.to_uppercase().into());
        chat.set_local_address(
            url.trim_start_matches("http://")
                .trim_start_matches("https://")
                .into(),
        );
        this.reload_conversations(ui);
        this.new_conversation(ui);
        Ok(this)
    }

    fn settings(&self) -> (String, String) {
        let url = self
            .store
            .get_setting("ollama_url")
            .ok()
            .flatten()
            .unwrap_or_else(|| DEFAULT_URL.into());
        let model = self
            .store
            .get_setting("ollama_model")
            .ok()
            .flatten()
            .unwrap_or_else(|| DEFAULT_MODEL.into());
        (url, model)
    }

    fn local_route(&self) -> String {
        format!("LOCAL · {}", self.settings().1.to_uppercase())
    }

    // -----------------------------------------------------------------------
    // Jobs

    /// Start a model run for `target`. Any run in progress is stopped first.
    fn start(&self, ui: &AppWindow, target: Target, messages: Vec<Message>) {
        self.stop(ui);
        let id = self.next_id.get();
        self.next_id.set(id + 1);
        let (url, model) = self.settings();
        let provider = match OpenAiCompatible::ollama(&url) {
            Ok(p) => p,
            Err(e) => return self.show_error(ui, target, &format!("{e:#}")),
        };
        let tx = self.tx.clone();
        let handle = self.rt.spawn(async move {
            let res = run_turn(&provider, &model, messages, id, &tx).await;
            let _ = tx.send((
                id,
                match res {
                    Ok(()) => Event::Done,
                    Err(e) => Event::Error(format!("{e:#}")),
                },
            ));
        });
        *self.job.borrow_mut() = Some(Job {
            id,
            target,
            route: self.local_route(),
            text: String::new(),
            actions: Vec::new(),
            handle,
        });
        ui.global::<Chat>()
            .set_busy(matches!(target, Target::Chat { .. }));
    }

    /// Stop the run in progress. A conversation keeps whatever was written so far.
    fn stop(&self, ui: &AppWindow) {
        let Some(job) = self.job.borrow_mut().take() else {
            return;
        };
        job.handle.abort();
        ui.global::<Chat>().set_busy(false);
        match job.target {
            Target::Card => ui.global::<Dash>().set_answer_busy(false),
            Target::Chat { conv, row } => {
                let text = job.text.trim();
                if text.is_empty() && job.actions.is_empty() {
                    if row < self.messages.row_count() && self.current.get() == Some(conv) {
                        self.messages.remove(row);
                    }
                } else {
                    let text = format!("{text} (stopped)");
                    self.finish_chat(conv, row, &job.route, &text, &job.actions);
                }
            }
        }
    }

    fn show_error(&self, ui: &AppWindow, target: Target, err: &str) {
        tracing::warn!("assistant: {err}");
        let unreachable = err.contains("can't reach") || err.contains("Connection refused");
        if unreachable {
            ui.global::<Chat>().set_local_status("UNREACHABLE".into());
        }
        let msg = if unreachable {
            "The local model isn't reachable. Is the Ollama container running? (docker ps --filter name=ollama)".to_string()
        } else {
            format!("Something went wrong: {err}")
        };
        match target {
            Target::Card => {
                let d = ui.global::<Dash>();
                d.set_answer_busy(false);
                d.set_answer_text(msg.into());
            }
            Target::Chat { conv, row } => {
                ui.global::<Chat>().set_busy(false);
                let (utc, _) = now();
                if let Err(e) = self
                    .store
                    .add_message(conv, "notice", &msg, "", false, "", utc)
                {
                    tracing::error!("saving notice failed: {e:#}");
                }
                if row < self.messages.row_count() && self.current.get() == Some(conv) {
                    if let Some(mut r) = self.messages.row_data(row) {
                        r.role = "notice".into();
                        r.route = "".into();
                        r.text = msg.into();
                        r.pending = false;
                        self.messages.set_row_data(row, r);
                    }
                }
            }
        }
    }

    /// Save a finished reply and settle its transcript row.
    fn finish_chat(&self, conv: i64, row: usize, route: &str, text: &str, actions: &[String]) {
        let (utc, _) = now();
        let actions = actions.join("  ·  ");
        if let Err(e) = self
            .store
            .add_message(conv, "assistant", text, route, false, &actions, utc)
        {
            tracing::error!("saving reply failed: {e:#}");
        }
        if row < self.messages.row_count() && self.current.get() == Some(conv) {
            if let Some(mut r) = self.messages.row_data(row) {
                r.text = text.into();
                r.actions = actions.into();
                r.pending = false;
                self.messages.set_row_data(row, r);
            }
        }
    }

    // -----------------------------------------------------------------------
    // Command-bar answer card

    pub fn ask(&self, ui: &AppWindow, question: &str) {
        let d = ui.global::<Dash>();
        d.set_answer_visible(true);
        d.set_answer_busy(true);
        d.set_answer_question(question.into());
        d.set_answer_route(self.local_route().into());
        d.set_answer_text("".into());
        d.set_answer_actions("".into());
        d.set_answer_can_undo(false);
        // Follow-ups: include the card's earlier exchange while it stays open.
        let mut messages = vec![Message::system(system_prompt())];
        {
            let mut history = self.card_history.borrow_mut();
            let skip = history.len().saturating_sub(HISTORY - 1);
            history.drain(..skip);
            messages.extend(history.iter().cloned());
            history.push(Message::user(question));
        }
        messages.push(Message::user(question));
        self.start(ui, Target::Card, messages);
        d.set_answer_busy(true); // `start` stopped any earlier run, which clears this
    }

    fn close_card(&self, ui: &AppWindow) {
        if matches!(
            self.job.borrow().as_ref().map(|j| j.target),
            Some(Target::Card)
        ) {
            self.stop(ui);
        }
        self.card_history.borrow_mut().clear();
        ui.global::<Dash>().set_answer_visible(false);
    }

    // -----------------------------------------------------------------------
    // Assistant screen

    fn reload_conversations(&self, ui: &AppWindow) {
        let (_, local) = now();
        let rows: Vec<ConvRow> = self
            .store
            .conversations()
            .unwrap_or_default()
            .iter()
            .map(|c| {
                let t = c.updated_utc.with_timezone(&Local).naive_local();
                let day = day_word(t.date(), local);
                let when = if day == "TODAY" {
                    time12(t.time())
                } else {
                    day
                };
                ConvRow {
                    id: c.id as i32,
                    title: c.title.as_str().into(),
                    when: when.into(),
                }
            })
            .collect();
        self.conversations.set_vec(rows);
        ui.global::<Chat>()
            .set_current_id(self.current.get().map(|c| c as i32).unwrap_or(-1));
    }

    fn new_conversation(&self, ui: &AppWindow) {
        self.stop(ui);
        self.current.set(None);
        self.new_is_cloud.set(false);
        self.messages.set_vec(Vec::new());
        let chat = ui.global::<Chat>();
        chat.set_current_id(-1);
        chat.set_current_title("New conversation".into());
        chat.set_cloud(false);
    }

    fn select(&self, ui: &AppWindow, id: i64) {
        if self.current.get() == Some(id) {
            return;
        }
        self.stop(ui);
        let Ok(Some(conv)) = self.store.conversation(id) else {
            return;
        };
        self.current.set(Some(id));
        let rows: Vec<ChatMsg> = self
            .store
            .messages(id)
            .unwrap_or_default()
            .iter()
            .map(msg_row)
            .collect();
        self.messages.set_vec(rows);
        let chat = ui.global::<Chat>();
        chat.set_current_id(id as i32);
        chat.set_current_title(conv.title.as_str().into());
        chat.set_cloud(conv.mode == "cloud");
    }

    fn delete(&self, ui: &AppWindow, id: i64) {
        if self.current.get() == Some(id) {
            self.new_conversation(ui);
        }
        if let Err(e) = self.store.delete_conversation(id) {
            tracing::error!("deleting conversation failed: {e:#}");
        }
        self.reload_conversations(ui);
    }

    fn set_cloud(&self, ui: &AppWindow, cloud: bool) {
        match self.current.get() {
            Some(id) => {
                if let Err(e) = self
                    .store
                    .set_conversation_mode(id, if cloud { "cloud" } else { "local" })
                {
                    tracing::error!("saving mode failed: {e:#}");
                }
            }
            None => self.new_is_cloud.set(cloud),
        }
        ui.global::<Chat>().set_cloud(cloud);
    }

    fn push_row(
        &self,
        role: &str,
        text: &str,
        route: &str,
        actions: &str,
        can_undo: bool,
        pending: bool,
    ) -> usize {
        let (utc, _) = now();
        self.messages.push(ChatMsg {
            id: 0,
            role: role.into(),
            text: text.into(),
            route: route.into(),
            cloud: false,
            actions: actions.into(),
            can_undo,
            pending,
            when: clock_label(utc).into(),
        });
        self.messages.row_count() - 1
    }

    fn send(&self, ui: &AppWindow, text: &str) {
        let text = text.trim().to_string();
        if text.is_empty() {
            return;
        }
        self.stop(ui);
        let chat = ui.global::<Chat>();
        chat.set_input("".into());
        let (utc, local) = now();

        // The first message creates the conversation.
        let conv = match self.current.get() {
            Some(c) => c,
            None => {
                let mode = if self.new_is_cloud.get() {
                    "cloud"
                } else {
                    "local"
                };
                match self
                    .store
                    .create_conversation(&conv_title(&text), mode, utc)
                {
                    Ok(c) => {
                        self.current.set(Some(c.id));
                        chat.set_current_title(c.title.as_str().into());
                        c.id
                    }
                    Err(e) => return tracing::error!("creating conversation failed: {e:#}"),
                }
            }
        };
        let save = |role: &str, content: &str, route: &str, actions: &str| {
            if let Err(e) = self
                .store
                .add_message(conv, role, content, route, false, actions, utc)
            {
                tracing::error!("saving message failed: {e:#}");
            }
        };
        save("user", &text, "", "");
        self.push_row("user", &text, "", "", false, false);

        // 1. Clock commands: the offline parser, no AI.
        if let Some(cmd) = command::parse(&text, local) {
            let sync_google = matches!(&cmd, command::Command::AddReminder { time: None, .. });
            let out = tools::apply_command(&self.store, cmd, utc, local);
            let (summary, undo) = match out.applied {
                Some(a) => (a.summary, a.undo),
                None => (String::new(), None),
            };
            let reply = out.say.unwrap_or_else(|| out.result.clone());
            save("assistant", &reply, BUILT_IN, &summary);
            let row = self.push_row(
                "assistant",
                &reply,
                BUILT_IN,
                &summary,
                undo.is_some(),
                false,
            );
            *self.undos.borrow_mut() = undo.into_iter().collect();
            self.undo_target.set(Some(Target::Chat { conv, row }));
            self.dash.refresh(ui);
            if sync_google {
                self.dash.request_google_sync();
            }
            self.reload_conversations(ui);
            return;
        }

        // 2. CLOUD conversations: no cloud provider yet, so nothing is sent.
        if chat.get_cloud() {
            save("notice", CLOUD_NOT_SET_UP, "", "");
            self.push_row("notice", CLOUD_NOT_SET_UP, "", "", false, false);
            self.reload_conversations(ui);
            return;
        }

        // 3. The local model, with recent history.
        let history: Vec<Message> = self
            .store
            .messages(conv)
            .unwrap_or_default()
            .iter()
            .filter(|m| m.role == "user" || m.role == "assistant")
            .map(|m| match m.role.as_str() {
                "user" => Message::user(m.content.clone()),
                _ => Message::assistant(m.content.clone(), vec![]),
            })
            .collect();
        let skip = history.len().saturating_sub(HISTORY);
        let mut messages = vec![Message::system(system_prompt())];
        messages.extend(history.into_iter().skip(skip));

        let row = self.push_row("assistant", "", &self.local_route(), "", false, true);
        self.reload_conversations(ui);
        self.start(ui, Target::Chat { conv, row }, messages);
    }

    // -----------------------------------------------------------------------
    // UNDO

    fn undo(&self, ui: &AppWindow) {
        let undos: Vec<Undo> = self.undos.borrow_mut().drain(..).collect();
        for u in undos.iter().rev() {
            if let Err(e) = actions::undo(&self.store, u) {
                tracing::error!("undo failed: {e:#}");
            }
        }
        match self.undo_target.take() {
            Some(Target::Card) => {
                let d = ui.global::<Dash>();
                d.set_answer_can_undo(false);
                d.set_answer_actions("UNDONE".into());
            }
            Some(Target::Chat { conv, row }) if self.current.get() == Some(conv) => {
                if let Some(mut r) = self.messages.row_data(row) {
                    r.can_undo = false;
                    r.actions = "UNDONE".into();
                    self.messages.set_row_data(row, r);
                }
            }
            _ => {}
        }
        self.dash.request_google_sync();
        self.dash.refresh(ui);
    }

    // -----------------------------------------------------------------------
    // Tick

    fn check_status(&self) {
        let (url, _) = self.settings();
        let Ok(p) = OpenAiCompatible::ollama(&url) else {
            return;
        };
        let tx = self.tx.clone();
        self.rt.spawn(async move {
            let _ = tx.send((0, Event::Status(p.test().await.is_ok())));
        });
    }

    /// Called from the UI tick: apply streamed text, run requested tools.
    pub fn tick(&self, ui: &AppWindow) {
        if Instant::now() >= self.next_status.get() {
            self.next_status.set(Instant::now() + STATUS_EVERY);
            self.check_status();
        }
        while let Ok((id, ev)) = self.rx.try_recv() {
            if let Event::Status(ok) = ev {
                let status = if ok { "READY" } else { "UNREACHABLE" };
                ui.global::<Chat>().set_local_status(status.into());
                ui.set_ai_local_status(status.into());
                continue;
            }
            let target = match self.job.borrow().as_ref() {
                Some(j) if j.id == id => j.target,
                _ => continue, // from a stopped run
            };
            match ev {
                Event::Status(_) => {}
                Event::Reset => {
                    if let Some(j) = self.job.borrow_mut().as_mut() {
                        j.text.clear();
                    }
                    self.show_text(ui, target, "");
                }
                Event::Text(t) => {
                    let text = {
                        let mut job = self.job.borrow_mut();
                        let j = job.as_mut().expect("job");
                        j.text.push_str(&t);
                        j.text.trim_start().to_string()
                    };
                    self.show_text(ui, target, &text);
                }
                Event::Tool { call, reply } => {
                    let (utc, local) = now();
                    let sync_google = call.name == "add_reminder";
                    let weather = match call.name.as_str() {
                        "get_weather" | "list_today" => self.weather.summary(),
                        _ => None,
                    };
                    tracing::info!(tool = %call.name, args = %call.args, "assistant tool call");
                    let out = tools::run(&self.store, weather, &call, utc, local);
                    if let Some(applied) = out.applied {
                        let actions = {
                            let mut job = self.job.borrow_mut();
                            let j = job.as_mut().expect("job");
                            j.actions.push(applied.summary);
                            j.actions.join("  ·  ")
                        };
                        if self.undo_target.get() != Some(target) {
                            self.undos.borrow_mut().clear();
                            self.undo_target.set(Some(target));
                        }
                        self.undos.borrow_mut().extend(applied.undo);
                        let can_undo = !self.undos.borrow().is_empty();
                        self.show_actions(ui, target, &actions, can_undo);
                        self.dash.refresh(ui);
                        if sync_google {
                            self.dash.request_google_sync();
                        }
                    }
                    let _ = reply.send(ToolReply {
                        result: out.result,
                        say: out.say,
                    });
                }
                Event::Done => {
                    let job = self.job.borrow_mut().take().expect("job");
                    ui.global::<Chat>().set_busy(false);
                    ui.global::<Chat>().set_local_status("READY".into());
                    let text = job.text.trim().to_string();
                    match job.target {
                        Target::Card => {
                            let d = ui.global::<Dash>();
                            d.set_answer_busy(false);
                            if text.is_empty() && job.actions.is_empty() {
                                d.set_answer_text("(no reply)".into());
                            }
                            let said = if text.is_empty() {
                                job.actions.join(". ")
                            } else {
                                text
                            };
                            self.card_history
                                .borrow_mut()
                                .push(Message::assistant(said, vec![]));
                        }
                        Target::Chat { conv, row } => {
                            let text = if text.is_empty() && job.actions.is_empty() {
                                "(no reply)".into()
                            } else {
                                text
                            };
                            self.finish_chat(conv, row, &job.route, &text, &job.actions);
                            self.reload_conversations(ui);
                        }
                    }
                }
                Event::Error(e) => {
                    let job = self.job.borrow_mut().take().expect("job");
                    self.show_error(ui, job.target, &e);
                }
            }
        }
    }

    fn show_text(&self, ui: &AppWindow, target: Target, text: &str) {
        match target {
            Target::Card => ui.global::<Dash>().set_answer_text(text.into()),
            Target::Chat { conv, row } => {
                if self.current.get() != Some(conv) {
                    return;
                }
                if let Some(mut r) = self.messages.row_data(row) {
                    r.text = text.into();
                    self.messages.set_row_data(row, r);
                }
            }
        }
    }

    fn show_actions(&self, ui: &AppWindow, target: Target, actions: &str, can_undo: bool) {
        match target {
            Target::Card => {
                let d = ui.global::<Dash>();
                d.set_answer_actions(actions.into());
                d.set_answer_can_undo(can_undo);
            }
            Target::Chat { conv, row } => {
                if self.current.get() != Some(conv) {
                    return;
                }
                if let Some(mut r) = self.messages.row_data(row) {
                    r.actions = actions.into();
                    r.can_undo = can_undo;
                    self.messages.set_row_data(row, r);
                }
            }
        }
    }
}

/// Small models sometimes write a tool call as JSON text instead of using the tool
/// interface. Returns the call if the text is exactly such an object.
fn json_tool_call(text: &str) -> Option<(String, serde_json::Value)> {
    let t = text.trim();
    if !(t.starts_with('{') && t.ends_with('}')) {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(t).ok()?;
    let name = v["name"].as_str()?.to_string();
    let args = [&v["parameters"], &v["arguments"]]
        .into_iter()
        .find(|a| a.is_object())
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    Some((name, args))
}

/// Stream one model reply. Returns (text, tool calls).
async fn stream_reply(
    provider: &OpenAiCompatible,
    model: &str,
    messages: &[Message],
    tools: &[ai::ToolSpec],
    id: u64,
    tx: &mpsc::Sender<(u64, Event)>,
) -> anyhow::Result<(String, Vec<ToolCall>)> {
    let mut stream = provider.chat(model, messages, tools).await?;
    let mut text = String::new();
    let mut calls = Vec::new();
    while let Some(chunk) = stream.next().await {
        match chunk? {
            Chunk::Text(t) => {
                text.push_str(&t);
                let _ = tx.send((id, Event::Text(t)));
            }
            Chunk::ToolCall(c) => calls.push(c),
            Chunk::Done(_) => break,
        }
    }
    Ok((text, calls))
}

/// One question: stream the reply, run any tool calls, then either show the
/// app's own confirmations (actions) or let the model answer from the results
/// (lookups). Repeats for at most `MAX_STEPS` rounds.
async fn run_turn(
    provider: &OpenAiCompatible,
    model: &str,
    mut messages: Vec<Message>,
    id: u64,
    tx: &mpsc::Sender<(u64, Event)>,
) -> anyhow::Result<()> {
    let specs = tools::specs();
    for _ in 0..MAX_STEPS {
        let (mut text, mut calls) =
            stream_reply(provider, model, &messages, &specs, id, tx).await?;

        if calls.is_empty() {
            let Some((name, args)) = json_tool_call(&text) else {
                return Ok(());
            };
            let _ = tx.send((id, Event::Reset));
            if specs.iter().any(|s| s.name == name) {
                tracing::info!(tool = %name, "recovered a tool call written as text");
                calls.push(ToolCall {
                    id: "json_0".into(),
                    name,
                    args,
                });
                text.clear();
            } else {
                // It invented a tool; ask again with no tools offered.
                tracing::info!(tool = %name, "model invented a tool; retrying without tools");
                stream_reply(provider, model, &messages, &[], id, tx).await?;
                return Ok(());
            }
        }

        messages.push(Message::assistant(text, calls.clone()));
        let mut says = Vec::new();
        let mut all_said = true;
        for call in calls {
            let (reply_tx, reply_rx) = oneshot::channel();
            let call_id = call.id.clone();
            tx.send((
                id,
                Event::Tool {
                    call,
                    reply: reply_tx,
                },
            ))
            .map_err(|_| anyhow::anyhow!("UI closed"))?;
            let reply = reply_rx.await.unwrap_or_else(|_| ToolReply {
                result: "error: cancelled".into(),
                say: None,
            });
            match reply.say {
                Some(s) => says.push(s),
                None => all_said = false,
            }
            messages.push(Message::tool_result(call_id, reply.result));
        }
        if all_said {
            // Every call was an action that succeeded: the app's confirmation is the reply.
            let _ = tx.send((id, Event::Reset));
            let _ = tx.send((id, Event::Text(says.join(" "))));
            return Ok(());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Full loop against the local Ollama container with an in-memory store:
    /// `cargo test -p smart-clock -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn live_assistant_turns() {
        let store = Store::open_in_memory().unwrap();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let provider = OpenAiCompatible::ollama(DEFAULT_URL).unwrap();

        let ask = |q: &str| -> (String, Vec<String>) {
            let (tx, rx) = mpsc::channel();
            let messages = vec![Message::system(system_prompt()), Message::user(q)];
            let p = &provider;
            let t0 = std::time::Instant::now();
            let (mut text, mut done) = (String::new(), Vec::new());
            std::thread::scope(|s| {
                s.spawn(|| {
                    rt.block_on(async {
                        let r = run_turn(p, DEFAULT_MODEL, messages, 1, &tx).await;
                        let _ = tx.send((
                            1,
                            if r.is_ok() {
                                Event::Done
                            } else {
                                Event::Error(format!("{r:?}"))
                            },
                        ));
                    })
                });
                while let Ok((_, ev)) = rx.recv() {
                    match ev {
                        Event::Text(t) => text.push_str(&t),
                        Event::Reset => text.clear(),
                        Event::Status(_) => {}
                        Event::Tool { call, reply } => {
                            let now = Utc::now();
                            let out = tools::run(
                                &store,
                                None,
                                &call,
                                now,
                                now.with_timezone(&Local).naive_local(),
                            );
                            println!("  tool {} {} -> {}", call.name, call.args, out.result);
                            if let Some(a) = out.applied {
                                done.push(a.summary);
                            }
                            let _ = reply.send(ToolReply {
                                result: out.result,
                                say: out.say,
                            });
                        }
                        Event::Done => break,
                        Event::Error(e) => panic!("{e}"),
                    }
                }
            });
            println!("Q: {q}\nA ({:.1?}): {}\n", t0.elapsed(), text.trim());
            (text, done)
        };

        let (_, done) = ask("set a 10 minute timer for the tea");
        assert!(done.iter().any(|d| d.starts_with("TIMER")), "{done:?}");
        let (_, done) = ask("remind me tomorrow at 9 am to call the bank");
        assert!(done.iter().any(|d| d.starts_with("REMINDER")), "{done:?}");
        let (_, done) = ask("wake me up at 6:30 am on weekdays");
        assert!(done.iter().any(|d| d.starts_with("ALARM")), "{done:?}");
        let (text, _) = ask("what timers and reminders do I have?");
        assert!(!text.trim().is_empty());
        let (text, done) = ask("how many teaspoons are in a tablespoon?");
        assert!(done.is_empty() && !text.trim().is_empty());

        println!(
            "timers: {:?}",
            store
                .timers()
                .unwrap()
                .iter()
                .map(|t| &t.name)
                .collect::<Vec<_>>()
        );
        println!(
            "reminders: {:?}",
            store
                .reminders()
                .unwrap()
                .iter()
                .map(|r| (&r.text, r.due_date, r.due_time))
                .collect::<Vec<_>>()
        );
        println!(
            "alarms: {:?}",
            store
                .alarms()
                .unwrap()
                .iter()
                .map(|a| (a.time_local, a.days))
                .collect::<Vec<_>>()
        );
    }

    /// "and tomorrow?" after a weather question must not create anything.
    /// `cargo test -p smart-clock live_follow_up -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn live_follow_up() {
        let store = Store::open_in_memory().unwrap();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let provider = OpenAiCompatible::ollama(DEFAULT_URL).unwrap();
        let weather = "Weather for Testville (temperatures in °F):\nNow: 64° partly cloudy.\n\
                       Today (Friday): partly cloudy, low 48° high 66°.\nTomorrow (Saturday): rain, low 45° high 58°.";
        for _ in 0..3 {
            let messages = vec![
                Message::system(system_prompt()),
                Message::user("what's the weather this afternoon?"),
                Message::assistant(
                    "It's 64° and partly cloudy, with a high of 66° today.",
                    vec![],
                ),
                Message::user("and tomorrow?"),
            ];
            let (tx, rx) = mpsc::channel();
            let (mut text, mut made) = (String::new(), Vec::new());
            std::thread::scope(|s| {
                s.spawn(|| {
                    rt.block_on(async {
                        let _ = run_turn(&provider, DEFAULT_MODEL, messages, 1, &tx).await;
                        let _ = tx.send((1, Event::Done));
                    })
                });
                while let Ok((_, ev)) = rx.recv() {
                    match ev {
                        Event::Text(t) => text.push_str(&t),
                        Event::Reset => text.clear(),
                        Event::Tool { call, reply } => {
                            let now = Utc::now();
                            let w = Some(weather.to_string());
                            let out = tools::run(
                                &store,
                                w,
                                &call,
                                now,
                                now.with_timezone(&Local).naive_local(),
                            );
                            println!("  tool {} {} -> {}", call.name, call.args, out.result);
                            if let Some(a) = out.applied {
                                made.push(a.summary);
                            }
                            let _ = reply.send(ToolReply {
                                result: out.result,
                                say: out.say,
                            });
                        }
                        Event::Done | Event::Error(_) => break,
                        Event::Status(_) => {}
                    }
                }
            });
            println!("A: {}", text.trim());
            assert!(made.is_empty(), "created {made:?}");
        }
        assert!(store.reminders().unwrap().is_empty());
    }
}
