//! The inline viewport's persistent status row and its queue preview rows
//! (plan row T3-S5, requirement **R5**).
//!
//! # This module composes nothing
//!
//! **FAILING-FIRST.** Every item this module's tests reach for is unbuilt.
//! The tests are the specification; the shape below is the contract they pin.
//!
//! R5's row carries model, context meter, elapsed turn time, `[N queued]` and
//! the ambient background-agent tag. Three of those five are **already
//! composed**, for the classic live row, in `kitsu-render`:
//!
//! - the ambient tag — `kitsu_render::live::agents_tag`, with its
//!   blocked-outranks-running truth table pinned by
//!   `agents_tag_truth_table_ranks_blocked_over_running` (`live.rs:1458`);
//! - the `[N queued]` tag — composed inline in `compose_row` (`live.rs:253`);
//! - the queue preview rows — `kitsu_render::live::compose_stack_rows`
//!   (`live.rs:602`), already clamped per row so a preview cannot wrap.
//!
//! A second copy of any of those here would be the fourth duplication this
//! epic has refused (F2, F38, F45, and T3-S3's "the trait is a seam, not a
//! source"), and `live.rs:115-123` already names this exact collision: the
//! viewport's bottom rows are "exactly where the spinner, draft, queue tag and
//! ambient agent tag belong, so running both would be duplication before it
//! is a conflict".
//!
//! So this module holds **no composition**. It is a caller:
//!
//! 1. `kitsu_render::live::compose_status_row(&StatusFields, cols)` — the one
//!    pure composer, which must live in `kitsu-render` beside `compose_row`
//!    because `kitsu-tui` depends on `kitsu-render` and not the reverse, so
//!    the classic row can reach it later and the TUI row cannot be the only
//!    home for it. Genuinely new there: the `model`/meter/elapsed segments
//!    (classic has no standalone status row — its tags ride the prompt row,
//!    and its model and meter appear only in `/context` and the spinner) and
//!    the drop order.
//! 2. `kitsu_render::live::compose_stack_rows` — reused verbatim.
//! 3. `kitsu_render::surface::Surface::queue` — the new seam, which must be a
//!    **defaulted** trait method writing nothing, so `ClassicSurface` is
//!    byte-identical: classic's previews come from `live_queue_state`, not
//!    from the seam.
//!
//! # The `←` glyph is not ours yet
//!
//! R5 permits `← N agents` "once A1's left-arrow affordance exists, and not
//! before". `[tui] left_arrow_opens_agents` is resolved at `main.rs:16565`
//! and consumed by nothing; there is no left-arrow binding and the agent pane
//! is T4. The row therefore renders the **bracketed** form, exactly as
//! classic does, and `agents_tag` is the single source of that text.

use kitsu_render::surface::{Fd, Surface};

/// Everything R5's status row shows, as data.
///
/// Re-exported, not redeclared: the type lives in `kitsu_render::live` beside
/// `LiveDraft`, for the same reason the composer does — `kitsu-tui` depends on
/// `kitsu-render` and not the reverse, so the classic row can reach both.
pub use kitsu_render::live::StatusFields;

/// Paint R5's status row and its queue preview rows onto `surface`.
///
/// This module composes nothing. The row is composed whole by
/// [`kitsu_render::live::compose_status_row`] and the previews by
/// [`kitsu_render::live::compose_stack_rows`]; the previews reach the backend
/// through the [`Surface::queue`] seam, so a backend with no pinned bottom
/// rows (classic) ignores them while the inline viewport pins them above the
/// row.
pub fn paint_status(
    surface: &dyn Surface,
    fields: &StatusFields,
    previews: &[String],
    cols: usize,
) {
    let row = kitsu_render::live::compose_status_row(fields, cols);
    let rows = kitsu_render::live::compose_stack_rows(previews, fields.queued, cols);
    surface.queue(&rows);
    surface.transcript_line(Fd::Stderr, &row);
}

#[cfg(test)]
mod tests {
    use super::*;
    use kitsu_render::live::{agents_tag, compose_stack_rows, compose_status_row};
    use std::time::Duration;

