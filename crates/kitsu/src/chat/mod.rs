//! Chat initialization and carrier coordination.

use crate::cli::Cli;
use crate::process_exit::EXIT_PROVIDER;
use kitsu_core::context::estimate_str;
use kitsu_core::ArtifactStoragePolicy;
use kitsu_providers::compiled::CompiledProvider;
use kitsu_providers::{ChatMessage, ProviderError, ProviderRouteHandle};
use kitsu_tools::{builtins, Mode, Permissions, Registry};
use rustyline::ExternalPrinter as _;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::agent_commands::await_background_agents;
use crate::agent_commands::background_agent_notice;
use crate::agent_commands::background_agent_notice_level;
use crate::agent_commands::cancel_agents_and_report_writes;
use crate::agent_commands::cancel_agents_and_report_writes_with_cleanup;
use crate::agent_commands::emit_script_background_results;
use crate::agent_commands::join_background_agents_at_exit;
use crate::agent_commands::surface_unobserved_background_results;
use crate::agent_commands::terminal_cleanup_state;
use crate::agent_hook;
use crate::agent_state_file;
use crate::artifacts;
use crate::attachments;
use crate::chat::commands::apply_queue_command;
use crate::chat::commands::handle_slash;
use crate::chat::commands::restored_messages;
use crate::chat::commands::same_directory;
use crate::chat::commands::SlashResult;
use crate::chat::commands::SubmittedPrompt;
#[cfg(unix)]
use crate::chat::commands::TuiRendererRequest;
use crate::chat::commands::TurnToolMode;
use crate::chat::state::artifact_directory_for_boundary;
use crate::chat::state::clear_session_boundary;
use crate::chat::state::persist_session;
use crate::chat::state::persist_session_required;
use crate::chat::state::persist_started_session;
use crate::chat::state::rotate_artifact_storage;
use crate::chat::state::ArtifactDirectoryAllocator;
use crate::chat::state::CtxEntry;
use crate::chat::state::CtxSourceKind;
use crate::chat::state::OffloadDirGuard;
use crate::chat::state::SessionInfo;
use crate::checkpoints::format_write_checkpoint;
use crate::checkpoints::note_rollback_retained_changes;
use crate::checkpoints::report_write_checkpoint;
use crate::checkpoints::report_write_checkpoints;
use crate::checkpoints::undo_hint_actionable;
use crate::checkpoints::UNDO_HINTS_ACTIONABLE;
use crate::cli_error::CliError;
use crate::cli_runtime::active_output_schema;
use crate::cli_runtime::emit_jsonl;
use crate::cli_runtime::emit_jsonl_summary;
use crate::cli_runtime::emit_jsonl_summary_with_local_failures;
use crate::cli_runtime::emit_jsonl_summary_with_postcondition_failures;
use crate::cli_runtime::emit_output_schema_receipt;
use crate::cli_runtime::jsonl_session_persistence_state;
use crate::cli_runtime::publish_piped_jsonl_terminal;
use crate::cli_runtime::record_human_terminal_facts;
use crate::cli_runtime::record_human_terminal_facts_preserving_error;
use crate::cli_runtime::record_interactive_terminal_facts;
use crate::cli_runtime::record_successful_operation;
use crate::cli_runtime::run_id;
use crate::cli_runtime::set_jsonl_session_receipt;
use crate::cli_runtime::AssistantDisposition;
use crate::cli_runtime::InteractiveRunAggregate;
use crate::cli_runtime::SessionSnapshotState;
use crate::cli_runtime::ToolErrorTotals;
use crate::cli_runtime::PROCESS_INFERENCE_CARRIER;
use crate::command_artifact;
use crate::command_policy::pregrant_error;
use crate::command_policy::reset_command_session;
use crate::command_policy::CommandEnvSpec;
use crate::command_policy::PregrantedCommand;
use crate::command_policy::PregrantedCommandSpec;
use crate::completion::emit_output_last_message_error;
use crate::completion::emit_required_write_armed;
use crate::completion::emit_required_write_error;
use crate::completion::emit_required_write_evaluation;
use crate::completion::evaluate_required_writes;
use crate::completion::prepare_required_writes;
use crate::completion::reject_host_output_command_authority;
use crate::completion::required_write_structured_repair;
use crate::completion::required_write_system_instruction;
use crate::completion::write_output_last_message;
use crate::context_window::ContextWindowProvenance;
use crate::llms;
use crate::management::reject_credential_command_root;
use crate::management::shell_quote;
use crate::presentation::shorten_home;
use crate::presentation::terminal_inline;
use crate::prompt::chat_system_prompt;
use crate::prompt::PromptEnvironment;
use crate::prompt_queue;
use crate::provider_diagnostics::map_agent_error_with_diagnostics;
use crate::provider_diagnostics::map_provider_error_with_diagnostics;
use crate::provider_diagnostics::safe_provider_error_message;
use crate::provider_diagnostics::ProviderOperation;
use crate::review::authorized_workspace_review;
use crate::review::empty_review_message;
use crate::review::evidence_disclosure_line;
use crate::review::review_evidence_is_empty;
use crate::review::review_target_label;
use crate::review::top_level_review_disclosure_event;
use crate::review::verified_review_prompt;
use crate::review::workspace_diff_tool;
use crate::review::ReviewRequest;
use crate::runtime_state::prepare_runtime_state_dir;
use crate::script::emit_run_receipt;
use crate::script::run_scripted;
use crate::session;
use crate::session_catalog::format_sessions;
#[cfg(unix)]
use crate::session_telemetry::refresh_status_usage;
use crate::setup::agent_join_timeout;
use crate::setup::artifact_policy_receipt;
use crate::setup::build_agent_config;
use crate::setup::build_setup;
use crate::setup::can_resolve_offload_pointers;
use crate::setup::cli_reasoning_effort;
use crate::setup::preload_skills;
use crate::setup::project_context_disabled;
use crate::setup::refresh_loaded_skill_messages;
use crate::setup::register_base_tools;
use crate::skills;
use crate::subagent;
use crate::terminal::approval::CliCommandPrompter;
use crate::terminal::approval::CliPrompter;
use crate::terminal::completion::bracketed_paste_enabled;
use crate::terminal::completion::ChatHelper;
use crate::terminal::completion::ComposerCompletion;
#[cfg(unix)]
use crate::terminal::input::read_idle_chat_line;
use crate::terminal::input::ChatReadline;
use crate::terminal::typeahead::TurnOutcome;
use crate::terminal::typeahead::TypeaheadCollector;
use crate::terminal::typeahead::TypeaheadOutcome;
use crate::terminal::typeahead::ASSISTIVE_INPUT_INTERRUPTED;
#[cfg(unix)]
use crate::tui_bridge;
use crate::turn::agent_error_jsonl_cause;
use crate::turn::await_cancellable;
use crate::turn::deadline_partial_answer;
use crate::turn::emit_partial_deadline_answer;
use crate::turn::empty_final_response_issue;
use crate::turn::finish_agent_trace;
use crate::turn::finish_assistant_disposition;
use crate::turn::is_deadline_timeout;
use crate::turn::live_agent_event_log;
use crate::turn::live_agent_event_log_capturing;
use crate::turn::live_command_event_log;
use crate::turn::read_piped_stdin_cancellable;
use crate::turn::rollback_agent_state;
use crate::turn::run_turn_cancellable;
use crate::turn::run_turn_steerable;
use crate::turn::snapshot_partial_answer;
use crate::turn::strict_failure_cli_error;
use crate::turn::strict_turn_failure;
use crate::turn::truncate_to_resumable_tail;
use crate::turn::warn_context_window_evidence;
use crate::turn::InteractiveTurnOutcome;
use crate::turn::PartialAnswerBuffer;
use crate::turn::ScriptFailureKind;

pub(crate) mod commands;
pub(crate) mod state;

pub(crate) const PROMPT_QUEUE_CAPACITY: usize = 8;
pub(crate) const STEERING_CAPACITY: usize = 4;

#[derive(Default)]
pub(crate) struct ChatInvocation<'a> {
    pub(crate) first_prompt: Option<&'a str>,
    pub(crate) no_tools: bool,
    pub(crate) allow_paths: &'a [String],
    pub(crate) allow_write: &'a [String],
    pub(crate) allow_read_write: &'a [String],
    pub(crate) require_writes: &'a [String],
    pub(crate) require_write_sha256: &'a [String],
    pub(crate) enable_commands: bool,
    pub(crate) command_roots: &'a [String],
    pub(crate) command_runtime_roots: &'a [String],
    pub(crate) command_env: &'a [String],
    pub(crate) allow_commands: &'a [String],
    pub(crate) allow_network: bool,
    pub(crate) script: Option<&'a str>,
    pub(crate) resume: Option<&'a str>,
    pub(crate) persist_session: bool,
    pub(crate) no_session: bool,
    pub(crate) session_name: Option<&'a str>,
    pub(crate) allow_turn_errors: bool,
    pub(crate) initial_review: Option<InitialReview>,
}

#[derive(Clone)]
pub(crate) struct InitialReview {
    pub(crate) request: ReviewRequest,
    pub(crate) instructions: Option<String>,
}

/// Whether the launch directory is granted as a read and/or write root.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CwdGrant {
    pub(crate) read: bool,
    pub(crate) write: bool,
}

/// How the launch directory is granted, given `--yes` and whether the run is
/// interactive. Pure so the policy is unit-testable without spawning kitsu
/// (the integration suite can't run on macOS — pre-existing credential-store
/// check). `--yes` means "no prompts, just work": grant the cwd read+write so
/// an automation run can read and write there without hand-assembling
/// `--allow-read-write <dir>` (scoped to the cwd — NOT the global bypass).
/// An interactive session gets the same read+write cwd grant, so editing the
/// current project just works; writes OUTSIDE the cwd still prompt (Mode::Ask).
/// Non-interactive without `--yes` grants nothing (deny-by-default stands).
pub(crate) fn cwd_grant(yes: bool, interactive: bool) -> CwdGrant {
    if yes || interactive {
        // Both a non-prompting `--yes` run and an interactive session grant the
        // launch directory read+write, so working in the current project just
        // works: `--yes` never prompts; interactive (Mode::Ask) still prompts
        // for writes OUTSIDE the cwd but not inside it. Previously interactive
        // granted cwd read-only, which made the model conclude the project was
        // read-only and decline to edit at all rather than attempt a write.
        CwdGrant {
            read: true,
            write: true,
        }
    } else {
        // Non-interactive without --yes stays deny-by-default.
        CwdGrant {
            read: false,
            write: false,
        }
    }
}

