//! CLI startup, command dispatch, and process exit.

use crate::cli::{Cli, Command};
use crate::process_exit::{evaluate_process_exit, EXIT_CANCELLED, EXIT_PROVIDER, EXIT_USER};
use clap::{CommandFactory, FromArgMatches};
use std::sync::atomic::Ordering;
use std::time::Instant;

use crate::ask::run_ask;
use crate::chat::run_chat;
use crate::chat::ChatInvocation;
use crate::chat::InitialReview;
use crate::cli_error::CliError;
use crate::cli_runtime::activate_model_command;
use crate::cli_runtime::emit_jsonl;
use crate::cli_runtime::emit_jsonl_summary;
use crate::cli_runtime::finalize_process_terminal_with_delivery;
use crate::cli_runtime::inference_carrier;
use crate::cli_runtime::inference_command_requested;
use crate::cli_runtime::jsonl_output_failure;
use crate::cli_runtime::jsonl_requested;
use crate::cli_runtime::output_last_message_preserved_without_commit;
use crate::cli_runtime::process_terminal_facts_or_fallback;
use crate::cli_runtime::publish_fallback_inference_jsonl_terminal;
use crate::cli_runtime::run_assistant_disposition;
use crate::cli_runtime::run_elapsed;
use crate::cli_runtime::run_spec;
use crate::cli_runtime::validate_no_alt_screen_flag;
use crate::cli_runtime::validate_output_last_message_surface;
use crate::cli_runtime::validate_output_schema_surface;
use crate::cli_runtime::ProcessTerminalFacts;
use crate::cli_runtime::JSONL_SUMMARY_EMITTED;
use crate::cli_runtime::MODEL_COMMAND_ACTIVE;
use crate::cli_runtime::OUTPUT_LAST_MESSAGE_REQUESTED;
use crate::cli_runtime::PROCESS_INFERENCE_CARRIER;
use crate::cli_runtime::PROCESS_INTERACTIVE_TERMINAL_FACTS;
use crate::cli_runtime::PROCESS_PUBLISHED_RUN;
use crate::cli_runtime::RUN_STARTED_AT;
use crate::cli_runtime::SCRIPT_ACCEPTED_PARTIAL;
use crate::doctor::run_doctor;
use crate::management::run_agents;
use crate::management::run_artifacts;
use crate::management::run_auth;
use crate::management::run_init;
use crate::management::run_sessions;
use crate::models::run_models;
use crate::review::run_review;
use crate::review::ReviewRequest;
use crate::review::ReviewScope;

/// Exit status when the process that was reading kitsu's output went away
/// first. Nothing failed on kitsu's side, and `cmd | head -1` is an everyday
/// shell idiom, so this mirrors the Unix convention of a quiet success.
pub(crate) const EXIT_BROKEN_PIPE: i32 = 0;

/// True for the panic Rust's print macros raise when the reader closed the
/// pipe. Message-shaped by necessity: `std` gives the hook no typed I/O error.
pub(crate) fn panic_payload_is_broken_pipe(payload: &str) -> bool {
    payload.starts_with("failed printing to std")
        && (payload.contains("Broken pipe") || payload.contains("os error 32"))
}

/// Rust masks `SIGPIPE` at startup, so a closed stdout turns every `println!`
/// into a panic and exit 101 — a code reserved for a real crash and absent from
/// kitsu's documented taxonomy (`kitsu sessions | head -1` hit it). The masking
/// is still wanted: the JSONL sink depends on seeing `BrokenPipe` as an error so
/// it can exit typed instead of dying by signal. So the panic is intercepted
/// here rather than the signal disposition being restored: a broken output pipe
/// stops the process quietly, everything else keeps the normal panic report.
pub(crate) fn install_broken_pipe_guard() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let payload = info
            .payload()
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| info.payload().downcast_ref::<&str>().copied())
            .unwrap_or_default();
        if panic_payload_is_broken_pipe(payload) {
            std::process::exit(EXIT_BROKEN_PIPE);
        }
        default_hook(info);
    }));
}

