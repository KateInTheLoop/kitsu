//! Guarded developer-command execution.
//!
//! Command execution is intentionally a separate authority from filesystem
//! [`crate::permissions::Permissions`]. Every exact `(argv, canonical cwd)`
//! invocation must be pre-granted or approved through a [`CommandPrompter`],
//! and that approval is only one gate: production execution additionally
//! requires fail-closed operating-system containment. On Linux, Kitsu launches
//! through bubblewrap with writable approved command roots, a small read-only
//! runtime, isolated network/PID/IPC/UTS namespaces, and a fixed minimal
//! environment. A session grant records the executable path and filesystem
//! identity observed when it was approved; a changed or retargeted executable
//! invalidates that grant. There is deliberately no auto-allow mode.
//!
//! Input is an argv array, not a command-line string. Kitsu invokes the
//! requested program directly without an intermediary shell or argument
//! expansion. The child has no stdin, stdout and stderr are drained into
//! independently bounded buffers, and both timeout and caller-future
//! cancellation kill and reap the child.
//!
//! Unsupported platforms and Linux hosts where the containment backend is
//! absent or unusable fail closed before approval with an inspectable typed
//! status. Windows also cannot yet bind extension/PATH resolution to
//! loader-equivalent executable validation.

use std::collections::VecDeque;
use std::ffi::OsString;
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

use crate::{HandlerFn, ToolError, ToolInterruption};

/// Default wall-clock limit for one command.
pub const DEFAULT_COMMAND_TIMEOUT: Duration = Duration::from_secs(120);
/// Maximum wall-clock limit a tool call may request under the default policy.
pub const DEFAULT_MAX_COMMAND_TIMEOUT: Duration = Duration::from_secs(600);
/// Maximum retained bytes for each of stdout and stderr under the default policy.
pub const DEFAULT_COMMAND_OUTPUT_BYTES: usize = 64 * 1024;
/// Maximum completed serialized bytes for an optional command-output artifact.
/// Pipes continue to be drained after the sink reaches this cap.
pub const DEFAULT_COMMAND_ARTIFACT_BYTES: usize = 16 * 1024 * 1024;
/// Default Linux containment executable. It is fingerprinted during preflight
/// and revalidated before every launch.
pub const DEFAULT_BUBBLEWRAP_PATH: &str = "/usr/bin/bwrap";
/// Fixed environment visible inside a default contained command.
pub const COMMAND_CONTAINMENT_ENVIRONMENT: &[(&str, &str)] = &[
    ("HOME", "/tmp/kitsu-home"),
    ("LANG", "C.UTF-8"),
    ("LC_ALL", "C.UTF-8"),
    ("LOGNAME", "kitsu"),
    (
        "PATH",
        "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
    ),
    ("TERM", "dumb"),
    ("TMPDIR", "/tmp"),
    ("USER", "kitsu"),
];
/// Case-insensitive substring markers that make an environment variable name
/// look like it could hold a secret. A bare `--command-env NAME` pass-through
/// (which silently forwards whatever value kitsu's own process happens to
/// hold under that name) is refused for a matching name; the explicit
/// `--command-env NAME=value` form is always allowed regardless of name,
/// since the caller is typing the literal value onto the command line rather
/// than asking kitsu to forward an ambient one.
pub const SECRET_LIKE_ENV_NAME_MARKERS: &[&str] = &["KEY", "TOKEN", "SECRET", "PASSWORD"];

/// True if `name` contains one of [`SECRET_LIKE_ENV_NAME_MARKERS`] as a
/// case-insensitive substring.
pub fn env_name_looks_secret(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    SECRET_LIKE_ENV_NAME_MARKERS
        .iter()
        .any(|marker| upper.contains(marker))
}

/// Merge the fixed base containment environment with caller-approved
/// pass-through entries. Pass-through entries override a base var of the
/// same name (last one wins) rather than duplicating it, so e.g. an explicit
/// `--command-env HOME=/real/project/.venv`-style override behaves
/// predictably instead of leaving two conflicting `--setenv HOME` values.
fn merged_containment_environment(extra: &[(String, String)]) -> Vec<(String, String)> {
    let mut merged: Vec<(String, String)> = COMMAND_CONTAINMENT_ENVIRONMENT
        .iter()
        .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
        .collect();
    for (name, value) in extra {
        match merged.iter_mut().find(|(existing, _)| existing == name) {
            Some(slot) => slot.1 = value.clone(),
            None => merged.push((name.clone(), value.clone())),
        }
    }
    merged
}
/// Time allowed for captured pipes to reach EOF after the direct child exits.
/// A detached descendant must not make command completion unbounded.
const PIPE_DRAIN_GRACE: Duration = Duration::from_millis(250);

/// How an invocation without an exact session grant is handled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandMode {
    /// Ask the command-specific prompter. A missing prompter fails closed.
    Ask,
    /// Deny invocations that do not already have an exact grant.
    Deny,
    /// DANGER: approve every invocation without prompting. Exists for
    /// explicitly opted-in unattended automation
    /// (`--dangerously-skip-permissions`). Only the *prompt* is skipped —
    /// command-root cwd confinement, argv validation, executable
    /// resolution, containment mounts, and secret-env filtering all still
    /// apply.
    AutoAllow,
}

/// The user's answer to a command-specific approval prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandDecision {
    /// Run this invocation without changing session grants.
    AllowOnce,
    /// Grant this exact argv in this exact canonical cwd for the session.
    AllowForSession,
    /// Run this invocation and stop asking for the rest of the session: the
    /// policy behaves as [`CommandMode::AutoAllow`] until
    /// [`CommandPolicy::set_session_auto`] turns it off.
    AllowAllForSession,
    /// Do not run the invocation.
    Deny,
    /// Authority input closed before a decision; interrupt the current run.
    Interrupt,
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
/// replacement or in-place modification. Supported non-Windows platforms
/// retain the canonical path, length, modification time, and readonly state as
/// a best-effort fingerprint. Windows does not issue fingerprints and command
/// execution therefore fails closed.
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

/// Production command-containment implementation selected by policy.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CommandContainmentBackendKind {
    /// Linux/Android: `bwrap` (Bubblewrap) namespaces + bind mounts.
    Bubblewrap,
    /// macOS: Apple Seatbelt via `/usr/bin/sandbox-exec`.
    Seatbelt,
}

/// The containment backend this build uses, selected by target OS. A binary
/// runs on exactly one OS, so every containment error/status it produces names
/// this backend. On platforms with no reviewed backend (e.g. Windows) this is a
/// harmless placeholder: those paths only ever carry an `UnsupportedPlatform`
/// error whose `Display` reports the reason, not a live backend.
#[cfg(target_os = "macos")]
const DEFAULT_CONTAINMENT_BACKEND: CommandContainmentBackendKind =
    CommandContainmentBackendKind::Seatbelt;
#[cfg(not(target_os = "macos"))]
const DEFAULT_CONTAINMENT_BACKEND: CommandContainmentBackendKind =
    CommandContainmentBackendKind::Bubblewrap;

/// Host-owned containment configuration. Tool arguments cannot modify it.
///
/// Additional runtime roots are mounted read-only. They are intended for
/// language runtimes/toolchains that are not installed below `/usr`; selecting
/// a broad directory can disclose its readable contents to approved commands,
/// so frontends should display every configured path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandContainmentConfig {
    bubblewrap_path: PathBuf,
    readonly_runtime_roots: Vec<PathBuf>,
    /// Opt-in (`--allow-network`): drop the network isolation for granted
    /// commands so ssh/curl/git-over-net can reach the network, and make the
    /// SSH host files (`~/.ssh/known_hosts`, `~/.ssh/config`) readable so a
    /// host key verifies. Off by default — the sandbox denies all network and
    /// every `~/.ssh` path. Private keys (`id_*`) stay denied either way.
    allow_network: bool,
}

impl Default for CommandContainmentConfig {
    fn default() -> Self {
        Self {
            bubblewrap_path: PathBuf::from(DEFAULT_BUBBLEWRAP_PATH),
            readonly_runtime_roots: Vec::new(),
            allow_network: false,
        }
    }
}

impl CommandContainmentConfig {
    pub fn bubblewrap_path(&self) -> &Path {
        &self.bubblewrap_path
    }

    pub fn readonly_runtime_roots(&self) -> &[PathBuf] {
        &self.readonly_runtime_roots
    }

    /// Whether `--allow-network` opened network access + SSH host files.
    pub fn allow_network(&self) -> bool {
        self.allow_network
    }

    /// Opt into network access for granted commands (`--allow-network`). Off by
    /// default; see [`CommandContainmentConfig`].
    pub fn with_network_allowed(mut self, allow: bool) -> Self {
        self.allow_network = allow;
        self
    }

    /// Select a bubblewrap executable. Availability and filesystem identity are
    /// checked by [`CommandPolicy::containment_status`] and again at launch.
    pub fn with_bubblewrap_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.bubblewrap_path = path.into();
        self
    }

    /// Add one explicit read-only runtime directory. `/` is never accepted as
    /// a runtime root; use narrow toolchain directories and never a credential
    /// or whole-home directory.
    pub fn with_readonly_runtime_root(
        mut self,
        path: impl AsRef<Path>,
    ) -> Result<Self, CommandContainmentError> {
        let requested = path.as_ref();
        let canonical = std::fs::canonicalize(requested).map_err(|error| {
            CommandContainmentError::new(
                CommandContainmentUnavailableReason::InvalidConfiguration,
                format!(
                    "read-only runtime root '{}' is not accessible: {error}",
                    requested.display()
                ),
            )
        })?;
        if canonical == Path::new("/") || !canonical.is_dir() {
            return Err(CommandContainmentError::new(
                CommandContainmentUnavailableReason::InvalidConfiguration,
                format!(
                    "read-only runtime root '{}' must be a directory narrower than '/'",
                    requested.display()
                ),
            ));
        }
        insert_minimal_root(&mut self.readonly_runtime_roots, canonical);
        Ok(self)
    }
}

/// Stable reason a mandatory containment backend cannot be used.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CommandContainmentUnavailableReason {
    UnsupportedPlatform,
    BackendMissing,
    BackendNotExecutable,
    BackendProbeFailed,
    BackendChanged,
    RuntimeUnavailable,
    ExecutableOutsideVisibleRoots,
    InvalidConfiguration,
}

/// Typed, actionable containment failure suitable for a doctor/status UI.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CommandContainmentError {
    pub backend: CommandContainmentBackendKind,
    pub reason: CommandContainmentUnavailableReason,
    pub detail: String,
}

impl CommandContainmentError {
    fn new(reason: CommandContainmentUnavailableReason, detail: String) -> Self {
        Self {
            // The build's own backend (one OS, one backend). See
            // DEFAULT_CONTAINMENT_BACKEND.
            backend: DEFAULT_CONTAINMENT_BACKEND,
            reason,
            detail,
        }
    }
}

impl std::fmt::Display for CommandContainmentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Remediation is backend-specific: a missing bwrap is installable, but
        // sandbox-exec ships with macOS so its absence means something else.
        let remediation = match self.backend {
            CommandContainmentBackendKind::Bubblewrap => {
                "install/enable bubblewrap or disable the command tool"
            }
            CommandContainmentBackendKind::Seatbelt => {
                "ensure /usr/bin/sandbox-exec is present (it ships with macOS) or disable the command tool"
            }
        };
        write!(
            f,
            "mandatory command containment ({:?}) is unavailable: {}; {remediation}",
            self.reason, self.detail
        )
    }
}

impl std::error::Error for CommandContainmentError {}

/// Verified containment capabilities and the exact host-controlled exposure.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CommandContainmentReady {
    pub backend: CommandContainmentBackendKind,
    pub backend_path: PathBuf,
    pub writable_command_roots: Vec<PathBuf>,
    pub readonly_runtime_roots: Vec<PathBuf>,
    /// The fixed base set plus any caller-approved `--command-env`
    /// pass-through entries, merged (pass-through wins on a name collision).
    /// Values for secret-shaped names (see [`env_name_looks_secret`]) are
    /// redacted here even though the contained process receives the real
    /// value — this field feeds durable disclosure surfaces (`/permissions`,
    /// JSONL), not the launch itself.
    pub environment: Vec<(String, String)>,
    /// Names only (never values) of the pass-through entries approved for
    /// this policy, in the order they were granted. A subset of the names in
    /// `environment` — present even when a value was redacted above, so a
    /// JSONL consumer can see exactly which variables were opted in without
    /// needing to diff against the fixed base set.
    #[serde(default)]
    pub passthrough_environment: Vec<String>,
    pub network_isolated: bool,
    pub pid_isolated: bool,
    pub ipc_isolated: bool,
    pub uts_isolated: bool,
}

/// Cached policy-level preflight state. `Unavailable` always means execution
/// will be denied before an approval prompt is shown.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum CommandContainmentStatus {
    Ready(CommandContainmentReady),
    Unavailable(CommandContainmentError),
}

impl CommandContainmentStatus {
    pub fn is_ready(&self) -> bool {
        matches!(self, Self::Ready(_))
    }

    pub fn error(&self) -> Option<&CommandContainmentError> {
        match self {
            Self::Ready(_) => None,
            Self::Unavailable(error) => Some(error),
        }
    }
}

/// Operating-system authority retained by a successfully contained command.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct CommandAmbientAuthority {
    /// False: host variables are cleared and replaced with a fixed allowlist.
    pub inherits_environment: bool,
    /// True for mutable host filesystem authority. The inspected containment
    /// status separately lists required read-only runtime mounts.
    pub filesystem_confined_to_command_roots: bool,
    /// True: the contained process receives a separate network namespace.
    pub network_confined: bool,
    pub executable_identity_pinned: bool,
}

impl CommandAmbientAuthority {
    pub const CURRENT: Self = Self {
        inherits_environment: false,
        filesystem_confined_to_command_roots: true,
        network_confined: true,
        executable_identity_pinned: false,
    };

    /// Concise wording suitable for an approval UI. The network clause tells
    /// the truth about `--allow-network`: isolation is stated only when it is
    /// actually in force, so an approving user is never told the network is
    /// sealed when a granted command can reach it.
    pub const fn notice(self) -> &'static str {
        if self.network_confined {
            "the command runs in mandatory OS containment: approved command roots are writable, required runtime paths are read-only, network/PID/IPC/UTS namespaces are isolated, and only a fixed non-secret environment is supplied; executable identity is revalidated but not launched from an immutable handle"
        } else {
            "the command runs in OS containment with --allow-network: approved command roots are writable, required runtime paths are read-only, PID/IPC/UTS namespaces are isolated but THE NETWORK IS REACHABLE (and ~/.ssh/known_hosts + config are readable; private keys are not), and only a fixed non-secret environment is supplied; executable identity is revalidated but not launched from an immutable handle"
        }
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
    /// Hard ceiling for the completed persistent artifact representation,
    /// including framing/encoding metadata rather than only raw pipe bytes.
    pub limit_bytes: u64,
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
    /// Aggregate raw stdout/stderr bytes offered by the command layer. A sink
    /// whose encoding reaches the serialized ceiling earlier must return its
    /// authoritative retained count in [`CommandOutputArtifact`].
    pub captured_bytes: u64,
    /// True when the command layer omitted additional raw bytes at its own cap.
    /// Sinks may additionally report encoding/framing truncation on output.
    pub truncated: bool,
    pub limit_bytes: u64,
}

/// Opaque, host-controlled reference to a completed full-output artifact.
///
/// The sink decides whether this is a session ID, protected path, or another
/// handle. Kitsu never interprets it as a path and never passes it to the child.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CommandOutputArtifact {
    pub reference: String,
    #[serde(default)]
    pub captured_bytes: u64,
    #[serde(default)]
    pub truncated: bool,
    #[serde(default)]
    pub limit_bytes: u64,
    /// Exact bytes in the completed persistent artifact representation.
    #[serde(default)]
    pub artifact_bytes: u64,
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
    /// failure. This is required rather than defaulting to a no-op: callers may
    /// report the sink's result, so `Ok(())` must be an implementation-owned
    /// assertion that cleanup was actually attempted successfully.
    fn abort(&self) -> std::io::Result<()>;
}

/// Optional host hook for protected full command output.
///
/// The command tool remains safe-by-default: no artifact is created unless the
/// embedding frontend explicitly installs a sink on [`CommandPolicy`]. A sink
/// must enforce [`CommandArtifactRequest::limit_bytes`] against its completed
/// persistent representation (including encoding/framing overhead) and return
/// truthful [`CommandOutputArtifact`] counters. A sink failure never stops pipe
/// draining or strands a child process.
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
    /// Completed serialized byte cap for the optional artifact sink.
    max_artifact_bytes: usize,
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
            max_artifact_bytes: DEFAULT_COMMAND_ARTIFACT_BYTES,
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

    pub fn max_artifact_bytes(self) -> usize {
        self.max_artifact_bytes
    }

    pub fn with_max_artifact_bytes(mut self, max_artifact_bytes: usize) -> Result<Self, ToolError> {
        if max_artifact_bytes == 0 {
            return Err(ToolError::InvalidArgs(
                "command artifact cap must be greater than zero".to_string(),
            ));
        }
        self.max_artifact_bytes = max_artifact_bytes;
        Ok(self)
    }
}

