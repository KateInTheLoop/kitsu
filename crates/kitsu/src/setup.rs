//! CLI composition of provider routes, tools, and initial configuration.

use crate::cli::Cli;
use crate::config::{Config, ConfigError, Gateway, GatewayAdapter};
use kitsu_core::context::DEFAULT_CONTEXT_WINDOW;
use kitsu_core::{AgentConfig, ArtifactStoragePolicy, CompactionMode};
use kitsu_providers::anthropic_messages::{
    AnthropicMessagesRouteBuildError, AnthropicMessagesRouteConfig,
};
use kitsu_providers::auth::{AuthProvider, EnvBearerAuth, NoAuth, StaticAuth};
use kitsu_providers::compiled::{
    build_compiled_catalog_route, build_compiled_provider_route, CompiledProvider,
    CompiledProviderRouteBuildError, CompiledProviderRouteConfig,
};
use kitsu_providers::openai_compat::{
    OpenAICompatCatalogRouteConfig, OpenAICompatRouteBuildError, OpenAICompatRouteConfig,
};
use kitsu_providers::{
    ChatMessage, JsonSchemaFormat, ProviderAuthSource, ProviderCatalogHandle,
    ProviderCatalogPolicy, ProviderError, ProviderProfileId, ProviderRouteContractError,
    ProviderRouteCurrent, ProviderRouteDeclaration, ProviderRouteHandle, ProviderRoutePolicy,
};
use kitsu_tools::{builtins, Permissions, Registry, RegistryError};
use std::sync::Arc;
use std::time::Duration;

use crate::agent_hook;
use crate::cli_error::CliError;
use crate::cli_runtime::active_output_schema;
use crate::context_window::context_window_cache_path;
use crate::context_window::inspect_cached_context_window;
use crate::context_window::redact_endpoint;
use crate::context_window::refresh_cached_context_window;
use crate::context_window::ContextWindowCacheState;
use crate::context_window::ContextWindowDiscoveryState;
use crate::context_window::ContextWindowProvenance;
use crate::context_window::ContextWindowRefreshConfig;
use crate::context_window::ContextWindowRefreshOutcome;
use crate::context_window::ContextWindowSource;
use crate::management::managed_credential;
use crate::management::safe_config_error;
use crate::presentation::shorten_home;
use crate::presentation::terminal_inline;
use crate::provider_diagnostics::base_url_has_loopback_host;
use crate::provider_diagnostics::expired_auth_warning;
use crate::provider_diagnostics::map_provider_error;
use crate::provider_diagnostics::map_provider_error_with_diagnostics;
use crate::provider_diagnostics::safe_auth_refresh_hint;
use crate::provider_diagnostics::ProviderDiagnosticContext;
use crate::provider_diagnostics::ProviderOperation;
use crate::runtime_state::runtime_state_dir;
use crate::skills;

pub(crate) fn route_build_cli_error(
    error: CompiledProviderRouteBuildError,
    operation: ProviderOperation<'_>,
    diagnostics: ProviderDiagnosticContext,
) -> CliError {
    match error {
        CompiledProviderRouteBuildError::CompatibleChat(OpenAICompatRouteBuildError::Auth(
            error,
        ))
        | CompiledProviderRouteBuildError::AnthropicMessages(
            AnthropicMessagesRouteBuildError::Auth(error),
        ) => map_provider_error_with_diagnostics(
            ProviderError::Auth(error),
            operation,
            None,
            diagnostics,
        ),
        CompiledProviderRouteBuildError::Contract(error)
        | CompiledProviderRouteBuildError::CompatibleChat(OpenAICompatRouteBuildError::Contract(
            error,
        ))
        | CompiledProviderRouteBuildError::AnthropicMessages(
            AnthropicMessagesRouteBuildError::Contract(error),
        ) => CliError::user(format!("config error: invalid provider route: {error}")),
        error => CliError::user(format!("config error: {error}")),
    }
}

pub(crate) fn compiled_provider_route_config(
    gateway: &Gateway,
    declaration: ProviderRouteDeclaration,
    model: String,
    auth_source: ProviderAuthSource,
    policy: ProviderRoutePolicy,
) -> CompiledProviderRouteConfig {
    match gateway.adapter {
        GatewayAdapter::CompatibleChat => {
            CompiledProviderRouteConfig::CompatibleChat(OpenAICompatRouteConfig {
                declaration,
                model,
                auth_source,
                base_url: gateway.base_url.clone(),
                chat_path: gateway.chat_path.clone(),
                models_path: gateway.models_path.clone().filter(|path| !path.is_empty()),
                policy,
            })
        }
        GatewayAdapter::AnthropicMessages => {
            CompiledProviderRouteConfig::AnthropicMessages(AnthropicMessagesRouteConfig {
                declaration,
                model,
                auth_source,
                base_url: gateway.base_url.clone(),
                messages_path: gateway.chat_path.clone(),
                policy,
            })
        }
    }
}

pub(crate) fn build_inference_management_route(
    config_path: &std::path::Path,
    gateway_name: &str,
    gateway: &Gateway,
    model: String,
    auth_source: ProviderAuthSource,
    auth: &dyn AuthProvider,
    timeout: Duration,
) -> Result<ProviderRouteHandle<CompiledProvider>, CompiledProviderRouteBuildError> {
    let models_path = gateway.models_path.clone().filter(|path| !path.is_empty());
    let catalog_policy = if models_path.is_some() {
        ProviderCatalogPolicy::Configured
    } else {
        ProviderCatalogPolicy::Unavailable
    };
    let declaration = current_provider_route_declaration(config_path, gateway_name, gateway)?;
    let policy = ProviderRoutePolicy::new(0, 0, timeout, timeout, timeout, catalog_policy)?;
    let route_config =
        compiled_provider_route_config(gateway, declaration, model, auth_source, policy);
    let snapshot = build_compiled_provider_route(route_config, auth)?;
    ProviderRouteCurrent::new(snapshot)
        .bind()
        .map_err(Into::into)
}

