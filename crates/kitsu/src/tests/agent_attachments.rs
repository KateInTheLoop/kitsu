use super::*;

#[test]
fn cycle_target_clamps_and_starts_from_detached() {
    let order = [10u64, 20, 30];
    // Forward/back from the middle.
    assert_eq!(cycle_target(&order, Some(20), 1), Some(30));
    assert_eq!(cycle_target(&order, Some(20), -1), Some(10));
    // Clamp at both ends — no wrap.
    assert_eq!(cycle_target(&order, Some(30), 1), Some(30));
    assert_eq!(cycle_target(&order, Some(10), -1), Some(10));
    // Detached: forward lands on the first agent, backward stays detached.
    assert_eq!(cycle_target(&order, None, 1), Some(10));
    assert_eq!(cycle_target(&order, None, -1), None);
    // An attachment no longer in the order falls to the first; empty order is inert.
    assert_eq!(cycle_target(&order, Some(99), 1), Some(10));
    assert_eq!(cycle_target(&[], Some(10), 1), None);
}

/// Requirement **A7**: **attaching a terminal agent takes custody of its
/// result**, exactly as peek does.
///
/// A7 names attach *beside* peek — "viewing a done agent (peek or
/// attach)" — and the reason is the same one T4-S3 pinned for peek:
/// attach is how the user reads that result, and a result the user has
/// read must not be reprinted by the exit surfacing. The acknowledgement
/// must go through the **one** acknowledging path T4-S3 left behind
/// (`peek_result` then `acknowledge_result`); a second `acknowledge_result`
/// call site is the shape F45 was closed by retiring.
///
/// Two non-vacuous halves, in both directions, as T4-S4a pinned the
/// selection's: a **running** agent has no handoff to take, and the agent
/// that was not attached to keeps its result unobserved.
#[cfg(unix)]
#[tokio::test]
async fn attaching_a_done_agent_takes_custody_of_its_result() {
    let (registry, ids) = registry_with_three_agents();
    registry.finish(ids[0], Ok("the attached answer".to_string()));
    registry.finish(ids[2], Ok("the untouched answer".to_string()));
    assert_eq!(
        registry.snapshot().unacknowledged,
        2,
        "NON-VACUOUS: both results start out unobserved"
    );

    // A2's order puts the one still-running agent first and the two done
    // rows after it, so the keystrokes below address a known agent. Taken
    // through `group_rows`, the view's own grouping, because
    // `overlay_visible_rows` yields the **registry's** order and the
    // selection walks the **displayed** one.
    let displayed: Vec<u64> =
        kitsu_tui::agents_view::group_rows(overlay_visible_rows(&registry, &OverlayState::new()))
            .into_iter()
            .flat_map(|group| group.rows.into_iter().map(|row| row.id))
            .collect();
    assert_eq!(
        displayed,
        vec![ids[1], ids[0], ids[2]],
        "A2: `working` outranks `done (unviewed)`, and the done rows keep \
             their start order"
    );

    // The running agent first: attaching it acknowledges nothing, because
    // there is nothing to take custody of.
    let mut running = OverlayUnderTest::new();
    running.feed(&registry, b"\r");
    assert_eq!(
        running.state.attaching,
        Some(ids[1]),
        "the running agent is the one attached here"
    );
    assert_eq!(
        registry.handoff_state(ids[1]),
        Some(subagent::thread::AgentHandoffState::Unavailable),
        "A7: a running agent has no handoff, so attaching takes nothing"
    );
    assert_eq!(
        registry.snapshot().unacknowledged,
        2,
        "NON-VACUOUS: and the two results are still unobserved"
    );

    // Now the done row under it.
    let mut done = OverlayUnderTest::new();
    done.feed(&registry, b"\x1b[B");
    assert_eq!(
        done.selected(&registry),
        Some(ids[0]),
        "one `\u{2193}` selects the first done row"
    );
    done.feed(&registry, b"\r");
    assert_eq!(
        registry.handoff_state(ids[0]),
        Some(subagent::thread::AgentHandoffState::Acknowledged),
        "A7: attaching a done agent acknowledges its handoff, exactly as \
             peeking it does"
    );
    assert_eq!(
        registry.handoff_state(ids[2]),
        Some(subagent::thread::AgentHandoffState::Ready),
        "attaching one row must not acknowledge another"
    );
}

/// **Attach with nothing selected says so, on the frame.**
///
/// Asserted through [`overlay_frame_rows`] rather than from
/// `state.report`, because the claim is that a user can *see* it
/// (**F71**), and a key that acts silently is indistinguishable from one
/// that failed (requirement T2-S5b).
#[cfg(unix)]
#[tokio::test]
async fn attaching_with_nothing_selected_says_so_on_the_frame() {
    let registry = subagent::AgentRegistry::new();
    let mut overlay = OverlayUnderTest::new();
    assert_eq!(
        overlay.selected(&registry),
        None,
        "NON-VACUOUS: an empty registry selects nothing"
    );

    let frame = overlay.feed_and_frame(&registry, b"\r");
    assert!(
        frame
            .iter()
            .any(|row| row.trim() == ATTACH_WITHOUT_SELECTION),
        "`Enter` with nothing selected must say so on the frame, but the \
             frame reads:\n{}",
        frame.join("\n")
    );
    assert_eq!(overlay.state.attaching, None, "and nothing is attached");

    // NON-VACUOUS, the other direction: with an agent on screen the same
    // key attaches and the row is gone.
    let (registry, ids) = registry_with_three_agents();
    let mut overlay = OverlayUnderTest::new();
    let frame = overlay.feed_and_frame(&registry, b"\r");
    assert_eq!(
        overlay.state.attaching,
        Some(ids[0]),
        "with a row selected the same key attaches"
    );
    assert!(
        !frame
            .iter()
            .any(|row| row.trim() == ATTACH_WITHOUT_SELECTION),
        "and the refusal is not painted: {}",
        frame.join("\n")
    );
}

