//! Carrier-neutral causal run identity and semantic outcome types.
//!
//! This module deliberately owns no presentation, operating-system input,
//! persistence, provider-wire, or tool types. Callers allocate an identity and
//! construct a [`RunSpec`] before any accepted work. Later lifecycle cards add
//! ordered settlement/cleanup/persistence/delivery phases around this narrow
//! identity-bearing contract.

use std::fmt;
use std::num::NonZeroU32;

/// Opaque, bounded identity shared by every event and terminal for one run.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RunId(Box<str>);

impl RunId {
    /// Enough room for UUID/ULID-style IDs plus a bounded host prefix while
    /// keeping every event field cheap to validate and safe to frame.
    pub const MAX_BYTES: usize = 128;

    pub fn new(value: impl Into<String>) -> Result<Self, RunIdError> {
        let value = value.into();
        if value.is_empty() {
            return Err(RunIdError::Empty);
        }
        if value.len() > Self::MAX_BYTES {
            return Err(RunIdError::TooLong {
                actual: value.len(),
                max: Self::MAX_BYTES,
            });
        }
        if let Some(index) = value.bytes().position(|byte| {
            !(byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
        }) {
            return Err(RunIdError::UnsafeByte { index });
        }
        Ok(Self(value.into_boxed_str()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RunId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl AsRef<str> for RunId {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunIdError {
    Empty,
    TooLong { actual: usize, max: usize },
    UnsafeByte { index: usize },
}

impl fmt::Display for RunIdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("run identity cannot be empty"),
            Self::TooLong { actual, max } => {
                write!(f, "run identity is {actual} bytes; maximum is {max}")
            }
            Self::UnsafeByte { index } => {
                write!(f, "run identity byte {index} is outside [A-Za-z0-9._:-]")
            }
        }
    }
}

impl std::error::Error for RunIdError {}

/// Identity assigned by a host before that host accepts run work.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunSpec {
    run_id: RunId,
}

impl RunSpec {
    pub fn new(run_id: RunId) -> Self {
        Self { run_id }
    }

    pub fn run_id(&self) -> &RunId {
        &self.run_id
    }

    /// Construct an event that cannot lose or replace this spec's identity.
    pub fn event(&self, kind: RunEventKind) -> RunEvent {
        RunEvent {
            run_id: self.run_id.clone(),
            kind,
        }
    }

    /// Construct semantic settlement independently from later cleanup,
    /// persistence, delivery, and process-exit phases.
    pub fn terminal(
        &self,
        class: RunOutcomeClass,
        assistant: RunAssistantDisposition,
    ) -> TerminalOutcome {
        TerminalOutcome {
            run_id: self.run_id.clone(),
            class,
            assistant,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunEventKind {
    Accepted,
    Interrupted(RunInterruption),
}

/// An interruption discovered below presentation and before any carrier can
/// reinterpret it as denial, EOF completion, or process success.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunInterruption {
    AuthorityInputClosed,
}

impl fmt::Display for RunInterruption {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AuthorityInputClosed => f.write_str("authority input closed"),
        }
    }
}

impl std::error::Error for RunInterruption {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunEvent {
    run_id: RunId,
    kind: RunEventKind,
}

impl RunEvent {
    pub fn run_id(&self) -> &RunId {
        &self.run_id
    }

    pub fn kind(&self) -> RunEventKind {
        self.kind
    }
}

/// Semantic classification only. Cleanup, persistence, delivery, and process
/// exit remain distinct later phases and must never rewrite this field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunOutcomeClass {
    Succeeded,
    PartiallySucceeded,
    UserFault,
    ProviderFault,
    ToolFault,
    Cancelled,
    Interrupted,
}

impl RunOutcomeClass {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::PartiallySucceeded => "partially_succeeded",
            Self::UserFault => "user_fault",
            Self::ProviderFault => "provider_fault",
            Self::ToolFault => "tool_fault",
            Self::Cancelled => "cancelled",
            Self::Interrupted => "interrupted",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunAssistantDisposition {
    Committed,
    CommittedPartial,
    Discarded,
    Absent,
}

impl RunAssistantDisposition {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Committed => "committed",
            Self::CommittedPartial => "committed_partial",
            Self::Discarded => "discarded",
            Self::Absent => "absent",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalOutcome {
    run_id: RunId,
    class: RunOutcomeClass,
    assistant: RunAssistantDisposition,
}

impl TerminalOutcome {
    pub fn run_id(&self) -> &RunId {
        &self.run_id
    }

    pub fn class(&self) -> RunOutcomeClass {
        self.class
    }

    pub fn assistant(&self) -> RunAssistantDisposition {
        self.assistant
    }
}

/// Monotonic terminal lifecycle. The semantic outcome settles first and later
/// operational phases can report failure without rewriting it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunPhase {
    Accepted,
    Settled,
    Joined,
    Persisted,
    Delivered,
    Published,
}

impl RunPhase {
    const fn next(self) -> Option<Self> {
        match self {
            Self::Accepted => Some(Self::Settled),
            Self::Settled => Some(Self::Joined),
            Self::Joined => Some(Self::Persisted),
            Self::Persisted => Some(Self::Delivered),
            Self::Delivered => Some(Self::Published),
            Self::Published => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunCleanupState {
    Joined,
    Residue { unjoined: NonZeroU32 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunPersistenceState {
    Committed,
    Preserved,
    Failed,
    NotRequested,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunDeliveryState {
    Delivered,
    Failed,
    NotAttempted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunPhaseError {
    Duplicate(RunPhase),
    OutOfOrder {
        current: RunPhase,
        expected: RunPhase,
        attempted: RunPhase,
    },
    AfterTerminal {
        attempted: RunPhase,
    },
    WorkAfterTerminal,
}

impl fmt::Display for RunPhaseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Duplicate(phase) => write!(f, "duplicate run phase: {phase:?}"),
            Self::OutOfOrder {
                current,
                expected,
                attempted,
            } => write!(
                f,
                "out-of-order run phase: current {current:?}, expected {expected:?}, attempted {attempted:?}"
            ),
            Self::AfterTerminal { attempted } => {
                write!(f, "run is terminal; rejected phase {attempted:?}")
            }
            Self::WorkAfterTerminal => f.write_str("run is terminal; rejected later work"),
        }
    }
}

impl std::error::Error for RunPhaseError {}

/// Provider-, carrier-, and store-free owner of one run's terminal phase
/// progression. Every mutation validates before publication, so a rejected
/// transition leaves the previous state unchanged.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunLifecycle {
    spec: RunSpec,
    phase: RunPhase,
    terminal: Option<TerminalOutcome>,
    cleanup: Option<RunCleanupState>,
    persistence: Option<RunPersistenceState>,
    delivery: Option<RunDeliveryState>,
}

impl RunLifecycle {
    pub fn accept(spec: &RunSpec) -> Self {
        Self {
            spec: spec.clone(),
            phase: RunPhase::Accepted,
            terminal: None,
            cleanup: None,
            persistence: None,
            delivery: None,
        }
    }

    pub fn phase(&self) -> RunPhase {
        self.phase
    }

    pub fn terminal(&self) -> Option<&TerminalOutcome> {
        self.terminal.as_ref()
    }

    pub fn ensure_work_allowed(&self) -> Result<(), RunPhaseError> {
        if self.phase == RunPhase::Published {
            Err(RunPhaseError::WorkAfterTerminal)
        } else {
            Ok(())
        }
    }

    pub fn settle(
        &mut self,
        class: RunOutcomeClass,
        assistant: RunAssistantDisposition,
    ) -> Result<(), RunPhaseError> {
        self.validate_transition(RunPhase::Settled)?;
        self.terminal = Some(self.spec.terminal(class, assistant));
        self.phase = RunPhase::Settled;
        Ok(())
    }

    pub fn join(&mut self, state: RunCleanupState) -> Result<(), RunPhaseError> {
        self.validate_transition(RunPhase::Joined)?;
        self.cleanup = Some(state);
        self.phase = RunPhase::Joined;
        Ok(())
    }

    pub fn persist(&mut self, state: RunPersistenceState) -> Result<(), RunPhaseError> {
        self.validate_transition(RunPhase::Persisted)?;
        self.persistence = Some(state);
        self.phase = RunPhase::Persisted;
        Ok(())
    }

    pub fn deliver(&mut self, state: RunDeliveryState) -> Result<(), RunPhaseError> {
        self.validate_transition(RunPhase::Delivered)?;
        self.delivery = Some(state);
        self.phase = RunPhase::Delivered;
        Ok(())
    }

    pub fn publish(&mut self) -> Result<PublishedRun, RunPhaseError> {
        self.validate_transition(RunPhase::Published)?;
        let published = PublishedRun {
            terminal: self
                .terminal
                .clone()
                .expect("validated lifecycle has a settled outcome"),
            cleanup: self.cleanup.expect("validated lifecycle has a join result"),
            persistence: self
                .persistence
                .expect("validated lifecycle has a persistence result"),
            delivery: self
                .delivery
                .expect("validated lifecycle has a delivery result"),
        };
        self.phase = RunPhase::Published;
        Ok(published)
    }

    fn validate_transition(&self, attempted: RunPhase) -> Result<(), RunPhaseError> {
        if self.phase == RunPhase::Published {
            return Err(RunPhaseError::AfterTerminal { attempted });
        }
        if attempted == self.phase {
            return Err(RunPhaseError::Duplicate(attempted));
        }
        let expected = self
            .phase
            .next()
            .expect("non-terminal run phase must have a successor");
        if attempted != expected {
            return Err(RunPhaseError::OutOfOrder {
                current: self.phase,
                expected,
                attempted,
            });
        }
        Ok(())
    }
}

/// Immutable terminal publication containing semantic and later-phase truth.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublishedRun {
    terminal: TerminalOutcome,
    cleanup: RunCleanupState,
    persistence: RunPersistenceState,
    delivery: RunDeliveryState,
}

impl PublishedRun {
    pub fn terminal(&self) -> &TerminalOutcome {
        &self.terminal
    }

    pub fn cleanup(&self) -> RunCleanupState {
        self.cleanup
    }

    pub fn persistence(&self) -> RunPersistenceState {
        self.persistence
    }

    pub fn delivery(&self) -> RunDeliveryState {
        self.delivery
    }

    pub fn is_clean_success(&self) -> bool {
        self.terminal.class() == RunOutcomeClass::Succeeded
            && self.cleanup == RunCleanupState::Joined
            && self.persistence != RunPersistenceState::Failed
            && self.delivery != RunDeliveryState::Failed
    }
}