pub(crate) fn build_catalog_management_route(
    config_path: &std::path::Path,
    gateway_name: &str,
    gateway: &Gateway,
    models_path: String,
    auth_source: ProviderAuthSource,
    auth: &dyn AuthProvider,
    timeout: Duration,
) -> Result<ProviderCatalogHandle<CompiledProvider>, CompiledProviderRouteBuildError> {
    if gateway.adapter == GatewayAdapter::AnthropicMessages {
        return Err(CompiledProviderRouteBuildError::AnthropicMessagesCatalogUnsupported);
    }
    let declaration = current_provider_catalog_route_declaration(config_path, gateway_name)?;
    let policy = ProviderRoutePolicy::new(
        0,
        0,
        timeout,
        timeout,
        timeout,
        ProviderCatalogPolicy::Configured,
    )?;
    let snapshot = build_compiled_catalog_route(
        OpenAICompatCatalogRouteConfig {
            declaration,
            auth_source,
            base_url: gateway.base_url.clone(),
            chat_path: gateway.chat_path.clone(),
            models_path,
            policy,
        },
        auth,
    )?;
    ProviderRouteCurrent::new(snapshot)
        .bind_catalog()
        .map_err(Into::into)
}

/// Provider + model + auth resolved from config and CLI flags. Shared by the
/// `ask` and `chat` commands so they construct the gateway connection identically.
pub(crate) struct ChatSetup {
    /// Atomic owner of the current immutable generation. `ask`/`chat` bind a
    /// request-facing handle from this owner; raw inference/catalog runtimes do
    /// not leave composition.
    pub(crate) provider_route: ProviderRouteCurrent<CompiledProvider>,
    pub(crate) gateway_name: String,
    pub(crate) provider_diagnostics: ProviderDiagnosticContext,
    pub(crate) config_path: std::path::PathBuf,
    pub(crate) state_dir: std::path::PathBuf,
    pub(crate) endpoint: String,
    pub(crate) auth_status: String,
    /// Config-level opt-out for the model-callable `workspace_diff` evidence
    /// tool (docs/verified-review.md). Defaults to enabled.
    pub(crate) workspace_evidence: bool,
    /// Config-level per-turn subagent spawn cap (`agent_spawn_cap`). `None`
    /// falls back to the mode default (interactive 8, scripted/piped 2);
    /// `--agent-spawn-cap` overrides.
    pub(crate) agent_spawn_cap: Option<u32>,
    /// Config-level `[agents] state_file` mode deciding whether this process
    /// publishes the per-process agent state hint file.
    pub(crate) agent_state_file: crate::config::StateFileMode,
    /// Config-level `[agents] blocking` mode: how a background agent's
    /// ungranted permission request is handled (design B1).
    pub(crate) agent_blocking: crate::config::AgentBlockingMode,
    /// Config-level `[agents] block_timeout_secs`, already typed (B3).
    pub(crate) agent_block_timeout: std::time::Duration,
    /// Config-level `[hooks] on_agent_state` command, already split into argv
    /// (requirement H3). `None` when absent, empty, or unsplittable, so an
    /// unusable command is a no-op rather than a per-boundary parse attempt.
    pub(crate) agent_state_hook: Option<Vec<String>>,
    /// The config-level `[tui]` table (design §4.8), already defaulted. Held
    /// whole rather than as a resolved mode: the flags that override it are
    /// read where the renderer is installed, so there is one resolution point
    /// instead of one per key.
    pub(crate) tui: crate::config::TuiConfig,
    /// Effective run-level policy for Kitsu-created automatic output
    /// artifacts. This never changes explicit session, credential, `-o`, or
    /// model filesystem destinations.
    pub(crate) artifact_policy: ArtifactStoragePolicy,
    pub(crate) artifact_policy_source: ArtifactPolicySource,
    /// Resolved model context window in tokens, if known (design §3.2 order:
    /// `--context-window` → profile `context_window` → `/v1/models` → unset).
    pub(crate) context_window: Option<u64>,
    /// True when `context_window` came from an explicit `--context-window` flag
    /// or the gateway profile's `context_window`, rather than `/v1/models`
    /// discovery. A pinned window is a user/config declaration that overrides
    /// "any value discovered from `/v1/models`" (see the flag's doc comment) —
    /// `/model` must not silently replace it with fresh auto-discovery.
    pub(crate) context_window_pinned: bool,
    /// Human-readable, secret-free provenance for the effective context
    /// window. The background cold-cache probe updates only its discovery
    /// state; the conservative current-run budget remains unchanged.
    pub(crate) context_window_provenance: Arc<std::sync::Mutex<ContextWindowProvenance>>,
    pub(crate) context_window_refresh: ContextWindowRefreshConfig,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ArtifactPolicySource {
    Default,
    Config,
    Cli,
}

impl ArtifactPolicySource {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Config => "config",
            Self::Cli => "cli",
        }
    }
}

