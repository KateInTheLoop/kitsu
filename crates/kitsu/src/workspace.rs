//! Read-only workspace inspection used by `/diff` and `/review`.
//!
//! Git is invoked directly (never through a shell) and every command is scoped
//! to the current repository. Model-authored diff text is not evidence; this
//! module reports the state Git and the filesystem actually contain.

#[cfg(unix)]
use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Output};

use kitsu_tools::builtins::fs::JournalTurnReport;

const MAX_DIFF_BYTES: usize = 256 * 1024;
const DIFF_TRUNCATION_MARKER: &str = "\n...[diff truncated at 256 KiB]\n";

fn cap_diff(diff: &mut String) -> bool {
    if diff.len() <= MAX_DIFF_BYTES {
        return false;
    }
    let mut boundary = MAX_DIFF_BYTES.saturating_sub(DIFF_TRUNCATION_MARKER.len());
    while !diff.is_char_boundary(boundary) {
        boundary -= 1;
    }
    diff.truncate(boundary);
    diff.push_str(DIFF_TRUNCATION_MARKER);
    true
}

/// Verified evidence scope for `/diff`, `/review`, and future noninteractive
/// review entry points.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReviewTarget {
    /// Index + working tree relative to HEAD, plus untracked files.
    WorkingTree,
    /// Index relative to HEAD.
    Staged,
    /// Working tree relative to the index, plus untracked files.
    Unstaged,
    /// Working tree relative to the merge-base of HEAD and this revision.
    Base(String),
    /// The patch introduced by exactly this commit.
    Commit(String),
    /// In-memory evidence derived from a write-journal checkpoint.
    Checkpoint(JournalTurnReport),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewEvidence {
    pub target: String,
    pub resolved_revision: Option<String>,
    pub merge_base: Option<String>,
    pub diff: String,
    pub truncated: bool,
}

fn git(root: &Path, args: &[&str]) -> Result<Output, String> {
    Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .map_err(|error| format!("failed to start git: {error}"))
}

fn git_owned<S: AsRef<OsStr>>(root: &Path, args: &[S]) -> Result<Output, String> {
    Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .map_err(|error| format!("failed to start git: {error}"))
}

fn stdout(output: Output, action: &str) -> Result<String, String> {
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        Err(git_failure(&output, action))
    }
}

fn git_failure(output: &Output, action: &str) -> String {
    let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if detail.is_empty() {
        format!("git could not {action}")
    } else {
        format!("git could not {action}: {detail}")
    }
}

pub fn repository_root(start: &Path) -> Result<PathBuf, String> {
    let output = git(start, &["rev-parse", "--show-toplevel"])?;
    let root = stdout(output, "locate the repository")?;
    if root.is_empty() {
        Err("not inside a Git worktree".to_string())
    } else {
        Ok(PathBuf::from(root))
    }
}

fn lexical_absolute(path: &Path) -> Result<PathBuf, String> {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                normalized.push(component.as_os_str());
            }
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    return Err(format!("path '{}' escapes the repository", path.display()));
                }
            }
        }
    }
    Ok(normalized)
}

/// Resolve a user-entered `/diff [path]` relative to the directory where Kitsu
/// started, then convert it to the root-relative Git pathspec Git expects.
pub fn pathspec_from_directory(
    root: &Path,
    directory: &Path,
    requested: &str,
) -> Result<Option<String>, String> {
    if requested.trim().is_empty() {
        return Ok(None);
    }
    let root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let directory = std::fs::canonicalize(directory).unwrap_or_else(|_| directory.to_path_buf());
    let requested = Path::new(requested);
    let absolute = if requested.is_absolute() {
        lexical_absolute(requested)?
    } else {
        lexical_absolute(&directory.join(requested))?
    };
    let relative = absolute.strip_prefix(&root).map_err(|_| {
        format!(
            "path '{}' is outside repository {}",
            requested.display(),
            root.display()
        )
    })?;
    let pathspec = if relative.as_os_str().is_empty() {
        ".".to_string()
    } else {
        relative.to_string_lossy().to_string()
    };
    Ok(Some(pathspec))
}

