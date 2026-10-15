//! Built-in filesystem and explicitly authorized developer-command tools.

pub mod command;
pub mod fs;

use std::sync::Arc;

use crate::permissions::Permissions;
use crate::{Registry, RegistryError};

/// Register the guarded developer-command and Git tools against their independent
/// command authority. Calling this function is the explicit enablement step;
/// filesystem permissions and auto-approval never register or authorize it.
pub fn register_command_tool(
    registry: &mut Registry,
    policy: Arc<command::CommandPolicy>,
) -> Result<(), RegistryError> {
    let (descriptor, handler) = command::command_exec_tool(policy.clone());
    registry.register_with_descriptor(descriptor, handler)?;
    let (descriptor, handler) = command::git_tool(policy);
    registry.register_with_descriptor(descriptor, handler)
}

/// How much of the read surface to advertise to the model.
///
/// Every tool costs context before the conversation starts: the full read set
/// serializes to ~3.5k tokens of JSON schema, which is more than the entire
/// window some local runtimes actually serve (Ollama defaults to 4096 and
/// silently truncates from the FRONT, destroying the system prompt and tool
/// schemas first). [`ToolTier::Lean`] advertises only the general-purpose
/// primitives — read, list, search — from which the specialised readers'
/// capabilities can be approximated, trading breadth for a prompt that fits.
///
/// The tier is an ADVERTISING decision, not an authority one: permissions are
/// unchanged, and a leaner registry can never reach a path the full one could
/// not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ToolTier {
    /// Every read tool, including the format-specific document readers.
    #[default]
    Full,
    /// Only the primitives that fit a small local context window.
    Lean,
}

impl ToolTier {
    /// The tier appropriate for a model whose resolved context window is
    /// `window` tokens, or [`ToolTier::Full`] when the window is unknown.
    ///
    /// Below [`LEAN_TIER_WINDOW_THRESHOLD`] the full schema set would consume
    /// most of the window (or all of it) before the user's first message, so
    /// the lean set is the only one that leaves room to actually work.
    pub fn for_window(window: Option<u64>) -> Self {
        match window {
            Some(window) if window < LEAN_TIER_WINDOW_THRESHOLD => Self::Lean,
            _ => Self::Full,
        }
    }

    pub fn is_lean(self) -> bool {
        matches!(self, Self::Lean)
    }
}

/// Resolved context windows below this get [`ToolTier::Lean`].
///
/// Sized so the full ~3.5k-token read schema stays a minority of the window:
/// at 16k it is ~22%, which leaves a workable conversation budget, while at
/// 8k it is ~44% and at Ollama's default 4096 it does not fit at all.
pub const LEAN_TIER_WINDOW_THRESHOLD: u64 = 16_384;

/// Register the read fs tools against `registry`, bound to `permissions`.
/// Content search is implemented in-process on Unix, so its availability does
/// not depend on a developer tool being present on the model runner's PATH.
pub fn register_fs_tools(
    registry: &mut Registry,
    policy: Arc<Permissions>,
) -> Result<(), RegistryError> {
    register_fs_tools_tiered(registry, policy, ToolTier::Full)
}

