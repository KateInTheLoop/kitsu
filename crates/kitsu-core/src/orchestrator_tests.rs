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
    ChatDelta, ChatRequest, ChatStream, FinishReason, JsonSchemaFormat, Provider, ProviderError,
    ToolCall,
};
use kitsu_tools::{HandlerFn, Registry, ToolError};

use crate::orchestrator::{
    run_agent_turn, run_agent_turn_with_state, AgentConfig, AgentSessionState, TurnCompletion,
};

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
    /// Records provider-native structured-output constraints.
    seen_response_formats: Arc<Mutex<Vec<Option<JsonSchemaFormat>>>>,
}

impl MockProvider {
    fn new(scripts: Vec<Script>) -> Self {
        Self {
            scripts: Arc::new(scripts),
            calls: Arc::new(AtomicUsize::new(0)),
            seen_messages: Arc::new(Mutex::new(Vec::new())),
            seen_tools: Arc::new(Mutex::new(Vec::new())),
            seen_max_tokens: Arc::new(Mutex::new(Vec::new())),
            seen_response_formats: Arc::new(Mutex::new(Vec::new())),
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
        self.seen_response_formats
            .lock()
            .unwrap()
            .push(req.response_format.clone());

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

/// A provider whose stream can fail after yielding one or more successful
/// deltas. The ordinary mock scripts only successful deltas.
#[derive(Clone)]
struct FallibleMockProvider {
    script: Arc<Vec<Result<ChatDelta, ProviderError>>>,
}

impl FallibleMockProvider {
    fn new(script: Vec<Result<ChatDelta, ProviderError>>) -> Self {
        Self {
            script: Arc::new(script),
        }
    }
}

impl Provider for FallibleMockProvider {
    async fn healthcheck(&self) -> Result<(), ProviderError> {
        Ok(())
    }

    async fn chat_stream(&self, _req: ChatRequest) -> Result<ChatStream, ProviderError> {
        Ok(Box::pin(stream::iter(self.script.as_ref().clone())))
    }

    async fn list_models(&self) -> Result<Option<Vec<String>>, ProviderError> {
        Ok(None)
    }
}

#[derive(Clone)]
struct TelemetryProvider;

impl Provider for TelemetryProvider {
    async fn healthcheck(&self) -> Result<(), ProviderError> {
        Ok(())
    }

    async fn chat_stream(&self, _req: ChatRequest) -> Result<ChatStream, ProviderError> {
        Ok(Box::pin(stream::iter(vec![Ok(ChatDelta::Done(
            FinishReason::Stop,
        ))])))
    }

    async fn chat_stream_with_telemetry(
        &self,
        req: ChatRequest,
        telemetry: Option<kitsu_providers::ProviderTelemetrySink>,
    ) -> Result<ChatStream, ProviderError> {
        if let Some(telemetry) = telemetry {
            telemetry.record(kitsu_providers::ProviderAttemptEvent {
                attempt: 1,
                max_attempts: 2,
                category: kitsu_providers::ProviderAttemptCategory::RateLimited,
                status: Some(429),
                outcome: kitsu_providers::ProviderAttemptOutcome::RetryScheduled,
                terminal: false,
                retry_delay_ms: Some(250),
                retry_after_used: false,
                remaining_deadline_ms: 10_000,
            });
            telemetry.record(kitsu_providers::ProviderAttemptEvent {
                attempt: 2,
                max_attempts: 2,
                category: kitsu_providers::ProviderAttemptCategory::Success,
                status: Some(200),
                outcome: kitsu_providers::ProviderAttemptOutcome::ResponseAccepted,
                terminal: true,
                retry_delay_ms: None,
                retry_after_used: false,
                remaining_deadline_ms: 9_700,
            });
        }
        self.chat_stream(req).await
    }

    async fn list_models(&self) -> Result<Option<Vec<String>>, ProviderError> {
        Ok(None)
    }
}

#[derive(Clone)]
struct SummaryTelemetryProvider;

impl Provider for SummaryTelemetryProvider {
    async fn healthcheck(&self) -> Result<(), ProviderError> {
        Ok(())
    }

    async fn chat_stream(&self, _req: ChatRequest) -> Result<ChatStream, ProviderError> {
        Ok(Box::pin(stream::iter(vec![
            Ok(ChatDelta::Text(
                "SUMMARY: earlier work on foo.rs".to_string(),
            )),
            Ok(ChatDelta::Usage(kitsu_providers::Usage {
                prompt_tokens: 111,
                completion_tokens: 22,
                total_tokens: 133,
                prompt_tokens_details: None,
            })),
            Ok(ChatDelta::Done(FinishReason::Stop)),
        ])))
    }

    async fn chat_stream_with_telemetry(
        &self,
        req: ChatRequest,
        telemetry: Option<kitsu_providers::ProviderTelemetrySink>,
    ) -> Result<ChatStream, ProviderError> {
        if let Some(telemetry) = telemetry {
            let _ = telemetry.record(kitsu_providers::ProviderAttemptEvent {
                attempt: 1,
                max_attempts: 1,
                category: kitsu_providers::ProviderAttemptCategory::Success,
                status: Some(200),
                outcome: kitsu_providers::ProviderAttemptOutcome::BodyCompleted,
                terminal: true,
                retry_delay_ms: None,
                retry_after_used: false,
                remaining_deadline_ms: 9_900,
            });
        }
        self.chat_stream(req).await
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
        request_timeout_ms: None,
        turn_deadline: None,
        max_rounds,
        max_context_bytes: 128 * 1024,
        context_window: None,
        offload_dir: None,
        artifact_policy: crate::orchestrator::ArtifactStoragePolicy::Automatic,
        stream: false,
        temperature: None,
        reasoning_effort: None,
        max_tokens: None,
        response_format: None,
        defer_response_format_until_final: false,
        structured_finalization_gate: None,
        compaction: crate::orchestrator::CompactionMode::Trim,
        quiet: false,
        event_log: None,
        skill_names: Vec::new(),
        run_tallies: None,
        spawn_budget: None,
    }
}

#[test]
fn spawn_budget_admits_saturates_and_resets() {
    let budget = crate::orchestrator::SpawnBudget::new(3);
    assert_eq!(budget.admit(2), 2);
    assert_eq!(budget.admit(2), 1, "partial admission at the boundary");
    assert_eq!(budget.admit(1), 0, "exhausted budget admits nothing");
    assert_eq!(budget.used(), 3);
    budget.reset();
    assert_eq!(budget.used(), 0);
    assert_eq!(budget.admit(1), 1, "a fresh turn has the whole cap again");
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

#[tokio::test]
async fn test_loop_forwards_and_discloses_structured_output_constraint() {
    use crate::orchestrator::{AgentEventLog, AgentRuntimeEvent};

    let provider = MockProvider::new(vec![vec![
        ChatDelta::Text(r#"{"answer":"42"}"#.to_string()),
        ChatDelta::Done(FinishReason::Stop),
    ]]);
    let tools = Registry::new();
    let schema = serde_json::json!({
        "type": "object",
        "properties": {"answer": {"type": "string"}},
        "required": ["answer"],
        "additionalProperties": false
    });
    let log = AgentEventLog::default();
    let mut config = cfg(1);
    config.response_format = Some(JsonSchemaFormat {
        name: "kitsu_output".to_string(),
        schema: schema.clone(),
        strict: true,
    });
    config.quiet = true;
    config.event_log = Some(log.clone());

    let output = run_agent_turn(&provider, &tools, &mut initial_messages(), &config)
        .await
        .expect("structured turn should complete");
    assert_eq!(output, r#"{"answer":"42"}"#);
    assert_eq!(
        provider.seen_response_formats.lock().unwrap().as_slice(),
        &[Some(JsonSchemaFormat {
            name: "kitsu_output".to_string(),
            schema,
            strict: true,
        })]
    );
    assert!(log.take().iter().any(|event| matches!(
        event,
        AgentRuntimeEvent::ProviderRequest {
            model: Some(model),
            structured_output: true,
            tools_offered: 0,
            ..
        } if model == "example-model-v1"
    )));
}

#[tokio::test]
async fn structured_output_can_be_reserved_for_a_tool_free_finalization_round() {
    use crate::orchestrator::{AgentEventLog, AgentRuntimeEvent};

    let provider = MockProvider::new(vec![
        vec![
            ChatDelta::ToolCall(tc("write_result", "write_1", "{}")),
            ChatDelta::Done(FinishReason::ToolCalls),
        ],
        vec![
            ChatDelta::Text("provisional action-phase answer".to_string()),
            ChatDelta::Done(FinishReason::Stop),
        ],
        vec![
            ChatDelta::Text(r#"{"answer":"verified"}"#.to_string()),
            ChatDelta::Usage(kitsu_providers::Usage {
                prompt_tokens: 100,
                completion_tokens: 5,
                total_tokens: 105,
                prompt_tokens_details: None,
            }),
            ChatDelta::Done(FinishReason::Stop),
        ],
    ]);
    let mut tools = Registry::new();
    tools.register_with_descriptor(
        kitsu_providers::ToolDescriptor {
            name: "write_result".to_string(),
            description: "write the required result".to_string(),
            parameters: serde_json::json!({"type": "object"}),
        },
        ok_handler(r#"{"changed":true}"#),
    );
    let schema = serde_json::json!({
        "type": "object",
        "properties": {"answer": {"type": "string"}},
        "required": ["answer"],
        "additionalProperties": false
    });
    let response_format = JsonSchemaFormat {
        name: "kitsu_output".to_string(),
        schema,
        strict: true,
    };
    let log = AgentEventLog::default();
    let mut config = cfg(4);
    config.response_format = Some(response_format.clone());
    config.defer_response_format_until_final = true;
    config.stream = true;
    config.quiet = true;
    config.event_log = Some(log.clone());

    let mut messages = initial_messages();
    let mut state = AgentSessionState::default();
    let output = run_agent_turn_with_state(&provider, &tools, &mut messages, &config, &mut state)
        .await
        .expect("two-phase structured turn should complete");

    assert_eq!(output, r#"{"answer":"verified"}"#);
    assert_eq!(
        provider.seen_response_formats.lock().unwrap().as_slice(),
        &[None, None, Some(response_format)]
    );
    let seen_tools = provider.seen_tools.lock().unwrap();
    assert!(seen_tools[0].is_some());
    assert!(seen_tools[1].is_some());
    assert!(seen_tools[2].is_none());
    let third_messages = &provider.seen_messages.lock().unwrap()[2];
    assert!(third_messages.iter().any(|message| {
        message.role == kitsu_providers::Role::User
            && message.content.contains("tool-action phase is complete")
    }));
    assert!(third_messages.iter().any(|message| {
        message.role == kitsu_providers::Role::Assistant
            && message.content == "provisional action-phase answer"
    }));

    let events = log.take();
    let requests = events
        .iter()
        .filter_map(|event| match event {
            AgentRuntimeEvent::ProviderRequest {
                tools_offered,
                structured_output,
                ..
            } => Some((*tools_offered, *structured_output)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(requests, vec![(1, false), (1, false), (0, true)]);
    let streamed = events
        .iter()
        .filter_map(|event| match event {
            AgentRuntimeEvent::AssistantDelta { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<String>();
    assert_eq!(
        streamed, output,
        "the provisional action answer stays hidden"
    );
    assert!(messages.iter().any(|message| {
        message.role == kitsu_providers::Role::Assistant
            && message
                .tool_calls
                .iter()
                .any(|call| call.call_id == "write_1")
    }));
    assert!(messages.iter().any(|message| {
        message.role == kitsu_providers::Role::Tool
            && message.tool_call_id.as_deref() == Some("write_1")
    }));
    assert!(messages.iter().all(|message| {
        message.content != "provisional action-phase answer"
            && !message.content.contains("tool-action phase is complete")
    }));
    assert_eq!(
        messages.last().map(|message| message.content.as_str()),
        Some(r#"{"answer":"verified"}"#)
    );
    assert!(
        !state.meter.anchored(&messages),
        "removing request-local handoff messages must invalidate the final request anchor"
    );
}

#[tokio::test]
async fn exact_output_gate_keeps_tools_available_for_one_bounded_repair() {
    use crate::orchestrator::{
        AgentEventLog, AgentRuntimeEvent, StructuredFinalizationGate, StructuredFinalizationRepair,
        StructuredFinalizationRepairTarget,
    };

    let provider = MockProvider::new(vec![
        vec![
            ChatDelta::ToolCall(tc("write_result", "write_wrong", "{}")),
            ChatDelta::Done(FinishReason::ToolCalls),
        ],
        vec![
            ChatDelta::Text("provisional before repair".to_string()),
            ChatDelta::Done(FinishReason::Stop),
        ],
        vec![
            ChatDelta::ToolCall(tc("write_result", "write_corrected", "{}")),
            ChatDelta::Done(FinishReason::ToolCalls),
        ],
        vec![
            ChatDelta::Text("provisional after repair".to_string()),
            ChatDelta::Done(FinishReason::Stop),
        ],
        vec![
            ChatDelta::Text(r#"{"answer":"verified"}"#.to_string()),
            ChatDelta::Usage(kitsu_providers::Usage {
                prompt_tokens: 120,
                completion_tokens: 5,
                total_tokens: 125,
                prompt_tokens_details: None,
            }),
            ChatDelta::Done(FinishReason::Stop),
        ],
    ]);
    let mut tools = Registry::new();
    tools.register_with_descriptor(
        kitsu_providers::ToolDescriptor {
            name: "write_result".to_string(),
            description: "write the required result".to_string(),
            parameters: serde_json::json!({"type": "object"}),
        },
        ok_handler(r#"{"changed":true}"#),
    );
    let response_format = JsonSchemaFormat {
        name: "kitsu_output".to_string(),
        schema: serde_json::json!({"type": "object"}),
        strict: true,
    };
    let evaluations = Arc::new(AtomicUsize::new(0));
    let gate_evaluations = evaluations.clone();
    let repair_instruction = "repair the exact bytes before finalizing".to_string();
    let gate_instruction = repair_instruction.clone();
    let target = StructuredFinalizationRepairTarget {
        path: "/work/result.json".to_string(),
        expected_sha256: "a".repeat(64),
        actual_sha256: "b".repeat(64),
    };
    let gate_target = target.clone();
    let log = AgentEventLog::default();
    let mut config = cfg(5);
    config.response_format = Some(response_format.clone());
    config.defer_response_format_until_final = true;
    config.structured_finalization_gate = Some(StructuredFinalizationGate::new(move || {
        (gate_evaluations.fetch_add(1, Ordering::SeqCst) == 0).then(|| {
            StructuredFinalizationRepair {
                cause: "required_write_sha256_mismatch".to_string(),
                instruction: gate_instruction.clone(),
                targets: vec![gate_target.clone()],
            }
        })
    }));
    config.quiet = true;
    config.event_log = Some(log.clone());

    let mut messages = initial_messages();
    let mut state = AgentSessionState::default();
    let output = run_agent_turn_with_state(&provider, &tools, &mut messages, &config, &mut state)
        .await
        .expect("the repaired turn should reach its reserved finalizer");

    assert_eq!(output, r#"{"answer":"verified"}"#);
    assert_eq!(
        evaluations.load(Ordering::SeqCst),
        1,
        "the remaining round is reserved without re-evaluating the repair gate"
    );
    assert_eq!(
        provider.seen_response_formats.lock().unwrap().as_slice(),
        &[None, None, None, None, Some(response_format)]
    );
    assert_eq!(
        provider
            .seen_tools
            .lock()
            .unwrap()
            .iter()
            .map(Option::is_some)
            .collect::<Vec<_>>(),
        vec![true, true, true, true, false]
    );
    let seen_messages = provider.seen_messages.lock().unwrap();
    assert!(seen_messages[2]
        .iter()
        .any(|message| message.role == kitsu_providers::Role::User
            && message.content == repair_instruction));
    assert!(seen_messages[2]
        .iter()
        .all(|message| message.content != "provisional before repair"));
    assert!(seen_messages[4]
        .iter()
        .any(|message| message.content == "provisional after repair"));
    assert!(seen_messages[4]
        .iter()
        .any(|message| message.content.contains("tool-action phase is complete")));
    drop(seen_messages);

    let events = log.take();
    assert!(events.iter().any(|event| matches!(
        event,
        AgentRuntimeEvent::StructuredFinalizationDeferred {
            round: 2,
            repair_round: 3,
            finalization_round_reserved: true,
            cause,
            targets,
            repair_instruction_disclosed: true,
            write_authority_granted: false,
        } if cause == "required_write_sha256_mismatch" && targets == &vec![target.clone()]
    )));
    assert!(messages.iter().any(|message| {
        message.role == kitsu_providers::Role::Assistant
            && message
                .tool_calls
                .iter()
                .any(|call| call.call_id == "write_corrected")
    }));
    assert!(messages.iter().all(|message| {
        message.content != repair_instruction
            && message.content != "provisional before repair"
            && message.content != "provisional after repair"
            && !message.content.contains("tool-action phase is complete")
    }));
    assert_eq!(
        messages.last().map(|message| message.content.as_str()),
        Some(r#"{"answer":"verified"}"#)
    );
    assert!(
        !state.meter.anchored(&messages),
        "request-local repair and finalization scaffolding invalidate usage anchors"
    );
}

#[tokio::test]
async fn exact_output_gate_does_not_consume_the_only_reserved_finalizer_round() {
    use crate::orchestrator::{
        AgentEventLog, AgentRuntimeEvent, StructuredFinalizationGate, StructuredFinalizationRepair,
    };

    let provider = MockProvider::new(vec![
        vec![
            ChatDelta::ToolCall(tc("write_result", "write_wrong", "{}")),
            ChatDelta::Done(FinishReason::ToolCalls),
        ],
        vec![
            ChatDelta::Text("provisional mismatch".to_string()),
            ChatDelta::Done(FinishReason::Stop),
        ],
        vec![
            ChatDelta::Text(r#"{"answer":"final"}"#.to_string()),
            ChatDelta::Done(FinishReason::Stop),
        ],
    ]);
    let mut tools = Registry::new();
    tools.register_with_descriptor(
        kitsu_providers::ToolDescriptor {
            name: "write_result".to_string(),
            description: "write the required result".to_string(),
            parameters: serde_json::json!({"type": "object"}),
        },
        ok_handler(r#"{"changed":true}"#),
    );
    let gate_calls = Arc::new(AtomicUsize::new(0));
    let observed_gate_calls = gate_calls.clone();
    let log = AgentEventLog::default();
    let mut config = cfg(3);
    config.response_format = Some(JsonSchemaFormat {
        name: "kitsu_output".to_string(),
        schema: serde_json::json!({"type": "object"}),
        strict: true,
    });
    config.defer_response_format_until_final = true;
    config.structured_finalization_gate = Some(StructuredFinalizationGate::new(move || {
        observed_gate_calls.fetch_add(1, Ordering::SeqCst);
        Some(StructuredFinalizationRepair {
            cause: "required_write_sha256_mismatch".to_string(),
            instruction: "repair".to_string(),
            targets: Vec::new(),
        })
    }));
    config.quiet = true;
    config.event_log = Some(log.clone());

    let output = run_agent_turn(&provider, &tools, &mut initial_messages(), &config)
        .await
        .expect("the finalizer remains available for terminal postcondition handling");

    assert_eq!(output, r#"{"answer":"final"}"#);
    assert_eq!(gate_calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        provider
            .seen_tools
            .lock()
            .unwrap()
            .iter()
            .map(Option::is_some)
            .collect::<Vec<_>>(),
        vec![true, true, false]
    );
    assert!(!log.take().iter().any(|event| matches!(
        event,
        AgentRuntimeEvent::StructuredFinalizationDeferred { .. }
    )));
}

#[tokio::test]
async fn deferred_structured_output_refuses_an_impossible_single_round_budget() {
    let provider = MockProvider::new(vec![vec![
        ChatDelta::Text("must not be requested".to_string()),
        ChatDelta::Done(FinishReason::Stop),
    ]]);
    let mut config = cfg(1);
    config.response_format = Some(JsonSchemaFormat {
        name: "kitsu_output".to_string(),
        schema: serde_json::json!({"type": "object"}),
        strict: true,
    });
    config.defer_response_format_until_final = true;

    let error = run_agent_turn(
        &provider,
        &Registry::new(),
        &mut initial_messages(),
        &config,
    )
    .await
    .expect_err("one round cannot contain action and structured finalization");

    assert!(error.to_string().contains("at least two rounds"));
    assert_eq!(provider.call_count(), 0);
}

#[tokio::test]
async fn deferred_structured_finalization_never_executes_an_unoffered_tool_call() {
    let provider = MockProvider::new(vec![
        vec![
            ChatDelta::Text("ready to finalize".to_string()),
            ChatDelta::Done(FinishReason::Stop),
        ],
        vec![
            ChatDelta::ToolCall(tc("write_result", "forbidden_final_call", "{}")),
            ChatDelta::Done(FinishReason::ToolCalls),
        ],
    ]);
    let ran = Arc::new(AtomicUsize::new(0));
    let handler_runs = ran.clone();
    let mut tools = Registry::new();
    tools.register_with_descriptor(
        kitsu_providers::ToolDescriptor {
            name: "write_result".to_string(),
            description: "write a result".to_string(),
            parameters: serde_json::json!({"type": "object"}),
        },
        Box::new(move |_args: String| {
            let handler_runs = handler_runs.clone();
            Box::pin(async move {
                handler_runs.fetch_add(1, Ordering::SeqCst);
                Ok::<String, ToolError>(r#"{"changed":true}"#.to_string())
            })
        }),
    );
    let mut config = cfg(3);
    config.response_format = Some(JsonSchemaFormat {
        name: "kitsu_output".to_string(),
        schema: serde_json::json!({"type": "object"}),
        strict: true,
    });
    config.defer_response_format_until_final = true;

    let error = run_agent_turn(&provider, &tools, &mut initial_messages(), &config)
        .await
        .expect_err("an unoffered finalization tool call must fail closed");

    assert!(matches!(
        error,
        crate::orchestrator::AgentError::ToolCallDuringToolFreeFinalization
    ));
    assert_eq!(ran.load(Ordering::SeqCst), 0);
    assert!(provider.seen_tools.lock().unwrap()[1].is_none());
}

#[tokio::test]
async fn deferred_finalization_remeasures_the_changed_response_schema() {
    let provider = MockProvider::new(vec![vec![
        ChatDelta::Text("ready to finalize".to_string()),
        ChatDelta::Usage(kitsu_providers::Usage {
            prompt_tokens: 10,
            completion_tokens: 2,
            total_tokens: 12,
            prompt_tokens_details: None,
        }),
        ChatDelta::Done(FinishReason::Stop),
    ]]);
    let mut tools = Registry::new();
    tools.register_with_descriptor(
        kitsu_providers::ToolDescriptor {
            name: "write_result".to_string(),
            description: "write a result".to_string(),
            parameters: serde_json::json!({"type": "object"}),
        },
        ok_handler(r#"{"changed":true}"#),
    );
    let mut config = cfg(3);
    config.context_window = Some(3_000);
    config.response_format = Some(JsonSchemaFormat {
        name: "kitsu_output".to_string(),
        schema: serde_json::json!({
            "type": "object",
            "description": "x".repeat(12_000)
        }),
        strict: true,
    });
    config.defer_response_format_until_final = true;

    let error = run_agent_turn(&provider, &tools, &mut initial_messages(), &config)
        .await
        .expect_err("the changed final schema must be remeasured before provider contact");

    assert!(matches!(
        error,
        crate::orchestrator::AgentError::ContextOverflow { .. }
    ));
    assert_eq!(
        provider.call_count(),
        1,
        "the oversized final schema must be blocked before a second provider request"
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

    let mut state = AgentSessionState::default();
    let result = run_agent_turn_with_state(
        &provider,
        &tools,
        &mut initial_messages(),
        &cfg(3),
        &mut state,
    )
    .await;

    assert!(
        result.is_ok(),
        "hitting max_rounds must stop gracefully, not error: {result:?}"
    );
    assert_eq!(
        provider.call_count(),
        3,
        "the loop must cap at exactly max_rounds (3) provider calls and not hang"
    );
    assert_eq!(state.last_turn.completion, TurnCompletion::MaxRounds);
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

    let mut state = AgentSessionState::default();
    let out = run_agent_turn_with_state(
        &provider,
        &tools,
        &mut initial_messages(),
        &cfg(10),
        &mut state,
    )
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
    assert_eq!(state.last_turn.tool_errors, 1);
    assert_eq!(state.last_turn.policy_errors, 0);

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

#[tokio::test]
async fn test_turn_status_separates_policy_denials_from_other_tool_errors() {
    let provider = MockProvider::new(vec![
        vec![
            ChatDelta::ToolCall(tc("write", "call_denied", "{}")),
            ChatDelta::Done(FinishReason::ToolCalls),
        ],
        vec![
            ChatDelta::Text("continued safely".to_string()),
            ChatDelta::Done(FinishReason::Stop),
        ],
    ]);
    let mut tools = Registry::new();
    tools.register(
        "write",
        Box::new(|_| {
            Box::pin(async {
                Err::<String, ToolError>(ToolError::PolicyDenied(
                    "path is outside the write roots".to_string(),
                ))
            })
        }),
    );
    let mut state = AgentSessionState::default();

    assert_eq!(
        run_agent_turn_with_state(
            &provider,
            &tools,
            &mut initial_messages(),
            &cfg(4),
            &mut state,
        )
        .await
        .unwrap(),
        "continued safely"
    );
    assert_eq!(state.last_turn.tool_errors, 1);
    assert_eq!(state.last_turn.policy_errors, 1);
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

    let mut state = AgentSessionState::default();
    let out = run_agent_turn_with_state(
        &provider,
        &tools,
        &mut initial_messages(),
        &cfg(10),
        &mut state,
    )
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
    assert_eq!(state.last_turn.completion, TurnCompletion::LengthLimited);
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

/// Cycle-27: identical SUCCESSFUL calls stop re-executing past the limit;
/// the model gets a reminder result instead.
#[tokio::test]
async fn identical_successful_repeats_short_circuit_after_limit() {
    let call = |id: &str| {
        vec![
            ChatDelta::ToolCall(ToolCall {
                tool_name: "echo".to_string(),
                args: r#"{"x":1}"#.to_string(),
                call_id: id.to_string(),
            }),
            ChatDelta::Done(FinishReason::ToolCalls),
        ]
    };
    let provider = MockProvider::new(vec![
        call("c1"),
        call("c2"),
        call("c3"),
        call("c4"),
        call("c5"),
        vec![
            ChatDelta::Text("stopped".to_string()),
            ChatDelta::Done(FinishReason::Stop),
        ],
    ]);
    let ran = Arc::new(AtomicUsize::new(0));
    let ran2 = ran.clone();
    let mut tools = Registry::new();
    tools.register_with_descriptor(
        kitsu_providers::ToolDescriptor {
            name: "echo".to_string(),
            description: "echo".to_string(),
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
        .unwrap();
    assert!(out.contains("stopped"));
    assert_eq!(
        ran.load(Ordering::SeqCst),
        3,
        "calls 4 and 5 must be short-circuited after 3 identical successes"
    );
    let seen = provider.seen_messages.lock().unwrap();
    assert!(
        seen.last().unwrap().iter().any(|m| {
            m.role == kitsu_providers::Role::Tool && m.content.contains("identical call repeated")
        }),
        "the reminder result reaches the model"
    );
}

/// Receipt durations: milliseconds under a second, one-decimal seconds after.
#[test]
fn format_tool_elapsed_formats_ms_and_seconds() {
    use crate::orchestrator::format_tool_elapsed;
    use std::time::Duration;
    assert_eq!(format_tool_elapsed(Duration::from_millis(34)), "34ms");
    assert_eq!(format_tool_elapsed(Duration::from_millis(999)), "999ms");
    assert_eq!(format_tool_elapsed(Duration::from_millis(2100)), "2.1s");
    assert_eq!(format_tool_elapsed(Duration::from_secs(61)), "61.0s");
}

/// Invalid escapes inside string literals are dropped; valid ones survive.
#[test]
fn relax_json_escapes_repairs_only_invalid_escapes() {
    assert_eq!(
        super::relax_json_escapes(r#"{"a": "it\'s"}"#),
        r#"{"a": "it's"}"#
    );
    assert_eq!(
        super::relax_json_escapes(r#"{"a": "keep\n\t\"quoted\" \\ intact"}"#),
        r#"{"a": "keep\n\t\"quoted\" \\ intact"}"#
    );
}

/// A text-form call whose only fault is a non-standard escape (`\'`) is
/// repaired and executed instead of being dumped as the answer.
#[tokio::test]
async fn test_text_protocol_repairs_invalid_escape() {
    assert_text_protocol_executes(
        r#"{"name": "fs_write_file", "arguments": {"path": "x.py", "content": "print(\'hi\')"}}"#,
    )
    .await;
}

/// A tool attempt too broken to repair (structurally invalid JSON naming a
/// real tool) must not masquerade as the answer: the model gets corrective
/// feedback, retries, and the real answer from the next round is returned.
#[tokio::test]
async fn test_text_protocol_malformed_call_feeds_back_instead_of_answering() {
    let provider = MockProvider::new(vec![
        vec![
            // Balanced braces, names a registered tool, but missing a comma —
            // unparseable even after escape repair.
            ChatDelta::Text(r#"{"name": "fs_write_file" "arguments": {}}"#.to_string()),
            ChatDelta::Done(FinishReason::Stop),
        ],
        vec![
            ChatDelta::Text("recovered answer".to_string()),
            ChatDelta::Done(FinishReason::Stop),
        ],
    ]);
    let ran = Arc::new(AtomicUsize::new(0));
    let tools = writing_registry(ran.clone());

    let mut state = AgentSessionState::default();
    let out = run_agent_turn_with_state(
        &provider,
        &tools,
        &mut initial_messages(),
        &cfg(10),
        &mut state,
    )
    .await
    .expect("the retry round should produce the committed answer");

    assert_eq!(ran.load(Ordering::SeqCst), 0, "nothing was executed");
    assert_eq!(provider.call_count(), 2, "feedback round then answer round");
    assert!(out.contains("recovered answer"), "{out}");
    assert!(
        !out.contains("fs_write_file"),
        "raw tool JSON must not be the answer: {out}"
    );
    assert_eq!(
        state.last_turn.tool_errors, 1,
        "the ignored attempt is accounted as a tool error"
    );
    let seen = provider.seen_messages.lock().unwrap();
    assert!(
        seen.last().unwrap().iter().any(|m| {
            m.role == kitsu_providers::Role::User && m.content.contains("NOT executed")
        }),
        "round 2 must carry the corrective feedback"
    );
}

/// A skill invoked as a fake tool (`{"name": "code-review"}`) gets targeted
/// corrective feedback instead of the raw JSON committing as the answer;
/// arbitrary unknown names (no matching skill) keep the old return-as-text
/// behavior, so real answers containing JSON stay untouched.
#[tokio::test]
async fn test_text_protocol_skill_confusion_feeds_back_instead_of_answering() {
    let provider = MockProvider::new(vec![
        vec![
            ChatDelta::Text(r#"{"name": "code-review", "arguments": {"path": "src"}}"#.to_string()),
            ChatDelta::Done(FinishReason::Stop),
        ],
        vec![
            ChatDelta::Text("loaded properly".to_string()),
            ChatDelta::Done(FinishReason::Stop),
        ],
    ]);
    let ran = Arc::new(AtomicUsize::new(0));
    let tools = writing_registry(ran.clone());
    let mut config = cfg(10);
    config.skill_names = vec!["code-review".to_string()];

    let mut state = AgentSessionState::default();
    let out = run_agent_turn_with_state(
        &provider,
        &tools,
        &mut initial_messages(),
        &config,
        &mut state,
    )
    .await
    .expect("retry round should produce the committed answer");

    assert_eq!(ran.load(Ordering::SeqCst), 0);
    assert_eq!(provider.call_count(), 2, "feedback round then answer round");
    assert!(out.contains("loaded properly"), "{out}");
    assert_eq!(state.last_turn.tool_errors, 1);
    let seen = provider.seen_messages.lock().unwrap();
    assert!(
        seen.last().unwrap().iter().any(|m| {
            m.role == kitsu_providers::Role::User
                && m.content.contains("SKILL, not a tool")
                && m.content.contains("`skill` tool")
        }),
        "round 2 must carry the skill-specific correction"
    );
}

/// Shared run tallies count reads, failures, and denials — including from
/// any config that carries the same Arc (subagents share it).
#[tokio::test]
async fn run_tallies_count_reads_failures_and_denials() {
    use crate::orchestrator::RunTallies;
    let provider = MockProvider::new(vec![
        vec![
            ChatDelta::ToolCall(ToolCall {
                tool_name: "denied_tool".to_string(),
                args: "{}".to_string(),
                call_id: "c1".to_string(),
            }),
            ChatDelta::Done(FinishReason::ToolCalls),
        ],
        vec![
            ChatDelta::Text("done".to_string()),
            ChatDelta::Done(FinishReason::Stop),
        ],
    ]);
    let mut tools = Registry::new();
    tools.register_with_descriptor(
        kitsu_providers::ToolDescriptor {
            name: "denied_tool".to_string(),
            description: "always denied".to_string(),
            parameters: serde_json::json!({"type": "object"}),
        },
        Box::new(|_args: String| {
            Box::pin(async move {
                Err::<String, ToolError>(ToolError::PolicyDenied("nope".to_string()))
            })
        }),
    );
    let tallies = std::sync::Arc::new(RunTallies::default());
    let mut config = cfg(10);
    config.run_tallies = Some(tallies.clone());

    let _ = run_agent_turn(&provider, &tools, &mut initial_messages(), &config).await;

    use std::sync::atomic::Ordering;
    assert_eq!(tallies.failed.load(Ordering::Acquire), 1);
    assert_eq!(tallies.denied.load(Ordering::Acquire), 1);
    assert_eq!(tallies.reads.load(Ordering::Acquire), 0);
}

/// Cycle-23 audit: after SPAWN_REFUSAL_LIMIT cap refusals the spawn tools
/// are withheld for the rest of the turn — later rounds advertise fewer
/// tools, and the model gets an explicit withdrawal notice.
#[tokio::test]
async fn spawn_cap_refusals_withhold_spawn_tools_for_the_turn() {
    use crate::orchestrator::{AgentEventLog, AgentRuntimeEvent};
    // Distinct args per attempt: a real storm varies its tasks, which is
    // exactly why the identical-signature loop-breaker never catches it.
    let spam = |id: &str, task: &str| {
        vec![
            ChatDelta::ToolCall(ToolCall {
                tool_name: "spawn_agent".to_string(),
                args: format!(r#"{{"task":"{task}"}}"#),
                call_id: id.to_string(),
            }),
            ChatDelta::Done(FinishReason::ToolCalls),
        ]
    };
    let provider = MockProvider::new(vec![
        spam("s1", "first"),
        spam("s2", "second"),
        vec![
            ChatDelta::Text("done inline".to_string()),
            ChatDelta::Done(FinishReason::Stop),
        ],
    ]);
    let mut tools = Registry::new();
    tools.register_with_descriptor(
        kitsu_providers::ToolDescriptor {
            name: "spawn_agent".to_string(),
            description: "spawn".to_string(),
            parameters: serde_json::json!({"type": "object"}),
        },
        Box::new(|_args: String| {
            Box::pin(async move {
                Err::<String, ToolError>(ToolError::ExecError(
                    "agent spawn cap reached: this turn already started 0 of 0 allowed subagents."
                        .to_string(),
                ))
            })
        }),
    );
    tools.register_with_descriptor(
        kitsu_providers::ToolDescriptor {
            name: "other_tool".to_string(),
            description: "still available".to_string(),
            parameters: serde_json::json!({"type": "object"}),
        },
        ok_handler("{}"),
    );
    let log = AgentEventLog::default();
    let mut config = cfg(10);
    config.quiet = true;
    config.event_log = Some(log.clone());

    let out = run_agent_turn(&provider, &tools, &mut initial_messages(), &config)
        .await
        .expect("turn completes inline after withdrawal");
    assert!(out.contains("done inline"));

    let offered: Vec<usize> = log
        .take()
        .iter()
        .filter_map(|event| match event {
            AgentRuntimeEvent::ProviderRequest { tools_offered, .. } => Some(*tools_offered),
            _ => None,
        })
        .collect();
    assert_eq!(
        offered,
        vec![2, 2, 1],
        "third round must advertise only the non-spawn tool: {offered:?}"
    );
    let seen = provider.seen_messages.lock().unwrap();
    assert!(
        seen.last()
            .unwrap()
            .iter()
            .any(|m| { m.role == kitsu_providers::Role::User && m.content.contains("WITHDRAWN") }),
        "the model gets the withdrawal notice"
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

    // Three-segment paths preserve crate identity (cycle-15 G5).
    assert_eq!(short_path("/x/y/deck1.md"), "/x/y/deck1.md");
    // Paths with 4+ segments show the last three with a leading `…`.
    assert_eq!(short_path("/a/b/c/d/e.rs"), "…/c/d/e.rs");
    assert_eq!(
        short_path("crates/kitsu-render/src/lib.rs"),
        "…/kitsu-render/src/lib.rs"
    );
    // Two-segment paths stay exact (relative or absolute).
    assert_eq!(short_path("a/b"), "a/b");
    assert_eq!(short_path("deck.md"), "deck.md");
    // Short absolute paths keep their leading slash: `/etc/hostname` must not
    // display as the relative-looking `etc/hostname`.
    assert_eq!(short_path("/etc/hostname"), "/etc/hostname");
    assert_eq!(short_path("/etc"), "/etc");

    assert_eq!(
        format_tool_header("fs_read_file", r#"{"path":"/x/y/deck1.md"}"#),
        "Read(/x/y/deck1.md)"
    );
    assert_eq!(
        format_tool_header("fs_write_file", r#"{"path":"out/fb.py","content":"x"}"#),
        "Write(out/fb.py)"
    );
    assert_eq!(
        format_tool_header("repo_grep", r#"{"pattern":"TODO","path":"/a/src"}"#),
        "Grep(\"TODO\", /a/src)"
    );
    // Unknown tool / unparseable args fall back to the raw name.
    assert_eq!(format_tool_header("weird_tool", "not json"), "weird_tool");

    // A dispatch receipt must name the worker and its task; without an arm the
    // agent tools have no `path` and render as a bare tool name.
    assert_eq!(
        format_tool_header("spawn_agent_bg", r#"{"task":"review parser.rs"}"#),
        "Agent(\"review parser.rs\")"
    );
    assert_eq!(
        format_tool_header(
            "spawn_agent_bg",
            r#"{"task":"review parser.rs","profile":"reviewer"}"#
        ),
        "Agent(reviewer: \"review parser.rs\")"
    );
    assert_eq!(
        format_tool_header("spawn_agents", r#"{"tasks":["one","two"]}"#),
        "Agent(x2: \"one\", \"two\")"
    );
    assert_eq!(
        format_tool_header("agent_result", r#"{"id":3}"#),
        "AgentResult(#3)"
    );
    assert_eq!(
        format_tool_result(
            "spawn_agent_bg",
            "{}",
            "Started background agent #1. It runs detached.",
            false
        ),
        "Started background agent #1. It runs detached."
    );

    assert_eq!(
        format_tool_result(
            "fs_write_file",
            "{}",
            r#"{"written":"x","bytes":42}"#,
            false
        ),
        "written · 42 bytes"
    );
    assert_eq!(
        format_tool_result(
            "fs_write_file",
            "{}",
            r#"{"written":"x","bytes":42,"change":"unchanged"}"#,
            false
        ),
        "unchanged · 42 bytes"
    );
    // A content-quality warning (garbage/corrupted write) surfaces inline so a
    // human watching the transcript sees it, not just the model.
    assert_eq!(
        format_tool_result(
            "fs_write_file",
            "{}",
            r#"{"written":"x","bytes":42,"change":"created","warning":"looks corrupted"}"#,
            false
        ),
        "created · 42 bytes — warning: looks corrupted"
    );
    assert_eq!(
        format_tool_header("fs_edit_file", r#"{"path":"fb.py"}"#),
        "Edit(fb.py)"
    );
    assert_eq!(
        format_tool_result(
            "fs_edit_file",
            "{}",
            r#"{"edited":"x","bytes":10,"change":"updated"}"#,
            false
        ),
        "updated · 10 bytes"
    );
    assert_eq!(
        format_tool_result("repo_grep", "{}", "l1\nl2\nl3", false),
        "3 matches"
    );
    assert_eq!(
        format_tool_result("repo_grep", "{}", "(no matches)", false),
        "no matches"
    );
    assert_eq!(
        format_tool_result("fs_read_file", "{}", "a\nb", false),
        "2 lines"
    );
    // A ranged read discloses its slice (cycle-16: silent 4-line reads of a
    // 1905-line file were invisible starvation).
    assert_eq!(
        format_tool_result(
            "fs_read_file",
            r#"{"path":"x.rs","start_line":1,"end_line":4}"#,
            "a\nb\nc\nd",
            false
        ),
        "4 lines (ranged read 1-4)"
    );
    assert_eq!(
        format_tool_result(
            "fs_read_file",
            r#"{"path":"x.rs","start_line":100}"#,
            "a\nb",
            false
        ),
        "2 lines (ranged read 100-end)"
    );
    assert_eq!(
        format_tool_result(
            "fs_read_file",
            "{}",
            r#"{"error":"PolicyDenied","detail":"nope"}"#,
            true
        ),
        "error: PolicyDenied — nope"
    );

    assert_eq!(
        format_tool_header(
            "command_exec",
            r#"{"argv":["git","status"],"cwd":"/workspace/project","timeout_ms":5000}"#
        ),
        r#"Command(argv="git" "status"; cwd="/workspace/project")"#
    );
    let command_output = serde_json::json!({
            "argv": ["git", "status"],
            "cwd": "/workspace/project",
            "executable": {
                "invocation": "git",
                "resolution": "ambient_path",
                "observed_path": "/usr/bin/git",
                "pinned": false
            },
            "ambient_authority": {
                "inherits_environment": true,
                "filesystem_confined_to_command_roots": false,
                "network_confined": false,
                "executable_identity_pinned": false
            },
            "termination": "exited",
            "success": false,
            "exit_code": 7,
            "signal": null,
            "stdout": "12\n[... 5 bytes omitted ...]\n89",
            "stderr": "bad",
            "stdout_bytes": 9,
            "stderr_bytes": 3,
            "stdout_retained_bytes": 4,
            "stderr_retained_bytes": 3,
            "stdout_truncated": true,
            "stderr_truncated": false,
            "stdout_capture_complete": true,
            "stderr_capture_complete": true,
            "elapsed_ms": 42,
            "artifact": {"reference": "session:command-1"},
            "artifact_error": null
    });
    let command_result =
        format_tool_result("command_exec", "{}", &command_output.to_string(), false);
    assert!(command_result.contains("exit 7 | 42 ms"));
    assert!(command_result.contains("stdout 4/9 B truncated"));
    assert!(command_result.contains("89"));
    assert!(command_result.contains("stderr 3 B"));
    assert!(command_result.contains("bad"));
    assert!(command_result.contains("full output=\"session:command-1\""));

    let mut cancelled_without_artifact = command_output.clone();
    cancelled_without_artifact["termination"] = serde_json::json!("cancelled");
    cancelled_without_artifact["artifact"] = serde_json::Value::Null;
    cancelled_without_artifact["artifact_error"] =
        serde_json::json!("no full-output artifact was created; no cleanup was required");
    let cancelled_result = format_tool_result(
        "command_exec",
        "{}",
        &cancelled_without_artifact.to_string(),
        false,
    );
    assert!(cancelled_result.contains("artifact status="));
    assert!(!cancelled_result.contains("artifact error="));

    let mut signaled = command_output.clone();
    signaled["termination"] = serde_json::json!("signaled");
    signaled["exit_code"] = serde_json::Value::Null;
    signaled["signal"] = serde_json::json!(15);
    assert!(
        format_tool_result("command_exec", "{}", &signaled.to_string(), false)
            .starts_with("signal 15 | 42 ms")
    );

    let mut timed_out = command_output.clone();
    timed_out["termination"] = serde_json::json!("timed_out");
    timed_out["exit_code"] = serde_json::Value::Null;
    timed_out["signal"] = serde_json::json!(9);
    assert!(
        format_tool_result("command_exec", "{}", &timed_out.to_string(), false)
            .starts_with("timeout (signal 9) | 42 ms")
    );

    let unsafe_header = format_tool_header(
        "command_exec",
        &serde_json::json!({
            "argv": ["printf", "\u{1b}[31mred\nnext"],
            "cwd": "/workspace\u{1b}]0;owned\u{7}",
        })
        .to_string(),
    );
    assert!(!unsafe_header.contains('\u{1b}'));
    assert!(!unsafe_header.contains('\n'));
    assert!(unsafe_header.contains("\\n"));

    let sanitized_warning = format_tool_result(
        "fs_write_file",
        "{}",
        r#"{"bytes":1,"warning":"raw <|im_start|> marker"}"#,
        false,
    );
    assert!(!sanitized_warning.contains("<|im_start|>"));
    assert!(sanitized_warning.contains("[chat-template marker]"));
}

/// Cycle-10 G5: the `full output=` artifact path is the one pointer to the
/// complete command output. It must never be elided — a real profile-scoped
/// artifact path routinely exceeds the old 160-char cap and was cut
/// mid-filename — and it stays the receipt's final field so a wrapped line
/// ends with a copyable path. Cycle-10 G2: a containment hint, when present,
/// is rendered as its own bounded receipt field.
#[test]
fn command_receipt_never_elides_the_artifact_path_and_renders_hints() {
    use crate::orchestrator::format_tool_result;

    let long_reference = format!(
        "/home/user/.kitsu/profiles/v1-fnv128-{}/sessions/s-1786459292594577016-2717952-0.artifacts/runtime-2718000-1786459292594577016-1/command-output-1.txt",
        "a".repeat(32)
    );
    assert!(long_reference.chars().count() > 160);
    let output = serde_json::json!({
        "argv": ["python3", "-m", "pytest", "-q"],
        "cwd": "/workspace/project",
        "executable": {
            "invocation": "python3",
            "resolution": "ambient_path",
            "observed_path": "/usr/bin/python3",
            "pinned": false
        },
        "ambient_authority": {
            "inherits_environment": false,
            "filesystem_confined_to_command_roots": true,
            "network_confined": true,
            "executable_identity_pinned": true
        },
        "termination": "exited",
        "success": false,
        "exit_code": 1,
        "signal": null,
        "stdout": "",
        "stderr": "/usr/bin/python3: No module named pytest",
        "stdout_bytes": 0,
        "stderr_bytes": 40,
        "stdout_retained_bytes": 0,
        "stderr_retained_bytes": 40,
        "stdout_truncated": false,
        "stderr_truncated": false,
        "stdout_capture_complete": true,
        "stderr_capture_complete": true,
        "elapsed_ms": 23,
        "artifact": {"reference": long_reference},
        "artifact_error": null,
        "containment_hint": "output matches 'No module named' — try --command-env / --command-root"
    });
    let receipt = format_tool_result("command_exec", "{}", &output.to_string(), false);
    assert!(
        receipt.contains(&format!("full output={long_reference:?}")),
        "artifact path must appear complete and unelided: {receipt}"
    );
    assert!(
        receipt.trim_end_matches('"').ends_with(&long_reference),
        "artifact path must be the receipt's final field: {receipt}"
    );
    assert!(
        receipt.contains("hint: output matches 'No module named'"),
        "containment hint must be rendered: {receipt}"
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
    .await
    .unwrap();
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
    .await
    .unwrap();
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
    cfg.context_window = Some(1_200);
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
    .await
    .unwrap();
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
fn artifact_free_inline_bound_is_utf8_safe_and_byte_exact() {
    use crate::orchestrator::bound_inline_output;

    let original = format!("HEAD{}TAIL", "🦀".repeat(3_000));
    let bounded = bound_inline_output(original.clone());

    assert!(bounded.truncated);
    assert!(bounded.content.len() <= 8 * 1024);
    assert_eq!(bounded.original_bytes, original.len() as u64);
    assert_eq!(
        bounded.original_bytes,
        bounded.retained_bytes.saturating_add(bounded.omitted_bytes)
    );
    assert!(bounded.retained_bytes <= 8 * 1024);
    assert!(bounded.content.starts_with("HEAD"));
    assert!(bounded.content.ends_with("TAIL"));
    assert!(bounded
        .content
        .contains("bytes omitted by artifact-free policy"));
    assert!(bounded
        .content
        .contains(&format!("original {} bytes", original.len())));
}

#[tokio::test]
async fn artifact_free_large_tool_result_is_bounded_before_next_request() {
    use crate::orchestrator::{
        AgentEventLog, AgentRuntimeEvent, ArtifactStoragePolicy, ToolArtifactStatus,
    };

    let provider = MockProvider::new(vec![
        vec![
            ChatDelta::ToolCall(tc("read", "large-result", "{}")),
            ChatDelta::Done(FinishReason::ToolCalls),
        ],
        vec![
            ChatDelta::Text("done".to_string()),
            ChatDelta::Done(FinishReason::Stop),
        ],
    ]);
    let result = format!(
        "HEAD{}CENTER_SENTINEL{}TAIL",
        "a".repeat(5_000),
        "b".repeat(5_000)
    );
    let original_bytes = result.len() as u64;
    let mut tools = Registry::new();
    tools.register(
        "read",
        Box::new(move |_args: String| {
            let result = result.clone();
            Box::pin(async move { Ok::<String, ToolError>(result) })
        }),
    );
    let log = AgentEventLog::default();
    let mut config = cfg(2);
    config.artifact_policy = ArtifactStoragePolicy::InlineOnly;
    config.event_log = Some(log.clone());
    config.quiet = true;

    let answer = run_agent_turn(&provider, &tools, &mut initial_messages(), &config)
        .await
        .unwrap();
    assert_eq!(answer, "done");

    let requests = provider.seen_messages.lock().unwrap();
    let tool_result = requests[1]
        .iter()
        .find(|message| message.role == kitsu_providers::Role::Tool)
        .expect("second request includes the tool result");
    assert!(tool_result.content.starts_with("HEAD"));
    assert!(tool_result.content.ends_with("TAIL"));
    assert!(tool_result
        .content
        .contains("bytes omitted by artifact-free policy"));
    assert!(!tool_result.content.contains("CENTER_SENTINEL"));
    drop(requests);

    assert!(log.take().iter().any(|event| matches!(
        event,
        AgentRuntimeEvent::ToolFinished {
            call_id,
            output_bytes,
            output_retained_bytes,
            output_truncated: true,
            artifact_status: ToolArtifactStatus::SuppressedByPolicy,
            ..
        } if call_id == "large-result"
            && *output_bytes == original_bytes
            && *output_retained_bytes <= 8 * 1024
    )));
}

#[test]
fn maybe_offload_writes_large_result_and_returns_pointer() {
    use crate::orchestrator::maybe_offload;
    let dir = tempfile::tempdir().unwrap();
    let big = "x".repeat(20_000);
    let (out, offloaded_to) = maybe_offload(big.clone(), "call-1", Some(dir.path()));
    assert!(out.contains("saved to"), "returns a pointer");
    assert!(offloaded_to.is_some(), "reports the artifact path");
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
fn maybe_offload_never_clobbers_an_earlier_result_for_the_same_call() {
    use crate::orchestrator::maybe_offload;
    let dir = tempfile::tempdir().unwrap();
    let first = "a".repeat(20_000);
    let second = "b".repeat(20_000);

    let (first_pointer, _) = maybe_offload(first.clone(), "same-call", Some(dir.path()));
    let (second_pointer, _) = maybe_offload(second.clone(), "same-call", Some(dir.path()));

    assert_ne!(first_pointer, second_pointer);
    let mut contents = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|entry| std::fs::read_to_string(entry.unwrap().path()).unwrap())
        .collect::<Vec<_>>();
    contents.sort();
    assert_eq!(contents, vec![first, second]);
}

#[test]
fn maybe_offload_keeps_small_results_inline() {
    use crate::orchestrator::maybe_offload;
    let dir = tempfile::tempdir().unwrap();
    let small = "hello".to_string();
    assert_eq!(
        maybe_offload(small.clone(), "c", Some(dir.path())),
        (small, None)
    );
}

#[test]
fn maybe_offload_is_noop_without_a_dir() {
    use crate::orchestrator::maybe_offload;
    let big = "x".repeat(20_000);
    assert_eq!(maybe_offload(big.clone(), "c", None), (big, None));
}

/// The cycle-15 headline regression: with a huge window and a nearly empty
/// conversation, a big successful result must STAY INLINE — swapping it for
/// a 500-char preview starves the model for no reason. Pressure returns once
/// the estimated prompt would pass half the effective budget.
#[test]
fn offload_pressure_requires_real_window_pressure() {
    use crate::orchestrator::offload_pressure;
    let messages = initial_messages();
    let big = "x".repeat(20_000); // ~5k estimated tokens

    let mut roomy = cfg(10);
    roomy.context_window = Some(131_072);
    assert!(
        !offload_pressure(&messages, &big, &roomy, None),
        "1.5k used + 5k added in a 131k window is not pressure"
    );

    let mut tight = cfg(10);
    tight.context_window = Some(8_192);
    assert!(
        offload_pressure(&messages, &big, &tight, None),
        "the same result in an 8k window is pressure"
    );

    let mut unknown = cfg(10);
    unknown.context_window = None;
    assert!(
        offload_pressure(&messages, &big, &unknown, None),
        "unknown window keeps the conservative old behavior"
    );
}

#[test]
fn artifact_policy_and_context_pressure_select_one_retention_path() {
    use crate::orchestrator::{prepare_tool_output, ArtifactStoragePolicy, ToolArtifactStatus};
    use kitsu_providers::ToolDescriptor;

    let dir = tempfile::tempdir().unwrap();
    let content = format!("HEAD{}TAIL", "x".repeat(20_000));
    let messages = initial_messages();
    let mut tools = Registry::new();
    tools.register_with_descriptor(
        ToolDescriptor {
            name: "fs_read_file".to_string(),
            description: "read a file".to_string(),
            parameters: serde_json::json!({"type": "object"}),
        },
        ok_handler("{}"),
    );

    let mut roomy = cfg(10);
    roomy.context_window = Some(131_072);
    roomy.offload_dir = Some(dir.path().to_path_buf());
    let inline = prepare_tool_output(
        content.clone(),
        "roomy",
        "repo_grep",
        false,
        &tools,
        &messages,
        &roomy,
        None,
    );
    assert_eq!(inline.content, content);
    assert_eq!(
        inline.retention.artifact_status,
        ToolArtifactStatus::NotNeeded
    );
    assert!(inline.artifact_path.is_none());
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);

    let mut tight = cfg(10);
    tight.context_window = Some(8_192);
    tight.offload_dir = Some(dir.path().to_path_buf());
    let retained = prepare_tool_output(
        content.clone(),
        "tight",
        "repo_grep",
        false,
        &tools,
        &messages,
        &tight,
        None,
    );
    assert_eq!(
        retained.retention.artifact_status,
        ToolArtifactStatus::Retained
    );
    assert!(retained.content.contains("full result saved to"));
    assert!(retained
        .artifact_path
        .as_ref()
        .is_some_and(|path| path.is_file()));
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);

    tight.artifact_policy = ArtifactStoragePolicy::InlineOnly;
    let suppressed = prepare_tool_output(
        content,
        "suppressed",
        "repo_grep",
        false,
        &tools,
        &messages,
        &tight,
        None,
    );
    assert_eq!(
        suppressed.retention.artifact_status,
        ToolArtifactStatus::SuppressedByPolicy
    );
    assert!(suppressed.retention.output_truncated);
    assert!(suppressed
        .content
        .contains("bytes omitted by artifact-free policy"));
    assert!(suppressed.artifact_path.is_none());
    assert_eq!(
        std::fs::read_dir(dir.path()).unwrap().count(),
        1,
        "artifact-free retention must not create a second file"
    );
}

#[test]
fn command_only_tools_keep_large_results_inline_until_read_tool_is_available() {
    use crate::orchestrator::maybe_offload_for_tools;
    use kitsu_providers::ToolDescriptor;

    let dir = tempfile::tempdir().unwrap();
    let big = "x".repeat(20_000);
    let mut tools = Registry::new();
    tools.register_with_descriptor(
        ToolDescriptor {
            name: "command_exec".to_string(),
            description: "run a command".to_string(),
            parameters: serde_json::json!({"type": "object"}),
        },
        ok_handler("{}"),
    );

    let mut tight = cfg(10);
    tight.context_window = Some(8_192);
    let msgs = initial_messages();
    let inline = maybe_offload_for_tools(
        big.clone(),
        "command",
        Some(dir.path()),
        &tools,
        &msgs,
        &tight,
        None,
    );
    assert_eq!(inline, (big.clone(), None));
    assert!(
        std::fs::read_dir(dir.path()).unwrap().next().is_none(),
        "a command-only model cannot recover an fs_read_file pointer"
    );

    tools.register_with_descriptor(
        ToolDescriptor {
            name: "fs_read_file".to_string(),
            description: "read a file".to_string(),
            parameters: serde_json::json!({"type": "object"}),
        },
        ok_handler("{}"),
    );
    let (pointer, offloaded_to) = maybe_offload_for_tools(
        big,
        "readable",
        Some(dir.path()),
        &tools,
        &msgs,
        &tight,
        None,
    );
    assert!(pointer.contains("full result saved to"));
    assert!(pointer.contains("fs_read_file"));
    assert!(offloaded_to.is_some());
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
    cfg.context_window = Some(1_200);
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
    .await
    .unwrap();

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
    cfg.context_window = Some(1_200);
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
    .await
    .unwrap();

    assert_eq!(failures, 1, "empty summary trips the breaker once");
    assert!(msgs.len() < before, "still shrank by dropping");
    assert!(
        msgs.iter().any(|m| m.content.contains("trimmed to fit")),
        "drop marker present (fell back to drop)"
    );
}

#[tokio::test]
async fn partial_summary_before_stream_error_is_never_installed() {
    use crate::orchestrator::{
        compact_context, CompactionMode, CompactionOutcome, SummaryFallbackReason,
    };
    use kitsu_providers::ChatMessage;

    let provider = FallibleMockProvider::new(vec![
        Ok(ChatDelta::Text("UNSAFE PARTIAL SUMMARY".to_string())),
        Err(ProviderError::Connection("stream interrupted".to_string())),
    ]);
    let tools = Registry::new();
    let mut c = cfg(10);
    c.context_window = Some(8_192);
    c.max_tokens = Some(64);
    c.compaction = CompactionMode::Summarize;
    let mut messages = vec![ChatMessage::system("sys")];
    for k in 0..4 {
        messages.push(ChatMessage::user(format!("q{k}")));
        messages.push(ChatMessage::assistant(format!("a{k}"), vec![]));
    }
    let mut state = AgentSessionState::default();

    let report = compact_context(&provider, &tools, &mut messages, &c, &mut state)
        .await
        .unwrap();
    let record = report.record.unwrap();
    assert_eq!(record.outcome, CompactionOutcome::SummaryFallbackTrim);
    assert_eq!(
        record.fallback_reason,
        Some(SummaryFallbackReason::ProviderFailure)
    );
    assert!(!messages
        .iter()
        .any(|message| message.content.contains("UNSAFE PARTIAL SUMMARY")));
    assert_eq!(messages[0].content, "sys");
    assert!(messages.iter().any(|message| message.content == "q3"));
}

#[tokio::test]
async fn truncated_or_unterminated_summary_streams_use_truthful_fallback() {
    use crate::orchestrator::{
        compact_context, CompactionMode, CompactionOutcome, SummaryFallbackReason,
    };
    use kitsu_providers::ChatMessage;

    let cases = [
        (
            "length",
            vec![
                ChatDelta::Text("PARTIAL LENGTH SUMMARY".to_string()),
                ChatDelta::Done(FinishReason::Length),
            ],
        ),
        (
            "bare EOF",
            vec![ChatDelta::Text("PARTIAL EOF SUMMARY".to_string())],
        ),
    ];
    for (case, script) in cases {
        let provider = MockProvider::new(vec![script]);
        let tools = Registry::new();
        let mut c = cfg(10);
        c.context_window = Some(8_192);
        c.max_tokens = Some(64);
        c.compaction = CompactionMode::Summarize;
        let mut messages = vec![ChatMessage::system("sys")];
        for k in 0..4 {
            messages.push(ChatMessage::user(format!("q{k}")));
            messages.push(ChatMessage::assistant(format!("a{k}"), vec![]));
        }
        let mut state = AgentSessionState::default();

        let report = compact_context(&provider, &tools, &mut messages, &c, &mut state)
            .await
            .unwrap();
        let record = report.record.unwrap();
        assert_eq!(
            record.outcome,
            CompactionOutcome::SummaryFallbackTrim,
            "{case}"
        );
        assert_eq!(
            record.fallback_reason,
            Some(SummaryFallbackReason::IncompleteResponse),
            "{case}"
        );
        assert!(
            !messages
                .iter()
                .any(|message| message.content.contains("PARTIAL")),
            "{case}: partial text must not become durable history"
        );
        assert_eq!(messages[0].content, "sys", "{case}");
        assert!(
            messages.iter().any(|message| message.content == "q3"),
            "{case}: protected recent context survives"
        );
    }
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
    let (pointer, _) = maybe_offload(big, "call-1", Some(dir.path()));
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
    use crate::orchestrator::{enforce_context_budget, AgentError, ContextUnit};
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
    let err = enforce_context_budget(
        &provider,
        &mut msgs,
        &cfg,
        &schema_json,
        true,
        &mut meter,
        &mut turn_usage,
        &mut failures,
    )
    .await
    .unwrap_err();

    assert!(
        msgs[2].content.contains("elided"),
        "schema overhead must count toward the budget even though the conversation alone doesn't"
    );
    assert!(matches!(
        err,
        AgentError::ContextOverflow {
            unit: ContextUnit::Tokens,
            ..
        }
    ));
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
    meter.record(300, &msgs, true);

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
    .await
    .unwrap();

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
    use crate::orchestrator::{
        enforce_context_budget, AgentEventLog, AgentRuntimeEvent, CompactionMode,
        ProviderRequestPurpose,
    };
    use kitsu_providers::{ChatMessage, Usage};

    let provider = SummaryTelemetryProvider;
    let mut cfg = cfg(10);
    cfg.context_window = Some(1_200);
    cfg.max_tokens = Some(64);
    cfg.compaction = CompactionMode::Summarize;
    let log = AgentEventLog::default();
    cfg.event_log = Some(log.clone());
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
    .await
    .unwrap();

    assert_eq!(
        turn_usage.prompt_tokens, 111,
        "the summarizer call's real usage must be folded into the turn's usage"
    );
    assert_eq!(turn_usage.completion_tokens, 22);

    let events = log.take();
    assert!(events.iter().any(|event| matches!(
        event,
        AgentRuntimeEvent::ProviderRequest {
            purpose: ProviderRequestPurpose::Compaction,
            model: Some(model),
            tools_offered: 0,
            ..
        } if model == "example-model-v1"
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        AgentRuntimeEvent::ProviderAttempt {
            purpose: ProviderRequestPurpose::Compaction,
            terminal: true,
            ..
        }
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        AgentRuntimeEvent::Usage {
            purpose: ProviderRequestPurpose::Compaction,
            total_tokens: 133,
            ..
        }
    )));
    assert!(!events
        .iter()
        .any(|event| matches!(event, AgentRuntimeEvent::AssistantDelta { .. })));
}

/// Regression: when there's nothing safely droppable (too few exchanges),
/// `CompactionMode::Trim` must not discard a still-valid anchor. Invalidating
/// unconditionally would force every later estimate back to a full
/// re-tokenize for no reason — exactly in the large, over-budget conversation
/// case where the anchor's cheap anchor+tail path matters most.
#[tokio::test]
async fn enforce_budget_trim_keeps_a_valid_anchor_when_nothing_is_droppable() {
    use crate::context::ContextMeter;
    use crate::orchestrator::{enforce_context_budget, AgentError};
    use kitsu_providers::ChatMessage;

    let provider = MockProvider::new(vec![]);
    let mut cfg = cfg(10); // compaction defaults to Trim
    cfg.context_window = Some(200); // target ~80 (see enforce_budget_token_path test)
    cfg.max_tokens = Some(64);

    // Only ONE user exchange (<= KEEP_RECENT_EXCHANGES) — nothing is safely
    // droppable, so drop_old_exchanges is a guaranteed no-op on `messages`.
    //
    // P5 (context.rs): estimate() now sanity-checks a recorded anchor against
    // the anchor-free full estimate and ignores it outside a 0.4x-2.5x band.
    // The old anchor here (500 against a ~8-token full estimate for "sys"/"q")
    // is far outside that band and would now be rejected, silently defeating
    // the overflow this test forces. Padded content raises the full estimate
    // to 41 so a still-over-target anchor of 90 stays plausible (90 <= 2.5*41
    // = 102.5, 90 >= 0.4*41 = 16.4) while remaining well above the ~80 target.
    let mut msgs = vec![
        ChatMessage::system("x".repeat(136).as_str()),
        ChatMessage::user("q"),
    ];
    let mut meter = ContextMeter::new();
    // A real, plausible anchor that's over target, forcing the compaction
    // match to run.
    meter.record(90, &msgs, true);

    let mut failures = 0u32;
    let mut turn_usage = kitsu_providers::Usage::default();
    let err = enforce_context_budget(
        &provider,
        &mut msgs,
        &cfg,
        "",
        false,
        &mut meter,
        &mut turn_usage,
        &mut failures,
    )
    .await
    .unwrap_err();

    assert!(matches!(err, AgentError::ContextOverflow { .. }));

    assert!(
        meter.anchored(&msgs),
        "a guaranteed-no-op Trim pass must not discard a still-valid anchor"
    );
}

// --- checked context invariant + manual compaction --------------------------

#[tokio::test]
async fn protected_token_overflow_fails_locally_without_calling_provider() {
    use crate::orchestrator::{AgentError, ContextUnit};
    use kitsu_providers::ChatMessage;

    let provider = MockProvider::new(vec![vec![
        ChatDelta::Text("must not be reached".to_string()),
        ChatDelta::Done(FinishReason::Stop),
    ]]);
    let tools = Registry::new();
    let mut c = cfg(1);
    c.context_window = Some(200);
    c.max_tokens = Some(64); // target = 80
    let mut messages = vec![
        ChatMessage::system("s".repeat(1_000)),
        ChatMessage::user("latest request"),
    ];

    let err = run_agent_turn(&provider, &tools, &mut messages, &c)
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        AgentError::ContextOverflow {
            required,
            target: 80,
            unit: ContextUnit::Tokens,
            window: Some(200),
            effective: Some(100),
        } if required > 80
    ));
    assert_eq!(
        provider.call_count(),
        0,
        "an unshrinkable protected prompt must never cross the provider boundary"
    );
}

#[tokio::test]
async fn protected_byte_overflow_also_fails_before_provider_when_window_unknown() {
    use crate::orchestrator::{AgentError, ContextUnit};
    use kitsu_providers::ChatMessage;

    let provider = MockProvider::new(vec![vec![ChatDelta::Done(FinishReason::Stop)]]);
    let tools = Registry::new();
    let mut c = cfg(1);
    c.context_window = None;
    c.max_context_bytes = 32;
    let mut messages = vec![
        ChatMessage::system("s".repeat(100)),
        ChatMessage::user("latest"),
    ];

    let err = run_agent_turn(&provider, &tools, &mut messages, &c)
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        AgentError::ContextOverflow {
            unit: ContextUnit::Bytes,
            window: None,
            ..
        }
    ));
    assert_eq!(provider.call_count(), 0);
}

#[tokio::test]
async fn structured_output_schema_is_budgeted_before_provider_contact() {
    use crate::orchestrator::{AgentError, ContextUnit};
    use kitsu_providers::ChatMessage;

    let provider = MockProvider::new(vec![vec![ChatDelta::Done(FinishReason::Stop)]]);
    let tools = Registry::new();
    let mut config = cfg(1);
    config.context_window = None;
    config.max_context_bytes = 128;
    config.response_format = Some(JsonSchemaFormat {
        name: "kitsu_output".to_string(),
        strict: true,
        schema: serde_json::json!({
            "type": "object",
            "description": "s".repeat(1_000),
            "properties": {},
            "required": [],
            "additionalProperties": false
        }),
    });
    let mut messages = vec![ChatMessage::user("small prompt")];

    let error = run_agent_turn(&provider, &tools, &mut messages, &config)
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        AgentError::ContextOverflow {
            unit: ContextUnit::Bytes,
            ..
        }
    ));
    assert_eq!(
        provider.call_count(),
        0,
        "the response schema must fit before any request is sent"
    );
}

#[tokio::test]
async fn final_round_nudge_is_budgeted_before_the_second_provider_call() {
    use crate::orchestrator::AgentError;
    use kitsu_providers::ChatMessage;

    let provider = MockProvider::new(vec![
        vec![
            ChatDelta::ToolCall(tc("clock", "c", "{}")),
            ChatDelta::Done(FinishReason::ToolCalls),
        ],
        vec![
            ChatDelta::Text("must not be reached".to_string()),
            ChatDelta::Done(FinishReason::Stop),
        ],
    ]);
    let mut tools = Registry::new();
    tools.register_with_descriptor(
        kitsu_providers::ToolDescriptor {
            name: "clock".to_string(),
            description: String::new(),
            parameters: serde_json::json!({"type": "object"}),
        },
        ok_handler("{}"),
    );
    let mut c = cfg(2);
    c.context_window = Some(400);
    c.max_tokens = Some(64); // target = 160
    let mut messages = vec![ChatMessage::system("s".repeat(480)), ChatMessage::user("q")];

    let err = run_agent_turn(&provider, &tools, &mut messages, &c)
        .await
        .unwrap_err();
    assert!(matches!(err, AgentError::ContextOverflow { .. }));
    assert_eq!(
        provider.call_count(),
        1,
        "the first round fits, but the final nudge must be checked before round two"
    );
    assert!(
        messages
            .iter()
            .any(|message| message.content.contains("reached the final step")),
        "sanity: the request-visible mutation that caused the overflow was present"
    );
}

#[tokio::test]
async fn manual_trim_compacts_below_threshold_and_ledger_persists_across_calls() {
    use crate::orchestrator::{compact_context, CompactionOutcome};
    use kitsu_providers::ChatMessage;

    let provider = MockProvider::new(vec![]);
    let tools = Registry::new();
    let mut c = cfg(10);
    c.context_window = Some(8_192);
    c.max_tokens = Some(64);
    let mut messages = vec![ChatMessage::system("sys")];
    for k in 0..4 {
        messages.push(ChatMessage::user(format!("q{k}")));
        messages.push(ChatMessage::assistant(format!("a{k}"), vec![]));
    }
    let mut state = AgentSessionState::default();

    let first = compact_context(&provider, &tools, &mut messages, &c, &mut state)
        .await
        .unwrap();
    let first_record = first.record.unwrap();
    assert!(
        first.changed,
        "manual compaction must not wait for pressure"
    );
    assert_eq!(first_record.sequence, 1);
    assert_eq!(first_record.outcome, CompactionOutcome::Trimmed);
    assert_eq!(first_record.exchanges_removed, 2);
    assert!(messages
        .iter()
        .any(|m| m.content.contains("trimmed to fit")));

    // Grow the same session and compact again. The ledger must survive calls,
    // while an existing trim marker is not miscounted as an eligible exchange.
    for k in 4..7 {
        messages.push(ChatMessage::user(format!("q{k}")));
        messages.push(ChatMessage::assistant(format!("a{k}"), vec![]));
    }
    let second = compact_context(&provider, &tools, &mut messages, &c, &mut state)
        .await
        .unwrap();
    assert_eq!(second.record.unwrap().sequence, 2);
    assert_eq!(state.compaction_ledger.total_compactions, 2);
    assert_eq!(state.compaction_ledger.manual_compactions, 2);
    assert_eq!(state.compaction_ledger.records.len(), 2);
    assert_eq!(state.compaction_ledger.exchanges_removed, 5);
    assert!(state.compaction_ledger.tokens_saved > 0);
    assert_eq!(provider.call_count(), 0, "Trim mode is entirely local");
}

#[tokio::test]
async fn summary_request_and_rebuilt_history_are_bounded_and_disclosed() {
    use crate::context::{message_tokens, ContextBudget};
    use crate::orchestrator::{compact_context, CompactionMode, CompactionOutcome};
    use kitsu_providers::ChatMessage;

    let provider = MockProvider::new(vec![vec![
        ChatDelta::Text("S".repeat(5_000)),
        ChatDelta::Done(FinishReason::Stop),
    ]]);
    let tools = Registry::new();
    let mut c = cfg(10);
    c.context_window = Some(1_200);
    c.max_tokens = Some(10_000);
    c.compaction = CompactionMode::Summarize;
    let mut messages = vec![ChatMessage::system("sys")];
    for k in 0..5 {
        let body = if k < 3 {
            "x".repeat(1_200)
        } else {
            "x".repeat(40)
        };
        messages.push(ChatMessage::user(format!("q{k} {body}")));
        messages.push(ChatMessage::assistant(format!("a{k} {body}"), vec![]));
    }
    let mut state = AgentSessionState::default();

    let report = compact_context(&provider, &tools, &mut messages, &c, &mut state)
        .await
        .unwrap();
    let record = report.record.unwrap();
    assert_eq!(record.outcome, CompactionOutcome::Summarized);
    assert!(record.summary_input_shortened);
    assert!(record.summary_output_shortened);
    assert!(record.tokens_after <= report.target_tokens);

    let seen_max = provider.seen_max_tokens.lock().unwrap();
    assert_eq!(
        seen_max.as_slice(),
        &[Some(600)],
        "the summary output cap must be derived from half the actual 1,200-token window"
    );
    drop(seen_max);
    let seen_messages = provider.seen_messages.lock().unwrap();
    let summary_prompt_tokens: u64 = seen_messages[0].iter().map(message_tokens).sum();
    assert!(
        summary_prompt_tokens <= ContextBudget::new(1_200, Some(600)).target,
        "the summarizer's own concrete prompt must satisfy its request target"
    );
    assert_eq!(state.compaction_ledger.summaries, 1);
}

#[tokio::test]
async fn summary_prompt_that_cannot_fit_falls_back_without_provider_call() {
    use crate::orchestrator::{
        compact_context, CompactionMode, CompactionOutcome, SummaryFallbackReason,
    };
    use kitsu_providers::ChatMessage;

    let provider = MockProvider::new(vec![vec![ChatDelta::Done(FinishReason::Stop)]]);
    let tools = Registry::new();
    let mut c = cfg(10);
    c.context_window = Some(100); // summary instructions alone exceed target=40
    c.max_tokens = Some(64);
    c.compaction = CompactionMode::Summarize;
    let mut messages = vec![ChatMessage::system("sys")];
    for k in 0..4 {
        messages.push(ChatMessage::user(format!("q{k}")));
        messages.push(ChatMessage::assistant(format!("a{k}"), vec![]));
    }
    let mut state = AgentSessionState::default();

    let report = compact_context(&provider, &tools, &mut messages, &c, &mut state)
        .await
        .unwrap();
    let record = report.record.unwrap();
    assert_eq!(record.outcome, CompactionOutcome::SummaryFallbackTrim);
    assert_eq!(
        record.fallback_reason,
        Some(SummaryFallbackReason::RequestTooLarge)
    );
    assert_eq!(
        provider.call_count(),
        0,
        "an oversized summarizer prompt must fall back locally, never be sent"
    );
}

#[tokio::test]
async fn structured_event_log_frames_provider_usage_tools_and_completion() {
    use crate::orchestrator::{AgentEventLog, AgentRuntimeEvent};
    use kitsu_providers::{ChatMessage, Usage};

    let provider = MockProvider::new(vec![
        vec![
            ChatDelta::ToolCall(tc("read", "call-1", r#"{"path":"a.txt"}"#)),
            ChatDelta::Done(FinishReason::ToolCalls),
        ],
        vec![
            ChatDelta::Text("done".to_string()),
            ChatDelta::Usage(Usage {
                prompt_tokens: 10,
                completion_tokens: 2,
                total_tokens: 12,
                prompt_tokens_details: None,
            }),
            ChatDelta::Done(FinishReason::Stop),
        ],
    ]);
    let mut tools = Registry::new();
    tools.register("read", ok_handler(r#"{"ok":true}"#));
    let log = AgentEventLog::default();
    let mut c = cfg(4);
    c.quiet = true;
    c.event_log = Some(log.clone());
    let mut messages = vec![ChatMessage::user("work")];

    let answer = run_agent_turn(&provider, &tools, &mut messages, &c)
        .await
        .unwrap();
    assert_eq!(answer, "done");
    let events = log.take();
    assert!(matches!(
        events.first(),
        Some(AgentRuntimeEvent::ProviderRequest { round: 1, .. })
    ));
    assert!(events.iter().any(|event| matches!(
        event,
        AgentRuntimeEvent::ToolStarted { name, call_id, .. }
            if name == "read" && call_id == "call-1"
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        AgentRuntimeEvent::ToolFinished {
            success: true,
            cancelled: false,
            ..
        }
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        AgentRuntimeEvent::Usage {
            total_tokens: 12,
            ..
        }
    )));
    assert!(matches!(
        events.last(),
        Some(AgentRuntimeEvent::TurnFinished {
            success: true,
            completion,
            tool_errors: 0,
            policy_errors: 0,
        }) if completion == "complete"
    ));
    assert!(log.take().is_empty(), "take drains the trace");
}

#[tokio::test]
async fn structured_event_log_streams_safe_assistant_deltas_while_quiet() {
    use crate::orchestrator::{AgentEventLog, AgentRuntimeEvent};
    use kitsu_providers::ChatMessage;

    let provider = MockProvider::new(vec![vec![
        ChatDelta::Text("hello ".to_string()),
        ChatDelta::Text("world".to_string()),
        ChatDelta::Done(FinishReason::Stop),
    ]]);
    let tools = Registry::new();
    let log = AgentEventLog::default();
    let mut c = cfg(1);
    c.stream = true;
    c.quiet = true;
    c.event_log = Some(log.clone());
    let mut messages = vec![ChatMessage::user("work")];

    assert_eq!(
        run_agent_turn(&provider, &tools, &mut messages, &c)
            .await
            .unwrap(),
        "hello world"
    );
    let deltas: Vec<_> = log
        .take()
        .into_iter()
        .filter_map(|event| match event {
            AgentRuntimeEvent::AssistantDelta { round, text } => Some((round, text)),
            _ => None,
        })
        .collect();
    assert_eq!(
        deltas,
        vec![(1, "hello ".to_string()), (1, "world".to_string())]
    );
}

#[tokio::test]
async fn structured_event_log_never_emits_synthesized_text_tool_json() {
    use crate::orchestrator::{AgentEventLog, AgentRuntimeEvent};
    use kitsu_providers::{ChatMessage, ToolDescriptor};

    let raw_call = r#"{"name":"read","arguments":{"path":"README.md"}}"#;
    let provider = MockProvider::new(vec![
        vec![
            ChatDelta::Text(raw_call.to_string()),
            ChatDelta::Done(FinishReason::Stop),
        ],
        vec![
            ChatDelta::Text("safe final answer".to_string()),
            ChatDelta::Done(FinishReason::Stop),
        ],
    ]);
    let mut tools = Registry::new();
    tools.register_with_descriptor(
        ToolDescriptor {
            name: "read".to_string(),
            description: "read".to_string(),
            parameters: serde_json::json!({"type": "object"}),
        },
        ok_handler(r#"{"ok":true}"#),
    );
    let log = AgentEventLog::default();
    let mut c = cfg(3);
    c.stream = true;
    c.quiet = true;
    c.event_log = Some(log.clone());
    let mut messages = vec![ChatMessage::user("work")];

    assert_eq!(
        run_agent_turn(&provider, &tools, &mut messages, &c)
            .await
            .unwrap(),
        "safe final answer"
    );
    let emitted: String = log
        .take()
        .into_iter()
        .filter_map(|event| match event {
            AgentRuntimeEvent::AssistantDelta { text, .. } => Some(text),
            _ => None,
        })
        .collect();
    assert_eq!(emitted, "safe final answer");
    assert!(!emitted.contains(raw_call));
}

#[tokio::test]
async fn assistant_deltas_exclude_intermediate_structured_tool_narration() {
    use crate::orchestrator::{AgentEventLog, AgentRuntimeEvent};
    use kitsu_providers::{ChatMessage, ToolDescriptor};

    let provider = MockProvider::new(vec![
        vec![
            ChatDelta::Text("I will inspect the file first.".to_string()),
            ChatDelta::ToolCall(tc("read", "call-1", r#"{"path":"README.md"}"#)),
            ChatDelta::Done(FinishReason::ToolCalls),
        ],
        vec![
            ChatDelta::Text("The final answer.".to_string()),
            ChatDelta::Done(FinishReason::Stop),
        ],
    ]);
    let mut tools = Registry::new();
    tools.register_with_descriptor(
        ToolDescriptor {
            name: "read".to_string(),
            description: "read".to_string(),
            parameters: serde_json::json!({"type": "object"}),
        },
        ok_handler(r#"{"ok":true}"#),
    );
    let log = AgentEventLog::default();
    let mut c = cfg(3);
    c.stream = true;
    c.quiet = true;
    c.event_log = Some(log.clone());
    let mut messages = vec![ChatMessage::user("work")];

    let answer = run_agent_turn(&provider, &tools, &mut messages, &c)
        .await
        .unwrap();
    let emitted: String = log
        .take()
        .into_iter()
        .filter_map(|event| match event {
            AgentRuntimeEvent::AssistantDelta { text, .. } => Some(text),
            _ => None,
        })
        .collect();

    assert_eq!(answer, "The final answer.");
    assert_eq!(emitted, answer);
    assert!(!emitted.contains("inspect the file"));
}

#[tokio::test]
async fn structured_event_log_uses_native_command_fields_instead_of_json_blobs() {
    use crate::orchestrator::{AgentEventLog, AgentRuntimeEvent};
    use kitsu_providers::ChatMessage;

    let provider = MockProvider::new(vec![
        vec![
            ChatDelta::ToolCall(tc(
                "command_exec",
                "command-1",
                r#"{"argv":["git","status"],"cwd":"/workspace","timeout_ms":5000}"#,
            )),
            ChatDelta::Done(FinishReason::ToolCalls),
        ],
        vec![
            ChatDelta::Text("done".to_string()),
            ChatDelta::Done(FinishReason::Stop),
        ],
    ]);
    let mut tools = Registry::new();
    tools.register(
        "command_exec",
        ok_handler(
            r#"{"argv":["git","status"],"cwd":"/workspace","executable":{"invocation":"git","resolution":"ambient_path","observed_path":"/usr/bin/git","pinned":false},"ambient_authority":{"inherits_environment":true,"filesystem_confined_to_command_roots":false,"network_confined":false,"executable_identity_pinned":false},"termination":"exited","success":true,"exit_code":0,"signal":null,"stdout":"clean","stderr":"","stdout_bytes":5,"stderr_bytes":0,"stdout_retained_bytes":5,"stderr_retained_bytes":0,"stdout_truncated":false,"stderr_truncated":false,"stdout_capture_complete":true,"stderr_capture_complete":true,"elapsed_ms":12,"artifact":{"reference":"session:command-1"},"artifact_error":null}"#,
        ),
    );
    let log = AgentEventLog::default();
    let mut c = cfg(4);
    c.quiet = true;
    c.event_log = Some(log.clone());
    let mut messages = vec![ChatMessage::user("work")];

    assert_eq!(
        run_agent_turn(&provider, &tools, &mut messages, &c)
            .await
            .unwrap(),
        "done"
    );
    let events = log.take();
    assert!(events.iter().any(|event| matches!(
        event,
        AgentRuntimeEvent::ToolStarted {
            call_id,
            args,
            command: Some(command),
            ..
        } if call_id == "command-1"
            && args == "[structured command invocation]"
            && command.argv == ["git", "status"]
            && command.cwd == "/workspace"
            && command.timeout_ms == Some(5000)
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        AgentRuntimeEvent::ToolFinished {
            call_id,
            output,
            command: Some(command),
            ..
        } if call_id == "command-1"
            && output == "[structured command result]"
            && command.exit_code == Some(0)
            && command.elapsed_ms == 12
            && command.stdout_bytes == 5
            && command.stdout_tail == "clean"
            && !command.ambient_authority.filesystem_confined_to_command_roots
            && command.artifact.as_ref().map(|artifact| artifact.reference.as_str())
                == Some("session:command-1")
    )));
}

#[tokio::test]
async fn structured_event_log_separates_logical_requests_from_physical_attempts() {
    use crate::orchestrator::{AgentEventLog, AgentRuntimeEvent, ProviderDeadlineScope};
    use kitsu_providers::{ChatMessage, ProviderAttemptOutcome};

    let provider = TelemetryProvider;
    let tools = Registry::new();
    let log = AgentEventLog::default();
    let mut c = cfg(1);
    c.quiet = true;
    c.event_log = Some(log.clone());
    c.request_timeout_ms = Some(30_000);
    c.turn_deadline = Some(tokio::time::Instant::now() + std::time::Duration::from_secs(60));
    let mut messages = vec![ChatMessage::user("work")];

    run_agent_turn(&provider, &tools, &mut messages, &c)
        .await
        .unwrap();
    let events = log.take();
    let requests = events
        .iter()
        .filter_map(|event| match event {
            AgentRuntimeEvent::ProviderRequest {
                request_timeout_ms,
                turn_remaining_ms,
                ..
            } => Some((*request_timeout_ms, *turn_remaining_ms)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(requests.len(), 1, "retries must not duplicate the request");
    assert_eq!(requests[0].0, Some(30_000));
    assert!(requests[0].1.is_some_and(|remaining| remaining <= 60_000));
    let attempts: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            AgentRuntimeEvent::ProviderAttempt {
                round,
                attempt,
                outcome,
                terminal,
                deadline_scope,
                remaining_deadline_ms,
                turn_remaining_ms,
                ..
            } => Some((
                *round,
                *attempt,
                *outcome,
                *terminal,
                *deadline_scope,
                *remaining_deadline_ms,
                *turn_remaining_ms,
            )),
            _ => None,
        })
        .collect();
    assert_eq!(attempts.len(), 2);
    assert_eq!(attempts[0].0, 1);
    assert_eq!(attempts[0].1, 1);
    assert_eq!(attempts[0].2, ProviderAttemptOutcome::RetryScheduled);
    assert!(!attempts[0].3);
    assert_eq!(attempts[0].4, ProviderDeadlineScope::ProviderRequest);
    assert_eq!(attempts[0].5, 10_000);
    assert_eq!(attempts[1].0, 1);
    assert_eq!(attempts[1].1, 2);
    assert_eq!(attempts[1].2, ProviderAttemptOutcome::ResponseAccepted);
    assert!(attempts[1].3);
    assert_eq!(attempts[1].4, ProviderDeadlineScope::ProviderRequest);
    assert_eq!(attempts[1].5, 9_700);
    assert!(attempts
        .iter()
        .all(|attempt| attempt.6.is_some_and(|remaining| remaining <= 60_000)));
    assert!(attempts[1].6 <= attempts[0].6);
}

#[tokio::test]
async fn structured_event_log_marks_normal_tool_errors_as_finished_not_cancelled() {
    use crate::orchestrator::{AgentEventLog, AgentRuntimeEvent};
    use kitsu_providers::{ChatMessage, ToolDescriptor};

    let provider = MockProvider::new(vec![
        vec![
            ChatDelta::ToolCall(tc("fail", "call-error", "{}")),
            ChatDelta::Done(FinishReason::ToolCalls),
        ],
        vec![
            ChatDelta::Text("recovered".to_string()),
            ChatDelta::Done(FinishReason::Stop),
        ],
    ]);
    let mut tools = Registry::new();
    tools.register_with_descriptor(
        ToolDescriptor {
            name: "fail".to_string(),
            description: "fail normally".to_string(),
            parameters: serde_json::json!({"type": "object"}),
        },
        err_handler(Arc::new(AtomicUsize::new(0))),
    );
    let log = AgentEventLog::default();
    let mut c = cfg(4);
    c.quiet = true;
    c.event_log = Some(log.clone());
    let mut messages = vec![ChatMessage::user("work")];

    assert_eq!(
        run_agent_turn(&provider, &tools, &mut messages, &c)
            .await
            .unwrap(),
        "recovered"
    );
    let events = log.take();
    assert!(events.iter().any(|event| matches!(
        event,
        AgentRuntimeEvent::ToolFinished {
            call_id,
            success: false,
            cancelled: false,
            output,
            ..
        } if call_id == "call-error" && output.contains("boom")
    )));
}

#[tokio::test]
async fn dropping_in_flight_tool_turn_records_one_cancelled_terminal_event() {
    use crate::orchestrator::{AgentEventLog, AgentRuntimeEvent};
    use kitsu_providers::{ChatMessage, ToolDescriptor};

    let provider = MockProvider::new(vec![vec![
        ChatDelta::ToolCall(tc("wait", "call-pending", "{}")),
        ChatDelta::Done(FinishReason::ToolCalls),
    ]]);
    let started = Arc::new(tokio::sync::Notify::new());
    let handler_started = started.clone();
    let handler: HandlerFn = Box::new(move |_args| {
        let handler_started = handler_started.clone();
        Box::pin(async move {
            handler_started.notify_one();
            futures::future::pending::<Result<String, ToolError>>().await
        })
    });
    let mut tools = Registry::new();
    tools.register_with_descriptor(
        ToolDescriptor {
            name: "wait".to_string(),
            description: "wait forever".to_string(),
            parameters: serde_json::json!({"type": "object"}),
        },
        handler,
    );
    let log = AgentEventLog::default();
    let mut c = cfg(4);
    c.quiet = true;
    c.event_log = Some(log.clone());
    let mut messages = vec![ChatMessage::user("work")];

    let mut turn = Box::pin(run_agent_turn(&provider, &tools, &mut messages, &c));
    tokio::select! {
        _ = started.notified() => {}
        result = &mut turn => panic!("pending tool turn unexpectedly finished: {result:?}"),
    }
    drop(turn);

    let events = log.take();
    assert_eq!(
        events
            .iter()
            .filter(
                |event| matches!(event, AgentRuntimeEvent::ToolStarted { call_id, .. }
                if call_id == "call-pending")
            )
            .count(),
        1
    );
    let terminal = events
        .iter()
        .filter(|event| {
            matches!(event, AgentRuntimeEvent::ToolFinished { call_id, .. }
            if call_id == "call-pending")
        })
        .collect::<Vec<_>>();
    assert_eq!(
        terminal.len(),
        1,
        "a started tool needs exactly one terminal"
    );
    assert!(matches!(
        terminal[0],
        AgentRuntimeEvent::ToolFinished {
            success: false,
            cancelled: true,
            output,
            ..
        } if output.contains("cancelled")
    ));
}

#[tokio::test]
async fn dropping_in_flight_command_records_native_cancelled_metadata() {
    use crate::orchestrator::{AgentEventLog, AgentRuntimeEvent};
    use kitsu_providers::{ChatMessage, ToolDescriptor};
    use kitsu_tools::CommandTermination;

    let provider = MockProvider::new(vec![vec![
        ChatDelta::ToolCall(tc(
            "command_exec",
            "call-command-pending",
            r#"{"argv":["sleep","30"],"cwd":".","timeout_ms":30000}"#,
        )),
        ChatDelta::Done(FinishReason::ToolCalls),
    ]]);
    let started = Arc::new(tokio::sync::Notify::new());
    let handler_started = started.clone();
    let handler: HandlerFn = Box::new(move |_args| {
        let handler_started = handler_started.clone();
        Box::pin(async move {
            handler_started.notify_one();
            futures::future::pending::<Result<String, ToolError>>().await
        })
    });
    let mut tools = Registry::new();
    tools.register_with_descriptor(
        ToolDescriptor {
            name: "command_exec".to_string(),
            description: "wait forever".to_string(),
            parameters: serde_json::json!({"type": "object"}),
        },
        handler,
    );
    let log = AgentEventLog::default();
    let mut c = cfg(4);
    c.quiet = true;
    c.event_log = Some(log.clone());
    let mut messages = vec![ChatMessage::user("work")];

    let mut turn = Box::pin(run_agent_turn(&provider, &tools, &mut messages, &c));
    tokio::select! {
        _ = started.notified() => {}
        result = &mut turn => panic!("pending command turn unexpectedly finished: {result:?}"),
    }
    drop(turn);

    let events = log.take();
    let terminal = events
        .iter()
        .find(|event| {
            matches!(event, AgentRuntimeEvent::ToolFinished { call_id, .. }
                if call_id == "call-command-pending")
        })
        .expect("cancelled command needs a terminal event");
    assert!(matches!(
        terminal,
        AgentRuntimeEvent::ToolFinished {
            success: false,
            cancelled: true,
            command: Some(command),
            ..
        } if command.termination == CommandTermination::Cancelled
            && !command.stdout_capture_complete
            && !command.stderr_capture_complete
            && command.artifact.is_none()
            && command.artifact_error.as_deref().is_some_and(|status| {
                status.contains("state was observed")
                    && status.contains("unverified")
                    && status.contains("no cleanup is claimed")
                    && !status.contains("cleanup was requested")
            })
    ));
}

#[cfg(unix)]
#[tokio::test]
async fn cancelling_real_command_preserves_observed_output_and_executable() {
    use crate::orchestrator::{AgentEventLog, AgentRuntimeEvent};
    use kitsu_providers::ChatMessage;
    use kitsu_tools::{command_exec_tool, CommandMode, CommandPolicy, CommandTermination};

    let dir = tempfile::tempdir().unwrap();
    let ready = dir.path().join("ready");
    let script = "printf observed-out; printf observed-err >&2; : > \"$1\"; sleep 30";
    let argv = vec![
        "/bin/sh".to_string(),
        "-c".to_string(),
        script.to_string(),
        "sh".to_string(),
        ready.to_string_lossy().into_owned(),
    ];
    let args = serde_json::json!({
        "argv": argv,
        "cwd": dir.path().to_string_lossy(),
        "timeout_ms": 30_000,
    })
    .to_string();
    let provider = MockProvider::new(vec![vec![
        ChatDelta::ToolCall(tc("command_exec", "call-real-command", &args)),
        ChatDelta::Done(FinishReason::ToolCalls),
    ]]);
    let policy = Arc::new(
        CommandPolicy::new(
            &[dir.path().to_string_lossy().into_owned()],
            CommandMode::Deny,
        )
        .unwrap(),
    );
    policy.grant_command(&argv, dir.path()).unwrap();
    let (descriptor, handler) = command_exec_tool(policy);
    let mut tools = Registry::new();
    tools.register_with_descriptor(descriptor, handler);
    let log = AgentEventLog::default();
    let mut c = cfg(4);
    c.quiet = true;
    c.event_log = Some(log.clone());
    let mut messages = vec![ChatMessage::user("work")];

    let mut turn = Box::pin(run_agent_turn(&provider, &tools, &mut messages, &c));
    for _ in 0..100 {
        tokio::select! {
            result = &mut turn => panic!("command unexpectedly finished: {result:?}"),
            _ = tokio::time::sleep(std::time::Duration::from_millis(10)) => {}
        }
        if ready.exists() {
            break;
        }
    }
    assert!(ready.exists(), "command should reach its sleeping phase");
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    drop(turn);

    let command = log
        .take()
        .into_iter()
        .find_map(|event| match event {
            AgentRuntimeEvent::ToolFinished {
                call_id,
                cancelled: true,
                command,
                ..
            } if call_id == "call-real-command" => command,
            _ => None,
        })
        .expect("real command cancellation needs structured progress");
    assert_eq!(command.termination, CommandTermination::Cancelled);
    assert!(command.stdout_bytes >= "observed-out".len() as u64);
    assert!(command.stderr_bytes >= "observed-err".len() as u64);
    assert!(command.stdout_tail.contains("observed-out"));
    assert!(command.stderr_tail.contains("observed-err"));
    assert!(command.executable.observed_path.is_some());
    assert!(command.elapsed_ms > 0);
    assert!(!command.stdout_capture_complete);
    assert!(!command.stderr_capture_complete);
    assert!(command.artifact.is_none());
    assert!(command.artifact_error.as_deref().is_some_and(|status| {
        status.contains("no full-output artifact was created")
            && status.contains("no cleanup was required")
    }));
}

#[tokio::test]
async fn failed_live_event_sink_stops_before_provider_send() {
    use crate::orchestrator::{AgentError, AgentEventLog, AgentRuntimeEvent};
    use kitsu_providers::ChatMessage;

    let provider = MockProvider::new(vec![vec![
        ChatDelta::Text("must not be requested".to_string()),
        ChatDelta::Done(FinishReason::Stop),
    ]]);
    let log = AgentEventLog::with_fallible_observer(|event| {
        !matches!(event, AgentRuntimeEvent::ProviderRequest { .. })
    });
    let mut c = cfg(2);
    c.quiet = true;
    c.event_log = Some(log);
    let mut messages = vec![ChatMessage::user("work")];

    let error = run_agent_turn(&provider, &Registry::new(), &mut messages, &c)
        .await
        .unwrap_err();
    assert!(matches!(error, AgentError::Loop(message) if message.contains("event sink")));
    assert_eq!(provider.call_count(), 0, "no unobservable send may start");
}

#[tokio::test]
async fn failed_live_event_sink_stops_before_tool_mutation() {
    use crate::orchestrator::{AgentError, AgentEventLog, AgentRuntimeEvent};
    use kitsu_providers::{ChatMessage, ToolDescriptor};

    let provider = MockProvider::new(vec![vec![
        ChatDelta::ToolCall(tc("mutate", "call-must-not-run", "{}")),
        ChatDelta::Done(FinishReason::ToolCalls),
    ]]);
    let runs = Arc::new(AtomicUsize::new(0));
    let handler_runs = runs.clone();
    let handler: HandlerFn = Box::new(move |_args| {
        let handler_runs = handler_runs.clone();
        Box::pin(async move {
            handler_runs.fetch_add(1, Ordering::SeqCst);
            Ok("mutated".to_string())
        })
    });
    let mut tools = Registry::new();
    tools.register_with_descriptor(
        ToolDescriptor {
            name: "mutate".to_string(),
            description: "must not run".to_string(),
            parameters: serde_json::json!({"type": "object"}),
        },
        handler,
    );
    let log = AgentEventLog::with_fallible_observer(|event| {
        !matches!(event, AgentRuntimeEvent::ToolStarted { .. })
    });
    let mut c = cfg(2);
    c.quiet = true;
    c.event_log = Some(log.clone());
    let mut messages = vec![ChatMessage::user("work")];

    let error = run_agent_turn(&provider, &tools, &mut messages, &c)
        .await
        .unwrap_err();
    assert!(matches!(error, AgentError::Loop(message) if message.contains("event sink")));
    assert_eq!(runs.load(Ordering::SeqCst), 0);
    assert!(log.take().iter().any(|event| matches!(
        event,
        AgentRuntimeEvent::ToolFinished {
            call_id,
            cancelled: true,
            ..
        } if call_id == "call-must-not-run"
    )));
}

// --- recovered tool errors & final-round status ------------------------------

/// A tool failure the model corrects in a later round is the normal agentic
/// retry pattern: cumulative `tool_errors` still reports it, but
/// `final_round_tool_errors` must be zero so strict callers can commit the
/// turn on its successful final state.
#[tokio::test]
async fn test_recovered_tool_error_clears_final_round_status() {
    let provider = MockProvider::new(vec![
        // round 1: the failing call
        vec![
            ChatDelta::ToolCall(tc("flaky", "call_1", "{\"try\":1}")),
            ChatDelta::Done(FinishReason::ToolCalls),
        ],
        // round 2: the corrected call succeeds
        vec![
            ChatDelta::ToolCall(tc("flaky", "call_2", "{\"try\":2}")),
            ChatDelta::Done(FinishReason::ToolCalls),
        ],
        // round 3: final answer
        vec![
            ChatDelta::Text("fixed".to_string()),
            ChatDelta::Done(FinishReason::Stop),
        ],
    ]);
    let calls = Arc::new(AtomicUsize::new(0));
    let mut tools = Registry::new();
    tools.register("flaky", {
        let calls = calls.clone();
        Box::new(move |_args: String| {
            let calls = calls.clone();
            Box::pin(async move {
                if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                    Err::<String, ToolError>(ToolError::ExecError(
                        "edit conflict: old_text matched 2 times".to_string(),
                    ))
                } else {
                    Ok::<String, ToolError>("updated".to_string())
                }
            })
        })
    });
    let mut state = AgentSessionState::default();
    let out = run_agent_turn_with_state(
        &provider,
        &tools,
        &mut initial_messages(),
        &cfg(10),
        &mut state,
    )
    .await
    .expect("a recovered tool error must not abort the loop");
    assert!(out.contains("fixed"));
    assert_eq!(state.last_turn.tool_errors, 1);
    assert_eq!(state.last_turn.policy_errors, 0);
    assert_eq!(
        state.last_turn.final_round_tool_errors, 0,
        "the last executed tool round succeeded, so the recovery must be visible"
    );
    assert_eq!(state.last_turn.completion, TurnCompletion::Complete);
}

/// An UNrecovered tool failure (the last tool round still failed) keeps a
/// nonzero `final_round_tool_errors` so strict callers still reject the turn.
#[tokio::test]
async fn test_unrecovered_tool_error_keeps_final_round_status() {
    let provider = MockProvider::new(vec![
        vec![
            ChatDelta::ToolCall(tc("bad_tool", "call_err", "{}")),
            ChatDelta::Done(FinishReason::ToolCalls),
        ],
        vec![
            ChatDelta::Text("gave up".to_string()),
            ChatDelta::Done(FinishReason::Stop),
        ],
    ]);
    let ran = Arc::new(AtomicUsize::new(0));
    let mut tools = Registry::new();
    tools.register("bad_tool", err_handler(ran.clone()));
    let mut state = AgentSessionState::default();
    run_agent_turn_with_state(
        &provider,
        &tools,
        &mut initial_messages(),
        &cfg(10),
        &mut state,
    )
    .await
    .expect("a failing tool must not abort the loop");
    assert_eq!(state.last_turn.tool_errors, 1);
    assert_eq!(state.last_turn.final_round_tool_errors, 1);
}

// --- context-window runtime cross-check --------------------------------------

#[test]
fn test_context_window_shortfall_heuristic() {
    use crate::orchestrator::context_window_shortfall;
    // The audited Ollama case: resolved 8192, sent ~4299, server clamped 4096.
    assert!(context_window_shortfall(4096, 4299, 8192));
    // Acceptance-test shape: 5000-token prompt clamped to 4096.
    assert!(context_window_shortfall(4096, 5000, 8192));
    // Reported at/above the window is the window working as resolved.
    assert!(!context_window_shortfall(8192, 9000, 8192));
    // Small estimate noise (tokenizer beating chars/4) must not warn.
    assert!(!context_window_shortfall(4000, 4100, 8192));
    // Tiny prompts never trigger.
    assert!(!context_window_shortfall(90, 100, 8192));
    assert!(!context_window_shortfall(0, 100, 8192));
    // Non-power-of-two clamps still trigger on a wide margin.
    assert!(!context_window_shortfall(3900, 4100, 8192));
    assert!(context_window_shortfall(3000, 5000, 8192));
    // Cycle-14 HF false positive: an exact hosted tokenizer far under a big
    // resolved window is estimator overshoot, not a clamp — stay quiet.
    assert!(!context_window_shortfall(1007, 2178, 131_072));
    assert!(!context_window_shortfall(1007, 2178, 32_768));
    // …but a power-of-two clamp stays detectable even under a big window.
    assert!(context_window_shortfall(4096, 9000, 131_072));
}

/// A usage report far below the sent estimate records durable window evidence
/// on the session state (without rewriting the resolved window).
#[tokio::test]
async fn test_usage_shortfall_records_window_evidence() {
    let provider = MockProvider::new(vec![vec![
        ChatDelta::Text("short answer".to_string()),
        ChatDelta::Usage(kitsu_providers::Usage {
            prompt_tokens: 4096,
            completion_tokens: 3,
            total_tokens: 4099,
            prompt_tokens_details: None,
        }),
        ChatDelta::Done(FinishReason::Stop),
    ]]);
    let tools = Registry::new();
    let mut c = cfg(2);
    c.context_window = Some(8192);
    c.quiet = true;
    // ~20k chars => ~5k estimated tokens: inside the budget, far above 4096.
    let mut messages = vec![kitsu_providers::ChatMessage::user("x".repeat(20_000))];
    let mut state = AgentSessionState::default();
    run_agent_turn_with_state(&provider, &tools, &mut messages, &c, &mut state)
        .await
        .expect("turn should succeed");
    let evidence = state
        .window_evidence
        .expect("shortfall evidence must be recorded");
    assert_eq!(evidence.reported_prompt_tokens, 4096);
    assert_eq!(evidence.resolved_window, 8192);
    assert!(evidence.estimated_sent_tokens > 4096);
    assert!(!evidence.warned, "warning is the CLI's one-time job");
}

/// A healthy usage report records no evidence.
#[tokio::test]
async fn test_healthy_usage_records_no_window_evidence() {
    let provider = MockProvider::new(vec![vec![
        ChatDelta::Text("ok".to_string()),
        ChatDelta::Usage(kitsu_providers::Usage {
            prompt_tokens: 5100,
            completion_tokens: 2,
            total_tokens: 5102,
            prompt_tokens_details: None,
        }),
        ChatDelta::Done(FinishReason::Stop),
    ]]);
    let tools = Registry::new();
    let mut c = cfg(2);
    c.context_window = Some(8192);
    c.quiet = true;
    let mut messages = vec![kitsu_providers::ChatMessage::user("x".repeat(20_000))];
    let mut state = AgentSessionState::default();
    run_agent_turn_with_state(&provider, &tools, &mut messages, &c, &mut state)
        .await
        .expect("turn should succeed");
    assert!(state.window_evidence.is_none());
}

#[test]
fn template_control_debris_detector_fires_on_tails_not_quotes() {
    use crate::orchestrator::template_control_debris_in_tail;
    // The live failure shape: mangled trailing tool call with the file path.
    let leaked = "analysis text...\n<|close|>argument<|sep|><|open|>argument key=\"path\"<|sep|>/tmp/x.md<|close|>argument<|sep|><|close|>call<|sep|><|close|>tools<|sep|>";
    assert!(template_control_debris_in_tail(leaked));
    // A single stray token is not enough.
    assert!(!template_control_debris_in_tail(
        "ends with one token <|sep|>"
    ));
    // Tokens QUOTED early in a long answer (e.g. discussing chat templates)
    // do not trip the tail-window detector.
    let mut quoted =
        String::from("Kimi uses tokens like <|close|> and <|sep|> in its chat template. ");
    quoted.push_str(&"More analysis follows here. ".repeat(20));
    assert!(!template_control_debris_in_tail(&quoted));
    // Uppercase or long identifiers are not control tokens.
    assert!(!template_control_debris_in_tail(
        "x <|NOTATOKEN|> y <|this_is_a_much_too_long_identifier_name|> z"
    ));
    // Clean answers never trip.
    assert!(!template_control_debris_in_tail(
        "a perfectly normal answer."
    ));
}

/// P6: a single shortfall strike (`shortfall_count == 1`) must NOT clamp the
/// effective session budget that gates outgoing requests — the two-strike
/// rule holds end-to-end through `request_measurement`, not just in the pure
/// `clamp_window_for_shortfall` helper.
#[test]
fn test_single_shortfall_does_not_clamp_request_measurement() {
    use crate::context::ContextMeter;
    use crate::orchestrator::{request_measurement, ContextWindowEvidence};

    let mut c = cfg(2);
    c.context_window = Some(8_192);
    let meter = ContextMeter::new();
    let unclamped = request_measurement(&[], &c, "", false, None, &meter, None);

    let one_strike = Some(ContextWindowEvidence {
        reported_prompt_tokens: 4_096,
        estimated_sent_tokens: 5_000,
        resolved_window: 8_192,
        warned: false,
        shortfall_count: 1,
    });
    let still_unclamped = request_measurement(&[], &c, "", false, None, &meter, one_strike);
    assert_eq!(unclamped.effective, still_unclamped.effective);
    assert_eq!(unclamped.target, still_unclamped.target);
    // The resolved window reported alongside the budget is never rewritten.
    assert_eq!(still_unclamped.window, Some(8_192));
}

/// P6: two shortfall strikes for the SAME resolved window DO clamp the
/// effective session budget toward the reported size, end-to-end through
/// `request_measurement` (the function that actually gates outgoing
/// requests) — while `window` (the resolved window) is still reported
/// unchanged, proving only the derived budget adapted.
#[test]
fn test_two_shortfalls_clamp_request_measurement_effective_budget() {
    use crate::context::ContextMeter;
    use crate::orchestrator::{request_measurement, ContextWindowEvidence};

    let mut c = cfg(2);
    c.context_window = Some(8_192);
    let meter = ContextMeter::new();
    let unclamped = request_measurement(&[], &c, "", false, None, &meter, None);

    let two_strikes = Some(ContextWindowEvidence {
        reported_prompt_tokens: 4_096,
        estimated_sent_tokens: 5_000,
        resolved_window: 8_192,
        warned: false,
        shortfall_count: 2,
    });
    let clamped = request_measurement(&[], &c, "", false, None, &meter, two_strikes);
    assert!(
        clamped.effective.unwrap() < unclamped.effective.unwrap(),
        "two strikes must shrink the effective budget below the unclamped one"
    );
    assert!(clamped.target < unclamped.target);
    // Resolved window is STILL reported as 8192 — never rewritten, only the
    // derived effective/target budget changed.
    assert_eq!(clamped.window, Some(8_192));

    // Evidence for a DIFFERENT resolved window (a stale carry-over from a
    // previous `--context-window`/model) must not clamp against the current one.
    let stale = Some(ContextWindowEvidence {
        reported_prompt_tokens: 2_048,
        estimated_sent_tokens: 3_000,
        resolved_window: 4_096,
        warned: false,
        shortfall_count: 5,
    });
    let ignored_stale = request_measurement(&[], &c, "", false, None, &meter, stale);
    assert_eq!(unclamped.effective, ignored_stale.effective);
}
