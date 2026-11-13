//! OpenAI-compatible Chat Completions adapter: Ollama's `/v1` endpoint today,
//! OpenAI and OpenRouter later (same wire format, different base URL and key).

use crate::{
    Capabilities, Chunk, ChunkStream, Message, Provider, Role, StopReason, ToolCall, ToolSpec,
};
use anyhow::Context;
use futures::{stream, StreamExt};
use serde_json::{json, Value};
use std::collections::{BTreeMap, VecDeque};
use std::time::Duration;

pub struct OpenAiCompatible {
    id: String,
    /// Includes `/v1`, no trailing slash.
    base: String,
    key: Option<String>,
    local: bool,
    client: reqwest::Client,
}

impl OpenAiCompatible {
    pub fn new(id: &str, base_v1: &str, key: Option<String>, local: bool) -> anyhow::Result<Self> {
        let client = reqwest::Client::builder()
            .user_agent(concat!("smart-clock/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(5))
            .build()?;
        Ok(Self {
            id: id.into(),
            base: base_v1.trim_end_matches('/').into(),
            key,
            local,
            client,
        })
    }

    /// Ollama at e.g. `http://127.0.0.1:11434` (the `/v1` suffix is added).
    pub fn ollama(base: &str) -> anyhow::Result<Self> {
        let base = base.trim_end_matches('/');
        let v1 = if base.ends_with("/v1") {
            base.to_string()
        } else {
            format!("{base}/v1")
        };
        Self::new("ollama", &v1, None, true)
    }

    fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        let req = self.client.request(method, format!("{}{path}", self.base));
        match &self.key {
            Some(k) => req.bearer_auth(k),
            None => req,
        }
    }
}

// ---------------------------------------------------------------------------
// Request body

pub(crate) fn message_json(m: &Message) -> Value {
    match m.role {
        Role::System | Role::User => json!({ "role": m.role, "content": m.content }),
        Role::Assistant => {
            let mut v = json!({ "role": "assistant", "content": m.content });
            if !m.tool_calls.is_empty() {
                v["tool_calls"] = m
                    .tool_calls
                    .iter()
                    .map(|c| {
                        json!({
                            "id": c.id,
                            "type": "function",
                            "function": { "name": c.name, "arguments": c.args.to_string() },
                        })
                    })
                    .collect();
            }
            v
        }
        Role::Tool => json!({
            "role": "tool",
            "tool_call_id": m.tool_call_id.clone().unwrap_or_default(),
            "content": m.content,
        }),
    }
}

pub(crate) fn request_body(model: &str, messages: &[Message], tools: &[ToolSpec]) -> Value {
    let mut body = json!({
        "model": model,
        "stream": true,
        "messages": messages.iter().map(message_json).collect::<Vec<_>>(),
    });
    if !tools.is_empty() {
        body["tools"] = tools
            .iter()
            .map(|t| {
                json!({
                    "type": "function",
                    "function": { "name": t.name, "description": t.description, "parameters": t.parameters },
                })
            })
            .collect();
    }
    body
}

// ---------------------------------------------------------------------------
// Streaming response

#[derive(Default)]
struct PartialCall {
    id: String,
    name: String,
    args: String,
}

/// Folds streamed `chat.completion.chunk` events into [`Chunk`]s. Tool-call
/// arguments can arrive in pieces, so calls are emitted once the turn finishes.
#[derive(Default)]
pub(crate) struct Accum {
    calls: BTreeMap<u64, PartialCall>,
    finished: bool,
}

impl Accum {
    pub(crate) fn feed(&mut self, v: &Value) -> anyhow::Result<Vec<Chunk>> {
        if let Some(err) = v.get("error") {
            let msg = err
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("unknown error");
            anyhow::bail!("server error: {msg}");
        }
        let mut out = Vec::new();
        let Some(choice) = v["choices"].get(0) else {
            return Ok(out);
        };
        let delta = &choice["delta"];
        if let Some(t) = delta["content"].as_str().filter(|t| !t.is_empty()) {
            out.push(Chunk::Text(t.to_string()));
        }
        if let Some(calls) = delta["tool_calls"].as_array() {
            for (pos, tc) in calls.iter().enumerate() {
                let idx = tc["index"].as_u64().unwrap_or(pos as u64);
                let entry = self.calls.entry(idx).or_default();
                if let Some(id) = tc["id"].as_str() {
                    entry.id = id.to_string();
                }
                if let Some(name) = tc["function"]["name"].as_str() {
                    entry.name.push_str(name);
                }
                match &tc["function"]["arguments"] {
                    Value::String(s) => entry.args.push_str(s),
                    Value::Null => {}
                    other => entry.args.push_str(&other.to_string()), // some servers send an object
                }
            }
        }
        if let Some(reason) = choice["finish_reason"].as_str() {
            out.extend(self.finish(Some(reason)));
        }
        Ok(out)
    }

