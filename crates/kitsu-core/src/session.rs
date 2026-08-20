//! Store-neutral canonical session identity, HEAD, and receipt values.
//!
//! This module owns no filesystem, database, configuration, credential,
//! permission, provider, tool-registry, or presentation types. Physical stores
//! validate and publish these values; they do not infer them from paths, tails,
//! rows, timestamps, or projections.

use std::collections::HashSet;
use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::{RunAssistantDisposition, RunId, RunOutcomeClass, TerminalOutcome};

pub const SESSION_CONTRACT_REVISION: u32 = 1;
const MAX_ID_BYTES: usize = 128;
pub const MAX_CANONICAL_OPERATIONS: usize = 16_384;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionIdentityKind {
    Store,
    Session,
    Revision,
    Turn,
}

impl SessionIdentityKind {
    const fn label(self) -> &'static str {
        match self {
            Self::Store => "store",
            Self::Session => "session",
            Self::Revision => "revision",
            Self::Turn => "turn",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionIdentityViolation {
    Empty,
    TooLong { actual: usize, max: usize },
    UnsafeByte { index: usize },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SessionIdentityError {
    pub kind: SessionIdentityKind,
    pub violation: SessionIdentityViolation,
}

impl fmt::Display for SessionIdentityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kind = self.kind.label();
        match self.violation {
            SessionIdentityViolation::Empty => write!(formatter, "{kind} identity cannot be empty"),
            SessionIdentityViolation::TooLong { actual, max } => write!(
                formatter,
                "{kind} identity is {actual} bytes; maximum is {max}"
            ),
            SessionIdentityViolation::UnsafeByte { index } => write!(
                formatter,
                "{kind} identity byte {index} is outside [A-Za-z0-9._:-]"
            ),
        }
    }
}

impl std::error::Error for SessionIdentityError {}

fn validate_identity(
    kind: SessionIdentityKind,
    value: String,
) -> Result<Box<str>, SessionIdentityError> {
    if value.is_empty() {
        return Err(SessionIdentityError {
            kind,
            violation: SessionIdentityViolation::Empty,
        });
    }
    if value.len() > MAX_ID_BYTES {
        return Err(SessionIdentityError {
            kind,
            violation: SessionIdentityViolation::TooLong {
                actual: value.len(),
                max: MAX_ID_BYTES,
            },
        });
    }
    if let Some(index) = value.bytes().position(|byte| {
        !(byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
    }) {
        return Err(SessionIdentityError {
            kind,
            violation: SessionIdentityViolation::UnsafeByte { index },
        });
    }
    Ok(value.into_boxed_str())
}

macro_rules! opaque_identity {
    ($name:ident, $kind:expr) => {
        #[derive(Clone, Debug, PartialEq, Eq, Hash)]
        pub struct $name(Box<str>);

        impl $name {
            pub const MAX_BYTES: usize = MAX_ID_BYTES;

            pub fn new(value: impl Into<String>) -> Result<Self, SessionIdentityError> {
                validate_identity($kind, value.into()).map(Self)
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(formatter)
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                self.as_str()
            }
        }

        impl Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                serializer.serialize_str(self.as_str())
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                Self::new(value).map_err(serde::de::Error::custom)
            }
        }
    };
}

