//! The runtime agent loop. Implements the pseudocode in
//! docs/architecture.md §5: call model → parse tool calls → run tools →
//! append results → repeat until the model stops or a cap is hit.

use crate::context::{estimate_str, message_tokens, ContextBudget, ContextMeter};
use futures::StreamExt;
use kitsu_providers::{
    ChatDelta, ChatMessage, ChatRequest, FinishReason, Provider, ProviderAttemptCategory,
    ProviderAttemptOutcome, ProviderError, ProviderTelemetrySink, Role, ToolCall,
};
use kitsu_tools::{
    CommandAmbientAuthority, CommandExecArgs, CommandExecutableIdentity,
    CommandExecutableResolution, CommandOutput, CommandOutputArtifact, CommandTermination,
    Registry,
};
use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};
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
    /// Coarse fallback used only when `context_window` is unknown (Stage 1).
    pub max_context_bytes: usize,
    /// Resolved model context window in tokens, if known (from config / CLI /
    /// gateway metadata). When set, token-based budgeting supersedes
    /// `max_context_bytes`. Consumed by the trimming cascade (Stage 1c+).
    pub context_window: Option<u64>,
    /// Directory for offloading large tool results (Stage 2, Tier 0). When set,
    /// a tool result over the size cap is written here and replaced in context by
    /// a pointer + preview the model can re-read. Must be a model-readable path.
    pub offload_dir: Option<std::path::PathBuf>,
    pub stream: bool,
    pub temperature: Option<f32>,
    /// Max output tokens per response. `None` lets the gateway apply its own
    /// (often small, e.g. 4096) default — which silently truncates long outputs
    /// like code reviews. Set this to lift the cap.
    pub max_tokens: Option<u32>,
    /// What to do when cheap trimming (offload + elide) isn't enough to fit the
    /// window (Stage 3). `Trim` drops the oldest whole exchanges; `Summarize`
    /// replaces them with an LLM-written summary (opt-in — quality is model-
    /// dependent). Falls back to `Trim` if summarization fails.
    pub compaction: CompactionMode,
    /// Suppress this turn's terminal narration (spinner, `⏺`/`⎿` tool activity,
    /// and the `kitsu-usage:`/`kitsu-context:` lines). Set for BACKGROUND
    /// subagents so their output doesn't corrupt the interactive prompt while
    /// they run detached; their result is delivered via the agent registry.
    pub quiet: bool,
    /// Optional structured trace for automation frontends. By default events
    /// are retained in memory; a frontend may also install a synchronous,
    /// content-bounded observer to surface progress while the turn is active.
    pub event_log: Option<AgentEventLog>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum AgentRuntimeEvent {
    ProviderRequest {
        round: u32,
        message_count: usize,
        tools_offered: usize,
    },
    /// One physical network send inside the preceding logical
    /// [`ProviderRequest`](Self::ProviderRequest). Retries produce additional
    /// attempt events, never additional logical request events.
    ProviderAttempt {
        round: u32,
        attempt: u32,
        max_attempts: u32,
        category: ProviderAttemptCategory,
        status: Option<u16>,
        outcome: ProviderAttemptOutcome,
        terminal: bool,
        retry_delay_ms: Option<u64>,
        retry_after_used: bool,
        remaining_deadline_ms: u64,
    },
    Usage {
        round: u32,
        prompt_tokens: u64,
        completion_tokens: u64,
        total_tokens: u64,
    },
    ToolStarted {
        round: u32,
        call_id: String,
        name: String,
        /// Generic tools retain a bounded raw argument preview. Commands use
        /// `command` instead of embedding minified JSON here.
        args: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        command: Option<CommandRuntimeInvocation>,
    },
    ToolFinished {
        round: u32,
        call_id: String,
        name: String,
        success: bool,
        /// True only when the in-flight tool future was dropped before it
        /// returned. Ordinary tool errors are finished with `cancelled: false`
        /// and their real structured error output.
        cancelled: bool,
        /// Generic tools retain a bounded raw output preview. Commands use
        /// `command` instead of embedding minified JSON here.
        output: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        command: Option<CommandRuntimeResult>,
    },
    TurnFinished {
        success: bool,
        completion: String,
        tool_errors: u32,
    },
}

/// Native command invocation fields for automation frontends.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CommandRuntimeInvocation {
    pub argv: Vec<String>,
    pub cwd: String,
    pub timeout_ms: Option<u64>,
}

/// Native command completion fields for automation frontends.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CommandRuntimeResult {
    pub termination: CommandTermination,
    pub success: bool,
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub elapsed_ms: u64,
    pub stdout_bytes: u64,
    pub stderr_bytes: u64,
    pub stdout_retained_bytes: usize,
    pub stderr_retained_bytes: usize,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
    pub stdout_capture_complete: bool,
    pub stderr_capture_complete: bool,
    pub stdout_tail: String,
    pub stderr_tail: String,
    pub executable: CommandExecutableIdentity,
    pub ambient_authority: CommandAmbientAuthority,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact: Option<CommandOutputArtifact>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact_error: Option<String>,
}

type AgentEventObserver =
    std::sync::Arc<dyn Fn(&AgentRuntimeEvent) -> bool + Send + Sync + 'static>;

#[derive(Clone, Default)]
pub struct AgentEventLog {
    inner: std::sync::Arc<std::sync::Mutex<Vec<AgentRuntimeEvent>>>,
    observer: Option<AgentEventObserver>,
    abort_requested: std::sync::Arc<AtomicBool>,
}

impl std::fmt::Debug for AgentEventLog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentEventLog")
            .field("live_observer", &self.observer.is_some())
            .finish_non_exhaustive()
    }
}

impl AgentEventLog {
    pub fn with_observer<F>(observer: F) -> Self
    where
        F: Fn(&AgentRuntimeEvent) + Send + Sync + 'static,
    {
        Self::with_fallible_observer(move |event| {
            observer(event);
            true
        })
    }

