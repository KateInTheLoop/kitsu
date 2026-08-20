//! Closed canonical logical payload and current-v1 migration input.

use std::collections::{HashMap, HashSet};

use chrono::DateTime;
use kitsu_core::session::{
    CanonicalHistory, CanonicalOperation, ExpectedRevision, LogicalHead, RevisionId,
    SessionAssistantDisposition, SessionId, SessionOutcomeClass, StoreId, TurnId,
};
use serde::{Deserialize, Serialize};

pub(super) const LOGICAL_SCHEMA_V1: u32 = 1;
const ARTIFACT_SCHEMA_V1: u32 = 1;
const PROJECTION_SCHEMA_V1: u32 = 1;
const MAX_PAYLOAD_BYTES: usize = 16 * 1024 * 1024;
const MAX_MESSAGES: usize = 65_536;
const MAX_MESSAGES_PER_OPERATION: usize = 4_096;
const MAX_TOOL_CALLS_PER_MESSAGE: usize = 64;
const MAX_CONTENT_BYTES: usize = 4 * 1024 * 1024;
const MAX_ARGUMENT_BYTES: usize = 1024 * 1024;
const MAX_METADATA_BYTES: usize = 4 * 1024 * 1024;
const MAX_ARTIFACTS: usize = 16_384;
const MAX_EFFECTS_PER_OPERATION: usize = 4_096;
const MAX_REFERENCES_PER_FACT: usize = 4_096;
const MAX_LOSS_ENTRIES: usize = 4_096;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CanonicalSessionRevisionV1 {
    logical_schema: u32,
    store_id: StoreId,
    session_id: SessionId,
    revision_id: RevisionId,
    parent_revision: ExpectedRevision,
    created_at: String,
    committed_at: String,
    metadata: CanonicalMetadataV1,
    lineage: CanonicalLineageV1,
    legacy_baseline: Option<LegacyBaselineV1>,
    operations: Vec<CanonicalOperationV1>,
    selected_head: LogicalHead,
    artifact_manifest: Vec<ImmutableArtifactRefV1>,
    compaction_binding: Option<ContextProjectionRefV1>,
}

impl CanonicalSessionRevisionV1 {
    pub(super) fn logical_schema(&self) -> u32 {
        self.logical_schema
    }

    pub(super) fn store_id(&self) -> &StoreId {
        &self.store_id
    }

    pub(super) fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    pub(super) fn revision_id(&self) -> &RevisionId {
        &self.revision_id
    }

    pub(super) fn parent_revision(&self) -> &ExpectedRevision {
        &self.parent_revision
    }

