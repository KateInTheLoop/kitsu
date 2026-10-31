use super::*;

// --- card T4-S3: peek (`Space`), and the exit guard ---
//
// The exit guard on unviewed results is
// `AgentRegistry::take_unobserved_terminal_results`, which the interactive
// loop calls once on its way out (`surface_unobserved_background_results`).
// It keys on the *handoff* -- `AgentHandoffState::Ready` -- and not on the
// phase, so a failed agent is in it exactly as a done one is. These tests
// are therefore pinned at unit level against a real `AgentRegistry`, which
// is where `peek_result`/`acknowledge_result` live and where the guard
// reads: a PTY session has no agents in it at all, so the round trip
// cannot be observed there. `Space`'s key routing is pinned in the PTY
// suite, which is the only place it can be.

/// Card T4-S3, requirement A7, forward direction. Peeking a done agent
/// acknowledges its handoff, so the exit guard no longer holds its result
/// back as unobserved -- and the same panel shows the bounded redacted
/// timeline and the result.
///
/// The backward direction is in the same test and is what makes the
/// forward one a claim: a second done agent that was never peeked is
/// still handed to the guard.
#[cfg(unix)]
#[test]
fn peeking_a_done_agent_acknowledges_it_so_the_exit_guard_lets_the_session_go() {
    let registry = subagent::AgentRegistry::new();
    let peeked = registry.start("peeked work".to_string()).unwrap();
    let untouched = registry.start("untouched work".to_string()).unwrap();
    registry.finish(peeked, Ok("the peeked answer".to_string()));
    registry.finish(untouched, Ok("the untouched answer".to_string()));
    assert_eq!(
        registry.snapshot().unacknowledged,
        2,
        "both results start out unobserved"
    );

    assert!(
        agent_view_peek(&registry, 9_999).is_none(),
        "an id the registry does not hold has no panel"
    );

    let snapshot = registry
        .thread_snapshot(peeked)
        .expect("the peeked agent has a thread snapshot");
    assert!(
        !snapshot.timeline.entries.is_empty(),
        "non-vacuous: this sample thread really did retain timeline events"
    );

    let panel = agent_view_peek(&registry, peeked)
        .unwrap_or_else(|| panic!("peek must answer for agent #{peeked}"))
        .join("\n");
    assert!(
        panel.contains("the peeked answer"),
        "A4: a done agent's result is in its peek panel: {panel}"
    );
    for entry in &snapshot.timeline.entries {
        assert!(
            panel.contains(&entry.text),
            "A4: the panel shows the same bounded redacted timeline \
                 `/jobs show` renders, so {:?} must be in it: {panel}",
            entry.text
        );
    }
    assert!(
        !panel.contains("the untouched answer"),
        "the panel is one agent's, not the deck's: {panel}"
    );

    assert_eq!(
        registry.handoff_state(peeked),
        Some(subagent::thread::AgentHandoffState::Acknowledged),
        "A7: viewing a done agent acknowledges its handoff, exactly as \
             `/jobs show` does"
    );
    assert_eq!(
        registry.handoff_state(untouched),
        Some(subagent::thread::AgentHandoffState::Ready),
        "peeking one row must not acknowledge another"
    );

    let unobserved = registry
        .take_unobserved_terminal_results()
        .iter()
        .map(|result| result.id)
        .collect::<Vec<_>>();
    assert_eq!(
        unobserved,
        vec![untouched],
        "the exit guard keeps the unpeeked result and lets the peeked one \
             go"
    );
}

/// Card T4-S3, requirement A7, the direction a careless implementation
/// gets wrong: **opening** the view, and **moving the selection** over its
/// rows, acknowledge nothing. Only `Space` does.
///
/// What opening paints is `agent_view_rows` fed through
/// `overlay_rows`; moving the selection repaints the same two sources, so
/// the loop below is every read the view performs short of a peek. An
/// implementer who adds selection state must keep that true of it too.
///
/// The POSITIVE CONTROL is in this test and on these same two rows:
/// peeking one of them does acknowledge, so the negative assertions above
/// it are about opening and moving rather than about a registry that never
/// acknowledges anything.
#[cfg(unix)]
#[test]
fn opening_the_agent_view_and_moving_over_its_rows_acknowledges_nothing() {
    let registry = subagent::AgentRegistry::new();
    let done = registry.start("done work".to_string()).unwrap();
    let failed = registry.start("failed work".to_string()).unwrap();
    registry.finish(done, Ok("the answer".to_string()));
    registry.finish(failed, Err("boom".to_string()));

    for _ in 0..3 {
        let rows = agent_view_rows(&registry);
        assert_eq!(rows.len(), 2, "both agents are rows in the view");
        for _selected in 0..rows.len() {
            let painted = kitsu_tui::agents_view::overlay_rows(agent_view_rows(&registry), 80);
            assert!(
                !painted.is_empty(),
                "non-vacuous: the overlay really painted a frame"
            );
        }
    }
    for id in [done, failed] {
        assert_eq!(
            registry.handoff_state(id),
            Some(subagent::thread::AgentHandoffState::Ready),
            "opening the view and moving the selection must not acknowledge \
                 agent #{id}"
        );
    }
    assert_eq!(
        registry.snapshot().unacknowledged,
        2,
        "the exit guard still holds both results after the view was opened"
    );

    let _ = agent_view_peek(&registry, failed)
        .unwrap_or_else(|| panic!("peek must answer for agent #{failed}"));
    assert_eq!(
        registry.handoff_state(failed),
        Some(subagent::thread::AgentHandoffState::Acknowledged),
        "POSITIVE CONTROL: a peek on one of the very rows just painted \
             does acknowledge"
    );
    assert_eq!(
        registry.handoff_state(done),
        Some(subagent::thread::AgentHandoffState::Ready),
        "and only the peeked row"
    );
    let unobserved = registry
        .take_unobserved_terminal_results()
        .iter()
        .map(|result| result.id)
        .collect::<Vec<_>>();
    assert_eq!(unobserved, vec![done]);
}

