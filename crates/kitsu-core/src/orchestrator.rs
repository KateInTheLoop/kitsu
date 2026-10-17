//! The runtime agent loop. Implements the pseudocode in
//! docs/architecture.md §5: call model → parse tool calls → run tools →
//! append results → repeat until the model stops or a cap is hit.

use crate::context::{
    clamp_max_tokens_to_reserve, clamp_window_for_shortfall, estimate_str, message_tokens,
    schema_fingerprint, ContextBudget, ContextMeter, CHARS_PER_TOKEN, PER_MESSAGE_TOKENS,
};

/// Detect mangled chat-template control tokens (e.g. Kimi's `<|close|>`,
/// `<|sep|>`) in the TAIL of a final answer — the residue a provider leaks
/// when it fails to parse a model's native-format trailing tool call
/// (observed live: the debris contained the Write call's file path). Only
/// the last 300 chars are scanned, and two distinct control tokens are
/// required, so answers that merely QUOTE such tokens earlier in prose or
/// code fences do not trip it.
fn template_control_debris_in_tail(text: &str) -> bool {
    let tail_start = text.len().saturating_sub(300);
    // Snap to a char boundary.
    let mut start = tail_start;
    while start < text.len() && !text.is_char_boundary(start) {
        start += 1;
    }
    let tail = &text[start..];
    let mut distinct = std::collections::HashSet::new();
    let bytes = tail.as_bytes();
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == b'<' && bytes[i + 1] == b'|' {
            if let Some(end) = tail[i + 2..].find("|>") {
                let token = &tail[i + 2..i + 2 + end];
                if !token.is_empty()
                    && token.len() <= 24
                    && token.chars().all(|ch| ch.is_ascii_lowercase() || ch == '_')
                {
                    distinct.insert(token.to_string());
                }
                i += 2 + end + 2;
                continue;
            }
        }
        i += 1;
    }
    distinct.len() >= 2
}
use futures::StreamExt;
use kitsu_providers::{
    ChatDelta, ChatMessage, ChatRequest, FinishReason, JsonSchemaFormat, Provider,
    ProviderAttemptCategory, ProviderAttemptOutcome, ProviderError, ProviderTelemetrySink,
    ReasoningEffort, Role, ToolCall, ToolDescriptor,
};
use kitsu_tools::{
    scope_command_progress, CommandAmbientAuthority, CommandExecArgs, CommandExecutableIdentity,
    CommandExecutableResolution, CommandOutput, CommandOutputArtifact, CommandProgressSink,
    CommandTermination, Registry, ToolInventory,
};
use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use thiserror::Error;

const STRUCTURED_FINALIZATION_NUDGE: &str = "The tool-action phase is complete. Tools are no \
longer available. Produce only the final answer now, following the required structured output \
format exactly.";

#[cfg(test)]
#[path = "orchestrator_tests.rs"]
mod orchestrator_tests;

/// Per-turn budget for subagent spawns, shared between the frontend's spawn
/// tools and the orchestrator. The orchestrator resets `used` at the start of
/// each turn whose [`AgentConfig`] carries the budget (the main conversation);
/// subagent configs carry no budget, so a nested turn never resets its
/// parent's. Small local models demonstrably ignore prompt-level spawn
/// guidance and fan out unboundedly on trivial turns — this is the hard stop
/// that turns the storm into a model-visible tool error.
#[derive(Debug, Default)]
pub struct SpawnBudget {
    cap: u32,
    used: std::sync::atomic::AtomicU32,
}

impl SpawnBudget {
    pub fn new(cap: u32) -> Self {
        Self {
            cap,
            used: std::sync::atomic::AtomicU32::new(0),
        }
    }

    pub fn cap(&self) -> u32 {
        self.cap
    }

    pub fn used(&self) -> u32 {
        self.used.load(Ordering::Acquire)
    }

    /// Admit up to `requested` spawns, returning how many were granted
    /// (possibly zero). Saturating and race-free under concurrent tool calls.
    pub fn admit(&self, requested: u32) -> u32 {
        let mut granted = 0;
        let _ = self
            .used
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                granted = requested.min(self.cap.saturating_sub(used));
                Some(used + granted)
            });
        granted
    }

    /// Start a fresh turn: the whole cap is available again.
    pub fn reset(&self) {
        self.used.store(0, Ordering::Release);
    }
}

/// Tuning for the agent loop.
#[derive(Debug, Clone)]
pub struct AgentConfig {
    pub model: String,
    /// Configured budget for each logical provider request. Frontends that
    /// know the transport timeout disclose it on `provider_request`; embedded
    /// providers may leave it unknown.
    pub request_timeout_ms: Option<u64>,
    /// Optional enclosing turn deadline enforced by the frontend. This is
    /// deliberately distinct from the per-request transport timeout because a
    /// multi-round turn must not appear to regain its whole budget on every
    /// provider call.
    pub turn_deadline: Option<tokio::time::Instant>,
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
    /// Size cap (chars) above which a successful tool result is offloaded to
    /// `offload_dir` (with a retrievable `fs_read_file` pointer) instead of kept
    /// inline, when under context pressure. `None` uses the default
    /// ([`OFFLOAD_CHAR_CAP`], ~2K tokens). Lowering it is "recall mode": more of
    /// what the agent sees becomes fs-retrievable, so trimming an oldest result
    /// leaves a pointer the model can re-read instead of losing it outright —
    /// the difference between a small model recovering `find_cli_tables` output
    /// and hallucinating it after the window trims that evidence away.
    pub offload_char_cap: Option<usize>,
    /// Whether Kitsu may create automatic tool/command output artifacts for
    /// this run. `InlineOnly` never writes an offload file and instead keeps a
    /// deterministic UTF-8-safe head/tail preview with explicit byte counts.
    /// This is storage policy only: it does not change tool authority or
    /// explicit model/host output destinations.
    pub artifact_policy: ArtifactStoragePolicy,
    pub stream: bool,
    pub temperature: Option<f32>,
    /// Optional reasoning-effort hint forwarded to compatible gateways.
    pub reasoning_effort: Option<ReasoningEffort>,
    /// Max output tokens per response. `None` lets the gateway apply its own
    /// (often small, e.g. 4096) default — which silently truncates long outputs
    /// like code reviews. Set this to lift the cap.
    pub max_tokens: Option<u32>,
    /// Provider-native strict response shape for main automation turns. Child
    /// agents and internal compaction requests deliberately leave this unset.
    pub response_format: Option<JsonSchemaFormat>,
    /// Keep provider-native response formatting off tool-capable action rounds
    /// and reserve it for one tool-free finalization round. The CLI enables
    /// this only when a required filesystem effect accompanies structured
    /// output, so small models are not asked to act and format the terminal
    /// answer in the same request.
    pub defer_response_format_until_final: bool,
    /// Host-owned readiness check consulted before a deferred structured
    /// finalization. When it reports a repair, the loop keeps tools available
    /// for one bounded action round and reserves a later tool-free finalizer.
    /// The callback grants no authority and is never installed for child
    /// agents or ordinary unstructured turns.
    pub structured_finalization_gate: Option<StructuredFinalizationGate>,
    /// What to do when cheap trimming (offload + elide) isn't enough to fit the
    /// window (Stage 3). `Trim` drops the oldest whole exchanges; `Summarize`
    /// replaces them with an LLM-written summary (opt-in — quality is model-
    /// dependent). Falls back to `Trim` if summarization fails.
    pub compaction: CompactionMode,
    /// Suppress this turn's terminal narration (spinner, `◆`/`◇` tool activity,
    /// and the `kitsu-usage:`/`kitsu-context:` lines). Set for BACKGROUND
    /// subagents so their output doesn't corrupt the interactive prompt while
    /// they run detached; their result is delivered via the agent registry.
    pub quiet: bool,
    /// Optional structured trace for automation frontends. By default events
    /// are retained in memory; a frontend may also install a synchronous,
    /// content-bounded observer to surface progress while the turn is active.
    pub event_log: Option<AgentEventLog>,
    /// Skill names known to this session, so text-protocol recovery can tell
    /// a skill invoked as a fake tool (`{"name": "code-review"}`) apart from
    /// arbitrary JSON in a real answer. Empty when no skills are loaded.
    pub skill_names: Vec<String>,
    /// Shared run-wide tallies (reads/denied/failed). Unlike per-state
    /// fields, this Arc spans the MAIN conversation and every subagent, so
    /// end-of-run receipts count delegated work (cycle-17 #1, cycle-19 #5).
    pub run_tallies: Option<std::sync::Arc<RunTallies>>,
    /// Per-turn subagent spawn budget (config `agent_spawn_cap`). Set only on
    /// the MAIN conversation's config; the orchestrator resets it when a turn
    /// starts. Subagent configs must leave this `None` so nested turns never
    /// refresh the parent's budget mid-turn.
    pub spawn_budget: Option<std::sync::Arc<SpawnBudget>>,
}

/// Run-level policy for Kitsu-created automatic output artifacts.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactStoragePolicy {
    /// Preserve normal private offload/full-command-output behavior when the
    /// frontend installs a destination.
    #[default]
    Automatic,
    /// Create no automatic artifact files; oversized results become bounded,
    /// explicitly lossy inline previews.
    InlineOnly,
}

/// What happened to the recoverable full output associated with one tool
/// result. This is deliberately separate from tool success/failure.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolArtifactStatus {
    /// The complete result already fit inline, so no artifact was useful.
    NotNeeded,
    /// A complete host-owned artifact was retained.
    Retained,
    /// Artifact creation was deliberately disabled by run policy.
    SuppressedByPolicy,
    /// Automatic storage was allowed but no recoverable artifact was created.
    Unavailable,
    /// The configured artifact sink reported a failure.
    Failed,
}

/// A deterministic, UTF-8-safe inline representation of output that cannot be
/// persisted as an automatic artifact. Raw byte counts deliberately exclude
/// the host-authored omission marker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundedInlineOutput {
    pub content: String,
    pub original_bytes: u64,
    pub retained_bytes: u64,
    pub omitted_bytes: u64,
    pub truncated: bool,
}

/// Keep a bounded head and tail when automatic artifact persistence is
/// disabled. This is public so frontends can apply the identical policy to
/// background/subagent results before they enter a registry or provider
/// context.
pub fn bound_inline_output(content: String) -> BoundedInlineOutput {
    let original_bytes = u64::try_from(content.len()).unwrap_or(u64::MAX);
    if content.len() <= OFFLOAD_CHAR_CAP {
        return BoundedInlineOutput {
            retained_bytes: original_bytes,
            original_bytes,
            omitted_bytes: 0,
            truncated: false,
            content,
        };
    }

    // Reserve enough room for the deterministic marker itself so the returned
    // representation also fits the cap. That makes this operation idempotent:
    // a background result bounded before registry storage is not re-truncated
    // when it later crosses another tool boundary.
    const MARKER_RESERVE: usize = 256;
    let raw_budget = OFFLOAD_CHAR_CAP.saturating_sub(MARKER_RESERVE);
    let head_budget = raw_budget / 2;
    let mut head_end = head_budget;
    while head_end > 0 && !content.is_char_boundary(head_end) {
        head_end -= 1;
    }

    let tail_budget = raw_budget.saturating_sub(head_end);
    let mut tail_start = content.len().saturating_sub(tail_budget);
    while tail_start < content.len() && !content.is_char_boundary(tail_start) {
        tail_start += 1;
    }
    if tail_start < head_end {
        tail_start = head_end;
    }

    let retained = head_end.saturating_add(content.len().saturating_sub(tail_start));
    let retained_bytes = u64::try_from(retained).unwrap_or(u64::MAX);
    let omitted_bytes = original_bytes.saturating_sub(retained_bytes);
    let marker = format!(
        "\n[... {omitted_bytes} bytes omitted by artifact-free policy; original \
         {original_bytes} bytes, retained {retained_bytes} bytes ...]\n"
    );
    let mut bounded = String::with_capacity(retained.saturating_add(marker.len()));
    bounded.push_str(&content[..head_end]);
    bounded.push_str(&marker);
    bounded.push_str(&content[tail_start..]);
    debug_assert!(bounded.len() <= OFFLOAD_CHAR_CAP);

    BoundedInlineOutput {
        content: bounded,
        original_bytes,
        retained_bytes,
        omitted_bytes,
        truncated: true,
    }
}

/// A content-bounded target disclosed when structured finalization is deferred
/// so the model can repair a host-verified exact-output mismatch.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct StructuredFinalizationRepairTarget {
    pub path: String,
    pub expected_sha256: String,
    pub actual_sha256: String,
}

/// Host-owned repair instruction returned by a structured-finalization gate.
/// `instruction` is request-local model context; the typed runtime event emits
/// only the stable cause and bounded target facts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StructuredFinalizationRepair {
    pub cause: String,
    pub instruction: String,
    pub targets: Vec<StructuredFinalizationRepairTarget>,
}

/// Cloneable synchronous readiness callback for deferred finalization.
#[derive(Clone)]
pub struct StructuredFinalizationGate {
    evaluate: Arc<dyn Fn() -> Option<StructuredFinalizationRepair> + Send + Sync>,
}

impl StructuredFinalizationGate {
    pub fn new<F>(evaluate: F) -> Self
    where
        F: Fn() -> Option<StructuredFinalizationRepair> + Send + Sync + 'static,
    {
        Self {
            evaluate: Arc::new(evaluate),
        }
    }

    fn evaluate(&self) -> Option<StructuredFinalizationRepair> {
        (self.evaluate)()
    }
}

