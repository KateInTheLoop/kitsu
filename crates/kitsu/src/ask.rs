//! One-shot prompt execution.

use crate::cli::Cli;
use crate::process_exit::EXIT_PROVIDER;
use kitsu_core::ArtifactStoragePolicy;
use kitsu_providers::ChatMessage;
use kitsu_tools::{Permissions, Registry};
use std::sync::Arc;

use crate::cli_error::CliError;
use crate::cli_runtime::emit_ask_jsonl_summary_with_local_failures;
use crate::cli_runtime::emit_jsonl;
use crate::cli_runtime::emit_output_schema_receipt;
use crate::cli_runtime::record_process_terminal_facts;
use crate::cli_runtime::record_successful_operation;
use crate::cli_runtime::AssistantDisposition;
use crate::cli_runtime::ProcessTerminalFacts;
use crate::cli_runtime::ToolErrorTotals;
use crate::completion::emit_output_last_message_error;
use crate::completion::write_output_last_message;
use crate::provider_diagnostics::map_agent_error_with_diagnostics;
use crate::provider_diagnostics::ProviderOperation;
use crate::setup::artifact_policy_receipt;
use crate::setup::build_agent_config;
use crate::setup::build_setup;
use crate::setup::preload_skills;
use crate::setup::project_context_disabled;
use crate::skills;
use crate::terminal::typeahead::TurnOutcome;
use crate::turn::agent_error_jsonl_cause;
use crate::turn::deadline_partial_answer;
use crate::turn::emit_partial_deadline_answer;
use crate::turn::finish_agent_trace;
use crate::turn::finish_assistant_disposition;
use crate::turn::live_agent_event_log_capturing;
use crate::turn::run_turn_cancellable;
use crate::turn::strict_failure_cli_error;
use crate::turn::strict_turn_failure;
use crate::turn::warn_context_window_evidence;
use crate::turn::PartialAnswerBuffer;
use crate::turn::ScriptFailureKind;

