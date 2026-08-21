//! Private strictly serial framed transport for one authorized child.
//!
//! This is an opaque line transport, not an MCP codec or a public process API.
//! It may own one prepared proof and then one child, with at most one frame in
//! flight. All authority, clocks, descriptors, and ownership tokens remain
//! internal to the injected-effects boundary.

#![allow(dead_code)]

use super::authority::PreparedChildLaunchV1;
use super::lifecycle::ChildExitTruthV1;
use super::supervisor::{ChildReapPollV1, OwnedChildTokenV1};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct SerialTransportPolicyV1 {
    pub(super) max_frames: u16,
    pub(super) line_millis: u64,
    pub(super) idle_millis: u64,
    pub(super) stdin_bytes: usize,
    pub(super) stdout_bytes: usize,
    pub(super) stderr_bytes: usize,
    pub(super) frame_bytes: usize,
    pub(super) drain_millis: u64,
    pub(super) kill_millis: u64,
    pub(super) reap_millis: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SerialChildCauseV1 {
    Cancel,
    Shutdown,
    StartupTimeout,
    LineTimeout,
    IdleTimeout,
    Eof,
    Crash,
    ProtocolViolation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SerialChildOperationV1 {
    StartupReady,
    BeginFrame,
    PollFrame,
    BeginDrain,
    DrainComplete,
    TerminateProcessGroup,
    ProcessGroupTerminated,
    Reap,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SerialOwnedResidueReasonV1 {
    ClockRead,
    ClockRegression {
        previous_millis: u64,
        observed_millis: u64,
    },
    ClockOverflow,
    Effect(SerialChildOperationV1),
    Transport(SerialChildCauseV1),
    ReapDeadline {
        cause: SerialChildCauseV1,
    },
    TransportDropped,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct SerialOwnedChildResidueV1 {
    child: OwnedChildTokenV1,
    reason: SerialOwnedResidueReasonV1,
    detail: Option<String>,
}

impl SerialOwnedChildResidueV1 {
    #[cfg(test)]
    pub(super) fn reason_for_test(&self) -> SerialOwnedResidueReasonV1 {
        self.reason
    }

    pub(super) fn child_id(&self) -> u64 {
        self.child.id()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SerialPollAllowanceV1 {
    pub(crate) stdin_bytes: usize,
    pub(crate) stdout_bytes: usize,
    pub(crate) stderr_bytes: usize,
    pub(crate) frame_bytes: usize,
}

pub(super) struct SerialIoProgressV1 {
    pub(super) stdin_bytes_written: usize,
    pub(super) stdout_bytes_read: usize,
    pub(super) stderr_bytes_read: usize,
}

pub(super) struct SerialOutputStatusV1 {
    pub(super) response: Option<Vec<u8>>,
    pub(super) extra_stdout_bytes: usize,
    pub(super) stdout_eof: bool,
    pub(super) stderr_eof: bool,
    pub(super) exit: Option<ChildExitTruthV1>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SerialFrameObservationV1 {
    stdin_bytes_written: usize,
    stdout_bytes_read: usize,
    stderr_bytes_read: usize,
    response: Option<Vec<u8>>,
    extra_stdout_bytes: usize,
    stdout_eof: bool,
    stderr_eof: bool,
    exit: Option<ChildExitTruthV1>,
    limit_exceeded: Option<super::authority::ChildIoLimitV1>,
}

impl SerialFrameObservationV1 {
    pub(crate) fn pending() -> Self {
        Self::progress(0, 0, 0)
    }

    pub(crate) fn progress(
        stdin_bytes_written: usize,
        stdout_bytes_read: usize,
        stderr_bytes_read: usize,
    ) -> Self {
        Self {
            stdin_bytes_written,
            stdout_bytes_read,
            stderr_bytes_read,
            response: None,
            extra_stdout_bytes: 0,
            stdout_eof: false,
            stderr_eof: false,
            exit: None,
            limit_exceeded: None,
        }
    }

    pub(crate) fn response(
        response: Vec<u8>,
        stdin_bytes_written: usize,
        stdout_bytes_read: usize,
        stderr_bytes_read: usize,
    ) -> Self {
        Self {
            response: Some(response),
            ..Self::progress(stdin_bytes_written, stdout_bytes_read, stderr_bytes_read)
        }
    }

    #[cfg(test)]
    fn response_with_extra_carry(
        response: Vec<u8>,
        stdin_bytes_written: usize,
        stdout_bytes_read: usize,
        stderr_bytes_read: usize,
        extra_stdout_bytes: usize,
    ) -> Self {
        Self {
            extra_stdout_bytes,
            ..Self::response(
                response,
                stdin_bytes_written,
                stdout_bytes_read,
                stderr_bytes_read,
            )
        }
    }

    #[cfg(test)]
    fn response_with_eof(
        response: Vec<u8>,
        stdin_bytes_written: usize,
        stdout_bytes_read: usize,
        stderr_bytes_read: usize,
    ) -> Self {
        Self {
            stdout_eof: true,
            ..Self::response(
                response,
                stdin_bytes_written,
                stdout_bytes_read,
                stderr_bytes_read,
            )
        }
    }

    #[cfg(test)]
    fn response_with_exit(
        response: Vec<u8>,
        stdin_bytes_written: usize,
        stdout_bytes_read: usize,
        stderr_bytes_read: usize,
        exit: ChildExitTruthV1,
    ) -> Self {
        Self {
            exit: Some(exit),
            ..Self::response(
                response,
                stdin_bytes_written,
                stdout_bytes_read,
                stderr_bytes_read,
            )
        }
    }

    #[cfg(test)]
    fn eof() -> Self {
        Self {
            stdout_eof: true,
            ..Self::pending()
        }
    }

    #[cfg(test)]
    fn exit(exit: ChildExitTruthV1) -> Self {
        Self {
            exit: Some(exit),
            ..Self::pending()
        }
    }

    pub(super) fn limit_exceeded(limit: super::authority::ChildIoLimitV1) -> Self {
        Self {
            limit_exceeded: Some(limit),
            ..Self::pending()
        }
    }

    pub(super) fn from_backend(progress: SerialIoProgressV1, output: SerialOutputStatusV1) -> Self {
        Self {
            stdin_bytes_written: progress.stdin_bytes_written,
            stdout_bytes_read: progress.stdout_bytes_read,
            stderr_bytes_read: progress.stderr_bytes_read,
            response: output.response,
            extra_stdout_bytes: output.extra_stdout_bytes,
            stdout_eof: output.stdout_eof,
            stderr_eof: output.stderr_eof,
            exit: output.exit,
            limit_exceeded: None,
        }
    }

    fn made_progress(&self) -> bool {
        self.stdin_bytes_written != 0 || self.stdout_bytes_read != 0 || self.stderr_bytes_read != 0
    }
}

pub(crate) trait SerialChildTransportEffectsV1 {
    fn monotonic_millis(&mut self) -> Result<u64, String>;
    fn spawn_serial(&mut self, launch: PreparedChildLaunchV1) -> Result<OwnedChildTokenV1, String>;
    fn startup_ready(&mut self, child: &OwnedChildTokenV1) -> Result<bool, String>;
    fn begin_frame(&mut self, child: &OwnedChildTokenV1, frame: &[u8]) -> Result<(), String>;
    fn poll_frame(
        &mut self,
        child: &OwnedChildTokenV1,
        allowance: SerialPollAllowanceV1,
    ) -> Result<SerialFrameObservationV1, String>;
    fn begin_drain(
        &mut self,
        child: &OwnedChildTokenV1,
        cause: SerialChildCauseV1,
    ) -> Result<(), String>;
    fn drain_complete(&mut self, child: &OwnedChildTokenV1) -> Result<bool, String>;
    fn terminate_process_group(&mut self, child: &OwnedChildTokenV1) -> Result<(), String>;
    fn process_group_terminated(&mut self, child: &OwnedChildTokenV1) -> Result<bool, String>;
    fn poll_reap(&mut self, child: OwnedChildTokenV1) -> ChildReapPollV1;
    fn retain_unstarted_launch(&mut self, launch: PreparedChildLaunchV1);
    fn retain_owned_residue(&mut self, residue: SerialOwnedChildResidueV1);
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SerialChildTransportFailureV1 {
    Spawn(String),
    ClockRead(String),
    ClockRegression {
        previous_millis: u64,
        observed_millis: u64,
    },
    ClockOverflow,
    Effect {
        operation: SerialChildOperationV1,
        detail: String,
    },
    Transport {
        cause: SerialChildCauseV1,
        exit: ChildExitTruthV1,
    },
    ReapDeadline {
        cause: SerialChildCauseV1,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SerialChildTransportStateV1 {
    Ready,
    Starting {
        deadline_millis: u64,
    },
    Idle {
        completed_frames: u16,
    },
    FrameInFlight {
        completed_frames: u16,
        line_deadline_millis: u64,
        idle_deadline_millis: u64,
    },
    ResponseReady {
        completed_frames: u16,
    },
    Exhausted {
        completed_frames: u16,
    },
    Draining {
        cause: SerialChildCauseV1,
        deadline_millis: u64,
    },
    Killing {
        cause: SerialChildCauseV1,
        deadline_millis: u64,
    },
    Reaping {
        cause: SerialChildCauseV1,
        deadline_millis: u64,
    },
    StoppedBeforeSpawn(SerialChildCauseV1),
    Stopped {
        cause: SerialChildCauseV1,
        exit: ChildExitTruthV1,
    },
    Failed(SerialChildTransportFailureV1),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SerialChildTransportErrorV1 {
    StartAlreadyAttempted,
    ChildNotReady,
    FrameAlreadyInFlight,
    ResponseNotConsumed,
    NoResponseReady,
    FrameLimitReached,
    InvalidFrame,
    FrameTooLarge,
    CumulativeLimit(super::authority::ChildIoLimitV1),
    NoActiveDeadline,
}

enum SerialClockFaultV1 {
    Read(String),
    Regression {
        previous_millis: u64,
        observed_millis: u64,
    },
}

pub(crate) struct SerialChildTransportV1<'effects, E: SerialChildTransportEffectsV1> {
    prepared: Option<PreparedChildLaunchV1>,
    policy: SerialTransportPolicyV1,
    effects: &'effects mut E,
    child: Option<OwnedChildTokenV1>,
    state: SerialChildTransportStateV1,
    response: Option<Vec<u8>>,
    last_now_millis: Option<u64>,
    start_attempted: bool,
    termination_requested: bool,
    stdin_accepted: usize,
    stdout_accepted: usize,
    stderr_accepted: usize,
    current_frame_bytes: usize,
    current_frame_written: usize,
}

impl<'effects, E> SerialChildTransportV1<'effects, E>
where
    E: SerialChildTransportEffectsV1,
{
    pub(crate) fn new(prepared: PreparedChildLaunchV1, effects: &'effects mut E) -> Self {
        let policy = prepared
            .serial_transport_policy()
            .expect("serial transport requires an authorized serial prepared proof");
        Self {
            prepared: Some(prepared),
            policy,
            effects,
            child: None,
            state: SerialChildTransportStateV1::Ready,
            response: None,
            last_now_millis: None,
            start_attempted: false,
            termination_requested: false,
            stdin_accepted: 0,
            stdout_accepted: 0,
            stderr_accepted: 0,
            current_frame_bytes: 0,
            current_frame_written: 0,
        }
    }

    pub(crate) fn state(&self) -> &SerialChildTransportStateV1 {
        &self.state
    }

    pub(crate) fn start(
        &mut self,
    ) -> Result<&SerialChildTransportStateV1, SerialChildTransportErrorV1> {
        if self.start_attempted {
            return Err(SerialChildTransportErrorV1::StartAlreadyAttempted);
        }
        self.start_attempted = true;
        let Some(now) = self.now_or_fail_closed() else {
            return Ok(&self.state);
        };
        let startup_millis = self.startup_millis();
        let Some(deadline_millis) = self.deadline_or_fail_closed(now, startup_millis) else {
            return Ok(&self.state);
        };
        let launch = self
            .prepared
            .take()
            .expect("an unstarted serial transport owns one prepared launch");
        match self.effects.spawn_serial(launch) {
            Ok(child) => {
                self.child = Some(child);
                self.state = SerialChildTransportStateV1::Starting { deadline_millis };
            }
            Err(detail) => {
                self.state = SerialChildTransportStateV1::Failed(
                    SerialChildTransportFailureV1::Spawn(detail),
                );
            }
        }
        Ok(&self.state)
    }

    fn startup_millis(&self) -> u64 {
        // Serial policy intentionally carries only post-startup values. The
        // exact startup budget remains projected by the same prepared proof.
        self.prepared
            .as_ref()
            .map(|prepared| prepared.supervision_phase_budgets_millis()[0])
            .unwrap_or_else(|| {
                // `start` consumes the proof only after this value is read.
                unreachable!("startup budget is read before spawn")
            })
    }

    pub(crate) fn begin_frame(
        &mut self,
        frame: Vec<u8>,
    ) -> Result<&SerialChildTransportStateV1, SerialChildTransportErrorV1> {
        let completed_frames = match self.state {
            SerialChildTransportStateV1::Idle { completed_frames } => completed_frames,
            SerialChildTransportStateV1::Starting { .. } | SerialChildTransportStateV1::Ready => {
                return Err(SerialChildTransportErrorV1::ChildNotReady)
            }
            SerialChildTransportStateV1::FrameInFlight { .. } => {
                return Err(SerialChildTransportErrorV1::FrameAlreadyInFlight)
            }
            SerialChildTransportStateV1::ResponseReady { .. } => {
                return Err(SerialChildTransportErrorV1::ResponseNotConsumed)
            }
            SerialChildTransportStateV1::Exhausted { .. } => {
                return Err(SerialChildTransportErrorV1::FrameLimitReached)
            }
            _ => return Err(SerialChildTransportErrorV1::ChildNotReady),
        };
        if completed_frames >= self.policy.max_frames {
            return Err(SerialChildTransportErrorV1::FrameLimitReached);
        }
        if frame.len() > self.policy.frame_bytes {
            return Err(SerialChildTransportErrorV1::FrameTooLarge);
        }
        if frame.is_empty() || !frame.ends_with(b"\n") || frame[..frame.len() - 1].contains(&b'\n')
        {
            return Err(SerialChildTransportErrorV1::InvalidFrame);
        }
        let Some(next_stdin) = self.stdin_accepted.checked_add(frame.len()) else {
            return Err(SerialChildTransportErrorV1::CumulativeLimit(
                super::authority::ChildIoLimitV1::Stdin,
            ));
        };
        if next_stdin > self.policy.stdin_bytes {
            return Err(SerialChildTransportErrorV1::CumulativeLimit(
                super::authority::ChildIoLimitV1::Stdin,
            ));
        }
        let Some(now) = self.now_or_fail_closed() else {
            return Ok(&self.state);
        };
        let Some(line_deadline_millis) = self.deadline_or_fail_closed(now, self.policy.line_millis)
        else {
            return Ok(&self.state);
        };
        let Some(idle_deadline_millis) = self.deadline_or_fail_closed(now, self.policy.idle_millis)
        else {
            return Ok(&self.state);
        };
        let result = self.effects.begin_frame(
            self.child
                .as_ref()
                .expect("an idle serial transport owns one child"),
            &frame,
        );
        if let Err(detail) = result {
            self.fail_effect(SerialChildOperationV1::BeginFrame, detail);
            return Ok(&self.state);
        }
        self.stdin_accepted = next_stdin;
        self.current_frame_bytes = frame.len();
        self.current_frame_written = 0;
        self.state = SerialChildTransportStateV1::FrameInFlight {
            completed_frames,
            line_deadline_millis,
            idle_deadline_millis,
        };
        Ok(&self.state)
    }

    pub(crate) fn take_response(&mut self) -> Result<Vec<u8>, SerialChildTransportErrorV1> {
        let SerialChildTransportStateV1::ResponseReady { completed_frames } = self.state else {
            return Err(SerialChildTransportErrorV1::NoResponseReady);
        };
        let response = self
            .response
            .take()
            .expect("response-ready state owns exactly one response");
        self.state = if completed_frames == self.policy.max_frames {
            SerialChildTransportStateV1::Exhausted { completed_frames }
        } else {
            SerialChildTransportStateV1::Idle { completed_frames }
        };
        Ok(response)
    }

    pub(crate) fn poll(
        &mut self,
    ) -> Result<&SerialChildTransportStateV1, SerialChildTransportErrorV1> {
        let phase = self.state.clone();
        match phase {
            SerialChildTransportStateV1::Starting { deadline_millis } => {
                let Some(now) = self.now_or_fail_closed() else {
                    return Ok(&self.state);
                };
                if now >= deadline_millis {
                    self.begin_drain_at(SerialChildCauseV1::StartupTimeout, now);
                } else {
                    match self.effects.startup_ready(
                        self.child
                            .as_ref()
                            .expect("a starting serial transport owns one child"),
                    ) {
                        Ok(true) => {
                            self.state = SerialChildTransportStateV1::Idle {
                                completed_frames: 0,
                            }
                        }
                        Ok(false) => {}
                        Err(detail) => {
                            self.fail_effect(SerialChildOperationV1::StartupReady, detail)
                        }
                    }
                }
            }
            SerialChildTransportStateV1::FrameInFlight {
                completed_frames,
                line_deadline_millis,
                idle_deadline_millis,
            } => {
                let Some(now) = self.now_or_fail_closed() else {
                    return Ok(&self.state);
                };
                if now >= line_deadline_millis {
                    self.begin_drain_at(SerialChildCauseV1::LineTimeout, now);
                } else if now >= idle_deadline_millis {
                    self.begin_drain_at(SerialChildCauseV1::IdleTimeout, now);
                } else {
                    self.poll_frame(completed_frames, line_deadline_millis, idle_deadline_millis);
                }
            }
            SerialChildTransportStateV1::Draining {
                cause,
                deadline_millis,
            } => {
                let Some(now) = self.now_or_fail_closed() else {
                    return Ok(&self.state);
                };
                if now >= deadline_millis {
                    self.begin_kill(cause);
                } else {
                    match self.effects.drain_complete(
                        self.child
                            .as_ref()
                            .expect("a draining serial transport owns one child"),
                    ) {
                        Ok(true) => self.begin_kill(cause),
                        Ok(false) => {}
                        Err(detail) => {
                            self.fail_effect(SerialChildOperationV1::DrainComplete, detail)
                        }
                    }
                }
            }
            SerialChildTransportStateV1::Killing {
                cause,
                deadline_millis,
            } => {
                let Some(now) = self.now_or_fail_closed() else {
                    return Ok(&self.state);
                };
                if now >= deadline_millis {
                    self.begin_reap(cause);
                } else {
                    match self.effects.process_group_terminated(
                        self.child
                            .as_ref()
                            .expect("a killing serial transport owns one child"),
                    ) {
                        Ok(true) => self.begin_reap(cause),
                        Ok(false) => {}
                        Err(detail) => {
                            self.fail_effect(SerialChildOperationV1::ProcessGroupTerminated, detail)
                        }
                    }
                }
            }
            SerialChildTransportStateV1::Reaping {
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
            SerialChildTransportStateV1::Ready
            | SerialChildTransportStateV1::Idle { .. }
            | SerialChildTransportStateV1::ResponseReady { .. }
            | SerialChildTransportStateV1::Exhausted { .. }
            | SerialChildTransportStateV1::StoppedBeforeSpawn(_)
            | SerialChildTransportStateV1::Stopped { .. }
            | SerialChildTransportStateV1::Failed(_) => {
                return Err(SerialChildTransportErrorV1::NoActiveDeadline)
            }
        }
        Ok(&self.state)
    }

    fn poll_frame(
        &mut self,
        completed_frames: u16,
        line_deadline_millis: u64,
        idle_deadline_millis: u64,
    ) {
        let allowance = SerialPollAllowanceV1 {
            stdin_bytes: self.policy.stdin_bytes.saturating_sub(self.stdin_accepted),
            stdout_bytes: self
                .policy
                .stdout_bytes
                .saturating_sub(self.stdout_accepted),
            stderr_bytes: self
                .policy
                .stderr_bytes
                .saturating_sub(self.stderr_accepted),
            frame_bytes: self.policy.frame_bytes,
        };
        let observation = self.effects.poll_frame(
            self.child
                .as_ref()
                .expect("an in-flight serial transport owns one child"),
            allowance,
        );
        let observation = match observation {
            Ok(observation) => observation,
            Err(detail) => {
                self.fail_effect(SerialChildOperationV1::PollFrame, detail);
                return;
            }
        };
        if let Some(limit) = observation.limit_exceeded {
            self.begin_protocol_failure(SerialChildCauseV1::ProtocolViolation);
            let _ = limit;
            return;
        }
        let Some(next_stdout) = self
            .stdout_accepted
            .checked_add(observation.stdout_bytes_read)
        else {
            self.begin_protocol_failure(SerialChildCauseV1::ProtocolViolation);
            return;
        };
        let Some(next_stderr) = self
            .stderr_accepted
            .checked_add(observation.stderr_bytes_read)
        else {
            self.begin_protocol_failure(SerialChildCauseV1::ProtocolViolation);
            return;
        };
        if next_stdout > self.policy.stdout_bytes || next_stderr > self.policy.stderr_bytes {
            self.begin_protocol_failure(SerialChildCauseV1::ProtocolViolation);
            return;
        }
        let Some(next_frame_written) = self
            .current_frame_written
            .checked_add(observation.stdin_bytes_written)
        else {
            self.begin_protocol_failure(SerialChildCauseV1::ProtocolViolation);
            return;
        };
        if next_frame_written > self.current_frame_bytes {
            self.begin_protocol_failure(SerialChildCauseV1::ProtocolViolation);
            return;
        }
        self.current_frame_written = next_frame_written;
        self.stdout_accepted = next_stdout;
        self.stderr_accepted = next_stderr;

        if observation
            .exit
            .is_some_and(|exit| exit != ChildExitTruthV1::Exited(0))
        {
            self.begin_protocol_failure(SerialChildCauseV1::Crash);
            return;
        }
        if observation.stdout_eof || observation.stderr_eof {
            self.begin_protocol_failure(SerialChildCauseV1::Eof);
            return;
        }
        if observation.exit.is_some() {
            self.begin_protocol_failure(SerialChildCauseV1::Crash);
            return;
        }
        if observation.extra_stdout_bytes != 0 {
            self.begin_protocol_failure(SerialChildCauseV1::ProtocolViolation);
            return;
        }
        if let Some(response) = observation.response {
            if self.current_frame_written != self.current_frame_bytes {
                self.begin_protocol_failure(SerialChildCauseV1::ProtocolViolation);
                return;
            }
            if response.is_empty()
                || response.len() > self.policy.frame_bytes
                || !response.ends_with(b"\n")
                || response[..response.len() - 1].contains(&b'\n')
            {
                self.begin_protocol_failure(SerialChildCauseV1::ProtocolViolation);
                return;
            }
            let Some(completed_frames) = completed_frames.checked_add(1) else {
                self.begin_protocol_failure(SerialChildCauseV1::ProtocolViolation);
                return;
            };
            if completed_frames > self.policy.max_frames {
                self.begin_protocol_failure(SerialChildCauseV1::ProtocolViolation);
                return;
            }
            self.response = Some(response);
            self.state = SerialChildTransportStateV1::ResponseReady { completed_frames };
            return;
        }
        if observation.made_progress() {
            let Some(progress_now) = self.now_or_fail_closed() else {
                return;
            };
            let Some(idle_deadline_millis) =
                self.deadline_or_fail_closed(progress_now, self.policy.idle_millis)
            else {
                return;
            };
            self.state = SerialChildTransportStateV1::FrameInFlight {
                completed_frames,
                line_deadline_millis,
                idle_deadline_millis,
            };
        } else {
            self.state = SerialChildTransportStateV1::FrameInFlight {
                completed_frames,
                line_deadline_millis,
                idle_deadline_millis,
            };
        }
    }

    pub(crate) fn cancel(
        &mut self,
    ) -> Result<&SerialChildTransportStateV1, SerialChildTransportErrorV1> {
        self.request_drain(SerialChildCauseV1::Cancel)
    }

    pub(crate) fn shutdown(
        &mut self,
    ) -> Result<&SerialChildTransportStateV1, SerialChildTransportErrorV1> {
        self.request_drain(SerialChildCauseV1::Shutdown)
    }

    fn request_drain(
        &mut self,
        cause: SerialChildCauseV1,
    ) -> Result<&SerialChildTransportStateV1, SerialChildTransportErrorV1> {
        match self.state {
            SerialChildTransportStateV1::Ready => {
                self.start_attempted = true;
                if let Some(prepared) = self.prepared.take() {
                    self.effects.retain_unstarted_launch(prepared);
                }
                self.state = SerialChildTransportStateV1::StoppedBeforeSpawn(cause);
            }
            SerialChildTransportStateV1::Starting { .. }
            | SerialChildTransportStateV1::Idle { .. }
            | SerialChildTransportStateV1::FrameInFlight { .. }
            | SerialChildTransportStateV1::ResponseReady { .. }
            | SerialChildTransportStateV1::Exhausted { .. } => {
                let Some(now) = self.now_or_fail_closed() else {
                    return Ok(&self.state);
                };
                self.response.take();
                self.begin_drain_at(cause, now);
            }
            SerialChildTransportStateV1::Draining { .. }
            | SerialChildTransportStateV1::Killing { .. }
            | SerialChildTransportStateV1::Reaping { .. }
            | SerialChildTransportStateV1::StoppedBeforeSpawn(_)
            | SerialChildTransportStateV1::Stopped { .. }
            | SerialChildTransportStateV1::Failed(_) => {}
        }
        Ok(&self.state)
    }

    fn begin_protocol_failure(&mut self, cause: SerialChildCauseV1) {
        let Some(now) = self.now_or_fail_closed() else {
            return;
        };
        self.response.take();
        self.begin_drain_at(cause, now);
    }

    fn begin_drain_at(&mut self, cause: SerialChildCauseV1, now: u64) {
        let Some(deadline_millis) = self.deadline_or_fail_closed(now, self.policy.drain_millis)
        else {
            return;
        };
        let result = self.effects.begin_drain(
            self.child
                .as_ref()
                .expect("a live serial transport owns one child"),
            cause,
        );
        if let Err(detail) = result {
            self.fail_effect(SerialChildOperationV1::BeginDrain, detail);
            return;
        }
        self.state = SerialChildTransportStateV1::Draining {
            cause,
            deadline_millis,
        };
    }

    fn begin_kill(&mut self, cause: SerialChildCauseV1) {
        self.termination_requested = true;
        let result = self.effects.terminate_process_group(
            self.child
                .as_ref()
                .expect("a draining serial transport owns one child"),
        );
        if let Err(detail) = result {
            self.fail_effect(SerialChildOperationV1::TerminateProcessGroup, detail);
            return;
        }
        let Some(now) = self.now_or_fail_closed() else {
            return;
        };
        let Some(deadline_millis) = self.deadline_or_fail_closed(now, self.policy.kill_millis)
        else {
            return;
        };
        self.state = SerialChildTransportStateV1::Killing {
            cause,
            deadline_millis,
        };
    }

    fn begin_reap(&mut self, cause: SerialChildCauseV1) {
        let Some(now) = self.now_or_fail_closed() else {
            return;
        };
        let Some(deadline_millis) = self.deadline_or_fail_closed(now, self.policy.reap_millis)
        else {
            return;
        };
        self.state = SerialChildTransportStateV1::Reaping {
            cause,
            deadline_millis,
        };
    }

    fn poll_reap(&mut self, cause: SerialChildCauseV1) {
        let child = self
            .child
            .take()
            .expect("a reaping serial transport owns one child");
        match self.effects.poll_reap(child) {
            ChildReapPollV1::Pending(child) => self.child = Some(child),
            ChildReapPollV1::Reaped(exit) => {
                self.state = match cause {
                    SerialChildCauseV1::Cancel | SerialChildCauseV1::Shutdown => {
                        SerialChildTransportStateV1::Stopped { cause, exit }
                    }
                    _ => SerialChildTransportStateV1::Failed(
                        SerialChildTransportFailureV1::Transport { cause, exit },
                    ),
                }
            }
            ChildReapPollV1::Failed { child, detail } => {
                self.child = Some(child);
                self.fail_effect(SerialChildOperationV1::Reap, detail);
            }
        }
    }

    fn fail_reap_deadline(&mut self, cause: SerialChildCauseV1) {
        let child = self
            .child
            .take()
            .expect("a reaping serial transport owns one child");
        self.effects
            .retain_owned_residue(SerialOwnedChildResidueV1 {
                child,
                reason: SerialOwnedResidueReasonV1::ReapDeadline { cause },
                detail: None,
            });
        self.state =
            SerialChildTransportStateV1::Failed(SerialChildTransportFailureV1::ReapDeadline {
                cause,
            });
    }

    fn observe_now(&mut self) -> Result<u64, SerialClockFaultV1> {
        let observed = self
            .effects
            .monotonic_millis()
            .map_err(SerialClockFaultV1::Read)?;
        if let Some(previous_millis) = self.last_now_millis {
            if observed < previous_millis {
                return Err(SerialClockFaultV1::Regression {
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
            Err(SerialClockFaultV1::Read(detail)) => {
                self.fail_closed(
                    SerialChildTransportFailureV1::ClockRead(detail.clone()),
                    SerialOwnedResidueReasonV1::ClockRead,
                    Some(detail),
                );
                None
            }
            Err(SerialClockFaultV1::Regression {
                previous_millis,
                observed_millis,
            }) => {
                self.fail_closed(
                    SerialChildTransportFailureV1::ClockRegression {
                        previous_millis,
                        observed_millis,
                    },
                    SerialOwnedResidueReasonV1::ClockRegression {
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
                    SerialChildTransportFailureV1::ClockOverflow,
                    SerialOwnedResidueReasonV1::ClockOverflow,
                    None,
                );
                None
            }
        }
    }

    fn fail_effect(&mut self, operation: SerialChildOperationV1, detail: String) {
        self.fail_closed(
            SerialChildTransportFailureV1::Effect {
                operation,
                detail: detail.clone(),
            },
            SerialOwnedResidueReasonV1::Effect(operation),
            Some(detail),
        );
    }

    fn fail_closed(
        &mut self,
        failure: SerialChildTransportFailureV1,
        reason: SerialOwnedResidueReasonV1,
        mut detail: Option<String>,
    ) {
        self.response.take();
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
            self.effects
                .retain_owned_residue(SerialOwnedChildResidueV1 {
                    child,
                    reason,
                    detail,
                });
        }
        self.state = SerialChildTransportStateV1::Failed(failure);
    }

    #[cfg(test)]
    pub(super) fn effects_for_test(&mut self) -> &mut E {
        self.effects
    }

    #[cfg(test)]
    fn policy_for_test(&self) -> SerialTransportPolicyV1 {
        self.policy
    }
}

impl<E: SerialChildTransportEffectsV1> Drop for SerialChildTransportV1<'_, E> {
    fn drop(&mut self) {
        self.response.take();
        if let Some(prepared) = self.prepared.take() {
            self.effects.retain_unstarted_launch(prepared);
        }
        let Some(child) = self.child.take() else {
            return;
        };
        let reason = match self.state {
            SerialChildTransportStateV1::Draining { cause, .. }
            | SerialChildTransportStateV1::Killing { cause, .. }
            | SerialChildTransportStateV1::Reaping { cause, .. } => {
                SerialOwnedResidueReasonV1::Transport(cause)
            }
            _ => SerialOwnedResidueReasonV1::TransportDropped,
        };
        let mut detail = None;
        if !self.termination_requested {
            self.termination_requested = true;
            detail = self.effects.terminate_process_group(&child).err();
        }
        self.effects
            .retain_owned_residue(SerialOwnedChildResidueV1 {
                child,
                reason,
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
        ChildEnvironmentPolicyV1, ChildFilesystemAccessV1, ChildFilesystemGrantV1,
        ChildFilesystemPolicyV1, ChildIoLimitV1, ChildIoLimitsV1, ChildLaunchAuthorityDecisionV1,
        ChildLaunchModeV1, ChildLaunchPreparationV1, ChildLaunchRequestV1, ChildNetworkPolicyV1,
        ChildPhaseBudgetsV1, ChildProcessGroupPolicyV1, ChildRequestPolicyV1, ChildRuntimePolicyV1,
        ChildTimeoutDispositionV1, PreparedChildContainmentTokenV1, PreparedChildLaunchV1,
    };
    use crate::external::lifecycle::ChildExitTruthV1;
    use crate::external::supervisor::{ChildReapPollV1, OwnedChildTokenV1};

    const EXECUTABLE: &str = "/opt/kitsu-fixtures/serial-peer";
    const WORKING_DIRECTORY: &str = "/tmp/kitsu-external";
    const DIGEST: &str = "abababababababababababababababababababababababababababababababab";
    const STARTUP_MILLIS: u64 = 5;
    const LINE_MILLIS: u64 = 17;
    const IDLE_MILLIS: u64 = 7;
    const DRAIN_MILLIS: u64 = 9;
    const KILL_MILLIS: u64 = 11;
    const REAP_MILLIS: u64 = 13;
    const MAX_FRAMES: u16 = 3;
    const STDIN_BYTES: usize = 16;
    const STDOUT_BYTES: usize = 24;
    const STDERR_BYTES: usize = 12;
    const FRAME_BYTES: usize = 8;

    fn runtime_policy() -> ChildRuntimePolicyV1 {
        ChildRuntimePolicyV1::new(
            ChildPhaseBudgetsV1::new(
                STARTUP_MILLIS,
                LINE_MILLIS,
                DRAIN_MILLIS,
                KILL_MILLIS,
                REAP_MILLIS,
            ),
            ChildIoLimitsV1::new(STDIN_BYTES, STDOUT_BYTES, STDERR_BYTES, FRAME_BYTES),
            ChildProcessGroupPolicyV1::Owned,
            ChildEnvironmentPolicyV1::new(true, Vec::new()),
            ChildFilesystemPolicyV1::new(vec![ChildFilesystemGrantV1::new(
                WORKING_DIRECTORY,
                ChildFilesystemAccessV1::ReadWrite,
            )]),
            ChildNetworkPolicyV1::Deny,
            ChildTimeoutDispositionV1::TerminateProcessGroupAndReap,
        )
        .with_request_policy(ChildRequestPolicyV1::serial(MAX_FRAMES, IDLE_MILLIS))
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
            Ok(PreparedChildContainmentTokenV1::new(41))
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
        let authority = AuthorityProfileV1::exact(
            EXECUTABLE,
            DIGEST,
            vec!["--stdio".to_string()],
            WORKING_DIRECTORY,
            runtime_policy(),
        )
        .unwrap();
        prepare_child_launch_request_v1(
            request,
            ChildLaunchAuthorityDecisionV1::Admit(&authority),
            &mut PreparationFixture,
        )
        .unwrap()
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Event {
        Now,
        Spawn,
        StartupReady,
        BeginFrame(Vec<u8>),
        PollFrame(SerialPollAllowanceV1),
        BeginDrain(SerialChildCauseV1),
        DrainComplete,
        TerminateProcessGroup,
        ProcessGroupTerminated,
        Reap,
        RetainPrepared,
        RetainResidue(SerialOwnedResidueReasonV1),
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum ReapScript {
        Pending,
        Reaped(ChildExitTruthV1),
        Failed,
    }

    struct SerialFixture {
        events: Vec<Event>,
        times: VecDeque<Result<u64, String>>,
        startup_ready: VecDeque<Result<bool, String>>,
        begin_frame_error: Option<String>,
        frame_polls: VecDeque<Result<SerialFrameObservationV1, String>>,
        begin_drain_error: Option<String>,
        drain_complete: VecDeque<Result<bool, String>>,
        terminate_error: Option<String>,
        process_group_terminated: VecDeque<Result<bool, String>>,
        reap: VecDeque<ReapScript>,
        retained_prepared: Vec<PreparedChildLaunchV1>,
        retained_residue: Vec<SerialOwnedChildResidueV1>,
        spawn_count: usize,
    }

    impl SerialFixture {
        fn with_times(times: impl IntoIterator<Item = u64>) -> Self {
            Self {
                events: Vec::new(),
                times: times.into_iter().map(Ok).collect(),
                startup_ready: VecDeque::new(),
                begin_frame_error: None,
                frame_polls: VecDeque::new(),
                begin_drain_error: None,
                drain_complete: VecDeque::new(),
                terminate_error: None,
                process_group_terminated: VecDeque::new(),
                reap: VecDeque::new(),
                retained_prepared: Vec::new(),
                retained_residue: Vec::new(),
                spawn_count: 0,
            }
        }

        fn next_bool(queue: &mut VecDeque<Result<bool, String>>) -> Result<bool, String> {
            queue.pop_front().unwrap_or(Ok(false))
        }
    }

    impl SerialChildTransportEffectsV1 for SerialFixture {
        fn monotonic_millis(&mut self) -> Result<u64, String> {
            self.events.push(Event::Now);
            self.times
                .pop_front()
                .expect("fixture monotonic time is configured")
        }

        fn spawn_serial(&mut self, _: PreparedChildLaunchV1) -> Result<OwnedChildTokenV1, String> {
            self.events.push(Event::Spawn);
            self.spawn_count += 1;
            Ok(OwnedChildTokenV1::new(73))
        }

        fn startup_ready(&mut self, _: &OwnedChildTokenV1) -> Result<bool, String> {
            self.events.push(Event::StartupReady);
            Self::next_bool(&mut self.startup_ready)
        }

        fn begin_frame(&mut self, _: &OwnedChildTokenV1, frame: &[u8]) -> Result<(), String> {
            self.events.push(Event::BeginFrame(frame.to_vec()));
            self.begin_frame_error.take().map_or(Ok(()), Err)
        }

        fn poll_frame(
            &mut self,
            _: &OwnedChildTokenV1,
            allowance: SerialPollAllowanceV1,
        ) -> Result<SerialFrameObservationV1, String> {
            self.events.push(Event::PollFrame(allowance));
            self.frame_polls
                .pop_front()
                .unwrap_or_else(|| Ok(SerialFrameObservationV1::pending()))
        }

        fn begin_drain(
            &mut self,
            _: &OwnedChildTokenV1,
            cause: SerialChildCauseV1,
        ) -> Result<(), String> {
            self.events.push(Event::BeginDrain(cause));
            self.begin_drain_error.take().map_or(Ok(()), Err)
        }

        fn drain_complete(&mut self, _: &OwnedChildTokenV1) -> Result<bool, String> {
            self.events.push(Event::DrainComplete);
            Self::next_bool(&mut self.drain_complete)
        }

        fn terminate_process_group(&mut self, _: &OwnedChildTokenV1) -> Result<(), String> {
            self.events.push(Event::TerminateProcessGroup);
            self.terminate_error.take().map_or(Ok(()), Err)
        }

        fn process_group_terminated(&mut self, _: &OwnedChildTokenV1) -> Result<bool, String> {
            self.events.push(Event::ProcessGroupTerminated);
            Self::next_bool(&mut self.process_group_terminated)
        }

        fn poll_reap(&mut self, child: OwnedChildTokenV1) -> ChildReapPollV1 {
            self.events.push(Event::Reap);
            match self.reap.pop_front().unwrap_or(ReapScript::Pending) {
                ReapScript::Pending => ChildReapPollV1::Pending(child),
                ReapScript::Reaped(exit) => ChildReapPollV1::Reaped(exit),
                ReapScript::Failed => ChildReapPollV1::Failed {
                    child,
                    detail: "reap fault".to_string(),
                },
            }
        }

        fn retain_unstarted_launch(&mut self, launch: PreparedChildLaunchV1) {
            self.events.push(Event::RetainPrepared);
            self.retained_prepared.push(launch);
        }

        fn retain_owned_residue(&mut self, residue: SerialOwnedChildResidueV1) {
            self.events
                .push(Event::RetainResidue(residue.reason_for_test()));
            self.retained_residue.push(residue);
        }
    }

    fn start_ready<'a>(
        effects: &'a mut SerialFixture,
    ) -> SerialChildTransportV1<'a, SerialFixture> {
        effects.startup_ready.push_back(Ok(true));
        let mut transport = SerialChildTransportV1::new(prepared_launch(), effects);
        assert!(matches!(
            transport.start().unwrap(),
            SerialChildTransportStateV1::Starting { .. }
        ));
        assert_eq!(
            transport.poll().unwrap(),
            &SerialChildTransportStateV1::Idle {
                completed_frames: 0,
            }
        );
        transport
    }

    fn complete_cleanup(transport: &mut SerialChildTransportV1<'_, SerialFixture>) {
        transport
            .effects_for_test()
            .drain_complete
            .push_back(Ok(true));
        transport
            .effects_for_test()
            .process_group_terminated
            .push_back(Ok(true));
        transport
            .effects_for_test()
            .reap
            .push_back(ReapScript::Reaped(ChildExitTruthV1::Signalled));
        assert!(matches!(
            transport.poll().unwrap(),
            SerialChildTransportStateV1::Killing { .. }
        ));
        assert!(matches!(
            transport.poll().unwrap(),
            SerialChildTransportStateV1::Reaping { .. }
        ));
        assert!(matches!(
            transport.poll().unwrap(),
            SerialChildTransportStateV1::Stopped { .. } | SerialChildTransportStateV1::Failed(_)
        ));
    }

    #[test]
    fn one_owner_processes_multiple_frames_strictly_serial_and_consumes_responses_once() {
        let mut effects = SerialFixture::with_times([0, 1, 2, 3, 4, 5, 6, 7]);
        let mut transport = start_ready(&mut effects);

        transport.begin_frame(b"one\n".to_vec()).unwrap();
        assert_eq!(
            transport.begin_frame(b"overlap\n".to_vec()),
            Err(SerialChildTransportErrorV1::FrameAlreadyInFlight)
        );
        transport
            .effects_for_test()
            .frame_polls
            .push_back(Ok(SerialFrameObservationV1::response(
                b"ONE\n".to_vec(),
                4,
                4,
                0,
            )));
        assert_eq!(
            transport.poll().unwrap(),
            &SerialChildTransportStateV1::ResponseReady {
                completed_frames: 1,
            }
        );
        assert_eq!(
            transport.begin_frame(b"overlap\n".to_vec()),
            Err(SerialChildTransportErrorV1::ResponseNotConsumed)
        );
        assert_eq!(transport.take_response().unwrap(), b"ONE\n");
        assert_eq!(
            transport.take_response(),
            Err(SerialChildTransportErrorV1::NoResponseReady)
        );

        transport.begin_frame(b"two\n".to_vec()).unwrap();
        transport
            .effects_for_test()
            .frame_polls
            .push_back(Ok(SerialFrameObservationV1::response(
                b"TWO\n".to_vec(),
                4,
                4,
                0,
            )));
        transport.poll().unwrap();
        assert_eq!(transport.take_response().unwrap(), b"TWO\n");
        assert_eq!(transport.effects_for_test().spawn_count, 1);
        assert_eq!(
            transport
                .effects_for_test()
                .events
                .iter()
                .filter(|event| matches!(event, Event::Spawn))
                .count(),
            1
        );
    }

    #[test]
    fn request_before_ready_overlap_and_frame_limit_are_rejected_before_effects() {
        let mut effects = SerialFixture::with_times([0, 1, 2, 3, 4, 5, 6, 7]);
        let mut transport = SerialChildTransportV1::new(prepared_launch(), &mut effects);
        assert_eq!(
            transport.begin_frame(b"early\n".to_vec()),
            Err(SerialChildTransportErrorV1::ChildNotReady)
        );
        transport.start().unwrap();
        assert_eq!(
            transport.begin_frame(b"starting\n".to_vec()),
            Err(SerialChildTransportErrorV1::ChildNotReady)
        );
        transport
            .effects_for_test()
            .startup_ready
            .push_back(Ok(true));
        transport.poll().unwrap();

        for number in 0..MAX_FRAMES {
            let frame = format!("{number}\n").into_bytes();
            transport.begin_frame(frame.clone()).unwrap();
            assert_eq!(
                transport.begin_frame(frame),
                Err(SerialChildTransportErrorV1::FrameAlreadyInFlight)
            );
            transport.effects_for_test().frame_polls.push_back(Ok(
                SerialFrameObservationV1::response(format!("{number}\n").into_bytes(), 2, 2, 0),
            ));
            transport.poll().unwrap();
            transport.take_response().unwrap();
        }
        let begin_count = transport
            .effects_for_test()
            .events
            .iter()
            .filter(|event| matches!(event, Event::BeginFrame(_)))
            .count();
        assert_eq!(
            transport.begin_frame(b"too-many\n".to_vec()),
            Err(SerialChildTransportErrorV1::FrameLimitReached)
        );
        assert_eq!(
            transport
                .effects_for_test()
                .events
                .iter()
                .filter(|event| matches!(event, Event::BeginFrame(_)))
                .count(),
            begin_count
        );
    }

    #[test]
    fn line_deadline_equality_expires_before_poll_effect() {
        let mut effects = SerialFixture::with_times([0, 1, 2, 2 + LINE_MILLIS]);
        let mut transport = start_ready(&mut effects);
        transport.begin_frame(b"line\n".to_vec()).unwrap();
        assert!(matches!(
            transport.poll().unwrap(),
            SerialChildTransportStateV1::Draining {
                cause: SerialChildCauseV1::LineTimeout,
                ..
            }
        ));
        assert!(!transport
            .effects_for_test()
            .events
            .iter()
            .any(|event| matches!(event, Event::PollFrame(_))));
    }

    #[test]
    fn idle_deadline_resets_only_after_nonzero_progress_using_fresh_injected_time() {
        let mut effects = SerialFixture::with_times([
            0,
            1,
            2,
            2 + IDLE_MILLIS - 1,
            2 + IDLE_MILLIS - 1,
            2 + IDLE_MILLIS + 2,
            2 + IDLE_MILLIS + 3,
        ]);
        let mut transport = start_ready(&mut effects);
        transport.begin_frame(b"line\n".to_vec()).unwrap();
        transport.effects_for_test().frame_polls.extend([
            Ok(SerialFrameObservationV1::pending()),
            Ok(SerialFrameObservationV1::progress(1, 0, 0)),
            Ok(SerialFrameObservationV1::pending()),
        ]);

        transport.poll().unwrap();
        let now_after_zero_progress = transport
            .effects_for_test()
            .events
            .iter()
            .filter(|event| matches!(event, Event::Now))
            .count();
        transport.poll().unwrap();
        assert_eq!(
            transport
                .effects_for_test()
                .events
                .iter()
                .filter(|event| matches!(event, Event::Now))
                .count(),
            now_after_zero_progress + 2,
            "nonzero progress must sample a fresh time for the new idle deadline"
        );
        assert!(matches!(
            transport.state(),
            SerialChildTransportStateV1::FrameInFlight {
                idle_deadline_millis,
                ..
            } if *idle_deadline_millis == 2 + IDLE_MILLIS + 2 + IDLE_MILLIS
        ));
        transport.poll().unwrap();
        assert_eq!(
            transport
                .effects_for_test()
                .events
                .iter()
                .filter(|event| matches!(event, Event::Now))
                .count(),
            now_after_zero_progress + 3,
            "zero-byte EAGAIN/poll activity must not refresh idle"
        );
    }

    #[test]
    fn invalid_or_cumulative_overcap_input_is_rejected_before_write_effect() {
        let mut effects = SerialFixture::with_times([0, 1, 2, 3, 4, 5]);
        let mut transport = start_ready(&mut effects);
        let mut oversized = vec![b'x'; FRAME_BYTES + 1];
        *oversized.last_mut().unwrap() = b'\n';
        for (frame, expected) in [
            (
                b"missing".to_vec(),
                SerialChildTransportErrorV1::InvalidFrame,
            ),
            (
                b"a\nb\n".to_vec(),
                SerialChildTransportErrorV1::InvalidFrame,
            ),
            (oversized, SerialChildTransportErrorV1::FrameTooLarge),
        ] {
            let before = transport.effects_for_test().events.len();
            assert_eq!(transport.begin_frame(frame), Err(expected));
            assert_eq!(transport.effects_for_test().events.len(), before);
        }

        for frame in [b"1234567\n".to_vec(), b"7654321\n".to_vec()] {
            transport.begin_frame(frame).unwrap();
            transport.effects_for_test().frame_polls.push_back(Ok(
                SerialFrameObservationV1::response(b"ok\n".to_vec(), 8, 3, 0),
            ));
            transport.poll().unwrap();
            transport.take_response().unwrap();
        }
        let before = transport.effects_for_test().events.len();
        assert_eq!(
            transport.begin_frame(b"x\n".to_vec()),
            Err(SerialChildTransportErrorV1::CumulativeLimit(
                ChildIoLimitV1::Stdin
            ))
        );
        assert_eq!(transport.effects_for_test().events.len(), before);
    }

    #[test]
    fn poll_allowance_is_exact_and_overcap_or_extra_carry_never_becomes_response_ready() {
        let mut effects = SerialFixture::with_times(0..=7);
        let mut transport = start_ready(&mut effects);
        transport.begin_frame(b"one\n".to_vec()).unwrap();
        transport
            .effects_for_test()
            .frame_polls
            .push_back(Ok(SerialFrameObservationV1::progress(4, 5, 2)));
        transport.poll().unwrap();
        let allowance = transport
            .effects_for_test()
            .events
            .iter()
            .find_map(|event| match event {
                Event::PollFrame(allowance) => Some(*allowance),
                _ => None,
            })
            .unwrap();
        assert_eq!(allowance.stdin_bytes, STDIN_BYTES - b"one\n".len());
        assert_eq!(allowance.stdout_bytes, STDOUT_BYTES);
        assert_eq!(allowance.stderr_bytes, STDERR_BYTES);
        assert_eq!(allowance.frame_bytes, FRAME_BYTES);

        transport.effects_for_test().frame_polls.push_back(Ok(
            SerialFrameObservationV1::response_with_extra_carry(b"ok\n".to_vec(), 0, 3, 0, 1),
        ));
        assert!(matches!(
            transport.poll().unwrap(),
            SerialChildTransportStateV1::Draining {
                cause: SerialChildCauseV1::ProtocolViolation,
                ..
            }
        ));
        assert!(matches!(
            transport.take_response(),
            Err(SerialChildTransportErrorV1::NoResponseReady)
        ));
    }

    #[test]
    fn response_ready_requires_running_child_and_no_eof_crash_or_carry_in_same_poll() {
        for observation in [
            SerialFrameObservationV1::response_with_eof(b"not-success\n".to_vec(), 0, 12, 0),
            SerialFrameObservationV1::response_with_exit(
                b"not-success\n".to_vec(),
                0,
                12,
                0,
                ChildExitTruthV1::Exited(0),
            ),
            SerialFrameObservationV1::response_with_extra_carry(
                b"not-success\n".to_vec(),
                0,
                12,
                0,
                1,
            ),
        ] {
            let mut effects = SerialFixture::with_times(0..=5);
            let mut transport = start_ready(&mut effects);
            transport.begin_frame(b"line\n".to_vec()).unwrap();
            transport
                .effects_for_test()
                .frame_polls
                .push_back(Ok(observation));
            assert!(matches!(
                transport.poll().unwrap(),
                SerialChildTransportStateV1::Draining { .. }
            ));
            assert!(!matches!(
                transport.state(),
                SerialChildTransportStateV1::ResponseReady { .. }
            ));
        }
    }

    #[test]
    fn eof_crash_and_effect_fault_terminate_then_transfer_typed_residue_never_success() {
        let cases = [
            (
                Ok(SerialFrameObservationV1::eof()),
                SerialOwnedResidueReasonV1::Transport(SerialChildCauseV1::Eof),
            ),
            (
                Ok(SerialFrameObservationV1::exit(ChildExitTruthV1::Exited(17))),
                SerialOwnedResidueReasonV1::Transport(SerialChildCauseV1::Crash),
            ),
            (
                Err("poll fault".to_string()),
                SerialOwnedResidueReasonV1::Effect(SerialChildOperationV1::PollFrame),
            ),
        ];
        for (poll, expected_reason) in cases {
            let mut effects = SerialFixture::with_times(0..=5);
            {
                let mut transport = start_ready(&mut effects);
                transport.begin_frame(b"line\n".to_vec()).unwrap();
                transport.effects_for_test().frame_polls.push_back(poll);
                transport.poll().unwrap();
                assert!(!matches!(
                    transport.state(),
                    SerialChildTransportStateV1::ResponseReady { .. }
                        | SerialChildTransportStateV1::Stopped { .. }
                ));
            }
            let terminate = effects
                .events
                .iter()
                .position(|event| *event == Event::TerminateProcessGroup)
                .expect("post-spawn failure attempts group termination");
            let retain = effects
                .events
                .iter()
                .position(|event| matches!(event, Event::RetainResidue(_)))
                .expect("post-spawn failure transfers typed residue");
            assert!(terminate < retain);
            assert_eq!(effects.retained_residue.len(), 1);
            assert_eq!(
                effects.retained_residue[0].reason_for_test(),
                expected_reason
            );
        }
    }

    #[test]
    fn cancel_and_shutdown_are_idempotent_and_share_descendant_reap_path() {
        for cause in [SerialChildCauseV1::Cancel, SerialChildCauseV1::Shutdown] {
            let mut effects = SerialFixture::with_times(0..=9);
            let mut transport = start_ready(&mut effects);
            transport.begin_frame(b"line\n".to_vec()).unwrap();
            match cause {
                SerialChildCauseV1::Cancel => {
                    transport.cancel().unwrap();
                    transport.cancel().unwrap();
                }
                SerialChildCauseV1::Shutdown => {
                    transport.shutdown().unwrap();
                    transport.shutdown().unwrap();
                }
                _ => unreachable!(),
            }
            assert_eq!(
                transport
                    .effects_for_test()
                    .events
                    .iter()
                    .filter(|event| matches!(event, Event::BeginDrain(_)))
                    .count(),
                1
            );
            complete_cleanup(&mut transport);
            assert_eq!(
                transport
                    .effects_for_test()
                    .events
                    .iter()
                    .filter(|event| matches!(event, Event::TerminateProcessGroup))
                    .count(),
                1
            );
        }
    }

    #[test]
    fn clock_regression_and_deadline_overflow_fail_closed_with_one_owned_residue() {
        for times in [vec![10, 11, 10], vec![u64::MAX - STARTUP_MILLIS + 1]] {
            let mut effects = SerialFixture::with_times(times);
            {
                let mut transport = SerialChildTransportV1::new(prepared_launch(), &mut effects);
                transport.start().unwrap();
                if matches!(
                    transport.state(),
                    SerialChildTransportStateV1::Starting { .. }
                ) {
                    transport.poll().unwrap();
                }
                if matches!(
                    transport.state(),
                    SerialChildTransportStateV1::Starting { .. }
                ) {
                    transport.poll().unwrap();
                }
                assert!(matches!(
                    transport.state(),
                    SerialChildTransportStateV1::Failed(
                        SerialChildTransportFailureV1::ClockRegression { .. }
                            | SerialChildTransportFailureV1::ClockOverflow
                    )
                ));
            }
            assert!(effects.retained_prepared.len() + effects.retained_residue.len() == 1);
        }
    }

    #[test]
    fn reap_deadline_transfers_one_retryable_typed_residue() {
        let mut effects = SerialFixture::with_times([0, 1, 2, 3, 4, 5, 6, 6 + REAP_MILLIS]);
        let mut transport = start_ready(&mut effects);
        transport.shutdown().unwrap();
        transport
            .effects_for_test()
            .drain_complete
            .push_back(Ok(true));
        transport
            .effects_for_test()
            .process_group_terminated
            .push_back(Ok(true));
        transport.poll().unwrap();
        transport.poll().unwrap();
        assert_eq!(
            transport.poll().unwrap(),
            &SerialChildTransportStateV1::Failed(SerialChildTransportFailureV1::ReapDeadline {
                cause: SerialChildCauseV1::Shutdown,
            })
        );
        assert_eq!(transport.effects_for_test().retained_residue.len(), 1);
        assert_eq!(
            transport.effects_for_test().retained_residue[0].reason_for_test(),
            SerialOwnedResidueReasonV1::ReapDeadline {
                cause: SerialChildCauseV1::Shutdown,
            }
        );
    }

    #[test]
    fn drop_before_spawn_retains_prepared_and_drop_after_spawn_terminates_then_retains_child() {
        let mut before = SerialFixture::with_times([]);
        drop(SerialChildTransportV1::new(prepared_launch(), &mut before));
        assert_eq!(before.retained_prepared.len(), 1);
        assert!(before.retained_residue.is_empty());
        assert_eq!(before.events, [Event::RetainPrepared]);

        let mut after = SerialFixture::with_times([0]);
        {
            let mut transport = SerialChildTransportV1::new(prepared_launch(), &mut after);
            transport.start().unwrap();
        }
        assert_eq!(after.retained_residue.len(), 1);
        assert_eq!(
            after.events[after.events.len() - 2..],
            [
                Event::TerminateProcessGroup,
                Event::RetainResidue(SerialOwnedResidueReasonV1::TransportDropped),
            ]
        );
    }

    #[test]
    fn exact_prepared_policy_values_are_consumed_for_every_deadline_and_cap() {
        let mut effects = SerialFixture::with_times([100, 101, 102]);
        effects.startup_ready.push_back(Ok(true));
        let mut transport = SerialChildTransportV1::new(prepared_launch(), &mut effects);
        assert_eq!(
            transport.start().unwrap(),
            &SerialChildTransportStateV1::Starting {
                deadline_millis: 100 + STARTUP_MILLIS,
            }
        );
        assert_eq!(
            transport.poll().unwrap(),
            &SerialChildTransportStateV1::Idle {
                completed_frames: 0,
            }
        );
        assert_eq!(
            transport.policy_for_test(),
            SerialTransportPolicyV1 {
                max_frames: MAX_FRAMES,
                line_millis: LINE_MILLIS,
                idle_millis: IDLE_MILLIS,
                stdin_bytes: STDIN_BYTES,
                stdout_bytes: STDOUT_BYTES,
                stderr_bytes: STDERR_BYTES,
                frame_bytes: FRAME_BYTES,
                drain_millis: DRAIN_MILLIS,
                kill_millis: KILL_MILLIS,
                reap_millis: REAP_MILLIS,
            }
        );
        transport.begin_frame(b"line\n".to_vec()).unwrap();
        assert_eq!(
            transport.state(),
            &SerialChildTransportStateV1::FrameInFlight {
                completed_frames: 0,
                line_deadline_millis: 102 + LINE_MILLIS,
                idle_deadline_millis: 102 + IDLE_MILLIS,
            }
        );
    }
}
