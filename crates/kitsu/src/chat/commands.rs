//! Interactive and scripted slash-command dispatch.

use crate::review::REVIEW_SCOPE_USAGE;
use crate::terminal::approval::AUTO_MODE_ON_NOTICE;

use crate::cli::Cli;
use kitsu_core::context::DEFAULT_CONTEXT_WINDOW;
use kitsu_providers::compiled::CompiledProvider;
use kitsu_providers::{ChatMessage, ProviderRouteCurrent};
use kitsu_tools::{Permissions, Registry};
use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use crate::agent_commands::agent_profiles_payload;
use crate::agent_commands::await_background_agents;
use crate::agent_commands::format_agent_profiles;
use crate::agent_commands::handle_jobs_command_attaching;
use crate::agent_commands::terminal_cleanup_state;
use crate::artifact_commands::cross_run_artifacts;
use crate::artifact_commands::handle_command_artifacts;
use crate::chat::state::context_window_provenance;
use crate::chat::state::SessionInfo;
use crate::checkpoints::format_checkpoint_summaries;
use crate::checkpoints::format_write_checkpoint;
use crate::checkpoints::report_write_checkpoints;
use crate::checkpoints::trusted_undo_history_event;
use crate::checkpoints::undo_hint_actionable;
use crate::checkpoints::undo_write_checkpoint;
use crate::command_policy::dangerous_bypass_revocation_error;
use crate::command_registry::matching_slash_commands;
use crate::command_registry::slash_help;
use crate::command_registry::SLASH_COMMANDS;
use crate::context_window::refresh_cached_context_window_explicit;
use crate::context_window::ContextWindowDiscoveryState;
use crate::context_window::ContextWindowProvenance;
use crate::context_window::ContextWindowRefreshOutcome;
use crate::doctor::run_doctor;
use crate::management::inferred_export_format;
use crate::models::filter_model_catalog;
use crate::models::finish_slash_model_catalog;
use crate::models::list_models_and_print;
use crate::models::list_models_with_runtime_telemetry;
use crate::models::report_slash_provider_error;
use crate::presentation::shorten_home;
use crate::presentation::terminal_inline;
use crate::prompt_queue;
use crate::provider_diagnostics::chat_error_message_with_diagnostics;
use crate::provider_diagnostics::ProviderOperation;
use crate::review::authorized_workspace_review;
use crate::review::empty_review_message;
use crate::review::evidence_disclosure_line;
use crate::review::evidence_disclosure_payload;
use crate::review::parse_exact_checkpoint_id;
use crate::review::parse_optional_checkpoint_id;
use crate::review::parse_review_request;
use crate::review::review_evidence_is_empty;
use crate::review::review_target_label;
use crate::review::verified_review_prompt;
use crate::session;
use crate::session_catalog::resume_picker;
use crate::session_telemetry::collect_context_sources_snapshot;
use crate::session_telemetry::collect_current_session_snapshot;
use crate::session_telemetry::context_command_payload;
use crate::session_telemetry::current_session_deck_options;
use crate::session_telemetry::format_permissions;
use crate::session_telemetry::permission_mode_label;
use crate::session_telemetry::permissions_payload;
use crate::session_telemetry::redirected_current_session_deck_options;
use crate::session_telemetry::render_context_command;
use crate::session_telemetry::session_status_payload;
use crate::session_telemetry_render;
use crate::setup::build_agent_config;
use crate::setup::compaction_report_payload;
use crate::setup::loaded_skill_message;
use crate::setup::reasoning_effort_label;
use crate::skills;
use crate::subagent;
use crate::terminal::agent_view::apply_detach_command;
#[cfg(unix)]
use crate::terminal::model_picker::run_model_picker;
#[cfg(unix)]
use crate::tui_bridge;
use crate::turn::await_cancellable;
use crate::turn::rollback_agent_state;
use crate::turn::script_failure_kind;
use crate::turn::ScriptFailureKind;

pub(crate) fn restored_messages(
    saved: &session::SavedSession,
    fresh_system: &ChatMessage,
) -> Vec<ChatMessage> {
    let mut messages = saved.messages.clone();
    if !messages.is_empty() {
        // Capabilities, skills, project context, and their budgets belong to
        // the current invocation. Never replay a saved system prompt whose
        // tool/schema/skill claims may no longer match the active boundary.
        messages[0] = fresh_system.clone();
    }
    messages
}

pub(crate) fn same_directory(saved: &str, current: &std::path::Path) -> bool {
    let saved = std::path::Path::new(saved);
    match (std::fs::canonicalize(saved), std::fs::canonicalize(current)) {
        (Ok(saved), Ok(current)) => saved == current,
        _ => saved == current,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TurnToolMode {
    Full,
    None,
}

#[derive(Debug, Clone)]
pub(crate) struct SubmittedPrompt {
    pub(crate) text: String,
    pub(crate) tool_mode: TurnToolMode,
    /// Structured record of a one-shot evidence disclosure, emitted as the
    /// command's `command_result` in JSONL mode.
    pub(crate) disclosure: Option<serde_json::Value>,
}

impl SubmittedPrompt {
    pub(crate) fn full(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            tool_mode: TurnToolMode::Full,
            disclosure: None,
        }
    }

    pub(crate) fn review(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            tool_mode: TurnToolMode::None,
            disclosure: None,
        }
    }

    pub(crate) fn with_disclosure(mut self, disclosure: serde_json::Value) -> Self {
        self.disclosure = Some(disclosure);
        self
    }
}

pub(crate) enum SlashResult {
    Continue(Option<serde_json::Value>),
    Quit(kitsu_core::RunCleanupState),
    Error,
    /// A host-authored, terminal-safe user error that automation should retain
    /// instead of replacing with the generic scripted-command fallback.
    UserFailure(String),
    ProviderFailure(String),
    Cancelled,
    SubmitPrompt(SubmittedPrompt),
    QueueAction(QueueCommand),
    Retry,
    ClearSession,
    NewSession,
    ResumeSession(String),
    ListSessions(String),
    ForkSession(Option<String>),
    ExportSession {
        destination: std::path::PathBuf,
        format: session::SessionExportFormat,
    },
    ArchiveSession(String),
    RestoreSession(String),
    DeleteSession(String),
    /// `/tui classic|inline|fullscreen`: swap the live renderer between
    /// turns.
    ///
    /// Carried out of the dispatcher rather than performed in it, because the
    /// swap is the `BridgeGuard`'s business and the guard is a `run_chat`
    /// local the dispatcher cannot see. The dispatcher's job is to decide
    /// *which* renderer was asked for; `run_chat` owns the teardown, the
    /// re-install and the receipt.
    SwitchTui(TuiRendererRequest),
}

/// The renderer `/tui` asked for. Deliberately not `tui_bridge::TuiMode`:
/// this type crosses the `#[cfg(unix)]` seam (the dispatcher and
/// `SlashResult` are platform-independent, the bridge is not).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TuiRendererRequest {
    Classic,
    Inline,
    Fullscreen,
}

impl TuiRendererRequest {
    pub(crate) fn label(self) -> &'static str {
        match self {
            TuiRendererRequest::Classic => "classic",
            TuiRendererRequest::Inline => "inline",
            TuiRendererRequest::Fullscreen => "fullscreen",
        }
    }
}

/// The modes `/tui` accepts, named in every message it prints so a user who
/// reads any one of them learns the whole grammar, `fullscreen` among them
/// now that a session can both start in it and switch into it.
pub(crate) const TUI_ACCEPTED_MODES: &str = "accepted: classic, inline, fullscreen";

/// The refusal a `/tui` typed **during a turn** gets.
///
/// The renderer is not swapped mid-turn, and — this is the point — the line
/// is not allowed to fall through to the type-ahead queue either, where it
/// would quietly become a future prompt sent to the model.
pub(crate) const TUI_MID_TURN_REFUSAL: &str =
    "/tui is accepted only between turns; the renderer is not swapped mid-turn";

/// The renderer in use at this instant, as `/tui` reports it.
pub(crate) fn live_tui_mode_label() -> &'static str {
    #[cfg(unix)]
    {
        tui_bridge::live_mode().label()
    }
    #[cfg(not(unix))]
    {
        "classic"
    }
}

#[derive(Debug)]
pub(crate) enum QueueCommand {
    List,
    Add(String),
    /// `/loop`: enqueue the same prompt several times in sequence.
    AddCopies {
        prompt: String,
        count: usize,
    },
    Remove(usize),
    Edit(usize, String),
    Move {
        from: usize,
        to: usize,
    },
    Promote(usize),
    Clear,
}

pub(crate) fn parse_queue_position(value: Option<&str>, usage: &str) -> Result<usize, String> {
    let Some(position) = value.and_then(|value| value.parse::<usize>().ok()) else {
        return Err(usage.to_string());
    };
    position
        .checked_sub(1)
        .ok_or_else(|| "queue positions start at 1".to_string())
}

