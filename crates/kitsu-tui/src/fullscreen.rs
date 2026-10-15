//! The fullscreen transcript renderer over the cell ring (plan row **T5-S1**,
//! requirement **N4**).
//!
//! This module is the cell ring's **first reader**. Until it existed the ring
//! was fed only by surfaces constructed in tests, because N4 declines to spend
//! up to `cell_ring_lines` styled lines on a ring nothing reads; the reader
//! therefore lands before the production feed, not after it.
//!
//! # What this module is, and what it is not
//!
//! It is a **pure view**: given a [`CellRing`] and a window of `rows` x `cols`,
//! [`FullscreenView::frame`] returns the rows to paint, oldest first. It owns
//! no terminal, writes no bytes and enters no alternate screen — entering the
//! alternate screen is [`crate::TuiRuntime`]'s, and the runtime gets its first
//! caller from the renderer switch that selects this mode. Keeping the view
//! pure is what lets the follow and paging behaviour be asserted through the
//! painted rows (finding **F71**) rather than from the anchor field.
//!
//! # Memory: the bound is the ring's, and it is in lines
//!
//! N4's promise is that memory is bounded by the ring "not by conversation
//! length", and the bound is in **lines, not bytes**: a single pathological
//! line still costs whatever it costs, which is finding **F52** and a
//! deliberate trade. This module adds no second bound and keeps no copy of
//! the ring, so the retained-line count after any number of appended lines is
//! the ring's `limit` and nothing here can grow with the conversation.
//!
//! # The anchor's transitions, not its keys
//!
//! Finding **F84**: enumerating the routes out of a state is not enumerating
//! the transitions out of it. Four keys page this view; the anchor has
//! **eleven** transitions, three of which no key triggers at all.
//!
//! | # | from | on | to |
//! |---|---|---|---|
//! | 1 | `Follow` | `Up`, with lines above the window | `Pinned` |
//! | 2 | `Follow` | `Up`, ring fits the window | `Follow` (nothing above to reveal) |
//! | 3 | `Pinned` | `Up` | `Pinned`, clamped at the oldest retained line |
//! | 4 | `Pinned` | `Down`, landing short of the newest line | `Pinned` |
//! | 5 | `Pinned` | `Down`, reaching the newest line | `Follow` (the re-latch) |
//! | 6 | `Follow` | `Down` | `Follow` |
//! | 7 | `Follow` | `Oldest` | `Pinned` at the oldest retained line |
//! | 8 | `Pinned` | `Oldest` | `Pinned` at the oldest retained line |
//! | 9 | `Pinned` | `Newest` | `Follow` |
//! | 10 | `Follow` | a line is appended | `Follow`, and the frame moves to it |
//! | 11 | `Pinned` | a line is **evicted** past the anchor | `Pinned`, frame clamped back into the ring |
//!
//! Transitions 5, 10 and 11 are the ones no key names. 5 is the auto-follow
//! re-latch: paging down off the end must resume following, or the view
//! silently stops updating one page short of the bottom. 10 is auto-follow
//! itself. 11 is the one the ring forces: the anchor is an **absolute** line
//! ordinal, stable across appends, and the ring is a sliding window, so a
//! view pinned on a line the ring has since evicted must move — the line it
//! was showing no longer exists anywhere in the process.

use crate::cells::CellRing;

/// How far one `Up`/`Down` press moves, given a window of `rows` rows.
///
/// A full window minus one row of overlap, so the row the user was reading at
/// the edge is still on screen after the press; a one-row window pages by one
/// row rather than by zero, which would make the key inert.
pub fn page_step(rows: u16) -> usize {
    usize::from(rows).saturating_sub(1).max(1)
}

/// The four presses that move this view.
///
/// Named by what they do, not by their key: the key column belongs in
/// [`crate::keys`], and the binary's decoder maps `PgUp`, `PgDn`, `Ctrl+Home`
/// and `Ctrl+End` onto these.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Page {
    /// `PgUp`: one window back towards the oldest retained line.
    Up,
    /// `PgDn`: one window on towards the newest line.
    Down,
    /// `Ctrl+Home`: the oldest line the ring still retains.
    Oldest,
    /// `Ctrl+End`: the newest line, and resume following.
    Newest,
}

/// Where the frame is anchored.
///
/// `Pinned` carries an **absolute** line ordinal — counted over the ring's
/// whole lifetime, evicted lines included — and not an index into the retained
/// lines. An index would drift under eviction while the anchor appeared
/// unchanged, which is exactly transition 11 going unnoticed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Anchor {
    Follow,
    Pinned { top: usize },
}

