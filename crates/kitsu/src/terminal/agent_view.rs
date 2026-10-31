//! Terminal agent overlays and attachment routing.

use kitsu_tools::Decision;

use crate::agent_commands::format_agent_thread;
use crate::agent_commands::format_steering_receipt;
use crate::agent_commands::jobs_reply_receipt;
use crate::agent_commands::pending_approval_request;
use crate::agent_commands::terminal_agent_status_label;
use crate::chat::commands::SlashResult;
use crate::presentation::terminal_inline;
use crate::subagent;
use crate::terminal::approval::parse_permission_answer;
#[cfg(unix)]
use crate::terminal::overlay_input::overlay_block_on;
#[cfg(unix)]
use crate::terminal::overlay_input::stdin_waiting;
#[cfg(unix)]
use crate::terminal::overlay_input::OverlayDecoder;
#[cfg(unix)]
use crate::terminal::overlay_input::OverlayInput;
#[cfg(unix)]
use crate::terminal::overlay_input::OverlayKey;
#[cfg(unix)]
use crate::terminal::overlay_input::OverlayLine;
#[cfg(unix)]
use crate::terminal::overlay_input::OverlayPrompt;
#[cfg(unix)]
use crate::terminal::overlay_input::SteerLine;
#[cfg(unix)]
use crate::terminal::overlay_input::SteerLineStep;
#[cfg(unix)]
use crate::terminal::overlay_input::ESC_SEQUENCE_GRACE;
#[cfg(unix)]
use crate::terminal::typeahead::RawStdinGuard;
#[cfg(unix)]
use crate::tui_bridge;

/// The agent view's rows, built from the registry (card T4-S2).
///
/// The thread DTO is the source of the shape — `tui_bridge`'s
/// [`agent_row_from_thread_dto`](tui_bridge::agent_row_from_thread_dto) is the
/// only mapping, and this is its first non-test caller — and the progress
/// snapshot fills in the three fields the DTO cannot carry (the tokens and the
/// activity with its age, which live on `AgentTelemetry`). Nothing is invented
/// where neither source has an answer: the fields stay absent.
/// The rows the overlay is showing **right now**: the registry's rows with the
/// filter line's filter applied (card T4-S5, requirement A3's `/ filter`).
///
/// It exists so that the loop and the registry-level tests ask the same
/// question once. The selection is synced against *these* rows and
/// [`handle_overlay_input`] is handed *these* rows, which is the card's safety
/// answer: a filter that hid the selected agent while the selection still
/// pointed at it would let `x`, `r` or `n` act on an agent the user cannot
/// see, which is **F68**'s family — a privileged action without visible
/// intent.
#[cfg(unix)]
pub(crate) fn overlay_visible_rows(
    registry: &subagent::AgentRegistry,
    state: &OverlayState,
) -> Vec<kitsu_tui::agents_view::AgentRow> {
    // The **one** place the filter is applied. The empty filter matches every
    // row, so an unfiltered view is this same expression and not a branch.
    agent_view_rows(registry)
        .into_iter()
        .filter(|row| state.filter.matches(row))
        .collect()
}

#[cfg(unix)]
pub(crate) fn agent_view_rows(
    registry: &subagent::AgentRegistry,
) -> Vec<kitsu_tui::agents_view::AgentRow> {
    let progress = registry.progress_snapshot();
    registry
        .thread_snapshot_dtos()
        .iter()
        .map(|dto| {
            let mut row = tui_bridge::agent_row_from_thread_dto(dto);
            if let Some(agent) = progress.agents.iter().find(|agent| agent.id == row.id) {
                row.tokens = Some(agent.telemetry.total_tokens);
                row.last_activity = agent.telemetry.last_activity.clone();
                row.last_activity_age = agent
                    .last_activity_ms_ago
                    .map(std::time::Duration::from_millis);
            }
            row
        })
        .collect()
}

/// Card T4-S3: `Space` peeks the selected row of the agent view.
///
/// Returns the panel's rows for `id`, or `None` for an id the registry does
/// not hold.
///
/// Requirement A4: the panel shows the bounded redacted timeline `/jobs show`
/// renders, the pending approval request while the agent is blocked, and the
/// result once it is terminal. It never shows a tool argument or raw tool
/// output — by construction, because the thread snapshot it reads carries an
/// approval **kind** only (F40, T2-S6).
///
/// Requirement A7: peeking a terminal agent acknowledges its handoff exactly
/// as `/jobs show` does (`peek_result` then `acknowledge_result`), so the exit
/// guard on unviewed results and the strict-automation rules are unchanged.
/// Only peeking does this: building the view's rows, opening it, or moving the
/// selection acknowledges nothing.
#[cfg(unix)]
pub(crate) fn agent_view_peek(registry: &subagent::AgentRegistry, id: u64) -> Option<Vec<String>> {
    let thread = registry.thread_snapshot(id)?;
    // `/jobs show`'s own renderer, not a second one: the phase label, the
    // handoff and the `bounded redacted timeline:` section are whatever that
    // surface prints, so the two cannot drift.
    let mut lines: Vec<String> = format_agent_thread(&thread)
        .lines()
        .map(str::to_string)
        .collect();
    // The pending request while the agent is blocked, by kind only — the same
    // `permission to <kind>` line the deck's blocked row carries.
    if let Some(request) = pending_approval_request(&thread) {
        lines.push(format!("  waiting on: {request}"));
    }
    // The result, and the acknowledgement that goes with having seen it,
    // exactly as `/jobs show` sequences them: `peek_result` then
    // `acknowledge_result`, and neither for an agent that is still running.
    // A blocked agent has no handoff, so the acknowledgement is a no-op there.
    if !matches!(thread.phase, subagent::thread::AgentThreadPhase::Running) {
        if let Some(result) = registry.peek_result(id) {
            lines.push("result:".to_string());
            lines.extend(
                kitsu_render::sanitize_terminal_text(&result)
                    .lines()
                    .map(str::to_string),
            );
        }
        let _ = registry.acknowledge_result(id);
    }
    Some(lines)
}

/// The peek panel as an overlay frame: the view's own title row, so the
/// surface on screen is still recognisably the agent view, and the panel
/// under it fitted to the terminal's width.
#[cfg(unix)]
pub(crate) fn agent_view_peek_frame(
    registry: &subagent::AgentRegistry,
    id: u64,
    cols: usize,
) -> Option<Vec<String>> {
    let panel = agent_view_peek(registry, id)?;
    let mut rows = vec![
        format!(
            "{} \u{2014} agent #{id}",
            kitsu_tui::agents_view::OVERLAY_TITLE
        ),
        String::new(),
    ];
    rows.extend(panel);
    Some(
        rows.into_iter()
            .map(|row| kitsu_render::fit_row_to_width(&row, cols))
            .collect(),
    )
}

/// What one of the agent view's action keys did, in the words the overlay
/// shows (card T4-S4b, requirement A3's `r`, `s`, `x`, `n`, `p`).
///
/// Requirement **T2-S5b** — a human must be able to tell *nothing happened*
/// from *it worked* — is a **wiring** obligation here rather than a registry
/// change: all five registry calls already answer with distinguishable
/// outcome enums, and this is the type that must not collapse them into one
/// line. `changed` is that distinction made machine-readable: `false` is every
/// "nothing happened" answer, including every refusal.
#[cfg(unix)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AgentViewReport {
    /// The line the overlay paints under the row list.
    pub(crate) line: String,
    /// Whether the registry changed. Every refusal is `false`.
    pub(crate) changed: bool,
}

/// `r`: answer the selected agent's parked permission request.
///
/// `typed` is the one key the decision read collected, and it is resolved by
/// [`parse_permission_answer`] — **the** parser, the second one having been
/// retired to close **F45**. A spelling that parser does not accept is
/// refused here rather than guessed at.
#[cfg(unix)]
pub(crate) fn agent_view_reply(
    registry: &subagent::AgentRegistry,
    id: u64,
    typed: &str,
) -> AgentViewReport {
    let Some(decision) = parse_permission_answer(typed) else {
        // Not guessed at, and nothing is sent: the agent stays blocked, which
        // is the one state from which the user can still answer it correctly.
        return AgentViewReport {
            line: format!(
                "cannot read {} as an answer for background agent #{id}; press y, a or n",
                terminal_inline(typed.trim())
            ),
            changed: false,
        };
    };
    let outcome = registry.answer(id, decision);
    AgentViewReport {
        // The receipt `/jobs reply` prints, not a second wording of it.
        line: jobs_reply_receipt(id, outcome, decision),
        changed: matches!(outcome, subagent::AgentAnswerOutcome::Answered),
    }
}

/// `s`: steer the selected agent with the line the sub-mode collected.
///
/// An empty or whitespace-only correction is a **refusal**, not a no-op:
/// steering restarts the thread and discards every millisecond it has already
/// spent, so a blank line must never reach the registry. On success the
/// receipt is shown — including the progress it discarded.
#[cfg(unix)]
pub(crate) fn agent_view_steer(
    registry: &subagent::AgentRegistry,
    id: u64,
    correction: &str,
) -> AgentViewReport {
    if correction.trim().is_empty() {
        // Refused here rather than at the registry: a blank correction would
        // throw away the thread's progress for nothing. The registry answers
        // the same `Empty`, so the line is the same either way.
        return agent_view_steer_report(Err(subagent::thread::AgentSteeringError::new(
            id,
            subagent::thread::AgentSteeringErrorKind::Empty,
            None,
        )));
    }
    agent_view_steer_report(registry.steer(id, correction))
}