    /// Flush pending tool calls and end the turn (idempotent).
    pub(crate) fn finish(&mut self, reason: Option<&str>) -> Vec<Chunk> {
        if self.finished {
            return vec![];
        }
        self.finished = true;
        let had_calls = !self.calls.is_empty();
        let mut out: Vec<Chunk> = std::mem::take(&mut self.calls)
            .into_values()
            .enumerate()
            .map(|(i, c)| {
                let args = if c.args.trim().is_empty() {
                    json!({})
                } else {
                    serde_json::from_str(&c.args).unwrap_or_else(|e| {
                        tracing::warn!("tool call {} has invalid JSON arguments: {e}", c.name);
                        json!({})
                    })
                };
                let id = if c.id.is_empty() {
                    format!("call_{i}")
                } else {
                    c.id
                };
                Chunk::ToolCall(ToolCall {
                    id,
                    name: c.name,
                    args,
                })
            })
            .collect();
        let stop = match reason {
            Some("tool_calls") => StopReason::ToolUse,
            Some("length") => StopReason::MaxTokens,
            Some("stop") | None if had_calls => StopReason::ToolUse,
            Some("stop") | None => StopReason::EndTurn,
            Some(_) => StopReason::Other,
        };
        out.push(Chunk::Done(stop));
        out
    }
}

struct SseState {
    body: futures::stream::BoxStream<'static, reqwest::Result<bytes::Bytes>>,
    buf: Vec<u8>,
    acc: Accum,
    queue: VecDeque<anyhow::Result<Chunk>>,
    ended: bool,
}

impl SseState {
    fn line(&mut self, raw: &[u8]) {
        let line = String::from_utf8_lossy(raw);
        let Some(payload) = line.trim().strip_prefix("data:").map(str::trim) else {
            return;
        };
        if payload == "[DONE]" {
            self.queue.extend(self.acc.finish(None).into_iter().map(Ok));
            self.ended = true;
            return;
        }
        match serde_json::from_str::<Value>(payload)
            .context("bad stream event")
            .and_then(|v| self.acc.feed(&v))
        {
            Ok(chunks) => self.queue.extend(chunks.into_iter().map(Ok)),
            Err(e) => {
                self.queue.push_back(Err(e));
                self.ended = true;
            }
        }
    }

