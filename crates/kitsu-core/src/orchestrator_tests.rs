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
        offload_dir: None,
        stream: false,
        temperature: None,
        max_tokens: None,
        compaction: crate::orchestrator::CompactionMode::Trim,
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
    assert!(
        msgs[2].content.contains("elided"),
        "big tool result must be elided"
    );
    assert_eq!(msgs[0].content, "sys");
    assert_eq!(msgs[1].content, "hello");
    assert_eq!(msgs[3].content, "thinking");
    // Now under target.
    let after: u64 = msgs.iter().map(message_tokens).sum();
    assert!(
        after <= 100,
        "must be at/under target after trimming, was {after}"
    );
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

#[tokio::test]
async fn enforce_budget_token_path_elides_when_window_set() {
    use crate::context::{message_tokens, ContextMeter};
    use crate::orchestrator::enforce_context_budget;
    use kitsu_providers::ChatMessage;

    let provider = MockProvider::new(vec![]);
    let mut cfg = cfg(10);
    cfg.context_window = Some(200); // effective ~100, target ~80
    cfg.max_tokens = Some(64);
    let mut meter = ContextMeter::new(); // no anchor -> full estimate

    let mut msgs = vec![
        ChatMessage::system("sys"),
        ChatMessage::user("q"),
        ChatMessage::tool("id1", "z".repeat(8000).as_str()), // huge
    ];
    let mut failures = 0u32;
    let mut turn_usage = kitsu_providers::Usage::default();
    enforce_context_budget(
        &provider,
        &mut msgs,
        &cfg,
        "",
        false,
        &mut meter,
        &mut turn_usage,
        &mut failures,
    )
    .await;
    assert!(msgs[2].content.contains("elided"));
    let total: u64 = msgs.iter().map(message_tokens).sum();
    let target = crate::context::ContextBudget::new(200, Some(64)).target;
    assert!(total <= target);
}

#[tokio::test]
async fn enforce_budget_byte_path_when_window_unset() {
    use crate::context::ContextMeter;
    use crate::orchestrator::enforce_context_budget;
    use kitsu_providers::ChatMessage;

    let provider = MockProvider::new(vec![]);
    let mut cfg = cfg(10);
    cfg.context_window = None; // byte fallback
    cfg.max_context_bytes = 100;
    let mut meter = ContextMeter::new();

    let mut msgs = vec![
        ChatMessage::system("sys"),
        ChatMessage::tool("id1", "b".repeat(500).as_str()),
    ];
    let mut failures = 0u32;
    let mut turn_usage = kitsu_providers::Usage::default();
    enforce_context_budget(
        &provider,
        &mut msgs,
        &cfg,
        "",
        false,
        &mut meter,
        &mut turn_usage,
        &mut failures,
    )
    .await;
    assert!(
        msgs[1].content.contains("elided"),
        "byte path should still trim"
    );
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
        msgs.push(ChatMessage::assistant(
            "",
            vec![tc("t", &format!("c{k}"), "{}")],
        ));
        msgs.push(ChatMessage::tool(format!("c{k}").as_str(), "some result"));
        msgs.push(ChatMessage::assistant(
            format!("answer {k}").as_str(),
            vec![],
        ));
    }
    let total: u64 = msgs.iter().map(message_tokens).sum();
    drop_old_exchanges(&mut msgs, total, 100, message_tokens);

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
    let total: u64 = msgs.iter().map(message_tokens).sum();
    drop_old_exchanges(&mut msgs, total, 1, message_tokens); // target tiny, but nothing safely droppable
    assert_eq!(
        msgs.len(),
        before,
        "must not drop the only/recent exchanges"
    );
}

#[tokio::test]
async fn enforce_budget_cascade_drops_turns_when_elision_insufficient() {
    use crate::context::{message_tokens, ContextMeter};
    use crate::orchestrator::enforce_context_budget;
    use kitsu_providers::ChatMessage;

    let provider = MockProvider::new(vec![]);
    let mut cfg = cfg(10);
    cfg.context_window = Some(400);
    cfg.max_tokens = Some(64);
    let mut meter = ContextMeter::new();

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
    let mut failures = 0u32;
    let mut turn_usage = kitsu_providers::Usage::default();
    enforce_context_budget(
        &provider,
        &mut msgs,
        &cfg,
        "",
        false,
        &mut meter,
        &mut turn_usage,
        &mut failures,
    )
    .await;
    let after: u64 = msgs.iter().map(message_tokens).sum();
    assert!(
        after < before,
        "cascade must shrink the conversation ({before} -> {after})"
    );
    assert_eq!(msgs[0].content, "sys");
    // Most recent exchange survives.
    assert!(msgs.iter().any(|m| m.content.contains("q4")));
}