    /// Visible width of a styled row, the in-tree idiom (`viewport.rs:1175`):
    /// sanitise the escapes away, then measure.
    fn visible_cols(row: &str) -> usize {
        kitsu_render::sanitize_terminal_text(row).chars().count()
    }

    fn plain(row: &str) -> String {
        kitsu_render::sanitize_terminal_text(row)
    }

    /// A busy, realistic row: every segment present and none of them short.
    fn busy() -> StatusFields {
        StatusFields {
            model: "claude-sonnet-4-5-20250929".to_string(),
            used: 104_000,
            effective: 160_000,
            elapsed: Some(Duration::from_secs(95)),
            queued: 4,
            blocked: 2,
            running: 5,
            ..Default::default()
        }
    }

    /// **The acceptance test.** Every width from 40 to 200 columns, not
    /// samples: the row's rendered width never exceeds the terminal.
    ///
    /// Positive half, so "no wrap" cannot pass vacuously (five findings in
    /// that family): the row must be non-empty at every width, and at every
    /// width it must still carry the ambient tag — the segment the drop order
    /// keeps last. An empty string trivially does not wrap.
    #[test]
    fn status_row_never_wraps_across_a_width_sweep() {
        let cases = [
            busy(),
            StatusFields {
                queued: 0,
                blocked: 0,
                running: 0,
                ..busy()
            },
            StatusFields {
                elapsed: None,
                blocked: 0,
                ..busy()
            },
            StatusFields {
                model: "q".to_string(),
                used: 0,
                effective: 8_192,
                elapsed: None,
                running: 1,
                ..Default::default()
            },
        ];
        for cols in 40usize..=200 {
            for (index, fields) in cases.iter().enumerate() {
                let row = compose_status_row(fields, cols);
                let width = visible_cols(&row);
                assert!(
                    width <= cols,
                    "case {index}: row is {width} cols wide at {cols} cols: {row:?}"
                );
                assert!(
                    width > 0,
                    "case {index}: a status row must render something at {cols} cols"
                );
                if let Some(tag) = agents_tag(fields.blocked, fields.running) {
                    assert!(
                        plain(&row).contains(tag.trim_end()),
                        "case {index}: the ambient tag is dropped last and must \
                         survive {cols} cols: {row:?}"
                    );
                }
            }
        }
    }

    /// The drop order, pinned as what the row **is** at each width rather
    /// than as a ban on a substring (F41).
    ///
    /// Derivation — and this is a derivation, not a quotation: **R5 states no
    /// drop order.** It comes from the classic row's own precedent, which the
    /// shared composer must not contradict:
    ///
    /// - `compose_row` (`live.rs:264`): "The spinner yields the row to the
    ///   draft when both cannot fit" — decoration yields to content.
    /// - `narrow_terminal_drops_spinner_but_keeps_draft_and_agents_tag`
    ///   (`live.rs:1641`): the tags outlive the spinner.
    /// - `agents_tag` (`live.rs:226`): blocked is "the thing the user must
    ///   act on", so the ambient tag is the last segment standing.
    ///
    /// First dropped to last kept: **meter, elapsed, model, `[N queued]`,
    /// ambient tag.** The meter goes first because `/context` renders it on
    /// demand; elapsed next because the spinner already carries elapsed past
    /// five seconds (`lib.rs:1554`); the model is truncated before it is
    /// dropped, being long and static.
    #[test]
    fn status_row_drops_segments_in_the_documented_order() {
        let fields = busy();
        // Width is swept downward; each segment, once gone, stays gone, and
        // the segments still present are exactly the suffix of the order.
        let order = ["meter", "elapsed", "model", "queued", "agents"];
        let present = |row: &str, segment: &str| -> bool {
            let row = plain(row);
            match segment {
                // The meter is the only segment made of cells.
                "meter" => row.contains('■') || row.contains('□'),
                "elapsed" => row.contains("1m35s"),
                // Truncation keeps a prefix, so the model counts as present
                // while any of it survives.
                "model" => row.contains("claude"),
                "queued" => row.contains("[4 queued]"),
                "agents" => row.contains("[2 blocked]"),
                _ => unreachable!(),
            }
        };
        let mut dropped: Vec<&str> = Vec::new();
        for cols in (40usize..=200).rev() {
            let row = compose_status_row(&fields, cols);
            for segment in order {
                if !present(&row, segment) && !dropped.contains(&segment) {
                    dropped.push(segment);
                }
            }
            // Whatever has been dropped so far must be a prefix of the order:
            // nothing later may go before something earlier.
            assert_eq!(
                dropped.as_slice(),
                &order[..dropped.len()],
                "at {cols} cols the drop order is violated: {row:?}"
            );
            for segment in &dropped {
                assert!(
                    !present(&row, segment),
                    "at {cols} cols {segment:?} came back after being dropped: {row:?}"
                );
            }
        }
        // Not vacuous: a 40-column terminal cannot hold all five, so the
        // sweep must actually have dropped something, and the ambient tag
        // must never be among the casualties.
        assert!(
            !dropped.is_empty(),
            "the sweep never narrowed enough to drop a segment"
        );
        assert!(
            !dropped.contains(&"agents"),
            "the ambient tag is last in the order and 40 cols still holds it"
        );
    }

