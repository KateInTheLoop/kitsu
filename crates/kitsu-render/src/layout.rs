//! Pure source↔cell layout for the inline composer (Stage 1A).
//!
//! This is the single authority the renderer and vertical navigation share:
//! given the draft's source text, the first-row prompt, the cell width, and a
//! display projection, it computes where every source boundary lands in
//! terminal cells and the reverse. It depends on nothing terminal — no ANSI,
//! no `InlineSurface`, no file descriptors. Contracts live in
//! `docs/tui-editor-contracts.md`.
//!
//! Source offsets are Unicode scalar (`char`) boundaries, matching
//! `TypeaheadEditor::cursor()`. Visual positions are zero-based cells within
//! physical rows. Wide characters occupy two cells; combining marks occupy
//! zero. Explicit newlines break rows; tabs display as `⇥` (one cell), matching
//! the live renderer rather than expanding to an environment tab stop. The
//! prompt occupies cells on the first row only. The full draft is laid out
//! before any viewport clipping — the first visible row is not necessarily the
//! first draft row.

/// Display width of one source scalar in terminal cells. A combining mark is
/// zero; an East-Asian wide / fullwidth glyph is two; a tab is one (shown as
/// `⇥`); everything else is one. Control characters are treated as zero — the
/// editor sanitizes them out of the draft before layout, so they never reach a
/// real cell, and a stray one must not desync the mapping.
fn cell_width(ch: char) -> usize {
    // One width authority for the whole crate (review): the layout must wrap
    // and pad with exactly the policy `fit_row_to_width` measures at paint
    // time, or a combining mark / emoji they disagree on pushes the box border
    // off its column.
    crate::char_display_width(ch)
}

/// Display width of `text` in terminal cells under this module's policy —
/// the authority the layout and the painter must share so a padded row is
/// exactly as wide as it was laid out.
pub fn text_width(text: &str) -> usize {
    text.chars().map(cell_width).sum()
}

/// Fit already-projected display text to exactly `cells` columns: truncate on
/// a cell boundary if it is wider (a wide glyph that would straddle the edge
/// is dropped and a space keeps the width exact), then pad with spaces. The
/// box painter uses this so a combining mark, an emoji, or a prompt wider than
/// the frame cannot push the right border off its column.
pub fn clamp_to_width(text: &str, cells: usize) -> String {
    let mut out = String::with_capacity(text.len());
    let mut used = 0usize;
    for ch in text.chars() {
        let w = cell_width(ch);
        if used + w > cells {
            break;
        }
        out.push(ch);
        used += w;
    }
    for _ in used..cells {
        out.push(' ');
    }
    out
}

/// A projection atom: a contiguous source range that lays out as one unit with
/// an independently measured label width. A paste token (Stage 2A) is one atom
/// spanning many source scalars whose label width differs from its backing
/// text; [`Layout::compute`] lays out ordinary text without building these.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Atom {
    /// Source scalar range `[start, end)` into the draft (prompt excluded).
    pub source: std::ops::Range<usize>,
    /// Cells this atom occupies when painted.
    pub width: usize,
    /// Glyphs painted for this atom (already projected; no newlines).
    pub glyphs: String,
    /// A hard row break (a source `'\n'`), not a painted atom.
    pub breaks: bool,
}

/// A source boundary placed in the grid: which physical row, which cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pos {
    pub row: usize,
    pub col: usize,
}

/// One physical row of the laid-out draft.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    /// Painted glyphs for this row (prompt excluded; add it when painting row 0).
    pub glyphs: String,
    /// Cells the content occupies (prompt excluded).
    pub width: usize,
    /// The source scalar index where this row's content begins. Used to bound
    /// [`Layout::cell_to_source`]'s scan to one row's worth of boundaries.
    pub start: usize,
}

/// The full-draft layout: every physical row, plus the cell position of each
/// source boundary `0..=len`. `boundary[i]` is where scalar index `i` begins;
/// `boundary[len]` is the end-of-draft caret slot.
#[derive(Clone, Debug)]
pub struct Layout {
    pub rows: Vec<Row>,
    boundaries: Vec<Pos>,
}

