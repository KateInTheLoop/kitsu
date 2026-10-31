use super::*;

#[test]
fn terminal_cleanup_state_counts_only_actual_unjoined_residue() {
    let joined = subagent::AgentTerminalStatus {
        id: 1,
        task: "joined".to_string(),
        status: subagent::AgentStatus::Cancelled("bounded stop".to_string()),
        joined: true,
    };
    let residue = subagent::AgentTerminalStatus {
        id: 2,
        task: "unattached".to_string(),
        status: subagent::AgentStatus::Cancelled("malformed fixture".to_string()),
        joined: false,
    };

    assert_eq!(
        terminal_cleanup_state(&[]),
        kitsu_core::RunCleanupState::Joined
    );
    assert_eq!(
        terminal_cleanup_state(&[joined, residue]),
        kitsu_core::RunCleanupState::Residue {
            unjoined: std::num::NonZeroU32::new(1).unwrap()
        }
    );
}

#[test]
fn the_profile_view_renders_the_dto_not_its_debug_form() {
    let mut catalog = subagent::profile::AgentProfileCatalog::new();
    catalog
        .insert(subagent::profile::AgentProfile {
            name: "reviewer".to_string(),
            model: None,
            reasoning_effort: subagent::profile::AgentReasoningEffort::Inherit,
            instructions: "Review the delegated evidence critically.".to_string(),
            tool_policy: subagent::profile::AgentToolPolicy::AllowList(vec![
                "fs_read_file".to_string(),
                "fs_list_dir".to_string(),
            ]),
            permission_defaults: subagent::profile::AgentPermissionDefaults {
                inherit_session: true,
                ..Default::default()
            },
        })
        .unwrap();
    catalog
        .insert(subagent::profile::AgentProfile::reasoning_only(
            "reasoning",
            "Reason from the supplied task only.",
        ))
        .unwrap();
    let rendered = format_agent_profiles(&catalog);
    assert_no_debug_leak(&rendered);
    assert!(rendered.contains("fs_read_file, fs_list_dir"), "{rendered}");
    assert!(rendered.contains("effort inherit"), "{rendered}");
    // The two fields F5 found dropped from the human view only.
    assert!(
        rendered.contains("Review the delegated evidence critically."),
        "{rendered}"
    );
    assert!(rendered.contains("inherits session grants"), "{rendered}");
    assert!(
        rendered.contains("isolated from session grants"),
        "{rendered}"
    );
    // An empty allow-list explains itself instead of printing `AllowList([])`.
    let mut narrowed = subagent::profile::AgentProfileCatalog::new();
    narrowed
        .insert(subagent::profile::AgentProfile {
            name: "reviewer".to_string(),
            model: None,
            reasoning_effort: subagent::profile::AgentReasoningEffort::Inherit,
            instructions: String::new(),
            tool_policy: subagent::profile::AgentToolPolicy::AllowList(Vec::new()),
            permission_defaults: subagent::profile::AgentPermissionDefaults::default(),
        })
        .unwrap();
    let narrowed = format_agent_profiles(&narrowed);
    assert_no_debug_leak(&narrowed);
    assert!(
        narrowed.contains("no tools - this run granted no read roots"),
        "{narrowed}"
    );
}

#[test]
fn the_thread_view_uses_the_dto_labels_for_phase_handoff_and_timeline() {
    let snapshot = subagent::thread::AgentThreadSnapshot {
        id: 1,
        task: "review the parser".to_string(),
        model: "test-model".to_string(),
        profile: Some("reviewer".to_string()),
        name: None,
        phase: subagent::thread::AgentThreadPhase::Running,
        waiting_for: None,
        handoff: subagent::thread::AgentHandoffState::Unavailable,
        elapsed_ms: 1200,
        result_bytes: None,
        steering_open: true,
        pending_steering: 0,
        timeline: subagent::thread::AgentTimelineSnapshot {
            entries: vec![subagent::thread::AgentTimelineEntry {
                sequence: 1,
                elapsed_ms: 10,
                kind: subagent::thread::AgentTimelineKind::Provider,
                round: Some(1),
                text: "request started".to_string(),
                truncated: false,
            }],
            dropped_entries: 0,
            retained_bytes: "request started".len(),
            closed: false,
        },
    };
    let rendered = format_agent_thread(&snapshot);
    assert_no_debug_leak(&rendered);
    assert!(rendered.contains("agent #1 running"), "{rendered}");
    assert!(rendered.contains("handoff: unavailable"), "{rendered}");
    assert!(rendered.contains("provider]"), "{rendered}");
    // Every human label equals its DTO field value.
    let dto = serde_json::to_value(subagent::thread::AgentThreadSnapshotDto::from_snapshot(
        &snapshot,
    ))
    .unwrap();
    assert_eq!(dto["thread"]["phase"], snapshot.phase.label());
    assert_eq!(dto["thread"]["handoff"], snapshot.handoff.label());
    assert_eq!(
        dto["thread"]["timeline"]["entries"][0]["kind"],
        snapshot.timeline.entries[0].kind.label()
    );
}

#[test]
fn the_jobs_deck_marks_each_state_for_the_active_terminal_mode() {
    let registry = subagent::AgentRegistry::default();
    let _running = registry.start("still working".to_string()).unwrap();
    let done = registry.start("finished cleanly".to_string()).unwrap();
    let failed = registry.start("broke".to_string()).unwrap();
    registry.finish(done, Ok("collected later".to_string()));
    registry.finish(failed, Err("provider request failed".to_string()));

    // A non-TTY never receives escape bytes. Ambient NO_COLOR is allowed to
    // select the documented ASCII rendering; explicit glyph/plain mappings
    // are covered by mode-pure kitsu-render tests.
    let styled = format_jobs(&registry);
    assert_no_debug_leak(&styled);
    assert!(!styled.contains('\u{1b}'), "{styled}");
    if kitsu_render::plain_mode() {
        assert!(styled.starts_with("# agent deck"), "{styled}");
        assert!(styled.contains("[run] #"), "{styled}");
        assert!(styled.contains("[rdy] #"), "{styled}");
        assert!(styled.contains("[err] #"), "{styled}");
    } else {
        assert!(styled.starts_with("◈ agent deck"), "{styled}");
        assert!(styled.contains("◆ #"), "{styled}");
        assert!(styled.contains("◈ #"), "{styled}");
        assert!(styled.contains("◇ #"), "{styled}");
    }
    assert!(styled.contains(&format!("/jobs show {done}")), "{styled}");
}

#[test]
fn running_deck_rows_carry_an_activity_child_and_finished_rows_do_not() {
    let registry = subagent::AgentRegistry::default();
    let running = registry.start("still working".to_string()).unwrap();
    let done = registry.start("finished cleanly".to_string()).unwrap();
    registry.finish(done, Ok("collected".to_string()));

    let deck = format_jobs(&registry);
    // Exactly one running row gets exactly one activity child line, with
    // the glyph and separator selected by the active terminal mode.
    let activity = if kitsu_render::plain_mode() {
        "-> started | 0s ago"
    } else {
        "↳ started · 0s ago"
    };
    assert_eq!(deck.matches(activity).count(), 1, "{deck}");
    let _ = running;

    // Formatter tiers: plain uses ASCII arrow and separator; no-activity
    // and no-age cases degrade gracefully.
    assert_eq!(
        agent_activity_child_row(Some("started tool Read"), Some(4_200), false),
        "↳ started tool Read · 4s ago"
    );
    assert_eq!(
        agent_activity_child_row(Some("usage updated"), Some(65_000), true),
        "-> usage updated | 1m05s ago"
    );
    assert_eq!(
        agent_activity_child_row(Some("started"), None, false),
        "↳ started"
    );
    assert_eq!(
        agent_activity_child_row(None, Some(10), false),
        "↳ no activity yet"
    );
}