/// Card T4-S3, the row's named case: a **failed** agent is not quietly
/// exempted from the exit guard. The guard keys on the handoff, which
/// `AgentThreadState::finish` sets to `Ready` for a failure exactly as for
/// a success, so an unviewed failed result is still held back and a peeked
/// one is released.
#[cfg(unix)]
#[test]
fn peeking_a_failed_agent_shows_the_failure_and_releases_only_that_one() {
    let registry = subagent::AgentRegistry::new();
    let peeked = registry.start("will fail".to_string()).unwrap();
    let unviewed = registry.start("will also fail".to_string()).unwrap();
    registry.finish(peeked, Err("boom".to_string()));
    registry.finish(unviewed, Err("also boom".to_string()));
    for id in [peeked, unviewed] {
        assert_eq!(
            registry.handoff_state(id),
            Some(subagent::thread::AgentHandoffState::Ready),
            "a failure is handed off exactly as a success is"
        );
    }

    let panel = agent_view_peek(&registry, peeked)
        .unwrap_or_else(|| panic!("peek must answer for agent #{peeked}"))
        .join("\n");
    assert!(
        panel.contains(&format!("agent #{peeked} failed: boom")),
        "A4: the panel shows a failed agent's failure: {panel}"
    );
    assert!(
        panel.contains(subagent::thread::AgentThreadPhase::Failed.label()),
        "the panel names the phase in the DTO's own spelling: {panel}"
    );

    let unobserved = registry
        .take_unobserved_terminal_results()
        .iter()
        .map(|result| result.id)
        .collect::<Vec<_>>();
    assert_eq!(
        unobserved,
        vec![unviewed],
        "an unviewed failed agent is still held by the exit guard; the \
             peeked one is not"
    );
}

/// Card T4-S3, requirement A4 on a blocked agent: the panel names the
/// pending request by its **kind**, and the tool argument that produced it
/// is nowhere in it. That is structural rather than filtered -- the thread
/// snapshot carries `AgentApprovalKind`, a unit enum (F40, T2-S6) -- and
/// peeking a blocked agent acknowledges nothing, because it has no
/// handoff to acknowledge.
///
/// The absence is NON-VACUOUS because of the first half: redaction leaves
/// paths untouched, so a second agent in the same registry whose result
/// text holds the very same path shows it. A panel that scrubbed paths
/// wholesale, or one that printed nothing at all, fails there.
#[cfg(unix)]
#[tokio::test]
async fn peeking_a_blocked_agent_shows_the_permission_kind_and_never_the_tool_argument() {
    let (registry, blocked, _answer) = registry_with_one_blocked_agent().await;

    let leaky = registry.start("a second job".to_string()).unwrap();
    registry.finish(leaky, Ok("wrote /work/notes.txt".to_string()));
    let control = agent_view_peek(&registry, leaky)
        .unwrap_or_else(|| panic!("peek must answer for agent #{leaky}"))
        .join("\n");
    assert!(
        control.contains("/work/notes.txt"),
        "NON-VACUITY: this panel prints a path it is given, so the absence \
             asserted below is a real claim: {control}"
    );

    let panel = agent_view_peek(&registry, blocked)
        .unwrap_or_else(|| panic!("peek must answer for agent #{blocked}"))
        .join("\n");
    assert!(
        panel.contains(&format!(
            "permission to {}",
            subagent::thread::AgentApprovalKind::WriteFile.label()
        )),
        "A4: the panel names the pending request by its kind: {panel}"
    );
    for argument in ["/work/notes.txt", "notes.txt", "/work"] {
        assert!(
            !panel.contains(argument),
            "A4: the peek panel leaked a tool argument ({argument}): {panel}"
        );
    }
    assert_eq!(
        registry.handoff_state(blocked),
        Some(subagent::thread::AgentHandoffState::Unavailable),
        "a blocked agent has no handoff, so peeking it acknowledges nothing"
    );
}

// --- card T4-S4: selection, and the keys that act on the selected agent ---
//
// Pinned at unit level against a real `AgentRegistry` for the reason
// T4-S3 gave above and for one more: a fixture PTY session has zero
// agents, so a key bound to "the selected agent" has nothing to act on
// there and every outcome would be the no-selection one. The PTY answers
// only routing.

