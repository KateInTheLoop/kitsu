//! CLI projections of session authority, context, and usage.

use crate::cli::Cli;
use kitsu_core::context::{
    clamp_window_for_shortfall, ContextBudget, ContextMeter, DEFAULT_CONTEXT_WINDOW,
};
use kitsu_core::ArtifactStoragePolicy;
use kitsu_providers::{ChatMessage, ProviderRouteReceipt};
use kitsu_tools::{Mode, Permissions};
use std::sync::Arc;

use crate::chat::state::context_window_provenance;
use crate::chat::state::CtxSourceKind;
use crate::chat::state::SessionInfo;
use crate::cli_runtime::run_spec;
use crate::command_policy::command_mode_label;
use crate::command_policy::CwdSource;
use crate::context_window::ContextWindowSource;
use crate::presentation::shorten_home;
use crate::presentation::terminal_inline;
use crate::review::WorkspaceEvidenceAuthority;
use crate::session_telemetry_render;
use crate::setup::artifact_policy_receipt;
use crate::setup::reasoning_effort_label;
use crate::subagent;
use crate::terminal::completion::current_terminal_columns;
#[cfg(unix)]
use crate::tui_bridge;

pub(crate) fn permission_mode_label(mode: Mode) -> (&'static str, &'static str) {
    match mode {
        Mode::Ask => ("ask", "prompt for unlisted paths"),
        Mode::Scoped => (
            "scoped",
            "non-prompting; reject paths outside current roots",
        ),
        Mode::AutoAllow => (
            "DANGER: global filesystem bypass",
            "allow every non-protected path without prompting",
        ),
        Mode::Deny => ("deny", "reject unlisted paths"),
    }
}