pub(crate) fn artifact_policy_receipt(
    policy: ArtifactStoragePolicy,
    source: ArtifactPolicySource,
    model_read_root_granted: bool,
) -> serde_json::Value {
    let automatic = policy == ArtifactStoragePolicy::Automatic;
    serde_json::json!({
        "mode": if automatic { "automatic" } else { "disabled" },
        "source": source.as_str(),
        "new_automatic_artifact_writes": automatic,
        "command_output_persistence": automatic,
        "tool_result_offload": automatic,
        "existing_artifacts_unchanged": !automatic,
        "model_artifact_read_root_granted": automatic && model_read_root_granted,
    })
}

pub(crate) fn project_context_disabled(cli: &Cli) -> bool {
    cli.no_project_context || std::env::var_os("KITSU_NO_PROJECT_CONTEXT").is_some()
}

/// Concatenate the bodies of the skills named via `--skill` into one context
/// block (or `None` if none/unknown). Unknown names warn and are skipped.
pub(crate) fn preload_skills(cli: &Cli, skillset: &skills::SkillSet) -> Option<String> {
    if cli.skill.is_empty() {
        return None;
    }
    let mut out = String::new();
    for name in &cli.skill {
        match skillset.load_with_snapshot(name) {
            Ok(loaded) => out.push_str(&format!("{}\n\n", loaded.text)),
            Err(error) => eprintln!(
                "~ cannot preload skill '{}': {} (skipped)",
                terminal_inline(name),
                terminal_inline(&error.to_string())
            ),
        }
    }
    let out = out.trim().to_string();
    if out.is_empty() {
        None
    } else {
        eprintln!("~ preloaded {} skill(s)", cli.skill.len());
        Some(out)
    }
}

pub(crate) const LOADED_SKILL_PREFIX: &str = "[Loaded skill: ";
pub(crate) const SKILL_SNAPSHOT_PREFIX: &str = "[Skill snapshot: ";

pub(crate) fn loaded_skill_name(content: &str) -> Option<&str> {
    let rest = content.strip_prefix(LOADED_SKILL_PREFIX)?;
    let end = rest.find("]\n")?;
    let name = &rest[..end];
    (!name.is_empty()).then_some(name)
}

pub(crate) fn loaded_skill_snapshot(content: &str) -> Option<skills::SkillContentSnapshot> {
    let snapshot_line = content.lines().nth(1)?;
    let encoded = snapshot_line
        .strip_prefix(SKILL_SNAPSHOT_PREFIX)?
        .strip_suffix(']')?;
    serde_json::from_str(encoded).ok()
}

pub(crate) fn loaded_skill_message(name: &str, loaded: &skills::LoadedSkill) -> ChatMessage {
    match serde_json::to_string(&loaded.snapshot) {
        Ok(snapshot) => ChatMessage::user(format!(
            "{LOADED_SKILL_PREFIX}{name}]\n{SKILL_SNAPSHOT_PREFIX}{snapshot}]\n\n{}",
            loaded.text
        )),
        // A non-UTF-8 host path can make a PathBuf snapshot non-serializable.
        // Retain the bounded loaded text; resume treats this as a legacy record
        // and revalidates it rather than crashing or trusting stale content.
        Err(_) => ChatMessage::user(format!("{LOADED_SKILL_PREFIX}{name}]\n\n{}", loaded.text)),
    }
}

/// Rebuild persisted `/skill` context from current descriptor-pinned sources.
/// Saved skill text is evidence, never authority: missing sources are removed,
/// changed hashes/resources are replaced, and every drift is disclosed.
pub(crate) fn refresh_loaded_skill_messages(
    messages: &mut Vec<ChatMessage>,
    skillset: &skills::SkillSet,
) -> Vec<String> {
    let mut refreshed = Vec::with_capacity(messages.len());
    let mut notices = Vec::new();
    for message in std::mem::take(messages) {
        let Some(name) = (matches!(message.role, kitsu_providers::Role::User))
            .then(|| loaded_skill_name(&message.content))
            .flatten()
        else {
            refreshed.push(message);
            continue;
        };
        match skillset.load_with_snapshot(name) {
            Ok(current) => {
                if let Some(previous) = loaded_skill_snapshot(&message.content) {
                    let drift = previous.drift_from(&current.snapshot);
                    if !drift.is_empty() {
                        notices.push(format!(
                            "refreshed loaded skill '{}' after {} changed",
                            name,
                            drift
                                .iter()
                                .map(|change| change.field.as_str())
                                .collect::<Vec<_>>()
                                .join(", ")
                        ));
                    }
                } else if message.content != loaded_skill_message(name, &current).content {
                    notices.push(format!(
                        "refreshed legacy loaded skill '{}' and captured current provenance",
                        name
                    ));
                }
                refreshed.push(loaded_skill_message(name, &current));
            }
            Err(error) => notices.push(format!("removed saved loaded skill '{}': {}", name, error)),
        }
    }
    *messages = refreshed;
    notices
}

