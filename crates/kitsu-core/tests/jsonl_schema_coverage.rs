//! Coverage of the published JSONL contract by the runtime event enum.
//!
//! `docs/schemas/kitsu-chat-v1.schema.json` is pinned for *structure* by
//! `kitsu-render`'s `published_schema_keeps_command_payload_and_event_definitions_distinct`,
//! which asserts that particular `$defs` exist and that particular `$ref`s
//! point where expected. It cannot assert *coverage*: `kitsu-render` has no
//! `kitsu-*` dependency (that is what makes "classic pays nothing" a
//! compile-time property), so it cannot see `AgentRuntimeEvent`. That is how
//! `tool_results_dropped` was added to the stream and never to the contract.
//!
//! These tests close that direction from `kitsu-core`, where the enum lives.
//!
//! ## How the variants are enumerated
//!
//! Rust has no reflection, so there is no runtime way to list the variants of
//! an enum. A hand-typed list of wire names is exactly the failure mode this
//! file exists to prevent, so instead the `runtime_event_samples!` macro below
//! takes one entry per variant and expands it into *two* things:
//!
//! 1. a sample value, used to obtain the wire name from `serde` itself, and
//! 2. an arm of a `match` over `&AgentRuntimeEvent` that has no wildcard.
//!
//! The compiler rejects that `match` when a variant has no arm, and the arm
//! and the sample come from the *same* macro entry, so a new variant cannot be
//! given an arm without also being given a sample. No wire name is written
//! down anywhere in this file: every name is read back out of the serialized
//! sample, which is what pins `#[serde(tag = "event", rename_all =
//! "snake_case")]` rather than assuming it.

use kitsu_core::{
    AgentRuntimeEvent, ProviderDeadlineScope, ProviderRequestPurpose,
    StructuredFinalizationRepairTarget, ToolArtifactStatus,
};
use kitsu_providers::{ProviderAttemptCategory, ProviderAttemptOutcome};

const PUBLISHED_SCHEMA: &str = include_str!("../../../docs/schemas/kitsu-chat-v1.schema.json");

/// Declares one entry per `AgentRuntimeEvent` variant and expands it into a
/// sample value plus a wildcard-free `match` arm. Adding a variant to the enum
/// breaks the generated `match`, and the only way to repair it is to add an
/// entry here, which also contributes the sample the coverage test walks.
macro_rules! runtime_event_samples {
    ( $( $pattern:pat => $sample:expr , )+ ) => {
        fn samples() -> Vec<AgentRuntimeEvent> {
            vec![ $( $sample ),+ ]
        }

        /// Exhaustiveness tripwire: no wildcard arm, so the compiler refuses a
        /// variant that `runtime_event_samples!` does not mention.
        fn assert_every_variant_is_declared(event: &AgentRuntimeEvent) {
            match event {
                $( $pattern => () ),+
            }
        }
    };
}

runtime_event_samples! {
    AgentRuntimeEvent::ProviderRequest { .. } => AgentRuntimeEvent::ProviderRequest {
        round: 1,
        purpose: ProviderRequestPurpose::Inference,
        model: Some("fixture-model".to_string()),
        message_count: 2,
        tools_offered: 3,
        structured_output: false,
        request_timeout_ms: Some(1_000),
        turn_remaining_ms: Some(2_000),
    },
    AgentRuntimeEvent::StructuredFinalizationDeferred { .. } =>
        AgentRuntimeEvent::StructuredFinalizationDeferred {
            round: 1,
            repair_round: 2,
            finalization_round_reserved: true,
            cause: "exact_output_mismatch".to_string(),
            targets: vec![StructuredFinalizationRepairTarget {
                path: "fixture.txt".to_string(),
                expected_sha256: "a".repeat(64),
                actual_sha256: "b".repeat(64),
            }],
            repair_instruction_disclosed: false,
            write_authority_granted: true,
        },
    AgentRuntimeEvent::ProviderAttempt { .. } => AgentRuntimeEvent::ProviderAttempt {
        round: 1,
        purpose: ProviderRequestPurpose::Inference,
        route_generation: Some(7),
        attempt: 1,
        max_attempts: 3,
        category: ProviderAttemptCategory::Success,
        status: Some(200),
        outcome: ProviderAttemptOutcome::BodyCompleted,
        terminal: true,
        retry_delay_ms: None,
        retry_after_used: false,
        deadline_scope: ProviderDeadlineScope::ProviderRequest,
        remaining_deadline_ms: 4_000,
        turn_remaining_ms: Some(5_000),
    },
    AgentRuntimeEvent::AssistantDelta { .. } => AgentRuntimeEvent::AssistantDelta {
        round: 1,
        text: "fixture".to_string(),
    },
    AgentRuntimeEvent::Usage { .. } => AgentRuntimeEvent::Usage {
        round: 1,
        purpose: ProviderRequestPurpose::Inference,
        prompt_tokens: 10,
        completion_tokens: 20,
        total_tokens: 30,
    },
    AgentRuntimeEvent::ToolStarted { .. } => AgentRuntimeEvent::ToolStarted {
        round: 1,
        call_id: "call-1".to_string(),
        name: "fs_read_file".to_string(),
        args: "{}".to_string(),
        command: None,
    },
    AgentRuntimeEvent::ToolFinished { .. } => AgentRuntimeEvent::ToolFinished {
        round: 1,
        call_id: "call-1".to_string(),
        name: "fs_read_file".to_string(),
        success: true,
        cancelled: false,
        output: "ok".to_string(),
        output_bytes: 2,
        output_retained_bytes: 2,
        output_truncated: false,
        artifact_status: ToolArtifactStatus::NotNeeded,
        command: None,
    },
    AgentRuntimeEvent::ToolResultsDropped { .. } => AgentRuntimeEvent::ToolResultsDropped {
        round: 1,
        dropped: 2,
    },
    AgentRuntimeEvent::TurnFinished { .. } => AgentRuntimeEvent::TurnFinished {
        success: true,
        completion: "final_text".to_string(),
        tool_errors: 0,
        policy_errors: 0,
    },
}