impl Default for CommandLimits {
    fn default() -> Self {
        Self {
            default_timeout: DEFAULT_COMMAND_TIMEOUT,
            max_timeout: DEFAULT_MAX_COMMAND_TIMEOUT,
            max_output_bytes: DEFAULT_COMMAND_OUTPUT_BYTES,
            max_artifact_bytes: DEFAULT_COMMAND_ARTIFACT_BYTES,
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

/// Descriptor-bound command root retained for the policy lifetime. On Linux
/// containment mounts clones of these descriptors, so a rename/symlink swap
/// cannot redirect bubblewrap to a different host directory. On other unix
/// (macOS/Seatbelt) the descriptor is not passed to the sandbox — it names the
/// root by path — but the held fd still lets [`revalidate`](Self::revalidate)
/// detect a dev/ino change before the profile is built, matching the cwd's own
/// anti-rename check ([`AuthorizedCwd::revalidate`]).
#[derive(Debug)]
struct BoundCommandRoot {
    canonical: PathBuf,
    #[cfg(unix)]
    directory: std::fs::File,
}

impl BoundCommandRoot {
    fn bind(canonical: PathBuf) -> Result<Self, ToolError> {
        #[cfg(unix)]
        let directory = open_directory(&canonical).map_err(|error| {
            ToolError::PolicyDenied(format!(
                "command root '{}' could not be bound for containment: {error}",
                canonical.display()
            ))
        })?;
        Ok(Self {
            canonical,
            #[cfg(unix)]
            directory,
        })
    }

    fn revalidate(&self) -> Result<(), ToolError> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;

            let descriptor = self.directory.metadata().map_err(|error| {
                ToolError::PolicyDenied(format!(
                    "command root '{}' descriptor is unavailable: {error}",
                    self.canonical.display()
                ))
            })?;
            let path = std::fs::metadata(&self.canonical).map_err(|_| {
                ToolError::PolicyDenied(format!(
                    "command root '{}' changed after policy construction; refusing to execute",
                    self.canonical.display()
                ))
            })?;
            if !descriptor.is_dir()
                || !path.is_dir()
                || descriptor.dev() != path.dev()
                || descriptor.ino() != path.ino()
            {
                return Err(ToolError::PolicyDenied(format!(
                    "command root '{}' changed after policy construction; refusing to execute",
                    self.canonical.display()
                )));
            }
        }
        Ok(())
    }
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

#[derive(Clone)]
struct ContainmentBackendReady {
    backend_path: PathBuf,
    readonly_runtime_roots: Vec<PathBuf>,
    fingerprint: CommandExecutableFingerprint,
}

struct ContainedLaunch {
    program: PathBuf,
    arguments: Vec<OsString>,
    argv0: Option<OsString>,
    inherited_paths: Vec<std::fs::File>,
    shell_signal_status: bool,
    /// Working directory to set on the outer process (unix only). Bubblewrap
    /// leaves this `None` — it `--chdir`s inside its own argv. Seatbelt (which
    /// has no `--chdir`) and the direct test backend set the authorized cwd
    /// here, so the contained command runs in the directory it was approved
    /// for. Applied at the exec site; never a path the model chose.
    working_directory: Option<PathBuf>,
}

/// Policy inputs both containment backends need from an authorized command:
/// the canonical program, the writable command roots, the read-only runtime
/// roots, and the authorized cwd. Produced by [`authorized_launch_inputs`];
/// each backend turns it into its own enforcement (bwrap argv+fds, or a
/// Seatbelt profile).
struct LaunchInputs {
    program: PathBuf,
    readonly_runtime_roots: Vec<PathBuf>,
    writable_command_roots: Vec<PathBuf>,
    cwd: PathBuf,
    /// `--allow-network`: network isolation is dropped and the SSH host files
    /// become readable. Both backends read this off the one struct.
    allow_network: bool,
}

/// Pull the canonical executable and run the shared executable-visibility gate,
/// returning the path sets a backend enforces. Backend-agnostic: the identical
/// check that bwrap and Seatbelt both require before building enforcement.
fn authorized_launch_inputs(
    policy: &CommandPolicy,
    ready: &ContainmentBackendReady,
    authorized: &AuthorizedCommand,
) -> Result<LaunchInputs, CommandContainmentError> {
    let executable = authorized.executable.fingerprint.as_ref().ok_or_else(|| {
        CommandContainmentError::new(
            CommandContainmentUnavailableReason::ExecutableOutsideVisibleRoots,
            "approved executable has no validated path".to_string(),
        )
    })?;
    let executable_visible = policy
        .command_roots
        .iter()
        .chain(ready.readonly_runtime_roots.iter())
        .any(|root| executable.canonical_path.starts_with(root));
    if !executable_visible {
        let toolchain_dir = executable
            .canonical_path
            .parent()
            .map(|dir| dir.display().to_string())
            .unwrap_or_else(|| executable.canonical_path.display().to_string());
        return Err(CommandContainmentError::new(
            CommandContainmentUnavailableReason::ExecutableOutsideVisibleRoots,
            format!(
                "approved executable '{}' is outside every command/runtime root; its own directory is a separate root requirement on top of the cwd root — add it with `--command-root {toolchain_dir}`",
                executable.canonical_path.display()
            ),
        ));
    }
    Ok(LaunchInputs {
        program: executable.canonical_path.clone(),
        readonly_runtime_roots: ready.readonly_runtime_roots.clone(),
        writable_command_roots: policy
            .bound_command_roots
            .iter()
            .map(|root| root.canonical.clone())
            .collect(),
        cwd: authorized.cwd.canonical.clone(),
        allow_network: policy.containment_config.allow_network,
    })
}

/// The contained child's environment: runtime-root toolchain wiring first
/// (PATH additions, CARGO_HOME/RUSTUP_HOME), then user `--command-env`
/// pass-through, which wins on a name clash. Bubblewrap feeds this into its own
/// `--clearenv`/`--setenv` argv; the exec site also applies it to the outer
/// process so Seatbelt (which inherits the outer env) sees the same set. Kept
/// as one formula so the two cannot drift.
fn runtime_plus_passthrough_env(
    config_roots: &[PathBuf],
    passthrough: &[(String, String)],
) -> Vec<(String, String)> {
    let mut env = runtime_root_environment(config_roots);
    env.extend(passthrough.iter().cloned());
    env
}

trait CommandContainmentBackend: Send + Sync {
    fn preflight(&self) -> Result<ContainmentBackendReady, CommandContainmentError>;

    fn refresh(&self);

    fn prepare(
        &self,
        policy: &CommandPolicy,
        authorized: &AuthorizedCommand,
        argv: &[String],
    ) -> Result<ContainedLaunch, CommandContainmentError>;
}

/// Linux (and the Windows/other fail-closed fallback) command containment via
/// Bubblewrap. Not built on macOS, which uses [`SeatbeltBackend`].
#[cfg(not(target_os = "macos"))]
struct BubblewrapBackend {
    config: CommandContainmentConfig,
    cached_preflight: Mutex<Option<Result<ContainmentBackendReady, CommandContainmentError>>>,
}

#[cfg(not(target_os = "macos"))]
impl BubblewrapBackend {
    fn new(config: CommandContainmentConfig) -> Self {
        Self {
            config,
            cached_preflight: Mutex::new(None),
        }
    }

    fn run_preflight(&self) -> Result<ContainmentBackendReady, CommandContainmentError> {
        #[cfg(not(target_os = "linux"))]
        {
            return Err(CommandContainmentError::new(
                CommandContainmentUnavailableReason::UnsupportedPlatform,
                format!(
                    "{} has no reviewed command-containment backend",
                    std::env::consts::OS
                ),
            ));
        }

        #[cfg(target_os = "linux")]
        {
            let configured = &self.config.bubblewrap_path;
            if !configured.is_absolute() {
                return Err(CommandContainmentError::new(
                    CommandContainmentUnavailableReason::InvalidConfiguration,
                    format!("bubblewrap path '{}' is not absolute", configured.display()),
                ));
            }
            if !configured.exists() {
                return Err(CommandContainmentError::new(
                    CommandContainmentUnavailableReason::BackendMissing,
                    format!("bubblewrap was not found at '{}'", configured.display()),
                ));
            }
            let fingerprint = fingerprint_executable(configured).ok_or_else(|| {
                CommandContainmentError::new(
                    CommandContainmentUnavailableReason::BackendNotExecutable,
                    format!(
                        "bubblewrap at '{}' is not a regular executable",
                        configured.display()
                    ),
                )
            })?;
            let help = std::process::Command::new(&fingerprint.canonical_path)
                .arg("--help")
                .env_clear()
                .stdin(Stdio::null())
                .output()
                .map_err(|error| {
                    CommandContainmentError::new(
                        CommandContainmentUnavailableReason::BackendProbeFailed,
                        format!("could not inspect bubblewrap capabilities: {error}"),
                    )
                })?;
            if !help.status.success()
                || !String::from_utf8_lossy(&help.stdout).contains("--bind-fd")
                || !String::from_utf8_lossy(&help.stdout).contains("--disable-userns")
            {
                return Err(CommandContainmentError::new(
                    CommandContainmentUnavailableReason::BackendProbeFailed,
                    "bubblewrap lacks required descriptor mounts or nested-user-namespace disabling"
                        .to_string(),
                ));
            }
            let runtime_mounts = runtime_mount_paths(&self.config)?;
            let true_path = [Path::new("/usr/bin/true"), Path::new("/bin/true")]
                .into_iter()
                .find(|path| path.is_file())
                .ok_or_else(|| {
                    CommandContainmentError::new(
                        CommandContainmentUnavailableReason::RuntimeUnavailable,
                        "neither /usr/bin/true nor /bin/true is available for the containment probe"
                            .to_string(),
                    )
                })?;
            let mut arguments = base_bubblewrap_arguments(&runtime_mounts, &[], false);
            arguments.push(OsString::from("--"));
            arguments.push(true_path.as_os_str().to_os_string());
            let output = std::process::Command::new(&fingerprint.canonical_path)
                .args(&arguments)
                .env_clear()
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .output()
                .map_err(|error| {
                    CommandContainmentError::new(
                        CommandContainmentUnavailableReason::BackendProbeFailed,
                        format!("could not start bubblewrap capability probe: {error}"),
                    )
                })?;
            if !output.status.success() {
                let detail = safe_backend_diagnostic(&output.stderr);
                return Err(CommandContainmentError::new(
                    CommandContainmentUnavailableReason::BackendProbeFailed,
                    format!(
                        "bubblewrap could not create the required mount/network/PID/IPC/UTS namespaces{}",
                        if detail.is_empty() {
                            String::new()
                        } else {
                            format!(": {detail}")
                        }
                    ),
                ));
            }
            if fingerprint_executable(&fingerprint.canonical_path).as_ref() != Some(&fingerprint) {
                return Err(CommandContainmentError::new(
                    CommandContainmentUnavailableReason::BackendChanged,
                    "bubblewrap changed during containment preflight".to_string(),
                ));
            }
            Ok(ContainmentBackendReady {
                backend_path: fingerprint.canonical_path.clone(),
                readonly_runtime_roots: runtime_mounts,
                fingerprint,
            })
        }
    }
}

#[cfg(not(target_os = "macos"))]
impl CommandContainmentBackend for BubblewrapBackend {
    fn preflight(&self) -> Result<ContainmentBackendReady, CommandContainmentError> {
        let mut cached = self.cached_preflight.lock().unwrap();
        if let Some(result) = cached.as_ref() {
            return result.clone();
        }
        let result = self.run_preflight();
        *cached = Some(result.clone());
        result
    }

    fn refresh(&self) {
        *self.cached_preflight.lock().unwrap() = None;
    }

    fn prepare(
        &self,
        policy: &CommandPolicy,
        authorized: &AuthorizedCommand,
        argv: &[String],
    ) -> Result<ContainedLaunch, CommandContainmentError> {
        let ready = self.preflight()?;
        if fingerprint_executable(&ready.backend_path).as_ref() != Some(&ready.fingerprint) {
            return Err(CommandContainmentError::new(
                CommandContainmentUnavailableReason::BackendChanged,
                format!(
                    "bubblewrap at '{}' changed after preflight",
                    ready.backend_path.display()
                ),
            ));
        }
        // Shared executable-visibility gate + canonical program path.
        let inputs = authorized_launch_inputs(policy, &ready, authorized)?;

        #[cfg(not(target_os = "linux"))]
        {
            let _ = (policy, authorized, argv, inputs);
            Err(CommandContainmentError::new(
                CommandContainmentUnavailableReason::UnsupportedPlatform,
                "bubblewrap containment is Linux-only".to_string(),
            ))
        }

        #[cfg(target_os = "linux")]
        {
            use std::os::fd::AsRawFd;

            // Shared env formula (runtime-root wiring then --command-env
            // pass-through, pass-through wins). base_bubblewrap_arguments feeds
            // it into bwrap's own --clearenv/--setenv.
            let launch_environment = runtime_plus_passthrough_env(
                self.config.readonly_runtime_roots(),
                &policy.passthrough_environment,
            );
            let allow_network = self.config.allow_network();
            let mut arguments = base_bubblewrap_arguments(&[], &launch_environment, allow_network);
            // `--allow-network`: bind the SSH host files read-only so `ssh` can
            // verify a host (the empty /etc the base builds has none). Only
            // these two files — never ~/.ssh — so private keys never enter the
            // sandbox. A name-based host still needs DNS (the declined option);
            // an IP-based one works with these alone.
            if allow_network {
                for file in ssh_host_read_files() {
                    arguments.push(OsString::from("--ro-bind-try"));
                    arguments.push(file.as_os_str().to_os_string());
                    arguments.push(file.as_os_str().to_os_string());
                }
            }
            let mut inherited_paths = Vec::with_capacity(
                ready
                    .readonly_runtime_roots
                    .len()
                    .saturating_add(policy.bound_command_roots.len())
                    .saturating_add(1),
            );
            for runtime in &ready.readonly_runtime_roots {
                let descriptor = open_containment_path(runtime).map_err(|error| {
                    CommandContainmentError::new(
                        CommandContainmentUnavailableReason::RuntimeUnavailable,
                        format!(
                            "cannot bind read-only runtime path '{}': {error}",
                            runtime.display()
                        ),
                    )
                })?;
                arguments.push(OsString::from("--ro-bind-fd"));
                arguments.push(OsString::from(descriptor.as_raw_fd().to_string()));
                arguments.push(runtime.as_os_str().to_os_string());
                inherited_paths.push(descriptor);
            }
            for root in &policy.bound_command_roots {
                root.revalidate().map_err(containment_policy_error)?;
                let directory = root.directory.try_clone().map_err(|error| {
                    CommandContainmentError::new(
                        CommandContainmentUnavailableReason::RuntimeUnavailable,
                        format!(
                            "cannot clone descriptor for command root '{}': {error}",
                            root.canonical.display()
                        ),
                    )
                })?;
                arguments.push(OsString::from("--bind-fd"));
                arguments.push(OsString::from(directory.as_raw_fd().to_string()));
                arguments.push(root.canonical.as_os_str().to_os_string());
                inherited_paths.push(directory);
            }
            // Rebind the exact descriptor-authorized cwd last. This preserves
            // its filesystem identity even if an ancestor is renamed between
            // the final revalidation and bubblewrap's mount setup.
            let cwd = authorized.cwd.directory.try_clone().map_err(|error| {
                CommandContainmentError::new(
                    CommandContainmentUnavailableReason::RuntimeUnavailable,
                    format!("cannot clone approved cwd descriptor: {error}"),
                )
            })?;
            arguments.push(OsString::from("--bind-fd"));
            arguments.push(OsString::from(cwd.as_raw_fd().to_string()));
            arguments.push(authorized.cwd.canonical.as_os_str().to_os_string());
            inherited_paths.push(cwd);
            arguments.push(OsString::from("--chdir"));
            arguments.push(authorized.cwd.canonical.as_os_str().to_os_string());
            arguments.push(OsString::from("--argv0"));
            arguments.push(OsString::from(&argv[0]));
            arguments.push(OsString::from("--"));
            arguments.push(inputs.program.as_os_str().to_os_string());
            arguments.extend(argv[1..].iter().map(OsString::from));
            Ok(ContainedLaunch {
                program: ready.backend_path,
                arguments,
                argv0: None,
                inherited_paths,
                // Bubblewrap documents 128+signal as the contained child's
                // signal termination encoding.
                shell_signal_status: true,
                // bwrap --chdir's inside its own argv.
                working_directory: None,
            })
        }
    }
}

/// The production containment backend for this build's OS. Linux → Bubblewrap,
/// macOS → Seatbelt. Every other platform (e.g. Windows) falls back to the
/// Bubblewrap backend, whose off-Linux `run_preflight`/`prepare` fail closed
/// with `UnsupportedPlatform` — the single slot a future native backend plugs
/// into. This is the only OS-vs-OS branch; everything above it is shared.
fn default_containment_backend(
    config: CommandContainmentConfig,
) -> Arc<dyn CommandContainmentBackend> {
    #[cfg(target_os = "macos")]
    {
        Arc::new(SeatbeltBackend::new(config))
    }
    #[cfg(not(target_os = "macos"))]
    {
        Arc::new(BubblewrapBackend::new(config))
    }
}

/// Mirrors [`BubblewrapBackend`]: a memoized preflight that fingerprints the
/// system `sandbox-exec` and validates the runtime roots, and a `prepare` that
/// turns the shared [`LaunchInputs`] into a deny-by-default SBPL profile passed
/// inline via `-p`. Unlike bwrap it binds roots by canonical PATH in the
/// profile, not by descriptor; the anti-rename defense is the dev/ino
/// revalidation that [`BoundCommandRoot`]/[`AuthorizedCwd`] already perform on
/// unix before this runs (see the root-pinning note on `build_seatbelt_profile`).
#[cfg(target_os = "macos")]
struct SeatbeltBackend {
    config: CommandContainmentConfig,
    cached_preflight: Mutex<Option<Result<ContainmentBackendReady, CommandContainmentError>>>,
}

/// The fixed macOS system path to Seatbelt's launcher. Unlike `bwrap` (which
/// varies by distro and is configurable) this ships with the OS at one place.
// ponytail: fixed system path; add a config knob only if a non-standard
// location ever shows up.
#[cfg(target_os = "macos")]
const DEFAULT_SANDBOX_EXEC_PATH: &str = "/usr/bin/sandbox-exec";

#[cfg(target_os = "macos")]
impl SeatbeltBackend {
    fn new(config: CommandContainmentConfig) -> Self {
        Self {
            config,
            cached_preflight: Mutex::new(None),
        }
    }

    fn run_preflight(&self) -> Result<ContainmentBackendReady, CommandContainmentError> {
        {
            let configured = Path::new(DEFAULT_SANDBOX_EXEC_PATH);
            if !configured.exists() {
                return Err(CommandContainmentError::new(
                    CommandContainmentUnavailableReason::BackendMissing,
                    format!("sandbox-exec was not found at '{}'", configured.display()),
                ));
            }
            let fingerprint = fingerprint_executable(configured).ok_or_else(|| {
                CommandContainmentError::new(
                    CommandContainmentUnavailableReason::BackendNotExecutable,
                    format!(
                        "sandbox-exec at '{}' is not a regular executable",
                        configured.display()
                    ),
                )
            })?;
            // Capability probe: a trivial allow-all profile around /usr/bin/true
            // must exit 0. Non-empty stderr is NOT a failure — some macOS builds
            // print a deprecation note (verified absent on the dev host, but we
            // do not depend on that). Only the exit status gates readiness.
            let probe = std::process::Command::new(&fingerprint.canonical_path)
                .args(["-p", "(version 1)(allow default)", "/usr/bin/true"])
                .env_clear()
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .output()
                .map_err(|error| {
                    CommandContainmentError::new(
                        CommandContainmentUnavailableReason::BackendProbeFailed,
                        format!("could not start sandbox-exec capability probe: {error}"),
                    )
                })?;
            if !probe.status.success() {
                let detail = safe_backend_diagnostic(&probe.stderr);
                return Err(CommandContainmentError::new(
                    CommandContainmentUnavailableReason::BackendProbeFailed,
                    format!(
                        "sandbox-exec could not apply a trivial sandbox profile{}",
                        if detail.is_empty() {
                            String::new()
                        } else {
                            format!(": {detail}")
                        }
                    ),
                ));
            }
            // System executable/library dirs are read-only roots too, so system
            // tools (/bin/sh, /usr/bin/git) pass the shared executable-visibility
            // gate — exactly as Linux's runtime_mount_paths seeds /usr,/bin,…
            // into readonly_runtime_roots. Tools outside these (Homebrew in
            // /opt/homebrew, /usr/local) need a --command-root, same as Linux.
            let mut readonly_runtime_roots: Vec<PathBuf> = ["/usr", "/bin", "/sbin"]
                .into_iter()
                .map(PathBuf::from)
                .filter(|path| path.exists())
                .collect();
            for root in validate_runtime_roots(&self.config)? {
                if !readonly_runtime_roots
                    .iter()
                    .any(|seed| root.starts_with(seed))
                {
                    readonly_runtime_roots.push(root);
                }
            }
            if fingerprint_executable(&fingerprint.canonical_path).as_ref() != Some(&fingerprint) {
                return Err(CommandContainmentError::new(
                    CommandContainmentUnavailableReason::BackendChanged,
                    "sandbox-exec changed during containment preflight".to_string(),
                ));
            }
            Ok(ContainmentBackendReady {
                backend_path: fingerprint.canonical_path.clone(),
                readonly_runtime_roots,
                fingerprint,
            })
        }
    }
}

#[cfg(target_os = "macos")]
impl CommandContainmentBackend for SeatbeltBackend {
    fn preflight(&self) -> Result<ContainmentBackendReady, CommandContainmentError> {
        let mut cached = self.cached_preflight.lock().unwrap();
        if let Some(result) = cached.as_ref() {
            return result.clone();
        }
        let result = self.run_preflight();
        *cached = Some(result.clone());
        result
    }

    fn refresh(&self) {
        *self.cached_preflight.lock().unwrap() = None;
    }

    fn prepare(
        &self,
        policy: &CommandPolicy,
        authorized: &AuthorizedCommand,
        argv: &[String],
    ) -> Result<ContainedLaunch, CommandContainmentError> {
        let ready = self.preflight()?;
        if fingerprint_executable(&ready.backend_path).as_ref() != Some(&ready.fingerprint) {
            return Err(CommandContainmentError::new(
                CommandContainmentUnavailableReason::BackendChanged,
                format!(
                    "sandbox-exec at '{}' changed after preflight",
                    ready.backend_path.display()
                ),
            ));
        }
        // Shared executable-visibility gate + canonical path sets. The cwd and
        // command roots have already had their dev/ino anti-rename check at the
        // exec site (AuthorizedCwd + BoundCommandRoot revalidate, both #[cfg(unix)]).
        let inputs = authorized_launch_inputs(policy, &ready, authorized)?;

        let profile = build_seatbelt_profile(&inputs)?;
        let mut arguments = Vec::with_capacity(argv.len().saturating_add(3));
        arguments.push(OsString::from("-p"));
        arguments.push(OsString::from(profile));
        arguments.push(inputs.program.as_os_str().to_os_string());
        arguments.extend(argv[1..].iter().map(OsString::from));
        Ok(ContainedLaunch {
            program: ready.backend_path,
            arguments,
            // sandbox-exec has no inner-argv0 override; the target sees
            // argv[0] = its canonical path. ponytail: add an `exec -a` shim
            // only if a basename-dispatching tool ever needs it.
            argv0: None,
            // Roots are named by path in the profile; no fds are passed.
            inherited_paths: Vec::new(),
            // sandbox-exec execve's the target in place, so waitpid yields the
            // target's own signal status (verified: `kill -TERM $$` under a
            // trivial profile reports SIGTERM). So decode signals the normal
            // way, not bwrap's 128+n convention.
            shell_signal_status: false,
            // No --chdir equivalent: run the child in its authorized cwd.
            working_directory: Some(inputs.cwd.clone()),
        })
    }
}

/// Build a deny-by-default Seatbelt (SBPL) profile confining a command to its
/// authorized roots. Writable access is EXACTLY the writable command roots, the
/// cwd, `/private/tmp` (the canonical `TMPDIR=/tmp`), and the three write-only
/// device nodes — nothing else. Reads cover the system/toolchain paths a
/// dynamically-linked binary needs. Network is denied.
///
/// Root-pinning note: Seatbelt names canonical PATHS here, so (unlike Linux's
/// descriptor bind) a path-component swap between the exec-site dev/ino
/// revalidation and sandbox-exec's own `open` could still redirect a write.
/// This is the same honest TOCTOU limit Linux already carries for the inner
/// executable (`pinned=false`); `sandbox-exec` takes no fd, so true fd-pinning
/// of roots is impossible via its CLI.
///
/// ponytail: the mach-lookup and dyld/system read sets are deliberately
/// generous — the enforced boundary is write-confinement + no network. Narrow
/// them only if a concrete escape or a toolchain-load failure demands it.
#[cfg(target_os = "macos")]
fn build_seatbelt_profile(inputs: &LaunchInputs) -> Result<String, CommandContainmentError> {
    use std::fmt::Write as _;

    let mut writable: Vec<&Path> = Vec::new();
    for root in &inputs.writable_command_roots {
        writable.push(root.as_path());
    }
    writable.push(inputs.cwd.as_path());

    let mut profile = String::new();
    profile.push_str("(version 1)\n");
    profile.push_str("(deny default)\n");
    // Network: denied by default. `--allow-network` must ALLOW it explicitly —
    // under `(deny default)`, merely omitting the deny still leaves network
    // denied, so a granted ssh/curl gets "Operation not permitted" on connect.
    // Write-confinement, the exec allow-list, and the secret-read denials below
    // are unaffected either way.
    if inputs.allow_network {
        profile.push_str("(allow network*)\n");
        // An outbound connection needs the socket syscalls too; `network*`
        // covers connect/bind but not socket creation under `(deny default)`.
        profile.push_str("(allow system-socket)\n");
    } else {
        profile.push_str("(deny network*)\n");
    }
    profile.push_str("(allow process-fork)\n");
    profile.push_str("(allow sysctl-read)\n");

    // Exec: only the binaries a Linux bwrap sandbox would mount — system dirs,
    // the Xcode/CLT toolchain that /usr/bin/git & co. shim into, and the
    // command + runtime roots (review F2). A granted `sh -c` can no longer
    // reach a binary outside them (e.g. /opt/homebrew without a root).
    profile.push_str("(allow process-exec*\n");
    for sys in [
        "/usr",
        "/bin",
        "/sbin",
        "/System",
        "/Library/Developer",
        "/Applications/Xcode.app",
    ] {
        let _ = writeln!(profile, "    (subpath \"{sys}\")");
    }
    for path in writable
        .iter()
        .copied()
        .chain(inputs.readonly_runtime_roots.iter().map(PathBuf::as_path))
    {
        write_subpath(&mut profile, path)?;
    }
    profile.push_str(")\n");

    // Mach services: an allow-list instead of every service (review F1). An
    // unrestricted mach-lookup reaches launchd/LaunchServices, which can start
    // processes outside the sandbox. These two are what sh/git/coreutils need
    // (verified empirically): per-user temp-dir resolution and user/group name
    // lookup.
    profile.push_str("(allow mach-lookup\n");
    let mut mach_services = vec![
        "com.apple.bsd.dirhelper",
        "com.apple.system.opendirectoryd.libinfo",
    ];
    if inputs.allow_network {
        // Outbound TCP consults SystemConfiguration for route/reachability
        // even for a bare IP; without it connect() fails before leaving the
        // host. DNS (mDNSResponder) is deliberately NOT added — name
        // resolution is the separate option the user did not take, so a
        // hostname still fails and an IP works.
        mach_services.push("com.apple.SystemConfiguration.configd");
    }
    for service in mach_services {
        let _ = writeln!(profile, "    (global-name \"{service}\")");
    }
    profile.push_str(")\n");

    // Reads stay broad: dyld's read set is SIP-hidden and version-specific, so
    // a read allow-list fails the wrong way (SIGABRT). Instead the known
    // secret locations are denied AFTER the broad allow — in SBPL the later
    // matching rule wins (verified) — so a contained command cannot read SSH
    // keys, cloud credentials, keychains, or the shell rc files that hold
    // gateway tokens (review F1).
    profile.push_str("(allow file-read*)\n");
    let secrets = seatbelt_secret_paths();
    if !secrets.is_empty() {
        profile.push_str("(deny file-read*\n");
        for path in &secrets {
            write_subpath(&mut profile, path)?;
        }
        profile.push_str(")\n");
    }
    // `--allow-network`: re-allow exactly the two SSH host files after the
    // blanket `~/.ssh` deny (the later rule wins), so `ssh` can read a host's
    // key and config to verify it. Private keys (`id_*`, `identity`) stay under
    // the deny — only these two literals are carved back out.
    if inputs.allow_network {
        let ssh_files = ssh_host_read_files();
        if !ssh_files.is_empty() {
            profile.push_str("(allow file-read*\n");
            for path in &ssh_files {
                write_literal(&mut profile, path)?;
            }
            profile.push_str(")\n");
        }
    }

    // Writes: ONLY the writable command roots, the cwd, the temp dir, and the
    // write-only device nodes. This is the enforced boundary the tests pin.
    profile.push_str("(allow file-write*\n");
    for path in &writable {
        write_subpath(&mut profile, path)?;
    }
    let _ = writeln!(profile, "    (subpath \"/private/tmp\")");
    for dev in ["/dev/null", "/dev/stdout", "/dev/stderr", "/dev/tty"] {
        let _ = writeln!(profile, "    (literal \"{dev}\")");
    }
    profile.push_str(")\n");

    Ok(profile)
}

/// Host paths a contained command must never read, relative to $HOME. The
/// kitsu config/state dirs are included: they hold managed credentials.
/// Shell rc files are included because users keep gateway tokens there.
// ponytail: fixed list of well-known secret homes; make it configurable if a
// user needs to add their own.
#[cfg(target_os = "macos")]
const SEATBELT_SECRET_HOME_PATHS: &[&str] = &[
    ".ssh",
    ".aws",
    ".gnupg",
    ".config/gcloud",
    ".azure",
    ".kube",
    ".docker",
    ".netrc",
    ".git-credentials",
    ".kitsu",
    ".config/kitsu",
    ".zshrc",
    ".zprofile",
    ".zshenv",
    ".bashrc",
    ".bash_profile",
    ".profile",
    "Library/Keychains",
];

/// The secret paths under the real home directory, canonicalized where they
/// exist (Seatbelt matches canonical paths). Missing paths are still listed
/// under the canonical home so a later-created file is covered.
#[cfg(target_os = "macos")]
fn seatbelt_secret_paths() -> Vec<PathBuf> {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return Vec::new();
    };
    let home = std::fs::canonicalize(&home).unwrap_or(home);
    SEATBELT_SECRET_HOME_PATHS
        .iter()
        .map(|rel| {
            let path = home.join(rel);
            std::fs::canonicalize(&path).unwrap_or(path)
        })
        .collect()
}

/// Emit one `    (subpath "…")` line with the path safely SBPL-quoted.
#[cfg(target_os = "macos")]
fn write_subpath(profile: &mut String, path: &Path) -> Result<(), CommandContainmentError> {
    use std::fmt::Write as _;
    let quoted = sbpl_quote(path)?;
    let _ = writeln!(profile, "    (subpath {quoted})");
    Ok(())
}

/// Emit one `    (literal "…")` line (a single file, not a subtree).
#[cfg(target_os = "macos")]
fn write_literal(profile: &mut String, path: &Path) -> Result<(), CommandContainmentError> {
    use std::fmt::Write as _;
    let quoted = sbpl_quote(path)?;
    let _ = writeln!(profile, "    (literal {quoted})");
    Ok(())
}

/// The SSH host files a contained `ssh` needs to read to verify a host under
/// `--allow-network`: `~/.ssh/known_hosts` and `~/.ssh/config`. Just these two
/// — never the whole `~/.ssh`, so private keys stay denied. Canonicalized where
/// present; a missing file is still listed under the canonical home so a
/// later-created one is covered.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn ssh_host_read_files() -> Vec<PathBuf> {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return Vec::new();
    };
    let home = std::fs::canonicalize(&home).unwrap_or(home);
    [".ssh/known_hosts", ".ssh/config"]
        .iter()
        .map(|rel| {
            let path = home.join(rel);
            std::fs::canonicalize(&path).unwrap_or(path)
        })
        .collect()
}