/// Register the base tool set (fs read/write, grep, skill) shared by the main
/// agent and by subagents. `spawn_agent` is registered separately and only for
/// the main agent, so subagents can't spawn further (depth-1).
///
/// `tier` trims the advertised read surface for small context windows — see
/// [`kitsu_tools::ToolTier`]. It changes only what the model is TOLD exists;
/// permissions and every authorization check are identical in both tiers.
// Each parameter is an independent axis of the registry (authority, skills,
// journal, command policy, advertised breadth) that the three call sites vary
// separately; bundling them into a struct would only move the argument list.
#[allow(clippy::too_many_arguments)]
pub(crate) fn register_base_tools(
    tools: &mut Registry,
    perms: &Arc<Permissions>,
    skillset: &Arc<skills::SkillSet>,
    can_read: bool,
    can_write: bool,
    journal: &kitsu_tools::builtins::fs::WriteJournal,
    commands: Option<&Arc<kitsu_tools::CommandPolicy>>,
    tier: kitsu_tools::ToolTier,
) -> Result<(), RegistryError> {
    if can_read {
        builtins::register_fs_tools_tiered(tools, perms.clone(), tier)?;
    }
    if can_write {
        let (descriptor, handler) =
            builtins::fs::write_file_tool_with_journal(perms.clone(), Some(journal.clone()));
        tools.register_with_descriptor(descriptor, handler)?;
    }
    // Exact-text edits necessarily inspect existing content. Do not advertise
    // that read-modify-write primitive for a genuinely write-only invocation.
    if can_read && can_write {
        let (descriptor, handler) =
            builtins::fs::edit_file_tool_with_journal(perms.clone(), Some(journal.clone()));
        tools.register_with_descriptor(descriptor, handler)?;
    }
    if !skillset.is_empty() {
        let (d, h) = skills::skill_tool(skillset.clone());
        tools.register_with_descriptor(d, h)?;
    }
    if let Some(commands) = commands {
        builtins::register_command_tool(tools, commands.clone())?;
    }

    Ok(())
}

pub(crate) fn can_resolve_offload_pointers(tool_names: &[String]) -> bool {
    tool_names.iter().any(|name| name == "fs_read_file")
}

/// Parse the `--compaction` flag into the orchestrator's mode.
pub(crate) fn compaction_mode(cli: &Cli) -> CompactionMode {
    if cli.compaction == "summarize" {
        CompactionMode::Summarize
    } else {
        CompactionMode::Trim
    }
}

pub(crate) fn compaction_mode_name(mode: CompactionMode) -> &'static str {
    match mode {
        CompactionMode::Trim => "trim",
        CompactionMode::Summarize => "summarize",
    }
}

pub(crate) fn compaction_reason_name(reason: kitsu_core::CompactionReason) -> &'static str {
    match reason {
        kitsu_core::CompactionReason::ContextTargetExceeded => "context_target_exceeded",
        kitsu_core::CompactionReason::ManualRequest => "manual_request",
    }
}

pub(crate) fn compaction_outcome_name(outcome: kitsu_core::CompactionOutcome) -> &'static str {
    match outcome {
        kitsu_core::CompactionOutcome::ToolResultsElided => "tool_results_elided",
        kitsu_core::CompactionOutcome::Trimmed => "trimmed",
        kitsu_core::CompactionOutcome::Summarized => "summarized",
        kitsu_core::CompactionOutcome::SummaryFallbackTrim => "summary_fallback_trim",
        kitsu_core::CompactionOutcome::SummaryFallbackNoOp => "summary_fallback_no_op",
    }
}

pub(crate) fn compaction_fallback_name(reason: kitsu_core::SummaryFallbackReason) -> &'static str {
    match reason {
        kitsu_core::SummaryFallbackReason::CircuitBreaker => "circuit_breaker",
        kitsu_core::SummaryFallbackReason::RequestTooLarge => "request_too_large",
        kitsu_core::SummaryFallbackReason::ProviderFailure => "provider_failure",
        kitsu_core::SummaryFallbackReason::EmptyCompletion => "empty_completion",
        kitsu_core::SummaryFallbackReason::IncompleteResponse => "incomplete_response",
        kitsu_core::SummaryFallbackReason::ResponseTooLarge => "response_too_large",
        kitsu_core::SummaryFallbackReason::ContextWindowUnknown => "context_window_unknown",
    }
}

pub(crate) fn compaction_report_payload(
    report: &kitsu_core::CompactionReport,
) -> serde_json::Value {
    let record = report.record.as_ref().map(|record| {
        serde_json::json!({
            "sequence": record.sequence,
            "reason": compaction_reason_name(record.reason),
            "configured_mode": compaction_mode_name(record.configured_mode),
            "outcome": compaction_outcome_name(record.outcome),
            "fallback_reason": record.fallback_reason.map(compaction_fallback_name),
            "tokens_before": record.tokens_before,
            "tokens_after": record.tokens_after,
            "messages_before": record.messages_before,
            "messages_after": record.messages_after,
            "tool_results_elided": record.tool_results_elided,
            "exchanges_removed": record.exchanges_removed,
            "summary_input_shortened": record.summary_input_shortened,
            "summary_output_shortened": record.summary_output_shortened,
        })
    });
    serde_json::json!({
        "kind": "compaction",
        "changed": report.changed,
        "target_tokens": report.target_tokens,
        "tokens_before": report.tokens_before,
        "tokens_after": report.tokens_after,
        "tokens_saved": report.tokens_before.saturating_sub(report.tokens_after),
        "record": record,
        "usage": {
            "prompt_tokens": report.usage.prompt_tokens,
            "completion_tokens": report.usage.completion_tokens,
            "total_tokens": report.usage.total_tokens,
        },
    })
}