// --- tool-result offload (Stage 2, Tier 0) ----------------------------------

#[test]
fn maybe_offload_writes_large_result_and_returns_pointer() {
    use crate::orchestrator::maybe_offload;
    let dir = tempfile::tempdir().unwrap();
    let big = "x".repeat(20_000);
    let out = maybe_offload(big.clone(), "call-1", Some(dir.path()));
    assert!(out.contains("saved to"), "returns a pointer");
    assert!(
        out.contains("fs_read_file"),
        "tells the model how to re-read"
    );
    assert!(
        out.len() < big.len(),
        "pointer is much smaller than the result"
    );
    // Full content persisted and intact (filename carries a unique seq suffix).
    let entry = std::fs::read_dir(dir.path())
        .unwrap()
        .next()
        .unwrap()
        .unwrap();
    assert!(entry.file_name().to_string_lossy().starts_with("call-1-"));
    assert_eq!(std::fs::read_to_string(entry.path()).unwrap(), big);
}

#[test]
fn maybe_offload_keeps_small_results_inline() {
    use crate::orchestrator::maybe_offload;
    let dir = tempfile::tempdir().unwrap();
    let small = "hello".to_string();
    assert_eq!(maybe_offload(small.clone(), "c", Some(dir.path())), small);
}

#[test]
fn maybe_offload_is_noop_without_a_dir() {
    use crate::orchestrator::maybe_offload;
    let big = "x".repeat(20_000);
    assert_eq!(maybe_offload(big.clone(), "c", None), big);
}

// --- summarizing compaction (Stage 3) ---------------------------------------

#[tokio::test]
async fn compaction_summarize_rebuilds_history_with_summary() {
    use crate::context::ContextMeter;
    use crate::orchestrator::{enforce_context_budget, CompactionMode};
    use kitsu_providers::ChatMessage;

    let provider = MockProvider::new(vec![vec![
        ChatDelta::Text("SUMMARY: earlier work on foo.rs".to_string()),
        ChatDelta::Done(FinishReason::Stop),
    ]]);
    let mut cfg = cfg(10);
    cfg.context_window = Some(400);
    cfg.max_tokens = Some(64);
    cfg.compaction = CompactionMode::Summarize;
    let mut meter = ContextMeter::new();

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
    let mut failures = 0u32;
    let mut turn_usage = kitsu_providers::Usage::default();
    enforce_context_budget(
        &provider,
        &mut msgs,
        &cfg,
        "",
        false,
        &mut meter,
        &mut turn_usage,
        &mut failures,
    )
    .await;

    assert_eq!(msgs[0].content, "sys", "system kept");
    assert!(
        msgs.iter()
            .any(|m| m.content.contains("SUMMARY: earlier work on foo.rs")),
        "the summary text is injected"
    );
    assert!(msgs.iter().any(|m| m.content.contains("q4")), "recent kept");
    assert!(
        !msgs.iter().any(|m| m.content.contains("q0")),
        "oldest summarized away"
    );
    assert_eq!(failures, 0, "success resets the breaker");
}

#[tokio::test]
async fn compaction_falls_back_to_drop_on_empty_summary() {
    use crate::context::ContextMeter;
    use crate::orchestrator::{enforce_context_budget, CompactionMode};
    use kitsu_providers::ChatMessage;

    // Provider returns no text → summary fails → fall back to dropping.
    let provider = MockProvider::new(vec![vec![ChatDelta::Done(FinishReason::Stop)]]);
    let mut cfg = cfg(10);
    cfg.context_window = Some(400);
    cfg.max_tokens = Some(64);
    cfg.compaction = CompactionMode::Summarize;
    let mut meter = ContextMeter::new();

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
    let before = msgs.len();
    let mut failures = 0u32;
    let mut turn_usage = kitsu_providers::Usage::default();
    enforce_context_budget(
        &provider,
        &mut msgs,
        &cfg,
        "",
        false,
        &mut meter,
        &mut turn_usage,
        &mut failures,
    )
    .await;

    assert_eq!(failures, 1, "empty summary trips the breaker once");
    assert!(msgs.len() < before, "still shrank by dropping");
    assert!(
        msgs.iter().any(|m| m.content.contains("trimmed to fit")),
        "drop marker present (fell back to drop)"
    );
}