/// `s`'s report, over the registry's own answer.
///
/// Split from [`agent_view_steer`] for one reason, and it is the reason this
/// key is a card of its own: a *steerable* agent is one with a live thread and
/// an open correction channel (`AgentSteeringErrorKind::Closed` otherwise), so
/// the success path cannot be reached from a registry a unit test builds with
/// `start`. The wording is therefore pinned over a receipt, and
/// `agent_view_steer` is this function applied to `AgentRegistry::steer`.
#[cfg(unix)]
pub(crate) fn agent_view_steer_report(
    outcome: Result<subagent::thread::AgentSteeringReceipt, subagent::thread::AgentSteeringError>,
) -> AgentViewReport {
    match outcome {
        // The registry's own receipt, which is what `/jobs steer` prints: it
        // already names the restart and the in-flight progress discarded.
        Ok(receipt) => AgentViewReport {
            line: format_steering_receipt(&receipt),
            changed: true,
        },
        // And the registry's own words for every refusal, for the same
        // reason: a second wording of `Closed` would be a second source of
        // truth about what the key just did.
        Err(error) => AgentViewReport {
            line: error.to_string(),
            changed: false,
        },
    }
}

/// `x`, confirmed: stop a live agent, or prune an already-terminal one.
///
/// A3 says *"`x` stop (twice to prune)"* and T4-S4a built the arming whose
/// second press is `StopPress::Confirmed`. Both are satisfied without a third
/// press and without changing T4-S4a: stopping an agent that has already
/// finished is meaningless (`AlreadyTerminal` changes nothing), so the second
/// press on a finished agent is the prune. It is sequenced as
/// `stop(id).await` *then* prune, rather than as a phase read followed by a
/// call, so no agent can finish in the gap between deciding and acting.
///
/// One exception, and it is the reason this is not a bare `prune`: pruning
/// **removes the record**, so pruning an agent whose result nobody has read
/// yet would destroy it and silence the exit guard
/// (`take_unobserved_terminal_results` can only see records that exist). An
/// unviewed result is therefore refused with a report saying so; `Space` or
/// `p` collects it first.
#[cfg(unix)]
pub(crate) async fn agent_view_stop_or_prune(
    registry: &subagent::AgentRegistry,
    id: u64,
) -> AgentViewReport {
    match registry.stop(id).await {
        subagent::AgentCancelOutcome::Cancelled(status) => {
            // The agent the user just stopped by hand: its cancellation is
            // delivered *here*, on the report line, so the exit guard does
            // not print it again as an unobserved result at shutdown. This is
            // custody of the row the key was pressed on and of no other.
            let _ = registry.acknowledge_result(status.id);
            AgentViewReport {
                line: format!("stopped background agent #{}", status.id),
                changed: true,
            }
        }
        subagent::AgentCancelOutcome::AlreadyTerminal(status) => {
            // Already finished, so stopping it is meaningless and this press
            // is the prune A3 asks the second press for -- except when the
            // result is still waiting. `prune` *removes the record*, and
            // `take_unobserved_terminal_results` can only report records that
            // exist, so pruning here would destroy the result and silence the
            // exit guard with it.
            if registry.handoff_state(id) == Some(subagent::thread::AgentHandoffState::Ready) {
                return AgentViewReport {
                    line: format!(
                        "background agent #{id} has a result nobody has read; peek it with space or acknowledge it with p, then press x again"
                    ),
                    changed: false,
                };
            }
            match registry.prune(id) {
                subagent::AgentPruneOutcome::Pruned(status) => AgentViewReport {
                    line: format!(
                        "pruned background agent #{} ({})",
                        status.id,
                        terminal_agent_status_label(&status.status)
                    ),
                    changed: true,
                },
                // Neither is reachable from the sequencing above -- the record
                // was terminal under the same lock a moment ago -- but a
                // report is cheaper than an unwrap.
                subagent::AgentPruneOutcome::Running => AgentViewReport {
                    line: format!("background agent #{} is still running", status.id),
                    changed: false,
                },
                subagent::AgentPruneOutcome::NotFound => AgentViewReport {
                    line: format!("no background job #{id}"),
                    changed: false,
                },
            }
        }
        subagent::AgentCancelOutcome::NotFound => AgentViewReport {
            line: format!("no background job #{id}"),
            changed: false,
        },
    }
}

/// `n`: name the selected agent. Legality is `AgentRegistry::rename`'s, pinned
/// by T4-S4a; this is the wiring that makes each of its refusals a line.
#[cfg(unix)]
pub(crate) fn agent_view_rename(
    registry: &subagent::AgentRegistry,
    id: u64,
    name: &str,
) -> AgentViewReport {
    match registry.rename(id, name) {
        subagent::AgentRenameOutcome::Renamed { previous } => AgentViewReport {
            line: match previous {
                Some(previous) => format!(
                    "named background agent #{id} {} (was {})",
                    terminal_inline(name.trim()),
                    terminal_inline(&previous)
                ),
                None => format!(
                    "named background agent #{id} {}",
                    terminal_inline(name.trim())
                ),
            },
            changed: true,
        },
        // The refusals carry no name: an empty one and a whitespace-only one
        // are the same refusal, which is `AgentRenameOutcome::Empty`'s own
        // grouping, and an unprintable one must not be echoed into the paint.
        subagent::AgentRenameOutcome::Empty => AgentViewReport {
            line: format!("a name for background agent #{id} cannot be empty"),
            changed: false,
        },
        subagent::AgentRenameOutcome::TooLong { max } => AgentViewReport {
            line: format!(
                "a name for background agent #{id} is at most {max} characters; it was not shortened for you"
            ),
            changed: false,
        },
        subagent::AgentRenameOutcome::Unprintable => AgentViewReport {
            line: format!(
                "a name for background agent #{id} cannot carry control characters"
            ),
            changed: false,
        },
        subagent::AgentRenameOutcome::NotFound => AgentViewReport {
            line: format!("no background job #{id}"),
            changed: false,
        },
    }
}

/// The dispatch key: start a background agent on the line the composer
/// collected (card **T4-S7**, requirement **A6**).
///
/// Split from the overlay's key arm for [`agent_view_steer_report`]'s reason,
/// and it is the same fixture reason: a dispatch that really starts an agent
/// needs a provider, a subagent registry and a profile catalog — none of which
/// the overlay has and none of which a unit-level registry fixture builds — so
/// the view holds a [`subagent::ViewDispatcher`] and this is the wiring that
/// turns one line of text into one report.
///
/// Admission is the dispatcher's, which is the **registry's**
/// (`AgentRegistryLimits`) and never the model's per-turn `SpawnBudget`
/// (**A6**); the syntax is `agents_view::parse_dispatch`'s. Both refusals come
/// back in their own author's words rather than a second wording here.
///
/// **Card T4-S7 is written test-first; this is the seam the tests pin and the
/// implementer fills.**
/// What the dispatch line answers on a carrier that never built a dispatcher
/// (card **T4-S7**): the line is still read, because the key opened it, and
/// the answer says why nothing started rather than discarding the task.
#[cfg(unix)]
pub(crate) const DISPATCH_WITHOUT_DISPATCHER: &str =
    "this session cannot start background agents from the view";

#[cfg(unix)]
pub(crate) fn agent_view_dispatch(
    dispatcher: &subagent::ViewDispatcher,
    text: &str,
) -> AgentViewReport {
    // The syntax first, because refusing it costs no gateway call: a
    // malformed line never reaches the dispatcher at all.
    let parsed = match kitsu_tui::agents_view::parse_dispatch(text) {
        Ok(parsed) => parsed,
        Err(error) => {
            return AgentViewReport {
                line: error.message().to_string(),
                changed: false,
            }
        }
    };
    match dispatcher(parsed.task, parsed.profile) {
        Ok(id) => AgentViewReport {
            line: format!("started background agent #{id}"),
            changed: true,
        },
        // The registry's or the catalog's own sentence, carried and not
        // reworded: it is what names the ceiling that refused.
        Err(error) => AgentViewReport {
            line: error,
            changed: false,
        },
    }
}

/// `p`: take custody of the selected agent's result. The **deliberate**
/// exception to T4-S3's rule that peek is the only call site that does.
#[cfg(unix)]
pub(crate) fn agent_view_acknowledge(
    registry: &subagent::AgentRegistry,
    id: u64,
) -> AgentViewReport {
    match registry.acknowledge_result(id) {
        subagent::AgentHandoffAckOutcome::Acknowledged => AgentViewReport {
            line: format!("acknowledged background agent #{id}'s result"),
            changed: true,
        },
        subagent::AgentHandoffAckOutcome::AlreadyAcknowledged => AgentViewReport {
            line: format!("background agent #{id}'s result was already collected"),
            changed: false,
        },
        subagent::AgentHandoffAckOutcome::NotReady => AgentViewReport {
            line: format!("background agent #{id} has no result to acknowledge yet"),
            changed: false,
        },
        subagent::AgentHandoffAckOutcome::NotFound => AgentViewReport {
            line: format!("no background job #{id}"),
            changed: false,
        },
    }
}

/// Any action key pressed with **no selection at all** — an empty view, or one
/// whose rows the filter took away.
///
/// Its own report rather than a silent nothing, for the reason T4-S4a gave
/// `press_stop` a `NoSelection` answer: a key that looks broken is worse than
/// a key that says there is nothing to act on.
#[cfg(unix)]
pub(crate) fn agent_view_no_selection(key: char) -> AgentViewReport {
    AgentViewReport {
        line: format!("no agent is selected, so `{key}` has nothing to act on"),
        changed: false,
    }
}

/// What `Enter`/`\u{2192}` say when there is **no selection at all** (card
/// **T4-S6a**, requirement A3's *attach*).
///
/// Its own wording rather than [`agent_view_no_selection`]'s, for two reasons:
/// that helper interpolates the keystroke as a character, which neither of
/// these two keys is, and a row carrying a raw `\r` would corrupt the frame it
/// is painted on.
#[cfg(unix)]
pub(crate) const ATTACH_WITHOUT_SELECTION: &str =
    "nothing is selected, so there is no agent to attach to";

