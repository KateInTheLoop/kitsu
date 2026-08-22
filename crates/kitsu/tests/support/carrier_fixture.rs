//! Provider-free oracle for the E02 carrier-terminal fixture.
//!
//! These are deliberately integration-test types, not a proposed production
//! `RunSpec`/`TerminalOutcome` API. Later cards map real carrier observations
//! into this oracle before extracting any runtime contract.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EntryPoint {
    Ask,
    PipedChat,
    ScriptChat,
    InteractiveChat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Surface {
    Human,
    Jsonl,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Carrier {
    pub entry: EntryPoint,
    pub surface: Surface,
}

impl Carrier {
    pub const fn label(self) -> &'static str {
        match (self.entry, self.surface) {
            (EntryPoint::Ask, Surface::Human) => "ask-human",
            (EntryPoint::Ask, Surface::Jsonl) => "ask-jsonl",
            (EntryPoint::PipedChat, Surface::Human) => "piped-human",
            (EntryPoint::PipedChat, Surface::Jsonl) => "piped-jsonl",
            (EntryPoint::ScriptChat, Surface::Human) => "script-human",
            (EntryPoint::ScriptChat, Surface::Jsonl) => "script-jsonl",
            (EntryPoint::InteractiveChat, Surface::Human) => "interactive-human",
            (EntryPoint::InteractiveChat, Surface::Jsonl) => "interactive-jsonl",
        }
    }
}

pub const CARRIERS: [Carrier; 7] = [
    Carrier {
        entry: EntryPoint::Ask,
        surface: Surface::Human,
    },
    Carrier {
        entry: EntryPoint::Ask,
        surface: Surface::Jsonl,
    },
    Carrier {
        entry: EntryPoint::PipedChat,
        surface: Surface::Human,
    },
    Carrier {
        entry: EntryPoint::PipedChat,
        surface: Surface::Jsonl,
    },
    Carrier {
        entry: EntryPoint::ScriptChat,
        surface: Surface::Human,
    },
    Carrier {
        entry: EntryPoint::ScriptChat,
        surface: Surface::Jsonl,
    },
    Carrier {
        entry: EntryPoint::InteractiveChat,
        surface: Surface::Human,
    },
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Scenario {
    Success,
    SequentialSameSession,
    PreflightFault,
    ProviderFault,
    ToolFault,
    ConfirmationEof,
    PreTerminalEof,
    CancelBeforeActive,
    CancelActive,
    CancelQueued,
    BlockedSink,
    EpipeSink,
    CleanupResidue,
    DuplicateTerminal,
    CrossRunTerminal,
    IdentityStripped,
    VersionMismatch,
}

impl Scenario {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::SequentialSameSession => "sequential-same-session",
            Self::PreflightFault => "preflight-fault",
            Self::ProviderFault => "provider-fault",
            Self::ToolFault => "tool-fault",
            Self::ConfirmationEof => "confirmation-eof",
            Self::PreTerminalEof => "pre-terminal-eof",
            Self::CancelBeforeActive => "cancel-before-active",
            Self::CancelActive => "cancel-active",
            Self::CancelQueued => "cancel-queued",
            Self::BlockedSink => "blocked-sink",
            Self::EpipeSink => "epipe-sink",
            Self::CleanupResidue => "cleanup-residue",
            Self::DuplicateTerminal => "duplicate-terminal",
            Self::CrossRunTerminal => "cross-run-terminal",
            Self::IdentityStripped => "identity-stripped",
            Self::VersionMismatch => "version-mismatch",
        }
    }
}

