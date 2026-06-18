//! The runtime agent loop. Implements the pseudocode in
//! docs/architecture.md §5: call model → parse tool calls → run tools →
//! append results → repeat until the model stops or a cap is hit.

use futures::StreamExt;
use kitsu_providers::{
    ChatDelta, ChatMessage, ChatRequest, FinishReason, Provider, ProviderError, Role, ToolCall,
};
use kitsu_tools::Registry;
use thiserror::Error;

#[cfg(test)]
#[path = "orchestrator_tests.rs"]
mod orchestrator_tests;

/// Tuning for the agent loop.
#[derive(Debug, Clone)]
pub struct AgentConfig {
    pub model: String,
    /// Hard backstop against infinite tool loops (design §5). Default 10.
    pub max_rounds: u32,
    /// Context byte budget; oldest tool results truncated first. Default 128 KiB.
    pub max_context_bytes: usize,
    pub stream: bool,
    pub temperature: Option<f32>,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            model: String::new(),
            max_rounds: 10,
            max_context_bytes: 128 * 1024,
            stream: true,
            temperature: None,
        }
    }
}

#[derive(Debug, Error)]
pub enum AgentError {
    #[error(transparent)]
    Provider(#[from] ProviderError),

    #[error("agent loop error: {0}")]
    Loop(String),
}

/// Run one user turn through the agent loop. `messages` is the full conversation
/// so far (system + history + latest user message). Returns the final assistant
/// text. Tools may be empty (degenerates to a single round — the MVP `ask` path).
///
/// Generic over `P: Provider` so tests can pass a mock provider.
pub async fn run_agent_turn<P>(
    provider: &P,
    tools: &Registry,
    mut messages: Vec<ChatMessage>,
    cfg: &AgentConfig,
) -> Result<String, AgentError>
where
    P: Provider,
{
    let mut last_assistant_text = String::new();
    let mut round: u32 = 0;

    loop {
        round += 1;
        if round > cfg.max_rounds {
            // (2) max-rounds backstop: stop gracefully, note it, return text.
            last_assistant_text.push_str("\n[stopped: max tool rounds reached]");
            return Ok(last_assistant_text);
        }

        enforce_context_budget(&mut messages, cfg.max_context_bytes);

        let req = ChatRequest {
            model: cfg.model.clone(),
            messages: messages.clone(),
            tools: tool_descriptors(tools),
            stream: cfg.stream,
            temperature: cfg.temperature,
            max_tokens: None,
        };

        // 401/403 surface here (or as the first stream item) as a hard error.
        let mut stream = provider.chat_stream(req).await?;

        let mut assistant_text = String::new();
        let mut tool_calls: Vec<ToolCall> = Vec::new();
        let mut finish = FinishReason::Stop;

        while let Some(item) = stream.next().await {
            match item? {
                ChatDelta::Text(t) => {
                    if cfg.stream {
                        kitsu_render::print_fragment(&t);
                    }
                    assistant_text.push_str(&t);
                }
                ChatDelta::ToolCall(tc) => tool_calls.push(tc),
                ChatDelta::Done(reason) => finish = reason,
            }
        }

        last_assistant_text = assistant_text.clone();

        // Record the assistant turn (text + any requested tool calls).
        messages.push(ChatMessage::assistant(
            assistant_text.clone(),
            tool_calls.clone(),
        ));

        if tool_calls.is_empty() {
            // (1) terminal: model stopped with no tool calls.
            match finish {
                FinishReason::Stop | FinishReason::Length => return Ok(assistant_text),
                // No tool calls but ToolCalls finish reason: nothing to run, end.
                FinishReason::ToolCalls => return Ok(assistant_text),
            }
        }

        // Tool calls present: run each, feed result (or error) back as a tool msg.
        for tc in tool_calls {
            let content = match tools.run(&tc.tool_name, tc.args.clone()).await {
                Ok(out) => out,
                Err(e) => {
                    // error-as-tool-result: serialize and continue (design §5).
                    serde_json::json!({ "error": e.kind(), "detail": e.to_string() }).to_string()
                }
            };
            messages.push(ChatMessage::tool(tc.call_id.clone(), content));
        }
        // continue to next round
    }
}

/// Build the tool descriptor list advertised to the model. The MVP registry
/// does not carry descriptors (only handlers), so when empty we advertise no
/// `tools` field at all (`None`); future phases can surface real descriptors.
fn tool_descriptors(_tools: &Registry) -> Option<Vec<kitsu_providers::ToolDescriptor>> {
    None
}

/// Enforce the context byte budget by truncating the oldest tool-result
/// messages first. Never drops the system prompt or the last user message
/// (design §5 context-growth management).
fn enforce_context_budget(messages: &mut [ChatMessage], max_bytes: usize) {
    let total: usize = messages.iter().map(|m| m.byte_size()).sum();
    if total <= max_bytes {
        return;
    }

    // Index of the last user message; it must be preserved.
    let last_user_idx = messages.iter().rposition(|m| m.role == Role::User);

    let mut current = total;
    let mut i = 0;
    while current > max_bytes && i < messages.len() {
        let is_protected = messages[i].role == Role::System
            || Some(i) == last_user_idx
            || messages[i].role != Role::Tool;
        if messages[i].role == Role::Tool && !is_protected {
            let elided = ChatMessage::tool(
                messages[i].tool_call_id.clone().unwrap_or_default(),
                "[older tool output elided]",
            );
            current -= messages[i].byte_size();
            current += elided.byte_size();
            messages[i] = elided;
        }
        i += 1;
    }
}