#[test]
fn the_jobs_view_groups_agents_by_state_in_the_documented_order() {
    let registry = subagent::AgentRegistry::default();
    let _running = registry.start("still working".to_string()).unwrap();
    let ready = registry.start("finished cleanly".to_string()).unwrap();
    let collected = registry.start("already picked up".to_string()).unwrap();
    let failed = registry.start("broke".to_string()).unwrap();
    registry.finish(ready, Ok("result awaiting pickup".to_string()));
    registry.finish(collected, Ok("result already read".to_string()));
    registry.finish(failed, Err("provider request failed".to_string()));
    // Observing the result is what moves a row from "result ready" to
    // "collected"; peeking must not.
    assert!(registry.result(collected).is_some());

    let view = format_jobs_view(&registry);
    assert_no_debug_leak(&view);
    assert!(!view.contains('\u{1b}'), "{view}");

    let working = jobs_view_heading_offset(&view, "working").unwrap_or_else(|| panic!("{view}"));
    let result_ready =
        jobs_view_heading_offset(&view, "done, result ready").unwrap_or_else(|| panic!("{view}"));
    let done_collected =
        jobs_view_heading_offset(&view, "done, collected").unwrap_or_else(|| panic!("{view}"));
    let failed_heading =
        jobs_view_heading_offset(&view, "failed").unwrap_or_else(|| panic!("{view}"));
    assert!(
        working < result_ready && result_ready < done_collected,
        "{view}"
    );
    assert!(done_collected < failed_heading, "{view}");

    assert_eq!(jobs_view_heading_count(&view, "working"), Some(1), "{view}");
    assert_eq!(
        jobs_view_heading_count(&view, "done, result ready"),
        Some(1),
        "{view}"
    );
    assert_eq!(
        jobs_view_heading_count(&view, "done, collected"),
        Some(1),
        "{view}"
    );
    assert_eq!(jobs_view_heading_count(&view, "failed"), Some(1), "{view}");
}

#[test]
fn jobs_view_omits_empty_groups_and_never_shows_needs_input_today() {
    let registry = subagent::AgentRegistry::default();
    let running = registry.start("still working".to_string()).unwrap();
    let _ = running;

    let view = format_jobs_view(&registry);
    assert!(
        jobs_view_heading_offset(&view, "working").is_some(),
        "{view}"
    );
    // Nothing is cancelled, done or failed, so those headings are absent
    // entirely rather than printed with a zero count.
    assert!(!view.contains("cancelled ("), "{view}");
    assert!(!view.contains("failed ("), "{view}");
    assert!(!view.contains("done, "), "{view}");
    // No state can reach "needs input" yet, so the group is unreachable.
    assert!(!view.contains("needs input"), "{view}");
}

/// One wire word, three vocabularies: the `/jobs` JSON `status`, the
/// terminal-status label and the thread DTO `phase` all spell the new
/// state `blocked`, so the deck, the JSON and the hook cannot drift.
#[test]
fn every_blocked_label_is_the_single_word_blocked() {
    assert_eq!(
        agent_progress_status_label(subagent::AgentProgressStatus::Blocked),
        "blocked"
    );
    assert_eq!(
        terminal_agent_status_label(&subagent::AgentStatus::Blocked),
        "blocked"
    );
    assert_eq!(
        subagent::thread::AgentThreadPhase::Blocked.label(),
        "blocked"
    );
    for label in [
        agent_progress_status_label(subagent::AgentProgressStatus::Blocked),
        subagent::thread::AgentThreadPhase::Blocked.label(),
    ] {
        assert!(label.is_ascii(), "deck labels stay ASCII: {label}");
    }
}

/// T1 built the `needs input` group ahead of the state. `Blocked` maps
/// onto it rather than introducing a seventh heading.
#[test]
fn a_blocked_agent_belongs_to_the_needs_input_group() {
    let agent = blocked_progress(3);
    assert_eq!(jobs_view_group(&agent, false), "needs input");
    assert_eq!(
        jobs_view_group(&agent, true),
        "needs input",
        "collection state cannot move a non-terminal agent"
    );
}

/// The deck row for a blocked agent: its own `Blocked` marker — **not** the
/// busy one — and the row's own status word is `blocked`.
///
/// **F34 closes here.** T2-S3 deferred the `kitsu-render` variant and this
/// test recorded the deferral by asserting `DeckState::Running`. The
/// product legitimately changed: `DeckState::Blocked` now exists, so the
/// honest reuse this test documented is no longer the honest answer, and
/// the deck row the user has to act on no longer renders identically to one
/// that is merely working.
///
/// Collection state cannot move a non-terminal agent, so both
/// `result_collected` values map the same way.
#[test]
fn the_blocked_deck_row_is_marked_blocked_and_reads_blocked() {
    assert_eq!(
        agent_deck_state(subagent::AgentProgressStatus::Blocked, false),
        kitsu_render::DeckState::Blocked
    );
    assert_eq!(
        agent_deck_state(subagent::AgentProgressStatus::Blocked, true),
        kitsu_render::DeckState::Blocked
    );
    let row = agent_deck_row_block(&blocked_progress(3), false, 2, 6);
    assert!(row.contains("blocked"), "{row}");
}

/// **F34, the visible half.** The whole point is the rendered row, not the
/// enum: a blocked row and a running row must not begin with the same
/// bytes. Derived from real samples of `agent_deck_row_block`, and compared
/// exactly rather than by substring (F41).
#[test]
fn a_blocked_deck_row_does_not_begin_like_a_running_row() {
    let blocked = agent_deck_row_block(&blocked_progress(3), false, 2, 6);
    let mut running_progress = blocked_progress(3);
    running_progress.status = subagent::AgentProgressStatus::Running;
    let running = agent_deck_row_block(&running_progress, false, 2, 6);

    let blocked_marker = kitsu_render::deck_marker(kitsu_render::DeckState::Blocked);
    let running_marker = kitsu_render::deck_marker(kitsu_render::DeckState::Running);
    assert_ne!(blocked_marker, running_marker);

    // The row's exact first line, not a substring of it. Everything after
    // the marker is identical between the two fixtures, which is what makes
    // the marker the only thing distinguishing them.
    assert_eq!(
        blocked
            .lines()
            .nth(1)
            .unwrap_or_else(|| panic!("{blocked}")),
        format!("  {blocked_marker} #3 blocked | reviewer | summarize the crate")
    );
    assert_eq!(
        running
            .lines()
            .nth(1)
            .unwrap_or_else(|| panic!("{running}")),
        format!("  {running_marker} #3 running | reviewer | summarize the crate")
    );
}

/// The rest of `agent_deck_state` is unchanged: adding `Blocked` must not
/// disturb the four mappings that shipped. Exhaustive over the status
/// vocabulary crossed with both collection states, so a fifth status or a
/// re-pointed arm cannot slip past.
#[test]
fn adding_blocked_leaves_every_other_deck_state_mapping_alone() {
    use kitsu_render::DeckState;
    use subagent::AgentProgressStatus as Status;
    let cases = [
        (Status::Running, false, DeckState::Running),
        (Status::Running, true, DeckState::Running),
        (Status::Blocked, false, DeckState::Blocked),
        (Status::Blocked, true, DeckState::Blocked),
        (Status::Done, false, DeckState::ResultReady),
        (Status::Done, true, DeckState::Stopped),
        (Status::Failed, false, DeckState::Failed),
        (Status::Failed, true, DeckState::Failed),
        (Status::Cancelled, false, DeckState::Stopped),
        (Status::Cancelled, true, DeckState::Stopped),
    ];
    for (status, collected, expected) in cases {
        assert_eq!(
            agent_deck_state(status, collected),
            expected,
            "{status:?} collected={collected}"
        );
    }
}

