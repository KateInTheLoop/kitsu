//! Private Linux process backend for an authorized external child.
//!
//! This backend does not provide filesystem containment and must remain
//! unmounted until a later authority card binds reviewed filesystem roots.
//! It supports one process/request only; serial long-lived MCP cycling is not
//! part of this contract.
//!
//! The digest in `PreparedChildLaunchV1` records admission-time observation;
//! it is not an immutable executable handle. This backend executes the exact
//! authorized path, so replacement between preparation and exec remains a
//! disclosed TOCTOU risk. No product acceptance or general sandbox claim is
//! valid until later authority supplies filesystem policy and an immutable or
//! equivalently revalidated execution mechanism.

#![allow(dead_code)]

use std::fmt;
use std::io::{self, Read, Write};
use std::mem::MaybeUninit;
use std::os::unix::process::CommandExt;
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use super::authority::{PreparedChildLaunchV1, PreparedChildOsLaunchV1};
use super::lifecycle::ChildExitTruthV1;
use super::supervisor::{
    ChildPhaseSupervisorEffectsV1, ChildReapPollV1, ChildSupervisorCauseV1, OwnedChildResidueV1,
    OwnedChildTokenV1,
};

const CHILD_TOKEN_ID: u64 = 1;
const PIPE_CHUNK_BYTES: usize = 8 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ContainmentSetupV1 {
    Enforce,
    FailBeforeSpawn,
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TestContainmentSetupV1 {
    FailBeforeSpawn,
}

struct PreparedChildContainmentV1 {
    seccomp_filter: Box<[libc::sock_filter]>,
    seccomp_filter_len: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RealChildCompletionTruthV1 {
    Success,
    Crash(ChildExitTruthV1),
}

#[derive(Debug)]
struct PipeReadOutcomeV1 {
    bytes: Vec<u8>,
    eof: bool,
    overflow: bool,
    error: Option<String>,
}

#[derive(Debug)]
struct PipeWriteOutcomeV1 {
    error: Option<String>,
}

struct ActiveChildV1 {
    child: Child,
    pgid: i32,
    phase_budgets_millis: [u64; 5],
    stdin: Option<ChildStdin>,
    stdin_worker: Option<JoinHandle<PipeWriteOutcomeV1>>,
    stdin_outcome: Option<PipeWriteOutcomeV1>,
    stdout_worker: Option<JoinHandle<PipeReadOutcomeV1>>,
    stdout_outcome: Option<PipeReadOutcomeV1>,
    stderr_worker: Option<JoinHandle<PipeReadOutcomeV1>>,
    stderr_outcome: Option<PipeReadOutcomeV1>,
    exit_observed: bool,
    exit_status: Option<ExitStatus>,
}

impl ActiveChildV1 {
    fn observe_child_exit(&mut self) -> Result<(), String> {
        if !self.exit_observed {
            self.exit_observed = child_exit_is_observable(self.child.id())?;
        }
        Ok(())
    }

    fn refresh_workers(&mut self) {
        if self.stdin_outcome.is_none()
            && self
                .stdin_worker
                .as_ref()
                .is_some_and(JoinHandle::is_finished)
        {
            let worker = self.stdin_worker.take().expect("finished stdin worker");
            self.stdin_outcome = Some(worker.join().unwrap_or_else(|_| PipeWriteOutcomeV1 {
                error: Some("stdin worker panicked".to_string()),
            }));
        }
        if self.stdout_outcome.is_none()
            && self
                .stdout_worker
                .as_ref()
                .is_some_and(JoinHandle::is_finished)
        {
            let worker = self.stdout_worker.take().expect("finished stdout worker");
            self.stdout_outcome = Some(worker.join().unwrap_or_else(|_| PipeReadOutcomeV1 {
                bytes: Vec::new(),
                eof: false,
                overflow: false,
                error: Some("stdout worker panicked".to_string()),
            }));
        }
        if self.stderr_outcome.is_none()
            && self
                .stderr_worker
                .as_ref()
                .is_some_and(JoinHandle::is_finished)
        {
            let worker = self.stderr_worker.take().expect("finished stderr worker");
            self.stderr_outcome = Some(worker.join().unwrap_or_else(|_| PipeReadOutcomeV1 {
                bytes: Vec::new(),
                eof: false,
                overflow: false,
                error: Some("stderr worker panicked".to_string()),
            }));
        }
    }

    fn worker_fault(&self) -> Option<String> {
        if let Some(error) = self
            .stdin_outcome
            .as_ref()
            .and_then(|outcome| outcome.error.as_ref())
        {
            return Some(format!("stdin worker: {error}"));
        }
        for (name, outcome) in [
            ("stdout", self.stdout_outcome.as_ref()),
            ("stderr", self.stderr_outcome.as_ref()),
        ] {
            if let Some(error) = outcome.and_then(|outcome| outcome.error.as_ref()) {
                return Some(format!("{name} worker: {error}"));
            }
            if outcome.is_some_and(|outcome| outcome.overflow) {
                return Some(format!("{name} exceeded its authorized byte limit"));
            }
        }
        None
    }

    fn all_workers_joined(&self) -> bool {
        self.stdin_outcome.is_some()
            && self.stdout_outcome.is_some()
            && self.stderr_outcome.is_some()
    }

    fn force_cleanup(&mut self) -> RealChildCleanupTruthV1 {
        // An unreaped direct child reserves the numeric process-group ID, so
        // this signal cannot be redirected to a reused unrelated group. Once
        // direct wait has happened, cleanup only observes the prior group kill.
        if self.exit_status.is_none() {
            let _ = signal_process_group(self.pgid, libc::SIGKILL);
        }
        if self.exit_status.is_none() {
            self.exit_status = self.child.wait().ok();
        }

        let group_deadline = Instant::now()
            .checked_add(Duration::from_millis(self.phase_budgets_millis[4]))
            .unwrap_or_else(Instant::now);
        while !process_group_is_gone(self.pgid) && Instant::now() < group_deadline {
            thread::yield_now();
        }

        self.stdin.take();
        if self.stdin_worker.is_none() && self.stdin_outcome.is_none() {
            self.stdin_outcome = Some(PipeWriteOutcomeV1 { error: None });
        }
        join_write_worker(&mut self.stdin_worker, &mut self.stdin_outcome);
        join_read_worker(&mut self.stdout_worker, &mut self.stdout_outcome, "stdout");
        join_read_worker(&mut self.stderr_worker, &mut self.stderr_outcome, "stderr");
        RealChildCleanupTruthV1 {
            group_gone: process_group_is_gone(self.pgid),
            direct_child_reaped: self.exit_status.is_some(),
            workers_joined: self.all_workers_joined(),
        }
    }
}

fn join_write_worker(
    worker: &mut Option<JoinHandle<PipeWriteOutcomeV1>>,
    outcome: &mut Option<PipeWriteOutcomeV1>,
) {
    if outcome.is_none() {
        *outcome = worker.take().map_or(
            Some(PipeWriteOutcomeV1 {
                error: Some("stdin worker was not owned".to_string()),
            }),
            |worker| {
                Some(worker.join().unwrap_or_else(|_| PipeWriteOutcomeV1 {
                    error: Some("stdin worker panicked".to_string()),
                }))
            },
        );
    }
}

fn join_read_worker(
    worker: &mut Option<JoinHandle<PipeReadOutcomeV1>>,
    outcome: &mut Option<PipeReadOutcomeV1>,
    name: &str,
) {
    if outcome.is_none() {
        *outcome = worker.take().map_or(
            Some(PipeReadOutcomeV1 {
                bytes: Vec::new(),
                eof: false,
                overflow: false,
                error: Some(format!("{name} worker was not owned")),
            }),
            |worker| {
                Some(worker.join().unwrap_or_else(|_| PipeReadOutcomeV1 {
                    bytes: Vec::new(),
                    eof: false,
                    overflow: false,
                    error: Some(format!("{name} worker panicked")),
                }))
            },
        );
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RealChildCompletionV1 {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    stdout_eof: bool,
    stderr_eof: bool,
    workers_joined: bool,
    direct_child_reaped: bool,
    truth: RealChildCompletionTruthV1,
}

impl RealChildCompletionV1 {
    fn stdout(&self) -> &[u8] {
        &self.stdout
    }

    fn stderr(&self) -> &[u8] {
        &self.stderr
    }

    fn stdout_eof(&self) -> bool {
        self.stdout_eof
    }

    fn stderr_eof(&self) -> bool {
        self.stderr_eof
    }

    fn workers_joined(&self) -> bool {
        self.workers_joined
    }

    fn direct_child_reaped(&self) -> bool {
        self.direct_child_reaped
    }

    fn truth(&self) -> RealChildCompletionTruthV1 {
        self.truth.clone()
    }

    fn is_success(&self) -> bool {
        self.truth == RealChildCompletionTruthV1::Success
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RealChildCleanupTruthV1 {
    group_gone: bool,
    direct_child_reaped: bool,
    workers_joined: bool,
}

impl RealChildCleanupTruthV1 {
    fn group_gone(self) -> bool {
        self.group_gone
    }

    fn direct_child_reaped(self) -> bool {
        self.direct_child_reaped
    }

    fn workers_joined(self) -> bool {
        self.workers_joined
    }
}

struct RealChildOwnedResidueV1 {
    _supervisor_residue: OwnedChildResidueV1,
    child: ActiveChildV1,
    cleaned: bool,
}

impl fmt::Debug for RealChildOwnedResidueV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RealChildOwnedResidueV1")
            .field("pgid", &self.child.pgid)
            .field("cleaned", &self.cleaned)
            .finish_non_exhaustive()
    }
}

impl RealChildOwnedResidueV1 {
    fn cleanup(mut self) -> Result<RealChildCleanupTruthV1, Box<Self>> {
        let truth = self.child.force_cleanup();
        if truth.group_gone && truth.direct_child_reaped && truth.workers_joined {
            self.cleaned = true;
            Ok(truth)
        } else {
            // The non-cloneable residue, including its supervisor proof and
            // OS observation state, remains owned for a later cleanup retry.
            Err(Box::new(self))
        }
    }
}

impl Drop for RealChildOwnedResidueV1 {
    fn drop(&mut self) {
        if !self.cleaned {
            let _ = self.child.force_cleanup();
            self.cleaned = true;
        }
    }
}

struct LinuxChildBackendV1 {
    started_at: Instant,
    stdin_frame: Option<Vec<u8>>,
    containment_setup: ContainmentSetupV1,
    active: Option<ActiveChildV1>,
    completion: Option<RealChildCompletionV1>,
    retained_residue: Option<RealChildOwnedResidueV1>,
    retained_unstarted: Vec<PreparedChildLaunchV1>,
    os_spawn_count: usize,
    request_start_count: usize,
}

impl LinuxChildBackendV1 {
    fn new(stdin_frame: Vec<u8>) -> Self {
        Self {
            started_at: Instant::now(),
            stdin_frame: Some(stdin_frame),
            containment_setup: ContainmentSetupV1::Enforce,
            active: None,
            completion: None,
            retained_residue: None,
            retained_unstarted: Vec::new(),
            os_spawn_count: 0,
            request_start_count: 0,
        }
    }

    #[cfg(test)]
    fn new_with_containment_setup_for_test(
        stdin_frame: Vec<u8>,
        setup: TestContainmentSetupV1,
    ) -> Self {
        let mut backend = Self::new(stdin_frame);
        backend.containment_setup = match setup {
            TestContainmentSetupV1::FailBeforeSpawn => ContainmentSetupV1::FailBeforeSpawn,
        };
        backend
    }

    fn os_spawn_count(&self) -> usize {
        self.os_spawn_count
    }

    fn request_start_count(&self) -> usize {
        self.request_start_count
    }

    fn take_completion(&mut self) -> Option<RealChildCompletionV1> {
        self.completion.take()
    }

    fn take_retained_residue(&mut self) -> Option<RealChildOwnedResidueV1> {
        self.retained_residue.take()
    }

    fn active_for(&self, child: &OwnedChildTokenV1) -> Result<&ActiveChildV1, String> {
        if child.id() != CHILD_TOKEN_ID {
            return Err("unknown child ownership token".to_string());
        }
        self.active
            .as_ref()
            .ok_or_else(|| "owned child is not active".to_string())
    }

    fn active_for_mut(&mut self, child: &OwnedChildTokenV1) -> Result<&mut ActiveChildV1, String> {
        if child.id() != CHILD_TOKEN_ID {
            return Err("unknown child ownership token".to_string());
        }
        self.active
            .as_mut()
            .ok_or_else(|| "owned child is not active".to_string())
    }
}

impl Drop for LinuxChildBackendV1 {
    fn drop(&mut self) {
        if let Some(mut child) = self.active.take() {
            let _ = child.force_cleanup();
        }
        if let Some(residue) = self.retained_residue.take() {
            drop(residue);
        }
    }
}

impl ChildPhaseSupervisorEffectsV1 for LinuxChildBackendV1 {
    fn monotonic_millis(&mut self) -> Result<u64, String> {
        checked_duration_millis_v1(self.started_at.elapsed())
    }

    fn spawn(&mut self, launch: PreparedChildLaunchV1) -> Result<OwnedChildTokenV1, String> {
        if self.active.is_some() || self.os_spawn_count != 0 {
            return Err("backend permits exactly one OS spawn".to_string());
        }
        let launch = launch.into_os_launch();
        let stdin_len = self.stdin_frame.as_ref().map_or(0, Vec::len);
        if stdin_len > launch.stdin_limit() || stdin_len > launch.frame_limit() {
            return Err("stdin frame exceeds its authorized byte limit".to_string());
        }
        let mut child = spawn_child(&launch, self.containment_setup)?;
        self.os_spawn_count += 1;
        let pgid = match i32::try_from(child.id()) {
            Ok(pgid) => pgid,
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("child pid exceeds i32".to_string());
            }
        };
        if let Err(detail) = validate_owned_child_group(pgid) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(detail);
        }
        let (stdin, stdout, stderr) =
            match (child.stdin.take(), child.stdout.take(), child.stderr.take()) {
                (Some(stdin), Some(stdout), Some(stderr)) => (stdin, stdout, stderr),
                pipes => {
                    drop(pipes);
                    let detail = cleanup_spawn_failure(
                        &mut child,
                        pgid,
                        io::Error::other("spawned child is missing an owned stdio pipe"),
                    );
                    return Err(detail);
                }
            };

        let stdout_worker = spawn_read_worker("kitsu-child-stdout", stdout, launch.stdout_limit())
            .map_err(|error| cleanup_spawn_failure(&mut child, pgid, error))?;
        let stderr_worker =
            match spawn_read_worker("kitsu-child-stderr", stderr, launch.stderr_limit()) {
                Ok(worker) => worker,
                Err(error) => {
                    let _ = signal_process_group(pgid, libc::SIGKILL);
                    let _ = child.wait();
                    let _ = stdout_worker.join();
                    return Err(error.to_string());
                }
            };
        self.active = Some(ActiveChildV1 {
            child,
            pgid,
            phase_budgets_millis: launch.phase_budgets_millis(),
            stdin: Some(stdin),
            stdin_worker: None,
            stdin_outcome: None,
            stdout_worker: Some(stdout_worker),
            stdout_outcome: None,
            stderr_worker: Some(stderr_worker),
            stderr_outcome: None,
            exit_observed: false,
            exit_status: None,
        });
        Ok(OwnedChildTokenV1::new(CHILD_TOKEN_ID))
    }

    fn startup_ready(&mut self, child: &OwnedChildTokenV1) -> Result<bool, String> {
        self.active_for(child)?;
        Ok(true)
    }

    fn begin_request(&mut self, child: &OwnedChildTokenV1) -> Result<(), String> {
        if self.request_start_count != 0 {
            return Err("backend permits exactly one request".to_string());
        }
        let frame = self
            .stdin_frame
            .take()
            .ok_or_else(|| "stdin frame was already consumed".to_string())?;
        let active = self.active_for_mut(child)?;
        let stdin = active
            .stdin
            .take()
            .ok_or_else(|| "owned stdin pipe was already consumed".to_string())?;
        let worker = thread::Builder::new()
            .name("kitsu-child-stdin".to_string())
            .spawn(move || write_stdin(stdin, frame))
            .map_err(|error| error.to_string())?;
        active.stdin_worker = Some(worker);
        self.request_start_count += 1;
        Ok(())
    }

    fn request_complete(&mut self, child: &OwnedChildTokenV1) -> Result<bool, String> {
        let active = self.active_for_mut(child)?;
        active.refresh_workers();
        active.observe_child_exit()?;
        if let Some(fault) = active.worker_fault() {
            return Err(fault);
        }
        Ok(active.exit_observed
            && active.all_workers_joined()
            && active
                .stdout_outcome
                .as_ref()
                .is_some_and(|outcome| outcome.eof)
            && active
                .stderr_outcome
                .as_ref()
                .is_some_and(|outcome| outcome.eof))
    }

    fn begin_drain(
        &mut self,
        child: &OwnedChildTokenV1,
        _: ChildSupervisorCauseV1,
    ) -> Result<(), String> {
        let active = self.active_for_mut(child)?;
        active.stdin.take();
        if active.stdin_worker.is_none() && active.stdin_outcome.is_none() {
            active.stdin_outcome = Some(PipeWriteOutcomeV1 { error: None });
        }
        Ok(())
    }

    fn drain_complete(&mut self, child: &OwnedChildTokenV1) -> Result<bool, String> {
        let active = self.active_for_mut(child)?;
        active.refresh_workers();
        active.observe_child_exit()?;
        if let Some(fault) = active.worker_fault() {
            return Err(fault);
        }
        Ok(active.exit_observed && active.all_workers_joined())
    }

    fn terminate_process_group(&mut self, child: &OwnedChildTokenV1) -> Result<(), String> {
        let active = self.active_for(child)?;
        if active.exit_status.is_some() {
            return Err("refusing group signal after direct-child reap".to_string());
        }
        let pgid = active.pgid;
        signal_process_group(pgid, libc::SIGKILL)
    }

    fn process_group_terminated(&mut self, child: &OwnedChildTokenV1) -> Result<bool, String> {
        let active = self.active_for_mut(child)?;
        active.observe_child_exit()?;
        active.refresh_workers();
        if !active.exit_observed {
            return Ok(false);
        }
        if active.exit_status.is_none() {
            active.exit_status = active.child.try_wait().map_err(|error| error.to_string())?;
        }
        Ok(active.exit_status.is_some() && process_group_is_gone(active.pgid))
    }

    fn poll_reap(&mut self, child: OwnedChildTokenV1) -> ChildReapPollV1 {
        if child.id() != CHILD_TOKEN_ID {
            return ChildReapPollV1::Failed {
                child,
                detail: "unknown child ownership token".to_string(),
            };
        }
        let Some(active) = self.active.as_mut() else {
            return ChildReapPollV1::Failed {
                child,
                detail: "owned child is not active".to_string(),
            };
        };
        if let Err(detail) = active.observe_child_exit() {
            return ChildReapPollV1::Failed { child, detail };
        }
        active.refresh_workers();
        if let Some(detail) = active.worker_fault() {
            return ChildReapPollV1::Failed { child, detail };
        }
        if !active.exit_observed || !active.all_workers_joined() {
            return ChildReapPollV1::Pending(child);
        }
        if active.exit_status.is_none() {
            match active.child.try_wait() {
                Ok(Some(status)) => active.exit_status = Some(status),
                Ok(None) => return ChildReapPollV1::Pending(child),
                Err(error) => {
                    return ChildReapPollV1::Failed {
                        child,
                        detail: error.to_string(),
                    }
                }
            }
        }
        if !process_group_is_gone(active.pgid) {
            return ChildReapPollV1::Pending(child);
        }

        let active = self.active.take().expect("checked active child");
        let exit = exit_truth(active.exit_status.expect("checked exit status"));
        let stdout = active.stdout_outcome.expect("checked stdout worker");
        let stderr = active.stderr_outcome.expect("checked stderr worker");
        let truth = if exit == ChildExitTruthV1::Exited(0) && stdout.eof && stderr.eof {
            RealChildCompletionTruthV1::Success
        } else {
            RealChildCompletionTruthV1::Crash(exit)
        };
        self.completion = Some(RealChildCompletionV1 {
            stdout: stdout.bytes,
            stderr: stderr.bytes,
            stdout_eof: stdout.eof,
            stderr_eof: stderr.eof,
            workers_joined: true,
            direct_child_reaped: true,
            truth,
        });
        ChildReapPollV1::Reaped(exit)
    }

    fn retain_unstarted_launch(&mut self, launch: PreparedChildLaunchV1) {
        self.retained_unstarted.push(launch);
    }

    fn retain_owned_residue(&mut self, residue: OwnedChildResidueV1) {
        let child = self
            .active
            .take()
            .expect("a supervisor residue corresponds to one active OS child");
        assert!(self.retained_residue.is_none(), "only one residue is owned");
        self.retained_residue = Some(RealChildOwnedResidueV1 {
            _supervisor_residue: residue,
            child,
            cleaned: false,
        });
    }
}

fn cleanup_spawn_failure(child: &mut Child, pgid: i32, error: io::Error) -> String {
    let _ = signal_process_group(pgid, libc::SIGKILL);
    let _ = child.wait();
    error.to_string()
}

fn spawn_read_worker<R>(
    name: &str,
    reader: R,
    limit: usize,
) -> io::Result<JoinHandle<PipeReadOutcomeV1>>
where
    R: Read + Send + 'static,
{
    thread::Builder::new()
        .name(name.to_string())
        .spawn(move || read_bounded_to_eof(reader, limit))
}

fn read_bounded_to_eof<R: Read>(mut reader: R, limit: usize) -> PipeReadOutcomeV1 {
    let mut bytes = Vec::with_capacity(limit.min(PIPE_CHUNK_BYTES));
    let mut buffer = [0_u8; PIPE_CHUNK_BYTES];
    let mut overflow = false;
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => {
                return PipeReadOutcomeV1 {
                    bytes,
                    eof: true,
                    overflow,
                    error: None,
                }
            }
            Ok(read) => {
                let remaining = limit.saturating_sub(bytes.len());
                let retain = remaining.min(read);
                bytes.extend_from_slice(&buffer[..retain]);
                overflow |= retain < read;
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => {
                return PipeReadOutcomeV1 {
                    bytes,
                    eof: false,
                    overflow,
                    error: Some(error.to_string()),
                }
            }
        }
    }
}

fn write_stdin(mut stdin: ChildStdin, frame: Vec<u8>) -> PipeWriteOutcomeV1 {
    let result = stdin.write_all(&frame).and_then(|()| stdin.flush());
    drop(stdin);
    PipeWriteOutcomeV1 {
        error: result.err().map(|error| error.to_string()),
    }
}

fn spawn_child(
    launch: &PreparedChildOsLaunchV1,
    containment_setup: ContainmentSetupV1,
) -> Result<Child, String> {
    // Filter construction and all of its allocation happen in the parent,
    // before Command::spawn can fork.
    let containment = prepare_child_containment(containment_setup)
        .map_err(|error| format!("cannot prepare child containment: {error}"))?;
    let mut command = Command::new(launch.executable());
    command
        .args(launch.arguments())
        .current_dir(launch.working_directory())
        .env_clear()
        .envs(launch.environment())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    // SAFETY: the closure uses only async-signal-safe syscalls and constructs
    // no shared process state. Every setup error aborts before exec.
    unsafe {
        command.pre_exec(move || install_child_containment(&containment));
    }
    command.spawn().map_err(|error| error.to_string())
}

fn exit_truth(status: ExitStatus) -> ChildExitTruthV1 {
    status
        .code()
        .map_or(ChildExitTruthV1::Signalled, ChildExitTruthV1::Exited)
}

fn signal_process_group(pgid: i32, signal: i32) -> Result<(), String> {
    validate_group_signal_target(pgid)?;
    // SAFETY: a negative pid addresses the already-owned process group.
    if unsafe { libc::kill(-pgid, signal) } == 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        Ok(())
    } else {
        Err(error.to_string())
    }
}

