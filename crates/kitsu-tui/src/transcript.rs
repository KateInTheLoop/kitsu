//! The transcript overlay's pure view over the cell ring (requirement **R6**,
//! first piece: the overlay exists, scrolls and closes).
//!
//! # What this module is
//!
//! A **pure view**, for [`crate::fullscreen`]'s reason: given a [`CellRing`]
//! and a window of `rows` x `cols`, [`TranscriptOverlay::frame`] returns the
//! rows to paint, oldest first, with the overlay's own footer row last. It
//! owns no terminal, writes no bytes, takes no alternate screen and spawns no
//! child: all of that is the binary's, which is what lets this behaviour be
//! asserted through the painted rows (finding **F71**).
//!
//! # One anchor implementation, two instances
//!
//! The overlay scrolls the same ring the fullscreen frame does, and it does
//! **not** grow a second anchor: it holds a [`FullscreenView`] and delegates
//! every scroll to it, so the eleven transitions in that module's table are
//! the overlay's transitions too, with no second copy to drift from them.
//!
//! Two *instances* of one implementation is not the drift this epic has
//! findings about — that is two implementations. The instances must be
//! separate, because the overlay's scroll position and the frame behind it
//! are different facts: paging the overlay must not move the frame the user
//! goes back to when the overlay closes, and the overlay is reachable from
//! the inline surface, where there is no fullscreen frame to share an anchor
//! with in the first place.
//!
//! # The footer advertises only what the overlay dispatches
//!
//! `keys::BINDINGS` is the **agent view's** table, guarded by a sweep over
//! *that* decoder's key space (**F82**), and the fullscreen paging keys were
//! deliberately kept out of it because they are live only while fullscreen
//! holds the screen (T5-S1c-iii). The overlay's keys are live only while the
//! overlay holds the screen, so they are advertised the same way the paging
//! keys are: **in situ**, on the surface that dispatches them, by
//! [`TranscriptOverlay::footer`] — and never a key this piece does not
//! dispatch, which is finding **F89** in the direction that bites.

use crate::cells::CellRing;
use crate::fullscreen::{FullscreenView, Page};
use crate::keys::Press;

/// The transcript overlay: a [`FullscreenView`] over the ring plus the
/// overlay's own chrome.
#[derive(Clone, Debug, Default)]
pub struct TranscriptOverlay {
    view: FullscreenView,
}

impl TranscriptOverlay {
    /// The overlay's key row, in the one wording it is painted with.
    ///
    /// Spelled here, beside the dispatch it describes, for the reason the
    /// module header gives: these keys are live only while the overlay holds
    /// the screen, exactly as the fullscreen paging keys are, so they are
    /// advertised **in situ** and not in `keys::BINDINGS`, whose guard is a
    /// sweep over the agent view's decoder (**F82**).
    ///
    /// It names the four paging presses [`Self::page`] forwards and the two
    /// presses [`closes`] answers, and nothing else: `/` search, `{`/`}`
    /// prompt jumps, `[` dump and `v` editor are later pieces of **R6**, and
    /// a row that named one of them before anything dispatched it would be
    /// finding **F89** painted onto the screen.
    const FOOTER: &'static str = "PgUp/PgDn scroll  Ctrl+Home/End oldest/newest  q/Esc close";

    /// An overlay that follows the newest line.
    pub fn new() -> Self {
        Self {
            view: FullscreenView::new(),
        }
    }

    /// Whether the overlay is following the newest line.
    pub fn is_following(&self) -> bool {
        self.view.is_following()
    }

    /// How many of a `rows`-high window are transcript, the footer row having
    /// taken one.
    ///
    /// The footer comes **out** of the window rather than over a transcript
    /// row: a row drawn on top of the content would silently cost the user a
    /// line of the thing the overlay was opened to read.
    pub fn content_rows(rows: u16) -> u16 {
        rows.saturating_sub(1)
    }

    /// Apply one scroll press against `ring` on a `rows`-high window.
    ///
    /// Delegated whole, against [`Self::content_rows`] and not `rows`: the
    /// window the anchor pages over is the one the frame paints, so a press
    /// handed the full height would step one row further than the user can
    /// see.
    pub fn page(&mut self, press: Page, ring: &CellRing, rows: u16) {
        self.view.page(press, ring, Self::content_rows(rows));
    }

    /// The rows to paint, oldest first, with the footer last.
    pub fn frame(&self, ring: &CellRing, rows: u16, cols: usize) -> Vec<String> {
        if rows == 0 {
            return Vec::new();
        }
        let mut rows_out = self.view.frame(ring, Self::content_rows(rows), cols);
        rows_out.push(self.footer(cols));
        rows_out
    }