pub(crate) async fn run_chat(cli: &Cli, invocation: ChatInvocation<'_>) -> Result<(), CliError> {
    let ChatInvocation {
        first_prompt,
        no_tools,
        allow_paths,
        allow_write,
        allow_read_write,
        require_writes,
        require_write_sha256,
        enable_commands,
        command_roots,
        command_runtime_roots,
        command_env,
        allow_commands,
        allow_network,
        script,
        resume,
        persist_session: session_requested,
        no_session,
        session_name,
        allow_turn_errors,
        initial_review,
    } = invocation;
    let setup = build_setup(cli).await?;
    let provider = Arc::new(setup.provider_route.clone());
    let initial_model = provider
        .bind()
        .map_err(|error| CliError::user(format!("cannot bind current provider route: {error}")))?
        .model()
        .to_string();

    // Permissions gate the fs tools. Interactive TTY chat uses "ask" mode
    // (prompt on demand) with read access to the launch directory by default;
    // scripted/piped runs are deny-by-default. --yes is non-prompting but keeps
    // root confinement. Only the deliberately alarming bypass flag permits an
    // enabled filesystem tool to operate outside current roots.
    let scripted = script.is_some();
    let forced_one_shot = initial_review.is_some();
    let interactive = kitsu_render::stdin_is_tty() && !scripted && !forced_one_shot;
    // Checkpoint blocks always print; the "/undo N" hint is only advertised
    // where a human can actually type it (the interactive REPL).
    UNDO_HINTS_ACTIONABLE.store(interactive, Ordering::Release);
    let durable_session = !no_session
        && (interactive || session_requested || session_name.is_some() || resume.is_some());
    let command_requested = enable_commands
        || !command_roots.is_empty()
        || !command_runtime_roots.is_empty()
        || !allow_commands.is_empty()
        || !command_env.is_empty()
        || allow_network;
    // Interactive developer chat offers Git without exposing a general command
    // runner. Every Git invocation still uses the independent approval policy.
    let default_git = interactive
        && !no_tools
        && std::env::current_dir().is_ok_and(|cwd| {
            reject_credential_command_root(&cwd, &setup.state_dir, "default Git root").is_ok()
        });
    let automatic_artifacts = setup.artifact_policy == ArtifactStoragePolicy::Automatic;
    if cli.jsonl && interactive {
        return Err(CliError::user(
            "--jsonl is an automation mode; use --script, pipe one prompt, or use `ask`"
                .to_string(),
        ));
    }
    if cli.output_last_message.is_some() && interactive {
        return Err(CliError::user(
            "--output-last-message is an automation mode; use --script, pipe one prompt, `ask`, or `review`"
                .to_string(),
        ));
    }
    let has_required_writes = !require_writes.is_empty() || !require_write_sha256.is_empty();
    if has_required_writes && interactive {
        return Err(CliError::user(
            "--require-write and --require-write-sha256 are automation modes; use --script or pipe one prompt"
                .to_string(),
        ));
    }
    if has_required_writes && cli.output_schema.is_some() && cli.max_rounds < 2 {
        return Err(CliError::user(
            "--output-schema with a required-write contract needs --max-rounds 2 or higher: one bounded round is reserved for the tool-free structured final answer"
                .to_string(),
        ));
    }
    if durable_session || (command_requested && automatic_artifacts) {
        prepare_runtime_state_dir(&setup.state_dir).map_err(CliError::user)?;
        // The state dir is being prepared anyway, so reclaim the litter that is
        // provably worthless: empty, orphaned, and held by nobody. Non-empty
        // orphans are never deleted automatically — they may hold the only copy
        // of a command's output — but they stop being invisible.
        if automatic_artifacts {
            let sweep = artifacts::sweep_empty_orphans(&setup.state_dir);
            if let Some(notice) = sweep.notice() {
                if !cli.jsonl {
                    eprintln!("{notice}");
                }
            }
        }
    }
    let sessions_dir = setup.state_dir.join("sessions");
    let resumed_existing = resume.is_some();
    let (mut session_store, resumed) = match (resume, durable_session) {
        (Some(selector), true) => {
            let admitted = session::ActiveSessionStore::resume_bound_current_v1(
                sessions_dir.clone(),
                selector,
            )
            .map_err(|error| CliError::user(format!("cannot resume chat: {error}")))?;
            let (store, saved) = admitted.into_parts();
            (Some(store), Some(saved))
        }
        (Some(selector), false) => (
            None,
            Some(
                session::SessionStore::load(&sessions_dir, selector)
                    .map_err(|error| CliError::user(format!("cannot resume chat: {error}")))?,
            ),
        ),
        (None, true) => (
            Some(
                session::ActiveSessionStore::create(sessions_dir.clone())
                    .map_err(CliError::user)?,
            ),
            None,
        ),
        (None, false) => (None, None),
    };
    if let Some(saved) = resumed.as_ref() {
        let current = std::env::current_dir().map_err(|error| {
            CliError::user(format!("cannot resolve current directory: {error}"))
        })?;
        if !same_directory(&saved.directory, &current) {
            return Err(CliError::user(format!(
                "cannot resume session '{}' from {} while Kitsu is running in {}; change to the saved directory first",
                saved.id,
                shorten_home(std::path::Path::new(&saved.directory)),
                shorten_home(&current)
            )));
        }
    }
    if let Some(requested_name) = session_name {
        let Some(store) = session_store.as_mut() else {
            return Err(CliError::user(
                "--name requires a durable session (drop --no-session)".to_string(),
            ));
        };
        session::valid_session_name(requested_name).map_err(CliError::user)?;
        // A name resolves in `--resume <name>`, so it must map to one session.
        let taken = session::SessionStore::list(&sessions_dir)
            .unwrap_or_default()
            .into_iter()
            .any(|summary| {
                summary.id != store.id() && summary.name.as_deref() == Some(requested_name)
            });
        if taken {
            return Err(CliError::user(format!(
                "another session is already named '{requested_name}' — pick a different --name or rename it via `kitsu sessions rename`"
            )));
        }
        store
            .set_name(Some(requested_name.to_string()))
            .map_err(CliError::user)?;
        eprintln!("~ session name: {}", terminal_inline(requested_name));
    }
    let mode = if cli.dangerously_skip_permissions || cli.dangerously_allow_all_filesystem {
        Mode::AutoAllow
    } else if cli.yes {
        Mode::Scoped
    } else if interactive {
        Mode::Ask
    } else {
        Mode::Deny
    };
    if cli.dangerously_skip_permissions {
        // The unattended bypass must never start silently: name exactly what
        // is off and which guards remain, once, before any tool runs.
        kitsu_render::PromptSafeAsyncNotice::warning(
            "DANGEROUSLY SKIPPING PERMISSIONS: filesystem prompts bypassed for all \
             non-protected paths; developer commands auto-approved inside their roots. \
             Remaining guards: Kitsu config/state protection, command-root confinement \
             + containment, and /permissions protect <path>",
        )
        .write_line(&mut std::io::stderr())
        .ok();
    }
    let mut read_roots: Vec<String> = allow_paths.to_vec();
    read_roots.extend(allow_read_write.iter().cloned());
    let mut write_roots: Vec<String> = allow_write.to_vec();
    write_roots.extend(allow_read_write.iter().cloned());
    if let Ok(cwd) = std::env::current_dir() {
        let cwd = cwd.to_string_lossy().to_string();
        let grant = cwd_grant(cli.yes, interactive);
        if grant.read {
            read_roots.push(cwd.clone());
        }
        if grant.write {
            write_roots.push(cwd);
        }
    }
    let mut perms = Permissions::new(&read_roots, &write_roots, mode)
        .map_err(|e| CliError::user(e.to_string()))?;
    if mode == Mode::Ask {
        perms = perms.with_prompter(Arc::new(CliPrompter));
    }
    let perms = Arc::new(perms);
    perms
        .protect_read_path(&setup.config_path.to_string_lossy())
        .map_err(|error| CliError::user(format!("cannot protect Kitsu config: {error}")))?;
    perms
        .protect_write_path(&setup.config_path.to_string_lossy())
        .map_err(|error| CliError::user(format!("cannot protect Kitsu config: {error}")))?;
    // Reserve the intended location even in a stateless run where it does not
    // exist yet. A broad write root or explicit global bypass must not become
    // permission to mutate Kitsu's state if background discovery creates it.
    perms
        .protect_read_path(&setup.state_dir.to_string_lossy())
        .map_err(|error| CliError::user(format!("cannot protect Kitsu state: {error}")))?;
    perms
        .protect_write_path(&setup.state_dir.to_string_lossy())
        .map_err(|error| CliError::user(format!("cannot protect Kitsu state: {error}")))?;
    if let Some(destination) = cli.output_last_message.as_deref() {
        if perms.is_within_protected_read_root(destination) {
            return Err(CliError::user(format!(
                "--output-last-message destination '{}' intersects Kitsu's protected config or state; choose a separate host-owned path",
                destination.display()
            )));
        }
        // The host installs this file only after model/background work ends.
        // Keep it absent from filesystem-tool authority even when its parent is
        // an allowed write root or the global filesystem bypass is active.
        perms
            .protect_read_path(&destination.to_string_lossy())
            .map_err(|error| {
                CliError::user(format!(
                    "cannot protect --output-last-message destination: {error}"
                ))
            })?;
        perms
            .protect_write_path(&destination.to_string_lossy())
            .map_err(|error| {
                CliError::user(format!(
                    "cannot protect --output-last-message destination: {error}"
                ))
            })?;
    }
    if let Some(schema) = cli.output_schema.as_deref() {
        // The host reads and validates the schema before constructing tools.
        // The model receives only its provider-native value; broad filesystem
        // roots must never turn the source file into model read/write authority.
        perms
            .protect_read_path(&schema.to_string_lossy())
            .map_err(|error| {
                CliError::user(format!("cannot protect --output-schema file: {error}"))
            })?;
        perms
            .protect_write_path(&schema.to_string_lossy())
            .map_err(|error| {
                CliError::user(format!("cannot protect --output-schema file: {error}"))
            })?;
    }

    // In interactive chat the tools are always available (permissions gate access
    // at call time). Non-interactively, register a tool only when it can actually
    // succeed — so scripted runs with no flags advertise nothing, and a read-only
    // grant doesn't advertise fs_write_file only to always deny it. Approval
    // flags are deliberately excluded: they change fallback policy but never
    // create capabilities. --allow-write is genuinely write-only; callers who
    // want edit/readback workflows opt into --allow-read-write explicitly.
    // --yes is the exception among approval flags: it now grants the launch cwd
    // as a read+write root (above), so it must also register the fs tools that
    // use that grant — otherwise "no prompts, just work" would silently offer
    // no tools at all. The unattended bypass likewise registers fs tools so
    // loop automation runs with no human.
    let can_read = !no_tools
        && !forced_one_shot
        && (interactive
            || cli.dangerously_skip_permissions
            || cli.yes
            || !allow_paths.is_empty()
            || !allow_read_write.is_empty());
    let can_write = !no_tools
        && (interactive
            || cli.dangerously_skip_permissions
            || cli.yes
            || !allow_write.is_empty()
            || !allow_read_write.is_empty());
    if has_required_writes && !can_write {
        return Err(CliError::user(
            "a required-write contract needs an enabled filesystem write tool and explicit --allow-write or --allow-read-write authority"
                .to_string(),
        ));
    }
    let required_write_contracts =
        prepare_required_writes(require_writes, require_write_sha256, &perms).await?;
    let defer_structured_finalization =
        !required_write_contracts.is_empty() && active_output_schema().is_some();
    let write_journal = kitsu_tools::builtins::fs::WriteJournal::new();
    let structured_finalization_gate = defer_structured_finalization.then(|| {
        let contracts = required_write_contracts.clone();
        let journal = write_journal.clone();
        kitsu_core::StructuredFinalizationGate::new(move || {
            required_write_structured_repair(&contracts, &journal)
        })
    });
    let command_artifact_sink =
        ((command_requested || default_git) && automatic_artifacts).then(|| {
            let quota = command_artifact::CommandArtifactQuota::new(
                command_artifact::DEFAULT_COMMAND_ARTIFACT_COUNT,
                command_artifact::DEFAULT_COMMAND_ARTIFACT_TOTAL_BYTES,
                command_artifact::CommandArtifactOverflowPolicy::PruneOldest,
            )
            .expect("non-zero built-in command artifact quota");
            command_artifact::CommandArtifactSink::with_quota(quota)
        });
    let command_pregrant_specs: Vec<PregrantedCommandSpec> = allow_commands
        .iter()
        .map(|encoded| PregrantedCommandSpec::parse(encoded))
        .collect::<Result<_, _>>()?;
    let mut explicit_command_grants: Vec<PregrantedCommand> = Vec::new();
    let command_policy = if command_requested || default_git {
        let launch_dir = std::env::current_dir().map_err(|error| {
            CliError::user(format!("cannot resolve command directory: {error}"))
        })?;
        let roots = if command_roots.is_empty() {
            vec![launch_dir.to_string_lossy().into_owned()]
        } else {
            command_roots.to_vec()
        };
        let command_mode = if cli.dangerously_skip_permissions {
            kitsu_tools::CommandMode::AutoAllow
        } else if interactive {
            kitsu_tools::CommandMode::Ask
        } else {
            kitsu_tools::CommandMode::Deny
        };
        let mut policy = kitsu_tools::CommandPolicy::new(&roots, command_mode)
            .map_err(|error| CliError::user(format!("invalid command policy: {error}")))?;
        for root in policy.command_roots() {
            reject_credential_command_root(&root, &setup.state_dir, "--command-root")?;
        }
        let mut readonly_runtime_roots = Vec::new();
        if !command_runtime_roots.is_empty() || allow_network {
            // Toolchains outside /usr (rustup/cargo installs in particular)
            // are invisible to contained commands without an explicit
            // read-only mount. Every configured root is disclosed: a runtime
            // root discloses its readable contents to approved commands.
            let mut containment = kitsu_tools::CommandContainmentConfig::default()
                .with_network_allowed(allow_network);
            for root in command_runtime_roots {
                containment = containment
                    .with_readonly_runtime_root(root)
                    .map_err(|error| {
                        CliError::user(format!("invalid --command-runtime-root: {error}"))
                    })?;
            }
            for root in containment.readonly_runtime_roots() {
                reject_credential_command_root(root, &setup.state_dir, "--command-runtime-root")?;
                readonly_runtime_roots.push(root.clone());
                eprintln!(
                    "~ command containment: read-only runtime root {} (contents readable by approved commands)",
                    terminal_inline(&root.display().to_string())
                );
            }
            if allow_network {
                // The sandbox weakening is disclosed at startup, like every
                // runtime root, so a session can never silently have network.
                eprintln!(
                    "~ command containment: --allow-network — approved commands may reach the network; ~/.ssh/known_hosts and ~/.ssh/config are readable (private keys stay denied)"
                );
            }
            policy = policy.with_containment_config(containment);
        }
        reject_host_output_command_authority(
            cli,
            &policy.command_roots(),
            &readonly_runtime_roots,
        )?;
        if interactive {
            policy = policy.with_prompter(Arc::new(CliCommandPrompter));
        }
        if let Some(sink) = command_artifact_sink.as_ref() {
            policy = policy.with_output_artifact_sink(Arc::new(sink.clone()));
        }
        if !command_env.is_empty() {
            let resolved_env = command_env
                .iter()
                .map(|raw| CommandEnvSpec::parse(raw)?.resolve())
                .collect::<Result<Vec<(String, String)>, CliError>>()?;
            policy = policy.with_passthrough_environment(resolved_env);
        }
        let policy = Arc::new(policy);
        explicit_command_grants = command_pregrant_specs
            .iter()
            .map(|spec| spec.resolve(&launch_dir))
            .collect::<Result<_, _>>()?;
        for grant in &mut explicit_command_grants {
            // Fail closed at startup, before any turn: an unsatisfiable pregrant
            // is a configuration error, never a silently dropped capability.
            let canonical = policy
                .grant_command(&grant.argv, &grant.cwd)
                .map_err(|error| {
                    CliError::user(pregrant_error(grant, &roots, &error.to_string()))
                })?;
            grant.cwd = canonical;
            // Cycle-19 audit #2: a path-form executable outside every command
            // and runtime root can never actually launch under containment —
            // catch it here, not after the model has burned rounds on it.
            if let Some(executable) = grant.argv.first().filter(|arg| arg.contains('/')) {
                let resolved = grant.cwd.join(executable);
                if let Ok(resolved) = std::fs::canonicalize(&resolved) {
                    // Containment always binds the standard system prefixes
                    // read-only (see runtime_mount_paths), so system binaries
                    // are inherently visible.
                    let system_visible = ["/usr", "/bin", "/lib", "/lib64", "/sbin"]
                        .iter()
                        .any(|prefix| resolved.starts_with(prefix));
                    let visible = system_visible
                        || policy
                            .command_roots()
                            .iter()
                            .any(|root| resolved.starts_with(root))
                        || policy
                            .containment_config()
                            .readonly_runtime_roots()
                            .iter()
                            .any(|root| resolved.starts_with(root));
                    if !visible {
                        return Err(CliError::user(format!(
                            "--allow-command executable '{}' is outside every command/runtime root and can never run under containment — add its directory with --command-root or --command-runtime-root {}",
                            resolved.display(),
                            shorten_home(resolved.parent().unwrap_or(&resolved)),
                        )));
                    }
                }
            }
        }
        Some(policy)
    } else {
        None
    };

    // Discover skills before building tool sets (they register the `skill`
    // tool). Skills are budget-gated context, not privileged — suggested actions
    // still go through the permission model. `--no-project-context` disables
    // repository discovery while retaining explicitly selected user skills.
    let no_project = project_context_disabled(cli);
    let skillset = Arc::new(
        std::env::current_dir()
            .map(|cwd| skills::SkillSet::discover(&cwd, no_project, &perms))
            .unwrap_or_default(),
    );

    // Small local windows cannot afford the full read schema: Ollama serves
    // 4096 tokens by default, while the full set alone serializes to more than
    // that — and it truncates from the front, so the system prompt and the tool
    // schemas are what get destroyed. Pick the tier ONCE here so the main
    // conversation, subagents, and specialist profiles all advertise the same
    // surface; a mixed set would let a subagent inherit a registry its own
    // window cannot hold.
    let tool_tier = kitsu_tools::ToolTier::for_window(setup.context_window);

    // The subagent tool set: base tools ONLY (no `spawn_agent` → depth-1).
    let sub_tools = {
        let mut t = Registry::new();
        register_base_tools(
            &mut t,
            &perms,
            &skillset,
            can_read,
            can_write,
            &write_journal,
            command_policy.as_ref().filter(|_| command_requested),
            tool_tier,
        )?;
        if default_git && !command_requested {
            let (descriptor, handler) = builtins::command::git_tool(
                command_policy
                    .as_ref()
                    .expect("default Git owns a command policy")
                    .clone(),
            );
            t.register_with_descriptor(descriptor, handler)?;
        }
        Arc::new(t)
    };

    // Specialist profiles are requests that are intersected with the current
    // invocation's real authority. `reviewer` receives only the already-
    // authorized read surface; `reasoning` receives an intentionally empty
    // registry and therefore works without dummy filesystem grants.
    let reviewer_tools = {
        let mut tools = Registry::new();
        register_base_tools(
            &mut tools,
            &perms,
            &skillset,
            can_read,
            false,
            &write_journal,
            None,
            tool_tier,
        )?;
        Arc::new(tools)
    };
    let reasoning_tools = Arc::new(Registry::new());
    let mut profile_catalog = subagent::profile::AgentProfileCatalog::new();
    profile_catalog
        .insert(subagent::profile::AgentProfile {
            name: "reviewer".to_string(),
            model: None,
            // Inherit, never High: a profile that injects `reasoning_effort`
            // into the request is rejected outright (HTTP 400) by gateways that
            // do not know the field, which includes the default local Ollama
            // gateway. Effort stays a parent-invocation decision.
            reasoning_effort: subagent::profile::AgentReasoningEffort::Inherit,
            instructions: "Review the delegated evidence critically. Do not modify files or run developer commands; return concrete, prioritized findings.".to_string(),
            tool_policy: subagent::profile::AgentToolPolicy::AllowList(
                reviewer_tools
                    .descriptors()
                    .iter()
                    .map(|descriptor| descriptor.name.clone())
                    .collect(),
            ),
            permission_defaults: subagent::profile::AgentPermissionDefaults {
                inherit_session: true,
                ..Default::default()
            },
        })
        .expect("built-in reviewer profile is valid");
    profile_catalog
        .insert(subagent::profile::AgentProfile::reasoning_only(
            "reasoning",
            "Reason from the supplied task only. You have no local tools or inherited filesystem/command authority; return a concise self-contained conclusion.",
        ))
        .expect("built-in reasoning profile is valid");
    let profile_catalog = Arc::new(profile_catalog);
    // Building a validated round-trip DTO here keeps the persistence contract
    // exercised even though this release ships only immutable built-ins.
    let profile_catalog = Arc::new(
        subagent::profile::AgentProfileCatalog::from_dto(profile_catalog.to_dto())
            .expect("built-in profile catalog round-trips"),
    );
    let profile_tools = Arc::new(std::collections::HashMap::from([
        ("reviewer".to_string(), reviewer_tools),
        ("reasoning".to_string(), reasoning_tools),
    ]));

    // The live (model, window) subagents should use, kept in sync with `/model`
    // (see the loop below) so a subagent never runs the session's initial model
    // against a stale window.
    let live_mw: subagent::LiveModelWindow = Arc::new(std::sync::Mutex::new((
        initial_model.clone(),
        setup.context_window,
    )));
    // Registry of BACKGROUND subagents (Stage C), shared with the spawn_agent_bg /
    // agent_result tools and the `/agents` command.
    let agent_registry = subagent::AgentRegistry::with_limits_and_thread_limits(
        subagent::AgentRegistryLimits::default(),
        subagent::thread::AgentThreadLimits::default(),
    )
    .map_err(|error| CliError::user(format!("invalid background-thread limits: {error}")))?;
    // `[agents] blocking` / `block_timeout_secs` (design B1/B3). The defaults
    // are the registry's own, so a config with no `[agents]` table is a no-op.
    agent_registry.set_blocking(setup.agent_blocking);
    agent_registry.set_block_timeout(setup.agent_block_timeout);

    // Per-turn subagent spawn budget. Prompt
    // guidance demonstrably does not restrain small local models from agent
    // storms, so the cap is enforced at the tool boundary; the orchestrator
    // resets it each main-conversation turn. Scripted/piped automation gets a
    // deliberately low default — storms there burn the whole run unattended.
    const SPAWN_CAP_INTERACTIVE_DEFAULT: u32 = 8;
    const SPAWN_CAP_SCRIPTED_DEFAULT: u32 = 2;
    let spawn_cap = cli
        .agent_spawn_cap
        .or(setup.agent_spawn_cap)
        .unwrap_or(if interactive {
            SPAWN_CAP_INTERACTIVE_DEFAULT
        } else {
            SPAWN_CAP_SCRIPTED_DEFAULT
        });
    let spawn_budget = Arc::new(kitsu_core::SpawnBudget::new(spawn_cap));
    // Run-wide tallies (reads/denied/failed) shared by the main conversation
    // and every spawned subagent, feeding the end-of-run receipt.
    let run_tallies = Arc::new(kitsu_core::RunTallies::default());

    // The main tool set: base tools plus, when spawning is enabled, the agent
    // tools that delegate to `sub_tools` in their own isolated context. A zero
    // cap removes the agent schemas entirely: advertising tools that can only
    // return "budget exhausted" wastes context and makes /tools misleading.
    let mut tools = Registry::new();
    if !no_tools {
        register_base_tools(
            &mut tools,
            &perms,
            &skillset,
            can_read,
            can_write,
            &write_journal,
            command_policy.as_ref().filter(|_| command_requested),
            tool_tier,
        )?;
        if default_git && !command_requested {
            let (descriptor, handler) = builtins::command::git_tool(
                command_policy
                    .as_ref()
                    .expect("default Git owns a command policy")
                    .clone(),
            );
            tools.register_with_descriptor(descriptor, handler)?;
        }
    }
    let mut sub_cfg = build_agent_config(
        initial_model.clone(),
        cli,
        setup.context_window,
        None,
        setup.artifact_policy,
    );
    sub_cfg.stream = false; // the subagent's output is a returned result, not streamed
                            // Foreground subagents run QUIET (cycle-15 audit #3 / cycle-27 double
                            // print): their inner narration corrupted the answer channel and their
                            // final text arrived twice — streamed by the child, then repeated by the
                            // parent. The ◆ Agent(...) header and ◇ result receipt remain the progress
                            // surface; the answer arrives once, through the parent.
    sub_cfg.quiet = true;
    sub_cfg.response_format = None;
    sub_cfg.skill_names = skillset.names().into_iter().map(str::to_string).collect();
    sub_cfg.run_tallies = Some(run_tallies.clone());
    let sub_system = if skillset.is_empty() {
        String::new()
    } else {
        format!(
            "You have these skills (call the `skill` tool to load one when a task matches):\n{}",
            skillset.index("")
        )
    };
    // Card **T4-S7**, requirement **A6**: the agent view's dispatcher. Built
    // here, where the session's binder, subagent registry, config template and
    // profile catalog all exist, and handed to **both** of the view's routes
    // (the composer's chord/`\u{2190}\u{2190}` route and `/jobs view`).
    //
    // It is built whatever `spawn_cap` is and whether or not the model got the
    // agent tools: `spawn_cap` is the *model's* per-turn fan-out cap, and a
    // human typing a task into the view is not a storming model (A6). What
    // admits a dispatch is the registry's own ceilings and nothing else.
    // Unconditional, and `Some` on every carrier that reaches here: the two
    // routes are what must not disagree, so the dispatcher is not gated on a
    // condition one route could satisfy and the other not. A carrier that
    // cannot draw the view declines the overlay itself, upstream of this.
    let view_dispatcher: Option<subagent::ViewDispatcher> = {
        let view_route = provider.clone();
        let mut view_cfg = sub_cfg.clone();
        view_cfg.quiet = true;
        Some(subagent::view_dispatcher(
            subagent::ProviderBinder::<ProviderRouteHandle<CompiledProvider>>::new(move |model| {
                view_route
                    .bind_model(model)
                    .map(Arc::new)
                    .map_err(ProviderError::from)
            }),
            sub_tools.clone(),
            view_cfg,
            live_mw.clone(),
            sub_system.clone(),
            agent_registry.clone(),
            subagent::BackgroundProfileRuntime::new(profile_catalog.clone(), profile_tools.clone()),
        ))
    };
    if !no_tools && spawn_cap > 0 {
        let subagent_route = provider.clone();
        let subagent_provider =
            subagent::ProviderBinder::<ProviderRouteHandle<CompiledProvider>>::new(move |model| {
                subagent_route
                    .bind_model(model)
                    .map(Arc::new)
                    .map_err(ProviderError::from)
            });
        // Stage A: synchronous single delegation.
        let (d, h) = subagent::spawn_agent_tool_with_binder(
            subagent_provider.clone(),
            sub_tools.clone(),
            sub_cfg.clone(),
            live_mw.clone(),
            sub_system.clone(),
            spawn_budget.clone(),
        );
        let registration = tools.register_with_descriptor(d, h);
        registration.expect("tool name must be unique");
        // Stages B and C cost ~370 tokens of schema and give a small local
        // model three more ways to get delegation wrong — on a lean window that
        // trade is backwards. One obvious synchronous `spawn_agent` is what
        // makes "just spawn an agent" work at 4k; parallel and detached
        // delegation return with a window that can afford them.
        if !tool_tier.is_lean() {
            // Stage B: fan several isolated subagents out in parallel.
            let (d, h) = subagent::spawn_agents_tool_with_binder(
                subagent_provider.clone(),
                sub_tools.clone(),
                sub_cfg.clone(),
                live_mw.clone(),
                sub_system.clone(),
                spawn_budget.clone(),
            );
            let registration = tools.register_with_descriptor(d, h);
            registration.expect("tool name must be unique");
            // Stage C: detached background subagent (runs quiet so it doesn't corrupt
            // the prompt) + a tool to fetch its result by id.
            let mut bg_cfg = sub_cfg;
            bg_cfg.quiet = true;
            let (d, h) = subagent::spawn_agent_bg_tool_with_binder(
                subagent_provider,
                sub_tools.clone(),
                bg_cfg,
                live_mw.clone(),
                sub_system,
                agent_registry.clone(),
                subagent::BackgroundProfileRuntime::new(profile_catalog.clone(), profile_tools),
                spawn_budget.clone(),
            );
            let registration = tools.register_with_descriptor(d, h);
            registration.expect("tool name must be unique");
            let (d, h) = subagent::agent_result_tool(agent_registry.clone());
            let registration = tools.register_with_descriptor(d, h);
            registration.expect("tool name must be unique");
            // T5-S6 (H5): block on a background agent's state. It spawns
            // nothing, so it consumes no `SpawnBudget` — but it lives inside
            // the `spawn_cap > 0` gate on purpose, next to `agent_result`,
            // which consumes none either: at a zero cap no background agent
            // can exist for a wait to wait on, so a registered `agent_wait`
            // would advertise a capability the session cannot deliver and the
            // model would burn a round on "no background agent #N".
            let (d, h) = subagent::agent_wait_tool(agent_registry.clone());
            let registration = tools.register_with_descriptor(d, h);
            registration.expect("tool name must be unique");
        }
    }

    // The model-callable `workspace_diff` evidence tool: a bounded disclosure
    // of host-gathered diff bytes (like /review), never a model filesystem
    // read, so it is independent of can_read and grants no root. Main
    // conversation only: review one-shots stay tool-free as documented, and
    // subagent registries are untouched (disclosure announcements are a
    // top-level conversation contract). The private-path snapshot is a
    // directory-level superset of `private_workspace_paths` for the whole
    // process (offload generations always land under the state dir, a session
    // sidecar beneath it, or the stateless temp base), so per-boundary
    // rotation cannot open an exclusion gap.
    if !no_tools && setup.workspace_evidence && !cli.no_workspace_evidence && !forced_one_shot {
        let launch_dir = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        let private_evidence_paths = vec![
            setup.config_path.clone(),
            setup.state_dir.clone(),
            std::env::temp_dir().join(format!("kitsu-offload-{}", run_id())),
        ];
        if let Some((d, h)) =
            workspace_diff_tool(&launch_dir, private_evidence_paths, &perms, &live_mw)
        {
            let registration = tools.register_with_descriptor(d, h);
            registration.expect("tool name must be unique");
        }
    }
    let tool_names: Vec<String> = tools.descriptors().iter().map(|d| d.name.clone()).collect();

    // Offloaded tool results and complete developer-command output share one
    // protected destination for the active session boundary. Durable sessions
    // keep generations beneath their sidecar; stateless command generations
    // remain under protected state so a one-shot process never prints a dead
    // path. Stateless non-command generations share a per-run temp namespace
    // that is removed when this process exits.
    // A pointer is useful only when this registry also exposes fs_read_file.
    // In command-only automation, offloading would replace usable inline
    // command output with a path the model has no tool capable of reading.
    let model_can_read_artifacts = automatic_artifacts && can_resolve_offload_pointers(&tool_names);
    let artifact_storage_enabled = automatic_artifacts
        && !tool_names.is_empty()
        && (model_can_read_artifacts || command_policy.is_some());
    let stateless_artifact_base = if command_policy.is_some() {
        setup.state_dir.join("artifacts")
    } else {
        std::env::temp_dir().join(format!("kitsu-offload-{}", run_id()))
    };
    let mut artifact_directories = ArtifactDirectoryAllocator::default();
    let offload_state = Rc::new(RefCell::new(None));
    if artifact_storage_enabled {
        if let Err(error) = rotate_artifact_storage(
            &perms,
            &offload_state,
            command_artifact_sink.as_ref(),
            || {
                artifact_directory_for_boundary(
                    &mut artifact_directories,
                    artifact_storage_enabled,
                    session_store.as_ref(),
                    &stateless_artifact_base,
                )
            },
        ) {
            eprintln!("warning: private artifact storage disabled: {error}");
        }
    }
    let _offload_guard =
        if artifact_storage_enabled && session_store.is_none() && command_policy.is_none() {
            Some(OffloadDirGuard(stateless_artifact_base.clone()))
        } else {
            None
        };

    let mut model = initial_model;
    // Shared with session UI state and updated in place by `/model`, so
    // switching models doesn't leave every subsequent turn budgeting against
    // the OLD model's window — a stale, larger window would under-trim and the
    // gateway would reject the oversized request instead of kitsu proactively
    // trimming to fit the new model's real limit.
    let context_window = Rc::new(Cell::new(setup.context_window));
    let reasoning_effort = Rc::new(Cell::new(cli_reasoning_effort(cli)));
    let base_cfg = {
        let context_window = context_window.clone();
        let reasoning_effort = reasoning_effort.clone();
        let offload_state = offload_state.clone();
        let spawn_budget = spawn_budget.clone();
        let run_tallies = run_tallies.clone();
        let structured_finalization_gate = structured_finalization_gate.clone();
        let skill_names: Vec<String> = skillset.names().into_iter().map(str::to_string).collect();
        move |model: String, cli: &Cli| {
            let mut config = build_agent_config(
                model,
                cli,
                context_window.get(),
                model_can_read_artifacts
                    .then(|| offload_state.borrow().clone())
                    .flatten(),
                setup.artifact_policy,
            );
            config.reasoning_effort = reasoning_effort.get();
            config.skill_names = skill_names.clone();
            config.run_tallies = Some(run_tallies.clone());
            config.defer_response_format_until_final = defer_structured_finalization;
            config.structured_finalization_gate = structured_finalization_gate.clone();
            // Main-conversation turns carry the spawn budget so the
            // orchestrator resets it at each turn start (subagent configs
            // stay `None`).
            config.spawn_budget = Some(spawn_budget.clone());
            config
        }
    };
    // Persists the context-meter anchor and compaction circuit breaker across
    // every turn of this interactive/one-shot chat session.
    let mut session_state = kitsu_core::AgentSessionState::default();

    // Assemble the system prompt while recording provenance (design §3): each
    // static context source with its token cost, policy, and reason, so `/context`
    // is honest about what loaded and why. Each source is pushed as a chunk so its
    // estimate (headers included) sums to the whole system-message estimate.
    let mut provenance: Vec<CtxEntry> = Vec::new();
    let prompt_environment = {
        let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        // Tell the model the SAME truth the permission layer enforces (see
        // `cwd_grant`): an interactive or `--yes` run grants the launch
        // directory read+write, so the model must see it as `read-write` and
        // edit directly instead of concluding the project is read-only. A
        // read-only cwd label here was why the model declined to write.
        let mut read_only: Vec<String> = allow_paths.to_vec();
        let mut read_write: Vec<String> = allow_read_write.to_vec();
        let grant = cwd_grant(cli.yes, interactive);
        let cwd_text = cwd.to_string_lossy().to_string();
        if grant.read && grant.write {
            if !read_write.contains(&cwd_text) {
                read_write.push(cwd_text);
            }
        } else if grant.read && !read_only.contains(&cwd_text) {
            read_only.push(cwd_text);
        }
        PromptEnvironment {
            access_prompts: perms.mode() == Mode::Ask,
            ..PromptEnvironment::from_grants(&cwd, &read_only, allow_write, &read_write)
                .with_command_pregrants(&explicit_command_grants)
        }
    };
    let base = chat_system_prompt(&tool_names, &prompt_environment);
    provenance.push(CtxEntry::new(
        CtxSourceKind::CoreInstructions,
        "base prompt",
        estimate_str(&base),
        "always",
        "core assistant instructions",
    ));
    let mut system = base;
    if setup.artifact_policy == ArtifactStoragePolicy::InlineOnly && !tool_names.is_empty() {
        let contract = "\n\nAutomatic artifact storage is disabled for this run. Commands return bounded previews only; oversized tool results are irreversibly reduced to explicit head/tail previews. No new saved full-output path is created; historical artifact pointers may remain in resumed context, but no managed artifact read root is granted.";
        system.push_str(contract);
        provenance.push(CtxEntry::new(
            CtxSourceKind::CoreInstructions,
            "artifact-free policy",
            estimate_str(contract),
            "host-enforced",
            "no automatic command or oversized tool-result files",
        ));
    }
    if let Some(contract) = required_write_system_instruction(&required_write_contracts) {
        let tokens = estimate_str(&contract);
        system.push_str(&contract);
        provenance.push(CtxEntry::new(
            CtxSourceKind::CoreInstructions,
            format!("required writes ({})", required_write_contracts.len()),
            tokens,
            "host-enforced",
            "verified model filesystem writes plus final-state changes",
        ));
    }
    let mut project_context_source = None;

    // Project context (llms.txt / AGENTS.md), budget-gated (Stage 4).
    if !project_context_disabled(cli) {
        if let Some((ctx, _, source)) = std::env::current_dir()
            .ok()
            .and_then(|cwd| llms::load(&cwd, None, setup.context_window, &perms))
        {
            project_context_source = Some(source.clone());
            let label = source
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("project context");
            let chunk = format!("\n\n---\nProject context (from {label}):\n{ctx}");
            let tokens = estimate_str(&chunk);
            system.push_str(&chunk);
            if resumed.is_none() {
                eprintln!(
                    "~ loaded project context from {} ({tokens} tokens; disable with --no-project-context)",
                    shorten_home(&source)
                );
            }
            provenance.push(CtxEntry::new(
                CtxSourceKind::ProjectContext,
                format!("project ({label})"),
                tokens,
                "loaded",
                format!("curated context from {}", shorten_home(&source)),
            ));
        }
    }
    // Skill index (always-on): names + descriptions; bodies load on demand.
    if !no_tools && !skillset.is_empty() {
        let mut chunk = String::from(
            "\n\n---\nAvailable skills — call the `skill` tool with the name to load full \
             instructions when a task matches:\n",
        );
        chunk.push_str(&skillset.index(""));
        let always = skillset.always_bodies();
        if !always.is_empty() {
            chunk.push('\n');
            chunk.push_str(&always);
        }
        system.push_str(&chunk);
        eprintln!("~ {} skill(s) available", skillset.names().len());
        provenance.push(CtxEntry::new(
            CtxSourceKind::SkillContext,
            format!("skill index ({})", skillset.names().len()),
            estimate_str(&chunk),
            "always",
            "names + descriptions (bodies load on demand)",
        ));
    }
    // --skill preloads named skills up front (in addition to on-demand loading).
    if let Some(pre) = preload_skills(cli, &skillset) {
        let chunk = format!("\n\n---\nPreloaded skills (follow these):\n{pre}");
        system.push_str(&chunk);
        provenance.push(CtxEntry::new(
            CtxSourceKind::SkillContext,
            format!("skills (--skill x{})", cli.skill.len()),
            estimate_str(&chunk),
            "preloaded",
            "--skill flag",
        ));
    }
    // Tool schemas ride the request's `tools` field, not the system message.
    if !tool_names.is_empty() {
        let schema_tokens =
            estimate_str(&serde_json::to_string(tools.descriptors()).unwrap_or_default());
        provenance.push(CtxEntry::new(
            CtxSourceKind::ToolSchemas,
            format!("tool schemas ({})", tool_names.len()),
            schema_tokens,
            "admitted",
            tool_names.join(", "),
        ));
    }
    let fresh_project_context_source = project_context_source.clone();
    let fresh_provenance = provenance.clone();
    let directory = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let session_id = Rc::new(RefCell::new(
        session_store.as_ref().map(|store| store.id().to_string()),
    ));

    let mut messages = vec![ChatMessage::system(system)];
    // `/new` starts from the context assembled for this invocation, rather than
    // inheriting an older resumed session's system prompt.
    let fresh_system_message = messages[0].clone();
    let mut resumed_context_refreshed = false;
    let mut resumed_skill_changes = Vec::new();
    if let Some(saved) = resumed {
        if saved.gateway != setup.gateway_name {
            return Err(CliError::user(format!(
                "saved session '{}' uses gateway '{}', but '{}' is selected",
                saved.id, saved.gateway, setup.gateway_name
            )));
        }
        if !matches!(
            saved.messages.first().map(|message| message.role),
            Some(kitsu_providers::Role::System)
        ) {
            return Err(CliError::user(format!(
                "saved session '{}' has no leading system message",
                saved.id
            )));
        }
        if cli.model.is_none() && saved.model != model {
            let resumed_model = saved.model.clone();
            let resumed_window = if setup.context_window_pinned {
                context_window.get()
            } else {
                await_cancellable(provider.model_context_length(&resumed_model))
                    .await
                    .ok_or_else(|| {
                        CliError::cancelled("session context-window discovery cancelled by Ctrl+C")
                    })?
                    .map_err(|error| {
                        map_provider_error_with_diagnostics(
                            error,
                            ProviderOperation::ModelCatalog,
                            Some(cli.timeout_secs),
                            setup.provider_diagnostics,
                        )
                    })?
            };
            let expected_generation = provider
                .bind()
                .map_err(|error| {
                    CliError::user(format!("cannot stage resumed model route: {error}"))
                })?
                .identity()
                .generation();
            provider
                .replace_model(expected_generation, resumed_model.clone())
                .map_err(|error| {
                    CliError::user(format!("cannot publish resumed model route: {error}"))
                })?;
            model = resumed_model;
            context_window.set(resumed_window);
            if !setup.context_window_pinned {
                *setup
                    .context_window_provenance
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) =
                    ContextWindowProvenance::live_discovery(resumed_window);
            }
        }
        project_context_source = fresh_project_context_source.clone();
        provenance = fresh_provenance.clone();
        if no_project && saved.project_context.is_some() {
            eprintln!("~ omitted saved project context because --no-project-context is active");
        }
        let system_context_changed = saved.messages.first() != Some(&fresh_system_message);
        messages = restored_messages(&saved, &fresh_system_message);
        resumed_skill_changes = refresh_loaded_skill_messages(&mut messages, &skillset);
        resumed_context_refreshed = system_context_changed || !resumed_skill_changes.is_empty();
        if resumed_context_refreshed {
            eprintln!(
                "~ rebuilt saved system context from current tools, project metadata, and bounded skill provenance"
            );
        }
        for change in &resumed_skill_changes {
            eprintln!("~ {}", terminal_inline(change));
        }
        let resumed_label = session_store
            .as_ref()
            .and_then(|store| store.name())
            .map(|name| format!("'{}' ({})", terminal_inline(name), saved.id))
            .unwrap_or_else(|| saved.id.clone());
        eprintln!("{}", kitsu_render::milestone_line(&format!(
            "resumed session {} (saved grants were not restored; runtime grants reset; invocation grants retained: {} read, {} write)",
            resumed_label,
            perms.base_read_roots().len(),
            perms.base_write_roots().len(),
        )));
        session_state.meter.invalidate();
    }
    *live_mw.lock().expect("live model/window") = (model.clone(), context_window.get());

    let session = SessionInfo {
        gateway: setup.gateway_name.clone(),
        provider_diagnostics: setup.provider_diagnostics,
        directory,
        config_path: setup.config_path.clone(),
        state_dir: setup.state_dir.clone(),
        endpoint: setup.endpoint.clone(),
        auth_status: setup.auth_status.clone(),
        artifact_policy: setup.artifact_policy,
        artifact_policy_source: setup.artifact_policy_source,
        project_context: RefCell::new(project_context_source),
        offload_dir: offload_state.clone(),
        model_can_read_artifacts,
        defer_structured_finalization,
        structured_finalization_gate,
        session_id,
        spawn_budget: spawn_budget.clone(),
        run_tallies: run_tallies.clone(),
        tool_names: tool_names.clone(),
        command_roots: command_policy
            .as_ref()
            .map(|policy| policy.command_roots())
            .unwrap_or_default(),
        command_policy: command_policy.clone(),
        command_artifact_sink: command_artifact_sink.clone(),
        invocation_command_grants: command_policy
            .as_ref()
            .map(|policy| policy.session_grants())
            .unwrap_or_default(),
        invocation_command_pregrants: explicit_command_grants.clone(),
        agent_profiles: profile_catalog,
        permissions: perms.clone(),
        autosave_dirty: Cell::new(false),
        window: context_window.clone(),
        reasoning_effort: reasoning_effort.clone(),
        window_provenance: setup.context_window_provenance.clone(),
        window_refresh: setup.context_window_refresh.clone(),
        max_tokens: cli.max_output_tokens,
        provenance: RefCell::new(provenance),
    };
    // A fresh durable id is only a plan until the first snapshot is installed;
    // a resumed id is already addressable. Scripted `/clear` can rotate this
    // receipt back to pending at a recoverable boundary.
    let initial_snapshot_state = if !durable_session {
        SessionSnapshotState::Disabled
    } else if resumed_existing {
        SessionSnapshotState::Resumable
    } else {
        SessionSnapshotState::PendingUntilFirstSuccess
    };
    set_jsonl_session_receipt(session.session_id.borrow().clone(), initial_snapshot_state);

    if setup.artifact_policy == ArtifactStoragePolicy::InlineOnly && !cli.jsonl {
        eprintln!(
            "~ artifacts disabled: commands keep bounded previews only; large tool results are not offloaded; existing artifacts are unchanged"
        );
    }

    if cli.jsonl {
        let provider_route = provider.receipt().map_err(|error| {
            CliError::user(format!("cannot inspect current provider route: {error}"))
        })?;
        emit_jsonl(
            "run_started",
            serde_json::json!({
                "mode": if scripted {
                    "script"
                } else if forced_one_shot {
                    "review"
                } else {
                    "piped"
                },
                "gateway": setup.gateway_name,
                "model": model,
                "provider_route": provider_route,
                "session_id": session.session_id.borrow().clone(),
                "resumed": resumed_existing,
                "durable_session": durable_session,
                "session_snapshot_state": initial_snapshot_state.as_str(),
                "system_context_refreshed": resumed_context_refreshed,
                "skill_context_changes": resumed_skill_changes,
                "artifact_policy": artifact_policy_receipt(
                    setup.artifact_policy,
                    setup.artifact_policy_source,
                    model_can_read_artifacts,
                ),
            }),
        )?;
    }
    emit_output_schema_receipt(cli)?;
    emit_required_write_armed(cli, &required_write_contracts)?;

    if durable_session && !interactive {
        eprintln!(
            "~ session {} autosave planned; a new id becomes resumable after the first successful turn or state-changing command (snapshots never store grants; invocation grants: {} read, {} write)",
            terminal_inline(
                session
                    .session_id
                    .borrow()
                    .as_deref()
                    .unwrap_or("unavailable")
            ),
            perms.base_read_roots().len(),
            perms.base_write_roots().len(),
        );
        // The planned session id is stable and may be announced before its
        // snapshot exists. New and resumed automation commit only after the
        // first successful turn or state-changing/query command, so an empty or
        // failed start cannot displace useful durable history.
    }

    if interactive {
        // First-contact orientation (cycle-11 audit): one torii line at the
        // session gate — the model/gateway you are talking to and the two
        // things a new user needs (`/help`, how to leave). The torii is the
        // session-boundary marker (see docs/architecture.md), and opening the
        // session is its most natural gate.
        kitsu_render::milestone(&format!(
            "kitsu chat — {} via {} · /help for commands · Ctrl+D exits",
            model, setup.gateway_name
        ));
    }

    let mut initial_review_submission = if let Some(review) = initial_review {
        report_write_checkpoints(write_journal.drain_and_take_reports().await);
        let requested_path = review.request.path.clone();
        let evidence =
            authorized_workspace_review(&session, &perms, &write_journal, review.request).map_err(
                |error| CliError::user(format!("cannot prepare verified review: {error}")),
            )?;
        if review_evidence_is_empty(&evidence) {
            return Err(CliError::user(empty_review_message(
                &evidence,
                requested_path.as_deref(),
                true,
            )));
        }
        if cli.jsonl {
            emit_jsonl(
                "review_disclosure",
                top_level_review_disclosure_event(&evidence),
            )?;
        } else {
            eprintln!("~ verified {}", review_target_label(&evidence));
            eprintln!("{}", evidence_disclosure_line(&evidence, &model));
        }
        let mut prompt = verified_review_prompt(&evidence);
        if let Some(instructions) = review
            .instructions
            .as_deref()
            .map(str::trim)
            .filter(|instructions| !instructions.is_empty())
        {
            prompt.push_str("\n\nAdditional user review instructions:\n");
            prompt.push_str(instructions);
        }
        Some(SubmittedPrompt::review(prompt))
    } else {
        None
    };

    // The per-process agent state file (`[agents] state_file`). The carrier
    // rule lives entirely in `should_write`, so this is the only gate: a
    // second check here would make an explicit `on` unobservable in the
    // scripted and piped carriers that reach this point. The guard removes the
    // file however this function returns, and writing is best effort — every
    // error is dropped, because a hint must never cost a turn.
    let agent_state_pid = std::process::id();
    let agent_state_dir = agent_state_file::state_dir();
    let agent_state_enabled = agent_state_file::should_write(
        setup.agent_state_file,
        PROCESS_INFERENCE_CARRIER.get().copied(),
    );
    let _agent_state_guard = agent_state_enabled
        .then(|| agent_state_file::StateFileGuard::new(agent_state_dir.clone(), agent_state_pid));

    // The inline viewport (T0-S5a/T3-S2): the default at a terminal, or
    // selected by `--tui inline` / `[tui] mode = "inline"`. Held for the whole
    // session so the scroll region is released on every exit path. In classic
    // mode — `--no-tui`, and what every non-terminal carrier resolves to —
    // `install` writes nothing and touches no global state, so the session is
    // byte-identical.
    // `mut` and reassignable, for card T3-S9: `/tui classic|inline` swaps the
    // renderer at the idle-prompt seam by dropping this guard (which runs the
    // established teardown in its established order) or by installing a fresh
    // one. There is deliberately no second teardown path.
    // Card **T5-S2**: the fullscreen segment is the second half of the same
    // decision, so it is resolved in the same block and from the same mode.
    // `None` in classic and inline; `Some` for as long as this session holds
    // the terminal's alternate screen, which is what `/tui`'s report and the
    // switch arm below both key on. Dropping it gives the screen and the
    // title back, on a renderer switch and on every exit path out of this
    // function alike.
    #[cfg(unix)]
    let (mut tui_bridge_guard, mut fullscreen_segment, _title_stack_guard) = {
        // T3-S2 resolves the whole `[tui]` table here, at the one point where
        // the renderer is chosen. Only `mode` has a consumer today; the
        // remaining four are resolved and bound so that the alternate-screen
        // force, the chord prefix (T4), the title row, the left-arrow binding
        // and the cell ring (T3-S6) each arrive at their renderer already
        // decided, rather than growing a second resolution point per card.
        // Card T4-S2 is `prefix`'s and `left_arrow_opens_agents`' first
        // consumer: both are translated here, at the one resolution point, and
        // published for the inline reader. The prefix is read as *configured*
        // and never as a hardcoded chord, so `prefix = "ctrl+t"` makes the
        // default `Ctrl+X a` inert.
        tui_bridge::set_inline_keys(tui_bridge::InlineKeys {
            prefix: tui_bridge::chord_prefix_byte(setup.tui.prefix.as_str()),
            left_arrow_opens_agents: setup.tui.left_arrow_opens_agents,
        });
        // Still resolved here and still without a consumer: the
        // alternate-screen force (the agent view takes the alternate screen
        // unconditionally today). The cell ring is no longer parked here -- it
        // is handed to `install` below.
        // F62: the flag is already refused in argv validation
        // (`validate_no_alt_screen_flag`), so reaching here means it was not
        // passed and this resolves to the configured value -- which can only
        // be `auto` or `always`, the deserializer having refused `never`.
        let _resolved_alternate_screen = setup
            .tui
            .effective_alternate_screen(cli.no_alt_screen)
            .expect("--no-alt-screen is refused in argv validation (F62)");
        // Card H2-title: `[tui] title`'s consumer, at last (**F139**). Stored
        // before `enter_fullscreen` below, because the key gates the title
        // *stack* that segment pushes as well as the OSC 2 string the turn
        // boundaries write.
        tui_bridge::set_title_enabled(setup.tui.title);
        // Card **H2-stack** (**F140**): the title *stack* is the session's,
        // not a renderer's. Before this card the one pusher was
        // `enter_fullscreen`, so a title set in classic or inline was never
        // pushed and outlived the process -- and the dying-signal restore
        // popped a stack it had never pushed. Pushed here, under exactly the
        // gate the OSC 2 string uses, and popped by this guard's `Drop` on
        // every exit path out of `run_chat`. Created **before**
        // `enter_fullscreen` below, which is what lets that segment see the
        // session's push and add none of its own.
        let title_stack_guard = tui_bridge::TitleStackGuard::new(Box::new(std::io::stderr()));
        // The inline viewport is the default only where it can paint: stdin
        // and stdout both terminals, plain mode off. Any other carrier --
        // piped, scripted, `--jsonl`, `--plain`, a redirected stdout -- gets
        // classic unless `--tui` or `[tui] mode` asked for a renderer by
        // name, which keeps every automation byte stream exactly as it was
        // before inline became the default. An explicit request is honoured
        // as before; only the *default* is guarded.
        let terminal_can_paint = interactive
            && std::io::IsTerminal::is_terminal(&std::io::stdout())
            && !kitsu_render::plain_mode();
        let mode = match setup
            .tui
            .effective_mode_for_carrier(cli.tui_mode(), terminal_can_paint)
        {
            crate::config::TuiMode::Classic => tui_bridge::TuiMode::Classic,
            crate::config::TuiMode::Inline => tui_bridge::TuiMode::Inline,
            crate::config::TuiMode::Fullscreen => tui_bridge::TuiMode::Fullscreen,
        };
        // A failed geometry probe (not a terminal, or an ioctl refusal) falls
        // back to the conventional 80x24, the same fallback the renderer's own
        // width probe uses.
        let (_cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));
        let installed = tui_bridge::install(
            mode,
            rows,
            1,
            setup.tui.cell_ring_lines,
            "kitsu",
            Box::new(std::io::stdout()),
            Box::new(std::io::stderr()),
        );
        if installed.is_some() || mode == tui_bridge::TuiMode::Fullscreen {
            // Requirement N5. The restore exists to give back what a surface
            // took, and there are two takers at startup, not one.
            //
            // The *inline* viewport takes a pinned DECSTBM region, raw
            // termios and bracketed paste, and `installed.is_some()` is how
            // that is recognised. Classic mode pins no region and holds raw
            // mode only inside rustyline's own readline, so it keeps the
            // signal dispositions it was started with and its byte stream is
            // unchanged, signal or no signal.
            //
            // Card **F96**: fullscreen is the third case and it was missing.
            // `install` is inert in fullscreen -- it pins no region, which is
            // right -- so gating the handler on the *region* left a session
            // started with `--tui fullscreen` with the default disposition
            // and no restore of any kind: no title pop, no alternate-screen
            // release, and no `tcsetattr` back to canonical termios, which is
            // a shell with no echo and no line editing until the user types
            // `reset` blind. The handler is owed wherever terminal state is
            // taken, so the condition names the mode as well as the region.
            tui_bridge::install_exit_restore();
        }
        // `install` is inert in fullscreen -- it pins no region and installs
        // no surface -- so the alternate screen is taken here instead, with
        // an empty frame: a session that starts in fullscreen has no inline
        // segment behind it and therefore no ring to repaint.
        let fullscreen = (mode == tui_bridge::TuiMode::Fullscreen).then(|| {
            tui_bridge::enter_fullscreen(
                Box::new(std::io::stderr()),
                &[],
                rows,
                kitsu_render::live::terminal_columns(),
                setup.tui.cell_ring_lines,
            )
        });
        (installed, fullscreen, title_stack_guard)
    };
    let publish_agent_state = |turn_active: bool| {
        if !agent_state_enabled {
            return;
        }
        let payload = agent_state_file::build(
            &agent_registry,
            agent_state_pid,
            session.directory.display().to_string(),
            session.session_id.borrow().clone(),
            turn_active,
            chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        );
        let _ = agent_state_file::write(&agent_state_dir, agent_state_pid, &payload);
    };
    // Requirement **H2**'s title clause, at the same three boundaries as the
    // state file above and deliberately not sharing its gate: `[tui] title`
    // and the state file are independent keys, and a user may want either
    // alone.
    //
    // `agent_state_file::process_state` is the one and only source of the
    // state word -- the precedence *"blocked outranks working"* stays in its
    // single home -- and the live count is `running + blocked`, which the
    // snapshot documents as disjoint tallies; the terminal, failed and
    // unacknowledged records are finished work and are not live agents.
    //
    // Cadence: these three sites are the session boundary, the top of the idle
    // loop and the start of a turn. Mid-turn changes are republished by the
    // registry-following cadence (`publish_cadence_state_title`), so the title
    // does not go stale while a turn runs.
    //
    // Unix only, like the rest of `tui_bridge`: the title is a terminal escape
    // sequence the bridge writes, and on other targets this is a no-op.
    let publish_title = |turn_active: bool| {
        #[cfg(unix)]
        {
            let snapshot = agent_registry.snapshot();
            tui_bridge::publish_state_title(
                agent_state_file::process_state(snapshot.running, snapshot.blocked, turn_active),
                snapshot.running + snapshot.blocked,
            );
        }
        #[cfg(not(unix))]
        let _ = turn_active;
    };
    // The agent state hook (`[hooks] on_agent_state`, requirement H3). It is
    // independent of the state file above: a user may want the notification
    // without the file, so this is gated only on a usable command. The
    // previous observations live beside the closure, because firing is driven
    // by the change between two boundaries, never by an agent's presence.
    let agent_hook_argv = setup.agent_state_hook.clone();
    let previous_agent_observations: std::cell::RefCell<Vec<agent_hook::AgentObservation>> =
        std::cell::RefCell::new(Vec::new());
    let fire_agent_state_hooks = || {
        let Some(argv) = agent_hook_argv.as_ref() else {
            return;
        };
        let current = agent_hook::observations(&agent_registry);
        let mut previous = previous_agent_observations.borrow_mut();
        for event in agent_hook::transitions(&previous, &current) {
            agent_hook::fire(argv, &event);
        }
        *previous = current;
    };
    // The session's own boundary: idle, before any carrier-specific driver
    // runs. Scripted, piped and one-shot carriers never reach the interactive
    // turn boundaries below, so this is the one hint they publish.
    publish_agent_state(false);
    publish_title(false);
    fire_agent_state_hooks();

    // Scripted mode: run turns from a file non-interactively, then exit. Works
    // regardless of TTY, so it's the deterministic multi-turn driver.
    if let Some(path) = script {
        return run_scripted(
            &setup,
            &provider,
            &tools,
            &mut messages,
            &mut model,
            cli,
            path,
            &perms,
            &write_journal,
            &skillset,
            &session,
            &context_window,
            &live_mw,
            &agent_registry,
            &mut session_store,
            &mut artifact_directories,
            artifact_storage_enabled,
            &stateless_artifact_base,
            allow_turn_errors,
            &required_write_contracts,
        )
        .await;
    }

    // Non-TTY stdin and explicit top-level review → one-shot, no interactive loop.
    if forced_one_shot || !kitsu_render::stdin_is_tty() {
        let one_shot_deadline = tokio::time::Instant::now()
            .checked_add(Duration::from_secs(cli.timeout_secs))
            .ok_or_else(|| CliError::user("--timeout-secs is too large"))?;
        let submitted = match initial_review_submission.take() {
            Some(review) => review,
            None => {
                let prompt = match first_prompt {
                    Some(p) => p.to_string(),
                    None => {
                        let buf = tokio::time::timeout_at(
                            one_shot_deadline,
                            read_piped_stdin_cancellable(),
                        )
                        .await
                        .map_err(|_| {
                            CliError::user(format!(
                                "stdin read exceeded the {}s end-to-end deadline",
                                cli.timeout_secs
                            ))
                        })?
                        .ok_or_else(|| CliError::cancelled("stdin read cancelled by Ctrl+C"))?
                        .map_err(|e| CliError::user(format!("failed to read stdin: {e}")))?;
                        buf.trim().to_string()
                    }
                };
                SubmittedPrompt::full(prompt)
            }
        };
        let prompt = submitted.text.clone();
        if prompt.is_empty() {
            return Err(CliError::user(
                "chat: no input (provide a prompt or pipe text on stdin)".to_string(),
            ));
        }
        if cli.jsonl {
            emit_jsonl(
                "turn_started",
                serde_json::json!({"turn_id": "piped:1", "input": &prompt}),
            )?;
        }
        let (expanded_text, attachment_labels) = if submitted.tool_mode == TurnToolMode::None {
            (prompt.clone(), Vec::new())
        } else {
            match tokio::time::timeout_at(
                one_shot_deadline,
                await_cancellable(attachments::expand(&prompt, &perms)),
            )
            .await
            {
                Ok(Some(Ok(expanded))) => (expanded.text, expanded.labels),
                Ok(Some(Err(error))) => {
                    if !cli.jsonl {
                        eprintln!("attachment error: {}", terminal_inline(&error));
                    }
                    let mapped = CliError::user(error);
                    if cli.jsonl {
                        publish_piped_jsonl_terminal(
                            kitsu_core::RunCleanupState::Joined,
                            "error",
                            0,
                            0,
                            0,
                            || {
                                emit_jsonl(
                                    "error",
                                    serde_json::json!({
                                        "turn_id": "piped:1",
                                        "kind": "attachment",
                                        "message": mapped.to_string(),
                                        "exit_code": mapped.exit_code(),
                                    }),
                                )?;
                                emit_jsonl(
                                    "turn_finished",
                                    serde_json::json!({"turn_id": "piped:1", "status": "error"}),
                                )?;
                                emit_jsonl_summary("error", 1, 0)
                            },
                        )?;
                    }
                    return Err(mapped);
                }
                Ok(None) => {
                    let mapped = CliError::cancelled("attachment preflight cancelled by Ctrl+C");
                    if cli.jsonl {
                        publish_piped_jsonl_terminal(
                            kitsu_core::RunCleanupState::Joined,
                            "cancelled",
                            0,
                            0,
                            0,
                            || {
                                emit_jsonl(
                                    "error",
                                    serde_json::json!({
                                        "turn_id": "piped:1",
                                        "kind": "cancelled",
                                        "message": mapped.to_string(),
                                        "exit_code": mapped.exit_code(),
                                    }),
                                )?;
                                emit_jsonl(
                                    "turn_finished",
                                    serde_json::json!({"turn_id": "piped:1", "status": "cancelled"}),
                                )?;
                                emit_jsonl_summary("cancelled", 1, 0)
                            },
                        )?;
                    }
                    return Err(mapped);
                }
                Err(_) => {
                    let mapped = CliError::provider(format!(
                        "attachment preflight exceeded the {}s end-to-end deadline",
                        cli.timeout_secs
                    ));
                    if cli.jsonl {
                        publish_piped_jsonl_terminal(
                            kitsu_core::RunCleanupState::Joined,
                            "error",
                            1,
                            0,
                            0,
                            || {
                                emit_jsonl(
                                    "error",
                                    serde_json::json!({
                                        "turn_id": "piped:1",
                                        "kind": "deadline",
                                        "message": mapped.to_string(),
                                        "exit_code": mapped.exit_code(),
                                    }),
                                )?;
                                emit_jsonl(
                                    "turn_finished",
                                    serde_json::json!({"turn_id": "piped:1", "status": "error"}),
                                )?;
                                emit_jsonl_summary("error", 1, 1)
                            },
                        )?;
                    }
                    return Err(mapped);
                }
            }
        };
        if !cli.jsonl && !attachment_labels.is_empty() {
            eprintln!("~ attached {} file(s):", attachment_labels.len());
            for label in &attachment_labels {
                eprintln!("  - {}", terminal_inline(label));
            }
        }
        let agent_watermark = agent_registry.watermark();
        let journal_label = if submitted.tool_mode == TurnToolMode::None {
            "verified review"
        } else {
            &prompt
        };
        let journal_turn = write_journal.begin_attributed_turn(journal_label);
        if let Some(previous) = journal_turn.previous {
            report_write_checkpoint(previous);
        }
        messages.push(ChatMessage::user(expanded_text));
        // Baseline for detecting tool progress if the turn later times out: any
        // completed round appends messages past this length.
        let committed_len = messages.len();
        let mut cfg = base_cfg(model.clone(), cli);
        cfg.turn_deadline = Some(one_shot_deadline);
        let partial_answer: PartialAnswerBuffer = Arc::default();
        let event_log =
            live_agent_event_log_capturing(cli.jsonl, "piped:1", Some(partial_answer.clone()));
        cfg.event_log = Some(event_log.clone());
        let no_tools = Registry::new();
        let turn_tools = match submitted.tool_mode {
            TurnToolMode::Full => &tools,
            TurnToolMode::None => &no_tools,
        };
        let outcome = match tokio::time::timeout_at(
            one_shot_deadline,
            write_journal.scope_turn(
                &journal_turn.token,
                run_turn_cancellable(
                    &provider,
                    turn_tools,
                    &mut messages,
                    &cfg,
                    &mut session_state,
                ),
            ),
        )
        .await
        {
            Ok(outcome) => outcome,
            Err(_) => TurnOutcome::Completed(Err(kitsu_core::AgentError::Provider(
                ProviderError::Timeout,
            ))),
        };
        let mut turn_reports = write_journal.drain_and_take_reports().await;
        report_write_checkpoints(turn_reports.clone());
        if let Some(report) = write_journal.finish_attributed_turn(&journal_turn.token) {
            turn_reports.push(report.clone());
            report_write_checkpoint(report);
        }
        let assistant = match &outcome {
            TurnOutcome::Completed(Ok(text)) => Some(text.as_str()),
            _ => None,
        };
        finish_agent_trace(cli, "piped:1", &event_log, assistant)?;
        warn_context_window_evidence(cli, Some("piped:1"), &mut session_state)?;
        let unrecovered_tool_errors = ToolErrorTotals::from_turn(&session_state.last_turn, false);
        return match outcome {
            TurnOutcome::Completed(Ok(text)) => {
                // A piped turn that delegated work joins it the way `/quit
                // wait` does. Only agents that outlive the join bound are
                // cancelled, and only then is the run incomplete.
                let cancelled_at_exit = join_background_agents_at_exit(
                    &agent_registry,
                    &write_journal,
                    agent_join_timeout(cli),
                    cli.jsonl,
                )
                .await;
                let cleanup_state = terminal_cleanup_state(&cancelled_at_exit);
                let background_results = agent_registry.take_unobserved_terminal_results();
                emit_script_background_results(&background_results, cli.jsonl)?;
                let failed_background = background_results
                    .iter()
                    .filter(|result| matches!(result.status, subagent::AgentStatus::Failed(_)))
                    .count();
                let unfinished_agents = cancelled_at_exit.len() + failed_background;
                if unfinished_agents > 0 {
                    finish_assistant_disposition(
                        cli,
                        "piped:1",
                        AssistantDisposition::Discarded,
                        Some("background agents did not complete, so the turn is incomplete"),
                    )?;
                    if !cli.jsonl {
                        emit_run_receipt(
                            &write_journal,
                            &agent_registry,
                            session.session_id.borrow().as_deref(),
                            0,
                            1,
                            session.run_tallies.reads.load(Ordering::Acquire),
                            session.run_tallies.denied.load(Ordering::Acquire),
                            session.run_tallies.failed.load(Ordering::Acquire),
                        );
                    }
                    let mapped = CliError::provider(format!(
                        "piped turn ended with {unfinished_agents} unfinished or failed background agent(s)"
                    ));
                    if cli.jsonl {
                        publish_piped_jsonl_terminal(cleanup_state, "error", 1, 0, 0, || {
                            emit_jsonl(
                                "error",
                                serde_json::json!({
                                    "turn_id": "piped:1",
                                    "kind": "unfinished_background_agents",
                                    "message": mapped.to_string(),
                                    "exit_code": mapped.exit_code(),
                                }),
                            )?;
                            emit_jsonl(
                                "turn_finished",
                                serde_json::json!({"turn_id": "piped:1", "status": "error"}),
                            )?;
                            emit_jsonl_summary_with_local_failures(
                                "error",
                                1,
                                1,
                                0,
                                0,
                                0,
                                unrecovered_tool_errors,
                            )
                        })?;
                    } else {
                        record_human_terminal_facts_preserving_error(cleanup_state);
                    }
                    return Err(mapped);
                }
                if let Some(failure) =
                    strict_turn_failure(&session_state.last_turn, cli.max_output_tokens, &text)
                {
                    // Cycle-30 (queued since the cycle-19 audit): a POLICY
                    // strict failure whose turn still completed a real final
                    // answer commits that answer as PARTIAL — the model was
                    // refused, adapted, and answered truthfully; destroying
                    // the answer punished honesty. The run still FAILS
                    // exactly as before: same exit code, same JSONL error
                    // and turn_finished frames — automation contracts are
                    // unchanged. Provider-kind failures (incomplete turns)
                    // keep the full rollback: there is no real answer to
                    // keep.
                    let commit_partial = failure.kind == ScriptFailureKind::Policy;
                    if commit_partial {
                        finish_assistant_disposition(
                            cli,
                            "piped:1",
                            AssistantDisposition::CommittedPartial,
                            Some(&failure.issue),
                        )?;
                        if let Err(persist_error) =
                            persist_session_required(&session_store, &session, &model, &messages)
                        {
                            if !cli.jsonl {
                                eprintln!(
                                    "~ warning: could not save the partial turn: {}",
                                    terminal_inline(&persist_error.to_string())
                                );
                            }
                        }
                        if !cli.jsonl {
                            eprintln!(
                                "~ the answer above is COMMITTED AS PARTIAL: {}; the run still fails so automation notices",
                                failure.issue
                            );
                        }
                    } else {
                        finish_assistant_disposition(
                            cli,
                            "piped:1",
                            AssistantDisposition::Discarded,
                            Some(&failure.issue),
                        )?;
                        let mut retained = turn_reports;
                        retained.extend(
                            cancel_agents_and_report_writes(
                                &agent_registry,
                                agent_watermark,
                                &write_journal,
                            )
                            .await,
                        );
                        note_rollback_retained_changes(
                            cli,
                            Some("piped:1"),
                            &mut messages,
                            &retained,
                            tools.is_registered("workspace_diff"),
                        )?;
                    }
                    if !cli.jsonl {
                        // Cycle-20 audit: a failing exit still owes the run
                        // receipt — denied/failed tallies especially.
                        emit_run_receipt(
                            &write_journal,
                            &agent_registry,
                            session.session_id.borrow().as_deref(),
                            0,
                            1,
                            session.run_tallies.reads.load(Ordering::Acquire),
                            session.run_tallies.denied.load(Ordering::Acquire),
                            session.run_tallies.failed.load(Ordering::Acquire),
                        );
                    }
                    let mapped = strict_failure_cli_error(&failure);
                    if cli.jsonl {
                        publish_piped_jsonl_terminal(
                            cleanup_state,
                            "error",
                            usize::from(failure.kind.is_provider_failure()),
                            usize::from(failure.kind == ScriptFailureKind::Tool),
                            usize::from(failure.kind == ScriptFailureKind::Policy),
                            || {
                                emit_jsonl(
                                    "error",
                                    failure.jsonl_payload(
                                        "piped:1",
                                        &mapped.to_string(),
                                        &session_state.last_turn,
                                    ),
                                )?;
                                emit_jsonl(
                                    "turn_finished",
                                    serde_json::json!({
                                        "turn_id": "piped:1",
                                        "status": "error",
                                        "tool_errors": session_state.last_turn.tool_errors,
                                        "policy_errors": session_state.last_turn.policy_errors,
                                    }),
                                )?;
                                emit_jsonl_summary_with_local_failures(
                                    "error",
                                    1,
                                    usize::from(failure.kind.is_provider_failure()),
                                    usize::from(failure.kind == ScriptFailureKind::OutputSchema),
                                    usize::from(failure.kind == ScriptFailureKind::Tool),
                                    usize::from(failure.kind == ScriptFailureKind::Policy),
                                    unrecovered_tool_errors,
                                )
                            },
                        )?;
                    } else {
                        record_human_terminal_facts_preserving_error(cleanup_state);
                    }
                    return Err(mapped);
                }
                let recovered_tool_errors =
                    ToolErrorTotals::from_turn(&session_state.last_turn, true);
                if let Err(mapped) =
                    persist_session_required(&session_store, &session, &model, &messages)
                {
                    finish_assistant_disposition(
                        cli,
                        "piped:1",
                        AssistantDisposition::Committed,
                        None,
                    )?;
                    record_successful_operation();
                    if cli.jsonl {
                        publish_piped_jsonl_terminal(cleanup_state, "error", 0, 0, 0, || {
                            emit_jsonl(
                                "error",
                                serde_json::json!({
                                    "turn_id": "piped:1",
                                    "kind": "session",
                                    "message": mapped.to_string(),
                                    "exit_code": mapped.exit_code(),
                                }),
                            )?;
                            emit_jsonl(
                                "turn_finished",
                                serde_json::json!({"turn_id": "piped:1", "status": "error"}),
                            )?;
                            emit_jsonl_summary_with_local_failures(
                                "error",
                                1,
                                0,
                                0,
                                0,
                                0,
                                recovered_tool_errors,
                            )
                        })?;
                    } else {
                        record_human_terminal_facts_preserving_error(cleanup_state);
                    }
                    return Err(mapped);
                }
                finish_assistant_disposition(
                    cli,
                    "piped:1",
                    AssistantDisposition::Committed,
                    None,
                )?;
                record_successful_operation();
                if !cli.jsonl && session_state.last_turn.tool_errors > 0 {
                    eprintln!(
                        "~ {} earlier tool error(s) were recovered within the turn; final state committed",
                        session_state.last_turn.tool_errors
                    );
                }
                if cli.jsonl {
                    emit_jsonl(
                        "turn_finished",
                        serde_json::json!({
                            "turn_id": "piped:1",
                            "status": "ok",
                            "tool_errors": session_state.last_turn.tool_errors,
                            "policy_errors": session_state.last_turn.policy_errors,
                        }),
                    )?;
                }
                let required_write_evaluation =
                    evaluate_required_writes(&required_write_contracts, &perms, &write_journal);
                emit_required_write_evaluation(
                    cli,
                    &required_write_contracts,
                    &required_write_evaluation,
                )?;
                if let Some(mapped) = required_write_evaluation.failure {
                    if cli.jsonl {
                        publish_piped_jsonl_terminal(cleanup_state, "error", 0, 0, 0, || {
                            emit_required_write_error(cli, &mapped)?;
                            emit_jsonl_summary_with_postcondition_failures(
                                "error",
                                1,
                                0,
                                0,
                                1,
                                0,
                                0,
                                recovered_tool_errors,
                            )
                        })?;
                    } else {
                        emit_required_write_error(cli, &mapped)?;
                        record_human_terminal_facts_preserving_error(cleanup_state);
                    }
                    return Err(mapped);
                }
                if let Err(mapped) = write_output_last_message(cli, "piped:1", &text) {
                    if cli.jsonl {
                        publish_piped_jsonl_terminal(cleanup_state, "error", 0, 0, 0, || {
                            emit_output_last_message_error(cli, "piped:1", &mapped)?;
                            emit_jsonl_summary_with_local_failures(
                                "error",
                                1,
                                0,
                                0,
                                0,
                                0,
                                recovered_tool_errors,
                            )
                        })?;
                    } else {
                        emit_output_last_message_error(cli, "piped:1", &mapped)?;
                        record_human_terminal_facts_preserving_error(cleanup_state);
                    }
                    return Err(mapped);
                }
                if !cli.jsonl {
                    emit_run_receipt(
                        &write_journal,
                        &agent_registry,
                        session.session_id.borrow().as_deref(),
                        1,
                        0,
                        session.run_tallies.reads.load(Ordering::Acquire),
                        session.run_tallies.denied.load(Ordering::Acquire),
                        session.run_tallies.failed.load(Ordering::Acquire),
                    );
                }
                if cli.jsonl {
                    publish_piped_jsonl_terminal(cleanup_state, "ok", 0, 0, 0, || {
                        emit_jsonl_summary_with_local_failures(
                            "ok",
                            0,
                            0,
                            0,
                            0,
                            0,
                            recovered_tool_errors,
                        )
                    })?;
                } else {
                    record_human_terminal_facts(cleanup_state)?;
                }
                Ok(())
            }
            TurnOutcome::Completed(Err(error)) => {
                if let Some(partial) = deadline_partial_answer(&error, &partial_answer) {
                    let partial_text = emit_partial_deadline_answer(cli, "piped:1", &partial)?;
                    // Commit the partial answer (with the completed tool
                    // rounds already in `messages`) to a durable session so a
                    // resume can continue from what was actually delivered.
                    messages.push(ChatMessage::assistant(partial_text, Vec::new()));
                    if let Err(persist_error) =
                        persist_session_required(&session_store, &session, &model, &messages)
                    {
                        if !cli.jsonl {
                            eprintln!(
                                "~ warning: could not save the partial turn: {}",
                                terminal_inline(&persist_error.to_string())
                            );
                        }
                    }
                } else if is_deadline_timeout(&error) {
                    // The turn hit the deadline. Trim any dangling final round
                    // (assistant tool_calls whose results never landed) so the
                    // transcript is safe to persist and resume, then decide
                    // whether completed tool work survived worth preserving.
                    truncate_to_resumable_tail(&mut messages);
                    if messages.len() > committed_len {
                        // Completed tool rounds remain: preserve them as a
                        // resumable partial instead of discarding the whole
                        // exploration. Automation (the piped path) has no human
                        // to /retry, so a total-loss discard silently throws away
                        // real work for slow models that pace past the deadline.
                        let empty = snapshot_partial_answer(&partial_answer);
                        let marker = emit_partial_deadline_answer(cli, "piped:1", &empty)?;
                        messages.push(ChatMessage::assistant(marker, Vec::new()));
                        if let Err(persist_error) =
                            persist_session_required(&session_store, &session, &model, &messages)
                        {
                            if !cli.jsonl {
                                eprintln!(
                                    "~ warning: could not save the partial turn: {}",
                                    terminal_inline(&persist_error.to_string())
                                );
                            }
                        }
                    } else {
                        // The deadline fired before any tool round completed:
                        // nothing durable to resume from, so discard as before.
                        finish_assistant_disposition(
                            cli,
                            "piped:1",
                            AssistantDisposition::Discarded,
                            Some("the model turn failed before commit"),
                        )?;
                    }
                } else {
                    finish_assistant_disposition(
                        cli,
                        "piped:1",
                        AssistantDisposition::Discarded,
                        Some("the model turn failed before commit"),
                    )?;
                }
                let cancelled = cancel_agents_and_report_writes_with_cleanup(
                    &agent_registry,
                    agent_watermark,
                    &write_journal,
                )
                .await;
                let cause = agent_error_jsonl_cause(&error);
                let mapped = map_agent_error_with_diagnostics(
                    error,
                    ProviderOperation::Chat { model: &model },
                    Some(cli.timeout_secs),
                    setup.provider_diagnostics,
                );
                if cli.jsonl {
                    publish_piped_jsonl_terminal(
                        cancelled.cleanup,
                        "error",
                        usize::from(mapped.exit_code() == EXIT_PROVIDER),
                        0,
                        0,
                        || {
                            let mut payload = mapped.jsonl_error_payload();
                            payload["turn_id"] = serde_json::json!("piped:1");
                            if let Some(cause) = cause {
                                payload["cause"] = serde_json::json!(cause);
                            }
                            emit_jsonl("error", payload)?;
                            emit_jsonl(
                                "turn_finished",
                                serde_json::json!({"turn_id": "piped:1", "status": "error"}),
                            )?;
                            emit_jsonl_summary_with_local_failures(
                                "error",
                                1,
                                usize::from(mapped.exit_code() == EXIT_PROVIDER),
                                0,
                                0,
                                0,
                                unrecovered_tool_errors,
                            )
                        },
                    )?;
                } else {
                    record_human_terminal_facts_preserving_error(cancelled.cleanup);
                }
                Err(mapped)
            }
            TurnOutcome::Cancelled => {
                finish_assistant_disposition(
                    cli,
                    "piped:1",
                    AssistantDisposition::Discarded,
                    Some("the model turn was cancelled before commit"),
                )?;
                let cancelled = cancel_agents_and_report_writes_with_cleanup(
                    &agent_registry,
                    agent_watermark,
                    &write_journal,
                )
                .await;
                let mapped = CliError::cancelled("request cancelled by Ctrl+C");
                if cli.jsonl {
                    publish_piped_jsonl_terminal(cancelled.cleanup, "cancelled", 0, 0, 0, || {
                        emit_jsonl(
                            "error",
                            serde_json::json!({
                                "turn_id": "piped:1",
                                "kind": "cancelled",
                                "message": mapped.to_string(),
                                "exit_code": mapped.exit_code(),
                            }),
                        )?;
                        emit_jsonl(
                            "turn_finished",
                            serde_json::json!({"turn_id": "piped:1", "status": "cancelled"}),
                        )?;
                        emit_jsonl_summary_with_local_failures(
                            "cancelled",
                            1,
                            0,
                            0,
                            0,
                            0,
                            unrecovered_tool_errors,
                        )
                    })?;
                } else {
                    record_human_terminal_facts_preserving_error(cancelled.cleanup);
                }
                Err(mapped)
            }
        };
    }

    // Interactive TTY loop. Bracketed paste keeps rich-mode multi-line input as
    // one turn. Literal plain/screen-reader input must not make Rustyline emit
    // the bracketed-paste terminal control sequences those modes suppress.
    // NO_COLOR only changes rendering and deliberately keeps the rich editor.
    let assistive_input = kitsu_render::assistive_input_mode();
    let assistive_signal_task = assistive_input.then(|| {
        tokio::spawn(async {
            loop {
                if tokio::signal::ctrl_c().await.is_err() {
                    break;
                }
                ASSISTIVE_INPUT_INTERRUPTED.store(true, Ordering::Release);
            }
        })
    });
    let rl_config = rustyline::Config::builder()
        .bracketed_paste(bracketed_paste_enabled(assistive_input))
        .completion_type(rustyline::CompletionType::List)
        .build();
    let mut rl: rustyline::Editor<ChatHelper, rustyline::history::DefaultHistory> =
        rustyline::Editor::with_config(rl_config)
            .map_err(|e| CliError::user(format!("failed to start input editor: {e}")))?;
    // Complete slash commands without changing the visible input-buffer width.
    rl.set_helper(Some(ChatHelper {
        completion: ComposerCompletion::at_launch_directory(perms.clone()),
        rich_palette: !kitsu_render::plain_mode(),
        live_model_window: live_mw.clone(),
        directory: shorten_home(&session.directory),
    }));
    let foreground_typeahead_active = Arc::new(AtomicBool::new(false));
    let (deferred_notice_sender, deferred_notice_receiver) = std::sync::mpsc::channel::<String>();
    // A prompt cannot be redrawn without cursor-control sequences. In
    // assistive mode, defer notices until the submitted line returns instead
    // of letting an asynchronous printer split partially typed input.
    // DEFAULT: no rustyline ExternalPrinter. Registering one degrades the
    // idle prompt's input processing to ~1 byte per event-loop wakeup on
    // this rustyline (14 through 18, empirically bisected cycle 14 —
    // burst input like pastes crawls; the fox's founding complaint). The
    // printer-less fallback below prints agent notices between prompt
    // cycles instead of live at idle; automation-first kitsu prefers
    // sacred input latency. KITSU_LIVE_NOTICES=1 opts back into the
    // printer for sessions that want live idle notices and accept the
    // input cost.
    let agent_notification_task = (!assistive_input
        && std::env::var_os("KITSU_LIVE_NOTICES").is_some())
    .then(|| rl.create_external_printer().ok())
    .flatten()
    .map(|mut printer| {
        let mut changes = agent_registry.subscribe();
        let notification_journal = write_journal.clone();
        let typeahead_active = foreground_typeahead_active.clone();
        let deferred = deferred_notice_sender.clone();
        tokio::spawn(async move {
            let mut previous = *changes.borrow_and_update();
            while changes.changed().await.is_ok() {
                let current = *changes.borrow_and_update();
                if let Some(message) = background_agent_notice(previous, current) {
                    let notice = kitsu_render::PromptSafeAsyncNotice::new(
                        background_agent_notice_level(previous, current),
                        message,
                    );
                    if typeahead_active.load(Ordering::Acquire)
                        || notice.publish_with(|frame| printer.print(frame)).is_err()
                    {
                        let _ = deferred.send(notice.line());
                    }
                }
                if current.terminal > previous.terminal {
                    for report in notification_journal.drain_and_take_reports().await {
                        let notice = kitsu_render::PromptSafeAsyncNotice::info(
                            format_write_checkpoint(&report, undo_hint_actionable()),
                        );
                        if typeahead_active.load(Ordering::Acquire)
                            || notice.publish_with(|frame| printer.print(frame)).is_err()
                        {
                            let _ = deferred.send(notice.line());
                        }
                    }
                }
                previous = current;
            }
        })
    });
    let agent_notifications_live = agent_notification_task.is_some();
    let history_path = setup.state_dir.join("history");
    if !no_session {
        let _ = rl.load_history(&history_path);
    }

    kitsu_render::chat_banner(
        &session.gateway,
        &model,
        &shorten_home(&session.directory),
        &session.tool_names,
    );

    // Kitsu owns submitted type-ahead in a bounded FIFO instead of leaving it as
    // invisible terminal buffering. The kernel retains any lines beyond the
    // bound until a queue slot opens, so overflow is never silently discarded.
    let prompt_queue = Arc::new(std::sync::Mutex::new(prompt_queue::PromptQueue::new(
        PROMPT_QUEUE_CAPACITY,
    )));
    let steering =
        prompt_queue::SteeringChannel::new(STEERING_CAPACITY).expect("positive steering capacity");
    if let Some(first) = first_prompt {
        prompt_queue
            .lock()
            .expect("prompt queue")
            .enqueue(first)
            .expect("the initial prompt fits an empty queue");
    }
    // Requirement **R5**: the inline viewport's pinned row becomes the
    // composed status row now that everything it counts exists. The queue and
    // the registry are read through a closure rather than copied in, because
    // the main loop drains the queue *between* turns -- a depth captured here
    // would be stale exactly when the idle row is painted.
    //
    // Built whether or not a viewport is installed, and kept for the whole
    // session: a classic session that later types `/tui inline` needs the
    // fields to hand the new guard, and composing them here is the only way
    // the re-install can publish a row that already carries the live model
    // instead of `install`'s fixed fallback. Publishing it is the guard's
    // half, and is redone on every install (see the `/tui` arm below).
    #[cfg(unix)]
    let status_source = {
        let queue = prompt_queue.clone();
        let registry = agent_registry.clone();
        let source = tui_bridge::StatusSource::new(Box::new(move || {
            let agents = registry.snapshot();
            tui_bridge::StatusCounts {
                queued: queue.lock().expect("prompt queue").status().queued,
                blocked: agents.blocked,
                running: agents.running,
                ..Default::default()
            }
        }));
        refresh_status_usage(Some(&source), &model, &messages, &session, &session_state);
        if let Some(guard) = tui_bridge_guard.as_ref() {
            guard.enable_live_status(source.clone());
        }
        Some(source)
    };
    // Card **T4-S8**, requirement **A8**, finding **F81**: the **idle**
    // composer's ambient cadence, and the only one it has.
    //
    // During a turn `run_live_typeahead` repaints once per
    // `AMBIENT_TICK_POLL_MS`. Between turns `read_line_inline_with_keys`
    // blocks in `read()` with no timeout, so a background agent that blocks
    // while the user sits at the prompt used to change the registry and
    // change nothing on screen until the next keystroke, resize or submitted
    // line -- which is exactly what the blocked-agent surfacing exists to
    // prevent, and what `tui_bridge.rs` already said in as many words: the
    // ambient tag is "state and not animation", so freezing it hides a
    // blocked agent.
    //
    // A subscriber, not a rewrite: the counts are a pull read through the
    // closure above at paint time, so the watch channel only has to trigger a
    // paint. Ungated on purpose -- the `KITSU_LIVE_NOTICES` notifier below is
    // opt-in and assistive-gated, which is why a default TUI session had no
    // subscriber at all. Held for the session; the task ends when the
    // registry's sender drops.
    // Card **H2-cadence**: the same subscriber republishes requirement H2's
    // terminal title, which the three `publish_title` sites leave stale
    // mid-turn. The turn flag is the one the turn boundaries already set two
    // lines from `publish_title(true)`/the turn's end, so the cadence's word
    // is the word those sites would publish.
    #[cfg(unix)]
    let title_turn_active = foreground_typeahead_active.clone();
    #[cfg(unix)]
    let _idle_ambient_cadence = tui_bridge::spawn_idle_ambient_cadence(
        agent_registry.subscribe(),
        |snapshot: &subagent::AgentRegistrySnapshot| (snapshot.blocked, snapshot.running),
        move |blocked, running| {
            tui_bridge::publish_cadence_state_title(
                blocked,
                running,
                title_turn_active.load(Ordering::Acquire),
            );
        },
    );
    let mut exit_after_queue = false;
    // Session transitions are control-plane actions, not user prompts. They
    // always run before queued model work and never consume bounded queue
    // capacity (notably the immediate switch after `/fork`).
    let mut priority_command: Option<String> = None;
    let prompt = kitsu_render::prompt_string();
    // Fallback for terminals where Rustyline cannot create an external printer.
    let mut last_agent_snapshot = agent_registry.snapshot();
    let mut last_failed: Option<SubmittedPrompt> = None;
    // Type-ahead the user was still composing when a turn ended; pre-filled
    // into the next live editor or idle prompt so typing is never dropped.
    let mut pending_draft = String::new();
    // Card **T4-S6a**, requirement A3/A5: the background agent this
    // composer is routed to, and the session's only home for it. The agent
    // view's `Enter`/`\u{2192}` sets it (through `apply_agent_view_exit`),
    // `\u{2190}` on an empty composer and `Ctrl+X d` clear it, and the
    // composer's prompt label is derived from it on every read. It lives here
    // because it must outlive both the overlay, which is per-invocation, and
    // one inline read, which returns on every submitted line (**F75**).
    // (The inline reader is the only thing that can attach, and the classic
    // reader on other platforms has no agent view at all — but `handle_slash`
    // takes the binding on every platform, where `/detach` then correctly
    // answers that nothing is attached.)
    let mut attached_agent: Option<u64> = None;
    let no_tools = Registry::new();
    let mut run_aggregate = InteractiveRunAggregate::default();

    loop {
        // Turn boundary: the previous turn has ended and nothing is running, so
        // the hint is refreshed as idle before the next prompt is collected.
        publish_agent_state(false);
        publish_title(false);
        fire_agent_state_hooks();
        let agent_snapshot = agent_registry.snapshot();
        if !agent_notifications_live {
            if let Some(message) = background_agent_notice(last_agent_snapshot, agent_snapshot) {
                eprintln!("{message}");
            }
            if agent_snapshot.terminal > last_agent_snapshot.terminal {
                report_write_checkpoints(write_journal.take_unreported_reports());
            }
            last_agent_snapshot = agent_snapshot;
        }
        let visible_prompt = if session.autosave_dirty.get() {
            format!("[UNSAVED] {prompt}")
        } else {
            prompt.clone()
        };
        let prioritized = priority_command.take();
        // **F77**, carried to the feed below: a priority command is kitsu's
        // own words (`/resume` after a fork, `/retry`), and the ring is the
        // transcript the overlay reads. Captured here because `prioritized` is
        // consumed by the `match` that builds `line`, so by the feed point the
        // provenance is no longer recoverable -- and it is not recoverable from
        // the text either, since the user can type `/retry` themselves.
        let line_is_injected = prioritized.is_some();
        let queued_line = if prioritized.is_none() {
            prompt_queue.lock().expect("prompt queue").dequeue()
        } else {
            None
        };
        let line = match (prioritized, queued_line) {
            (Some(command), _) => command,
            (None, Some(queued)) => {
                // Queue bookkeeping is only worth narrating while a backlog
                // remains; draining the final item stays silent.
                let status = prompt_queue.lock().expect("prompt queue").status();
                if status.queued > 0 {
                    eprintln!(
                        "~ running queued prompt ({} remaining; {} queue slot(s) available)",
                        status.queued,
                        status.remaining()
                    );
                }
                queued
            }
            (None, None) if exit_after_queue => {
                debug_assert!(prompt_queue
                    .lock()
                    .expect("prompt queue")
                    .status()
                    .is_empty());
                let cleanup = await_background_agents(
                    &agent_registry,
                    "end of input never discards detached work",
                )
                .await;
                run_aggregate.observe_cleanup(cleanup);
                break;
            }
            (None, None) => {
                let initial = std::mem::take(&mut pending_draft);
                // Finding **F97**: the fullscreen frame follows the ring, and
                // the idle prompt is where it catches up. A fullscreen session
                // writes its transcript onto the alternate screen as it
                // streams, so the frame is stale by exactly the lines that
                // arrived since the last paint; repainting here -- no turn
                // running, nothing else writing -- puts the retained lines
                // back above them. A prompt that follows no new output
                // repaints nothing.
                #[cfg(unix)]
                if let Some(segment) = fullscreen_segment.as_mut() {
                    let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));
                    segment.repaint_if_the_ring_grew(rows, cols as usize);
                }
                // The idle row's meter reflects the history as it stands now,
                // after the previous turn committed or rolled back.
                #[cfg(unix)]
                refresh_status_usage(
                    status_source.as_ref(),
                    &model,
                    &messages,
                    &session,
                    &session_state,
                );
                // Inline mode replaces rustyline on this path rather than
                // adding to it: two composers drawing a draft at once would
                // put one copy inside the scroll region and one on the pinned
                // row.
                #[cfg(unix)]
                let read = read_idle_chat_line(
                    &mut rl,
                    tui_bridge_guard.as_ref(),
                    fullscreen_segment.as_mut(),
                    &agent_registry,
                    &visible_prompt,
                    assistive_input,
                    &initial,
                    &mut attached_agent,
                    view_dispatcher.as_ref(),
                );
                #[cfg(not(unix))]
                let read = read_chat_line(&mut rl, &visible_prompt, assistive_input, &initial);
                match read {
                    ChatReadline::Line(line) => line,
                    ChatReadline::Eof => {
                        let running = agent_registry.running();
                        if running > 0 {
                            eprintln!(
                            "exit waiting for {running} background agent(s); detached work will not be discarded"
                        );
                            exit_after_queue = true;
                            continue;
                        }
                        break;
                    }
                    ChatReadline::Interrupted => {
                        eprintln!("^C (input cleared; Ctrl+D or /quit exits)");
                        continue;
                    }
                    ChatReadline::Error(error) => {
                        run_aggregate.record_non_success(kitsu_core::RunOutcomeClass::UserFault);
                        eprintln!("input error: {error}");
                        break;
                    }
                }
            }
        };
        let line = line.trim().to_string();
        if line.is_empty() {
            continue;
        }
        if !no_session {
            let _ = rl.add_history_entry(&line);
        }
        // **F102 / UI bug**: the inline composer paints the submitted line and
        // erases it on Enter, so without an echo here the user never sees what
        // they sent and the transcript jumps straight to the agent's reply.
        // This is the one place every route a submitted line can take
        // converges: the three idle readers through `ChatReadline`, a queued
        // prompt and a priority command through the `line` binding above. It is
        // after the empty-line skip and beside `add_history_entry` deliberately
        // -- what is worth remembering in history is worth echoing and retaining
        // in the ring -- and before the slash dispatch and the turn, so the
        // prompt is on screen and in the ring ahead of anything it provokes.
        // Echoes bytes only on a surface whose composer erased the line
        // (inline/fullscreen); classic's kernel echo already showed it.
        #[cfg(unix)]
        {
            let origin = if line_is_injected {
                tui_bridge::PromptOrigin::Injected
            } else {
                tui_bridge::PromptOrigin::User
            };
            // A slash command is a local UI action with its own visible output
            // (a receipt, or an overlay that repaints the screen), so it is
            // recorded in the ring but NOT echoed into the transcript -- echoing
            // it would double a `/context` report and break an overlay's
            // restore-the-screen invariant. An ordinary turn is the user's
            // words to the agent, and the inline composer erased them, so it is
            // echoed above the answer it provokes.
            if line.starts_with('/') {
                tui_bridge::note_submitted_prompt(&line, origin);
            } else {
                tui_bridge::echo_submitted_prompt(&visible_prompt, &line, origin);
            }
        }
        let mut submitted = SubmittedPrompt::full(line);

        if let Some(rest) = submitted.text.strip_prefix('/') {
            let slash_name = rest.split_whitespace().next().unwrap_or("").to_string();
            let durable_model_before = model.clone();
            let durable_messages_before = messages.clone();
            let command_runtime_log = (slash_name == "compact")
                .then(|| live_command_event_log(false, 0, &submitted.text));
            let result = handle_slash(
                rest,
                &mut messages,
                &mut model,
                &provider,
                &tools,
                cli,
                &perms,
                &write_journal,
                &session,
                &skillset,
                &context_window,
                setup.context_window_pinned,
                &mut session_state,
                &agent_registry,
                command_runtime_log.as_ref(),
                &mut attached_agent,
                // Card **T4-S7**: `/jobs view` is the agent view's second
                // route, and it dispatches through the same closure the
                // composer's route does.
                view_dispatcher.as_ref(),
            )
            .await;
            // Keep subagents in sync with any `/model` change (which may have
            // swapped the model and re-discovered the window).
            *live_mw.lock().expect("live model/window") = (model.clone(), context_window.get());
            match result {
                SlashResult::Continue(_) => {
                    if model != durable_model_before || messages != durable_messages_before {
                        persist_session(&session_store, &session, &model, &messages);
                    }
                    continue;
                }
                SlashResult::UserFailure(message) => {
                    eprintln!("{}", terminal_inline(&message));
                    continue;
                }
                SlashResult::Error => continue,
                SlashResult::ProviderFailure(_) => {
                    run_aggregate.record_non_success(kitsu_core::RunOutcomeClass::ProviderFault);
                    continue;
                }
                SlashResult::Cancelled => {
                    run_aggregate.record_non_success(kitsu_core::RunOutcomeClass::Cancelled);
                    continue;
                }
                SlashResult::Quit(cleanup) => {
                    run_aggregate.observe_cleanup(cleanup);
                    let running = agent_registry.running();
                    if running > 0 {
                        eprintln!(
                            "exit blocked: {running} background agent(s) still running; use /agents and wait for them to finish"
                        );
                        continue;
                    }
                    report_write_checkpoints(write_journal.drain_and_take_reports().await);
                    break;
                }
                SlashResult::SwitchTui(request) => {
                    #[cfg(unix)]
                    {
                        match request {
                            TuiRendererRequest::Classic => {
                                if fullscreen_segment.is_some() {
                                    // Dropping the segment **is** the
                                    // teardown here too: the alternate screen
                                    // is given back and the title popped, in
                                    // that order, by the one `Drop` every
                                    // other exit path runs as well. The
                                    // terminal is then whole -- this session
                                    // owns no pinned row, no scroll region,
                                    // no alternate screen and no title of its
                                    // own.
                                    fullscreen_segment = None;
                                    eprintln!(
                                        "~ switched to the {} renderer; the conversation is unchanged",
                                        request.label()
                                    );
                                } else if tui_bridge_guard.is_some() {
                                    // Dropping the guard **is** the teardown,
                                    // in the order earlier cards established:
                                    // the status painter is unpublished, then
                                    // the modal painter, then the surface and
                                    // the live-row flag are put back, and only
                                    // then is the viewport hidden. There is no
                                    // second teardown path here on purpose.
                                    tui_bridge_guard = None;
                                    eprintln!(
                                        "~ switched to the {} renderer; the conversation is unchanged",
                                        request.label()
                                    );
                                } else {
                                    eprintln!(
                                        "~ already using the {} renderer; nothing changed",
                                        request.label()
                                    );
                                }
                            }
                            TuiRendererRequest::Inline => {
                                if tui_bridge_guard.is_some() {
                                    eprintln!(
                                        "~ already using the {} renderer; nothing changed",
                                        request.label()
                                    );
                                } else {
                                    // Out of fullscreen first, when that is
                                    // where this session was: the alternate
                                    // screen has to be given back *before*
                                    // the viewport pins a region and writes
                                    // its pinned row, or the user's
                                    // transcript lands on a screen that is
                                    // about to be discarded. A terminal too
                                    // short for a region then leaves the
                                    // session on classic, which the message
                                    // below says -- the screen is given back
                                    // either way.
                                    fullscreen_segment = None;
                                    // The geometry is probed again rather than
                                    // remembered: the terminal may have been
                                    // resized during the classic segment, when
                                    // no viewport was listening for SIGWINCH.
                                    let (_cols, rows) =
                                        crossterm::terminal::size().unwrap_or((80, 24));
                                    tui_bridge_guard = tui_bridge::install(
                                        tui_bridge::TuiMode::Inline,
                                        rows,
                                        1,
                                        setup.tui.cell_ring_lines,
                                        "kitsu",
                                        Box::new(std::io::stdout()),
                                        Box::new(std::io::stderr()),
                                    );
                                    match tui_bridge_guard.as_ref() {
                                        Some(guard) => {
                                            // `Once`-gated, so a second install
                                            // registers nothing -- which is
                                            // correct: the handler holds no
                                            // viewport handle at all, only two
                                            // atomics and a `const` byte
                                            // string, so the restore the first
                                            // install registered gives back
                                            // exactly what this one took.
                                            tui_bridge::install_exit_restore();
                                            // Requirement **R5** again, and the
                                            // trap this card had to avoid: the
                                            // fields are published once per
                                            // *install*, not once per session.
                                            // A re-install that skipped this
                                            // would leave `install`'s fixed
                                            // "kitsu" fallback on the pinned
                                            // row for the rest of the session.
                                            refresh_status_usage(
                                                status_source.as_ref(),
                                                &model,
                                                &messages,
                                                &session,
                                                &session_state,
                                            );
                                            if let Some(source) = status_source.as_ref() {
                                                guard.enable_live_status(source.clone());
                                            }
                                            eprintln!(
                                                "~ switched to the {} renderer; the conversation is unchanged",
                                                request.label()
                                            );
                                        }
                                        // Finding F19's gate declined: the
                                        // terminal is too short for a region.
                                        // Nothing was displaced, so the
                                        // classic renderer simply stays.
                                        None => eprintln!(
                                            "the inline renderer needs a terminal at least 3 rows tall; staying on the classic renderer"
                                        ),
                                    }
                                }
                            }
                            TuiRendererRequest::Fullscreen => {
                                if fullscreen_segment.is_some() {
                                    eprintln!(
                                        "~ already using the {} renderer; nothing changed",
                                        request.label()
                                    );
                                } else {
                                    // Requirement **N4**: the ring is the
                                    // inline viewport's scrollback, and it
                                    // belongs to the surface the teardown is
                                    // about to put back -- so it is read
                                    // *before* the guard is dropped. Out of
                                    // classic there is no guard and therefore
                                    // no ring, and the frame is empty: classic
                                    // feeds nothing, so there is nothing to
                                    // repaint and nothing is painted.
                                    let retained = tui_bridge_guard
                                        .as_ref()
                                        .and_then(tui_bridge::BridgeGuard::cell_lines)
                                        .unwrap_or_default();
                                    // The inline teardown, in its established
                                    // order and through the one path that
                                    // runs it, before any byte reaches the
                                    // alternate screen.
                                    tui_bridge_guard = None;
                                    // Probed again, for the reason the inline
                                    // arm gives: the terminal may have been
                                    // resized while no viewport was listening
                                    // for SIGWINCH.
                                    let (cols, rows) =
                                        crossterm::terminal::size().unwrap_or((80, 24));
                                    fullscreen_segment = Some(tui_bridge::enter_fullscreen(
                                        Box::new(std::io::stderr()),
                                        &retained,
                                        rows,
                                        cols as usize,
                                        setup.tui.cell_ring_lines,
                                    ));
                                    // Finding **F94**'s third variant, found
                                    // by this card: *this* route into
                                    // fullscreen registered no dying-signal
                                    // restore at all. The two that did are
                                    // the inline segment's `install` (a
                                    // switch out of inline) and the startup
                                    // block's mode check (`--tui fullscreen`,
                                    // card **F96**) -- so a session that
                                    // reached fullscreen out of **classic**
                                    // held the alternate screen on the
                                    // default disposition, and now holds raw
                                    // termios on it too, because this
                                    // renderer has a composer of its own.
                                    // `Once`-gated, so this is a no-op in the
                                    // sessions that already registered it.
                                    tui_bridge::install_exit_restore();
                                    eprintln!(
                                        "~ switched to the {} renderer; the conversation is unchanged",
                                        request.label()
                                    );
                                }
                            }
                        }
                    }
                    #[cfg(not(unix))]
                    {
                        let _ = request;
                        eprintln!("the renderer switch is available on unix terminals only");
                    }
                    continue;
                }
                SlashResult::SubmitPrompt(prompt) => submitted = prompt,
                SlashResult::QueueAction(command) => {
                    let mut queue = prompt_queue.lock().expect("prompt queue");
                    apply_queue_command(&mut queue, command);
                    continue;
                }
                SlashResult::Retry => {
                    let Some(prompt) = last_failed.clone() else {
                        eprintln!("nothing to retry");
                        continue;
                    };
                    eprintln!("~ retrying the last failed prompt");
                    submitted = prompt;
                }
                SlashResult::ClearSession => {
                    let running = agent_registry.running();
                    if running > 0 {
                        eprintln!(
                            "clear blocked while {running} background agent(s) are running; use /agents and wait for them to finish"
                        );
                        continue;
                    }
                    run_aggregate.record_background_snapshot(&agent_registry.progress_snapshot());
                    surface_unobserved_background_results(&agent_registry);
                    while deferred_notice_receiver.try_recv().is_ok() {}
                    match clear_session_boundary(
                        &mut session_store,
                        &session,
                        &model,
                        &mut messages,
                        &mut session_state,
                        &agent_registry,
                        &write_journal,
                        &mut artifact_directories,
                        artifact_storage_enabled,
                        &stateless_artifact_base,
                    )
                    .await
                    {
                        Ok(receipt) => {
                            last_failed = None;
                            eprintln!("{}", kitsu_render::milestone_line(&receipt.human_summary()));
                            if let Some(warning) = receipt.artifact_warning.as_deref() {
                                eprintln!(
                                    "warning: private artifact storage disabled after clear: {warning}"
                                );
                            }
                        }
                        Err(error) => eprintln!("{error}"),
                    }
                    continue;
                }
                SlashResult::NewSession => {
                    if agent_registry.running() > 0 {
                        eprintln!(
                            "new session blocked while background agents are running; use /agents and wait for them to finish"
                        );
                        continue;
                    }
                    run_aggregate.record_background_snapshot(&agent_registry.progress_snapshot());
                    surface_unobserved_background_results(&agent_registry);
                    while deferred_notice_receiver.try_recv().is_ok() {}
                    if !persist_started_session(&session_store, &session, &model, &messages) {
                        eprintln!(
                            "new session cancelled because the current session could not be saved"
                        );
                        continue;
                    }
                    report_write_checkpoints(write_journal.drain_and_take_reports().await);
                    if let Some(store) = session_store.as_mut() {
                        if let Err(error) = store.reset() {
                            eprintln!(
                                "new session cancelled because its autosave lease could not be created: {error}"
                            );
                            continue;
                        }
                        *session.session_id.borrow_mut() = Some(store.id().to_string());
                    }
                    messages = vec![fresh_system_message.clone()];
                    session_state = kitsu_core::AgentSessionState::default();
                    last_failed = None;
                    agent_registry.reset();
                    write_journal.reset();
                    *session.project_context.borrow_mut() = fresh_project_context_source.clone();
                    *session.provenance.borrow_mut() = fresh_provenance.clone();
                    perms.reset_runtime_grants();
                    reset_command_session(command_policy.as_ref(), &explicit_command_grants);
                    if let Err(error) = rotate_artifact_storage(
                        &perms,
                        &session.offload_dir,
                        command_artifact_sink.as_ref(),
                        || {
                            artifact_directory_for_boundary(
                                &mut artifact_directories,
                                artifact_storage_enabled,
                                session_store.as_ref(),
                                &stateless_artifact_base,
                            )
                        },
                    ) {
                        eprintln!("warning: private artifact storage disabled: {error}");
                    }
                    eprintln!("{}", kitsu_render::milestone_line(&format!(
                        "started new session {} (runtime grants reset; invocation grants retained: {} read, {} write)",
                        session
                            .session_id
                            .borrow()
                            .as_deref()
                            .unwrap_or("(autosave disabled)"),
                        perms.base_read_roots().len(),
                        perms.base_write_roots().len(),
                    )));
                    continue;
                }
                SlashResult::ResumeSession(selector) => {
                    if agent_registry.running() > 0 {
                        eprintln!(
                            "resume blocked while background agents are running; use /agents and wait for them to finish"
                        );
                        continue;
                    }
                    let active_id = session.session_id.borrow().clone();
                    let read_only_saved;
                    let staged_resume;
                    let saved = if no_session {
                        staged_resume = None;
                        read_only_saved = Some(
                            match session::SessionStore::load_other(
                                &sessions_dir,
                                &selector,
                                active_id.as_deref(),
                            ) {
                                Ok(saved) => saved,
                                Err(error) => {
                                    eprintln!("cannot resume chat: {error}");
                                    continue;
                                }
                            },
                        );
                        read_only_saved.as_ref().expect("read-only resume value")
                    } else {
                        read_only_saved = None;
                        staged_resume = Some(
                            match session::ActiveSessionStore::resume_other_bound_current_v1(
                                sessions_dir.clone(),
                                &selector,
                                active_id.as_deref(),
                            ) {
                                Ok(staged) => staged,
                                Err(error) => {
                                    eprintln!("cannot resume chat: {error}");
                                    continue;
                                }
                            },
                        );
                        staged_resume.as_ref().expect("staged resume").saved()
                    };
                    if saved.gateway != session.gateway {
                        eprintln!(
                            "cannot resume session '{}': it uses gateway '{}', not '{}'",
                            saved.id,
                            terminal_inline(&saved.gateway),
                            terminal_inline(&session.gateway)
                        );
                        continue;
                    }
                    if !same_directory(&saved.directory, &session.directory) {
                        eprintln!(
                            "cannot resume session '{}' from {} while Kitsu is running in {}; change to the saved directory first",
                            saved.id,
                            shorten_home(std::path::Path::new(&saved.directory)),
                            shorten_home(&session.directory)
                        );
                        continue;
                    }
                    if !matches!(
                        saved.messages.first().map(|message| message.role),
                        Some(kitsu_providers::Role::System)
                    ) {
                        eprintln!(
                            "cannot resume session '{}': missing system context",
                            saved.id
                        );
                        continue;
                    }
                    run_aggregate.record_background_snapshot(&agent_registry.progress_snapshot());
                    surface_unobserved_background_results(&agent_registry);
                    while deferred_notice_receiver.try_recv().is_ok() {}
                    if !persist_started_session(&session_store, &session, &model, &messages) {
                        eprintln!(
                            "resume cancelled because the current session could not be saved"
                        );
                        continue;
                    }
                    report_write_checkpoints(write_journal.drain_and_take_reports().await);
                    let resumed_model = saved.model.clone();
                    let resumed_window = if setup.context_window_pinned {
                        context_window.get()
                    } else {
                        let Some(window) =
                            await_cancellable(provider.model_context_length(&resumed_model)).await
                        else {
                            eprintln!("^C (session context-window discovery cancelled)");
                            continue;
                        };
                        match window {
                            Ok(window) => window,
                            Err(error) => {
                                eprintln!(
                                    "cannot discover session context window: {}",
                                    safe_provider_error_message(
                                        &error,
                                        ProviderOperation::ModelCatalog,
                                        Some(cli.timeout_secs),
                                        setup.provider_diagnostics,
                                    )
                                );
                                continue;
                            }
                        }
                    };
                    let expected_generation = match provider.bind() {
                        Ok(route) => route.identity().generation(),
                        Err(error) => {
                            eprintln!(
                                "cannot stage resumed model route: {}",
                                safe_provider_error_message(
                                    &error.into(),
                                    ProviderOperation::Chat {
                                        model: &resumed_model,
                                    },
                                    Some(cli.timeout_secs),
                                    setup.provider_diagnostics,
                                )
                            );
                            continue;
                        }
                    };
                    if let Err(error) =
                        provider.replace_model(expected_generation, resumed_model.clone())
                    {
                        eprintln!(
                            "cannot publish resumed model route: {}",
                            safe_provider_error_message(
                                &error.into(),
                                ProviderOperation::Chat {
                                    model: &resumed_model,
                                },
                                Some(cli.timeout_secs),
                                setup.provider_diagnostics,
                            )
                        );
                        continue;
                    }
                    let (resumed_store, saved) = match staged_resume {
                        Some(staged) => {
                            let (store, saved) = staged.into_parts();
                            (Some(store), saved)
                        }
                        None => (
                            None,
                            read_only_saved.expect("read-only resume value after validation"),
                        ),
                    };
                    model = resumed_model;
                    context_window.set(resumed_window);
                    if !setup.context_window_pinned {
                        *session
                            .window_provenance
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner) =
                            ContextWindowProvenance::live_discovery(resumed_window);
                    }
                    if let Some(store) = resumed_store {
                        session_store = Some(store);
                        *session.session_id.borrow_mut() = Some(saved.id.clone());
                    }
                    perms.reset_runtime_grants();
                    reset_command_session(command_policy.as_ref(), &explicit_command_grants);
                    if let Err(error) = rotate_artifact_storage(
                        &perms,
                        &session.offload_dir,
                        command_artifact_sink.as_ref(),
                        || {
                            artifact_directory_for_boundary(
                                &mut artifact_directories,
                                artifact_storage_enabled,
                                session_store.as_ref(),
                                &stateless_artifact_base,
                            )
                        },
                    ) {
                        eprintln!("warning: private artifact storage disabled: {error}");
                    }
                    *session.project_context.borrow_mut() = fresh_project_context_source.clone();
                    *session.provenance.borrow_mut() = fresh_provenance.clone();
                    if no_project && saved.project_context.is_some() {
                        eprintln!(
                            "~ omitted saved project context because --no-project-context is active"
                        );
                    }
                    let system_context_changed =
                        saved.messages.first() != Some(&fresh_system_message);
                    messages = restored_messages(&saved, &fresh_system_message);
                    let skill_changes = refresh_loaded_skill_messages(&mut messages, &skillset);
                    if system_context_changed || !skill_changes.is_empty() {
                        eprintln!(
                            "~ rebuilt saved system context from current tools, project metadata, and bounded skill provenance"
                        );
                    }
                    for change in skill_changes {
                        eprintln!("~ {}", terminal_inline(&change));
                    }
                    session_state = kitsu_core::AgentSessionState::default();
                    last_failed = None;
                    agent_registry.reset();
                    write_journal.reset();
                    *live_mw.lock().expect("live model/window") =
                        (model.clone(), context_window.get());
                    eprintln!("{}", kitsu_render::milestone_line(&format!(
                        "resumed {} (saved grants were not restored; runtime grants reset; invocation grants retained: {} read, {} write)",
                        saved.id,
                        perms.base_read_roots().len(),
                        perms.base_write_roots().len(),
                    )));
                    continue;
                }
                SlashResult::ListSessions(filter) => {
                    let current = session.session_id.borrow().clone();
                    match format_sessions(&sessions_dir, current.as_deref(), &filter) {
                        Ok(list) => eprintln!("{list}"),
                        Err(error) => eprintln!("cannot list sessions: {error}"),
                    }
                    continue;
                }
                SlashResult::ForkSession(selector) => {
                    if agent_registry.running() > 0 {
                        eprintln!("fork blocked while background agents are running");
                        continue;
                    }
                    if !persist_session(&session_store, &session, &model, &messages) {
                        eprintln!("fork cancelled because the active session could not be saved");
                        continue;
                    }
                    report_write_checkpoints(write_journal.drain_and_take_reports().await);
                    let active_id = session.session_id.borrow().clone();
                    let Some(selector) = selector.or(active_id) else {
                        eprintln!("cannot fork: session autosave is disabled");
                        continue;
                    };
                    match session::SessionStore::fork_with_source(&sessions_dir, &selector) {
                        Ok(forked) => {
                            eprintln!(
                                "~ forked {} -> {}; switching with fresh runtime permissions",
                                forked.source_session_id, forked.session.id
                            );
                            priority_command = Some(format!("/resume {}", forked.session.id));
                        }
                        Err(error) => eprintln!("cannot fork session: {error}"),
                    }
                    continue;
                }
                SlashResult::ExportSession {
                    destination,
                    format,
                } => {
                    if !persist_session(&session_store, &session, &model, &messages) {
                        eprintln!("export cancelled because the active session could not be saved");
                        continue;
                    }
                    let Some(active_id) = session.session_id.borrow().clone() else {
                        eprintln!("cannot export: session autosave is disabled");
                        continue;
                    };
                    match session::SessionStore::export(
                        &sessions_dir,
                        &active_id,
                        &destination,
                        format,
                    ) {
                        Ok(path) => eprintln!("~ exported session to {}", path.display()),
                        Err(error) => eprintln!("cannot export session: {error}"),
                    }
                    continue;
                }
                SlashResult::ArchiveSession(id) => {
                    let active_id = session.session_id.borrow().clone();
                    match session::SessionStore::archive(&sessions_dir, &id, active_id.as_deref()) {
                        Ok(path) => eprintln!(
                            "~ archived session {id} at {}; restore with /restore {id}",
                            shorten_home(&path)
                        ),
                        Err(error) => eprintln!("cannot archive session: {error}"),
                    }
                    continue;
                }
                SlashResult::RestoreSession(id) => {
                    match session::SessionStore::restore_archived(&sessions_dir, &id) {
                        Ok(saved) => eprintln!(
                            "~ restored session {}; resume with /resume {}",
                            saved.id, saved.id
                        ),
                        Err(error) => eprintln!("cannot restore session: {error}"),
                    }
                    continue;
                }
                SlashResult::DeleteSession(id) => {
                    let active_id = session.session_id.borrow().clone();
                    match session::SessionStore::delete(&sessions_dir, &id, active_id.as_deref()) {
                        Ok(()) => eprintln!("~ permanently deleted session {id}"),
                        Err(error) => eprintln!("cannot delete session: {error}"),
                    }
                    continue;
                }
            }
        }

        // Keep the submitted form for `/retry`; attachments are re-read on a
        // retry so the model never receives a silently stale snapshot.
        let retry_prompt = submitted.clone();
        let expanded = match await_cancellable(attachments::expand(&submitted.text, &perms)).await {
            Some(Ok(expanded)) => expanded,
            Some(Err(error)) => {
                eprintln!("attachment error: {}", terminal_inline(&error));
                continue;
            }
            None => {
                eprintln!("^C (attachment preflight cancelled)");
                continue;
            }
        };
        if !expanded.labels.is_empty() {
            eprintln!("~ attached {} file(s):", expanded.labels.len());
            for label in &expanded.labels {
                eprintln!("  - {}", terminal_inline(label));
            }
        }

        let messages_before = messages.clone();
        let state_before = session_state.clone();
        let agent_watermark = agent_registry.watermark();
        let journal_turn = write_journal.begin_attributed_turn(&retry_prompt.text);
        if let Some(previous) = journal_turn.previous {
            report_write_checkpoint(previous);
        }
        messages.push(ChatMessage::user(expanded.text));
        let mut cfg = base_cfg(model.clone(), cli);
        let event_log = live_agent_event_log(false, "interactive");
        cfg.event_log = Some(event_log.clone());
        let turn_tools = match submitted.tool_mode {
            TurnToolMode::Full => &tools,
            TurnToolMode::None => &no_tools,
        };
        // Publish the active steering token before stdin collection starts, so
        // even an immediately submitted `/steer` targets this exact turn.
        let (steering_turn, steering_receiver) = steering
            .begin_turn()
            .expect("interactive turns open steering sequentially");
        // Turn boundary: a turn is now active, so the hint flips to working
        // before any inference begins.
        publish_agent_state(true);
        publish_title(true);
        fire_agent_state_hooks();
        foreground_typeahead_active.store(true, Ordering::Release);
        // A styled interactive session gets the live raw-mode editor (visible
        // draft + history on the status row); scripted/plain/assistive
        // sessions keep the canonical line collector.
        let typeahead_live_prompt =
            (!assistive_input && kitsu_render::stdin_is_tty() && kitsu_render::stderr_styled())
                .then(kitsu_render::prompt_string);
        let typeahead_history: Vec<String> = rl.history().iter().cloned().collect();
        // The turn is now running, so R5's elapsed segment starts counting
        // and the row's meter picks up the prompt just pushed. Both before
        // the collector starts, because its first poll tick paints the row.
        #[cfg(unix)]
        {
            refresh_status_usage(
                status_source.as_ref(),
                &model,
                &messages,
                &session,
                &session_state,
            );
            if let Some(source) = status_source.as_ref() {
                source.begin_turn();
            }
        }
        let typeahead = TypeaheadCollector::start(
            prompt_queue.clone(),
            steering.clone(),
            agent_registry.clone(),
            typeahead_live_prompt,
            typeahead_history,
            std::mem::take(&mut pending_draft),
        );
        let turn_outcome = write_journal
            .scope_turn(
                &journal_turn.token,
                run_turn_steerable(
                    &provider,
                    turn_tools,
                    &mut messages,
                    &cfg,
                    &mut session_state,
                    &steering,
                    steering_turn,
                    steering_receiver,
                ),
            )
            .await;
        let captured = typeahead.finish();
        // Between turns `elapsed` is `None`: the idle row carries no elapsed
        // segment, so it has nothing to tick and needs no cadence.
        #[cfg(unix)]
        if let Some(source) = status_source.as_ref() {
            source.end_turn();
        }
        foreground_typeahead_active.store(false, Ordering::Release);
        pending_draft = captured.pending_draft;
        while let Ok(notice) = deferred_notice_receiver.try_recv() {
            eprintln!("{notice}");
        }
        if captured.captured > 0 && !captured.live {
            // Only narrate type-ahead that is still waiting: input that was
            // consumed immediately (e.g. an /exit) needs no bookkeeping line.
            // The live editor already printed a per-prompt queue receipt.
            let status = prompt_queue.lock().expect("prompt queue").status();
            if status.queued > 0 {
                eprintln!("~ captured type-ahead: {status}");
            }
        }
        if captured.eof {
            exit_after_queue = true;
            if prompt_queue.lock().expect("prompt queue").status().queued > 0 {
                eprintln!("~ end of input received; exiting after queued prompts finish");
            }
        }
        for outcome in captured.outcomes {
            match outcome {
                TypeaheadOutcome::Notice(notice) => {
                    eprintln!("~ {}", kitsu_render::sanitize_terminal_text(&notice));
                }
                TypeaheadOutcome::SteeringError(error) => {
                    eprintln!("steering refused: {}", terminal_inline(&error));
                }
                TypeaheadOutcome::QueueError(error) => {
                    eprintln!("queue refused: {}", terminal_inline(&error));
                }
            }
        }
        let mut turn_reports = write_journal.drain_and_take_reports().await;
        report_write_checkpoints(turn_reports.clone());
        if let Some(report) = write_journal.finish_attributed_turn(&journal_turn.token) {
            turn_reports.push(report.clone());
            report_write_checkpoint(report);
        }
        let assistant = match &turn_outcome {
            InteractiveTurnOutcome::Turn(TurnOutcome::Completed(Ok(text))) => Some(text.as_str()),
            _ => None,
        };
        finish_agent_trace(cli, "interactive", &event_log, assistant)?;
        warn_context_window_evidence(cli, None, &mut session_state)?;
        match turn_outcome {
            InteractiveTurnOutcome::Turn(TurnOutcome::Completed(Ok(text))) => {
                if text.trim().is_empty() {
                    let issue = empty_final_response_issue(cli.max_output_tokens);
                    finish_assistant_disposition(
                        cli,
                        "interactive",
                        AssistantDisposition::Discarded,
                        Some(&issue),
                    )?;
                    messages = messages_before;
                    rollback_agent_state(&mut session_state, state_before);
                    run_aggregate.record_turn(
                        kitsu_core::RunOutcomeClass::ProviderFault,
                        kitsu_core::RunAssistantDisposition::Discarded,
                    );
                    let cancelled = cancel_agents_and_report_writes_with_cleanup(
                        &agent_registry,
                        agent_watermark,
                        &write_journal,
                    )
                    .await;
                    run_aggregate.observe_cleanup(cancelled.cleanup);
                    let mut retained = turn_reports;
                    retained.extend(cancelled.reports);
                    last_failed = Some(retry_prompt);
                    eprintln!(
                        "~ {}; turn rolled back - /retry to try again",
                        terminal_inline(&issue)
                    );
                    eprintln!("~ completed file changes, if any, remain on disk");
                    note_rollback_retained_changes(
                        cli,
                        None,
                        &mut messages,
                        &retained,
                        tools.is_registered("workspace_diff"),
                    )?;
                } else {
                    finish_assistant_disposition(
                        cli,
                        "interactive",
                        AssistantDisposition::Committed,
                        None,
                    )?;
                    run_aggregate.record_turn(
                        kitsu_core::RunOutcomeClass::Succeeded,
                        kitsu_core::RunAssistantDisposition::Committed,
                    );
                    last_failed = None;
                    persist_session(&session_store, &session, &model, &messages);
                }
            }
            InteractiveTurnOutcome::Turn(TurnOutcome::Completed(Err(e))) => {
                let mapped = map_agent_error_with_diagnostics(
                    e,
                    ProviderOperation::Chat { model: &model },
                    Some(cli.timeout_secs),
                    session.provider_diagnostics,
                );
                run_aggregate.record_turn(
                    mapped.outcome_class(),
                    kitsu_core::RunAssistantDisposition::Discarded,
                );
                finish_assistant_disposition(
                    cli,
                    "interactive",
                    AssistantDisposition::Discarded,
                    Some("the model turn failed before commit"),
                )?;
                messages = messages_before;
                rollback_agent_state(&mut session_state, state_before);
                let cancelled = cancel_agents_and_report_writes_with_cleanup(
                    &agent_registry,
                    agent_watermark,
                    &write_journal,
                )
                .await;
                run_aggregate.observe_cleanup(cancelled.cleanup);
                let mut retained = turn_reports;
                retained.extend(cancelled.reports);
                last_failed = Some(retry_prompt);
                let message = kitsu_render::sanitize_terminal_text(&mapped.to_string());
                for line in kitsu_render::error_lines(&message) {
                    eprintln!("{line}");
                }
                eprintln!(
                    "~ failed turn rolled back; completed file changes, if any, remain on disk"
                );
                note_rollback_retained_changes(
                    cli,
                    None,
                    &mut messages,
                    &retained,
                    tools.is_registered("workspace_diff"),
                )?;
            }
            InteractiveTurnOutcome::Turn(TurnOutcome::Cancelled) => {
                run_aggregate.record_turn(
                    kitsu_core::RunOutcomeClass::Cancelled,
                    kitsu_core::RunAssistantDisposition::Discarded,
                );
                finish_assistant_disposition(
                    cli,
                    "interactive",
                    AssistantDisposition::Discarded,
                    Some("the model turn was cancelled before commit"),
                )?;
                messages = messages_before;
                rollback_agent_state(&mut session_state, state_before);
                let cancelled = cancel_agents_and_report_writes_with_cleanup(
                    &agent_registry,
                    agent_watermark,
                    &write_journal,
                )
                .await;
                run_aggregate.observe_cleanup(cancelled.cleanup);
                let mut retained = turn_reports;
                retained.extend(cancelled.reports);
                last_failed = Some(retry_prompt);
                kitsu_render::finish_line();
                eprintln!("^C (response cancelled; turn rolled back - /retry to try again)");
                eprintln!("~ completed file changes, if any, remain on disk");
                note_rollback_retained_changes(
                    cli,
                    None,
                    &mut messages,
                    &retained,
                    tools.is_registered("workspace_diff"),
                )?;
            }
            InteractiveTurnOutcome::Steered(corrections) => {
                run_aggregate.record_replaced_turn();
                finish_assistant_disposition(
                    cli,
                    "interactive",
                    AssistantDisposition::Discarded,
                    Some("the partial turn was replaced by a steering correction"),
                )?;
                messages = messages_before;
                rollback_agent_state(&mut session_state, state_before);
                let cancelled = cancel_agents_and_report_writes_with_cleanup(
                    &agent_registry,
                    agent_watermark,
                    &write_journal,
                )
                .await;
                run_aggregate.observe_cleanup(cancelled.cleanup);
                let mut retained = turn_reports;
                retained.extend(cancelled.reports);
                note_rollback_retained_changes(
                    cli,
                    None,
                    &mut messages,
                    &retained,
                    tools.is_registered("workspace_diff"),
                )?;
                let mut restarted = retry_prompt;
                restarted.text.push_str(
                    "\n\n[User steering correction; restart this turn using the updated direction below.]",
                );
                for correction in &corrections {
                    restarted.text.push_str("\n- ");
                    restarted.text.push_str(correction);
                }
                last_failed = Some(restarted);
                priority_command = Some("/retry".to_string());
                kitsu_render::finish_line();
                eprintln!(
                    "~ steering accepted ({} correction(s)); partial response rolled back and the turn will restart before queued prompts",
                    corrections.len()
                );
                eprintln!("~ completed file changes, if any, remain on disk");
            }
            InteractiveTurnOutcome::QuitCancelled => {
                run_aggregate.record_turn(
                    kitsu_core::RunOutcomeClass::Cancelled,
                    kitsu_core::RunAssistantDisposition::Discarded,
                );
                finish_assistant_disposition(
                    cli,
                    "interactive",
                    AssistantDisposition::Discarded,
                    Some("the foreground turn was cancelled by /quit cancel"),
                )?;
                messages = messages_before;
                rollback_agent_state(&mut session_state, state_before);
                let cancelled = cancel_agents_and_report_writes_with_cleanup(
                    &agent_registry,
                    agent_watermark,
                    &write_journal,
                )
                .await;
                run_aggregate.observe_cleanup(cancelled.cleanup);
                let mut retained = turn_reports;
                retained.extend(cancelled.reports);
                note_rollback_retained_changes(
                    cli,
                    None,
                    &mut messages,
                    &retained,
                    tools.is_registered("workspace_diff"),
                )?;
                let stopped = agent_registry.cancel_all().await;
                run_aggregate.observe_cleanup(terminal_cleanup_state(&stopped));
                let removed = prompt_queue.lock().expect("prompt queue").clear();
                exit_after_queue = true;
                last_failed = None;
                kitsu_render::finish_line();
                eprintln!(
                    "~ exit cancellation completed: foreground rolled back, {} background job(s) joined, {} queued prompt(s) cleared",
                    stopped.len(),
                    removed.len()
                );
                eprintln!("~ completed file changes, if any, remain on disk");
            }
        }
    }

    let cleanup = await_background_agents(
        &agent_registry,
        "chat input ended before background work completed",
    )
    .await;
    run_aggregate.observe_cleanup(cleanup);
    run_aggregate.record_background_snapshot(&agent_registry.progress_snapshot());
    let surfaced_results = surface_unobserved_background_results(&agent_registry);
    if surfaced_results > 0 {
        eprintln!(
            "~ surfaced {surfaced_results} previously unopened background result(s) before exit"
        );
    }
    report_write_checkpoints(write_journal.drain_and_take_reports().await);
    if !no_session {
        let _ = rl.save_history(&history_path);
        // Tell the user how to come back: an autosaved session is only useful
        // if its id is visible at exit. Printed to stderr (not the JSONL event
        // stream), only for a real interactive session that actually persisted
        // an id, so scripted/piped runs stay quiet.
        if interactive {
            if let Some(id) = session.session_id.borrow().as_deref() {
                eprintln!(
                    "~ session saved: resume with `kitsu chat --resume {}` (or `--resume last`)",
                    shell_quote(id)
                );
            }
        }
    }
    if let Some(task) = agent_notification_task {
        task.abort();
    }
    if let Some(task) = assistive_signal_task {
        task.abort();
    }
    record_interactive_terminal_facts(
        run_aggregate.terminal_facts(jsonl_session_persistence_state()),
    )?;
    Ok(())
}
