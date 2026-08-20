use kitsu_core::{
    CanonicalHistory, CanonicalOperation, CanonicalTerminal, CommitDurability,
    CorruptSessionReason, ExpectedRevision, LogicalHead, RevisionId, SchemaDimension,
    SessionAssistantDisposition, SessionCommitOutcome, SessionCommitReceipt, SessionHistoryError,
    SessionId, SessionIdentityError, SessionIdentityKind, SessionIdentityViolation,
    SessionLookupState, SessionOutcomeClass, StoreId, TurnId, MAX_CANONICAL_OPERATIONS,
};

fn store() -> StoreId {
    StoreId::new("store-v1-local").unwrap()
}

fn session() -> SessionId {
    SessionId::new("s-fixture-001").unwrap()
}

fn revision(value: &str) -> RevisionId {
    RevisionId::new(value).unwrap()
}

fn turn(value: &str) -> TurnId {
    TurnId::new(value).unwrap()
}

fn terminal(
    run: &str,
    class: SessionOutcomeClass,
    assistant: SessionAssistantDisposition,
) -> CanonicalTerminal {
    CanonicalTerminal::new(kitsu_core::RunId::new(run).unwrap(), class, assistant)
}

#[test]
fn opaque_session_identities_are_bounded_typed_and_machine_safe() {
    assert_eq!(
        RevisionId::new(""),
        Err(SessionIdentityError {
            kind: SessionIdentityKind::Revision,
            violation: SessionIdentityViolation::Empty,
        })
    );
    assert_eq!(
        TurnId::new("unsafe/turn"),
        Err(SessionIdentityError {
            kind: SessionIdentityKind::Turn,
            violation: SessionIdentityViolation::UnsafeByte { index: 6 },
        })
    );
    assert_eq!(
        StoreId::new("x".repeat(StoreId::MAX_BYTES + 1)),
        Err(SessionIdentityError {
            kind: SessionIdentityKind::Store,
            violation: SessionIdentityViolation::TooLong {
                actual: StoreId::MAX_BYTES + 1,
                max: StoreId::MAX_BYTES,
            },
        })
    );
    assert_eq!(
        serde_json::to_value(store()).unwrap(),
        serde_json::json!("store-v1-local")
    );
    assert!(serde_json::from_value::<RevisionId>(serde_json::json!("bad revision")).is_err());
}

#[test]
fn canonical_operations_advance_or_retain_head_from_terminal_truth() {
    let first = turn("turn-v1-001");
    let advanced = CanonicalOperation::new(
        first.clone(),
        terminal(
            "run-advance",
            SessionOutcomeClass::Succeeded,
            SessionAssistantDisposition::Committed,
        ),
        LogicalHead::Genesis,
        LogicalHead::Turn(first.clone()),
    )
    .unwrap();
    assert_eq!(advanced.after_head(), &LogicalHead::Turn(first.clone()));

    let retained = CanonicalOperation::new(
        turn("turn-v1-002"),
        terminal(
            "run-retain",
            SessionOutcomeClass::Cancelled,
            SessionAssistantDisposition::Discarded,
        ),
        LogicalHead::Turn(first.clone()),
        LogicalHead::Turn(first.clone()),
    )
    .unwrap();
    assert_eq!(retained.before_head(), retained.after_head());

    assert!(CanonicalOperation::new(
        turn("turn-v1-bad-advance"),
        terminal(
            "run-bad-advance",
            SessionOutcomeClass::Succeeded,
            SessionAssistantDisposition::Committed,
        ),
        LogicalHead::Genesis,
        LogicalHead::Genesis,
    )
    .is_err());

    let mut malformed = serde_json::to_value(advanced).unwrap();
    malformed["after_head"] = serde_json::json!({"kind": "genesis"});
    assert!(serde_json::from_value::<CanonicalOperation>(malformed).is_err());
    assert!(CanonicalOperation::new(
        turn("turn-v1-bad-retain"),
        terminal(
            "run-bad-retain",
            SessionOutcomeClass::ProviderFault,
            SessionAssistantDisposition::Absent,
        ),
        LogicalHead::Genesis,
        LogicalHead::Turn(turn("turn-v1-bad-retain")),
    )
    .is_err());
}

