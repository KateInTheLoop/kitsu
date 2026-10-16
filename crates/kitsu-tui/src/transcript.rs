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
use crate::fullscreen::{FullscreenView, Jump, Page};
use crate::keys::Press;

/// The transcript overlay: a [`FullscreenView`] over the ring plus the
/// overlay's own chrome.
#[derive(Clone, Debug, Default)]
pub struct TranscriptOverlay {
    view: FullscreenView,
    /// The query being typed, or `None` when no query line is open. The
    /// query line is a *mode*: while it is `Some`, the surface's own keys --
    /// `q` included -- are text.
    query: Option<String>,
    /// The **absolute** ordinals the last submitted query matched, oldest
    /// first: the snapshot `n`/`N` walk. Absolute, because the ring is a
    /// sliding window and a stored index would address a different line after
    /// the next eviction.
    matches: Vec<usize>,
    /// Which of [`Self::matches`] the walk is standing on, or `None` when no
    /// query has matched.
    cursor: Option<usize>,
    /// The miss to report in the chrome row, kept after the query line closes
    /// so the user is told their query found nothing (**F75**).
    miss: Option<String>,
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
    /// It names the four paging presses [`Self::page`] forwards, the two
    /// prompt jumps [`Self::jump`] answers, the `/` search and `n`/`N` match
    /// walk [`Self::submit_search`] and [`Self::repeat_search`] answer, and
    /// the two presses [`closes`] answers -- and nothing else: the `[` dump
    /// and the `v` editor are later pieces of **R6**, and a row that named
    /// one of them before anything dispatched it would be finding **F89**
    /// painted onto the screen.
    /// Seventy-seven columns, which is the constraint that cost
    /// `Ctrl+Home/End` its `oldest/newest` gloss: at eighty columns the fit
    /// would have cut `q/Esc close` off the end, and a row that drops the key
    /// the user needs to leave the surface is worse than one that leaves a
    /// self-describing key pair to describe itself.
    const FOOTER: &'static str =
        "PgUp/PgDn scroll  {/} prompt  / search  n/N match  Ctrl+Home/End  q/Esc close";