    /// The overlay's key row, fitted to `cols` columns.
    ///
    /// The fit is [`kitsu_render::fit_row_to_width`]'s, as every other row on
    /// these surfaces is: a footer that wrapped would cost the transcript row
    /// above it on a screen the frame fills.
    pub fn footer(&self, cols: usize) -> String {
        kitsu_render::fit_row_to_width(Self::FOOTER, cols)
    }
}

/// Whether `press` closes the overlay: `q` and `Esc`, and nothing else.
///
/// Deliberately not `Enter`, which is the agent view's attach, and not the
/// arrows: this overlay has no selection for them to move.
pub fn closes(press: Press) -> bool {
    matches!(press, Press::Char('q') | Press::Esc)
}

#[cfg(test)]
mod tests {
    use super::{closes, TranscriptOverlay};
    use crate::cells::CellRing;
    use crate::fullscreen::{FullscreenView, Page};
    use crate::keys::Press;
    use kitsu_render::surface::Fd;

    /// A ring of `limit` lines fed `count` numbered lines, as
    /// `fullscreen`'s own tests build one.
    fn ring_of(limit: usize, count: usize) -> CellRing {
        let mut ring = CellRing::new(limit);
        for n in 0..count {
            ring.push(Fd::Stdout, &format!("line {n}"));
        }
        ring
    }

    /// Every press the overlay's decoder can produce, as an enumeration of the
    /// **keyboard** rather than of the dispatch's arms: `Press::Char` is
    /// `0x20..=0x7e` by construction, exactly as the agent view's sweep
    /// (**F82**) relies on.
    fn every_press() -> Vec<Press> {
        let mut presses: Vec<Press> = (0x20u8..=0x7e).map(|b| Press::Char(b as char)).collect();
        presses.extend([
            Press::Up,
            Press::Down,
            Press::Enter,
            Press::Right,
            Press::Esc,
        ]);
        presses
    }

    /// The overlay opens on the newest lines and the footer row is **taken
    /// out of the window**, not drawn over a transcript row.
    ///
    /// The expected content is not invented: a 4-row window over ten lines is
    /// `line 6..line 9` in `fullscreen`'s own asserted behaviour, so a 5-row
    /// overlay must show those four and spend its fifth row on the footer.
    ///
    /// Not vacuous: the content rows are pinned by value, so an overlay that
    /// painted the oldest lines, a constant, or the whole ring fails before
    /// the footer is even looked at; and the row count is asserted against the
    /// window, so an overlay that quietly drew a sixth row fails too.
    #[test]
    fn the_overlay_opens_on_the_newest_lines_and_spends_one_row_on_its_footer() {
        let ring = ring_of(100, 10);
        let overlay = TranscriptOverlay::new();
        assert!(overlay.is_following(), "a new overlay follows");
        assert_eq!(
            TranscriptOverlay::content_rows(5),
            4,
            "the footer takes one row of the window"
        );

        let frame = overlay.frame(&ring, 5, 80);
        assert_eq!(frame.len(), 5, "the overlay fills its window exactly");
        assert_eq!(
            &frame[..4],
            ["line 6", "line 7", "line 8", "line 9"],
            "the newest four lines, oldest first"
        );
        assert_eq!(
            frame[4],
            overlay.footer(80),
            "and the last row is the footer, which is where the overlay's \
             keys are advertised"
        );
    }

    /// The overlay's anchor is **its own**: paging the overlay leaves the
    /// fullscreen frame behind it exactly where it was.
    ///
    /// This is the claim that makes two anchors over one ring legitimate. It
    /// is asserted on one ring with both views live, by value on both sides.
    ///
    /// Not vacuous: the overlay's frame is asserted to have actually *moved*
    /// (`assert_ne` against its own opening frame), so a `page` that did
    /// nothing at all would pass the "the frame did not move" half while
    /// failing this one.
    #[test]
    fn paging_the_overlay_does_not_move_the_fullscreen_frame_behind_it() {
        let ring = ring_of(100, 10);
        let frame_view = FullscreenView::new();
        let behind = frame_view.frame(&ring, 4, 80);
        assert_eq!(behind, ["line 6", "line 7", "line 8", "line 9"]);

        let mut overlay = TranscriptOverlay::new();
        let opened = overlay.frame(&ring, 5, 80);
        overlay.page(Page::Up, &ring, 5);
        let paged = overlay.frame(&ring, 5, 80);
        assert_ne!(paged, opened, "the overlay's own frame moved");
        assert_eq!(
            &paged[..4],
            ["line 3", "line 4", "line 5", "line 6"],
            "one window back over the 4 content rows"
        );
        assert!(!overlay.is_following(), "paging up pins the overlay");

        assert_eq!(
            frame_view.frame(&ring, 4, 80),
            behind,
            "the fullscreen frame is untouched by the overlay's paging: the \
             two anchors are separate instances of one implementation"
        );
        assert!(frame_view.is_following());
    }

