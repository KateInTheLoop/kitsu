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

use unicode_width::UnicodeWidthChar;

/// Display width of one source scalar in terminal cells. A combining mark is
/// zero; an East-Asian wide / fullwidth glyph is two; a tab is one (shown as
/// `⇥`); everything else is one. Control characters are treated as zero — the
/// editor sanitizes them out of the draft before layout, so they never reach a
/// real cell, and a stray one must not desync the mapping.
fn cell_width(ch: char) -> usize {
    if ch == '\t' {
        return 1;
    }
    UnicodeWidthChar::width(ch).unwrap_or(0)
}

/// The glyph a source scalar paints as. Newlines never reach here (they break
/// rows); tabs render as `⇥`, matching `live::display_char`.
fn display_glyph(ch: char) -> char {
    if ch == '\t' {
        '⇥'
    } else {
        ch
    }
}

/// A projection atom: a contiguous source range that lays out as one unit with
/// an independently measured label width. Ordinary text is one atom per
/// scalar; a paste token (Stage 2A) is one atom spanning many source scalars
/// whose label width differs from its backing text. Reserved here so the
/// layout authority already accounts for label width without 2A's editing.
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

impl Atom {
    /// One ordinary source scalar as its own atom.
    /// The glyph is sanitized, so a bidi control paints as its visible
    /// `[U+202E]` marker and a stray control paints as nothing -- one source
    /// boundary either way.
    fn scalar(index: usize, ch: char) -> Self {
        let glyphs = crate::sanitize_terminal_text(&display_glyph(ch).to_string());
        Atom {
            source: index..index + 1,
            width: glyphs.chars().map(cell_width).sum(),
            glyphs,
            breaks: false,
        }
    }
}

/// How the draft projects to display atoms. The default is one atom per source
/// scalar; later a paste-aware projection replaces chosen ranges with single
/// wide-label atoms. Returned atoms must be contiguous and cover `[0, len)` of
/// the source in order; a `'\n'` is never inside an atom (it is a row break).
pub trait Projection {
    /// Project `source` (the whole draft) into display atoms in source order.
    fn atoms(&self, source: &str) -> Vec<Atom>;
}

/// The identity projection: every scalar is its own atom. Newlines are emitted
/// as zero-width atoms so callers that walk atoms still see them; the layout
/// pass treats them as row breaks.
#[derive(Clone, Copy, Debug, Default)]
pub struct ScalarProjection;

impl Projection for ScalarProjection {
    fn atoms(&self, source: &str) -> Vec<Atom> {
        source
            .chars()
            .enumerate()
            .map(|(i, ch)| {
                if ch == '\n' {
                    Atom {
                        source: i..i + 1,
                        width: 0,
                        glyphs: String::new(),
                        breaks: true,
                    }
                } else {
                    Atom::scalar(i, ch)
                }
            })
            .collect()
    }
}

/// A source boundary placed in the grid: which physical row, which cell, and
/// whether the prompt sits before it on that row.
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
    /// The source scalar index where this row's content begins.
    pub start: usize,
    /// Leading cells reserved by the prompt (row 0 only; else 0).
    pub prompt_cells: usize,
}

/// The full-draft layout: every physical row, plus the cell position of each
/// source boundary `0..=len`. `boundary[i]` is where scalar index `i` begins;
/// `boundary[len]` is the end-of-draft caret slot.
#[derive(Clone, Debug)]
pub struct Layout {
    pub rows: Vec<Row>,
    boundaries: Vec<Pos>,
}

