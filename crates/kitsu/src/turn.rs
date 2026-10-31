//! Turn execution, rollback, and assistant disposition.

use crate::cli::Cli;
use crate::process_exit::{EXIT_CANCELLED, EXIT_PROVIDER, EXIT_USER};
use kitsu_core::{run_agent_turn_with_state, AgentConfig};
use kitsu_providers::compiled::CompiledProvider;
use kitsu_providers::{ChatMessage, ProviderError, ProviderRouteCurrent};
use kitsu_tools::Registry;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use crate::cli_error::CliError;
use crate::cli_runtime::active_output_schema;
use crate::cli_runtime::emit_jsonl;
use crate::cli_runtime::jsonl_output_failure;
use crate::cli_runtime::jsonl_requested;
use crate::cli_runtime::mark_model_inference_started;
use crate::cli_runtime::record_run_assistant_disposition;
use crate::cli_runtime::AssistantDisposition;
use crate::cli_runtime::FULLY_COMMITTED_ASSISTANT_SEEN;
use crate::presentation::format_dropped_results_notice;
use crate::presentation::format_runtime_notice;
use crate::presentation::terminal_inline;
use crate::prompt_queue;
use crate::terminal::approval::APPROVAL_INPUT_ACTIVE;
use crate::terminal::typeahead::TurnOutcome;
use crate::terminal::typeahead::ACTIVE_QUIT_CANCEL;
use crate::terminal::typeahead::FOREGROUND_CANCELLED;

/// Run turns from a script file non-interactively, threading multi-turn history
/// and printing a readable transcript (`user> …` then the response). Each line
/// is one turn; blank lines and `#` comments are skipped; `/slash` commands work.
///
/// A `<<<` line begins a MULTI-LINE turn (a pasted code block or stack trace),
/// preserved verbatim with its newlines until a closing `>>>` line — the scripted
/// equivalent of a bracketed paste in the interactive REPL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScriptFailureKind {
    User,
    Provider,
    OutputSchema,
    Tool,
    Policy,
    Cancelled,
    JsonlOutput,
}

impl ScriptFailureKind {
    pub(crate) fn exit_code(self) -> i32 {
        match self {
            Self::Provider | Self::OutputSchema => EXIT_PROVIDER,
            Self::Cancelled => EXIT_CANCELLED,
            Self::User | Self::Tool | Self::Policy | Self::JsonlOutput => EXIT_USER,
        }
    }