pub(crate) fn parse_queue_command(arg: &str) -> Result<QueueCommand, String> {
    let arg = arg.trim();
    if arg.is_empty() || arg == "list" {
        return Ok(QueueCommand::List);
    }
    let mut words = arg.split_whitespace();
    match words.next().unwrap_or_default() {
        "add" => {
            let text = arg.strip_prefix("add").unwrap_or_default().trim();
            if text.is_empty() {
                Err("usage: /queue add <prompt>".to_string())
            } else {
                Ok(QueueCommand::Add(text.to_string()))
            }
        }
        "remove" => {
            let position = parse_queue_position(words.next(), "usage: /queue remove <number>")?;
            if words.next().is_some() {
                return Err("usage: /queue remove <number>".to_string());
            }
            Ok(QueueCommand::Remove(position))
        }
        "edit" => {
            let position =
                parse_queue_position(words.next(), "usage: /queue edit <number> <prompt>")?;
            let mut split = arg.splitn(3, char::is_whitespace);
            let _ = split.next();
            let _ = split.next();
            let replacement = split.next().unwrap_or_default().trim();
            if replacement.is_empty() {
                Err("usage: /queue edit <number> <prompt>".to_string())
            } else {
                Ok(QueueCommand::Edit(position, replacement.to_string()))
            }
        }
        "move" => {
            let from = parse_queue_position(words.next(), "usage: /queue move <from> <to>")?;
            let to = parse_queue_position(words.next(), "usage: /queue move <from> <to>")?;
            if words.next().is_some() {
                return Err("usage: /queue move <from> <to>".to_string());
            }
            Ok(QueueCommand::Move { from, to })
        }
        "promote" => {
            let position = parse_queue_position(words.next(), "usage: /queue promote <number>")?;
            if words.next().is_some() {
                return Err("usage: /queue promote <number>".to_string());
            }
            Ok(QueueCommand::Promote(position))
        }
        "clear" if words.next().is_none() => Ok(QueueCommand::Clear),
        // Preserve the original `/queue <prompt>` shorthand — but a single
        // word one edit away from a subaction is far more likely a typo than
        // a deliberate one-word prompt, and silently queueing "lst" as a
        // prompt would be worse than pausing to ask. Multi-word input is
        // always a prompt.
        word => {
            if words.next().is_none() {
                if let Some(suggestion) = closest_queue_subcommand(word) {
                    return Err(format!(
                        "unknown /queue action '{word}' - did you mean '/queue {suggestion}'? (or /queue add {word} to queue it as a prompt)"
                    ));
                }
            }
            Ok(QueueCommand::Add(arg.to_string()))
        }
    }
}

/// Deliberately stricter than `closest_jobs_subcommand` (distance <= 2, exact
/// mismatch only): `/queue`'s bare-prompt shorthand means a false positive
/// here rejects a legitimate one-word prompt, so only near-certain typos get
/// intercepted.
pub(crate) fn closest_queue_subcommand(name: &str) -> Option<&'static str> {
    const SUBCOMMANDS: &[&str] = &["list", "add", "remove", "edit", "move", "promote", "clear"];
    SUBCOMMANDS
        .iter()
        .map(|&cmd| (edit_distance(name, cmd), cmd))
        .min()
        .filter(|(distance, _)| (1..=2).contains(distance))
        .map(|(_, cmd)| cmd)
}

pub(crate) fn queue_prompt_preview(prompt: &str) -> String {
    let mut preview = terminal_inline(prompt)
        .chars()
        .take(120)
        .collect::<String>();
    if terminal_inline(prompt).chars().count() > 120 {
        preview.push_str("...");
    }
    preview
}

pub(crate) fn execute_queue_command(
    queue: &mut prompt_queue::PromptQueue,
    command: QueueCommand,
) -> Result<String, String> {
    match command {
        QueueCommand::List => {
            let status = queue.status();
            let mut output = kitsu_render::deck_header(&format!(
                "prompt queue — {} / {} prompts",
                status.queued, status.capacity
            ));
            for (index, prompt) in queue.iter().enumerate() {
                output.push_str(&format!(
                    "\n  [{}] {}",
                    index + 1,
                    queue_prompt_preview(prompt)
                ));
            }
            Ok(output)
        }
        QueueCommand::AddCopies { prompt, count } => {
            let mut queued = 0usize;
            for _ in 0..count {
                if queue.enqueue(prompt.clone()).is_err() {
                    break;
                }
                queued += 1;
            }
            if queued == 0 {
                Err(format!(
                    "queue is at capacity {}; finish or remove a queued turn and try again",
                    queue.status().capacity
                ))
            } else if queued < count {
                Ok(format!(
                    "queued {queued} of {count} copies (queue reached capacity {}); they run in sequence after the current turn",
                    queue.status().capacity
                ))
            } else {
                Ok(format!(
                    "queued {count} copies; they run in sequence after the current turn — /queue to inspect, /queue clear to abort"
                ))
            }
        }
        QueueCommand::Add(prompt) => match queue.enqueue(prompt) {
            Ok(status) => Ok(status.to_string()),
            Err(error) => Err(format!(
                    "queue refused a {}-character prompt at capacity {}; finish or remove a queued turn and try again",
                    error.prompt().chars().count(),
                    error.capacity()
                )),
        },
        QueueCommand::Remove(index) => match queue.remove(index) {
            Some(prompt) => Ok(format!(
                "removed queued prompt [{}]: {}",
                index + 1,
                queue_prompt_preview(&prompt)
            )),
            None => Err(format!("queue position {} is out of range", index + 1)),
        },
        QueueCommand::Edit(index, replacement) => match queue.edit(index, replacement) {
            Ok(_) => Ok(format!("edited queued prompt [{}]", index + 1)),
            Err(error) => Err(format!(
                    "{error}; {}-character replacement was not queued",
                    error.replacement().chars().count()
                )),
        },
        QueueCommand::Move { from, to } => match queue.reorder(from, to) {
            Ok(()) => Ok(format!(
                "moved queued prompt [{}] -> [{}]",
                from + 1,
                to + 1
            )),
            Err(error) => Err(error.to_string()),
        },
        QueueCommand::Promote(index) => match queue.promote(index) {
            Ok(()) => Ok(format!("promoted queued prompt [{}] to run next", index + 1)),
            Err(error) => Err(error.to_string()),
        },
        QueueCommand::Clear => {
            let removed = queue.clear();
            Ok(format!("cleared {} queued prompt(s)", removed.len()))
        }
    }
}

pub(crate) fn apply_queue_command(
    queue: &mut prompt_queue::PromptQueue,
    command: QueueCommand,
) -> bool {
    match execute_queue_command(queue, command) {
        Ok(message) => {
            eprintln!("~ {message}");
            true
        }
        Err(error) => {
            eprintln!("{error}");
            false
        }
    }
}

pub(crate) fn edit_distance(left: &str, right: &str) -> usize {
    let mut previous: Vec<usize> = (0..=right.len()).collect();
    for (left_index, left_byte) in left.bytes().enumerate() {
        let mut current = vec![left_index + 1];
        for (right_index, right_byte) in right.bytes().enumerate() {
            let replace = previous[right_index] + usize::from(left_byte != right_byte);
            let insert = current[right_index] + 1;
            let delete = previous[right_index + 1] + 1;
            current.push(replace.min(insert).min(delete));
        }
        previous = current;
    }
    previous[right.len()]
}

pub(crate) fn closest_slash_command(name: &str) -> Option<&'static str> {
    let prefix: Vec<_> = matching_slash_commands(name);
    if prefix.len() == 1 {
        return Some(prefix[0].name);
    }
    SLASH_COMMANDS
        .iter()
        .map(|spec| (edit_distance(name, spec.name), spec.name))
        .min()
        .filter(|(distance, _)| *distance <= 3)
        .map(|(_, name)| name)
}

pub(crate) fn closest_jobs_subcommand(name: &str) -> Option<&'static str> {
    const SUBCOMMANDS: &[&str] = &[
        "list",
        "view",
        "show",
        "explain",
        "steer",
        "close-steering",
        "stop",
        "stop-all",
        "prune",
        "reply",
    ];
    SUBCOMMANDS
        .iter()
        .map(|&cmd| (edit_distance(name, cmd), cmd))
        .min()
        .filter(|(distance, _)| *distance <= 3)
        .map(|(_, cmd)| cmd)
}

/// `/help keys`: the agent view's bindings table as text (card **T4-S10**).
///
/// One row per binding, laid out by the same `help_key_row` the overlay's
/// help frame paints, so the two readers cannot print one table two ways.
/// The header is prose and carries no key label, so a reader matching rows by
/// their key column does not mistake it for a binding.
pub(crate) fn agent_view_keys_help() -> String {
    let mut out = String::from("the agent view's keys (/jobs view, or ctrl+x a):\n");
    for binding in kitsu_tui::keys::BINDINGS {
        out.push_str(&kitsu_tui::agents_view::help_key_row(binding));
        out.push('\n');
    }
    out.push_str("  the composer's own chords are listed by /help tui.");
    out
}