/// The `/jobs` deck still marks every state it renders, with `blocked`
/// now a fifth marker in the output rather than a second `◆`, and the
/// header counts still sum to `total` with the blocked agent included.
///
/// The marker set is compared against `kitsu_render::deck_marker` rather
/// than against literal glyphs, so this test cannot drift from the
/// vocabulary it is checking.
#[test]
fn the_jobs_deck_distinguishes_a_blocked_row_from_the_running_rows() {
    let snapshot = subagent::AgentProgressSnapshot {
        generation: 1,
        total: 3,
        running: 1,
        done: 0,
        failed: 0,
        cancelled: 0,
        blocked: 2,
        max_running: 4,
        max_records: 32,
        available_running_slots: 3,
        available_record_slots: 29,
        agents: Vec::new(),
    };
    let counts = jobs_deck_counts(&snapshot);
    assert!(
            counts.contains("3 total \u{b7} 1 running \u{b7} 0 done \u{b7} 0 failed \u{b7} 0 cancelled \u{b7} 2 blocked"),
            "{counts}"
        );
    assert_eq!(
        snapshot.running + snapshot.done + snapshot.failed + snapshot.cancelled + snapshot.blocked,
        snapshot.total,
        "the per-state counts must still sum to total: {counts}"
    );

    let mut running_progress = blocked_progress(4);
    running_progress.status = subagent::AgentProgressStatus::Running;
    let blocked_row = agent_deck_row_block(&blocked_progress(3), false, 4, 8);
    let running_row = agent_deck_row_block(&running_progress, false, 4, 8);
    let blocked_marker = kitsu_render::deck_marker(kitsu_render::DeckState::Blocked);
    let running_marker = kitsu_render::deck_marker(kitsu_render::DeckState::Running);

    assert_eq!(
        blocked_row.lines().nth(1).unwrap(),
        format!("    {blocked_marker} #3 blocked | reviewer | summarize the crate")
    );
    assert_eq!(
        running_row.lines().nth(1).unwrap(),
        format!("    {running_marker} #4 running | reviewer | summarize the crate")
    );
}

/// The deck header counts gain a `blocked` term after `cancelled`, so the
/// per-state counts still sum to `total`. The existing count prefix is
/// unchanged, which is why the new term is appended rather than inserted.
#[test]
fn the_deck_counts_report_blocked_after_cancelled() {
    let snapshot = subagent::AgentProgressSnapshot {
        generation: 1,
        total: 5,
        running: 1,
        done: 1,
        failed: 1,
        cancelled: 1,
        blocked: 1,
        max_running: 4,
        max_records: 32,
        available_running_slots: 3,
        available_record_slots: 27,
        agents: Vec::new(),
    };
    let counts = jobs_deck_counts(&snapshot);
    assert!(
        counts.contains("5 total · 1 running · 1 done · 1 failed · 1 cancelled · 1 blocked"),
        "{counts}"
    );
}

/// B4: JSONL output is unchanged. `background_result` keeps exactly
/// `done|failed|cancelled|running`; a blocked agent is non-terminal, so it
/// reports as `running` and never adds a fifth status word. A later stage
/// that needs it must add a `background_blocked` event instead.
#[test]
fn a_blocked_agent_adds_no_jsonl_status_value() {
    let payload = background_result_payload(&subagent::AgentTerminalStatus {
        id: 4,
        task: "summarize the crate".to_string(),
        status: subagent::AgentStatus::Blocked,
        joined: false,
    });
    assert_eq!(payload["status"], "running");
    assert_eq!(payload["result"], "");
}

/// `/jobs explain` on a blocked agent: the phase line says `blocked`, and
/// because `Blocked` is non-terminal the explanation keeps the live
/// shape (`since: started …`, steering reported) rather than the
/// past-tense terminal shape.
#[test]
fn explain_reads_a_blocked_agent_as_live() {
    let (progress, thread) = explain_fixture(
        9,
        subagent::AgentProgressStatus::Blocked,
        subagent::thread::AgentThreadPhase::Blocked,
        subagent::thread::AgentHandoffState::Unavailable,
    );
    let explain = format_agent_explain(&progress, &thread);
    assert_no_debug_leak(&explain);
    let header = explain.lines().next().unwrap();
    assert!(header.ends_with("| blocked"), "{header}");

    let labels = explain_labels(&explain);
    assert_eq!(labels[0].0, "since");
    assert!(
        labels[0].1.starts_with("started"),
        "a blocked agent has not finished running: {explain}"
    );
    assert!(
        labels.iter().any(|(label, _)| label == "steering"),
        "steering stays answerable while blocked: {explain}"
    );
    assert!(
        !labels.iter().any(|(label, _)| label == "handoff"),
        "a non-terminal agent has no handoff line: {explain}"
    );
    let body = explain
        .strip_prefix(&kitsu_render::deck_marker(kitsu_render::DeckState::Blocked))
        .unwrap_or_else(|| panic!("no deck marker: {explain}"));
    assert!(body.is_ascii(), "{explain}");
}

/// `/jobs show` on a blocked thread prints the DTO's own spelling.
#[test]
fn the_thread_view_names_the_blocked_phase() {
    let (_progress, thread) = explain_fixture(
        9,
        subagent::AgentProgressStatus::Blocked,
        subagent::thread::AgentThreadPhase::Blocked,
        subagent::thread::AgentHandoffState::Unavailable,
    );
    let shown = format_agent_thread(&thread);
    assert_no_debug_leak(&shown);
    assert!(shown.contains("blocked"), "{shown}");
    assert!(
        shown.contains("handoff: unavailable"),
        "a blocked thread offers no result: {shown}"
    );
}

#[test]
fn jobs_view_moves_a_done_agent_from_result_ready_to_collected_on_handoff() {
    let registry = subagent::AgentRegistry::default();
    let done = registry.start("finished cleanly".to_string()).unwrap();
    registry.finish(done, Ok("the answer".to_string()));

    let before = format_jobs_view(&registry);
    assert!(
        jobs_view_heading_offset(&before, "done, result ready").is_some(),
        "{before}"
    );
    assert!(!before.contains("done, collected"), "{before}");

    // Peeking is not a handoff.
    assert!(registry.peek_result(done).is_some());
    let peeked = format_jobs_view(&registry);
    assert!(
        jobs_view_heading_offset(&peeked, "done, result ready").is_some(),
        "{peeked}"
    );

    assert!(matches!(
        registry.acknowledge_result(done),
        subagent::AgentHandoffAckOutcome::Acknowledged
    ));
    let after = format_jobs_view(&registry);
    assert!(
        jobs_view_heading_offset(&after, "done, collected").is_some(),
        "{after}"
    );
    assert!(!after.contains("done, result ready"), "{after}");
}

#[test]
fn jobs_view_group_order_is_independent_of_agent_creation_order() {
    let registry = subagent::AgentRegistry::default();
    // Ids ascend while states descend: the lowest id is the group printed
    // last, so any id-ordered implementation prints the groups backwards.
    let failed = registry.start("broke".to_string()).unwrap();
    let collected = registry.start("already picked up".to_string()).unwrap();
    let ready = registry.start("finished cleanly".to_string()).unwrap();
    let _running = registry.start("still working".to_string()).unwrap();
    registry.finish(failed, Err("provider request failed".to_string()));
    registry.finish(collected, Ok("result already read".to_string()));
    registry.finish(ready, Ok("result awaiting pickup".to_string()));
    assert!(registry.result(collected).is_some());

    let view = format_jobs_view(&registry);
    let offsets = [
        jobs_view_heading_offset(&view, "working"),
        jobs_view_heading_offset(&view, "done, result ready"),
        jobs_view_heading_offset(&view, "done, collected"),
        jobs_view_heading_offset(&view, "failed"),
    ]
    .map(|offset| offset.unwrap_or_else(|| panic!("{view}")));
    assert!(offsets.windows(2).all(|pair| pair[0] < pair[1]), "{view}");
}

#[test]
fn an_empty_jobs_view_says_the_same_thing_the_flat_deck_says() {
    let registry = subagent::AgentRegistry::default();
    let view = format_jobs_view(&registry);
    assert_no_debug_leak(&view);
    assert!(!view.contains('\u{1b}'), "{view}");
    assert!(
        view.contains("no background agents — ask the model to delegate work, or see /profiles"),
        "{view}"
    );
    assert!(view.contains("agent deck by state"), "{view}");
    if kitsu_render::plain_mode() {
        assert!(view.starts_with("# agent deck by state"), "{view}");
    } else {
        assert!(view.starts_with("◈ agent deck by state"), "{view}");
    }
    // No group heading can appear when there is nothing to group.
    for heading in [
        "needs input",
        "working",
        "done, result ready",
        "done, collected",
        "failed",
        "cancelled",
    ] {
        assert!(jobs_view_heading_offset(&view, heading).is_none(), "{view}");
    }
}

