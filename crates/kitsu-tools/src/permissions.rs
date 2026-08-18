//! Runtime filesystem permissions (design §7.1, Claude-Code-style approval).
//!
//! Reads and writes are gated by separate deny-by-default allowlists. Unlike a
//! static policy, an operation on a not-yet-allowed path is resolved at runtime
//! by the [`Mode`]:
//!
//! - [`Mode::Ask`] — consult a [`Prompter`] (interactive approval; "always" can
//!   widen the session allowlist).
//! - [`Mode::Scoped`] — never prompt and deny anything outside the current
//!   roots (`--yes`).
//! - [`Mode::AutoAllow`] — allow every non-protected path without prompting;
//!   reserved for an explicitly dangerous global bypass.
//! - [`Mode::Deny`] — deny anything not pre-granted (non-interactive default).
//!
//! Containment is enforced by canonicalizing paths (the parent, for writes to a
//! not-yet-existing file) before the `starts_with` check, defeating `..`/symlink
//! escapes. The allowlists live behind `RwLock` so `/allow` and an "always"
//! approval can widen them mid-session.

#[cfg(unix)]
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use crate::error::{ToolError, ToolInterruption};

/// Default per-tool output cap (bytes); larger tool results are truncated.
pub const DEFAULT_MAX_BYTES: usize = 64 * 1024;
/// Cap on a single generated file's size (bytes); a larger write is rejected.
pub const DEFAULT_MAX_WRITE_BYTES: usize = 1024 * 1024;

/// Stable identity of one Unix filesystem object while that object exists.
///
/// Hard links have distinct paths but share this pair. Device identity is
/// included because inode numbers are only unique within one filesystem.
/// Directory hard links are intentionally out of scope: ordinary Unix
/// filesystems prohibit them, and this policy records regular files only.
#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct ProtectedFileIdentity {
    device: u64,
    inode: u64,
}

#[cfg(unix)]
fn protected_file_identity(metadata: &std::fs::Metadata) -> Option<ProtectedFileIdentity> {
    use std::os::unix::fs::MetadataExt;

    metadata.is_file().then_some(ProtectedFileIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

/// Add every existing regular file below `path` without following symlinks.
/// Missing entries are benign because protected state can be created lazily.
#[cfg(unix)]
fn collect_protected_file_identities(
    path: &Path,
    identities: &mut HashSet<ProtectedFileIdentity>,
    multiply_linked: &mut HashSet<ProtectedFileIdentity>,
) -> Result<(), std::io::Error> {
    use std::os::unix::fs::MetadataExt;

    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            return Ok(());
        }
        Err(error) => return Err(error),
    };
    if metadata.file_type().is_symlink() {
        return Ok(());
    }
    if let Some(identity) = protected_file_identity(&metadata) {
        identities.insert(identity);
        if metadata.nlink() > 1 {
            multiply_linked.insert(identity);
        }
        return Ok(());
    }
    if !metadata.is_dir() {
        return Ok(());
    }
    for entry in std::fs::read_dir(path)? {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        collect_protected_file_identities(&entry.path(), identities, multiply_linked)?;
    }
    Ok(())
}

#[cfg(unix)]
fn refresh_protected_file_identities(
    roots: &RwLock<Vec<PathBuf>>,
    identities: &RwLock<HashSet<ProtectedFileIdentity>>,
    multiply_linked: &RwLock<HashSet<ProtectedFileIdentity>>,
) -> Result<(), ToolError> {
    let roots = roots.read().unwrap().clone();
    let mut discovered = HashSet::new();
    let mut discovered_multiply_linked = HashSet::new();
    for root in &roots {
        collect_protected_file_identities(root, &mut discovered, &mut discovered_multiply_linked)
            .map_err(|error| {
            ToolError::PolicyDenied(format!(
                "cannot safely inspect Kitsu-private path '{}': {error}",
                root.display()
            ))
        })?;
    }
    identities.write().unwrap().extend(discovered);
    multiply_linked
        .write()
        .unwrap()
        .extend(discovered_multiply_linked);
    Ok(())
}

#[cfg(unix)]
fn path_has_protected_file_identity(
    path: &Path,
    identities: &RwLock<HashSet<ProtectedFileIdentity>>,
) -> bool {
    std::fs::metadata(path)
        .ok()
        .and_then(|metadata| protected_file_identity(&metadata))
        .is_some_and(|identity| identities.read().unwrap().contains(&identity))
}

#[cfg(unix)]
fn path_has_multiply_linked_protected_identity(
    path: &Path,
    identities: &RwLock<HashSet<ProtectedFileIdentity>>,
) -> bool {
    path_has_protected_file_identity(path, identities)
}

#[cfg(unix)]
fn path_is_multiply_linked_regular_file(path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;

    std::fs::metadata(path).is_ok_and(|metadata| metadata.is_file() && metadata.nlink() > 1)
}

/// A read or a write — what the model is asking to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Read,
    Write,
}

impl Action {
    pub fn verb(self) -> &'static str {
        match self {
            Action::Read => "read",
            Action::Write => "write",
        }
    }
}

/// The user's answer to a permission prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Allow this one operation, don't widen the allowlist.
    AllowOnce,
    /// Allow and add the target's directory to the session allowlist.
    AllowAlways,
    /// Refuse; the tool returns a policy-denied error fed back to the model.
    Deny,
    /// Authority input closed before a decision; interrupt the current run.
    Interrupt,
}

/// How not-yet-allowed operations are handled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Prompt the user (interactive TTY).
    Ask,
    /// Never prompt; rely exclusively on explicit and current session roots.
    Scoped,
    /// Auto-allow every non-protected operation; never prompt.
    AutoAllow,
    /// Deny anything not pre-granted; never prompt (non-interactive default).
    Deny,
}

/// What the model wants to do, handed to the [`Prompter`] for a decision.
#[derive(Debug, Clone)]
pub struct PermissionRequest {
    pub action: Action,
    /// Resolved absolute path of the target (for display).
    pub path: String,
    /// Directory that an "always" decision would grant.
    pub grant_dir: String,
}

/// One exact regular-file read authorized by [`Permissions`].
///
/// On Unix this captures the device/inode selected at authorization time. The
/// caller must open the returned path without following symlinks and then call
/// [`Self::validate_opened_file`] before consuming any bytes. This closes the
/// final pathname check-to-open window without forcing the permission layer to
/// own filesystem I/O or leak an open descriptor across abstractions.
#[derive(Debug, Clone)]
pub struct ReadAuthorization {
    apparent: PathBuf,
    path: PathBuf,
    #[cfg(unix)]
    identity: ProtectedFileIdentity,
}

/// Host-only authorization for one exact implicit-input directory snapshot.
///
/// This is intentionally separate from model-visible read roots. On Unix the
/// token records only the single-link regular-file identities present below a
/// no-symlink root when it is issued. Callers still open every file without
/// following any path component and validate that descriptor before consuming
/// bytes. Files linked in later, multiply linked files, and files explicitly
/// observed as Kitsu-private are rejected.
#[derive(Debug, Clone)]
pub struct TrustedImplicitReadRoot {
    root: PathBuf,
    #[cfg(unix)]
    identities: HashSet<ProtectedFileIdentity>,
}

impl TrustedImplicitReadRoot {
    pub fn path(&self) -> &Path {
        &self.root
    }

    pub fn validate_opened_file(
        &self,
        requested: &Path,
        file: &std::fs::File,
        permissions: &Permissions,
    ) -> Result<(), ToolError> {
        #[cfg(not(unix))]
        {
            let _ = (requested, file, permissions);
            Err(ToolError::PolicyDenied(
                "trusted implicit filesystem inputs are unavailable on this platform because secure no-reparse traversal is not implemented"
                    .to_string(),
            ))
        }

        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;

            let apparent = normalize_apparent(requested)?;
            if apparent == self.root || !apparent.starts_with(&self.root) {
                return Err(ToolError::PolicyDenied(format!(
                    "implicit read target '{}' is outside trusted input root '{}'",
                    requested.display(),
                    self.root.display()
                )));
            }
            let metadata = file.metadata().map_err(|error| {
                ToolError::PolicyDenied(format!(
                    "cannot inspect implicit read target '{}': {error}",
                    requested.display()
                ))
            })?;
            let Some(identity) = protected_file_identity(&metadata) else {
                return Err(ToolError::PolicyDenied(format!(
                    "implicit read target '{}' is not a regular file",
                    requested.display()
                )));
            };
            if metadata.nlink() > 1 {
                return Err(ToolError::PolicyDenied(format!(
                    "implicit read target '{}' has multiple hard links; refusing to ingest an ambiguous filesystem identity",
                    requested.display()
                )));
            }
            if !self.identities.contains(&identity) {
                return Err(ToolError::PolicyDenied(format!(
                    "implicit read target '{}' was not present in the trusted input snapshot",
                    requested.display()
                )));
            }
            if permissions
                .explicitly_observed_read_files
                .read()
                .unwrap()
                .contains(&identity)
            {
                return Err(ToolError::PolicyDenied(format!(
                    "implicit read target '{}' has been explicitly observed as Kitsu-private state",
                    requested.display()
                )));
            }
            Ok(())
        }
    }
}

impl ReadAuthorization {
    /// Canonical path which the caller must open without following symlinks.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Verify that `file` is the exact regular file authorized earlier and that
    /// its identity has not since become Kitsu-private.
    pub fn validate_opened_file(
        &self,
        file: &std::fs::File,
        permissions: &Permissions,
    ) -> Result<(), ToolError> {
        let metadata = file.metadata().map_err(|error| {
            ToolError::PolicyDenied(format!(
                "cannot inspect opened read target '{}': {error}",
                self.path.display()
            ))
        })?;
        if !metadata.is_file() {
            return Err(ToolError::PolicyDenied(format!(
                "read target '{}' is not a regular file",
                self.path.display()
            )));
        }
        #[cfg(unix)]
        if protected_file_identity(&metadata) != Some(self.identity) {
            return Err(ToolError::PolicyDenied(format!(
                "read target '{}' changed after authorization; retry the read",
                self.path.display()
            )));
        }
        permissions.ensure_opened_read_allowed(&self.apparent, &self.path, &metadata, true)
    }
}

/// Something that can ask the user to approve an operation. Implemented by the
/// CLI (prints a prompt and reads a keypress); the tools crate stays UI-free.
pub trait Prompter: Send + Sync {
    fn ask(&self, req: &PermissionRequest) -> Decision;
}

/// Runtime read/write permissions.
pub struct Permissions {
    base_read_roots: Vec<PathBuf>,
    base_write_roots: Vec<PathBuf>,
    /// Invocation roots still active in this session. A user can revoke one
    /// without mutating the immutable base snapshot restored on session reset.
    read_roots: RwLock<Vec<PathBuf>>,
    write_roots: RwLock<Vec<PathBuf>>,
    /// Grants added by prompts and slash commands after construction. Keeping
    /// these separate makes effective authority and provenance inspectable.
    runtime_read_roots: RwLock<Vec<PathBuf>>,
    runtime_write_roots: RwLock<Vec<PathBuf>>,
    protected_read_roots: RwLock<Vec<PathBuf>>,
    /// Existing regular files under protected read roots, keyed by Unix
    /// device/inode so a hard-link alias cannot shed Kitsu-private status.
    /// Identities are retained for this Permissions lifetime after names are
    /// removed; an inode recycled unusually quickly can therefore cause a
    /// conservative denial rather than exposing a former private alias.
    #[cfg(unix)]
    protected_read_files: RwLock<HashSet<ProtectedFileIdentity>>,
    /// Files present when a protected root is registered, explicitly observed
    /// as private later, or discovered with multiple names remain non-readable
    /// through an internal-read subtree. This prevents a private one-link file
    /// from being hard-linked into an artifact exception and having its
    /// original name removed before authorization.
    #[cfg(unix)]
    protected_read_multiply_linked_files: RwLock<HashSet<ProtectedFileIdentity>>,
    /// Files explicitly committed or re-observed by the Kitsu host after
    /// policy construction. Trusted implicit-root snapshots never override
    /// this classification.
    #[cfg(unix)]
    explicitly_observed_read_files: RwLock<HashSet<ProtectedFileIdentity>>,
    internal_read_roots: RwLock<Vec<PathBuf>>,
    protected_write_roots: RwLock<Vec<PathBuf>>,
    /// Write-side counterpart to `protected_read_files`.
    #[cfg(unix)]
    protected_write_files: RwLock<HashSet<ProtectedFileIdentity>>,
    #[cfg(unix)]
    protected_write_multiply_linked_files: RwLock<HashSet<ProtectedFileIdentity>>,
    mode: Mode,
    prompter: Option<Arc<dyn Prompter>>,
    max_bytes: usize,
    max_write_bytes: usize,
}

