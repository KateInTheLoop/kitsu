//! Private Linux process backend for an authorized external child.
//!
//! Linux x86-64 preparation copies the reviewed native ELF into a sealed
//! executable memfd and builds a Landlock ruleset for exact reviewed roots.
//! Spawn consumes those descriptors and uses `fchdir` plus `execveat`; there is
//! no path fallback. This closes mutable launch bytes and the Landlock-governed
//! content operations, but it is not a complete filesystem sandbox: Landlock
//! does not hide paths or govern stat/access, chmod/chown/xattrs/timestamps,
//! flock/fcntl, and this backend supplies no mount namespace complement. Host
//! signal and ptrace authority is also outside this card. It therefore remains
//! private and unmounted from MCP/product wiring. The serial transport below
//! is an opaque bounded line mechanism; it is not an MCP codec or concurrent
//! request multiplexer.

#![allow(dead_code)]

use std::ffi::CString;
use std::fmt;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::mem::MaybeUninit;
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use ring::digest::{Context, SHA256};

use super::authority::{
    AdmittedChildLaunchV1, ChildFilesystemAccessV1, ChildLaunchPreparationV1, ChildRequestPolicyV1,
    PreparedChildContainmentTokenV1, PreparedChildLaunchV1, PreparedChildOsLaunchV1,
};
use super::lifecycle::ChildExitTruthV1;
use super::serial::{
    SerialChildCauseV1, SerialChildTransportEffectsV1, SerialFrameObservationV1,
    SerialIoProgressV1, SerialOutputStatusV1, SerialOwnedChildResidueV1, SerialPollAllowanceV1,
};
use super::supervisor::{
    ChildPhaseSupervisorEffectsV1, ChildReapPollV1, ChildSupervisorCauseV1, OwnedChildResidueV1,
    OwnedChildTokenV1,
};

const CHILD_TOKEN_ID: u64 = 1;
const PIPE_CHUNK_BYTES: usize = 8 * 1024;
const MAX_EXECUTABLE_BYTES_V1: u64 = 192 * 1024 * 1024;
const REQUIRED_LANDLOCK_ABI_V1: i32 = 3;
const MFD_EXEC_V1: libc::c_uint = 0x0010;
const AT_EMPTY_PATH_V1: libc::c_int = 0x1000;

const LANDLOCK_CREATE_RULESET_VERSION_V1: u32 = 1;
const LANDLOCK_RULE_PATH_BENEATH_V1: u32 = 1;
const LANDLOCK_ACCESS_FS_EXECUTE_V1: u64 = 1 << 0;
const LANDLOCK_ACCESS_FS_WRITE_FILE_V1: u64 = 1 << 1;
const LANDLOCK_ACCESS_FS_READ_FILE_V1: u64 = 1 << 2;
const LANDLOCK_ACCESS_FS_READ_DIR_V1: u64 = 1 << 3;
const LANDLOCK_ACCESS_FS_REMOVE_DIR_V1: u64 = 1 << 4;
const LANDLOCK_ACCESS_FS_REMOVE_FILE_V1: u64 = 1 << 5;
const LANDLOCK_ACCESS_FS_MAKE_CHAR_V1: u64 = 1 << 6;
const LANDLOCK_ACCESS_FS_MAKE_DIR_V1: u64 = 1 << 7;
const LANDLOCK_ACCESS_FS_MAKE_REG_V1: u64 = 1 << 8;
const LANDLOCK_ACCESS_FS_MAKE_SOCK_V1: u64 = 1 << 9;
const LANDLOCK_ACCESS_FS_MAKE_FIFO_V1: u64 = 1 << 10;
const LANDLOCK_ACCESS_FS_MAKE_BLOCK_V1: u64 = 1 << 11;
const LANDLOCK_ACCESS_FS_MAKE_SYM_V1: u64 = 1 << 12;
const LANDLOCK_ACCESS_FS_REFER_V1: u64 = 1 << 13;
const LANDLOCK_ACCESS_FS_TRUNCATE_V1: u64 = 1 << 14;
const LANDLOCK_ACCESS_FS_IOCTL_DEV_V1: u64 = 1 << 15;
static NEXT_PREPARED_CONTAINMENT_TOKEN_ID_V1: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChildFilesystemAuthorityNonclaimV1 {
    PathVisibilityStatAndAccess,
    MetadataAndExtendedAttributes,
    FileLocksAndDescriptorControl,
    MountNamespaceIsolation,
    HostSignalsAndPtrace,
}