pub(crate) fn minimal_root_count(mut roots: Vec<std::path::PathBuf>) -> u64 {
    roots.sort();
    roots.dedup();
    let mut minimal = Vec::<std::path::PathBuf>::new();
    for root in roots {
        if minimal.iter().any(|parent| root.starts_with(parent)) {
            continue;
        }
        minimal.push(root);
    }
    u64::try_from(minimal.len()).unwrap_or(u64::MAX)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn current_session_authority_input(
    mode: Mode,
    read_roots: Vec<std::path::PathBuf>,
    write_roots: Vec<std::path::PathBuf>,
    protected_read_roots: Vec<std::path::PathBuf>,
    protected_write_roots: Vec<std::path::PathBuf>,
    exact_command_grants: u64,
    commands_enabled: bool,
) -> kitsu_core::EffectiveAuthorityInput {
    let protected_roots = protected_read_roots
        .into_iter()
        .chain(protected_write_roots)
        .collect();
    kitsu_core::EffectiveAuthorityInput {
        mode: match mode {
            Mode::Ask => kitsu_core::EffectiveAuthorityMode::Ask,
            Mode::Scoped => kitsu_core::EffectiveAuthorityMode::Scoped,
            Mode::AutoAllow => kitsu_core::EffectiveAuthorityMode::AutoAllow,
            Mode::Deny => kitsu_core::EffectiveAuthorityMode::Deny,
        },
        read_roots: minimal_root_count(read_roots),
        write_roots: minimal_root_count(write_roots),
        protected_roots: minimal_root_count(protected_roots),
        exact_command_grants,
        commands_enabled,
    }
}

pub(crate) fn current_session_context_input(
    reported_window: Option<u64>,
    source: ContextWindowSource,
    max_tokens: Option<u32>,
    used_tokens: u64,
    provider_anchored: bool,
    window_evidence: Option<kitsu_core::ContextWindowEvidence>,
) -> kitsu_core::ContextSnapshotInput {
    let resolved_window = reported_window.unwrap_or(DEFAULT_CONTEXT_WINDOW);
    let conservative_default = matches!(&source, ContextWindowSource::ConservativeDefault);
    let telemetry_source = match source {
        ContextWindowSource::Cli => kitsu_core::ContextWindowSource::CommandLine,
        ContextWindowSource::Profile => kitsu_core::ContextWindowSource::GatewayProfile,
        ContextWindowSource::DiskCache => kitsu_core::ContextWindowSource::DiskCache,
        ContextWindowSource::LiveDiscovery => {
            kitsu_core::ContextWindowSource::LiveProviderDiscovery
        }
        ContextWindowSource::ServedWindow => kitsu_core::ContextWindowSource::RuntimeServedWindow,
        ContextWindowSource::ConservativeDefault => {
            kitsu_core::ContextWindowSource::ConservativeDefault
        }
    };
    let window_provenance = kitsu_core::TelemetryProvenance::ContextWindow(telemetry_source);
    let window_tokens = if conservative_default {
        kitsu_core::TelemetryValue::estimated(resolved_window, window_provenance)
    } else {
        kitsu_core::TelemetryValue::authoritative(resolved_window, window_provenance)
    };
    let budgeted_window = window_evidence.map_or(resolved_window, |evidence| {
        if evidence.resolved_window == resolved_window {
            clamp_window_for_shortfall(
                resolved_window,
                evidence.reported_prompt_tokens,
                evidence.shortfall_count,
            )
        } else {
            resolved_window
        }
    });
    let effective_tokens = ContextBudget::new(budgeted_window, max_tokens).effective;
    kitsu_core::ContextSnapshotInput {
        window_tokens,
        effective_tokens: kitsu_core::TelemetryValue::derived(
            effective_tokens,
            kitsu_core::TelemetryProvenance::ContextBudgetArithmetic,
        ),
        used_tokens: kitsu_core::TelemetryValue::estimated(
            used_tokens,
            if provider_anchored {
                kitsu_core::TelemetryProvenance::ProviderAnchoredContextMeter
            } else {
                kitsu_core::TelemetryProvenance::ContextMeterEstimate
            },
        ),
    }
}

pub(crate) fn current_session_snapshot_from_observation(
    provider_route: ProviderRouteReceipt,
    session_id: Option<String>,
    run_id: kitsu_core::RunId,
    authority: kitsu_core::EffectiveAuthorityInput,
    context: kitsu_core::ContextSnapshotInput,
    usage: kitsu_core::RunUsageSnapshot,
) -> kitsu_core::CurrentSessionSnapshot {
    kitsu_core::CurrentSessionSnapshot::from_input(kitsu_core::CurrentSessionSnapshotInput {
        provider_route,
        session_id: session_id.and_then(|session_id| kitsu_core::SessionId::new(session_id).ok()),
        run_id: Some(run_id),
        authority,
        context,
        usage: kitsu_core::SessionUsageInput::from_run_usage(
            &usage,
            kitsu_core::SessionCostInput::unavailable(),
        ),
    })
}

/// Capture the existing current-process owners into one typed DTO without an
/// await point or a provider, account, filesystem, or persistence operation.
pub(crate) fn collect_current_session_snapshot(
    provider_route: ProviderRouteReceipt,
    messages: &[ChatMessage],
    session: &SessionInfo,
    perms: &Permissions,
    state: &kitsu_core::AgentSessionState,
) -> kitsu_core::CurrentSessionSnapshot {
    let session_id = session.session_id.borrow().clone();
    let run_id = run_spec().run_id().clone();

    // Take each authority collection once, then reduce those immutable local
    // observations. Protected read/write paths are one authority surface.
    let mode = perms.mode();
    let read_roots = perms.read_roots();
    let write_roots = perms.write_roots();
    let protected_read_roots = perms.protected_read_roots();
    let protected_write_roots = perms.protected_write_roots();
    let command_grants = session
        .command_policy
        .as_ref()
        .map(|policy| policy.session_grants())
        .unwrap_or_default();
    let commands_enabled = session
        .tool_names
        .iter()
        .any(|tool_name| matches!(tool_name.as_str(), "command_exec" | "git"));
    let authority = current_session_authority_input(
        mode,
        read_roots,
        write_roots,
        protected_read_roots,
        protected_write_roots,
        u64::try_from(command_grants.len()).unwrap_or(u64::MAX),
        commands_enabled,
    );

    let reported_window = session.window.get();
    let source = context_window_provenance(&session.window_provenance).source;
    let breakdown = ContextMeter::new().breakdown(messages);
    let provenance_tokens = session
        .provenance
        .borrow()
        .iter()
        .fold(0_u64, |total, entry| total.saturating_add(entry.tokens));
    let estimated = provenance_tokens
        .saturating_add(breakdown.user)
        .saturating_add(breakdown.assistant)
        .saturating_add(breakdown.tool_results);
    let provider_anchored = state.meter.anchored(messages);
    let used_tokens = if provider_anchored {
        state.meter.estimate(messages)
    } else {
        estimated
    };
    let context = current_session_context_input(
        reported_window,
        source,
        session.max_tokens,
        used_tokens,
        provider_anchored,
        state.window_evidence,
    );
    let usage = session.run_tallies.usage_snapshot();

    current_session_snapshot_from_observation(
        provider_route,
        session_id,
        run_id,
        authority,
        context,
        usage,
    )
}

/// Capture the coarse current-request contributors already retained in memory.
/// Static chunks are folded by their closed source kind, never by parsing
/// display labels. Embedded attachment and on-demand skill-body text is left
/// inside the user-message estimate and remains separately unavailable.
pub(crate) fn collect_context_sources_snapshot(
    messages: &[ChatMessage],
    session: &SessionInfo,
    state: &kitsu_core::AgentSessionState,
) -> kitsu_core::ContextSourcesSnapshot {
    let mut core_instructions_tokens = 0_u64;
    let mut project_context_tokens = 0_u64;
    let mut skill_context_tokens = 0_u64;
    let mut tool_schema_tokens = 0_u64;
    for entry in session.provenance.borrow().iter() {
        let destination = match entry.kind {
            CtxSourceKind::CoreInstructions => &mut core_instructions_tokens,
            CtxSourceKind::ProjectContext => &mut project_context_tokens,
            CtxSourceKind::SkillContext => &mut skill_context_tokens,
            CtxSourceKind::ToolSchemas => &mut tool_schema_tokens,
        };
        *destination = destination.saturating_add(entry.tokens);
    }

    let meter = ContextMeter::new();
    let breakdown = meter.breakdown(messages);
    let initial_system_tokens = messages
        .first()
        .filter(|message| matches!(message.role, kitsu_providers::Role::System))
        .map(|message| meter.breakdown(std::slice::from_ref(message)).system)
        .unwrap_or(0);

    let resolved_window = session.window.get().unwrap_or(DEFAULT_CONTEXT_WINDOW);
    let budgeted_window = state.window_evidence.map_or(resolved_window, |evidence| {
        if evidence.resolved_window == resolved_window {
            clamp_window_for_shortfall(
                resolved_window,
                evidence.reported_prompt_tokens,
                evidence.shortfall_count,
            )
        } else {
            resolved_window
        }
    });
    let budget = ContextBudget::new(budgeted_window, session.max_tokens);
    let ledger = &state.compaction_ledger;
    kitsu_core::ContextSourcesSnapshot::from_input(kitsu_core::ContextSourcesSnapshotInput {
        core_instructions_tokens,
        project_context_tokens,
        skill_context_tokens,
        tool_schema_tokens,
        runtime_system_message_tokens: breakdown.system.saturating_sub(initial_system_tokens),
        user_message_tokens: breakdown.user,
        assistant_message_tokens: breakdown.assistant,
        tool_result_tokens: breakdown.tool_results,
        prompt_budget_tokens: budget.effective,
        output_reserve_tokens: budget.reserve,
        trim_target_tokens: budget.target,
        estimate_buffer_tokens: budget.effective.saturating_sub(budget.target),
        compactions: ledger.total_compactions,
        exchanges_removed: ledger.exchanges_removed,
        tool_results_elided: ledger.tool_results_elided,
        compaction_tokens_saved: ledger.tokens_saved,
    })
}

pub(crate) fn current_session_deck_options(
    cli: &Cli,
) -> session_telemetry_render::TelemetryDeckOptions {
    let destination = if cli.screen_reader {
        session_telemetry_render::TelemetryDeckDestination::ScreenReader
    } else if cli.plain {
        session_telemetry_render::TelemetryDeckDestination::Plain
    } else if kitsu_render::plain_mode() {
        session_telemetry_render::TelemetryDeckDestination::NoColor
    } else if kitsu_render::stderr_styled() {
        session_telemetry_render::TelemetryDeckDestination::StyledTerminal
    } else {
        session_telemetry_render::TelemetryDeckDestination::Redirected
    };
    session_telemetry_render::TelemetryDeckOptions {
        destination,
        columns: current_terminal_columns().unwrap_or(80),
    }
}

pub(crate) fn redirected_current_session_deck_options(
) -> session_telemetry_render::TelemetryDeckOptions {
    session_telemetry_render::TelemetryDeckOptions {
        destination: session_telemetry_render::TelemetryDeckDestination::Redirected,
        columns: 80,
    }
}

pub(crate) fn effective_authority_mode_label(
    mode: kitsu_core::EffectiveAuthorityMode,
) -> (&'static str, &'static str) {
    match mode {
        kitsu_core::EffectiveAuthorityMode::Ask => ("ask", "prompt for unlisted paths"),
        kitsu_core::EffectiveAuthorityMode::Scoped => (
            "scoped",
            "non-prompting; reject paths outside current roots",
        ),
        kitsu_core::EffectiveAuthorityMode::AutoAllow => (
            "DANGER: global filesystem bypass",
            "allow every non-protected path without prompting",
        ),
        kitsu_core::EffectiveAuthorityMode::Deny => ("deny", "reject unlisted paths"),
    }
}

pub(crate) fn snapshot_context_window_source_label(
    provenance: kitsu_core::TelemetryProvenance,
) -> &'static str {
    let kitsu_core::TelemetryProvenance::ContextWindow(source) = provenance else {
        unreachable!("current status window has context-window provenance")
    };
    match source {
        kitsu_core::ContextWindowSource::CommandLine => "CLI --context-window",
        kitsu_core::ContextWindowSource::GatewayProfile => "gateway profile context_window",
        kitsu_core::ContextWindowSource::DiskCache => "disk cache",
        kitsu_core::ContextWindowSource::LiveProviderDiscovery => "live model discovery",
        kitsu_core::ContextWindowSource::RuntimeServedWindow => "runtime-served window (clamped)",
        kitsu_core::ContextWindowSource::ConservativeDefault => "conservative default",
    }
}

