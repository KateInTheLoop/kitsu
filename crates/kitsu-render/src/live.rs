//! The live status row: one cursor-addressed stderr row shared by the
//! animated spinner and the during-turn input draft.
//!
//! While a foreground turn runs, kitsu shows a single rewritable row at the
//! bottom of the transcript: the fox spinner segment and, when the user is
//! typing ahead, a live input draft with a visible cursor. Transcript writers
//! (streamed stdout lines, tool receipts, notices) wrap their writes in
//! [`with_live_row_suspended`] so a full line never splices into the row: the
//! row is cleared, the line prints, and the row is repainted underneath.
//!
//! The row only exists on a styled stderr TTY. Non-TTY, `--plain`, and
//! assistive sessions never enter this module's draw path — every function
//! degrades to pure state bookkeeping with zero bytes written, so scripted
//! output stays byte-identical.

use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

use owo_colors::OwoColorize;

use crate::{display_width, sanitize_terminal_text};

/// The user's in-progress input during a foreground turn. `buffer` is plain
/// sanitized text (the editor never stores control bytes); `cursor` is a char
/// index into `buffer`. `hint` is a display-only inline completion suffix
/// (e.g. the rest of a slash-command name) rendered dimmed after the buffer
/// when the cursor sits at the end; it is never part of the draft itself.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LiveDraft {
    pub prompt: String,
    pub buffer: String,
    pub cursor: usize,
    pub hint: String,
    /// Reserved hints (e.g. the reverse-search status) claim their columns
    /// FIRST and the draft windows into the remainder — they must stay
    /// visible even when the draft is trimmed. Unreserved hints (slash
    /// completion) render only when the draft fully fits.
    pub hint_reserved: bool,
}

#[derive(Default)]
struct LiveRowState {
    spinner: Option<String>,
    draft: Option<LiveDraft>,
    /// Prompts waiting in the queue behind the running turn; rendered as a
    /// dimmed `[N queued]` tag so the pending stack stays visible after its
    /// receipt scrolls away.
    queued: usize,
    /// Detached background agents waiting on the user; rendered as a dimmed
    /// `[N blocked] ` tag leftmost on the status row so an agent that needs
    /// an answer stays visible after its one-off notice scrolls away.
    blocked: usize,
    /// Detached background agents still running; rendered as a dimmed
    /// `[N agent(s)] ` tag only when nothing is blocked — progress matters
    /// less than a question the user must answer.
    running: usize,
    /// Up to the first three queued prompts, rendered as dimmed stack rows
    /// above the status row (the live region's upper rows).
    previews: Vec<String>,
    /// The stack rows changed and the next redraw must repaint the whole
    /// region, not just the bottom status row.
    region_dirty: bool,
    /// How many physical rows the live region currently occupies on the
    /// terminal (stack rows + status row); 0 when nothing is painted. The
    /// cursor always rests on the bottom row while drawn.
    drawn_rows: usize,
    /// Terminal width at the last paint; a change forces a full repaint so
    /// stack rows never linger at a stale width.
    last_cols: usize,
    /// The upper (non-status) rows as last painted; content changes with an
    /// unchanged row count (e.g. the cursor crossing logical lines) must
    /// also force a full repaint.
    last_upper: Vec<String>,
}

fn state() -> &'static Mutex<LiveRowState> {
    static STATE: OnceLock<Mutex<LiveRowState>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(LiveRowState::default()))
}

/// Nested transcript writers must not clear/repaint recursively; only the
/// outermost suspension owns the terminal row.
static SUSPEND_DEPTH: AtomicUsize = AtomicUsize::new(0);

/// Does the terminal cursor stand part way along a row that already holds
/// assistant text?
///
/// The region is painted with no absolute positioning — `render_region`'s fast
/// path writes `\r\x1b[2K{status}` and `clear_region_locked` walks upward
/// clearing rows — so it always acts on whatever row the cursor occupies. Every
/// transcript writer but one leaves the cursor at column 0 of a fresh row,
/// which is what makes that safe. A message or streamed answer whose last line
/// carries no terminating newline does not: the row under the cursor is the
/// answer's last line, and painting the region there erases it. Writers that
/// can leave such a row set this flag (see `set_partial_line`); the next paint
/// steps down onto a fresh row first and clears it.
static PARTIAL_LINE: AtomicBool = AtomicBool::new(false);

/// Record whether the last stdout transcript write left an unterminated line
/// on the live row's terminal. Only writers that share the row's terminal need
/// call this; a stdout that is redirected away cannot move the row's cursor.
pub fn set_partial_line(partial: bool) {
    PARTIAL_LINE.store(partial, Ordering::Release);
}

/// Whether a partial line is still unprotected. Consumes the flag: the caller
/// is about to step the cursor onto a fresh row, after which the text row is
/// out of the region's reach.
fn take_partial_line() -> bool {
    PARTIAL_LINE.swap(false, Ordering::AcqRel)
}

/// Finding F15's off switch: whether a live status row may be painted at all.
///
/// Default `true`, so a session without the inline viewport is byte-identical
/// to the pre-gate renderer. The inline viewport pins its rows outside the
/// DECSTBM scroll region and suspends nothing, so a live row painted at the
/// same time lands *inside* the region and either scrolls away with the
/// transcript or fights the viewport's parked cursor. The viewport wins,
/// because its bottom rows are exactly where the spinner, draft, queue tag and
/// ambient agent tag belong, so running both would be duplication before it is
/// a conflict.
static LIVE_ROWS_ENABLED: AtomicBool = AtomicBool::new(true);

/// Turn the live status row on or off process-wide, returning the **previous**
/// setting so a caller can restore exactly what it displaced rather than a
/// hardcoded default.
pub fn set_live_rows_enabled(enabled: bool) -> bool {
    LIVE_ROWS_ENABLED.swap(enabled, Ordering::AcqRel)
}

fn live_rows_supported() -> bool {
    LIVE_ROWS_ENABLED.load(Ordering::Acquire) && crate::stderr_styled() && !crate::plain_mode()
}

#[cfg(unix)]
fn terminal_cols() -> usize {
    let mut size: libc::winsize = unsafe { std::mem::zeroed() };
    // SAFETY: TIOCGWINSZ writes one winsize struct through the valid pointer;
    // stderr's fd is process-owned and never closed here.
    let result = unsafe { libc::ioctl(libc::STDERR_FILENO, libc::TIOCGWINSZ, &mut size) };
    if result == 0 && size.ws_col > 0 {
        size.ws_col as usize
    } else {
        80
    }
}

#[cfg(not(unix))]
fn terminal_cols() -> usize {
    80
}

pub fn terminal_rows() -> usize {
    #[cfg(unix)]
    {
        let mut size: libc::winsize = unsafe { std::mem::zeroed() };
        // SAFETY: size is a valid output buffer for stderr's terminal ioctl.
        if unsafe { libc::ioctl(libc::STDERR_FILENO, libc::TIOCGWINSZ, &mut size) } == 0
            && size.ws_row > 0
        {
            return usize::from(size.ws_row);
        }
    }
    24
}

/// Control characters a draft may legitimately contain (multi-line pastes,
/// Alt+Enter newlines, pasted tabs) render as visible placeholders so the
/// row never breaks its single-physical-line invariant.
fn display_char(ch: char) -> char {
    match ch {
        '\n' => '⏎',
        '\t' => '⇥',
        _ => ch,
    }
}

/// Clamp `buffer` to `avail` display columns, keeping the cursor visible.
/// Trimmed edges are marked with `…`. Returns the visible slice, the display
/// width of the visible text left of the cursor, and whether the buffer was
/// windowed (authoritative — a literal `…` in the buffer must not be
/// mistaken for a trim marker).
fn fit_draft_window(buffer: &str, cursor: usize, avail: usize) -> (String, usize, bool) {
    let chars: Vec<char> = buffer.chars().map(display_char).collect();
    let cursor = cursor.min(chars.len());
    let widths: Vec<usize> = chars
        .iter()
        .map(|&c| crate::char_display_width(c))
        .collect();
    let total: usize = widths.iter().sum();
    if total <= avail {
        return (chars.iter().collect(), widths[..cursor].iter().sum(), false);
    }
    if avail < 4 {
        // Too narrow for a meaningful window; show nothing but keep the
        // cursor parked at the start so the row still fits.
        return (String::new(), 0, true);
    }
    // Expand a window around the cursor greedily; reserve one column per
    // trimmed edge for its `…` marker.
    let mut lo = cursor;
    let mut hi = cursor;
    let mut used = 0usize;
    loop {
        let left_trim = lo > 0;
        let right_trim = hi < chars.len();
        let reserve = usize::from(left_trim) + usize::from(right_trim);
        let room = avail.saturating_sub(used + reserve);
        if left_trim && used + reserve < avail && widths[lo - 1] <= room {
            lo -= 1;
            used += widths[lo];
            continue;
        }
        if right_trim && used + reserve < avail && widths[hi] <= room {
            used += widths[hi];
            hi += 1;
            continue;
        }
        break;
    }
    let mut visible = String::new();
    let mut cursor_offset = 0usize;
    if lo > 0 {
        visible.push('…');
        cursor_offset += 1;
    }
    cursor_offset += widths[lo..cursor].iter().sum::<usize>();
    visible.extend(&chars[lo..hi]);
    if hi < chars.len() {
        visible.push('…');
    }
    (visible, cursor_offset, true)
}

/// Compose the styled row and the 1-based cursor column (when a draft is
/// present). Pure so the layout is unit-testable without a terminal.
/// The ambient background-agent tag (trailing space included), or `None`
/// when no detached agent is blocked or running. Blocked outranks running:
/// an agent waiting on the user is the thing the user must act on. Pure.
pub fn agents_tag(blocked: usize, running: usize) -> Option<String> {
    if blocked > 0 {
        Some(format!("[{blocked} blocked] "))
    } else if running == 1 {
        Some("[1 agent] ".to_string())
    } else if running > 0 {
        Some(format!("[{running} agents] "))
    } else {
        None
    }
}

/// The pending-stack tag (trailing space included), or `None` when nothing is
/// queued. Named rather than inlined so the classic live row and R5's status
/// row cannot drift apart on the literal (finding F38). Pure.
pub fn queued_tag(queued: usize) -> Option<String> {
    (queued > 0).then(|| format!("[{queued} queued] "))
}

/// Everything requirement **R5**'s persistent status row shows, as data:
/// pure input to [`compose_status_row`].
///
/// It lives here, beside [`LiveDraft`], because the row's *content* is shared
/// by both renderers — the inline viewport paints it as a pinned bottom row,
/// the classic renderer can reach the same composer later — and `kitsu-tui`
/// depends on `kitsu-render`, never the reverse.
#[derive(Clone, Debug, Default)]
pub struct StatusFields {
    /// The live model id.
    pub model: String,
    /// Context tokens used and the effective window, for the meter.
    pub used: u64,
    pub effective: u64,
    /// Elapsed time in the current turn, or `None` between turns.
    pub elapsed: Option<std::time::Duration>,
    /// Depth of the prompt queue, behind `[N queued]`.
    pub queued: usize,
    /// Detached agents needing input, and running, behind the ambient tag.
    pub blocked: usize,
    pub running: usize,
    /// The background agent this composer is attached to, behind
    /// `attached \u{b7} agent N`, or `None` when the composer is the
    /// session's own (card **T4-S6-status**, design \u{a7}4.5).
    ///
    /// Derived from the composer loop's one session binding on every read --
    /// the same binding `composer_prompt` takes its label from (**F75**) --
    /// so the row and the prompt label cannot disagree about what the user is
    /// attached to.
    pub attached: Option<u64>,
}

/// Cells in the status row's meter. `/context` renders sixteen in a block it
/// owns three lines of; a one-line row shared with four other segments gets
/// half that, so the meter costs the row twelve columns rather than twenty.
const STATUS_METER_CELLS: u64 = 8;

