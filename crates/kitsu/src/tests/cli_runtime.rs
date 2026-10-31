use super::*;

#[test]
fn ask_terminal_lifecycle_publishes_clean_delivery_once() {
    let deliveries = std::cell::Cell::new(0);
    let (published, delivery) = finalize_ask_terminal_with_delivery(
        kitsu_core::RunOutcomeClass::Succeeded,
        kitsu_core::RunAssistantDisposition::Committed,
        || {
            deliveries.set(deliveries.get() + 1);
            Ok::<_, &'static str>(())
        },
    )
    .expect("valid ask phases publish");

    assert!(delivery.is_ok());
    assert_eq!(deliveries.get(), 1);
    assert_eq!(
        published.terminal().class(),
        kitsu_core::RunOutcomeClass::Succeeded
    );
    assert_eq!(
        published.terminal().assistant(),
        kitsu_core::RunAssistantDisposition::Committed
    );
    assert_eq!(published.cleanup(), kitsu_core::RunCleanupState::Joined);
    assert_eq!(
        published.persistence(),
        kitsu_core::RunPersistenceState::NotRequested
    );
    assert_eq!(
        published.delivery(),
        kitsu_core::RunDeliveryState::Delivered
    );
    assert!(published.is_clean_success());
}

#[test]
fn ask_terminal_lifecycle_publishes_delivery_failure_without_reclassification() {
    let deliveries = std::cell::Cell::new(0);
    let (published, delivery) = finalize_ask_terminal_with_delivery(
        kitsu_core::RunOutcomeClass::ProviderFault,
        kitsu_core::RunAssistantDisposition::Discarded,
        || {
            deliveries.set(deliveries.get() + 1);
            Err::<(), _>("blocked sink")
        },
    )
    .expect("delivery failure is terminal phase data");

    assert_eq!(delivery.unwrap_err(), "blocked sink");
    assert_eq!(deliveries.get(), 1);
    assert_eq!(
        published.terminal().class(),
        kitsu_core::RunOutcomeClass::ProviderFault
    );
    assert_eq!(
        published.terminal().assistant(),
        kitsu_core::RunAssistantDisposition::Discarded
    );
    assert_eq!(published.delivery(), kitsu_core::RunDeliveryState::Failed);
    assert!(!published.is_clean_success());
}

#[test]
fn process_terminal_helper_rejects_a_second_cross_path_publication() {
    let spec = kitsu_core::RunSpec::new(
        kitsu_core::RunId::new("shared-process-owner").expect("valid fixture identity"),
    );
    let mut lifecycle = kitsu_core::RunLifecycle::accept(&spec);
    let deliveries = std::cell::Cell::new(0);

    let (_, first_delivery) = finalize_owned_terminal_with_delivery(
        &mut lifecycle,
        kitsu_core::RunOutcomeClass::Succeeded,
        kitsu_core::RunAssistantDisposition::Committed,
        kitsu_core::RunCleanupState::Joined,
        kitsu_core::RunPersistenceState::NotRequested,
        || {
            deliveries.set(deliveries.get() + 1);
            Ok::<_, &'static str>(())
        },
    )
    .expect("first carrier publishes");
    assert!(first_delivery.is_ok());

    let duplicate = finalize_owned_terminal_with_delivery(
        &mut lifecycle,
        kitsu_core::RunOutcomeClass::ProviderFault,
        kitsu_core::RunAssistantDisposition::Discarded,
        kitsu_core::RunCleanupState::Joined,
        kitsu_core::RunPersistenceState::NotRequested,
        || {
            deliveries.set(deliveries.get() + 1);
            Ok::<_, &'static str>(())
        },
    )
    .expect_err("another carrier cannot publish the process terminal again");

    assert_eq!(deliveries.get(), 1, "rejected publication cannot deliver");
    assert_eq!(
        duplicate,
        kitsu_core::RunPhaseError::AfterTerminal {
            attempted: kitsu_core::RunPhase::Settled,
        }
    );
}