/// Finding **F75**: the two halves of A3/A5 **connected**. The agent the
/// view attached to reaches the composer's session binding, the prompt
/// label says so, and the `\u{2190}`/`Ctrl+X d` detach routes are live
/// because of it.
///
/// Every other test on this card drives one half: the overlay's own state,
/// or the reader with an attachment handed to it. Both halves passed while
/// production carried nothing between them — the attach arm wrote
/// `OverlayState::attaching` and `read_chat_line_inline` hardcoded `None`,
/// so `Enter` closed the view and attached nothing and neither detach key
/// could ever fire. This test is the one that fails when they are
/// disconnected: it goes **through** the production carriers
/// ([`AgentViewExit::leaving`], which is what `run_agent_view_overlay`
/// returns, and [`apply_agent_view_exit`], which is what the inline
/// reader's `OpenAgentView` arm calls) rather than asserting either side
/// of the gap.
///
/// The terminal is the one thing not traversed: the overlay's byte loop
/// and the composer's painter both need a tty, so the bytes go through the
/// loop's own dispatch (`overlay_feed_chunk`, **F69**) and the reader's own
/// key routes, with nothing between them but the two carriers production
/// uses.
#[cfg(unix)]
#[tokio::test]
// **F107**: not optional. `#[serial]` excludes other `#[serial]` tests, not
// plain ones, and this test reaches the process-global surface slot through
// PRODUCTION code rather than `SurfaceProbe`, so it can install over a
// serialised test's probe mid-run. Measured: 3 failures in 421 unguarded
// runs of `--bins`, 0 in 400 with this guard.
#[serial_test::serial]
async fn the_views_attach_reaches_the_composer_and_makes_the_detach_keys_live() {
    /// One read of the inline composer, with `[tui]` resolved the way a
    /// configured session resolves it: the chord prefix bound and the
    /// two-press `\u{2190}` route on, so the detach routes are asserted
    /// against the keymap a user actually has.
    fn one_read(attached: Option<u64>, bytes: &[u8]) -> tui_bridge::InlineRead {
        let keys = tui_bridge::InlineKeys {
            prefix: tui_bridge::chord_prefix_byte("ctrl+x"),
            left_arrow_opens_agents: true,
        };
        assert!(keys.prefix.is_some(), "the chord prefix must be bound");
        let mut input = std::io::Cursor::new(bytes.to_vec());
        let mut paint = |_: &str, _: usize, _: &str, _: bool| {};
        tui_bridge::read_line_inline_with_keys(
            &mut input,
            &mut paint,
            &[],
            &mut || {},
            keys,
            attached,
            None,
            None,
        )
        .expect("a cursor cannot fail to read")
    }

    const BASE: &str = "kitsu> ";
    const LEFT: &[u8] = b"\x1b[D";
    const CHORD_D: &[u8] = b"\x18d";

    let (registry, ids) = registry_with_three_agents();
    // The session binding, exactly as the composer loop starts it.
    let mut attached: Option<u64> = None;

    // NON-VACUOUS, and the shape of the bug: with nothing carried across
    // -- which is what production did -- the composer wears its own
    // prompt and neither detach key can fire.
    assert_eq!(
        tui_bridge::composer_prompt(BASE, attached),
        BASE,
        "detached, the composer wears the session's own prompt"
    );
    assert!(
        !matches!(
            one_read(attached, LEFT),
            tui_bridge::InlineRead::Detached(_)
        ),
        "and `\u{2190}` cannot detach with nothing attached"
    );
    assert!(
        !matches!(
            one_read(attached, CHORD_D),
            tui_bridge::InlineRead::Detached(_)
        ),
        "nor can `Ctrl+X d`, which is inert and silent there"
    );

    // The view attaches, and closes doing it.
    let mut overlay = OverlayUnderTest::new();
    assert!(
        overlay.feed_closing(&registry, b"\r"),
        "A3: `Enter` leaves the view"
    );
    // The two carriers production uses, in production's order: the exit
    // `run_agent_view_overlay` returns, and the composer's application of
    // it. Nothing reads `OverlayState` after this point, because by then
    // the view that owned it is gone.
    let exit = AgentViewExit::leaving(&overlay.state);
    assert!(exit.shown, "the view ran");
    apply_agent_view_exit(&exit, &mut attached);
    assert_eq!(
        attached,
        Some(ids[0]),
        "F75: the agent the view attached to must reach the composer's \
             binding, which is the only home that outlives the overlay"
    );

    // What the user sees, and what the keys now do.
    assert_eq!(
        tui_bridge::composer_prompt(BASE, attached),
        format!("agent {}> ", ids[0]),
        "the prompt label names the attached agent (design \u{00a7}4.5)"
    );
    assert_eq!(
        one_read(attached, LEFT),
        tui_bridge::InlineRead::Detached(String::new()),
        "A5: `\u{2190}` on an empty composer detaches -- on the FIRST \
             press, before the two-press agent-view route is consulted"
    );
    assert_eq!(
        one_read(attached, CHORD_D),
        tui_bridge::InlineRead::Detached(String::new()),
        "and so does `Ctrl+X d`"
    );
    // A non-empty draft keeps `\u{2190}` as cursor movement, attached or
    // not: the overload is safe only while there is nothing to move over.
    assert_eq!(
        one_read(attached, b"ab\x1b[D\r"),
        tui_bridge::InlineRead::Submitted("ab".to_string()),
        "a non-empty draft keeps `\u{2190}` as cursor movement while attached"
    );

    // Leaving the view by any other door attaches nothing and does not
    // disturb the attachment in force: `q` is not a detach key.
    let mut quit = OverlayUnderTest::new();
    assert!(quit.feed_closing(&registry, b"q"), "`q` closes the view");
    let exit = AgentViewExit::leaving(&quit.state);
    assert_eq!(exit.attached, None, "and attaches nothing");
    apply_agent_view_exit(&exit, &mut attached);
    assert_eq!(
        attached,
        Some(ids[0]),
        "leaving the view is not detaching; `\u{2190}` and `/detach` are"
    );
    // A view that was never shown is the same answer.
    apply_agent_view_exit(&AgentViewExit::declined(), &mut attached);
    assert_eq!(attached, Some(ids[0]));

    // **F107-b**: end the attachment before returning, or this test
    // leaves `FOREGROUND_RECAP` holding a buffer and the surface slot
    // holding the `BufferedSurface` that displaced the real one. Harmless
    // only while every later path attaches before it detaches; a
    // detach-first test would `set_surface` this test's dead buffer.
    // Driven through the composer loop's own detach arm -- the route both
    // `\u{2190}` and `Ctrl+X d` land in -- rather than a test-only
    // teardown, because the thing that must be exercised is the
    // production end of an attachment. Measured: the slot is `Some(..)`
    // here without this call and `None` with it, and the displaced
    // surface is reinstalled.
    let _ = apply_inline_detach(&mut attached, String::new());
}

/// Finding **F76**, end to end: **`Ctrl+X d` with a line typed detaches
/// and the line reaches the user's next read.**
///
/// **FAILING-FIRST.** Both halves are driven through production and in
/// production's order -- `read_line_inline_with_keys` for the keystroke,
/// then [`apply_inline_detach`], which is the composer loop's detach arm
/// -- because **F75** and **F79** were both "the halves each worked".
/// The replay is the loop's own: the carried draft re-enters the reader
/// ahead of the terminal's bytes, exactly as a suspend's draft does, so
/// the next read is where the user sees it.
///
/// The loop itself needs a real tty, which is why the arm is a function.
#[cfg(unix)]
#[test]
fn the_detach_chord_detaches_and_its_draft_reaches_the_next_read() {
    /// One read of the inline composer with the configured keymap, as the
    /// two tests above drive it.
    fn one_read(attached: Option<u64>, bytes: &[u8]) -> tui_bridge::InlineRead {
        let keys = tui_bridge::InlineKeys {
            prefix: tui_bridge::chord_prefix_byte("ctrl+x"),
            left_arrow_opens_agents: true,
        };
        assert!(keys.prefix.is_some(), "the chord prefix must be bound");
        let mut input = std::io::Cursor::new(bytes.to_vec());
        let mut paint = |_: &str, _: usize, _: &str, _: bool| {};
        tui_bridge::read_line_inline_with_keys(
            &mut input,
            &mut paint,
            &[],
            &mut || {},
            keys,
            attached,
            None,
            None,
        )
        .expect("a cursor cannot fail to read")
    }

    const BASE: &str = "kitsu> ";
    let (registry, ids) = registry_with_three_agents();
    let running_before = registry.snapshot().running;
    // The session binding, as the composer loop holds it while attached.
    let mut attached: Option<u64> = Some(ids[0]);

    let outcome = one_read(attached, b"half a line\x18d");
    let tui_bridge::InlineRead::Detached(draft) = outcome else {
        panic!("F76: `Ctrl+X d` while attached must detach: {outcome:?}");
    };
    // The loop's arm, verbatim: it clears the binding and hands back what
    // the next read must replay.
    let carried = apply_inline_detach(&mut attached, draft);
    assert_eq!(attached, None, "the chord detaches");
    assert_eq!(
        tui_bridge::composer_prompt(BASE, attached),
        BASE,
        "and the composer goes back to the session's own prompt"
    );
    assert_eq!(
        carried, "half a line",
        "F76: the draft the user typed is what the loop carries"
    );
    // The replay, through the loop's own cursor chain: the carried draft
    // is fed ahead of the terminal's next bytes, so the user finds the
    // line still there and submits it.
    let replayed = one_read(attached, format!("{carried}\r").as_bytes());
    assert_eq!(
        replayed,
        tui_bridge::InlineRead::Submitted("half a line".to_string()),
        "and the next read starts from it"
    );
    // Detaching stops nothing: the agent is still running.
    assert_eq!(
        registry.snapshot().running,
        running_before,
        "a detach is a routing change and not a cancellation"
    );

    // NON-VACUOUS: the `\u{2190}` route is unchanged, and its payload is
    // genuinely empty -- so the assertion above is about the chord's
    // draft and not about every detach carrying a constant.
    let mut attached = Some(ids[1]);
    let outcome = one_read(attached, b"\x1b[D");
    let tui_bridge::InlineRead::Detached(draft) = outcome else {
        panic!("A5: `\u{2190}` on an empty composer must detach: {outcome:?}");
    };
    let carried = apply_inline_detach(&mut attached, draft);
    assert_eq!(attached, None, "`\u{2190}` still detaches");
    assert_eq!(carried, "", "and carries an empty draft, because it is one");
    // NON-VACUOUS: `\u{2190}` with a draft typed is cursor movement and
    // not a detach, which is what leaves the chord its reason to exist.
    let attached = Some(ids[2]);
    let outcome = one_read(attached, b"ab\x1b[D\r");
    assert_eq!(
        outcome,
        tui_bridge::InlineRead::Submitted("ab".to_string()),
        "a non-empty draft keeps `\u{2190}` as cursor movement"
    );
    assert_eq!(
        attached,
        Some(ids[2]),
        "so the attachment is untouched and the chord is the only route \
             left for a non-empty draft"
    );
}