pub const SCENARIOS: [Scenario; 17] = [
    Scenario::Success,
    Scenario::SequentialSameSession,
    Scenario::PreflightFault,
    Scenario::ProviderFault,
    Scenario::ToolFault,
    Scenario::ConfirmationEof,
    Scenario::PreTerminalEof,
    Scenario::CancelBeforeActive,
    Scenario::CancelActive,
    Scenario::CancelQueued,
    Scenario::BlockedSink,
    Scenario::EpipeSink,
    Scenario::CleanupResidue,
    Scenario::DuplicateTerminal,
    Scenario::CrossRunTerminal,
    Scenario::IdentityStripped,
    Scenario::VersionMismatch,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Applicability {
    Required,
    MustFailClosedWithoutPrompt,
    NotApplicable,
}

pub fn applicability(carrier: Carrier, scenario: Scenario) -> Applicability {
    match scenario {
        Scenario::ConfirmationEof if carrier.entry != EntryPoint::InteractiveChat => {
            Applicability::MustFailClosedWithoutPrompt
        }
        Scenario::VersionMismatch if carrier.surface != Surface::Jsonl => {
            Applicability::NotApplicable
        }
        Scenario::BlockedSink | Scenario::EpipeSink
            if carrier.entry == EntryPoint::InteractiveChat =>
        {
            Applicability::NotApplicable
        }
        _ => Applicability::Required,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutcomeClass {
    Succeeded,
    PartiallySucceeded,
    UserFault,
    ProviderFault,
    ToolFault,
    Cancelled,
    Interrupted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssistantDisposition {
    Committed,
    Discarded,
    Absent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PersistenceState {
    Committed,
    Preserved,
    Failed,
    NotRequested,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryState {
    Delivered,
    Failed,
    NotAttempted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CleanupState {
    Joined,
    TypedResidue,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FixtureEvent {
    IdentityAssigned(&'static str),
    Accepted(&'static str),
    Settled {
        run_id: &'static str,
        class: OutcomeClass,
        assistant: AssistantDisposition,
    },
    Joined {
        run_id: &'static str,
        cleanup: CleanupState,
    },
    Persisted {
        run_id: &'static str,
        state: PersistenceState,
    },
    DeliveryFinished {
        run_id: &'static str,
        state: DeliveryState,
    },
    TerminalPublished(&'static str),
    WorkObserved(&'static str),
    ProcessExited(i32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FixtureViolation {
    IdentityMissing,
    IdentityReassigned,
    IdentityAssignedAfterAcceptance,
    CrossRunEvent,
    PhaseRegression,
    DuplicateTerminal,
    WorkAfterTerminal,
    TerminalMissing,
    ProcessExitMissing,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Observation {
    pub run_id: Option<&'static str>,
    pub class: Option<OutcomeClass>,
    pub assistant: Option<AssistantDisposition>,
    pub cleanup: Option<CleanupState>,
    pub persistence: Option<PersistenceState>,
    pub delivery: Option<DeliveryState>,
    pub terminal_count: usize,
    pub exit_code: Option<i32>,
    pub violations: Vec<FixtureViolation>,
}

impl Observation {
    fn push_once(&mut self, violation: FixtureViolation) {
        if !self.violations.contains(&violation) {
            self.violations.push(violation);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Phase {
    Accepted,
    Settled,
    Joined,
    Persisted,
    Delivered,
    Terminal,
    Exited,
}

fn event_run_id(event: FixtureEvent) -> Option<&'static str> {
    match event {
        FixtureEvent::IdentityAssigned(run_id)
        | FixtureEvent::Accepted(run_id)
        | FixtureEvent::Settled { run_id, .. }
        | FixtureEvent::Joined { run_id, .. }
        | FixtureEvent::Persisted { run_id, .. }
        | FixtureEvent::DeliveryFinished { run_id, .. }
        | FixtureEvent::TerminalPublished(run_id)
        | FixtureEvent::WorkObserved(run_id) => Some(run_id),
        FixtureEvent::ProcessExited(_) => None,
    }
}

fn event_phase(event: FixtureEvent) -> Option<Phase> {
    match event {
        FixtureEvent::IdentityAssigned(_) | FixtureEvent::WorkObserved(_) => None,
        FixtureEvent::Accepted(_) => Some(Phase::Accepted),
        FixtureEvent::Settled { .. } => Some(Phase::Settled),
        FixtureEvent::Joined { .. } => Some(Phase::Joined),
        FixtureEvent::Persisted { .. } => Some(Phase::Persisted),
        FixtureEvent::DeliveryFinished { .. } => Some(Phase::Delivered),
        FixtureEvent::TerminalPublished(_) => Some(Phase::Terminal),
        FixtureEvent::ProcessExited(_) => Some(Phase::Exited),
    }
}

/// Fold a scripted carrier trace into one observation without creating a
/// provider, opening a socket, reading ambient configuration, or touching a
/// session store.
pub fn observe(events: &[FixtureEvent]) -> Observation {
    let mut observation = Observation {
        run_id: None,
        class: None,
        assistant: None,
        cleanup: None,
        persistence: None,
        delivery: None,
        terminal_count: 0,
        exit_code: None,
        violations: Vec::new(),
    };
    let mut accepted = false;
    let mut last_phase = None;

    for event in events.iter().copied() {
        if observation.terminal_count > 0
            && !matches!(
                event,
                FixtureEvent::TerminalPublished(_) | FixtureEvent::ProcessExited(_)
            )
        {
            observation.push_once(FixtureViolation::WorkAfterTerminal);
        }

        if let Some(event_id) = event_run_id(event) {
            match observation.run_id {
                Some(run_id) if run_id != event_id => {
                    observation.push_once(FixtureViolation::CrossRunEvent);
                    continue;
                }
                None if !matches!(event, FixtureEvent::IdentityAssigned(_)) => {
                    observation.push_once(FixtureViolation::IdentityMissing);
                }
                _ => {}
            }
        }

        if let Some(phase) = event_phase(event) {
            if last_phase.is_some_and(|previous| phase <= previous) {
                observation.push_once(FixtureViolation::PhaseRegression);
            } else {
                last_phase = Some(phase);
            }
        }

        match event {
            FixtureEvent::IdentityAssigned(run_id) => {
                if accepted {
                    observation.push_once(FixtureViolation::IdentityAssignedAfterAcceptance);
                }
                match observation.run_id {
                    None => observation.run_id = Some(run_id),
                    Some(existing) if existing != run_id => {
                        observation.push_once(FixtureViolation::IdentityReassigned);
                    }
                    Some(_) => {}
                }
            }
            FixtureEvent::Accepted(_) => accepted = true,
            FixtureEvent::Settled {
                class, assistant, ..
            } => {
                if observation.class.is_none() {
                    observation.class = Some(class);
                    observation.assistant = Some(assistant);
                }
            }
            FixtureEvent::Joined { cleanup, .. } => {
                if observation.cleanup.is_none() {
                    observation.cleanup = Some(cleanup);
                }
            }
            FixtureEvent::Persisted { state, .. } => {
                if observation.persistence.is_none() {
                    observation.persistence = Some(state);
                }
            }
            FixtureEvent::DeliveryFinished { state, .. } => {
                if observation.delivery.is_none() {
                    observation.delivery = Some(state);
                }
            }
            FixtureEvent::TerminalPublished(_) => {
                observation.terminal_count += 1;
                if observation.terminal_count > 1 {
                    observation.push_once(FixtureViolation::DuplicateTerminal);
                }
            }
            FixtureEvent::WorkObserved(_) => {}
            FixtureEvent::ProcessExited(code) => {
                if observation.exit_code.is_none() {
                    observation.exit_code = Some(code);
                }
            }
        }
    }

    if accepted && observation.terminal_count == 0 {
        observation.push_once(FixtureViolation::TerminalMissing);
    }
    if observation.exit_code.is_none() {
        observation.push_once(FixtureViolation::ProcessExitMissing);
    }
    observation
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExpectedOutcome {
    pub run_id: &'static str,
    pub class: OutcomeClass,
    pub assistant: AssistantDisposition,
    pub cleanup: CleanupState,
    pub persistence: PersistenceState,
    pub delivery: DeliveryState,
    pub terminal_count: usize,
    pub exit_code: i32,
}

impl ExpectedOutcome {
    pub fn mismatches(self, observation: &Observation) -> Vec<&'static str> {
        let mut mismatches = Vec::new();
        if observation.run_id != Some(self.run_id) {
            mismatches.push("run_id");
        }
        if observation.class != Some(self.class) {
            mismatches.push("class");
        }
        if observation.assistant != Some(self.assistant) {
            mismatches.push("assistant");
        }
        if observation.cleanup != Some(self.cleanup) {
            mismatches.push("cleanup");
        }
        if observation.persistence != Some(self.persistence) {
            mismatches.push("persistence");
        }
        if observation.delivery != Some(self.delivery) {
            mismatches.push("delivery");
        }
        if observation.terminal_count != self.terminal_count {
            mismatches.push("terminal_count");
        }
        if observation.exit_code != Some(self.exit_code) {
            mismatches.push("exit_code");
        }
        if !observation.violations.is_empty() {
            mismatches.push("violations");
        }
        mismatches
    }
}
