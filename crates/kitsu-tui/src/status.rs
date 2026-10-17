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
                queued: 0,
                blocked: 0,
                running: 1,
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
