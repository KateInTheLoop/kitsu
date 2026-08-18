use std::num::NonZeroU32;

use kitsu_core::{
    RunAssistantDisposition, RunCleanupState, RunDeliveryState, RunEventKind, RunId, RunIdError,
    RunInterruption, RunLifecycle, RunOutcomeClass, RunPersistenceState, RunPhase, RunPhaseError,
    RunSpec,
};

#[test]
fn run_spec_assigns_identity_before_every_event_and_terminal() {
    let id = RunId::new("fixture-run-001").unwrap();
    let spec = RunSpec::new(id.clone());

    let accepted = spec.event(RunEventKind::Accepted);
    assert_eq!(accepted.run_id(), &id);
    assert_eq!(accepted.kind(), RunEventKind::Accepted);

    let interrupted = spec.event(RunEventKind::Interrupted(
        RunInterruption::AuthorityInputClosed,
    ));
    assert_eq!(interrupted.run_id(), &id);
    assert_eq!(
        interrupted.kind(),
        RunEventKind::Interrupted(RunInterruption::AuthorityInputClosed)
    );

    let terminal = spec.terminal(
        RunOutcomeClass::Interrupted,
        RunAssistantDisposition::Absent,
    );
    assert_eq!(terminal.run_id(), &id);
    assert_eq!(terminal.class(), RunOutcomeClass::Interrupted);
    assert_eq!(terminal.assistant(), RunAssistantDisposition::Absent);
}

#[test]
fn run_identity_is_bounded_nonempty_and_machine_safe() {
    assert_eq!(RunId::new("").unwrap_err(), RunIdError::Empty);
    assert_eq!(
        RunId::new("x".repeat(RunId::MAX_BYTES + 1)).unwrap_err(),
        RunIdError::TooLong {
            actual: RunId::MAX_BYTES + 1,
            max: RunId::MAX_BYTES,
        }
    );
    assert_eq!(
        RunId::new("unsafe/run").unwrap_err(),
        RunIdError::UnsafeByte { index: 6 }
    );
    assert_eq!(
        RunId::new("unsafe\nrun").unwrap_err(),
        RunIdError::UnsafeByte { index: 6 }
    );
    assert_eq!(
        RunId::new("unsafe run").unwrap_err(),
        RunIdError::UnsafeByte { index: 6 }
    );
    assert!(RunId::new("01J5:host_a.run-9").is_ok());
}

#[test]
fn semantic_class_and_assistant_disposition_remain_independent() {
    let spec = RunSpec::new(RunId::new("partial-run").unwrap());
    let terminal = spec.terminal(
        RunOutcomeClass::ProviderFault,
        RunAssistantDisposition::CommittedPartial,
    );

    assert_eq!(terminal.class(), RunOutcomeClass::ProviderFault);
    assert_eq!(
        terminal.assistant(),
        RunAssistantDisposition::CommittedPartial
    );
    assert_eq!(terminal.class().as_str(), "provider_fault");
    assert_eq!(terminal.assistant().as_str(), "committed_partial");
}

#[test]
fn neutral_outcome_labels_are_complete_and_stable() {
    assert_eq!(
        [
            RunOutcomeClass::Succeeded,
            RunOutcomeClass::PartiallySucceeded,
            RunOutcomeClass::UserFault,
            RunOutcomeClass::ProviderFault,
            RunOutcomeClass::ToolFault,
            RunOutcomeClass::Cancelled,
            RunOutcomeClass::Interrupted,
        ]
        .map(RunOutcomeClass::as_str),
        [
            "succeeded",
            "partially_succeeded",
            "user_fault",
            "provider_fault",
            "tool_fault",
            "cancelled",
            "interrupted",
        ]
    );
    assert_eq!(
        [
            RunAssistantDisposition::Committed,
            RunAssistantDisposition::CommittedPartial,
            RunAssistantDisposition::Discarded,
            RunAssistantDisposition::Absent,
        ]
        .map(RunAssistantDisposition::as_str),
        ["committed", "committed_partial", "discarded", "absent"]
    );
}