fn append_untracked(diff: &mut String, root: &Path, relative: &Path) {
    let path = root.join(relative);
    let Ok(metadata) = std::fs::symlink_metadata(&path) else {
        return;
    };
    if !metadata.file_type().is_file() && !metadata.file_type().is_symlink() {
        return;
    }
    if !diff.is_empty() && !diff.ends_with('\n') {
        diff.push('\n');
    }
    let label = relative.to_string_lossy();
    diff.push_str(&format!(
        "diff --git a/{label} b/{label}\nnew file (untracked)\n--- /dev/null\n+++ b/{label}\n"
    ));
    if metadata.file_type().is_symlink() {
        diff.push_str("Symbolic link (target and content omitted)\n");
        return;
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let Ok(file) = options.open(&path) else {
        diff.push_str("File content unavailable\n");
        return;
    };
    let remaining = MAX_DIFF_BYTES.saturating_sub(diff.len());
    let mut bytes = Vec::with_capacity(remaining.min(64 * 1024));
    if file
        .take(remaining.saturating_add(1) as u64)
        .read_to_end(&mut bytes)
        .is_err()
    {
        diff.push_str("File content unavailable\n");
        return;
    }
    match String::from_utf8(bytes) {
        Ok(text) => {
            let lines: Vec<_> = text.lines().collect();
            diff.push_str(&format!("@@ -0,0 +1,{} @@\n", lines.len()));
            for line in lines {
                diff.push('+');
                diff.push_str(line);
                diff.push('\n');
                if diff.len() >= MAX_DIFF_BYTES {
                    return;
                }
            }
        }
        Err(_) => diff.push_str("Binary file (content omitted)\n"),
    }
}

/// Return the verified working-tree diff, including untracked text files.
/// `path_filter`, when present, is passed to Git as a pathspec after `--`.
#[cfg(test)]
pub fn diff(start: &Path, path_filter: Option<&str>) -> Result<String, String> {
    diff_excluding(start, path_filter, &[])
}

/// Return the verified working-tree diff while omitting Kitsu-owned
/// configuration/session paths, even when they live inside the repository.
#[cfg(test)]
pub fn diff_excluding(
    start: &Path,
    path_filter: Option<&str>,
    excluded: &[PathBuf],
) -> Result<String, String> {
    review_evidence_excluding(start, ReviewTarget::WorkingTree, path_filter, excluded)
        .map(|evidence| evidence.diff)
}

fn review_pathspecs(
    root: &Path,
    path_filter: Option<&str>,
    excluded: &[PathBuf],
) -> Result<Vec<OsString>, String> {
    if path_filter.is_some_and(|path| path.contains('\0')) {
        return Err("review path contains an interior NUL byte".to_string());
    }
    let filter_spec = path_filter
        .filter(|path| !path.trim().is_empty())
        .map(|path| OsString::from(format!(":(literal){path}")));
    let excluded = live_excluded_paths(root, excluded)?;
    let mut exclusion_specs = Vec::new();
    for path in &excluded {
        for path in path_identities(root, path) {
            if let Ok(relative) = path.strip_prefix(root) {
                if !relative.as_os_str().is_empty() {
                    exclusion_specs.push(literal_exclusion_pathspec(relative));
                }
            }
        }
    }
    exclusion_specs.sort_unstable();
    exclusion_specs.dedup();
    let mut specs = Vec::with_capacity(usize::from(filter_spec.is_some()) + exclusion_specs.len());
    if let Some(path) = filter_spec {
        specs.push(path);
    }
    specs.extend(exclusion_specs);
    Ok(specs)
}

fn literal_exclusion_pathspec(relative: &Path) -> OsString {
    const PREFIX: &str = ":(exclude,literal)";
    #[cfg(unix)]
    {
        use std::os::unix::ffi::{OsStrExt, OsStringExt};

        let mut bytes = PREFIX.as_bytes().to_vec();
        bytes.extend_from_slice(relative.as_os_str().as_bytes());
        OsString::from_vec(bytes)
    }
    #[cfg(not(unix))]
    {
        let mut spec = OsString::from(PREFIX);
        spec.push(relative.as_os_str());
        spec
    }
}

/// Unix hard links do not have a canonical "source" path, so live review
/// exclusion also compares regular files by `(device, inode)`. Hard links
/// cannot cross filesystems (the device component prevents inode collisions),
/// and ordinary Unix filesystems do not permit directory hard links. On other
/// platforms review retains the existing lexical/canonical path exclusions.
#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct LiveFileIdentity {
    device: u64,
    inode: u64,
}

#[cfg(unix)]
fn live_file_identity(metadata: &std::fs::Metadata) -> Option<LiveFileIdentity> {
    use std::os::unix::fs::MetadataExt;

    metadata.is_file().then_some(LiveFileIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

#[cfg(unix)]
fn collect_live_file_identities(
    path: &Path,
    identities: &mut HashSet<LiveFileIdentity>,
) -> Result<(), String> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(format!(
                "could not inspect private review exclusion '{}': {error}",
                path.display()
            ))
        }
    };
    if metadata.file_type().is_symlink() {
        return Ok(());
    }
    if let Some(identity) = live_file_identity(&metadata) {
        identities.insert(identity);
        return Ok(());
    }
    if !metadata.is_dir() {
        return Ok(());
    }
    let entries = std::fs::read_dir(path).map_err(|error| {
        format!(
            "could not inspect private review exclusion '{}': {error}",
            path.display()
        )
    })?;
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(format!(
                    "could not inspect private review exclusion '{}': {error}",
                    path.display()
                ))
            }
        };
        collect_live_file_identities(&entry.path(), identities)?;
    }
    Ok(())
}

