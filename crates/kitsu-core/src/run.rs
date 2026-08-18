//! Carrier-neutral causal run identity and semantic outcome types.
//!
//! This module deliberately owns no presentation, operating-system input,
//! persistence, provider-wire, or tool types. Callers allocate an identity and
//! construct a [`RunSpec`] before any accepted work. Later lifecycle cards add
//! ordered settlement/cleanup/persistence/delivery phases around this narrow
//! identity-bearing contract.

use std::fmt;

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