    /// Attach a live observer that may stop the turn by returning `false`.
    ///
    /// This is used by durable automation sinks: once a JSONL record cannot be
    /// written, no later provider call or tool mutation should proceed without
    /// observable framing. The event remains retained for local diagnostics.
    pub fn with_fallible_observer<F>(observer: F) -> Self
    where
        F: Fn(&AgentRuntimeEvent) -> bool + Send + Sync + 'static,
    {
        Self {
            inner: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
            observer: Some(std::sync::Arc::new(observer)),
            abort_requested: std::sync::Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn record(&self, event: AgentRuntimeEvent) {
        self.inner
            .lock()
            .expect("agent event log")
            .push(event.clone());
        if let Some(observer) = &self.observer {
            if !observer(&event) {
                self.abort_requested.store(true, Ordering::Release);
            }
        }
    }

    pub fn take(&self) -> Vec<AgentRuntimeEvent> {
        std::mem::take(&mut *self.inner.lock().expect("agent event log"))
    }

    pub fn has_live_observer(&self) -> bool {
        self.observer.is_some()
    }

    pub fn abort_requested(&self) -> bool {
        self.abort_requested.load(Ordering::Acquire)
    }
}

fn abort_if_event_sink_failed(cfg: &AgentConfig) -> Result<(), AgentError> {
    if cfg
        .event_log
        .as_ref()
        .is_some_and(AgentEventLog::abort_requested)
    {
        Err(AgentError::Loop(
            "runtime event sink closed; stopped before further provider or tool work".to_string(),
        ))
    } else {
        Ok(())
    }
}

fn provider_telemetry_sink(
    log: Option<&AgentEventLog>,
    round: u32,
) -> Option<ProviderTelemetrySink> {
    log.map(|log| {
        let log = log.clone();
        ProviderTelemetrySink::new(move |event| {
            log.record(AgentRuntimeEvent::ProviderAttempt {
                round,
                attempt: event.attempt,
                max_attempts: event.max_attempts,
                category: event.category,
                status: event.status,
                outcome: event.outcome,
                terminal: event.terminal,
                retry_delay_ms: event.retry_delay_ms,
                retry_after_used: event.retry_after_used,
                remaining_deadline_ms: event.remaining_deadline_ms,
            });
        })
    })
}

/// Pairs every recorded [`AgentRuntimeEvent::ToolStarted`] with exactly one
/// terminal [`AgentRuntimeEvent::ToolFinished`], even when the outer agent turn
/// is cancelled and drops an in-flight tool future. Normal successes and tool
/// errors explicitly finish/disarm the guard; only an early drop is reported as
/// cancellation.
struct ToolEventGuard {
    pending: Option<PendingToolEvent>,
}

struct PendingToolEvent {
    log: AgentEventLog,
    round: u32,
    call_id: String,
    name: String,
    command: Option<CommandRuntimeInvocation>,
    started: std::time::Instant,
}

impl ToolEventGuard {
    fn start(
        log: Option<&AgentEventLog>,
        round: u32,
        call_id: &str,
        name: &str,
        args: &str,
    ) -> Self {
        let pending = log.map(|log| {
            let command = command_runtime_invocation(name, args);
            log.record(AgentRuntimeEvent::ToolStarted {
                round,
                call_id: call_id.to_string(),
                name: name.to_string(),
                args: if command.is_some() {
                    "[structured command invocation]".to_string()
                } else {
                    event_preview(args)
                },
                command: command.clone(),
            });
            PendingToolEvent {
                log: log.clone(),
                round,
                call_id: call_id.to_string(),
                name: name.to_string(),
                command,
                started: std::time::Instant::now(),
            }
        });
        Self { pending }
    }

    fn finish(mut self, success: bool, output: &str) {
        if let Some(pending) = self.pending.take() {
            let command = pending
                .command
                .as_ref()
                .and_then(|_| command_runtime_result(output));
            pending.log.record(AgentRuntimeEvent::ToolFinished {
                round: pending.round,
                call_id: pending.call_id,
                name: pending.name,
                success,
                cancelled: false,
                output: if command.is_some() {
                    "[structured command result]".to_string()
                } else {
                    event_preview(output)
                },
                command,
            });
        }
    }
}

impl Drop for ToolEventGuard {
    fn drop(&mut self) {
        if let Some(pending) = self.pending.take() {
            pending.log.record(AgentRuntimeEvent::ToolFinished {
                round: pending.round,
                call_id: pending.call_id,
                name: pending.name,
                success: false,
                cancelled: true,
                output: "[cancelled: tool execution interrupted]".to_string(),
                command: pending.command.as_ref().map(|invocation| {
                    let executable_name = invocation.argv.first().cloned().unwrap_or_default();
                    let executable_path = std::path::Path::new(&executable_name);
                    let executable_resolution = if executable_path.is_absolute()
                        || executable_path.components().count() > 1
                    {
                        CommandExecutableResolution::InvocationPath
                    } else {
                        CommandExecutableResolution::AmbientPath
                    };
                    CommandRuntimeResult {
                        termination: CommandTermination::Cancelled,
                        success: false,
                        exit_code: None,
                        signal: None,
                        elapsed_ms: pending
                            .started
                            .elapsed()
                            .as_millis()
                            .min(u128::from(u64::MAX)) as u64,
                        stdout_bytes: 0,
                        stderr_bytes: 0,
                        stdout_retained_bytes: 0,
                        stderr_retained_bytes: 0,
                        stdout_truncated: false,
                        stderr_truncated: false,
                        stdout_capture_complete: false,
                        stderr_capture_complete: false,
                        stdout_tail: String::new(),
                        stderr_tail: String::new(),
                        executable: CommandExecutableIdentity {
                            invocation: executable_name,
                            resolution: executable_resolution,
                            observed_path: None,
                            pinned: false,
                        },
                        ambient_authority: CommandAmbientAuthority::CURRENT,
                        artifact: None,
                        artifact_error: Some(
                            "command cancelled; unfinished full-output artifact was not published \
                             and cleanup was requested"
                                .to_string(),
                        ),
                    }
                }),
            });
        }
    }
}

/// Strategy for reclaiming context when trimming tool results isn't enough.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CompactionMode {
    /// Drop the oldest whole exchanges (lossy but free). The safe default.
    #[default]
    Trim,
    /// Summarize the oldest exchanges into a note via one LLM call (opt-in).
    Summarize,
}

/// Why a context compaction ran. Kept separate from [`CompactionMode`] so a UI
/// can distinguish an automatic pressure response from an explicit `/compact`
/// without trying to infer intent from the resulting message shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactionReason {
    ContextTargetExceeded,
    ManualRequest,
}

/// What an individual compaction actually did. This may differ from the
/// configured [`CompactionMode`]: summarization deliberately falls back to a
/// local trim when the summary request/response is unusable or cannot fit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactionOutcome {
    ToolResultsElided,
    Trimmed,
    Summarized,
    SummaryFallbackTrim,
}

/// Why a requested summary used the trim fallback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SummaryFallbackReason {
    /// The per-session circuit breaker has disabled further summary calls.
    CircuitBreaker,
    /// Even a locally shortened summary prompt could not fit the model window.
    RequestTooLarge,
    /// The provider failed or returned no usable summary text.
    ProviderFailure,
    /// The summary stream produced partial text but did not terminate with a
    /// complete `stop` finish (for example length truncation, a tool-call finish,
    /// or EOF without a terminal finish event).
    IncompleteResponse,
    /// No non-empty summary could coexist with the protected recent context.
    ResponseTooLarge,
    /// A real context window was unavailable, so a bounded summary call could
    /// not be proved safe.
    ContextWindowUnknown,
}

/// One durable, machine-readable account of a context compaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactionRecord {
    /// Monotonic, session-local sequence assigned by [`CompactionLedger`].
    pub sequence: u64,
    pub reason: CompactionReason,
    pub configured_mode: CompactionMode,
    pub outcome: CompactionOutcome,
    pub fallback_reason: Option<SummaryFallbackReason>,
    pub tokens_before: u64,
    pub tokens_after: u64,
    pub messages_before: usize,
    pub messages_after: usize,
    pub tool_results_elided: u32,
    pub exchanges_removed: u32,
    /// The summarizer saw a bounded head/tail view rather than the whole old
    /// transcript. This is provenance, not an error: the provider request was
    /// deliberately shortened locally to honor its own context invariant.
    pub summary_input_shortened: bool,
    /// The returned summary was shortened locally before it was admitted into
    /// the rebuilt main conversation.
    pub summary_output_shortened: bool,
}

/// Persistent per-session compaction telemetry. The cumulative counters never
/// lose information; detailed records are bounded so a very long chat cannot
/// grow state without limit.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CompactionLedger {
    pub total_compactions: u64,
    pub automatic_compactions: u64,
    pub manual_compactions: u64,
    pub summaries: u64,
    pub trims: u64,
    pub tool_results_elided: u64,
    pub exchanges_removed: u64,
    pub tokens_saved: u64,
    pub omitted_records: u64,
    pub records: Vec<CompactionRecord>,
}

impl CompactionLedger {
    const RECORD_LIMIT: usize = 64;

    fn push(&mut self, mut record: CompactionRecord) -> CompactionRecord {
        self.total_compactions = self.total_compactions.saturating_add(1);
        record.sequence = self.total_compactions;
        match record.reason {
            CompactionReason::ContextTargetExceeded => {
                self.automatic_compactions = self.automatic_compactions.saturating_add(1);
            }
            CompactionReason::ManualRequest => {
                self.manual_compactions = self.manual_compactions.saturating_add(1);
            }
        }
        match record.outcome {
            CompactionOutcome::Summarized => {
                self.summaries = self.summaries.saturating_add(1);
            }
            CompactionOutcome::Trimmed | CompactionOutcome::SummaryFallbackTrim => {
                self.trims = self.trims.saturating_add(1);
            }
            CompactionOutcome::ToolResultsElided => {}
        }
        self.tool_results_elided = self
            .tool_results_elided
            .saturating_add(u64::from(record.tool_results_elided));
        self.exchanges_removed = self
            .exchanges_removed
            .saturating_add(u64::from(record.exchanges_removed));
        self.tokens_saved = self
            .tokens_saved
            .saturating_add(record.tokens_before.saturating_sub(record.tokens_after));
        if self.records.len() == Self::RECORD_LIMIT {
            self.records.remove(0);
            self.omitted_records = self.omitted_records.saturating_add(1);
        }
        self.records.push(record.clone());
        record
    }

    pub fn last(&self) -> Option<&CompactionRecord> {
        self.records.last()
    }
}

/// Result returned by the public manual-compaction API.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactionReport {
    pub changed: bool,
    pub target_tokens: u64,
    pub tokens_before: u64,
    pub tokens_after: u64,
    pub record: Option<CompactionRecord>,
    /// Provider usage incurred by an optional summarizer call.
    pub usage: kitsu_providers::Usage,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            model: String::new(),
            max_rounds: 10,
            max_context_bytes: 128 * 1024,
            context_window: None,
            offload_dir: None,
            stream: true,
            temperature: None,
            max_tokens: None,
            compaction: CompactionMode::Trim,
            quiet: false,
            event_log: None,
        }
    }
}

#[derive(Debug, Error)]
pub enum AgentError {
    #[error(transparent)]
    Provider(#[from] ProviderError),

    #[error("agent loop error: {0}")]
    Loop(String),

    #[error(
        "context preflight stopped an oversized provider request: protected prompt needs about {required} {unit}, but the configured target is {target} {unit}; shorten the latest request or system instructions, disable large tool schemas, lower max output tokens, clear/compact history, or select a model with a larger context window"
    )]
    ContextOverflow {
        required: u64,
        target: u64,
        unit: ContextUnit,
        /// The resolved model window for token checks; `None` identifies the
        /// coarse byte-budget fallback used when model metadata is unavailable.
        window: Option<u64>,
    },

    #[error(
        "manual compaction needs a resolved model context window; configure context_window or select a model whose window Kitsu can discover"
    )]
    ContextWindowUnknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextUnit {
    Tokens,
    Bytes,
}

impl std::fmt::Display for ContextUnit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Tokens => f.write_str("tokens"),
            Self::Bytes => f.write_str("bytes"),
        }
    }
}