/// Does this scalar need the full sanitizer, or can it be laid out as itself?
///
/// ASCII graphic characters and the space pass through
/// [`crate::sanitize_terminal_text`] unchanged at one cell, which is the
/// overwhelming bulk of any typed or pasted draft (a code paste is almost
/// entirely this). Everything else — controls, bidi/format controls, line and
/// paragraph separators, and wide or combining glyphs — takes the sanitizing
/// path. This is the fast path review 8 calls for: it avoids a per-scalar
/// `String` allocation and sanitizer call for the common character.
fn is_plain_ascii(ch: char) -> bool {
    ch == ' ' || ch.is_ascii_graphic()
}

/// Builds rows and the source→cell boundary table from a stream of display
/// units, so [`Layout::compute`] (a direct char walk) and
/// [`Layout::compute_from_atoms`] (the paste path) share one layout loop
/// without either allocating the other's intermediate form.
struct RowBuilder {
    total_width: usize,
    prompt_cells: usize,
    rows: Vec<Row>,
    boundaries: Vec<Pos>,
    row_glyphs: String,
    row_width: usize,
    row_start: usize,
    row_index: usize,
    first_row: bool,
    /// Content column of the last positive-width glyph on this row, so a
    /// zero-width combining mark shares that cell instead of the advanced one.
    last_visible_col: Option<usize>,
}

impl RowBuilder {
    fn new(prompt_cells: usize, total_width: usize, scalar_count: usize) -> Self {
        RowBuilder {
            total_width,
            prompt_cells,
            rows: Vec::new(),
            boundaries: vec![Pos { row: 0, col: 0 }; scalar_count + 1],
            row_glyphs: String::new(),
            row_width: 0,
            row_start: 0,
            row_index: 0,
            first_row: true,
            last_visible_col: None,
        }
    }

    /// Usable content cells on the current row (the prompt eats into row 0).
    fn avail(&self) -> usize {
        if self.first_row {
            self.total_width.saturating_sub(self.prompt_cells).max(1)
        } else {
            self.total_width
        }
    }

    fn prompt_offset(&self) -> usize {
        if self.first_row {
            self.prompt_cells
        } else {
            0
        }
    }

    fn flush(&mut self) {
        self.rows.push(Row {
            glyphs: std::mem::take(&mut self.row_glyphs),
            width: self.row_width,
            start: self.row_start,
        });
        self.row_width = 0;
    }

    /// A hard row break at source index `src` (a `'\n'`).
    fn push_break(&mut self, src: usize) {
        self.boundaries[src] = Pos {
            row: self.row_index,
            col: self.prompt_offset() + self.row_width,
        };
        self.flush();
        self.row_index += 1;
        self.first_row = false;
        self.row_start = src + 1;
        self.last_visible_col = None;
    }

    /// One painted unit spanning source range `src`, `width` cells wide, drawn
    /// as `glyphs`. A multi-scalar (paste) unit collapses its interior
    /// boundaries onto its start cell; a zero-width unit shares the previous
    /// visible cell.
    fn push_unit(&mut self, src: std::ops::Range<usize>, width: usize, glyphs: &str) {
        // Soft-wrap a positive-width unit that no longer fits. A fresh row
        // always offers a full width, so a unit wider than the whole row (an
        // oversized paste label) still gets its own row rather than looping.
        if width > 0 && self.row_width + width > self.avail() && self.row_width > 0 {
            self.flush();
            self.row_index += 1;
            self.first_row = false;
            self.row_start = src.start;
            self.last_visible_col = None;
        }
        let content_col = if width == 0 {
            self.last_visible_col.unwrap_or(self.row_width)
        } else {
            self.row_width
        };
        let here = Pos {
            row: self.row_index,
            col: self.prompt_offset() + content_col,
        };
        for b in src {
            self.boundaries[b] = here;
        }
        self.row_glyphs.push_str(glyphs);
        if width > 0 {
            self.last_visible_col = Some(self.row_width);
        }
        self.row_width += width;
    }

