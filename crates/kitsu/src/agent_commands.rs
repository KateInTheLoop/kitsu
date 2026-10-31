//! Agent inspection, steering, and joined shutdown.

use kitsu_tools::Decision;
use std::sync::Arc;

use crate::chat::commands::closest_jobs_subcommand;
use crate::checkpoints::report_write_checkpoints;
use crate::cli_error::CliError;
use crate::cli_runtime::emit_jsonl;
use crate::cli_runtime::jsonl_requested;
use crate::presentation::terminal_inline;
use crate::subagent;
#[cfg(unix)]
use crate::terminal::agent_view::apply_jobs_view_exit;
#[cfg(unix)]
use crate::terminal::agent_view::run_agent_view_overlay;
use crate::terminal::approval::parse_permission_answer;

pub(crate) fn report_cancelled_agents(ids: Vec<u64>) {
    if jsonl_requested() {
        return;
    }
    if !ids.is_empty() {
        let ids = ids
            .iter()
            .map(|id| format!("#{id}"))
            .collect::<Vec<_>>()
            .join(", ");
        eprintln!("~ cancelled background agent(s) started by the rolled-back turn: {ids}");
    }
}

/// The severity a background transition should be announced with. A detached
/// worker that failed is never reported as plain completion — that is the
/// misreporting that let a failed profile spawn read as success.
pub(crate) fn background_agent_notice_level(
    previous: subagent::AgentRegistrySnapshot,
    current: subagent::AgentRegistrySnapshot,
) -> kitsu_render::AsyncNoticeLevel {
    if current.failed > previous.failed {
        kitsu_render::AsyncNoticeLevel::Error
    } else {
        kitsu_render::AsyncNoticeLevel::Info
    }
}

pub(crate) fn background_agent_notice(
    previous: subagent::AgentRegistrySnapshot,
    current: subagent::AgentRegistrySnapshot,
) -> Option<String> {
    let failed = current.failed.saturating_sub(previous.failed);
    let completed = current.terminal.saturating_sub(previous.terminal);
    if failed > 0 {
        return Some(format!(
            "{failed} background agent(s) FAILED - /jobs list for the failure class{}",
            if current.running > 0 {
                format!("; {} still running", current.running)
            } else {
                String::new()
            }
        ));
    }
    if completed > 0 {
        return Some(if current.running == 0 {
            format!("{completed} background agent(s) finished - /agents for results")
        } else {
            format!(
                "{completed} background agent(s) finished; {} still running - /agents to check",
                current.running
            )
        });
    }
    if current.running > previous.running {
        Some(format!(
            "{} background agent(s) running - /agents to check",
            current.running
        ))
    } else if current.running < previous.running {
        Some(if current.running == 0 {
            "background agents finished - /agents for results".to_string()
        } else {
            format!(
                "background agent finished; {} still running - /agents to check",
                current.running
            )
        })
    } else {
        None
    }
}

pub(crate) fn agent_progress_status_label(status: subagent::AgentProgressStatus) -> &'static str {
    match status {
        subagent::AgentProgressStatus::Running => "running",
        subagent::AgentProgressStatus::Blocked => "blocked",
        subagent::AgentProgressStatus::Done => "done",
        subagent::AgentProgressStatus::Failed => "failed",
        subagent::AgentProgressStatus::Cancelled => "cancelled",
    }
}

pub(crate) fn terminal_agent_status_label(status: &subagent::AgentStatus) -> &'static str {
    match status {
        subagent::AgentStatus::Running => "running",
        subagent::AgentStatus::Blocked => "blocked",
        subagent::AgentStatus::Done(_) => "done",
        subagent::AgentStatus::Failed(_) => "failed",
        subagent::AgentStatus::Cancelled(_) => "cancelled",
    }
}

/// The deck marker for one agent row. A finished agent whose result nobody has
/// collected yet is `◈` — a durable artifact awaiting custody — not a spent
/// `◇`; that is the row the user still has to act on.
pub(crate) fn agent_deck_state(
    status: subagent::AgentProgressStatus,
    result_collected: bool,
) -> kitsu_render::DeckState {
    match status {
        subagent::AgentProgressStatus::Running => kitsu_render::DeckState::Running,
        // A blocked agent is non-terminal but it needs the user, so it gets
        // its own marker rather than the busy one (F34). Collection state
        // cannot move a non-terminal agent, so `result_collected` is ignored.
        subagent::AgentProgressStatus::Blocked => kitsu_render::DeckState::Blocked,
        subagent::AgentProgressStatus::Done if !result_collected => {
            kitsu_render::DeckState::ResultReady
        }
        subagent::AgentProgressStatus::Done => kitsu_render::DeckState::Stopped,
        subagent::AgentProgressStatus::Failed => kitsu_render::DeckState::Failed,
        subagent::AgentProgressStatus::Cancelled => kitsu_render::DeckState::Stopped,
    }
}

/// The Running-row activity child line (ui-parity item 13): the most recent
/// bounded activity label with its age, so a stalled agent shows at a
/// glance. Terminal rows carry no child — their story is complete.
pub(crate) fn agent_activity_child_row(
    activity: Option<&str>,
    ms_ago: Option<u64>,
    plain: bool,
) -> String {
    let arrow = if plain { "->" } else { "\u{21b3}" };
    match activity {
        None => format!("{arrow} no activity yet"),
        Some(activity) => {
            let age = ms_ago
                .map(|ms| {
                    format!(
                        " \u{b7} {} ago",
                        kitsu_render::format_wait_elapsed(std::time::Duration::from_millis(ms))
                    )
                })
                .unwrap_or_default();
            let age = if plain {
                age.replace('\u{b7}', "|")
            } else {
                age
            };
            format!("{arrow} {}{age}", terminal_inline(activity))
        }
    }
}

/// The deck's shared count line: one expression so the flat deck and the
/// grouped view can never disagree on the numbers they report.
pub(crate) fn jobs_deck_counts(snapshot: &subagent::AgentProgressSnapshot) -> String {
    format!(
        "{} total \u{b7} {} running \u{b7} {} done \u{b7} {} failed \u{b7} {} cancelled \u{b7} {} blocked \u{b7} {} running slot(s) free \u{b7} {} record slot(s) free",
        snapshot.total,
        snapshot.running,
        snapshot.done,
        snapshot.failed,
        snapshot.cancelled,
        snapshot.blocked,
        snapshot.available_running_slots,
        snapshot.available_record_slots,
    )
}

