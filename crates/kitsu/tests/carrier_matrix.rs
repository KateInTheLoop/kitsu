#[path = "../src/process_exit.rs"]
mod process_exit;
mod support;

use std::collections::BTreeSet;

use support::carrier_fixture::{
    applicability, observe, Applicability, AssistantDisposition, CleanupState, DeliveryState,
    EntryPoint, ExpectedOutcome, FixtureEvent, FixtureViolation, OutcomeClass, PersistenceState,
    Surface, CARRIERS, SCENARIOS,
};

const RUN_ID: &str = "fixture-run-001";

fn successful_trace(delivery: DeliveryState, exit_code: i32) -> Vec<FixtureEvent> {
    vec![
        FixtureEvent::IdentityAssigned(RUN_ID),
        FixtureEvent::Accepted(RUN_ID),
        FixtureEvent::Settled {
            run_id: RUN_ID,
            class: OutcomeClass::Succeeded,
            assistant: AssistantDisposition::Committed,
        },
        FixtureEvent::Joined {
            run_id: RUN_ID,
            cleanup: CleanupState::Joined,
        },
        FixtureEvent::Persisted {
            run_id: RUN_ID,
            state: PersistenceState::Committed,
        },
        FixtureEvent::DeliveryFinished {
            run_id: RUN_ID,
            state: delivery,
        },
        FixtureEvent::TerminalPublished(RUN_ID),
        FixtureEvent::ProcessExited(exit_code),
    ]
}

#[test]
fn carrier_matrix_covers_every_required_shape_and_scenario() {
    assert_eq!(CARRIERS.len(), 7);
    assert_eq!(SCENARIOS.len(), 17);

    let carrier_labels = CARRIERS
        .iter()
        .map(|carrier| carrier.label())
        .collect::<BTreeSet<_>>();
    let scenario_labels = SCENARIOS
        .iter()
        .map(|scenario| scenario.label())
        .collect::<BTreeSet<_>>();
    assert_eq!(carrier_labels.len(), CARRIERS.len());
    assert_eq!(scenario_labels.len(), SCENARIOS.len());

    let mut required = 0;
    let mut fail_closed = 0;
    let mut not_applicable = 0;
    for carrier in CARRIERS {
        for scenario in SCENARIOS {
            match applicability(carrier, scenario) {
                Applicability::Required => required += 1,
                Applicability::MustFailClosedWithoutPrompt => fail_closed += 1,
                Applicability::NotApplicable => not_applicable += 1,
            }
        }
    }

    assert_eq!(required + fail_closed + not_applicable, 7 * 17);
    assert_eq!(required, 107);
    assert_eq!(
        fail_closed, 6,
        "only interactive chat may consume confirmation EOF"
    );
    assert_eq!(not_applicable, 6);
}

#[test]
fn deterministic_oracle_accepts_one_ordered_success_terminal() {
    let observation = observe(&successful_trace(DeliveryState::Delivered, 0));
    let expected = ExpectedOutcome {
        run_id: RUN_ID,
        class: OutcomeClass::Succeeded,
        assistant: AssistantDisposition::Committed,
        cleanup: CleanupState::Joined,
        persistence: PersistenceState::Committed,
        delivery: DeliveryState::Delivered,
        terminal_count: 1,
        exit_code: 0,
    };

    assert_eq!(expected.mismatches(&observation), Vec::<&str>::new());
}

#[test]
fn delivery_failure_does_not_reclassify_semantic_success() {
    let observation = observe(&successful_trace(DeliveryState::Failed, 1));

    assert_eq!(observation.class, Some(OutcomeClass::Succeeded));
    assert_eq!(observation.assistant, Some(AssistantDisposition::Committed));
    assert_eq!(observation.persistence, Some(PersistenceState::Committed));
    assert_eq!(observation.delivery, Some(DeliveryState::Failed));
    assert_eq!(observation.exit_code, Some(1));
    assert!(observation.violations.is_empty());
}

