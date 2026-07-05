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
    /// Max output tokens per response. `None` lets the gateway apply its own
    /// (often small, e.g. 4096) default — which silently truncates long outputs
    /// like code reviews. Set this to lift the cap.
    pub max_tokens: Option<u32>,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            model: String::new(),
            max_rounds: 10,
            max_context_bytes: 128 * 1024,
            stream: true,
            temperature: None,
            max_tokens: None,
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
///
/// As a side effect, when the gateway reported token usage, a single machine-
/// readable `kitsu-usage: {...}` line is written to stderr after the turn — used
/// by external consumers for cost reporting. Never on
/// stdout, which carries only the assistant text.
pub async fn run_agent_turn<P>(
    provider: &P,
    tools: &Registry,
    messages: &mut Vec<ChatMessage>,
    cfg: &AgentConfig,
) -> Result<String, AgentError>
where
    P: Provider,
{
    let mut turn_usage = kitsu_providers::Usage::default();
    let result = run_agent_turn_inner(provider, tools, messages, cfg, &mut turn_usage).await;
    // Emit usage regardless of Ok/Err — partial turns still cost tokens. Only
    // when the gateway actually reported something.
    if !turn_usage.is_empty() {
        // ISSUE-003: when streaming, the assistant text was written to stdout
        // without a trailing newline. Terminate that line before the usage line
        // (stderr) so the two don't run together (`OKkitsu-usage:` → `OK\nkitsu-usage:`).
        if cfg.stream {
            kitsu_render::finish_line();
        }
        kitsu_render::emit_usage(
            turn_usage.prompt_tokens,
            turn_usage.completion_tokens,
            turn_usage.total_tokens,
        );
    }
    result
}