/// Card **T4-S6-status**, requirement A5 / design §4.5: **the row and the
/// prompt label are the same attachment**, carried by production.
///
/// **FAILING-FIRST**, and this is the test that fails if the row is given
/// a copy of the attachment instead of a read of it. Every transition is
/// driven through the production carriers T4-S6a shipped — the view's
/// `Enter`, [`AgentViewExit::leaving`], [`apply_agent_view_exit`], and the
/// reader's `Detached` outcome — and after each one **two** derived
/// surfaces are asserted against each other: the status row
/// (`StatusSource::compose`, the string `repaint_status_row` paints) and
/// the composer prompt (`tui_bridge::composer_prompt`, the label in front
/// of the caret). Nothing in this test tells the row anything; the row's
/// counts closure reads the **same cell** `apply_agent_view_exit` writes,
/// which is the shape production must have because the painter composes
/// the row from a cadence tick the composer loop is not on.
///
/// Why both surfaces and not just the row: §4.5's amendment rests the
/// `←`-detaches-on-the-first-press rule on the attachment being legible
/// at the moment of the press, and the label carried that argument alone
/// (**F74**). Two labels derived from one binding keep it; a label and a
/// *copy* would be the fifth occurrence of **F75**'s family, so the
/// agreement is asserted after every transition rather than once.
///
/// The terminal is the one thing not traversed, exactly as in
/// `the_views_attach_reaches_the_composer_and_makes_the_detach_keys_live`.
#[cfg(unix)]
#[tokio::test]
// **F107**: not optional. `#[serial]` excludes other `#[serial]` tests, not
// plain ones, and this test reaches the process-global surface slot through
// PRODUCTION code rather than `SurfaceProbe`, so it can install over a
// serialised test's probe mid-run. Measured: 3 failures in 421 unguarded
// runs of `--bins`, 0 in 400 with this guard.
#[serial_test::serial]
async fn the_status_row_and_the_prompt_label_read_the_one_attachment_binding() {
    /// One read of the inline composer with a configured keymap, as the
    /// F75 test drives it.
    fn one_read(attached: Option<u64>, bytes: &[u8]) -> tui_bridge::InlineRead {
        let keys = tui_bridge::InlineKeys {
            prefix: tui_bridge::chord_prefix_byte("ctrl+x"),
            left_arrow_opens_agents: true,
        };
        let mut input = std::io::Cursor::new(bytes.to_vec());
        let mut paint = |_: &str, _: usize, _: &str, _: bool| {};
        tui_bridge::read_line_inline_with_keys(
            &mut input,
            &mut paint,
            &[],
            &mut || {},
            keys,
            attached,
            None,
            None,
        )
        .expect("a cursor cannot fail to read")
    }

    const BASE: &str = "kitsu> ";
    const COLS: usize = 120;
    const LEFT: &[u8] = b"\x1b[D";

    let (registry, ids) = registry_with_three_agents();
    // The session binding, behind the handle the status row's counts
    // closure reads at paint time. One cell, two readers.
    let binding: Arc<std::sync::Mutex<Option<u64>>> = Arc::new(std::sync::Mutex::new(None));
    let cell = binding.clone();
    let source = tui_bridge::StatusSource::new(Box::new(move || tui_bridge::StatusCounts {
        attached: *cell.lock().expect("the session binding"),
        ..Default::default()
    }));
    source.set_usage("example-model-v1", 1_000, 100_000);
    let row = || kitsu_render::sanitize_terminal_text(&source.compose(COLS));
    let label = || tui_bridge::composer_prompt(BASE, *binding.lock().expect("the binding"));

    // Detached: neither surface claims an attachment, and the row is a
    // row (the control that makes the absence non-vacuous).
    assert!(
        row().contains("example-model-v1"),
        "control: the detached row is painted: {:?}",
        row()
    );
    assert!(!row().contains("attached"), "{:?}", row());
    assert_eq!(label(), BASE);

    // Attach, through the two carriers production uses, in its order.
    let mut overlay = OverlayUnderTest::new();
    assert!(
        overlay.feed_closing(&registry, b"\r"),
        "A3: `Enter` attaches and leaves the view"
    );
    let exit = AgentViewExit::leaving(&overlay.state);
    apply_agent_view_exit(&exit, &mut binding.lock().expect("the binding"));
    assert_eq!(
        *binding.lock().expect("the binding"),
        Some(ids[0]),
        "F75: the view's attachment reaches the session binding"
    );
    assert_eq!(
        label(),
        format!("agent {}> ", ids[0]),
        "the prompt label names it (T4-S6a, already shipped)"
    );
    assert!(
        row().contains(&format!("attached \u{b7} agent {}", ids[0])),
        "design §4.5: and so does the status row, off the same binding: \
             {:?}",
        row()
    );

    // An agent finishing while attached does **not** detach: the segment
    // is derived from the binding, and the registry is not consulted.
    // Decided here, because §4.5 does not say; T4-S6a's "leaving the view
    // is not detaching" is the precedent, and a row that detached itself
    // would disagree with the label in front of the caret.
    registry.finish(ids[0], Ok("alpha: done".to_string()));
    assert!(
        registry.snapshot().terminal >= 1,
        "control: the attached agent really reached a terminal state"
    );
    assert!(
        row().contains(&format!("attached \u{b7} agent {}", ids[0])),
        "an agent finishing while attached must not silently detach: \
             {:?}",
        row()
    );
    assert_eq!(
        label(),
        format!("agent {}> ", ids[0]),
        "and the two surfaces still agree"
    );

    // Detach, through the reader's own outcome and the composer loop's
    // arm, which is `*attached = None` and nothing else: no second
    // notification exists, so a row holding a copy stays red here.
    assert_eq!(
        one_read(*binding.lock().expect("the binding"), LEFT),
        tui_bridge::InlineRead::Detached(String::new()),
        "A5: `\u{2190}` on an empty composer detaches while attached"
    );
    *binding.lock().expect("the binding") = None;
    // **F107-b**: clearing the cell is the whole of the detach for the
    // two derived surfaces, but it is not the whole of the attachment --
    // `apply_agent_view_exit` also installed a recap buffer, and only
    // `replay_foreground_recap` (the one function all four production
    // transitions out of an attachment call) takes it back out. Without
    // it the re-attach below nests a second buffer over the first and the
    // test returns with both leaked.
    replay_foreground_recap(ids[0]);
    assert_eq!(label(), BASE, "the label goes back to the session's own");
    assert!(
        !row().contains("attached"),
        "A5: and the row stops claiming an attachment: {:?}",
        row()
    );
    assert!(
        row().contains("example-model-v1"),
        "while still being the whole row: {:?}",
        row()
    );

    // `Ctrl+X d` is the same binding and the same arm: re-attach through
    // production and detach with the chord.
    apply_agent_view_exit(&exit, &mut binding.lock().expect("the binding"));
    assert!(row().contains("attached \u{b7} agent"), "{:?}", row());
    assert_eq!(
        one_read(*binding.lock().expect("the binding"), b"\x18d"),
        tui_bridge::InlineRead::Detached(String::new()),
        "A5: `Ctrl+X d` detaches too"
    );
    *binding.lock().expect("the binding") = None;
    // **F107-b**, as above: the second attachment's buffer, ended through
    // the production function. Measured: `FOREGROUND_RECAP` is `None` and
    // the surface this test started with is reinstalled.
    replay_foreground_recap(ids[0]);
    assert!(!row().contains("attached"), "{:?}", row());
    assert_eq!(label(), BASE);
}

// ------------------------------------------------------------------
// Finding **F77**: `/detach` is registered, dispatched — and does
// nothing. Its arm is unconditional, written while nothing could
// attach; the rework that made attachment work never revisited it, so
// `\u{2190}` and `Ctrl+X d` detach today and the command never has.
//
// **FAILING-FIRST**, against [`apply_detach_command`], which is the whole
// of the command: it answers *and* clears, so the answer cannot be
// computed from one state and the clearing applied to another. Today it
// returns `NothingAttached` and clears nothing, which is exactly what the
// shipped arm does.
//
// **What this pair cannot reach, and why the seam is where it is.**
// `handle_slash` cannot be driven from a unit test: it takes a
// `&SessionInfo` (thirty-odd fields, no `Default`, no test constructor
// anywhere in the tree) and a bound `ProviderRouteCurrent`, and the
// detach arm returns before touching either but Rust still demands them.
// The PTY harness cannot reach it either, for the reason T4-S6a already
// recorded: a fixture session has **zero agents** and no way to spawn one,
// so nothing there can attach. So the card splits the way T4-S4b's
// `agent_view_steer_report`/`agent_view_steer` pair split: the decision
// and the clearing are pinned here against the real binding, and the one
// step left unpinned is `chat::run_chat` handing its own
// `attached_agent` to the arm. That step is a single argument, the binding
// is the attachment `Option<u64>` in `chat::run_chat`, and a copy made to feed
// it would have to be newly introduced — which is what review is for.
// ------------------------------------------------------------------