/// Build the per-turn `AgentConfig` shared by `ask`, scripted runs, and
/// interactive chat, from a single place — so a future field/default change
/// can't silently apply to only some call sites (as `ask`'s hardcoded
/// `offload_dir: None` very nearly diverged from the other two paths).
pub(crate) fn build_agent_config(
    model: String,
    cli: &Cli,
    context_window: Option<u64>,
    offload_dir: Option<std::path::PathBuf>,
    artifact_policy: ArtifactStoragePolicy,
) -> AgentConfig {
    let reasoning_effort = cli_reasoning_effort(cli);
    AgentConfig {
        model,
        request_timeout_ms: Some(cli.timeout_secs.saturating_mul(1_000)),
        turn_deadline: None,
        max_rounds: cli.max_rounds,
        max_context_bytes: 128 * 1024,
        // The CLI's documented conservative default is an enforcement value,
        // not just a display fallback. Keep the session cell as `None` so
        // `/status` can disclose that it was defaulted, but every provider
        // request and manual compaction receives the same checked 8K budget.
        context_window: Some(context_window.unwrap_or(DEFAULT_CONTEXT_WINDOW)),
        offload_dir: (artifact_policy == ArtifactStoragePolicy::Automatic)
            .then_some(offload_dir)
            .flatten(),
        offload_char_cap: cli.offload_cap.map(|cap| cap as usize),
        artifact_policy,
        stream: !cli.no_stream,
        temperature: cli.temperature,
        reasoning_effort,
        max_tokens: cli.max_output_tokens,
        response_format: active_output_schema().map(|schema| JsonSchemaFormat {
            name: "kitsu_output".to_string(),
            schema: schema.value.clone(),
            strict: true,
        }),
        defer_response_format_until_final: false,
        structured_finalization_gate: None,
        compaction: compaction_mode(cli),
        quiet: cli.jsonl,
        event_log: None,
        skill_names: Vec::new(),
        run_tallies: None,
        spawn_budget: None,
    }
}

pub(crate) fn cli_reasoning_effort(cli: &Cli) -> Option<kitsu_providers::ReasoningEffort> {
    cli.reasoning_effort.as_deref().map(|effort| match effort {
        "none" => kitsu_providers::ReasoningEffort::None,
        "minimal" => kitsu_providers::ReasoningEffort::Minimal,
        "low" => kitsu_providers::ReasoningEffort::Low,
        "medium" => kitsu_providers::ReasoningEffort::Medium,
        "high" => kitsu_providers::ReasoningEffort::High,
        "xhigh" => kitsu_providers::ReasoningEffort::XHigh,
        _ => unreachable!("clap validates reasoning effort"),
    })
}

/// The bound a non-interactive exit puts on joining live background agents.
/// Unset means "as long as one model turn is allowed to take", which is the
/// only other deadline the invocation already declared.
pub(crate) fn agent_join_timeout(cli: &Cli) -> std::time::Duration {
    std::time::Duration::from_secs(cli.agent_join_timeout.unwrap_or(cli.timeout_secs))
}

pub(crate) fn reasoning_effort_label(
    effort: Option<kitsu_providers::ReasoningEffort>,
) -> &'static str {
    match effort {
        None => "gateway default",
        Some(kitsu_providers::ReasoningEffort::None) => "none",
        Some(kitsu_providers::ReasoningEffort::Minimal) => "minimal",
        Some(kitsu_providers::ReasoningEffort::Low) => "low",
        Some(kitsu_providers::ReasoningEffort::Medium) => "medium",
        Some(kitsu_providers::ReasoningEffort::High) => "high",
        Some(kitsu_providers::ReasoningEffort::XHigh) => "xhigh",
    }
}

/// Encode the canonical config location without lossy path conversion. This is
/// an identity input only: the bytes are hashed before they enter the route
/// declaration and are never exposed in provider receipts.
pub(crate) fn provider_profile_namespace(config_path: &std::path::Path) -> Vec<u8> {
    let config_path =
        std::fs::canonicalize(config_path).unwrap_or_else(|_| config_path.to_path_buf());
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        config_path.as_os_str().as_bytes().to_vec()
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        config_path
            .as_os_str()
            .encode_wide()
            .flat_map(u16::to_le_bytes)
            .collect()
    }
    #[cfg(not(any(unix, windows)))]
    {
        config_path.to_string_lossy().as_bytes().to_vec()
    }
}

/// Declare the exact compiled route selected by configuration. `flavor`
/// remains a compatibility/display label and cannot silently select a wire
/// dialect; only the closed `adapter` field can do that.
pub(crate) fn current_provider_route_declaration(
    config_path: &std::path::Path,
    gateway_name: &str,
    gateway: &Gateway,
) -> Result<ProviderRouteDeclaration, ProviderRouteContractError> {
    let profile = current_provider_profile_id(config_path, gateway_name)?;
    let has_model_catalog = gateway
        .models_path
        .as_deref()
        .is_some_and(|path| !path.is_empty());
    Ok(match gateway.adapter {
        GatewayAdapter::CompatibleChat => {
            ProviderRouteDeclaration::compatible_chat_v1(profile, has_model_catalog)
        }
        GatewayAdapter::AnthropicMessages => ProviderRouteDeclaration::native_messages_v1(profile),
    })
}

pub(crate) fn current_provider_catalog_route_declaration(
    config_path: &std::path::Path,
    gateway_name: &str,
) -> Result<ProviderRouteDeclaration, ProviderRouteContractError> {
    Ok(ProviderRouteDeclaration::compatible_catalog_v1(
        current_provider_profile_id(config_path, gateway_name)?,
    ))
}

pub(crate) fn current_provider_profile_id(
    config_path: &std::path::Path,
    gateway_name: &str,
) -> Result<ProviderProfileId, ProviderRouteContractError> {
    ProviderProfileId::derive_v1(
        provider_profile_namespace(config_path),
        gateway_name.as_bytes(),
    )
}

