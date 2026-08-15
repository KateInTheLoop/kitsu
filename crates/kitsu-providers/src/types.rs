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

/// OpenAI-compatible strict JSON Schema response format. Gateways receive this
/// out-of-band from the conversation, so it constrains final assistant text
/// without becoming a user-message prompt convention.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JsonSchemaFormat {
    pub name: String,
    pub schema: serde_json::Value,
    pub strict: bool,
}

/// A single chat request to a provider.
#[derive(Debug, Clone)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    pub tools: Option<Vec<ToolDescriptor>>,
    pub stream: bool,
    pub temperature: Option<f32>,
    /// Optional OpenAI-compatible reasoning-effort hint. Gateways that support
    /// reasoning models receive this as `reasoning_effort`; ordinary models may
    /// ignore it when omitted.
    pub reasoning_effort: Option<ReasoningEffort>,
    pub max_tokens: Option<u32>,
    pub response_format: Option<JsonSchemaFormat>,
}

/// Common OpenAI-compatible reasoning-effort levels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningEffort {
    None,
    Minimal,
    Low,
    Medium,
    High,
    #[serde(rename = "xhigh")]
    XHigh,
}

/// Why the model stopped generating.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinishReason {
    Stop,
    Length,
    ToolCalls,
}

/// Token usage reported by the gateway for one completion. Anthropic (and most
/// OpenAI-compat) responses carry this in a `usage` object; some gateways/models
/// omit it, so every field defaults to 0 and callers should treat an all-zero
/// `Usage` as "not reported".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub prompt_tokens: u64,
    #[serde(default)]
    pub completion_tokens: u64,
    #[serde(default)]
    pub total_tokens: u64,
}

impl Usage {
    /// True when the gateway reported nothing usable (all fields zero).
    pub fn is_empty(&self) -> bool {
        self.prompt_tokens == 0 && self.completion_tokens == 0 && self.total_tokens == 0
    }

    /// Merge another usage into this one (summed across multi-round turns).
    pub fn add(&mut self, other: Usage) {
        self.prompt_tokens += other.prompt_tokens;
        self.completion_tokens += other.completion_tokens;
        self.total_tokens += other.total_tokens;
    }
}

/// One incremental piece of a streamed response.
#[derive(Debug, Clone, PartialEq)]
pub enum ChatDelta {
    /// A fragment of assistant text.
    Text(String),
    /// A (possibly partial, accumulated) tool call surfaced when complete.
    ToolCall(ToolCall),
    /// Token usage for this completion, surfaced before `Done` when the gateway
    /// reports it. May appear at most once per response.
    Usage(Usage),
    /// Terminal marker for the stream.
    Done(FinishReason),
}