/// The overlay's state between keys: everything a key can change that is not
/// the terminal itself.
///
/// It exists so that [`handle_overlay_input`] can be the **one** `match` on
/// [`OverlayInput`] in the binary (finding **F69**). The byte loop owns the
/// terminal -- reading, the 50 ms grace, painting, the alternate screen -- and
/// this is what it hands the dispatch.
#[cfg(unix)]
pub(crate) struct OverlayState {
    /// Whether the help frame is on screen.
    pub(crate) helping: bool,
    /// The peek panel for one agent, while `Space` is holding it up.
    pub(crate) peeking: Option<Vec<String>>,
    /// Which agent the keys act on.
    pub(crate) selection: kitsu_tui::agents_view::AgentSelection,
    /// What the last action key did, painted as the one report row.
    pub(crate) report: Option<String>,
    /// The prompt one of the action keys has open.
    pub(crate) prompt: Option<OverlayPrompt>,
    /// `s`'s text sub-mode: a line edited by `kitsu-editor`, which reads the
    /// raw bytes itself while it is open. It is its own state rather than an
    /// [`OverlayPrompt`], because the prompts are fed decoded keys and this is
    /// fed bytes.
    pub(crate) steering: Option<SteerLine>,
    /// `n`'s rename line while the name is being typed, read as raw bytes for
    /// the reasons `s`'s is and one of its own: a name may be non-ASCII and
    /// the registry accepts it, while `OverlayKey::Char` is ASCII-only by
    /// construction, so the decoded prompt this replaces could not express a
    /// legal name at all (**F70**).
    pub(crate) renaming: Option<SteerLine>,
    /// The dispatch line while a task is being typed (card **T4-S7**,
    /// requirement **A6**): the view's **fourth** edited line, on the same
    /// byte bypass as the three above it and for the same three reasons — a
    /// task may be non-ASCII, a paste is content and must not submit, and a
    /// character really does arrive split across two reads.
    ///
    /// It has no id, like `/`'s and unlike `s`'s and `n`'s: a dispatch is
    /// about an agent that does not exist yet, so there is nothing selected
    /// for it to act on.
    ///
    /// It inherits **F72** with the other three: these lines are fed raw bytes
    /// and the overlay's decoder is not in the path, so the burst fallback
    /// does not protect them and on a terminal that does not bracket pastes a
    /// pasted task submits at its first newline. That gap is not this card's.
    pub(crate) dispatching: Option<OverlayLine>,
    /// How a submitted dispatch line starts an agent: the session's binder,
    /// subagent registry, config template and profile catalog, closed over
    /// (card **T4-S7**). `None` where the carrier never built one.
    pub(crate) dispatcher: Option<subagent::ViewDispatcher>,
    /// `/`'s filter line while it is being typed, read as raw bytes for the
    /// same reasons `s`'s is — a term may be non-ASCII, a paste is content,
    /// and a character may arrive split across two reads (card T4-S5).
    pub(crate) filtering: Option<OverlayLine>,
    /// The agent the view was told to attach to, for the loop to pick up as
    /// it leaves (card **T4-S6a**, requirement A3's `Enter`/`\u{2192}`).
    ///
    /// Attach is state rather than a step for the reason the filter is: the
    /// key's whole effect is recorded here, and the terminal half of the loop
    /// — which leaves the alternate screen, prints the `\u{26E9}` milestone and
    /// routes the composer — reads it afterwards. A step carrying the id would
    /// put the id in two places.
    pub(crate) attaching: Option<u64>,
    /// The filter in force, which is what [`overlay_visible_rows`] applies.
    ///
    /// It lives here and nowhere else, so it survives every repaint and every
    /// regroup and dies with the view: a reopen builds a fresh `OverlayState`,
    /// and a filter outliving the screen it was set on would hide agents for a
    /// reason nothing on screen explains. The default is the empty filter,
    /// which matches every row.
    pub(crate) filter: kitsu_tui::agents_view::AgentFilter,
}

#[cfg(unix)]
impl OverlayState {
    pub(crate) fn new() -> Self {
        Self {
            helping: false,
            peeking: None,
            selection: kitsu_tui::agents_view::AgentSelection::new(),
            report: None,
            prompt: None,
            steering: None,
            renaming: None,
            filtering: None,
            dispatching: None,
            dispatcher: None,
            attaching: None,
            filter: kitsu_tui::agents_view::AgentFilter::default(),
        }
    }
}

/// What the byte loop must do about the key it just handed to
/// [`handle_overlay_input`], in the loop's own terms.
#[cfg(unix)]
#[derive(Debug, Eq, PartialEq)]
pub(crate) enum OverlayStep {
    /// Carry on with the rest of the decoded chunk.
    Continue,
    /// `q`, or an `Esc` with nothing left to step out of: finish the chunk and
    /// leave the view.
    Closing,
    /// Stop reading this chunk: one of the keys that open a line (`s`, `n`
    /// or `/`) did so on this very keystroke, and the bytes after it are the
    /// line's to read, not the decoder's.
    StopChunk,
}

/// Every key and every paste the overlay acts on, in one place.
///
/// This is the whole of the view's input dispatch and the **only** `match` on
/// [`OverlayInput`] in this binary: `run_agent_view_overlay` keeps the parts
/// that need a terminal and calls this for the parts that do not, and the
/// registry-level tests call this same function. A test model that mirrored
/// these arms could stop reflecting them without anything failing, which is
/// finding **F69**; there is no model to diverge from now.
/// Whether a lone `ESC` is held and the 50 ms grace is therefore the next
/// thing to spend (**F65**, card T4-S4a).
///
/// An open line holds its own `ESC`, inside the editor, and takes precedence:
/// while a line is up the decoder is not in the path at all.
#[cfg(unix)]
pub(crate) fn overlay_escape_held(state: &OverlayState, decoder: &OverlayDecoder) -> bool {
    if state.steering.is_some() {
        return state.steering.as_ref().is_some_and(SteerLine::escape_held);
    }
    if state.renaming.is_some() {
        return state.renaming.as_ref().is_some_and(SteerLine::escape_held);
    }
    if state.filtering.is_some() {
        return state
            .filtering
            .as_ref()
            .is_some_and(OverlayLine::escape_held);
    }
    // The dispatch line, identically: while it is up the decoder is not in
    // the path and the line holds its own `ESC`.
    if state.dispatching.is_some() {
        return state
            .dispatching
            .as_ref()
            .is_some_and(OverlayLine::escape_held);
    }
    decoder.escape_pending()
}

/// The grace expired with nothing waiting: the held `ESC` was the `Esc` key.
///
/// `Esc` means *step back out of the frame you are in*, and the frames nest:
/// an open line, then a prompt, then the help, then a peek, then the view
/// itself. Returns whether the view is closing.
///
/// Extracted from the loop for [`overlay_feed_chunk`]'s reason (**F69**): the
/// resolution is state, the *waiting* is the terminal's, and a test that
/// re-spelled the resolution would be a mirror.
#[cfg(unix)]
pub(crate) fn overlay_resolve_held_escape(
    state: &mut OverlayState,
    decoder: &mut OverlayDecoder,
) -> bool {
    // The line resolves its own held `ESC`, on the same grace and for the same
    // reason: inside it, `Esc` cancels the line and sends nothing.
    if state.steering.is_some() {
        if let Some(line) = state.steering.as_mut() {
            if line.quiet_tick() == SteerLineStep::Cancelled {
                state.steering = None;
                state.report = None;
            }
        }
        return false;
    }
    // `n`'s line, identically: `Esc` steps out of the rename frame and
    // renames nothing, which is the one reading under which a half-typed
    // name cannot be applied by accident.
    if state.renaming.is_some() {
        if let Some(line) = state.renaming.as_mut() {
            if line.quiet_tick() == SteerLineStep::Cancelled {
                state.renaming = None;
                state.report = None;
            }
        }
        return false;
    }
    // `/`'s line resolves its own held `ESC` the same way, and cancelling it
    // is **not** clearing the filter already in force: `Esc` steps out of one
    // frame, and a cancel that also cleared would do two things at once.
    if state.filtering.is_some() {
        if let Some(line) = state.filtering.as_mut() {
            if line.quiet_tick() == SteerLineStep::Cancelled {
                state.filtering = None;
                state.report = None;
            }
        }
        return false;
    }
    // The dispatch line resolves its own held `ESC` the same way, and
    // cancelling it starts nothing: `Esc` steps out of one frame.
    if state.dispatching.is_some() {
        if let Some(line) = state.dispatching.as_mut() {
            if line.quiet_tick() == SteerLineStep::Cancelled {
                state.dispatching = None;
                state.report = None;
            }
        }
        return false;
    }
    decoder.resolve_escape();
    if state.prompt.is_some() {
        state.prompt = None;
        state.report = None;
    } else if state.helping {
        state.helping = false;
    } else if state.peeking.is_some() {
        state.peeking = None;
    } else {
        return true;
    }
    false
}