    fn drain(&mut self, final_flush: bool) {
        while let Some(nl) = self.buf.iter().position(|b| *b == b'\n') {
            let raw: Vec<u8> = self.buf.drain(..=nl).collect();
            self.line(&raw);
        }
        if final_flush && !self.buf.is_empty() {
            let raw = std::mem::take(&mut self.buf);
            self.line(&raw);
        }
    }
}

fn sse_stream(resp: reqwest::Response) -> ChunkStream {
    let state = SseState {
        body: resp.bytes_stream().boxed(),
        buf: Vec::new(),
        acc: Accum::default(),
        queue: VecDeque::new(),
        ended: false,
    };
    stream::unfold(state, |mut s| async move {
        loop {
            if let Some(c) = s.queue.pop_front() {
                return Some((c, s));
            }
            if s.ended {
                return None;
            }
            match s.body.next().await {
                Some(Ok(bytes)) => {
                    s.buf.extend_from_slice(&bytes);
                    s.drain(false);
                }
                Some(Err(e)) => {
                    s.queue
                        .push_back(Err(anyhow::Error::new(e).context("stream interrupted")));
                    s.ended = true;
                }
                None => {
                    s.drain(true);
                    let tail = s.acc.finish(None);
                    s.queue.extend(tail.into_iter().map(Ok));
                    s.ended = true;
                }
            }
        }
    })
    .boxed()
}

async fn check(resp: reqwest::Response) -> anyhow::Result<reqwest::Response> {
    let status = resp.status();
    if status.is_success() {
        return Ok(resp);
    }
    let body = resp.text().await.unwrap_or_default();
    let msg = serde_json::from_str::<Value>(&body)
        .ok()
        .and_then(|v| v["error"]["message"].as_str().map(String::from))
        .unwrap_or(body);
    anyhow::bail!("HTTP {status}: {msg}")
}

#[async_trait::async_trait]
impl Provider for OpenAiCompatible {
    fn id(&self) -> &str {
        &self.id
    }

    fn capabilities(&self, _model: &str) -> Capabilities {
        Capabilities {
            tools: true,
            streaming: true,
            vision: false,
            local: self.local,
        }
    }

    async fn chat(
        &self,
        model: &str,
        messages: &[Message],
        tools: &[ToolSpec],
    ) -> anyhow::Result<ChunkStream> {
        let resp = self
            .request(reqwest::Method::POST, "/chat/completions")
            .json(&request_body(model, messages, tools))
            .send()
            .await
            .with_context(|| format!("can't reach {}", self.base))?;
        Ok(sse_stream(check(resp).await?))
    }

    async fn test(&self) -> anyhow::Result<()> {
        self.models().await.map(|_| ())
    }