opaque_identity!(StoreId, SessionIdentityKind::Store);
opaque_identity!(SessionId, SessionIdentityKind::Session);
opaque_identity!(RevisionId, SessionIdentityKind::Revision);
opaque_identity!(TurnId, SessionIdentityKind::Turn);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "revision",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum ExpectedRevision {
    Missing,
    Exact(RevisionId),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "turn",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum LogicalHead {
    Genesis,
    Turn(TurnId),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionOutcomeClass {
    Succeeded,
    PartiallySucceeded,
    UserFault,
    ProviderFault,
    ToolFault,
    Cancelled,
    Interrupted,
}

impl From<RunOutcomeClass> for SessionOutcomeClass {
    fn from(value: RunOutcomeClass) -> Self {
        match value {
            RunOutcomeClass::Succeeded => Self::Succeeded,
            RunOutcomeClass::PartiallySucceeded => Self::PartiallySucceeded,
            RunOutcomeClass::UserFault => Self::UserFault,
            RunOutcomeClass::ProviderFault => Self::ProviderFault,
            RunOutcomeClass::ToolFault => Self::ToolFault,
            RunOutcomeClass::Cancelled => Self::Cancelled,
            RunOutcomeClass::Interrupted => Self::Interrupted,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionAssistantDisposition {
    Committed,
    CommittedPartial,
    Discarded,
    Absent,
}

impl SessionAssistantDisposition {
    const fn advances_head(self) -> bool {
        matches!(self, Self::Committed | Self::CommittedPartial)
    }
}

impl From<RunAssistantDisposition> for SessionAssistantDisposition {
    fn from(value: RunAssistantDisposition) -> Self {
        match value {
            RunAssistantDisposition::Committed => Self::Committed,
            RunAssistantDisposition::CommittedPartial => Self::CommittedPartial,
            RunAssistantDisposition::Discarded => Self::Discarded,
            RunAssistantDisposition::Absent => Self::Absent,
        }
    }
}

mod run_id_serde {
    use serde::{Deserialize, Deserializer, Serializer};

    use crate::RunId;

    pub fn serialize<S>(value: &RunId, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(value.as_str())
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<RunId, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        RunId::new(value).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CanonicalTerminal {
    #[serde(with = "run_id_serde")]
    run_id: RunId,
    class: SessionOutcomeClass,
    assistant: SessionAssistantDisposition,
}

impl CanonicalTerminal {
    pub fn new(
        run_id: RunId,
        class: SessionOutcomeClass,
        assistant: SessionAssistantDisposition,
    ) -> Self {
        Self {
            run_id,
            class,
            assistant,
        }
    }

    pub fn from_terminal(value: &TerminalOutcome) -> Self {
        Self::new(
            value.run_id().clone(),
            value.class().into(),
            value.assistant().into(),
        )
    }

    pub fn run_id(&self) -> &RunId {
        &self.run_id
    }

    pub fn class(&self) -> SessionOutcomeClass {
        self.class
    }

    pub fn assistant(&self) -> SessionAssistantDisposition {
        self.assistant
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CanonicalOperationError {
    CommittedHeadMismatch {
        turn: TurnId,
        observed: LogicalHead,
    },
    UncommittedHeadChanged {
        before: LogicalHead,
        after: LogicalHead,
    },
}

impl fmt::Display for CanonicalOperationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CommittedHeadMismatch { turn, observed } => write!(
                formatter,
                "committed operation '{turn}' must select its own turn head, observed {observed:?}"
            ),
            Self::UncommittedHeadChanged { before, after } => write!(
                formatter,
                "discarded or absent assistant cannot change logical head from {before:?} to {after:?}"
            ),
        }
    }
}

impl std::error::Error for CanonicalOperationError {}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CanonicalOperation {
    turn_id: TurnId,
    terminal: CanonicalTerminal,
    before_head: LogicalHead,
    after_head: LogicalHead,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CanonicalOperationWire {
    turn_id: TurnId,
    terminal: CanonicalTerminal,
    before_head: LogicalHead,
    after_head: LogicalHead,
}

impl<'de> Deserialize<'de> for CanonicalOperation {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = CanonicalOperationWire::deserialize(deserializer)?;
        Self::new(
            wire.turn_id,
            wire.terminal,
            wire.before_head,
            wire.after_head,
        )
        .map_err(serde::de::Error::custom)
    }
}

impl CanonicalOperation {
    pub fn new(
        turn_id: TurnId,
        terminal: CanonicalTerminal,
        before_head: LogicalHead,
        after_head: LogicalHead,
    ) -> Result<Self, CanonicalOperationError> {
        if terminal.assistant.advances_head() {
            let expected = LogicalHead::Turn(turn_id.clone());
            if after_head != expected {
                return Err(CanonicalOperationError::CommittedHeadMismatch {
                    turn: turn_id,
                    observed: after_head,
                });
            }
        } else if after_head != before_head {
            return Err(CanonicalOperationError::UncommittedHeadChanged {
                before: before_head,
                after: after_head,
            });
        }
        Ok(Self {
            turn_id,
            terminal,
            before_head,
            after_head,
        })
    }

    pub fn turn_id(&self) -> &TurnId {
        &self.turn_id
    }

    pub fn terminal(&self) -> &CanonicalTerminal {
        &self.terminal
    }

    pub fn before_head(&self) -> &LogicalHead {
        &self.before_head
    }

    pub fn after_head(&self) -> &LogicalHead {
        &self.after_head
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionHistoryError {
    TooManyOperations { actual: usize, max: usize },
    UnknownParentHead { index: usize, observed: LogicalHead },
    DuplicateTurn { index: usize, turn: TurnId },
    DuplicateRun { index: usize, run: RunId },
    SelectedHeadUnknown { observed: LogicalHead },
}

impl fmt::Display for SessionHistoryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooManyOperations { actual, max } => {
                write!(
                    formatter,
                    "canonical history has {actual} operations; maximum is {max}"
                )
            }
            Self::UnknownParentHead { index, observed } => write!(
                formatter,
                "canonical operation {index} begins at unknown or uncommitted head {observed:?}"
            ),
            Self::DuplicateTurn { index, turn } => {
                write!(
                    formatter,
                    "canonical operation {index} repeats turn '{turn}'"
                )
            }
            Self::DuplicateRun { index, run } => {
                write!(formatter, "canonical operation {index} repeats run '{run}'")
            }
            Self::SelectedHeadUnknown { observed } => write!(
                formatter,
                "selected logical head {observed:?} is not a committed canonical operation"
            ),
        }
    }
}

impl std::error::Error for SessionHistoryError {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CanonicalHistory {
    operations: Vec<CanonicalOperation>,
    selected_head: LogicalHead,
}

impl CanonicalHistory {
    pub fn new(
        operations: Vec<CanonicalOperation>,
        selected_head: LogicalHead,
    ) -> Result<Self, SessionHistoryError> {
        if operations.len() > MAX_CANONICAL_OPERATIONS {
            return Err(SessionHistoryError::TooManyOperations {
                actual: operations.len(),
                max: MAX_CANONICAL_OPERATIONS,
            });
        }
        let mut turns = HashSet::with_capacity(operations.len());
        let mut runs = HashSet::with_capacity(operations.len());
        let mut committed_heads = HashSet::with_capacity(operations.len());
        for (index, operation) in operations.iter().enumerate() {
            if !turns.insert(operation.turn_id.clone()) {
                return Err(SessionHistoryError::DuplicateTurn {
                    index,
                    turn: operation.turn_id.clone(),
                });
            }
            if !runs.insert(operation.terminal.run_id.clone()) {
                return Err(SessionHistoryError::DuplicateRun {
                    index,
                    run: operation.terminal.run_id.clone(),
                });
            }
            if matches!(
                &operation.before_head,
                LogicalHead::Turn(turn) if !committed_heads.contains(turn)
            ) {
                return Err(SessionHistoryError::UnknownParentHead {
                    index,
                    observed: operation.before_head.clone(),
                });
            }
            if operation.terminal.assistant.advances_head() {
                committed_heads.insert(operation.turn_id.clone());
            }
        }
        if matches!(&selected_head, LogicalHead::Turn(turn) if !committed_heads.contains(turn)) {
            return Err(SessionHistoryError::SelectedHeadUnknown {
                observed: selected_head,
            });
        }
        Ok(Self {
            operations,
            selected_head,
        })
    }

    pub fn operations(&self) -> &[CanonicalOperation] {
        &self.operations
    }

    pub fn selected_head(&self) -> &LogicalHead {
        &self.selected_head
    }
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct CanonicalHistoryRef<'a> {
    contract_revision: u32,
    operations: &'a [CanonicalOperation],
    selected_head: &'a LogicalHead,
}

impl Serialize for CanonicalHistory {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        CanonicalHistoryRef {
            contract_revision: SESSION_CONTRACT_REVISION,
            operations: &self.operations,
            selected_head: &self.selected_head,
        }
        .serialize(serializer)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CanonicalHistoryWire {
    contract_revision: u32,
    operations: Vec<CanonicalOperation>,
    selected_head: LogicalHead,
}

impl<'de> Deserialize<'de> for CanonicalHistory {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = CanonicalHistoryWire::deserialize(deserializer)?;
        if wire.contract_revision != SESSION_CONTRACT_REVISION {
            return Err(serde::de::Error::custom(format!(
                "unsupported canonical session contract revision {}; expected {}",
                wire.contract_revision, SESSION_CONTRACT_REVISION
            )));
        }
        Self::new(wire.operations, wire.selected_head).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CorruptSessionReason {
    Malformed,
    IdentityMismatch,
    DigestMismatch,
    InvalidHead,
    BrokenCausality,
    MissingReference,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SchemaDimension {
    Logical,
    Physical,
    Artifact,
    Projection,
    Export,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionLookupStateError {
    UnsupportedContractRevision { observed: u32, supported: u32 },
    InvalidSchemaRevision,
    SupportedSchemaReportedAsUnsupported,
    MissingConflictObservation,
    MatchingConflictObservation,
}

impl fmt::Display for SessionLookupStateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedContractRevision {
                observed,
                supported,
            } => write!(
                formatter,
                "unsupported session state contract revision {observed}; expected {supported}"
            ),
            Self::InvalidSchemaRevision => formatter.write_str("schema revisions must be nonzero"),
            Self::SupportedSchemaReportedAsUnsupported => {
                formatter.write_str("observed and supported schema revisions are equal")
            }
            Self::MissingConflictObservation => {
                formatter.write_str("expected-missing conflict requires an observed revision")
            }
            Self::MatchingConflictObservation => {
                formatter.write_str("conflict observation cannot equal the expected revision")
            }
        }
    }
}

impl std::error::Error for SessionLookupStateError {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionLookupState {
    Missing {
        tombstone_revision: Option<RevisionId>,
    },
    Present {
        revision: RevisionId,
        head: LogicalHead,
    },
    Corrupt {
        session: SessionId,
        observed_revision: Option<RevisionId>,
        reason: CorruptSessionReason,
    },
    Unsupported {
        dimension: SchemaDimension,
        observed_revision: u32,
        supported_revision: u32,
    },
    Conflict {
        expected: ExpectedRevision,
        observed: Option<RevisionId>,
    },
}

#[derive(Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
enum SessionLookupStateRef<'a> {
    Missing {
        contract_revision: u32,
        tombstone_revision: &'a Option<RevisionId>,
    },
    Present {
        contract_revision: u32,
        revision: &'a RevisionId,
        head: &'a LogicalHead,
    },
    Corrupt {
        contract_revision: u32,
        session: &'a SessionId,
        observed_revision: &'a Option<RevisionId>,
        reason: CorruptSessionReason,
    },
    Unsupported {
        contract_revision: u32,
        dimension: SchemaDimension,
        observed_revision: u32,
        supported_revision: u32,
    },
    Conflict {
        contract_revision: u32,
        expected: &'a ExpectedRevision,
        observed: &'a Option<RevisionId>,
    },
}

impl Serialize for SessionLookupState {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let wire = match self {
            Self::Missing { tombstone_revision } => SessionLookupStateRef::Missing {
                contract_revision: SESSION_CONTRACT_REVISION,
                tombstone_revision,
            },
            Self::Present { revision, head } => SessionLookupStateRef::Present {
                contract_revision: SESSION_CONTRACT_REVISION,
                revision,
                head,
            },
            Self::Corrupt {
                session,
                observed_revision,
                reason,
            } => SessionLookupStateRef::Corrupt {
                contract_revision: SESSION_CONTRACT_REVISION,
                session,
                observed_revision,
                reason: *reason,
            },
            Self::Unsupported {
                dimension,
                observed_revision,
                supported_revision,
            } => SessionLookupStateRef::Unsupported {
                contract_revision: SESSION_CONTRACT_REVISION,
                dimension: *dimension,
                observed_revision: *observed_revision,
                supported_revision: *supported_revision,
            },
            Self::Conflict { expected, observed } => SessionLookupStateRef::Conflict {
                contract_revision: SESSION_CONTRACT_REVISION,
                expected,
                observed,
            },
        };
        wire.serialize(serializer)
    }
}

#[derive(Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
enum SessionLookupStateWire {
    Missing {
        contract_revision: u32,
        tombstone_revision: Option<RevisionId>,
    },
    Present {
        contract_revision: u32,
        revision: RevisionId,
        head: LogicalHead,
    },
    Corrupt {
        contract_revision: u32,
        session: SessionId,
        observed_revision: Option<RevisionId>,
        reason: CorruptSessionReason,
    },
    Unsupported {
        contract_revision: u32,
        dimension: SchemaDimension,
        observed_revision: u32,
        supported_revision: u32,
    },
    Conflict {
        contract_revision: u32,
        expected: ExpectedRevision,
        observed: Option<RevisionId>,
    },
}

impl SessionLookupState {
    pub fn unsupported(
        dimension: SchemaDimension,
        observed_revision: u32,
        supported_revision: u32,
    ) -> Result<Self, SessionLookupStateError> {
        if observed_revision == 0 || supported_revision == 0 {
            return Err(SessionLookupStateError::InvalidSchemaRevision);
        }
        if observed_revision == supported_revision {
            return Err(SessionLookupStateError::SupportedSchemaReportedAsUnsupported);
        }
        Ok(Self::Unsupported {
            dimension,
            observed_revision,
            supported_revision,
        })
    }

    pub fn conflict(
        expected: ExpectedRevision,
        observed: Option<RevisionId>,
    ) -> Result<Self, SessionLookupStateError> {
        match (&expected, &observed) {
            (ExpectedRevision::Missing, None) => {
                return Err(SessionLookupStateError::MissingConflictObservation)
            }
            (ExpectedRevision::Exact(expected), Some(observed)) if expected == observed => {
                return Err(SessionLookupStateError::MatchingConflictObservation)
            }
            _ => {}
        }
        Ok(Self::Conflict { expected, observed })
    }
}

impl<'de> Deserialize<'de> for SessionLookupState {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = SessionLookupStateWire::deserialize(deserializer)?;
        match wire {
            SessionLookupStateWire::Missing {
                contract_revision,
                tombstone_revision,
            } => {
                validate_state_contract_revision(contract_revision)
                    .map_err(serde::de::Error::custom)?;
                Ok(Self::Missing { tombstone_revision })
            }
            SessionLookupStateWire::Present {
                contract_revision,
                revision,
                head,
            } => {
                validate_state_contract_revision(contract_revision)
                    .map_err(serde::de::Error::custom)?;
                Ok(Self::Present { revision, head })
            }
            SessionLookupStateWire::Corrupt {
                contract_revision,
                session,
                observed_revision,
                reason,
            } => {
                validate_state_contract_revision(contract_revision)
                    .map_err(serde::de::Error::custom)?;
                Ok(Self::Corrupt {
                    session,
                    observed_revision,
                    reason,
                })
            }
            SessionLookupStateWire::Unsupported {
                contract_revision,
                dimension,
                observed_revision,
                supported_revision,
            } => {
                validate_state_contract_revision(contract_revision)
                    .map_err(serde::de::Error::custom)?;
                Self::unsupported(dimension, observed_revision, supported_revision)
                    .map_err(serde::de::Error::custom)
            }
            SessionLookupStateWire::Conflict {
                contract_revision,
                expected,
                observed,
            } => {
                validate_state_contract_revision(contract_revision)
                    .map_err(serde::de::Error::custom)?;
                Self::conflict(expected, observed).map_err(serde::de::Error::custom)
            }
        }
    }
}

fn validate_state_contract_revision(observed: u32) -> Result<(), SessionLookupStateError> {
    if observed == SESSION_CONTRACT_REVISION {
        Ok(())
    } else {
        Err(SessionLookupStateError::UnsupportedContractRevision {
            observed,
            supported: SESSION_CONTRACT_REVISION,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommitDurability {
    Confirmed,
    PublishedUnconfirmed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionCommitError {
    RevisionDidNotAdvance,
    UnsupportedContractRevision { observed: u32, supported: u32 },
}

impl fmt::Display for SessionCommitError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RevisionDidNotAdvance => {
                formatter.write_str("published revision must differ from its exact base")
            }
            Self::UnsupportedContractRevision {
                observed,
                supported,
            } => write!(
                formatter,
                "unsupported session receipt contract revision {observed}; expected {supported}"
            ),
        }
    }
}

impl std::error::Error for SessionCommitError {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionCommitReceipt {
    store: StoreId,
    session: SessionId,
    base_revision: ExpectedRevision,
    new_revision: RevisionId,
    head: LogicalHead,
    durability: CommitDurability,
}

impl SessionCommitReceipt {
    pub fn new(
        store: StoreId,
        session: SessionId,
        base_revision: ExpectedRevision,
        new_revision: RevisionId,
        head: LogicalHead,
        durability: CommitDurability,
    ) -> Result<Self, SessionCommitError> {
        if matches!(&base_revision, ExpectedRevision::Exact(base) if base == &new_revision) {
            return Err(SessionCommitError::RevisionDidNotAdvance);
        }
        Ok(Self {
            store,
            session,
            base_revision,
            new_revision,
            head,
            durability,
        })
    }

    pub fn store(&self) -> &StoreId {
        &self.store
    }

    pub fn session(&self) -> &SessionId {
        &self.session
    }

    pub fn base_revision(&self) -> &ExpectedRevision {
        &self.base_revision
    }

    pub fn new_revision(&self) -> &RevisionId {
        &self.new_revision
    }

    pub fn head(&self) -> &LogicalHead {
        &self.head
    }

    pub fn durability(&self) -> CommitDurability {
        self.durability
    }
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct SessionCommitReceiptRef<'a> {
    contract_revision: u32,
    store: &'a StoreId,
    session: &'a SessionId,
    base_revision: &'a ExpectedRevision,
    new_revision: &'a RevisionId,
    head: &'a LogicalHead,
    durability: CommitDurability,
}

impl Serialize for SessionCommitReceipt {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        SessionCommitReceiptRef {
            contract_revision: SESSION_CONTRACT_REVISION,
            store: &self.store,
            session: &self.session,
            base_revision: &self.base_revision,
            new_revision: &self.new_revision,
            head: &self.head,
            durability: self.durability,
        }
        .serialize(serializer)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionCommitReceiptWire {
    contract_revision: u32,
    store: StoreId,
    session: SessionId,
    base_revision: ExpectedRevision,
    new_revision: RevisionId,
    head: LogicalHead,
    durability: CommitDurability,
}

impl<'de> Deserialize<'de> for SessionCommitReceipt {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = SessionCommitReceiptWire::deserialize(deserializer)?;
        if wire.contract_revision != SESSION_CONTRACT_REVISION {
            return Err(serde::de::Error::custom(
                SessionCommitError::UnsupportedContractRevision {
                    observed: wire.contract_revision,
                    supported: SESSION_CONTRACT_REVISION,
                },
            ));
        }
        Self::new(
            wire.store,
            wire.session,
            wire.base_revision,
            wire.new_revision,
            wire.head,
            wire.durability,
        )
        .map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionCommitOutcome {
    NotPublished {
        store: StoreId,
        session: SessionId,
        base_revision: ExpectedRevision,
    },
    Published {
        receipt: SessionCommitReceipt,
    },
}

#[derive(Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
enum SessionCommitOutcomeRef<'a> {
    NotPublished {
        contract_revision: u32,
        store: &'a StoreId,
        session: &'a SessionId,
        base_revision: &'a ExpectedRevision,
    },
    Published {
        contract_revision: u32,
        receipt: &'a SessionCommitReceipt,
    },
}

impl Serialize for SessionCommitOutcome {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let wire = match self {
            Self::NotPublished {
                store,
                session,
                base_revision,
            } => SessionCommitOutcomeRef::NotPublished {
                contract_revision: SESSION_CONTRACT_REVISION,
                store,
                session,
                base_revision,
            },
            Self::Published { receipt } => SessionCommitOutcomeRef::Published {
                contract_revision: SESSION_CONTRACT_REVISION,
                receipt,
            },
        };
        wire.serialize(serializer)
    }
}

#[derive(Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case", deny_unknown_fields)]
enum SessionCommitOutcomeWire {
    NotPublished {
        contract_revision: u32,
        store: StoreId,
        session: SessionId,
        base_revision: ExpectedRevision,
    },
    Published {
        contract_revision: u32,
        receipt: SessionCommitReceipt,
    },
}

impl<'de> Deserialize<'de> for SessionCommitOutcome {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = SessionCommitOutcomeWire::deserialize(deserializer)?;
        match wire {
            SessionCommitOutcomeWire::NotPublished {
                contract_revision,
                store,
                session,
                base_revision,
            } => {
                validate_commit_contract_revision(contract_revision)
                    .map_err(serde::de::Error::custom)?;
                Ok(Self::NotPublished {
                    store,
                    session,
                    base_revision,
                })
            }
            SessionCommitOutcomeWire::Published {
                contract_revision,
                receipt,
            } => {
                validate_commit_contract_revision(contract_revision)
                    .map_err(serde::de::Error::custom)?;
                Ok(Self::Published { receipt })
            }
        }
    }
}

fn validate_commit_contract_revision(observed: u32) -> Result<(), SessionCommitError> {
    if observed == SESSION_CONTRACT_REVISION {
        Ok(())
    } else {
        Err(SessionCommitError::UnsupportedContractRevision {
            observed,
            supported: SESSION_CONTRACT_REVISION,
        })
    }
}
