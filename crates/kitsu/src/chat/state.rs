//! Live conversation state, persistence, and artifact rotation.

use kitsu_core::ArtifactStoragePolicy;
use kitsu_providers::ChatMessage;
use kitsu_tools::Permissions;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use crate::artifacts;
use crate::checkpoints::report_write_checkpoints;
use crate::cli_error::CliError;
use crate::cli_runtime::mark_jsonl_session_persistence_failed;
use crate::cli_runtime::mark_jsonl_session_resumable;
use crate::cli_runtime::run_id;
use crate::cli_runtime::set_jsonl_session_receipt;
use crate::cli_runtime::SessionSnapshotState;
use crate::command_artifact;
use crate::command_policy::reset_command_session;
use crate::command_policy::PregrantedCommand;
use crate::context_window::ContextWindowProvenance;
use crate::context_window::ContextWindowRefreshConfig;
use crate::presentation::terminal_inline;
use crate::provider_diagnostics::ProviderDiagnosticContext;
use crate::session;
use crate::setup::ArtifactPolicySource;
use crate::subagent;

/// Removes a stateless run's offload namespace (and every session generation
/// beneath it) when the chat ends, so we don't leak temp dirs into `/tmp`.
pub(crate) struct OffloadDirGuard(pub(crate) std::path::PathBuf);
impl Drop for OffloadDirGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Allocates a destination that has never carried artifact authority in this
/// process. Session sidecars remain durable, but every `/new` and `/resume`
/// receives a fresh child so resetting runtime grants cannot be undone by
/// granting the same directory again.
#[derive(Debug, Default)]
pub(crate) struct ArtifactDirectoryAllocator {
    pub(crate) next_generation: u64,
    /// Advisory locks held for the lifetime of this process, one per allocated
    /// generation. Cross-run pruning tests these instead of pids (which are
    /// reused) or marker files (which a killed process never removes); the
    /// kernel releases them on process death, including `SIGKILL`.
    pub(crate) locks: Vec<std::fs::File>,
}

impl ArtifactDirectoryAllocator {
    pub(crate) fn allocate(
        &mut self,
        base: &std::path::Path,
    ) -> Result<std::path::PathBuf, String> {
        std::fs::create_dir_all(base).map_err(|error| {
            format!(
                "cannot create artifact namespace {}: {error}",
                base.display()
            )
        })?;
        let metadata = std::fs::symlink_metadata(base).map_err(|error| {
            format!(
                "cannot inspect artifact namespace {}: {error}",
                base.display()
            )
        })?;
        if metadata.file_type().is_symlink() || !metadata.file_type().is_dir() {
            return Err(format!(
                "artifact namespace '{}' is not a regular directory",
                base.display()
            ));
        }

        for _ in 0..256 {
            let generation = self.next_generation;
            self.next_generation = self
                .next_generation
                .checked_add(1)
                .ok_or_else(|| "artifact generation counter exhausted".to_string())?;
            let directory = base.join(format!("runtime-{}-{generation}", run_id()));
            match std::fs::create_dir(&directory) {
                Ok(()) => {
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt as _;
                        if let Err(error) = std::fs::set_permissions(
                            &directory,
                            std::fs::Permissions::from_mode(0o700),
                        ) {
                            let _ = std::fs::remove_dir(&directory);
                            return Err(format!(
                                "cannot secure artifact directory {}: {error}",
                                directory.display()
                            ));
                        }
                    }
                    if let Some(lock) = artifacts::hold_generation_lock(&directory) {
                        self.locks.push(lock);
                    }
                    return Ok(directory);
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => {
                    return Err(format!(
                        "cannot create artifact directory {}: {error}",
                        directory.display()
                    ));
                }
            }
        }
        Err(format!(
            "cannot allocate a fresh artifact directory under {}",
            base.display()
        ))
    }
}

/// A provenance entry: a static context source, why it's loaded, its estimated
/// token cost, and the governing policy — honest accounting (design §3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CtxSourceKind {
    CoreInstructions,
    ProjectContext,
    SkillContext,
    ToolSchemas,
}

#[derive(Clone)]
pub(crate) struct CtxEntry {
    pub(crate) kind: CtxSourceKind,
    pub(crate) label: String,
    pub(crate) tokens: u64,
    pub(crate) policy: &'static str,
    pub(crate) why: String,
}

impl CtxEntry {
    pub(crate) fn new(
        kind: CtxSourceKind,
        label: impl Into<String>,
        tokens: u64,
        policy: &'static str,
        why: impl Into<String>,
    ) -> Self {
        Self {
            kind,
            label: label.into(),
            tokens,
            policy,
            why: why.into(),
        }
    }
}