    /// R5's row content, at a width that holds all of it: the five segments
    /// are there, and the ambient tag is **`agents_tag`'s own text** rather
    /// than a second rendering of the same rule.
    #[test]
    fn status_row_carries_every_segment_requirement_r5_names() {
        let row = plain(&compose_status_row(&busy(), 200));
        assert!(row.contains("claude-sonnet-4-5-20250929"), "model: {row:?}");
        assert!(row.contains('■'), "context meter cells: {row:?}");
        assert!(row.contains("65%"), "meter utilisation: {row:?}");
        assert!(row.contains("1m35s"), "elapsed turn time: {row:?}");
        assert!(row.contains("[4 queued]"), "queue depth: {row:?}");
        assert_eq!(
            agents_tag(2, 5).as_deref(),
            Some("[2 blocked] "),
            "the expectation reused, not reinvented"
        );
        assert!(row.contains("[2 blocked]"), "ambient tag: {row:?}");
    }

    /// The ambient tag's precedence and its zero case are **not** re-derived
    /// here: the row shows exactly what `agents_tag` returns, for every entry
    /// of the truth table `agents_tag_truth_table_ranks_blocked_over_running`
    /// (`live.rs:1458`) already pins. Blocked outranks running; both zero is
    /// no tag at all.
    #[test]
    fn status_row_defers_to_agents_tag_for_the_ambient_counter() {
        for &(blocked, running) in &[
            (0usize, 0usize),
            (0, 1),
            (0, 2),
            (0, 7),
            (1, 0),
            (3, 0),
            (2, 5),
            (1, 1),
        ] {
            let fields = StatusFields {
                blocked,
                running,
                ..busy()
            };
            let row = plain(&compose_status_row(&fields, 200));
            match agents_tag(blocked, running) {
                Some(tag) => assert!(
                    row.contains(tag.trim_end()),
                    "({blocked},{running}) must render {tag:?}: {row:?}"
                ),
                None => {
                    // Positive form, not a substring ban: with no agents the
                    // row equals the row composed with the tag's counts at
                    // zero *and* nothing bracketed but the queue tag remains.
                    assert!(
                        !row.contains("blocked") && !row.contains("agent"),
                        "both counts zero hides the tag entirely: {row:?}"
                    );
                    assert!(
                        row.contains("[4 queued]"),
                        "hiding the ambient tag must not take its neighbours: {row:?}"
                    );
                }
            }
        }
    }

    /// `← N agents` is forbidden until A1's left-arrow affordance exists
    /// (R5). Pinned as what the row **is**: the ambient tag is `agents_tag`'s
    /// bracketed text, byte for byte, so no arrow form can creep in without
    /// this equality failing.
    #[test]
    fn the_ambient_tag_is_the_bracketed_form_until_a1_lands() {
        let fields = StatusFields {
            blocked: 0,
            running: 3,
            ..busy()
        };
        let row = plain(&compose_status_row(&fields, 200));
        let tag = agents_tag(0, 3).expect("three running agents carry a tag");
        assert_eq!(tag, "[3 agents] ");
        assert!(row.contains(tag.trim_end()), "{row:?}");
    }