/// Which user-controlled grant sources were removed by a revoke operation.
/// Protected and Kitsu-internal roots are deliberately not revocable through
/// this API.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Revocation {
    pub invocation: bool,
    pub runtime: bool,
}

impl Revocation {
    pub fn description(self) -> &'static str {
        match (self.invocation, self.runtime) {
            (true, true) => "invocation and session",
            (true, false) => "invocation",
            (false, true) => "session",
            (false, false) => "none",
        }
    }
}

fn canonicalize_roots(roots: &[String], label: &str) -> Result<Vec<PathBuf>, ToolError> {
    let mut out = Vec::with_capacity(roots.len());
    for r in roots {
        let canon = std::fs::canonicalize(r).map_err(|e| {
            ToolError::PolicyDenied(format!("{label} '{r}' is not accessible: {e}"))
        })?;
        insert_minimal_root(&mut out, canon);
    }
    Ok(out)
}

/// Resolve an intended private path even when its final components have not
/// been created yet.  Canonicalizing the nearest existing ancestor pins the
/// protection to the same namespace identity used later by `check_write`.
///
/// This differs deliberately from an allow-root: grants must already exist so
/// the user cannot accidentally authorize a misspelled location, while Kitsu
/// must be able to reserve its own state directory before a background task or
/// a later session save creates it.
fn canonicalize_intended(path: &Path) -> Result<PathBuf, ToolError> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|error| {
                ToolError::PolicyDenied(format!(
                    "private path '{}' cannot be resolved: {error}",
                    path.display()
                ))
            })?
            .join(path)
    };
    let mut cursor = absolute.as_path();
    let mut missing = Vec::new();
    loop {
        match std::fs::canonicalize(cursor) {
            Ok(mut resolved) => {
                for component in missing.iter().rev() {
                    resolved.push(component);
                }
                return Ok(resolved);
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                ) =>
            {
                let name = cursor.file_name().ok_or_else(|| {
                    ToolError::PolicyDenied(format!(
                        "private path '{}' has no accessible ancestor",
                        path.display()
                    ))
                })?;
                missing.push(name.to_os_string());
                cursor = cursor.parent().ok_or_else(|| {
                    ToolError::PolicyDenied(format!(
                        "private path '{}' has no accessible ancestor",
                        path.display()
                    ))
                })?;
            }
            Err(error) => {
                return Err(ToolError::PolicyDenied(format!(
                    "private path '{}' is not accessible: {error}",
                    path.display()
                )));
            }
        }
    }
}

/// Absolute lexical identity of a request, without following symlinks. This is
/// retained alongside the resolved identity so a future protected path cannot
/// be replaced by a symlink and thereby shed its apparent private prefix.
fn normalize_apparent(path: &Path) -> Result<PathBuf, ToolError> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|error| {
                ToolError::PolicyDenied(format!(
                    "path '{}' cannot be resolved: {error}",
                    path.display()
                ))
            })?
            .join(path)
    };
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    Ok(normalized)
}

/// Insert one canonical root while keeping the set minimal. A parent grant
/// already covers its descendants; a newly-added parent replaces child grants.
fn insert_minimal_root(roots: &mut Vec<PathBuf>, root: PathBuf) {
    if roots.iter().any(|existing| root.starts_with(existing)) {
        return;
    }
    roots.retain(|existing| !existing.starts_with(&root));
    roots.push(root);
    roots.sort();
}

fn merged_roots(invocation: &[PathBuf], runtime: &[PathBuf]) -> Vec<PathBuf> {
    let mut roots = invocation.to_vec();
    for root in runtime {
        insert_minimal_root(&mut roots, root.clone());
    }
    roots
}

/// Canonical identity for revocation. Revocation is allowed to target a root
/// that has since disappeared because removing authority is always safe. If a
/// path still exists, canonicalization preserves the identity used at grant
/// time; otherwise the absolute lexical identity can match a displayed root.
fn revocation_target(path: &str) -> Result<PathBuf, ToolError> {
    match std::fs::canonicalize(path) {
        Ok(canonical) => Ok(canonical),
        Err(_) => normalize_apparent(Path::new(path)),
    }
}

fn remove_exact_root(roots: &RwLock<Vec<PathBuf>>, target: &Path) -> bool {
    let mut roots = roots.write().unwrap();
    let before = roots.len();
    roots.retain(|root| root != target);
    roots.len() != before
}

/// The directory an "always" grant should add for a resolved target path: the
/// path itself if it is a directory, otherwise its parent.
fn grant_dir_for(canon: &Path) -> PathBuf {
    if canon.is_dir() {
        canon.to_path_buf()
    } else {
        canon
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| canon.to_path_buf())
    }
}

impl Permissions {
    /// Build permissions from read/write allow roots and a mode. Roots are
    /// canonicalized now (a missing/invalid root is rejected).
    pub fn new(
        read_roots: &[String],
        write_roots: &[String],
        mode: Mode,
    ) -> Result<Self, ToolError> {
        let read_roots = canonicalize_roots(read_roots, "allow-path")?;
        let write_roots = canonicalize_roots(write_roots, "allow-write")?;
        Ok(Self {
            base_read_roots: read_roots.clone(),
            base_write_roots: write_roots.clone(),
            read_roots: RwLock::new(read_roots),
            write_roots: RwLock::new(write_roots),
            runtime_read_roots: RwLock::new(Vec::new()),
            runtime_write_roots: RwLock::new(Vec::new()),
            protected_read_roots: RwLock::new(Vec::new()),
            #[cfg(unix)]
            protected_read_files: RwLock::new(HashSet::new()),
            #[cfg(unix)]
            protected_read_multiply_linked_files: RwLock::new(HashSet::new()),
            #[cfg(unix)]
            explicitly_observed_read_files: RwLock::new(HashSet::new()),
            internal_read_roots: RwLock::new(Vec::new()),
            protected_write_roots: RwLock::new(Vec::new()),
            #[cfg(unix)]
            protected_write_files: RwLock::new(HashSet::new()),
            #[cfg(unix)]
            protected_write_multiply_linked_files: RwLock::new(HashSet::new()),
            mode,
            prompter: None,
            max_bytes: DEFAULT_MAX_BYTES,
            max_write_bytes: DEFAULT_MAX_WRITE_BYTES,
        })
    }

    /// Attach a prompter (used with [`Mode::Ask`]).
    pub fn with_prompter(mut self, prompter: Arc<dyn Prompter>) -> Self {
        self.prompter = Some(prompter);
        self
    }

    /// Test/convenience constructor: fixed roots, `Deny` mode, no prompter.
    pub fn for_roots(read_roots: &[String], write_roots: &[String]) -> Result<Self, ToolError> {
        Self::new(read_roots, write_roots, Mode::Deny)
    }

    pub fn max_bytes(&self) -> usize {
        self.max_bytes
    }

    /// Byte ceiling for write/edit operations, including the pre-image snapshot
    /// the journal reads before a mutation. Larger than [`Self::max_bytes`] (the
    /// read cap) so that editing a source file is not bounded by the much
    /// smaller read window — a 64 KiB read cap must not make a 129 KiB file
    /// uneditable. Host-side postconditions use the same bound when
    /// fingerprinting declared output targets, so verification cannot read
    /// more than the model could write.
    pub fn max_write_bytes(&self) -> usize {
        self.max_write_bytes
    }

    /// Current fallback policy for paths not already covered by a session root.
    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// Snapshot the minimal union of invocation and runtime read roots.
    pub fn read_roots(&self) -> Vec<PathBuf> {
        merged_roots(
            &self.read_roots.read().unwrap(),
            &self.runtime_read_roots.read().unwrap(),
        )
    }

    /// Snapshot the minimal union of invocation and runtime write roots.
    pub fn write_roots(&self) -> Vec<PathBuf> {
        merged_roots(
            &self.write_roots.read().unwrap(),
            &self.runtime_write_roots.read().unwrap(),
        )
    }

    /// Immutable invocation read roots. A session revoke may make one inactive;
    /// [`Self::reset_runtime_grants`] restores this snapshot.
    pub fn base_read_roots(&self) -> Vec<PathBuf> {
        self.base_read_roots.clone()
    }

    /// Immutable invocation write roots.
    pub fn base_write_roots(&self) -> Vec<PathBuf> {
        self.base_write_roots.clone()
    }

    /// Invocation read roots that remain active in the current session.
    pub fn active_base_read_roots(&self) -> Vec<PathBuf> {
        self.read_roots.read().unwrap().clone()
    }

    /// Invocation write roots that remain active in the current session.
    pub fn active_base_write_roots(&self) -> Vec<PathBuf> {
        self.write_roots.read().unwrap().clone()
    }

    /// Read roots granted by prompts or slash commands in this session.
    pub fn runtime_read_roots(&self) -> Vec<PathBuf> {
        self.runtime_read_roots.read().unwrap().clone()
    }

    /// Write roots granted by prompts or slash commands in this session.
    pub fn runtime_write_roots(&self) -> Vec<PathBuf> {
        self.runtime_write_roots.read().unwrap().clone()
    }

    /// Paths containing Kitsu's own configuration/transcripts. Except for an
    /// exact internal read root, these remain unreadable to model tools even
    /// under a broader allow root or auto-approval.
    pub fn protect_read_path(&self, path: &str) -> Result<PathBuf, ToolError> {
        let requested = Path::new(path);
        let apparent = normalize_apparent(requested)?;
        let canon = canonicalize_intended(requested)?;
        {
            let mut roots = self.protected_read_roots.write().unwrap();
            insert_minimal_root(&mut roots, apparent);
            insert_minimal_root(&mut roots, canon.clone());
        }
        #[cfg(unix)]
        {
            // Internal exceptions are granted only to fresh runtime artifact
            // generations. Every file already present when private state is
            // registered is therefore strict private state, even if it has a
            // single link at this instant. Later refreshes remain narrower and
            // add only multiply-linked identities so newly generated artifacts
            // do not lose their intended read exception.
            let roots = self.protected_read_roots.read().unwrap().clone();
            let mut discovered = HashSet::new();
            let mut multiply_linked = HashSet::new();
            for root in &roots {
                collect_protected_file_identities(root, &mut discovered, &mut multiply_linked)
                    .map_err(|error| {
                        ToolError::PolicyDenied(format!(
                            "cannot safely inspect Kitsu-private path '{}': {error}",
                            root.display()
                        ))
                    })?;
            }
            self.protected_read_files
                .write()
                .unwrap()
                .extend(discovered.iter().copied());
            self.protected_read_multiply_linked_files
                .write()
                .unwrap()
                .extend(discovered);
        }
        Ok(canon)
    }