#[test]
#[serial]
fn the_jobs_view_header_reports_the_same_counts_as_the_flat_deck() {
    let registry = subagent::AgentRegistry::default();
    let _running = registry.start("still working".to_string()).unwrap();
    let done = registry.start("finished cleanly".to_string()).unwrap();
    let failed = registry.start("broke".to_string()).unwrap();
    registry.finish(done, Ok("the answer".to_string()));
    registry.finish(failed, Err("provider request failed".to_string()));

    let flat = format_jobs(&registry);
    let view = format_jobs_view(&registry);
    let counts = if kitsu_render::plain_mode() {
        "3 total | 1 running | 1 done | 1 failed | 0 cancelled"
    } else {
        "3 total · 1 running · 1 done · 1 failed · 0 cancelled"
    };
    assert!(flat.contains(counts), "{flat}");
    assert!(view.contains(counts), "{view}");
    // The header summary is the flat deck's summary with the grouped lead.
    let flat_header = flat.lines().next().unwrap();
    let view_header = view.lines().next().unwrap();
    let (view_lead, flat_lead) = if kitsu_render::plain_mode() {
        ("agent deck by state - ", "agent deck - ")
    } else {
        ("agent deck by state — ", "agent deck — ")
    };
    assert_eq!(
        view_header.replace(view_lead, flat_lead),
        flat_header,
        "{view_header} vs {flat_header}"
    );
}

#[test]
fn jobs_view_rows_carry_the_same_content_as_the_flat_deck_rows() {
    let registry = subagent::AgentRegistry::default();
    let done = registry.start("finished cleanly".to_string()).unwrap();
    registry.finish(done, Ok("the answer".to_string()));

    let flat = format_jobs(&registry);
    let view = format_jobs_view(&registry);
    // The detail continuation line (model, tokens, tools, writes, last
    // activity) must be identical once indentation is stripped: the two
    // views render the same row through one shared helper.
    assert_eq!(jobs_detail_line(&flat), jobs_detail_line(&view));

    // The row line itself is identical too, marker and label included.
    let row_of = |rendered: &str| -> String {
        rendered
            .lines()
            .find(|line| line.contains(&format!("#{done} ")))
            .unwrap_or_else(|| panic!("no agent row in: {rendered}"))
            .trim()
            .to_string()
    };
    assert_eq!(row_of(&flat), row_of(&view));

    // Grouped rows sit one level deeper than their heading.
    let row_line = view
        .lines()
        .find(|line| line.contains(&format!("#{done} ")))
        .unwrap_or_else(|| panic!("{view}"));
    assert!(row_line.starts_with("    "), "{row_line:?}");
    assert!(!row_line.starts_with("     "), "{row_line:?}");

    // Continuation lines keep the deck's +4 relationship to their row:
    // the flat deck is 2/6, the grouped view is 4/8.
    let detail_indent = |rendered: &str| -> usize {
        let line = rendered
            .lines()
            .find(|line| line.contains("| tokens "))
            .unwrap_or_else(|| panic!("{rendered}"));
        line.len() - line.trim_start().len()
    };
    assert_eq!(detail_indent(&flat), 6, "{flat}");
    assert_eq!(detail_indent(&view), 8, "{view}");
}

#[test]
fn jobs_view_running_rows_keep_the_activity_child_line() {
    let registry = subagent::AgentRegistry::default();
    let _running = registry.start("still working".to_string()).unwrap();
    let done = registry.start("finished cleanly".to_string()).unwrap();
    registry.finish(done, Ok("the answer".to_string()));

    let view = format_jobs_view(&registry);
    let activity = if kitsu_render::plain_mode() {
        "-> started | 0s ago"
    } else {
        "↳ started · 0s ago"
    };
    // Exactly the running row carries exactly one child, as in the deck.
    assert_eq!(view.matches(activity).count(), 1, "{view}");
    assert_eq!(
        format_jobs(&registry).matches(activity).count(),
        view.matches(activity).count(),
        "{view}"
    );
}

#[test]
fn jobs_view_group_headings_are_unglyphed_two_space_indented_text() {
    let registry = subagent::AgentRegistry::default();
    let _running = registry.start("still working".to_string()).unwrap();
    let done = registry.start("finished cleanly".to_string()).unwrap();
    registry.finish(done, Ok("the answer".to_string()));

    let view = format_jobs_view(&registry);
    for heading in ["working", "done, result ready"] {
        let line = view
            .lines()
            .find(|line| line.trim_start().starts_with(heading))
            .unwrap_or_else(|| panic!("{view}"));
        assert_eq!(line, format!("  {heading} (1)"), "{view}");
        // No deck glyph or ASCII marker leaks into a heading.
        for glyph in ["◆", "◈", "◇", "[run]", "[rdy]", "[err]", "#"] {
            assert!(!line.contains(glyph), "{line:?}");
        }
        // Headings bypass deck_header/sanitize_plain_inline, so they must
        // already be pure ASCII: --plain and NO_COLOR emit zero non-ASCII
        // bytes, and mode-dependent heading text would break this test.
        assert!(line.is_ascii(), "{line:?}");
    }
}

#[test]
#[serial]
fn the_flat_jobs_deck_keeps_its_shape_after_the_row_helper_is_shared() {
    let registry = subagent::AgentRegistry::default();
    let _running = registry.start("still working".to_string()).unwrap();
    let done = registry.start("finished cleanly".to_string()).unwrap();
    let failed = registry.start("broke".to_string()).unwrap();
    registry.finish(done, Ok("collected later".to_string()));
    registry.finish(failed, Err("provider request failed".to_string()));

    let flat = format_jobs(&registry);
    assert_no_debug_leak(&flat);
    assert!(!flat.contains('\u{1b}'), "{flat}");
    if kitsu_render::plain_mode() {
        assert!(flat.starts_with("# agent deck -"), "{flat}");
        assert!(flat.contains("[run] #"), "{flat}");
        assert!(flat.contains("[rdy] #"), "{flat}");
        assert!(flat.contains("[err] #"), "{flat}");
    } else {
        assert!(flat.starts_with("◈ agent deck —"), "{flat}");
        assert!(flat.contains("◆ #"), "{flat}");
        assert!(flat.contains("◈ #"), "{flat}");
        assert!(flat.contains("◇ #"), "{flat}");
    }
    assert!(flat.contains(&format!("/jobs show {done}")), "{flat}");
    // The flat deck stays flat: no group heading ever appears in it.
    assert!(!flat.contains("agent deck by state"), "{flat}");
    for heading in ["needs input", "working", "done, result ready"] {
        assert!(jobs_view_heading_offset(&flat, heading).is_none(), "{flat}");
    }
    // Rows stay at the flat deck's two-space indent.
    let row = flat
        .lines()
        .find(|line| line.contains(&format!("#{done} ")))
        .unwrap_or_else(|| panic!("{flat}"));
    assert!(row.starts_with("  ") && !row.starts_with("   "), "{row:?}");
}