pub(crate) fn session_status_payload(
    messages: &[ChatMessage],
    session: &SessionInfo,
    registry: &subagent::AgentRegistry,
    state: &kitsu_core::AgentSessionState,
    provider_route: &ProviderRouteReceipt,
    observation: &session_telemetry_render::CurrentStatusObservation,
    text: String,
) -> serde_json::Value {
    let snapshot = observation.snapshot();
    let route = &snapshot.status.route;
    let authority = &snapshot.status.authority;
    let context = &snapshot.context;
    let authority_mode = *authority
        .mode
        .value()
        .expect("current status authority mode is observed");
    let (permission_mode, permission_behavior) = effective_authority_mode_label(authority_mode);
    let window = context
        .window_tokens
        .value()
        .copied()
        .expect("current status context window is observed");
    let effective = context
        .effective_tokens
        .value()
        .copied()
        .expect("current status effective context is observed");
    let used = context
        .used_tokens
        .value()
        .copied()
        .expect("current status used context is observed");
    let remaining = context
        .available_tokens
        .value()
        .copied()
        .expect("current status available context is derived");
    let version = observation
        .version()
        .value()
        .expect("current status version is observed");
    let cwd = observation
        .cwd()
        .value()
        .expect("current status cwd is observed");
    let model = route
        .model
        .value()
        .expect("the active chat route has a model");
    let read_roots = authority
        .read_roots
        .value()
        .copied()
        .expect("current status read-root count is observed");
    let write_roots = authority
        .write_roots
        .value()
        .copied()
        .expect("current status write-root count is observed");
    let commands_enabled = authority
        .commands_enabled
        .value()
        .copied()
        .expect("current status command authority is observed");
    let exact_command_grants = authority
        .exact_command_grants
        .value()
        .copied()
        .expect("current status command-grant count is observed");
    let user_turns = messages
        .iter()
        .filter(|message| matches!(message.role, kitsu_providers::Role::User))
        .count();
    let assistant_turns = messages
        .iter()
        .filter(|message| matches!(message.role, kitsu_providers::Role::Assistant))
        .count();
    let tool_results = messages
        .iter()
        .filter(|message| matches!(message.role, kitsu_providers::Role::Tool))
        .count();
    let provenance = context_window_provenance(&session.window_provenance);
    let progress = registry.progress_snapshot();
    let project_context = session
        .project_context
        .borrow()
        .as_deref()
        .map(|path| path.to_string_lossy().into_owned());
    let autosave_enabled = snapshot.status.session_id.value().is_some();

    serde_json::json!({
        "kind": "status",
        "text": text,
        "version": version,
        "provider_route": provider_route,
        "session": {
            "id": snapshot.status.session_id.value(),
            "gateway": session.gateway,
            "model": model,
            "reasoning_effort": reasoning_effort_label(session.reasoning_effort.get()),
            "cwd": cwd,
            "config_path": session.config_path,
            "state_dir": session.state_dir,
            "endpoint": session.endpoint,
            "auth_status": session.auth_status,
            "project_context": project_context,
            "autosave_enabled": autosave_enabled,
            "autosave_dirty": session.autosave_dirty.get(),
        },
        "context": {
            "configured_window": session.window.get(),
            "window": window,
            "effective": effective,
            "used": used,
            "remaining": remaining,
            "source": snapshot_context_window_source_label(context.window_tokens.provenance()),
            "cache": provenance.cache.label(),
            "discovery": provenance.discovery.label(),
        },
        "conversation": {
            "user_turns": user_turns,
            "assistant_turns": assistant_turns,
            "tool_results": tool_results,
            "messages": messages.len(),
        },
        "compaction": {
            "runs": state.compaction_ledger.total_compactions,
            "tokens_saved": state.compaction_ledger.tokens_saved,
            "exchanges_removed": state.compaction_ledger.exchanges_removed,
            "tool_results_elided": state.compaction_ledger.tool_results_elided,
        },
        "tools": {
            "count": session.tool_names.len(),
            "names": session.tool_names,
        },
        "permissions": {
            "mode": permission_mode,
            "behavior": permission_behavior,
            "read_roots": read_roots,
            "write_roots": write_roots,
        },
        "commands": {
            "enabled": commands_enabled,
            "mode": command_mode_label(session.command_policy.as_ref()),
            "cwd_roots": session.command_roots.len(),
            "exact_grants": exact_command_grants,
        },
        "artifact_policy": artifact_policy_receipt(
            session.artifact_policy,
            session.artifact_policy_source,
            session.model_can_read_artifacts,
        ),
        "agents": {
            "total": progress.total,
            "running": progress.running,
            "done": progress.done,
            "failed": progress.failed,
            "cancelled": progress.cancelled,
        },
    })
}

