//! The bounded, fd-tagged **cell ring** (plan row T3-S6, requirement **N4**).
//!
//! `fullscreen` keeps no conversation-length scrollback: memory is bounded by
//! this ring, which the TUI surface feeds one transcript line at a time and
//! which the transcript overlay and the `[` dump then read. Because kitsu
//! persists only `messages`, the ring is the **sole** in-process record of
//! what was on screen — anything it evicts is gone from the overlay and is
//! reachable only through `/export`.
//!
//! # What a stored line is: the styled bytes, stripped on demand
//!
//! [`CellLine::styled`] returns the line's bytes exactly as the surface
//! received them, SGR and all, because that is already
//! [`kitsu_render::surface::Surface::transcript_line`]'s contract ("must be
//! written verbatim, with no re-styling, trimming or re-sanitizing") and the
//! overlay repaints these rows. [`CellLine::plain`] derives the stripped form
//! with the existing [`kitsu_render::sanitize_terminal_text`] for the `[` dump
//! and for a future search. The direction is deliberate: styling can always be
//! stripped, never reconstructed, and N4 makes this the only copy.
//!
//! # Who owns the ring
//!
//! Nothing here is a process global, and the classic surface in `kitsu-render`
//! cannot reach it: the ring is owned by the TUI backend that has an overlay
//! to feed it (see [`crate::viewport::InlineSurface::with_cell_ring`]), and a
//! backend built without one keeps no cells and writes the same bytes.
//!
use std::collections::VecDeque;

use kitsu_render::surface::Fd;

/// One retained transcript line, with the stream it was written on.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CellLine {
    fd: Fd,
    styled: String,
    prompt: bool,
}

impl CellLine {
    /// Which stream this line was emitted on.
    pub fn fd(&self) -> Fd {
        self.fd
    }

    /// The line's bytes exactly as the surface received them.
    pub fn styled(&self) -> &str {
        &self.styled
    }

    /// Whether this line is the **user's own submitted prompt** rather than
    /// something the session wrote onto the transcript.
    ///
    /// The mark is set by [`CellRing::push_prompt`] and by nothing else, so
    /// it is true for exactly the lines the composer's own feed handed over.
    /// Deliberately **not** derived from [`Self::fd`]: a prompt files on
    /// `Stdout` with the conversation, which `Stderr` is tools and
    /// diagnostics, and a rule inferred from the sink selector would mark
    /// every assistant row as well (**F70**).
    pub fn is_prompt(&self) -> bool {
        self.prompt
    }

    /// The line with terminal control sequences removed, for the `[` dump.
    pub fn plain(&self) -> String {
        // `sanitize_terminal_text` is the crate's one stripper, so the dump
        // cannot drift from what the rest of the renderer calls "plain".
        // `ascii_structural_markers` is deliberately NOT applied: that is a
        // `--plain` tier transform, not a storage one.
        kitsu_render::sanitize_terminal_text(&self.styled)
    }
}

/// A bounded FIFO of fd-tagged transcript lines: at most `limit` lines are
/// retained and the oldest is evicted to make room.
#[derive(Debug)]
pub struct CellRing {
    limit: usize,
    lines: VecDeque<CellLine>,
    evicted: usize,
}

impl CellRing {
    /// A ring bounded at `limit` lines.
    ///
    /// `limit` comes from `[tui] cell_ring_lines`, which is range-checked at
    /// the config parse, so no clamping happens here.
    pub fn new(limit: usize) -> Self {
        Self {
            limit,
            lines: VecDeque::new(),
            evicted: 0,
        }
    }

    /// The configured bound.
    pub fn limit(&self) -> usize {
        self.limit
    }

    /// How many lines are retained now.
    pub fn len(&self) -> usize {
        self.lines.len()
    }

    /// Whether anything is retained.
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// How many lines have been evicted over the ring's lifetime — what
    /// `/export` is the route to.
    pub fn evicted(&self) -> usize {
        self.evicted
    }

    /// Append one line on `fd`, evicting the oldest if the ring is full.
    ///
    /// Never marked the user's prompt: this is the session's own feed, and
    /// [`Self::push_prompt`] is the only door the mark comes through.
    pub fn push(&mut self, fd: Fd, text: &str) {
        self.push_marked(fd, text, false);
    }