/// Card T4-S4, the regression T4-S3 left a note about: **moving the
/// selection acknowledges nothing.** The earlier test asserted that over a
/// stand-in for selection (a loop counter) because no selection existed;
/// this one asserts it over the real [`kitsu_tui::agents_view::AgentSelection`]
/// walking every row of a registry that really does have unacknowledged
/// results, which is the thing an implementer can break.
///
/// The POSITIVE CONTROL is in this test, on the very rows just walked: the
/// walk ends with `p`'s own call on one of them, which does acknowledge.
/// So the negative assertions are about moving rather than about a
/// registry that never acknowledges anything.
#[cfg(unix)]
#[test]
fn moving_the_agent_views_selection_over_every_row_acknowledges_nothing() {
    let registry = subagent::AgentRegistry::new();
    let done = registry.start("done work".to_string()).unwrap();
    let failed = registry.start("failed work".to_string()).unwrap();
    let running = registry.start("still running".to_string()).unwrap();
    registry.finish(done, Ok("the answer".to_string()));
    registry.finish(failed, Err("boom".to_string()));
    assert_eq!(
        registry.snapshot().unacknowledged,
        2,
        "non-vacuous: there really are two results waiting to be collected"
    );

    let rows = agent_view_rows(&registry);
    assert_eq!(rows.len(), 3, "all three agents are rows in the view");
    let mut selection = kitsu_tui::agents_view::AgentSelection::new();
    selection.sync(&rows);

    // Every row, down and back up, repainting each time exactly as the
    // overlay loop does — and `overlay_rows_selected` is the paint, so a
    // renderer that acknowledged while drawing the mark fails here.
    let mut visited = vec![selection.selected().expect("a row is selected")];
    for _ in 0..rows.len() * 2 {
        selection.move_down(&rows);
        let _ = kitsu_tui::agents_view::overlay_rows_selected(
            agent_view_rows(&registry),
            80,
            selection.selected(),
        );
        visited.push(selection.selected().expect("the selection clamps"));
    }
    for _ in 0..rows.len() * 2 {
        selection.move_up(&rows);
        let _ = kitsu_tui::agents_view::overlay_rows_selected(
            agent_view_rows(&registry),
            80,
            selection.selected(),
        );
        visited.push(selection.selected().expect("the selection clamps"));
    }
    for id in [done, failed, running] {
        assert!(
            visited.contains(&id),
            "non-vacuous: the walk really did select agent #{id}, so the \
                 assertions below are about a row that was moved over; \
                 visited: {visited:?}"
        );
    }

    for id in [done, failed] {
        assert_eq!(
            registry.handoff_state(id),
            Some(subagent::thread::AgentHandoffState::Ready),
            "moving the selection over agent #{id} must not take custody \
                 of its result"
        );
    }
    assert_eq!(
        registry.snapshot().unacknowledged,
        2,
        "the exit guard still holds both results after the selection \
             walked every row twice"
    );

    // POSITIVE CONTROL: `p`'s own registry call, on one of the rows just
    // walked over.
    assert_eq!(
        registry.acknowledge_result(failed),
        subagent::AgentHandoffAckOutcome::Acknowledged,
        "POSITIVE CONTROL: `p` on a row the selection just moved over does \
             acknowledge"
    );
    assert_eq!(
        registry.handoff_state(done),
        Some(subagent::thread::AgentHandoffState::Ready),
        "and only that row"
    );
    let unobserved = registry
        .take_unobserved_terminal_results()
        .iter()
        .map(|result| result.id)
        .collect::<Vec<_>>();
    assert_eq!(unobserved, vec![done]);
}

/// Card T4-S4, the one **new** registry method: `n` rename.
///
/// The name is what A2's row head shows in place of the task head, so a
/// rename that was accepted and then not shown, or coerced to something
/// the user did not type, is the silent-rejection failure this project
/// keeps refusing. `AgentRegistry::rename` therefore answers with an
/// outcome that names *why* a name was refused, and does not borrow
/// `safe_inline_label` — that helper coerces an empty label to `unknown`
/// and truncates with an ellipsis, which is right for a label the model
/// produced and wrong for a name a human just typed.
///
/// Legality, decided here because A3 does not say: a name is the trimmed
/// input; it must be non-empty, at most
/// `AgentRegistry::NAME_MAX_CHARS` characters, and free of control
/// characters (a newline or an `ESC` in a name would corrupt every row it
/// is drawn on). Any phase may be renamed, including a terminal one: an
/// agent whose result you are about to read is exactly the one worth
/// labelling.
#[cfg(unix)]
#[test]
fn renaming_the_selected_agent_shows_the_new_name_and_names_its_refusals() {
    use subagent::AgentRenameOutcome;

    // A name must be able to hold a short human phrase. Const-asserted so
    // narrowing the budget is a compile error.
    const _: () = assert!(subagent::AgentRegistry::NAME_MAX_CHARS >= 16);

    let registry = subagent::AgentRegistry::new();
    let id = registry.start("summarize the crate".to_string()).unwrap();
    let other = registry.start("run the build".to_string()).unwrap();

    // Before: the row head is the task head, and no name is carried.
    let head_of = |id: u64| {
        agent_view_rows(&registry)
            .into_iter()
            .find(|row| row.id == id)
            .unwrap_or_else(|| panic!("agent #{id} has a row"))
    };
    let before = head_of(id);
    assert_eq!(before.name, None, "an agent starts out unnamed");
    assert_eq!(
        before.head(),
        "summarize the crate",
        "non-vacuous: unnamed, the head is the task head"
    );

    assert_eq!(
        registry.rename(id, "reviewer-1"),
        AgentRenameOutcome::Renamed { previous: None },
        "a legal name is accepted, and the answer says what it replaced"
    );
    let after = head_of(id);
    assert_eq!(
        after.name.as_deref(),
        Some("reviewer-1"),
        "the name reaches the view's row through the thread snapshot, \
             which is the only mapping (`agent_row_from_thread_dto`)"
    );
    assert_eq!(
        after.head(),
        "reviewer-1",
        "A2's head is the name once there is one"
    );
    assert_eq!(
        registry
            .thread_snapshot(id)
            .expect("a snapshot")
            .name
            .as_deref(),
        Some("reviewer-1"),
        "and it is on the snapshot itself, so `/jobs show` and the DTO \
             carry it too rather than the overlay keeping a private name"
    );
    assert_eq!(
        head_of(other).name,
        None,
        "renaming one agent names one agent"
    );

    // Renaming again reports the name it replaced, so the user is never
    // left guessing whether the key reached the row they meant.
    assert_eq!(
        registry.rename(id, "  reviewer-2  "),
        AgentRenameOutcome::Renamed {
            previous: Some("reviewer-1".to_string()),
        },
        "surrounding whitespace is trimmed, which is not a coercion of \
             what the user meant"
    );
    assert_eq!(head_of(id).name.as_deref(), Some("reviewer-2"));

    // Every refusal is its own answer, and none of them changes the name.
    let too_long = "n".repeat(subagent::AgentRegistry::NAME_MAX_CHARS + 1);
    let refusals: Vec<(String, AgentRenameOutcome)> = vec![
        ("".to_string(), AgentRenameOutcome::Empty),
        ("   \t ".to_string(), AgentRenameOutcome::Empty),
        (
            too_long,
            AgentRenameOutcome::TooLong {
                max: subagent::AgentRegistry::NAME_MAX_CHARS,
            },
        ),
        ("two\nlines".to_string(), AgentRenameOutcome::Unprintable),
        (
            "esc\x1b[2Jhere".to_string(),
            AgentRenameOutcome::Unprintable,
        ),
    ];
    for (input, expected) in refusals {
        assert_eq!(
            registry.rename(id, &input),
            expected,
            "rename({input:?}) must be refused with a reason, not \
                 silently coerced"
        );
        assert_eq!(
            head_of(id).name.as_deref(),
            Some("reviewer-2"),
            "a refused rename leaves the name it had: {input:?}"
        );
    }

    // A name exactly at the budget is legal: the refusal above is about
    // one character too many and not about the budget being unusable.
    let at_budget = "m".repeat(subagent::AgentRegistry::NAME_MAX_CHARS);
    assert_eq!(
        registry.rename(id, &at_budget),
        AgentRenameOutcome::Renamed {
            previous: Some("reviewer-2".to_string()),
        },
        "the budget is inclusive"
    );
    assert_eq!(head_of(id).name.as_deref(), Some(at_budget.as_str()));

    // A non-ASCII name is a name: the rule is control characters, not
    // ASCII-only. A user naming an agent in their own language is not
    // typing an illegal name.
    assert!(matches!(
        registry.rename(id, "r\u{e9}viseur"),
        AgentRenameOutcome::Renamed { .. }
    ));
    assert_eq!(head_of(id).name.as_deref(), Some("r\u{e9}viseur"));

    assert_eq!(
        registry.rename(9_999, "ghost"),
        AgentRenameOutcome::NotFound,
        "renaming an id the registry does not hold is its own answer"
    );
}
// --- card T4-S4b: the action keys `r`, `s`, `x`, `n`, `p` ---
//
// Pinned at unit level against a real `AgentRegistry` for T4-S3's and
// T4-S4a's reason, which has not changed: a fixture PTY session has zero
// agents, so every key bound to "the selected agent" would answer the
// no-selection case there. The PTY answers routing only.

