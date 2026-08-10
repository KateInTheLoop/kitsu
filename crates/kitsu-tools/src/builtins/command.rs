//! Guarded developer-command execution.
//!
//! Command execution is intentionally a separate authority from filesystem
//! [`crate::permissions::Permissions`]. A command root only constrains the
//! process working directory; it does not grant any command. Every exact
//! `(argv, canonical cwd)` invocation must be pre-granted or approved through a
//! [`CommandPrompter`]. There is deliberately no auto-allow mode.
//!
//! Input is an argv array, not a command-line string. Kitsu invokes the
//! requested program directly without an intermediary shell or argument
//! expansion. The child has no stdin, stdout and stderr are drained into
//! independently bounded buffers, and both timeout and caller-future
//! cancellation kill and reap the child.

use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::fd::AsRawFd;
#[cfg(any(target_os = "linux", target_os = "android"))]
use std::os::fd::FromRawFd;

use kitsu_providers::ToolDescriptor;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt};

use crate::{HandlerFn, ToolError};

/// Default wall-clock limit for one command.
pub const DEFAULT_COMMAND_TIMEOUT: Duration = Duration::from_secs(120);
/// Maximum wall-clock limit a tool call may request under the default policy.
pub const DEFAULT_MAX_COMMAND_TIMEOUT: Duration = Duration::from_secs(600);
/// Maximum retained bytes for each of stdout and stderr under the default policy.
pub const DEFAULT_COMMAND_OUTPUT_BYTES: usize = 64 * 1024;
/// Time allowed for captured pipes to reach EOF after the direct child exits.
/// A detached descendant must not make command completion unbounded.
const PIPE_DRAIN_GRACE: Duration = Duration::from_millis(250);

/// How an invocation without an exact session grant is handled.
///
/// Unlike filesystem permissions, command policy has no auto-allow mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandMode {
    /// Ask the command-specific prompter. A missing prompter fails closed.
    Ask,
    /// Deny invocations that do not already have an exact grant.
    Deny,
}

/// The user's answer to a command-specific approval prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandDecision {
    /// Run this invocation without changing session grants.
    AllowOnce,
    /// Grant this exact argv in this exact canonical cwd for the session.
    AllowForSession,
    /// Do not run the invocation.
    Deny,
}

/// Details presented for command-specific approval.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandApprovalRequest {
    /// Exact argv that will be passed to the operating system.
    pub argv: Vec<String>,
    /// Canonical working directory, already checked against command roots.
    pub cwd: String,
    /// Effective timeout after applying the command policy.
    pub timeout_ms: u64,
}

impl CommandApprovalRequest {
    /// Human-readable, control-character-escaped argv. This is quoted for a
    /// prompt and is never parsed back into the argv used for execution.
    pub fn display_argv(&self) -> String {
        display_argv(&self.argv)
    }
}

/// UI boundary for command approval. The CLI can implement this independently
/// of its filesystem permission prompt.
pub trait CommandPrompter: Send + Sync {
    fn ask(&self, request: &CommandApprovalRequest) -> CommandDecision;
}

/// Execution limits owned by command policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandLimits {
    default_timeout: Duration,
    max_timeout: Duration,
    /// Retained bytes per stream. Pipes continue to be drained after this cap.
    max_output_bytes: usize,
}

impl CommandLimits {
    pub fn new(
        default_timeout: Duration,
        max_timeout: Duration,
        max_output_bytes: usize,
    ) -> Result<Self, ToolError> {
        if default_timeout.is_zero() {
            return Err(ToolError::InvalidArgs(
                "default command timeout must be greater than zero".to_string(),
            ));
        }
        if max_timeout < default_timeout {
            return Err(ToolError::InvalidArgs(
                "maximum command timeout must be at least the default timeout".to_string(),
            ));
        }
        if max_output_bytes == 0 {
            return Err(ToolError::InvalidArgs(
                "command output cap must be greater than zero".to_string(),
            ));
        }
        Ok(Self {
            default_timeout,
            max_timeout,
            max_output_bytes,
        })
    }

    pub fn default_timeout(self) -> Duration {
        self.default_timeout
    }

    pub fn max_timeout(self) -> Duration {
        self.max_timeout
    }

    pub fn max_output_bytes(self) -> usize {
        self.max_output_bytes
    }
}