/// One agent's deck row and its continuation lines, shared by `/jobs list`
/// (indents 2/6) and `/jobs view` (indents 4/8) so the two renderings can
/// never drift in content.
pub(crate) fn agent_deck_row_block(
    agent: &subagent::AgentProgress,
    result_collected: bool,
    row_indent: usize,
    continuation_indent: usize,
) -> String {
    let row = " ".repeat(row_indent);
    let cont = " ".repeat(continuation_indent);
    let model = if agent.telemetry.model.is_empty() {
        "unknown".to_string()
    } else {
        terminal_inline(&agent.telemetry.model)
    };
    let activity = agent
        .telemetry
        .last_activity
        .as_deref()
        .map(terminal_inline)
        .unwrap_or_else(|| "none".to_string());
    let failure = agent
        .telemetry
        .failure_class
        .map(|class| format!(" | failure {class}"))
        .unwrap_or_default();
    let route = match (
        agent.telemetry.route_generation,
        agent.telemetry.route_generation_mismatch,
    ) {
        (Some(generation), false) => format!(" | route gen {generation}"),
        (Some(generation), true) => format!(" | route gen {generation} (mismatch observed)"),
        (None, true) => " | route generation mismatch observed".to_string(),
        (None, false) => String::new(),
    };
    let bytes = agent
        .result_bytes
        .map(|bytes| format!(" | result {bytes} B"))
        .unwrap_or_default();
    let state = agent_deck_state(agent.status, result_collected);
    let profile = agent
        .profile
        .as_deref()
        .map(terminal_inline)
        .unwrap_or_else(|| "default profile".to_string());
    let mut output = format!(
        "\n{row}{} #{} {} | {} | {}\n{cont}{:.1}s | model {}{} | tokens {}/{}/{} | tools {} | writes {} | last {}{}{}",
        kitsu_render::deck_marker(state),
        agent.id,
        agent_progress_status_label(agent.status),
        profile,
        terminal_inline(&agent.task),
        agent.elapsed_ms as f64 / 1000.0,
        model,
        route,
        agent.telemetry.prompt_tokens,
        agent.telemetry.completion_tokens,
        agent.telemetry.total_tokens,
        agent.telemetry.tool_count,
        agent.telemetry.write_count,
        activity,
        failure,
        bytes,
    );
    if agent.telemetry.recent_activity.len() > 1 {
        let recent = agent
            .telemetry
            .recent_activity
            .iter()
            .map(|activity| terminal_inline(activity))
            .collect::<Vec<_>>()
            .join(" -> ");
        output.push_str(&format!("\n{cont}recent {recent}"));
    }
    if state == kitsu_render::DeckState::ResultReady {
        output.push_str(&format!(
            "\n{cont}result ready and not yet collected - /jobs show {}",
            agent.id
        ));
    }
    if matches!(agent.status, subagent::AgentProgressStatus::Running) {
        output.push_str(&format!(
            "\n{cont}{}",
            agent_activity_child_row(
                agent.telemetry.last_activity.as_deref(),
                agent.last_activity_ms_ago,
                kitsu_render::plain_mode(),
            )
        ));
        output.push_str(&format!(
            "\n{cont}partial response text withheld until the job is terminal"
        ));
    }
    output
}

/// Agents whose Done result is still awaiting pickup, so a row can tell
/// "result ready" from "collected".
pub(crate) fn jobs_ready_handoff_ids(
    registry: &subagent::AgentRegistry,
) -> std::collections::HashSet<u64> {
    registry
        .thread_snapshots()
        .into_iter()
        .filter(|thread| thread.handoff == subagent::thread::AgentHandoffState::Ready)
        .map(|thread| thread.id)
        .collect()
}

pub(crate) const JOBS_EMPTY_DECK_LINE: &str =
    "\n  no background agents \u{2014} ask the model to delegate work, or see /profiles";

/// Human `/jobs list`. Rendered from the same progress snapshot the JSONL
/// `command_result` payload is built from, using the DTO's own status spellings
/// — no `{:?}` reaches the terminal.
pub(crate) fn format_jobs(registry: &subagent::AgentRegistry) -> String {
    let snapshot = registry.progress_snapshot();
    let ready = jobs_ready_handoff_ids(registry);
    let mut output = kitsu_render::deck_header(&format!(
        "agent deck \u{2014} {}",
        jobs_deck_counts(&snapshot)
    ));
    if snapshot.agents.is_empty() {
        output.push_str(JOBS_EMPTY_DECK_LINE);
        return output;
    }
    for agent in &snapshot.agents {
        output.push_str(&agent_deck_row_block(
            agent,
            !ready.contains(&agent.id),
            2,
            6,
        ));
    }
    output
}

/// Which `/jobs view` group one agent belongs to. `needs input` is
/// unreachable today — nothing can put an agent into `Blocked` yet — so the
/// group is still never produced, but the arm that will produce it is here.
pub(crate) fn jobs_view_group(
    agent: &subagent::AgentProgress,
    result_collected: bool,
) -> &'static str {
    match agent.status {
        subagent::AgentProgressStatus::Running => "working",
        subagent::AgentProgressStatus::Blocked => "needs input",
        subagent::AgentProgressStatus::Done if !result_collected => "done, result ready",
        subagent::AgentProgressStatus::Done => "done, collected",
        subagent::AgentProgressStatus::Failed => "failed",
        subagent::AgentProgressStatus::Cancelled => "cancelled",
    }
}

/// Human `/jobs view`: the same rows the flat deck prints, grouped by state in
/// a fixed order with empty groups omitted. Headings bypass `deck_header`, so
/// they are pure ASCII in every output tier.
pub(crate) fn format_jobs_view(registry: &subagent::AgentRegistry) -> String {
    let snapshot = registry.progress_snapshot();
    let ready = jobs_ready_handoff_ids(registry);
    let mut output = kitsu_render::deck_header(&format!(
        "agent deck by state \u{2014} {}",
        jobs_deck_counts(&snapshot)
    ));
    if snapshot.agents.is_empty() {
        output.push_str(JOBS_EMPTY_DECK_LINE);
        return output;
    }
    // `needs input` is reachable only under `[agents] blocking = "ask"`, which
    // is where a gate parks a detached agent on a user answer; under the
    // default `fail_closed` no agent ever enters the group.
    const GROUPS: [&str; 6] = [
        "needs input",
        "working",
        "done, result ready",
        "done, collected",
        "failed",
        "cancelled",
    ];
    for heading in GROUPS {
        let members = snapshot
            .agents
            .iter()
            .filter(|agent| jobs_view_group(agent, !ready.contains(&agent.id)) == heading)
            .collect::<Vec<_>>();
        if members.is_empty() {
            continue;
        }
        debug_assert!(heading.is_ascii(), "deck group headings stay ASCII");
        output.push_str(&format!("\n  {heading} ({})", members.len()));
        for agent in members {
            output.push_str(&agent_deck_row_block(
                agent,
                !ready.contains(&agent.id),
                4,
                8,
            ));
        }
    }
    output
}

