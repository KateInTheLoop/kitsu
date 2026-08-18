use kitsu_core::{
    RunAssistantDisposition, RunEventKind, RunId, RunIdError, RunInterruption, RunOutcomeClass,
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
}