/// Per-session state that should persist across multiple calls to
/// [`run_agent_turn_with_state`] within one multi-turn session (interactive
/// chat, a scripted run): the context meter's real-usage anchor, and the
/// compaction circuit breaker. Losing these between turns means every new turn
/// re-estimates the whole conversation from a cold chars/4 guess instead of the
/// gateway's real `prompt_tokens`, and a summarizer that's already proven broken
/// for this model/gateway gets re-tried from scratch (and re-burns its failure
/// budget) on every subsequent turn. Single-turn callers (`ask`, one-shot piped
/// input) can use `Default` via [`run_agent_turn`].
#[derive(Debug, Default, Clone)]
pub struct AgentSessionState {
    pub meter: ContextMeter,
    pub compact_failures: u32,
    pub compaction_ledger: CompactionLedger,
    /// Machine-readable outcome of the most recent turn. Interactive chat can
    /// still show partial/recovered answers, while strict scripted callers can
    /// reject incomplete automation without scraping display markers.
    pub last_turn: AgentTurnStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TurnCompletion {
    #[default]
    Complete,
    LengthLimited,
    MaxRounds,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AgentTurnStatus {
    pub completion: TurnCompletion,
    pub tool_errors: u32,
}

/// Run one user turn through the agent loop, with no state persisted across
/// turns (each call starts a fresh [`ContextMeter`] anchor and compaction
/// circuit breaker). Correct for single-turn callers; multi-turn callers
/// (interactive chat, scripted runs) should use [`run_agent_turn_with_state`] so
/// the meter's real-usage anchor and circuit breaker survive between turns.
pub async fn run_agent_turn<P>(
    provider: &P,
    tools: &Registry,
    messages: &mut Vec<ChatMessage>,
    cfg: &AgentConfig,
) -> Result<String, AgentError>
where
    P: Provider,
{
    run_agent_turn_with_state(
        provider,
        tools,
        messages,
        cfg,
        &mut AgentSessionState::default(),
    )
    .await
}

/// Run one user turn through the agent loop. `messages` is the full conversation
/// so far (system + history + latest user message). Returns the final assistant
/// text. Tools may be empty (degenerates to a single round — the MVP `ask` path).
/// `state` carries the context-meter anchor and compaction circuit breaker across
/// turns — see [`AgentSessionState`].
///
/// Generic over `P: Provider` so tests can pass a mock provider.
///
/// As a side effect, turn usage/context telemetry is written to stderr after the
/// turn. Terminals get a compact human summary; redirected output gets stable
/// `kitsu-usage:`/`kitsu-context:` records. Stdout carries only assistant text.
pub async fn run_agent_turn_with_state<P>(
    provider: &P,
    tools: &Registry,
    messages: &mut Vec<ChatMessage>,
    cfg: &AgentConfig,
    state: &mut AgentSessionState,
) -> Result<String, AgentError>
where
    P: Provider,
{
    let mut turn_usage = kitsu_providers::Usage::default();
    state.last_turn = AgentTurnStatus::default();
    let result = run_agent_turn_inner(provider, tools, messages, cfg, &mut turn_usage, state).await;
    if let Some(log) = &cfg.event_log {
        let completion = match state.last_turn.completion {
            TurnCompletion::Complete => "complete",
            TurnCompletion::LengthLimited => "length_limited",
            TurnCompletion::MaxRounds => "max_rounds",
        };
        log.record(AgentRuntimeEvent::TurnFinished {
            success: result.is_ok(),
            completion: completion.to_string(),
            tool_errors: state.last_turn.tool_errors,
        });
    }
    // The orchestrator owns assistant rendering and the end-of-turn boundary so
    // the answer always precedes telemetry in both streaming and buffered mode.
    if !cfg.quiet {
        if !cfg.stream {
            if let Ok(text) = &result {
                kitsu_render::print_message(text);
            }
        }
        kitsu_render::finish_line();
    }
    if !cfg.quiet {
        let usage = (!turn_usage.is_empty()).then_some((
            turn_usage.prompt_tokens,
            turn_usage.completion_tokens,
            turn_usage.total_tokens,
        ));
        let context = cfg.context_window.map(|window| {
            let budget = ContextBudget::new(window, cfg.max_tokens);
            (state.meter.estimate(messages), budget.effective, window)
        });
        kitsu_render::emit_turn_stats(usage, context);
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
    state: &mut AgentSessionState,
) -> Result<String, AgentError>
where
    P: Provider,
{
    let AgentSessionState {
        meter,
        compact_failures,
        compaction_ledger,
        last_turn: turn_status,
    } = state;
    let mut last_assistant_text = String::new();
    let mut round: u32 = 0;

    // ISSUE-007 loop-breaker: track consecutive identical tool-call failures so a
    // model that fixates on a failing call (e.g. a denied path) is forced to a
    // final answer instead of burning every round. Signature = tool_name + args.
    let mut consecutive_failures: std::collections::HashMap<String, u32> =
        std::collections::HashMap::new();
    let mut force_final_next = false;
    const REPEAT_FAIL_LIMIT: u32 = 2;

    // The tool set is fixed for the whole turn (every round advertises the same
    // descriptors), so serialize the schema once here rather than redoing it on
    // every round inside enforce_context_budget.
    let schema_json = serde_json::to_string(tools.descriptors()).unwrap_or_default();

    loop {
        round += 1;
        if round > cfg.max_rounds {
            // (2) max-rounds backstop: stop gracefully, note it, return text.
            last_assistant_text.push_str("\n[stopped: max tool rounds reached]");
            turn_status.completion = TurnCompletion::MaxRounds;
            return Ok(last_assistant_text);
        }

        // On the FINAL permitted round, withhold tools and inject a nudge so the
        // model must produce its final answer instead of emitting yet another
        // tool call that we'd truncate to "[stopped: max tool rounds reached]".
        // This is what turned tool-aware reviews of large repos from 36-byte
        // stubs into real reviews (robustness fix). A round budget of
        // 1 has no "earlier" round to nudge from, so only applies when >1.
        // Final round when: the round budget is exhausted, OR a tool call has
        // failed identically REPEAT_FAIL_LIMIT times (ISSUE-007 loop-breaker).
        let is_final_round = (round == cfg.max_rounds && cfg.max_rounds > 1) || force_final_next;
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

        // Budget only after every request-visible mutation (notably the final
        // round nudge above), and against the schema actually offered THIS
        // round. This establishes the invariant the concrete request below is
        // required to satisfy.
        enforce_context_budget_recording(
            provider,
            messages,
            cfg,
            &schema_json,
            tools_were_offered,
            meter,
            turn_usage,
            compact_failures,
            compaction_ledger,
            CompactionReason::ContextTargetExceeded,
            false,
        )
        .await?;

        // Number of messages this request sends — the anchor point for the meter.
        let sent_len = messages.len();
        let req = ChatRequest {
            model: cfg.model.clone(),
            messages: messages.clone(),
            tools: offered_tools,
            stream: cfg.stream,
            temperature: cfg.temperature,
            max_tokens: cfg.max_tokens,
        };

        // Keep the final check adjacent to the provider boundary. If a future
        // edit adds another message or tool mutation between compaction and
        // request construction, this check fails locally instead of leaking an
        // oversized request to the gateway.
        ensure_chat_request_fits(&req, cfg, meter)?;
        if let Some(log) = &cfg.event_log {
            log.record(AgentRuntimeEvent::ProviderRequest {
                round,
                message_count: req.messages.len(),
                tools_offered: req.tools.as_ref().map_or(0, Vec::len),
            });
        }
        abort_if_event_sink_failed(cfg)?;

        // "Vibing kitsu" while we wait for the model. No-op off a TTY; cleared on
        // the first stream item (or on drop if the request errors).
        let mut spinner = if cfg.quiet {
            None
        } else {
            Some(kitsu_render::Spinner::start())
        };

        // 401/403 surface here (or as the first stream item) as a hard error.
        let mut stream = provider
            .chat_stream_with_telemetry(req, provider_telemetry_sink(cfg.event_log.as_ref(), round))
            .await?;
        abort_if_event_sink_failed(cfg)?;

        let mut assistant_text = String::new();
        let mut tool_calls: Vec<ToolCall> = Vec::new();
        let mut finish = FinishReason::Stop;

        while let Some(item) = stream.next().await {
            if let Some(s) = spinner.take() {
                s.stop();
            }
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
                ChatDelta::Usage(u) => {
                    // Anchor the meter on this response's real prompt token
                    // count. `tools_were_offered` records whether THIS request
                    // included the schema, so a later round can tell whether the
                    // anchor already accounts for it (see
                    // ContextMeter::anchor_includes_schema).
                    meter.record(u.prompt_tokens, sent_len, tools_were_offered);
                    turn_usage.add(u);
                    if let Some(log) = &cfg.event_log {
                        log.record(AgentRuntimeEvent::Usage {
                            round,
                            prompt_tokens: u.prompt_tokens,
                            completion_tokens: u.completion_tokens,
                            total_tokens: u.total_tokens,
                        });
                    }
                    abort_if_event_sink_failed(cfg)?;
                }
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
                tracing::debug!(
                    count = recovered.len(),
                    "recovered text-protocol tool call(s)"
                );
                tool_calls.extend(recovered);
                synthesized_call = true;
            }
        }

        // ISSUE-002 / ISSUE-008: this round's text was buffered (tools were
        // offered). Print it now UNLESS it was a text-protocol tool call we
        // synthesized — that text IS the raw JSON, so suppress it. A model's real
        // reasoning before a STRUCTURED tool call, and terminal answers, print
        // normally (previously over-suppressed, hiding read/grep output).
        if cfg.stream && tools_were_offered && !synthesized_call {
            kitsu_render::print_message(&assistant_text);
        }

        last_assistant_text = assistant_text.clone();

        // Record the assistant turn. When we synthesized a call from text, store
        // empty content so the raw tool-call JSON doesn't pollute the history.
        let assistant_content = if synthesized_call {
            String::new()
        } else {
            assistant_text.clone()
        };
        messages.push(ChatMessage::assistant(
            assistant_content,
            tool_calls.clone(),
        ));

        if tool_calls.is_empty() {
            // (1) terminal: model stopped with no tool calls.
            match finish {
                FinishReason::Stop => return Ok(assistant_text),
                // Truncated by the model's output cap — surface it instead of
                // letting a cut-off answer masquerade as a complete one.
                FinishReason::Length => {
                    tracing::warn!("response truncated: model hit its output length limit");
                    assistant_text.push_str("\n[truncated: model output length limit reached]");
                    turn_status.completion = TurnCompletion::LengthLimited;
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
            turn_status.completion = TurnCompletion::LengthLimited;
            tracing::warn!(
                "tool-call round truncated by the model's output length limit; \
                 tool arguments may be incomplete"
            );
        }

        // Tool calls present: run each, feed result (or error) back as a tool msg.
        for tc in tool_calls {
            // Narrate the action (Claude-Code-style), e.g. `⏺ Read(deck1.md)`.
            if !cfg.quiet {
                kitsu_render::tool_activity(&format_tool_header(&tc.tool_name, &tc.args));
            }
            let tool_event = ToolEventGuard::start(
                cfg.event_log.as_ref(),
                round,
                &tc.call_id,
                &tc.tool_name,
                &tc.args,
            );
            abort_if_event_sink_failed(cfg)?;
            let sig = format!("{}\u{0}{}", tc.tool_name, tc.args);
            let (content, is_err) = match tools.run(&tc.tool_name, tc.args.clone()).await {
                Ok(out) => {
                    consecutive_failures.remove(&sig); // a success resets the streak
                    (out, false)
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
                    turn_status.tool_errors = turn_status.tool_errors.saturating_add(1);
                    (
                        serde_json::json!({ "error": e.kind(), "detail": e.to_string() })
                            .to_string(),
                        true,
                    )
                }
            };
            if !cfg.quiet {
                kitsu_render::tool_result(&format_tool_result(&tc.tool_name, &content, is_err));
            }
            tool_event.finish(!is_err, &content);
            abort_if_event_sink_failed(cfg)?;
            // Offload a large successful result to disk, leaving a re-readable
            // pointer, so one big result can't dominate a small window (Stage 2).
            let content = if is_err {
                content
            } else {
                maybe_offload_for_tools(content, &tc.call_id, cfg.offload_dir.as_deref(), tools)
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
        let Some(obj) = value.as_object() else {
            continue;
        };
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

/// Friendly, compact activity header for a tool call — e.g.
/// `Read(deck1.md)`, `Write(fb.py)`, `Grep("TODO", src)`, `List(scratch)`.
/// Falls back to the raw tool name if args don't parse.
fn event_preview(value: &str) -> String {
    const LIMIT: usize = 8 * 1024;
    if value.len() <= LIMIT {
        return value.to_string();
    }
    let mut boundary = LIMIT;
    while boundary > 0 && !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    format!("{}\n[truncated event payload]", &value[..boundary])
}

fn command_runtime_invocation(name: &str, args: &str) -> Option<CommandRuntimeInvocation> {
    if name != "command_exec" {
        return None;
    }
    let args = serde_json::from_str::<CommandExecArgs>(args).ok()?;
    Some(CommandRuntimeInvocation {
        argv: args.argv,
        cwd: args.cwd,
        timeout_ms: args.timeout_ms,
    })
}

fn command_runtime_result(output: &str) -> Option<CommandRuntimeResult> {
    let output = serde_json::from_str::<CommandOutput>(output).ok()?;
    Some(CommandRuntimeResult {
        termination: output.termination,
        success: output.success,
        exit_code: output.exit_code,
        signal: output.signal,
        elapsed_ms: output.elapsed_ms,
        stdout_bytes: output.stdout_bytes,
        stderr_bytes: output.stderr_bytes,
        stdout_retained_bytes: output.stdout_retained_bytes,
        stderr_retained_bytes: output.stderr_retained_bytes,
        stdout_truncated: output.stdout_truncated,
        stderr_truncated: output.stderr_truncated,
        stdout_capture_complete: output.stdout_capture_complete,
        stderr_capture_complete: output.stderr_capture_complete,
        stdout_tail: terminal_safe_tail(&output.stdout, 1024),
        stderr_tail: terminal_safe_tail(&output.stderr, 1024),
        executable: output.executable,
        ambient_authority: output.ambient_authority,
        artifact: output.artifact,
        artifact_error: output.artifact_error,
    })
}

fn terminal_safe_inline(value: &str) -> String {
    let clean = kitsu_render::sanitize_terminal_text(value);
    let mut output = String::with_capacity(clean.len());
    for character in clean.chars() {
        match character {
            '\n' => output.push_str("\\n"),
            '\t' => output.push_str("\\t"),
            '\r' => output.push_str("\\r"),
            character => output.push(character),
        }
    }
    output
}

fn terminal_safe_tail(value: &str, max_chars: usize) -> String {
    let safe = terminal_safe_inline(value);
    let chars = safe.chars().collect::<Vec<_>>();
    if chars.len() <= max_chars {
        return safe;
    }
    format!(
        "...{}",
        chars[chars.len() - max_chars..].iter().collect::<String>()
    )
}

fn bounded_human_value(value: &str, max_chars: usize) -> String {
    let safe = terminal_safe_inline(value);
    let mut chars = safe.chars();
    let head = chars.by_ref().take(max_chars).collect::<String>();
    if chars.next().is_some() {
        format!("{head}...")
    } else {
        head
    }
}

fn format_tool_header(name: &str, args: &str) -> String {
    if name == "command_exec" {
        return serde_json::from_str::<CommandExecArgs>(args)
            .ok()
            .map(|args| {
                let argv = bounded_human_value(&args.display_argv(), 320);
                let cwd = bounded_human_value(&args.cwd, 240);
                format!("Command(argv={argv}; cwd={cwd:?})")
            })
            .unwrap_or_else(|| "Command(invalid arguments)".to_string());
    }
    let friendly = match name {
        "fs_read_file" => "Read",
        "fs_list_dir" => "List",
        "repo_grep" => "Grep",
        "fs_write_file" => "Write",
        "fs_edit_file" => "Edit",
        other => other,
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(args) else {
        return friendly.to_string();
    };
    if name == "repo_grep" {
        let pat = v.get("pattern").and_then(|x| x.as_str()).unwrap_or("");
        let path = v
            .get("path")
            .and_then(|x| x.as_str())
            .map(short_path)
            .unwrap_or_default();
        return format!("{friendly}(\"{pat}\", {path})");
    }
    match v.get("path").and_then(|x| x.as_str()) {
        Some(p) => format!("{friendly}({})", short_path(p)),
        None => friendly.to_string(),
    }
}

/// Result summary shown under a tool call — bytes for writes, matches for grep,
/// lines otherwise; the error kind on failure.
fn format_tool_result(name: &str, content: &str, is_err: bool) -> String {
    if is_err {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(content) {
            if let Some(kind) = v.get("error").and_then(|x| x.as_str()) {
                let detail = v
                    .get("detail")
                    .and_then(|value| value.as_str())
                    .unwrap_or("")
                    .lines()
                    .next()
                    .unwrap_or("");
                let detail: String = detail.chars().take(180).collect();
                return if detail.is_empty() {
                    format!("error: {kind}")
                } else {
                    format!("error: {kind} — {detail}")
                };
            }
        }
        return "error".to_string();
    }
    match name {
        "command_exec" => serde_json::from_str::<CommandOutput>(content)
            .ok()
            .map(|output| format_command_result(&output))
            .unwrap_or_else(|| "command completed (invalid result metadata)".to_string()),
        "fs_write_file" | "fs_edit_file" => serde_json::from_str::<serde_json::Value>(content)
            .ok()
            .map(|value| {
                let bytes = value.get("bytes").and_then(|item| item.as_u64());
                let change = value
                    .get("change")
                    .and_then(|item| item.as_str())
                    .unwrap_or("written");
                let base = match bytes {
                    Some(bytes) => format!("{change} · {bytes} bytes"),
                    None => change.to_string(),
                };
                match value.get("warning").and_then(|item| item.as_str()) {
                    Some(warning) => {
                        format!("{base} — warning: {}", safe_warning_summary(warning))
                    }
                    None => base,
                }
            })
            .unwrap_or_else(|| "written".to_string()),
        "repo_grep" => {
            if content == "(no matches)" {
                "no matches".to_string()
            } else {
                let n = content.lines().count();
                format!("{n} match{}", if n == 1 { "" } else { "es" })
            }
        }
        _ => {
            let n = content.lines().count();
            format!("{n} line{}", if n == 1 { "" } else { "s" })
        }
    }
}

fn format_command_result(output: &CommandOutput) -> String {
    let completion = match output.termination {
        CommandTermination::Exited => match output.exit_code {
            Some(code) => format!("exit {code}"),
            None => "exited".to_string(),
        },
        CommandTermination::Signaled => match output.signal {
            Some(signal) => format!("signal {signal}"),
            None => "signaled".to_string(),
        },
        CommandTermination::TimedOut => match output.signal {
            Some(signal) => format!("timeout (signal {signal})"),
            None => "timeout".to_string(),
        },
        CommandTermination::Cancelled => "cancelled".to_string(),
    };
    let mut fields = vec![completion, format!("{} ms", output.elapsed_ms)];
    append_command_stream_summary(
        &mut fields,
        "stdout",
        &output.stdout,
        output.stdout_bytes,
        output.stdout_retained_bytes,
        output.stdout_truncated,
        output.stdout_capture_complete,
    );
    append_command_stream_summary(
        &mut fields,
        "stderr",
        &output.stderr,
        output.stderr_bytes,
        output.stderr_retained_bytes,
        output.stderr_truncated,
        output.stderr_capture_complete,
    );
    if let Some(artifact) = &output.artifact {
        fields.push(format!(
            "full output={:?}",
            bounded_human_value(&artifact.reference, 160)
        ));
    }
    if let Some(error) = &output.artifact_error {
        fields.push(format!(
            "artifact error={:?}",
            bounded_human_value(error, 160)
        ));
    }
    fields.join(" | ")
}

fn append_command_stream_summary(
    fields: &mut Vec<String>,
    name: &str,
    preview: &str,
    total_bytes: u64,
    retained_bytes: usize,
    truncated: bool,
    complete: bool,
) {
    if total_bytes == 0 && complete {
        return;
    }
    let mut metadata = if truncated {
        format!("{name} {retained_bytes}/{total_bytes} B")
    } else {
        format!("{name} {total_bytes} B")
    };
    if truncated {
        metadata.push_str(" truncated");
    }
    if !complete {
        metadata.push_str(" incomplete");
    }
    if !preview.is_empty() {
        metadata.push_str(" tail=");
        metadata.push_str(&format!("{:?}", terminal_safe_tail(preview, 180)));
    }
    fields.push(metadata);
}

fn safe_warning_summary(warning: &str) -> String {
    const CHAT_TEMPLATE_MARKERS: [&str; 8] = [
        "<|im_start|>",
        "<|im_end|>",
        "<|start_header_id|>",
        "<|end_header_id|>",
        "<|eot_id|>",
        "[INST]",
        "[/INST]",
        "<tool_call>",
    ];
    let mut safe = terminal_safe_inline(warning);
    for marker in CHAT_TEMPLATE_MARKERS {
        safe = safe.replace(marker, "[chat-template marker]");
    }
    bounded_human_value(&safe, 180)
}

/// Compact a path to at most its last two segments (with a leading `…` when more
/// were dropped), so activity lines stay short.
fn short_path(p: &str) -> String {
    let parts: Vec<&str> = p
        .trim_end_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();
    match parts.as_slice() {
        [] => p.to_string(),
        [one] => (*one).to_string(),
        [.., a, b] => {
            let tail = format!("{a}/{b}");
            if parts.len() > 2 {
                format!("…/{tail}")
            } else {
                tail
            }
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct RequestMeasurement {
    used: u64,
    target: u64,
    window: Option<u64>,
    unit: ContextUnit,
}

fn structural_request_tokens(messages: &[ChatMessage], schema_json: &str, has_schema: bool) -> u64 {
    messages.iter().map(message_tokens).sum::<u64>()
        + if has_schema {
            estimate_str(schema_json)
        } else {
            0
        }
}

fn structural_request_bytes(messages: &[ChatMessage], schema_json: &str, has_schema: bool) -> u64 {
    messages.iter().map(|m| m.byte_size() as u64).sum::<u64>()
        + if has_schema {
            schema_json.len() as u64
        } else {
            0
        }
}

fn request_measurement(
    messages: &[ChatMessage],
    cfg: &AgentConfig,
    schema_json: &str,
    has_schema: bool,
    max_tokens: Option<u32>,
    meter: &ContextMeter,
) -> RequestMeasurement {
    if let Some(window) = cfg.context_window {
        let schema_tokens = if has_schema {
            estimate_str(schema_json)
        } else {
            0
        };
        // A valid real-usage anchor is stronger than a cold chars/token pass.
        // Add schemas only if that exact anchored request did not already carry
        // them. Any mutation of covered messages invalidates the anchor first.
        let used = meter.estimate(messages)
            + if meter.anchor_includes_schema(messages) {
                0
            } else {
                schema_tokens
            };
        RequestMeasurement {
            used,
            target: ContextBudget::new(window, max_tokens).target,
            window: Some(window),
            unit: ContextUnit::Tokens,
        }
    } else {
        let schema_bytes = if has_schema {
            schema_json.len() as u64
        } else {
            0
        };
        RequestMeasurement {
            used: messages.iter().map(|m| m.byte_size() as u64).sum::<u64>() + schema_bytes,
            target: cfg.max_context_bytes as u64,
            window: None,
            unit: ContextUnit::Bytes,
        }
    }
}

fn check_measurement(measurement: RequestMeasurement) -> Result<RequestMeasurement, AgentError> {
    if measurement.used > measurement.target {
        return Err(AgentError::ContextOverflow {
            required: measurement.used,
            target: measurement.target,
            unit: measurement.unit,
            window: measurement.window,
        });
    }
    Ok(measurement)
}

/// The last-line guard used at every provider boundary in this module.
fn ensure_chat_request_fits(
    req: &ChatRequest,
    cfg: &AgentConfig,
    meter: &ContextMeter,
) -> Result<RequestMeasurement, AgentError> {
    let has_schema = req.tools.as_ref().is_some_and(|tools| !tools.is_empty());
    let schema_json = req
        .tools
        .as_ref()
        .map(|tools| serde_json::to_string(tools).unwrap_or_default())
        .unwrap_or_default();
    check_measurement(request_measurement(
        &req.messages,
        cfg,
        &schema_json,
        has_schema,
        req.max_tokens,
        meter,
    ))
}

/// Compact eligible old exchanges immediately, even when the conversation is
/// currently below the automatic threshold. This is the core entry point for a
/// `/compact` UI. The same preflight used by normal turns is applied after the
/// manual operation, and summarization is never attempted without a resolved
/// window that can bound its own provider request.
pub async fn compact_context<P>(
    provider: &P,
    tools: &Registry,
    messages: &mut Vec<ChatMessage>,
    cfg: &AgentConfig,
    state: &mut AgentSessionState,
) -> Result<CompactionReport, AgentError>
where
    P: Provider,
{
    if cfg.context_window.is_none() {
        return Err(AgentError::ContextWindowUnknown);
    }
    let has_schema = !tools.descriptors().is_empty();
    let schema_json = serde_json::to_string(tools.descriptors()).unwrap_or_default();
    let mut usage = kitsu_providers::Usage::default();
    enforce_context_budget_recording(
        provider,
        messages,
        cfg,
        &schema_json,
        has_schema,
        &mut state.meter,
        &mut usage,
        &mut state.compact_failures,
        &mut state.compaction_ledger,
        CompactionReason::ManualRequest,
        true,
    )
    .await
}

/// Test-facing wrapper that exercises the production preflight without needing
/// to construct a persistent ledger.
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
async fn enforce_context_budget<P>(
    provider: &P,
    messages: &mut Vec<ChatMessage>,
    cfg: &AgentConfig,
    schema_json: &str,
    has_schema: bool,
    meter: &mut ContextMeter,
    turn_usage: &mut kitsu_providers::Usage,
    compact_failures: &mut u32,
) -> Result<CompactionReport, AgentError>
where
    P: Provider,
{
    enforce_context_budget_recording(
        provider,
        messages,
        cfg,
        schema_json,
        has_schema,
        meter,
        turn_usage,
        compact_failures,
        &mut CompactionLedger::default(),
        CompactionReason::ContextTargetExceeded,
        false,
    )
    .await
}

#[derive(Debug, Default)]
struct CompactionProgress {
    outcome: Option<CompactionOutcome>,
    fallback_reason: Option<SummaryFallbackReason>,
    tool_results_elided: u32,
    exchanges_removed: u32,
    summary_input_shortened: bool,
    summary_output_shortened: bool,
}

/// Keep the conversation within budget before sending (design §3, Stage 1c/2/3)
/// and record every actual reshape in the session ledger. On return `Ok`, the
/// exact next request shape is at or below its configured target. On failure no
/// provider request is made by the caller.
#[allow(clippy::too_many_arguments)]
async fn enforce_context_budget_recording<P>(
    provider: &P,
    messages: &mut Vec<ChatMessage>,
    cfg: &AgentConfig,
    schema_json: &str,
    has_schema: bool,
    meter: &mut ContextMeter,
    turn_usage: &mut kitsu_providers::Usage,
    compact_failures: &mut u32,
    ledger: &mut CompactionLedger,
    reason: CompactionReason,
    force: bool,
) -> Result<CompactionReport, AgentError>
where
    P: Provider,
{
    let token_before = structural_request_tokens(messages, schema_json, has_schema);
    let messages_before = messages.len();
    let initial = request_measurement(
        messages,
        cfg,
        schema_json,
        has_schema,
        cfg.max_tokens,
        meter,
    );
    let mut progress = CompactionProgress::default();

    if cfg.context_window.is_none() {
        // With no model window, preserve the historical coarse byte guard. It is
        // still a checked invariant: elide, then drop whole exchanges, then fail
        // locally if protected content alone exceeds the configured cap.
        if initial.used > initial.target {
            let (after_elide, elided) =
                trim_tool_results_counted(messages, initial.used, initial.target, |m| {
                    m.byte_size() as u64
                });
            if elided > 0 {
                meter.invalidate();
                progress.tool_results_elided = elided;
                progress.outcome = Some(CompactionOutcome::ToolResultsElided);
            }
            if after_elide > initial.target {
                let mut current = after_elide;
                let mut removed_total = 0u32;
                loop {
                    let removed = drop_old_exchanges_detailed(
                        messages,
                        current,
                        initial.target,
                        |m| m.byte_size() as u64,
                        false,
                    );
                    if removed == 0 {
                        break;
                    }
                    meter.invalidate();
                    removed_total = removed_total.saturating_add(removed);
                    current = structural_request_bytes(messages, schema_json, has_schema);
                    if current <= initial.target {
                        break;
                    }
                }
                if removed_total > 0 {
                    progress.exchanges_removed = removed_total;
                    progress.outcome = Some(if cfg.compaction == CompactionMode::Summarize {
                        progress.fallback_reason =
                            Some(SummaryFallbackReason::ContextWindowUnknown);
                        CompactionOutcome::SummaryFallbackTrim
                    } else {
                        CompactionOutcome::Trimmed
                    });
                }
            }
        }
        let final_measurement = request_measurement(
            messages,
            cfg,
            schema_json,
            has_schema,
            cfg.max_tokens,
            meter,
        );
        let record = record_progress(
            ledger,
            cfg,
            reason,
            &progress,
            token_before,
            structural_request_tokens(messages, schema_json, has_schema),
            messages_before,
            messages.len(),
        );
        check_measurement(final_measurement)?;
        return Ok(CompactionReport {
            changed: record.is_some(),
            target_tokens: 0,
            tokens_before: token_before,
            tokens_after: structural_request_tokens(messages, schema_json, has_schema),
            record,
            usage: *turn_usage,
        });
    }

    let target = initial.target;
    if !force && initial.used <= target {
        return Ok(CompactionReport {
            changed: false,
            target_tokens: target,
            tokens_before: token_before,
            tokens_after: token_before,
            record: None,
            usage: *turn_usage,
        });
    }

    let mut running = initial.used;
    // Automatic pressure first reclaims old tool bodies cheaply. Explicit
    // compaction goes straight to eligible whole exchanges so it is
    // deterministic below the threshold and does not damage protected recent
    // tool context merely to manufacture savings.
    if !force {
        let (after_elide, elided) =
            trim_tool_results_counted(messages, running, target, message_tokens);
        running = after_elide;
        if elided > 0 {
            meter.invalidate();
            progress.tool_results_elided = elided;
            progress.outcome = Some(CompactionOutcome::ToolResultsElided);
            // Once covered content changes, use one consistent cold estimate for
            // all subsequent stages and the final adjacent preflight.
            running = structural_request_tokens(messages, schema_json, has_schema);
        }
    }

    if force || running > target {
        match cfg.compaction {
            CompactionMode::Trim => {
                let mut current = running;
                let mut removed_total = 0u32;
                loop {
                    let removed = drop_old_exchanges_detailed(
                        messages,
                        current,
                        target,
                        message_tokens,
                        force,
                    );
                    if removed == 0 {
                        break;
                    }
                    meter.invalidate();
                    removed_total = removed_total.saturating_add(removed);
                    current = structural_request_tokens(messages, schema_json, has_schema);
                    if force || current <= target {
                        break;
                    }
                }
                if removed_total > 0 {
                    progress.exchanges_removed = removed_total;
                    progress.outcome = Some(CompactionOutcome::Trimmed);
                }
            }
            CompactionMode::Summarize => {
                summarize_or_fallback(
                    provider,
                    messages,
                    cfg,
                    schema_json,
                    has_schema,
                    target,
                    compact_failures,
                    turn_usage,
                    meter,
                    &mut progress,
                )
                .await;
            }
        }
    }

    let token_after = structural_request_tokens(messages, schema_json, has_schema);
    let record = record_progress(
        ledger,
        cfg,
        reason,
        &progress,
        token_before,
        token_after,
        messages_before,
        messages.len(),
    );
    check_measurement(request_measurement(
        messages,
        cfg,
        schema_json,
        has_schema,
        cfg.max_tokens,
        meter,
    ))?;
    Ok(CompactionReport {
        changed: record.is_some(),
        target_tokens: target,
        tokens_before: token_before,
        tokens_after: token_after,
        record,
        usage: *turn_usage,
    })
}

#[allow(clippy::too_many_arguments)]
fn record_progress(
    ledger: &mut CompactionLedger,
    cfg: &AgentConfig,
    reason: CompactionReason,
    progress: &CompactionProgress,
    tokens_before: u64,
    tokens_after: u64,
    messages_before: usize,
    messages_after: usize,
) -> Option<CompactionRecord> {
    let outcome = progress.outcome?;
    Some(ledger.push(CompactionRecord {
        sequence: 0,
        reason,
        configured_mode: cfg.compaction,
        outcome,
        fallback_reason: progress.fallback_reason,
        tokens_before,
        tokens_after,
        messages_before,
        messages_after,
        tool_results_elided: progress.tool_results_elided,
        exchanges_removed: progress.exchanges_removed,
        summary_input_shortened: progress.summary_input_shortened,
        summary_output_shortened: progress.summary_output_shortened,
    }))
}

/// Consecutive failed compactions after which we stop trying and just drop
/// (circuit breaker — a doomed summarize loop would burn API calls).
const MAX_COMPACT_FAILURES: u32 = 3;
const DEFAULT_SUMMARY_MAX_TOKENS: u32 = 1_024;
const SUMMARY_PROMPT: &str = "Summarize the conversation above so it can continue after the \
older messages are dropped. Be concise but preserve everything needed to keep working. Cover: \
(1) the user's primary request and intent; (2) files and code touched — paths, what changed, and \
why; (3) key decisions, errors, and their fixes; (4) pending tasks; (5) what was being worked on \
most recently; (6) the exact next step. Output only the summary.";
const SUMMARY_INPUT_PREFIX: &str = "Conversation to compact:\n";
const SUMMARY_INPUT_SUFFIX: &str = "\n\nCompaction instructions:\n";
const SUMMARY_HISTORY_PREFIX: &str =
    "[Summary of the earlier conversation, compacted to fit the context window]:\n\n";

#[derive(Debug, Clone, Copy)]
struct CompactionSpan {
    prefix_end: usize,
    recent_start: usize,
    exchanges: u32,
}

fn compaction_span(messages: &[ChatMessage]) -> Option<CompactionSpan> {
    let user_idxs = user_message_indices(messages);
    let first_actual = user_idxs
        .first()
        .is_some_and(|idx| messages[*idx].content == TRIM_MARKER) as usize;
    let actual = user_idxs.len().saturating_sub(first_actual);
    if actual <= KEEP_RECENT_EXCHANGES {
        return None;
    }
    let prefix_end = user_idxs[first_actual];
    let recent_start = user_idxs[first_actual + actual - KEEP_RECENT_EXCHANGES];
    Some(CompactionSpan {
        prefix_end,
        recent_start,
        exchanges: (actual - KEEP_RECENT_EXCHANGES) as u32,
    })
}

fn summary_max_tokens(window: u64, configured: Option<u32>) -> Option<u32> {
    let window_cap = u32::try_from(window / 2).unwrap_or(u32::MAX);
    let requested = configured
        .unwrap_or(DEFAULT_SUMMARY_MAX_TOKENS)
        .min(DEFAULT_SUMMARY_MAX_TOKENS);
    (window_cap > 0).then_some(requested.min(window_cap).max(1))
}

fn summary_transcript(messages: &[ChatMessage]) -> String {
    let mut out = String::new();
    for message in messages {
        let role = match message.role {
            Role::System => "SYSTEM",
            Role::User => "USER",
            Role::Assistant => "ASSISTANT",
            Role::Tool => "TOOL",
        };
        out.push_str(role);
        out.push_str(":\n");
        out.push_str(&message.content);
        if !message.tool_calls.is_empty() {
            out.push_str("\nTOOL CALLS: ");
            out.push_str(&serde_json::to_string(&message.tool_calls).unwrap_or_default());
        }
        out.push_str("\n\n");
    }
    out
}

fn floor_char_boundary(text: &str, mut at: usize) -> usize {
    at = at.min(text.len());
    while at > 0 && !text.is_char_boundary(at) {
        at -= 1;
    }
    at
}

fn ceil_char_boundary(text: &str, mut at: usize) -> usize {
    at = at.min(text.len());
    while at < text.len() && !text.is_char_boundary(at) {
        at += 1;
    }
    at
}

fn truncate_middle_to_bytes(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    const MARKER: &str = "\n[... older summary input shortened locally ...]\n";
    if max_bytes <= MARKER.len() {
        return text[..floor_char_boundary(text, max_bytes)].to_string();
    }
    let remaining = max_bytes - MARKER.len();
    let head_end = floor_char_boundary(text, remaining / 2);
    let tail_start = ceil_char_boundary(text, text.len().saturating_sub(remaining - head_end));
    format!("{}{}{}", &text[..head_end], MARKER, &text[tail_start..])
}

fn bounded_summary_request(
    cfg: &AgentConfig,
    old_messages: &[ChatMessage],
) -> Option<(ChatRequest, bool)> {
    let window = cfg.context_window?;
    let max_tokens = summary_max_tokens(window, cfg.max_tokens)?;
    let target = ContextBudget::new(window, Some(max_tokens)).target;
    let max_prompt_bytes = usize::try_from(target.saturating_mul(4)).unwrap_or(usize::MAX);
    let fixed_bytes =
        SUMMARY_INPUT_PREFIX.len() + SUMMARY_INPUT_SUFFIX.len() + SUMMARY_PROMPT.len();
    if fixed_bytes >= max_prompt_bytes {
        return None;
    }
    let transcript = summary_transcript(old_messages);
    let available = max_prompt_bytes - fixed_bytes;
    let bounded = truncate_middle_to_bytes(&transcript, available);
    if bounded.trim().is_empty() {
        return None;
    }
    let shortened = bounded.len() < transcript.len();
    let content = format!("{SUMMARY_INPUT_PREFIX}{bounded}{SUMMARY_INPUT_SUFFIX}{SUMMARY_PROMPT}");
    Some((
        ChatRequest {
            model: cfg.model.clone(),
            messages: vec![ChatMessage::user(content)],
            tools: None,
            stream: false,
            temperature: cfg.temperature,
            max_tokens: Some(max_tokens),
        },
        shortened,
    ))
}

fn rebuilt_with_summary(
    messages: &[ChatMessage],
    span: CompactionSpan,
    summary: &str,
) -> Vec<ChatMessage> {
    let mut rebuilt = messages[..span.prefix_end].to_vec();
    rebuilt.push(ChatMessage::user(format!(
        "{SUMMARY_HISTORY_PREFIX}{summary}"
    )));
    rebuilt.extend_from_slice(&messages[span.recent_start..]);
    rebuilt
}

fn fit_summary_rebuild(
    messages: &[ChatMessage],
    span: CompactionSpan,
    summary: &str,
    schema_json: &str,
    has_schema: bool,
    target: u64,
) -> Option<(Vec<ChatMessage>, bool)> {
    let mut cap = summary.len();
    loop {
        let bounded = truncate_middle_to_bytes(summary, cap);
        if bounded.trim().is_empty() {
            return None;
        }
        let rebuilt = rebuilt_with_summary(messages, span, bounded.trim());
        let used = structural_request_tokens(&rebuilt, schema_json, has_schema);
        if used <= target {
            return Some((rebuilt, bounded.len() < summary.len()));
        }
        if cap == 0 {
            return None;
        }
        let excess_bytes = usize::try_from(used.saturating_sub(target).saturating_mul(4))
            .unwrap_or(usize::MAX)
            .max(1);
        let next = cap.saturating_sub(excess_bytes);
        cap = if next == cap {
            cap.saturating_sub(1)
        } else {
            next
        };
    }
}

#[allow(clippy::too_many_arguments)]
async fn summarize_or_fallback<P>(
    provider: &P,
    messages: &mut Vec<ChatMessage>,
    cfg: &AgentConfig,
    schema_json: &str,
    has_schema: bool,
    target: u64,
    failures: &mut u32,
    turn_usage: &mut kitsu_providers::Usage,
    meter: &mut ContextMeter,
    progress: &mut CompactionProgress,
) where
    P: Provider,
{
    let Some(span) = compaction_span(messages) else {
        return;
    };

    let fallback = if *failures >= MAX_COMPACT_FAILURES {
        Some(SummaryFallbackReason::CircuitBreaker)
    } else {
        match bounded_summary_request(cfg, &messages[span.prefix_end..span.recent_start]) {
            None => Some(SummaryFallbackReason::RequestTooLarge),
            Some((req, shortened)) => {
                progress.summary_input_shortened = shortened;
                // This is a provider request in its own right. Check the concrete
                // bounded request immediately before crossing the boundary.
                let cold_meter = ContextMeter::new();
                if ensure_chat_request_fits(&req, cfg, &cold_meter).is_err() {
                    Some(SummaryFallbackReason::RequestTooLarge)
                } else {
                    match collect_text(provider, req).await {
                        SummaryTextCollection::Complete { text, usage }
                            if !text.trim().is_empty() =>
                        {
                            if let Some(usage) = usage {
                                turn_usage.add(usage);
                            }
                            if let Some((rebuilt, output_shortened)) = fit_summary_rebuild(
                                messages,
                                span,
                                text.trim(),
                                schema_json,
                                has_schema,
                                target,
                            ) {
                                *messages = rebuilt;
                                meter.invalidate();
                                *failures = 0;
                                progress.exchanges_removed = span.exchanges;
                                progress.summary_output_shortened = output_shortened;
                                progress.outcome = Some(CompactionOutcome::Summarized);
                                if !cfg.quiet {
                                    kitsu_render::compaction_boundary();
                                }
                                return;
                            }
                            *failures = failures.saturating_add(1);
                            Some(SummaryFallbackReason::ResponseTooLarge)
                        }
                        SummaryTextCollection::Complete { usage, .. } => {
                            if let Some(usage) = usage {
                                turn_usage.add(usage);
                            }
                            *failures = failures.saturating_add(1);
                            Some(SummaryFallbackReason::ProviderFailure)
                        }
                        SummaryTextCollection::Failed { reason, usage } => {
                            if let Some(usage) = usage {
                                turn_usage.add(usage);
                            }
                            *failures = failures.saturating_add(1);
                            Some(reason)
                        }
                    }
                }
            }
        }
    };

    // Summary failure is always local and deterministic from here: remove every
    // eligible old exchange, preserve the leading system/marker plus the two
    // recent exchanges, then let the common final preflight either accept that
    // smallest safe history or return ContextOverflow for protected context.
    let current = structural_request_tokens(messages, schema_json, has_schema);
    let removed = drop_old_exchanges_detailed(messages, current, target, message_tokens, true);
    if removed > 0 {
        meter.invalidate();
        progress.exchanges_removed = removed;
        progress.outcome = Some(CompactionOutcome::SummaryFallbackTrim);
        progress.fallback_reason = fallback;
    }
}

enum SummaryTextCollection {
    Complete {
        text: String,
        usage: Option<kitsu_providers::Usage>,
    },
    Failed {
        reason: SummaryFallbackReason,
        usage: Option<kitsu_providers::Usage>,
    },
}

/// Collect all streamed text (and usage, if reported) from a non-tool request.
/// Text is usable only when the provider stream is error-free, contains no tool
/// calls, and explicitly terminates with `Done(Stop)`. This prevents partial
/// text before a stream error, length cutoff, or bare EOF from being installed
/// as durable conversation history.
async fn collect_text<P: Provider>(provider: &P, req: ChatRequest) -> SummaryTextCollection {
    let mut stream = match provider.chat_stream(req).await {
        Ok(stream) => stream,
        Err(_) => {
            return SummaryTextCollection::Failed {
                reason: SummaryFallbackReason::ProviderFailure,
                usage: None,
            };
        }
    };
    let mut text = String::new();
    let mut usage = None;
    let mut finish = None;
    let mut unexpected_tool_call = false;
    while let Some(item) = stream.next().await {
        match item {
            Ok(ChatDelta::Text(t)) => text.push_str(&t),
            Ok(ChatDelta::Usage(u)) => usage = Some(u),
            Ok(ChatDelta::Done(reason)) => finish = Some(reason),
            Ok(ChatDelta::ToolCall(_)) => unexpected_tool_call = true,
            Err(_) => {
                return SummaryTextCollection::Failed {
                    reason: SummaryFallbackReason::ProviderFailure,
                    usage,
                };
            }
        }
    }
    if unexpected_tool_call || finish != Some(FinishReason::Stop) {
        SummaryTextCollection::Failed {
            reason: SummaryFallbackReason::IncompleteResponse,
            usage,
        }
    } else {
        SummaryTextCollection::Complete { text, usage }
    }
}

/// Elide the oldest tool-result bodies (replacing each with a short stub) until
/// the running `total` — measured by `size` — is at or under `limit`,
/// decrementing by the amount each elision actually frees. Never touches the
/// system prompt, the last user message, or any non-tool message. Returns the
/// resulting total.
#[cfg(test)]
fn trim_tool_results(
    messages: &mut [ChatMessage],
    total: u64,
    limit: u64,
    size: impl Fn(&ChatMessage) -> u64,
) -> u64 {
    trim_tool_results_counted(messages, total, limit, size).0
}

fn trim_tool_results_counted(
    messages: &mut [ChatMessage],
    mut total: u64,
    limit: u64,
    size: impl Fn(&ChatMessage) -> u64,
) -> (u64, u32) {
    if total <= limit {
        return (total, 0);
    }
    let last_user_idx = messages.iter().rposition(|m| m.role == Role::User);
    let mut elided = 0u32;
    let mut i = 0;
    while total > limit && i < messages.len() {
        // Only elide tool results (never system / last-user / assistant turns).
        if messages[i].role == Role::Tool && Some(i) != last_user_idx {
            let before = size(&messages[i]);
            let stub = elided_tool_stub(&messages[i]);
            let after = size(&stub);
            if after < before {
                total = total.saturating_sub(before - after);
                messages[i] = stub;
                elided = elided.saturating_add(1);
            }
        }
        i += 1;
    }
    (total, elided)
}

/// Build the elided replacement for a tool-result message. If the message is
/// already an offload pointer ([`maybe_offload`]'s "full result saved to
/// &lt;path&gt;" stub), keep the pointer line but drop the preview body — replacing
/// the whole thing with the generic stub would permanently strand the on-disk
/// result, since the model could no longer `fs_read_file` a path it can't see
/// anymore. Anything else collapses to the generic elision stub. Idempotent: if
/// the message is already just the bare pointer line, this returns identical
/// content (caller's before/after size check then leaves it alone).
///
/// Detection is anchored on [`OFFLOAD_POINTER_PREFIX`] (the exact prefix
/// `maybe_offload` emits), not a bare `contains("saved to")` — an ordinary tool
/// result can legitimately contain that phrase (e.g. a build tool printing
/// "Output saved to build/out.js"), and a bare substring match would wrongly
/// treat it as a recoverable pointer, silently discarding the rest of that
/// line's siblings instead of applying the intended generic elision.
fn elided_tool_stub(m: &ChatMessage) -> ChatMessage {
    let call_id = m.tool_call_id.clone().unwrap_or_default();
    if let Some(pointer_line) = m
        .content
        .lines()
        .find(|l| l.starts_with(OFFLOAD_POINTER_PREFIX))
    {
        return ChatMessage::tool(call_id, pointer_line.to_string());
    }
    ChatMessage::tool(call_id, "[older tool output elided]")
}

/// Size (chars) above which a successful tool result is offloaded to disk
/// instead of kept inline (Stage 2, Tier 0). ~2K tokens.
const OFFLOAD_CHAR_CAP: usize = 8 * 1024;
/// Exact prefix `maybe_offload` emits for its pointer line — the anchor
/// `elided_tool_stub` uses to recognize an offload pointer. Kept as one
/// constant shared by both ends so they can't silently drift apart.
const OFFLOAD_POINTER_PREFIX: &str = "[tool output ";
/// How much of an offloaded result to keep inline as a preview.
const OFFLOAD_PREVIEW_CHARS: usize = 500;

/// Offload only when the active model can actually recover the resulting
/// pointer. A registered-but-unadvertised handler is insufficient: the model
/// must receive an `fs_read_file` descriptor in this turn's tool catalog.
fn maybe_offload_for_tools(
    content: String,
    call_id: &str,
    dir: Option<&std::path::Path>,
    tools: &Registry,
) -> String {
    let can_read = tools.is_registered("fs_read_file")
        && tools
            .descriptors()
            .iter()
            .any(|descriptor| descriptor.name == "fs_read_file");
    if !can_read {
        return content;
    }
    maybe_offload(content, call_id, dir)
}

/// If `content` exceeds the cap and an offload `dir` is set, allocate a unique
/// private `<dir>/<call_id>-<sequence>.txt` artifact and return a pointer +
/// preview the model can re-read via `fs_read_file`. Returns the content
/// unchanged when there is no dir, it's under the cap, or the write fails (the
/// trimming cascade still bounds it). The offload dir must be model-readable.
fn maybe_offload(content: String, call_id: &str, dir: Option<&std::path::Path>) -> String {
    let Some(dir) = dir else {
        return content;
    };
    if content.len() <= OFFLOAD_CHAR_CAP {
        return content;
    }
    let safe_id: String = call_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    let name = if safe_id.is_empty() {
        "result".to_string()
    } else {
        safe_id
    };
    let path = match kitsu_tools::builtins::fs::write_unique_private_text_file(dir, &name, &content)
    {
        Ok(path) => path,
        Err(_) => return content,
    };
    let preview: String = content.chars().take(OFFLOAD_PREVIEW_CHARS).collect();
    format!(
        "{OFFLOAD_POINTER_PREFIX}{:.1} KB — full result saved to {}. Re-read it (or a line range) \
         with fs_read_file on that path.]\n\nPreview:\n{preview}…",
        content.len() as f64 / 1024.0,
        path.display(),
    )
}

/// How many recent user-delimited exchanges to always keep intact.
const KEEP_RECENT_EXCHANGES: usize = 2;
const TRIM_MARKER: &str = "[earlier conversation trimmed to fit the context window]";

/// Drop the oldest whole exchanges (a user message and everything up to the next
/// user message) when tool-result elision wasn't enough. Cutting only at user
/// boundaries preserves API invariants — no tool_result is ever orphaned from its
/// assistant tool_call. The leading system prefix and the most recent
/// [`KEEP_RECENT_EXCHANGES`] exchanges are always kept; a marker records the drop.
/// Indices of `Role::User` messages, in order — the exchange boundaries both
/// [`drop_old_exchanges`] and summarizing compaction need. Kept as one function
/// so the two can't drift out of sync with each other.
fn user_message_indices(messages: &[ChatMessage]) -> Vec<usize> {
    messages
        .iter()
        .enumerate()
        .filter(|(_, m)| m.role == Role::User)
        .map(|(i, _)| i)
        .collect()
}

/// Returns `true` if it actually dropped an exchange, `false` if there was
/// nothing safely droppable (too few exchanges) or nothing needed dropping —
/// callers use this to decide whether `messages` was structurally reshaped
/// (and so whether a [`ContextMeter`] anchor needs invalidating), since
/// invalidating on a guaranteed no-op would needlessly discard a still-valid
/// anchor and force every later estimate back to a full re-tokenize.
#[cfg(test)]
fn drop_old_exchanges(
    messages: &mut Vec<ChatMessage>,
    total: u64,
    target: u64,
    size: impl Fn(&ChatMessage) -> u64,
) -> bool {
    drop_old_exchanges_detailed(messages, total, target, size, false) > 0
}

/// Detailed/force-capable form used by telemetry and explicit manual
/// compaction. `force_all` deterministically removes every eligible old
/// exchange even when `total` is already below `target`.
fn drop_old_exchanges_detailed(
    messages: &mut Vec<ChatMessage>,
    mut total: u64,
    target: u64,
    size: impl Fn(&ChatMessage) -> u64,
    force_all: bool,
) -> u32 {
    let user_idxs = user_message_indices(messages);
    let marker_present = user_idxs
        .first()
        .is_some_and(|idx| messages[*idx].content == TRIM_MARKER);
    let first_actual = usize::from(marker_present);
    let actual = user_idxs.len().saturating_sub(first_actual);
    if actual <= KEEP_RECENT_EXCHANGES {
        return 0; // nothing safely droppable
    }
    let max_drop = actual - KEEP_RECENT_EXCHANGES;

    // `total` starts from the caller's running (anchored) estimate; decrement by
    // what each dropped exchange frees, keeping both trim stages on one scale.
    // Account for the marker we insert on the first drop so returning "fits"
    // cannot be invalidated merely by the marker's own token cost.
    let mut e = 0;
    while (force_all || total > target) && e < max_drop {
        let start = user_idxs[first_actual + e];
        let end = user_idxs[first_actual + e + 1];
        total = total.saturating_sub(messages[start..end].iter().map(&size).sum::<u64>());
        if e == 0 && !marker_present {
            total = total.saturating_add(size(&ChatMessage::user(TRIM_MARKER)));
        }
        e += 1;
    }
    if e == 0 {
        return 0;
    }
    let drop_start = user_idxs[first_actual];
    let drop_end = user_idxs[first_actual + e];
    messages.drain(drop_start..drop_end);
    if !marker_present {
        messages.insert(drop_start, ChatMessage::user(TRIM_MARKER));
    }
    u32::try_from(e).unwrap_or(u32::MAX)
}