/// Card T4-S4b, the `r` key: **a decision read, not a text prompt**, and
/// the decision is resolved by `parse_permission_answer` — *the* parser,
/// the second one having been retired to close **F45**.
///
/// That the key really goes through that parser is pinned by behaviour
/// rather than by inspection: for every spelling the parser accepts, the
/// decision the gate receives is the one the parser returns for it. A key
/// with its own little match would have to agree with the parser on all
/// six, including `yes`/`always`/`no`, which no single keystroke produces
/// and a hand-rolled reader would therefore not bother with.
#[cfg(unix)]
#[tokio::test]
async fn replying_to_the_selected_agent_reads_a_decision_through_the_one_shared_parser() {
    let mut lines = Vec::new();
    for spelling in ["y", "yes", "a", "always", "n", "no"] {
        let expected = parse_permission_answer(spelling)
            .unwrap_or_else(|| panic!("NON-VACUOUS: the shared parser accepts {spelling:?}"));
        let (registry, id, answer) = registry_with_one_blocked_agent().await;
        let report = agent_view_reply(&registry, id, spelling);
        assert!(
            report.changed,
            "`r` with {spelling:?} answered the agent, so something changed: \
                 {report:?}"
        );
        assert_eq!(
            answer.await.expect("the gate is resolved"),
            expected,
            "the gate receives exactly the decision the shared parser \
                 returns for {spelling:?}"
        );
        assert_no_debug_leak(&report.line);
        lines.push(report.line);
    }

    // A spelling the parser refuses is refused here too, and the agent is
    // left blocked rather than answered with a guess.
    let (registry, id, answer) = registry_with_one_blocked_agent().await;
    assert!(
        parse_permission_answer("maybe").is_none(),
        "NON-VACUOUS: the shared parser refuses {:?}",
        "maybe"
    );
    let refused = agent_view_reply(&registry, id, "maybe");
    assert!(
        !refused.changed,
        "an unreadable decision changes nothing: {refused:?}"
    );
    assert_eq!(
        registry.answer(id, Decision::Deny),
        subagent::AgentAnswerOutcome::Answered,
        "NON-VACUOUS: the agent really was still blocked after the \
             refusal, so the refusal sent nothing"
    );
    assert_eq!(answer.await.expect("the gate is resolved"), Decision::Deny);
    lines.push(refused.line);

    // `r` on an agent that is not blocked, and on one that does not
    // exist: each its own answer, neither of them a success.
    let registry = subagent::AgentRegistry::new();
    let running = registry.start("nothing to answer".to_string()).unwrap();
    let not_blocked = agent_view_reply(&registry, running, "y");
    assert!(
        !not_blocked.changed,
        "`r` on an agent with no parked request changes nothing: \
             {not_blocked:?}"
    );
    let missing = agent_view_reply(&registry, 9_999, "y");
    assert!(!missing.changed, "`r` on an unknown id: {missing:?}");
    lines.push(not_blocked.line);
    lines.push(missing.line);

    // Every answer the user can meet reads differently. The six
    // successes may share wording with each other only if they name the
    // decision, so they are compared as a set against the refusals rather
    // than pairwise among themselves.
    for line in &lines {
        assert_no_debug_leak(line);
        assert!(
            line.contains("1") || line.contains("9999") || line.contains("9_999"),
            "every report names the agent it is about: {line:?}"
        );
    }
    let distinguishable = [
        lines[0].clone(),
        lines[6].clone(),
        lines[7].clone(),
        lines[8].clone(),
    ];
    for (index, left) in distinguishable.iter().enumerate() {
        for right in &distinguishable[index + 1..] {
            assert_ne!(
                left, right,
                "answered, unreadable, not-blocked and not-found must not \
                     print the same line (T2-S5b)"
            );
        }
    }
}

