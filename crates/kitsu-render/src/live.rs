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
use std::sync::atomic::{AtomicUsize, Ordering};
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
}

fn state() -> &'static Mutex<LiveRowState> {
    static STATE: OnceLock<Mutex<LiveRowState>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(LiveRowState::default()))
}

/// Nested transcript writers must not clear/repaint recursively; only the
/// outermost suspension owns the terminal row.
static SUSPEND_DEPTH: AtomicUsize = AtomicUsize::new(0);

fn live_rows_supported() -> bool {
    crate::stderr_styled() && !crate::plain_mode()
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
fn compose_row(
    spinner: Option<&str>,
    draft: Option<&LiveDraft>,
    queued: usize,
    cols: usize,
) -> (String, Option<usize>) {
    let spinner_width = spinner.map(|segment| display_width(&sanitize_terminal_text(segment)));
    let Some(draft) = draft else {
        return (spinner.unwrap_or_default().to_string(), None);
    };
    // The pending-stack tag sits between the spinner and the prompt; its
    // trailing space is part of its width.
    let queue_tag = (queued > 0).then(|| format!("[{queued} queued] "));
    let queue_tag_width = queue_tag.as_deref().map(display_width).unwrap_or(0);
    let prompt_width = display_width(&sanitize_terminal_text(&draft.prompt)) + queue_tag_width;
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
    let (visible, cursor_offset, windowed) =
        fit_draft_window(&draft.buffer, draft.cursor, buffer_avail);
    let mut row = String::new();
    if let Some((segment, _)) = spinner {
        row.push_str(segment);
        row.push_str("  ");
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

/// Compose the dimmed queued-prompt stack rows shown above the status row:
/// up to three upcoming prompts in execution order, then a `+N more` row.
/// Pure for unit testing. Rows are pre-truncated to `cols`.
fn compose_stack_rows(previews: &[String], queued: usize, cols: usize) -> Vec<String> {
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
    if row_state.spinner.is_none() && row_state.draft.is_none() {
        if row_state.drawn_rows > 0 {
            clear_region_locked(&mut err, row_state.drawn_rows);
            let _ = err.flush();
            row_state.drawn_rows = 0;
        }
        return;
    }
    let cols = terminal_cols();
    // A resize invalidates the painted stack rows (they hold the old
    // width); force the full-repaint path (sonnet review, cycle 9).
    if cols != row_state.last_cols {
        row_state.last_cols = cols;
        row_state.region_dirty = true;
    }
    // Stack rows render only alongside a draft, mirroring the queue tag.
    let stack = if row_state.draft.is_some() {
        compose_stack_rows(&row_state.previews, row_state.queued, cols)
    } else {
        Vec::new()
    };
    let (status, cursor_col) = compose_row(
        row_state.spinner.as_deref(),
        row_state.draft.as_ref(),
        row_state.queued,
        cols,
    );
    let wanted_rows = stack.len() + 1;
    if row_state.drawn_rows == wanted_rows && !row_state.region_dirty {
        // Fast path (spinner ticks, keystrokes): only the bottom status row
        // changed; upper stack rows stay put — no flicker.
        let _ = write!(err, "\r\x1b[2K{status}");
    } else {
        clear_region_locked(&mut err, row_state.drawn_rows);
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
        for row in &stack {
            let _ = write!(err, "{row}\r\n");
        }
        let _ = write!(err, "{status}");
        row_state.drawn_rows = wanted_rows;
        row_state.region_dirty = false;
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
        let (row, col) = compose_row(None, Some(&draft), 0, 80);
        assert_eq!(row, "kitsu> abc");
        // 7 prompt columns + 2 chars, 1-based.
        assert_eq!(col, Some(10));
    }

    #[test]
    fn compose_row_spinner_only_has_no_cursor_column() {
        let (row, col) = compose_row(Some("* thinking"), None, 0, 80);
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
        let (row, col) = compose_row(Some("( .a. ) kitsu is thinking"), Some(&draft), 0, 20);
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
        let (row, col) = compose_row(None, Some(&draft), 0, 80);
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
        let (row, _) = compose_row(None, Some(&mid_cursor), 0, 80);
        assert!(!row.contains("\u{1b}[2m"), "no hint mid-buffer: {row:?}");

        let no_room = LiveDraft {
            prompt: "> ".to_string(),
            buffer: "/status".to_string(),
            cursor: 7,
            hint: "extra".to_string(),
            hint_reserved: false,
        };
        // avail = 10 - 2 - 1 = 7: the buffer exactly fills it.
        let (row, _) = compose_row(None, Some(&no_room), 0, 10);
        assert!(
            !row.contains("extra"),
            "hint must never displace text: {row:?}"
        );
    }

    #[test]
    fn hint_respects_windowing_even_when_the_buffer_contains_a_literal_ellipsis() {
        // Windowed + literal `…` in the buffer: the hint must stay hidden
        // (the pre-fix heuristic sniffed `…` out of the visible text and
        // wrongly rendered a hint over trimmed content).
        let trimmed = LiveDraft {
            prompt: "> ".to_string(),
            buffer: "/que…something-long-enough-to-trim".to_string(),
            cursor: 34,
            hint: "ue".to_string(),
            hint_reserved: false,
        };
        let (row, _) = compose_row(None, Some(&trimmed), 0, 16);
        assert!(
            !row.contains("\u{1b}[2m"),
            "no hint on a windowed draft: {row:?}"
        );

        // Unwindowed + literal `…`: the hint still renders.
        let fits = LiveDraft {
            prompt: "> ".to_string(),
            buffer: "/que".to_string(),
            cursor: 4,
            hint: "ue".to_string(),
            hint_reserved: false,
        };
        let (row, _) = compose_row(None, Some(&fits), 0, 80);
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
        let (row, col) = compose_row(None, Some(&draft), 2, 80);
        assert!(sanitize_terminal_text(&row).starts_with("[2 queued] kitsu> abc"));
        assert!(row.contains("\u{1b}[2m"), "tag must be dimmed: {row:?}");
        // "[2 queued] " is 11 columns; 7 prompt + 2 chars + 1 (1-based).
        assert_eq!(col, Some(21));
        // Zero queued: no tag, original column.
        let (row, col) = compose_row(None, Some(&draft), 0, 80);
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
        let (row, _) = compose_row(None, Some(&draft), 0, 48);
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
        let (row, _) = compose_row(None, Some(&unreserved), 0, 48);
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
        let (row, _) = compose_row(Some("* thinking"), None, 3, 80);
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
        let (_, col) = compose_row(None, Some(&draft), 0, 80);
        assert_eq!(col, Some(8));
    }
}