pub(crate) fn command_resolution_label(
    resolution: kitsu_tools::CommandExecutableResolution,
) -> &'static str {
    match resolution {
        kitsu_tools::CommandExecutableResolution::AmbientPath => "ambient PATH lookup",
        kitsu_tools::CommandExecutableResolution::InvocationPath => "invocation path",
    }
}

pub(crate) fn format_command_grant_identity(grant: &kitsu_tools::CommandGrantInfo) -> String {
    let Some(executable) = grant.executable.as_ref() else {
        return "executable identity unavailable (legacy grant; resolution, observed path, and fingerprint unknown)"
            .to_string();
    };
    let identity = &executable.identity;
    let observed = identity
        .observed_path
        .as_deref()
        .map(terminal_inline)
        .unwrap_or_else(|| "unresolved".to_string());
    let fingerprint = executable
        .fingerprint
        .as_ref()
        .map(|fingerprint| {
            format!(
                "bound to {} (revalidated before execution)",
                shorten_home(&fingerprint.canonical_path)
            )
        })
        .unwrap_or_else(|| "unresolved (not executable under this grant)".to_string());
    format!(
        "executable {} | resolution {} | observed {} | fingerprint {} | immutable handle {}",
        terminal_inline(&identity.invocation),
        command_resolution_label(identity.resolution),
        observed,
        fingerprint,
        if identity.pinned { "yes" } else { "no" },
    )
}