/// Everything one `read` of the descriptor does to the overlay's state, with
/// nothing terminal-owned in it: an open line's **byte** bypass first, then
/// the decoder's keys and pastes through the one dispatch.
///
/// Extracted for the same reason [`handle_overlay_input`] was (finding
/// **F69**): a chunk of bytes now has **one** path, which the loop and the
/// registry-level tests both take. The byte bypass is not a detail the tests
/// may re-spell — it is what makes a non-ASCII character and a bracketed
/// paste work in a line at all — so a test-local copy of it would be a mirror
/// that can stop reflecting.
///
/// Returns whether the view is closing.
#[cfg(unix)]
pub(crate) fn overlay_feed_chunk(
    registry: &subagent::AgentRegistry,
    state: &mut OverlayState,
    decoder: &mut OverlayDecoder,
    view_rows: &[kitsu_tui::agents_view::AgentRow],
    cols: usize,
    bytes: &[u8],
) -> bool {
    // An open steer line reads the bytes itself, bypassing the decoder
    // entirely: the editor needs the real `ESC[200~`/`ESC[201~` to apply
    // its "a paste never submits" rule, and it is also what makes a
    // multi-byte character split across two reads work here.
    if let Some(line) = state.steering.as_mut() {
        match line.feed(bytes) {
            SteerLineStep::Editing => {}
            SteerLineStep::Submit(correction) => {
                state.report = Some(agent_view_steer(registry, line.id(), &correction).line);
                state.steering = None;
            }
            SteerLineStep::Cancelled => {
                state.steering = None;
                state.report = None;
            }
        }
        return false;
    }
    // `n`'s rename line, on the same byte bypass and for the same three
    // reasons the filter line below gives — and F70 is the finding that a
    // *legal* non-ASCII name was unreachable from the keyboard until it was.
    if let Some(line) = state.renaming.as_mut() {
        match line.feed(bytes) {
            SteerLineStep::Editing => {}
            SteerLineStep::Submit(name) => {
                let report = agent_view_rename(registry, line.id(), &name);
                if report.changed {
                    state.renaming = None;
                } else {
                    // Refused: the line stays **open** carrying the name, as
                    // `/`'s refused term does, so the message and the text it
                    // is about are on screen together and one wrong character
                    // is corrected rather than a 48-character name retyped.
                    // The refusal is painted on *this* frame — the rename
                    // frame is innermost, so a report row under the row list
                    // would be a message the user cannot see (**F71**).
                    line.restore(&name);
                }
                state.report = Some(report.line);
            }
            SteerLineStep::Cancelled => {
                state.renaming = None;
                state.report = None;
            }
        }
        return false;
    }
    // `/`'s filter line, on the same byte bypass and for the same three
    // reasons: a non-ASCII term is legal and `OverlayKey::Char` is ASCII-only
    // by construction, a bracketed paste must not submit, and a multi-byte
    // character really does arrive split across two reads.
    if let Some(line) = state.filtering.as_mut() {
        match line.feed(bytes) {
            SteerLineStep::Editing => {}
            SteerLineStep::Submit(text) => match kitsu_tui::agents_view::parse_filter(&text) {
                // An empty line parses to the empty filter, which is how the
                // filter is cleared without closing and reopening the view.
                Ok(filter) => {
                    state.filter = filter;
                    state.filtering = None;
                    state.report = None;
                }
                // Refused: the rows are left exactly as they were and the
                // line stays **open**, carrying the text back so the one
                // wrong character can be corrected. Filtering to nothing
                // would be the worst reading of a bad term — an empty view
                // says nothing about why, and in this view an empty view can
                // mean failing to show the agent waiting on an answer.
                Err(error) => {
                    line.restore(&text);
                    state.report = Some(error.message().to_string());
                }
            },
            SteerLineStep::Cancelled => {
                state.filtering = None;
                state.report = None;
            }
        }
        return false;
    }
    // The dispatch line (card **T4-S7**, requirement **A6**), on the same
    // byte bypass as the three above it and for the same three reasons: a
    // task may be non-ASCII, a bracketed paste is content and must not
    // submit, and a multi-byte character really does arrive split across two
    // reads.
    //
    // The dispatcher is cloned out of the state before the line is borrowed:
    // it is an `Arc`, and the alternative is holding two borrows of the same
    // state across the submit.
    let dispatcher = state.dispatcher.clone();
    if let Some(line) = state.dispatching.as_mut() {
        match line.feed(bytes) {
            SteerLineStep::Editing => {}
            SteerLineStep::Submit(text) => match dispatcher.as_ref() {
                Some(dispatcher) => {
                    let report = agent_view_dispatch(dispatcher, &text);
                    if report.changed {
                        state.dispatching = None;
                    } else {
                        // Refused: the line stays **open** carrying the task,
                        // as `/`'s refused term and `n`'s refused name do, so
                        // the refusal and the text it is about are on screen
                        // together and the dispatch is retried with one
                        // keystroke rather than retyped. The refusal is
                        // painted on *this* frame, the innermost one
                        // (**F71**).
                        line.restore(&text);
                    }
                    state.report = Some(report.line);
                }
                // A carrier that never built a dispatcher cannot start an
                // agent, and says so rather than swallowing the line: a key
                // that acts silently is indistinguishable from one that
                // failed (requirement T2-S5b).
                None => {
                    line.restore(&text);
                    state.report = Some(DISPATCH_WITHOUT_DISPATCHER.to_string());
                }
            },
            SteerLineStep::Cancelled => {
                state.dispatching = None;
                state.report = None;
            }
        }
        return false;
    }
    let mut closing = false;
    for input in decoder.feed(bytes) {
        match handle_overlay_input(registry, state, view_rows, cols, input) {
            OverlayStep::Continue => {}
            OverlayStep::Closing => closing = true,
            // A line opened (`s`, `n` or `/`): the rest of this chunk is
            // not text anybody typed, and the bytes after it are read by
            // the line itself from here on.
            OverlayStep::StopChunk => break,
        }
    }
    closing
}

/// The rows the overlay would paint, for the state it is in: **the frame
/// chain**, which is what decides whether a message is on screen at all.
///
/// Extracted from the loop for [`overlay_feed_chunk`]'s reason (**F69**) and
/// for **F71**'s: the chain nests, innermost first, and a report row painted
/// under the row list is *not* on screen while a line is open. A test that
/// reads `state.report` has asserted the message exists; only this function
/// says whether a user can see it, so the loop and the tests take the same
/// one.
#[cfg(unix)]
pub(crate) fn overlay_frame_rows(
    state: &OverlayState,
    view_rows: &[kitsu_tui::agents_view::AgentRow],
    cols: usize,
) -> Vec<String> {
    if let Some(line) = state.steering.as_ref() {
        // The innermost frame of all: while a correction is being typed
        // it owns the screen, caret included, because the overlay hides
        // the terminal's own cursor.
        return kitsu_tui::agents_view::steer_prompt_rows(
            line.id(),
            line.draft(),
            line.cursor(),
            cols,
        );
    }
    if let Some(line) = state.renaming.as_ref() {
        // `n`'s line is an innermost frame too, and it takes the report with
        // it: a refused name leaves the line open, and the registry's reason
        // belongs on the frame the name was typed on and not under a row list
        // this frame covers (**F71**).
        return kitsu_tui::agents_view::rename_prompt_rows(
            line.id(),
            line.draft(),
            line.cursor(),
            cols,
            state.report.as_deref(),
        );
    }
    if let Some(line) = state.filtering.as_ref() {
        // The filter line is an innermost frame too, for the same reason:
        // it owns the caret the view otherwise hides.
        return kitsu_tui::agents_view::filter_prompt_rows(
            line.draft(),
            line.cursor(),
            cols,
            state.report.as_deref(),
        );
    }
    if let Some(line) = state.dispatching.as_ref() {
        // The dispatch line is an innermost frame too, and it takes the
        // report with it: a refused dispatch leaves the line open, and the
        // registry's reason belongs on the frame the task was typed on rather
        // than under a row list this frame covers (**F71**).
        return kitsu_tui::agents_view::dispatch_prompt_rows(
            line.draft(),
            line.cursor(),
            cols,
            state.report.as_deref(),
        );
    }
    if state.helping {
        return kitsu_tui::agents_view::overlay_help_rows(cols);
    }
    if let Some(panel) = state.peeking.as_ref() {
        return panel.clone();
    }
    // An open prompt owns the report row; otherwise the last action's
    // report has it.
    let line = match state.prompt.as_ref() {
        Some(open) => Some(open.report_line()),
        None => state.report.clone(),
    };
    kitsu_tui::agents_view::overlay_rows_with_report(
        view_rows.to_vec(),
        cols,
        state.selection.selected(),
        line.as_deref(),
    )
}

