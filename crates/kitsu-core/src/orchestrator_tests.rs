//! Tests for the agent loop (design §5). TESTER-owned; no production logic here.
//! The Developer fills `run_agent_turn`'s body.
//!
//! These tests drive `run_agent_turn` with a MOCK Provider whose `chat_stream`
//! returns a scripted `ChatStream` per round, and count provider calls with an
//! AtomicUsize. No env, no network — so no `serial_test` is needed.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use futures::stream;
use kitsu_providers::{
    ChatDelta, ChatRequest, ChatStream, FinishReason, Provider, ProviderError, ToolCall,
};
use kitsu_tools::{HandlerFn, Registry, ToolError};

use crate::orchestrator::{run_agent_turn, AgentConfig};

/// One scripted round's worth of deltas.
type Script = Vec<ChatDelta>;

/// A mock Provider that yields a pre-scripted `ChatStream` per round.
///
/// - `scripts`: one entry per round; the Nth call returns the Nth script.
/// - When calls exceed the script list, the last script is repeated (used by the
///   max-rounds test, which scripts an always-tool-calling provider).
/// - `calls`: counts how many times `chat_stream` was invoked.
/// - `seen_messages`: records the request messages of every call so tests can
///   inspect that tool results were fed back.
#[derive(Clone)]
struct MockProvider {
    scripts: Arc<Vec<Script>>,
    calls: Arc<AtomicUsize>,
    seen_messages: Arc<Mutex<Vec<Vec<kitsu_providers::ChatMessage>>>>,
    /// Records the `tools` field of every request so tests can assert what was
    /// advertised to the model.
    seen_tools: Arc<Mutex<Vec<Option<Vec<kitsu_providers::ToolDescriptor>>>>>,
}

