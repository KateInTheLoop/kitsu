//! Bounded, redacted state for one detached-agent thread.
//!
//! The durable view intentionally contains no tool arguments, tool results,
//! provider bodies, call identifiers, or the registry's unbounded terminal
//! result field. It can retain bounded/redacted assistant text and is suitable
//! for UI inspection and local persistence after
//! [`AgentThreadSnapshotDto::validate`] succeeds.

use std::collections::VecDeque;
use std::fmt;
use std::time::Instant;

use kitsu_core::{AgentRuntimeEvent, ProviderRequestPurpose};
use kitsu_providers::{ProviderAttemptCategory, ProviderAttemptOutcome};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

/// Version 2 added the non-terminal `blocked` phase to a closed serde enum,
/// so a v1 reader cannot decode a v2 DTO.
pub const AGENT_THREAD_SCHEMA_VERSION: u32 = 2;
pub const DEFAULT_AGENT_TIMELINE_EVENTS: usize = 128;
pub const DEFAULT_AGENT_TIMELINE_BYTES: usize = 64 * 1024;
pub const DEFAULT_AGENT_TIMELINE_ENTRY_BYTES: usize = 4 * 1024;
pub const DEFAULT_AGENT_STEERING_CAPACITY: usize = 8;
pub const DEFAULT_AGENT_STEERING_BYTES: usize = 8 * 1024;

const MAX_AGENT_TIMELINE_EVENTS: usize = 2_048;
const MAX_AGENT_TIMELINE_BYTES: usize = 2 * 1024 * 1024;
const MAX_AGENT_TIMELINE_ENTRY_BYTES: usize = 64 * 1024;
const MAX_AGENT_STEERING_CAPACITY: usize = 64;
const MAX_AGENT_STEERING_BYTES: usize = 64 * 1024;
const MIN_AGENT_TIMELINE_BYTES: usize = 64;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AgentThreadLimits {
    pub max_events: usize,
    pub max_bytes: usize,
    pub max_entry_bytes: usize,
    pub steering_capacity: usize,
    pub max_steering_bytes: usize,
}

impl Default for AgentThreadLimits {
    fn default() -> Self {
        Self {
            max_events: DEFAULT_AGENT_TIMELINE_EVENTS,
            max_bytes: DEFAULT_AGENT_TIMELINE_BYTES,
            max_entry_bytes: DEFAULT_AGENT_TIMELINE_ENTRY_BYTES,
            steering_capacity: DEFAULT_AGENT_STEERING_CAPACITY,
            max_steering_bytes: DEFAULT_AGENT_STEERING_BYTES,
        }
    }
}

impl AgentThreadLimits {
    pub fn validate(self) -> Result<Self, AgentThreadLimitError> {
        if !(1..=MAX_AGENT_TIMELINE_EVENTS).contains(&self.max_events) {
            return Err(AgentThreadLimitError(
                "max_events",
                MAX_AGENT_TIMELINE_EVENTS,
            ));
        }
        if !(MIN_AGENT_TIMELINE_BYTES..=MAX_AGENT_TIMELINE_BYTES).contains(&self.max_bytes) {
            return Err(AgentThreadLimitError("max_bytes", MAX_AGENT_TIMELINE_BYTES));
        }
        if !(1..=MAX_AGENT_TIMELINE_ENTRY_BYTES).contains(&self.max_entry_bytes)
            || self.max_entry_bytes > self.max_bytes
        {
            return Err(AgentThreadLimitError(
                "max_entry_bytes",
                MAX_AGENT_TIMELINE_ENTRY_BYTES,
            ));
        }
        if !(1..=MAX_AGENT_STEERING_CAPACITY).contains(&self.steering_capacity) {
            return Err(AgentThreadLimitError(
                "steering_capacity",
                MAX_AGENT_STEERING_CAPACITY,
            ));
        }
        if !(1..=MAX_AGENT_STEERING_BYTES).contains(&self.max_steering_bytes) {
            return Err(AgentThreadLimitError(
                "max_steering_bytes",
                MAX_AGENT_STEERING_BYTES,
            ));
        }
        Ok(self)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AgentThreadLimitError(&'static str, usize);

impl fmt::Display for AgentThreadLimitError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "invalid agent thread {} (hard maximum {})",
            self.0, self.1
        )
    }
}

impl std::error::Error for AgentThreadLimitError {}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentTimelineKind {
    UserTask,
    Assistant,
    Provider,
    Tool,
    Usage,
    Steering,
    Lifecycle,
    /// One permission request parked on this agent, and its decision (T2-S6).
    Approval,
}

impl AgentTimelineKind {
    /// The DTO spelling. Human rendering uses this too, so a `/jobs show`
    /// timeline can never disagree with the JSONL beside it.
    pub fn label(self) -> &'static str {
        match self {
            Self::UserTask => "user_task",
            Self::Assistant => "assistant",
            Self::Provider => "provider",
            Self::Tool => "tool",
            Self::Usage => "usage",
            Self::Steering => "steering",
            Self::Lifecycle => "lifecycle",
            Self::Approval => "approval",
        }
    }
}

/// What a parked permission request is asking for, *without its arguments*.
///
/// Deliberately argument-free. `docs/functionality.md` states as an absolute
/// that tool arguments and output are never copied into the inspection
/// record, and a permission request is precisely a tool plus its arguments,
/// so only the kind crosses into the timeline. `/jobs explain`'s question
/// ("what is this agent waiting on") is answered by the kind alone.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentApprovalKind {
    ReadFile,
    WriteFile,
    RunCommand,
}

impl AgentApprovalKind {
    /// The machine spelling, shared by every surface outside the process: the
    /// state file's `waiting_for`, the `on_agent_state` hook's
    /// `KITSU_AGENT_WAITING_FOR`, and `kitsu agents --json`. It is the serde
    /// spelling too, so one vocabulary reaches an outside reader.
    pub fn wire(self) -> &'static str {
        match self {
            Self::ReadFile => "read_file",
            Self::WriteFile => "write_file",
            Self::RunCommand => "run_command",
        }
    }

    /// The phrase that follows "permission to".
    pub fn label(self) -> &'static str {
        match self {
            Self::ReadFile => "read a file",
            Self::WriteFile => "write a file",
            Self::RunCommand => "run a command",
        }
    }
}

/// How a parked permission request was decided.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AgentApprovalOutcome {
    AllowedOnce,
    AllowedAlways,
    Denied,
    /// No human answered within `agents.block_timeout_secs` (B3). This is the
    /// one outcome with nobody present, so the record matters most here.
    TimedOut,
    /// The park was cancelled rather than decided: `cancel_matching` took
    /// the parked prompt itself and resolved the gate with `Interrupt`, so
    /// the outcome is recorded at that site (F43).
    Cancelled,
}

