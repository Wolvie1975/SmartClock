//! Command-bar assistant: anything the offline parser doesn't understand goes to the
//! local model (Ollama), with the clock tools available.
//!
//! The model runs on a background tokio runtime. Tool calls come back to the UI
//! thread over a channel (the store isn't `Send`), run through `tools::run`, and
//! the result is sent back to the model. Each reply shows in the answer card above
//! the command bar, with UNDO for anything it changed.

use crate::dashboard::DashController;
use crate::tools;
use crate::weather::WeatherController;
use crate::{AppWindow, Dash};
use ai::openai::OpenAiCompatible;
use ai::{Chunk, Message, Provider, ToolCall};
use chrono::{Local, Utc};
use clock_core::actions::{self, Undo};
use clock_core::Store;
use futures::StreamExt;
use slint::ComponentHandle;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::mpsc;
use tokio::sync::oneshot;

/// Model ↔ tool round trips per question.
const MAX_STEPS: usize = 4;
const DEFAULT_URL: &str = "http://127.0.0.1:11434";
const DEFAULT_MODEL: &str = "llama3.2:3b";

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
}

pub struct AssistantController {
    store: Rc<Store>,
    weather: Rc<WeatherController>,
    dash: Rc<DashController>,
    rt: tokio::runtime::Runtime,
    tx: mpsc::Sender<(u64, Event)>,
    rx: mpsc::Receiver<(u64, Event)>,
    job: RefCell<Option<tokio::task::JoinHandle<()>>>,
    current: Cell<u64>,
    text: RefCell<String>,
    actions: RefCell<Vec<String>>,
    undos: RefCell<Vec<Undo>>,
}

fn system_prompt() -> String {
    let now = Local::now();
    format!(
        "You are the assistant built into a desk clock. You help with the time, alarms, timers, \
         reminders, today's schedule, the weather, and quick everyday questions.\n\
         It is {}.\n\
         Use the tools to set timers, alarms and reminders, and to look up today's details or the \
         weather; never claim you did something without calling the tool. Give times with AM or PM.\n\
         Only state facts the tools gave you. Don't work out times or dates yourself, and don't \
         describe buttons or features you weren't told about.\n\
         Reply in one to three short sentences of plain text, with no markdown or lists.",
        now.format("%A %-d %B %Y, %-I:%M %p")
    )
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
            current: Cell::new(0),
            text: RefCell::new(String::new()),
            actions: RefCell::new(Vec::new()),
            undos: RefCell::new(Vec::new()),
        });

        let d = ui.global::<Dash>();
        {
            let (c, weak) = (this.clone(), ui.as_weak());
            d.on_answer_close(move || {
                if let Some(ui) = weak.upgrade() {
                    c.close(&ui);
                }
            });
        }
        {
            let (c, weak) = (this.clone(), ui.as_weak());
            d.on_answer_undo(move || {
                if let Some(ui) = weak.upgrade() {
                    c.undo(&ui);
                }
            });
        }
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

    /// Ask the local model. Cancels any answer still in progress.
    pub fn ask(&self, ui: &AppWindow, question: &str) {
        self.abort();
        let id = self.current.get() + 1;
        self.current.set(id);
        self.text.borrow_mut().clear();
        self.actions.borrow_mut().clear();
        self.undos.borrow_mut().clear();

        let (url, model) = self.settings();
        let d = ui.global::<Dash>();
        d.set_answer_visible(true);
        d.set_answer_busy(true);
        d.set_answer_question(question.into());
        d.set_answer_route(format!("LOCAL · {}", model.to_uppercase()).into());
        d.set_answer_text("".into());
        d.set_answer_actions("".into());
        d.set_answer_can_undo(false);

        let provider = match OpenAiCompatible::ollama(&url) {
            Ok(p) => p,
            Err(e) => return self.fail(ui, &format!("{e:#}")),
        };
        let messages = vec![Message::system(system_prompt()), Message::user(question)];
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
        *self.job.borrow_mut() = Some(handle);
    }

    fn abort(&self) {
        if let Some(j) = self.job.borrow_mut().take() {
            j.abort();
        }
    }

    fn close(&self, ui: &AppWindow) {
        self.abort();
        self.current.set(self.current.get() + 1); // ignore late events
        ui.global::<Dash>().set_answer_visible(false);
    }

    fn undo(&self, ui: &AppWindow) {
        let undos: Vec<Undo> = self.undos.borrow_mut().drain(..).collect();
        for u in undos.iter().rev() {
            if let Err(e) = actions::undo(&self.store, u) {
                tracing::error!("undo failed: {e:#}");
            }
        }
        let d = ui.global::<Dash>();
        d.set_answer_can_undo(false);
        d.set_answer_actions("UNDONE".into());
        self.dash.refresh(ui);
    }

    fn fail(&self, ui: &AppWindow, err: &str) {
        tracing::warn!("assistant: {err}");
        let d = ui.global::<Dash>();
        d.set_answer_busy(false);
        let unreachable = err.contains("can't reach") || err.contains("Connection refused");
        let msg = if unreachable {
            "The local model isn't reachable. Is the Ollama container running? (docker ps --filter name=ollama)".to_string()
        } else {
            format!("Something went wrong: {err}")
        };
        d.set_answer_text(msg.into());
    }

    /// Called from the UI tick: apply streamed text and run requested tools.
    pub fn tick(&self, ui: &AppWindow) {
        while let Ok((id, ev)) = self.rx.try_recv() {
            if id != self.current.get() {
                continue; // from a cancelled answer
            }
            let d = ui.global::<Dash>();
            match ev {
                Event::Reset => {
                    self.text.borrow_mut().clear();
                    d.set_answer_text("".into());
                }
                Event::Text(t) => {
                    let mut text = self.text.borrow_mut();
                    text.push_str(&t);
                    d.set_answer_text(text.trim_start().into());
                }
                Event::Tool { call, reply } => {
                    let now_utc = Utc::now();
                    let now_local = now_utc.with_timezone(&Local).naive_local();
                    let weather = match call.name.as_str() {
                        "get_weather" | "list_today" => self.weather.summary(),
                        _ => None,
                    };
                    tracing::info!(tool = %call.name, args = %call.args, "assistant tool call");
                    let out = tools::run(&self.store, weather, &call, now_utc, now_local);
                    if let Some(applied) = out.applied {
                        self.actions.borrow_mut().push(applied.summary);
                        if let Some(u) = applied.undo {
                            self.undos.borrow_mut().push(u);
                        }
                        d.set_answer_actions(self.actions.borrow().join("  ·  ").into());
                        d.set_answer_can_undo(!self.undos.borrow().is_empty());
                        self.dash.refresh(ui);
                    }
                    let _ = reply.send(ToolReply {
                        result: out.result,
                        say: out.say,
                    });
                }
                Event::Done => {
                    d.set_answer_busy(false);
                    if self.text.borrow().trim().is_empty() && self.actions.borrow().is_empty() {
                        d.set_answer_text("(no reply)".into());
                    }
                }
                Event::Error(e) => self.fail(ui, &e),
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
}