fn validate_owned_child_group(child_pid: i32) -> Result<(), String> {
    validate_group_signal_target(child_pid)?;
    // SAFETY: getpgid performs a scalar kernel identity query.
    let observed = unsafe { libc::getpgid(child_pid) };
    if observed == child_pid {
        Ok(())
    } else if observed == -1 {
        Err(format!(
            "cannot prove spawned child process-group ownership: {}",
            io::Error::last_os_error()
        ))
    } else {
        Err(format!(
            "spawned child group mismatch: pid {child_pid}, pgid {observed}"
        ))
    }
}

fn validate_group_signal_target(pgid: i32) -> Result<(), String> {
    // No owned child group can be init, this process, or this process's group.
    let host_pid = i32::try_from(std::process::id()).map_err(|_| "host pid exceeds i32")?;
    // SAFETY: getpgrp performs a scalar kernel identity query.
    let host_pgid = unsafe { libc::getpgrp() };
    if pgid <= 1 || pgid == host_pid || pgid == host_pgid {
        Err(format!("refusing unsafe process-group target {pgid}"))
    } else {
        Ok(())
    }
}

fn process_group_is_gone(pgid: i32) -> bool {
    // SAFETY: signal zero is a kernel liveness query only.
    if unsafe { libc::kill(-pgid, 0) } == 0 {
        return false;
    }
    io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
}

