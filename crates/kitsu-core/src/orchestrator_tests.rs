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
    /// Records the `max_tokens` field of every request.
    seen_max_tokens: Arc<Mutex<Vec<Option<u32>>>>,
}

impl MockProvider {
    fn new(scripts: Vec<Script>) -> Self {
        Self {
            scripts: Arc::new(scripts),
            calls: Arc::new(AtomicUsize::new(0)),
            seen_messages: Arc::new(Mutex::new(Vec::new())),
            seen_tools: Arc::new(Mutex::new(Vec::new())),
            seen_max_tokens: Arc::new(Mutex::new(Vec::new())),
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
        self.seen_max_tokens.lock().unwrap().push(req.max_tokens);

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
        context_window: None,
        stream: false,
        temperature: None,
        max_tokens: None,
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

/// Robustness (robustness fix): on the FINAL permitted round the loop
/// must withhold tools so the model is forced to produce a final answer, rather
/// than emitting another tool call that gets truncated to the stop-marker.
/// Here the provider always tool-calls when tools are offered, and returns real
/// text when they are not. With max_rounds=3 we expect: rounds 1-2 offer tools,
/// round 3 offers none → a real answer, NOT the "[stopped...]" marker.
#[tokio::test]
async fn test_final_round_withholds_tools_and_forces_answer() {
    let provider = MockProvider::new(vec![
        // rounds 1 & 2: tool-call (tools were offered)
        vec![
            ChatDelta::ToolCall(tc("loop_tool", "c1", "{}")),
            ChatDelta::Done(FinishReason::ToolCalls),
        ],
        vec![
            ChatDelta::ToolCall(tc("loop_tool", "c2", "{}")),
            ChatDelta::Done(FinishReason::ToolCalls),
        ],
        // round 3 (final): the model produces its real answer
        vec![
            ChatDelta::Text("FINAL REVIEW BODY".to_string()),
            ChatDelta::Done(FinishReason::Stop),
        ],
    ]);
    let mut tools = Registry::new();
    tools.register_with_descriptor(
        kitsu_providers::ToolDescriptor {
            name: "loop_tool".to_string(),
            description: "loops".to_string(),
            parameters: serde_json::json!({"type": "object"}),
        },
        ok_handler(r#"{"ok":true}"#),
    );

    let out = run_agent_turn(&provider, &tools, &mut initial_messages(), &cfg(3))
        .await
        .expect("should produce a final answer, not error");

    assert_eq!(
        out, "FINAL REVIEW BODY",
        "final round must return real text"
    );
    assert!(
        !out.contains("max tool rounds reached"),
        "forcing a final answer must avoid the truncation stop-marker"
    );

    // Tools offered on rounds 1 & 2, withheld (None) on the final round 3.
    let seen = provider.seen_tools.lock().unwrap();
    assert_eq!(seen.len(), 3);
    assert!(seen[0].is_some(), "round 1 should offer tools");
    assert!(seen[1].is_some(), "round 2 should offer tools");
    assert!(
        seen[2].is_none(),
        "final round must withhold tools to force a final answer"
    );
}

/// The force-final-answer logic must NOT trigger for a single-round budget
/// (max_rounds=1): there is no earlier round to have gathered context, so a
/// 1-round turn should behave exactly as before (tools offered, normal stop).
#[tokio::test]
async fn test_single_round_budget_still_offers_tools() {
    let provider = MockProvider::new(vec![vec![
        ChatDelta::Text("answer".to_string()),
        ChatDelta::Done(FinishReason::Stop),
    ]]);
    let mut tools = Registry::new();
    tools.register_with_descriptor(
        kitsu_providers::ToolDescriptor {
            name: "loop_tool".to_string(),
            description: "loops".to_string(),
            parameters: serde_json::json!({"type": "object"}),
        },
        ok_handler(r#"{"ok":true}"#),
    );

    let _ = run_agent_turn(&provider, &tools, &mut initial_messages(), &cfg(1))
        .await
        .expect("ok");

    let seen = provider.seen_tools.lock().unwrap();
    assert_eq!(seen.len(), 1);
    assert!(
        seen[0].is_some(),
        "a 1-round budget must still offer tools (no force-final special-case)"
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

    assert!(
        out.contains("done"),
        "should reach the second round, got: {out}"
    );
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
        second_req.iter().any(|m| m.content.contains("first reply")),
        "second turn should include first turn's assistant message"
    );
    assert!(
        second_req
            .iter()
            .any(|m| m.content.contains("second question")),
        "second turn should include the new user message"
    );
}

/// max_tokens from AgentConfig must propagate onto every ChatRequest
/// (robustness fix: None lets the gateway cap output at ~4096, truncating
/// long reviews). When set, the request carries it.
#[tokio::test]
async fn test_max_tokens_propagates_to_request() {
    let provider = MockProvider::new(vec![vec![
        ChatDelta::Text("ok".to_string()),
        ChatDelta::Done(FinishReason::Stop),
    ]]);
    let tools = Registry::new();
    let mut c = cfg(5);
    c.max_tokens = Some(16000);
    let _ = run_agent_turn(&provider, &tools, &mut initial_messages(), &c)
        .await
        .expect("ok");
    let seen = provider.seen_max_tokens.lock().unwrap();
    assert_eq!(seen[0], Some(16000), "max_tokens must reach the request");
}

/// Default (None) is preserved — we don't silently force a cap.
#[tokio::test]
async fn test_max_tokens_default_is_none() {
    let provider = MockProvider::new(vec![vec![
        ChatDelta::Text("ok".to_string()),
        ChatDelta::Done(FinishReason::Stop),
    ]]);
    let tools = Registry::new();
    let _ = run_agent_turn(&provider, &tools, &mut initial_messages(), &cfg(5))
        .await
        .expect("ok");
    assert_eq!(provider.seen_max_tokens.lock().unwrap()[0], None);
}

// --- text-protocol tool calls (ISSUE-001) -----------------------------------

/// A descriptor-carrying registry whose tool bumps `ran` if it is ever invoked
/// (the negative tests assert it stays 0).
fn writing_registry(ran: Arc<AtomicUsize>) -> Registry {
    let mut tools = Registry::new();
    tools.register_with_descriptor(
        kitsu_providers::ToolDescriptor {
            name: "fs_write_file".to_string(),
            description: "write a file".to_string(),
            parameters: serde_json::json!({"type": "object"}),
        },
        Box::new(move |_args: String| {
            let ran = ran.clone();
            Box::pin(async move {
                ran.fetch_add(1, Ordering::SeqCst);
                Ok::<String, ToolError>("{}".to_string())
            })
        }),
    );
    tools
}

/// Run one text-protocol case: round 1 returns `text` (a text-form tool call)
/// with Done(Stop) — exactly how local models present it — round 2 finishes.
/// Assert the tool ran and its result was fed back on round 2.
async fn assert_text_protocol_executes(text: &str) {
    let provider = MockProvider::new(vec![
        vec![
            ChatDelta::Text(text.to_string()),
            ChatDelta::Done(FinishReason::Stop),
        ],
        vec![
            ChatDelta::Text("done".to_string()),
            ChatDelta::Done(FinishReason::Stop),
        ],
    ]);
    let ran = Arc::new(AtomicUsize::new(0));
    let mut tools = Registry::new();
    let ran2 = ran.clone();
    tools.register_with_descriptor(
        kitsu_providers::ToolDescriptor {
            name: "fs_write_file".to_string(),
            description: "write a file".to_string(),
            parameters: serde_json::json!({"type": "object"}),
        },
        Box::new(move |_args: String| {
            let ran = ran2.clone();
            Box::pin(async move {
                ran.fetch_add(1, Ordering::SeqCst);
                Ok::<String, ToolError>(r#"{"written":"x.py","bytes":10}"#.to_string())
            })
        }),
    );

    let out = run_agent_turn(&provider, &tools, &mut initial_messages(), &cfg(10))
        .await
        .expect("text-protocol turn should succeed");

    assert_eq!(
        ran.load(Ordering::SeqCst),
        1,
        "the text-form tool call should have executed exactly once"
    );
    assert_eq!(
        provider.call_count(),
        2,
        "one tool round then a final round"
    );
    assert!(
        out.contains("done"),
        "final answer should be returned: {out}"
    );
    let seen = provider.seen_messages.lock().unwrap();
    assert!(
        seen.last()
            .unwrap()
            .iter()
            .any(|m| m.role == kitsu_providers::Role::Tool && m.content.contains("written")),
        "the tool result must be fed back on round 2"
    );
}

#[tokio::test]
async fn test_text_protocol_bare_json() {
    assert_text_protocol_executes(
        r#"{"name": "fs_write_file", "arguments": {"path": "x.py", "content": "print(1)"}}"#,
    )
    .await;
}

#[tokio::test]
async fn test_text_protocol_fenced_json() {
    assert_text_protocol_executes(
        "Sure, I'll write it:\n```json\n{\"name\": \"fs_write_file\", \"arguments\": {\"path\": \"x.py\"}}\n```",
    )
    .await;
}

#[tokio::test]
async fn test_text_protocol_tagged() {
    assert_text_protocol_executes(
        "<tool_call>{\"name\": \"fs_write_file\", \"parameters\": {\"path\": \"x.py\"}}</tool_call>",
    )
    .await;
}

/// Plain prose (even with a stray brace) must NOT be treated as a tool call —
/// it's a normal terminal answer, exactly one provider call.
#[tokio::test]
async fn test_text_protocol_ignores_prose() {
    let provider = MockProvider::new(vec![vec![
        ChatDelta::Text("Use a HashMap { key: value } for this.".to_string()),
        ChatDelta::Done(FinishReason::Stop),
    ]]);
    let ran = Arc::new(AtomicUsize::new(0));
    let tools = writing_registry(ran.clone());

    let out = run_agent_turn(&provider, &tools, &mut initial_messages(), &cfg(10))
        .await
        .expect("ok");

    assert_eq!(
        ran.load(Ordering::SeqCst),
        0,
        "prose must not trigger a tool"
    );
    assert_eq!(
        provider.call_count(),
        1,
        "no extra round for a plain answer"
    );
    assert!(out.contains("HashMap"));
}

/// A text-form call for an UNADVERTISED tool name is ignored (returned as text).
#[tokio::test]
async fn test_text_protocol_unknown_tool_ignored() {
    let provider = MockProvider::new(vec![vec![
        ChatDelta::Text(r#"{"name": "rm_rf", "arguments": {}}"#.to_string()),
        ChatDelta::Done(FinishReason::Stop),
    ]]);
    let ran = Arc::new(AtomicUsize::new(0));
    let tools = writing_registry(ran.clone());

    let _ = run_agent_turn(&provider, &tools, &mut initial_messages(), &cfg(10))
        .await
        .expect("ok");

    assert_eq!(ran.load(Ordering::SeqCst), 0, "unknown tool must not run");
    assert_eq!(provider.call_count(), 1);
}

/// ISSUE-006: a model may batch SEVERAL text-form tool calls in one message
/// (e.g. writing multiple files). All of them must execute, not just the first.
#[tokio::test]
async fn test_text_protocol_multiple_calls_in_one_turn() {
    let text = concat!(
        r#"{"name": "fs_write_file", "arguments": {"path": "a.py"}}"#,
        "\n",
        r#"{"name": "fs_write_file", "arguments": {"path": "b.py"}}"#,
        "\n",
        r#"{"name": "fs_write_file", "arguments": {"path": "c.py"}}"#,
    );
    let provider = MockProvider::new(vec![
        vec![
            ChatDelta::Text(text.to_string()),
            ChatDelta::Done(FinishReason::Stop),
        ],
        vec![
            ChatDelta::Text("done".to_string()),
            ChatDelta::Done(FinishReason::Stop),
        ],
    ]);
    let ran = Arc::new(AtomicUsize::new(0));
    let mut tools = Registry::new();
    let ran2 = ran.clone();
    tools.register_with_descriptor(
        kitsu_providers::ToolDescriptor {
            name: "fs_write_file".to_string(),
            description: "write a file".to_string(),
            parameters: serde_json::json!({"type": "object"}),
        },
        Box::new(move |_args: String| {
            let ran = ran2.clone();
            Box::pin(async move {
                ran.fetch_add(1, Ordering::SeqCst);
                Ok::<String, ToolError>("{}".to_string())
            })
        }),
    );

    let out = run_agent_turn(&provider, &tools, &mut initial_messages(), &cfg(10))
        .await
        .expect("ok");

    assert_eq!(
        ran.load(Ordering::SeqCst),
        3,
        "all three text-form calls in one turn should execute"
    );
    assert!(out.contains("done"));
    let seen = provider.seen_messages.lock().unwrap();
    let tool_msgs = seen
        .last()
        .unwrap()
        .iter()
        .filter(|m| m.role == kitsu_providers::Role::Tool)
        .count();
    assert_eq!(
        tool_msgs, 3,
        "three tool results must be fed back on round 2"
    );
}

// --- ISSUE-007: loop-breaker on repeated identical failures -----------------

/// A provider that ALWAYS emits the same failing tool call while tools are
/// offered, and produces a final text answer once tools are withheld — models
/// the "fixated on a denied call" pattern.
#[derive(Clone)]
struct FixatedProvider {
    calls: Arc<AtomicUsize>,
}

impl Provider for FixatedProvider {
    async fn healthcheck(&self) -> Result<(), ProviderError> {
        Ok(())
    }
    async fn chat_stream(&self, req: ChatRequest) -> Result<ChatStream, ProviderError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let deltas = if req.tools.is_some() {
            vec![
                ChatDelta::ToolCall(tc("bad_tool", "c", "{}")),
                ChatDelta::Done(FinishReason::ToolCalls),
            ]
        } else {
            vec![
                ChatDelta::Text("giving up on that tool; here is my answer".to_string()),
                ChatDelta::Done(FinishReason::Stop),
            ]
        };
        Ok(Box::pin(stream::iter(
            deltas.into_iter().map(Ok::<ChatDelta, ProviderError>),
        )))
    }
    async fn list_models(&self) -> Result<Option<Vec<String>>, ProviderError> {
        Ok(None)
    }
}

/// The loop-breaker must force a final answer after REPEAT_FAIL_LIMIT identical
/// failures — well before the (generous) max_rounds cap — instead of looping.
#[tokio::test]
async fn test_loop_breaker_on_repeated_failure() {
    let ran = Arc::new(AtomicUsize::new(0));
    let mut tools = Registry::new();
    tools.register_with_descriptor(
        kitsu_providers::ToolDescriptor {
            name: "bad_tool".to_string(),
            description: "always fails".to_string(),
            parameters: serde_json::json!({"type": "object"}),
        },
        err_handler(ran.clone()),
    );
    let calls = Arc::new(AtomicUsize::new(0));
    let provider = FixatedProvider {
        calls: calls.clone(),
    };

    let out = run_agent_turn(&provider, &tools, &mut initial_messages(), &cfg(10))
        .await
        .expect("must return gracefully, not error");

    // 2 failing rounds trip the breaker; round 3 withholds tools → final answer.
    assert!(
        calls.load(Ordering::SeqCst) <= 4,
        "loop-breaker should force-final well before max_rounds=10, got {} calls",
        calls.load(Ordering::SeqCst)
    );
    assert!(
        ran.load(Ordering::SeqCst) <= 3,
        "the failing tool must not run all 10 rounds, ran {}",
        ran.load(Ordering::SeqCst)
    );
    assert!(
        out.contains("giving up"),
        "should reach the forced final answer, got: {out}"
    );
    assert!(
        !out.contains("max tool rounds reached"),
        "must not hit the max-rounds stop marker"
    );
}

// --- tool-activity formatting -----------------------------------------------

#[test]
fn tool_activity_formatting() {
    use crate::orchestrator::{format_tool_header, format_tool_result, short_path};

    assert_eq!(short_path("/x/y/deck1.md"), "…/y/deck1.md");
    assert_eq!(short_path("a/b"), "a/b");
    assert_eq!(short_path("deck.md"), "deck.md");

    assert_eq!(
        format_tool_header("fs_read_file", r#"{"path":"/x/y/deck1.md"}"#),
        "Read(…/y/deck1.md)"
    );
    assert_eq!(
        format_tool_header("fs_write_file", r#"{"path":"out/fb.py","content":"x"}"#),
        "Write(out/fb.py)"
    );
    assert_eq!(
        format_tool_header("repo_grep", r#"{"pattern":"TODO","path":"/a/src"}"#),
        "Grep(\"TODO\", a/src)"
    );
    // Unknown tool / unparseable args fall back to the raw name.
    assert_eq!(format_tool_header("weird_tool", "not json"), "weird_tool");

    assert_eq!(
        format_tool_result("fs_write_file", r#"{"written":"x","bytes":42}"#, false),
        "42 bytes written"
    );
    assert_eq!(
        format_tool_result("repo_grep", "l1\nl2\nl3", false),
        "3 matches"
    );
    assert_eq!(
        format_tool_result("repo_grep", "(no matches)", false),
        "no matches"
    );
    assert_eq!(format_tool_result("fs_read_file", "a\nb", false), "2 lines");
    assert_eq!(
        format_tool_result(
            "fs_read_file",
            r#"{"error":"PolicyDenied","detail":"nope"}"#,
            true
        ),
        "error: PolicyDenied"
    );
}

// --- context trimming (Stage 1c) --------------------------------------------

#[test]
fn trim_elides_oldest_tool_results_first_and_preserves_turns() {
    use crate::context::message_tokens;
    use crate::orchestrator::trim_tool_results;
    use kitsu_providers::ChatMessage;

    let mut msgs = vec![
        ChatMessage::system("sys"),
        ChatMessage::user("hello"),
        ChatMessage::tool("id1", "x".repeat(4000).as_str()), // big, ~1000 tokens
        ChatMessage::assistant("thinking", vec![]),
        ChatMessage::tool("id2", "recent small result"),
    ];
    let total: u64 = msgs.iter().map(message_tokens).sum();
    assert!(total > 100);
    trim_tool_results(&mut msgs, total, 100, message_tokens);

    // Big tool result elided to the stub; system/user/assistant untouched.
    assert!(msgs[2].content.contains("elided"), "big tool result must be elided");
    assert_eq!(msgs[0].content, "sys");
    assert_eq!(msgs[1].content, "hello");
    assert_eq!(msgs[3].content, "thinking");
    // Now under target.
    let after: u64 = msgs.iter().map(message_tokens).sum();
    assert!(after <= 100, "must be at/under target after trimming, was {after}");
}

#[test]
fn trim_is_a_noop_when_already_under_budget() {
    use crate::context::message_tokens;
    use crate::orchestrator::trim_tool_results;
    use kitsu_providers::ChatMessage;

    let mut msgs = vec![
        ChatMessage::system("sys"),
        ChatMessage::tool("id1", "small"),
    ];
    let before: Vec<String> = msgs.iter().map(|m| m.content.clone()).collect();
    let total: u64 = msgs.iter().map(message_tokens).sum();
    trim_tool_results(&mut msgs, total, 10_000, message_tokens);
    let after: Vec<String> = msgs.iter().map(|m| m.content.clone()).collect();
    assert_eq!(before, after, "nothing should change under budget");
}

#[test]
fn enforce_budget_token_path_elides_when_window_set() {
    use crate::context::{ContextMeter, message_tokens};
    use crate::orchestrator::enforce_context_budget;
    use kitsu_providers::ChatMessage;

    let mut cfg = cfg(10);
    cfg.context_window = Some(200); // effective ~100, target ~80
    cfg.max_tokens = Some(64);
    let meter = ContextMeter::new(); // no anchor -> full estimate

    let mut msgs = vec![
        ChatMessage::system("sys"),
        ChatMessage::user("q"),
        ChatMessage::tool("id1", "z".repeat(8000).as_str()), // huge
    ];
    enforce_context_budget(&mut msgs, &cfg, &meter);
    assert!(msgs[2].content.contains("elided"));
    let total: u64 = msgs.iter().map(message_tokens).sum();
    let target = crate::context::ContextBudget::new(200, Some(64)).target;
    assert!(total <= target);
}

#[test]
fn enforce_budget_byte_path_when_window_unset() {
    use crate::orchestrator::enforce_context_budget;
    use crate::context::ContextMeter;
    use kitsu_providers::ChatMessage;

    let mut cfg = cfg(10);
    cfg.context_window = None; // byte fallback
    cfg.max_context_bytes = 100;
    let meter = ContextMeter::new();

    let mut msgs = vec![
        ChatMessage::system("sys"),
        ChatMessage::tool("id1", "b".repeat(500).as_str()),
    ];
    enforce_context_budget(&mut msgs, &cfg, &meter);
    assert!(msgs[1].content.contains("elided"), "byte path should still trim");
}

// --- turn-dropping (Stage 2) -------------------------------------------------

#[test]
fn drop_old_exchanges_keeps_system_and_recent_no_orphans() {
    use crate::context::message_tokens;
    use crate::orchestrator::drop_old_exchanges;
    use kitsu_providers::{ChatMessage, Role};

    // system + 4 exchanges: user + assistant(tool_call) + tool + assistant(text).
    let mut msgs = vec![ChatMessage::system("sys")];
    for k in 0..4 {
        msgs.push(ChatMessage::user(
            format!("question {k} {}", "x".repeat(200)).as_str(),
        ));
        msgs.push(ChatMessage::assistant("", vec![tc("t", &format!("c{k}"), "{}")]));
        msgs.push(ChatMessage::tool(format!("c{k}").as_str(), "some result"));
        msgs.push(ChatMessage::assistant(format!("answer {k}").as_str(), vec![]));
    }
    drop_old_exchanges(&mut msgs, 100, message_tokens);

    assert_eq!(msgs[0].content, "sys", "system prompt preserved");
    assert!(
        msgs.iter().any(|m| m.content.contains("trimmed to fit")),
        "a drop marker is inserted"
    );
    assert!(
        msgs.iter().any(|m| m.content.contains("question 3")),
        "the most recent exchange is kept"
    );
    assert!(
        !msgs.iter().any(|m| m.content.contains("question 0")),
        "the oldest exchange is dropped"
    );
    // No orphaned tool_result at the cut: after system, the next message is the
    // user marker (a valid exchange boundary), not a dangling tool/assistant.
    assert_eq!(msgs[1].role, Role::User);
}

#[test]
fn drop_old_exchanges_noop_with_few_exchanges() {
    use crate::context::message_tokens;
    use crate::orchestrator::drop_old_exchanges;
    use kitsu_providers::ChatMessage;

    let mut msgs = vec![
        ChatMessage::system("sys"),
        ChatMessage::user("only question"),
        ChatMessage::assistant("only answer", vec![]),
    ];
    let before = msgs.len();
    drop_old_exchanges(&mut msgs, 1, message_tokens); // target tiny, but nothing safely droppable
    assert_eq!(msgs.len(), before, "must not drop the only/recent exchanges");
}

#[test]
fn enforce_budget_cascade_drops_turns_when_elision_insufficient() {
    use crate::context::{message_tokens, ContextMeter};
    use crate::orchestrator::enforce_context_budget;
    use kitsu_providers::ChatMessage;

    let mut cfg = cfg(10);
    cfg.context_window = Some(400);
    cfg.max_tokens = Some(64);
    let meter = ContextMeter::new();

    // Many big NON-tool turns (elision can't help) across several exchanges.
    let mut msgs = vec![ChatMessage::system("sys")];
    for k in 0..5 {
        msgs.push(ChatMessage::user(
            format!("q{k} {}", "y".repeat(300)).as_str(),
        ));
        msgs.push(ChatMessage::assistant(
            format!("a{k} {}", "z".repeat(300)).as_str(),
            vec![],
        ));
    }
    let before: u64 = msgs.iter().map(message_tokens).sum();
    enforce_context_budget(&mut msgs, &cfg, &meter);
    let after: u64 = msgs.iter().map(message_tokens).sum();
    assert!(after < before, "cascade must shrink the conversation ({before} -> {after})");
    assert_eq!(msgs[0].content, "sys");
    // Most recent exchange survives.
    assert!(msgs.iter().any(|m| m.content.contains("q4")));
}