/// Identify the credential source that the immediately following auth freeze
/// will resolve. Missing or malformed credentials fail before a snapshot is
/// returned, so only successful values can become a route receipt.
pub(crate) fn current_provider_auth_source(
    gateway: &Gateway,
    managed_token_available: bool,
) -> ProviderAuthSource {
    match gateway.auth.method.as_str() {
        "none" => ProviderAuthSource::None,
        "static" => ProviderAuthSource::StaticConfiguration,
        "bearer-env" => {
            let environment_selected = std::env::var_os(&gateway.auth.env)
                .and_then(|value| value.into_string().ok())
                .is_some_and(|value| !value.trim().is_empty());
            if environment_selected {
                ProviderAuthSource::Environment
            } else if managed_token_available {
                ProviderAuthSource::ManagedCredential
            } else {
                // The freeze will return the existing typed missing/malformed
                // auth error; this placeholder is never published.
                ProviderAuthSource::Environment
            }
        }
        _ => ProviderAuthSource::None,
    }
}

/// Resolve config + gateway profile and build the provider/auth/model. Emits the
/// best-effort token-expiry warning (warn-but-send, design §3).
/// Best-effort gateway choice: `--model` without `--gateway` goes to the
/// auto-detected local server when it lists that model (exactly, or as a unique
/// substring like `glm`), resolved to the full local id. Otherwise the
/// requested gateway and model pass through unchanged.
pub(crate) fn route_model_selection<'a>(
    cli: &'a Cli,
    config: &Config,
) -> Result<(Option<&'a str>, Option<String>), CliError> {
    let requested_model = cli.model.clone();
    let (None, Some(model)) = (cli.gateway.as_deref(), requested_model.as_deref()) else {
        return Ok((cli.gateway.as_deref(), requested_model));
    };
    match config
        .route_model_to_local(model)
        .map_err(|error| CliError::user(error.to_string()))?
    {
        Some(local_model) => {
            if !cli.jsonl {
                eprintln!(
                    "~ --model {} found on the local server; using gateway local ({})",
                    terminal_inline(model),
                    terminal_inline(&local_model)
                );
            }
            Ok((Some("local"), Some(local_model)))
        }
        None => Ok((None, requested_model)),
    }
}