/// Finding **F77**: **`/detach` clears the one binding both derived
/// surfaces read.**
///
/// Shaped deliberately like
/// `the_status_row_and_the_prompt_label_read_the_one_attachment_binding`,
/// because the failure it must catch is the same one twice over: an arm
/// that clears *something* and reports success is indistinguishable, from
/// inside the arm, from one that clears the session's attachment. So
/// nothing here is constructed and handed to the command. The attachment
/// arrives through the production carriers T4-S6a shipped — the view's
/// `Enter`, [`AgentViewExit::leaving`], [`apply_agent_view_exit`] — into
/// the cell the status row's counts closure and
/// [`tui_bridge::composer_prompt`] both read, and the command is given
/// that same cell. A command wired to anything else leaves both surfaces
/// naming the agent, which is where this test then fails.
///
/// The two surfaces are asserted rather than the cell (**F71**): the row
/// is read back off `StatusSource::compose`, the string
/// `repaint_status_row` paints, and the label off the function that sits
/// in front of the caret. 120 columns, where the segment demonstrably
/// fits — **F78** forbids reading anything into the row's behaviour
/// across widths, so this test says nothing about other widths.
#[cfg(unix)]
#[tokio::test]
// **F107**: not optional. `#[serial]` excludes other `#[serial]` tests, not
// plain ones, and this test reaches the process-global surface slot through
// PRODUCTION code rather than `SurfaceProbe`, so it can install over a
// serialised test's probe mid-run. Measured: 3 failures in 421 unguarded
// runs of `--bins`, 0 in 400 with this guard.
#[serial_test::serial]
async fn the_detach_command_clears_the_binding_the_row_and_the_label_read() {
    const BASE: &str = "kitsu> ";
    const COLS: usize = 120;

    let (registry, ids) = registry_with_three_agents();
    let binding: Arc<std::sync::Mutex<Option<u64>>> = Arc::new(std::sync::Mutex::new(None));
    let cell = binding.clone();
    let source = tui_bridge::StatusSource::new(Box::new(move || tui_bridge::StatusCounts {
        attached: *cell.lock().expect("the session binding"),
        ..Default::default()
    }));
    source.set_usage("example-model-v1", 1_000, 100_000);
    let row = || kitsu_render::sanitize_terminal_text(&source.compose(COLS));
    let label = || tui_bridge::composer_prompt(BASE, *binding.lock().expect("the binding"));

    // Attach through production, in production's order.
    let mut overlay = OverlayUnderTest::new();
    assert!(
        overlay.feed_closing(&registry, b"\r"),
        "A3: `Enter` attaches and leaves the view"
    );
    apply_agent_view_exit(
        &AgentViewExit::leaving(&overlay.state),
        &mut binding.lock().expect("the binding"),
    );
    assert_eq!(
        *binding.lock().expect("the binding"),
        Some(ids[0]),
        "control: the view's attachment reached the session binding"
    );
    let segment = format!("attached \u{b7} agent {}", ids[0]);
    assert!(
        row().contains(&segment),
        "control, and the non-vacuity of both absences below: the row \
             names the agent at {COLS} columns before the command runs: {:?}",
        row()
    );
    assert_eq!(
        label(),
        format!("agent {}> ", ids[0]),
        "control: and so does the prompt label"
    );
    let running_before = registry.snapshot().running;
    assert_eq!(
        running_before, 3,
        "control: three agents are running, so requirement A5's \
             `detaching stops nothing` has something to be true of"
    );

    // The command, against that binding and nothing else.
    let outcome = apply_detach_command(&mut binding.lock().expect("the binding"));
    assert_eq!(
        outcome,
        DetachCommandOutcome::Detached(ids[0]),
        "F77: `/detach` while attached detaches, and says which agent it \
             left"
    );
    assert!(
        !row().contains("attached"),
        "F77: the status row stops claiming an attachment, because it is \
             derived from the binding the command cleared: {:?}",
        row()
    );
    assert!(
        row().contains("example-model-v1"),
        "while still being the whole row: {:?}",
        row()
    );
    assert_eq!(
        label(),
        BASE,
        "F77: and the composer wears the session's own prompt again"
    );
    assert_eq!(
        registry.snapshot().running,
        running_before,
        "requirement A5 / design §4.5: detaching stops **nothing** -- the \
             agent keeps running, and the registry is not touched"
    );

    // Idempotent, and the same answer a never-attached session gets: the
    // second `/detach` has nothing to clear.
    assert_eq!(
        apply_detach_command(&mut binding.lock().expect("the binding")),
        DetachCommandOutcome::NothingAttached,
        "a second `/detach` has nothing left to detach from"
    );
    assert!(!row().contains("attached"), "{:?}", row());
    assert_eq!(label(), BASE);
}

/// Finding **F77**, the behaviour that must **survive** the wiring:
/// `/detach` with nothing attached still answers, and what each answer
/// says.
///
/// The refusal is today's one correct half — requirement T2-S5b, a
/// submitted line is an intention with somewhere to put the answer — and
/// the PTY session
/// `inline_mode_routes_the_detach_chord_and_answers_detach_without_an_attachment`
/// pins that this exact string reaches the scroll region, alongside a
/// `/context` control proving the detector fires. That observation is also
/// what makes the success line visible (**F71**): it is the same
/// `eprintln!` from the same arm, so the channel is already measured on a
/// real terminal. What is pinned here is the text and the dispatcher
/// answer for each outcome.
#[test]
fn the_detach_command_answers_whether_or_not_anything_was_attached() {
    assert_eq!(
        DetachCommandOutcome::NothingAttached.report(),
        "not attached to a background agent",
        "the refusal the PTY session measures on screen, unchanged by the \
             wiring"
    );
    assert!(
        matches!(
            DetachCommandOutcome::NothingAttached.slash_result(),
            SlashResult::Error
        ),
        "and it stays an error, so scripted automation keeps reporting it \
             as one"
    );

    // The success line, decided on this card: `\u{2190}` and `Ctrl+X d`
    // are silent because the label changing under the caret is their
    // feedback, but a submitted command moves the eye into the region and
    // is the last moment a user who read `/detach` as `cancel` can be
    // told otherwise.
    assert_eq!(
        DetachCommandOutcome::Detached(7).report(),
        "detached from agent 7; it keeps running",
        "the success line names the agent left and says the agent keeps \
             running, which is the registry `details` clause at the one moment \
             it matters"
    );
    assert_ne!(
        DetachCommandOutcome::Detached(7).report(),
        DetachCommandOutcome::NothingAttached.report(),
        "NON-VACUOUS: the two answers are different lines, so a user can \
             tell a detach from a refusal"
    );
    match DetachCommandOutcome::Detached(7).slash_result() {
        SlashResult::Continue(Some(value)) => {
            assert_eq!(
                value,
                serde_json::json!({"kind": "detach", "agent": 7}),
                "a detach that happened is a command that succeeded, with \
                     the structured record scripted automation reads"
            );
        }
        _ => panic!(
            "`/detach` must succeed when it detached, with a structured record; \
                 it answered with another `SlashResult` variant"
        ),
    }
}