/// How many lines of a multi-line draft to show ABOVE the cursor's line in the
/// composer before collapsing the rest into a `⋮ +N earlier` marker. The pinned
/// region grows to fit its content (clamped to the terminal height by the
/// viewport), so this is what makes the input visibly expand into a box for a
/// large paste or hand-typed block, instead of the old 2-line peephole. Older
/// lines stay reachable by scrolling the cursor up; the marker shows how many
/// are hidden.
const DRAFT_CONTEXT_ROWS: usize = 12;

/// A full-width framed input box (Phase 1 of the Claude-CLI-style composer,
/// see docs/tui-input-box-requirements.md). Renders the draft inside a
/// kitsu-banner frame that is ALWAYS present (even empty), so the input region
/// and its caret exist from launch rather than appearing only once typing
/// starts. Pure: returns the frame rows plus the caret's (row, col) *within the
/// returned rows*, both 1-based, for the live loop to position the hardware
/// cursor. The live wiring (scroll region, resize, overlay interplay) consumes
/// this; it is kept separate so the geometry is unit-testable without a
/// terminal — the one part of the composer that can be.
///
/// `cols` is the terminal width. The box claims the full width. `inner` is the
/// usable text width (`cols - 4`: two border cells + one pad each side). The
/// draft's cursor line is shown on the caret row; extra lines render above up
/// to [`DRAFT_CONTEXT_ROWS`], older ones collapsing into a `⋮` marker inside the
/// frame. Returns at least 3 rows (top border, one content row, bottom border).
///
/// NOTE: unused until Phase 2 wires it into the composer painter. The correct
/// wiring site is `tui_bridge::paint_composer` → `compose_input_row` (the
/// pinned-viewport bottom row that actually draws the idle `kitsu>` prompt),
/// NOT the typeahead `render_region`. Wiring a multi-row box there means
/// growing the pinned DECSTBM viewport region to the box height — a viewport
/// change that must be verified on a real terminal. Built and unit-tested first
/// so the geometry is correct before the un-testable wiring.
/// See docs/tui-input-box-requirements.md.
pub struct InputBox {
    pub rows: Vec<String>,
    /// 1-based (row, col) of the caret within `rows`.
    pub caret: (usize, usize),
}

pub fn compose_input_box(draft: &LiveDraft, cols: usize) -> InputBox {
    // A sane floor so a pathologically narrow terminal still yields a box
    // rather than panicking on the width arithmetic.
    let cols = cols.max(8);
    let (r, g, b) = crate::theme().accent;
    let inner = cols - 4; // "│ " + text + " │"
    let bar = |s: &str| format!("│ {s} │"); // caller pads `s` to `inner`

    // Content lines (plain, display-mapped, clamped to `inner`), with the
    // caret's line index and column within it.
    let (content, caret_line, caret_col_in_line) = input_box_content(draft, inner);

    let horizontal: String = "─".repeat(cols.saturating_sub(2));
    let top = format!("{}", format!("╭{horizontal}╮").truecolor(r, g, b));
    let bottom = format!("{}", format!("╰{horizontal}╯").truecolor(r, g, b));

    let mut rows = Vec::with_capacity(content.len() + 2);
    rows.push(top);
    for line in &content {
        // Pad the (already clamped) plain text to the inner width.
        let pad = inner.saturating_sub(display_width(line));
        let padded = format!("{line}{}", " ".repeat(pad));
        rows.push(bar(&padded));
    }
    rows.push(bottom);

    // Caret: content starts at row index 1 (after the top border); columns are
    // "│ " = 2 border/pad cells + the column within the line, all 1-based.
    let caret_row = 1 + caret_line + 1; // +1 for top border, +1 for 1-based
    let caret_col = 2 + caret_col_in_line + 1;
    InputBox {
        rows,
        caret: (caret_row, caret_col),
    }
}

/// Fit a composed box into `max_rows` (the height the viewport will actually
/// grant). The viewport keeps the LAST rows of over-tall content, which would
/// cut off the top border (review F8), so trim CONTENT rows here instead:
/// first the rows above the caret (oldest context), then rows below it. Both
/// borders and the caret row always survive; the caret row index is kept in
/// step. Never returns fewer than 3 rows.
pub fn fit_input_box(mut b: InputBox, max_rows: usize) -> InputBox {
    let max_rows = max_rows.max(3);
    if b.rows.len() <= max_rows {
        return b;
    }
    let overflow = b.rows.len() - max_rows;
    let caret_idx = b.caret.0 - 1; // 0-based; index 0 is the top border
    let drop_above = overflow.min(caret_idx.saturating_sub(1));
    if drop_above > 0 {
        b.rows.drain(1..1 + drop_above);
        b.caret.0 -= drop_above;
    }
    let remaining = overflow - drop_above;
    if remaining > 0 {
        let start = b.caret.0; // first row below the caret row (0-based)
        let end = (start + remaining).min(b.rows.len() - 1); // keep bottom border
        if start < end {
            b.rows.drain(start..end);
        }
    }
    b
}

/// The plain content lines for the input box, the caret's line index among
/// them, and the caret's column within that line. Mirrors
/// `compose_draft_context_rows`' budget but returns plain (unframed) text so
/// the box renderer owns the framing. The prompt is the first thing on the
/// caret line (e.g. "kitsu> "), matching the single-row composer.
fn input_box_content(draft: &LiveDraft, inner: usize) -> (Vec<String>, usize, usize) {
    let prompt = sanitize_terminal_text(&draft.prompt);
    let prompt_w = display_width(&prompt);
    let buffer = &draft.buffer;
    let chars: Vec<char> = buffer.chars().collect();
    let cursor = draft.cursor.min(chars.len());
    let logical: Vec<&str> = buffer.split('\n').collect();
    let cursor_line = chars[..cursor].iter().filter(|&&c| c == '\n').count();
    let (line_start, _, _) = current_line_bounds(buffer, cursor);
    let cursor_in_line = cursor - line_start; // char offset within the cursor's logical line

    // Wrap each logical line into `inner`-wide visual rows. The prompt prefixes
    // the FIRST logical line only. Track where the caret lands (visual row +
    // display column) as rows accumulate.
    let mut rows: Vec<String> = Vec::new();
    let mut caret_row = 0usize;
    let mut caret_col = prompt_w; // fallback: just past the prompt
    for (li, line) in logical.iter().enumerate() {
        let full = if li == 0 {
            format!("{prompt}{}", sanitize_terminal_text(line))
        } else {
            sanitize_terminal_text(line)
        };
        let segs = wrap_segments(&full, inner); // display_char-mapped, 1 char→1 char
        if li == cursor_line {
            // Caret's char column within `full` (prompt counts on line 0).
            let caret_c = if li == 0 { prompt.chars().count() + cursor_in_line } else { cursor_in_line };
            let (seg_idx, col) = locate_caret_in_segments(&segs, caret_c, inner);
            caret_row = rows.len() + seg_idx;
            caret_col = col;
        }
        rows.extend(segs);
    }

    // Bound the box height: keep a window of at most DRAFT_CONTEXT_ROWS visual
    // rows ending at the caret row (caret stays visible at/near the bottom),
    // collapsing anything above into a `⋮` marker.
    if rows.len() > DRAFT_CONTEXT_ROWS {
        let end = (caret_row + 1).max(DRAFT_CONTEXT_ROWS).min(rows.len());
        let start = end.saturating_sub(DRAFT_CONTEXT_ROWS);
        let hidden = start;
        rows = rows[start..end].to_vec();
        caret_row -= start;
        if hidden > 0 {
            rows.insert(0, format!("⋮ +{hidden} row(s) above"));
            caret_row += 1;
        }
    }
    (rows, caret_row, caret_col.min(inner))
}

/// Which wrap segment holds char index `caret_c`, and the caret's display
/// column within it. A caret exactly at a full segment's end moves to the start
/// of the next segment (so typing continues on the next visual row), except on
/// the last segment where it rests at the end.
fn locate_caret_in_segments(segs: &[String], caret_c: usize, inner: usize) -> (usize, usize) {
    let mut acc = 0usize;
    for (i, seg) in segs.iter().enumerate() {
        let seg_chars = seg.chars().count();
        if caret_c < acc + seg_chars || i + 1 == segs.len() {
            let offset = caret_c.saturating_sub(acc).min(seg_chars);
            let col = display_width(&seg.chars().take(offset).collect::<String>());
            return (i, col.min(inner));
        }
        if caret_c == acc + seg_chars {
            // At the boundary of a full, non-last segment → next row, col 0.
            return (i + 1, 0);
        }
        acc += seg_chars;
    }
    (0, 0)
}

/// The shortest model segment worth keeping. Below this the model is dropped
/// outright instead of truncated to an unreadable stub.
const STATUS_MODEL_MIN_COLS: usize = 7;

/// The separator between the row's content segments, and the width charged
/// for joining any two pieces (the tags join with one space, so charging the
/// widest separator can only leave the row narrower than its budget).
const STATUS_SEP: &str = " · ";

/// The compact context meter: filled/empty cells and the utilisation
/// percentage. `None` when no window is known, since a meter with no
/// denominator would read as an empty context rather than an unknown one.
fn status_meter(used: u64, effective: u64) -> Option<String> {
    if effective == 0 {
        return None;
    }
    let pct = (used.saturating_mul(100) / effective).min(999);
    let filled = {
        let cells = (used.saturating_mul(STATUS_METER_CELLS) / effective).min(STATUS_METER_CELLS);
        // A non-empty context never reads as wholly empty, as in `/context`.
        if used > 0 && cells == 0 {
            1
        } else {
            cells
        }
    };
    let cells: String = (0..STATUS_METER_CELLS)
        .map(|index| if index < filled { '■' } else { '□' })
        .collect();
    Some(format!("{cells} {pct}%"))
}

/// Keep a `budget`-wide prefix of `text`, marking the cut with `…`.
fn clip_to(text: &str, budget: usize) -> String {
    if display_width(text) <= budget {
        return text.to_string();
    }
    let room = budget.saturating_sub(1);
    let mut used = 0usize;
    let mut clipped: String = text
        .chars()
        .take_while(|&ch| {
            used += crate::char_display_width(ch);
            used <= room
        })
        .collect();
    clipped.push('…');
    clipped
}