#[test]
fn canonical_history_has_one_fixed_closed_vector() {
    let first_turn = turn("turn-v1-001");
    let failed_turn = turn("turn-v1-002");
    let head = LogicalHead::Turn(first_turn.clone());
    let history = CanonicalHistory::new(
        vec![
            CanonicalOperation::new(
                first_turn.clone(),
                terminal(
                    "run-history-1",
                    SessionOutcomeClass::Succeeded,
                    SessionAssistantDisposition::Committed,
                ),
                LogicalHead::Genesis,
                head.clone(),
            )
            .unwrap(),
            CanonicalOperation::new(
                failed_turn,
                terminal(
                    "run-history-2",
                    SessionOutcomeClass::ProviderFault,
                    SessionAssistantDisposition::Discarded,
                ),
                head.clone(),
                head.clone(),
            )
            .unwrap(),
        ],
        head.clone(),
    )
    .unwrap();

    let value = serde_json::to_value(&history).unwrap();
    assert_eq!(
        value,
        serde_json::json!({
            "contract_revision": 1,
            "operations": [
                {
                    "turn_id": "turn-v1-001",
                    "terminal": {
                        "run_id": "run-history-1",
                        "class": "succeeded",
                        "assistant": "committed"
                    },
                    "before_head": {"kind": "genesis"},
                    "after_head": {"kind": "turn", "turn": "turn-v1-001"}
                },
                {
                    "turn_id": "turn-v1-002",
                    "terminal": {
                        "run_id": "run-history-2",
                        "class": "provider_fault",
                        "assistant": "discarded"
                    },
                    "before_head": {"kind": "turn", "turn": "turn-v1-001"},
                    "after_head": {"kind": "turn", "turn": "turn-v1-001"}
                }
            ],
            "selected_head": {"kind": "turn", "turn": "turn-v1-001"}
        })
    );
    assert_eq!(
        serde_json::from_value::<CanonicalHistory>(value).unwrap(),
        history
    );
}

