//! Subagents (see docs/architecture.md): the `spawn_agent` tool
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
#[cfg(test)]
use kitsu_providers::ProviderRouteContractError;
use kitsu_providers::{
    ChatMessage, Provider, ProviderAttemptCategory, ProviderAttemptOutcome, ProviderError,
    ToolDescriptor,
};
use kitsu_tools::{Decision, HandlerFn, Registry, ToolError};
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

/// Arguments to `agent_wait` (T5-S6, H5). `timeout_secs` is `u64`, so a
/// negative value is an argument error rather than a silent zero or infinity;
/// `until` is a string so its rejection can name the argument.
#[derive(serde::Deserialize)]
struct WaitArgs {
    id: u64,
    until: String,
    timeout_secs: Option<u64>,
}

/// The state an `agent_wait` call is waiting for.
#[derive(Clone, Copy, PartialEq, Eq)]
enum AgentWaitUntil {
    /// The registry's terminal predicate: done, failed, or cancelled.
    Done,
    /// Parked on a user answer ([`AgentStatus::Blocked`]). Non-terminal.
    Blocked,
}

impl AgentWaitUntil {
    /// The `until` spelling, as the model wrote it and as the timeout reports it.
    fn label(self) -> &'static str {
        match self {
            Self::Done => "done",
            Self::Blocked => "blocked",
        }
    }
}

/// Where one agent stands, as far as a wait is concerned. The two non-terminal
/// statuses are distinguished because a park satisfies a `blocked` wait but
/// not a `done` one.
#[derive(Clone, Copy, PartialEq, Eq)]
enum AgentWaitState {
    Running,
    Blocked,
    Terminal,
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

/// Resolve one immutable provider binding after a subagent's effective model
/// (including an explicit profile override) is known. A binder is snapshotted
/// once per subagent run, so replacement cannot mix generations across its
/// internal rounds.
type ProviderBindFn<P> = dyn Fn(&str) -> Result<Arc<P>, ProviderError> + Send + Sync;

pub struct ProviderBinder<P> {
    bind: Arc<ProviderBindFn<P>>,
}

impl<P> Clone for ProviderBinder<P> {
    fn clone(&self) -> Self {
        Self {
            bind: self.bind.clone(),
        }
    }
}

impl<P: 'static> ProviderBinder<P> {
    pub fn new<F>(bind: F) -> Self
    where
        F: Fn(&str) -> Result<Arc<P>, ProviderError> + Send + Sync + 'static,
    {
        Self {
            bind: Arc::new(bind),
        }
    }

    #[cfg(test)]
    fn fixed(provider: Arc<P>) -> Self
    where
        P: Send + Sync,
    {
        Self::new(move |_| Ok(provider.clone()))
    }

    fn resolve(&self, model: &str) -> Result<Arc<P>, ToolError> {
        (self.bind)(model).map_err(|_| {
            ToolError::ExecError(
                "provider route could not be bound for this subagent model".to_string(),
            )
        })
    }
}

/// System prompt seeding a subagent's isolated context.
const SUBAGENT_SYSTEM: &str = "You are a kitsu subagent handling ONE delegated task in a fresh, \
isolated context (you cannot see the parent conversation). Use your tools to complete it, then \
reply with a concise, self-contained result the caller can use directly. Do not ask questions — \
do the task with what you have.";

/// Status of a background subagent (Stage C).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentStatus {
    Running,
    /// Parked on a user answer. Non-terminal, like `Running`, and a unit
    /// variant because nothing carries a reason yet.
    Blocked,
    Done(String),
    Failed(String),
    Cancelled(String),
}

/// One permission request parked on its agent, waiting for an answer.
///
/// The token distinguishes this park from any later one for the same agent, so
/// a fired block timeout can never resolve a request it does not own.
struct ParkedPrompt {
    token: u64,
    /// What the parked request asks permission for, *without its arguments*
    /// (T2-S6). Derived once at the channel boundary in
    /// [`AgentRegistry::prompt_sender`]; the payload itself never travels
    /// this far, so no path or argv word can reach the timeline.
    kind: thread::AgentApprovalKind,
    reply: tokio::sync::oneshot::Sender<Decision>,
}

/// Collapse a parked request's payload to the argument-free approval kind.
///
/// This is the security boundary of T2-S6: it is the only place the payload
/// is inspected, and the only thing it returns is a unit enum variant. The
/// action verb plus the payload discriminant carry everything "permission to
/// X" needs.
fn approval_kind(payload: &kitsu_tools::PromptRequestPayload) -> thread::AgentApprovalKind {
    match payload {
        kitsu_tools::PromptRequestPayload::Path(request) => match request.action {
            kitsu_tools::Action::Read => thread::AgentApprovalKind::ReadFile,
            kitsu_tools::Action::Write => thread::AgentApprovalKind::WriteFile,
        },
        kitsu_tools::PromptRequestPayload::Command(_) => thread::AgentApprovalKind::RunCommand,
    }
}

/// How a resolved decision is spelled in the approval record. `Interrupt` is
/// never a human answer: it arrives from B3's timeout or a cancellation, so
/// it is recorded as a timeout rather than a denial.
fn approval_outcome(decision: Decision) -> thread::AgentApprovalOutcome {
    match decision {
        Decision::AllowOnce => thread::AgentApprovalOutcome::AllowedOnce,
        Decision::AllowAlways => thread::AgentApprovalOutcome::AllowedAlways,
        Decision::Deny => thread::AgentApprovalOutcome::Denied,
        Decision::Interrupt => thread::AgentApprovalOutcome::TimedOut,
    }
}