/// Compose requirement **R5**'s one-line status row: model, context meter,
/// elapsed turn time, `[N queued]` and the ambient background-agent tag,
/// clamped to `cols` so it can never wrap. Pure.
///
/// # Drop order
///
/// First dropped to last kept: **meter, elapsed, model, `[N queued]`,
/// `attached · agent N`, ambient tag**. Decoration yields to content,
/// exactly as the classic row's spinner yields to the draft: the meter goes first because `/context` renders it on
/// demand, elapsed next because the spinner already carries elapsed past five
/// seconds, the model is truncated before it is dropped (long and static),
/// and the ambient tag is last standing because a blocked agent is the thing
/// the user must act on.
///
/// Segments are considered in reverse drop order, each against what the
/// higher-priority ones left, so a narrower terminal can only ever lose
/// segments — a dropped segment never reappears as the row shrinks.
///
/// The ambient tag is [`agents_tag`]'s own text and the queue tag is
/// [`queued_tag`]'s: R5's `← N agents` form is forbidden until A1's
/// left-arrow affordance exists, so there is exactly one source for each.
pub fn compose_status_row(fields: &StatusFields, cols: usize) -> String {
    let sep_width = display_width(STATUS_SEP);
    let mut used = 0usize;
    // Charge the join for every piece after the first.
    let fits = |width: usize, used: &mut usize| -> bool {
        let cost = width + if *used == 0 { 0 } else { sep_width };
        if *used + cost <= cols {
            *used += cost;
            true
        } else {
            false
        }
    };

    // 1. The ambient tag, last in the drop order and so first to claim room.
    let agents = agents_tag(fields.blocked, fields.running)
        .map(|tag| tag.trim_end().to_string())
        .filter(|tag| fits(display_width(tag), &mut used));
    // 2. The attachment label (card **T4-S6-status**, design §4.5). It claims
    //    room right after the ambient tag: §4.5's amendment rests the
    //    `←`-detaches-on-the-first-press rule on the attachment being legible
    //    at the moment of the press, so it outranks `[N queued]`, the model,
    //    elapsed and the meter; it still yields to the ambient tag, because a
    //    blocked agent is the thing the user must act on while losing this
    //    label costs one keystroke.
    //
    //    Derived from `fields.attached` on every call and never remembered,
    //    so a repaint composes it again from the one session binding the
    //    composer prompt label reads (**F75**).
    let attached = fields
        .attached
        .map(|id| format!("attached · agent {id}"))
        .filter(|label| fits(display_width(label), &mut used));
    // 3. The queue tag.
    let queue = queued_tag(fields.queued)
        .map(|tag| tag.trim_end().to_string())
        .filter(|tag| fits(display_width(tag), &mut used));
    // 4. The model: truncated before it is dropped.
    let model_width = display_width(&fields.model);
    let (model, model_whole) = if fields.model.is_empty() {
        (None, true)
    } else if fits(model_width, &mut used) {
        (Some(fields.model.clone()), true)
    } else {
        let room = cols
            .saturating_sub(used + if used == 0 { 0 } else { sep_width })
            .min(model_width);
        if room >= STATUS_MODEL_MIN_COLS {
            let clipped = clip_to(&fields.model, room);
            used += display_width(&clipped) + if used == 0 { 0 } else { sep_width };
            (Some(clipped), false)
        } else {
            (None, false)
        }
    };
    // 5. Elapsed, only once the model is whole: the model outranks it, so a
    //    truncated model means elapsed is already gone.
    let elapsed = fields
        .elapsed
        .filter(|_| model_whole)
        .map(crate::format_wait_elapsed)
        .filter(|text| fits(display_width(text), &mut used));
    // 6. The meter, first to go, so last to be offered room. It needs the
    //    model whole and elapsed either absent from the data or still kept.
    let elapsed_intact = fields.elapsed.is_none() || elapsed.is_some();
    let meter = status_meter(fields.used, fields.effective)
        .filter(|_| model_whole && elapsed_intact)
        .filter(|text| fits(display_width(text), &mut used));

    let mut content: Vec<String> = Vec::new();
    if let Some(attached) = attached {
        content.push(attached);
    }
    if let Some(model) = model {
        content.push(model);
    }
    if let Some(meter) = meter {
        content.push(format!("{}", meter.dimmed()));
    }
    if let Some(elapsed) = elapsed {
        content.push(format!("{}", elapsed.dimmed()));
    }
    let mut row = content.join(STATUS_SEP);
    for tag in [queue, agents].into_iter().flatten() {
        if !row.is_empty() {
            row.push(' ');
        }
        row.push_str(&format!("{}", tag.dimmed()));
    }
    row
}

fn compose_row(
    spinner: Option<&str>,
    draft: Option<&LiveDraft>,
    blocked: usize,
    running: usize,
    queued: usize,
    cols: usize,
) -> (String, Option<usize>) {
    let spinner_width = spinner.map(|segment| display_width(&sanitize_terminal_text(segment)));
    let Some(draft) = draft else {
        return (spinner.unwrap_or_default().to_string(), None);
    };
    // The pending-stack tag sits between the spinner and the prompt; its
    // trailing space is part of its width.
    let queue_tag = queued_tag(queued);
    let queue_tag_width = queue_tag.as_deref().map(display_width).unwrap_or(0);
    // The background-agent tag sits leftmost of the tags, before the queue
    // tag; its trailing space is part of its width too.
    let agents_tag = agents_tag(blocked, running);
    let agents_tag_width = agents_tag.as_deref().map(display_width).unwrap_or(0);
    let prompt_width =
        display_width(&sanitize_terminal_text(&draft.prompt)) + agents_tag_width + queue_tag_width;
    // The spinner yields the row to the draft when both cannot fit.
    let spinner = match (spinner, spinner_width) {
        (Some(segment), Some(width)) if width + 2 + prompt_width + 8 <= cols => {
            Some((segment, width))
        }
        _ => None,
    };
    let lead = spinner.map(|(_, width)| width + 2).unwrap_or(0);
    // One column is reserved so an end-of-buffer cursor never wraps the row.
    let avail = cols.saturating_sub(lead + prompt_width + 1);
    // A reserved hint claims its columns before the draft is windowed.
    let hint_width = display_width(&draft.hint);
    let buffer_avail = if draft.hint_reserved && !draft.hint.is_empty() {
        avail.saturating_sub(hint_width)
    } else {
        avail
    };
    // Multi-line drafts show only the cursor's logical line here; earlier
    // lines render as dim context rows above (compose_draft_context_rows).
    let (line_start, line_end, cursor_in_line) = current_line_bounds(&draft.buffer, draft.cursor);
    let line: String = draft
        .buffer
        .chars()
        .skip(line_start)
        .take(line_end - line_start)
        .collect();
    let line_mapped: String = line.chars().map(display_char).collect();
    let (visible, cursor_offset, windowed) = if !draft.hint_reserved
        && soft_wrap_active(
            line.chars().count(),
            cursor_in_line,
            display_width(&line_mapped),
            buffer_avail,
        ) {
        // Soft wrap: the bottom row carries only the final width segment
        // (earlier segments are context rows); it fits by construction.
        let segments = wrap_segments(&line, buffer_avail);
        let last = segments.last().cloned().unwrap_or_default();
        let offset = display_width(&last);
        (last, offset, false)
    } else {
        fit_draft_window(&line, cursor_in_line, buffer_avail)
    };
    let mut row = String::new();
    if let Some((segment, _)) = spinner {
        row.push_str(segment);
        row.push_str("  ");
    }
    if let Some(tag) = agents_tag {
        row.push_str(&format!("{}", tag.dimmed()));
    }
    if let Some(tag) = queue_tag {
        row.push_str(&format!("{}", tag.dimmed()));
    }
    row.push_str(&draft.prompt);
    row.push_str(&visible);
    // The completion hint renders dimmed after the draft, but only when the
    // draft fully fit unwindowed, the cursor sits at its end, and the hint
    // has room — a hint must never displace the text it completes. The
    // cursor column stays before the hint so typing continues over it.
    let buffer_chars = draft.buffer.chars().count();
    let buffer_width = display_width(&visible);
    if draft.hint_reserved && !draft.hint.is_empty() {
        // Reserved hints always render (their space was carved out above),
        // truncated only if the terminal cannot even hold them alone.
        let space = avail.saturating_sub(buffer_width).max(1);
        let hint: String = {
            let mut used = 0usize;
            draft
                .hint
                .chars()
                .take_while(|&ch| {
                    used += crate::char_display_width(ch);
                    used <= space
                })
                .collect()
        };
        if !hint.is_empty() {
            row.push_str(&format!("{}", hint.dimmed()));
        }
        return (row, Some(lead + prompt_width + cursor_offset + 1));
    }
    if !draft.hint.is_empty()
        && draft.cursor == buffer_chars
        && !windowed
        && cursor_offset == buffer_width
    {
        let space = avail.saturating_sub(buffer_width);
        let hint: String = {
            let mut used = 0usize;
            draft
                .hint
                .chars()
                .take_while(|&ch| {
                    used += crate::char_display_width(ch);
                    used <= space
                })
                .collect()
        };
        if !hint.is_empty() {
            row.push_str(&format!("{}", hint.dimmed()));
        }
    }
    (row, Some(lead + prompt_width + cursor_offset + 1))
}

/// The columns available to the draft text on the status row, given the
/// spinner segment, background-agent tag, queue tag, and prompt — the SAME
/// math compose_row uses, shared so soft-wrap segmentation agrees between the
/// bottom row and the wrap context rows. Pure.
fn draft_segment_width(
    spinner: Option<&str>,
    draft: &LiveDraft,
    blocked: usize,
    running: usize,
    queued: usize,
    cols: usize,
) -> usize {
    let spinner_width = spinner.map(|segment| display_width(&sanitize_terminal_text(segment)));
    let queue_tag_width = if queued > 0 {
        display_width(&format!("[{queued} queued] "))
    } else {
        0
    };
    let agents_tag_width = agents_tag(blocked, running)
        .as_deref()
        .map(display_width)
        .unwrap_or(0);
    let prompt_width =
        display_width(&sanitize_terminal_text(&draft.prompt)) + agents_tag_width + queue_tag_width;
    let lead = match spinner_width {
        Some(width) if width + 2 + prompt_width + 8 <= cols => width + 2,
        _ => 0,
    };
    cols.saturating_sub(lead + prompt_width + 1)
}

/// Greedy display-width segmentation of one (display-mapped) logical line —
/// the soft-wrap unit. Pure.
fn wrap_segments(line: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut segments = Vec::new();
    let mut current = String::new();
    let mut used = 0usize;
    for ch in line.chars().map(display_char) {
        let w = crate::char_display_width(ch);
        if used + w > width && !current.is_empty() {
            segments.push(std::mem::take(&mut current));
            used = 0;
        }
        current.push(ch);
        used += w;
    }
    if !current.is_empty() || segments.is_empty() {
        segments.push(current);
    }
    segments
}

/// Soft-wrap applies when the cursor rests at the END of a logical line too
/// wide for the status row (the dominant typing case): earlier segments
/// become context rows and the bottom row shows only the final segment.
/// Mid-line cursors keep the horizontal window. Pure.
fn soft_wrap_active(
    line_chars: usize,
    cursor_in_line: usize,
    line_width: usize,
    avail: usize,
) -> bool {
    avail >= 8 && cursor_in_line == line_chars && line_width > avail
}

/// Char-index bounds of the logical line containing `cursor` (newline-split)
/// plus the cursor's offset within it. Pure.
fn current_line_bounds(buffer: &str, cursor: usize) -> (usize, usize, usize) {
    let chars: Vec<char> = buffer.chars().collect();
    let cursor = cursor.min(chars.len());
    let start = chars[..cursor]
        .iter()
        .rposition(|&ch| ch == '\n')
        .map(|i| i + 1)
        .unwrap_or(0);
    let end = chars[cursor..]
        .iter()
        .position(|&ch| ch == '\n')
        .map(|i| cursor + i)
        .unwrap_or(chars.len());
    (start, end, cursor - start)
}

/// Wrap context rows for one logical line (soft-wrap): when the cursor sits
/// at the line's end and it overflows the status row, all but the final
/// width segment render as dim '│' rows above (older segments collapse into
/// a '⋮ +N earlier' marker). Pure.
fn compose_wrap_rows_for_line(
    line: &str,
    cursor_in_line: usize,
    cols: usize,
    segment_width: usize,
) -> Vec<String> {
    let line_mapped: String = line.chars().map(display_char).collect();
    if !soft_wrap_active(
        line.chars().count(),
        cursor_in_line,
        display_width(&line_mapped),
        segment_width,
    ) {
        return Vec::new();
    }
    let clamp = |plain: String| -> String {
        if display_width(&plain) <= cols {
            return plain;
        }
        let budget = cols.saturating_sub(1);
        let mut used = 0usize;
        let mut clipped: String = plain
            .chars()
            .take_while(|&ch| {
                used += crate::char_display_width(ch);
                used <= budget
            })
            .collect();
        clipped.push('…');
        clipped
    };
    let segments = wrap_segments(line, segment_width);
    let earlier = segments.len().saturating_sub(1);
    let mut rows = Vec::new();
    if earlier > DRAFT_CONTEXT_ROWS {
        rows.push(format!(
            "{}",
            clamp(format!("  ⋮ +{} earlier segment(s)", earlier - DRAFT_CONTEXT_ROWS)).dimmed()
        ));
    }
    for segment in segments
        .iter()
        .take(earlier)
        .skip(earlier.saturating_sub(DRAFT_CONTEXT_ROWS))
    {
        rows.push(format!("{}", clamp(format!("  │ {segment}")).dimmed()));
    }
    rows
}

