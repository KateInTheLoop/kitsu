//! Subagents (design docs/subagents-design.md, Stage A): the `spawn_agent` tool
//! runs a nested, ISOLATED agent loop on a delegated task and returns only the
//! conclusion to the parent — so a big exploration stays out of the main context.
//!
//! The subagent inherits the provider, permissions, tools, and skills, but gets
//! a FRESH message list and its own context budget. It is NOT given the
//! `spawn_agent` tool (depth-1), and its filesystem actions still go through the
//! shared permission model — no privilege escalation.

pub mod profile;
pub mod thread;

use self::thread::{
    AgentHandoffState, AgentSteeringError, AgentSteeringErrorKind, AgentSteeringReceipt,
    AgentThreadLimitError, AgentThreadLimits, AgentThreadPhase, AgentThreadRuntime,
    AgentThreadSnapshot, AgentThreadSnapshotDto,
};

use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use futures::StreamExt;
use kitsu_core::{
    bound_inline_output, run_agent_turn, AgentConfig, AgentError, AgentEventLog, AgentRuntimeEvent,
    ArtifactStoragePolicy, SpawnBudget,
};
use kitsu_providers::{
    ChatMessage, Provider, ProviderAttemptCategory, ProviderAttemptOutcome, ProviderError,
    ToolDescriptor,
};
use kitsu_tools::{HandlerFn, Registry, ToolError};
use tokio::sync::watch;

#[derive(serde::Deserialize)]
struct SpawnArgs {
    task: String,
    #[serde(default)]
    profile: Option<String>,
}

#[derive(serde::Deserialize)]
struct SpawnManyArgs {
    tasks: Vec<String>,
}

#[derive(serde::Deserialize)]
struct ResultArgs {
    id: u64,
}

/// Cap on concurrent subagents in one `spawn_agents` call, so the model can't
/// fan out unboundedly against the gateway. Extra tasks beyond this are still run
/// — just not all at once (see `buffer` below).
const MAX_PARALLEL: usize = 8;

/// The model-visible error when the per-turn spawn budget is exhausted
/// (config `agent_spawn_cap` / `--agent-spawn-cap`). Phrased as an instruction
/// so a storming model course-corrects instead of retrying the spawn.
fn spawn_cap_error(budget: &SpawnBudget) -> String {
    format!(
        "agent spawn cap reached: this turn already started {} of {} allowed subagents. Do the \
         remaining work yourself, inline, without spawning agents; the user can raise the cap \
         with `agent_spawn_cap` in the config or --agent-spawn-cap.",
        budget.used(),
        budget.cap()
    )
}
/// Maximum number of content-free activity labels retained per background job.
const RECENT_ACTIVITY_LIMIT: usize = 8;
/// Default limit on simultaneously running detached subagents.
pub const DEFAULT_MAX_BACKGROUND_RUNNING: usize = 8;
/// Default number of detached-agent records retained until explicitly pruned.
pub const DEFAULT_MAX_BACKGROUND_RECORDS: usize = 128;

/// The live `(model, context_window)` a subagent should use, shared with the
/// chat loop so `/model` (which changes the model and re-discovers the window)
/// propagates to subagents instead of them running the session's initial model
/// against a stale window.
pub type LiveModelWindow = Arc<Mutex<(String, Option<u64>)>>;

/// System prompt seeding a subagent's isolated context.
const SUBAGENT_SYSTEM: &str = "You are a kitsu subagent handling ONE delegated task in a fresh, \
isolated context (you cannot see the parent conversation). Use your tools to complete it, then \
reply with a concise, self-contained result the caller can use directly. Do not ask questions — \
do the task with what you have.";

/// Status of a background subagent (Stage C).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentStatus {
    Running,
    Done(String),
    Failed(String),
    Cancelled(String),
}

struct AgentRecord {
    task: String,
    status: AgentStatus,
    handle: Option<tokio::task::JoinHandle<()>>,
    started_at: Instant,
    finished_at: Option<Instant>,
    telemetry: AgentTelemetry,
    thread: thread::AgentThreadState,
}

/// Safe, bounded observability collected for one background subagent.
///
/// Activity labels deliberately exclude prompts, tool arguments/results, and
/// provider response bodies. Full results remain available only through the
/// existing explicit [`AgentRegistry::result`] API.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AgentTelemetry {
    pub model: String,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
    pub tool_count: u64,
    pub write_count: u64,
    pub last_activity: Option<String>,
    pub recent_activity: Vec<String>,
    pub failure_class: Option<AgentFailureClass>,
}

/// Stable, body-free classification for a failed background turn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentFailureClass {
    ProviderConnection,
    ProviderTimeout,
    ProviderInvalidResponse,
    ProviderAuthentication,
    ProviderRateLimited,
    ProviderClientError,
    ProviderServerError,
    ProviderHttp,
    ProviderToolsUnsupported,
    ProviderAuthConfiguration,
    AgentLoop,
    ContextOverflow,
    ContextWindowUnknown,
}

impl fmt::Display for AgentFailureClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            Self::ProviderConnection => "provider_connection",
            Self::ProviderTimeout => "provider_timeout",
            Self::ProviderInvalidResponse => "provider_invalid_response",
            Self::ProviderAuthentication => "provider_authentication",
            Self::ProviderRateLimited => "provider_rate_limited",
            Self::ProviderClientError => "provider_client_error",
            Self::ProviderServerError => "provider_server_error",
            Self::ProviderHttp => "provider_http",
            Self::ProviderToolsUnsupported => "provider_tools_unsupported",
            Self::ProviderAuthConfiguration => "provider_auth_configuration",
            Self::AgentLoop => "agent_loop",
            Self::ContextOverflow => "context_overflow",
            Self::ContextWindowUnknown => "context_window_unknown",
        };
        f.write_str(label)
    }
}

struct SubagentRunReport {
    result: Result<String, String>,
    telemetry: AgentTelemetry,
}

impl AgentTelemetry {
    fn started(model: String) -> Self {
        Self {
            model: thread::safe_inline_label(&model, 256),
            last_activity: Some("started".to_string()),
            recent_activity: vec!["started".to_string()],
            ..Self::default()
        }
    }

    fn record_activity(&mut self, activity: String) {
        self.last_activity = Some(activity.clone());
        if self.recent_activity.len() == RECENT_ACTIVITY_LIMIT {
            self.recent_activity.remove(0);
        }
        self.recent_activity.push(activity);
    }
}

/// Admission limits for the detached-agent registry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AgentRegistryLimits {
    pub max_running: usize,
    pub max_records: usize,
}

impl Default for AgentRegistryLimits {
    fn default() -> Self {
        Self {
            max_running: DEFAULT_MAX_BACKGROUND_RUNNING,
            max_records: DEFAULT_MAX_BACKGROUND_RECORDS,
        }
    }
}

/// Why a detached subagent could not be admitted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentCapacityKind {
    Running,
    Records,
}

/// A rejected detached-agent start. Existing work is never evicted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentStartError {
    pub kind: AgentCapacityKind,
    pub current: usize,
    pub limit: usize,
}

impl fmt::Display for AgentStartError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.kind {
            AgentCapacityKind::Running => write!(
                f,
                "background-agent concurrency limit reached ({} running, limit {}); wait for or cancel an agent",
                self.current, self.limit
            ),
            AgentCapacityKind::Records => write!(
                f,
                "background-agent record capacity reached ({} retained, limit {}); prune terminal agents before starting another",
                self.current, self.limit
            ),
        }
    }
}

impl std::error::Error for AgentStartError {}

/// Detailed, non-secret progress for one retained background agent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentProgress {
    pub id: u64,
    pub task: String,
    /// The specialist profile this agent was spawned with, if any. Presentation
    /// only — the deck needs it to say *what kind* of worker a row is.
    pub profile: Option<String>,
    pub status: AgentProgressStatus,
    pub elapsed_ms: u64,
    pub result_bytes: Option<usize>,
    pub telemetry: AgentTelemetry,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentProgressStatus {
    Running,
    Done,
    Failed,
    Cancelled,
}

/// Rich registry state for `/agents`-style status UIs. The existing compact
/// watch snapshot remains source-compatible for current frontends.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentProgressSnapshot {
    pub generation: u64,
    pub total: usize,
    pub running: usize,
    pub done: usize,
    pub failed: usize,
    pub cancelled: usize,
    pub max_running: usize,
    pub max_records: usize,
    pub available_running_slots: usize,
    pub available_record_slots: usize,
    pub agents: Vec<AgentProgress>,
}

/// Inspectable terminal status. Cancellation APIs publish and return a newly
/// cancelled status only after its task handle has been joined.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentTerminalStatus {
    pub id: u64,
    pub task: String,
    pub status: AgentStatus,
    /// False only for a malformed/unattached running record; production starts
    /// attach atomically and therefore return `true`.
    pub joined: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentCancelOutcome {
    Cancelled(AgentTerminalStatus),
    AlreadyTerminal(AgentTerminalStatus),
    NotFound,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentPruneOutcome {
    Pruned(AgentTerminalStatus),
    Running,
    NotFound,
}

/// Outcome of explicitly acknowledging a terminal result handoff.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentHandoffAckOutcome {
    Acknowledged,
    AlreadyAcknowledged,
    NotReady,
    NotFound,
}

/// Outcome of closing one agent's correction channel without stopping it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentSteeringCloseOutcome {
    Closed,
    AlreadyClosed,
    NotRunning,
    NotFound,
}

/// Registry of background subagents, shared between `spawn_agent_bg`,
/// `agent_result`, and the `/agents` command. Detached subagents update their
/// record on completion; the user (via `/agents`) or the model (via
/// `agent_result`) reads status/results back.
///
/// Interactive frontends can subscribe to race-free state changes with
/// [`AgentRegistry::subscribe`]. Watch updates are snapshots rather than edge
/// notifications: a slow receiver may observe several changes at once, but it
/// can never miss the latest running/terminal counts or the monotonic
/// generation identifying that the registry changed.
pub struct AgentRegistry {
    agents: Mutex<HashMap<u64, AgentRecord>>,
    next: AtomicU64,
    generation: AtomicU64,
    changes: watch::Sender<AgentRegistrySnapshot>,
    limits: AgentRegistryLimits,
    thread_limits: AgentThreadLimits,
}

/// Cheap state published whenever the externally visible agent registry
/// changes. `terminal` includes done, failed, and cancelled agents; `failed` is
/// carried separately so a watcher can report a detached failure as a failure
/// instead of as generic completion.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AgentRegistrySnapshot {
    pub generation: u64,
    pub running: usize,
    pub terminal: usize,
    pub failed: usize,
}

impl Default for AgentRegistry {
    fn default() -> Self {
        Self::with_config(AgentRegistryLimits::default())
    }
}

impl AgentRegistry {
    fn with_config(limits: AgentRegistryLimits) -> Self {
        let (changes, _receiver) = watch::channel(AgentRegistrySnapshot::default());
        Self {
            agents: Mutex::new(HashMap::new()),
            next: AtomicU64::new(0),
            generation: AtomicU64::new(0),
            changes,
            limits,
            thread_limits: AgentThreadLimits::default(),
        }
    }

    #[cfg(test)]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Construct a registry with explicit admission and retention bounds.
    /// Zero is valid and disables the corresponding kind of admission.
    #[cfg(test)]
    pub fn with_limits(limits: AgentRegistryLimits) -> Arc<Self> {
        Arc::new(Self::with_config(limits))
    }