    fn finish(mut self, scalar_count: usize) -> Layout {
        // The end-of-draft boundary sits just past the last content cell.
        self.boundaries[scalar_count] = Pos {
            row: self.row_index,
            col: self.prompt_offset() + self.row_width,
        };
        self.flush();
        Layout {
            rows: self.rows,
            boundaries: self.boundaries,
        }
    }
}

fn prompt_width(prompt: &str) -> usize {
    prompt.chars().map(cell_width).sum()
}

impl Layout {
    /// Lay the whole draft out at `width` cells with a first-row `prompt`.
    /// `width` is clamped to at least one usable content cell. The prompt
    /// occupies cells on row 0 only; if it alone fills the width, content still
    /// gets a one-cell minimum so the caret has a home. The full draft is laid
    /// out before any viewport clipping — the first visible row is not
    /// necessarily the first draft row.
    pub fn compute(source: &str, prompt: &str, width: usize) -> Layout {
        let scalar_count = source.chars().count();
        let mut b = RowBuilder::new(prompt_width(prompt), width.max(1), scalar_count);
        let mut buf = [0u8; 4];
        for (i, ch) in source.chars().enumerate() {
            if ch == '\n' {
                b.push_break(i);
            } else if ch == '\t' {
                b.push_unit(i..i + 1, 1, "⇥"); // matches live::display_char
            } else if is_plain_ascii(ch) {
                b.push_unit(i..i + 1, 1, ch.encode_utf8(&mut buf));
            } else {
                // Sanitize so a bidi control paints its visible `[U+202E]`
                // marker and a stray control paints as nothing — one source
                // boundary either way.
                let glyphs = crate::sanitize_terminal_text(&ch.to_string());
                b.push_unit(i..i + 1, glyphs.chars().map(cell_width).sum(), &glyphs);
            }
        }
        b.finish(scalar_count)
    }

    /// Lay out a draft already projected into [`Atom`]s (the Stage 2A paste
    /// path): a paste token is one wide atom over many source scalars. Atoms
    /// must be contiguous and cover `[0, len)` in source order.
    pub fn compute_from_atoms(atoms: &[Atom], prompt: &str, width: usize) -> Layout {
        let scalar_count = atoms.last().map_or(0, |a| a.source.end);
        let mut b = RowBuilder::new(prompt_width(prompt), width.max(1), scalar_count);
        for atom in atoms {
            if atom.breaks {
                b.push_break(atom.source.start);
            } else {
                b.push_unit(atom.source.clone(), atom.width, &atom.glyphs);
            }
        }
        b.finish(scalar_count)
    }

    /// Cell position of a source boundary. An out-of-range index clamps to the
    /// end-of-draft slot.
    pub fn source_to_cell(&self, source_index: usize) -> Pos {
        let i = source_index.min(self.boundaries.len() - 1);
        self.boundaries[i]
    }

    /// Source boundary nearest a requested visual target. A target in the
    /// middle of a wide glyph lands on that glyph's start boundary (affinity
    /// toward the earlier boundary). A target past a row's content lands on the
    /// last boundary of that row; a target below the last row clamps to
    /// end-of-draft.
    pub fn cell_to_source(&self, target: Pos) -> usize {
        // Boundaries are monotonic in row as the source index grows, so one
        // row's boundaries are a contiguous slice — scan only that slice
        // (review 8: the old scan walked every boundary in the draft). The
        // slice runs from this row's first source index to the next row's
        // (or end-of-draft on the last row), inclusive of that end boundary.
        let last = self.boundaries.len() - 1;
        let (lo, hi) = match self.rows.get(target.row) {
            Some(row) => {
                let hi = self
                    .rows
                    .get(target.row + 1)
                    .map_or(last, |next| next.start);
                (row.start, hi.min(last))
            }
            // No such row (target below the draft): clamp to end-of-draft.
            None => return last,
        };
        let mut best: Option<(usize, usize)> = None; // (source_index, |col delta|)
        for idx in lo..=hi {
            let pos = self.boundaries[idx];
            if pos.row != target.row {
                continue;
            }
            // Affinity: on a tie prefer the boundary at or before the target
            // column, and among boundaries sharing one cell (a base and its
            // combining mark) keep the earliest, so motion never splits them.
            let delta = pos.col.abs_diff(target.col);
            let prefer = match best {
                None => true,
                Some((best_idx, best_delta)) => {
                    delta < best_delta
                        || (delta == best_delta
                            && pos.col <= target.col
                            && self.boundaries[best_idx].col > target.col)
                }
            };
            if prefer {
                best = Some((idx, delta));
            }
        }
        best.map_or(last, |(idx, _)| idx)
    }