#[cfg(unix)]
pub(crate) fn handle_overlay_input(
    registry: &subagent::AgentRegistry,
    state: &mut OverlayState,
    view_rows: &[kitsu_tui::agents_view::AgentRow],
    cols: usize,
    input: OverlayInput,
) -> OverlayStep {
    // A prompt is taken out of the state for the duration of the key
    // and put back if it is still open afterwards: that is what lets
    // each arm close it without holding a borrow of it.
    //
    // Pasted text is routed before any of that, because a paste is
    // **content and never a command** (finding **F68**): it fills a
    // text prompt's draft, newline included and without submitting,
    // and it means nothing at all over the row list or inside `r`'s
    // decision read.
    let key = match input {
        OverlayInput::Key(key) => key,
        OverlayInput::Paste(text) => {
            match state.prompt.take() {
                // The read stays open rather than resolving to a
                // pasted letter: `r` then a pasted `y` would approve
                // a filesystem write nobody approved. The open read
                // owns the report row, so it keeps saying what it
                // wants and nothing is lost by staying quiet here.
                Some(OverlayPrompt::Decision(id)) => {
                    state.prompt = Some(OverlayPrompt::Decision(id));
                }
                // Over the row list the paste is dropped — and said
                // so. A key that acts silently is indistinguishable
                // from one that failed (requirement T2-S5b), and the
                // unbracketed-burst rule has a false positive (a fast
                // typist's two keys in one read), so the one case a
                // user cannot otherwise explain is exactly this one.
                // The count is reported and the text is not: it is
                // the user's clipboard, not the view's to paint. A paste
                // *into* a line never reaches here at all: an open line is
                // fed the raw bytes and the decoder is not in that path.
                None => {
                    state.report = Some(format!(
                        "ignored a paste of {} characters: pasted text is not keys here, \
                             so open a line first (n names, s steers)",
                        text.chars().count()
                    ));
                }
            }
            return OverlayStep::Continue;
        }
    };
    match state.prompt.take() {
        // `r`'s decision read: the next key is the answer, and
        // `parse_permission_answer` is what reads it.
        Some(OverlayPrompt::Decision(id)) => match key {
            OverlayKey::StepBack => state.report = None,
            OverlayKey::Char(typed) => {
                state.report = Some(agent_view_reply(registry, id, &typed.to_string()).line);
            }
            // An arrow or an Enter inside the read is neither an
            // answer nor a cancel, so the read stays open rather than
            // resolving to a guess.
            OverlayKey::Up
            | OverlayKey::Down
            | OverlayKey::Right
            | OverlayKey::Enter
            | OverlayKey::Backspace
            | OverlayKey::Tab
            | OverlayKey::Interrupt => state.prompt = Some(OverlayPrompt::Decision(id)),
        },
        None => match key {
            // Esc, resolved as a real key press rather than as the
            // start of a sequence: out of the help if it is showing,
            // out of a peek panel if one is up, out of the view
            // otherwise. `q` is the conventional synonym for the
            // latter.
            OverlayKey::StepBack => {
                if state.helping {
                    state.helping = false;
                } else if state.peeking.is_some() {
                    state.peeking = None;
                } else {
                    return OverlayStep::Closing;
                }
            }
            // The arrows move the selection and nothing else: they are
            // not an exit, and no byte of their escape sequence
            // reaches the composer underneath. They clamp rather than
            // wrap, and they are inert while the help or a peek panel
            // is up, where there is no row list on screen to move
            // over.
            OverlayKey::Up => {
                if !state.helping && state.peeking.is_none() {
                    state.selection.move_up(view_rows);
                }
            }
            OverlayKey::Down => {
                if !state.helping && state.peeking.is_none() {
                    state.selection.move_down(view_rows);
                }
            }
            // Card **T4-S6a**, requirement A3: `Enter` and its synonym
            // `\u{2192}` **attach** to the selected agent and leave the view.
            // Leaving is part of the key and not an extra step: A5 streams the
            // attached agent into the **main viewport**, which a full-screen
            // overlay would cover. Inert while the help or a peek panel is up,
            // like every other key that acts on the selection, because the
            // selected row is not on screen there.
            //
            // Requirement **A7**: attaching a terminal agent takes custody of
            // its result, because attach is how the user reads it. The
            // acknowledgement goes through `agent_view_peek` -- the one
            // acknowledging path T4-S3 left -- rather than a second
            // `acknowledge_result` call site, which is the shape F45 was
            // closed by retiring. Its panel is discarded here: the view is
            // closing, and the stream the user attached to is the viewport's.
            OverlayKey::Enter | OverlayKey::Right => {
                if !state.helping && state.peeking.is_none() {
                    state.selection.disarm();
                    match state.selection.selected() {
                        None => state.report = Some(ATTACH_WITHOUT_SELECTION.to_string()),
                        Some(id) => {
                            let _ = agent_view_peek(registry, id);
                            state.attaching = Some(id);
                            return OverlayStep::Closing;
                        }
                    }
                }
            }
            // Backspace belongs to the prompts; over the row list there is
            // nothing for it to do, and it must not reach the composer
            // underneath either.
            // Tab and Ctrl-C have no meaning over the row list (review F4
            // only gave them meaning in the model popup).
            OverlayKey::Backspace | OverlayKey::Tab | OverlayKey::Interrupt => {}
            OverlayKey::Char('?') => state.helping = true,
            OverlayKey::Char('q') => return OverlayStep::Closing,
            // Space: peek the selected agent. The byte is the view's
            // and goes no further -- nothing is forwarded to the
            // composer underneath, and it is not an exit either (test
            // T4-S3a). With no rows there is nothing to peek and the
            // frame is unchanged. Peeking is the only *read* here that
            // acknowledges a handoff; painting rows and moving the
            // selection do not, and neither do `r`, `x` and `n`.
            OverlayKey::Char(' ') => {
                if !state.helping && state.peeking.is_none() {
                    state.selection.disarm();
                    if let Some(id) = state.selection.selected() {
                        state.peeking = agent_view_peek_frame(registry, id, cols);
                    }
                }
            }
            // The action keys (card T4-S4b, requirement A3). Every one
            // of them acts on the selected agent and reports what it
            // did, including when there is nothing selected: a key
            // that looks broken is worse than one that says so. They
            // are inert while the help or a peek panel is up, like the
            // arrows, because the selected row is not on screen there.
            //
            // Each of them is also a keystroke that is not `x`, so
            // each disarms a half-pressed stop (T4-S4a).
            OverlayKey::Char(pressed @ ('r' | 's' | 'n' | 'p')) => {
                if !state.helping && state.peeking.is_none() {
                    state.selection.disarm();
                    match state.selection.selected() {
                        None => state.report = Some(agent_view_no_selection(pressed).line),
                        Some(id) => match pressed {
                            'r' => state.prompt = Some(OverlayPrompt::Decision(id)),
                            's' => {
                                // Card T4-S4b-ii: the real line over
                                // `kitsu-editor`, in place of T4-S4b's
                                // naive one. `n` is on the same line as
                                // of card F70.
                                state.steering = Some(SteerLine::new(id));
                                // The rest of this chunk is not a
                                // correction anybody typed -- the
                                // line opened on this very keystroke
                                // -- and the bytes after `s` are read
                                // by the line itself from here on.
                                return OverlayStep::StopChunk;
                            }
                            // Card F70: the real line over `kitsu-editor`,
                            // exactly as `s` above, in place of T4-S4b's
                            // decoded prompt. A name may be non-ASCII and
                            // the registry accepts it; `OverlayKey::Char`
                            // cannot carry one byte of it.
                            'n' => {
                                state.renaming = Some(SteerLine::new(id));
                                // The previous key's report is not this
                                // frame's: the rename frame paints a refusal
                                // of what is typed *into* it, and a stale
                                // line there would read as one (**F71**).
                                state.report = None;
                                // The bytes after `n` in this very chunk are
                                // the line's to read, as they are after `s`.
                                return OverlayStep::StopChunk;
                            }
                            _ => {
                                state.report = Some(agent_view_acknowledge(registry, id).line);
                            }
                        },
                    }
                }
            }
            // `/` opens the filter line (card T4-S5, requirement A3's last
            // unbound key). Unlike the action keys beside it, it needs no
            // selection: a filter is about the rows on screen, not about one
            // agent, and it is how a user *finds* the agent to select. It is
            // a keystroke that is not `x`, so it disarms a half-pressed stop.
            OverlayKey::Char('/') => {
                if !state.helping && state.peeking.is_none() {
                    state.selection.disarm();
                    state.filtering = Some(OverlayLine::new());
                    // The previous key's report is not this frame's: the
                    // filter frame paints a refusal of what is typed *into*
                    // it, and a stale line there would read as one.
                    state.report = None;
                    // The bytes after `/` in this very chunk are the line's to
                    // read, exactly as they are after `s`.
                    return OverlayStep::StopChunk;
                }
            }
            // `x` is two presses: the first arms, the second stops a
            // live agent and prunes an already-terminal one. The
            // arming is T4-S4a's and is not changed here; what the
            // confirmation *does* was never pinned, and this is it.
            OverlayKey::Char('x') => {
                if !state.helping && state.peeking.is_none() {
                    let armed_on = state.selection.selected();
                    state.report = match state.selection.press_stop() {
                        kitsu_tui::agents_view::StopPress::Armed => armed_on
                            .map(|id| format!("press x again to stop background agent #{id}")),
                        kitsu_tui::agents_view::StopPress::Confirmed(id) => {
                            Some(overlay_block_on(agent_view_stop_or_prune(registry, id)).line)
                        }
                        kitsu_tui::agents_view::StopPress::NoSelection => {
                            Some(agent_view_no_selection('x').line)
                        }
                    };
                }
            }
            // The dispatch key opens the view's fourth edited line (card
            // **T4-S7**, requirement **A6**). Like `/` it needs no selection:
            // a dispatch is about an agent that does not exist yet, so there
            // is nothing selected for it to act on. It is a keystroke that is
            // not `x`, so it disarms a half-pressed stop.
            OverlayKey::Char(pressed) if pressed == kitsu_tui::agents_view::DISPATCH_KEY => {
                if !state.helping && state.peeking.is_none() {
                    state.selection.disarm();
                    state.dispatching = Some(OverlayLine::new());
                    // The previous key's report is not this frame's: the
                    // dispatch frame paints a refusal of what is typed *into*
                    // it, and a stale line there would read as one (**F71**).
                    state.report = None;
                    // The bytes after the key in this very chunk are the
                    // line's to read, exactly as they are after `s`.
                    return OverlayStep::StopChunk;
                }
            }
            // Every other printable byte is the view's and goes no
            // further: the overlay owns the keyboard while it is up.
            OverlayKey::Char(_) => {}
        },
    }
    OverlayStep::Continue
}