impl Default for CommandLimits {
    fn default() -> Self {
        Self {
            default_timeout: DEFAULT_COMMAND_TIMEOUT,
            max_timeout: DEFAULT_MAX_COMMAND_TIMEOUT,
            max_output_bytes: DEFAULT_COMMAND_OUTPUT_BYTES,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CommandGrant {
    argv: Vec<String>,
    cwd: PathBuf,
}

/// A working directory whose filesystem identity was captured during
/// authorization. On Unix the open descriptor is also how the child enters
/// the directory, so a pathname replacement after the final check cannot
/// redirect execution to a different directory.
#[derive(Debug)]
struct AuthorizedCwd {
    canonical: PathBuf,
    #[cfg(unix)]
    directory: std::fs::File,
}

impl AuthorizedCwd {
    fn bind(
        policy: &CommandPolicy,
        requested: &Path,
        canonical: PathBuf,
    ) -> Result<Self, ToolError> {
        #[cfg(unix)]
        let directory = open_directory(&canonical).map_err(|error| {
            ToolError::PolicyDenied(format!(
                "command cwd '{}' could not be bound for execution: {error}",
                requested.display()
            ))
        })?;
        let authorized = Self {
            canonical,
            #[cfg(unix)]
            directory,
        };

        // Close the canonicalize-to-open race as well as possible: the path
        // must still resolve to the descriptor we just opened.
        authorized.revalidate(policy, requested)?;
        Ok(authorized)
    }

    fn revalidate(&self, policy: &CommandPolicy, requested: &Path) -> Result<(), ToolError> {
        let Ok(current) = policy.resolve_cwd(requested) else {
            return Err(cwd_changed_after_authorization(requested));
        };
        if current != self.canonical {
            return Err(cwd_changed_after_authorization(requested));
        }

        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;

            let descriptor = self
                .directory
                .metadata()
                .map_err(|_| cwd_changed_after_authorization(requested))?;
            let path = std::fs::metadata(&current)
                .map_err(|_| cwd_changed_after_authorization(requested))?;
            if !descriptor.is_dir()
                || !path.is_dir()
                || descriptor.dev() != path.dev()
                || descriptor.ino() != path.ino()
            {
                return Err(cwd_changed_after_authorization(requested));
            }
        }

        Ok(())
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn open_directory(path: &Path) -> std::io::Result<std::fs::File> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let path = CString::new(path.as_os_str().as_bytes()).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "directory path contains a NUL byte",
        )
    })?;
    // O_PATH needs only search permission and gives us a stable reference to
    // the directory without granting read access. Linux supports fchdir on an
    // O_PATH directory descriptor.
    // SAFETY: `path` is a live, NUL-terminated C string and `open` does not
    // retain its pointer. A successful descriptor is uniquely transferred to
    // `File`, which closes it on drop.
    let descriptor = unsafe {
        libc::open(
            path.as_ptr(),
            libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    };
    if descriptor == -1 {
        Err(std::io::Error::last_os_error())
    } else {
        // SAFETY: `descriptor` was returned successfully by `open` and is not
        // owned anywhere else.
        Ok(unsafe { std::fs::File::from_raw_fd(descriptor) })
    }
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
fn open_directory(path: &Path) -> std::io::Result<std::fs::File> {
    // Portable Unix fallback. Some platforms lack an O_PATH/O_SEARCH
    // equivalent, so opening an execute-only directory may fail closed.
    std::fs::File::open(path)
}

fn cwd_changed_after_authorization(requested: &Path) -> ToolError {
    ToolError::PolicyDenied(format!(
        "command cwd '{}' changed after authorization; refusing to execute",
        requested.display()
    ))
}

/// Standalone authority for developer commands.
///
/// Construction does not enable a tool by itself; callers must explicitly
/// register [`command_exec_tool`] (or call
/// [`super::register_command_tool`](crate::builtins::register_command_tool)).
pub struct CommandPolicy {
    command_roots: Vec<PathBuf>,
    grants: RwLock<Vec<CommandGrant>>,
    mode: CommandMode,
    prompter: Option<Arc<dyn CommandPrompter>>,
    limits: CommandLimits,
}

impl CommandPolicy {
    /// Create a deny-by-default command authority. Every root is canonicalized
    /// now and must be an existing directory.
    pub fn new(command_roots: &[String], mode: CommandMode) -> Result<Self, ToolError> {
        let mut roots = Vec::with_capacity(command_roots.len());
        for root in command_roots {
            let canonical = std::fs::canonicalize(root).map_err(|error| {
                ToolError::PolicyDenied(format!("command root '{root}' is not accessible: {error}"))
            })?;
            if !canonical.is_dir() {
                return Err(ToolError::PolicyDenied(format!(
                    "command root '{root}' is not a directory"
                )));
            }
            insert_minimal_root(&mut roots, canonical);
        }
        Ok(Self {
            command_roots: roots,
            grants: RwLock::new(Vec::new()),
            mode,
            prompter: None,
            limits: CommandLimits::default(),
        })
    }

    /// Attach a command-specific prompter (used only in [`CommandMode::Ask`]).
    pub fn with_prompter(mut self, prompter: Arc<dyn CommandPrompter>) -> Self {
        self.prompter = Some(prompter);
        self
    }

    /// Replace the default timeout and output bounds.
    pub fn with_limits(mut self, limits: CommandLimits) -> Self {
        self.limits = limits;
        self
    }

    pub fn mode(&self) -> CommandMode {
        self.mode
    }

    pub fn limits(&self) -> CommandLimits {
        self.limits
    }

    /// Snapshot the canonical roots in which commands may start.
    pub fn command_roots(&self) -> Vec<PathBuf> {
        self.command_roots.clone()
    }

    /// Add an exact session grant without prompting. This is intended for an
    /// explicit command allowlist in future CLI/config wiring; it still refuses
    /// a cwd outside the command roots.
    pub fn grant_command(&self, argv: &[String], cwd: &Path) -> Result<PathBuf, ToolError> {
        validate_argv(argv)?;
        let cwd = self.resolve_cwd(cwd)?;
        self.insert_grant(argv.to_vec(), cwd.clone());
        Ok(cwd)
    }

    /// Revoke all exact command grants accumulated in this policy.
    pub fn reset_session_grants(&self) {
        self.grants.write().unwrap().clear();
    }

    /// Whether this exact argv/cwd pair currently has a session grant.
    pub fn is_granted(&self, argv: &[String], cwd: &Path) -> bool {
        let Ok(cwd) = self.resolve_cwd(cwd) else {
            return false;
        };
        self.grants
            .read()
            .unwrap()
            .iter()
            .any(|grant| grant.argv == argv && grant.cwd == cwd)
    }

    fn resolve_cwd(&self, requested: &Path) -> Result<PathBuf, ToolError> {
        let canonical = std::fs::canonicalize(requested).map_err(|error| {
            ToolError::PolicyDenied(format!(
                "command cwd '{}' is not accessible: {error}",
                requested.display()
            ))
        })?;
        if !canonical.is_dir() {
            return Err(ToolError::PolicyDenied(format!(
                "command cwd '{}' is not a directory",
                requested.display()
            )));
        }
        if !self
            .command_roots
            .iter()
            .any(|root| canonical.starts_with(root))
        {
            return Err(ToolError::PolicyDenied(format!(
                "command cwd '{}' is outside the approved command roots",
                requested.display()
            )));
        }
        Ok(canonical)
    }

    async fn authorize(
        &self,
        argv: &[String],
        requested_cwd: &Path,
        timeout: Duration,
    ) -> Result<AuthorizedCwd, ToolError> {
        validate_argv(argv)?;
        let cwd = self.resolve_cwd(requested_cwd)?;
        let authorized_cwd = AuthorizedCwd::bind(self, requested_cwd, cwd.clone())?;
        if self
            .grants
            .read()
            .unwrap()
            .iter()
            .any(|grant| grant.argv == argv && grant.cwd == cwd)
        {
            return Ok(authorized_cwd);
        }

        match self.mode {
            CommandMode::Deny => Err(unapproved(argv, &cwd)),
            CommandMode::Ask => {
                let request = CommandApprovalRequest {
                    argv: argv.to_vec(),
                    cwd: cwd.display().to_string(),
                    timeout_ms: duration_millis(timeout),
                };
                match self.prompt(request).await {
                    CommandDecision::AllowOnce => Ok(authorized_cwd),
                    CommandDecision::AllowForSession => {
                        self.insert_grant(argv.to_vec(), cwd.clone());
                        Ok(authorized_cwd)
                    }
                    CommandDecision::Deny => Err(ToolError::PolicyDenied(format!(
                        "user denied command {} in '{}'",
                        display_argv(argv),
                        cwd.display()
                    ))),
                }
            }
        }
    }

    fn insert_grant(&self, argv: Vec<String>, cwd: PathBuf) {
        let mut grants = self.grants.write().unwrap();
        if !grants
            .iter()
            .any(|grant| grant.argv == argv && grant.cwd == cwd)
        {
            grants.push(CommandGrant { argv, cwd });
        }
    }

    async fn prompt(&self, request: CommandApprovalRequest) -> CommandDecision {
        match &self.prompter {
            Some(prompter) => {
                let prompter = prompter.clone();
                tokio::task::spawn_blocking(move || prompter.ask(&request))
                    .await
                    .unwrap_or(CommandDecision::Deny)
            }
            None => CommandDecision::Deny,
        }
    }
}

fn insert_minimal_root(roots: &mut Vec<PathBuf>, root: PathBuf) {
    if roots.iter().any(|existing| root.starts_with(existing)) {
        return;
    }
    roots.retain(|existing| !existing.starts_with(&root));
    roots.push(root);
    roots.sort();
}

fn validate_argv(argv: &[String]) -> Result<(), ToolError> {
    if argv.is_empty() {
        return Err(ToolError::InvalidArgs(
            "argv must contain a program".to_string(),
        ));
    }
    if argv[0].is_empty() {
        return Err(ToolError::InvalidArgs(
            "argv[0] must not be empty".to_string(),
        ));
    }
    if argv.iter().any(|arg| arg.contains('\0')) {
        return Err(ToolError::InvalidArgs(
            "argv must not contain NUL bytes".to_string(),
        ));
    }
    Ok(())
}

fn display_argv(argv: &[String]) -> String {
    // This is display-only, not a string that is ever parsed or executed.
    argv.iter()
        .map(|argument| format!("{argument:?}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn unapproved(argv: &[String], cwd: &Path) -> ToolError {
    ToolError::PolicyDenied(format!(
        "command {} in '{}' has not been approved",
        display_argv(argv),
        cwd.display()
    ))
}

fn duration_millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

/// JSON arguments for [`command_exec_tool`].
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CommandExecArgs {
    /// Program in element zero followed by literal arguments.
    pub argv: Vec<String>,
    /// Required working directory, constrained by [`CommandPolicy`].
    pub cwd: String,
    /// Optional wall-clock limit; must not exceed the policy maximum.
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

/// Stable termination classification returned to the model.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CommandTermination {
    Exited,
    Signaled,
    TimedOut,
}

/// Structured command result. A non-zero exit or signal is a completed tool
/// result, not an executor error, so the model can inspect truthful metadata.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CommandOutput {
    pub argv: Vec<String>,
    pub cwd: String,
    pub termination: CommandTermination,
    pub success: bool,
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
    pub elapsed_ms: u64,
}

#[derive(Debug, Clone)]
struct BoundedBytes {
    bytes: Vec<u8>,
    truncated: bool,
}

async fn capture_bounded<R>(
    mut stream: R,
    max_bytes: usize,
    captured: Arc<Mutex<BoundedBytes>>,
) -> std::io::Result<()>
where
    R: AsyncRead + Unpin,
{
    let mut buffer = [0_u8; 8 * 1024];
    loop {
        let read = stream.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        let mut captured = captured.lock().unwrap();
        let remaining = max_bytes.saturating_sub(captured.bytes.len());
        let retained = read.min(remaining);
        captured.bytes.extend_from_slice(&buffer[..retained]);
        captured.truncated |= retained < read;
    }
    Ok(())
}

fn bounded_text(output: BoundedBytes, max_bytes: usize) -> (String, bool) {
    let mut text = String::from_utf8_lossy(&output.bytes).into_owned();
    let mut truncated = output.truncated;
    if text.len() > max_bytes {
        let mut end = max_bytes;
        while end > 0 && !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        truncated = true;
    }
    (text, truncated)
}

#[derive(Debug)]
enum SupervisedTermination {
    Natural(ExitStatus),
    TimedOut(ExitStatus),
}

#[derive(Debug)]
struct SupervisedOutput {
    termination: SupervisedTermination,
    stdout: BoundedBytes,
    stderr: BoundedBytes,
}

/// Owns a child while it is supervised. A dedicated standard thread is
/// started before the first cancellation point and waits idle for an ownership
/// handoff. If the async execution future is dropped, the lease synchronously
/// kills the process group and transfers the child to that thread for reaping,
/// so cleanup does not depend on the Tokio runtime remaining alive.
struct ChildReaperLease {
    child: Option<tokio::process::Child>,
    reaper_sender: Option<std::sync::mpsc::Sender<tokio::process::Child>>,
    reaper_thread: Option<std::thread::JoinHandle<()>>,
}

impl ChildReaperLease {
    fn new(child: tokio::process::Child) -> std::io::Result<Self> {
        let (reaper_sender, reaper_receiver) = std::sync::mpsc::channel();
        let reaper_thread = match std::thread::Builder::new()
            .name("kitsu-command-reaper".to_string())
            .spawn(move || {
                if let Ok(child) = reaper_receiver.recv() {
                    reap_child_without_runtime(child);
                }
            }) {
            Ok(reaper_thread) => reaper_thread,
            Err(error) => {
                // Thread creation failed, so fail the command before its first
                // await and synchronously clean up the already-spawned child.
                reap_child_without_runtime(child);
                return Err(error);
            }
        };
        Ok(Self {
            child: Some(child),
            reaper_sender: Some(reaper_sender),
            reaper_thread: Some(reaper_thread),
        })
    }
}

impl std::ops::Deref for ChildReaperLease {
    type Target = tokio::process::Child;

    fn deref(&self) -> &Self::Target {
        self.child.as_ref().expect("child lease is always armed")
    }
}

impl std::ops::DerefMut for ChildReaperLease {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.child.as_mut().expect("child lease is always armed")
    }
}

impl Drop for ChildReaperLease {
    fn drop(&mut self) {
        let Some(mut child) = self.child.take() else {
            return;
        };
        if child.id().is_some() {
            // Kill synchronously before handing ownership away so caller
            // cancellation cannot return while the process group is still
            // intentionally running.
            kill_process_group(child.id());
            let _ = child.start_kill();
            let sender = self
                .reaper_sender
                .take()
                .expect("live child lease has a reaper sender");
            if let Err(error) = sender.send(child) {
                // The prestarted receiver has no fallible work before `recv`,
                // but retain a fail-safe if the thread terminated unexpectedly.
                reap_child_without_runtime(error.0);
            }
            // A live child may take arbitrarily long to leave an uninterruptible
            // kernel state. Detach the standard reaper rather than block Drop.
            self.reaper_thread.take();
        } else {
            // Normal wait/timeout paths already reaped the child. Disconnect
            // the idle thread and join it so successful calls leak no threads.
            self.reaper_sender.take();
            if let Some(reaper_thread) = self.reaper_thread.take() {
                let _ = reaper_thread.join();
            }
        }
    }
}

fn reap_child_without_runtime(mut child: tokio::process::Child) {
    kill_process_group(child.id());
    let _ = child.start_kill();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) => std::thread::sleep(Duration::from_millis(5)),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return,
        }
    }
}