/// Card T4-S4b, the `s` key: the correction's **receipt is shown**, and an
/// empty line is a **refusal** rather than a no-op.
///
/// Steering restarts the thread and throws away every millisecond it has
/// already spent — the receipt says how many — so a user who typed a blank
/// line must be told rather than left wondering whether it went. The
/// registry already refuses one (`AgentSteeringErrorKind::Empty`); what
/// this pins is that the refusal becomes a line and not a silence.
///
/// The success path is pinned over a receipt rather than over a steered
/// agent: a steerable agent needs a live thread with an open correction
/// channel, which `AgentRegistry::start` does not give (it answers
/// `Closed`), and building one needs the provider, config and spawn
/// helpers that live in `subagent`'s own test module. That boundary is the
/// clearest argument for `s` being its own card.
#[cfg(unix)]
#[test]
fn steering_the_selected_agent_shows_its_receipt_and_names_every_refusal() {
    use subagent::thread::{AgentSteeringErrorKind, AgentSteeringReceipt};

    // A real sample: the receipt the registry hands back, formatted by
    // the formatter `/jobs steer` already prints.
    let receipt = AgentSteeringReceipt {
        id: 7,
        sequence: 1,
        pending: 0,
        discarded_ms: 2_500,
    };
    let sent = agent_view_steer_report(Ok(receipt));
    assert!(sent.changed, "a correction was sent: {sent:?}");
    assert!(
        sent.line.contains(&format_steering_receipt(&receipt)),
        "`s` shows the registry's own receipt, which reads {:?}; the \
             report line is {:?}",
        format_steering_receipt(&receipt),
        sent.line
    );
    assert!(
        sent.line.contains("discarded"),
        "so it says that in-flight progress was thrown away, because that \
             is the cost of the key: {:?}",
        sent.line
    );
    assert_no_debug_leak(&sent.line);

    // Every refusal the registry can answer with becomes its own line.
    // The messages are `AgentSteeringError`'s own `Display`, which is what
    // `/jobs steer` prints, rather than a second wording for the same
    // failure.
    let mut refusals = Vec::new();
    for kind in [
        AgentSteeringErrorKind::Empty,
        AgentSteeringErrorKind::NotRunning,
        AgentSteeringErrorKind::Closed,
        AgentSteeringErrorKind::UnknownAgent,
        AgentSteeringErrorKind::Full,
    ] {
        let error = subagent::thread::AgentSteeringError::new(7, kind, None);
        let report = agent_view_steer_report(Err(error.clone()));
        assert!(
            !report.changed,
            "a refused correction changes nothing: {report:?}"
        );
        assert!(
            report.line.contains(&error.to_string()),
            "and it says why, in the registry's own words ({}): {:?}",
            error,
            report.line
        );
        assert_no_debug_leak(&report.line);
        refusals.push(report.line);
    }
    for (index, left) in refusals.iter().enumerate() {
        for right in &refusals[index + 1..] {
            assert_ne!(
                left, right,
                "every refusal reads differently, because a human must be \
                     able to tell them apart (T2-S5b)"
            );
        }
        assert_ne!(
            *left, sent.line,
            "and none of them reads like a correction that went"
        );
    }

    // `agent_view_steer` is that report applied to the registry's answer,
    // not a second wording of it. Pinned on real samples: an empty line
    // and an unknown id, both of which a registry built with `start` can
    // really produce.
    let registry = subagent::AgentRegistry::new();
    let id = registry.start("long task".to_string()).unwrap();
    for (correction, target, kind) in [
        ("", id, AgentSteeringErrorKind::Empty),
        ("   ", id, AgentSteeringErrorKind::Empty),
        ("slow down", 9_999, AgentSteeringErrorKind::UnknownAgent),
    ] {
        let through_the_key = agent_view_steer(&registry, target, correction);
        assert!(
            !through_the_key.changed,
            "steer({correction:?}) on #{target} is refused: \
                 {through_the_key:?}"
        );
        assert_eq!(
            through_the_key,
            agent_view_steer_report(Err(subagent::thread::AgentSteeringError::new(
                target, kind, None
            ))),
            "the key reports the registry's answer through the one report \
                 function, rather than inventing a second wording for \
                 {kind:?}"
        );
    }
    // NON-VACUOUS: the registry really does answer those two, so the
    // equality above is not comparing two unimplemented branches.
    assert_eq!(
        registry
            .steer(id, "")
            .expect_err("an empty line is refused")
            .kind,
        AgentSteeringErrorKind::Empty
    );
    assert_eq!(
        registry
            .steer(9_999, "slow down")
            .expect_err("an unknown id is refused")
            .kind,
        AgentSteeringErrorKind::UnknownAgent
    );
}