    /// Construct a registry with explicit job and per-thread retention bounds.
    pub fn with_limits_and_thread_limits(
        limits: AgentRegistryLimits,
        thread_limits: AgentThreadLimits,
    ) -> Result<Arc<Self>, AgentThreadLimitError> {
        let thread_limits = thread_limits.validate()?;
        let mut registry = Self::with_config(limits);
        registry.thread_limits = thread_limits;
        Ok(Arc::new(registry))
    }

    /// Register a new running agent, returning its id.
    #[cfg(test)]
    pub(crate) fn start(&self, task: String) -> Result<u64, AgentStartError> {
        let (id, update) = {
            let mut agents = self.agents.lock().expect("agent registry");
            self.check_capacity(&agents)?;
            let id = self.next.fetch_add(1, Ordering::Relaxed) + 1;
            let (thread, _runtime) =
                thread::AgentThreadState::new(&task, "", None, self.thread_limits);
            agents.insert(
                id,
                AgentRecord {
                    task,
                    status: AgentStatus::Running,
                    handle: None,
                    started_at: Instant::now(),
                    finished_at: None,
                    telemetry: AgentTelemetry::started(String::new()),
                    thread,
                },
            );
            (id, self.changed_snapshot(&agents))
        };
        self.publish(update);
        Ok(id)
    }

    /// Admit and atomically attach one detached task. The spawned future can
    /// finish immediately, but its completion blocks on the registry lock until
    /// the handle is installed, closing the start/attach cancellation race.
    #[cfg(test)]
    fn spawn_background<F>(
        self: &Arc<Self>,
        task: String,
        future: F,
    ) -> Result<u64, AgentStartError>
    where
        F: std::future::Future<Output = Result<String, String>> + Send + 'static,
    {
        self.spawn_background_report(task, "test-model".to_string(), move |_, _| async move {
            SubagentRunReport {
                result: future.await,
                telemetry: AgentTelemetry::started("test-model".to_string()),
            }
        })
    }

    /// Admit and atomically attach one instrumented detached task.
    #[cfg(test)]
    fn spawn_background_report<F, Fut>(
        self: &Arc<Self>,
        task: String,
        model: String,
        build: F,
    ) -> Result<u64, AgentStartError>
    where
        F: FnOnce(u64, Arc<Self>) -> Fut,
        Fut: std::future::Future<Output = SubagentRunReport> + Send + 'static,
    {
        let full_task = task.clone();
        self.spawn_background_thread(
            task,
            full_task,
            model,
            None,
            move |id, registry, _runtime| build(id, registry),
        )
    }

    /// Admit and atomically attach one detached task with an inspectable thread
    /// and a correction receiver owned by its nested turn.
    fn spawn_background_thread<F, Fut>(
        self: &Arc<Self>,
        task: String,
        full_task: String,
        model: String,
        profile: Option<String>,
        build: F,
    ) -> Result<u64, AgentStartError>
    where
        F: FnOnce(u64, Arc<Self>, AgentThreadRuntime) -> Fut,
        Fut: std::future::Future<Output = SubagentRunReport> + Send + 'static,
    {
        let (id, update) = {
            let mut agents = self.agents.lock().expect("agent registry");
            self.check_capacity(&agents)?;
            let id = self.next.fetch_add(1, Ordering::Relaxed) + 1;
            let (thread, runtime) = thread::AgentThreadState::new(
                &full_task,
                &model,
                profile.as_deref(),
                self.thread_limits,
            );
            agents.insert(
                id,
                AgentRecord {
                    task,
                    status: AgentStatus::Running,
                    handle: None,
                    started_at: Instant::now(),
                    finished_at: None,
                    telemetry: AgentTelemetry::started(model),
                    thread,
                },
            );
            let running_registry = self.clone();
            let future = build(id, running_registry.clone(), runtime);
            let handle = tokio::spawn(async move {
                running_registry.finish_report(id, future.await);
            });
            agents
                .get_mut(&id)
                .expect("new background agent record")
                .handle = Some(handle);
            (id, self.changed_snapshot(&agents))
        };
        self.publish(update);
        Ok(id)
    }