pub(crate) fn main() {
    let _ = RUN_STARTED_AT.set(Instant::now());
    // Every surface receives one neutral identity before argument parsing can
    // accept work. JSONL, human mode, and private artifacts then refer to the
    // same process-run spec even when presentation does not yet display it.
    let _ = run_spec();
    install_broken_pipe_guard();
    let jsonl_mode = jsonl_requested();
    // Logs (never the token: HeaderValue is marked sensitive) go to stderr.
    // JSONL owns both output channels: stdout is framed events and routine
    // diagnostics are suppressed so automation never has to scrape stderr.
    let trace_writer = if jsonl_mode {
        tracing_subscriber::fmt::writer::BoxMakeWriter::new(std::io::sink)
    } else {
        tracing_subscriber::fmt::writer::BoxMakeWriter::new(std::io::stderr)
    };
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_writer(trace_writer)
        .try_init();

    let (mut code, mut outcome_class) = match run() {
        Ok(()) if jsonl_mode => {
            jsonl_output_failure().map_or((0, kitsu_core::RunOutcomeClass::Succeeded), |error| {
                std::eprintln!("{error}");
                (error.exit_code(), error.outcome_class())
            })
        }
        Ok(()) => (
            0,
            if SCRIPT_ACCEPTED_PARTIAL.load(Ordering::Acquire) {
                kitsu_core::RunOutcomeClass::PartiallySucceeded
            } else {
                kitsu_core::RunOutcomeClass::Succeeded
            },
        ),
        Err(e) => {
            let outcome_class = e.outcome_class();
            let code = if !jsonl_mode {
                // One weighted `! error:` notice (ember-red on a TTY); any
                // extra lines in the message are the actionable hint.
                for line in kitsu_render::error_lines(&e.to_string()) {
                    std::eprintln!("{line}");
                }
                e.exit_code()
            } else {
                if !JSONL_SUMMARY_EMITTED.load(Ordering::Acquire) {
                    let frame_result = if MODEL_COMMAND_ACTIVE.load(Ordering::Acquire) {
                        // Carrier-provided facts win. Before the first provider
                        // turn, Joined is structural; after it, missing cleanup
                        // evidence is explicitly NotObserved rather than guessed.
                        publish_fallback_inference_jsonl_terminal(&e)
                    } else if jsonl_output_failure().is_none() {
                        let cancelled = e.exit_code() == EXIT_CANCELLED;
                        emit_jsonl("error", e.jsonl_error_payload()).and_then(|()| {
                            emit_jsonl_summary(
                                if cancelled { "cancelled" } else { "error" },
                                1,
                                usize::from(e.exit_code() == EXIT_PROVIDER),
                            )
                        })
                    } else {
                        Err(jsonl_output_failure()
                            .expect("management fallback observed terminal output failure"))
                    };
                    // `jsonl_output_failure` below reports the retained
                    // terminal error exactly once when framing itself fails.
                    let _ = frame_result;
                }
                jsonl_output_failure().map_or_else(
                    || e.exit_code(),
                    |output_error| {
                        // Framing is impossible after a terminal write/flush
                        // failure, so this is the one JSONL-mode stderr diagnostic.
                        std::eprintln!("{output_error}");
                        output_error.exit_code()
                    },
                )
            };
            (code, outcome_class)
        }
    };
    if jsonl_mode && MODEL_COMMAND_ACTIVE.load(Ordering::Acquire) {
        match (
            PROCESS_INFERENCE_CARRIER.get().copied(),
            PROCESS_PUBLISHED_RUN.get(),
        ) {
            (Some(carrier), Some(published)) => {
                code = evaluate_process_exit(code, carrier, published);
            }
            _ if code == 0 => {
                std::eprintln!("internal JSONL inference lifecycle was not published");
                code = EXIT_USER;
            }
            _ => {}
        }
    }
    if !jsonl_mode && output_last_message_preserved_without_commit() {
        std::eprintln!("~ --output-last-message unchanged: no committed assistant message");
    }
    if !jsonl_mode && MODEL_COMMAND_ACTIVE.load(Ordering::Acquire) {
        let interactive_facts = PROCESS_INTERACTIVE_TERMINAL_FACTS.get().copied();
        if let Some(facts) = interactive_facts {
            outcome_class = facts.outcome;
        }
        let terminal = run_spec().terminal(
            outcome_class,
            interactive_facts.map_or_else(run_assistant_disposition, |facts| facts.assistant),
        );
        let status = match terminal.class() {
            kitsu_core::RunOutcomeClass::Succeeded => kitsu_render::RunTimingStatus::Ok,
            kitsu_core::RunOutcomeClass::PartiallySucceeded => {
                kitsu_render::RunTimingStatus::Partial
            }
            kitsu_core::RunOutcomeClass::Cancelled => kitsu_render::RunTimingStatus::Cancelled,
            kitsu_core::RunOutcomeClass::Interrupted => kitsu_render::RunTimingStatus::Interrupted,
            kitsu_core::RunOutcomeClass::UserFault
            | kitsu_core::RunOutcomeClass::ProviderFault
            | kitsu_core::RunOutcomeClass::ToolFault => kitsu_render::RunTimingStatus::Error,
        };
        let deliver = || {
            kitsu_render::emit_run_timing(
                run_elapsed(),
                status,
                kitsu_render::HumanRunReceipt {
                    run_id: terminal.run_id().as_str(),
                    outcome: terminal.class().as_str(),
                    assistant: terminal.assistant().as_str(),
                },
            )
        };
        let facts = interactive_facts.map_or_else(process_terminal_facts_or_fallback, |facts| {
            ProcessTerminalFacts {
                cleanup: facts.cleanup,
                persistence: facts.persistence,
            }
        });
        match finalize_process_terminal_with_delivery(
            terminal.class(),
            terminal.assistant(),
            facts.cleanup,
            facts.persistence,
            deliver,
        ) {
            Ok((published, delivery)) => {
                debug_assert_eq!(published.terminal(), &terminal);
                debug_assert_eq!(
                    delivery.is_ok(),
                    published.delivery() == kitsu_core::RunDeliveryState::Delivered
                );
                let carrier = PROCESS_INFERENCE_CARRIER
                    .get()
                    .copied()
                    .expect("active model command has an inference carrier");
                code = evaluate_process_exit(code, carrier, &published);
            }
            Err(error) => {
                std::eprintln!("internal human inference finalization failed: {error}");
                if code == 0 {
                    code = EXIT_USER;
                }
            }
        }
    }
    std::process::exit(code);
}