#[test]
fn history_accepts_valid_ancestor_head_but_rejects_unknowns_and_duplicates() {
    let first = turn("turn-v1-001");
    let operation = CanonicalOperation::new(
        first.clone(),
        terminal(
            "run-history",
            SessionOutcomeClass::Succeeded,
            SessionAssistantDisposition::Committed,
        ),
        LogicalHead::Genesis,
        LogicalHead::Turn(first.clone()),
    )
    .unwrap();
    assert_eq!(
        CanonicalHistory::new(
            vec![operation.clone()],
            LogicalHead::Turn(turn("turn-v1-unknown"))
        ),
        Err(SessionHistoryError::SelectedHeadUnknown {
            observed: LogicalHead::Turn(turn("turn-v1-unknown")),
        })
    );
    assert_eq!(
        CanonicalHistory::new(vec![operation.clone(), operation], LogicalHead::Turn(first)),
        Err(SessionHistoryError::DuplicateTurn {
            index: 1,
            turn: turn("turn-v1-001"),
        })
    );

    let unknown = turn("turn-v1-missing-parent");
    let orphan = CanonicalOperation::new(
        turn("turn-v1-orphan"),
        terminal(
            "run-v1-orphan",
            SessionOutcomeClass::Cancelled,
            SessionAssistantDisposition::Discarded,
        ),
        LogicalHead::Turn(unknown.clone()),
        LogicalHead::Turn(unknown.clone()),
    )
    .unwrap();
    assert_eq!(
        CanonicalHistory::new(vec![orphan], LogicalHead::Genesis),
        Err(SessionHistoryError::UnknownParentHead {
            index: 0,
            observed: LogicalHead::Turn(unknown),
        })
    );

    let first = turn("turn-v1-run-a");
    let second = turn("turn-v1-run-b");
    let shared_run = "run-v1-duplicate";
    let first_head = LogicalHead::Turn(first.clone());
    let second_head = LogicalHead::Turn(second.clone());
    let duplicate_run = CanonicalHistory::new(
        vec![
            CanonicalOperation::new(
                first,
                terminal(
                    shared_run,
                    SessionOutcomeClass::Succeeded,
                    SessionAssistantDisposition::Committed,
                ),
                LogicalHead::Genesis,
                first_head.clone(),
            )
            .unwrap(),
            CanonicalOperation::new(
                second,
                terminal(
                    shared_run,
                    SessionOutcomeClass::Succeeded,
                    SessionAssistantDisposition::Committed,
                ),
                first_head,
                second_head.clone(),
            )
            .unwrap(),
        ],
        second_head,
    );
    assert!(matches!(
        duplicate_run,
        Err(SessionHistoryError::DuplicateRun { index: 1, .. })
    ));

    let first = turn("turn-v1-ancestor-a");
    let second = turn("turn-v1-ancestor-b");
    let first_head = LogicalHead::Turn(first.clone());
    let second_head = LogicalHead::Turn(second.clone());
    let ancestor_selected = CanonicalHistory::new(
        vec![
            CanonicalOperation::new(
                first,
                terminal(
                    "run-v1-ancestor-a",
                    SessionOutcomeClass::Succeeded,
                    SessionAssistantDisposition::Committed,
                ),
                LogicalHead::Genesis,
                first_head.clone(),
            )
            .unwrap(),
            CanonicalOperation::new(
                second,
                terminal(
                    "run-v1-ancestor-b",
                    SessionOutcomeClass::Succeeded,
                    SessionAssistantDisposition::Committed,
                ),
                first_head.clone(),
                second_head,
            )
            .unwrap(),
        ],
        first_head.clone(),
    )
    .unwrap();
    assert_eq!(ancestor_selected.selected_head(), &first_head);

    let bounded_operation = CanonicalOperation::new(
        turn("turn-v1-bound"),
        terminal(
            "run-v1-bound",
            SessionOutcomeClass::Succeeded,
            SessionAssistantDisposition::Committed,
        ),
        LogicalHead::Genesis,
        LogicalHead::Turn(turn("turn-v1-bound")),
    )
    .unwrap();
    assert!(matches!(
        CanonicalHistory::new(
            vec![bounded_operation; MAX_CANONICAL_OPERATIONS + 1],
            LogicalHead::Genesis
        ),
        Err(SessionHistoryError::TooManyOperations { actual, max })
            if actual == MAX_CANONICAL_OPERATIONS + 1 && max == MAX_CANONICAL_OPERATIONS
    ));

    let mut future =
        serde_json::to_value(CanonicalHistory::new(Vec::new(), LogicalHead::Genesis).unwrap())
            .unwrap();
    future["contract_revision"] = serde_json::json!(2);
    assert!(serde_json::from_value::<CanonicalHistory>(future).is_err());

    let mut unknown =
        serde_json::to_value(CanonicalHistory::new(Vec::new(), LogicalHead::Genesis).unwrap())
            .unwrap();
    unknown["ambient_head"] = serde_json::json!("tail");
    assert!(serde_json::from_value::<CanonicalHistory>(unknown).is_err());
}