    pub(crate) fn strict_jsonl_kind(self) -> &'static str {
        match self {
            Self::Provider => "incomplete_turn",
            Self::OutputSchema => "output_schema",
            Self::Tool => "tool",
            Self::Policy => "policy",
            Self::User => "user",
            Self::Cancelled => "cancelled",
            Self::JsonlOutput => "jsonl_output",
        }
    }

    pub(crate) const fn is_provider_failure(self) -> bool {
        matches!(self, Self::Provider | Self::OutputSchema)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OutputSchemaFailureDetails {
    pub(crate) schema_sha256: String,
    pub(crate) instance_path: String,
    pub(crate) validation_keyword: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StrictTurnFailure {
    pub(crate) kind: ScriptFailureKind,
    pub(crate) issue: String,
    pub(crate) output_schema: Option<OutputSchemaFailureDetails>,
}

impl StrictTurnFailure {
    pub(crate) fn new(kind: ScriptFailureKind, issue: impl Into<String>) -> Self {
        Self {
            kind,
            issue: issue.into(),
            output_schema: None,
        }
    }

    pub(crate) fn jsonl_payload(
        &self,
        turn_id: &str,
        message: &str,
        turn_status: &kitsu_core::AgentTurnStatus,
    ) -> serde_json::Value {
        let mut payload = serde_json::json!({
            "turn_id": turn_id,
            "kind": self.kind.strict_jsonl_kind(),
            "message": message,
            "tool_errors": turn_status.tool_errors,
            "policy_errors": turn_status.policy_errors,
            "exit_code": self.kind.exit_code(),
        });
        if let Some(details) = self.output_schema.as_ref() {
            payload["schema_sha256"] = serde_json::json!(details.schema_sha256);
            payload["instance_path"] = serde_json::json!(details.instance_path);
            payload["validation_keyword"] = serde_json::json!(details.validation_keyword);
        }
        payload
    }
}

pub(crate) fn strict_failure_cli_error(failure: &StrictTurnFailure) -> CliError {
    match failure.kind {
        ScriptFailureKind::Provider => {
            CliError::provider(format!("incomplete turn: {}", failure.issue))
        }
        ScriptFailureKind::OutputSchema => CliError::provider(failure.issue.clone()),
        ScriptFailureKind::Tool | ScriptFailureKind::Policy => {
            CliError::tool(failure.issue.clone())
        }
        _ => CliError::user(failure.issue.clone()),
    }
}

pub(crate) fn script_failure_kind(error: &kitsu_core::AgentError) -> ScriptFailureKind {
    match error {
        kitsu_core::AgentError::Provider(
            ProviderError::Auth(_) | ProviderError::RouteContract(_),
        )
        | kitsu_core::AgentError::Interrupted(_)
        | kitsu_core::AgentError::ContextOverflow { .. }
        | kitsu_core::AgentError::ContextWindowUnknown => ScriptFailureKind::User,
        _ => ScriptFailureKind::Provider,
    }
}

pub(crate) fn agent_error_jsonl_cause(error: &kitsu_core::AgentError) -> Option<&'static str> {
    matches!(
        error,
        kitsu_core::AgentError::ToolCallDuringToolFreeFinalization
    )
    .then_some("tool_call_during_tool_free_finalization")
}

pub(crate) fn transcript_line(prefix: &str, text: &str) {
    use std::io::Write;
    if jsonl_requested() {
        return;
    }
    // Echoes are metadata, not assistant output. Keeping them with slash
    // diagnostics on stderr preserves clean pipeable stdout and deterministic
    // command/result ordering in captured scripts.
    let mut stderr = std::io::stderr().lock();
    let text = kitsu_render::sanitize_terminal_text(text);
    let _ = writeln!(stderr, "{prefix}> {text}");
    let _ = stderr.flush();
}

pub(crate) fn emit_agent_trace(
    turn_id: &str,
    log: &kitsu_core::AgentEventLog,
    assistant: Option<&str>,
) -> Result<(), CliError> {
    for event in log.take() {
        emit_runtime_event(turn_id, &event)?;
    }
    if let Some(text) = assistant {
        emit_jsonl(
            "assistant",
            serde_json::json!({"turn_id": turn_id, "text": text}),
        )?;
    }
    Ok(())
}

pub(crate) fn emit_runtime_event(
    turn_id: &str,
    event: &kitsu_core::AgentRuntimeEvent,
) -> Result<(), CliError> {
    if matches!(event, kitsu_core::AgentRuntimeEvent::TurnFinished { .. }) {
        return Ok(());
    }
    let Ok(serde_json::Value::Object(mut object)) = serde_json::to_value(event) else {
        return Ok(());
    };
    let kind = object
        .remove("event")
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_else(|| "runtime_event".to_string());
    object.insert(
        "turn_id".to_string(),
        serde_json::Value::String(turn_id.to_string()),
    );
    emit_jsonl(&kind, serde_json::Value::Object(object))
}

pub(crate) fn emit_command_runtime_event(
    line: usize,
    command: &str,
    event: &kitsu_core::AgentRuntimeEvent,
) -> Result<(), CliError> {
    if matches!(event, kitsu_core::AgentRuntimeEvent::TurnFinished { .. }) {
        return Ok(());
    }
    let Ok(serde_json::Value::Object(mut object)) = serde_json::to_value(event) else {
        return Ok(());
    };
    let kind = object
        .remove("event")
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_else(|| "runtime_event".to_string());
    object.insert("line".to_string(), serde_json::Value::Number(line.into()));
    object.insert(
        "command".to_string(),
        serde_json::Value::String(command.to_string()),
    );
    emit_jsonl(&kind, serde_json::Value::Object(object))
}

/// Streamed assistant output captured turn-side, so a deadline expiry can
/// preserve the already-delivered partial answer instead of discarding it.
#[derive(Debug, Default, Clone)]
pub(crate) struct PartialAnswer {
    pub(crate) text: String,
    pub(crate) last_round: u32,
}

pub(crate) type PartialAnswerBuffer = Arc<std::sync::Mutex<PartialAnswer>>;

pub(crate) fn snapshot_partial_answer(buffer: &PartialAnswerBuffer) -> PartialAnswer {
    buffer
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

pub(crate) fn live_agent_event_log(jsonl: bool, turn_id: &str) -> kitsu_core::AgentEventLog {
    live_agent_event_log_capturing(jsonl, turn_id, None)
}

pub(crate) fn live_agent_event_log_capturing(
    jsonl: bool,
    turn_id: &str,
    partial: Option<PartialAnswerBuffer>,
) -> kitsu_core::AgentEventLog {
    let turn_id = turn_id.to_string();
    kitsu_core::AgentEventLog::with_fallible_observer(move |event| {
        let observed_round = match event {
            kitsu_core::AgentRuntimeEvent::ProviderRequest { round, .. }
            | kitsu_core::AgentRuntimeEvent::AssistantDelta { round, .. } => Some(*round),
            _ => None,
        };
        if let Some(partial) = partial.as_ref() {
            let mut partial = partial
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(round) = observed_round {
                partial.last_round = partial.last_round.max(round);
            }
            if let kitsu_core::AgentRuntimeEvent::AssistantDelta { text, .. } = event {
                partial.text.push_str(text);
            }
        }
        if jsonl {
            // The process-long emitter retains a terminal write failure. The
            // turn boundary below converts it into the required nonzero exit.
            emit_runtime_event(&turn_id, event).is_ok()
        } else if let Some(notice) = format_dropped_results_notice(event) {
            kitsu_render::finish_line();
            eprintln!("~ {notice}");
            true
        } else if let Some(notice) = format_runtime_notice(event) {
            // A mid-stream status must start on its own line, never fused onto
            // the last streamed stdout character.
            kitsu_render::finish_line();
            eprintln!("~ {notice}");
            true
        } else {
            true
        }
    })
}

pub(crate) fn live_command_event_log(
    jsonl: bool,
    line: usize,
    command: &str,
) -> kitsu_core::AgentEventLog {
    let command = command.to_string();
    kitsu_core::AgentEventLog::with_fallible_observer(move |event| {
        if jsonl {
            emit_command_runtime_event(line, &command, event).is_ok()
        } else if let Some(notice) = format_dropped_results_notice(event) {
            kitsu_render::finish_line();
            eprintln!("~ {notice}");
            true
        } else if let Some(notice) = format_runtime_notice(event) {
            kitsu_render::finish_line();
            eprintln!("~ {notice}");
            true
        } else {
            true
        }
    })
}

pub(crate) fn finish_agent_trace(
    cli: &Cli,
    turn_id: &str,
    log: &kitsu_core::AgentEventLog,
    assistant: Option<&str>,
) -> Result<(), CliError> {
    if log.has_live_observer() {
        // Live observers already delivered every runtime record in causal
        // order. Drain the retained copy and emit only the assistant payload.
        let _ = log.take();
        if cli.jsonl {
            if let Some(error) = jsonl_output_failure() {
                return Err(error);
            }
            if let Some(text) = assistant {
                emit_jsonl(
                    "assistant",
                    serde_json::json!({"turn_id": turn_id, "text": text}),
                )?;
            }
        }
        return Ok(());
    }
    if cli.jsonl {
        return emit_agent_trace(turn_id, log, assistant);
    }
    for event in log.take() {
        if let Some(notice) = format_dropped_results_notice(&event) {
            kitsu_render::finish_line();
            eprintln!("~ {notice}");
        }
        if let Some(notice) = format_runtime_notice(&event) {
            kitsu_render::finish_line();
            eprintln!("~ {notice}");
        }
    }
    Ok(())
}

/// Marker appended to a deadline-expired partial answer, mirroring the
/// orchestrator's `[stopped: …]`/`[truncated: …]` stream markers so piped
/// consumers can detect the truncation in-band.
pub(crate) const DEADLINE_PARTIAL_MARKER: &str =
    "\n[stopped: deadline expired - answer is partial]";
pub(crate) const DEADLINE_TOOL_ROUNDS_MARKER: &str = "\n[stopped: deadline expired - completed tool results were preserved; no final answer was produced]";

/// Close a deadline-expired turn that already streamed real output: keep the
/// delivered partial answer and label it honestly (human notice / JSONL
/// `assistant` + `committed_partial` disposition) instead of destroying it.
/// Returns the full partial text (marker included) for callers that commit it
/// to a durable session. The turn still fails with the mapped provider error
/// (exit 2): only the destruction of already-delivered output changes, not
/// the failure classification.
pub(crate) fn emit_partial_deadline_answer(
    cli: &Cli,
    turn_id: &str,
    partial: &PartialAnswer,
) -> Result<String, CliError> {
    let marker = if partial.text.is_empty() {
        DEADLINE_TOOL_ROUNDS_MARKER
    } else {
        DEADLINE_PARTIAL_MARKER
    };
    let partial_text = format!("{}{}", partial.text, marker);
    if cli.jsonl {
        // Keep the deltas-concatenate-to-assistant contract: the marker is
        // emitted as a delta before the assistant payload that includes it.
        emit_jsonl(
            "assistant_delta",
            serde_json::json!({
                "turn_id": turn_id,
                "round": partial.last_round.max(1),
                "text": marker,
            }),
        )?;
        emit_jsonl(
            "assistant",
            serde_json::json!({"turn_id": turn_id, "text": partial_text}),
        )?;
    } else {
        // The partial bytes are already on stdout; append the marker there so
        // a piped consumer can detect the truncation without parsing stderr.
        kitsu_render::print_message(marker);
    }
    let reason = if partial.text.is_empty() {
        format!(
            "timed out after {}s - completed tool results and file changes were kept; no final answer was produced",
            cli.timeout_secs
        )
    } else {
        format!(
            "timed out after {}s - the answer above is partial; completed tool results and file changes were kept",
            cli.timeout_secs
        )
    };
    finish_assistant_disposition(
        cli,
        turn_id,
        AssistantDisposition::CommittedPartial,
        Some(&reason),
    )?;
    Ok(partial_text)
}

/// The deadline-expired partial answer for `error`, if there is one worth
/// preserving: the turn failed on the inference deadline AND real streamed
/// output was already delivered.
pub(crate) fn deadline_partial_answer(
    error: &kitsu_core::AgentError,
    partial: &PartialAnswerBuffer,
) -> Option<PartialAnswer> {
    is_deadline_timeout(error)
        .then(|| snapshot_partial_answer(partial))
        .filter(|partial| !partial.text.is_empty())
}

/// Did this turn fail on the inference deadline (as opposed to a provider or
/// tool error)? A deadline expiry leaves the completed tool rounds in the
/// message history worth preserving; other failures do not carry that promise.
pub(crate) fn is_deadline_timeout(error: &kitsu_core::AgentError) -> bool {
    matches!(
        error,
        kitsu_core::AgentError::Provider(ProviderError::Timeout)
    )
}

/// Drop an incomplete trailing tool round so `messages` is safe to persist and
/// resume. The orchestrator pushes an assistant `tool_calls` message and only
/// later pushes the matching tool results; if the turn deadline fires between
/// those, the tail is an assistant call with no (or partial) answers — a shape
/// most providers reject on the next request. Truncate back to the last point
/// where every requested tool call is answered, so the saved transcript always
/// ends on a complete round (or a plain user/assistant turn).
///
/// Returns the number of messages removed.
pub(crate) fn truncate_to_resumable_tail(messages: &mut Vec<ChatMessage>) -> usize {
    use kitsu_providers::Role;
    let original = messages.len();
    while let Some(call_idx) = messages
        .iter()
        .rposition(|m| m.role == Role::Assistant && !m.tool_calls.is_empty())
    {
        let answered: std::collections::HashSet<&str> = messages[call_idx + 1..]
            .iter()
            .filter(|m| m.role == Role::Tool)
            .filter_map(|m| m.tool_call_id.as_deref())
            .collect();
        let all_answered = messages[call_idx]
            .tool_calls
            .iter()
            .all(|call| answered.contains(call.call_id.as_str()));
        if all_answered {
            break;
        }
        // The most recent tool round is incomplete: drop it and any partial
        // answers that followed, then re-check the now-exposed tail.
        messages.truncate(call_idx);
    }
    original - messages.len()
}

/// Close the provisional assistant-output transaction after local strictness
/// checks. Streamed deltas remain useful for live UIs, but this terminal record
/// makes it impossible for automation to mistake rolled-back text for a
/// committed answer. Human mode visibly invalidates discarded output and
/// labels deadline-expired partial output honestly instead of destroying it.
pub(crate) fn finish_assistant_disposition(
    cli: &Cli,
    turn_id: &str,
    disposition: AssistantDisposition,
    reason: Option<&str>,
) -> Result<(), CliError> {
    record_run_assistant_disposition(disposition);
    if disposition == AssistantDisposition::Committed {
        FULLY_COMMITTED_ASSISTANT_SEEN.store(true, Ordering::Release);
    }
    if cli.jsonl {
        emit_jsonl(
            "assistant_disposition",
            serde_json::json!({
                "turn_id": turn_id,
                "status": disposition.jsonl_status(),
                "reason": reason,
            }),
        )?;
        return Ok(());
    }
    match disposition {
        AssistantDisposition::Committed => {}
        AssistantDisposition::CommittedPartial => {
            // The partial answer is already on stdout; terminate its line
            // before this stderr status so the two streams can't fuse.
            kitsu_render::finish_line();
            eprintln!(
                "~ {}",
                reason
                    .map(terminal_inline)
                    .unwrap_or_else(|| "the answer above is partial".to_string())
            );
        }
        AssistantDisposition::Discarded => {
            // Only warn about discarded provisional output when this turn
            // actually streamed some: an alarming DISCARDED line under a turn
            // that produced nothing is noise (JSONL always gets the record).
            let streamed = kitsu_render::turn_wrote_assistant_output();
            kitsu_render::finish_line();
            if streamed {
                eprintln!(
                    "~ assistant output above is provisional and was not committed{}",
                    reason
                        .map(|reason| format!(": {}", terminal_inline(reason)))
                        .unwrap_or_default()
                );
            }
        }
    }
    Ok(())
}

pub(crate) fn empty_final_response_issue(max_output_tokens: Option<u32>) -> String {
    if let Some(current) = max_output_tokens {
        let suggested = current.saturating_mul(2).max(current.saturating_add(1));
        format!(
            "model returned no final assistant text; retry with --max-output-tokens {suggested} or higher, or use a model that produces visible output"
        )
    } else {
        "model returned no final assistant text; retry with an explicit larger --max-output-tokens <n>, revise the prompt, or use a different model"
            .to_string()
    }
}

pub(crate) fn strict_turn_failure(
    status: &kitsu_core::AgentTurnStatus,
    max_output_tokens: Option<u32>,
    assistant_text: &str,
) -> Option<StrictTurnFailure> {
    use kitsu_core::TurnCompletion;
    // Policy denials always fail the turn: local authority was refused, and a
    // later successful call does not un-refuse it. Plain tool errors fail the
    // turn only when the FINAL tool round still carried one — an early failure
    // the model corrected in a later round (the normal agentic retry pattern,
    // e.g. an edit-precondition conflict followed by a fixed edit) commits.
    if status.policy_errors > 0 {
        let kind = if status.policy_errors == status.tool_errors {
            ScriptFailureKind::Policy
        } else {
            ScriptFailureKind::Tool
        };
        let issue = if kind == ScriptFailureKind::Policy {
            format!(
                "{} tool call(s) were denied by local policy during the turn",
                status.policy_errors
            )
        } else {
            format!(
                "{} tool call(s) failed during the turn ({} local policy denial(s))",
                status.tool_errors, status.policy_errors
            )
        };
        return Some(StrictTurnFailure::new(kind, issue));
    }
    if status.final_round_tool_errors > 0 {
        return Some(StrictTurnFailure::new(
            ScriptFailureKind::Tool,
            format!(
                "{} tool call(s) failed during the turn ({} local policy denial(s))",
                status.tool_errors, status.policy_errors
            ),
        ));
    }
    match status.completion {
        TurnCompletion::LengthLimited => {
            let issue = if let Some(current) = max_output_tokens {
                let suggested = current.saturating_mul(2).max(current.saturating_add(1));
                format!(
                    "model output hit --max-output-tokens {current}; retry with --max-output-tokens {suggested} or higher"
                )
            } else {
                "model output hit the gateway's length limit; retry with an explicit larger --max-output-tokens <n>"
                    .to_string()
            };
            Some(StrictTurnFailure::new(ScriptFailureKind::Provider, issue))
        }
        TurnCompletion::MaxRounds => Some(StrictTurnFailure::new(
            ScriptFailureKind::Provider,
            "agent exhausted the maximum tool rounds",
        )),
        TurnCompletion::Complete if assistant_text.trim().is_empty() => {
            Some(StrictTurnFailure::new(
                ScriptFailureKind::Provider,
                empty_final_response_issue(max_output_tokens),
            ))
        }
        TurnCompletion::Complete => active_output_schema().and_then(|schema| {
            schema.validate_text(assistant_text).err().map(|error| {
                StrictTurnFailure {
                    issue: format!(
                        "final assistant output failed --output-schema validation (sha256 {}): {error}",
                        schema.sha256
                    ),
                    kind: ScriptFailureKind::OutputSchema,
                    output_schema: Some(OutputSchemaFailureDetails {
                        schema_sha256: schema.sha256.clone(),
                        instance_path: error.instance_path,
                        validation_keyword: error.validation_keyword,
                    }),
                }
            })
        }),
    }
}

/// One-time warning for runtime context-window evidence (see
/// [`kitsu_core::ContextWindowEvidence`]): the provider's usage report implies
/// the server's real window is smaller than the resolved one. Human mode
/// prints one stderr line; JSONL emits a typed `context_window_mismatch`
/// event. The evidence stays recorded so `/context` provenance can surface it,
/// and the resolved window is never rewritten automatically.
pub(crate) fn warn_context_window_evidence(
    cli: &Cli,
    turn_id: Option<&str>,
    state: &mut kitsu_core::AgentSessionState,
) -> Result<(), CliError> {
    let Some(evidence) = state.window_evidence.as_mut() else {
        return Ok(());
    };
    if evidence.warned {
        return Ok(());
    }
    evidence.warned = true;
    if cli.jsonl {
        let mut payload = serde_json::json!({
            "reported_prompt_tokens": evidence.reported_prompt_tokens,
            "estimated_sent_tokens": evidence.estimated_sent_tokens,
            "resolved_window": evidence.resolved_window,
        });
        if let Some(turn_id) = turn_id {
            payload["turn_id"] = serde_json::Value::String(turn_id.to_string());
        }
        emit_jsonl("context_window_mismatch", payload)?;
    } else {
        eprintln!(
            "~ context window mismatch suspected: the provider accepted {} of ~{} estimated prompt tokens; the server likely serves a smaller window than the resolved {} and truncated the input (set --context-window {} or fix the gateway profile; /context shows provenance)",
            evidence.reported_prompt_tokens,
            evidence.estimated_sent_tokens,
            evidence.resolved_window,
            evidence.reported_prompt_tokens,
        );
    }
    Ok(())
}

/// Roll conversation-scoped agent state back to its pre-turn snapshot while
/// preserving runtime backend telemetry: context-window evidence describes the
/// serving backend, not the rolled-back turn, so a rollback must neither erase
/// it nor re-arm its one-time warning.
pub(crate) fn rollback_agent_state(
    state: &mut kitsu_core::AgentSessionState,
    mut snapshot: kitsu_core::AgentSessionState,
) {
    if snapshot.window_evidence.is_none() {
        snapshot.window_evidence = state.window_evidence;
    }
    *state = snapshot;
}

/// Run one turn, racing it against Ctrl+C. The turn future is awaited directly
/// (never spawned), so a Ctrl+C drops it — and with it the in-flight HTTP
/// request — with no leaked task (design §10). `state` persists the context-meter
/// anchor and compaction circuit breaker across the session's turns.
pub(crate) async fn await_cancellable<F>(future: F) -> Option<F::Output>
where
    F: std::future::Future,
{
    FOREGROUND_CANCELLED.store(false, Ordering::Release);
    tokio::pin!(future);
    let outcome = tokio::select! {
        result = &mut future => Some(result),
        _ = tokio::signal::ctrl_c() => None,
    };
    if outcome.is_none() {
        FOREGROUND_CANCELLED.store(true, Ordering::Release);
        while APPROVAL_INPUT_ACTIVE.load(Ordering::Acquire) > 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    } else {
        FOREGROUND_CANCELLED.store(false, Ordering::Release);
    }
    outcome
}

/// Read a potentially blocking OS stream on a detached thread. Dropping a
/// Tokio `spawn_blocking` task does not stop its syscall and the runtime waits
/// for such tasks during shutdown; a FIFO or an intentionally held-open stdin
/// could therefore prevent a Ctrl+C JSONL run from ever emitting and exiting.
/// Detached standard threads do not hold Tokio shutdown hostage.
pub(crate) async fn read_piped_stdin_cancellable() -> Option<std::io::Result<String>> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        use std::io::Read;
        let mut input = String::new();
        let result = std::io::stdin().read_to_string(&mut input).map(|_| input);
        let _ = sender.send(result);
    });
    await_cancellable(async move {
        receiver.await.unwrap_or_else(|_| {
            Err(std::io::Error::other(
                "stdin reader stopped before returning a result",
            ))
        })
    })
    .await
}