impl std::fmt::Debug for StructuredFinalizationGate {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StructuredFinalizationGate")
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProviderRequestPurpose {
    Inference,
    Compaction,
    ModelCatalog,
}

/// Scope of the countdown reported by a physical provider attempt.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProviderDeadlineScope {
    ProviderRequest,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum AgentRuntimeEvent {
    ProviderRequest {
        round: u32,
        purpose: ProviderRequestPurpose,
        /// Exact model selected for this inference or compaction request.
        /// Catalog lookups are not routed to a model and leave this absent.
        #[serde(skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        message_count: usize,
        tools_offered: usize,
        /// True when this logical request carries a provider-native strict
        /// JSON Schema response format. This is metadata only; the schema
        /// body remains out of the event stream.
        structured_output: bool,
        /// Configured budget for this logical provider request. Historical or
        /// embedded producers may omit it when the transport budget is not
        /// known to the orchestrator.
        #[serde(skip_serializing_if = "Option::is_none")]
        request_timeout_ms: Option<u64>,
        /// Remaining budget for the enclosing turn, when the frontend enforces
        /// one independently of the provider request timeout.
        #[serde(skip_serializing_if = "Option::is_none")]
        turn_remaining_ms: Option<u64>,
    },
    /// A host-verified exact-output mismatch kept the next bounded round in
    /// tool-action mode instead of spending the reserved structured finalizer.
    StructuredFinalizationDeferred {
        round: u32,
        repair_round: u32,
        finalization_round_reserved: bool,
        cause: String,
        targets: Vec<StructuredFinalizationRepairTarget>,
        repair_instruction_disclosed: bool,
        write_authority_granted: bool,
    },
    /// One physical network send inside the preceding logical
    /// [`ProviderRequest`](Self::ProviderRequest). Retries produce additional
    /// attempt events, never additional logical request events.
    ProviderAttempt {
        round: u32,
        purpose: ProviderRequestPurpose,
        /// Immutable route generation attributed by Kitsu's admission handle.
        /// Legacy/raw providers leave it absent rather than inventing identity.
        #[serde(skip_serializing_if = "Option::is_none")]
        route_generation: Option<u64>,
        attempt: u32,
        max_attempts: u32,
        category: ProviderAttemptCategory,
        status: Option<u16>,
        outcome: ProviderAttemptOutcome,
        terminal: bool,
        retry_delay_ms: Option<u64>,
        retry_after_used: bool,
        /// `remaining_deadline_ms` is scoped to this provider request and may
        /// reset when the agent starts another logical request.
        deadline_scope: ProviderDeadlineScope,
        remaining_deadline_ms: u64,
        /// Independently decreasing enclosing-turn budget, when present.
        #[serde(skip_serializing_if = "Option::is_none")]
        turn_remaining_ms: Option<u64>,
    },
    /// Assistant text safe for immediate frontend display. Text is emitted as
    /// chunks when no tools were offered. Tool-capable rounds are buffered and
    /// emitted only after text-protocol tool JSON has been ruled out.
    AssistantDelta { round: u32, text: String },
    Usage {
        round: u32,
        purpose: ProviderRequestPurpose,
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
        /// Exact raw bytes returned by the tool before inline reduction. For a
        /// native command this is stdout + stderr, whose per-stream counters
        /// remain authoritative in `command`.
        output_bytes: u64,
        /// Raw output bytes retained in the bounded inline representation.
        /// Host-authored markers/pointers are not included in this count.
        output_retained_bytes: u64,
        /// True when raw output bytes were omitted from the inline result.
        output_truncated: bool,
        /// Whether full output was unnecessary, retained, deliberately
        /// suppressed, unavailable, or failed independently of tool success.
        artifact_status: ToolArtifactStatus,
        #[serde(skip_serializing_if = "Option::is_none")]
        command: Option<Box<CommandRuntimeResult>>,
    },
    /// History trimming replaced one or more tool results with a dead
    /// `[older tool output elided]` stub: the bodies are gone and nothing
    /// recallable was left in their place, so the provider never saw them.
    /// A lossless eviction (one that keeps an offload pointer the model can
    /// still `fs_read_file`) is deliberately NOT reported here — that is the
    /// documented good case `--offload-cap` exists to produce, and reporting
    /// it would teach readers to ignore this event.
    ToolResultsDropped { round: u32, dropped: u32 },
    TurnFinished {
        success: bool,
        completion: String,
        tool_errors: u32,
        policy_errors: u32,
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
    pub artifact_status: ToolArtifactStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact: Option<CommandOutputArtifact>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact_error: Option<String>,
    /// Host-authored containment remediation hint for missing-module/command
    /// failure shapes (see `CommandOutput::containment_hint`). Additive.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub containment_hint: Option<String>,
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
    purpose: ProviderRequestPurpose,
    turn_deadline: Option<tokio::time::Instant>,
    usage_tracker: Option<ProviderResponseUsageTracker>,
) -> Option<ProviderTelemetrySink> {
    if log.is_none() && usage_tracker.is_none() {
        return None;
    }
    let log = log.cloned();
    Some(ProviderTelemetrySink::with_fallible_context_observer(
        move |observation| {
            let event = observation.event;
            if let Some(tracker) = &usage_tracker {
                tracker.observe_attempt(&event);
            }
            if let Some(log) = &log {
                log.record(AgentRuntimeEvent::ProviderAttempt {
                    round,
                    purpose,
                    route_generation: observation
                        .route_generation
                        .map(|generation| generation.get()),
                    attempt: event.attempt,
                    max_attempts: event.max_attempts,
                    category: event.category,
                    status: event.status,
                    outcome: event.outcome,
                    terminal: event.terminal,
                    retry_delay_ms: event.retry_delay_ms,
                    retry_after_used: event.retry_after_used,
                    deadline_scope: ProviderDeadlineScope::ProviderRequest,
                    remaining_deadline_ms: event.remaining_deadline_ms,
                    turn_remaining_ms: remaining_turn_millis(turn_deadline),
                });
                !log.abort_requested()
            } else {
                true
            }
        },
    ))
}

fn remaining_turn_millis(deadline: Option<tokio::time::Instant>) -> Option<u64> {
    deadline.map(|deadline| {
        u64::try_from(
            deadline
                .saturating_duration_since(tokio::time::Instant::now())
                .as_millis(),
        )
        .unwrap_or(u64::MAX)
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
    command_progress: Option<std::sync::Arc<std::sync::Mutex<Option<CommandOutput>>>>,
    artifact_policy: ArtifactStoragePolicy,
    started: std::time::Instant,
}

#[derive(Debug, Clone, Copy)]
struct ToolOutputRetention {
    output_bytes: u64,
    output_retained_bytes: u64,
    output_truncated: bool,
    artifact_status: ToolArtifactStatus,
}

struct PreparedToolOutput {
    content: String,
    retention: ToolOutputRetention,
    artifact_path: Option<std::path::PathBuf>,
}

#[derive(Clone, Copy)]
struct ToolOutputRequest<'a> {
    call_id: &'a str,
    tool_name: &'a str,
    is_err: bool,
    window_evidence: Option<ContextWindowEvidence>,
}

impl ToolOutputRetention {
    fn complete(value: &str) -> Self {
        let bytes = u64::try_from(value.len()).unwrap_or(u64::MAX);
        Self {
            output_bytes: bytes,
            output_retained_bytes: bytes,
            output_truncated: false,
            artifact_status: ToolArtifactStatus::NotNeeded,
        }
    }

    fn from_command(command: &CommandRuntimeResult) -> Self {
        Self {
            output_bytes: command.stdout_bytes.saturating_add(command.stderr_bytes),
            output_retained_bytes: u64::try_from(
                command
                    .stdout_retained_bytes
                    .saturating_add(command.stderr_retained_bytes),
            )
            .unwrap_or(u64::MAX),
            output_truncated: command.stdout_truncated || command.stderr_truncated,
            artifact_status: command.artifact_status,
        }
    }
}

impl ToolEventGuard {
    fn start(
        log: Option<&AgentEventLog>,
        round: u32,
        call_id: &str,
        name: &str,
        args: &str,
        artifact_policy: ArtifactStoragePolicy,
    ) -> Self {
        let pending = log.map(|log| {
            let command = command_runtime_invocation(name, args);
            let command_progress = command
                .as_ref()
                .map(|_| std::sync::Arc::new(std::sync::Mutex::new(None)));
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
                command_progress,
                artifact_policy,
                started: std::time::Instant::now(),
            }
        });
        Self { pending }
    }

    fn command_progress_sink(&self) -> Option<CommandProgressSink> {
        let progress = self.pending.as_ref()?.command_progress.as_ref()?.clone();
        Some(CommandProgressSink::new(move |snapshot| {
            *progress.lock().expect("command progress") = Some(snapshot);
        }))
    }

    fn finish(mut self, success: bool, output: &str, retention: ToolOutputRetention) {
        if let Some(pending) = self.pending.take() {
            let command = pending
                .command
                .as_ref()
                .and_then(|_| command_runtime_result(output, pending.artifact_policy))
                .map(Box::new);
            let retention = command
                .as_deref()
                .map(ToolOutputRetention::from_command)
                .unwrap_or(retention);
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
                output_bytes: retention.output_bytes,
                output_retained_bytes: retention.output_retained_bytes,
                output_truncated: retention.output_truncated,
                artifact_status: retention.artifact_status,
                command,
            });
        }
    }
}

impl Drop for ToolEventGuard {
    fn drop(&mut self) {
        if let Some(pending) = self.pending.take() {
            let observed_command = pending
                .command_progress
                .as_ref()
                .and_then(|progress| progress.lock().expect("command progress").clone())
                .map(|output| command_runtime_result_from_output(output, pending.artifact_policy))
                .map(Box::new);
            let command = observed_command.or_else(|| {
                pending.command.as_ref().map(|invocation| {
                    let executable_name = invocation.argv.first().cloned().unwrap_or_default();
                    let executable_path = std::path::Path::new(&executable_name);
                    let executable_resolution = if executable_path.is_absolute()
                        || executable_path.components().count() > 1
                    {
                        CommandExecutableResolution::InvocationPath
                    } else {
                        CommandExecutableResolution::AmbientPath
                    };
                    Box::new(CommandRuntimeResult {
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
                        artifact_status: if pending.artifact_policy
                            == ArtifactStoragePolicy::InlineOnly
                        {
                            ToolArtifactStatus::SuppressedByPolicy
                        } else {
                            ToolArtifactStatus::Unavailable
                        },
                        artifact: None,
                        artifact_error: (pending.artifact_policy
                            != ArtifactStoragePolicy::InlineOnly)
                            .then(|| {
                                "command cancelled before full-output artifact state was observed; \
                                 cleanup requirement is unverified and no cleanup is claimed"
                                    .to_string()
                            }),
                        containment_hint: None,
                    })
                })
            });
            let retention = command
                .as_deref()
                .map(ToolOutputRetention::from_command)
                .unwrap_or_else(|| {
                    ToolOutputRetention::complete("[cancelled: tool execution interrupted]")
                });
            pending.log.record(AgentRuntimeEvent::ToolFinished {
                round: pending.round,
                call_id: pending.call_id,
                name: pending.name,
                success: false,
                cancelled: true,
                output: "[cancelled: tool execution interrupted]".to_string(),
                output_bytes: retention.output_bytes,
                output_retained_bytes: retention.output_retained_bytes,
                output_truncated: retention.output_truncated,
                artifact_status: retention.artifact_status,
                command,
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
    /// The summary fallback ran but found no eligible old exchanges to remove.
    SummaryFallbackNoOp,
}

/// Why a requested summary used the trim fallback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SummaryFallbackReason {
    /// The per-session circuit breaker has disabled further summary calls.
    CircuitBreaker,
    /// Even a locally shortened summary prompt could not fit the model window.
    RequestTooLarge,
    /// The provider failed (transport or stream error).
    ProviderFailure,
    /// The provider completed successfully but returned no usable summary text.
    EmptyCompletion,
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
            CompactionOutcome::ToolResultsElided | CompactionOutcome::SummaryFallbackNoOp => {}
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
            request_timeout_ms: None,
            turn_deadline: None,
            max_rounds: 10,
            max_context_bytes: 128 * 1024,
            context_window: None,
            offload_dir: None,
            offload_char_cap: None,
            artifact_policy: ArtifactStoragePolicy::Automatic,
            stream: true,
            temperature: None,
            reasoning_effort: None,
            max_tokens: None,
            response_format: None,
            defer_response_format_until_final: false,
            structured_finalization_gate: None,
            compaction: CompactionMode::Trim,
            quiet: false,
            event_log: None,
            skill_names: Vec::new(),
            run_tallies: None,
            spawn_budget: None,
        }
    }
}

#[derive(Debug, Error)]
pub enum AgentError {
    #[error(transparent)]
    Provider(#[from] ProviderError),

    #[error("run interrupted: {0}")]
    Interrupted(crate::run::RunInterruption),

    #[error(
        "model returned a tool call after tools were withheld for structured finalization; no tool was executed"
    )]
    ToolCallDuringToolFreeFinalization,

    #[error("agent loop error: {0}")]
    Loop(String),

    #[error(
        "context preflight stopped an oversized provider request: protected prompt needs about {required} {unit}, but the trim target is {target} {unit}{}; shorten the latest request or system instructions, disable large tool schemas, raise the window with --context-window <n>, lower --max-output-tokens, clear/compact history, or select a model with a larger context window",
        context_budget_detail(*window, *effective, *target, *unit)
    )]
    ContextOverflow {
        required: u64,
        target: u64,
        unit: ContextUnit,
        /// The resolved model window for token checks; `None` identifies the
        /// coarse byte-budget fallback used when model metadata is unavailable.
        window: Option<u64>,
        /// The prompt budget the `kitsu-context:` line reports (window minus the
        /// output reservation). `None` in the byte fallback, where the target is
        /// the whole budget. Carried so the two numbers a user sees side by side
        /// can explain their own difference instead of silently disagreeing.
        effective: Option<u64>,
    },

    #[error(
        "manual compaction needs a resolved model context window; configure context_window or select a model whose window Kitsu can discover"
    )]
    ContextWindowUnknown,
}