/// Single-line wrapper used by compose_draft_context_rows' early return.
fn compose_wrap_rows(
    buffer: &str,
    cursor: usize,
    cols: usize,
    segment_width: usize,
) -> Vec<String> {
    compose_wrap_rows_for_line(
        buffer,
        cursor.min(buffer.chars().count()),
        cols,
        segment_width,
    )
}

/// Dim context rows for a multi-line draft: up to two logical lines above
/// the cursor's line (older lines collapse into a `⋮ +N earlier` marker)
/// and a `⋮ +N below` marker when lines exist after the cursor's line.
/// The cursor's own line renders on the status row itself. Pure.
fn compose_draft_context_rows(
    buffer: &str,
    cursor: usize,
    cols: usize,
    // 0 disables soft-wrap rows (reserved-hint mode shrinks the bottom row,
    // which would desync wrap boundaries between rows).
    segment_width: usize,
) -> Vec<String> {
    if !buffer.contains('\n') {
        // Single-line drafts can still soft-wrap.
        return compose_wrap_rows(buffer, cursor, cols, segment_width);
    }
    let chars: Vec<char> = buffer.chars().collect();
    let cursor = cursor.min(chars.len());
    let lines: Vec<String> = buffer.split('\n').map(str::to_string).collect();
    let cursor_line = chars[..cursor].iter().filter(|&&ch| ch == '\n').count();
    let clamp = |plain: String| -> String {
        if display_width(&plain) <= cols {
            return plain;
        }
        let budget = cols.saturating_sub(1);
        let mut used = 0usize;
        let mut clipped: String = plain
            .chars()
            .take_while(|&ch| {
                used += crate::char_display_width(ch);
                used <= budget
            })
            .collect();
        clipped.push('…');
        clipped
    };
    let mut rows = Vec::new();
    if cursor_line > DRAFT_CONTEXT_ROWS {
        rows.push(format!(
            "{}",
            clamp(format!("  ⋮ +{} earlier line(s)", cursor_line - DRAFT_CONTEXT_ROWS)).dimmed()
        ));
    }
    for line in lines
        .iter()
        .take(cursor_line)
        .skip(cursor_line.saturating_sub(DRAFT_CONTEXT_ROWS))
    {
        let text: String = crate::sanitize_terminal_text(line)
            .chars()
            .map(display_char)
            .collect();
        rows.push(format!("{}", clamp(format!("  │ {text}")).dimmed()));
    }
    let (line_start, line_end, _) = current_line_bounds(buffer, cursor);
    let cursor_line_text: String = buffer
        .chars()
        .skip(line_start)
        .take(line_end - line_start)
        .collect();
    rows.extend(compose_wrap_rows_for_line(
        &cursor_line_text,
        cursor.min(buffer.chars().count()) - line_start,
        cols,
        segment_width,
    ));
    let after = lines.len().saturating_sub(cursor_line + 1);
    if after > 0 {
        // Name the submit semantics: only the cursor line is visible on the
        // status row, but Enter sends the whole draft (review, cycle 10).
        rows.push(format!(
            "{}",
            clamp(format!("  ⋮ +{after} line(s) below — Enter submits all")).dimmed()
        ));
    }
    rows
}

/// Compose the dimmed queued-prompt stack rows shown above the status row:
/// up to three upcoming prompts in execution order, then a `+N more` row.
/// Pure for unit testing. Rows are pre-truncated to `cols`.
pub fn compose_stack_rows(previews: &[String], queued: usize, cols: usize) -> Vec<String> {
    if queued == 0 || previews.is_empty() {
        return Vec::new();
    }
    // Every emitted row is clamped to `cols` AFTER composition, prefix
    // included: a stack row that wraps desyncs the region's row-count
    // arithmetic from the screen (sonnet review, cycle 9). Clamping happens
    // on the plain text, before styling, so the SGR wrapper stays intact.
    let clamp = |plain: String| -> String {
        if display_width(&plain) <= cols {
            return plain;
        }
        let budget = cols.saturating_sub(1);
        let mut used = 0usize;
        let mut clipped: String = plain
            .chars()
            .take_while(|&ch| {
                used += crate::char_display_width(ch);
                used <= budget
            })
            .collect();
        clipped.push('…');
        clipped
    };
    let mut rows = Vec::new();
    for (index, preview) in previews.iter().take(3).enumerate() {
        let label = if index == 0 { "next" } else { "then" };
        let text = crate::sanitize_terminal_text(preview)
            .chars()
            .map(display_char)
            .collect::<String>();
        let prefix = format!("  ↳ {label}: ");
        let budget = cols.saturating_sub(display_width(&prefix) + 1);
        let mut used = 0usize;
        let mut clipped: String = text
            .chars()
            .take_while(|&ch| {
                used += crate::char_display_width(ch);
                used <= budget
            })
            .collect();
        if clipped.chars().count() < text.chars().count() {
            clipped.push('…');
        }
        rows.push(format!("{}", clamp(format!("{prefix}{clipped}")).dimmed()));
    }
    if queued > 3 {
        rows.push(format!(
            "{}",
            clamp(format!("  ↳ +{} more queued", queued - 3)).dimmed()
        ));
    }
    rows
}

/// Erase the painted region: the cursor rests on the bottom row, so clear it
/// and walk upward clearing each row, ending at the region's top, column 0.
fn clear_region_locked(err: &mut impl Write, drawn_rows: usize) {
    if drawn_rows == 0 {
        return;
    }
    let _ = write!(err, "\r\x1b[2K");
    for _ in 1..drawn_rows {
        let _ = write!(err, "\x1b[1A\x1b[2K");
    }
}

fn redraw_locked(row_state: &mut LiveRowState) {
    if !live_rows_supported() {
        return;
    }
    let mut err = io::stderr().lock();
    let cols = terminal_cols();
    render_region(&mut err, row_state, cols, terminal_rows());
}

/// The full region-painting state machine, with the writer and terminal
/// width injected so branch selection (fast path vs full repaint, shrink,
/// clear) and `drawn_rows` transitions are unit-testable byte-for-byte —
/// the exact territory where the cycle-9 review found bugs. The public
/// wrappers supply `io::stderr()` and the real ioctl width; behavior is
/// identical.
fn render_region(err: &mut impl Write, row_state: &mut LiveRowState, cols: usize, rows: usize) {
    row_state.drawn_rows = row_state.drawn_rows.min(rows.max(1));
    if row_state.spinner.is_none() && row_state.draft.is_none() {
        if row_state.drawn_rows > 0 {
            clear_region_locked(err, row_state.drawn_rows);
            let _ = err.flush();
            row_state.drawn_rows = 0;
        }
        return;
    }
    // The region is about to be painted on the row the cursor occupies. When
    // that row holds the unterminated last line of an answer, step down onto a
    // fresh row first: otherwise the paint (or the clear that precedes the
    // next one) erases text the user is meant to keep. Only reachable with no
    // region drawn — a drawn region already owns the cursor's row.
    if row_state.drawn_rows == 0 && take_partial_line() {
        let _ = write!(err, "\r\n");
    }
    // A resize invalidates the painted stack rows (they hold the old
    // width); force the full-repaint path (sonnet review, cycle 9).
    if cols != row_state.last_cols {
        row_state.last_cols = cols;
        row_state.region_dirty = true;
    }
    // Upper rows: queue stack + multi-line draft context, both only
    // alongside a draft.
    let mut upper = if row_state.draft.is_some() {
        compose_stack_rows(&row_state.previews, row_state.queued, cols)
    } else {
        Vec::new()
    };
    if let Some(draft) = &row_state.draft {
        let segment_width = if draft.hint_reserved {
            0
        } else {
            draft_segment_width(
                row_state.spinner.as_deref(),
                draft,
                row_state.blocked,
                row_state.running,
                row_state.queued,
                cols,
            )
        };
        upper.extend(compose_draft_context_rows(
            &draft.buffer,
            draft.cursor,
            cols,
            segment_width,
        ));
    }
    let (mut status, mut cursor_col) = compose_row(
        row_state.spinner.as_deref(),
        row_state.draft.as_ref(),
        row_state.blocked,
        row_state.running,
        row_state.queued,
        cols,
    );
    // Leave a transcript row visible even when queued prompts and a multi-line
    // draft would otherwise occupy the whole terminal. Keep the context nearest
    // the cursor, dropping older preview rows first.
    let max_upper = rows.saturating_sub(2);
    if upper.len() > max_upper {
        upper.drain(..upper.len() - max_upper);
    }
    // A spinner without a draft, or tags/prompt wider than a tiny terminal,
    // must not wrap into physical rows that drawn_rows does not account for.
    let available = cols.saturating_sub(1);
    if display_width(&sanitize_terminal_text(&status)) > available {
        let mut used = 0;
        status = sanitize_terminal_text(&status)
            .chars()
            .take_while(|&ch| {
                used += crate::char_display_width(ch);
                used <= available
            })
            .collect();
        cursor_col = cursor_col.map(|col| col.min(available + 1));
    }
    let wanted_rows = upper.len() + 1;
    if row_state.drawn_rows == wanted_rows
        && !row_state.region_dirty
        && upper == row_state.last_upper
    {
        // Fast path (spinner ticks, keystrokes): only the bottom status row
        // changed; upper stack rows stay put — no flicker.
        let _ = write!(err, "\r\x1b[2K{status}");
    } else {
        clear_region_locked(err, row_state.drawn_rows);
        // When the region SHRINKS, the cleared excess rows sit below the
        // new region; step down through them first so the shorter region
        // still hugs the bottom of the screen instead of floating above
        // blank lines (sonnet review, cycle 9 — reachable via the approval
        // handoff's draft hide). The blanks become scrollback whitespace.
        let shrink = row_state.drawn_rows.saturating_sub(wanted_rows);
        if shrink > 0 {
            let _ = write!(err, "{}", "\n".repeat(shrink));
        }
        let _ = write!(err, "\r");
        for row in &upper {
            let _ = write!(err, "{row}\r\n");
        }
        let _ = write!(err, "{status}");
        row_state.drawn_rows = wanted_rows;
        row_state.region_dirty = false;
        row_state.last_upper = upper;
    }
    if let Some(col) = cursor_col {
        let _ = write!(err, "\x1b[{col}G");
    }
    let _ = err.flush();
}

fn redraw() {
    if SUSPEND_DEPTH.load(Ordering::Acquire) > 0 {
        return;
    }
    let mut row_state = state()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    redraw_locked(&mut row_state);
}

fn clear_row() {
    let mut row_state = state()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if row_state.drawn_rows > 0 {
        let mut err = io::stderr().lock();
        clear_region_locked(&mut err, row_state.drawn_rows);
        let _ = err.flush();
        row_state.drawn_rows = 0;
    }
}

/// Run `write` with the live row lifted off the terminal, repainting it after
/// the writes land. Reentrant: only the outermost caller touches the terminal.
/// A no-op (beyond the closure) when no row is drawn.
pub fn with_live_row_suspended<T>(write: impl FnOnce() -> T) -> T {
    struct Guard;
    impl Drop for Guard {
        fn drop(&mut self) {
            if SUSPEND_DEPTH.fetch_sub(1, Ordering::AcqRel) == 1 {
                redraw();
            }
        }
    }
    if SUSPEND_DEPTH.fetch_add(1, Ordering::AcqRel) == 0 {
        clear_row();
    }
    let _guard = Guard;
    write()
}

/// The current live-row suspension depth, for tests that must prove a routed
/// writer balances its own suspension (a leaked one would lift the row for the
/// rest of the process without failing any other assertion).
#[cfg(test)]
pub(crate) fn suspend_depth() -> usize {
    SUSPEND_DEPTH.load(Ordering::Acquire)
}

/// Show the input draft on the live row. `prompt` may carry styling; it is
/// measured through the sanitizer.
pub fn live_input_begin(prompt: String) {
    {
        let mut row_state = state()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        row_state.draft = Some(LiveDraft {
            prompt,
            ..LiveDraft::default()
        });
    }
    redraw();
}