    /// Append one line on `fd`, carrying `prompt` onto the stored cell.
    ///
    /// The one place a [`CellLine`] is built, which is why adding the mark was
    /// a one-site change: the fields are private and there is no public
    /// constructor, so no caller outside this module can make an unmarked
    /// line claim to be a prompt or the reverse.
    fn push_marked(&mut self, fd: Fd, text: &str, prompt: bool) {
        // Evict first, so the retained count never exceeds the bound even
        // momentarily, and `while` rather than `if` so a ring somehow holding
        // more than its bound converges instead of staying over it forever.
        while self.lines.len() >= self.limit {
            // A ring of zero lines would otherwise spin: `new` takes the
            // config-checked bound, whose minimum is 1, but the guard costs
            // nothing and keeps the invariant local to this function.
            if self.lines.pop_front().is_none() {
                return;
            }
            self.evicted += 1;
        }
        self.lines.push_back(CellLine {
            fd,
            // The surface's bytes verbatim, SGR included: styling can be
            // stripped later (see `CellLine::plain`) but never reconstructed,
            // and N4 makes this the only copy.
            styled: text.to_string(),
            prompt,
        });
    }

    /// Re-append an already retained line, mark included.
    ///
    /// For the frame rebuild (`tui_bridge::frame_ring`), which both ring
    /// readers -- the fullscreen frame and the `Ctrl+O` overlay -- go through:
    /// rebuilding from `(fd, styled)` alone silently drops the mark and the
    /// overlay shows one speaker however faithfully the feed stored it
    /// (**F79**). It takes a `CellLine` rather than a `(fd, text, bool)`
    /// triple on purpose, so the only way to obtain the mark is to have been
    /// handed a line that already carries it.
    pub fn push_retained(&mut self, line: &CellLine) {
        self.push_marked(line.fd, &line.styled, line.prompt);
    }

    /// Append the user's own submitted prompt, **one marked cell per line**.
    ///
    /// `fd` is [`Fd::Stdout`] deliberately: `Fd` is a sink selector, so the
    /// question is which stream a prompt would be written on if it ever were,
    /// and the answer makes the filter semantics come out right -- `Stdout` is
    /// *the conversation*, the assistant's text and the user's lines together,
    /// and `Stderr` is *tools and meta*.
    ///
    /// The split is [`str::lines`], exactly as `write_block` already splits
    /// the bytes a block writes, because both `FullscreenView::frame` and
    /// `TranscriptOverlay::frame` fit **one row per cell**: a cell holding an
    /// embedded newline is painted as a single row that cannot fit. Every
    /// resulting cell is marked, not only the first, so the overlay can style
    /// a whole block as the user's and R6-c can coalesce it into one jump
    /// target instead of stopping inside it.
    ///
    /// Separate from [`Self::push`] so the existing feed keeps its signature
    /// and its four call sites.
    pub fn push_prompt(&mut self, text: &str) {
        for line in text.lines() {
            self.push_marked(Fd::Stdout, line, true);
        }
    }

    /// The **absolute ordinal of the first row of each prompt block**, oldest
    /// first (requirement **R6-c**).
    ///
    /// A *block* is a maximal run of **adjacent** prompt-marked cells, not one
    /// cell: [`Self::push_prompt`] stores one marked cell per line because
    /// both frames fit one row per cell, so a two-line prompt is two marked
    /// cells and a jump that stepped cells would stop twice inside one thing
    /// the user typed once. Coalescing is therefore done here, where the marks
    /// are, rather than in each of the two views that walk them.
    ///
    /// The ordinals are **absolute** — counted over the ring's whole lifetime,
    /// evicted lines included — because that is the unit
    /// [`crate::fullscreen::FullscreenView`]'s anchor is in, and mixing the
    /// two is transition 11 going unnoticed. Only retained rows are reported,
    /// so a block whose first rows have been evicted is reported at its oldest
    /// row that still exists.
    pub fn prompt_blocks(&self) -> Vec<usize> {
        // One pass over the retained marks, keeping the run's head: a block
        // starts at a marked row whose predecessor in the ring is not marked.
        // The first retained row is a head whenever it is marked, which is
        // also the partial-eviction answer -- the row the block started on is
        // gone from the process, so its oldest surviving row is the stop.
        let oldest = self.evicted();
        let mut blocks = Vec::new();
        let mut previous_marked = false;
        for (index, line) in self.lines.iter().enumerate() {
            if line.prompt && !previous_marked {
                blocks.push(oldest + index);
            }
            previous_marked = line.prompt;
        }
        blocks
    }