/// Card T4-S4b, the `x` key's **second press**: it stops a live agent and
/// prunes an already-terminal one.
///
/// This is how A3's *"`x` stop (twice to prune)"* and T4-S4a's shipped
/// arming are both satisfied: T4-S4a's second press answers
/// `StopPress::Confirmed`, and what confirming *does* was never pinned.
/// Stopping an agent that has already finished is meaningless, so on a
/// finished agent the second press is the prune — no third press, no
/// second key, and no change to T4-S4a.
#[cfg(unix)]
#[tokio::test]
async fn stopping_the_selected_agent_twice_stops_a_live_one_and_prunes_a_terminal_one() {
    let registry = subagent::AgentRegistry::new();
    let live = registry.start("still working".to_string()).unwrap();
    let stopped = agent_view_stop_or_prune(&registry, live).await;
    assert!(stopped.changed, "a live agent was stopped: {stopped:?}");
    assert!(
        registry.thread_snapshot(live).is_some(),
        "stopping is not pruning: the row is still there to be read, \
             which is the whole difference between the two presses"
    );

    // The same agent again, now terminal: this press prunes it. Its
    // result is collected first, because an unviewed one is refused (see
    // the test below).
    let _ = registry.acknowledge_result(live);
    let pruned = agent_view_stop_or_prune(&registry, live).await;
    assert!(pruned.changed, "a terminal agent was pruned: {pruned:?}");
    assert!(
        registry.thread_snapshot(live).is_none(),
        "pruning removes the record: {pruned:?}"
    );
    assert_ne!(
        pruned.line, stopped.line,
        "stopped and pruned must not print the same line: a human cannot \
             otherwise tell which of the two just happened (T2-S5b)"
    );

    // A blocked agent is live, not terminal: the second press stops it
    // rather than pruning it, and its parked request is resolved.
    let (blocked_registry, blocked, answer) = registry_with_one_blocked_agent().await;
    let blocked_report = agent_view_stop_or_prune(&blocked_registry, blocked).await;
    assert!(
        blocked_report.changed,
        "a blocked agent is stoppable: {blocked_report:?}"
    );
    assert!(
        blocked_registry.thread_snapshot(blocked).is_some(),
        "and stopped, not pruned"
    );
    assert_eq!(
        answer.await.expect("the gate is resolved"),
        Decision::Interrupt,
        "NON-VACUOUS: the stop really reached the agent"
    );

    // An id the registry does not hold is its own answer.
    let missing = agent_view_stop_or_prune(&registry, 9_999).await;
    assert!(!missing.changed, "`x` on an unknown id: {missing:?}");
    for line in [&stopped.line, &pruned.line, &missing.line] {
        assert_no_debug_leak(line);
    }
    assert_ne!(missing.line, pruned.line);
    assert_ne!(missing.line, stopped.line);
}

/// Card T4-S4b, the one thing `x` must **refuse**: pruning an agent whose
/// result nobody has read.
///
/// `AgentRegistry::prune` removes the record, and
/// `take_unobserved_terminal_results` can only report records that exist
/// — so pruning an unviewed result destroys it *and* silences the exit
/// guard that T4-S3 and T4-S4a spent two cards protecting. `x` is the only
/// key that can do that, so it is the key that refuses.
///
/// The POSITIVE CONTROL is the second half: once the result is collected,
/// the very same press prunes.
#[cfg(unix)]
#[tokio::test]
async fn stopping_a_terminal_agent_whose_result_is_unviewed_refuses_to_prune_it() {
    let registry = subagent::AgentRegistry::new();
    let id = registry.start("will finish".to_string()).unwrap();
    registry.finish(id, Ok("nobody has read me".to_string()));
    assert_eq!(
        registry.handoff_state(id),
        Some(subagent::thread::AgentHandoffState::Ready),
        "NON-VACUOUS: the result really is waiting to be collected"
    );

    let refused = agent_view_stop_or_prune(&registry, id).await;
    assert!(
        !refused.changed,
        "an unviewed result is not destroyed by a keystroke: {refused:?}"
    );
    assert!(
        refused.line.contains("result"),
        "and the report says what is in the way, so the user knows to \
             peek or acknowledge first: {:?}",
        refused.line
    );
    assert!(
        registry.thread_snapshot(id).is_some(),
        "the record is still there: {refused:?}"
    );
    assert_eq!(
        registry
            .take_unobserved_terminal_results()
            .iter()
            .map(|result| result.id)
            .collect::<Vec<_>>(),
        vec![id],
        "and the exit guard still holds the result"
    );

    // POSITIVE CONTROL: with the result collected, the same press prunes.
    let registry = subagent::AgentRegistry::new();
    let id = registry.start("will finish".to_string()).unwrap();
    registry.finish(id, Ok("collect me".to_string()));
    assert!(
        agent_view_acknowledge(&registry, id).changed,
        "`p` collects it"
    );
    let pruned = agent_view_stop_or_prune(&registry, id).await;
    assert!(
        pruned.changed,
        "POSITIVE CONTROL: a collected result can be pruned, so the \
             refusal above is about the unviewed one: {pruned:?}"
    );
    assert!(registry.thread_snapshot(id).is_none());
    assert_ne!(
        pruned.line, refused.line,
        "pruned and refused are two answers (T2-S5b)"
    );
}