/// Finding **F79**: **`/jobs view` attaches to the binding both derived
/// surfaces read** — the one test this card adds, as the explicit
/// exception the finding authorises.
///
/// `run_agent_view_overlay` has two callers and this arm kept only
/// `shown`, so selecting a row and pressing `Enter` from `/jobs view`
/// attached to nothing: the prompt label and the status row never changed
/// and the detach keys stayed inert, while the chord and arrow routes
/// worked. That is **F75 word for word in the third consumer of the same
/// exit value**, so this is shaped like
/// `the_detach_command_clears_the_binding_the_row_and_the_label_read`:
/// nothing is constructed and handed to the arm. The attachment arrives
/// through the production carriers — the view's `Enter`,
/// [`AgentViewExit::leaving`] — and the arm's own half,
/// [`apply_jobs_view_exit`], is given the **same cell** the status row's
/// counts closure and [`tui_bridge::composer_prompt`] read, so an arm
/// wired to a copy, or to nothing, leaves both surfaces bare and this
/// test fails there. (The arm cannot be entered through
/// `handle_jobs_command_attaching` from a test: `run_agent_view_overlay`
/// declines outside inline mode on a tty, which is exactly why the
/// discarded attachment survived review.)
///
/// Both surfaces are asserted rather than the cell (**F71**), at 120
/// columns where the segment demonstrably fits — **F78** forbids reading
/// anything into the row across widths, so this says nothing about any
/// other width. `shown` is asserted too: it is the answer the text-deck
/// fallback depends on, and it must survive the fix that adds the second.
#[cfg(unix)]
#[tokio::test]
// **F107**: not optional. `#[serial]` excludes other `#[serial]` tests, not
// plain ones, and this test reaches the process-global surface slot through
// PRODUCTION code rather than `SurfaceProbe`, so it can install over a
// serialised test's probe mid-run. Measured: 3 failures in 421 unguarded
// runs of `--bins`, 0 in 400 with this guard.
#[serial_test::serial]
async fn the_jobs_view_command_attaches_to_the_binding_the_row_and_the_label_read() {
    const BASE: &str = "kitsu> ";
    const COLS: usize = 120;

    let (registry, ids) = registry_with_three_agents();
    let binding: Arc<std::sync::Mutex<Option<u64>>> = Arc::new(std::sync::Mutex::new(None));
    let cell = binding.clone();
    let source = tui_bridge::StatusSource::new(Box::new(move || tui_bridge::StatusCounts {
        attached: *cell.lock().expect("the session binding"),
        ..Default::default()
    }));
    source.set_usage("example-model-v1", 1_000, 100_000);
    let row = || kitsu_render::sanitize_terminal_text(&source.compose(COLS));
    let label = || tui_bridge::composer_prompt(BASE, *binding.lock().expect("the binding"));

    let segment = format!("attached \u{b7} agent {}", ids[0]);
    assert!(
        !row().contains("attached"),
        "control, and the non-vacuity of the assertions below: the row \
             claims no attachment before the view runs: {:?}",
        row()
    );
    assert_eq!(label(), BASE, "control: nor does the prompt label");

    // The view, through production, in production's order.
    let mut overlay = OverlayUnderTest::new();
    assert!(
        overlay.feed_closing(&registry, b"\r"),
        "A3: `Enter` attaches and leaves the view"
    );
    let exit = AgentViewExit::leaving(&overlay.state);

    // The `/jobs view` arm's half, against that binding and nothing else.
    let shown = apply_jobs_view_exit(&exit, &mut binding.lock().expect("the binding"));
    assert!(
        shown,
        "F79: the arm still answers that the view was shown, so the text \
             deck is not also printed underneath an overlay (T4-S2 A1)"
    );
    assert!(
        row().contains(&segment),
        "F79: the status row names the agent `/jobs view` attached to, \
             because the arm applied the exit to the session's own binding \
             rather than discarding it: {:?}",
        row()
    );
    assert!(
        row().contains("example-model-v1"),
        "while still being the whole row: {:?}",
        row()
    );
    assert_eq!(
        label(),
        format!("agent {}> ", ids[0]),
        "F79: and the composer wears the attached agent's prompt, which \
             is what makes `\u{2190}`, `Ctrl+X d` and `/detach` live"
    );

    // And the command is then detachable, which is the whole point of
    // folding F79 into F77's card: `/detach` reads this same binding.
    assert_eq!(
        apply_detach_command(&mut binding.lock().expect("the binding")),
        DetachCommandOutcome::Detached(ids[0]),
        "F77+F79: an attachment made by `/jobs view` is one `/detach` can \
             leave"
    );
    assert!(!row().contains("attached"), "{:?}", row());
    assert_eq!(label(), BASE);
}

/// Card **T4-S10**, requirement 1 and finding **F57**: **every press the
/// overlay dispatches is in the bindings table, and every press in the
/// table is dispatched.**
///
/// The sweep is over the *keyboard*, not over the arms: every printable
/// byte the decoder can turn into an `OverlayKey::Char` (`0x20..=0x7e` by
/// construction), plus each non-printable key it decodes -- the arrows,
/// `Enter`, `Backspace` and a resolved `Esc`. Nothing here reads the
/// dispatch's arms or the help's rows, so it cannot agree with either by
/// construction, which is what F57's guard could not do.
///
/// "Dispatched" is asked of the **painted frame** and of whether the view
/// closed (**F71**), never of a state field: a key that changes the frame
/// or ends the view did something a user can see, and a key that changes
/// neither did not.
///
/// Two presses are deliberately *not* in the table and are asserted
/// inert, so this is not satisfiable by a table that lists the whole
/// keyboard: `Backspace`, which belongs to the prompts and must not reach
/// the composer underneath, and a letter with no binding at all.
#[cfg(unix)]
#[tokio::test]
async fn every_press_the_overlay_dispatches_is_in_the_bindings_table() {
    use kitsu_tui::keys::{Press, BINDINGS};

    /// One key of the sweep: how a terminal delivers it, and the
    /// `Press` the bindings table would have to name it by.
    struct Swept {
        name: String,
        bytes: Vec<u8>,
        /// `None` for a key the table cannot name: `Backspace` has no
        /// `Press` variant because it is bound to nothing.
        press: Option<Press>,
        /// `Esc` is one byte and the start of every arrow, so it is only
        /// a key once a quiet tick has resolved it.
        held: bool,
    }

    let mut sweep: Vec<Swept> = Vec::new();
    for byte in 0x20u8..=0x7e {
        let typed = byte as char;
        sweep.push(Swept {
            name: format!("`{typed}`"),
            bytes: vec![byte],
            press: Some(Press::Char(typed)),
            held: false,
        });
    }
    for (name, bytes, press) in [
        ("up", &b"\x1b[A"[..], Some(Press::Up)),
        ("down", &b"\x1b[B"[..], Some(Press::Down)),
        ("right", &b"\x1b[C"[..], Some(Press::Right)),
        ("enter", &b"\r"[..], Some(Press::Enter)),
        ("backspace", &b"\x7f"[..], None),
    ] {
        sweep.push(Swept {
            name: name.to_string(),
            bytes: bytes.to_vec(),
            press,
            held: false,
        });
    }
    sweep.push(Swept {
        name: "esc".to_string(),
        bytes: b"\x1b".to_vec(),
        press: Some(Press::Esc),
        held: true,
    });

    let mut acts: Vec<String> = Vec::new();
    let mut inert: Vec<String> = Vec::new();
    for key in &sweep {
        // A fresh registry and a fresh view per key: `x`, `p` and the
        // dispatch key change the registry, and a shared one would make
        // the sweep's answer depend on its order.
        let (registry, _ids) = registry_with_three_agents();
        let mut overlay = OverlayUnderTest::new();
        // Two presses of `\u{2193}` first, so that there *is* a selected
        // agent (the keys that act on one are otherwise inert, and the
        // sweep would call them unbound) and so that `\u{2191}` has a row
        // to move back to rather than clamping where it stands.
        overlay.feed_and_frame_at(&registry, b"\x1b[B", BINDINGS_SWEEP_COLS);
        let before = overlay.feed_and_frame_at(&registry, b"", BINDINGS_SWEEP_COLS);
        let closing = overlay.feed_closing(&registry, &key.bytes);
        let closing = closing || (key.held && overlay.quiet_tick());
        let after = {
            let rows = overlay.visible(&registry);
            overlay_frame_rows(&overlay.state, &rows, BINDINGS_SWEEP_COLS)
        };
        if closing || after != before {
            acts.push(key.name.clone());
        } else {
            inert.push(key.name.clone());
        }
    }

    // NON-VACUOUS, both ways: a sweep that found every key inert, or
    // every key bound, would prove nothing about the table.
    assert!(
        !acts.is_empty() && !inert.is_empty(),
        "the sweep must find both bound and unbound keys; bound: \
             {acts:?}, unbound: {inert:?}"
    );

    let tabled = |press: Option<Press>| {
        press.is_some_and(|press| {
            BINDINGS
                .iter()
                .any(|binding| binding.presses.contains(&press))
        })
    };

    // Direction one, the F57 direction the brief could not guard before:
    // a key that *does* something and is in no table row is a key no
    // reader can advertise.
    let undocumented: Vec<&str> = sweep
        .iter()
        .filter(|key| acts.contains(&key.name) && !tabled(key.press))
        .map(|key| key.name.as_str())
        .collect();
    assert!(
        undocumented.is_empty(),
        "every press that changes the frame or closes the view is one \
             row of the bindings table; these are dispatched and in no row: \
             {undocumented:?}"
    );

    // Direction two: a row nothing acts on is F57 verbatim -- the help
    // would advertise a key that does nothing.
    let unbound: Vec<&str> = sweep
        .iter()
        .filter(|key| tabled(key.press) && inert.contains(&key.name))
        .map(|key| key.name.as_str())
        .collect();
    assert!(
        unbound.is_empty(),
        "every row of the bindings table is a press the dispatch acts \
             on; these are advertised and inert: {unbound:?}"
    );

    // And the two presses that must stay out of the table, so this is
    // not satisfiable by listing the keyboard.
    assert!(
        inert.iter().any(|name| name == "backspace"),
        "backspace belongs to the prompts: over the row list it does \
             nothing and must not reach the composer underneath"
    );
    assert!(
        inert.iter().any(|name| name == "`z`"),
        "`z` is bound to nothing, and the overlay still owns the byte"
    );
}