/// `/help <command>`: syntax, explanation, and aliases for one command.
/// Aliases resolve to their parent; unknown names get a nearest-help nudge.
pub(crate) fn slash_command_help(query: &str) -> String {
    let query = query.trim_start_matches('/');
    // Card **T4-S10**: `/help keys` is not a new slash command but an
    // argument to this one, so there is no registry entry without an arm
    // (**F57**) -- `/help`'s arm already feeds any argument here. It prints
    // `kitsu_tui::keys::BINDINGS`, the same table and the same wording the
    // agent view's help overlay paints, in the overlay's order.
    if query == "keys" {
        return agent_view_keys_help();
    }
    let Some(spec) = SLASH_COMMANDS.iter().find(|spec| spec.name == query) else {
        return format!(
            "no command named /{} — /help lists every command",
            terminal_inline(query)
        );
    };
    // An alias explains its parent (and says so).
    let (spec, alias_note) = match spec.alias_of {
        Some(parent) => (
            SLASH_COMMANDS
                .iter()
                .find(|candidate| candidate.name == parent)
                .unwrap_or(spec),
            Some(format!("/{} is an alias of /{parent}.", spec.name)),
        ),
        None => (spec, None),
    };
    let mut out = spec.syntax();
    out.push_str(
        "
  ",
    );
    out.push_str(spec.description);
    let details = spec.details.trim();
    if !details.is_empty() {
        out.push_str(
            "
  ",
        );
        out.push_str(details);
    }
    let aliases: Vec<&str> = SLASH_COMMANDS
        .iter()
        .filter(|candidate| candidate.alias_of == Some(spec.name))
        .map(|candidate| candidate.name)
        .collect();
    if !aliases.is_empty() {
        out.push_str(&format!(
            "
  aliases: /{}",
            aliases.join(", /")
        ));
    }
    if let Some(note) = alias_note {
        out.push_str(&format!(
            "
  {note}"
        ));
    }
    out
}

pub(crate) fn slash_syntax(name: &str) -> String {
    SLASH_COMMANDS
        .iter()
        .find(|spec| spec.name == name)
        .map(|spec| spec.syntax())
        .unwrap_or_else(|| format!("/{name}"))
}

pub(crate) fn skill_catalog_payload(
    skillset: &skills::SkillSet,
    filter: &str,
) -> serde_json::Value {
    let filter_lower = filter.trim().to_lowercase();
    let entries = skillset
        .names()
        .into_iter()
        .filter_map(|name| skillset.get(name))
        .filter(|skill| {
            filter_lower.is_empty()
                || skill.name.to_lowercase().contains(&filter_lower)
                || skill.description.to_lowercase().contains(&filter_lower)
        })
        .map(|skill| {
            let source = skillset.source(&skill.name);
            let duplicates = skillset
                .duplicates()
                .iter()
                .filter(|duplicate| duplicate.name == skill.name)
                .map(|duplicate| {
                    serde_json::json!({
                        "selected_origin": duplicate.selected.origin.as_str(),
                        "selected_manifest": duplicate.selected.manifest,
                        "ignored_origin": duplicate.ignored.origin.as_str(),
                        "ignored_manifest": duplicate.ignored.manifest,
                    })
                })
                .collect::<Vec<_>>();
            let origin = source.map(|source| source.origin.as_str());
            let automatically_loaded = skill.always
                && source.map_or(true, |source| source.origin != skills::SkillOrigin::Project);
            serde_json::json!({
                "name": skill.name,
                "description": skill.description,
                "when_to_use": skill.when_to_use,
                "always": skill.always,
                "automatically_loaded": automatically_loaded,
                "origin": origin,
                "manifest": source.map(|source| &source.manifest),
                "duplicates": duplicates,
            })
        })
        .collect::<Vec<_>>();
    serde_json::json!({
        "kind": "skills",
        "filter": filter,
        "count": entries.len(),
        "skills": entries,
        "source_snapshots": skillset.source_snapshots(),
        "omissions": skillset.omissions(),
    })
}

pub(crate) fn skill_load_payload(
    skillset: &skills::SkillSet,
    skill: &skills::Skill,
    changed: bool,
) -> serde_json::Value {
    let source = skillset.source(&skill.name);
    serde_json::json!({
        "kind": "skill_load",
        "name": skill.name,
        "changed": changed,
        "origin": source.map(|source| source.origin.as_str()),
        "manifest": source.map(|source| &source.manifest),
        "reference_links": skill.links.len(),
    })
}

pub(crate) async fn refresh_session_context_window(
    model: &str,
    provider: &ProviderRouteCurrent<CompiledProvider>,
    session: &SessionInfo,
    context_window: &Rc<Cell<Option<u64>>>,
    context_window_pinned: bool,
    jsonl: bool,
    timeout_secs: u64,
) -> SlashResult {
    if context_window_pinned {
        let source = context_window_provenance(&session.window_provenance)
            .source
            .label();
        if !jsonl {
            eprintln!(
                "context window is pinned by {source}; remove that setting before discovery refresh"
            );
        }
        return SlashResult::Error;
    }
    if !session.window_refresh.enabled() {
        if !jsonl {
            eprintln!(
                "context-window refresh is unavailable because models_path is not configured"
            );
        }
        return SlashResult::Error;
    }
    let cache_key = session.window_refresh.cache_key(model);
    let Some(outcome) = await_cancellable(refresh_cached_context_window_explicit(
        provider,
        model,
        &session.window_refresh.cache_path,
        &cache_key,
    ))
    .await
    else {
        if !jsonl {
            eprintln!("^C (context-window refresh cancelled)");
        }
        return SlashResult::Cancelled;
    };
    let outcome = match outcome {
        Ok(outcome) => outcome,
        Err(error) => {
            return report_slash_provider_error(
                "context-window refresh failed",
                error,
                ProviderOperation::ModelCatalog,
                timeout_secs,
                session.provider_diagnostics,
            );
        }
    };
    let (window, cache_written) = match outcome {
        ContextWindowRefreshOutcome::Cached(window) => (window, true),
        ContextWindowRefreshOutcome::DiscoveredButNotCached(window) => (window, false),
        ContextWindowRefreshOutcome::NoMetadata => {
            session
                .window_provenance
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .discovery = ContextWindowDiscoveryState::FinishedWithoutMetadata;
            if !jsonl {
                eprintln!(
                    "context-window refresh returned no usable metadata (or the advisory request failed); the current window is unchanged"
                );
            }
            return SlashResult::Error;
        }
    };
    context_window.set(Some(window));
    *session
        .window_provenance
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) =
        ContextWindowProvenance::refreshed(cache_written);
    if !jsonl {
        eprintln!(
            "~ refreshed context window for {}: {window} tokens ({})",
            terminal_inline(model),
            if cache_written {
                "cached for later runs"
            } else {
                "private cache write failed; current run updated"
            }
        );
    }
    SlashResult::Continue(Some(serde_json::json!({
        "kind": "context_refresh",
        "model": model,
        "window": window,
        "cache_written": cache_written,
    })))
}