pub(crate) fn jobs_snapshot_payload(registry: &subagent::AgentRegistry) -> serde_json::Value {
    let snapshot = registry.progress_snapshot();
    let agents = snapshot
        .agents
        .into_iter()
        .map(|agent| {
            serde_json::json!({
                "id": agent.id,
                "task": agent.task,
                "status": agent_progress_status_label(agent.status),
                "elapsed_ms": agent.elapsed_ms,
                "result_bytes": agent.result_bytes,
                "telemetry": {
                    "model": agent.telemetry.model,
                    "route_generation": agent.telemetry.route_generation,
                    "route_generation_mismatch": agent.telemetry.route_generation_mismatch,
                    "prompt_tokens": agent.telemetry.prompt_tokens,
                    "completion_tokens": agent.telemetry.completion_tokens,
                    "total_tokens": agent.telemetry.total_tokens,
                    "tool_count": agent.telemetry.tool_count,
                    "write_count": agent.telemetry.write_count,
                    "last_activity": agent.telemetry.last_activity,
                    "recent_activity": agent.telemetry.recent_activity,
                    "failure_class": agent.telemetry.failure_class.map(|class| class.to_string()),
                },
            })
        })
        .collect::<Vec<_>>();
    let threads = registry.thread_snapshot_dtos();
    debug_assert!(threads
        .iter()
        .all(|thread| thread.validate_default().is_ok()));
    serde_json::json!({
        "generation": snapshot.generation,
        "total": snapshot.total,
        "running": snapshot.running,
        "done": snapshot.done,
        "failed": snapshot.failed,
        "cancelled": snapshot.cancelled,
        "blocked": snapshot.blocked,
        "max_running": snapshot.max_running,
        "max_records": snapshot.max_records,
        "available_running_slots": snapshot.available_running_slots,
        "available_record_slots": snapshot.available_record_slots,
        "agents": agents,
        "threads": threads,
    })
}

pub(crate) fn terminal_job_payload(status: &subagent::AgentTerminalStatus) -> serde_json::Value {
    serde_json::json!({
        "id": status.id,
        "task": status.task,
        "status": terminal_agent_status_label(&status.status),
        "joined": status.joined,
    })
}

/// The steering receipt, in the terms of what steering actually does.
///
/// A correction does not join the running attempt: `run_subagent_report_with_progress`
/// drops the in-flight future and restarts the worker on a fresh transcript
/// (system + task + latest correction). "Queued" implied the opposite, so the
/// receipt names the restart, the elapsed work it throws away, and the effects
/// that survive it.
pub(crate) fn format_steering_receipt(receipt: &subagent::thread::AgentSteeringReceipt) -> String {
    format!(
        "restarting background agent #{} with your correction (sequence {}; {} queued) - {:.1}s of in-flight progress discarded; completed file changes kept",
        receipt.id,
        receipt.sequence,
        receipt.pending,
        receipt.discarded_ms as f64 / 1000.0,
    )
}

/// Human `/jobs show`. Every label is the thread DTO's own spelling, so the
/// terminal and the JSONL `thread` payload read identically.
pub(crate) fn format_agent_thread(snapshot: &subagent::thread::AgentThreadSnapshot) -> String {
    let state = match snapshot.phase {
        subagent::thread::AgentThreadPhase::Running => kitsu_render::DeckState::Running,
        // F34: the same marker the `/jobs` deck gives a blocked agent, so the
        // glyph does not depend on which subcommand was typed.
        subagent::thread::AgentThreadPhase::Blocked => kitsu_render::DeckState::Blocked,
        subagent::thread::AgentThreadPhase::Done
            if snapshot.handoff == subagent::thread::AgentHandoffState::Ready =>
        {
            kitsu_render::DeckState::ResultReady
        }
        subagent::thread::AgentThreadPhase::Done => kitsu_render::DeckState::Stopped,
        subagent::thread::AgentThreadPhase::Failed => kitsu_render::DeckState::Failed,
        subagent::thread::AgentThreadPhase::Cancelled => kitsu_render::DeckState::Stopped,
    };
    let mut output = format!(
        "{} agent #{} {} | model {} | profile {} | {:.1}s\n  task: {}\n  steering: {} ({} queued; each restarts the worker) | handoff: {}",
        kitsu_render::deck_marker(state),
        snapshot.id,
        snapshot.phase.label(),
        terminal_inline(&snapshot.model),
        snapshot
            .profile
            .as_deref()
            .map(terminal_inline)
            .unwrap_or_else(|| "default".to_string()),
        snapshot.elapsed_ms as f64 / 1000.0,
        terminal_inline(&snapshot.task),
        if snapshot.steering_open { "open" } else { "closed" },
        snapshot.pending_steering,
        snapshot.handoff.label(),
    );
    if snapshot.timeline.entries.is_empty() {
        output.push_str("\n  timeline: no retained events");
    } else {
        output.push_str("\n  bounded redacted timeline:");
        for entry in &snapshot.timeline.entries {
            output.push_str(&format!(
                "\n    [{} +{:.1}s {}] {}{}",
                entry.sequence,
                entry.elapsed_ms as f64 / 1000.0,
                entry.kind.label(),
                kitsu_render::sanitize_terminal_text(&entry.text),
                if entry.truncated { " [truncated]" } else { "" },
            ));
        }
        if snapshot.timeline.dropped_entries > 0 {
            output.push_str(&format!(
                "\n    ... {} older event(s) omitted by the thread budget",
                snapshot.timeline.dropped_entries
            ));
        }
    }
    output
}