/// **The `\u{26E9}` milestone, through the surface that paints it.**
///
/// Driven through `apply_agent_view_exit` -- the **one** seam both attach
/// routes (the `\u{2190}\u{2190}` overlay and `/jobs view`) go through, and the
/// seam T4-S6a's tests already drive -- so this is the wired route and
/// not a formatter called in a vacuum.
#[cfg(unix)]
#[test]
#[serial]
fn attaching_paints_one_torii_milestone_that_names_the_agent_and_the_route() {
    let probe = SurfaceProbe::install();
    let mut attached: Option<u64> = None;

    // CONTROL, and the non-vacuity of the claim below: a view closed with
    // `q` attached to nothing, so nothing is announced.
    apply_agent_view_exit(&AgentViewExit::declined(), &mut attached);
    assert_eq!(attached, None);
    assert!(
        probe.lines().is_empty(),
        "leaving the view without attaching announces nothing: {:?}",
        probe.lines()
    );

    apply_agent_view_exit(&attaching_exit(7), &mut attached);
    assert_eq!(attached, Some(7), "control: the binding took the agent");
    assert_eq!(
        probe.lines(),
        vec![kitsu_render::milestone_line(&attach_milestone_message(7))],
        "design \u{a7}4.5: attaching paints exactly one `\u{26E9}` milestone, \
             through the renderer's surface, and it says what the composer \
             now does with a submitted line"
    );
}

/// The milestone is painted for a **change** of binding and nothing else.
///
/// Both halves matter. `Enter` in the view on the agent you are already
/// attached to is a no-op, and a second identical milestone would be a
/// claim that something happened; attaching to a *different* agent is a
/// real change and must announce the new id, or a user who re-attached
/// would steer an agent the transcript never named.
#[cfg(unix)]
#[test]
#[serial]
fn the_attach_milestone_is_painted_once_per_change_of_binding() {
    let probe = SurfaceProbe::install();
    let mut attached: Option<u64> = None;

    apply_agent_view_exit(&attaching_exit(7), &mut attached);
    let after_first = probe.lines();
    assert_eq!(after_first.len(), 1, "control: {after_first:?}");

    apply_agent_view_exit(&attaching_exit(7), &mut attached);
    assert_eq!(
        probe.lines(),
        after_first,
        "re-attaching to the agent already attached changes nothing, so \
             it announces nothing"
    );

    apply_agent_view_exit(&attaching_exit(9), &mut attached);
    assert_eq!(attached, Some(9));
    assert_eq!(
        probe.lines(),
        vec![
            kitsu_render::milestone_line(&attach_milestone_message(7)),
            kitsu_render::milestone_line(&attach_milestone_message(9)),
        ],
        "and moving the attachment announces the agent it moved to"
    );
}

/// **Which lines are the attached agent's at all.**
///
/// The composer's loop asks this one question before routing, so what it
/// excludes is as load-bearing as what it claims: a session whose every
/// line went to the agent could not reach `/detach` and would be a trap.
#[cfg(unix)]
#[test]
fn only_a_non_command_line_at_an_attached_composer_belongs_to_the_agent() {
    assert_eq!(
        attached_route_target("fix the UTF-8 boundary", Some(3)),
        Some(3),
        "A5: while attached, a typed line is the agent's"
    );
    assert_eq!(
        attached_route_target("fix the UTF-8 boundary", None),
        None,
        "CONTROL: detached, the same line is the model's"
    );
    for command in ["/detach", "/jobs steer 3 later", "/help", "  /detach  "] {
        assert_eq!(
            attached_route_target(command, Some(3)),
            None,
            "a command stays a command while attached, or the session \
                 could never leave by command: {command:?}"
        );
    }
    for blank in ["", "   ", "\t"] {
        assert_eq!(
            attached_route_target(blank, Some(3)),
            None,
            "a blank line is not a correction: {blank:?}"
        );
    }
    assert_eq!(
        attached_route_target("   fix the parser", Some(3)),
        Some(3),
        "leading whitespace does not make a line a command"
    );
}

/// **The answer route: a blocked agent, answered through the one
/// parser.**
///
/// Every spelling `parse_permission_answer` accepts is driven, and the
/// expected decision is taken **from that parser** rather than written
/// out here (**F45**): a route that grew its own `match` on `y`/`a`/`n`
/// would drift from the foreground prompt and from `/jobs reply` the
/// first time the vocabulary changed, and this test would then fail on
/// the spelling it stopped sharing.
///
/// Non-vacuous by the gate: the agent's parked request really resolves
/// with that decision, so an implementation that computed an answer and
/// delivered nothing fails here.
#[cfg(unix)]
#[tokio::test]
#[serial]
async fn a_line_typed_at_a_blocked_attached_agent_answers_it_through_the_one_parser() {
    for spelling in ["y", "yes", "YES", "a", "always", "n", "no", " y\n"] {
        let expected = parse_permission_answer(spelling)
            .unwrap_or_else(|| panic!("the one parser accepts {spelling:?}"));
        let (registry, id, answer) = registry_with_one_blocked_agent().await;
        let probe = SurfaceProbe::install();

        let submit = route_attached_submission(&registry, id, spelling);

        assert_eq!(
            submit.route,
            AttachedRoute::Answered(expected),
            "A5: a blocked attached agent is answered, not steered \
                 ({spelling:?})"
        );
        assert_eq!(
            submit.draft, "",
            "a delivered line is gone from the composer ({spelling:?})"
        );
        assert_eq!(
            answer.await.expect("the gate's oneshot is resolved"),
            expected,
            "the agent's parked request sees exactly the decision the one \
                 parser read ({spelling:?})"
        );
        assert_eq!(
            registry.snapshot().blocked,
            0,
            "and it is no longer blocked ({spelling:?})"
        );
        assert_eq!(
            probe.lines(),
            vec![jobs_reply_receipt(
                id,
                subagent::AgentAnswerOutcome::Answered,
                expected
            )],
            "the user reads `/jobs reply`'s own receipt, painted through \
                 the surface -- not a second wording of it ({spelling:?})"
        );
    }
}

/// **Test: a line typed at an attached agent is retained as the user's
/// prompt** (**F102**, the second feed point).
///
/// This route `continue`s before the line can become a
/// `ChatReadline::Line` -- the comment at its call site says so -- so the
/// foreground feed never sees it, and leaving it out would ship a key that
/// works at the idle prompt and quietly does nothing during an attachment
/// (**F84**/**F75**).
///
/// Driven through `route_attached_submission`, the real route, over a real
/// `InlineSurface` with a real ring: nothing here mirrors the decision the
/// production path makes.
///
/// **Ordering is asserted, not assumed:** the answer's receipt is painted
/// by this same function, so the prompt must precede it in the ring
/// exactly as the foreground feed precedes a turn. The `Steered` route is
/// the same `delivered` branch and is not re-swept here.
#[cfg(unix)]
#[tokio::test]
#[serial]
async fn a_line_typed_at_an_attached_agent_is_retained_as_the_users_prompt() {
    let (registry, id, answer) = registry_with_one_blocked_agent().await;
    let probe = RingProbe::install();

    let submit = route_attached_submission(&registry, id, "y");

    assert_eq!(
        submit.draft, "",
        "precondition: the line was delivered, not refused"
    );
    let marks = probe.marked();
    assert_eq!(
        marks
            .first()
            .map(|(is_prompt, text)| (*is_prompt, text.as_str())),
        Some((true, "y")),
        "the user's line is the first thing in the ring, ahead of the \
             receipt it provoked: {marks:?}"
    );
    assert_eq!(
        marks.len(),
        2,
        "and the receipt is the only other cell: {marks:?}"
    );
    assert!(
        !marks[1].0,
        "the receipt is the session's own words, not the user's: {marks:?}"
    );
    drop(probe);
    // The receiver is dropped, never awaited: on a refusal the gate's
    // oneshot is never resolved, so awaiting it would hang this test
    // forever -- which it did, measured, before this line said so.
    drop(answer);
}