fn filesystem_authority_nonclaims_v1() -> &'static [ChildFilesystemAuthorityNonclaimV1] {
    &[
        ChildFilesystemAuthorityNonclaimV1::PathVisibilityStatAndAccess,
        ChildFilesystemAuthorityNonclaimV1::MetadataAndExtendedAttributes,
        ChildFilesystemAuthorityNonclaimV1::FileLocksAndDescriptorControl,
        ChildFilesystemAuthorityNonclaimV1::MountNamespaceIsolation,
        ChildFilesystemAuthorityNonclaimV1::HostSignalsAndPtrace,
    ]
}

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FilesystemPreparationSetupV1 {
    Enforce,
    UnsupportedLandlockAbi,
    MemfdUnavailable,
    SealRejected,
    RulesetRejected,
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TestFilesystemPreparationSetupV1 {
    UnsupportedLandlockAbi,
    MemfdUnavailable,
    SealRejected,
    RulesetRejected,
}

struct PreparedChildContainmentV1 {
    seccomp_filter: Box<[libc::sock_filter]>,
    seccomp_filter_len: u16,
}

#[repr(C)]
struct LandlockRulesetAttrV1 {
    handled_access_fs: u64,
}

#[repr(C, packed)]
struct LandlockPathBeneathAttrV1 {
    allowed_access: u64,
    parent_fd: i32,
}

struct PendingExecutableImageV1 {
    image: File,
    source_path: PathBuf,
    digest: [u8; 32],
    seals: i32,
    header: [u8; 64],
    header_len: usize,
}

struct PreparedFilesystemContainmentV1 {
    token_id: u64,
    executable_image: File,
    cwd: File,
    landlock_ruleset: File,
    executable_seals: i32,
}

struct PreparedExecVectorsV1 {
    _argv_storage: Box<[CString]>,
    argv_addresses: Box<[usize]>,
    _environment_storage: Box<[CString]>,
    environment_addresses: Box<[usize]>,
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

struct ActiveSerialChildV1 {
    child: Child,
    pgid: i32,
    phase_budgets_millis: [u64; 5],
    stdin: Option<ChildStdin>,
    stdout: Option<ChildStdout>,
    stderr: Option<ChildStderr>,
    pending_stdin: Vec<u8>,
    pending_stdin_offset: usize,
    stdout_carry: Vec<u8>,
    io_limits: [usize; 4],
    max_frames: u16,
    frame_count: u16,
    stdin_accepted: usize,
    stdout_accepted: usize,
    stderr_accepted: usize,
    exit_observed: bool,
    observed_exit_truth: Option<ChildExitTruthV1>,
    exit_status: Option<ExitStatus>,
}

impl ActiveSerialChildV1 {
    fn observe_child_exit(&mut self) -> Result<(), String> {
        if !self.exit_observed {
            self.observed_exit_truth = child_exit_observation(self.child.id())?;
            self.exit_observed = self.observed_exit_truth.is_some();
        }
        Ok(())
    }

    fn force_cleanup(&mut self) -> RealChildCleanupTruthV1 {
        self.stdin.take();
        self.stdout.take();
        self.stderr.take();
        if self.exit_status.is_none() {
            let _ = signal_process_group(self.pgid, libc::SIGKILL);
            self.exit_status = self.child.wait().ok();
        }
        let group_deadline = Instant::now()
            .checked_add(Duration::from_millis(self.phase_budgets_millis[4]))
            .unwrap_or_else(Instant::now);
        while !process_group_is_gone(self.pgid) && Instant::now() < group_deadline {
            thread::yield_now();
        }
        RealChildCleanupTruthV1 {
            group_gone: process_group_is_gone(self.pgid),
            direct_child_reaped: self.exit_status.is_some(),
            workers_joined: true,
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

struct RealSerialOwnedResidueV1 {
    _transport_residue: SerialOwnedChildResidueV1,
    child: ActiveSerialChildV1,
    cleaned: bool,
}

impl fmt::Debug for RealSerialOwnedResidueV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RealSerialOwnedResidueV1")
            .field("pgid", &self.child.pgid)
            .field("cleaned", &self.cleaned)
            .finish_non_exhaustive()
    }
}

impl RealSerialOwnedResidueV1 {
    fn cleanup(mut self) -> Result<RealChildCleanupTruthV1, Box<Self>> {
        let truth = self.child.force_cleanup();
        if truth.group_gone && truth.direct_child_reaped {
            self.cleaned = true;
            Ok(truth)
        } else {
            Err(Box::new(self))
        }
    }
}

impl Drop for RealSerialOwnedResidueV1 {
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
    filesystem_setup: FilesystemPreparationSetupV1,
    pending_source: Option<(File, PathBuf)>,
    pending_image: Option<PendingExecutableImageV1>,
    prepared_filesystem: Option<PreparedFilesystemContainmentV1>,
    active: Option<ActiveChildV1>,
    serial_active: Option<ActiveSerialChildV1>,
    completion: Option<RealChildCompletionV1>,
    retained_residue: Option<RealChildOwnedResidueV1>,
    serial_retained_residue: Option<RealSerialOwnedResidueV1>,
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
            filesystem_setup: FilesystemPreparationSetupV1::Enforce,
            pending_source: None,
            pending_image: None,
            prepared_filesystem: None,
            active: None,
            serial_active: None,
            completion: None,
            retained_residue: None,
            serial_retained_residue: None,
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

    #[cfg(test)]
    fn new_with_filesystem_setup_for_test(
        stdin_frame: Vec<u8>,
        setup: TestFilesystemPreparationSetupV1,
    ) -> Self {
        let mut backend = Self::new(stdin_frame);
        backend.filesystem_setup = match setup {
            TestFilesystemPreparationSetupV1::UnsupportedLandlockAbi => {
                FilesystemPreparationSetupV1::UnsupportedLandlockAbi
            }
            TestFilesystemPreparationSetupV1::MemfdUnavailable => {
                FilesystemPreparationSetupV1::MemfdUnavailable
            }
            TestFilesystemPreparationSetupV1::SealRejected => {
                FilesystemPreparationSetupV1::SealRejected
            }
            TestFilesystemPreparationSetupV1::RulesetRejected => {
                FilesystemPreparationSetupV1::RulesetRejected
            }
        };
        backend
    }

    fn os_spawn_count(&self) -> usize {
        self.os_spawn_count
    }

    fn request_start_count(&self) -> usize {
        self.request_start_count
    }

    #[cfg(test)]
    fn prepared_containment_count_for_test(&self) -> usize {
        usize::from(self.prepared_filesystem.is_some())
    }

    #[cfg(test)]
    fn prepared_executable_seals_for_test(&self) -> Option<i32> {
        self.prepared_filesystem
            .as_ref()
            .map(|prepared| prepared.executable_seals)
    }

    fn take_completion(&mut self) -> Option<RealChildCompletionV1> {
        self.completion.take()
    }

    fn take_retained_residue(&mut self) -> Option<RealChildOwnedResidueV1> {
        self.retained_residue.take()
    }

    fn take_serial_retained_residue(&mut self) -> Option<RealSerialOwnedResidueV1> {
        self.serial_retained_residue.take()
    }

    #[cfg(test)]
    fn active_pgid_for_test(&self) -> Option<i32> {
        self.serial_active.as_ref().map(|child| child.pgid)
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

    fn serial_active_for(&self, child: &OwnedChildTokenV1) -> Result<&ActiveSerialChildV1, String> {
        if child.id() != CHILD_TOKEN_ID {
            return Err("unknown serial child ownership token".to_string());
        }
        self.serial_active
            .as_ref()
            .ok_or_else(|| "owned serial child is not active".to_string())
    }

    fn serial_active_for_mut(
        &mut self,
        child: &OwnedChildTokenV1,
    ) -> Result<&mut ActiveSerialChildV1, String> {
        if child.id() != CHILD_TOKEN_ID {
            return Err("unknown serial child ownership token".to_string());
        }
        self.serial_active
            .as_mut()
            .ok_or_else(|| "owned serial child is not active".to_string())
    }
}

impl Drop for LinuxChildBackendV1 {
    fn drop(&mut self) {
        if let Some(mut child) = self.active.take() {
            let _ = child.force_cleanup();
        }
        if let Some(mut child) = self.serial_active.take() {
            let _ = child.force_cleanup();
        }
        if let Some(residue) = self.retained_residue.take() {
            drop(residue);
        }
        if let Some(residue) = self.serial_retained_residue.take() {
            drop(residue);
        }
    }
}

impl ChildLaunchPreparationV1 for LinuxChildBackendV1 {
    fn canonicalize_executable(&mut self, executable: &Path) -> Result<PathBuf, String> {
        if self.pending_source.is_some()
            || self.pending_image.is_some()
            || self.prepared_filesystem.is_some()
        {
            return Err("backend permits exactly one executable preparation".to_string());
        }
        let (source, observed_path) = open_exact_regular_executable(executable)
            .map_err(|error| format!("cannot pin exact executable: {error}"))?;
        self.pending_source = Some((source, observed_path.clone()));
        Ok(observed_path)
    }

    fn executable_sha256(&mut self, executable: &Path) -> Result<[u8; 32], String> {
        let (source, observed_path) = self
            .pending_source
            .take()
            .ok_or_else(|| "executable was not pinned before hashing".to_string())?;
        if observed_path != executable {
            return Err("pinned executable identity changed before hashing".to_string());
        }
        let image = stage_executable_image(source, observed_path)
            .map_err(|error| format!("cannot stage immutable executable: {error}"))?;
        let digest = image.digest;
        self.pending_image = Some(image);
        Ok(digest)
    }

    fn containment_preflight(
        &mut self,
        launch: &AdmittedChildLaunchV1,
    ) -> Result<PreparedChildContainmentTokenV1, String> {
        if self.prepared_filesystem.is_some() {
            return Err("backend permits exactly one prepared containment".to_string());
        }
        let image = self
            .pending_image
            .take()
            .ok_or_else(|| "immutable executable image was not prepared".to_string())?;
        if image.source_path != launch.executable() || &image.digest != launch.executable_sha256() {
            return Err("immutable executable image does not match admitted identity".to_string());
        }
        validate_native_elf_x86_64(&image.header[..image.header_len])
            .map_err(|error| error.to_string())?;
        match self.filesystem_setup {
            FilesystemPreparationSetupV1::UnsupportedLandlockAbi => {
                return Err("required Landlock ABI is unavailable".to_string())
            }
            FilesystemPreparationSetupV1::MemfdUnavailable => {
                return Err("executable memfd is unavailable".to_string())
            }
            FilesystemPreparationSetupV1::SealRejected => {
                return Err("executable seals were rejected".to_string())
            }
            FilesystemPreparationSetupV1::RulesetRejected => {
                return Err("Landlock ruleset was rejected".to_string())
            }
            FilesystemPreparationSetupV1::Enforce => {}
        }
        let token_id = allocate_prepared_containment_token_id_v1()?;
        let prepared = prepare_filesystem_containment(launch, image, token_id)
            .map_err(|error| format!("cannot prepare filesystem containment: {error}"))?;
        self.prepared_filesystem = Some(prepared);
        Ok(PreparedChildContainmentTokenV1::new(token_id))
    }
}

impl ChildPhaseSupervisorEffectsV1 for LinuxChildBackendV1 {
    fn monotonic_millis(&mut self) -> Result<u64, String> {
        checked_duration_millis_v1(self.started_at.elapsed())
    }

    fn spawn(&mut self, launch: PreparedChildLaunchV1) -> Result<OwnedChildTokenV1, String> {
        if self.active.is_some() || self.serial_active.is_some() || self.os_spawn_count != 0 {
            return Err("backend permits exactly one OS spawn".to_string());
        }
        let prepared = self
            .prepared_filesystem
            .take()
            .filter(|prepared| prepared.token_id == launch.containment_token().id())
            .ok_or_else(|| "launch has no matching prepared containment token".to_string())?;
        let launch = launch.into_os_launch();
        if launch.request_policy() != ChildRequestPolicyV1::OneShot {
            return Err("one-shot backend requires an authorized one-shot policy".to_string());
        }
        let stdin_len = self.stdin_frame.as_ref().map_or(0, Vec::len);
        if stdin_len > launch.stdin_limit() || stdin_len > launch.frame_limit() {
            return Err("stdin frame exceeds its authorized byte limit".to_string());
        }
        let mut child = spawn_child(&launch, prepared, self.containment_setup)?;
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
            return Err(cleanup_spawn_failure(
                &mut child,
                pgid,
                io::Error::other(detail),
            ));
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

impl SerialChildTransportEffectsV1 for LinuxChildBackendV1 {
    fn monotonic_millis(&mut self) -> Result<u64, String> {
        checked_duration_millis_v1(self.started_at.elapsed())
    }

    fn spawn_serial(&mut self, launch: PreparedChildLaunchV1) -> Result<OwnedChildTokenV1, String> {
        if self.active.is_some() || self.serial_active.is_some() || self.os_spawn_count != 0 {
            return Err("backend permits exactly one OS spawn".to_string());
        }
        let prepared = self
            .prepared_filesystem
            .take()
            .filter(|prepared| prepared.token_id == launch.containment_token().id())
            .ok_or_else(|| "launch has no matching prepared containment token".to_string())?;
        let launch = launch.into_os_launch();
        let ChildRequestPolicyV1::Serial {
            max_frames,
            idle_millis: _,
        } = launch.request_policy()
        else {
            return Err("serial backend requires an authorized serial policy".to_string());
        };
        let mut child = spawn_child(&launch, prepared, self.containment_setup)?;
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
            return Err(cleanup_spawn_failure(
                &mut child,
                pgid,
                io::Error::other(detail),
            ));
        }
        let (stdin, stdout, stderr) =
            match (child.stdin.take(), child.stdout.take(), child.stderr.take()) {
                (Some(stdin), Some(stdout), Some(stderr)) => (stdin, stdout, stderr),
                pipes => {
                    drop(pipes);
                    return Err(cleanup_spawn_failure(
                        &mut child,
                        pgid,
                        io::Error::other("spawned serial child is missing an owned stdio pipe"),
                    ));
                }
            };
        if let Err(error) = set_nonblocking_stdio(&stdin, &stdout, &stderr) {
            drop((stdin, stdout, stderr));
            return Err(cleanup_spawn_failure(&mut child, pgid, error));
        }
        self.serial_active = Some(ActiveSerialChildV1 {
            child,
            pgid,
            phase_budgets_millis: launch.phase_budgets_millis(),
            stdin: Some(stdin),
            stdout: Some(stdout),
            stderr: Some(stderr),
            pending_stdin: Vec::new(),
            pending_stdin_offset: 0,
            stdout_carry: Vec::new(),
            io_limits: [
                launch.stdin_limit(),
                launch.stdout_limit(),
                launch.stderr_limit(),
                launch.frame_limit(),
            ],
            max_frames,
            frame_count: 0,
            stdin_accepted: 0,
            stdout_accepted: 0,
            stderr_accepted: 0,
            exit_observed: false,
            observed_exit_truth: None,
            exit_status: None,
        });
        Ok(OwnedChildTokenV1::new(CHILD_TOKEN_ID))
    }

    fn startup_ready(&mut self, child: &OwnedChildTokenV1) -> Result<bool, String> {
        self.serial_active_for(child)?;
        Ok(true)
    }

    fn begin_frame(&mut self, child: &OwnedChildTokenV1, frame: &[u8]) -> Result<(), String> {
        let next_request_count = self
            .request_start_count
            .checked_add(1)
            .ok_or_else(|| "serial request count overflow".to_string())?;
        let active = self.serial_active_for_mut(child)?;
        if !active.pending_stdin.is_empty() || !active.stdout_carry.is_empty() {
            return Err("serial backend already owns in-flight frame bytes".to_string());
        }
        if active.frame_count >= active.max_frames {
            return Err("serial backend frame count is exhausted".to_string());
        }
        let next_frame_count = active
            .frame_count
            .checked_add(1)
            .ok_or_else(|| "serial frame count overflow".to_string())?;
        if frame.is_empty()
            || frame.len() > active.io_limits[3]
            || !frame.ends_with(b"\n")
            || frame[..frame.len() - 1].contains(&b'\n')
        {
            return Err("serial frame violates its authorized line bound".to_string());
        }
        let next_stdin = active
            .stdin_accepted
            .checked_add(frame.len())
            .ok_or_else(|| "serial stdin byte count overflow".to_string())?;
        if next_stdin > active.io_limits[0] {
            return Err("serial stdin exceeds its authorized cumulative limit".to_string());
        }
        active.pending_stdin.extend_from_slice(frame);
        active.pending_stdin_offset = 0;
        active.stdin_accepted = next_stdin;
        active.frame_count = next_frame_count;
        self.request_start_count = next_request_count;
        Ok(())
    }

    fn poll_frame(
        &mut self,
        child: &OwnedChildTokenV1,
        allowance: SerialPollAllowanceV1,
    ) -> Result<SerialFrameObservationV1, String> {
        let active = self.serial_active_for_mut(child)?;
        let exact_allowance = SerialPollAllowanceV1 {
            stdin_bytes: active.io_limits[0].saturating_sub(active.stdin_accepted),
            stdout_bytes: active.io_limits[1].saturating_sub(active.stdout_accepted),
            stderr_bytes: active.io_limits[2].saturating_sub(active.stderr_accepted),
            frame_bytes: active.io_limits[3],
        };
        if allowance != exact_allowance {
            return Err("serial poll allowance diverged from the prepared policy".to_string());
        }

        let stdin_bytes_written = write_serial_pending(active)?;
        let stdout = read_nonblocking_available(
            active
                .stdout
                .as_mut()
                .ok_or_else(|| "serial stdout is closed".to_string())?,
            exact_allowance.stdout_bytes,
        )?;
        if stdout.overflow {
            return Ok(SerialFrameObservationV1::limit_exceeded(
                super::authority::ChildIoLimitV1::Stdout,
            ));
        }
        let stderr = read_nonblocking_available(
            active
                .stderr
                .as_mut()
                .ok_or_else(|| "serial stderr is closed".to_string())?,
            exact_allowance.stderr_bytes,
        )?;
        if stderr.overflow {
            return Ok(SerialFrameObservationV1::limit_exceeded(
                super::authority::ChildIoLimitV1::Stderr,
            ));
        }
        active.stdout_accepted = active
            .stdout_accepted
            .checked_add(stdout.bytes.len())
            .ok_or_else(|| "serial stdout byte count overflow".to_string())?;
        active.stderr_accepted = active
            .stderr_accepted
            .checked_add(stderr.bytes.len())
            .ok_or_else(|| "serial stderr byte count overflow".to_string())?;

        let stdout_bytes_read = stdout.bytes.len();
        let stderr_bytes_read = stderr.bytes.len();
        let (response, extra_stdout_bytes) =
            if let Some(newline) = stdout.bytes.iter().position(|byte| *byte == b'\n') {
                let response_len = active
                    .stdout_carry
                    .len()
                    .checked_add(newline + 1)
                    .ok_or_else(|| "serial frame byte count overflow".to_string())?;
                if response_len > active.io_limits[3] {
                    return Ok(SerialFrameObservationV1::limit_exceeded(
                        super::authority::ChildIoLimitV1::Frame,
                    ));
                }
                let extra = stdout.bytes.len() - newline - 1;
                let mut response = std::mem::take(&mut active.stdout_carry);
                response.extend_from_slice(&stdout.bytes[..=newline]);
                (Some(response), extra)
            } else {
                let carry_len = active
                    .stdout_carry
                    .len()
                    .checked_add(stdout.bytes.len())
                    .ok_or_else(|| "serial frame byte count overflow".to_string())?;
                if carry_len > active.io_limits[3] {
                    return Ok(SerialFrameObservationV1::limit_exceeded(
                        super::authority::ChildIoLimitV1::Frame,
                    ));
                }
                active.stdout_carry.extend_from_slice(&stdout.bytes);
                (None, 0)
            };
        active.observe_child_exit()?;
        let exit = active.observed_exit_truth;
        Ok(SerialFrameObservationV1::from_backend(
            SerialIoProgressV1 {
                stdin_bytes_written,
                stdout_bytes_read,
                stderr_bytes_read,
            },
            SerialOutputStatusV1 {
                response,
                extra_stdout_bytes,
                stdout_eof: stdout.eof,
                stderr_eof: stderr.eof,
                exit,
            },
        ))
    }

    fn begin_drain(
        &mut self,
        child: &OwnedChildTokenV1,
        _: SerialChildCauseV1,
    ) -> Result<(), String> {
        let active = self.serial_active_for_mut(child)?;
        active.stdin.take();
        active.pending_stdin.clear();
        active.pending_stdin_offset = 0;
        Ok(())
    }

    fn drain_complete(&mut self, child: &OwnedChildTokenV1) -> Result<bool, String> {
        let active = self.serial_active_for_mut(child)?;
        active.observe_child_exit()?;
        Ok(active.exit_observed)
    }

    fn terminate_process_group(&mut self, child: &OwnedChildTokenV1) -> Result<(), String> {
        let active = self.serial_active_for(child)?;
        if active.exit_status.is_some() {
            return Err("refusing group signal after direct-child reap".to_string());
        }
        signal_process_group(active.pgid, libc::SIGKILL)
    }

    fn process_group_terminated(&mut self, child: &OwnedChildTokenV1) -> Result<bool, String> {
        let active = self.serial_active_for_mut(child)?;
        active.observe_child_exit()?;
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
                detail: "unknown serial child ownership token".to_string(),
            };
        }
        let Some(active) = self.serial_active.as_mut() else {
            return ChildReapPollV1::Failed {
                child,
                detail: "owned serial child is not active".to_string(),
            };
        };
        if let Err(detail) = active.observe_child_exit() {
            return ChildReapPollV1::Failed { child, detail };
        }
        if !active.exit_observed {
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
        let mut active = self.serial_active.take().expect("checked serial child");
        active.stdin.take();
        active.stdout.take();
        active.stderr.take();
        ChildReapPollV1::Reaped(exit_truth(
            active.exit_status.expect("checked serial exit status"),
        ))
    }

    fn retain_unstarted_launch(&mut self, launch: PreparedChildLaunchV1) {
        self.retained_unstarted.push(launch);
    }

    fn retain_owned_residue(&mut self, residue: SerialOwnedChildResidueV1) {
        assert_eq!(residue.child_id(), CHILD_TOKEN_ID);
        let child = self
            .serial_active
            .take()
            .expect("a serial residue corresponds to one active OS child");
        assert!(
            self.serial_retained_residue.is_none(),
            "only one serial residue is owned"
        );
        self.serial_retained_residue = Some(RealSerialOwnedResidueV1 {
            _transport_residue: residue,
            child,
            cleaned: false,
        });
    }
}

struct NonblockingReadV1 {
    bytes: Vec<u8>,
    eof: bool,
    overflow: bool,
}

fn set_nonblocking_stdio(
    stdin: &ChildStdin,
    stdout: &ChildStdout,
    stderr: &ChildStderr,
) -> io::Result<()> {
    for fd in [stdin.as_raw_fd(), stdout.as_raw_fd(), stderr.as_raw_fd()] {
        // SAFETY: each fd is live and uniquely owned by the active child state.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: F_SETFL updates only the status flags of the same live fd.
        if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

fn write_serial_pending(active: &mut ActiveSerialChildV1) -> Result<usize, String> {
    if active.pending_stdin_offset == active.pending_stdin.len() {
        active.pending_stdin.clear();
        active.pending_stdin_offset = 0;
        return Ok(0);
    }
    let stdin = active
        .stdin
        .as_mut()
        .ok_or_else(|| "serial stdin is closed".to_string())?;
    match stdin.write(&active.pending_stdin[active.pending_stdin_offset..]) {
        Ok(written) => {
            active.pending_stdin_offset += written;
            if active.pending_stdin_offset == active.pending_stdin.len() {
                active.pending_stdin.clear();
                active.pending_stdin_offset = 0;
            }
            Ok(written)
        }
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(0),
        Err(error) if error.kind() == io::ErrorKind::Interrupted => Ok(0),
        Err(error) => Err(error.to_string()),
    }
}

fn read_nonblocking_available<R: Read>(
    reader: &mut R,
    remaining: usize,
) -> Result<NonblockingReadV1, String> {
    let detect_limit = remaining.saturating_add(1);
    let mut bytes = Vec::with_capacity(detect_limit.min(PIPE_CHUNK_BYTES));
    let mut eof = false;
    for _ in 0..64 {
        if bytes.len() == detect_limit {
            break;
        }
        let mut buffer = [0_u8; PIPE_CHUNK_BYTES];
        let read_cap = (detect_limit - bytes.len()).min(buffer.len());
        match reader.read(&mut buffer[..read_cap]) {
            Ok(0) => {
                eof = true;
                break;
            }
            Ok(read) => bytes.extend_from_slice(&buffer[..read]),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error.to_string()),
        }
    }
    let overflow = bytes.len() > remaining;
    if overflow {
        bytes.truncate(remaining);
    }
    Ok(NonblockingReadV1 {
        bytes,
        eof,
        overflow,
    })
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

#[cfg(target_arch = "x86_64")]
fn open_exact_regular_executable(path: &Path) -> io::Result<(File, PathBuf)> {
    let file = open_exact_descriptor(path, libc::O_RDONLY, false)?;
    let metadata = file.metadata()?;
    if !metadata.file_type().is_file() || metadata.permissions().mode() & 0o111 == 0 {
        return Err(io::Error::from_raw_os_error(libc::EACCES));
    }
    Ok((file, path.to_path_buf()))
}

#[cfg(not(target_arch = "x86_64"))]
fn open_exact_regular_executable(_: &Path) -> io::Result<(File, PathBuf)> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "external-child preparation is unsupported on this architecture",
    ))
}

#[cfg(target_arch = "x86_64")]
fn open_exact_descriptor(path: &Path, access: i32, directory: bool) -> io::Result<File> {
    let path_c = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;
    let directory_flag = if directory { libc::O_DIRECTORY } else { 0 };
    // SAFETY: `path_c` is a bounded NUL-terminated path. The returned fd is
    // uniquely adopted below on success.
    let fd = unsafe {
        libc::open(
            path_c.as_ptr(),
            access | directory_flag | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful open returns one newly owned descriptor.
    let file = unsafe { File::from_raw_fd(fd) };
    let metadata = file.metadata()?;
    if metadata.file_type().is_symlink() {
        return Err(io::Error::from_raw_os_error(libc::ELOOP));
    }
    let observed = std::fs::read_link(format!("/proc/self/fd/{fd}"))?;
    if observed != path {
        return Err(io::Error::from_raw_os_error(libc::ESTALE));
    }
    Ok(file)
}

#[cfg(target_arch = "x86_64")]
fn stage_executable_image(
    mut source: File,
    source_path: PathBuf,
) -> io::Result<PendingExecutableImageV1> {
    let source_size = source.metadata()?.len();
    if source_size == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "executable image is empty",
        ));
    }
    if source_size > MAX_EXECUTABLE_BYTES_V1 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "executable exceeds the bounded image cap",
        ));
    }
    source.seek(SeekFrom::Start(0))?;
    let name = b"kitsu-child-image\0";
    let flags = libc::MFD_ALLOW_SEALING | libc::MFD_CLOEXEC | MFD_EXEC_V1;
    // SAFETY: memfd_create reads the static NUL-terminated name and returns a
    // new descriptor on success.
    let fd = unsafe { libc::syscall(libc::SYS_memfd_create, name.as_ptr(), flags) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let raw_fd = i32::try_from(fd).map_err(|_| io::Error::from_raw_os_error(libc::EOVERFLOW))?;
    // SAFETY: successful memfd_create returns one newly owned descriptor.
    let mut image = unsafe { File::from_raw_fd(raw_fd) };
    let mut context = Context::new(&SHA256);
    let mut header = [0_u8; 64];
    let header_len = source.read(&mut header)?;
    context.update(&header[..header_len]);
    image.write_all(&header[..header_len])?;
    let mut copied = u64::try_from(header_len).expect("ELF header length fits u64");
    let mut buffer = [0_u8; 16 * 1024];
    loop {
        let read = source.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        copied = copied
            .checked_add(u64::try_from(read).expect("buffer length fits u64"))
            .ok_or_else(|| io::Error::from_raw_os_error(libc::EFBIG))?;
        if copied > MAX_EXECUTABLE_BYTES_V1 {
            return Err(io::Error::from_raw_os_error(libc::EFBIG));
        }
        context.update(&buffer[..read]);
        image.write_all(&buffer[..read])?;
    }
    if copied != source_size {
        return Err(io::Error::from_raw_os_error(libc::ESTALE));
    }
    // SAFETY: fchmod changes only the newly owned memfd mode.
    if unsafe { libc::fchmod(image.as_raw_fd(), 0o500) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let required_seals =
        libc::F_SEAL_WRITE | libc::F_SEAL_GROW | libc::F_SEAL_SHRINK | libc::F_SEAL_SEAL;
    // SAFETY: fcntl applies and queries seals on the owned memfd.
    if unsafe { libc::fcntl(image.as_raw_fd(), libc::F_ADD_SEALS, required_seals) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let observed_seals = unsafe { libc::fcntl(image.as_raw_fd(), libc::F_GET_SEALS) };
    if observed_seals < 0 {
        return Err(io::Error::last_os_error());
    }
    if observed_seals & required_seals != required_seals {
        return Err(io::Error::from_raw_os_error(libc::EPERM));
    }
    image.seek(SeekFrom::Start(0))?;
    let digest = context.finish();
    let mut digest_bytes = [0_u8; 32];
    digest_bytes.copy_from_slice(digest.as_ref());
    Ok(PendingExecutableImageV1 {
        image,
        source_path,
        digest: digest_bytes,
        seals: observed_seals,
        header,
        header_len,
    })
}

#[cfg(not(target_arch = "x86_64"))]
fn stage_executable_image(_: File, _: PathBuf) -> io::Result<PendingExecutableImageV1> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "external-child preparation is unsupported on this architecture",
    ))
}

