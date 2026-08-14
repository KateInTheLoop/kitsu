//! Built-in filesystem and explicitly authorized developer-command tools.

pub mod command;
pub mod fs;

use std::sync::Arc;

use crate::permissions::Permissions;
use crate::Registry;

/// Register the guarded developer-command tool against its independent
/// command authority. Calling this function is the explicit enablement step;
/// filesystem permissions and auto-approval never register or authorize it.
pub fn register_command_tool(registry: &mut Registry, policy: Arc<command::CommandPolicy>) {
    let (descriptor, handler) = command::command_exec_tool(policy);
    registry.register_with_descriptor(descriptor, handler);
}

/// Register the read fs tools against `registry`, bound to `permissions`.
/// `repo_grep` is registered only when `rg` is on PATH; otherwise it is simply
/// not advertised (design Gap 7).
pub fn register_fs_tools(registry: &mut Registry, policy: Arc<Permissions>) {
    let (d, h) = fs::read_file_tool(policy.clone());
    registry.register_with_descriptor(d, h);

    let (d, h) = fs::list_dir_tool(policy.clone());
    registry.register_with_descriptor(d, h);

    let (d, h) = fs::read_csv_tool(policy.clone());
    registry.register_with_descriptor(d, h);

    if rg_available() {
        let (d, h) = fs::grep_tool(policy);
        registry.register_with_descriptor(d, h);
    }
}

/// Register the write-capable fs tools (`fs_write_file` and `fs_edit_file`)
/// against `registry`, gated by `permissions`. Writes to not-yet-allowed paths
/// are resolved at runtime by the permission mode.
pub fn register_write_tools(registry: &mut Registry, policy: Arc<Permissions>) {
    let (d, h) = fs::write_file_tool(policy.clone());
    registry.register_with_descriptor(d, h);

    let (d, h) = fs::edit_file_tool(policy);
    registry.register_with_descriptor(d, h);
}

/// Register write tools that record exact pre/post images for conflict-aware
/// per-turn undo. The same journal can be shared by foreground and delegated
/// tool registries.
pub fn register_write_tools_with_journal(
    registry: &mut Registry,
    policy: Arc<Permissions>,
    journal: fs::WriteJournal,
) {
    let (d, h) = fs::write_file_tool_with_journal(policy.clone(), Some(journal.clone()));
    registry.register_with_descriptor(d, h);

    let (d, h) = fs::edit_file_tool_with_journal(policy, Some(journal));
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_registration_advertises_write_and_edit_tools() {
        let policy = Arc::new(Permissions::for_roots(&[], &[]).unwrap());
        let mut registry = Registry::new();

        register_write_tools(&mut registry, policy);

        assert!(registry.is_registered("fs_write_file"));
        assert!(registry.is_registered("fs_edit_file"));
        assert!(registry
            .descriptors()
            .iter()
            .any(|descriptor| descriptor.name == "fs_edit_file"));
    }

    #[test]
    fn fs_registration_advertises_csv_reader() {
        let policy = Arc::new(Permissions::for_roots(&[], &[]).unwrap());
        let mut registry = Registry::new();
        register_fs_tools(&mut registry, policy);
        assert!(registry.is_registered("fs_read_csv"));
        assert!(registry
            .descriptors()
            .iter()
            .any(|d| d.name == "fs_read_csv"));
    }

    #[test]
    fn filesystem_auto_allow_never_enables_command_execution() {
        let filesystem =
            Arc::new(Permissions::new(&[], &[], crate::permissions::Mode::AutoAllow).unwrap());
        let mut registry = Registry::new();

        register_fs_tools(&mut registry, filesystem);

        assert!(!registry.is_registered("command_exec"));
    }

    #[test]
    fn command_registration_is_an_explicit_separate_step() {
        let root = tempfile::tempdir().unwrap();
        let command_policy = Arc::new(
            command::CommandPolicy::new(
                &[root.path().to_string_lossy().into_owned()],
                command::CommandMode::Deny,
            )
            .unwrap(),
        );
        let mut registry = Registry::new();

        register_command_tool(&mut registry, command_policy);

        assert!(registry.is_registered("command_exec"));
        assert!(registry
            .descriptors()
            .iter()
            .any(|descriptor| descriptor.name == "command_exec"));
    }
}