/// **Test: and a refused one is not** -- the opposite failure, and the one
/// that would put a line in the transcript the user is still editing.
///
/// On every refusal `AttachedSubmit::draft` hands the line back as the
/// next read's draft (**F76**), so it was never submitted. A transcript
/// claiming it was said is a false claim in the one record the overlay
/// reads (**F77**), and `{`/`}` would stop on a line that never ran.
///
/// The fixture is the settled race: a correction typed while the agent
/// blocks is not an answer and cannot be steered, so nothing is sent.
#[cfg(unix)]
#[tokio::test]
#[serial]
async fn a_refused_attached_submission_is_not_retained_as_a_prompt() {
    const CORRECTION: &str = "no, use the byte offsets from the lexer";
    assert!(
        parse_permission_answer(CORRECTION).is_none(),
        "premise: the one parser does not read a correction as an answer"
    );
    let (registry, id, answer) = registry_with_one_blocked_agent().await;
    let probe = RingProbe::install();

    let submit = route_attached_submission(&registry, id, CORRECTION);

    assert_eq!(
        submit.route,
        AttachedRoute::Refused,
        "precondition: nothing was sent"
    );
    assert_eq!(
        submit.draft, CORRECTION,
        "precondition: the line is back in the composer (F76)"
    );
    let marks = probe.marked();
    assert!(
        marks.iter().all(|(is_prompt, _)| !is_prompt),
        "a line still being edited was never said: {marks:?}"
    );
    drop(probe);
    // Never awaited: nothing was sent, so this oneshot is never resolved.
    drop(answer);
}

/// **The other half of the race: the agent finished while you typed.**
///
/// Not blocked, so the line is steered -- and the registry is the
/// adjudicator, so a line aimed at an agent that is no longer running is
/// refused **in the registry's own words**, with the line kept. An
/// implementation that pre-judged the state and invented a wording would
/// fail on the text; one that swallowed the line would fail on the draft.
#[cfg(unix)]
#[tokio::test]
#[serial]
async fn a_line_routed_to_an_agent_that_is_no_longer_running_is_refused_in_its_own_words() {
    const CORRECTION: &str = "stop at the first failing case";
    let registry = subagent::AgentRegistry::new();
    let id = registry.start("already done".to_string()).unwrap();
    registry.finish(id, Ok("done".to_string()));

    // Derived, not spelled: a refused steer changes nothing, so asking
    // the registry for its own refusal is side-effect free.
    let expected = registry
        .steer(id, CORRECTION)
        .expect_err("a terminal agent cannot be steered")
        .to_string();
    let probe = SurfaceProbe::install();

    let submit = route_attached_submission(&registry, id, CORRECTION);

    assert_eq!(submit.route, AttachedRoute::Refused);
    assert_eq!(
        submit.draft, CORRECTION,
        "F76: a refusal the user could not have predicted must not cost \
             them the line"
    );
    assert_eq!(
        probe.lines(),
        vec![expected],
        "the registry's own words for the refusal, painted through the \
             surface"
    );
}

/// **All three detach routes replay the recap** (design \u{a7}4.5,
/// **F79**).
///
/// **FAILING-FIRST**: nothing buffers today, so the foreground lines
/// reach the probe immediately and no header is ever painted.
///
/// **How the routes were enumerated (F79: callers are enumerable).**
/// There are exactly three keystroke/command routes out of an
/// attachment and exactly two arms they reach:
/// `\u{2190}` and `Ctrl+X d` both become `InlineRead::Detached` and go
/// through [`apply_inline_detach`]; `/detach` goes through
/// [`apply_detach_command`]. Both arms are driven here, and the two
/// keystrokes are driven as **keystrokes** through
/// `read_line_inline_with_keys` rather than by constructing the outcome
/// -- **F77** was an arm that predated the feature and **F79** was a
/// caller that discarded the new return value, and both would have
/// survived a test that called the arm directly.
///
/// **Each route is asserted on what the recap *contains*** (T4-S8's
/// vacuity result), and on the withholding that made a recap necessary:
/// a buffer that forwarded output as it arrived would fail the
/// mid-attachment assertion while passing every claim about the header.
#[cfg(unix)]
#[test]
#[serial]
fn every_detach_route_replays_the_foreground_recap_under_one_torii_header() {
    /// Attach to `id`, produce two foreground meta lines, and return the
    /// probe's view *while still attached* -- which must be the attach
    /// milestone and nothing else.
    fn attach_and_produce(
        probe: &SurfaceProbe,
        attached: &mut Option<u64>,
        id: u64,
    ) -> Vec<String> {
        apply_agent_view_exit(&attaching_exit(id), attached);
        assert_eq!(*attached, Some(id), "control: the binding took");
        foreground_meta("* read src/main.rs");
        foreground_meta("* edited src/main.rs");
        probe.lines()
    }

    /// What the probe must hold after a detach from `id`: the attach
    /// milestone, then the recap header, then the two buffered lines in
    /// the order the foreground produced them.
    ///
    /// The attach milestone is in the **prefix** and never in the recap:
    /// it was painted before the attachment began, and \u{a7}4.5's clause
    /// is about what the foreground produced *while* attached.
    fn expected_after_detach(id: u64) -> Vec<String> {
        vec![
            kitsu_render::milestone_line(&attach_milestone_message(id)),
            kitsu_render::milestone_line(&detach_recap_message(id)),
            "* read src/main.rs".to_string(),
            "* edited src/main.rs".to_string(),
        ]
    }

    // ---- route 1 of 3: `\u{2190}` on an empty composer ----
    {
        let probe = SurfaceProbe::install();
        let mut attached: Option<u64> = None;
        let during = attach_and_produce(&probe, &mut attached, 7);
        assert_eq!(
            during,
            vec![kitsu_render::milestone_line(&attach_milestone_message(7))],
            "A5: while attached the foreground's output is buffered, not \
                 painted over the agent view the user is reading"
        );

        let outcome = one_composer_read(attached, b"\x1b[D");
        let tui_bridge::InlineRead::Detached(draft) = outcome else {
            panic!("A5: `\u{2190}` on an empty composer detaches: {outcome:?}");
        };
        let carried = apply_inline_detach(&mut attached, draft);
        assert_eq!(attached, None, "control: the arm cleared the binding");
        assert_eq!(carried, "", "control: `\u{2190}` carries no draft");
        assert_eq!(
            probe.lines(),
            expected_after_detach(7),
            "design \u{a7}4.5: the `\u{2190}` route replays what the \
                 foreground produced, headed by its `\u{26E9}` milestone"
        );
    }

    // ---- route 2 of 3: the chord `Ctrl+X d`, with a draft typed ----
    {
        let probe = SurfaceProbe::install();
        let mut attached: Option<u64> = None;
        attach_and_produce(&probe, &mut attached, 11);

        let outcome = one_composer_read(attached, b"half a line\x18d");
        let tui_bridge::InlineRead::Detached(draft) = outcome else {
            panic!("F76: `Ctrl+X d` while attached detaches: {outcome:?}");
        };
        let carried = apply_inline_detach(&mut attached, draft);
        assert_eq!(
            carried, "half a line",
            "control, F76: the chord still carries the draft -- the recap \
                 is painted *and* the line is kept"
        );
        assert_eq!(
            probe.lines(),
            expected_after_detach(11),
            "design \u{a7}4.5: the chord route replays the same recap"
        );
    }

    // ---- route 3 of 3: `/detach` ----
    {
        let probe = SurfaceProbe::install();
        let mut attached: Option<u64> = None;
        attach_and_produce(&probe, &mut attached, 13);

        let outcome = apply_detach_command(&mut attached);
        assert_eq!(
            outcome,
            DetachCommandOutcome::Detached(13),
            "control: the command detached"
        );
        assert_eq!(attached, None);
        assert_eq!(
            probe.lines(),
            expected_after_detach(13),
            "F79/F77: the command route is a third route and replays too \
                 -- its own answer is the slash reply, which is not painted \
                 through this seam"
        );
    }
}