#[test]
fn duplicate_settlement_is_rejected_without_rewriting_the_first_outcome() {
    let mut trace = successful_trace(DeliveryState::Delivered, 0);
    trace.insert(
        3,
        FixtureEvent::Settled {
            run_id: RUN_ID,
            class: OutcomeClass::ProviderFault,
            assistant: AssistantDisposition::Discarded,
        },
    );

    let observation = observe(&trace);
    assert_eq!(observation.class, Some(OutcomeClass::Succeeded));
    assert_eq!(observation.assistant, Some(AssistantDisposition::Committed));
    assert!(
        observation
            .violations
            .contains(&FixtureViolation::PhaseRegression),
        "duplicate settlement must violate strict phase order"
    );
}

#[test]
fn oracle_rejects_identity_phase_terminal_and_post_terminal_corruption() {
    let observation = observe(&[
        FixtureEvent::Accepted(RUN_ID),
        FixtureEvent::IdentityAssigned(RUN_ID),
        FixtureEvent::Joined {
            run_id: RUN_ID,
            cleanup: CleanupState::TypedResidue,
        },
        FixtureEvent::Settled {
            run_id: RUN_ID,
            class: OutcomeClass::Interrupted,
            assistant: AssistantDisposition::Discarded,
        },
        FixtureEvent::TerminalPublished(RUN_ID),
        FixtureEvent::TerminalPublished(RUN_ID),
        FixtureEvent::WorkObserved(RUN_ID),
        FixtureEvent::TerminalPublished("other-run"),
        FixtureEvent::ProcessExited(2),
    ]);

    for violation in [
        FixtureViolation::IdentityMissing,
        FixtureViolation::IdentityAssignedAfterAcceptance,
        FixtureViolation::PhaseRegression,
        FixtureViolation::DuplicateTerminal,
        FixtureViolation::WorkAfterTerminal,
        FixtureViolation::CrossRunEvent,
    ] {
        assert!(
            observation.violations.contains(&violation),
            "missing {violation:?}: {:?}",
            observation.violations
        );
    }
}

#[test]
fn oracle_keeps_outcome_dimensions_distinct() {
    let variants = [
        OutcomeClass::PartiallySucceeded,
        OutcomeClass::UserFault,
        OutcomeClass::ProviderFault,
        OutcomeClass::ToolFault,
        OutcomeClass::Cancelled,
        OutcomeClass::Interrupted,
    ];
    let assistant = [
        AssistantDisposition::Committed,
        AssistantDisposition::Discarded,
        AssistantDisposition::Absent,
    ];
    let persistence = [
        PersistenceState::Committed,
        PersistenceState::Preserved,
        PersistenceState::Failed,
        PersistenceState::NotRequested,
    ];
    let delivery = [
        DeliveryState::Delivered,
        DeliveryState::Failed,
        DeliveryState::NotAttempted,
    ];

    assert_eq!(variants.len(), 6);
    assert_eq!(assistant.len(), 3);
    assert_eq!(persistence.len(), 4);
    assert_eq!(delivery.len(), 3);
}

fn production_carrier(
    carrier: support::carrier_fixture::Carrier,
) -> process_exit::InferenceCarrier {
    match (carrier.entry, carrier.surface) {
        (EntryPoint::Ask, Surface::Human) => process_exit::InferenceCarrier::AskHuman,
        (EntryPoint::Ask, Surface::Jsonl) => process_exit::InferenceCarrier::AskJsonl,
        (EntryPoint::PipedChat, Surface::Human) => process_exit::InferenceCarrier::PipedHuman,
        (EntryPoint::PipedChat, Surface::Jsonl) => process_exit::InferenceCarrier::PipedJsonl,
        (EntryPoint::ScriptChat, Surface::Human) => process_exit::InferenceCarrier::ScriptHuman,
        (EntryPoint::ScriptChat, Surface::Jsonl) => process_exit::InferenceCarrier::ScriptJsonl,
        (EntryPoint::InteractiveChat, Surface::Human) => {
            process_exit::InferenceCarrier::InteractiveHuman
        }
        (EntryPoint::InteractiveChat, Surface::Jsonl) => {
            panic!("interactive JSONL is not one of the seven supported carriers")
        }
    }
}

