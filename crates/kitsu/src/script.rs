//! Scripted conversation execution and shutdown.

use crate::cli::Cli;
use crate::process_exit::{EXIT_CANCELLED, EXIT_PROVIDER, EXIT_USER};
use kitsu_providers::compiled::CompiledProvider;
use kitsu_providers::{ChatMessage, ProviderError, ProviderRouteCurrent};
use kitsu_tools::{Permissions, Registry};
use std::cell::Cell;
use std::rc::Rc;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use crate::agent_commands::cancel_agents_and_report_writes;
use crate::agent_commands::emit_script_background_results;
use crate::agent_commands::join_background_agents_at_exit;
use crate::agent_commands::terminal_cleanup_state;
use crate::attachments;
use crate::chat::commands::handle_slash;
use crate::chat::commands::SlashResult;
use crate::chat::commands::SubmittedPrompt;
use crate::chat::commands::TurnToolMode;
use crate::chat::state::clear_session_boundary;
use crate::chat::state::persist_session_required;
use crate::chat::state::ArtifactDirectoryAllocator;
use crate::chat::state::SessionInfo;
use crate::checkpoints::note_rollback_retained_changes;
use crate::checkpoints::report_write_checkpoint;
use crate::checkpoints::report_write_checkpoints;
use crate::checkpoints::RETAINED_FAILED_TURN_CHANGES;
use crate::cli_error::CliError;
use crate::cli_runtime::emit_jsonl;
use crate::cli_runtime::emit_jsonl_summary_with_postcondition_failures;
use crate::cli_runtime::finalize_process_terminal_with_delivery;
use crate::cli_runtime::jsonl_output_failure;
use crate::cli_runtime::jsonl_session_persistence_state;
use crate::cli_runtime::jsonl_summary_outcome;
use crate::cli_runtime::record_process_terminal_facts;
use crate::cli_runtime::run_assistant_disposition;
use crate::cli_runtime::AssistantDisposition;
use crate::cli_runtime::ProcessTerminalFacts;
use crate::cli_runtime::ToolErrorTotals;
use crate::cli_runtime::SCRIPT_ACCEPTED_PARTIAL;
use crate::cli_runtime::SUCCESSFUL_OPERATIONS;
use crate::completion::emit_output_last_message_error;
use crate::completion::emit_required_write_error;
use crate::completion::emit_required_write_evaluation;
use crate::completion::evaluate_required_writes;
use crate::completion::write_output_last_message;
use crate::completion::RequiredWriteContract;
use crate::presentation::terminal_inline;
use crate::provider_diagnostics::map_agent_error_with_diagnostics;
use crate::provider_diagnostics::ProviderOperation;
use crate::session;
use crate::session_catalog::session_catalog_payload;
use crate::setup::agent_join_timeout;
use crate::setup::build_agent_config;
use crate::setup::ChatSetup;
use crate::skills;
use crate::subagent;
use crate::terminal::typeahead::TurnOutcome;
use crate::turn::agent_error_jsonl_cause;
use crate::turn::await_cancellable;
use crate::turn::finish_agent_trace;
use crate::turn::finish_assistant_disposition;
use crate::turn::live_agent_event_log;
use crate::turn::live_command_event_log;
use crate::turn::read_text_file_cancellable;
use crate::turn::rollback_agent_state;
use crate::turn::run_turn_cancellable;
use crate::turn::script_failure_kind;
use crate::turn::strict_turn_failure;
use crate::turn::transcript_line;
use crate::turn::warn_context_window_evidence;
use crate::turn::ScriptFailureKind;

#[derive(Default)]
pub(crate) struct ScriptRunStatus {
    /// Run-wide tallies mirrored from the shared `RunTallies` for the
    /// end-of-run receipt.
    pub(crate) file_reads: u64,
    pub(crate) denied_calls: u64,
    pub(crate) failed_calls: u64,
    /// User turns and slash commands that completed successfully.
    /// `--allow-turn-errors` accepts PARTIAL success; a run where no operation
    /// succeeded stays an error.
    pub(crate) successful_operations: usize,
    pub(crate) failures: usize,
    pub(crate) provider_failures: usize,
    pub(crate) output_schema_failures: usize,
    pub(crate) postcondition_failures: usize,
    pub(crate) tool_failures: usize,
    pub(crate) policy_failures: usize,
    pub(crate) tool_error_totals: ToolErrorTotals,
    /// Last assistant payload whose turn received a committed disposition.
    /// Failed, cancelled, blank, length-limited, and committed-partial turns
    /// never replace this candidate.
    pub(crate) last_committed_assistant: Option<(String, String)>,
    /// True when the operation-specific JSONL error frame was successfully
    /// written before the body returned its primary error.
    pub(crate) primary_error_framed: bool,
}

impl ScriptRunStatus {
    pub(crate) fn record_successful_operation(&mut self) {
        self.successful_operations += 1;
    }

    pub(crate) fn record_turn_failure(&mut self, kind: ScriptFailureKind) {
        self.failures += 1;
        self.provider_failures += usize::from(kind.is_provider_failure());
        self.output_schema_failures += usize::from(kind == ScriptFailureKind::OutputSchema);
        self.tool_failures += usize::from(kind == ScriptFailureKind::Tool);
        self.policy_failures += usize::from(kind == ScriptFailureKind::Policy);
    }

    pub(crate) fn record_turn_errors(
        &mut self,
        turn_status: &kitsu_core::AgentTurnStatus,
        recovered: bool,
    ) {
        self.tool_error_totals.record_turn(turn_status, recovered);
    }
}

pub(crate) fn failure_count_phrase(failures: usize) -> String {
    if failures == 1 {
        "1 failure".to_string()
    } else {
        format!("{failures} failures")
    }
}