/// The fullscreen transcript view: an anchor into a [`CellRing`].
#[derive(Clone, Debug)]
pub struct FullscreenView {
    anchor: Anchor,
}

impl Default for FullscreenView {
    fn default() -> Self {
        Self::new()
    }
}

impl FullscreenView {
    /// A view that follows the newest line.
    pub fn new() -> Self {
        Self {
            anchor: Anchor::Follow,
        }
    }

    /// Whether the view is following the newest line, so an appended line
    /// enters the frame (transition 10).
    pub fn is_following(&self) -> bool {
        matches!(self.anchor, Anchor::Follow)
    }

    /// The absolute ordinal of the oldest line `ring` still retains.
    fn oldest(ring: &CellRing) -> usize {
        ring.evicted()
    }

    /// The largest anchor that still fills the window from the newest line:
    /// the anchor the follow position is equivalent to.
    fn follow_top(ring: &CellRing, rows: u16) -> usize {
        let newest_end = ring.evicted() + ring.len();
        newest_end
            .saturating_sub(usize::from(rows))
            .max(Self::oldest(ring))
    }

    /// Apply one press against `ring` on a `rows`-high window.
    pub fn page(&mut self, press: Page, ring: &CellRing, rows: u16) {
        let oldest = Self::oldest(ring);
        let follow_top = Self::follow_top(ring, rows);
        let step = page_step(rows);
        self.anchor = match (self.anchor, press) {
            // Transition 9 and the `Follow` half of 8's sibling: the end key
            // always resumes following, from either state.
            (_, Page::Newest) => Anchor::Follow,
            // Transitions 7 and 8: the home key always pins, from either
            // state, even when the whole ring is on screen -- "stop following
            // and show the top" is right whether or not the top is visible.
            (_, Page::Oldest) => Anchor::Pinned { top: oldest },
            // Transition 6.
            (Anchor::Follow, Page::Down) => Anchor::Follow,
            // Transitions 1 and 2: there is something above the window only
            // when the follow anchor is past the oldest line.
            (Anchor::Follow, Page::Up) => {
                if follow_top > oldest {
                    Anchor::Pinned {
                        top: follow_top.saturating_sub(step).max(oldest),
                    }
                } else {
                    Anchor::Follow
                }
            }
            // Transition 3.
            (Anchor::Pinned { top }, Page::Up) => Anchor::Pinned {
                top: top.max(oldest).saturating_sub(step).max(oldest),
            },
            // Transitions 4 and 5: reaching the newest line re-latches
            // following, which is the transition no key names.
            (Anchor::Pinned { top }, Page::Down) => {
                let next = top.max(oldest).saturating_add(step);
                if next >= follow_top {
                    Anchor::Follow
                } else {
                    Anchor::Pinned { top: next }
                }
            }
        };
    }