/// Spell out the relationship between the window, the effective prompt budget,
/// and the trim target so the preflight error and the `kitsu-context:` line
/// never present two unexplained numbers for the same limit.
fn context_budget_detail(
    window: Option<u64>,
    effective: Option<u64>,
    target: u64,
    unit: ContextUnit,
) -> String {
    match (window, effective) {
        (Some(window), Some(effective)) => format!(
            " (window {window} {unit}, effective prompt budget {effective} {unit}, trim target = effective minus a {} {unit} reserve)",
            effective.saturating_sub(target)
        ),
        _ => String::new(),
    }
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
    /// Runtime evidence that the serving backend's real context window is
    /// smaller than the resolved one (see [`ContextWindowEvidence`]). This is
    /// backend telemetry, not conversation state: callers that roll a failed
    /// turn back should preserve it.
    pub window_evidence: Option<ContextWindowEvidence>,
    /// Successful `fs_read_file` calls across the whole run. Rendered in the
    /// end-of-run receipt so "wrote a summary of files it never opened"
    /// (cycle-16 M1) is visible at a glance.
    pub run_reads: u64,
    /// Cumulative provider usage across the run, for `/cost`.
    pub run_usage: kitsu_providers::Usage,
    /// Completed model turns across the run, for `/cost`.
    pub run_turns: u32,
    /// P11: cached (fingerprint, schema_json, token estimate) for the tool
    /// schema. The preflight at every provider boundary used to re-run
    /// `estimate_str` over the full serialized schema every round even though
    /// the tool registry almost never changes within a session. Keyed by the
    /// P7 [`schema_fingerprint`] of the serialized schema: a matching
    /// fingerprint reuses the cached estimate, a mismatch recomputes and
    /// overwrites. Session-scoped so no cross-session staleness is possible.
    pub schema_cache: Option<(u64, String, u64)>,
}

/// Evidence, gathered from a provider `usage` report, that the server accepted
/// markedly fewer prompt tokens than the request was estimated to contain —
/// the signature of a gateway silently truncating input to a context window
/// smaller than the resolved one (e.g. a config claiming 8192 while Ollama
/// serves `num_ctx = 4096`). Kitsu records the discrepancy, warns once, and
/// surfaces it in `/context` provenance; it never silently rewrites the
/// resolved window, because token estimates are heuristic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextWindowEvidence {
    /// `prompt_tokens` from the provider's usage report.
    pub reported_prompt_tokens: u64,
    /// Kitsu's preflight estimate for the request that produced the report.
    pub estimated_sent_tokens: u64,
    /// The resolved window the request was budgeted against.
    pub resolved_window: u64,
    /// The one-time warning for this evidence has already been emitted.
    pub warned: bool,
    /// Number of shortfall hits recorded for THIS `resolved_window` (>= 1).
    /// Resets to 1 whenever the resolved window changes (see the recording
    /// site in `run_agent_turn_inner`). P6: once this reaches 2 (two-strike
    /// rule), [`clamp_window_for_shortfall`] clamps the EFFECTIVE SESSION
    /// BUDGET fed to [`ContextBudget::new`] toward `reported_prompt_tokens` —
    /// `resolved_window` itself is never rewritten, only the derived budget.
    pub shortfall_count: u32,
}