/// The wire name of one event, read back out of `serde` rather than typed.
///
/// The serialized tag is what the JSONL frontend publishes as the envelope's
/// `type`, so this is the name the contract must define.
fn wire_name(event: &AgentRuntimeEvent) -> String {
    assert_every_variant_is_declared(event);
    let value = serde_json::to_value(event).expect("runtime event serializes");
    value
        .get("event")
        .and_then(|tag| tag.as_str())
        .unwrap_or_else(|| panic!("runtime event carries no string `event` tag: {value}"))
        .to_string()
}

/// The def names reachable from the root `oneOf`.
///
/// Measured: the root has exactly `$schema`, `$id`, `title`, `description`,
/// `oneOf` and `$defs` — no `anyOf`, no bare `$ref`, nothing else that could
/// admit a line. The `oneOf` is therefore **closed**: a line whose `type` is
/// excluded from `forward_compatible_event` and has no branch of its own
/// matches no branch at all, so the published contract *rejects* an event the
/// tool really emits. That is worse than the omission it replaces, which is
/// why coverage is three-dimensional and not two.
fn one_of_branch_targets(schema: &serde_json::Value) -> Vec<String> {
    schema["oneOf"]
        .as_array()
        .expect("published schema root is a oneOf")
        .iter()
        .filter_map(|branch| branch["$ref"].as_str())
        .filter_map(|reference| reference.strip_prefix("#/$defs/"))
        .map(|name| name.to_string())
        .collect()
}

fn schema() -> serde_json::Value {
    serde_json::from_str(PUBLISHED_SCHEMA).expect("published schema parses")
}

/// A definition is event-shaped when it pins the envelope's `type` to a
/// constant. Payload definitions (`command_execution_result`, `envelope`,
/// `turn_identity`, ...) do not, which is how the two kinds are told apart.
fn event_type_const(definition: &serde_json::Value) -> Option<String> {
    let direct = definition
        .get("properties")
        .and_then(|props| props.get("type"))
        .and_then(|ty| ty.get("const"))
        .and_then(|c| c.as_str());
    if let Some(name) = direct {
        return Some(name.to_string());
    }
    definition
        .get("allOf")
        .and_then(|all| all.as_array())
        .into_iter()
        .flatten()
        .find_map(|member| {
            member
                .get("properties")
                .and_then(|props| props.get("type"))
                .and_then(|ty| ty.get("const"))
                .and_then(|c| c.as_str())
                .map(|name| name.to_string())
        })
}