/// Quote a path as an SBPL string literal. Trust boundary: a path is about to
/// become part of a security policy, so this fails closed rather than produce a
/// profile that doesn't mean what it says.
///
/// - Non-UTF-8 → refuse (never lossily transcode a path into a policy).
/// - Control chars / newlines → refuse (defends the s-expression structure).
/// - Otherwise wrap in `"`, backslash-escaping `\` and `"`. A literal `)` inside
///   a quoted SBPL string is inert, so escaping `\` and `"` is sufficient.
#[cfg(target_os = "macos")]
fn sbpl_quote(path: &Path) -> Result<String, CommandContainmentError> {
    let text = path.to_str().ok_or_else(|| {
        CommandContainmentError::new(
            CommandContainmentUnavailableReason::InvalidConfiguration,
            format!("containment path is not valid UTF-8: {}", path.display()),
        )
    })?;
    if text.chars().any(|c| c.is_control()) {
        return Err(CommandContainmentError::new(
            CommandContainmentUnavailableReason::InvalidConfiguration,
            format!(
                "containment path contains a control character: {}",
                path.display()
            ),
        ));
    }
    let mut quoted = String::with_capacity(text.len() + 2);
    quoted.push('"');
    for c in text.chars() {
        if c == '\\' || c == '"' {
            quoted.push('\\');
        }
        quoted.push(c);
    }
    quoted.push('"');
    Ok(quoted)
}

#[cfg(test)]
struct DirectTestContainmentBackend;

#[cfg(test)]
impl CommandContainmentBackend for DirectTestContainmentBackend {
    fn preflight(&self) -> Result<ContainmentBackendReady, CommandContainmentError> {
        let path = std::env::current_exe().map_err(|error| {
            CommandContainmentError::new(
                CommandContainmentUnavailableReason::BackendProbeFailed,
                error.to_string(),
            )
        })?;
        let fingerprint = fingerprint_executable(&path).ok_or_else(|| {
            CommandContainmentError::new(
                CommandContainmentUnavailableReason::BackendProbeFailed,
                "test executable could not be fingerprinted".to_string(),
            )
        })?;
        Ok(ContainmentBackendReady {
            backend_path: path,
            readonly_runtime_roots: Vec::new(),
            fingerprint,
        })
    }

    fn refresh(&self) {}

    fn prepare(
        &self,
        _policy: &CommandPolicy,
        authorized: &AuthorizedCommand,
        argv: &[String],
    ) -> Result<ContainedLaunch, CommandContainmentError> {
        let executable = authorized
            .executable
            .fingerprint
            .as_ref()
            .expect("authorized test command is resolved");
        Ok(ContainedLaunch {
            program: executable.canonical_path.clone(),
            arguments: argv[1..].iter().map(OsString::from).collect(),
            argv0: Some(OsString::from(&argv[0])),
            inherited_paths: Vec::new(),
            shell_signal_status: false,
            working_directory: Some(authorized.cwd.canonical.clone()),
        })
    }
}

// Used only by the Linux bwrap root-fd revalidation path.
#[cfg(target_os = "linux")]
fn containment_policy_error(error: ToolError) -> CommandContainmentError {
    CommandContainmentError::new(
        CommandContainmentUnavailableReason::InvalidConfiguration,
        error.to_string(),
    )
}

fn containment_denied(error: CommandContainmentError) -> ToolError {
    ToolError::PolicyDenied(error.to_string())
}

#[cfg(target_os = "linux")]
fn runtime_mount_paths(
    config: &CommandContainmentConfig,
) -> Result<Vec<PathBuf>, CommandContainmentError> {
    let mut mounts = Vec::new();
    for path in ["/usr", "/bin", "/lib", "/lib64", "/sbin"] {
        let path = PathBuf::from(path);
        if path.exists() {
            mounts.push(path);
        }
    }
    for path in ["/etc/ld.so.cache", "/etc/ld.so.conf", "/etc/ld.so.conf.d"] {
        let path = PathBuf::from(path);
        if path.exists() {
            mounts.push(path);
        }
    }
    // The Debian/Fedora "alternatives" symlink farm: `/usr/bin/awk`,
    // `/usr/bin/editor`, `/usr/bin/pager`, `cc`, and friends are symlinks into
    // `/etc/alternatives`, which in turn points back under `/usr/bin`. `/usr` is
    // already mounted above, but without the intermediate `/etc/alternatives`
    // directory the chain dead-ends inside the sandbox and the tool resolves to
    // "command not found". Mounting the farm read-only restores those tools
    // without exposing anything writable.
    let alternatives = PathBuf::from("/etc/alternatives");
    if alternatives.exists() {
        mounts.push(alternatives);
    }
    // Append the validated config runtime roots, skipping any already covered
    // by a system mount above.
    for current in validate_runtime_roots(config)? {
        if !mounts.iter().any(|root| current.starts_with(root)) {
            mounts.push(current);
        }
    }
    Ok(mounts)
}

/// Re-canonicalize each configured read-only runtime root and fail closed if it
/// changed or is no longer a directory, de-duplicating roots nested in earlier
/// ones. Backend-agnostic: Linux folds these into its bind-mount list, Seatbelt
/// uses them directly as the profile's read-only roots (and as the source of
/// the toolchain env via [`runtime_root_environment`]).
fn validate_runtime_roots(
    config: &CommandContainmentConfig,
) -> Result<Vec<PathBuf>, CommandContainmentError> {
    let mut roots: Vec<PathBuf> = Vec::new();
    for configured in &config.readonly_runtime_roots {
        let current = std::fs::canonicalize(configured).map_err(|error| {
            CommandContainmentError::new(
                CommandContainmentUnavailableReason::RuntimeUnavailable,
                format!(
                    "read-only runtime root '{}' is no longer accessible: {error}",
                    configured.display()
                ),
            )
        })?;
        if &current != configured || !current.is_dir() {
            return Err(CommandContainmentError::new(
                CommandContainmentUnavailableReason::RuntimeUnavailable,
                format!(
                    "read-only runtime root '{}' changed after configuration",
                    configured.display()
                ),
            ));
        }
        if !roots.iter().any(|root| current.starts_with(root)) {
            roots.push(current);
        }
    }
    Ok(roots)
}

/// Environment adjustments derived from configured read-only runtime roots.
/// A toolchain mounted into the sandbox is useless if nothing points at it:
/// each root's `bin/` directory joins the fixed containment PATH, and
/// `.cargo` / `.rustup` roots name CARGO_HOME / RUSTUP_HOME so their
/// launchers resolve without the (deliberately absent) real $HOME. Explicit
/// `--command-env` pass-through entries still override these by name.
fn runtime_root_environment(roots: &[PathBuf]) -> Vec<(String, String)> {
    let mut environment = Vec::new();
    let mut path_additions = Vec::new();
    for root in roots {
        let bin = root.join("bin");
        if bin.is_dir() {
            path_additions.push(bin.display().to_string());
        }
        match root.file_name().and_then(|name| name.to_str()) {
            Some(".cargo") => {
                environment.push(("CARGO_HOME".to_string(), root.display().to_string()));
            }
            Some(".rustup") => {
                environment.push(("RUSTUP_HOME".to_string(), root.display().to_string()));
            }
            _ => {}
        }
    }
    if !path_additions.is_empty() {
        let base_path = COMMAND_CONTAINMENT_ENVIRONMENT
            .iter()
            .find(|(name, _)| *name == "PATH")
            .map(|(_, value)| *value)
            .unwrap_or_default();
        environment.push((
            "PATH".to_string(),
            format!("{}:{base_path}", path_additions.join(":")),
        ));
    }
    environment
}

#[cfg(target_os = "linux")]
fn base_bubblewrap_arguments(
    runtime_mounts: &[PathBuf],
    extra_env: &[(String, String)],
    allow_network: bool,
) -> Vec<OsString> {
    let mut flags = vec!["--die-with-parent", "--new-session", "--unshare-user"];
    // Network isolation is dropped only under `--allow-network`; the other
    // namespaces (pid/ipc/uts/cgroup) and the userns lockdown are unchanged.
    if !allow_network {
        flags.push("--unshare-net");
    }
    flags.extend([
        "--unshare-pid",
        "--unshare-ipc",
        "--unshare-uts",
        "--unshare-cgroup-try",
        "--disable-userns",
        "--hostname",
        "kitsu",
        "--cap-drop",
        "ALL",
        "--proc",
        "/proc",
        "--dev",
        "/dev",
        "--tmpfs",
        "/tmp",
        "--dir",
        "/tmp/kitsu-home",
        "--dir",
        "/etc",
    ]);
    let mut arguments = flags.into_iter().map(OsString::from).collect::<Vec<_>>();
    for path in runtime_mounts {
        arguments.push(OsString::from("--ro-bind"));
        arguments.push(path.as_os_str().to_os_string());
        arguments.push(path.as_os_str().to_os_string());
    }
    arguments.push(OsString::from("--clearenv"));
    for (name, value) in merged_containment_environment(extra_env) {
        arguments.push(OsString::from("--setenv"));
        arguments.push(OsString::from(name));
        arguments.push(OsString::from(value));
    }
    arguments
}

#[cfg(target_os = "linux")]
fn open_containment_path(path: &Path) -> std::io::Result<std::fs::File> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let path = CString::new(path.as_os_str().as_bytes()).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "containment path contains a NUL byte",
        )
    })?;
    // SAFETY: `path` is a live NUL-terminated string. O_PATH pins the selected
    // filesystem object without opening its content and ownership transfers to
    // the returned File exactly once.
    let descriptor = unsafe { libc::open(path.as_ptr(), libc::O_PATH | libc::O_CLOEXEC) };
    if descriptor == -1 {
        Err(std::io::Error::last_os_error())
    } else {
        // SAFETY: successful `open` returned one uniquely owned descriptor.
        Ok(unsafe { std::fs::File::from_raw_fd(descriptor) })
    }
}

fn safe_backend_diagnostic(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .chars()
        .filter(|character| !character.is_control() || *character == ' ')
        .take(240)
        .collect::<String>()
        .trim()
        .to_string()
}

