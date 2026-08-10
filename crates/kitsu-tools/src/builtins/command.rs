//! Guarded developer-command execution.
//!
//! Command execution is intentionally a separate authority from filesystem
//! [`crate::permissions::Permissions`]. A command root only constrains the
//! process working directory; it does not sandbox the child process or grant
//! any command. An approved child retains Kitsu's ambient operating-system and
//! environment authority. Every exact `(argv, canonical cwd)` invocation must
//! be pre-granted or approved through a [`CommandPrompter`]. A session grant
//! also records the executable path and filesystem identity observed when it
//! was approved; a changed or retargeted executable invalidates that grant.
//! There is deliberately no auto-allow mode.
//!
//! Input is an argv array, not a command-line string. Kitsu invokes the
//! requested program directly without an intermediary shell or argument
//! expansion. The child has no stdin, stdout and stderr are drained into
//! independently bounded buffers, and both timeout and caller-future
//! cancellation kill and reap the child.

use std::collections::VecDeque;
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

/// How Kitsu resolved `argv[0]` while authorizing an approved command.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CommandExecutableResolution {
    /// A bare program name is looked up through Kitsu's ambient `PATH`.
    AmbientPath,
    /// An absolute or cwd-relative pathname is invoked directly.
    InvocationPath,
}

/// Truthful executable-identity information attached to approvals and results.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CommandExecutableIdentity {
    /// The literal, mutable invocation string from `argv[0]`.
    pub invocation: String,
    pub resolution: CommandExecutableResolution,
    /// Best-effort canonical path observed while authorizing.
    pub observed_path: Option<String>,
    /// False because execution is not based on an immutable executable handle.
    /// Session grants still bind to and revalidate the observed file identity.
    pub pinned: bool,
}

/// Filesystem identity bound to an exact session command grant.
///
/// Unix records include device/inode identity and metadata that changes on
/// replacement or in-place modification. Other platforms retain the canonical
/// path, length, modification time, and readonly state as a best-effort
/// fingerprint.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CommandExecutableFingerprint {
    pub canonical_path: PathBuf,
    pub length: u64,
    pub readonly: bool,
    pub modified_unix_seconds: Option<i64>,
    pub modified_subsec_nanos: Option<u32>,
    pub unix_device: Option<u64>,
    pub unix_inode: Option<u64>,
    pub unix_mode: Option<u32>,
    pub unix_uid: Option<u32>,
    pub unix_gid: Option<u32>,
    pub unix_changed_seconds: Option<i64>,
    pub unix_changed_nanos: Option<i64>,
}

/// Executable observation stored with a session grant.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CommandGrantedExecutable {
    pub identity: CommandExecutableIdentity,
    /// `None` records that resolution did not find an executable. If a matching
    /// executable later appears, the changed observation invalidates the grant;
    /// unresolved observations are never executed.
    pub fingerprint: Option<CommandExecutableFingerprint>,
}

/// Ambient operating-system authority retained by an approved command.
///
/// Command roots constrain only the initial cwd. They are not a filesystem,
/// network, process, credential, or environment sandbox.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct CommandAmbientAuthority {
    pub inherits_environment: bool,
    pub filesystem_confined_to_command_roots: bool,
    pub network_confined: bool,
    pub executable_identity_pinned: bool,
}

impl CommandAmbientAuthority {
    pub const CURRENT: Self = Self {
        inherits_environment: true,
        filesystem_confined_to_command_roots: false,
        network_confined: false,
        executable_identity_pinned: false,
    };

    /// Concise wording suitable for an approval UI.
    pub const fn notice(self) -> &'static str {
        "cwd roots are not a sandbox: the command inherits Kitsu's ambient filesystem, network, process, and environment authority; session approval binds and revalidates the observed executable identity but does not use an immutable executable handle"
    }
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
    /// Executable observation that this approval will bind for the session.
    pub executable: CommandExecutableIdentity,
    /// Authority the process will retain if approved.
    pub ambient_authority: CommandAmbientAuthority,
}

impl CommandApprovalRequest {
    /// Human-readable, control-character-escaped argv. This is quoted for a
    /// prompt and is never parsed back into the argv used for execution.
    pub fn display_argv(&self) -> String {
        display_argv(&self.argv)
    }

    /// Security wording an approval frontend should show before accepting.
    pub const fn authority_notice(&self) -> &'static str {
        self.ambient_authority.notice()
    }
}

/// UI boundary for command approval. The CLI can implement this independently
/// of its filesystem permission prompt.
pub trait CommandPrompter: Send + Sync {
    fn ask(&self, request: &CommandApprovalRequest) -> CommandDecision;
}

/// Host-selected request for a full-output artifact.
///
/// This value is constructed only after authorization. Tool arguments contain
/// no artifact path or identifier, so a model cannot redirect artifact writes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandArtifactRequest {
    pub argv: Vec<String>,
    pub cwd: String,
    pub timeout_ms: u64,
    pub executable: CommandExecutableIdentity,
}

/// Which captured command pipe produced an artifact chunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandOutputStream {
    Stdout,
    Stderr,
}

/// Terminal metadata supplied when a full-output artifact is finalized.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandArtifactCompletion {
    pub termination: CommandTermination,
    pub success: bool,
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub elapsed_ms: u64,
    pub stdout_bytes: u64,
    pub stderr_bytes: u64,
    pub stdout_capture_complete: bool,
    pub stderr_capture_complete: bool,
}

/// Opaque, host-controlled reference to a completed full-output artifact.
///
/// The sink decides whether this is a session ID, protected path, or another
/// handle. Kitsu never interprets it as a path and never passes it to the child.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CommandOutputArtifact {
    pub reference: String,
}