/// Truncation heuristic for [`ContextWindowEvidence`]. Estimates are chars/4
/// fuzzy, so a bare `reported < estimated` would cry wolf on every model whose
/// tokenizer beats the heuristic. Require the reported count to fall below the
/// resolved window AND either miss the estimate by a wide margin (>= 1/8 of the
/// estimate, minimum 256 tokens) or sit exactly on a power-of-two boundary of
/// at least 2048 — the shape of a clamped server-side `num_ctx`.
fn context_window_shortfall(reported: u64, estimated_sent: u64, window: u64) -> bool {
    if reported == 0 || reported >= window || reported >= estimated_sent {
        return false;
    }
    let shortfall = estimated_sent - reported;
    let wide_margin = shortfall >= (estimated_sent / 8).max(256);
    let clamped_boundary = reported.is_power_of_two() && reported >= 2048;
    // A clamp hypothesis claims the server's real window IS `reported`. When
    // that implied window is under a quarter of the resolved one and sits on
    // no clamp-shaped boundary, the far likelier story is the chars/4
    // estimator overshooting an accurate tokenizer (hosted routers report
    // exact counts) — cycle-14's HF audit saw this warning on nearly every
    // turn. Stay quiet rather than crying wolf.
    let plausible_clamp = reported.saturating_mul(4) >= window;
    clamped_boundary || (wide_margin && plausible_clamp)
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
    /// Subset of `tool_errors` caused by an explicit local authority denial.
    pub policy_errors: u32,
    /// Tool errors in the LAST round that executed tool calls. Zero when the
    /// model recovered from earlier failures in a later round — the normal
    /// agentic retry pattern (e.g. an `fs_edit_file` precondition conflict
    /// followed by a corrected, successful edit). Strict callers should judge
    /// the turn's final state by this, not by the cumulative `tool_errors`.
    pub final_round_tool_errors: u32,
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
    let turn_start = std::time::Instant::now();
    let mut turn_usage = kitsu_providers::Usage::default();
    state.last_turn = AgentTurnStatus::default();
    if let Some(budget) = &cfg.spawn_budget {
        budget.reset();
    }
    if !cfg.quiet {
        // Reset the per-turn stdout marker so error handling can tell whether
        // THIS turn streamed provisional output (quiet subagents never print).
        kitsu_render::begin_turn_output();
    }
    let result = run_agent_turn_inner(provider, tools, messages, cfg, &mut turn_usage, state).await;
    let elapsed = turn_start.elapsed();
    state.run_usage.prompt_tokens += turn_usage.prompt_tokens;
    state.run_usage.completion_tokens += turn_usage.completion_tokens;
    state.run_usage.total_tokens += turn_usage.total_tokens;
    state.run_turns += 1;
    if let Some(tallies) = &cfg.run_tallies {
        tallies
            .prompt_tokens
            .fetch_add(turn_usage.prompt_tokens, Ordering::AcqRel);
        tallies
            .completion_tokens
            .fetch_add(turn_usage.completion_tokens, Ordering::AcqRel);
        tallies
            .total_tokens
            .fetch_add(turn_usage.total_tokens, Ordering::AcqRel);
        tallies.turns.fetch_add(1, Ordering::AcqRel);
        if !turn_usage.is_empty() {
            let mut per_model = tallies
                .per_model
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let bin = per_model.entry(cfg.model.clone()).or_insert((0, 0, 0));
            bin.0 += turn_usage.prompt_tokens;
            bin.1 += turn_usage.completion_tokens;
            bin.2 += turn_usage.total_tokens;
        }
    }
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
            policy_errors: state.last_turn.policy_errors,
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
            // P6: report the effective budget actually gating requests, not the
            // raw resolved window, once shortfall evidence has clamped it.
            let budget = ContextBudget::new(
                effective_budget_window(window, state.window_evidence),
                cfg.max_tokens,
            );
            (state.meter.estimate(messages), budget.effective, window)
        });
        if result.is_ok() {
            kitsu_render::emit_turn_stats(usage, context, Some(elapsed));
        } else {
            // A failed turn gets no human stats line — the error notice owns
            // that moment — but redirected runs keep the machine records.
            kitsu_render::emit_failed_turn_stats(usage, context, Some(elapsed));
        }
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
    if cfg.defer_response_format_until_final && cfg.response_format.is_some() && cfg.max_rounds < 2
    {
        return Err(AgentError::Loop(
            "deferred structured output needs at least two rounds (tool action plus finalization)"
                .to_string(),
        ));
    }
    let AgentSessionState {
        meter,
        compact_failures,
        compaction_ledger,
        last_turn: turn_status,
        window_evidence,
        run_reads,
        run_usage: _,
        run_turns: _,
        schema_cache,
    } = state;
    let mut last_assistant_text = String::new();
    let mut round: u32 = 0;
    // One bounded re-prompt per turn when provider-mangled chat-template
    // debris is detected in a final answer (sibling of the malformed-JSON
    // text-call recovery below).
    let mut debris_retry_used = false;

    // ISSUE-007 loop-breaker: track consecutive identical tool-call failures so a
    // model that fixates on a failing call (e.g. a denied path) is forced to a
    // final answer instead of burning every round. Signature = tool_name + args.
    let mut consecutive_failures: std::collections::HashMap<String, u32> =
        std::collections::HashMap::new();
    // Cycle-27 audit: nine byte-identical SUCCESSFUL workspace_diff calls
    // cost 6.7x tokens — the failure breaker never sees successful repeats.
    // After this many identical successes, further identical calls are
    // answered with a short reminder instead of re-executing.
    const REPEAT_SUCCESS_LIMIT: u32 = 3;
    let mut consecutive_successes: std::collections::HashMap<String, u32> =
        std::collections::HashMap::new();
    let mut force_final_next = false;
    let mut force_structured_final_next = false;
    // A deferred action response and the host-authored finalization nudge are
    // request-local scaffolding, not a second user turn. Retain the provisional
    // assistant long enough to build the final provider request, then remove
    // both messages before appending the committed final assistant.
    let mut structured_handoff_assistant: Option<ChatMessage> = None;
    // An exact-output repair instruction is visible to one provider request,
    // then removed before that request's real assistant/tool transcript is
    // appended. Like the finalization handoff, it is host scaffolding rather
    // than a durable user turn.
    let mut structured_repair_nudge: Option<String> = None;
    const REPEAT_FAIL_LIMIT: u32 = 2;
    // Cycle-23 audit: the spawn cap's polite refusal was ignored 6x by a
    // hosted model. After this many refusals in one turn the spawn tools are
    // WITHHELD for the turn's remainder (ISSUE-007 pattern applied to
    // spawning) — the model keeps its other tools and must work inline.
    const SPAWN_REFUSAL_LIMIT: u32 = 2;
    let mut spawn_cap_refusals: u32 = 0;
    let mut spawn_tools_withheld = false;

    loop {
        round += 1;
        if round > cfg.max_rounds {
            // (2) max-rounds backstop: stop gracefully, note it, return text.
            last_assistant_text.push_str("\n[stopped: max tool rounds reached]");
            turn_status.completion = TurnCompletion::MaxRounds;
            if cfg.stream && !last_assistant_text.is_empty() {
                if let Some(log) = &cfg.event_log {
                    log.record(AgentRuntimeEvent::AssistantDelta {
                        round: round.saturating_sub(1).max(1),
                        text: last_assistant_text.clone(),
                    });
                }
                abort_if_event_sink_failed(cfg)?;
            }
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
        let is_final_round = (round == cfg.max_rounds && cfg.max_rounds > 1)
            || force_final_next
            || force_structured_final_next;
        // Freeze schemas and handler identities together for this provider
        // request. The response must resolve every structured or recovered call
        // through this exact inventory, never a later registry view.
        let request_inventory = tools.inventory();
        let offered_tools = if is_final_round {
            None
        } else if spawn_tools_withheld {
            tool_descriptors(&request_inventory).map(|descriptors| {
                descriptors
                    .into_iter()
                    .filter(|descriptor| !descriptor.name.starts_with("spawn_agent"))
                    .collect::<Vec<_>>()
            })
        } else {
            tool_descriptors(&request_inventory)
        };
        if is_final_round && tool_descriptors(&request_inventory).is_some() {
            let nudge = if force_structured_final_next {
                STRUCTURED_FINALIZATION_NUDGE
            } else if force_final_next {
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
        let response_format = if cfg.defer_response_format_until_final && tools_were_offered {
            None
        } else {
            cfg.response_format.clone()
        };
        if cfg.defer_response_format_until_final && !tools_were_offered && response_format.is_some()
        {
            // The preceding action request was metered with tool schemas, while
            // this reserved final request carries a different response schema.
            // A ContextMeter anchor only records whether constraints existed,
            // not their identity or size, so retaining that anchor could make
            // the final budget reuse the tool-schema cost and omit a larger
            // response schema. Re-estimate this one constraint transition from
            // the concrete messages and schema before crossing the provider
            // boundary; the final usage report will establish a fresh anchor.
            meter.invalidate();
        }
        let schema_json =
            serialized_request_constraints(offered_tools.as_deref(), response_format.as_ref())?;
        let request_constraints_present = tools_were_offered || response_format.is_some();

        // Budget only after every request-visible mutation (notably the final
        // round nudge above), and against the schema actually offered THIS
        // round. This establishes the invariant the concrete request below is
        // required to satisfy.
        enforce_context_budget_recording(
            provider,
            messages,
            cfg,
            &schema_json,
            request_constraints_present,
            meter,
            turn_usage,
            compact_failures,
            compaction_ledger,
            round,
            CompactionReason::ContextTargetExceeded,
            false,
            *window_evidence,
        )
        .await?;

        // Context enforcement may compact earlier exchanges. Resolve the
        // exact request-local suffix only after that mutation and before the
        // request clone. The last-user preservation invariant keeps the host
        // nudge present; the provisional assistant may already have been
        // compacted away, in which case only the nudge needs removal.
        let structured_finalization_ephemera = if force_structured_final_next {
            let nudge_index = messages
                .iter()
                .rposition(|message| {
                    message.role == Role::User && message.content == STRUCTURED_FINALIZATION_NUDGE
                })
                .ok_or_else(|| {
                    AgentError::Loop(
                        "structured-finalization context lost its host handoff".to_string(),
                    )
                })?;
            let start = structured_handoff_assistant
                .as_ref()
                .filter(|provisional| {
                    nudge_index > 0 && messages.get(nudge_index - 1) == Some(*provisional)
                })
                .map_or(nudge_index, |_| nudge_index - 1);
            Some(start..nudge_index + 1)
        } else {
            None
        };

        // P9: the budget promised `effective = window - reserve`, so the reply
        // the provider is told it may send must be bounded by that same clamped
        // reserve. Clamp down only: a smaller caller-supplied max_tokens is left
        // alone, and None stays None (no artificial cap is invented).
        let max_tokens = match cfg.context_window {
            Some(window) => {
                let budget = ContextBudget::new(
                    effective_budget_window(window, *window_evidence),
                    cfg.max_tokens,
                );
                clamp_max_tokens_to_reserve(cfg.max_tokens, &budget)
            }
            // No resolved window: no budget to clamp against; send as-is.
            None => cfg.max_tokens,
        };

        let req = ChatRequest {
            model: cfg.model.clone(),
            messages: messages.clone(),
            tools: offered_tools,
            stream: cfg.stream,
            temperature: cfg.temperature,
            reasoning_effort: cfg.reasoning_effort,
            max_tokens,
            response_format,
            prompt_cache_key: None,
        };
        let request_history_reshaped = if let Some(nudge) = structured_repair_nudge.take() {
            let nudge_index = messages
                .iter()
                .rposition(|message| message.role == Role::User && message.content == nudge)
                .ok_or_else(|| {
                    AgentError::Loop(
                        "structured-finalization repair context lost its host instruction"
                            .to_string(),
                    )
                })?;
            messages.remove(nudge_index);
            // The concrete request includes a message that durable history no
            // longer does. Do not let its usage report become a reusable prefix
            // anchor for the cleaned transcript.
            meter.invalidate();
            true
        } else {
            false
        };

        // Keep the final check adjacent to the provider boundary. If a future
        // edit adds another message or tool mutation between compaction and
        // request construction, this check fails locally instead of leaking an
        // oversized request to the gateway.
        let sent_measurement =
            ensure_chat_request_fits(&req, cfg, meter, *window_evidence, schema_cache)?;
        // Token-denominated estimate of the request being sent, kept so the
        // usage report below can be cross-checked against what left the door.
        let sent_estimate_tokens =
            (sent_measurement.unit == ContextUnit::Tokens).then_some(sent_measurement.used);
        if let Some(log) = &cfg.event_log {
            log.record(AgentRuntimeEvent::ProviderRequest {
                round,
                purpose: ProviderRequestPurpose::Inference,
                model: Some(cfg.model.clone()),
                message_count: req.messages.len(),
                tools_offered: req.tools.as_ref().map_or(0, Vec::len),
                structured_output: req.response_format.is_some(),
                request_timeout_ms: cfg.request_timeout_ms,
                turn_remaining_ms: remaining_turn_millis(cfg.turn_deadline),
            });
        }
        abort_if_event_sink_failed(cfg)?;

        // "Vibing kitsu" while we wait for the model. No-op off a TTY; cleared on
        // the first stream item when text streams live, otherwise when the
        // stream ends (or on drop if the request errors).
        let mut spinner = if cfg.quiet {
            None
        } else {
            Some(kitsu_render::Spinner::start())
        };

        let usage_tracker = cfg.run_tallies.as_ref().map(|tallies| {
            ProviderResponseUsageTracker::new(
                std::sync::Arc::clone(tallies),
                ProviderRequestPurpose::Inference,
            )
        });

        // 401/403 surface here (or as the first stream item) as a hard error.
        let stream_result = provider
            .chat_stream_with_telemetry(
                req,
                provider_telemetry_sink(
                    cfg.event_log.as_ref(),
                    round,
                    ProviderRequestPurpose::Inference,
                    cfg.turn_deadline,
                    usage_tracker.clone(),
                ),
            )
            .await;
        // A fallible observer can revoke retries while the provider future is
        // still inside its transport loop. Prefer that local automation error
        // over the adapter's private stop signal.
        abort_if_event_sink_failed(cfg)?;
        let mut stream = stream_result?;

        let mut assistant_text = String::new();
        let mut tool_calls: Vec<ToolCall> = Vec::new();
        let mut finish = FinishReason::Stop;

        // With tools offered the reply is buffered (ISSUE-002), so the first
        // item shows nothing; keep the spinner up until the stream ends rather
        // than leaving a bare prompt that looks idle mid-turn.
        let streams_live = cfg.stream && !tools_were_offered;
        while let Some(item) = stream.next().await {
            if streams_live {
                if let Some(s) = spinner.take() {
                    s.stop();
                }
            }
            match item? {
                ChatDelta::Text(t) => {
                    // ISSUE-002: when tools are advertised this round, DON'T stream
                    // text live — buffer it. A text-protocol model may be emitting
                    // a tool call as JSON; we suppress that and only print real
                    // answers (below). The final round withholds tools, so final
                    // answers still stream live.
                    if cfg.stream && !tools_were_offered {
                        if !cfg.quiet {
                            kitsu_render::print_fragment(&t);
                        }
                        if let Some(log) = &cfg.event_log {
                            log.record(AgentRuntimeEvent::AssistantDelta {
                                round,
                                text: t.clone(),
                            });
                        }
                        abort_if_event_sink_failed(cfg)?;
                    }
                    assistant_text.push_str(&t);
                }
                ChatDelta::ToolCall(tc) => tool_calls.push(tc),
                ChatDelta::Usage(u) => {
                    if let Some(tracker) = &usage_tracker {
                        tracker.record_reported(u);
                    }
                    // Anchor the meter on this response's real prompt token
                    // count. `request_constraints_present` records whether THIS
                    // request included tool and/or response schemas, so a later
                    // round can tell whether the anchor already accounts for it (see
                    // ContextMeter::anchor_includes_schema).
                    if request_history_reshaped {
                        meter.invalidate();
                    } else {
                        meter.record(
                            u.prompt_tokens,
                            messages,
                            request_constraints_present,
                            &schema_json,
                        );
                    }
                    turn_usage.add(u);
                    // Cross-check the resolved window against runtime truth:
                    // a usage report far below the sent estimate (and below
                    // the window) means the server truncated the prompt to a
                    // smaller real window. Record once per resolved window;
                    // the CLI warns and surfaces it in /context provenance.
                    if let (Some(window), Some(sent_estimate)) =
                        (cfg.context_window, sent_estimate_tokens)
                    {
                        if context_window_shortfall(u.prompt_tokens, sent_estimate, window) {
                            match window_evidence {
                                // Same resolved window as the last shortfall: count this
                                // as another strike (P6 two-strike rule) and refresh the
                                // reported numbers to the latest observation.
                                Some(evidence) if evidence.resolved_window == window => {
                                    evidence.shortfall_count =
                                        evidence.shortfall_count.saturating_add(1);
                                    evidence.reported_prompt_tokens = u.prompt_tokens;
                                    evidence.estimated_sent_tokens = sent_estimate;
                                }
                                // First shortfall ever, or the resolved window changed
                                // (new gateway/model/`--context-window`): start a fresh
                                // strike count for the new window.
                                _ => {
                                    *window_evidence = Some(ContextWindowEvidence {
                                        reported_prompt_tokens: u.prompt_tokens,
                                        estimated_sent_tokens: sent_estimate,
                                        resolved_window: window,
                                        warned: false,
                                        shortfall_count: 1,
                                    });
                                }
                            }
                        }
                    }
                    if let Some(log) = &cfg.event_log {
                        log.record(AgentRuntimeEvent::Usage {
                            round,
                            purpose: ProviderRequestPurpose::Inference,
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
        if let Some(s) = spinner.take() {
            s.stop();
        }
        if let Some(tracker) = &usage_tracker {
            tracker.record_completed();
        }

        if cfg.defer_response_format_until_final && !tools_were_offered && !tool_calls.is_empty() {
            return Err(AgentError::ToolCallDuringToolFreeFinalization);
        }

        // Text-protocol fallback (ISSUE-001): some models — especially local ones
        // via Ollama — emit a tool call as plain-text JSON in `content` instead of
        // structured `tool_calls`. If tools were advertised this round and none
        // came back structured, try to parse one from the text and execute it.
        let mut synthesized_call = false;
        let mut malformed_text_calls = 0usize;
        let mut skill_confusion: Option<String> = None;
        if tool_calls.is_empty() && tools_were_offered {
            let recovered =
                parse_text_tool_calls(&assistant_text, &request_inventory, &cfg.skill_names);
            if !recovered.calls.is_empty() {
                tracing::debug!(
                    count = recovered.calls.len(),
                    "recovered text-protocol tool call(s)"
                );
                tool_calls.extend(recovered.calls);
                synthesized_call = true;
            } else if recovered.malformed > 0 {
                // A tool attempt too broken to repair. Suppress the raw JSON
                // (it is not an answer) and spend the round on corrective
                // feedback instead of committing garbage as the result.
                malformed_text_calls = recovered.malformed;
                synthesized_call = true;
            } else if recovered.skill_confusion.is_some() {
                // A skill invoked as a fake tool — same treatment, with a
                // targeted correction naming the real path.
                skill_confusion = recovered.skill_confusion;
                synthesized_call = true;
            }
        }

        // A combined tool-action/strict-output request biases some smaller
        // models toward a schema-valid placeholder instead of acting. In the
        // deferred mode, a no-tool response from an action round is only a
        // provisional handoff: retain it in model context, suppress it from
        // committed output, and spend the next bounded round on a tool-free
        // provider-native structured final answer.
        let needs_structured_finalization = cfg.defer_response_format_until_final
            && cfg.response_format.is_some()
            && tools_were_offered
            && tool_calls.is_empty()
            && malformed_text_calls == 0
            && skill_confusion.is_none()
            && finish != FinishReason::Length;

        // ISSUE-002 / ISSUE-008: this round's text was buffered (tools were
        // offered). Print it now UNLESS it was a text-protocol tool call we
        // synthesized — that text IS the raw JSON, so suppress it. A model's real
        // reasoning before a STRUCTURED tool call, and terminal answers, print
        // normally (previously over-suppressed, hiding read/grep output).
        if cfg.stream && tools_were_offered && !synthesized_call && !needs_structured_finalization {
            if !cfg.quiet {
                kitsu_render::print_message(&assistant_text);
            }
            // Text accompanying a structured tool call is intermediate
            // narration, not the committed answer returned by this turn. Keep
            // the human narration, but do not label it as assistant output in
            // automation: concatenated deltas must reproduce `assistant.text`.
            if tool_calls.is_empty() && !assistant_text.is_empty() {
                if let Some(log) = &cfg.event_log {
                    log.record(AgentRuntimeEvent::AssistantDelta {
                        round,
                        text: assistant_text.clone(),
                    });
                }
                abort_if_event_sink_failed(cfg)?;
            }
        }

        // Before consuming the tool-free structured finalizer, give a
        // host-verified exact-output mismatch another bounded action round.
        // Two rounds must remain: one for repair and one reserved finalizer.
        // When the budget cannot accommodate both, preserve the existing
        // fail-closed path and let terminal postcondition evaluation reject the
        // mismatch after the final response.
        if needs_structured_finalization && round.saturating_add(2) <= cfg.max_rounds {
            if let Some(repair) = cfg
                .structured_finalization_gate
                .as_ref()
                .and_then(StructuredFinalizationGate::evaluate)
            {
                if let Some(log) = &cfg.event_log {
                    log.record(AgentRuntimeEvent::StructuredFinalizationDeferred {
                        round,
                        repair_round: round.saturating_add(1),
                        finalization_round_reserved: true,
                        cause: repair.cause,
                        targets: repair.targets,
                        repair_instruction_disclosed: true,
                        write_authority_granted: false,
                    });
                }
                abort_if_event_sink_failed(cfg)?;
                structured_repair_nudge = Some(repair.instruction.clone());
                messages.push(ChatMessage::user(repair.instruction));
                meter.invalidate();
                continue;
            }
        }

        // Record the assistant turn. When we synthesized a call from text, store
        // empty content so the raw tool-call JSON doesn't pollute the history.
        let assistant_content = if synthesized_call {
            String::new()
        } else {
            assistant_text.clone()
        };
        if let Some(ephemera) = structured_finalization_ephemera {
            messages.drain(ephemera);
            structured_handoff_assistant = None;
            // The latest usage anchor describes the provider request WITH the
            // removed scaffolding. It is no longer a prefix of committed
            // history, so the next turn must remeasure instead of reusing it.
            meter.invalidate();
        }
        last_assistant_text = assistant_content.clone();
        messages.push(ChatMessage::assistant(
            assistant_content,
            tool_calls.clone(),
        ));

        if tool_calls.is_empty() && skill_confusion.is_some() {
            let skill = skill_confusion.take().unwrap_or_default();
            turn_status.tool_errors += 1;
            if !cfg.quiet {
                kitsu_render::tool_result_error(&format!(
                    "ignored a text-form call to skill '{skill}' (skills are not tools); asked the model to load it via the skill tool"
                ));
            }
            messages.push(ChatMessage::user(format!(
                "'{skill}' is a SKILL, not a tool, and was NOT invoked. Load it by calling the \
                 `skill` tool as a structured tool call with arguments {{\"name\": \"{skill}\"}}, \
                 then follow the returned instructions."
            )));
            continue;
        }

        if tool_calls.is_empty() && malformed_text_calls > 0 {
            // Cycle-12 audit: a malformed text-form tool attempt must neither
            // masquerade as the committed answer nor vanish silently — the
            // user sees an ignored-call receipt, the model gets told exactly
            // how to re-issue, and the retry stays bounded by max_rounds.
            turn_status.tool_errors += malformed_text_calls as u32;
            if !cfg.quiet {
                kitsu_render::tool_result_error(&format!(
                    "ignored {malformed_text_calls} malformed text-form tool call(s) (unparseable JSON); asked the model to re-issue"
                ));
            }
            messages.push(ChatMessage::user(
                "Your last message contained a text-form tool call whose JSON could not be parsed \
                 (for example an invalid escape sequence). It was NOT executed. Re-issue it as a \
                 proper structured tool call, or as strictly valid JSON of the form \
                 {\"name\": \"<tool_name>\", \"arguments\": { ... }} with no surrounding prose."
                    .to_string(),
            ));
            continue;
        }

        if needs_structured_finalization {
            structured_handoff_assistant = messages.last().cloned();
            force_structured_final_next = true;
            continue;
        }

        if tool_calls.is_empty() {
            // (1) terminal: model stopped with no tool calls.
            match finish {
                FinishReason::Stop => {
                    if !debris_retry_used && template_control_debris_in_tail(&assistant_text) {
                        // A provider failed to parse the model's native-format
                        // trailing tool call and leaked its remains as text.
                        // Don't accept the destroyed action silently: tell the
                        // model and let it re-issue the call properly, once.
                        debris_retry_used = true;
                        turn_status.tool_errors += 1;
                        if !cfg.quiet {
                            kitsu_render::tool_result_error(
                                "response tail contains chat-template control tokens — a trailing tool call was likely destroyed by the provider; asked the model to re-issue it",
                            );
                        }
                        messages.push(ChatMessage::assistant(assistant_text.clone(), Vec::new()));
                        messages.push(ChatMessage::user(
                            "Your last message ended in raw chat-template control tokens (like <|...|>) — a tool call you attempted was NOT executed because the serving provider failed to parse it. Re-issue that action as a proper structured tool call now, without repeating the prose."
                                .to_string(),
                        ));
                        assistant_text.clear();
                        continue;
                    }
                    return Ok(assistant_text);
                }
                // Truncated by the model's output cap — surface it instead of
                // letting a cut-off answer masquerade as a complete one.
                FinishReason::Length => {
                    tracing::warn!("response truncated: model hit its output length limit");
                    const MARKER: &str = "\n[truncated: model output length limit reached]";
                    assistant_text.push_str(MARKER);
                    if cfg.stream && !cfg.quiet {
                        kitsu_render::print_message(MARKER);
                    }
                    if cfg.stream {
                        if let Some(log) = &cfg.event_log {
                            log.record(AgentRuntimeEvent::AssistantDelta {
                                round,
                                text: MARKER.to_string(),
                            });
                        }
                        abort_if_event_sink_failed(cfg)?;
                    }
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
        // Each tool round overwrites `final_round_tool_errors`, so after the turn
        // it reports the LAST executed tool round — zero when the model recovered
        // from earlier failures before finishing.
        turn_status.final_round_tool_errors = 0;
        for tc in tool_calls {
            // Narrate the action, e.g. `◆ Read(deck1.md)`.
            if !cfg.quiet {
                kitsu_render::tool_activity(&format_tool_header(&tc.tool_name, &tc.args));
            }
            let tool_event = ToolEventGuard::start(
                cfg.event_log.as_ref(),
                round,
                &tc.call_id,
                &tc.tool_name,
                &tc.args,
                cfg.artifact_policy,
            );
            abort_if_event_sink_failed(cfg)?;
            let sig = format!("{}\u{0}{}", tc.tool_name, tc.args);
            let call_started = std::time::Instant::now();
            let repeat_count = consecutive_successes.get(&sig).copied().unwrap_or(0);
            let tool_result = if repeat_count >= REPEAT_SUCCESS_LIMIT {
                // Identical successful call repeated past the limit: answer
                // with a reminder instead of re-running (and re-paying for)
                // the same result.
                Ok(format!(
                    "[identical call repeated: this exact {} call already succeeded {repeat_count} time(s) this turn and its result is above — use it, or change the arguments]",
                    tc.tool_name
                ))
            } else {
                match tool_event.command_progress_sink() {
                    Some(sink) => {
                        scope_command_progress(
                            sink,
                            request_inventory.run(&tc.tool_name, tc.args.clone()),
                        )
                        .await
                    }
                    None => request_inventory.run(&tc.tool_name, tc.args.clone()).await,
                }
            };
            let (content, is_err) = match tool_result {
                Ok(out) => {
                    consecutive_failures.remove(&sig); // a success resets the streak
                    *consecutive_successes.entry(sig.clone()).or_insert(0) += 1;
                    (out, false)
                }
                Err(kitsu_tools::ToolError::Interrupted(
                    kitsu_tools::ToolInterruption::AuthorityInputClosed,
                )) => {
                    let content = serde_json::json!({
                        "error": "Interrupted",
                        "detail": "authority input closed before a decision"
                    })
                    .to_string();
                    tool_event.finish(false, &content, ToolOutputRetention::complete(&content));
                    abort_if_event_sink_failed(cfg)?;
                    return Err(AgentError::Interrupted(
                        crate::run::RunInterruption::AuthorityInputClosed,
                    ));
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
                    turn_status.final_round_tool_errors =
                        turn_status.final_round_tool_errors.saturating_add(1);
                    if matches!(e, kitsu_tools::ToolError::PolicyDenied(_)) {
                        turn_status.policy_errors = turn_status.policy_errors.saturating_add(1);
                        if let Some(tallies) = &cfg.run_tallies {
                            tallies.denied.fetch_add(1, Ordering::AcqRel);
                        }
                    }
                    if tc.tool_name.starts_with("spawn_agent")
                        && e.to_string().contains("agent spawn cap reached")
                    {
                        spawn_cap_refusals = spawn_cap_refusals.saturating_add(1);
                        if spawn_cap_refusals >= SPAWN_REFUSAL_LIMIT && !spawn_tools_withheld {
                            spawn_tools_withheld = true;
                            if !cfg.quiet {
                                kitsu_render::tool_result_error(
                                    "spawn cap refusals ignored — spawn tools withheld for the rest of this turn; the model keeps its other tools",
                                );
                            }
                            messages.push(ChatMessage::user(
                                "The agent spawn cap was reached and further spawn attempts were refused. \
                                 Spawn tools are now WITHDRAWN for the rest of this turn: do the remaining \
                                 work yourself with your other tools, then give your final answer."
                                    .to_string(),
                            ));
                        }
                    }
                    (
                        serde_json::json!({ "error": e.kind(), "detail": e.to_string() })
                            .to_string(),
                        true,
                    )
                }
            };
            // Describe the result BEFORE any offload substitution, but print
            // AFTER the decision so the receipt can disclose the swap — a
            // `◇ 1828 lines` line over a 500-char preview is a lie the user
            // has no way to detect (cycle-15 headline).
            let summary = (!cfg.quiet).then(|| {
                let mut summary = format_tool_result(&tc.tool_name, &tc.args, &content, is_err);
                // UI-parity item 3: every receipt carries its duration, except
                // command_exec whose result already reports its own elapsed.
                if !matches!(tc.tool_name.as_str(), "command_exec" | "git") {
                    summary.push_str(&format!(
                        " · {}",
                        format_tool_elapsed(call_started.elapsed())
                    ));
                }
                summary
            });
            if !is_err && tc.tool_name == "fs_read_file" {
                *run_reads += 1;
            }
            if let Some(tallies) = &cfg.run_tallies {
                if is_err {
                    tallies.failed.fetch_add(1, Ordering::AcqRel);
                } else if tc.tool_name == "fs_read_file" {
                    tallies.reads.fetch_add(1, Ordering::AcqRel);
                }
            }
            // Commands already carry independently bounded stdout/stderr
            // previews plus exact counters. Generic results are either
            // pressure-offloaded under the default policy or irreversibly
            // reduced to a deterministic head/tail preview in artifact-free
            // mode.
            let prepared = prepare_tool_output(
                content,
                ToolOutputRequest {
                    call_id: &tc.call_id,
                    tool_name: &tc.tool_name,
                    is_err,
                    window_evidence: *window_evidence,
                },
                &request_inventory,
                messages,
                cfg,
            );
            if let Some(mut summary) = summary {
                if let Some(path) = &prepared.artifact_path {
                    summary.push_str(&format!(
                        " → context got a {OFFLOAD_PREVIEW_CHARS}-char preview; full text at {} (model can fs_read_file it)",
                        path.display()
                    ));
                }
                if is_err {
                    kitsu_render::tool_result_error(&summary);
                } else {
                    kitsu_render::tool_result(&summary);
                }
            }
            tool_event.finish(!is_err, &prepared.content, prepared.retention);
            abort_if_event_sink_failed(cfg)?;
            messages.push(ChatMessage::tool(tc.call_id.clone(), prepared.content));
        }
        // continue to next round
    }
}

/// Build the tool descriptor list advertised to the model. Tools registered
/// with a descriptor are surfaced; an empty set means no `tools` field is sent
/// at all (`None`), preserving the descriptor-free `ask` path.
trait ToolCatalog {
    fn descriptors(&self) -> &[ToolDescriptor];
    fn is_registered(&self, name: &str) -> bool;
}

impl ToolCatalog for Registry {
    fn descriptors(&self) -> &[ToolDescriptor] {
        self.descriptors()
    }

    fn is_registered(&self, name: &str) -> bool {
        self.is_registered(name)
    }
}

impl ToolCatalog for ToolInventory {
    fn descriptors(&self) -> &[ToolDescriptor] {
        self.descriptors()
    }

    fn is_registered(&self, name: &str) -> bool {
        self.is_registered(name)
    }
}

fn tool_descriptors(tools: &impl ToolCatalog) -> Option<Vec<kitsu_providers::ToolDescriptor>> {
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
/// What scanning assistant text for text-protocol tool calls found.
struct TextToolRecovery {
    calls: Vec<ToolCall>,
    /// Candidates that named a registered tool but stayed unparseable even
    /// after escape repair. They were NOT executed; the caller owes the model
    /// corrective feedback instead of printing the raw JSON as the answer.
    malformed: usize,
    /// A well-formed call whose `name` is a known SKILL, not a tool — the
    /// cycle-14 pattern where a model invokes `{"name": "code-review", …}`
    /// directly instead of loading it through the `skill` tool. Carries the
    /// skill name for a targeted corrective message.
    skill_confusion: Option<String>,
}

/// Repair the common invalid-escape faults small models emit inside otherwise
/// well-shaped tool-call JSON (`\'` in particular): inside string literals,
/// a backslash before a character that is not a valid JSON escape is dropped,
/// keeping the character. Everything else passes through untouched.
fn relax_json_escapes(candidate: &str) -> String {
    let mut out = String::with_capacity(candidate.len());
    let mut chars = candidate.chars();
    let mut in_string = false;
    while let Some(c) = chars.next() {
        if in_string && c == '\\' {
            match chars.next() {
                Some(next)
                    if matches!(next, '"' | '\\' | '/' | 'b' | 'f' | 'n' | 'r' | 't' | 'u') =>
                {
                    out.push('\\');
                    out.push(next);
                }
                Some(next) => out.push(next),
                None => {}
            }
            continue;
        }
        if c == '"' {
            in_string = !in_string;
        }
        out.push(c);
    }
    out
}

fn parse_text_tool_calls(
    text: &str,
    tools: &impl ToolCatalog,
    skill_names: &[String],
) -> TextToolRecovery {
    let descriptors = tools.descriptors();
    let mut calls = Vec::new();
    let mut malformed = 0usize;
    let mut skill_confusion = None;
    if descriptors.is_empty() {
        return TextToolRecovery {
            calls,
            malformed,
            skill_confusion,
        };
    }
    for candidate in extract_json_objects(text) {
        let parsed = serde_json::from_str::<serde_json::Value>(&candidate).or_else(|_| {
            serde_json::from_str::<serde_json::Value>(&relax_json_escapes(&candidate))
        });
        let Ok(value) = parsed else {
            // Narrow detection to keep false positives out: only an object
            // that has a "name" key AND quotes a registered tool name counts
            // as a failed tool attempt (arbitrary JSON in a real answer does
            // not trip this).
            if candidate.contains("\"name\"")
                && descriptors
                    .iter()
                    .any(|d| candidate.contains(&format!("\"{}\"", d.name)))
            {
                malformed += 1;
            }
            continue;
        };
        let Some(obj) = value.as_object() else {
            continue;
        };
        let Some(name) = obj.get("name").and_then(|n| n.as_str()) else {
            continue;
        };
        if !descriptors.iter().any(|d| d.name == name) {
            if skill_confusion.is_none() && skill_names.iter().any(|skill| skill == name) {
                skill_confusion = Some(name.to_string());
            }
            continue;
        }
        // `arguments` may be an object or a JSON-encoded string; `parameters` is a
        // common alias. Default to `{}` when the model omitted args entirely.
        let args = match obj.get("arguments").or_else(|| obj.get("parameters")) {
            Some(serde_json::Value::String(s)) => s.clone(),
            Some(v @ serde_json::Value::Object(_)) => v.to_string(),
            _ => "{}".to_string(),
        };
        let call_id = format!("textcall-{}", calls.len());
        calls.push(ToolCall {
            tool_name: name.to_string(),
            args,
            call_id,
        });
    }
    // An UNBALANCED attempt — truncated output, a stray bracket — produces no
    // balanced candidate at all, so the per-candidate malformed detection above
    // never sees it, and the raw JSON used to fall through every guard and be
    // COMMITTED as the final answer with the run reporting success (observed
    // live: llama3.2 closed an fs_write_file call with `"})}` and kitsu
    // reported "succeeded" having written nothing). Same narrow signature as
    // the per-candidate check, applied to the whole text: an object opener
    // plus "name" quoting a registered tool is a failed tool attempt, not an
    // answer.
    if calls.is_empty()
        && malformed == 0
        && skill_confusion.is_none()
        && text.contains('{')
        && text.contains("\"name\"")
        && descriptors
            .iter()
            .any(|d| text.contains(&format!("\"{}\"", d.name)))
    {
        malformed += 1;
    }
    TextToolRecovery {
        calls,
        malformed,
        skill_confusion,
    }
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
    let args = match name {
        "command_exec" => serde_json::from_str::<CommandExecArgs>(args).ok()?,
        "git" => {
            let mut value: serde_json::Value = serde_json::from_str(args).ok()?;
            let object = value.as_object_mut()?;
            let suffix = object.remove("args")?;
            let mut argv = vec![serde_json::Value::String("git".to_string())];
            argv.extend(suffix.as_array()?.iter().cloned());
            object.insert("argv".to_string(), serde_json::Value::Array(argv));
            serde_json::from_value::<CommandExecArgs>(value).ok()?
        }
        _ => return None,
    };
    Some(CommandRuntimeInvocation {
        argv: args.argv,
        cwd: args.cwd,
        timeout_ms: args.timeout_ms,
    })
}

fn command_runtime_result(
    output: &str,
    artifact_policy: ArtifactStoragePolicy,
) -> Option<CommandRuntimeResult> {
    let output = serde_json::from_str::<CommandOutput>(output).ok()?;
    Some(command_runtime_result_from_output(output, artifact_policy))
}

fn command_runtime_result_from_output(
    output: CommandOutput,
    artifact_policy: ArtifactStoragePolicy,
) -> CommandRuntimeResult {
    let artifact_status = if artifact_policy == ArtifactStoragePolicy::InlineOnly {
        ToolArtifactStatus::SuppressedByPolicy
    } else if output.artifact.is_some() {
        ToolArtifactStatus::Retained
    } else if output.artifact_error.is_some() {
        ToolArtifactStatus::Failed
    } else if output.stdout_truncated || output.stderr_truncated {
        ToolArtifactStatus::Unavailable
    } else {
        ToolArtifactStatus::NotNeeded
    };
    CommandRuntimeResult {
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
        artifact_status,
        artifact: output.artifact,
        artifact_error: output.artifact_error,
        containment_hint: output.containment_hint,
    }
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
    if matches!(name, "command_exec" | "git") {
        let label = if name == "git" { "Git" } else { "Command" };
        return command_runtime_invocation(name, args)
            .map(|args| {
                let invocation = CommandExecArgs {
                    argv: args.argv,
                    cwd: args.cwd,
                    timeout_ms: args.timeout_ms,
                };
                let argv = bounded_human_value(&invocation.display_argv(), 320);
                let cwd = bounded_human_value(&invocation.cwd, 240);
                format!("{label}(argv={argv}; cwd={cwd:?})")
            })
            .unwrap_or_else(|| format!("{label}(invalid arguments)"));
    }
    let friendly = match name {
        "fs_read_file" => "Read",
        "fs_list_dir" => "List",
        "repo_grep" => "Grep",
        "fs_write_file" => "Write",
        "fs_edit_file" => "Edit",
        "spawn_agent" | "spawn_agents" | "spawn_agent_bg" => "Agent",
        "agent_result" => "AgentResult",
        other => other,
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(args) else {
        return friendly.to_string();
    };
    // The agent tools carry no `path`, so without this arm the single most
    // important receipt in the deck — "a worker was dispatched, doing X" —
    // renders as a bare tool name.
    if matches!(name, "spawn_agent" | "spawn_agent_bg") {
        let task = bounded_human_value(v.get("task").and_then(|x| x.as_str()).unwrap_or(""), 96);
        return match v.get("profile").and_then(|x| x.as_str()) {
            Some(profile) => format!(
                "{friendly}({}: \"{task}\")",
                bounded_human_value(profile, 64)
            ),
            None => format!("{friendly}(\"{task}\")"),
        };
    }
    if name == "spawn_agents" {
        let tasks = v.get("tasks").and_then(|x| x.as_array());
        let count = tasks.map_or(0, Vec::len);
        let labels = tasks
            .map(|tasks| {
                tasks
                    .iter()
                    .filter_map(|task| task.as_str())
                    .map(|task| format!("\"{}\"", bounded_human_value(task, 48)))
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        return format!("{friendly}(x{count}: {labels})");
    }
    if name == "agent_result" {
        let id = v
            .get("id")
            .and_then(serde_json::Value::as_u64)
            .map(|id| id.to_string())
            .unwrap_or_else(|| "?".to_string());
        return format!("{friendly}(#{id})");
    }
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
/// Compact duration for tool receipts: `34ms` under a second, `2.1s` beyond.
fn format_tool_elapsed(elapsed: std::time::Duration) -> String {
    if elapsed.as_secs() == 0
        && elapsed.subsec_millis() < 1000
        && elapsed < std::time::Duration::from_secs(1)
    {
        format!("{}ms", elapsed.as_millis())
    } else {
        format!("{:.1}s", elapsed.as_secs_f64())
    }
}

fn format_tool_result(name: &str, args: &str, content: &str, is_err: bool) -> String {
    if is_err {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(content) {
            if let Some(kind) = v.get("error").and_then(|x| x.as_str()) {
                // Keep the WHOLE detail (newlines flattened) up to a bound,
                // with an explicit ellipsis — the old first-line/180-char cut
                // silently ate the remedy clauses kitsu itself appends
                // (cycle-19 audit #3).
                let raw = v
                    .get("detail")
                    .and_then(|value| value.as_str())
                    .unwrap_or("")
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ");
                let mut detail: String = raw.chars().take(360).collect();
                if raw.chars().count() > 360 {
                    detail.push('…');
                }
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
        "command_exec" | "git" => serde_json::from_str::<CommandOutput>(content)
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
        // "1 line" is useless for a dispatch receipt; the agent tools return a
        // one-sentence status whose first line IS the summary.
        "spawn_agent_bg" | "agent_result" => {
            bounded_human_value(content.lines().next().unwrap_or_default(), 180)
        }
        // A directory listing's unit is entries, not lines (cycle-15 audit).
        "fs_list_dir" => {
            let n = content.lines().count();
            format!("{n} entr{}", if n == 1 { "y" } else { "ies" })
        }
        // A ranged read must SAY it was ranged (cycle-16 audit: a model
        // reading lines 1-4 of a 1905-line file under a small window is
        // silent starvation unless the receipt discloses the slice).
        "fs_read_file" => {
            let truncated = content
                .lines()
                .any(|line| line.starts_with("...[truncated"));
            let n = content
                .lines()
                .filter(|line| !line.starts_with("...[truncated"))
                .count();
            let range = serde_json::from_str::<serde_json::Value>(args)
                .ok()
                .and_then(|value| {
                    let start = value.get("start_line").and_then(|v| v.as_u64());
                    let end = value.get("end_line").and_then(|v| v.as_u64());
                    match (start, end) {
                        (None, None) => None,
                        (start, end) => Some(format!(
                            " (ranged read {}-{})",
                            start.map_or("start".to_string(), |v| v.to_string()),
                            end.map_or("end".to_string(), |v| v.to_string()),
                        )),
                    }
                })
                .unwrap_or_default();
            let capped = if truncated {
                // Quote the served-of-total detail from the marker so the
                // human sees the gap size without opening the raw result.
                content
                    .lines()
                    .find(|line| line.starts_with("...[truncated"))
                    .and_then(|line| line.split(" — ").nth(1))
                    .map(|detail| {
                        format!(
                            ", capped: {}",
                            detail.split(';').next().unwrap_or(detail).trim()
                        )
                    })
                    .unwrap_or_else(|| ", capped at the byte limit".to_string())
            } else {
                String::new()
            };
            format!("{n} line{}{range}{capped}", if n == 1 { "" } else { "s" })
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
    if let Some(status) = &output.artifact_error {
        fields.push(format!(
            "artifact status={}",
            bounded_human_value(status, 160)
        ));
    }
    if let Some(hint) = &output.containment_hint {
        fields.push(format!("hint: {}", bounded_human_value(hint, 480)));
    }
    // The artifact path is the one pointer to the complete output, so it is
    // never elided (every other field is bounded instead) and stays last:
    // when a terminal wraps the receipt, the line ends with a copyable path.
    if let Some(artifact) = &output.artifact {
        fields.push(format!(
            "full output={}",
            terminal_safe_inline(&artifact.reference)
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

/// Compact a path to at most its last three segments (with a leading `…` when more
/// were dropped), so activity lines stay short while preserving crate identity.
/// Cycle-15 G5 finding: distinguish `kitsu-render/src/lib.rs` from `kitsu-core/src/lib.rs`.
fn short_path(p: &str) -> String {
    let parts: Vec<&str> = p
        .trim_end_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();
    match parts.as_slice() {
        [] => p.to_string(),
        [one] if p.starts_with('/') => format!("/{one}"),
        [one] => (*one).to_string(),
        [a, b] => {
            let tail = format!("{a}/{b}");
            if p.starts_with('/') {
                // Keep short absolute paths exact: `/etc/hostname` must not
                // display as the relative-looking `etc/hostname`.
                format!("/{tail}")
            } else {
                tail
            }
        }
        [.., a, b, c] => {
            let tail = format!("{a}/{b}/{c}");
            if parts.len() > 3 {
                format!("…/{tail}")
            } else if p.starts_with('/') {
                format!("/{tail}")
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
    effective: Option<u64>,
    unit: ContextUnit,
}

/// Structural token total for a request shape. `schema_tokens` is the
/// precomputed estimate for the (fixed-per-call) tool schema — P11 threads one
/// computed/cached value through instead of re-running `estimate_str` on every
/// invocation (some callers, e.g. `fit_summary_rebuild`, call this in a loop).
fn structural_request_tokens(messages: &[ChatMessage], schema_tokens: u64) -> u64 {
    messages.iter().map(message_tokens).sum::<u64>() + schema_tokens
}

fn structural_request_bytes(messages: &[ChatMessage], schema_json: &str, has_schema: bool) -> u64 {
    messages.iter().map(|m| m.byte_size() as u64).sum::<u64>()
        + if has_schema {
            schema_json.len() as u64
        } else {
            0
        }
}

/// The window this session should actually budget requests against right now:
/// the resolved `window`, clamped by [`clamp_window_for_shortfall`] once P6's
/// two-strike rule is satisfied for shortfall evidence recorded against this
/// SAME resolved window (a stale `window_evidence` from a since-changed window
/// is ignored). Pure read — never mutates `window` or the evidence; the
/// resolved window and config stay exactly what they always were, only the
/// number handed to [`ContextBudget::new`] adapts.
fn effective_budget_window(window: u64, window_evidence: Option<ContextWindowEvidence>) -> u64 {
    match window_evidence {
        Some(ev) if ev.resolved_window == window => {
            clamp_window_for_shortfall(window, ev.reported_prompt_tokens, ev.shortfall_count)
        }
        _ => window,
    }
}

// P11 added `schema_tokens` (the cached/fresh estimate) alongside the
// pre-existing `schema_json`/`has_schema`, which are still needed here for
// the anchor fingerprint check and the byte-mode fallback — matches the
// `#[allow(clippy::too_many_arguments)]` precedent already used by the other
// measurement-plumbing functions in this module.
#[allow(clippy::too_many_arguments)]
fn request_measurement(
    messages: &[ChatMessage],
    cfg: &AgentConfig,
    schema_json: &str,
    has_schema: bool,
    schema_tokens: u64,
    max_tokens: Option<u32>,
    meter: &ContextMeter,
    window_evidence: Option<ContextWindowEvidence>,
) -> RequestMeasurement {
    if let Some(window) = cfg.context_window {
        // A valid real-usage anchor is stronger than a cold chars/token pass.
        // Add schemas only if that exact anchored request did not already carry
        // them. Any mutation of covered messages invalidates the anchor first.
        let used = meter.estimate(messages)
            + if meter.anchor_includes_schema(messages, schema_json) {
                0
            } else {
                schema_tokens
            };
        // P6: budget against the clamped effective window (if repeated shortfall
        // evidence earned one), not necessarily the raw resolved window. `window`
        // itself — reported below as the resolved model window — is untouched.
        let budget =
            ContextBudget::new(effective_budget_window(window, window_evidence), max_tokens);
        RequestMeasurement {
            used,
            target: budget.target,
            window: Some(window),
            effective: Some(budget.effective),
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
            effective: None,
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
            effective: measurement.effective,
        });
    }
    Ok(measurement)
}

/// The last-line guard used at every provider boundary in this module.
///
/// P11: the schema token estimate is cached per session, keyed by the P7
/// [`schema_fingerprint`] of the serialized schema. A cache hit reuses the
/// stored `schema_json` and estimate without re-running `estimate_str`; a miss
/// (first preflight or a changed tool registry) recomputes and overwrites the
/// cache. Serialization of `req.tools` still happens here — it is required to
/// fingerprint the CURRENT request's schema — but the chars/token pass over it
/// does not.
fn ensure_chat_request_fits(
    req: &ChatRequest,
    cfg: &AgentConfig,
    meter: &ContextMeter,
    window_evidence: Option<ContextWindowEvidence>,
    schema_cache: &mut Option<(u64, String, u64)>,
) -> Result<RequestMeasurement, AgentError> {
    let has_schema =
        req.tools.as_ref().is_some_and(|tools| !tools.is_empty()) || req.response_format.is_some();
    let schema_json =
        serialized_request_constraints(req.tools.as_deref(), req.response_format.as_ref())?;
    let schema_tokens = if has_schema {
        cached_schema_estimate(schema_cache, &schema_json)
    } else {
        0
    };
    check_measurement(request_measurement(
        &req.messages,
        cfg,
        &schema_json,
        has_schema,
        schema_tokens,
        req.max_tokens,
        meter,
        window_evidence,
    ))
}

fn serialized_request_constraints(
    tools: Option<&[ToolDescriptor]>,
    response_format: Option<&JsonSchemaFormat>,
) -> Result<String, AgentError> {
    let mut serialized = String::new();
    if let Some(tools) = tools.filter(|tools| !tools.is_empty()) {
        serialized.push_str(&serde_json::to_string(tools).map_err(|error| {
            AgentError::Loop(format!("failed to serialise tools schema: {error}"))
        })?);
    }
    if let Some(response_format) = response_format {
        if !serialized.is_empty() {
            serialized.push('\n');
        }
        // Budget the exact OpenAI-compatible wrapper, not merely the internal
        // value: the provider adds both `type` and `json_schema` at the wire
        // boundary and those bytes count against the same context window.
        serialized.push_str(
            &serde_json::to_string(&serde_json::json!({
                "type": "json_schema",
                "json_schema": {
                    "name": &response_format.name,
                    "strict": response_format.strict,
                    "schema": &response_format.schema,
                }
            }))
            .map_err(|error| {
                AgentError::Loop(format!("failed to serialise response schema: {error}"))
            })?,
        );
    }
    Ok(serialized)
}

/// P11 helper: return the cached token estimate for `schema_json` when the
/// session cache's fingerprint matches, otherwise compute `estimate_str` fresh
/// and overwrite the cache with the new (fingerprint, schema_json, estimate)
/// triple. Reuses the P7 [`schema_fingerprint`] — no new hashing scheme.
fn cached_schema_estimate(cache: &mut Option<(u64, String, u64)>, schema_json: &str) -> u64 {
    let fingerprint = schema_fingerprint(schema_json);
    if let Some((cached_fp, _, cached_estimate)) = cache.as_ref() {
        if *cached_fp == fingerprint {
            return *cached_estimate;
        }
    }
    let estimate = estimate_str(schema_json);
    *cache = Some((fingerprint, schema_json.to_string(), estimate));
    estimate
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
    let has_schema = !tools.descriptors().is_empty() || cfg.response_format.is_some();
    let schema_json = serialized_request_constraints(
        (!tools.descriptors().is_empty()).then_some(tools.descriptors()),
        cfg.response_format.as_ref(),
    )?;
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
        1,
        CompactionReason::ManualRequest,
        true,
        state.window_evidence,
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
        1,
        CompactionReason::ContextTargetExceeded,
        false,
        None,
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
    round: u32,
    reason: CompactionReason,
    force: bool,
    window_evidence: Option<ContextWindowEvidence>,
) -> Result<CompactionReport, AgentError>
where
    P: Provider,
{
    // P11: the schema is fixed for this whole call, so its token estimate is
    // computed once here and threaded into every request_measurement /
    // structural_request_tokens call below instead of being re-derived from
    // schema_json on each pass.
    let schema_tokens = if has_schema {
        estimate_str(schema_json)
    } else {
        0
    };
    let token_before = structural_request_tokens(messages, schema_tokens);
    let messages_before = messages.len();
    let initial = request_measurement(
        messages,
        cfg,
        schema_json,
        has_schema,
        schema_tokens,
        cfg.max_tokens,
        meter,
        window_evidence,
    );
    let mut progress = CompactionProgress::default();

    if cfg.context_window.is_none() {
        // With no model window, preserve the historical coarse byte guard. It is
        // still a checked invariant: elide, then drop whole exchanges, then fail
        // locally if protected content alone exceeds the configured cap.
        if initial.used > initial.target {
            let (after_elide, elided, dropped) =
                trim_tool_results_counted(messages, initial.used, initial.target, |m| {
                    m.byte_size() as u64
                });
            if elided > 0 {
                meter.invalidate();
                progress.tool_results_elided = elided;
                progress.outcome = Some(CompactionOutcome::ToolResultsElided);
            }
            report_dropped_tool_results(cfg, round, dropped);
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
            schema_tokens,
            cfg.max_tokens,
            meter,
            window_evidence,
        );
        let record = record_progress(
            ledger,
            cfg,
            reason,
            &progress,
            token_before,
            structural_request_tokens(messages, schema_tokens),
            messages_before,
            messages.len(),
        );
        check_measurement(final_measurement)?;
        return Ok(CompactionReport {
            changed: record.is_some(),
            target_tokens: 0,
            tokens_before: token_before,
            tokens_after: structural_request_tokens(messages, schema_tokens),
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
        let (after_elide, elided, dropped) =
            trim_tool_results_counted(messages, running, target, message_tokens);
        running = after_elide;
        report_dropped_tool_results(cfg, round, dropped);
        if elided > 0 {
            meter.invalidate();
            progress.tool_results_elided = elided;
            progress.outcome = Some(CompactionOutcome::ToolResultsElided);
            // Once covered content changes, use one consistent cold estimate for
            // all subsequent stages and the final adjacent preflight.
            running = structural_request_tokens(messages, schema_tokens);
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
                    current = structural_request_tokens(messages, schema_tokens);
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
                    schema_tokens,
                    target,
                    compact_failures,
                    turn_usage,
                    meter,
                    &mut progress,
                    round,
                    window_evidence,
                )
                .await?;
            }
        }
    }

    let token_after = structural_request_tokens(messages, schema_tokens);
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
        schema_tokens,
        cfg.max_tokens,
        meter,
        window_evidence,
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
    window_evidence: Option<ContextWindowEvidence>,
) -> Option<(ChatRequest, bool)> {
    let window = cfg.context_window?;
    let max_tokens = summary_max_tokens(window, cfg.max_tokens)?;
    // P6: the compaction summarizer's own outbound request must fit inside the
    // same clamped session budget as everything else — a resolved-window-sized
    // summary target would re-risk the exact truncation this clamp exists to
    // prevent.
    let target = ContextBudget::new(
        effective_budget_window(window, window_evidence),
        Some(max_tokens),
    )
    .target;
    // Budget the request in TOKEN space, not a flat bytes/CHARS_PER_TOKEN
    // guess, so the request this function hands back is actually estimated at
    // or under `target` once built (message_tokens is what `ensure_chat_-
    // request_fits` measures it with just after). Reserve PER_MESSAGE_TOKENS
    // for the single wrapping message's own overhead, and the fixed prefix/
    // suffix/prompt text's REAL estimate (not its byte length — the prompt
    // text contains non-ASCII punctuation that costs a full token each under
    // the two-rate estimator, not a byte-len/4 fraction of one). Only the
    // remaining token headroom is converted back to a byte budget, and only
    // for the variable transcript, which is ASCII-dominated conversation
    // text in the common case.
    let fixed_tokens = estimate_str(SUMMARY_INPUT_PREFIX)
        + estimate_str(SUMMARY_INPUT_SUFFIX)
        + estimate_str(SUMMARY_PROMPT);
    let transcript_token_budget = target
        .saturating_sub(PER_MESSAGE_TOKENS)
        .saturating_sub(fixed_tokens);
    if transcript_token_budget == 0 {
        return None;
    }
    let available = usize::try_from(transcript_token_budget.saturating_mul(CHARS_PER_TOKEN as u64))
        .unwrap_or(usize::MAX);
    let transcript = summary_transcript(old_messages);
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
            reasoning_effort: cfg.reasoning_effort,
            max_tokens: Some(max_tokens),
            response_format: None,
            prompt_cache_key: None,
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
    schema_tokens: u64,
    target: u64,
) -> Option<(Vec<ChatMessage>, bool)> {
    let mut cap = summary.len();
    loop {
        let bounded = truncate_middle_to_bytes(summary, cap);
        if bounded.trim().is_empty() {
            return None;
        }
        let rebuilt = rebuilt_with_summary(messages, span, bounded.trim());
        let used = structural_request_tokens(&rebuilt, schema_tokens);
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
    schema_tokens: u64,
    target: u64,
    failures: &mut u32,
    turn_usage: &mut kitsu_providers::Usage,
    meter: &mut ContextMeter,
    progress: &mut CompactionProgress,
    round: u32,
    window_evidence: Option<ContextWindowEvidence>,
) -> Result<(), AgentError>
where
    P: Provider,
{
    let Some(span) = compaction_span(messages) else {
        return Ok(());
    };

    let fallback = if *failures >= MAX_COMPACT_FAILURES {
        Some(SummaryFallbackReason::CircuitBreaker)
    } else {
        match bounded_summary_request(
            cfg,
            &messages[span.prefix_end..span.recent_start],
            window_evidence,
        ) {
            None => Some(SummaryFallbackReason::RequestTooLarge),
            Some((req, shortened)) => {
                progress.summary_input_shortened = shortened;
                // This is a provider request in its own right. Check the concrete
                // bounded request immediately before crossing the boundary.
                let cold_meter = ContextMeter::new();
                // The bounded summary request carries no tools, so the P11
                // schema cache is irrelevant here — pass a fresh empty slot.
                if ensure_chat_request_fits(&req, cfg, &cold_meter, window_evidence, &mut None)
                    .is_err()
                {
                    Some(SummaryFallbackReason::RequestTooLarge)
                } else {
                    if let Some(log) = &cfg.event_log {
                        log.record(AgentRuntimeEvent::ProviderRequest {
                            round,
                            purpose: ProviderRequestPurpose::Compaction,
                            model: Some(cfg.model.clone()),
                            message_count: req.messages.len(),
                            tools_offered: 0,
                            structured_output: false,
                            request_timeout_ms: cfg.request_timeout_ms,
                            turn_remaining_ms: remaining_turn_millis(cfg.turn_deadline),
                        });
                    }
                    abort_if_event_sink_failed(cfg)?;
                    let usage_tracker = cfg.run_tallies.as_ref().map(|tallies| {
                        ProviderResponseUsageTracker::new(
                            std::sync::Arc::clone(tallies),
                            ProviderRequestPurpose::Compaction,
                        )
                    });
                    let collection = collect_text(
                        provider,
                        req,
                        provider_telemetry_sink(
                            cfg.event_log.as_ref(),
                            round,
                            ProviderRequestPurpose::Compaction,
                            cfg.turn_deadline,
                            usage_tracker.clone(),
                        ),
                        usage_tracker,
                    )
                    .await;
                    abort_if_event_sink_failed(cfg)?;
                    match collection {
                        SummaryTextCollection::Complete { text, usage }
                            if !text.trim().is_empty() =>
                        {
                            if let Some(usage) = usage {
                                record_compaction_usage(cfg, round, turn_usage, usage)?;
                            }
                            if let Some((rebuilt, output_shortened)) = fit_summary_rebuild(
                                messages,
                                span,
                                text.trim(),
                                schema_tokens,
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
                                return Ok(());
                            }
                            *failures = failures.saturating_add(1);
                            Some(SummaryFallbackReason::ResponseTooLarge)
                        }
                        SummaryTextCollection::Complete { usage, .. } => {
                            if let Some(usage) = usage {
                                record_compaction_usage(cfg, round, turn_usage, usage)?;
                            }
                            *failures = failures.saturating_add(1);
                            Some(SummaryFallbackReason::EmptyCompletion)
                        }
                        SummaryTextCollection::Failed { reason, usage } => {
                            if let Some(usage) = usage {
                                record_compaction_usage(cfg, round, turn_usage, usage)?;
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
    let current = structural_request_tokens(messages, schema_tokens);
    let removed = drop_old_exchanges_detailed(messages, current, target, message_tokens, true);
    if removed > 0 {
        meter.invalidate();
        progress.exchanges_removed = removed;
        progress.outcome = Some(CompactionOutcome::SummaryFallbackTrim);
        progress.fallback_reason = fallback;
    } else {
        // The fallback ran but there was nothing left to trim. Record a
        // distinct outcome so a caller never mistakes a stale prior-round
        // outcome for this round's result.
        progress.exchanges_removed = 0;
        progress.outcome = Some(CompactionOutcome::SummaryFallbackNoOp);
        progress.fallback_reason = fallback;
    }
    Ok(())
}

fn record_compaction_usage(
    cfg: &AgentConfig,
    round: u32,
    turn_usage: &mut kitsu_providers::Usage,
    usage: kitsu_providers::Usage,
) -> Result<(), AgentError> {
    turn_usage.add(usage);
    if let Some(log) = &cfg.event_log {
        log.record(AgentRuntimeEvent::Usage {
            round,
            purpose: ProviderRequestPurpose::Compaction,
            prompt_tokens: usage.prompt_tokens,
            completion_tokens: usage.completion_tokens,
            total_tokens: usage.total_tokens,
        });
    }
    abort_if_event_sink_failed(cfg)
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
async fn collect_text<P: Provider>(
    provider: &P,
    req: ChatRequest,
    telemetry: Option<ProviderTelemetrySink>,
    usage_tracker: Option<ProviderResponseUsageTracker>,
) -> SummaryTextCollection {
    let mut stream = match provider.chat_stream_with_telemetry(req, telemetry).await {
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
            Ok(ChatDelta::Usage(u)) => {
                if let Some(tracker) = &usage_tracker {
                    tracker.record_reported(u);
                }
                usage = Some(u);
            }
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
    if let Some(tracker) = &usage_tracker {
        tracker.record_completed();
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

/// Report an unrecoverable tool-result loss on the runtime event stream, and
/// only that: `dropped == 0` emits nothing, so every run that loses nothing
/// (including a lossless offload-pointer eviction) keeps its stream exactly as
/// it is today. Emitted at the elision site because that is the only place the
/// lossy/lossless split is known.
fn report_dropped_tool_results(cfg: &AgentConfig, round: u32, dropped: u32) {
    if dropped == 0 {
        return;
    }
    if let Some(log) = &cfg.event_log {
        log.record(AgentRuntimeEvent::ToolResultsDropped { round, dropped });
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

/// Returns `(total, elided, dropped)`. `elided` counts every eviction;
/// `dropped` counts only the unrecoverable subset — an eviction whose stub is
/// not an [`OFFLOAD_POINTER_PREFIX`] pointer, so the body is gone with no
/// recall path. The two differ exactly when a low `--offload-cap` has already
/// offloaded the result: `elided_tool_stub` then keeps the pointer line and
/// drops only the preview, which is lossless and must not be reported as loss.
fn trim_tool_results_counted(
    messages: &mut [ChatMessage],
    mut total: u64,
    limit: u64,
    size: impl Fn(&ChatMessage) -> u64,
) -> (u64, u32, u32) {
    if total <= limit {
        return (total, 0, 0);
    }
    let last_user_idx = messages.iter().rposition(|m| m.role == Role::User);
    let mut elided = 0u32;
    let mut dropped = 0u32;
    let mut i = 0;
    while total > limit && i < messages.len() {
        // Only elide tool results (never system / last-user / assistant turns).
        if messages[i].role == Role::Tool && Some(i) != last_user_idx {
            let before = size(&messages[i]);
            let stub = elided_tool_stub(&messages[i]);
            let after = size(&stub);
            if after < before {
                let recallable = stub.content.starts_with(OFFLOAD_POINTER_PREFIX);
                total = total.saturating_sub(before - after);
                messages[i] = stub;
                elided = elided.saturating_add(1);
                if !recallable {
                    dropped = dropped.saturating_add(1);
                }
            }
        }
        i += 1;
    }
    (total, elided, dropped)
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

/// Run-wide tallies shared across the MAIN conversation and every spawned
/// subagent (one Arc, like the spawn budget), so end-of-run receipts count
/// delegated work too: successful reads, denied tool calls, failed tool
/// calls (denied is a subset of failed).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ProviderUsageCoverageSnapshot {
    pub responses_reporting_usage: u64,
    pub responses_missing_usage: u64,
    /// `None` means no response itemized cached prompt tokens. `Some(0)` is
    /// retained when the provider explicitly supplied a zero cache count.
    pub cached_prompt_tokens: Option<u64>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RunProviderUsageCoverageSnapshot {
    pub all: ProviderUsageCoverageSnapshot,
    pub inference: ProviderUsageCoverageSnapshot,
    pub compaction: ProviderUsageCoverageSnapshot,
    pub model_catalog: ProviderUsageCoverageSnapshot,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RunUsageSnapshot {
    pub turns: u64,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
    pub coverage: RunProviderUsageCoverageSnapshot,
}

#[derive(Debug, Clone, Copy, Default)]
struct ProviderUsageCoverageTallies {
    responses_reporting_usage: u64,
    responses_missing_usage: u64,
    cached_prompt_tokens: Option<u64>,
}

impl ProviderUsageCoverageTallies {
    fn record(&mut self, usage: Option<kitsu_providers::Usage>) {
        match usage {
            Some(usage) => {
                self.responses_reporting_usage = self.responses_reporting_usage.saturating_add(1);
                if let Some(cached) = usage
                    .prompt_tokens_details
                    .and_then(|details| details.cached_tokens)
                {
                    self.cached_prompt_tokens = Some(
                        self.cached_prompt_tokens
                            .unwrap_or(0)
                            .saturating_add(cached),
                    );
                }
            }
            None => {
                self.responses_missing_usage = self.responses_missing_usage.saturating_add(1);
            }
        }
    }

    fn snapshot(self) -> ProviderUsageCoverageSnapshot {
        ProviderUsageCoverageSnapshot {
            responses_reporting_usage: self.responses_reporting_usage,
            responses_missing_usage: self.responses_missing_usage,
            cached_prompt_tokens: self.cached_prompt_tokens,
        }
    }
}

#[derive(Debug, Default)]
struct RunProviderUsageCoverageTallies {
    all: ProviderUsageCoverageTallies,
    inference: ProviderUsageCoverageTallies,
    compaction: ProviderUsageCoverageTallies,
    model_catalog: ProviderUsageCoverageTallies,
}

impl RunProviderUsageCoverageTallies {
    fn record(&mut self, purpose: ProviderRequestPurpose, usage: Option<kitsu_providers::Usage>) {
        self.all.record(usage);
        match purpose {
            ProviderRequestPurpose::Inference => self.inference.record(usage),
            ProviderRequestPurpose::Compaction => self.compaction.record(usage),
            ProviderRequestPurpose::ModelCatalog => self.model_catalog.record(usage),
        }
    }

    fn snapshot(&self) -> RunProviderUsageCoverageSnapshot {
        RunProviderUsageCoverageSnapshot {
            all: self.all.snapshot(),
            inference: self.inference.snapshot(),
            compaction: self.compaction.snapshot(),
            model_catalog: self.model_catalog.snapshot(),
        }
    }
}

#[derive(Debug, Default)]
pub struct RunTallies {
    pub reads: std::sync::atomic::AtomicU64,
    pub denied: std::sync::atomic::AtomicU64,
    pub failed: std::sync::atomic::AtomicU64,
    /// Billed usage across the run — accumulated at turn end regardless of
    /// later rollback (cycle-23: a rolled-back turn erased ~9.8k billed
    /// tokens from /cost), and shared with subagent configs so delegated
    /// spend counts too.
    pub prompt_tokens: std::sync::atomic::AtomicU64,
    pub completion_tokens: std::sync::atomic::AtomicU64,
    pub total_tokens: std::sync::atomic::AtomicU64,
    pub turns: std::sync::atomic::AtomicU64,
    /// Per-model spend bins: `/model` mid-session must not blur attribution
    /// (cycle-25 audit). Key = model id; value = (prompt, completion, total).
    pub per_model: std::sync::Mutex<std::collections::BTreeMap<String, (u64, u64, u64)>>,
    provider_usage_coverage: std::sync::Mutex<RunProviderUsageCoverageTallies>,
}

impl RunTallies {
    fn record_provider_response(
        &self,
        purpose: ProviderRequestPurpose,
        usage: Option<kitsu_providers::Usage>,
    ) {
        self.provider_usage_coverage
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .record(purpose, usage);
    }

    /// Capture the neutral current-process usage inputs. Coverage is retained
    /// independently from token totals so omitted provider usage never becomes
    /// a reported zero, while explicit cached-token zero remains present.
    pub fn usage_snapshot(&self) -> RunUsageSnapshot {
        let coverage = self
            .provider_usage_coverage
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .snapshot();
        RunUsageSnapshot {
            turns: self.turns.load(Ordering::Acquire),
            prompt_tokens: self.prompt_tokens.load(Ordering::Acquire),
            completion_tokens: self.completion_tokens.load(Ordering::Acquire),
            total_tokens: self.total_tokens.load(Ordering::Acquire),
            coverage,
        }
    }
}

#[derive(Debug, Default)]
struct ProviderResponseUsageTrackerState {
    counted_response_attempts: std::collections::BTreeSet<u32>,
    final_response_attempt: Option<u32>,
    final_response_recorded: bool,
}

/// Per-logical-request accounting guard. Physical attempt numbers deduplicate
/// adapter dispositions without introducing a durable request identity.
#[derive(Clone, Debug)]
struct ProviderResponseUsageTracker {
    tallies: std::sync::Arc<RunTallies>,
    purpose: ProviderRequestPurpose,
    state: std::sync::Arc<std::sync::Mutex<ProviderResponseUsageTrackerState>>,
}

impl ProviderResponseUsageTracker {
    fn new(tallies: std::sync::Arc<RunTallies>, purpose: ProviderRequestPurpose) -> Self {
        Self {
            tallies,
            purpose,
            state: std::sync::Arc::new(std::sync::Mutex::new(
                ProviderResponseUsageTrackerState::default(),
            )),
        }
    }

    fn observe_attempt(&self, event: &kitsu_providers::ProviderAttemptEvent) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match event.outcome {
            ProviderAttemptOutcome::RetryScheduled if !event.terminal => {
                state.final_response_attempt = Some(event.attempt.saturating_add(1));
            }
            ProviderAttemptOutcome::ResponseAccepted
            | ProviderAttemptOutcome::BodyCompleted
            | ProviderAttemptOutcome::BodyFailed
                if event.status.is_some() =>
            {
                state.final_response_attempt = Some(event.attempt);
            }
            _ => {}
        }

        // An HTTP rejection accepted by the adapter is a completed provider
        // response even when its body is intentionally discarded before a
        // retry. A transport-only failure has no response and is not coverage.
        let completed_without_usage = event.status.is_some()
            && matches!(
                event.outcome,
                ProviderAttemptOutcome::RetryScheduled
                    | ProviderAttemptOutcome::Rejected
                    | ProviderAttemptOutcome::DeadlineExceeded
            );
        if !completed_without_usage {
            return;
        }
        if state.counted_response_attempts.insert(event.attempt) {
            self.tallies.record_provider_response(self.purpose, None);
        }
    }

    fn record_reported(&self, usage: kitsu_providers::Usage) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !state.final_response_recorded {
            let new_response = state.final_response_attempt.map_or(true, |attempt| {
                state.counted_response_attempts.insert(attempt)
            });
            if new_response {
                self.tallies
                    .record_provider_response(self.purpose, Some(usage));
            }
            state.final_response_recorded = true;
        }
    }

    fn record_completed(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !state.final_response_recorded {
            let new_response = state.final_response_attempt.map_or(true, |attempt| {
                state.counted_response_attempts.insert(attempt)
            });
            if new_response {
                self.tallies.record_provider_response(self.purpose, None);
            }
            state.final_response_recorded = true;
        }
    }
}

/// True when swapping this inline result for a preview is actually needed:
/// keeping it inline would push the estimated prompt past half the effective
/// budget. Below that, headroom is comfortable — and replacing full evidence
/// with a 500-char preview only starves the model (cycle-15 headline: a
/// 131k-window turn lost a 1828-line read at 1.5k tokens used, then answered
/// honestly from the preview and was blamed for fabricating).
fn offload_pressure(
    messages: &[ChatMessage],
    content: &str,
    cfg: &AgentConfig,
    window_evidence: Option<ContextWindowEvidence>,
) -> bool {
    let Some(window) = cfg.context_window else {
        return true; // unknown window: keep the old conservative behavior
    };
    // P6: offload against the clamped effective budget once shortfall evidence
    // has earned one, so a session pinned to a too-large resolved window still
    // starts offloading at the REAL usable threshold instead of the stale one.
    let effective = ContextBudget::new(
        effective_budget_window(window, window_evidence),
        cfg.max_tokens,
    )
    .effective;
    let used: u64 = messages.iter().map(message_tokens).sum();
    let added = estimate_str(content);
    used.saturating_add(added).saturating_mul(2) > effective
}

fn prepare_tool_output(
    content: String,
    request: ToolOutputRequest<'_>,
    tools: &impl ToolCatalog,
    messages: &[ChatMessage],
    cfg: &AgentConfig,
) -> PreparedToolOutput {
    // Native command results are already bounded per stream and must remain
    // valid JSON so the event layer can parse their authoritative counters.
    if matches!(request.tool_name, "command_exec" | "git") {
        let retention = ToolOutputRetention::complete(&content);
        return PreparedToolOutput {
            content,
            retention,
            artifact_path: None,
        };
    }

    if cfg.artifact_policy == ArtifactStoragePolicy::InlineOnly {
        let bounded = bound_inline_output(content);
        let retention = ToolOutputRetention {
            output_bytes: bounded.original_bytes,
            output_retained_bytes: bounded.retained_bytes,
            output_truncated: bounded.truncated,
            artifact_status: if bounded.truncated {
                ToolArtifactStatus::SuppressedByPolicy
            } else {
                ToolArtifactStatus::NotNeeded
            },
        };
        return PreparedToolOutput {
            content: bounded.content,
            retention,
            artifact_path: None,
        };
    }

    // Preserve the existing default: errors remain inline, while successful
    // oversized results may be offloaded if the model can recover the pointer.
    if request.is_err {
        let retention = ToolOutputRetention::complete(&content);
        return PreparedToolOutput {
            content,
            retention,
            artifact_path: None,
        };
    }
    maybe_offload_for_tools_detailed(
        content,
        request.call_id,
        cfg.offload_dir.as_deref(),
        tools,
        messages,
        cfg,
        request.window_evidence,
    )
}

/// Offload only when the active model can actually recover the resulting
/// pointer. A registered-but-unadvertised handler is insufficient: the model
/// must receive an `fs_read_file` descriptor in this turn's tool catalog.
/// Also requires genuine context pressure (see [`offload_pressure`]).
#[cfg(test)]
fn maybe_offload_for_tools(
    content: String,
    call_id: &str,
    dir: Option<&std::path::Path>,
    tools: &Registry,
    messages: &[ChatMessage],
    cfg: &AgentConfig,
    window_evidence: Option<ContextWindowEvidence>,
) -> (String, Option<std::path::PathBuf>) {
    let prepared = maybe_offload_for_tools_detailed(
        content,
        call_id,
        dir,
        tools,
        messages,
        cfg,
        window_evidence,
    );
    (prepared.content, prepared.artifact_path)
}

fn maybe_offload_for_tools_detailed(
    content: String,
    call_id: &str,
    dir: Option<&std::path::Path>,
    tools: &impl ToolCatalog,
    messages: &[ChatMessage],
    cfg: &AgentConfig,
    window_evidence: Option<ContextWindowEvidence>,
) -> PreparedToolOutput {
    let can_read = tools.is_registered("fs_read_file")
        && tools
            .descriptors()
            .iter()
            .any(|descriptor| descriptor.name == "fs_read_file");
    if !can_read || !offload_pressure(messages, &content, cfg, window_evidence) {
        let retention = ToolOutputRetention::complete(&content);
        return PreparedToolOutput {
            content,
            retention,
            artifact_path: None,
        };
    }
    let cap = cfg.offload_char_cap.unwrap_or(OFFLOAD_CHAR_CAP);
    maybe_offload_detailed(content, call_id, dir, cap)
}

/// If `content` exceeds the cap and an offload `dir` is set, allocate a unique
/// private `<dir>/<call_id>-<sequence>.txt` artifact and return a pointer +
/// preview the model can re-read via `fs_read_file`. Returns the content
/// unchanged when there is no dir, it's under the cap, or the write fails (the
/// trimming cascade still bounds it). The offload dir must be model-readable.
#[cfg(test)]
fn maybe_offload(
    content: String,
    call_id: &str,
    dir: Option<&std::path::Path>,
) -> (String, Option<std::path::PathBuf>) {
    let prepared = maybe_offload_detailed(content, call_id, dir, OFFLOAD_CHAR_CAP);
    (prepared.content, prepared.artifact_path)
}

/// Like [`maybe_offload`] but with an explicit size cap, so tests can exercise
/// the configurable "recall mode" threshold.
#[cfg(test)]
fn maybe_offload_with_cap(
    content: String,
    call_id: &str,
    dir: Option<&std::path::Path>,
    cap: usize,
) -> (String, Option<std::path::PathBuf>) {
    let prepared = maybe_offload_detailed(content, call_id, dir, cap);
    (prepared.content, prepared.artifact_path)
}

fn maybe_offload_detailed(
    content: String,
    call_id: &str,
    dir: Option<&std::path::Path>,
    cap: usize,
) -> PreparedToolOutput {
    let Some(dir) = dir else {
        let retention = ToolOutputRetention::complete(&content);
        return PreparedToolOutput {
            content,
            retention,
            artifact_path: None,
        };
    };
    if content.len() <= cap {
        let retention = ToolOutputRetention::complete(&content);
        return PreparedToolOutput {
            content,
            retention,
            artifact_path: None,
        };
    }
    let original_bytes = u64::try_from(content.len()).unwrap_or(u64::MAX);
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
        Err(_) => {
            let mut retention = ToolOutputRetention::complete(&content);
            retention.artifact_status = ToolArtifactStatus::Failed;
            return PreparedToolOutput {
                content,
                retention,
                artifact_path: None,
            };
        }
    };
    let preview: String = content.chars().take(OFFLOAD_PREVIEW_CHARS).collect();
    let preview_bytes = u64::try_from(preview.len()).unwrap_or(u64::MAX);
    let rendered = format!(
        "{OFFLOAD_POINTER_PREFIX}{:.1} KB — full result saved to {}. Re-read it (or a line range) \
         with fs_read_file on that path.]\n\nPreview:\n{preview}…",
        content.len() as f64 / 1024.0,
        path.display(),
    );
    PreparedToolOutput {
        content: rendered,
        retention: ToolOutputRetention {
            output_bytes: original_bytes,
            output_retained_bytes: preview_bytes,
            output_truncated: true,
            artifact_status: ToolArtifactStatus::Retained,
        },
        artifact_path: Some(path),
    }
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