/// What one run of the agent view leaves behind for the composer underneath
/// (card **T4-S6a**, finding **F75**).
///
/// Two things, because the overlay has two callers with two different
/// questions: `/jobs view` only asks **whether** the view was shown, so it can
/// print the text deck when it was not; the idle composer asks **what the view
/// attached to**, because requirement A3's `Enter`/`\u{2192}` is an action
/// whose entire effect lands outside the view.
///
/// It is a return value rather than something the caller reads off
/// [`OverlayState`] because the state does not survive the run: it is built
/// inside [`run_agent_view_overlay`] and dropped when the view closes — and
/// attach *closes the view*, so by the time the composer could look, there is
/// nothing left to look at. **F75** is what happens without this: the attach
/// arm wrote `OverlayState::attaching` and no production code ever read it.
#[cfg(unix)]
#[derive(Debug, Default, Eq, PartialEq)]
pub(crate) struct AgentViewExit {
    /// Whether the overlay was actually shown. `false` when the mode or the
    /// terminal declined it, which is the answer `/jobs view` falls back on.
    pub(crate) shown: bool,
    /// The agent `Enter`/`\u{2192}` attached to, if either was pressed.
    pub(crate) attached: Option<u64>,
}

#[cfg(unix)]
impl AgentViewExit {
    /// The view was not shown, so it attached to nothing.
    pub(crate) fn declined() -> Self {
        Self::default()
    }

    /// The view ran and has closed: whatever the attach arm recorded is now
    /// the composer's to act on.
    pub(crate) fn leaving(state: &OverlayState) -> Self {
        Self {
            shown: true,
            attached: state.attaching,
        }
    }
}

/// The foreground recap buffer for the attachment in force, if any (design
/// \u{a7}4.5), or `None` when nothing is attached.
pub(crate) type ForegroundRecap = Option<std::sync::Arc<kitsu_render::surface::BufferedSurface>>;

/// The one recap buffer a session can have.
///
/// Process-global because the thing it displaces is: the surface slot
/// ([`kitsu_render::surface::set_surface`]) is the one seam every routed
/// writer in this binary reaches through, and an attachment is a property of
/// the session, not of any one call stack. One slot, because the session
/// binding it shadows holds at most one agent.
pub(crate) static FOREGROUND_RECAP: std::sync::Mutex<ForegroundRecap> = std::sync::Mutex::new(None);

/// The recap buffer, with a poisoned lock recovered: a panic elsewhere must
/// not make the next attachment panic too.
pub(crate) fn foreground_recap_slot() -> std::sync::MutexGuard<'static, ForegroundRecap> {
    FOREGROUND_RECAP
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Start withholding the foreground orchestrator's output, by installing a
/// buffer over whatever surface is installed now (design \u{a7}4.5).
///
/// Called at the one moment the binding begins, so that output produced
/// *before* the attachment is already painted and is never in a recap.
#[cfg(unix)]
pub(crate) fn begin_foreground_buffering() {
    let mut slot = foreground_recap_slot();
    let buffer = std::sync::Arc::new(kitsu_render::surface::BufferedSurface::new(
        kitsu_render::surface::active_surface(),
    ));
    kitsu_render::surface::set_surface(std::sync::Arc::clone(&buffer) as _);
    *slot = Some(buffer);
}

/// Put the displaced surface back and replay what the attachment to `id`
/// withheld, under its own `\u{26E9}` header (design \u{a7}4.5).
///
/// **Every way an attachment ends calls this**, which is four and not three:
/// the two detach keystrokes both land in [`apply_inline_detach`], `/detach`
/// lands in [`apply_detach_command`], and
/// [`apply_agent_view_exit`] re-binding to a *different* agent ends the first
/// attachment with no key pressed at all. Enumerating the routes out of the
/// state is not enumerating the transitions out of it; another transition
/// that moves the binding needs this call too.
///
/// **The milestone is the recap's header, not an announcement of the
/// detach**, so an empty recap paints nothing: the two keystrokes are silent
/// by design and the command gives its own answer, and a header ending in a
/// colon with nothing under it reads as a bug.
///
/// The recap is replayed through the *displaced* surface after it is
/// reinstalled, so the overflow note and the lines go to the terminal the
/// user is returning to rather than back into a buffer.
pub(crate) fn replay_foreground_recap(id: u64) {
    let Some(buffer) = foreground_recap_slot().take() else {
        return;
    };
    let inner = buffer.inner();
    kitsu_render::surface::set_surface(std::sync::Arc::clone(&inner));

    let recap = buffer.take_recap();
    if recap.is_empty() {
        return;
    }
    inner.transcript_line(
        kitsu_render::surface::Fd::Stderr,
        &kitsu_render::milestone_line(&format!("detached from agent {id} \u{2014} recap:")),
    );
    for (fd, text) in recap {
        inner.transcript_line(fd, &text);
    }
}

/// Carry one agent view's attachment into the composer's session binding
/// (card **T4-S6a**, requirement A3).
///
/// The session binding is the attachment's **only** home: it has to outlive
/// both the overlay (per-invocation) and one inline read (one submitted line),
/// and it is the single source the prompt label and the `\u{2190}`/`Ctrl+X d`
/// routes are both derived from.
///
/// A view that was closed with `q` or `Esc` attached to nothing and leaves the
/// existing attachment alone: leaving the view is not detaching, and `\u{2190}`
/// and `/detach` are the keys that are.
#[cfg(unix)]
pub(crate) fn apply_agent_view_exit(exit: &AgentViewExit, attached: &mut Option<u64>) {
    if let Some(id) = exit.attached {
        // Card **T4-S6b**: the `\u{26E9}` milestone (design \u{a7}4.5), painted
        // here because this is the one seam both attach routes go through,
        // and only on a **change** of binding -- re-attaching to the agent
        // already attached changes nothing, and a second identical milestone
        // would claim something happened.
        //
        // It names the **route**, not just the agent (**F82**): attaching
        // adds no key, it changes what `Enter` means, so the moment the
        // binding begins is the only place that change can be enumerated.
        if *attached != Some(id) {
            // The fourth transition out of an attachment: re-binding ends the
            // previous one, and what it buffered is replayed under *its* own
            // agent's header before the new attachment is announced, or a
            // reader would attribute one agent's turn to another.
            if let Some(previous) = attached.take() {
                replay_foreground_recap(previous);
            }
            *attached = Some(id);
            kitsu_render::surface::active_surface().transcript_line(
                kitsu_render::surface::Fd::Stderr,
                &kitsu_render::milestone_line(&format!(
                    "attached to agent {id}; your next line steers it, or \
                     answers it while it is blocked"
                )),
            );
            begin_foreground_buffering();
        }
    }
}

/// Perform one detach for the composer loop, and hand back the draft the
/// next read must replay (finding **F76**).
///
/// Clearing the session binding is the whole of the detach -- the next
/// iteration re-derives the prompt label and the `\u{2190}` route off it --
/// and the draft is carried because `Ctrl+X d` has no empty-composer
/// precondition: the chord exists precisely for the case where `\u{2190}` is
/// cursor movement, so it is the route that can arrive with text typed.
///
/// A function rather than two lines in the arm for `apply_agent_view_exit`'s
/// reason: the loop around it needs a real tty, so this is the only seam a
/// test can drive, and there is exactly one of it (**F69**).
#[cfg(unix)]
pub(crate) fn apply_inline_detach(attached: &mut Option<u64>, draft: String) -> String {
    if let Some(id) = attached.take() {
        replay_foreground_recap(id);
    }
    draft
}

/// Perform one `Ctrl+X 1..9` for the composer loop (card **T4-S6-ordinal**),
/// and hand back the draft the next read must replay (**F76**).
///
/// The digit is a **1-based ordinal over the rows the view would display**,
/// never an id: ids pass nine after nine spawns, and once `#1` is pruned and
/// `#11` exists an id read is *wrong* rather than dead. The rows come from
/// [`overlay_visible_rows`] -- the one place A3's filter is applied -- so the
/// position the user counted off the screen is the position resolved here,
/// and `kitsu_tui::agents_view::nth_displayed` owns both the 1-based
/// convention and the bounds.
///
/// A position no row holds -- `0`, an ordinal past the last row, or any digit
/// at all while the view is empty -- is **inert and silent**, exactly as
/// `Ctrl+X d` with nothing attached is: a chord is a slip, and attaching to
/// an agent the user did not count off the screen is worse than doing
/// nothing. The draft is handed back either way.
///
/// Attaching goes through [`apply_agent_view_exit`], which is the one seam
/// every attach route shares: it ends the previous attachment, replays what
/// that one buffered under its own agent's header and prints the
/// `\u{26E9}` milestone, and it does all of that only on a **change** of
/// binding -- so the chord typed on the agent already attached is a no-op
/// rather than a second identical milestone, and the chord typed while
/// attached to another agent *switches*.
///
/// A function rather than lines in the arm for [`apply_inline_detach`]'s
/// reason: the loop around it needs a real tty, so this is the only seam a
/// test can drive, and there is exactly one of it (**F69**).
#[cfg(unix)]
pub(crate) fn apply_inline_attach_ordinal(
    registry: &subagent::AgentRegistry,
    attached: &mut Option<u64>,
    ordinal: usize,
    draft: String,
) -> String {
    // The composer has no filter of its own -- A3's filter line lives in the
    // overlay and dies with it -- so the state here is the default one, whose
    // empty filter matches every row. Going through `overlay_visible_rows`
    // anyway keeps one expression between the registry and what the view
    // displays, instead of a second path that could come to disagree with it.
    let rows = overlay_visible_rows(registry, &OverlayState::new());
    if let Some(id) = kitsu_tui::agents_view::nth_displayed(&rows, ordinal) {
        apply_agent_view_exit(
            &AgentViewExit {
                shown: false,
                attached: Some(id),
            },
            attached,
        );
    }
    draft
}