    async fn models(&self) -> anyhow::Result<Vec<String>> {
        let resp = self
            .request(reqwest::Method::GET, "/models")
            .timeout(Duration::from_secs(10))
            .send()
            .await
            .with_context(|| format!("can't reach {}", self.base))?;
        let v: Value = check(resp).await?.json().await?;
        Ok(v["data"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|m| m["id"].as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed_all(events: &[Value]) -> Vec<Chunk> {
        let mut acc = Accum::default();
        let mut out = Vec::new();
        for e in events {
            out.extend(acc.feed(e).unwrap());
        }
        out.extend(acc.finish(None));
        out
    }

    #[test]
    fn text_stream() {
        let out = feed_all(&[
            json!({"choices":[{"delta":{"role":"assistant","content":"It's "}}]}),
            json!({"choices":[{"delta":{"content":"sunny."}}]}),
            json!({"choices":[{"delta":{},"finish_reason":"stop"}]}),
        ]);
        assert_eq!(
            out,
            [
                Chunk::Text("It's ".into()),
                Chunk::Text("sunny.".into()),
                Chunk::Done(StopReason::EndTurn)
            ]
        );
    }

    #[test]
    fn tool_call_whole_and_in_pieces() {
        // Ollama sends the whole call in one event.
        let whole = feed_all(&[json!({"choices":[{"delta":{"tool_calls":[
            {"id":"call_a","index":0,"type":"function","function":{"name":"set_timer","arguments":"{\"minutes\":5}"}}
        ]},"finish_reason":"tool_calls"}]})]);
        assert_eq!(
            whole,
            [
                Chunk::ToolCall(ToolCall {
                    id: "call_a".into(),
                    name: "set_timer".into(),
                    args: json!({"minutes": 5})
                }),
                Chunk::Done(StopReason::ToolUse)
            ]
        );

        // OpenAI streams arguments in fragments.
        let pieces = feed_all(&[
            json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","function":{"name":"add_alarm","arguments":""}}]}}]}),
            json!({"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"time\":"}}]}}]}),
            json!({"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"07:00\"}"}}]}}]}),
            json!({"choices":[{"delta":{},"finish_reason":"tool_calls"}]}),
        ]);
        assert_eq!(
            pieces[0],
            Chunk::ToolCall(ToolCall {
                id: "c1".into(),
                name: "add_alarm".into(),
                args: json!({"time": "07:00"})
            })
        );
    }

    #[test]
    fn errors_and_done_without_finish() {
        let mut acc = Accum::default();
        assert!(acc
            .feed(&json!({"error":{"message":"model not found"}}))
            .is_err());
        // Stream ends with [DONE] and no finish_reason → still one Done.
        let out = feed_all(&[json!({"choices":[{"delta":{"content":"hi"}}]})]);
        assert_eq!(out.last(), Some(&Chunk::Done(StopReason::EndTurn)));
        let mut acc = Accum::default();
        assert_eq!(acc.finish(Some("stop")).len(), 1);
        assert!(acc.finish(Some("stop")).is_empty());
    }

    #[test]
    fn request_shape() {
        let msgs = [
            Message::system("be brief"),
            Message::user("timer 5 min"),
            Message::assistant(
                "",
                vec![ToolCall {
                    id: "c1".into(),
                    name: "set_timer".into(),
                    args: json!({"minutes": 5}),
                }],
            ),
            Message::tool_result("c1", "started"),
        ];
        let tools = [ToolSpec {
            name: "set_timer".into(),
            description: "Start a timer".into(),
            parameters: json!({"type":"object","properties":{"minutes":{"type":"number"}}}),
        }];
        let body = request_body("llama3.2:3b", &msgs, &tools);
        assert_eq!(body["stream"], true);
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(
            body["messages"][2]["tool_calls"][0]["function"]["arguments"],
            "{\"minutes\":5}"
        );
        assert_eq!(body["messages"][3]["tool_call_id"], "c1");
        assert_eq!(body["tools"][0]["function"]["name"], "set_timer");
        assert!(request_body("m", &msgs[..2], &[]).get("tools").is_none());
    }

    /// Live check against the local Ollama container:
    /// `cargo test -p ai -- --ignored --nocapture`
    #[tokio::test]
    #[ignore]
    async fn live_ollama() {
        let base = std::env::var("OLLAMA_URL").unwrap_or_else(|_| "http://127.0.0.1:11434".into());
        let model = std::env::var("OLLAMA_MODEL").unwrap_or_else(|_| "llama3.2:3b".into());
        let p = OpenAiCompatible::ollama(&base).unwrap();
        let models = p.models().await.unwrap();
        println!("models: {models:?}");
        assert!(models.contains(&model));

        let t0 = std::time::Instant::now();
        let mut s = p
            .chat(
                &model,
                &[Message::user("Reply with exactly: hello clock")],
                &[],
            )
            .await
            .unwrap();
        let mut text = String::new();
        while let Some(c) = s.next().await {
            if let Chunk::Text(t) = c.unwrap() {
                text.push_str(&t);
            }
        }
        println!("text ({:?}): {text}", t0.elapsed());
        assert!(text.to_lowercase().contains("hello"));

        let tools = [ToolSpec {
            name: "set_timer".into(),
            description: "Start a countdown timer.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "minutes": {"type": "number", "description": "Length in minutes"},
                    "name": {"type": "string", "description": "Optional label"}
                },
                "required": ["minutes"]
            }),
        }];
        let t0 = std::time::Instant::now();
        let mut s = p
            .chat(
                &model,
                &[Message::user(
                    "Start a 5 minute timer for the pasta, please.",
                )],
                &tools,
            )
            .await
            .unwrap();
        let mut calls = Vec::new();
        while let Some(c) = s.next().await {
            if let Chunk::ToolCall(tc) = c.unwrap() {
                calls.push(tc);
            }
        }
        println!("tool calls ({:?}): {calls:?}", t0.elapsed());
        assert_eq!(calls.first().map(|c| c.name.as_str()), Some("set_timer"));
    }
}