#[test]
fn explaining_a_running_agent_says_since_when_and_what_it_waits_on() {
    let (progress, thread) = explain_fixture(
        3,
        subagent::AgentProgressStatus::Running,
        subagent::thread::AgentThreadPhase::Running,
        subagent::thread::AgentHandoffState::Unavailable,
    );
    let explain = format_agent_explain(&progress, &thread);
    assert_no_debug_leak(&explain);
    assert!(!explain.contains('\u{1b}'), "{explain}");

    let header = explain
        .lines()
        .next()
        .unwrap_or_else(|| panic!("{explain}"));
    assert!(header.contains("agent #3 explained"), "{header}");
    assert!(header.contains("running"), "{header}");
    if kitsu_render::plain_mode() {
        assert!(header.starts_with("[run] "), "{header}");
    } else {
        assert!(header.starts_with("◆ "), "{header}");
    }

    let labels = explain_labels(&explain);
    // Two ages, two deliberately different formats, each matching its own
    // established precedent. `since` is a run duration, which this file
    // renders at one decimal throughout (`format_agent_thread`). An
    // activity age is whole seconds, because `agent_activity_child_row`
    // routes the deck's own activity age through
    // `kitsu_render::format_wait_elapsed`, which truncates. Do not
    // "unify" these: that breaks one precedent whichever way it goes.
    let since = explain_label_value(&labels, "since").unwrap_or_else(|| panic!("{explain}"));
    assert_eq!(since, "started 12.5s ago", "{explain}");
    let waiting = explain_label_value(&labels, "waiting on").unwrap_or_else(|| panic!("{explain}"));
    assert!(waiting.contains("tool read"), "{explain}");
    assert!(
        waiting.contains(&format!(
            "{} ago",
            kitsu_render::format_wait_elapsed(std::time::Duration::from_millis(3_200))
        )),
        "{explain}"
    );
    assert!(!waiting.contains("3.2s"), "{explain}");
    // `show` says "queued" for this field; `explain` must not invent a
    // second word for one fact.
    assert_eq!(
        explain_label_value(&labels, "steering"),
        Some("open | 2 queued"),
        "{explain}"
    );
    // A running agent has not failed, has no collected-or-not result, and
    // its activity is the thing it is waiting on, not a past event.
    for absent in ["because", "last activity", "handoff"] {
        assert!(
            explain_label_value(&labels, absent).is_none(),
            "a running explanation must omit {absent}: {explain}"
        );
    }
}

#[test]
fn explaining_a_done_agent_with_an_uncollected_result_points_at_jobs_show() {
    let (progress, thread) = explain_fixture(
        4,
        subagent::AgentProgressStatus::Done,
        subagent::thread::AgentThreadPhase::Done,
        subagent::thread::AgentHandoffState::Ready,
    );
    let explain = format_agent_explain(&progress, &thread);
    assert_no_debug_leak(&explain);
    assert!(!explain.contains('\u{1b}'), "{explain}");

    let labels = explain_labels(&explain);
    assert_eq!(
        explain_label_value(&labels, "since"),
        Some("ran 12.5s"),
        "{explain}"
    );
    let handoff = explain_label_value(&labels, "handoff").unwrap_or_else(|| panic!("{explain}"));
    assert!(handoff.starts_with("ready |"), "{explain}");
    assert!(handoff.contains("/jobs show 4"), "{explain}");
    // Nothing is being waited on and nothing can be steered any more.
    for absent in ["waiting on", "steering"] {
        assert!(
            explain_label_value(&labels, absent).is_none(),
            "a terminal explanation must omit {absent}: {explain}"
        );
    }
}

#[test]
fn explaining_a_done_agent_whose_result_was_collected_says_acknowledged() {
    let (progress, thread) = explain_fixture(
        5,
        subagent::AgentProgressStatus::Done,
        subagent::thread::AgentThreadPhase::Done,
        subagent::thread::AgentHandoffState::Acknowledged,
    );
    let explain = format_agent_explain(&progress, &thread);
    let labels = explain_labels(&explain);
    let handoff = explain_label_value(&labels, "handoff").unwrap_or_else(|| panic!("{explain}"));
    assert!(handoff.starts_with("acknowledged |"), "{explain}");
    assert!(!handoff.contains("/jobs show"), "{explain}");
}

#[test]
fn explaining_a_failed_agent_names_the_failure_class_before_the_last_activity() {
    let (mut progress, thread) = explain_fixture(
        6,
        subagent::AgentProgressStatus::Failed,
        subagent::thread::AgentThreadPhase::Failed,
        subagent::thread::AgentHandoffState::Unavailable,
    );
    progress.telemetry.failure_class = Some(subagent::AgentFailureClass::ProviderRateLimited);
    let explain = format_agent_explain(&progress, &thread);
    assert_no_debug_leak(&explain);

    let labels = explain_labels(&explain);
    assert_eq!(
        explain_label_value(&labels, "because"),
        Some(
            subagent::AgentFailureClass::ProviderRateLimited
                .to_string()
                .as_str()
        ),
        "{explain}"
    );
    let activity =
        explain_label_value(&labels, "last activity").unwrap_or_else(|| panic!("{explain}"));
    assert!(activity.contains("tool read"), "{explain}");
    // Same shared formatter as the deck's activity age, not `since`'s
    // one-decimal run duration.
    assert!(
        activity.contains(&format!(
            "{} ago",
            kitsu_render::format_wait_elapsed(std::time::Duration::from_millis(3_200))
        )),
        "{explain}"
    );
    assert!(!activity.contains("3.2s"), "{explain}");

    // The cause is stated before the evidence, so the line order reads as
    // an explanation rather than a log.
    let because = explain
        .find("\n  because: ")
        .unwrap_or_else(|| panic!("{explain}"));
    let last = explain
        .find("\n  last activity: ")
        .unwrap_or_else(|| panic!("{explain}"));
    assert!(because < last, "{explain}");
}

#[test]
fn explaining_a_failed_agent_without_a_failure_class_omits_because() {
    let (progress, thread) = explain_fixture(
        7,
        subagent::AgentProgressStatus::Failed,
        subagent::thread::AgentThreadPhase::Failed,
        subagent::thread::AgentHandoffState::Unavailable,
    );
    assert_eq!(progress.telemetry.failure_class, None);
    let explain = format_agent_explain(&progress, &thread);
    let labels = explain_labels(&explain);
    assert!(
        explain_label_value(&labels, "because").is_none(),
        "{explain}"
    );
    // The rest of the explanation still renders.
    assert_eq!(
        explain_label_value(&labels, "since"),
        Some("ran 12.5s"),
        "{explain}"
    );
    assert!(
        explain_label_value(&labels, "last activity").is_some(),
        "{explain}"
    );
}

#[test]
fn explaining_a_running_agent_with_no_activity_says_so_in_words() {
    let (mut progress, thread) = explain_fixture(
        8,
        subagent::AgentProgressStatus::Running,
        subagent::thread::AgentThreadPhase::Running,
        subagent::thread::AgentHandoffState::Unavailable,
    );
    progress.telemetry.last_activity = None;
    progress.last_activity_ms_ago = None;
    let explain = format_agent_explain(&progress, &thread);
    assert!(
        explain.contains("\n  waiting on: no activity recorded yet"),
        "{explain}"
    );
    // No empty parenthetical age leaks out when there is no timestamp.
    assert!(!explain.contains("( ago)"), "{explain}");
    assert!(!explain.contains("(0s ago)"), "{explain}");
    assert!(!explain.contains("(0.0s ago)"), "{explain}");
}