/// The permission a blocked agent is still waiting for, as `permission to X`
/// (T2-S6), or `None` once its newest approval entry is a decision.
///
/// Reads the bounded redacted timeline, which holds the approval *kind* and
/// no tool argument, so this can never surface a path or an argv word.
pub(crate) fn pending_approval_request(
    thread: &subagent::thread::AgentThreadSnapshot,
) -> Option<String> {
    let latest = thread
        .timeline
        .entries
        .iter()
        .rev()
        .find(|entry| entry.kind == subagent::thread::AgentTimelineKind::Approval)?;
    let pending = latest.text.strip_suffix(" requested")?;
    // The recorded spelling is `approval to <kind>`; accept the equivalent
    // `permission to <kind>` too, so the line reads the same either way.
    let kind = pending
        .strip_prefix("approval to ")
        .or_else(|| pending.strip_prefix("permission to "))
        .unwrap_or(pending);
    Some(format!("permission to {}", terminal_inline(kind)))
}

/// Human `/jobs explain <id>`, borrowed from herdr's `agent explain`: why is
/// this agent in this state, since when, and what is it waiting on.
///
/// Deliberately *not* `/jobs show`: no timeline, no task text, no result, and
/// no acknowledgement. Two ages appear in two formats on purpose. `since` is a
/// run duration and keeps the one decimal this file uses throughout; an
/// activity age goes through `kitsu_render::format_wait_elapsed`, which
/// truncates to whole seconds, because that is what the deck's own activity
/// line shows. They are not unified.
///
/// The output does not pass through `sanitize_plain_inline`, so every static
/// byte here is ASCII and every separator is `|`; user-supplied text is routed
/// through `terminal_inline`.
pub(crate) fn format_agent_explain(
    progress: &subagent::AgentProgress,
    thread: &subagent::thread::AgentThreadSnapshot,
) -> String {
    let state = match thread.phase {
        subagent::thread::AgentThreadPhase::Running => kitsu_render::DeckState::Running,
        // F34: the same marker the `/jobs` deck gives a blocked agent, so the
        // glyph does not depend on which subcommand was typed.
        subagent::thread::AgentThreadPhase::Blocked => kitsu_render::DeckState::Blocked,
        subagent::thread::AgentThreadPhase::Done
            if thread.handoff == subagent::thread::AgentHandoffState::Ready =>
        {
            kitsu_render::DeckState::ResultReady
        }
        subagent::thread::AgentThreadPhase::Done => kitsu_render::DeckState::Stopped,
        subagent::thread::AgentThreadPhase::Failed => kitsu_render::DeckState::Failed,
        subagent::thread::AgentThreadPhase::Cancelled => kitsu_render::DeckState::Stopped,
    };
    // Non-terminal, not just `Running`: a blocked agent takes the live shape
    // (`since: started …`, a steering line, no handoff line).
    let running = matches!(
        thread.phase,
        subagent::thread::AgentThreadPhase::Running | subagent::thread::AgentThreadPhase::Blocked
    );
    let elapsed = thread.elapsed_ms as f64 / 1000.0;
    let mut output = format!(
        "{} agent #{} explained | {}",
        kitsu_render::deck_marker(state),
        thread.id,
        thread.phase.label(),
    );

    if running {
        output.push_str(&format!("\n  since: started {elapsed:.1}s ago"));
    } else {
        output.push_str(&format!("\n  since: ran {elapsed:.1}s"));
    }

    let activity = progress.telemetry.last_activity.as_deref().map(|label| {
        let label = terminal_inline(label);
        match progress.last_activity_ms_ago {
            Some(ms) => format!(
                "{label} ({} ago)",
                kitsu_render::format_wait_elapsed(std::time::Duration::from_millis(ms))
            ),
            None => label,
        }
    });

    if running {
        // T2-S6: a parked permission request is the real answer to "what is
        // this agent waiting on", so it outranks the last tool activity. The
        // pending request is the newest approval entry that no outcome entry
        // has closed; the entry text carries the kind only, never an
        // argument.
        let pending_permission = pending_approval_request(thread);
        match (pending_permission.as_deref(), activity.as_deref()) {
            (Some(permission), _) => output.push_str(&format!("\n  waiting on: {permission}")),
            (None, Some(activity)) => output.push_str(&format!("\n  waiting on: {activity}")),
            (None, None) => output.push_str("\n  waiting on: no activity recorded yet"),
        }
    } else {
        if matches!(thread.phase, subagent::thread::AgentThreadPhase::Failed) {
            if let Some(class) = progress.telemetry.failure_class {
                output.push_str(&format!("\n  because: {class}"));
            }
        }
        // `AgentProgress::last_activity_ms_ago` is doc-commented "(running
        // rows)" but is in fact populated for terminal agents too, which this
        // line's age depends on. Narrowing it to match that comment would
        // silently drop the age here.
        if let Some(activity) = activity.as_deref() {
            output.push_str(&format!("\n  last activity: {activity}"));
        }
    }

    if running {
        if thread.steering_open {
            output.push_str(&format!(
                "\n  steering: open | {} queued",
                thread.pending_steering
            ));
        } else {
            output.push_str("\n  steering: closed");
        }
    } else {
        let handoff = match thread.handoff {
            subagent::thread::AgentHandoffState::Ready => {
                format!("ready | result not collected, see /jobs show {}", thread.id)
            }
            subagent::thread::AgentHandoffState::Acknowledged => {
                "acknowledged | result already collected".to_string()
            }
            subagent::thread::AgentHandoffState::Unavailable => "unavailable".to_string(),
        };
        output.push_str(&format!("\n  handoff: {handoff}"));
    }

    output
}

/// `/jobs explain <id>` for both dispatchers: `None` for an unknown id.
///
/// Read-only by construction. It never calls `acknowledge_result` and never
/// calls `result`, which acknowledges too, so diagnosing an agent cannot take
/// custody of its result and defeat the exit guard on unviewed results.
///
/// There is no per-id progress accessor, so the matching progress record is
/// found by scan; the registry caps records at 128, so this is bounded.
pub(crate) fn agent_explain_view(registry: &subagent::AgentRegistry, id: u64) -> Option<String> {
    let thread = registry.thread_snapshot(id)?;
    let progress = registry
        .progress_snapshot()
        .agents
        .into_iter()
        .find(|agent| agent.id == id)?;
    Some(format_agent_explain(&progress, &thread))
}

/// The one advertised form of `/jobs reply`, shared by both dispatchers so a
/// bad argument reads identically whether a turn is running or not.
pub(crate) const JOBS_REPLY_USAGE: &str = "usage: /jobs reply <id> <y|a|n>";