pub(crate) async fn build_setup(cli: &Cli) -> Result<ChatSetup, CliError> {
    let path = Config::resolve_path(cli.config.as_deref());
    let selected_config = cli.config.is_some()
        || std::env::var_os("KITSU_CONFIG").is_some_and(|value| !value.is_empty());
    if !path.exists() {
        let message = if selected_config {
            format!(
                "config error: requested config '{}' does not exist",
                path.display()
            )
        } else {
            format!(
                "config error: no config found at {}\n  start with: kitsu init\n  then check: kitsu --config ./kitsu.toml doctor\n  then chat: kitsu --config ./kitsu.toml chat\n  or copy the bundled config to ~/.kitsu/config.toml",
                shorten_home(&path)
            )
        };
        return Err(CliError::user(message));
    }
    let config = Config::load(&path).map_err(|error| CliError::user(safe_config_error(&error)))?;
    let (artifact_policy, artifact_policy_source) = if cli.no_artifacts {
        (ArtifactStoragePolicy::InlineOnly, ArtifactPolicySource::Cli)
    } else {
        match config.artifact_storage {
            Some(false) => (
                ArtifactStoragePolicy::InlineOnly,
                ArtifactPolicySource::Config,
            ),
            Some(true) => (
                ArtifactStoragePolicy::Automatic,
                ArtifactPolicySource::Config,
            ),
            None => (
                ArtifactStoragePolicy::Automatic,
                ArtifactPolicySource::Default,
            ),
        }
    };
    // Resolve the location now, but create it only when a command actually
    // needs durable state. In particular, provider-free/empty scripts should
    // continue to work in containers with a read-only HOME.
    let state_dir = runtime_state_dir(&path);

    let (requested_gateway, requested_model) = route_model_selection(cli, &config)?;
    let (gateway_name, gateway) = config
        .select_gateway(requested_gateway)
        .map_err(|e| CliError::user(e.to_string()))?;
    let gateway_name = gateway_name.to_string();
    let provider_diagnostics = ProviderDiagnosticContext::from_gateway(&gateway_name, gateway);
    gateway
        .validate_flavor()
        .map_err(|e| CliError::user(e.to_string()))?;
    let provider_route_declaration =
        current_provider_route_declaration(&path, &gateway_name, gateway).map_err(|error| {
            CliError::user(format!(
                "config error: cannot derive provider profile identity: {error}"
            ))
        })?;
    let model = gateway
        .resolve_model(requested_model.as_deref())
        .map_err(|e| CliError::user(e.to_string()))?;
    let managed_token = managed_credential(&state_dir, &gateway_name, gateway)?;
    let provider_auth_source = current_provider_auth_source(gateway, managed_token.is_some());

    let auth_status = match gateway.auth.method.as_str() {
        "none" => "none (no credential required)".to_string(),
        "static" => "static credential configured".to_string(),
        "bearer-env" => format!(
            "bearer-env {} (source: {})",
            gateway.auth.env,
            match provider_auth_source {
                ProviderAuthSource::Environment => "environment",
                ProviderAuthSource::ManagedCredential => "managed",
                ProviderAuthSource::None | ProviderAuthSource::StaticConfiguration => "missing",
            }
        ),
        other => other.to_string(),
    };
    let endpoint = redact_endpoint(&format!("{}{}", gateway.base_url, gateway.chat_path));

    let auth = build_auth(gateway, &gateway_name, managed_token)
        .map_err(|e| CliError::user(e.to_string()))?;
    let models_path = gateway.models_path.clone().filter(|p| !p.is_empty());
    let context_window_refresh = ContextWindowRefreshConfig {
        cache_path: context_window_cache_path(&state_dir),
        base_url: gateway.base_url.clone(),
        models_path: models_path.clone(),
    };
    let catalog_policy = if models_path.is_some() {
        ProviderCatalogPolicy::Configured
    } else {
        ProviderCatalogPolicy::Unavailable
    };
    let route_policy = ProviderRoutePolicy::new(
        2,
        0,
        Duration::from_secs(cli.timeout_secs),
        Duration::from_secs(cli.timeout_secs),
        Duration::from_secs(2),
        catalog_policy,
    )
    .map_err(|error| CliError::user(format!("config error: invalid provider policy: {error}")))?;
    let route_config = compiled_provider_route_config(
        gateway,
        provider_route_declaration,
        model.clone(),
        provider_auth_source,
        route_policy,
    );
    let provider_route_snapshot = build_compiled_provider_route(route_config, auth.as_ref())
        .map_err(|error| {
            route_build_cli_error(
                error,
                ProviderOperation::Chat { model: &model },
                provider_diagnostics,
            )
        })?;
    let provider_route = ProviderRouteCurrent::new(provider_route_snapshot);
    provider_route.bind().map_err(|error| {
        CliError::user(format!(
            "config error: cannot bind initial provider route: {error}"
        ))
    })?;

    if let kitsu_providers::ExpiryStatus::Expired { since } = auth.expiry_status() {
        eprintln!(
            "{}",
            expired_auth_warning(&since, gateway.auth.refresh_hint.as_deref())
        );
    }

    // Resolve the context window: explicit flag → profile → cached
    // discovery. A cold optional catalog probe warms the disk cache in the
    // background; it never delays the healthy inference path. This invocation
    // uses the documented conservative default until a later run finds that
    // cache (build_agent_config enforces the default, while `None` preserves
    // honest provenance in /status and /context).
    let context_window_pinned = cli.context_window.or(gateway.context_window).is_some();
    let mut cold_refresh = None;
    let (context_window, context_window_provenance) = if let Some(window) = cli.context_window {
        (
            Some(window),
            ContextWindowProvenance::pinned(ContextWindowSource::Cli),
        )
    } else if let Some(window) = gateway.context_window {
        (
            Some(window),
            ContextWindowProvenance::pinned(ContextWindowSource::Profile),
        )
    } else {
        let cache_key = context_window_refresh.cache_key(&model);
        let cache_path = context_window_refresh.cache_path.clone();
        let cached = inspect_cached_context_window(&cache_path, &cache_key);
        match cached.window {
            Some(window) => {
                let age_secs = match cached.state {
                    ContextWindowCacheState::Fresh { age_secs, .. } => age_secs,
                    _ => 0,
                };
                (Some(window), ContextWindowProvenance::cached(age_secs))
            }
            None => {
                let discovery = if models_path.is_some() {
                    cold_refresh = Some((cache_path, cache_key));
                    ContextWindowDiscoveryState::Pending
                } else {
                    ContextWindowDiscoveryState::Disabled
                };
                (
                    None,
                    ContextWindowProvenance::defaulted(cached.state, discovery),
                )
            }
        }
    };
    // Ground truth beats assumption. Every source above is a claim about the
    // window (a flag, a profile line, a catalog field); the runtime's SERVED
    // window is what the next request will actually be measured against. Ollama
    // in particular loads a 131072-token model with a 4096-token window, reports
    // nothing on /v1/models, and truncates oversize prompts from the front
    // without erroring — so budgeting against a too-large claim silently
    // destroys the system prompt and tool schemas.
    //
    // Clamp DOWN only. A served window larger than the configured one is not a
    // reason to raise the budget: the profile may be deliberately conservative,
    // and nothing is broken by under-filling. An explicit `--context-window`
    // stays authoritative (it is the documented escape hatch for a wrong or
    // absent probe) but still gets a warning when it disagrees, so a truncating
    // run is never silent.
    //
    // Scoped to endpoints that plausibly ARE Ollama. `/api/ps` is an
    // Ollama-native route, so probing it everywhere would add a startup request
    // that every other gateway can only 404 — real latency and traffic for
    // information they never expose.
    //
    // A flavor check alone is not enough: the shipped `[gateways.ollama]`
    // profile declares `flavor = "openai"`, so it would miss the exact case
    // this fixes. Loopback alone is too broad — it would probe every local
    // vLLM and llama.cpp too. The union of the profile's identity and Ollama's
    // default port catches a differently-named profile pointing at a standard
    // install, while leaving other local runtimes alone.
    let looks_like_ollama = base_url_has_loopback_host(&gateway.base_url)
        && (gateway_name.eq_ignore_ascii_case("ollama")
            || gateway.flavor.eq_ignore_ascii_case("ollama")
            || gateway.base_url.contains(":11434"));
    // Strictly advisory: a route that cannot be bound, or a probe that errors,
    // means "no better information" and leaves the resolved window alone.
    let served_window = if looks_like_ollama {
        provider_route
            .served_context_length(&model)
            .await
            .ok()
            .flatten()
    } else {
        None
    };
    let (context_window, context_window_provenance) = match (context_window, served_window) {
        (Some(resolved), Some(served)) if served < resolved => {
            let from_cli = cli.context_window.is_some();
            eprintln!(
                "~ the gateway is serving {model} with a {served}-token context window, \
                 smaller than the resolved {resolved}. {}",
                if from_cli {
                    "Keeping your explicit --context-window; prompts over the served window will \
                     be truncated by the runtime, usually destroying the system prompt and tool \
                     schemas first."
                } else {
                    "Budgeting against the served window instead (/context shows provenance); \
                     raise it at the runtime — for Ollama, OLLAMA_CONTEXT_LENGTH or a Modelfile \
                     num_ctx — not here."
                }
            );
            if from_cli {
                (Some(resolved), context_window_provenance)
            } else {
                (
                    Some(served),
                    ContextWindowProvenance::pinned(ContextWindowSource::ServedWindow),
                )
            }
        }
        // No probe, an unloaded model, a non-Ollama gateway, or a served window
        // at least as large as the resolved one: keep what we resolved.
        (window, _) => (window, context_window_provenance),
    };
    let context_window_provenance = Arc::new(std::sync::Mutex::new(context_window_provenance));
    if let Some((cache_path, cache_key)) = cold_refresh {
        let discovery_provider = provider_route.clone();
        let discovery_model = model.clone();
        let discovery_provenance = context_window_provenance.clone();
        tokio::spawn(async move {
            let discovery = match refresh_cached_context_window(
                &discovery_provider,
                &discovery_model,
                &cache_path,
                &cache_key,
            )
            .await
            {
                Ok(ContextWindowRefreshOutcome::Cached(window)) => {
                    ContextWindowDiscoveryState::ReadyForNextRun { window }
                }
                Ok(ContextWindowRefreshOutcome::DiscoveredButNotCached(window)) => {
                    ContextWindowDiscoveryState::CompleteButNotCached { window }
                }
                Ok(ContextWindowRefreshOutcome::NoMetadata) | Err(_) => {
                    ContextWindowDiscoveryState::FinishedWithoutMetadata
                }
            };
            let mut provenance = discovery_provenance
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            // `/model` or session resume can replace the running model while
            // this advisory probe is in flight. Never relabel that newer
            // model's provenance with the old model's completion.
            if provenance.discovery == ContextWindowDiscoveryState::Pending {
                provenance.discovery = discovery;
            }
        });
    }

    if let Some(name) = cli.theme.as_deref().or(config.theme.as_deref()) {
        kitsu_render::set_theme(name).map_err(CliError::user)?;
    }

    let agents_config = config.agents.unwrap_or_default();
    Ok(ChatSetup {
        provider_route,
        gateway_name,
        provider_diagnostics,
        config_path: path,
        state_dir,
        endpoint,
        auth_status,
        workspace_evidence: config.workspace_evidence.unwrap_or(true),
        agent_spawn_cap: config.agent_spawn_cap,
        agent_state_file: agents_config.state_file,
        agent_blocking: agents_config.blocking,
        agent_block_timeout: agents_config.block_timeout(),
        agent_state_hook: config
            .hooks
            .unwrap_or_default()
            .on_agent_state
            .as_deref()
            .and_then(agent_hook::split_command),
        tui: config.tui.unwrap_or_default(),
        artifact_policy,
        artifact_policy_source,
        context_window,
        context_window_pinned,
        context_window_provenance,
        context_window_refresh,
    })
}