fn child_exit_is_observable(child_pid: u32) -> Result<bool, String> {
    let mut information = MaybeUninit::<libc::siginfo_t>::zeroed();
    // SAFETY: waitid writes one siginfo_t, WNOWAIT preserves the direct-child
    // wait right, and the pid comes from the owned Child handle.
    let result = unsafe {
        libc::waitid(
            libc::P_PID,
            child_pid,
            information.as_mut_ptr(),
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error().to_string());
    }
    // SAFETY: successful waitid initializes the supplied siginfo_t. A zero
    // si_pid is the specified WNOHANG result when no child is waitable.
    Ok(unsafe { information.assume_init().si_pid() } != 0)
}

fn checked_duration_millis_v1(duration: Duration) -> Result<u64, String> {
    u64::try_from(duration.as_millis())
        .map_err(|_| "monotonic duration does not fit in u64 milliseconds".to_string())
}

fn prepare_child_containment(setup: ContainmentSetupV1) -> io::Result<PreparedChildContainmentV1> {
    if setup == ContainmentSetupV1::FailBeforeSpawn {
        return Err(io::Error::from_raw_os_error(libc::ENOTSUP));
    }
    let seccomp_filter = build_seccomp_filter()?;
    let seccomp_filter_len = u16::try_from(seccomp_filter.len())
        .map_err(|_| io::Error::from_raw_os_error(libc::E2BIG))?;
    Ok(PreparedChildContainmentV1 {
        seccomp_filter,
        seccomp_filter_len,
    })
}