struct AgentRecord {
    task: String,
    status: AgentStatus,
    /// `Some` exactly while `status` is [`AgentStatus::Blocked`].
    pending_prompt: Option<ParkedPrompt>,
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
    /// First immutable provider route generation observed for this child run.
    pub route_generation: Option<u64>,
    /// True if attempt evidence ever claimed a second generation. A correctly
    /// bound child keeps this false; retaining the bit avoids hiding drift.
    pub route_generation_mismatch: bool,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
    pub tool_count: u64,
    pub write_count: u64,
    pub last_activity: Option<String>,
    /// When the last activity was recorded — the deck renders its age so a
    /// stalled running agent is visible at a glance (ui-parity item 13).
    pub last_activity_at: Option<Instant>,
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
    ProviderRouteConfiguration,
    Interrupted,
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
            Self::ProviderRouteConfiguration => "provider_route_configuration",
            Self::Interrupted => "interrupted",
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
            last_activity_at: Some(Instant::now()),
            recent_activity: vec!["started".to_string()],
            ..Self::default()
        }
    }

    fn record_activity(&mut self, activity: String) {
        self.last_activity = Some(activity.clone());
        self.last_activity_at = Some(Instant::now());
        if self.recent_activity.len() == RECENT_ACTIVITY_LIMIT {
            self.recent_activity.remove(0);
        }
        self.recent_activity.push(activity);
    }

    fn observe_route_generation(&mut self, generation: Option<u64>) {
        let Some(generation) = generation else {
            return;
        };
        match self.route_generation {
            None => self.route_generation = Some(generation),
            Some(existing) if existing != generation => self.route_generation_mismatch = true,
            Some(_) => {}
        }
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
    /// What a parked agent is waiting on: the permission **kind** and nothing
    /// else (T2-S7, N8). `Some` only while `status` is
    /// [`AgentProgressStatus::Blocked`]. A unit enum deliberately, so no path
    /// or argv word can ride out to the `on_agent_state` hook (F40).
    pub waiting_for: Option<thread::AgentApprovalKind>,
    pub elapsed_ms: u64,
    pub result_bytes: Option<usize>,
    /// Milliseconds since the agent's last recorded activity (running rows).
    pub last_activity_ms_ago: Option<u64>,
    pub telemetry: AgentTelemetry,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentProgressStatus {
    Running,
    /// Parked on a user answer; the `/jobs view` group `needs input`.
    Blocked,
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
    /// Agents parked on a user answer. Counted here so the per-state counts
    /// still sum to `total`. Unrelated to `AgentRegistrySnapshot::blocked`.
    pub blocked: usize,
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

/// B3's default blocking timeout: a blocked agent resolves `Interrupt` after
/// this long, so a headless run cannot hang. This constant is the DEFAULT of
/// the `agents.block_timeout_secs` config key; the value a registry actually
/// waits is [`AgentRegistry::block_timeout`].
pub const DEFAULT_AGENT_BLOCK_TIMEOUT: std::time::Duration =
    std::time::Duration::from_secs(30 * 60);

/// Atomic encoding of [`AgentBlockingMode`]. `FailClosed` is zero so a
/// zero-initialized registry is fail-closed by construction.
const BLOCKING_FAIL_CLOSED: u64 = 0;
const BLOCKING_ASK: u64 = 1;

/// Outcome of answering one agent's parked permission request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentAnswerOutcome {
    /// The agent was blocked; its gate received exactly this decision.
    Answered,
    /// The agent exists but has no parked request. Reported rather than
    /// silently ignored so `/jobs reply` can tell the user why nothing moved.
    NotBlocked,
    NotFound,
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

/// Outcome of naming one agent (the agent view's `n` key).
///
/// Every refusal names *why*, because the name is what A2's row head shows: a
/// name accepted and then not shown, or coerced into something the user did
/// not type, is the silent rejection this project refuses. In particular this
/// deliberately does not borrow `safe_inline_label`, which coerces an empty
/// label to `unknown` and truncates with an ellipsis — right for a label the
/// model produced, wrong for a name a human just typed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentRenameOutcome {
    /// Named, reporting the name this one replaced so the user can tell the
    /// key reached the row they meant.
    Renamed {
        previous: Option<String>,
    },
    /// The name was empty, or whitespace only.
    Empty,
    /// Longer than `max` characters. Refused rather than truncated.
    TooLong {
        max: usize,
    },
    /// The name carries a control character — a newline or an `ESC` in a name
    /// would corrupt every row it is drawn on. Non-ASCII is legal: the rule is
    /// control characters, not ASCII.
    Unprintable,
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
    /// Monotonic park identifier; see [`ParkedPrompt::token`].
    next_prompt_token: AtomicU64,
    changes: watch::Sender<AgentRegistrySnapshot>,
    limits: AgentRegistryLimits,
    thread_limits: AgentThreadLimits,
    /// `agents.blocking` for the run, stored atomically so the spawn path can
    /// read it through the `Arc` it already holds. 0 is `FailClosed`.
    blocking: AtomicU64,
    /// `agents.block_timeout_secs` for the run, kept in nanoseconds so a
    /// sub-second timeout (which only a test asks for) survives the round trip.
    block_timeout_nanos: AtomicU64,
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
    /// Retained terminal agents whose result handoff is still
    /// [`AgentHandoffState::Ready`], meaning no user or model surface has
    /// observed the result yet. Carried separately from `terminal` because a
    /// record stays terminal forever once it finishes, while this count is what
    /// a watcher must surface as "there is something here for you to read" and
    /// must drop again the moment the result is read or acknowledged.
    pub unacknowledged: usize,
    /// Agents parked on a user answer ([`AgentStatus::Blocked`]). Non-terminal
    /// and disjoint from `running`, so `running + blocked + terminal` is the
    /// retained record count.
    pub blocked: usize,
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
            next_prompt_token: AtomicU64::new(0),
            changes,
            limits,
            thread_limits: AgentThreadLimits::default(),
            blocking: AtomicU64::new(BLOCKING_FAIL_CLOSED),
            block_timeout_nanos: AtomicU64::new(DEFAULT_AGENT_BLOCK_TIMEOUT.as_nanos() as u64),
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
                    pending_prompt: None,
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
                    pending_prompt: None,
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
            .filter(|record| matches!(record.status, AgentStatus::Running | AgentStatus::Blocked))
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

    /// Channel the gates park ungranted decisions on (design §4.4, B1-B3).
    ///
    /// The returned sender's receiver is serviced by a detached task that marks
    /// the requesting agent [`AgentStatus::Blocked`], holds its reply channel,
    /// and arms a [`AgentRegistry::block_timeout`] timer resolving
    /// [`Decision::Interrupt`] so a headless run cannot hang (B3). The task
    /// holds only a weak reference, so handing out a sender never keeps the
    /// registry alive.
    pub fn prompt_sender(self: &Arc<Self>) -> kitsu_tools::PromptRequestSender {
        let (sender, mut receiver) =
            tokio::sync::mpsc::unbounded_channel::<kitsu_tools::PromptRequest>();
        let owner = Arc::downgrade(self);
        tokio::spawn(async move {
            while let Some(request) = receiver.recv().await {
                let Some(registry) = owner.upgrade() else {
                    // The registry is gone; dropping `reply` resolves
                    // `Interrupt` for this and every later request.
                    break;
                };
                let agent_id = request.agent_id;
                // Derive the argument-free kind here, at the boundary, and
                // carry only that onward (T2-S6).
                let kind = approval_kind(&request.payload);
                let Some(token) = registry.park_prompt(agent_id, kind, request.reply) else {
                    continue;
                };
                let timer_owner = Arc::downgrade(&registry);
                // Read the CONFIGURED timeout, not the constant: the constant
                // is only this value's default.
                let timeout = registry.block_timeout();
                drop(registry);
                tokio::spawn(async move {
                    tokio::time::sleep(timeout).await;
                    if let Some(registry) = timer_owner.upgrade() {
                        registry.resolve_prompt(agent_id, Some(token), Decision::Interrupt);
                    }
                });
            }
        });
        sender
    }

    /// The run's `agents.blocking` mode. Taken by `&self` so the spawn path can
    /// read it through the `Arc` it already holds.
    pub fn blocking(&self) -> crate::config::AgentBlockingMode {
        match self.blocking.load(Ordering::Relaxed) {
            BLOCKING_ASK => crate::config::AgentBlockingMode::Ask,
            _ => crate::config::AgentBlockingMode::FailClosed,
        }
    }

    /// Install the run's `agents.blocking` mode.
    pub fn set_blocking(&self, mode: crate::config::AgentBlockingMode) {
        let encoded = match mode {
            crate::config::AgentBlockingMode::Ask => BLOCKING_ASK,
            crate::config::AgentBlockingMode::FailClosed => BLOCKING_FAIL_CLOSED,
        };
        self.blocking.store(encoded, Ordering::Relaxed);
    }

    /// How long a parked request waits before resolving `Interrupt` (B3).
    pub fn block_timeout(&self) -> std::time::Duration {
        std::time::Duration::from_nanos(self.block_timeout_nanos.load(Ordering::Relaxed))
    }

    /// Install the run's blocking timeout. Stored in nanoseconds, so a
    /// sub-second value is preserved exactly rather than truncated to zero.
    pub fn set_block_timeout(&self, timeout: std::time::Duration) {
        self.block_timeout_nanos.store(
            timeout.as_nanos().min(u128::from(u64::MAX)) as u64,
            Ordering::Relaxed,
        );
    }

    /// Answer one blocked agent's parked request (design §4.4; B1).
    ///
    /// All three of `AllowOnce`, `AllowAlways` and `Deny` are answers: the gate
    /// turns a denial into a `PolicyDenied` tool error itself, so the agent
    /// resumes either way. A non-blocked agent and an unknown id are reported
    /// distinctly and change no state, because `/jobs reply` takes a
    /// human-typed id in a world where the agent may have just timed out,
    /// been cancelled, or finished.
    pub fn answer(&self, id: u64, decision: Decision) -> AgentAnswerOutcome {
        self.resolve_prompt(id, None, decision)
    }

    /// Park one request on its agent, returning the park token. A request for
    /// an unknown, terminal, or already-blocked agent is resolved immediately
    /// as `Interrupt` rather than held, so no gate can wait forever.
    fn park_prompt(
        &self,
        id: u64,
        kind: thread::AgentApprovalKind,
        reply: tokio::sync::oneshot::Sender<Decision>,
    ) -> Option<u64> {
        let (token, update) = {
            let mut agents = self.agents.lock().expect("agent registry");
            let parkable = agents
                .get(&id)
                .is_some_and(|record| matches!(record.status, AgentStatus::Running));
            if !parkable {
                let _ = reply.send(Decision::Interrupt);
                return None;
            }
            let token = self.next_prompt_token.fetch_add(1, Ordering::Relaxed) + 1;
            let record = agents.get_mut(&id).expect("inspected agent record");
            record.status = AgentStatus::Blocked;
            record.thread.block();
            record.thread.record_approval_request(kind);
            record.pending_prompt = Some(ParkedPrompt { token, kind, reply });
            (token, self.changed_snapshot(&agents))
        };
        // B4: publish a new generation so watchers observe the block.
        self.publish(update);
        Some(token)
    }

    /// Resolve a parked request with exactly `decision` and resume its agent.
    /// `token` of `None` matches whatever is parked (a user answer); `Some`
    /// matches only that park, so a stale timeout is a no-op.
    fn resolve_prompt(
        &self,
        id: u64,
        token: Option<u64>,
        decision: Decision,
    ) -> AgentAnswerOutcome {
        let (reply, update) = {
            let mut agents = self.agents.lock().expect("agent registry");
            let Some(record) = agents.get_mut(&id) else {
                return AgentAnswerOutcome::NotFound;
            };
            let parked = match (&record.pending_prompt, token) {
                (Some(parked), Some(token)) => parked.token == token,
                (Some(_), None) => true,
                (None, _) => false,
            };
            if !parked {
                return AgentAnswerOutcome::NotBlocked;
            }
            let parked = record
                .pending_prompt
                .take()
                .expect("inspected parked prompt");
            record.status = AgentStatus::Running;
            record.thread.unblock();
            record
                .thread
                .record_approval_outcome(parked.kind, approval_outcome(decision));
            (parked.reply, self.changed_snapshot(&agents))
        };
        self.publish(update);
        // Outside the lock: the waiting gate wakes and may re-enter the
        // registry immediately.
        let _ = reply.send(decision);
        AgentAnswerOutcome::Answered
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
        let mut blocked = 0;
        for agent in &progress {
            match agent.status {
                AgentProgressStatus::Running => running += 1,
                AgentProgressStatus::Blocked => blocked += 1,
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
            blocked,
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
            (!matches!(record.status, AgentStatus::Running | AgentStatus::Blocked))
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
                Some(record)
                    if !matches!(record.status, AgentStatus::Running | AgentStatus::Blocked) =>
                {
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

    /// Compatible ID-only projection retained for focused registry tests.
    #[cfg(test)]
    pub async fn cancel_started_after(&self, watermark: u64) -> Vec<u64> {
        self.cancel_started_after_with_status(watermark)
            .await
            .into_iter()
            .map(|status| status.id)
            .collect()
    }

    /// Status-preserving rollback cancellation for terminal lifecycle owners.
    /// This retains malformed/unattached `joined: false` truth instead of
    /// silently upgrading it to successful cleanup.
    pub async fn cancel_started_after_with_status(
        &self,
        watermark: u64,
    ) -> Vec<AgentTerminalStatus> {
        self.cancel_matching(
            |id| id > watermark,
            "cancelled because the parent turn was rolled back",
        )
        .await
    }

    async fn cancel_matching<F>(&self, mut predicate: F, reason: &str) -> Vec<AgentTerminalStatus>
    where
        F: FnMut(u64) -> bool,
    {
        let (mut statuses, handles, replies, update) = {
            let mut agents = self.agents.lock().expect("agent registry");
            let mut statuses = Vec::new();
            let mut handles = Vec::new();
            let mut replies = Vec::new();
            for (id, record) in agents.iter_mut() {
                if predicate(*id)
                    && matches!(record.status, AgentStatus::Running | AgentStatus::Blocked)
                {
                    // A blocked agent is cancellable like a running one; its
                    // parked request resolves `Interrupt` rather than being
                    // dropped silently, and the thread leaves `Blocked` so the
                    // terminal transition starts from `Running` as ever.
                    record.thread.unblock();
                    if let Some(parked) = record.pending_prompt.take() {
                        // Recorded here, not through `resolve_prompt`: the
                        // prompt is already taken under this lock, and the
                        // outcome must land before `finish` closes the
                        // timeline (F43).
                        record.thread.record_approval_outcome(
                            parked.kind,
                            thread::AgentApprovalOutcome::Cancelled,
                        );
                        replies.push(parked.reply);
                    }
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
            (statuses, handles, replies, update)
        };
        for reply in replies {
            let _ = reply.send(Decision::Interrupt);
        }
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
            if matches!(record.status, AgentStatus::Running | AgentStatus::Blocked) {
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
                    (!matches!(record.status, AgentStatus::Running | AgentStatus::Blocked))
                        .then_some(*id)
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
            .filter(|r| matches!(r.status, AgentStatus::Running | AgentStatus::Blocked))
            .count()
    }

    /// Clear completed records at a session boundary. Callers first ensure no
    /// agents are running, so results from one conversation cannot be fetched
    /// by the next conversation through `agent_result`.
    pub fn reset(&self) {
        let update = {
            let mut agents = self.agents.lock().expect("agent registry");
            debug_assert!(!agents.values().any(|record| matches!(
                record.status,
                AgentStatus::Running | AgentStatus::Blocked
            )));
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
                    AgentStatus::Blocked => "blocked",
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
            let acknowledged =
                !matches!(record.status, AgentStatus::Running | AgentStatus::Blocked)
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

    /// Where one agent stands as far as `agent_wait` is concerned, or `None`
    /// if the id belongs to no agent of this session.
    ///
    /// `Terminal` is the registry's own predicate — anything that is neither
    /// `Running` nor `Blocked` — so a failed or cancelled agent satisfies a
    /// `done` wait instead of hanging it to the timeout.
    fn wait_state(&self, id: u64) -> Option<AgentWaitState> {
        self.agents
            .lock()
            .expect("agent registry")
            .get(&id)
            .map(|record| match record.status {
                AgentStatus::Running => AgentWaitState::Running,
                AgentStatus::Blocked => AgentWaitState::Blocked,
                _ => AgentWaitState::Terminal,
            })
    }

    /// Return the explicit handoff state without reading result content.
    pub fn handoff_state(&self, id: u64) -> Option<AgentHandoffState> {
        self.agents
            .lock()
            .expect("agent registry")
            .get(&id)
            .map(|record| record.thread.handoff())
    }

    /// The longest display name an agent may be given, in characters.
    ///
    /// A name stands in for the task head on A2's row, so the budget is the
    /// head's: a name that fitted the row and a task head that fitted it are
    /// the same width.
    pub const NAME_MAX_CHARS: usize = kitsu_tui::agents_view::AgentRow::TASK_HEAD_CHARS;

    /// Name one agent, for the agent view's `n` key.
    ///
    /// The name is the trimmed input; it must be non-empty, at most
    /// [`AgentRegistry::NAME_MAX_CHARS`] characters and free of control
    /// characters. Any phase may be renamed, terminal included — the agent
    /// whose result you are about to read is exactly the one worth labelling —
    /// and naming a row takes **no** custody of its result: the handoff is
    /// untouched here, so the exit guard still holds an unread result back.
    ///
    /// A refused name leaves the previous name exactly as it was.
    pub fn rename(&self, id: u64, name: &str) -> AgentRenameOutcome {
        let name = name.trim();
        if name.is_empty() {
            return AgentRenameOutcome::Empty;
        }
        if name.chars().count() > Self::NAME_MAX_CHARS {
            return AgentRenameOutcome::TooLong {
                max: Self::NAME_MAX_CHARS,
            };
        }
        if name.chars().any(char::is_control) {
            return AgentRenameOutcome::Unprintable;
        }
        let (previous, update) = {
            let mut agents = self.agents.lock().expect("agent registry");
            let Some(record) = agents.get_mut(&id) else {
                return AgentRenameOutcome::NotFound;
            };
            let previous = record.thread.set_name(name);
            // The counts are unchanged, but the generation is not: a watcher
            // repaints so the new name is on screen without waiting for the
            // next unrelated change.
            (previous, self.changed_snapshot(&agents))
        };
        self.publish(update);
        AgentRenameOutcome::Renamed { previous }
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
                    // `acknowledge_handoff` MUTATES the handoff state; it must be
                    // called unconditionally. Inside `debug_assert!` the whole
                    // expression is compiled out of a release build, so the
                    // transition to Acknowledged silently never happened in the
                    // binary users actually run — a fetched result stayed Ready
                    // forever and was re-delivered as unobserved at shutdown.
                    let acknowledged = record.thread.acknowledge_handoff();
                    debug_assert!(acknowledged);
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
                    // Same release-build hazard as `acknowledge_result`: this
                    // call marks the result observed, so it cannot live inside
                    // `debug_assert!` or every call would re-return results that
                    // were already drained.
                    let acknowledged = record.thread.acknowledge_handoff();
                    debug_assert!(acknowledged);
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
        let blocked = agents
            .values()
            .filter(|record| matches!(record.status, AgentStatus::Blocked))
            .count();
        let failed = agents
            .values()
            .filter(|record| matches!(record.status, AgentStatus::Failed(_)))
            .count();
        let unacknowledged = agents
            .values()
            .filter(|record| record.thread.handoff() == AgentHandoffState::Ready)
            .count();
        AgentRegistrySnapshot {
            generation,
            running,
            terminal: agents.len().saturating_sub(running).saturating_sub(blocked),
            failed,
            unacknowledged,
            blocked,
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
        AgentStatus::Blocked => format!("agent #{id} is waiting on an answer"),
        AgentStatus::Done(result) => result.clone(),
        AgentStatus::Failed(error) => format!("agent #{id} failed: {error}"),
        AgentStatus::Cancelled(reason) => format!("agent #{id} cancelled: {reason}"),
    }
}

fn agent_progress(id: u64, record: &AgentRecord) -> AgentProgress {
    let (status, result_bytes) = match &record.status {
        AgentStatus::Running => (AgentProgressStatus::Running, None),
        AgentStatus::Blocked => (AgentProgressStatus::Blocked, None),
        AgentStatus::Done(result) => (AgentProgressStatus::Done, Some(result.len())),
        AgentStatus::Failed(error) => (AgentProgressStatus::Failed, Some(error.len())),
        AgentStatus::Cancelled(reason) => (AgentProgressStatus::Cancelled, Some(reason.len())),
    };
    AgentProgress {
        id,
        task: record.task.clone(),
        profile: record.thread.profile().map(str::to_string),
        status,
        waiting_for: record.pending_prompt.as_ref().map(|parked| parked.kind),
        elapsed_ms: elapsed_millis(record.started_at, record.finished_at),
        result_bytes,
        last_activity_ms_ago: record
            .telemetry
            .last_activity_at
            .map(|at| at.elapsed().as_millis() as u64),
        telemetry: record.telemetry.clone(),
    }
}

fn terminal_status(id: u64, record: &AgentRecord, joined: bool) -> AgentTerminalStatus {
    debug_assert!(!matches!(
        record.status,
        AgentStatus::Running | AgentStatus::Blocked
    ));
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
            ProviderError::RouteContract(_) => AgentFailureClass::ProviderRouteConfiguration,
        },
        AgentError::ToolCallDuringToolFreeFinalization => {
            AgentFailureClass::ProviderInvalidResponse
        }
        AgentError::Interrupted(_) => AgentFailureClass::Interrupted,
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
            ProviderError::RouteContract(_) => {
                "provider route changed or lacks a required capability".to_string()
            }
        },
        AgentError::ToolCallDuringToolFreeFinalization => {
            "model returned a tool call during tool-free structured finalization; no tool was executed"
                .to_string()
        }
        // B3: a blocked agent whose parked request timed out arrives here. The
        // named interruption is kept so the reason says WHICH authority input
        // closed rather than only that the run stopped.
        AgentError::Interrupted(interruption) => format!(
            "run interrupted before completion: {}",
            match interruption {
                kitsu_core::run::RunInterruption::AuthorityInputClosed =>
                    kitsu_tools::ToolInterruption::AuthorityInputClosed,
            }
        ),
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
            route_generation,
            attempt,
            max_attempts,
            category,
            outcome,
            ..
        } => {
            telemetry.observe_route_generation(route_generation);
            let route = route_generation
                .map(|generation| format!(" on route generation {generation}"))
                .unwrap_or_default();
            telemetry.record_activity(format!(
                "provider attempt {attempt}/{max_attempts}{route}: {}, {}",
                provider_category_label(category),
                provider_outcome_label(outcome)
            ));
        }
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
        // Deliberate no-op: agent telemetry feeds the TTY agents view, and
        // F120 scopes that human-facing surface to a separate card. Matched
        // explicitly rather than swept into a wildcard so adding an activity
        // line here stays a decision, not an accident.
        AgentRuntimeEvent::ToolResultsDropped { .. } => {}
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
#[cfg(test)]
pub fn spawn_agent_tool<P: Provider + 'static>(
    provider: Arc<P>,
    sub_tools: Arc<Registry>,
    sub_cfg: AgentConfig,
    live: LiveModelWindow,
    extra_system: String,
    budget: Arc<SpawnBudget>,
) -> (ToolDescriptor, HandlerFn) {
    spawn_agent_tool_with_binder(
        ProviderBinder::fixed(provider),
        sub_tools,
        sub_cfg,
        live,
        extra_system,
        budget,
    )
}

pub fn spawn_agent_tool_with_binder<P: Provider + 'static>(
    provider: ProviderBinder<P>,
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
            let provider = provider.resolve(&cfg.model)?;
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
#[cfg(test)]
pub fn spawn_agents_tool<P: Provider + 'static>(
    provider: Arc<P>,
    sub_tools: Arc<Registry>,
    sub_cfg: AgentConfig,
    live: LiveModelWindow,
    extra_system: String,
    budget: Arc<SpawnBudget>,
) -> (ToolDescriptor, HandlerFn) {
    spawn_agents_tool_with_binder(
        ProviderBinder::fixed(provider),
        sub_tools,
        sub_cfg,
        live,
        extra_system,
        budget,
    )
}

pub fn spawn_agents_tool_with_binder<P: Provider + 'static>(
    provider: ProviderBinder<P>,
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
            let provider = provider.resolve(&cfg.model)?;
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

#[cfg(test)]
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
    spawn_agent_bg_tool_with_binder(
        ProviderBinder::fixed(provider),
        sub_tools,
        sub_cfg,
        live,
        extra_system,
        registry,
        profile_runtime,
        budget,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn spawn_agent_bg_tool_with_binder<P: Provider + 'static>(
    provider: ProviderBinder<P>,
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
            let provider = provider.resolve(&cfg.model)?;
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
                        // B1: `blocking = ask` routes this detached agent's
                        // ungranted gates through the registry so the user can
                        // answer them; the default stays byte-identically
                        // fail-closed.
                        let prompt_route = match running_registry.blocking() {
                            crate::config::AgentBlockingMode::Ask => {
                                Some(running_registry.prompt_sender())
                            }
                            crate::config::AgentBlockingMode::FailClosed => None,
                        };
                        let run = run_subagent_report_with_progress(
                            &provider,
                            &effective_tools,
                            &cfg,
                            &effective_system,
                            a.task,
                            Some((running_registry, id)),
                            Some(runtime),
                        );
                        let mut report = match prompt_route {
                            Some(sender) => {
                                kitsu_tools::scope_with_registry_prompts(id, sender, run).await
                            }
                            None => kitsu_tools::scope_without_interactive_prompts(run).await,
                        };
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

/// How the agent view's composer starts an agent (card T4-S7, requirement
/// **A6**): a task and an optional profile name in, an id or one line of
/// refusal out.
///
/// A closure rather than a function the overlay calls, because the overlay has
/// no provider: the binder, the subagent registry, the config template and the
/// profile catalog are all the session's, and the view holds only this.
///
/// **It takes no [`SpawnBudget`], and that is the point of the card.** The
/// budget is the *model's* per-turn fan-out cap — it exists so a storming model
/// cannot fan out against the gateway, and `spawn_cap_error` is phrased as an
/// instruction to a model for exactly that reason. A human typing a task into
/// the view is not a storming model, so admission here is the registry's own
/// ceilings ([`AgentRegistryLimits`]) and nothing else. A dispatch refused
/// because the *model* had already spent the turn's budget would be a refusal
/// with nothing to do with the person who typed it.
///
/// The `String` error is a line for the frame, and it is the registry's or the
/// catalog's own wording ([`AgentStartError`]'s `Display`, or
/// `spawn_agent_bg`'s unknown-profile message) rather than a second wording of
/// either.
pub type ViewDispatcher = Arc<dyn Fn(String, Option<String>) -> Result<u64, String> + Send + Sync>;

/// Build the agent view's dispatcher over a provider binder.
///
/// The same arguments `spawn_agent_bg_tool_with_binder` takes, minus the
/// budget, and the same work: resolve the profile against the catalog, build
/// the effective tools and system prompt, and admit through
/// `AgentRegistry::spawn_background_thread`.
///
/// **Card T4-S7 is written test-first; this is the seam the tests pin and the
/// implementer fills.**
#[allow(clippy::too_many_arguments)]
pub fn view_dispatcher<P: Provider + 'static>(
    provider: ProviderBinder<P>,
    sub_tools: Arc<Registry>,
    sub_cfg: AgentConfig,
    live: LiveModelWindow,
    extra_system: String,
    registry: Arc<AgentRegistry>,
    profile_runtime: BackgroundProfileRuntime,
) -> ViewDispatcher {
    Arc::new(move |task: String, profile: Option<String>| {
        // The catalog answers the one question the view's parser deliberately
        // did not: whether the name `@` selected exists. Its own wording,
        // because a second wording of "no such profile" would be a second
        // source of truth about which profiles exist.
        let selected_profile = match profile.as_deref() {
            Some(name) => Some(profile_runtime.catalog.get(name).cloned().ok_or_else(|| {
                format!("unknown background-agent profile '{name}'; use /profiles")
            })?),
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
            // Forward-only-if-explicitly-requested, exactly as the model's
            // tool does it: a profile that says nothing keeps the parent's
            // omission, so gateways that reject the field are not broken by
            // merely selecting a profile.
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
        let provider = provider
            .resolve(&cfg.model)
            .map_err(|error| error.to_string())?;
        let model = cfg.model.clone();
        let preview = task_preview(&task);
        let profile_name = selected_profile
            .as_ref()
            .map(|profile| profile.name.clone());
        let rejected_field_hint = selected_profile.as_ref().and_then(|profile| {
            profile.reasoning_effort.requested().map(|_| {
                format!(
                    "profile '{}' requested reasoning_effort={} \u{2014} this gateway rejected \
                     that request field; see /profiles or dispatch without a profile",
                    profile.name,
                    profile.reasoning_effort.label()
                )
            })
        });
        // **A6's whole point**: admission is the registry's ceilings here and
        // nothing else. No `SpawnBudget` is consulted, because the budget caps
        // a *model's* per-turn fan-out and a human typing a task is not a
        // storming model.
        registry
            .spawn_background_thread(
                preview,
                task.clone(),
                model,
                profile_name,
                move |id, running_registry, runtime| async move {
                    let prompt_route = match running_registry.blocking() {
                        crate::config::AgentBlockingMode::Ask => {
                            Some(running_registry.prompt_sender())
                        }
                        crate::config::AgentBlockingMode::FailClosed => None,
                    };
                    let run = run_subagent_report_with_progress(
                        &provider,
                        &effective_tools,
                        &cfg,
                        &effective_system,
                        task,
                        Some((running_registry, id)),
                        Some(runtime),
                    );
                    let mut report = match prompt_route {
                        Some(sender) => {
                            kitsu_tools::scope_with_registry_prompts(id, sender, run).await
                        }
                        None => kitsu_tools::scope_without_interactive_prompts(run).await,
                    };
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
            .map_err(|error| error.to_string())
    })
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

/// Default timeout for one `agent_wait` call when the model omits
/// `timeout_secs`. Deliberately short next to [`DEFAULT_AGENT_BLOCK_TIMEOUT`]:
/// that one bounds a *human* answering an approval, this one bounds a
/// foreground round the model is paying for.
pub const DEFAULT_AGENT_WAIT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// Ceiling on an `agent_wait` timeout. A model that asks for `u64::MAX`
/// seconds gets this instead of an overflowing deadline.
pub const MAX_AGENT_WAIT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30 * 60);

/// Build the `agent_wait` tool (T5-S6, H5): block until a background agent
/// reaches `done` or `blocked`, or until this call's own timeout elapses.
///
/// It spawns nothing, so it takes **no** [`SpawnBudget`] — that absent
/// parameter is H5's "does not consume the `SpawnBudget`", enforced by the
/// signature rather than by a runtime check. The `--max-rounds` bound is the
/// orchestrator's, not this tool's: one call is one round of the caller's turn.
pub fn agent_wait_tool(registry: Arc<AgentRegistry>) -> (ToolDescriptor, HandlerFn) {
    let descriptor = ToolDescriptor {
        name: "agent_wait".to_string(),
        // Kept deliberately terse: schema tokens come out of the model's
        // window, and the five multi-agent tools already cost ~750 of them.
        description: "Wait for a background subagent (spawn_agent_bg) to reach a state: \
                      `done` (finished, failed, or cancelled) or `blocked` (parked on an \
                      answer). Spawns nothing, so it does not count against the spawn cap. \
                      Returns the agent's result or status, or a 'timed out' line."
            .to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "id": {"type": "integer", "description": "The background agent id"},
                "until": {"type": "string", "enum": ["done", "blocked"]},
                "timeout_secs": {
                    "type": "integer",
                    "description": "Seconds to wait; default 60, max 1800, 0 polls once."
                }
            },
            "required": ["id", "until"]
        }),
    };
    let handler: HandlerFn = Box::new(move |args: String| {
        let registry = registry.clone();
        Box::pin(async move {
            // A negative `timeout_secs` fails here, in `u64` deserialization:
            // it is the model's mistake, and coercing it to zero or to forever
            // would hide that.
            let a: WaitArgs =
                serde_json::from_str(&args).map_err(|e| ToolError::InvalidArgs(e.to_string()))?;
            // `until` is deserialized as a string and matched here rather than
            // as a serde enum, so the rejection names the argument at fault.
            let until = match a.until.as_str() {
                "done" => AgentWaitUntil::Done,
                "blocked" => AgentWaitUntil::Blocked,
                other => {
                    return Err(ToolError::InvalidArgs(format!(
                        "until must be \"done\" or \"blocked\", got \"{other}\""
                    )))
                }
            };
            // Clamped BEFORE it becomes a deadline: an unclamped `i64::MAX`
            // seconds overflows `std::time`'s instant arithmetic and panics.
            let timeout = a
                .timeout_secs
                .map(std::time::Duration::from_secs)
                .unwrap_or(DEFAULT_AGENT_WAIT_TIMEOUT)
                .min(MAX_AGENT_WAIT_TIMEOUT);

            // Armed before the first state read, so a change landing between
            // the read and the wait cannot be missed (the check-then-wait race).
            let mut changes = registry.subscribe();
            // First of the two unknown-id guards: a typo must cost nothing at
            // all, never a whole timeout. The second is in the loop below,
            // which re-reads the state on every wakeup.
            if registry.wait_state(a.id).is_none() {
                return Err(ToolError::ExecError(format!(
                    "no background agent #{}",
                    a.id
                )));
            }
            let deadline = tokio::time::Instant::now() + timeout;
            loop {
                let Some(state) = registry.wait_state(a.id) else {
                    // Second guard: the record vanished while we waited.
                    return Err(ToolError::ExecError(format!(
                        "no background agent #{}",
                        a.id
                    )));
                };
                match (until, state) {
                    // `done` is the registry's terminal predicate, not the
                    // `Done` variant alone: a failed or cancelled agent will
                    // never reach `Done`, so waiting for the variant would
                    // burn the whole timeout on a state that cannot arrive.
                    (AgentWaitUntil::Done, AgentWaitState::Terminal)
                    | (AgentWaitUntil::Blocked, AgentWaitState::Blocked) => {
                        return Ok(registry
                            .result(a.id)
                            .unwrap_or_else(|| format!("no background agent #{}", a.id)))
                    }
                    // A terminal agent can never park, so holding the caller's
                    // round open for a `blocked` that is now unreachable is
                    // pointless. (The converse does NOT hold: `blocked` is
                    // non-terminal, so a `done` wait on a parked agent runs
                    // its timeout out.)
                    (AgentWaitUntil::Blocked, AgentWaitState::Terminal) => {
                        let result = registry
                            .result(a.id)
                            .unwrap_or_else(|| format!("no background agent #{}", a.id));
                        return Ok(format!(
                            "agent #{} reached a terminal state and can no longer block: {result}",
                            a.id
                        ));
                    }
                    _ => {}
                }
                // A timeout is a normal outcome, not a `ToolError`: the model
                // is told the state it did not reach, the timeout actually
                // used (defaulted or clamped), and where the agent stands.
                if tokio::time::timeout_at(deadline, changes.changed())
                    .await
                    .is_err()
                {
                    let status = registry
                        .peek_result(a.id)
                        .unwrap_or_else(|| format!("no background agent #{}", a.id));
                    return Ok(format!(
                        "timed out after {}s waiting for agent #{} to reach {}: {status}",
                        timeout.as_secs(),
                        a.id,
                        until.label()
                    ));
                }
            }
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
                    prompt_tokens_details: None,
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
        async fn chat_stream_with_telemetry(
            &self,
            req: ChatRequest,
            telemetry: Option<kitsu_providers::ProviderTelemetrySink>,
        ) -> Result<ChatStream, ProviderError> {
            let stream = self.chat_stream(req).await?;
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
                    remaining_deadline_ms: 1_000,
                });
            }
            Ok(stream)
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
        let bound_models = Arc::new(Mutex::new(Vec::<String>::new()));
        let binder_provider = provider.clone();
        let binder_models = bound_models.clone();
        let binder = ProviderBinder::new(move |model| {
            binder_models.lock().unwrap().push(model.to_string());
            Ok(binder_provider.clone())
        });
        let live: LiveModelWindow = Arc::new(Mutex::new(("model-A".to_string(), Some(128_000))));
        let (_d, handler) = spawn_agent_tool_with_binder(
            binder,
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
        assert_eq!(
            *bound_models.lock().unwrap(),
            ["model-B"],
            "one immutable provider binding is resolved per subagent run"
        );
    }

    #[tokio::test]
    async fn subagent_report_retains_one_immutable_route_generation() {
        use kitsu_providers::{
            ProviderAuthSource, ProviderCatalogPolicy, ProviderProfileId, ProviderRouteCurrent,
            ProviderRouteDeclaration, ProviderRoutePolicy, ProviderRouteSnapshot,
        };
        use std::time::Duration;

        let runtime = Arc::new(MockProvider {
            rounds: Mutex::new(vec![vec![
                ChatDelta::Text("route observed".to_string()),
                ChatDelta::Done(FinishReason::Stop),
            ]]),
            last_model: Mutex::new(None),
        });
        let declaration = ProviderRouteDeclaration::compatible_chat_v1(
            ProviderProfileId::derive_v1(b"subagent-route-test", b"primary").unwrap(),
            false,
        );
        let policy = ProviderRoutePolicy::new(
            0,
            0,
            Duration::from_secs(30),
            Duration::from_secs(30),
            Duration::from_secs(2),
            ProviderCatalogPolicy::Unavailable,
        )
        .unwrap();
        let snapshot = ProviderRouteSnapshot::initial(
            declaration,
            "model-A",
            ProviderAuthSource::None,
            policy,
            runtime.clone(),
            runtime,
        )
        .unwrap();
        let provider = Arc::new(
            ProviderRouteCurrent::new(snapshot)
                .bind_model("model-B")
                .unwrap(),
        );
        let report = run_subagent_report(
            &provider,
            &Arc::new(Registry::new()),
            &AgentConfig {
                model: "model-B".to_string(),
                stream: false,
                ..Default::default()
            },
            "",
            "inspect route".to_string(),
        )
        .await;

        assert_eq!(report.result.unwrap(), "route observed");
        assert_eq!(report.telemetry.route_generation, Some(1));
        assert!(!report.telemetry.route_generation_mismatch);
        assert!(report
            .telemetry
            .recent_activity
            .iter()
            .any(|activity| { activity.contains("route generation 1") }));
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
    fn event_summary_preserves_first_route_generation_and_exposes_drift() {
        let attempt = |route_generation| AgentRuntimeEvent::ProviderAttempt {
            round: 1,
            purpose: kitsu_core::ProviderRequestPurpose::Inference,
            route_generation: Some(route_generation),
            attempt: 1,
            max_attempts: 1,
            category: ProviderAttemptCategory::Success,
            status: Some(200),
            outcome: ProviderAttemptOutcome::BodyCompleted,
            terminal: true,
            retry_delay_ms: None,
            retry_after_used: false,
            deadline_scope: kitsu_core::ProviderDeadlineScope::ProviderRequest,
            remaining_deadline_ms: 1_000,
            turn_remaining_ms: None,
        };
        let telemetry = summarize_events(
            "safe-model".to_string(),
            vec![attempt(4), attempt(4), attempt(5)],
            None,
        );

        assert_eq!(telemetry.route_generation, Some(4));
        assert!(telemetry.route_generation_mismatch);
        let activity = telemetry.recent_activity.join("\n");
        assert!(activity.contains("route generation 4"));
        assert!(activity.contains("route generation 5"));
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
            AgentError::Provider(ProviderError::RouteContract(
                ProviderRouteContractError::CurrentRouteUnavailable,
            )),
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

    /// Regression: acknowledgement is a STATE TRANSITION, and it must survive
    /// the release profile.
    ///
    /// Both acknowledgement paths once called the mutating
    /// `acknowledge_handoff()` from inside a `debug_assert!`. That macro does
    /// not evaluate its expression when debug assertions are off, so in the
    /// binary users actually run the transition never happened: a fetched
    /// result stayed `Ready` forever, `acknowledge_result` never reported
    /// `AlreadyAcknowledged`, and every `take_unobserved_terminal_results` call
    /// re-returned results already drained — re-printing finished agent output
    /// at shutdown. Debug CI stayed green throughout.
    ///
    /// This test asserts the observable transition rather than the mechanism.
    /// Note it can only FAIL under `--release`: with debug assertions on, the
    /// macro does evaluate its expression and the side effect still happens, so
    /// the bug is invisible in a debug run by construction. No test can close
    /// that gap from inside the debug profile — which is why the workflow runs
    /// the suite in both profiles. Verified by reintroducing the bug: debug
    /// passed, release failed.
    #[test]
    fn acknowledgement_transition_survives_the_release_profile() {
        let registry = AgentRegistry::new();
        let id = registry.start("t".to_string()).unwrap();
        registry.finish(id, Ok("done".to_string()));

        assert_eq!(registry.handoff_state(id), Some(AgentHandoffState::Ready));
        assert_eq!(
            registry.acknowledge_result(id),
            AgentHandoffAckOutcome::Acknowledged
        );
        // The transition must be durable, not merely reported.
        assert_eq!(
            registry.handoff_state(id),
            Some(AgentHandoffState::Acknowledged),
            "acknowledge_result must actually transition the state"
        );
        assert_eq!(
            registry.acknowledge_result(id),
            AgentHandoffAckOutcome::AlreadyAcknowledged
        );
        assert!(
            registry.take_unobserved_terminal_results().is_empty(),
            "an acknowledged result must never resurface as unobserved"
        );

        // The drain path owns the same transition.
        let drained = registry.start("u".to_string()).unwrap();
        registry.finish(drained, Ok("done".to_string()));
        assert_eq!(registry.take_unobserved_terminal_results().len(), 1);
        assert_eq!(
            registry.handoff_state(drained),
            Some(AgentHandoffState::Acknowledged),
            "draining must mark the result observed"
        );
        assert!(
            registry.take_unobserved_terminal_results().is_empty(),
            "a drained result must not be returned twice"
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
    async fn unattached_fault_fixture_reports_typed_unjoined_residue() {
        let registry = AgentRegistry::new();
        // Production admission uses spawn_background_thread and installs its
        // handle atomically. The test-only split start is retained precisely to
        // inject the malformed state that a cleanup contract must not call
        // joined.
        let id = registry.start("unattached fault".to_string()).unwrap();
        let cancelled = registry.cancel(id).await;
        let AgentCancelOutcome::Cancelled(cancelled) = cancelled else {
            panic!("unattached running record should cancel: {cancelled:?}");
        };
        assert!(!cancelled.joined);
        assert!(matches!(cancelled.status, AgentStatus::Cancelled(_)));
        assert_eq!(registry.snapshot().running, 0);
        assert_eq!(registry.snapshot().terminal, 1);
    }

    #[tokio::test]
    async fn rollback_cancellation_status_preserves_unjoined_fault_truth() {
        let registry = AgentRegistry::new();
        let id = registry
            .start("unattached rollback fault".to_string())
            .unwrap();

        let cancelled = registry.cancel_started_after_with_status(0).await;

        assert_eq!(cancelled.len(), 1);
        assert_eq!(cancelled[0].id, id);
        assert!(!cancelled[0].joined);
        assert!(matches!(cancelled[0].status, AgentStatus::Cancelled(_)));
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

    #[test]
    fn fresh_registry_snapshot_has_no_unacknowledged_or_blocked_agents() {
        let registry = AgentRegistry::new();
        let snapshot = registry.snapshot();
        assert_eq!(snapshot.unacknowledged, 0);
        assert_eq!(snapshot.blocked, 0);
    }

    #[test]
    fn a_done_agent_is_published_as_terminal_and_unacknowledged() {
        let registry = AgentRegistry::new();
        let mut changes = registry.subscribe();
        let id = registry.start("ambient status".to_string()).unwrap();
        registry.finish(id, Ok("done".to_string()));

        let snapshot = *changes.borrow_and_update();
        assert_eq!(snapshot.terminal, 1);
        assert_eq!(snapshot.unacknowledged, 1);
        assert_eq!(snapshot.running, 0);
        assert_eq!(registry.handoff_state(id), Some(AgentHandoffState::Ready));
    }

    /// Headline T1-S1 behaviour: acknowledging a result must publish a NEW
    /// snapshot whose `unacknowledged` has dropped, while the record itself is
    /// retained and still counted as terminal.
    #[test]
    fn acknowledging_a_result_publishes_a_snapshot_with_no_unacknowledged_agents() {
        let registry = AgentRegistry::new();
        let mut changes = registry.subscribe();
        let id = registry.start("needs an ack".to_string()).unwrap();
        registry.finish(id, Ok("done".to_string()));

        let before = *changes.borrow_and_update();
        assert_eq!(before.terminal, 1);
        assert_eq!(
            before.unacknowledged, 1,
            "an unread result is unacknowledged"
        );

        assert_eq!(
            registry.acknowledge_result(id),
            AgentHandoffAckOutcome::Acknowledged
        );

        let after = *changes.borrow_and_update();
        assert!(
            after.generation > before.generation,
            "acknowledgement must publish a new snapshot"
        );
        assert_eq!(
            after.unacknowledged, 0,
            "an acknowledged result is no longer unacknowledged"
        );
        assert_eq!(
            after.terminal, 1,
            "the record is retained, so it is still terminal"
        );
        assert_eq!(registry.snapshot(), after);
    }

    #[test]
    fn reading_a_result_clears_unacknowledged_but_peeking_does_not() {
        let registry = AgentRegistry::new();
        let id = registry.start("peek then read".to_string()).unwrap();
        registry.finish(id, Ok("payload".to_string()));

        assert_eq!(registry.peek_result(id).as_deref(), Some("payload"));
        assert_eq!(
            registry.snapshot().unacknowledged,
            1,
            "peek_result must not acknowledge the handoff"
        );

        assert_eq!(registry.result(id).as_deref(), Some("payload"));
        assert_eq!(
            registry.snapshot().unacknowledged,
            0,
            "result() acknowledges implicitly"
        );
        assert_eq!(registry.snapshot().terminal, 1);
    }

    #[test]
    fn a_failed_agent_counts_as_unacknowledged_until_acknowledged() {
        let registry = AgentRegistry::new();
        let id = registry.start("will fail".to_string()).unwrap();
        registry.finish(id, Err("boom".to_string()));

        let snapshot = registry.snapshot();
        assert_eq!(snapshot.failed, 1);
        assert_eq!(snapshot.unacknowledged, 1);

        assert_eq!(
            registry.acknowledge_result(id),
            AgentHandoffAckOutcome::Acknowledged
        );
        let snapshot = registry.snapshot();
        assert_eq!(snapshot.failed, 1, "the failure is still a failure");
        assert_eq!(
            snapshot.unacknowledged, 0,
            "failed and unacknowledged are independent counts"
        );
    }

    #[test]
    fn draining_unobserved_terminal_results_clears_unacknowledged() {
        let registry = AgentRegistry::new();
        let done = registry.start("done work".to_string()).unwrap();
        let failed = registry.start("failed work".to_string()).unwrap();
        registry.finish(done, Ok("ok".to_string()));
        registry.finish(failed, Err("bad".to_string()));
        assert_eq!(registry.snapshot().unacknowledged, 2);

        assert_eq!(registry.take_unobserved_terminal_results().len(), 2);
        let snapshot = registry.snapshot();
        assert_eq!(snapshot.unacknowledged, 0);
        assert_eq!(snapshot.terminal, 2);
    }

    #[test]
    fn one_of_two_terminal_agents_acknowledged_leaves_one_unacknowledged() {
        let registry = AgentRegistry::new();
        let first = registry.start("first".to_string()).unwrap();
        let second = registry.start("second".to_string()).unwrap();
        registry.finish(first, Ok("one".to_string()));
        registry.finish(second, Ok("two".to_string()));
        assert_eq!(registry.snapshot().unacknowledged, 2);

        assert_eq!(
            registry.acknowledge_result(first),
            AgentHandoffAckOutcome::Acknowledged
        );
        let snapshot = registry.snapshot();
        assert_eq!(snapshot.terminal, 2);
        assert_eq!(snapshot.unacknowledged, 1);
    }

    /// Guard: `blocked` counts agents parked on a user answer, and nothing
    /// else. `park_prompt` is its only writer, so an agent this test never
    /// parks — running or terminal — must leave every snapshot publishing
    /// zero. A status that leaked into the count would misreport the
    /// `needs input` group.
    #[test]
    fn blocked_is_zero_for_every_agent_this_test_never_parks() {
        let registry = AgentRegistry::new();
        let running = registry.start("still running".to_string()).unwrap();
        assert_eq!(
            registry.snapshot().blocked,
            0,
            "a running agent is not blocked"
        );

        let done = registry.start("done".to_string()).unwrap();
        registry.finish(done, Ok("ok".to_string()));
        assert_eq!(
            registry.snapshot().blocked,
            0,
            "a terminal agent is not blocked"
        );

        assert_eq!(
            registry.acknowledge_result(done),
            AgentHandoffAckOutcome::Acknowledged
        );
        assert_eq!(registry.snapshot().blocked, 0);
        let _ = running;
    }

    #[test]
    fn closing_a_terminal_record_leaves_no_stale_unacknowledged_count() {
        let registry = AgentRegistry::new();
        let kept = registry.start("kept".to_string()).unwrap();
        let closed = registry.start("closed".to_string()).unwrap();
        registry.finish(kept, Ok("kept result".to_string()));
        registry.finish(closed, Ok("closed result".to_string()));
        assert_eq!(registry.snapshot().unacknowledged, 2);

        let AgentPruneOutcome::Pruned(_) = registry.close(closed) else {
            panic!("a terminal record closes");
        };
        let snapshot = registry.snapshot();
        assert_eq!(snapshot.terminal, 1);
        assert_eq!(
            snapshot.unacknowledged, 1,
            "a closed record must not keep counting as unacknowledged"
        );
        assert_eq!(snapshot.blocked, 0);

        let AgentPruneOutcome::Pruned(_) = registry.prune(kept) else {
            panic!("a terminal record prunes");
        };
        let snapshot = registry.snapshot();
        assert_eq!(snapshot.terminal, 0);
        assert_eq!(snapshot.unacknowledged, 0);
    }

    // ---------------------------------------------------------------------
    // T2-S4: the registry's receiving side for parked permission requests
    // (design B1/B3/B4). These are failing-first tests; `prompt_sender` and
    // `answer` are STUBS until the implementer lands the receiver.
    // ---------------------------------------------------------------------

    use kitsu_tools::{
        Action, PermissionRequest, PromptRequest, PromptRequestPayload, ToolInterruption,
    };

    /// The exact request a path gate would park for `agent_id`.
    fn park_path_request(
        sender: &kitsu_tools::PromptRequestSender,
        agent_id: u64,
    ) -> tokio::sync::oneshot::Receiver<Decision> {
        let (reply, answer) = tokio::sync::oneshot::channel();
        sender
            .send(PromptRequest {
                agent_id,
                payload: PromptRequestPayload::Path(PermissionRequest {
                    action: Action::Write,
                    path: "/work/notes.txt".to_string(),
                    grant_dir: "/work".to_string(),
                }),
                reply,
            })
            .expect("the registry owns the receiver and keeps it open");
        answer
    }

    fn progress_status(registry: &AgentRegistry, id: u64) -> Option<AgentProgressStatus> {
        registry
            .progress_snapshot()
            .agents
            .into_iter()
            .find(|agent| agent.id == id)
            .map(|agent| agent.status)
    }

    /// Yield until the registry has observed the parked request. No sleeping,
    /// so this is safe under `start_paused`.
    async fn await_blocked(registry: &AgentRegistry, id: u64) {
        for _ in 0..1_000 {
            if progress_status(registry, id) == Some(AgentProgressStatus::Blocked) {
                return;
            }
            tokio::task::yield_now().await;
        }
        panic!("agent #{id} never became blocked after a request was parked");
    }

    async fn answered_registry(decision: Decision) -> (Arc<AgentRegistry>, u64, Decision) {
        let registry = AgentRegistry::new();
        let id = registry.start("needs an answer".to_string()).unwrap();
        let sender = registry.prompt_sender();
        let answer = park_path_request(&sender, id);
        await_blocked(&registry, id).await;

        assert_eq!(
            registry.answer(id, decision),
            AgentAnswerOutcome::Answered,
            "a blocked agent accepts an answer"
        );
        let resolved = answer.await.expect("the gate's oneshot is resolved");
        (registry, id, resolved)
    }

    #[tokio::test]
    async fn answering_a_blocked_agent_with_allow_once_resolves_its_gate_and_resumes_it() {
        let (registry, id, resolved) = answered_registry(Decision::AllowOnce).await;
        assert_eq!(
            resolved,
            Decision::AllowOnce,
            "the gate sees exactly the answer"
        );
        assert_eq!(
            progress_status(&registry, id),
            Some(AgentProgressStatus::Running)
        );
        assert_eq!(
            registry.snapshot().blocked,
            0,
            "an answered agent is no longer counted"
        );
        assert_eq!(
            registry.thread_snapshot(id).unwrap().phase,
            AgentThreadPhase::Running,
            "the thread phase leaves Blocked too"
        );
    }

    #[tokio::test]
    async fn answering_a_blocked_agent_with_allow_always_resolves_its_gate_and_resumes_it() {
        let (registry, id, resolved) = answered_registry(Decision::AllowAlways).await;
        assert_eq!(resolved, Decision::AllowAlways);
        assert_eq!(
            progress_status(&registry, id),
            Some(AgentProgressStatus::Running)
        );
        assert_eq!(registry.snapshot().blocked, 0);
    }

    #[tokio::test]
    async fn answering_a_blocked_agent_with_deny_resolves_its_gate_and_resumes_it() {
        let (registry, id, resolved) = answered_registry(Decision::Deny).await;
        assert_eq!(
            resolved,
            Decision::Deny,
            "deny is an answer, not an interruption: the agent keeps running"
        );
        assert_eq!(
            progress_status(&registry, id),
            Some(AgentProgressStatus::Running)
        );
        assert_eq!(registry.snapshot().blocked, 0);
    }

    #[tokio::test]
    async fn answering_an_agent_that_is_not_blocked_reports_why_and_changes_nothing() {
        let registry = AgentRegistry::new();
        let running = registry.start("running".to_string()).unwrap();
        let done = registry.start("done".to_string()).unwrap();
        registry.finish(done, Ok("ok".to_string()));

        assert_eq!(
            registry.answer(running, Decision::AllowOnce),
            AgentAnswerOutcome::NotBlocked,
            "a running agent has no parked request to answer"
        );
        assert_eq!(
            registry.answer(done, Decision::AllowOnce),
            AgentAnswerOutcome::NotBlocked,
            "a terminal agent has no parked request to answer"
        );
        assert_eq!(
            registry.answer(9_999, Decision::AllowOnce),
            AgentAnswerOutcome::NotFound,
            "an unknown id is distinguishable from a known, unblocked one"
        );

        assert_eq!(
            progress_status(&registry, running),
            Some(AgentProgressStatus::Running)
        );
        assert_eq!(
            progress_status(&registry, done),
            Some(AgentProgressStatus::Done)
        );
        assert_eq!(registry.snapshot().blocked, 0);
    }

    #[tokio::test]
    async fn a_parked_request_blocks_its_agent_and_the_blocked_count_is_real() {
        let registry = AgentRegistry::new();
        let blocked = registry.start("will block".to_string()).unwrap();
        let busy = registry.start("stays running".to_string()).unwrap();
        let sender = registry.prompt_sender();

        let before = registry.snapshot().generation;
        let _answer = park_path_request(&sender, blocked);
        await_blocked(&registry, blocked).await;

        assert_eq!(
            registry.snapshot().blocked,
            1,
            "AgentRegistrySnapshot::blocked is a real count, not a literal 0"
        );
        assert!(
            registry.snapshot().generation > before,
            "blocking publishes a new generation so watchers see it (B4)"
        );
        assert_eq!(
            progress_status(&registry, blocked),
            Some(AgentProgressStatus::Blocked)
        );
        assert_eq!(
            progress_status(&registry, busy),
            Some(AgentProgressStatus::Running)
        );
        assert_eq!(
            registry.thread_snapshot(blocked).unwrap().phase,
            AgentThreadPhase::Blocked,
            "the thread DTO phase blocks with the record (B5)"
        );
        let progress = registry.progress_snapshot();
        assert_eq!(progress.blocked, 1);
        assert_eq!(
            progress.running
                + progress.blocked
                + progress.done
                + progress.failed
                + progress.cancelled,
            progress.total,
            "the per-state counts still sum to total"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_blocked_agent_times_out_to_interrupt_after_the_default_block_timeout() {
        assert_eq!(
            DEFAULT_AGENT_BLOCK_TIMEOUT,
            std::time::Duration::from_secs(30 * 60),
            "B3's default is 30 minutes"
        );

        let registry = AgentRegistry::new();
        let id = registry.start("waits forever".to_string()).unwrap();
        let sender = registry.prompt_sender();
        let mut answer = park_path_request(&sender, id);
        await_blocked(&registry, id).await;

        tokio::time::advance(DEFAULT_AGENT_BLOCK_TIMEOUT - std::time::Duration::from_secs(1)).await;
        tokio::task::yield_now().await;
        assert!(
            answer.try_recv().is_err(),
            "the request is still parked one second before the timeout"
        );
        assert_eq!(registry.snapshot().blocked, 1);

        tokio::time::advance(std::time::Duration::from_secs(2)).await;
        assert_eq!(
            answer
                .await
                .expect("the timeout resolves the gate rather than dropping it"),
            Decision::Interrupt,
            "B3: the timeout answers Interrupt, which the gates map to AuthorityInputClosed"
        );
        assert_eq!(
            registry.snapshot().blocked,
            0,
            "a timed-out agent is no longer blocked"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_timed_out_blocked_agent_ends_as_failed_interrupted() {
        let registry = AgentRegistry::new();
        let sender = registry.prompt_sender();
        let gate_sender = sender.clone();
        // Stands in for the gate: park the request, then map the answer the way
        // `Permissions::resolve` already maps `Decision::Interrupt`.
        let id = registry
            .spawn_background("gated write".to_string(), async move {
                let (reply, answer) = tokio::sync::oneshot::channel();
                let _ = gate_sender.send(PromptRequest {
                    agent_id: 1,
                    payload: PromptRequestPayload::Path(PermissionRequest {
                        action: Action::Write,
                        path: "/work/notes.txt".to_string(),
                        grant_dir: "/work".to_string(),
                    }),
                    reply,
                });
                match answer.await.unwrap_or(Decision::Interrupt) {
                    Decision::Interrupt => Err(ToolInterruption::AuthorityInputClosed.to_string()),
                    other => Ok(format!("{other:?}")),
                }
            })
            .unwrap();
        await_blocked(&registry, id).await;

        tokio::time::advance(DEFAULT_AGENT_BLOCK_TIMEOUT + std::time::Duration::from_secs(1)).await;
        for _ in 0..1_000 {
            if progress_status(&registry, id) == Some(AgentProgressStatus::Failed) {
                break;
            }
            tokio::task::yield_now().await;
        }
        let status = registry
            .take_unobserved_terminal_results()
            .into_iter()
            .find(|agent| agent.id == id)
            .map(|agent| agent.status)
            .expect("the timed-out agent is retained");
        assert_eq!(
            status,
            AgentStatus::Failed(ToolInterruption::AuthorityInputClosed.to_string()),
            "B3: a headless run cannot hang; it fails as interrupted"
        );
        assert_eq!(registry.snapshot().blocked, 0);
    }

    #[tokio::test]
    async fn stopping_a_blocked_agent_resolves_its_pending_request_as_interrupt() {
        let registry = AgentRegistry::new();
        let id = registry.start("stopped while blocked".to_string()).unwrap();
        let sender = registry.prompt_sender();
        let answer = park_path_request(&sender, id);
        await_blocked(&registry, id).await;

        let outcome = registry.cancel(id).await;
        assert!(
            matches!(outcome, AgentCancelOutcome::Cancelled(_)),
            "/jobs stop stops a blocked agent, it is not already terminal: {outcome:?}"
        );
        assert_eq!(
            answer
                .await
                .expect("stop resolves the gate rather than leaving it awaiting"),
            Decision::Interrupt,
            "/jobs stop on a blocked agent resolves Interrupt"
        );
        assert_eq!(registry.snapshot().blocked, 0);
        assert_eq!(
            progress_status(&registry, id),
            Some(AgentProgressStatus::Cancelled)
        );
    }

    #[tokio::test]
    async fn tearing_down_a_blocked_agent_leaves_no_request_parked() {
        let registry = AgentRegistry::new();
        let id = registry
            .start("torn down while blocked".to_string())
            .unwrap();
        let sender = registry.prompt_sender();
        let answer = park_path_request(&sender, id);
        await_blocked(&registry, id).await;

        let cancelled = registry.cancel_all().await;
        assert_eq!(
            cancelled.len(),
            1,
            "a blocked agent is cancellable like a running one"
        );
        assert_eq!(
            answer
                .await
                .expect("teardown resolves the gate rather than dropping it silently"),
            Decision::Interrupt
        );
        assert_eq!(
            registry.snapshot().blocked,
            0,
            "no request stays parked after teardown"
        );
        assert_eq!(
            registry.answer(id, Decision::AllowOnce),
            AgentAnswerOutcome::NotBlocked,
            "the parked request is gone, so a late answer finds nothing to resolve"
        );
    }

    // ---------------------------------------------------------------------
    // T2-S5a: `agents.blocking` / `agents.block_timeout_secs` reach the
    // registry, and the spawn path scopes a detached agent's tool work with
    // the registry route only under `ask` (design B1/B3, plan row T2-S5).
    //
    // FAILING-FIRST: `AgentRegistry::blocking`, `set_blocking`,
    // `block_timeout` and `set_block_timeout` do not exist yet, and the spawn
    // path still calls `scope_without_interactive_prompts` unconditionally.
    // ---------------------------------------------------------------------

    use crate::config::AgentBlockingMode;

    /// A registry built by any constructor is fail-closed with B3's 30-minute
    /// timeout until the run's config says otherwise. Nothing in this card
    /// moves either default.
    #[test]
    fn a_fresh_registry_is_fail_closed_with_the_thirty_minute_block_timeout() {
        let registry = AgentRegistry::new();
        assert_eq!(
            registry.blocking(),
            AgentBlockingMode::FailClosed,
            "blocking is opt-in: a registry nobody configured behaves exactly as today"
        );
        assert_eq!(
            registry.block_timeout(),
            DEFAULT_AGENT_BLOCK_TIMEOUT,
            "the constant stays the default even though the value is now configurable"
        );
        assert_eq!(
            AgentRegistry::with_limits(AgentRegistryLimits::default()).blocking(),
            AgentBlockingMode::FailClosed
        );
    }

    /// The point of making the timeout configurable: a parked request must
    /// resolve `Interrupt` after the *configured* duration, not after the
    /// hard-coded 30 minutes. Paused clock, so this is exact rather than slow.
    #[tokio::test(start_paused = true)]
    async fn a_configured_block_timeout_replaces_the_hard_coded_thirty_minutes() {
        let registry = AgentRegistry::new();
        registry.set_block_timeout(std::time::Duration::from_secs(2));
        assert_eq!(
            registry.block_timeout(),
            std::time::Duration::from_secs(2),
            "the configured value is what the registry reports"
        );

        let id = registry.start("waits two seconds".to_string()).unwrap();
        let sender = registry.prompt_sender();
        let mut answer = park_path_request(&sender, id);
        await_blocked(&registry, id).await;

        tokio::time::advance(std::time::Duration::from_secs(1)).await;
        tokio::task::yield_now().await;
        assert!(
            answer.try_recv().is_err(),
            "still parked one second into a two-second timeout"
        );

        // Advance past the configured two seconds but nowhere near 30 minutes,
        // and poll rather than `await` -- awaiting would let the paused clock
        // auto-advance to whatever timer is pending and pass either way.
        tokio::time::advance(std::time::Duration::from_secs(2)).await;
        let mut resolved = None;
        for _ in 0..1_000 {
            if let Ok(decision) = answer.try_recv() {
                resolved = Some(decision);
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(
            resolved,
            Some(Decision::Interrupt),
            "the timeout must fire at the configured duration, not at DEFAULT_AGENT_BLOCK_TIMEOUT"
        );
        assert_eq!(registry.snapshot().blocked, 0);
    }

    /// A tool registry whose single tool asks a REAL `Mode::Ask` path gate
    /// about an unlisted path. No prompter is attached, exactly as a
    /// background agent's permissions are reached today, so the whole
    /// behaviour under test is the prompt route.
    fn real_path_gate_tools(target: std::path::PathBuf) -> Arc<Registry> {
        let perms = Arc::new(
            kitsu_tools::Permissions::new(&[], &[], kitsu_tools::Mode::Ask)
                .expect("empty roots are valid"),
        );
        let mut tools = Registry::new();
        let handler: HandlerFn = Box::new(move |_args: String| {
            let perms = perms.clone();
            let target = target.clone();
            Box::pin(async move {
                perms
                    .check_read(&target)
                    .await
                    .map(|path| format!("read {}", path.display()))
            })
        });
        tools
            .register_with_descriptor(
                ToolDescriptor {
                    name: "gated_read".to_string(),
                    description: "Read one fixed path through the real permission gate".to_string(),
                    parameters: serde_json::json!({"type": "object", "properties": {}}),
                },
                handler,
            )
            .expect("fresh registry");
        Arc::new(tools)
    }

    /// Records every request body it is handed, so a test can assert what the
    /// gate handed back as the tool result. Round 1 calls `gated_read`; every
    /// later round ends the turn.
    struct GatedToolProvider {
        rounds: AtomicUsize,
        seen: Arc<Mutex<Vec<String>>>,
    }

    impl Provider for GatedToolProvider {
        async fn healthcheck(&self) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn chat_stream(&self, req: ChatRequest) -> Result<ChatStream, ProviderError> {
            for message in &req.messages {
                self.seen.lock().unwrap().push(message.content.clone());
            }
            let deltas = if self.rounds.fetch_add(1, AtomicOrdering::SeqCst) == 0 {
                vec![
                    ChatDelta::ToolCall(kitsu_providers::ToolCall {
                        tool_name: "gated_read".to_string(),
                        args: "{}".to_string(),
                        call_id: "gate-call-1".to_string(),
                    }),
                    ChatDelta::Done(FinishReason::ToolCalls),
                ]
            } else {
                vec![
                    ChatDelta::Text("GATED_AGENT_FINISHED".to_string()),
                    ChatDelta::Done(FinishReason::Stop),
                ]
            };
            Ok(Box::pin(stream::iter(
                deltas.into_iter().map(Ok::<_, ProviderError>),
            )))
        }
        async fn list_models(&self) -> Result<Option<Vec<String>>, ProviderError> {
            Ok(None)
        }
    }

    fn spawn_bg_against_gate(
        registry: Arc<AgentRegistry>,
        target: std::path::PathBuf,
        seen: Arc<Mutex<Vec<String>>>,
    ) -> HandlerFn {
        let mut cfg = sub_cfg();
        cfg.quiet = true;
        let live: LiveModelWindow = Arc::new(Mutex::new(("m".to_string(), None)));
        let (_descriptor, handler) = spawn_agent_bg_tool(
            Arc::new(GatedToolProvider {
                rounds: AtomicUsize::new(0),
                seen,
            }),
            real_path_gate_tools(target),
            cfg,
            live,
            String::new(),
            registry,
            empty_profile_runtime(),
            open_budget(),
        );
        handler
    }

    /// THE DEFAULT, and the most important assertion in this card: with
    /// `blocking = fail_closed` the detached agent's tool work is still
    /// scoped `Denied`, so the gate returns today's byte-identical message
    /// and nothing is ever parked.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn by_default_a_detached_agent_still_fails_closed_at_a_real_permission_gate() {
        let outside = tempfile::TempDir::new().expect("a directory no root covers");
        let target = outside.path().join("secret.txt");
        std::fs::write(&target, "secret").unwrap();

        let registry = AgentRegistry::new();
        assert_eq!(registry.blocking(), AgentBlockingMode::FailClosed);
        let seen = Arc::new(Mutex::new(Vec::new()));
        let handler = spawn_bg_against_gate(registry.clone(), target.clone(), seen.clone());

        handler(r#"{"task":"read the file"}"#.to_string())
            .await
            .expect("the spawn returns an id immediately");

        let mut changes = registry.subscribe();
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if changes.borrow_and_update().terminal == 1 {
                    break;
                }
                changes.changed().await.expect("registry alive");
            }
        })
        .await
        .expect("a fail-closed agent reaches a terminal state without any human");

        assert_eq!(
            registry.snapshot().blocked,
            0,
            "the default route parks nothing, so no agent is ever blocked"
        );
        let transcript = seen.lock().unwrap().join("\n");
        assert!(
            transcript.contains("background agents cannot open interactive permission prompts"),
            "the default must keep today's exact fail-closed message: {transcript}"
        );
    }

    /// Under `ask`, the same agent parks instead: the spawn path scopes its
    /// tool work with `scope_with_registry_prompts`, so the real gate reaches
    /// the real registry and the blocked count is real (B1, B4).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn under_ask_a_detached_agent_parks_at_a_real_permission_gate() {
        let outside = tempfile::TempDir::new().expect("a directory no root covers");
        let target = outside.path().join("secret.txt");
        std::fs::write(&target, "secret").unwrap();

        let registry = AgentRegistry::new();
        registry.set_blocking(AgentBlockingMode::Ask);
        let seen = Arc::new(Mutex::new(Vec::new()));
        let handler = spawn_bg_against_gate(registry.clone(), target.clone(), seen.clone());

        handler(r#"{"task":"read the file"}"#.to_string())
            .await
            .expect("the spawn returns an id immediately");

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while registry.snapshot().blocked == 0 {
            assert!(
                std::time::Instant::now() < deadline,
                "the agent never parked: the spawn path is not using the registry route"
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }

        assert_eq!(registry.snapshot().blocked, 1);
        assert_eq!(
            progress_status(&registry, 1),
            Some(AgentProgressStatus::Blocked),
            "/jobs sees it under `needs input`"
        );
        assert_eq!(
            registry.thread_snapshot(1).unwrap().phase,
            AgentThreadPhase::Blocked
        );
        assert!(
            !seen
                .lock()
                .unwrap()
                .iter()
                .any(|message| message.contains("cannot open interactive permission prompts")),
            "a parked gate must not also hand the model the fail-closed error"
        );

        // The answer travels the real route back into the real gate.
        assert_eq!(
            registry.answer(1, Decision::AllowOnce),
            AgentAnswerOutcome::Answered
        );
        let mut changes = registry.subscribe();
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if changes.borrow_and_update().terminal == 1 {
                    break;
                }
                changes.changed().await.expect("registry alive");
            }
        })
        .await
        .expect("an answered agent resumes and finishes");
        assert_eq!(registry.snapshot().blocked, 0);
        let result = registry.result(1).expect("agent #1 exists");
        assert!(result.contains("GATED_AGENT_FINISHED"), "{result}");
    }

    /// `ask` with a short timeout is the whole headless story: nobody
    /// answers, the configured timer fires, and the agent ends
    /// `Failed(interrupted)` through the real gate's `AuthorityInputClosed`
    /// mapping (B3).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn under_ask_an_unanswered_real_gate_times_out_to_failed_interrupted() {
        let outside = tempfile::TempDir::new().expect("a directory no root covers");
        let target = outside.path().join("secret.txt");
        std::fs::write(&target, "secret").unwrap();

        let registry = AgentRegistry::new();
        registry.set_blocking(AgentBlockingMode::Ask);
        registry.set_block_timeout(std::time::Duration::from_millis(250));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let handler = spawn_bg_against_gate(registry.clone(), target.clone(), seen);

        handler(r#"{"task":"read the file"}"#.to_string())
            .await
            .expect("the spawn returns an id immediately");

        let mut changes = registry.subscribe();
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if changes.borrow_and_update().terminal == 1 {
                    break;
                }
                changes.changed().await.expect("registry alive");
            }
        })
        .await
        .expect("an unanswered blocked agent must time out rather than hang");

        let status = registry
            .take_unobserved_terminal_results()
            .into_iter()
            .find(|agent| agent.id == 1)
            .map(|agent| agent.status)
            .expect("the timed-out agent is retained");
        match status {
            AgentStatus::Failed(reason) => assert!(
                reason.contains(&ToolInterruption::AuthorityInputClosed.to_string()),
                "B3 maps the timeout to AuthorityInputClosed: {reason}"
            ),
            other => panic!("expected Failed(interrupted), got {other:?}"),
        }
        assert_eq!(registry.snapshot().blocked, 0);
    }

    // ---------------------------------------------------------------------
    // T2-S6: the approval record. The registry's park/resolve paths record
    // nothing today, so every test below this banner is expected to fail.
    // ---------------------------------------------------------------------

    fn approval_entries(registry: &AgentRegistry, id: u64) -> Vec<String> {
        registry
            .thread_snapshot(id)
            .expect("the agent is retained")
            .timeline
            .entries
            .into_iter()
            .filter(|entry| entry.kind == thread::AgentTimelineKind::Approval)
            .map(|entry| entry.text)
            .collect()
    }

    /// The argv/cwd a command gate would park. Every argument here is a
    /// plausible leak target: none of them is touched by
    /// `sanitize_and_redact_text` (proved in `thread.rs`).
    fn park_command_request(
        sender: &kitsu_tools::PromptRequestSender,
        agent_id: u64,
    ) -> tokio::sync::oneshot::Receiver<Decision> {
        let (reply, answer) = tokio::sync::oneshot::channel();
        sender
            .send(PromptRequest {
                agent_id,
                payload: PromptRequestPayload::Command(kitsu_tools::CommandApprovalRequest {
                    argv: vec!["rm".to_string(), "-rf".to_string(), "/work/tmp".to_string()],
                    cwd: "/work".to_string(),
                    timeout_ms: 1_000,
                    executable: kitsu_tools::CommandExecutableIdentity {
                        invocation: "rm".to_string(),
                        resolution: kitsu_tools::CommandExecutableResolution::InvocationPath,
                        observed_path: Some("/usr/bin/rm".to_string()),
                        pinned: false,
                    },
                    ambient_authority: kitsu_tools::CommandAmbientAuthority {
                        inherits_environment: false,
                        filesystem_confined_to_command_roots: true,
                        network_confined: true,
                        executable_identity_pinned: false,
                    },
                }),
                reply,
            })
            .expect("the registry owns the receiver and keeps it open");
        answer
    }

    /// Parking writes a request entry naming the permission kind, and only
    /// the kind: the path `/work/notes.txt` and the grant dir `/work` that
    /// `park_path_request` sends must not reach the timeline (N8, and the
    /// documented absolute in `docs/functionality.md` that tool arguments are
    /// never copied into the inspection record).
    #[tokio::test]
    async fn parking_a_path_request_records_an_argument_free_approval_entry() {
        let registry = AgentRegistry::new();
        let id = registry.start("needs an answer".to_string()).unwrap();
        let sender = registry.prompt_sender();
        let _answer = park_path_request(&sender, id);
        await_blocked(&registry, id).await;

        let approvals = approval_entries(&registry, id);
        assert_eq!(
            approvals.len(),
            1,
            "the park leaves exactly one request entry: {approvals:?}"
        );
        assert!(
            approvals[0].contains("write a file"),
            "the entry says what was asked for: {approvals:?}"
        );
        let encoded = serde_json::to_string(&registry.thread_snapshot(id).unwrap()).unwrap();
        for argument in ["notes.txt", "/work"] {
            assert!(
                !encoded.contains(argument),
                "the approval record leaked a tool argument ({argument}): {encoded}"
            );
        }
    }

    /// The command gate's kind maps distinctly, and its argv stays out.
    #[tokio::test]
    async fn parking_a_command_request_records_the_command_kind_without_its_argv() {
        let registry = AgentRegistry::new();
        let id = registry.start("needs an answer".to_string()).unwrap();
        let sender = registry.prompt_sender();
        let _answer = park_command_request(&sender, id);
        await_blocked(&registry, id).await;

        let approvals = approval_entries(&registry, id);
        assert_eq!(approvals.len(), 1, "{approvals:?}");
        assert!(
            approvals[0].contains("run a command"),
            "a command gate is a distinct kind: {approvals:?}"
        );
        // The entry text is pinned *exactly*, which is the strongest form of
        // this assertion: if the text is this and nothing else, no argument can
        // be inside it, whatever the argv happened to be. A substring ban
        // cannot say that, and a bare one is worse than useless here -- `"rm"`
        // also forbids the words permission, format, confirm and firmware, so
        // it constrains the product's wording by accident rather than
        // protecting anything (F41).
        assert_eq!(
            approvals[0], "approval to run a command requested",
            "the entry says the kind and nothing else: {approvals:?}"
        );
        // The snapshot-wide sweep stays, because an argument could leak into
        // some *other* field, but it uses tokens distinctive enough that only a
        // real leak matches.
        let encoded = serde_json::to_string(&registry.thread_snapshot(id).unwrap()).unwrap();
        for argument in ["-rf", "/work/tmp", "/usr/bin/rm", "rm -rf"] {
            assert!(
                !encoded.contains(argument),
                "the approval record leaked an argv word ({argument}): {encoded}"
            );
        }
    }

    /// Every human answer completes the record with its own spelling.
    #[tokio::test]
    async fn answering_a_blocked_agent_completes_its_approval_record() {
        for (decision, expected) in [
            (Decision::AllowOnce, "allowed once"),
            (Decision::AllowAlways, "allowed always"),
            (Decision::Deny, "denied"),
        ] {
            let (registry, id, _resolved) = answered_registry(decision).await;
            let approvals = approval_entries(&registry, id);
            assert_eq!(
                approvals.len(),
                2,
                "{decision:?}: a request and its decision: {approvals:?}"
            );
            assert!(
                approvals[1].contains(expected),
                "{decision:?} is recorded as {expected:?}: {approvals:?}"
            );
        }
    }

    /// B3's timeout is the one decision taken with no human present, so it
    /// must leave a record too, spelled as a timeout rather than a denial.
    #[tokio::test(start_paused = true)]
    async fn a_timed_out_approval_is_recorded_as_timed_out() {
        let registry = AgentRegistry::new();
        let id = registry.start("never answered".to_string()).unwrap();
        let sender = registry.prompt_sender();
        let answer = park_path_request(&sender, id);
        await_blocked(&registry, id).await;

        tokio::time::advance(DEFAULT_AGENT_BLOCK_TIMEOUT + std::time::Duration::from_secs(1)).await;
        assert_eq!(
            answer.await.expect("the timeout resolves the gate"),
            Decision::Interrupt
        );

        let approvals = approval_entries(&registry, id);
        assert_eq!(
            approvals.len(),
            2,
            "an unanswered park is still a decided request: {approvals:?}"
        );
        assert!(
            approvals[1].contains("timed out"),
            "a timeout is not a denial: {approvals:?}"
        );
    }

    // ---------------------------------------------------------------------
    // F43: a cancellation is recorded as a cancellation, not as a timeout.
    //
    // Found while writing these tests, and WIDER than the finding states:
    // `cancel_matching` does not go through `resolve_prompt` at all. It
    // takes `pending_prompt` itself and sends `Decision::Interrupt` on the
    // reply channel directly, so today a cancelled park records **no
    // outcome entry whatsoever** — the request entry is left dangling. The
    // first assertion each test below makes is therefore a count.
    //
    // STUB: `AgentApprovalOutcome::Cancelled` does not exist, so these
    // tests fail to COMPILE until it does.
    // ---------------------------------------------------------------------

    /// `/jobs stop <id>` on a parked agent. The record must say the approval
    /// was cancelled, and the exact-equality assertion is what keeps it from
    /// being recorded as the timeout it is not.
    #[tokio::test]
    async fn cancelling_a_blocked_agent_records_the_approval_as_cancelled() {
        let registry = AgentRegistry::new();
        let id = registry
            .start("cancelled while blocked".to_string())
            .unwrap();
        let sender = registry.prompt_sender();
        let answer = park_path_request(&sender, id);
        await_blocked(&registry, id).await;

        assert!(matches!(
            registry.cancel(id).await,
            AgentCancelOutcome::Cancelled(_)
        ));

        let approvals = approval_entries(&registry, id);
        assert_eq!(
            approvals,
            vec![
                "approval to write a file requested".to_string(),
                "approval to write a file cancelled".to_string(),
            ],
            "a deliberate cancellation is recorded as a cancellation"
        );

        // Requirement 4: the cancellation itself must not regress.
        assert_eq!(
            answer
                .await
                .expect("cancellation resolves the gate rather than dropping it"),
            Decision::Interrupt
        );
        assert_eq!(
            progress_status(&registry, id),
            Some(AgentProgressStatus::Cancelled)
        );
        assert_eq!(registry.snapshot().blocked, 0);
        assert_eq!(
            registry.answer(id, Decision::AllowOnce),
            AgentAnswerOutcome::NotBlocked
        );
    }

    /// `/jobs stop-all` takes the same path for a parked agent, so it must
    /// leave the same record.
    #[tokio::test]
    async fn cancelling_all_records_a_blocked_agent_s_approval_as_cancelled() {
        let registry = AgentRegistry::new();
        let id = registry.start("cancelled by stop-all".to_string()).unwrap();
        let sender = registry.prompt_sender();
        let answer = park_path_request(&sender, id);
        await_blocked(&registry, id).await;

        assert_eq!(registry.cancel_all().await.len(), 1);
        assert_eq!(
            approval_entries(&registry, id),
            vec![
                "approval to write a file requested".to_string(),
                "approval to write a file cancelled".to_string(),
            ],
        );
        assert_eq!(
            answer.await.expect("stop-all resolves the gate"),
            Decision::Interrupt
        );
    }

    /// The T2-S6 invariant, restated for the fix: EVERY resolution leaves
    /// exactly one outcome entry — never none, never two. A cancelled park
    /// still has B3's timer armed against it, and that timer later calls
    /// `resolve_prompt(.., Some(token), Interrupt)`; if the fix records at
    /// the cancel site without the timer's park check still rejecting the
    /// dead park, the entry would be followed by a second "timed out".
    #[tokio::test(start_paused = true)]
    async fn a_cancelled_park_records_exactly_one_outcome_even_after_its_timer_fires() {
        let registry = AgentRegistry::new();
        let id = registry
            .start("cancelled then timed out".to_string())
            .unwrap();
        let sender = registry.prompt_sender();
        let answer = park_path_request(&sender, id);
        await_blocked(&registry, id).await;

        assert!(matches!(
            registry.cancel(id).await,
            AgentCancelOutcome::Cancelled(_)
        ));
        assert_eq!(
            answer.await.expect("cancellation resolves the gate"),
            Decision::Interrupt
        );

        // Let the armed B3 timer fire on the park it no longer owns.
        tokio::time::advance(DEFAULT_AGENT_BLOCK_TIMEOUT + std::time::Duration::from_secs(1)).await;
        tokio::task::yield_now().await;

        assert_eq!(
            approval_entries(&registry, id),
            vec![
                "approval to write a file requested".to_string(),
                "approval to write a file cancelled".to_string(),
            ],
            "one park, one outcome: the late timer must not append a second \
             outcome nor rewrite the cancellation as a timeout"
        );
    }

    /// Requirement 2: the case that already worked must keep working, and it
    /// is the case with no human present. Exact equality, so the fix cannot
    /// blur the timeout into the new cancellation spelling either.
    #[tokio::test(start_paused = true)]
    async fn a_timed_out_approval_is_still_recorded_as_timed_out_exactly() {
        let registry = AgentRegistry::new();
        let id = registry.start("never answered".to_string()).unwrap();
        let sender = registry.prompt_sender();
        let answer = park_path_request(&sender, id);
        await_blocked(&registry, id).await;

        tokio::time::advance(DEFAULT_AGENT_BLOCK_TIMEOUT + std::time::Duration::from_secs(1)).await;
        assert_eq!(
            answer.await.expect("the timeout resolves the gate"),
            Decision::Interrupt
        );

        assert_eq!(
            approval_entries(&registry, id),
            vec![
                "approval to write a file requested".to_string(),
                "approval to write a file timed out".to_string(),
            ],
            "an unattended timeout stays a timeout"
        );
        assert_eq!(
            progress_status(&registry, id),
            Some(AgentProgressStatus::Running),
            "a timeout resumes the agent; it does not cancel it"
        );
    }

    /// An answered park is unaffected by the fix: `answer()` still routes
    /// through the shared path and records the human's decision.
    #[tokio::test]
    async fn answering_a_blocked_agent_is_not_recorded_as_a_cancellation() {
        for (decision, expected) in [
            (Decision::AllowOnce, "approval to write a file allowed once"),
            (
                Decision::AllowAlways,
                "approval to write a file allowed always",
            ),
            (Decision::Deny, "approval to write a file denied"),
        ] {
            let (registry, id, _resolved) = answered_registry(decision).await;
            assert_eq!(
                approval_entries(&registry, id),
                vec![
                    "approval to write a file requested".to_string(),
                    expected.to_string(),
                ],
                "{decision:?} is the human's answer, not a cancellation"
            );
        }
    }

    // ---------------------------------------------------------------------
    // T2-S7: the blocked state reaches the outside world. Two of the three
    // deliverable surfaces are assembled in this process and are exercised
    // here through the real park path, so a path and an argv word genuinely
    // enter the process before each absence is asserted.
    //
    // STUBS: `AgentThreadState::snapshot` publishes `waiting_for: None`,
    // `agent_progress` sets `waiting_for: None`, `AgentApprovalKind::wire`
    // returns "", and `agent_hook::observations` reports the last activity
    // label for a blocked agent. Every test below fails until those land.
    // ---------------------------------------------------------------------

    /// The state-file payload for this registry, as the session loop builds it.
    fn state_payload(registry: &AgentRegistry, turn_active: bool) -> serde_json::Value {
        let built = crate::agent_state_file::build(
            registry,
            4242,
            "/workspace/kitsu".to_string(),
            Some("sess-7".to_string()),
            turn_active,
            "2026-10-14T12:00:00Z".to_string(),
        );
        serde_json::to_value(&built).expect("the state file payload serializes")
    }

    fn thread_in(payload: &serde_json::Value, id: u64) -> serde_json::Value {
        payload["threads"]
            .as_array()
            .expect("threads is an array")
            .iter()
            .find(|dto| dto["thread"]["id"] == id)
            .unwrap_or_else(|| panic!("no thread record for agent #{id} in {payload}"))
            .clone()
    }

    /// Surface 1, the positive half: a parked agent publishes the process
    /// state `blocked`, its tally, and -- per agent -- the permission *kind*
    /// it waits on, spelled the one machine way.
    #[tokio::test]
    async fn the_state_file_publishes_the_kind_a_parked_agent_waits_on() {
        let registry = AgentRegistry::new();
        let id = registry.start("needs an answer".to_string()).unwrap();
        let sender = registry.prompt_sender();
        let _answer = park_path_request(&sender, id);
        await_blocked(&registry, id).await;

        let payload = state_payload(&registry, true);
        assert_eq!(
            payload["state"], "blocked",
            "a process with a parked agent is blocked, not working: {payload}"
        );
        assert_eq!(payload["counts"]["blocked"], 1, "{payload}");
        let thread = thread_in(&payload, id);
        assert_eq!(thread["thread"]["phase"], "blocked", "{thread}");
        assert_eq!(
            thread["thread"]["waiting_for"], "write_file",
            "the parked agent publishes its permission kind: {thread}"
        );
        assert_eq!(
            thread["thread"]["waiting_for"],
            thread::AgentApprovalKind::WriteFile.wire(),
            "the state file uses `wire()`, not a vocabulary of its own"
        );
    }

    /// And the released half: answering the request takes `waiting_for` away
    /// entirely rather than leaving a stale reason behind for a poller.
    #[tokio::test]
    async fn answering_a_parked_agent_removes_waiting_for_from_the_state_file() {
        let registry = AgentRegistry::new();
        let id = registry.start("needs an answer".to_string()).unwrap();
        let sender = registry.prompt_sender();
        let answer = park_path_request(&sender, id);
        await_blocked(&registry, id).await;
        assert_eq!(
            thread_in(&state_payload(&registry, true), id)["thread"]["waiting_for"],
            "write_file",
            "the blocked half must hold before the released half means anything"
        );

        assert_eq!(
            registry.answer(id, Decision::AllowOnce),
            AgentAnswerOutcome::Answered
        );
        let _ = answer.await;

        let payload = state_payload(&registry, true);
        assert_eq!(payload["counts"]["blocked"], 0, "{payload}");
        assert_eq!(payload["state"], "working", "{payload}");
        let thread = thread_in(&payload, id);
        assert_eq!(thread["thread"]["phase"], "running", "{thread}");
        assert!(
            thread["thread"].get("waiting_for").is_none(),
            "a released agent carries no waiting_for key at all: {thread}"
        );
    }

    /// Surface 1's leak test. The path `/work/notes.txt` and the argv
    /// `rm -rf /work/tmp` are sent through the real `PromptRequestSender`, so
    /// they are inside the process and one function call from the writer.
    ///
    /// Not vacuous, for three separate reasons, each asserted rather than
    /// argued:
    ///   * the writer *can* emit such a string -- a payload whose `cwd` is
    ///     `/work/notes.txt` serializes with it present, so the sweep below is
    ///     searching a surface that would show a leak;
    ///   * the park really happened -- `waiting_for` is pinned to the kind
    ///     first, so an empty or absent record cannot pass the sweep by
    ///     carrying nothing at all;
    ///   * the exact-equality assertion is the primary guard (F41): the field
    ///     *is* the kind's word, so no argument can be inside it whatever the
    ///     payload was. The sweep only catches a leak into some *other* field,
    ///     and uses tokens distinctive enough that only a real leak matches.
    #[tokio::test]
    async fn the_state_file_never_carries_a_parked_path_or_argv_word() {
        let probe = crate::agent_state_file::build(
            &AgentRegistry::new(),
            4242,
            "/work/notes.txt".to_string(),
            None,
            false,
            "2026-10-14T12:00:00Z".to_string(),
        );
        assert!(
            serde_json::to_string(&probe)
                .unwrap()
                .contains("/work/notes.txt"),
            "the writer would emit an argument if it were given one; \
             an absence assertion against it therefore has teeth"
        );

        for (park, kind, arguments) in [
            (
                park_path_request as fn(&kitsu_tools::PromptRequestSender, u64) -> _,
                "write_file",
                &["/work/notes.txt", "notes.txt"][..],
            ),
            (
                park_command_request as fn(&kitsu_tools::PromptRequestSender, u64) -> _,
                "run_command",
                &["/work/tmp", "/usr/bin/rm", "rm -rf", "-rf"][..],
            ),
        ] {
            let registry = AgentRegistry::new();
            let id = registry.start("needs an answer".to_string()).unwrap();
            let sender = registry.prompt_sender();
            let _answer = park(&sender, id);
            await_blocked(&registry, id).await;

            let payload = state_payload(&registry, true);
            assert_eq!(
                thread_in(&payload, id)["thread"]["waiting_for"],
                kind,
                "the field is the kind and nothing else: {payload}"
            );
            let encoded = serde_json::to_string(&payload).unwrap();
            for argument in arguments {
                assert!(
                    !encoded.contains(argument),
                    "the state file leaked a parked argument ({argument}): {encoded}"
                );
            }
        }
    }

    /// Surface 2, the positive half: the hook's payload assembly reports a
    /// parked agent as `blocked`, and tells the child what it waits on using
    /// the same machine word the state file publishes.
    #[tokio::test]
    async fn the_agent_state_hook_fires_blocked_with_the_kind_it_waits_on() {
        let registry = AgentRegistry::new();
        let id = registry.start("needs an answer".to_string()).unwrap();
        let sender = registry.prompt_sender();
        let _answer = park_command_request(&sender, id);

        let before = crate::agent_hook::observations(&registry);
        await_blocked(&registry, id).await;
        let current = crate::agent_hook::observations(&registry);

        let events = crate::agent_hook::transitions(&before, &current);
        assert_eq!(events.len(), 1, "the park is one transition: {events:?}");
        assert_eq!(events[0].id, id);
        assert_eq!(events[0].state, "blocked", "{events:?}");
        assert_eq!(
            events[0].waiting_for, "run_command",
            "the event names the permission kind: {events:?}"
        );

        let env = crate::agent_hook::hook_env(&events[0]);
        assert_eq!(env[1].0, "KITSU_AGENT_STATE");
        assert_eq!(env[1].1, "blocked", "{env:?}");
        assert_eq!(env[2].0, "KITSU_AGENT_WAITING_FOR");
        assert_eq!(
            env[2].1,
            thread::AgentApprovalKind::RunCommand.wire(),
            "the child's environment carries `wire()`: {env:?}"
        );

        // Released: the hook fires a second time, with the state moved into.
        assert_eq!(
            registry.answer(id, Decision::Deny),
            AgentAnswerOutcome::Answered
        );
        let released = crate::agent_hook::observations(&registry);
        let events = crate::agent_hook::transitions(&current, &released);
        assert_eq!(events.len(), 1, "the release is one transition: {events:?}");
        assert_eq!(events[0].state, "running", "{events:?}");
        assert_ne!(
            events[0].waiting_for, "run_command",
            "a released agent is no longer waiting on its permission: {events:?}"
        );
    }

    /// Surface 2's leak test: a hook runs an *external program*, so the parked
    /// path and argv must not reach its environment.
    ///
    /// Not vacuous: `hook_env` is first shown to pass a path through intact --
    /// `safe_inline_label` strips control characters and credential forms, and
    /// leaves filesystem paths and argv words completely untouched (pinned in
    /// `thread.rs`) -- so the absences below are about what is assembled, not
    /// about a filter that would have hidden a leak anyway.
    #[tokio::test]
    async fn the_hook_environment_never_carries_a_parked_path_or_argv_word() {
        let probe = crate::agent_hook::hook_env(&crate::agent_hook::HookEvent {
            id: 1,
            state: "blocked".to_string(),
            waiting_for: "/work/notes.txt rm -rf /work/tmp".to_string(),
        });
        assert!(
            probe[2].1.contains("/work/notes.txt") && probe[2].1.contains("/work/tmp"),
            "hook_env passes a path and an argv word through intact ({probe:?}); \
             an absence assertion against it therefore has teeth"
        );

        for (park, kind, arguments) in [
            (
                park_path_request as fn(&kitsu_tools::PromptRequestSender, u64) -> _,
                "write_file",
                &["/work/notes.txt", "notes.txt"][..],
            ),
            (
                park_command_request as fn(&kitsu_tools::PromptRequestSender, u64) -> _,
                "run_command",
                &["/work/tmp", "/usr/bin/rm", "rm -rf", "-rf"][..],
            ),
        ] {
            let registry = AgentRegistry::new();
            let id = registry.start("needs an answer".to_string()).unwrap();
            let sender = registry.prompt_sender();
            let _answer = park(&sender, id);
            await_blocked(&registry, id).await;

            let current = crate::agent_hook::observations(&registry);
            let events = crate::agent_hook::transitions(&[], &current);
            assert_eq!(events.len(), 1, "{events:?}");
            assert_eq!(
                events[0].waiting_for, kind,
                "the event's reason is the kind and nothing else: {events:?}"
            );
            let env = crate::agent_hook::hook_env(&events[0]);
            let exported = env
                .iter()
                .map(|(name, value)| format!("{name}={value}"))
                .collect::<Vec<_>>()
                .join("\n");
            for argument in arguments {
                assert!(
                    !exported.contains(argument),
                    "the hook environment leaked a parked argument ({argument}): {exported}"
                );
            }
        }
    }

    // ---- Card T4-S7, requirement A6: what admits a dispatch ----
    //
    // The view's half (the key, the parse, the painted refusal) is pinned in
    // `main.rs`; this is the half that needs a provider, which is why the card
    // splits here -- the same split T4-S4b and T4-S6b were made along.

    fn dispatcher_over(registry: Arc<AgentRegistry>, live: LiveModelWindow) -> ViewDispatcher {
        let mut cfg = sub_cfg();
        cfg.quiet = true;
        view_dispatcher(
            ProviderBinder::fixed(Arc::new(EchoProvider)),
            Arc::new(Registry::new()),
            cfg,
            live,
            String::new(),
            registry,
            empty_profile_runtime(),
        )
    }

    /// **Requirement A6's risk gate, in the positive form: a dispatch succeeds
    /// in a state where the model's per-turn `SpawnBudget` would have refused
    /// it.**
    ///
    /// A test that merely avoided mentioning `SpawnBudget` would prove nothing
    /// -- it would pass against an implementation that consulted a budget
    /// which happened to be open. So the budget here is really exhausted, and
    /// the POSITIVE CONTROL is the model's own `spawn_agent_bg` being refused
    /// by it on the very next call, against the same registry, in the same
    /// state, with the same task.
    ///
    /// `SpawnBudget` exists to stop a *storming model* fanning out within one
    /// turn -- `spawn_cap_error` is phrased as an instruction to a model for
    /// exactly that reason. A human typing a task into the agent view is not a
    /// storming model, and a dispatch refused because the model had already
    /// spent the turn's budget would be a refusal with nothing to do with the
    /// person who typed it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_view_dispatch_is_admitted_by_the_registry_and_never_by_the_spawn_budget() {
        let registry = AgentRegistry::new();
        let live: LiveModelWindow = Arc::new(Mutex::new(("m".to_string(), None)));
        let budget = Arc::new(SpawnBudget::new(1));
        let mut cfg = sub_cfg();
        cfg.quiet = true;
        let (_descriptor, model_spawn) = spawn_agent_bg_tool(
            Arc::new(EchoProvider),
            Arc::new(Registry::new()),
            cfg,
            live.clone(),
            String::new(),
            registry.clone(),
            empty_profile_runtime(),
            budget.clone(),
        );

        // The turn's one allowed model spawn, spent.
        model_spawn(r#"{"task":"the model's first spawn"}"#.to_string())
            .await
            .expect("the first spawn is within the cap");
        assert_eq!(budget.used(), 1, "the turn's budget is now exhausted");

        // POSITIVE CONTROL: in exactly this state the model is refused.
        let refused = model_spawn(r#"{"task":"the model's second spawn"}"#.to_string())
            .await
            .expect_err("POSITIVE CONTROL: the budget refuses the model here");
        assert!(
            refused.to_string().contains("agent spawn cap reached"),
            "and it is the budget that refused it: {refused}"
        );
        let before = registry.progress_snapshot().total;

        // The person's dispatch, in the same state, against the same registry.
        let dispatch = dispatcher_over(registry.clone(), live);
        let id = dispatch("the person's dispatch".to_string(), None).expect(
            "A6: the registry admits this -- the model's per-turn budget is not \
             consulted for a dispatch a human typed",
        );
        assert!(id > 0, "a dispatch returns the id the view reports");
        assert_eq!(
            registry.progress_snapshot().total,
            before + 1,
            "and the record really exists: the dispatch started an agent"
        );
        assert_eq!(
            budget.used(),
            1,
            "the budget was neither consulted nor spent by the dispatch"
        );
    }

    /// **The card's named acceptance, at the registry: at `max_running` a
    /// dispatch is refused, in the registry's own words, naming the ceiling.**
    ///
    /// Refused and not queued: A6 gives the view the registry's ceilings, and
    /// `check_capacity` never evicts existing work.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_view_dispatch_over_max_running_is_refused_in_the_registrys_own_words() {
        let registry = AgentRegistry::with_limits(AgentRegistryLimits {
            max_running: 1,
            max_records: 64,
        });
        let live: LiveModelWindow = Arc::new(Mutex::new(("m".to_string(), None)));
        let dispatch = dispatcher_over(registry.clone(), live);

        // One running agent, which is this registry's whole running capacity.
        // `start` leaves it `Running` with no task to finish, so the ceiling
        // below is reached deterministically.
        let held = registry
            .start("the one running agent".to_string())
            .expect("the first start is within the limit");
        assert_eq!(registry.running(), 1, "NON-VACUOUS: the ceiling is reached");

        let error = dispatch("one agent too many".to_string(), None)
            .expect_err("A6: the registry's running ceiling refuses this");
        assert_eq!(
            error,
            AgentStartError {
                kind: AgentCapacityKind::Running,
                current: 1,
                limit: 1,
            }
            .to_string(),
            "the refusal is the registry's own sentence, which names the \
             ceiling and what to do about it"
        );
        assert_eq!(
            registry.progress_snapshot().total,
            1,
            "and nothing was started: the refusal never evicts the work \
             already running"
        );

        // POSITIVE CONTROL: with the ceiling freed the same dispatch is
        // admitted, so this is a capacity refusal and not a dispatcher that
        // refuses everything.
        registry.finish(held, Ok("done".to_string()));
        assert!(
            dispatch("one agent after all".to_string(), None).is_ok(),
            "POSITIVE CONTROL: the same dispatch succeeds once a slot frees"
        );
    }

    /// **A6 names both ceilings, and `max_records` is not `max_running`.**
    ///
    /// They fail differently and the correction differs: a terminal agent
    /// still occupies a *record*, so a registry with nothing running can still
    /// refuse, and the answer is to prune rather than to wait. A view that
    /// only knew about `max_running` would refuse this dispatch with no
    /// explanation at all.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_view_dispatch_over_max_records_is_refused_as_a_record_ceiling() {
        let registry = AgentRegistry::with_limits(AgentRegistryLimits {
            max_running: 4,
            max_records: 1,
        });
        let live: LiveModelWindow = Arc::new(Mutex::new(("m".to_string(), None)));
        let dispatch = dispatcher_over(registry.clone(), live);

        let finished = registry
            .start("an agent that has finished".to_string())
            .expect("the first start is within both limits");
        registry.finish(finished, Ok("its result".to_string()));
        assert_eq!(
            (registry.running(), registry.progress_snapshot().total),
            (0, 1),
            "NON-VACUOUS: nothing is running, so only the record ceiling can \
             refuse the dispatch below"
        );

        let error = dispatch("one record too many".to_string(), None)
            .expect_err("A6: the registry's record ceiling refuses this");
        assert_eq!(
            error,
            AgentStartError {
                kind: AgentCapacityKind::Records,
                current: 1,
                limit: 1,
            }
            .to_string(),
            "and it is the *record* ceiling's sentence, which says to prune"
        );
    }

    /// **An unknown profile is refused, and it starts nothing.**
    ///
    /// `@<profile>` is syntax the view can parse and a name only the catalog
    /// can judge, so this refusal is the dispatcher's. It is `spawn_agent_bg`'s
    /// own wording, because a second wording of "no such profile" would be a
    /// second source of truth about which profiles exist.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_view_dispatch_with_an_unknown_profile_starts_nothing() {
        let registry = AgentRegistry::new();
        let live: LiveModelWindow = Arc::new(Mutex::new(("m".to_string(), None)));
        let dispatch = dispatcher_over(registry.clone(), live);

        let error = dispatch(
            "review the diff".to_string(),
            Some("no-such-profile".to_string()),
        )
        .expect_err("the catalog has no such profile");
        assert!(
            error.contains("unknown background-agent profile 'no-such-profile'"),
            "the catalog's own refusal, naming the profile that does not \
             exist: {error}"
        );
        assert_eq!(
            registry.progress_snapshot().total,
            0,
            "and nothing was started with the default profile instead: a \
             dispatch that silently dropped the selection would run the wrong \
             kind of agent on the work"
        );

        // POSITIVE CONTROL: the same dispatch with no profile at all is
        // admitted, so the refusal above is the name and not the dispatcher.
        assert!(
            dispatch("review the diff".to_string(), None).is_ok(),
            "POSITIVE CONTROL: the default profile is always available"
        );
    }

    // ------------------------------------------------------------------
    // Card **T4-S6b**: the routed composer's **steer** half.
    //
    // Split here for the reason T4-S4b, T4-S6a and T4-S7 were all split
    // here: a *steerable* agent is one with a live thread and an open
    // correction channel, and that needs a provider fixture
    // (`SteeringProvider`, `empty_profile_runtime`, `spawn_agent_bg_tool`)
    // which exists only in this module. `main.rs`'s registry fixtures can
    // reach every refusal and the whole answer route, and pin those; the one
    // thing they cannot build is a steer that *succeeds*, which is this
    // test.
    //
    // The routed function itself lives in `main.rs` (the crate root), beside
    // the composer loop that calls it; a child module can reach it.
    // ------------------------------------------------------------------

    /// Every transcript line painted through the renderer's surface seam.
    /// **F71**: the receipt claim is made against the painted bytes, not
    /// against a returned `String` no one proved reaches a terminal.
    #[derive(Default)]
    struct PaintedLines(Mutex<Vec<String>>);

    impl kitsu_render::surface::Surface for PaintedLines {
        fn transcript_line(&self, fd: kitsu_render::surface::Fd, text: &str) {
            assert_eq!(
                fd,
                kitsu_render::surface::Fd::Stderr,
                "a receipt is a meta line: {text:?}"
            );
            self.0.lock().expect("painted lines").push(text.to_string());
        }
    }

    /// Install a recorder over the process-global surface slot and restore
    /// the displaced surface on drop, so a panicking assertion cannot leave
    /// this binary's later tests writing into a dead recorder.
    struct SurfaceProbe {
        previous: Option<Arc<dyn kitsu_render::surface::Surface>>,
        painted: Arc<PaintedLines>,
    }

    impl SurfaceProbe {
        fn install() -> Self {
            let painted = Arc::new(PaintedLines::default());
            let previous = kitsu_render::surface::set_surface(painted.clone());
            Self {
                previous: Some(previous),
                painted,
            }
        }

        fn lines(&self) -> Vec<String> {
            self.painted.0.lock().expect("painted lines").clone()
        }
    }

    impl Drop for SurfaceProbe {
        fn drop(&mut self) {
            if let Some(previous) = self.previous.take() {
                kitsu_render::surface::set_surface(previous);
            }
        }
    }

    /// **The steer route, end to end: a composer line corrects the attached
    /// agent's work.**
    ///
    /// FAILS today: `route_attached_submission` sends nothing.
    ///
    /// The assertion is what the route **causes** (T4-S8's vacuity result):
    /// the in-flight attempt is cancelled and restarted, the provider is
    /// called a second time, and the restarted turn's answer carries the
    /// user's words. A route that merely reported a receipt would fail on
    /// every one of those.
    ///
    /// The receipt is pinned as `format_steering_receipt`'s own output
    /// (`/jobs steer`'s and `s`'s wording) rather than as a second spelling
    /// of it; only the discarded-progress duration, which is wall-clock, is
    /// left unpinned, and it is bracketed on both sides so nothing can be
    /// inserted around it.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[serial_test::serial]
    async fn a_composer_line_while_attached_steers_the_agent_and_restarts_its_turn() {
        use std::time::Duration;

        let registry = AgentRegistry::new();
        let provider = Arc::new(SteeringProvider {
            calls: AtomicUsize::new(0),
        });
        let live: LiveModelWindow = Arc::new(Mutex::new(("m".to_string(), None)));
        let mut cfg = sub_cfg();
        cfg.quiet = true;
        let (_descriptor, handler) = spawn_agent_bg_tool(
            provider.clone(),
            Arc::new(Registry::new()),
            cfg,
            live,
            String::new(),
            registry.clone(),
            empty_profile_runtime(),
            open_budget(),
        );
        handler(r#"{"task":"review the parser"}"#.to_string())
            .await
            .unwrap();
        // The thread must be live and steerable before the line is routed,
        // or a refusal would masquerade as the behaviour under test.
        tokio::time::timeout(Duration::from_secs(1), async {
            while !registry
                .thread_snapshot(1)
                .is_some_and(|snapshot| snapshot.steering_open)
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the spawned agent's correction channel opens");

        let probe = SurfaceProbe::install();
        let submit = crate::route_attached_submission(&registry, 1, "focus on UTF-8 boundaries");

        assert_eq!(
            submit.route,
            crate::AttachedRoute::Steered,
            "A5: while attached to a working agent, the composer steers it"
        );
        assert_eq!(
            submit.draft, "",
            "a delivered correction is gone from the composer"
        );

        let painted = probe.lines();
        assert_eq!(painted.len(), 1, "exactly one receipt: {painted:?}");
        let receipt = &painted[0];
        // `format_steering_receipt`'s own output, pinned around its two
        // runtime fields -- the queue depth (the restart may already have
        // taken the correction off the queue) and the wall-clock progress it
        // discarded -- so nothing can be inserted anywhere in the line.
        assert!(
            receipt
                .starts_with("restarting background agent #1 with your correction (sequence 1; ")
                && receipt.contains(" queued) - ")
                && receipt
                    .ends_with("s of in-flight progress discarded; completed file changes kept"),
            "the receipt `/jobs steer` prints, not a second wording of it: \
             {receipt:?}"
        );

        tokio::time::timeout(Duration::from_secs(5), async {
            while registry.progress_snapshot().running != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the steered turn restarts and finishes");
        assert_eq!(
            provider.calls.load(AtomicOrdering::SeqCst),
            2,
            "the in-flight attempt was cancelled and a second one started"
        );
        let result = registry.peek_result(1).expect("agent #1 has a result");
        assert!(
            result.contains("focus on UTF-8 boundaries"),
            "and the restarted turn followed the line the user typed into the \
             composer: {result}"
        );
        assert!(
            registry
                .thread_snapshot(1)
                .expect("agent #1 exists")
                .timeline
                .entries
                .iter()
                .any(|entry| {
                    entry.kind == thread::AgentTimelineKind::Lifecycle
                        && entry.text.contains("correction #1 applied")
                }),
            "and the thread records the correction as applied"
        );
    }

    // ---------------------------------------------------------------------
    // T5-S6 (H5): the `agent_wait` tool.
    //
    // `blocked` is NOT a new word here: `AgentStatus::Blocked` /
    // `AgentProgressStatus::Blocked` / `AgentThreadPhase::Blocked` all label
    // "blocked" and all mean *parked on a user answer*, which is what
    // `park_prompt` installs. `done` is the registry's existing `terminal`
    // predicate (`not in {Running, Blocked}`), so a failed or cancelled agent
    // satisfies a `done` wait instead of hanging it to the timeout.
    //
    // Every timing assertion below runs on tokio's VIRTUAL clock
    // (`start_paused = true`). Nothing sleeps on wall time: the clock only
    // moves when the runtime is idle on a timer, so "returned at once" is
    // `elapsed == ZERO` exactly and "timed out" is `elapsed == timeout`
    // exactly. `yield_now` does not move it, so `await_blocked` is safe.
    // ---------------------------------------------------------------------

    fn wait_tool(registry: &Arc<AgentRegistry>) -> HandlerFn {
        agent_wait_tool(registry.clone()).1
    }

    fn wait_args(id: u64, until: &str, timeout_secs: Option<i64>) -> String {
        match timeout_secs {
            Some(secs) => {
                format!(r#"{{"id":{id},"until":"{until}","timeout_secs":{secs}}}"#)
            }
            None => format!(r#"{{"id":{id},"until":"{until}"}}"#),
        }
    }

    /// Virtual-clock elapsed time across one `agent_wait` call, plus its
    /// outcome. This is the whole no-flake seam: the duration is exact.
    async fn timed_wait(
        registry: &Arc<AgentRegistry>,
        args: String,
    ) -> (Result<String, ToolError>, std::time::Duration) {
        let handler = wait_tool(registry);
        let before = tokio::time::Instant::now();
        let outcome = handler(args).await;
        (outcome, tokio::time::Instant::now() - before)
    }

    /// A registry holding one agent parked on an approval, with its gate
    /// receiver kept alive so nothing resolves the park behind the test.
    async fn parked_registry() -> (
        Arc<AgentRegistry>,
        u64,
        tokio::sync::oneshot::Receiver<Decision>,
        kitsu_tools::PromptRequestSender,
    ) {
        let registry = AgentRegistry::new();
        let id = registry.start("parks on an answer".to_string()).unwrap();
        let sender = registry.prompt_sender();
        let answer = park_path_request(&sender, id);
        await_blocked(&registry, id).await;
        (registry, id, answer, sender)
    }

    /// The descriptor the model reads. `until` must be a closed enum, or the
    /// model is free to invent a third state the tool cannot honour.
    #[test]
    fn agent_wait_describes_an_id_a_closed_until_enum_and_a_timeout() {
        let (descriptor, _handler) = agent_wait_tool(AgentRegistry::new());
        assert_eq!(descriptor.name, "agent_wait");
        assert!(
            !descriptor.description.is_empty(),
            "the model needs to know what the tool blocks on"
        );
        let properties = &descriptor.parameters["properties"];
        assert_eq!(properties["id"]["type"], "integer");
        assert_eq!(
            properties["until"]["enum"],
            serde_json::json!(["done", "blocked"]),
            "H5 spells the states `done|blocked`, and nothing else is answerable"
        );
        assert_eq!(
            properties["timeout_secs"]["type"], "integer",
            "the unit lives in the key name, as it does for `block_timeout_secs`"
        );
        assert_eq!(
            descriptor.parameters["required"],
            serde_json::json!(["id", "until"]),
            "a timeout is optional and defaults; an id and a state are not"
        );
        assert!(
            descriptor
                .description
                .to_lowercase()
                .contains("does not count against"),
            "H5's budget exemption is the model-visible half of the requirement: {}",
            descriptor.description
        );
    }

    /// The state is already satisfied when the call arrives: an agent that
    /// finished long ago must answer in the same breath, not wait.
    #[tokio::test(start_paused = true)]
    async fn agent_wait_until_done_returns_an_already_finished_result_without_waiting() {
        let registry = AgentRegistry::new();
        let id = registry.start("finished an hour ago".to_string()).unwrap();
        registry.finish(id, Ok("THE ANSWER".to_string()));

        let (outcome, elapsed) = timed_wait(&registry, wait_args(id, "done", Some(600))).await;
        assert_eq!(
            outcome.expect("a finished agent is a successful wait"),
            "THE ANSWER",
            "a satisfied wait returns exactly what `agent_result` would"
        );
        assert_eq!(
            elapsed,
            std::time::Duration::ZERO,
            "an already-satisfied wait consumes none of its timeout"
        );
    }

    /// `done` is the registry's `terminal` predicate, not the `Done` variant
    /// alone. If it were the variant alone, waiting on an agent that failed or
    /// was cancelled would burn the whole timeout for an answer that could
    /// never change — the half-working case.
    #[tokio::test(start_paused = true)]
    async fn agent_wait_until_done_is_satisfied_by_failed_and_cancelled_agents() {
        let registry = AgentRegistry::new();

        let failed = registry.start("fails".to_string()).unwrap();
        registry.finish(failed, Err("boom".to_string()));
        let (outcome, elapsed) = timed_wait(&registry, wait_args(failed, "done", Some(600))).await;
        assert_eq!(
            outcome.expect("a failed agent is terminal, so the wait succeeds"),
            format!("agent #{failed} failed: boom"),
        );
        assert_eq!(elapsed, std::time::Duration::ZERO, "failed is terminal now");

        let cancelled = registry.start("is cancelled".to_string()).unwrap();
        registry.cancel(cancelled).await;
        let (outcome, elapsed) =
            timed_wait(&registry, wait_args(cancelled, "done", Some(600))).await;
        assert!(outcome
            .expect("a cancelled agent is terminal, so the wait succeeds")
            .starts_with(&format!("agent #{cancelled} cancelled: ")),);
        assert_eq!(
            elapsed,
            std::time::Duration::ZERO,
            "cancelled is terminal now"
        );
    }

    /// `until: blocked` on an agent already parked on an answer.
    #[tokio::test(start_paused = true)]
    async fn agent_wait_until_blocked_returns_at_once_for_an_already_parked_agent() {
        let (registry, id, _answer, _sender) = parked_registry().await;

        let (outcome, elapsed) = timed_wait(&registry, wait_args(id, "blocked", Some(600))).await;
        assert_eq!(
            outcome.expect("a parked agent satisfies a `blocked` wait"),
            format!("agent #{id} is waiting on an answer"),
            "the blocked wording is the registry's existing one"
        );
        assert_eq!(elapsed, std::time::Duration::ZERO);
    }

    /// The agent reaches the state *while* the call is blocked. This is the
    /// tool's whole reason to exist, and it must not race: the wait is armed
    /// before the park happens.
    #[tokio::test(start_paused = true)]
    async fn agent_wait_wakes_when_the_agent_reaches_the_state_during_the_call() {
        let registry = AgentRegistry::new();
        let id = registry.start("finishes later".to_string()).unwrap();

        let handler = wait_tool(&registry);
        let waiter = tokio::spawn(handler(wait_args(id, "done", Some(600))));
        // Let the wait arm itself on the watch channel before anything changes.
        for _ in 0..64 {
            tokio::task::yield_now().await;
        }
        registry.finish(id, Ok("LATE ANSWER".to_string()));

        assert_eq!(
            waiter
                .await
                .expect("the wait task does not panic")
                .expect("reaching the state is a successful wait"),
            "LATE ANSWER",
        );
    }

    /// A timeout is a NORMAL outcome, not an error: the model is told the
    /// state it did not reach and must stay free to do something else.
    #[tokio::test(start_paused = true)]
    async fn agent_wait_until_done_times_out_as_an_ok_outcome_naming_the_current_state() {
        let registry = AgentRegistry::new();
        let id = registry.start("runs forever".to_string()).unwrap();

        let (outcome, elapsed) = timed_wait(&registry, wait_args(id, "done", Some(90))).await;
        assert_eq!(
            outcome.expect("a timeout is an answer the model can act on, not a tool error"),
            format!(
                "timed out after 90s waiting for agent #{id} to reach done: \
                 agent #{id} is still running"
            ),
        );
        assert_eq!(
            elapsed,
            std::time::Duration::from_secs(90),
            "the wait honours exactly the timeout it was given"
        );
    }

    /// The *other* terminal state: a `blocked` wait on an agent that goes
    /// terminal can never be satisfied, so it must return at once rather than
    /// hold the model's round open for a state that is now unreachable.
    #[tokio::test(start_paused = true)]
    async fn agent_wait_until_blocked_gives_up_at_once_once_the_agent_is_terminal() {
        let registry = AgentRegistry::new();
        let id = registry.start("finishes instead".to_string()).unwrap();
        registry.finish(id, Ok("done not blocked".to_string()));

        let (outcome, elapsed) = timed_wait(&registry, wait_args(id, "blocked", Some(600))).await;
        assert_eq!(
            outcome.expect("an unreachable state is reported, not errored"),
            format!(
                "agent #{id} reached a terminal state and can no longer block: \
                 done not blocked"
            ),
        );
        assert_eq!(
            elapsed,
            std::time::Duration::ZERO,
            "a terminal agent can never block, so waiting for it is pointless"
        );
    }

    /// And the converse: `blocked` is NON-terminal, so a `done` wait on an
    /// agent that parks and stays parked must run its timeout out. The park
    /// must not be mistaken for an early answer.
    #[tokio::test(start_paused = true)]
    async fn agent_wait_until_done_runs_its_timeout_out_on_a_permanently_blocked_agent() {
        let (registry, id, _answer, _sender) = parked_registry().await;

        let (outcome, elapsed) = timed_wait(&registry, wait_args(id, "done", Some(120))).await;
        assert_eq!(
            outcome.expect("a timeout is a normal outcome"),
            format!(
                "timed out after 120s waiting for agent #{id} to reach done: \
                 agent #{id} is waiting on an answer"
            ),
            "blocked is not done: the wait reports the park it is stuck behind"
        );
        assert_eq!(
            elapsed,
            std::time::Duration::from_secs(120),
            "a blocked agent is still live, so the wait is bounded only by its timeout"
        );
    }

    /// An unknown id — and, because the registry IS the session, an id
    /// belonging to no agent of this session is the same case. It must fail
    /// immediately: making a typo cost a 60-second round is the defect.
    #[tokio::test(start_paused = true)]
    async fn agent_wait_on_an_unknown_id_errors_immediately_without_spending_its_timeout() {
        let registry = AgentRegistry::new();
        let known = registry.start("exists".to_string()).unwrap();
        let stranger = known + 999;

        let (outcome, elapsed) =
            timed_wait(&registry, wait_args(stranger, "done", Some(600))).await;
        match outcome {
            Err(ToolError::ExecError(message)) => assert_eq!(
                message,
                format!("no background agent #{stranger}"),
                "the same wording `agent_result` uses for an unknown id"
            ),
            other => panic!("an unknown id is an execution error: {other:?}"),
        }
        assert_eq!(
            elapsed,
            std::time::Duration::ZERO,
            "an id that cannot resolve must not cost a whole timeout"
        );
    }

    /// Argument edges: an `until` outside the enum, and a NEGATIVE timeout.
    /// Both are the model's mistake and both are rejected rather than guessed
    /// at — a negative timeout silently read as zero, or as forever, is
    /// exactly the invisible-now, visible-later failure.
    #[tokio::test(start_paused = true)]
    async fn agent_wait_rejects_an_unknown_until_value_and_a_negative_timeout() {
        let registry = AgentRegistry::new();
        let id = registry.start("runs".to_string()).unwrap();

        let (outcome, elapsed) = timed_wait(&registry, wait_args(id, "finished", Some(60))).await;
        match outcome {
            Err(ToolError::InvalidArgs(message)) => assert!(
                message.contains("until"),
                "the rejection must name the argument at fault: {message}"
            ),
            other => panic!("an `until` outside the enum is an argument error: {other:?}"),
        }
        assert_eq!(elapsed, std::time::Duration::ZERO);

        let (outcome, elapsed) = timed_wait(&registry, wait_args(id, "done", Some(-5))).await;
        assert!(
            matches!(outcome, Err(ToolError::InvalidArgs(_))),
            "a negative timeout is rejected, never coerced to zero or to forever: {outcome:?}"
        );
        assert_eq!(elapsed, std::time::Duration::ZERO);
    }

    /// `timeout_secs: 0` is a legitimate request: poll the state once and
    /// answer now. It is not an error and it is not "wait forever".
    #[tokio::test(start_paused = true)]
    async fn agent_wait_with_a_zero_timeout_polls_once_and_returns_immediately() {
        let registry = AgentRegistry::new();
        let running = registry.start("still going".to_string()).unwrap();

        let (outcome, elapsed) = timed_wait(&registry, wait_args(running, "done", Some(0))).await;
        assert_eq!(
            outcome.expect("a zero timeout is a poll, not an error"),
            format!(
                "timed out after 0s waiting for agent #{running} to reach done: \
                 agent #{running} is still running"
            ),
        );
        assert_eq!(elapsed, std::time::Duration::ZERO, "zero means zero");

        // A zero timeout on an ALREADY satisfied state still answers.
        let finished = registry.start("finished".to_string()).unwrap();
        registry.finish(finished, Ok("QUICK".to_string()));
        let (outcome, _) = timed_wait(&registry, wait_args(finished, "done", Some(0))).await;
        assert_eq!(
            outcome.expect("a satisfied state needs no time at all"),
            "QUICK",
            "a zero timeout must still report a state that is already true"
        );
    }

    /// A missing timeout defaults; an absurd one clamps. Both pin the
    /// constants, because an unclamped `u64::MAX` seconds is an overflowing
    /// deadline and a 584-billion-year round.
    #[tokio::test(start_paused = true)]
    async fn agent_wait_defaults_a_missing_timeout_and_clamps_an_absurd_one() {
        assert_eq!(
            DEFAULT_AGENT_WAIT_TIMEOUT,
            std::time::Duration::from_secs(60),
            "T5-S6's default wait is one minute"
        );
        assert_eq!(
            MAX_AGENT_WAIT_TIMEOUT,
            std::time::Duration::from_secs(30 * 60),
            "T5-S6 caps one wait at thirty minutes"
        );
        assert!(
            DEFAULT_AGENT_WAIT_TIMEOUT < MAX_AGENT_WAIT_TIMEOUT,
            "the default must sit under the ceiling it is clamped by"
        );

        let registry = AgentRegistry::new();

        let id = registry.start("runs forever".to_string()).unwrap();
        let (outcome, elapsed) = timed_wait(&registry, wait_args(id, "done", None)).await;
        assert_eq!(
            elapsed, DEFAULT_AGENT_WAIT_TIMEOUT,
            "an omitted timeout waits exactly the default"
        );
        assert_eq!(
            outcome.expect("a defaulted timeout is still a normal outcome"),
            format!(
                "timed out after {}s waiting for agent #{id} to reach done: \
                 agent #{id} is still running",
                DEFAULT_AGENT_WAIT_TIMEOUT.as_secs()
            ),
            "the reported number is the timeout actually used, not the one asked for"
        );

        let absurd = registry.start("outlives the universe".to_string()).unwrap();
        let (outcome, elapsed) =
            timed_wait(&registry, wait_args(absurd, "done", Some(i64::MAX))).await;
        assert_eq!(
            elapsed, MAX_AGENT_WAIT_TIMEOUT,
            "an absurd timeout is clamped to the ceiling rather than overflowing"
        );
        assert_eq!(
            outcome.expect("a clamped timeout still answers"),
            format!(
                "timed out after {}s waiting for agent #{absurd} to reach done: \
                 agent #{absurd} is still running",
                MAX_AGENT_WAIT_TIMEOUT.as_secs()
            ),
            "the model is told the clamped timeout, so it can tell it was clamped"
        );
    }

    /// Two waits on the same id, and a wait on the agent that is itself
    /// calling. Neither may deadlock: the registry lock is never held across
    /// the wait, so a self-wait simply runs its timeout out and the model gets
    /// its round back.
    #[tokio::test(start_paused = true)]
    async fn two_waits_on_one_id_both_resolve_and_a_self_wait_cannot_deadlock() {
        let registry = AgentRegistry::new();
        let id = registry
            .start("one agent, two watchers".to_string())
            .unwrap();

        let first = tokio::spawn(wait_tool(&registry)(wait_args(id, "done", Some(600))));
        let second = tokio::spawn(wait_tool(&registry)(wait_args(id, "done", Some(600))));
        for _ in 0..64 {
            tokio::task::yield_now().await;
        }
        registry.finish(id, Ok("SHARED".to_string()));
        assert_eq!(
            first
                .await
                .expect("no panic")
                .expect("the first waiter wakes"),
            "SHARED",
        );
        assert_eq!(
            second
                .await
                .expect("no panic")
                .expect("the second waiter wakes too"),
            "SHARED",
            "a wait must not consume the state it observed"
        );

        // The self-wait: an agent waiting for its OWN completion. It is still
        // running by definition, so the only correct outcome is its timeout.
        let itself = registry.start("waits on itself".to_string()).unwrap();
        let (outcome, elapsed) = timed_wait(&registry, wait_args(itself, "done", Some(30))).await;
        assert_eq!(
            elapsed,
            std::time::Duration::from_secs(30),
            "a self-wait returns on its timeout instead of hanging the turn"
        );
        assert!(outcome
            .expect("a self-wait times out rather than erroring")
            .contains("is still running"),);
    }

    /// H5's own emphasis: `agent_wait` spawns nothing, so it must work with
    /// the turn's `SpawnBudget` fully spent, and must not spend any of it.
    ///
    /// The POSITIVE CONTROL is `spawn_agent` on the same exhausted budget
    /// being refused: without it this test would pass against a budget that
    /// was never exhausted in the first place.
    #[tokio::test(start_paused = true)]
    async fn agent_wait_works_on_an_exhausted_spawn_budget_and_never_consumes_it() {
        let registry = AgentRegistry::new();
        let id = registry.start("already finished".to_string()).unwrap();
        registry.finish(id, Ok("WAITED ANYWAY".to_string()));

        let budget = Arc::new(SpawnBudget::new(1));
        let mut cfg = sub_cfg();
        cfg.quiet = true;
        let (_descriptor, spawn) = spawn_agent_tool(
            Arc::new(EchoProvider),
            Arc::new(Registry::new()),
            cfg,
            Arc::new(Mutex::new(("m".to_string(), None))) as LiveModelWindow,
            String::new(),
            budget.clone(),
        );

        // Spend the whole cap, then confirm it really is spent.
        spawn(r#"{"task":"burn the budget"}"#.to_string())
            .await
            .expect("the first spawn is within the cap");
        assert_eq!(budget.used(), 1, "the cap of one is now spent");
        let refused = spawn(r#"{"task":"one too many"}"#.to_string())
            .await
            .expect_err("the positive control: a second spawn is refused");
        assert!(
            format!("{refused}").contains("agent spawn cap reached"),
            "the control must fail for the budget's reason: {refused}"
        );

        let used_before = budget.used();
        let outcome = wait_tool(&registry)(wait_args(id, "done", Some(600)))
            .await
            .expect("H5: a wait spawns nothing, so an exhausted budget cannot refuse it");
        assert_eq!(outcome, "WAITED ANYWAY");
        assert_eq!(
            budget.used(),
            used_before,
            "H5: a wait consumes none of the SpawnBudget"
        );
    }
}
