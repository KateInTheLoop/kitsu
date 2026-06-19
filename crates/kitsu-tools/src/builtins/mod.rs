//! Built-in tools. Read-only filesystem helpers, gated by a [`Policy`].

pub mod fs;

use std::sync::Arc;

use crate::policy::Policy;
use crate::Registry;

/// Register the available read-only fs tools against `registry`, bound to
/// `policy`. `repo.grep` is registered only when `rg` is on PATH; otherwise it
/// is simply not advertised (design Gap 7).
pub fn register_fs_tools(registry: &mut Registry, policy: Arc<Policy>) {
    let (d, h) = fs::read_file_tool(policy.clone());
    registry.register_with_descriptor(d, h);

    let (d, h) = fs::list_dir_tool(policy.clone());
    registry.register_with_descriptor(d, h);

    if rg_available() {
        let (d, h) = fs::grep_tool(policy);
        registry.register_with_descriptor(d, h);
    }
}

/// Is ripgrep (`rg`) available on PATH?
fn rg_available() -> bool {
    std::process::Command::new("rg")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}