    fn check_capacity(&self, agents: &HashMap<u64, AgentRecord>) -> Result<(), AgentStartError> {
        let running = agents
            .values()
            .filter(|record| matches!(record.status, AgentStatus::Running))
            .count();
        if running >= self.limits.max_running {
            return Err(AgentStartError {
                kind: AgentCapacityKind::Running,
                current: running,
                limit: self.limits.max_running,
            });
        }
        if agents.len() >= self.limits.max_records {
            return Err(AgentStartError {
                kind: AgentCapacityKind::Records,
                current: agents.len(),
                limit: self.limits.max_records,
            });
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn finish(&self, id: u64, result: Result<String, String>) {
        self.finish_with_telemetry(id, result, None);
    }

    /// Mark an agent done/failed and publish its content-free telemetry.
    fn finish_report(&self, id: u64, report: SubagentRunReport) {
        self.finish_with_telemetry(id, report.result, Some(report.telemetry));
    }

    fn finish_with_telemetry(
        &self,
        id: u64,
        result: Result<String, String>,
        telemetry: Option<AgentTelemetry>,
    ) {
        let update = {
            let mut agents = self.agents.lock().expect("agent registry");
            let Some(rec) = agents.get_mut(&id) else {
                return;
            };
            // Cancellation is terminal. An abort races with a task leaving its
            // final await; a late completion must never rewrite "cancelled" to
            // "done" after the UI has claimed termination.
            if !matches!(rec.status, AgentStatus::Running) {
                return;
            }
            if let Some(telemetry) = telemetry {
                rec.telemetry = telemetry;
            }
            let (status, phase, result_bytes) = match result {
                Ok(result) => {
                    let bytes = result.len();
                    (AgentStatus::Done(result), AgentThreadPhase::Done, bytes)
                }
                Err(error) => {
                    let bytes = error.len();
                    (AgentStatus::Failed(error), AgentThreadPhase::Failed, bytes)
                }
            };
            rec.status = status;
            rec.thread.finish(phase, result_bytes);
            rec.handle = None;
            rec.finished_at = Some(Instant::now());
            self.changed_snapshot(&agents)
        };
        self.publish(update);
    }

    /// Fold one body-free runtime event into a still-running job and publish a
    /// new generation immediately. This powers truthful `/jobs` progress while
    /// the nested turn is active rather than only after its final response.
    fn record_progress_event(&self, id: u64, event: AgentRuntimeEvent) {
        let update = {
            let mut agents = self.agents.lock().expect("agent registry");
            let Some(record) = agents.get_mut(&id) else {
                return;
            };
            if !matches!(record.status, AgentStatus::Running) {
                return;
            }
            record.thread.record_runtime_event(&event);
            record_telemetry_event(&mut record.telemetry, event);
            self.changed_snapshot(&agents)
        };
        self.publish(update);
    }

    #[cfg(test)]
    fn attach(&self, id: u64, handle: tokio::task::JoinHandle<()>) {
        if let Some(rec) = self.agents.lock().expect("agent registry").get_mut(&id) {
            if matches!(rec.status, AgentStatus::Running) {
                rec.handle = Some(handle);
            } else {
                handle.abort();
            }
        } else {
            handle.abort();
        }
    }

    /// Highest id allocated before a parent turn starts.
    pub fn watermark(&self) -> u64 {
        self.next.load(Ordering::Relaxed)
    }

    /// Subscribe to registry changes without a check-then-sleep race.
    ///
    /// The receiver immediately contains the latest snapshot, including changes
    /// that completed before subscription. Call `borrow_and_update()` once to
    /// establish a baseline, then await `changed()`; after a wake, use
    /// [`AgentRegistry::result`] or [`AgentRegistry::list`] for authoritative
    /// details. Because the generation is monotonic, coalesced watch updates do
    /// not masquerade as a single agent completion.
    pub fn subscribe(&self) -> watch::Receiver<AgentRegistrySnapshot> {
        self.changes.subscribe()
    }

    /// Return the most recently published registry snapshot.
    pub fn snapshot(&self) -> AgentRegistrySnapshot {
        *self.changes.borrow()
    }

    /// Detailed capacity, terminal-outcome, and per-agent progress snapshot.
    /// Running records expose their model/start activity immediately; usage,
    /// tool/write totals, recent activity, and safe failure class are filled
    /// from the nested turn's event log when it reaches a terminal state.
    pub fn progress_snapshot(&self) -> AgentProgressSnapshot {
        let agents = self.agents.lock().expect("agent registry");
        let mut progress = agents
            .iter()
            .map(|(id, record)| agent_progress(*id, record))
            .collect::<Vec<_>>();
        progress.sort_by_key(|agent| agent.id);
        let mut running = 0;
        let mut done = 0;
        let mut failed = 0;
        let mut cancelled = 0;
        for agent in &progress {
            match agent.status {
                AgentProgressStatus::Running => running += 1,
                AgentProgressStatus::Done => done += 1,
                AgentProgressStatus::Failed => failed += 1,
                AgentProgressStatus::Cancelled => cancelled += 1,
            }
        }
        AgentProgressSnapshot {
            generation: self.generation.load(Ordering::Relaxed),
            total: progress.len(),
            running,
            done,
            failed,
            cancelled,
            max_running: self.limits.max_running,
            max_records: self.limits.max_records,
            available_running_slots: self.limits.max_running.saturating_sub(running),
            available_record_slots: self.limits.max_records.saturating_sub(progress.len()),
            agents: progress,
        }
    }

    /// Inspect one agent's bounded, redacted task/event thread. It may contain
    /// bounded assistant deltas, but never the registry's unbounded terminal
    /// result field; use [`AgentRegistry::result`] to perform and acknowledge a
    /// handoff.
    pub fn thread_snapshot(&self, id: u64) -> Option<AgentThreadSnapshot> {
        let agents = self.agents.lock().expect("agent registry");
        let record = agents.get(&id)?;
        Some(
            record
                .thread
                .snapshot(id, elapsed_millis(record.started_at, record.finished_at)),
        )
    }

    /// Inspect all retained threads in stable id order.
    pub fn thread_snapshots(&self) -> Vec<AgentThreadSnapshot> {
        let agents = self.agents.lock().expect("agent registry");
        let mut snapshots = agents
            .iter()
            .map(|(id, record)| {
                record
                    .thread
                    .snapshot(*id, elapsed_millis(record.started_at, record.finished_at))
            })
            .collect::<Vec<_>>();
        snapshots.sort_by_key(|snapshot| snapshot.id);
        snapshots
    }

    /// Export sanitized, versioned DTOs. This never serializes the unbounded
    /// terminal result field, tool arguments/results, provider bodies, handles,
    /// or permission grants. Bounded assistant timeline text remains visible.
    pub fn thread_snapshot_dtos(&self) -> Vec<AgentThreadSnapshotDto> {
        self.thread_snapshots()
            .iter()
            .map(AgentThreadSnapshotDto::from_snapshot)
            .collect()
    }

    /// Queue a bounded correction for exactly one running agent.
    pub fn steer(
        &self,
        id: u64,
        correction: &str,
    ) -> Result<AgentSteeringReceipt, AgentSteeringError> {
        let (receipt, update) = {
            let mut agents = self.agents.lock().expect("agent registry");
            let Some(record) = agents.get_mut(&id) else {
                return Err(AgentSteeringError::new(
                    id,
                    AgentSteeringErrorKind::UnknownAgent,
                    None,
                ));
            };
            if !matches!(record.status, AgentStatus::Running) {
                return Err(AgentSteeringError::new(
                    id,
                    AgentSteeringErrorKind::NotRunning,
                    None,
                ));
            }
            let mut receipt = record.thread.submit_steering(id, correction)?;
            // The restarted attempt begins from a fresh transcript, so every
            // millisecond the thread has already spent is discarded.
            receipt.discarded_ms = elapsed_millis(record.started_at, record.finished_at);
            (receipt, self.changed_snapshot(&agents))
        };
        self.publish(update);
        Ok(receipt)
    }

    /// Close one running agent's correction channel without stopping its work.
    pub fn close_steering(&self, id: u64) -> AgentSteeringCloseOutcome {
        let (outcome, update) = {
            let mut agents = self.agents.lock().expect("agent registry");
            let Some(record) = agents.get_mut(&id) else {
                return AgentSteeringCloseOutcome::NotFound;
            };
            if !matches!(record.status, AgentStatus::Running) {
                return AgentSteeringCloseOutcome::NotRunning;
            }
            if !record.thread.close_steering("closed by user request") {
                return AgentSteeringCloseOutcome::AlreadyClosed;
            }
            (
                AgentSteeringCloseOutcome::Closed,
                self.changed_snapshot(&agents),
            )
        };
        self.publish(update);
        outcome
    }

    fn mark_steering_applied(&self, id: u64, sequence: u64) {
        let update = {
            let mut agents = self.agents.lock().expect("agent registry");
            let Some(record) = agents.get_mut(&id) else {
                return;
            };
            if !record.thread.steering_applied(sequence) {
                return;
            }
            self.changed_snapshot(&agents)
        };
        self.publish(update);
    }

    /// Abort one running agent and join its task before publishing/returning
    /// the cancelled terminal status. Terminal records are returned unchanged.
    pub async fn cancel(&self, id: u64) -> AgentCancelOutcome {
        let existing = {
            let agents = self.agents.lock().expect("agent registry");
            let Some(record) = agents.get(&id) else {
                return AgentCancelOutcome::NotFound;
            };
            (!matches!(record.status, AgentStatus::Running))
                .then(|| terminal_status(id, record, true))
        };
        if let Some(existing) = existing {
            return AgentCancelOutcome::AlreadyTerminal(existing);
        }

        let mut cancelled = self
            .cancel_matching(|candidate| candidate == id, "cancelled by user request")
            .await;
        if let Some(status) = cancelled.pop() {
            AgentCancelOutcome::Cancelled(status)
        } else {
            // Natural completion can win between the first inspection and the
            // cancellation lock. Report the authoritative terminal state.
            let agents = self.agents.lock().expect("agent registry");
            match agents.get(&id) {
                Some(record) if !matches!(record.status, AgentStatus::Running) => {
                    AgentCancelOutcome::AlreadyTerminal(terminal_status(id, record, true))
                }
                Some(_) | None => AgentCancelOutcome::NotFound,
            }
        }
    }

    /// Codex-style spelling for [`AgentRegistry::cancel`].
    pub async fn stop(&self, id: u64) -> AgentCancelOutcome {
        self.cancel(id).await
    }

    /// Abort and join every running agent. Returned statuses are id-ordered.
    pub async fn cancel_all(&self) -> Vec<AgentTerminalStatus> {
        self.cancel_matching(|_| true, "cancelled by user request")
            .await
    }

    /// Abort and join every running agent, recording why. The reason is the
    /// text a user and a JSONL consumer both read back, so callers must pass
    /// the cause that actually applies rather than reusing another path's.
    pub async fn cancel_all_with_reason(&self, reason: &str) -> Vec<AgentTerminalStatus> {
        self.cancel_matching(|_| true, reason).await
    }

    /// Abort detached agents created by a parent turn that is being rolled
    /// back. Agents that predate the turn are intentionally left alone.
    pub async fn cancel_started_after(&self, watermark: u64) -> Vec<u64> {
        self.cancel_matching(
            |id| id > watermark,
            "cancelled because the parent turn was rolled back",
        )
        .await
        .into_iter()
        .map(|status| status.id)
        .collect()
    }

    async fn cancel_matching<F>(&self, mut predicate: F, reason: &str) -> Vec<AgentTerminalStatus>
    where
        F: FnMut(u64) -> bool,
    {
        let (mut statuses, handles, update) = {
            let mut agents = self.agents.lock().expect("agent registry");
            let mut statuses = Vec::new();
            let mut handles = Vec::new();
            for (id, record) in agents.iter_mut() {
                if predicate(*id) && matches!(record.status, AgentStatus::Running) {
                    record.status = AgentStatus::Cancelled(reason.to_string());
                    record
                        .thread
                        .finish(AgentThreadPhase::Cancelled, reason.len());
                    record.finished_at = Some(Instant::now());
                    let handle = record.handle.take();
                    statuses.push(terminal_status(*id, record, handle.is_some()));
                    if let Some(handle) = handle {
                        handles.push(handle);
                    }
                }
            }
            let update = (!statuses.is_empty()).then(|| self.changed_snapshot(&agents));
            (statuses, handles, update)
        };
        // Abort every task before awaiting any one of them, then join outside
        // the registry lock. No cancellation notification is published early.
        for handle in &handles {
            handle.abort();
        }
        for handle in handles {
            let _ = handle.await;
        }
        if let Some(update) = update {
            self.publish(update);
        }
        statuses.sort_by_key(|status| status.id);
        statuses
    }

    /// Remove one terminal record. Running work is never implicitly cancelled.
    pub fn prune(&self, id: u64) -> AgentPruneOutcome {
        let (outcome, update) = {
            let mut agents = self.agents.lock().expect("agent registry");
            let Some(record) = agents.get(&id) else {
                return AgentPruneOutcome::NotFound;
            };
            if matches!(record.status, AgentStatus::Running) {
                return AgentPruneOutcome::Running;
            }
            let record = agents.remove(&id).expect("inspected agent record");
            let outcome = AgentPruneOutcome::Pruned(terminal_status(id, &record, true));
            (outcome, self.changed_snapshot(&agents))
        };
        self.publish(update);
        outcome
    }

    /// Close a terminal thread and release its retained transcript/result.
    /// Running work must be stopped first; this is the user-facing spelling of
    /// the existing non-destructive [`AgentRegistry::prune`] contract.
    pub fn close(&self, id: u64) -> AgentPruneOutcome {
        self.prune(id)
    }

    /// Remove all terminal records, retaining every running agent. Returned
    /// statuses are id-ordered so callers can report exactly what was pruned.
    pub fn prune_terminal(&self) -> Vec<AgentTerminalStatus> {
        let (mut pruned, update) = {
            let mut agents = self.agents.lock().expect("agent registry");
            let ids = agents
                .iter()
                .filter_map(|(id, record)| {
                    (!matches!(record.status, AgentStatus::Running)).then_some(*id)
                })
                .collect::<Vec<_>>();
            let mut pruned = Vec::with_capacity(ids.len());
            for id in ids {
                let record = agents.remove(&id).expect("collected terminal record");
                pruned.push(terminal_status(id, &record, true));
            }
            let update = (!pruned.is_empty()).then(|| self.changed_snapshot(&agents));
            (pruned, update)
        };
        if let Some(update) = update {
            self.publish(update);
        }
        pruned.sort_by_key(|status| status.id);
        pruned
    }

    /// Count of still-running background agents (for a prompt hint).
    pub fn running(&self) -> usize {
        self.agents
            .lock()
            .expect("agent registry")
            .values()
            .filter(|r| matches!(r.status, AgentStatus::Running))
            .count()
    }

    /// Clear completed records at a session boundary. Callers first ensure no
    /// agents are running, so results from one conversation cannot be fetched
    /// by the next conversation through `agent_result`.
    pub fn reset(&self) {
        let update = {
            let mut agents = self.agents.lock().expect("agent registry");
            debug_assert!(!agents
                .values()
                .any(|record| matches!(record.status, AgentStatus::Running)));
            agents.clear();
            self.changed_snapshot(&agents)
        };
        self.next.store(0, Ordering::Relaxed);
        self.publish(update);
    }

    /// One line per agent (`#id  [status]  task`), id-ordered, for `/agents`.
    #[cfg(test)]
    pub fn list(&self) -> String {
        let g = self.agents.lock().expect("agent registry");
        if g.is_empty() {
            return "no background agents this session".to_string();
        }
        let mut ids: Vec<&u64> = g.keys().collect();
        ids.sort();
        ids.iter()
            .map(|id| {
                let rec = &g[id];
                let tag = match &rec.status {
                    AgentStatus::Running => "running",
                    AgentStatus::Done(_) => "done",
                    AgentStatus::Failed(_) => "failed",
                    AgentStatus::Cancelled(_) => "cancelled",
                };
                format!("#{id}  [{tag}]  {}", rec.task)
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The full result/status text for one agent, or `None` if unknown.
    /// Reading a terminal result acknowledges it for shutdown reporting;
    /// polling a running job does not.
    pub fn result(&self, id: u64) -> Option<String> {
        let (result, update) = {
            let mut agents = self.agents.lock().expect("agent registry");
            let record = agents.get_mut(&id)?;
            let result = result_text(id, &record.status);
            let acknowledged = !matches!(record.status, AgentStatus::Running)
                && record.thread.acknowledge_handoff();
            let update = acknowledged.then(|| self.changed_snapshot(&agents));
            (result, update)
        };
        if let Some(update) = update {
            self.publish(update);
        }
        Some(result)
    }

    /// Read status/result text without acknowledging a ready handoff.
    pub fn peek_result(&self, id: u64) -> Option<String> {
        let agents = self.agents.lock().expect("agent registry");
        agents
            .get(&id)
            .map(|record| result_text(id, &record.status))
    }

    /// Return the explicit handoff state without reading result content.
    pub fn handoff_state(&self, id: u64) -> Option<AgentHandoffState> {
        self.agents
            .lock()
            .expect("agent registry")
            .get(&id)
            .map(|record| record.thread.handoff())
    }

    /// Acknowledge a result handoff without copying its content.
    pub fn acknowledge_result(&self, id: u64) -> AgentHandoffAckOutcome {
        let update = {
            let mut agents = self.agents.lock().expect("agent registry");
            let Some(record) = agents.get_mut(&id) else {
                return AgentHandoffAckOutcome::NotFound;
            };
            match record.thread.handoff() {
                AgentHandoffState::Unavailable => return AgentHandoffAckOutcome::NotReady,
                AgentHandoffState::Acknowledged => {
                    return AgentHandoffAckOutcome::AlreadyAcknowledged;
                }
                AgentHandoffState::Ready => {
                    debug_assert!(record.thread.acknowledge_handoff());
                }
            }
            self.changed_snapshot(&agents)
        };
        self.publish(update);
        AgentHandoffAckOutcome::Acknowledged
    }

    /// Return every terminal result that no user/model surface has consumed,
    /// mark it observed, and keep the registry records available for later
    /// inspection. Interactive shutdown uses this to ensure completed detached
    /// work is printed before the process exits instead of silently vanishing.
    pub fn take_unobserved_terminal_results(&self) -> Vec<AgentTerminalStatus> {
        let (mut results, update) = {
            let mut agents = self.agents.lock().expect("agent registry");
            let results = agents
                .iter_mut()
                .filter_map(|(id, record)| {
                    if record.thread.handoff() != AgentHandoffState::Ready {
                        return None;
                    }
                    debug_assert!(record.thread.acknowledge_handoff());
                    Some(terminal_status(*id, record, true))
                })
                .collect::<Vec<_>>();
            let update = (!results.is_empty()).then(|| self.changed_snapshot(&agents));
            (results, update)
        };
        if let Some(update) = update {
            self.publish(update);
        }
        results.sort_by_key(|result| result.id);
        results
    }

    /// Build the next snapshot while holding `agents`, establishing one total
    /// order for generation numbers and their associated counts.
    fn changed_snapshot(&self, agents: &HashMap<u64, AgentRecord>) -> AgentRegistrySnapshot {
        let generation = self.generation.fetch_add(1, Ordering::Relaxed) + 1;
        let running = agents
            .values()
            .filter(|record| matches!(record.status, AgentStatus::Running))
            .count();
        let failed = agents
            .values()
            .filter(|record| matches!(record.status, AgentStatus::Failed(_)))
            .count();
        AgentRegistrySnapshot {
            generation,
            running,
            terminal: agents.len().saturating_sub(running),
            failed,
        }
    }

    /// Publish outside `agents` so a frontend can copy a watch snapshot and
    /// query result details without lock-order coupling. Concurrent publishers
    /// retain only the highest generation, preventing a delayed older mutation
    /// from replacing a newer snapshot.
    fn publish(&self, update: AgentRegistrySnapshot) {
        self.changes.send_if_modified(|current| {
            if current.generation >= update.generation {
                false
            } else {
                *current = update;
                true
            }
        });
    }
}

fn elapsed_millis(started: Instant, finished: Option<Instant>) -> u64 {
    let elapsed = finished
        .unwrap_or_else(Instant::now)
        .duration_since(started);
    u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
}

fn result_text(id: u64, status: &AgentStatus) -> String {
    match status {
        AgentStatus::Running => format!("agent #{id} is still running"),
        AgentStatus::Done(result) => result.clone(),
        AgentStatus::Failed(error) => format!("agent #{id} failed: {error}"),
        AgentStatus::Cancelled(reason) => format!("agent #{id} cancelled: {reason}"),
    }
}

fn agent_progress(id: u64, record: &AgentRecord) -> AgentProgress {
    let (status, result_bytes) = match &record.status {
        AgentStatus::Running => (AgentProgressStatus::Running, None),
        AgentStatus::Done(result) => (AgentProgressStatus::Done, Some(result.len())),
        AgentStatus::Failed(error) => (AgentProgressStatus::Failed, Some(error.len())),
        AgentStatus::Cancelled(reason) => (AgentProgressStatus::Cancelled, Some(reason.len())),
    };
    AgentProgress {
        id,
        task: record.task.clone(),
        profile: record.thread.profile().map(str::to_string),
        status,
        elapsed_ms: elapsed_millis(record.started_at, record.finished_at),
        result_bytes,
        telemetry: record.telemetry.clone(),
    }
}

fn terminal_status(id: u64, record: &AgentRecord, joined: bool) -> AgentTerminalStatus {
    debug_assert!(!matches!(record.status, AgentStatus::Running));
    AgentTerminalStatus {
        id,
        task: record.task.clone(),
        status: record.status.clone(),
        joined,
    }
}

/// Snapshot the current model + window into a runnable config (so a mid-session
/// `/model` applies to every subagent spawned afterwards).
fn resolve_cfg(template: &AgentConfig, live: &LiveModelWindow) -> AgentConfig {
    let mut cfg = template.clone();
    let (model, window) = live.lock().expect("live model/window").clone();
    cfg.model = model;
    cfg.context_window = window;
    cfg
}

fn classify_agent_error(error: &AgentError) -> AgentFailureClass {
    match error {
        AgentError::Provider(error) => match error {
            ProviderError::Connection(_) => AgentFailureClass::ProviderConnection,
            ProviderError::Timeout => AgentFailureClass::ProviderTimeout,
            ProviderError::InvalidResponse(_) => AgentFailureClass::ProviderInvalidResponse,
            ProviderError::ResponseTooLarge { .. } => AgentFailureClass::ProviderInvalidResponse,
            ProviderError::Unauthorized { .. } => AgentFailureClass::ProviderAuthentication,
            ProviderError::Http { status: 429, .. } => AgentFailureClass::ProviderRateLimited,
            ProviderError::Http { status, .. } if (400..500).contains(status) => {
                AgentFailureClass::ProviderClientError
            }
            ProviderError::Http { status, .. } if (500..600).contains(status) => {
                AgentFailureClass::ProviderServerError
            }
            ProviderError::Http { .. } => AgentFailureClass::ProviderHttp,
            ProviderError::ToolsNotSupported => AgentFailureClass::ProviderToolsUnsupported,
            ProviderError::Auth(_) => AgentFailureClass::ProviderAuthConfiguration,
        },
        AgentError::ToolCallDuringToolFreeFinalization => {
            AgentFailureClass::ProviderInvalidResponse
        }
        AgentError::Loop(_) => AgentFailureClass::AgentLoop,
        AgentError::ContextOverflow { .. } => AgentFailureClass::ContextOverflow,
        AgentError::ContextWindowUnknown => AgentFailureClass::ContextWindowUnknown,
    }
}

/// Render a subagent failure without copying provider-controlled diagnostics
/// into a durable job result or a parent-model tool result. Provider response
/// bodies, URLs, transport strings, auth hints, and loop detail can all contain
/// secrets; retain only typed categories and locally generated numeric fields.
fn safe_subagent_error(error: &AgentError) -> String {
    let detail = match error {
        AgentError::Provider(error) => match error {
            ProviderError::Connection(_) => "provider connection failed".to_string(),
            ProviderError::Timeout => "provider request timed out".to_string(),
            ProviderError::InvalidResponse(_) => {
                "provider returned an invalid response".to_string()
            }
            ProviderError::ResponseTooLarge { part, limit_bytes } => format!(
                "provider {part} exceeded the {limit_bytes}-byte safety limit"
            ),
            ProviderError::Unauthorized { status, .. } => {
                format!("provider authentication failed (HTTP {status})")
            }
            ProviderError::Http { status, .. } => {
                format!("provider request failed (HTTP {status})")
            }
            ProviderError::ToolsNotSupported => {
                "provider does not support required tool calls".to_string()
            }
            ProviderError::Auth(_) => "provider credentials are not configured".to_string(),
        },
        AgentError::ToolCallDuringToolFreeFinalization => {
            "model returned a tool call during tool-free structured finalization; no tool was executed"
                .to_string()
        }
        AgentError::Loop(_) => "agent loop failed".to_string(),
        AgentError::ContextOverflow {
            required,
            target,
            unit,
            ..
        } => format!(
            "context preflight rejected the request ({required} {unit} required; {target} {unit} available)"
        ),
        AgentError::ContextWindowUnknown => {
            "model context window is unknown; compaction could not run safely".to_string()
        }
    };
    format!("subagent failed: {detail}")
}

fn provider_category_label(category: ProviderAttemptCategory) -> &'static str {
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

fn provider_outcome_label(outcome: ProviderAttemptOutcome) -> &'static str {
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

/// Keep an untrusted provider-supplied tool name useful as a label without
/// allowing terminal controls, newlines, or unbounded text into `/jobs`.
fn safe_activity_name(name: &str) -> String {
    let mut safe = String::new();
    let mut previous_space = false;
    for character in name.chars().filter(|character| !character.is_control()) {
        let character = if character.is_whitespace() {
            ' '
        } else {
            character
        };
        if character == ' ' && previous_space {
            continue;
        }
        previous_space = character == ' ';
        if safe.chars().count() == 64 {
            break;
        }
        safe.push(character);
    }
    let safe = safe.trim();
    if safe.is_empty() {
        "unknown tool".to_string()
    } else if name
        .chars()
        .filter(|character| !character.is_control())
        .count()
        > 64
    {
        format!("{safe}…")
    } else {
        safe.to_string()
    }
}

fn summarize_events(
    model: String,
    events: Vec<AgentRuntimeEvent>,
    failure_class: Option<AgentFailureClass>,
) -> AgentTelemetry {
    let mut telemetry = AgentTelemetry::started(model);
    telemetry.failure_class = failure_class;
    for event in events {
        record_telemetry_event(&mut telemetry, event);
    }
    telemetry
}

fn record_telemetry_event(telemetry: &mut AgentTelemetry, event: AgentRuntimeEvent) {
    match event {
        AgentRuntimeEvent::ProviderRequest { round, .. } => {
            telemetry.record_activity(format!("provider request round {round}"));
        }
        AgentRuntimeEvent::StructuredFinalizationDeferred {
            repair_round,
            targets,
            ..
        } => telemetry.record_activity(format!(
            "structured finalization deferred to repair {} target{} in round {repair_round}",
            targets.len(),
            if targets.len() == 1 { "" } else { "s" }
        )),
        AgentRuntimeEvent::ProviderAttempt {
            attempt,
            max_attempts,
            category,
            outcome,
            ..
        } => telemetry.record_activity(format!(
            "provider attempt {attempt}/{max_attempts}: {}, {}",
            provider_category_label(category),
            provider_outcome_label(outcome)
        )),
        AgentRuntimeEvent::Usage {
            prompt_tokens,
            completion_tokens,
            total_tokens,
            ..
        } => {
            telemetry.prompt_tokens = telemetry.prompt_tokens.saturating_add(prompt_tokens);
            telemetry.completion_tokens = telemetry
                .completion_tokens
                .saturating_add(completion_tokens);
            telemetry.total_tokens = telemetry.total_tokens.saturating_add(total_tokens);
            telemetry.record_activity("usage updated".to_string());
        }
        AgentRuntimeEvent::AssistantDelta { .. } => {}
        AgentRuntimeEvent::ToolStarted { name, .. } => {
            telemetry.record_activity(format!("started tool {}", safe_activity_name(&name)))
        }
        AgentRuntimeEvent::ToolFinished {
            name,
            success,
            cancelled,
            ..
        } => {
            telemetry.tool_count = telemetry.tool_count.saturating_add(1);
            if success && matches!(name.as_str(), "fs_write_file" | "fs_edit_file") {
                telemetry.write_count = telemetry.write_count.saturating_add(1);
            }
            let outcome = if cancelled {
                "cancelled"
            } else if success {
                "finished"
            } else {
                "failed"
            };
            telemetry.record_activity(format!("tool {} {outcome}", safe_activity_name(&name)));
        }
        AgentRuntimeEvent::TurnFinished {
            success,
            tool_errors,
            ..
        } => {
            let outcome = if success { "completed" } else { "failed" };
            telemetry.record_activity(format!(
                "turn {outcome}; {tool_errors} tool error{}",
                if tool_errors == 1 { "" } else { "s" }
            ));
        }
    }
}

/// Run one subagent and retain a body-free event summary alongside its result.
async fn run_subagent_report<P: Provider>(
    provider: &Arc<P>,
    sub_tools: &Arc<Registry>,
    cfg: &AgentConfig,
    extra_system: &str,
    task: String,
) -> SubagentRunReport {
    run_subagent_report_with_progress(provider, sub_tools, cfg, extra_system, task, None, None)
        .await
}

async fn run_subagent_report_with_progress<P: Provider>(
    provider: &Arc<P>,
    sub_tools: &Arc<Registry>,
    cfg: &AgentConfig,
    extra_system: &str,
    task: String,
    progress: Option<(Arc<AgentRegistry>, u64)>,
    mut thread_runtime: Option<AgentThreadRuntime>,
) -> SubagentRunReport {
    let bound_registry_result =
        progress.is_some() && cfg.artifact_policy == ArtifactStoragePolicy::InlineOnly;
    let mut system = SUBAGENT_SYSTEM.to_string();
    if !extra_system.is_empty() {
        system.push_str("\n\n");
        system.push_str(extra_system);
    }
    let event_log = match &progress {
        Some((registry, id)) => {
            let registry = registry.clone();
            let id = *id;
            AgentEventLog::with_observer(move |event| {
                registry.record_progress_event(id, event.clone());
            })
        }
        None => AgentEventLog::default(),
    };
    let mut instrumented_cfg = cfg.clone();
    instrumented_cfg.event_log = Some(event_log.clone());
    let mut latest_correction = None;
    let outcome = loop {
        let mut messages = vec![
            ChatMessage::system(system.clone()),
            ChatMessage::user(task.clone()),
        ];
        if let Some(correction) = &latest_correction {
            messages.push(ChatMessage::user(format!(
                "Updated direction from the parent while you were working:\n{correction}"
            )));
        }

        let turn = run_agent_turn(provider, sub_tools, &mut messages, &instrumented_cfg);
        tokio::pin!(turn);
        let steering = match thread_runtime.as_mut() {
            Some(runtime) => tokio::select! {
                biased;
                outcome = &mut turn => break outcome,
                correction = runtime.recv_steering() => correction,
            },
            None => break turn.await,
        };
        match steering {
            Some(message) => {
                if let Some((registry, id)) = &progress {
                    registry.mark_steering_applied(*id, message.sequence);
                }
                latest_correction = Some(message.correction);
                // Dropping `turn` stops the in-flight provider/tool future. The
                // next attempt uses a fresh transcript plus the latest bounded
                // correction; already-completed filesystem effects remain.
            }
            None => {
                // The UI explicitly closed steering. Let this attempt finish
                // without polling a permanently closed receiver.
                break turn.await;
            }
        }
    };
    let failure_class = outcome.as_ref().err().map(classify_agent_error);
    let telemetry = summarize_events(cfg.model.clone(), event_log.take(), failure_class);
    let result = match outcome {
        Ok(text) if !text.trim().is_empty() => Ok(if bound_registry_result {
            bound_inline_output(text).content
        } else {
            text
        }),
        Ok(_) => Ok("(subagent returned no result)".to_string()),
        Err(error) => Err(safe_subagent_error(&error)),
    };
    SubagentRunReport { result, telemetry }
}

/// Run one subagent to completion in its own fresh context and return its final
/// text (or an error string). Shared by `spawn_agent` and `spawn_agents`.
async fn run_subagent<P: Provider>(
    provider: &Arc<P>,
    sub_tools: &Arc<Registry>,
    cfg: &AgentConfig,
    extra_system: &str,
    task: String,
) -> Result<String, String> {
    run_subagent_report(provider, sub_tools, cfg, extra_system, task)
        .await
        .result
}

/// A short one-line label for a task, for the collected `spawn_agents` output.
fn task_preview(task: &str) -> String {
    let safe_task = thread::sanitize_and_redact_text(task);
    let first = safe_task.lines().next().unwrap_or("").trim();
    let short: String = first.chars().take(60).collect();
    if first.chars().count() > 60 {
        format!("{short}…")
    } else {
        short
    }
}

/// Build the `spawn_agent` tool. `sub_tools` is the subagent's registry (base
/// tools only — it must NOT contain `spawn_agent`). `sub_cfg` is the subagent's
/// non-streaming agent config *template*; its `model` and `context_window` are
/// overwritten at call time from `live` so subagents track `/model`. `extra_system`
/// seeds inherited context (e.g. a skill index) into the subagent's system prompt.
pub fn spawn_agent_tool<P: Provider + 'static>(
    provider: Arc<P>,
    sub_tools: Arc<Registry>,
    sub_cfg: AgentConfig,
    live: LiveModelWindow,
    extra_system: String,
    budget: Arc<SpawnBudget>,
) -> (ToolDescriptor, HandlerFn) {
    let descriptor = ToolDescriptor {
        name: "spawn_agent".to_string(),
        description: "Delegate a self-contained sub-task to a fresh subagent that has its own \
                      isolated context and the same file tools and skills. Use it to keep a large \
                      exploration (reading many files, research) out of the main conversation — \
                      only the subagent's result comes back. Give a complete, standalone task."
            .to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "task": {"type": "string", "description": "A complete, self-contained task for the subagent"}
            },
            "required": ["task"]
        }),
    };
    let handler: HandlerFn = Box::new(move |args: String| {
        let provider = provider.clone();
        let sub_tools = sub_tools.clone();
        let sub_cfg = sub_cfg.clone();
        let live = live.clone();
        let extra_system = extra_system.clone();
        let budget = budget.clone();
        Box::pin(async move {
            let a: SpawnArgs =
                serde_json::from_str(&args).map_err(|e| ToolError::InvalidArgs(e.to_string()))?;
            if budget.admit(1) == 0 {
                return Err(ToolError::ExecError(spawn_cap_error(&budget)));
            }
            let cfg = resolve_cfg(&sub_cfg, &live);
            // Fresh state each call; the isolated loop runs to completion and only
            // its final answer is returned (child rounds never enter the parent).
            run_subagent(&provider, &sub_tools, &cfg, &extra_system, a.task)
                .await
                .map_err(ToolError::ExecError)
        })
    });
    (descriptor, handler)
}

/// Build the `spawn_agents` tool: run SEVERAL isolated subagents concurrently and
/// return their collected results (Stage B). Each subagent has its own fresh
/// context, so they run in parallel safely; concurrency is capped at
/// [`MAX_PARALLEL`]. The subagent registry must NOT contain `spawn_agent` /
/// `spawn_agents` (depth-1). One failing subagent does not fail the others.
pub fn spawn_agents_tool<P: Provider + 'static>(
    provider: Arc<P>,
    sub_tools: Arc<Registry>,
    sub_cfg: AgentConfig,
    live: LiveModelWindow,
    extra_system: String,
    budget: Arc<SpawnBudget>,
) -> (ToolDescriptor, HandlerFn) {
    let descriptor = ToolDescriptor {
        name: "spawn_agents".to_string(),
        description:
            "Delegate SEVERAL independent sub-tasks at once, each to its own isolated \
                      subagent, run in parallel. Returns all results labelled by task. Use it to \
                      fan out independent work (e.g. review several files) — only the results come \
                      back, not each subagent's steps. Pass an array of complete, standalone tasks."
                .to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "tasks": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Independent, self-contained tasks (each runs in its own subagent)"
                }
            },
            "required": ["tasks"]
        }),
    };
    let handler: HandlerFn = Box::new(move |args: String| {
        let provider = provider.clone();
        let sub_tools = sub_tools.clone();
        let sub_cfg = sub_cfg.clone();
        let live = live.clone();
        let extra_system = extra_system.clone();
        let budget = budget.clone();
        Box::pin(async move {
            let a: SpawnManyArgs =
                serde_json::from_str(&args).map_err(|e| ToolError::InvalidArgs(e.to_string()))?;
            if a.tasks.is_empty() {
                return Err(ToolError::InvalidArgs(
                    "tasks must be non-empty".to_string(),
                ));
            }
            // Per-turn spawn budget: admit what fits, refuse the rest. The
            // refused tasks still get labelled entries in the collected output
            // so the model sees exactly which work it must now do inline.
            let admitted = budget.admit(a.tasks.len() as u32) as usize;
            if admitted == 0 {
                return Err(ToolError::ExecError(spawn_cap_error(&budget)));
            }
            let refused: Vec<(usize, String)> = a
                .tasks
                .iter()
                .enumerate()
                .skip(admitted)
                .map(|(i, task)| (i, task_preview(task)))
                .collect();
            let cap_note = (!refused.is_empty()).then(|| spawn_cap_error(&budget));
            let tasks: Vec<String> = a.tasks.into_iter().take(admitted).collect();
            // Same model/window snapshot for the whole batch.
            let cfg = resolve_cfg(&sub_cfg, &live);
            // Run each task in its own subagent, at most MAX_PARALLEL at a time.
            let stream = futures::stream::iter(tasks.into_iter().enumerate().map(|(i, task)| {
                let provider = provider.clone();
                let sub_tools = sub_tools.clone();
                let cfg = cfg.clone();
                let extra_system = extra_system.clone();
                async move {
                    let preview = task_preview(&task);
                    let r = run_subagent(&provider, &sub_tools, &cfg, &extra_system, task).await;
                    (i, preview, r)
                }
            }));
            let mut results: Vec<(usize, String, Result<String, String>)> = stream
                .buffer_unordered(MAX_PARALLEL)
                .collect::<Vec<_>>()
                .await;
            results.sort_by_key(|(i, _, _)| *i); // stable, task-order output
            let mut out = String::new();
            for (i, preview, r) in results {
                out.push_str(&format!("=== subagent {} — {} ===\n", i + 1, preview));
                match r {
                    Ok(text) => out.push_str(text.trim()),
                    Err(e) => out.push_str(&format!("[{e}]")),
                }
                out.push_str("\n\n");
            }
            for (i, preview) in refused {
                out.push_str(&format!(
                    "=== subagent {} — {} ===\n[not started: {}]\n\n",
                    i + 1,
                    preview,
                    cap_note.as_deref().unwrap_or("agent spawn cap reached")
                ));
            }
            Ok(out.trim().to_string())
        })
    });
    (descriptor, handler)
}