#[cfg(unix)]
fn live_git_paths(root: &Path) -> Result<Vec<PathBuf>, String> {
    // Ask Git for exactly the tracked and reviewable-untracked worktree paths.
    // This avoids walking object stores, ignored build trees, or nested repos.
    let output = git(
        root,
        &[
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ],
    )?;
    if !output.status.success() {
        return Err(git_failure(
            &output,
            "list worktree paths for private-alias checks",
        ));
    }
    Ok(output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|relative| {
            use std::os::unix::ffi::OsStringExt;
            root.join(std::ffi::OsString::from_vec(relative.to_vec()))
        })
        .collect())
}

fn live_excluded_paths(root: &Path, excluded: &[PathBuf]) -> Result<Vec<PathBuf>, String> {
    let mut expanded = excluded.to_vec();
    #[cfg(unix)]
    {
        let mut protected = HashSet::new();
        for path in excluded {
            for identity_path in path_identities(root, path) {
                collect_live_file_identities(&identity_path, &mut protected)?;
            }
        }
        if !protected.is_empty() {
            for path in live_git_paths(root)? {
                let metadata = match std::fs::symlink_metadata(&path) {
                    Ok(metadata) => metadata,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(error) => {
                        return Err(format!(
                            "could not inspect repository path '{}' for private aliases: {error}",
                            path.display()
                        ))
                    }
                };
                if metadata.file_type().is_symlink() {
                    continue;
                }
                if live_file_identity(&metadata)
                    .is_some_and(|identity| protected.contains(&identity))
                {
                    expanded.push(path);
                }
            }
        }
        expanded.sort_unstable();
        expanded.dedup();
    }
    Ok(expanded)
}

fn run_git_diff(
    root: &Path,
    mut args: Vec<OsString>,
    pathspecs: &[OsString],
    action: &str,
) -> Result<String, String> {
    args.push("--".into());
    args.extend(pathspecs.iter().cloned());
    let output = git_owned(root, &args)?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(if detail.is_empty() {
            format!("git could not {action}")
        } else {
            format!("git could not {action}: {detail}")
        })
    }
}

fn resolve_commit(root: &Path, revision: &str) -> Result<String, String> {
    if revision.is_empty()
        || revision.len() > 1024
        || revision.starts_with('-')
        || revision.trim() != revision
        || revision.chars().any(|character| character.is_control())
        || revision.chars().any(char::is_whitespace)
    {
        return Err("invalid revision: expected a non-option Git revision without whitespace or control characters".to_string());
    }
    let expression = format!("{revision}^{{commit}}");
    let output = git_owned(
        root,
        &[
            "rev-parse".to_string(),
            "--verify".to_string(),
            "--end-of-options".to_string(),
            expression,
        ],
    )?;
    let resolved = stdout(output, "resolve the revision")?;
    if !(40..=64).contains(&resolved.len())
        || !resolved.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err("git returned an invalid object ID while resolving the revision".to_string());
    }
    Ok(resolved)
}

fn merge_base(root: &Path, resolved: &str) -> Result<String, String> {
    let output = git(root, &["merge-base", "HEAD", resolved])?;
    let base = stdout(output, "find the merge base")?;
    if !(40..=64).contains(&base.len()) || !base.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("git returned an invalid merge-base object ID".to_string());
    }
    Ok(base)
}

fn head_exists(root: &Path) -> Result<bool, String> {
    let output = git_owned(
        root,
        &[
            "rev-parse".to_string(),
            "--verify".to_string(),
            "--quiet".to_string(),
            "--end-of-options".to_string(),
            "HEAD^{commit}".to_string(),
        ],
    )?;
    if output.status.success() {
        Ok(true)
    } else if output.status.code() == Some(1) && output.stderr.is_empty() {
        Ok(false)
    } else {
        Err(git_failure(&output, "resolve HEAD"))
    }
}

fn append_untracked_files(
    rendered: &mut String,
    root: &Path,
    pathspecs: &[OsString],
) -> Result<(), String> {
    let mut args: Vec<OsString> = vec![
        "ls-files".into(),
        "-z".into(),
        "--others".into(),
        "--exclude-standard".into(),
        "--".into(),
    ];
    args.extend(pathspecs.iter().cloned());
    let untracked = git_owned(root, &args)?;
    if !untracked.status.success() {
        let detail = String::from_utf8_lossy(&untracked.stderr)
            .trim()
            .to_string();
        return Err(if detail.is_empty() {
            "git could not list untracked files".to_string()
        } else {
            format!("git could not list untracked files: {detail}")
        });
    }
    for relative in untracked
        .stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
    {
        #[cfg(unix)]
        let relative = {
            use std::os::unix::ffi::OsStringExt;
            PathBuf::from(std::ffi::OsString::from_vec(relative.to_vec()))
        };
        #[cfg(not(unix))]
        let relative = PathBuf::from(String::from_utf8_lossy(relative).into_owned());
        append_untracked(rendered, root, &relative);
        if rendered.len() >= MAX_DIFF_BYTES {
            break;
        }
    }
    Ok(())
}