    /// An overlay that follows the newest line.
    pub fn new() -> Self {
        Self {
            view: FullscreenView::new(),
            ..Self::default()
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

    /// Jump to the nearest prompt block in `jump`'s direction (requirement
    /// **R6-c**), and report whether there was one.
    ///
    /// Delegated whole to the held [`FullscreenView`], against
    /// [`Self::content_rows`] and not `rows`, for [`Self::page`]'s reason: the
    /// window a jump clamps against is the one the frame paints.
    pub fn jump(&mut self, jump: Jump, ring: &CellRing, rows: u16) -> bool {
        self.view.jump(jump, ring, Self::content_rows(rows))
    }

    /// **R6-b: the query line and the match walk.**
    ///
    /// These are the *policy* over [`FullscreenView::pin_to`]; `{`/`}` is the
    /// other one, and there is deliberately no second anchor concept.
    ///
    /// Open the query line (`/`), which scrolls nothing: the scan is
    /// [`Self::submit_search`]'s, and a per-keystroke scan is what this shape
    /// exists to avoid -- a full `plain()` pass over a default 10 000-line
    /// ring was measured at ~4 ms release / ~14 ms debug, and ~10 ms / ~45 ms
    /// with one **F52** pasted megabyte in it, so a twenty-character query
    /// would be most of a second of scanning for an answer the user has not
    /// asked for yet.
    pub fn begin_search(&mut self) {
        self.query = Some(String::new());
        // The previous query's miss is not this query's: the chrome row is
        // the query line from here until the submit.
        self.miss = None;
    }

    /// The query being typed, or `None` when no query line is open.
    ///
    /// This is also the **mode** the byte loop reads: while it is `Some`, the
    /// overlay's own keys are query text, or the word `query` could not be
    /// searched for and `q` would take the surface away mid-typing.
    pub fn search_query(&self) -> Option<&str> {
        self.query.as_deref()
    }

    /// Append one printable character to the open query line.
    pub fn push_search_char(&mut self, ch: char) {
        if let Some(query) = self.query.as_mut() {
            query.push(ch);
        }
    }

    /// Delete the last character of the open query line.
    ///
    /// Inert on an empty query rather than closing the query line: deleting
    /// past the start must not take the surface away behind the user.
    pub fn backspace_search(&mut self) {
        if let Some(query) = self.query.as_mut() {
            query.pop();
        }
    }

    /// Abandon the query line, leaving the overlay open and unmoved.
    pub fn cancel_search(&mut self) {
        self.query = None;
        self.miss = None;
    }

    /// Run the typed query (`Enter`) and jump to its newest match, reporting
    /// whether the frame moved.
    ///
    /// The **one** scan per query: the matching absolute ordinals are stored
    /// and [`Self::repeat_search`] walks them, so `n`/`N` are O(1) per press
    /// and allocate nothing. The consequence is that the match list is a
    /// snapshot — a line appended after the submit joins the next query's
    /// matches, not this walk's.
    ///
    /// The landing is the **newest** match, and the matching **line** rather
    /// than its prompt block: `/` moves to the text the user asked for, and
    /// the block start is a row their query may not appear in.
    ///
    /// An empty query is a **cancel**: `""` is a substring of every line in
    /// the ring, so matching all of them and jumping would be a keypress the
    /// user made by accident moving the frame.
    pub fn submit_search(&mut self, ring: &CellRing, rows: u16) -> bool {
        let Some(query) = self.query.take() else {
            return false;
        };
        self.miss = None;
        if query.is_empty() {
            self.matches.clear();
            self.cursor = None;
            return false;
        }
        // Case-insensitive: a transcript is prose, and a case-sensitive
        // default is the **F75** shape -- the user types what they remember
        // seeing and the key appears to do nothing.
        let needle = query.to_lowercase();
        // `plain()` is exactly the glyphs of the painted row, so what is
        // searched is what the overlay paints. The ordinals are **absolute**
        // (`evicted() + index`), which is what lets them outlive an eviction.
        let oldest = ring.evicted();
        self.matches = ring
            .lines()
            .iter()
            .enumerate()
            .filter(|(_, line)| line.plain().to_lowercase().contains(&needle))
            .map(|(index, _)| oldest + index)
            .collect();
        let Some(newest) = self.matches.len().checked_sub(1) else {
            self.cursor = None;
            // On screen, because the user has just typed a whole query and
            // "nothing happened" must not have to be inferred from a frame
            // that did not move (**F75**).
            self.miss = Some(format!("/{query}  no match"));
            return false;
        };
        self.cursor = Some(newest);
        self.pin_to_match(self.matches[newest], ring, rows)
    }

    /// Walk to the next match in `dir` (`n`/`N`), reporting whether the frame
    /// moved.
    ///
    /// `n` is **newer** and `N` is **older**, reusing R6-c's [`Jump`] rather
    /// than growing a second direction concept. **Neither wraps**: `{`/`}`
    /// already settled what "nothing in that direction" means on this surface
    /// -- nothing happens and `false` comes back -- and a wrap would be
    /// **F75** at its worst, with `n` at the newest match teleporting to the
    /// oldest and nothing on screen saying which had happened.
    pub fn repeat_search(&mut self, dir: Jump, ring: &CellRing, rows: u16) -> bool {
        let Some(at) = self.cursor else {
            return false;
        };
        let next = match dir {
            Jump::Next if at + 1 < self.matches.len() => at + 1,
            Jump::Prev if at > 0 => at - 1,
            _ => return false,
        };
        if self.pin_to_match(self.matches[next], ring, rows) {
            self.cursor = Some(next);
            true
        } else {
            false
        }
    }

    /// How many matches the last submitted query found.
    pub fn match_count(&self) -> usize {
        self.matches.len()
    }

    /// Pin the frame's top row to `ordinal`, obeying **R6-c's** cannot-move
    /// rule rather than inventing a second one for `/`.
    ///
    /// The clamp is [`FullscreenView::pin_to`]'s one home and the cursor is
    /// [`FullscreenView::top`]'s, so there is no arithmetic here: the pin is
    /// tried on a copy, and if the copy's top row is the one the frame is
    /// already on then this is no transition at all -- the view is left
    /// exactly as it was, follow state included, and `false` comes back.
    /// Pinning there would break the follow while the frame did not change,
    /// which is invisible at the keypress and visible later as output that
    /// silently stopped arriving (**F75**).
    fn pin_to_match(&mut self, ordinal: usize, ring: &CellRing, rows: u16) -> bool {
        let content = Self::content_rows(rows);
        let cursor = self.view.top(ring, content);
        let mut pinned = self.view.clone();
        pinned.pin_to(ordinal, ring, content);
        if pinned.top(ring, content) == cursor {
            return false;
        }
        self.view = pinned;
        true
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

    /// The overlay's **chrome** row, fitted to `cols` columns: the key row,
    /// or the open query line, or the last query's miss.
    ///
    /// The query line **replaces** this row rather than costing a second one:
    /// the overlay has exactly one row of chrome, [`Self::content_rows`]
    /// already takes exactly one row out of the window, and an invisible
    /// query line is **F75** at its most basic -- the user cannot see what
    /// they are about to search for, or that they are in a query at all.
    ///
    /// The fit is [`kitsu_render::fit_row_to_width`]'s, as every other row on
    /// these surfaces is: a footer that wrapped would cost the transcript row
    /// above it on a screen the frame fills.
    pub fn footer(&self, cols: usize) -> String {
        let row = match (self.query.as_deref(), self.miss.as_deref()) {
            (Some(query), _) => format!("/{query}"),
            (None, Some(miss)) => miss.to_owned(),
            (None, None) => Self::FOOTER.to_owned(),
        };
        kitsu_render::fit_row_to_width(&row, cols)
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
    use crate::fullscreen::{FullscreenView, Jump, Page};
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

    /// **R6-c in the overlay**: `{`/`}` jump between prompt blocks, and the
    /// jump is **delegated** to the held [`FullscreenView`] against the
    /// overlay's *content* rows, not its window.
    ///
    /// The fixture and its arithmetic (**F61**), since every row below is
    /// stated in ordinals: prompts at 0, and at 3-4 as one two-row block;
    /// eight retained lines; a 6-row window, so `content_rows` is 5 and
    /// `follow_top` is `8 - 5 = 3`. The opening content is therefore ordinals
    /// 3..=7 and the jump cursor is 3.
    ///
    /// Not vacuous, and specifically not satisfiable by an overlay that passed
    /// its full height down: the `}` half is compared against a reference
    /// `FullscreenView` at the content height, and at height 6 the reference's
    /// `follow_top` would be 2 rather than 3, so the clamped landing differs.
    /// The frames are also asserted to have moved, so a `jump` that returned
    /// `true` without doing anything fails.
    #[test]
    fn the_overlay_jumps_between_prompt_blocks_over_its_content_rows() {
        const ROWS: u16 = 6;
        const CONTENT: u16 = 5;
        let mut ring = CellRing::new(16);
        ring.push_prompt("first question");
        ring.push(Fd::Stdout, "answer one");
        ring.push(Fd::Stdout, "answer two");
        ring.push_prompt("second question\nwith a tail");
        ring.push(Fd::Stdout, "answer three");
        ring.push(Fd::Stdout, "answer four");
        ring.push(Fd::Stdout, "answer five");
        assert_eq!((ring.len(), ring.evicted()), (8, 0));
        assert_eq!(ring.prompt_blocks(), vec![0, 3], "the fixture's two blocks");
        assert_eq!(
            TranscriptOverlay::content_rows(ROWS),
            CONTENT,
            "the footer takes one row, so the jump clamps over five"
        );

        let mut overlay = TranscriptOverlay::new();
        let mut reference = FullscreenView::new();
        let opened = overlay.frame(&ring, ROWS, 80);
        assert_eq!(
            &opened[..usize::from(CONTENT)],
            [
                "second question",
                "with a tail",
                "answer three",
                "answer four",
                "answer five",
            ],
            "ordinals 3..=7"
        );

        assert!(
            overlay.jump(Jump::Prev, &ring, ROWS),
            "there is a block above the cursor (ordinal 0, below ordinal 3)"
        );
        assert!(reference.jump(Jump::Prev, &ring, CONTENT));
        let at_first = overlay.frame(&ring, ROWS, 80);
        assert_ne!(at_first, opened, "the overlay's frame actually moved");
        assert_eq!(
            &at_first[..usize::from(CONTENT)],
            [
                "first question",
                "answer one",
                "answer two",
                "second question",
                "with a tail",
            ],
            "ordinals 0..=4: the oldest block, at its first row"
        );
        assert_eq!(
            &at_first[..usize::from(CONTENT)],
            reference.frame(&ring, CONTENT, 80).as_slice(),
            "and it is the held view's own answer, not a second anchor"
        );
        assert_eq!(at_first[usize::from(CONTENT)], overlay.footer(80));
        assert!(!overlay.is_following(), "a jump pins the overlay");
        assert_eq!(overlay.is_following(), reference.is_following());

        // The other direction, and the half that catches a jump handed the
        // full window height instead of the content height.
        assert!(overlay.jump(Jump::Next, &ring, ROWS));
        assert!(reference.jump(Jump::Next, &ring, CONTENT));
        let back = overlay.frame(&ring, ROWS, 80);
        assert_eq!(
            &back[..usize::from(CONTENT)],
            &opened[..usize::from(CONTENT)],
            "`}}` lands back on the second block, clamped to follow_top = 3"
        );
        assert_eq!(
            &back[..usize::from(CONTENT)],
            reference.frame(&ring, CONTENT, 80).as_slice(),
            "delegated, at the content height and not the window's"
        );
        assert!(
            !overlay.is_following(),
            "and landing on the newest block pins rather than re-latching"
        );

        // Nowhere to go, in the overlay too: the keys report it rather than
        // moving the frame to somewhere arbitrary.
        let mut at_oldest = TranscriptOverlay::new();
        assert!(at_oldest.jump(Jump::Prev, &ring, ROWS));
        let oldest_frame = at_oldest.frame(&ring, ROWS, 80);
        assert!(
            !at_oldest.jump(Jump::Prev, &ring, ROWS),
            "no block above the oldest one"
        );
        assert_eq!(at_oldest.frame(&ring, ROWS, 80), oldest_frame);
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
    ///
    /// **R6-c moved one word, and only one.** `{`/`}` were in the forbidden
    /// set at R6-a because nothing dispatched them; R6-c dispatches them, so
    /// the capability word `jump` leaves the forbidden set and the two
    /// characters join the advertised one. `search`, `dump`, `editor`,
    /// `scrollback` and the literal `[` **stay** forbidden: **R6-b** and
    /// **R6-d** are still unbuilt, and this guard is what stops the footer
    /// promising them.
    ///
    /// The two braces are checked by their **character**, unlike the words
    /// above, and that is sound where `/`, `n` and `v` were not: no other
    /// token in a key row contains a brace, so the assertion cannot be
    /// satisfied only by its own fixture (**F58**).
    #[test]
    fn the_footer_advertises_the_overlays_keys_and_no_key_it_does_not_answer() {
        let overlay = TranscriptOverlay::new();
        let footer = overlay.footer(80);
        assert!(
            !footer.is_empty(),
            "the overlay advertises its keys in situ"
        );
        for advertised in ["q", "Esc", "PgUp", "PgDn", "{", "}", "n/N"] {
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
        // **R6-b**: `search` has moved out of this list and into the required
        // advertisements below, because `/` is now dispatched. `dump`,
        // `editor` and `scrollback` stay forbidden -- R6-d is gated -- so the
        // guard that has kept this footer honest through two cards is
        // narrowed by exactly one word and not relaxed.
        for advertised in ["search", "match"] {
            assert!(
                lowered.contains(advertised),
                "the footer names the {advertised:?} capability, which R6-b \
                 dispatches; it reads {footer:?}"
            );
        }
        for unanswered in ["dump", "editor", "scrollback"] {
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

    // ---- R6-b: `/` search, and the `n`/`N` match walk ----
    //
    // These are a **policy** over `FullscreenView::pin_to`, which R6-c built
    // for this card and deliberately left prompt-unaware. `{`/`}` is the other
    // policy over the same anchor; there is no second anchor concept here, and
    // the cannot-move rule below is R6-c's rule obeyed rather than a second
    // one invented.

    /// **R6-b's fixture**: three occurrences of one needle, spread across both
    /// speakers and with one of them inside the **second row** of a two-row
    /// prompt block.
    ///
    /// The ordinals, done rather than assumed (**F61**), because every
    /// assertion below is stated in them:
    ///
    /// | ordinal | row | |
    /// |---|---|---|
    /// | 0 | `alpha needle` | **prompt**, block 1, and a **match** |
    /// | 1 | `answer a0` | |
    /// | 2 | `answer a1 needle` | a **match**, on the session's side |
    /// | 3 | `beta one` | **prompt**, block 2, not a match |
    /// | 4 | `beta two needle` | block 2's **second row**, and a **match** |
    /// | 5 | `answer b0` | |
    /// | 6 | `answer b1` | |
    /// | 7 | `gamma` | **prompt**, block 3 |
    /// | 8 | `answer c0` | |
    /// | 9 | `answer c1` | |
    ///
    /// Ten retained lines, nothing evicted. Every test below uses a **5**-row
    /// overlay, so `content_rows` is 4, `follow_top` is `10 - 4 = 6`, the
    /// opening content is ordinals `6..=9` and the jump cursor is 6.
    fn search_ring() -> CellRing {
        let mut ring = CellRing::new(100);
        ring.push_prompt("alpha needle");
        ring.push(Fd::Stdout, "answer a0");
        ring.push(Fd::Stdout, "answer a1 needle");
        ring.push_prompt("beta one\nbeta two needle");
        ring.push(Fd::Stdout, "answer b0");
        ring.push(Fd::Stdout, "answer b1");
        ring.push_prompt("gamma");
        ring.push(Fd::Stdout, "answer c0");
        ring.push(Fd::Stdout, "answer c1");
        ring
    }

    /// Type `query` into a fresh overlay's query line, one character at a
    /// time, as the byte loop hands them over.
    fn typed(overlay: &mut TranscriptOverlay, query: &str) {
        overlay.begin_search();
        for ch in query.chars() {
            overlay.push_search_char(ch);
        }
    }

    /// The content rows of a 5-row, 80-column overlay frame, footer dropped.
    fn content(overlay: &TranscriptOverlay, ring: &CellRing) -> Vec<String> {
        let mut frame = overlay.frame(ring, 5, 80);
        frame.pop();
        frame
    }

    /// **The fixture is the fixture** (**F99**/**F61**): the ordinals the
    /// table above claims are asserted once, here, so every search test below
    /// states a specification rather than describing whatever `search_ring`
    /// happened to build.
    #[test]
    fn the_search_fixture_holds_the_matches_the_search_tests_assume() {
        let ring = search_ring();
        assert_eq!((ring.len(), ring.evicted()), (10, 0));
        assert_eq!(
            ring.prompt_blocks(),
            vec![0, 3, 7],
            "and the two-row prompt is one block starting at 3"
        );
        let needles: Vec<usize> = ring
            .lines()
            .iter()
            .enumerate()
            .filter(|(_, line)| line.plain().contains("needle"))
            .map(|(index, _)| index)
            .collect();
        assert_eq!(
            needles,
            vec![0, 2, 4],
            "three matches: one the user typed, one the session wrote, and \
             one in the second row of the two-row prompt"
        );
        assert!(
            ring.lines()[0].is_prompt() && !ring.lines()[2].is_prompt(),
            "so the fixture really spans both speakers"
        );
        assert_eq!(
            content(&TranscriptOverlay::new(), &ring),
            vec!["answer b1", "gamma", "answer c0", "answer c1"],
            "so the opening content is ordinals 6..=9 and the cursor is 6"
        );
    }

    /// **Test: a submitted query lands on the NEWEST match, and it lands on
    /// the matching LINE even when that line is inside a prompt block.**
    ///
    /// The card's two decisions in one assertion. The newest match is the
    /// entry point because the overlay itself opens on the newest lines: "find
    /// this in the transcript" means the most recent occurrence, and a search
    /// that landed on the oldest would make the user walk the whole history
    /// back to where they already were.
    ///
    /// And `/` is **line**-granular where `{` is **block**-granular, which is
    /// not an inconsistency but the difference between the two features: `{`
    /// moves between prompts, so it must not stop inside one, while `/` moves
    /// to the text the user asked for, which is on ordinal **4** — the second
    /// row of block 2. A search that reported 3 would be showing the user a
    /// row their query does not appear in.
    ///
    /// Not vacuous: the content rows are pinned **by value**, so a submit that
    /// moved nowhere, moved to a constant, or moved to the block start fails;
    /// and the frame is asserted to differ from the opening one, so a submit
    /// that merely repainted fails too.
    #[test]
    fn a_submitted_query_lands_on_the_newest_matching_line_not_on_its_block() {
        let ring = search_ring();
        let mut overlay = TranscriptOverlay::new();
        let opening = content(&overlay, &ring);

        typed(&mut overlay, "needle");
        assert_eq!(
            overlay.search_query(),
            Some("needle"),
            "the typed characters are the query, in order"
        );
        assert!(
            overlay.submit_search(&ring, 5),
            "three matches, and the newest can be scrolled to the top, so the \
             frame moves"
        );
        assert_eq!(
            overlay.match_count(),
            3,
            "the submit found every occurrence, not just the one it jumped to"
        );
        assert_eq!(
            overlay.search_query(),
            None,
            "the query line closes on submit; the matches outlive it"
        );
        assert!(
            !overlay.is_following(),
            "a successful search pins, which is transition 12: a search that \
             left the view following would be scrolled off its own match by \
             the next appended line"
        );

        let landed = content(&overlay, &ring);
        assert_ne!(landed, opening, "the frame actually moved");
        assert_eq!(
            landed,
            vec!["beta two needle", "answer b0", "answer b1", "gamma"],
            "ordinals 4..=7: the newest match is at the TOP of the frame, and \
             it is the prompt's second row, not the block start at 3"
        );
    }

    /// **Test: a query that matches only the user's own prompt is found.**
    ///
    /// The ring retains the user's submitted lines now, so `/` searches both
    /// speakers — the capability R6-a's version of this card could not have
    /// offered, when "search the transcript" would have meant half a
    /// transcript. `alpha` appears on ordinal 0 and nowhere else, and ordinal
    /// 0 is a prompt, so a search that skipped the user's lines finds nothing
    /// here and fails on the very first assertion.
    #[test]
    fn a_query_matching_only_the_users_own_prompt_is_still_found() {
        let ring = search_ring();
        let mut overlay = TranscriptOverlay::new();
        typed(&mut overlay, "alpha");
        assert!(
            overlay.submit_search(&ring, 5),
            "`alpha` is on the user's own prompt line, and `/` searches both \
             speakers"
        );
        assert_eq!(overlay.match_count(), 1, "exactly the one prompt line");
        assert_eq!(
            content(&overlay, &ring),
            vec!["alpha needle", "answer a0", "answer a1 needle", "beta one"],
            "ordinals 0..=3, the prompt at the top of the frame"
        );
    }

    /// **Test: matching ignores case.**
    ///
    /// A transcript is prose, and a case-sensitive default is the **F75**
    /// shape: the user types what they remember seeing, the key appears to do
    /// nothing, and nothing on screen explains why. Asserted against the same
    /// fixture as the lowercase query, so the two must agree on both the
    /// count and the landing — a search that lowercased the query but not the
    /// line, or the other way round, fails.
    #[test]
    fn a_query_matches_without_regard_to_case() {
        let ring = search_ring();
        let mut lower = TranscriptOverlay::new();
        typed(&mut lower, "needle");
        assert!(lower.submit_search(&ring, 5));

        let mut upper = TranscriptOverlay::new();
        typed(&mut upper, "NeEdLe");
        assert!(
            upper.submit_search(&ring, 5),
            "the same three lines match whatever case the user typed"
        );
        assert_eq!(upper.match_count(), lower.match_count());
        assert_eq!(
            content(&upper, &ring),
            content(&lower, &ring),
            "and the mixed-case query lands exactly where the lowercase one did"
        );
    }

    /// **Test: a query with no match leaves the overlay exactly as it was, and
    /// the overlay says so.**
    ///
    /// Both halves matter. Leaving the anchor alone is the R6-c rule: a search
    /// that pinned on a miss would break the follow for nothing. Saying so is
    /// **F75** — a key that silently does nothing is a defect, and here the
    /// user has just typed a whole query, so "nothing happened" needs to be on
    /// the screen rather than inferred from a frame that did not move.
    ///
    /// Bidirectional, because both failure modes are live: the frame is pinned
    /// by value *and* the follow state is asserted, so a miss that scrolled
    /// fails, and the report is asserted to be visible, so a miss that was
    /// silent fails.
    #[test]
    fn a_query_with_no_match_moves_nothing_and_reports_the_miss_on_screen() {
        let ring = search_ring();
        let mut overlay = TranscriptOverlay::new();
        let opening = overlay.frame(&ring, 5, 80);

        typed(&mut overlay, "haystack");
        assert!(
            !overlay.submit_search(&ring, 5),
            "nothing in the fixture says `haystack`"
        );
        assert_eq!(overlay.match_count(), 0);
        assert!(
            overlay.is_following(),
            "a miss does not end the follow: the user would stop receiving \
             output because of a typo"
        );
        assert_eq!(
            content(&overlay, &ring),
            vec!["answer b1", "gamma", "answer c0", "answer c1"],
            "the content rows are byte-identical to the opening frame"
        );

        let chrome = overlay.frame(&ring, 5, 80).pop().expect("a footer row");
        assert_ne!(
            chrome, opening[4],
            "the chrome row changed, because the miss is reported there: a \
             frame identical to the opening one is a key that silently did \
             nothing (**F75**)"
        );
        assert!(
            chrome.to_ascii_lowercase().contains("no match"),
            "and it says which way it failed; it reads {chrome:?}"
        );
    }

    /// **Test: an empty query is not a match for everything.**
    ///
    /// `""` is a substring of every line, so the one-line implementation of
    /// this feature matches all ten of them and jumps — on a key the user
    /// pressed by accident. An empty submit is therefore a **cancel**: no
    /// matches, no jump, no pin, and the query line closes.
    #[test]
    fn an_empty_query_matches_nothing_rather_than_everything() {
        let ring = search_ring();
        let mut overlay = TranscriptOverlay::new();
        overlay.begin_search();
        assert_eq!(
            overlay.search_query(),
            Some(""),
            "the query line is open and empty"
        );
        assert!(
            !overlay.submit_search(&ring, 5),
            "an empty query is a cancel, not a match for every line"
        );
        assert_eq!(overlay.match_count(), 0, "and emphatically not ten");
        assert!(overlay.is_following(), "so nothing pinned");
        assert_eq!(
            overlay.search_query(),
            None,
            "and the query line closed rather than staying open forever"
        );
        assert_eq!(
            content(&overlay, &ring),
            vec!["answer b1", "gamma", "answer c0", "answer c1"],
            "the opening frame, unmoved"
        );
    }

    /// **Test: `n` and `N` walk the matches, and NEITHER wraps.**
    ///
    /// The card's wrap decision, pinned. `N` walks towards the older matches
    /// and `n` back towards the newer ones; at the oldest match `N` is inert
    /// and at the newest `n` is inert, and both report it.
    ///
    /// No wrap, for two reasons. `{`/`}` already settled what "nothing in that
    /// direction" means on this overlay — it does nothing and reports false —
    /// and a second policy for the same situation on the same surface is this
    /// epic's oldest failure mode. And a wrap is **F75** in its worst form:
    /// `n` at the newest match would teleport the frame to the oldest one, the
    /// user would read it as their place being lost rather than as the end of
    /// the matches, and nothing would say which had happened.
    ///
    /// Not vacuous, and not satisfiable by a stub that always returns false:
    /// every landing is pinned by value, and the inert presses at the two ends
    /// are asserted to leave the frame **byte-identical** to the landing
    /// before them, so a walk that moved on a false return fails too.
    #[test]
    fn n_and_shift_n_walk_the_matches_and_neither_one_wraps() {
        let ring = search_ring();
        let mut overlay = TranscriptOverlay::new();
        typed(&mut overlay, "needle");
        assert!(overlay.submit_search(&ring, 5), "lands on ordinal 4");

        assert!(
            overlay.repeat_search(Jump::Prev, &ring, 5),
            "`N` walks to the next older match, ordinal 2"
        );
        assert_eq!(
            content(&overlay, &ring),
            vec![
                "answer a1 needle",
                "beta one",
                "beta two needle",
                "answer b0"
            ],
            "ordinals 2..=5"
        );

        assert!(
            overlay.repeat_search(Jump::Prev, &ring, 5),
            "`N` again, to the oldest match at ordinal 0"
        );
        let oldest = content(&overlay, &ring);
        assert_eq!(
            oldest,
            vec!["alpha needle", "answer a0", "answer a1 needle", "beta one"],
            "ordinals 0..=3"
        );

        assert!(
            !overlay.repeat_search(Jump::Prev, &ring, 5),
            "and there is no fourth match above: `N` at the oldest match does \
             NOT wrap round to the newest"
        );
        assert_eq!(
            content(&overlay, &ring),
            oldest,
            "the frame is byte-identical, so the inert press really was inert"
        );

        assert!(
            overlay.repeat_search(Jump::Next, &ring, 5),
            "`n` comes back down, to ordinal 2"
        );
        assert_eq!(
            content(&overlay, &ring),
            vec![
                "answer a1 needle",
                "beta one",
                "beta two needle",
                "answer b0"
            ],
            "the same ordinals 2..=5 the walk up passed through"
        );
        assert!(
            overlay.repeat_search(Jump::Next, &ring, 5),
            "`n` to the newest match, ordinal 4"
        );
        let newest = content(&overlay, &ring);
        assert!(
            !overlay.repeat_search(Jump::Next, &ring, 5),
            "and `n` at the newest match does NOT wrap round to the oldest"
        );
        assert_eq!(
            content(&overlay, &ring),
            newest,
            "byte-identical again, in the other direction"
        );
    }

    /// **Test: `n` and `N` are inert until a query has been submitted.**
    ///
    /// The enumerated case the brief names, and the one a match walk gets
    /// wrong by indexing an empty list: with no active query there is nothing
    /// to repeat, so both report false and leave the follow alone. Asserted on
    /// a ring that **does** contain matchable text, so this is "no query", not
    /// "no match" wearing its clothes.
    #[test]
    fn n_and_shift_n_do_nothing_until_a_query_has_been_submitted() {
        let ring = search_ring();
        let mut overlay = TranscriptOverlay::new();
        assert_eq!(overlay.search_query(), None, "no query line is open");
        assert_eq!(overlay.match_count(), 0);
        for dir in [Jump::Next, Jump::Prev] {
            assert!(
                !overlay.repeat_search(dir, &ring, 5),
                "{dir:?} has no query to repeat"
            );
            assert!(overlay.is_following(), "so it did not pin either");
            assert_eq!(
                content(&overlay, &ring),
                vec!["answer b1", "gamma", "answer c0", "answer c1"],
                "and the frame is the opening one"
            );
        }
    }

    /// **Test: a search whose match cannot MOVE the frame obeys R6-c's rule
    /// rather than inventing a second one.**
    ///
    /// The single match is ordinal 9, the newest line, and it is **already on
    /// screen** — the opening frame is ordinals 6..=9. The ring cannot scroll
    /// it to the top, so its clamped target is ordinal 6, which is the row the
    /// frame is already on. R6-c settled what happens then: no transition at
    /// all, the anchor is left exactly as it was, follow state included, and
    /// the jump reports false. Pinning there would break the follow while the
    /// frame did not change — invisible at the keypress and visible later as
    /// output that silently stopped arriving (**F75**), which is the worse of
    /// the two cases to get wrong and the one that overruled R6-c's first
    /// answer.
    ///
    /// Non-vacuity is the point of the second half: a match that *can* move
    /// the frame does, from a pinned start, so "returns false and stays
    /// following" is not an implementation that never moves at all.
    #[test]
    fn a_match_that_cannot_move_the_frame_leaves_the_follow_exactly_as_it_was() {
        let ring = ring_of(100, 10);
        let mut overlay = TranscriptOverlay::new();
        typed(&mut overlay, "line 9");
        assert_eq!(
            content(&overlay, &ring),
            vec!["line 6", "line 7", "line 8", "line 9"],
            "the match at ordinal 9 is already on screen, at the bottom"
        );
        assert!(
            !overlay.submit_search(&ring, 5),
            "the one match clamps back onto the cursor, so the frame cannot \
             move and this is no transition at all"
        );
        assert_eq!(
            overlay.match_count(),
            1,
            "the query did match: this is a jump that cannot move, not a miss"
        );
        assert!(
            overlay.is_following(),
            "and the follow survives, which is R6-c's rule obeyed rather than \
             a second rule invented for `/`"
        );
        assert_eq!(
            content(&overlay, &ring),
            vec!["line 6", "line 7", "line 8", "line 9"],
            "byte-identical"
        );

        // The other direction, so the rule above is not an implementation
        // that simply never moves: from the oldest line, a match that IS on
        // screen and CAN be scrolled to the top moves the frame.
        let mut pinned = TranscriptOverlay::new();
        pinned.page(Page::Oldest, &ring, 5);
        assert_eq!(
            content(&pinned, &ring),
            vec!["line 0", "line 1", "line 2", "line 3"],
            "ordinals 0..=3, and the cursor is 0"
        );
        typed(&mut pinned, "line 2");
        assert!(
            pinned.submit_search(&ring, 5),
            "ordinal 2 is on screen and clamps to 2, which is not the cursor"
        );
        assert_eq!(
            content(&pinned, &ring),
            vec!["line 2", "line 3", "line 4", "line 5"],
            "so the frame moves and puts the match on the top row"
        );
    }

    /// **Test: the matches are a SNAPSHOT taken at submit, so `n` never
    /// re-scans the ring.**
    ///
    /// This is the cost decision made observable instead of timed. A full
    /// `plain()` pass over a default 10 000-line ring was **measured** at
    /// ~4 ms in release and ~14 ms in debug, and ~10 ms / ~45 ms once one
    /// **F52** pasted megabyte is in the ring, because `plain()` allocates a
    /// fresh `String` per line and `CellRing::lines` clones every line first.
    /// A search that re-scanned would therefore pay that on every `n`, so the
    /// scan happens **once per submitted query** and `n`/`N` walk the stored
    /// ordinals.
    ///
    /// The behavioural consequence is what is pinned here, because it is the
    /// user-visible half of the same decision: a line appended *after* the
    /// submit does not join the match list, and is picked up by the next `/`
    /// rather than silently appearing mid-walk. An implementation that
    /// re-scanned would report four matches here and fail.
    #[test]
    fn the_matches_are_a_snapshot_taken_at_submit_so_a_repeat_never_rescans() {
        let mut ring = search_ring();
        let mut overlay = TranscriptOverlay::new();
        typed(&mut overlay, "needle");
        assert!(overlay.submit_search(&ring, 5));
        assert_eq!(overlay.match_count(), 3);

        // Ordinal 10 is the appended match; the four fillers behind it are
        // there so that the next query's newest match can actually be
        // scrolled to the top (**F58**: without them `follow_top` is 11 and
        // ordinal 10 clamps onto the cursor, so the assertion below would be
        // unsatisfiable rather than true).
        ring.push(Fd::Stdout, "a fourth needle, appended after the submit");
        for n in 0..4 {
            ring.push(Fd::Stdout, &format!("later {n}"));
        }
        assert_eq!((ring.len(), ring.evicted()), (15, 0));
        assert_eq!(
            overlay.match_count(),
            3,
            "the match list is the one the submit took: `n` walks stored \
             ordinals and does not re-scan 10 000 lines per press"
        );
        assert!(
            !overlay.repeat_search(Jump::Next, &ring, 5),
            "and `n` still stops at the newest match the submit found: the \
             line appended since is not in the walk"
        );

        let mut walked = TranscriptOverlay::new();
        typed(&mut walked, "needle");
        assert!(
            walked.submit_search(&ring, 5),
            "follow_top is 15 - 4 = 11 and the newest match is ordinal 10, so \
             the frame can move"
        );
        assert_eq!(
            walked.match_count(),
            4,
            "and the NEXT `/` does see the appended line, so the snapshot is \
             per query and not a cache that goes stale forever"
        );
        assert_eq!(
            content(&walked, &ring),
            vec![
                "a fourth needle, appended after the submit",
                "later 0",
                "later 1",
                "later 2",
            ],
            "ordinals 10..=13, the appended match on the top row"
        );
    }

    /// **Test: a stored match the ring has since EVICTED does not invent a
    /// frame, and does not move one it cannot move.**
    ///
    /// The snapshot holds **absolute** ordinals, which is what makes this case
    /// exist: the ring is a sliding window, so a match can stop existing
    /// between the submit and the `N`. The ordinal is clamped by `pin_to`'s
    /// one clamp — `top`'s — so the frame stays inside the ring, and the
    /// cannot-move rule then applies exactly as it does to any other jump.
    ///
    /// The fixture, done rather than assumed: a 6-line ring with matches on
    /// ordinals 0 and 1, then four more lines pushed, so ordinals 0..=3 are
    /// evicted and the oldest retained line is 4.
    #[test]
    fn a_match_the_ring_has_evicted_clamps_instead_of_inventing_a_frame() {
        let mut ring = CellRing::new(6);
        ring.push(Fd::Stdout, "oldest needle");
        ring.push(Fd::Stdout, "second needle");
        for n in 0..4 {
            ring.push(Fd::Stdout, &format!("filler {n}"));
        }
        let mut overlay = TranscriptOverlay::new();
        typed(&mut overlay, "needle");
        assert!(
            overlay.submit_search(&ring, 5),
            "six lines, content_rows 4, so follow_top is 2 and ordinal 1 \
             clamps to 1: the frame moves"
        );
        assert_eq!(
            content(&overlay, &ring),
            vec!["second needle", "filler 0", "filler 1", "filler 2"],
            "ordinals 1..=4"
        );

        for n in 4..8 {
            ring.push(Fd::Stdout, &format!("filler {n}"));
        }
        assert_eq!(
            (ring.len(), ring.evicted()),
            (6, 4),
            "both matched lines are gone from the process now"
        );
        assert_eq!(
            content(&overlay, &ring),
            vec!["filler 2", "filler 3", "filler 4", "filler 5"],
            "the pinned frame clamped back into the ring, which is \
             transition 11 and not this card's arithmetic"
        );
        assert!(
            !overlay.repeat_search(Jump::Prev, &ring, 5),
            "`N` onto an evicted ordinal clamps onto the cursor, so it cannot \
             move the frame and does nothing at all"
        );
        assert_eq!(
            content(&overlay, &ring),
            vec!["filler 2", "filler 3", "filler 4", "filler 5"],
            "byte-identical: no second clamp, no invented frame"
        );
    }

    /// **Test: the query line is PAINTED, in the overlay's one chrome row.**
    ///
    /// Typing into an invisible query line is **F75** at its most basic: the
    /// user cannot see what they are about to search for, or that they are in
    /// a query at all. The overlay has exactly one row of chrome and
    /// `content_rows` takes exactly one row out of the window, so the query
    /// line replaces the footer rather than costing a second transcript row —
    /// the footer's own reason for coming out of the window in the first
    /// place.
    ///
    /// Not vacuous: the content rows are asserted unchanged across the whole
    /// sequence, so a query line that stole a transcript row fails, and the
    /// chrome row is pinned by value at three points, so a query line that
    /// painted a constant fails.
    #[test]
    fn the_query_line_is_painted_in_the_footers_row_and_costs_no_transcript_row() {
        let ring = search_ring();
        let mut overlay = TranscriptOverlay::new();
        let opening = overlay.frame(&ring, 5, 80);
        assert_eq!(opening.len(), 5);

        overlay.begin_search();
        let open = overlay.frame(&ring, 5, 80);
        assert_eq!(open.len(), 5, "still exactly the window, footer included");
        assert_eq!(
            &open[..4],
            &opening[..4],
            "and the transcript rows are untouched: `/` scrolls nothing"
        );
        assert_eq!(
            open[4], "/",
            "the empty query line is the prompt character alone, so the user \
             can see they are in a query"
        );

        typed(&mut overlay, "needle");
        let typing = overlay.frame(&ring, 5, 80);
        assert_eq!(
            typing[4], "/needle",
            "and it shows what they have typed so far"
        );
        assert_eq!(
            &typing[..4],
            &opening[..4],
            "still nothing scrolled: the search runs on submit, not on every \
             keystroke"
        );

        overlay.cancel_search();
        assert_eq!(
            overlay.frame(&ring, 5, 80),
            opening,
            "and a cancelled query gives the footer row back, byte-identical \
             to the frame before the `/`"
        );
    }

    /// **Test: the query line's own editing keys.**
    ///
    /// Three enumerated cases that are each a silent dead end if missed.
    /// Backspace, because a query line a typo cannot be corrected in is a
    /// query line the user has to abandon; backspace on an **empty** query,
    /// because deleting past the start must not close the query line or the
    /// overlay; and `q`, because `q` closes this overlay and must be ordinary
    /// text while a query is open — otherwise the word `query` cannot be
    /// searched for, and the overlay vanishes mid-typing.
    #[test]
    fn the_query_line_takes_backspace_and_treats_the_close_keys_letter_as_text() {
        let mut overlay = TranscriptOverlay::new();
        typed(&mut overlay, "quer");
        assert_eq!(
            overlay.search_query(),
            Some("quer"),
            "`q` is text here, not the close key"
        );
        overlay.push_search_char('t');
        overlay.backspace_search();
        assert_eq!(
            overlay.search_query(),
            Some("quer"),
            "backspace deletes the last character, and only the last one"
        );
        for _ in 0..4 {
            overlay.backspace_search();
        }
        assert_eq!(
            overlay.search_query(),
            Some(""),
            "down to empty, and the query line is still open"
        );
        overlay.backspace_search();
        assert_eq!(
            overlay.search_query(),
            Some(""),
            "backspace on an empty query is inert: it does not close the \
             query line behind the user's back"
        );
    }
}
