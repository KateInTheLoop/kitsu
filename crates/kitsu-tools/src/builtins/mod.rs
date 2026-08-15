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
/// Content search is implemented in-process on Unix, so its availability does
/// not depend on a developer tool being present on the model runner's PATH.
pub fn register_fs_tools(registry: &mut Registry, policy: Arc<Permissions>) {
    let (d, h) = fs::read_file_tool(policy.clone());
    registry.register_with_descriptor(d, h);

    let (d, h) = fs::list_dir_tool(policy.clone());
    registry.register_with_descriptor(d, h);

    let (d, h) = fs::read_csv_tool(policy.clone());
    registry.register_with_descriptor(d, h);

    let (d, h) = fs::read_pdf_tool(policy.clone());
    registry.register_with_descriptor(d, h);

    let (d, h) = fs::read_xlsx_tool(policy.clone());
    registry.register_with_descriptor(d, h);

    let (d, h) = fs::read_docx_tool(policy.clone());
    registry.register_with_descriptor(d, h);

    let (d, h) = fs::read_image_tool(policy.clone());
    registry.register_with_descriptor(d, h);

    let (d, h) = fs::read_document_tool(policy.clone());
    registry.register_with_descriptor(d, h);

    let (d, h) = fs::scan_file_tool(policy.clone());
    registry.register_with_descriptor(d, h);

    let (d, h) = fs::find_symbol_tool(policy.clone());
    registry.register_with_descriptor(d, h);

    let (d, h) = fs::read_manifest_tool(policy.clone());
    registry.register_with_descriptor(d, h);

    let (d, h) = fs::audit_summary_tool(policy.clone());
    registry.register_with_descriptor(d, h);

    let (d, h) = fs::diff_config_tool(policy.clone());
    registry.register_with_descriptor(d, h);

    #[cfg(unix)]
    {
        let (d, h) = fs::grep_tool(policy);
        registry.register_with_descriptor(d, h);
    }

    #[cfg(not(unix))]
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

/// Non-Unix builds retain the fixed-argv ripgrep backend and advertise it only
/// when the dependency is actually usable. Unix builds never execute this
/// discovery path because their search is native and always registered.
#[cfg(not(unix))]
fn rg_available() -> bool {
    std::process::Command::new("rg")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
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
        assert!(registry.is_registered("fs_read_pdf"));
        assert!(registry.is_registered("read_document"));
        assert!(registry.is_registered("fs_read_xlsx"));
        assert!(registry.is_registered("fs_read_docx"));
        assert!(registry.is_registered("fs_read_image"));
        assert!(registry.is_registered("fs_scan_file"));
        assert!(registry.is_registered("find_symbol"));
        assert!(registry.is_registered("read_manifest"));
        assert!(registry.is_registered("audit_summary"));
        assert!(registry.is_registered("fs_diff_config"));
        assert!(registry
            .descriptors()
            .iter()
            .any(|d| d.name == "fs_read_csv"));
        assert!(registry
            .descriptors()
            .iter()
            .any(|d| d.name == "fs_read_pdf"));
        assert!(registry
            .descriptors()
            .iter()
            .any(|d| d.name == "read_document"));
    }

    #[test]
    fn read_registration_keeps_common_and_specialized_descriptors_unique() {
        let policy = Arc::new(Permissions::for_roots(&[], &[]).unwrap());
        let mut registry = Registry::new();
        register_fs_tools(&mut registry, policy);

        let names = registry
            .descriptors()
            .iter()
            .map(|descriptor| descriptor.name.as_str())
            .collect::<Vec<_>>();
        let unique = names
            .iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            names.len(),
            unique.len(),
            "duplicate descriptors: {names:?}"
        );

        for name in [
            "fs_read_file",
            "fs_list_dir",
            "fs_read_csv",
            "fs_read_pdf",
            "fs_read_xlsx",
            "read_document",
            "fs_scan_file",
            "fs_diff_config",
        ] {
            assert!(unique.contains(name), "missing common read tool {name}");
            assert!(registry.is_registered(name), "missing handler for {name}");
        }
        #[cfg(unix)]
        assert!(unique.contains("repo_grep"));
        assert!(!unique.contains("fs_write_file"));
        assert!(!unique.contains("fs_edit_file"));
        assert!(!unique.contains("command_exec"));
    }

    #[test]
    fn filesystem_auto_allow_never_enables_command_execution() {
        let filesystem =
            Arc::new(Permissions::new(&[], &[], crate::permissions::Mode::AutoAllow).unwrap());
        let mut registry = Registry::new();

        register_fs_tools(&mut registry, filesystem);

        assert!(!registry.is_registered("command_exec"));
    }

    #[cfg(unix)]
    #[test]
    fn filesystem_registration_always_advertises_bounded_content_search() {
        let filesystem = Arc::new(Permissions::for_roots(&[], &[]).unwrap());
        let mut registry = Registry::new();

        register_fs_tools(&mut registry, filesystem);

        assert!(registry.is_registered("repo_grep"));
        let descriptor = registry
            .descriptors()
            .iter()
            .find(|descriptor| descriptor.name == "repo_grep")
            .unwrap();
        assert_eq!(
            descriptor.parameters["properties"]["max_count"]["maximum"],
            1_000
        );
        assert_eq!(
            descriptor.parameters["properties"]["pattern"]["maxLength"],
            4_096
        );
        assert_eq!(
            descriptor.parameters["properties"]["literal"]["type"],
            "boolean"
        );
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