#[test]
fn published_schema_defines_every_runtime_event_variant() {
    let schema = schema();
    let defs = schema["$defs"]
        .as_object()
        .expect("published schema exposes $defs");

    let branches = one_of_branch_targets(&schema);

    let mut missing = Vec::new();
    let mut mismatched = Vec::new();
    let mut unreachable = Vec::new();
    let mut covered = 0usize;

    for event in samples() {
        let name = wire_name(&event);
        match defs.get(&name) {
            None => missing.push(name.clone()),
            Some(definition) => match event_type_const(definition) {
                Some(ref declared) if declared == &name => covered += 1,
                other => mismatched.push(format!(
                    "$defs/{name} declares type {}",
                    other.map_or_else(|| "<none>".to_string(), |c| format!("{c:?}"))
                )),
            },
        }
        if !branches.contains(&name) {
            unreachable.push(name);
        }
    }

    assert!(
        missing.is_empty() && mismatched.is_empty() && unreachable.is_empty(),
        "the published JSONL contract does not cover every AgentRuntimeEvent variant.\n\
         defined and pinned {covered}/{total} variants.\n\
         missing $defs entry: {missing:?}\n\
         wire name not pinned by the entry: {mismatched:?}\n\
         no root `oneOf` branch, so the closed root REJECTS the event: {unreachable:?}\n\
         fix, in docs/schemas/kitsu-chat-v1.schema.json: define $defs/<wire name> \
         pinning `type` to the same string, add a root oneOf branch \
         $ref-ing it, and exclude the name from forward_compatible_event.",
        total = samples().len(),
    );
}

#[test]
fn forward_compatible_event_excludes_every_runtime_event_variant() {
    let schema = schema();
    let excluded: Vec<String> = schema["$defs"]["forward_compatible_event"]["allOf"]
        .as_array()
        .expect("forward_compatible_event is a composition")
        .iter()
        .find_map(|member| {
            member["properties"]["type"]["not"]["enum"]
                .as_array()
                .map(|names| {
                    names
                        .iter()
                        .filter_map(|name| name.as_str().map(str::to_string))
                        .collect()
                })
        })
        .expect("forward_compatible_event excludes the known event types");

    let missing: Vec<String> = samples()
        .iter()
        .map(wire_name)
        .filter(|name| !excluded.contains(name))
        .collect();

    assert!(
        missing.is_empty(),
        "forward_compatible_event would match known events, so an unknown-event \
         consumer silently accepts them as forward-compatible.\n\
         not excluded: {missing:?}\n\
         fix: add each name to \
         $defs/forward_compatible_event's `type.not.enum` in \
         docs/schemas/kitsu-chat-v1.schema.json.",
    );
}

/// The reverse direction is deliberately *not* an error.
///
/// Measured: the schema has 42 `$defs`, of which far more are event-shaped
/// than `AgentRuntimeEvent` has variants, because the JSONL frontend emits
/// run- and turn-level events (`run_started`, `final_summary`, `error`, ...)
/// that the orchestrator enum never produces, and because payload definitions
/// (`envelope`, `command_execution_result`, ...) are not events at all. An
/// event-shaped def with no matching variant is therefore legitimate, not a
/// stale entry.
///
/// The same measurement settles the root `oneOf`: 28 branches against nine
/// variants, and `run_started`, `assistant`, `error` and friends all have one,
/// so a branch with no variant behind it is legitimate too. Asserting the
/// reverse of either direction would fail on a correct contract.
///
/// What this test pins is only that the partition stays real: the enum's names
/// are a strict subset of both sets, there is at least one def of each kind so
/// `event_type_const` is still discriminating something, and the one branch
/// that is deliberately not event-shaped is the catch-all — if a second
/// appeared, the root would no longer be closed in the way
/// `one_of_branch_targets` documents.
#[test]
fn schema_event_definitions_are_a_superset_of_runtime_event_variants() {
    let schema = schema();
    let defs = schema["$defs"]
        .as_object()
        .expect("published schema exposes $defs");

    let event_shaped: Vec<&String> = defs
        .iter()
        .filter(|(_, definition)| event_type_const(definition).is_some())
        .map(|(key, _)| key)
        .collect();
    let payload_shaped = defs.len() - event_shaped.len();

    assert!(
        payload_shaped > 0,
        "no payload-shaped definitions remain; `type.const` no longer \
         discriminates events from payloads",
    );
    assert!(
        event_shaped.len() > samples().len(),
        "expected the contract to describe more event types ({}) than the \
         orchestrator enum has variants ({}); the frontend emits run- and \
         turn-level events the enum never produces",
        event_shaped.len(),
        samples().len(),
    );

    let branches = one_of_branch_targets(&schema);
    assert!(
        branches.len() > samples().len(),
        "expected the root oneOf to admit more event types ({}) than the \
         orchestrator enum has variants ({})",
        branches.len(),
        samples().len(),
    );
    let not_event_shaped: Vec<&String> = branches
        .iter()
        .filter(|name| {
            defs.get(name.as_str())
                .is_none_or(|definition| event_type_const(definition).is_none())
        })
        .collect();
    assert_eq!(
        not_event_shaped,
        vec!["forward_compatible_event"],
        "the root oneOf should hold exactly one branch that does not pin a \
         `type` constant, the forward-compatibility catch-all",
    );
}