/// **A detach with nothing buffered paints nothing at all.**
///
/// **FAILING-FIRST** in the sense that matters: it passes today and must
/// still pass after the implementation, and the implementation that gets
/// it wrong is the obvious one -- paint the header, then the lines.
///
/// **The decision, and why.** \u{a7}4.5 makes the milestone the thing the
/// recap is *"replayed with"*: it is the recap's **header**, not an
/// announcement of the detach. And \u{a7}4.5 settles the announcement
/// separately -- *"the two keys are silent and the command answers"* --
/// because `\u{2190}` and `Ctrl+X d` give their feedback through the
/// prompt label changing under the caret, where the eye already is. A
/// bare `\u{26E9} detached from agent N \u{2014} recap:` with nothing
/// under it would break that silence rule **and** read as a bug: a
/// colon promising content that never came.
///
/// Non-vacuous against the test above, which pins that the same three
/// routes *do* paint the header when the buffer holds anything.
#[cfg(unix)]
#[test]
#[serial]
fn a_detach_with_nothing_buffered_paints_no_recap_header() {
    for id in [3_u64, 5] {
        let probe = SurfaceProbe::install();
        let mut attached: Option<u64> = None;
        apply_agent_view_exit(&attaching_exit(id), &mut attached);
        let attach_only = vec![kitsu_render::milestone_line(&attach_milestone_message(id))];
        assert_eq!(probe.lines(), attach_only, "control: the attach announced");

        // No foreground output at all: the user attached, read the view,
        // and came back.
        if id == 3 {
            let outcome = one_composer_read(attached, b"\x1b[D");
            let tui_bridge::InlineRead::Detached(draft) = outcome else {
                panic!("control: `\u{2190}` detaches: {outcome:?}");
            };
            apply_inline_detach(&mut attached, draft);
        } else {
            apply_detach_command(&mut attached);
        }
        assert_eq!(attached, None, "control: detached");
        assert_eq!(
            probe.lines(),
            attach_only,
            "an empty recap has no header: the `\u{26E9}` line is the \
                 recap's header and there is no recap"
        );
    }
}

/// **The recap carries nothing from before the attachment** (design
/// \u{a7}4.5), including across a re-attachment.
///
/// **FAILING-FIRST.**
///
/// Two gaps this closes. Output produced *before* attaching belongs to
/// the foreground the user was already watching and has already been
/// painted -- replaying it would duplicate it. And a second attachment
/// must start empty: a buffer kept across a detach would replay the
/// previous attachment's output under the new agent's header, attributing
/// one agent's turn to another.
#[cfg(unix)]
#[test]
#[serial]
fn the_recap_holds_only_what_the_foreground_produced_while_attached() {
    let probe = SurfaceProbe::install();
    let mut attached: Option<u64> = None;

    // Before any attachment: ordinary foreground output, painted at once.
    foreground_meta("* before the attach");
    assert_eq!(
        probe.lines(),
        vec!["* before the attach".to_string()],
        "control: with nothing attached the foreground paints normally"
    );

    // First attachment, with output, detached by `/detach`.
    apply_agent_view_exit(&attaching_exit(7), &mut attached);
    foreground_meta("* during the first attach");
    apply_detach_command(&mut attached);

    // Second attachment, to a different agent, with its own output.
    apply_agent_view_exit(&attaching_exit(9), &mut attached);
    foreground_meta("* during the second attach");
    apply_detach_command(&mut attached);

    assert_eq!(
        probe.lines(),
        vec![
            "* before the attach".to_string(),
            kitsu_render::milestone_line(&attach_milestone_message(7)),
            kitsu_render::milestone_line(&detach_recap_message(7)),
            "* during the first attach".to_string(),
            kitsu_render::milestone_line(&attach_milestone_message(9)),
            kitsu_render::milestone_line(&detach_recap_message(9)),
            "* during the second attach".to_string(),
        ],
        "each recap holds its own attachment's output and no other's"
    );
}

/// **Re-attaching to a different agent is leaving the first one, and
/// replays its recap before announcing the second** (**F79**: a fourth
/// route out of an attachment, found by the brief's own enumeration
/// coming up one short).
///
/// **FAILING-FIRST.**
///
/// \u{a7}4.5 names three detach routes and `apply_agent_view_exit` is a
/// fourth way an attachment ends: `Enter` in the view on a *different*
/// agent re-binds without any detach key being pressed. Whatever the
/// first attachment buffered has to go somewhere, and the only two
/// options are "replayed" and "silently dropped" -- and a buffer whose
/// whole purpose is that the foreground is not lost cannot drop it.
///
/// **The order is the assertion.** The recap is painted under the
/// *first* agent's header, before the second agent's attach milestone,
/// or a reader would attribute agent 7's turn to agent 9.
#[cfg(unix)]
#[test]
#[serial]
fn re_attaching_to_another_agent_replays_the_first_ones_recap_first() {
    let probe = SurfaceProbe::install();
    let mut attached: Option<u64> = None;

    apply_agent_view_exit(&attaching_exit(7), &mut attached);
    foreground_meta("* agent 7's attachment");
    apply_agent_view_exit(&attaching_exit(9), &mut attached);
    assert_eq!(attached, Some(9), "control: the binding moved");
    foreground_meta("* agent 9's attachment");
    apply_detach_command(&mut attached);

    assert_eq!(
        probe.lines(),
        vec![
            kitsu_render::milestone_line(&attach_milestone_message(7)),
            kitsu_render::milestone_line(&detach_recap_message(7)),
            "* agent 7's attachment".to_string(),
            kitsu_render::milestone_line(&attach_milestone_message(9)),
            kitsu_render::milestone_line(&detach_recap_message(9)),
            "* agent 9's attachment".to_string(),
        ],
        "a re-attachment leaves the first agent, and its recap is headed \
             by the agent it belongs to"
    );
}

/// **The overflow note reaches the user**, not just the buffer
/// (**F52**, **F71**).
///
/// **FAILING-FIRST.**
///
/// The byte bound and the note are pinned at unit level in
/// `kitsu-render`; this is the claim that the note is *painted*, which is
/// the half F71 exists for. A recap that computed the note and replayed
/// only the surviving lines would pass every unit test and still show
/// the user a hole with nothing marking it.
#[cfg(unix)]
#[test]
#[serial]
fn an_overflowing_recap_paints_its_overflow_note_under_the_header() {
    let probe = SurfaceProbe::install();
    let mut attached: Option<u64> = None;
    apply_agent_view_exit(&attaching_exit(4), &mut attached);

    // 1 KiB a line, two buffers' worth: the bound must cost it lines.
    const LINE: usize = 1024;
    let line = |n: usize| format!("{n:04} {}", "q".repeat(LINE - 5));
    let pushed = 2 * kitsu_render::surface::RECAP_BUFFER_BYTES / LINE;
    for n in 0..pushed {
        foreground_meta(&line(n));
    }
    apply_detach_command(&mut attached);

    let painted = probe.lines();
    assert_eq!(
        painted.first().map(String::as_str),
        Some(kitsu_render::milestone_line(&attach_milestone_message(4)).as_str()),
        "control: {painted:?}"
    );
    assert_eq!(
        painted.get(1).map(String::as_str),
        Some(kitsu_render::milestone_line(&detach_recap_message(4)).as_str()),
        "the header still heads it"
    );
    let kept = painted.len() - 3;
    assert_eq!(
        painted.get(2).map(String::as_str),
        Some(kitsu_render::surface::recap_overflow_note(pushed - kept).as_str()),
        "F52: the note sits directly under the header, where the dropped \
             output was, and says how much went"
    );
    assert!(
        kept > 0 && kept < pushed,
        "non-vacuous: the recap kept some of the output and lost some: \
             {kept} of {pushed}"
    );
    assert_eq!(
        painted.last().map(String::as_str),
        Some(line(pushed - 1).as_str()),
        "and what it kept ends at the newest line, which is what a user \
             returning to the foreground is looking for"
    );
}
