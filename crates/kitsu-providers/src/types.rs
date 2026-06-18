//! Shared, provider-agnostic data model. AUTHORITATIVE CONTRACT — both the
//! Developer (impl) and Tester (tests) build against these signatures. Changes
//! here are a design decision, not an implementation detail.

use serde::{Deserialize, Serialize};

/// Conversation role. Maps directly onto the OpenAI-compatible wire shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

/// A single tool/function call requested by the model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCall {
    pub tool_name: String,
    /// Raw JSON arguments as a string (accumulated from streamed deltas).
    pub args: String,
    /// Correlation id used to match the tool result back to this call.
    pub call_id: String,
}

/// Internal message type (provider-agnostic). Providers translate to/from their
/// own JSON at the edges; core/orchestrator only ever sees this.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: Role,
    pub content: String,
    /// For tool-result messages: the `call_id` of the originating tool call.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub tool_call_id: Option<String>,
    /// For assistant messages that requested tools (native mode).
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub tool_calls: Vec<ToolCall>,
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: content.into(),
            tool_call_id: None,
            tool_calls: Vec::new(),
        }
    }
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: content.into(),
            tool_call_id: None,
            tool_calls: Vec::new(),
        }
    }
    pub fn assistant(content: impl Into<String>, tool_calls: Vec<ToolCall>) -> Self {
        Self {
            role: Role::Assistant,
            content: content.into(),
            tool_call_id: None,
            tool_calls,
        }
    }
    pub fn tool(call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: Role::Tool,
            content: content.into(),
            tool_call_id: Some(call_id.into()),
            tool_calls: Vec::new(),
        }
    }

    /// Approximate byte size, used by the orchestrator's context-budget enforcement.
    pub fn byte_size(&self) -> usize {
        self.content.len()
            + self.tool_call_id.as_ref().map_or(0, |s| s.len())
            + self
                .tool_calls
                .iter()
                .map(|c| c.tool_name.len() + c.args.len() + c.call_id.len())
                .sum::<usize>()
    }
}

/// A function/tool descriptor advertised to the model (OpenAI `tools` format).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolDescriptor {
    pub name: String,
    pub description: String,
    /// JSON Schema object describing the parameters.
    pub parameters: serde_json::Value,
}

/// A single chat request to a provider.
#[derive(Debug, Clone)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    pub tools: Option<Vec<ToolDescriptor>>,
    pub stream: bool,
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
}

/// Why the model stopped generating.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinishReason {
    Stop,
    Length,
    ToolCalls,
}

/// One incremental piece of a streamed response.
#[derive(Debug, Clone, PartialEq)]
pub enum ChatDelta {
    /// A fragment of assistant text.
    Text(String),
    /// A (possibly partial, accumulated) tool call surfaced when complete.
    ToolCall(ToolCall),
    /// Terminal marker for the stream.
    Done(FinishReason),
}