#[test]
fn explain_labels_keep_their_documented_order_shape_and_ascii_tier() {
    const ORDER: [&str; 6] = [
        "since",
        "waiting on",
        "because",
        "last activity",
        "steering",
        "handoff",
    ];
    let cases = [
        (
            subagent::AgentProgressStatus::Running,
            subagent::thread::AgentThreadPhase::Running,
            subagent::thread::AgentHandoffState::Unavailable,
            Some(subagent::AgentFailureClass::ProviderTimeout),
        ),
        (
            subagent::AgentProgressStatus::Failed,
            subagent::thread::AgentThreadPhase::Failed,
            subagent::thread::AgentHandoffState::Unavailable,
            Some(subagent::AgentFailureClass::ProviderTimeout),
        ),
        (
            subagent::AgentProgressStatus::Done,
            subagent::thread::AgentThreadPhase::Done,
            subagent::thread::AgentHandoffState::Ready,
            None,
        ),
        (
            subagent::AgentProgressStatus::Cancelled,
            subagent::thread::AgentThreadPhase::Cancelled,
            subagent::thread::AgentHandoffState::Unavailable,
            None,
        ),
    ];
    for (index, (status, phase, handoff, class)) in cases.into_iter().enumerate() {
        let (mut progress, thread) = explain_fixture(9, status, phase, handoff);
        progress.telemetry.failure_class = class;
        let explain = format_agent_explain(&progress, &thread);
        assert_no_debug_leak(&explain);
        assert!(!explain.contains('\u{1b}'), "case {index}: {explain}");

        // `explain_labels` itself pins the `"  <label>: <value>"` shape.
        let labels = explain_labels(&explain);
        assert!(!labels.is_empty(), "case {index}: {explain}");
        let positions = labels
            .iter()
            .map(|(label, _)| {
                ORDER
                    .iter()
                    .position(|known| known == label)
                    .unwrap_or_else(|| panic!("case {index}: unknown label {label:?}"))
            })
            .collect::<Vec<_>>();
        assert!(
            positions.windows(2).all(|pair| pair[0] < pair[1]),
            "case {index}: labels out of order: {explain}"
        );
        // `since` is unconditional.
        assert_eq!(labels[0].0, "since", "case {index}: {explain}");

        // The explanation does not pass through `sanitize_plain_inline`, so
        // every byte it adds to the state marker must already be ASCII and
        // every separator must be `|`.
        let marker = kitsu_render::deck_marker(match phase {
            subagent::thread::AgentThreadPhase::Running => kitsu_render::DeckState::Running,
            subagent::thread::AgentThreadPhase::Blocked => kitsu_render::DeckState::Blocked,
            subagent::thread::AgentThreadPhase::Done
                if handoff == subagent::thread::AgentHandoffState::Ready =>
            {
                kitsu_render::DeckState::ResultReady
            }
            subagent::thread::AgentThreadPhase::Done => kitsu_render::DeckState::Stopped,
            subagent::thread::AgentThreadPhase::Failed => kitsu_render::DeckState::Failed,
            subagent::thread::AgentThreadPhase::Cancelled => kitsu_render::DeckState::Stopped,
        });
        let body = explain
            .strip_prefix(&marker)
            .unwrap_or_else(|| panic!("case {index}: no deck marker: {explain}"));
        assert!(body.is_ascii(), "case {index}: {explain}");
        for forbidden in ["·", "—", "↳", "\u{2502}"] {
            assert!(!explain.contains(forbidden), "case {index}: {explain}");
        }
    }
}

#[test]
fn explain_answers_a_different_question_than_show() {
    let (progress, thread) = explain_fixture(
        10,
        subagent::AgentProgressStatus::Done,
        subagent::thread::AgentThreadPhase::Done,
        subagent::thread::AgentHandoffState::Ready,
    );
    let explain = format_agent_explain(&progress, &thread);
    let show = format_agent_thread(&thread);

    // `show` dumps the whole bounded timeline and the task text; `explain`
    // answers why/since-when/waiting-on and dumps nothing.
    assert!(show.contains("bounded redacted timeline:"), "{show}");
    assert!(!explain.contains("bounded redacted timeline"), "{explain}");
    assert!(!explain.contains("timeline:"), "{explain}");
    assert!(show.contains("\n  task: "), "{show}");
    assert!(!explain.contains("\n  task: "), "{explain}");
    assert!(!explain.contains("summarize the crate"), "{explain}");

    // And it says, in the header, that it is an explanation.
    assert!(explain.contains("explained"), "{explain}");
    assert!(!show.contains("explained"), "{show}");
}

#[test]
fn explaining_an_agent_never_collects_its_result() {
    let registry = subagent::AgentRegistry::default();
    let done = registry.start("finished cleanly".to_string()).unwrap();
    registry.finish(done, Ok("the answer".to_string()));
    assert_eq!(
        registry.handoff_state(done),
        Some(subagent::thread::AgentHandoffState::Ready)
    );
    let before = registry.snapshot().unacknowledged;
    assert_eq!(before, 1);

    let explain =
        agent_explain_view(&registry, done).unwrap_or_else(|| panic!("agent #{done} exists"));
    assert!(
        explain.contains(&format!("agent #{done} explained")),
        "{explain}"
    );

    // Diagnosing an agent must never be the act that takes custody of its
    // result: `/jobs show` acknowledges, and if `explain` did too the exit
    // guard that refuses to quit on unviewed results would be defeated.
    assert_eq!(
        registry.handoff_state(done),
        Some(subagent::thread::AgentHandoffState::Ready),
        "explain acknowledged the handoff: {explain}"
    );
    assert_eq!(registry.snapshot().unacknowledged, before, "{explain}");
    let view = format_jobs_view(&registry);
    assert!(
        jobs_view_heading_offset(&view, "done, result ready").is_some(),
        "{view}"
    );
    assert!(!view.contains("done, collected"), "{view}");
}

#[test]
fn explaining_an_unknown_agent_id_has_nothing_to_render() {
    let registry = subagent::AgentRegistry::default();
    assert!(agent_explain_view(&registry, 404).is_none());
    let known = registry.start("still working".to_string()).unwrap();
    assert!(agent_explain_view(&registry, known).is_some());
    assert!(agent_explain_view(&registry, known + 1).is_none());
}

#[test]
fn the_steering_receipt_says_the_worker_restarts() {
    let receipt = subagent::thread::AgentSteeringReceipt {
        id: 3,
        sequence: 2,
        pending: 1,
        discarded_ms: 72_000,
    };
    let rendered = format_steering_receipt(&receipt);
    assert!(
        rendered.contains("restarting background agent #3"),
        "{rendered}"
    );
    assert!(
        rendered.contains("72.0s of in-flight progress discarded"),
        "{rendered}"
    );
    assert!(
        rendered.contains("completed file changes kept"),
        "{rendered}"
    );
    assert!(!rendered.contains("queued for"), "{rendered}");
}

#[test]
fn coalesced_background_completion_still_has_a_notice() {
    let previous = subagent::AgentRegistrySnapshot {
        generation: 1,
        running: 0,
        terminal: 0,
        failed: 0,
        unacknowledged: 0,
        blocked: 0,
    };
    let current = subagent::AgentRegistrySnapshot {
        generation: 3,
        running: 0,
        terminal: 1,
        failed: 0,
        unacknowledged: 0,
        blocked: 0,
    };
    let notice = background_agent_notice(previous, current).unwrap();
    assert!(notice.contains("1 background agent(s) finished"));
    assert_eq!(
        background_agent_notice_level(previous, current),
        kitsu_render::AsyncNoticeLevel::Info
    );
}

#[test]
fn a_detached_failure_is_announced_as_a_failure_not_as_completion() {
    let previous = subagent::AgentRegistrySnapshot {
        generation: 1,
        running: 1,
        terminal: 0,
        failed: 0,
        unacknowledged: 0,
        blocked: 0,
    };
    let current = subagent::AgentRegistrySnapshot {
        generation: 2,
        running: 0,
        terminal: 1,
        failed: 1,
        unacknowledged: 0,
        blocked: 0,
    };
    let notice = background_agent_notice(previous, current).unwrap();
    assert!(notice.contains("FAILED"), "{notice}");
    assert!(!notice.contains("finished"), "{notice}");
    assert_eq!(
        background_agent_notice_level(previous, current),
        kitsu_render::AsyncNoticeLevel::Error
    );
}

// --- site 1: the idle dispatcher, and its JSON payload ---