/// One host-owned full-output artifact being streamed.
///
/// Implementations must protect their destination from untrusted replacement,
/// use restrictive access controls where output may contain secrets, and clean
/// up an unfinished artifact when [`Self::abort`] is called or when dropped
/// without [`Self::complete`]. Methods can be called concurrently for stdout
/// and stderr, but preserve order within each individual stream.
pub trait CommandOutputArtifactWriter: Send + Sync {
    fn write_chunk(&self, stream: CommandOutputStream, chunk: &[u8]) -> std::io::Result<()>;

    fn complete(
        &self,
        completion: &CommandArtifactCompletion,
    ) -> std::io::Result<CommandOutputArtifact>;

    /// Remove or quarantine an incomplete artifact after capture/finalization
    /// failure. Implementations that retain partial output should override this.
    fn abort(&self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Optional host hook for protected full command output.
///
/// The command tool remains safe-by-default: no artifact is created unless the
/// embedding frontend explicitly installs a sink on [`CommandPolicy`]. A sink
/// failure never stops pipe draining or strands a child process.
pub trait CommandOutputArtifactSink: Send + Sync {
    fn begin(
        &self,
        request: &CommandArtifactRequest,
    ) -> Result<Arc<dyn CommandOutputArtifactWriter>, ToolError>;
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

/// One exact session grant. The cwd is canonical, argv elements are the literal
/// invocation strings approved by the user, and `executable` records the
/// observed executable identity that must continue to match.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CommandGrantInfo {
    pub argv: Vec<String>,
    pub cwd: PathBuf,
    /// Optional only for backward-compatible deserialization. Grants created by
    /// this version always contain an observation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executable: Option<CommandGrantedExecutable>,
}

impl CommandGrantInfo {
    pub fn display_argv(&self) -> String {
        display_argv(&self.argv)
    }
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

/// Cwd and executable identity captured as one authorization decision.
#[derive(Debug)]
struct AuthorizedCommand {
    cwd: AuthorizedCwd,
    executable: CommandGrantedExecutable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GrantMatch {
    Missing,
    Current,
    Invalidated,
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
    grants: RwLock<Vec<CommandGrantInfo>>,
    mode: CommandMode,
    prompter: Option<Arc<dyn CommandPrompter>>,
    limits: CommandLimits,
    artifact_sink: Option<Arc<dyn CommandOutputArtifactSink>>,
    #[cfg(test)]
    executable_search_path: RwLock<Option<std::ffi::OsString>>,
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
            artifact_sink: None,
            #[cfg(test)]
            executable_search_path: RwLock::new(None),
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

    /// Stream complete stdout/stderr to a host-owned protected artifact.
    /// Disabled by default; the model cannot request or choose the destination.
    pub fn with_output_artifact_sink(mut self, sink: Arc<dyn CommandOutputArtifactSink>) -> Self {
        self.artifact_sink = Some(sink);
        self
    }

    pub fn mode(&self) -> CommandMode {
        self.mode
    }

    pub fn limits(&self) -> CommandLimits {
        self.limits
    }

    /// Describe authority retained by every command under the current policy.
    pub const fn ambient_authority(&self) -> CommandAmbientAuthority {
        CommandAmbientAuthority::CURRENT
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
        let executable = self.observe_executable(argv, &cwd);
        self.insert_grant(argv.to_vec(), cwd.clone(), executable);
        Ok(cwd)
    }

    /// Snapshot every exact session grant in deterministic cwd/argv order.
    pub fn session_grants(&self) -> Vec<CommandGrantInfo> {
        let mut grants = self.grants.read().unwrap().clone();
        grants.sort_by(|left, right| {
            left.cwd
                .cmp(&right.cwd)
                .then_with(|| left.argv.cmp(&right.argv))
        });
        grants
    }

    /// Revoke exactly one previously inspected grant. This does not
    /// canonicalize again, so a grant remains revocable if its cwd was renamed
    /// or deleted after approval. A non-matching value changes nothing.
    pub fn revoke_exact_grant(&self, grant: &CommandGrantInfo) -> bool {
        let mut grants = self.grants.write().unwrap();
        let Some(index) = grants.iter().position(|candidate| candidate == grant) else {
            return false;
        };
        grants.remove(index);
        true
    }

    /// Resolve a currently accessible cwd, then revoke that exact argv/cwd
    /// pair. Use [`Self::revoke_exact_grant`] for an inspected stale path.
    pub fn revoke_command(
        &self,
        argv: &[String],
        cwd: &Path,
    ) -> Result<Option<CommandGrantInfo>, ToolError> {
        validate_argv(argv)?;
        let canonical = self.resolve_cwd(cwd)?;
        let mut grants = self.grants.write().unwrap();
        let Some(index) = grants
            .iter()
            .position(|grant| grant.argv == argv && grant.cwd == canonical)
        else {
            return Ok(None);
        };
        Ok(Some(grants.remove(index)))
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
        let executable = self.observe_executable(argv, &cwd);
        self.match_grant(argv, &cwd, &executable) == GrantMatch::Current
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
    ) -> Result<AuthorizedCommand, ToolError> {
        validate_argv(argv)?;
        let cwd = self.resolve_cwd(requested_cwd)?;
        let authorized_cwd = AuthorizedCwd::bind(self, requested_cwd, cwd.clone())?;
        let executable = self.observe_executable(argv, &cwd);
        let grant_match = self.match_grant(argv, &cwd, &executable);
        if grant_match == GrantMatch::Current {
            return Ok(AuthorizedCommand {
                cwd: authorized_cwd,
                executable,
            });
        }

        match self.mode {
            CommandMode::Deny => {
                if grant_match == GrantMatch::Invalidated {
                    Err(executable_changed(argv, &cwd))
                } else {
                    Err(unapproved(argv, &cwd))
                }
            }
            CommandMode::Ask => {
                if !crate::interactive_prompts_allowed() {
                    return Err(ToolError::PolicyDenied(format!(
                        "background command {} in '{}' requires interactive approval; pre-grant the exact argv/cwd before starting background work",
                        display_argv(argv),
                        cwd.display()
                    )));
                }
                let request = CommandApprovalRequest {
                    argv: argv.to_vec(),
                    cwd: cwd.display().to_string(),
                    timeout_ms: duration_millis(timeout),
                    executable: executable.identity.clone(),
                    ambient_authority: self.ambient_authority(),
                };
                match self.prompt(request).await {
                    CommandDecision::AllowOnce => Ok(AuthorizedCommand {
                        cwd: authorized_cwd,
                        executable,
                    }),
                    CommandDecision::AllowForSession => {
                        self.insert_grant(argv.to_vec(), cwd, executable.clone());
                        Ok(AuthorizedCommand {
                            cwd: authorized_cwd,
                            executable,
                        })
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

    fn insert_grant(&self, argv: Vec<String>, cwd: PathBuf, executable: CommandGrantedExecutable) {
        let mut grants = self.grants.write().unwrap();
        if let Some(existing) = grants
            .iter()
            .position(|grant| grant.argv == argv && grant.cwd == cwd)
        {
            grants[existing].executable = Some(executable);
        } else {
            grants.push(CommandGrantInfo {
                argv,
                cwd,
                executable: Some(executable),
            });
        }
    }

    /// Check an exact grant and eagerly discard it when its executable
    /// observation no longer matches. A stale grant must never spring back to
    /// life if a path is later changed back.
    fn match_grant(
        &self,
        argv: &[String],
        cwd: &Path,
        executable: &CommandGrantedExecutable,
    ) -> GrantMatch {
        let mut grants = self.grants.write().unwrap();
        let Some(index) = grants
            .iter()
            .position(|grant| grant.argv == argv && grant.cwd == cwd)
        else {
            return GrantMatch::Missing;
        };
        if grants[index].executable.as_ref() == Some(executable) {
            GrantMatch::Current
        } else {
            grants.remove(index);
            GrantMatch::Invalidated
        }
    }

    fn observe_executable(&self, argv: &[String], cwd: &Path) -> CommandGrantedExecutable {
        #[cfg(test)]
        if let Some(search_path) = self.executable_search_path.read().unwrap().clone() {
            return observe_executable_with_path(argv, cwd, Some(&search_path));
        }
        observe_executable_with_path(argv, cwd, std::env::var_os("PATH").as_deref())
    }

    #[cfg(test)]
    fn set_test_executable_search_path(&self, path: &Path) {
        *self.executable_search_path.write().unwrap() = Some(path.as_os_str().to_os_string());
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

#[cfg(test)]
fn observe_executable(argv: &[String], cwd: &Path) -> CommandGrantedExecutable {
    observe_executable_with_path(argv, cwd, std::env::var_os("PATH").as_deref())
}

fn observe_executable_with_path(
    argv: &[String],
    cwd: &Path,
    search_path: Option<&std::ffi::OsStr>,
) -> CommandGrantedExecutable {
    let invocation = argv.first().cloned().unwrap_or_default();
    let invocation_path = Path::new(&invocation);
    let uses_ambient_path = !invocation_path.is_absolute()
        && invocation_path
            .components()
            .all(|component| matches!(component, std::path::Component::Normal(_)))
        && invocation_path.components().count() == 1;
    let resolution = if uses_ambient_path {
        CommandExecutableResolution::AmbientPath
    } else {
        CommandExecutableResolution::InvocationPath
    };

    let fingerprint = if uses_ambient_path {
        search_path.and_then(|path| {
            std::env::split_paths(path)
                .map(|directory| {
                    if directory.as_os_str().is_empty() {
                        cwd.join(&invocation)
                    } else if directory.is_absolute() {
                        directory.join(&invocation)
                    } else {
                        cwd.join(directory).join(&invocation)
                    }
                })
                .find_map(|candidate| fingerprint_executable(&candidate))
        })
    } else {
        let candidate = if invocation_path.is_absolute() {
            invocation_path.to_path_buf()
        } else {
            cwd.join(invocation_path)
        };
        fingerprint_executable(&candidate)
    };

    CommandGrantedExecutable {
        identity: CommandExecutableIdentity {
            invocation,
            resolution,
            observed_path: fingerprint
                .as_ref()
                .map(|fingerprint| fingerprint.canonical_path.display().to_string()),
            pinned: false,
        },
        fingerprint,
    }
}

fn fingerprint_executable(candidate: &Path) -> Option<CommandExecutableFingerprint> {
    let canonical_path = std::fs::canonicalize(candidate).ok()?;
    let metadata = std::fs::metadata(&canonical_path).ok()?;
    if !metadata.is_file() {
        return None;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o111 == 0 {
            return None;
        }
    }

    #[cfg(unix)]
    let (
        modified_unix_seconds,
        modified_subsec_nanos,
        unix_device,
        unix_inode,
        unix_mode,
        unix_uid,
        unix_gid,
        unix_changed_seconds,
        unix_changed_nanos,
    ) = {
        use std::os::unix::fs::MetadataExt;
        (
            Some(metadata.mtime()),
            u32::try_from(metadata.mtime_nsec()).ok(),
            Some(metadata.dev()),
            Some(metadata.ino()),
            Some(metadata.mode()),
            Some(metadata.uid()),
            Some(metadata.gid()),
            Some(metadata.ctime()),
            Some(metadata.ctime_nsec()),
        )
    };
    #[cfg(not(unix))]
    let (
        modified_unix_seconds,
        modified_subsec_nanos,
        unix_device,
        unix_inode,
        unix_mode,
        unix_uid,
        unix_gid,
        unix_changed_seconds,
        unix_changed_nanos,
    ) = {
        let modified = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok());
        (
            modified.and_then(|duration| i64::try_from(duration.as_secs()).ok()),
            modified.map(|duration| duration.subsec_nanos()),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
    };

    Some(CommandExecutableFingerprint {
        canonical_path,
        length: metadata.len(),
        readonly: metadata.permissions().readonly(),
        modified_unix_seconds,
        modified_subsec_nanos,
        unix_device,
        unix_inode,
        unix_mode,
        unix_uid,
        unix_gid,
        unix_changed_seconds,
        unix_changed_nanos,
    })
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

fn executable_changed(argv: &[String], cwd: &Path) -> ToolError {
    ToolError::PolicyDenied(format!(
        "the executable identity for command {} in '{}' changed after it was granted; approval was revoked",
        display_argv(argv),
        cwd.display()
    ))
}

fn unresolved_executable(argv: &[String], cwd: &Path) -> ToolError {
    ToolError::PolicyDenied(format!(
        "the executable identity for command {} in '{}' could not be resolved; refusing unbound execution",
        display_argv(argv),
        cwd.display()
    ))
}

fn duration_millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

/// JSON arguments for [`command_exec_tool`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
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

impl CommandExecArgs {
    /// Human-readable, control-character-escaped argv for review only.
    pub fn display_argv(&self) -> String {
        display_argv(&self.argv)
    }
}

/// Stable termination classification returned to the model.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CommandTermination {
    Exited,
    Signaled,
    TimedOut,
    /// The caller dropped the in-flight tool before the supervisor completed.
    Cancelled,
}

/// Structured command result. A non-zero exit or signal is a completed tool
/// result, not an executor error, so the model can inspect truthful metadata.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CommandOutput {
    pub argv: Vec<String>,
    pub cwd: String,
    pub executable: CommandExecutableIdentity,
    pub ambient_authority: CommandAmbientAuthority,
    pub termination: CommandTermination,
    pub success: bool,
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    /// Total bytes drained from stdout, including bytes omitted from `stdout`.
    pub stdout_bytes: u64,
    /// Total bytes drained from stderr, including bytes omitted from `stderr`.
    pub stderr_bytes: u64,
    /// Original stdout bytes retained in the bounded head/tail preview.
    pub stdout_retained_bytes: usize,
    /// Original stderr bytes retained in the bounded head/tail preview.
    pub stderr_retained_bytes: usize,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
    /// False when a detached descendant kept a pipe open beyond the drain grace.
    pub stdout_capture_complete: bool,
    /// False when a detached descendant kept a pipe open beyond the drain grace.
    pub stderr_capture_complete: bool,
    pub elapsed_ms: u64,
    /// Host-owned full-output reference, when a frontend installed a sink.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact: Option<CommandOutputArtifact>,
    /// Artifact failure is reported without discarding the completed command.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact_error: Option<String>,
}

#[derive(Debug, Clone)]
struct BoundedBytes {
    head: Vec<u8>,
    tail: VecDeque<u8>,
    head_capacity: usize,
    tail_capacity: usize,
    total_bytes: u64,
    complete: bool,
}

impl BoundedBytes {
    fn new(max_bytes: usize) -> Self {
        let head_capacity = max_bytes / 2;
        let tail_capacity = max_bytes - head_capacity;
        Self {
            head: Vec::with_capacity(head_capacity),
            tail: VecDeque::with_capacity(tail_capacity),
            head_capacity,
            tail_capacity,
            total_bytes: 0,
            complete: true,
        }
    }

    fn retain(&mut self, chunk: &[u8]) {
        self.total_bytes = self
            .total_bytes
            .saturating_add(u64::try_from(chunk.len()).unwrap_or(u64::MAX));

        let head_remaining = self.head_capacity.saturating_sub(self.head.len());
        let head_bytes = chunk.len().min(head_remaining);
        self.head.extend_from_slice(&chunk[..head_bytes]);
        let remainder = &chunk[head_bytes..];
        if self.tail_capacity == 0 || remainder.is_empty() {
            return;
        }
        if remainder.len() >= self.tail_capacity {
            self.tail.clear();
            self.tail
                .extend(&remainder[remainder.len() - self.tail_capacity..]);
            return;
        }
        let overflow = self
            .tail
            .len()
            .saturating_add(remainder.len())
            .saturating_sub(self.tail_capacity);
        self.tail.drain(..overflow);
        self.tail.extend(remainder);
    }

    fn retained_bytes(&self) -> usize {
        self.head.len().saturating_add(self.tail.len())
    }

    fn truncated(&self) -> bool {
        !self.complete || self.total_bytes > self.retained_bytes() as u64
    }
}

struct ArtifactCapture {
    writer: Arc<dyn CommandOutputArtifactWriter>,
    first_error: Mutex<Option<String>>,
}

impl ArtifactCapture {
    fn new(writer: Arc<dyn CommandOutputArtifactWriter>) -> Self {
        Self {
            writer,
            first_error: Mutex::new(None),
        }
    }

    fn write(&self, stream: CommandOutputStream, chunk: &[u8]) {
        if self.first_error.lock().unwrap().is_some() {
            return;
        }
        if let Err(error) = self.writer.write_chunk(stream, chunk) {
            *self.first_error.lock().unwrap() = Some(error.to_string());
        }
    }

    fn complete(
        &self,
        completion: &CommandArtifactCompletion,
    ) -> (Option<CommandOutputArtifact>, Option<String>) {
        if let Some(error) = self.first_error.lock().unwrap().clone() {
            // Do not bless a partial artifact with a completed reference. The
            // writer's abort hook owns cleanup.
            let error = match self.writer.abort() {
                Ok(()) => error,
                Err(cleanup) => format!("{error}; artifact cleanup failed: {cleanup}"),
            };
            return (None, Some(error));
        }
        match self.writer.complete(completion) {
            Ok(artifact) => (Some(artifact), None),
            Err(error) => {
                let error = match self.writer.abort() {
                    Ok(()) => error.to_string(),
                    Err(cleanup) => {
                        format!("{error}; artifact cleanup failed: {cleanup}")
                    }
                };
                (None, Some(error))
            }
        }
    }
}

async fn capture_bounded<R>(
    mut stream: R,
    captured: Arc<Mutex<BoundedBytes>>,
    artifact: Option<Arc<ArtifactCapture>>,
    output_stream: CommandOutputStream,
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
        if let Some(artifact) = &artifact {
            artifact.write(output_stream, &buffer[..read]);
        }
        captured.lock().unwrap().retain(&buffer[..read]);
    }
    Ok(())
}

struct BoundedText {
    text: String,
    total_bytes: u64,
    retained_bytes: usize,
    truncated: bool,
    complete: bool,
}

fn bounded_text(output: BoundedBytes) -> BoundedText {
    let retained_bytes = output.retained_bytes();
    let truncated = output.truncated();
    let mut text = String::new();
    text.push_str(&String::from_utf8_lossy(&output.head));
    if truncated {
        let omitted = output.total_bytes.saturating_sub(retained_bytes as u64);
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        if output.complete {
            text.push_str(&format!("[... {omitted} bytes omitted ...]"));
        } else if omitted == 0 {
            text.push_str("[... capture incomplete ...]");
        } else {
            text.push_str(&format!(
                "[... {omitted} observed bytes omitted; capture incomplete ...]"
            ));
        }
        if !output.tail.is_empty() {
            text.push('\n');
        }
    }
    let tail = output.tail.into_iter().collect::<Vec<_>>();
    text.push_str(&String::from_utf8_lossy(&tail));
    BoundedText {
        text,
        total_bytes: output.total_bytes,
        retained_bytes,
        truncated,
        complete: output.complete,
    }
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
    artifact: Option<Arc<ArtifactCapture>>,
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
    let stdout_capture = Arc::new(Mutex::new(BoundedBytes::new(max_output_bytes)));
    let stderr_capture = Arc::new(Mutex::new(BoundedBytes::new(max_output_bytes)));
    let mut reader_tasks = PipeReaderTasks {
        stdout: tokio::spawn(capture_bounded(
            stdout,
            stdout_capture.clone(),
            artifact.clone(),
            CommandOutputStream::Stdout,
        )),
        stderr: tokio::spawn(capture_bounded(
            stderr,
            stderr_capture.clone(),
            artifact,
            CommandOutputStream::Stderr,
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
            stdout_capture.lock().unwrap().complete = false;
            stderr_capture.lock().unwrap().complete = false;
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
    authorized: &AuthorizedCommand,
    requested_cwd: &Path,
    policy: &CommandPolicy,
    timeout: Duration,
    max_output_bytes: usize,
    artifact: Option<Arc<ArtifactCapture>>,
) -> Result<SupervisedOutput, ToolError> {
    let executable = authorized
        .executable
        .fingerprint
        .as_ref()
        .ok_or_else(|| unresolved_executable(argv, &authorized.cwd.canonical))?;
    let mut command = tokio::process::Command::new(&executable.canonical_path);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.as_std_mut().arg0(&argv[0]);
    }
    command
        .args(&argv[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    {
        command.process_group(0);
        let directory = authorized
            .cwd
            .directory
            .try_clone()
            .map_err(command_failed)?;
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
    command.current_dir(&authorized.cwd.canonical);
    // Do not let inherited dynamic-loader overrides affect approved programs.
    for variable in [
        "LD_PRELOAD",
        "LD_LIBRARY_PATH",
        "DYLD_INSERT_LIBRARIES",
        "DYLD_LIBRARY_PATH",
    ] {
        command.env_remove(variable);
    }

    // These are deliberately the final pathname checks before spawn. Unix then
    // changes directory by the already-authorized descriptor; the executable
    // is launched by the canonical target observed during authorization.
    // Platforms still retain a narrow executable check-to-spawn race because
    // std::process does not execute an already-open immutable handle.
    authorized.cwd.revalidate(policy, requested_cwd)?;
    if policy.observe_executable(argv, &authorized.cwd.canonical) != authorized.executable {
        return Err(executable_changed(argv, &authorized.cwd.canonical));
    }
    let child = command.spawn().map_err(command_failed)?;
    supervise_child(child, timeout, max_output_bytes, artifact)
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
    let authorized = policy
        .authorize(&args.argv, Path::new(&args.cwd), timeout)
        .await?;
    let executable = authorized.executable.identity.clone();
    let artifact_request = CommandArtifactRequest {
        argv: args.argv.clone(),
        cwd: authorized.cwd.canonical.display().to_string(),
        timeout_ms: duration_millis(timeout),
        executable: executable.clone(),
    };
    let (artifact, artifact_begin_error) = match policy.artifact_sink.as_ref() {
        Some(sink) => match sink.begin(&artifact_request) {
            Ok(writer) => (Some(Arc::new(ArtifactCapture::new(writer))), None),
            Err(error) => {
                // Full-output capture is optional observability. A host sink
                // outage must not revoke an otherwise authorized command or
                // interfere with its ordinary bounded pipe draining.
                (None, Some(error.to_string()))
            }
        },
        None => (None, None),
    };
    let started = Instant::now();
    let captured = run_supervised_command(
        &args.argv,
        &authorized,
        Path::new(&args.cwd),
        policy,
        timeout,
        limits.max_output_bytes,
        artifact.clone(),
    )
    .await?;
    let elapsed_ms = duration_millis(started.elapsed());
    let stdout = bounded_text(captured.stdout);
    let stderr = bounded_text(captured.stderr);

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

    let completion = CommandArtifactCompletion {
        termination,
        success,
        exit_code,
        signal,
        elapsed_ms,
        stdout_bytes: stdout.total_bytes,
        stderr_bytes: stderr.total_bytes,
        stdout_capture_complete: stdout.complete,
        stderr_capture_complete: stderr.complete,
    };
    let (output_artifact, artifact_completion_error) = artifact
        .map(|artifact| artifact.complete(&completion))
        .unwrap_or((None, None));
    let artifact_error = artifact_begin_error.or(artifact_completion_error);

    Ok(CommandOutput {
        argv: args.argv,
        cwd: authorized.cwd.canonical.display().to_string(),
        executable,
        ambient_authority: policy.ambient_authority(),
        termination,
        success,
        exit_code,
        signal,
        stdout: stdout.text,
        stderr: stderr.text,
        stdout_bytes: stdout.total_bytes,
        stderr_bytes: stderr.total_bytes,
        stdout_retained_bytes: stdout.retained_bytes,
        stderr_retained_bytes: stderr.retained_bytes,
        stdout_truncated: stdout.truncated,
        stderr_truncated: stderr.truncated,
        stdout_capture_complete: stdout.complete,
        stderr_capture_complete: stderr.complete,
        elapsed_ms,
        artifact: output_artifact,
        artifact_error,
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
                      required and must be inside an approved command root, but that root is not \
                      a filesystem or network sandbox: the process retains Kitsu's ambient OS \
                      authority and environment. Session approval binds the observed executable \
                      filesystem identity and is revoked if it changes. Returns bounded head/tail \
                      stdout/stderr previews, truthful byte \
                      counts, and structured exit, signal, or timeout metadata."
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

    #[cfg(unix)]
    fn write_test_executable(path: &Path, output: &str) {
        use std::os::unix::fs::PermissionsExt;

        std::fs::write(path, format!("#!/bin/sh\nprintf '%s' '{output}'\n")).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }

    struct FixedPrompter {
        decision: CommandDecision,
        prompts: Arc<AtomicUsize>,
    }

    #[derive(Default)]
    struct MemoryArtifact {
        stdout: Mutex<Vec<u8>>,
        stderr: Mutex<Vec<u8>>,
        completion: Mutex<Option<CommandArtifactCompletion>>,
        fail_writes: bool,
    }

    impl CommandOutputArtifactWriter for MemoryArtifact {
        fn write_chunk(&self, stream: CommandOutputStream, chunk: &[u8]) -> std::io::Result<()> {
            if self.fail_writes {
                return Err(std::io::Error::other("artifact unavailable"));
            }
            match stream {
                CommandOutputStream::Stdout => self.stdout.lock().unwrap().extend_from_slice(chunk),
                CommandOutputStream::Stderr => self.stderr.lock().unwrap().extend_from_slice(chunk),
            }
            Ok(())
        }

        fn complete(
            &self,
            completion: &CommandArtifactCompletion,
        ) -> std::io::Result<CommandOutputArtifact> {
            *self.completion.lock().unwrap() = Some(*completion);
            Ok(CommandOutputArtifact {
                reference: "session-artifact:command-1".to_string(),
            })
        }
    }

    struct MemoryArtifactSink {
        writer: Arc<MemoryArtifact>,
        requests: Mutex<Vec<CommandArtifactRequest>>,
    }

    impl CommandOutputArtifactSink for MemoryArtifactSink {
        fn begin(
            &self,
            request: &CommandArtifactRequest,
        ) -> Result<Arc<dyn CommandOutputArtifactWriter>, ToolError> {
            self.requests.lock().unwrap().push(request.clone());
            Ok(self.writer.clone())
        }
    }

    struct FailingArtifactSink;

    impl CommandOutputArtifactSink for FailingArtifactSink {
        fn begin(
            &self,
            _request: &CommandArtifactRequest,
        ) -> Result<Arc<dyn CommandOutputArtifactWriter>, ToolError> {
            Err(ToolError::ExecError(
                "artifact begin unavailable".to_string(),
            ))
        }
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

    #[cfg(unix)]
    #[tokio::test]
    async fn replacing_an_approved_executable_forces_fresh_approval() {
        let dir = tempfile::tempdir().unwrap();
        let executable = dir.path().join("approved-tool");
        let replacement = dir.path().join("replacement-tool");
        write_test_executable(&executable, "first");
        write_test_executable(&replacement, "second");
        let argv = vec![executable.to_string_lossy().into_owned()];
        let prompts = Arc::new(AtomicUsize::new(0));
        let policy = CommandPolicy::new(&[root(&dir)], CommandMode::Ask)
            .unwrap()
            .with_prompter(Arc::new(FixedPrompter {
                decision: CommandDecision::AllowForSession,
                prompts: prompts.clone(),
            }));

        let first = execute_command(
            &policy,
            CommandExecArgs {
                argv: argv.clone(),
                cwd: root(&dir),
                timeout_ms: None,
            },
        )
        .await
        .unwrap();
        let original_grant = policy.session_grants().pop().unwrap();
        std::fs::rename(&replacement, &executable).unwrap();
        let second = execute_command(
            &policy,
            CommandExecArgs {
                argv,
                cwd: root(&dir),
                timeout_ms: None,
            },
        )
        .await
        .unwrap();
        let replacement_grant = policy.session_grants().pop().unwrap();

        assert_eq!(first.stdout, "first");
        assert_eq!(second.stdout, "second");
        assert_eq!(prompts.load(Ordering::Relaxed), 2);
        assert_ne!(original_grant.executable, replacement_grant.executable);
        let original = original_grant.executable.unwrap().fingerprint.unwrap();
        let replacement = replacement_grant.executable.unwrap().fingerprint.unwrap();
        assert_eq!(original.canonical_path, replacement.canonical_path);
        assert_ne!(original.unix_inode, replacement.unix_inode);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn retargeting_path_for_an_approved_name_forces_fresh_approval() {
        let dir = tempfile::tempdir().unwrap();
        let first_bin = dir.path().join("first-bin");
        let second_bin = dir.path().join("second-bin");
        std::fs::create_dir(&first_bin).unwrap();
        std::fs::create_dir(&second_bin).unwrap();
        write_test_executable(&first_bin.join("path-tool"), "first-path");
        write_test_executable(&second_bin.join("path-tool"), "second-path");
        let prompts = Arc::new(AtomicUsize::new(0));
        let policy = CommandPolicy::new(&[root(&dir)], CommandMode::Ask)
            .unwrap()
            .with_prompter(Arc::new(FixedPrompter {
                decision: CommandDecision::AllowForSession,
                prompts: prompts.clone(),
            }));
        policy.set_test_executable_search_path(&first_bin);
        let invocation = args(&["path-tool"], dir.path());

        let first = execute_command(&policy, invocation.clone()).await.unwrap();
        let first_grant = policy.session_grants().pop().unwrap();
        policy.set_test_executable_search_path(&second_bin);
        let second = execute_command(&policy, invocation).await.unwrap();
        let second_grant = policy.session_grants().pop().unwrap();

        assert_eq!(first.stdout, "first-path");
        assert_eq!(second.stdout, "second-path");
        assert_eq!(prompts.load(Ordering::Relaxed), 2);
        assert_ne!(first_grant.executable, second_grant.executable);
        assert_eq!(
            second.executable.observed_path.as_deref(),
            Some(second_bin.join("path-tool").to_string_lossy().as_ref())
        );
    }

    #[tokio::test]
    async fn background_scope_never_calls_the_interactive_command_prompter() {
        let dir = tempfile::tempdir().unwrap();
        let prompts = Arc::new(AtomicUsize::new(0));
        let policy = CommandPolicy::new(&[root(&dir)], CommandMode::Ask)
            .unwrap()
            .with_prompter(Arc::new(FixedPrompter {
                decision: CommandDecision::AllowOnce,
                prompts: prompts.clone(),
            }));

        let result = crate::scope_without_interactive_prompts(execute_command(
            &policy,
            args(&["unapproved-background-command"], dir.path()),
        ))
        .await;

        assert!(matches!(
            result,
            Err(ToolError::PolicyDenied(message))
                if message.contains("background command") && message.contains("pre-grant")
        ));
        assert_eq!(prompts.load(Ordering::Relaxed), 0);
        assert!(policy.session_grants().is_empty());
    }

    #[tokio::test]
    async fn an_approved_unresolved_executable_is_never_run_without_an_identity() {
        let dir = tempfile::tempdir().unwrap();
        let prompts = Arc::new(AtomicUsize::new(0));
        let policy = CommandPolicy::new(&[root(&dir)], CommandMode::Ask)
            .unwrap()
            .with_prompter(Arc::new(FixedPrompter {
                decision: CommandDecision::AllowOnce,
                prompts: prompts.clone(),
            }));

        let result = execute_command(
            &policy,
            args(&["definitely-unresolved-command"], dir.path()),
        )
        .await;

        assert!(matches!(
            result,
            Err(ToolError::PolicyDenied(message)) if message.contains("refusing unbound execution")
        ));
        assert_eq!(prompts.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn exact_command_grants_are_inspectable_sorted_and_individually_revocable() {
        let dir = tempfile::tempdir().unwrap();
        let child = dir.path().join("child");
        std::fs::create_dir(&child).unwrap();
        let policy = CommandPolicy::new(&[root(&dir)], CommandMode::Deny).unwrap();
        let alpha = vec!["tool".to_string(), "alpha".to_string()];
        let zeta = vec!["tool".to_string(), "zeta".to_string()];
        policy.grant_command(&zeta, dir.path()).unwrap();
        policy.grant_command(&alpha, dir.path()).unwrap();
        policy.grant_command(&alpha, dir.path()).unwrap();
        policy.grant_command(&alpha, &child).unwrap();

        let grants = policy.session_grants();
        assert_eq!(grants.len(), 3, "duplicate grants are coalesced");
        assert_eq!(grants[0].cwd, std::fs::canonicalize(dir.path()).unwrap());
        assert_eq!(grants[0].argv, alpha);
        assert_eq!(grants[1].argv, zeta);
        assert_eq!(grants[2].cwd, std::fs::canonicalize(&child).unwrap());
        assert_eq!(grants[2].display_argv(), r#""tool" "alpha""#);

        assert_eq!(
            policy
                .revoke_command(&["tool".to_string(), "missing".to_string()], dir.path())
                .unwrap(),
            None
        );
        assert_eq!(policy.session_grants().len(), 3);
        assert_eq!(
            policy.revoke_command(&alpha, dir.path()).unwrap().as_ref(),
            Some(&grants[0])
        );
        assert!(!policy.is_granted(&alpha, dir.path()));
        assert!(policy.is_granted(&zeta, dir.path()));

        let stale = grants[2].clone();
        std::fs::remove_dir(&child).unwrap();
        assert!(policy.revoke_exact_grant(&stale));
        assert!(!policy.revoke_exact_grant(&stale));
        assert_eq!(policy.session_grants(), vec![grants[1].clone()]);
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
        assert_eq!(output.stdout, "12\n[... 5 bytes omitted ...]\n89");
        assert_eq!(output.stderr, "ab\n[... 5 bytes omitted ...]\nhi");
        assert_eq!(output.stdout_bytes, 9);
        assert_eq!(output.stderr_bytes, 9);
        assert_eq!(output.stdout_retained_bytes, 4);
        assert_eq!(output.stderr_retained_bytes, 4);
        assert!(output.stdout_truncated);
        assert!(output.stderr_truncated);
        assert!(output.stdout_capture_complete);
        assert!(output.stderr_capture_complete);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn optional_artifact_sink_receives_full_output_beyond_preview_cap() {
        let dir = tempfile::tempdir().unwrap();
        let argv = ["/bin/sh", "-c", "printf 123456789; printf abcdefghi >&2"];
        let limits = CommandLimits::new(Duration::from_secs(1), Duration::from_secs(1), 4).unwrap();
        let writer = Arc::new(MemoryArtifact::default());
        let sink = Arc::new(MemoryArtifactSink {
            writer: writer.clone(),
            requests: Mutex::new(Vec::new()),
        });
        let policy = CommandPolicy::new(&[root(&dir)], CommandMode::Deny)
            .unwrap()
            .with_limits(limits)
            .with_output_artifact_sink(sink.clone());
        let argv = argv
            .iter()
            .map(|argument| (*argument).to_string())
            .collect::<Vec<_>>();
        policy.grant_command(&argv, dir.path()).unwrap();

        let output = execute_command(
            &policy,
            CommandExecArgs {
                argv,
                cwd: root(&dir),
                timeout_ms: None,
            },
        )
        .await
        .unwrap();

        assert_eq!(&*writer.stdout.lock().unwrap(), b"123456789");
        assert_eq!(&*writer.stderr.lock().unwrap(), b"abcdefghi");
        assert_eq!(
            output
                .artifact
                .as_ref()
                .map(|artifact| artifact.reference.as_str()),
            Some("session-artifact:command-1")
        );
        assert_eq!(output.artifact_error, None);
        assert_eq!(sink.requests.lock().unwrap().len(), 1);
        let completion = writer.completion.lock().unwrap().unwrap();
        assert_eq!(completion.stdout_bytes, 9);
        assert_eq!(completion.stderr_bytes, 9);
        assert_eq!(completion.termination, CommandTermination::Exited);
        assert!(completion.stdout_capture_complete);
        assert!(completion.stderr_capture_complete);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn artifact_failure_does_not_stop_pipe_draining_or_hide_command_result() {
        let dir = tempfile::tempdir().unwrap();
        let argv = ["/usr/bin/printf", "%s", "complete-output"];
        let writer = Arc::new(MemoryArtifact {
            fail_writes: true,
            ..MemoryArtifact::default()
        });
        let sink = Arc::new(MemoryArtifactSink {
            writer,
            requests: Mutex::new(Vec::new()),
        });
        let policy = CommandPolicy::new(&[root(&dir)], CommandMode::Deny)
            .unwrap()
            .with_output_artifact_sink(sink);
        let argv = argv
            .iter()
            .map(|argument| (*argument).to_string())
            .collect::<Vec<_>>();
        policy.grant_command(&argv, dir.path()).unwrap();

        let output = execute_command(
            &policy,
            CommandExecArgs {
                argv,
                cwd: root(&dir),
                timeout_ms: None,
            },
        )
        .await
        .unwrap();

        assert_eq!(output.stdout, "complete-output");
        assert_eq!(output.exit_code, Some(0));
        assert_eq!(output.artifact, None);
        assert!(output
            .artifact_error
            .as_deref()
            .is_some_and(|error| error.contains("artifact unavailable")));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn artifact_begin_failure_does_not_prevent_the_authorized_command() {
        let dir = tempfile::tempdir().unwrap();
        let argv = ["/usr/bin/printf", "%s", "command-still-ran"];
        let policy = CommandPolicy::new(&[root(&dir)], CommandMode::Deny)
            .unwrap()
            .with_output_artifact_sink(Arc::new(FailingArtifactSink));
        let argv = argv
            .iter()
            .map(|argument| (*argument).to_string())
            .collect::<Vec<_>>();
        policy.grant_command(&argv, dir.path()).unwrap();

        let output = execute_command(
            &policy,
            CommandExecArgs {
                argv,
                cwd: root(&dir),
                timeout_ms: None,
            },
        )
        .await
        .expect("optional artifact startup must not block command execution");

        assert_eq!(output.stdout, "command-still-ran");
        assert_eq!(output.exit_code, Some(0));
        assert!(output.stdout_capture_complete);
        assert_eq!(output.artifact, None);
        assert!(output
            .artifact_error
            .as_deref()
            .is_some_and(|error| error.contains("artifact begin unavailable")));
    }

    #[test]
    fn authority_api_truthfully_labels_mutable_ambient_execution() {
        let dir = tempfile::tempdir().unwrap();
        let policy = CommandPolicy::new(&[root(&dir)], CommandMode::Deny).unwrap();
        let authority = policy.ambient_authority();
        assert!(authority.inherits_environment);
        assert!(!authority.filesystem_confined_to_command_roots);
        assert!(!authority.network_confined);
        assert!(!authority.executable_identity_pinned);
        assert!(authority.notice().contains("not a sandbox"));

        let argv = vec!["definitely-unresolved-program".to_string()];
        let executable = observe_executable(&argv, dir.path());
        assert_eq!(
            executable.identity.resolution,
            CommandExecutableResolution::AmbientPath
        );
        assert_eq!(executable.identity.observed_path, None);
        assert!(!executable.identity.pinned);
        assert_eq!(executable.fingerprint, None);
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