    /// The overlay **delegates** scrolling rather than reimplementing it: over
    /// a scripted run of presses, its content rows are `FullscreenView`'s
    /// answer for the same ring and the same content height, row for row.
    ///
    /// This is the anti-drift assertion, and it is a comparison by value
    /// against the live implementation (**F97**): it cannot be satisfied by a
    /// second anchor that merely agrees on the opening frame, because the
    /// script walks the clamps at both ends and the auto-follow re-latch.
    ///
    /// Not vacuous: the two sides are asserted to be non-empty and to have
    /// taken at least two distinct values over the script, so a pair of views
    /// that both painted nothing — or both froze — cannot pass.
    #[test]
    fn the_overlay_scrolls_the_ring_through_the_fullscreen_views_transitions() {
        const ROWS: u16 = 6;
        const CONTENT: u16 = 5;
        let mut ring = ring_of(40, 40);
        let mut overlay = TranscriptOverlay::new();
        let mut reference = FullscreenView::new();

        let script = [
            Page::Up,
            Page::Up,
            Page::Down,
            Page::Oldest,
            Page::Up,
            Page::Down,
            Page::Down,
            Page::Down,
            Page::Down,
            Page::Down,
            Page::Down,
            Page::Down,
            Page::Newest,
            Page::Down,
        ];
        let mut seen: Vec<Vec<String>> = Vec::new();
        for (step, press) in script.into_iter().enumerate() {
            overlay.page(press, &ring, ROWS);
            reference.page(press, &ring, CONTENT);
            let content: Vec<String> = overlay.frame(&ring, ROWS, 80);
            let expected = reference.frame(&ring, CONTENT, 80);
            assert_eq!(
                content.len(),
                usize::from(ROWS),
                "step {step} ({press:?}): the overlay fills its window"
            );
            assert_eq!(
                &content[..usize::from(CONTENT)],
                expected.as_slice(),
                "step {step} ({press:?}): the overlay's content rows are the \
                 fullscreen view's answer for the same ring"
            );
            assert_eq!(
                overlay.is_following(),
                reference.is_following(),
                "step {step} ({press:?}): and so is its follow state"
            );
            seen.push(expected);
        }
        seen.dedup();
        assert!(
            seen.len() >= 2,
            "the script moved the frame to at least two distinct positions, \
             so the agreement above is not two frozen views agreeing"
        );

        // And the delegation holds across an append, which is the transition
        // no key names (transition 10).
        ring.push(Fd::Stdout, "line 40");
        assert_eq!(
            &overlay.frame(&ring, ROWS, 80)[..usize::from(CONTENT)],
            reference.frame(&ring, CONTENT, 80).as_slice(),
            "an appended line reaches both or neither"
        );
    }

    /// `q` and `Esc` close the overlay, and **nothing else does**, asserted by
    /// a total sweep over the key space the overlay's decoder can produce.
    ///
    /// The sweep is the agent view's answer to **F57** reused: it enumerates
    /// the keyboard, not the dispatch's arms, so it cannot agree with the
    /// implementation by reading it. It is also the guard against the two keys
    /// that would be easy to add by reflex and are wrong here — `Enter` (the
    /// agent view's attach) and `Up`/`Down`, which this overlay has no
    /// selection for.
    #[test]
    fn q_and_esc_close_the_overlay_and_no_other_press_does() {
        let closing: Vec<Press> = every_press().into_iter().filter(|p| closes(*p)).collect();
        assert_eq!(
            closing,
            vec![Press::Char('q'), Press::Esc],
            "exactly `q` and `Esc` close the overlay, over the whole key space"
        );
        assert!(closes(Press::Char('q')));
        assert!(closes(Press::Esc));
        assert!(
            !closes(Press::Enter),
            "`Enter` is the agent view's attach, not a close here"
        );
        assert!(!closes(Press::Char('Q')), "the close key is lower case");
    }

