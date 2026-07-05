//! Built-in tools. Filesystem helpers, gated by runtime [`Permissions`].

pub mod fs;

use std::sync::Arc;

use crate::permissions::Permissions;
use crate::Registry;

/// Register the read fs tools against `registry`, bound to `permissions`.
/// `repo_grep` is registered only when `rg` is on PATH; otherwise it is simply
/// not advertised (design Gap 7).
pub fn register_fs_tools(registry: &mut Registry, policy: Arc<Permissions>) {
    let (d, h) = fs::read_file_tool(policy.clone());
    registry.register_with_descriptor(d, h);

    let (d, h) = fs::list_dir_tool(policy.clone());
    registry.register_with_descriptor(d, h);

    if rg_available() {
        let (d, h) = fs::grep_tool(policy);
        registry.register_with_descriptor(d, h);
    }
}

/// Register the write-capable fs tool (`fs_write_file`) against `registry`,
/// gated by `permissions`. The model is told it can generate files; writes to
/// not-yet-allowed paths are resolved at runtime by the permission mode.
pub fn register_write_tools(registry: &mut Registry, policy: Arc<Permissions>) {
    let (d, h) = fs::write_file_tool(policy);
    registry.register_with_descriptor(d, h);
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