/// [`register_fs_tools`], but advertising only the subset `tier` allows.
///
/// The lean tier keeps `fs_read_file`, `fs_list_dir` and `repo_grep`: the three
/// primitives every other read tool is a specialisation of. A model that needs
/// a CSV can still read it as text; a model with no window left can do nothing
/// at all.
pub fn register_fs_tools_tiered(
    registry: &mut Registry,
    policy: Arc<Permissions>,
    tier: ToolTier,
) -> Result<(), RegistryError> {
    let (d, h) = fs::read_file_tool(policy.clone());
    registry.register_with_descriptor(d, h)?;

    let (d, h) = fs::list_dir_tool(policy.clone());
    registry.register_with_descriptor(d, h)?;

    if tier.is_lean() {
        // Search is the one non-obvious primitive a lean model cannot
        // reconstruct from read+list, so it stays even in the smallest set.
        #[cfg(unix)]
        {
            let (d, h) = fs::grep_tool(policy);
            registry.register_with_descriptor(d, h)?;
        }
        #[cfg(not(unix))]
        if rg_available() {
            let (d, h) = fs::grep_tool(policy);
            registry.register_with_descriptor(d, h)?;
        }
        return Ok(());
    }

    let (d, h) = fs::read_csv_tool(policy.clone());
    registry.register_with_descriptor(d, h)?;

    let (d, h) = fs::read_pdf_tool(policy.clone());
    registry.register_with_descriptor(d, h)?;

    let (d, h) = fs::read_xlsx_tool(policy.clone());
    registry.register_with_descriptor(d, h)?;

    let (d, h) = fs::read_docx_tool(policy.clone());
    registry.register_with_descriptor(d, h)?;

    let (d, h) = fs::read_pptx_tool(policy.clone());
    registry.register_with_descriptor(d, h)?;

    let (d, h) = fs::read_notebook_tool(policy.clone());
    registry.register_with_descriptor(d, h)?;

    let (d, h) = fs::read_html_tool(policy.clone());
    registry.register_with_descriptor(d, h)?;

    let (d, h) = fs::read_image_tool(policy.clone());
    registry.register_with_descriptor(d, h)?;

    let (d, h) = fs::read_document_tool(policy.clone());
    registry.register_with_descriptor(d, h)?;

    let (d, h) = fs::scan_file_tool(policy.clone());
    registry.register_with_descriptor(d, h)?;

    let (d, h) = fs::find_symbol_tool(policy.clone());
    registry.register_with_descriptor(d, h)?;

    let (d, h) = fs::outline_file_tool(policy.clone());
    registry.register_with_descriptor(d, h)?;

    let (d, h) = fs::read_manifest_tool(policy.clone());
    registry.register_with_descriptor(d, h)?;

    let (d, h) = fs::audit_summary_tool(policy.clone());
    registry.register_with_descriptor(d, h)?;

    let (d, h) = fs::scan_secrets_tool(policy.clone());
    registry.register_with_descriptor(d, h)?;

    let (d, h) = fs::diff_config_tool(policy.clone());
    registry.register_with_descriptor(d, h)?;

    #[cfg(unix)]
    {
        let (d, h) = fs::grep_tool(policy);
        registry.register_with_descriptor(d, h)?;
    }

    #[cfg(not(unix))]
    if rg_available() {
        let (d, h) = fs::grep_tool(policy);
        registry.register_with_descriptor(d, h)?;
    }

    Ok(())
}

/// Register the write-capable fs tools (`fs_write_file` and `fs_edit_file`)
/// against `registry`, gated by `permissions`. Writes to not-yet-allowed paths
/// are resolved at runtime by the permission mode.
pub fn register_write_tools(
    registry: &mut Registry,
    policy: Arc<Permissions>,
) -> Result<(), RegistryError> {
    let (d, h) = fs::write_file_tool(policy.clone());
    registry.register_with_descriptor(d, h)?;

    let (d, h) = fs::edit_file_tool(policy);
    registry.register_with_descriptor(d, h)
}