    /// The rows to paint, oldest first: at most `rows` lines from `ring`,
    /// each fitted to `cols` columns.
    ///
    /// Lines are handed on as the **ring's styled bytes**, SGR included,
    /// because N4 makes the ring the only copy and this renderer repaints
    /// them. Fitting is delegated to [`kitsu_render::fit_row_to_width`], which
    /// returns a row that fits byte-identically and terminates a row it had to
    /// cut with one `ESC[0m` — findings **F52**/**F84b**: a cut that severs an
    /// SGR sequence colours every row drawn after it.
    pub fn frame(&self, ring: &CellRing, rows: u16, cols: usize) -> Vec<String> {
        let lines = ring.lines();
        if rows == 0 || lines.is_empty() {
            return Vec::new();
        }
        let oldest = Self::oldest(ring);
        let top = match self.anchor {
            Anchor::Follow => Self::follow_top(ring, rows),
            // Transition 11, in the one place it can be enforced: the anchor
            // is clamped into the ring *for the frame*, so a view pinned on an
            // evicted line shows the oldest line that still exists rather than
            // nothing at all.
            Anchor::Pinned { top } => top.clamp(oldest, Self::follow_top(ring, rows)),
        };
        let start = top - oldest;
        lines
            .iter()
            .skip(start)
            .take(usize::from(rows))
            .map(|line| kitsu_render::fit_row_to_width(line.styled(), cols))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::{page_step, FullscreenView, Page};
    use crate::cells::CellRing;
    use kitsu_render::surface::Fd;

    /// A ring of `limit` lines fed `count` numbered lines.
    fn ring_of(limit: usize, count: usize) -> CellRing {
        let mut ring = CellRing::new(limit);
        for n in 0..count {
            ring.push(Fd::Stdout, &format!("line {n}"));
        }
        ring
    }

    /// Requirement **N4**, the plan row's acceptance test, as a *line* bound
    /// rather than an RSS measurement.
    ///
    /// N4's promise is that memory is bounded by the ring "**not by
    /// conversation length**", and its T3-S6 amendment says the bound is in
    /// lines, not bytes, with a per-line clamp explicitly declined (**F52**).
    /// An RSS assertion would therefore measure a property the requirement has
    /// disclaimed, in a unit (resident pages) that allocator reuse and test
    /// parallelism make nondeterministic. What is both achievable and
    /// meaningful is the structural claim: the retained line count, and the
    /// *bytes the ring itself holds*, are a function of the bound and not of
    /// how many lines the conversation produced.
    ///
    /// Not vacuous: the count after 5 000 lines is compared against the count
    /// after 10 000, so an unbounded `Vec` fails on the second; the eviction
    /// accounting is asserted to conserve every line, so a ring that dropped
    /// lines without counting them fails; and the frame is asserted to be a
    /// window of real content, so a renderer that paints nothing fails.
    #[test]
    fn five_thousand_lines_leave_the_ring_at_its_bound_and_the_frame_at_the_window() {
        const LIMIT: usize = 200;
        const ROWS: u16 = 24;
        let view = FullscreenView::new();

        let mut ring = ring_of(LIMIT, 5_000);
        assert_eq!(ring.len(), LIMIT, "5 000 lines retain the bound, not 5 000");
        assert_eq!(ring.evicted(), 5_000 - LIMIT);
        assert_eq!(
            ring.len() + ring.evicted(),
            5_000,
            "every line is either retained or counted as evicted"
        );
        let after_five_thousand = ring.len();
        let bytes_after_five_thousand: usize =
            ring.lines().iter().map(|line| line.styled().len()).sum();

        for n in 5_000..10_000 {
            ring.push(Fd::Stdout, &format!("line {n}"));
        }
        assert_eq!(
            ring.len(),
            after_five_thousand,
            "doubling the conversation does not grow the ring: N4's bound is \
             the ring's, not the conversation's"
        );
        assert_eq!(ring.len() + ring.evicted(), 10_000);
        let bytes_after_ten_thousand: usize =
            ring.lines().iter().map(|line| line.styled().len()).sum();
        assert_eq!(
            bytes_after_ten_thousand, bytes_after_five_thousand,
            "the bytes the ring holds are the retained lines' and no others: \
             the evicted lines' bytes are not kept anywhere"
        );

        let frame = view.frame(&ring, ROWS, 80);
        assert_eq!(
            frame.len(),
            usize::from(ROWS),
            "the frame is the window, not the ring and not empty"
        );
        assert_eq!(
            frame.last().map(String::as_str),
            Some("line 9999"),
            "and it ends on the newest line"
        );
    }

    /// Auto-follow, asserted through the painted rows (**F71**): a following
    /// view's frame is the newest `rows` lines, and an appended line enters it
    /// while the oldest row leaves (transition 10).
    ///
    /// Not vacuous: the frame before the append is pinned exactly, so a
    /// renderer that painted a constant, an empty frame, or the *oldest*
    /// lines fails before the append is even made.
    #[test]
    fn a_following_frame_is_the_newest_rows_and_an_appended_line_enters_it() {
        let mut ring = ring_of(100, 10);
        let view = FullscreenView::new();
        assert!(view.is_following(), "a new view follows");
        assert_eq!(
            view.frame(&ring, 3, 80),
            vec!["line 7", "line 8", "line 9"],
            "the newest three lines, oldest first"
        );

        ring.push(Fd::Stdout, "line 10");
        assert_eq!(
            view.frame(&ring, 3, 80),
            vec!["line 8", "line 9", "line 10"],
            "the appended line entered the frame and the oldest row left"
        );
    }

    /// A ring holding fewer lines than the window is painted whole, with no
    /// padding rows invented: the painter clears the rows it owns.
    #[test]
    fn a_frame_taller_than_the_ring_paints_only_what_the_ring_holds() {
        let ring = ring_of(100, 2);
        let view = FullscreenView::new();
        assert_eq!(view.frame(&ring, 10, 80), vec!["line 0", "line 1"]);
        assert_eq!(
            FullscreenView::new().frame(&CellRing::new(8), 10, 80),
            Vec::<String>::new(),
            "an empty ring paints nothing"
        );
    }

    /// Transition 1 and transition 9: `Up` stops following and moves the
    /// frame back, and `Newest` resumes following from wherever it got to.
    ///
    /// Not vacuous: the paged frame is asserted to be *different* content, by
    /// value, so a `page` that only flipped a flag fails.
    #[test]
    fn paging_up_stops_following_and_ctrl_end_resumes_it() {
        let ring = ring_of(100, 10);
        let mut view = FullscreenView::new();
        let following = view.frame(&ring, 4, 80);
        assert_eq!(following, vec!["line 6", "line 7", "line 8", "line 9"]);

        view.page(Page::Up, &ring, 4);
        assert!(!view.is_following(), "paging up pins the frame");
        let paged = view.frame(&ring, 4, 80);
        assert_eq!(
            paged,
            vec!["line 3", "line 4", "line 5", "line 6"],
            "one window back, overlapping by the row at the edge"
        );
        assert_ne!(paged, following, "the frame actually moved");

        view.page(Page::Newest, &ring, 4);
        assert!(view.is_following(), "the end key resumes following");
        assert_eq!(view.frame(&ring, 4, 80), following);
    }

    /// Transition 10 is suppressed while pinned: an appended line must not
    /// move a pinned frame, or the view scrolls out from under the reader.
    #[test]
    fn a_line_appended_while_pinned_does_not_move_the_frame() {
        let mut ring = ring_of(100, 10);
        let mut view = FullscreenView::new();
        view.page(Page::Up, &ring, 4);
        let pinned = view.frame(&ring, 4, 80);
        assert_eq!(pinned, vec!["line 3", "line 4", "line 5", "line 6"]);

        for n in 10..20 {
            ring.push(Fd::Stdout, &format!("line {n}"));
        }
        assert_eq!(
            view.frame(&ring, 4, 80),
            pinned,
            "ten appended lines left a pinned frame exactly where it was"
        );
        assert!(!view.is_following());
    }

    /// **Transition 5, the one no key names:** paging down until the newest
    /// line is reached resumes following, rather than stopping one window
    /// short with a view that silently never updates again.
    ///
    /// Not vacuous: following is asserted to be *live* afterwards by appending
    /// a line and seeing it in the frame, so a `page` that merely set the flag
    /// without restoring the follow anchor fails.
    #[test]
    fn paging_down_onto_the_newest_line_re_latches_following() {
        let mut ring = ring_of(100, 12);
        let mut view = FullscreenView::new();
        view.page(Page::Oldest, &ring, 4);
        assert!(!view.is_following());
        assert_eq!(
            view.frame(&ring, 4, 80),
            vec!["line 0", "line 1", "line 2", "line 3"]
        );

        // Short of the newest line: still pinned (transition 4).
        view.page(Page::Down, &ring, 4);
        assert!(
            !view.is_following(),
            "one window down is not the bottom yet"
        );
        assert_eq!(
            view.frame(&ring, 4, 80),
            vec!["line 3", "line 4", "line 5", "line 6"]
        );

        // Down twice more reaches the newest line (transition 5).
        view.page(Page::Down, &ring, 4);
        view.page(Page::Down, &ring, 4);
        assert!(
            view.is_following(),
            "reaching the newest line resumes following"
        );
        ring.push(Fd::Stdout, "line 12");
        assert_eq!(
            view.frame(&ring, 4, 80).last().map(String::as_str),
            Some("line 12"),
            "and following is live, not just a flag"
        );
    }

    /// Transitions 2, 3 and 6: the three presses that must be clamped rather
    /// than wrapped or saturating into nonsense.
    ///
    /// Covers the degenerate shape the ring makes easy to get wrong -- a ring
    /// that fits entirely in the window has nothing above it, so `Up` has
    /// nowhere to go and must leave the view following rather than pinning it
    /// to a position it is already at.
    #[test]
    fn paging_past_either_end_is_clamped_and_idempotent() {
        let ring = ring_of(100, 10);

        // Transition 3, clamped at the oldest retained line.
        let mut view = FullscreenView::new();
        for _ in 0..50 {
            view.page(Page::Up, &ring, 4);
        }
        assert_eq!(
            view.frame(&ring, 4, 80),
            vec!["line 0", "line 1", "line 2", "line 3"],
            "fifty page-ups stop at the oldest line"
        );
        assert!(!view.is_following());

        // Transition 6, and the follow frame is unchanged by it.
        let mut following = FullscreenView::new();
        let before = following.frame(&ring, 4, 80);
        for _ in 0..50 {
            following.page(Page::Down, &ring, 4);
        }
        assert!(following.is_following());
        assert_eq!(following.frame(&ring, 4, 80), before);

        // Transition 2: the whole ring is on screen, so there is nothing
        // above the window to reveal.
        let short = ring_of(100, 3);
        let mut fits = FullscreenView::new();
        let whole = fits.frame(&short, 10, 80);
        assert_eq!(whole, vec!["line 0", "line 1", "line 2"]);
        fits.page(Page::Up, &short, 10);
        assert!(
            fits.is_following(),
            "with nothing above the window, page-up leaves the view following"
        );
        assert_eq!(fits.frame(&short, 10, 80), whole);
    }

    /// Transitions 7 and 8: `Ctrl+Home` reaches the oldest line the ring
    /// **still retains**, which is not line zero of the conversation once the
    /// ring has evicted -- N4 names `/export` as the route to what it evicted,
    /// and this key must not pretend otherwise.
    #[test]
    fn ctrl_home_shows_the_oldest_line_the_ring_still_retains() {
        let ring = ring_of(10, 1_000);
        assert_eq!(ring.evicted(), 990);
        let mut view = FullscreenView::new();
        view.page(Page::Oldest, &ring, 4);
        assert_eq!(
            view.frame(&ring, 4, 80),
            vec!["line 990", "line 991", "line 992", "line 993"],
            "the oldest retained line, not line 0, which is gone"
        );

        // Transition 8: pressing it again from a pinned state is idempotent.
        view.page(Page::Oldest, &ring, 4);
        assert_eq!(
            view.frame(&ring, 4, 80),
            vec!["line 990", "line 991", "line 992", "line 993"]
        );
    }

    /// **Transition 11, the one the ring forces and no key triggers:** a
    /// pinned frame whose lines are evicted out from under it must come back
    /// inside the ring, because the lines it was showing no longer exist
    /// anywhere in the process.
    ///
    /// Not vacuous: the pinned frame is asserted by value before the eviction,
    /// and afterwards every painted row is asserted to be a line the ring
    /// still retains -- so a view that kept painting the evicted ordinals
    /// (or panicked indexing past the retained lines) fails.
    #[test]
    fn eviction_under_a_pinned_frame_brings_it_back_inside_the_ring() {
        let mut ring = ring_of(8, 8);
        let mut view = FullscreenView::new();
        view.page(Page::Oldest, &ring, 3);
        assert_eq!(view.frame(&ring, 3, 80), vec!["line 0", "line 1", "line 2"]);

        // Evict everything the pinned frame was showing, and more.
        for n in 8..16 {
            ring.push(Fd::Stdout, &format!("line {n}"));
        }
        assert_eq!(ring.evicted(), 8, "the pinned lines are gone");
        let frame = view.frame(&ring, 3, 80);
        let retained: Vec<String> = ring.lines().iter().map(|line| line.plain()).collect();
        assert_eq!(frame.len(), 3, "the frame still fills the window");
        for row in &frame {
            assert!(
                retained.contains(row),
                "every painted row is a line the ring still retains; {row:?} \
                 is not among {retained:?}"
            );
        }
        assert_eq!(
            frame,
            vec!["line 8", "line 9", "line 10"],
            "clamped to the oldest line that still exists"
        );
        assert!(
            !view.is_following(),
            "being clamped is not the same as being released: the view is \
             still pinned, so it does not start chasing the newest line"
        );
    }

    /// N4: a stored line is the surface's bytes verbatim, styling included,
    /// and this renderer repaints those rows -- so the frame carries the SGR
    /// through rather than the stripped form.
    ///
    /// Not vacuous: the fixture is asserted to carry ESC before the
    /// comparison, and the stripped form is asserted *not* to be what comes
    /// back, so a renderer calling `CellLine::plain` fails.
    #[test]
    fn the_frame_carries_the_rings_styled_bytes_and_not_the_stripped_form() {
        let styled = "\x1b[1;32mdone\x1b[0m tool";
        assert!(styled.contains('\x1b'), "the fixture carries SGR bytes");
        let mut ring = CellRing::new(4);
        ring.push(Fd::Stderr, styled);
        let frame = FullscreenView::new().frame(&ring, 4, 80);
        assert_eq!(frame, vec![styled.to_string()], "byte for byte");
        assert_ne!(
            frame,
            vec!["done tool".to_string()],
            "the stripped form is the `[` dump's, not the frame's"
        );
    }

    /// Findings **F52**/**F84b**: a row too wide for the window is cut, and a
    /// cut row that carried SGR ends in a reset, or the severed sequence
    /// colours every row painted after it.
    ///
    /// Asserted as *what the output is* (**F41**) by comparing against
    /// `fit_row_to_width`'s own answer: this renderer must delegate the fit
    /// rather than grow a second width policy that can drift from the one the
    /// inline viewport paints with.
    #[test]
    fn an_over_wide_styled_row_is_cut_to_the_window_and_ends_reset() {
        let long = format!("\x1b[31m{}\x1b[0m", "x".repeat(200));
        let mut ring = CellRing::new(4);
        ring.push(Fd::Stdout, &long);
        let frame = FullscreenView::new().frame(&ring, 1, 20);
        assert_eq!(
            frame,
            vec![kitsu_render::fit_row_to_width(&long, 20)],
            "the fit is delegated, not reimplemented"
        );
        let row = &frame[0];
        assert!(
            row.ends_with("\x1b[0m"),
            "a cut row that carried SGR must end reset, got {:?}",
            row.escape_debug().to_string()
        );
        assert_eq!(
            row.chars().filter(|ch| *ch == 'x').count(),
            20,
            "exactly the window's columns of visible text"
        );
    }

    /// A zero-row window paints nothing and does not panic: the fullscreen
    /// renderer shares the terminal-too-short case the inline viewport's F19
    /// bound already has, and a renderer that indexed with a zero height
    /// would be a division or an empty-range panic on the first resize.
    #[test]
    fn a_zero_row_window_paints_nothing() {
        let ring = ring_of(100, 10);
        let mut view = FullscreenView::new();
        assert_eq!(view.frame(&ring, 0, 80), Vec::<String>::new());
        view.page(Page::Up, &ring, 0);
        view.page(Page::Down, &ring, 0);
        assert_eq!(view.frame(&ring, 0, 80), Vec::<String>::new());
    }

    /// The step is a window minus the overlap row, and never zero: a one-row
    /// window that paged by zero would make `PgUp` inert, which is the shape
    /// of key advertised but doing nothing that T4-S10's sweep exists to
    /// catch.
    #[test]
    fn the_page_step_is_a_window_less_the_overlap_and_never_zero() {
        assert_eq!(page_step(24), 23);
        assert_eq!(page_step(2), 1);
        assert_eq!(page_step(1), 1, "a one-row window still pages");
        assert_eq!(page_step(0), 1, "and a degenerate one cannot page by zero");
    }

    /// The ring's **first reader** reads what the surface actually wrote.
    ///
    /// N4 declines to feed the ring in production until something reads it,
    /// which makes this renderer the gate on that feed; this test pins the
    /// seam the feed will arrive through -- 5 000 transcript lines through
    /// `InlineSurface::with_cell_ring` retain the bound and no more.
    ///
    /// Not vacuous: the sink is asserted to have carried all 5 000 lines'
    /// bytes, so the bounded retention is measured on a surface that really
    /// did write an unbounded conversation.
    #[test]
    fn five_thousand_surface_lines_retain_only_the_configured_bound() {
        use crate::viewport::InlineSurface;
        use kitsu_render::surface::Surface;
        use std::io::Write;
        use std::sync::{Arc, Mutex};

        #[derive(Clone, Default)]
        struct CountingSink(Arc<Mutex<usize>>);
        impl Write for CountingSink {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                *self.0.lock().unwrap() += buf.iter().filter(|b| **b == b'\n').count();
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        const LIMIT: usize = 200;
        let sink = CountingSink::default();
        let surface =
            InlineSurface::with_cell_ring(Box::new(sink.clone()), Box::new(sink.clone()), LIMIT);
        for n in 0..5_000 {
            surface.transcript_line(Fd::Stdout, &format!("line {n}"));
        }
        assert_eq!(
            *sink.0.lock().unwrap(),
            5_000,
            "all 5 000 lines reached the terminal"
        );
        let cells = surface.cells().expect("a ring was asked for");
        assert_eq!(cells.len(), LIMIT, "and only the bound is retained");
        assert_eq!(
            cells.last().map(|line| line.plain()),
            Some("line 4999".to_string())
        );
    }
}