    // -----------------------------------------------------------------------
    // Card **T4-S6-status**, requirement A5 / design §4.5: the
    // `attached · agent N` segment.
    //
    // **FAILING-FIRST.** `StatusFields::attached` exists (the field is this
    // card's authorised production edit) and `compose_status_row` ignores it,
    // so every test below is red until the segment is composed.
    //
    // Every assertion goes through a **painting** surface — `paint_status`,
    // which is the only thing in this crate that puts the row in front of a
    // user, and `compose_status_row`, which is the string it paints. Finding
    // **F71**: a test that reads the attachment back off `StatusFields` would
    // pass against a row that never mentions it.
    //
    // The row's form is pinned as a **derivation** of the detached row rather
    // than a retyped literal (F41): the attached row is the detached row with
    // one segment in front of it, so the expectation cannot drift from the
    // other four segments and the segment's position is pinned too.
    // -----------------------------------------------------------------------

    /// What a user sees: the bytes `paint_status` hands the surface.
    ///
    /// The row goes out as a `transcript_line` on stderr (`paint_status`), so
    /// this is the one line of the painted pair that is the status row.
    fn painted_row(fields: &StatusFields, cols: usize) -> String {
        let backend = LineBackend::new();
        paint_status(&backend, fields, &[], cols);
        let lines = backend.lines();
        assert_eq!(lines.len(), 1, "paint_status paints exactly one row");
        assert_eq!(lines[0].0, Fd::Stderr, "the status row is a stderr row");
        plain(&lines[0].1)
    }

    /// The segment is **painted**, and it **names the agent** — so a user
    /// attached to one of several can tell which (design §4.5).
    ///
    /// Pinned as what the painted row *is*: the row composed from the same
    /// fields with nothing attached, with `attached · agent N` in front of
    /// it. Two different ids, because an assertion that passes for any id
    /// has not pinned that the row names *this* one.
    #[test]
    fn paint_status_paints_the_attachment_segment_naming_the_agent() {
        let detached = painted_row(&busy(), 200);
        assert!(
            detached.contains("claude-sonnet-4-5-20250929") && detached.contains("[2 blocked]"),
            "the baseline row must be the whole row, or the derivation below \
             compares nothing: {detached:?}"
        );
        for id in [7u64, 12] {
            let fields = StatusFields {
                attached: Some(id),
                ..busy()
            };
            assert_eq!(
                painted_row(&fields, 200),
                format!("attached \u{b7} agent {id} \u{b7} {detached}"),
                "design §4.5: the status row shows `attached \u{b7} agent {id}`, \
                 leftmost, ahead of the row it already painted"
            );
        }
    }

    /// Absent when nothing is attached — with the positive control that makes
    /// the absence non-vacuous: the row still renders, and still carries its
    /// other four segments.
    ///
    /// Stated as an equality against the whole row rather than a substring
    /// ban (F41): there is no id, no `agent` and no `attached` anywhere in it
    /// because the row *is* exactly the four-segment row.
    #[test]
    fn the_attachment_segment_is_absent_when_nothing_is_attached() {
        let fields = busy();
        assert_eq!(fields.attached, None, "the fixture is the detached case");
        let row = painted_row(&fields, 200);
        // The control: the row is painted and complete.
        assert!(row.contains("claude-sonnet-4-5-20250929"), "model: {row:?}");
        assert!(row.contains("65%"), "meter: {row:?}");
        assert!(row.contains("1m35s"), "elapsed: {row:?}");
        assert!(row.contains("[4 queued]"), "queue tag: {row:?}");
        assert!(row.contains("[2 blocked]"), "ambient tag: {row:?}");
        // And the segment is not in it: the detached row equals the attached
        // row with the segment and its separator removed, so nothing of it
        // can be hiding elsewhere in the row.
        let attached = painted_row(
            &StatusFields {
                attached: Some(7),
                ..busy()
            },
            200,
        );
        assert_eq!(
            attached
                .strip_prefix("attached \u{b7} agent 7 \u{b7} ")
                .expect("the attached row carries the segment"),
            row,
            "detaching removes the segment and nothing else"
        );
    }