pub(crate) fn format_permissions(perms: &Permissions, session: &SessionInfo) -> String {
    let (mode, behavior) = permission_mode_label(perms.mode());
    let base_reads = perms.base_read_roots();
    let base_writes = perms.base_write_roots();
    let invocation_reads = perms.active_base_read_roots();
    let invocation_writes = perms.active_base_write_roots();
    let runtime_reads = perms.runtime_read_roots();
    let runtime_writes = perms.runtime_write_roots();
    let mut out = format!("permissions:\n  mode         {mode} ({behavior})\n  read roots");
    if invocation_reads.is_empty() && runtime_reads.is_empty() {
        out.push_str("   none\n");
    } else {
        out.push('\n');
        for root in &invocation_reads {
            out.push_str(&format!(
                "    - {} (invocation/base{})\n",
                shorten_home(root),
                if root == &session.directory {
                    ", launch-directory read"
                } else {
                    ""
                }
            ));
        }
        for root in &runtime_reads {
            let managed_artifact = session
                .offload_dir
                .borrow()
                .as_deref()
                .is_some_and(|path| path == root);
            out.push_str(&format!(
                "    - {} (session/runtime{})\n",
                shorten_home(root),
                if managed_artifact {
                    ", Kitsu-managed artifacts"
                } else {
                    ""
                }
            ));
        }
    }
    out.push_str("  write roots");
    if invocation_writes.is_empty() && runtime_writes.is_empty() {
        out.push_str("  none");
    } else {
        out.push('\n');
        for root in &invocation_writes {
            out.push_str(&format!("    - {} (invocation/base)\n", shorten_home(root)));
        }
        for root in &runtime_writes {
            out.push_str(&format!("    - {} (session/runtime)\n", shorten_home(root)));
        }
        out.pop();
    }
    let revoked_reads: Vec<_> = base_reads
        .iter()
        .filter(|root| !invocation_reads.contains(root))
        .collect();
    let revoked_writes: Vec<_> = base_writes
        .iter()
        .filter(|root| !invocation_writes.contains(root))
        .collect();
    out.push_str("\n  revoked invocation roots");
    if revoked_reads.is_empty() && revoked_writes.is_empty() {
        out.push_str(" none");
    } else {
        for root in revoked_reads {
            out.push_str(&format!("\n    - {} (read)", shorten_home(root)));
        }
        for root in revoked_writes {
            out.push_str(&format!("\n    - {} (write)", shorten_home(root)));
        }
        out.push_str("\n    reset/new session restores invocation roots");
    }
    let protected_reads = perms.protected_read_roots();
    out.push_str("\n  protected reads");
    if protected_reads.is_empty() {
        out.push_str(" none");
    } else {
        for root in protected_reads {
            out.push_str(&format!("\n    - {} (Kitsu-private)", shorten_home(&root)));
        }
    }
    let internal_reads = perms.internal_read_roots();
    out.push_str("\n  internal reads");
    if internal_reads.is_empty() {
        out.push_str(" none");
    } else {
        for root in internal_reads {
            out.push_str(&format!("\n    - {} (Kitsu-managed)", shorten_home(&root)));
        }
    }
    out.push_str("\n  host evidence");
    let has_workspace_diff_tool = session
        .tool_names
        .iter()
        .any(|name| name == "workspace_diff");
    match WorkspaceEvidenceAuthority::for_session(session, perms) {
        Ok(authority) => {
            out.push_str(&format!(
                "\n    - {} (git worktree; /diff, /review, kitsu review{})",
                shorten_home(&authority.root),
                if has_workspace_diff_tool {
                    ", workspace_diff tool"
                } else {
                    ""
                }
            ));
            out.push_str(
                "\n      host-only: read by Kitsu's own git, never a model read root, never a model filesystem target",
            );
            for path in &authority.private {
                out.push_str(&format!(
                    "\n      excludes {} (Kitsu-private)",
                    shorten_home(path)
                ));
            }
            out.push_str(
                "\n      write checkpoints are replayed from Kitsu's own verified snapshots, including changes written outside this root",
            );
            out.push_str("\n      /review discloses one bounded diff per command; /diff never leaves this terminal");
            if has_workspace_diff_tool {
                out.push_str(
                    "\n      workspace_diff discloses one bounded diff per model call, announced each time; disable with --no-workspace-evidence",
                );
            }
        }
        Err(_) => {
            out.push_str(&format!(
                "\n    none ({})",
                WorkspaceEvidenceAuthority::unavailable_reason(session, perms)
            ));
        }
    }
    let protected_writes = perms.protected_write_roots();
    out.push_str("\n  protected writes");
    if protected_writes.is_empty() {
        out.push_str(" none");
    } else {
        for root in protected_writes {
            out.push_str(&format!("\n    - {}", shorten_home(&root)));
        }
    }
    out.push_str("\n  commands     ");
    if let Some(mode) = command_mode_label(session.command_policy.as_ref()) {
        out.push_str(&format!(
            "{mode}; separate exact argv/cwd approval; filesystem flags have no effect"
        ));
        out.push_str(
            "\n    exact approval is followed by mandatory OS containment; cwd roots are writable mounts, runtime roots are read-only, and network/process namespaces are isolated",
        );
        for root in &session.command_roots {
            out.push_str(&format!("\n    - {}", shorten_home(root)));
        }
        let grants = session
            .command_policy
            .as_ref()
            .map(|policy| policy.session_grants())
            .unwrap_or_default();
        out.push_str("\n    exact session grants");
        if grants.is_empty() {
            out.push_str(" none");
        } else {
            for (index, grant) in grants.iter().enumerate() {
                let provenance = if session.invocation_command_grants.contains(grant) {
                    "invocation/base"
                } else {
                    "session/runtime"
                };
                let origin = pregrant_cwd_source(session, grant)
                    .map(|source| format!(", {}", source.label()))
                    .unwrap_or_default();
                out.push_str(&format!(
                    "\n      [{}] {} (cwd {}; {provenance}{origin})",
                    index + 1,
                    terminal_inline(&grant.display_argv()),
                    shorten_home(&grant.cwd),
                ));
                out.push_str(&format!(
                    "\n          {}",
                    format_command_grant_identity(grant)
                ));
            }
            out.push_str("\n      revoke with /revoke-command <number>");
        }
        let revoked_invocation = session
            .invocation_command_grants
            .iter()
            .filter(|grant| !grants.contains(grant))
            .collect::<Vec<_>>();
        if !revoked_invocation.is_empty() {
            out.push_str("\n    revoked invocation command grants");
            for grant in revoked_invocation {
                let origin = pregrant_cwd_source(session, grant)
                    .map(|source| format!("{}; ", source.label()))
                    .unwrap_or_default();
                out.push_str(&format!(
                    "\n      - {} (cwd {}; {origin}reset/new session restores it)",
                    terminal_inline(&grant.display_argv()),
                    shorten_home(&grant.cwd),
                ));
                out.push_str(&format!(
                    "\n          {}",
                    format_command_grant_identity(grant)
                ));
            }
        }
        if let Some(policy) = session.command_policy.as_ref() {
            match policy.containment_status() {
                kitsu_tools::CommandContainmentStatus::Ready(ready) => {
                    out.push_str(&format!(
                        "\n    OS containment ready: {:?} at {}; network/PID/IPC/UTS isolated; fixed environment{}",
                        ready.backend,
                        shorten_home(&ready.backend_path),
                        if ready.passthrough_environment.is_empty() {
                            String::new()
                        } else {
                            format!(
                                " + --command-env pass-through: {}",
                                ready.passthrough_environment.join(", ")
                            )
                        },
                    ));
                    for root in ready.writable_command_roots {
                        out.push_str(&format!("\n      rw {}", shorten_home(&root)));
                    }
                    for root in ready.readonly_runtime_roots {
                        out.push_str(&format!("\n      ro {}", shorten_home(&root)));
                    }
                }
                kitsu_tools::CommandContainmentStatus::Unavailable(error) => {
                    out.push_str(&format!(
                        "\n    OS containment unavailable ({:?}): {}",
                        error.reason,
                        terminal_inline(&error.detail),
                    ));
                }
            }
        }
    } else {
        out.push_str("disabled (enable explicitly with --enable-commands)");
    }
    out.push_str("\n  artifacts    ");
    match session.artifact_policy {
        ArtifactStoragePolicy::Automatic => out.push_str(&format!(
            "automatic ({})",
            session.artifact_policy_source.as_str()
        )),
        ArtifactStoragePolicy::InlineOnly => out.push_str(&format!(
            "disabled ({}, bounded inline only; no new automatic writes; existing artifacts unchanged)",
            session.artifact_policy_source.as_str()
        )),
    }
    out
}

