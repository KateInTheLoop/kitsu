//! Process-exit evaluation for the seven inference carriers.
//!
//! This module deliberately depends only on the neutral published run contract.
//! Carrier presentation, provider behavior, JSONL framing, and session storage
//! cannot reinterpret semantic or later-phase truth here.

pub(crate) const EXIT_USER: i32 = 1;
pub(crate) const EXIT_PROVIDER: i32 = 2;
pub(crate) const EXIT_CANCELLED: i32 = 130;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InferenceCarrier {
    AskHuman,
    AskJsonl,
    PipedHuman,
    PipedJsonl,
    ScriptHuman,
    ScriptJsonl,
    InteractiveHuman,
}

impl InferenceCarrier {
    const fn is_interactive(self) -> bool {
        matches!(self, Self::InteractiveHuman)
    }
}

/// Convert an immutable lifecycle publication into process status without
/// rewriting its semantic outcome. A prior nonzero status is causal and wins;
/// later-phase failures fail closed only when the semantic path was otherwise
/// successful. Interactive chat keeps recoverable per-turn outcomes in its
/// terminal receipt without turning a deliberate `/quit` into process failure.
pub(crate) fn evaluate_process_exit(
    requested_code: i32,
    carrier: InferenceCarrier,
    published: &kitsu_core::PublishedRun,
) -> i32 {
    if requested_code != 0 {
        return requested_code;
    }
    if published.cleanup() != kitsu_core::RunCleanupState::Joined
        || published.persistence() == kitsu_core::RunPersistenceState::Failed
        || published.delivery() != kitsu_core::RunDeliveryState::Delivered
    {
        return EXIT_USER;
    }
    if carrier.is_interactive() {
        return 0;
    }
    match published.terminal().class() {
        kitsu_core::RunOutcomeClass::Succeeded
        | kitsu_core::RunOutcomeClass::PartiallySucceeded => 0,
        kitsu_core::RunOutcomeClass::ProviderFault => EXIT_PROVIDER,
        kitsu_core::RunOutcomeClass::Cancelled => EXIT_CANCELLED,
        kitsu_core::RunOutcomeClass::UserFault
        | kitsu_core::RunOutcomeClass::ToolFault
        | kitsu_core::RunOutcomeClass::Interrupted => EXIT_USER,
    }
}
