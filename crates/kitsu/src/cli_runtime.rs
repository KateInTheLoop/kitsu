//! CLI process lifecycle and JSONL delivery state.

use crate::cli::{Cli, Command};
use crate::process_exit::{InferenceCarrier, EXIT_CANCELLED, EXIT_PROVIDER};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::cli_error::CliError;
use crate::session;
use crate::structured_output;
use crate::subagent;

pub(crate) const JSONL_SCHEMA: &str = "kitsu.chat.v1";
pub(crate) static JSONL_SUMMARY_EMITTED: AtomicBool = AtomicBool::new(false);
/// `-o` is a host-owned sink, so a run with no fully committed assistant must
/// say that the destination was preserved instead of making automation infer
/// a no-op from the absence of an installation receipt.
pub(crate) static OUTPUT_LAST_MESSAGE_REQUESTED: AtomicBool = AtomicBool::new(false);
pub(crate) static FULLY_COMMITTED_ASSISTANT_SEEN: AtomicBool = AtomicBool::new(false);
/// Process-entry clock used by both human model-command receipts and JSONL's
/// terminal summary. `Instant` avoids wall-clock jumps; conversion saturates
/// for completeness even though a CLI process cannot realistically reach it.
pub(crate) static RUN_STARTED_AT: OnceLock<Instant> = OnceLock::new();
/// Set only by inference-capable commands, keeping management/help output free
/// of an unrelated timing footer.
pub(crate) static MODEL_COMMAND_ACTIVE: AtomicBool = AtomicBool::new(false);
/// Distinguishes structurally child-free inference preflight from a carrier
/// that has begun a provider turn. Only the former may use the top-level
/// fallback's `Joined` cleanup fact without guessing.
pub(crate) static MODEL_INFERENCE_STARTED: AtomicBool = AtomicBool::new(false);
/// One phase owner for the inference-capable process run. Carrier helpers must
/// advance this instance instead of constructing presentation-local owners.
pub(crate) static PROCESS_RUN_LIFECYCLE: OnceLock<Mutex<kitsu_core::RunLifecycle>> =
    OnceLock::new();
pub(crate) static PROCESS_TERMINAL_FACTS: OnceLock<ProcessTerminalFacts> = OnceLock::new();
pub(crate) static PROCESS_INTERACTIVE_TERMINAL_FACTS: OnceLock<InteractiveTerminalFacts> =
    OnceLock::new();
pub(crate) static PROCESS_INFERENCE_CARRIER: OnceLock<InferenceCarrier> = OnceLock::new();
pub(crate) static PROCESS_PUBLISHED_RUN: OnceLock<kitsu_core::PublishedRun> = OnceLock::new();
/// Set only when `--allow-turn-errors` converts a scripted run with both a
/// success and a failure into an accepted partial result. The process-level
/// timing footer otherwise sees only exit code 0 and would mislabel it `ok`.
pub(crate) static SCRIPT_ACCEPTED_PARTIAL: AtomicBool = AtomicBool::new(false);
/// Last terminal assistant disposition observed in this process-run. Zero is
/// the valid initial `Absent` state.
pub(crate) static RUN_ASSISTANT_DISPOSITION: AtomicU8 = AtomicU8::new(0);
/// Exact count of semantically successful requested operations mirrored into
/// JSONL's terminal summary. Scripted execution owns its authoritative local
/// count (notably, `/quit` is control flow rather than a successful operation)
/// and stores it at finalization. Single-operation commands record a success
/// only after accepting a catalog result or committing a model answer.
pub(crate) static SUCCESSFUL_OPERATIONS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static OUTPUT_SCHEMA: OnceLock<structured_output::OutputSchema> = OnceLock::new();
/// Terminal durability state mirrored into `final_summary`. A fresh session has
/// a planned id before its first snapshot exists; exposing that id as resumable
/// sent automation toward a file that was never created. Keep the plan and its
/// materialization state together so the terminal id/state receipt cannot
/// disagree (cycle-18 automation audit).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SessionSnapshotState {
    Disabled,
    PendingUntilFirstSuccess,
    Resumable,
}

impl SessionSnapshotState {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::PendingUntilFirstSuccess => "pending_until_first_success",
            Self::Resumable => "resumable",
        }
    }
}

#[derive(Debug)]
pub(crate) struct JsonlSessionReceipt {
    pub(crate) planned_id: Option<String>,
    pub(crate) snapshot_state: SessionSnapshotState,
    pub(crate) persistence_state: kitsu_core::RunPersistenceState,
}