// --- regressions: drop_old_exchanges underflow, offload-pointer survival,
// tool-schema token accounting -----------------------------------------------

/// `total` starts far smaller than a single exchange's naive `size` estimate —
/// mirrors the real bug: an anchored `total` (real API `prompt_tokens`) can be
/// smaller than what the per-message heuristic computes for a dense exchange.
/// A plain `total -= sum` would panic (debug, overflow checks on) or wrap to
/// u64::MAX (release), which would then keep the `while total > target` loop
/// running and drop every droppable exchange instead of stopping cleanly.
#[test]
fn drop_old_exchanges_does_not_underflow_when_per_exchange_estimate_exceeds_total() {
    use crate::orchestrator::drop_old_exchanges;
    use kitsu_providers::ChatMessage;

    let mut msgs = vec![ChatMessage::system("sys")];
    for k in 0..6 {
        msgs.push(ChatMessage::user(format!("q{k}").as_str()));
        msgs.push(ChatMessage::assistant(format!("a{k}").as_str(), vec![]));
    }
    // Deliberately dwarfs the tiny starting `total` of 10.
    let huge_size = |_: &ChatMessage| 1_000_000u64;
    drop_old_exchanges(&mut msgs, 10, 5, huge_size); // must not panic

    assert!(
        msgs.iter().any(|m| m.content.contains("trimmed to fit")),
        "should still drop at least the oldest exchange"
    );
    assert!(
        msgs.iter().any(|m| m.content.contains("q5")),
        "the most recent exchange must survive, not be wiped out by an underflow-driven over-drop"
    );
    assert!(
        msgs.iter().any(|m| m.content.contains("q4")),
        "KEEP_RECENT_EXCHANGES must still be honored"
    );
}

/// An already-offloaded tool result (a `maybe_offload` pointer) must keep its
/// on-disk path when it needs to be elided further — replacing it with the
/// generic "[older tool output elided]" stub would strand the file: the model
/// can no longer `fs_read_file` a path it can't see anymore.
#[test]
fn trim_tool_results_preserves_offload_pointer_but_drops_preview() {
    use crate::orchestrator::{maybe_offload, trim_tool_results};
    use kitsu_providers::ChatMessage;

    let dir = tempfile::tempdir().unwrap();
    let big = "x".repeat(20_000);
    let pointer = maybe_offload(big, "call-1", Some(dir.path()));
    assert!(
        pointer.contains("saved to"),
        "sanity: got an offload pointer"
    );
    assert!(
        pointer.contains("Preview:"),
        "sanity: pointer has a preview body"
    );

    let mut msgs = vec![
        ChatMessage::user("q"),
        ChatMessage::tool("call-1", pointer.as_str()),
        ChatMessage::user("more recent question"),
    ];
    let size = |m: &ChatMessage| m.byte_size() as u64;
    let total: u64 = msgs.iter().map(size).sum();
    // Impossibly small limit forces elision.
    trim_tool_results(&mut msgs, total, 1, size);

    let after = &msgs[1].content;
    assert!(
        after.contains("saved to"),
        "the on-disk pointer must survive re-elision: {after}"
    );
    assert!(
        !after.contains("Preview:"),
        "the preview body should be dropped to actually free space: {after}"
    );
    assert!(
        after.len() < pointer.len(),
        "elided form must still be smaller than the original pointer+preview"
    );
}

/// Re-eliding a message that's already just the bare pointer line (no preview
/// left) must be a no-op — otherwise repeated elision passes could still churn
/// or (if ever implemented carelessly) lose the pointer on a second pass.
#[test]
fn elided_tool_stub_is_idempotent_on_a_bare_pointer() {
    use crate::orchestrator::elided_tool_stub;
    use kitsu_providers::ChatMessage;

    let bare = ChatMessage::tool(
        "id",
        "[tool output 5.0 KB — full result saved to /tmp/x.txt. Re-read it (or a line range) \
         with fs_read_file on that path.]",
    );
    let stub = elided_tool_stub(&bare);
    assert_eq!(
        stub.content, bare.content,
        "an already-minimal pointer must be left exactly as-is"
    );
}

