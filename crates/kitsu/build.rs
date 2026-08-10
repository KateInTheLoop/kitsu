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

fn main() {
    let sha = git(&["rev-parse", "--short", "HEAD"]).unwrap_or_else(|| "unknown".to_string());
    let dirty = git(&["status", "--porcelain"])
        .map(|s| !s.is_empty())
        .unwrap_or(false);
    let sha = if dirty { format!("{sha}-dirty") } else { sha };
    let date = git(&["log", "-1", "--format=%cs"]).unwrap_or_else(|| "unknown".to_string());

    println!("cargo:rustc-env=KITSU_GIT_SHA={sha}");
    println!("cargo:rustc-env=KITSU_BUILD_DATE={date}");
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../.git/index");
}