impl AgentApprovalOutcome {
    pub fn label(self) -> &'static str {
        match self {
            Self::AllowedOnce => "allowed once",
            Self::AllowedAlways => "allowed always",
            Self::Denied => "denied",
            Self::TimedOut => "timed out",
            Self::Cancelled => "cancelled",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AgentTimelineEntry {
    pub sequence: u64,
    pub elapsed_ms: u64,
    pub kind: AgentTimelineKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub round: Option<u32>,
    pub text: String,
    pub truncated: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AgentTimelineSnapshot {
    pub entries: Vec<AgentTimelineEntry>,
    pub dropped_entries: u64,
    pub retained_bytes: usize,
    pub closed: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentThreadPhase {
    Running,
    /// Parked on a user answer. Non-terminal: the thread's timeline stays
    /// open, it offers no handoff, and steering remains answerable. No agent
    /// can enter this phase yet; a later story populates it.
    Blocked,
    Done,
    Failed,
    Cancelled,
}

impl AgentThreadPhase {
    /// The DTO spelling, shared with the human `/jobs` view.
    pub fn label(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Blocked => "blocked",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentHandoffState {
    Unavailable,
    Ready,
    Acknowledged,
}

impl AgentHandoffState {
    /// The DTO spelling, shared with the human `/jobs` view.
    pub fn label(self) -> &'static str {
        match self {
            Self::Unavailable => "unavailable",
            Self::Ready => "ready",
            Self::Acknowledged => "acknowledged",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AgentThreadSnapshot {
    pub id: u64,
    pub task: String,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    /// Optional display name. Absent from the JSON entirely when `None`,
    /// which keeps pre-`name` fixtures byte-identical.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub phase: AgentThreadPhase,
    /// What this agent is parked on, when it is parked: the permission
    /// **kind** and nothing else (T2-S7, N8). Absent from the JSON entirely
    /// when the agent is not blocked, which keeps every pre-T2-S7 fixture
    /// byte-identical. The type is a unit enum with no argument-bearing
    /// field, so a path or an argv word cannot travel here without altering
    /// a public type (F40).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waiting_for: Option<AgentApprovalKind>,
    pub handoff: AgentHandoffState,
    pub elapsed_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_bytes: Option<usize>,
    pub steering_open: bool,
    pub pending_steering: usize,
    pub timeline: AgentTimelineSnapshot,
}

/// Versioned persistence representation. It deliberately mirrors only the
/// sanitized snapshot, never a runnable handle, permission grant, raw tool
/// event, steering sender, or unbounded terminal result field. Bounded
/// assistant timeline text remains visible.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AgentThreadSnapshotDto {
    pub schema_version: u32,
    pub thread: AgentThreadSnapshot,
}

impl AgentThreadSnapshotDto {
    pub fn from_snapshot(snapshot: &AgentThreadSnapshot) -> Self {
        Self {
            schema_version: AGENT_THREAD_SCHEMA_VERSION,
            thread: snapshot.clone(),
        }
    }

    pub fn validate(&self, limits: AgentThreadLimits) -> Result<(), AgentThreadDtoError> {
        let limits = limits
            .validate()
            .map_err(|error| AgentThreadDtoError(error.to_string()))?;
        if self.schema_version != AGENT_THREAD_SCHEMA_VERSION {
            return Err(AgentThreadDtoError(format!(
                "unsupported agent thread schema {}; expected {}",
                self.schema_version, AGENT_THREAD_SCHEMA_VERSION
            )));
        }
        if self.thread.timeline.entries.len() > limits.max_events {
            return Err(AgentThreadDtoError(
                "agent thread timeline exceeds its event limit".to_string(),
            ));
        }
        let mut retained_bytes = 0usize;
        let mut prior_sequence = None;
        for entry in &self.thread.timeline.entries {
            if entry.text.len() > limits.max_entry_bytes
                || sanitize_and_redact_text(&entry.text) != entry.text
            {
                return Err(AgentThreadDtoError(
                    "agent thread timeline contains oversized or unsafe text".to_string(),
                ));
            }
            if prior_sequence.is_some_and(|prior| entry.sequence <= prior) {
                return Err(AgentThreadDtoError(
                    "agent thread timeline sequence is not strictly increasing".to_string(),
                ));
            }
            prior_sequence = Some(entry.sequence);
            retained_bytes = retained_bytes.saturating_add(entry.text.len());
        }
        if retained_bytes != self.thread.timeline.retained_bytes
            || retained_bytes > limits.max_bytes
            || self.thread.pending_steering > limits.steering_capacity
        {
            return Err(AgentThreadDtoError(
                "agent thread timeline accounting is invalid".to_string(),
            ));
        }
        if self.thread.task.len() > limits.max_entry_bytes
            || self.thread.model.len() > limits.max_entry_bytes
            || sanitize_and_redact_text(&self.thread.task) != self.thread.task
            || sanitize_and_redact_text(&self.thread.model) != self.thread.model
            || self.thread.profile.as_ref().is_some_and(|profile| {
                profile.len() > 256 || sanitize_and_redact_text(profile) != *profile
            })
        {
            return Err(AgentThreadDtoError(
                "agent thread metadata contains oversized or unsafe text".to_string(),
            ));
        }
        let terminal = !matches!(
            self.thread.phase,
            AgentThreadPhase::Running | AgentThreadPhase::Blocked
        );
        if terminal != self.thread.timeline.closed
            || (terminal && self.thread.handoff == AgentHandoffState::Unavailable)
            || (!terminal && self.thread.handoff != AgentHandoffState::Unavailable)
            || (terminal && self.thread.steering_open)
        {
            return Err(AgentThreadDtoError(
                "agent thread lifecycle state is inconsistent".to_string(),
            ));
        }
        Ok(())
    }

    pub fn validate_default(&self) -> Result<(), AgentThreadDtoError> {
        self.validate(AgentThreadLimits::default())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentThreadDtoError(String);

impl fmt::Display for AgentThreadDtoError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for AgentThreadDtoError {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentSteeringMessage {
    pub sequence: u64,
    pub correction: String,
}

/// What a submitted correction actually does.
///
/// Steering restarts the worker: the in-flight attempt is dropped and the next
/// one runs a fresh transcript (system + task + latest correction). Callers must
/// be able to say so, and to say how much elapsed work that discards, so
/// `discarded_ms` carries the thread's elapsed time at submission. It is filled
/// in by the registry, which owns the clock.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AgentSteeringReceipt {
    pub id: u64,
    pub sequence: u64,
    pub pending: usize,
    pub discarded_ms: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AgentSteeringErrorKind {
    UnknownAgent,
    NotRunning,
    Closed,
    Empty,
    TooLarge,
    Full,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentSteeringError {
    pub id: u64,
    pub kind: AgentSteeringErrorKind,
    pub limit: Option<usize>,
}

impl AgentSteeringError {
    pub(crate) fn new(id: u64, kind: AgentSteeringErrorKind, limit: Option<usize>) -> Self {
        Self { id, kind, limit }
    }
}

impl fmt::Display for AgentSteeringError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.kind {
            AgentSteeringErrorKind::UnknownAgent => {
                write!(formatter, "no background agent #{}", self.id)
            }
            AgentSteeringErrorKind::NotRunning => {
                write!(
                    formatter,
                    "background agent #{} is no longer running",
                    self.id
                )
            }
            AgentSteeringErrorKind::Closed => {
                write!(
                    formatter,
                    "background agent #{} is closed to steering",
                    self.id
                )
            }
            AgentSteeringErrorKind::Empty => formatter.write_str("steering correction is empty"),
            AgentSteeringErrorKind::TooLarge => write!(
                formatter,
                "steering correction exceeds the {}-byte limit",
                self.limit.unwrap_or_default()
            ),
            AgentSteeringErrorKind::Full => write!(
                formatter,
                "background agent #{} steering queue is full",
                self.id
            ),
        }
    }
}

impl std::error::Error for AgentSteeringError {}

/// Receiver owned by exactly one running nested-agent loop. Each record gets a
/// distinct channel, so a correction can never be consumed by another agent.
pub struct AgentThreadRuntime {
    receiver: mpsc::Receiver<AgentSteeringMessage>,
}

impl AgentThreadRuntime {
    pub async fn recv_steering(&mut self) -> Option<AgentSteeringMessage> {
        self.receiver.recv().await
    }

    #[cfg(test)]
    pub fn try_recv_steering(&mut self) -> Result<AgentSteeringMessage, mpsc::error::TryRecvError> {
        self.receiver.try_recv()
    }
}

pub(crate) struct AgentThreadState {
    task: String,
    model: String,
    profile: Option<String>,
    phase: AgentThreadPhase,
    /// The argument-free kind this thread is parked on, set by
    /// [`AgentThreadState::record_approval_request`] and cleared by
    /// [`AgentThreadState::unblock`]. A unit enum, so no tool argument can
    /// travel from here to the snapshot (F40).
    waiting_for: Option<AgentApprovalKind>,
    handoff: AgentHandoffState,
    result_bytes: Option<usize>,
    steering_sender: Option<mpsc::Sender<AgentSteeringMessage>>,
    steering_capacity: usize,
    max_steering_bytes: usize,
    next_steering_sequence: u64,
    timeline: AgentTimeline,
}

impl AgentThreadState {
    pub(crate) fn new(
        task: &str,
        model: &str,
        profile: Option<&str>,
        limits: AgentThreadLimits,
    ) -> (Self, AgentThreadRuntime) {
        let limits = limits.validate().expect("validated agent thread limits");
        let (sender, receiver) = mpsc::channel(limits.steering_capacity);
        let mut timeline = AgentTimeline::new(limits);
        timeline.push(AgentTimelineKind::UserTask, None, task);
        (
            Self {
                task: sanitize_and_truncate(task, limits.max_entry_bytes).0,
                model: sanitize_and_truncate(
                    &safe_inline_label(model, 256),
                    limits.max_entry_bytes,
                )
                .0,
                profile: profile.map(|name| safe_inline_label(name, 64)),
                phase: AgentThreadPhase::Running,
                waiting_for: None,
                handoff: AgentHandoffState::Unavailable,
                result_bytes: None,
                steering_sender: Some(sender),
                steering_capacity: limits.steering_capacity,
                max_steering_bytes: limits.max_steering_bytes,
                next_steering_sequence: 0,
                timeline,
            },
            AgentThreadRuntime { receiver },
        )
    }

    pub(crate) fn record_runtime_event(&mut self, event: &AgentRuntimeEvent) {
        if self.phase != AgentThreadPhase::Running {
            return;
        }
        match event {
            AgentRuntimeEvent::ProviderRequest { round, purpose, .. } => self.timeline.push(
                AgentTimelineKind::Provider,
                Some(*round),
                &format!("{} request started", purpose_label(*purpose)),
            ),
            AgentRuntimeEvent::StructuredFinalizationDeferred {
                round,
                repair_round,
                targets,
                ..
            } => self.timeline.push(
                AgentTimelineKind::Lifecycle,
                Some(*round),
                &format!(
                    "structured finalization deferred; repair round {repair_round} for {} target{}",
                    targets.len(),
                    if targets.len() == 1 { "" } else { "s" }
                ),
            ),
            AgentRuntimeEvent::ProviderAttempt {
                round,
                route_generation,
                attempt,
                max_attempts,
                category,
                outcome,
                ..
            } => {
                let route = route_generation
                    .map(|generation| format!(" on route generation {generation}"))
                    .unwrap_or_default();
                self.timeline.push(
                    AgentTimelineKind::Provider,
                    Some(*round),
                    &format!(
                        "attempt {attempt}/{max_attempts}{route}: {}, {}",
                        category_label(*category),
                        outcome_label(*outcome)
                    ),
                )
            }
            AgentRuntimeEvent::AssistantDelta { round, text } => {
                self.timeline
                    .push(AgentTimelineKind::Assistant, Some(*round), text);
            }
            AgentRuntimeEvent::Usage {
                round,
                prompt_tokens,
                completion_tokens,
                total_tokens,
                ..
            } => self.timeline.push(
                AgentTimelineKind::Usage,
                Some(*round),
                &format!(
                    "usage: {prompt_tokens} prompt, {completion_tokens} completion, {total_tokens} total tokens"
                ),
            ),
            AgentRuntimeEvent::ToolStarted { round, name, .. } => self.timeline.push(
                AgentTimelineKind::Tool,
                Some(*round),
                &format!("tool {} started", safe_inline_label(name, 128)),
            ),
            AgentRuntimeEvent::ToolFinished {
                round,
                name,
                success,
                cancelled,
                ..
            } => {
                let outcome = if *cancelled {
                    "cancelled"
                } else if *success {
                    "finished"
                } else {
                    "failed"
                };
                self.timeline.push(
                    AgentTimelineKind::Tool,
                    Some(*round),
                    &format!("tool {} {outcome}", safe_inline_label(name, 128)),
                );
            }
            AgentRuntimeEvent::TurnFinished {
                success,
                tool_errors,
                policy_errors,
                ..
            } => self.timeline.push(
                AgentTimelineKind::Lifecycle,
                None,
                &format!(
                    "turn {}; {tool_errors} tool errors, {policy_errors} policy errors",
                    if *success { "completed" } else { "failed" }
                ),
            ),
        }
    }

    pub(crate) fn submit_steering(
        &mut self,
        id: u64,
        correction: &str,
    ) -> Result<AgentSteeringReceipt, AgentSteeringError> {
        if self.phase != AgentThreadPhase::Running {
            return Err(AgentSteeringError::new(
                id,
                AgentSteeringErrorKind::NotRunning,
                None,
            ));
        }
        if correction.len() > self.max_steering_bytes {
            return Err(AgentSteeringError::new(
                id,
                AgentSteeringErrorKind::TooLarge,
                Some(self.max_steering_bytes),
            ));
        }
        let correction = sanitize_and_redact_text(correction).trim().to_string();
        if correction.is_empty() {
            return Err(AgentSteeringError::new(
                id,
                AgentSteeringErrorKind::Empty,
                None,
            ));
        }
        let Some(sender) = self.steering_sender.as_ref() else {
            return Err(AgentSteeringError::new(
                id,
                AgentSteeringErrorKind::Closed,
                None,
            ));
        };
        let sequence = self.next_steering_sequence.saturating_add(1);
        let message = AgentSteeringMessage {
            sequence,
            correction: correction.clone(),
        };
        match sender.try_send(message) {
            Ok(()) => {
                self.next_steering_sequence = sequence;
                let pending = self.steering_capacity.saturating_sub(sender.capacity());
                self.timeline.push(
                    AgentTimelineKind::Steering,
                    None,
                    &format!("correction #{sequence} queued: {correction}"),
                );
                Ok(AgentSteeringReceipt {
                    id,
                    sequence,
                    pending,
                    discarded_ms: 0,
                })
            }
            Err(mpsc::error::TrySendError::Full(_)) => Err(AgentSteeringError::new(
                id,
                AgentSteeringErrorKind::Full,
                Some(self.steering_capacity),
            )),
            Err(mpsc::error::TrySendError::Closed(_)) => {
                self.steering_sender = None;
                Err(AgentSteeringError::new(
                    id,
                    AgentSteeringErrorKind::Closed,
                    None,
                ))
            }
        }
    }

    pub(crate) fn steering_applied(&mut self, sequence: u64) -> bool {
        if self.phase == AgentThreadPhase::Running {
            self.timeline.push(
                AgentTimelineKind::Lifecycle,
                None,
                &format!("steering correction #{sequence} applied"),
            );
            true
        } else {
            false
        }
    }

    pub(crate) fn close_steering(&mut self, reason: &str) -> bool {
        let was_open = self.steering_sender.take().is_some();
        if was_open && self.phase == AgentThreadPhase::Running {
            self.timeline.push(
                AgentTimelineKind::Lifecycle,
                None,
                &format!("steering closed: {}", safe_inline_label(reason, 256)),
            );
        }
        was_open
    }

    /// Record that this agent parked on a permission request (T2-S6).
    ///
    /// The entry reads `approval to <kind> requested`, built only from
    /// `&'static str` labels and pinned by the registry's park tests.
    ///
    /// Takes the kind and nothing else: the path, grant dir, argv and cwd the
    /// gate parked never reach this function, so no tool argument can be
    /// formatted into the inspection record. Deliberately *not* routed
    /// through `record_runtime_event`, which returns early unless the phase
    /// is `Running` — a park is precisely the `Blocked` phase.
    pub(crate) fn record_approval_request(&mut self, kind: AgentApprovalKind) {
        // The blocked reason travels as this typed kind rather than as a
        // free-form telemetry activity label, which would reach the hook
        // environment as a string.
        self.waiting_for = Some(kind);
        self.timeline.push(
            AgentTimelineKind::Approval,
            None,
            &format!("approval to {} requested", kind.label()),
        );
    }

    /// Record how a parked permission request was decided (T2-S6).
    ///
    /// Reached by every resolution path, including B3's timeout, which is the
    /// one decision taken with nobody present.
    pub(crate) fn record_approval_outcome(
        &mut self,
        kind: AgentApprovalKind,
        outcome: AgentApprovalOutcome,
    ) {
        self.timeline.push(
            AgentTimelineKind::Approval,
            None,
            &format!("approval to {} {}", kind.label(), outcome.label()),
        );
    }

    /// Park a running thread on a user answer (B5). `Blocked` is
    /// non-terminal: `finish` and the steering rules still treat it exactly
    /// as they treat `Running` once [`AgentThreadState::unblock`] resumes it.
    pub(crate) fn block(&mut self) -> bool {
        if self.phase != AgentThreadPhase::Running {
            return false;
        }
        self.phase = AgentThreadPhase::Blocked;
        true
    }

    /// Resume a blocked thread. Every terminal transition goes through
    /// `Running`, so a cancellation of a blocked agent unblocks first.
    pub(crate) fn unblock(&mut self) -> bool {
        if self.phase != AgentThreadPhase::Blocked {
            return false;
        }
        self.phase = AgentThreadPhase::Running;
        self.waiting_for = None;
        true
    }

    pub(crate) fn finish(&mut self, phase: AgentThreadPhase, result_bytes: usize) {
        if self.phase != AgentThreadPhase::Running {
            return;
        }
        debug_assert_ne!(phase, AgentThreadPhase::Running);
        self.phase = phase;
        self.result_bytes = Some(result_bytes);
        self.handoff = AgentHandoffState::Ready;
        self.steering_sender = None;
        self.timeline.push(
            AgentTimelineKind::Lifecycle,
            None,
            match phase {
                AgentThreadPhase::Done => "agent completed; result ready for handoff",
                AgentThreadPhase::Failed => "agent failed; result ready for handoff",
                AgentThreadPhase::Cancelled => "agent cancelled; result ready for handoff",
                AgentThreadPhase::Running | AgentThreadPhase::Blocked => unreachable!(),
            },
        );
        self.timeline.closed = true;
    }

    pub(crate) fn acknowledge_handoff(&mut self) -> bool {
        if self.handoff == AgentHandoffState::Ready {
            self.handoff = AgentHandoffState::Acknowledged;
            true
        } else {
            false
        }
    }

    pub(crate) fn handoff(&self) -> AgentHandoffState {
        self.handoff
    }

    pub(crate) fn profile(&self) -> Option<&str> {
        self.profile.as_deref()
    }

    pub(crate) fn snapshot(&self, id: u64, elapsed_ms: u64) -> AgentThreadSnapshot {
        let pending_steering = self.steering_sender.as_ref().map_or(0, |sender| {
            self.steering_capacity.saturating_sub(sender.capacity())
        });
        AgentThreadSnapshot {
            id,
            task: self.task.clone(),
            model: self.model.clone(),
            profile: self.profile.clone(),
            name: None,
            phase: self.phase,
            waiting_for: (self.phase == AgentThreadPhase::Blocked)
                .then_some(self.waiting_for)
                .flatten(),
            handoff: self.handoff,
            elapsed_ms,
            result_bytes: self.result_bytes,
            steering_open: self.steering_sender.is_some(),
            pending_steering,
            timeline: self.timeline.snapshot(),
        }
    }
}

struct AgentTimeline {
    started_at: Instant,
    limits: AgentThreadLimits,
    entries: VecDeque<AgentTimelineEntry>,
    dropped_entries: u64,
    retained_bytes: usize,
    next_sequence: u64,
    closed: bool,
}

impl AgentTimeline {
    fn new(limits: AgentThreadLimits) -> Self {
        Self {
            started_at: Instant::now(),
            limits,
            entries: VecDeque::new(),
            dropped_entries: 0,
            retained_bytes: 0,
            next_sequence: 0,
            closed: false,
        }
    }

    fn push(&mut self, kind: AgentTimelineKind, round: Option<u32>, text: &str) {
        if self.closed {
            return;
        }
        let (text, truncated) = sanitize_and_truncate(text, self.limits.max_entry_bytes);
        self.next_sequence = self.next_sequence.saturating_add(1);
        self.retained_bytes = self.retained_bytes.saturating_add(text.len());
        self.entries.push_back(AgentTimelineEntry {
            sequence: self.next_sequence,
            elapsed_ms: elapsed_ms(self.started_at),
            kind,
            round,
            text,
            truncated,
        });
        while self.entries.len() > self.limits.max_events
            || self.retained_bytes > self.limits.max_bytes
        {
            let Some(dropped) = self.entries.pop_front() else {
                break;
            };
            self.retained_bytes = self.retained_bytes.saturating_sub(dropped.text.len());
            self.dropped_entries = self.dropped_entries.saturating_add(1);
        }
    }

    fn snapshot(&self) -> AgentTimelineSnapshot {
        AgentTimelineSnapshot {
            entries: self.entries.iter().cloned().collect(),
            dropped_entries: self.dropped_entries,
            retained_bytes: self.retained_bytes,
            closed: self.closed,
        }
    }
}

fn elapsed_ms(started_at: Instant) -> u64 {
    u64::try_from(started_at.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn purpose_label(purpose: ProviderRequestPurpose) -> &'static str {
    match purpose {
        ProviderRequestPurpose::Inference => "inference",
        ProviderRequestPurpose::Compaction => "compaction",
        ProviderRequestPurpose::ModelCatalog => "model catalog",
    }
}

fn category_label(category: ProviderAttemptCategory) -> &'static str {
    match category {
        ProviderAttemptCategory::Success => "success",
        ProviderAttemptCategory::RateLimited => "rate limited",
        ProviderAttemptCategory::Unavailable => "unavailable",
        ProviderAttemptCategory::Authentication => "authentication",
        ProviderAttemptCategory::ClientError => "client error",
        ProviderAttemptCategory::ServerError => "server error",
        ProviderAttemptCategory::UnexpectedStatus => "unexpected status",
        ProviderAttemptCategory::Timeout => "timeout",
        ProviderAttemptCategory::Connection => "connection",
        ProviderAttemptCategory::ResponseTooLarge => "response too large",
        ProviderAttemptCategory::InvalidResponse => "invalid response",
        ProviderAttemptCategory::Cancelled => "cancelled",
    }
}

fn outcome_label(outcome: ProviderAttemptOutcome) -> &'static str {
    match outcome {
        ProviderAttemptOutcome::ResponseAccepted => "accepted",
        ProviderAttemptOutcome::BodyCompleted => "body completed",
        ProviderAttemptOutcome::BodyFailed => "body failed",
        ProviderAttemptOutcome::RetryScheduled => "retry scheduled",
        ProviderAttemptOutcome::Rejected => "rejected",
        ProviderAttemptOutcome::TransportFailed => "transport failed",
        ProviderAttemptOutcome::DeadlineExceeded => "deadline exceeded",
        ProviderAttemptOutcome::Cancelled => "cancelled",
    }
}

pub(crate) fn safe_inline_label(value: &str, max_chars: usize) -> String {
    let value = sanitize_and_redact_text(value);
    let mut output = String::new();
    let mut prior_space = false;
    let mut truncated = false;
    for character in value.chars() {
        let character = if character.is_whitespace() {
            ' '
        } else {
            character
        };
        if character == ' ' && prior_space {
            continue;
        }
        if output.chars().count() == max_chars {
            truncated = true;
            break;
        }
        prior_space = character == ' ';
        output.push(character);
    }
    let output = output.trim();
    if output.is_empty() {
        "unknown".to_string()
    } else if truncated {
        format!("{output}…")
    } else {
        output.to_string()
    }
}

fn sanitize_and_truncate(value: &str, max_bytes: usize) -> (String, bool) {
    let value = sanitize_and_redact_text(value);
    if value.len() <= max_bytes {
        return (value, false);
    }
    if max_bytes == 0 {
        return (String::new(), true);
    }
    const ELLIPSIS: &str = "…";
    let suffix = if max_bytes >= ELLIPSIS.len() {
        ELLIPSIS
    } else {
        ""
    };
    let mut end = max_bytes.saturating_sub(suffix.len());
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    let mut output = value[..end].to_string();
    output.push_str(suffix);
    (output, true)
}

/// Strip terminal controls and redact common credential forms before text can
/// enter a durable snapshot. This is defense in depth, not an authorization
/// boundary; callers must still avoid submitting secrets as agent prompts.
pub(crate) fn sanitize_and_redact_text(value: &str) -> String {
    let sanitized = kitsu_render::sanitize_terminal_text(value);
    let mut output = redact_url_userinfo(&redact_known_prefixes(&sanitized));
    for keyword in [
        "api_key",
        "apikey",
        "access_token",
        "auth_token",
        "authorization",
        "password",
        "secret",
        "token",
    ] {
        output = redact_assignment(&output, keyword);
    }
    redact_bearer(&output)
}

fn redact_known_prefixes(value: &str) -> String {
    const PREFIXES: [&str; 11] = [
        "github_pat_",
        "hf_",
        "sk-",
        "ghp_",
        "gho_",
        "glpat-",
        "npm_",
        "xoxb-",
        "xoxp-",
        "AKIA",
        "AIza",
    ];
    let mut output = String::with_capacity(value.len());
    let mut index = 0;
    while index < value.len() {
        let tail = &value[index..];
        let Some(prefix) = PREFIXES.iter().find(|prefix| tail.starts_with(**prefix)) else {
            let character = tail.chars().next().expect("non-empty tail");
            output.push(character);
            index += character.len_utf8();
            continue;
        };
        let mut end = index + prefix.len();
        while end < value.len() {
            let character = value[end..].chars().next().expect("non-empty tail");
            if !(character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.')) {
                break;
            }
            end += character.len_utf8();
        }
        // Short words such as "hf_model" are ordinary identifiers. Credential
        // prefixes are redacted only when followed by a plausible secret body.
        if end.saturating_sub(index + prefix.len()) >= 8 {
            output.push_str("<redacted>");
            index = end;
        } else {
            let character = tail.chars().next().expect("non-empty tail");
            output.push(character);
            index += character.len_utf8();
        }
    }
    output
}

fn redact_url_userinfo(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut copied = 0;
    let mut search = 0;
    while let Some(relative) = value[search..].find("://") {
        let authority_start = search + relative + 3;
        let authority_end = value[authority_start..]
            .char_indices()
            .find_map(|(offset, character)| {
                matches!(character, '/' | '?' | '#' | ' ' | '\n' | '\t')
                    .then_some(authority_start + offset)
            })
            .unwrap_or(value.len());
        if let Some(relative_at) = value[authority_start..authority_end].rfind('@') {
            let at = authority_start + relative_at;
            output.push_str(&value[copied..authority_start]);
            output.push_str("<redacted>");
            output.push_str(&value[at..authority_end]);
            copied = authority_end;
        }
        search = authority_end;
        if search == value.len() {
            break;
        }
    }
    output.push_str(&value[copied..]);
    output
}

fn redact_assignment(value: &str, keyword: &str) -> String {
    let lowercase = value.to_ascii_lowercase();
    let mut output = String::with_capacity(value.len());
    let mut copied = 0;
    let mut search = 0;
    while let Some(relative) = lowercase[search..].find(keyword) {
        let start = search + relative;
        let before_ok = start == 0 || !lowercase.as_bytes()[start - 1].is_ascii_alphanumeric();
        let after_keyword = start + keyword.len();
        let after_ok = after_keyword == lowercase.len()
            || !lowercase.as_bytes()[after_keyword].is_ascii_alphanumeric();
        if !before_ok || !after_ok {
            search = after_keyword;
            continue;
        }
        let mut delimiter = after_keyword;
        while delimiter < value.len() && value.as_bytes()[delimiter].is_ascii_whitespace() {
            delimiter += 1;
        }
        if delimiter == value.len() || !matches!(value.as_bytes()[delimiter], b'=' | b':') {
            search = after_keyword;
            continue;
        }
        let mut secret_start = delimiter + 1;
        while secret_start < value.len()
            && (value.as_bytes()[secret_start].is_ascii_whitespace()
                || matches!(value.as_bytes()[secret_start], b'\'' | b'"'))
        {
            secret_start += 1;
        }
        let mut end = secret_start;
        while end < value.len()
            && !value.as_bytes()[end].is_ascii_whitespace()
            && !matches!(
                value.as_bytes()[end],
                b',' | b';' | b'&' | b'\'' | b'"' | b'}' | b']'
            )
        {
            end += 1;
        }
        output.push_str(&value[copied..secret_start]);
        output.push_str("<redacted>");
        copied = end;
        search = end;
    }
    output.push_str(&value[copied..]);
    output
}

fn redact_bearer(value: &str) -> String {
    let lowercase = value.to_ascii_lowercase();
    let mut output = String::with_capacity(value.len());
    let mut copied = 0;
    let mut search = 0;
    while let Some(relative) = lowercase[search..].find("bearer") {
        let start = search + relative;
        let before_ok = start == 0 || !lowercase.as_bytes()[start - 1].is_ascii_alphanumeric();
        let mut token_start = start + "bearer".len();
        if !before_ok
            || token_start == value.len()
            || !value.as_bytes()[token_start].is_ascii_whitespace()
        {
            search = token_start;
            continue;
        }
        while token_start < value.len() && value.as_bytes()[token_start].is_ascii_whitespace() {
            token_start += 1;
        }
        let mut end = token_start;
        while end < value.len()
            && !value.as_bytes()[end].is_ascii_whitespace()
            && !matches!(value.as_bytes()[end], b',' | b';' | b'\'' | b'"')
        {
            end += 1;
        }
        output.push_str(&value[copied..token_start]);
        output.push_str("<redacted>");
        copied = end;
        search = end;
    }
    output.push_str(&value[copied..]);
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tiny_limits() -> AgentThreadLimits {
        AgentThreadLimits {
            max_events: 3,
            max_bytes: 128,
            max_entry_bytes: 64,
            steering_capacity: 1,
            max_steering_bytes: 32,
        }
    }

    #[test]
    fn credential_redaction_handles_tokens_assignments_bearer_and_url_userinfo() {
        let rendered = sanitize_and_redact_text(
            "hf_abcdefghijklmnopqrstuvwxyz token=plain-secret Bearer abc.def.ghi \
             https://user:password@example.invalid/private AKIA1234567890ABCDEF",
        );
        for secret in [
            "hf_abcdefghijklmnopqrstuvwxyz",
            "plain-secret",
            "abc.def.ghi",
            "user:password",
            "AKIA1234567890ABCDEF",
        ] {
            assert!(!rendered.contains(secret), "leaked {secret}: {rendered}");
        }
        assert!(rendered.contains("https://<redacted>@example.invalid/private"));
    }

    #[test]
    fn timeline_is_bounded_redacted_and_excludes_tool_payloads() {
        let secret = "hf_abcdefghijklmnopqrstuvwxyz123456";
        let (mut state, _runtime) = AgentThreadState::new(
            &format!("review token={secret}"),
            "model",
            None,
            tiny_limits(),
        );
        state.record_runtime_event(&AgentRuntimeEvent::ToolStarted {
            round: 1,
            call_id: "private-call-id".to_string(),
            name: "read_file".to_string(),
            args: format!(r#"{{"password":"{secret}"}}"#),
            command: None,
        });
        state.record_runtime_event(&AgentRuntimeEvent::ToolFinished {
            round: 1,
            call_id: "private-call-id".to_string(),
            name: "read_file".to_string(),
            success: true,
            cancelled: false,
            output: format!("file contained {secret}"),
            output_bytes: 0,
            output_retained_bytes: 0,
            output_truncated: false,
            artifact_status: kitsu_core::ToolArtifactStatus::NotNeeded,
            command: None,
        });
        state.record_runtime_event(&AgentRuntimeEvent::AssistantDelta {
            round: 1,
            text: format!("done; bearer {secret}\u{1b}[31m"),
        });
        state.record_runtime_event(&AgentRuntimeEvent::AssistantDelta {
            round: 1,
            text: "tail".repeat(30),
        });
        let snapshot = state.snapshot(7, 12);
        let encoded = serde_json::to_string(&snapshot).unwrap();
        assert!(snapshot.timeline.entries.len() <= tiny_limits().max_events);
        assert!(snapshot.timeline.retained_bytes <= tiny_limits().max_bytes);
        assert!(snapshot.timeline.dropped_entries > 0);
        assert!(!encoded.contains(secret));
        assert!(!encoded.contains("private-call-id"));
        assert!(!encoded.contains("password"));
        assert!(!encoded.contains("file contained"));
        assert!(!encoded.contains("\u{1b}"));
        assert!(snapshot
            .timeline
            .entries
            .iter()
            .any(|entry| entry.truncated));
    }

    #[tokio::test]
    async fn steering_is_targeted_bounded_and_closes() {
        let (mut first, mut first_runtime) =
            AgentThreadState::new("one", "model", None, tiny_limits());
        let (mut second, mut second_runtime) =
            AgentThreadState::new("two", "model", None, tiny_limits());
        let receipt = first.submit_steering(1, "focus on parser").unwrap();
        assert_eq!(receipt.id, 1);
        assert_eq!(
            first.submit_steering(1, "queue is full").unwrap_err().kind,
            AgentSteeringErrorKind::Full
        );
        assert!(matches!(
            second_runtime.try_recv_steering(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
        let correction = first_runtime.recv_steering().await.unwrap();
        assert_eq!(correction.correction, "focus on parser");
        assert!(first.steering_applied(correction.sequence));
        assert!(first.close_steering("user closed thread"));
        assert!(first_runtime.recv_steering().await.is_none());
        assert_eq!(
            first.submit_steering(1, "late").unwrap_err().kind,
            AgentSteeringErrorKind::Closed
        );

        second.finish(AgentThreadPhase::Cancelled, 9);
        assert!(second_runtime.recv_steering().await.is_none());
        assert_eq!(second.handoff(), AgentHandoffState::Ready);
        assert!(second.acknowledge_handoff());
        assert_eq!(second.handoff(), AgentHandoffState::Acknowledged);
    }

    #[test]
    fn persistence_dto_round_trip_rejects_unsafe_or_inconsistent_data() {
        let limits = tiny_limits();
        let (mut state, _runtime) = AgentThreadState::new("task", "model", None, limits);
        state.record_runtime_event(&AgentRuntimeEvent::AssistantDelta {
            round: 1,
            text: "safe response".to_string(),
        });
        state.finish(AgentThreadPhase::Done, 13);
        let snapshot = state.snapshot(1, 5);
        let encoded =
            serde_json::to_string(&AgentThreadSnapshotDto::from_snapshot(&snapshot)).unwrap();
        let dto: AgentThreadSnapshotDto = serde_json::from_str(&encoded).unwrap();
        dto.validate(limits).unwrap();

        let mut invalid = dto;
        invalid.thread.timeline.retained_bytes += 1;
        assert!(invalid.validate(limits).is_err());
    }

    // -----------------------------------------------------------------
    // T2-S3 (B5): the `Blocked` phase. Nothing can enter it yet, so every
    // fixture below is built by hand rather than driven through
    // `AgentThreadState`.
    // -----------------------------------------------------------------

    /// A non-terminal `Blocked` thread, built to satisfy the post-B5
    /// lifecycle rules: `handoff == Unavailable`, `timeline.closed == false`,
    /// and steering still open.
    fn blocked_snapshot(name: Option<&str>) -> AgentThreadSnapshot {
        AgentThreadSnapshot {
            id: 7,
            task: "summarize the crate".to_string(),
            model: "qwen2.5-coder".to_string(),
            profile: Some("reviewer".to_string()),
            name: name.map(str::to_string),
            phase: AgentThreadPhase::Blocked,
            waiting_for: None,
            handoff: AgentHandoffState::Unavailable,
            elapsed_ms: 12_500,
            result_bytes: None,
            steering_open: true,
            pending_steering: 0,
            timeline: AgentTimelineSnapshot {
                entries: vec![AgentTimelineEntry {
                    sequence: 1,
                    elapsed_ms: 10,
                    kind: AgentTimelineKind::UserTask,
                    round: None,
                    text: "summarize the crate".to_string(),
                    truncated: false,
                }],
                dropped_entries: 0,
                retained_bytes: 19,
                closed: false,
            },
        }
    }

    /// The wire vocabulary: `blocked` on the phase enum, and the schema
    /// version bumped to 2 because a closed serde enum gained a value.
    #[test]
    fn the_blocked_phase_label_is_blocked_at_schema_version_two() {
        assert_eq!(AgentThreadPhase::Blocked.label(), "blocked");
        assert!(
            AgentThreadPhase::Blocked.label().is_ascii(),
            "phase labels reach the ASCII-only deck"
        );
        assert_eq!(AGENT_THREAD_SCHEMA_VERSION, 2);
    }

    /// The round trip, and `skip_serializing_if` on the new `name`: an absent
    /// name must not appear in the JSON at all, which is what keeps existing
    /// fixtures byte-identical.
    #[test]
    fn a_blocked_dto_round_trips_and_omits_an_absent_name() {
        let snapshot = blocked_snapshot(None);
        let dto = AgentThreadSnapshotDto::from_snapshot(&snapshot);
        dto.validate_default()
            .expect("a blocked thread is a valid non-terminal DTO");

        let encoded = serde_json::to_string(&dto).unwrap();
        let value: serde_json::Value = serde_json::from_str(&encoded).unwrap();
        assert_eq!(value["schema_version"], 2);
        assert_eq!(value["thread"]["phase"], "blocked");
        assert!(
            value["thread"].get("name").is_none(),
            "an absent name is skipped entirely, not serialized as null: {encoded}"
        );

        let decoded: AgentThreadSnapshotDto = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, dto, "the blocked DTO survives a round trip");
        decoded.validate_default().unwrap();
    }

    /// A present `name` is carried, and `deny_unknown_fields` still accepts
    /// the field it now declares.
    #[test]
    fn a_present_thread_name_round_trips() {
        let dto = AgentThreadSnapshotDto::from_snapshot(&blocked_snapshot(Some("reviewer-1")));
        let encoded = serde_json::to_string(&dto).unwrap();
        let value: serde_json::Value = serde_json::from_str(&encoded).unwrap();
        assert_eq!(value["thread"]["name"], "reviewer-1");
        let decoded: AgentThreadSnapshotDto = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded.thread.name.as_deref(), Some("reviewer-1"));
        decoded.validate_default().unwrap();
    }

    /// `terminal = phase not in {Running, Blocked}`: every lifecycle rule in
    /// `validate()` must treat `Blocked` exactly as it treats `Running`.
    #[test]
    fn validate_treats_blocked_as_non_terminal() {
        let dto = |snapshot: AgentThreadSnapshot| AgentThreadSnapshotDto::from_snapshot(&snapshot);

        // A closed timeline is a terminal-only trait.
        let mut closed = blocked_snapshot(None);
        closed.timeline.closed = true;
        assert!(
            dto(closed).validate_default().is_err(),
            "a blocked thread's timeline is still open"
        );

        // `handoff == Unavailable` iff non-terminal, so a blocked thread must
        // never advertise a result to collect.
        for handoff in [AgentHandoffState::Ready, AgentHandoffState::Acknowledged] {
            let mut offered = blocked_snapshot(None);
            offered.handoff = handoff;
            assert!(
                dto(offered).validate_default().is_err(),
                "a blocked thread has no handoff: {handoff:?}"
            );
        }

        // Steering stays available while an agent waits for its answer.
        let mut open = blocked_snapshot(None);
        open.steering_open = true;
        assert!(
            dto(open).validate_default().is_ok(),
            "steering is open on a non-terminal thread"
        );
        let mut shut = blocked_snapshot(None);
        shut.steering_open = false;
        assert!(
            dto(shut).validate_default().is_ok(),
            "a closed steering channel is permitted, just not required"
        );
    }

    /// The version bump is load-bearing: a v1 reader-written DTO carries the
    /// old closed enum and must be rejected by this process's validator.
    #[test]
    fn the_previous_schema_version_is_rejected_by_validate() {
        let mut dto = AgentThreadSnapshotDto::from_snapshot(&blocked_snapshot(None));
        dto.schema_version = 1;
        let error = dto
            .validate_default()
            .expect_err("version 1 is not this process's schema");
        assert!(
            error.to_string().contains("expected 2"),
            "the error names the current schema: {error}"
        );
    }

    // -----------------------------------------------------------------
    // T2-S6: the approval record. `record_approval_request` and
    // `record_approval_outcome` are STUBS; every test below this banner is
    // expected to fail until they append entries.
    // -----------------------------------------------------------------

    /// The wire and human vocabulary. Satisfied by the vocabulary stub alone
    /// (the enums and labels), so it is a spelling pin, not a behaviour one.
    #[test]
    fn the_approval_timeline_kind_and_its_labels_have_one_spelling() {
        assert_eq!(AgentTimelineKind::Approval.label(), "approval");
        let kind = serde_json::to_string(&AgentTimelineKind::Approval).unwrap();
        assert_eq!(kind, "\"approval\"", "the DTO spelling matches the label");
        for (kind, label) in [
            (AgentApprovalKind::ReadFile, "read a file"),
            (AgentApprovalKind::WriteFile, "write a file"),
            (AgentApprovalKind::RunCommand, "run a command"),
        ] {
            assert_eq!(kind.label(), label);
            assert!(kind.label().is_ascii(), "approval labels reach the deck");
        }
        for (outcome, label) in [
            (AgentApprovalOutcome::AllowedOnce, "allowed once"),
            (AgentApprovalOutcome::AllowedAlways, "allowed always"),
            (AgentApprovalOutcome::Denied, "denied"),
            (AgentApprovalOutcome::TimedOut, "timed out"),
        ] {
            assert_eq!(outcome.label(), label);
            assert!(outcome.label().is_ascii());
        }
    }

    /// NON-VACUITY PROOF for every "the path does not appear" assertion in
    /// this file and in `main.rs`.
    ///
    /// `sanitize_and_redact_text` redacts credentials. It does *not* redact a
    /// filesystem path, a URL path, or an argv word. So an implementation
    /// that formatted the request's arguments into the entry text would put
    /// them into the snapshot verbatim, and the absence assertions below
    /// would fail. They are claims about construction (kind-only entries),
    /// not restatements of what redaction already does.
    #[test]
    fn redaction_does_not_remove_a_path_or_an_argv_word() {
        for argument in [
            "/work/secrets/notes.txt",
            "/home/mmpg/workspace/kitsu",
            "rm -rf /work/tmp",
            "https://internal.invalid/private/report",
        ] {
            assert_eq!(
                sanitize_and_redact_text(argument),
                argument,
                "redaction passes arguments through unchanged, so a kind-only \
                 approval entry is the only thing keeping them out"
            );
        }
    }

    /// Parking records a request entry; deciding records the outcome. The
    /// entries carry the kind and nothing else — no path, no grant dir, no
    /// argv.
    #[test]
    fn an_approval_request_and_its_outcome_are_recorded_without_arguments() {
        let (mut state, _runtime) = AgentThreadState::new(
            "summarize the crate",
            "model",
            None,
            AgentThreadLimits::default(),
        );
        assert!(state.block());
        state.record_approval_request(AgentApprovalKind::WriteFile);
        state.record_approval_outcome(
            AgentApprovalKind::WriteFile,
            AgentApprovalOutcome::AllowedOnce,
        );
        assert!(state.unblock());

        let snapshot = state.snapshot(7, 12);
        let approvals = snapshot
            .timeline
            .entries
            .iter()
            .filter(|entry| entry.kind == AgentTimelineKind::Approval)
            .collect::<Vec<_>>();
        assert_eq!(
            approvals.len(),
            2,
            "one entry for the request and one for the decision: {:?}",
            snapshot.timeline.entries
        );
        assert!(
            approvals[0].text.contains("write a file"),
            "the request names what it asks for: {:?}",
            approvals[0]
        );
        assert!(
            approvals[1].text.contains("allowed once"),
            "the decision names the outcome: {:?}",
            approvals[1]
        );
        assert!(
            approvals[0].sequence < approvals[1].sequence,
            "the request precedes its decision"
        );

        // N8 / docs/functionality.md: no tool argument crosses into the
        // inspection record. See `redaction_does_not_remove_a_path_or_an_argv_word`
        // for why this cannot pass vacuously.
        let encoded = serde_json::to_string(&snapshot).unwrap();
        for argument in ["/work", "notes.txt", "rm -rf", "https://"] {
            assert!(
                !encoded.contains(argument),
                "an approval entry leaked an argument ({argument}): {encoded}"
            );
        }
    }

    /// Every outcome is recordable and spelled distinctly, including the
    /// timeout, which is the one case with no human present.
    #[test]
    fn each_approval_outcome_including_the_timeout_leaves_a_distinct_record() {
        for (outcome, expected) in [
            (AgentApprovalOutcome::AllowedOnce, "allowed once"),
            (AgentApprovalOutcome::AllowedAlways, "allowed always"),
            (AgentApprovalOutcome::Denied, "denied"),
            (AgentApprovalOutcome::TimedOut, "timed out"),
        ] {
            let (mut state, _runtime) =
                AgentThreadState::new("task", "model", None, AgentThreadLimits::default());
            state.record_approval_request(AgentApprovalKind::RunCommand);
            state.record_approval_outcome(AgentApprovalKind::RunCommand, outcome);
            let snapshot = state.snapshot(1, 1);
            let texts = snapshot
                .timeline
                .entries
                .iter()
                .filter(|entry| entry.kind == AgentTimelineKind::Approval)
                .map(|entry| entry.text.clone())
                .collect::<Vec<_>>();
            assert_eq!(
                texts.len(),
                2,
                "{outcome:?}: {:?}",
                snapshot.timeline.entries
            );
            assert!(
                texts[1].contains(expected),
                "{outcome:?} is recorded as {expected:?}: {texts:?}"
            );
        }
    }

    /// An approval entry is recorded while the thread is `Blocked`.
    /// `record_runtime_event` returns early unless the phase is `Running`, so
    /// an implementation that routed the approval through that path would
    /// record nothing at all — this is the test that catches it.
    #[test]
    fn an_approval_is_recorded_while_the_thread_is_blocked() {
        let (mut state, _runtime) =
            AgentThreadState::new("task", "model", None, AgentThreadLimits::default());
        assert!(state.block());
        assert_eq!(state.snapshot(1, 1).phase, AgentThreadPhase::Blocked);
        state.record_approval_request(AgentApprovalKind::ReadFile);
        assert!(
            state
                .snapshot(1, 1)
                .timeline
                .entries
                .iter()
                .any(|entry| entry.kind == AgentTimelineKind::Approval),
            "the park itself happens in the Blocked phase, so the entry must \
             be recordable there"
        );
    }

    /// Item 5: a parked-and-answered agent cannot grow the timeline without
    /// limit, and the DTO it produces still validates (which re-checks the
    /// redaction and accounting invariants for the new entries).
    #[test]
    fn repeated_approvals_stay_within_the_timeline_budget_and_validate() {
        let limits = tiny_limits();
        let (mut state, _runtime) = AgentThreadState::new("task", "model", None, limits);
        for _ in 0..64 {
            state.record_approval_request(AgentApprovalKind::WriteFile);
            state.record_approval_outcome(
                AgentApprovalKind::WriteFile,
                AgentApprovalOutcome::Denied,
            );
        }
        let snapshot = state.snapshot(3, 4);
        assert!(
            snapshot
                .timeline
                .entries
                .iter()
                .any(|entry| entry.kind == AgentTimelineKind::Approval),
            "approvals are recorded at all: {:?}",
            snapshot.timeline.entries
        );
        assert!(snapshot.timeline.entries.len() <= limits.max_events);
        assert!(snapshot.timeline.retained_bytes <= limits.max_bytes);
        assert!(
            snapshot.timeline.dropped_entries > 0,
            "128 approval entries do not fit in a 3-event budget"
        );
        AgentThreadSnapshotDto::from_snapshot(&snapshot)
            .validate(limits)
            .expect("approval entries satisfy the DTO's redaction and accounting rules");
    }

    // -----------------------------------------------------------------
    // F43: a cancelled approval is not a timeout. `AgentApprovalOutcome`
    // has no `Cancelled` variant today, so both tests below are expected
    // to fail to COMPILE until the variant and its label exist.
    // -----------------------------------------------------------------

    /// The fifth outcome's spelling, and that it is its own word rather than
    /// a second name for the timeout. Vocabulary pin only, no behaviour.
    #[test]
    fn a_cancelled_approval_has_its_own_label_distinct_from_the_timeout() {
        assert_eq!(AgentApprovalOutcome::Cancelled.label(), "cancelled");
        assert!(AgentApprovalOutcome::Cancelled.label().is_ascii());
        assert_ne!(
            AgentApprovalOutcome::Cancelled.label(),
            AgentApprovalOutcome::TimedOut.label(),
            "a human cancellation and an unattended timeout are different facts"
        );
        let labels = [
            AgentApprovalOutcome::AllowedOnce,
            AgentApprovalOutcome::AllowedAlways,
            AgentApprovalOutcome::Denied,
            AgentApprovalOutcome::TimedOut,
            AgentApprovalOutcome::Cancelled,
        ]
        .map(AgentApprovalOutcome::label);
        let mut unique = labels.to_vec();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(
            unique.len(),
            labels.len(),
            "five outcomes, five spellings: {labels:?}"
        );
    }

    /// Every outcome's recorded entry text, pinned by exact equality rather
    /// than by `contains`, so neither the cancellation nor the timeout can
    /// be recorded as the other (F41: pin what the output *is*).
    #[test]
    fn every_outcome_including_cancellation_records_its_own_exact_entry_text() {
        for (outcome, expected) in [
            (
                AgentApprovalOutcome::AllowedOnce,
                "approval to write a file allowed once",
            ),
            (
                AgentApprovalOutcome::AllowedAlways,
                "approval to write a file allowed always",
            ),
            (
                AgentApprovalOutcome::Denied,
                "approval to write a file denied",
            ),
            (
                AgentApprovalOutcome::TimedOut,
                "approval to write a file timed out",
            ),
            (
                AgentApprovalOutcome::Cancelled,
                "approval to write a file cancelled",
            ),
        ] {
            let (mut state, _runtime) =
                AgentThreadState::new("task", "model", None, AgentThreadLimits::default());
            assert!(state.block());
            state.record_approval_request(AgentApprovalKind::WriteFile);
            state.record_approval_outcome(AgentApprovalKind::WriteFile, outcome);
            let texts = state
                .snapshot(1, 1)
                .timeline
                .entries
                .into_iter()
                .filter(|entry| entry.kind == AgentTimelineKind::Approval)
                .map(|entry| entry.text)
                .collect::<Vec<_>>();
            assert_eq!(
                texts,
                vec![
                    "approval to write a file requested".to_string(),
                    expected.to_string()
                ],
                "{outcome:?} records one request and one outcome, spelled exactly"
            );
        }
    }

    // -----------------------------------------------------------------
    // T2-S7: `waiting_for` on the wire. The DTO field exists as a STUB
    // (`AgentThreadState::snapshot` always publishes `None`) and
    // `AgentApprovalKind::wire` is a STUB returning "", so the two tests
    // below that pin content FAIL until the implementer lands the card.
    // -----------------------------------------------------------------

    /// One machine vocabulary for the three surfaces outside the process.
    /// `wire()` is what the state file, the hook environment and
    /// `kitsu agents --json` all carry, and it is the serde spelling too, so a
    /// reader of any one of them can compare bytes with a reader of another.
    /// It is deliberately *not* `label()`: that is the English phrase `/jobs`
    /// prints after "approval to".
    #[test]
    fn the_waiting_for_kind_has_one_machine_spelling_shared_with_serde() {
        for (kind, wire, label) in [
            (AgentApprovalKind::ReadFile, "read_file", "read a file"),
            (AgentApprovalKind::WriteFile, "write_file", "write a file"),
            (
                AgentApprovalKind::RunCommand,
                "run_command",
                "run a command",
            ),
        ] {
            assert_eq!(kind.wire(), wire, "{kind:?} has one machine spelling");
            assert!(kind.wire().is_ascii(), "{kind:?} reaches a child's environ");
            assert_eq!(
                serde_json::to_string(&kind).unwrap(),
                format!("\"{wire}\""),
                "{kind:?}: the serde spelling is the machine spelling"
            );
            assert_eq!(kind.label(), label, "the human phrase is unchanged");
            assert_ne!(
                kind.wire(),
                kind.label(),
                "{kind:?}: the machine word and the human phrase are distinct"
            );
        }
    }

    /// The F40 guard, stated structurally: `waiting_for` is a closed unit enum,
    /// so a path or an argv word is not merely absent from it -- it cannot be
    /// *represented* in it. A foreign writer that put one there produces a DTO
    /// this binary refuses to decode rather than one it prints.
    ///
    /// Not vacuous: the same slot with a real kind decodes, which is asserted
    /// first, so the refusals below are about the value and not about the
    /// field, the fixture or the surrounding JSON.
    #[test]
    fn a_waiting_for_field_cannot_represent_a_path_or_an_argv_word() {
        let encode = |waiting_for: serde_json::Value| {
            let mut dto = serde_json::to_value(AgentThreadSnapshotDto::from_snapshot(
                &blocked_snapshot(None),
            ))
            .unwrap();
            dto["thread"]["waiting_for"] = waiting_for;
            dto
        };

        let good = encode(serde_json::json!("write_file"));
        let decoded: AgentThreadSnapshotDto =
            serde_json::from_value(good).expect("a real kind decodes in this slot");
        assert_eq!(
            decoded.thread.waiting_for,
            Some(AgentApprovalKind::WriteFile)
        );

        for leak in [
            serde_json::json!("/work/notes.txt"),
            serde_json::json!("rm -rf /work/tmp"),
            serde_json::json!("write a file"),
            serde_json::json!({"write_file": "/work/notes.txt"}),
        ] {
            assert!(
                serde_json::from_value::<AgentThreadSnapshotDto>(encode(leak.clone())).is_err(),
                "waiting_for must not accept {leak}"
            );
        }
    }

    /// The shape on the wire: the kind's machine word when parked, and the key
    /// gone entirely when not -- which is what keeps every pre-T2-S7 fixture
    /// byte-identical.
    #[test]
    fn a_blocked_dto_publishes_its_waiting_for_kind_and_omits_it_when_absent() {
        let mut snapshot = blocked_snapshot(None);
        snapshot.waiting_for = Some(AgentApprovalKind::WriteFile);
        let dto = AgentThreadSnapshotDto::from_snapshot(&snapshot);
        dto.validate(AgentThreadLimits::default())
            .expect("a parked thread is a valid non-terminal DTO");
        let value = serde_json::to_value(&dto).unwrap();
        assert_eq!(value["thread"]["waiting_for"], "write_file");
        assert_eq!(
            value["thread"]["waiting_for"],
            AgentApprovalKind::WriteFile.wire(),
            "the DTO spelling is `wire()`, not a second vocabulary"
        );
        assert_eq!(value["thread"]["phase"], "blocked");
        let decoded: AgentThreadSnapshotDto = serde_json::from_value(value).unwrap();
        assert_eq!(decoded, dto, "the parked DTO survives a round trip");

        let mut running = blocked_snapshot(None);
        running.phase = AgentThreadPhase::Running;
        running.waiting_for = None;
        let value = serde_json::to_value(AgentThreadSnapshotDto::from_snapshot(&running)).unwrap();
        assert!(
            value["thread"].get("waiting_for").is_none(),
            "an unparked thread carries no waiting_for key at all: {value}"
        );
    }
}