#[test]
fn lookup_states_are_closed_typed_and_semantically_validated() {
    let states = [
        SessionLookupState::Missing {
            tombstone_revision: None,
        },
        SessionLookupState::Present {
            revision: revision("revision-v1-present"),
            head: LogicalHead::Genesis,
        },
        SessionLookupState::Corrupt {
            session: session(),
            observed_revision: Some(revision("revision-v1-corrupt")),
            reason: CorruptSessionReason::InvalidHead,
        },
        SessionLookupState::unsupported(SchemaDimension::Physical, 2, 1).unwrap(),
        SessionLookupState::conflict(
            ExpectedRevision::Exact(revision("revision-v1-expected")),
            Some(revision("revision-v1-observed")),
        )
        .unwrap(),
    ];
    for state in states {
        let value = serde_json::to_value(&state).unwrap();
        assert_eq!(value["contract_revision"], 1);
        assert_eq!(
            serde_json::from_value::<SessionLookupState>(value).unwrap(),
            state
        );
    }

    let mut future = serde_json::to_value(SessionLookupState::Present {
        revision: revision("revision-v1-future"),
        head: LogicalHead::Genesis,
    })
    .unwrap();
    future["contract_revision"] = serde_json::json!(2);
    assert!(serde_json::from_value::<SessionLookupState>(future).is_err());

    assert!(SessionLookupState::unsupported(SchemaDimension::Logical, 1, 1).is_err());
    assert!(SessionLookupState::conflict(ExpectedRevision::Missing, None).is_err());
    let same = revision("revision-v1-same");
    assert!(
        SessionLookupState::conflict(ExpectedRevision::Exact(same.clone()), Some(same)).is_err()
    );
    assert!(
        serde_json::from_value::<SessionLookupState>(serde_json::json!({
            "state": "present",
            "contract_revision": 1,
            "revision": "revision-v1-present",
            "head": {"kind": "genesis"},
            "path": "/not-canonical"
        }))
        .is_err()
    );
}

#[test]
fn commit_receipts_separate_publication_from_semantic_outcome() {
    let receipt = SessionCommitReceipt::new(
        store(),
        session(),
        ExpectedRevision::Exact(revision("revision-v1-base")),
        revision("revision-v1-next"),
        LogicalHead::Turn(turn("turn-v1-009")),
        CommitDurability::Confirmed,
    )
    .unwrap();
    let outcome = SessionCommitOutcome::Published { receipt };
    let value = serde_json::to_value(&outcome).unwrap();
    assert_eq!(
        value,
        serde_json::json!({
            "outcome": "published",
            "contract_revision": 1,
            "receipt": {
                "contract_revision": 1,
                "store": "store-v1-local",
                "session": "s-fixture-001",
                "base_revision": {
                    "kind": "exact",
                    "revision": "revision-v1-base"
                },
                "new_revision": "revision-v1-next",
                "head": {"kind": "turn", "turn": "turn-v1-009"},
                "durability": "confirmed"
            }
        })
    );
    assert_eq!(
        serde_json::from_value::<SessionCommitOutcome>(value).unwrap(),
        outcome
    );

    let mut future_outcome = serde_json::to_value(&outcome).unwrap();
    future_outcome["contract_revision"] = serde_json::json!(2);
    assert!(serde_json::from_value::<SessionCommitOutcome>(future_outcome).is_err());
    let mut future_receipt = serde_json::to_value(&outcome).unwrap();
    future_receipt["receipt"]["contract_revision"] = serde_json::json!(2);
    assert!(serde_json::from_value::<SessionCommitOutcome>(future_receipt).is_err());

    let not_published = SessionCommitOutcome::NotPublished {
        store: store(),
        session: session(),
        base_revision: ExpectedRevision::Missing,
    };
    let not_published_value = serde_json::to_value(&not_published).unwrap();
    assert_eq!(
        not_published_value,
        serde_json::json!({
            "outcome": "not_published",
            "contract_revision": 1,
            "store": "store-v1-local",
            "session": "s-fixture-001",
            "base_revision": {"kind": "missing"}
        })
    );
    assert_eq!(
        serde_json::from_value::<SessionCommitOutcome>(not_published_value).unwrap(),
        not_published
    );

    let same = revision("revision-v1-same");
    assert!(SessionCommitReceipt::new(
        store(),
        session(),
        ExpectedRevision::Exact(same.clone()),
        same,
        LogicalHead::Genesis,
        CommitDurability::PublishedUnconfirmed,
    )
    .is_err());
}