/// Parse `<id> <y|a|n>` for `/jobs reply`.
///
/// The key is read by [`parse_permission_answer`], the very function the
/// foreground approval prompt uses, rather than by a second match: that is what
/// keeps the two prompts' vocabularies from drifting apart, and it is why
/// `yes`/`always`/`no` and any casing work here too. `Interrupt` is
/// unreachable, because that spelling is not in the accepted vocabulary.
pub(crate) fn parse_jobs_reply(
    parts: &mut std::str::SplitWhitespace<'_>,
) -> Option<(u64, Decision)> {
    let id = parts.next()?.parse::<u64>().ok()?;
    let decision = parse_permission_answer(parts.next()?)?;
    if parts.next().is_some() {
        return None;
    }
    Some((id, decision))
}

/// Machine label for one answered decision, for the `/jobs reply` payload.
pub(crate) fn permission_decision_label(decision: Decision) -> &'static str {
    match decision {
        Decision::AllowOnce => "allow_once",
        Decision::AllowAlways => "allow_always",
        Decision::Deny => "deny",
        Decision::Interrupt => "interrupt",
    }
}

/// Machine label for one answer outcome. Spelled out rather than derived from
/// `Debug`, so `NotBlocked` reads as `not_blocked` in the payload.
pub(crate) fn agent_answer_outcome_label(outcome: subagent::AgentAnswerOutcome) -> &'static str {
    match outcome {
        subagent::AgentAnswerOutcome::Answered => "answered",
        subagent::AgentAnswerOutcome::NotBlocked => "not_blocked",
        subagent::AgentAnswerOutcome::NotFound => "not_found",
    }
}

/// What the user reads after `/jobs reply`. A non-blocked agent is a reported
/// no-op, not a failure: the id may have been answered, timed out, or finished
/// between reading the deck and typing.
pub(crate) fn jobs_reply_receipt(
    id: u64,
    outcome: subagent::AgentAnswerOutcome,
    decision: Decision,
) -> String {
    match outcome {
        subagent::AgentAnswerOutcome::Answered => format!(
            "answered background agent #{id}: {}",
            permission_decision_label(decision)
        ),
        subagent::AgentAnswerOutcome::NotBlocked => {
            format!("background agent #{id} is not waiting on an answer")
        }
        subagent::AgentAnswerOutcome::NotFound => format!("no background job #{id}"),
    }
}

