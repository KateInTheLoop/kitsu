//! Embeds build provenance (git commit + date) so `--version` and `/status`
//! can self-diagnose a binary that has drifted from the source/docs sitting
//! next to it, instead of surfacing as an unexplained "unknown command".

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug)]
struct GitLayout {
    repository: PathBuf,
    git_dir: PathBuf,
    common_dir: PathBuf,
}

fn git(cwd: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .current_dir(cwd)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .args(args)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8(output.stdout).ok()?;
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

fn absolute_path(cwd: &Path, value: &str) -> PathBuf {
    let path = PathBuf::from(value);
    if path.is_absolute() {
        path
    } else {
        cwd.join(path)
    }
}

fn git_layout(workspace: &Path) -> Option<GitLayout> {
    let repository = absolute_path(
        workspace,
        &git(workspace, &["rev-parse", "--show-toplevel"])?,
    );
    let git_dir = absolute_path(
        workspace,
        &git(workspace, &["rev-parse", "--absolute-git-dir"])?,
    );
    // `--git-common-dir` is the repository `.git` directory in an ordinary
    // checkout and the shared administrative directory in a linked worktree.
    // Git documents a relative result as relative to the current directory.
    let common_dir = absolute_path(
        workspace,
        &git(workspace, &["rev-parse", "--git-common-dir"])?,
    );
    Some(GitLayout {
        repository,
        git_dir,
        common_dir,
    })
}

fn emit_existing(path: impl AsRef<Path>) {
    let path = path.as_ref();
    if path.exists() {
        println!("cargo:rerun-if-changed={}", path.display());
    }
}

fn emit_git_inputs(workspace: &Path, layout: &GitLayout) {
    // Per-worktree files. HEAD is sufficient for detached checkouts; index is
    // required for staged/unstaged dirty-state transitions.
    for name in ["HEAD", "index", "commondir", "gitdir", "config.worktree"] {
        emit_existing(layout.git_dir.join(name));
    }

    // Shared files. A branch can move between packed and loose storage, so
    // watch both packed-refs and the small refs/heads tree. Reftable-backed
    // repositories keep their mutable tables in the reftable directory.
    for name in ["packed-refs", "config"] {
        emit_existing(layout.common_dir.join(name));
    }
    emit_existing(layout.common_dir.join("info").join("exclude"));
    emit_existing(layout.repository.join(".gitignore"));
    emit_existing(layout.common_dir.join("refs").join("heads"));
    emit_existing(layout.common_dir.join("reftable"));

    if let Some(head_ref) = git(workspace, &["symbolic-ref", "-q", "HEAD"]) {
        if let Some(path) = git(workspace, &["rev-parse", "--git-path", &head_ref]) {
            emit_existing(absolute_path(workspace, &path));
        }
    }

    // `.git` is a pointer file in linked worktrees. Watching it detects a
    // checkout being reattached to different administrative metadata without
    // recursively watching an ordinary repository's object database.
    let marker = layout.repository.join(".git");
    if marker.is_file() {
        emit_existing(marker);
    }
}

fn pathspec(repository: &Path, path: &Path) -> String {
    let relative = path.strip_prefix(repository).unwrap_or(path);
    let relative = relative.to_string_lossy().replace('\\', "/");
    format!(":(top,literal){relative}")
}

/// Cargo does not rerun a build script merely because arbitrary Git metadata
/// changes once explicit `rerun-if-changed` directives exist. Watch both the
/// build inputs used for dirty-state calculation and the actual per-worktree /
/// common Git metadata which can advance HEAD without touching source files.
fn emit_rerun_inputs(workspace: &Path, layout: Option<&GitLayout>) {
    for path in [
        workspace.join("Cargo.toml"),
        workspace.join("Cargo.lock"),
        workspace.join("crates"),
    ] {
        emit_existing(path);
    }
    for variable in ["GIT_DIR", "GIT_WORK_TREE", "GIT_COMMON_DIR"] {
        println!("cargo:rerun-if-env-changed={variable}");
    }
    if let Some(layout) = layout {
        emit_git_inputs(workspace, layout);
    }
}

fn main() {
    let manifest_dir = PathBuf::from(
        env::var_os("CARGO_MANIFEST_DIR").unwrap_or_else(|| PathBuf::from(".").into_os_string()),
    );
    let workspace = manifest_dir
        .parent()
        .and_then(Path::parent)
        .unwrap_or(&manifest_dir)
        .to_path_buf();
    let layout = git_layout(&workspace);
    emit_rerun_inputs(&workspace, layout.as_ref());

    let sha = git(&workspace, &["rev-parse", "--short=12", "HEAD"])
        .unwrap_or_else(|| "unknown".to_string());
    // Keep provenance scoped to inputs that can affect the workspace build.
    // This includes untracked source modules, but deliberately ignores an
    // unrelated scratch file or generated audit report elsewhere in the repo.
    let dirty = layout
        .as_ref()
        .and_then(|layout| {
            let tracked = [
                pathspec(&layout.repository, &workspace.join("Cargo.toml")),
                pathspec(&layout.repository, &workspace.join("Cargo.lock")),
                pathspec(&layout.repository, &workspace.join("crates")),
            ];
            let mut args = vec!["status", "--porcelain", "--untracked-files=all", "--"];
            args.extend(tracked.iter().map(String::as_str));
            git(&workspace, &args)
        })
        .map(|status| !status.is_empty())
        .unwrap_or(false);
    let sha = if dirty { format!("{sha}-dirty") } else { sha };
    let date =
        git(&workspace, &["log", "-1", "--format=%cs"]).unwrap_or_else(|| "unknown".to_string());

    println!("cargo:rustc-env=KITSU_GIT_SHA={sha}");
    println!("cargo:rustc-env=KITSU_BUILD_DATE={date}");
}