pub(crate) async fn run_ask(cli: &Cli, prompt: &str) -> Result<(), CliError> {
    record_process_terminal_facts(ProcessTerminalFacts {
        cleanup: kitsu_core::RunCleanupState::Joined,
        persistence: kitsu_core::RunPersistenceState::NotRequested,
    })?;
    let setup = build_setup(cli).await?;
    let provider = Arc::new(setup.provider_route.clone());
    let model = provider
        .bind()
        .map_err(|error| CliError::user(format!("cannot bind current provider route: {error}")))?
        .model()
        .to_string();
    if setup.artifact_policy == ArtifactStoragePolicy::InlineOnly && !cli.jsonl {
        eprintln!(
            "~ artifacts disabled: commands keep bounded previews only; large tool results are not offloaded; existing artifacts are unchanged"
        );
    }
    if let Some(destination) = cli.output_last_message.as_deref() {
        let private_guard =
            Permissions::for_roots(&[], &[]).map_err(|error| CliError::user(error.to_string()))?;
        private_guard
            .protect_read_path(&setup.config_path.to_string_lossy())
            .map_err(|error| CliError::user(format!("cannot protect Kitsu config: {error}")))?;
        private_guard
            .protect_read_path(&setup.state_dir.to_string_lossy())
            .map_err(|error| CliError::user(format!("cannot protect Kitsu state: {error}")))?;
        if private_guard.is_within_protected_read_root(destination) {
            return Err(CliError::user(format!(
                "--output-last-message destination '{}' intersects Kitsu's protected config or state; choose a separate host-owned path",
                destination.display()
            )));
        }
    }

    let mut messages = Vec::new();
    // Preloaded skills (--skill) work even in the tool-less `ask` path: inject the
    // bodies as a leading system message.
    if !cli.skill.is_empty() {
        let no_project = project_context_disabled(cli);
        let implicit_permissions =
            Permissions::for_roots(&[], &[]).map_err(|error| CliError::user(error.to_string()))?;
        implicit_permissions
            .protect_read_path(&setup.config_path.to_string_lossy())
            .map_err(|error| CliError::user(format!("cannot protect Kitsu config: {error}")))?;
        implicit_permissions
            .protect_read_path(&setup.state_dir.to_string_lossy())
            .map_err(|error| CliError::user(format!("cannot protect Kitsu state: {error}")))?;
        let skillset = std::env::current_dir()
            .map(|cwd| skills::SkillSet::discover(&cwd, no_project, &implicit_permissions))
            .unwrap_or_default();
        if let Some(pre) = preload_skills(cli, &skillset) {
            messages.push(ChatMessage::system(format!(
                "Follow these skill instructions:\n\n{pre}"
            )));
        }
    }
    messages.push(ChatMessage::user(prompt));
    let tools = Registry::new();
    let mut agent_cfg = build_agent_config(
        model.clone(),
        cli,
        setup.context_window,
        None,
        setup.artifact_policy,
    );
    let partial_answer: PartialAnswerBuffer = Arc::default();
    let event_log =
        live_agent_event_log_capturing(cli.jsonl, "ask:1", Some(partial_answer.clone()));
    agent_cfg.event_log = Some(event_log.clone());
    if cli.jsonl {
        let provider_route = provider.receipt().map_err(|error| {
            CliError::user(format!("cannot inspect current provider route: {error}"))
        })?;
        emit_jsonl(
            "run_started",
            serde_json::json!({
                "mode": "ask",
                "gateway": setup.gateway_name,
                "model": model.clone(),
                "provider_route": provider_route,
                "artifact_policy": artifact_policy_receipt(
                    setup.artifact_policy,
                    setup.artifact_policy_source,
                    false,
                ),
            }),
        )?;
    }
    emit_output_schema_receipt(cli)?;
    if cli.jsonl {
        emit_jsonl(
            "turn_started",
            serde_json::json!({"turn_id": "ask:1", "input": prompt}),
        )?;
    }

    let mut state = kitsu_core::AgentSessionState::default();
    let outcome =
        run_turn_cancellable(&provider, &tools, &mut messages, &agent_cfg, &mut state).await;
    let assistant = match &outcome {
        TurnOutcome::Completed(Ok(text)) => Some(text.as_str()),
        _ => None,
    };
    finish_agent_trace(cli, "ask:1", &event_log, assistant)?;
    warn_context_window_evidence(cli, Some("ask:1"), &mut state)?;
    match outcome {
        TurnOutcome::Completed(Ok(text)) => {
            if let Some(failure) =
                strict_turn_failure(&state.last_turn, cli.max_output_tokens, &text)
            {
                finish_assistant_disposition(
                    cli,
                    "ask:1",
                    AssistantDisposition::Discarded,
                    Some(&failure.issue),
                )?;
                let mapped = strict_failure_cli_error(&failure);
                if cli.jsonl {
                    emit_jsonl(
                        "error",
                        failure.jsonl_payload("ask:1", &mapped.to_string(), &state.last_turn),
                    )?;
                    emit_jsonl(
                        "turn_finished",
                        serde_json::json!({
                            "turn_id": "ask:1",
                            "status": "error",
                            "tool_errors": state.last_turn.tool_errors,
                            "policy_errors": state.last_turn.policy_errors,
                        }),
                    )?;
                    emit_ask_jsonl_summary_with_local_failures(
                        "error",
                        1,
                        usize::from(failure.kind.is_provider_failure()),
                        usize::from(failure.kind == ScriptFailureKind::OutputSchema),
                        usize::from(failure.kind == ScriptFailureKind::Tool),
                        usize::from(failure.kind == ScriptFailureKind::Policy),
                        ToolErrorTotals::from_turn(&state.last_turn, false),
                    )?;
                }
                return Err(mapped);
            }
            let recovered_tool_errors = ToolErrorTotals::from_turn(&state.last_turn, true);
            finish_assistant_disposition(cli, "ask:1", AssistantDisposition::Committed, None)?;
            record_successful_operation();
            if cli.jsonl {
                emit_jsonl(
                    "turn_finished",
                    serde_json::json!({
                        "turn_id": "ask:1",
                        "status": "ok",
                        "tool_errors": state.last_turn.tool_errors,
                        "policy_errors": state.last_turn.policy_errors,
                    }),
                )?;
            }
            if let Err(mapped) = write_output_last_message(cli, "ask:1", &text) {
                emit_output_last_message_error(cli, "ask:1", &mapped)?;
                if cli.jsonl {
                    emit_ask_jsonl_summary_with_local_failures(
                        "error",
                        1,
                        0,
                        0,
                        0,
                        0,
                        recovered_tool_errors,
                    )?;
                }
                return Err(mapped);
            }
            if cli.jsonl {
                emit_ask_jsonl_summary_with_local_failures(
                    "ok",
                    0,
                    0,
                    0,
                    0,
                    0,
                    recovered_tool_errors,
                )?;
            }
            Ok(())
        }
        TurnOutcome::Completed(Err(error)) => {
            if let Some(partial) = deadline_partial_answer(&error, &partial_answer) {
                emit_partial_deadline_answer(cli, "ask:1", &partial)?;
            } else {
                finish_assistant_disposition(
                    cli,
                    "ask:1",
                    AssistantDisposition::Discarded,
                    Some("the model turn failed before commit"),
                )?;
            }
            let cause = agent_error_jsonl_cause(&error);
            let mapped = map_agent_error_with_diagnostics(
                error,
                ProviderOperation::Chat { model: &model },
                Some(cli.timeout_secs),
                setup.provider_diagnostics,
            );
            if cli.jsonl {
                let mut payload = mapped.jsonl_error_payload();
                payload["turn_id"] = serde_json::json!("ask:1");
                if let Some(cause) = cause {
                    payload["cause"] = serde_json::json!(cause);
                }
                emit_jsonl("error", payload)?;
                emit_jsonl(
                    "turn_finished",
                    serde_json::json!({"turn_id": "ask:1", "status": "error"}),
                )?;
                emit_ask_jsonl_summary_with_local_failures(
                    "error",
                    1,
                    usize::from(mapped.exit_code() == EXIT_PROVIDER),
                    0,
                    0,
                    0,
                    ToolErrorTotals::from_turn(&state.last_turn, false),
                )?;
            }
            Err(mapped)
        }
        TurnOutcome::Cancelled => {
            finish_assistant_disposition(
                cli,
                "ask:1",
                AssistantDisposition::Discarded,
                Some("the model turn was cancelled before commit"),
            )?;
            let mapped = CliError::cancelled("request cancelled by Ctrl+C");
            if cli.jsonl {
                emit_jsonl(
                    "error",
                    serde_json::json!({
                        "turn_id": "ask:1",
                        "kind": "cancelled",
                        "message": mapped.to_string(),
                        "exit_code": mapped.exit_code(),
                    }),
                )?;
                emit_jsonl(
                    "turn_finished",
                    serde_json::json!({"turn_id": "ask:1", "status": "cancelled"}),
                )?;
                emit_ask_jsonl_summary_with_local_failures(
                    "cancelled",
                    1,
                    0,
                    0,
                    0,
                    0,
                    ToolErrorTotals::from_turn(&state.last_turn, false),
                )?;
            }
            Err(mapped)
        }
    }
}