pub(crate) async fn handle_jobs_command_attaching(
    registry: &Arc<subagent::AgentRegistry>,
    journal: &kitsu_tools::builtins::fs::WriteJournal,
    arg: &str,
    attached: &mut Option<u64>,
    // Card **T4-S7**: `/jobs view` is the agent view's **second** route, and
    // it must be able to dispatch too. **F79**'s lesson applied before the
    // defect: a carrier that reached only the chord route would start agents
    // on one route and silently refuse on the other.
    view_dispatcher: Option<&subagent::ViewDispatcher>,
) -> Result<serde_json::Value, ()> {
    let mut parts = arg.split_whitespace();
    let action = parts.next().unwrap_or("list");
    match action {
        "list" if parts.next().is_none() => {
            eprintln!("{}", format_jobs(registry));
            Ok(serde_json::json!({
                "kind": "jobs",
                "action": "list",
                "changed": false,
                "snapshot": jobs_snapshot_payload(registry),
            }))
        }
        "view" if parts.next().is_none() => {
            // Card T4-S2, requirement A1: in a TUI mode `/jobs view` *is* the
            // overlay. The text deck is not also printed — it would land in
            // the transcript underneath an alternate-screen overlay, where the
            // user never sees it and it still scrolls their history. Classic
            // mode opens no overlay, so it keeps the deck unchanged.
            // **F79**: the exit carries *two* answers and this arm used to
            // keep only `shown`, so `/jobs view` + `Enter` attached to
            // nothing. The attachment goes where the inline reader puts it.
            #[cfg(unix)]
            let shown = apply_jobs_view_exit(
                &run_agent_view_overlay(registry, true, view_dispatcher),
                attached,
            );
            #[cfg(not(unix))]
            let shown = {
                let _ = view_dispatcher;
                false
            };
            if !shown {
                eprintln!("{}", format_jobs_view(registry));
            }
            Ok(serde_json::json!({
                "kind": "jobs",
                "action": "view",
                "changed": false,
                "snapshot": jobs_snapshot_payload(registry),
            }))
        }
        "show" => {
            let Some(id) = parts.next().and_then(|value| value.parse::<u64>().ok()) else {
                eprintln!("usage: /jobs show <id>");
                return Err(());
            };
            if parts.next().is_some() {
                eprintln!("usage: /jobs show <id>");
                return Err(());
            }
            let Some(thread) = registry.thread_snapshot(id) else {
                eprintln!("no background job #{id}");
                return Err(());
            };
            eprintln!("{}", format_agent_thread(&thread));
            let result = registry.peek_result(id);
            if !matches!(thread.phase, subagent::thread::AgentThreadPhase::Running) {
                if let Some(result) = result.as_deref() {
                    eprintln!(
                        "\nresult:\n{}",
                        kitsu_render::sanitize_terminal_text(result)
                    );
                }
                let _ = registry.acknowledge_result(id);
            }
            Ok(serde_json::json!({
                "kind": "jobs",
                "action": "show",
                "changed": false,
                "id": id,
                "thread": subagent::thread::AgentThreadSnapshotDto::from_snapshot(&thread),
                "result": result,
                "handoff": registry.handoff_state(id),
                "snapshot": jobs_snapshot_payload(registry),
            }))
        }
        "explain" => {
            let Some(id) = parts.next().and_then(|value| value.parse::<u64>().ok()) else {
                eprintln!("usage: /jobs explain <id>");
                return Err(());
            };
            if parts.next().is_some() {
                eprintln!("usage: /jobs explain <id>");
                return Err(());
            }
            let Some(explanation) = agent_explain_view(registry, id) else {
                eprintln!("no background job #{id}");
                return Err(());
            };
            eprintln!("{explanation}");
            Ok(serde_json::json!({
                "kind": "jobs",
                "action": "explain",
                "changed": false,
                "id": id,
                "thread": registry
                    .thread_snapshot(id)
                    .as_ref()
                    .map(subagent::thread::AgentThreadSnapshotDto::from_snapshot),
                "handoff": registry.handoff_state(id),
                "snapshot": jobs_snapshot_payload(registry),
            }))
        }
        "steer" => {
            let Some(id) = parts.next().and_then(|value| value.parse::<u64>().ok()) else {
                eprintln!("usage: /jobs steer <id> <correction>");
                return Err(());
            };
            let correction = parts.collect::<Vec<_>>().join(" ");
            if correction.trim().is_empty() {
                eprintln!("usage: /jobs steer <id> <correction>");
                return Err(());
            }
            match registry.steer(id, &correction) {
                Ok(receipt) => {
                    eprintln!("~ {}", format_steering_receipt(&receipt));
                    Ok(serde_json::json!({
                        "kind": "jobs",
                        "action": "steer",
                        "changed": true,
                        "receipt": {
                            "id": receipt.id,
                            "sequence": receipt.sequence,
                            "pending": receipt.pending,
                            "restarts_thread": true,
                            "discarded_ms": receipt.discarded_ms,
                        },
                        "snapshot": jobs_snapshot_payload(registry),
                    }))
                }
                Err(error) => {
                    eprintln!("cannot steer background agent #{id}: {error}");
                    Err(())
                }
            }
        }
        "close-steering" => {
            let Some(id) = parts.next().and_then(|value| value.parse::<u64>().ok()) else {
                eprintln!("usage: /jobs close-steering <id>");
                return Err(());
            };
            if parts.next().is_some() {
                eprintln!("usage: /jobs close-steering <id>");
                return Err(());
            }
            let outcome = registry.close_steering(id);
            let changed = matches!(outcome, subagent::AgentSteeringCloseOutcome::Closed);
            eprintln!("~ background agent #{id} steering: {outcome:?}");
            Ok(serde_json::json!({
                "kind": "jobs",
                "action": "close_steering",
                "changed": changed,
                "id": id,
                "outcome": format!("{outcome:?}").to_lowercase(),
                "snapshot": jobs_snapshot_payload(registry),
            }))
        }
        "stop" => {
            let Some(id) = parts.next().and_then(|value| value.parse::<u64>().ok()) else {
                eprintln!("usage: /jobs stop <id>");
                return Err(());
            };
            if parts.next().is_some() {
                eprintln!("usage: /jobs stop <id>");
                return Err(());
            }
            match registry.stop(id).await {
                subagent::AgentCancelOutcome::Cancelled(status) => {
                    report_write_checkpoints(journal.drain_and_take_reports().await);
                    eprintln!("~ stopped and joined background job #{}", status.id);
                    Ok(serde_json::json!({
                        "kind": "jobs",
                        "action": "stop",
                        "changed": true,
                        "job": terminal_job_payload(&status),
                        "snapshot": jobs_snapshot_payload(registry),
                    }))
                }
                subagent::AgentCancelOutcome::AlreadyTerminal(status) => {
                    eprintln!(
                        "~ background job #{} is already {}",
                        status.id,
                        terminal_agent_status_label(&status.status)
                    );
                    Ok(serde_json::json!({
                        "kind": "jobs",
                        "action": "stop",
                        "changed": false,
                        "job": terminal_job_payload(&status),
                        "snapshot": jobs_snapshot_payload(registry),
                    }))
                }
                subagent::AgentCancelOutcome::NotFound => {
                    eprintln!("no background job #{id}");
                    Err(())
                }
            }
        }
        "stop-all" if parts.next().is_none() => {
            let stopped = registry.cancel_all().await;
            report_write_checkpoints(journal.drain_and_take_reports().await);
            eprintln!("~ stopped and joined {} background job(s)", stopped.len());
            let jobs = stopped.iter().map(terminal_job_payload).collect::<Vec<_>>();
            Ok(serde_json::json!({
                "kind": "jobs",
                "action": "stop_all",
                "changed": !jobs.is_empty(),
                "jobs": jobs,
                "snapshot": jobs_snapshot_payload(registry),
            }))
        }
        "prune" | "close" => {
            let Some(target) = parts.next() else {
                eprintln!("usage: /jobs prune <id|all>");
                return Err(());
            };
            if parts.next().is_some() {
                eprintln!("usage: /jobs prune <id|all>");
                return Err(());
            }
            if target == "all" {
                let pruned = registry.prune_terminal();
                eprintln!("~ pruned {} terminal background job(s)", pruned.len());
                let jobs = pruned.iter().map(terminal_job_payload).collect::<Vec<_>>();
                return Ok(serde_json::json!({
                    "kind": "jobs",
                    "action": "prune_all",
                    "changed": !jobs.is_empty(),
                    "jobs": jobs,
                    "snapshot": jobs_snapshot_payload(registry),
                }));
            }
            let Ok(id) = target.parse::<u64>() else {
                eprintln!("usage: /jobs prune <id|all>");
                return Err(());
            };
            let unviewed =
                registry.handoff_state(id) == Some(subagent::thread::AgentHandoffState::Ready);
            match registry.close(id) {
                subagent::AgentPruneOutcome::Pruned(status) => {
                    if unviewed {
                        print_background_results(std::slice::from_ref(&status));
                    }
                    eprintln!("~ pruned background job #{id}");
                    Ok(serde_json::json!({
                        "kind": "jobs",
                        "action": "prune",
                        "changed": true,
                        "job": terminal_job_payload(&status),
                        "snapshot": jobs_snapshot_payload(registry),
                    }))
                }
                subagent::AgentPruneOutcome::Running => {
                    eprintln!("background job #{id} is still running; stop it first");
                    Err(())
                }
                subagent::AgentPruneOutcome::NotFound => {
                    eprintln!("no background job #{id}");
                    Err(())
                }
            }
        }
        "reply" => {
            let Some((id, decision)) = parse_jobs_reply(&mut parts) else {
                eprintln!("{JOBS_REPLY_USAGE}");
                return Err(());
            };
            let outcome = registry.answer(id, decision);
            if matches!(outcome, subagent::AgentAnswerOutcome::NotFound) {
                eprintln!("no background job #{id}");
                return Err(());
            }
            eprintln!("~ {}", jobs_reply_receipt(id, outcome, decision));
            Ok(serde_json::json!({
                "kind": "jobs",
                "action": "reply",
                "changed": matches!(outcome, subagent::AgentAnswerOutcome::Answered),
                "id": id,
                "outcome": agent_answer_outcome_label(outcome),
                "decision": permission_decision_label(decision),
                "snapshot": jobs_snapshot_payload(registry),
            }))
        }
        _ => {
            // The unknown action is always named: a spelling far enough from
            // every subcommand to have no nearest suggestion is exactly the
            // one the user most needs told back to them.
            match closest_jobs_subcommand(action) {
                Some(suggestion) => {
                    eprintln!("unknown action '{action}' - did you mean '{suggestion}'?")
                }
                None => eprintln!("unknown action '{action}'"),
            }
            eprintln!("usage: /jobs [list|view|show <id>|explain <id>|steer <id> <text>|close-steering <id>|stop <id>|stop-all|prune <id|all>|reply <id> <y|a|n>]");
            Err(())
        }
    }
}