/// Build the `spawn_agent_bg` tool: start a subagent that runs DETACHED in the
/// background and return its id immediately (Stage C). The subagent runs quiet
/// (no terminal narration) so it doesn't corrupt the interactive prompt; its
/// result is stored in `registry` and fetched later via `agent_result`/`/agents`.
/// `sub_cfg` must already be quiet. Depth-1: the subagent's registry has no spawn
/// tools.
#[derive(Clone)]
pub struct BackgroundProfileRuntime {
    catalog: Arc<profile::AgentProfileCatalog>,
    tools: Arc<HashMap<String, Arc<Registry>>>,
}

impl BackgroundProfileRuntime {
    pub fn new(
        catalog: Arc<profile::AgentProfileCatalog>,
        tools: Arc<HashMap<String, Arc<Registry>>>,
    ) -> Self {
        Self { catalog, tools }
    }
}

#[allow(clippy::too_many_arguments)]
pub fn spawn_agent_bg_tool<P: Provider + 'static>(
    provider: Arc<P>,
    sub_tools: Arc<Registry>,
    sub_cfg: AgentConfig,
    live: LiveModelWindow,
    extra_system: String,
    registry: Arc<AgentRegistry>,
    profile_runtime: BackgroundProfileRuntime,
    budget: Arc<SpawnBudget>,
) -> (ToolDescriptor, HandlerFn) {
    let descriptor = ToolDescriptor {
        name: "spawn_agent_bg".to_string(),
        description: "Start a subagent that runs in the BACKGROUND on a self-contained task and \
                      return its id immediately (it does not block). Use it for long work the user \
                      can keep chatting through; fetch the result later with agent_result(id), or \
                      the user can run /agents. Give a complete, standalone task."
            .to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "task": {"type": "string", "description": "A complete, self-contained task for the background subagent"},
                "profile": {"type": "string", "description": "Optional validated specialist profile name; available built-ins are reviewer and reasoning"}
            },
            "required": ["task"]
        }),
    };
    let handler: HandlerFn = Box::new(move |args: String| {
        let provider = provider.clone();
        let sub_tools = sub_tools.clone();
        let sub_cfg = sub_cfg.clone();
        let live = live.clone();
        let extra_system = extra_system.clone();
        let registry = registry.clone();
        let profile_runtime = profile_runtime.clone();
        let budget = budget.clone();
        Box::pin(async move {
            let a: SpawnArgs =
                serde_json::from_str(&args).map_err(|e| ToolError::InvalidArgs(e.to_string()))?;
            if budget.admit(1) == 0 {
                return Err(ToolError::ExecError(spawn_cap_error(&budget)));
            }
            let selected_profile = match a.profile.as_deref() {
                Some(name) => {
                    Some(profile_runtime.catalog.get(name).cloned().ok_or_else(|| {
                        ToolError::InvalidArgs(format!(
                            "unknown background-agent profile '{name}'; use /profiles"
                        ))
                    })?)
                }
                None => None,
            };
            let mut cfg = resolve_cfg(&sub_cfg, &live);
            let effective_tools = selected_profile
                .as_ref()
                .and_then(|profile| profile_runtime.tools.get(&profile.name))
                .cloned()
                .unwrap_or_else(|| sub_tools.clone());
            let mut effective_system = extra_system.clone();
            if let Some(profile) = selected_profile.as_ref() {
                if let Some(profile_model) = profile.model.as_ref() {
                    cfg.model = profile_model.clone();
                }
                // Forward-only-if-explicitly-requested, exactly like the parent
                // `--reasoning-effort` flag. A profile that says nothing keeps
                // the parent's omission, so gateways that reject the field are
                // not broken by merely selecting a profile.
                if let Some(effort) = profile.reasoning_effort.requested() {
                    cfg.reasoning_effort = Some(effort);
                }
                if !profile.instructions.trim().is_empty() {
                    if !effective_system.is_empty() {
                        effective_system.push_str("\n\n");
                    }
                    effective_system.push_str(&profile.instructions);
                }
                debug_assert!(profile.tool_policy.accepts_registry(&effective_tools));
            }
            let model = cfg.model.clone();
            let preview = task_preview(&a.task);
            let profile_name = selected_profile
                .as_ref()
                .map(|profile| profile.name.clone());
            // A profile is the only thing that can add a request field the user
            // never asked for, so a 4xx from a profile spawn gets the one hint
            // that explains it. Built without borrowing the profile so the
            // detached future stays 'static.
            let rejected_field_hint = selected_profile.as_ref().and_then(|profile| {
                profile.reasoning_effort.requested().map(|_| {
                    format!(
                        "profile '{}' requested reasoning_effort={} — this gateway rejected that \
                         request field; see /profiles or spawn without a profile",
                        profile.name,
                        profile.reasoning_effort.label()
                    )
                })
            });
            // Admission and task-handle attachment are atomic with respect to
            // cancel-all/session teardown; no externally visible running record
            // can exist without a joinable handle.
            let id = registry
                .spawn_background_thread(
                    preview,
                    a.task.clone(),
                    model,
                    profile_name,
                    move |id, running_registry, runtime| async move {
                        let mut report = kitsu_tools::scope_without_interactive_prompts(
                            run_subagent_report_with_progress(
                                &provider,
                                &effective_tools,
                                &cfg,
                                &effective_system,
                                a.task,
                                Some((running_registry, id)),
                                Some(runtime),
                            ),
                        )
                        .await;
                        if report.telemetry.failure_class
                            == Some(AgentFailureClass::ProviderClientError)
                        {
                            if let (Err(error), Some(hint)) =
                                (&mut report.result, rejected_field_hint.as_ref())
                            {
                                error.push_str("; ");
                                error.push_str(hint);
                            }
                        }
                        report
                    },
                )
                .map_err(|error| ToolError::ExecError(error.to_string()))?;
            Ok(format!(
                "Started background agent #{id}. It runs detached — fetch its result later with \
                 agent_result({id}); the user can also run /agents to see status."
            ))
        })
    });
    (descriptor, handler)
}