/// Standalone authority for developer commands.
///
/// Construction does not enable a tool by itself; callers must explicitly
/// register [`command_exec_tool`] (or call
/// [`super::register_command_tool`](crate::builtins::register_command_tool)).
pub struct CommandPolicy {
    command_roots: Vec<PathBuf>,
    bound_command_roots: Vec<BoundCommandRoot>,
    grants: RwLock<Vec<CommandGrantInfo>>,
    mode: CommandMode,
    /// Auto mode chosen at a prompt (`AllowAllForSession`): an `Ask` policy
    /// then skips the prompt like `AutoAllow` does. Never set for `Deny`.
    session_auto: std::sync::atomic::AtomicBool,
    prompter: Option<Arc<dyn CommandPrompter>>,
    limits: CommandLimits,
    artifact_sink: Option<Arc<dyn CommandOutputArtifactSink>>,
    containment_config: CommandContainmentConfig,
    containment_backend: Arc<dyn CommandContainmentBackend>,
    /// Opt-in, explicit environment pass-through on top of the fixed base set
    /// (deny-by-default otherwise: nothing else is inherited from the host).
    /// Resolved and secret-name-checked by the caller (the CLI layer) before
    /// reaching here; see [`env_name_looks_secret`].
    passthrough_environment: Vec<(String, String)>,
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
        let bound_command_roots = roots
            .iter()
            .cloned()
            .map(BoundCommandRoot::bind)
            .collect::<Result<Vec<_>, _>>()?;
        let containment_config = CommandContainmentConfig::default();
        #[cfg(not(test))]
        let containment_backend: Arc<dyn CommandContainmentBackend> =
            default_containment_backend(containment_config.clone());
        #[cfg(test)]
        let containment_backend: Arc<dyn CommandContainmentBackend> =
            Arc::new(DirectTestContainmentBackend);
        Ok(Self {
            command_roots: roots,
            bound_command_roots,
            grants: RwLock::new(Vec::new()),
            mode,
            session_auto: std::sync::atomic::AtomicBool::new(false),
            prompter: None,
            limits: CommandLimits::default(),
            artifact_sink: None,
            containment_backend,
            containment_config,
            passthrough_environment: Vec::new(),
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

    /// Replace the host-owned mandatory containment configuration. This never
    /// disables containment; an unusable configuration is exposed through
    /// [`Self::containment_status`] and every execution fails closed.
    pub fn with_containment_config(mut self, config: CommandContainmentConfig) -> Self {
        self.containment_backend = default_containment_backend(config.clone());
        self.containment_config = config;
        self
    }

    pub fn containment_config(&self) -> &CommandContainmentConfig {
        &self.containment_config
    }

    /// Opt-in, explicit environment variables layered on top of the fixed
    /// non-secret containment base set (`--command-env` at the CLI). This
    /// never widens deny-by-default host inheritance: only exactly these
    /// caller-approved name/value pairs are added, on top of the same fixed
    /// base every contained command already receives. A later entry with the
    /// same name as an earlier one (or a base var) overrides it.
    pub fn with_passthrough_environment(mut self, entries: Vec<(String, String)>) -> Self {
        self.passthrough_environment = entries;
        self
    }

    /// The exact pass-through entries this policy was constructed with (not
    /// including the fixed base set).
    pub fn passthrough_environment(&self) -> &[(String, String)] {
        &self.passthrough_environment
    }

    /// Preflight the mandatory backend once and return a cached, inspectable
    /// status suitable for `/permissions`, `/doctor`, or startup diagnostics.
    pub fn containment_status(&self) -> CommandContainmentStatus {
        match self.containment_backend.preflight() {
            Ok(ready) => CommandContainmentStatus::Ready(CommandContainmentReady {
                backend: DEFAULT_CONTAINMENT_BACKEND,
                backend_path: ready.backend_path,
                writable_command_roots: self.command_roots.clone(),
                readonly_runtime_roots: ready.readonly_runtime_roots,
                environment: merged_containment_environment(&self.passthrough_environment)
                    .into_iter()
                    .map(|(name, value)| {
                        let value = if env_name_looks_secret(&name) {
                            "<redacted>".to_string()
                        } else {
                            value
                        };
                        (name, value)
                    })
                    .collect(),
                passthrough_environment: self
                    .passthrough_environment
                    .iter()
                    .map(|(name, _)| name.clone())
                    .collect(),
                network_isolated: true,
                pid_isolated: true,
                ipc_isolated: true,
                uts_isolated: true,
            }),
            Err(error) => CommandContainmentStatus::Unavailable(error),
        }
    }

    /// Discard the cached probe and inspect the currently configured backend
    /// again. Existing exact command grants are preserved.
    pub fn refresh_containment_status(&self) -> CommandContainmentStatus {
        self.containment_backend.refresh();
        self.containment_status()
    }

    #[cfg(test)]
    fn with_test_containment(mut self) -> Self {
        self.containment_backend = Arc::new(DirectTestContainmentBackend);
        self
    }

    pub fn mode(&self) -> CommandMode {
        self.mode
    }

    /// Whether auto mode was turned on at a prompt this session.
    pub fn session_auto(&self) -> bool {
        self.session_auto.load(std::sync::atomic::Ordering::Acquire)
    }

    /// Turn prompt-chosen auto mode on or off. Only an `Ask` policy can be in
    /// auto mode; for the others this is a no-op and returns `false`.
    pub fn set_session_auto(&self, on: bool) -> bool {
        if self.mode != CommandMode::Ask {
            return false;
        }
        self.session_auto
            .store(on, std::sync::atomic::Ordering::Release);
        true
    }

    pub fn limits(&self) -> CommandLimits {
        self.limits
    }

    /// Describe authority retained by every command under the current policy.
    pub fn ambient_authority(&self) -> CommandAmbientAuthority {
        CommandAmbientAuthority {
            network_confined: !self.containment_config.allow_network,
            ..CommandAmbientAuthority::CURRENT
        }
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
        if executable.fingerprint.is_none() {
            return Err(unresolved_executable(argv, &cwd));
        }
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
        if executable.fingerprint.is_none() {
            return false;
        }
        self.match_grant(argv, &cwd, &executable) == GrantMatch::Current
    }

    fn resolve_cwd(&self, requested: &Path) -> Result<PathBuf, ToolError> {
        // Models routinely echo kitsu's own tilde-elided display paths back
        // as cwd (cycle-18: the banner taught `~/…` and the denial taught
        // nothing). Expand a leading `~` against $HOME before resolution.
        let expanded;
        let requested = match requested.strip_prefix("~") {
            Ok(rest) => match crate::permissions::host_home_dir() {
                Some(home) if rest.as_os_str().is_empty() => {
                    expanded = PathBuf::from(home);
                    expanded.as_path()
                }
                Some(home) => {
                    expanded = PathBuf::from(home).join(rest);
                    expanded.as_path()
                }
                None => requested,
            },
            Err(_) => requested,
        };
        // Inaccessible/non-directory cwds are ENVIRONMENT errors, not
        // authority refusals: report them as recoverable execution errors so
        // one typo'd path doesn't hard-fail a whole scripted turn the model
        // then corrects (cycle-18 gap #3). Only the roots check below is a
        // genuine policy denial.
        let canonical = std::fs::canonicalize(requested).map_err(|error| {
            ToolError::ExecError(format!(
                "command cwd '{}' is not accessible: {error}",
                requested.display()
            ))
        })?;
        if !canonical.is_dir() {
            return Err(ToolError::ExecError(format!(
                "command cwd '{}' is not a directory",
                requested.display()
            )));
        }
        if !self
            .command_roots
            .iter()
            .any(|root| canonical.starts_with(root))
        {
            let approved = self
                .command_roots
                .iter()
                .map(|root| format!("'{}'", root.display()))
                .collect::<Vec<_>>()
                .join(", ");
            return Err(ToolError::PolicyDenied(format!(
                "command cwd '{}' is outside the approved command roots ({approved})",
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
        if executable.fingerprint.is_none() {
            return Err(unresolved_executable(argv, &cwd));
        }
        let grant_match = self.match_grant(argv, &cwd, &executable);
        if grant_match == GrantMatch::Current {
            return Ok(AuthorizedCommand {
                cwd: authorized_cwd,
                executable,
            });
        }

        match self.mode {
            // The dangerous bypass answers the approval question, nothing
            // else: cwd/root confinement and executable resolution were
            // already enforced above, and containment still wraps execution.
            // `--dangerously-skip-permissions` is a whole-run decision and so
            // bypasses every route; prompt-chosen auto mode (`session_auto`)
            // is scoped to the foreground route below, never background work.
            CommandMode::AutoAllow => Ok(AuthorizedCommand {
                cwd: authorized_cwd,
                executable,
            }),
            CommandMode::Deny => {
                if grant_match == GrantMatch::Invalidated {
                    Err(executable_changed(argv, &cwd))
                } else {
                    Err(unapproved(argv, &cwd))
                }
            }
            CommandMode::Ask => {
                let route = crate::prompt_route();
                let request = CommandApprovalRequest {
                    argv: argv.to_vec(),
                    cwd: cwd.display().to_string(),
                    timeout_ms: duration_millis(timeout),
                    executable: executable.identity.clone(),
                    ambient_authority: self.ambient_authority(),
                };
                // Same question, same answer vocabulary, same grant code as
                // the foreground gate: `Decision::AllowAlways` becomes exactly
                // the existing `AllowForSession` exact argv/cwd grant.
                let decision = match route {
                    crate::PromptRoute::Denied => {
                        return Err(ToolError::PolicyDenied(format!(
                            "background command {} in '{}' requires interactive approval; pre-grant the exact argv/cwd before starting background work",
                            display_argv(argv),
                            cwd.display()
                        )));
                    }
                    // Prompt-chosen auto mode skips the question for the rest
                    // of the session, but only here, where a human chose it:
                    // the exact same bypass for a background or subagent
                    // command would silently widen an answer the user gave to
                    // one foreground prompt into authority they never granted.
                    crate::PromptRoute::Foreground if self.session_auto() => {
                        CommandDecision::AllowOnce
                    }
                    crate::PromptRoute::Foreground => self.prompt(request).await,
                    crate::PromptRoute::Registry { agent_id, sender } => match crate::ask_registry(
                        agent_id,
                        &sender,
                        crate::PromptRequestPayload::Command(request),
                    )
                    .await
                    {
                        crate::Decision::AllowOnce => CommandDecision::AllowOnce,
                        crate::Decision::AllowAlways => CommandDecision::AllowForSession,
                        crate::Decision::Deny => CommandDecision::Deny,
                        crate::Decision::Interrupt => CommandDecision::Interrupt,
                    },
                };
                match decision {
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
                    CommandDecision::AllowAllForSession => {
                        self.set_session_auto(true);
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
                    CommandDecision::Interrupt => Err(ToolError::Interrupted(
                        ToolInterruption::AuthorityInputClosed,
                    )),
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
                    .unwrap_or(CommandDecision::Interrupt)
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
    #[cfg(windows)]
    {
        // Rust's Windows process launcher applies platform-specific executable
        // resolution which is not equivalent to canonicalizing one apparent
        // path. Until the policy can reproduce that selection and validate a
        // loader-accepted image, a fingerprint would be false authority.
        let _ = candidate;
        None
    }

    #[cfg(not(windows))]
    {
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
    #[cfg(windows)]
    {
        ToolError::PolicyDenied(format!(
            "the executable identity for command {} in '{}' could not be resolved securely on Windows; loader-equivalent extension and executable validation is unavailable, so Kitsu is refusing unbound execution",
            display_argv(argv),
            cwd.display()
        ))
    }

    #[cfg(not(windows))]
    {
        ToolError::PolicyDenied(format!(
            "the executable identity for command {} in '{}' could not be resolved; refusing unbound execution",
            display_argv(argv),
            cwd.display()
        ))
    }
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
    /// Artifact failures and cancellation cleanup status are reported without
    /// discarding the command's bounded output metadata.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact_error: Option<String>,
    /// Host-authored diagnosis appended when a contained command exits nonzero
    /// with output matching a missing-module/missing-command shape — a failure
    /// that on pip-user/npm-style toolchains is usually caused by the mandatory
    /// containment environment (fixed env allowlist, no host home mount), not
    /// by the project. Names the concrete remedies (`--command-env`,
    /// `--command-root`) so the model can self-correct instead of giving up.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub containment_hint: Option<String>,
}

/// Live, bounded command progress observer used by frontends to retain truthful
/// metadata when their outer agent future is cancelled. Snapshots always use
/// `termination=cancelled` until the ordinary command result returns; they
/// contain only the same bounded previews exposed by [`CommandOutput`].
#[derive(Clone)]
pub struct CommandProgressSink(Arc<dyn Fn(CommandOutput) + Send + Sync + 'static>);

impl CommandProgressSink {
    pub fn new<F>(observer: F) -> Self
    where
        F: Fn(CommandOutput) + Send + Sync + 'static,
    {
        Self(Arc::new(observer))
    }

    fn record(&self, snapshot: CommandOutput) {
        (self.0)(snapshot);
    }
}

impl std::fmt::Debug for CommandProgressSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CommandProgressSink")
            .finish_non_exhaustive()
    }
}

tokio::task_local! {
    static COMMAND_PROGRESS_SINK: CommandProgressSink;
}

/// Scope one tool future to a bounded live command observer. Non-command tools
/// simply never publish a snapshot, so the orchestrator can install this only
/// when the invocation was recognized as `command_exec`.
pub async fn scope_command_progress<F>(sink: CommandProgressSink, future: F) -> F::Output
where
    F: std::future::Future,
{
    COMMAND_PROGRESS_SINK.scope(sink, future).await
}

fn current_command_progress_sink() -> Option<CommandProgressSink> {
    COMMAND_PROGRESS_SINK.try_with(Clone::clone).ok()
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
    limit_bytes: u64,
    state: Mutex<ArtifactCaptureState>,
}

#[derive(Clone, Default)]
struct ArtifactCaptureState {
    first_error: Option<String>,
    captured_bytes: u64,
    truncated: bool,
}

impl ArtifactCapture {
    fn new(writer: Arc<dyn CommandOutputArtifactWriter>, limit_bytes: usize) -> Self {
        Self {
            writer,
            limit_bytes: u64::try_from(limit_bytes).unwrap_or(u64::MAX),
            state: Mutex::new(ArtifactCaptureState::default()),
        }
    }

    fn write(&self, stream: CommandOutputStream, chunk: &[u8]) {
        let mut state = self.state.lock().unwrap();
        if state.first_error.is_some() {
            return;
        }
        let remaining = self.limit_bytes.saturating_sub(state.captured_bytes);
        let retained = chunk
            .len()
            .min(usize::try_from(remaining).unwrap_or(usize::MAX));
        if retained < chunk.len() {
            state.truncated = true;
        }
        if retained == 0 {
            return;
        }
        match self.writer.write_chunk(stream, &chunk[..retained]) {
            Ok(()) => {
                state.captured_bytes = state
                    .captured_bytes
                    .saturating_add(u64::try_from(retained).unwrap_or(u64::MAX));
            }
            Err(error) => state.first_error = Some(error.to_string()),
        }
    }

    fn complete(
        &self,
        completion: &CommandArtifactCompletion,
    ) -> (Option<CommandOutputArtifact>, Option<String>) {
        let state = self.state.lock().unwrap().clone();
        if let Some(error) = state.first_error {
            // Do not bless a partial artifact with a completed reference. The
            // writer's abort hook owns cleanup.
            let error = match self.writer.abort() {
                Ok(()) => error,
                Err(cleanup) => format!("{error}; artifact cleanup failed: {cleanup}"),
            };
            return (None, Some(error));
        }
        let mut completion = *completion;
        completion.captured_bytes = state.captured_bytes;
        completion.truncated = state.truncated;
        completion.limit_bytes = self.limit_bytes;
        match self.writer.complete(&completion) {
            Ok(mut artifact) => {
                // The sink knows its encoded persistent representation. The
                // command layer additionally caps raw bytes offered to it; if
                // that independent cap fired, retain the stricter status.
                artifact.truncated |= state.truncated;
                artifact.limit_bytes = self.limit_bytes;
                let status = artifact.truncated.then(|| {
                    format!(
                        "command-output artifact reached the {}-byte serialized safety limit; remaining command output was drained but omitted",
                        self.limit_bytes
                    )
                });
                (Some(artifact), status)
            }
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

    fn abort(&self) -> std::io::Result<()> {
        self.writer.abort()
    }
}

async fn capture_bounded<R>(
    mut stream: R,
    captured: Arc<Mutex<BoundedBytes>>,
    artifact: Option<Arc<ArtifactCapture>>,
    output_stream: CommandOutputStream,
    progress: Arc<CommandProgressCapture>,
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
        progress.record();
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

struct CommandProgressCapture {
    sink: Option<CommandProgressSink>,
    argv: Vec<String>,
    cwd: String,
    executable: CommandExecutableIdentity,
    ambient_authority: CommandAmbientAuthority,
    started: Instant,
    stdout: Arc<Mutex<BoundedBytes>>,
    stderr: Arc<Mutex<BoundedBytes>>,
    artifact_error: Mutex<Option<String>>,
}

#[derive(Clone)]
struct CommandCaptureContext {
    artifact: Option<Arc<ArtifactCapture>>,
    stdout: Arc<Mutex<BoundedBytes>>,
    stderr: Arc<Mutex<BoundedBytes>>,
    progress: Arc<CommandProgressCapture>,
}

impl CommandProgressCapture {
    fn record(&self) {
        let Some(sink) = &self.sink else {
            return;
        };
        let mut stdout = self.stdout.lock().unwrap().clone();
        let mut stderr = self.stderr.lock().unwrap().clone();
        // This is a live cancellation snapshot, never a claim that EOF was
        // observed. Marking the captures incomplete also makes bounded_text
        // disclose that its retained bytes are only the observed prefix/tail.
        stdout.complete = false;
        stderr.complete = false;
        let stdout = bounded_text(stdout);
        let stderr = bounded_text(stderr);
        sink.record(CommandOutput {
            argv: self.argv.clone(),
            cwd: self.cwd.clone(),
            executable: self.executable.clone(),
            ambient_authority: self.ambient_authority,
            termination: CommandTermination::Cancelled,
            success: false,
            exit_code: None,
            signal: None,
            stdout: stdout.text,
            stderr: stderr.text,
            stdout_bytes: stdout.total_bytes,
            stderr_bytes: stderr.total_bytes,
            stdout_retained_bytes: stdout.retained_bytes,
            stderr_retained_bytes: stderr.retained_bytes,
            stdout_truncated: stdout.truncated,
            stderr_truncated: stderr.truncated,
            stdout_capture_complete: false,
            stderr_capture_complete: false,
            elapsed_ms: duration_millis(self.started.elapsed()),
            artifact: None,
            artifact_error: self.artifact_error.lock().unwrap().clone(),
            containment_hint: None,
        });
    }

    fn append_artifact_status(&self, status: String) {
        let mut artifact_error = self.artifact_error.lock().unwrap();
        match artifact_error.as_mut() {
            Some(existing) => {
                existing.push_str("; ");
                existing.push_str(&status);
            }
            None => *artifact_error = Some(status),
        }
    }
}

/// Publishes one final live snapshot and synchronously invokes the host's
/// artifact abort hook if the command future disappears before returning its
/// ordinary structured result.
struct CommandCancellationGuard {
    progress: Arc<CommandProgressCapture>,
    artifact: Option<Arc<ArtifactCapture>>,
    armed: bool,
}

impl CommandCancellationGuard {
    fn new(progress: Arc<CommandProgressCapture>, artifact: Option<Arc<ArtifactCapture>>) -> Self {
        Self {
            progress,
            artifact,
            armed: true,
        }
    }

    fn disarm(mut self) {
        self.armed = false;
    }
}

impl Drop for CommandCancellationGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let status = match &self.artifact {
            Some(artifact) => match artifact.abort() {
                Ok(()) => {
                    "command cancelled; full-output artifact cleanup requested; sink reported \
                     success, but cleanup was not independently verified"
                        .to_string()
                }
                Err(error) => format!(
                    "command cancelled; full-output artifact cleanup requested; sink reported \
                     failure: {error}"
                ),
            },
            None => {
                "command cancelled; no full-output artifact was created; no cleanup was required"
                    .to_string()
            }
        };
        self.progress.append_artifact_status(status);
        self.progress.record();
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
    shell_signal_status: bool,
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
    capture: CommandCaptureContext,
    shell_signal_status: bool,
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
    let CommandCaptureContext {
        artifact,
        stdout: stdout_capture,
        stderr: stderr_capture,
        progress,
    } = capture;
    let mut reader_tasks = PipeReaderTasks {
        stdout: tokio::spawn(capture_bounded(
            stdout,
            stdout_capture.clone(),
            artifact.clone(),
            CommandOutputStream::Stdout,
            progress.clone(),
        )),
        stderr: tokio::spawn(capture_bounded(
            stderr,
            stderr_capture.clone(),
            artifact,
            CommandOutputStream::Stderr,
            progress,
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
        shell_signal_status,
    })
}

async fn run_supervised_command(
    argv: &[String],
    authorized: &AuthorizedCommand,
    requested_cwd: &Path,
    policy: &CommandPolicy,
    timeout: Duration,
    capture: CommandCaptureContext,
) -> Result<SupervisedOutput, ToolError> {
    // These remain the final pathname checks before spawning the reviewed
    // containment backend. Bubblewrap receives descriptor-bound root/cwd
    // mounts, while the inner executable retains the existing narrow
    // check-to-exec race truthfully described by `pinned=false`.
    authorized.cwd.revalidate(policy, requested_cwd)?;
    for root in &policy.bound_command_roots {
        root.revalidate()?;
    }
    if policy.observe_executable(argv, &authorized.cwd.canonical) != authorized.executable {
        return Err(executable_changed(argv, &authorized.cwd.canonical));
    }
    let launch = policy
        .containment_backend
        .prepare(policy, authorized, argv)
        .map_err(containment_denied)?;
    let mut command = tokio::process::Command::new(&launch.program);
    command
        .args(&launch.arguments)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    // The full contained environment: base + runtime-root toolchain wiring +
    // --command-env pass-through. Bubblewrap ignores this outer env (its own
    // `--clearenv`/`--setenv` governs the child), but Seatbelt and the direct
    // test backend `execve` the target in place, so they inherit it — and
    // without the runtime-root wiring here a Seatbelt child would lose the
    // cargo/rustup PATH additions (Seam 1). Applying it unconditionally is
    // correct for all three: bwrap wipes it, the others need it.
    let launch_environment = runtime_plus_passthrough_env(
        policy.containment_config.readonly_runtime_roots(),
        &policy.passthrough_environment,
    );
    for (name, value) in merged_containment_environment(&launch_environment) {
        command.env(name, value);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;

        command.process_group(0);
        if let Some(argv0) = &launch.argv0 {
            command.as_std_mut().arg0(argv0);
        }
        let inherited_paths = launch.inherited_paths;
        // SAFETY: `fcntl` is async-signal-safe. Clearing FD_CLOEXEC is required
        // only for descriptor arguments consumed by bubblewrap during setup;
        // bubblewrap closes them before executing the contained command.
        unsafe {
            command.pre_exec(move || {
                for path in &inherited_paths {
                    let descriptor = path.as_raw_fd();
                    let flags = libc::fcntl(descriptor, libc::F_GETFD);
                    if flags == -1
                        || libc::fcntl(descriptor, libc::F_SETFD, flags & !libc::FD_CLOEXEC) == -1
                    {
                        return Err(std::io::Error::last_os_error());
                    }
                }
                Ok(())
            });
        }
    }
    // Run the contained command in its authorized cwd. Bubblewrap leaves this
    // None (it --chdir's inside its own argv); Seatbelt and the direct test
    // backend set it, since neither re-roots the child any other way. The path
    // is the canonical authorized cwd, never a model-chosen value.
    if let Some(cwd) = &launch.working_directory {
        command.current_dir(cwd);
    }
    let shell_signal_status = launch.shell_signal_status;
    let child = command.spawn().map_err(command_failed)?;
    supervise_child(child, timeout, capture, shell_signal_status)
        .await
        .map_err(command_failed)
}

fn command_failed(error: std::io::Error) -> ToolError {
    // F135: `ETXTBSY` at `execve` means the executable's inode is open for
    // writing somewhere right now, which is definitionally a passing
    // condition. Name it as transient, additively -- the OS error text is
    // never replaced (F131).
    //
    // The natural discriminator is `std::io::ErrorKind::ExecutableFileBusy`,
    // but it is stable only since Rust 1.83 while this workspace declares
    // `rust-version = "1.80"`, so `clippy::incompatible_msrv` rejects it --
    // correctly, because the code really would not compile on 1.80. Match the
    // named POSIX errno instead, and switch to the error kind once the MSRV
    // rises past 1.83.
    if error.raw_os_error() == Some(libc::ETXTBSY) {
        return ToolError::ExecError(format!(
            "command failed: {error}; this is transient -- the executable is open for \
             writing right now, so retry once the writer has finished"
        ));
    }
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
    // Containment is independent of approval and checked first so an unusable
    // host never asks a person to approve a command it cannot run safely.
    policy
        .containment_backend
        .preflight()
        .map_err(containment_denied)?;
    let authorized = policy
        .authorize(&args.argv, Path::new(&args.cwd), timeout)
        .await?;
    let executable = authorized.executable.identity.clone();
    let artifact_request = CommandArtifactRequest {
        argv: args.argv.clone(),
        cwd: authorized.cwd.canonical.display().to_string(),
        timeout_ms: duration_millis(timeout),
        executable: executable.clone(),
        limit_bytes: u64::try_from(limits.max_artifact_bytes).unwrap_or(u64::MAX),
    };
    let (artifact, artifact_begin_error) = match policy.artifact_sink.as_ref() {
        Some(sink) => match sink.begin(&artifact_request) {
            Ok(writer) => (
                Some(Arc::new(ArtifactCapture::new(
                    writer,
                    limits.max_artifact_bytes,
                ))),
                None,
            ),
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
    let stdout_capture = Arc::new(Mutex::new(BoundedBytes::new(limits.max_output_bytes)));
    let stderr_capture = Arc::new(Mutex::new(BoundedBytes::new(limits.max_output_bytes)));
    let progress = Arc::new(CommandProgressCapture {
        sink: current_command_progress_sink(),
        argv: args.argv.clone(),
        cwd: authorized.cwd.canonical.display().to_string(),
        executable: executable.clone(),
        ambient_authority: policy.ambient_authority(),
        started,
        stdout: stdout_capture.clone(),
        stderr: stderr_capture.clone(),
        artifact_error: Mutex::new(artifact_begin_error.clone()),
    });
    let cancellation_guard = CommandCancellationGuard::new(progress.clone(), artifact.clone());
    progress.record();
    let capture = CommandCaptureContext {
        artifact: artifact.clone(),
        stdout: stdout_capture,
        stderr: stderr_capture,
        progress,
    };
    let captured = run_supervised_command(
        &args.argv,
        &authorized,
        Path::new(&args.cwd),
        policy,
        timeout,
        capture,
    )
    .await?;
    let elapsed_ms = duration_millis(started.elapsed());
    let stdout = bounded_text(captured.stdout);
    let stderr = bounded_text(captured.stderr);

    let shell_signal_status = captured.shell_signal_status;
    let (termination, success, exit_code, signal) = match captured.termination {
        SupervisedTermination::Natural(status) => {
            if shell_signal_status {
                if let Some(code) = status.code().filter(|code| (129..=255).contains(code)) {
                    let signal = code - 128;
                    (CommandTermination::Signaled, false, None, Some(signal))
                } else {
                    (
                        CommandTermination::Exited,
                        status.success(),
                        status.code(),
                        None,
                    )
                }
            } else {
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
        captured_bytes: 0,
        truncated: false,
        limit_bytes: u64::try_from(limits.max_artifact_bytes).unwrap_or(u64::MAX),
    };
    let (output_artifact, artifact_completion_error) = artifact
        .map(|artifact| artifact.complete(&completion))
        .unwrap_or((None, None));
    let artifact_error = artifact_begin_error.or(artifact_completion_error);
    cancellation_guard.disarm();
    let containment_hint =
        containment_failure_hint(termination, success, &stdout.text, &stderr.text);

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
        containment_hint,
    })
}

/// Toolchain-failure shapes that, inside mandatory containment, usually mean
/// the sandbox environment (fixed env allowlist, unmounted host home) rather
/// than the project is at fault. Matched against the bounded output previews
/// of a nonzero natural exit only.
const CONTAINMENT_HINT_SHAPES: [&str; 5] = [
    "No module named",
    "ModuleNotFoundError",
    "command not found",
    ": not found",
    "Cannot find module",
];

/// Diagnose a contained command failure that matches a missing-module or
/// missing-command shape. The hint is host-authored, bounded, and names the
/// concrete remedies, mirroring the outside-root executable denial's style.
/// It reaches both the human receipt and the model-visible tool result via
/// [`CommandOutput::containment_hint`].
fn containment_failure_hint(
    termination: CommandTermination,
    success: bool,
    stdout: &str,
    stderr: &str,
) -> Option<String> {
    if success || termination != CommandTermination::Exited {
        return None;
    }
    let shape = CONTAINMENT_HINT_SHAPES
        .iter()
        .find(|shape| stderr.contains(*shape) || stdout.contains(*shape))?;
    Some(format!(
        "output matches '{shape}' — this is often the containment environment, not the \
         project: contained commands run with a fixed non-secret environment and no host \
         home directory, so user-level installs (e.g. pip's ~/.local user-site) are \
         invisible even when the same command works on the host. If it works outside \
         kitsu, pass the interpreter's search path with `--command-env` (e.g. \
         `--command-env PYTHONPATH=<host site-packages dir>`) and/or expose the \
         toolchain's directory with `--command-root <dir>`"
    ))
}

/// Build the advertised `command_exec` tool bound to an explicit command
/// authority. Registration is a separate opt-in step.
pub fn command_exec_tool(policy: Arc<CommandPolicy>) -> (ToolDescriptor, HandlerFn) {
    let limits = policy.limits();
    let descriptor = ToolDescriptor {
        name: "command_exec".to_string(),
        // The network clause tracks the policy so the model is not told the
        // network is sealed when `--allow-network` lets a granted command
        // reach it — the stale "isolated" wording is what makes a model refuse
        // to even attempt ssh/curl.
        description: format!(
            "Run one explicitly approved developer command as a literal argv array. \
             Kitsu never parses a shell command string. cwd is required and must be \
             inside an approved command root. Exact approval is separate from mandatory \
             OS containment: writable access is limited to approved command roots, \
             runtime mounts are read-only, {network_clause}, \
             and the environment is cleared to a fixed non-secret allowlist. Missing or \
             unsupported containment fails closed. Session approval binds the observed \
             executable filesystem identity and is revoked if it changes. Returns \
             bounded head/tail stdout/stderr previews, truthful byte \
             counts, and structured exit, signal, or timeout metadata.",
            network_clause = if policy.ambient_authority().network_confined {
                "network/PID/IPC/UTS namespaces are isolated"
            } else {
                "PID/IPC/UTS namespaces are isolated but the network is reachable \
                 (--allow-network): you CAN run ssh/curl/git over the network, and \
                 ~/.ssh/known_hosts and ~/.ssh/config are readable (private keys are not)"
            },
        )
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

/// Git-specific entry point backed by the same exact command approvals and
/// containment as command_exec. It is registered only with command capability.
pub fn git_tool(policy: Arc<CommandPolicy>) -> (ToolDescriptor, HandlerFn) {
    let (mut descriptor, _) = command_exec_tool(policy.clone());
    descriptor.name = "git".to_string();
    descriptor.description = "Run a local Git operation through the guarded command runner. \
        Supply literal arguments after git, for example [\"status\",\"--short\"] or \
        [\"switch\",\"-c\",\"my-branch\"], and an explicit cwd. Exact argv/cwd approval \
        and mandatory OS containment apply, including to writes and hooks. No shell \
        string is parsed and network access is unavailable inside containment."
        .to_string();
    let properties = descriptor.parameters["properties"].as_object_mut().unwrap();
    let mut arguments = properties.remove("argv").unwrap();
    arguments["description"] =
        serde_json::json!("Literal Git arguments, excluding the git executable");
    properties.insert("args".to_string(), arguments);
    descriptor.parameters["required"] = serde_json::json!(["args", "cwd"]);
    let handler: HandlerFn = Box::new(move |raw_args: String| {
        let policy = policy.clone();
        Box::pin(async move {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct GitArgs {
                args: Vec<String>,
                cwd: String,
                timeout_ms: Option<u64>,
            }
            let args: GitArgs = serde_json::from_str(&raw_args)
                .map_err(|error| ToolError::InvalidArgs(error.to_string()))?;
            if args.args.is_empty() {
                return Err(ToolError::InvalidArgs(
                    "Git arguments must not be empty".to_string(),
                ));
            }
            let argv = std::iter::once("git".to_string())
                .chain(args.args)
                .collect();
            let output = execute_command(
                &policy,
                CommandExecArgs {
                    argv,
                    cwd: args.cwd,
                    timeout_ms: args.timeout_ms,
                },
            )
            .await?;
            serde_json::to_string(&output)
                .map_err(|error| ToolError::ExecError(format!("serialize Git result: {error}")))
        })
    });
    (descriptor, handler)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn git_tool_requires_the_same_exact_approval_as_command_exec() {
        let dir = tempfile::tempdir().unwrap();
        let policy = Arc::new(CommandPolicy::new(&[root(&dir)], CommandMode::Deny).unwrap());
        let (descriptor, handler) = git_tool(policy);
        assert_eq!(descriptor.name, "git");
        assert!(descriptor.parameters["properties"].get("args").is_some());
        assert!(descriptor.parameters["properties"].get("argv").is_none());
        let error = handler(
            serde_json::json!({
                "args": ["switch", "-c", "must-not-exist"],
                "cwd": dir.path(),
            })
            .to_string(),
        )
        .await
        .unwrap_err();
        assert!(matches!(error, ToolError::PolicyDenied(_)), "{error}");
        assert!(!dir.path().join(".git").exists());
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn git_tool_creates_a_branch_only_after_exact_argv_approval() {
        let dir = tempfile::tempdir().unwrap();
        let policy = Arc::new(CommandPolicy::new(&[root(&dir)], CommandMode::Deny).unwrap());
        if !matches!(
            policy.containment_status(),
            CommandContainmentStatus::Ready(_)
        ) {
            eprintln!(
                "skipping live Git containment: {:?}",
                policy.containment_status()
            );
            return;
        }
        let (_, handler) = git_tool(policy.clone());
        for git_args in [
            vec!["init", "--quiet"],
            vec!["switch", "-c", "review-branch"],
        ] {
            let argv: Vec<String> = std::iter::once("git")
                .chain(git_args.iter().copied())
                .map(str::to_string)
                .collect();
            policy.grant_command(&argv, dir.path()).unwrap();
            let output: CommandOutput = serde_json::from_str(
                &handler(
                    serde_json::json!({
                        "args": git_args, "cwd": dir.path(),
                    })
                    .to_string(),
                )
                .await
                .unwrap(),
            )
            .unwrap();
            assert_eq!(output.exit_code, Some(0), "{}", output.stderr);
        }
        let head = std::process::Command::new("git")
            .args(["symbolic-ref", "--short", "HEAD"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        assert!(head.status.success());
        assert_eq!(
            String::from_utf8(head.stdout).unwrap().trim(),
            "review-branch"
        );
    }

    #[tokio::test]
    async fn git_tool_rejects_empty_arguments_and_unknown_fields() {
        let dir = tempfile::tempdir().unwrap();
        let policy = Arc::new(CommandPolicy::new(&[root(&dir)], CommandMode::Deny).unwrap());
        let (_, handler) = git_tool(policy);
        for input in [
            serde_json::json!({"args": [], "cwd": dir.path()}),
            serde_json::json!({"args": ["status"], "cwd": dir.path(), "shell": "sh"}),
        ] {
            assert!(matches!(
                handler(input.to_string()).await,
                Err(ToolError::InvalidArgs(_))
            ));
        }
    }

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
        aborted: AtomicUsize,
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
                captured_bytes: completion.captured_bytes,
                truncated: completion.truncated,
                limit_bytes: completion.limit_bytes,
                artifact_bytes: completion.captured_bytes,
            })
        }

        fn abort(&self) -> std::io::Result<()> {
            self.aborted.fetch_add(1, Ordering::Relaxed);
            Ok(())
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

    #[cfg(not(windows))]
    #[tokio::test]
    async fn command_root_does_not_itself_grant_execution() {
        let dir = tempfile::tempdir().unwrap();
        let policy = CommandPolicy::new(&[root(&dir)], CommandMode::Deny).unwrap();
        let invocation = args(&["/bin/echo", "definitely-not-run"], dir.path());

        assert!(matches!(
            execute_command(&policy, invocation).await,
            Err(ToolError::PolicyDenied(message)) if message.contains("has not been approved")
        ));
    }

    #[cfg(not(windows))]
    #[tokio::test]
    async fn auto_allow_executes_inside_roots_without_any_prompter() {
        let dir = tempfile::tempdir().unwrap();
        // No prompter attached: AutoAllow must never consult one.
        let policy = CommandPolicy::new(&[root(&dir)], CommandMode::AutoAllow).unwrap();
        let output = execute_command(&policy, args(&["/bin/echo", "auto-allowed"], dir.path()))
            .await
            .unwrap();
        assert!(output.stdout.contains("auto-allowed"));
    }

    #[tokio::test]
    async fn auto_allow_still_denies_cwd_outside_command_roots() {
        let allowed = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let policy = CommandPolicy::new(&[root(&allowed)], CommandMode::AutoAllow).unwrap();
        // The dangerous bypass skips approval prompts only — root confinement
        // is not negotiable.
        assert!(matches!(
            execute_command(&policy, args(&["definitely-not-run"], outside.path())).await,
            Err(ToolError::PolicyDenied(message)) if message.contains("outside the approved command roots")
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

    #[cfg(not(windows))]
    #[tokio::test]
    async fn closed_authority_input_interrupts_before_command_execution() {
        let allowed = tempfile::tempdir().unwrap();
        let target = allowed.path().join("must-not-exist.txt");
        let prompts = Arc::new(AtomicUsize::new(0));
        let policy = CommandPolicy::new(&[root(&allowed)], CommandMode::Ask)
            .unwrap()
            .with_prompter(Arc::new(FixedPrompter {
                decision: CommandDecision::Interrupt,
                prompts: prompts.clone(),
            }));
        let target_arg = target.to_string_lossy().into_owned();
        let touch = if cfg!(target_os = "macos") {
            "/usr/bin/touch"
        } else {
            "/bin/touch"
        };

        let result =
            execute_command(&policy, args(&[touch, target_arg.as_str()], allowed.path())).await;
        assert!(
            matches!(
                &result,
                Err(ToolError::Interrupted(
                    ToolInterruption::AuthorityInputClosed
                ))
            ),
            "expected closed authority input, got {result:?}"
        );
        assert_eq!(prompts.load(Ordering::Relaxed), 1);
        assert!(!target.exists(), "interrupted command was executed");
    }

    /// Cycle-18 command-path fixes: `~/…` cwds expand against $HOME instead
    /// of denying (kitsu's own banner teaches tilde paths); the
    /// outside-roots denial names the approved roots; and an inaccessible
    /// cwd is a recoverable ExecError, not a policy denial.
    #[tokio::test]
    async fn cwd_tilde_expands_missing_is_recoverable_and_denials_name_roots() {
        let allowed = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let policy = CommandPolicy::new(&[root(&allowed)], CommandMode::Ask)
            .unwrap()
            .with_prompter(Arc::new(FixedPrompter {
                decision: CommandDecision::AllowOnce,
                prompts: Arc::new(AtomicUsize::new(0)),
            }));

        // The outside-roots denial teaches the approved roots.
        let denial = execute_command(&policy, args(&["definitely-not-run"], outside.path())).await;
        assert!(matches!(
            denial,
            Err(ToolError::PolicyDenied(message))
                if message.contains("outside the approved command roots")
                    && message.contains(&allowed.path().canonicalize().unwrap().display().to_string())
        ));

        // A missing cwd is an environment error the model can recover from.
        let missing = allowed.path().join("nope");
        assert!(matches!(
            execute_command(&policy, args(&["definitely-not-run"], &missing)).await,
            Err(ToolError::ExecError(message)) if message.contains("not accessible")
        ));

        // A tilde cwd resolves against $HOME rather than failing outright:
        // with HOME pointed at the allowed root, `~` lands inside the roots
        // and proceeds past cwd resolution to the (unapproved-command) gate.
        // Serialized via env override only if HOME manipulation is safe here;
        // instead verify via the unexpanded error NOT firing: a `~`-prefixed
        // path must not produce the old "'~/...' is not accessible" shape
        // when HOME/<rest> exists.
        let home = std::env::var_os("HOME").expect("HOME set in test env");
        let marker = std::path::Path::new(&home);
        if marker.is_dir() {
            let outcome = execute_command(
                &policy,
                args(&["definitely-not-run"], std::path::Path::new("~")),
            )
            .await;
            match outcome {
                // HOME is outside the approved root → the roots denial, with
                // the EXPANDED path (no leading tilde) in the message.
                Err(ToolError::PolicyDenied(message)) => {
                    assert!(
                        !message.contains("'~'"),
                        "tilde should have been expanded before resolution: {message}"
                    );
                }
                other => panic!("expected a roots denial for ~, got {other:?}"),
            }
        }
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

    #[cfg(not(windows))]
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

        let first = args(&["/bin/echo", "one"], dir.path());
        let _ = execute_command(&policy, first.clone()).await;
        let _ = execute_command(&policy, first).await;
        let _ = execute_command(&policy, args(&["/bin/echo", "two"], dir.path())).await;
        let _ = execute_command(&policy, args(&["/bin/echo", "one"], &child)).await;

        assert_eq!(prompts.load(Ordering::Relaxed), 3);
    }

    /// `[u]` auto mode: one prompt, then every later command (any argv, any
    /// root cwd) runs without asking, until it is turned off again. A `Deny`
    /// policy can never be put into auto mode.
    #[cfg(not(windows))]
    #[tokio::test]
    async fn auto_mode_from_a_prompt_stops_asking_until_turned_off() {
        let dir = tempfile::tempdir().unwrap();
        let prompts = Arc::new(AtomicUsize::new(0));
        let policy = CommandPolicy::new(&[root(&dir)], CommandMode::Ask)
            .unwrap()
            .with_prompter(Arc::new(FixedPrompter {
                decision: CommandDecision::AllowAllForSession,
                prompts: prompts.clone(),
            }));

        for word in ["one", "two", "three"] {
            let out = execute_command(&policy, args(&["/bin/echo", word], dir.path()))
                .await
                .unwrap();
            assert_eq!(out.stdout.trim(), word);
        }
        assert_eq!(prompts.load(Ordering::Relaxed), 1, "asked once only");
        assert!(policy.session_auto());

        assert!(policy.set_session_auto(false));
        let _ = execute_command(&policy, args(&["/bin/echo", "four"], dir.path())).await;
        assert_eq!(prompts.load(Ordering::Relaxed), 2, "off: asks again");

        let deny = CommandPolicy::new(&[root(&dir)], CommandMode::Deny).unwrap();
        assert!(!deny.set_session_auto(true), "Deny cannot enter auto mode");
        assert!(
            execute_command(&deny, args(&["/bin/echo", "x"], dir.path()))
                .await
                .is_err()
        );
    }

    /// Auto mode is foreground-only: a prompt answer cannot widen into
    /// background or subagent commands. With `session_auto` on, a `Denied`
    /// route still fails closed and a `Registry` route still parks the
    /// question for a human instead of running silently.
    #[cfg(not(windows))]
    #[tokio::test]
    async fn auto_mode_does_not_leak_into_background_or_registry_routes() {
        let dir = tempfile::tempdir().unwrap();
        let prompts = Arc::new(AtomicUsize::new(0));
        let policy = Arc::new(
            CommandPolicy::new(&[root(&dir)], CommandMode::Ask)
                .unwrap()
                .with_prompter(Arc::new(FixedPrompter {
                    decision: CommandDecision::AllowAllForSession,
                    prompts: prompts.clone(),
                })),
        );

        // Turn auto mode on through the foreground prompt, as `[u]` does.
        execute_command(&policy, args(&["/bin/echo", "fg"], dir.path()))
            .await
            .unwrap();
        assert!(policy.session_auto());

        // Background: still fails closed despite auto mode.
        let denied = crate::scope_without_interactive_prompts(execute_command(
            &policy,
            args(&["/bin/echo", "bg"], dir.path()),
        ))
        .await;
        assert!(
            matches!(&denied, Err(ToolError::PolicyDenied(message)) if message.contains("requires interactive approval")),
            "auto mode must not authorize a background command: {denied:?}"
        );

        // Registry: the question is still parked for a human, not auto-run.
        let (sender, seen) = crate::registry_prompt_stub(crate::Decision::AllowOnce);
        let before = prompts.load(Ordering::Relaxed);
        crate::scope_with_registry_prompts(
            7,
            sender,
            execute_command(&policy, args(&["/bin/echo", "agent"], dir.path())),
        )
        .await
        .unwrap();
        assert_eq!(seen.lock().unwrap().len(), 1, "the registry was asked");
        assert_eq!(
            prompts.load(Ordering::Relaxed),
            before,
            "the foreground prompter was not consulted for the registry route"
        );
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

    #[cfg(not(windows))]
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
            args(&["/bin/echo", "unapproved-background-command"], dir.path()),
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

    // T2-S2 — the command gate consults the prompt route. Symmetric with the
    // path gate: same channel, same answer vocabulary, existing grant code.

    #[cfg(not(windows))]
    #[tokio::test]
    async fn registry_route_allow_once_runs_the_command_without_a_session_grant() {
        let dir = tempfile::tempdir().unwrap();
        let prompts = Arc::new(AtomicUsize::new(0));
        let policy = CommandPolicy::new(&[root(&dir)], CommandMode::Ask)
            .unwrap()
            .with_prompter(Arc::new(FixedPrompter {
                decision: CommandDecision::AllowForSession,
                prompts: prompts.clone(),
            }));
        let (sender, seen) = crate::registry_prompt_stub(crate::Decision::AllowOnce);
        let invocation = args(&["/bin/echo", "registry-allow-once"], dir.path());

        let output = crate::scope_with_registry_prompts(
            21,
            sender,
            execute_command(&policy, invocation.clone()),
        )
        .await
        .expect("a registry-routed AllowOnce must run this command");

        assert_eq!(output.stdout.trim_end(), "registry-allow-once");
        assert_eq!(
            prompts.load(Ordering::Relaxed),
            0,
            "a background gate must never touch the interactive command prompter"
        );
        assert!(
            policy.session_grants().is_empty(),
            "allow once must grant nothing durable"
        );
        let requests = seen.lock().unwrap().clone();
        assert_eq!(requests.len(), 1, "exactly one request is parked");
        match &requests[0] {
            crate::PromptRequestPayload::Command(request) => {
                assert_eq!(request.argv, invocation.argv);
                assert_eq!(
                    request.cwd,
                    std::fs::canonicalize(dir.path())
                        .unwrap()
                        .display()
                        .to_string()
                );
                assert!(
                    request.executable.observed_path.is_some(),
                    "the parked request must carry the same bound executable \
                     identity the foreground prompter is shown"
                );
            }
            other => panic!("the command gate must park a Command payload, got {other:?}"),
        }
    }

    #[cfg(not(windows))]
    #[tokio::test]
    async fn registry_route_allow_always_installs_the_exact_argv_cwd_grant() {
        let dir = tempfile::tempdir().unwrap();
        let policy = CommandPolicy::new(&[root(&dir)], CommandMode::Ask).unwrap();
        let (sender, seen) = crate::registry_prompt_stub(crate::Decision::AllowAlways);
        let invocation = args(&["/bin/echo", "registry-allow-always"], dir.path());

        crate::scope_with_registry_prompts(
            22,
            sender.clone(),
            execute_command(&policy, invocation.clone()),
        )
        .await
        .expect("a registry-routed AllowAlways must run this command");

        // The grant must be the *existing* exact argv/cwd session grant, not a
        // broader background-only approval.
        let grants = policy.session_grants();
        assert_eq!(grants.len(), 1, "exactly one exact grant is installed");
        assert_eq!(grants[0].argv, invocation.argv);
        assert_eq!(grants[0].cwd, std::fs::canonicalize(dir.path()).unwrap());
        assert!(
            grants[0].executable.is_some(),
            "the grant must bind the observed executable identity, as the \
             foreground AllowForSession path does"
        );
        assert!(
            !policy.is_granted(&["/bin/echo".to_string()], dir.path()),
            "the grant must stay exact: a different argv is still unapproved"
        );

        // The installed grant satisfies a repeat without a second question, and
        // it is the same grant the foreground session sees.
        crate::scope_with_registry_prompts(
            22,
            sender,
            execute_command(&policy, invocation.clone()),
        )
        .await
        .expect("the exact grant covers a repeat");
        execute_command(&policy, invocation)
            .await
            .expect("the foreground sees the same session grant");
        assert_eq!(
            seen.lock().unwrap().len(),
            1,
            "the installed grant must satisfy later runs without re-asking"
        );
    }

    #[cfg(not(windows))]
    #[tokio::test]
    async fn registry_route_deny_refuses_exactly_like_the_foreground_deny() {
        let dir = tempfile::tempdir().unwrap();
        let prompts = Arc::new(AtomicUsize::new(0));
        let foreground = CommandPolicy::new(&[root(&dir)], CommandMode::Ask)
            .unwrap()
            .with_prompter(Arc::new(FixedPrompter {
                decision: CommandDecision::Deny,
                prompts: prompts.clone(),
            }));
        let invocation = args(&["/bin/echo", "registry-deny"], dir.path());
        let expected = match execute_command(&foreground, invocation.clone()).await {
            Err(ToolError::PolicyDenied(message)) => message,
            other => panic!("foreground Deny must be PolicyDenied, got {other:?}"),
        };

        let policy = CommandPolicy::new(&[root(&dir)], CommandMode::Ask).unwrap();
        let (sender, seen) = crate::registry_prompt_stub(crate::Decision::Deny);
        let error =
            crate::scope_with_registry_prompts(23, sender, execute_command(&policy, invocation))
                .await
                .expect_err("a registry-routed Deny must refuse");

        match error {
            ToolError::PolicyDenied(message) => assert_eq!(
                message, expected,
                "a background denial must be indistinguishable from the foreground denial"
            ),
            other => panic!("expected PolicyDenied, got {other:?}"),
        }
        assert!(policy.session_grants().is_empty());
        assert_eq!(seen.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn unresolved_executable_is_rejected_before_prompt_or_session_grant() {
        let dir = tempfile::tempdir().unwrap();
        let prompts = Arc::new(AtomicUsize::new(0));
        let policy = CommandPolicy::new(&[root(&dir)], CommandMode::Ask)
            .unwrap()
            .with_prompter(Arc::new(FixedPrompter {
                decision: CommandDecision::AllowForSession,
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
        assert_eq!(prompts.load(Ordering::Relaxed), 0);
        assert!(policy.session_grants().is_empty());
        assert!(!policy.is_granted(&["definitely-unresolved-command".to_string()], dir.path()));
    }

    #[cfg(not(windows))]
    #[test]
    fn exact_command_grants_are_inspectable_sorted_and_individually_revocable() {
        let dir = tempfile::tempdir().unwrap();
        let child = dir.path().join("child");
        std::fs::create_dir(&child).unwrap();
        let policy = CommandPolicy::new(&[root(&dir)], CommandMode::Deny).unwrap();
        let alpha = vec!["/bin/echo".to_string(), "alpha".to_string()];
        let zeta = vec!["/bin/echo".to_string(), "zeta".to_string()];
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
        assert_eq!(grants[2].display_argv(), r#""/bin/echo" "alpha""#);

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

    #[test]
    fn exact_pregrant_rejects_an_unresolved_or_non_executable_program() {
        let dir = tempfile::tempdir().unwrap();
        let policy = CommandPolicy::new(&[root(&dir)], CommandMode::Deny).unwrap();

        for program in ["missing-program", "plain-file"] {
            if program == "plain-file" {
                std::fs::write(dir.path().join(program), "not executable").unwrap();
            }
            let argv = vec![format!("./{program}")];
            let error = policy
                .grant_command(&argv, dir.path())
                .expect_err("a dead exact grant must be rejected at configuration time");
            assert!(matches!(
                error,
                ToolError::PolicyDenied(message)
                    if message.contains("could not be resolved")
                        && message.contains("refusing unbound execution")
            ));
        }
        assert!(policy.session_grants().is_empty());
    }

    #[cfg(windows)]
    #[test]
    fn windows_command_authority_fails_closed_without_loader_equivalent_validation() {
        let dir = tempfile::tempdir().unwrap();
        let policy = CommandPolicy::new(&[root(&dir)], CommandMode::Deny).unwrap();
        let executable = std::env::current_exe().unwrap();
        let argv = vec![executable.to_string_lossy().into_owned()];

        assert!(
            fingerprint_executable(&executable).is_none(),
            "even a known executable must not receive an untruthful pathname fingerprint"
        );
        let error = policy
            .grant_command(&argv, dir.path())
            .expect_err("Windows commands must fail closed before a secure resolver exists");
        assert!(matches!(
            error,
            ToolError::PolicyDenied(message)
                if message.contains("resolved securely on Windows")
                    && message.contains("loader-equivalent")
                    && message.contains("refusing unbound execution")
        ));
        assert!(policy.session_grants().is_empty());
    }

    #[cfg(not(windows))]
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
        let invocation = args(&["/bin/echo", "allow-once"], dir.path());

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
            .with_limits(limits)
            .with_test_containment();
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
        let artifact = output.artifact.as_ref().unwrap();
        assert_eq!(artifact.captured_bytes, 18);
        assert!(!artifact.truncated);
        assert_eq!(artifact.limit_bytes, DEFAULT_COMMAND_ARTIFACT_BYTES as u64);
        assert_eq!(sink.requests.lock().unwrap().len(), 1);
        let completion = writer.completion.lock().unwrap().unwrap();
        assert_eq!(completion.stdout_bytes, 9);
        assert_eq!(completion.stderr_bytes, 9);
        assert_eq!(completion.termination, CommandTermination::Exited);
        assert!(completion.stdout_capture_complete);
        assert!(completion.stderr_capture_complete);
        assert_eq!(completion.captured_bytes, 18);
        assert!(!completion.truncated);
        assert_eq!(
            completion.limit_bytes,
            DEFAULT_COMMAND_ARTIFACT_BYTES as u64
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn artifact_cap_omits_excess_while_pipes_continue_to_drain() {
        let dir = tempfile::tempdir().unwrap();
        let argv = [
            "/bin/sh",
            "-c",
            "printf 123456789abc; printf ABCDEFGHIJKL >&2",
        ];
        let limits = CommandLimits::new(Duration::from_secs(1), Duration::from_secs(1), 4)
            .unwrap()
            .with_max_artifact_bytes(10)
            .unwrap();
        let writer = Arc::new(MemoryArtifact::default());
        let sink = Arc::new(MemoryArtifactSink {
            writer: writer.clone(),
            requests: Mutex::new(Vec::new()),
        });
        let policy = CommandPolicy::new(&[root(&dir)], CommandMode::Deny)
            .unwrap()
            .with_limits(limits)
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

        assert_eq!(output.stdout_bytes, 12);
        assert_eq!(output.stderr_bytes, 12);
        assert!(output.stdout_capture_complete);
        assert!(output.stderr_capture_complete);
        let captured = writer.stdout.lock().unwrap().len() + writer.stderr.lock().unwrap().len();
        assert_eq!(captured, 10);
        let artifact = output.artifact.as_ref().unwrap();
        assert_eq!(artifact.captured_bytes, 10);
        assert!(artifact.truncated);
        assert_eq!(artifact.limit_bytes, 10);
        assert!(output.artifact_error.as_deref().is_some_and(|status| {
            status.contains("10-byte serialized safety limit")
                && status.contains("drained but omitted")
        }));
        let completion = writer.completion.lock().unwrap().unwrap();
        assert_eq!(completion.captured_bytes, 10);
        assert!(completion.truncated);
        assert_eq!(completion.limit_bytes, 10);
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
    fn authority_api_truthfully_labels_contained_execution() {
        let dir = tempfile::tempdir().unwrap();
        let policy = CommandPolicy::new(&[root(&dir)], CommandMode::Deny).unwrap();
        let authority = policy.ambient_authority();
        assert!(!authority.inherits_environment);
        assert!(authority.filesystem_confined_to_command_roots);
        assert!(authority.network_confined);
        assert!(!authority.executable_identity_pinned);
        assert!(authority.notice().contains("mandatory OS containment"));

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

    #[test]
    fn secret_like_env_names_are_detected_case_insensitively() {
        for name in [
            "API_KEY",
            "my_token",
            "AWS_SECRET_ACCESS_KEY",
            "DB_PASSWORD",
            "Password",
        ] {
            assert!(env_name_looks_secret(name), "{name} should look secret");
        }
        for name in [
            "PYTHONPATH",
            "NODE_PATH",
            "VIRTUAL_ENV",
            "CARGO_HOME",
            "HOME",
        ] {
            assert!(
                !env_name_looks_secret(name),
                "{name} should not look secret"
            );
        }
    }

    #[test]
    fn runtime_root_environment_wires_toolchain_paths() {
        let dir = tempfile::tempdir().unwrap();
        let cargo = dir.path().join(".cargo");
        std::fs::create_dir_all(cargo.join("bin")).unwrap();
        let rustup = dir.path().join(".rustup");
        std::fs::create_dir_all(&rustup).unwrap();

        let environment = runtime_root_environment(&[cargo.clone(), rustup.clone()]);
        let lookup = |name: &str| {
            environment
                .iter()
                .find(|(entry, _)| entry == name)
                .map(|(_, value)| value.as_str())
        };
        assert_eq!(
            lookup("CARGO_HOME"),
            Some(cargo.display().to_string()).as_deref()
        );
        assert_eq!(
            lookup("RUSTUP_HOME"),
            Some(rustup.display().to_string()).as_deref()
        );
        let path = lookup("PATH").expect("cargo bin joins PATH");
        assert!(
            path.starts_with(&cargo.join("bin").display().to_string()),
            "{path}"
        );
        assert!(path.contains("/usr/bin"), "base PATH retained: {path}");

        // No roots, no adjustments — the fixed environment stands alone.
        assert!(runtime_root_environment(&[]).is_empty());
    }

    #[test]
    fn containment_status_discloses_passthrough_names_but_redacts_secret_shaped_values() {
        let dir = tempfile::tempdir().unwrap();
        let policy = CommandPolicy::new(&[root(&dir)], CommandMode::Deny)
            .unwrap()
            .with_passthrough_environment(vec![
                (
                    "MY_API_KEY".to_string(),
                    "sk-live-should-not-leak".to_string(),
                ),
                ("PYTHONPATH".to_string(), "/some/safe/path".to_string()),
            ]);
        let CommandContainmentStatus::Ready(ready) = policy.containment_status() else {
            panic!("test containment backend must report ready");
        };
        assert_eq!(
            ready.passthrough_environment,
            vec!["MY_API_KEY".to_string(), "PYTHONPATH".to_string()]
        );
        let value_of = |name: &str| {
            ready
                .environment
                .iter()
                .find(|(candidate, _)| candidate == name)
                .map(|(_, value)| value.clone())
        };
        assert_eq!(value_of("MY_API_KEY").as_deref(), Some("<redacted>"));
        assert_eq!(value_of("PYTHONPATH").as_deref(), Some("/some/safe/path"));
        // The fixed base set is untouched and still fully disclosed.
        assert_eq!(value_of("HOME").as_deref(), Some("/tmp/kitsu-home"));
    }

    #[test]
    fn passthrough_environment_overrides_a_same_named_base_var() {
        let merged = merged_containment_environment(&[(
            "HOME".to_string(),
            "/tmp/kitsu-passthrough-home".to_string(),
        )]);
        assert_eq!(merged.len(), COMMAND_CONTAINMENT_ENVIRONMENT.len());
        let home = merged
            .iter()
            .find(|(name, _)| name == "HOME")
            .map(|(_, value)| value.as_str());
        assert_eq!(home, Some("/tmp/kitsu-passthrough-home"));
    }

    // Linux-only: it forces a BubblewrapBackend via a bogus bwrap path and
    // expects BackendMissing. On macOS `with_containment_config` yields a
    // SeatbeltBackend that ignores that path and probes /usr/bin/sandbox-exec,
    // so the premise does not hold. The equivalent macOS failure modes are
    // covered by the Seatbelt profile/preflight tests.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn missing_containment_backend_is_typed_and_denied_before_approval() {
        let dir = tempfile::tempdir().unwrap();
        let prompts = Arc::new(AtomicUsize::new(0));
        let missing = dir.path().join("missing-bwrap");
        let policy = CommandPolicy::new(&[root(&dir)], CommandMode::Ask)
            .unwrap()
            .with_prompter(Arc::new(FixedPrompter {
                decision: CommandDecision::AllowOnce,
                prompts: prompts.clone(),
            }))
            .with_containment_config(
                CommandContainmentConfig::default().with_bubblewrap_path(&missing),
            );

        let status = policy.containment_status();
        let CommandContainmentStatus::Unavailable(error) = status else {
            panic!("missing backend must be unavailable")
        };
        assert_eq!(
            error.reason,
            CommandContainmentUnavailableReason::BackendMissing
        );
        assert!(error.detail.contains(&missing.display().to_string()));

        let result = execute_command(&policy, args(&["/usr/bin/true"], dir.path())).await;
        assert!(matches!(
            result,
            Err(ToolError::PolicyDenied(message))
                if message.contains("mandatory command containment")
                    && message.contains("BackendMissing")
        ));
        assert_eq!(prompts.load(Ordering::Relaxed), 0);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn bubblewrap_plan_clears_environment_and_requests_all_required_namespaces() {
        let arguments = base_bubblewrap_arguments(&[], &[], false)
            .into_iter()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        for required in [
            "--unshare-net",
            "--unshare-pid",
            "--unshare-ipc",
            "--unshare-uts",
            "--clearenv",
            "--disable-userns",
        ] {
            assert!(arguments.iter().any(|argument| argument == required));
        }
        // `--allow-network` drops ONLY the network namespace; every other
        // isolation flag stays.
        let net = base_bubblewrap_arguments(&[], &[], true)
            .into_iter()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert!(
            !net.iter().any(|argument| argument == "--unshare-net"),
            "network namespace dropped under --allow-network"
        );
        for still_isolated in [
            "--unshare-pid",
            "--unshare-ipc",
            "--unshare-uts",
            "--disable-userns",
        ] {
            assert!(net.iter().any(|argument| argument == still_isolated));
        }
        assert!(!arguments
            .iter()
            .any(|argument| argument.contains("TOKEN") || argument.contains("SECRET")));
        let clear = arguments
            .iter()
            .position(|argument| argument == "--clearenv")
            .unwrap();
        let first_set = arguments
            .iter()
            .position(|argument| argument == "--setenv")
            .unwrap();
        assert!(clear < first_set);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn bubblewrap_plan_lets_passthrough_env_override_a_base_var() {
        let arguments = base_bubblewrap_arguments(
            &[],
            &[(
                "HOME".to_string(),
                "/tmp/kitsu-passthrough-home".to_string(),
            )],
            false,
        )
        .into_iter()
        .map(|argument| argument.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
        // Exactly one --setenv HOME survives (the pass-through value), not two
        // conflicting ones.
        let home_positions = arguments
            .iter()
            .enumerate()
            .filter(|(_, argument)| *argument == "HOME")
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        assert_eq!(home_positions.len(), 1);
        assert_eq!(
            arguments[home_positions[0] + 1],
            "/tmp/kitsu-passthrough-home"
        );
    }

    /// Regression for cycle-8 audit gap 1: mandatory containment's fixed
    /// environment breaks interpreter-based toolchains whose module search
    /// path is env-driven (PYTHONPATH/HOME-derived user-site, NODE_PATH,
    /// etc.), *even when every relevant directory is already mounted* as a
    /// command root. Reproduced here with a minimal stand-in for the audit's
    /// `pytest` failure (`ModuleNotFoundError` from a script whose importable
    /// module lives outside the working directory) rather than depending on
    /// a real pytest installation being present on the test host. Confirms
    /// the root cause is the environment, not a missing mount: the "before"
    /// run already has the module's directory bind-mounted as a command root
    /// and still fails; only adding the explicit `--command-env` PYTHONPATH
    /// pass-through fixes it.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn command_env_passthrough_fixes_interpreter_module_resolution() {
        let workdir = tempfile::tempdir().unwrap();
        let moddir = tempfile::tempdir().unwrap();
        std::fs::write(
            moddir.path().join("kitsu_passthrough_probe.py"),
            "VALUE = \"kitsu-passthrough-ok\"\n",
        )
        .unwrap();
        let python3 = Path::new("/usr/bin/python3");
        if !python3.is_file() {
            eprintln!("skipping: /usr/bin/python3 not present on this host");
            return;
        }
        let argv = vec![
            "/usr/bin/python3".to_string(),
            "-c".to_string(),
            "import kitsu_passthrough_probe; print(kitsu_passthrough_probe.VALUE)".to_string(),
        ];

        // Before: the module's directory IS mounted (as a second command
        // root), but nothing tells the interpreter to look there. This is
        // exactly the audit's finding — more roots did not help.
        let policy = CommandPolicy::new(&[root(&workdir), root(&moddir)], CommandMode::Deny)
            .unwrap()
            .with_containment_config(CommandContainmentConfig::default());
        let CommandContainmentStatus::Ready(_) = policy.containment_status() else {
            eprintln!(
                "skipping live containment assertions: {:?}",
                policy.containment_status()
            );
            return;
        };
        policy.grant_command(&argv, workdir.path()).unwrap();
        let before = execute_command(
            &policy,
            CommandExecArgs {
                argv: argv.clone(),
                cwd: root(&workdir),
                timeout_ms: None,
            },
        )
        .await
        .unwrap();
        assert!(!before.success);
        assert!(
            before.stderr.contains("ModuleNotFoundError"),
            "{}",
            before.stderr
        );
        // Cycle-10 G2: the failure shape must carry a host-authored hint that
        // names the likely cause (containment env) and both remedies.
        let hint = before
            .containment_hint
            .as_deref()
            .expect("missing-module failure inside containment must carry a hint");
        assert!(hint.contains("--command-env"), "{hint}");
        assert!(hint.contains("--command-root"), "{hint}");

        // After: the identical grant/mounts, plus one opt-in --command-env
        // PYTHONPATH pass-through naming the already-mounted directory.
        let policy = CommandPolicy::new(&[root(&workdir), root(&moddir)], CommandMode::Deny)
            .unwrap()
            .with_containment_config(CommandContainmentConfig::default())
            .with_passthrough_environment(vec![(
                "PYTHONPATH".to_string(),
                moddir.path().to_string_lossy().into_owned(),
            )]);
        policy.grant_command(&argv, workdir.path()).unwrap();
        let after = execute_command(
            &policy,
            CommandExecArgs {
                argv,
                cwd: root(&workdir),
                timeout_ms: None,
            },
        )
        .await
        .unwrap();
        assert!(after.success, "{}", after.stderr);
        assert_eq!(after.stdout.trim(), "kitsu-passthrough-ok");
        assert!(
            after.containment_hint.is_none(),
            "successful commands must not carry a containment hint"
        );
    }

    /// Cycle-10 G2: the failure-shape matcher fires only on a nonzero natural
    /// exit whose output matches a missing-module/missing-command shape, and
    /// the hint names both remedies. Successes, signals, and unrelated
    /// failures stay hint-free so the receipt is not polluted.
    #[test]
    fn containment_hint_matches_only_missing_toolchain_shapes() {
        let hint = containment_failure_hint(
            CommandTermination::Exited,
            false,
            "",
            "/usr/bin/python3: No module named pytest\n",
        )
        .expect("missing-module stderr must produce a hint");
        assert!(hint.contains("No module named"), "{hint}");
        assert!(hint.contains("--command-env"), "{hint}");
        assert!(hint.contains("--command-root"), "{hint}");

        let shell = containment_failure_hint(
            CommandTermination::Exited,
            false,
            "sh: 1: pytest: not found\n",
            "",
        );
        assert!(shell.is_some(), "shell not-found stdout must also match");

        assert!(
            containment_failure_hint(
                CommandTermination::Exited,
                true,
                "No module named anything",
                ""
            )
            .is_none(),
            "successful exits never hint"
        );
        assert!(
            containment_failure_hint(
                CommandTermination::Signaled,
                false,
                "",
                "No module named pytest"
            )
            .is_none(),
            "signals are not exit-shape failures"
        );
        assert!(
            containment_failure_hint(
                CommandTermination::Exited,
                false,
                "",
                "assert 1 == 2 failed"
            )
            .is_none(),
            "ordinary failures stay hint-free"
        );
    }

    /// Regression for cycle-8 audit gap 2: an approved executable whose own
    /// directory isn't separately visible to the sandbox must be denied with
    /// a message that names the missing directory and the exact flag
    /// (`--command-root <dir>`) that fixes it, not just the bare executable
    /// path.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn executable_outside_visible_roots_names_the_missing_dir_and_the_fix() {
        let cwd_dir = tempfile::tempdir().unwrap();
        let toolchain_dir = tempfile::tempdir().unwrap();
        let probe = toolchain_dir.path().join("kitsu-probe-tool");
        write_test_executable(&probe, "unreachable");
        let argv = vec![probe.to_string_lossy().into_owned()];

        // Only cwd_dir is a command root — toolchain_dir (the executable's
        // own directory) is deliberately withheld, matching the audit's
        // repro of a CI author following the documented pregrant recipe
        // verbatim without also granting the toolchain directory.
        let policy = CommandPolicy::new(&[root(&cwd_dir)], CommandMode::Deny)
            .unwrap()
            .with_containment_config(CommandContainmentConfig::default());
        let CommandContainmentStatus::Ready(_) = policy.containment_status() else {
            eprintln!(
                "skipping live containment assertions: {:?}",
                policy.containment_status()
            );
            return;
        };
        policy.grant_command(&argv, cwd_dir.path()).unwrap();
        let result = execute_command(
            &policy,
            CommandExecArgs {
                argv,
                cwd: root(&cwd_dir),
                timeout_ms: None,
            },
        )
        .await;
        let Err(ToolError::PolicyDenied(message)) = result else {
            panic!("expected PolicyDenied, got {result:?}");
        };
        assert!(
            message.contains(&toolchain_dir.path().display().to_string()),
            "{message}"
        );
        assert!(
            message.contains(&format!(
                "--command-root {}",
                toolchain_dir.path().display()
            )),
            "{message}"
        );
        assert!(
            message.contains("ExecutableOutsideVisibleRoots"),
            "{message}"
        );
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn live_containment_blocks_outside_reads_network_host_processes_and_secrets() {
        let allowed = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let secret_path = outside.path().join("host-secret.txt");
        std::fs::write(&secret_path, "HOST_SECRET_BODY").unwrap();
        let policy = CommandPolicy::new(&[root(&allowed)], CommandMode::Deny)
            .unwrap()
            .with_containment_config(CommandContainmentConfig::default());
        let CommandContainmentStatus::Ready(ready) = policy.containment_status() else {
            eprintln!(
                "skipping live containment assertions: {:?}",
                policy.containment_status()
            );
            return;
        };
        assert!(ready.network_isolated && ready.pid_isolated && ready.ipc_isolated);
        assert!(ready.uts_isolated);
        assert_eq!(
            ready.environment.len(),
            COMMAND_CONTAINMENT_ENVIRONMENT.len()
        );

        let outside_read = vec![
            "/usr/bin/cat".to_string(),
            secret_path.to_string_lossy().into_owned(),
        ];
        policy.grant_command(&outside_read, allowed.path()).unwrap();
        let output = execute_command(
            &policy,
            CommandExecArgs {
                argv: outside_read,
                cwd: root(&allowed),
                timeout_ms: None,
            },
        )
        .await
        .unwrap();
        assert!(!output.success);
        assert!(!output.stdout.contains("HOST_SECRET_BODY"));
        assert!(!output.stderr.contains("HOST_SECRET_BODY"));

        // SAFETY: this unique test-only variable is not read by other tests;
        // production launch uses env_clear and never enumerates the host env.
        unsafe { std::env::set_var("KITSU_CONTAINMENT_TEST_SECRET", "DO_NOT_INHERIT") };
        let environment = vec!["/usr/bin/env".to_string()];
        policy.grant_command(&environment, allowed.path()).unwrap();
        let output = execute_command(
            &policy,
            CommandExecArgs {
                argv: environment,
                cwd: root(&allowed),
                timeout_ms: None,
            },
        )
        .await
        .unwrap();
        // SAFETY: restore the process environment after the scoped assertion.
        unsafe { std::env::remove_var("KITSU_CONTAINMENT_TEST_SECRET") };
        assert!(output.success);
        assert!(!output.stdout.contains("KITSU_CONTAINMENT_TEST_SECRET"));
        assert!(output.stdout.contains("HOME=/tmp/kitsu-home"));

        let network = vec![
            "/usr/bin/python3".to_string(),
            "-c".to_string(),
            "import socket; s=socket.socket(); s.settimeout(.2);\ntry: s.connect(('1.1.1.1',53)); print('reachable')\nexcept OSError: print('isolated')"
                .to_string(),
        ];
        policy.grant_command(&network, allowed.path()).unwrap();
        let output = execute_command(
            &policy,
            CommandExecArgs {
                argv: network,
                cwd: root(&allowed),
                timeout_ms: Some(2_000),
            },
        )
        .await
        .unwrap();
        assert!(output.success, "{}", output.stderr);
        assert_eq!(output.stdout.trim(), "isolated");

        let host_pid = std::process::id().to_string();
        let process = vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            "if [ -e \"/proc/$1\" ]; then printf visible; else printf isolated; fi".to_string(),
            "sh".to_string(),
            host_pid,
        ];
        policy.grant_command(&process, allowed.path()).unwrap();
        let output = execute_command(
            &policy,
            CommandExecArgs {
                argv: process,
                cwd: root(&allowed),
                timeout_ms: None,
            },
        )
        .await
        .unwrap();
        assert!(output.success, "{}", output.stderr);
        assert_eq!(output.stdout, "isolated");
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
                )
                .with_test_containment(),
        );
        policy.grant_command(&argv, dir.path()).unwrap();
        let invocation = CommandExecArgs {
            argv,
            cwd: root(&dir),
            timeout_ms: None,
        };
        let snapshots = Arc::new(Mutex::new(Vec::new()));
        let observed = snapshots.clone();
        let progress = CommandProgressSink::new(move |snapshot| {
            observed.lock().unwrap().push(snapshot);
        });
        let task_policy = policy.clone();
        let task = tokio::spawn(async move {
            scope_command_progress(progress, execute_command(&task_policy, invocation)).await
        });

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

        let snapshot = snapshots.lock().unwrap().last().cloned().unwrap();
        assert!(snapshot.artifact.is_none());
        assert!(snapshot.artifact_error.as_deref().is_some_and(|status| {
            status.contains("no full-output artifact was created")
                && status.contains("no cleanup was required")
        }));

        wait_until_reaped(pid).await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cancellation_snapshot_preserves_output_and_reports_unverified_artifact_cleanup() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("pid-progress");
        let ready_file = dir.path().join("ready-progress");
        let argv = vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            "echo $$ > \"$1\"; printf partial-out; printf partial-err >&2; : > \"$2\"; exec sleep 30"
                .to_string(),
            "sh".to_string(),
            pid_file.to_string_lossy().into_owned(),
            ready_file.to_string_lossy().into_owned(),
        ];
        let writer = Arc::new(MemoryArtifact::default());
        let sink = Arc::new(MemoryArtifactSink {
            writer: writer.clone(),
            requests: Mutex::new(Vec::new()),
        });
        let policy = Arc::new(
            CommandPolicy::new(&[root(&dir)], CommandMode::Deny)
                .unwrap()
                .with_output_artifact_sink(sink)
                .with_test_containment(),
        );
        policy.grant_command(&argv, dir.path()).unwrap();
        let invocation = CommandExecArgs {
            argv,
            cwd: root(&dir),
            timeout_ms: None,
        };
        let snapshots = Arc::new(Mutex::new(Vec::new()));
        let observed = snapshots.clone();
        let progress = CommandProgressSink::new(move |snapshot| {
            observed.lock().unwrap().push(snapshot);
        });
        let task = tokio::spawn(async move {
            scope_command_progress(progress, execute_command(&policy, invocation)).await
        });

        for _ in 0..100 {
            if ready_file.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            ready_file.exists(),
            "command should publish its partial output"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
        let pid = std::fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse::<i32>()
            .unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());

        let snapshot = snapshots.lock().unwrap().last().cloned().unwrap();
        assert_eq!(snapshot.termination, CommandTermination::Cancelled);
        assert!(snapshot.stdout.contains("partial-out"));
        assert!(snapshot.stderr.contains("partial-err"));
        assert!(snapshot.stdout_bytes >= "partial-out".len() as u64);
        assert!(snapshot.stderr_bytes >= "partial-err".len() as u64);
        assert!(snapshot.executable.observed_path.is_some());
        assert!(!snapshot.stdout_capture_complete);
        assert!(!snapshot.stderr_capture_complete);
        assert!(snapshot.artifact.is_none());
        assert!(snapshot.artifact_error.as_deref().is_some_and(|status| {
            status.contains("cleanup requested")
                && status.contains("not independently verified")
                && !status.contains("cleanup completed")
        }));
        assert_eq!(writer.aborted.load(Ordering::Relaxed), 1);
        assert!(writer.completion.lock().unwrap().is_none());
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
                )
                .with_test_containment(),
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

    // The alternatives symlink farm (`/usr/bin/awk -> /etc/alternatives/awk ->
    // /usr/bin/mawk`) only resolves inside the sandbox when `/etc/alternatives`
    // is mounted alongside `/usr`. Assert the invariant against the real host:
    // wherever the farm exists, the default containment must mount it. On hosts
    // without it (the directory is absent), the check is vacuous by design,
    // matching the function's own `.exists()` guard.
    #[cfg(target_os = "linux")]
    #[test]
    fn default_containment_mounts_alternatives_farm_when_present() {
        let mounts = runtime_mount_paths(&CommandContainmentConfig::default()).unwrap();
        let farm = PathBuf::from("/etc/alternatives");
        if farm.exists() {
            assert!(
                mounts.contains(&farm),
                "/etc/alternatives exists on host but was not mounted into the sandbox; \
                 alternatives-managed tools (awk, editor, pager) would fail to resolve"
            );
        }
        // `/usr` is unconditionally mounted where present, so the farm can never
        // be the *only* mount — a sanity check that the base set is intact.
        if PathBuf::from("/usr").exists() {
            assert!(mounts.contains(&PathBuf::from("/usr")));
        }
    }

    /// Exercise the complete public-name -> alternatives farm -> implementation
    /// chain inside real containment. The mount-list assertion above catches
    /// accidental omission, while this regression catches a mount that is
    /// present but unusable by the sandboxed process.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn live_containment_resolves_an_alternatives_managed_executable() {
        let awk = Path::new("/usr/bin/awk");
        let Ok(link_target) = std::fs::read_link(awk) else {
            eprintln!("skipping: /usr/bin/awk is not a symlink on this host");
            return;
        };
        if !link_target.to_string_lossy().contains("etc/alternatives") {
            eprintln!("skipping: /usr/bin/awk is not managed by /etc/alternatives on this host");
            return;
        }

        let workdir = tempfile::tempdir().unwrap();
        let policy = CommandPolicy::new(&[root(&workdir)], CommandMode::Deny)
            .unwrap()
            .with_containment_config(CommandContainmentConfig::default());
        let CommandContainmentStatus::Ready(_) = policy.containment_status() else {
            eprintln!(
                "skipping live containment assertions: {:?}",
                policy.containment_status()
            );
            return;
        };
        let argv = vec![
            awk.display().to_string(),
            "BEGIN { print \"kitsu-alternatives-ok\" }".to_string(),
        ];
        policy.grant_command(&argv, workdir.path()).unwrap();

        let output = execute_command(
            &policy,
            CommandExecArgs {
                argv,
                cwd: root(&workdir),
                timeout_ms: None,
            },
        )
        .await
        .unwrap();

        assert!(output.success, "stderr: {}", output.stderr);
        assert_eq!(output.exit_code, Some(0));
        assert_eq!(output.stdout, "kitsu-alternatives-ok\n");
    }

    /// Writes a `#!/bin/sh` script, makes it executable, and returns the write
    /// descriptor still open. Linux's `deny_write_access()` consults
    /// `inode->i_writecount`, which is process-independent, so every `execve`
    /// of this inode fails with `ETXTBSY` for as long as the returned handle
    /// lives. That makes the F134 race a deterministic fixture: no sleeps, no
    /// sibling forks and no wall-clock assertions are involved.
    #[cfg(target_os = "linux")]
    fn write_test_executable_keeping_write_descriptor(path: &Path, output: &str) -> std::fs::File {
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;

        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)
            .unwrap();
        file.write_all(format!("#!/bin/sh\nprintf '%s' '{output}'\n").as_bytes())
            .unwrap();
        file.flush().unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
        file
    }

    /// F135: `ETXTBSY` at `execve` means "this inode is open for writing by
    /// somebody" and is definitionally transient. The failure a person reads
    /// must say so, and must still carry the underlying OS error.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn etxtbsy_at_spawn_is_reported_as_a_transient_condition() {
        let dir = tempfile::tempdir().unwrap();
        let executable = dir.path().join("busy-tool");
        let writer = write_test_executable_keeping_write_descriptor(&executable, "never-runs");
        let invocation = executable.to_string_lossy().into_owned();
        let policy = CommandPolicy::new(&[root(&dir)], CommandMode::AutoAllow).unwrap();

        let error = execute_command(&policy, args(&[invocation.as_str()], dir.path()))
            .await
            .expect_err("a write descriptor held on the executable must fail the spawn");
        drop(writer);

        let ToolError::ExecError(message) = error else {
            panic!("ETXTBSY at spawn must surface as an ExecError: {error:?}");
        };
        // The OS error survives: F131's rule is that a label never replaces the
        // errno text.
        assert!(
            message.contains("os error 26"),
            "the underlying ETXTBSY must still be reported: {message}"
        );
        let lowercase = message.to_lowercase();
        assert!(
            lowercase.contains("transient"),
            "the message must name the condition as transient: {message}"
        );
        assert!(
            lowercase.contains("retry") || lowercase.contains("again"),
            "the message must tell the person that retrying can succeed: {message}"
        );
    }

    /// Non-vacuity for the fixture above, in both directions: the spawn fails
    /// with `ETXTBSY` precisely while a write descriptor is held, and the very
    /// same executable at the very same path runs once it is closed. Nothing
    /// here waits on the clock -- closing the descriptor is what clears
    /// `i_writecount`.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn releasing_the_write_descriptor_lets_the_same_command_run() {
        let dir = tempfile::tempdir().unwrap();
        let executable = dir.path().join("briefly-busy-tool");
        let writer = write_test_executable_keeping_write_descriptor(&executable, "ran-after-close");
        let policy = CommandPolicy::new(&[root(&dir)], CommandMode::AutoAllow).unwrap();
        let program = executable.to_string_lossy().into_owned();
        let invocation = args(&[program.as_str()], dir.path());

        let error = execute_command(&policy, invocation.clone())
            .await
            .expect_err("the spawn must fail while the write descriptor is held");
        assert!(
            matches!(&error, ToolError::ExecError(message) if message.contains("os error 26")),
            "the held descriptor must produce ETXTBSY, not some other failure: {error:?}"
        );

        drop(writer);

        // Closing our descriptor clears *our* contribution to
        // `inode->i_writecount`, but F134's mechanism means a sibling test in
        // this binary may have forked while the descriptor above was open and
        // still hold a copy of it in a child that has not reached `execve`.
        // Measured: 1 failure in 30 runs of the three-test selection before
        // this loop existed (capture:
        // `f135-releasing-flake-run-8.txt`, assertion
        // `closing the write descriptor must let the identical spawn succeed:
        // ExecError("command failed: Text file busy (os error 26)")`). So this
        // half re-attempts, bounded and without ever consulting the clock, and
        // insists every intervening failure be exactly that external ETXTBSY.
        let mut attempts = 0usize;
        let output = loop {
            attempts += 1;
            match execute_command(&policy, invocation.clone()).await {
                Ok(output) => break output,
                Err(ToolError::ExecError(message))
                    if message.contains("os error 26") && attempts < 64 => {}
                Err(error) => panic!(
                    "closing the write descriptor must let the identical spawn succeed; \
                     attempt {attempts} failed with {error:?}"
                ),
            }
        };
        assert!(output.success, "stderr: {}", output.stderr);
        assert_eq!(output.stdout, "ran-after-close");
    }

    /// The transient wording is earned by `ETXTBSY` alone. A spawn that fails
    /// for a reason waiting cannot fix -- here, a shebang naming an interpreter
    /// that does not exist -- must not be advertised as retryable.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_spawn_failure_that_is_not_etxtbsy_is_not_called_transient() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let executable = dir.path().join("broken-interpreter-tool");
        std::fs::write(&executable, "#!/kitsu-no-such-interpreter-f135\nexit 0\n").unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        let policy = CommandPolicy::new(&[root(&dir)], CommandMode::AutoAllow).unwrap();

        let program = executable.to_string_lossy().into_owned();
        let error = execute_command(&policy, args(&[program.as_str()], dir.path()))
            .await
            .expect_err("a missing interpreter must fail the spawn");

        let ToolError::ExecError(message) = error else {
            panic!("a failed spawn must surface as an ExecError: {error:?}");
        };
        assert!(
            !message.contains("os error 26"),
            "this fixture must not be an ETXTBSY: {message}"
        );
        let lowercase = message.to_lowercase();
        assert!(
            !lowercase.contains("transient"),
            "a permanent spawn failure must not claim to be transient: {message}"
        );
    }

    // ---- macOS Seatbelt backend ---------------------------------------------

    #[cfg(target_os = "macos")]
    fn launch_inputs_for(program: &str, writable: &[&str], cwd: &str) -> LaunchInputs {
        LaunchInputs {
            program: PathBuf::from(program),
            readonly_runtime_roots: Vec::new(),
            writable_command_roots: writable.iter().map(PathBuf::from).collect(),
            cwd: PathBuf::from(cwd),
            allow_network: false,
        }
    }

    /// `--allow-network` on macOS: network is denied by default and allowed
    /// with the flag, and the two SSH host files are re-allowed AFTER the
    /// `~/.ssh` deny (later rule wins) while `~/.ssh` itself stays denied, so
    /// private keys never become readable.
    #[cfg(target_os = "macos")]
    #[test]
    fn seatbelt_network_and_ssh_host_files_are_opt_in() {
        let sealed = build_seatbelt_profile(&launch_inputs_for(
            "/opt/tool/bin/ssh",
            &["/work/project"],
            "/work/project",
        ))
        .expect("profile builds");
        assert!(sealed.contains("(deny network*)"), "default denies network");

        let mut inputs =
            launch_inputs_for("/opt/tool/bin/ssh", &["/work/project"], "/work/project");
        inputs.allow_network = true;
        let open = build_seatbelt_profile(&inputs).expect("profile builds");
        assert!(
            !open.contains("(deny network*)"),
            "--allow-network drops the network deny: {open}"
        );
        assert!(
            open.contains("(allow network*)"),
            "--allow-network must ALLOW network explicitly, not just omit the \
             deny — (deny default) leaves it denied otherwise: {open}"
        );
        // ~/.ssh is still denied; the two host files are re-allowed after it.
        let deny_ssh = open.find("/.ssh\")").expect("~/.ssh still denied");
        let allow_known = open
            .find("/.ssh/known_hosts\")")
            .expect("known_hosts re-allowed");
        assert!(
            allow_known > deny_ssh,
            "the host-file allow must FOLLOW the ~/.ssh deny so it wins: {open}"
        );
        assert!(
            open.contains("/.ssh/config\")"),
            "config re-allowed: {open}"
        );
        // Only those two literals are carved out — no blanket ~/.ssh read.
        assert!(
            !open.contains("(allow file-read*\n    (subpath \""),
            "no subtree re-allow under ~/.ssh: {open}"
        );
    }

    /// The profile is deny-default, isolates the network, names the program for
    /// exec, and the writable set is EXACTLY the command roots + cwd + tmp (+dev
    /// nodes) — nothing else. Pure string assertions, nothing spawned (mirrors
    /// the `base_bubblewrap_arguments` plan tests).
    #[cfg(target_os = "macos")]
    #[test]
    fn seatbelt_profile_denies_by_default_and_confines_writes_to_the_roots() {
        let inputs =
            launch_inputs_for("/opt/tool/bin/git", &["/work/project"], "/work/project/sub");
        let profile = build_seatbelt_profile(&inputs).expect("profile builds");

        assert!(profile.contains("(version 1)"), "{profile}");
        assert!(profile.contains("(deny default)"), "{profile}");
        assert!(profile.contains("(deny network*)"), "{profile}");
        // Review F1/F2: no blanket process or mach-lookup allows.
        assert!(
            !profile.contains("(allow process*)"),
            "no blanket process allow: {profile}"
        );
        assert!(
            !profile.contains("(allow mach-lookup)\n"),
            "no blanket mach-lookup: {profile}"
        );
        assert!(
            profile.contains("(allow process-exec*\n"),
            "exec is an allow-list: {profile}"
        );
        assert!(
            profile.contains("(global-name \"com.apple.bsd.dirhelper\")"),
            "{profile}"
        );
        // Reads are broad, but secret paths are denied AFTER the allow (the
        // later SBPL rule wins).
        let allow_read = profile
            .find("(allow file-read*)")
            .expect("broad read allow");
        let deny_read = profile.find("(deny file-read*").expect("secret deny block");
        assert!(deny_read > allow_read, "deny must follow the allow to win");
        assert!(profile.contains("/.ssh\")"), "ssh keys denied: {profile}");
        assert!(
            profile.contains("/.zshrc\")"),
            "shell rc (gateway tokens) denied: {profile}"
        );

        // Isolate the file-write* block (from its opening line to the lone `)`
        // terminator) and assert its subpaths are exactly the writable roots +
        // cwd + /private/tmp (dev nodes are literals, not subpaths).
        let after_open = profile
            .split("(allow file-write*\n")
            .nth(1)
            .expect("a file-write* block");
        let write_block: String = after_open
            .lines()
            .take_while(|l| l.trim() != ")")
            .collect::<Vec<_>>()
            .join("\n");
        let write_subpaths: Vec<&str> = write_block
            .lines()
            .filter_map(|l| {
                l.trim()
                    .strip_prefix("(subpath \"")
                    .and_then(|s| s.strip_suffix("\")"))
            })
            .collect();
        let mut expected = vec!["/work/project", "/work/project/sub", "/private/tmp"];
        let mut got = write_subpaths.clone();
        expected.sort_unstable();
        got.sort_unstable();
        assert_eq!(
            got, expected,
            "writable subpaths must be exactly roots+cwd+tmp: {profile}"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn seatbelt_profile_carries_no_environment_secrets() {
        // The profile never carries env at all (env rides the outer process),
        // so it trivially cannot leak a token — assert that invariant holds.
        let inputs = launch_inputs_for("/bin/echo", &["/w"], "/w");
        let profile = build_seatbelt_profile(&inputs).unwrap();
        assert!(!profile.contains("TOKEN"), "{profile}");
        assert!(!profile.contains("SECRET"), "{profile}");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn sbpl_quote_escapes_quotes_and_rejects_control_and_non_utf8() {
        use std::os::unix::ffi::OsStrExt;

        // A path with an embedded double-quote is escaped, not dropped.
        let q = sbpl_quote(Path::new("/weird/a\"b")).expect("quotes escape");
        assert_eq!(q, "\"/weird/a\\\"b\"");
        // A backslash is escaped too.
        let b = sbpl_quote(Path::new("/weird/a\\b")).unwrap();
        assert_eq!(b, "\"/weird/a\\\\b\"");
        // A newline (s-expr structure attack) is refused.
        assert!(matches!(
            sbpl_quote(Path::new("/weird/a\nb")),
            Err(e) if e.reason == CommandContainmentUnavailableReason::InvalidConfiguration
        ));
        // Non-UTF-8 is refused (never lossily transcoded into a policy).
        let non_utf8 = std::ffi::OsStr::from_bytes(b"/weird/\xff");
        assert!(matches!(
            sbpl_quote(Path::new(non_utf8)),
            Err(e) if e.reason == CommandContainmentUnavailableReason::InvalidConfiguration
        ));
    }

    /// Live end-to-end: a granted command runs contained via sandbox-exec, and
    /// a write OUTSIDE the granted roots is denied by the profile. Skips (does
    /// not fail) if Seatbelt is unavailable, mirroring the bwrap live test.
    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn live_seatbelt_runs_a_command_and_blocks_outside_writes() {
        let allowed = tempfile::tempdir().unwrap();
        // The profile allows /private/tmp for command scratch files, so a
        // fixture there cannot prove write confinement.
        let outside = tempfile::tempdir_in("/private/var/tmp").unwrap();
        let policy = CommandPolicy::new(&[root(&allowed)], CommandMode::Deny)
            .unwrap()
            .with_containment_config(CommandContainmentConfig::default());
        let CommandContainmentStatus::Ready(ready) = policy.containment_status() else {
            eprintln!(
                "skipping live Seatbelt assertions: {:?}",
                policy.containment_status()
            );
            return;
        };
        assert_eq!(ready.backend, CommandContainmentBackendKind::Seatbelt);

        // A write INSIDE the granted root succeeds and is contained.
        let inside = vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            "echo contained > ./inside.txt".to_string(),
        ];
        policy.grant_command(&inside, allowed.path()).unwrap();
        let output = execute_command(
            &policy,
            CommandExecArgs {
                argv: inside,
                cwd: root(&allowed),
                timeout_ms: Some(5_000),
            },
        )
        .await
        .unwrap();
        assert!(
            output.success,
            "write inside root should succeed: {}",
            output.stderr
        );
        assert!(
            allowed.path().join("inside.txt").exists(),
            "file was written in the root"
        );

        // A write OUTSIDE every granted root is denied by the profile.
        let leak = outside.path().join("leak.txt");
        let escape = vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            format!("echo escaped > {}", leak.display()),
        ];
        policy.grant_command(&escape, allowed.path()).unwrap();
        let output = execute_command(
            &policy,
            CommandExecArgs {
                argv: escape,
                cwd: root(&allowed),
                timeout_ms: Some(5_000),
            },
        )
        .await
        .unwrap();
        assert!(!output.success, "write outside the roots must fail");
        assert!(
            !leak.exists(),
            "the outside file must not have been created"
        );
    }

    // ---- macOS Seatbelt backend: live integration (file ops, git, scripts,
    // permission grant/revoke). Each skips (does not fail) when Seatbelt is
    // unavailable, mirroring the Linux live tests. ------------------------------

    /// A contained policy over `root` using the real Seatbelt backend, or None
    /// if containment is not ready on this host (the test should then skip).
    #[cfg(target_os = "macos")]
    fn live_seatbelt_policy(root_dir: &tempfile::TempDir) -> Option<CommandPolicy> {
        let policy = CommandPolicy::new(&[root(root_dir)], CommandMode::Deny)
            .unwrap()
            .with_containment_config(CommandContainmentConfig::default());
        match policy.containment_status() {
            CommandContainmentStatus::Ready(ready)
                if ready.backend == CommandContainmentBackendKind::Seatbelt =>
            {
                Some(policy)
            }
            other => {
                eprintln!("skipping live Seatbelt test: {other:?}");
                None
            }
        }
    }

    #[cfg(target_os = "macos")]
    async fn run_granted(
        policy: &CommandPolicy,
        dir: &tempfile::TempDir,
        argv: &[&str],
    ) -> CommandOutput {
        let argv: Vec<String> = argv.iter().map(|a| (*a).to_string()).collect();
        policy.grant_command(&argv, dir.path()).unwrap();
        execute_command(
            policy,
            CommandExecArgs {
                argv,
                cwd: root(dir),
                timeout_ms: Some(10_000),
            },
        )
        .await
        .unwrap()
    }

    /// File operations round-trip under containment: a write inside the root
    /// lands on disk and a subsequent contained read sees it.
    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn live_seatbelt_file_ops_round_trip_inside_the_root() {
        let dir = tempfile::tempdir().unwrap();
        let Some(policy) = live_seatbelt_policy(&dir) else {
            return;
        };

        let write = run_granted(
            &policy,
            &dir,
            &["/bin/sh", "-c", "printf 'hello-contained' > ./note.txt"],
        )
        .await;
        assert!(write.success, "write inside root: {}", write.stderr);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("note.txt")).unwrap(),
            "hello-contained"
        );

        // A contained read of the same file sees the content.
        let read = run_granted(&policy, &dir, &["/bin/cat", "./note.txt"]).await;
        assert!(read.success, "read inside root: {}", read.stderr);
        assert_eq!(read.stdout.trim(), "hello-contained");
    }

    /// Git works end-to-end under containment: init, add, commit, status — all
    /// writing inside the granted root. This is the headline capability the
    /// macOS backend unlocks.
    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn live_seatbelt_git_init_add_commit_status() {
        let dir = tempfile::tempdir().unwrap();
        let Some(policy) = live_seatbelt_policy(&dir) else {
            return;
        };
        let git = "/usr/bin/git";
        if !std::path::Path::new(git).exists() {
            eprintln!("skipping: {git} not present");
            return;
        }

        let init = run_granted(&policy, &dir, &[git, "init", "-q"]).await;
        assert!(init.success, "git init: {}", init.stderr);
        assert!(dir.path().join(".git").exists(), "repo created in the root");

        // Config + a file + add + commit, all contained writes.
        for argv in [
            vec![git, "config", "user.email", "t@t"],
            vec![git, "config", "user.name", "t"],
        ] {
            let out = run_granted(&policy, &dir, &argv).await;
            assert!(out.success, "git config: {}", out.stderr);
        }
        let _ = run_granted(&policy, &dir, &["/bin/sh", "-c", "echo hi > ./f.txt"]).await;
        let add = run_granted(&policy, &dir, &[git, "add", "f.txt"]).await;
        assert!(add.success, "git add: {}", add.stderr);
        let commit = run_granted(&policy, &dir, &[git, "commit", "-q", "-m", "x"]).await;
        assert!(commit.success, "git commit: {}", commit.stderr);
        let status = run_granted(&policy, &dir, &[git, "status", "--porcelain"]).await;
        assert!(status.success, "git status: {}", status.stderr);
        assert!(
            status.stdout.trim().is_empty(),
            "clean tree after commit: {:?}",
            status.stdout
        );
    }

    /// A multi-step shell script runs contained and its writes (including a
    /// nested directory) stay inside the root.
    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn live_seatbelt_script_creates_nested_files_inside_the_root() {
        let dir = tempfile::tempdir().unwrap();
        let Some(policy) = live_seatbelt_policy(&dir) else {
            return;
        };

        let out = run_granted(
            &policy,
            &dir,
            &["/bin/sh", "-c", "mkdir -p sub/deep && echo a > sub/deep/a.txt && echo b >> sub/deep/a.txt && wc -l < sub/deep/a.txt"],
        )
        .await;
        assert!(out.success, "script: {}", out.stderr);
        assert_eq!(out.stdout.trim(), "2", "script ran all steps");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("sub/deep/a.txt")).unwrap(),
            "a\nb\n"
        );
    }

    /// Review F1: a granted command cannot read host secrets. Uses the real
    /// shell rc / ssh dir when present and checks the denial reason, never the
    /// content (nothing is printed on success either).
    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn live_seatbelt_denies_reading_host_secret_paths() {
        let dir = tempfile::tempdir().unwrap();
        let Some(policy) = live_seatbelt_policy(&dir) else {
            return;
        };
        let home = std::env::var("HOME").unwrap();
        let mut checked = 0;
        for rel in [".zshrc", ".ssh", ".aws", ".kitsu"] {
            let path = format!("{home}/{rel}");
            if !std::path::Path::new(&path).exists() {
                continue;
            }
            checked += 1;
            let probe = format!("ls -d {path}/. >/dev/null 2>&1 || head -c1 {path} >/dev/null");
            let out = run_granted(&policy, &dir, &["/bin/sh", "-c", &probe]).await;
            assert!(
                !out.success,
                "{rel} must not be readable inside the sandbox"
            );
            assert!(out.stdout.is_empty(), "nothing from {rel} reaches stdout");
            assert!(
                out.stderr.contains("Operation not permitted"),
                "{rel} denied by the sandbox, not missing: {:?}",
                out.stderr
            );
        }
        if checked == 0 {
            eprintln!("skipping: no secret paths exist under $HOME on this host");
        }
    }

    /// Review F2: exec is an allow-list. A granted `sh -c` cannot run a binary
    /// outside the system dirs and command/runtime roots (Linux bwrap parity).
    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn live_seatbelt_denies_exec_outside_the_roots() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let Some(policy) = live_seatbelt_policy(&dir) else {
            return;
        };
        let script = outside.path().join("tool.sh");
        std::fs::write(&script, "#!/bin/sh\necho ESCAPED\n").unwrap();
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let script = std::fs::canonicalize(&script).unwrap();

        let out = run_granted(&policy, &dir, &["/bin/sh", "-c", &script.to_string_lossy()]).await;
        assert!(!out.success, "exec outside the roots must fail");
        assert!(
            !out.stdout.contains("ESCAPED"),
            "the outside binary never ran"
        );
        assert!(
            out.stderr.contains("Operation not permitted"),
            "{:?}",
            out.stderr
        );

        // CONTROL: the same script inside the command root runs.
        let inside = dir.path().join("tool.sh");
        std::fs::write(&inside, "#!/bin/sh\necho INSIDE\n").unwrap();
        std::fs::set_permissions(&inside, std::fs::Permissions::from_mode(0o755)).unwrap();
        let inside = std::fs::canonicalize(&inside).unwrap();
        let out = run_granted(&policy, &dir, &["/bin/sh", "-c", &inside.to_string_lossy()]).await;
        assert!(
            out.success,
            "exec inside a root still works: {}",
            out.stderr
        );
        assert_eq!(out.stdout.trim(), "INSIDE");
    }

    /// The permission grant/revoke flow gates execution: in Deny mode an
    /// ungranted argv is refused before it runs; granting lets it run; revoking
    /// refuses it again. (Independent of the sandbox — works without Seatbelt —
    /// but asserted here as the authority half of the backend contract.)
    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn live_seatbelt_grant_then_revoke_gates_execution() {
        let dir = tempfile::tempdir().unwrap();
        let Some(policy) = live_seatbelt_policy(&dir) else {
            return;
        };
        let argv = vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            "echo ok > ./g.txt".to_string(),
        ];

        // Ungranted in Deny mode: refused before execution, no file.
        let denied = execute_command(
            &policy,
            CommandExecArgs {
                argv: argv.clone(),
                cwd: root(&dir),
                timeout_ms: Some(5_000),
            },
        )
        .await;
        assert!(denied.is_err(), "ungranted command must be denied");
        assert!(
            !dir.path().join("g.txt").exists(),
            "denied command wrote nothing"
        );

        // Granted: runs and writes.
        policy.grant_command(&argv, dir.path()).unwrap();
        let ok = execute_command(
            &policy,
            CommandExecArgs {
                argv: argv.clone(),
                cwd: root(&dir),
                timeout_ms: Some(5_000),
            },
        )
        .await
        .unwrap();
        assert!(ok.success, "granted command runs: {}", ok.stderr);
        assert!(
            dir.path().join("g.txt").exists(),
            "granted command wrote the file"
        );

        // Revoked: refused again.
        assert!(
            policy.revoke_command(&argv, dir.path()).unwrap().is_some(),
            "revoke removes the grant"
        );
        let denied_again = execute_command(
            &policy,
            CommandExecArgs {
                argv,
                cwd: root(&dir),
                timeout_ms: Some(5_000),
            },
        )
        .await;
        assert!(denied_again.is_err(), "revoked command is denied again");
    }
}
