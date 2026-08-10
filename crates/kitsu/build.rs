//! Embeds build provenance (git commit + date) so `--version` and `/status`
//! can self-diagnose a binary that has drifted from the source/docs sitting
//! next to it, instead of surfacing as an unexplained "unknown command".

use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8(output.stdout).ok()?;
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

/// Cargo does not rerun a build script merely because an arbitrary worktree
/// file changed once explicit `rerun-if-changed` directives exist. Watch the
/// complete source tree (including newly created, untracked module files) and
/// workspace manifests so the embedded `-dirty` state follows real build
/// inputs. Also watch the symbolic branch ref: `.git/HEAD` itself does not
/// change when another commit is made on the checked-out branch.
fn emit_rerun_inputs() {
    for path in ["../../Cargo.toml", "../../Cargo.lock", "../../crates"] {
        println!("cargo:rerun-if-changed={path}");
    }

    for git_path in ["HEAD", "index", "packed-refs"] {
        if let Some(path) = git(&["rev-parse", "--git-path", git_path]) {
            println!("cargo:rerun-if-changed={path}");
        }
    }
    if let Some(head_ref) = git(&["symbolic-ref", "-q", "HEAD"]) {
        if let Some(path) = git(&["rev-parse", "--git-path", &head_ref]) {
            println!("cargo:rerun-if-changed={path}");
        }
    }
}

fn main() {
    emit_rerun_inputs();

    let sha = git(&["rev-parse", "--short", "HEAD"]).unwrap_or_else(|| "unknown".to_string());
    // Keep provenance scoped to inputs that can affect the workspace build.
    // This includes untracked source modules, but deliberately ignores an
    // unrelated scratch file or generated audit report elsewhere in the repo.
    let dirty = git(&[
        "status",
        "--porcelain",
        "--untracked-files=all",
        "--",
        ":/Cargo.toml",
        ":/Cargo.lock",
        ":/crates",
    ])
    .map(|s| !s.is_empty())
    .unwrap_or(false);
    let sha = if dirty { format!("{sha}-dirty") } else { sha };
    let date = git(&["log", "-1", "--format=%cs"]).unwrap_or_else(|| "unknown".to_string());

    println!("cargo:rustc-env=KITSU_GIT_SHA={sha}");
    println!("cargo:rustc-env=KITSU_BUILD_DATE={date}");
}
