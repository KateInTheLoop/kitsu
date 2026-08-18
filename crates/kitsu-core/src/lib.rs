//! kitsu-core — the orchestrator agent loop. This is the heart of kitsu:
//! call model → parse tool calls → run tools → append results → repeat until the
//! model stops or a cap is hit (design §5). Loop BODY is implemented by the
//! Developer agent; the signature + config are pinned here as the contract.

pub mod context;
pub mod orchestrator;
pub mod run;

pub use context::{ContextBreakdown, ContextBudget, ContextMeter, DEFAULT_CONTEXT_WINDOW};
pub use kitsu_providers::ReasoningEffort;
pub use orchestrator::{
    bound_inline_output, compact_context, run_agent_turn, run_agent_turn_with_state, AgentConfig,
    AgentError, AgentEventLog, AgentRuntimeEvent, AgentSessionState, AgentTurnStatus,
    ArtifactStoragePolicy, BoundedInlineOutput, CommandRuntimeInvocation, CommandRuntimeResult,
    CompactionLedger, CompactionMode, CompactionOutcome, CompactionReason, CompactionRecord,
    CompactionReport, ContextUnit, ContextWindowEvidence, ProviderDeadlineScope,
    ProviderRequestPurpose, RunTallies, SpawnBudget, StructuredFinalizationGate,
    StructuredFinalizationRepair, StructuredFinalizationRepairTarget, SummaryFallbackReason,
    ToolArtifactStatus, TurnCompletion,
};
pub use run::{
    PublishedRun, RunAssistantDisposition, RunCleanupState, RunDeliveryState, RunEvent,
    RunEventKind, RunId, RunIdError, RunInterruption, RunLifecycle, RunOutcomeClass,
    RunPersistenceState, RunPhase, RunPhaseError, RunSpec, TerminalOutcome,
};