async fn kill_and_reap(child: &mut tokio::process::Child) -> std::io::Result<ExitStatus> {
    // The child starts in its own process group on Unix. Kill the group first
    // so compiler/test descendants cannot keep running or retain our output
    // pipes after their direct parent is reaped.
    kill_process_group(child.id());
    // `start_kill` can race the group kill or natural exit. `wait` is
    // authoritative and reaps in all cases.
    let kill_error = child.start_kill().err();
    match child.wait().await {
        Ok(status) => Ok(status),
        Err(wait_error) => Err(std::io::Error::other(match kill_error {
            Some(kill_error) => {
                format!("failed to kill child ({kill_error}) and reap it ({wait_error})")
            }
            None => format!("failed to reap killed child: {wait_error}"),
        })),
    }
}

#[cfg(unix)]
fn kill_process_group(child_id: Option<u32>) {
    let Some(child_id) = child_id.and_then(|id| i32::try_from(id).ok()) else {
        return;
    };
    // The child is spawned with pgid == pid. A negative pid targets exactly
    // that process group, never Kitsu's own group.
    // SAFETY: `kill` does not dereference memory. `child_id` is positive after
    // checked conversion, and negating it targets the dedicated child group.
    let _ = unsafe { libc::kill(-child_id, libc::SIGKILL) };
}