fn validate_native_elf_x86_64(header: &[u8]) -> io::Result<()> {
    const ELFCLASS64: u8 = 2;
    const ELFDATA2LSB: u8 = 1;
    const EM_X86_64: u16 = 62;
    let is_native = header.len() >= 20
        && &header[..4] == b"\x7fELF"
        && header[4] == ELFCLASS64
        && header[5] == ELFDATA2LSB
        && u16::from_le_bytes([header[18], header[19]]) == EM_X86_64;
    if is_native {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "initial executable must be a native ELF x86-64 image",
        ))
    }
}

#[cfg(target_arch = "x86_64")]
fn prepare_filesystem_containment(
    launch: &AdmittedChildLaunchV1,
    image: PendingExecutableImageV1,
    token_id: u64,
) -> io::Result<PreparedFilesystemContainmentV1> {
    let landlock_abi = landlock_abi_version()?;
    if landlock_abi < REQUIRED_LANDLOCK_ABI_V1 {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "required Landlock ABI 3 is unavailable",
        ));
    }
    let handled_access = landlock_handled_access(landlock_abi);
    let landlock_ruleset = create_landlock_ruleset(handled_access)?;
    for grant in launch.filesystem().grants() {
        add_landlock_path_rule(&landlock_ruleset, grant.path(), grant.access())?;
    }
    let cwd = open_exact_descriptor(
        launch.working_directory(),
        libc::O_PATH | libc::O_RDONLY,
        true,
    )?;
    Ok(PreparedFilesystemContainmentV1 {
        token_id,
        executable_image: image.image,
        cwd,
        landlock_ruleset,
        executable_seals: image.seals,
    })
}

#[cfg(not(target_arch = "x86_64"))]
fn prepare_filesystem_containment(
    _: &AdmittedChildLaunchV1,
    _: PendingExecutableImageV1,
    _: u64,
) -> io::Result<PreparedFilesystemContainmentV1> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "external-child filesystem enforcement is unsupported on this architecture",
    ))
}

fn allocate_prepared_containment_token_id_v1() -> Result<u64, String> {
    NEXT_PREPARED_CONTAINMENT_TOKEN_ID_V1
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            current.checked_add(1)
        })
        .map_err(|_| "prepared containment token space is exhausted".to_string())
}

#[cfg(target_arch = "x86_64")]
fn landlock_abi_version() -> io::Result<i32> {
    // SAFETY: the VERSION query uses the documented null-attribute form.
    let abi = unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            std::ptr::null::<LandlockRulesetAttrV1>(),
            0_usize,
            LANDLOCK_CREATE_RULESET_VERSION_V1,
        )
    };
    if abi < 0 {
        Err(io::Error::last_os_error())
    } else {
        i32::try_from(abi).map_err(|_| io::Error::from_raw_os_error(libc::EOVERFLOW))
    }
}

fn landlock_handled_access(abi: i32) -> u64 {
    let through_truncate = LANDLOCK_ACCESS_FS_EXECUTE_V1
        | LANDLOCK_ACCESS_FS_WRITE_FILE_V1
        | LANDLOCK_ACCESS_FS_READ_FILE_V1
        | LANDLOCK_ACCESS_FS_READ_DIR_V1
        | LANDLOCK_ACCESS_FS_REMOVE_DIR_V1
        | LANDLOCK_ACCESS_FS_REMOVE_FILE_V1
        | LANDLOCK_ACCESS_FS_MAKE_CHAR_V1
        | LANDLOCK_ACCESS_FS_MAKE_DIR_V1
        | LANDLOCK_ACCESS_FS_MAKE_REG_V1
        | LANDLOCK_ACCESS_FS_MAKE_SOCK_V1
        | LANDLOCK_ACCESS_FS_MAKE_FIFO_V1
        | LANDLOCK_ACCESS_FS_MAKE_BLOCK_V1
        | LANDLOCK_ACCESS_FS_MAKE_SYM_V1
        | LANDLOCK_ACCESS_FS_REFER_V1
        | LANDLOCK_ACCESS_FS_TRUNCATE_V1;
    if abi >= 5 {
        through_truncate | LANDLOCK_ACCESS_FS_IOCTL_DEV_V1
    } else {
        through_truncate
    }
}

#[cfg(target_arch = "x86_64")]
fn create_landlock_ruleset(handled_access_fs: u64) -> io::Result<File> {
    let attribute = LandlockRulesetAttrV1 { handled_access_fs };
    // SAFETY: the kernel reads the fixed-layout ruleset prefix and returns one
    // newly owned descriptor.
    let fd = unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            &attribute,
            std::mem::size_of::<LandlockRulesetAttrV1>(),
            0_u32,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let raw_fd = i32::try_from(fd).map_err(|_| io::Error::from_raw_os_error(libc::EOVERFLOW))?;
    // SAFETY: successful ruleset creation returns one newly owned descriptor.
    Ok(unsafe { File::from_raw_fd(raw_fd) })
}