pub(crate) fn json_paths(paths: impl IntoIterator<Item = std::path::PathBuf>) -> Vec<String> {
    paths
        .into_iter()
        .map(|path| path.to_string_lossy().into_owned())
        .collect()
}

/// CLI-layer cwd provenance for one active grant, or `None` when the grant was
/// created at runtime through the approval prompt rather than by a flag.
pub(crate) fn pregrant_cwd_source(
    session: &SessionInfo,
    grant: &kitsu_tools::CommandGrantInfo,
) -> Option<CwdSource> {
    session
        .invocation_command_pregrants
        .iter()
        .find(|pregrant| pregrant.argv == grant.argv && pregrant.cwd == grant.cwd)
        .map(|pregrant| pregrant.cwd_source)
}

pub(crate) fn command_grant_payload(
    grant: &kitsu_tools::CommandGrantInfo,
    provenance: &'static str,
    cwd_source: Option<CwdSource>,
) -> serde_json::Value {
    let fingerprint_status = match grant.executable.as_ref() {
        Some(executable) if executable.fingerprint.is_some() => "bound_and_revalidated",
        Some(_) => "unresolved",
        None => "legacy_unknown",
    };
    serde_json::json!({
        "argv": grant.argv,
        "cwd": grant.cwd,
        "provenance": provenance,
        "cwd_source": cwd_source.map(CwdSource::payload_value),
        "executable": grant.executable,
        "fingerprint_status": fingerprint_status,
    })
}

pub(crate) fn permissions_payload(
    perms: &Permissions,
    session: &SessionInfo,
    text: String,
) -> serde_json::Value {
    let (mode, behavior) = permission_mode_label(perms.mode());
    let base_reads = perms.base_read_roots();
    let base_writes = perms.base_write_roots();
    let invocation_reads = perms.active_base_read_roots();
    let invocation_writes = perms.active_base_write_roots();
    let runtime_reads = perms.runtime_read_roots();
    let runtime_writes = perms.runtime_write_roots();
    let revoked_reads = base_reads
        .iter()
        .filter(|root| !invocation_reads.contains(root))
        .cloned()
        .collect::<Vec<_>>();
    let revoked_writes = base_writes
        .iter()
        .filter(|root| !invocation_writes.contains(root))
        .cloned()
        .collect::<Vec<_>>();
    let active_grants = session
        .command_policy
        .as_ref()
        .map(|policy| policy.session_grants())
        .unwrap_or_default();
    let command_grants = active_grants
        .iter()
        .map(|grant| {
            command_grant_payload(
                grant,
                if session.invocation_command_grants.contains(grant) {
                    "invocation/base"
                } else {
                    "session/runtime"
                },
                pregrant_cwd_source(session, grant),
            )
        })
        .collect::<Vec<_>>();
    let revoked_command_grants = session
        .invocation_command_grants
        .iter()
        .filter(|grant| !active_grants.contains(grant))
        .map(|grant| {
            command_grant_payload(
                grant,
                "revoked_invocation",
                pregrant_cwd_source(session, grant),
            )
        })
        .collect::<Vec<_>>();
    let containment = session
        .command_policy
        .as_ref()
        .map(|policy| policy.containment_status());
    let has_workspace_diff_tool = session
        .tool_names
        .iter()
        .any(|name| name == "workspace_diff");
    let host_evidence = match WorkspaceEvidenceAuthority::for_session(session, perms) {
        Ok(authority) => {
            let mut consumers = vec!["diff", "review", "checkpoint"];
            if has_workspace_diff_tool {
                consumers.push("workspace_diff");
            }
            serde_json::json!({
                "repository_root": authority.root,
                "consumers": consumers,
                "model_read_root": false,
                "prompt_disclosure": if has_workspace_diff_tool {
                    "user_command_or_workspace_diff_tool"
                } else {
                    "explicit_user_command_only"
                },
                "excluded_private_paths": json_paths(authority.private),
            })
        }
        Err(_) => serde_json::json!({
            "repository_root": serde_json::Value::Null,
            "consumers": ["checkpoint"],
            "model_read_root": false,
            "prompt_disclosure": "explicit_user_command_only",
            "unavailable": WorkspaceEvidenceAuthority::unavailable_reason(session, perms),
        }),
    };

    serde_json::json!({
        "kind": "permissions",
        "text": text,
        "mode": mode,
        "behavior": behavior,
        "dangerous_global_bypass": perms.mode() == Mode::AutoAllow,
        "filesystem": {
            "read": {
                "active_invocation": json_paths(invocation_reads),
                "active_runtime": json_paths(runtime_reads),
                "revoked_invocation": json_paths(revoked_reads),
            },
            "write": {
                "active_invocation": json_paths(invocation_writes),
                "active_runtime": json_paths(runtime_writes),
                "revoked_invocation": json_paths(revoked_writes),
            },
            "protected_read_roots": json_paths(perms.protected_read_roots()),
            "internal_read_roots": json_paths(perms.internal_read_roots()),
            "protected_write_roots": json_paths(perms.protected_write_roots()),
        },
        "host_evidence": host_evidence,
        "commands": {
            "enabled": session.command_policy.is_some(),
            "mode": command_mode_label(session.command_policy.as_ref()),
            "cwd_roots": json_paths(session.command_roots.clone()),
            "exact_grants": command_grants,
            "revoked_invocation_grants": revoked_command_grants,
            "ambient_authority_notice": session.command_policy.as_ref().map(|policy| policy.ambient_authority().notice()),
            "containment": containment,
        },
        "artifact_policy": artifact_policy_receipt(
            session.artifact_policy,
            session.artifact_policy_source,
            session.model_can_read_artifacts,
        ),
    })
}