/// Build the appropriate AuthProvider from the profile's auth block.
pub(crate) fn build_auth(
    gateway: &Gateway,
    gateway_name: &str,
    managed_token: Option<zeroize::Zeroizing<String>>,
) -> Result<Arc<dyn AuthProvider>, ConfigError> {
    gateway.validate_auth()?;
    let auth = &gateway.auth;
    let provider: Arc<dyn AuthProvider> = match auth.method.as_str() {
        "none" => Arc::new(NoAuth),
        "static" => {
            let token = auth.token.clone().ok_or(ConfigError::StaticTokenRequired)?;
            Arc::new(StaticAuth {
                token,
                header: auth.header.clone(),
                scheme: auth.scheme.clone(),
            })
        }
        "bearer-env" => {
            let login_hint = format!(
                "run `kitsu --gateway {} auth login`",
                terminal_inline(gateway_name)
            );
            let refresh_hint = safe_auth_refresh_hint(auth.refresh_hint.as_deref())
                .filter(|hint| hint != &login_hint)
                .map(|hint| format!("{login_hint}, or {hint}"))
                .unwrap_or(login_hint);
            Arc::new(EnvBearerAuth {
                env: auth.env.clone(),
                expires_env: auth.expires_env.clone(),
                header: auth.header.clone(),
                scheme: auth.scheme.clone(),
                // Store only display-safe guidance in the runtime auth provider
                // so every later Missing/Unauthorized error is safe by
                // construction.
                refresh_hint: Some(refresh_hint),
                managed_token,
            })
        }
        other => {
            return Err(ConfigError::UnsupportedAuthMethod {
                method: other.to_string(),
            })
        }
    };
    Ok(provider)
}

/// Validate local credential readiness before inference-capable commands build
/// project context, register tools, emit run-start telemetry, or contact a
/// gateway. This calls the same auth-provider path as the real request and
/// immediately discards the sensitive header, keeping validation and recovery
/// wording consistent without ever exposing the credential value.
pub(crate) fn require_auth_ready(
    auth: &dyn AuthProvider,
    operation: ProviderOperation<'_>,
) -> Result<(), CliError> {
    auth.header()
        .map(|_| ())
        .map_err(|error| map_provider_error(ProviderError::Auth(error), operation, None))
}