pub(crate) async fn read_text_file_cancellable(
    path: std::path::PathBuf,
) -> Option<std::io::Result<String>> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        let result = std::fs::read_to_string(path);
        let _ = sender.send(result);
    });
    await_cancellable(async move {
        receiver.await.unwrap_or_else(|_| {
            Err(std::io::Error::other(
                "file reader stopped before returning a result",
            ))
        })
    })
    .await
}

pub(crate) async fn run_turn_cancellable(
    provider: &ProviderRouteCurrent<CompiledProvider>,
    tools: &Registry,
    messages: &mut Vec<ChatMessage>,
    cfg: &AgentConfig,
    state: &mut kitsu_core::AgentSessionState,
) -> TurnOutcome {
    let provider = match provider.bind_model(&cfg.model) {
        Ok(provider) => provider,
        Err(error) => {
            return TurnOutcome::Completed(Err(kitsu_core::AgentError::Provider(error.into())))
        }
    };
    mark_model_inference_started();
    match await_cancellable(run_agent_turn_with_state(
        &provider, tools, messages, cfg, state,
    ))
    .await
    {
        Some(result) => TurnOutcome::Completed(result),
        None => TurnOutcome::Cancelled,
    }
}

pub(crate) enum InteractiveTurnOutcome {
    Turn(TurnOutcome),
    Steered(Vec<String>),
    QuitCancelled,
}