/// Stable UI metadata plus context-window provenance for the running chat.
/// `window` is shared with the `AgentConfig` builder via a `Cell` so `/model`
/// updates both request budgeting and `/status`/`/context` immediately.
pub(crate) struct SessionInfo {
    pub(crate) gateway: String,
    pub(crate) provider_diagnostics: ProviderDiagnosticContext,
    pub(crate) directory: std::path::PathBuf,
    pub(crate) config_path: std::path::PathBuf,
    pub(crate) state_dir: std::path::PathBuf,
    pub(crate) endpoint: String,
    pub(crate) auth_status: String,
    pub(crate) artifact_policy: ArtifactStoragePolicy,
    pub(crate) artifact_policy_source: ArtifactPolicySource,
    pub(crate) project_context: RefCell<Option<std::path::PathBuf>>,
    pub(crate) offload_dir: Rc<RefCell<Option<std::path::PathBuf>>>,
    pub(crate) model_can_read_artifacts: bool,
    /// Split combined required-write + structured-output automation into
    /// schema-free tool rounds followed by one schema-constrained final round.
    pub(crate) defer_structured_finalization: bool,
    /// Host-owned exact-output readiness gate paired with the deferred mode.
    pub(crate) structured_finalization_gate: Option<kitsu_core::StructuredFinalizationGate>,
    pub(crate) session_id: Rc<RefCell<Option<String>>>,
    /// Per-turn subagent spawn budget shared with the spawn tools (G3).
    pub(crate) spawn_budget: Arc<kitsu_core::SpawnBudget>,
    /// Run-wide shared tallies (parent + subagents) for receipts.
    pub(crate) run_tallies: Arc<kitsu_core::RunTallies>,
    pub(crate) tool_names: Vec<String>,
    pub(crate) command_roots: Vec<std::path::PathBuf>,
    pub(crate) command_policy: Option<Arc<kitsu_tools::CommandPolicy>>,
    pub(crate) command_artifact_sink: Option<command_artifact::CommandArtifactSink>,
    pub(crate) invocation_command_grants: Vec<kitsu_tools::CommandGrantInfo>,
    /// CLI-layer provenance for the invocation pregrants, kept parallel to
    /// `invocation_command_grants` rather than inside `CommandGrantInfo`: the
    /// cwd origin is a flag fact, and folding it into the policy struct would
    /// perturb the argv+cwd+executable equality that classifies provenance.
    pub(crate) invocation_command_pregrants: Vec<PregrantedCommand>,
    pub(crate) agent_profiles: Arc<subagent::profile::AgentProfileCatalog>,
    pub(crate) permissions: Arc<Permissions>,
    pub(crate) autosave_dirty: Cell<bool>,
    pub(crate) window: Rc<Cell<Option<u64>>>,
    pub(crate) reasoning_effort: Rc<Cell<Option<kitsu_providers::ReasoningEffort>>>,
    pub(crate) window_provenance: Arc<std::sync::Mutex<ContextWindowProvenance>>,
    pub(crate) window_refresh: ContextWindowRefreshConfig,
    pub(crate) max_tokens: Option<u32>,
    pub(crate) provenance: RefCell<Vec<CtxEntry>>,
}