/// The agent loop proper. Accumulates per-round token usage into `turn_usage`
/// so the public wrapper can emit it once after the turn ends (success or error).
async fn run_agent_turn_inner<P>(
    provider: &P,
    tools: &Registry,
    messages: &mut Vec<ChatMessage>,
    cfg: &AgentConfig,
    turn_usage: &mut kitsu_providers::Usage,
) -> Result<String, AgentError>
where
    P: Provider,
{
    let mut last_assistant_text = String::new();
    let mut round: u32 = 0;

    // ISSUE-007 loop-breaker: track consecutive identical tool-call failures so a
    // model that fixates on a failing call (e.g. a denied path) is forced to a
    // final answer instead of burning every round. Signature = tool_name + args.
    let mut consecutive_failures: std::collections::HashMap<String, u32> =
        std::collections::HashMap::new();
    let mut force_final_next = false;
    const REPEAT_FAIL_LIMIT: u32 = 2;

    loop {
        round += 1;
        if round > cfg.max_rounds {
            // (2) max-rounds backstop: stop gracefully, note it, return text.
            last_assistant_text.push_str("\n[stopped: max tool rounds reached]");
            return Ok(last_assistant_text);
        }

        enforce_context_budget(messages, cfg.max_context_bytes);

        // On the FINAL permitted round, withhold tools and inject a nudge so the
        // model must produce its final answer instead of emitting yet another
        // tool call that we'd truncate to "[stopped: max tool rounds reached]".
        // This is what turned tool-aware reviews of large repos from 36-byte
        // stubs into real reviews (robustness fix). A round budget of
        // 1 has no "earlier" round to nudge from, so only applies when >1.
        // Final round when: the round budget is exhausted, OR a tool call has
        // failed identically REPEAT_FAIL_LIMIT times (ISSUE-007 loop-breaker).
        let is_final_round =
            (round == cfg.max_rounds && cfg.max_rounds > 1) || force_final_next;
        let offered_tools = if is_final_round {
            None
        } else {
            tool_descriptors(tools)
        };
        if is_final_round && tool_descriptors(tools).is_some() {
            let nudge = if force_final_next {
                "A tool call kept failing the same way. Stop retrying it — produce your \
                 complete final answer now using what you already have, or explain what \
                 you could not do."
            } else {
                "You have reached the final step and may not call any more tools. \
                 Produce your complete final answer now using what you have already \
                 gathered."
            };
            messages.push(ChatMessage::user(nudge));
        }
        // Captured before `offered_tools` is moved into the request; drives the
        // text-protocol fallback below.
        let tools_were_offered = offered_tools.is_some();

        let req = ChatRequest {
            model: cfg.model.clone(),
            messages: messages.clone(),
            tools: offered_tools,
            stream: cfg.stream,
            temperature: cfg.temperature,
            max_tokens: cfg.max_tokens,
        };

        // 401/403 surface here (or as the first stream item) as a hard error.
        let mut stream = provider.chat_stream(req).await?;

        let mut assistant_text = String::new();
        let mut tool_calls: Vec<ToolCall> = Vec::new();
        let mut finish = FinishReason::Stop;

        while let Some(item) = stream.next().await {
            match item? {
                ChatDelta::Text(t) => {
                    // ISSUE-002: when tools are advertised this round, DON'T stream
                    // text live — buffer it. A text-protocol model may be emitting
                    // a tool call as JSON; we suppress that and only print real
                    // answers (below). The final round withholds tools, so final
                    // answers still stream live.
                    if cfg.stream && !tools_were_offered {
                        kitsu_render::print_fragment(&t);
                    }
                    assistant_text.push_str(&t);
                }
                ChatDelta::ToolCall(tc) => tool_calls.push(tc),
                ChatDelta::Usage(u) => turn_usage.add(u),
                ChatDelta::Done(reason) => finish = reason,
            }
        }

        // Text-protocol fallback (ISSUE-001): some models — especially local ones
        // via Ollama — emit a tool call as plain-text JSON in `content` instead of
        // structured `tool_calls`. If tools were advertised this round and none
        // came back structured, try to parse one from the text and execute it.
        let mut synthesized_call = false;
        if tool_calls.is_empty() && tools_were_offered {
            let recovered = parse_text_tool_calls(&assistant_text, tools);
            if !recovered.is_empty() {
                tracing::debug!(count = recovered.len(), "recovered text-protocol tool call(s)");
                tool_calls.extend(recovered);
                synthesized_call = true;
            }
        }

        last_assistant_text = assistant_text.clone();

        // Record the assistant turn. When we synthesized a call from text, store
        // empty content so the raw tool-call JSON doesn't pollute the history.
        let assistant_content = if synthesized_call {
            String::new()
        } else {
            assistant_text.clone()
        };
        messages.push(ChatMessage::assistant(assistant_content, tool_calls.clone()));

        if tool_calls.is_empty() {
            // This round was a real answer, not a tool call. If we buffered it
            // (tools were offered, so live streaming was withheld to hide any
            // tool-call JSON — ISSUE-002), emit the answer now.
            if cfg.stream && tools_were_offered {
                kitsu_render::print_fragment(&assistant_text);
            }
            // (1) terminal: model stopped with no tool calls.
            match finish {
                FinishReason::Stop => return Ok(assistant_text),
                // Truncated by the model's output cap — surface it instead of
                // letting a cut-off answer masquerade as a complete one.
                FinishReason::Length => {
                    tracing::warn!("response truncated: model hit its output length limit");
                    assistant_text.push_str("\n[truncated: model output length limit reached]");
                    return Ok(assistant_text);
                }
                // No tool calls but ToolCalls finish reason: nothing to run, end.
                FinishReason::ToolCalls => return Ok(assistant_text),
            }
        }

        // Tool calls present but the response was length-truncated: the tool
        // arguments may be cut off (invalid JSON). Surface it rather than running
        // them blindly — the tool's own arg validation will then fail loudly.
        if finish == FinishReason::Length {
            tracing::warn!(
                "tool-call round truncated by the model's output length limit; \
                 tool arguments may be incomplete"
            );
        }

        // Tool calls present: run each, feed result (or error) back as a tool msg.
        for tc in tool_calls {
            let sig = format!("{}\u{0}{}", tc.tool_name, tc.args);
            let content = match tools.run(&tc.tool_name, tc.args.clone()).await {
                Ok(out) => {
                    consecutive_failures.remove(&sig); // a success resets the streak
                    out
                }
                Err(e) => {
                    // error-as-tool-result: serialize and continue (design §5). Also
                    // count identical consecutive failures so the loop-breaker can
                    // force a final answer instead of looping (ISSUE-007).
                    let n = consecutive_failures.entry(sig).or_insert(0);
                    *n += 1;
                    if *n >= REPEAT_FAIL_LIMIT {
                        force_final_next = true;
                    }
                    serde_json::json!({ "error": e.kind(), "detail": e.to_string() }).to_string()
                }
            };
            messages.push(ChatMessage::tool(tc.call_id.clone(), content));
        }
        // continue to next round
    }
}