    pub(super) fn selected_head(&self) -> &LogicalHead {
        &self.selected_head
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CanonicalMetadataV1 {
    name: Option<String>,
    launch_directory: String,
    requested_gateway: String,
    requested_model: String,
    project_context_source: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum CanonicalLineageV1 {
    Root,
    Fork {
        source_store: StoreId,
        source_session: SessionId,
        source_revision: RevisionId,
        source_head: LogicalHead,
        branch_turn: LogicalHead,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyBaselineV1 {
    legacy_schema: u32,
    source_session_id: SessionId,
    source_snapshot_sha256: String,
    source_updated_at: String,
    messages: Vec<CanonicalMessageV1>,
    loss_manifest: Vec<LegacyBaselineLossV1>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum LegacyBaselineLossV1 {
    #[serde(rename = "terminal_outcomes_unavailable")]
    TerminalOutcomes,
    #[serde(rename = "logical_heads_unavailable")]
    LogicalHeads,
    #[serde(rename = "turn_lineage_unavailable")]
    TurnLineage,
}

const LEGACY_BASELINE_LOSSES: [LegacyBaselineLossV1; 3] = [
    LegacyBaselineLossV1::TerminalOutcomes,
    LegacyBaselineLossV1::LogicalHeads,
    LegacyBaselineLossV1::TurnLineage,
];

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CanonicalOperationV1 {
    operation: CanonicalOperation,
    messages: Vec<CanonicalMessageV1>,
    retained_effects: Vec<RetainedEffectRefV1>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "lowercase", deny_unknown_fields)]
enum CanonicalMessageV1 {
    System {
        content: String,
    },
    User {
        content: String,
    },
    Assistant {
        content: String,
        tool_calls: Vec<CanonicalToolCallV1>,
    },
    Tool {
        call_id: String,
        content: String,
        artifact_ids: Vec<String>,
    },
}

impl CanonicalMessageV1 {
    fn content(&self) -> &str {
        match self {
            Self::System { content }
            | Self::User { content }
            | Self::Assistant { content, .. }
            | Self::Tool { content, .. } => content,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CanonicalToolCallV1 {
    tool_name: String,
    args: String,
    call_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RetainedEffectRefV1 {
    effect_id: String,
    artifact_ids: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ImmutableArtifactRefV1 {
    artifact_schema: u32,
    artifact_id: String,
    sha256: String,
    bytes: u64,
    media_type: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ContextProjectionRefV1 {
    projection_schema: u32,
    projection_id: String,
    source_revision: RevisionId,
    source_head: LogicalHead,
    included_operations: Vec<TurnId>,
    route_generation: String,
    capability_generation: String,
    model_generation: String,
    estimator_generation: String,
    budget_tokens: u64,
    loss_manifest: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum CurrentV1MigrationBlocker {
    /// Current v1 never persisted semantic run terminals or assistant
    /// disposition, so a migrator must receive those facts from a truthful
    /// source rather than infer them from transcript tails or process exit.
    MissingTerminalOutcomes,
    /// Pointer-bearing transcript facts require a separately authenticated,
    /// immutable artifact closure before any canonical candidate can be built.
    ArtifactClosureRequired { message_indices: Vec<usize> },
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct CurrentV1MigrationInput {
    source_snapshot_sha256: String,
    id: String,
    created_at: String,
    updated_at: String,
    directory: String,
    gateway: String,
    model: String,
    name: Option<String>,
    project_context: Option<String>,
    messages: Vec<CanonicalMessageV1>,
    blockers: Vec<CurrentV1MigrationBlocker>,
}

impl CurrentV1MigrationInput {
    fn source_snapshot_sha256(&self) -> &str {
        &self.source_snapshot_sha256
    }

    fn messages(&self) -> &[CanonicalMessageV1] {
        &self.messages
    }

    fn blockers(&self) -> &[CurrentV1MigrationBlocker] {
        &self.blockers
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CurrentV1Wire {
    version: u32,
    id: String,
    created_at: String,
    updated_at: String,
    directory: String,
    gateway: String,
    model: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    project_context: Option<String>,
    messages: Vec<CurrentV1MessageWire>,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
enum CurrentV1RoleWire {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CurrentV1MessageWire {
    role: CurrentV1RoleWire,
    content: String,
    #[serde(default)]
    tool_call_id: Option<String>,
    #[serde(default)]
    tool_calls: Vec<CurrentV1ToolCallWire>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CurrentV1ToolCallWire {
    tool_name: String,
    args: String,
    call_id: String,
}

fn decode_current_v1_migration_input(
    bytes: &[u8],
    expected_session_id: &str,
) -> Result<CurrentV1MigrationInput, String> {
    check_payload_size(bytes, "current-v1 snapshot")?;
    if !super::valid_id(expected_session_id) {
        return Err("expected current-v1 session id is unsafe".to_string());
    }
    let wire: CurrentV1Wire = serde_json::from_slice(bytes)
        .map_err(|error| format!("cannot decode strict current-v1 snapshot: {error}"))?;
    if wire.version != super::SESSION_VERSION {
        return Err(format!(
            "unsupported current-v1 logical version {}; expected {}",
            wire.version,
            super::SESSION_VERSION
        ));
    }
    if wire.id != expected_session_id || !super::valid_id(&wire.id) {
        return Err("current-v1 session id does not match its selected identity".to_string());
    }
    validate_timestamp(&wire.created_at, "current-v1 created_at")?;
    validate_timestamp(&wire.updated_at, "current-v1 updated_at")?;
    validate_timestamp_order(&wire.created_at, &wire.updated_at, "current-v1")?;
    validate_resume_intent(
        &wire.directory,
        &wire.gateway,
        &wire.model,
        wire.name.as_deref(),
        wire.project_context.as_deref(),
    )?;

    if wire.messages.len() > MAX_MESSAGES {
        return Err(format!(
            "current-v1 transcript has {} messages; maximum is {MAX_MESSAGES}",
            wire.messages.len()
        ));
    }
    let messages = wire
        .messages
        .into_iter()
        .enumerate()
        .map(|(index, message)| convert_current_message(index, message))
        .collect::<Result<Vec<_>, _>>()?;
    validate_message_causality(&messages, true, true, false, "current-v1 transcript")?;

    let pointer_indices = messages
        .iter()
        .enumerate()
        .filter_map(|(index, message)| match message {
            CanonicalMessageV1::Tool { content, .. } if contains_offload_pointer(content) => {
                Some(index)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    let mut blockers = vec![CurrentV1MigrationBlocker::MissingTerminalOutcomes];
    if !pointer_indices.is_empty() {
        blockers.push(CurrentV1MigrationBlocker::ArtifactClosureRequired {
            message_indices: pointer_indices,
        });
    }

    Ok(CurrentV1MigrationInput {
        source_snapshot_sha256: crate::skills::content_sha256_hex(bytes),
        id: wire.id,
        created_at: wire.created_at,
        updated_at: wire.updated_at,
        directory: wire.directory,
        gateway: wire.gateway,
        model: wire.model,
        name: wire.name,
        project_context: wire.project_context,
        messages,
        blockers,
    })
}

fn build_current_v1_legacy_baseline(
    input: CurrentV1MigrationInput,
    store_id: StoreId,
    revision_id: RevisionId,
    committed_at: String,
) -> Result<CanonicalSessionRevisionV1, String> {
    if input.blockers.iter().any(|blocker| {
        matches!(
            blocker,
            CurrentV1MigrationBlocker::ArtifactClosureRequired { .. }
        )
    }) {
        return Err(
            "current-v1 legacy baseline requires explicit immutable artifact closure resolution"
                .to_string(),
        );
    }
    if input.blockers.as_slice() != [CurrentV1MigrationBlocker::MissingTerminalOutcomes] {
        return Err("current-v1 migration input has an unsupported blocker set".to_string());
    }
    validate_timestamp(&committed_at, "legacy migration committed_at")?;
    validate_timestamp_order(&input.updated_at, &committed_at, "legacy migration")?;
    let session_id = SessionId::new(input.id.clone())
        .map_err(|error| format!("current-v1 session identity is not canonical: {error}"))?;
    let payload = CanonicalSessionRevisionV1 {
        logical_schema: LOGICAL_SCHEMA_V1,
        store_id,
        session_id: session_id.clone(),
        revision_id,
        parent_revision: ExpectedRevision::Missing,
        created_at: input.created_at,
        committed_at,
        metadata: CanonicalMetadataV1 {
            name: input.name,
            launch_directory: input.directory,
            requested_gateway: input.gateway,
            requested_model: input.model,
            project_context_source: input.project_context,
        },
        lineage: CanonicalLineageV1::Root,
        legacy_baseline: Some(LegacyBaselineV1 {
            legacy_schema: super::SESSION_VERSION,
            source_session_id: session_id,
            source_snapshot_sha256: input.source_snapshot_sha256,
            source_updated_at: input.updated_at,
            messages: input.messages,
            loss_manifest: LEGACY_BASELINE_LOSSES.to_vec(),
        }),
        operations: Vec::new(),
        selected_head: LogicalHead::Genesis,
        artifact_manifest: Vec::new(),
        compaction_binding: None,
    };
    validate_canonical_payload(&payload)?;
    Ok(payload)
}

pub(super) fn prepare_current_v1_legacy_baseline(
    bytes: &[u8],
    expected_session_id: &str,
    store_id: StoreId,
    revision_id: RevisionId,
    committed_at: String,
) -> Result<CanonicalSessionRevisionV1, String> {
    let input = decode_current_v1_migration_input(bytes, expected_session_id)?;
    build_current_v1_legacy_baseline(input, store_id, revision_id, committed_at)
}

fn convert_current_message(
    index: usize,
    message: CurrentV1MessageWire,
) -> Result<CanonicalMessageV1, String> {
    bounded_content(&message.content, index)?;
    let calls = message
        .tool_calls
        .into_iter()
        .map(|call| CanonicalToolCallV1 {
            tool_name: call.tool_name,
            args: call.args,
            call_id: call.call_id,
        })
        .collect::<Vec<_>>();
    match message.role {
        CurrentV1RoleWire::System => {
            reject_nonassistant_tool_fields(index, &message.tool_call_id, &calls)?;
            Ok(CanonicalMessageV1::System {
                content: message.content,
            })
        }
        CurrentV1RoleWire::User => {
            reject_nonassistant_tool_fields(index, &message.tool_call_id, &calls)?;
            Ok(CanonicalMessageV1::User {
                content: message.content,
            })
        }
        CurrentV1RoleWire::Assistant => {
            if message.tool_call_id.is_some() {
                return Err(format!(
                    "current-v1 message {index} assistant cannot carry tool_call_id"
                ));
            }
            Ok(CanonicalMessageV1::Assistant {
                content: message.content,
                tool_calls: calls,
            })
        }
        CurrentV1RoleWire::Tool => {
            if !calls.is_empty() {
                return Err(format!(
                    "current-v1 message {index} tool result cannot introduce tool calls"
                ));
            }
            let call_id = message.tool_call_id.ok_or_else(|| {
                format!("current-v1 message {index} tool result is missing tool_call_id")
            })?;
            Ok(CanonicalMessageV1::Tool {
                call_id,
                content: message.content,
                artifact_ids: Vec::new(),
            })
        }
    }
}

fn reject_nonassistant_tool_fields(
    index: usize,
    tool_call_id: &Option<String>,
    calls: &[CanonicalToolCallV1],
) -> Result<(), String> {
    if tool_call_id.is_some() || !calls.is_empty() {
        Err(format!(
            "current-v1 message {index} role cannot carry tool correlation fields"
        ))
    } else {
        Ok(())
    }
}

fn contains_offload_pointer(content: &str) -> bool {
    content
        .lines()
        .any(|line| line.starts_with("[tool output ") && line.contains("full result saved to "))
}

pub(super) fn decode_canonical_payload(bytes: &[u8]) -> Result<CanonicalSessionRevisionV1, String> {
    check_payload_size(bytes, "canonical payload")?;
    let payload: CanonicalSessionRevisionV1 = serde_json::from_slice(bytes)
        .map_err(|error| format!("cannot decode closed canonical payload: {error}"))?;
    validate_canonical_payload(&payload)?;
    Ok(payload)
}

pub(super) fn encode_canonical_payload(
    payload: &CanonicalSessionRevisionV1,
) -> Result<Vec<u8>, String> {
    validate_canonical_payload(payload)?;
    let bytes = serde_json::to_vec(payload)
        .map_err(|error| format!("cannot encode canonical payload: {error}"))?;
    check_payload_size(&bytes, "canonical payload")?;
    Ok(bytes)
}

fn validate_canonical_payload(payload: &CanonicalSessionRevisionV1) -> Result<(), String> {
    if payload.logical_schema != LOGICAL_SCHEMA_V1 {
        return Err(format!(
            "unsupported canonical logical schema {}; expected {LOGICAL_SCHEMA_V1}",
            payload.logical_schema
        ));
    }
    validate_timestamp(&payload.created_at, "canonical created_at")?;
    validate_timestamp(&payload.committed_at, "canonical committed_at")?;
    validate_timestamp_order(
        &payload.created_at,
        &payload.committed_at,
        "canonical payload",
    )?;
    validate_resume_intent(
        &payload.metadata.launch_directory,
        &payload.metadata.requested_gateway,
        &payload.metadata.requested_model,
        payload.metadata.name.as_deref(),
        payload.metadata.project_context_source.as_deref(),
    )?;
    if let Some(baseline) = &payload.legacy_baseline {
        validate_legacy_baseline(payload, baseline)?;
    }
    if matches!(payload.lineage, CanonicalLineageV1::Fork { .. })
        && !matches!(payload.parent_revision, ExpectedRevision::Missing)
    {
        return Err("fork lineage must begin with a missing destination revision".to_string());
    }
    if payload.operations.len() > kitsu_core::session::MAX_CANONICAL_OPERATIONS {
        return Err(format!(
            "canonical payload has {} operations; maximum is {}",
            payload.operations.len(),
            kitsu_core::session::MAX_CANONICAL_OPERATIONS
        ));
    }
    if payload.artifact_manifest.len() > MAX_ARTIFACTS {
        return Err(format!(
            "canonical payload has {} artifacts; maximum is {MAX_ARTIFACTS}",
            payload.artifact_manifest.len()
        ));
    }

    let mut artifact_ids = HashSet::with_capacity(payload.artifact_manifest.len());
    for artifact in &payload.artifact_manifest {
        if artifact.artifact_schema != ARTIFACT_SCHEMA_V1 {
            return Err(format!(
                "unsupported artifact schema {}; expected {ARTIFACT_SCHEMA_V1}",
                artifact.artifact_schema
            ));
        }
        validate_opaque_id(&artifact.artifact_id, "artifact id")?;
        if !artifact_ids.insert(artifact.artifact_id.as_str()) {
            return Err(format!("duplicate artifact '{}'", artifact.artifact_id));
        }
        validate_sha256(&artifact.sha256, "artifact digest")?;
        if let Some(media_type) = &artifact.media_type {
            bounded_nonempty(media_type, 256, "artifact media type")?;
        }
    }

    let mut total_messages = payload
        .legacy_baseline
        .as_ref()
        .map_or(0, |baseline| baseline.messages.len());
    let mut effect_ids = HashSet::new();
    let mut referenced_artifacts = HashSet::new();
    let mut operation_envelopes = Vec::with_capacity(payload.operations.len());
    for (index, operation) in payload.operations.iter().enumerate() {
        total_messages = total_messages
            .checked_add(operation.messages.len())
            .ok_or_else(|| "canonical message count overflow".to_string())?;
        if total_messages > MAX_MESSAGES {
            return Err(format!(
                "canonical payload has more than {MAX_MESSAGES} messages"
            ));
        }
        validate_canonical_operation(
            index,
            operation,
            &artifact_ids,
            &mut referenced_artifacts,
            &mut effect_ids,
        )?;
        operation_envelopes.push(operation.operation.clone());
    }
    if let Some(unreferenced) = artifact_ids
        .iter()
        .find(|artifact| !referenced_artifacts.contains(**artifact))
    {
        return Err(format!(
            "canonical manifest contains unreferenced artifact '{unreferenced}'"
        ));
    }
    CanonicalHistory::new(operation_envelopes, payload.selected_head.clone())
        .map_err(|error| format!("invalid canonical history: {error}"))?;
    if let Some(binding) = &payload.compaction_binding {
        validate_projection_binding(payload, binding)?;
    }
    Ok(())
}

fn validate_legacy_baseline(
    payload: &CanonicalSessionRevisionV1,
    baseline: &LegacyBaselineV1,
) -> Result<(), String> {
    if baseline.legacy_schema != super::SESSION_VERSION {
        return Err(format!(
            "unsupported legacy baseline schema {}; expected {}",
            baseline.legacy_schema,
            super::SESSION_VERSION
        ));
    }
    if matches!(payload.lineage, CanonicalLineageV1::Root)
        && baseline.source_session_id != payload.session_id
    {
        return Err(
            "root legacy baseline source session must match canonical session identity".to_string(),
        );
    }
    validate_sha256(
        &baseline.source_snapshot_sha256,
        "legacy source snapshot digest",
    )?;
    validate_timestamp(&baseline.source_updated_at, "legacy source updated_at")?;
    validate_timestamp_order(
        &payload.created_at,
        &baseline.source_updated_at,
        "legacy source",
    )?;
    validate_timestamp_order(
        &baseline.source_updated_at,
        &payload.committed_at,
        "legacy migration",
    )?;
    if baseline.loss_manifest.as_slice() != LEGACY_BASELINE_LOSSES {
        return Err("legacy baseline must carry the exact fixed loss manifest".to_string());
    }
    if baseline.messages.len() > MAX_MESSAGES {
        return Err(format!(
            "legacy baseline has {} messages; maximum is {MAX_MESSAGES}",
            baseline.messages.len()
        ));
    }
    validate_message_causality(&baseline.messages, true, true, false, "legacy baseline")?;
    for message in &baseline.messages {
        if let CanonicalMessageV1::Tool {
            content,
            artifact_ids,
            ..
        } = message
        {
            if contains_offload_pointer(content) || !artifact_ids.is_empty() {
                return Err(
                    "legacy baseline has unresolved artifact closure; explicit resolution is required"
                        .to_string(),
                );
            }
        }
    }
    Ok(())
}

fn validate_canonical_operation<'a>(
    index: usize,
    operation: &'a CanonicalOperationV1,
    artifacts: &HashSet<&'a str>,
    referenced_artifacts: &mut HashSet<&'a str>,
    effect_ids: &mut HashSet<&'a str>,
) -> Result<(), String> {
    if operation.messages.is_empty() || operation.messages.len() > MAX_MESSAGES_PER_OPERATION {
        return Err(format!(
            "canonical operation {index} message count must be within 1..={MAX_MESSAGES_PER_OPERATION}"
        ));
    }
    let committed = matches!(
        operation.operation.terminal().assistant(),
        SessionAssistantDisposition::Committed | SessionAssistantDisposition::CommittedPartial
    );
    validate_terminal_agreement(index, &operation.operation)?;
    validate_message_causality(
        &operation.messages,
        committed,
        false,
        true,
        &format!("canonical operation {index}"),
    )?;
    if committed
        && !matches!(
            operation.messages.last(),
            Some(CanonicalMessageV1::Assistant { content, tool_calls })
                if !content.is_empty() && tool_calls.is_empty()
        )
    {
        return Err(format!(
            "canonical operation {index} committed assistant must end with non-empty final assistant output"
        ));
    }
    if operation.retained_effects.len() > MAX_EFFECTS_PER_OPERATION {
        return Err(format!(
            "canonical operation {index} has too many retained effects"
        ));
    }
    for effect in &operation.retained_effects {
        validate_opaque_id(&effect.effect_id, "effect id")?;
        if !effect_ids.insert(effect.effect_id.as_str()) {
            return Err(format!("duplicate effect '{}'", effect.effect_id));
        }
        validate_artifact_references(
            &effect.artifact_ids,
            artifacts,
            referenced_artifacts,
            "retained effect",
        )?;
    }
    for message in &operation.messages {
        if let CanonicalMessageV1::Tool {
            content,
            artifact_ids,
            ..
        } = message
        {
            if contains_offload_pointer(content) && artifact_ids.is_empty() {
                return Err(format!(
                    "canonical operation {index} has an artifact pointer without an immutable artifact reference"
                ));
            }
            validate_artifact_references(
                artifact_ids,
                artifacts,
                referenced_artifacts,
                "tool result",
            )?;
        }
    }
    Ok(())
}

fn validate_terminal_agreement(index: usize, operation: &CanonicalOperation) -> Result<(), String> {
    let terminal = operation.terminal();
    let agrees = matches!(
        (terminal.class(), terminal.assistant()),
        (
            SessionOutcomeClass::Succeeded,
            SessionAssistantDisposition::Committed
        ) | (
            SessionOutcomeClass::PartiallySucceeded,
            SessionAssistantDisposition::CommittedPartial
        ) | (
            SessionOutcomeClass::UserFault
                | SessionOutcomeClass::ProviderFault
                | SessionOutcomeClass::ToolFault
                | SessionOutcomeClass::Cancelled
                | SessionOutcomeClass::Interrupted,
            SessionAssistantDisposition::Discarded | SessionAssistantDisposition::Absent
        )
    );
    if agrees {
        Ok(())
    } else {
        Err(format!(
            "canonical operation {index} terminal class and assistant disposition disagree"
        ))
    }
}

fn validate_message_causality(
    messages: &[CanonicalMessageV1],
    require_closed: bool,
    allow_leading_system: bool,
    require_single_initial_user: bool,
    label: &str,
) -> Result<(), String> {
    let mut admitted = HashSet::new();
    let mut pending = HashSet::new();
    let mut saw_non_system = false;
    let mut awaiting_user = true;
    let mut user_count = 0usize;
    for (index, message) in messages.iter().enumerate() {
        bounded_content(message.content(), index)?;
        match message {
            CanonicalMessageV1::System { .. } => {
                if !allow_leading_system || saw_non_system {
                    return Err(format!("{label} message {index} has misplaced system role"));
                }
            }
            CanonicalMessageV1::User { .. } => {
                saw_non_system = true;
                user_count += 1;
                if !pending.is_empty() || !awaiting_user {
                    return Err(format!(
                        "{label} message {index} begins a user exchange before the prior exchange closes"
                    ));
                }
                awaiting_user = false;
                if require_single_initial_user && (index != 0 || user_count != 1) {
                    return Err(format!(
                        "{label} must contain exactly one initial user input"
                    ));
                }
            }
            CanonicalMessageV1::Assistant { tool_calls, .. } => {
                saw_non_system = true;
                if awaiting_user {
                    return Err(format!(
                        "{label} message {index} assistant fact has no active settled user exchange"
                    ));
                }
                if !pending.is_empty() {
                    return Err(format!(
                        "{label} message {index} assistant fact precedes unresolved tool call results"
                    ));
                }
                if tool_calls.len() > MAX_TOOL_CALLS_PER_MESSAGE {
                    return Err(format!("{label} message {index} has too many tool calls"));
                }
                for call in tool_calls {
                    validate_tool_call(call, label, index)?;
                    if !admitted.insert(call.call_id.as_str()) {
                        return Err(format!(
                            "{label} message {index} repeats tool call '{}'",
                            call.call_id
                        ));
                    }
                    pending.insert(call.call_id.as_str());
                }
                if tool_calls.is_empty() {
                    awaiting_user = true;
                }
            }
            CanonicalMessageV1::Tool { call_id, .. } => {
                saw_non_system = true;
                if awaiting_user {
                    return Err(format!(
                        "{label} message {index} tool result has no active user exchange"
                    ));
                }
                bounded_correlation_id(call_id, "tool result call id")?;
                if !pending.remove(call_id.as_str()) {
                    return Err(format!(
                        "{label} message {index} resolves unknown tool call '{call_id}'"
                    ));
                }
            }
        }
    }
    if require_single_initial_user && user_count != 1 {
        return Err(format!(
            "{label} must contain exactly one initial user input"
        ));
    }
    if require_closed && !pending.is_empty() {
        let mut unresolved = pending.into_iter().collect::<Vec<_>>();
        unresolved.sort_unstable();
        return Err(format!(
            "{label} has unresolved tool call(s): {}",
            unresolved.join(", ")
        ));
    }
    Ok(())
}

fn validate_tool_call(
    call: &CanonicalToolCallV1,
    label: &str,
    message_index: usize,
) -> Result<(), String> {
    validate_opaque_id(&call.tool_name, "tool name")?;
    bounded_correlation_id(&call.call_id, "tool call id")?;
    if call.args.len() > MAX_ARGUMENT_BYTES {
        return Err(format!(
            "{label} message {message_index} tool arguments exceed {MAX_ARGUMENT_BYTES} bytes"
        ));
    }
    let args: serde_json::Value = serde_json::from_str(&call.args).map_err(|error| {
        format!("{label} message {message_index} tool arguments are not JSON: {error}")
    })?;
    if !args.is_object() {
        return Err(format!(
            "{label} message {message_index} tool arguments must be a JSON object"
        ));
    }
    Ok(())
}

fn validate_artifact_references<'a>(
    references: &'a [String],
    artifacts: &HashSet<&'_ str>,
    referenced_artifacts: &mut HashSet<&'a str>,
    label: &str,
) -> Result<(), String> {
    if references.len() > MAX_REFERENCES_PER_FACT {
        return Err(format!("{label} has too many artifact references"));
    }
    let mut unique = HashSet::with_capacity(references.len());
    for reference in references {
        validate_opaque_id(reference, "artifact reference")?;
        if !unique.insert(reference.as_str()) {
            return Err(format!("{label} repeats artifact '{reference}'"));
        }
        if !artifacts.contains(reference.as_str()) {
            return Err(format!("{label} references missing artifact '{reference}'"));
        }
        referenced_artifacts.insert(reference.as_str());
    }
    Ok(())
}

fn validate_projection_binding(
    payload: &CanonicalSessionRevisionV1,
    binding: &ContextProjectionRefV1,
) -> Result<(), String> {
    if binding.projection_schema != PROJECTION_SCHEMA_V1 {
        return Err(format!(
            "unsupported projection schema {}; expected {PROJECTION_SCHEMA_V1}",
            binding.projection_schema
        ));
    }
    validate_opaque_id(&binding.projection_id, "projection id")?;
    for (value, label) in [
        (&binding.route_generation, "route generation"),
        (&binding.capability_generation, "capability generation"),
        (&binding.model_generation, "model generation"),
        (&binding.estimator_generation, "estimator generation"),
    ] {
        validate_opaque_id(value, label)?;
    }
    if binding.source_revision != payload.revision_id {
        return Err(format!(
            "compaction source revision '{}' does not match canonical revision '{}'",
            binding.source_revision, payload.revision_id
        ));
    }
    if binding.source_head != payload.selected_head {
        return Err("compaction source head does not match selected canonical head".to_string());
    }
    if binding.budget_tokens == 0 {
        return Err("compaction budget must be nonzero".to_string());
    }
    if binding.loss_manifest.len() > MAX_LOSS_ENTRIES {
        return Err("compaction loss manifest is too large".to_string());
    }
    for loss in &binding.loss_manifest {
        bounded_nonempty(loss, 4096, "compaction loss entry")?;
    }

    let by_turn = payload
        .operations
        .iter()
        .map(|operation| (operation.operation.turn_id(), &operation.operation))
        .collect::<HashMap<_, _>>();
    let mut ancestry = Vec::new();
    let mut cursor = payload.selected_head.clone();
    while let LogicalHead::Turn(turn) = cursor {
        let operation = by_turn
            .get(&turn)
            .ok_or_else(|| format!("compaction ancestry cannot resolve selected turn '{turn}'"))?;
        ancestry.push(turn);
        cursor = operation.before_head().clone();
    }
    ancestry.reverse();
    if binding.included_operations != ancestry {
        return Err(
            "compaction included operations do not equal selected-head ancestry".to_string(),
        );
    }
    Ok(())
}

fn check_payload_size(bytes: &[u8], label: &str) -> Result<(), String> {
    if bytes.is_empty() || bytes.len() > MAX_PAYLOAD_BYTES {
        return Err(format!(
            "{label} length must be within 1..={MAX_PAYLOAD_BYTES} bytes"
        ));
    }
    Ok(())
}

fn validate_resume_intent(
    directory: &str,
    gateway: &str,
    model: &str,
    name: Option<&str>,
    project_context: Option<&str>,
) -> Result<(), String> {
    bounded_nonempty(directory, 32 * 1024, "launch directory")?;
    bounded_nonempty(gateway, 4 * 1024, "requested gateway")?;
    bounded_nonempty(model, 4 * 1024, "requested model")?;
    if let Some(name) = name {
        super::valid_session_name(name)?;
    }
    if project_context.is_some_and(|context| context.len() > MAX_METADATA_BYTES) {
        return Err(format!(
            "project context exceeds {MAX_METADATA_BYTES} bytes"
        ));
    }
    Ok(())
}

fn validate_timestamp(value: &str, label: &str) -> Result<(), String> {
    if value.len() > 64 {
        return Err(format!("{label} is too long"));
    }
    DateTime::parse_from_rfc3339(value)
        .map(|_| ())
        .map_err(|error| format!("{label} is not RFC3339: {error}"))
}

fn validate_timestamp_order(created: &str, committed: &str, label: &str) -> Result<(), String> {
    let created = DateTime::parse_from_rfc3339(created)
        .map_err(|error| format!("{label} created timestamp is invalid: {error}"))?;
    let committed = DateTime::parse_from_rfc3339(committed)
        .map_err(|error| format!("{label} committed timestamp is invalid: {error}"))?;
    if committed < created {
        return Err(format!("{label} committed timestamp predates creation"));
    }
    Ok(())
}

fn bounded_content(content: &str, index: usize) -> Result<(), String> {
    if content.len() > MAX_CONTENT_BYTES {
        Err(format!(
            "message {index} content exceeds {MAX_CONTENT_BYTES} bytes"
        ))
    } else {
        Ok(())
    }
}

fn bounded_nonempty(value: &str, max: usize, label: &str) -> Result<(), String> {
    if value.is_empty() || value.len() > max {
        Err(format!("{label} length must be within 1..={max} bytes"))
    } else {
        Ok(())
    }
}

fn validate_opaque_id(value: &str, label: &str) -> Result<(), String> {
    bounded_nonempty(value, 128, label)?;
    if let Some(index) = value.bytes().position(|byte| {
        !(byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
    }) {
        return Err(format!("{label} byte {index} is unsafe"));
    }
    Ok(())
}

fn bounded_correlation_id(value: &str, label: &str) -> Result<(), String> {
    bounded_nonempty(value, 512, label)?;
    if let Some(index) = value.bytes().position(|byte| byte.is_ascii_control()) {
        return Err(format!("{label} byte {index} is a control character"));
    }
    Ok(())
}

fn validate_sha256(value: &str, label: &str) -> Result<(), String> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return Err(format!("{label} must be 64 lowercase hexadecimal bytes"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn current_v1(messages: serde_json::Value) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "version": 1,
            "id": "s-source",
            "created_at": "2026-08-20T10:00:00.000Z",
            "updated_at": "2026-08-20T10:01:00.000Z",
            "directory": "/work/tree",
            "gateway": "openai",
            "model": "gpt-test",
            "name": "kept_name",
            "project_context": "exact historical context",
            "messages": messages,
        }))
        .unwrap()
    }

    fn closed_messages() -> serde_json::Value {
        serde_json::json!([
            {"role":"system","content":"historical system"},
            {"role":"user","content":"inspect"},
            {"role":"assistant","content":"","tool_calls":[{
                "tool_name":"fs_read_file","args":"{\"path\":\"a.rs\"}","call_id":"call-1"
            }]},
            {"role":"tool","content":"contents","tool_call_id":"call-1"},
            {"role":"assistant","content":"done"}
        ])
    }

    #[test]
    fn strict_current_v1_is_preserved_as_non_publishable_migration_input() {
        let bytes = current_v1(closed_messages());
        let decoded = decode_current_v1_migration_input(&bytes, "s-source").unwrap();

        assert_eq!(
            decoded.source_snapshot_sha256(),
            crate::skills::content_sha256_hex(&bytes)
        );
        assert_eq!(decoded.messages().len(), 5);
        assert_eq!(decoded.messages()[0].content(), "historical system");
        assert_eq!(
            decoded.blockers(),
            &[CurrentV1MigrationBlocker::MissingTerminalOutcomes]
        );
    }

    #[test]
    fn current_saved_session_encoding_is_accepted_without_provider_wire_reuse() {
        let saved = super::super::SavedSession {
            version: super::super::SESSION_VERSION,
            id: "s-source".to_string(),
            created_at: "2026-08-20T10:00:00.000Z".to_string(),
            updated_at: "2026-08-20T10:01:00.000Z".to_string(),
            directory: "/work/tree".to_string(),
            gateway: "openai".to_string(),
            model: "gpt-test".to_string(),
            name: Some("kept_name".to_string()),
            project_context: None,
            messages: vec![
                kitsu_providers::ChatMessage::system("historical system"),
                kitsu_providers::ChatMessage::user("inspect"),
                kitsu_providers::ChatMessage::assistant("done", Vec::new()),
            ],
        };
        let bytes = serde_json::to_vec(&saved).unwrap();
        let decoded = decode_current_v1_migration_input(&bytes, "s-source").unwrap();

        assert_eq!(decoded.messages().len(), saved.messages.len());
        assert_eq!(decoded.messages()[2].content(), "done");
    }

    #[test]
    fn current_v1_rejects_unknown_fields_at_every_wire_level() {
        let mut root: serde_json::Value =
            serde_json::from_slice(&current_v1(closed_messages())).unwrap();
        root["ambient_authority"] = serde_json::json!(true);
        assert!(
            decode_current_v1_migration_input(&serde_json::to_vec(&root).unwrap(), "s-source")
                .unwrap_err()
                .contains("unknown field")
        );

        let mut message: serde_json::Value =
            serde_json::from_slice(&current_v1(closed_messages())).unwrap();
        message["messages"][1]["provider_cache"] = serde_json::json!("ambient");
        assert!(decode_current_v1_migration_input(
            &serde_json::to_vec(&message).unwrap(),
            "s-source"
        )
        .unwrap_err()
        .contains("unknown field"));

        let mut call: serde_json::Value =
            serde_json::from_slice(&current_v1(closed_messages())).unwrap();
        call["messages"][2]["tool_calls"][0]["approval"] = serde_json::json!("remember");
        assert!(
            decode_current_v1_migration_input(&serde_json::to_vec(&call).unwrap(), "s-source")
                .unwrap_err()
                .contains("unknown field")
        );
    }

    #[test]
    fn current_v1_rejects_broken_or_open_tool_causality() {
        let unknown_result = serde_json::json!([
            {"role":"user","content":"inspect"},
            {"role":"tool","content":"contents","tool_call_id":"not-admitted"}
        ]);
        assert!(
            decode_current_v1_migration_input(&current_v1(unknown_result), "s-source")
                .unwrap_err()
                .contains("unknown tool call")
        );

        let open_call = serde_json::json!([
            {"role":"user","content":"inspect"},
            {"role":"assistant","content":"","tool_calls":[{
                "tool_name":"fs_read_file","args":"{}","call_id":"call-1"
            }]}
        ]);
        assert!(
            decode_current_v1_migration_input(&current_v1(open_call), "s-source")
                .unwrap_err()
                .contains("unresolved tool call")
        );

        let assistant_first = serde_json::json!([
            {"role":"assistant","content":"invented"}
        ]);
        assert!(
            decode_current_v1_migration_input(&current_v1(assistant_first), "s-source")
                .unwrap_err()
                .contains("no active settled user exchange")
        );
    }

    #[test]
    fn current_v1_artifact_pointer_requires_explicit_closure_resolution() {
        let messages = serde_json::json!([
            {"role":"user","content":"inspect"},
            {"role":"assistant","content":"","tool_calls":[{
                "tool_name":"fs_read_file","args":"{}","call_id":"call-1"
            }]},
            {"role":"tool","tool_call_id":"call-1","content":
                "[tool output 9.0 KB — full result saved to /private/s-source.artifacts/call-1.txt. Re-read it with fs_read_file on that path.]"},
            {"role":"assistant","content":"done"}
        ]);
        let decoded = decode_current_v1_migration_input(&current_v1(messages), "s-source").unwrap();
        assert_eq!(
            decoded.blockers(),
            &[
                CurrentV1MigrationBlocker::MissingTerminalOutcomes,
                CurrentV1MigrationBlocker::ArtifactClosureRequired {
                    message_indices: vec![2]
                }
            ]
        );
    }

    #[test]
    fn explicit_legacy_import_preserves_transcript_and_declares_fixed_loss() {
        let source = current_v1(closed_messages());
        let payload = prepare_current_v1_legacy_baseline(
            &source,
            "s-source",
            StoreId::new("store-1").unwrap(),
            RevisionId::new("revision-1").unwrap(),
            "2026-08-20T11:00:00.000Z".to_string(),
        )
        .unwrap();

        assert_eq!(payload.selected_head, LogicalHead::Genesis);
        assert!(payload.operations.is_empty());
        let baseline = payload.legacy_baseline.as_ref().unwrap();
        assert_eq!(
            baseline.source_snapshot_sha256,
            crate::skills::content_sha256_hex(&source)
        );
        assert_eq!(baseline.messages.len(), 5);
        assert_eq!(
            baseline.loss_manifest,
            vec![
                LegacyBaselineLossV1::TerminalOutcomes,
                LegacyBaselineLossV1::LogicalHeads,
                LegacyBaselineLossV1::TurnLineage,
            ]
        );
        let round_trip =
            decode_canonical_payload(&encode_canonical_payload(&payload).unwrap()).unwrap();
        assert_eq!(round_trip, payload);

        let mut mismatch = serde_json::to_value(payload).unwrap();
        mismatch["legacy_baseline"]["source_session_id"] = serde_json::json!("other-session");
        assert!(
            decode_canonical_payload(&serde_json::to_vec(&mismatch).unwrap())
                .unwrap_err()
                .contains("source session")
        );
    }

    #[test]
    fn legacy_import_refuses_unresolved_artifact_pointer_without_writing_a_payload() {
        let messages = serde_json::json!([
            {"role":"user","content":"inspect"},
            {"role":"assistant","content":"","tool_calls":[{
                "tool_name":"fs_read_file","args":"{}","call_id":"call-1"
            }]},
            {"role":"tool","tool_call_id":"call-1","content":
                "[tool output 9.0 KB — full result saved to /private/s-source.artifacts/call-1.txt.]"},
            {"role":"assistant","content":"done"}
        ]);
        let error = prepare_current_v1_legacy_baseline(
            &current_v1(messages),
            "s-source",
            StoreId::new("store-1").unwrap(),
            RevisionId::new("revision-1").unwrap(),
            "2026-08-20T11:00:00.000Z".to_string(),
        )
        .unwrap_err();

        assert!(error.contains("artifact closure"));
    }

    #[test]
    fn canonical_legacy_baseline_rejects_missing_or_fabricated_loss_facts() {
        let input =
            decode_current_v1_migration_input(&current_v1(closed_messages()), "s-source").unwrap();
        let payload = build_current_v1_legacy_baseline(
            input,
            StoreId::new("store-1").unwrap(),
            RevisionId::new("revision-1").unwrap(),
            "2026-08-20T11:00:00.000Z".to_string(),
        )
        .unwrap();
        let mut value = serde_json::to_value(payload).unwrap();
        value["legacy_baseline"]["loss_manifest"] =
            serde_json::json!(["terminal_outcomes_unavailable"]);
        assert!(
            decode_canonical_payload(&serde_json::to_vec(&value).unwrap())
                .unwrap_err()
                .contains("exact fixed loss manifest")
        );

        value["legacy_baseline"]["loss_manifest"] = serde_json::json!([
            "terminal_outcomes_unavailable",
            "logical_heads_unavailable",
            "turn_lineage_unavailable"
        ]);
        value["legacy_baseline"]["messages"][4] =
            serde_json::json!({"role":"system","content":"done"});
        assert!(
            decode_canonical_payload(&serde_json::to_vec(&value).unwrap())
                .unwrap_err()
                .contains("misplaced system role")
        );
    }

    #[test]
    fn complete_payload_round_trips_whole_terminal_operation() {
        let bytes = complete_payload_fixture();
        let decoded = decode_canonical_payload(&bytes).unwrap();
        let encoded = encode_canonical_payload(&decoded).unwrap();
        let reparsed = decode_canonical_payload(&encoded).unwrap();

        assert_eq!(decoded, reparsed);
        assert_eq!(decoded.operations.len(), 1);
        assert_eq!(decoded.operations[0].messages.len(), 4);
        assert_eq!(
            decoded.selected_head,
            kitsu_core::session::LogicalHead::Turn(
                kitsu_core::session::TurnId::new("turn-1").unwrap()
            )
        );
        assert!(!String::from_utf8(encoded)
            .unwrap()
            .contains("canonical_digest"));
    }

    #[test]
    fn committed_operation_requires_closed_causality_and_manifest_closure() {
        let mut open: serde_json::Value =
            serde_json::from_slice(&complete_payload_fixture()).unwrap();
        open["operations"][0]["messages"]
            .as_array_mut()
            .unwrap()
            .remove(2);
        assert!(
            decode_canonical_payload(&serde_json::to_vec(&open).unwrap())
                .unwrap_err()
                .contains("unresolved tool call")
        );

        let mut missing: serde_json::Value =
            serde_json::from_slice(&complete_payload_fixture()).unwrap();
        missing["artifact_manifest"] = serde_json::json!([]);
        assert!(
            decode_canonical_payload(&serde_json::to_vec(&missing).unwrap())
                .unwrap_err()
                .contains("missing artifact")
        );

        let mut unreferenced: serde_json::Value =
            serde_json::from_slice(&complete_payload_fixture()).unwrap();
        unreferenced["artifact_manifest"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({
                "artifact_schema": 1,
                "artifact_id": "artifact-extra",
                "sha256": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                "bytes": 1,
                "media_type": null
            }));
        assert!(
            decode_canonical_payload(&serde_json::to_vec(&unreferenced).unwrap())
                .unwrap_err()
                .contains("unreferenced artifact")
        );

        let mut disagreement: serde_json::Value =
            serde_json::from_slice(&complete_payload_fixture()).unwrap();
        disagreement["operations"][0]["operation"]["terminal"]["class"] =
            serde_json::json!("provider_fault");
        assert!(
            decode_canonical_payload(&serde_json::to_vec(&disagreement).unwrap())
                .unwrap_err()
                .contains("terminal class and assistant disposition disagree")
        );
    }

    #[test]
    fn canonical_payload_rejects_unknown_fields_and_stale_projection_binding() {
        let mut unknown: serde_json::Value =
            serde_json::from_slice(&complete_payload_fixture()).unwrap();
        unknown["permission_grants"] = serde_json::json!(["all"]);
        assert!(
            decode_canonical_payload(&serde_json::to_vec(&unknown).unwrap())
                .unwrap_err()
                .contains("unknown field")
        );

        let mut stale: serde_json::Value =
            serde_json::from_slice(&complete_payload_fixture()).unwrap();
        stale["compaction_binding"]["source_revision"] = serde_json::json!("rev-other");
        assert!(
            decode_canonical_payload(&serde_json::to_vec(&stale).unwrap())
                .unwrap_err()
                .contains("source revision")
        );
    }

    fn complete_payload_fixture() -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "logical_schema": 1,
            "store_id": "store-1",
            "session_id": "session-1",
            "revision_id": "rev-1",
            "parent_revision": {"kind":"missing"},
            "created_at": "2026-08-20T10:00:00.000Z",
            "committed_at": "2026-08-20T10:01:00.000Z",
            "metadata": {
                "name": "review",
                "launch_directory": "/work/tree",
                "requested_gateway": "openai",
                "requested_model": "gpt-test",
                "project_context_source": "AGENTS.md"
            },
            "lineage": {"kind":"root"},
            "operations": [{
                "operation": {
                    "turn_id": "turn-1",
                    "terminal": {
                        "run_id": "run-1",
                        "class": "succeeded",
                        "assistant": "committed"
                    },
                    "before_head": {"kind":"genesis"},
                    "after_head": {"kind":"turn","turn":"turn-1"}
                },
                "messages": [
                    {"role":"user","content":"inspect"},
                    {"role":"assistant","content":"","tool_calls":[{
                        "tool_name":"fs_read_file","args":"{\"path\":\"a.rs\"}","call_id":"call-1"
                    }]},
                    {"role":"tool","call_id":"call-1","content":"contents","artifact_ids":["artifact-1"]},
                    {"role":"assistant","content":"done","tool_calls":[]}
                ],
                "retained_effects":[{
                    "effect_id":"effect-1","artifact_ids":["artifact-1"]
                }]
            }],
            "selected_head": {"kind":"turn","turn":"turn-1"},
            "artifact_manifest": [{
                "artifact_schema": 1,
                "artifact_id": "artifact-1",
                "sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "bytes": 8,
                "media_type": "text/plain"
            }],
            "compaction_binding": {
                "projection_schema": 1,
                "projection_id": "projection-1",
                "source_revision": "rev-1",
                "source_head": {"kind":"turn","turn":"turn-1"},
                "included_operations": ["turn-1"],
                "route_generation": "route-1",
                "capability_generation": "caps-1",
                "model_generation": "model-1",
                "estimator_generation": "estimator-1",
                "budget_tokens": 4096,
                "loss_manifest": ["tool output elided"]
            }
        }))
        .unwrap()
    }
}