pub(crate) async fn wait_for_active_quit_cancel() {
    while !ACTIVE_QUIT_CANCEL.load(Ordering::Acquire) {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_turn_steerable(
    provider: &ProviderRouteCurrent<CompiledProvider>,
    tools: &Registry,
    messages: &mut Vec<ChatMessage>,
    cfg: &AgentConfig,
    state: &mut kitsu_core::AgentSessionState,
    steering: &prompt_queue::SteeringChannel,
    turn: prompt_queue::SteeringTurnId,
    mut corrections: prompt_queue::SteeringReceiver,
) -> InteractiveTurnOutcome {
    let provider = match provider.bind_model(&cfg.model) {
        Ok(provider) => provider,
        Err(error) => {
            return InteractiveTurnOutcome::Turn(TurnOutcome::Completed(Err(
                kitsu_core::AgentError::Provider(error.into()),
            )))
        }
    };
    mark_model_inference_started();
    debug_assert_eq!(corrections.turn(), turn);
    FOREGROUND_CANCELLED.store(false, Ordering::Release);
    ACTIVE_QUIT_CANCEL.store(false, Ordering::Release);
    let future = run_agent_turn_with_state(&provider, tools, messages, cfg, state);
    tokio::pin!(future);
    tokio::select! {
        result = &mut future => {
            let _ = steering.close_turn(turn);
            FOREGROUND_CANCELLED.store(false, Ordering::Release);
            InteractiveTurnOutcome::Turn(TurnOutcome::Completed(result))
        }
        _ = tokio::signal::ctrl_c() => {
            let _ = steering.close_turn(turn);
            FOREGROUND_CANCELLED.store(true, Ordering::Release);
            while APPROVAL_INPUT_ACTIVE.load(Ordering::Acquire) > 0 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            InteractiveTurnOutcome::Turn(TurnOutcome::Cancelled)
        }
        correction = corrections.recv() => {
            let _ = steering.close_turn(turn);
            FOREGROUND_CANCELLED.store(true, Ordering::Release);
            while APPROVAL_INPUT_ACTIVE.load(Ordering::Acquire) > 0 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            let mut texts = correction
                .into_iter()
                .map(prompt_queue::SteeringCorrection::into_text)
                .collect::<Vec<_>>();
            texts.extend(
                corrections
                    .drain_pending()
                    .into_iter()
                    .map(prompt_queue::SteeringCorrection::into_text),
            );
            InteractiveTurnOutcome::Steered(texts)
        }
        _ = wait_for_active_quit_cancel() => {
            let _ = steering.close_turn(turn);
            FOREGROUND_CANCELLED.store(true, Ordering::Release);
            while APPROVAL_INPUT_ACTIVE.load(Ordering::Acquire) > 0 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            InteractiveTurnOutcome::QuitCancelled
        }
    }
}
