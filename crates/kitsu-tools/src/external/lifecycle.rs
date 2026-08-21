//! Provider-free lifecycle ownership for one prepared external child.
//!
//! The only construction input is [`PreparedChildLaunchV1`]. Process effects
//! are injected and kept private, so this state machine cannot discover,
//! admit, or prepare an executable and does not provide a production spawner.

#![allow(dead_code)]

use super::authority::PreparedChildLaunchV1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChildStreamV1 {
    Stdout,
    Stderr,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ChildRuntimeSignalsV1 {
    stdout_eof: bool,
    stderr_eof: bool,
    idle_observed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChildDrainCauseV1 {
    Cancel,
    Shutdown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChildExitTruthV1 {
    Exited(i32),
    Signalled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ChildResidueV1 {
    detail: String,
}

impl ChildResidueV1 {
    pub(crate) fn new(detail: impl Into<String>) -> Self {
        Self {
            detail: detail.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ChildJoinOutcomeV1 {
    Joined(ChildExitTruthV1),
    Residue(ChildResidueV1),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ChildStopTruthV1 {
    Exited(ChildExitTruthV1),
    Drained {
        cause: ChildDrainCauseV1,
        exit: ChildExitTruthV1,
    },
    StoppedBeforeSpawn(ChildDrainCauseV1),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ChildFailureTruthV1 {
    Spawn(String),
    Crash(ChildExitTruthV1),
    ExitResidue(ChildResidueV1),
    Drain {
        cause: ChildDrainCauseV1,
        signal_error: Option<String>,
        join: ChildJoinOutcomeV1,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ChildLifecycleStateV1 {
    Ready,
    Starting,
    Running(ChildRuntimeSignalsV1),
    Draining(ChildDrainCauseV1),
    Stopped(ChildStopTruthV1),
    Failed(ChildFailureTruthV1),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChildLifecyclePhaseV1 {
    Ready,
    Starting,
    Running,
    Draining,
    Stopped,
    Failed,
}

impl ChildLifecycleStateV1 {
    pub(crate) fn phase(&self) -> ChildLifecyclePhaseV1 {
        match self {
            Self::Ready => ChildLifecyclePhaseV1::Ready,
            Self::Starting => ChildLifecyclePhaseV1::Starting,
            Self::Running(_) => ChildLifecyclePhaseV1::Running,
            Self::Draining(_) => ChildLifecyclePhaseV1::Draining,
            Self::Stopped(_) => ChildLifecyclePhaseV1::Stopped,
            Self::Failed(_) => ChildLifecyclePhaseV1::Failed,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ChildProcessTokenV1(u64);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ChildDropCleanupReportV1 {
    state_before_drop: ChildLifecycleStateV1,
    signal_error: Option<String>,
    join: ChildJoinOutcomeV1,
}

pub(crate) trait ChildProcessEffectsV1 {
    fn spawn(&mut self, launch: PreparedChildLaunchV1) -> Result<ChildProcessTokenV1, String>;
    fn request_cancel(&mut self, child: ChildProcessTokenV1) -> Result<(), String>;
    fn request_shutdown(&mut self, child: ChildProcessTokenV1) -> Result<(), String>;
    fn join(&mut self, child: ChildProcessTokenV1) -> ChildJoinOutcomeV1;
    fn retain_drop_cleanup_truth(&mut self, report: ChildDropCleanupReportV1);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChildLifecycleErrorV1 {
    SpawnAlreadyAttempted,
    NotRunning(ChildLifecyclePhaseV1),
    LateTerminal(ChildLifecyclePhaseV1),
}

/// Single-owner lifecycle. It is intentionally not `Clone`.
pub(crate) struct ChildLifecycleOwnerV1<'effects, E: ChildProcessEffectsV1> {
    prepared: Option<PreparedChildLaunchV1>,
    effects: &'effects mut E,
    child: Option<ChildProcessTokenV1>,
    state: ChildLifecycleStateV1,
    spawn_attempted: bool,
}

impl<'effects, E> ChildLifecycleOwnerV1<'effects, E>
where
    E: ChildProcessEffectsV1,
{
    pub(crate) fn new(prepared: PreparedChildLaunchV1, effects: &'effects mut E) -> Self {
        Self {
            prepared: Some(prepared),
            effects,
            child: None,
            state: ChildLifecycleStateV1::Ready,
            spawn_attempted: false,
        }
    }

    pub(crate) fn state(&self) -> &ChildLifecycleStateV1 {
        &self.state
    }

    pub(crate) fn start(&mut self) -> Result<&ChildLifecycleStateV1, ChildLifecycleErrorV1> {
        if self.spawn_attempted {
            return Err(ChildLifecycleErrorV1::SpawnAlreadyAttempted);
        }
        self.spawn_attempted = true;
        self.state = ChildLifecycleStateV1::Starting;

        let launch = self
            .prepared
            .take()
            .expect("a ready lifecycle owns exactly one prepared launch");
        match self.effects.spawn(launch) {
            Ok(child) => {
                self.child = Some(child);
                self.state = ChildLifecycleStateV1::Running(ChildRuntimeSignalsV1::default());
            }
            Err(error) => {
                self.state = ChildLifecycleStateV1::Failed(ChildFailureTruthV1::Spawn(error));
            }
        }
        Ok(&self.state)
    }

    pub(crate) fn observe_eof(
        &mut self,
        stream: ChildStreamV1,
    ) -> Result<&ChildLifecycleStateV1, ChildLifecycleErrorV1> {
        let phase = self.state.phase();
        let ChildLifecycleStateV1::Running(signals) = &mut self.state else {
            return Err(ChildLifecycleErrorV1::NotRunning(phase));
        };
        match stream {
            ChildStreamV1::Stdout => signals.stdout_eof = true,
            ChildStreamV1::Stderr => signals.stderr_eof = true,
        }
        Ok(&self.state)
    }

    pub(crate) fn observe_idle(&mut self) -> Result<&ChildLifecycleStateV1, ChildLifecycleErrorV1> {
        let phase = self.state.phase();
        let ChildLifecycleStateV1::Running(signals) = &mut self.state else {
            return Err(ChildLifecycleErrorV1::NotRunning(phase));
        };
        signals.idle_observed = true;
        Ok(&self.state)
    }

    /// Reap a child whose exit became observable and classify only the join
    /// result owned by this lifecycle. Callers cannot inject an exit status
    /// that bypasses the process-effects join boundary.
    pub(crate) fn reap_observed_exit(
        &mut self,
    ) -> Result<&ChildLifecycleStateV1, ChildLifecycleErrorV1> {
        let phase = self.state.phase();
        match phase {
            ChildLifecyclePhaseV1::Stopped | ChildLifecyclePhaseV1::Failed => {
                return Err(ChildLifecycleErrorV1::LateTerminal(phase));
            }
            ChildLifecyclePhaseV1::Running => {}
            ChildLifecyclePhaseV1::Ready
            | ChildLifecyclePhaseV1::Starting
            | ChildLifecyclePhaseV1::Draining => {
                return Err(ChildLifecycleErrorV1::NotRunning(phase));
            }
        }

        let child = self
            .child
            .expect("a running lifecycle owns one child token");
        self.state = match self.effects.join(child) {
            ChildJoinOutcomeV1::Joined(exit) => {
                self.child.take();
                match exit {
                    ChildExitTruthV1::Exited(0) => {
                        ChildLifecycleStateV1::Stopped(ChildStopTruthV1::Exited(exit))
                    }
                    ChildExitTruthV1::Exited(_) | ChildExitTruthV1::Signalled => {
                        ChildLifecycleStateV1::Failed(ChildFailureTruthV1::Crash(exit))
                    }
                }
            }
            ChildJoinOutcomeV1::Residue(residue) => {
                ChildLifecycleStateV1::Failed(ChildFailureTruthV1::ExitResidue(residue))
            }
        };
        Ok(&self.state)
    }

    pub(crate) fn cancel(&mut self) -> Result<&ChildLifecycleStateV1, ChildLifecycleErrorV1> {
        self.drain(ChildDrainCauseV1::Cancel)
    }

    pub(crate) fn shutdown(&mut self) -> Result<&ChildLifecycleStateV1, ChildLifecycleErrorV1> {
        self.drain(ChildDrainCauseV1::Shutdown)
    }

    fn drain(
        &mut self,
        cause: ChildDrainCauseV1,
    ) -> Result<&ChildLifecycleStateV1, ChildLifecycleErrorV1> {
        let phase = self.state.phase();
        match phase {
            ChildLifecyclePhaseV1::Ready => {
                self.spawn_attempted = true;
                self.prepared.take();
                self.state =
                    ChildLifecycleStateV1::Stopped(ChildStopTruthV1::StoppedBeforeSpawn(cause));
                return Ok(&self.state);
            }
            ChildLifecyclePhaseV1::Running => {}
            ChildLifecyclePhaseV1::Stopped | ChildLifecyclePhaseV1::Failed => {
                return Ok(&self.state);
            }
            ChildLifecyclePhaseV1::Starting | ChildLifecyclePhaseV1::Draining => {
                return Err(ChildLifecycleErrorV1::NotRunning(phase));
            }
        }

        self.state = ChildLifecycleStateV1::Draining(cause);
        let child = self
            .child
            .expect("a running lifecycle owns one child token");
        let signal_error = match cause {
            ChildDrainCauseV1::Cancel => self.effects.request_cancel(child),
            ChildDrainCauseV1::Shutdown => self.effects.request_shutdown(child),
        }
        .err();
        let join = self.effects.join(child);

        match (signal_error, join) {
            (None, ChildJoinOutcomeV1::Joined(exit)) => {
                self.child.take();
                self.state =
                    ChildLifecycleStateV1::Stopped(ChildStopTruthV1::Drained { cause, exit });
            }
            (signal_error, join) => {
                if matches!(join, ChildJoinOutcomeV1::Joined(_)) {
                    self.child.take();
                }
                self.state = ChildLifecycleStateV1::Failed(ChildFailureTruthV1::Drain {
                    cause,
                    signal_error,
                    join,
                });
            }
        }
        Ok(&self.state)
    }
}

impl<E: ChildProcessEffectsV1> Drop for ChildLifecycleOwnerV1<'_, E> {
    fn drop(&mut self) {
        let Some(child) = self.child.take() else {
            return;
        };

        let state_before_drop = self.state.clone();
        self.state = ChildLifecycleStateV1::Draining(ChildDrainCauseV1::Cancel);
        let signal_error = self.effects.request_cancel(child).err();
        let join = self.effects.join(child);
        self.effects
            .retain_drop_cleanup_truth(ChildDropCleanupReportV1 {
                state_before_drop,
                signal_error,
                join,
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
        ChildEnvironmentPolicyV1, ChildFilesystemAccessV1, ChildFilesystemGrantV1,
        ChildFilesystemPolicyV1, ChildIoLimitsV1, ChildLaunchAuthorityDecisionV1,
        ChildLaunchModeV1, ChildLaunchPreparationV1, ChildLaunchRequestV1, ChildNetworkPolicyV1,
        ChildPhaseBudgetsV1, ChildProcessGroupPolicyV1, ChildRuntimePolicyV1,
        ChildTimeoutDispositionV1, PreparedChildContainmentTokenV1,
    };

    const EXECUTABLE: &str = "/opt/kitsu-fixtures/stdio-peer";
    const WORKING_DIRECTORY: &str = "/tmp/kitsu-external";
    const DIGEST: &str = "abababababababababababababababababababababababababababababababab";

    fn runtime_policy() -> ChildRuntimePolicyV1 {
        ChildRuntimePolicyV1::new(
            ChildPhaseBudgetsV1::new(5_000, 30_000, 5_000, 2_000, 5_000),
            ChildIoLimitsV1::new(64 * 1024, 256 * 1024, 64 * 1024, 32 * 1024),
            ChildProcessGroupPolicyV1::Owned,
            ChildEnvironmentPolicyV1::new(true, Vec::new()),
            ChildFilesystemPolicyV1::new(vec![ChildFilesystemGrantV1::new(
                WORKING_DIRECTORY,
                ChildFilesystemAccessV1::ReadWrite,
            )]),
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

        fn containment_preflight(
            &mut self,
            _: &AdmittedChildLaunchV1,
        ) -> Result<PreparedChildContainmentTokenV1, String> {
            Ok(PreparedChildContainmentTokenV1::new(1))
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

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum EffectEvent {
        Spawn,
        Cancel(ChildProcessTokenV1),
        Shutdown(ChildProcessTokenV1),
        Join(ChildProcessTokenV1),
        DropTruth,
    }

    struct ProcessFixture {
        events: Vec<EffectEvent>,
        spawn_result: Result<ChildProcessTokenV1, String>,
        signal_results: VecDeque<Result<(), String>>,
        join_results: VecDeque<ChildJoinOutcomeV1>,
        drop_reports: Vec<ChildDropCleanupReportV1>,
    }

    impl Default for ProcessFixture {
        fn default() -> Self {
            Self {
                events: Vec::new(),
                spawn_result: Ok(ChildProcessTokenV1(7)),
                signal_results: VecDeque::new(),
                join_results: VecDeque::new(),
                drop_reports: Vec::new(),
            }
        }
    }

    impl ProcessFixture {
        fn signal_result(&mut self) -> Result<(), String> {
            self.signal_results.pop_front().unwrap_or(Ok(()))
        }

        fn join_result(&mut self) -> ChildJoinOutcomeV1 {
            self.join_results
                .pop_front()
                .unwrap_or(ChildJoinOutcomeV1::Joined(ChildExitTruthV1::Exited(0)))
        }
    }

    impl ChildProcessEffectsV1 for ProcessFixture {
        fn spawn(&mut self, _: PreparedChildLaunchV1) -> Result<ChildProcessTokenV1, String> {
            self.events.push(EffectEvent::Spawn);
            self.spawn_result.clone()
        }

        fn request_cancel(&mut self, child: ChildProcessTokenV1) -> Result<(), String> {
            self.events.push(EffectEvent::Cancel(child));
            self.signal_result()
        }

        fn request_shutdown(&mut self, child: ChildProcessTokenV1) -> Result<(), String> {
            self.events.push(EffectEvent::Shutdown(child));
            self.signal_result()
        }

        fn join(&mut self, child: ChildProcessTokenV1) -> ChildJoinOutcomeV1 {
            self.events.push(EffectEvent::Join(child));
            self.join_result()
        }

        fn retain_drop_cleanup_truth(&mut self, report: ChildDropCleanupReportV1) {
            self.events.push(EffectEvent::DropTruth);
            self.drop_reports.push(report);
        }
    }

    #[test]
    fn construction_and_preparation_never_spawn() {
        let prepared = prepared_launch();
        let mut effects = ProcessFixture::default();
        {
            let owner = ChildLifecycleOwnerV1::new(prepared, &mut effects);
            assert_eq!(owner.state(), &ChildLifecycleStateV1::Ready);
        }
        assert!(effects.events.is_empty());
    }

    #[test]
    fn start_consumes_prepared_launch_and_refuses_duplicate_spawn() {
        let mut effects = ProcessFixture::default();
        {
            let mut owner = ChildLifecycleOwnerV1::new(prepared_launch(), &mut effects);
            assert!(matches!(
                owner.start(),
                Ok(ChildLifecycleStateV1::Running(_))
            ));
            assert_eq!(
                owner.start(),
                Err(ChildLifecycleErrorV1::SpawnAlreadyAttempted)
            );
        }
        assert_eq!(
            effects.events,
            vec![
                EffectEvent::Spawn,
                EffectEvent::Cancel(ChildProcessTokenV1(7)),
                EffectEvent::Join(ChildProcessTokenV1(7)),
                EffectEvent::DropTruth,
            ]
        );
    }

    #[test]
    fn spawn_failure_is_terminal_and_cannot_retry() {
        let mut effects = ProcessFixture {
            spawn_result: Err("fixture spawn failed".to_string()),
            ..ProcessFixture::default()
        };
        {
            let mut owner = ChildLifecycleOwnerV1::new(prepared_launch(), &mut effects);
            assert_eq!(
                owner.start(),
                Ok(&ChildLifecycleStateV1::Failed(ChildFailureTruthV1::Spawn(
                    "fixture spawn failed".to_string()
                )))
            );
            assert_eq!(
                owner.start(),
                Err(ChildLifecycleErrorV1::SpawnAlreadyAttempted)
            );
        }
        assert_eq!(effects.events, vec![EffectEvent::Spawn]);
    }

    #[test]
    fn eof_and_idle_are_retained_as_non_terminal_observations() {
        let mut effects = ProcessFixture::default();
        let mut owner = ChildLifecycleOwnerV1::new(prepared_launch(), &mut effects);
        owner.start().unwrap();
        owner.observe_eof(ChildStreamV1::Stdout).unwrap();
        owner.observe_eof(ChildStreamV1::Stderr).unwrap();
        owner.observe_idle().unwrap();

        assert_eq!(
            owner.state(),
            &ChildLifecycleStateV1::Running(ChildRuntimeSignalsV1 {
                stdout_eof: true,
                stderr_eof: true,
                idle_observed: true,
            })
        );
    }

    #[test]
    fn crash_after_eof_retains_causal_failure_truth() {
        let mut effects = ProcessFixture {
            join_results: VecDeque::from([ChildJoinOutcomeV1::Joined(ChildExitTruthV1::Exited(
                17,
            ))]),
            ..ProcessFixture::default()
        };
        let mut owner = ChildLifecycleOwnerV1::new(prepared_launch(), &mut effects);
        owner.start().unwrap();
        owner.observe_eof(ChildStreamV1::Stdout).unwrap();
        assert_eq!(
            owner.reap_observed_exit(),
            Ok(&ChildLifecycleStateV1::Failed(ChildFailureTruthV1::Crash(
                ChildExitTruthV1::Exited(17)
            )))
        );
    }

    #[test]
    fn cancel_signals_before_join_and_shutdown_is_idempotent() {
        let mut effects = ProcessFixture::default();
        {
            let mut owner = ChildLifecycleOwnerV1::new(prepared_launch(), &mut effects);
            owner.start().unwrap();
            assert_eq!(
                owner.cancel(),
                Ok(&ChildLifecycleStateV1::Stopped(ChildStopTruthV1::Drained {
                    cause: ChildDrainCauseV1::Cancel,
                    exit: ChildExitTruthV1::Exited(0),
                }))
            );
            owner.shutdown().unwrap();
            owner.cancel().unwrap();
        }
        assert_eq!(
            effects.events,
            vec![
                EffectEvent::Spawn,
                EffectEvent::Cancel(ChildProcessTokenV1(7)),
                EffectEvent::Join(ChildProcessTokenV1(7)),
            ]
        );
    }

    #[test]
    fn shutdown_signals_before_join_and_cancel_is_idempotent() {
        let mut effects = ProcessFixture::default();
        {
            let mut owner = ChildLifecycleOwnerV1::new(prepared_launch(), &mut effects);
            owner.start().unwrap();
            owner.shutdown().unwrap();
            owner.cancel().unwrap();
            owner.shutdown().unwrap();
        }
        assert_eq!(
            effects.events,
            vec![
                EffectEvent::Spawn,
                EffectEvent::Shutdown(ChildProcessTokenV1(7)),
                EffectEvent::Join(ChildProcessTokenV1(7)),
            ]
        );
    }

    #[test]
    fn duplicate_and_late_terminal_observations_are_refused() {
        let mut effects = ProcessFixture::default();
        let mut owner = ChildLifecycleOwnerV1::new(prepared_launch(), &mut effects);
        assert_eq!(
            owner.reap_observed_exit(),
            Err(ChildLifecycleErrorV1::NotRunning(
                ChildLifecyclePhaseV1::Ready
            ))
        );
        owner.start().unwrap();
        owner.reap_observed_exit().unwrap();
        assert_eq!(
            owner.reap_observed_exit(),
            Err(ChildLifecycleErrorV1::LateTerminal(
                ChildLifecyclePhaseV1::Stopped
            ))
        );
    }

    #[test]
    fn observed_exit_residue_remains_owned_for_drop_cleanup() {
        let observed_residue = ChildResidueV1::new("wait reported an unreaped descendant");
        let drop_residue = ChildResidueV1::new("descendant survived drop cleanup");
        let mut effects = ProcessFixture {
            join_results: VecDeque::from([
                ChildJoinOutcomeV1::Residue(observed_residue.clone()),
                ChildJoinOutcomeV1::Residue(drop_residue.clone()),
            ]),
            ..ProcessFixture::default()
        };
        {
            let mut owner = ChildLifecycleOwnerV1::new(prepared_launch(), &mut effects);
            owner.start().unwrap();
            assert_eq!(
                owner.reap_observed_exit(),
                Ok(&ChildLifecycleStateV1::Failed(
                    ChildFailureTruthV1::ExitResidue(observed_residue)
                ))
            );
        }

        assert_eq!(effects.drop_reports.len(), 1);
        assert_eq!(
            effects.drop_reports[0].join,
            ChildJoinOutcomeV1::Residue(drop_residue)
        );
    }

    #[test]
    fn unjoined_drain_residue_remains_typed_and_drop_retries_cleanup() {
        let residue = ChildResidueV1::new("descendant 19 still running");
        let drop_residue = ChildResidueV1::new("descendant 19 survived drop cleanup");
        let mut effects = ProcessFixture {
            join_results: VecDeque::from([
                ChildJoinOutcomeV1::Residue(residue.clone()),
                ChildJoinOutcomeV1::Residue(drop_residue.clone()),
            ]),
            ..ProcessFixture::default()
        };
        {
            let mut owner = ChildLifecycleOwnerV1::new(prepared_launch(), &mut effects);
            owner.start().unwrap();
            assert_eq!(
                owner.cancel(),
                Ok(&ChildLifecycleStateV1::Failed(ChildFailureTruthV1::Drain {
                    cause: ChildDrainCauseV1::Cancel,
                    signal_error: None,
                    join: ChildJoinOutcomeV1::Residue(residue),
                }))
            );
        }

        assert_eq!(effects.drop_reports.len(), 1);
        assert_eq!(
            effects.drop_reports[0].join,
            ChildJoinOutcomeV1::Residue(drop_residue)
        );
        assert_eq!(
            effects.events,
            vec![
                EffectEvent::Spawn,
                EffectEvent::Cancel(ChildProcessTokenV1(7)),
                EffectEvent::Join(ChildProcessTokenV1(7)),
                EffectEvent::Cancel(ChildProcessTokenV1(7)),
                EffectEvent::Join(ChildProcessTokenV1(7)),
                EffectEvent::DropTruth,
            ]
        );
    }

    #[test]
    fn drop_of_running_child_records_residue_instead_of_success() {
        let residue = ChildResidueV1::new("process group could not be joined");
        let mut effects = ProcessFixture {
            signal_results: VecDeque::from([Err("cancel signal failed".to_string())]),
            join_results: VecDeque::from([ChildJoinOutcomeV1::Residue(residue.clone())]),
            ..ProcessFixture::default()
        };
        {
            let mut owner = ChildLifecycleOwnerV1::new(prepared_launch(), &mut effects);
            owner.start().unwrap();
        }

        assert_eq!(effects.drop_reports.len(), 1);
        assert_eq!(
            effects.drop_reports[0],
            ChildDropCleanupReportV1 {
                state_before_drop: ChildLifecycleStateV1::Running(ChildRuntimeSignalsV1::default()),
                signal_error: Some("cancel signal failed".to_string()),
                join: ChildJoinOutcomeV1::Residue(residue),
            }
        );
    }
}