/// Update the draft text and cursor (char index). No-op if no draft is live.
pub fn live_input_update(buffer: String, cursor: usize, hint: String, hint_reserved: bool) {
    {
        let mut row_state = state()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(draft) = row_state.draft.as_mut() else {
            return;
        };
        draft.cursor = cursor.min(buffer.chars().count());
        draft.buffer = buffer;
        draft.hint = hint;
        draft.hint_reserved = hint_reserved;
    }
    redraw();
}

/// Update the pending-queue state: the depth (dimmed `[N queued]` tag on the
/// status row; 0 hides it) and up to the first three queued prompts, shown as
/// dimmed stack rows above the status row. No-op when nothing changed.
pub fn live_queue_state(queued: usize, previews: Vec<String>) {
    {
        let mut row_state = state()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if row_state.queued == queued && row_state.previews == previews {
            return;
        }
        row_state.queued = queued;
        row_state.previews = previews;
        row_state.region_dirty = true;
    }
    redraw();
}

/// Update the background-agent counts behind the status row's ambient tag:
/// `blocked` detached agents waiting on the user, `running` ones still
/// working (0/0 hides the tag). No-op when nothing changed. Unlike
/// [`live_queue_state`] this touches only the bottom status row, so it leaves
/// `region_dirty` alone — the collector calls it on every poll tick and a
/// full-region repaint per count change would be wasted work.
pub fn live_agents_state(blocked: usize, running: usize) {
    {
        let mut row_state = state()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if row_state.blocked == blocked && row_state.running == running {
            return;
        }
        row_state.blocked = blocked;
        row_state.running = running;
    }
    redraw();
}

/// Remove the input draft from the live row (spinner, if any, remains).
pub fn live_input_end() {
    {
        let mut row_state = state()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        row_state.draft = None;
    }
    redraw();
}

/// Replace the spinner segment on the live row (`None` removes it). The
/// segment must be one physical line with no carriage returns.
pub(crate) fn set_spinner_segment(segment: Option<String>) {
    {
        let mut row_state = state()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        row_state.spinner = segment;
    }
    redraw();
}

/// Compose the inline viewport's pinned input row: *exactly* the classic
/// during-turn row for the same draft, minus the spinner and the queue and
/// agent tags the inline surface does not have.
///
/// Additive by design. The inline renderer needs the same hint handling this
/// module already implements -- a reserved reverse-search status that claims
/// its columns before the draft is windowed, an unreserved completion suffix
/// dropped when the row is tight -- and reimplementing that math in `kitsu`
/// is how the two renderers drift. So the inline path delegates here instead
/// of concatenating its own row, and `compose_row` stays untouched.
pub fn compose_input_row(draft: &LiveDraft, cols: usize) -> (String, Option<usize>) {
    compose_row(None, Some(draft), 0, 0, 0, cols)
}