#[cfg(target_arch = "x86_64")]
fn add_landlock_path_rule(
    ruleset: &File,
    path: &Path,
    access: ChildFilesystemAccessV1,
) -> io::Result<()> {
    let parent = open_exact_descriptor(path, libc::O_PATH | libc::O_RDONLY, false)?;
    let is_directory = parent.metadata()?.file_type().is_dir();
    let allowed_access = match (access, is_directory) {
        (ChildFilesystemAccessV1::ReadOnly, true) => {
            LANDLOCK_ACCESS_FS_EXECUTE_V1
                | LANDLOCK_ACCESS_FS_READ_FILE_V1
                | LANDLOCK_ACCESS_FS_READ_DIR_V1
        }
        (ChildFilesystemAccessV1::ReadOnly, false) => {
            LANDLOCK_ACCESS_FS_EXECUTE_V1 | LANDLOCK_ACCESS_FS_READ_FILE_V1
        }
        (ChildFilesystemAccessV1::ReadWrite, true) => {
            LANDLOCK_ACCESS_FS_WRITE_FILE_V1
                | LANDLOCK_ACCESS_FS_READ_FILE_V1
                | LANDLOCK_ACCESS_FS_READ_DIR_V1
                | LANDLOCK_ACCESS_FS_REMOVE_DIR_V1
                | LANDLOCK_ACCESS_FS_REMOVE_FILE_V1
                | LANDLOCK_ACCESS_FS_MAKE_CHAR_V1
                | LANDLOCK_ACCESS_FS_MAKE_DIR_V1
                | LANDLOCK_ACCESS_FS_MAKE_REG_V1
                | LANDLOCK_ACCESS_FS_MAKE_SOCK_V1
                | LANDLOCK_ACCESS_FS_MAKE_FIFO_V1
                | LANDLOCK_ACCESS_FS_MAKE_BLOCK_V1
                | LANDLOCK_ACCESS_FS_MAKE_SYM_V1
                | LANDLOCK_ACCESS_FS_REFER_V1
                | LANDLOCK_ACCESS_FS_TRUNCATE_V1
        }
        (ChildFilesystemAccessV1::ReadWrite, false) => {
            LANDLOCK_ACCESS_FS_WRITE_FILE_V1
                | LANDLOCK_ACCESS_FS_READ_FILE_V1
                | LANDLOCK_ACCESS_FS_TRUNCATE_V1
        }
    };
    let attribute = LandlockPathBeneathAttrV1 {
        allowed_access,
        parent_fd: parent.as_raw_fd(),
    };
    // SAFETY: the kernel copies the packed path-beneath attribute and takes a
    // reference to the pinned parent object during this call.
    let result = unsafe {
        libc::syscall(
            libc::SYS_landlock_add_rule,
            ruleset.as_raw_fd(),
            LANDLOCK_RULE_PATH_BENEATH_V1,
            &attribute,
            0_u32,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

fn prepare_exec_vectors(launch: &PreparedChildOsLaunchV1) -> io::Result<PreparedExecVectorsV1> {
    let mut argv_storage = Vec::with_capacity(launch.arguments().len() + 1);
    argv_storage.push(
        CString::new(launch.executable().as_os_str().as_bytes())
            .map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?,
    );
    for argument in launch.arguments() {
        argv_storage.push(
            CString::new(argument.as_bytes())
                .map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?,
        );
    }
    let argv_storage = argv_storage.into_boxed_slice();
    let argv_addresses = argv_storage
        .iter()
        .map(|value| value.as_ptr() as usize)
        .chain(std::iter::once(0_usize))
        .collect::<Vec<_>>()
        .into_boxed_slice();

    let environment_storage = launch
        .environment()
        .map(|(name, value)| {
            CString::new(format!("{name}={value}"))
                .map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))
        })
        .collect::<io::Result<Vec<_>>>()?
        .into_boxed_slice();
    let environment_addresses = environment_storage
        .iter()
        .map(|value| value.as_ptr() as usize)
        .chain(std::iter::once(0_usize))
        .collect::<Vec<_>>()
        .into_boxed_slice();
    Ok(PreparedExecVectorsV1 {
        _argv_storage: argv_storage,
        argv_addresses,
        _environment_storage: environment_storage,
        environment_addresses,
    })
}

#[cfg(target_arch = "x86_64")]
fn restrict_child_filesystem(ruleset_fd: RawFd) -> io::Result<()> {
    // SAFETY: restrict_self consumes no userspace buffers and applies only to
    // the calling post-fork child after no_new_privs is set.
    if unsafe { libc::syscall(libc::SYS_landlock_restrict_self, ruleset_fd, 0_u32) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(not(target_arch = "x86_64"))]
fn restrict_child_filesystem(_: RawFd) -> io::Result<()> {
    Err(io::Error::from_raw_os_error(libc::ENOTSUP))
}

#[cfg(target_arch = "x86_64")]
fn exec_prepared_image(
    filesystem: &PreparedFilesystemContainmentV1,
    vectors: &PreparedExecVectorsV1,
) -> io::Result<()> {
    let empty_path = b"\0";
    // SAFETY: every address points into CString storage retained by `vectors`,
    // both pointer lists are null-terminated, and the executable fd is sealed.
    unsafe {
        libc::syscall(
            libc::SYS_execveat,
            filesystem.executable_image.as_raw_fd(),
            empty_path.as_ptr(),
            vectors
                .argv_addresses
                .as_ptr()
                .cast::<*const libc::c_char>(),
            vectors
                .environment_addresses
                .as_ptr()
                .cast::<*const libc::c_char>(),
            AT_EMPTY_PATH_V1,
        );
    }
    Err(io::Error::last_os_error())
}

#[cfg(not(target_arch = "x86_64"))]
fn exec_prepared_image(
    _: &PreparedFilesystemContainmentV1,
    _: &PreparedExecVectorsV1,
) -> io::Result<()> {
    Err(io::Error::from_raw_os_error(libc::ENOTSUP))
}

fn spawn_child(
    launch: &PreparedChildOsLaunchV1,
    filesystem: PreparedFilesystemContainmentV1,
    containment_setup: ContainmentSetupV1,
) -> Result<Child, String> {
    // Filter construction and all of its allocation happen in the parent,
    // before Command::spawn can fork.
    let containment = prepare_child_containment(containment_setup)
        .map_err(|error| format!("cannot prepare child containment: {error}"))?;
    let exec_vectors = prepare_exec_vectors(launch)
        .map_err(|error| format!("cannot prepare exact exec vectors: {error}"))?;
    let mut command = Command::new("/__kitsu_execveat_path_fallback_forbidden__");
    command
        .args(launch.arguments())
        .env_clear()
        .envs(launch.environment())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    // SAFETY: the closure uses only async-signal-safe syscalls and constructs
    // no shared process state. Every setup error aborts before exec.
    unsafe {
        command.pre_exec(move || {
            install_child_containment(&containment, &filesystem)?;
            exec_prepared_image(&filesystem, &exec_vectors)
        });
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
    child_exit_observation(child_pid).map(|exit| exit.is_some())
}

fn child_exit_observation(child_pid: u32) -> Result<Option<ChildExitTruthV1>, String> {
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
    let information = unsafe { information.assume_init() };
    if unsafe { information.si_pid() } == 0 {
        return Ok(None);
    }
    let truth = if information.si_code == libc::CLD_EXITED {
        ChildExitTruthV1::Exited(unsafe { information.si_status() })
    } else {
        ChildExitTruthV1::Signalled
    };
    Ok(Some(truth))
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

fn install_child_containment(
    containment: &PreparedChildContainmentV1,
    filesystem: &PreparedFilesystemContainmentV1,
) -> io::Result<()> {
    verify_child_owns_process_group()?;
    // SAFETY: fchdir reads the already-pinned directory descriptor and changes
    // only the post-fork child's cwd.
    if unsafe { libc::fchdir(filesystem.cwd.as_raw_fd()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    clear_inherited_capabilities()?;
    restrict_child_filesystem(filesystem.landlock_ruleset.as_raw_fd())?;
    close_inherited_descriptors()?;
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
    use std::ffi::CString;
    use std::fs::{OpenOptions, Permissions};
    use std::io::{Read, Write};
    use std::os::fd::RawFd;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::{symlink, PermissionsExt};
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};

    use ring::digest::{Context, SHA256};
    use serial_test::serial;

    use super::*;
    use crate::external::authority::{
        prepare_child_launch_request_v1, AuthorityProfileV1, ChildEnvironmentPolicyV1,
        ChildEnvironmentVariableV1, ChildFilesystemAccessV1, ChildFilesystemGrantV1,
        ChildFilesystemPolicyV1, ChildIoLimitsV1, ChildLaunchAdmissionErrorV1,
        ChildLaunchAuthorityDecisionV1, ChildLaunchModeV1, ChildLaunchRequestV1,
        ChildNetworkPolicyV1, ChildPhaseBudgetsV1, ChildProcessGroupPolicyV1, ChildRequestPolicyV1,
        ChildRuntimePolicyV1, ChildTimeoutDispositionV1, PreparedChildLaunchV1,
    };
    use crate::external::handler::{
        CallerExternalToolGrantV1, ExternalMcpEchoCompletionV1, ExternalMcpEchoSourceOwnerV1,
        ExternalMcpEchoSourceStateV1, FixtureEchoRequestV1, HostExternalToolGrantV1,
    };
    use crate::external::lifecycle::ChildExitTruthV1;
    use crate::external::mcp::{
        McpSerialChildFailureV1, McpSerialChildOwnerV1, McpSerialChildStateV1,
    };
    use crate::external::serial::{
        SerialChildCauseV1, SerialChildTransportFailureV1, SerialChildTransportStateV1,
        SerialChildTransportV1,
    };
    use crate::external::supervisor::{
        ChildPhaseSupervisorFailureV1, ChildPhaseSupervisorStateV1, ChildPhaseSupervisorV1,
        ChildSupervisorCauseV1,
    };
    use crate::mcp_codec::{ClientError, ClientEvent, CodecError};

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
        filesystem: ChildFilesystemPolicyV1,
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
            filesystem,
            ChildNetworkPolicyV1::Deny,
            ChildTimeoutDispositionV1::TerminateProcessGroupAndReap,
        )
    }

    fn serial_runtime_policy(
        limits: TestLimits,
        environment: Vec<ChildEnvironmentVariableV1>,
        filesystem: ChildFilesystemPolicyV1,
        max_frames: u16,
        idle_millis: u64,
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
            filesystem,
            ChildNetworkPolicyV1::Deny,
            ChildTimeoutDispositionV1::TerminateProcessGroupAndReap,
        )
        .with_request_policy(ChildRequestPolicyV1::serial(max_frames, idle_millis))
    }

    fn confined_filesystem_policy(
        cwd: &Path,
        mut grants: Vec<ChildFilesystemGrantV1>,
    ) -> ChildFilesystemPolicyV1 {
        grants.insert(
            0,
            ChildFilesystemGrantV1::new("/proc", ChildFilesystemAccessV1::ReadOnly),
        );
        grants.insert(
            0,
            ChildFilesystemGrantV1::new("/usr", ChildFilesystemAccessV1::ReadOnly),
        );
        grants.push(ChildFilesystemGrantV1::new(
            cwd,
            ChildFilesystemAccessV1::ReadWrite,
        ));
        ChildFilesystemPolicyV1::new(grants)
    }

    fn prepared_helper(
        cwd: &Path,
        mode: &str,
        limits: TestLimits,
        extra_environment: Vec<ChildEnvironmentVariableV1>,
        stdin: Vec<u8>,
    ) -> (PreparedChildLaunchV1, LinuxChildBackendV1) {
        let executable = helper_executable();
        let mut backend = LinuxChildBackendV1::new(stdin);
        let prepared = prepare_confined_helper(
            &executable,
            cwd,
            mode,
            limits,
            extra_environment,
            confined_filesystem_policy(cwd, Vec::new()),
            &mut backend,
        )
        .unwrap();
        (prepared, backend)
    }

    fn prepare_confined_helper(
        executable: &Path,
        cwd: &Path,
        mode: &str,
        limits: TestLimits,
        mut extra_environment: Vec<ChildEnvironmentVariableV1>,
        filesystem: ChildFilesystemPolicyV1,
        backend: &mut LinuxChildBackendV1,
    ) -> Result<PreparedChildLaunchV1, ChildLaunchAdmissionErrorV1> {
        extra_environment.insert(0, ChildEnvironmentVariableV1::new("KITSU_CHILD_MODE", mode));
        let policy = runtime_policy(limits, extra_environment, filesystem);
        let request = ChildLaunchRequestV1::new(
            1,
            ChildLaunchModeV1::Enabled,
            executable,
            digest_hex(executable),
            helper_arguments(),
            cwd,
            policy.clone(),
        );
        let authority = AuthorityProfileV1::exact(
            executable,
            digest_hex(executable),
            helper_arguments(),
            cwd,
            policy,
        )
        .unwrap();
        prepare_child_launch_request_v1(
            request,
            ChildLaunchAuthorityDecisionV1::Admit(&authority),
            backend,
        )
    }

    fn prepared_serial_program(
        cwd: &Path,
        executable: &Path,
        arguments: Vec<String>,
        limits: TestLimits,
        max_frames: u16,
        idle_millis: u64,
    ) -> (PreparedChildLaunchV1, LinuxChildBackendV1) {
        let filesystem = confined_filesystem_policy(cwd, Vec::new());
        let policy = serial_runtime_policy(
            limits,
            vec![ChildEnvironmentVariableV1::new("LC_ALL", "C")],
            filesystem,
            max_frames,
            idle_millis,
        );
        let request = ChildLaunchRequestV1::new(
            1,
            ChildLaunchModeV1::Enabled,
            executable,
            digest_hex(executable),
            arguments.clone(),
            cwd,
            policy.clone(),
        );
        let authority =
            AuthorityProfileV1::exact(executable, digest_hex(executable), arguments, cwd, policy)
                .unwrap();
        let mut backend = LinuxChildBackendV1::new(Vec::new());
        let prepared = prepare_child_launch_request_v1(
            request,
            ChildLaunchAuthorityDecisionV1::Admit(&authority),
            &mut backend,
        )
        .unwrap();
        (prepared, backend)
    }

    const MCP_PEER_ENV_V1: &str = "KITSU_INTERNAL_TEST_MCP_PEER_V1";
    const MCP_INITIALIZE_RESPONSE_LINE_V1: &[u8] = b"{\"id\":1,\"jsonrpc\":\"2.0\",\"result\":{\"capabilities\":{\"tools\":{\"listChanged\":false}},\"protocolVersion\":\"2025-11-25\",\"serverInfo\":{\"name\":\"kitsu-test-peer\",\"version\":\"1\"}}}\n";
    const MCP_LIST_RESPONSE_LINE_V1: &[u8] = b"{\"id\":2,\"jsonrpc\":\"2.0\",\"result\":{\"tools\":[{\"description\":\"Returns one integer as text.\",\"inputSchema\":{\"additionalProperties\":false,\"properties\":{\"value\":{\"type\":\"integer\"}},\"required\":[\"value\"],\"type\":\"object\"},\"name\":\"fixture.echo\"}]}}\n";
    const MCP_CALL_RESPONSE_LINE_V1: &[u8] = b"{\"id\":3,\"jsonrpc\":\"2.0\",\"result\":{\"content\":[{\"text\":\"7\",\"type\":\"text\"}],\"isError\":false}}\n";

    #[derive(Debug, Clone, Copy)]
    enum RealMcpPeerModeV1 {
        Exact,
        Eof,
        Crash,
        WrongId,
        Malformed,
        Descendant,
    }

    impl RealMcpPeerModeV1 {
        fn discriminator(self) -> &'static str {
            match self {
                Self::Exact => "exact",
                Self::Eof => "eof",
                Self::Crash => "crash",
                Self::WrongId => "wrong-id",
                Self::Malformed => "malformed",
                Self::Descendant => "descendant",
            }
        }
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    #[used]
    #[link_section = ".init_array"]
    static KITSU_TEST_MCP_PEER_PREMAIN_V1: unsafe extern "C" fn() = kitsu_test_mcp_peer_premain_v1;

    // This test-only ELF entry runs after the admitted exec has installed the
    // prepared Landlock/seccomp state but before libtest can write to stdout.
    // The absent discriminator path only reads `environ` and returns. Every
    // selected path below uses fixed buffers and async-signal-safe libc calls;
    // it never enters Rust stdio, allocation, discovery, or panic machinery.
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    unsafe extern "C" fn kitsu_test_mcp_peer_premain_v1() {
        let value = unsafe { libc::getenv(c"KITSU_INTERNAL_TEST_MCP_PEER_V1".as_ptr()) };
        if value.is_null() {
            return;
        }
        let mode = [
            RealMcpPeerModeV1::Exact,
            RealMcpPeerModeV1::Eof,
            RealMcpPeerModeV1::Crash,
            RealMcpPeerModeV1::WrongId,
            RealMcpPeerModeV1::Malformed,
            RealMcpPeerModeV1::Descendant,
        ]
        .into_iter()
        .find(|mode| unsafe { raw_c_value_equals_v1(value, mode.discriminator().as_bytes()) });
        let Some(mode) = mode else {
            unsafe { libc::_exit(89) };
        };
        let exit = unsafe { run_raw_mcp_peer_v1(mode) };
        unsafe { libc::_exit(exit) };
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    unsafe fn raw_c_value_equals_v1(value: *const libc::c_char, expected: &[u8]) -> bool {
        for (index, expected) in expected.iter().enumerate() {
            if unsafe { *value.add(index).cast::<u8>() } != *expected {
                return false;
            }
        }
        unsafe { *value.add(expected.len()).cast::<u8>() == 0 }
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    unsafe fn run_raw_mcp_peer_v1(mode: RealMcpPeerModeV1) -> i32 {
        const INITIALIZE: &[u8] = b"{\"id\":1,\"jsonrpc\":\"2.0\",\"method\":\"initialize\",\"params\":{\"capabilities\":{},\"clientInfo\":{\"name\":\"kitsu-tools\",\"version\":\"0.1.0\"},\"protocolVersion\":\"2025-11-25\"}}\n";
        const INITIALIZED: &[u8] =
            b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n";
        const TOOLS_LIST: &[u8] = b"{\"id\":2,\"jsonrpc\":\"2.0\",\"method\":\"tools/list\"}\n";
        const TOOLS_CALL: &[u8] = b"{\"id\":3,\"jsonrpc\":\"2.0\",\"method\":\"tools/call\",\"params\":{\"arguments\":{\"value\":7},\"name\":\"fixture.echo\"}}\n";
        const WRONG_ID: &[u8] = b"{\"id\":99,\"jsonrpc\":\"2.0\",\"result\":{\"capabilities\":{\"tools\":{\"listChanged\":false}},\"protocolVersion\":\"2025-11-25\",\"serverInfo\":{\"name\":\"kitsu-test-peer\",\"version\":\"1\"}}}\n";

        if matches!(mode, RealMcpPeerModeV1::Descendant)
            && !unsafe { raw_mcp_descendant_marker_v1() }
        {
            return 90;
        }
        if !unsafe { raw_expect_stdin_v1(INITIALIZE) } {
            return 91;
        }
        match mode {
            RealMcpPeerModeV1::Eof => return 0,
            RealMcpPeerModeV1::Crash => {
                // Keep the stdio descriptors live in the same owned process
                // group so direct-child exit 17 is observable before pipe EOF.
                // SAFETY: the pre-main process is single-threaded, and the
                // fork child performs only the async-signal-safe pause loop.
                let descriptor_holder = unsafe { libc::fork() };
                if descriptor_holder < 0 {
                    return 18;
                }
                if descriptor_holder == 0 {
                    unsafe { raw_pause_forever_v1() }
                }
                return 17;
            }
            RealMcpPeerModeV1::WrongId => {
                if !unsafe { raw_write_all_v1(libc::STDOUT_FILENO, WRONG_ID) } {
                    return 92;
                }
                unsafe { raw_pause_forever_v1() }
            }
            RealMcpPeerModeV1::Malformed => {
                if !unsafe { raw_write_all_v1(libc::STDOUT_FILENO, b"{]\n") } {
                    return 93;
                }
                unsafe { raw_pause_forever_v1() }
            }
            RealMcpPeerModeV1::Exact | RealMcpPeerModeV1::Descendant => {}
        }
        if !unsafe { raw_write_all_v1(libc::STDOUT_FILENO, MCP_INITIALIZE_RESPONSE_LINE_V1) }
            || !unsafe { raw_expect_stdin_v1(INITIALIZED) }
            || !unsafe { raw_expect_stdin_v1(TOOLS_LIST) }
            || !unsafe { raw_write_all_v1(libc::STDOUT_FILENO, MCP_LIST_RESPONSE_LINE_V1) }
            || !unsafe { raw_expect_stdin_v1(TOOLS_CALL) }
            || !unsafe { raw_write_all_v1(libc::STDOUT_FILENO, MCP_CALL_RESPONSE_LINE_V1) }
        {
            return 94;
        }
        unsafe { raw_pause_forever_v1() }
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    unsafe fn raw_expect_stdin_v1(expected: &[u8]) -> bool {
        for expected in expected {
            let mut observed = 0_u8;
            loop {
                let read =
                    unsafe { libc::read(libc::STDIN_FILENO, (&mut observed as *mut u8).cast(), 1) };
                if read == 1 {
                    break;
                }
                if read == 0 {
                    return false;
                }
                if read < 0 && unsafe { raw_errno_v1() } == libc::EINTR {
                    continue;
                }
                return false;
            }
            if observed != *expected {
                return false;
            }
        }
        true
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    unsafe fn raw_write_all_v1(fd: RawFd, mut bytes: &[u8]) -> bool {
        while !bytes.is_empty() {
            let written = unsafe { libc::write(fd, bytes.as_ptr().cast(), bytes.len()) };
            if written > 0 {
                let Ok(written) = usize::try_from(written) else {
                    return false;
                };
                let Some(remaining) = bytes.get(written..) else {
                    return false;
                };
                bytes = remaining;
            } else if written < 0 && unsafe { raw_errno_v1() } == libc::EINTR {
                continue;
            } else {
                return false;
            }
        }
        true
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    unsafe fn raw_mcp_descendant_marker_v1() -> bool {
        let descendant = unsafe { libc::fork() };
        if descendant < 0 {
            return false;
        }
        if descendant == 0 {
            unsafe { raw_pause_forever_v1() }
        }
        let fd = unsafe {
            libc::openat(
                libc::AT_FDCWD,
                c"mcp-descendant-pids".as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC,
                0o600,
            )
        };
        if fd < 0 {
            return false;
        }
        let mut bytes = [0_u8; 96];
        let mut length = 0_usize;
        let formatted =
            raw_push_positive_i32_v1(&mut bytes, &mut length, unsafe { libc::getpid() })
                && raw_push_byte_v1(&mut bytes, &mut length, b' ')
                && raw_push_positive_i32_v1(&mut bytes, &mut length, unsafe { libc::getpgrp() })
                && raw_push_byte_v1(&mut bytes, &mut length, b' ')
                && raw_push_positive_i32_v1(&mut bytes, &mut length, descendant)
                && raw_push_byte_v1(&mut bytes, &mut length, b'\n');
        let written = formatted
            && bytes
                .get(..length)
                .is_some_and(|bytes| unsafe { raw_write_all_v1(fd, bytes) });
        let closed = unsafe { libc::close(fd) } == 0;
        written && closed
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    fn raw_push_positive_i32_v1(bytes: &mut [u8], length: &mut usize, value: i32) -> bool {
        let Ok(mut value) = u32::try_from(value) else {
            return false;
        };
        let mut reversed = [0_u8; 10];
        let mut digits = 0_usize;
        loop {
            let Some(digit) = reversed.get_mut(digits) else {
                return false;
            };
            let Some(decimal) = b"0123456789".get((value % 10) as usize) else {
                return false;
            };
            *digit = *decimal;
            digits += 1;
            value /= 10;
            if value == 0 {
                break;
            }
        }
        if bytes.len().saturating_sub(*length) < digits {
            return false;
        }
        let Some(reversed) = reversed.get(..digits) else {
            return false;
        };
        for digit in reversed.iter().rev() {
            let Some(output) = bytes.get_mut(*length) else {
                return false;
            };
            *output = *digit;
            *length += 1;
        }
        true
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    fn raw_push_byte_v1(bytes: &mut [u8], length: &mut usize, value: u8) -> bool {
        if *length == bytes.len() {
            return false;
        }
        let Some(output) = bytes.get_mut(*length) else {
            return false;
        };
        *output = value;
        *length += 1;
        true
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    unsafe fn raw_pause_forever_v1() -> ! {
        loop {
            unsafe { libc::pause() };
        }
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    unsafe fn raw_errno_v1() -> i32 {
        unsafe { *libc::__errno_location() }
    }

    fn prepared_real_mcp_peer(
        cwd: &Path,
        mode: RealMcpPeerModeV1,
    ) -> (PreparedChildLaunchV1, LinuxChildBackendV1) {
        let executable = helper_executable();
        let arguments = helper_arguments();
        let cwd_access = if matches!(mode, RealMcpPeerModeV1::Descendant) {
            ChildFilesystemAccessV1::ReadWrite
        } else {
            ChildFilesystemAccessV1::ReadOnly
        };
        let filesystem = ChildFilesystemPolicyV1::new(vec![
            ChildFilesystemGrantV1::new(
                "/usr/lib/x86_64-linux-gnu",
                ChildFilesystemAccessV1::ReadOnly,
            ),
            ChildFilesystemGrantV1::new(cwd, cwd_access),
        ]);
        assert_eq!(filesystem.grants().len(), 2);
        assert_eq!(
            filesystem.grants()[0],
            ChildFilesystemGrantV1::new(
                "/usr/lib/x86_64-linux-gnu",
                ChildFilesystemAccessV1::ReadOnly,
            )
        );
        assert_eq!(
            filesystem.grants()[1],
            ChildFilesystemGrantV1::new(cwd, cwd_access)
        );
        let limits = TestLimits {
            stdin_bytes: 2 * 1024,
            stdout_bytes: 2 * 1024,
            stderr_bytes: 256,
            ..TestLimits::default()
        };
        let policy = serial_runtime_policy(
            limits,
            vec![ChildEnvironmentVariableV1::new(
                MCP_PEER_ENV_V1,
                mode.discriminator(),
            )],
            filesystem,
            4,
            250,
        );
        let request = ChildLaunchRequestV1::new(
            1,
            ChildLaunchModeV1::Enabled,
            &executable,
            digest_hex(&executable),
            arguments.clone(),
            cwd,
            policy.clone(),
        );
        let authority =
            AuthorityProfileV1::exact(&executable, digest_hex(&executable), arguments, cwd, policy)
                .unwrap();
        let mut backend = LinuxChildBackendV1::new(Vec::new());
        let prepared = prepare_child_launch_request_v1(
            request,
            ChildLaunchAuthorityDecisionV1::Admit(&authority),
            &mut backend,
        )
        .unwrap();
        (prepared, backend)
    }

    fn drive_real_mcp_until(
        owner: &mut McpSerialChildOwnerV1<'_, LinuxChildBackendV1>,
        expected: fn(&McpSerialChildStateV1) -> bool,
    ) {
        let wall_deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if expected(owner.state()) {
                return;
            }
            assert!(
                Instant::now() < wall_deadline,
                "MCP fixture hung before expected state: {:?}",
                owner.state()
            );
            assert!(
                !matches!(
                    owner.state(),
                    McpSerialChildStateV1::Stopped | McpSerialChildStateV1::Failed { .. }
                ),
                "MCP fixture stopped before expected state: {:?}",
                owner.state()
            );
            owner.poll().unwrap();
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn start_real_mcp(owner: &mut McpSerialChildOwnerV1<'_, LinuxChildBackendV1>) {
        owner.start().unwrap();
        drive_real_mcp_until(owner, |state| {
            matches!(state, McpSerialChildStateV1::ReadyForToolsList)
        });
    }

    fn drive_real_mcp_event(
        owner: &mut McpSerialChildOwnerV1<'_, LinuxChildBackendV1>,
    ) -> ClientEvent {
        drive_real_mcp_until(owner, |state| {
            matches!(state, McpSerialChildStateV1::EventReady)
        });
        owner.take_event().unwrap()
    }

    fn drive_real_mcp_terminal(owner: &mut McpSerialChildOwnerV1<'_, LinuxChildBackendV1>) {
        drive_real_mcp_until(owner, |state| {
            matches!(
                state,
                McpSerialChildStateV1::Stopped | McpSerialChildStateV1::Failed { .. }
            )
        });
    }

    fn complete_real_mcp_exchange(owner: &mut McpSerialChildOwnerV1<'_, LinuxChildBackendV1>) {
        start_real_mcp(owner);
        let pgid = owner
            .effects_for_test()
            .active_pgid_for_test()
            .expect("one live MCP process group");
        owner.begin_tools_list(None).unwrap();
        assert_eq!(
            drive_real_mcp_event(owner),
            ClientEvent::ToolsListed(serde_json::json!({
                "tools": [{
                    "description": "Returns one integer as text.",
                    "inputSchema": {
                        "additionalProperties": false,
                        "properties": {"value": {"type": "integer"}},
                        "required": ["value"],
                        "type": "object"
                    },
                    "name": "fixture.echo"
                }]
            }))
        );
        assert_eq!(owner.effects_for_test().active_pgid_for_test(), Some(pgid));
        owner
            .begin_tools_call("fixture.echo", serde_json::json!({"value": 7}))
            .unwrap();
        assert_eq!(
            drive_real_mcp_event(owner),
            ClientEvent::ToolCalled(serde_json::json!({
                "content": [{"text": "7", "type": "text"}],
                "isError": false
            }))
        );
        assert_eq!(owner.effects_for_test().active_pgid_for_test(), Some(pgid));
    }

    fn start_real_serial(transport: &mut SerialChildTransportV1<'_, LinuxChildBackendV1>) {
        transport.start().unwrap();
        let wall_deadline = Instant::now() + Duration::from_secs(5);
        while !matches!(transport.state(), SerialChildTransportStateV1::Idle { .. }) {
            assert!(
                Instant::now() < wall_deadline,
                "serial startup hung: {:?}",
                transport.state()
            );
            transport.poll().unwrap();
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn poll_real_serial_response(
        transport: &mut SerialChildTransportV1<'_, LinuxChildBackendV1>,
    ) -> Vec<u8> {
        let wall_deadline = Instant::now() + Duration::from_secs(5);
        loop {
            assert!(
                Instant::now() < wall_deadline,
                "serial response hung: {:?}",
                transport.state()
            );
            match transport.state() {
                SerialChildTransportStateV1::FrameInFlight { .. } => {
                    transport.poll().unwrap();
                }
                SerialChildTransportStateV1::ResponseReady { .. } => {
                    return transport.take_response().unwrap()
                }
                state => panic!("serial response failed: {state:?}"),
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn finish_real_serial(
        transport: &mut SerialChildTransportV1<'_, LinuxChildBackendV1>,
        cause: SerialChildCauseV1,
    ) -> SerialChildTransportStateV1 {
        match cause {
            SerialChildCauseV1::Cancel => {
                transport.cancel().unwrap();
            }
            SerialChildCauseV1::Shutdown => {
                transport.shutdown().unwrap();
            }
            _ => unreachable!("fixture uses an explicit host stop cause"),
        }
        let wall_deadline = Instant::now() + Duration::from_secs(5);
        loop {
            assert!(
                Instant::now() < wall_deadline,
                "serial cleanup hung: {:?}",
                transport.state()
            );
            match transport.state() {
                SerialChildTransportStateV1::Draining { .. }
                | SerialChildTransportStateV1::Killing { .. }
                | SerialChildTransportStateV1::Reaping { .. } => {
                    transport.poll().unwrap();
                }
                SerialChildTransportStateV1::Stopped { .. }
                | SerialChildTransportStateV1::Failed(_) => return transport.state().clone(),
                state => panic!("unexpected serial cleanup state: {state:?}"),
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn drive_real_serial_failure(
        transport: &mut SerialChildTransportV1<'_, LinuxChildBackendV1>,
    ) -> SerialChildTransportStateV1 {
        let wall_deadline = Instant::now() + Duration::from_secs(5);
        loop {
            assert!(
                Instant::now() < wall_deadline,
                "serial failure cleanup hung: {:?}",
                transport.state()
            );
            match transport.state() {
                SerialChildTransportStateV1::FrameInFlight { .. }
                | SerialChildTransportStateV1::Draining { .. }
                | SerialChildTransportStateV1::Killing { .. }
                | SerialChildTransportStateV1::Reaping { .. } => {
                    transport.poll().unwrap();
                }
                SerialChildTransportStateV1::Failed(_) => return transport.state().clone(),
                SerialChildTransportStateV1::ResponseReady { .. } => {
                    panic!("EOF/crash/idle became response success")
                }
                state => panic!("unexpected serial failure state: {state:?}"),
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn drive_one_shot(
        prepared: PreparedChildLaunchV1,
        backend: LinuxChildBackendV1,
    ) -> (ChildPhaseSupervisorStateV1, LinuxChildBackendV1) {
        drive_one_shot_with_backend(prepared, backend)
    }

    fn drive_one_shot_with_backend(
        prepared: PreparedChildLaunchV1,
        mut backend: LinuxChildBackendV1,
    ) -> (ChildPhaseSupervisorStateV1, LinuxChildBackendV1) {
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
        mut backend: LinuxChildBackendV1,
        cause: ChildSupervisorCauseV1,
    ) -> (ChildPhaseSupervisorStateV1, LinuxChildBackendV1) {
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
    #[serial]
    fn prepared_projection_is_exact_consuming_and_redacts_environment_values() {
        let cwd = tempfile::tempdir().unwrap();
        let (prepared, _backend) = prepared_helper(
            cwd.path(),
            "inspect",
            TestLimits::default(),
            vec![ChildEnvironmentVariableV1::new(
                "KITSU_PROJECTED_VALUE",
                "projection-secret",
            )],
            Vec::new(),
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
    #[serial]
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
            confined_filesystem_policy(cwd.path(), Vec::new()),
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
        let mut disabled_backend = LinuxChildBackendV1::new(Vec::new());
        assert_eq!(
            prepare_child_launch_request_v1(
                disabled,
                ChildLaunchAuthorityDecisionV1::Admit(&authority),
                &mut disabled_backend,
            ),
            Err(ChildLaunchAdmissionErrorV1::Disabled)
        );
        assert_eq!(disabled_backend.os_spawn_count(), 0);

        let (prepared, backend) =
            prepared_helper(cwd.path(), "echo", limits, Vec::new(), b"12345".to_vec());
        let (terminal, oversized_backend) = drive_one_shot(prepared, backend);
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
    #[serial]
    fn unsupported_containment_fails_before_spawn_and_child_code() {
        let cwd = tempfile::tempdir().unwrap();
        let marker = cwd.path().join("must-not-exist");
        let mut backend = LinuxChildBackendV1::new_with_containment_setup_for_test(
            Vec::new(),
            TestContainmentSetupV1::FailBeforeSpawn,
        );
        let executable = helper_executable();
        let prepared = prepare_confined_helper(
            &executable,
            cwd.path(),
            "marker",
            TestLimits::default(),
            vec![ChildEnvironmentVariableV1::new(
                "KITSU_MARKER_PATH",
                marker.to_string_lossy(),
            )],
            confined_filesystem_policy(cwd.path(), Vec::new()),
            &mut backend,
        )
        .unwrap();
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
    #[serial]
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
        let (prepared, backend) = prepared_helper(
            cwd.path(),
            "inspect",
            TestLimits::default(),
            vec![
                ChildEnvironmentVariableV1::new("KITSU_PROJECTED_VALUE", "exact-value"),
                ChildEnvironmentVariableV1::new("KITSU_INHERITED_FD", sockets[1].to_string()),
                ChildEnvironmentVariableV1::new("KITSU_HOST_PID", host_pid.to_string()),
                ChildEnvironmentVariableV1::new("KITSU_HOST_PGID", host_pgid.to_string()),
            ],
            Vec::new(),
        );
        let (terminal, mut backend) = drive_one_shot(prepared, backend);
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
    #[serial]
    fn bounded_stdin_round_trip_is_one_shot_and_requires_exit_plus_both_eofs() {
        let cwd = tempfile::tempdir().unwrap();
        let input = b"one bounded request\n".to_vec();
        let (prepared, backend) = prepared_helper(
            cwd.path(),
            "echo",
            TestLimits::default(),
            Vec::new(),
            input.clone(),
        );
        let (terminal, mut backend) = drive_one_shot(prepared, backend);
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
    #[serial]
    fn stdout_and_stderr_overflow_fail_closed_into_owned_joined_residue() {
        let cwd = tempfile::tempdir().unwrap();
        let limits = TestLimits {
            stdout_bytes: 128,
            stderr_bytes: 128,
            ..TestLimits::default()
        };
        let (prepared, backend) =
            prepared_helper(cwd.path(), "flood", limits, Vec::new(), Vec::new());
        let (terminal, mut backend) = drive_one_shot(prepared, backend);
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
    #[serial]
    fn crash_after_output_eof_is_never_success() {
        let cwd = tempfile::tempdir().unwrap();
        let (prepared, backend) = prepared_helper(
            cwd.path(),
            "crash",
            TestLimits::default(),
            Vec::new(),
            Vec::new(),
        );
        let (terminal, mut backend) = drive_one_shot(prepared, backend);
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
    #[serial]
    fn response_line_and_idle_process_expire_real_monotonic_request_budget() {
        let cwd = tempfile::tempdir().unwrap();
        let limits = TestLimits {
            request_millis: 20,
            ..TestLimits::default()
        };
        let (prepared, backend) = prepared_helper(
            cwd.path(),
            "response_then_hang",
            limits,
            Vec::new(),
            Vec::new(),
        );
        let (terminal, mut backend) = drive_one_shot(prepared, backend);
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
    #[serial]
    fn cancel_and_shutdown_kill_group_reap_direct_child_and_join_workers() {
        for cause in [
            ChildSupervisorCauseV1::Cancel,
            ChildSupervisorCauseV1::Shutdown,
        ] {
            let cwd = tempfile::tempdir().unwrap();
            let (prepared, backend) = prepared_helper(
                cwd.path(),
                "descendant",
                TestLimits::default(),
                Vec::new(),
                Vec::new(),
            );
            let (terminal, mut backend) = drive_cancel_or_shutdown(prepared, backend, cause);
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
    #[serial]
    fn dropping_live_supervisor_transfers_and_cleans_all_owned_workers() {
        let cwd = tempfile::tempdir().unwrap();
        let (prepared, mut backend) = prepared_helper(
            cwd.path(),
            "hang",
            TestLimits::default(),
            Vec::new(),
            Vec::new(),
        );
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

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    #[test]
    #[serial]
    fn prepared_token_cannot_cross_consume_another_backends_resources() {
        let first_cwd = tempfile::tempdir().unwrap();
        let second_cwd = tempfile::tempdir().unwrap();
        let first_marker = first_cwd.path().join("must-not-run");
        let second_marker = second_cwd.path().join("must-not-run");
        let executable = helper_executable();
        let mut first_backend = LinuxChildBackendV1::new(Vec::new());
        let first = prepare_confined_helper(
            &executable,
            first_cwd.path(),
            "sealed_marker",
            TestLimits::default(),
            vec![ChildEnvironmentVariableV1::new(
                "KITSU_MARKER_PATH",
                first_marker.to_string_lossy(),
            )],
            confined_filesystem_policy(first_cwd.path(), Vec::new()),
            &mut first_backend,
        )
        .unwrap();
        let mut second_backend = LinuxChildBackendV1::new(Vec::new());
        let second = prepare_confined_helper(
            &executable,
            second_cwd.path(),
            "sealed_marker",
            TestLimits::default(),
            vec![ChildEnvironmentVariableV1::new(
                "KITSU_MARKER_PATH",
                second_marker.to_string_lossy(),
            )],
            confined_filesystem_policy(second_cwd.path(), Vec::new()),
            &mut second_backend,
        )
        .unwrap();

        {
            let mut supervisor = ChildPhaseSupervisorV1::new(first, &mut second_backend);
            assert!(matches!(
                supervisor.start(),
                Ok(ChildPhaseSupervisorStateV1::Failed(
                    ChildPhaseSupervisorFailureV1::Spawn(_)
                ))
            ));
        }
        drop(second);
        assert_eq!(first_backend.prepared_containment_count_for_test(), 1);
        assert_eq!(second_backend.prepared_containment_count_for_test(), 0);
        assert_eq!(second_backend.os_spawn_count(), 0);
        assert!(!first_marker.exists());
        assert!(!second_marker.exists());
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    #[test]
    #[serial]
    fn landlock_enforces_reviewed_content_roots_without_claiming_path_or_metadata_secrecy() {
        let tree = tempfile::tempdir().unwrap();
        let readonly = tree.path().join("readonly");
        let readwrite = tree.path().join("readwrite");
        let outside = tree.path().join("outside");
        std::fs::create_dir_all(&readonly).unwrap();
        std::fs::create_dir_all(&readwrite).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(readonly.join("reviewed.txt"), b"reviewed").unwrap();
        std::fs::write(outside.join("secret.txt"), b"outside").unwrap();
        let readwrite_executable = readwrite.join("must-not-execute");
        let outside_executable = outside.join("must-not-execute");
        std::fs::copy(helper_executable(), &readwrite_executable).unwrap();
        std::fs::copy(helper_executable(), &outside_executable).unwrap();
        std::fs::set_permissions(&readwrite_executable, Permissions::from_mode(0o755)).unwrap();
        std::fs::set_permissions(&outside_executable, Permissions::from_mode(0o755)).unwrap();
        symlink(&outside, readwrite.join("escape")).unwrap();

        let filesystem = confined_filesystem_policy(
            &readwrite,
            vec![ChildFilesystemGrantV1::new(
                &readonly,
                ChildFilesystemAccessV1::ReadOnly,
            )],
        );
        let environment = vec![
            ChildEnvironmentVariableV1::new("KITSU_FS_READONLY", readonly.to_string_lossy()),
            ChildEnvironmentVariableV1::new("KITSU_FS_READWRITE", readwrite.to_string_lossy()),
            ChildEnvironmentVariableV1::new("KITSU_FS_OUTSIDE", outside.to_string_lossy()),
            ChildEnvironmentVariableV1::new(
                "KITSU_FS_RW_EXECUTABLE",
                readwrite_executable.to_string_lossy(),
            ),
            ChildEnvironmentVariableV1::new(
                "KITSU_FS_OUTSIDE_EXECUTABLE",
                outside_executable.to_string_lossy(),
            ),
        ];
        let mut backend = LinuxChildBackendV1::new(Vec::new());
        let executable = helper_executable();
        let prepared = prepare_confined_helper(
            &executable,
            &readwrite,
            "filesystem_probe",
            TestLimits::default(),
            environment,
            filesystem,
            &mut backend,
        )
        .unwrap();
        assert_eq!(backend.prepared_containment_count_for_test(), 1);

        let (terminal, mut backend) = drive_one_shot_with_backend(prepared, backend);
        assert!(
            matches!(terminal, ChildPhaseSupervisorStateV1::Stopped { .. }),
            "unexpected filesystem terminal state: {terminal:?}"
        );
        let completion = backend.take_completion().expect("filesystem completion");
        let json = fixture_json(completion.stdout(), completion.stderr());
        assert_eq!(json["readonly_contents"], "reviewed");
        for operation in [
            "readonly_create_errno",
            "readonly_truncate_errno",
            "readonly_rename_errno",
            "readwrite_execute_errno",
            "outside_read_errno",
            "outside_write_errno",
            "outside_execute_errno",
            "symlink_escape_errno",
        ] {
            assert_eq!(
                json[operation],
                libc::EACCES,
                "{operation} unexpectedly escaped content authority: {json}"
            );
        }
        assert_eq!(json["readwrite_create_errno"], 0);
        assert_eq!(json["readwrite_rename_errno"], 0);
        assert_eq!(json["readonly_execute_success"].as_bool(), Some(true));

        // Landlock does not hide paths or govern these metadata operations.
        // These observations deliberately prevent this card from claiming a
        // full filesystem sandbox or an MCP-safe mount.
        assert_eq!(json["outside_metadata_visible"].as_bool(), Some(true));
        assert_eq!(json["outside_access_visible"].as_bool(), Some(true));
        assert_eq!(json["outside_chmod_errno"], 0);
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    #[test]
    #[serial]
    fn preparation_seals_exact_launch_bytes_against_post_admission_mutation() {
        let tree = tempfile::tempdir().unwrap();
        let cwd = tree.path().join("work");
        std::fs::create_dir(&cwd).unwrap();
        let executable = tree.path().join("prepared-image");
        std::fs::copy(helper_executable(), &executable).unwrap();
        std::fs::set_permissions(&executable, Permissions::from_mode(0o755)).unwrap();
        let marker = cwd.join("sealed-image-ran");
        let filesystem = confined_filesystem_policy(&cwd, Vec::new());
        let mut backend = LinuxChildBackendV1::new(Vec::new());
        let prepared = prepare_confined_helper(
            &executable,
            &cwd,
            "sealed_marker",
            TestLimits::default(),
            vec![ChildEnvironmentVariableV1::new(
                "KITSU_MARKER_PATH",
                marker.to_string_lossy(),
            )],
            filesystem,
            &mut backend,
        )
        .unwrap();

        let seals = backend.prepared_executable_seals_for_test().unwrap();
        let required =
            libc::F_SEAL_WRITE | libc::F_SEAL_GROW | libc::F_SEAL_SHRINK | libc::F_SEAL_SEAL;
        assert_eq!(seals & required, required);
        OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&executable)
            .unwrap()
            .write_all(b"post-preparation replacement")
            .unwrap();

        let (terminal, mut backend) = drive_one_shot_with_backend(prepared, backend);
        assert!(
            matches!(terminal, ChildPhaseSupervisorStateV1::Stopped { .. }),
            "sealed launch did not complete: {terminal:?}"
        );
        assert_eq!(std::fs::read(&marker).unwrap(), b"sealed bytes ran");
        assert_eq!(
            backend.take_completion().unwrap().truth(),
            RealChildCompletionTruthV1::Success
        );
        assert_eq!(backend.prepared_containment_count_for_test(), 0);
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    #[test]
    #[serial]
    fn prepared_cwd_descriptor_cannot_be_redirected_by_path_replacement() {
        let tree = tempfile::tempdir().unwrap();
        let authorized = tree.path().join("cwd");
        let relocated = tree.path().join("authorized-relocated");
        std::fs::create_dir(&authorized).unwrap();
        std::fs::write(authorized.join("identity"), b"authorized inode").unwrap();
        let filesystem = confined_filesystem_policy(&authorized, Vec::new());
        let executable = helper_executable();
        let mut backend = LinuxChildBackendV1::new(Vec::new());
        let prepared = prepare_confined_helper(
            &executable,
            &authorized,
            "cwd_probe",
            TestLimits::default(),
            Vec::new(),
            filesystem,
            &mut backend,
        )
        .unwrap();

        std::fs::rename(&authorized, &relocated).unwrap();
        std::fs::create_dir(&authorized).unwrap();
        std::fs::write(authorized.join("identity"), b"replacement trap").unwrap();

        let (terminal, mut backend) = drive_one_shot_with_backend(prepared, backend);
        assert!(
            matches!(terminal, ChildPhaseSupervisorStateV1::Stopped { .. }),
            "pinned-cwd fixture failed: {terminal:?}"
        );
        let completion = backend.take_completion().unwrap();
        let json = fixture_json(completion.stdout(), completion.stderr());
        assert_eq!(json["relative_identity"], "authorized inode");
        assert_eq!(json["cwd"], relocated.to_string_lossy().as_ref());
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    #[test]
    #[serial]
    fn unsupported_or_incomplete_preparation_fails_before_target_code_or_spawn() {
        let tree = tempfile::tempdir().unwrap();
        let cwd = tree.path().join("work");
        std::fs::create_dir(&cwd).unwrap();
        let marker = cwd.join("must-not-run");
        let executable = helper_executable();
        for setup in [
            TestFilesystemPreparationSetupV1::UnsupportedLandlockAbi,
            TestFilesystemPreparationSetupV1::MemfdUnavailable,
            TestFilesystemPreparationSetupV1::SealRejected,
            TestFilesystemPreparationSetupV1::RulesetRejected,
        ] {
            let mut backend =
                LinuxChildBackendV1::new_with_filesystem_setup_for_test(Vec::new(), setup);
            let result = prepare_confined_helper(
                &executable,
                &cwd,
                "sealed_marker",
                TestLimits::default(),
                vec![ChildEnvironmentVariableV1::new(
                    "KITSU_MARKER_PATH",
                    marker.to_string_lossy(),
                )],
                confined_filesystem_policy(&cwd, Vec::new()),
                &mut backend,
            );
            assert!(
                matches!(
                    result,
                    Err(ChildLaunchAdmissionErrorV1::ContainmentPreflight(_))
                ),
                "{setup:?} did not fail in preparation: {result:?}"
            );
            assert_eq!(backend.os_spawn_count(), 0, "setup={setup:?}");
            assert_eq!(backend.prepared_containment_count_for_test(), 0);
            assert!(!marker.exists());
        }

        let script = tree.path().join("not-native-elf");
        std::fs::write(&script, b"#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&script, Permissions::from_mode(0o755)).unwrap();
        let mut backend = LinuxChildBackendV1::new(Vec::new());
        let result = prepare_confined_helper(
            &script,
            &cwd,
            "sealed_marker",
            TestLimits::default(),
            vec![ChildEnvironmentVariableV1::new(
                "KITSU_MARKER_PATH",
                marker.to_string_lossy(),
            )],
            confined_filesystem_policy(&cwd, Vec::new()),
            &mut backend,
        );
        assert!(matches!(
            result,
            Err(ChildLaunchAdmissionErrorV1::ContainmentPreflight(detail))
                if detail.contains("native ELF")
        ));
        assert_eq!(backend.os_spawn_count(), 0);
        assert_eq!(backend.prepared_containment_count_for_test(), 0);
        assert!(!marker.exists());
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    #[test]
    fn claim_boundary_keeps_landlock_and_host_authority_gaps_explicit() {
        assert_eq!(
            filesystem_authority_nonclaims_v1(),
            &[
                ChildFilesystemAuthorityNonclaimV1::PathVisibilityStatAndAccess,
                ChildFilesystemAuthorityNonclaimV1::MetadataAndExtendedAttributes,
                ChildFilesystemAuthorityNonclaimV1::FileLocksAndDescriptorControl,
                ChildFilesystemAuthorityNonclaimV1::MountNamespaceIsolation,
                ChildFilesystemAuthorityNonclaimV1::HostSignalsAndPtrace,
            ]
        );
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    #[test]
    fn executable_cap_rejects_a_sparse_cap_plus_one_image_before_memfd_creation() {
        let tree = tempfile::tempdir().unwrap();
        let executable = tree.path().join("oversized-native-image");
        let source = File::create(&executable).unwrap();
        source.set_len(MAX_EXECUTABLE_BYTES_V1 + 1).unwrap();
        drop(source);
        std::fs::set_permissions(&executable, Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            std::fs::metadata(&executable).unwrap().len(),
            MAX_EXECUTABLE_BYTES_V1 + 1
        );

        let mut backend = LinuxChildBackendV1::new(Vec::new());
        let observed = backend.canonicalize_executable(&executable).unwrap();
        let error = backend.executable_sha256(&observed).unwrap_err();
        assert!(
            error.contains("executable exceeds the bounded image cap"),
            "unexpected cap error: {error}"
        );
        assert!(backend.pending_image.is_none());
        assert_eq!(backend.prepared_containment_count_for_test(), 0);
        assert_eq!(backend.os_spawn_count(), 0);
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    #[test]
    #[serial]
    fn real_serial_transport_reuses_one_sealed_child_for_three_frames() {
        let cwd = tempfile::tempdir().unwrap();
        let executable = Path::new("/usr/bin/bash");
        let arguments = vec![
            "--noprofile".to_string(),
            "--norc".to_string(),
            "-c".to_string(),
            "while IFS= read -r line; do printf '%s\\n' \"$line\"; done".to_string(),
        ];
        let (prepared, mut backend) = prepared_serial_program(
            cwd.path(),
            executable,
            arguments,
            TestLimits::default(),
            3,
            250,
        );
        {
            let mut transport = SerialChildTransportV1::new(prepared, &mut backend);
            start_real_serial(&mut transport);
            let pgid = transport
                .effects_for_test()
                .active_pgid_for_test()
                .expect("one child group");
            for frame in [b"one\n".to_vec(), b"two\n".to_vec(), b"three\n".to_vec()] {
                transport.begin_frame(frame.clone()).unwrap();
                assert_eq!(poll_real_serial_response(&mut transport), frame);
                assert_eq!(
                    transport.effects_for_test().active_pgid_for_test(),
                    Some(pgid)
                );
            }
            assert!(matches!(
                finish_real_serial(&mut transport, SerialChildCauseV1::Shutdown),
                SerialChildTransportStateV1::Stopped {
                    cause: SerialChildCauseV1::Shutdown,
                    ..
                }
            ));
        }
        assert_eq!(backend.os_spawn_count(), 1);
        assert_eq!(backend.request_start_count(), 3);
        assert!(backend.take_serial_retained_residue().is_none());
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    #[test]
    #[serial]
    fn real_serial_partial_line_hits_idle_timeout_and_reaps_without_response() {
        let cwd = tempfile::tempdir().unwrap();
        let executable = Path::new("/usr/bin/bash");
        let arguments = vec![
            "--noprofile".to_string(),
            "--norc".to_string(),
            "-c".to_string(),
            "IFS= read -r line; printf partial; IFS= read -r blocked".to_string(),
        ];
        let limits = TestLimits {
            request_millis: 500,
            ..TestLimits::default()
        };
        let (prepared, mut backend) =
            prepared_serial_program(cwd.path(), executable, arguments, limits, 2, 20);
        let terminal;
        {
            let mut transport = SerialChildTransportV1::new(prepared, &mut backend);
            start_real_serial(&mut transport);
            transport.begin_frame(b"request\n".to_vec()).unwrap();
            terminal = drive_real_serial_failure(&mut transport);
        }
        assert!(matches!(
            terminal,
            SerialChildTransportStateV1::Failed(SerialChildTransportFailureV1::Transport {
                cause: SerialChildCauseV1::IdleTimeout,
                ..
            })
        ));
        assert_eq!(backend.os_spawn_count(), 1);
        assert!(backend.take_serial_retained_residue().is_none());
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    #[test]
    #[serial]
    fn real_serial_eof_and_crash_after_line_are_never_response_success() {
        let cases = [
            ("IFS= read -r line; exit 0", SerialChildCauseV1::Eof),
            (
                "IFS= read -r line; printf 'not-success\\n'; exit 17",
                SerialChildCauseV1::Crash,
            ),
        ];
        for (script, expected_cause) in cases {
            let cwd = tempfile::tempdir().unwrap();
            let executable = Path::new("/usr/bin/bash");
            let arguments = vec![
                "--noprofile".to_string(),
                "--norc".to_string(),
                "-c".to_string(),
                script.to_string(),
            ];
            let (prepared, mut backend) = prepared_serial_program(
                cwd.path(),
                executable,
                arguments,
                TestLimits::default(),
                2,
                250,
            );
            let terminal;
            {
                let mut transport = SerialChildTransportV1::new(prepared, &mut backend);
                start_real_serial(&mut transport);
                transport.begin_frame(b"request\n".to_vec()).unwrap();
                terminal = drive_real_serial_failure(&mut transport);
            }
            assert!(
                matches!(
                    terminal,
                    SerialChildTransportStateV1::Failed(
                        SerialChildTransportFailureV1::Transport { cause, .. }
                    ) if cause == expected_cause
                ),
                "script={script:?}, expected={expected_cause:?}, terminal={terminal:?}"
            );
            assert_eq!(backend.os_spawn_count(), 1);
            assert!(backend.take_serial_retained_residue().is_none());
        }
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    #[test]
    #[serial]
    fn real_serial_cancel_and_shutdown_reap_the_owned_descendant_group() {
        for cause in [SerialChildCauseV1::Cancel, SerialChildCauseV1::Shutdown] {
            let cwd = tempfile::tempdir().unwrap();
            let marker = cwd.path().join("serial-descendant-pids");
            let executable = Path::new("/usr/bin/bash");
            let arguments = vec![
                "--noprofile".to_string(),
                "--norc".to_string(),
                "-c".to_string(),
                "sleep 30 & descendant=$!; printf '%s %s\\n' \"$$\" \"$descendant\" > \"$1\"; wait"
                    .to_string(),
                "kitsu-serial-fixture".to_string(),
                marker.to_string_lossy().into_owned(),
            ];
            let (prepared, mut backend) = prepared_serial_program(
                cwd.path(),
                executable,
                arguments,
                TestLimits::default(),
                2,
                250,
            );
            {
                let mut transport = SerialChildTransportV1::new(prepared, &mut backend);
                start_real_serial(&mut transport);
                let wall_deadline = Instant::now() + Duration::from_secs(5);
                while !marker.exists() {
                    assert!(Instant::now() < wall_deadline, "descendant marker missing");
                    std::thread::sleep(Duration::from_millis(1));
                }
                assert!(matches!(
                    finish_real_serial(&mut transport, cause),
                    SerialChildTransportStateV1::Stopped {
                        cause: observed,
                        ..
                    } if observed == cause
                ));
            }
            let pids = std::fs::read_to_string(&marker).unwrap();
            let mut pids = pids
                .split_whitespace()
                .map(|value| value.parse::<i32>().unwrap());
            let pgid = pids.next().unwrap();
            let descendant = pids.next().unwrap();
            assert!(
                process_group_is_gone(pgid),
                "group {pgid} survived {cause:?}"
            );
            assert!(
                process_is_gone(descendant),
                "descendant {descendant} survived {cause:?}"
            );
            assert!(backend.take_serial_retained_residue().is_none());
        }
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    #[test]
    #[serial]
    fn real_mcp_tools_only_exchange_is_exact_and_one_sealed_pid() {
        let cwd = tempfile::tempdir().unwrap();
        let (prepared, mut backend) = prepared_real_mcp_peer(cwd.path(), RealMcpPeerModeV1::Exact);
        assert_eq!(backend.prepared_containment_count_for_test(), 1);
        assert_eq!(
            backend.prepared_executable_seals_for_test(),
            Some(libc::F_SEAL_WRITE | libc::F_SEAL_GROW | libc::F_SEAL_SHRINK | libc::F_SEAL_SEAL)
        );
        let pgid;
        {
            let mut owner = McpSerialChildOwnerV1::new(prepared, &mut backend).unwrap();
            complete_real_mcp_exchange(&mut owner);
            pgid = owner
                .effects_for_test()
                .active_pgid_for_test()
                .expect("one live MCP process group");
            assert_eq!(owner.effects_for_test().os_spawn_count(), 1);
            assert_eq!(owner.effects_for_test().request_start_count(), 4);
            assert_eq!(
                owner.response_frames_for_test(),
                &[
                    MCP_INITIALIZE_RESPONSE_LINE_V1.to_vec(),
                    MCP_LIST_RESPONSE_LINE_V1.to_vec(),
                    MCP_CALL_RESPONSE_LINE_V1.to_vec(),
                ]
            );
            owner.shutdown().unwrap();
            drive_real_mcp_terminal(&mut owner);
            assert_eq!(owner.state(), &McpSerialChildStateV1::Stopped);
            assert!(matches!(
                owner.transport_state_for_test(),
                SerialChildTransportStateV1::Stopped {
                    cause: SerialChildCauseV1::Shutdown,
                    ..
                }
            ));
        }
        assert_eq!(backend.os_spawn_count(), 1);
        assert_eq!(backend.request_start_count(), 4);
        assert!(process_group_is_gone(pgid));
        assert!(backend.take_serial_retained_residue().is_none());
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    #[test]
    #[serial]
    fn real_mcp_echo_source_publishes_calls_withdraws_and_reaps_one_sealed_peer() {
        let cwd = tempfile::tempdir().unwrap();
        let (prepared, mut backend) = prepared_real_mcp_peer(cwd.path(), RealMcpPeerModeV1::Exact);
        assert_eq!(backend.prepared_containment_count_for_test(), 1);
        assert_eq!(
            backend.prepared_executable_seals_for_test(),
            Some(libc::F_SEAL_WRITE | libc::F_SEAL_GROW | libc::F_SEAL_SHRINK | libc::F_SEAL_SEAL)
        );
        let pgid;
        {
            let mut owner = ExternalMcpEchoSourceOwnerV1::new(
                prepared,
                &mut backend,
                "fixture-source",
                "sealed-boot-1",
                1,
                "echo",
            )
            .unwrap();
            owner.start().unwrap();
            let wall_deadline = Instant::now() + Duration::from_secs(5);
            while !matches!(owner.state(), ExternalMcpEchoSourceStateV1::PublishedReady) {
                assert!(
                    Instant::now() < wall_deadline,
                    "echo source hung before publication: {:?}",
                    owner.state()
                );
                assert!(
                    !matches!(owner.state(), ExternalMcpEchoSourceStateV1::Failed { .. }),
                    "echo source failed before publication: {:?} / {:?}",
                    owner.state(),
                    owner.failure()
                );
                owner.poll().unwrap();
                std::thread::sleep(Duration::from_millis(1));
            }
            assert_eq!(owner.publication().unwrap().tools().len(), 1);
            assert_eq!(owner.binding().unwrap().wire_name(), "fixture.echo");
            pgid = owner
                .effects_for_test()
                .active_pgid_for_test()
                .expect("one live MCP process group");
            let host = HostExternalToolGrantV1::from_binding(owner.binding().unwrap());
            let caller = CallerExternalToolGrantV1::from_binding(owner.binding().unwrap());
            owner
                .begin_echo(FixtureEchoRequestV1::new(7), &host, &caller)
                .unwrap();
            while !matches!(owner.state(), ExternalMcpEchoSourceStateV1::Completed) {
                assert!(
                    Instant::now() < wall_deadline,
                    "echo source hung before completion: {:?}",
                    owner.state()
                );
                assert!(
                    !matches!(owner.state(), ExternalMcpEchoSourceStateV1::Failed { .. }),
                    "echo source failed before completion: {:?} / {:?}",
                    owner.state(),
                    owner.failure()
                );
                owner.poll().unwrap();
                std::thread::sleep(Duration::from_millis(1));
            }
            assert_eq!(owner.take_result().unwrap().value(), 7);
            assert_eq!(
                owner.completion(),
                Some(ExternalMcpEchoCompletionV1::EchoValidated)
            );
            assert!(owner.publication().is_none());
            assert!(owner.binding().is_none());
            assert_eq!(owner.effects_for_test().os_spawn_count(), 1);
            assert_eq!(owner.effects_for_test().request_start_count(), 4);
            assert_eq!(
                owner.response_frames_for_test(),
                &[
                    MCP_INITIALIZE_RESPONSE_LINE_V1.to_vec(),
                    MCP_LIST_RESPONSE_LINE_V1.to_vec(),
                    MCP_CALL_RESPONSE_LINE_V1.to_vec(),
                ]
            );
        }
        assert_eq!(backend.os_spawn_count(), 1);
        assert_eq!(backend.request_start_count(), 4);
        assert!(process_group_is_gone(pgid));
        assert!(backend.take_serial_retained_residue().is_none());
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    #[test]
    #[serial]
    fn real_mcp_peer_eof_crash_wrong_id_and_malformed_fail_closed_and_reap() {
        for mode in [
            RealMcpPeerModeV1::Eof,
            RealMcpPeerModeV1::Crash,
            RealMcpPeerModeV1::WrongId,
            RealMcpPeerModeV1::Malformed,
        ] {
            let cwd = tempfile::tempdir().unwrap();
            let (prepared, mut backend) = prepared_real_mcp_peer(cwd.path(), mode);
            let pgid;
            {
                let mut owner = McpSerialChildOwnerV1::new(prepared, &mut backend).unwrap();
                owner.start().unwrap();
                pgid = owner
                    .effects_for_test()
                    .active_pgid_for_test()
                    .expect("one live MCP process group");
                drive_real_mcp_terminal(&mut owner);
                assert!(owner.take_event().is_err(), "mode={mode:?}");
                match mode {
                    RealMcpPeerModeV1::Eof => {
                        assert!(
                            matches!(
                                owner.state(),
                                McpSerialChildStateV1::Failed {
                                    failure: McpSerialChildFailureV1::Cleanup {
                                        primary,
                                        transport: SerialChildTransportFailureV1::Transport {
                                            cause: SerialChildCauseV1::Eof,
                                            exit: ChildExitTruthV1::Exited(0),
                                        },
                                    }
                                } if matches!(
                                    primary.as_ref(),
                                    McpSerialChildFailureV1::TransportCause(SerialChildCauseV1::Eof)
                                )
                            ),
                            "mode={mode:?}, owner={:?}, transport={:?}",
                            owner.state(),
                            owner.transport_state_for_test()
                        );
                        assert!(
                            matches!(
                                owner.transport_state_for_test(),
                                SerialChildTransportStateV1::Failed(
                                    SerialChildTransportFailureV1::Transport {
                                        cause: SerialChildCauseV1::Eof,
                                        exit: ChildExitTruthV1::Exited(0),
                                    }
                                )
                            ),
                            "mode={mode:?}, owner={:?}, transport={:?}",
                            owner.state(),
                            owner.transport_state_for_test()
                        );
                    }
                    RealMcpPeerModeV1::Crash => {
                        assert!(
                            matches!(
                                owner.state(),
                                McpSerialChildStateV1::Failed {
                                    failure: McpSerialChildFailureV1::Cleanup {
                                        primary,
                                        transport: SerialChildTransportFailureV1::Transport {
                                            cause: SerialChildCauseV1::Crash,
                                            exit: ChildExitTruthV1::Exited(17),
                                        },
                                    }
                                } if matches!(
                                    primary.as_ref(),
                                    McpSerialChildFailureV1::TransportCause(SerialChildCauseV1::Crash)
                                )
                            ),
                            "mode={mode:?}, owner={:?}, transport={:?}",
                            owner.state(),
                            owner.transport_state_for_test()
                        );
                        assert!(
                            matches!(
                                owner.transport_state_for_test(),
                                SerialChildTransportStateV1::Failed(
                                    SerialChildTransportFailureV1::Transport {
                                        cause: SerialChildCauseV1::Crash,
                                        exit: ChildExitTruthV1::Exited(17),
                                    }
                                )
                            ),
                            "mode={mode:?}, owner={:?}, transport={:?}",
                            owner.state(),
                            owner.transport_state_for_test()
                        );
                    }
                    RealMcpPeerModeV1::WrongId => {
                        assert!(
                            matches!(
                                owner.state(),
                                McpSerialChildStateV1::Failed {
                                    failure: McpSerialChildFailureV1::Cleanup {
                                        primary,
                                        transport: SerialChildTransportFailureV1::Transport {
                                            cause: SerialChildCauseV1::ProtocolViolation,
                                            exit: ChildExitTruthV1::Signalled,
                                        },
                                    }
                                } if matches!(
                                    primary.as_ref(),
                                    McpSerialChildFailureV1::Client(
                                        ClientError::UnexpectedResponseId {
                                            expected: Some(1),
                                            actual: 99,
                                        }
                                    )
                                )
                            ),
                            "mode={mode:?}, owner={:?}, transport={:?}",
                            owner.state(),
                            owner.transport_state_for_test()
                        );
                        assert!(
                            matches!(
                                owner.transport_state_for_test(),
                                SerialChildTransportStateV1::Failed(
                                    SerialChildTransportFailureV1::Transport {
                                        cause: SerialChildCauseV1::ProtocolViolation,
                                        exit: ChildExitTruthV1::Signalled,
                                    }
                                )
                            ),
                            "mode={mode:?}, owner={:?}, transport={:?}",
                            owner.state(),
                            owner.transport_state_for_test()
                        );
                    }
                    RealMcpPeerModeV1::Malformed => {
                        assert!(
                            matches!(
                                owner.state(),
                                McpSerialChildStateV1::Failed {
                                    failure: McpSerialChildFailureV1::Cleanup {
                                        primary,
                                        transport: SerialChildTransportFailureV1::Transport {
                                            cause: SerialChildCauseV1::ProtocolViolation,
                                            exit: ChildExitTruthV1::Signalled,
                                        },
                                    }
                                } if matches!(
                                    primary.as_ref(),
                                    McpSerialChildFailureV1::Codec(CodecError::InvalidJson { .. })
                                )
                            ),
                            "mode={mode:?}, owner={:?}, transport={:?}",
                            owner.state(),
                            owner.transport_state_for_test()
                        );
                        assert!(
                            matches!(
                                owner.transport_state_for_test(),
                                SerialChildTransportStateV1::Failed(
                                    SerialChildTransportFailureV1::Transport {
                                        cause: SerialChildCauseV1::ProtocolViolation,
                                        exit: ChildExitTruthV1::Signalled,
                                    }
                                )
                            ),
                            "mode={mode:?}, owner={:?}, transport={:?}",
                            owner.state(),
                            owner.transport_state_for_test()
                        );
                    }
                    _ => unreachable!("closed failure-mode table"),
                }
                assert_eq!(owner.effects_for_test().os_spawn_count(), 1);
                assert_eq!(owner.effects_for_test().request_start_count(), 1);
            }
            assert!(process_group_is_gone(pgid), "mode={mode:?}, pgid={pgid}");
            assert!(
                backend.take_serial_retained_residue().is_none(),
                "mode={mode:?}"
            );
        }
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    #[test]
    #[serial]
    fn real_mcp_cancel_and_shutdown_reap_owned_descendant_group() {
        for cause in [SerialChildCauseV1::Cancel, SerialChildCauseV1::Shutdown] {
            let cwd = tempfile::tempdir().unwrap();
            let marker = cwd.path().join("mcp-descendant-pids");
            let (prepared, mut backend) =
                prepared_real_mcp_peer(cwd.path(), RealMcpPeerModeV1::Descendant);
            let (pgid, descendant);
            {
                let mut owner = McpSerialChildOwnerV1::new(prepared, &mut backend).unwrap();
                complete_real_mcp_exchange(&mut owner);
                let wall_deadline = Instant::now() + Duration::from_secs(5);
                while !marker.exists() {
                    assert!(Instant::now() < wall_deadline, "MCP PID marker missing");
                    std::thread::sleep(Duration::from_millis(1));
                }
                let pids = std::fs::read_to_string(&marker).unwrap();
                let pids = pids
                    .split_whitespace()
                    .map(|value| value.parse::<i32>().unwrap())
                    .collect::<Vec<_>>();
                assert_eq!(pids.len(), 3);
                let direct = pids[0];
                pgid = pids[1];
                descendant = pids[2];
                assert_eq!(direct, pgid);
                assert_ne!(descendant, direct);
                assert_eq!(owner.effects_for_test().active_pgid_for_test(), Some(pgid));
                match cause {
                    SerialChildCauseV1::Cancel => {
                        owner.cancel().unwrap();
                    }
                    SerialChildCauseV1::Shutdown => {
                        owner.shutdown().unwrap();
                    }
                    _ => unreachable!("closed explicit-stop table"),
                }
                drive_real_mcp_terminal(&mut owner);
                assert_eq!(owner.state(), &McpSerialChildStateV1::Stopped);
                assert!(matches!(
                    owner.transport_state_for_test(),
                    SerialChildTransportStateV1::Stopped {
                        cause: observed,
                        ..
                    } if *observed == cause
                ));
            }
            assert!(
                process_group_is_gone(pgid),
                "group {pgid} survived {cause:?}"
            );
            assert!(
                process_is_gone(descendant),
                "descendant {descendant} survived {cause:?}"
            );
            assert!(backend.take_serial_retained_residue().is_none());
        }
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
            "sealed_marker" => {
                std::fs::write(
                    std::env::var("KITSU_MARKER_PATH").unwrap(),
                    b"sealed bytes ran",
                )
                .unwrap();
            }
            "filesystem_probe" => child_filesystem_probe(),
            "cwd_probe" => {
                let result = serde_json::json!({
                    "cwd": std::env::current_dir().unwrap(),
                    "relative_identity": std::fs::read_to_string("identity").unwrap(),
                });
                println!("KITSU_FIXTURE_JSON={result}");
            }
            other => panic!("unknown child fixture {other}"),
        }
    }

    fn child_filesystem_probe() {
        let readonly = PathBuf::from(std::env::var("KITSU_FS_READONLY").unwrap());
        let readwrite = PathBuf::from(std::env::var("KITSU_FS_READWRITE").unwrap());
        let outside = PathBuf::from(std::env::var("KITSU_FS_OUTSIDE").unwrap());
        let readwrite_executable = PathBuf::from(std::env::var("KITSU_FS_RW_EXECUTABLE").unwrap());
        let outside_executable =
            PathBuf::from(std::env::var("KITSU_FS_OUTSIDE_EXECUTABLE").unwrap());
        let outside_secret = outside.join("secret.txt");
        let access_path = CString::new(outside_secret.as_os_str().as_bytes()).unwrap();

        let readonly_contents = std::fs::read_to_string(readonly.join("reviewed.txt")).unwrap();
        let readonly_create_errno =
            result_errno(std::fs::write(readonly.join("must-not-create"), b"denied"));
        let readonly_truncate_errno = result_errno(
            OpenOptions::new()
                .write(true)
                .truncate(true)
                .open(readonly.join("reviewed.txt")),
        );
        let readonly_rename_errno = result_errno(std::fs::rename(
            readonly.join("reviewed.txt"),
            readwrite.join("must-not-move"),
        ));
        let readwrite_create_errno =
            result_errno(std::fs::write(readwrite.join("created"), b"created"));
        let readwrite_rename_errno = result_errno(std::fs::rename(
            readwrite.join("created"),
            readwrite.join("renamed"),
        ));
        let readonly_execute_success = std::process::Command::new("/usr/bin/true")
            .status()
            .is_ok_and(|status| status.success());
        let readwrite_execute_errno =
            result_errno(std::process::Command::new(readwrite_executable).status());
        let outside_read_errno = result_errno(std::fs::read(&outside_secret));
        let outside_write_errno =
            result_errno(std::fs::write(outside.join("must-not-create"), b"denied"));
        let outside_execute_errno =
            result_errno(std::process::Command::new(outside_executable).status());
        let symlink_escape_errno = result_errno(std::fs::read(readwrite.join("escape/secret.txt")));
        let outside_metadata_visible = std::fs::metadata(&outside_secret).is_ok();
        // SAFETY: the CString is NUL-terminated and points to live storage.
        let outside_access_visible = unsafe { libc::access(access_path.as_ptr(), libc::F_OK) } == 0;
        let outside_chmod_errno = result_errno(std::fs::set_permissions(
            &outside_secret,
            Permissions::from_mode(0o640),
        ));
        let result = serde_json::json!({
            "readonly_contents": readonly_contents,
            "readonly_create_errno": readonly_create_errno,
            "readonly_truncate_errno": readonly_truncate_errno,
            "readonly_rename_errno": readonly_rename_errno,
            "readwrite_create_errno": readwrite_create_errno,
            "readwrite_rename_errno": readwrite_rename_errno,
            "readonly_execute_success": readonly_execute_success,
            "readwrite_execute_errno": readwrite_execute_errno,
            "outside_read_errno": outside_read_errno,
            "outside_write_errno": outside_write_errno,
            "outside_execute_errno": outside_execute_errno,
            "symlink_escape_errno": symlink_escape_errno,
            "outside_metadata_visible": outside_metadata_visible,
            "outside_access_visible": outside_access_visible,
            "outside_chmod_errno": outside_chmod_errno,
        });
        println!("KITSU_FIXTURE_JSON={result}");
    }

    fn result_errno<T>(result: std::io::Result<T>) -> i32 {
        result.map_or_else(
            |error| error.raw_os_error().unwrap_or(0),
            |value| {
                drop(value);
                0
            },
        )
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