pub(crate) fn context_window_provenance(
    provenance: &Arc<std::sync::Mutex<ContextWindowProvenance>>,
) -> ContextWindowProvenance {
    provenance
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

pub(crate) fn persist_session(
    store: &Option<session::ActiveSessionStore>,
    session: &SessionInfo,
    model: &str,
    messages: &[ChatMessage],
) -> bool {
    let Some(store) = store else {
        return true;
    };
    let project_context = session.project_context.borrow();
    let commit = match store.save(
        &session.directory,
        &session.gateway,
        model,
        project_context.as_deref(),
        messages,
    ) {
        Ok(commit) => {
            // Installation makes this id addressable even if the later
            // private-identity observation reports an automation failure.
            mark_jsonl_session_resumable(store.id());
            commit
        }
        Err(error) => {
            if error.installed_commit().is_some() {
                // The save API distinguishes a pre-install failure from a
                // post-install durability/cleanup failure. In the latter case
                // the exact snapshot is already present and resumable, even
                // though the requested operation must still fail truthfully.
                mark_jsonl_session_resumable(store.id());
            }
            #[cfg(unix)]
            if let Some(commit) = error.installed_commit() {
                if let Err(observation_error) = session
                    .permissions
                    .observe_protected_file(commit.path(), commit.file())
                {
                    eprintln!(
                        "warning: failed session snapshot was installed, but its private file identity could not be recorded: {}",
                        terminal_inline(&observation_error.to_string())
                    );
                }
            }
            mark_jsonl_session_persistence_failed();
            session.autosave_dirty.set(true);
            eprintln!("warning: session was not saved: {error}");
            return false;
        }
    };
    #[cfg(unix)]
    let observation = session
        .permissions
        .observe_protected_file(commit.path(), commit.file());
    #[cfg(not(unix))]
    let observation = session.permissions.observe_protected_path(commit.path());
    if let Err(error) = observation {
        mark_jsonl_session_persistence_failed();
        session.autosave_dirty.set(true);
        eprintln!(
            "warning: session snapshot was saved but its private file identity could not be recorded: {}",
            terminal_inline(&error.to_string())
        );
        false
    } else {
        session.autosave_dirty.set(false);
        true
    }
}

pub(crate) fn persist_session_required(
    store: &Option<session::ActiveSessionStore>,
    session: &SessionInfo,
    model: &str,
    messages: &[ChatMessage],
) -> Result<(), CliError> {
    if persist_session(store, session, model, messages) {
        Ok(())
    } else {
        Err(CliError::user(
            "session autosave failed; refusing to report durable automation success",
        ))
    }
}

pub(crate) fn persist_started_session(
    store: &Option<session::ActiveSessionStore>,
    session: &SessionInfo,
    model: &str,
    messages: &[ChatMessage],
) -> bool {
    if messages
        .iter()
        .all(|message| matches!(message.role, kitsu_providers::Role::System))
    {
        return true;
    }
    persist_session(store, session, model, messages)
}

#[derive(Debug)]
pub(crate) struct ClearSessionReceipt {
    pub(crate) previous_session_id: Option<String>,
    pub(crate) session_id: Option<String>,
    pub(crate) removed_turns: usize,
    pub(crate) removed_messages: usize,
    pub(crate) history_preserved: bool,
    pub(crate) retained_read_roots: usize,
    pub(crate) retained_write_roots: usize,
    pub(crate) artifact_warning: Option<String>,
}

impl ClearSessionReceipt {
    pub(crate) fn snapshot_state(&self) -> &'static str {
        if self.session_id.is_some() {
            "pending_until_first_success"
        } else {
            "disabled"
        }
    }

    pub(crate) fn payload(&self) -> serde_json::Value {
        serde_json::json!({
            "kind": "clear_session_boundary",
            "previous_session_id": self.previous_session_id,
            "session_id": self.session_id,
            "removed_turns": self.removed_turns,
            "removed_messages": self.removed_messages,
            "history_preserved": self.history_preserved,
            "previous_session_resumable": self.history_preserved,
            "new_session_snapshot_state": self.snapshot_state(),
            "runtime_grants_reset": true,
            "invocation_read_roots_retained": self.retained_read_roots,
            "invocation_write_roots_retained": self.retained_write_roots,
            "artifact_warning": self.artifact_warning,
        })
    }

    pub(crate) fn human_summary(&self) -> String {
        match (
            self.previous_session_id.as_deref(),
            self.session_id.as_deref(),
            self.history_preserved,
        ) {
            (Some(previous), Some(current), true) => format!(
                "cleared context; previous session {} remains resumable ({} user turn(s), {} message(s) preserved); started {} (snapshot pending first successful turn; runtime grants reset)",
                terminal_inline(previous),
                self.removed_turns,
                self.removed_messages,
                terminal_inline(current),
            ),
            (_, Some(current), false) => format!(
                "cleared context; no prior conversation needed saving; started {} (snapshot pending first successful turn; runtime grants reset)",
                terminal_inline(current),
            ),
            _ => format!(
                "cleared in-memory context (autosave disabled; {} user turn(s) and {} message(s) were not saved; runtime grants reset)",
                self.removed_turns, self.removed_messages,
            ),
        }
    }
}