/// Render `/context`: the static provenance rows (what loaded and why) plus the
/// live conversation, with token cost and policy per source. The per-turn
/// `kitsu-context:` line carries the usage-anchored number; this is an estimate.
/// The ONE context-used figure both `/context` and `/cost` report: the
/// meter's provider-anchored estimate when available, else the sum of the
/// provenance rows (system sources + tool schemas) and the live
/// conversation breakdown. Cycle-23 audit: the two commands disagreed by
/// exactly the tool-schema size because /cost read the bare meter.
pub(crate) fn session_context_used(
    messages: &[ChatMessage],
    session: &SessionInfo,
    state: &kitsu_core::AgentSessionState,
) -> (u64, u64) {
    let b = ContextMeter::new().breakdown(messages);
    let prov_total: u64 = session.provenance.borrow().iter().map(|e| e.tokens).sum();
    let estimated = prov_total + b.user + b.assistant + b.tool_results;
    let used = if state.meter.anchored(messages) {
        state.meter.estimate(messages)
    } else {
        estimated
    };
    (used, estimated)
}

/// Push the model and the context meter's numbers onto the inline status
/// row's live fields (requirement **R5**).
///
/// It reads the *same* `session_context_used` and `ContextBudget::effective`
/// that `/context` renders, so the row's meter and the command can never
/// disagree -- the cycle-23 audit's lesson, applied before a second reader
/// exists rather than after.
///
/// Called at both turn boundaries rather than per paint: the numbers only
/// change when the history does, and the paint runs on another thread that
/// cannot borrow the history at all.
#[cfg(unix)]
pub(crate) fn refresh_status_usage(
    source: Option<&Arc<tui_bridge::StatusSource>>,
    model: &str,
    messages: &[ChatMessage],
    session: &SessionInfo,
    state: &kitsu_core::AgentSessionState,
) {
    let Some(source) = source else {
        return;
    };
    let (used, _) = session_context_used(messages, session, state);
    let window = session.window.get().unwrap_or(DEFAULT_CONTEXT_WINDOW);
    let effective = ContextBudget::new(window, session.max_tokens).effective;
    source.set_usage(model, used, effective);
}