/// Card T4-S4b, the `n` key: `AgentRegistry::rename`'s refusals each
/// become their own line. The legality rules themselves are T4-S4a's and
/// are pinned there; this is the wiring, which is what T2-S5b is about.
#[cfg(unix)]
#[test]
fn renaming_the_selected_agent_through_the_n_key_reports_each_refusal() {
    let registry = subagent::AgentRegistry::new();
    let id = registry.start("summarize the crate".to_string()).unwrap();

    let named = agent_view_rename(&registry, id, "reviewer-1");
    assert!(named.changed, "a legal name is accepted: {named:?}");
    assert!(
        named.line.contains("reviewer-1"),
        "and the report shows the name that is now on the row: {:?}",
        named.line
    );

    let mut lines = vec![named.line.clone()];
    for illegal in [
        "",
        "   ",
        &"n".repeat(subagent::AgentRegistry::NAME_MAX_CHARS + 1),
        "two\nlines",
    ] {
        let report = agent_view_rename(&registry, id, illegal);
        assert!(
            !report.changed,
            "rename({illegal:?}) is refused: {report:?}"
        );
        assert_no_debug_leak(&report.line);
        lines.push(report.line);
    }
    let missing = agent_view_rename(&registry, 9_999, "ghost");
    assert!(!missing.changed, "`n` on an unknown id: {missing:?}");
    lines.push(missing.line);
    assert_eq!(
        registry
            .thread_snapshot(id)
            .expect("a snapshot")
            .name
            .as_deref(),
        Some("reviewer-1"),
        "NON-VACUOUS: every refusal left the name the one legal rename \
             gave it, so none of them was quietly applied"
    );

    // Empty, too long, unprintable and not-found are four refusals, and
    // the acceptance is a fifth answer: five lines, five readings. The
    // two empty spellings share an answer by design (both are `Empty`),
    // so they are compared as one.
    let distinct = [
        lines[0].clone(),
        lines[1].clone(),
        lines[3].clone(),
        lines[4].clone(),
        lines[5].clone(),
    ];
    for (index, left) in distinct.iter().enumerate() {
        for right in &distinct[index + 1..] {
            assert_ne!(
                left, right,
                "every rename answer reads differently (T2-S5b)"
            );
        }
    }
    assert_eq!(
        lines[1], lines[2],
        "a trimmed-to-nothing name is the same refusal as an empty one, \
             which is `AgentRenameOutcome::Empty`'s own grouping"
    );
}

/// Card T4-S4b, the custody property for a third time. T4-S3 made peek the
/// single call site that takes custody of a result; T4-S4a pinned that
/// moving the selection does not become a second one. **`r`, `s`, `x` and
/// `n` must not become one either.** `p` is the deliberate exception, and
/// it is this test's positive control.
///
/// `x` is the interesting one: its prune *removes* a record, which is a
/// stronger thing than acknowledging one, and the sibling test above pins
/// that it refuses to do so to an unviewed result. Here it is the stop,
/// on a live agent, that must leave every other result alone.
#[cfg(unix)]
#[tokio::test]
async fn the_action_keys_other_than_p_take_no_custody_of_a_result() {
    let registry = subagent::AgentRegistry::new();
    let done = registry.start("done work".to_string()).unwrap();
    let failed = registry.start("failed work".to_string()).unwrap();
    let live = registry.start("still running".to_string()).unwrap();
    registry.finish(done, Ok("the answer".to_string()));
    registry.finish(failed, Err("boom".to_string()));
    assert_eq!(
        registry.snapshot().unacknowledged,
        2,
        "NON-VACUOUS: there really are two results waiting to be collected"
    );

    // `n` on a done one and `x` on the live one: keys that act, and whose
    // action is not a collection. `s` is here as its refusal only --
    // steering a registry built with `start` answers `Closed` -- so the
    // key that acts and collects nothing is `n`, with `x` the destructive
    // one.
    assert!(!agent_view_steer(&registry, live, "slow down").changed);
    assert!(agent_view_rename(&registry, done, "the-one-with-the-answer").changed);
    assert!(agent_view_stop_or_prune(&registry, live).await.changed);
    for id in [done, failed] {
        assert_eq!(
            registry.handoff_state(id),
            Some(subagent::thread::AgentHandoffState::Ready),
            "no action key took custody of agent #{id}'s result"
        );
    }
    assert_eq!(
        registry.snapshot().unacknowledged,
        2,
        "the exit guard still holds both results"
    );

    // `r` has its own registry, because answering needs a blocked agent
    // and a blocked agent is the one phase that has no result yet. What
    // it must not do is collect somebody else's.
    let (replying, blocked, answer) = registry_with_one_blocked_agent().await;
    let collectable = replying.start("done elsewhere".to_string()).unwrap();
    replying.finish(collectable, Ok("not yours".to_string()));
    assert!(agent_view_reply(&replying, blocked, "y").changed);
    assert_eq!(
        answer.await.expect("the gate is resolved"),
        Decision::AllowOnce
    );
    assert_eq!(
        replying.handoff_state(collectable),
        Some(subagent::thread::AgentHandoffState::Ready),
        "`r` collects nothing"
    );

    // POSITIVE CONTROL: `p`, the deliberate exception, does collect.
    assert!(
        agent_view_acknowledge(&registry, done).changed,
        "POSITIVE CONTROL: `p` takes custody, so the assertions above are \
             about the other keys and not about a registry that never \
             acknowledges anything"
    );
    assert_eq!(
        registry
            .take_unobserved_terminal_results()
            .iter()
            .map(|result| result.id)
            .collect::<Vec<_>>(),
        vec![failed],
        "and only the row `p` was pressed on"
    );
}