fn install_child_containment(containment: &PreparedChildContainmentV1) -> io::Result<()> {
    verify_child_owns_process_group()?;
    close_inherited_descriptors()?;
    clear_inherited_capabilities()?;
    install_seccomp_filter(containment)
}

fn verify_child_owns_process_group() -> io::Result<()> {
    // SAFETY: both calls are scalar identity queries in the post-fork child.
    let pid = unsafe { libc::getpid() };
    let pgid = unsafe { libc::getpgrp() };
    if pid > 1 && pid == pgid {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(libc::EPERM))
    }
}

fn close_inherited_descriptors() -> io::Result<()> {
    // SAFETY: close_range changes descriptor flags only in the post-fork child.
    let result = unsafe {
        libc::syscall(
            libc::SYS_close_range,
            3_u32,
            u32::MAX,
            libc::CLOSE_RANGE_CLOEXEC,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[repr(C)]
struct CapabilityHeaderV1 {
    version: u32,
    pid: i32,
}

#[repr(C)]
struct CapabilityDataV1 {
    effective: u32,
    permitted: u32,
    inheritable: u32,
}

fn clear_inherited_capabilities() -> io::Result<()> {
    // SAFETY: prctl and capset receive scalar values and pointers to the local
    // fixed-layout capability structures.
    if unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } != 0 {
        return Err(io::Error::last_os_error());
    }
    if unsafe {
        libc::prctl(
            libc::PR_CAP_AMBIENT,
            libc::PR_CAP_AMBIENT_CLEAR_ALL,
            0,
            0,
            0,
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    let header = CapabilityHeaderV1 {
        version: 0x2008_0522,
        pid: 0,
    };
    let data = [
        CapabilityDataV1 {
            effective: 0,
            permitted: 0,
            inheritable: 0,
        },
        CapabilityDataV1 {
            effective: 0,
            permitted: 0,
            inheritable: 0,
        },
    ];
    if unsafe { libc::syscall(libc::SYS_capset, &header, data.as_ptr()) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(target_arch = "x86_64")]
fn build_seccomp_filter() -> io::Result<Box<[libc::sock_filter]>> {
    const BPF_LD: u16 = 0x00;
    const BPF_W: u16 = 0x00;
    const BPF_ABS: u16 = 0x20;
    const BPF_JMP: u16 = 0x05;
    const BPF_JEQ: u16 = 0x10;
    const BPF_JSET: u16 = 0x40;
    const BPF_K: u16 = 0x00;
    const BPF_RET: u16 = 0x06;
    const SECCOMP_RET_KILL_PROCESS: u32 = 0x8000_0000;
    const SECCOMP_RET_ERRNO: u32 = 0x0005_0000;
    const SECCOMP_RET_ALLOW: u32 = 0x7fff_0000;
    const AUDIT_ARCH_X86_64: u32 = 0xc000_003e;
    const X32_SYSCALL_BIT: u32 = 0x4000_0000;
    const NAMESPACE_FLAGS: u32 = 0x7e02_0000;

    fn statement(code: u16, value: u32) -> libc::sock_filter {
        libc::sock_filter {
            code,
            jt: 0,
            jf: 0,
            k: value,
        }
    }

    fn jump(code: u16, value: u32, yes: u8, no: u8) -> libc::sock_filter {
        libc::sock_filter {
            code,
            jt: yes,
            jf: no,
            k: value,
        }
    }

    let load_word = BPF_LD | BPF_W | BPF_ABS;
    let jump_equal = BPF_JMP | BPF_JEQ | BPF_K;
    let jump_bits_set = BPF_JMP | BPF_JSET | BPF_K;
    let return_value = BPF_RET | BPF_K;
    let deny = SECCOMP_RET_ERRNO | u32::try_from(libc::EPERM).expect("errno fits u32");
    let unavailable = SECCOMP_RET_ERRNO | u32::try_from(libc::ENOSYS).expect("errno fits u32");
    let denied_syscalls = [
        libc::SYS_socket,
        libc::SYS_socketpair,
        libc::SYS_bind,
        libc::SYS_listen,
        libc::SYS_accept,
        libc::SYS_accept4,
        libc::SYS_connect,
        libc::SYS_getsockname,
        libc::SYS_getpeername,
        libc::SYS_sendto,
        libc::SYS_recvfrom,
        libc::SYS_sendmsg,
        libc::SYS_recvmsg,
        libc::SYS_sendmmsg,
        libc::SYS_recvmmsg,
        libc::SYS_setsockopt,
        libc::SYS_getsockopt,
        libc::SYS_shutdown,
        libc::SYS_io_uring_setup,
        libc::SYS_io_uring_enter,
        libc::SYS_io_uring_register,
        libc::SYS_bpf,
        libc::SYS_pidfd_open,
        libc::SYS_pidfd_getfd,
        libc::SYS_setsid,
        libc::SYS_setpgid,
        libc::SYS_unshare,
        libc::SYS_setns,
    ];
    let mut filter = Vec::with_capacity(8 + denied_syscalls.len() * 2);
    filter.push(statement(load_word, 4));
    filter.push(jump(jump_equal, AUDIT_ARCH_X86_64, 1, 0));
    filter.push(statement(return_value, SECCOMP_RET_KILL_PROCESS));
    filter.push(statement(load_word, 0));
    // x32 shares the audit architecture but uses a distinct syscall-number
    // bit. Reject it so none of the deny list can be bypassed through that ABI.
    filter.push(jump(jump_bits_set, X32_SYSCALL_BIT, 0, 1));
    filter.push(statement(return_value, unavailable));
    for syscall in denied_syscalls {
        filter.push(jump(
            jump_equal,
            u32::try_from(syscall).expect("syscall number fits u32"),
            0,
            1,
        ));
        filter.push(statement(return_value, deny));
    }
    // glibc uses ENOSYS to fall back from clone3 to legacy clone, whose
    // namespace flags are inspectable below. clone3 remains unavailable.
    filter.push(jump(
        jump_equal,
        u32::try_from(libc::SYS_clone3).expect("syscall number fits u32"),
        0,
        1,
    ));
    filter.push(statement(return_value, unavailable));
    filter.push(jump(
        jump_equal,
        u32::try_from(libc::SYS_clone).expect("syscall number fits u32"),
        0,
        3,
    ));
    filter.push(statement(load_word, 16));
    filter.push(jump(jump_bits_set, NAMESPACE_FLAGS, 0, 1));
    filter.push(statement(return_value, deny));
    filter.push(statement(return_value, SECCOMP_RET_ALLOW));

    Ok(filter.into_boxed_slice())
}

fn install_seccomp_filter(containment: &PreparedChildContainmentV1) -> io::Result<()> {
    let program = libc::sock_fprog {
        len: containment.seccomp_filter_len,
        filter: containment.seccomp_filter.as_ptr().cast_mut(),
    };
    // SAFETY: the kernel copies the bounded filter during this prctl call.
    if unsafe {
        libc::prctl(
            libc::PR_SET_SECCOMP,
            libc::SECCOMP_MODE_FILTER,
            &program as *const libc::sock_fprog,
        )
    } == 0
    {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(not(target_arch = "x86_64"))]
fn build_seccomp_filter() -> io::Result<Box<[libc::sock_filter]>> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "external-child containment is unsupported on this architecture",
    ))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::io::{Read, Write};
    use std::os::fd::RawFd;
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};

    use ring::digest::{Context, SHA256};

    use super::*;
    use crate::external::authority::{
        prepare_child_launch_request_v1, AdmittedChildLaunchV1, AuthorityProfileV1,
        ChildEnvironmentPolicyV1, ChildEnvironmentVariableV1, ChildIoLimitsV1,
        ChildLaunchAdmissionErrorV1, ChildLaunchAuthorityDecisionV1, ChildLaunchModeV1,
        ChildLaunchPreparationV1, ChildLaunchRequestV1, ChildNetworkPolicyV1, ChildPhaseBudgetsV1,
        ChildProcessGroupPolicyV1, ChildRuntimePolicyV1, ChildTimeoutDispositionV1,
        PreparedChildLaunchV1,
    };
    use crate::external::lifecycle::ChildExitTruthV1;
    use crate::external::supervisor::{
        ChildPhaseSupervisorFailureV1, ChildPhaseSupervisorStateV1, ChildPhaseSupervisorV1,
        ChildSupervisorCauseV1,
    };

    const HELPER_TEST: &str = "external::backend::tests::real_child_fixture_entry";

    #[derive(Debug, Clone, Copy)]
    struct TestLimits {
        startup_millis: u64,
        request_millis: u64,
        drain_millis: u64,
        kill_millis: u64,
        reap_millis: u64,
        stdin_bytes: usize,
        stdout_bytes: usize,
        stderr_bytes: usize,
    }

    impl Default for TestLimits {
        fn default() -> Self {
            Self {
                startup_millis: 500,
                request_millis: 1_000,
                drain_millis: 100,
                kill_millis: 500,
                reap_millis: 500,
                stdin_bytes: 4 * 1024,
                stdout_bytes: 64 * 1024,
                stderr_bytes: 64 * 1024,
            }
        }
    }

    #[derive(Default)]
    struct RealPreparation;

    impl ChildLaunchPreparationV1 for RealPreparation {
        fn canonicalize_executable(&mut self, executable: &Path) -> Result<PathBuf, String> {
            executable.canonicalize().map_err(|error| error.to_string())
        }

        fn executable_sha256(&mut self, executable: &Path) -> Result<[u8; 32], String> {
            sha256(executable)
        }

        fn containment_preflight(&mut self, _: &AdmittedChildLaunchV1) -> Result<(), String> {
            Ok(())
        }
    }

    fn sha256(path: &Path) -> Result<[u8; 32], String> {
        let mut file = std::fs::File::open(path).map_err(|error| error.to_string())?;
        let mut context = Context::new(&SHA256);
        let mut buffer = [0_u8; 16 * 1024];
        loop {
            let read = file.read(&mut buffer).map_err(|error| error.to_string())?;
            if read == 0 {
                break;
            }
            context.update(&buffer[..read]);
        }
        let digest = context.finish();
        let mut bytes = [0_u8; 32];
        bytes.copy_from_slice(digest.as_ref());
        Ok(bytes)
    }

    fn digest_hex(path: &Path) -> String {
        sha256(path)
            .unwrap()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    fn helper_executable() -> PathBuf {
        std::env::current_exe().unwrap().canonicalize().unwrap()
    }

    fn helper_arguments() -> Vec<String> {
        vec![
            "--exact".to_string(),
            HELPER_TEST.to_string(),
            "--nocapture".to_string(),
        ]
    }

    fn runtime_policy(
        limits: TestLimits,
        environment: Vec<ChildEnvironmentVariableV1>,
    ) -> ChildRuntimePolicyV1 {
        ChildRuntimePolicyV1::new(
            ChildPhaseBudgetsV1::new(
                limits.startup_millis,
                limits.request_millis,
                limits.drain_millis,
                limits.kill_millis,
                limits.reap_millis,
            ),
            ChildIoLimitsV1::new(
                limits.stdin_bytes,
                limits.stdout_bytes,
                limits.stderr_bytes,
                limits.stdin_bytes.min(limits.stdout_bytes),
            ),
            ChildProcessGroupPolicyV1::Owned,
            ChildEnvironmentPolicyV1::new(true, environment),
            ChildNetworkPolicyV1::Deny,
            ChildTimeoutDispositionV1::TerminateProcessGroupAndReap,
        )
    }

    fn prepared_helper(
        cwd: &Path,
        mode: &str,
        limits: TestLimits,
        mut extra_environment: Vec<ChildEnvironmentVariableV1>,
    ) -> PreparedChildLaunchV1 {
        let executable = helper_executable();
        extra_environment.insert(0, ChildEnvironmentVariableV1::new("KITSU_CHILD_MODE", mode));
        let policy = runtime_policy(limits, extra_environment);
        let request = ChildLaunchRequestV1::new(
            1,
            ChildLaunchModeV1::Enabled,
            &executable,
            digest_hex(&executable),
            helper_arguments(),
            cwd,
            policy.clone(),
        );
        let authority = AuthorityProfileV1::exact(
            &executable,
            digest_hex(&executable),
            helper_arguments(),
            cwd,
            policy,
        )
        .unwrap();
        prepare_child_launch_request_v1(
            request,
            ChildLaunchAuthorityDecisionV1::Admit(&authority),
            &mut RealPreparation,
        )
        .unwrap()
    }

    fn drive_one_shot(
        prepared: PreparedChildLaunchV1,
        stdin: Vec<u8>,
    ) -> (ChildPhaseSupervisorStateV1, LinuxChildBackendV1) {
        let mut backend = LinuxChildBackendV1::new(stdin);
        let terminal;
        {
            let mut supervisor = ChildPhaseSupervisorV1::new(prepared, &mut backend);
            let wall_deadline = Instant::now() + Duration::from_secs(5);
            loop {
                assert!(Instant::now() < wall_deadline, "fixture supervisor hung");
                match supervisor.state() {
                    ChildPhaseSupervisorStateV1::Ready => {
                        supervisor.start().unwrap();
                    }
                    ChildPhaseSupervisorStateV1::Starting { .. }
                    | ChildPhaseSupervisorStateV1::Requesting { .. }
                    | ChildPhaseSupervisorStateV1::Draining { .. }
                    | ChildPhaseSupervisorStateV1::Killing { .. }
                    | ChildPhaseSupervisorStateV1::Reaping { .. } => {
                        supervisor.poll().unwrap();
                    }
                    ChildPhaseSupervisorStateV1::Running => {
                        supervisor.begin_request().unwrap();
                    }
                    ChildPhaseSupervisorStateV1::RequestComplete => {
                        supervisor.shutdown().unwrap();
                    }
                    ChildPhaseSupervisorStateV1::StoppedBeforeSpawn(_)
                    | ChildPhaseSupervisorStateV1::Stopped { .. }
                    | ChildPhaseSupervisorStateV1::Failed(_) => {
                        terminal = supervisor.state().clone();
                        break;
                    }
                }
                std::thread::sleep(Duration::from_millis(1));
            }
        }
        (terminal, backend)
    }

    fn drive_cancel_or_shutdown(
        prepared: PreparedChildLaunchV1,
        cause: ChildSupervisorCauseV1,
    ) -> (ChildPhaseSupervisorStateV1, LinuxChildBackendV1) {
        let mut backend = LinuxChildBackendV1::new(Vec::new());
        let terminal;
        {
            let mut supervisor = ChildPhaseSupervisorV1::new(prepared, &mut backend);
            supervisor.start().unwrap();
            let wall_deadline = Instant::now() + Duration::from_secs(5);
            while !matches!(supervisor.state(), ChildPhaseSupervisorStateV1::Running) {
                assert!(Instant::now() < wall_deadline, "startup fixture hung");
                supervisor.poll().unwrap();
                std::thread::sleep(Duration::from_millis(1));
            }
            match cause {
                ChildSupervisorCauseV1::Cancel => {
                    supervisor.cancel().unwrap();
                }
                ChildSupervisorCauseV1::Shutdown => {
                    supervisor.shutdown().unwrap();
                }
                _ => unreachable!("fixture uses an explicit host drain cause"),
            }
            loop {
                assert!(Instant::now() < wall_deadline, "drain fixture hung");
                match supervisor.state() {
                    ChildPhaseSupervisorStateV1::Draining { .. }
                    | ChildPhaseSupervisorStateV1::Killing { .. }
                    | ChildPhaseSupervisorStateV1::Reaping { .. } => {
                        supervisor.poll().unwrap();
                    }
                    ChildPhaseSupervisorStateV1::Stopped { .. }
                    | ChildPhaseSupervisorStateV1::Failed(_) => {
                        terminal = supervisor.state().clone();
                        break;
                    }
                    state => panic!("unexpected drain state {state:?}"),
                }
                std::thread::sleep(Duration::from_millis(1));
            }
        }
        (terminal, backend)
    }

    fn fixture_json(stdout: &[u8], stderr: &[u8]) -> serde_json::Value {
        let stdout = String::from_utf8_lossy(stdout);
        let line = stdout
            .lines()
            .find_map(|line| line.strip_prefix("KITSU_FIXTURE_JSON="))
            .unwrap_or_else(|| {
                panic!(
                    "fixture JSON missing; stdout={stdout:?}, stderr={:?}",
                    String::from_utf8_lossy(stderr)
                )
            });
        serde_json::from_str(line).unwrap()
    }

    fn clear_cloexec(fd: RawFd) {
        // SAFETY: the fixture owns `fd`; fcntl neither aliases nor dereferences
        // Rust memory.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        assert!(flags >= 0);
        assert_eq!(
            unsafe { libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) },
            0
        );
    }

    #[test]
    fn prepared_projection_is_exact_consuming_and_redacts_environment_values() {
        let cwd = tempfile::tempdir().unwrap();
        let prepared = prepared_helper(
            cwd.path(),
            "inspect",
            TestLimits::default(),
            vec![ChildEnvironmentVariableV1::new(
                "KITSU_PROJECTED_VALUE",
                "projection-secret",
            )],
        );
        let launch = prepared.into_os_launch();

        assert_eq!(launch.executable(), helper_executable());
        assert_eq!(launch.arguments(), helper_arguments());
        assert_eq!(launch.working_directory(), cwd.path());
        assert_eq!(
            launch.environment_for_test(),
            [
                ("KITSU_CHILD_MODE", "inspect"),
                ("KITSU_PROJECTED_VALUE", "projection-secret"),
            ]
        );
        assert_eq!(launch.stdin_limit(), TestLimits::default().stdin_bytes);
        assert_eq!(launch.stdout_limit(), TestLimits::default().stdout_bytes);
        assert_eq!(launch.stderr_limit(), TestLimits::default().stderr_bytes);
        let debug = format!("{launch:?}");
        assert!(debug.contains("KITSU_PROJECTED_VALUE"));
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains("projection-secret"));
    }

    #[test]
    fn disabled_admission_and_oversized_stdin_have_zero_os_spawn() {
        let cwd = tempfile::tempdir().unwrap();
        let executable = helper_executable();
        let limits = TestLimits {
            stdin_bytes: 4,
            ..TestLimits::default()
        };
        let policy = runtime_policy(
            limits,
            vec![ChildEnvironmentVariableV1::new("KITSU_CHILD_MODE", "echo")],
        );
        let disabled = ChildLaunchRequestV1::new(
            1,
            ChildLaunchModeV1::Disabled,
            &executable,
            digest_hex(&executable),
            helper_arguments(),
            cwd.path(),
            policy.clone(),
        );
        let authority = AuthorityProfileV1::exact(
            &executable,
            digest_hex(&executable),
            helper_arguments(),
            cwd.path(),
            policy,
        )
        .unwrap();
        assert_eq!(
            prepare_child_launch_request_v1(
                disabled,
                ChildLaunchAuthorityDecisionV1::Admit(&authority),
                &mut RealPreparation,
            ),
            Err(ChildLaunchAdmissionErrorV1::Disabled)
        );
        let disabled_backend = LinuxChildBackendV1::new(Vec::new());
        assert_eq!(disabled_backend.os_spawn_count(), 0);

        let prepared = prepared_helper(cwd.path(), "echo", limits, Vec::new());
        let (terminal, oversized_backend) = drive_one_shot(prepared, b"12345".to_vec());
        assert!(matches!(
            terminal,
            ChildPhaseSupervisorStateV1::Failed(ChildPhaseSupervisorFailureV1::Spawn(_))
        ));
        assert_eq!(oversized_backend.os_spawn_count(), 0);
    }

    #[test]
    fn seccomp_program_is_built_and_validated_in_the_parent() {
        let containment = prepare_child_containment(ContainmentSetupV1::Enforce).unwrap();
        assert!(!containment.seccomp_filter.is_empty());
        assert_eq!(
            usize::from(containment.seccomp_filter_len),
            containment.seccomp_filter.len()
        );
    }

    #[test]
    fn unsupported_containment_fails_before_spawn_and_child_code() {
        let cwd = tempfile::tempdir().unwrap();
        let marker = cwd.path().join("must-not-exist");
        let prepared = prepared_helper(
            cwd.path(),
            "marker",
            TestLimits::default(),
            vec![ChildEnvironmentVariableV1::new(
                "KITSU_MARKER_PATH",
                marker.to_string_lossy(),
            )],
        );
        let mut backend = LinuxChildBackendV1::new_with_containment_setup_for_test(
            Vec::new(),
            TestContainmentSetupV1::FailBeforeSpawn,
        );
        {
            let mut supervisor = ChildPhaseSupervisorV1::new(prepared, &mut backend);
            assert!(matches!(
                supervisor.start(),
                Ok(ChildPhaseSupervisorStateV1::Failed(
                    ChildPhaseSupervisorFailureV1::Spawn(_)
                ))
            ));
        }
        assert_eq!(backend.os_spawn_count(), 0);
        assert!(!marker.exists());
        assert!(backend.take_completion().is_none());
        assert!(backend.take_retained_residue().is_none());
    }

    #[test]
    fn process_group_target_guard_rejects_the_harness_process_and_group() {
        let host_pid = i32::try_from(std::process::id()).unwrap();
        // SAFETY: getpgrp performs a scalar kernel identity query.
        let host_pgid = unsafe { libc::getpgrp() };
        assert!(validate_group_signal_target(host_pid).is_err());
        assert!(validate_group_signal_target(host_pgid).is_err());
        assert!(validate_group_signal_target(1).is_err());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn live_child_has_exact_argv_cwd_env_group_fd_capability_and_syscall_fences() {
        let cwd = tempfile::tempdir().unwrap();
        let mut sockets = [0_i32; 2];
        // SAFETY: `sockets` has space for both returned descriptors.
        assert_eq!(
            unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, sockets.as_mut_ptr()) },
            0
        );
        clear_cloexec(sockets[1]);
        let host_pid = i32::try_from(std::process::id()).unwrap();
        // SAFETY: getpgrp performs a scalar kernel identity query.
        let host_pgid = unsafe { libc::getpgrp() };
        let prepared = prepared_helper(
            cwd.path(),
            "inspect",
            TestLimits::default(),
            vec![
                ChildEnvironmentVariableV1::new("KITSU_PROJECTED_VALUE", "exact-value"),
                ChildEnvironmentVariableV1::new("KITSU_INHERITED_FD", sockets[1].to_string()),
                ChildEnvironmentVariableV1::new("KITSU_HOST_PID", host_pid.to_string()),
                ChildEnvironmentVariableV1::new("KITSU_HOST_PGID", host_pgid.to_string()),
            ],
        );
        let (terminal, mut backend) = drive_one_shot(prepared, Vec::new());
        assert!(
            matches!(terminal, ChildPhaseSupervisorStateV1::Stopped { .. }),
            "unexpected terminal state: {terminal:?}"
        );
        let completion = backend.take_completion().expect("one completion");
        let json = fixture_json(completion.stdout(), completion.stderr());

        assert_eq!(json["cwd"], cwd.path().to_string_lossy().as_ref());
        assert_eq!(
            json["argv"],
            serde_json::json!([
                helper_executable().to_string_lossy(),
                "--exact",
                HELPER_TEST,
                "--nocapture",
            ])
        );
        assert_eq!(
            json["environment"],
            serde_json::json!({
                "KITSU_CHILD_MODE": "inspect",
                "KITSU_HOST_PGID": host_pgid.to_string(),
                "KITSU_HOST_PID": host_pid.to_string(),
                "KITSU_INHERITED_FD": sockets[1].to_string(),
                "KITSU_PROJECTED_VALUE": "exact-value",
            })
        );
        assert_eq!(json["pid"], json["pgid"]);
        assert_ne!(json["pid"].as_i64().unwrap(), i64::from(host_pid));
        assert_eq!(json["ppid"].as_i64().unwrap(), i64::from(host_pid));
        assert_ne!(json["pgid"].as_i64().unwrap(), i64::from(host_pgid));
        assert_eq!(json["inherited_fd_errno"], libc::EBADF);
        assert_eq!(json["socket_errno"], libc::EPERM);
        assert_eq!(json["socketpair_errno"], libc::EPERM);
        assert_eq!(json["setsid_errno"], libc::EPERM);
        assert_eq!(json["setpgid_errno"], libc::EPERM);
        assert_eq!(json["unshare_errno"], libc::EPERM);
        assert_eq!(json["setns_errno"], libc::EPERM);
        assert_eq!(json["ordinary_thread_value"], 7);
        assert_eq!(json["clone3_errno"], libc::ENOSYS);
        assert_eq!(json["namespace_clone_errno"], libc::EPERM);
        assert_eq!(json["x32_socket_errno"], libc::ENOSYS);
        assert_eq!(json["no_new_privs"], 1);
        assert_eq!(json["cap_inheritable"], "0000000000000000");
        assert_eq!(json["cap_permitted"], "0000000000000000");
        assert_eq!(json["cap_effective"], "0000000000000000");
        assert_eq!(json["cap_ambient"], "0000000000000000");
        assert!(completion.stdout_eof() && completion.stderr_eof());
        assert!(completion.workers_joined());
        assert_eq!(completion.truth(), RealChildCompletionTruthV1::Success);
        assert_eq!(backend.os_spawn_count(), 1);
        assert_eq!(backend.request_start_count(), 1);

        // SAFETY: both descriptors remain owned by this fixture parent.
        unsafe {
            libc::close(sockets[0]);
            libc::close(sockets[1]);
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn bounded_stdin_round_trip_is_one_shot_and_requires_exit_plus_both_eofs() {
        let cwd = tempfile::tempdir().unwrap();
        let input = b"one bounded request\n".to_vec();
        let prepared = prepared_helper(cwd.path(), "echo", TestLimits::default(), Vec::new());
        let (terminal, mut backend) = drive_one_shot(prepared, input.clone());
        assert!(
            matches!(terminal, ChildPhaseSupervisorStateV1::Stopped { .. }),
            "unexpected terminal state: {terminal:?}"
        );
        let completion = backend.take_completion().unwrap();
        assert!(String::from_utf8_lossy(completion.stdout()).contains("one bounded request"));
        assert!(completion.stderr().is_empty());
        assert!(completion.stdout_eof() && completion.stderr_eof());
        assert_eq!(completion.truth(), RealChildCompletionTruthV1::Success);
        assert_eq!(backend.request_start_count(), 1);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn stdout_and_stderr_overflow_fail_closed_into_owned_joined_residue() {
        let cwd = tempfile::tempdir().unwrap();
        let limits = TestLimits {
            stdout_bytes: 128,
            stderr_bytes: 128,
            ..TestLimits::default()
        };
        let prepared = prepared_helper(cwd.path(), "flood", limits, Vec::new());
        let (terminal, mut backend) = drive_one_shot(prepared, Vec::new());
        assert!(matches!(
            terminal,
            ChildPhaseSupervisorStateV1::Failed(ChildPhaseSupervisorFailureV1::Effect { .. })
        ));
        assert!(backend.take_completion().is_none());
        let residue = backend.take_retained_residue().expect("typed residue");
        let cleanup = residue.cleanup().expect("owned residue cleanup");
        assert!(cleanup.group_gone());
        assert!(cleanup.direct_child_reaped());
        assert!(cleanup.workers_joined());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn crash_after_output_eof_is_never_success() {
        let cwd = tempfile::tempdir().unwrap();
        let prepared = prepared_helper(cwd.path(), "crash", TestLimits::default(), Vec::new());
        let (terminal, mut backend) = drive_one_shot(prepared, Vec::new());
        let completion = backend
            .take_completion()
            .unwrap_or_else(|| panic!("crash completion missing; terminal={terminal:?}"));
        assert!(completion.stdout_eof() && completion.stderr_eof());
        assert!(String::from_utf8_lossy(completion.stdout()).contains("response-before-crash"));
        assert_eq!(
            completion.truth(),
            RealChildCompletionTruthV1::Crash(ChildExitTruthV1::Exited(17))
        );
        assert!(!completion.is_success());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn response_line_and_idle_process_expire_real_monotonic_request_budget() {
        let cwd = tempfile::tempdir().unwrap();
        let limits = TestLimits {
            request_millis: 20,
            ..TestLimits::default()
        };
        let prepared = prepared_helper(cwd.path(), "response_then_hang", limits, Vec::new());
        let (terminal, mut backend) = drive_one_shot(prepared, Vec::new());
        assert!(
            matches!(
                terminal,
                ChildPhaseSupervisorStateV1::Failed(ChildPhaseSupervisorFailureV1::PhaseTimeout {
                    cause: ChildSupervisorCauseV1::RequestTimeout,
                    ..
                })
            ),
            "unexpected terminal state: {terminal:?}"
        );
        let completion = backend
            .take_completion()
            .expect("timed-out child was reaped");
        assert!(String::from_utf8_lossy(completion.stdout()).contains("early-response"));
        assert!(!completion.is_success());
        assert!(backend.take_retained_residue().is_none());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn cancel_and_shutdown_kill_group_reap_direct_child_and_join_workers() {
        for cause in [
            ChildSupervisorCauseV1::Cancel,
            ChildSupervisorCauseV1::Shutdown,
        ] {
            let cwd = tempfile::tempdir().unwrap();
            let prepared =
                prepared_helper(cwd.path(), "descendant", TestLimits::default(), Vec::new());
            let (terminal, mut backend) = drive_cancel_or_shutdown(prepared, cause);
            assert!(
                matches!(
                    terminal,
                    ChildPhaseSupervisorStateV1::Stopped {
                        cause: observed,
                        ..
                    } if observed == cause
                ),
                "unexpected {cause:?} terminal state: {terminal:?}"
            );
            let completion = backend.take_completion().expect("drained completion");
            let json = fixture_json(completion.stdout(), completion.stderr());
            let pgid = i32::try_from(json["pgid"].as_i64().unwrap()).unwrap();
            let descendant = i32::try_from(json["descendant"].as_i64().unwrap()).unwrap();
            assert!(process_group_is_gone(pgid));
            assert!(process_is_gone(descendant));
            assert!(completion.direct_child_reaped());
            assert!(completion.workers_joined());
            assert!(backend.take_retained_residue().is_none());
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn dropping_live_supervisor_transfers_and_cleans_all_owned_workers() {
        let cwd = tempfile::tempdir().unwrap();
        let prepared = prepared_helper(cwd.path(), "hang", TestLimits::default(), Vec::new());
        let mut backend = LinuxChildBackendV1::new(Vec::new());
        {
            let mut supervisor = ChildPhaseSupervisorV1::new(prepared, &mut backend);
            supervisor.start().unwrap();
        }
        let residue = backend.take_retained_residue().expect("drop residue");
        let cleanup = residue.cleanup().expect("drop residue cleanup");
        assert!(cleanup.group_gone());
        assert!(cleanup.direct_child_reaped());
        assert!(cleanup.workers_joined());
    }

    #[test]
    fn monotonic_duration_conversion_is_checked() {
        assert_eq!(checked_duration_millis_v1(Duration::from_millis(7)), Ok(7));
        assert!(checked_duration_millis_v1(Duration::from_secs(u64::MAX)).is_err());
    }

    #[test]
    fn real_child_fixture_entry() {
        let Ok(mode) = std::env::var("KITSU_CHILD_MODE") else {
            return;
        };
        match mode.as_str() {
            "inspect" => child_inspect(),
            "echo" => {
                let mut input = Vec::new();
                std::io::stdin().read_to_end(&mut input).unwrap();
                std::io::stdout().write_all(&input).unwrap();
            }
            "flood" => {
                std::io::stdout().write_all(&vec![b'o'; 64 * 1024]).unwrap();
                std::io::stderr().write_all(&vec![b'e'; 64 * 1024]).unwrap();
            }
            "crash" => {
                println!("response-before-crash");
                std::process::exit(17);
            }
            "response_then_hang" => {
                println!("early-response");
                std::io::stdout().flush().unwrap();
                loop {
                    std::thread::sleep(Duration::from_secs(1));
                }
            }
            "descendant" => child_with_descendant(),
            "hang" => loop {
                std::thread::sleep(Duration::from_secs(1));
            },
            "marker" => {
                std::fs::write(std::env::var("KITSU_MARKER_PATH").unwrap(), b"ran").unwrap();
            }
            other => panic!("unknown child fixture {other}"),
        }
    }

    fn child_inspect() {
        let ordinary_thread_value = std::thread::spawn(|| 7_u64).join().unwrap();
        let inherited_fd: RawFd = std::env::var("KITSU_INHERITED_FD")
            .unwrap()
            .parse()
            .unwrap();
        // SAFETY: these calls use scalar arguments and intentionally exercise
        // the child's syscall filter.
        let inherited_fd_result = unsafe { libc::fcntl(inherited_fd, libc::F_GETFD) };
        let inherited_fd_errno = if inherited_fd_result == -1 {
            errno()
        } else {
            0
        };
        let socket = unsafe { libc::socket(libc::AF_INET, libc::SOCK_STREAM, 0) };
        let socket_errno = if socket == -1 { errno() } else { 0 };
        let mut pair = [0_i32; 2];
        let socketpair =
            unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, pair.as_mut_ptr()) };
        let socketpair_errno = if socketpair == -1 { errno() } else { 0 };
        let setsid_errno = if unsafe { libc::setsid() } == -1 {
            errno()
        } else {
            0
        };
        let setpgid_errno = if unsafe { libc::setpgid(0, 0) } == -1 {
            errno()
        } else {
            0
        };
        let unshare_errno = if unsafe { libc::unshare(0) } == -1 {
            errno()
        } else {
            0
        };
        let setns_errno = if unsafe { libc::setns(-1, 0) } == -1 {
            errno()
        } else {
            0
        };
        let clone3_errno = if unsafe {
            libc::syscall(libc::SYS_clone3, std::ptr::null::<libc::c_void>(), 0_usize)
        } == -1
        {
            errno()
        } else {
            0
        };
        let x32_socket_errno = if unsafe {
            libc::syscall(
                libc::SYS_socket | 0x4000_0000,
                libc::AF_INET,
                libc::SOCK_STREAM,
                0,
            )
        } == -1
        {
            errno()
        } else {
            0
        };
        let namespace_clone = unsafe {
            libc::syscall(
                libc::SYS_clone,
                libc::CLONE_NEWUSER | libc::SIGCHLD,
                0_usize,
                0_usize,
                0_usize,
                0_usize,
            )
        };
        let namespace_clone_errno = if namespace_clone == -1 {
            errno()
        } else {
            if namespace_clone == 0 {
                unsafe { libc::_exit(91) };
            }
            let mut status = 0;
            unsafe { libc::waitpid(namespace_clone as libc::pid_t, &mut status, 0) };
            0
        };
        let status = std::fs::read_to_string("/proc/self/status").unwrap();
        let status_fields: BTreeMap<_, _> = status
            .lines()
            .filter_map(|line| line.split_once(':'))
            .map(|(name, value)| (name, value.trim()))
            .collect();
        let environment: BTreeMap<_, _> = std::env::vars().collect();
        let result = serde_json::json!({
            "argv": std::env::args().collect::<Vec<_>>(),
            "cwd": std::env::current_dir().unwrap(),
            "environment": environment,
            "pid": unsafe { libc::getpid() },
            "ppid": unsafe { libc::getppid() },
            "pgid": unsafe { libc::getpgrp() },
            "inherited_fd_errno": inherited_fd_errno,
            "socket_errno": socket_errno,
            "socketpair_errno": socketpair_errno,
            "setsid_errno": setsid_errno,
            "setpgid_errno": setpgid_errno,
            "unshare_errno": unshare_errno,
            "setns_errno": setns_errno,
            "ordinary_thread_value": ordinary_thread_value,
            "clone3_errno": clone3_errno,
            "namespace_clone_errno": namespace_clone_errno,
            "x32_socket_errno": x32_socket_errno,
            "no_new_privs": status_fields["NoNewPrivs"].parse::<u64>().unwrap(),
            "cap_inheritable": status_fields["CapInh"],
            "cap_permitted": status_fields["CapPrm"],
            "cap_effective": status_fields["CapEff"],
            "cap_ambient": status_fields["CapAmb"],
        });
        println!("KITSU_FIXTURE_JSON={result}");
    }

    fn child_with_descendant() {
        // SAFETY: the fixture immediately enters simple async-signal-safe child
        // work after fork and does not share mutable Rust state across paths.
        let descendant = unsafe { libc::fork() };
        assert!(descendant >= 0);
        if descendant == 0 {
            loop {
                unsafe { libc::pause() };
            }
        }
        let result = serde_json::json!({
            "pid": unsafe { libc::getpid() },
            "pgid": unsafe { libc::getpgrp() },
            "descendant": descendant,
        });
        println!("KITSU_FIXTURE_JSON={result}");
        std::io::stdout().flush().unwrap();
        loop {
            unsafe { libc::pause() };
        }
    }

    fn errno() -> i32 {
        std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
    }

    fn process_group_is_gone(pgid: i32) -> bool {
        // SAFETY: a zero signal performs only a kernel liveness query.
        unsafe { libc::kill(-pgid, 0) == -1 && errno() == libc::ESRCH }
    }

    fn process_is_gone(pid: i32) -> bool {
        // SAFETY: a zero signal performs only a kernel liveness query.
        unsafe { libc::kill(pid, 0) == -1 && errno() == libc::ESRCH }
    }
}
