//! Runtime filesystem permissions (design §7.1, Claude-Code-style approval).
//!
//! Reads and writes are gated by separate deny-by-default allowlists. Unlike a
//! static policy, an operation on a not-yet-allowed path is resolved at runtime
//! by the [`Mode`]:
//!
//! - [`Mode::Ask`] — consult a [`Prompter`] (interactive approval; "always" can
//!   widen the session allowlist).
//! - [`Mode::AutoAllow`] — allow everything (the `--yes` escape hatch).
//! - [`Mode::Deny`] — deny anything not pre-granted (non-interactive default).
//!
//! Containment is enforced by canonicalizing paths (the parent, for writes to a
//! not-yet-existing file) before the `starts_with` check, defeating `..`/symlink
//! escapes. The allowlists live behind `RwLock` so `/allow` and an "always"
//! approval can widen them mid-session.

use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use crate::error::ToolError;

/// Default per-tool output cap (bytes); larger tool results are truncated.
pub const DEFAULT_MAX_BYTES: usize = 64 * 1024;
/// Cap on a single generated file's size (bytes); a larger write is rejected.
pub const DEFAULT_MAX_WRITE_BYTES: usize = 1024 * 1024;

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
}

/// How not-yet-allowed operations are handled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Prompt the user (interactive TTY).
    Ask,
    /// Auto-allow everything (`--yes`); never prompt.
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

/// Something that can ask the user to approve an operation. Implemented by the
/// CLI (prints a prompt and reads a keypress); the tools crate stays UI-free.
pub trait Prompter: Send + Sync {
    fn ask(&self, req: &PermissionRequest) -> Decision;
}

/// Runtime read/write permissions.
pub struct Permissions {
    base_read_roots: Vec<PathBuf>,
    base_write_roots: Vec<PathBuf>,
    read_roots: RwLock<Vec<PathBuf>>,
    write_roots: RwLock<Vec<PathBuf>>,
    protected_read_roots: RwLock<Vec<PathBuf>>,
    mode: Mode,
    prompter: Option<Arc<dyn Prompter>>,
    max_bytes: usize,
    max_write_bytes: usize,
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
            protected_read_roots: RwLock::new(Vec::new()),
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

    /// Current fallback policy for paths not already covered by a session root.
    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// Snapshot the canonical read roots currently granted for this session.
    pub fn read_roots(&self) -> Vec<PathBuf> {
        self.read_roots.read().unwrap().clone()
    }

    /// Snapshot the canonical write roots currently granted for this session.
    pub fn write_roots(&self) -> Vec<PathBuf> {
        self.write_roots.read().unwrap().clone()
    }

    /// Paths containing Kitsu's own configuration/transcripts. These remain
    /// unreadable to model tools even under a broader allow root or `--yes`.
    pub fn protect_read_path(&self, path: &str) -> Result<PathBuf, ToolError> {
        let canon = std::fs::canonicalize(path).map_err(|e| {
            ToolError::PolicyDenied(format!("private path '{path}' is not accessible: {e}"))
        })?;
        insert_minimal_root(
            &mut self.protected_read_roots.write().unwrap(),
            canon.clone(),
        );
        Ok(canon)
    }

    pub fn protected_read_roots(&self) -> Vec<PathBuf> {
        self.protected_read_roots.read().unwrap().clone()
    }

    pub fn is_read_protected(&self, path: &Path) -> bool {
        let canon = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        self.protected_read_roots
            .read()
            .unwrap()
            .iter()
            .any(|root| canon.starts_with(root))
    }

    /// Revoke grants added after construction while preserving explicit CLI
    /// roots (and the interactive launch-directory read root). Session switches
    /// use this so one conversation's approvals cannot authorize another.
    pub fn reset_runtime_grants(&self) {
        *self.read_roots.write().unwrap() = self.base_read_roots.clone();
        *self.write_roots.write().unwrap() = self.base_write_roots.clone();
    }

    /// Add a read-allowed root at runtime (e.g. the `/allow` command).
    pub fn grant_read_root(&self, path: &str) -> Result<PathBuf, ToolError> {
        let canon = std::fs::canonicalize(path).map_err(|e| {
            ToolError::PolicyDenied(format!("path '{path}' is not accessible: {e}"))
        })?;
        insert_minimal_root(&mut self.read_roots.write().unwrap(), canon.clone());
        Ok(canon)
    }

    /// Add a write-allowed root at runtime (e.g. the `/allow-write` command).
    pub fn grant_write_root(&self, path: &str) -> Result<PathBuf, ToolError> {
        let canon = std::fs::canonicalize(path).map_err(|e| {
            ToolError::PolicyDenied(format!("path '{path}' is not accessible: {e}"))
        })?;
        insert_minimal_root(&mut self.write_roots.write().unwrap(), canon.clone());
        Ok(canon)
    }

    /// Check (and possibly prompt for) read access to `requested`. Returns the
    /// resolved absolute path on success.
    pub async fn check_read(&self, requested: &Path) -> Result<PathBuf, ToolError> {
        let canon = std::fs::canonicalize(requested).map_err(|_| {
            ToolError::PolicyDenied(format!(
                "path '{}' is not accessible or does not exist",
                requested.display()
            ))
        })?;
        if self
            .protected_read_roots
            .read()
            .unwrap()
            .iter()
            .any(|root| canon.starts_with(root))
        {
            return Err(ToolError::PolicyDenied(format!(
                "path '{}' contains Kitsu-private configuration or session state",
                requested.display()
            )));
        }
        if self
            .read_roots
            .read()
            .unwrap()
            .iter()
            .any(|r| canon.starts_with(r))
        {
            return Ok(canon);
        }
        self.resolve(Action::Read, requested, canon).await
    }

    /// Check (and possibly prompt for) write access to `requested`. The target
    /// may not exist yet, so the parent is canonicalized before the check.
    pub async fn check_write(&self, requested: &Path) -> Result<PathBuf, ToolError> {
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
        if self
            .write_roots
            .read()
            .unwrap()
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
            Mode::Deny => Err(self.denied(action, requested)),
            Mode::AutoAllow => Ok(canon),
            Mode::Ask => {
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
                            Action::Read => &self.read_roots,
                            Action::Write => &self.write_roots,
                        };
                        insert_minimal_root(&mut roots.write().unwrap(), grant_dir);
                        Ok(canon)
                    }
                    Decision::Deny => Err(ToolError::PolicyDenied(format!(
                        "user denied {} access to '{}'",
                        action.verb(),
                        requested.display()
                    ))),
                }
            }
        }
    }

    fn denied(&self, action: Action, requested: &Path) -> ToolError {
        ToolError::PolicyDenied(format!(
            "path '{}' is outside the allowed {} paths",
            requested.display(),
            action.verb()
        ))
    }

    /// Run the prompter off the async runtime (it does blocking terminal IO).
    async fn prompt(&self, req: PermissionRequest) -> Decision {
        match &self.prompter {
            Some(p) => {
                let p = p.clone();
                tokio::task::spawn_blocking(move || p.ask(&req))
                    .await
                    .unwrap_or(Decision::Deny)
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
}

#[cfg(test)]
#[cfg(unix)]
mod symlink_tests {
    use super::*;

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
}