/// The width of the terminal the live row draws on, shared so the inline
/// guard -- which is handed only a row count -- does not re-probe the
/// terminal itself. A second probe is a second source of truth, which is the
/// drift this seam exists to prevent. Never 0: the probe falls back to 80.
pub fn terminal_columns() -> usize {
    terminal_cols()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_draft_fits_unclipped_with_cursor_after_prefix() {
        let (visible, offset, windowed) = fit_draft_window("hello world", 5, 40);
        assert!(!windowed);
        assert_eq!(visible, "hello world");
        assert_eq!(offset, 5);
    }

    #[test]
    fn long_draft_windows_around_cursor_with_ellipses() {
        let buffer = "abcdefghijklmnopqrstuvwxyz";
        let (visible, offset, windowed) = fit_draft_window(buffer, 13, 10);
        assert!(windowed);
        assert!(visible.starts_with('…') && visible.ends_with('…'));
        assert!(display_width(&visible) <= 10);
        let core: String = visible.chars().filter(|c| *c != '…').collect();
        assert!(core.contains('m'), "cursor char stays visible: {visible}");
        assert!(offset <= display_width(&visible));
    }

    #[test]
    fn cursor_at_end_of_long_draft_shows_tail() {
        let buffer = "abcdefghijklmnopqrstuvwxyz";
        let (visible, offset, _) = fit_draft_window(buffer, 26, 10);
        assert!(visible.starts_with('…'));
        assert!(visible.ends_with('z'));
        assert_eq!(offset, display_width(&visible));
    }

    #[test]
    fn compose_row_places_cursor_after_prompt_and_prefix() {
        let draft = LiveDraft {
            prompt: "kitsu> ".to_string(),
            buffer: "abc".to_string(),
            cursor: 2,
            ..LiveDraft::default()
        };
        let (row, col) = compose_row(None, Some(&draft), 0, 0, 0, 80);
        assert_eq!(row, "kitsu> abc");
        // 7 prompt columns + 2 chars, 1-based.
        assert_eq!(col, Some(10));
    }

    #[test]
    fn compose_row_spinner_only_has_no_cursor_column() {
        let (row, col) = compose_row(Some("* thinking"), None, 0, 0, 0, 80);
        assert_eq!(row, "* thinking");
        assert_eq!(col, None);
    }

    #[test]
    fn narrow_terminal_drops_spinner_but_keeps_draft() {
        let draft = LiveDraft {
            prompt: "kitsu> ".to_string(),
            buffer: "hello".to_string(),
            cursor: 5,
            ..LiveDraft::default()
        };
        let (row, col) = compose_row(Some("( .a. ) kitsu is thinking"), Some(&draft), 0, 0, 0, 20);
        assert!(!row.contains("thinking"));
        assert!(row.starts_with("kitsu> "));
        assert_eq!(col, Some(13));
    }

    #[test]
    fn newlines_and_tabs_render_as_visible_placeholders() {
        let (visible, offset, _) = fit_draft_window("one\ntwo\tend", 4, 40);
        assert_eq!(visible, "one⏎two⇥end");
        // Cursor sits just past the rendered ⏎.
        assert_eq!(offset, 3 + display_width("⏎"));
    }

    #[test]
    fn hint_renders_dimmed_after_the_draft_without_moving_the_cursor() {
        let draft = LiveDraft {
            prompt: "kitsu> ".to_string(),
            buffer: "/que".to_string(),
            cursor: 4,
            hint: "ue".to_string(),
            hint_reserved: false,
        };
        let (row, col) = compose_row(None, Some(&draft), 0, 0, 0, 80);
        assert!(row.contains("\u{1b}[2m"), "hint must be dimmed: {row:?}");
        assert!(sanitize_terminal_text(&row).contains("/queue"));
        // Cursor sits right after the typed text, before the hint.
        assert_eq!(col, Some(12));
    }

    #[test]
    fn hint_is_suppressed_mid_buffer_and_when_it_has_no_room() {
        let mid_cursor = LiveDraft {
            prompt: "> ".to_string(),
            buffer: "/que".to_string(),
            cursor: 2,
            hint: "ue".to_string(),
            hint_reserved: false,
        };
        let (row, _) = compose_row(None, Some(&mid_cursor), 0, 0, 0, 80);
        assert!(!row.contains("\u{1b}[2m"), "no hint mid-buffer: {row:?}");

        let no_room = LiveDraft {
            prompt: "> ".to_string(),
            buffer: "/status".to_string(),
            cursor: 7,
            hint: "extra".to_string(),
            hint_reserved: false,
        };
        // avail = 10 - 2 - 1 = 7: the buffer exactly fills it.
        let (row, _) = compose_row(None, Some(&no_room), 0, 0, 0, 10);
        assert!(
            !row.contains("extra"),
            "hint must never displace text: {row:?}"
        );
    }

    #[test]
    fn hint_respects_windowing_even_when_the_buffer_contains_a_literal_ellipsis() {
        // Windowed + literal `…` in the buffer: the hint must stay hidden
        // (the pre-fix heuristic sniffed `…` out of the visible text and
        // wrongly rendered a hint over trimmed content). A MID-line cursor
        // keeps the window; soft-wrap only engages at end-of-line, where a
        // hint became legitimate (nothing on the row is hidden anymore).
        let trimmed = LiveDraft {
            prompt: "> ".to_string(),
            buffer: "/que…something-long-enough-to-trim".to_string(),
            cursor: 10,
            hint: "ue".to_string(),
            hint_reserved: false,
        };
        let (row, _) = compose_row(None, Some(&trimmed), 0, 0, 0, 16);
        assert!(
            !row.contains("\u{1b}[2m"),
            "no hint on a windowed draft: {row:?}"
        );
        // End-of-line on the same overlong buffer soft-wraps: the tail
        // segment fully fits, so the hint may render (new, intended).
        let wrapped = LiveDraft {
            cursor: 34,
            ..trimmed.clone()
        };
        let (row, _) = compose_row(None, Some(&wrapped), 0, 0, 0, 16);
        assert!(row.contains("\u{1b}[2m"), "hint on wrapped tail: {row:?}");

        // Unwindowed + literal `…`: the hint still renders.
        let fits = LiveDraft {
            prompt: "> ".to_string(),
            buffer: "/que".to_string(),
            cursor: 4,
            hint: "ue".to_string(),
            hint_reserved: false,
        };
        let (row, _) = compose_row(None, Some(&fits), 0, 0, 0, 80);
        assert!(row.contains("\u{1b}[2m"));
    }

    #[test]
    fn queue_tag_renders_dimmed_and_shifts_the_cursor_column() {
        let draft = LiveDraft {
            prompt: "kitsu> ".to_string(),
            buffer: "abc".to_string(),
            cursor: 2,
            ..LiveDraft::default()
        };
        let (row, col) = compose_row(None, Some(&draft), 0, 0, 2, 80);
        assert!(sanitize_terminal_text(&row).starts_with("[2 queued] kitsu> abc"));
        assert!(row.contains("\u{1b}[2m"), "tag must be dimmed: {row:?}");
        // "[2 queued] " is 11 columns; 7 prompt + 2 chars + 1 (1-based).
        assert_eq!(col, Some(21));
        // Zero queued: no tag, original column.
        let (row, col) = compose_row(None, Some(&draft), 0, 0, 0, 80);
        assert!(!sanitize_terminal_text(&row).contains("queued"));
        assert_eq!(col, Some(10));
    }

    #[test]
    fn reserved_hint_survives_a_windowed_draft() {
        // A reverse-search status must stay visible even when the match is
        // long enough to window; the draft shrinks to make room.
        let draft = LiveDraft {
            prompt: "> ".to_string(),
            buffer: "a very long matched history entry indeed".to_string(),
            cursor: 40,
            hint: "  ⌕ reverse-search 'match'".to_string(),
            hint_reserved: true,
        };
        let (row, _) = compose_row(None, Some(&draft), 0, 0, 0, 48);
        let plain = sanitize_terminal_text(&row);
        assert!(
            plain.contains("reverse-search 'match'"),
            "status lost: {plain}"
        );
        assert!(plain.contains('…'), "draft should window: {plain}");
        // Unreserved hints keep the old suppress-on-window behavior.
        let unreserved = LiveDraft {
            hint_reserved: false,
            ..draft
        };
        let (row, _) = compose_row(None, Some(&unreserved), 0, 0, 0, 48);
        assert!(!sanitize_terminal_text(&row).contains("reverse-search"));
    }

    #[test]
    fn stack_rows_list_upcoming_prompts_in_order_with_a_more_row() {
        let previews = vec![
            "first queued".to_string(),
            "second queued".to_string(),
            "third queued".to_string(),
        ];
        let rows = compose_stack_rows(&previews, 5, 80);
        assert_eq!(rows.len(), 4);
        let plain: Vec<String> = rows.iter().map(|r| sanitize_terminal_text(r)).collect();
        assert!(plain[0].contains("next: first queued"));
        assert!(plain[1].contains("then: second queued"));
        assert!(plain[2].contains("then: third queued"));
        assert!(plain[3].contains("+2 more queued"));
        for row in &rows {
            assert!(row.contains("\u{1b}[2m"), "stack rows are dimmed: {row:?}");
        }
    }

    #[test]
    fn stack_rows_truncate_and_flatten_multiline_prompts() {
        let previews = vec!["a multi\nline prompt that is quite long indeed".to_string()];
        let rows = compose_stack_rows(&previews, 1, 24);
        assert_eq!(rows.len(), 1);
        let plain = sanitize_terminal_text(&rows[0]);
        assert!(plain.ends_with('…'), "long preview truncates: {plain}");
        assert!(display_width(&plain) <= 24);
        assert!(
            plain.contains('⏎'),
            "newline renders as placeholder: {plain}"
        );
        // Empty queue: no rows.
        assert!(compose_stack_rows(&[], 0, 80).is_empty());
    }

    #[test]
    fn soft_wrap_segments_and_rows_agree_with_the_bottom_row() {
        // 26 chars at width 10 → segments of 10/10/6.
        let line = "abcdefghijklmnopqrstuvwxyz";
        let segments = wrap_segments(line, 10);
        assert_eq!(segments, vec!["abcdefghij", "klmnopqrst", "uvwxyz"]);
        // Cursor at end of the overlong line, prompt "kitsu> " (7) at 18
        // cols → avail = 10; bottom row shows the FINAL segment, cursor
        // after it, and the two earlier segments render as wrap rows.
        let draft = LiveDraft {
            prompt: "kitsu> ".to_string(),
            buffer: line.to_string(),
            cursor: 26,
            ..LiveDraft::default()
        };
        let (row, col) = compose_row(None, Some(&draft), 0, 0, 0, 18);
        let plain = sanitize_terminal_text(&row);
        assert!(
            plain.ends_with("uvwxyz"),
            "bottom shows final segment: {plain}"
        );
        assert!(
            !plain.contains('…'),
            "soft wrap replaces the window: {plain}"
        );
        assert_eq!(col, Some(7 + 6 + 1));
        let rows = compose_draft_context_rows(line, 26, 18, 10);
        let plain_rows: Vec<String> = rows.iter().map(|r| sanitize_terminal_text(r)).collect();
        assert_eq!(plain_rows.len(), 2, "{plain_rows:?}");
        assert!(plain_rows[0].contains("abcdefghij"));
        assert!(plain_rows[1].contains("klmnopqrst"));
        // Enough segments to exceed the context budget collapse older ones
        // into a marker (200 chars at width 10 → 20 segments, 19 earlier).
        let long = "x".repeat(200);
        let rows = compose_draft_context_rows(&long, 200, 80, 10);
        let plain_rows: Vec<String> = rows.iter().map(|r| sanitize_terminal_text(r)).collect();
        assert!(plain_rows[0].contains("earlier segment(s)"), "{plain_rows:?}");
        let shown = plain_rows.iter().filter(|r| r.contains("│")).count();
        assert_eq!(shown, DRAFT_CONTEXT_ROWS, "shows the budget worth of segments: {plain_rows:?}");
    }

    #[test]
    fn soft_wrap_stays_off_mid_line_when_short_and_under_reserved_hints() {
        let line = "abcdefghijklmnopqrstuvwxyz";
        // Mid-line cursor keeps the horizontal window (no wrap rows).
        assert!(compose_draft_context_rows(line, 12, 18, 10).is_empty());
        let draft = LiveDraft {
            prompt: "kitsu> ".to_string(),
            buffer: line.to_string(),
            cursor: 12,
            ..LiveDraft::default()
        };
        let (row, _) = compose_row(None, Some(&draft), 0, 0, 0, 18);
        assert!(
            sanitize_terminal_text(&row).contains('…'),
            "window still used"
        );
        // Short lines never wrap.
        assert!(compose_draft_context_rows("short", 5, 18, 10).is_empty());
        // segment_width 0 (reserved-hint mode) disables wrap rows.
        assert!(compose_draft_context_rows(line, 26, 18, 0).is_empty());
        // Tiny widths refuse to wrap (fall back to the window).
        assert!(compose_draft_context_rows(line, 26, 18, 5).is_empty());
    }

    #[test]
    fn current_line_bounds_split_on_newlines() {
        assert_eq!(current_line_bounds("abc", 2), (0, 3, 2));
        // "ab\ncd\nef" — cursor on 'd' (index 4): line "cd" spans 3..5.
        assert_eq!(current_line_bounds("ab\ncd\nef", 4), (3, 5, 1));
        // Cursor exactly ON a newline belongs to the line it terminates.
        assert_eq!(current_line_bounds("ab\ncd", 2), (0, 2, 2));
        // Cursor at end of the last line.
        assert_eq!(current_line_bounds("ab\ncd", 5), (3, 5, 2));
    }

    #[test]
    fn multiline_draft_shows_cursor_line_with_context_rows_above() {
        let draft = LiveDraft {
            prompt: "kitsu> ".to_string(),
            buffer: "first line\nsecond line\nthird".to_string(),
            cursor: 28, // end of "third"
            ..LiveDraft::default()
        };
        let (row, col) = compose_row(None, Some(&draft), 0, 0, 0, 80);
        let plain = sanitize_terminal_text(&row);
        assert!(
            plain.contains("third"),
            "status row shows cursor line: {plain}"
        );
        assert!(
            !plain.contains("first"),
            "earlier lines are context rows: {plain}"
        );
        // 7 prompt + 5 chars of "third" + 1.
        assert_eq!(col, Some(13));

        let rows = compose_draft_context_rows(&draft.buffer, draft.cursor, 80, 70);
        let plain: Vec<String> = rows.iter().map(|r| sanitize_terminal_text(r)).collect();
        assert_eq!(plain.len(), 2);
        assert!(plain[0].contains("first line"));
        assert!(plain[1].contains("second line"));
    }

    #[test]
    fn input_box_is_always_framed_with_a_caret_even_when_empty() {
        // Empty draft: a 3-row full-width box (top, one content row, bottom),
        // caret on the content row just after the prompt. This is the fix for
        // "no box at idle + stranded cursor": the region and caret exist with
        // no input.
        let draft = LiveDraft { prompt: "kitsu> ".to_string(), ..LiveDraft::default() };
        let b = compose_input_box(&draft, 40);
        assert_eq!(b.rows.len(), 3, "empty box is top+content+bottom: {:?}", b.rows);
        assert!(sanitize_terminal_text(&b.rows[0]).starts_with("╭"));
        assert!(sanitize_terminal_text(&b.rows[2]).starts_with("╰"));
        // Full width: every row is `cols` display columns.
        for row in &b.rows {
            assert_eq!(display_width(&sanitize_terminal_text(row)), 40, "full-width: {row:?}");
        }
        // Caret on row 2 (1-based: top border is 1), column after "│ kitsu> ".
        assert_eq!(b.caret.0, 2, "caret on the content row");
        assert_eq!(b.caret.1, 2 + "kitsu> ".len() + 1, "caret just past the prompt");
    }

    #[test]
    fn input_box_places_caret_after_typed_text_and_grows_multiline() {
        // Single line: caret advances with the cursor.
        let draft = LiveDraft {
            prompt: "kitsu> ".to_string(),
            buffer: "hello".to_string(),
            cursor: 5,
            ..LiveDraft::default()
        };
        let b = compose_input_box(&draft, 40);
        assert_eq!(b.rows.len(), 3);
        assert!(sanitize_terminal_text(&b.rows[1]).contains("kitsu> hello"));
        assert_eq!(b.caret.1, 2 + "kitsu> hello".len() + 1);

        // Multi-line: the box grows; prompt prefixes the FIRST line, caret on
        // the cursor's line.
        let draft = LiveDraft {
            prompt: "kitsu> ".to_string(),
            buffer: "first\nsecond\nthird".to_string(),
            cursor: 18, // end of "third"
            ..LiveDraft::default()
        };
        let b = compose_input_box(&draft, 40);
        // top + "kitsu> first" + "second" + "third" + bottom = 5 rows.
        assert_eq!(b.rows.len(), 5, "{:?}", b.rows);
        assert!(sanitize_terminal_text(&b.rows[1]).contains("kitsu> first"), "{:?}", b.rows);
        assert!(sanitize_terminal_text(&b.rows[3]).contains("third"), "{:?}", b.rows);
        assert!(!sanitize_terminal_text(&b.rows[3]).contains("kitsu>"), "prompt only on first line");
        assert_eq!(b.caret.0, 4, "caret on the cursor's line (row 4, 1-based)");
        assert_eq!(b.caret.1, 2 + "third".len() + 1, "caret after 'third'");
    }

    #[test]
    fn fit_input_box_keeps_both_borders_and_the_caret_row() {
        // Review F8: a box taller than the grant must lose CONTENT rows, never
        // the top border (the viewport would otherwise drop it from the top).
        let buffer: String = (1..=20).map(|n| format!("l{n}\n")).collect::<String>() + "end";
        let draft = LiveDraft {
            prompt: "kitsu> ".to_string(),
            buffer: buffer.clone(),
            cursor: buffer.chars().count(),
            ..LiveDraft::default()
        };
        let full = compose_input_box(&draft, 40);
        assert!(full.rows.len() > 6, "precondition: taller than the grant");
        let fit = fit_input_box(full, 6);
        assert_eq!(fit.rows.len(), 6, "{:?}", fit.rows);
        assert!(sanitize_terminal_text(&fit.rows[0]).starts_with('╭'), "top border kept");
        assert!(sanitize_terminal_text(&fit.rows[5]).starts_with('╰'), "bottom border kept");
        let caret_row = sanitize_terminal_text(&fit.rows[fit.caret.0 - 1]);
        assert!(caret_row.contains("end"), "caret row survives: {caret_row:?}");

        // A box that already fits is untouched.
        let small = compose_input_box(&LiveDraft { prompt: "k> ".to_string(), ..LiveDraft::default() }, 40);
        assert_eq!(fit_input_box(small, 6).rows.len(), 3);
    }

    #[test]
    fn input_box_soft_wraps_a_long_single_line_into_multiple_rows() {
        // A long line with NO newlines must WRAP into several content rows (the
        // box grows), not truncate to one row with '…'. inner = 40-4 = 36.
        let text: String = "x".repeat(100);
        let draft = LiveDraft {
            prompt: "kitsu> ".to_string(),
            buffer: text.clone(),
            cursor: text.chars().count(),
            ..LiveDraft::default()
        };
        let b = compose_input_box(&draft, 40);
        // "kitsu> " (7) + 100 x = 107 cols / 36 inner → ceil = 3 content rows.
        let content = b.rows.len() - 2; // minus top+bottom borders
        assert_eq!(content, 3, "long line wraps across rows: {:?}", b.rows);
        // No ellipsis: wrapping replaced truncation.
        assert!(!b.rows.iter().any(|r| sanitize_terminal_text(r).contains('…')), "no truncation: {:?}", b.rows);
        // Caret is on the LAST content row (row index: top border=1, +3 content).
        assert_eq!(b.caret.0, 4, "caret on the last wrapped row: {:?}", b.rows);
    }

    #[test]
    fn context_rows_grow_with_the_draft_and_collapse_only_past_the_budget() {
        // A 6-line draft (well under DRAFT_CONTEXT_ROWS) shows every earlier
        // line — the box grows instead of a 2-line peephole. Cursor in l5.
        let buffer = "l1\nl2\nl3\nl4\nl5\nl6";
        let rows = compose_draft_context_rows(buffer, 13, 80, 70);
        let plain: Vec<String> = rows.iter().map(|r| sanitize_terminal_text(r)).collect();
        assert!(!plain.iter().any(|r| r.contains("earlier line(s)")), "no collapse under budget: {plain:?}");
        assert!(plain[0].contains("l1") && plain[1].contains("l2"), "earliest lines shown: {plain:?}");
        assert!(plain.iter().any(|r| r.contains("l4")), "all context lines shown: {plain:?}");
        assert!(plain.last().unwrap().contains("+1 line(s) below — Enter submits all"));

        // A draft with more earlier lines than the budget DOES collapse the
        // overflow into the marker, keeping the box bounded.
        let big: String = (1..=30).map(|n| format!("line{n}\n")).collect::<String>() + "cursorline";
        let cursor = big.chars().count(); // at the end, after 30 newlines
        let rows = compose_draft_context_rows(&big, cursor, 80, 70);
        let plain: Vec<String> = rows.iter().map(|r| sanitize_terminal_text(r)).collect();
        assert!(plain[0].contains("earlier line(s)"), "overflow collapses: {plain:?}");
        // Exactly DRAFT_CONTEXT_ROWS context lines + the marker (+ cursor line).
        let shown = plain.iter().filter(|r| r.contains("│")).count();
        assert_eq!(shown, DRAFT_CONTEXT_ROWS, "shows the budget worth of lines: {plain:?}");

        // Single-line drafts produce no context rows.
        assert!(compose_draft_context_rows("plain", 3, 80, 70).is_empty());
        // Narrow terminals clamp context rows too.
        for row in compose_draft_context_rows(buffer, 13, 8, 6) {
            assert!(display_width(&sanitize_terminal_text(&row)) <= 8);
        }
    }

    #[test]
    fn stack_rows_never_exceed_a_narrow_terminal() {
        // Prefix (10 cols) wider than the terminal, and the '+N more' row
        // (18+ cols) — both must clamp, never wrap (row-count desync).
        let previews = vec![
            "one".to_string(),
            "two".to_string(),
            "three".to_string(),
            "four".to_string(),
        ];
        for cols in [4usize, 8, 10, 12, 17] {
            let rows = compose_stack_rows(&previews, 9, cols);
            for row in &rows {
                let width = display_width(&sanitize_terminal_text(row));
                assert!(
                    width <= cols,
                    "row wider than {cols} cols ({width}): {row:?}"
                );
            }
        }
    }

    fn drafted_state() -> LiveRowState {
        LiveRowState {
            spinner: Some("* thinking".to_string()),
            draft: Some(LiveDraft {
                prompt: "kitsu> ".to_string(),
                buffer: "abc".to_string(),
                cursor: 3,
                ..LiveDraft::default()
            }),
            ..LiveRowState::default()
        }
    }

    #[test]
    fn live_region_leaves_transcript_space_on_a_short_terminal() {
        let mut state = drafted_state();
        state.queued = 4;
        state.previews = vec!["queued work".to_string(); 4];
        state.draft.as_mut().unwrap().buffer = "first\nsecond\nthird".to_string();
        state.draft.as_mut().unwrap().cursor = 18;
        let mut out = Vec::new();
        render_region(&mut out, &mut state, 20, 3);
        assert!(state.drawn_rows <= 2);
        assert!(state.last_upper.len() <= 1);
    }

    #[test]
    fn spinner_and_prompt_cannot_wrap_outside_the_accounted_live_rows() {
        for cols in [1, 4, 8, 16, 32] {
            for has_draft in [false, true] {
                let mut state = drafted_state();
                state.spinner = Some("a long thinking spinner that cannot fit".to_string());
                if !has_draft {
                    state.draft = None;
                }
                let mut out = Vec::new();
                render_region(&mut out, &mut state, cols, 3);
                let text = sanitize_terminal_text(&String::from_utf8(out).unwrap());
                assert!(
                    display_width(text.trim_matches(['\r', '\n'])) < cols,
                    "{cols}: {text:?}"
                );
                assert_eq!(state.drawn_rows, 1);
            }
        }
    }

    #[test]
    fn render_region_first_paint_then_fast_path_then_full_repaint() {
        let mut state = drafted_state();
        let mut out: Vec<u8> = Vec::new();
        // First paint: full path (last_cols 0 → dirty), one row.
        render_region(&mut out, &mut state, 80, 24);
        assert_eq!(state.drawn_rows, 1);
        assert!(!state.region_dirty);
        // Steady state: the fast path repaints ONLY the bottom row in place.
        out.clear();
        render_region(&mut out, &mut state, 80, 24);
        let bytes = String::from_utf8(out.clone()).unwrap();
        assert!(bytes.starts_with("\r\u{1b}[2K"), "fast path clears one row");
        assert!(
            !bytes.contains("\r\n"),
            "fast path must not touch upper rows: {bytes:?}"
        );
        assert_eq!(state.drawn_rows, 1);
        // Queue content appears: full repaint grows the region.
        state.queued = 2;
        state.previews = vec!["first".to_string(), "second".to_string()];
        state.region_dirty = true;
        out.clear();
        render_region(&mut out, &mut state, 80, 24);
        assert_eq!(state.drawn_rows, 3);
        let bytes = String::from_utf8(out).unwrap();
        assert_eq!(bytes.matches("\r\n").count(), 2, "two upper rows painted");
        assert_eq!(state.last_upper.len(), 2);
    }

    #[test]
    fn render_region_shrink_steps_down_and_clear_ends_at_zero() {
        let mut state = drafted_state();
        state.queued = 2;
        state.previews = vec!["first".to_string(), "second".to_string()];
        let mut out: Vec<u8> = Vec::new();
        render_region(&mut out, &mut state, 80, 24);
        assert_eq!(state.drawn_rows, 3);
        // Draft hidden (approval handoff): spinner-only, shrink 3 → 1.
        state.draft = None;
        out.clear();
        render_region(&mut out, &mut state, 80, 24);
        assert_eq!(state.drawn_rows, 1);
        let bytes = String::from_utf8(out.clone()).unwrap();
        // Old region cleared bottom-up (two 1A2K pairs), then two \n steps
        // down so the shorter region hugs the bottom.
        assert!(bytes.contains("\u{1b}[1A\u{1b}[2K\u{1b}[1A\u{1b}[2K"));
        assert!(bytes.contains("\n\n"), "shrink steps down: {bytes:?}");
        // Everything gone: clear-only branch zeroes the region.
        state.spinner = None;
        out.clear();
        render_region(&mut out, &mut state, 80, 24);
        assert_eq!(state.drawn_rows, 0);
        // And a further call writes nothing at all.
        out.clear();
        render_region(&mut out, &mut state, 80, 24);
        assert!(out.is_empty());
    }

    #[test]
    fn render_region_resize_and_upper_content_changes_force_full_repaint() {
        let mut state = drafted_state();
        state.draft.as_mut().unwrap().buffer = "one\ntwo\nthree".to_string();
        state.draft.as_mut().unwrap().cursor = 13; // end of "three"
        let mut out: Vec<u8> = Vec::new();
        render_region(&mut out, &mut state, 80, 24);
        let rows_before = state.drawn_rows;
        assert!(rows_before > 1, "multi-line draft has context rows");
        // Same content, same width: fast path (no upper repaint).
        out.clear();
        render_region(&mut out, &mut state, 80, 24);
        assert!(!String::from_utf8(out.clone()).unwrap().contains("\r\n"));
        // Cursor crosses a line boundary: same row count possible, but the
        // upper CONTENT changes → last_upper comparison forces full repaint.
        state.draft.as_mut().unwrap().cursor = 5; // inside "two"
        out.clear();
        render_region(&mut out, &mut state, 80, 24);
        assert!(
            String::from_utf8(out.clone()).unwrap().contains("\r\n"),
            "upper-content change repaints fully"
        );
        // Resize: content identical but width changed → full repaint.
        out.clear();
        render_region(&mut out, &mut state, 60, 24);
        assert!(String::from_utf8(out).unwrap().contains("\r\n"));
        assert_eq!(state.last_cols, 60);
    }

    #[test]
    fn clear_region_walks_bottom_up_and_noops_on_zero() {
        let mut out: Vec<u8> = Vec::new();
        clear_region_locked(&mut out, 0);
        assert!(out.is_empty());
        clear_region_locked(&mut out, 1);
        assert_eq!(out, b"\r\x1b[2K");
        out.clear();
        clear_region_locked(&mut out, 3);
        assert_eq!(out, b"\r\x1b[2K\x1b[1A\x1b[2K\x1b[1A\x1b[2K");
    }

    #[test]
    fn queue_tag_only_renders_alongside_a_draft() {
        let (row, _) = compose_row(Some("* thinking"), None, 0, 0, 3, 80);
        assert!(!row.contains("queued"), "spinner-only row carries no tag");
    }

    #[test]
    fn styled_prompt_measures_by_visible_width() {
        let draft = LiveDraft {
            prompt: "\u{1b}[1mkitsu>\u{1b}[0m ".to_string(),
            buffer: String::new(),
            cursor: 0,
            ..LiveDraft::default()
        };
        let (_, col) = compose_row(None, Some(&draft), 0, 0, 0, 80);
        assert_eq!(col, Some(8));
    }

    #[test]
    fn agents_tag_truth_table_ranks_blocked_over_running() {
        // (blocked, running) -> rendered tag (trailing space included).
        let cases: &[(usize, usize, Option<&str>)] = &[
            (0, 0, None),
            (0, 1, Some("[1 agent] ")),
            (0, 2, Some("[2 agents] ")),
            (0, 7, Some("[7 agents] ")),
            (1, 0, Some("[1 blocked] ")),
            (3, 0, Some("[3 blocked] ")),
            // Blocked outranks running: an agent waiting on the user is the
            // thing the user must act on.
            (2, 5, Some("[2 blocked] ")),
            (1, 1, Some("[1 blocked] ")),
        ];
        for &(blocked, running, expected) in cases {
            assert_eq!(
                agents_tag(blocked, running).as_deref(),
                expected,
                "agents_tag({blocked}, {running})"
            );
        }
    }

    #[test]
    fn agents_tag_renders_dimmed_and_shifts_the_cursor_column() {
        let draft = LiveDraft {
            prompt: "kitsu> ".to_string(),
            buffer: "abc".to_string(),
            cursor: 2,
            ..LiveDraft::default()
        };
        let (row, col) = compose_row(None, Some(&draft), 1, 0, 0, 80);
        assert!(sanitize_terminal_text(&row).starts_with("[1 blocked] kitsu> abc"));
        assert!(row.contains("\u{1b}[2m"), "tag must be dimmed: {row:?}");
        // "[1 blocked] " is 12 columns; + 7 prompt + 2 chars + 1 (1-based).
        assert_eq!(col, Some(12 + 7 + 2 + 1));
        // Running-only tag is narrower by its wording.
        let (row, col) = compose_row(None, Some(&draft), 0, 2, 0, 80);
        assert!(sanitize_terminal_text(&row).starts_with("[2 agents] kitsu> abc"));
        assert!(row.contains("\u{1b}[2m"), "tag must be dimmed: {row:?}");
        // "[2 agents] " is 11 columns; + 7 prompt + 2 chars + 1 (1-based).
        assert_eq!(col, Some(11 + 7 + 2 + 1));
        // No agents: no tag, original column.
        let (row, col) = compose_row(None, Some(&draft), 0, 0, 0, 80);
        assert!(!sanitize_terminal_text(&row).contains("agent"));
        assert!(!sanitize_terminal_text(&row).contains("blocked"));
        assert_eq!(col, Some(10));
    }

    #[test]
    fn agents_tag_precedes_the_queue_tag_and_both_shift_the_cursor() {
        let draft = LiveDraft {
            prompt: "kitsu> ".to_string(),
            buffer: "abc".to_string(),
            cursor: 2,
            ..LiveDraft::default()
        };
        let (row, col) = compose_row(None, Some(&draft), 1, 0, 2, 80);
        let plain = sanitize_terminal_text(&row);
        assert_eq!(plain, "[1 blocked] [2 queued] kitsu> abc");
        assert!(row.contains("\u{1b}[2m"), "tags must be dimmed: {row:?}");
        // "[1 blocked] " 12 + "[2 queued] " 11 + 7 prompt + 2 chars + 1.
        assert_eq!(col, Some(12 + 11 + 7 + 2 + 1));
        // Running-only variant keeps the same ordering.
        let (row, col) = compose_row(None, Some(&draft), 0, 1, 3, 80);
        assert_eq!(
            sanitize_terminal_text(&row),
            "[1 agent] [3 queued] kitsu> abc"
        );
        // "[1 agent] " 10 + "[3 queued] " 11 + 7 + 2 + 1.
        assert_eq!(col, Some(10 + 11 + 7 + 2 + 1));
    }

    #[test]
    fn agents_tag_only_renders_alongside_a_draft() {
        let (row, col) = compose_row(Some("* thinking"), None, 2, 3, 0, 80);
        assert_eq!(row, "* thinking");
        assert_eq!(col, None);
        assert!(!row.contains("blocked"), "spinner-only row carries no tag");
        assert!(!row.contains("agent"), "spinner-only row carries no tag");
    }

    #[test]
    fn agents_tag_row_never_wraps_across_a_width_sweep() {
        let draft = LiveDraft {
            prompt: "kitsu> ".to_string(),
            buffer: "the quick brown fox jumps over the lazy dog".to_string(),
            cursor: 20,
            ..LiveDraft::default()
        };
        for cols in 40usize..=200 {
            for &(blocked, running, queued) in
                &[(1usize, 0usize, 2usize), (0, 4, 1), (3, 7, 0), (0, 1, 9)]
            {
                let (row, col) = compose_row(
                    Some("( .a. ) kitsu is thinking"),
                    Some(&draft),
                    blocked,
                    running,
                    queued,
                    cols,
                );
                let width = display_width(&sanitize_terminal_text(&row));
                assert!(
                    width <= cols,
                    "row wider than {cols} cols ({width}) at ({blocked},{running},{queued}): {row:?}"
                );
                let col = col.expect("a draft row always reports a cursor column");
                assert!(
                    col >= 1 && col <= cols,
                    "cursor column {col} outside {cols} cols at ({blocked},{running},{queued})"
                );
            }
        }
    }

    #[test]
    fn agents_tag_width_agrees_between_compose_row_and_draft_segment_width() {
        let draft = LiveDraft {
            prompt: "kitsu> ".to_string(),
            buffer: "abc".to_string(),
            cursor: 3,
            ..LiveDraft::default()
        };
        let prompt_width = display_width(&draft.prompt);
        for cols in [40usize, 60, 80, 120] {
            for &(blocked, running, queued) in &[
                (0usize, 0usize, 0usize),
                (1, 0, 0),
                (0, 1, 0),
                (0, 2, 0),
                (2, 5, 3),
                (0, 0, 2),
            ] {
                let tag_width = agents_tag(blocked, running)
                    .as_deref()
                    .map(display_width)
                    .unwrap_or(0);
                let queue_width = if queued > 0 {
                    display_width(&format!("[{queued} queued] "))
                } else {
                    0
                };
                // No spinner, so the lead is 0 and the draft keeps every
                // column the tags and prompt do not claim, less the one
                // reserved so an end-of-buffer cursor cannot wrap.
                let expected = cols.saturating_sub(tag_width + queue_width + prompt_width + 1);
                assert_eq!(
                    draft_segment_width(None, &draft, blocked, running, queued, cols),
                    expected,
                    "segment width at {cols} cols with ({blocked},{running},{queued})"
                );
                // And the row's own arithmetic: the cursor sits one past the
                // tags, the prompt and the whole (fitting) buffer.
                let (_, col) = compose_row(None, Some(&draft), blocked, running, queued, cols);
                assert_eq!(
                    col,
                    Some(tag_width + queue_width + prompt_width + display_width(&draft.buffer) + 1),
                    "cursor column at {cols} cols with ({blocked},{running},{queued})"
                );
            }
        }
    }

    #[test]
    fn agents_tag_survives_the_sanitize_round_trip() {
        let draft = LiveDraft {
            prompt: "kitsu> ".to_string(),
            buffer: "abc".to_string(),
            cursor: 3,
            ..LiveDraft::default()
        };
        let (row, _) = compose_row(None, Some(&draft), 2, 1, 1, 80);
        assert!(row.contains("\u{1b}[2m"), "styled row should dim: {row:?}");
        let plain = sanitize_terminal_text(&row);
        assert!(plain.contains("[2 blocked] "), "plain tag text: {plain:?}");
        assert!(
            !plain.contains('\u{1b}'),
            "no escape bytes may survive: {plain:?}"
        );
    }

    #[test]
    fn narrow_terminal_drops_spinner_but_keeps_draft_and_agents_tag() {
        let draft = LiveDraft {
            prompt: "kitsu> ".to_string(),
            buffer: "hello".to_string(),
            cursor: 5,
            ..LiveDraft::default()
        };
        let (row, col) = compose_row(Some("( .a. ) kitsu is thinking"), Some(&draft), 1, 0, 0, 30);
        let plain = sanitize_terminal_text(&row);
        assert!(
            !plain.contains("thinking"),
            "spinner drops first: {plain:?}"
        );
        assert!(plain.starts_with("[1 blocked] kitsu> "), "{plain:?}");
        assert!(plain.contains("hello"), "draft must survive: {plain:?}");
        assert!(display_width(&plain) <= 30, "row must not wrap: {plain:?}");
        // 12 tag + 7 prompt + 5 chars + 1 (1-based).
        assert_eq!(col, Some(12 + 7 + 5 + 1));
    }

    /// Finding F15, pinned in the crate that owns the mechanism.
    ///
    /// The inline viewport pins its rows outside the DECSTBM scroll region and
    /// suspends nothing, so a live status row painted at the same time lands
    /// *inside* the region and either scrolls away with the transcript or
    /// fights the viewport's parked cursor. The resolution is that the
    /// viewport replaces the live row, which needs a process-wide off switch
    /// here: `set_live_rows_enabled` returns the previous setting and
    /// `live_rows_supported` consults it, so with it off every draw path
    /// degrades to the same pure bookkeeping a non-TTY session already gets.
    ///
    /// CAVEAT on the two global pieces this touches. The setting is
    /// process-wide and so is the live-row state, so the test restores both,
    /// and the setting is restored through `RestoreLiveRows` so an assertion
    /// that panics cannot leave the row disabled for the rest of this test
    /// binary. No serialisation lock is needed: the other tests in this module
    /// are pure composers that never consult the gate, and the gate's default
    /// is enabled, so the only observable window is this test's own body.
    struct RestoreLiveRows(bool);

    impl Drop for RestoreLiveRows {
        fn drop(&mut self) {
            set_live_rows_enabled(self.0);
        }
    }

    #[test]
    fn disabling_the_live_rows_stands_the_row_down_and_re_enabling_restores_it() {
        let _restore = RestoreLiveRows(set_live_rows_enabled(true));

        // Default enabled: with no viewport the gate is transparent and the
        // predicate is exactly the pre-F15 expression, so nothing changes byte
        // for byte for a classic session.
        assert_eq!(
            live_rows_supported(),
            crate::stderr_styled() && !crate::plain_mode(),
            "enabled, the gate must not alter the supported predicate"
        );

        // Disabled, the predicate is false unconditionally -- styled TTY or
        // not -- which is what makes the draw paths inert.
        let previous = set_live_rows_enabled(false);
        assert!(
            previous,
            "set_live_rows_enabled returns the previous setting"
        );
        assert!(
            !live_rows_supported(),
            "the live row must not be supported while the viewport owns the rows"
        );

        // A draft update writes nothing: with the row unsupported, `redraw`
        // returns before touching the terminal, so no region is ever painted
        // and `drawn_rows` stays at zero. That counter is the only
        // process-independent witness of "wrote bytes" available here, since
        // `redraw` takes the real stderr.
        live_input_begin("kitsu> ".to_string());
        live_input_update("hello".to_string(), 5, String::new(), false);
        assert_eq!(
            state()
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .drawn_rows,
            0,
            "a draft update must paint no rows while the live row is disabled"
        );
        live_input_end();

        // Re-enabling restores the previous behaviour: the predicate is the
        // bare pre-F15 expression again.
        let previous = set_live_rows_enabled(true);
        assert!(!previous, "the disabled setting is reported back");
        assert_eq!(
            live_rows_supported(),
            crate::stderr_styled() && !crate::plain_mode(),
            "re-enabling restores the original predicate exactly"
        );
    }

    // ------------------------------------------------------------------
    // The inline viewport's shared composition surface.
    //
    // The inline renderer (`--tui inline`) paints its own pinned row and
    // has no spinner, queue tag or agent tag to place, but it needs exactly
    // the hint handling this module already implements: a reserved
    // reverse-search status that claims its columns before the draft is
    // windowed, and an unreserved slash-completion suffix that is dropped
    // when the row is tight. Reimplementing that math in `kitsu` is how the
    // two renderers drift, so the inline path composes through here.
    //
    // `compose_input_row` is therefore required to be a thin, *additive*
    // wrapper over `compose_row` -- test 1 below pins equality with the
    // classic row rather than re-deriving the expected string, so the two
    // renderers cannot disagree about a draft they were told the same thing
    // about.
    // ------------------------------------------------------------------

    /// The inline row is byte-identical to the classic row for the same
    /// draft, with no spinner and no tags. Equality against `compose_row`,
    /// not against a literal: the point is shared composition, so anything
    /// that makes the inline row differ -- including a future change to
    /// windowing or hint placement -- must break this.
    #[test]
    fn compose_input_row_is_the_classic_row_without_spinner_or_tags() {
        for (buffer, cursor, hint, reserved, cols) in [
            ("/s", 2usize, "teer", false, 80usize),
            ("hello", 5, "", false, 80),
            (
                "a long matched history entry, long enough that it must window",
                61,
                "  ⌕ reverse-search 'match'",
                true,
                64,
            ),
            ("abcdefghij", 4, "klm", false, 14),
        ] {
            let draft = LiveDraft {
                prompt: "kitsu> ".to_string(),
                buffer: buffer.to_string(),
                cursor,
                hint: hint.to_string(),
                hint_reserved: reserved,
            };
            assert_eq!(
                compose_input_row(&draft, cols),
                compose_row(None, Some(&draft), 0, 0, 0, cols),
                "the inline row must BE the classic row for {buffer:?} at {cols} cols"
            );
        }
    }

    /// The unreserved slash hint renders dimmed immediately after the draft,
    /// and the cursor column stays *before* it so typing continues over it.
    #[test]
    fn compose_input_row_renders_an_unreserved_hint_dimmed_after_the_draft() {
        let draft = LiveDraft {
            prompt: "kitsu> ".to_string(),
            buffer: "/s".to_string(),
            cursor: 2,
            hint: "teer".to_string(),
            hint_reserved: false,
        };
        let (row, col) = compose_input_row(&draft, 80);
        assert_eq!(sanitize_terminal_text(&row), "kitsu> /steer");
        assert!(
            row.contains("\u{1b}[2mteer"),
            "hint must be dimmed: {row:?}"
        );
        // 7 prompt columns + 2 typed chars, 1-based: before the hint.
        assert_eq!(col, Some(10));
    }

    /// The whole point of the reserved flag: a reverse-search status survives
    /// a draft long enough to compete for every column on the row.
    #[test]
    fn compose_input_row_keeps_a_reserved_hint_against_a_competing_draft() {
        let draft = LiveDraft {
            prompt: "kitsu> ".to_string(),
            buffer: "a very long matched history entry indeed, long enough to window".to_string(),
            cursor: 63,
            hint: "  ⌕ reverse-search 'match'".to_string(),
            hint_reserved: true,
        };
        let (row, _) = compose_input_row(&draft, 64);
        let plain = sanitize_terminal_text(&row);
        assert!(
            plain.contains("⌕ reverse-search 'match'"),
            "the reserved status must survive a windowed draft: {plain:?}"
        );
        assert!(plain.contains('…'), "the draft yields columns: {plain:?}");
        assert!(display_width(&plain) <= 64, "row overflows: {plain:?}");
    }

    /// The inline guard has no column count of its own (it is handed only a
    /// row count), so the width probe is shared from here too rather than
    /// re-ioctl'd in `kitsu`. It must never be 0, or every draft would
    /// compose to an empty row.
    #[test]
    fn terminal_columns_is_never_zero() {
        assert!(terminal_columns() > 0);
    }
}