/// Handle a slash command (the leading `/` already stripped).
#[allow(clippy::too_many_arguments)]
pub(crate) async fn handle_slash(
    cmd: &str,
    messages: &mut Vec<ChatMessage>,
    model: &mut String,
    provider: &ProviderRouteCurrent<CompiledProvider>,
    tools: &Registry,
    cli: &Cli,
    perms: &Permissions,
    journal: &kitsu_tools::builtins::fs::WriteJournal,
    session: &SessionInfo,
    skillset: &skills::SkillSet,
    context_window: &Rc<Cell<Option<u64>>>,
    context_window_pinned: bool,
    state: &mut kitsu_core::AgentSessionState,
    registry: &Arc<subagent::AgentRegistry>,
    runtime_log: Option<&kitsu_core::AgentEventLog>,
    // The composer's session binding: the one home of the background agent
    // this composer is routed to. Two arms need it — `detach` clears it
    // (**F77**) and `jobs view` sets it (**F79**) — and it is a `&mut`
    // rather than a copy so the answer and the write are the same event.
    attached: &mut Option<u64>,
    // Card **T4-S7**: handed on to `/jobs view`, which is one of the agent
    // view's two routes. `None` from a carrier that never built one — the
    // scripted body cannot open the overlay at all.
    view_dispatcher: Option<&subagent::ViewDispatcher>,
) -> SlashResult {
    let mut parts = cmd.splitn(2, char::is_whitespace);
    let name = parts.next().unwrap_or("");
    let arg = parts.next().unwrap_or("").trim();

    if !arg.is_empty()
        && matches!(
            name,
            "clear"
                | "checkpoints"
                | "compact"
                | "cost"
                | "new"
                | "profiles"
                | "retry"
                | "status"
                | "tools"
        )
    {
        eprintln!("usage: {}", slash_syntax(name));
        return SlashResult::Error;
    }

    match name {
        "quit" | "exit" => {
            let cleanup = match arg {
                "" => kitsu_core::RunCleanupState::Joined,
                "wait" => {
                    await_background_agents(
                        registry,
                        "exit requested with wait while background jobs are running",
                    )
                    .await
                }
                "cancel" => {
                    let cancelled = registry.cancel_all().await;
                    let cleanup = terminal_cleanup_state(&cancelled);
                    eprintln!(
                        "~ cancelled and joined {} background job(s) before exit",
                        cancelled.len()
                    );
                    cleanup
                }
                _ => {
                    eprintln!("usage: /{name} [wait|cancel]");
                    return SlashResult::Error;
                }
            };
            return SlashResult::Quit(cleanup);
        }
        "" | "help" => {
            if !arg.is_empty() {
                let text = slash_command_help(arg);
                eprintln!("{text}");
                return SlashResult::Continue(Some(serde_json::json!({
                    "kind": "help",
                    "command": arg,
                    "text": text,
                })));
            }
            // The JSONL payload always carries the unstyled text; only the
            // live terminal gets bold group headings.
            let text = slash_help(model, false);
            if kitsu_render::stderr_styled() {
                eprintln!("{}", slash_help(model, true));
            } else {
                eprintln!("{text}");
            }
            return SlashResult::Continue(Some(serde_json::json!({
                "kind": "help",
                "model": model,
                "text": text,
                "commands": SLASH_COMMANDS.iter().map(|spec| serde_json::json!({
                    "name": spec.name,
                    "syntax": spec.syntax(),
                    "description": spec.description,
                    "alias_of": spec.alias_of,
                })).collect::<Vec<_>>(),
            })));
        }
        "status" => {
            // The receipt is a secret-free, in-memory observation of the
            // already-bound route. `/status` never sends a provider request.
            let provider_route = match provider.receipt() {
                Ok(receipt) => receipt,
                Err(error) => {
                    eprintln!("cannot inspect current provider route: {error}");
                    return SlashResult::Error;
                }
            };
            let snapshot = collect_current_session_snapshot(
                provider_route.clone(),
                messages,
                session,
                perms,
                state,
            );
            let observation = session_telemetry_render::CurrentStatusObservation::new(
                snapshot,
                kitsu_core::TelemetryValue::authoritative(
                    crate::cli::LONG_VERSION.to_string(),
                    kitsu_core::TelemetryProvenance::RuntimeIdentity,
                ),
                kitsu_core::TelemetryValue::authoritative(
                    session.directory.to_string_lossy().into_owned(),
                    kitsu_core::TelemetryProvenance::RuntimeIdentity,
                ),
            );
            if !cli.jsonl {
                eprintln!(
                    "{}",
                    session_telemetry_render::render_current_status_observation(
                        &observation,
                        current_session_deck_options(cli),
                    )
                );
            }
            let text = session_telemetry_render::render_current_status_observation(
                &observation,
                redirected_current_session_deck_options(),
            );
            return SlashResult::Continue(Some(session_status_payload(
                messages,
                session,
                registry,
                state,
                &provider_route,
                &observation,
                text,
            )));
        }
        "permissions" => {
            let protect_request = match arg.trim() {
                "protect" => Some(""),
                trimmed => trimmed.strip_prefix("protect "),
            };
            if let Some(path) = protect_request {
                let path = path.trim();
                if path.is_empty() {
                    eprintln!("usage: /permissions protect <path> (session-wide read+write guard; irrevocable in-session)");
                    return SlashResult::Error;
                }
                // Order matters for atomicity of the message: attempt both
                // protections; a failure on either leaves an honest error and
                // whatever protection did land (protections only ever narrow
                // authority, so a partial application is safe).
                let read = perms.protect_read_path(path);
                let write = perms.protect_write_path(path);
                match (read, write) {
                    (Ok(canon), Ok(_)) => {
                        eprintln!(
                            "~ protected for this session (read+write, survives all bypass flags): {}",
                            shorten_home(&canon)
                        );
                        let text = format_permissions(perms, session);
                        return SlashResult::Continue(Some(permissions_payload(
                            perms, session, text,
                        )));
                    }
                    // Protections only ever narrow authority, so a half-landed
                    // protection is safe — but it must be disclosed honestly,
                    // not reported as if nothing changed.
                    (Ok(_), Err(error)) => {
                        eprintln!(
                            "partial: READ protection applied, write protection failed for '{path}': {error}"
                        );
                        return SlashResult::Error;
                    }
                    (Err(error), Ok(_)) => {
                        eprintln!(
                            "partial: WRITE protection applied, read protection failed for '{path}': {error}"
                        );
                        return SlashResult::Error;
                    }
                    (Err(error), Err(_)) => {
                        eprintln!("cannot protect '{path}': {error}");
                        return SlashResult::Error;
                    }
                }
            }
            if !arg.trim().is_empty() {
                eprintln!("usage: {}", slash_syntax("permissions"));
                return SlashResult::Error;
            }
            let text = format_permissions(perms, session);
            if !cli.jsonl {
                eprintln!("{text}");
            }
            return SlashResult::Continue(Some(permissions_payload(perms, session, text)));
        }
        "context" => match arg {
            "" => {
                if !cli.jsonl {
                    render_context_command(model, messages, session, state);
                }
                return SlashResult::Continue(Some(context_command_payload(
                    model, messages, session, state,
                )));
            }
            "window" => {
                // This reads the immutable receipt for the already-bound route;
                // neither receipt capture nor the typed session observation has
                // an await point or a provider/account/filesystem operation.
                let provider_route = match provider.receipt() {
                    Ok(receipt) => receipt,
                    Err(error) => {
                        eprintln!("cannot inspect current provider route: {error}");
                        return SlashResult::Error;
                    }
                };
                let snapshot = collect_current_session_snapshot(
                    provider_route,
                    messages,
                    session,
                    perms,
                    state,
                );
                if !cli.jsonl {
                    eprintln!(
                        "{}",
                        session_telemetry_render::render_current_session_context(
                            &snapshot,
                            current_session_deck_options(cli),
                        )
                    );
                }
                // The legacy bare `/context` payload describes its detailed
                // source/compaction view, not this typed focused projection.
                return SlashResult::Continue(None);
            }
            "sources" => {
                let snapshot = collect_context_sources_snapshot(messages, session, state);
                if !cli.jsonl {
                    eprintln!(
                        "{}",
                        session_telemetry_render::render_context_sources(
                            &snapshot,
                            current_session_deck_options(cli),
                        )
                    );
                }
                // This observation deliberately has no public JSONL payload;
                // the script driver retains only the original lifecycle text.
                return SlashResult::Continue(None);
            }
            "refresh" => {
                return refresh_session_context_window(
                    model,
                    provider,
                    session,
                    context_window,
                    context_window_pinned,
                    cli.jsonl,
                    cli.timeout_secs,
                )
                .await;
            }
            _ => {
                eprintln!("usage: {}", slash_syntax("context"));
                return SlashResult::Error;
            }
        },
        "usage" | "cost" => {
            if name == "usage" && !matches!(arg, "" | "session") {
                eprintln!("usage: {}", slash_syntax("usage"));
                return SlashResult::Error;
            }
            // The receipt is a secret-free, in-memory observation of the
            // already-bound route. This command never sends a provider request.
            let provider_route = match provider.receipt() {
                Ok(receipt) => receipt,
                Err(error) => {
                    eprintln!("cannot inspect current provider route: {error}");
                    return SlashResult::Error;
                }
            };
            let snapshot =
                collect_current_session_snapshot(provider_route, messages, session, perms, state);
            if !cli.jsonl {
                eprintln!(
                    "{}",
                    session_telemetry_render::render_current_session_usage(
                        &snapshot,
                        current_session_deck_options(cli),
                    )
                );
            }
            // Both spellings deliberately retain the existing lifecycle-only
            // JSONL contract: the script driver keeps the original command
            // text in command_started/command_finished and receives no result.
            return SlashResult::Continue(None);
        }
        "compact" => {
            let mut cfg = build_agent_config(
                model.clone(),
                cli,
                context_window.get(),
                session.offload_dir.borrow().clone(),
                session.artifact_policy,
            );
            cfg.reasoning_effort = session.reasoning_effort.get();
            cfg.event_log = runtime_log.cloned();
            let messages_before = messages.clone();
            let state_before = state.clone();
            let result = await_cancellable(kitsu_core::compact_context(
                provider, tools, messages, &cfg, state,
            ))
            .await;
            let Some(result) = result else {
                *messages = messages_before;
                rollback_agent_state(state, state_before);
                eprintln!("^C (manual compaction cancelled; context restored)");
                return SlashResult::Cancelled;
            };
            match result {
                Ok(report) => {
                    let payload = compaction_report_payload(&report);
                    if report.changed {
                        if let Some(record) = report.record.as_ref() {
                            eprintln!(
                                "~ compacted context: {} -> {} tokens ({:?}; {} exchange(s) removed, {} tool result(s) elided)",
                                report.tokens_before,
                                report.tokens_after,
                                record.outcome,
                                record.exchanges_removed,
                                record.tool_results_elided
                            );
                        } else {
                            eprintln!(
                                "~ compacted context: {} -> {} tokens",
                                report.tokens_before, report.tokens_after
                            );
                        }
                    } else {
                        eprintln!(
                            "~ nothing eligible to compact ({} tokens; target {})",
                            report.tokens_before, report.target_tokens
                        );
                    }
                    return SlashResult::Continue(Some(payload));
                }
                Err(error) => {
                    let kind = script_failure_kind(&error);
                    *messages = messages_before;
                    rollback_agent_state(state, state_before);
                    let message = chat_error_message_with_diagnostics(
                        error,
                        model,
                        cli.timeout_secs,
                        session.provider_diagnostics,
                    );
                    eprintln!("cannot compact context (context restored): {message}");
                    return if kind == ScriptFailureKind::Provider {
                        SlashResult::ProviderFailure(message)
                    } else {
                        SlashResult::Error
                    };
                }
            }
        }
        "checkpoints" => {
            report_write_checkpoints(journal.drain_and_take_reports().await);
            let checkpoints = journal.checkpoint_summaries();
            eprintln!("{}", format_checkpoint_summaries(&checkpoints));
            return SlashResult::Continue(Some(serde_json::json!({
                "kind": "checkpoints",
                "count": checkpoints.len(),
                "checkpoints": checkpoints.iter().map(|checkpoint| serde_json::json!({
                    "id": checkpoint.id,
                    "label": checkpoint.label,
                    "change_count": checkpoint.change_count,
                    "paths": checkpoint.paths,
                })).collect::<Vec<_>>(),
            })));
        }
        "checkpoint" => {
            let id = match parse_exact_checkpoint_id(arg) {
                Ok(id) => id,
                Err(error) => {
                    eprintln!("usage: /checkpoint <id> ({})", terminal_inline(&error));
                    return SlashResult::Error;
                }
            };
            report_write_checkpoints(journal.drain_and_take_reports().await);
            match journal.checkpoint_preview(id) {
                Some(report) => {
                    eprintln!(
                        "{}",
                        format_write_checkpoint(&report, undo_hint_actionable())
                    );
                    return SlashResult::Continue(Some(serde_json::json!({
                        "kind": "checkpoint",
                        "id": report.id,
                        "label": report.label,
                        "unified_diff": report.unified_diff,
                        "diff_truncated": report.diff_truncated,
                        "changes": report.changes.iter().map(|change| serde_json::json!({
                            "path": change.path,
                            "change": change.change,
                            "before_bytes": change.before_bytes,
                            "after_bytes": change.after_bytes,
                            "first_changed_line": change.first_changed_line,
                        })).collect::<Vec<_>>(),
                    })));
                }
                None => {
                    return SlashResult::UserFailure(format!(
                        "checkpoint #{id} is not available; run /checkpoints to list recoverable checkpoints"
                    ));
                }
            }
        }
        "undo" => {
            let id = match parse_optional_checkpoint_id(arg) {
                Ok(id) => id,
                Err(error) => {
                    eprintln!("usage: /undo [id] ({})", terminal_inline(&error));
                    return SlashResult::Error;
                }
            };
            if registry.running() > 0 {
                eprintln!("undo blocked while background agents are running; use /agents and wait for them to finish");
                return SlashResult::Error;
            }
            report_write_checkpoints(journal.drain_and_take_reports().await);
            match undo_write_checkpoint(journal, perms, id).await {
                Ok(report) => {
                    messages.push(trusted_undo_history_event(&report));
                    eprintln!(
                        "{}",
                        kitsu_render::milestone_line(&format!(
                            "restored checkpoint #{} ({}):",
                            report.id,
                            terminal_inline(&report.label)
                        ))
                    );
                    for change in report.changes {
                        eprintln!("  - {}", shorten_home(&change.path));
                    }
                }
                Err(error) => {
                    eprintln!("undo failed: {}", terminal_inline(&error.to_string()));
                    return SlashResult::Error;
                }
            }
        }
        "diff" => {
            let request = match parse_review_request(arg) {
                Ok(request) => request,
                Err(error) => {
                    eprintln!(
                        "usage: /diff {REVIEW_SCOPE_USAGE} ({})",
                        terminal_inline(&error)
                    );
                    return SlashResult::Error;
                }
            };
            report_write_checkpoints(journal.drain_and_take_reports().await);
            match authorized_workspace_review(session, perms, journal, request) {
                Ok(evidence) => {
                    if !cli.jsonl {
                        eprintln!("~ verified {}", review_target_label(&evidence));
                    }
                    if !cli.jsonl {
                        kitsu_render::render_diff(&evidence.diff);
                    }
                    return SlashResult::Continue(Some(serde_json::json!({
                        "kind": "diff",
                        "target": evidence.target,
                        "resolved_revision": evidence.resolved_revision,
                        "merge_base": evidence.merge_base,
                        "truncated": evidence.truncated,
                        "diff": evidence.diff,
                        // /diff renders host bytes to this terminal only.
                        "evidence_bytes": evidence.diff.len(),
                        "model_disclosed": false,
                    })));
                }
                Err(error) => {
                    eprintln!("cannot show diff: {}", terminal_inline(&error));
                    return SlashResult::Error;
                }
            }
        }
        "review" => {
            let request = match parse_review_request(arg) {
                Ok(request) => request,
                Err(error) => {
                    eprintln!(
                        "usage: /review {REVIEW_SCOPE_USAGE} ({})",
                        terminal_inline(&error)
                    );
                    return SlashResult::Error;
                }
            };
            report_write_checkpoints(journal.drain_and_take_reports().await);
            let requested_path = request.path.clone();
            match authorized_workspace_review(session, perms, journal, request) {
                Ok(evidence) if review_evidence_is_empty(&evidence) => {
                    eprintln!(
                        "{}",
                        empty_review_message(&evidence, requested_path.as_deref(), false)
                    );
                    return SlashResult::Error;
                }
                Ok(evidence) => {
                    if !cli.jsonl {
                        eprintln!("~ verified {}", review_target_label(&evidence));
                        eprintln!("{}", evidence_disclosure_line(&evidence, model));
                    }
                    let disclosure = evidence_disclosure_payload(&evidence);
                    return SlashResult::SubmitPrompt(
                        SubmittedPrompt::review(verified_review_prompt(&evidence))
                            .with_disclosure(disclosure),
                    );
                }
                Err(error) => {
                    eprintln!("cannot review diff: {}", terminal_inline(&error));
                    return SlashResult::Error;
                }
            }
        }
        "new" => return SlashResult::NewSession,
        "fork" => {
            return SlashResult::ForkSession((!arg.is_empty()).then(|| arg.to_string()));
        }
        "export" => {
            let mut args = arg.split_whitespace();
            let Some(destination) = args.next() else {
                eprintln!("usage: /export <file> [markdown|json]");
                return SlashResult::Error;
            };
            let format = match args.next() {
                Some("markdown" | "md") => session::SessionExportFormat::Markdown,
                Some("json") => session::SessionExportFormat::Json,
                None => {
                    let inferred = inferred_export_format(std::path::Path::new(destination));
                    if inferred == session::SessionExportFormat::Json {
                        eprintln!("~ inferred json format from the .json destination");
                    }
                    inferred
                }
                Some(other) => {
                    eprintln!("unknown export format '{other}'; use markdown or json");
                    return SlashResult::Error;
                }
            };
            if args.next().is_some() {
                eprintln!("usage: /export <file> [markdown|json]");
                return SlashResult::Error;
            }
            return SlashResult::ExportSession {
                destination: std::path::PathBuf::from(destination),
                format,
            };
        }
        "archive" | "restore" | "delete" if arg.is_empty() => {
            eprintln!("usage: /{name} <exact-id>");
            return SlashResult::Error;
        }
        "archive" => return SlashResult::ArchiveSession(arg.to_string()),
        "restore" => return SlashResult::RestoreSession(arg.to_string()),
        "delete" => return SlashResult::DeleteSession(arg.to_string()),
        "resume" => {
            // Bare /resume on a TTY opens the picker (cycle-13 parity item:
            // claude's bare `--resume` offers a chooser; ids are unmemorable).
            if arg.is_empty() && kitsu_render::stdin_is_tty() {
                return match resume_picker(session) {
                    Some(selector) => SlashResult::ResumeSession(selector),
                    None => SlashResult::Continue(None),
                };
            }
            return SlashResult::ResumeSession(if arg.is_empty() {
                "last".to_string()
            } else {
                arg.to_string()
            });
        }
        "doctor" => {
            if !arg.is_empty() && arg != "online" {
                eprintln!("usage: {}", slash_syntax("doctor"));
                return SlashResult::Error;
            }
            return match run_doctor(cli, arg == "online", false, false).await {
                Ok(()) => SlashResult::Continue(None),
                Err(error) => {
                    eprintln!("{error}");
                    SlashResult::Error
                }
            };
        }
        "loop" => {
            let mut parts = arg.splitn(2, char::is_whitespace);
            let count = parts.next().unwrap_or("").parse::<usize>().ok();
            let prompt = parts.next().unwrap_or("").trim().to_string();
            let Some(count) = count.filter(|count| (2..=20).contains(count)) else {
                eprintln!("usage: {} (count 2-20)", slash_syntax("loop"));
                return SlashResult::Error;
            };
            if prompt.is_empty() || prompt.starts_with('/') {
                eprintln!(
                    "usage: {} — the prompt must be a plain model turn, not a slash command",
                    slash_syntax("loop")
                );
                return SlashResult::Error;
            }
            return SlashResult::QueueAction(QueueCommand::AddCopies { prompt, count });
        }
        "theme" => {
            if arg.is_empty() {
                let active = kitsu_render::theme().name;
                eprintln!("themes (active marked *):");
                for theme in kitsu_render::THEMES {
                    let active_mark = theme.name == active;
                    let row = kitsu_render::theme_row(
                        theme,
                        active_mark,
                        kitsu_render::stderr_styled(),
                        kitsu_render::plain_mode(),
                    );
                    eprintln!("{}", row);
                }
                eprintln!("switch with /theme <name>; persist via `theme = \"<name>\"` in the config or --theme");
                return SlashResult::Continue(None);
            }
            return match kitsu_render::set_theme(arg) {
                Ok(theme) => {
                    eprintln!(
                        "{}",
                        kitsu_render::milestone_line(&format!(
                            "theme: {} — {}",
                            theme.name, theme.description
                        ))
                    );
                    SlashResult::Continue(None)
                }
                Err(error) => {
                    eprintln!("{error}");
                    SlashResult::Error
                }
            };
        }
        "tui" => {
            return match arg {
                // Bare `/tui` reports, and the report doubles as the usage
                // line. The mode comes from the *live* renderer rather than
                // from `--tui` or `[tui] mode`, so it stays right across a
                // switch.
                "" => {
                    eprintln!("tui mode: {} ({TUI_ACCEPTED_MODES})", live_tui_mode_label());
                    SlashResult::Continue(None)
                }
                "classic" => SlashResult::SwitchTui(TuiRendererRequest::Classic),
                "inline" => SlashResult::SwitchTui(TuiRendererRequest::Inline),
                // The third renderer, answered exactly like the other two.
                // This arm used to refuse the name by hand, while the mode had
                // no renderer to select; it has one now, so the refusal is
                // retired rather than left to tell the user something untrue.
                "fullscreen" => SlashResult::SwitchTui(TuiRendererRequest::Fullscreen),
                other => {
                    eprintln!(
                        "unknown tui mode '{}'; {TUI_ACCEPTED_MODES}",
                        terminal_inline(other)
                    );
                    SlashResult::Error
                }
            };
        }
        "queue" => {
            return match parse_queue_command(arg) {
                Ok(command) => SlashResult::QueueAction(command),
                Err(error) => {
                    eprintln!("{error}");
                    SlashResult::Error
                }
            };
        }
        "reasoning" => {
            let selected = match arg {
                "" => session.reasoning_effort.get(),
                "default" => None,
                "none" => Some(kitsu_providers::ReasoningEffort::None),
                "minimal" => Some(kitsu_providers::ReasoningEffort::Minimal),
                "low" => Some(kitsu_providers::ReasoningEffort::Low),
                "medium" => Some(kitsu_providers::ReasoningEffort::Medium),
                "high" => Some(kitsu_providers::ReasoningEffort::High),
                "xhigh" => Some(kitsu_providers::ReasoningEffort::XHigh),
                _ => {
                    eprintln!("usage: {}", slash_syntax("reasoning"));
                    return SlashResult::Error;
                }
            };
            if !arg.is_empty() {
                session.reasoning_effort.set(selected);
                eprintln!(
                    "~ reasoning effort set to {} for subsequent turns",
                    reasoning_effort_label(selected)
                );
            } else {
                eprintln!("reasoning effort: {}", reasoning_effort_label(selected));
            }
            return SlashResult::Continue(Some(serde_json::json!({
                "kind": "reasoning",
                "changed": !arg.is_empty(),
                "effort": reasoning_effort_label(selected),
            })));
        }
        // Card **T4-S6a**: `/detach`, the command requirement A5 names beside
        // `\u{2190}`. A submitted line is an intention with somewhere to put
        // the answer (T2-S5b), so unlike the `Ctrl+X d` chord it is answered
        // rather than silently inert. **F57** is why the arm exists at all:
        // a registered command with no arm falls through to `unknown command`
        // and, mid-turn, is enqueued as a model prompt. **F77**: it answers
        // *and* clears in one call, against the composer's own binding, so
        // the two cannot disagree.
        "detach" => {
            let outcome = apply_detach_command(attached);
            eprintln!("{}", outcome.report());
            return outcome.slash_result();
        }
        "steer" => {
            eprintln!(
                "/steer is accepted only while a response is running; enter `/steer <correction>` as type-ahead during that turn"
            );
            return SlashResult::Error;
        }
        "retry" => return SlashResult::Retry,
        "sessions" => return SlashResult::ListSessions(arg.to_string()),
        "tools" => {
            let (mode, behavior) = permission_mode_label(perms.mode());
            eprintln!(
                "tools ({} enabled; permissions: {mode} - {behavior}):",
                session.tool_names.len()
            );
            for name in &session.tool_names {
                eprintln!("  - {name}");
            }
            if session.tool_names.is_empty() {
                eprintln!(
                    "  none - pre-grant --allow-path, --allow-write, or --allow-read-write for scripted runs"
                );
            }
            return SlashResult::Continue(Some(serde_json::json!({
                "kind": "tools",
                "count": session.tool_names.len(),
                "tools": session.tool_names,
                "permission_mode": mode,
                "permission_behavior": behavior,
            })));
        }
        "artifacts" => {
            return match handle_command_artifacts(
                session.command_artifact_sink.as_ref(),
                cross_run_artifacts(session),
                session.artifact_policy,
                session.artifact_policy_source,
                arg,
            ) {
                Ok(payload) => SlashResult::Continue(Some(payload)),
                Err(()) => SlashResult::Error,
            };
        }
        "profiles" => {
            let text = format_agent_profiles(&session.agent_profiles);
            eprintln!("{text}");
            return SlashResult::Continue(Some(agent_profiles_payload(&session.agent_profiles)));
        }
        "agents" => {
            // Any other argument is forwarded verbatim: /jobs owns the
            // did-you-mean suggester and the usage line.
            let command = if arg.is_empty() {
                "list".to_string()
            } else if arg.parse::<u64>().is_ok() {
                format!("show {arg}")
            } else {
                arg.to_string()
            };
            return match handle_jobs_command_attaching(
                registry,
                journal,
                &command,
                attached,
                view_dispatcher,
            )
            .await
            {
                Ok(mut payload) => {
                    payload["alias"] = serde_json::Value::String("agents".to_string());
                    SlashResult::Continue(Some(payload))
                }
                Err(()) => SlashResult::Error,
            };
        }
        "agent" => {
            return match handle_jobs_command_attaching(
                registry,
                journal,
                arg,
                attached,
                view_dispatcher,
            )
            .await
            {
                Ok(mut payload) => {
                    payload["alias"] = serde_json::Value::String("agent".to_string());
                    SlashResult::Continue(Some(payload))
                }
                Err(()) => SlashResult::Error,
            };
        }
        "jobs" => {
            return match handle_jobs_command_attaching(
                registry,
                journal,
                arg,
                attached,
                view_dispatcher,
            )
            .await
            {
                Ok(payload) => SlashResult::Continue(Some(payload)),
                Err(()) => SlashResult::Error,
            };
        }
        "skills" => {
            if skillset.is_empty() {
                eprintln!(
                    "no skills found — add SKILL.md dirs under .kitsu/skills/ or ~/.kitsu/skills/"
                );
            } else {
                eprintln!("skills:\n{}", skillset.terminal_index(arg));
                eprintln!("load one with /skill <name>, or the model can call the `skill` tool");
            }
            return SlashResult::Continue(Some(skill_catalog_payload(skillset, arg)));
        }
        "skill" => {
            if arg.is_empty() {
                eprintln!("usage: /skill <name> (loads the skill's instructions into context)");
                return SlashResult::Error;
            } else {
                match skillset.get(arg) {
                    Some(s) => {
                        let marker = format!("[Loaded skill: {}]", s.name);
                        if messages
                            .iter()
                            .any(|message| message.content.starts_with(&marker))
                        {
                            eprintln!("~ skill '{}' is already loaded", terminal_inline(&s.name));
                            return SlashResult::Continue(Some(skill_load_payload(
                                skillset, s, false,
                            )));
                        }
                        let loaded = match skillset.load_with_snapshot(&s.name) {
                            Ok(loaded) => loaded,
                            Err(error) => {
                                eprintln!(
                                    "cannot load skill '{}': {}",
                                    terminal_inline(&s.name),
                                    terminal_inline(&error.to_string())
                                );
                                return SlashResult::Error;
                            }
                        };
                        if !s.links.is_empty() {
                            eprintln!("~ {} reference link(s) (not fetched)", s.links.len());
                        }
                        messages.push(loaded_skill_message(&s.name, &loaded));
                        eprintln!("~ loaded skill '{}'", terminal_inline(&s.name));
                        let mut payload = skill_load_payload(skillset, s, true);
                        payload["snapshot"] = serde_json::json!(loaded.snapshot);
                        return SlashResult::Continue(Some(payload));
                    }
                    None => {
                        eprintln!("no skill named '{}' - try /skills", terminal_inline(arg));
                        return SlashResult::Error;
                    }
                }
            }
        }
        "models" => {
            // Interactive inline+tty: open the popup over the composer,
            // pre-filtered by `arg`. A pick delegates to the `/model <id>`
            // switch path (one switch code path); a cancel is a no-op. Scripted/
            // piped/classic/non-tty fall through to the static text list below.
            #[cfg(unix)]
            if tui_bridge::live_mode() == tui_bridge::TuiMode::Inline
                && kitsu_render::stdin_is_tty()
                && kitsu_render::stderr_styled()
            {
                match await_cancellable(list_models_with_runtime_telemetry(
                    provider,
                    runtime_log,
                    cli.timeout_secs.saturating_mul(1_000),
                ))
                .await
                {
                    Some(Ok(Some(models))) => {
                        let catalog = filter_model_catalog(models, "");
                        if let Some(picked) = run_model_picker(&catalog, model, arg.trim()) {
                            // Delegate the pick to the switch path (boxed for
                            // async recursion), so validation, context-window
                            // pinning and replace_model are not duplicated.
                            return Box::pin(handle_slash(
                                &format!("model {picked}"),
                                messages,
                                model,
                                provider,
                                tools,
                                cli,
                                perms,
                                journal,
                                session,
                                skillset,
                                context_window,
                                context_window_pinned,
                                state,
                                registry,
                                runtime_log,
                                attached,
                                view_dispatcher,
                            ))
                            .await;
                        }
                        // Cancelled: the composer is already restored.
                        return SlashResult::Continue(None);
                    }
                    Some(Ok(None)) => {
                        eprintln!(
                            "this gateway does not expose a model list (its models_path is empty)"
                        );
                        return SlashResult::Continue(None);
                    }
                    Some(Err(error)) => {
                        return report_slash_provider_error(
                            "could not list models",
                            error,
                            ProviderOperation::ModelCatalog,
                            cli.timeout_secs,
                            session.provider_diagnostics,
                        );
                    }
                    None => {
                        eprintln!("^C (model catalog request cancelled)");
                        return SlashResult::Cancelled;
                    }
                }
            }
            let Some(result) = await_cancellable(list_models_and_print(
                provider,
                arg,
                model,
                runtime_log,
                cli.timeout_secs.saturating_mul(1_000),
            ))
            .await
            else {
                eprintln!("^C (model catalog request cancelled)");
                return SlashResult::Cancelled;
            };
            return match finish_slash_model_catalog(
                result,
                cli.timeout_secs,
                session.provider_diagnostics,
            ) {
                Ok(models) => SlashResult::Continue(Some(serde_json::json!({
                    "kind": "model_catalog",
                    "filter": arg,
                    "count": models.len(),
                    "models": models,
                }))),
                Err(result) => result,
            };
        }
        "allow" => {
            if arg.is_empty() {
                eprintln!("usage: /allow <path> (grant read access to a directory/file)");
                return SlashResult::Error;
            } else if !session.tool_names.iter().any(|name| name == "fs_read_file") {
                eprintln!(
                    "read tools are disabled in this run - restart with --allow-path <path> (scripted/piped chat cannot add tools after startup)"
                );
                return SlashResult::Error;
            } else {
                match perms.grant_read_root(arg) {
                    Ok(p) => eprintln!("~ read access granted: {}", p.display()),
                    Err(e) => {
                        eprintln!("{e}");
                        return SlashResult::Error;
                    }
                }
            }
        }
        "allow-read-write" => {
            if arg.is_empty() {
                eprintln!("usage: /allow-read-write <path> (grant both read and write access)");
                return SlashResult::Error;
            }
            let read_enabled = session.tool_names.iter().any(|name| name == "fs_read_file");
            let write_enabled = session
                .tool_names
                .iter()
                .any(|name| name == "fs_write_file");
            if !read_enabled || !write_enabled {
                eprintln!(
                    "combined read/write tools are disabled in this run - restart with --allow-read-write <path> (scripted/piped chat cannot add tools after startup)"
                );
                return SlashResult::Error;
            }
            match perms.grant_read_write_root(arg) {
                Ok(p) => eprintln!("~ read/write access granted: {}", p.display()),
                Err(e) => {
                    eprintln!("{e}");
                    return SlashResult::Error;
                }
            }
        }
        "allow-write" => {
            if arg.is_empty() {
                eprintln!("usage: /allow-write <path> (grant write access to a file or directory)");
                return SlashResult::Error;
            } else if !session
                .tool_names
                .iter()
                .any(|name| name == "fs_write_file")
            {
                eprintln!(
                    "write tools are disabled in this run - restart with --allow-write <path> (scripted/piped chat cannot add tools after startup)"
                );
                return SlashResult::Error;
            } else {
                match perms.grant_write_root(arg) {
                    Ok(p) => eprintln!("~ write access granted: {}", p.display()),
                    Err(e) => {
                        eprintln!("{e}");
                        return SlashResult::Error;
                    }
                }
            }
        }
        "revoke-read" => {
            if arg.is_empty() {
                eprintln!("usage: /revoke-read <exact-root>");
                return SlashResult::Error;
            }
            if let Some(error) = dangerous_bypass_revocation_error(perms.mode(), "read") {
                eprintln!("{error}");
                return SlashResult::Error;
            }
            match perms.revoke_read_root(arg) {
                Ok(revoked) => eprintln!(
                    "~ read root revoked for this session ({}) - /permissions shows remaining coverage",
                    revoked.description()
                ),
                Err(e) => {
                    eprintln!("{e}");
                    return SlashResult::Error;
                }
            }
        }
        "revoke-write" => {
            if arg.is_empty() {
                eprintln!("usage: /revoke-write <exact-root>");
                return SlashResult::Error;
            }
            if let Some(error) = dangerous_bypass_revocation_error(perms.mode(), "write") {
                eprintln!("{error}");
                return SlashResult::Error;
            }
            match perms.revoke_write_root(arg) {
                Ok(revoked) => eprintln!(
                    "~ write root revoked for this session ({}) - /permissions shows remaining coverage",
                    revoked.description()
                ),
                Err(e) => {
                    eprintln!("{e}");
                    return SlashResult::Error;
                }
            }
        }
        "auto" => {
            let Some(policy) = session.command_policy.as_ref() else {
                eprintln!("commands are disabled in this run");
                return SlashResult::Error;
            };
            let on = match arg {
                "" => None,
                "on" => Some(true),
                "off" => Some(false),
                _ => {
                    eprintln!("usage: /auto [on|off]");
                    return SlashResult::Error;
                }
            };
            if let Some(on) = on {
                if !policy.set_session_auto(on) {
                    eprintln!(
                        "auto mode applies to prompted commands; this run's command mode is {:?}",
                        policy.mode()
                    );
                    return SlashResult::Error;
                }
            }
            if policy.mode() == kitsu_tools::CommandMode::AutoAllow {
                eprintln!("~ auto mode: on for this run (--dangerously-skip-permissions)");
            } else if policy.session_auto() {
                eprintln!("{AUTO_MODE_ON_NOTICE}");
            } else {
                eprintln!("~ auto mode off: each new command asks before it runs");
            }
        }
        "revoke-command" => {
            let Ok(index) = arg.parse::<usize>() else {
                eprintln!("usage: /revoke-command <number> (see /permissions)");
                return SlashResult::Error;
            };
            let Some(policy) = session.command_policy.as_ref() else {
                eprintln!("commands are disabled in this run");
                return SlashResult::Error;
            };
            let grants = policy.session_grants();
            let Some(grant) = index.checked_sub(1).and_then(|index| grants.get(index)) else {
                eprintln!(
                    "no command grant #{index}; /permissions lists {} active grant(s)",
                    grants.len()
                );
                return SlashResult::Error;
            };
            if policy.revoke_exact_grant(grant) {
                eprintln!(
                    "~ revoked command grant #{index}: {} (cwd {})",
                    terminal_inline(&grant.display_argv()),
                    shorten_home(&grant.cwd),
                );
            } else {
                eprintln!("command grant #{index} changed before it could be revoked; retry");
                return SlashResult::Error;
            }
        }
        "save" => {
            if arg.is_empty() {
                eprintln!("usage: /save <file> (writes the last response; code blocks if present)");
                return SlashResult::Error;
            } else {
                match messages
                    .iter()
                    .rev()
                    .find(|m| matches!(m.role, kitsu_providers::Role::Assistant))
                {
                    Some(m) => {
                        let body = extract_code_or_text(&m.content);
                        match std::fs::write(arg, body.as_bytes()) {
                            Ok(()) => eprintln!("~ saved {} bytes to {arg}", body.len()),
                            Err(e) => {
                                eprintln!("save failed: {e}");
                                return SlashResult::Error;
                            }
                        }
                    }
                    None => {
                        eprintln!("nothing to save yet");
                        return SlashResult::Error;
                    }
                }
            }
        }
        "clear" => {
            // Saving and rotating durable identity is owned by the driver, not
            // this parser: mutating here would make the caller overwrite the
            // only resumable copy under the same session id.
            return SlashResult::ClearSession;
        }
        "model" => {
            // Bare `/model` on an inline tty opens the arrow-scroll picker; the
            // pick becomes the switch target and flows through the same
            // validate + context-window + replace path a typed id does, so
            // there is one switch code path and the picker adds no second one.
            // ponytail: the picker fetches the catalog to populate itself and
            // the switch below re-fetches to validate — a second GET on a rare,
            // interactive action, not worth threading the list through.
            let picked: Option<String> = if arg.is_empty() {
                #[cfg(unix)]
                {
                    match await_cancellable(list_models_with_runtime_telemetry(
                        provider,
                        runtime_log,
                        cli.timeout_secs.saturating_mul(1_000),
                    ))
                    .await
                    {
                        Some(Ok(Some(models))) => {
                            run_model_picker(&filter_model_catalog(models, ""), model, "")
                        }
                        _ => None,
                    }
                }
                #[cfg(not(unix))]
                {
                    None
                }
            } else {
                None
            };
            let arg: &str = match (arg.is_empty(), picked.as_deref()) {
                (true, Some(choice)) => choice,
                (true, None) => {
                    // No pick (cancelled, non-tty, classic/fullscreen, or no
                    // catalog): keep the original usage message.
                    eprintln!(
                        "usage: /model <name> (current: {}); /models to list available",
                        terminal_inline(model)
                    );
                    return SlashResult::Error;
                }
                (false, _) => arg,
            };
            if arg == "list" || arg.starts_with("list ") {
                // Alias: `/model list [filter]` → same as `/models [filter]`.
                let filter = arg.strip_prefix("list").unwrap_or("").trim();
                let Some(result) = await_cancellable(list_models_and_print(
                    provider,
                    filter,
                    model,
                    runtime_log,
                    cli.timeout_secs.saturating_mul(1_000),
                ))
                .await
                else {
                    eprintln!("^C (model catalog request cancelled)");
                    return SlashResult::Cancelled;
                };
                return match finish_slash_model_catalog(
                    result,
                    cli.timeout_secs,
                    session.provider_diagnostics,
                ) {
                    Ok(models) => SlashResult::Continue(Some(serde_json::json!({
                        "kind": "model_catalog",
                        "filter": filter,
                        "count": models.len(),
                        "models": models,
                    }))),
                    Err(result) => result,
                };
            } else {
                let previous_model = model.clone();
                let expected_generation = match provider.bind() {
                    Ok(route) => route.identity().generation(),
                    Err(error) => {
                        return report_slash_provider_error(
                            "could not stage model switch",
                            error.into(),
                            ProviderOperation::ModelCatalog,
                            cli.timeout_secs,
                            session.provider_diagnostics,
                        );
                    }
                };
                let Some(catalog) = await_cancellable(list_models_with_runtime_telemetry(
                    provider,
                    runtime_log,
                    cli.timeout_secs.saturating_mul(1_000),
                ))
                .await
                else {
                    eprintln!("^C (model validation cancelled)");
                    return SlashResult::Cancelled;
                };
                let catalog_validated = match catalog {
                    Ok(Some(models)) => {
                        let models = filter_model_catalog(models, "");
                        // Provider-pin / policy suffixes (HF router:
                        // `model:fastest`, `model:cheapest`, `model:provider`)
                        // are legitimate ids the catalog never lists — the
                        // config's own recipes document them. Validate the
                        // BASE id and accept the suffix (cycle-27 audit #3).
                        let base = arg.split_once(':').map_or(arg, |(base, _)| base);
                        if !models
                            .iter()
                            .any(|candidate| candidate == arg || candidate == base)
                        {
                            eprintln!(
                                "model '{}' is not in the gateway catalog - run /models",
                                terminal_inline(arg)
                            );
                            return SlashResult::Error;
                        }
                        true
                    }
                    Ok(None) => false,
                    Err(error) => {
                        return report_slash_provider_error(
                            "could not validate model",
                            error,
                            ProviderOperation::ModelCatalog,
                            cli.timeout_secs,
                            session.provider_diagnostics,
                        );
                    }
                };
                // An explicit --context-window flag or gateway-profile
                // context_window is a user/config declaration that overrides
                // discovery entirely (see the flag's doc comment) — switching
                // models must not silently replace it with fresh auto-discovery.
                // Only re-resolve when the window was never pinned in the first
                // place: carrying over the OLD model's discovered window (or
                // lack thereof) would otherwise budget against the wrong limit.
                let (next_window, next_provenance, note) = if context_window_pinned {
                    let provenance = context_window_provenance(&session.window_provenance);
                    (
                        context_window.get(),
                        None,
                        format!(
                            " (context window pinned by {}; unchanged)",
                            provenance.source.label()
                        ),
                    )
                } else {
                    let Some(discovered) =
                        await_cancellable(provider.model_context_length(arg)).await
                    else {
                        eprintln!("^C (context-window discovery cancelled)");
                        return SlashResult::Cancelled;
                    };
                    let discovered = match discovered {
                        Ok(discovered) => discovered,
                        Err(error) => {
                            return report_slash_provider_error(
                                "could not discover context window",
                                error,
                                ProviderOperation::ModelCatalog,
                                cli.timeout_secs,
                                session.provider_diagnostics,
                            );
                        }
                    };
                    let note = match discovered {
                        Some(w) => format!(" (context window: {w} tokens)"),
                        None => format!(
                            " (context window not reported; using conservative {DEFAULT_CONTEXT_WINDOW}-token default)"
                        ),
                    };
                    (
                        discovered,
                        Some(ContextWindowProvenance::live_discovery(discovered)),
                        note,
                    )
                };
                if let Err(error) = provider.replace_model(expected_generation, arg) {
                    return report_slash_provider_error(
                        "could not publish model switch",
                        error.into(),
                        ProviderOperation::ModelCatalog,
                        cli.timeout_secs,
                        session.provider_diagnostics,
                    );
                }
                context_window.set(next_window);
                if let Some(provenance) = next_provenance {
                    *session
                        .window_provenance
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = provenance;
                }
                *model = arg.to_string();
                // The meter's anchor is a real `prompt_tokens` count measured
                // under the OLD model's tokenizer; a different model can tokenize
                // the same text differently, so that anchor must not be trusted
                // for the new one. The summarize circuit breaker is likewise
                // model-specific.
                state.meter.invalidate();
                state.compact_failures = 0;
                eprintln!("~ switched to {}{note}", terminal_inline(model));
                let provenance = context_window_provenance(&session.window_provenance);
                return SlashResult::Continue(Some(serde_json::json!({
                    "kind": "model_switch",
                    "changed": previous_model != *model,
                    "previous_model": previous_model,
                    "model": model,
                    "catalog_validated": catalog_validated,
                    "context_window": context_window.get(),
                    "effective_context_window": context_window
                        .get()
                        .unwrap_or(DEFAULT_CONTEXT_WINDOW),
                    "context_window_pinned": context_window_pinned,
                    "context_source": provenance.source.label(),
                    "context_cache": provenance.cache.label(),
                    "context_discovery": provenance.discovery.label(),
                })));
            }
        }
        other => {
            let display = terminal_inline(other);
            let message = if let Some(suggestion) = closest_slash_command(other) {
                format!("unknown command: /{display} - did you mean /{suggestion}?")
            } else {
                format!("unknown command: /{display} (try /help)")
            };
            return SlashResult::UserFailure(message);
        }
    }
    SlashResult::Continue(None)
}

/// For `/save`: if the text contains fenced ``` code blocks, return the
/// concatenation of their contents (so a `.rs`/`.py` file gets just the code);
/// otherwise return the whole text. Useful for the "ask for code, then save it"
/// coding workflow — a lightweight bridge between paste and file generation.
pub(crate) fn extract_code_or_text(text: &str) -> String {
    let mut blocks: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut in_block = false;
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            if in_block {
                blocks.push(std::mem::take(&mut cur));
            }
            in_block = !in_block;
            continue;
        }
        if in_block {
            cur.push_str(line);
            cur.push('\n');
        }
    }
    if blocks.is_empty() {
        text.to_string()
    } else {
        blocks.join("\n")
    }
}