/// Regression: an ordinary tool result that happens to contain the phrase
/// "saved to" (e.g. a build tool's own log output) must NOT be mistaken for a
/// `maybe_offload` pointer — detection is anchored on the exact prefix
/// `maybe_offload` emits, not a bare substring match, so this collapses to the
/// normal generic elision stub instead of impersonating a (bogus) pointer.
#[test]
fn elided_tool_stub_does_not_false_positive_on_ordinary_saved_to_text() {
    use crate::orchestrator::elided_tool_stub;
    use kitsu_providers::ChatMessage;

    let ordinary = ChatMessage::tool(
        "id",
        "Compiling project...\nOutput saved to build/out.js\nDone in 1.2s",
    );
    let stub = elided_tool_stub(&ordinary);
    assert_eq!(
        stub.content, "[older tool output elided]",
        "ordinary output containing 'saved to' must get the generic stub, not be \
         mistaken for a recoverable offload pointer"
    );
}

/// Budgeting must count the tools' advertised JSON schema — real overhead on
/// every request — not just the conversation messages, when there's no usage
/// anchor to fall back on (the un-anchored, cold-estimate case).
#[tokio::test]
async fn enforce_budget_counts_tool_schema_tokens_when_unanchored() {
    use crate::context::ContextMeter;
    use crate::orchestrator::enforce_context_budget;
    use kitsu_providers::ChatMessage;

    let provider = MockProvider::new(vec![]);
    let mut cfg = cfg(10);
    cfg.context_window = Some(200); // target ~80 (see enforce_budget_token_path test)
    cfg.max_tokens = Some(64);
    let mut meter = ContextMeter::new(); // no anchor -> cold estimate

    let mut tools = Registry::new();
    // A large schema (~2000 chars, ~500+ tokens) that alone blows the ~80 target.
    tools.register_with_descriptor(
        kitsu_providers::ToolDescriptor {
            name: "big_tool".to_string(),
            description: "d".repeat(2000),
            parameters: serde_json::json!({"type": "object"}),
        },
        ok_handler("{}"),
    );
    let schema_json = serde_json::to_string(tools.descriptors()).unwrap();

    let mut msgs = vec![
        ChatMessage::system("sys"),
        ChatMessage::user("q"),
        // Small enough that the conversation ALONE stays well under the target;
        // large enough that eliding it is a real (not stub-inflating) win.
        ChatMessage::tool("id1", "z".repeat(200).as_str()),
    ];
    let mut failures = 0u32;
    let mut turn_usage = kitsu_providers::Usage::default();
    enforce_context_budget(
        &provider,
        &mut msgs,
        &cfg,
        &schema_json,
        true,
        &mut meter,
        &mut turn_usage,
        &mut failures,
    )
    .await;

    assert!(
        msgs[2].content.contains("elided"),
        "schema overhead must count toward the budget even though the conversation alone doesn't"
    );
}

/// When the meter IS anchored (a real `prompt_tokens` from a past request that
/// already included the schemas sent that round), the schema estimate must NOT
/// be added again — double-counting would trigger needless trimming/dropping.
#[tokio::test]
async fn enforce_budget_does_not_double_count_schema_tokens_when_anchored() {
    use crate::context::ContextMeter;
    use crate::orchestrator::enforce_context_budget;
    use kitsu_providers::ChatMessage;

    let provider = MockProvider::new(vec![]);
    let mut cfg = cfg(10);
    cfg.context_window = Some(1000);
    cfg.max_tokens = None; // reserve=500 -> effective=500 -> buffer=100 -> target=400

    let mut tools = Registry::new();
    tools.register_with_descriptor(
        kitsu_providers::ToolDescriptor {
            name: "big_tool".to_string(),
            description: "d".repeat(2000), // ~500+ tokens if double-counted
            parameters: serde_json::json!({"type": "object"}),
        },
        ok_handler("{}"),
    );
    let schema_json = serde_json::to_string(tools.descriptors()).unwrap();

    let mut msgs = vec![ChatMessage::system("sys")];
    for k in 0..3 {
        msgs.push(ChatMessage::user(format!("q{k}").as_str()));
        msgs.push(ChatMessage::assistant(format!("a{k}").as_str(), vec![]));
    }
    let sent_len = msgs.len();
    let mut meter = ContextMeter::new();
    // Real usage for exactly these messages was 300 tokens — comfortably under
    // the 400 target on its own (and already reflects that round's schema cost,
    // hence `had_schema: true`).
    meter.record(300, sent_len, true);

    let mut failures = 0u32;
    let mut turn_usage = kitsu_providers::Usage::default();
    enforce_context_budget(
        &provider,
        &mut msgs,
        &cfg,
        &schema_json,
        true,
        &mut meter,
        &mut turn_usage,
        &mut failures,
    )
    .await;

    assert_eq!(
        msgs.len(),
        sent_len,
        "anchored estimate must not double-count schema tokens and trigger a needless drop"
    );
}