fn published_run(
    class: kitsu_core::RunOutcomeClass,
    cleanup: kitsu_core::RunCleanupState,
    persistence: kitsu_core::RunPersistenceState,
    delivery: kitsu_core::RunDeliveryState,
) -> kitsu_core::PublishedRun {
    let spec = kitsu_core::RunSpec::new(
        kitsu_core::RunId::new("independent-exit-fixture").expect("valid fixture run id"),
    );
    let mut lifecycle = kitsu_core::RunLifecycle::accept(&spec);
    let assistant = match class {
        kitsu_core::RunOutcomeClass::Succeeded => kitsu_core::RunAssistantDisposition::Committed,
        kitsu_core::RunOutcomeClass::PartiallySucceeded => {
            kitsu_core::RunAssistantDisposition::CommittedPartial
        }
        kitsu_core::RunOutcomeClass::UserFault
        | kitsu_core::RunOutcomeClass::ProviderFault
        | kitsu_core::RunOutcomeClass::ToolFault
        | kitsu_core::RunOutcomeClass::Cancelled
        | kitsu_core::RunOutcomeClass::Interrupted => {
            kitsu_core::RunAssistantDisposition::Discarded
        }
    };
    lifecycle.settle(class, assistant).expect("settle fixture");
    lifecycle.join(cleanup).expect("join fixture");
    lifecycle.persist(persistence).expect("persist fixture");
    lifecycle.deliver(delivery).expect("deliver fixture");
    lifecycle.publish().expect("publish fixture")
}

#[test]
fn independent_matrix_evaluates_exact_production_exit_for_all_seven_carriers() {
    let semantic_cases = [
        (kitsu_core::RunOutcomeClass::Succeeded, 0, 0),
        (kitsu_core::RunOutcomeClass::PartiallySucceeded, 0, 0),
        (kitsu_core::RunOutcomeClass::UserFault, 1, 0),
        (kitsu_core::RunOutcomeClass::ProviderFault, 2, 0),
        (kitsu_core::RunOutcomeClass::ToolFault, 1, 0),
        (kitsu_core::RunOutcomeClass::Cancelled, 130, 0),
        (kitsu_core::RunOutcomeClass::Interrupted, 1, 0),
    ];

    for carrier in CARRIERS {
        let production = production_carrier(carrier);
        let interactive = carrier.entry == EntryPoint::InteractiveChat;
        for (class, automation_exit, interactive_exit) in semantic_cases {
            let published = published_run(
                class,
                kitsu_core::RunCleanupState::Joined,
                kitsu_core::RunPersistenceState::Preserved,
                kitsu_core::RunDeliveryState::Delivered,
            );
            assert_eq!(
                process_exit::evaluate_process_exit(0, production, &published),
                if interactive {
                    interactive_exit
                } else {
                    automation_exit
                },
                "semantic exit mismatch for {} and {class:?}",
                carrier.label()
            );
        }

        for published in [
            published_run(
                kitsu_core::RunOutcomeClass::Succeeded,
                kitsu_core::RunCleanupState::NotObserved,
                kitsu_core::RunPersistenceState::Preserved,
                kitsu_core::RunDeliveryState::Delivered,
            ),
            published_run(
                kitsu_core::RunOutcomeClass::Succeeded,
                kitsu_core::RunCleanupState::Joined,
                kitsu_core::RunPersistenceState::Failed,
                kitsu_core::RunDeliveryState::Delivered,
            ),
            published_run(
                kitsu_core::RunOutcomeClass::Succeeded,
                kitsu_core::RunCleanupState::Joined,
                kitsu_core::RunPersistenceState::Preserved,
                kitsu_core::RunDeliveryState::Failed,
            ),
        ] {
            assert_eq!(
                process_exit::evaluate_process_exit(0, production, &published),
                process_exit::EXIT_USER,
                "later-phase failure must fail closed for {}",
                carrier.label()
            );
        }

        let clean = published_run(
            kitsu_core::RunOutcomeClass::Succeeded,
            kitsu_core::RunCleanupState::Joined,
            kitsu_core::RunPersistenceState::Preserved,
            kitsu_core::RunDeliveryState::Delivered,
        );
        assert_eq!(
            process_exit::evaluate_process_exit(process_exit::EXIT_PROVIDER, production, &clean),
            process_exit::EXIT_PROVIDER,
            "a prior causal failure must remain primary for {}",
            carrier.label()
        );
    }
}