fn path_identities(root: &Path, path: &Path) -> Vec<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    };
    let lexical = lexical_absolute(&absolute).unwrap_or_else(|_| absolute.clone());
    let mut identities = vec![lexical];
    if let Ok(canonical) = std::fs::canonicalize(&absolute) {
        if !identities.contains(&canonical) {
            identities.push(canonical);
        }
    }
    identities
}

fn checkpoint_change_included(
    root: &Path,
    path: &Path,
    path_filter: Option<&str>,
    excluded: &[PathBuf],
) -> bool {
    let change_identities = path_identities(root, path);
    if excluded.iter().any(|excluded| {
        let excluded_identities = path_identities(root, excluded);
        change_identities.iter().any(|path| {
            excluded_identities
                .iter()
                .any(|excluded| path == excluded || path.starts_with(excluded))
        })
    }) {
        return false;
    }
    let Some(filter) = path_filter.filter(|filter| !filter.trim().is_empty()) else {
        return true;
    };
    let filter = Path::new(filter);
    change_identities.iter().any(|path| {
        path.strip_prefix(root)
            .is_ok_and(|relative| relative == filter || relative.starts_with(filter))
    })
}

fn checkpoint_evidence(
    start: &Path,
    report: JournalTurnReport,
    path_filter: Option<&str>,
    excluded: &[PathBuf],
) -> Result<ReviewEvidence, String> {
    if path_filter.is_some_and(|path| path.contains('\0')) {
        return Err("review path contains an interior NUL byte".to_string());
    }
    let root = repository_root(start).unwrap_or_else(|_| start.to_path_buf());
    let root = std::fs::canonicalize(&root).unwrap_or(root);
    let mut diff = String::new();
    let mut truncated = false;
    for change in &report.changes {
        if !checkpoint_change_included(&root, &change.path, path_filter, excluded) {
            continue;
        }
        if !diff.is_empty() && !diff.ends_with('\n') {
            diff.push('\n');
        }
        diff.push_str(&change.unified_diff);
        truncated |= change.diff_truncated;
        if cap_diff(&mut diff) {
            truncated = true;
            break;
        }
    }
    if diff.trim().is_empty() {
        diff = "(checkpoint has no reviewable changes)".to_string();
    }
    Ok(ReviewEvidence {
        target: format!("checkpoint:{}", report.id),
        resolved_revision: None,
        merge_base: None,
        diff,
        truncated,
    })
}

/// Build bounded, verified review evidence for one explicit scope.
#[cfg(test)]
pub fn review_evidence(
    start: &Path,
    target: ReviewTarget,
    path_filter: Option<&str>,
) -> Result<ReviewEvidence, String> {
    review_evidence_excluding(start, target, path_filter, &[])
}