    pub fn protected_read_roots(&self) -> Vec<PathBuf> {
        self.protected_read_roots.read().unwrap().clone()
    }

    /// Allow model reads inside one exact, trusted subtree of an existing
    /// protected parent for the current session. This is reserved for
    /// Kitsu-owned integrations such as generated artifacts and trusted skill
    /// assets; ordinary read grants can never create this exception.
    ///
    /// Both paths are compared after canonicalization, so a symlink that merely
    /// appears beneath a protected parent cannot grant access somewhere else.
    /// The protected parent itself cannot be registered as an internal root.
    pub fn allow_internal_read_root(&self, path: &str) -> Result<PathBuf, ToolError> {
        let canon = std::fs::canonicalize(path).map_err(|e| {
            ToolError::PolicyDenied(format!(
                "internal read path '{path}' is not accessible: {e}"
            ))
        })?;
        let is_strict_child = self
            .protected_read_roots
            .read()
            .unwrap()
            .iter()
            .any(|root| canon.as_path() != root.as_path() && canon.starts_with(root));
        if !is_strict_child {
            return Err(ToolError::PolicyDenied(format!(
                "internal read path '{path}' is not beneath a protected parent"
            )));
        }
        insert_minimal_root(
            &mut self.internal_read_roots.write().unwrap(),
            canon.clone(),
        );
        Ok(canon)
    }

    /// Snapshot the canonical trusted subtrees that may be read through an
    /// otherwise protected parent.
    pub fn internal_read_roots(&self) -> Vec<PathBuf> {
        self.internal_read_roots.read().unwrap().clone()
    }

    /// Whether either the request's apparent namespace path or its current
    /// resolved destination lies under Kitsu-private state. This intentionally
    /// ignores internal-read exceptions and is used when deciding whether a
    /// trusted Kitsu integration must request that narrower capability.
    pub fn is_within_protected_read_root(&self, path: &Path) -> bool {
        let apparent = normalize_apparent(path).unwrap_or_else(|_| path.to_path_buf());
        let resolved = std::fs::canonicalize(path).unwrap_or_else(|_| apparent.clone());
        let protected_by_path = self
            .protected_read_roots
            .read()
            .unwrap()
            .iter()
            .any(|root| apparent.starts_with(root) || resolved.starts_with(root));
        #[cfg(unix)]
        {
            // A single-link file cannot currently alias another namespace
            // entry. Previously observed identities remain in the set, while
            // multiply-linked candidates trigger discovery of new state files.
            if path_is_multiply_linked_regular_file(&resolved) {
                let _ = refresh_protected_file_identities(
                    &self.protected_read_roots,
                    &self.protected_read_files,
                    &self.protected_read_multiply_linked_files,
                );
            }
            protected_by_path
                || path_has_protected_file_identity(&resolved, &self.protected_read_files)
        }
        #[cfg(not(unix))]
        {
            protected_by_path
        }
    }

    pub fn is_read_protected(&self, path: &Path) -> bool {
        let apparent = normalize_apparent(path).unwrap_or_else(|_| path.to_path_buf());
        let canon = std::fs::canonicalize(path).unwrap_or_else(|_| apparent.clone());
        let protected = self.is_within_protected_read_root(path);
        let internal = self
            .internal_read_roots
            .read()
            .unwrap()
            .iter()
            .any(|root| canon.starts_with(root));
        #[cfg(unix)]
        let unsafe_internal_alias = path_has_multiply_linked_protected_identity(
            &canon,
            &self.protected_read_multiply_linked_files,
        );
        #[cfg(not(unix))]
        let unsafe_internal_alias = false;
        protected && (!internal || unsafe_internal_alias)
    }

    /// Grant a Kitsu-selected directory without allowing a symlinked final
    /// component to redirect the capability. Private children receive only the
    /// narrow internal-read exception; ordinary directories receive a regular
    /// session grant.
    pub fn grant_trusted_read_root(&self, path: &Path) -> Result<PathBuf, ToolError> {
        let metadata = std::fs::symlink_metadata(path).map_err(|error| {
            ToolError::PolicyDenied(format!(
                "trusted read path '{}' is not accessible: {error}",
                path.display()
            ))
        })?;
        if metadata.file_type().is_symlink() || !metadata.file_type().is_dir() {
            return Err(ToolError::PolicyDenied(format!(
                "trusted read path '{}' is not a regular directory",
                path.display()
            )));
        }
        if self.is_within_protected_read_root(path) {
            self.allow_internal_read_root(&path.to_string_lossy())
        } else {
            self.grant_read_root(&path.to_string_lossy())
        }
    }

    /// Mark a Kitsu-private file or directory as non-writable by model tools.
    /// This protection has higher precedence than CLI/session write roots,
    /// interactive approval, and [`Mode::AutoAllow`].
    pub fn protect_write_path(&self, path: &str) -> Result<PathBuf, ToolError> {
        let requested = Path::new(path);
        let apparent = normalize_apparent(requested)?;
        let canon = canonicalize_intended(requested)?;
        {
            let mut roots = self.protected_write_roots.write().unwrap();
            insert_minimal_root(&mut roots, apparent);
            insert_minimal_root(&mut roots, canon.clone());
        }
        #[cfg(unix)]
        refresh_protected_file_identities(
            &self.protected_write_roots,
            &self.protected_write_files,
            &self.protected_write_multiply_linked_files,
        )?;
        Ok(canon)
    }

    /// Snapshot Kitsu-private roots that model tools may never write.
    pub fn protected_write_roots(&self) -> Vec<PathBuf> {
        self.protected_write_roots.read().unwrap().clone()
    }

    /// Record the stable identity of Kitsu-private state immediately after the
    /// host creates or replaces it.
    ///
    /// Protected-directory rescans normally discover multiply-linked files at
    /// authorization time. That is necessarily too late if another process
    /// hard-links a newly created private file and removes its private name
    /// before the first model access. Host persistence paths call this at their
    /// commit boundary so the identity remains denied through the surviving
    /// alias for this [`Permissions`] lifetime.
    pub fn observe_protected_path(&self, path: &Path) -> Result<PathBuf, ToolError> {
        let apparent = normalize_apparent(path)?;
        let canon = std::fs::canonicalize(path).map_err(|error| {
            ToolError::PolicyDenied(format!(
                "cannot observe Kitsu-private path '{}': {error}",
                path.display()
            ))
        })?;
        let protects_read = self
            .protected_read_roots
            .read()
            .unwrap()
            .iter()
            .any(|root| apparent.starts_with(root) || canon.starts_with(root));
        let protects_write = self
            .protected_write_roots
            .read()
            .unwrap()
            .iter()
            .any(|root| apparent.starts_with(root) || canon.starts_with(root));
        if !protects_read && !protects_write {
            return Err(ToolError::PolicyDenied(format!(
                "path '{}' is not beneath a registered Kitsu-private root",
                path.display()
            )));
        }

        #[cfg(unix)]
        {
            let mut discovered = HashSet::new();
            let mut multiply_linked = HashSet::new();
            collect_protected_file_identities(&apparent, &mut discovered, &mut multiply_linked)
                .map_err(|error| {
                    ToolError::PolicyDenied(format!(
                        "cannot safely inspect Kitsu-private path '{}': {error}",
                        path.display()
                    ))
                })?;
            if protects_read {
                self.protected_read_files
                    .write()
                    .unwrap()
                    .extend(discovered.iter().copied());
                self.protected_read_multiply_linked_files
                    .write()
                    .unwrap()
                    .extend(discovered.iter().copied());
                self.explicitly_observed_read_files
                    .write()
                    .unwrap()
                    .extend(discovered.iter().copied());
            }
            if protects_write {
                self.protected_write_files
                    .write()
                    .unwrap()
                    .extend(discovered);
                self.protected_write_multiply_linked_files
                    .write()
                    .unwrap()
                    .extend(multiply_linked);
            }
        }
        Ok(canon)
    }

    /// Record a just-created private regular file through an already-open host
    /// descriptor. Unlike [`Self::observe_protected_path`], this remains sound
    /// if another same-user process hard-links the file and removes its private
    /// pathname between the atomic commit and policy bookkeeping: the open file
    /// keeps the installed inode alive until its identity is stored.
    #[cfg(unix)]
    pub fn observe_protected_file(
        &self,
        path: &Path,
        file: &std::fs::File,
    ) -> Result<PathBuf, ToolError> {
        use std::os::unix::fs::MetadataExt;

        let apparent = normalize_apparent(path)?;
        let protects_read = self
            .protected_read_roots
            .read()
            .unwrap()
            .iter()
            .any(|root| apparent.starts_with(root));
        let protects_write = self
            .protected_write_roots
            .read()
            .unwrap()
            .iter()
            .any(|root| apparent.starts_with(root));
        if !protects_read && !protects_write {
            return Err(ToolError::PolicyDenied(format!(
                "path '{}' is not beneath a registered Kitsu-private root",
                path.display()
            )));
        }

        let metadata = file.metadata().map_err(|error| {
            ToolError::PolicyDenied(format!(
                "cannot inspect committed Kitsu-private file '{}': {error}",
                path.display()
            ))
        })?;
        let Some(identity) = protected_file_identity(&metadata) else {
            return Err(ToolError::PolicyDenied(format!(
                "committed Kitsu-private path '{}' is not a regular file",
                path.display()
            )));
        };
        if protects_read {
            self.protected_read_files.write().unwrap().insert(identity);
            self.protected_read_multiply_linked_files
                .write()
                .unwrap()
                .insert(identity);
            self.explicitly_observed_read_files
                .write()
                .unwrap()
                .insert(identity);
        }
        if protects_write {
            self.protected_write_files.write().unwrap().insert(identity);
            if metadata.nlink() > 1 {
                self.protected_write_multiply_linked_files
                    .write()
                    .unwrap()
                    .insert(identity);
            }
        }
        Ok(apparent)
    }

    /// Revoke grants added after construction, including internal reads, and
    /// restore invocation roots that were explicitly revoked in this session.
    /// Session switches use this so one conversation's approvals and artifact
    /// roots cannot authorize another while the invocation policy remains the
    /// stable baseline.
    pub fn reset_runtime_grants(&self) {
        *self.read_roots.write().unwrap() = self.base_read_roots.clone();
        *self.write_roots.write().unwrap() = self.base_write_roots.clone();
        self.runtime_read_roots.write().unwrap().clear();
        self.runtime_write_roots.write().unwrap().clear();
        self.internal_read_roots.write().unwrap().clear();
    }

    /// Add a read-allowed root at runtime (e.g. the `/allow` command).
    pub fn grant_read_root(&self, path: &str) -> Result<PathBuf, ToolError> {
        let canon = std::fs::canonicalize(path).map_err(|e| {
            ToolError::PolicyDenied(format!("path '{path}' is not accessible: {e}"))
        })?;
        insert_minimal_root(&mut self.runtime_read_roots.write().unwrap(), canon.clone());
        Ok(canon)
    }

    /// Add a write-allowed root at runtime (e.g. the `/allow-write` command).
    pub fn grant_write_root(&self, path: &str) -> Result<PathBuf, ToolError> {
        let canon = std::fs::canonicalize(path).map_err(|e| {
            ToolError::PolicyDenied(format!("path '{path}' is not accessible: {e}"))
        })?;
        insert_minimal_root(
            &mut self.runtime_write_roots.write().unwrap(),
            canon.clone(),
        );
        Ok(canon)
    }