impl MockProvider {
    fn new(scripts: Vec<Script>) -> Self {
        Self {
            scripts: Arc::new(scripts),
            calls: Arc::new(AtomicUsize::new(0)),
            seen_messages: Arc::new(Mutex::new(Vec::new())),
            seen_tools: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn call_count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl Provider for MockProvider {
    async fn healthcheck(&self) -> Result<(), ProviderError> {
        Ok(())
    }

    async fn chat_stream(&self, req: ChatRequest) -> Result<ChatStream, ProviderError> {
        let idx = self.calls.fetch_add(1, Ordering::SeqCst);
        self.seen_messages
            .lock()
            .unwrap()
            .push(req.messages.clone());
        self.seen_tools.lock().unwrap().push(req.tools.clone());

        let script = self
            .scripts
            .get(idx)
            .or_else(|| self.scripts.last())
            .cloned()
            .unwrap_or_default();

        let items = script.into_iter().map(Ok::<ChatDelta, ProviderError>);
        let s: ChatStream = Box::pin(stream::iter(items.collect::<Vec<_>>()));
        Ok(s)
    }

    async fn list_models(&self) -> Result<Option<Vec<String>>, ProviderError> {
        Ok(None)
    }
}

fn tc(name: &str, call_id: &str, args: &str) -> ToolCall {
    ToolCall {
        tool_name: name.to_string(),
        args: args.to_string(),
        call_id: call_id.to_string(),
    }
}

fn cfg(max_rounds: u32) -> AgentConfig {
    AgentConfig {
        model: "example-model-v1".to_string(),
        max_rounds,
        max_context_bytes: 128 * 1024,
        stream: false,
        temperature: None,
    }
}

/// A handler that always returns a fixed JSON string.
fn ok_handler(json: &'static str) -> HandlerFn {
    Box::new(move |_args: String| {
        Box::pin(async move { Ok::<String, ToolError>(json.to_string()) })
    })
}

/// A handler that always errors, and bumps a counter so the test can confirm it ran.
fn err_handler(ran: Arc<AtomicUsize>) -> HandlerFn {
    Box::new(move |_args: String| {
        let ran = ran.clone();
        Box::pin(async move {
            ran.fetch_add(1, Ordering::SeqCst);
            Err::<String, ToolError>(ToolError::ExecError("boom".to_string()))
        })
    })
}

fn initial_messages() -> Vec<kitsu_providers::ChatMessage> {
    vec![
        kitsu_providers::ChatMessage::system("you are a test bot"),
        kitsu_providers::ChatMessage::user("hello"),
    ]
}

// --- single round, no tools -------------------------------------------------

/// design §5 termination (1): provider returns text then Done(Stop), empty
/// registry → returns the text, exactly one provider call.
#[tokio::test]
async fn test_loop_single_round_no_tools() {
    let provider = MockProvider::new(vec![vec![
        ChatDelta::Text("the answer is 42".to_string()),
        ChatDelta::Done(FinishReason::Stop),
    ]]);
    let tools = Registry::new();

    let out = run_agent_turn(&provider, &tools, &mut initial_messages(), &cfg(10))
        .await
        .expect("single-round turn should succeed");

    assert!(
        out.contains("the answer is 42"),
        "final text should contain the assistant message, got: {out}"
    );
    assert_eq!(
        provider.call_count(),
        1,
        "no-tools path should make exactly one call"
    );
}

// --- native single tool round -----------------------------------------------

/// design §5: round 1 returns a ToolCall + Done(ToolCalls); the registered tool
/// returns a known JSON string; round 2 returns Done(Stop) with text. The tool
/// runs and the final text is returned; exactly two provider rounds.
#[tokio::test]
async fn test_loop_native_single_tool_round() {
    let provider = MockProvider::new(vec![
        // round 1: request a tool
        vec![
            ChatDelta::ToolCall(tc("get_time", "call_1", "{}")),
            ChatDelta::Done(FinishReason::ToolCalls),
        ],
        // round 2: final answer
        vec![
            ChatDelta::Text("it is noon".to_string()),
            ChatDelta::Done(FinishReason::Stop),
        ],
    ]);

    let mut tools = Registry::new();
    tools.register("get_time", ok_handler(r#"{"time":"12:00"}"#));

    let out = run_agent_turn(&provider, &tools, &mut initial_messages(), &cfg(10))
        .await
        .expect("tool round should succeed");

    assert!(
        out.contains("it is noon"),
        "final text should be the round-2 answer: {out}"
    );
    assert_eq!(
        provider.call_count(),
        2,
        "a single tool round means exactly two provider calls"
    );

    // The round-2 request must have carried the tool result back to the model.
    let seen = provider.seen_messages.lock().unwrap();
    let round2 = seen.last().expect("there should be a round-2 request");
    assert!(
        round2
            .iter()
            .any(|m| m.role == kitsu_providers::Role::Tool && m.content.contains("12:00")),
        "round 2 must include the tool-result message fed back to the model"
    );
}

// --- max rounds cap ---------------------------------------------------------

/// design §5 termination (2): provider ALWAYS returns a ToolCall (never stops).
/// With max_rounds = 3, the loop stops after 3 rounds (does not hang) and returns
/// without error. Exactly 3 provider calls.
#[tokio::test]
async fn test_loop_max_rounds_cap() {
    // A single always-tool-calling script; MockProvider repeats the last script.
    let provider = MockProvider::new(vec![vec![
        ChatDelta::ToolCall(tc("loop_tool", "call_x", "{}")),
        ChatDelta::Done(FinishReason::ToolCalls),
    ]]);

    let mut tools = Registry::new();
    tools.register("loop_tool", ok_handler(r#"{"ok":true}"#));

    let result = run_agent_turn(&provider, &tools, &mut initial_messages(), &cfg(3)).await;

    assert!(
        result.is_ok(),
        "hitting max_rounds must stop gracefully, not error: {result:?}"
    );
    assert_eq!(
        provider.call_count(),
        3,
        "the loop must cap at exactly max_rounds (3) provider calls and not hang"
    );
}

// --- tool error fed back ----------------------------------------------------

/// design §5 error-as-tool-result: a tool that returns Err does NOT abort the
/// loop — its error is serialized as a tool message and the loop continues; the
/// provider then returns Done(Stop). The tool ran and the run succeeds.
#[tokio::test]
async fn test_loop_tool_error_fed_back() {
    let provider = MockProvider::new(vec![
        // round 1: request the failing tool
        vec![
            ChatDelta::ToolCall(tc("bad_tool", "call_err", "{}")),
            ChatDelta::Done(FinishReason::ToolCalls),
        ],
        // round 2: recover and finish
        vec![
            ChatDelta::Text("recovered".to_string()),
            ChatDelta::Done(FinishReason::Stop),
        ],
    ]);

    let ran = Arc::new(AtomicUsize::new(0));
    let mut tools = Registry::new();
    tools.register("bad_tool", err_handler(ran.clone()));

    let out = run_agent_turn(&provider, &tools, &mut initial_messages(), &cfg(10))
        .await
        .expect("a failing tool must not abort the loop");

    assert_eq!(
        ran.load(Ordering::SeqCst),
        1,
        "the failing tool should have been invoked once"
    );
    assert_eq!(
        provider.call_count(),
        2,
        "the loop must continue a second round after the tool error (not abort)"
    );
    assert!(
        out.contains("recovered"),
        "the loop should reach the round-2 answer: {out}"
    );

    // The error must have been fed back as a tool message on round 2.
    let seen = provider.seen_messages.lock().unwrap();
    let round2 = seen.last().expect("there should be a round-2 request");
    assert!(
        round2
            .iter()
            .any(|m| m.role == kitsu_providers::Role::Tool && m.content.contains("ExecError")),
        "round 2 must include the serialized tool error fed back to the model"
    );
}

/// Fix #4: a `FinishReason::Length` (model hit its output cap) must be surfaced
/// to the caller, not silently treated as a clean stop. The returned text keeps
/// the partial content and gains a visible truncation marker.
#[tokio::test]
async fn test_loop_length_finish_is_surfaced() {
    let provider = MockProvider::new(vec![vec![
        ChatDelta::Text("partial answer that got cut o".to_string()),
        ChatDelta::Done(FinishReason::Length),
    ]]);
    let tools = Registry::new();

    let out = run_agent_turn(&provider, &tools, &mut initial_messages(), &cfg(10))
        .await
        .expect("length-truncated turn should still return Ok with the partial text");

    assert!(
        out.contains("partial answer that got cut o"),
        "partial content should be preserved, got: {out}"
    );
    assert!(
        out.contains("truncated"),
        "truncation should be surfaced to the caller, got: {out}"
    );
    assert_eq!(provider.call_count(), 1);
}

/// Fix #4 (edge case): a `Length` finish during a TOOL-CALL round must not be
/// silently ignored. The loop still runs the tool (so a recoverable round can
/// proceed) but the truncation is logged; here we assert the loop continues to
/// a second round and completes rather than mishandling the truncated round.
#[tokio::test]
async fn test_loop_length_during_tool_call_round() {
    let provider = MockProvider::new(vec![
        vec![
            ChatDelta::ToolCall(tc("get_time", "call_1", "{}")),
            ChatDelta::Done(FinishReason::Length),
        ],
        vec![
            ChatDelta::Text("done".to_string()),
            ChatDelta::Done(FinishReason::Stop),
        ],
    ]);
    let mut tools = Registry::new();
    tools.register("get_time", ok_handler(r#"{"time":"noon"}"#));

    let out = run_agent_turn(&provider, &tools, &mut initial_messages(), &cfg(10))
        .await
        .expect("truncated tool-call round should still proceed");

    assert!(out.contains("done"), "should reach the second round, got: {out}");
    assert_eq!(provider.call_count(), 2, "should run two rounds");
}

// --- tool descriptor advertising + multi-turn threading ---------------------

/// A registry with one descriptor-carrying tool advertises it to the model.
#[tokio::test]
async fn test_descriptors_advertised_to_model() {
    let provider = MockProvider::new(vec![vec![
        ChatDelta::Text("ok".to_string()),
        ChatDelta::Done(FinishReason::Stop),
    ]]);
    let mut tools = Registry::new();
    tools.register_with_descriptor(
        kitsu_providers::ToolDescriptor {
            name: "fs.read_file".to_string(),
            description: "read a file".to_string(),
            parameters: serde_json::json!({"type": "object"}),
        },
        ok_handler("{}"),
    );

    let mut messages = initial_messages();
    let _ = run_agent_turn(&provider, &tools, &mut messages, &cfg(10))
        .await
        .expect("turn ok");

    let seen = provider.seen_tools.lock().unwrap();
    let first = seen.first().expect("one call");
    let advertised = first.as_ref().expect("tools should be advertised");
    assert!(advertised.iter().any(|d| d.name == "fs.read_file"));
}

/// A registry built only with the legacy `register` (no descriptor) advertises
/// nothing — guards the descriptor-free `ask` path.
#[tokio::test]
async fn test_no_descriptors_means_no_tools_field() {
    let provider = MockProvider::new(vec![vec![
        ChatDelta::Text("ok".to_string()),
        ChatDelta::Done(FinishReason::Stop),
    ]]);
    let mut tools = Registry::new();
    tools.register("legacy_tool", ok_handler("{}"));

    let mut messages = initial_messages();
    let _ = run_agent_turn(&provider, &tools, &mut messages, &cfg(10))
        .await
        .expect("turn ok");

    let seen = provider.seen_tools.lock().unwrap();
    assert!(seen.first().expect("one call").is_none(), "no tools field");
}

/// Multi-turn: calling run_agent_turn twice with the same &mut Vec accumulates
/// history — the second call's request includes the first turn's messages.
#[tokio::test]
async fn test_multi_turn_history_accumulates() {
    let provider = MockProvider::new(vec![
        vec![
            ChatDelta::Text("first reply".to_string()),
            ChatDelta::Done(FinishReason::Stop),
        ],
        vec![
            ChatDelta::Text("second reply".to_string()),
            ChatDelta::Done(FinishReason::Stop),
        ],
    ]);
    let tools = Registry::new();
    let mut messages = initial_messages();

    let _ = run_agent_turn(&provider, &tools, &mut messages, &cfg(10))
        .await
        .expect("turn 1");
    // Simulate the chat loop pushing the next user message.
    messages.push(kitsu_providers::ChatMessage::user("second question"));
    let _ = run_agent_turn(&provider, &tools, &mut messages, &cfg(10))
        .await
        .expect("turn 2");

    // The second request must carry the first turn's assistant reply.
    let seen = provider.seen_messages.lock().unwrap();
    let second_req = &seen[1];
    assert!(
        second_req
            .iter()
            .any(|m| m.content.contains("first reply")),
        "second turn should include first turn's assistant message"
    );
    assert!(
        second_req
            .iter()
            .any(|m| m.content.contains("second question")),
        "second turn should include the new user message"
    );
}