/// The idle path releases the agent too, and its JSON payload matches the
/// shape `stop` emits: `kind`/`action`/`changed`/`snapshot`, plus the
/// `id`/`outcome` pair `close-steering` uses to report an outcome enum.
#[tokio::test]
#[cfg(unix)]
async fn idle_reply_releases_the_agent_and_emits_a_stop_shaped_payload() {
    let (registry, id, answer) = registry_with_one_blocked_agent().await;
    let journal = kitsu_tools::builtins::fs::WriteJournal::new();

    let payload = handle_jobs_command_attaching(
        &registry,
        &journal,
        &format!("reply {id} a"),
        &mut None,
        None,
    )
    .await
    .expect("replying to a blocked agent succeeds");

    assert_eq!(payload["kind"], "jobs");
    assert_eq!(payload["action"], "reply");
    assert_eq!(payload["changed"], true);
    assert_eq!(payload["id"], id);
    assert_eq!(payload["outcome"], "answered");
    assert_eq!(payload["decision"], "allow_always");
    assert!(
        payload.get("snapshot").is_some(),
        "every /jobs payload carries the deck snapshot: {payload}"
    );
    assert_eq!(
        answer.await.expect("the gate's oneshot is resolved"),
        Decision::AllowAlways
    );
}

/// The two no-op outcomes on the idle path. `NotBlocked` is a reported
/// no-op (`changed: false`), exactly as `stop` reports an already-terminal
/// job; an unknown id is the usual `Err(())` rejection.
#[tokio::test]
#[cfg(unix)]
async fn idle_reply_reports_not_waiting_as_an_unchanged_payload_and_unknown_as_an_error() {
    let registry = subagent::AgentRegistry::new();
    let running = registry.start("busy, not parked".to_string()).unwrap();
    let journal = kitsu_tools::builtins::fs::WriteJournal::new();

    let payload = handle_jobs_command_attaching(
        &registry,
        &journal,
        &format!("reply {running} y"),
        &mut None,
        None,
    )
    .await
    .expect("a reported no-op is not a failure");
    assert_eq!(payload["action"], "reply");
    assert_eq!(payload["changed"], false);
    assert_eq!(payload["outcome"], "not_blocked");

    assert_eq!(
        handle_jobs_command_attaching(&registry, &journal, "reply 9999 y", &mut None, None).await,
        Err(()),
        "an unknown id is rejected"
    );
    assert_eq!(
        handle_jobs_command_attaching(&registry, &journal, "reply", &mut None, None).await,
        Err(()),
        "a missing id is a usage error"
    );
    assert_eq!(
        handle_jobs_command_attaching(
            &registry,
            &journal,
            &format!("reply {running} maybe"),
            &mut None,
            None
        )
        .await,
        Err(()),
        "an unknown key is a usage error"
    );
}

/// Card **F67-print**, the half a print must not buy: `/jobs prune <id>`
/// still **removes the record**.
///
/// F67's decision is "print the result, then prune" -- option two, not
/// option three. The `x` key refuses, because *the second press of an
/// arming is not an explicit id*; a typed id **is** the confirmation, and
/// that asymmetry is the finding's own conclusion. So an implementation
/// that made this command refuse an unviewed result would delete the
/// finding, and this test is what fails when it does.
///
/// It also pins what automation receives, which is the measured baseline
/// the interactive print is matched against: `terminal_job_payload` --
/// `id`, `task`, the status label, `joined` -- unchanged by the print.
#[tokio::test]
#[cfg(unix)]
async fn idle_prune_of_an_unviewed_result_still_removes_the_record() {
    let registry = std::sync::Arc::new(subagent::AgentRegistry::new());
    let journal = kitsu_tools::builtins::fs::WriteJournal::new();
    let id = registry.start("nobody read this one".to_string()).unwrap();
    registry.finish(id, Ok("the lost result".to_string()));
    assert_eq!(
        registry.handoff_state(id),
        Some(subagent::thread::AgentHandoffState::Ready),
        "NON-VACUOUS: the result really is unviewed when the prune is typed"
    );

    let payload =
        handle_jobs_command_attaching(&registry, &journal, &format!("prune {id}"), &mut None, None)
            .await
            .expect("a typed id prunes");

    assert_eq!(payload["action"], "prune");
    assert_eq!(
        payload["changed"], true,
        "a typed id is the confirmation (F67): this command prunes, it \
             does not refuse like `x`: {payload}"
    );
    assert!(
        registry.thread_snapshot(id).is_none(),
        "the record is gone, which is what `AgentRegistry::prune` is \
             for and what printing must not have replaced: {payload}"
    );
    assert_eq!(
        payload["job"]["id"], id,
        "automation's payload is unchanged: {payload}"
    );
    assert_eq!(payload["job"]["task"], "nobody read this one");
    assert_eq!(payload["job"]["status"], "done");
    assert_eq!(payload["job"]["joined"], true);
    assert!(
        registry.take_unobserved_terminal_results().is_empty(),
        "and nothing is left for the exit guard to surface -- which is \
             exactly why the result had to be printed here"
    );
}

// ------------------------------------------------------------------
// T2-S6: the approval record in the two inspection surfaces.
// `record_approval_request`/`record_approval_outcome` are STUBS, so
// every test below this banner is expected to fail.
// ------------------------------------------------------------------

/// `/jobs show` renders the approval entries in timeline order, and the
/// path the gate actually parked (`/work/notes.txt`, `/work`) does not
/// appear. Driven through the real registry rather than a hand-built
/// snapshot: `format_agent_thread` prints any entry generically, so a
/// fixture-built Approval entry would prove nothing about whether one is
/// ever recorded.
#[cfg(unix)]
#[tokio::test]
async fn jobs_show_lists_the_approval_request_and_its_decision_in_order() {
    let (registry, id, answer) = registry_with_one_blocked_agent().await;

    let blocked = format_agent_thread(&registry.thread_snapshot(id).unwrap());
    assert_no_debug_leak(&blocked);
    assert!(
        blocked.contains("approval"),
        "the timeline names the approval entry's kind: {blocked}"
    );
    assert!(
        blocked.contains("write a file"),
        "/jobs show says what the agent asked for: {blocked}"
    );
    // NON-VACUOUS: `format_agent_thread` prints every entry's text
    // verbatim through `sanitize_terminal_text`, which does not touch
    // paths (see `redaction_does_not_remove_a_path_or_an_argv_word` in
    // `subagent/thread.rs`). Any implementation that formatted the
    // request's arguments into the entry would print them here.
    for argument in ["notes.txt", "/work"] {
        assert!(
            !blocked.contains(argument),
            "/jobs show leaked a tool argument ({argument}): {blocked}"
        );
    }

    assert_eq!(
        registry.answer(id, Decision::Deny),
        subagent::AgentAnswerOutcome::Answered
    );
    assert_eq!(answer.await.expect("the gate is resolved"), Decision::Deny);

    let decided = format_agent_thread(&registry.thread_snapshot(id).unwrap());
    assert_no_debug_leak(&decided);
    let request_at = decided
        .find("write a file")
        .unwrap_or_else(|| panic!("no request entry: {decided}"));
    let decision_at = decided
        .find("denied")
        .unwrap_or_else(|| panic!("no decision entry: {decided}"));
    assert!(
        request_at < decision_at,
        "the request is shown before its decision: {decided}"
    );
    for argument in ["notes.txt", "/work"] {
        assert!(
            !decided.contains(argument),
            "/jobs show leaked a tool argument ({argument}): {decided}"
        );
    }
}

/// `/jobs explain` answers "why is this agent in this state". For a
/// blocked agent that is "it is waiting for permission to X", so the
/// `waiting on` line must say so instead of reporting the last tool
/// activity.
#[test]
fn explaining_a_blocked_agent_says_what_permission_it_waits_for() {
    let (progress, mut thread) = explain_fixture(
        9,
        subagent::AgentProgressStatus::Blocked,
        subagent::thread::AgentThreadPhase::Blocked,
        subagent::thread::AgentHandoffState::Unavailable,
    );
    thread
        .timeline
        .entries
        .push(subagent::thread::AgentTimelineEntry {
            sequence: 2,
            elapsed_ms: 20,
            kind: subagent::thread::AgentTimelineKind::Approval,
            round: None,
            text: format!(
                "permission to {} requested",
                subagent::thread::AgentApprovalKind::WriteFile.label()
            ),
            truncated: false,
        });
    thread.timeline.retained_bytes += thread.timeline.entries[1].text.len();

    let explain = format_agent_explain(&progress, &thread);
    assert_no_debug_leak(&explain);
    let labels = explain_labels(&explain);
    let waiting = explain_label_value(&labels, "waiting on")
        .unwrap_or_else(|| panic!("a blocked agent is waiting on something: {explain}"));
    assert!(
        waiting.contains("permission to write a file"),
        "explain names the permission, not the last tool: {explain}"
    );
    let body = explain
        .strip_prefix(&kitsu_render::deck_marker(kitsu_render::DeckState::Blocked))
        .unwrap_or_else(|| panic!("no deck marker: {explain}"));
    assert!(body.is_ascii(), "{explain}");
}