#[cfg(not(unix))]
fn kill_process_group(_child_id: Option<u32>) {}

struct PipeReaderTasks {
    stdout: tokio::task::JoinHandle<std::io::Result<()>>,
    stderr: tokio::task::JoinHandle<std::io::Result<()>>,
}

impl Drop for PipeReaderTasks {
    fn drop(&mut self) {
        // JoinHandle drop detaches by default. Explicit abortion ensures a
        // cancelled command does not leave pipe readers behind on a live
        // runtime if a descendant escaped the process group with a pipe open.
        self.stdout.abort();
        self.stderr.abort();
    }
}

async fn supervise_child(
    child: tokio::process::Child,
    timeout: Duration,
    max_output_bytes: usize,
) -> std::io::Result<SupervisedOutput> {
    let mut child = ChildReaperLease::new(child)?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| std::io::Error::other("child stdout was not captured"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| std::io::Error::other("child stderr was not captured"))?;
    let stdout_capture = Arc::new(Mutex::new(BoundedBytes {
        bytes: Vec::with_capacity(max_output_bytes.min(8 * 1024)),
        truncated: false,
    }));
    let stderr_capture = Arc::new(Mutex::new(BoundedBytes {
        bytes: Vec::with_capacity(max_output_bytes.min(8 * 1024)),
        truncated: false,
    }));
    let mut reader_tasks = PipeReaderTasks {
        stdout: tokio::spawn(capture_bounded(
            stdout,
            max_output_bytes,
            stdout_capture.clone(),
        )),
        stderr: tokio::spawn(capture_bounded(
            stderr,
            max_output_bytes,
            stderr_capture.clone(),
        )),
    };

    let termination = tokio::select! {
        status = child.wait() => status.map(SupervisedTermination::Natural),
        _ = tokio::time::sleep(timeout) => {
            kill_and_reap(&mut child).await.map(SupervisedTermination::TimedOut)
        }
    };

    // A wait failure could leave the child live. Make one more best-effort
    // kill+wait attempt before joining pipe readers.
    if termination.is_err() {
        let _ = kill_and_reap(&mut child).await;
    }
    let readers = tokio::time::timeout(PIPE_DRAIN_GRACE, async {
        let (stdout, stderr) = tokio::join!(&mut reader_tasks.stdout, &mut reader_tasks.stderr);
        stdout.map_err(|error| {
            std::io::Error::other(format!("stdout reader join error: {error}"))
        })??;
        stderr.map_err(|error| {
            std::io::Error::other(format!("stderr reader join error: {error}"))
        })??;
        Ok::<(), std::io::Error>(())
    })
    .await;
    match readers {
        Ok(result) => result?,
        Err(_) => {
            // A descendant escaped or deliberately detached while retaining a
            // pipe. Closing our readers bounds completion; mark both snapshots
            // incomplete because either stream could still have produced data.
            reader_tasks.stdout.abort();
            reader_tasks.stderr.abort();
            let _ = (&mut reader_tasks.stdout).await;
            let _ = (&mut reader_tasks.stderr).await;
            stdout_capture.lock().unwrap().truncated = true;
            stderr_capture.lock().unwrap().truncated = true;
        }
    }
    let stdout = stdout_capture.lock().unwrap().clone();
    let stderr = stderr_capture.lock().unwrap().clone();

    Ok(SupervisedOutput {
        termination: termination?,
        stdout,
        stderr,
    })
}