    /// Add matching read and write roots as one explicit combined capability.
    pub fn grant_read_write_root(&self, path: &str) -> Result<PathBuf, ToolError> {
        let canon = std::fs::canonicalize(path).map_err(|e| {
            ToolError::PolicyDenied(format!("path '{path}' is not accessible: {e}"))
        })?;
        insert_minimal_root(&mut self.runtime_read_roots.write().unwrap(), canon.clone());
        insert_minimal_root(
            &mut self.runtime_write_roots.write().unwrap(),
            canon.clone(),
        );
        Ok(canon)
    }

    /// Revoke one exact user-visible read grant for this session. Descendants
    /// of a broader grant are not accepted because this policy has no deny-hole
    /// representation; the error points users at the exact roots to revoke.
    pub fn revoke_read_root(&self, path: &str) -> Result<Revocation, ToolError> {
        self.revoke_root(path, Action::Read)
    }

    /// Revoke one exact user-visible write grant for this session.
    pub fn revoke_write_root(&self, path: &str) -> Result<Revocation, ToolError> {
        self.revoke_root(path, Action::Write)
    }

    fn revoke_root(&self, path: &str, action: Action) -> Result<Revocation, ToolError> {
        let target = revocation_target(path)?;
        let (invocation_roots, runtime_roots) = match action {
            Action::Read => (&self.read_roots, &self.runtime_read_roots),
            Action::Write => (&self.write_roots, &self.runtime_write_roots),
        };
        let revocation = Revocation {
            invocation: remove_exact_root(invocation_roots, &target),
            runtime: remove_exact_root(runtime_roots, &target),
        };
        if revocation.invocation || revocation.runtime {
            return Ok(revocation);
        }

        let covering = match action {
            Action::Read => self.read_roots(),
            Action::Write => self.write_roots(),
        }
        .into_iter()
        .find(|root| target.starts_with(root));
        let detail = covering
            .map(|root| {
                format!(
                    " (it is covered by '{}'; revoke that exact root instead)",
                    root.display()
                )
            })
            .unwrap_or_default();
        Err(ToolError::PolicyDenied(format!(
            "no exact active {} grant for '{}'{}",
            action.verb(),
            target.display(),
            detail
        )))
    }

    /// Check (and possibly prompt for) read access to `requested`. Returns the
    /// resolved absolute path on success.
    pub async fn check_read(&self, requested: &Path) -> Result<PathBuf, ToolError> {
        let apparent = normalize_apparent(requested)?;
        let canon = std::fs::canonicalize(requested).map_err(|_| {
            ToolError::PolicyDenied(format!(
                "path '{}' is not accessible or does not exist",
                requested.display()
            ))
        })?;
        #[cfg(unix)]
        if path_is_multiply_linked_regular_file(&canon) {
            refresh_protected_file_identities(
                &self.protected_read_roots,
                &self.protected_read_files,
                &self.protected_read_multiply_linked_files,
            )?;
        }
        let protected_by_path = self
            .protected_read_roots
            .read()
            .unwrap()
            .iter()
            .any(|root| apparent.starts_with(root) || canon.starts_with(root));
        #[cfg(unix)]
        let protected = protected_by_path
            || path_has_protected_file_identity(&canon, &self.protected_read_files);
        #[cfg(not(unix))]
        let protected = protected_by_path;
        if protected {
            let internal = self
                .internal_read_roots
                .read()
                .unwrap()
                .iter()
                .any(|root| canon.starts_with(root));
            #[cfg(unix)]
            let unsafe_internal_alias = path_has_multiply_linked_protected_identity(
                &canon,
                &self.protected_read_multiply_linked_files,
            );
            #[cfg(not(unix))]
            let unsafe_internal_alias = false;
            if internal && !unsafe_internal_alias {
                return Ok(canon);
            }
            return Err(ToolError::PolicyDenied(format!(
                "path '{}' contains Kitsu-private configuration or session state",
                requested.display()
            )));
        }
        if self.read_roots().iter().any(|r| canon.starts_with(r)) {
            return Ok(canon);
        }
        self.resolve(Action::Read, requested, canon).await
    }

    /// Authorize one exact regular-file read.
    ///
    /// Callers must open [`ReadAuthorization::path`] without following symlinks
    /// and validate the resulting descriptor before reading. Plain
    /// [`Self::check_read`] remains available for directory/listing operations
    /// whose descriptor ownership is handled by the tool itself.
    pub async fn authorize_read_file(
        &self,
        requested: &Path,
    ) -> Result<ReadAuthorization, ToolError> {
        let apparent = normalize_apparent(requested)?;
        let path = self.check_read(requested).await?;
        let metadata = std::fs::metadata(&path).map_err(|error| {
            ToolError::PolicyDenied(format!(
                "cannot inspect read target '{}': {error}",
                requested.display()
            ))
        })?;
        self.ensure_opened_read_allowed(&apparent, &path, &metadata, true)?;
        if !metadata.is_file() {
            return Err(ToolError::PolicyDenied(format!(
                "read target '{}' is not a regular file",
                requested.display()
            )));
        }
        Ok(ReadAuthorization {
            apparent,
            path,
            #[cfg(unix)]
            identity: protected_file_identity(&metadata)
                .expect("regular Unix file has a stable device/inode identity"),
        })
    }