pub(crate) fn agent_profiles_payload(
    catalog: &subagent::profile::AgentProfileCatalog,
) -> serde_json::Value {
    serde_json::json!({
        "kind": "agent_profiles",
        "count": catalog.len(),
        "profiles": catalog.to_dto().profiles,
    })
}

/// Human `/profiles`, rendered from the same catalog DTO the JSONL payload
/// serializes: DTO spellings for effort, the tool names themselves instead of
/// `AllowList([...])`, and the two fields that actually decide which profile to
/// pick — the instruction line and whether it inherits session grants.
pub(crate) fn format_agent_profiles(catalog: &subagent::profile::AgentProfileCatalog) -> String {
    if catalog.is_empty() {
        return "no background-agent profiles are available".to_string();
    }
    let mut output = kitsu_render::deck_header(&format!(
        "background-agent profiles — {} available",
        catalog.len()
    ));
    for profile in catalog.iter() {
        let (tools, can_act) = match &profile.tool_policy {
            subagent::profile::AgentToolPolicy::None => {
                ("no tools (reasoning only)".to_string(), false)
            }
            subagent::profile::AgentToolPolicy::Inherit => {
                ("inherits this run's authorized tools".to_string(), true)
            }
            // An allow-list intersected down to nothing is honest behaviour with
            // an invisible explanation; say why instead of printing `AllowList([])`.
            subagent::profile::AgentToolPolicy::AllowList(tools) if tools.is_empty() => (
                "no tools - this run granted no read roots".to_string(),
                false,
            ),
            subagent::profile::AgentToolPolicy::AllowList(tools) => (
                tools
                    .iter()
                    .map(|tool| terminal_inline(tool))
                    .collect::<Vec<_>>()
                    .join(", "),
                true,
            ),
        };
        // Filled = this profile can act; hollow = it can only reason.
        let marker = if kitsu_render::plain_mode() {
            "-"
        } else if can_act {
            kitsu_render::GLYPH_TOOL_CALL
        } else {
            kitsu_render::GLYPH_TOOL_RESULT
        };
        output.push_str(&format!(
            "\n  {} {} | model {} | effort {} | tools {} | {}",
            marker,
            terminal_inline(&profile.name),
            profile
                .model
                .as_deref()
                .map(terminal_inline)
                .unwrap_or_else(|| "session model".to_string()),
            profile.reasoning_effort.label(),
            tools,
            if profile.permission_defaults.inherit_session {
                "inherits session grants"
            } else {
                "isolated from session grants"
            },
        ));
        if !profile.instructions.trim().is_empty() {
            output.push_str(&format!(
                "\n      {}",
                terminal_inline(profile.instructions.trim())
            ));
        }
    }
    output.push_str("\n~ background agents select one with spawn_agent_bg({task, profile})");
    output
}

pub(crate) async fn await_background_agents(
    registry: &Arc<subagent::AgentRegistry>,
    reason: &str,
) -> kitsu_core::RunCleanupState {
    let running = registry.running();
    if running == 0 {
        return kitsu_core::RunCleanupState::Joined;
    }
    if !jsonl_requested() {
        eprintln!(
            "{reason}; waiting for {running} background agent(s) to finish (Ctrl+C cancels them)"
        );
    }
    let mut changes = registry.subscribe();
    while changes.borrow().running > 0 {
        tokio::select! {
            changed = changes.changed() => {
                if changed.is_err() {
                    break;
                }
            }
            _ = tokio::signal::ctrl_c() => {
                let cancelled = registry.cancel_started_after_with_status(0).await;
                let cleanup = terminal_cleanup_state(&cancelled);
                report_cancelled_agents(cancelled.iter().map(|status| status.id).collect());
                return cleanup;
            }
        }
    }
    if registry.running() == 0 {
        kitsu_core::RunCleanupState::Joined
    } else {
        kitsu_core::RunCleanupState::NotObserved
    }
}

/// Why a non-interactive shutdown stopped waiting. Only a bound that was
/// actually hit may be named in the cancellation text.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ShutdownJoinStop {
    Timeout,
    Interrupted,
    RegistryClosed,
}