/// Register write tools that record exact pre/post images for conflict-aware
/// per-turn undo. The same journal can be shared by foreground and delegated
/// tool registries.
pub fn register_write_tools_with_journal(
    registry: &mut Registry,
    policy: Arc<Permissions>,
    journal: fs::WriteJournal,
) -> Result<(), RegistryError> {
    let (d, h) = fs::write_file_tool_with_journal(policy.clone(), Some(journal.clone()));
    registry.register_with_descriptor(d, h)?;

    let (d, h) = fs::edit_file_tool_with_journal(policy, Some(journal));
    registry.register_with_descriptor(d, h)
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

    const CODEX_READ_CAPABILITIES: [&str; 10] = [
        "fs_read_image",
        "find_symbol",
        "read_manifest",
        "fs_read_docx",
        "audit_summary",
        "scan_secrets",
        "fs_read_pptx",
        "fs_read_notebook",
        "fs_read_html",
        "outline_file",
    ];

    fn read_tool_names(tier: ToolTier) -> Vec<String> {
        let policy = Arc::new(Permissions::for_roots(&[], &[]).unwrap());
        let mut registry = Registry::new();
        register_fs_tools_tiered(&mut registry, policy, tier).unwrap();
        registry
            .descriptors()
            .iter()
            .map(|d| d.name.clone())
            .collect()
    }

    /// The whole point of the lean tier: the advertised read surface must fit
    /// beside a conversation in a 4096-token window. The full set serializes to
    /// more than that window on its own, so a small local model never sees its
    /// own system prompt.
    #[test]
    fn lean_tier_read_schema_fits_a_small_window() {
        let policy = Arc::new(Permissions::for_roots(&[], &[]).unwrap());
        let mut lean = Registry::new();
        register_fs_tools_tiered(&mut lean, policy.clone(), ToolTier::Lean).unwrap();
        let mut full = Registry::new();
        register_fs_tools_tiered(&mut full, policy, ToolTier::Full).unwrap();

        // ~4 chars/token, matching the estimator the context budget uses.
        let tokens = |r: &Registry| serde_json::to_string(r.descriptors()).unwrap().len() / 4;
        assert!(
            tokens(&lean) < 1_000,
            "lean read schema must leave room to work in a 4096-token window, got ~{} tokens",
            tokens(&lean)
        );
        assert!(
            tokens(&full) > tokens(&lean) * 2,
            "full tier should be materially larger; lean ~{}, full ~{}",
            tokens(&lean),
            tokens(&full)
        );
    }

    /// Lean keeps read, list, and search — the primitives every dropped tool is
    /// a specialisation of. Losing search would leave a model unable to locate
    /// anything it was not handed a path to.
    #[test]
    fn lean_tier_keeps_the_general_purpose_primitives() {
        let lean = read_tool_names(ToolTier::Lean);
        assert!(lean.iter().any(|n| n == "fs_read_file"));
        assert!(lean.iter().any(|n| n == "fs_list_dir"));
        #[cfg(unix)]
        assert!(lean.iter().any(|n| n == "repo_grep"), "got {lean:?}");
    }

    /// The format-specific readers are what makes the full set too big; none of
    /// them may survive into the lean tier.
    #[test]
    fn lean_tier_drops_the_specialised_readers() {
        let lean = read_tool_names(ToolTier::Lean);
        for dropped in CODEX_READ_CAPABILITIES {
            assert!(
                !lean.iter().any(|n| n == dropped),
                "lean tier must not advertise {dropped}"
            );
        }
    }

    /// No regression for models that can afford the full set: an explicit Full
    /// tier, and the untiered entry point, register exactly what they always did.
    #[test]
    fn full_tier_is_unchanged_and_is_the_untiered_default() {
        let full = read_tool_names(ToolTier::Full);
        for kept in CODEX_READ_CAPABILITIES {
            assert!(full.iter().any(|n| n == kept), "full tier lost {kept}");
        }

        let policy = Arc::new(Permissions::for_roots(&[], &[]).unwrap());
        let mut untiered = Registry::new();
        register_fs_tools(&mut untiered, policy).unwrap();
        let untiered: Vec<String> = untiered
            .descriptors()
            .iter()
            .map(|d| d.name.clone())
            .collect();
        assert_eq!(untiered, full, "register_fs_tools must default to Full");
    }

    /// The tier is chosen from the resolved window. Ollama's default 4096 and
    /// the shipped profile's optimistic 8192 both land on Lean; a hosted window
    /// stays Full. An unknown window is NOT treated as small — `None` means "no
    /// information", and silently shrinking a large model's toolset would be a
    /// regression for every gateway that reports nothing.
    #[test]
    fn tier_is_selected_from_the_resolved_window() {
        assert_eq!(ToolTier::for_window(Some(4_096)), ToolTier::Lean);
        assert_eq!(ToolTier::for_window(Some(8_192)), ToolTier::Lean);
        assert_eq!(
            ToolTier::for_window(Some(LEAN_TIER_WINDOW_THRESHOLD - 1)),
            ToolTier::Lean
        );
        assert_eq!(
            ToolTier::for_window(Some(LEAN_TIER_WINDOW_THRESHOLD)),
            ToolTier::Full,
            "the threshold itself is affordable"
        );
        assert_eq!(ToolTier::for_window(Some(131_072)), ToolTier::Full);
        assert_eq!(ToolTier::for_window(None), ToolTier::Full);
    }

    /// A leaner registry is an advertising decision, never an authority one: it
    /// must not gain a handler the full tier lacks.
    #[test]
    fn lean_tier_never_adds_capability() {
        let lean = read_tool_names(ToolTier::Lean);
        let full = read_tool_names(ToolTier::Full);
        for name in &lean {
            assert!(
                full.contains(name),
                "{name} is advertised by the lean tier but not the full one"
            );
        }
    }

    #[test]
    fn write_registration_advertises_write_and_edit_tools() {
        let policy = Arc::new(Permissions::for_roots(&[], &[]).unwrap());
        let mut registry = Registry::new();

        register_write_tools(&mut registry, policy).unwrap();

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
        register_fs_tools(&mut registry, policy).unwrap();
        assert!(registry.is_registered("fs_read_csv"));
        assert!(registry.is_registered("fs_read_pdf"));
        assert!(registry.is_registered("read_document"));
        assert!(registry.is_registered("fs_read_xlsx"));
        assert!(registry.is_registered("fs_read_docx"));
        assert!(registry.is_registered("fs_read_pptx"));
        assert!(registry.is_registered("fs_read_notebook"));
        assert!(registry.is_registered("fs_read_html"));
        assert!(registry.is_registered("fs_read_image"));
        assert!(registry.is_registered("fs_scan_file"));
        assert!(registry.is_registered("find_symbol"));
        assert!(registry.is_registered("outline_file"));
        assert!(registry.is_registered("read_manifest"));
        assert!(registry.is_registered("audit_summary"));
        assert!(registry.is_registered("scan_secrets"));
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

    /// Rebase guard: every capability added by the Codex UX branch must retain
    /// exactly one advertised handler and a valid path-based object schema.
    /// A conflict that keeps a descriptor but drops its handler (or vice versa)
    /// would otherwise compile while leaving the model-facing registry broken.
    #[test]
    fn codex_read_capabilities_have_unique_path_schemas() {
        let policy = Arc::new(Permissions::for_roots(&[], &[]).unwrap());
        let mut registry = Registry::new();
        register_fs_tools(&mut registry, policy).unwrap();

        let mut all_names = std::collections::HashSet::new();
        for descriptor in registry.descriptors() {
            assert!(
                all_names.insert(descriptor.name.as_str()),
                "duplicate advertised tool descriptor: {}",
                descriptor.name
            );
        }

        for name in CODEX_READ_CAPABILITIES {
            assert!(registry.is_registered(name), "missing handler for {name}");
            let descriptor = registry
                .descriptors()
                .iter()
                .find(|descriptor| descriptor.name == name)
                .unwrap_or_else(|| panic!("missing descriptor for {name}"));
            assert_eq!(descriptor.parameters["type"], "object", "{name}");
            assert_eq!(
                descriptor.parameters["properties"]["path"]["type"], "string",
                "{name} must expose a string path"
            );
            assert!(
                descriptor.parameters["required"]
                    .as_array()
                    .is_some_and(|required| required.iter().any(|field| field == "path")),
                "{name} must require path"
            );
        }
    }

    /// All Codex-added filesystem capabilities share the same read authority.
    /// Verify the registry integration cannot accidentally bypass that boundary
    /// for one format or analysis helper during conflict resolution.
    #[tokio::test]
    async fn codex_read_capabilities_fail_closed_without_read_authority() {
        let policy = Arc::new(Permissions::for_roots(&[], &[]).unwrap());
        let mut registry = Registry::new();
        register_fs_tools(&mut registry, policy).unwrap();
        let denied_path = "/kitsu-rebase-guard/not-authorized";

        for name in CODEX_READ_CAPABILITIES {
            let args = if name == "find_symbol" {
                serde_json::json!({"path": denied_path, "symbol": "needle"})
            } else {
                serde_json::json!({"path": denied_path})
            };
            let error = match registry.run(name, args.to_string()).await {
                Err(error) => error,
                Ok(output) => panic!("{name} bypassed read policy: {output}"),
            };
            assert!(
                matches!(error, crate::ToolError::PolicyDenied(_)),
                "{name} returned the wrong denial: {error}"
            );
        }
    }

    #[test]
    fn read_registration_keeps_common_and_specialized_descriptors_unique() {
        let policy = Arc::new(Permissions::for_roots(&[], &[]).unwrap());
        let mut registry = Registry::new();
        register_fs_tools(&mut registry, policy).unwrap();

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

        register_fs_tools(&mut registry, filesystem).unwrap();

        assert!(!registry.is_registered("command_exec"));
    }

    #[cfg(unix)]
    #[test]
    fn filesystem_registration_always_advertises_bounded_content_search() {
        let filesystem = Arc::new(Permissions::for_roots(&[], &[]).unwrap());
        let mut registry = Registry::new();

        register_fs_tools(&mut registry, filesystem).unwrap();

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

        register_command_tool(&mut registry, command_policy).unwrap();

        assert!(registry.is_registered("command_exec"));
        assert!(registry.is_registered("git"));
        assert!(registry
            .descriptors()
            .iter()
            .any(|descriptor| descriptor.name == "command_exec"));
    }

    #[test]
    fn complete_builtin_registry_has_exact_descriptor_handler_parity() {
        let root = tempfile::tempdir().unwrap();
        let filesystem = Arc::new(Permissions::for_roots(&[], &[]).unwrap());
        let command_policy = Arc::new(
            command::CommandPolicy::new(
                &[root.path().to_string_lossy().into_owned()],
                command::CommandMode::Deny,
            )
            .unwrap(),
        );
        let mut registry = Registry::new();

        register_fs_tools(&mut registry, filesystem.clone()).unwrap();
        register_write_tools(&mut registry, filesystem).unwrap();
        register_command_tool(&mut registry, command_policy).unwrap();

        let descriptor_names = registry
            .descriptors()
            .iter()
            .map(|descriptor| descriptor.name.as_str())
            .collect::<Vec<_>>();
        let unique_names = descriptor_names
            .iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>();

        assert_eq!(descriptor_names.len(), unique_names.len());
        assert_eq!(registry.handlers.len(), descriptor_names.len());
        for name in descriptor_names {
            assert!(registry.is_registered(name), "missing handler for {name}");
        }
    }
}
