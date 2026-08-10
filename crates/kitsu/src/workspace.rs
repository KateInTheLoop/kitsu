//! Read-only workspace inspection used by `/diff`, `/review`, and `/status`.
//!
//! Git is invoked directly (never through a shell) and every command is scoped
//! to the current repository. Model-authored diff text is not evidence; this
//! module reports the state Git and the filesystem actually contain.

use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Output};

const MAX_DIFF_BYTES: usize = 256 * 1024;

fn cap_diff(diff: &mut String) -> bool {
    if diff.len() <= MAX_DIFF_BYTES {
        return false;
    }
    let mut boundary = MAX_DIFF_BYTES;
    while !diff.is_char_boundary(boundary) {
        boundary -= 1;
    }
    diff.truncate(boundary);
    diff.push_str("\n...[diff truncated at 256 KiB]\n");
    true
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceStatus {
    pub root: PathBuf,
    pub branch: String,
    pub changed: usize,
    pub untracked: usize,
}

fn git(root: &Path, args: &[&str]) -> Result<Output, String> {
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
        let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(if detail.is_empty() {
            format!("git could not {action}")
        } else {
            format!("git could not {action}: {detail}")
        })
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

pub fn status(start: &Path) -> Option<WorkspaceStatus> {
    let root = repository_root(start).ok()?;
    let branch = git(&root, &["branch", "--show-current"])
        .ok()
        .and_then(|output| stdout(output, "read the branch").ok())
        .filter(|branch| !branch.is_empty())
        .unwrap_or_else(|| "detached".to_string());
    let porcelain = git(
        &root,
        &["status", "--porcelain=v1", "--untracked-files=all"],
    )
    .ok()
    .and_then(|output| stdout(output, "read status").ok())?;
    let lines: Vec<_> = porcelain.lines().collect();
    let untracked = lines.iter().filter(|line| line.starts_with("?? ")).count();
    Some(WorkspaceStatus {
        root,
        branch,
        changed: lines.len(),
        untracked,
    })
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
pub fn diff_excluding(
    start: &Path,
    path_filter: Option<&str>,
    excluded: &[PathBuf],
) -> Result<String, String> {
    let root = repository_root(start)?;
    let filter_spec = path_filter
        .filter(|path| !path.trim().is_empty())
        .map(|path| format!(":(literal){path}"));
    let exclusion_specs: Vec<String> = excluded
        .iter()
        .filter_map(|path| {
            let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.clone());
            path.strip_prefix(&root).ok().and_then(|relative| {
                (!relative.as_os_str().is_empty())
                    .then(|| format!(":(exclude,literal){}", relative.to_string_lossy()))
            })
        })
        .collect();
    let mut args = vec![
        "diff",
        "--no-ext-diff",
        "--no-textconv",
        "--no-color",
        "HEAD",
        "--",
    ];
    if let Some(path) = filter_spec.as_deref() {
        args.push(path);
    }
    for excluded in &exclusion_specs {
        args.push(excluded);
    }
    let tracked_output = git(&root, &args)?;
    let mut rendered = if tracked_output.status.success() {
        String::from_utf8_lossy(&tracked_output.stdout).to_string()
    } else {
        // An unborn repository has no HEAD. The index and worktree diffs still
        // provide useful evidence in that case.
        let mut fallback = String::new();
        for base in [
            vec![
                "diff",
                "--cached",
                "--no-ext-diff",
                "--no-textconv",
                "--no-color",
                "--",
            ],
            vec!["diff", "--no-ext-diff", "--no-textconv", "--no-color", "--"],
        ] {
            let mut command = base;
            if let Some(path) = filter_spec.as_deref() {
                command.push(path);
            }
            for excluded in &exclusion_specs {
                command.push(excluded);
            }
            let output = git(&root, &command)?;
            if output.status.success() {
                fallback.push_str(&String::from_utf8_lossy(&output.stdout));
            }
        }
        fallback
    };

    if cap_diff(&mut rendered) {
        return Ok(rendered.trim_end().to_string());
    }

    let mut untracked_args = vec!["ls-files", "-z", "--others", "--exclude-standard", "--"];
    if let Some(path) = filter_spec.as_deref() {
        untracked_args.push(path);
    }
    for excluded in &exclusion_specs {
        untracked_args.push(excluded);
    }
    let untracked = git(&root, &untracked_args)?;
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
        append_untracked(&mut rendered, &root, &relative);
        if rendered.len() >= MAX_DIFF_BYTES {
            cap_diff(&mut rendered);
            break;
        }
    }

    if rendered.trim().is_empty() {
        Ok("(working tree clean)".to_string())
    } else {
        Ok(rendered.trim_end().to_string())
    }
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

        let state = status(repo.path()).unwrap();
        assert_eq!(state.changed, 2);
        assert_eq!(state.untracked, 1);
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
        assert!(rendered.len() <= MAX_DIFF_BYTES + 64);
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