impl JsonlSessionReceipt {
    pub(crate) const fn new(
        planned_id: Option<String>,
        snapshot_state: SessionSnapshotState,
    ) -> Self {
        let persistence_state = match snapshot_state {
            SessionSnapshotState::Disabled => kitsu_core::RunPersistenceState::NotRequested,
            SessionSnapshotState::PendingUntilFirstSuccess | SessionSnapshotState::Resumable => {
                kitsu_core::RunPersistenceState::Preserved
            }
        };
        Self {
            planned_id,
            snapshot_state,
            persistence_state,
        }
    }

    pub(crate) const fn disabled() -> Self {
        Self::new(None, SessionSnapshotState::Disabled)
    }

    pub(crate) fn terminal_id(&self) -> Option<String> {
        (self.snapshot_state == SessionSnapshotState::Resumable)
            .then(|| self.planned_id.clone())
            .flatten()
    }

    pub(crate) const fn persistence_state(&self) -> kitsu_core::RunPersistenceState {
        self.persistence_state
    }

    pub(crate) fn mark_committed(&mut self, session_id: &str) {
        self.planned_id = Some(session_id.to_string());
        self.snapshot_state = SessionSnapshotState::Resumable;
        self.persistence_state = kitsu_core::RunPersistenceState::Committed;
    }

    pub(crate) fn mark_failed(&mut self) {
        if self.persistence_state != kitsu_core::RunPersistenceState::NotRequested {
            self.persistence_state = kitsu_core::RunPersistenceState::Failed;
        }
    }
}

pub(crate) static JSONL_SESSION_RECEIPT: Mutex<JsonlSessionReceipt> =
    Mutex::new(JsonlSessionReceipt::disabled());

pub(crate) fn jsonl_requested() -> bool {
    std::env::args_os()
        .skip(1)
        .take_while(|arg| arg != "--")
        .any(|arg| arg == "--jsonl" || arg.to_str().is_some_and(|arg| arg.starts_with("--jsonl=")))
}

pub(crate) fn run_spec() -> &'static kitsu_core::RunSpec {
    static RUN_SPEC: OnceLock<kitsu_core::RunSpec> = OnceLock::new();
    RUN_SPEC.get_or_init(|| {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let run_id = kitsu_core::RunId::new(format!("{}-{nanos}", std::process::id()))
            .expect("process-generated run identity is machine-safe and bounded");
        kitsu_core::RunSpec::new(run_id)
    })
}

pub(crate) fn run_id() -> &'static str {
    run_spec().run_id().as_str()
}

pub(crate) fn process_run_lifecycle() -> &'static Mutex<kitsu_core::RunLifecycle> {
    PROCESS_RUN_LIFECYCLE.get_or_init(|| Mutex::new(kitsu_core::RunLifecycle::accept(run_spec())))
}

pub(crate) fn activate_model_command(carrier: InferenceCarrier) -> Result<(), CliError> {
    PROCESS_INFERENCE_CARRIER.set(carrier).map_err(|_| {
        CliError::user("internal inference carrier was initialized more than once".to_string())
    })?;
    MODEL_COMMAND_ACTIVE.store(true, Ordering::Release);
    let _ = process_run_lifecycle();
    Ok(())
}

