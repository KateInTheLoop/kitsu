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
//! the transitions out of it. Four keys page this view and two jump it; the
//! anchor has **twelve** transitions, three of which no key triggers at all.
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
//! | 12 | either | a **jump** with a prompt block to land on | `Pinned` at that block's first row, clamped |
//!
//! A jump that cannot **move the frame** is not a thirteenth row: it is no
//! transition at all, and the anchor — follow state included — is left exactly
//! as it was. That covers two cases under one rule: no prompt block beyond the
//! cursor in that direction, and a block whose clamped target *is* the row the
//! frame is already on. Both would break the follow for nothing, and the
//! second is the worse of the two to get wrong, because pinning there is
//! invisible at the keypress and shows up later as output that silently
//! stopped arriving (**F75**). A jump therefore never resumes following
//! either: it only declines to end it, and 5 and `Newest` stay the only two
//! routes back.
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

/// Which way a **prompt jump** looks (requirement **R6-c**).
///
/// Named by direction rather than by its key, as [`Page`] is: `{` and `}` are
/// the transcript overlay's spelling and they live in the overlay's own
/// footer, because they are live only while it holds the screen.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Jump {
    /// `{`: the nearest prompt block above the frame's top row.
    Prev,
    /// `}`: the nearest prompt block below the frame's top row.
    Next,
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

    /// The absolute ordinal of the frame's top row: the cursor every jump is
    /// measured from, and the same clamp [`Self::frame`] paints with.
    ///
    /// Shared on purpose: a cursor taken from the raw `Pinned` ordinal would
    /// disagree with the painted frame exactly when the anchor is outside the
    /// ring (transition 11), so a jump would be measured from a row that is
    /// not on screen.
    pub fn top(&self, ring: &CellRing, rows: u16) -> usize {
        let oldest = Self::oldest(ring);
        let follow_top = Self::follow_top(ring, rows);
        match self.anchor {
            Anchor::Follow => follow_top,
            Anchor::Pinned { top } => top.clamp(oldest, follow_top),
        }
    }

    /// **Pin the frame to an absolute line ordinal** (transition 12).
    ///
    /// This is the *one* seam for "put the frame on this line", and it is
    /// deliberately not a prompt-jump method: **R6-b**'s `/` search needs
    /// exactly "jump to the match" and calls this with the match's ordinal, so
    /// the two features share one anchor concept instead of growing a second.
    /// `ordinal` is clamped into the ring **at the moment of the call**, with
    /// the same bounds the frame is clamped to, so a caller may hand over an
    /// ordinal the ring has evicted or one past the newest line without
    /// inventing a frame. Clamping here rather than only in [`Self::frame`] is
    /// load-bearing: a stored ordinal past the newest line would be re-clamped
    /// against a *moving* follow position on every repaint, so the frame would
    /// chase appended lines while reporting itself pinned — following under
    /// another name.
    ///
    /// Always leaves the view **pinned**, never following, even when the
    /// clamp lands on the follow position: landing on a line in order to read
    /// it and then being scrolled off it by the next appended line is the
    /// defect, not the feature. The `Down` re-latch (transition 5) is the only
    /// route back to following that is not `Newest`.
    pub fn pin_to(&mut self, ordinal: usize, ring: &CellRing, rows: u16) {
        // The clamp is `top`'s, not a second copy of it: an ordinal pinned
        // raw would be re-clamped against a moving `follow_top` on every
        // repaint, and a clamp written out again here is the second home this
        // epic keeps paying for.
        let clamped = Self {
            anchor: Anchor::Pinned { top: ordinal },
        }
        .top(ring, rows);
        self.anchor = Anchor::Pinned { top: clamped };
    }

    /// Jump to the nearest prompt block in `jump`'s direction, and report
    /// whether there was one (transition 12).
    ///
    /// The target is the nearest [`CellRing::prompt_blocks`] ordinal strictly
    /// beyond [`Self::top`], so a jump moves between **blocks** and not
    /// between the rows of one block.
    ///
    /// A jump that cannot **move the frame** leaves the view exactly as it was
    /// — anchor, frame and [`Self::is_following`] alike — and returns `false`,
    /// so the caller can report that nothing happened. One rule, two cases:
    /// there is no block beyond the cursor, or the target **clamps back onto
    /// the cursor** because the ring cannot scroll it to the top. Either way
    /// pinning would break the follow for nothing, and in the second case it
    /// would do so invisibly — the frame does not change, so the user learns
    /// of it only when new output stops arriving (**F75**).
    ///
    /// A successful jump always **pins** (transition 12); an unsuccessful one
    /// never unpins. Neither ever resumes following: [`Page::Down`]'s
    /// re-latch and [`Page::Newest`] are the only routes back.
    pub fn jump(&mut self, jump: Jump, ring: &CellRing, rows: u16) -> bool {
        // The cursor is the painted top row, so the comparison below is
        // against what the user is looking at (transition 11).
        let cursor = self.top(ring, rows);
        let blocks = ring.prompt_blocks();
        let target = match jump {
            Jump::Prev => blocks.into_iter().rev().find(|&start| start < cursor),
            Jump::Next => blocks.into_iter().find(|&start| start > cursor),
        };
        let oldest = Self::oldest(ring);
        let follow_top = Self::follow_top(ring, rows);
        match target {
            // The guard is on the **landing**, not on the target: a block that
            // the ring cannot scroll to the top lands back on the cursor, and
            // a jump that cannot move the frame must not break the follow for
            // nothing (**F75**). Direction-symmetric by construction, so
            // `Prev` needs no arm of its own: a `Prev` target is strictly
            // below the cursor and `prompt_blocks` never reports an evicted
            // ordinal, so it can never clamp back onto it.
            Some(start) if start.clamp(oldest, follow_top) != cursor => {
                self.pin_to(start, ring, rows);
                true
            }
            _ => false,
        }
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
        // Transition 11, in the one place it can be enforced, and through
        // `top` so the clamp has one home: a view pinned on an evicted line
        // shows the oldest line that still exists rather than nothing at all,
        // and a jump's cursor is measured from the row actually painted.
        let start = self.top(ring, rows) - oldest;
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
    use super::{page_step, FullscreenView, Jump, Page};
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

    /// **R6-c's fixture**: three prompt blocks among the lines they provoked,
    /// one of them two rows long.
    ///
    /// The ordinals, done rather than assumed (**F61**), because every
    /// assertion below is stated in them:
    ///
    /// | ordinal | row | |
    /// |---|---|---|
    /// | 0 | `alpha` | **prompt**, block 1, and the ring's first line |
    /// | 1 | `answer a0` | |
    /// | 2 | `answer a1` | |
    /// | 3 | `beta one` | **prompt**, block 2 |
    /// | 4 | `beta two` | **prompt**, the second row of block 2 |
    /// | 5 | `answer b0` | |
    /// | 6 | `answer b1` | |
    /// | 7 | `gamma` | **prompt**, block 3 |
    /// | 8 | `answer c0` | |
    /// | 9 | `answer c1` | |
    ///
    /// Ten retained lines, nothing evicted, so on a **4**-row window
    /// `follow_top` is `10 - 4 = 6` and the following frame is ordinals
    /// `6..=9`.
    fn prompt_ring() -> CellRing {
        let mut ring = CellRing::new(100);
        ring.push_prompt("alpha");
        ring.push(Fd::Stdout, "answer a0");
        ring.push(Fd::Stdout, "answer a1");
        ring.push_prompt("beta one\nbeta two");
        ring.push(Fd::Stdout, "answer b0");
        ring.push(Fd::Stdout, "answer b1");
        ring.push_prompt("gamma");
        ring.push(Fd::Stdout, "answer c0");
        ring.push(Fd::Stdout, "answer c1");
        ring
    }

    /// **The fixture is the fixture** (**F99**/**F61**): the ordinals the
    /// table above claims are asserted once, here, so the jump tests below are
    /// stating a specification rather than describing whatever
    /// `prompt_ring` happened to build.
    #[test]
    fn the_prompt_jump_fixture_holds_the_blocks_the_jump_tests_assume() {
        let ring = prompt_ring();
        assert_eq!((ring.len(), ring.evicted()), (10, 0));
        assert_eq!(ring.prompt_blocks(), vec![0, 3, 7]);
        assert_eq!(
            FullscreenView::new().frame(&ring, 4, 80),
            vec!["answer b1", "gamma", "answer c0", "answer c1"],
            "so the following frame is ordinals 6..=9 and the jump cursor is 6"
        );
        assert_eq!(FullscreenView::new().top(&ring, 4), 6);
    }

    /// **Transition 12, and the card's decision: `{` moves between prompt
    /// BLOCKS, never between the rows of one.**
    ///
    /// From the following frame (cursor 6) the nearest block above is 3, which
    /// is `beta one` — the **first** row of the two-row prompt. A jump that
    /// walked marked rows instead would land on 4, `beta two`, stopping inside
    /// one thing the user typed once; the second expected frame below is
    /// asserted *against* that answer by value, so the row-stepping
    /// implementation fails with its own message rather than passing.
    ///
    /// Bidirectional, because there are two failure modes: `{` is then undone
    /// by `}` back onto the same block, so a jump that only ever moved one way
    /// (or that moved by a fixed step rather than to a block) fails too.
    #[test]
    fn a_prompt_jump_moves_between_blocks_and_never_into_the_middle_of_one() {
        let ring = prompt_ring();
        let mut view = FullscreenView::new();

        assert!(
            view.jump(Jump::Prev, &ring, 4),
            "there is a prompt block above the following frame"
        );
        let at_beta = view.frame(&ring, 4, 80);
        assert_eq!(
            at_beta,
            vec!["beta one", "beta two", "answer b0", "answer b1"],
            "the frame's top row is the block's FIRST row (ordinal 3)"
        );
        assert_ne!(
            at_beta,
            vec!["beta two", "answer b0", "answer b1", "gamma"],
            "and not its second row (ordinal 4), which is what a jump that \
             stepped marked ROWS rather than blocks would paint: a two-line \
             prompt is one stop"
        );
        assert!(!view.is_following(), "a jump pins (transition 12)");

        assert!(view.jump(Jump::Prev, &ring, 4), "and again, to block 1");
        assert_eq!(
            view.frame(&ring, 4, 80),
            vec!["alpha", "answer a0", "answer a1", "beta one"],
            "ordinal 0, the oldest block, which is also the ring's first line"
        );

        // The other direction, from the oldest block back to block 2: one
        // press, not two, because block 2 is one block however many rows it
        // spans.
        assert!(view.jump(Jump::Next, &ring, 4), "and back down");
        assert_eq!(
            view.frame(&ring, 4, 80),
            at_beta,
            "`}}` lands on the same block `{{` came from, at its first row"
        );
    }

    /// **A jump with nowhere to go is no transition at all**: the frame, the
    /// anchor and `is_following()` are all left exactly as they were, and
    /// `false` comes back.
    ///
    /// Both ends, and both follow states, because the silent-no-op is this
    /// epic's named defect shape (**F75**: a key that appears to work is a
    /// defect). The `{`-while-following half is the one that bites: a jump
    /// that pinned before discovering it had no target would break the follow
    /// for nothing, so an appended line is pushed afterwards and asserted to
    /// still reach the frame.
    #[test]
    fn a_jump_with_no_block_to_land_on_changes_nothing_at_all() {
        let ring = prompt_ring();

        // `{` from the oldest block: pinned, and it stays pinned where it is.
        let mut pinned = FullscreenView::new();
        assert!(pinned.jump(Jump::Prev, &ring, 4));
        assert!(pinned.jump(Jump::Prev, &ring, 4));
        let at_oldest = pinned.frame(&ring, 4, 80);
        assert_eq!(
            at_oldest,
            vec!["alpha", "answer a0", "answer a1", "beta one"],
            "on the oldest block"
        );
        assert!(
            !pinned.jump(Jump::Prev, &ring, 4),
            "there is no block above the oldest one, and the jump says so"
        );
        assert_eq!(pinned.frame(&ring, 4, 80), at_oldest, "and moved nothing");
        assert!(!pinned.is_following(), "and is still pinned, as it was");

        // `{` while following over a ring whose only prompt is below the
        // cursor: the follow must survive.
        let mut tail = CellRing::new(100);
        for n in 0..5 {
            tail.push(Fd::Stdout, &format!("p{n}"));
        }
        tail.push_prompt("the newest line");
        assert_eq!(tail.prompt_blocks(), vec![5], "ordinal 5, the last line");
        let mut following = FullscreenView::new();
        let opened = following.frame(&tail, 3, 80);
        assert_eq!(
            opened,
            vec!["p3", "p4", "the newest line"],
            "a 3-row window over 6 lines is ordinals 3..=5"
        );
        assert!(
            !following.jump(Jump::Prev, &tail, 3),
            "the cursor is ordinal 3 and the only block is at 5, so there is \
             nothing above"
        );
        assert_eq!(following.frame(&tail, 3, 80), opened);
        assert!(
            following.is_following(),
            "and the follow is intact: a jump that had nowhere to go must not \
             have quietly broken it"
        );
        tail.push(Fd::Stdout, "and one more");
        assert_eq!(
            following.frame(&tail, 3, 80).last().map(String::as_str),
            Some("and one more"),
            "following is live, not just a flag"
        );
    }

    /// **A ring with no prompts at all leaves both keys inert**, in both
    /// directions and from both follow states.
    ///
    /// The case a feature built on a mark forgets: before the user has
    /// submitted anything — or after their prompts have aged out of the ring —
    /// `{`/`}` have nothing to jump between, and landing "somewhere" would be
    /// worse than landing nowhere.
    #[test]
    fn a_ring_with_no_prompts_leaves_both_jump_keys_inert() {
        let ring = ring_of(100, 10);
        assert_eq!(
            ring.prompt_blocks(),
            Vec::<usize>::new(),
            "the fixture really has no prompts"
        );

        for pinned_first in [false, true] {
            let mut view = FullscreenView::new();
            if pinned_first {
                view.page(Page::Oldest, &ring, 4);
            }
            let before = view.frame(&ring, 4, 80);
            let following_before = view.is_following();
            assert!(!before.is_empty(), "the frame paints something first");
            for jump in [Jump::Prev, Jump::Next] {
                assert!(
                    !view.jump(jump, &ring, 4),
                    "{jump:?} has no block to land on (pinned_first = \
                     {pinned_first})"
                );
                assert_eq!(view.frame(&ring, 4, 80), before, "{jump:?} moved nothing");
                assert_eq!(view.is_following(), following_before);
            }
        }
    }

    /// **A jump whose target resolves to the row the frame is already on
    /// changes nothing at all** — frame, anchor and `is_following()` alike —
    /// and reports no movement. This is the `}`-at-the-bottom case.
    ///
    /// **This is not a separate decision: it is the no-target rule.**
    /// `a_jump_with_no_block_to_land_on_changes_nothing_at_all` holds that a
    /// jump which cannot move the frame must not break the follow *for
    /// nothing*. Here the clamp makes the target **unreachable as a top row**,
    /// so the jump equally cannot move the frame, so the same trade gets the
    /// same answer. The two must not be re-litigated apart.
    ///
    /// The consequence of the other answer is worse than a declined key,
    /// which is why the rule is this way round: pinning here is invisible at
    /// the moment it happens — the user presses `}` and **sees nothing
    /// change** — and visible only later, when new output silently stops
    /// arriving. That is **F75**'s second kind, *a key that appears to work is
    /// a defect*, with a delayed symptom that is harder to attribute than an
    /// immediate one. "There is no next prompt beyond the one you are on" is a
    /// defensible report; "following is now off and I will not tell you" is
    /// not.
    ///
    /// **And this is emphatically not `}` re-latching the follow.** A jump
    /// never resumes following; it only declines to *end* it. Transition 5
    /// (`Down` onto the newest line) and `Newest` remain the only two routes
    /// back, exactly as the module's table says.
    ///
    /// The arithmetic (**F61**): the cursor is 6 and the newest block is 7, so
    /// a block really is below the cursor — this is *not* the "no block at
    /// all" case, and `gamma` is asserted to be on screen to prove it.
    /// `follow_top` is also 6, so the target clamps to 6, which is the cursor,
    /// so there is no row to move to.
    ///
    /// Not vacuous, and in both directions: the frame is compared **by value**
    /// so a jump that moved it fails, and the follow is proved **live** rather
    /// than merely flagged by appending a line and seeing it enter the frame —
    /// so the pinning answer fails here with its own message.
    #[test]
    fn a_jump_onto_a_block_already_at_the_frames_top_changes_nothing_at_all() {
        let mut ring = prompt_ring();
        let mut view = FullscreenView::new();
        let opened = view.frame(&ring, 4, 80);
        assert_eq!(view.top(&ring, 4), 6, "the cursor, and follow_top");
        assert_eq!(
            ring.prompt_blocks(),
            vec![0, 3, 7],
            "there IS a block below the cursor: this is not the no-block case"
        );
        assert!(
            opened.contains(&"gamma".to_string()),
            "and it is already on screen: {opened:?}"
        );

        assert!(
            !view.jump(Jump::Next, &ring, 4),
            "the target clamps to 6, which is the row the frame is already \
             on, so the jump has nowhere to move to and says so"
        );
        assert_eq!(view.frame(&ring, 4, 80), opened, "and it moved nothing");
        assert!(
            view.is_following(),
            "and did not break the follow for nothing, which is the same \
             trade the no-target rule already makes"
        );

        // The follow is **live**, not a flag: this is the assertion the
        // pinning answer fails, and it is the symptom the user would have
        // seen later rather than at the keypress.
        ring.push(Fd::Stdout, "answer c2");
        assert_eq!(
            view.frame(&ring, 4, 80).last().map(String::as_str),
            Some("answer c2"),
            "new output still arrives: a `}}` that had nowhere to go must not \
             have silently stopped the transcript updating"
        );

        // Repeating it is still inert, on the ring as it now stands: the
        // append moved `follow_top` to 7, so the cursor is 7 and the block at
        // 7 is the row the frame is on.
        assert_eq!(view.top(&ring, 4), 7);
        assert!(!view.jump(Jump::Next, &ring, 4));
        assert!(view.is_following());
    }

    /// **A prompt block evicted past the anchor stops being a jump target**,
    /// and the jump then reports that rather than landing on an ordinal the
    /// ring can no longer paint (transition 11 meeting transition 12).
    ///
    /// The arithmetic: a 6-line ring holds prompts at ordinals 0 and 3; `{`
    /// from the following frame pins on ordinal 0. Four more pushes evict
    /// ordinals 0..=3 — both blocks — leaving `oldest = 4`, `follow_top =
    /// 10 - 3 = 7` and no marked row retained, so the clamped frame is
    /// ordinals 4..=6 and both keys are inert.
    #[test]
    fn a_prompt_block_evicted_past_the_anchor_is_no_longer_a_jump_target() {
        let mut ring = CellRing::new(6);
        ring.push_prompt("old prompt");
        ring.push(Fd::Stdout, "x1");
        ring.push(Fd::Stdout, "x2");
        ring.push_prompt("mid prompt");
        ring.push(Fd::Stdout, "x4");
        ring.push(Fd::Stdout, "x5");
        assert_eq!(ring.prompt_blocks(), vec![0, 3]);

        let mut view = FullscreenView::new();
        assert!(view.jump(Jump::Prev, &ring, 3), "onto ordinal 0");
        assert_eq!(
            view.frame(&ring, 3, 80),
            vec!["old prompt", "x1", "x2"],
            "the oldest block"
        );

        for n in 6..10 {
            ring.push(Fd::Stdout, &format!("x{n}"));
        }
        assert_eq!(ring.evicted(), 4, "both blocks are gone from the process");
        assert_eq!(ring.prompt_blocks(), Vec::<usize>::new());
        let clamped = view.frame(&ring, 3, 80);
        assert_eq!(
            clamped,
            vec!["x4", "x5", "x6"],
            "the pinned frame is clamped back into the ring (transition 11)"
        );
        for jump in [Jump::Prev, Jump::Next] {
            assert!(
                !view.jump(jump, &ring, 3),
                "{jump:?} has nothing retained to land on"
            );
            assert_eq!(view.frame(&ring, 3, 80), clamped);
            assert!(!view.is_following(), "and the clamp did not release it");
        }
    }

    /// **`pin_to` is the ordinal seam, and it is the seam R6-b inherits.**
    ///
    /// `{`/`}` are a *policy* over ordinals; "put the frame on this line" is
    /// the mechanism, and **R6-b**'s `/` search needs exactly the mechanism
    /// and nothing else — it hands over the match's ordinal. Asserting the
    /// mechanism separately from the policy is what keeps search from growing
    /// a second anchor concept to drift from this one.
    ///
    /// The clamp is asserted at **both** ends and, past the newest line, by
    /// appending: a `pin_to` that merely stored the ordinal and let
    /// [`FullscreenView::frame`] clamp it would re-clamp against a moving
    /// `follow_top` on every repaint and chase appended lines while reporting
    /// itself pinned.
    #[test]
    fn pin_to_puts_the_frame_on_an_absolute_ordinal_and_always_pins() {
        let mut ring = prompt_ring();

        let mut view = FullscreenView::new();
        view.pin_to(2, &ring, 4);
        assert_eq!(
            view.frame(&ring, 4, 80),
            vec!["answer a1", "beta one", "beta two", "answer b0"],
            "ordinal 2 is the top row, which is not a block boundary: the \
             mechanism is ordinals, not prompts"
        );
        assert_eq!(view.top(&ring, 4), 2);
        assert!(!view.is_following());

        // Below the oldest retained line: clamped up, not wrapped.
        view.pin_to(0, &ring, 4);
        assert_eq!(view.top(&ring, 4), 0);
        let mut evicting = CellRing::new(6);
        for n in 0..20 {
            evicting.push(Fd::Stdout, &format!("line {n}"));
        }
        let mut low = FullscreenView::new();
        low.pin_to(3, &evicting, 3);
        assert_eq!(
            low.frame(&evicting, 3, 80),
            vec!["line 14", "line 15", "line 16"],
            "ordinal 3 is evicted, so the clamp is the oldest retained line"
        );

        // Past the newest line: clamped down, and genuinely pinned there.
        let mut high = FullscreenView::new();
        high.pin_to(999, &ring, 4);
        assert!(!high.is_following(), "pinning never resumes the follow");
        let at_bottom = high.frame(&ring, 4, 80);
        assert_eq!(
            at_bottom,
            vec!["answer b1", "gamma", "answer c0", "answer c1"],
            "clamped to follow_top = 6"
        );
        ring.push(Fd::Stdout, "answer c2");
        assert_eq!(
            high.frame(&ring, 4, 80),
            at_bottom,
            "and it stays there: the ordinal was clamped when it was stored, \
             so the frame does not chase the newest line"
        );
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