/// Build the `agent_result` tool: fetch a background agent's status or result by
/// id (returns "still running" until it finishes).
pub fn agent_result_tool(registry: Arc<AgentRegistry>) -> (ToolDescriptor, HandlerFn) {
    let descriptor = ToolDescriptor {
        name: "agent_result".to_string(),
        description: "Fetch the status or result of a background subagent started with \
                      spawn_agent_bg, by its id. Returns 'still running' until it finishes."
            .to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "id": {"type": "integer", "description": "The background agent id"}
            },
            "required": ["id"]
        }),
    };
    let handler: HandlerFn = Box::new(move |args: String| {
        let registry = registry.clone();
        Box::pin(async move {
            let a: ResultArgs =
                serde_json::from_str(&args).map_err(|e| ToolError::InvalidArgs(e.to_string()))?;
            registry
                .result(a.id)
                .ok_or_else(|| ToolError::ExecError(format!("no background agent #{}", a.id)))
        })
    });
    (descriptor, handler)
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::stream;
    use kitsu_providers::{
        ChatDelta, ChatRequest, ChatStream, FinishReason, ProviderError, Role, Usage,
    };
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
    use std::sync::Mutex;

    /// A provider that echoes each request's task (last user message) back as the
    /// answer, so a batch of subagents produces per-task-distinguishable results.
    struct EchoProvider;
    impl Provider for EchoProvider {
        async fn healthcheck(&self) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn chat_stream(&self, req: ChatRequest) -> Result<ChatStream, ProviderError> {
            let task = req
                .messages
                .iter()
                .rev()
                .find(|m| matches!(m.role, Role::User))
                .map(|m| m.content.clone())
                .unwrap_or_default();
            Ok(Box::pin(stream::iter(vec![
                Ok(ChatDelta::Text(format!("echo:{task}"))),
                Ok(ChatDelta::Usage(Usage {
                    prompt_tokens: 11,
                    completion_tokens: 7,
                    total_tokens: 18,
                })),
                Ok(ChatDelta::Done(FinishReason::Stop)),
            ])))
        }
        async fn list_models(&self) -> Result<Option<Vec<String>>, ProviderError> {
            Ok(None)
        }
    }

    /// A provider that returns one scripted round per `chat_stream` call and
    /// records the model of the last request it saw.
    struct MockProvider {
        rounds: Mutex<Vec<Vec<ChatDelta>>>,
        last_model: Mutex<Option<String>>,
    }

    /// The first request never completes. A steering correction must cancel
    /// that attempt and restart with the correction as the latest user message.
    struct SteeringProvider {
        calls: AtomicUsize,
    }

    impl Provider for SteeringProvider {
        async fn healthcheck(&self) -> Result<(), ProviderError> {
            Ok(())
        }

        async fn chat_stream(&self, req: ChatRequest) -> Result<ChatStream, ProviderError> {
            if self.calls.fetch_add(1, AtomicOrdering::SeqCst) == 0 {
                return std::future::pending::<Result<ChatStream, ProviderError>>().await;
            }
            let latest_user = req
                .messages
                .iter()
                .rev()
                .find(|message| matches!(message.role, Role::User))
                .map(|message| message.content.clone())
                .unwrap_or_default();
            Ok(Box::pin(stream::iter(vec![
                Ok(ChatDelta::Text(format!("followed:{latest_user}"))),
                Ok(ChatDelta::Done(FinishReason::Stop)),
            ])))
        }

        async fn list_models(&self) -> Result<Option<Vec<String>>, ProviderError> {
            Ok(None)
        }
    }
    impl Provider for MockProvider {
        async fn healthcheck(&self) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn chat_stream(&self, req: ChatRequest) -> Result<ChatStream, ProviderError> {
            *self.last_model.lock().unwrap() = Some(req.model.clone());
            let mut r = self.rounds.lock().unwrap();
            let script = if r.is_empty() {
                Vec::new()
            } else {
                r.remove(0)
            };
            Ok(Box::pin(stream::iter(
                script.into_iter().map(Ok::<_, ProviderError>),
            )))
        }
        async fn list_models(&self) -> Result<Option<Vec<String>>, ProviderError> {
            Ok(None)
        }
    }

    fn sub_cfg() -> AgentConfig {
        AgentConfig {
            model: "m".to_string(),
            stream: false,
            ..Default::default()
        }
    }

    fn open_budget() -> Arc<SpawnBudget> {
        Arc::new(SpawnBudget::new(u32::MAX))
    }

    fn empty_profile_runtime() -> BackgroundProfileRuntime {
        BackgroundProfileRuntime::new(
            Arc::new(profile::AgentProfileCatalog::new()),
            Arc::new(HashMap::new()),
        )
    }

    #[tokio::test]
    async fn spawn_agent_runs_nested_loop_and_returns_only_the_child_answer() {
        // The child's single round returns text; that text is the tool result.
        let provider = Arc::new(MockProvider {
            rounds: Mutex::new(vec![vec![
                ChatDelta::Text("SUBAGENT_RESULT_42".to_string()),
                ChatDelta::Done(FinishReason::Stop),
            ]]),
            last_model: Mutex::new(None),
        });
        let live: LiveModelWindow = Arc::new(Mutex::new(("m".to_string(), None)));
        let (desc, handler) = spawn_agent_tool(
            provider,
            Arc::new(Registry::new()),
            sub_cfg(),
            live,
            String::new(),
            open_budget(),
        );
        assert_eq!(desc.name, "spawn_agent");
        let out = handler(r#"{"task":"do the thing"}"#.to_string())
            .await
            .expect("subagent should succeed");
        assert!(
            out.contains("SUBAGENT_RESULT_42"),
            "returns the child's answer"
        );
        // Isolation: the returned value is exactly the child's conclusion, not a
        // transcript of its rounds.
        assert!(
            !out.contains("do the thing"),
            "no parent/child transcript leaks"
        );
    }

    #[tokio::test]
    async fn spawn_agent_uses_live_model_at_call_time() {
        // Regression for the frozen-config defect: a `/model` switch after the
        // tool is built must reach the subagent.
        let provider = Arc::new(MockProvider {
            rounds: Mutex::new(vec![vec![
                ChatDelta::Text("ok".to_string()),
                ChatDelta::Done(FinishReason::Stop),
            ]]),
            last_model: Mutex::new(None),
        });
        let live: LiveModelWindow = Arc::new(Mutex::new(("model-A".to_string(), Some(128_000))));
        let (_d, handler) = spawn_agent_tool(
            provider.clone(),
            Arc::new(Registry::new()),
            sub_cfg(),
            live.clone(),
            String::new(),
            open_budget(),
        );
        // Switch the model as `/model` would, AFTER the tool was built.
        *live.lock().unwrap() = ("model-B".to_string(), Some(8_000));
        let _ = handler(r#"{"task":"x"}"#.to_string()).await.unwrap();
        assert_eq!(
            provider.last_model.lock().unwrap().as_deref(),
            Some("model-B"),
            "subagent used the live model, not the frozen initial one"
        );
    }

    #[tokio::test]
    async fn spawn_agents_runs_all_tasks_and_labels_results_in_order() {
        let live: LiveModelWindow = Arc::new(Mutex::new(("m".to_string(), None)));
        let (desc, handler) = spawn_agents_tool(
            Arc::new(EchoProvider),
            Arc::new(Registry::new()),
            sub_cfg(),
            live,
            String::new(),
            open_budget(),
        );
        assert_eq!(desc.name, "spawn_agents");
        let out = handler(r#"{"tasks":["alpha","beta","gamma"]}"#.to_string())
            .await
            .expect("batch should succeed");
        // Every task ran and its own result came back.
        assert!(out.contains("echo:alpha"), "alpha ran");
        assert!(out.contains("echo:beta"), "beta ran");
        assert!(out.contains("echo:gamma"), "gamma ran");
        // Results are labelled and emitted in task order despite concurrent run.
        assert!(out.contains("subagent 1 — alpha"));
        assert!(out.contains("subagent 3 — gamma"));
        assert!(
            out.find("subagent 1").unwrap() < out.find("subagent 3").unwrap(),
            "stable task-order output"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn spawn_agent_bg_registers_runs_detached_and_stores_result() {
        use std::time::Duration;
        let registry = AgentRegistry::new();
        let mut changes = registry.subscribe();
        let live: LiveModelWindow = Arc::new(Mutex::new(("m".to_string(), None)));
        let mut cfg = sub_cfg();
        cfg.quiet = true;
        let profile_runtime = empty_profile_runtime();
        let (desc, handler) = spawn_agent_bg_tool(
            Arc::new(EchoProvider),
            Arc::new(Registry::new()),
            cfg,
            live,
            String::new(),
            registry.clone(),
            profile_runtime,
            open_budget(),
        );
        assert_eq!(desc.name, "spawn_agent_bg");
        // Returns immediately with an id (does not block on the subagent).
        let out = handler(r#"{"task":"do work"}"#.to_string())
            .await
            .expect("spawn should return an id");
        assert!(out.contains("#1"), "returned the agent id: {out}");

        // Await registry changes rather than polling. If start and finish both
        // happened before this future runs, watch still contains the latest
        // terminal snapshot.
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let snapshot = *changes.borrow_and_update();
                if snapshot.terminal == 1 {
                    break;
                }
                changes.changed().await.expect("registry still exists");
            }
        })
        .await
        .expect("background agent should finish");
        let r = registry.result(1).expect("agent #1 exists");
        assert!(r.contains("echo:do work"), "stored result: {r}");
        assert_eq!(registry.running(), 0, "no agents still running");
        let progress = registry.progress_snapshot();
        let telemetry = &progress.agents[0].telemetry;
        assert_eq!(telemetry.model, "m");
        assert_eq!(telemetry.prompt_tokens, 11);
        assert_eq!(telemetry.completion_tokens, 7);
        assert_eq!(telemetry.total_tokens, 18);
        assert_eq!(telemetry.tool_count, 0);
        assert_eq!(telemetry.write_count, 0);
        assert_eq!(telemetry.failure_class, None);
        assert_eq!(
            telemetry.last_activity.as_deref(),
            Some("turn completed; 0 tool errors")
        );

        // agent_result tool fetches it by id; unknown id errors.
        let (_d, rh) = agent_result_tool(registry.clone());
        let got = rh(r#"{"id":1}"#.to_string()).await.unwrap();
        assert!(got.contains("echo:do work"));
        assert!(rh(r#"{"id":999}"#.to_string()).await.is_err());
        // /agents-style listing shows the finished agent.
        assert!(registry.list().contains("#1"));
        assert!(registry.list().contains("[done]"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn background_steering_restarts_only_that_thread_and_handoff_is_explicit() {
        use std::time::Duration;

        let registry = AgentRegistry::new();
        let provider = Arc::new(SteeringProvider {
            calls: AtomicUsize::new(0),
        });
        let live: LiveModelWindow = Arc::new(Mutex::new(("m".to_string(), None)));
        let mut cfg = sub_cfg();
        cfg.quiet = true;
        let profile_runtime = empty_profile_runtime();
        let (_descriptor, handler) = spawn_agent_bg_tool(
            provider.clone(),
            Arc::new(Registry::new()),
            cfg,
            live,
            String::new(),
            registry.clone(),
            profile_runtime,
            open_budget(),
        );
        handler(
            r#"{"task":"review parser; token=hf_abcdefghijklmnopqrstuvwxyz123456"}"#.to_string(),
        )
        .await
        .unwrap();

        let receipt = registry
            .steer(1, "focus on UTF-8 boundaries")
            .expect("running thread accepts a targeted correction");
        assert_eq!(receipt.id, 1);
        assert_eq!(receipt.sequence, 1);

        tokio::time::timeout(Duration::from_secs(1), async {
            while registry.progress_snapshot().running != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("steered turn should restart and finish");
        assert_eq!(provider.calls.load(AtomicOrdering::SeqCst), 2);

        let result = registry.peek_result(1).unwrap();
        assert!(result.contains("focus on UTF-8 boundaries"));
        assert_eq!(registry.handoff_state(1), Some(AgentHandoffState::Ready));
        let snapshot = registry.thread_snapshot(1).unwrap();
        assert_eq!(snapshot.phase, AgentThreadPhase::Done);
        assert_eq!(snapshot.handoff, AgentHandoffState::Ready);
        assert!(!snapshot.steering_open);
        let persisted =
            serde_json::to_string(&AgentThreadSnapshotDto::from_snapshot(&snapshot)).unwrap();
        assert!(!persisted.contains("hf_abcdefghijklmnopqrstuvwxyz123456"));
        assert!(snapshot.timeline.entries.iter().any(|entry| {
            entry.kind == thread::AgentTimelineKind::Lifecycle
                && entry.text.contains("correction #1 applied")
        }));
        assert_eq!(
            registry.acknowledge_result(1),
            AgentHandoffAckOutcome::Acknowledged
        );
        assert_eq!(
            registry.acknowledge_result(1),
            AgentHandoffAckOutcome::AlreadyAcknowledged
        );
        assert!(registry.take_unobserved_terminal_results().is_empty());
    }

    #[test]
    fn event_summary_counts_usage_and_tools_without_copying_event_bodies() {
        let telemetry = summarize_events(
            "safe-model".to_string(),
            vec![
                AgentRuntimeEvent::Usage {
                    round: 1,
                    purpose: kitsu_core::ProviderRequestPurpose::Inference,
                    prompt_tokens: 9,
                    completion_tokens: 4,
                    total_tokens: 13,
                },
                AgentRuntimeEvent::Usage {
                    round: 2,
                    purpose: kitsu_core::ProviderRequestPurpose::Inference,
                    prompt_tokens: 3,
                    completion_tokens: 2,
                    total_tokens: 5,
                },
                AgentRuntimeEvent::ToolStarted {
                    round: 1,
                    call_id: "call-secret".to_string(),
                    name: "fs_write_file\n\u{1b}[31m".to_string(),
                    args: "SECRET_ARGS".to_string(),
                    command: None,
                },
                AgentRuntimeEvent::ToolFinished {
                    round: 1,
                    call_id: "call-secret".to_string(),
                    name: "fs_write_file".to_string(),
                    success: true,
                    cancelled: false,
                    output: "SECRET_OUTPUT".to_string(),
                    output_bytes: 13,
                    output_retained_bytes: 13,
                    output_truncated: false,
                    artifact_status: kitsu_core::ToolArtifactStatus::NotNeeded,
                    command: None,
                },
                AgentRuntimeEvent::ToolFinished {
                    round: 2,
                    call_id: "call-2".to_string(),
                    name: "fs_edit_file".to_string(),
                    success: false,
                    cancelled: false,
                    output: "ANOTHER_SECRET".to_string(),
                    output_bytes: 14,
                    output_retained_bytes: 14,
                    output_truncated: false,
                    artifact_status: kitsu_core::ToolArtifactStatus::NotNeeded,
                    command: None,
                },
                AgentRuntimeEvent::TurnFinished {
                    success: true,
                    completion: "SECRET_COMPLETION".to_string(),
                    tool_errors: 1,
                    policy_errors: 0,
                },
            ],
            None,
        );

        assert_eq!(telemetry.model, "safe-model");
        assert_eq!(telemetry.prompt_tokens, 12);
        assert_eq!(telemetry.completion_tokens, 6);
        assert_eq!(telemetry.total_tokens, 18);
        assert_eq!(telemetry.tool_count, 2);
        assert_eq!(telemetry.write_count, 1, "only successful writes count");
        let activity = telemetry.recent_activity.join("\n");
        assert!(!activity.contains("SECRET"));
        assert!(!activity.contains("call-secret"));
        assert!(telemetry.recent_activity.iter().all(|item| !item
            .chars()
            .any(|character| matches!(character, '\n' | '\r' | '\u{1b}'))));
        assert_eq!(
            telemetry.last_activity.as_deref(),
            Some("turn completed; 1 tool error")
        );
    }

    #[test]
    fn provider_failure_classification_never_retains_the_response_body() {
        let error = AgentError::Provider(ProviderError::Http {
            status: 429,
            body: "SECRET_PROVIDER_BODY".to_string(),
        });
        let failure_class = classify_agent_error(&error);
        assert_eq!(failure_class, AgentFailureClass::ProviderRateLimited);
        assert_eq!(failure_class.to_string(), "provider_rate_limited");
        assert!(!failure_class.to_string().contains("SECRET"));
    }

    #[test]
    fn subagent_failure_text_never_retains_provider_or_loop_detail() {
        let unsafe_detail = "TOKEN_SECRET https://user:pass@example.invalid/private";
        let cases = [
            AgentError::Provider(ProviderError::Connection(unsafe_detail.to_string())),
            AgentError::Provider(ProviderError::InvalidResponse(unsafe_detail.to_string())),
            AgentError::Provider(ProviderError::Unauthorized {
                status: 401,
                hint: Some(unsafe_detail.to_string()),
            }),
            AgentError::Provider(ProviderError::Http {
                status: 503,
                body: unsafe_detail.to_string(),
            }),
            AgentError::Loop(unsafe_detail.to_string()),
        ];

        for error in cases {
            let rendered = safe_subagent_error(&error);
            assert!(rendered.starts_with("subagent failed:"));
            assert!(!rendered.contains("TOKEN_SECRET"));
            assert!(!rendered.contains("example.invalid"));
            assert!(!rendered.contains("user:pass"));
        }
        assert!(
            safe_subagent_error(&AgentError::Provider(ProviderError::Http {
                status: 503,
                body: unsafe_detail.to_string(),
            }))
            .contains("HTTP 503")
        );
    }

    #[tokio::test]
    async fn spawn_agent_bg_surfaces_registry_capacity_without_starting_work() {
        let registry = AgentRegistry::with_limits(AgentRegistryLimits {
            max_running: 0,
            max_records: 0,
        });
        let live: LiveModelWindow = Arc::new(Mutex::new(("m".to_string(), None)));
        let mut cfg = sub_cfg();
        cfg.quiet = true;
        let profile_runtime = empty_profile_runtime();
        let (_descriptor, handler) = spawn_agent_bg_tool(
            Arc::new(EchoProvider),
            Arc::new(Registry::new()),
            cfg,
            live,
            String::new(),
            registry.clone(),
            profile_runtime,
            open_budget(),
        );

        let error = handler(r#"{"task":"must not start"}"#.to_string())
            .await
            .unwrap_err();
        assert!(error.to_string().contains("concurrency limit reached"));
        assert_eq!(registry.watermark(), 0);
        assert_eq!(registry.progress_snapshot().total, 0);
    }

    #[tokio::test]
    async fn running_background_job_publishes_live_usage_and_activity() {
        let registry = AgentRegistry::new();
        let (release, wait) = tokio::sync::oneshot::channel::<()>();
        let id = registry
            .spawn_background_report(
                "live progress".to_string(),
                "live-model".to_string(),
                move |id, running_registry| async move {
                    running_registry.record_progress_event(
                        id,
                        AgentRuntimeEvent::ProviderRequest {
                            round: 1,
                            purpose: kitsu_core::ProviderRequestPurpose::Inference,
                            model: Some("live-model".to_string()),
                            message_count: 2,
                            tools_offered: 1,
                            structured_output: false,
                            request_timeout_ms: None,
                            turn_remaining_ms: None,
                        },
                    );
                    running_registry.record_progress_event(
                        id,
                        AgentRuntimeEvent::Usage {
                            round: 1,
                            purpose: kitsu_core::ProviderRequestPurpose::Inference,
                            prompt_tokens: 12,
                            completion_tokens: 3,
                            total_tokens: 15,
                        },
                    );
                    let _ = wait.await;
                    SubagentRunReport {
                        result: Ok("done".to_string()),
                        telemetry: AgentTelemetry::started("live-model".to_string()),
                    }
                },
            )
            .unwrap();

        let mut release = Some(release);
        for _ in 0..100 {
            let snapshot = registry.progress_snapshot();
            if snapshot.agents[0].telemetry.total_tokens == 15 {
                assert_eq!(snapshot.running, 1);
                assert_eq!(snapshot.agents[0].id, id);
                assert_eq!(snapshot.agents[0].telemetry.prompt_tokens, 12);
                assert_eq!(
                    snapshot.agents[0].telemetry.last_activity.as_deref(),
                    Some("usage updated")
                );
                release.take().unwrap().send(()).unwrap();
                let _ = registry.cancel_all().await;
                return;
            }
            tokio::task::yield_now().await;
        }
        panic!("running job never published live telemetry");
    }

    #[test]
    fn subscription_created_after_completion_contains_the_completion() {
        let registry = AgentRegistry::new();
        let id = registry.start("already fast".to_string()).unwrap();
        registry.finish(id, Ok("finished before subscribe".to_string()));

        let changes = registry.subscribe();
        let snapshot = *changes.borrow();
        assert_eq!(snapshot.generation, 2);
        assert_eq!(snapshot.running, 0);
        assert_eq!(snapshot.terminal, 1);
        assert_eq!(registry.snapshot(), snapshot);
        assert_eq!(
            registry.result(id).as_deref(),
            Some("finished before subscribe")
        );
    }

    #[test]
    fn shutdown_take_returns_only_terminal_results_not_already_observed() {
        let registry = AgentRegistry::new();
        let observed = registry.start("opened result".to_string()).unwrap();
        let unseen = registry.start("unseen result".to_string()).unwrap();
        let running = registry.start("still running".to_string()).unwrap();
        registry.finish(observed, Ok("opened".to_string()));
        registry.finish(unseen, Err("needs attention".to_string()));

        assert_eq!(registry.result(observed).as_deref(), Some("opened"));
        assert!(registry.result(running).unwrap().contains("still running"));

        let pending = registry.take_unobserved_terminal_results();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, unseen);
        assert!(matches!(pending[0].status, AgentStatus::Failed(_)));
        assert!(registry.take_unobserved_terminal_results().is_empty());
        assert_eq!(registry.running(), 1);
    }

    #[tokio::test]
    async fn coalesced_notifications_retain_the_latest_registry_state() {
        let registry = AgentRegistry::new();
        let mut changes = registry.subscribe();
        let first = registry.start("first".to_string()).unwrap();
        let second = registry.start("second".to_string()).unwrap();
        registry.finish(first, Ok("one".to_string()));
        registry.finish(second, Err("two failed".to_string()));

        // The receiver need not consume four individual edges. One wake exposes
        // the newest generation and counts, from which both results are readable.
        changes.changed().await.expect("registry still exists");
        let snapshot = *changes.borrow_and_update();
        assert_eq!(snapshot.generation, 4);
        assert_eq!(snapshot.running, 0);
        assert_eq!(snapshot.terminal, 2);
        assert_eq!(registry.result(first).as_deref(), Some("one"));
        assert!(registry.result(second).unwrap().contains("two failed"));
    }

    #[tokio::test]
    async fn rollback_cancels_only_agents_started_after_watermark() {
        let registry = AgentRegistry::new();
        let old = registry.start("older work".to_string()).unwrap();
        let old_handle = tokio::spawn(std::future::pending::<()>());
        registry.attach(old, old_handle);
        let watermark = registry.watermark();

        let new = registry.start("rolled-back work".to_string()).unwrap();
        let new_handle = tokio::spawn(std::future::pending::<()>());
        registry.attach(new, new_handle);
        let mut changes = registry.subscribe();
        let before_cancel = *changes.borrow_and_update();

        assert_eq!(registry.cancel_started_after(watermark).await, vec![new]);
        changes.changed().await.expect("registry still exists");
        let after_cancel = *changes.borrow_and_update();
        assert!(after_cancel.generation > before_cancel.generation);
        assert_eq!(after_cancel.running, 1);
        assert_eq!(after_cancel.terminal, 1);
        assert!(registry.result(old).unwrap().contains("still running"));
        assert!(registry
            .result(new)
            .unwrap()
            .contains("parent turn was rolled back"));
        assert_eq!(registry.cancel_started_after(0).await, vec![old]);
    }

    #[tokio::test]
    async fn background_limits_cancel_and_prune_have_joined_terminal_semantics() {
        let registry = AgentRegistry::with_limits(AgentRegistryLimits {
            max_running: 1,
            max_records: 2,
        });
        let first = registry
            .spawn_background("first".to_string(), std::future::pending())
            .unwrap();
        let error = registry
            .spawn_background("refused-running".to_string(), std::future::pending())
            .unwrap_err();
        assert_eq!(error.kind, AgentCapacityKind::Running);
        assert_eq!(registry.watermark(), first, "refusal allocates no id");

        let progress = registry.progress_snapshot();
        assert_eq!(progress.running, 1);
        assert_eq!(progress.available_running_slots, 0);
        assert_eq!(progress.available_record_slots, 1);
        assert_eq!(progress.agents[0].task, "first");
        assert_eq!(progress.agents[0].status, AgentProgressStatus::Running);
        assert_eq!(registry.prune(first), AgentPruneOutcome::Running);

        let cancelled = registry.cancel(first).await;
        let AgentCancelOutcome::Cancelled(cancelled) = cancelled else {
            panic!("running agent should be cancelled: {cancelled:?}");
        };
        assert!(cancelled.joined);
        assert!(matches!(cancelled.status, AgentStatus::Cancelled(_)));
        let cancelled_thread = registry.thread_snapshot(first).unwrap();
        assert_eq!(cancelled_thread.phase, AgentThreadPhase::Cancelled);
        assert_eq!(cancelled_thread.handoff, AgentHandoffState::Ready);
        assert!(!cancelled_thread.steering_open);
        assert!(matches!(
            registry.cancel(first).await,
            AgentCancelOutcome::AlreadyTerminal(_)
        ));

        let second = registry
            .spawn_background("second".to_string(), std::future::pending())
            .unwrap();
        assert_eq!(registry.cancel_all().await.len(), 1);
        let error = registry
            .spawn_background("refused-record".to_string(), std::future::pending())
            .unwrap_err();
        assert_eq!(error.kind, AgentCapacityKind::Records);
        assert_eq!(registry.watermark(), second, "refusal allocates no id");

        let AgentPruneOutcome::Pruned(pruned) = registry.close(first) else {
            panic!("terminal agent should be pruned");
        };
        assert_eq!(pruned.id, first);
        let third = registry
            .spawn_background("third".to_string(), std::future::pending())
            .unwrap();
        assert_eq!(third, second + 1);
        assert_eq!(registry.cancel_all().await.len(), 1);
        let pruned = registry.prune_terminal();
        assert_eq!(
            pruned.iter().map(|status| status.id).collect::<Vec<_>>(),
            vec![second, third]
        );
        assert_eq!(registry.progress_snapshot().total, 0);
    }

    #[tokio::test]
    async fn zero_background_limits_fail_closed_without_allocating_ids() {
        let registry = AgentRegistry::with_limits(AgentRegistryLimits {
            max_running: 0,
            max_records: 0,
        });
        let error = registry
            .spawn_background("disabled".to_string(), std::future::pending())
            .unwrap_err();
        assert_eq!(error.kind, AgentCapacityKind::Running);
        assert_eq!(registry.watermark(), 0);
        assert_eq!(registry.progress_snapshot().total, 0);
    }

    #[tokio::test]
    async fn spawn_agent_refuses_when_spawn_cap_exhausted() {
        let live: LiveModelWindow = Arc::new(Mutex::new(("m".to_string(), None)));
        let (_d, handler) = spawn_agent_tool(
            Arc::new(EchoProvider),
            Arc::new(Registry::new()),
            sub_cfg(),
            live,
            String::new(),
            Arc::new(SpawnBudget::new(0)),
        );
        let error = handler(r#"{"task":"must not start"}"#.to_string())
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("agent spawn cap reached"),
            "model-visible cap error expected: {error}"
        );
    }

    #[tokio::test]
    async fn spawn_agents_admits_up_to_cap_and_labels_the_rest() {
        let live: LiveModelWindow = Arc::new(Mutex::new(("m".to_string(), None)));
        let budget = Arc::new(SpawnBudget::new(1));
        let (_d, handler) = spawn_agents_tool(
            Arc::new(EchoProvider),
            Arc::new(Registry::new()),
            sub_cfg(),
            live,
            String::new(),
            budget.clone(),
        );
        let out = handler(r#"{"tasks":["first task","second task"]}"#.to_string())
            .await
            .expect("the admitted task still runs");
        assert!(out.contains("=== subagent 1"), "{out}");
        assert!(
            out.contains("[not started: agent spawn cap reached"),
            "refused task must be labelled for the model: {out}"
        );
        assert_eq!(budget.used(), 1, "only the admitted spawn is counted");
    }

    #[tokio::test]
    async fn spawn_agents_rejects_empty_tasks() {
        let live: LiveModelWindow = Arc::new(Mutex::new(("m".to_string(), None)));
        let (_d, handler) = spawn_agents_tool(
            Arc::new(EchoProvider),
            Arc::new(Registry::new()),
            sub_cfg(),
            live,
            String::new(),
            open_budget(),
        );
        assert!(handler(r#"{"tasks":[]}"#.to_string()).await.is_err());
    }

    #[tokio::test]
    async fn spawn_agent_reports_bad_args() {
        let provider = Arc::new(MockProvider {
            rounds: Mutex::new(vec![]),
            last_model: Mutex::new(None),
        });
        let live: LiveModelWindow = Arc::new(Mutex::new(("m".to_string(), None)));
        let (_d, handler) = spawn_agent_tool(
            provider,
            Arc::new(Registry::new()),
            sub_cfg(),
            live,
            String::new(),
            open_budget(),
        );
        assert!(handler("not json".to_string()).await.is_err());
    }
}