pub(crate) fn render_context_command(
    model: &str,
    messages: &[ChatMessage],
    session: &SessionInfo,
    state: &kitsu_core::AgentSessionState,
) {
    let b = ContextMeter::new().breakdown(messages);
    let ctx_window = session.window.get();
    let window = ctx_window.unwrap_or(DEFAULT_CONTEXT_WINDOW);
    let budget = ContextBudget::new(window, session.max_tokens);
    let note = if ctx_window.is_some() {
        ""
    } else {
        "default - set context_window or --context-window"
    };
    // One accounting truth, computed before the provenance borrow below
    // (session_context_used takes its own borrow).
    let (used, estimated) = session_context_used(messages, session, state);
    // Static provenance (system prompt sources + tool schemas) then the live
    // conversation. breakdown.system is already covered by the provenance rows.
    let provenance = session.provenance.borrow();
    let rows: Vec<(&str, u64, &str, &str)> = provenance
        .iter()
        .map(|e| (e.label.as_str(), e.tokens, e.policy, e.why.as_str()))
        .chain([
            ("user turns", b.user, "admitted", "your messages"),
            ("assistant turns", b.assistant, "admitted", "model replies"),
            (
                "tool results",
                b.tool_results,
                "admitted",
                if session.artifact_policy == ArtifactStoragePolicy::InlineOnly {
                    "tool outputs (oversized results bounded inline; no automatic artifacts)"
                } else {
                    "tool outputs (offloaded -> pointers)"
                },
            ),
        ])
        .collect();
    kitsu_render::render_context(model, window, note, budget.effective, used, &rows);
    if used != estimated {
        // Rows are chars/4 estimates; the total is anchored to the
        // provider's reported prompt tokens. Say so instead of letting the
        // arithmetic silently disagree (cycle-23 audit).
        let (sign, diff) = if used >= estimated {
            ("+", used - estimated)
        } else {
            ("-", estimated - used)
        };
        eprintln!(
            "rows sum to {estimated} (chars/4 estimates); used total is provider-anchored ({sign}{diff} adjustment)"
        );
    }
    let window_provenance = context_window_provenance(&session.window_provenance);
    eprintln!(
        "context window source: {}",
        window_provenance.source.label()
    );
    eprintln!("context window cache: {}", window_provenance.cache.label());
    eprintln!(
        "context window discovery: {}",
        window_provenance.discovery.label()
    );
    if let Some(evidence) = state.window_evidence {
        eprintln!(
            "context window runtime check: provider accepted {} of ~{} estimated prompt tokens; server window likely smaller than the resolved {}",
            evidence.reported_prompt_tokens,
            evidence.estimated_sent_tokens,
            evidence.resolved_window
        );
        // P6: once 2+ shortfalls have been observed for this SAME resolved
        // window, the session's effective budget is clamped toward the
        // reported size for the rest of the session. Surface it here as
        // provenance, never silently: the resolved window above is unchanged.
        let budgeted = clamp_window_for_shortfall(
            evidence.resolved_window,
            evidence.reported_prompt_tokens,
            evidence.shortfall_count,
        );
        if budgeted < evidence.resolved_window {
            eprintln!(
                "context window budget clamp: {} shortfall report(s) for this resolved window; session budget now clamped to {} tokens (resolved window {} unchanged)",
                evidence.shortfall_count, budgeted, evidence.resolved_window
            );
        }
    }
    let ledger = &state.compaction_ledger;
    if ledger.total_compactions > 0 {
        eprintln!(
            "compaction: {} total ({} manual, {} automatic; {} summarized, {} trimmed); {} token(s) saved, {} exchange(s) removed, {} tool result(s) elided",
            ledger.total_compactions,
            ledger.manual_compactions,
            ledger.automatic_compactions,
            ledger.summaries,
            ledger.trims,
            ledger.tokens_saved,
            ledger.exchanges_removed,
            ledger.tool_results_elided
        );
        if let Some(last) = ledger.last() {
            eprintln!(
                "last compaction: {:?} via {:?}, {} -> {} tokens{}",
                last.reason,
                last.outcome,
                last.tokens_before,
                last.tokens_after,
                last.fallback_reason
                    .map(|reason| format!(" (fallback: {reason:?})"))
                    .unwrap_or_default()
            );
        }
    } else {
        eprintln!("compaction: none yet; run /compact to compact eligible older exchanges");
    }
}

pub(crate) fn context_command_payload(
    model: &str,
    messages: &[ChatMessage],
    session: &SessionInfo,
    state: &kitsu_core::AgentSessionState,
) -> serde_json::Value {
    let breakdown = ContextMeter::new().breakdown(messages);
    let reported_window = session.window.get();
    let window = reported_window.unwrap_or(DEFAULT_CONTEXT_WINDOW);
    // P6: once 2+ shortfalls have been observed for this SAME resolved window,
    // the reported effective budget reflects the clamp — the window above
    // (and any config) is never rewritten, only this derived number.
    let budgeted_window = state.window_evidence.map_or(window, |evidence| {
        if evidence.resolved_window == window {
            clamp_window_for_shortfall(
                window,
                evidence.reported_prompt_tokens,
                evidence.shortfall_count,
            )
        } else {
            window
        }
    });
    let budget = ContextBudget::new(budgeted_window, session.max_tokens);
    let provenance = session.provenance.borrow();
    let provenance_tokens: u64 = provenance.iter().map(|entry| entry.tokens).sum();
    let estimated = provenance_tokens
        .saturating_add(breakdown.user)
        .saturating_add(breakdown.assistant)
        .saturating_add(breakdown.tool_results);
    let used = if state.meter.anchored(messages) {
        state.meter.estimate(messages)
    } else {
        estimated
    };
    let window_provenance = context_window_provenance(&session.window_provenance);
    let ledger = &state.compaction_ledger;
    let last_compaction = ledger.last().map(|last| {
        serde_json::json!({
            "reason": format!("{:?}", last.reason),
            "outcome": format!("{:?}", last.outcome),
            "tokens_before": last.tokens_before,
            "tokens_after": last.tokens_after,
            "fallback_reason": last.fallback_reason.map(|reason| format!("{reason:?}")),
        })
    });
    serde_json::json!({
        "kind": "context",
        "model": model,
        "window": window,
        "window_reported": reported_window.is_some(),
        "effective_budget": budget.effective,
        "used": used,
        "remaining": budget.effective.saturating_sub(used),
        "source": window_provenance.source.label(),
        "cache": window_provenance.cache.label(),
        "discovery": window_provenance.discovery.label(),
        "runtime_evidence": state.window_evidence.map(|evidence| serde_json::json!({
            "reported_prompt_tokens": evidence.reported_prompt_tokens,
            "estimated_sent_tokens": evidence.estimated_sent_tokens,
            "resolved_window": evidence.resolved_window,
            "shortfall_count": evidence.shortfall_count,
            "budget_clamped": budgeted_window < window,
        })),
        "roles": {
            "user": breakdown.user,
            "assistant": breakdown.assistant,
            "tool_results": breakdown.tool_results,
        },
        "sources": provenance.iter().map(|entry| serde_json::json!({
            "label": entry.label,
            "tokens": entry.tokens,
            "policy": entry.policy,
            "why": entry.why,
        })).collect::<Vec<_>>(),
        "compaction": {
            "total": ledger.total_compactions,
            "manual": ledger.manual_compactions,
            "automatic": ledger.automatic_compactions,
            "summaries": ledger.summaries,
            "trims": ledger.trims,
            "tokens_saved": ledger.tokens_saved,
            "exchanges_removed": ledger.exchanges_removed,
            "tool_results_elided": ledger.tool_results_elided,
            "last": last_compaction,
        },
    })
}