    /// The retained lines, oldest first.
    pub fn lines(&self) -> Vec<CellLine> {
        self.lines.iter().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::{CellLine, CellRing};
    use kitsu_render::surface::Fd;

    fn plain_texts(ring: &CellRing) -> Vec<String> {
        ring.lines().iter().map(CellLine::plain).collect()
    }

    /// The acceptance test the plan row names: at the bound the **oldest**
    /// line leaves, not the newest, and not a line from the middle.
    ///
    /// Not vacuous: the pushes before the bound are asserted to be retained in
    /// full first, so the eviction assertion is observing a ring that really
    /// did hold three lines.
    #[test]
    fn cell_ring_evicts_the_oldest_line_at_the_bound() {
        let mut ring = CellRing::new(3);
        for text in ["one", "two", "three"] {
            ring.push(Fd::Stdout, text);
        }
        assert_eq!(ring.len(), 3, "the ring filled to its bound");
        assert_eq!(plain_texts(&ring), vec!["one", "two", "three"]);
        assert_eq!(ring.evicted(), 0, "nothing evicted before the bound");

        ring.push(Fd::Stdout, "four");
        assert_eq!(ring.len(), 3, "the bound holds");
        assert_eq!(
            plain_texts(&ring),
            vec!["two", "three", "four"],
            "the oldest line left and the order of the rest is unchanged"
        );
        assert_eq!(ring.evicted(), 1);
    }

    /// Requirement **N4**: the bound is a *memory* bound, so it must hold
    /// under far more pushes than the limit.
    ///
    /// Not vacuous, and specifically not satisfiable by an unbounded `Vec`:
    /// 5 000 lines go in behind a bound of 64, and the test asserts the
    /// retained count (64, not 5 000), the eviction count (4 936) and that
    /// the survivors are exactly the newest 64 **in order** — an unbounded
    /// `Vec` fails the first assertion, a ring that evicted the newest fails
    /// the third, and one that reordered fails it too.
    #[test]
    fn cell_ring_bound_holds_under_far_more_pushes_than_the_limit() {
        const LIMIT: usize = 64;
        const PUSHES: usize = 5_000;
        let mut ring = CellRing::new(LIMIT);
        for n in 0..PUSHES {
            ring.push(Fd::Stdout, &format!("line {n}"));
        }
        assert_eq!(ring.len(), LIMIT, "retained lines are bounded by the limit");
        assert_eq!(ring.limit(), LIMIT);
        assert_eq!(ring.evicted(), PUSHES - LIMIT);
        let expected: Vec<String> = (PUSHES - LIMIT..PUSHES)
            .map(|n| format!("line {n}"))
            .collect();
        assert_eq!(
            plain_texts(&ring),
            expected,
            "the survivors are the newest lines, oldest-first"
        );
    }

    /// The fd tag is per line and survives eviction, and interleaved
    /// stdout/stderr lines keep both their tags and their relative order.
    ///
    /// Not vacuous: the sequence contains both tags before and after the
    /// evicted prefix, so a ring that dropped the tag (or tagged every line
    /// the same) cannot pass, and the asserted pairs are compared as a whole
    /// sequence rather than by substring (finding F41).
    #[test]
    fn cell_ring_keeps_the_fd_tag_and_the_interleaved_order() {
        let mut ring = CellRing::new(4);
        let fed = [
            (Fd::Stdout, "assistant a"),
            (Fd::Stderr, "tool a"),
            (Fd::Stdout, "assistant b"),
            (Fd::Stderr, "tool b"),
            (Fd::Stdout, "assistant c"),
            (Fd::Stderr, "tool c"),
        ];
        for (fd, text) in fed {
            ring.push(fd, text);
        }
        let got: Vec<(Fd, String)> = ring
            .lines()
            .iter()
            .map(|line| (line.fd(), line.plain()))
            .collect();
        assert_eq!(
            got,
            vec![
                (Fd::Stdout, "assistant b".to_string()),
                (Fd::Stderr, "tool b".to_string()),
                (Fd::Stdout, "assistant c".to_string()),
                (Fd::Stderr, "tool c".to_string()),
            ]
        );
    }

    /// What a stored line **is**: the styled bytes verbatim, with the stripped
    /// form derived on demand.
    ///
    /// Not vacuous: the input genuinely carries SGR bytes (asserted before the
    /// comparison), so `styled` returning a sanitized string and `plain`
    /// returning the styled string are both caught.
    #[test]
    fn cell_ring_stores_styled_bytes_verbatim_and_strips_them_on_demand() {
        let styled = "\x1b[1;32mdone\x1b[0m tool";
        assert!(styled.contains('\x1b'), "the fixture carries SGR bytes");
        let mut ring = CellRing::new(2);
        ring.push(Fd::Stderr, styled);
        let lines = ring.lines();
        assert_eq!(lines.len(), 1);
        assert_eq!(
            lines[0].styled(),
            styled,
            "the ring keeps the surface's bytes, byte for byte"
        );
        assert_eq!(
            lines[0].plain(),
            "done tool",
            "the stripped form is what the `[` dump reads"
        );
        assert_eq!(lines[0].fd(), Fd::Stderr);
    }

    /// A fresh ring is empty, which is what makes the "nothing retained"
    /// assertions elsewhere mean something, and the first push is observable.
    #[test]
    fn a_fresh_cell_ring_is_empty_and_the_first_push_is_observable() {
        let mut ring = CellRing::new(1);
        assert!(ring.is_empty());
        assert_eq!(ring.len(), 0);
        assert_eq!(ring.lines(), Vec::new());
        ring.push(Fd::Stdout, "first");
        assert!(!ring.is_empty(), "the push is not a no-op");
        assert_eq!(plain_texts(&ring), vec!["first"]);
    }

    /// The documented minimum, `1`: one line is retained, and it is the
    /// newest. A ring that treated `1` as "disabled" would retain none.
    #[test]
    fn a_one_line_cell_ring_retains_only_the_newest_line() {
        let mut ring = CellRing::new(1);
        for text in ["a", "b", "c"] {
            ring.push(Fd::Stdout, text);
        }
        assert_eq!(ring.len(), 1);
        assert_eq!(plain_texts(&ring), vec!["c"]);
        assert_eq!(ring.evicted(), 2);
    }

    /// The retained lines as `(is a prompt, stripped text)`, which is the pair
    /// the three tests below compare: a mark with no text beside it would pass
    /// for a ring that marked an entry it never stored.
    fn marked_texts(ring: &CellRing) -> Vec<(bool, String)> {
        ring.lines()
            .iter()
            .map(|line| (line.is_prompt(), line.plain()))
            .collect()
    }

    /// **The user's submitted prompt is retained, and retained *as* a prompt**
    /// (**F102**). The ring is **N4**'s sole source for the `Ctrl+O` overlay,
    /// so a ring that cannot tell the user's line from the session's is a
    /// transcript with one speaker, and `{`/`}` has nothing to jump between.
    ///
    /// The text is asserted beside the mark on purpose: a `push_prompt` that
    /// marked an entry without storing the line, or stored it without the
    /// mark, fails here rather than half-passing.
    #[test]
    fn the_ring_retains_the_users_submitted_prompt_as_a_prompt() {
        let mut ring = CellRing::new(8);
        ring.push_prompt("explain the ring");

        assert_eq!(
            marked_texts(&ring),
            vec![(true, "explain the ring".to_string())],
            "the submitted line is in the ring and is marked the user's"
        );
    }

    /// **And nothing the session wrote is marked the user's** -- the opposite
    /// failure, which is the one a role marker invites: a feed that marked
    /// every line, or a reader that inferred the mark from
    /// [`CellLine::fd`], would make every row a prompt boundary and `{`/`}`
    /// would stop on each of them.
    ///
    /// Both descriptors are swept because `Stderr` is the tempting one to
    /// mistake for the user: it carries tools, results and diagnostics, and a
    /// rule is only as wide as the narrowest input path feeding it (**F70**).
    #[test]
    fn the_ring_marks_no_line_the_session_wrote_as_a_prompt() {
        let mut ring = CellRing::new(8);
        ring.push(Fd::Stdout, "assistant text");
        ring.push(Fd::Stderr, "* tool ran");

        assert_eq!(
            marked_texts(&ring),
            vec![
                (false, "assistant text".to_string()),
                (false, "* tool ran".to_string()),
            ],
            "only the composer's own feed may mark a line the user's"
        );
    }

    /// **A prompt keeps its place among the lines it provoked.** The overlay
    /// repaints the ring in order and `{`/`}` walks it, so a prompt stored out
    /// of order -- appended last, or hoisted ahead of the previous turn --
    /// would put the user's line beside the wrong answer.
    ///
    /// Interleaved rather than grouped: the marks and the texts are compared
    /// as one sequence, so a ring that kept the texts in order while losing
    /// which of them were prompts fails, and so does the reverse.
    #[test]
    fn the_ring_keeps_prompts_in_place_among_the_lines_they_provoked() {
        let mut ring = CellRing::new(8);
        ring.push_prompt("first question");
        ring.push(Fd::Stdout, "first answer");
        ring.push(Fd::Stderr, "* tool ran");
        ring.push_prompt("second question");
        ring.push(Fd::Stdout, "second answer");

        assert_eq!(
            marked_texts(&ring),
            vec![
                (true, "first question".to_string()),
                (false, "first answer".to_string()),
                (false, "* tool ran".to_string()),
                (true, "second question".to_string()),
                (false, "second answer".to_string()),
            ],
            "the ring's order is the order of arrival, prompts included"
        );
    }

    /// **A prompt files with the conversation, not with the tools.**
    ///
    /// [`Fd`] is a **sink selector**, not a label, so the choice is which
    /// stream a prompt would be written on if it ever were: the main output,
    /// never the diagnostic one. That makes the filter semantics come out
    /// right -- a reader taking `Stdout` gets *the conversation*, the
    /// assistant's text and the user's lines together, and a reader taking
    /// `Stderr` gets *tools and meta*.
    ///
    /// Deliberately **one** assertion. The grouping is cheap to get wrong and
    /// expensive to notice, and an undocumented choice drifts into a reader
    /// filing the user's words with tool output; everything else about a
    /// prompt is pinned by the tests above, not here.
    #[test]
    fn a_prompt_files_with_the_conversation_and_not_with_the_tools() {
        let mut ring = CellRing::new(4);
        ring.push_prompt("explain the ring");

        assert_eq!(
            ring.lines()[0].fd(),
            Fd::Stdout,
            "a prompt belongs on the conversation's stream, not the \
             diagnostic one"
        );
    }

    /// **R6-c: a multi-line prompt is ONE jump stop, not one per line.**
    ///
    /// This is the decision the card turns on. `push_prompt` stores one marked
    /// cell per line because both frames fit one row per cell, so a two-line
    /// prompt is two marked cells; a `}` that walked marked *rows* would stop
    /// twice inside one thing the user typed once, which is not what "the next
    /// prompt" means. Adjacent marked rows therefore coalesce into one
    /// **block**, and the block is reported at its **first** row.
    ///
    /// Not vacuous, and specifically not satisfiable by a row-walker: the
    /// three-line prompt's rows are at ordinals 1, 2 and 3 and the asserted
    /// answer names only 1, so an implementation that returned every marked
    /// ordinal fails with 1, 2, 3, 5 against 1, 5; and the single-line prompt
    /// at 5 is there so a coalescer that swallowed *all* prompts into one
    /// block fails too. The marked ordinals are asserted alongside, so the
    /// fixture is proved to really hold four marked rows before the
    /// coalescing claim is made (**F99**).
    #[test]
    fn adjacent_prompt_rows_coalesce_into_one_jump_block() {
        let mut ring = CellRing::new(16);
        ring.push(Fd::Stdout, "a preamble line");
        ring.push_prompt("first line\nsecond line\nthird line");
        ring.push(Fd::Stdout, "an answer");
        ring.push_prompt("a one line prompt");

        let marked: Vec<usize> = ring
            .lines()
            .iter()
            .enumerate()
            .filter(|(_, line)| line.is_prompt())
            .map(|(index, _)| index)
            .collect();
        assert_eq!(
            marked,
            vec![1, 2, 3, 5],
            "the fixture really holds four marked rows, three of them adjacent"
        );

        assert_eq!(
            ring.prompt_blocks(),
            vec![1, 5],
            "two blocks, each at its first row: the three-line prompt is ONE \
             stop, so a jump cannot land inside it"
        );
    }

    /// **Two prompts submitted back to back are still one block**, because the
    /// rule is adjacency of marked rows and nothing separates them.
    ///
    /// Deliberate and pinned rather than left to the implementation: it is the
    /// one case where "a block" and "a submission" differ, and a
    /// `prompt_blocks` that tracked submissions instead of adjacency would
    /// answer `[0, 1]` here and stop twice on rows the user sees as one
    /// run of their own text.
    #[test]
    fn two_prompts_with_nothing_between_them_are_one_block() {
        let mut ring = CellRing::new(16);
        ring.push_prompt("first submission");
        ring.push_prompt("second submission");
        ring.push(Fd::Stdout, "an answer");
        ring.push_prompt("third submission");

        assert_eq!(ring.len(), 4, "four rows, three of them marked");
        assert_eq!(
            ring.prompt_blocks(),
            vec![0, 3],
            "adjacency is the rule, so the two back-to-back submissions are \
             one run of the user's text and one stop"
        );
    }

    /// **A ring with no prompts offers nothing to jump between**, and neither
    /// does an empty one — the two cases where `{`/`}` must report "nowhere to
    /// go" rather than land somewhere arbitrary.
    ///
    /// Not vacuous: the no-prompt ring is asserted to be non-empty first, so
    /// this is not an empty answer from an empty ring twice over.
    #[test]
    fn a_ring_with_no_prompts_has_no_jump_blocks() {
        assert_eq!(
            CellRing::new(8).prompt_blocks(),
            Vec::<usize>::new(),
            "an empty ring"
        );

        let mut ring = CellRing::new(8);
        ring.push(Fd::Stdout, "assistant text");
        ring.push(Fd::Stderr, "* tool ran");
        assert_eq!(ring.len(), 2, "the ring really holds lines");
        assert_eq!(
            ring.prompt_blocks(),
            Vec::<usize>::new(),
            "none of which is the user's, so there is nothing to jump between"
        );
    }

    /// **The ordinals are absolute, and a block whose head has been evicted is
    /// reported at its oldest row that still exists.**
    ///
    /// Absolute because that is the unit `FullscreenView`'s anchor is in: a
    /// retained-index answer would drift under eviction while looking
    /// unchanged, which is transition 11 going unnoticed. The partial block is
    /// the case the sliding window forces — the row the block *started* on is
    /// gone from the process, so the stop has to be the oldest row of it that
    /// is left, not an ordinal the frame can no longer paint.
    ///
    /// The arithmetic, done rather than assumed (**F61**): a 4-line ring fed a
    /// three-line prompt at ordinals 0..2 plus one plain line at 3 is full;
    /// one more push evicts ordinal 0, leaving marked rows at 1 and 2, so the
    /// block is 1; a second push evicts ordinal 1, leaving one marked row at
    /// 2, so the block is 2; a third leaves no marked row at all.
    #[test]
    fn prompt_block_ordinals_are_absolute_and_survive_a_partial_eviction() {
        let mut ring = CellRing::new(4);
        ring.push_prompt("l0\nl1\nl2");
        ring.push(Fd::Stdout, "plain 3");
        assert_eq!((ring.len(), ring.evicted()), (4, 0), "the ring is full");
        assert_eq!(
            ring.prompt_blocks(),
            vec![0],
            "one block, at the ring's very first line"
        );

        ring.push(Fd::Stdout, "plain 4");
        assert_eq!(ring.evicted(), 1);
        assert_eq!(
            ring.prompt_blocks(),
            vec![1],
            "the block's first row is gone, so the stop is its oldest \
             surviving row -- an absolute ordinal, not an index"
        );

        ring.push(Fd::Stdout, "plain 5");
        assert_eq!(ring.evicted(), 2);
        assert_eq!(ring.prompt_blocks(), vec![2], "one surviving row of it");

        ring.push(Fd::Stdout, "plain 6");
        assert_eq!(ring.evicted(), 3);
        assert_eq!(
            ring.prompt_blocks(),
            Vec::<usize>::new(),
            "and now the whole block is gone: `/export` is the route to it, \
             not a jump"
        );
    }

    /// **A multi-line prompt is one cell per line, and every one of them is
    /// marked.**
    ///
    /// One line per cell is load-bearing: both `FullscreenView::frame` and
    /// `TranscriptOverlay::frame` fit **one row per cell**, so a cell holding
    /// an embedded newline is painted as a single row that cannot fit and the
    /// frame is garbled. The split is `write_block`'s, which already does
    /// exactly this to the bytes a block writes.
    ///
    /// Every cell is marked, not only the first: that is what lets the overlay
    /// style a whole block as the user's, and what lets R6-c coalesce adjacent
    /// prompt rows into one jump target instead of stopping inside a block.
    ///
    /// The count is asserted beside the marks so a split that produced the
    /// right marks over the wrong number of cells fails here.
    #[test]
    fn a_multi_line_prompt_is_one_marked_cell_per_line() {
        let mut ring = CellRing::new(8);
        ring.push_prompt("first line\nsecond line\nthird line");

        assert_eq!(ring.len(), 3, "one cell per line, as `write_block` splits");
        assert_eq!(
            marked_texts(&ring),
            vec![
                (true, "first line".to_string()),
                (true, "second line".to_string()),
                (true, "third line".to_string()),
            ],
            "every row of the block is the user's, so the overlay can style \
             the block and R6-c can coalesce it"
        );
    }
}