pub fn review_evidence_excluding(
    start: &Path,
    target: ReviewTarget,
    path_filter: Option<&str>,
    excluded: &[PathBuf],
) -> Result<ReviewEvidence, String> {
    let target = match target {
        ReviewTarget::Checkpoint(report) => {
            return checkpoint_evidence(start, report, path_filter, excluded)
        }
        target => target,
    };

    let root = repository_root(start)?;
    let pathspecs = review_pathspecs(&root, path_filter, excluded)?;
    let common: [OsString; 4] = [
        "diff".into(),
        "--no-ext-diff".into(),
        "--no-textconv".into(),
        "--no-color".into(),
    ];
    let mut resolved_revision = None;
    let mut resolved_merge_base = None;
    let (target_label, mut rendered, include_untracked, clean_message) = match target {
        ReviewTarget::WorkingTree => {
            let rendered = if head_exists(&root)? {
                let mut args = common.to_vec();
                args.push("HEAD".into());
                run_git_diff(&root, args, &pathspecs, "read the working tree")?
            } else {
                // An unborn repository has no HEAD. Preserve index and
                // worktree evidence instead of treating it as clean.
                let mut staged = common.to_vec();
                staged.push("--cached".into());
                let mut fallback =
                    run_git_diff(&root, staged, &pathspecs, "read the unborn index")?;
                fallback.push_str(&run_git_diff(
                    &root,
                    common.to_vec(),
                    &pathspecs,
                    "read the working tree",
                )?);
                fallback
            };
            (
                "working_tree".to_string(),
                rendered,
                true,
                "(working tree clean)",
            )
        }
        ReviewTarget::Staged => {
            let mut args = common.to_vec();
            args.push("--cached".into());
            (
                "staged".to_string(),
                run_git_diff(&root, args, &pathspecs, "read staged changes")?,
                false,
                "(no staged changes)",
            )
        }
        ReviewTarget::Unstaged => (
            "unstaged".to_string(),
            run_git_diff(&root, common.to_vec(), &pathspecs, "read unstaged changes")?,
            true,
            "(no unstaged or untracked changes)",
        ),
        ReviewTarget::Base(revision) => {
            let resolved = resolve_commit(&root, &revision)?;
            let base = merge_base(&root, &resolved)?;
            let mut args = common.to_vec();
            args.push(base.clone().into());
            let rendered = run_git_diff(&root, args, &pathspecs, "diff from the merge base")?;
            resolved_revision = Some(resolved);
            resolved_merge_base = Some(base);
            (
                format!("base:{revision}"),
                rendered,
                true,
                "(no changes from merge base)",
            )
        }
        ReviewTarget::Commit(revision) => {
            let resolved = resolve_commit(&root, &revision)?;
            let mut args: Vec<OsString> = vec![
                "show".into(),
                "--format=".into(),
                "--no-ext-diff".into(),
                "--no-textconv".into(),
                "--no-color".into(),
                resolved.clone().into(),
            ];
            args.push("--".into());
            args.extend(pathspecs.iter().cloned());
            let output = git_owned(&root, &args)?;
            let rendered = if output.status.success() {
                String::from_utf8_lossy(&output.stdout).to_string()
            } else {
                return Err(git_failure(&output, "show the commit"));
            };
            resolved_revision = Some(resolved);
            (
                format!("commit:{revision}"),
                rendered,
                false,
                "(commit has no changes for this path)",
            )
        }
        ReviewTarget::Checkpoint(_) => unreachable!("handled above"),
    };

    let mut truncated = cap_diff(&mut rendered);
    if include_untracked && !truncated {
        append_untracked_files(&mut rendered, &root, &pathspecs)?;
        truncated = cap_diff(&mut rendered);
    }
    if rendered.trim().is_empty() {
        rendered = clean_message.to_string();
    }
    Ok(ReviewEvidence {
        target: target_label,
        resolved_revision,
        merge_base: resolved_merge_base,
        diff: rendered.trim_end().to_string(),
        truncated,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_git(root: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .status()
            .expect("start git");
        assert!(status.success(), "git {args:?}");
    }

    fn git_stdout(root: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .output()
            .expect("start git");
        assert!(output.status.success(), "git {args:?}");
        String::from_utf8(output.stdout).unwrap().trim().to_string()
    }

    fn commit_all(root: &Path, message: &str) -> String {
        run_git(root, &["add", "."]);
        run_git(
            root,
            &[
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.com",
                "commit",
                "--quiet",
                "-m",
                message,
            ],
        );
        git_stdout(root, &["rev-parse", "HEAD"])
    }

    #[test]
    fn reports_tracked_and_untracked_ground_truth() {
        let repo = tempfile::tempdir().unwrap();
        run_git(repo.path(), &["init", "--quiet"]);
        std::fs::write(repo.path().join("tracked.txt"), "old\n").unwrap();
        run_git(repo.path(), &["add", "tracked.txt"]);
        run_git(
            repo.path(),
            &[
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.com",
                "commit",
                "--quiet",
                "-m",
                "base",
            ],
        );
        std::fs::write(repo.path().join("tracked.txt"), "new\n").unwrap();
        std::fs::write(repo.path().join("fresh.txt"), "hello\n").unwrap();

        let rendered = diff(repo.path(), None).unwrap();
        assert!(rendered.contains("-old"));
        assert!(rendered.contains("+new"));
        assert!(rendered.contains("new file (untracked)"));
        assert!(rendered.contains("+hello"));
    }

    #[test]
    fn review_targets_separate_staged_unstaged_and_working_tree_evidence() {
        let repo = tempfile::tempdir().unwrap();
        run_git(repo.path(), &["init", "--quiet"]);
        let tracked = repo.path().join("tracked.txt");
        std::fs::write(&tracked, "base\n").unwrap();
        commit_all(repo.path(), "base");

        std::fs::write(&tracked, "index\n").unwrap();
        run_git(repo.path(), &["add", "tracked.txt"]);
        std::fs::write(&tracked, "worktree\n").unwrap();
        std::fs::write(repo.path().join("fresh.txt"), "untracked\n").unwrap();

        let staged = review_evidence(repo.path(), ReviewTarget::Staged, None).unwrap();
        assert!(staged.diff.contains("+index"), "{}", staged.diff);
        assert!(!staged.diff.contains("worktree"));
        assert!(!staged.diff.contains("fresh.txt"));

        let unstaged = review_evidence(repo.path(), ReviewTarget::Unstaged, None).unwrap();
        assert!(unstaged.diff.contains("-index"), "{}", unstaged.diff);
        assert!(unstaged.diff.contains("+worktree"), "{}", unstaged.diff);
        assert!(unstaged.diff.contains("fresh.txt"), "{}", unstaged.diff);
        assert!(unstaged.diff.contains("+untracked"), "{}", unstaged.diff);

        let working = review_evidence(repo.path(), ReviewTarget::WorkingTree, None).unwrap();
        assert!(working.diff.contains("-base"), "{}", working.diff);
        assert!(working.diff.contains("+worktree"), "{}", working.diff);
        assert!(working.diff.contains("fresh.txt"), "{}", working.diff);
    }

    #[test]
    fn base_uses_merge_base_and_commit_is_an_exact_historical_patch() {
        let repo = tempfile::tempdir().unwrap();
        run_git(repo.path(), &["init", "--quiet"]);
        let tracked = repo.path().join("tracked.txt");
        std::fs::write(&tracked, "base\n").unwrap();
        let base = commit_all(repo.path(), "base");
        std::fs::write(&tracked, "committed\n").unwrap();
        let commit = commit_all(repo.path(), "feature");
        std::fs::write(&tracked, "dirty\n").unwrap();

        let exact =
            review_evidence(repo.path(), ReviewTarget::Commit("HEAD".to_string()), None).unwrap();
        assert_eq!(exact.resolved_revision.as_deref(), Some(commit.as_str()));
        assert!(exact.diff.contains("-base"), "{}", exact.diff);
        assert!(exact.diff.contains("+committed"), "{}", exact.diff);
        assert!(!exact.diff.contains("dirty"));

        let from_base =
            review_evidence(repo.path(), ReviewTarget::Base("HEAD~1".to_string()), None).unwrap();
        assert_eq!(from_base.merge_base.as_deref(), Some(base.as_str()));
        assert_eq!(from_base.resolved_revision.as_deref(), Some(base.as_str()));
        assert!(from_base.diff.contains("-base"), "{}", from_base.diff);
        assert!(from_base.diff.contains("+dirty"), "{}", from_base.diff);
    }

    #[test]
    fn malicious_revision_strings_are_rejected_as_data_not_options_or_shell() {
        let outer = tempfile::tempdir().unwrap();
        let repo = outer.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        run_git(&repo, &["init", "--quiet"]);
        std::fs::write(repo.join("tracked.txt"), "base\n").unwrap();
        commit_all(&repo, "base");
        let sentinel = outer.path().join("must-not-exist");

        for revision in [
            "--help".to_string(),
            "HEAD\n--help".to_string(),
            "HEAD --help".to_string(),
            format!("HEAD;touch{}", sentinel.display()),
        ] {
            for target in [
                ReviewTarget::Commit(revision.clone()),
                ReviewTarget::Base(revision.clone()),
            ] {
                assert!(review_evidence(&repo, target, None).is_err());
                assert!(!sentinel.exists());
            }
        }
    }

    #[test]
    fn every_live_scope_preserves_private_exclusions_and_untracked_rules() {
        let repo = tempfile::tempdir().unwrap();
        run_git(repo.path(), &["init", "--quiet"]);
        let private = repo.path().join("kitsu.toml");
        let visible = repo.path().join("visible.txt");
        std::fs::write(&private, "PRIVATE_BASE\n").unwrap();
        std::fs::write(&visible, "visible base\n").unwrap();
        commit_all(repo.path(), "base");
        std::fs::write(&private, "PRIVATE_STAGED\n").unwrap();
        std::fs::write(&visible, "VISIBLE_STAGED\n").unwrap();
        run_git(repo.path(), &["add", "."]);
        let private_dir = repo.path().join(".kitsu-private");
        std::fs::create_dir(&private_dir).unwrap();
        std::fs::write(private_dir.join("session.json"), "PRIVATE_UNTRACKED\n").unwrap();
        std::fs::write(repo.path().join("fresh.txt"), "VISIBLE_UNTRACKED\n").unwrap();
        let excluded = vec![private.clone(), private_dir.clone()];

        for target in [
            ReviewTarget::WorkingTree,
            ReviewTarget::Staged,
            ReviewTarget::Unstaged,
            ReviewTarget::Base("HEAD".to_string()),
            ReviewTarget::Commit("HEAD".to_string()),
        ] {
            let evidence = review_evidence_excluding(repo.path(), target, None, &excluded).unwrap();
            assert!(!evidence.diff.contains("PRIVATE_BASE"));
            assert!(!evidence.diff.contains("PRIVATE_STAGED"));
            assert!(!evidence.diff.contains("PRIVATE_UNTRACKED"));
            assert!(!evidence.diff.contains("kitsu.toml"));
            assert!(!evidence.diff.contains("session.json"));
        }

        let staged =
            review_evidence_excluding(repo.path(), ReviewTarget::Staged, None, &excluded).unwrap();
        assert!(staged.diff.contains("VISIBLE_STAGED"));
        assert!(!staged.diff.contains("VISIBLE_UNTRACKED"));
        let unstaged =
            review_evidence_excluding(repo.path(), ReviewTarget::Unstaged, None, &excluded)
                .unwrap();
        assert!(unstaged.diff.contains("VISIBLE_UNTRACKED"));
    }

    #[cfg(unix)]
    #[test]
    fn private_symlink_exclusion_preserves_its_apparent_repository_path() {
        use std::os::unix::fs::symlink;

        let repo = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        run_git(repo.path(), &["init", "--quiet"]);
        let private_link = repo.path().join("private-link");
        std::fs::write(outside.path().join("target"), "PRIVATE_TARGET\n").unwrap();
        symlink(outside.path().join("target"), &private_link).unwrap();
        std::fs::write(repo.path().join("visible.txt"), "VISIBLE\n").unwrap();

        let untracked = review_evidence_excluding(
            repo.path(),
            ReviewTarget::Unstaged,
            None,
            std::slice::from_ref(&private_link),
        )
        .unwrap();
        assert!(untracked.diff.contains("VISIBLE"));
        assert!(!untracked.diff.contains("private-link"));
        assert!(!untracked
            .diff
            .contains(&outside.path().to_string_lossy().to_string()));

        commit_all(repo.path(), "base");
        let historical = review_evidence_excluding(
            repo.path(),
            ReviewTarget::Commit("HEAD".to_string()),
            None,
            &[private_link],
        )
        .unwrap();
        assert!(historical.diff.contains("VISIBLE"));
        assert!(!historical.diff.contains("private-link"));
        assert!(!historical
            .diff
            .contains(&outside.path().to_string_lossy().to_string()));
    }

    #[cfg(unix)]
    #[test]
    fn live_review_excludes_hard_link_aliases_of_private_directory_files() {
        use std::os::unix::ffi::OsStringExt;

        let repo = tempfile::tempdir().unwrap();
        let private = tempfile::tempdir().unwrap();
        run_git(repo.path(), &["init", "--quiet"]);

        let session = private.path().join("session.json");
        let alias = repo.path().join("apparently-public.json");
        let visible = repo.path().join("visible.txt");
        std::fs::write(&session, "PRIVATE_BASE\n").unwrap();
        std::fs::hard_link(&session, &alias).unwrap();
        std::fs::write(&visible, "visible base\n").unwrap();
        commit_all(repo.path(), "base");

        std::fs::write(&session, "PRIVATE_STAGED\n").unwrap();
        std::fs::write(&visible, "VISIBLE_STAGED\n").unwrap();
        run_git(repo.path(), &["add", "."]);
        std::fs::write(&session, "PRIVATE_WORKTREE\n").unwrap();
        std::fs::write(&visible, "VISIBLE_WORKTREE\n").unwrap();
        let untracked_alias = repo.path().join("untracked-private-alias.json");
        std::fs::hard_link(&session, &untracked_alias).unwrap();
        let byte_name_alias = repo.path().join(std::ffi::OsString::from_vec(
            b"private-alias-\xff.json".to_vec(),
        ));
        std::fs::hard_link(&session, &byte_name_alias).unwrap();

        for target in [
            ReviewTarget::WorkingTree,
            ReviewTarget::Staged,
            ReviewTarget::Unstaged,
            ReviewTarget::Base("HEAD".to_string()),
            ReviewTarget::Commit("HEAD".to_string()),
        ] {
            let evidence = review_evidence_excluding(
                repo.path(),
                target,
                None,
                &[private.path().to_path_buf()],
            )
            .unwrap();
            assert!(evidence.diff.contains("visible"), "{}", evidence.diff);
            assert!(!evidence.diff.contains("PRIVATE_"), "{}", evidence.diff);
            assert!(
                !evidence.diff.contains("apparently-public.json"),
                "{}",
                evidence.diff
            );
            assert!(
                !evidence.diff.contains("untracked-private-alias.json"),
                "{}",
                evidence.diff
            );
        }

        // An explicit user filter for the innocent-looking alias must not
        // override the private exclusion pathspec.
        let filtered = review_evidence_excluding(
            repo.path(),
            ReviewTarget::WorkingTree,
            Some("apparently-public.json"),
            &[private.path().to_path_buf()],
        )
        .unwrap();
        assert!(!filtered.diff.contains("PRIVATE_"), "{}", filtered.diff);
        assert!(
            !filtered.diff.contains("apparently-public.json"),
            "{}",
            filtered.diff
        );
    }

    #[test]
    fn checkpoint_scope_filters_private_paths_and_retains_bounded_evidence() {
        use kitsu_tools::builtins::fs::{JournalChangeReport, JournalTurnReport};

        let repo = tempfile::tempdir().unwrap();
        run_git(repo.path(), &["init", "--quiet"]);
        let visible = repo.path().join("visible.txt");
        let private = repo.path().join("private.txt");
        let report = JournalTurnReport {
            id: 7,
            label: "edit".to_string(),
            changes: vec![
                JournalChangeReport {
                    path: visible,
                    change: "updated",
                    before_bytes: Some(3),
                    after_bytes: 3,
                    first_changed_line: 1,
                    unified_diff:
                        "--- a/visible.txt\n+++ b/visible.txt\n@@ -1,1 +1,1 @@\n-old\n+new\n"
                            .to_string(),
                    diff_truncated: false,
                },
                JournalChangeReport {
                    path: private.clone(),
                    change: "updated",
                    before_bytes: Some(6),
                    after_bytes: 6,
                    first_changed_line: 1,
                    unified_diff:
                        "--- a/private.txt\n+++ b/private.txt\n@@ -1,1 +1,1 @@\n-secret\n+SECRET\n"
                            .to_string(),
                    diff_truncated: false,
                },
            ],
            unified_diff: String::new(),
            diff_truncated: false,
        };
        let evidence = review_evidence_excluding(
            repo.path(),
            ReviewTarget::Checkpoint(report),
            None,
            &[private],
        )
        .unwrap();
        assert_eq!(evidence.target, "checkpoint:7");
        assert!(evidence.diff.contains("visible.txt"));
        assert!(!evidence.diff.contains("private.txt"));
        assert!(!evidence.diff.contains("SECRET"));
    }

    #[test]
    fn clean_repository_is_explicit() {
        let repo = tempfile::tempdir().unwrap();
        run_git(repo.path(), &["init", "--quiet"]);
        assert_eq!(diff(repo.path(), None).unwrap(), "(working tree clean)");
    }

    #[test]
    fn relative_filter_is_resolved_from_launch_subdirectory() {
        let repo = tempfile::tempdir().unwrap();
        let package = repo.path().join("crates/widget");
        std::fs::create_dir_all(package.join("src")).unwrap();
        run_git(repo.path(), &["init", "--quiet"]);
        std::fs::write(package.join("src/lib.rs"), "old\n").unwrap();
        run_git(repo.path(), &["add", "."]);
        run_git(
            repo.path(),
            &[
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.com",
                "commit",
                "--quiet",
                "-m",
                "base",
            ],
        );
        std::fs::write(package.join("src/lib.rs"), "new\n").unwrap();

        let pathspec = pathspec_from_directory(repo.path(), &package, "src/lib.rs")
            .unwrap()
            .unwrap();
        assert_eq!(pathspec, "crates/widget/src/lib.rs");
        assert!(diff(repo.path(), Some(&pathspec)).unwrap().contains("+new"));
        assert!(pathspec_from_directory(repo.path(), &package, "../../../outside").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn nul_delimited_untracked_names_are_not_omitted() {
        let repo = tempfile::tempdir().unwrap();
        run_git(repo.path(), &["init", "--quiet"]);
        std::fs::write(repo.path().join("odd\nname.txt"), "SPECIAL_NAME_CONTENT\n").unwrap();
        let rendered = diff(repo.path(), None).unwrap();
        assert!(rendered.contains("SPECIAL_NAME_CONTENT"), "{rendered}");
    }

    #[test]
    fn huge_untracked_file_is_bounded() {
        let repo = tempfile::tempdir().unwrap();
        run_git(repo.path(), &["init", "--quiet"]);
        std::fs::write(repo.path().join("huge.txt"), vec![b'x'; MAX_DIFF_BYTES * 2]).unwrap();
        let rendered = diff(repo.path(), None).unwrap();
        assert!(rendered.len() <= MAX_DIFF_BYTES);
        assert!(rendered.contains("diff truncated"));
    }

    #[test]
    fn private_runtime_paths_are_excluded_from_verified_diff() {
        let repo = tempfile::tempdir().unwrap();
        run_git(repo.path(), &["init", "--quiet"]);
        let config = repo.path().join("kitsu.toml");
        std::fs::write(&config, "token = 'DO_NOT_SEND'\n").unwrap();
        std::fs::write(repo.path().join("visible.txt"), "VISIBLE_CHANGE\n").unwrap();

        let rendered = diff_excluding(repo.path(), None, std::slice::from_ref(&config)).unwrap();
        assert!(rendered.contains("VISIBLE_CHANGE"));
        assert!(!rendered.contains("DO_NOT_SEND"));
        assert!(!rendered.contains("kitsu.toml"));
    }

    #[cfg(unix)]
    #[test]
    fn untracked_symlink_does_not_disclose_target_contents() {
        use std::os::unix::fs::symlink;

        let repo = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        run_git(repo.path(), &["init", "--quiet"]);
        std::fs::write(outside.path().join("secret.txt"), "DO_NOT_DISCLOSE").unwrap();
        symlink(
            outside.path().join("secret.txt"),
            repo.path().join("linked-secret"),
        )
        .unwrap();

        let rendered = diff(repo.path(), None).unwrap();
        assert!(rendered.contains("Symbolic link"));
        assert!(!rendered.contains("DO_NOT_DISCLOSE"));
        assert!(!rendered.contains(&outside.path().to_string_lossy().to_string()));
    }
}