    /// Where the segment sits in the row's priority, which **design §4.5 does
    /// not state**. Decided here, and the reasoning is the card's own:
    ///
    /// - it **outranks `[N queued]`**, the model, elapsed and the meter,
    ///   because the `←`-detaches-on-the-first-press rule rests on the
    ///   attachment being legible *at the moment of the press* (§4.5's
    ///   amendment), and a segment a narrow terminal drops takes that
    ///   argument with it;
    /// - it **yields to the ambient tag**, which stays last standing: a
    ///   blocked agent is the thing the user must act on (`agents_tag`),
    ///   while losing the label costs one keystroke and stops nothing.
    ///
    /// So the row claims room for it **second**, right after the ambient tag,
    /// and that is what the exact assertion below pins: with the ambient tag
    /// present, the attachment is painted at exactly the widths where the tag
    /// plus a separator plus the label fit, and at no others. Nothing
    /// lower-ranked can take its room, and it can never outlive the tag.
    ///
    /// # Why it is pinned that way and not as the prefix property
    ///
    /// `status_row_drops_segments_in_the_documented_order` pins the drop set
    /// as a prefix of the order and bans revivals. Neither that nor a
    /// per-width "if the queue tag survives, so does the attachment" is
    /// **satisfiable** here, and the reason is pre-existing rather than this
    /// segment's: each segment is offered what the higher-ranked ones left,
    /// so a *narrower* low-ranked segment can fit where a wider high-ranked
    /// one could not. Measured against a prototype of this row: dropping
    /// `[4 queued]` at 44 columns frees thirteen columns and a **clipped
    /// model reappears**, and below 32 columns the ten-column queue tag is
    /// painted where the eighteen-column attachment cannot be. Asserting
    /// either of those away would be an unsatisfiable assertion (F61), so
    /// this test pins the claim order instead — exactly — and says nothing
    /// about the model's revival in either direction.
    ///
    /// The sweep runs from 20 columns, not 40, so it covers widths where the
    /// attachment itself is dropped; otherwise "it yields to the ambient tag"
    /// would be vacuous.
    #[test]
    fn the_attachment_segment_claims_room_second_after_the_ambient_tag() {
        const ID: u64 = 7;
        let fields = StatusFields {
            attached: Some(ID),
            ..busy()
        };
        let label = format!("attached \u{b7} agent {ID}");
        let tag = agents_tag(fields.blocked, fields.running)
            .expect("the fixture has blocked agents")
            .trim_end()
            .to_string();
        // `STATUS_SEP` is private to `kitsu_render::live`; the row joins any
        // two pieces with it and charges three columns for the join.
        let sep = " \u{b7} ".chars().count();
        let needed = tag.chars().count() + sep + label.chars().count();
        let mut kept = 0usize;
        let mut dropped = 0usize;
        let mut took_the_queue_tags_room = 0usize;
        for cols in (20usize..=200).rev() {
            let row = painted_row(&fields, cols);
            let width = visible_cols(&row);
            assert!(
                width <= cols,
                "the attached row is {width} cols wide at {cols} cols: {row:?}"
            );
            assert!(
                width > 0,
                "a status row must render something at {cols} cols: {row:?}"
            );
            let painted = row.contains(&label);
            assert_eq!(
                painted,
                cols >= needed,
                "at {cols} cols the attachment must be painted exactly when \
                 the ambient tag, a separator and the label fit ({needed} \
                 cols): it claims room second and nothing lower-ranked can \
                 take it: {row:?}"
            );
            if painted {
                assert!(
                    row.contains(&tag),
                    "and it can never outlive the ambient tag: {row:?}"
                );
                kept += 1;
                // The displacement that makes "outranks" observable: this
                // width has room for the queue tag without the attachment,
                // and the attachment has it instead.
                let without = painted_row(&busy(), cols);
                if without.contains("[4 queued]") && !row.contains("[4 queued]") {
                    took_the_queue_tags_room += 1;
                }
            } else {
                dropped += 1;
            }
        }
        // Non-vacuous three ways: widths that keep it, widths that drop it
        // (so the equality is a real boundary and not a constant), and at
        // least one width where it is painted *instead of* the queue tag.
        assert!(kept > 0, "no width in the sweep painted the attachment");
        assert!(dropped > 0, "no width in the sweep dropped it");
        assert!(
            took_the_queue_tags_room > 0,
            "no width showed the attachment taking room the queue tag would \
             otherwise have had, so the ranking between them was never \
             exercised"
        );
    }