#[test]
fn lifecycle_publishes_one_complete_ordered_terminal() {
    let spec = RunSpec::new(RunId::new("ordered-run").unwrap());
    let mut lifecycle = RunLifecycle::accept(&spec);
    assert_eq!(lifecycle.phase(), RunPhase::Accepted);

    lifecycle
        .settle(
            RunOutcomeClass::Succeeded,
            RunAssistantDisposition::Committed,
        )
        .unwrap();
    lifecycle.join(RunCleanupState::Joined).unwrap();
    lifecycle.persist(RunPersistenceState::Committed).unwrap();
    lifecycle.deliver(RunDeliveryState::Delivered).unwrap();
    let published = lifecycle.publish().unwrap();

    assert_eq!(lifecycle.phase(), RunPhase::Published);
    assert_eq!(published.terminal().run_id(), spec.run_id());
    assert_eq!(published.terminal().class(), RunOutcomeClass::Succeeded);
    assert_eq!(published.cleanup(), RunCleanupState::Joined);
    assert_eq!(published.persistence(), RunPersistenceState::Committed);
    assert_eq!(published.delivery(), RunDeliveryState::Delivered);
    assert!(published.is_clean_success());
}

#[test]
fn later_phase_failures_never_rewrite_semantic_settlement() {
    let spec = RunSpec::new(RunId::new("residue-run").unwrap());
    let mut lifecycle = RunLifecycle::accept(&spec);
    lifecycle
        .settle(
            RunOutcomeClass::Succeeded,
            RunAssistantDisposition::Committed,
        )
        .unwrap();
    lifecycle
        .join(RunCleanupState::Residue {
            unjoined: NonZeroU32::new(2).unwrap(),
        })
        .unwrap();
    lifecycle.persist(RunPersistenceState::Failed).unwrap();
    lifecycle.deliver(RunDeliveryState::Failed).unwrap();
    let published = lifecycle.publish().unwrap();

    assert_eq!(published.terminal().class(), RunOutcomeClass::Succeeded);
    assert_eq!(
        published.terminal().assistant(),
        RunAssistantDisposition::Committed
    );
    assert_eq!(
        published.cleanup(),
        RunCleanupState::Residue {
            unjoined: NonZeroU32::new(2).unwrap(),
        }
    );
    assert!(!published.is_clean_success());
}

#[test]
fn duplicate_and_out_of_order_transitions_are_rejected_without_mutation() {
    let spec = RunSpec::new(RunId::new("invalid-order-run").unwrap());
    let mut lifecycle = RunLifecycle::accept(&spec);

    assert_eq!(
        lifecycle.persist(RunPersistenceState::Committed),
        Err(RunPhaseError::OutOfOrder {
            current: RunPhase::Accepted,
            expected: RunPhase::Settled,
            attempted: RunPhase::Persisted,
        })
    );
    assert_eq!(lifecycle.phase(), RunPhase::Accepted);

    lifecycle
        .settle(RunOutcomeClass::UserFault, RunAssistantDisposition::Absent)
        .unwrap();
    assert_eq!(
        lifecycle.settle(
            RunOutcomeClass::Succeeded,
            RunAssistantDisposition::Committed,
        ),
        Err(RunPhaseError::Duplicate(RunPhase::Settled))
    );
    assert_eq!(
        lifecycle.terminal().unwrap().class(),
        RunOutcomeClass::UserFault,
        "a duplicate settlement must not rewrite the first outcome"
    );
    assert_eq!(lifecycle.phase(), RunPhase::Settled);
}

#[test]
fn terminal_publication_is_single_and_rejects_later_work() {
    let spec = RunSpec::new(RunId::new("terminal-run").unwrap());
    let mut lifecycle = RunLifecycle::accept(&spec);
    lifecycle
        .settle(
            RunOutcomeClass::Cancelled,
            RunAssistantDisposition::Discarded,
        )
        .unwrap();
    lifecycle.join(RunCleanupState::Joined).unwrap();
    lifecycle
        .persist(RunPersistenceState::NotRequested)
        .unwrap();
    lifecycle.deliver(RunDeliveryState::NotAttempted).unwrap();
    lifecycle.publish().unwrap();

    assert_eq!(
        lifecycle.publish(),
        Err(RunPhaseError::AfterTerminal {
            attempted: RunPhase::Published,
        })
    );
    assert_eq!(
        lifecycle.ensure_work_allowed(),
        Err(RunPhaseError::WorkAfterTerminal)
    );
    assert_eq!(lifecycle.phase(), RunPhase::Published);
}