/// Non-interactive end-of-input handling for live background agents: join them
/// the way `/quit wait` does, bounded by `timeout`, and cancel only when the
/// bound (or an interrupt) is actually reached.
///
/// Returns the agents that had to be cancelled — empty when everything finished
/// on its own, in which case the run's detached work is complete and its
/// results are real results, not casualties of the exit path.
pub(crate) async fn join_background_agents_at_exit(
    registry: &Arc<subagent::AgentRegistry>,
    journal: &kitsu_tools::builtins::fs::WriteJournal,
    timeout: std::time::Duration,
    quiet: bool,
) -> Vec<subagent::AgentTerminalStatus> {
    let running = registry.running();
    if running == 0 {
        return Vec::new();
    }
    if !quiet {
        eprintln!(
            "~ input ended with {running} background agent(s) running; joining them (up to {}s; Ctrl+C cancels, --agent-join-timeout adjusts)",
            timeout.as_secs()
        );
    }
    let mut changes = registry.subscribe();
    let deadline = tokio::time::sleep(timeout);
    tokio::pin!(deadline);
    // Backed-off join heartbeat (cycle-10 G4): cumulative elapsed plus
    // per-agent state counts, so a long join reads as progress, not a wedge,
    // without stacking a line every few seconds.
    let join_started = std::time::Instant::now();
    let mut beat: u32 = 0;
    let heartbeat = tokio::time::sleep(kitsu_render::heartbeat_backoff(beat));
    tokio::pin!(heartbeat);
    let mut stop = None;
    while changes.borrow().running > 0 {
        tokio::select! {
            changed = changes.changed() => {
                if changed.is_err() {
                    stop = Some(ShutdownJoinStop::RegistryClosed);
                    break;
                }
            }
            () = &mut deadline => {
                stop = Some(ShutdownJoinStop::Timeout);
                break;
            }
            () = &mut heartbeat, if !quiet => {
                let (live, finished, failed) = {
                    let snapshot = changes.borrow();
                    (snapshot.running, snapshot.terminal, snapshot.failed)
                };
                let failures = if failed > 0 {
                    format!(" ({failed} failed)")
                } else {
                    String::new()
                };
                let message = format!(
                    "still waiting {} — {live} background agent(s) running, {finished} finished{failures}",
                    kitsu_render::format_wait_elapsed(join_started.elapsed())
                );
                kitsu_render::PromptSafeAsyncNotice::info(message)
                    .write_line(&mut std::io::stderr())
                    .ok();
                beat += 1;
                heartbeat
                    .as_mut()
                    .reset(tokio::time::Instant::now() + kitsu_render::heartbeat_backoff(beat));
            }
            _ = tokio::signal::ctrl_c() => {
                stop = Some(ShutdownJoinStop::Interrupted);
                break;
            }
        }
    }
    let cancelled = match stop {
        None => Vec::new(),
        Some(stop) => {
            let reason = match stop {
                ShutdownJoinStop::Timeout => format!(
                    "cancelled by shutdown policy: still running after the {}s exit join",
                    timeout.as_secs()
                ),
                ShutdownJoinStop::Interrupted => {
                    "cancelled by shutdown policy: the exit join was interrupted".to_string()
                }
                ShutdownJoinStop::RegistryClosed => {
                    "cancelled by shutdown policy: the agent registry closed during the exit join"
                        .to_string()
                }
            };
            registry.cancel_all_with_reason(&reason).await
        }
    };
    if !quiet {
        if cancelled.is_empty() {
            eprintln!("~ joined {running} background agent(s) before exit");
        } else {
            let ids = cancelled
                .iter()
                .map(|status| format!("#{}", status.id))
                .collect::<Vec<_>>()
                .join(", ");
            eprintln!(
                "~ cancelled {} unfinished background agent(s) by shutdown policy: {ids} (nothing was rolled back; completed file changes remain on disk)",
                cancelled.len()
            );
        }
    }
    report_write_checkpoints(journal.drain_and_take_reports().await);
    cancelled
}

pub(crate) fn terminal_cleanup_state(
    terminal: &[subagent::AgentTerminalStatus],
) -> kitsu_core::RunCleanupState {
    let unjoined = terminal.iter().filter(|status| !status.joined).count();
    let Some(unjoined) = std::num::NonZeroU32::new(u32::try_from(unjoined).unwrap_or(u32::MAX))
    else {
        return kitsu_core::RunCleanupState::Joined;
    };
    kitsu_core::RunCleanupState::Residue { unjoined }
}

/// Surface every completed detached result that was never opened through
/// `/jobs show` or `agent_result`. Shutdown is the last point at which an
/// interactive, in-memory registry can preserve that work, so silently dropping
/// it would make a successful background task indistinguishable from data loss.
pub(crate) fn print_background_results(results: &[subagent::AgentTerminalStatus]) {
    for result in results {
        eprintln!(
            "~ background job #{} result ({}):",
            result.id,
            terminal_inline(&result.task)
        );
        // A detached failure is rendered as an error, not as a body: a worker
        // that never ran must never read like a delivered result.
        if let subagent::AgentStatus::Failed(error) = &result.status {
            kitsu_render::render_error(&format!("background job #{} failed: {error}", result.id));
            continue;
        }
        let body = match &result.status {
            subagent::AgentStatus::Done(body) => body.clone(),
            subagent::AgentStatus::Cancelled(reason) => format!("cancelled: {reason}"),
            subagent::AgentStatus::Failed(_)
            | subagent::AgentStatus::Running
            | subagent::AgentStatus::Blocked => continue,
        };
        eprintln!("{}", kitsu_render::sanitize_terminal_text(&body));
    }
}

pub(crate) fn background_result_payload(
    result: &subagent::AgentTerminalStatus,
) -> serde_json::Value {
    let (status, body) = match &result.status {
        subagent::AgentStatus::Done(body) => ("done", body.clone()),
        subagent::AgentStatus::Failed(error) => ("failed", error.clone()),
        subagent::AgentStatus::Cancelled(reason) => ("cancelled", reason.clone()),
        // B4: no fifth status word. A blocked agent is non-terminal and so
        // reports as `running`; a later stage needing it adds its own event.
        subagent::AgentStatus::Running | subagent::AgentStatus::Blocked => {
            ("running", String::new())
        }
    };
    serde_json::json!({
        "id": result.id,
        "task": result.task,
        "status": status,
        "result": body,
        "joined": result.joined,
    })
}

pub(crate) fn surface_unobserved_background_results(registry: &subagent::AgentRegistry) -> usize {
    let results = registry.take_unobserved_terminal_results();
    print_background_results(&results);
    results.len()
}

pub(crate) fn emit_script_background_results(
    results: &[subagent::AgentTerminalStatus],
    jsonl: bool,
) -> Result<(), CliError> {
    if jsonl {
        for result in results {
            emit_jsonl("background_result", background_result_payload(result))?;
        }
    } else {
        print_background_results(results);
    }
    Ok(())
}

/// Cancel agents started after `watermark` and report their write checkpoints.
/// The drained reports are returned so a rollback site can brief the next turn
/// about file changes that survived the rollback (cycle-10 G1).
pub(crate) async fn cancel_agents_and_report_writes(
    registry: &Arc<subagent::AgentRegistry>,
    watermark: u64,
    journal: &kitsu_tools::builtins::fs::WriteJournal,
) -> Vec<kitsu_tools::builtins::fs::JournalTurnReport> {
    cancel_agents_and_report_writes_with_cleanup(registry, watermark, journal)
        .await
        .reports
}

pub(crate) struct CancelledAgentWrites {
    pub(crate) reports: Vec<kitsu_tools::builtins::fs::JournalTurnReport>,
    pub(crate) cleanup: kitsu_core::RunCleanupState,
}

pub(crate) async fn cancel_agents_and_report_writes_with_cleanup(
    registry: &Arc<subagent::AgentRegistry>,
    watermark: u64,
    journal: &kitsu_tools::builtins::fs::WriteJournal,
) -> CancelledAgentWrites {
    let cancelled = registry.cancel_started_after_with_status(watermark).await;
    let cleanup = terminal_cleanup_state(&cancelled);
    report_cancelled_agents(cancelled.iter().map(|status| status.id).collect());
    let reports = journal.drain_and_take_reports().await;
    report_write_checkpoints(reports.clone());
    CancelledAgentWrites { reports, cleanup }
}
