//! Policy-driven phase supervision for one prepared external child.
//!
//! This module remains provider-free and injects both effects and monotonic
//! time. It does not contain a production process implementation.
//! Natural exit, EOF, and crash classification remain owned by the existing
//! lifecycle truth; a future backend must compose that truth with these phase
//! deadlines without weakening either contract.

#![allow(dead_code)]

use super::authority::PreparedChildLaunchV1;
use super::lifecycle::ChildExitTruthV1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChildSupervisorCauseV1 {
    Cancel,
    Shutdown,
    StartupTimeout,
    RequestTimeout,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChildSupervisorOperationV1 {
    StartupReady,
    BeginRequest,
    RequestComplete,
    BeginDrain,
    DrainComplete,
    TerminateProcessGroup,
    ProcessGroupTerminated,
    Reap,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChildOwnedResidueReasonV1 {
    ClockRead,
    ClockRegression {
        previous_millis: u64,
        observed_millis: u64,
    },
    ClockOverflow,
    Effect(ChildSupervisorOperationV1),
    ReapDeadline {
        cause: ChildSupervisorCauseV1,
    },
    SupervisorDropped,
}

/// Linear ownership of one injected child. It is intentionally neither
/// `Clone` nor `Copy`.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct OwnedChildTokenV1(u64);

impl OwnedChildTokenV1 {
    pub(crate) fn new(value: u64) -> Self {
        Self(value)
    }

    pub(super) fn id(&self) -> u64 {
        self.0
    }
}

/// A child which could not be reaped inside the authorized deadline. The
/// effects owner must retain this non-cloneable value for later cleanup.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct OwnedChildResidueV1 {
    child: OwnedChildTokenV1,
    reason: ChildOwnedResidueReasonV1,
    detail: Option<String>,
}

pub(crate) enum ChildReapPollV1 {
    Pending(OwnedChildTokenV1),
    Reaped(ChildExitTruthV1),
    Failed {
        child: OwnedChildTokenV1,
        detail: String,
    },
}

pub(crate) trait ChildPhaseSupervisorEffectsV1 {
    fn monotonic_millis(&mut self) -> Result<u64, String>;
    fn spawn(&mut self, launch: PreparedChildLaunchV1) -> Result<OwnedChildTokenV1, String>;
    fn startup_ready(&mut self, child: &OwnedChildTokenV1) -> Result<bool, String>;
    fn begin_request(&mut self, child: &OwnedChildTokenV1) -> Result<(), String>;
    fn request_complete(&mut self, child: &OwnedChildTokenV1) -> Result<bool, String>;
    fn begin_drain(
        &mut self,
        child: &OwnedChildTokenV1,
        cause: ChildSupervisorCauseV1,
    ) -> Result<(), String>;
    fn drain_complete(&mut self, child: &OwnedChildTokenV1) -> Result<bool, String>;
    fn terminate_process_group(&mut self, child: &OwnedChildTokenV1) -> Result<(), String>;
    fn process_group_terminated(&mut self, child: &OwnedChildTokenV1) -> Result<bool, String>;
    fn poll_reap(&mut self, child: OwnedChildTokenV1) -> ChildReapPollV1;
    fn retain_unstarted_launch(&mut self, launch: PreparedChildLaunchV1);
    fn retain_owned_residue(&mut self, residue: OwnedChildResidueV1);
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ChildPhaseSupervisorFailureV1 {
    Spawn(String),
    ClockRead(String),
    ClockRegression {
        previous_millis: u64,
        observed_millis: u64,
    },
    ClockOverflow,
    Effect {
        operation: ChildSupervisorOperationV1,
        detail: String,
    },
    PhaseTimeout {
        cause: ChildSupervisorCauseV1,
        exit: ChildExitTruthV1,
    },
    ReapDeadline {
        cause: ChildSupervisorCauseV1,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ChildPhaseSupervisorStateV1 {
    Ready,
    Starting {
        deadline_millis: u64,
    },
    Running,
    Requesting {
        deadline_millis: u64,
    },
    RequestComplete,
    Draining {
        cause: ChildSupervisorCauseV1,
        deadline_millis: u64,
    },
    Killing {
        cause: ChildSupervisorCauseV1,
        deadline_millis: u64,
    },
    Reaping {
        cause: ChildSupervisorCauseV1,
        deadline_millis: u64,
    },
    StoppedBeforeSpawn(ChildSupervisorCauseV1),
    Stopped {
        cause: ChildSupervisorCauseV1,
        exit: ChildExitTruthV1,
    },
    Failed(ChildPhaseSupervisorFailureV1),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChildPhaseSupervisorErrorV1 {
    StartAlreadyAttempted,
    RequestNotReady,
    RequestAlreadyStarted,
    NoActiveDeadline,
}

#[derive(Debug, Clone, Copy)]
struct ChildSupervisorBudgetsV1 {
    startup_millis: u64,
    request_millis: u64,
    drain_millis: u64,
    kill_millis: u64,
    reap_millis: u64,
}

impl ChildSupervisorBudgetsV1 {
    fn from_prepared(prepared: &PreparedChildLaunchV1) -> Self {
        let [startup_millis, request_millis, drain_millis, kill_millis, reap_millis] =
            prepared.supervision_phase_budgets_millis();
        Self {
            startup_millis,
            request_millis,
            drain_millis,
            kill_millis,
            reap_millis,
        }
    }
}

enum ChildClockFaultV1 {
    Read(String),
    Regression {
        previous_millis: u64,
        observed_millis: u64,
    },
}

/// Single-owner, policy-driven phase supervisor. It is intentionally not
/// `Clone`; its prepared proof and child token move through exactly one path.
pub(crate) struct ChildPhaseSupervisorV1<'effects, E: ChildPhaseSupervisorEffectsV1> {
    prepared: Option<PreparedChildLaunchV1>,
    budgets: ChildSupervisorBudgetsV1,
    effects: &'effects mut E,
    child: Option<OwnedChildTokenV1>,
    state: ChildPhaseSupervisorStateV1,
    last_now_millis: Option<u64>,
    start_attempted: bool,
    request_started: bool,
    termination_requested: bool,
}

impl<'effects, E> ChildPhaseSupervisorV1<'effects, E>
where
    E: ChildPhaseSupervisorEffectsV1,
{
    pub(crate) fn new(prepared: PreparedChildLaunchV1, effects: &'effects mut E) -> Self {
        let budgets = ChildSupervisorBudgetsV1::from_prepared(&prepared);
        Self {
            prepared: Some(prepared),
            budgets,
            effects,
            child: None,
            state: ChildPhaseSupervisorStateV1::Ready,
            last_now_millis: None,
            start_attempted: false,
            request_started: false,
            termination_requested: false,
        }
    }

    pub(crate) fn state(&self) -> &ChildPhaseSupervisorStateV1 {
        &self.state
    }

    pub(crate) fn start(
        &mut self,
    ) -> Result<&ChildPhaseSupervisorStateV1, ChildPhaseSupervisorErrorV1> {
        if self.start_attempted {
            return Err(ChildPhaseSupervisorErrorV1::StartAlreadyAttempted);
        }
        self.start_attempted = true;
        let Some(now) = self.now_or_fail_closed() else {
            return Ok(&self.state);
        };
        let startup_budget = self.budgets.startup_millis;
        let Some(deadline_millis) = self.deadline_or_fail_closed(now, startup_budget) else {
            return Ok(&self.state);
        };

        let launch = self
            .prepared
            .take()
            .expect("an unstarted supervisor owns one prepared launch");
        match self.effects.spawn(launch) {
            Ok(child) => {
                self.child = Some(child);
                self.state = ChildPhaseSupervisorStateV1::Starting { deadline_millis };
            }
            Err(error) => {
                self.state = ChildPhaseSupervisorStateV1::Failed(
                    ChildPhaseSupervisorFailureV1::Spawn(error),
                );
            }
        }
        Ok(&self.state)
    }

    pub(crate) fn begin_request(
        &mut self,
    ) -> Result<&ChildPhaseSupervisorStateV1, ChildPhaseSupervisorErrorV1> {
        if self.request_started {
            return Err(ChildPhaseSupervisorErrorV1::RequestAlreadyStarted);
        }
        if self.state != ChildPhaseSupervisorStateV1::Running {
            return Err(ChildPhaseSupervisorErrorV1::RequestNotReady);
        }
        self.request_started = true;
        let Some(now) = self.now_or_fail_closed() else {
            return Ok(&self.state);
        };
        let request_budget = self.budgets.request_millis;
        let Some(deadline_millis) = self.deadline_or_fail_closed(now, request_budget) else {
            return Ok(&self.state);
        };
        let result = self.effects.begin_request(
            self.child
                .as_ref()
                .expect("a running supervisor owns one child"),
        );
        if let Err(detail) = result {
            self.fail_effect(ChildSupervisorOperationV1::BeginRequest, detail);
            return Ok(&self.state);
        }
        self.state = ChildPhaseSupervisorStateV1::Requesting { deadline_millis };
        Ok(&self.state)
    }

    pub(crate) fn poll(
        &mut self,
    ) -> Result<&ChildPhaseSupervisorStateV1, ChildPhaseSupervisorErrorV1> {
        let phase = self.state.clone();
        match phase {
            ChildPhaseSupervisorStateV1::Starting { deadline_millis } => {
                let Some(now) = self.now_or_fail_closed() else {
                    return Ok(&self.state);
                };
                if now >= deadline_millis {
                    self.begin_drain_at(ChildSupervisorCauseV1::StartupTimeout, now);
                } else {
                    let ready = self.effects.startup_ready(
                        self.child
                            .as_ref()
                            .expect("a starting supervisor owns one child"),
                    );
                    match ready {
                        Ok(true) => self.state = ChildPhaseSupervisorStateV1::Running,
                        Ok(false) => {}
                        Err(detail) => {
                            self.fail_effect(ChildSupervisorOperationV1::StartupReady, detail)
                        }
                    }
                }
            }
            ChildPhaseSupervisorStateV1::Requesting { deadline_millis } => {
                let Some(now) = self.now_or_fail_closed() else {
                    return Ok(&self.state);
                };
                if now >= deadline_millis {
                    self.begin_drain_at(ChildSupervisorCauseV1::RequestTimeout, now);
                } else {
                    let complete = self.effects.request_complete(
                        self.child
                            .as_ref()
                            .expect("a requesting supervisor owns one child"),
                    );
                    match complete {
                        Ok(true) => self.state = ChildPhaseSupervisorStateV1::RequestComplete,
                        Ok(false) => {}
                        Err(detail) => {
                            self.fail_effect(ChildSupervisorOperationV1::RequestComplete, detail)
                        }
                    }
                }
            }
            ChildPhaseSupervisorStateV1::Draining {
                cause,
                deadline_millis,
            } => {
                let Some(now) = self.now_or_fail_closed() else {
                    return Ok(&self.state);
                };
                if now >= deadline_millis {
                    self.begin_kill(cause);
                } else {
                    let complete = self.effects.drain_complete(
                        self.child
                            .as_ref()
                            .expect("a draining supervisor owns one child"),
                    );
                    match complete {
                        Ok(true) => self.begin_kill(cause),
                        Ok(false) => {}
                        Err(detail) => {
                            self.fail_effect(ChildSupervisorOperationV1::DrainComplete, detail)
                        }
                    }
                }
            }
            ChildPhaseSupervisorStateV1::Killing {
                cause,
                deadline_millis,
            } => {
                let Some(now) = self.now_or_fail_closed() else {
                    return Ok(&self.state);
                };
                if now >= deadline_millis {
                    self.begin_reap(cause);
                } else {
                    let terminated = self.effects.process_group_terminated(
                        self.child
                            .as_ref()
                            .expect("a killing supervisor owns one child"),
                    );
                    match terminated {
                        Ok(true) => self.begin_reap(cause),
                        Ok(false) => {}
                        Err(detail) => self.fail_effect(
                            ChildSupervisorOperationV1::ProcessGroupTerminated,
                            detail,
                        ),
                    }
                }
            }
            ChildPhaseSupervisorStateV1::Reaping {
                cause,
                deadline_millis,
            } => {
                let Some(now) = self.now_or_fail_closed() else {
                    return Ok(&self.state);
                };
                if now >= deadline_millis {
                    self.fail_reap_deadline(cause);
                } else {
                    self.poll_reap(cause);
                }
            }
            ChildPhaseSupervisorStateV1::Ready
            | ChildPhaseSupervisorStateV1::Running
            | ChildPhaseSupervisorStateV1::RequestComplete
            | ChildPhaseSupervisorStateV1::StoppedBeforeSpawn(_)
            | ChildPhaseSupervisorStateV1::Stopped { .. }
            | ChildPhaseSupervisorStateV1::Failed(_) => {
                return Err(ChildPhaseSupervisorErrorV1::NoActiveDeadline)
            }
        }
        Ok(&self.state)
    }

    pub(crate) fn cancel(
        &mut self,
    ) -> Result<&ChildPhaseSupervisorStateV1, ChildPhaseSupervisorErrorV1> {
        self.request_drain(ChildSupervisorCauseV1::Cancel)
    }

    pub(crate) fn shutdown(
        &mut self,
    ) -> Result<&ChildPhaseSupervisorStateV1, ChildPhaseSupervisorErrorV1> {
        self.request_drain(ChildSupervisorCauseV1::Shutdown)
    }

    fn request_drain(
        &mut self,
        cause: ChildSupervisorCauseV1,
    ) -> Result<&ChildPhaseSupervisorStateV1, ChildPhaseSupervisorErrorV1> {
        match self.state.clone() {
            ChildPhaseSupervisorStateV1::Ready => {
                self.start_attempted = true;
                if let Some(prepared) = self.prepared.take() {
                    self.effects.retain_unstarted_launch(prepared);
                }
                self.state = ChildPhaseSupervisorStateV1::StoppedBeforeSpawn(cause);
            }
            ChildPhaseSupervisorStateV1::Starting { .. }
            | ChildPhaseSupervisorStateV1::Running
            | ChildPhaseSupervisorStateV1::Requesting { .. }
            | ChildPhaseSupervisorStateV1::RequestComplete => {
                let Some(now) = self.now_or_fail_closed() else {
                    return Ok(&self.state);
                };
                self.begin_drain_at(cause, now);
            }
            ChildPhaseSupervisorStateV1::Draining { .. }
            | ChildPhaseSupervisorStateV1::Killing { .. }
            | ChildPhaseSupervisorStateV1::Reaping { .. }
            | ChildPhaseSupervisorStateV1::StoppedBeforeSpawn(_)
            | ChildPhaseSupervisorStateV1::Stopped { .. }
            | ChildPhaseSupervisorStateV1::Failed(_) => {}
        }
        Ok(&self.state)
    }

    fn begin_drain_at(&mut self, cause: ChildSupervisorCauseV1, now: u64) {
        let drain_budget = self.budgets.drain_millis;
        let Some(deadline_millis) = self.deadline_or_fail_closed(now, drain_budget) else {
            return;
        };
        let result = self.effects.begin_drain(
            self.child
                .as_ref()
                .expect("a live supervisor owns one child"),
            cause,
        );
        if let Err(detail) = result {
            self.fail_effect(ChildSupervisorOperationV1::BeginDrain, detail);
            return;
        }
        self.state = ChildPhaseSupervisorStateV1::Draining {
            cause,
            deadline_millis,
        };
    }

    fn begin_kill(&mut self, cause: ChildSupervisorCauseV1) {
        self.termination_requested = true;
        let result = self.effects.terminate_process_group(
            self.child
                .as_ref()
                .expect("a draining supervisor owns one child"),
        );
        if let Err(detail) = result {
            self.fail_effect(ChildSupervisorOperationV1::TerminateProcessGroup, detail);
            return;
        }
        let Some(now) = self.now_or_fail_closed() else {
            return;
        };
        let kill_budget = self.budgets.kill_millis;
        let Some(deadline_millis) = self.deadline_or_fail_closed(now, kill_budget) else {
            return;
        };
        self.state = ChildPhaseSupervisorStateV1::Killing {
            cause,
            deadline_millis,
        };
    }

    fn begin_reap(&mut self, cause: ChildSupervisorCauseV1) {
        let Some(now) = self.now_or_fail_closed() else {
            return;
        };
        let reap_budget = self.budgets.reap_millis;
        let Some(deadline_millis) = self.deadline_or_fail_closed(now, reap_budget) else {
            return;
        };
        self.state = ChildPhaseSupervisorStateV1::Reaping {
            cause,
            deadline_millis,
        };
    }

    fn poll_reap(&mut self, cause: ChildSupervisorCauseV1) {
        let child = self
            .child
            .take()
            .expect("a reaping supervisor owns one child");
        match self.effects.poll_reap(child) {
            ChildReapPollV1::Pending(child) => self.child = Some(child),
            ChildReapPollV1::Reaped(exit) => {
                self.state = match cause {
                    ChildSupervisorCauseV1::Cancel | ChildSupervisorCauseV1::Shutdown => {
                        ChildPhaseSupervisorStateV1::Stopped { cause, exit }
                    }
                    ChildSupervisorCauseV1::StartupTimeout
                    | ChildSupervisorCauseV1::RequestTimeout => {
                        ChildPhaseSupervisorStateV1::Failed(
                            ChildPhaseSupervisorFailureV1::PhaseTimeout { cause, exit },
                        )
                    }
                };
            }
            ChildReapPollV1::Failed { child, detail } => {
                self.child = Some(child);
                self.fail_effect(ChildSupervisorOperationV1::Reap, detail);
            }
        }
    }

    fn fail_reap_deadline(&mut self, cause: ChildSupervisorCauseV1) {
        let child = self
            .child
            .take()
            .expect("a reaping supervisor owns one child");
        self.effects.retain_owned_residue(OwnedChildResidueV1 {
            child,
            reason: ChildOwnedResidueReasonV1::ReapDeadline { cause },
            detail: None,
        });
        self.state =
            ChildPhaseSupervisorStateV1::Failed(ChildPhaseSupervisorFailureV1::ReapDeadline {
                cause,
            });
    }

    fn observe_now(&mut self) -> Result<u64, ChildClockFaultV1> {
        let observed = self
            .effects
            .monotonic_millis()
            .map_err(ChildClockFaultV1::Read)?;
        if let Some(previous_millis) = self.last_now_millis {
            if observed < previous_millis {
                return Err(ChildClockFaultV1::Regression {
                    previous_millis,
                    observed_millis: observed,
                });
            }
        }
        self.last_now_millis = Some(observed);
        Ok(observed)
    }

    fn now_or_fail_closed(&mut self) -> Option<u64> {
        match self.observe_now() {
            Ok(now) => Some(now),
            Err(ChildClockFaultV1::Read(detail)) => {
                self.fail_closed(
                    ChildPhaseSupervisorFailureV1::ClockRead(detail.clone()),
                    ChildOwnedResidueReasonV1::ClockRead,
                    Some(detail),
                );
                None
            }
            Err(ChildClockFaultV1::Regression {
                previous_millis,
                observed_millis,
            }) => {
                self.fail_closed(
                    ChildPhaseSupervisorFailureV1::ClockRegression {
                        previous_millis,
                        observed_millis,
                    },
                    ChildOwnedResidueReasonV1::ClockRegression {
                        previous_millis,
                        observed_millis,
                    },
                    None,
                );
                None
            }
        }
    }

    fn deadline_or_fail_closed(&mut self, now: u64, budget: u64) -> Option<u64> {
        match now.checked_add(budget) {
            Some(deadline) => Some(deadline),
            None => {
                self.fail_closed(
                    ChildPhaseSupervisorFailureV1::ClockOverflow,
                    ChildOwnedResidueReasonV1::ClockOverflow,
                    None,
                );
                None
            }
        }
    }

    fn fail_effect(&mut self, operation: ChildSupervisorOperationV1, detail: String) {
        self.fail_closed(
            ChildPhaseSupervisorFailureV1::Effect {
                operation,
                detail: detail.clone(),
            },
            ChildOwnedResidueReasonV1::Effect(operation),
            Some(detail),
        );
    }

    fn fail_closed(
        &mut self,
        failure: ChildPhaseSupervisorFailureV1,
        reason: ChildOwnedResidueReasonV1,
        mut detail: Option<String>,
    ) {
        if let Some(prepared) = self.prepared.take() {
            self.effects.retain_unstarted_launch(prepared);
        }
        if let Some(child) = self.child.take() {
            if !self.termination_requested {
                self.termination_requested = true;
                if let Err(terminate_error) = self.effects.terminate_process_group(&child) {
                    match &mut detail {
                        Some(detail) => {
                            detail.push_str("; terminate process group: ");
                            detail.push_str(&terminate_error);
                        }
                        None => detail = Some(terminate_error),
                    }
                }
            }
            self.effects.retain_owned_residue(OwnedChildResidueV1 {
                child,
                reason,
                detail,
            });
        }
        self.state = ChildPhaseSupervisorStateV1::Failed(failure);
    }
}

impl<E: ChildPhaseSupervisorEffectsV1> Drop for ChildPhaseSupervisorV1<'_, E> {
    fn drop(&mut self) {
        if let Some(prepared) = self.prepared.take() {
            self.effects.retain_unstarted_launch(prepared);
        }
        let Some(child) = self.child.take() else {
            return;
        };

        let mut detail = None;
        if !self.termination_requested {
            self.termination_requested = true;
            detail = self.effects.terminate_process_group(&child).err();
        }
        self.effects.retain_owned_residue(OwnedChildResidueV1 {
            child,
            reason: ChildOwnedResidueReasonV1::SupervisorDropped,
            detail,
        });
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::external::authority::{
        prepare_child_launch_request_v1, AdmittedChildLaunchV1, AuthorityProfileV1,
        ChildEnvironmentPolicyV1, ChildIoLimitsV1, ChildLaunchAuthorityDecisionV1,
        ChildLaunchModeV1, ChildLaunchPreparationV1, ChildLaunchRequestV1, ChildNetworkPolicyV1,
        ChildPhaseBudgetsV1, ChildProcessGroupPolicyV1, ChildRuntimePolicyV1,
        ChildTimeoutDispositionV1, PreparedChildLaunchV1,
    };
    use crate::external::lifecycle::ChildExitTruthV1;

    const EXECUTABLE: &str = "/opt/kitsu-fixtures/stdio-peer";
    const WORKING_DIRECTORY: &str = "/tmp/kitsu-external";
    const DIGEST: &str = "abababababababababababababababababababababababababababababababab";
    const STARTUP_MILLIS: u64 = 5;
    const REQUEST_MILLIS: u64 = 7;
    const DRAIN_MILLIS: u64 = 9;
    const KILL_MILLIS: u64 = 11;
    const REAP_MILLIS: u64 = 13;

    fn runtime_policy() -> ChildRuntimePolicyV1 {
        ChildRuntimePolicyV1::new(
            ChildPhaseBudgetsV1::new(
                STARTUP_MILLIS,
                REQUEST_MILLIS,
                DRAIN_MILLIS,
                KILL_MILLIS,
                REAP_MILLIS,
            ),
            ChildIoLimitsV1::new(64 * 1024, 256 * 1024, 64 * 1024, 32 * 1024),
            ChildProcessGroupPolicyV1::Owned,
            ChildEnvironmentPolicyV1::new(true, Vec::new()),
            ChildNetworkPolicyV1::Deny,
            ChildTimeoutDispositionV1::TerminateProcessGroupAndReap,
        )
    }

    #[derive(Default)]
    struct PreparationFixture;

    impl ChildLaunchPreparationV1 for PreparationFixture {
        fn canonicalize_executable(&mut self, executable: &Path) -> Result<PathBuf, String> {
            Ok(executable.to_path_buf())
        }

        fn executable_sha256(&mut self, _: &Path) -> Result<[u8; 32], String> {
            Ok([0xab; 32])
        }

        fn containment_preflight(&mut self, _: &AdmittedChildLaunchV1) -> Result<(), String> {
            Ok(())
        }
    }

    fn prepared_launch() -> PreparedChildLaunchV1 {
        let request = ChildLaunchRequestV1::new(
            1,
            ChildLaunchModeV1::Enabled,
            EXECUTABLE,
            DIGEST,
            vec!["--stdio".to_string()],
            WORKING_DIRECTORY,
            runtime_policy(),
        );
        let profile = AuthorityProfileV1::exact(
            EXECUTABLE,
            DIGEST,
            vec!["--stdio".to_string()],
            WORKING_DIRECTORY,
            runtime_policy(),
        )
        .unwrap();
        prepare_child_launch_request_v1(
            request,
            ChildLaunchAuthorityDecisionV1::Admit(&profile),
            &mut PreparationFixture,
        )
        .unwrap()
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum ReapScript {
        Pending,
        Reaped(ChildExitTruthV1),
        Failed,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum EffectEvent {
        Now,
        Spawn,
        StartupReady,
        BeginRequest,
        RequestComplete,
        BeginDrain(ChildSupervisorCauseV1),
        DrainComplete,
        TerminateProcessGroup,
        ProcessGroupTerminated,
        Reap,
        RetainPrepared,
        RetainResidue(ChildOwnedResidueReasonV1),
    }

    struct SupervisorFixture {
        events: Vec<EffectEvent>,
        times: VecDeque<Result<u64, String>>,
        spawn_error: Option<String>,
        startup_ready: VecDeque<Result<bool, String>>,
        begin_request_error: Option<String>,
        request_complete: VecDeque<Result<bool, String>>,
        begin_drain_error: Option<String>,
        drain_complete: VecDeque<Result<bool, String>>,
        terminate_error: Option<String>,
        process_group_terminated: VecDeque<Result<bool, String>>,
        reap: VecDeque<ReapScript>,
        failed_spawn_launches: Vec<PreparedChildLaunchV1>,
        retained_prepared: Vec<PreparedChildLaunchV1>,
        retained_residue: Vec<OwnedChildResidueV1>,
    }

    impl SupervisorFixture {
        fn with_times(times: impl IntoIterator<Item = u64>) -> Self {
            Self {
                events: Vec::new(),
                times: times.into_iter().map(Ok).collect(),
                spawn_error: None,
                startup_ready: VecDeque::new(),
                begin_request_error: None,
                request_complete: VecDeque::new(),
                begin_drain_error: None,
                drain_complete: VecDeque::new(),
                terminate_error: None,
                process_group_terminated: VecDeque::new(),
                reap: VecDeque::new(),
                failed_spawn_launches: Vec::new(),
                retained_prepared: Vec::new(),
                retained_residue: Vec::new(),
            }
        }

        fn next_bool(queue: &mut VecDeque<Result<bool, String>>) -> Result<bool, String> {
            queue.pop_front().unwrap_or(Ok(false))
        }
    }

    impl ChildPhaseSupervisorEffectsV1 for SupervisorFixture {
        fn monotonic_millis(&mut self) -> Result<u64, String> {
            self.events.push(EffectEvent::Now);
            self.times
                .pop_front()
                .expect("fixture monotonic time is configured")
        }

        fn spawn(&mut self, launch: PreparedChildLaunchV1) -> Result<OwnedChildTokenV1, String> {
            self.events.push(EffectEvent::Spawn);
            if let Some(error) = self.spawn_error.take() {
                self.failed_spawn_launches.push(launch);
                return Err(error);
            }
            Ok(OwnedChildTokenV1::new(7))
        }

        fn startup_ready(&mut self, _: &OwnedChildTokenV1) -> Result<bool, String> {
            self.events.push(EffectEvent::StartupReady);
            Self::next_bool(&mut self.startup_ready)
        }

        fn begin_request(&mut self, _: &OwnedChildTokenV1) -> Result<(), String> {
            self.events.push(EffectEvent::BeginRequest);
            match self.begin_request_error.take() {
                Some(error) => Err(error),
                None => Ok(()),
            }
        }

        fn request_complete(&mut self, _: &OwnedChildTokenV1) -> Result<bool, String> {
            self.events.push(EffectEvent::RequestComplete);
            Self::next_bool(&mut self.request_complete)
        }

        fn begin_drain(
            &mut self,
            _: &OwnedChildTokenV1,
            cause: ChildSupervisorCauseV1,
        ) -> Result<(), String> {
            self.events.push(EffectEvent::BeginDrain(cause));
            match self.begin_drain_error.take() {
                Some(error) => Err(error),
                None => Ok(()),
            }
        }

        fn drain_complete(&mut self, _: &OwnedChildTokenV1) -> Result<bool, String> {
            self.events.push(EffectEvent::DrainComplete);
            Self::next_bool(&mut self.drain_complete)
        }

        fn terminate_process_group(&mut self, _: &OwnedChildTokenV1) -> Result<(), String> {
            self.events.push(EffectEvent::TerminateProcessGroup);
            match self.terminate_error.take() {
                Some(error) => Err(error),
                None => Ok(()),
            }
        }

        fn process_group_terminated(&mut self, _: &OwnedChildTokenV1) -> Result<bool, String> {
            self.events.push(EffectEvent::ProcessGroupTerminated);
            Self::next_bool(&mut self.process_group_terminated)
        }

        fn poll_reap(&mut self, child: OwnedChildTokenV1) -> ChildReapPollV1 {
            self.events.push(EffectEvent::Reap);
            match self.reap.pop_front().unwrap_or(ReapScript::Pending) {
                ReapScript::Pending => ChildReapPollV1::Pending(child),
                ReapScript::Reaped(exit) => ChildReapPollV1::Reaped(exit),
                ReapScript::Failed => ChildReapPollV1::Failed {
                    child,
                    detail: "fixture reap fault".to_string(),
                },
            }
        }

        fn retain_unstarted_launch(&mut self, launch: PreparedChildLaunchV1) {
            self.events.push(EffectEvent::RetainPrepared);
            self.retained_prepared.push(launch);
        }

        fn retain_owned_residue(&mut self, residue: OwnedChildResidueV1) {
            self.events.push(EffectEvent::RetainResidue(residue.reason));
            self.retained_residue.push(residue);
        }
    }

    #[derive(Debug, Clone, Copy)]
    enum EffectFaultStage {
        StartupReady,
        BeginRequest,
        BeginDrain,
        DrainComplete,
        TerminateProcessGroup,
        Reap,
    }

    fn run_post_spawn_effect_fault(stage: EffectFaultStage) -> SupervisorFixture {
        let times = match stage {
            EffectFaultStage::DrainComplete => vec![100, 101, 102, 103],
            EffectFaultStage::TerminateProcessGroup | EffectFaultStage::Reap => {
                vec![100, 101, 102, 111, 112, 113, 114, 115]
            }
            EffectFaultStage::StartupReady
            | EffectFaultStage::BeginRequest
            | EffectFaultStage::BeginDrain => vec![100, 101, 102],
        };
        let mut effects = SupervisorFixture::with_times(times);
        let expected_operation = match stage {
            EffectFaultStage::StartupReady => {
                effects
                    .startup_ready
                    .push_back(Err("startup-ready fault".to_string()));
                ChildSupervisorOperationV1::StartupReady
            }
            EffectFaultStage::BeginRequest => {
                effects.startup_ready.push_back(Ok(true));
                effects.begin_request_error = Some("begin-request fault".to_string());
                ChildSupervisorOperationV1::BeginRequest
            }
            EffectFaultStage::BeginDrain => {
                effects.startup_ready.push_back(Ok(true));
                effects.begin_drain_error = Some("begin-drain fault".to_string());
                ChildSupervisorOperationV1::BeginDrain
            }
            EffectFaultStage::DrainComplete => {
                effects.startup_ready.push_back(Ok(true));
                effects
                    .drain_complete
                    .push_back(Err("drain-complete fault".to_string()));
                ChildSupervisorOperationV1::DrainComplete
            }
            EffectFaultStage::TerminateProcessGroup => {
                effects.startup_ready.push_back(Ok(true));
                effects.terminate_error = Some("terminate fault".to_string());
                ChildSupervisorOperationV1::TerminateProcessGroup
            }
            EffectFaultStage::Reap => {
                effects.startup_ready.push_back(Ok(true));
                effects.process_group_terminated.push_back(Ok(true));
                effects.reap.push_back(ReapScript::Failed);
                ChildSupervisorOperationV1::Reap
            }
        };

        {
            let mut supervisor = ChildPhaseSupervisorV1::new(prepared_launch(), &mut effects);
            supervisor.start().unwrap();
            match stage {
                EffectFaultStage::StartupReady => {
                    supervisor.poll().unwrap();
                }
                EffectFaultStage::BeginRequest => {
                    supervisor.poll().unwrap();
                    supervisor.begin_request().unwrap();
                }
                EffectFaultStage::BeginDrain => {
                    supervisor.poll().unwrap();
                    supervisor.cancel().unwrap();
                }
                EffectFaultStage::DrainComplete => {
                    supervisor.poll().unwrap();
                    supervisor.cancel().unwrap();
                    supervisor.poll().unwrap();
                }
                EffectFaultStage::TerminateProcessGroup => {
                    supervisor.poll().unwrap();
                    supervisor.cancel().unwrap();
                    supervisor.poll().unwrap();
                }
                EffectFaultStage::Reap => {
                    supervisor.poll().unwrap();
                    supervisor.cancel().unwrap();
                    supervisor.poll().unwrap();
                    supervisor.poll().unwrap();
                    supervisor.poll().unwrap();
                }
            }
            assert!(
                matches!(
                    supervisor.state(),
                    ChildPhaseSupervisorStateV1::Failed(
                        ChildPhaseSupervisorFailureV1::Effect { operation, .. }
                    ) if *operation == expected_operation
                ),
                "fault stage {stage:?} ended in {:?}, expected {expected_operation:?}",
                supervisor.state()
            );
        }

        assert_eq!(effects.retained_residue.len(), 1);
        assert_eq!(
            effects.retained_residue[0].reason,
            ChildOwnedResidueReasonV1::Effect(expected_operation)
        );
        assert_eq!(
            effects
                .events
                .iter()
                .filter(|event| matches!(event, EffectEvent::RetainResidue(_)))
                .count(),
            1
        );
        let terminate = effects
            .events
            .iter()
            .position(|event| *event == EffectEvent::TerminateProcessGroup)
            .unwrap();
        let retain = effects
            .events
            .iter()
            .position(|event| matches!(event, EffectEvent::RetainResidue(_)))
            .unwrap();
        assert!(terminate < retain);
        effects
    }

    #[test]
    fn spawn_failure_is_terminal_without_child_residue_or_later_effects() {
        let mut effects = SupervisorFixture::with_times([100]);
        effects.spawn_error = Some("fixture spawn fault".to_string());
        {
            let mut supervisor = ChildPhaseSupervisorV1::new(prepared_launch(), &mut effects);
            assert_eq!(
                supervisor.start(),
                Ok(&ChildPhaseSupervisorStateV1::Failed(
                    ChildPhaseSupervisorFailureV1::Spawn("fixture spawn fault".to_string())
                ))
            );
        }
        assert_eq!(effects.events, [EffectEvent::Now, EffectEvent::Spawn]);
        assert_eq!(effects.failed_spawn_launches.len(), 1);
        assert!(effects.retained_residue.is_empty());
        assert!(effects.retained_prepared.is_empty());
    }

    #[test]
    fn every_post_spawn_effect_fault_terminates_then_transfers_one_typed_residue() {
        for stage in [
            EffectFaultStage::StartupReady,
            EffectFaultStage::BeginRequest,
            EffectFaultStage::BeginDrain,
            EffectFaultStage::DrainComplete,
            EffectFaultStage::TerminateProcessGroup,
            EffectFaultStage::Reap,
        ] {
            let effects = run_post_spawn_effect_fault(stage);
            assert!(!effects
                .events
                .iter()
                .any(|event| { matches!(event, EffectEvent::RetainPrepared) }));
        }
    }

    #[test]
    fn startup_ready_before_deadline_enters_running() {
        let mut effects = SupervisorFixture::with_times([100, 104]);
        effects.startup_ready.push_back(Ok(true));
        {
            let mut supervisor = ChildPhaseSupervisorV1::new(prepared_launch(), &mut effects);
            assert_eq!(
                supervisor.start(),
                Ok(&ChildPhaseSupervisorStateV1::Starting {
                    deadline_millis: 105,
                })
            );
            assert_eq!(supervisor.poll(), Ok(&ChildPhaseSupervisorStateV1::Running));
        }
        assert_eq!(
            effects.events[..4],
            [
                EffectEvent::Now,
                EffectEvent::Spawn,
                EffectEvent::Now,
                EffectEvent::StartupReady,
            ]
        );
    }

    #[test]
    fn startup_deadline_equality_times_out_without_polling_ready() {
        let mut effects = SupervisorFixture::with_times([100, 105]);
        effects.startup_ready.push_back(Ok(true));
        {
            let mut supervisor = ChildPhaseSupervisorV1::new(prepared_launch(), &mut effects);
            supervisor.start().unwrap();
            assert_eq!(
                supervisor.poll(),
                Ok(&ChildPhaseSupervisorStateV1::Draining {
                    cause: ChildSupervisorCauseV1::StartupTimeout,
                    deadline_millis: 105 + DRAIN_MILLIS,
                })
            );
        }
        assert!(!effects.events.contains(&EffectEvent::StartupReady));
        assert!(effects.events.contains(&EffectEvent::BeginDrain(
            ChildSupervisorCauseV1::StartupTimeout
        )));
    }

    #[test]
    fn request_is_ready_gated_single_use_and_uses_exact_request_budget() {
        let mut effects = SupervisorFixture::with_times([100, 101, 102, 108]);
        effects.startup_ready.push_back(Ok(true));
        effects.request_complete.push_back(Ok(true));
        {
            let mut supervisor = ChildPhaseSupervisorV1::new(prepared_launch(), &mut effects);
            supervisor.start().unwrap();
            assert_eq!(
                supervisor.begin_request(),
                Err(ChildPhaseSupervisorErrorV1::RequestNotReady)
            );
            supervisor.poll().unwrap();
            assert_eq!(
                supervisor.begin_request(),
                Ok(&ChildPhaseSupervisorStateV1::Requesting {
                    deadline_millis: 102 + REQUEST_MILLIS,
                })
            );
            assert_eq!(
                supervisor.poll(),
                Ok(&ChildPhaseSupervisorStateV1::RequestComplete)
            );
            assert_eq!(
                supervisor.begin_request(),
                Err(ChildPhaseSupervisorErrorV1::RequestAlreadyStarted)
            );
        }
        assert_eq!(
            effects
                .events
                .iter()
                .filter(|event| **event == EffectEvent::BeginRequest)
                .count(),
            1
        );
    }

    #[test]
    fn request_deadline_equality_enters_bounded_drain() {
        let mut effects = SupervisorFixture::with_times([100, 101, 102, 109]);
        effects.startup_ready.push_back(Ok(true));
        effects.request_complete.push_back(Ok(true));
        {
            let mut supervisor = ChildPhaseSupervisorV1::new(prepared_launch(), &mut effects);
            supervisor.start().unwrap();
            supervisor.poll().unwrap();
            supervisor.begin_request().unwrap();
            assert_eq!(
                supervisor.poll(),
                Ok(&ChildPhaseSupervisorStateV1::Draining {
                    cause: ChildSupervisorCauseV1::RequestTimeout,
                    deadline_millis: 109 + DRAIN_MILLIS,
                })
            );
        }
        assert!(!effects.events.contains(&EffectEvent::RequestComplete));
    }

    #[test]
    fn drain_escalation_terminates_group_before_kill_budget_and_reap() {
        let mut effects = SupervisorFixture::with_times([100, 101, 102, 111, 112, 113, 114, 115]);
        effects.startup_ready.push_back(Ok(true));
        effects.process_group_terminated.push_back(Ok(true));
        effects
            .reap
            .push_back(ReapScript::Reaped(ChildExitTruthV1::Exited(0)));
        {
            let mut supervisor = ChildPhaseSupervisorV1::new(prepared_launch(), &mut effects);
            supervisor.start().unwrap();
            supervisor.poll().unwrap();
            assert_eq!(
                supervisor.cancel(),
                Ok(&ChildPhaseSupervisorStateV1::Draining {
                    cause: ChildSupervisorCauseV1::Cancel,
                    deadline_millis: 102 + DRAIN_MILLIS,
                })
            );
            assert_eq!(
                supervisor.poll(),
                Ok(&ChildPhaseSupervisorStateV1::Killing {
                    cause: ChildSupervisorCauseV1::Cancel,
                    deadline_millis: 112 + KILL_MILLIS,
                })
            );
            assert_eq!(
                supervisor.poll(),
                Ok(&ChildPhaseSupervisorStateV1::Reaping {
                    cause: ChildSupervisorCauseV1::Cancel,
                    deadline_millis: 114 + REAP_MILLIS,
                })
            );
            assert_eq!(
                supervisor.poll(),
                Ok(&ChildPhaseSupervisorStateV1::Stopped {
                    cause: ChildSupervisorCauseV1::Cancel,
                    exit: ChildExitTruthV1::Exited(0),
                })
            );
        }
        let terminate = effects
            .events
            .iter()
            .position(|event| *event == EffectEvent::TerminateProcessGroup)
            .unwrap();
        let group_poll = effects
            .events
            .iter()
            .position(|event| *event == EffectEvent::ProcessGroupTerminated)
            .unwrap();
        let reap = effects
            .events
            .iter()
            .position(|event| *event == EffectEvent::Reap)
            .unwrap();
        assert!(terminate < group_poll && group_poll < reap);
        assert_eq!(effects.events[terminate + 1], EffectEvent::Now);
        assert_eq!(effects.events[group_poll + 1], EffectEvent::Now);
    }

    #[test]
    fn reap_deadline_transfers_typed_owned_residue_and_never_succeeds() {
        let mut effects = SupervisorFixture::with_times([100, 101, 102, 111, 112, 113, 114, 127]);
        effects.startup_ready.push_back(Ok(true));
        effects.process_group_terminated.push_back(Ok(true));
        effects
            .reap
            .push_back(ReapScript::Reaped(ChildExitTruthV1::Exited(0)));
        {
            let mut supervisor = ChildPhaseSupervisorV1::new(prepared_launch(), &mut effects);
            supervisor.start().unwrap();
            supervisor.poll().unwrap();
            supervisor.shutdown().unwrap();
            supervisor.poll().unwrap();
            supervisor.poll().unwrap();
            assert_eq!(
                supervisor.poll(),
                Ok(&ChildPhaseSupervisorStateV1::Failed(
                    ChildPhaseSupervisorFailureV1::ReapDeadline {
                        cause: ChildSupervisorCauseV1::Shutdown,
                    }
                ))
            );
        }
        assert!(!effects.events.contains(&EffectEvent::Reap));
        assert_eq!(effects.retained_residue.len(), 1);
        assert_eq!(
            effects.retained_residue[0].reason,
            ChildOwnedResidueReasonV1::ReapDeadline {
                cause: ChildSupervisorCauseV1::Shutdown,
            }
        );
    }

    #[test]
    fn cancellation_and_shutdown_are_idempotent_from_every_live_phase() {
        for ready_polls_before_request in [0, 1, 2] {
            let mut effects = SupervisorFixture::with_times([100, 101, 102, 103, 104]);
            effects.startup_ready.push_back(Ok(true));
            {
                let mut supervisor = ChildPhaseSupervisorV1::new(prepared_launch(), &mut effects);
                supervisor.start().unwrap();
                if ready_polls_before_request >= 1 {
                    supervisor.poll().unwrap();
                }
                if ready_polls_before_request == 2 {
                    supervisor.begin_request().unwrap();
                }
                let expected = supervisor.cancel().unwrap().clone();
                assert_eq!(supervisor.shutdown(), Ok(&expected));
                assert_eq!(supervisor.cancel(), Ok(&expected));
            }
            assert_eq!(
                effects
                    .events
                    .iter()
                    .filter(|event| matches!(event, EffectEvent::BeginDrain(_)))
                    .count(),
                1
            );
        }
    }

    #[test]
    fn clock_regression_fails_closed_into_owned_residue() {
        let mut effects = SupervisorFixture::with_times([100, 99]);
        {
            let mut supervisor = ChildPhaseSupervisorV1::new(prepared_launch(), &mut effects);
            supervisor.start().unwrap();
            assert_eq!(
                supervisor.poll(),
                Ok(&ChildPhaseSupervisorStateV1::Failed(
                    ChildPhaseSupervisorFailureV1::ClockRegression {
                        previous_millis: 100,
                        observed_millis: 99,
                    }
                ))
            );
        }
        assert_eq!(effects.retained_residue.len(), 1);
        assert_eq!(
            effects.retained_residue[0].reason,
            ChildOwnedResidueReasonV1::ClockRegression {
                previous_millis: 100,
                observed_millis: 99,
            }
        );
        assert!(!effects.events.contains(&EffectEvent::StartupReady));
    }

    #[test]
    fn fresh_post_termination_clock_fault_fails_closed_before_kill_clock_starts() {
        let mut effects = SupervisorFixture::with_times([100, 101, 102, 111]);
        effects
            .times
            .push_back(Err("kill clock unavailable".to_string()));
        effects.startup_ready.push_back(Ok(true));
        {
            let mut supervisor = ChildPhaseSupervisorV1::new(prepared_launch(), &mut effects);
            supervisor.start().unwrap();
            supervisor.poll().unwrap();
            supervisor.cancel().unwrap();
            assert_eq!(
                supervisor.poll(),
                Ok(&ChildPhaseSupervisorStateV1::Failed(
                    ChildPhaseSupervisorFailureV1::ClockRead("kill clock unavailable".to_string())
                ))
            );
        }
        assert_eq!(effects.retained_residue.len(), 1);
        let terminate = effects
            .events
            .iter()
            .position(|event| *event == EffectEvent::TerminateProcessGroup)
            .unwrap();
        let failed_clock = effects
            .events
            .iter()
            .rposition(|event| *event == EffectEvent::Now)
            .unwrap();
        let retain = effects
            .events
            .iter()
            .position(|event| matches!(event, EffectEvent::RetainResidue(_)))
            .unwrap();
        assert!(terminate < failed_clock && failed_clock < retain);
    }

    #[test]
    fn fresh_post_confirmation_clock_regression_fails_before_reap_clock_starts() {
        let mut effects = SupervisorFixture::with_times([100, 101, 102, 111, 112, 113, 112]);
        effects.startup_ready.push_back(Ok(true));
        effects.process_group_terminated.push_back(Ok(true));
        {
            let mut supervisor = ChildPhaseSupervisorV1::new(prepared_launch(), &mut effects);
            supervisor.start().unwrap();
            supervisor.poll().unwrap();
            supervisor.cancel().unwrap();
            supervisor.poll().unwrap();
            assert_eq!(
                supervisor.poll(),
                Ok(&ChildPhaseSupervisorStateV1::Failed(
                    ChildPhaseSupervisorFailureV1::ClockRegression {
                        previous_millis: 113,
                        observed_millis: 112,
                    }
                ))
            );
        }
        assert_eq!(effects.retained_residue.len(), 1);
        assert_eq!(
            effects.retained_residue[0].reason,
            ChildOwnedResidueReasonV1::ClockRegression {
                previous_millis: 113,
                observed_millis: 112,
            }
        );
        assert!(!effects.events.contains(&EffectEvent::Reap));
    }

    #[test]
    fn every_deadline_overflow_fails_closed_without_silent_ownership_loss() {
        let mut startup = SupervisorFixture::with_times([u64::MAX - STARTUP_MILLIS + 1]);
        {
            let mut supervisor = ChildPhaseSupervisorV1::new(prepared_launch(), &mut startup);
            assert_eq!(
                supervisor.start(),
                Ok(&ChildPhaseSupervisorStateV1::Failed(
                    ChildPhaseSupervisorFailureV1::ClockOverflow
                ))
            );
        }
        assert!(!startup.events.contains(&EffectEvent::Spawn));
        assert_eq!(startup.retained_prepared.len(), 1);

        let mut request = SupervisorFixture::with_times([100, 101, u64::MAX - REQUEST_MILLIS + 1]);
        request.startup_ready.push_back(Ok(true));
        {
            let mut supervisor = ChildPhaseSupervisorV1::new(prepared_launch(), &mut request);
            supervisor.start().unwrap();
            supervisor.poll().unwrap();
            assert_eq!(
                supervisor.begin_request(),
                Ok(&ChildPhaseSupervisorStateV1::Failed(
                    ChildPhaseSupervisorFailureV1::ClockOverflow
                ))
            );
        }
        assert_eq!(request.retained_residue.len(), 1);

        let mut drain = SupervisorFixture::with_times([100, 101, u64::MAX - DRAIN_MILLIS + 1]);
        drain.startup_ready.push_back(Ok(true));
        {
            let mut supervisor = ChildPhaseSupervisorV1::new(prepared_launch(), &mut drain);
            supervisor.start().unwrap();
            supervisor.poll().unwrap();
            assert_eq!(
                supervisor.cancel(),
                Ok(&ChildPhaseSupervisorStateV1::Failed(
                    ChildPhaseSupervisorFailureV1::ClockOverflow
                ))
            );
        }
        assert_eq!(drain.retained_residue.len(), 1);

        let mut kill = SupervisorFixture::with_times([
            100,
            101,
            102,
            102 + DRAIN_MILLIS,
            u64::MAX - KILL_MILLIS + 1,
        ]);
        kill.startup_ready.push_back(Ok(true));
        {
            let mut supervisor = ChildPhaseSupervisorV1::new(prepared_launch(), &mut kill);
            supervisor.start().unwrap();
            supervisor.poll().unwrap();
            supervisor.cancel().unwrap();
            assert_eq!(
                supervisor.poll(),
                Ok(&ChildPhaseSupervisorStateV1::Failed(
                    ChildPhaseSupervisorFailureV1::ClockOverflow
                ))
            );
        }
        assert_eq!(kill.retained_residue.len(), 1);

        let mut reap = SupervisorFixture::with_times([
            100,
            101,
            102,
            102 + DRAIN_MILLIS,
            102 + DRAIN_MILLIS + 1,
            102 + DRAIN_MILLIS + 2,
            u64::MAX - REAP_MILLIS + 1,
        ]);
        reap.startup_ready.push_back(Ok(true));
        reap.process_group_terminated.push_back(Ok(true));
        {
            let mut supervisor = ChildPhaseSupervisorV1::new(prepared_launch(), &mut reap);
            supervisor.start().unwrap();
            supervisor.poll().unwrap();
            supervisor.cancel().unwrap();
            supervisor.poll().unwrap();
            assert_eq!(
                supervisor.poll(),
                Ok(&ChildPhaseSupervisorStateV1::Failed(
                    ChildPhaseSupervisorFailureV1::ClockOverflow
                ))
            );
        }
        assert_eq!(reap.retained_residue.len(), 1);
    }

    #[test]
    fn exact_prepared_policy_values_define_all_phase_deadlines() {
        let mut effects = SupervisorFixture::with_times([10, 11, 12, 21, 30, 31, 32, 33]);
        effects.startup_ready.push_back(Ok(true));
        effects.process_group_terminated.push_back(Ok(true));
        {
            let mut supervisor = ChildPhaseSupervisorV1::new(prepared_launch(), &mut effects);
            assert_eq!(
                supervisor.start().unwrap(),
                &ChildPhaseSupervisorStateV1::Starting {
                    deadline_millis: 10 + STARTUP_MILLIS,
                }
            );
            supervisor.poll().unwrap();
            assert_eq!(
                supervisor.begin_request().unwrap(),
                &ChildPhaseSupervisorStateV1::Requesting {
                    deadline_millis: 12 + REQUEST_MILLIS,
                }
            );
            assert_eq!(
                supervisor.cancel().unwrap(),
                &ChildPhaseSupervisorStateV1::Draining {
                    cause: ChildSupervisorCauseV1::Cancel,
                    deadline_millis: 21 + DRAIN_MILLIS,
                }
            );
            assert_eq!(
                supervisor.poll().unwrap(),
                &ChildPhaseSupervisorStateV1::Killing {
                    cause: ChildSupervisorCauseV1::Cancel,
                    deadline_millis: 31 + KILL_MILLIS,
                }
            );
            assert_eq!(
                supervisor.poll().unwrap(),
                &ChildPhaseSupervisorStateV1::Reaping {
                    cause: ChildSupervisorCauseV1::Cancel,
                    deadline_millis: 33 + REAP_MILLIS,
                }
            );
        }
    }

    #[test]
    fn drop_transfers_prepared_or_live_child_ownership_instead_of_discarding_it() {
        let mut unstarted = SupervisorFixture::with_times([]);
        {
            let _supervisor = ChildPhaseSupervisorV1::new(prepared_launch(), &mut unstarted);
        }
        assert_eq!(unstarted.retained_prepared.len(), 1);

        let mut live = SupervisorFixture::with_times([100]);
        {
            let mut supervisor = ChildPhaseSupervisorV1::new(prepared_launch(), &mut live);
            supervisor.start().unwrap();
        }
        assert_eq!(live.retained_residue.len(), 1);
        assert_eq!(
            live.events,
            [
                EffectEvent::Now,
                EffectEvent::Spawn,
                EffectEvent::TerminateProcessGroup,
                EffectEvent::RetainResidue(ChildOwnedResidueReasonV1::SupervisorDropped),
            ]
        );
    }
}