    // -----------------------------------------------------------------------
    // `Surface::queue`, the new seam.
    // -----------------------------------------------------------------------

    /// A line-only backend: it implements `transcript_line` and nothing
    /// else, so it exercises the defaults of `write_block` and `queue`.
    /// `Surface` is `Send + Sync`, so the log sits behind a `Mutex`.
    struct LineBackend(std::sync::Mutex<Vec<(Fd, String)>>);

    impl LineBackend {
        fn new() -> Self {
            Self(std::sync::Mutex::new(Vec::new()))
        }
        fn lines(&self) -> Vec<(Fd, String)> {
            self.0.lock().unwrap().clone()
        }
    }

    impl Surface for LineBackend {
        fn transcript_line(&self, fd: Fd, text: &str) {
            self.0.lock().unwrap().push((fd, text.to_string()));
        }
    }

    /// The seam's default must write **nothing**: classic's queue previews
    /// come from the live row's own `live_queue_state`, so routing them
    /// through the seam as transcript lines would scroll them into the
    /// transcript and change classic's bytes.
    ///
    /// Stated positively: the recorder's line log is identical before and
    /// after a `queue` call carrying real rows.
    #[test]
    fn the_default_queue_seam_emits_no_transcript_lines() {
        let backend = LineBackend::new();
        backend.transcript_line(Fd::Stderr, "a receipt");
        let before = backend.lines();
        backend.queue(&[
            "  ↳ next: first".to_string(),
            "  ↳ then: second".to_string(),
        ]);
        assert_eq!(
            backend.lines(),
            before,
            "a backend without bottom rows must ignore the queue seam"
        );
        assert_eq!(
            before.len(),
            1,
            "and the log must have held something, or this compares nothing"
        );
    }

    /// The inline viewport's override: the rows handed to `queue` are the
    /// rows `compose_stack_rows` composed, verbatim and in order — the seam
    /// carries them, it does not re-render them.
    #[test]
    fn paint_status_hands_the_seam_compose_stack_rows_output_verbatim() {
        struct QueueSpy(std::sync::Mutex<Vec<Vec<String>>>);
        impl Surface for QueueSpy {
            fn transcript_line(&self, _fd: Fd, _text: &str) {}
            fn queue(&self, rows: &[String]) {
                self.0.lock().unwrap().push(rows.to_vec());
            }
        }
        let previews = vec![
            "fix the parser".to_string(),
            "then run the suite".to_string(),
            "then write the docs".to_string(),
        ];
        let fields = StatusFields {
            queued: 5,
            ..busy()
        };
        let spy = QueueSpy(std::sync::Mutex::new(Vec::new()));

        paint_status(&spy, &fields, &previews, 80);

        let seen = spy.0.lock().unwrap().clone();
        assert_eq!(
            seen,
            vec![compose_stack_rows(&previews, 5, 80)],
            "the seam receives kitsu-render's rows, unmodified"
        );
        assert!(
            !seen[0].is_empty(),
            "five queued prompts compose at least one preview row"
        );
    }

    /// The preview rows obey the same no-wrap bound as the status row, across
    /// the same 40..=200 sweep. `compose_stack_rows` already clamps per row
    /// (`live.rs:607`); this pins that the seam inherits it rather than
    /// re-truncating at a second width.
    #[test]
    fn queue_preview_rows_never_wrap_across_a_width_sweep() {
        let previews = vec![
            "a very long queued prompt that will not fit a narrow terminal".to_string(),
            "a second queued prompt, also comfortably too long".to_string(),
            "third".to_string(),
        ];
        for cols in 40usize..=200 {
            let rows = compose_stack_rows(&previews, 9, cols);
            assert!(
                !rows.is_empty(),
                "nine queued prompts must compose preview rows at {cols} cols"
            );
            for row in &rows {
                let width = visible_cols(row);
                assert!(
                    width <= cols,
                    "preview row is {width} cols wide at {cols}: {row:?}"
                );
            }
        }
    }
}
