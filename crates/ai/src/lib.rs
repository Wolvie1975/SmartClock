//! AI providers for the assistant (DEVELOPMENT.md section 10).
//!
//! One [`Provider`] trait; adapters:
//! - [`openai::OpenAiCompatible`]: Ollama (`/v1`), and later OpenAI / OpenRouter.
//! - Anthropic and Gemini come next.
//!
//! Replies stream as [`Chunk`]s so the transcript can fill in as the model writes.

use futures::stream::BoxStream;
use serde::{Deserialize, Serialize};

pub mod openai;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

/// A tool call the model asked for.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub args: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Message {
    pub role: Role,
    pub content: String,
    /// Assistant messages: tool calls it made.
    pub tool_calls: Vec<ToolCall>,
    /// Tool messages: which call this answers.
    pub tool_call_id: Option<String>,
}

impl Message {
    pub fn system(text: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: text.into(),
            tool_calls: vec![],
            tool_call_id: None,
        }
    }
    pub fn user(text: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: text.into(),
            tool_calls: vec![],
            tool_call_id: None,
        }
    }
    pub fn assistant(text: impl Into<String>, tool_calls: Vec<ToolCall>) -> Self {
        Self {
            role: Role::Assistant,
            content: text.into(),
            tool_calls,
            tool_call_id: None,
        }
    }
    pub fn tool_result(call_id: impl Into<String>, result: impl Into<String>) -> Self {
        Self {
            role: Role::Tool,
            content: result.into(),
            tool_calls: vec![],
            tool_call_id: Some(call_id.into()),
        }
    }
}

/// A tool the model may call. `parameters` is a JSON Schema object.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    EndTurn,
    ToolUse,
    MaxTokens,
    Other,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Chunk {
    Text(String),
    ToolCall(ToolCall),
    Done(StopReason),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capabilities {
    pub tools: bool,
    pub streaming: bool,
    pub vision: bool,
    /// Runs on this machine; nothing leaves the laptop.
    pub local: bool,
}

pub type ChunkStream = BoxStream<'static, anyhow::Result<Chunk>>;

#[async_trait::async_trait]
pub trait Provider: Send + Sync {
    /// "ollama", "anthropic", "gemini", …
    fn id(&self) -> &str;
    fn capabilities(&self, model: &str) -> Capabilities;
    async fn chat(
        &self,
        model: &str,
        messages: &[Message],
        tools: &[ToolSpec],
    ) -> anyhow::Result<ChunkStream>;
    /// Settings → TEST: check the server is reachable and the credentials work.
    async fn test(&self) -> anyhow::Result<()>;
    /// Model ids the server offers (for the Settings picker).
    async fn models(&self) -> anyhow::Result<Vec<String>>;
}
