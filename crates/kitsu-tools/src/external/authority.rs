//! Provider-free admission values for a future bounded child supervisor.
//!
//! Admission is deliberately split from executable observation. Disabled,
//! denied, unsupported, malformed, or authority-mismatched requests return
//! before the injected preparation boundary can canonicalize or hash a path
//! or inspect the containment backend. No process API exists in this module.

#![allow(dead_code)]

use std::collections::HashSet;
use std::fmt;
use std::path::{Component, Path, PathBuf};

const CHILD_LAUNCH_REQUEST_VERSION_V1: u32 = 1;
const SHA256_HEX_BYTES: usize = 64;
const MAX_ARGUMENT_COUNT: usize = 256;
const MAX_ARGUMENT_BYTES: usize = 16 * 1024;
const MAX_TOTAL_ARGUMENT_BYTES: usize = 64 * 1024;
const MAX_STARTUP_BUDGET_MILLIS: u64 = 30_000;
const MAX_REQUEST_BUDGET_MILLIS: u64 = 5 * 60_000;
const MAX_DRAIN_BUDGET_MILLIS: u64 = 30_000;
const MAX_KILL_BUDGET_MILLIS: u64 = 10_000;
const MAX_REAP_BUDGET_MILLIS: u64 = 30_000;
const MAX_STDIN_BYTES: usize = 1024 * 1024;
const MAX_STDOUT_BYTES: usize = 4 * 1024 * 1024;
const MAX_STDERR_BYTES: usize = 1024 * 1024;
const MAX_FRAME_BYTES: usize = 256 * 1024;
const MAX_ENVIRONMENT_VARIABLES: usize = 64;
const MAX_ENVIRONMENT_NAME_BYTES: usize = 128;
const MAX_ENVIRONMENT_VALUE_BYTES: usize = 4 * 1024;
const MAX_ENVIRONMENT_PROJECTION_BYTES: usize = 16 * 1024;
const MAX_FILESYSTEM_GRANTS: usize = 64;
const MAX_FILESYSTEM_PATH_BYTES: usize = 4 * 1024;
const MAX_FILESYSTEM_POLICY_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChildLaunchModeV1 {
    Disabled,
    Enabled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ChildPhaseBudgetsV1 {
    startup_millis: u64,
    request_millis: u64,
    drain_millis: u64,
    kill_millis: u64,
    reap_millis: u64,
}

impl ChildPhaseBudgetsV1 {
    pub(crate) fn new(
        startup_millis: u64,
        request_millis: u64,
        drain_millis: u64,
        kill_millis: u64,
        reap_millis: u64,
    ) -> Self {
        Self {
            startup_millis,
            request_millis,
            drain_millis,
            kill_millis,
            reap_millis,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ChildIoLimitsV1 {
    stdin_bytes: usize,
    stdout_bytes: usize,
    stderr_bytes: usize,
    frame_bytes: usize,
}

impl ChildIoLimitsV1 {
    pub(crate) fn new(
        stdin_bytes: usize,
        stdout_bytes: usize,
        stderr_bytes: usize,
        frame_bytes: usize,
    ) -> Self {
        Self {
            stdin_bytes,
            stdout_bytes,
            stderr_bytes,
            frame_bytes,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChildProcessGroupPolicyV1 {
    Inherited,
    Owned,
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct ChildEnvironmentValueV1(String);

impl fmt::Debug for ChildEnvironmentValueV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("\"[REDACTED]\"")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ChildEnvironmentVariableV1 {
    name: String,
    value: ChildEnvironmentValueV1,
}

impl ChildEnvironmentVariableV1 {
    pub(crate) fn new(name: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            value: ChildEnvironmentValueV1(value.into()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ChildEnvironmentPolicyV1 {
    clear_inherited: bool,
    projection: Vec<ChildEnvironmentVariableV1>,
}

impl ChildEnvironmentPolicyV1 {
    pub(crate) fn new(clear_inherited: bool, projection: Vec<ChildEnvironmentVariableV1>) -> Self {
        Self {
            clear_inherited,
            projection,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChildFilesystemAccessV1 {
    ReadOnly,
    ReadWrite,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ChildFilesystemGrantV1 {
    path: PathBuf,
    access: ChildFilesystemAccessV1,
}

impl ChildFilesystemGrantV1 {
    pub(crate) fn new(path: impl Into<PathBuf>, access: ChildFilesystemAccessV1) -> Self {
        Self {
            path: path.into(),
            access,
        }
    }

    pub(super) fn path(&self) -> &Path {
        &self.path
    }

    pub(super) fn access(&self) -> ChildFilesystemAccessV1 {
        self.access
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ChildFilesystemPolicyV1 {
    grants: Vec<ChildFilesystemGrantV1>,
}

impl ChildFilesystemPolicyV1 {
    pub(crate) fn new(grants: Vec<ChildFilesystemGrantV1>) -> Self {
        Self { grants }
    }

    pub(super) fn grants(&self) -> &[ChildFilesystemGrantV1] {
        &self.grants
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum ChildNetworkPolicyV1 {
    #[default]
    Deny,
    Allow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChildTimeoutDispositionV1 {
    Abandon,
    TerminateProcessGroupAndReap,
}

/// Exact, inert runtime facts supplied independently by a request and host.
/// Validation happens during profile construction or pure admission; neither
/// path observes the filesystem or a containment backend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ChildRuntimePolicyV1 {
    phase_budgets: ChildPhaseBudgetsV1,
    io_limits: ChildIoLimitsV1,
    process_group: ChildProcessGroupPolicyV1,
    environment: ChildEnvironmentPolicyV1,
    filesystem: ChildFilesystemPolicyV1,
    network: ChildNetworkPolicyV1,
    timeout_disposition: ChildTimeoutDispositionV1,
}

impl ChildRuntimePolicyV1 {
    pub(crate) fn new(
        phase_budgets: ChildPhaseBudgetsV1,
        io_limits: ChildIoLimitsV1,
        process_group: ChildProcessGroupPolicyV1,
        environment: ChildEnvironmentPolicyV1,
        filesystem: ChildFilesystemPolicyV1,
        network: ChildNetworkPolicyV1,
        timeout_disposition: ChildTimeoutDispositionV1,
    ) -> Self {
        Self {
            phase_budgets,
            io_limits,
            process_group,
            environment,
            filesystem,
            network,
            timeout_disposition,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChildRuntimePhaseV1 {
    Startup,
    Request,
    Drain,
    Kill,
    Reap,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChildIoLimitV1 {
    Stdin,
    Stdout,
    Stderr,
    Frame,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChildRuntimePolicyErrorV1 {
    InvalidPhaseBudget(ChildRuntimePhaseV1),
    InvalidIoLimit(ChildIoLimitV1),
    FrameExceedsStreamLimit(ChildIoLimitV1),
    ProcessGroupNotOwned,
    InheritedEnvironmentNotCleared,
    InvalidEnvironmentProjection,
    InvalidFilesystemPolicy,
    NetworkNotDenied,
    TimeoutMayAbandon,
}

/// Inert, untrusted launch facts. Constructing this value performs no I/O.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ChildLaunchRequestV1 {
    version: u32,
    mode: ChildLaunchModeV1,
    executable: PathBuf,
    executable_sha256: String,
    arguments: Vec<String>,
    working_directory: PathBuf,
    runtime_policy: ChildRuntimePolicyV1,
}

impl ChildLaunchRequestV1 {
    pub(crate) fn new(
        version: u32,
        mode: ChildLaunchModeV1,
        executable: impl Into<PathBuf>,
        executable_sha256: impl Into<String>,
        arguments: Vec<String>,
        working_directory: impl Into<PathBuf>,
        runtime_policy: ChildRuntimePolicyV1,
    ) -> Self {
        Self {
            version,
            mode,
            executable: executable.into(),
            executable_sha256: executable_sha256.into(),
            arguments,
            working_directory: working_directory.into(),
            runtime_policy,
        }
    }
}

/// Host-authored authority, kept separate from untrusted request facts.
///
/// V1 is intentionally exact rather than root- or pattern-based: executable,
/// content digest, argv, and cwd must all match. Future authority expansion is
/// a separate reviewed change, not a broader interpretation of these fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AuthorityProfileV1 {
    spec: ValidatedChildLaunchSpecV1,
}

impl AuthorityProfileV1 {
    pub(crate) fn exact(
        executable: impl Into<PathBuf>,
        executable_sha256: impl Into<String>,
        arguments: Vec<String>,
        working_directory: impl Into<PathBuf>,
        runtime_policy: ChildRuntimePolicyV1,
    ) -> Result<Self, ChildLaunchAdmissionErrorV1> {
        Ok(Self {
            spec: ValidatedChildLaunchSpecV1::new(
                executable.into(),
                executable_sha256.into(),
                arguments,
                working_directory.into(),
                runtime_policy,
            )?,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChildLaunchAuthorityDecisionV1<'a> {
    Deny,
    Admit(&'a AuthorityProfileV1),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AuthorityMismatchFieldV1 {
    Executable,
    ExecutableSha256,
    Arguments,
    WorkingDirectory,
    RuntimePolicy,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ChildLaunchAdmissionErrorV1 {
    Disabled,
    AuthorityDenied,
    UnsupportedVersion { observed: u32 },
    InvalidExecutablePath,
    InvalidExecutableSha256,
    InvalidArguments,
    InvalidWorkingDirectory,
    WorkingDirectoryOutsideFilesystemPolicy,
    InvalidRuntimePolicy(ChildRuntimePolicyErrorV1),
    AuthorityMismatch(AuthorityMismatchFieldV1),
    ExecutableCanonicalization(String),
    ExecutablePathChanged,
    ExecutableHash(String),
    ExecutableDigestMismatch,
    ContainmentPreflight(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ExecutableSha256V1([u8; 32]);

impl ExecutableSha256V1 {
    fn parse(value: &str) -> Result<Self, ChildLaunchAdmissionErrorV1> {
        if value.len() != SHA256_HEX_BYTES
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(ChildLaunchAdmissionErrorV1::InvalidExecutableSha256);
        }

        let mut digest = [0_u8; 32];
        for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
            digest[index] = (hex_nibble(pair[0]) << 4) | hex_nibble(pair[1]);
        }
        Ok(Self(digest))
    }
}

fn hex_nibble(byte: u8) -> u8 {
    match byte {
        b'0'..=b'9' => byte - b'0',
        b'a'..=b'f' => byte - b'a' + 10,
        _ => unreachable!("digest syntax is checked before decoding"),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ValidatedChildLaunchSpecV1 {
    executable: PathBuf,
    executable_sha256: ExecutableSha256V1,
    arguments: Vec<String>,
    working_directory: PathBuf,
    runtime_policy: ChildRuntimePolicyV1,
}

impl ValidatedChildLaunchSpecV1 {
    fn new(
        executable: PathBuf,
        executable_sha256: String,
        arguments: Vec<String>,
        working_directory: PathBuf,
        runtime_policy: ChildRuntimePolicyV1,
    ) -> Result<Self, ChildLaunchAdmissionErrorV1> {
        if !is_normalized_absolute_path(&executable) {
            return Err(ChildLaunchAdmissionErrorV1::InvalidExecutablePath);
        }
        let executable_sha256 = ExecutableSha256V1::parse(&executable_sha256)?;
        validate_arguments(&arguments)?;
        if !is_normalized_absolute_path(&working_directory) {
            return Err(ChildLaunchAdmissionErrorV1::InvalidWorkingDirectory);
        }
        validate_runtime_policy(&runtime_policy)
            .map_err(ChildLaunchAdmissionErrorV1::InvalidRuntimePolicy)?;
        if !runtime_policy
            .filesystem
            .grants
            .iter()
            .any(|grant| working_directory.starts_with(&grant.path))
        {
            return Err(ChildLaunchAdmissionErrorV1::WorkingDirectoryOutsideFilesystemPolicy);
        }
        Ok(Self {
            executable,
            executable_sha256,
            arguments,
            working_directory,
            runtime_policy,
        })
    }
}

fn is_normalized_absolute_path(path: &Path) -> bool {
    let Some(text) = path.to_str() else {
        return false;
    };
    let normalized: PathBuf = path.components().collect();
    path.is_absolute()
        && !text.contains('\0')
        && normalized == path
        && path.components().all(|component| match component {
            Component::Prefix(_) => cfg!(windows),
            Component::RootDir | Component::Normal(_) => true,
            Component::CurDir | Component::ParentDir => false,
        })
}

fn validate_arguments(arguments: &[String]) -> Result<(), ChildLaunchAdmissionErrorV1> {
    if arguments.len() > MAX_ARGUMENT_COUNT {
        return Err(ChildLaunchAdmissionErrorV1::InvalidArguments);
    }
    let mut total = 0_usize;
    for argument in arguments {
        if argument.as_bytes().contains(&0) || argument.len() > MAX_ARGUMENT_BYTES {
            return Err(ChildLaunchAdmissionErrorV1::InvalidArguments);
        }
        total = total.saturating_add(argument.len());
        if total > MAX_TOTAL_ARGUMENT_BYTES {
            return Err(ChildLaunchAdmissionErrorV1::InvalidArguments);
        }
    }
    Ok(())
}

fn validate_runtime_policy(policy: &ChildRuntimePolicyV1) -> Result<(), ChildRuntimePolicyErrorV1> {
    for (phase, observed, maximum) in [
        (
            ChildRuntimePhaseV1::Startup,
            policy.phase_budgets.startup_millis,
            MAX_STARTUP_BUDGET_MILLIS,
        ),
        (
            ChildRuntimePhaseV1::Request,
            policy.phase_budgets.request_millis,
            MAX_REQUEST_BUDGET_MILLIS,
        ),
        (
            ChildRuntimePhaseV1::Drain,
            policy.phase_budgets.drain_millis,
            MAX_DRAIN_BUDGET_MILLIS,
        ),
        (
            ChildRuntimePhaseV1::Kill,
            policy.phase_budgets.kill_millis,
            MAX_KILL_BUDGET_MILLIS,
        ),
        (
            ChildRuntimePhaseV1::Reap,
            policy.phase_budgets.reap_millis,
            MAX_REAP_BUDGET_MILLIS,
        ),
    ] {
        if observed == 0 || observed > maximum {
            return Err(ChildRuntimePolicyErrorV1::InvalidPhaseBudget(phase));
        }
    }

    for (limit, observed, maximum) in [
        (
            ChildIoLimitV1::Stdin,
            policy.io_limits.stdin_bytes,
            MAX_STDIN_BYTES,
        ),
        (
            ChildIoLimitV1::Stdout,
            policy.io_limits.stdout_bytes,
            MAX_STDOUT_BYTES,
        ),
        (
            ChildIoLimitV1::Stderr,
            policy.io_limits.stderr_bytes,
            MAX_STDERR_BYTES,
        ),
        (
            ChildIoLimitV1::Frame,
            policy.io_limits.frame_bytes,
            MAX_FRAME_BYTES,
        ),
    ] {
        if observed == 0 || observed > maximum {
            return Err(ChildRuntimePolicyErrorV1::InvalidIoLimit(limit));
        }
    }
    if policy.io_limits.frame_bytes > policy.io_limits.stdin_bytes {
        return Err(ChildRuntimePolicyErrorV1::FrameExceedsStreamLimit(
            ChildIoLimitV1::Stdin,
        ));
    }
    if policy.io_limits.frame_bytes > policy.io_limits.stdout_bytes {
        return Err(ChildRuntimePolicyErrorV1::FrameExceedsStreamLimit(
            ChildIoLimitV1::Stdout,
        ));
    }

    if policy.process_group != ChildProcessGroupPolicyV1::Owned {
        return Err(ChildRuntimePolicyErrorV1::ProcessGroupNotOwned);
    }
    if !policy.environment.clear_inherited {
        return Err(ChildRuntimePolicyErrorV1::InheritedEnvironmentNotCleared);
    }
    validate_environment_projection(&policy.environment.projection)?;
    validate_filesystem_policy(&policy.filesystem)?;
    if policy.network != ChildNetworkPolicyV1::Deny {
        return Err(ChildRuntimePolicyErrorV1::NetworkNotDenied);
    }
    if policy.timeout_disposition != ChildTimeoutDispositionV1::TerminateProcessGroupAndReap {
        return Err(ChildRuntimePolicyErrorV1::TimeoutMayAbandon);
    }
    Ok(())
}

fn validate_filesystem_policy(
    policy: &ChildFilesystemPolicyV1,
) -> Result<(), ChildRuntimePolicyErrorV1> {
    if policy.grants.is_empty() || policy.grants.len() > MAX_FILESYSTEM_GRANTS {
        return Err(ChildRuntimePolicyErrorV1::InvalidFilesystemPolicy);
    }
    let mut total_bytes = 0_usize;
    for (index, grant) in policy.grants.iter().enumerate() {
        let path_bytes = grant.path.as_os_str().as_encoded_bytes().len();
        total_bytes = total_bytes.saturating_add(path_bytes);
        if grant.path.parent().is_none()
            || !is_normalized_absolute_path(&grant.path)
            || path_bytes > MAX_FILESYSTEM_PATH_BYTES
            || total_bytes > MAX_FILESYSTEM_POLICY_BYTES
        {
            return Err(ChildRuntimePolicyErrorV1::InvalidFilesystemPolicy);
        }
        if policy.grants[..index]
            .iter()
            .any(|other| grant.path.starts_with(&other.path) || other.path.starts_with(&grant.path))
        {
            return Err(ChildRuntimePolicyErrorV1::InvalidFilesystemPolicy);
        }
    }
    Ok(())
}

fn validate_environment_projection(
    projection: &[ChildEnvironmentVariableV1],
) -> Result<(), ChildRuntimePolicyErrorV1> {
    if projection.len() > MAX_ENVIRONMENT_VARIABLES {
        return Err(ChildRuntimePolicyErrorV1::InvalidEnvironmentProjection);
    }

    let mut names = HashSet::with_capacity(projection.len());
    let mut total_bytes = 0_usize;
    for variable in projection {
        if !is_environment_name(&variable.name)
            || variable.value.0.as_bytes().contains(&0)
            || variable.value.0.len() > MAX_ENVIRONMENT_VALUE_BYTES
            || !names.insert(variable.name.as_str())
        {
            return Err(ChildRuntimePolicyErrorV1::InvalidEnvironmentProjection);
        }
        total_bytes = total_bytes
            .saturating_add(variable.name.len())
            .saturating_add(variable.value.0.len());
        if total_bytes > MAX_ENVIRONMENT_PROJECTION_BYTES {
            return Err(ChildRuntimePolicyErrorV1::InvalidEnvironmentProjection);
        }
    }
    Ok(())
}

fn is_environment_name(name: &str) -> bool {
    if name.is_empty() || name.len() > MAX_ENVIRONMENT_NAME_BYTES {
        return false;
    }
    let mut bytes = name.bytes();
    matches!(bytes.next(), Some(b'A'..=b'Z' | b'_'))
        && bytes.all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
}

/// Pure admission result. Possessing it proves only that inert request facts
/// matched host policy; it does not prove that an executable exists or that
/// containment is available.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AdmittedChildLaunchV1 {
    spec: ValidatedChildLaunchSpecV1,
}

impl AdmittedChildLaunchV1 {
    pub(super) fn executable(&self) -> &Path {
        &self.spec.executable
    }

    pub(super) fn executable_sha256(&self) -> &[u8; 32] {
        &self.spec.executable_sha256.0
    }

    pub(super) fn working_directory(&self) -> &Path {
        &self.spec.working_directory
    }

    pub(super) fn filesystem(&self) -> &ChildFilesystemPolicyV1 {
        &self.spec.runtime_policy.filesystem
    }
}

/// Preparation operations that may observe the host after pure admission.
///
/// The eventual supervisor will provide the production implementation. This
/// card injects the boundary so tests can prove rejected inputs invoke none of
/// these operations. There is deliberately no spawn method.
pub(crate) trait ChildLaunchPreparationV1 {
    fn canonicalize_executable(&mut self, executable: &Path) -> Result<PathBuf, String>;
    fn executable_sha256(&mut self, executable: &Path) -> Result<[u8; 32], String>;
    fn containment_preflight(
        &mut self,
        launch: &AdmittedChildLaunchV1,
    ) -> Result<PreparedChildContainmentTokenV1, String>;
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct PreparedChildContainmentTokenV1 {
    id: u64,
}

impl PreparedChildContainmentTokenV1 {
    pub(crate) fn new(id: u64) -> Self {
        Self { id }
    }

    pub(super) fn id(&self) -> u64 {
        self.id
    }

    #[cfg(test)]
    fn id_for_test(&self) -> u64 {
        self.id
    }
}

/// Prepared, still-unspawned identity for the future lifecycle owner.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct PreparedChildLaunchV1 {
    canonical_executable: PathBuf,
    executable_sha256: ExecutableSha256V1,
    arguments: Vec<String>,
    working_directory: PathBuf,
    runtime_policy: ChildRuntimePolicyV1,
    containment_token: PreparedChildContainmentTokenV1,
}

/// Exact OS-launch projection of one consumed preparation proof.
///
/// This value stays private to the external runtime. It is intentionally
/// neither `Clone` nor `Copy`, and its custom debug output preserves
/// environment names while redacting every value.
pub(super) struct PreparedChildOsLaunchV1 {
    executable: PathBuf,
    executable_sha256: ExecutableSha256V1,
    arguments: Vec<String>,
    working_directory: PathBuf,
    phase_budgets: ChildPhaseBudgetsV1,
    io_limits: ChildIoLimitsV1,
    environment: ChildEnvironmentPolicyV1,
    filesystem: ChildFilesystemPolicyV1,
    containment_token: PreparedChildContainmentTokenV1,
}

impl fmt::Debug for PreparedChildOsLaunchV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedChildOsLaunchV1")
            .field("executable", &self.executable)
            .field("executable_sha256", &self.executable_sha256)
            .field("arguments", &self.arguments)
            .field("working_directory", &self.working_directory)
            .field("phase_budgets", &self.phase_budgets)
            .field("io_limits", &self.io_limits)
            .field("environment", &self.environment)
            .field("filesystem", &self.filesystem)
            .field("containment_token", &self.containment_token)
            .finish()
    }
}

impl PreparedChildOsLaunchV1 {
    pub(super) fn executable(&self) -> &Path {
        &self.executable
    }

    pub(super) fn arguments(&self) -> &[String] {
        &self.arguments
    }

    pub(super) fn working_directory(&self) -> &Path {
        &self.working_directory
    }

    pub(super) fn environment(&self) -> impl Iterator<Item = (&str, &str)> {
        self.environment
            .projection
            .iter()
            .map(|variable| (variable.name.as_str(), variable.value.0.as_str()))
    }

    #[cfg(test)]
    pub(super) fn environment_for_test(&self) -> Vec<(&str, &str)> {
        self.environment().collect()
    }

    #[cfg(test)]
    pub(super) fn filesystem_for_test(&self) -> &ChildFilesystemPolicyV1 {
        &self.filesystem
    }

    #[cfg(test)]
    pub(super) fn containment_token_for_test(&self) -> &PreparedChildContainmentTokenV1 {
        &self.containment_token
    }

    pub(super) fn containment_token(&self) -> &PreparedChildContainmentTokenV1 {
        &self.containment_token
    }

    pub(super) fn phase_budgets_millis(&self) -> [u64; 5] {
        [
            self.phase_budgets.startup_millis,
            self.phase_budgets.request_millis,
            self.phase_budgets.drain_millis,
            self.phase_budgets.kill_millis,
            self.phase_budgets.reap_millis,
        ]
    }

    pub(super) fn stdin_limit(&self) -> usize {
        self.io_limits.stdin_bytes
    }

    pub(super) fn stdout_limit(&self) -> usize {
        self.io_limits.stdout_bytes
    }

    pub(super) fn stderr_limit(&self) -> usize {
        self.io_limits.stderr_bytes
    }

    pub(super) fn frame_limit(&self) -> usize {
        self.io_limits.frame_bytes
    }
}

impl PreparedChildLaunchV1 {
    pub(super) fn containment_token(&self) -> &PreparedChildContainmentTokenV1 {
        &self.containment_token
    }

    /// Exposes only the already-authorized phase budgets to the private
    /// supervisor. Callers cannot replace them or reconstruct a launch proof.
    pub(super) fn supervision_phase_budgets_millis(&self) -> [u64; 5] {
        [
            self.runtime_policy.phase_budgets.startup_millis,
            self.runtime_policy.phase_budgets.request_millis,
            self.runtime_policy.phase_budgets.drain_millis,
            self.runtime_policy.phase_budgets.kill_millis,
            self.runtime_policy.phase_budgets.reap_millis,
        ]
    }

    /// Consumes the one-shot preparation proof into the exact private launch
    /// values. The runtime cannot accept caller-supplied replacements.
    pub(super) fn into_os_launch(self) -> PreparedChildOsLaunchV1 {
        let Self {
            canonical_executable,
            executable_sha256,
            arguments,
            working_directory,
            runtime_policy,
            containment_token,
        } = self;
        PreparedChildOsLaunchV1 {
            executable: canonical_executable,
            executable_sha256,
            arguments,
            working_directory,
            phase_budgets: runtime_policy.phase_budgets,
            io_limits: runtime_policy.io_limits,
            environment: runtime_policy.environment,
            filesystem: runtime_policy.filesystem,
            containment_token,
        }
    }
}

pub(crate) fn admit_child_launch_v1(
    request: ChildLaunchRequestV1,
    decision: ChildLaunchAuthorityDecisionV1<'_>,
) -> Result<AdmittedChildLaunchV1, ChildLaunchAdmissionErrorV1> {
    // Disabled and denied modes intentionally precede version and syntax
    // inspection. An inert entry cannot cause executable discovery merely by
    // containing malformed or future-version fields.
    if request.mode == ChildLaunchModeV1::Disabled {
        return Err(ChildLaunchAdmissionErrorV1::Disabled);
    }
    let profile = match decision {
        ChildLaunchAuthorityDecisionV1::Deny => {
            return Err(ChildLaunchAdmissionErrorV1::AuthorityDenied)
        }
        ChildLaunchAuthorityDecisionV1::Admit(profile) => profile,
    };
    if request.version != CHILD_LAUNCH_REQUEST_VERSION_V1 {
        return Err(ChildLaunchAdmissionErrorV1::UnsupportedVersion {
            observed: request.version,
        });
    }
    let requested = ValidatedChildLaunchSpecV1::new(
        request.executable,
        request.executable_sha256,
        request.arguments,
        request.working_directory,
        request.runtime_policy,
    )?;
    if requested.executable != profile.spec.executable {
        return Err(ChildLaunchAdmissionErrorV1::AuthorityMismatch(
            AuthorityMismatchFieldV1::Executable,
        ));
    }
    if requested.executable_sha256 != profile.spec.executable_sha256 {
        return Err(ChildLaunchAdmissionErrorV1::AuthorityMismatch(
            AuthorityMismatchFieldV1::ExecutableSha256,
        ));
    }
    if requested.arguments != profile.spec.arguments {
        return Err(ChildLaunchAdmissionErrorV1::AuthorityMismatch(
            AuthorityMismatchFieldV1::Arguments,
        ));
    }
    if requested.working_directory != profile.spec.working_directory {
        return Err(ChildLaunchAdmissionErrorV1::AuthorityMismatch(
            AuthorityMismatchFieldV1::WorkingDirectory,
        ));
    }
    if requested.runtime_policy != profile.spec.runtime_policy {
        return Err(ChildLaunchAdmissionErrorV1::AuthorityMismatch(
            AuthorityMismatchFieldV1::RuntimePolicy,
        ));
    }
    Ok(AdmittedChildLaunchV1 { spec: requested })
}

pub(crate) fn prepare_admitted_child_launch_v1<P>(
    admitted: AdmittedChildLaunchV1,
    preparation: &mut P,
) -> Result<PreparedChildLaunchV1, ChildLaunchAdmissionErrorV1>
where
    P: ChildLaunchPreparationV1,
{
    let canonical_executable = preparation
        .canonicalize_executable(&admitted.spec.executable)
        .map_err(ChildLaunchAdmissionErrorV1::ExecutableCanonicalization)?;
    if canonical_executable != admitted.spec.executable {
        return Err(ChildLaunchAdmissionErrorV1::ExecutablePathChanged);
    }
    let observed_sha256 = preparation
        .executable_sha256(&canonical_executable)
        .map_err(ChildLaunchAdmissionErrorV1::ExecutableHash)?;
    if observed_sha256 != admitted.spec.executable_sha256.0 {
        return Err(ChildLaunchAdmissionErrorV1::ExecutableDigestMismatch);
    }
    let containment_token = preparation
        .containment_preflight(&admitted)
        .map_err(ChildLaunchAdmissionErrorV1::ContainmentPreflight)?;

    Ok(PreparedChildLaunchV1 {
        canonical_executable,
        executable_sha256: admitted.spec.executable_sha256,
        arguments: admitted.spec.arguments,
        working_directory: admitted.spec.working_directory,
        runtime_policy: admitted.spec.runtime_policy,
        containment_token,
    })
}

pub(crate) fn prepare_child_launch_request_v1<P>(
    request: ChildLaunchRequestV1,
    decision: ChildLaunchAuthorityDecisionV1<'_>,
    preparation: &mut P,
) -> Result<PreparedChildLaunchV1, ChildLaunchAdmissionErrorV1>
where
    P: ChildLaunchPreparationV1,
{
    let admitted = admit_child_launch_v1(request, decision)?;
    prepare_admitted_child_launch_v1(admitted, preparation)
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXECUTABLE: &str = "/opt/kitsu-fixtures/stdio-peer";
    const WORKING_DIRECTORY: &str = "/tmp/kitsu-external";
    const DIGEST: &str = "abababababababababababababababababababababababababababababababab";

    fn digest_bytes() -> [u8; 32] {
        [0xab; 32]
    }

    fn runtime_policy() -> ChildRuntimePolicyV1 {
        ChildRuntimePolicyV1::new(
            ChildPhaseBudgetsV1::new(5_000, 30_000, 5_000, 2_000, 5_000),
            ChildIoLimitsV1::new(64 * 1024, 256 * 1024, 64 * 1024, 32 * 1024),
            ChildProcessGroupPolicyV1::Owned,
            ChildEnvironmentPolicyV1::new(
                true,
                vec![
                    ChildEnvironmentVariableV1::new("LANG", "C.UTF-8"),
                    ChildEnvironmentVariableV1::new("KITSU_EXTERNAL_TOKEN", "fixture-secret"),
                ],
            ),
            filesystem_policy(),
            ChildNetworkPolicyV1::Deny,
            ChildTimeoutDispositionV1::TerminateProcessGroupAndReap,
        )
    }

    fn filesystem_policy() -> ChildFilesystemPolicyV1 {
        ChildFilesystemPolicyV1::new(vec![
            ChildFilesystemGrantV1::new("/opt/kitsu-fixtures", ChildFilesystemAccessV1::ReadOnly),
            ChildFilesystemGrantV1::new("/tmp/kitsu-external", ChildFilesystemAccessV1::ReadWrite),
        ])
    }

    fn valid_request_with_policy(policy: ChildRuntimePolicyV1) -> ChildLaunchRequestV1 {
        ChildLaunchRequestV1::new(
            CHILD_LAUNCH_REQUEST_VERSION_V1,
            ChildLaunchModeV1::Enabled,
            EXECUTABLE,
            DIGEST,
            vec!["--stdio".to_string()],
            WORKING_DIRECTORY,
            policy,
        )
    }

    fn valid_request() -> ChildLaunchRequestV1 {
        valid_request_with_policy(runtime_policy())
    }

    fn profile_with_policy(
        policy: ChildRuntimePolicyV1,
    ) -> Result<AuthorityProfileV1, ChildLaunchAdmissionErrorV1> {
        AuthorityProfileV1::exact(
            EXECUTABLE,
            DIGEST,
            vec!["--stdio".to_string()],
            WORKING_DIRECTORY,
            policy,
        )
    }

    fn profile() -> AuthorityProfileV1 {
        profile_with_policy(runtime_policy()).unwrap()
    }

    #[derive(Default)]
    struct EffectProbe {
        canonicalizations: usize,
        hashes: usize,
        preflights: usize,
        spawns: usize,
        canonical_path: Option<PathBuf>,
        observed_digest: Option<[u8; 32]>,
        canonicalization_error: Option<String>,
        hash_error: Option<String>,
        preflight_error: Option<String>,
    }

    impl EffectProbe {
        fn successful() -> Self {
            Self {
                canonical_path: Some(PathBuf::from(EXECUTABLE)),
                observed_digest: Some(digest_bytes()),
                ..Self::default()
            }
        }

        fn assert_zero_effects(&self) {
            assert_eq!(self.canonicalizations, 0);
            assert_eq!(self.hashes, 0);
            assert_eq!(self.preflights, 0);
            assert_eq!(self.spawns, 0);
        }
    }

    impl ChildLaunchPreparationV1 for EffectProbe {
        fn canonicalize_executable(&mut self, _: &Path) -> Result<PathBuf, String> {
            self.canonicalizations += 1;
            match self.canonicalization_error.take() {
                Some(error) => Err(error),
                None => Ok(self
                    .canonical_path
                    .clone()
                    .expect("test canonical path is configured")),
            }
        }

        fn executable_sha256(&mut self, _: &Path) -> Result<[u8; 32], String> {
            self.hashes += 1;
            match self.hash_error.take() {
                Some(error) => Err(error),
                None => Ok(self.observed_digest.expect("test digest is configured")),
            }
        }

        fn containment_preflight(
            &mut self,
            _: &AdmittedChildLaunchV1,
        ) -> Result<PreparedChildContainmentTokenV1, String> {
            self.preflights += 1;
            match self.preflight_error.take() {
                Some(error) => Err(error),
                None => Ok(PreparedChildContainmentTokenV1::new(7)),
            }
        }
    }

    fn assert_rejected_without_effects(
        request: ChildLaunchRequestV1,
        decision: ChildLaunchAuthorityDecisionV1<'_>,
        expected: ChildLaunchAdmissionErrorV1,
    ) {
        let mut probe = EffectProbe::successful();
        assert_eq!(
            prepare_child_launch_request_v1(request, decision, &mut probe),
            Err(expected)
        );
        probe.assert_zero_effects();
    }

    #[test]
    fn disabled_request_is_inert_even_when_every_other_field_is_invalid() {
        let mut invalid_policy = runtime_policy();
        invalid_policy.phase_budgets.startup_millis = 0;
        let request = ChildLaunchRequestV1::new(
            99,
            ChildLaunchModeV1::Disabled,
            "stdio-peer",
            "not-a-digest",
            vec!["bad\0argument".to_string()],
            "relative-cwd",
            invalid_policy,
        );
        let profile = profile();
        assert_rejected_without_effects(
            request,
            ChildLaunchAuthorityDecisionV1::Admit(&profile),
            ChildLaunchAdmissionErrorV1::Disabled,
        );
    }

    #[test]
    fn authority_denial_precedes_version_or_syntax_inspection() {
        let mut invalid_policy = runtime_policy();
        invalid_policy.network = ChildNetworkPolicyV1::Allow;
        let request = ChildLaunchRequestV1::new(
            99,
            ChildLaunchModeV1::Enabled,
            "./stdio-peer",
            "bad",
            Vec::new(),
            "relative-cwd",
            invalid_policy,
        );
        assert_rejected_without_effects(
            request,
            ChildLaunchAuthorityDecisionV1::Deny,
            ChildLaunchAdmissionErrorV1::AuthorityDenied,
        );
    }

    #[test]
    fn unsupported_version_is_rejected_before_preparation() {
        let mut request = valid_request();
        request.version = 2;
        let profile = profile();
        assert_rejected_without_effects(
            request,
            ChildLaunchAuthorityDecisionV1::Admit(&profile),
            ChildLaunchAdmissionErrorV1::UnsupportedVersion { observed: 2 },
        );
    }

    #[test]
    fn path_style_and_non_normalized_executables_are_inert() {
        for executable in [
            "stdio-peer",
            "./stdio-peer",
            "../bin/stdio-peer",
            "/opt/kitsu-fixtures/../stdio-peer",
            "",
        ] {
            let mut request = valid_request();
            request.executable = PathBuf::from(executable);
            let profile = profile();
            assert_rejected_without_effects(
                request,
                ChildLaunchAuthorityDecisionV1::Admit(&profile),
                ChildLaunchAdmissionErrorV1::InvalidExecutablePath,
            );
        }
    }

    #[test]
    fn invalid_digest_forms_are_rejected_before_preparation() {
        for digest in [
            "",
            "ab",
            "ABABABABABABABABABABABABABABABABABABABABABABABABABABABABABAB",
            "gggggggggggggggggggggggggggggggggggggggggggggggggggggggggggggggg",
        ] {
            let mut request = valid_request();
            request.executable_sha256 = digest.to_string();
            let profile = profile();
            assert_rejected_without_effects(
                request,
                ChildLaunchAuthorityDecisionV1::Admit(&profile),
                ChildLaunchAdmissionErrorV1::InvalidExecutableSha256,
            );
        }
    }

    #[test]
    fn malformed_arguments_and_working_directory_are_inert() {
        let profile = profile();
        let mut nul_argument = valid_request();
        nul_argument.arguments = vec!["bad\0argument".to_string()];
        assert_rejected_without_effects(
            nul_argument,
            ChildLaunchAuthorityDecisionV1::Admit(&profile),
            ChildLaunchAdmissionErrorV1::InvalidArguments,
        );

        let mut relative_cwd = valid_request();
        relative_cwd.working_directory = PathBuf::from("relative");
        assert_rejected_without_effects(
            relative_cwd,
            ChildLaunchAuthorityDecisionV1::Admit(&profile),
            ChildLaunchAdmissionErrorV1::InvalidWorkingDirectory,
        );
    }

    #[test]
    fn every_phase_budget_is_nonzero_and_capped_before_preparation() {
        let cases = [
            (
                ChildRuntimePhaseV1::Startup,
                0,
                MAX_STARTUP_BUDGET_MILLIS + 1,
            ),
            (
                ChildRuntimePhaseV1::Request,
                1,
                MAX_REQUEST_BUDGET_MILLIS + 1,
            ),
            (ChildRuntimePhaseV1::Drain, 2, MAX_DRAIN_BUDGET_MILLIS + 1),
            (ChildRuntimePhaseV1::Kill, 3, MAX_KILL_BUDGET_MILLIS + 1),
            (ChildRuntimePhaseV1::Reap, 4, MAX_REAP_BUDGET_MILLIS + 1),
        ];

        for (phase, index, excessive) in cases {
            for observed in [0, excessive] {
                let mut policy = runtime_policy();
                let values = [
                    &mut policy.phase_budgets.startup_millis,
                    &mut policy.phase_budgets.request_millis,
                    &mut policy.phase_budgets.drain_millis,
                    &mut policy.phase_budgets.kill_millis,
                    &mut policy.phase_budgets.reap_millis,
                ];
                *values.into_iter().nth(index).unwrap() = observed;
                let profile = profile();
                assert_rejected_without_effects(
                    valid_request_with_policy(policy),
                    ChildLaunchAuthorityDecisionV1::Admit(&profile),
                    ChildLaunchAdmissionErrorV1::InvalidRuntimePolicy(
                        ChildRuntimePolicyErrorV1::InvalidPhaseBudget(phase),
                    ),
                );
            }
        }
    }

    #[test]
    fn every_io_limit_is_nonzero_and_capped_before_preparation() {
        let cases = [
            (ChildIoLimitV1::Stdin, 0, MAX_STDIN_BYTES + 1),
            (ChildIoLimitV1::Stdout, 1, MAX_STDOUT_BYTES + 1),
            (ChildIoLimitV1::Stderr, 2, MAX_STDERR_BYTES + 1),
            (ChildIoLimitV1::Frame, 3, MAX_FRAME_BYTES + 1),
        ];

        for (limit, index, excessive) in cases {
            for observed in [0, excessive] {
                let mut policy = runtime_policy();
                let values = [
                    &mut policy.io_limits.stdin_bytes,
                    &mut policy.io_limits.stdout_bytes,
                    &mut policy.io_limits.stderr_bytes,
                    &mut policy.io_limits.frame_bytes,
                ];
                *values.into_iter().nth(index).unwrap() = observed;
                let profile = profile();
                assert_rejected_without_effects(
                    valid_request_with_policy(policy),
                    ChildLaunchAuthorityDecisionV1::Admit(&profile),
                    ChildLaunchAdmissionErrorV1::InvalidRuntimePolicy(
                        ChildRuntimePolicyErrorV1::InvalidIoLimit(limit),
                    ),
                );
            }
        }
    }

    #[test]
    fn frame_limit_must_fit_both_protocol_stream_budgets() {
        for stream in [ChildIoLimitV1::Stdin, ChildIoLimitV1::Stdout] {
            let mut policy = runtime_policy();
            policy.io_limits.frame_bytes = 2_048;
            match stream {
                ChildIoLimitV1::Stdin => policy.io_limits.stdin_bytes = 1_024,
                ChildIoLimitV1::Stdout => policy.io_limits.stdout_bytes = 1_024,
                ChildIoLimitV1::Stderr | ChildIoLimitV1::Frame => unreachable!(),
            }
            let profile = profile();
            assert_rejected_without_effects(
                valid_request_with_policy(policy),
                ChildLaunchAuthorityDecisionV1::Admit(&profile),
                ChildLaunchAdmissionErrorV1::InvalidRuntimePolicy(
                    ChildRuntimePolicyErrorV1::FrameExceedsStreamLimit(stream),
                ),
            );
        }
    }

    #[test]
    fn containment_and_cleanup_truths_are_mandatory_before_preparation() {
        let mut inherited_group = runtime_policy();
        inherited_group.process_group = ChildProcessGroupPolicyV1::Inherited;
        let mut inherited_environment = runtime_policy();
        inherited_environment.environment.clear_inherited = false;
        let mut network_allowed = runtime_policy();
        network_allowed.network = ChildNetworkPolicyV1::Allow;
        let mut abandonment = runtime_policy();
        abandonment.timeout_disposition = ChildTimeoutDispositionV1::Abandon;

        for (policy, reason) in [
            (
                inherited_group,
                ChildRuntimePolicyErrorV1::ProcessGroupNotOwned,
            ),
            (
                inherited_environment,
                ChildRuntimePolicyErrorV1::InheritedEnvironmentNotCleared,
            ),
            (network_allowed, ChildRuntimePolicyErrorV1::NetworkNotDenied),
            (abandonment, ChildRuntimePolicyErrorV1::TimeoutMayAbandon),
        ] {
            let profile = profile();
            assert_rejected_without_effects(
                valid_request_with_policy(policy),
                ChildLaunchAuthorityDecisionV1::Admit(&profile),
                ChildLaunchAdmissionErrorV1::InvalidRuntimePolicy(reason),
            );
        }
        assert_eq!(ChildNetworkPolicyV1::default(), ChildNetworkPolicyV1::Deny);
    }

    #[test]
    fn environment_projection_is_explicit_unique_and_bounded() {
        let mut too_many = runtime_policy();
        too_many.environment.projection = (0..=MAX_ENVIRONMENT_VARIABLES)
            .map(|index| ChildEnvironmentVariableV1::new(format!("KITSU_{index}"), "value"))
            .collect();
        let mut duplicate = runtime_policy();
        duplicate
            .environment
            .projection
            .push(ChildEnvironmentVariableV1::new("LANG", "other"));
        let mut invalid_name = runtime_policy();
        invalid_name.environment.projection[0].name = "BAD-NAME".to_string();
        let mut oversized_value = runtime_policy();
        oversized_value.environment.projection[0].value =
            ChildEnvironmentValueV1("x".repeat(MAX_ENVIRONMENT_VALUE_BYTES + 1));
        let mut aggregate_too_large = runtime_policy();
        aggregate_too_large.environment.projection = (0..5)
            .map(|index| {
                ChildEnvironmentVariableV1::new(
                    format!("KITSU_{index}"),
                    "x".repeat(MAX_ENVIRONMENT_VALUE_BYTES),
                )
            })
            .collect();
        let mut nul_value = runtime_policy();
        nul_value.environment.projection[0].value =
            ChildEnvironmentValueV1("before\0after".to_string());

        for policy in [
            too_many,
            duplicate,
            invalid_name,
            oversized_value,
            aggregate_too_large,
            nul_value,
        ] {
            let profile = profile();
            assert_rejected_without_effects(
                valid_request_with_policy(policy),
                ChildLaunchAuthorityDecisionV1::Admit(&profile),
                ChildLaunchAdmissionErrorV1::InvalidRuntimePolicy(
                    ChildRuntimePolicyErrorV1::InvalidEnvironmentProjection,
                ),
            );
        }
    }

    #[test]
    fn host_profile_rejects_an_invalid_runtime_policy_without_observation() {
        let mut policy = runtime_policy();
        policy.phase_budgets.reap_millis = 0;
        assert_eq!(
            profile_with_policy(policy),
            Err(ChildLaunchAdmissionErrorV1::InvalidRuntimePolicy(
                ChildRuntimePolicyErrorV1::InvalidPhaseBudget(ChildRuntimePhaseV1::Reap)
            ))
        );
    }

    #[test]
    fn every_valid_runtime_policy_fact_requires_an_exact_authority_match() {
        let mut changed_budget = runtime_policy();
        changed_budget.phase_budgets.request_millis += 1;
        let mut changed_limit = runtime_policy();
        changed_limit.io_limits.frame_bytes += 1;
        let mut changed_value = runtime_policy();
        changed_value.environment.projection[1].value =
            ChildEnvironmentValueV1("different-secret".to_string());
        let mut changed_order = runtime_policy();
        changed_order.environment.projection.reverse();

        for authority_policy in [changed_budget, changed_limit, changed_value, changed_order] {
            let profile = profile_with_policy(authority_policy).unwrap();
            assert_rejected_without_effects(
                valid_request(),
                ChildLaunchAuthorityDecisionV1::Admit(&profile),
                ChildLaunchAdmissionErrorV1::AuthorityMismatch(
                    AuthorityMismatchFieldV1::RuntimePolicy,
                ),
            );
        }
    }

    #[test]
    fn environment_values_are_redacted_from_all_debug_projections() {
        let request = valid_request();
        let profile = profile();
        let mut probe = EffectProbe::successful();
        let prepared = prepare_child_launch_request_v1(
            request.clone(),
            ChildLaunchAuthorityDecisionV1::Admit(&profile),
            &mut probe,
        )
        .unwrap();

        for projection in [
            format!("{request:?}"),
            format!("{profile:?}"),
            format!("{prepared:?}"),
        ] {
            assert!(projection.contains("KITSU_EXTERNAL_TOKEN"));
            assert!(projection.contains("[REDACTED]"));
            assert!(!projection.contains("fixture-secret"));
        }
    }

    #[test]
    fn every_authority_mismatch_is_inert() {
        let cases = [
            (
                AuthorityProfileV1::exact(
                    "/opt/kitsu-fixtures/other",
                    DIGEST,
                    vec!["--stdio".to_string()],
                    WORKING_DIRECTORY,
                    runtime_policy(),
                )
                .unwrap(),
                AuthorityMismatchFieldV1::Executable,
            ),
            (
                AuthorityProfileV1::exact(
                    EXECUTABLE,
                    "cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd",
                    vec!["--stdio".to_string()],
                    WORKING_DIRECTORY,
                    runtime_policy(),
                )
                .unwrap(),
                AuthorityMismatchFieldV1::ExecutableSha256,
            ),
            (
                AuthorityProfileV1::exact(
                    EXECUTABLE,
                    DIGEST,
                    vec!["--different".to_string()],
                    WORKING_DIRECTORY,
                    runtime_policy(),
                )
                .unwrap(),
                AuthorityMismatchFieldV1::Arguments,
            ),
            (
                AuthorityProfileV1::exact(
                    EXECUTABLE,
                    DIGEST,
                    vec!["--stdio".to_string()],
                    "/tmp/kitsu-external/other",
                    runtime_policy(),
                )
                .unwrap(),
                AuthorityMismatchFieldV1::WorkingDirectory,
            ),
        ];
        for (profile, field) in cases {
            assert_rejected_without_effects(
                valid_request(),
                ChildLaunchAuthorityDecisionV1::Admit(&profile),
                ChildLaunchAdmissionErrorV1::AuthorityMismatch(field),
            );
        }
    }

    #[test]
    fn successful_preparation_observes_exact_identity_without_spawning() {
        let profile = profile();
        let mut probe = EffectProbe::successful();
        let prepared = prepare_child_launch_request_v1(
            valid_request(),
            ChildLaunchAuthorityDecisionV1::Admit(&profile),
            &mut probe,
        )
        .unwrap();

        assert_eq!(prepared.canonical_executable, Path::new(EXECUTABLE));
        assert_eq!(prepared.executable_sha256.0, digest_bytes());
        assert_eq!(prepared.arguments, ["--stdio"]);
        assert_eq!(prepared.working_directory, PathBuf::from(WORKING_DIRECTORY));
        assert_eq!(probe.canonicalizations, 1);
        assert_eq!(probe.hashes, 1);
        assert_eq!(probe.preflights, 1);
        assert_eq!(probe.spawns, 0);
    }

    #[test]
    fn changed_canonical_path_stops_before_hash_and_preflight() {
        let profile = profile();
        let mut probe = EffectProbe {
            canonical_path: Some(PathBuf::from("/different/stdio-peer")),
            observed_digest: Some(digest_bytes()),
            ..EffectProbe::default()
        };
        assert_eq!(
            prepare_child_launch_request_v1(
                valid_request(),
                ChildLaunchAuthorityDecisionV1::Admit(&profile),
                &mut probe,
            ),
            Err(ChildLaunchAdmissionErrorV1::ExecutablePathChanged)
        );
        assert_eq!(probe.canonicalizations, 1);
        assert_eq!(probe.hashes, 0);
        assert_eq!(probe.preflights, 0);
        assert_eq!(probe.spawns, 0);
    }

    #[test]
    fn digest_mismatch_stops_before_containment_preflight() {
        let profile = profile();
        let mut probe = EffectProbe {
            canonical_path: Some(PathBuf::from(EXECUTABLE)),
            observed_digest: Some([0xcd; 32]),
            ..EffectProbe::default()
        };
        assert_eq!(
            prepare_child_launch_request_v1(
                valid_request(),
                ChildLaunchAuthorityDecisionV1::Admit(&profile),
                &mut probe,
            ),
            Err(ChildLaunchAdmissionErrorV1::ExecutableDigestMismatch)
        );
        assert_eq!(probe.canonicalizations, 1);
        assert_eq!(probe.hashes, 1);
        assert_eq!(probe.preflights, 0);
        assert_eq!(probe.spawns, 0);
    }

    #[test]
    fn preparation_failures_do_not_advance_later_effects() {
        let profile = profile();
        let mut canonicalization_failure = EffectProbe {
            canonicalization_error: Some("unavailable".to_string()),
            ..EffectProbe::successful()
        };
        assert_eq!(
            prepare_child_launch_request_v1(
                valid_request(),
                ChildLaunchAuthorityDecisionV1::Admit(&profile),
                &mut canonicalization_failure,
            ),
            Err(ChildLaunchAdmissionErrorV1::ExecutableCanonicalization(
                "unavailable".to_string()
            ))
        );
        assert_eq!(canonicalization_failure.canonicalizations, 1);
        assert_eq!(canonicalization_failure.hashes, 0);
        assert_eq!(canonicalization_failure.preflights, 0);

        let mut hash_failure = EffectProbe {
            hash_error: Some("unreadable".to_string()),
            ..EffectProbe::successful()
        };
        assert_eq!(
            prepare_child_launch_request_v1(
                valid_request(),
                ChildLaunchAuthorityDecisionV1::Admit(&profile),
                &mut hash_failure,
            ),
            Err(ChildLaunchAdmissionErrorV1::ExecutableHash(
                "unreadable".to_string()
            ))
        );
        assert_eq!(hash_failure.canonicalizations, 1);
        assert_eq!(hash_failure.hashes, 1);
        assert_eq!(hash_failure.preflights, 0);

        let mut preflight_failure = EffectProbe {
            preflight_error: Some("containment unavailable".to_string()),
            ..EffectProbe::successful()
        };
        assert_eq!(
            prepare_child_launch_request_v1(
                valid_request(),
                ChildLaunchAuthorityDecisionV1::Admit(&profile),
                &mut preflight_failure,
            ),
            Err(ChildLaunchAdmissionErrorV1::ContainmentPreflight(
                "containment unavailable".to_string()
            ))
        );
        assert_eq!(preflight_failure.canonicalizations, 1);
        assert_eq!(preflight_failure.hashes, 1);
        assert_eq!(preflight_failure.preflights, 1);
        assert_eq!(preflight_failure.spawns, 0);
    }

    #[test]
    fn filesystem_authority_is_mandatory_bounded_exact_and_inert_when_invalid() {
        let too_many_grants = ChildFilesystemPolicyV1::new(
            (0..=MAX_FILESYSTEM_GRANTS)
                .map(|index| {
                    ChildFilesystemGrantV1::new(
                        format!("/reviewed-{index}"),
                        ChildFilesystemAccessV1::ReadOnly,
                    )
                })
                .collect(),
        );
        let oversized_path = ChildFilesystemPolicyV1::new(vec![ChildFilesystemGrantV1::new(
            format!("/{}", "x".repeat(MAX_FILESYSTEM_PATH_BYTES)),
            ChildFilesystemAccessV1::ReadOnly,
        )]);
        let oversized_policy = ChildFilesystemPolicyV1::new(
            (0..MAX_FILESYSTEM_GRANTS)
                .map(|index| {
                    ChildFilesystemGrantV1::new(
                        format!(
                            "/{index}-{}",
                            "x".repeat(MAX_FILESYSTEM_POLICY_BYTES / MAX_FILESYSTEM_GRANTS)
                        ),
                        ChildFilesystemAccessV1::ReadOnly,
                    )
                })
                .collect(),
        );
        let invalid = [
            ChildFilesystemPolicyV1::new(Vec::new()),
            ChildFilesystemPolicyV1::new(vec![ChildFilesystemGrantV1::new(
                "/",
                ChildFilesystemAccessV1::ReadOnly,
            )]),
            ChildFilesystemPolicyV1::new(vec![ChildFilesystemGrantV1::new(
                "relative/root",
                ChildFilesystemAccessV1::ReadOnly,
            )]),
            ChildFilesystemPolicyV1::new(vec![ChildFilesystemGrantV1::new(
                "/reviewed/../escape",
                ChildFilesystemAccessV1::ReadOnly,
            )]),
            ChildFilesystemPolicyV1::new(vec![ChildFilesystemGrantV1::new(
                "/reviewed\0escape",
                ChildFilesystemAccessV1::ReadOnly,
            )]),
            ChildFilesystemPolicyV1::new(vec![
                ChildFilesystemGrantV1::new(
                    "/opt/kitsu-fixtures",
                    ChildFilesystemAccessV1::ReadOnly,
                ),
                ChildFilesystemGrantV1::new(
                    "/opt/kitsu-fixtures",
                    ChildFilesystemAccessV1::ReadOnly,
                ),
            ]),
            ChildFilesystemPolicyV1::new(vec![
                ChildFilesystemGrantV1::new(
                    "/opt/kitsu-fixtures",
                    ChildFilesystemAccessV1::ReadOnly,
                ),
                ChildFilesystemGrantV1::new(
                    "/opt/kitsu-fixtures",
                    ChildFilesystemAccessV1::ReadWrite,
                ),
            ]),
            ChildFilesystemPolicyV1::new(vec![
                ChildFilesystemGrantV1::new("/opt", ChildFilesystemAccessV1::ReadOnly),
                ChildFilesystemGrantV1::new(
                    "/opt/kitsu-fixtures",
                    ChildFilesystemAccessV1::ReadWrite,
                ),
            ]),
            ChildFilesystemPolicyV1::new(vec![
                ChildFilesystemGrantV1::new("/var", ChildFilesystemAccessV1::ReadOnly),
                ChildFilesystemGrantV1::new("/var/lib", ChildFilesystemAccessV1::ReadOnly),
            ]),
            too_many_grants,
            oversized_path,
            oversized_policy,
        ];

        for filesystem in invalid {
            let mut policy = runtime_policy();
            policy.filesystem = filesystem;
            let mut probe = EffectProbe::successful();
            assert!(matches!(
                prepare_child_launch_request_v1(
                    valid_request_with_policy(policy),
                    ChildLaunchAuthorityDecisionV1::Admit(&profile()),
                    &mut probe,
                ),
                Err(ChildLaunchAdmissionErrorV1::InvalidRuntimePolicy(
                    ChildRuntimePolicyErrorV1::InvalidFilesystemPolicy
                ))
            ));
            probe.assert_zero_effects();
        }
    }

    #[test]
    fn cwd_must_be_covered_by_an_exact_reviewed_filesystem_grant() {
        let policy = ChildRuntimePolicyV1::new(
            ChildPhaseBudgetsV1::new(5_000, 30_000, 5_000, 2_000, 5_000),
            ChildIoLimitsV1::new(64 * 1024, 256 * 1024, 64 * 1024, 32 * 1024),
            ChildProcessGroupPolicyV1::Owned,
            ChildEnvironmentPolicyV1::new(true, Vec::new()),
            ChildFilesystemPolicyV1::new(vec![ChildFilesystemGrantV1::new(
                "/opt/kitsu-fixtures",
                ChildFilesystemAccessV1::ReadOnly,
            )]),
            ChildNetworkPolicyV1::Deny,
            ChildTimeoutDispositionV1::TerminateProcessGroupAndReap,
        );
        let mut probe = EffectProbe::successful();
        assert_eq!(
            prepare_child_launch_request_v1(
                valid_request_with_policy(policy),
                ChildLaunchAuthorityDecisionV1::Admit(&profile()),
                &mut probe,
            ),
            Err(ChildLaunchAdmissionErrorV1::WorkingDirectoryOutsideFilesystemPolicy)
        );
        probe.assert_zero_effects();
    }

    #[test]
    fn every_filesystem_grant_fact_requires_exact_authority_match_before_observation() {
        let mut changed_access = runtime_policy();
        changed_access.filesystem.grants[1].access = ChildFilesystemAccessV1::ReadOnly;
        let mut changed_path = runtime_policy();
        changed_path.filesystem.grants[0].path = PathBuf::from("/opt/other-runtime");
        let mut changed_order = runtime_policy();
        changed_order.filesystem.grants.reverse();

        for authority_policy in [changed_access, changed_path, changed_order] {
            let profile = profile_with_policy(authority_policy).unwrap();
            assert_rejected_without_effects(
                valid_request(),
                ChildLaunchAuthorityDecisionV1::Admit(&profile),
                ChildLaunchAdmissionErrorV1::AuthorityMismatch(
                    AuthorityMismatchFieldV1::RuntimePolicy,
                ),
            );
        }
    }

    #[test]
    fn successful_preparation_binds_one_linear_containment_token_into_the_proof() {
        let profile = profile();
        let mut probe = EffectProbe::successful();
        let prepared = prepare_child_launch_request_v1(
            valid_request(),
            ChildLaunchAuthorityDecisionV1::Admit(&profile),
            &mut probe,
        )
        .unwrap();

        assert_eq!(prepared.containment_token.id_for_test(), 7);
        assert_eq!(probe.preflights, 1);
        let launch = prepared.into_os_launch();
        assert_eq!(launch.containment_token_for_test().id_for_test(), 7);
        assert_eq!(launch.filesystem_for_test(), &filesystem_policy());
    }
}