#[test]
fn process_terminal_owner_is_reserved_only_for_inference_commands() {
    for argv in [
        vec!["kitsu"],
        vec!["kitsu", "ask", "hello"],
        vec!["kitsu", "review"],
        vec!["kitsu", "chat"],
        vec!["kitsu", "-p", "hello"],
    ] {
        let cli = Cli::try_parse_from(argv.clone()).expect("inference command parses");
        assert!(
            inference_command_requested(&cli),
            "expected inference command: {argv:?}"
        );
    }

    for argv in [
        vec!["kitsu", "init"],
        vec!["kitsu", "doctor"],
        vec!["kitsu", "models"],
        vec!["kitsu", "sessions"],
        vec!["kitsu", "artifacts"],
    ] {
        let cli = Cli::try_parse_from(argv.clone()).expect("management command parses");
        assert!(
            !inference_command_requested(&cli),
            "management command cannot reserve inference lifecycle: {argv:?}"
        );
    }
}

#[test]
fn inference_carrier_classification_covers_the_seven_supported_surfaces() {
    let cases = [
        (
            vec!["kitsu", "ask", "hello"],
            false,
            InferenceCarrier::AskHuman,
        ),
        (
            vec!["kitsu", "--jsonl", "ask", "hello"],
            false,
            InferenceCarrier::AskJsonl,
        ),
        (vec!["kitsu", "chat"], false, InferenceCarrier::PipedHuman),
        (
            vec!["kitsu", "--jsonl", "chat"],
            false,
            InferenceCarrier::PipedJsonl,
        ),
        (
            vec!["kitsu", "chat", "--script", "turns.txt"],
            true,
            InferenceCarrier::ScriptHuman,
        ),
        (
            vec!["kitsu", "--jsonl", "chat", "--script", "turns.txt"],
            true,
            InferenceCarrier::ScriptJsonl,
        ),
        (
            vec!["kitsu", "chat"],
            true,
            InferenceCarrier::InteractiveHuman,
        ),
    ];

    for (argv, stdin_is_tty, expected) in cases {
        let cli = Cli::try_parse_from(argv.clone()).expect("carrier fixture parses");
        assert_eq!(
            inference_carrier(&cli, stdin_is_tty),
            Some(expected),
            "wrong carrier for {argv:?}"
        );
    }

    let review = Cli::try_parse_from(["kitsu", "review"]).expect("review parses");
    assert_eq!(
        inference_carrier(&review, true),
        Some(InferenceCarrier::PipedHuman),
        "review is always a one-shot carrier even when stdin is a TTY"
    );
}

#[test]
fn process_terminal_fact_handoff_is_single_and_exact() {
    let receipt = OnceLock::new();
    let facts = ProcessTerminalFacts {
        cleanup: kitsu_core::RunCleanupState::Joined,
        persistence: kitsu_core::RunPersistenceState::Preserved,
    };

    record_process_terminal_facts_in(&receipt, facts).expect("first carrier owns facts");
    assert_eq!(receipt.get(), Some(&facts));
    assert!(record_process_terminal_facts_in(&receipt, facts).is_err());
}

#[test]
fn interactive_aggregate_is_not_last_turn_wins() {
    let mut failed_then_recovered = InteractiveRunAggregate::default();
    failed_then_recovered.record_turn(
        kitsu_core::RunOutcomeClass::ProviderFault,
        kitsu_core::RunAssistantDisposition::Discarded,
    );
    failed_then_recovered.record_turn(
        kitsu_core::RunOutcomeClass::Succeeded,
        kitsu_core::RunAssistantDisposition::Committed,
    );
    assert_eq!(
        failed_then_recovered.outcome(),
        kitsu_core::RunOutcomeClass::PartiallySucceeded
    );
    assert_eq!(
        failed_then_recovered.assistant(),
        kitsu_core::RunAssistantDisposition::Committed
    );

    let mut succeeded_then_failed = InteractiveRunAggregate::default();
    succeeded_then_failed.record_turn(
        kitsu_core::RunOutcomeClass::Succeeded,
        kitsu_core::RunAssistantDisposition::Committed,
    );
    succeeded_then_failed.record_turn(
        kitsu_core::RunOutcomeClass::Interrupted,
        kitsu_core::RunAssistantDisposition::Discarded,
    );
    assert_eq!(
        succeeded_then_failed.outcome(),
        kitsu_core::RunOutcomeClass::PartiallySucceeded
    );
    assert_eq!(
        succeeded_then_failed.assistant(),
        kitsu_core::RunAssistantDisposition::Discarded
    );
}