impl Layout {
    /// Lay the whole draft out at `width` cells with a first-row `prompt`,
    /// using `projection`. `width` is clamped to at least one usable content
    /// cell. The prompt occupies cells on row 0 only; if it alone fills the
    /// width, content still gets a one-cell minimum so the caret has a home.
    pub fn compute(
        source: &str,
        prompt: &str,
        width: usize,
        projection: &dyn Projection,
    ) -> Layout {
        let total_width = width.max(1);
        let prompt_cells: usize = prompt.chars().map(cell_width).sum();

        let mut rows: Vec<Row> = Vec::new();
        let scalar_count = source.chars().count();
        // boundary[i] for i in 0..=scalar_count.
        let mut boundaries: Vec<Pos> = vec![Pos { row: 0, col: 0 }; scalar_count + 1];

        // Row-building state.
        let mut row_glyphs = String::new();
        let mut row_width = 0usize; // content cells used (prompt excluded)
        let mut row_start = 0usize; // source index this row's content began at
        let mut row_index = 0usize;
        let mut first_row = true;
        // Content column of the last positive-width glyph on this row, so a
        // zero-width combining mark can share that cell instead of the advanced
        // column. Reset at every row break.
        let mut last_visible_col: Option<usize> = None;

        // Usable content cells on the current row.
        let avail = |first: bool| -> usize {
            if first {
                total_width.saturating_sub(prompt_cells).max(1)
            } else {
                total_width
            }
        };

        let flush = |rows: &mut Vec<Row>,
                     glyphs: &mut String,
                     width: &mut usize,
                     start: usize,
                     first: bool| {
            rows.push(Row {
                glyphs: std::mem::take(glyphs),
                width: *width,
                start,
                prompt_cells: if first { prompt_cells } else { 0 },
            });
            *width = 0;
        };

        let atoms = projection.atoms(source);
        for atom in &atoms {
            // A newline's boundary sits at the end of the current row's content;
            // then the row breaks.
            if atom.breaks {
                boundaries[atom.source.start] = Pos {
                    row: row_index,
                    col: prompt_cells_for(first_row, prompt_cells) + row_width,
                };
                flush(
                    &mut rows,
                    &mut row_glyphs,
                    &mut row_width,
                    row_start,
                    first_row,
                );
                row_index += 1;
                first_row = false;
                row_start = atom.source.end;
                last_visible_col = None;
                continue;
            }

            // Soft-wrap a positive-width atom that no longer fits. A fresh row
            // always offers a full width, so an atom wider than the whole row
            // (an oversized paste label) still gets its own row rather than
            // looping forever.
            if atom.width > 0 && row_width + atom.width > avail(first_row) && row_width > 0 {
                flush(
                    &mut rows,
                    &mut row_glyphs,
                    &mut row_width,
                    row_start,
                    first_row,
                );
                row_index += 1;
                first_row = false;
                row_start = atom.source.start;
                last_visible_col = None;
            }

            // Record the start-of-paint cell for every scalar this atom spans.
            // A multi-scalar (paste) atom collapses its interior boundaries onto
            // the atom's start cell — the caret cannot land inside a token. A
            // zero-width mark shares the previous visible cell.
            let content_col = if atom.width == 0 {
                last_visible_col.unwrap_or(row_width)
            } else {
                row_width
            };
            let here = Pos {
                row: row_index,
                col: prompt_cells_for(first_row, prompt_cells) + content_col,
            };
            for b in atom.source.clone() {
                boundaries[b] = here;
            }

            row_glyphs.push_str(&atom.glyphs);
            if atom.width > 0 {
                last_visible_col = Some(row_width);
            }
            row_width += atom.width;
        }

        // The end-of-draft boundary sits just past the last content cell.
        boundaries[scalar_count] = Pos {
            row: row_index,
            col: prompt_cells_for(first_row, prompt_cells) + row_width,
        };
        flush(
            &mut rows,
            &mut row_glyphs,
            &mut row_width,
            row_start,
            first_row,
        );

        Layout { rows, boundaries }
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
        // Candidate boundaries on the target row, in source order.
        let mut best: Option<(usize, usize)> = None; // (source_index, |col delta|)
        for (idx, pos) in self.boundaries.iter().enumerate() {
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
        if let Some((idx, _)) = best {
            return idx;
        }
        // No boundary on that row (target below the draft): clamp to the end.
        self.boundaries.len() - 1
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

    /// Clip the full layout to `height` physical rows around `anchor`'s row,
    /// keeping the anchored row visible. Returns the rows to paint and the
    /// index of the first visible row (so callers can offset boundary rows).
    pub fn viewport(&self, anchor: Pos, height: usize) -> (&[Row], usize) {
        if height == 0 || self.rows.len() <= height {
            return (&self.rows, 0);
        }
        // Keep the anchor row in view, preferring to show rows above it.
        let last = self.rows.len() - 1;
        let anchor_row = anchor.row.min(last);
        let start = anchor_row.saturating_sub(height - 1).min(last + 1 - height);
        (&self.rows[start..start + height], start)
    }
}

fn prompt_cells_for(first_row: bool, prompt_cells: usize) -> usize {
    if first_row {
        prompt_cells
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout(source: &str, prompt: &str, width: usize) -> Layout {
        Layout::compute(source, prompt, width, &ScalarProjection)
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
        let l = layout("aaaabbbbccccdddd", "", 4); // 4 rows of 4
        assert_eq!(l.rows.len(), 4);
        let anchor = l.source_to_cell(15); // last 'd', on row 3
        let (vis, first) = l.viewport(anchor, 2);
        assert_eq!(first, 2, "shows the last two rows");
        assert_eq!(vis.len(), 2);
        assert_eq!(vis[0].glyphs, "cccc");
        assert_eq!(vis[1].glyphs, "dddd");
    }

    /// Fixture 7: a paste label wider than the viewport gets its own row and
    /// its interior boundaries collapse to the atom's start — you cannot land
    /// the caret inside the token.
    #[test]
    fn paste_label_wider_than_viewport() {
        struct PasteProj;
        impl Projection for PasteProj {
            fn atoms(&self, source: &str) -> Vec<Atom> {
                // Whole source is one wide token.
                let len = source.chars().count();
                vec![Atom {
                    source: 0..len,
                    width: 20,
                    glyphs: "[Pasted text #1 +9 lines]".to_string(),
                    breaks: false,
                }]
            }
        }
        let l = Layout::compute("xxxxx", "", 8, &PasteProj);
        assert_eq!(l.rows.len(), 1, "oversized atom still gets one row");
        assert_eq!(l.rows[0].width, 20);
        // Every interior source boundary maps to the token's start cell.
        for i in 0..5 {
            assert_eq!(l.source_to_cell(i), Pos { row: 0, col: 0 });
        }
        // End-of-draft is just past the label.
        assert_eq!(l.source_to_cell(5), Pos { row: 0, col: 20 });
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