    /// Validate an already-open host-selected implicit input such as project
    /// context or a filesystem skill. This does not consult or widen ordinary
    /// model read grants; it only rejects Kitsu-private paths and identities.
    /// Internal artifact exceptions are deliberately not honored for implicit
    /// prompt inputs.
    pub fn validate_trusted_opened_read(
        &self,
        requested: &Path,
        file: &std::fs::File,
    ) -> Result<(), ToolError> {
        let apparent = normalize_apparent(requested)?;
        let canonical = std::fs::canonicalize(requested).unwrap_or_else(|_| apparent.clone());
        let metadata = file.metadata().map_err(|error| {
            ToolError::PolicyDenied(format!(
                "cannot inspect implicit read target '{}': {error}",
                requested.display()
            ))
        })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;

            if metadata.is_file() && metadata.nlink() > 1 {
                return Err(ToolError::PolicyDenied(format!(
                    "implicit read target '{}' has multiple hard links; refusing to ingest an ambiguous filesystem identity",
                    requested.display()
                )));
            }
        }
        self.ensure_opened_read_allowed(&apparent, &canonical, &metadata, false)
    }

    /// Validate a descriptor opened by a read tool while walking an already
    /// authorized directory capability. `apparent` and `canonical` are the
    /// stable namespace paths beneath that pinned root. This preserves the
    /// same private-path, hard-link identity, and internal-artifact rules as an
    /// exact [`ReadAuthorization`] without re-resolving a pathname that may
    /// have been renamed after the root descriptor was acquired.
    pub(crate) fn validate_opened_model_read(
        &self,
        apparent: &Path,
        canonical: &Path,
        file: &std::fs::File,
    ) -> Result<(), ToolError> {
        let metadata = file.metadata().map_err(|error| {
            ToolError::PolicyDenied(format!(
                "cannot inspect opened read target '{}': {error}",
                canonical.display()
            ))
        })?;
        self.ensure_opened_read_allowed(apparent, canonical, &metadata, true)
    }

    /// Snapshot one host-selected directory for implicit prompt inputs without
    /// granting any model filesystem authority.
    ///
    /// The root and all file identities are captured before discovery. A
    /// caller must subsequently use [`TrustedImplicitReadRoot::validate_opened_file`]
    /// on a component-wise no-follow descriptor. Non-Unix platforms fail
    /// closed until an equivalent no-reparse traversal exists.
    pub fn authorize_trusted_implicit_read_root(
        &self,
        requested: &Path,
    ) -> Result<TrustedImplicitReadRoot, ToolError> {
        #[cfg(not(unix))]
        {
            let _ = requested;
            Err(ToolError::PolicyDenied(
                "trusted implicit filesystem inputs are unavailable on this platform because secure no-reparse traversal is not implemented"
                    .to_string(),
            ))
        }

        #[cfg(unix)]
        {
            let root = normalize_apparent(requested)?;
            let metadata = std::fs::symlink_metadata(&root).map_err(|error| {
                ToolError::PolicyDenied(format!(
                    "trusted implicit read root '{}' is not accessible: {error}",
                    requested.display()
                ))
            })?;
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(ToolError::PolicyDenied(format!(
                    "trusted implicit read root '{}' is not a regular directory",
                    requested.display()
                )));
            }
            let canonical = std::fs::canonicalize(&root).map_err(|error| {
                ToolError::PolicyDenied(format!(
                    "trusted implicit read root '{}' cannot be resolved: {error}",
                    requested.display()
                ))
            })?;
            if canonical != root {
                return Err(ToolError::PolicyDenied(format!(
                    "trusted implicit read root '{}' traverses a symlink or reparse-like alias",
                    requested.display()
                )));
            }

            let mut identities = HashSet::new();
            let mut multiply_linked = HashSet::new();
            collect_protected_file_identities(&root, &mut identities, &mut multiply_linked)
                .map_err(|error| {
                    ToolError::PolicyDenied(format!(
                        "cannot safely snapshot trusted implicit read root '{}': {error}",
                        requested.display()
                    ))
                })?;
            identities.retain(|identity| !multiply_linked.contains(identity));
            Ok(TrustedImplicitReadRoot { root, identities })
        }
    }

    fn ensure_opened_read_allowed(
        &self,
        apparent: &Path,
        canonical: &Path,
        metadata: &std::fs::Metadata,
        allow_internal: bool,
    ) -> Result<(), ToolError> {
        if !metadata.is_file() {
            return Err(ToolError::PolicyDenied(format!(
                "read target '{}' is not a regular file",
                canonical.display()
            )));
        }

        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;

            if metadata.nlink() > 1 {
                refresh_protected_file_identities(
                    &self.protected_read_roots,
                    &self.protected_read_files,
                    &self.protected_read_multiply_linked_files,
                )?;
            }
        }
        let protected_by_path = self
            .protected_read_roots
            .read()
            .unwrap()
            .iter()
            .any(|root| apparent.starts_with(root) || canonical.starts_with(root));
        #[cfg(unix)]
        let identity = protected_file_identity(metadata)
            .expect("regular Unix file has a stable device/inode identity");
        #[cfg(unix)]
        let protected = protected_by_path
            || self
                .protected_read_files
                .read()
                .unwrap()
                .contains(&identity);
        #[cfg(not(unix))]
        let protected = protected_by_path;
        if !protected {
            return Ok(());
        }

        if allow_internal {
            let internal = self
                .internal_read_roots
                .read()
                .unwrap()
                .iter()
                .any(|root| canonical.starts_with(root));
            #[cfg(unix)]
            let unsafe_internal_alias = self
                .protected_read_multiply_linked_files
                .read()
                .unwrap()
                .contains(&identity);
            #[cfg(not(unix))]
            let unsafe_internal_alias = false;
            if internal && !unsafe_internal_alias {
                return Ok(());
            }
        }

        Err(ToolError::PolicyDenied(format!(
            "path '{}' contains Kitsu-private configuration or session state",
            apparent.display()
        )))
    }

    /// Check (and possibly prompt for) write access to `requested`. The target
    /// may not exist yet, so the parent is canonicalized before the check.
    pub async fn check_write(&self, requested: &Path) -> Result<PathBuf, ToolError> {
        let apparent = normalize_apparent(requested)?;
        let file_name = requested.file_name().ok_or_else(|| {
            ToolError::PolicyDenied(format!(
                "path '{}' has no file name to write",
                requested.display()
            ))
        })?;
        let parent = requested.parent().filter(|p| !p.as_os_str().is_empty());
        let canon_parent = match parent {
            Some(p) => std::fs::canonicalize(p),
            None => std::fs::canonicalize("."),
        }
        .map_err(|_| {
            ToolError::PolicyDenied(format!(
                "parent directory of '{}' is not accessible or does not exist",
                requested.display()
            ))
        })?;
        let target = canon_parent.join(file_name);
        // If the target already exists as a SYMLINK, a write follows it — so
        // containment must hold for the link's REAL destination, not the link
        // path (which is trivially inside the allowed parent). Resolve it and
        // check that. An unresolved/dangling symlink must fail closed: returning
        // the in-root link path would let the later write follow it and create a
        // file outside the allowed root.
        let contained_as = match std::fs::symlink_metadata(&target) {
            Ok(m) if m.file_type().is_symlink() => {
                std::fs::canonicalize(&target).map_err(|_| {
                    ToolError::PolicyDenied(format!(
                        "write target '{}' is an unresolved symlink",
                        requested.display()
                    ))
                })?
            }
            _ => target.clone(),
        };
        #[cfg(unix)]
        if path_is_multiply_linked_regular_file(&contained_as) {
            refresh_protected_file_identities(
                &self.protected_write_roots,
                &self.protected_write_files,
                &self.protected_write_multiply_linked_files,
            )?;
        }
        // Check both identities. `target` protects a private path even when its
        // final component is a symlink pointing out, while `contained_as`
        // protects a private destination reached through a symlink alias.
        let protected_by_path = self
            .protected_write_roots
            .read()
            .unwrap()
            .iter()
            .any(|root| {
                apparent.starts_with(root)
                    || target.starts_with(root)
                    || contained_as.starts_with(root)
            });
        #[cfg(unix)]
        let protected = protected_by_path
            || path_has_protected_file_identity(&contained_as, &self.protected_write_files);
        #[cfg(not(unix))]
        let protected = protected_by_path;
        if protected {
            return Err(ToolError::PolicyDenied(format!(
                "path '{}' contains Kitsu-private configuration or session state",
                requested.display()
            )));
        }
        if self
            .write_roots()
            .iter()
            .any(|r| contained_as.starts_with(r))
        {
            return Ok(contained_as);
        }
        // For a symlink, both the prompt and the returned write target must be
        // the resolved destination. Showing/authorizing the innocent-looking
        // in-repo link while later following it outside would be a confused-
        // deputy write.
        self.resolve(Action::Write, requested, contained_as).await
    }

    /// Shared tail of check_read/check_write for a not-yet-allowed `canon` path.
    async fn resolve(
        &self,
        action: Action,
        requested: &Path,
        canon: PathBuf,
    ) -> Result<PathBuf, ToolError> {
        match self.mode {
            Mode::Scoped | Mode::Deny => Err(self.denied(action, requested)),
            Mode::AutoAllow => Ok(canon),
            Mode::Ask => {
                if !crate::interactive_prompts_allowed() {
                    return Err(ToolError::PolicyDenied(format!(
                        "{} denied: background agents cannot open interactive permission prompts; \
                         pre-grant '{}' from the foreground session",
                        action.verb(),
                        requested.display()
                    )));
                }
                let grant_dir = grant_dir_for(&canon);
                let req = PermissionRequest {
                    action,
                    path: canon.display().to_string(),
                    grant_dir: grant_dir.display().to_string(),
                };
                match self.prompt(req).await {
                    Decision::AllowOnce => Ok(canon),
                    Decision::AllowAlways => {
                        let roots = match action {
                            Action::Read => &self.runtime_read_roots,
                            Action::Write => &self.runtime_write_roots,
                        };
                        insert_minimal_root(&mut roots.write().unwrap(), grant_dir);
                        Ok(canon)
                    }
                    Decision::Deny => Err(ToolError::PolicyDenied(format!(
                        "user denied {} access to '{}'",
                        action.verb(),
                        requested.display()
                    ))),
                    Decision::Interrupt => Err(ToolError::Interrupted(
                        ToolInterruption::AuthorityInputClosed,
                    )),
                }
            }
        }
    }

    fn denied(&self, action: Action, requested: &Path) -> ToolError {
        let (roots, flag, slash) = match action {
            Action::Read => (self.read_roots(), "--allow-path", "/allow"),
            Action::Write => (self.write_roots(), "--allow-write", "/allow-write"),
        };
        let allowed = if roots.is_empty() {
            "none granted".to_string()
        } else {
            roots
                .iter()
                .map(|r| format!("'{}'", r.display()))
                .collect::<Vec<_>>()
                .join(", ")
        };
        ToolError::PolicyDenied(format!(
            "{} denied: '{}' is outside the allowed {} root(s) ({allowed}) — grant it with \
             {flag} <dir> (or {slash} <dir> mid-session)",
            action.verb(),
            requested.display(),
            action.verb(),
        ))
    }

    /// Run the prompter off the async runtime (it does blocking terminal IO).
    async fn prompt(&self, req: PermissionRequest) -> Decision {
        match &self.prompter {
            Some(p) => {
                let p = p.clone();
                tokio::task::spawn_blocking(move || p.ask(&req))
                    .await
                    .unwrap_or(Decision::Interrupt)
            }
            None => Decision::Deny,
        }
    }

    /// Reject a generated file larger than the write cap.
    pub fn check_write_size(&self, len: usize) -> Result<(), ToolError> {
        if len > self.max_write_bytes {
            return Err(ToolError::PolicyDenied(format!(
                "content is {len} bytes, exceeding the {} byte write cap",
                self.max_write_bytes
            )));
        }
        Ok(())
    }

    /// Truncate `content` to the byte cap on a UTF-8 boundary, marking if cut.
    pub fn cap_output(&self, mut content: String) -> String {
        if content.len() <= self.max_bytes {
            return content;
        }
        let mut end = self.max_bytes;
        while end > 0 && !content.is_char_boundary(end) {
            end -= 1;
        }
        content.truncate(end);
        content.push_str("\n...[truncated: output exceeded byte cap]");
        content
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_file(dir: &Path, name: &str, body: &str) -> PathBuf {
        let p = dir.join(name);
        let mut f = std::fs::File::create(&p).expect("create temp file");
        f.write_all(body.as_bytes()).expect("write temp file");
        p
    }

    #[tokio::test]
    async fn read_inside_root_is_allowed() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_string_lossy().to_string();
        let perms = Permissions::for_roots(&[root], &[]).unwrap();
        let f = write_file(dir.path(), "ok.txt", "hi");
        assert!(perms.check_read(&f).await.is_ok());
    }

    #[tokio::test]
    async fn read_outside_root_is_denied_in_deny_mode() {
        let allowed = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let perms =
            Permissions::for_roots(&[allowed.path().to_string_lossy().to_string()], &[]).unwrap();
        let f = write_file(other.path(), "secret.txt", "nope");
        assert!(matches!(
            perms.check_read(&f).await,
            Err(ToolError::PolicyDenied(_))
        ));
    }

    #[tokio::test]
    async fn dotdot_write_escape_is_denied() {
        let base = tempfile::tempdir().unwrap();
        let allowed = base.path().join("allowed");
        let sibling = base.path().join("sibling");
        std::fs::create_dir_all(&allowed).unwrap();
        std::fs::create_dir_all(&sibling).unwrap();
        let perms = Permissions::for_roots(&[], &[allowed.to_string_lossy().to_string()]).unwrap();
        let escape = allowed.join("..").join("sibling").join("pwned.rs");
        assert!(matches!(
            perms.check_write(&escape).await,
            Err(ToolError::PolicyDenied(_))
        ));
        assert!(!sibling.join("pwned.rs").exists());
    }

    #[tokio::test]
    async fn auto_allow_mode_permits_anything() {
        let dir = tempfile::tempdir().unwrap();
        let perms = Permissions::new(&[], &[], Mode::AutoAllow).unwrap();
        let f = write_file(dir.path(), "x.txt", "hi");
        assert!(perms.check_read(&f).await.is_ok());
        let target = dir.path().join("new.rs");
        assert!(perms.check_write(&target).await.is_ok());
    }

    #[tokio::test]
    async fn scoped_mode_never_escapes_current_roots() {
        let allowed = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let allowed_file = write_file(allowed.path(), "inside.txt", "ok");
        let outside_file = write_file(outside.path(), "outside.txt", "secret");
        let root = allowed.path().to_string_lossy().to_string();
        let perms = Permissions::new(
            std::slice::from_ref(&root),
            std::slice::from_ref(&root),
            Mode::Scoped,
        )
        .unwrap();

        assert_eq!(perms.check_read(&allowed_file).await.unwrap(), allowed_file);
        assert!(perms
            .check_write(&allowed.path().join("new.txt"))
            .await
            .is_ok());
        assert!(matches!(
            perms.check_read(&outside_file).await,
            Err(ToolError::PolicyDenied(_))
        ));
        assert!(matches!(
            perms.check_write(&outside.path().join("new.txt")).await,
            Err(ToolError::PolicyDenied(_))
        ));
    }

    #[tokio::test]
    async fn write_root_does_not_imply_read_authority() {
        let dir = tempfile::tempdir().unwrap();
        let existing = write_file(dir.path(), "existing.txt", "private");
        let root = dir.path().to_string_lossy().to_string();
        let perms = Permissions::new(&[], std::slice::from_ref(&root), Mode::Scoped).unwrap();

        assert!(matches!(
            perms.check_read(&existing).await,
            Err(ToolError::PolicyDenied(_))
        ));
        assert!(perms.check_write(&dir.path().join("new.txt")).await.is_ok());
    }

    #[tokio::test]
    async fn ask_mode_widens_on_allow_always() {
        struct Yes;
        impl Prompter for Yes {
            fn ask(&self, _req: &PermissionRequest) -> Decision {
                Decision::AllowAlways
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let perms = Permissions::new(&[], &[], Mode::Ask)
            .unwrap()
            .with_prompter(Arc::new(Yes));
        let f = write_file(dir.path(), "a.txt", "hi");
        // First access prompts (Yes -> AllowAlways) and widens the allowlist.
        assert!(perms.check_read(&f).await.is_ok());
        // A sibling in the now-granted dir is allowed WITHOUT prompting.
        let g = write_file(dir.path(), "b.txt", "hi");
        assert!(perms.check_read(&g).await.is_ok());
    }

    #[tokio::test]
    async fn ask_mode_denies_on_user_deny() {
        struct No;
        impl Prompter for No {
            fn ask(&self, _req: &PermissionRequest) -> Decision {
                Decision::Deny
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let perms = Permissions::new(&[], &[], Mode::Ask)
            .unwrap()
            .with_prompter(Arc::new(No));
        let f = write_file(dir.path(), "a.txt", "hi");
        assert!(matches!(
            perms.check_read(&f).await,
            Err(ToolError::PolicyDenied(_))
        ));
    }

    #[tokio::test]
    async fn ask_mode_interrupts_when_authority_input_closes() {
        struct Closed;
        impl Prompter for Closed {
            fn ask(&self, _req: &PermissionRequest) -> Decision {
                Decision::Interrupt
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("must-not-exist.txt");
        let perms = Permissions::new(&[], &[], Mode::Ask)
            .unwrap()
            .with_prompter(Arc::new(Closed));

        assert!(matches!(
            perms.check_write(&target).await,
            Err(ToolError::Interrupted(
                ToolInterruption::AuthorityInputClosed
            ))
        ));
        assert!(perms.write_roots().is_empty());
        assert!(!target.exists());
    }

    #[tokio::test]
    async fn background_scope_denies_without_invoking_interactive_prompter() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct CountPrompts(Arc<AtomicUsize>);
        impl Prompter for CountPrompts {
            fn ask(&self, _req: &PermissionRequest) -> Decision {
                self.0.fetch_add(1, Ordering::Relaxed);
                Decision::AllowOnce
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let target = write_file(dir.path(), "background.txt", "private");
        let prompts = Arc::new(AtomicUsize::new(0));
        let perms = Permissions::new(&[], &[], Mode::Ask)
            .unwrap()
            .with_prompter(Arc::new(CountPrompts(prompts.clone())));

        let error = crate::scope_without_interactive_prompts(perms.check_read(&target))
            .await
            .unwrap_err();
        assert!(matches!(error, ToolError::PolicyDenied(_)));
        assert!(error.to_string().contains("background agents"));
        assert_eq!(prompts.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn runtime_grants_are_deduplicated_and_parent_minimized() {
        let dir = tempfile::tempdir().unwrap();
        let child = dir.path().join("child");
        std::fs::create_dir(&child).unwrap();
        let perms = Permissions::for_roots(&[], &[]).unwrap();

        perms.grant_read_root(&child.to_string_lossy()).unwrap();
        perms.grant_read_root(&child.to_string_lossy()).unwrap();
        assert_eq!(perms.read_roots().len(), 1);

        perms
            .grant_read_root(&dir.path().to_string_lossy())
            .unwrap();
        assert_eq!(perms.read_roots(), vec![dir.path().to_path_buf()]);
    }

    #[test]
    fn session_reset_revokes_runtime_grants_but_keeps_cli_roots() {
        let base = tempfile::tempdir().unwrap();
        let runtime = tempfile::tempdir().unwrap();
        let perms =
            Permissions::for_roots(&[base.path().to_string_lossy().to_string()], &[]).unwrap();
        perms
            .grant_read_root(&runtime.path().to_string_lossy())
            .unwrap();
        assert_eq!(perms.read_roots().len(), 2);

        perms.reset_runtime_grants();
        assert_eq!(perms.read_roots(), vec![base.path().to_path_buf()]);
    }

    #[test]
    fn grant_provenance_and_exact_revocation_survive_session_reset() {
        let base = tempfile::tempdir().unwrap();
        let runtime = tempfile::tempdir().unwrap();
        let perms = Permissions::for_roots(
            &[base.path().to_string_lossy().to_string()],
            &[base.path().to_string_lossy().to_string()],
        )
        .unwrap();
        perms
            .grant_read_write_root(&runtime.path().to_string_lossy())
            .unwrap();

        assert_eq!(perms.base_read_roots(), vec![base.path().to_path_buf()]);
        assert_eq!(
            perms.active_base_read_roots(),
            vec![base.path().to_path_buf()]
        );
        assert_eq!(
            perms.runtime_read_roots(),
            vec![runtime.path().to_path_buf()]
        );
        assert_eq!(
            perms.runtime_write_roots(),
            vec![runtime.path().to_path_buf()]
        );

        let revoked_read = perms
            .revoke_read_root(&base.path().to_string_lossy())
            .unwrap();
        let revoked_write = perms
            .revoke_write_root(&base.path().to_string_lossy())
            .unwrap();
        assert_eq!(
            revoked_read,
            Revocation {
                invocation: true,
                runtime: false
            }
        );
        assert_eq!(revoked_write, revoked_read);
        assert!(perms.active_base_read_roots().is_empty());
        assert!(perms.active_base_write_roots().is_empty());

        perms.reset_runtime_grants();
        assert_eq!(
            perms.active_base_read_roots(),
            vec![base.path().to_path_buf()]
        );
        assert_eq!(
            perms.active_base_write_roots(),
            vec![base.path().to_path_buf()]
        );
        assert!(perms.runtime_read_roots().is_empty());
        assert!(perms.runtime_write_roots().is_empty());
    }

    #[test]
    fn revoke_requires_an_exact_root_and_never_touches_protected_state() {
        let dir = tempfile::tempdir().unwrap();
        let child = dir.path().join("child");
        let private = dir.path().join("private");
        std::fs::create_dir(&child).unwrap();
        std::fs::create_dir(&private).unwrap();
        let perms = Permissions::for_roots(
            &[dir.path().to_string_lossy().to_string()],
            &[dir.path().to_string_lossy().to_string()],
        )
        .unwrap();
        perms.protect_read_path(&private.to_string_lossy()).unwrap();
        perms
            .protect_write_path(&private.to_string_lossy())
            .unwrap();

        let error = perms
            .revoke_read_root(&child.to_string_lossy())
            .unwrap_err()
            .to_string();
        assert!(error.contains("revoke that exact root"));
        assert_eq!(perms.protected_read_roots(), vec![private.clone()]);
        assert_eq!(perms.protected_write_roots(), vec![private]);
    }

    #[tokio::test]
    async fn protected_write_overrides_broad_root_and_auto_allow() {
        let dir = tempfile::tempdir().unwrap();
        let private = dir.path().join("private");
        std::fs::create_dir(&private).unwrap();
        let existing = write_file(&private, "session.json", "secret");
        let public = dir.path().join("public.txt");

        let broad =
            Permissions::for_roots(&[], &[dir.path().to_string_lossy().to_string()]).unwrap();
        broad
            .protect_write_path(&private.to_string_lossy())
            .unwrap();
        assert!(matches!(
            broad.check_write(&existing).await,
            Err(ToolError::PolicyDenied(_))
        ));
        assert!(matches!(
            broad.check_write(&private.join("new.json")).await,
            Err(ToolError::PolicyDenied(_))
        ));
        assert_eq!(broad.check_write(&public).await.unwrap(), public);

        let automatic = Permissions::new(&[], &[], Mode::AutoAllow).unwrap();
        automatic
            .protect_write_path(&private.to_string_lossy())
            .unwrap();
        assert!(matches!(
            automatic.check_write(&existing).await,
            Err(ToolError::PolicyDenied(_))
        ));
        assert_eq!(automatic.check_write(&public).await.unwrap(), public);
    }

    #[tokio::test]
    async fn intended_private_state_stays_protected_when_created_later() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join(".kitsu").join("profiles").join("future");
        let broad = dir.path().to_string_lossy().to_string();
        let perms = Permissions::new(
            std::slice::from_ref(&broad),
            std::slice::from_ref(&broad),
            Mode::AutoAllow,
        )
        .unwrap();

        assert!(!state.exists());
        assert_eq!(
            perms.protect_read_path(&state.to_string_lossy()).unwrap(),
            state
        );
        assert_eq!(
            perms.protect_write_path(&state.to_string_lossy()).unwrap(),
            state
        );

        std::fs::create_dir_all(&state).unwrap();
        let secret = write_file(&state, "session.json", "private");
        assert!(matches!(
            perms.check_read(&secret).await,
            Err(ToolError::PolicyDenied(_))
        ));
        assert!(matches!(
            perms.check_write(&secret).await,
            Err(ToolError::PolicyDenied(_))
        ));
        assert!(matches!(
            perms.check_write(&state.join("later.json")).await,
            Err(ToolError::PolicyDenied(_))
        ));
    }

    #[tokio::test]
    async fn protected_write_is_denied_without_prompting() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct CountPrompts(Arc<AtomicUsize>);
        impl Prompter for CountPrompts {
            fn ask(&self, _request: &PermissionRequest) -> Decision {
                self.0.fetch_add(1, Ordering::Relaxed);
                Decision::AllowAlways
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let private = dir.path().join("private");
        std::fs::create_dir(&private).unwrap();
        let target = private.join("session.json");
        let prompts = Arc::new(AtomicUsize::new(0));
        let perms = Permissions::new(&[], &[], Mode::Ask)
            .unwrap()
            .with_prompter(Arc::new(CountPrompts(prompts.clone())));
        perms
            .protect_write_path(&private.to_string_lossy())
            .unwrap();

        assert!(matches!(
            perms.check_write(&target).await,
            Err(ToolError::PolicyDenied(_))
        ));
        assert_eq!(prompts.load(Ordering::Relaxed), 0);
        assert!(perms.write_roots().is_empty());
    }

    #[tokio::test]
    async fn internal_read_root_is_a_narrow_protected_parent_exception() {
        let dir = tempfile::tempdir().unwrap();
        let private = dir.path().join("private");
        let artifacts = private.join("artifacts");
        let sessions = private.join("sessions");
        std::fs::create_dir_all(&artifacts).unwrap();
        std::fs::create_dir_all(&sessions).unwrap();
        let session = write_file(&sessions, "saved.json", "secret");

        let perms = Permissions::for_roots(&[], &[]).unwrap();
        perms.protect_read_path(&private.to_string_lossy()).unwrap();
        perms
            .allow_internal_read_root(&artifacts.to_string_lossy())
            .unwrap();
        // Internal exceptions are for fresh runtime artifacts. A file already
        // present when private state is registered remains private forever.
        let artifact = write_file(&artifacts, "result.txt", "safe output");

        // The internal capability grants its canonical subtree directly; it
        // does not depend on a user-visible read root.
        assert_eq!(perms.check_read(&artifact).await.unwrap(), artifact);
        assert!(!perms.is_read_protected(&artifact));
        assert!(perms.is_read_protected(&session));
        assert!(matches!(
            perms.check_read(&session).await,
            Err(ToolError::PolicyDenied(_))
        ));
        assert!(matches!(
            perms.check_read(&private).await,
            Err(ToolError::PolicyDenied(_))
        ));

        // A later broad session grant must not expose protected siblings.
        perms
            .grant_read_root(&dir.path().to_string_lossy())
            .unwrap();
        assert!(matches!(
            perms.check_read(&session).await,
            Err(ToolError::PolicyDenied(_))
        ));
        assert_eq!(perms.check_read(&artifact).await.unwrap(), artifact);
    }

    #[cfg(unix)]
    #[test]
    fn trusted_implicit_root_is_snapshot_bound_and_never_overrides_observed_private_files() {
        let dir = tempfile::tempdir().unwrap();
        let private = dir.path().join(".kitsu");
        let skills = private.join("skills");
        let skill_dir = skills.join("review");
        let sessions = private.join("sessions");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::create_dir(&sessions).unwrap();
        let manifest = write_file(&skill_dir, "SKILL.md", "trusted instructions");
        let session = write_file(&sessions, "saved.json", "private transcript");

        let perms = Permissions::for_roots(&[], &[]).unwrap();
        perms.protect_read_path(&private.to_string_lossy()).unwrap();
        let trusted = perms.authorize_trusted_implicit_read_root(&skills).unwrap();
        let manifest_file = std::fs::File::open(&manifest).unwrap();
        trusted
            .validate_opened_file(&manifest, &manifest_file, &perms)
            .unwrap();

        let session_file = std::fs::File::open(&session).unwrap();
        assert!(matches!(
            trusted.validate_opened_file(&session, &session_file, &perms),
            Err(ToolError::PolicyDenied(_))
        ));

        let linked = skill_dir.join("linked-session.json");
        std::fs::hard_link(&session, &linked).unwrap();
        let linked_file = std::fs::File::open(&linked).unwrap();
        assert!(matches!(
            trusted.validate_opened_file(&linked, &linked_file, &perms),
            Err(ToolError::PolicyDenied(message)) if message.contains("multiple hard links")
        ));

        let late = write_file(&skill_dir, "late.txt", "not in snapshot");
        let late_file = std::fs::File::open(&late).unwrap();
        assert!(matches!(
            trusted.validate_opened_file(&late, &late_file, &perms),
            Err(ToolError::PolicyDenied(message)) if message.contains("not present")
        ));

        perms
            .observe_protected_file(&manifest, &manifest_file)
            .unwrap();
        assert!(matches!(
            trusted.validate_opened_file(&manifest, &manifest_file, &perms),
            Err(ToolError::PolicyDenied(message)) if message.contains("explicitly observed")
        ));

        let displaced = private.join("skills-before-replacement");
        std::fs::rename(&skills, &displaced).unwrap();
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(&manifest, "replacement instructions").unwrap();
        let replacement_file = std::fs::File::open(&manifest).unwrap();
        assert!(matches!(
            trusted.validate_opened_file(&manifest, &replacement_file, &perms),
            Err(ToolError::PolicyDenied(message)) if message.contains("not present")
        ));
    }

    #[test]
    fn internal_read_root_must_be_a_strict_protected_child() {
        let dir = tempfile::tempdir().unwrap();
        let private = dir.path().join("private");
        let child = private.join("artifacts");
        let outside = dir.path().join("outside");
        std::fs::create_dir_all(&child).unwrap();
        std::fs::create_dir(&outside).unwrap();
        let perms = Permissions::for_roots(&[], &[]).unwrap();
        perms.protect_read_path(&private.to_string_lossy()).unwrap();

        assert!(matches!(
            perms.allow_internal_read_root(&private.to_string_lossy()),
            Err(ToolError::PolicyDenied(_))
        ));
        assert!(matches!(
            perms.allow_internal_read_root(&outside.to_string_lossy()),
            Err(ToolError::PolicyDenied(_))
        ));
        assert_eq!(
            perms
                .allow_internal_read_root(&child.to_string_lossy())
                .unwrap(),
            child
        );
    }

    #[tokio::test]
    async fn reset_revokes_internal_reads_but_keeps_private_protections() {
        let dir = tempfile::tempdir().unwrap();
        let private = dir.path().join("private");
        let artifacts = private.join("artifacts");
        let runtime = dir.path().join("runtime");
        std::fs::create_dir_all(&artifacts).unwrap();
        std::fs::create_dir(&runtime).unwrap();
        let perms = Permissions::for_roots(&[], &[]).unwrap();
        perms.protect_read_path(&private.to_string_lossy()).unwrap();
        perms
            .allow_internal_read_root(&artifacts.to_string_lossy())
            .unwrap();
        let artifact = write_file(&artifacts, "result.txt", "current session");
        perms
            .protect_write_path(&private.to_string_lossy())
            .unwrap();
        perms.grant_read_root(&runtime.to_string_lossy()).unwrap();
        perms.grant_write_root(&runtime.to_string_lossy()).unwrap();
        assert_eq!(perms.check_read(&artifact).await.unwrap(), artifact);

        perms.reset_runtime_grants();

        assert!(perms.read_roots().is_empty());
        assert!(perms.write_roots().is_empty());
        assert_eq!(perms.protected_read_roots(), vec![private.clone()]);
        assert!(perms.internal_read_roots().is_empty());
        assert_eq!(perms.protected_write_roots(), vec![private]);
        assert!(matches!(
            perms.check_read(&artifact).await,
            Err(ToolError::PolicyDenied(_))
        ));
    }

    #[test]
    fn future_private_root_below_regular_file_ancestor_is_still_reserved() {
        let base = tempfile::tempdir().unwrap();
        let unusable_parent = base.path().join("home-is-a-file");
        std::fs::write(&unusable_parent, "not a directory").unwrap();
        let future = unusable_parent.join(".kitsu/profiles/example");
        let perms = Permissions::for_roots(&[], &[]).unwrap();

        assert!(perms.protect_read_path(&future.to_string_lossy()).is_ok());
        assert!(perms.protect_write_path(&future.to_string_lossy()).is_ok());
        assert!(perms
            .protected_read_roots()
            .iter()
            .any(|root| root == &future));
        assert!(perms
            .protected_write_roots()
            .iter()
            .any(|root| root == &future));
    }
}

#[cfg(test)]
#[cfg(unix)]
mod symlink_tests {
    use super::*;

    #[tokio::test]
    async fn future_private_root_cannot_shed_protection_via_late_symlink() {
        let base = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let future = base.path().join("future-private-state");
        let outside_secret = outside.path().join("session.json");
        std::fs::write(&outside_secret, "private").unwrap();
        let broad = base.path().to_string_lossy().to_string();
        let perms = Permissions::new(
            std::slice::from_ref(&broad),
            std::slice::from_ref(&broad),
            Mode::AutoAllow,
        )
        .unwrap();
        perms.protect_read_path(&future.to_string_lossy()).unwrap();
        perms.protect_write_path(&future.to_string_lossy()).unwrap();

        std::os::unix::fs::symlink(outside.path(), &future).unwrap();
        let apparent_secret = future.join("session.json");

        assert!(matches!(
            perms.check_read(&apparent_secret).await,
            Err(ToolError::PolicyDenied(_))
        ));
        assert!(matches!(
            perms.check_write(&apparent_secret).await,
            Err(ToolError::PolicyDenied(_))
        ));
        assert_eq!(std::fs::read_to_string(outside_secret).unwrap(), "private");
    }

    #[tokio::test]
    async fn apparent_private_identity_survives_ancestor_symlink_retargeting() {
        let base = tempfile::tempdir().unwrap();
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let alias = base.path().join("state-alias");
        std::os::unix::fs::symlink(first.path(), &alias).unwrap();
        let private = alias.join("profile");
        std::fs::create_dir_all(&private).unwrap();
        let broad = base.path().to_string_lossy().to_string();
        let perms = Permissions::new(
            std::slice::from_ref(&broad),
            std::slice::from_ref(&broad),
            Mode::AutoAllow,
        )
        .unwrap();
        perms.protect_read_path(&private.to_string_lossy()).unwrap();
        perms
            .protect_write_path(&private.to_string_lossy())
            .unwrap();

        std::fs::remove_file(&alias).unwrap();
        std::os::unix::fs::symlink(second.path(), &alias).unwrap();
        std::fs::create_dir_all(second.path().join("profile")).unwrap();
        let redirected = private.join("session.json");
        std::fs::write(second.path().join("profile/session.json"), "private").unwrap();

        assert!(matches!(
            perms.check_read(&redirected).await,
            Err(ToolError::PolicyDenied(_))
        ));
        assert!(matches!(
            perms.check_write(&redirected).await,
            Err(ToolError::PolicyDenied(_))
        ));
    }

    #[tokio::test]
    async fn internal_read_works_through_stable_alias_but_not_after_retarget() {
        let base = tempfile::tempdir().unwrap();
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let alias = base.path().join("state-alias");
        std::os::unix::fs::symlink(first.path(), &alias).unwrap();
        let private = alias.join("profile");
        let artifacts = private.join("artifacts");
        std::fs::create_dir_all(&artifacts).unwrap();
        let first_artifact = artifacts.join("result.txt");
        let perms = Permissions::for_roots(&[], &[]).unwrap();
        perms.protect_read_path(&private.to_string_lossy()).unwrap();
        perms
            .allow_internal_read_root(&artifacts.to_string_lossy())
            .unwrap();
        // Runtime artifact generations are granted while empty; their files
        // are created only after the narrow exception exists.
        std::fs::write(&first_artifact, "first").unwrap();

        assert_eq!(
            perms.check_read(&first_artifact).await.unwrap(),
            std::fs::canonicalize(&first_artifact).unwrap()
        );

        std::fs::remove_file(&alias).unwrap();
        std::os::unix::fs::symlink(second.path(), &alias).unwrap();
        std::fs::create_dir_all(second.path().join("profile/artifacts")).unwrap();
        let redirected = artifacts.join("result.txt");
        std::fs::write(second.path().join("profile/artifacts/result.txt"), "second").unwrap();
        assert!(matches!(
            perms.check_read(&redirected).await,
            Err(ToolError::PolicyDenied(_))
        ));
    }

    #[tokio::test]
    async fn symlink_final_component_write_escape_is_denied() {
        let base = tempfile::tempdir().unwrap();
        let allowed = base.path().join("allowed");
        let outside = base.path().join("outside");
        std::fs::create_dir_all(&allowed).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let secret = outside.join("secret.txt");
        std::fs::write(&secret, "orig").unwrap();
        // A symlink INSIDE the allowed write root pointing OUTSIDE it.
        let link = allowed.join("link.txt");
        std::os::unix::fs::symlink(&secret, &link).unwrap();
        let perms = Permissions::for_roots(&[], &[allowed.to_string_lossy().to_string()]).unwrap();
        // Writing through the in-allowlist symlink must be denied (resolves outside).
        assert!(matches!(
            perms.check_write(&link).await,
            Err(ToolError::PolicyDenied(_))
        ));
        assert_eq!(std::fs::read_to_string(&secret).unwrap(), "orig");
    }

    #[tokio::test]
    async fn dangling_symlink_write_escape_is_denied() {
        let base = tempfile::tempdir().unwrap();
        let allowed = base.path().join("allowed");
        let outside = base.path().join("outside");
        std::fs::create_dir_all(&allowed).unwrap();
        std::fs::create_dir_all(&outside).unwrap();

        // The destination does not exist yet, so canonicalizing the symlink itself
        // fails. Falling back to the in-root link path would authorize a later
        // std::fs::write, which follows the link and creates this outside file.
        let escaped = outside.join("created-through-link.txt");
        let link = allowed.join("link.txt");
        std::os::unix::fs::symlink(&escaped, &link).unwrap();

        let perms = Permissions::for_roots(&[], &[allowed.to_string_lossy().to_string()]).unwrap();
        assert!(matches!(
            perms.check_write(&link).await,
            Err(ToolError::PolicyDenied(_))
        ));
        assert!(!escaped.exists());
    }

    #[tokio::test]
    async fn symlink_within_write_root_is_allowed() {
        let base = tempfile::tempdir().unwrap();
        let allowed = base.path().join("allowed");
        std::fs::create_dir_all(&allowed).unwrap();
        let real = allowed.join("real.txt");
        std::fs::write(&real, "x").unwrap();
        let link = allowed.join("link.txt");
        std::os::unix::fs::symlink(&real, &link).unwrap(); // link stays inside root
        let perms = Permissions::for_roots(&[], &[allowed.to_string_lossy().to_string()]).unwrap();
        assert_eq!(perms.check_write(&link).await.unwrap(), real);
    }

    #[tokio::test]
    async fn ask_mode_displays_and_authorizes_symlink_destination() {
        use std::sync::Mutex;

        struct Capture(Arc<Mutex<Option<String>>>);
        impl Prompter for Capture {
            fn ask(&self, request: &PermissionRequest) -> Decision {
                *self.0.lock().unwrap() = Some(request.path.clone());
                Decision::AllowOnce
            }
        }

        let base = tempfile::tempdir().unwrap();
        let project = base.path().join("project");
        let outside = base.path().join("outside");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let destination = outside.join("credentials.txt");
        std::fs::write(&destination, "original").unwrap();
        let link = project.join("config.txt");
        std::os::unix::fs::symlink(&destination, &link).unwrap();
        let shown = Arc::new(Mutex::new(None));
        let perms = Permissions::new(&[], &[], Mode::Ask)
            .unwrap()
            .with_prompter(Arc::new(Capture(shown.clone())));

        let authorized = perms.check_write(&link).await.unwrap();
        assert_eq!(authorized, destination);
        assert_eq!(shown.lock().unwrap().as_deref(), destination.to_str());
    }

    #[tokio::test]
    async fn symlink_alias_cannot_write_a_protected_destination() {
        let base = tempfile::tempdir().unwrap();
        let private = base.path().join("private");
        std::fs::create_dir(&private).unwrap();
        let secret = private.join("session.json");
        std::fs::write(&secret, "original").unwrap();
        let alias = base.path().join("innocent.json");
        std::os::unix::fs::symlink(&secret, &alias).unwrap();
        let perms =
            Permissions::for_roots(&[], &[base.path().to_string_lossy().to_string()]).unwrap();
        perms
            .protect_write_path(&private.to_string_lossy())
            .unwrap();

        assert!(matches!(
            perms.check_write(&alias).await,
            Err(ToolError::PolicyDenied(_))
        ));
        assert_eq!(std::fs::read_to_string(secret).unwrap(), "original");
    }

    #[tokio::test]
    async fn protected_symlink_path_cannot_redirect_a_write_outward() {
        let base = tempfile::tempdir().unwrap();
        let private = base.path().join("private");
        let outside = base.path().join("outside.txt");
        std::fs::create_dir(&private).unwrap();
        std::fs::write(&outside, "original").unwrap();
        let link = private.join("redirect.txt");
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        let perms =
            Permissions::for_roots(&[], &[base.path().to_string_lossy().to_string()]).unwrap();
        perms
            .protect_write_path(&private.to_string_lossy())
            .unwrap();

        assert!(matches!(
            perms.check_write(&link).await,
            Err(ToolError::PolicyDenied(_))
        ));
        assert_eq!(std::fs::read_to_string(outside).unwrap(), "original");
    }

    #[tokio::test]
    async fn hard_link_alias_of_a_protected_file_is_never_readable_or_writable() {
        let base = tempfile::tempdir().unwrap();
        let private = base.path().join("kitsu.toml");
        let alias = base.path().join("innocent.toml");
        std::fs::write(&private, "token = 'private'\n").unwrap();
        std::fs::hard_link(&private, &alias).unwrap();
        let broad = base.path().to_string_lossy().to_string();
        let perms = Permissions::new(
            std::slice::from_ref(&broad),
            std::slice::from_ref(&broad),
            Mode::AutoAllow,
        )
        .unwrap();
        perms.protect_read_path(&private.to_string_lossy()).unwrap();
        perms
            .protect_write_path(&private.to_string_lossy())
            .unwrap();

        assert!(perms.is_read_protected(&alias));
        assert!(matches!(
            perms.check_read(&alias).await,
            Err(ToolError::PolicyDenied(_))
        ));
        assert!(matches!(
            perms.check_write(&alias).await,
            Err(ToolError::PolicyDenied(_))
        ));
        assert_eq!(
            std::fs::read_to_string(private).unwrap(),
            "token = 'private'\n"
        );
    }

    #[tokio::test]
    async fn protected_directory_discovers_new_files_and_remembers_unlinked_names() {
        let base = tempfile::tempdir().unwrap();
        let private = base.path().join("state");
        std::fs::create_dir(&private).unwrap();
        let broad = base.path().to_string_lossy().to_string();
        let perms = Permissions::new(
            std::slice::from_ref(&broad),
            std::slice::from_ref(&broad),
            Mode::AutoAllow,
        )
        .unwrap();
        perms.protect_read_path(&private.to_string_lossy()).unwrap();
        perms
            .protect_write_path(&private.to_string_lossy())
            .unwrap();

        // The file did not exist when the directory was protected. The next
        // authorization refreshes identities below the protected directory.
        let session = private.join("session.json");
        let alias = base.path().join("shared-session.json");
        std::fs::write(&session, "new private state").unwrap();
        std::fs::hard_link(&session, &alias).unwrap();
        assert!(matches!(
            perms.check_read(&alias).await,
            Err(ToolError::PolicyDenied(_))
        ));
        assert!(matches!(
            perms.check_write(&alias).await,
            Err(ToolError::PolicyDenied(_))
        ));

        // Keep the stable identity for this Permissions lifetime even if the
        // original private namespace entry disappears while its alias lives.
        std::fs::remove_file(&session).unwrap();
        assert!(matches!(
            perms.check_read(&alias).await,
            Err(ToolError::PolicyDenied(_))
        ));
        assert!(matches!(
            perms.check_write(&alias).await,
            Err(ToolError::PolicyDenied(_))
        ));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn host_observation_protects_alias_after_private_name_is_removed() {
        use std::os::unix::fs::MetadataExt;

        let base = tempfile::tempdir().unwrap();
        let private = base.path().join("state");
        std::fs::create_dir(&private).unwrap();
        let broad = base.path().to_string_lossy().to_string();
        let perms = Permissions::new(
            std::slice::from_ref(&broad),
            std::slice::from_ref(&broad),
            Mode::AutoAllow,
        )
        .unwrap();
        perms.protect_read_path(&private.to_string_lossy()).unwrap();
        perms
            .protect_write_path(&private.to_string_lossy())
            .unwrap();

        // Model a Kitsu save boundary: the host records the committed private
        // inode before another process can make an alias the only remaining
        // namespace entry.
        let session = private.join("session.json");
        let alias = base.path().join("apparently-public.json");
        std::fs::write(&session, "private transcript").unwrap();
        perms.observe_protected_path(&session).unwrap();
        std::fs::hard_link(&session, &alias).unwrap();
        std::fs::remove_file(&session).unwrap();
        assert_eq!(std::fs::metadata(&alias).unwrap().nlink(), 1);

        assert!(matches!(
            perms.check_read(&alias).await,
            Err(ToolError::PolicyDenied(_))
        ));
        assert!(matches!(
            perms.check_write(&alias).await,
            Err(ToolError::PolicyDenied(_))
        ));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn descriptor_observation_closes_commit_to_path_observation_gap() {
        let base = tempfile::tempdir().unwrap();
        let private = base.path().join("state");
        std::fs::create_dir(&private).unwrap();
        let broad = base.path().to_string_lossy().to_string();
        let perms = Permissions::new(
            std::slice::from_ref(&broad),
            std::slice::from_ref(&broad),
            Mode::AutoAllow,
        )
        .unwrap();
        perms.protect_read_path(&private.to_string_lossy()).unwrap();
        perms
            .protect_write_path(&private.to_string_lossy())
            .unwrap();

        let session = private.join("session.json");
        let alias = base.path().join("surviving-alias.json");
        let committed = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&session)
            .unwrap();
        std::fs::hard_link(&session, &alias).unwrap();
        std::fs::remove_file(&session).unwrap();

        // Path-based observation is now impossible, but the host's commit
        // descriptor still identifies the exact installed private inode.
        assert!(perms.observe_protected_path(&session).is_err());
        perms.observe_protected_file(&session, &committed).unwrap();
        assert!(matches!(
            perms.check_read(&alias).await,
            Err(ToolError::PolicyDenied(_))
        ));
        assert!(matches!(
            perms.check_write(&alias).await,
            Err(ToolError::PolicyDenied(_))
        ));
    }

    #[tokio::test]
    async fn internal_read_root_does_not_exempt_a_private_hard_link_alias() {
        let base = tempfile::tempdir().unwrap();
        let private = base.path().join("state");
        let sessions = private.join("sessions");
        let artifacts = private.join("artifacts");
        std::fs::create_dir_all(&sessions).unwrap();
        std::fs::create_dir_all(&artifacts).unwrap();
        let session = sessions.join("session.json");
        std::fs::write(&session, "private transcript").unwrap();

        let perms = Permissions::for_roots(&[], &[]).unwrap();
        perms.protect_read_path(&private.to_string_lossy()).unwrap();
        perms
            .allow_internal_read_root(&artifacts.to_string_lossy())
            .unwrap();
        let legitimate_artifact = artifacts.join("result.txt");
        std::fs::write(&legitimate_artifact, "safe result").unwrap();
        assert_eq!(
            perms.check_read(&legitimate_artifact).await.unwrap(),
            legitimate_artifact
        );

        let alias = artifacts.join("apparently-safe.json");
        std::fs::hard_link(&session, &alias).unwrap();
        // The private inode was observed with one link. Remove its original
        // name before the first authorization check: the surviving alias must
        // not become indistinguishable from a generated artifact merely
        // because its current link count returned to one.
        std::fs::remove_file(&session).unwrap();
        assert!(perms.is_read_protected(&alias));
        assert!(matches!(
            perms.check_read(&alias).await,
            Err(ToolError::PolicyDenied(_))
        ));
    }

    #[tokio::test]
    async fn observed_future_private_file_cannot_become_a_one_link_internal_alias() {
        let base = tempfile::tempdir().unwrap();
        let private = base.path().join("state");
        let sessions = private.join("sessions");
        let artifacts = private.join("artifacts");
        std::fs::create_dir_all(&sessions).unwrap();
        std::fs::create_dir_all(&artifacts).unwrap();

        let perms = Permissions::for_roots(&[], &[]).unwrap();
        perms.protect_read_path(&private.to_string_lossy()).unwrap();
        perms
            .allow_internal_read_root(&artifacts.to_string_lossy())
            .unwrap();

        let session = sessions.join("later.json");
        let committed = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&session)
            .unwrap();
        perms.observe_protected_file(&session, &committed).unwrap();

        let alias = artifacts.join("surviving.json");
        std::fs::hard_link(&session, &alias).unwrap();
        std::fs::remove_file(&session).unwrap();
        assert!(matches!(
            perms.check_read(&alias).await,
            Err(ToolError::PolicyDenied(_))
        ));
    }

    #[tokio::test]
    async fn exact_read_authorization_rejects_final_hard_link_substitution() {
        let base = tempfile::tempdir().unwrap();
        let allowed = base.path().join("allowed");
        let private = base.path().join("private");
        std::fs::create_dir(&allowed).unwrap();
        std::fs::create_dir(&private).unwrap();
        let target = allowed.join("target.txt");
        let secret = private.join("session.json");
        std::fs::write(&target, "public").unwrap();
        std::fs::write(&secret, "private transcript").unwrap();

        let perms = Permissions::for_roots(&[allowed.to_string_lossy().into_owned()], &[]).unwrap();
        perms.protect_read_path(&private.to_string_lossy()).unwrap();
        let authorization = perms.authorize_read_file(&target).await.unwrap();

        // Replace the final name after policy authorization without using a
        // symlink, then remove the original private name so link count alone no
        // longer distinguishes the alias.
        std::fs::remove_file(&target).unwrap();
        std::fs::hard_link(&secret, &target).unwrap();
        std::fs::remove_file(&secret).unwrap();
        let opened = std::fs::File::open(&target).unwrap();

        assert!(matches!(
            authorization.validate_opened_file(&opened, &perms),
            Err(ToolError::PolicyDenied(message)) if message.contains("changed after authorization")
        ));
    }

    #[test]
    fn trusted_implicit_read_rejects_any_multiply_linked_identity() {
        let base = tempfile::tempdir().unwrap();
        let project = base.path().join("project");
        let outside = base.path().join("outside");
        std::fs::create_dir(&project).unwrap();
        std::fs::create_dir(&outside).unwrap();
        let secret = outside.join("unrelated-secret.txt");
        let context = project.join("AGENTS.md");
        std::fs::write(&secret, "token = secret").unwrap();

        let perms = Permissions::for_roots(&[], &[]).unwrap();
        std::fs::hard_link(&secret, &context).unwrap();
        let opened = std::fs::File::open(&context).unwrap();

        assert!(matches!(
            perms.validate_trusted_opened_read(&context, &opened),
            Err(ToolError::PolicyDenied(message)) if message.contains("multiple hard links")
        ));
    }

    #[test]
    fn symlink_cannot_make_an_external_directory_an_internal_read_root() {
        let base = tempfile::tempdir().unwrap();
        let private = base.path().join("private");
        let outside = base.path().join("outside");
        std::fs::create_dir(&private).unwrap();
        std::fs::create_dir(&outside).unwrap();
        let apparent_child = private.join("artifacts");
        std::os::unix::fs::symlink(&outside, &apparent_child).unwrap();
        let perms = Permissions::for_roots(&[], &[]).unwrap();
        perms.protect_read_path(&private.to_string_lossy()).unwrap();

        assert!(matches!(
            perms.allow_internal_read_root(&apparent_child.to_string_lossy()),
            Err(ToolError::PolicyDenied(_))
        ));
    }
}
