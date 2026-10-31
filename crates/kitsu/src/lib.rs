//! Kitsu host application and CLI adapters.
//!
//! The executable calls `run_cli`; provider, session, and tool components stay
//! separate from terminal adapters so another host can compose them here.

mod agent_hook;
mod agent_state_file;
mod artifacts;
mod attachments;
mod cli;
mod command_artifact;
mod config;
mod credential_store;
mod llms;
mod process_exit;
mod prompt_queue;
mod session;
mod session_telemetry_render;
mod skills;
mod structured_output;
mod subagent;
mod tui_bridge;
mod workspace;

/// Human diagnostics are silent in JSONL automation mode. Errors that can no
/// longer be framed because stdout itself failed use `std::eprintln!` directly.
macro_rules! eprintln {
    () => {
        if !$crate::cli_runtime::jsonl_requested() {
            std::eprintln!();
        }
    };
    ($($arg:tt)*) => {
        if !$crate::cli_runtime::jsonl_requested() {
            std::eprintln!($($arg)*);
        }
    };
}

mod agent_commands;
mod artifact_commands;
mod ask;
mod chat;
mod checkpoints;
mod cli_app;
mod cli_error;
mod cli_runtime;
mod command_policy;
mod command_registry;
mod completion;
mod context_window;
mod doctor;
mod management;
mod models;
mod presentation;
mod prompt;
mod provider_diagnostics;
mod review;
mod runtime_state;
mod script;
mod session_catalog;
mod session_telemetry;
mod setup;
mod terminal;
mod turn;

/// Run the CLI adapter, including its process-level exit handling.
pub fn run_cli() {
    cli_app::main();
}

#[cfg(test)]
mod tests;
