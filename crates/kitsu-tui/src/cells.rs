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
    pub fn push(&mut self, fd: Fd, text: &str) {
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
        });
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
}