/// Make `/clear` a recoverable session boundary. The only fallible durable
/// operations happen before messages, identity mirrors, grants, agents, or
/// artifacts are changed; a save or lease failure therefore leaves the active
/// conversation usable under its original id.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn clear_session_boundary(
    session_store: &mut Option<session::ActiveSessionStore>,
    session: &SessionInfo,
    model: &str,
    messages: &mut Vec<ChatMessage>,
    state: &mut kitsu_core::AgentSessionState,
    registry: &Arc<subagent::AgentRegistry>,
    journal: &kitsu_tools::builtins::fs::WriteJournal,
    artifact_directories: &mut ArtifactDirectoryAllocator,
    artifact_storage_enabled: bool,
    stateless_artifact_base: &std::path::Path,
) -> Result<ClearSessionReceipt, String> {
    let running = registry.running();
    if running > 0 {
        return Err(format!(
            "clear blocked while {running} background agent(s) are running; use /agents and wait for them to finish"
        ));
    }

    let leading_system = messages
        .first()
        .filter(|message| matches!(message.role, kitsu_providers::Role::System))
        .cloned();
    let removed_turns = messages
        .iter()
        .filter(|message| matches!(message.role, kitsu_providers::Role::User))
        .count();
    let removed_messages = messages
        .len()
        .saturating_sub(usize::from(leading_system.is_some()));
    let had_durable_content = messages
        .iter()
        .any(|message| !matches!(message.role, kitsu_providers::Role::System));
    let previous_session_id = session.session_id.borrow().clone();
    let durable = session_store.is_some();

    if !persist_started_session(&*session_store, session, model, messages) {
        return Err("clear cancelled because the current session could not be saved".to_string());
    }
    if let Some(store) = session_store.as_mut() {
        store.reset().map_err(|error| {
            format!("clear cancelled because a fresh autosave lease could not be created: {error}")
        })?;
        *session.session_id.borrow_mut() = Some(store.id().to_string());
    }
    let session_id = session.session_id.borrow().clone();
    set_jsonl_session_receipt(
        session_id.clone(),
        if session_id.is_some() {
            SessionSnapshotState::PendingUntilFirstSuccess
        } else {
            SessionSnapshotState::Disabled
        },
    );

    report_write_checkpoints(journal.drain_and_take_reports().await);
    *messages = leading_system.into_iter().collect();
    *state = kitsu_core::AgentSessionState::default();
    registry.reset();
    journal.reset();
    session.permissions.reset_runtime_grants();
    reset_command_session(
        session.command_policy.as_ref(),
        &session.invocation_command_pregrants,
    );
    let artifact_warning = rotate_artifact_storage(
        &session.permissions,
        &session.offload_dir,
        session.command_artifact_sink.as_ref(),
        || {
            artifact_directory_for_boundary(
                artifact_directories,
                artifact_storage_enabled,
                session_store.as_ref(),
                stateless_artifact_base,
            )
        },
    )
    .err();
    // A fresh durable id is planned but deliberately has no empty snapshot;
    // the next successful turn will publish it.
    session.autosave_dirty.set(session_id.is_some());

    Ok(ClearSessionReceipt {
        previous_session_id,
        session_id,
        removed_turns,
        removed_messages,
        history_preserved: durable && had_durable_content,
        retained_read_roots: session.permissions.base_read_roots().len(),
        retained_write_roots: session.permissions.base_write_roots().len(),
        artifact_warning,
    })
}

pub(crate) fn activate_offload_dir(
    perms: &Permissions,
    state: &Rc<RefCell<Option<std::path::PathBuf>>>,
    desired: Option<std::path::PathBuf>,
) -> Result<(), String> {
    // Drop the previous session's destination before doing any fallible work.
    // Otherwise a failed directory creation could leave direct orchestrator
    // writes pointed at the old session even after its permission was revoked.
    *state.borrow_mut() = None;
    if let Some(dir) = desired.as_ref() {
        std::fs::create_dir_all(dir).map_err(|error| {
            format!(
                "cannot create artifact directory {}: {error}",
                dir.display()
            )
        })?;
        perms
            .protect_write_path(&dir.to_string_lossy())
            .map_err(|error| format!("cannot protect artifact directory: {error}"))?;
        perms
            .grant_trusted_read_root(dir)
            .map_err(|error| error.to_string())?;
    }
    *state.borrow_mut() = desired;
    Ok(())
}

pub(crate) fn artifact_directory_for_boundary(
    allocator: &mut ArtifactDirectoryAllocator,
    enabled: bool,
    store: Option<&session::ActiveSessionStore>,
    stateless_base: &std::path::Path,
) -> Result<Option<std::path::PathBuf>, String> {
    if !enabled {
        return Ok(None);
    }
    let base = match store {
        Some(store) => store.artifacts_dir()?,
        None => stateless_base.to_path_buf(),
    };
    allocator.allocate(&base).map(Some)
}

pub(crate) fn rotate_artifact_storage<F>(
    perms: &Permissions,
    state: &Rc<RefCell<Option<std::path::PathBuf>>>,
    command_sink: Option<&command_artifact::CommandArtifactSink>,
    destination: F,
) -> Result<(), String>
where
    F: FnOnce() -> Result<Option<std::path::PathBuf>, String>,
{
    // Revoke the host writer's old destination before allocating or validating
    // the replacement. The caller resets runtime filesystem grants first; this
    // ordering ensures an allocation failure leaves neither authority stale.
    *state.borrow_mut() = None;
    if let Some(sink) = command_sink {
        sink.set_directory(None);
    }
    let desired = destination()?;
    activate_offload_dir(perms, state, desired)?;
    if let Some(sink) = command_sink {
        sink.set_directory(state.borrow().clone());
    }
    Ok(())
}