    /// The footer advertises what this piece **dispatches**, and no key it
    /// does not: a row that named `/`, `[`, `n` or `v` before anything
    /// answered them would be finding **F89** painted onto the screen.
    ///
    /// Asserted as painted bytes (**F71**) and in both directions, so a footer
    /// that advertised everything and a footer that advertised nothing both
    /// fail.
    #[test]
    fn the_footer_advertises_the_overlays_keys_and_no_key_it_does_not_answer() {
        let overlay = TranscriptOverlay::new();
        let footer = overlay.footer(80);
        assert!(
            !footer.is_empty(),
            "the overlay advertises its keys in situ"
        );
        for advertised in ["q", "Esc", "PgUp", "PgDn"] {
            assert!(
                footer.contains(advertised),
                "the footer names {advertised:?}, which this overlay \
                 dispatches; it reads {footer:?}"
            );
        }
        // The keys this piece does not answer are checked by **capability
        // name**, not by their letter, and that is not squeamishness: a bare
        // `/` is how a key row spells `PgUp/PgDn`, a bare `n` is in `PgDn`,
        // and a bare `v` is in any word with a v in it. Forbidding the
        // characters would reject correct footers, which is an assertion that
        // only its own fixture can satisfy (**F58**). The capability words are
        // what an over-eager row would actually say.
        let lowered = footer.to_ascii_lowercase();
        for unanswered in ["search", "dump", "editor", "scrollback", "jump"] {
            assert!(
                !lowered.contains(unanswered),
                "the footer must not advertise {unanswered:?}: nothing \
                 dispatches it yet; it reads {footer:?}"
            );
        }
        assert!(
            !footer.contains('['),
            "the footer must not name the `[` dump: it is not dispatched \
             here, and on this surface it would also be the one key the \
             alternate screen has nowhere to dump to; it reads {footer:?}"
        );
    }

    /// The footer is fitted to the window and never wraps, across a width
    /// sweep — the property `status` already holds its own row to, and for the
    /// same reason: a wrapped row on a screen the frame fills costs a
    /// transcript row.
    #[test]
    fn the_footer_fits_every_width_and_never_wraps() {
        let overlay = TranscriptOverlay::new();
        for cols in 1usize..=200 {
            let footer = overlay.footer(cols);
            // Before the fit is checked: an **empty** footer is fitted to
            // every width trivially, so without this the whole sweep is
            // satisfied by advertising nothing (finding **F99**).
            assert!(
                !footer.is_empty(),
                "the footer at {cols} columns still advertises something: a \
                 row that vanishes at narrow widths is a key the user cannot \
                 discover on the surface that dispatches it"
            );
            assert_eq!(
                footer,
                kitsu_render::fit_row_to_width(&footer, cols),
                "the footer at {cols} columns is already fitted, so the fit \
                 is delegated rather than reimplemented"
            );
            assert!(
                !footer.contains('\n') && !footer.contains('\r'),
                "the footer is one row at {cols} columns: {footer:?}"
            );
        }
    }

    /// The degenerate windows: an empty ring, and a window with no room for
    /// anything but the footer. Neither may panic, and neither may invent
    /// transcript rows the ring does not hold.
    #[test]
    fn an_empty_ring_and_a_one_row_window_paint_no_invented_transcript_rows() {
        let overlay = TranscriptOverlay::new();

        let empty = CellRing::new(8);
        assert_eq!(
            overlay.frame(&empty, 5, 80),
            vec![overlay.footer(80)],
            "an empty ring paints the footer and nothing else"
        );

        let ring = ring_of(100, 10);
        assert_eq!(
            TranscriptOverlay::content_rows(1),
            0,
            "a one-row window is all footer"
        );
        assert_eq!(
            overlay.frame(&ring, 1, 80),
            vec![overlay.footer(80)],
            "and it paints the footer rather than a transcript row the user \
             cannot read a second line of"
        );
        assert_eq!(
            overlay.frame(&ring, 0, 80),
            Vec::<String>::new(),
            "a zero-row window paints nothing at all"
        );
    }

    /// N4: the overlay repaints the ring's **styled** bytes, as the fullscreen
    /// frame does, and not the stripped form `plain` derives for the `[` dump.
    ///
    /// Not vacuous: the fixture is asserted to carry ESC first, and the
    /// stripped form is asserted not to be what comes back.
    #[test]
    fn the_overlays_content_rows_carry_the_rings_styled_bytes() {
        let styled = "\x1b[1;32mdone\x1b[0m tool";
        assert!(styled.contains('\x1b'), "the fixture carries SGR bytes");
        let mut ring = CellRing::new(4);
        ring.push(Fd::Stderr, styled);
        let frame = TranscriptOverlay::new().frame(&ring, 3, 80);
        assert_eq!(
            frame.first().map(String::as_str),
            Some(styled),
            "byte for byte, as the ring received it"
        );
        assert_ne!(
            frame.first().map(String::as_str),
            Some("done tool"),
            "the stripped form is the `[` dump's, not the overlay's"
        );
    }
}