/// Once the request is decided the agent is no longer waiting on it, so
/// `explain` must not keep claiming it is. Pins that the outcome entry
/// closes the pending request rather than the request entry alone
/// driving the line.
#[test]
fn explaining_a_resumed_agent_no_longer_claims_to_wait_for_permission() {
    let (progress, mut thread) = explain_fixture(
        9,
        subagent::AgentProgressStatus::Running,
        subagent::thread::AgentThreadPhase::Running,
        subagent::thread::AgentHandoffState::Unavailable,
    );
    for (sequence, text) in [
        (2u64, "permission to write a file requested".to_string()),
        (3, "permission to write a file allowed once".to_string()),
    ] {
        thread.timeline.retained_bytes += text.len();
        thread
            .timeline
            .entries
            .push(subagent::thread::AgentTimelineEntry {
                sequence,
                elapsed_ms: 20,
                kind: subagent::thread::AgentTimelineKind::Approval,
                round: None,
                text,
                truncated: false,
            });
    }

    let explain = format_agent_explain(&progress, &thread);
    let labels = explain_labels(&explain);
    let waiting = explain_label_value(&labels, "waiting on")
        .unwrap_or_else(|| panic!("a running agent still has a waiting-on line: {explain}"));
    // NON-VACUOUS: the sibling test above requires this exact string to
    // be printed from an Approval entry, and this thread's timeline
    // carries that very entry. Only the decision distinguishes them.
    assert!(
        !waiting.contains("permission to"),
        "an answered agent is not waiting for permission any more: {explain}"
    );
}

// ------------------------------------------------------------------
// F43: the two inspection surfaces must not call a cancellation a
// timeout. STUB: `AgentApprovalOutcome::Cancelled` does not exist, so
// the second test fails to COMPILE; the first fails on content.
// ------------------------------------------------------------------

/// `/jobs show` for a cancelled park and for a timed-out park, side by
/// side in one test so the absence assertion cannot be vacuous: the
/// timed-out agent proves this very surface does print "timed out" from
/// an approval entry, and the cancelled agent must not.
#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn jobs_show_distinguishes_a_cancelled_approval_from_a_timed_out_one() {
    let registry = subagent::AgentRegistry::new();
    let cancelled_id = registry
        .start("cancelled while blocked".to_string())
        .unwrap();
    let timed_out_id = registry.start("never answered".to_string()).unwrap();
    let sender = registry.prompt_sender();
    let cancelled_answer = park_a_path_request(&sender, cancelled_id);
    let timed_out_answer = park_a_path_request(&sender, timed_out_id);
    for id in [cancelled_id, timed_out_id] {
        for _ in 0..1_000 {
            let blocked = registry
                .progress_snapshot()
                .agents
                .into_iter()
                .any(|agent| {
                    agent.id == id && agent.status == subagent::AgentProgressStatus::Blocked
                });
            if blocked {
                break;
            }
            tokio::task::yield_now().await;
        }
    }

    assert!(matches!(
        registry.cancel(cancelled_id).await,
        subagent::AgentCancelOutcome::Cancelled(_)
    ));
    assert_eq!(
        cancelled_answer.await.expect("the gate is resolved"),
        Decision::Interrupt
    );
    tokio::time::advance(subagent::DEFAULT_AGENT_BLOCK_TIMEOUT + std::time::Duration::from_secs(1))
        .await;
    assert_eq!(
        timed_out_answer
            .await
            .expect("the timeout resolves the gate"),
        Decision::Interrupt
    );

    let timed_out = format_agent_thread(&registry.thread_snapshot(timed_out_id).unwrap());
    assert_no_debug_leak(&timed_out);
    // NON-VACUITY: this surface prints an outcome's words verbatim.
    assert!(
        timed_out.contains("timed out"),
        "/jobs show prints a timeout outcome from an approval entry: {timed_out}"
    );

    let cancelled = format_agent_thread(&registry.thread_snapshot(cancelled_id).unwrap());
    assert_no_debug_leak(&cancelled);
    assert!(
        cancelled.contains("write a file cancelled"),
        "/jobs show says the approval was cancelled: {cancelled}"
    );
    assert!(
        !cancelled.contains("timed out"),
        "a deliberate cancellation is not reported as an unattended \
             timeout: {cancelled}"
    );
    for argument in ["notes.txt", "/work"] {
        assert!(
            !cancelled.contains(argument),
            "/jobs show leaked a tool argument ({argument}): {cancelled}"
        );
    }
}

/// `/jobs explain` and the cancelled outcome. A GUARD, green today and
/// required to stay green: `pending_approval_request` keys on the
/// `" requested"` suffix, so the new `cancelled` outcome must close the
/// pending request exactly as `allowed once` does, and an implementer
/// who spells the entry differently (or records the cancellation as a
/// second *request*) would break this line.
///
/// The phase is `Running` on purpose: `cancel_matching` calls
/// `thread.unblock()` before the terminal transition, so a thread that
/// is `Running` with a cancelled approval as its newest entry is a
/// state the cancel path really passes through.
///
/// NON-VACUITY is proved inside this test: the first half builds the
/// same thread WITHOUT the outcome entry and shows this surface does
/// print "waiting on: permission to write a file" from it.
#[test]
fn explain_does_not_treat_a_cancelled_approval_as_a_pending_request() {
    fn fixture_with(
        texts: &[String],
    ) -> (
        subagent::AgentProgress,
        subagent::thread::AgentThreadSnapshot,
    ) {
        let (progress, mut thread) = explain_fixture(
            9,
            subagent::AgentProgressStatus::Running,
            subagent::thread::AgentThreadPhase::Running,
            subagent::thread::AgentHandoffState::Unavailable,
        );
        for (offset, text) in texts.iter().enumerate() {
            thread.timeline.retained_bytes += text.len();
            thread
                .timeline
                .entries
                .push(subagent::thread::AgentTimelineEntry {
                    sequence: 2 + offset as u64,
                    elapsed_ms: 20,
                    kind: subagent::thread::AgentTimelineKind::Approval,
                    round: None,
                    text: text.clone(),
                    truncated: false,
                });
        }
        (progress, thread)
    }

    let requested = format!(
        "approval to {} requested",
        subagent::thread::AgentApprovalKind::WriteFile.label()
    );
    let cancelled = format!(
        "approval to {} {}",
        subagent::thread::AgentApprovalKind::WriteFile.label(),
        subagent::thread::AgentApprovalOutcome::Cancelled.label()
    );

    let (progress, thread) = fixture_with(std::slice::from_ref(&requested));
    let pending = format_agent_explain(&progress, &thread);
    let pending_labels = explain_labels(&pending);
    let pending_waiting = explain_label_value(&pending_labels, "waiting on")
        .unwrap_or_else(|| panic!("a pending request has a waiting-on line: {pending}"));
    assert_eq!(
            pending_waiting, "permission to write a file",
            "NON-VACUITY: this surface prints exactly this from a request              entry, so the absence asserted below is a real claim"
        );

    let (progress, thread) = fixture_with(&[requested, cancelled]);
    let decided = format_agent_explain(&progress, &thread);
    assert_no_debug_leak(&decided);
    let decided_labels = explain_labels(&decided);
    let decided_waiting = explain_label_value(&decided_labels, "waiting on")
        .unwrap_or_else(|| panic!("the waiting-on line is still printed: {decided}"));
    assert!(
        !decided_waiting.contains("permission to"),
        "a cancelled approval is decided, not pending: {decided}"
    );
}