pub(crate) fn mark_model_inference_started() {
    MODEL_INFERENCE_STARTED.store(true, Ordering::Release);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ProcessTerminalFacts {
    pub(crate) cleanup: kitsu_core::RunCleanupState,
    pub(crate) persistence: kitsu_core::RunPersistenceState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct InteractiveTerminalFacts {
    pub(crate) outcome: kitsu_core::RunOutcomeClass,
    pub(crate) assistant: kitsu_core::RunAssistantDisposition,
    pub(crate) cleanup: kitsu_core::RunCleanupState,
    pub(crate) persistence: kitsu_core::RunPersistenceState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct InteractiveRunAggregate {
    pub(crate) successful_operation: bool,
    pub(crate) primary_non_success: Option<kitsu_core::RunOutcomeClass>,
    pub(crate) assistant: kitsu_core::RunAssistantDisposition,
    pub(crate) cleanup: kitsu_core::RunCleanupState,
    pub(crate) replaced_turns: u32,
}

impl Default for InteractiveRunAggregate {
    fn default() -> Self {
        Self {
            successful_operation: false,
            primary_non_success: None,
            assistant: kitsu_core::RunAssistantDisposition::Absent,
            cleanup: kitsu_core::RunCleanupState::Joined,
            replaced_turns: 0,
        }
    }
}

impl InteractiveRunAggregate {
    pub(crate) fn record_non_success(&mut self, outcome: kitsu_core::RunOutcomeClass) {
        debug_assert!(!matches!(outcome, kitsu_core::RunOutcomeClass::Succeeded));
        self.primary_non_success.get_or_insert(outcome);
    }

    pub(crate) fn record_turn(
        &mut self,
        outcome: kitsu_core::RunOutcomeClass,
        assistant: kitsu_core::RunAssistantDisposition,
    ) {
        match outcome {
            kitsu_core::RunOutcomeClass::Succeeded => self.successful_operation = true,
            kitsu_core::RunOutcomeClass::PartiallySucceeded => {
                self.successful_operation = true;
                self.primary_non_success
                    .get_or_insert(kitsu_core::RunOutcomeClass::PartiallySucceeded);
            }
            other => self.record_non_success(other),
        }
        self.assistant = assistant;
    }

    pub(crate) fn record_replaced_turn(&mut self) {
        self.replaced_turns = self.replaced_turns.saturating_add(1);
        self.assistant = kitsu_core::RunAssistantDisposition::Discarded;
    }

    pub(crate) fn record_background_snapshot(
        &mut self,
        snapshot: &subagent::AgentProgressSnapshot,
    ) {
        self.successful_operation |= snapshot.done > 0;
        if snapshot.failed > 0 {
            self.primary_non_success
                .get_or_insert(kitsu_core::RunOutcomeClass::ProviderFault);
        }
        if snapshot.cancelled > 0 {
            self.primary_non_success
                .get_or_insert(kitsu_core::RunOutcomeClass::Cancelled);
        }
    }

    pub(crate) fn observe_cleanup(&mut self, cleanup: kitsu_core::RunCleanupState) {
        use kitsu_core::RunCleanupState::{Joined, NotObserved, Residue};
        self.cleanup = match (self.cleanup, cleanup) {
            (NotObserved, _) | (_, NotObserved) => NotObserved,
            (Joined, state) => state,
            (state, Joined) => state,
            (Residue { unjoined: left }, Residue { unjoined: right }) => Residue {
                unjoined: std::num::NonZeroU32::new(left.get().saturating_add(right.get()))
                    .expect("sum of nonzero cleanup residue remains nonzero"),
            },
        };
    }

    pub(crate) fn outcome(&self) -> kitsu_core::RunOutcomeClass {
        match (self.successful_operation, self.primary_non_success) {
            (true, Some(_)) => kitsu_core::RunOutcomeClass::PartiallySucceeded,
            (true, None) | (false, None) => kitsu_core::RunOutcomeClass::Succeeded,
            (false, Some(outcome)) => outcome,
        }
    }

    pub(crate) const fn assistant(&self) -> kitsu_core::RunAssistantDisposition {
        self.assistant
    }

    #[cfg(test)]
    pub(crate) const fn replaced_turns(&self) -> u32 {
        self.replaced_turns
    }

    pub(crate) fn terminal_facts(
        &self,
        persistence: kitsu_core::RunPersistenceState,
    ) -> InteractiveTerminalFacts {
        InteractiveTerminalFacts {
            outcome: self.outcome(),
            assistant: self.assistant(),
            cleanup: self.cleanup,
            persistence,
        }
    }
}

pub(crate) fn record_interactive_terminal_facts(
    facts: InteractiveTerminalFacts,
) -> Result<(), CliError> {
    PROCESS_INTERACTIVE_TERMINAL_FACTS
        .set(facts)
        .map_err(|_| CliError::user("interactive terminal facts were already recorded"))
}

pub(crate) fn record_process_terminal_facts_in(
    receipt: &OnceLock<ProcessTerminalFacts>,
    facts: ProcessTerminalFacts,
) -> Result<(), &'static str> {
    receipt
        .set(facts)
        .map_err(|_| "process terminal facts were already recorded")
}

pub(crate) fn record_process_terminal_facts(facts: ProcessTerminalFacts) -> Result<(), CliError> {
    record_process_terminal_facts_in(&PROCESS_TERMINAL_FACTS, facts).map_err(CliError::user)
}

pub(crate) fn process_terminal_facts_or_fallback() -> ProcessTerminalFacts {
    PROCESS_TERMINAL_FACTS
        .get()
        .copied()
        .unwrap_or(ProcessTerminalFacts {
            cleanup: if MODEL_INFERENCE_STARTED.load(Ordering::Acquire) {
                kitsu_core::RunCleanupState::NotObserved
            } else {
                kitsu_core::RunCleanupState::Joined
            },
            persistence: jsonl_session_persistence_state(),
        })
}

pub(crate) fn record_human_terminal_facts(
    cleanup: kitsu_core::RunCleanupState,
) -> Result<(), CliError> {
    record_process_terminal_facts(ProcessTerminalFacts {
        cleanup,
        persistence: jsonl_session_persistence_state(),
    })
}

pub(crate) fn record_human_terminal_facts_preserving_error(cleanup: kitsu_core::RunCleanupState) {
    if let Err(error) = record_human_terminal_facts(cleanup) {
        eprintln!("~ additional terminal fact handoff failure: {error}");
    }
}

pub(crate) fn run_elapsed() -> Duration {
    RUN_STARTED_AT.get_or_init(Instant::now).elapsed()
}

pub(crate) fn run_elapsed_ms() -> u64 {
    u64::try_from(run_elapsed().as_millis()).unwrap_or(u64::MAX)
}

pub(crate) fn record_successful_operation() {
    SUCCESSFUL_OPERATIONS.fetch_add(1, Ordering::AcqRel);
}

pub(crate) fn jsonl_emitter() -> &'static Mutex<kitsu_render::jsonl::JsonlEmitter<std::io::Stdout>>
{
    static EMITTER: OnceLock<Mutex<kitsu_render::jsonl::JsonlEmitter<std::io::Stdout>>> =
        OnceLock::new();
    EMITTER.get_or_init(|| {
        Mutex::new(kitsu_render::jsonl::JsonlEmitter::new(
            std::io::stdout(),
            JSONL_SCHEMA,
            run_id(),
        ))
    })
}

pub(crate) fn emit_jsonl(kind: &str, payload: serde_json::Value) -> Result<(), CliError> {
    jsonl_emitter()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .emit(kind, payload)
        .map_err(CliError::jsonl_output)
}

pub(crate) fn jsonl_output_failure() -> Option<CliError> {
    jsonl_emitter()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .failure()
        .cloned()
        .map(CliError::jsonl_output)
}

/// Run-level counts of individual foreground tool-call errors. These are
/// deliberately separate from `*_failures`, which count failed requested
/// operations/turns. Policy errors are a subset of tool errors; recovered
/// errors belong to a turn that ultimately committed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ToolErrorTotals {
    pub(crate) tool_errors: u64,
    pub(crate) policy_errors: u64,
    pub(crate) recovered_tool_errors: u64,
    pub(crate) recovered_policy_errors: u64,
}

impl ToolErrorTotals {
    pub(crate) fn from_turn(status: &kitsu_core::AgentTurnStatus, recovered: bool) -> Self {
        let mut totals = Self::default();
        totals.record_turn(status, recovered);
        totals
    }

    pub(crate) fn record_turn(&mut self, status: &kitsu_core::AgentTurnStatus, recovered: bool) {
        let tool_errors = u64::from(status.tool_errors);
        let policy_errors = u64::from(status.policy_errors);
        debug_assert!(policy_errors <= tool_errors);
        self.tool_errors = self.tool_errors.saturating_add(tool_errors);
        self.policy_errors = self.policy_errors.saturating_add(policy_errors);
        if recovered {
            self.recovered_tool_errors = self.recovered_tool_errors.saturating_add(tool_errors);
            self.recovered_policy_errors =
                self.recovered_policy_errors.saturating_add(policy_errors);
        }
    }
}

pub(crate) fn emit_jsonl_summary(
    status: &str,
    failures: usize,
    provider_failures: usize,
) -> Result<(), CliError> {
    emit_jsonl_summary_with_local_failures(
        status,
        failures,
        provider_failures,
        0,
        0,
        0,
        ToolErrorTotals::default(),
    )
}

pub(crate) fn emit_jsonl_summary_with_local_failures(
    status: &str,
    failures: usize,
    provider_failures: usize,
    output_schema_failures: usize,
    tool_failures: usize,
    policy_failures: usize,
    tool_error_totals: ToolErrorTotals,
) -> Result<(), CliError> {
    emit_jsonl_summary_with_postcondition_failures(
        status,
        failures,
        provider_failures,
        output_schema_failures,
        0,
        tool_failures,
        policy_failures,
        tool_error_totals,
    )
}

pub(crate) fn jsonl_summary_outcome(
    status: &str,
    provider_failures: usize,
    tool_failures: usize,
    policy_failures: usize,
) -> Result<kitsu_core::RunOutcomeClass, CliError> {
    match status {
        "ok" => Ok(kitsu_core::RunOutcomeClass::Succeeded),
        "partial" => Ok(kitsu_core::RunOutcomeClass::PartiallySucceeded),
        "cancelled" => Ok(kitsu_core::RunOutcomeClass::Cancelled),
        "error" if provider_failures > 0 => Ok(kitsu_core::RunOutcomeClass::ProviderFault),
        "error" if tool_failures > 0 || policy_failures > 0 => {
            Ok(kitsu_core::RunOutcomeClass::ToolFault)
        }
        "error" => Ok(kitsu_core::RunOutcomeClass::UserFault),
        unknown => Err(CliError::user(format!(
            "internal final summary status is not recognized: {unknown}"
        ))),
    }
}

/// Run the deliberately narrow `ask --jsonl` terminal path through the common
/// lifecycle. `ask` has no tools/children or session store, making Joined and
/// NotRequested structural facts rather than carrier guesses. The delivery
/// closure is injectable so provider-free tests can cut the sink boundary.
#[cfg(test)]
pub(crate) fn finalize_terminal_with_delivery<E>(
    outcome: kitsu_core::RunOutcomeClass,
    assistant: kitsu_core::RunAssistantDisposition,
    cleanup: kitsu_core::RunCleanupState,
    persistence: kitsu_core::RunPersistenceState,
    deliver: impl FnOnce() -> Result<(), E>,
) -> Result<(kitsu_core::PublishedRun, Result<(), E>), kitsu_core::RunPhaseError> {
    let mut lifecycle = kitsu_core::RunLifecycle::accept(run_spec());
    finalize_owned_terminal_with_delivery(
        &mut lifecycle,
        outcome,
        assistant,
        cleanup,
        persistence,
        deliver,
    )
}

pub(crate) fn finalize_owned_terminal_with_delivery<E>(
    lifecycle: &mut kitsu_core::RunLifecycle,
    outcome: kitsu_core::RunOutcomeClass,
    assistant: kitsu_core::RunAssistantDisposition,
    cleanup: kitsu_core::RunCleanupState,
    persistence: kitsu_core::RunPersistenceState,
    deliver: impl FnOnce() -> Result<(), E>,
) -> Result<(kitsu_core::PublishedRun, Result<(), E>), kitsu_core::RunPhaseError> {
    lifecycle.settle(outcome, assistant)?;
    lifecycle.join(cleanup)?;
    lifecycle.persist(persistence)?;
    let delivery = deliver();
    lifecycle.deliver(if delivery.is_ok() {
        kitsu_core::RunDeliveryState::Delivered
    } else {
        kitsu_core::RunDeliveryState::Failed
    })?;
    let published = lifecycle.publish()?;
    Ok((published, delivery))
}

pub(crate) fn finalize_process_terminal_with_delivery<E>(
    outcome: kitsu_core::RunOutcomeClass,
    assistant: kitsu_core::RunAssistantDisposition,
    cleanup: kitsu_core::RunCleanupState,
    persistence: kitsu_core::RunPersistenceState,
    deliver: impl FnOnce() -> Result<(), E>,
) -> Result<(kitsu_core::PublishedRun, Result<(), E>), kitsu_core::RunPhaseError> {
    let mut lifecycle = process_run_lifecycle()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let result = finalize_owned_terminal_with_delivery(
        &mut lifecycle,
        outcome,
        assistant,
        cleanup,
        persistence,
        deliver,
    );
    if let Ok((published, _)) = &result {
        let recorded = PROCESS_PUBLISHED_RUN.set(published.clone());
        debug_assert!(recorded.is_ok(), "process terminal publication is unique");
    }
    result
}

#[cfg(test)]
pub(crate) fn finalize_ask_terminal_with_delivery<E>(
    outcome: kitsu_core::RunOutcomeClass,
    assistant: kitsu_core::RunAssistantDisposition,
    deliver: impl FnOnce() -> Result<(), E>,
) -> Result<(kitsu_core::PublishedRun, Result<(), E>), kitsu_core::RunPhaseError> {
    finalize_terminal_with_delivery(
        outcome,
        assistant,
        kitsu_core::RunCleanupState::Joined,
        kitsu_core::RunPersistenceState::NotRequested,
        deliver,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_ask_jsonl_summary_with_local_failures(
    status: &str,
    failures: usize,
    provider_failures: usize,
    output_schema_failures: usize,
    tool_failures: usize,
    policy_failures: usize,
    tool_error_totals: ToolErrorTotals,
) -> Result<(), CliError> {
    let outcome = jsonl_summary_outcome(status, provider_failures, tool_failures, policy_failures)?;
    let assistant = run_assistant_disposition();
    let (published, delivery) = finalize_process_terminal_with_delivery(
        outcome,
        assistant,
        kitsu_core::RunCleanupState::Joined,
        kitsu_core::RunPersistenceState::NotRequested,
        || {
            emit_jsonl_summary_with_local_failures(
                status,
                failures,
                provider_failures,
                output_schema_failures,
                tool_failures,
                policy_failures,
                tool_error_totals,
            )
        },
    )
    .map_err(|error| CliError::user(format!("internal ask finalization failed: {error}")))?;
    debug_assert_eq!(published.terminal().class(), outcome);
    debug_assert_eq!(published.terminal().assistant(), assistant);
    delivery
}

pub(crate) fn publish_fallback_inference_jsonl_terminal(error: &CliError) -> Result<(), CliError> {
    debug_assert!(MODEL_COMMAND_ACTIVE.load(Ordering::Acquire));
    let outcome = error.outcome_class();
    let assistant = run_assistant_disposition();
    let facts = process_terminal_facts_or_fallback();
    let cancelled = error.exit_code() == EXIT_CANCELLED;
    let (published, delivery) = finalize_process_terminal_with_delivery(
        outcome,
        assistant,
        facts.cleanup,
        facts.persistence,
        || {
            emit_jsonl("error", error.jsonl_error_payload())?;
            emit_jsonl_summary(
                if cancelled { "cancelled" } else { "error" },
                1,
                usize::from(error.exit_code() == EXIT_PROVIDER),
            )
        },
    )
    .map_err(|phase_error| {
        CliError::user(format!(
            "internal fallback inference finalization failed: {phase_error}"
        ))
    })?;
    debug_assert_eq!(published.terminal().class(), outcome);
    debug_assert_eq!(published.terminal().assistant(), assistant);
    debug_assert_eq!(published.cleanup(), facts.cleanup);
    debug_assert_eq!(published.persistence(), facts.persistence);
    delivery
}

pub(crate) fn publish_piped_jsonl_terminal(
    cleanup: kitsu_core::RunCleanupState,
    status: &str,
    provider_failures: usize,
    tool_failures: usize,
    policy_failures: usize,
    deliver: impl FnOnce() -> Result<(), CliError>,
) -> Result<(), CliError> {
    let outcome = jsonl_summary_outcome(status, provider_failures, tool_failures, policy_failures)?;
    let assistant = run_assistant_disposition();
    let persistence = jsonl_session_persistence_state();
    let (published, delivery) =
        finalize_process_terminal_with_delivery(outcome, assistant, cleanup, persistence, deliver)
            .map_err(|error| {
                CliError::user(format!("internal piped finalization failed: {error}"))
            })?;
    debug_assert_eq!(published.terminal().class(), outcome);
    debug_assert_eq!(published.terminal().assistant(), assistant);
    debug_assert_eq!(published.cleanup(), cleanup);
    debug_assert_eq!(published.persistence(), persistence);
    delivery
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_jsonl_summary_with_postcondition_failures(
    status: &str,
    failures: usize,
    provider_failures: usize,
    output_schema_failures: usize,
    postcondition_failures: usize,
    tool_failures: usize,
    policy_failures: usize,
    tool_error_totals: ToolErrorTotals,
) -> Result<(), CliError> {
    debug_assert!(output_schema_failures <= provider_failures);
    let outcome = jsonl_summary_outcome(status, provider_failures, tool_failures, policy_failures)?;
    let terminal = run_spec().terminal(outcome, run_assistant_disposition());
    let cancelled_failures = usize::from(status == "cancelled" && failures > 0);
    let user_failures = failures
        .saturating_sub(provider_failures + tool_failures + policy_failures + cancelled_failures);
    debug_assert!(postcondition_failures <= user_failures);
    let (session_id, session_snapshot_state) = {
        let receipt = JSONL_SESSION_RECEIPT
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        (receipt.terminal_id(), receipt.snapshot_state.as_str())
    };
    let mut payload = serde_json::json!({
        "status": status,
        "outcome": terminal.class().as_str(),
        "assistant": terminal.assistant().as_str(),
        "failures": failures,
        "provider_failures": provider_failures,
        "output_schema_failures": output_schema_failures,
        "postcondition_failures": postcondition_failures,
        "tool_failures": tool_failures,
        "policy_failures": policy_failures,
        "tool_errors": tool_error_totals.tool_errors,
        "policy_errors": tool_error_totals.policy_errors,
        "recovered_tool_errors": tool_error_totals.recovered_tool_errors,
        "recovered_policy_errors": tool_error_totals.recovered_policy_errors,
        "user_failures": user_failures,
        "cancelled_failures": cancelled_failures,
        "successful_operations": SUCCESSFUL_OPERATIONS.load(Ordering::Acquire),
        "duration_ms": run_elapsed_ms(),
        "session_id": session_id,
        "session_snapshot_state": session_snapshot_state,
    });
    if let Some(state) = output_last_message_summary_state(postcondition_failures) {
        payload["output_last_message_state"] = serde_json::json!(state);
    }
    emit_jsonl("final_summary", payload)?;
    JSONL_SUMMARY_EMITTED.store(true, Ordering::Release);
    Ok(())
}

pub(crate) fn output_last_message_preserved_without_commit() -> bool {
    OUTPUT_LAST_MESSAGE_REQUESTED.load(Ordering::Acquire)
        && !FULLY_COMMITTED_ASSISTANT_SEEN.load(Ordering::Acquire)
}

pub(crate) fn output_last_message_summary_state(
    postcondition_failures: usize,
) -> Option<&'static str> {
    if !OUTPUT_LAST_MESSAGE_REQUESTED.load(Ordering::Acquire) {
        None
    } else if !FULLY_COMMITTED_ASSISTANT_SEEN.load(Ordering::Acquire) {
        Some("preserved_no_committed_assistant")
    } else if postcondition_failures > 0 {
        Some("preserved_postcondition_failure")
    } else {
        None
    }
}

pub(crate) fn set_jsonl_session_receipt(
    planned_id: Option<String>,
    snapshot_state: SessionSnapshotState,
) {
    *JSONL_SESSION_RECEIPT
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) =
        JsonlSessionReceipt::new(planned_id, snapshot_state);
}

pub(crate) fn mark_jsonl_session_resumable(session_id: &str) {
    JSONL_SESSION_RECEIPT
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .mark_committed(session_id);
}

pub(crate) fn mark_jsonl_session_persistence_failed() {
    JSONL_SESSION_RECEIPT
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .mark_failed();
}

pub(crate) fn jsonl_session_persistence_state() -> kitsu_core::RunPersistenceState {
    JSONL_SESSION_RECEIPT
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .persistence_state()
}

pub(crate) fn validate_output_last_message_surface(cli: &Cli) -> Result<(), CliError> {
    let Some(destination) = cli.output_last_message.as_deref() else {
        return Ok(());
    };
    let inference_surface = cli.print_prompt.is_some()
        || matches!(
            cli.command.as_ref(),
            None | Some(Command::Ask { .. })
                | Some(Command::Review { .. })
                | Some(Command::Chat { .. })
        );
    if !inference_surface {
        return Err(CliError::user(
            "--output-last-message is supported only by non-interactive ask, review, piped chat, and scripted chat"
                .to_string(),
        ));
    }
    if cli.jsonl && destination.to_str().is_none() {
        return Err(CliError::user(
            "--output-last-message PATH must be valid UTF-8 when combined with --jsonl".to_string(),
        ));
    }
    let interactive_surface = cli.print_prompt.is_none()
        && match cli.command.as_ref() {
            None => kitsu_render::stdin_is_tty(),
            Some(Command::Chat { script, .. }) => script.is_none() && kitsu_render::stdin_is_tty(),
            _ => false,
        };
    if interactive_surface {
        return Err(CliError::user(
            "--output-last-message is an automation mode; use --script, pipe one prompt, `ask`, or `review`"
                .to_string(),
        ));
    }
    session::validate_output_file_target(destination).map_err(CliError::user)
}

/// Refuse `--no-alt-screen` while nothing can honour it (**F62**).
///
/// The flag is the argv door onto `[tui] alternate_screen = "never"`, which
/// the config deserializer refuses for the same reason: there is no
/// in-viewport overlay path, so the agent view takes the alternate screen
/// whatever either door says. Refusing only the file would move the lie to
/// the command line rather than close it.
///
/// It fires for every subcommand, `doctor` included, because the flag is
/// `global = true`: a global flag refused on some subcommands and accepted on
/// others is a rule the user has to memorise, and `doctor` is exactly where a
/// silent lie is worst -- it is run to learn whether the setup is sound, and
/// reporting "all good" while ignoring a flag the user passed is the defect
/// this refusal exists to close.
pub(crate) fn validate_no_alt_screen_flag(cli: &Cli) -> Result<(), CliError> {
    crate::config::TuiConfig::default()
        .effective_alternate_screen(cli.no_alt_screen)
        .map(|_| ())
        .map_err(CliError::user)
}

pub(crate) fn validate_output_schema_surface(cli: &Cli) -> Result<(), CliError> {
    let Some(path) = cli.output_schema.as_deref() else {
        return Ok(());
    };
    let inference_surface = cli.print_prompt.is_some()
        || matches!(
            cli.command.as_ref(),
            None | Some(Command::Ask { .. })
                | Some(Command::Review { .. })
                | Some(Command::Chat { .. })
        );
    if !inference_surface {
        return Err(CliError::user(
            "--output-schema is supported only by non-interactive ask, review, piped chat, and scripted chat"
                .to_string(),
        ));
    }
    let interactive_surface = cli.print_prompt.is_none()
        && match cli.command.as_ref() {
            None => kitsu_render::stdin_is_tty(),
            Some(Command::Chat { script, .. }) => script.is_none() && kitsu_render::stdin_is_tty(),
            _ => false,
        };
    if interactive_surface {
        return Err(CliError::user(
            "--output-schema is an automation mode; use --script, pipe one prompt, `ask`, or `review`"
                .to_string(),
        ));
    }
    let schema = structured_output::OutputSchema::load(path).map_err(CliError::user)?;
    OUTPUT_SCHEMA
        .set(schema)
        .map_err(|_| CliError::user("--output-schema was initialized more than once".to_string()))
}

pub(crate) fn inference_command_requested(cli: &Cli) -> bool {
    cli.print_prompt.is_some()
        || matches!(
            cli.command.as_ref(),
            None | Some(Command::Ask { .. })
                | Some(Command::Review { .. })
                | Some(Command::Chat { .. })
        )
}

pub(crate) fn inference_carrier(cli: &Cli, stdin_is_tty: bool) -> Option<InferenceCarrier> {
    let human_or_jsonl = |human, jsonl| if cli.jsonl { jsonl } else { human };
    if cli.print_prompt.is_some() {
        return Some(human_or_jsonl(
            InferenceCarrier::AskHuman,
            InferenceCarrier::AskJsonl,
        ));
    }
    match cli.command.as_ref() {
        Some(Command::Ask { .. }) => Some(human_or_jsonl(
            InferenceCarrier::AskHuman,
            InferenceCarrier::AskJsonl,
        )),
        Some(Command::Review { .. }) => Some(human_or_jsonl(
            InferenceCarrier::PipedHuman,
            InferenceCarrier::PipedJsonl,
        )),
        Some(Command::Chat {
            script: Some(_), ..
        }) => Some(human_or_jsonl(
            InferenceCarrier::ScriptHuman,
            InferenceCarrier::ScriptJsonl,
        )),
        None | Some(Command::Chat { script: None, .. }) if stdin_is_tty && !cli.jsonl => {
            Some(InferenceCarrier::InteractiveHuman)
        }
        None | Some(Command::Chat { script: None, .. }) => Some(human_or_jsonl(
            InferenceCarrier::PipedHuman,
            InferenceCarrier::PipedJsonl,
        )),
        _ => None,
    }
}

pub(crate) fn active_output_schema() -> Option<&'static structured_output::OutputSchema> {
    OUTPUT_SCHEMA.get()
}

pub(crate) fn emit_output_schema_receipt(cli: &Cli) -> Result<(), CliError> {
    let Some(schema) = active_output_schema() else {
        return Ok(());
    };
    if cli.jsonl {
        emit_jsonl(
            "output_schema",
            serde_json::json!({
                "name": "kitsu_output",
                "bytes": schema.bytes,
                "sha256": schema.sha256,
                "strict": true,
                "provider_format": "json_schema",
                "schema_preflight_validated": true,
                "local_validation": true,
                "schema_content_included": false,
                "model_file_authority": false,
            }),
        )?;
    } else {
        eprintln!(
            "~ structured output: strict JSON Schema ({} bytes, sha256 {})",
            schema.bytes, schema.sha256
        );
    }
    Ok(())
}

/// Terminal state of a turn's provisional assistant-output transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AssistantDisposition {
    /// The turn succeeded; streamed output is the committed answer.
    Committed,
    /// The deadline expired after real streamed output: the delivered partial
    /// answer is kept and labeled partial instead of being destroyed.
    CommittedPartial,
    /// Strict rollback: streamed output must not be mistaken for an answer.
    Discarded,
}

pub(crate) fn record_run_assistant_disposition(disposition: AssistantDisposition) {
    let encoded = match disposition {
        AssistantDisposition::Committed => 1,
        AssistantDisposition::CommittedPartial => 2,
        AssistantDisposition::Discarded => 3,
    };
    RUN_ASSISTANT_DISPOSITION.store(encoded, Ordering::Release);
}

pub(crate) fn run_assistant_disposition() -> kitsu_core::RunAssistantDisposition {
    match RUN_ASSISTANT_DISPOSITION.load(Ordering::Acquire) {
        1 => kitsu_core::RunAssistantDisposition::Committed,
        2 => kitsu_core::RunAssistantDisposition::CommittedPartial,
        3 => kitsu_core::RunAssistantDisposition::Discarded,
        _ => kitsu_core::RunAssistantDisposition::Absent,
    }
}

impl AssistantDisposition {
    pub(crate) fn jsonl_status(self) -> &'static str {
        match self {
            Self::Committed => "committed",
            Self::CommittedPartial => "committed_partial",
            Self::Discarded => "discarded",
        }
    }
}