/// Build the tool descriptor list advertised to the model. Tools registered
/// with a descriptor are surfaced; an empty set means no `tools` field is sent
/// at all (`None`), preserving the descriptor-free `ask` path.
fn tool_descriptors(tools: &Registry) -> Option<Vec<kitsu_providers::ToolDescriptor>> {
    let d = tools.descriptors();
    if d.is_empty() {
        None
    } else {
        Some(d.to_vec())
    }
}

/// Text-protocol tool-call parser (ISSUE-001, ISSUE-006). Some models return tool
/// calls as plain-text JSON in `content` (bare, in a ```` ```json ```` fence, or
/// wrapped in `<tool_call>…</tool_call>`) instead of the structured `tool_calls`
/// field — and a model may batch SEVERAL calls in one message (e.g. writing
/// multiple files). Parse EVERY balanced JSON object from `text`; for each with a
/// `name` matching an advertised tool plus `arguments`/`parameters`, synthesize a
/// [`ToolCall`]. Returns them in order (empty if none parse/match).
fn parse_text_tool_calls(text: &str, tools: &Registry) -> Vec<ToolCall> {
    let descriptors = tools.descriptors();
    let mut calls = Vec::new();
    if descriptors.is_empty() {
        return calls;
    }
    for candidate in extract_json_objects(text) {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&candidate) else {
            continue;
        };
        let Some(obj) = value.as_object() else { continue };
        let Some(name) = obj.get("name").and_then(|n| n.as_str()) else {
            continue;
        };
        if !descriptors.iter().any(|d| d.name == name) {
            continue;
        }
        // `arguments` may be an object or a JSON-encoded string; `parameters` is a
        // common alias. Default to `{}` when the model omitted args entirely.
        let args = match obj.get("arguments").or_else(|| obj.get("parameters")) {
            Some(serde_json::Value::String(s)) => s.clone(),
            Some(v) => v.to_string(),
            None => "{}".to_string(),
        };
        let call_id = format!("textcall-{}", calls.len());
        calls.push(ToolCall {
            tool_name: name.to_string(),
            args,
            call_id,
        });
    }
    calls
}

/// Return every balanced top-level `{…}` JSON object substring in `text`,
/// respecting string literals and escapes so braces inside strings don't
/// miscount. Nested objects are part of their enclosing top-level object; only
/// the outermost objects are returned.
fn extract_json_objects(text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'{' {
            if let Some(end) = balanced_object_end(bytes, i) {
                // i and end both index ASCII braces → valid char boundaries.
                out.push(text[i..=end].to_string());
                i = end + 1;
                continue;
            }
        }
        i += 1;
    }
    out
}

/// Index of the `}` that closes the `{` at `start`, or `None` if unbalanced.
fn balanced_object_end(bytes: &[u8], start: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut in_str = false;
    let mut escaped = false;
    for (i, &c) in bytes.iter().enumerate().skip(start) {
        if in_str {
            if escaped {
                escaped = false;
            } else if c == b'\\' {
                escaped = true;
            } else if c == b'"' {
                in_str = false;
            }
        } else {
            match c {
                b'"' => in_str = true,
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(i);
                    }
                }
                _ => {}
            }
        }
    }
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