/// Step the attachment to the previous (`delta < 0`) or next (`delta > 0`)
/// agent in display order, and return the draft for the loop to replay.
///
/// Pure except for the `attached` mutation, and goes through the same
/// `overlay_visible_rows` + `display_order` the overlay and the ordinal attach
/// use, so the three never disagree about order. Clamps at both ends (no wrap);
/// when detached, `Shift+Right` lands on the first agent and `Shift+Left` is
/// inert, so a single chord from the normal composer attaches to something
/// predictable rather than the last row. An empty view leaves attachment as it
/// was.
#[cfg(unix)]
pub(crate) fn apply_inline_cycle_agent(
    registry: &subagent::AgentRegistry,
    attached: &mut Option<u64>,
    delta: isize,
    draft: String,
) -> String {
    let rows = overlay_visible_rows(registry, &OverlayState::new());
    let order = kitsu_tui::agents_view::display_order(&rows);
    if let Some(next) = cycle_target(&order, *attached, delta) {
        apply_agent_view_exit(
            &AgentViewExit {
                shown: false,
                attached: Some(next),
            },
            attached,
        );
    }
    draft
}

/// The agent a `Shift+Left`/`Shift+Right` step lands on, or `None` to leave
/// attachment unchanged. Pure over the display order so the clamp and the
/// detached-start rules are testable without a registry.
///
/// Clamps at both ends (no wrap). Detached: `+1` starts at the first agent,
/// `-1` stays detached. Empty order: nothing.
#[cfg(unix)]
pub(crate) fn cycle_target(order: &[u64], attached: Option<u64>, delta: isize) -> Option<u64> {
    if order.is_empty() {
        return None;
    }
    match attached.and_then(|id| order.iter().position(|row| *row == id)) {
        Some(current) => {
            let stepped = (current as isize + delta).clamp(0, order.len() as isize - 1);
            Some(order[stepped as usize])
        }
        None if delta > 0 => Some(order[0]),
        None => None,
    }
}

/// Which agent, if any, one submitted line belongs to (card **T4-S6b**,
/// requirement A5: *"while attached, the composer sends `steer`, or an
/// approval answer when the agent is blocked"*).
///
/// Pure, and the only decision about **whether** the composer's line leaves
/// the session at all: the composer loop asks this and, on `Some`, hands the
/// line to [`route_attached_submission`] instead of making it a turn.
///
/// Two lines are **not** the attached agent's, however attached the session
/// is: a slash command (which is how `/detach` can still be reached from an
/// attached composer -- a session that could not leave by command would be a
/// trap) and a blank line (which the loop already drops).
///
#[cfg(unix)]
pub(crate) fn attached_route_target(line: &str, attached: Option<u64>) -> Option<u64> {
    let id = attached?;
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('/') {
        return None;
    }
    Some(id)
}

/// What one routed composer line did to the attached agent (card **T4-S6b**).
#[cfg(unix)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum AttachedRoute {
    /// The line was queued as a correction on a working agent.
    Steered,
    /// The line was read as an approval answer -- by
    /// [`parse_permission_answer`], **the** parser (**F45**) -- and delivered
    /// to the blocked agent's parked request.
    Answered(Decision),
    /// Nothing was sent. Every refusal, including a correction typed while the
    /// agent was blocking.
    Refused,
}

/// One routed composer submission: what it did, and the draft the next read
/// must replay (**F76**).
#[cfg(unix)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AttachedSubmit {
    /// Which route the line took.
    pub(crate) route: AttachedRoute,
    /// Empty when the line was delivered; the user's line verbatim on every
    /// refusal, so a refused correction is still in the composer afterwards.
    pub(crate) draft: String,
}

/// Send one composer line to the agent the session is attached to (card
/// **T4-S6b**, requirement A5).
///
/// **The steer-versus-answer rule**, which A5 leaves open: the agent's state
/// is read **once, at submit time**, and
///
/// * a **blocked** agent is answered -- through
///   [`parse_permission_answer`] and nothing else -- and a line that parser
///   does not accept is **refused with nothing sent**, because a correction
///   typed while the agent happened to block must not be guessed at as
///   `y`/`a`/`n`;
/// * any other state is **steered**, and the registry is the final
///   adjudicator: an agent that blocked (or finished) between the read and
///   the call refuses in its own words rather than being answered from a
///   stale read.
///
/// So neither race can destroy what the user typed: every refusal hands the
/// line back in `draft` and paints its reason through the renderer's surface,
/// where the eye already is (**F71**).
///
/// This function is also the **only** place either route is taken, so the
/// answer the user reads and the thing that happened cannot come from two
/// places (**F69**): the receipt painted is `/jobs steer`'s and `/jobs
/// reply`'s own ([`format_steering_receipt`], [`jobs_reply_receipt`]), never a
/// second wording of them.
///
#[cfg(unix)]
pub(crate) fn route_attached_submission(
    registry: &subagent::AgentRegistry,
    id: u64,
    line: &str,
) -> AttachedSubmit {
    // The state, read once, here: everything below decides off this one read,
    // and the registry adjudicates whichever route it picks.
    let blocked = registry
        .progress_snapshot()
        .agents
        .into_iter()
        .any(|agent| agent.id == id && agent.status == subagent::AgentProgressStatus::Blocked);
    let (line_painted, route, delivered) = if blocked {
        // The decision the one parser reads, so `Answered` can carry it; the
        // report below calls that same parser, and this card adds no second
        // `match` on `y`/`a`/`n` (**F45**).
        let decision = parse_permission_answer(line);
        // `r`'s own path: its receipt on an answer, its refusal wording on a
        // line the parser cannot read -- never a second spelling of either.
        let report = agent_view_reply(registry, id, line);
        let route = match decision {
            Some(decision) if report.changed => AttachedRoute::Answered(decision),
            // Not an answer at all, or the agent stopped blocking in the gap:
            // nothing was sent, so the line comes back.
            _ => AttachedRoute::Refused,
        };
        let delivered = report.changed;
        (report.line, route, delivered)
    } else {
        // Any other state steers, and the registry has the last word: an
        // agent that blocked or finished while the line was being typed
        // refuses in its own words.
        match registry.steer(id, line) {
            Ok(receipt) => (
                format_steering_receipt(&receipt),
                AttachedRoute::Steered,
                true,
            ),
            Err(error) => (error.to_string(), AttachedRoute::Refused, false),
        }
    };
    // **F102**, the second feed point: this route `continue`s before the line
    // can become a `ChatReadline::Line`, so the foreground feed at the `line`
    // binding never sees it, and without this call `{`/`}` would skip
    // everything typed during an attachment -- a key that quietly does nothing
    // on one route (**F84**/**F75**). It lives here rather than at the caller
    // because `delivered` is this function's own answer.
    //
    // **Before** the receipt below, for the reason the foreground feed sits
    // before the turn: a prompt must be in the ring ahead of what it provoked.
    //
    // Nothing is fed on a refusal. `draft` hands the line back as the next
    // read's draft (**F76**), so a refused line was never submitted -- it is
    // still being edited, and a transcript must not claim it was said.
    if delivered {
        tui_bridge::note_submitted_prompt(line, tui_bridge::PromptOrigin::User);
    }
    // Through the renderer's surface, where the eye already is (**F71**).
    kitsu_render::surface::active_surface()
        .transcript_line(kitsu_render::surface::Fd::Stderr, &line_painted);
    AttachedSubmit {
        route,
        // **F76**: a delivered line is gone from the composer, and every
        // refusal hands it back as the next read's draft.
        draft: if delivered {
            String::new()
        } else {
            line.to_string()
        },
    }
}

/// The `/jobs view` arm's whole half of one agent view run (**F79**).
///
/// The arm kept only `shown` and threw the attachment away, so `/jobs view`
/// plus `Enter` attached to nothing while the chord and arrow routes worked:
/// **F75 verbatim in the third consumer of this exit value**. Both answers
/// now leave through one call, which is also the seam a test can drive —
/// `run_agent_view_overlay` itself declines outside a tty, so the arm is
/// otherwise unreachable from a test.
#[cfg(unix)]
pub(crate) fn apply_jobs_view_exit(exit: &AgentViewExit, attached: &mut Option<u64>) -> bool {
    apply_agent_view_exit(exit, attached);
    exit.shown
}

/// The refusal `/detach` gives when the session is not attached to anything
/// (card **T4-S6a**, requirement A5). A submitted line is an intention with
/// somewhere to put the answer (T2-S5b), so unlike the silent `Ctrl+X d`
/// chord the command answers even when there is nothing to do.
pub(crate) const DETACH_WITHOUT_ATTACHMENT: &str = "not attached to a background agent";

/// What `/detach` did, and therefore what it says (finding **F77**).
///
/// `/detach`'s arm was written while attachment was impossible, so it had one
/// answer to give; the rework that made attachment work never revisited it.
/// This is the decision the arm must take instead, kept as a value so the
/// answer and the clearing are the same event rather than two places that can
/// disagree.
///
/// **Wired (F77):** the `detach` arm of [`crate::chat::commands::handle_slash`] produces this
/// against the binding it was handed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DetachCommandOutcome {
    /// The composer was attached to this agent and is not any more. The agent
    /// itself is untouched: detaching **stops nothing** (design §4.5; stopping
    /// is `x` in the agent view or `/jobs stop`).
    Detached(u64),
    /// Nothing was attached, which is not a failure of the user's but is still
    /// answered.
    NothingAttached,
}

impl DetachCommandOutcome {
    /// The line the user reads.
    ///
    /// **Why a success line at all**, when `\u{2190}` and `Ctrl+X d` are
    /// silent: those are keypresses, and the prompt label changing back under
    /// the caret *is* their feedback, delivered where the eye already is. A
    /// submitted command scrolls the transcript and moves the eye into the
    /// region, which is the surface that has somewhere to put an answer
    /// (T2-S5b) — and it is the one moment a user who misread the command as
    /// "cancel the agent" can still be told otherwise, which is why the line
    /// repeats the registry's "stops nothing" clause rather than being a bare
    /// acknowledgement.
    pub(crate) fn report(self) -> String {
        match self {
            DetachCommandOutcome::Detached(id) => {
                format!("detached from agent {id}; it keeps running")
            }
            DetachCommandOutcome::NothingAttached => DETACH_WITHOUT_ATTACHMENT.to_string(),
        }
    }