async fn run_supervised_command(
    argv: &[String],
    cwd: &AuthorizedCwd,
    requested_cwd: &Path,
    policy: &CommandPolicy,
    timeout: Duration,
    max_output_bytes: usize,
) -> Result<SupervisedOutput, ToolError> {
    let mut command = tokio::process::Command::new(&argv[0]);
    command
        .args(&argv[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    {
        command.process_group(0);
        let directory = cwd.directory.try_clone().map_err(command_failed)?;
        // SAFETY: the closure calls only `fchdir`, which is async-signal-safe,
        // using a descriptor owned by the closure and inherited across fork.
        // O_CLOEXEC closes it when the approved program is executed.
        unsafe {
            command.pre_exec(move || {
                if libc::fchdir(directory.as_raw_fd()) == -1 {
                    Err(std::io::Error::last_os_error())
                } else {
                    Ok(())
                }
            });
        }
    }
    #[cfg(not(unix))]
    command.current_dir(&cwd.canonical);
    // Do not let inherited dynamic-loader overrides affect approved programs.
    for variable in [
        "LD_PRELOAD",
        "LD_LIBRARY_PATH",
        "DYLD_INSERT_LIBRARIES",
        "DYLD_LIBRARY_PATH",
    ] {
        command.env_remove(variable);
    }

    // This is deliberately the final pathname operation before spawn. Unix
    // then changes directory by the already-authorized descriptor, closing
    // the remaining pathname check-to-use window. Other platforms fail closed
    // on a detected change but retain their platform's current_dir race.
    cwd.revalidate(policy, requested_cwd)?;
    let child = command.spawn().map_err(command_failed)?;
    supervise_child(child, timeout, max_output_bytes)
        .await
        .map_err(command_failed)
}

fn command_failed(error: std::io::Error) -> ToolError {
    ToolError::ExecError(format!("command failed: {error}"))
}

fn requested_timeout(
    requested_ms: Option<u64>,
    limits: CommandLimits,
) -> Result<Duration, ToolError> {
    let Some(requested_ms) = requested_ms else {
        return Ok(limits.default_timeout);
    };
    if requested_ms == 0 {
        return Err(ToolError::InvalidArgs(
            "timeout_ms must be greater than zero".to_string(),
        ));
    }
    let requested = Duration::from_millis(requested_ms);
    if requested > limits.max_timeout {
        return Err(ToolError::InvalidArgs(format!(
            "timeout_ms {requested_ms} exceeds the command policy maximum of {}",
            duration_millis(limits.max_timeout)
        )));
    }
    Ok(requested)
}

fn exit_signal(status: &ExitStatus) -> Option<i32> {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        status.signal()
    }
    #[cfg(not(unix))]
    {
        let _ = status;
        None
    }
}

/// Authorize and execute one command, returning structured result metadata.
/// Dropping this future after spawn synchronously kills the child process group
/// and hands the child to a runtime-independent thread for reaping.
pub async fn execute_command(
    policy: &CommandPolicy,
    args: CommandExecArgs,
) -> Result<CommandOutput, ToolError> {
    let limits = policy.limits();
    let timeout = requested_timeout(args.timeout_ms, limits)?;
    let cwd = policy
        .authorize(&args.argv, Path::new(&args.cwd), timeout)
        .await?;
    let started = Instant::now();
    let captured = run_supervised_command(
        &args.argv,
        &cwd,
        Path::new(&args.cwd),
        policy,
        timeout,
        limits.max_output_bytes,
    )
    .await?;
    let elapsed_ms = duration_millis(started.elapsed());
    let (stdout, stdout_truncated) = bounded_text(captured.stdout, limits.max_output_bytes);
    let (stderr, stderr_truncated) = bounded_text(captured.stderr, limits.max_output_bytes);

    let (termination, success, exit_code, signal) = match captured.termination {
        SupervisedTermination::Natural(status) => {
            let termination = if status.code().is_some() {
                CommandTermination::Exited
            } else {
                CommandTermination::Signaled
            };
            (
                termination,
                status.success(),
                status.code(),
                exit_signal(&status),
            )
        }
        SupervisedTermination::TimedOut(status) => (
            CommandTermination::TimedOut,
            false,
            status.code(),
            exit_signal(&status),
        ),
    };

    Ok(CommandOutput {
        argv: args.argv,
        cwd: cwd.canonical.display().to_string(),
        termination,
        success,
        exit_code,
        signal,
        stdout,
        stderr,
        stdout_truncated,
        stderr_truncated,
        elapsed_ms,
    })
}

/// Build the advertised `command_exec` tool bound to an explicit command
/// authority. Registration is a separate opt-in step.
pub fn command_exec_tool(policy: Arc<CommandPolicy>) -> (ToolDescriptor, HandlerFn) {
    let limits = policy.limits();
    let descriptor = ToolDescriptor {
        name: "command_exec".to_string(),
        description: "Run one explicitly approved developer command as a literal argv array. \
                      Kitsu invokes the program directly without an intermediary shell. cwd is \
                      required and must be inside an approved command root. Returns bounded \
                      stdout/stderr and structured exit, signal, or timeout metadata."
            .to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "argv": {
                    "type": "array",
                    "items": {"type": "string"},
                    "minItems": 1,
                    "description": "Program followed by literal arguments; never a shell command string"
                },
                "cwd": {
                    "type": "string",
                    "description": "Explicit working directory inside an approved command root"
                },
                "timeout_ms": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": duration_millis(limits.max_timeout),
                    "description": format!(
                        "Optional timeout in milliseconds (default {}, maximum {})",
                        duration_millis(limits.default_timeout),
                        duration_millis(limits.max_timeout)
                    )
                }
            },
            "required": ["argv", "cwd"],
            "additionalProperties": false
        }),
    };
    let handler: HandlerFn = Box::new(move |raw_args: String| {
        let policy = policy.clone();
        Box::pin(async move {
            let args: CommandExecArgs = serde_json::from_str(&raw_args)
                .map_err(|error| ToolError::InvalidArgs(error.to_string()))?;
            let output = execute_command(&policy, args).await?;
            serde_json::to_string(&output)
                .map_err(|error| ToolError::ExecError(format!("serialize command result: {error}")))
        })
    });
    (descriptor, handler)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn root(dir: &tempfile::TempDir) -> String {
        dir.path().to_string_lossy().into_owned()
    }

    fn args(argv: &[&str], cwd: &Path) -> CommandExecArgs {
        CommandExecArgs {
            argv: argv.iter().map(|arg| (*arg).to_string()).collect(),
            cwd: cwd.to_string_lossy().into_owned(),
            timeout_ms: None,
        }
    }

    struct FixedPrompter {
        decision: CommandDecision,
        prompts: Arc<AtomicUsize>,
    }

    impl CommandPrompter for FixedPrompter {
        fn ask(&self, _request: &CommandApprovalRequest) -> CommandDecision {
            self.prompts.fetch_add(1, Ordering::Relaxed);
            self.decision
        }
    }

    #[cfg(unix)]
    enum CwdReplacement {
        Directory,
        Symlink(PathBuf),
    }

    #[cfg(unix)]
    struct SwappingPrompter {
        cwd: PathBuf,
        displaced: PathBuf,
        replacement: CwdReplacement,
    }

    #[cfg(unix)]
    impl CommandPrompter for SwappingPrompter {
        fn ask(&self, _request: &CommandApprovalRequest) -> CommandDecision {
            std::fs::rename(&self.cwd, &self.displaced).unwrap();
            match &self.replacement {
                CwdReplacement::Directory => std::fs::create_dir(&self.cwd).unwrap(),
                CwdReplacement::Symlink(target) => {
                    std::os::unix::fs::symlink(target, &self.cwd).unwrap();
                }
            }
            CommandDecision::AllowOnce
        }
    }

    #[tokio::test]
    async fn command_root_does_not_itself_grant_execution() {
        let dir = tempfile::tempdir().unwrap();
        let policy = CommandPolicy::new(&[root(&dir)], CommandMode::Deny).unwrap();
        let invocation = args(&["definitely-not-run"], dir.path());

        assert!(matches!(
            execute_command(&policy, invocation).await,
            Err(ToolError::PolicyDenied(message)) if message.contains("has not been approved")
        ));
    }

    #[tokio::test]
    async fn cwd_must_be_under_an_explicit_command_root_before_prompting() {
        let allowed = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let prompts = Arc::new(AtomicUsize::new(0));
        let policy = CommandPolicy::new(&[root(&allowed)], CommandMode::Ask)
            .unwrap()
            .with_prompter(Arc::new(FixedPrompter {
                decision: CommandDecision::AllowOnce,
                prompts: prompts.clone(),
            }));

        assert!(matches!(
            execute_command(&policy, args(&["definitely-not-run"], outside.path())).await,
            Err(ToolError::PolicyDenied(message)) if message.contains("outside the approved command roots")
        ));
        assert_eq!(prompts.load(Ordering::Relaxed), 0);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn prompt_time_symlink_swap_outside_roots_is_rejected() {
        let allowed = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let cwd = allowed.path().join("cwd");
        let displaced = allowed.path().join("cwd-before-prompt");
        std::fs::create_dir(&cwd).unwrap();
        let policy = CommandPolicy::new(&[root(&allowed)], CommandMode::Ask)
            .unwrap()
            .with_prompter(Arc::new(SwappingPrompter {
                cwd: cwd.clone(),
                displaced: displaced.clone(),
                replacement: CwdReplacement::Symlink(outside.path().to_path_buf()),
            }));

        let result = execute_command(
            &policy,
            args(&["/bin/sh", "-c", "printf owned > cwd-owned"], &cwd),
        )
        .await;

        assert!(matches!(
            result,
            Err(ToolError::PolicyDenied(message))
                if message.contains("changed after authorization")
        ));
        assert!(!outside.path().join("cwd-owned").exists());
        assert!(!displaced.join("cwd-owned").exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn prompt_time_same_path_directory_replacement_is_rejected() {
        let allowed = tempfile::tempdir().unwrap();
        let cwd = allowed.path().join("cwd");
        let displaced = allowed.path().join("cwd-before-prompt");
        std::fs::create_dir(&cwd).unwrap();
        let policy = CommandPolicy::new(&[root(&allowed)], CommandMode::Ask)
            .unwrap()
            .with_prompter(Arc::new(SwappingPrompter {
                cwd: cwd.clone(),
                displaced: displaced.clone(),
                replacement: CwdReplacement::Directory,
            }));

        let result = execute_command(
            &policy,
            args(&["/bin/sh", "-c", "printf owned > cwd-owned"], &cwd),
        )
        .await;

        assert!(matches!(
            result,
            Err(ToolError::PolicyDenied(message))
                if message.contains("changed after authorization")
        ));
        assert!(!cwd.join("cwd-owned").exists());
        assert!(!displaced.join("cwd-owned").exists());
    }

    #[tokio::test]
    async fn session_approval_is_exact_for_argv_and_canonical_cwd() {
        let dir = tempfile::tempdir().unwrap();
        let child = dir.path().join("child");
        std::fs::create_dir(&child).unwrap();
        let prompts = Arc::new(AtomicUsize::new(0));
        let policy = CommandPolicy::new(&[root(&dir)], CommandMode::Ask)
            .unwrap()
            .with_prompter(Arc::new(FixedPrompter {
                decision: CommandDecision::AllowForSession,
                prompts: prompts.clone(),
            }));

        let first = args(&["definitely-missing", "one"], dir.path());
        let _ = execute_command(&policy, first.clone()).await;
        let _ = execute_command(&policy, first).await;
        let _ = execute_command(&policy, args(&["definitely-missing", "two"], dir.path())).await;
        let _ = execute_command(&policy, args(&["definitely-missing", "one"], &child)).await;

        assert_eq!(prompts.load(Ordering::Relaxed), 3);
    }

    #[tokio::test]
    async fn allow_once_does_not_create_a_session_grant() {
        let dir = tempfile::tempdir().unwrap();
        let prompts = Arc::new(AtomicUsize::new(0));
        let policy = CommandPolicy::new(&[root(&dir)], CommandMode::Ask)
            .unwrap()
            .with_prompter(Arc::new(FixedPrompter {
                decision: CommandDecision::AllowOnce,
                prompts: prompts.clone(),
            }));
        let invocation = args(&["definitely-missing"], dir.path());

        let _ = execute_command(&policy, invocation.clone()).await;
        let _ = execute_command(&policy, invocation).await;

        assert_eq!(prompts.load(Ordering::Relaxed), 2);
    }

    #[tokio::test]
    async fn timeout_is_policy_bounded_before_approval() {
        let dir = tempfile::tempdir().unwrap();
        let prompts = Arc::new(AtomicUsize::new(0));
        let limits =
            CommandLimits::new(Duration::from_millis(10), Duration::from_millis(20), 64).unwrap();
        let policy = CommandPolicy::new(&[root(&dir)], CommandMode::Ask)
            .unwrap()
            .with_limits(limits)
            .with_prompter(Arc::new(FixedPrompter {
                decision: CommandDecision::AllowOnce,
                prompts: prompts.clone(),
            }));
        let mut invocation = args(&["definitely-not-run"], dir.path());
        invocation.timeout_ms = Some(21);

        assert!(matches!(
            execute_command(&policy, invocation).await,
            Err(ToolError::InvalidArgs(message)) if message.contains("exceeds")
        ));
        assert_eq!(prompts.load(Ordering::Relaxed), 0);
    }

    #[cfg(unix)]
    fn granted_policy(
        dir: &tempfile::TempDir,
        argv: &[&str],
        limits: CommandLimits,
    ) -> CommandPolicy {
        let policy = CommandPolicy::new(&[root(dir)], CommandMode::Deny)
            .unwrap()
            .with_limits(limits);
        let argv = argv
            .iter()
            .map(|arg| (*arg).to_string())
            .collect::<Vec<_>>();
        policy.grant_command(&argv, dir.path()).unwrap();
        policy
    }

    #[cfg(unix)]
    fn process_exists(pid: i32) -> bool {
        let result = unsafe { libc::kill(pid, 0) };
        result == 0 || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
    }

    #[cfg(unix)]
    async fn wait_until_reaped(pid: i32) {
        for _ in 0..100 {
            if !process_exists(pid) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("child process {pid} was not killed and reaped");
    }

    #[cfg(unix)]
    fn wait_until_reaped_without_runtime(pid: i32) {
        for _ in 0..200 {
            if !process_exists(pid) {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("child process {pid} was not reaped after runtime shutdown");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn arguments_are_literal_and_never_shell_parsed() {
        let dir = tempfile::tempdir().unwrap();
        let argv = ["/usr/bin/printf", "%s", "one; echo injected | cat"];
        let policy = granted_policy(&dir, &argv, CommandLimits::default());

        let output = execute_command(&policy, args(&argv, dir.path()))
            .await
            .unwrap();

        assert_eq!(output.stdout, "one; echo injected | cat");
        assert_eq!(output.exit_code, Some(0));
        assert_eq!(output.termination, CommandTermination::Exited);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn nonzero_exit_and_stream_bounds_are_truthful() {
        let dir = tempfile::tempdir().unwrap();
        let argv = [
            "/bin/sh",
            "-c",
            "printf 123456789; printf abcdefghi >&2; exit 7",
        ];
        let limits = CommandLimits::new(Duration::from_secs(1), Duration::from_secs(1), 4).unwrap();
        let policy = granted_policy(&dir, &argv, limits);

        let output = execute_command(&policy, args(&argv, dir.path()))
            .await
            .unwrap();

        assert_eq!(output.termination, CommandTermination::Exited);
        assert!(!output.success);
        assert_eq!(output.exit_code, Some(7));
        assert_eq!(output.signal, None);
        assert_eq!(output.stdout, "1234");
        assert_eq!(output.stderr, "abcd");
        assert!(output.stdout_truncated);
        assert!(output.stderr_truncated);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn signal_metadata_is_truthful() {
        let dir = tempfile::tempdir().unwrap();
        let argv = ["/bin/sh", "-c", "kill -TERM $$"];
        let policy = granted_policy(&dir, &argv, CommandLimits::default());

        let output = execute_command(&policy, args(&argv, dir.path()))
            .await
            .unwrap();

        assert_eq!(output.termination, CommandTermination::Signaled);
        assert!(!output.success);
        assert_eq!(output.exit_code, None);
        assert_eq!(output.signal, Some(libc::SIGTERM));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn timeout_kills_and_reaps_child_and_returns_metadata() {
        let dir = tempfile::tempdir().unwrap();
        // Do not `exec` sleep: this verifies the timeout terminates descendants
        // that would otherwise keep the captured pipes open after `sh` dies.
        let argv = ["/bin/sh", "-c", "echo $$; sleep 30"];
        let limits =
            CommandLimits::new(Duration::from_millis(50), Duration::from_millis(50), 64).unwrap();
        let policy = granted_policy(&dir, &argv, limits);

        let output = execute_command(&policy, args(&argv, dir.path()))
            .await
            .unwrap();
        let pid = output.stdout.trim().parse::<i32>().unwrap();

        assert_eq!(output.termination, CommandTermination::TimedOut);
        assert!(!output.success);
        assert_eq!(output.exit_code, None);
        assert_eq!(output.signal, Some(libc::SIGKILL));
        wait_until_reaped(pid).await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn dropping_caller_future_kills_and_reaps_child() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("pid");
        let pid_arg = pid_file.to_string_lossy().into_owned();
        let argv = vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            "echo $$ > \"$1\"; exec sleep 30".to_string(),
            "sh".to_string(),
            pid_arg,
        ];
        let policy = Arc::new(
            CommandPolicy::new(&[root(&dir)], CommandMode::Deny)
                .unwrap()
                .with_limits(
                    CommandLimits::new(Duration::from_secs(30), Duration::from_secs(30), 64)
                        .unwrap(),
                ),
        );
        policy.grant_command(&argv, dir.path()).unwrap();
        let invocation = CommandExecArgs {
            argv,
            cwd: root(&dir),
            timeout_ms: None,
        };
        let task_policy = policy.clone();
        let task = tokio::spawn(async move { execute_command(&task_policy, invocation).await });

        for _ in 0..100 {
            if pid_file.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let pid = std::fs::read_to_string(&pid_file)
            .expect("child should publish its pid")
            .trim()
            .parse::<i32>()
            .unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());

        wait_until_reaped(pid).await;
    }

    #[cfg(unix)]
    #[test]
    fn dropping_runtime_immediately_after_abort_still_reaps_child() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("pid");
        let pid_arg = pid_file.to_string_lossy().into_owned();
        let argv = vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            "echo $$ > \"$1\"; exec sleep 30".to_string(),
            "sh".to_string(),
            pid_arg,
        ];
        let policy = Arc::new(
            CommandPolicy::new(&[root(&dir)], CommandMode::Deny)
                .unwrap()
                .with_limits(
                    CommandLimits::new(Duration::from_secs(30), Duration::from_secs(30), 64)
                        .unwrap(),
                ),
        );
        policy.grant_command(&argv, dir.path()).unwrap();
        let invocation = CommandExecArgs {
            argv,
            cwd: root(&dir),
            timeout_ms: None,
        };
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        let pid = runtime.block_on(async {
            let task_policy = policy.clone();
            let task = tokio::spawn(async move { execute_command(&task_policy, invocation).await });
            for _ in 0..100 {
                if pid_file.exists() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            let pid = std::fs::read_to_string(&pid_file)
                .expect("child should publish its pid")
                .trim()
                .parse::<i32>()
                .unwrap();
            task.abort();
            pid
        });

        // Do not poll the aborted command task or its cleanup again. Runtime
        // teardown must still transfer the child to the standard reaper.
        drop(runtime);
        wait_until_reaped_without_runtime(pid);
    }

    #[test]
    fn tool_factory_advertises_required_argv_and_cwd() {
        let dir = tempfile::tempdir().unwrap();
        let policy = Arc::new(CommandPolicy::new(&[root(&dir)], CommandMode::Deny).unwrap());
        let (descriptor, _handler) = command_exec_tool(policy);

        assert_eq!(descriptor.name, "command_exec");
        assert_eq!(
            descriptor.parameters["required"],
            serde_json::json!(["argv", "cwd"])
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn registered_handler_returns_structured_json() {
        let dir = tempfile::tempdir().unwrap();
        let policy = Arc::new(CommandPolicy::new(&[root(&dir)], CommandMode::Deny).unwrap());
        let argv = vec![
            "/usr/bin/printf".to_string(),
            "%s".to_string(),
            "ok".to_string(),
        ];
        policy.grant_command(&argv, dir.path()).unwrap();
        let (_descriptor, handler) = command_exec_tool(policy);

        let raw = serde_json::json!({"argv": argv, "cwd": root(&dir)}).to_string();
        let output: serde_json::Value = serde_json::from_str(&handler(raw).await.unwrap()).unwrap();

        assert_eq!(output["termination"], "exited");
        assert_eq!(output["exit_code"], 0);
        assert_eq!(output["stdout"], "ok");
    }
}