/// The single shutdown boundary for scripted chat. Every body exit reaches
/// this function, including parse/persistence errors, cancellation, and a
/// terminal JSONL sink. Detached tasks are JOINED (bounded by
/// `agent_join_timeout`) before their unobserved terminal results are surfaced;
/// only work that outlives that bound is cancelled, and it is reported as a
/// shutdown-policy cancellation rather than as a rolled-back turn. When JSONL
/// remains writable, the run-level error (if needed) and final summary follow
/// those results, making `final_summary` the last event.
/// One-line end-of-run receipt for non-interactive chat runs (cycle-11 audit
/// gap #1). A multi-minute run that edits nothing and exits 0 must SAY so —
/// silence reading as success cost a full audit run before anyone noticed the
/// task was a no-op. Human stderr only: JSONL consumers already receive
/// `turn_finished` and `run_summary` frames.
#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_run_receipt(
    journal: &kitsu_tools::builtins::fs::WriteJournal,
    registry: &Arc<subagent::AgentRegistry>,
    session_id: Option<&str>,
    successful_operations: usize,
    failures: usize,
    file_reads: u64,
    denied_calls: u64,
    failed_calls: u64,
) {
    let checkpoints = journal.checkpoint_summaries();
    let files: std::collections::BTreeSet<&std::path::PathBuf> = checkpoints
        .iter()
        .flat_map(|checkpoint| checkpoint.paths.iter())
        .collect();
    let changes: usize = checkpoints
        .iter()
        .map(|checkpoint| checkpoint.change_count)
        .sum();
    let operations = if successful_operations == 1 {
        "1 successful operation".to_string()
    } else {
        format!("{successful_operations} successful operations")
    };
    let progress = if failures == 0 {
        operations
    } else {
        format!("{operations} / {}", failure_count_phrase(failures))
    };
    let agent_progress = registry.progress_snapshot();
    let mut agents = String::new();
    if agent_progress.total > 0 {
        // Zero-count qualifiers stay silent: "0 cancelled" on a clean exit
        // reads as an incident that never happened.
        agents = format!(", {} background agent(s)", agent_progress.total);
        if agent_progress.failed > 0 {
            agents.push_str(&format!(" ({} failed)", agent_progress.failed));
        }
        if agent_progress.cancelled > 0 {
            agents.push_str(&format!(" ({} cancelled)", agent_progress.cancelled));
        }
    }
    // Reads render unconditionally: "1 file written, 0 files read" is the
    // visible signature of a model summarizing sources it never opened
    // (cycle-16 M1). Denied/failed calls render only when non-zero — but
    // then always (cycle-19 #5: "1 turn(s) ok" was hiding denials).
    let mut trouble = String::new();
    if denied_calls > 0 {
        trouble.push_str(&format!(", {denied_calls} denied tool call(s)"));
    }
    let plain_failures = failed_calls.saturating_sub(denied_calls);
    if plain_failures > 0 {
        trouble.push_str(&format!(", {plain_failures} failed tool call(s)"));
    }
    eprintln!(
        "~ run summary: {progress}, {file_reads} file(s) read, {} file(s) written ({} change(s)){agents}{trouble}, session {}",
        files.len(),
        changes,
        session_id.unwrap_or("not saved"),
    );
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn finalize_scripted_run(
    registry: &Arc<subagent::AgentRegistry>,
    journal: &kitsu_tools::builtins::fs::WriteJournal,
    perms: &Permissions,
    required_writes: &[RequiredWriteContract],
    session_id: Option<String>,
    cli: &Cli,
    allow_turn_errors: bool,
    agent_join_timeout: std::time::Duration,
    mut status: ScriptRunStatus,
    body_result: Result<(), CliError>,
) -> Result<(), CliError> {
    let jsonl = cli.jsonl;
    SUCCESSFUL_OPERATIONS.store(status.successful_operations, Ordering::Release);
    let body_completed = body_result.is_ok();
    let mut primary_error = body_result.err();
    if let Some(error) = &primary_error {
        status.failures += 1;
        status.provider_failures += usize::from(error.exit_code() == EXIT_PROVIDER);
    }

    // This awaits every task handle (joined or aborted). It must run even after
    // the JSONL emitter becomes terminal and cannot accept another frame. An
    // interrupted run is not made to wait: the user already asked to stop.
    let agent_join_timeout = if primary_error
        .as_ref()
        .is_some_and(|error| error.exit_code() == EXIT_CANCELLED)
    {
        std::time::Duration::ZERO
    } else {
        agent_join_timeout
    };
    let cancelled_at_exit =
        join_background_agents_at_exit(registry, journal, agent_join_timeout, jsonl).await;
    let cleanup_state = terminal_cleanup_state(&cancelled_at_exit);
    if !cancelled_at_exit.is_empty() {
        // Work kitsu had to stop is not successful automation. Work it waited
        // for and collected is reported by its own terminal status below.
        status.failures += cancelled_at_exit.len();
        if !jsonl {
            eprintln!(
                "script ended before {} background agent(s) finished; detached work that had to be cancelled is not successful automation",
                cancelled_at_exit.len()
            );
        }
    }
    // A detached writer may have finished immediately before the running-count
    // snapshot. Its standalone checkpoint still belongs to this script exit.
    report_write_checkpoints(journal.drain_and_take_reports().await);

    let background_results = registry.take_unobserved_terminal_results();
    let mut finalization_output_error =
        emit_script_background_results(&background_results, jsonl).err();
    let unseen_background_failures = background_results
        .iter()
        .filter(|result| matches!(result.status, subagent::AgentStatus::Failed(_)))
        .count();
    if unseen_background_failures > 0 {
        status.failures += unseen_background_failures;
        status.provider_failures += unseen_background_failures;
        if !jsonl {
            eprintln!(
                "script retained {unseen_background_failures} failed background agent result(s); unacknowledged detached failures are not successful automation"
            );
        }
    }

    let required_write_evaluation = evaluate_required_writes(required_writes, perms, journal);
    let required_writes_satisfied = required_write_evaluation.satisfied;
    if !required_writes.is_empty() {
        if let Err(output_error) =
            emit_required_write_evaluation(cli, required_writes, &required_write_evaluation)
        {
            finalization_output_error = Some(output_error);
        }
        if let Some(error) = required_write_evaluation.failure {
            status.failures += 1;
            status.postcondition_failures += 1;
            let framed = emit_required_write_error(cli, &error);
            if let Err(output_error) = framed.as_ref() {
                finalization_output_error = Some(CliError::user(output_error.to_string()));
            }
            if primary_error.is_none() {
                status.primary_error_framed = jsonl && framed.is_ok();
                primary_error = Some(error);
            } else if !jsonl {
                eprintln!(
                    "~ additional completion postcondition failure: {}",
                    terminal_inline(&error.to_string())
                );
            }
        }
    }

    if required_writes_satisfied {
        if let Some((turn_id, text)) = status.last_committed_assistant.as_ref() {
            if let Err(error) = write_output_last_message(cli, turn_id, text) {
                if jsonl_output_failure().is_some() {
                    finalization_output_error = Some(error);
                } else {
                    status.failures += 1;
                    let framed = emit_output_last_message_error(cli, turn_id, &error);
                    if let Err(output_error) = framed.as_ref() {
                        finalization_output_error = Some(CliError::user(output_error.to_string()));
                    }
                    if primary_error.is_none() {
                        status.primary_error_framed = jsonl && framed.is_ok();
                        primary_error = Some(error);
                    } else if !jsonl {
                        eprintln!(
                            "~ additional output failure: {}",
                            terminal_inline(&error.to_string())
                        );
                    }
                }
            }
        }
    }

    let aggregate_failure = primary_error.is_none() && status.failures > 0;
    let accepted_partial =
        aggregate_failure && allow_turn_errors && status.successful_operations > 0;
    if aggregate_failure && !allow_turn_errors {
        let message = format!(
            "script failed: {} turn(s) or command(s) failed (use --allow-turn-errors to accept partial success)",
            status.failures
        );
        primary_error = Some(if status.provider_failures > 0 {
            CliError::provider(message)
        } else if status.tool_failures > 0 || status.policy_failures > 0 {
            CliError::tool(message)
        } else {
            CliError::user(message)
        });
        // Aggregate turn/command failures already have their own framed events.
        status.primary_error_framed = true;
    } else if aggregate_failure && status.successful_operations == 0 {
        // --allow-turn-errors accepts PARTIAL success. When every turn and
        // command failed there is no success to accept, so a zero exit would
        // mislabel a total failure.
        let message = format!(
            "script failed: all {} turn(s) or command(s) failed (--allow-turn-errors accepts partial success, not total failure)",
            status.failures
        );
        primary_error = Some(if status.provider_failures > 0 {
            CliError::provider(message)
        } else if status.tool_failures > 0 || status.policy_failures > 0 {
            CliError::tool(message)
        } else {
            CliError::user(message)
        });
        status.primary_error_framed = true;
    }

    if accepted_partial && primary_error.is_none() {
        SCRIPT_ACCEPTED_PARTIAL.store(true, Ordering::Release);
    }

    if body_completed && status.failures > 0 && !jsonl {
        eprintln!("script summary: {}", failure_count_phrase(status.failures));
    }
    if !jsonl {
        emit_run_receipt(
            journal,
            registry,
            session_id.as_deref(),
            status.successful_operations,
            status.failures,
            status.file_reads,
            status.denied_calls,
            status.failed_calls,
        );
    }
    // Cycle-10 G1: retained-on-disk effects of rolled-back turns are part of
    // the run's truthful end state, not just of the turn that failed.
    let retained_changes = RETAINED_FAILED_TURN_CHANGES.load(Ordering::Acquire);
    if retained_changes > 0 && !jsonl {
        eprintln!(
            "~ {retained_changes} file change(s) from rolled-back turn(s) remain on disk; review the worktree (e.g. git diff or kitsu review --unstaged) before trusting it"
        );
    }

    let persistence = jsonl_session_persistence_state();
    if !jsonl {
        if let Err(error) = record_process_terminal_facts(ProcessTerminalFacts {
            cleanup: cleanup_state,
            persistence,
        }) {
            if primary_error.is_none() {
                primary_error = Some(error);
            }
        }
    }

    if jsonl {
        let summary_status = if primary_error
            .as_ref()
            .is_some_and(|error| error.exit_code() == EXIT_CANCELLED)
        {
            "cancelled"
        } else if primary_error.is_some() {
            "error"
        } else if status.failures > 0 {
            "partial"
        } else {
            "ok"
        };
        let lifecycle_delivery = (|| -> Result<Result<(), CliError>, CliError> {
            let outcome = jsonl_summary_outcome(
                summary_status,
                status.provider_failures,
                status.tool_failures,
                status.policy_failures,
            )?;
            let assistant = run_assistant_disposition();
            let (published, delivery) = finalize_process_terminal_with_delivery(
                outcome,
                assistant,
                cleanup_state,
                persistence,
                || {
                    // Once an earlier emit fails the emitter retains that
                    // terminal error. Record failed delivery without trying
                    // another frame, but only after cleanup above completed.
                    if let Some(output_error) = finalization_output_error.take() {
                        return Err(output_error);
                    }
                    if let Some(output_error) = jsonl_output_failure() {
                        return Err(output_error);
                    }
                    if let Some(error) = primary_error.as_ref() {
                        if !status.primary_error_framed {
                            emit_jsonl(
                                "error",
                                serde_json::json!({
                                    "kind": error.jsonl_kind(),
                                    "message": error.to_string(),
                                    "exit_code": error.exit_code(),
                                }),
                            )?;
                        }
                    }
                    emit_jsonl_summary_with_postcondition_failures(
                        summary_status,
                        status.failures,
                        status.provider_failures,
                        status.output_schema_failures,
                        status.postcondition_failures,
                        status.tool_failures,
                        status.policy_failures,
                        status.tool_error_totals,
                    )
                },
            )
            .map_err(|error| {
                CliError::user(format!("internal scripted finalization failed: {error}"))
            })?;
            debug_assert_eq!(published.terminal().class(), outcome);
            debug_assert_eq!(published.terminal().assistant(), assistant);
            debug_assert_eq!(published.cleanup(), cleanup_state);
            debug_assert_eq!(published.persistence(), persistence);
            Ok(delivery)
        })();
        finalization_output_error = match lifecycle_delivery {
            Ok(Ok(())) => None,
            Ok(Err(output_error)) | Err(output_error) => Some(output_error),
        };
    }

    // Cleanup/finalization never replaces the semantic error that caused the
    // exit. With no earlier error, a terminal output failure is authoritative.
    match (primary_error, finalization_output_error) {
        (Some(primary), _) => Err(primary),
        (None, Some(output)) => Err(output),
        (None, None) => Ok(()),
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_scripted(
    setup: &ChatSetup,
    provider: &ProviderRouteCurrent<CompiledProvider>,
    tools: &Registry,
    messages: &mut Vec<ChatMessage>,
    model: &mut String,
    cli: &Cli,
    path: &str,
    perms: &Permissions,
    journal: &kitsu_tools::builtins::fs::WriteJournal,
    skillset: &skills::SkillSet,
    session: &SessionInfo,
    context_window: &Rc<Cell<Option<u64>>>,
    live_mw: &subagent::LiveModelWindow,
    registry: &Arc<subagent::AgentRegistry>,
    session_store: &mut Option<session::ActiveSessionStore>,
    artifact_directories: &mut ArtifactDirectoryAllocator,
    artifact_storage_enabled: bool,
    stateless_artifact_base: &std::path::Path,
    allow_turn_errors: bool,
    required_writes: &[RequiredWriteContract],
) -> Result<(), CliError> {
    let mut status = ScriptRunStatus::default();
    let result = run_scripted_body(
        setup,
        provider,
        tools,
        messages,
        model,
        cli,
        path,
        perms,
        journal,
        skillset,
        session,
        context_window,
        live_mw,
        registry,
        session_store,
        artifact_directories,
        artifact_storage_enabled,
        stateless_artifact_base,
        &mut status,
    )
    .await;
    let run_session_id = session.session_id.borrow().clone();
    finalize_scripted_run(
        registry,
        journal,
        perms,
        required_writes,
        run_session_id,
        cli,
        allow_turn_errors,
        agent_join_timeout(cli),
        status,
        result,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_scripted_body(
    setup: &ChatSetup,
    provider: &ProviderRouteCurrent<CompiledProvider>,
    tools: &Registry,
    messages: &mut Vec<ChatMessage>,
    model: &mut String,
    cli: &Cli,
    path: &str,
    perms: &Permissions,
    journal: &kitsu_tools::builtins::fs::WriteJournal,
    skillset: &skills::SkillSet,
    session: &SessionInfo,
    context_window: &Rc<Cell<Option<u64>>>,
    live_mw: &subagent::LiveModelWindow,
    registry: &Arc<subagent::AgentRegistry>,
    session_store: &mut Option<session::ActiveSessionStore>,
    artifact_directories: &mut ArtifactDirectoryAllocator,
    artifact_storage_enabled: bool,
    stateless_artifact_base: &std::path::Path,
    status: &mut ScriptRunStatus,
) -> Result<(), CliError> {
    let content = tokio::time::timeout(
        Duration::from_secs(cli.timeout_secs),
        read_text_file_cancellable(std::path::PathBuf::from(path)),
    )
    .await
    .map_err(|_| {
        CliError::user(format!(
            "script read exceeded the {}s end-to-end deadline: {path}",
            cli.timeout_secs
        ))
    })?
    .ok_or_else(|| CliError::cancelled(format!("script read cancelled: {path}")))?
    .map_err(|e| CliError::user(format!("cannot read script '{path}': {e}")))?;
    // One session state for the whole script — the context-meter anchor and
    // compaction circuit breaker persist across turns, matching interactive chat
    // (a script is a multi-turn session, not a series of independent one-shots).
    let mut state = kitsu_core::AgentSessionState::default();
    let scripted_skill_names: Vec<String> =
        skillset.names().into_iter().map(str::to_string).collect();
    let lines: Vec<&str> = content.lines().collect();
    let mut idx = 0;
    let mut last_failed: Option<SubmittedPrompt> = None;
    macro_rules! stop_if_cancelled {
        ($kind:expr, $line:expr) => {
            if $kind == ScriptFailureKind::JsonlOutput {
                return Err(jsonl_output_failure().unwrap_or_else(|| {
                    CliError::user("JSONL output failed without a retained error")
                }));
            }
            if $kind == ScriptFailureKind::Cancelled {
                status.primary_error_framed = cli.jsonl;
                return Err(CliError::cancelled(format!(
                    "script cancelled at {path}:{}",
                    $line
                )));
            }
        };
    }
    macro_rules! record_turn_failure {
        ($kind:expr) => {{
            let kind = $kind;
            status.record_turn_failure(kind);
        }};
    }
    // Review prompts receive verified evidence directly and deliberately run
    // without the mutation-capable chat registry.
    let no_tools = Registry::new();
    while idx < lines.len() {
        let line_number = idx + 1;
        let trimmed = lines[idx].trim();
        idx += 1;

        if trimmed == "<<<" {
            // Collect a verbatim multi-line block until the closing ">>>".
            let mut block: Vec<&str> = Vec::new();
            while idx < lines.len() && lines[idx].trim() != ">>>" {
                block.push(lines[idx]);
                idx += 1;
            }
            if idx == lines.len() {
                return Err(CliError::user(format!(
                    "{path}:{line_number}: unterminated <<< block (missing >>>)"
                )));
            }
            idx += 1; // consume the closing ">>>"
            let prompt = SubmittedPrompt::full(block.join("\n"));
            let result = run_scripted_turn(
                provider,
                tools,
                perms,
                journal,
                messages,
                model,
                cli,
                &prompt.text,
                &prompt.text,
                session,
                &mut state,
                context_window,
                &scripted_skill_names,
                registry,
                status,
                &format!("{path}:{line_number}"),
            )
            .await;
            match result {
                Ok(()) => {
                    persist_session_required(session_store, session, model, messages)?;
                    status.record_successful_operation();
                    last_failed = None;
                }
                Err(kind) => {
                    stop_if_cancelled!(kind, line_number);
                    record_turn_failure!(kind);
                    last_failed = Some(prompt);
                }
            }
            continue;
        }
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix('/') {
            transcript_line("command", trimmed);
            if cli.jsonl {
                emit_jsonl(
                    "command_started",
                    serde_json::json!({
                        "line": line_number,
                        "command": trimmed,
                    }),
                )?;
            }
            let durable_model_before = model.clone();
            let durable_messages_before = messages.clone();
            let command_name = rest.split_whitespace().next().unwrap_or("");
            let command_runtime_log = (command_name == "compact"
                || (cli.jsonl && matches!(command_name, "models" | "model")))
            .then(|| live_command_event_log(cli.jsonl, line_number, trimmed));
            // Scripted runs can never be attached: the agent view's overlay
            // returns `declined()` unless inline mode, a tty and styled
            // stderr all hold, so nothing on this path reaches
            // `apply_agent_view_exit`. The binding is still real, and local
            // to the command, rather than an argument nobody can satisfy.
            let mut attached_agent: Option<u64> = None;
            let result = handle_slash(
                rest,
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
                setup.context_window_pinned,
                &mut state,
                registry,
                command_runtime_log.as_ref(),
                &mut attached_agent,
                // The scripted body cannot open the overlay at all -- it
                // declines outside inline mode on a tty with styled stderr --
                // so there is nothing for a dispatcher to reach here.
                None,
            )
            .await;
            let finish_command = |status: &str, error_message: Option<&str>, exit_code: i32| {
                if !cli.jsonl {
                    return Ok(());
                }
                if let Some(message) = error_message {
                    emit_jsonl(
                        "error",
                        serde_json::json!({
                            "line": line_number,
                            "command": trimmed,
                            "kind": if exit_code == EXIT_PROVIDER { "provider" } else { "user" },
                            "message": message,
                            "exit_code": exit_code,
                        }),
                    )?;
                }
                emit_jsonl(
                    "command_finished",
                    serde_json::json!({
                        "line": line_number,
                        "command": trimmed,
                        "status": status,
                    }),
                )?;
                Ok::<(), CliError>(())
            };
            macro_rules! persist_command_session {
                () => {
                    if let Err(error) =
                        persist_session_required(session_store, session, model, messages)
                    {
                        let framed = finish_command(
                            "error",
                            Some(&format!("session persistence failed: {error}")),
                            error.exit_code(),
                        );
                        status.primary_error_framed = cli.jsonl && framed.is_ok();
                        return Err(error);
                    }
                };
            }
            // Keep subagents in sync with any `/model` change in the script.
            *live_mw.lock().expect("live model/window") = (model.clone(), context_window.get());
            match result {
                SlashResult::Continue(output) => {
                    if *model != durable_model_before || *messages != durable_messages_before {
                        persist_command_session!();
                    }
                    if let Some(output) = output {
                        if cli.jsonl {
                            emit_jsonl(
                                "command_result",
                                serde_json::json!({
                                    "line": line_number,
                                    "command": trimmed,
                                    "result": output,
                                }),
                            )?;
                        }
                    }
                    status.record_successful_operation();
                    finish_command("ok", None, EXIT_USER)?;
                    continue;
                }
                SlashResult::ClearSession => {
                    let running = registry.running();
                    if running > 0 {
                        status.failures += 1;
                        let message = format!(
                            "clear blocked while {running} background agent(s) are running; use /agents and wait for them to finish"
                        );
                        finish_command("error", Some(&message), EXIT_USER)?;
                        if !cli.jsonl {
                            eprintln!(
                                "{}:{line_number}: {}",
                                terminal_inline(path),
                                terminal_inline(&message)
                            );
                        }
                        continue;
                    }
                    let background_results = registry.take_unobserved_terminal_results();
                    emit_script_background_results(&background_results, cli.jsonl)?;
                    match clear_session_boundary(
                        session_store,
                        session,
                        model,
                        messages,
                        &mut state,
                        registry,
                        journal,
                        artifact_directories,
                        artifact_storage_enabled,
                        stateless_artifact_base,
                    )
                    .await
                    {
                        Ok(receipt) => {
                            if cli.jsonl {
                                emit_jsonl(
                                    "command_result",
                                    serde_json::json!({
                                        "line": line_number,
                                        "command": trimmed,
                                        "result": receipt.payload(),
                                    }),
                                )?;
                            } else {
                                eprintln!(
                                    "{}",
                                    kitsu_render::milestone_line(&receipt.human_summary())
                                );
                                if let Some(warning) = receipt.artifact_warning.as_deref() {
                                    eprintln!(
                                        "warning: private artifact storage disabled after clear: {warning}"
                                    );
                                }
                            }
                            status.record_successful_operation();
                            last_failed = None;
                            finish_command("ok", None, EXIT_USER)?;
                        }
                        Err(error) => {
                            status.failures += 1;
                            finish_command("error", Some(&error), EXIT_USER)?;
                            if !cli.jsonl {
                                eprintln!(
                                    "{}:{line_number}: {}",
                                    terminal_inline(path),
                                    terminal_inline(&error)
                                );
                            }
                        }
                    }
                    continue;
                }
                SlashResult::Cancelled => {
                    if cli.jsonl {
                        emit_jsonl(
                            "error",
                            serde_json::json!({
                                "line": line_number,
                                "command": trimmed,
                                "kind": "cancelled",
                                "message": "command cancelled by Ctrl+C",
                                "exit_code": EXIT_CANCELLED,
                            }),
                        )?;
                    }
                    finish_command("cancelled", None, EXIT_CANCELLED)?;
                    status.primary_error_framed = cli.jsonl;
                    return Err(CliError::cancelled(format!(
                        "script cancelled at {path}:{line_number}"
                    )));
                }
                SlashResult::Quit(_) => {
                    finish_command("ok", None, EXIT_USER)?;
                    break;
                }
                SlashResult::Error => {
                    status.failures += 1;
                    let message = format!("command failed: {trimmed}");
                    finish_command("error", Some(&message), EXIT_USER)?;
                    if !cli.jsonl {
                        eprintln!(
                            "{}:{line_number}: {}",
                            terminal_inline(path),
                            kitsu_render::sanitize_terminal_text(&message)
                        );
                    }
                    continue;
                }
                SlashResult::UserFailure(message) => {
                    status.failures += 1;
                    finish_command("error", Some(&message), EXIT_USER)?;
                    if !cli.jsonl {
                        eprintln!(
                            "{}:{line_number}: {}",
                            terminal_inline(path),
                            terminal_inline(&message)
                        );
                    }
                    continue;
                }
                SlashResult::ProviderFailure(detail) => {
                    status.failures += 1;
                    status.provider_failures += 1;
                    let message = format!("provider command failed: {trimmed}: {detail}");
                    finish_command("error", Some(&message), EXIT_PROVIDER)?;
                    if !cli.jsonl {
                        eprintln!(
                            "{}:{line_number}: {}",
                            terminal_inline(path),
                            kitsu_render::sanitize_terminal_text(&message)
                        );
                    }
                    continue;
                }
                SlashResult::SubmitPrompt(prompt) => {
                    if let Some(disclosure) = prompt.disclosure.as_ref() {
                        if cli.jsonl {
                            emit_jsonl(
                                "command_result",
                                serde_json::json!({
                                    "line": line_number,
                                    "command": trimmed,
                                    "result": disclosure,
                                }),
                            )?;
                        }
                    }
                    let turn_tools = match prompt.tool_mode {
                        TurnToolMode::Full => tools,
                        TurnToolMode::None => &no_tools,
                    };
                    let result = run_scripted_turn(
                        provider,
                        turn_tools,
                        perms,
                        journal,
                        messages,
                        model,
                        cli,
                        &prompt.text,
                        trimmed,
                        session,
                        &mut state,
                        context_window,
                        &scripted_skill_names,
                        registry,
                        status,
                        &format!("{path}:{line_number}"),
                    )
                    .await;
                    match result {
                        Ok(()) => {
                            persist_command_session!();
                            status.record_successful_operation();
                            last_failed = None;
                            finish_command("ok", None, EXIT_USER)?;
                        }
                        Err(kind) => {
                            if kind == ScriptFailureKind::Cancelled {
                                finish_command("cancelled", None, EXIT_CANCELLED)?;
                            }
                            stop_if_cancelled!(kind, line_number);
                            record_turn_failure!(kind);
                            last_failed = Some(prompt);
                            finish_command("error", None, EXIT_USER)?;
                        }
                    }
                    continue;
                }
                SlashResult::Retry => {
                    let Some(prompt) = last_failed.clone() else {
                        status.failures += 1;
                        let message = "nothing to retry";
                        finish_command("error", Some(message), EXIT_USER)?;
                        if !cli.jsonl {
                            eprintln!("{}:{line_number}: {message}", terminal_inline(path));
                        }
                        continue;
                    };
                    let turn_tools = match prompt.tool_mode {
                        TurnToolMode::Full => tools,
                        TurnToolMode::None => &no_tools,
                    };
                    let result = run_scripted_turn(
                        provider,
                        turn_tools,
                        perms,
                        journal,
                        messages,
                        model,
                        cli,
                        &prompt.text,
                        "/retry",
                        session,
                        &mut state,
                        context_window,
                        &scripted_skill_names,
                        registry,
                        status,
                        &format!("{path}:{line_number}"),
                    )
                    .await;
                    match result {
                        Ok(()) => {
                            persist_command_session!();
                            status.record_successful_operation();
                            last_failed = None;
                            finish_command("ok", None, EXIT_USER)?;
                        }
                        Err(kind) => {
                            if kind == ScriptFailureKind::Cancelled {
                                finish_command("cancelled", None, EXIT_CANCELLED)?;
                            }
                            stop_if_cancelled!(kind, line_number);
                            record_turn_failure!(kind);
                            finish_command("error", None, EXIT_USER)?;
                        }
                    }
                    continue;
                }
                SlashResult::ListSessions(filter) => {
                    let current = session.session_id.borrow().clone();
                    match session_catalog_payload(
                        &setup.state_dir.join("sessions"),
                        current.as_deref(),
                        &filter,
                    ) {
                        Ok(output) => {
                            if !cli.jsonl {
                                eprintln!(
                                    "{}",
                                    output["text"].as_str().unwrap_or("no saved sessions")
                                );
                            } else {
                                emit_jsonl(
                                    "command_result",
                                    serde_json::json!({
                                        "line": line_number,
                                        "command": trimmed,
                                        "result": output,
                                    }),
                                )?;
                            }
                            status.record_successful_operation();
                            finish_command("ok", None, EXIT_USER)?;
                        }
                        Err(error) => {
                            status.failures += 1;
                            let message = format!("cannot list sessions: {error}");
                            finish_command("error", Some(&message), EXIT_USER)?;
                            if !cli.jsonl {
                                eprintln!(
                                    "{}:{line_number}: {}",
                                    terminal_inline(path),
                                    terminal_inline(&message)
                                );
                            }
                        }
                    }
                    continue;
                }
                SlashResult::QueueAction(_) => {
                    status.failures += 1;
                    let message =
                        "queue commands require interactive chat; put prompts on later script lines";
                    finish_command("error", Some(message), EXIT_USER)?;
                    if !cli.jsonl {
                        eprintln!("{}:{line_number}: {message}", terminal_inline(path));
                    }
                    continue;
                }
                SlashResult::SwitchTui(_) => {
                    status.failures += 1;
                    let message =
                        "the renderer switch requires interactive chat; set the renderer with --tui or `[tui] mode`";
                    finish_command("error", Some(message), EXIT_USER)?;
                    if !cli.jsonl {
                        eprintln!("{}:{line_number}: {message}", terminal_inline(path));
                    }
                    continue;
                }
                SlashResult::NewSession
                | SlashResult::ResumeSession(_)
                | SlashResult::ForkSession(_)
                | SlashResult::ExportSession { .. }
                | SlashResult::ArchiveSession(_)
                | SlashResult::RestoreSession(_)
                | SlashResult::DeleteSession(_) => {
                    status.failures += 1;
                    let message = "session commands require interactive chat";
                    finish_command("error", Some(message), EXIT_USER)?;
                    if !cli.jsonl {
                        eprintln!("{}:{line_number}: {message}", terminal_inline(path));
                    }
                    continue;
                }
            }
        }
        let prompt = SubmittedPrompt::full(trimmed);
        let result = run_scripted_turn(
            provider,
            tools,
            perms,
            journal,
            messages,
            model,
            cli,
            &prompt.text,
            trimmed,
            session,
            &mut state,
            context_window,
            &scripted_skill_names,
            registry,
            status,
            &format!("{path}:{line_number}"),
        )
        .await;
        // Mirror after every turn (not just at the end) so cancelled or
        // failing scripts still report the reads that actually happened —
        // from the SHARED counter, so delegated subagent reads count too.
        status.file_reads = session.run_tallies.reads.load(Ordering::Acquire);
        status.denied_calls = session.run_tallies.denied.load(Ordering::Acquire);
        status.failed_calls = session.run_tallies.failed.load(Ordering::Acquire);
        match result {
            Ok(()) => {
                persist_session_required(session_store, session, model, messages)?;
                status.record_successful_operation();
                last_failed = None;
            }
            Err(kind) => {
                stop_if_cancelled!(kind, line_number);
                record_turn_failure!(kind);
                last_failed = Some(prompt);
            }
        }
    }
    status.file_reads = session.run_tallies.reads.load(Ordering::Acquire);
    status.denied_calls = session.run_tallies.denied.load(Ordering::Acquire);
    status.failed_calls = session.run_tallies.failed.load(Ordering::Acquire);
    Ok(())
}

/// Send one scripted user turn. Failed turns roll conversation/meter state back
/// transactionally while completed filesystem side effects remain inspectable.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_scripted_turn(
    provider: &ProviderRouteCurrent<CompiledProvider>,
    tools: &Registry,
    perms: &Permissions,
    journal: &kitsu_tools::builtins::fs::WriteJournal,
    messages: &mut Vec<ChatMessage>,
    model: &str,
    cli: &Cli,
    input: &str,
    display_input: &str,
    session: &SessionInfo,
    state: &mut kitsu_core::AgentSessionState,
    context_window: &Rc<Cell<Option<u64>>>,
    skill_names: &[String],
    registry: &Arc<subagent::AgentRegistry>,
    run_status: &mut ScriptRunStatus,
    location: &str,
) -> Result<(), ScriptFailureKind> {
    transcript_line("user", display_input);
    if cli.jsonl {
        emit_jsonl(
            "turn_started",
            serde_json::json!({"turn_id": location, "input": display_input}),
        )
        .map_err(|_| ScriptFailureKind::JsonlOutput)?;
    }
    let expanded = match await_cancellable(attachments::expand(input, perms)).await {
        Some(Ok(expanded)) => expanded,
        Some(Err(error)) => {
            if !cli.jsonl {
                eprintln!(
                    "{}: attachment error: {}",
                    terminal_inline(location),
                    terminal_inline(&error)
                );
            }
            if cli.jsonl {
                emit_jsonl(
                    "error",
                    serde_json::json!({
                        "turn_id": location,
                        "kind": "attachment",
                        "message": error.to_string(),
                        "exit_code": EXIT_USER,
                    }),
                )
                .map_err(|_| ScriptFailureKind::JsonlOutput)?;
                emit_jsonl(
                    "turn_finished",
                    serde_json::json!({"turn_id": location, "status": "error"}),
                )
                .map_err(|_| ScriptFailureKind::JsonlOutput)?;
            }
            return Err(ScriptFailureKind::User);
        }
        None => {
            if cli.jsonl {
                emit_jsonl(
                    "error",
                    serde_json::json!({
                        "turn_id": location,
                        "kind": "cancelled",
                        "message": "attachment preflight cancelled by Ctrl+C",
                        "exit_code": EXIT_CANCELLED,
                    }),
                )
                .map_err(|_| ScriptFailureKind::JsonlOutput)?;
                emit_jsonl(
                    "turn_finished",
                    serde_json::json!({"turn_id": location, "status": "cancelled"}),
                )
                .map_err(|_| ScriptFailureKind::JsonlOutput)?;
            }
            return Err(ScriptFailureKind::Cancelled);
        }
    };
    if !cli.jsonl && !expanded.labels.is_empty() {
        eprintln!("~ attached {} file(s):", expanded.labels.len());
        for label in &expanded.labels {
            eprintln!("  - {}", terminal_inline(label));
        }
    }
    let messages_before = messages.clone();
    let state_before = state.clone();
    let agent_watermark = registry.watermark();
    let journal_turn = journal.begin_attributed_turn(display_input);
    if let Some(previous) = journal_turn.previous {
        report_write_checkpoint(previous);
    }
    messages.push(ChatMessage::user(expanded.text));
    let mut cfg = build_agent_config(
        model.to_string(),
        cli,
        context_window.get(),
        session
            .model_can_read_artifacts
            .then(|| session.offload_dir.borrow().clone())
            .flatten(),
        session.artifact_policy,
    );
    cfg.reasoning_effort = session.reasoning_effort.get();
    cfg.spawn_budget = Some(session.spawn_budget.clone());
    cfg.run_tallies = Some(session.run_tallies.clone());
    cfg.skill_names = skill_names.to_vec();
    cfg.defer_response_format_until_final = session.defer_structured_finalization;
    cfg.structured_finalization_gate = session.structured_finalization_gate.clone();
    let event_log = live_agent_event_log(cli.jsonl, location);
    cfg.event_log = Some(event_log.clone());
    // One scripted turn gets one inference budget. A tool call must not reset
    // `--timeout-secs` for the next provider round: without this outer bound a
    // nominal 45-second automation turn can run for 45 seconds per round.
    let turn_deadline = tokio::time::Instant::now()
        .checked_add(Duration::from_secs(cli.timeout_secs))
        .ok_or(ScriptFailureKind::User)?;
    cfg.turn_deadline = Some(turn_deadline);
    let outcome = match tokio::time::timeout_at(
        turn_deadline,
        journal.scope_turn(
            &journal_turn.token,
            run_turn_cancellable(provider, tools, messages, &cfg, state),
        ),
    )
    .await
    {
        Ok(outcome) => outcome,
        Err(_) => TurnOutcome::Completed(Err(kitsu_core::AgentError::Provider(
            ProviderError::Timeout,
        ))),
    };
    // Keep this turn's verified reports: a failed turn's rollback needs them
    // to brief the next turn about file changes retained on disk (G1).
    let mut turn_reports = journal.drain_and_take_reports().await;
    report_write_checkpoints(turn_reports.clone());
    if let Some(report) = journal.finish_attributed_turn(&journal_turn.token) {
        turn_reports.push(report.clone());
        report_write_checkpoint(report);
    }
    let assistant = match &outcome {
        TurnOutcome::Completed(Ok(text)) => Some(text.as_str()),
        _ => None,
    };
    finish_agent_trace(cli, location, &event_log, assistant)
        .map_err(|_| ScriptFailureKind::JsonlOutput)?;
    warn_context_window_evidence(cli, Some(location), state)
        .map_err(|_| ScriptFailureKind::JsonlOutput)?;
    match outcome {
        TurnOutcome::Completed(Ok(text)) => {
            if let Some(failure) =
                strict_turn_failure(&state.last_turn, cli.max_output_tokens, &text)
            {
                run_status.record_turn_errors(&state.last_turn, false);
                finish_assistant_disposition(
                    cli,
                    location,
                    AssistantDisposition::Discarded,
                    Some(&failure.issue),
                )
                .map_err(|_| ScriptFailureKind::JsonlOutput)?;
                if cli.jsonl {
                    emit_jsonl(
                        "error",
                        failure.jsonl_payload(location, &failure.issue, &state.last_turn),
                    )
                    .map_err(|_| ScriptFailureKind::JsonlOutput)?;
                    emit_jsonl(
                        "turn_finished",
                        serde_json::json!({
                            "turn_id": location,
                            "status": "error",
                            "tool_errors": state.last_turn.tool_errors,
                            "policy_errors": state.last_turn.policy_errors,
                        }),
                    )
                    .map_err(|_| ScriptFailureKind::JsonlOutput)?;
                }
                *messages = messages_before;
                rollback_agent_state(state, state_before);
                let mut retained = turn_reports;
                retained.extend(
                    cancel_agents_and_report_writes(registry, agent_watermark, journal).await,
                );
                if !cli.jsonl {
                    eprintln!(
                        "{}: strict scripted turn failed: {}",
                        terminal_inline(location),
                        failure.issue
                    );
                    eprintln!(
                        "kitsu-context-rollback: incomplete turn removed from conversation state"
                    );
                    eprintln!(
                        "~ failed turn rolled back; completed file changes, if any, remain on disk"
                    );
                }
                note_rollback_retained_changes(
                    cli,
                    Some(location),
                    messages,
                    &retained,
                    tools.is_registered("workspace_diff"),
                )
                .map_err(|_| ScriptFailureKind::JsonlOutput)?;
                Err(failure.kind)
            } else {
                run_status.record_turn_errors(&state.last_turn, true);
                finish_assistant_disposition(cli, location, AssistantDisposition::Committed, None)
                    .map_err(|_| ScriptFailureKind::JsonlOutput)?;
                run_status.last_committed_assistant = Some((location.to_string(), text.clone()));
                if !cli.jsonl && state.last_turn.tool_errors > 0 {
                    eprintln!(
                        "~ {} earlier tool error(s) were recovered within the turn; final state committed",
                        state.last_turn.tool_errors
                    );
                }
                if cli.jsonl {
                    emit_jsonl(
                        "turn_finished",
                        serde_json::json!({
                            "turn_id": location,
                            "status": "ok",
                            "tool_errors": state.last_turn.tool_errors,
                            "policy_errors": state.last_turn.policy_errors,
                        }),
                    )
                    .map_err(|_| ScriptFailureKind::JsonlOutput)?;
                }
                Ok(())
            }
        }
        TurnOutcome::Completed(Err(e)) => {
            run_status.record_turn_errors(&state.last_turn, false);
            finish_assistant_disposition(
                cli,
                location,
                AssistantDisposition::Discarded,
                Some("the model turn failed before commit"),
            )
            .map_err(|_| ScriptFailureKind::JsonlOutput)?;
            let cause = agent_error_jsonl_cause(&e);
            let kind = script_failure_kind(&e);
            let mapped = map_agent_error_with_diagnostics(
                e,
                ProviderOperation::Chat { model },
                Some(cli.timeout_secs),
                session.provider_diagnostics,
            );
            let error_message = mapped.to_string();
            if cli.jsonl {
                let mut payload = mapped.jsonl_error_payload();
                payload["turn_id"] = serde_json::json!(location);
                payload["tool_errors"] = serde_json::json!(state.last_turn.tool_errors);
                payload["policy_errors"] = serde_json::json!(state.last_turn.policy_errors);
                if let Some(cause) = cause {
                    payload["cause"] = serde_json::json!(cause);
                }
                emit_jsonl("error", payload).map_err(|_| ScriptFailureKind::JsonlOutput)?;
                emit_jsonl(
                    "turn_finished",
                    serde_json::json!({
                        "turn_id": location,
                        "status": "error",
                        "tool_errors": state.last_turn.tool_errors,
                        "policy_errors": state.last_turn.policy_errors,
                    }),
                )
                .map_err(|_| ScriptFailureKind::JsonlOutput)?;
            }
            *messages = messages_before;
            rollback_agent_state(state, state_before);
            let mut retained = turn_reports;
            retained
                .extend(cancel_agents_and_report_writes(registry, agent_watermark, journal).await);
            if !cli.jsonl {
                eprintln!("{}: {error_message}", terminal_inline(location));
                eprintln!("kitsu-context-rollback: failed turn removed from conversation state");
                eprintln!(
                    "~ failed turn rolled back; completed file changes, if any, remain on disk"
                );
            }
            note_rollback_retained_changes(
                cli,
                Some(location),
                messages,
                &retained,
                tools.is_registered("workspace_diff"),
            )
            .map_err(|_| ScriptFailureKind::JsonlOutput)?;
            Err(kind)
        }
        TurnOutcome::Cancelled => {
            run_status.record_turn_errors(&state.last_turn, false);
            finish_assistant_disposition(
                cli,
                location,
                AssistantDisposition::Discarded,
                Some("the model turn was cancelled before commit"),
            )
            .map_err(|_| ScriptFailureKind::JsonlOutput)?;
            if cli.jsonl {
                emit_jsonl(
                    "error",
                    serde_json::json!({
                        "turn_id": location,
                        "kind": "cancelled",
                        "message": "request cancelled by Ctrl+C",
                        "exit_code": EXIT_CANCELLED,
                    }),
                )
                .map_err(|_| ScriptFailureKind::JsonlOutput)?;
                emit_jsonl(
                    "turn_finished",
                    serde_json::json!({"turn_id": location, "status": "cancelled"}),
                )
                .map_err(|_| ScriptFailureKind::JsonlOutput)?;
            }
            *messages = messages_before;
            rollback_agent_state(state, state_before);
            let mut retained = turn_reports;
            retained
                .extend(cancel_agents_and_report_writes(registry, agent_watermark, journal).await);
            if !cli.jsonl {
                eprintln!("{}: request cancelled by Ctrl+C", terminal_inline(location));
                eprintln!("kitsu-context-rollback: cancelled turn removed from conversation state");
                eprintln!("~ completed file changes, if any, remain on disk");
            }
            note_rollback_retained_changes(
                cli,
                Some(location),
                messages,
                &retained,
                tools.is_registered("workspace_diff"),
            )
            .map_err(|_| ScriptFailureKind::JsonlOutput)?;
            Err(ScriptFailureKind::Cancelled)
        }
    }
}