    /// One visual row of `↑`/`↓` motion from `cursor`, aiming for
    /// `preferred_col` (else the cursor's own cell). Returns the new source
    /// boundary and the column to keep preferring, or `None` when `cursor` is
    /// already on the first (`up`) or last row -- the full-draft boundary where
    /// the key falls through to history.
    pub fn vertical(
        &self,
        cursor: usize,
        preferred_col: Option<usize>,
        up: bool,
    ) -> Option<(usize, usize)> {
        let here = self.source_to_cell(cursor);
        let row = if up {
            here.row.checked_sub(1)?
        } else {
            here.row + 1
        };
        if row >= self.rows.len() {
            return None;
        }
        let col = preferred_col.unwrap_or(here.col);
        Some((self.cell_to_source(Pos { row, col }), col))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout(source: &str, prompt: &str, width: usize) -> Layout {
        Layout::compute(source, prompt, width)
    }

    /// Fixture 1: empty draft with a long prompt. One row; the only boundary is
    /// end-of-draft, parked just past the prompt.
    #[test]
    fn empty_draft_with_long_prompt() {
        let l = layout("", "kitsu very long prompt> ", 40);
        assert_eq!(l.rows.len(), 1);
        let end = l.source_to_cell(0);
        assert_eq!(end, Pos { row: 0, col: 24 }, "caret parks past the prompt");
        // And the reverse: a click just past the prompt is the empty draft.
        assert_eq!(l.cell_to_source(Pos { row: 0, col: 24 }), 0);
    }

    /// Fixture 2: a newline at the start and at the end.
    #[test]
    fn leading_and_trailing_newline() {
        let l = layout("\nabc\n", "> ", 40);
        assert_eq!(l.rows.len(), 3, "break, 'abc', break → 3 rows");
        // 'a' is source index 1, on row 1 at col 0 (no prompt off row 0).
        assert_eq!(l.source_to_cell(1), Pos { row: 1, col: 0 });
        // The leading newline boundary (index 0) is on row 0 at the prompt.
        assert_eq!(l.source_to_cell(0), Pos { row: 0, col: 2 });
        // End-of-draft is on the final empty row.
        assert_eq!(l.source_to_cell(5), Pos { row: 2, col: 0 });
    }

    /// Fixture 3: a soft wrap at exactly the last usable cell. Width 5, prompt
    /// 2 → 3 content cells on row 0, full 5 after. "abcdef" wraps.
    #[test]
    fn soft_wrap_at_exact_boundary() {
        let l = layout("abcdef", "> ", 5);
        // Row 0 holds "abc" (3 usable cells), row 1 holds "def".
        assert_eq!(l.rows.len(), 2);
        assert_eq!(l.rows[0].glyphs, "abc");
        assert_eq!(l.rows[1].glyphs, "def");
        assert_eq!(
            l.source_to_cell(2),
            Pos { row: 0, col: 4 },
            "'c' last on row 0"
        );
        assert_eq!(
            l.source_to_cell(3),
            Pos { row: 1, col: 0 },
            "'d' wraps to row 1"
        );
    }

    /// Fixture 4: a width-two glyph at the wrap boundary moves whole to the next
    /// row rather than splitting across cells.
    #[test]
    fn wide_char_at_wrap_boundary() {
        // Width 4, no prompt: "ab" + '中'(2) would need cell 2..4 — fits exactly.
        // "abc中": 'a','b','c' fill 0..3, '中' needs 2 cells but only 1 remains.
        let l = layout("abc中", "", 4);
        assert_eq!(l.rows.len(), 2);
        assert_eq!(l.rows[0].glyphs, "abc");
        assert_eq!(l.rows[1].glyphs, "中");
        assert_eq!(
            l.source_to_cell(3),
            Pos { row: 1, col: 0 },
            "wide glyph whole on row 1"
        );
        // A target in the middle of the wide glyph lands on its start boundary.
        assert_eq!(l.cell_to_source(Pos { row: 1, col: 1 }), 3);
    }

    /// Fixture 5: a base character plus a combining mark share one cell and one
    /// start boundary; the combining mark adds zero width.
    #[test]
    fn base_plus_combining_mark() {
        // "e" + U+0301 (combining acute) — one cell wide together.
        let l = layout("e\u{0301}x", "", 10);
        assert_eq!(l.rows.len(), 1);
        assert_eq!(l.rows[0].width, 2, "é (1 cell) + x (1 cell)");
        assert_eq!(l.source_to_cell(0), Pos { row: 0, col: 0 }, "base at col 0");
        assert_eq!(
            l.source_to_cell(1),
            Pos { row: 0, col: 0 },
            "mark shares the cell"
        );
        assert_eq!(
            l.source_to_cell(2),
            Pos { row: 0, col: 1 },
            "x after the one cell"
        );
    }

    /// Fixture 6: multiple wraps above the visible slice — the viewport keeps
    /// the anchored row and reports where it starts.
    #[test]
    fn wraps_above_the_visible_slice() {
        // The whole draft is laid out before any viewport clip: all four rows
        // exist, and Row.start pins where each one begins in the source, which
        // is what lets the caller window it (and cell_to_source scan one row).
        let l = layout("aaaabbbbccccdddd", "", 4); // 4 rows of 4
        assert_eq!(l.rows.len(), 4);
        assert_eq!(l.source_to_cell(15).row, 3, "last 'd' is on row 3");
        assert_eq!(l.rows[2].glyphs, "cccc");
        assert_eq!(l.rows[2].start, 8);
        assert_eq!(l.rows[3].start, 12);
    }

    /// Fixture 7: a paste label wider than the frame (the Stage 2A atom path)
    /// gets its own row and its interior boundaries collapse to the atom's
    /// start — you cannot land the caret inside the token.
    #[test]
    fn paste_label_wider_than_frame() {
        let atoms = [Atom {
            source: 0..5,
            width: 20,
            glyphs: "[Pasted text #1 +9 lines]".to_string(),
            breaks: false,
        }];
        let l = Layout::compute_from_atoms(&atoms, "", 8);
        assert_eq!(l.rows.len(), 1, "oversized atom still gets one row");
        assert_eq!(l.rows[0].width, 20);
        // Every interior source boundary maps to the token's start cell.
        for i in 0..5 {
            assert_eq!(l.source_to_cell(i), Pos { row: 0, col: 0 });
        }
        // End-of-draft is just past the label.
        assert_eq!(l.source_to_cell(5), Pos { row: 0, col: 20 });
    }

    /// A multi-row paste atom stream still lays out and maps correctly: a
    /// break between two tokens, each a single wide atom.
    #[test]
    fn paste_atoms_across_a_row_break() {
        let atoms = [
            Atom {
                source: 0..3,
                width: 4,
                glyphs: "[#1]".to_string(),
                breaks: false,
            },
            Atom {
                source: 3..4,
                width: 0,
                glyphs: String::new(),
                breaks: true,
            },
            Atom {
                source: 4..7,
                width: 4,
                glyphs: "[#2]".to_string(),
                breaks: false,
            },
        ];
        let l = Layout::compute_from_atoms(&atoms, "> ", 40);
        assert_eq!(l.rows.len(), 2);
        assert_eq!(l.source_to_cell(0), Pos { row: 0, col: 2 }, "after prompt");
        assert_eq!(l.source_to_cell(4), Pos { row: 1, col: 0 });
        assert_eq!(l.source_to_cell(7), Pos { row: 1, col: 4 });
    }

    /// Sanitized display text: a bidi control keeps one source boundary but paints
    /// its visible marker's width, and the boundaries after it shift by that width.
    #[test]
    fn bidi_control_paints_its_visible_marker() {
        let l = layout("a\u{202e}b", "", 40);
        assert_eq!(l.rows[0].glyphs, "a[U+202E]b");
        assert_eq!(l.source_to_cell(1), Pos { row: 0, col: 1 });
        assert_eq!(l.source_to_cell(2), Pos { row: 0, col: 9 });
    }

    /// Vertical motion keeps its preferred column through a short row, never
    /// splits a base from its combining mark, and reports the draft boundaries.
    #[test]
    fn vertical_motion_keeps_the_preferred_column() {
        // Rows: "> abcdef" / "x" / "ghijkl"; the caret starts after 'e' (col 6).
        let l = layout("abcdef\nx\nghijkl", "> ", 40);
        let start = 4; // 'e'
        assert_eq!(l.source_to_cell(start), Pos { row: 0, col: 6 });
        assert_eq!(
            l.vertical(start, None, true),
            None,
            "first row: falls to history"
        );
        let (short, col) = l.vertical(start, None, false).unwrap();
        assert_eq!(short, 8, "short row clamps to its end (the newline slot)");
        assert_eq!(col, 6, "but the preferred column survives");
        let (back, _) = l.vertical(short, Some(col), false).unwrap();
        assert_eq!(
            l.source_to_cell(back),
            Pos { row: 2, col: 6 },
            "'k' under 'e'"
        );
        assert_eq!(l.vertical(back, Some(col), false), None, "last row");

        // A combining mark shares its base's cell: landing there picks the base.
        let m = layout("ab\ne\u{0301}x", "", 40);
        assert_eq!(m.vertical(0, None, false), Some((3, 0)));
    }

    /// clamp_to_width fits to exactly `cells` under the layout's own width
    /// policy: a combining mark adds no width (review F: the heuristic pad
    /// left the border a column short), a wide glyph is two and is dropped
    /// whole rather than straddling the edge, and text wider than the frame
    /// (a long prompt on a narrow terminal) is truncated, not overflowed.
    #[test]
    fn clamp_to_width_is_exact_in_cells() {
        assert_eq!(clamp_to_width("ab", 4), "ab  ");
        assert_eq!(text_width(&clamp_to_width("e\u{0301}x", 4)), 4); // é + x = 2 cells
        assert_eq!(clamp_to_width("e\u{0301}x", 4), "e\u{0301}x  ");
        // '中' is two cells: at width 3 only one fits, the second is dropped
        // and a space keeps the width exact rather than straddling the edge.
        assert_eq!(clamp_to_width("中中", 3), "中 ");
        assert_eq!(text_width(&clamp_to_width("中中", 3)), 3);
        // A prompt wider than the frame is truncated to the frame.
        assert_eq!(clamp_to_width("kitsu> a", 4), "kits");
        // An emoji counts as two cells under this policy.
        assert_eq!(text_width(&clamp_to_width("✅x", 5)), 5);
    }

    /// cell_to_source past a row's content clamps to that row's last boundary,
    /// and below the draft clamps to end-of-draft.
    #[test]
    fn hit_positions_clamp() {
        let l = layout("ab\ncd", "> ", 40);
        // Far right of row 0 → end of "ab" content (index 2, the newline slot).
        assert_eq!(l.cell_to_source(Pos { row: 0, col: 99 }), 2);
        // Below everything → end-of-draft (index 5).
        assert_eq!(l.cell_to_source(Pos { row: 9, col: 0 }), 5);
    }
}