    /// The dispatcher's answer. A detach that happened is a command that
    /// succeeded, with the structured record scripted automation reads; a
    /// detach with nothing attached is the refusal the PTY session pins.
    pub(crate) fn slash_result(self) -> SlashResult {
        match self {
            DetachCommandOutcome::Detached(id) => SlashResult::Continue(Some(serde_json::json!({
                "kind": "detach",
                "agent": id,
            }))),
            DetachCommandOutcome::NothingAttached => SlashResult::Error,
        }
    }
}

/// Perform `/detach` against the composer's session binding — the one home the
/// prompt label and the status row are both derived from (**F77**).
///
/// This is the whole of the command: [`crate::chat::commands::handle_slash`] calls it with
/// the binding it was handed and prints [`DetachCommandOutcome::report`], so
/// the answer cannot be computed from one state and the clearing applied to
/// another. The mirror for the agent view's half is
/// [`apply_agent_view_exit`].
///
/// **Wired (F77).** [`crate::chat::commands::handle_slash`] is handed the composer's own binding —
/// `chat::run_chat` passes `attached_agent` itself — and its other call site
/// (`run_scripted_body`) can never hold an attachment: the overlay declines
/// outside inline mode on a tty with styled stderr.
pub(crate) fn apply_detach_command(attached: &mut Option<u64>) -> DetachCommandOutcome {
    match attached.take() {
        Some(id) => {
            replay_foreground_recap(id);
            DetachCommandOutcome::Detached(id)
        }
        None => DetachCommandOutcome::NothingAttached,
    }
}

/// Open the agent view overlay and run it until it is closed, returning
/// whether it was opened at all (card T4-S2, requirement A1).
///
/// `false` is the answer in classic mode and on any carrier that cannot be
/// drawn into — a pipe, an assistive reader — and the caller then falls back to
/// whatever it printed before. That is what keeps classic's byte stream
/// identical: nothing here writes a byte before the decision is taken.
///
/// # The alternate screen, and why the restore is exact
///
/// The overlay takes the terminal's alternate screen (`ESC[?1049h`) and gives
/// it back on close (`ESC[?1049l`). The restore is then the *terminal's* own —
/// the transcript the user had is still on the primary screen, untouched,
/// rather than being repainted from a ring with the documented ring
/// limitation. The DECSTBM region and the pinned status row are re-established
/// afterwards through [`tui_bridge::reinstall_pinned_region`], because a
/// terminal that keeps per-screen margins would otherwise hand back an
/// unpinned one.
///
/// `engage_raw` is the caller's statement about whose raw mode is in force:
/// the idle reader already holds a `RawStdinGuard`, and nesting a second one
/// would snapshot the *raw* modes as the canonical ones for the dying-signal
/// restore.
#[cfg(unix)]
pub(crate) fn run_agent_view_overlay(
    registry: &subagent::AgentRegistry,
    engage_raw: bool,
    // Card **T4-S7**, requirement **A6**: how this run of the view starts an
    // agent. A parameter and not something the view builds, because the
    // binder, the subagent registry, the config template and the profile
    // catalog are all the session's — and a parameter because **both** of this
    // function's callers must be able to dispatch: the chord/`\u{2190}\u{2190}`
    // route and `/jobs view`. **F77/F79** is what a second carrier that
    // reached only one of them costs.
    view_dispatcher: Option<&subagent::ViewDispatcher>,
) -> AgentViewExit {
    use std::io::{Read as _, Write as _};

    if tui_bridge::live_mode() != tui_bridge::TuiMode::Inline {
        return AgentViewExit::declined();
    }
    if !kitsu_render::stdin_is_tty() || !kitsu_render::stderr_styled() {
        return AgentViewExit::declined();
    }
    // Held for the whole loop when it is this function's to hold, dropped on
    // every return path by being bound here.
    let _raw = if engage_raw {
        match RawStdinGuard::engage() {
            Some(guard) => Some(guard),
            // No raw mode means no keystrokes, which means no way back out of
            // a full-screen overlay. Declining is the only safe answer.
            None => return AgentViewExit::declined(),
        }
    } else {
        None
    };

    let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));
    let mut err = std::io::stderr();
    // Enter, clear, and hide the cursor: the view has no caret of its own, and
    // a cursor parked on a painted row reads as a prompt that is not there.
    // Bracketed paste on with the screen: the payload then arrives wrapped
    // in `ESC[200~`/`ESC[201~`, which is how [`OverlayDecoder`] tells a paste
    // from typing without guessing (finding **F68**). The idle reader's
    // `RawStdinGuard` has already enabled it when it owns raw mode, and this
    // guard's own `engage` does when the overlay owns it, so this is a
    // belt-and-braces statement of the mode by the surface that depends on
    // it; terminals without support ignore the sequence.
    //
    // It is deliberately **not** disabled on the way out: the composer
    // underneath needs it on, and whichever `RawStdinGuard` owns the terminal
    // turns it off in its `Drop` when stdin goes back to rustyline.
    // Card **F94**, and before the bytes: this is the overlay's half of the
    // flag every dying exit path reads (`kitsu_tui::alternate_screen_taken`).
    // The leave below is straight-line code after the read loop, so a panic
    // or a signal in the overlay never reaches it -- the handler and the
    // panic hook give the screen back instead, and this store is what tells
    // them they owe it.
    kitsu_tui::note_alternate_screen(true);
    let _ = err.write_all(b"\x1b[?1049h\x1b[2J\x1b[H\x1b[?25l\x1b[?2004h");
    let _ = err.flush();

    // Everything a key can change: which frame is on screen (`?` is help and
    // `Esc` steps back out of it, so the key never leaves the user somewhere
    // they cannot return from), the peek panel (card T4-S3), which agent the
    // keys act on (card T4-S4a; re-synced against freshly built rows on every
    // repaint, which is what keeps it on its *agent* while rows regroup under
    // it -- the sync acknowledges nothing, and neither does painting the
    // mark), what the last action key did and the prompt one of them has open
    // (card T4-S4b; both are painted as the one report row under the rows,
    // because a key that acts silently is indistinguishable from a key that
    // failed, requirement T2-S5b), and `s`'s steer line (card T4-S4b-ii,
    // which reads the raw bytes itself while it is open -- the reason the
    // editor is reusable here at all is that this is where the bytes are
    // read).
    //
    // It is a struct rather than seven locals because [`handle_overlay_input`]
    // is what changes it: one dispatch, which the tests call too (**F69**).
    let mut state = OverlayState::new();
    state.dispatcher = view_dispatcher.cloned();
    let mut stdin = std::io::stdin();
    let mut buffer = [0u8; 256];
    let mut decoder = OverlayDecoder::new();
    loop {
        let view_rows = overlay_visible_rows(registry, &state);
        state.selection.sync(&view_rows);
        let content = overlay_frame_rows(&state, &view_rows, cols as usize);
        // Repainted whole, with absolute addressing, and never into the last
        // row: the pinned composer row belongs to the viewport underneath, and
        // an overlay that wrote there would make its own keys indistinguishable
        // from typing.
        let mut frame = String::from("\x1b[2J");
        let budget = rows.saturating_sub(1) as usize;
        for (index, row) in content.iter().take(budget).enumerate() {
            frame.push_str(&format!("\x1b[{};1H\x1b[2K{row}", index + 1));
        }
        let _ = err.write_all(frame.as_bytes());
        let _ = err.flush();

        // An `ESC` byte is ambiguous until the next byte arrives *or does
        // not*: `Esc` is one byte, `\u{2193}` is `ESC [ B`. A terminal emits
        // the rest of a sequence in the same breath, so a quiet grace period
        // is what tells the two apart — the same answer T4-S2 gave at the
        // idle composer, where a lone `ESC` stays pending until the next byte
        // and is resolved on a quiet tick.
        // The steer line resolves its own held `ESC`, on the same grace and
        // for the same reason: inside it, `Esc` cancels the line and sends
        // nothing. A tick with no `ESC` held is not even waited for, so the
        // line costs no latency per keystroke.
        if overlay_escape_held(&state, &decoder) && !stdin_waiting(ESC_SEQUENCE_GRACE) {
            if overlay_resolve_held_escape(&mut state, &mut decoder) {
                break;
            }
            continue;
        }

        let count = match stdin.read(&mut buffer) {
            // A drained descriptor cannot deliver the key that would close the
            // view, so the view closes itself rather than spinning.
            Ok(0) => break,
            Ok(count) => count,
            // A signal woke the read (a `SIGWINCH`, typically): repaint and
            // carry on, which also re-reads the terminal's rows next frame.
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        };
        if overlay_feed_chunk(
            registry,
            &mut state,
            &mut decoder,
            &view_rows,
            cols as usize,
            &buffer[..count],
        ) {
            break;
        }
    }

    let _ = err.write_all(b"\x1b[?25h\x1b[?1049l");
    let _ = err.flush();
    // Card **F94**: after the bytes, so a signal in this window writes a
    // second leave onto a primary screen rather than none onto an alternate
    // one. The normal path has now given the screen back, so a dying signal
    // arriving afterwards owes nothing.
    kitsu_tui::note_alternate_screen(false);
    // The region and the pinned row, after the terminal has given the primary
    // screen back: control bytes and the bottom row only, so the restored
    // transcript is not disturbed.
    tui_bridge::reinstall_pinned_region();
    AgentViewExit::leaving(&state)
}
