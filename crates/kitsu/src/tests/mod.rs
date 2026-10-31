//! Shared unit-test fixtures and tests grouped by responsibility.

use crate::cli::{Cli, Command};
use crate::config::Gateway;
use crate::process_exit::{InferenceCarrier, EXIT_USER};
#[cfg(test)]
use clap::Parser;
use clap::{CommandFactory, FromArgMatches};
use kitsu_core::context::{ContextBudget, DEFAULT_CONTEXT_WINDOW};
use kitsu_core::ArtifactStoragePolicy;
use kitsu_providers::{
    ChatMessage, ProviderAuthSource, ProviderError, ProviderRouteContractError,
    ProviderRouteReceipt,
};
use kitsu_tools::{builtins, Decision, Mode, Permissions, Registry};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Mutex, OnceLock};

use crate::agent_commands::*;
use crate::chat::commands::*;
use crate::chat::state::*;
use crate::chat::*;
use crate::checkpoints::*;
use crate::cli_app::*;
use crate::cli_error::*;
use crate::cli_runtime::*;
use crate::command_artifact;
use crate::command_policy::*;
use crate::command_registry::*;
use crate::completion::*;
use crate::config;
use crate::context_window::*;
use crate::models::*;
use crate::presentation::*;
use crate::prompt::*;
use crate::prompt_queue;
use crate::provider_diagnostics::*;
use crate::review::*;
use crate::runtime_state::*;
use crate::script::*;
use crate::session;
use crate::session_catalog::*;
use crate::session_telemetry::*;
use crate::setup::*;
use crate::skills;
use crate::subagent;
use crate::terminal::agent_view::*;
use crate::terminal::approval::*;
use crate::terminal::completion::*;
use crate::terminal::input::*;
use crate::terminal::model_picker::*;
use crate::terminal::overlay_input::*;
use crate::terminal::transcript::*;
use crate::terminal::typeahead::*;
#[cfg(unix)]
use crate::tui_bridge;
use crate::turn::*;
use crate::workspace;
use serial_test::serial;

mod support;
use support::{
    assert_no_debug_leak, blocked_progress, call, chat_helper, chat_helper_completing_in,
    classic_candidates, expect_ok, explain_fixture, explain_label_value, explain_labels,
    inline_candidates, jobs_detail_line, jobs_view_heading_count, jobs_view_heading_offset,
    parse_with_effort, record_test_checkpoint, retained_note_test_report,
    telemetry_route_receipt_fixture, EnvVarGuard, TUI_MID_TURN_REFUSAL,
};
#[cfg(unix)]
use support::{
    attach_milestone_message, attaching_exit, classic_history_walk, detach_recap_message,
    dispatch_key, drive_transcript, drive_transcript_with_matches, drive_transcript_with_prompts,
    foreground_meta, inline_history_snapshot, inline_history_walk, one_composer_read,
    over_long_name, park_a_path_request, recording_dispatcher, registry_with_one_blocked_agent,
    registry_with_three_agents, resolve, type_filter, type_line, OverlayUnderTest, RingProbe,
    SurfaceProbe, ATTACH_WITHOUT_SELECTION, BINDINGS_SWEEP_COLS, NON_ASCII_NAME, OPENING,
    OVERLAY_PASTE_END, OVERLAY_PASTE_START, OVERLAY_TEST_COLS, PASTED_CORRECTION,
};

mod agent_attachments;
mod agent_commands;
mod agent_view_actions;
mod agent_view_dispatch;
mod agent_view_filter;
mod agent_view_rename;
mod chat;
mod chat_commands;
mod chat_state;
mod checkpoints;
mod cli_app;
mod cli_error;
mod cli_runtime;
mod command_policy;
mod command_registry;
mod completion;
mod context_window;
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
mod terminal_approval;
mod terminal_completion;
mod terminal_input;
mod terminal_model_picker;
mod terminal_overlay_input;
mod terminal_transcript;
mod terminal_typeahead;
mod turn;