/// Card T4-S4b: **an action key with no selection says so.** T4-S4a gave
/// `press_stop` a `NoSelection` answer rather than a silent `Armed` for
/// this reason, and the same holds for the other four keys: a key that
/// looks broken is worse than one that says there is nothing to act on.
///
/// The NON-VACUOUS half is the last assertion: the no-selection line is
/// not the line any key prints when it does act.
#[cfg(unix)]
#[test]
fn an_action_key_with_nothing_selected_says_so_rather_than_nothing() {
    let mut lines = Vec::new();
    for key in ['r', 's', 'x', 'n', 'p'] {
        let report = agent_view_no_selection(key);
        assert!(
            !report.changed,
            "with nothing selected, `{key}` changes nothing: {report:?}"
        );
        assert!(
            !report.line.trim().is_empty(),
            "but it is not silent either: `{key}` printed {:?}",
            report.line
        );
        assert_no_debug_leak(&report.line);
        lines.push(report.line);
    }

    let registry = subagent::AgentRegistry::new();
    let id = registry.start("summarize the crate".to_string()).unwrap();
    let acted = agent_view_rename(&registry, id, "reviewer-1");
    assert!(acted.changed, "NON-VACUOUS: a key with a selection acts");
    for line in &lines {
        assert_ne!(
            *line, acted.line,
            "the no-selection report is not what a key prints when it does \
                 act"
        );
    }
}

/// Finding **F68**: **a pasted chunk never stops or prunes an agent.**
///
/// `x` arms on the first press and stops on the second, so a paste
/// carrying two `x` bytes is a stop. The agent must still be running
/// afterwards.
///
/// The POSITIVE CONTROL is the second half, and it is the same two bytes:
/// two reads of `x` really do stop a live agent.
#[cfg(unix)]
#[tokio::test]
async fn a_pasted_chunk_containing_two_x_bytes_never_stops_an_agent() {
    assert!(
        PASTED_CORRECTION.matches('x').count() >= 2,
        "NON-VACUOUS: the sample really does carry at least two `x` bytes, \
             which is an armed stop and then a confirmed one: \
             {PASTED_CORRECTION:?}"
    );

    let registry = subagent::AgentRegistry::new();
    let id = registry.start("still working".to_string()).unwrap();
    let mut overlay = OverlayUnderTest::new();
    overlay.feed(&registry, PASTED_CORRECTION.as_bytes());
    let rows = agent_view_rows(&registry);
    assert_eq!(
        rows.iter().find(|row| row.id == id).map(|row| row.state),
        Some(kitsu_render::AgentViewState::Working),
        "the paste stopped nothing: agent #{id} is still running. Rows: \
             {rows:?}, reports: {:?}",
        overlay.reports
    );

    // POSITIVE CONTROL: the same two bytes, one read each.
    let mut typed = OverlayUnderTest::new();
    typed.feed(&registry, b"x");
    typed.feed(&registry, b"x");
    let after = agent_view_rows(&registry);
    assert_ne!(
        after.iter().find(|row| row.id == id).map(|row| row.state),
        Some(kitsu_render::AgentViewState::Working),
        "POSITIVE CONTROL: `x` twice, typed as two reads, really does \
             stop agent #{id}, so the assertion above is about the paste. \
             Rows: {after:?}, reports: {:?}",
        typed.reports
    );
}

/// **The race, settled: a correction typed while the agent blocks.**
///
/// The line is not `y`, `a` or `n`, so it is not an answer; the agent is
/// blocked, so it cannot be steered. The rule is that **nothing is sent**
/// and **nothing is lost**: the agent stays blocked (the one state from
/// which the user can still answer it), the line comes back as the next
/// read's draft (**F76**), and the reason is painted where the eye is
/// (**F71**).
///
/// The refusal's wording is **derived from `r`'s**: `agent_view_reply`
/// with an unreadable key returns its refusal before touching the
/// registry, so calling it here is side-effect free and pins "the
/// composer refuses in the words the decision read already uses". A
/// second wording of "that is not an answer" is exactly the drift F45 was
/// closed to stop.
#[cfg(unix)]
#[tokio::test]
#[serial]
async fn a_correction_typed_while_the_agent_blocked_sends_nothing_and_keeps_the_line() {
    const CORRECTION: &str = "no, use the byte offsets from the lexer";
    assert!(
        parse_permission_answer(CORRECTION).is_none(),
        "premise: the one parser does not read a correction as an answer"
    );

    let (registry, id, mut answer) = registry_with_one_blocked_agent().await;
    let expected_refusal = agent_view_reply(&registry, id, CORRECTION);
    assert!(
        !expected_refusal.changed,
        "premise: `r`'s refusal path changes nothing, so deriving the \
             wording from it leaves the agent blocked"
    );
    let probe = SurfaceProbe::install();

    let submit = route_attached_submission(&registry, id, CORRECTION);

    assert_eq!(
        submit.route,
        AttachedRoute::Refused,
        "a correction is never guessed at as `y`/`a`/`n`"
    );
    assert_eq!(
        submit.draft, CORRECTION,
        "F76: the half-typed line survives the refusal -- losing it is a \
             defect this epic has already paid for once"
    );
    assert!(
        matches!(
            answer.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty)
        ),
        "nothing reached the agent's parked request"
    );
    assert_eq!(
        registry.snapshot().blocked,
        1,
        "and it is still blocked, which is the one state it can still be \
             answered from"
    );
    assert_eq!(
        probe.lines(),
        vec![expected_refusal.line],
        "F71: the refusal is painted through the surface, in the words \
             `r`'s decision read already uses"
    );
}