pub(crate) fn run() -> Result<(), CliError> {
    // Parse via ArgMatches (not the plain derive) so `apply_effort` can see
    // which budget flags were user-set vs defaulted, then fold in the preset.
    let cli = match Cli::command().try_get_matches().and_then(|matches| {
        Cli::from_arg_matches(&matches).map(|mut cli| {
            cli.apply_effort(&matches);
            cli
        })
    }) {
        Ok(cli) => cli,
        Err(error) => match error.kind() {
            clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion => {
                if jsonl_requested() {
                    return Err(CliError::user(
                        "--jsonl cannot be combined with human --help or --version output",
                    ));
                }
                // `kitsu --help | head -3` closes the pipe mid-help. That is the
                // reader's choice, not a kitsu failure; every other write error
                // still reports.
                return match error.print() {
                    Ok(()) => Ok(()),
                    Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
                    Err(e) => Err(CliError::user(format!("failed to print command help: {e}"))),
                };
            }
            _ => return Err(CliError::user(error.to_string())),
        },
    };
    kitsu_render::configure_terminal_modes(cli.plain, cli.reduced_motion, cli.screen_reader);
    if inference_command_requested(&cli) {
        // Accept the process lifecycle before output/config/runtime preflight,
        // not only after a carrier has successfully built its provider.
        let carrier = inference_carrier(&cli, kitsu_render::stdin_is_tty())
            .expect("every inference command has an explicit carrier");
        activate_model_command(carrier)?;
    }
    validate_no_alt_screen_flag(&cli)?;
    validate_output_last_message_surface(&cli)?;
    OUTPUT_LAST_MESSAGE_REQUESTED.store(cli.output_last_message.is_some(), Ordering::Release);
    validate_output_schema_surface(&cli)?;

    let runtime = tokio::runtime::Runtime::new()
        .map_err(|e| CliError::user(format!("failed to start runtime: {e}")))?;

    if let Some(prompt) = cli.print_prompt.as_deref() {
        if cli.command.is_some() {
            return Err(CliError::user(
                "-p/--prompt is a top-level one-shot mode and cannot be combined with a subcommand; use `kitsu -p <PROMPT>` or the subcommand's own prompt argument",
            ));
        }
        return runtime.block_on(run_ask(&cli, prompt));
    }

    match cli.command.as_ref() {
        None => runtime.block_on(run_chat(&cli, ChatInvocation::default())),
        Some(Command::Init { path, json }) => run_init(&cli, path.as_deref(), *json),
        Some(Command::Doctor {
            online,
            inference,
            json,
        }) => runtime.block_on(run_doctor(&cli, *online, *inference, *json)),
        Some(Command::Auth { json, command }) => runtime.block_on(run_auth(&cli, *json, command)),
        Some(Command::Ask { prompt }) => runtime.block_on(run_ask(&cli, prompt)),
        Some(Command::Review {
            path,
            staged,
            unstaged,
            base,
            commit,
            instructions,
            resume,
            session,
        }) => {
            let scope = if *staged {
                ReviewScope::Staged
            } else if *unstaged {
                ReviewScope::Unstaged
            } else if let Some(revision) = base {
                ReviewScope::Base(revision.clone())
            } else if let Some(revision) = commit {
                ReviewScope::Commit(revision.clone())
            } else {
                ReviewScope::Working
            };
            runtime.block_on(run_review(
                &cli,
                InitialReview {
                    request: ReviewRequest {
                        scope,
                        path: path.clone(),
                    },
                    instructions: instructions.clone(),
                },
                resume.as_deref(),
                *session,
            ))
        }
        Some(Command::Models { filter }) => {
            runtime.block_on(run_models(&cli, filter.as_deref().unwrap_or("")))
        }
        Some(Command::Sessions { json, command }) => {
            runtime.block_on(run_sessions(&cli, *json, command.as_ref()))
        }
        Some(Command::Artifacts { json, command }) => run_artifacts(&cli, *json, command.as_ref()),
        Some(Command::Agents { json, all }) => run_agents(&cli, *json, *all),
        Some(Command::Chat {
            prompt,
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
            session,
            no_session,
            name,
            allow_turn_errors,
        }) => runtime.block_on(run_chat(
            &cli,
            ChatInvocation {
                first_prompt: prompt.as_deref(),
                no_tools: *no_tools,
                allow_paths,
                allow_write,
                allow_read_write,
                require_writes,
                require_write_sha256,
                enable_commands: *enable_commands,
                command_roots,
                command_runtime_roots,
                command_env,
                allow_commands,
                allow_network: *allow_network,
                script: script.as_deref(),
                resume: resume.as_deref(),
                persist_session: *session,
                no_session: *no_session,
                session_name: name.as_deref(),
                allow_turn_errors: *allow_turn_errors,
                initial_review: None,
            },
        )),
    }
}