#[test]
fn interactive_retry_replacement_is_not_a_terminal_fault() {
    let mut aggregate = InteractiveRunAggregate::default();
    aggregate.record_replaced_turn();
    aggregate.record_turn(
        kitsu_core::RunOutcomeClass::Succeeded,
        kitsu_core::RunAssistantDisposition::Committed,
    );

    assert_eq!(aggregate.outcome(), kitsu_core::RunOutcomeClass::Succeeded);
    assert_eq!(aggregate.replaced_turns(), 1);
}

#[test]
fn interactive_terminal_facts_preserve_cleanup_and_persistence() {
    let mut aggregate = InteractiveRunAggregate::default();
    aggregate.record_turn(
        kitsu_core::RunOutcomeClass::Cancelled,
        kitsu_core::RunAssistantDisposition::Discarded,
    );
    aggregate.observe_cleanup(kitsu_core::RunCleanupState::Residue {
        unjoined: std::num::NonZeroU32::new(2).unwrap(),
    });
    let facts = aggregate.terminal_facts(kitsu_core::RunPersistenceState::Failed);

    assert_eq!(facts.outcome, kitsu_core::RunOutcomeClass::Cancelled);
    assert_eq!(
        facts.assistant,
        kitsu_core::RunAssistantDisposition::Discarded
    );
    assert_eq!(
        facts.cleanup,
        kitsu_core::RunCleanupState::Residue {
            unjoined: std::num::NonZeroU32::new(2).unwrap()
        }
    );
    assert_eq!(facts.persistence, kitsu_core::RunPersistenceState::Failed);
}

#[test]
fn session_receipt_tracks_persistence_independently_from_snapshot_addressability() {
    let disabled = JsonlSessionReceipt::new(None, SessionSnapshotState::Disabled);
    assert_eq!(
        disabled.persistence_state(),
        kitsu_core::RunPersistenceState::NotRequested
    );

    let mut pending = JsonlSessionReceipt::new(
        Some("planned-session".to_string()),
        SessionSnapshotState::PendingUntilFirstSuccess,
    );
    assert_eq!(
        pending.persistence_state(),
        kitsu_core::RunPersistenceState::Preserved
    );
    assert_eq!(pending.terminal_id(), None);

    pending.mark_committed("planned-session");
    assert_eq!(
        pending.persistence_state(),
        kitsu_core::RunPersistenceState::Committed
    );
    assert_eq!(pending.terminal_id().as_deref(), Some("planned-session"));

    pending.mark_failed();
    assert_eq!(
        pending.persistence_state(),
        kitsu_core::RunPersistenceState::Failed
    );
    assert_eq!(
        pending.terminal_id().as_deref(),
        Some("planned-session"),
        "post-install failure must not erase addressability"
    );
}

#[test]
fn tool_error_totals_separate_raw_errors_from_committed_recovery() {
    let failed = kitsu_core::AgentTurnStatus {
        tool_errors: 3,
        policy_errors: 1,
        ..Default::default()
    };
    let recovered = kitsu_core::AgentTurnStatus {
        tool_errors: 2,
        ..Default::default()
    };
    let mut totals = ToolErrorTotals::default();
    totals.record_turn(&failed, false);
    totals.record_turn(&recovered, true);

    assert_eq!(
        totals,
        ToolErrorTotals {
            tool_errors: 5,
            policy_errors: 1,
            recovered_tool_errors: 2,
            recovered_policy_errors: 0,
        }
    );
}

#[test]
fn artifact_allocator_skips_an_existing_generation_instead_of_reusing_it() {
    let root = tempfile::tempdir().unwrap();
    let occupied = root.path().join(format!("runtime-{}-0", run_id()));
    std::fs::create_dir(&occupied).unwrap();
    std::fs::write(occupied.join("prior.txt"), "prior session").unwrap();
    let mut allocator = ArtifactDirectoryAllocator::default();

    let allocated = allocator.allocate(root.path()).unwrap();

    assert_ne!(allocated, occupied);
    assert!(allocated.is_dir());
    assert_eq!(
        std::fs::read_to_string(occupied.join("prior.txt")).unwrap(),
        "prior session"
    );
}