/// The compaction summarizer issues an extra `chat_stream` call outside the
/// main round loop; its real token cost must be folded into the turn's usage
/// so `kitsu-usage:` (cost reporting) doesn't silently under-report spend.
#[tokio::test]
async fn compact_conversation_folds_summarizer_usage_into_turn_usage() {
    use crate::context::ContextMeter;
    use crate::orchestrator::{enforce_context_budget, CompactionMode};
    use kitsu_providers::{ChatMessage, Usage};

    let provider = MockProvider::new(vec![vec![
        ChatDelta::Text("SUMMARY: earlier work on foo.rs".to_string()),
        ChatDelta::Usage(Usage {
            prompt_tokens: 111,
            completion_tokens: 22,
            total_tokens: 133,
        }),
        ChatDelta::Done(FinishReason::Stop),
    ]]);
    let mut cfg = cfg(10);
    cfg.context_window = Some(400);
    cfg.max_tokens = Some(64);
    cfg.compaction = CompactionMode::Summarize;
    let mut meter = ContextMeter::new();

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
    let mut failures = 0u32;
    let mut turn_usage = Usage::default();
    enforce_context_budget(
        &provider,
        &mut msgs,
        &cfg,
        "",
        false,
        &mut meter,
        &mut turn_usage,
        &mut failures,
    )
    .await;

    assert_eq!(
        turn_usage.prompt_tokens, 111,
        "the summarizer call's real usage must be folded into the turn's usage"
    );
    assert_eq!(turn_usage.completion_tokens, 22);
}

/// Regression: when there's nothing safely droppable (too few exchanges),
/// `CompactionMode::Trim` must not discard a still-valid anchor. Invalidating
/// unconditionally would force every later estimate back to a full
/// re-tokenize for no reason — exactly in the large, over-budget conversation
/// case where the anchor's cheap anchor+tail path matters most.
#[tokio::test]
async fn enforce_budget_trim_keeps_a_valid_anchor_when_nothing_is_droppable() {
    use crate::context::ContextMeter;
    use crate::orchestrator::enforce_context_budget;
    use kitsu_providers::ChatMessage;

    let provider = MockProvider::new(vec![]);
    let mut cfg = cfg(10); // compaction defaults to Trim
    cfg.context_window = Some(200); // target ~80 (see enforce_budget_token_path test)
    cfg.max_tokens = Some(64);

    // Only ONE user exchange (<= KEEP_RECENT_EXCHANGES) — nothing is safely
    // droppable, so drop_old_exchanges is a guaranteed no-op on `messages`.
    let mut msgs = vec![ChatMessage::system("sys"), ChatMessage::user("q")];
    let mut meter = ContextMeter::new();
    let sent_len = msgs.len();
    // A real anchor that's over target, forcing the compaction match to run.
    meter.record(500, sent_len, true);

    let mut failures = 0u32;
    let mut turn_usage = kitsu_providers::Usage::default();
    enforce_context_budget(
        &provider,
        &mut msgs,
        &cfg,
        "",
        false,
        &mut meter,
        &mut turn_usage,
        &mut failures,
    )
    .await;

    assert!(
        meter.anchored(&msgs),
        "a guaranteed-no-op Trim pass must not discard a still-valid anchor"
    );
}
