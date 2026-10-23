//! The bridge between the binary and `kitsu-tui`'s inline viewport (T0-S5a).
//!
//! This module is the single place where the preview renderer is switched on.
//! It does three things and deliberately nothing else: it decides the mode
//! from one environment value, it installs [`InlineSurface`][iv] into
//! `kitsu-render`'s process-wide surface slot, and it hands back a
//! session-lifetime guard that puts everything back.
//!
//! [iv]: kitsu_tui::viewport::InlineSurface
//!
//! # Finding F15: the viewport replaces the live status row
//!
//! `ClassicSurface::transcript_line` wraps every write in
//! `kitsu_render::live::with_live_row_suspended`, which clears a row at the
//! cursor and repaints it afterwards. The inline viewport suspends nothing,
//! because its rows are pinned *outside* the DECSTBM scroll region and the
//! transcript scrolls underneath them. Run both and the live row is painted
//! inside the scroll region, so it either scrolls away with the transcript or
//! fights the viewport's parked cursor for the same cells.
//!
//! The resolution is not to coordinate them but to retire one: the viewport's
//! bottom rows are exactly where the spinner, the draft and the status belong,
//! so painting a live row as well is duplication before it is a conflict. So
//! [`install`] disables the live row for as long as the viewport is installed
//! and [`BridgeGuard`]'s `Drop` restores whatever the setting was before.
//!
//! # Failing closed
//!
//! [`mode_from_value`] returns [`TuiMode::Inline`] for the exact value
//! `"inline"` and [`TuiMode::Classic`] for everything else — a missing value,
//! an empty one, an unknown word, a different case, a stray trailing space. A
//! typo must not silently move a user into a preview renderer, so the match is
//! byte-exact rather than forgiving.
//!
//! # Unix only
//!
//! The whole module is gated with an inner `#![cfg(unix)]` rather than a
//! `cfg(unix)` on each item. The viewport is only ever reached through a raw
//! terminal mode and a `TIOCGWINSZ` geometry probe, both of which are unix-only
//! in this workspace (`kitsu-render`'s own `terminal_cols` is already
//! `cfg(unix)`), so there is no item here that would be meaningful on its own
//! elsewhere. One attribute at the top gates every present and future item
//! uniformly; a per-item list drifts the moment someone adds an item and
//! forgets the attribute.

#![cfg(unix)]

use std::sync::Arc;

use crate::subagent::thread;
use kitsu_render::live;
use kitsu_render::surface::{set_surface, Surface};
use kitsu_tui::viewport::{
    clamp_viewport_rows, fit_viewport_rows, plan_resize, InlineSurface, ResizePlan,
};

/// Which renderer a session runs: the classic line renderer, or the inline
/// viewport preview.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TuiMode {
    /// The renderer kitsu has always had.
    Classic,
    /// The pinned inline viewport from `kitsu-tui`.
    Inline,
    /// The fullscreen transcript renderer: the terminal's alternate screen,
    /// painted from the cell ring inline mode retains. It pins no DECSTBM
    /// region and installs no surface, so [`install`] is inert for it and
    /// [`enter_fullscreen`] is what takes the terminal.
    Fullscreen,
}

impl TuiMode {
    /// The one spelling `/tui`, `--tui` and `[tui] mode` all use.
    pub(crate) fn label(self) -> &'static str {
        match self {
            TuiMode::Classic => "classic",
            TuiMode::Inline => "inline",
            TuiMode::Fullscreen => "fullscreen",
        }
    }
}

/// Whether an inline viewport is installed **right now**.
///
/// Process-global for the same reason `STATUS_PAINTER` is, plus one of its
/// own: `/tui`'s dispatch arm has to report the live renderer and cannot see
/// the `run_chat` local that holds the guard. Maintained by exactly two
/// writers — [`install`] on the way in and [`BridgeGuard::drop`] on the way
/// out — so a runtime switch keeps it true by construction rather than by a
/// caller remembering to update it.
static INLINE_INSTALLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Whether a fullscreen segment holds the terminal's alternate screen **right
/// now**.
///
/// The fullscreen counterpart of [`INLINE_INSTALLED`], and global for the same
/// reason: `/tui`'s dispatch arm reports the live renderer and cannot see the
/// `run_chat` local that holds the segment. Maintained by exactly two writers
/// — [`enter_fullscreen`] on the way in and [`FullscreenSegment::leave`] on
/// the way out — and the two flags are mutually exclusive by construction,
/// because every route into one renderer tears the other down first.
static FULLSCREEN_ACTIVE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The renderer this process is painting through at this instant.
pub(crate) fn live_mode() -> TuiMode {
    if FULLSCREEN_ACTIVE.load(std::sync::atomic::Ordering::Acquire) {
        TuiMode::Fullscreen
    } else if INLINE_INSTALLED.load(std::sync::atomic::Ordering::Acquire) {
        TuiMode::Inline
    } else {
        TuiMode::Classic
    }
}

/// A session-lifetime guard that puts back everything [`install`] displaced.
///
/// Both pieces of state are process-global, so the guard carries the
/// **previous** values rather than restoring defaults: the surface slot's
/// former occupant and the live-row setting as it was found.
pub(crate) struct BridgeGuard {
    /// The viewport and its latched geometry, kept so `Drop` can release the
    /// scroll region after the seam has been pointed back at the displaced
    /// surface.
    ///
    /// Behind an `Arc` because the geometry is shared with the process-global
    /// modal painter (card T3-S7a): an approval prompter runs with no guard
    /// in hand and must re-latch and paint through the *same* clamp this
    /// session's guard does, never a second copy of it.
    pane: Arc<PinnedPane>,
    /// The surface that was in the slot before the viewport went in.
    previous_surface: Arc<dyn Surface>,
    /// The live-row setting as it was found, which may itself be `false`.
    previous_live_rows: bool,
    /// Requirement **R5**'s live status row, once a session has one.
    ///
    /// `RefCell` because the guard is held by shared reference everywhere
    /// (exactly as [`rows`](Self::rows) is a `Cell`) and the session that owns
    /// the fields does not exist yet when `install` runs: the viewport is
    /// installed before the prompt queue the queue tag counts.
    live_status: std::cell::RefCell<Option<Arc<StatusSource>>>,
}

/// Install the inline viewport, or do nothing at all in classic mode.
///
/// The ordering here is load-bearing. `show` and `paint` run **before**
/// [`set_surface`], so the geometry bytes cannot have a transcript line
/// interleaved into them by a writer that reaches the seam in between.
pub(crate) fn install(
    mode: TuiMode,
    rows: u16,
    viewport_rows: u16,
    cell_ring_lines: usize,
    status: &str,
    out: Box<dyn std::io::Write + Send>,
    err: Box<dyn std::io::Write + Send>,
) -> Option<BridgeGuard> {
    match mode {
        // Inert: no bytes, no surface swap, and the live-row setting
        // untouched. Fullscreen shares the arm because it pins no region and
        // installs no surface of its own -- it takes the alternate screen
        // through `enter_fullscreen`, which taps the displaced surface and
        // leaves it printing the transcript onto that screen.
        TuiMode::Classic | TuiMode::Fullscreen => return None,
        TuiMode::Inline => {}
    }

    // Finding F19: the policy gate. The viewport needs a scrolling region of
    // at least two rows above it, because DECSTBM's bottom margin must *exceed*
    // its top margin of 1 -- `ESC[1;1r` is a one-row region real terminals
    // ignore, just like the `ESC[1;0r` a saturating `rows - viewport_rows` used
    // to produce. So `+ 2`, never `+ 1`: declining is the useful outcome here,
    // since the caller falls back to the classic renderer, whereas installing a
    // viewport the terminal silently discards would stand the classic live row
    // down and leave nothing pinned in its place.
    //
    // The refusal happens *before* anything is touched: no bytes on either
    // sink, the surface slot left as it was, and the live row left enabled.
    if rows < viewport_rows.saturating_add(2) {
        return None;
    }

    // Requirement N4: inline mode always carries a ring, bounded by the
    // configured line count. There is no off switch -- the bound's minimum is
    // one line -- so the newest line is always retained.
    let viewport = Arc::new(InlineSurface::with_cell_ring(out, err, cell_ring_lines));
    viewport.show(rows, viewport_rows);
    viewport.paint(&[status.to_string()], live::terminal_columns());

    // Finding F15: the viewport owns the bottom rows, so the live row stands
    // down for as long as it is installed.
    let previous_live_rows = live::set_live_rows_enabled(false);
    let previous_surface = set_surface(viewport.clone());

    let pane = Arc::new(PinnedPane {
        viewport,
        status: status.to_string(),
        rows: std::sync::atomic::AtomicU16::new(rows),
        viewport_rows: std::sync::atomic::AtomicU16::new(viewport_rows),
    });
    // Card T3-S7a: publish the pane for the approval modal, which paints from
    // a thread that holds no guard. Taken back by `Drop`, exactly as
    // `STATUS_PAINTER` is.
    *lock(&MODAL_PANE) = Some(pane.clone());
    INLINE_INSTALLED.store(true, std::sync::atomic::Ordering::Release);

    Some(BridgeGuard {
        pane,
        previous_surface,
        previous_live_rows,
        live_status: std::cell::RefCell::new(None),
    })
}

impl BridgeGuard {
    /// The lines the installed viewport's cell ring retains, oldest first, or
    /// `None` when the viewport was built without a ring.
    ///
    /// The one window onto requirement **N4**'s ring from outside
    /// `kitsu-tui`: the surface itself stays private to the pane, so this is
    /// how the fullscreen renderer — and these tests — read what the surface
    /// wrote.
    ///
    /// The production reader is the `/tui fullscreen` switch, which calls
    /// this **before** dropping the guard (the ring belongs to the surface the
    /// teardown puts back) and hands the lines to [`enter_fullscreen`].
    pub(crate) fn cell_lines(&self) -> Option<Vec<kitsu_tui::cells::CellLine>> {
        self.pane.viewport.cells()
    }
}

/// Take the terminal's alternate screen, and push the window title onto the
/// terminal's title stack. Requirement **N5**'s two clauses, as bytes.
///
/// The clear and the home cursor ride along with the enable, exactly as the
/// agent view's own enter does: the alternate screen a terminal hands back is
/// not guaranteed to be blank.
const ENTER_ALT_SCREEN: &[u8] = b"\x1b[?1049h\x1b[2J\x1b[H";
/// Give the alternate screen back. The restore is then the *terminal's* own:
/// the transcript the user had is still on the primary screen, untouched.
const LEAVE_ALT_SCREEN: &[u8] = b"\x1b[?1049l";
/// Push the window title (`CSI 22;2t`), and pop it (`CSI 23;2t`).
///
/// These are the same two sequences `kitsu_tui::TuiRuntime` writes, and they
/// are written here rather than by borrowing that runtime for one reason,
/// which is the same one `run_agent_view_overlay` states for the alternate
/// screen: `TuiRuntime::enter` also enables **crossterm's** raw mode, which
/// clears `OPOST`, and this binary's several hundred direct
/// `println!`/`eprintln!` writers would staircase down the screen. Raw mode
/// in this process belongs to `RawStdinGuard` and to nothing else, so a
/// fullscreen segment states the screen and the title and leaves termios
/// alone. Under tmux and GNU screen both sequences are silently ignored and a
/// title kitsu set would stay; an ignored sequence is inert rather than
/// broken.
const PUSH_TITLE: &[u8] = b"\x1b[22;2t";
/// See [`PUSH_TITLE`].
const POP_TITLE: &[u8] = b"\x1b[23;2t";

// ---------------------------------------------------------------------------
// Requirement **H2**'s title clause (card H2-title).
//
// `run_chat` resolves `[tui] title` once into the global below, calls
// `publish_state_title` at the three state-publishing boundaries, and the
// fullscreen segment consults `title_enabled` before it touches the title
// stack.
// ---------------------------------------------------------------------------

/// Whether `[tui] title` is honoured this run.
///
/// Process-global for the same reason [`INLINE_INSTALLED`] is: the title is a
/// property of the terminal this process holds, and the writers that set it
/// (the fullscreen segment, and the turn boundary in `run_chat`) hold no
/// common handle. Defaults to `true`, matching `TuiConfig::default`, so a
/// surface that never resolves the table behaves exactly as it does today.
static TITLE_ENABLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// Publish `[tui] title` for this run. Called once, where the rest of the
/// `[tui]` table is resolved.
pub(crate) fn set_title_enabled(on: bool) {
    TITLE_ENABLED.store(on, std::sync::atomic::Ordering::Release);
}

/// Whether the terminal title — the OSC 2 string *and* the title stack — is
/// this run's to touch.
pub(crate) fn title_enabled() -> bool {
    TITLE_ENABLED.load(std::sync::atomic::Ordering::Acquire)
}

/// Whether **this session** has already pushed the terminal's title stack.
///
/// Process-global for the same reason [`TITLE_ENABLED`] is: the session's
/// push and a fullscreen segment's push have no common handle, and the stack
/// they would both push is a property of the terminal this process holds.
///
/// Card **H2-stack**, finding **F140**: before this card `PUSH_TITLE` had one
/// production site, `enter_fullscreen`, so a title set in `classic` or
/// `inline` was never pushed and never popped and **outlived kitsu's exit on
/// every terminal** -- while the dying-signal restore wrote the pop
/// unconditionally, so an inline session popped a stack it never pushed. The
/// stack is therefore pushed **once per session**, by
/// [`TitleStackGuard::new`], and a fullscreen segment pushes only when the
/// session has not.
static TITLE_STACK_PUSHED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Whether the session-start push has happened. See [`TITLE_STACK_PUSHED`].
pub(crate) fn title_stack_pushed() -> bool {
    TITLE_STACK_PUSHED.load(std::sync::atomic::Ordering::Acquire)
}

/// The session's one push of the terminal title stack, popped when this is
/// dropped.
///
/// A guard rather than an enter/leave pair, for the reason [`BridgeGuard`]
/// and [`FullscreenSegment`] are guards: every exit path out of `run_chat`
/// drops it -- `/quit`, `Ctrl+D`, an error return, an unwinding panic -- and
/// a second teardown path is how one of them comes to differ from the other.
///
/// The gate is `title_enabled() && title_surface_allows_osc()`: **exactly**
/// the condition [`publish_state_title`] applies to the OSC 2 string itself.
/// A session that writes no title must push no stack, so the plain and
/// assistive surfaces and a non-TTY stderr keep today's byte stream
/// (requirement **N1**). Gating on the key alone would push one on the plain
/// classic surface, which is the mutation
/// `tui_title_stack_is_not_pushed_on_the_plain_or_assistive_classic_surfaces`
/// exists to catch.
///
/// The dying-signal restore's pop stays **unconditional** (`SIGNAL_RESTORE`
/// is byte-pinned): after this push it is correct for every session that set
/// a title, and for a session that set none it is a pop against an empty
/// stack, which no terminal distinguishes.
pub(crate) struct TitleStackGuard {
    /// Where the push went, so the pop goes to the same sink.
    err: Box<dyn std::io::Write + Send>,
    /// Whether the gate was open and this guard really pushed, so `Drop` pops
    /// exactly what was pushed and nothing else.
    pushed: bool,
}

impl TitleStackGuard {
    /// Push the title stack for this session, when the title is this run's to
    /// touch at all.
    pub(crate) fn new(err: Box<dyn std::io::Write + Send>) -> Self {
        let mut err = err;
        let pushed = title_enabled() && title_surface_allows_osc();
        if pushed {
            let _ = err.write_all(PUSH_TITLE);
            let _ = err.flush();
            TITLE_STACK_PUSHED.store(true, std::sync::atomic::Ordering::Release);
        }
        Self { err, pushed }
    }
}

impl Drop for TitleStackGuard {
    fn drop(&mut self) {
        if !self.pushed {
            return;
        }
        self.pushed = false;
        TITLE_STACK_PUSHED.store(false, std::sync::atomic::Ordering::Release);
        let _ = self.err.write_all(POP_TITLE);
        let _ = self.err.flush();
    }
}

/// The title requirement **H2** names, rendered.
///
/// `state` is [`crate::agent_state_file::process_state`]'s own word — this
/// function never recomputes it, so "blocked outranks working" has exactly
/// one home — and `agents` is the live agent count (`running + blocked`,
/// which that snapshot documents as disjoint).
///
/// The rendering H2 left open, decided here:
///
/// * **The count is always present**, `0 agents` included. A title a herder
///   parses is worth more predictable than short.
/// * **Singular at one**, `1 agent`, because the title is read by people too
///   and `(\d+) agents?` is no harder to parse than `(\d+) agents`.
/// * **Sanitised**, via `kitsu_tui::sanitize_title`, because the state word
///   and the count are the only parts that are kitsu's own today and the
///   boundary belongs here rather than at the next caller that is not.
pub(crate) fn state_title(state: &str, agents: usize) -> String {
    let unit = if agents == 1 { "agent" } else { "agents" };
    kitsu_tui::sanitize_title(&format!("kitsu \u{b7} {state} \u{b7} {agents} {unit}"))
}

/// Write the OSC 2 title onto `out`, or nothing at all when `enabled` is
/// false.
///
/// `enabled` is the *whole* decision — `[tui] title` and the surface gate
/// below — passed in rather than read, so this is testable against a buffer
/// and the gate has one evaluation point.
pub(crate) fn write_state_title(
    out: &mut dyn std::io::Write,
    enabled: bool,
    state: &str,
    agents: usize,
) {
    if !enabled {
        return;
    }
    let _ = write!(out, "\x1b]2;{}\x1b\\", state_title(state, agents));
    let _ = out.flush();
}

/// Whether this surface may receive the title's control bytes at all
/// (requirement **N1**, as F120/F122 settled it: per descriptor).
///
/// The same gate F122 shipped for its human line, and for its reason: the
/// title goes to **stderr**, so a non-terminal stderr must not receive it,
/// and `--plain`/`NO_COLOR`/`--screen-reader` all fold into `plain_mode`. A
/// terminal stdout is required as well, which is stricter than the minimum:
/// a run whose stdout is redirected is being consumed by something. JSONL is
/// unaffected by construction — its stream is stdout and the title is never
/// written there.
pub(crate) fn title_surface_allows_osc() -> bool {
    kitsu_render::stderr_styled() && kitsu_render::is_tty()
}

/// Set the terminal title from the state the state file publishes, on the one
/// sink the title's other bytes already go to.
pub(crate) fn publish_state_title(state: &str, agents: usize) {
    let enabled = title_enabled() && title_surface_allows_osc();
    write_state_title(&mut std::io::stderr(), enabled, state, agents);
}

/// The idle ambient cadence's title write: card **H2-cadence**, the last
/// recorded deviation from requirement **H2**'s cadence clause.
///
/// The three `run_chat` publish sites are the session boundary, the top of
/// the idle loop and the start of a turn, so the title is correct at the
/// prompt and at turn start and **goes stale during a turn**. The registry's
/// `watch` channel already has an ungated, session-long subscriber --
/// [`spawn_idle_ambient_cadence`], which dedupes on `(blocked, running)`,
/// *precisely the title's own count inputs*, and has no turn gate, so it
/// fires mid-turn -- and this is the function it publishes through.
///
/// **Why the counts are not the whole story.** Mid-turn `turn_active` is
/// `true`, so the word is `blocked > 0 ? "blocked" : "working"`. The one
/// transition the three sites cannot cover and the counts alone cannot
/// render is an agent **finishing** mid-turn into `(0, 0)`: the true word is
/// `"working"` and a cadence that assumed `turn_active = false` would write
/// `"idle"`. So the flag is plumbed rather than guessed, and the word still
/// comes from [`crate::agent_state_file::process_state`] -- never a second
/// computation of the precedence.
///
/// `enabled` is the whole gate, passed in exactly as [`write_state_title`]
/// takes it, so this is testable against a buffer.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn write_cadence_state_title(
    out: &mut dyn std::io::Write,
    enabled: bool,
    blocked: usize,
    running: usize,
    turn_active: bool,
) {
    // Through `cadence_title_parts` so the word and the count keep one
    // derivation with the production publish point.
    let (state, agents) = cadence_title_parts(blocked, running, turn_active);
    write_state_title(out, enabled, state, agents);
}

/// The word and the count a cadence wake's `(blocked, running)` pair renders
/// to: the state word from [`crate::agent_state_file::process_state`], so the
/// precedence *"blocked outranks working"* keeps its single home, and the
/// live count `blocked + running`, the two tallies that snapshot documents as
/// disjoint.
///
/// Extracted so the buffer-testable writer above and the production publish
/// point below share **one** derivation: the mismatch F141 is about -- a
/// second path that a test cannot see -- has nowhere to appear.
pub(crate) fn cadence_title_parts(
    blocked: usize,
    running: usize,
    turn_active: bool,
) -> (&'static str, usize) {
    (
        crate::agent_state_file::process_state(running, blocked, turn_active),
        blocked + running,
    )
}

/// Publish the cadence's title on the one sink the title's other bytes go to.
///
/// **Beside the repaint, not inside it.** [`repaint_status_row`] is inert in
/// classic mode and in any session that never enabled a live status row --
/// *"what keeps the classic byte stream identical"* -- so a title riding
/// inside it would inherit that inertness and a styled `classic` session
/// would still go stale. The title goes straight to stderr, under the same
/// surface gate as the other three sites.
pub(crate) fn publish_cadence_state_title(blocked: usize, running: usize, turn_active: bool) {
    let (state, agents) = cadence_title_parts(blocked, running, turn_active);
    publish_state_title(state, agents);
}

/// A live fullscreen segment: the alternate screen and the pushed title, given
/// back when this is dropped.
///
/// A guard rather than an enter/leave pair of calls, for the reason
/// [`BridgeGuard`] is one: `/tui` swaps renderers by dropping the outgoing
/// one, every exit path out of `run_chat` drops it too, and a second teardown
/// path is how one of them comes to differ from the other.
pub(crate) struct FullscreenSegment {
    /// The sink the enter bytes went to, kept so the leave bytes go to the
    /// same one.
    err: Box<dyn std::io::Write + Send>,
    /// Whether the terminal has already been given back, so the restore bytes
    /// are written at most once per segment.
    left: bool,
    /// The lines the entry frame was painted from, oldest first: the outgoing
    /// inline viewport's ring, or empty out of classic and at startup.
    ///
    /// Kept because the ring they came from belongs to the surface the inline
    /// teardown has already put back, so there is nowhere else to read them
    /// from once the segment is live.
    seed: Vec<kitsu_tui::cells::CellLine>,
    /// The seam tap, which records what the session writes onto the alternate
    /// screen so the frame can be painted again from it.
    tap: Arc<FullscreenTap>,
    /// The surface the tap displaced, put back by [`Self::leave`].
    previous_surface: Arc<dyn Surface>,
    /// How many of the tap's lines the last frame was painted from, so a
    /// prompt that follows no new output repaints nothing and the byte stream
    /// of an idle fullscreen session is the one it was before this card.
    painted_tap_lines: usize,
    /// Whether *this segment* pushed the title stack, and therefore owes a
    /// pop (card **H2-stack**). False when `[tui] title = false`, and false
    /// when the session already pushed its own at start -- the counts are one
    /// push and one pop per session either way.
    pushed_title: bool,
    /// Where the frame is anchored: the paging state, and the only state the
    /// four paging keys add.
    ///
    /// Findings **F94/F96/F98**: the paths into a state are enumerable, and
    /// this one lives here because this type's single [`Drop`] is already the
    /// one clearer on every path out of fullscreen -- a `/tui` swap, a
    /// `run_chat` return, a panic. An anchor kept anywhere else would be a
    /// second thing to put back, and a second teardown path is how the two
    /// come to differ.
    view: kitsu_tui::fullscreen::FullscreenView,
}

/// The seam tap a live fullscreen segment installs: every transcript line the
/// session writes while the alternate screen is held is forwarded verbatim to
/// the surface fullscreen displaced, and a copy is kept in a line-bounded
/// ring so the frame can be painted again from it (requirement **N4**'s bound,
/// and **F52**'s: lines, never bytes).
///
/// The tap **only records.** The repaint is driven by
/// [`FullscreenSegment::repaint_if_the_ring_grew`] at the idle prompt, never
/// from inside a write: a repaint at the seam would reach the inline and
/// classic renderers as well, whose contracts are one write per transcript
/// line (**R3**) and a byte stream identical to the unrouted writers'.
struct FullscreenTap {
    /// The surface fullscreen displaced. It still owns every byte: this type
    /// adds no output of its own.
    inner: Arc<dyn Surface>,
    /// What the session has written since the segment took the screen, oldest
    /// first.
    ring: std::sync::Mutex<kitsu_tui::cells::CellRing>,
}

impl FullscreenTap {
    /// The recorded lines, oldest first.
    fn lines(&self) -> Vec<kitsu_tui::cells::CellLine> {
        lock(&self.ring).lines()
    }
}

impl Surface for FullscreenTap {
    /// Forward first, record after: the bytes reach the terminal before any
    /// bookkeeping, exactly as `InlineSurface` orders the same pair, and the
    /// recorded order is the written order by construction.
    fn transcript_line(&self, fd: kitsu_render::surface::Fd, text: &str) {
        self.inner.transcript_line(fd, text);
        lock(&self.ring).push(fd, text);
    }

    /// The block runs against the displaced surface's own writer, so its
    /// single-suspension property and its descriptor are the displaced
    /// surface's: this override exists to *see* the bytes, not to buffer them.
    ///
    /// A block whose bytes end without a newline is recorded as a short line,
    /// for the reason `InlineSurface::write_block` states: text that is on the
    /// terminal and absent from the ring would be lost from the frame for
    /// good.
    fn write_block(
        &self,
        fd: kitsu_render::surface::Fd,
        block: &mut dyn FnMut(&mut dyn std::io::Write),
    ) {
        let mut seen: Vec<u8> = Vec::new();
        self.inner.write_block(fd, &mut |writer| {
            let mut tee = Tee {
                inner: writer,
                seen: &mut seen,
            };
            block(&mut tee);
        });
        let text = String::from_utf8_lossy(&seen);
        let mut ring = lock(&self.ring);
        for line in text.lines() {
            ring.push(fd, line);
        }
    }

    /// Forwarded whole: the queued-prompt rows belong to a backend with pinned
    /// bottom rows, and a fullscreen segment pins none, so this is the
    /// displaced surface's answer and never a ring entry.
    fn queue(&self, rows: &[String]) {
        self.inner.queue(rows);
    }

    /// **Recorded, never forwarded** -- the one method on this tap that does
    /// not reach the displaced surface.
    ///
    /// The tap's ring is the *only* ring while a fullscreen segment holds the
    /// screen (`install` is inert in that mode, so there is no inline surface
    /// behind it), so the prompt must land here or the overlay shows one
    /// speaker in fullscreen and two inline -- a key that quietly does nothing
    /// on one surface (**F84**/**F89**). Forwarding would hand the prompt to
    /// the displaced inline surface's ring, which no reader can see from here,
    /// and nothing is written either way: `note_prompt` emits no bytes.
    fn note_prompt(&self, text: &str) {
        lock(&self.ring).push_prompt(text);
    }
}

/// A writer that copies what it forwards, for [`FullscreenTap::write_block`].
struct Tee<'a> {
    inner: &'a mut dyn std::io::Write,
    seen: &'a mut Vec<u8>,
}

impl std::io::Write for Tee<'_> {
    /// Only the bytes the inner writer accepted are recorded, so a short write
    /// cannot put a line in the ring that never reached the terminal.
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let written = self.inner.write(buf)?;
        self.seen.extend_from_slice(&buf[..written]);
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

impl FullscreenSegment {
    /// Paint the frame again when the session has written something since the
    /// last one, and write nothing at all when it has not (**F97**).
    ///
    /// Called from the idle prompt, which is the one place in a fullscreen
    /// session where no turn is streaming and the screen is therefore not
    /// being written by anything else. The frame is the seed lines the entry
    /// paint used followed by everything the tap recorded, so the line that
    /// was retained from the inline segment stays above the ones the
    /// fullscreen segment added.
    ///
    /// The gate is what keeps an idle session's byte stream unchanged: a
    /// prompt that follows no output repaints nothing, so the frame is written
    /// once per growth of the ring rather than once per prompt.
    pub(crate) fn repaint_if_the_ring_grew(&mut self, rows: u16, cols: usize) {
        if self.left {
            return;
        }
        let recorded = self.tap.lines();
        if recorded.len() <= self.painted_tap_lines {
            return;
        }
        self.painted_tap_lines = recorded.len();
        // From the segment's **own** anchor, not a fresh one: a view
        // constructed per paint would un-pin the frame on the next line the
        // session writes, which is paging that lasts until the next output.
        let frame = {
            let ring = frame_ring(&self.lines_now());
            self.view.frame(&ring, window_rows(rows), cols)
        };
        self.paint_frame(&frame);
    }

    /// The lines one frame is painted from: the seed the entry paint used,
    /// then everything the tap has recorded since, oldest first.
    fn lines_now(&self) -> Vec<kitsu_tui::cells::CellLine> {
        let mut lines = self.seed.clone();
        lines.extend(self.tap.lines());
        lines
    }

    /// Write one frame onto the screen the segment already holds.
    ///
    /// The clear and the home cursor are the entry paint's, minus the
    /// alternate-screen enable: this is a repaint of a screen already held,
    /// so re-enabling it would be a second `ESC[?1049h` the leave has no
    /// counterpart for.
    fn paint_frame(&mut self, frame: &[String]) {
        let _ = self.err.write_all(b"\x1b[2J\x1b[H");
        for row in frame {
            let _ = self.err.write_all(row.as_bytes());
            let _ = self.err.write_all(b"\r\n");
        }
        let _ = self.err.flush();
    }

    /// The lines the frame is painted from, oldest first: the seed plus
    /// everything the tap has recorded.
    ///
    /// Requirement **R6**: the transcript overlay opens over the same lines
    /// this segment's frame is painted from, and reads them through this
    /// rather than through a ring of its own.
    pub(crate) fn transcript_lines(&self) -> Vec<kitsu_tui::cells::CellLine> {
        self.lines_now()
    }

    /// Paint the frame again from this segment's **own** anchor, whatever the
    /// ring has done.
    ///
    /// Requirement **R6**'s close: the transcript overlay painted over this
    /// screen and scrolled its own anchor, so `q` must put back the frame the
    /// user left -- which is this segment's anchor, untouched by the overlay's
    /// separate instance. Unlike [`Self::repaint_if_the_ring_grew`] this is
    /// unconditional, because nothing about the ring changed and the screen is
    /// nevertheless not the one the segment painted.
    pub(crate) fn repaint_frame(&mut self, rows: u16, cols: usize) {
        if self.left {
            return;
        }
        let frame = {
            let ring = frame_ring(&self.lines_now());
            self.view.frame(&ring, window_rows(rows), cols)
        };
        self.paint_frame(&frame);
    }

    /// Whether this segment's frame is still following the newest line.
    ///
    /// The anchor's own transitions are [`kitsu_tui::fullscreen`]'s and are
    /// pinned there; what this exposes is which segment the anchor belongs
    /// to.
    // Production's caller arrives with the sink this card leaves unwired.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn is_following(&self) -> bool {
        self.view.is_following()
    }

    /// Apply one paging press to this segment's anchor and repaint the frame
    /// from it.
    ///
    /// The repaint is **the press's own**, and that is the whole reason this
    /// method writes bytes: the frame follows the ring only when the ring
    /// *grew* ([`Self::repaint_if_the_ring_grew`]) and the reader paints only
    /// the composer row, so a press that left the painting to someone else
    /// would move the anchor and leave the screen exactly as it was -- a dead
    /// key with state behind it.
    ///
    /// `rows`/`cols` are the caller's geometry probe, taken per press for
    /// [`Self::paint_composer`]'s reason: the bottom row is the composer's, so
    /// the frame's window is one row shorter than the screen, exactly as the
    /// entry paint's is.
    pub(crate) fn page(&mut self, press: kitsu_tui::fullscreen::Page, rows: u16, cols: usize) {
        if self.left {
            return;
        }
        let frame = {
            let ring = frame_ring(&self.lines_now());
            self.view.page(press, &ring, window_rows(rows));
            self.view.frame(&ring, window_rows(rows), cols)
        };
        self.paint_frame(&frame);
    }

    /// Paint the composer onto the **bottom row** of the alternate screen,
    /// at an absolute address and without a line feed.
    ///
    /// This is fullscreen's own composer row, and it is the counterpart of
    /// `BridgeGuard::paint_composer` rather than a second renderer: the row
    /// itself is composed by `kitsu_render::live::compose_input_row`, the
    /// same function the inline composer and the classic live row compose
    /// theirs with, so the three cannot drift in what a draft looks like.
    ///
    /// # Why an absolute address and no newline
    ///
    /// A fullscreen segment pins no DECSTBM region -- the frame fills the
    /// screen and the cursor is parked on the one row the frame held back
    /// (see [`enter_fullscreen`]). So the one thing this paint must never do
    /// is **scroll**: every row it scrolls off the top is a frame row the
    /// user loses for good, which is finding **F97**'s second half as
    /// measured on a pty (a cooked-mode editor's palette cost eleven of
    /// them, and backspacing did not bring them back). Addressing the bottom
    /// row, erasing it and writing the row with no trailing `\n` leaves
    /// every other cell of the screen exactly as the frame left it, so the
    /// frame survives being typed at by construction rather than by being
    /// repainted afterwards.
    ///
    /// `rows` and `cols` are the caller's geometry probe, taken per paint, so
    /// a `SIGWINCH` that wakes the reader lands the next row on the new
    /// bottom row without this type latching a geometry of its own.
    ///
    /// The caret is left in the draft, which is what makes the row a composer
    /// and not a status line: `compose_input_row` returns the column it
    /// belongs in, windowed with the draft.
    ///
    /// The draft arrives as the `LiveDraft` the renderer's own composition
    /// takes, rather than as its five fields: this method's business is
    /// *where* the row goes, and the row's content is the shared value type
    /// the inline composer and the classic live row are already built from.
    ///
    /// There is no slash palette on this surface. A palette is several rows
    /// tall and the only space for it is the frame's, and the frame is what
    /// this card exists to stop losing; the one-row composer is what the
    /// alternate screen has room for.
    pub(crate) fn paint_composer(&mut self, draft: &live::LiveDraft, rows: u16, cols: usize) {
        if self.left {
            return;
        }
        let (row, caret) = live::compose_input_row(draft, cols);
        let bottom = rows.max(1);
        let _ = self
            .err
            .write_all(format!("\x1b[{bottom};1H\x1b[2K").as_bytes());
        let _ = self.err.write_all(row.as_bytes());
        // Columns are 1-based on the wire; `caret` is a 0-based display
        // column within the row, and a row with no caret parks at the start.
        let column = caret.unwrap_or(0).saturating_add(1);
        let _ = self
            .err
            .write_all(format!("\x1b[{bottom};{column}H").as_bytes());
        let _ = self.err.flush();
    }

    /// Take the composer row back once the read is over, leaving the cursor at
    /// the start of it.
    ///
    /// The mirror of [`paint_composer`](Self::paint_composer), and the reason
    /// it exists is the cursor rather than the row: a submitted line is
    /// followed by the turn's own output, written at wherever the cursor was
    /// left, and a caret parked in the middle of a drafted row would start
    /// the transcript there. Erasing the row and homing the column puts the
    /// screen back exactly as the frame's last `\r\n` left it, which is where
    /// every writer in this process expects to continue.
    pub(crate) fn clear_composer(&mut self, rows: u16) {
        if self.left {
            return;
        }
        let bottom = rows.max(1);
        let _ = self
            .err
            .write_all(format!("\x1b[{bottom};1H\x1b[2K").as_bytes());
        let _ = self.err.flush();
    }

    /// Give the terminal back: the alternate screen first, then the title.
    ///
    /// Idempotent, and every I/O result is dropped: this is the body of
    /// [`Drop`], there is nothing useful to do about a terminal that will not
    /// take its own escape sequences, and the only place left to report it to
    /// is that terminal.
    fn leave(&mut self) {
        if self.left {
            return;
        }
        self.left = true;
        // Before the bytes, so `/tui`'s report cannot observe a renderer the
        // terminal has already been taken back from.
        FULLSCREEN_ACTIVE.store(false, std::sync::atomic::Ordering::Release);
        // And before them for a second reason: the next transcript line must
        // not be recorded into a frame that no longer has a screen. The
        // put-back is conditional on the tap still being the installed
        // surface, because the switch into inline installs its viewport
        // *after* dropping this segment on one path and could not be relied
        // on to stay in that order; an unconditional `set_surface` here would
        // then displace the incoming renderer.
        if Arc::ptr_eq(
            &kitsu_render::surface::active_surface(),
            &(self.tap.clone() as Arc<dyn Surface>),
        ) {
            set_surface(self.previous_surface.clone());
        }
        let _ = self.err.write_all(LEAVE_ALT_SCREEN);
        // The mirror of `enter_fullscreen`'s gate (cards H2-title and
        // H2-stack): a segment pops exactly what *it* pushed. A run with
        // `title = false` pushed none, and a session that pushed its own at
        // start owns that one -- popping it here would underflow the stack by
        // one per segment.
        if self.pushed_title {
            let _ = self.err.write_all(POP_TITLE);
        }
        let _ = self.err.flush();
        // Card F94, and the mirror of `enter_fullscreen`'s store: after the
        // bytes, so no exit path can read "free" while the screen is still
        // held. A signal in this window writes a second leave onto a primary
        // screen; a signal in the opposite window would write none at all.
        kitsu_tui::note_alternate_screen(false);
    }
}

impl Drop for FullscreenSegment {
    fn drop(&mut self) {
        self.leave();
    }
}

/// Enter fullscreen: take the alternate screen and paint the **tail** of
/// `lines` into it through `kitsu_tui::fullscreen::FullscreenView`.
///
/// `lines` is the inline viewport's cell ring as [`BridgeGuard::cell_lines`]
/// returned it, oldest first, and empty when the outgoing renderer was
/// classic — classic feeds no ring, so a switch out of it has nothing to
/// repaint and paints nothing. That is the honest frame for an empty ring
/// rather than a reason to conjure one.
///
/// The ring is rebuilt here over the copy `cell_lines` already handed out,
/// because the ring itself belongs to the surface the teardown has just put
/// back. Its limit is the line count, so nothing is evicted and the view's
/// follow anchor resolves to the newest `rows - 1` lines.
///
/// One row is held back from the window on purpose: the switch receipt and
/// the prompt are written to the same screen straight afterwards, so a frame
/// that filled every row would scroll its own oldest line off before the user
/// saw it. Paging back through the rest of the ring needs a key decoder and a
/// view that outlives the paint; this segment shows the tail, which is
/// strictly more than the classic renderer shows of a transcript that has
/// scrolled away.
///
/// Finding **F97**: the entry frame is not the only frame. The segment
/// installs a [`FullscreenTap`] over the surface fullscreen displaced, so
/// what the session writes onto the alternate screen is recorded as well as
/// printed, and [`FullscreenSegment::repaint_if_the_ring_grew`] paints the
/// seed lines and the recorded ones again at the idle prompt. The frame
/// therefore follows the ring instead of freezing at the moment of the
/// switch.
///
/// The rows are written as lines rather than at absolute addresses, with this
/// backend's own CRLF: the cursor is then left under the frame, where the
/// receipt and everything the session prints next belong.
pub(crate) fn enter_fullscreen(
    err: Box<dyn std::io::Write + Send>,
    lines: &[kitsu_tui::cells::CellLine],
    rows: u16,
    cols: usize,
    ring_lines: usize,
) -> FullscreenSegment {
    let frame = fullscreen_frame(lines, rows, cols);

    let mut err = err;
    // Card F94: **before** the bytes, and the opposite order to
    // `FULLSCREEN_ACTIVE` below on purpose. This flag is read by the exit
    // paths `Drop` does not run on -- the dying-signal handler and the panic
    // hook -- so a signal landing between the store and the write must find
    // the screen claimed rather than free: a restore of a screen not yet
    // taken costs one `ESC[?1049l` on the primary screen, while the reverse
    // window leaves the user on the alternate screen, which is the defect
    // itself.
    kitsu_tui::note_alternate_screen(true);
    // Card H2-title: `[tui] title = false` owns the title *stack* as well as
    // the OSC 2 string (**F139** measured this half as ungated). The surface
    // gate is deliberately not consulted here: these two sequences reach the
    // plain and assistive surfaces today and requirement N1 is satisfied by
    // leaving them byte-identical.
    //
    // Card **H2-stack**: and only when the *session* has not pushed already.
    // One push and one pop per session is not a free choice -- a styled
    // fullscreen session's counts are pinned at exactly one each by
    // `tui_switch_from_fullscreen_into_classic_restores_the_screen_and_the_title`.
    let pushed_title = title_enabled() && !title_stack_pushed();
    if pushed_title {
        let _ = err.write_all(PUSH_TITLE);
    }
    let _ = err.write_all(ENTER_ALT_SCREEN);
    for row in &frame {
        let _ = err.write_all(row.as_bytes());
        let _ = err.write_all(b"\r\n");
    }
    let _ = err.flush();
    // After the bytes: the terminal is held by the time anything can report
    // that it is.
    FULLSCREEN_ACTIVE.store(true, std::sync::atomic::Ordering::Release);
    // Last, and after the entry frame's bytes: the tap records what the
    // session writes *onto the alternate screen*, so installing it before the
    // frame would put the frame's own rows in the ring if a writer reached the
    // seam in between.
    //
    // The bound is the caller's configured cell-ring line count, the same one
    // `install` hands the inline viewport, and it is a count of lines --
    // **F52**'s bound is never bytes.
    let tap = Arc::new(FullscreenTap {
        inner: kitsu_render::surface::active_surface(),
        ring: std::sync::Mutex::new(kitsu_tui::cells::CellRing::new(ring_lines.max(1))),
    });
    let previous_surface = set_surface(tap.clone());
    FullscreenSegment {
        err,
        left: false,
        seed: lines.to_vec(),
        tap,
        previous_surface,
        painted_tap_lines: 0,
        pushed_title,
        view: kitsu_tui::fullscreen::FullscreenView::new(),
    }
}

/// Whose words a submitted line is.
///
/// A transcript is a document, and a document is worth what its claims are
/// worth (**F77**): `/resume` and `/retry` are **kitsu's** words, injected by
/// `run_chat` as `priority_command`, and marking them the user's would be a
/// false claim inside the one record the overlay reads. The distinction is a
/// parameter rather than a guess at the text because it is not recoverable
/// from the line -- the user can type `/retry` themselves, and that one *is*
/// theirs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PromptOrigin {
    /// The user typed it: at the idle prompt, during a turn (queued), or at an
    /// attached agent.
    User,
    /// Kitsu synthesized it. Never marked the user's.
    Injected,
}

/// Hand the **installed** surface the user's own submitted prompt, so a
/// backend that keeps cells can retain it (**F102**, requirement **N4**).
///
/// # Why this is the one feed point
///
/// The three idle readers -- the inline composer, the fullscreen composer and
/// rustyline (plus the assistive line reader) -- all return through
/// `ChatReadline`, and a queued prompt and a priority command join the same
/// `line` binding in `run_chat`. One call *there*, after the empty-line skip
/// and beside `add_history_entry`, is reached by every route a submitted line
/// can take, and by nothing else: `run_scripted_turn` and the one-shot `-p`
/// route build a `SubmittedPrompt` without a terminal and have no ring.
///
/// # Ordering is structural, not hoped for
///
/// The call sits on the single-threaded path between the read and whatever the
/// line provokes -- a slash command's report, a turn's streamed output -- so
/// the prompt is in the ring before the first byte of its answer can be
/// written. It is not sequenced against the stream; it precedes it.
///
/// Writes no bytes: see [`Surface::note_prompt`].
pub(crate) fn note_submitted_prompt(text: &str, origin: PromptOrigin) {
    match origin {
        PromptOrigin::User => kitsu_render::surface::active_surface().note_prompt(text),
        // Not handed over at all, which is the strongest reading of "never
        // marked the user's": there is no byte-free seam for retaining an
        // unmarked line, and inventing one to file kitsu's own synthesized
        // command beside its output would be a second decision this card did
        // not make (**F61**). The command's *output* is retained as it always
        // was, through `transcript_line`. The exclusion has one home -- here --
        // rather than being folded into the call sites, so there is one place
        // to change if the ring should one day carry injected lines unmarked.
        PromptOrigin::Injected => {}
    }
}

/// The rows of one fullscreen frame over `lines`, oldest line first.
///
/// The ring is rebuilt over the caller's copy because the ring the seed lines
/// came from belongs to the surface the inline teardown already put back. Its
/// limit is the line count, so nothing is evicted and the view's follow anchor
/// resolves to the newest `rows - 1` lines.
fn fullscreen_frame(lines: &[kitsu_tui::cells::CellLine], rows: u16, cols: usize) -> Vec<String> {
    let ring = frame_ring(lines);
    kitsu_tui::fullscreen::FullscreenView::new().frame(&ring, window_rows(rows), cols)
}

/// The ring one frame is read through, rebuilt over the caller's copy of the
/// lines for [`fullscreen_frame`]'s reason.
///
/// The limit is the line count, so nothing is evicted and a paging anchor is
/// an ordinal into exactly the lines the segment holds.
pub(crate) fn frame_ring(lines: &[kitsu_tui::cells::CellLine]) -> kitsu_tui::cells::CellRing {
    let mut ring = kitsu_tui::cells::CellRing::new(lines.len().max(1));
    for line in lines {
        // `push_retained`, never `push(fd, styled)`: this rebuild is the
        // chokepoint *both* ring readers go through -- the fullscreen frame
        // and the `Ctrl+O` overlay -- so a prompt mark dropped here is lost to
        // every reader however faithfully the feed stored it (**F79**).
        ring.push_retained(line);
    }
    ring
}

/// The frame's window: the screen minus the composer's bottom row.
///
/// One row is the composer's on every paint -- the entry frame's, the
/// ring-grew repaint's and a page press's -- so the subtraction lives in one
/// place rather than at each of the three.
fn window_rows(rows: u16) -> u16 {
    rows.saturating_sub(1)
}

/// The pinned pane: the inline viewport plus the geometry currently latched
/// on it -- the terminal height and the pinned row count [`install`] was
/// given.
///
/// Finding F21: the geometry is carried here because `InlineSurface::hide`
/// *takes* the latched `(bottom, viewport_rows)`. A release therefore leaves
/// the surface with nothing to pin, so the reinstall must supply the geometry
/// again from here -- and it has to be durable rather than consumed, because
/// a session can suspend more than once.
///
/// Finding F16: a resize updates both in place, so every method here takes
/// `&self` -- each caller holds the pane by shared reference, and a resize
/// that could not update the stored geometry would make the next suspend
/// reinstall the pre-resize one.
///
/// They are `AtomicU16` rather than `Cell` for one reason: `BridgeGuard` is a
/// `run_chat` local that is neither `Send` nor `Sync`, so a pane the
/// process-global modal painter can reach must carry its geometry in a form
/// that crosses threads.
pub(crate) struct PinnedPane {
    viewport: Arc<InlineSurface>,
    /// The status text [`install`] painted: what the single pinned row falls
    /// back to when no session has published live fields, and so what the
    /// approval modal's teardown restores in that case.
    status: String,
    rows: std::sync::atomic::AtomicU16,
    viewport_rows: std::sync::atomic::AtomicU16,
}

/// Requirement **R5**'s status row as *live state*, re-read on every paint
/// rather than captured once (card T3-S5 left the row composed but
/// unreachable because [`install`]'s `status` was a `&str`).
///
/// Three kinds of field, separated because they change at three cadences:
///
/// * `usage` -- the model id and the context meter's numerator and
///   denominator. These change at turn boundaries, where the one place that
///   owns the message history recomputes them (`session_context_used`), so
///   they are *pushed* in.
/// * `turn_start` -- the instant R5's elapsed segment counts from, `None`
///   between turns so the idle row carries no elapsed at all.
/// * `counts` -- the queue depth and the detached-agent counts, which are
///   **pulled** through a closure instead of pushed. The queue is drained by
///   the main loop *between* turns, so a stored depth is stale exactly when
///   the idle row is painted; a closure over the queue and the registry can
///   only ever report what is true at paint time.
pub(crate) struct StatusSource {
    usage: std::sync::Mutex<StatusUsage>,
    turn_start: std::sync::Mutex<Option<std::time::Instant>>,
    counts: Box<dyn Fn() -> StatusCounts + Send + Sync>,
    /// The row the cadence painter last put on screen, so an unchanged row
    /// costs no bytes -- the same "no-op when nothing changed" rule
    /// `live_agents_state` applies, and what keeps a 50 ms tick from emitting
    /// twenty identical paints a second into the transcript's stream.
    painted: std::sync::Mutex<Option<String>>,
    /// When the cadence painter last actually wrote the row, for note
    /// **N3**'s frame budget. `None` until the first paint, so the first
    /// frame of a session is never made to wait.
    last_paint: std::sync::Mutex<Option<std::time::Instant>>,
}

/// The model and meter numbers, pushed at turn boundaries.
#[derive(Clone, Debug, Default)]
struct StatusUsage {
    model: String,
    used: u64,
    effective: u64,
}

/// What the row's two optional tags count, as of paint time.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct StatusCounts {
    pub(crate) queued: usize,
    pub(crate) blocked: usize,
    pub(crate) running: usize,
    /// The agent the composer is attached to, read off the session binding at
    /// paint time -- a pull, like the two counts above, because the binding
    /// moves between paints and a value copied in would be stale exactly when
    /// the user looks (card **T4-S6-status**).
    pub(crate) attached: Option<u64>,
}

impl StatusSource {
    /// A source whose tags are read through `counts` on every paint.
    pub(crate) fn new(counts: Box<dyn Fn() -> StatusCounts + Send + Sync>) -> Arc<Self> {
        Arc::new(Self {
            usage: std::sync::Mutex::new(StatusUsage::default()),
            turn_start: std::sync::Mutex::new(None),
            counts,
            painted: std::sync::Mutex::new(None),
            last_paint: std::sync::Mutex::new(None),
        })
    }

    /// Record the live model and the context meter's numbers.
    pub(crate) fn set_usage(&self, model: &str, used: u64, effective: u64) {
        *lock(&self.usage) = StatusUsage {
            model: model.to_string(),
            used,
            effective,
        };
    }

    /// Start R5's elapsed segment: a turn is now running.
    pub(crate) fn begin_turn(&self) {
        *lock(&self.turn_start) = Some(std::time::Instant::now());
    }

    /// Stop it. Between turns `elapsed` is `None`, so the idle row has
    /// nothing to tick and needs no cadence of its own.
    pub(crate) fn end_turn(&self) {
        *lock(&self.turn_start) = None;
    }

    /// The composer's input, as of now.
    fn fields(&self) -> live::StatusFields {
        let usage = lock(&self.usage).clone();
        let counts = (self.counts)();
        live::StatusFields {
            model: usage.model,
            used: usage.used,
            effective: usage.effective,
            elapsed: lock(&self.turn_start).map(|start| start.elapsed()),
            queued: counts.queued,
            blocked: counts.blocked,
            running: counts.running,
            // The attachment is pulled with the counts, so the row the
            // painter composes on the cadence tick is derived from the
            // composer's session binding as it stands at paint time -- never
            // a copy taken when the source was built (card **T4-S6-status**).
            attached: counts.attached,
        }
    }

    /// Compose the row at `cols`. Nothing is cached: the point of the type is
    /// that the row is built from the fields as they stand.
    pub(crate) fn compose(&self, cols: usize) -> String {
        live::compose_status_row(&self.fields(), cols)
    }

    /// Forget what the cadence painter last painted, so its next tick paints
    /// unconditionally. Called whenever anything *else* writes the pinned row
    /// (every idle paint does), because the dedupe above is only sound while
    /// the painter is the row's sole author.
    fn invalidate(&self) {
        *lock(&self.painted) = None;
    }
}

/// Take a mutex ignoring poisoning, for the same reason
/// `InlineSurface::lock` does: a panic mid-paint leaves a usable terminal,
/// and refusing to draw afterwards would only strand a pinned region with a
/// frozen row on it.
fn lock<T>(mutex: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The painter the during-turn cadence reaches: the installed viewport and
/// the live fields, published by [`BridgeGuard::enable_live_status`] and
/// taken back by the guard's `Drop`.
///
/// Process-global because the cadence runs on the type-ahead collector's
/// thread (`run_live_typeahead`'s 50 ms `libc::poll` tick) while the guard is
/// a `run_chat` local that is neither `Send` nor `Sync`. Both halves stored
/// together so a painter can never be published without the fields it paints
/// from.
static STATUS_PAINTER: std::sync::Mutex<Option<(Arc<InlineSurface>, Arc<StatusSource>)>> =
    std::sync::Mutex::new(None);

/// The pinned pane, published for painters that hold no [`BridgeGuard`]
/// (card T3-S7a's approval modal), and taken back by the guard's `Drop`.
///
/// Process-global for exactly `STATUS_PAINTER`'s reason: an approval prompter
/// runs on the tool executor, while the guard is a `run_chat` local that is
/// neither `Send` nor `Sync`. `InlineSurface::paint` is write-only and knows
/// nothing about stdin, and the pane carries its own latched geometry, so a
/// painter needs nothing from the guard but this handle.
static MODAL_PANE: std::sync::Mutex<Option<Arc<PinnedPane>>> = std::sync::Mutex::new(None);

/// Requirement **R7**: paint an approval prompt as a modal on the pinned
/// bottom rows, and say whether it went there.
///
/// `false` means no pinned pane is installed -- classic mode, or a terminal
/// below finding F19's bound -- and the caller then prints the classic stderr
/// prompt instead. That is what keeps classic's bytes identical: classic
/// installs no viewport, so this is inert before it writes anything.
///
/// The rows are [`kitsu_render::approval_modal`]'s composition of the
/// caller's own header, provenance and option strings, painted through
/// [`PinnedPane::paint_rows`] so the clamp, the re-latch and the paint are
/// one operation with F19's and F16's behaviour intact.
/// Card **T3-S7b**: `cue` is the row the user types at, appended below the
/// option line when the answer is read in-loop. It is a *row* rather than a
/// `> ` written into the scroll region, which is the whole point of the card:
/// the modal owns the cue, so nothing about the prompt lands in the
/// transcript. `None` keeps the modal exactly as T3-S7a painted it.
pub(crate) fn paint_approval_modal(
    header: &str,
    details: &[String],
    options: &str,
    cue: Option<&str>,
) -> bool {
    let mut rows = kitsu_render::approval_modal(header, details, options);
    if let Some(cue) = cue {
        rows.push(cue.to_string());
    }
    paint_modal_rows(&rows)
}

/// Paint arbitrary rows onto the pinned modal pane over the composer, growing
/// the viewport to fit (clamped by the terminal). Returns false when no pane is
/// published (classic mode / no inline viewport), so the caller can fall back.
///
/// This is the non-fullscreen popup primitive: an overlay that lives above the
/// composer row rather than taking the alternate screen. The model picker and
/// the approval modal both paint through here; [`clear_approval_modal`] takes
/// any of them back down and restores the composer's status row. `max_rows` the
/// pane will grant is [`granted_rows`](BridgeGuard::granted_rows) via the
/// published pane — callers that need the budget ahead of time read it there.
pub(crate) fn paint_modal_rows(rows: &[String]) -> bool {
    let Some(pane) = lock(&MODAL_PANE).clone() else {
        return false;
    };
    pane.paint_rows(rows)
}

/// How many rows the pinned modal pane would grant for a popup — the budget a
/// composer-popup must fit within (terminal height less the pinned composer
/// row). Zero when no pane is published.
pub(crate) fn modal_pane_budget() -> u16 {
    lock(&MODAL_PANE)
        .clone()
        .map_or(0, |pane| pane.granted_rows(u16::MAX).saturating_sub(1))
}

/// Whether a pinned pane is published at all: card **T3-S7b**'s "is there a
/// pane" question, asked *before* anything is claimed or printed.
///
/// Deliberately cheap and deliberately not the whole answer: a published pane
/// whose viewport is released still paints nothing, so the bypass decision is
/// taken from [`paint_approval_modal`]'s own return value. This only says
/// whether it is worth asking -- in classic mode it is not, and the prompter
/// must then not disturb a byte.
pub(crate) fn approval_pane_published() -> bool {
    lock(&MODAL_PANE).is_some()
}

/// Take the modal back down once the decision has been read, leaving the one
/// status row the pane had before it.
///
/// The row is recomposed from the published [`StatusSource`] rather than
/// remembered, so the row that comes back is the row the cadence would paint
/// next -- and `invalidate` is what lets that cadence paint it again, since
/// its dedupe would otherwise believe the modal's bottom row is on screen.
pub(crate) fn clear_approval_modal() {
    let Some(pane) = lock(&MODAL_PANE).clone() else {
        return;
    };
    // Review 3: clone the live draft rather than take-then-restore it. Taking
    // it out and re-storing through paint_turn_draft resurrected a draft a
    // concurrent end_turn_draft() had just cleared. Cloning never writes
    // TURN_DRAFT, so an end_turn_draft that wins before the clone leaves it
    // `None` here and the status row comes back. One window remains and is left
    // open on purpose: an end_turn_draft landing *after* the clone repaints a
    // during-turn box for a turn that just ended — but it is transient (the
    // idle reader repaints the composer on its next frame) and closing it would
    // mean holding TURN_DRAFT across terminal I/O, which is not worth it for a
    // one-frame cosmetic flicker.
    let turn = lock(&TURN_DRAFT).clone();
    if let Some(t) = turn {
        paint_turn_draft_on(&pane, &t);
        return;
    }
    pane.paint_rows(std::slice::from_ref(&status_row_for(&pane)));
}

/// The status row to restore on a pinned pane: R5's composed row when a
/// session has published its fields, else the pane's fixed install text. The
/// compose path invalidates the cadence painter's dedupe so it paints again.
fn status_row_for(pane: &PinnedPane) -> String {
    match lock(&STATUS_PAINTER).clone() {
        Some((_, source)) => {
            source.invalidate();
            source.compose(live::terminal_columns())
        }
        None => pane.status.clone(),
    }
}

/// The during-turn draft last painted by [`paint_turn_draft`], so taking an
/// approval modal down puts it back: the prompter clears the modal on its own
/// thread, after the collector has already repainted, and would otherwise
/// leave only the status row until the next keystroke.
#[derive(Clone)]
struct TurnDraft {
    prompt: String,
    draft: String,
    cursor: usize,
    hint: String,
    hint_reserved: bool,
}

static TURN_DRAFT: std::sync::Mutex<Option<TurnDraft>> = std::sync::Mutex::new(None);

/// The collector is done with its draft: a later modal teardown restores the
/// status row only.
pub(crate) fn end_turn_draft() {
    *lock(&TURN_DRAFT) = None;
}

/// Paint the **during-turn** draft on the pinned pane, exactly as the idle
/// composer paints it, and remember it so a later approval-modal teardown can
/// put it back. The classic live row stands down while the inline viewport is
/// installed (finding F15), so without this the collector's draft was stored
/// and queued but never drawn. Returns false when no pane is published
/// (classic mode).
pub(crate) fn paint_turn_draft(
    prompt: &str,
    draft: &str,
    cursor: usize,
    hint: &str,
    hint_reserved: bool,
) -> bool {
    let Some(pane) = lock(&MODAL_PANE).clone() else {
        return false;
    };
    let turn = TurnDraft {
        prompt: prompt.to_string(),
        draft: draft.to_string(),
        cursor,
        hint: hint.to_string(),
        hint_reserved,
    };
    *lock(&TURN_DRAFT) = Some(turn.clone());
    paint_turn_draft_on(&pane, &turn)
}

/// Paint one [`TurnDraft`] on `pane` without touching the stored `TURN_DRAFT`.
/// Shared by [`paint_turn_draft`] (which stores first) and
/// [`clear_approval_modal`] (which restores a clone, so a racing
/// [`end_turn_draft`] can never be resurrected — review 3). An empty draft
/// collapses the pane to the status row, so no empty box stays pinned while
/// the turn streams (review F7).
fn paint_turn_draft_on(pane: &PinnedPane, turn: &TurnDraft) -> bool {
    if turn.draft.is_empty() && turn.hint.is_empty() {
        return pane.paint_rows(&[status_row_for(pane)]);
    }
    if !input_box_enabled() {
        // The single-row fallback's one row is the status row, which the
        // turn's cadence repaints every tick; the draft gets its own row below.
        // ponytail: no slash palette here during a turn; the box path has it.
        let composed = live::LiveDraft {
            prompt: turn.prompt.clone(),
            buffer: turn.draft.clone(),
            cursor: turn.cursor,
            hint: turn.hint.clone(),
            hint_reserved: turn.hint_reserved,
        };
        let row = live::compose_input_row(&composed, live::terminal_columns()).0;
        return pane.paint_rows(&[status_row_for(pane), row]);
    }
    pane.paint_composer(
        &|| status_row_for(pane),
        &turn.prompt,
        &turn.draft,
        turn.cursor,
        &turn.hint,
        turn.hint_reserved,
        kitsu_render::stderr_styled(),
    )
}

/// Repaint R5's status row on the pinned viewport row, if one is installed
/// and a session has published its fields.
///
/// **The cadence.** Called from `run_live_typeahead`'s existing 50 ms poll
/// tick, which runs for the whole of every foreground turn in inline mode and
/// already syncs the agent counts there -- so elapsed ticks over inside 50 ms
/// of the second changing, twenty times finer than R5's one-second
/// granularity, with no new thread, timer or dependency. Inert in classic
/// mode and in any session that never enabled a live status row, which is
/// what keeps the classic byte stream identical.
pub(crate) fn repaint_status_row() {
    let painter = lock(&STATUS_PAINTER).clone();
    let Some((viewport, source)) = painter else {
        return;
    };
    let cols = live::terminal_columns();
    let fields = source.fields();
    let row = live::compose_status_row(&fields, cols);
    // The dedupe key. Normally it is the row itself: an unchanged row costs
    // no bytes (T3-S5b). Under `--reduced-motion` it is the row recomposed
    // with `elapsed: None`, so a row whose *only* moved segment is the clock
    // compares equal to the one on screen and no paint follows -- note
    // **N9**'s "periodic age refresh", suppressed. The queue tag and the
    // ambient agent tag stay in the key, because they are state and not
    // animation: freezing them would hide a blocked agent.
    let key = if kitsu_render::reduced_motion_mode() {
        let still = live::StatusFields {
            elapsed: None,
            ..fields
        };
        live::compose_status_row(&still, cols)
    } else {
        row.clone()
    };
    {
        let mut painted = lock(&source.painted);
        if painted.as_deref() == Some(key.as_str()) {
            return;
        }
        // Note **N3**'s frame budget, enforced here rather than in a
        // caller's `sleep`: the row has more than one author now (the
        // approval modal's teardown paints it too), and the dedupe above
        // only ever catches a byte-identical row, so field churn alone would
        // otherwise paint as fast as it is called. Inside the window the
        // call returns *without* recording the key, so the next call past it
        // paints whatever the fields say then -- intermediate frames
        // collapse, the latest one is never lost.
        if lock(&source.last_paint).is_some_and(|last| last.elapsed() < STATUS_FRAME_INTERVAL) {
            return;
        }
        *painted = Some(key);
    }
    *lock(&source.last_paint) = Some(std::time::Instant::now());
    // Top row only: with the framed input box the status row sits above the
    // box, and repainting it must not clobber the box or pull the caret out of
    // it (review F7). In single-row mode the top row IS the row, so this is
    // the same bytes as before.
    viewport.paint_top_row(&row, cols);
}

/// Note **N3**'s coalescing window: one row paint per 1/60 s.
const STATUS_FRAME_INTERVAL: std::time::Duration = std::time::Duration::from_nanos(16_666_667);

/// Requirement **A8**'s budget: the ambient background-agent counter must
/// update within 250 ms of a registry change in TUI modes.
///
/// Named, and asserted against, so that raising a cadence past it fails
/// loudly rather than silently breaking a numbered requirement — finding
/// **F50**: "a requirement whose number appears nowhere in the test suite is
/// a requirement nobody is holding."
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) const AMBIENT_LATENCY_BUDGET: std::time::Duration =
    std::time::Duration::from_millis(250);

/// Card **T4-S8**, requirement **A8**: the **idle** composer's ambient
/// staleness bound -- the longest a registry change can go unpainted between
/// turns -- or `None` if it has no cadence at all.
///
/// # Why this seam exists
///
/// The during-turn half of A8 was already met: `run_live_typeahead` syncs the counts
/// and calls [`repaint_status_row`] once per `AMBIENT_TICK_POLL_MS`
/// (`main.rs`) tick, for the whole of every foreground turn. The **idle**
/// half was not, until this cadence. `read_line_inline_with_keys` blocks in `read()` with no
/// timeout and nothing else repaints between turns, so a background agent
/// that blocks while the user is sitting at the composer changes the registry
/// and changes **nothing on screen** until the user presses a key, resizes
/// the terminal or submits a line. A8 grants the "next prompt cycle"
/// carve-out to `classic` only.
///
/// The counts are already a **pull** ([`StatusCounts`], read through
/// `StatusSource`'s closure at paint time), so nothing has to be pushed
/// anywhere: the whole gap is that at idle **nobody calls paint**. Closing it
/// needs a cadence, and this is the interval that cadence runs at.
///
/// # What drives it
///
/// [`spawn_idle_ambient_cadence`] (one production call site, `main.rs`, right
/// where the session's [`StatusSource`] is built) subscribes to
/// `AgentRegistry::subscribe` and runs [`idle_ambient_cadence_step`] for the
/// life of the session. A registry change wakes the step, which calls
/// [`repaint_status_row`] — the counts are a pull, so a subscriber only has
/// to trigger a paint — and then, one
/// [`IDLE_AMBIENT_REFRESH_INTERVAL`] later, calls it again, because note
/// **N3**'s coalescing window drops a paint inside 1/60 s of the previous one
/// *without recording it*. That retry is the only thing that can defer the
/// cadence's paint, so it is the interval reported here.
///
/// A wake whose counts are unchanged is a **bare wake**: the step returns
/// having painted nothing, so reduced motion and N3's budget are untouched.
///
/// **The value is only honest because the cadence uses this same constant and
/// `the_idle_ambient_cadence_paints_a_registry_change_with_no_byte_read`
/// drives the real step through the painting surface.** A constant returned
/// by a function nothing drives would be exactly the vacuous pass F50 is
/// about.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn idle_ambient_refresh_interval() -> Option<std::time::Duration> {
    Some(IDLE_AMBIENT_REFRESH_INTERVAL)
}

/// How long a registry change can sit unseen once the idle cadence has been
/// woken by it: note **N3**'s coalescing window, the one and only thing that
/// can defer [`idle_ambient_cadence_step`]'s paint, and twelve times finer
/// than requirement **A8**'s 250 ms budget.
///
/// Deliberately *not* a polling period: the cadence is woken by the
/// registry's `watch` channel, so an idle session with no background agents
/// does no work at all.
const IDLE_AMBIENT_REFRESH_INTERVAL: std::time::Duration = STATUS_FRAME_INTERVAL;

/// One wake of the idle ambient cadence: wait for the next registry change
/// and, if it changed anything the row shows, repaint. Returns `false` once
/// the registry is gone and the cadence should stop.
///
/// Generic over the watched value and the counts projection so the step can be
/// driven by a plain `watch` channel in a test — the production projection is
/// `|snapshot| (snapshot.blocked, snapshot.running)`, the two numbers
/// requirement **A8**'s ambient tag is composed from.
///
/// `seen` is the pair the cadence last painted. When a wake carries the same
/// pair it is a **bare wake** (the registry's generation moved, the tag did
/// not) and **no frame is painted** — the rule
/// `read_line_inline_with_keys` applies to a resize-less `SIGWINCH` wake,
/// applied here.
///
/// Nothing in here touches stdin, the decoder or the editor's byte bypass
/// (findings **F72**/**F68**): the composer stays blocked in `read()`, and
/// the row is repainted around it from the pull the painter already does.
pub(crate) async fn idle_ambient_cadence_step<T, F, H>(
    changes: &mut tokio::sync::watch::Receiver<T>,
    counts: &F,
    seen: &mut (usize, usize),
    title: &H,
) -> bool
where
    F: Fn(&T) -> (usize, usize),
    H: Fn(usize, usize),
{
    if changes.changed().await.is_err() {
        return false;
    }
    let current = counts(&changes.borrow_and_update());
    if *seen == current {
        // A bare wake: nothing the row shows moved, so no frame.
        return true;
    }
    *seen = current;
    // Card **H2-cadence**: the title is published here, beside the repaint
    // rather than inside it, and **once**: the N3 retry below is the
    // painter's, and the title has no coalescing window to retry past.
    title(current.0, current.1);
    repaint_status_row();
    // Note **N3**: a paint inside the 1/60 s window returns *without*
    // recording the key, so the change would otherwise wait for the next
    // registry event -- which may never come while an agent sits blocked.
    // One retry past the window closes that, and the dedupe makes it free
    // when the first call landed.
    tokio::time::sleep(IDLE_AMBIENT_REFRESH_INTERVAL).await;
    repaint_status_row();
    true
}

/// Install the idle composer's ambient cadence for the session: card
/// **T4-S8**, requirement **A8**.
///
/// The **one** production caller is `main.rs`, beside the session's
/// [`StatusSource`]. Deliberately ungated — unlike the classic-mode
/// external-printer notifier, which is behind `KITSU_LIVE_NOTICES` and an
/// assistive-input check and therefore leaves a default TUI session with no
/// subscriber at all. Inert in classic mode and in any session that never
/// enabled a live status row, because [`repaint_status_row`] is: that is what
/// keeps the classic byte stream identical.
///
/// The task ends on its own when the registry's sender drops.
pub(crate) fn spawn_idle_ambient_cadence<T, F, H>(
    mut changes: tokio::sync::watch::Receiver<T>,
    counts: F,
    title: H,
) -> tokio::task::JoinHandle<()>
where
    T: Send + Sync + 'static,
    F: Fn(&T) -> (usize, usize) + Send + Sync + 'static,
    H: Fn(usize, usize) + Send + Sync + 'static,
{
    tokio::spawn(async move {
        let mut seen = counts(&changes.borrow_and_update());
        while idle_ambient_cadence_step(&mut changes, &counts, &mut seen, &title).await {}
    })
}

/// Whether the composer paints the framed input box. On by default;
/// `KITSU_INPUT_BOX=0` (or `false`) restores the single composer row. Test
/// builds default to the legacy row so the composer/palette tests keep pinning
/// that renderer; the box geometry is tested in `kitsu_render::live`.
pub(crate) fn input_box_enabled() -> bool {
    input_box_setting(std::env::var_os("KITSU_INPUT_BOX").as_deref(), !cfg!(test))
}

/// `KITSU_INPUT_BOX` parsing: `0`/`false` off, `1`/`true` on, anything else
/// (including unset) falls back to `default`.
fn input_box_setting(value: Option<&std::ffi::OsStr>, default: bool) -> bool {
    match value.and_then(|v| v.to_str()) {
        Some("0") | Some("false") => false,
        Some("1") | Some("true") => true,
        _ => default,
    }
}

impl BridgeGuard {
    /// Hand the guard requirement **R5**'s live fields, replacing the fixed
    /// `status` text for every idle paint, and publish the during-turn
    /// cadence's painter.
    ///
    /// Called once per session, by the one caller that owns the fields. The
    /// registration is the guard's, so `Drop` takes it back and a released
    /// viewport can never be painted by a late tick.
    pub(crate) fn enable_live_status(&self, source: Arc<StatusSource>) {
        *lock(&STATUS_PAINTER) = Some((self.pane.viewport.clone(), source.clone()));
        *self.live_status.borrow_mut() = Some(source);
        // Paint at once rather than at the next read: `install` has already
        // put the fallback text on the row, and leaving it there until the
        // first keystroke would show a frame of a row this session does not
        // use.
        let row = self.status_row();
        self.pane
            .viewport
            .paint(std::slice::from_ref(&row), live::terminal_columns());
    }

    /// What belongs on the pinned row when there is no draft to show:
    /// requirement **R5**'s composed status row once a session has published
    /// its fields, and `install`'s fixed text until then.
    ///
    /// Re-read, never captured: each call composes from the fields as they
    /// stand, which is the whole difference between this and the `&str`
    /// T3-S5 left behind.
    fn status_row(&self) -> String {
        match self.live_status.borrow().as_ref() {
            Some(source) => {
                // This paint is about to author the row, so the cadence
                // painter's dedupe no longer describes what is on screen.
                source.invalidate();
                source.compose(live::terminal_columns())
            }
            None => self.pane.status.clone(),
        }
    }
    /// Paint the idle prompt's draft onto the pinned viewport row.
    ///
    /// The row is the *status* row until the user starts typing and the
    /// composer only while a draft exists: an empty draft repaints the status
    /// text [`install`] put there, so an idle session's bottom row reads the
    /// same whether or not a read is in flight.
    ///
    /// The viewport itself stays private. The wiring only ever needs a row on
    /// screen, so the guard exposes that rather than handing out the
    /// `Arc<InlineSurface>` and letting a caller `show`/`hide` it behind the
    /// guard's back.
    /// The draft is composed through [`kitsu_render::live::compose_input_row`]
    /// rather than concatenated here: `hint` is the row's hint slot and
    /// `hint_reserved` says whether it claims its columns *before* the draft
    /// is windowed (the reverse-search status) or only renders when the draft
    /// already fits (the slash-completion suffix). That math lives in
    /// `kitsu-render` for the classic row, so the guard is where the two
    /// renderers converge on one value type instead of drifting apart.
    // `paint_composer` is the production painter now -- it is this method plus
    // the palette, and routing every production paint through one of them
    // keeps a keystroke from repainting fewer rows than the viewport has. What
    // remains here is the one-row composition the tests measure parity
    // against, so in a non-test build nothing calls it.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn paint_draft(
        &self,
        prompt: &str,
        draft: &str,
        cursor: usize,
        hint: &str,
        hint_reserved: bool,
    ) {
        let row = if draft.is_empty() && hint.is_empty() {
            self.status_row()
        } else {
            let composed = live::LiveDraft {
                prompt: prompt.to_string(),
                buffer: draft.to_string(),
                cursor,
                hint: hint.to_string(),
                hint_reserved,
            };
            // The cursor column comes back too, but the viewport is one row
            // and `InlineSurface::paint` parks the cursor back inside the
            // scroll region, so there is nowhere to put a caret until the
            // multi-row composer lands.
            live::compose_input_row(&composed, live::terminal_columns()).0
        };
        // Deliberately *not* `paint_rows`: that one makes the viewport as
        // tall as its content, and the draft is one row, so routing the draft
        // through it would shrink a viewport another caller made taller --
        // which `resize_relatches_by_tearing_down_the_old_geometry_and_setting_up_the_new`
        // pins against. The draft paints into whatever height is latched.
        self.pane
            .viewport
            .paint(std::slice::from_ref(&row), live::terminal_columns());
    }

    /// The palette-aware composer paint: the bordered slash palette's rows,
    /// if this draft has one, followed by the composer row as the **last**
    /// row, painted through [`paint_rows`](Self::paint_rows) so the
    /// viewport's height follows the content.
    ///
    /// The height is asked for **before** the palette is laid out. A box
    /// cannot be built full and then clamped: `fit_viewport_rows` drops rows
    /// from the top, which for a box is its lid. So the terminal's grant
    /// comes first, one row of it is spent on the composer, and the palette
    /// is built into what is left -- which below
    /// [`PALETTE_MIN_ROWS`] is nothing at all, and then the composer paints
    /// alone with no partial frame.
    ///
    /// With no palette the content is one row, so this is exactly
    /// [`paint_draft`](Self::paint_draft)'s bytes: the composer row is
    /// composed through the same [`kitsu_render::live::compose_input_row`] at
    /// the same width, and a one-row paint through `paint_rows` re-latches
    /// nothing when the height is already one.
    ///
    /// `rich` is a parameter rather than something read here because the
    /// surface's styling is the *renderer's* state, not this process's
    /// stderr: `main.rs` passes what the renderer decided.
    ///
    /// The palette is recomputed on every call rather than cached. It is a
    /// few `format!`s over a static registry once per keystroke, and a cache
    /// would need the granted height in its key -- a palette rebuilt at the
    /// old budget surviving a resize is exactly the defect
    /// `resize_rows_preserves_an_open_palettes_height` pins.
    /// After a read: collapse the viewport back to the single status row, so an
    /// empty input box is not left pinned while the turn streams (review F7).
    /// The next idle read paints the box again.
    pub(crate) fn reset_composer(&self) -> bool {
        self.paint_rows(&[self.status_row()])
    }

    pub(crate) fn paint_composer(
        &self,
        prompt: &str,
        draft: &str,
        cursor: usize,
        hint: &str,
        hint_reserved: bool,
        rich: bool,
    ) -> bool {
        self.pane.paint_composer(
            &|| self.status_row(),
            prompt,
            draft,
            cursor,
            hint,
            hint_reserved,
            rich,
        )
    }

    /// A terminal resize that keeps the viewport's **current** height, for
    /// `main.rs`'s `on_resize` to call instead of `resize(rows, 1)`.
    ///
    /// The hardcoded one row is the defect: a resize with the palette open
    /// collapsed it to its bottom row and left the rest of the box inside the
    /// scroll region, where the next transcript line scrolled it away. The
    /// height already lives in the guard, so the re-probe preserves it --
    /// genuinely latched, so the next paint lands on the right absolute
    /// lines.
    pub(crate) fn resize_rows(&self, rows: u16) -> bool {
        self.resize(rows, self.pane.viewport_rows())
    }

    /// T0-S15: how many rows a [`paint_rows`](Self::paint_rows) request for
    /// `requested` rows would actually be granted on this terminal.
    ///
    /// The pane's own query, so the guard and the modal painter share one
    /// clamp (see [`PinnedPane::granted_rows`]).
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn granted_rows(&self, requested: u16) -> u16 {
        self.pane.granted_rows(requested)
    }

    /// T0-S14: draw `content` on the pinned viewport, making the viewport as
    /// tall as the content needs -- the pane's one operation (see
    /// [`PinnedPane::paint_rows`]).
    pub(crate) fn paint_rows(&self, content: &[String]) -> bool {
        self.pane.paint_rows(content)
    }

    /// Finding F21, step 1: hand the terminal back as a normal full-screen
    /// scrolling terminal before the process stops.
    ///
    /// Exactly `InlineSurface::hide`'s teardown — clear the pinned rows,
    /// release the DECSTBM region, park below the transcript — and nothing
    /// else: no termios restore and no signal, both of which are the
    /// caller's, in that order, after this returns.
    pub(crate) fn release_for_suspend(&self) {
        // Delegated, not reimplemented: `hide` is the only place the teardown
        // byte sequence lives, so a release cannot drift from a drop.
        self.pane.viewport.hide();
    }

    /// Finding F16: apply a terminal resize, through the pane that owns the
    /// latched geometry (see [`PinnedPane::resize`]).
    pub(crate) fn resize(&self, rows: u16, viewport_rows: u16) -> bool {
        self.pane.resize(rows, viewport_rows)
    }

    /// Finding F21, step 5: put back what [`release_for_suspend`] took, so the
    /// round trip is indistinguishable from a fresh [`install`].
    ///
    /// [`release_for_suspend`]: Self::release_for_suspend
    pub(crate) fn reinstall_after_resume(&self) {
        // Exactly what [`install`] does after the geometry gate, in the same
        // order and through the same two calls: re-latch and set the region,
        // then paint the row. The geometry comes from the guard because
        // `hide` took the surface's own copy (see [`rows`](Self::rows)), and
        // the row is the *status* — the draft is the caller's to repaint when
        // it re-enters the reader.
        self.pane
            .viewport
            .show(self.pane.rows(), self.pane.viewport_rows());
        let row = self.status_row();
        self.pane
            .viewport
            .paint(std::slice::from_ref(&row), live::terminal_columns());
    }
}

impl PinnedPane {
    /// The composer's whole paint -- status row, slash palette, and the framed
    /// box (or the single row with `KITSU_INPUT_BOX=0`) -- shared by the idle
    /// reader ([`BridgeGuard::paint_composer`]) and the during-turn collector
    /// ([`paint_turn_draft`]), so both show a draft the same way.
    #[allow(clippy::too_many_arguments)]
    fn paint_composer(
        &self,
        status_row: &dyn Fn() -> String,
        prompt: &str,
        draft: &str,
        cursor: usize,
        hint: &str,
        hint_reserved: bool,
        rich: bool,
    ) -> bool {
        let columns = live::terminal_columns();
        // The framed input box is the default; KITSU_INPUT_BOX=0 restores the
        // single composer row. It reuses `paint_rows`' region growth — the same
        // machinery the slash palette grows into — and keeps the palette: the
        // palette rows sit ABOVE the box, so `/` completion still works.
        if input_box_enabled() {
            let composed = live::LiveDraft {
                prompt: prompt.to_string(),
                buffer: draft.to_string(),
                cursor,
                hint: hint.to_string(),
                hint_reserved,
            };
            // Fit the box to what the terminal will grant BEFORE painting, so
            // the viewport never drops the top border to make room (review F8).
            // One row of the grant is the status row, which sits at the TOP
            // (decision 4: meter above the box). It must be the region's first
            // row because the status cadence repaints exactly that row.
            let grant = usize::from(self.granted_rows(u16::MAX)).saturating_sub(1);
            let boxed = live::fit_input_box(live::compose_input_box(&composed, columns), grant);
            let budget = u16::try_from(grant.saturating_sub(boxed.rows.len())).unwrap_or(0);
            let mut content = vec![status_row()];
            if crate::slash_palette_adds_to_the_row(draft, hint) {
                content.extend(palette_rows(
                    draft,
                    rich,
                    crate::composer_surface_inner_width(Some(columns)),
                    budget,
                ));
            }
            let above = content.len();
            content.extend(boxed.rows.iter().cloned());
            let painted = self.paint_rows(&content);
            // Own the caret inside the box, through the surface so the next
            // transcript write re-parks on the scroll region first (review F5).
            // The row is counted from the viewport's top, from the latched
            // geometry — not a fresh terminal probe (review F8).
            if painted {
                let row = u16::try_from(above + boxed.caret.0).unwrap_or(u16::MAX);
                let col = u16::try_from(boxed.caret.1).unwrap_or(u16::MAX);
                self.viewport.place_caret(row, col);
            }
            return painted;
        }
        // What this terminal will actually grant, less the composer's own
        // row: the palette's whole budget. `granted_rows` is the same clamp
        // `paint_rows` latches, so there is no second bound to drift.
        let budget = self.granted_rows(u16::MAX).saturating_sub(1);
        // Whether a palette belongs on screen at all is the slash registry's
        // call, not this module's: a draft that already names a command, or
        // whose hint points somewhere the list does not lead, gets the row
        // alone rather than a menu that contradicts it.
        let mut content = if crate::slash_palette_adds_to_the_row(draft, hint) {
            palette_rows(
                draft,
                rich,
                crate::composer_surface_inner_width(Some(columns)),
                budget,
            )
        } else {
            Vec::new()
        };
        content.push(if draft.is_empty() && hint.is_empty() {
            // Exactly `paint_draft`'s rule: an idle row reads the status
            // text, so the bottom row says the same whether or not a read is
            // in flight. Keeping the rule here is what lets the post-read
            // reset route through this method -- it then collapses an open
            // palette *and* leaves today's status row behind.
            status_row()
        } else {
            let composed = live::LiveDraft {
                prompt: prompt.to_string(),
                buffer: draft.to_string(),
                cursor,
                hint: hint.to_string(),
                hint_reserved,
            };
            live::compose_input_row(&composed, columns).0
        });
        self.paint_rows(&content)
    }

    /// The terminal height currently latched.
    fn rows(&self) -> u16 {
        self.rows.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// The pinned row count currently latched.
    fn viewport_rows(&self) -> u16 {
        self.viewport_rows
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// T0-S15: how many rows a [`paint_rows`](Self::paint_rows) request for
    /// `requested` rows would actually be granted on this terminal.
    ///
    /// A **query**, not a second source of truth: it is
    /// [`kitsu_tui::viewport::clamp_viewport_rows`] against the guard's own
    /// stored terminal height, which is precisely what `paint_rows` latches
    /// for the same request. A caller that computed the clamp itself would be
    /// a second copy of F19's bound, free to drift from the one the paint
    /// uses.
    ///
    /// It exists because a bordered box cannot be built at full size and then
    /// clamped: `fit_viewport_rows` drops rows from the **top**, which for a
    /// list is right and for a box removes the lid. The palette therefore asks
    /// what height it will get *before* laying its content out.
    pub(crate) fn granted_rows(&self, requested: u16) -> u16 {
        clamp_viewport_rows(self.rows(), requested)
    }

    /// T0-S14: draw `content` on the pinned viewport, making the viewport as
    /// tall as the content needs.
    ///
    /// One operation on purpose: the clamp, the re-latch when the height
    /// changed, and the paint. A caller that could latch one height and then
    /// paint a different number of rows would spill rows into the scroll
    /// region, where they scroll away with the transcript -- so the guard owns
    /// the row count and the caller only says what it wants drawn.
    ///
    /// The height is [`kitsu_tui::viewport::clamp_viewport_rows`] of the
    /// request against the stored terminal height, and the rows drawn are
    /// [`kitsu_tui::viewport::fit_viewport_rows`] of the request -- dropped
    /// from the top, so the composer survives. A height change is a re-latch
    /// through the same [`resize`](Self::resize) path a terminal resize takes,
    /// with a different second argument, which is also what keeps an
    /// **unchanged** height from emitting any region bytes.
    ///
    /// Returns whether a viewport is pinned once the paint has been applied.
    ///
    // The first production caller is the bordered slash palette, the card
    // that actually requests more than one row; the draft deliberately does
    // not route through here (see `paint_draft`), so in a non-test build
    // nothing calls it yet.
    pub(crate) fn paint_rows(&self, content: &[String]) -> bool {
        // No geometry logic below the guard: the clamp is the pure function
        // `show`'s bound lives in, and the re-latch is `resize`'s own path
        // with the terminal height unchanged -- so `Unchanged` still emits
        // nothing and `Release` still releases.
        let height = clamp_viewport_rows(
            self.rows(),
            u16::try_from(content.len()).unwrap_or(u16::MAX),
        );
        if height == 0 {
            // Nothing fits: a terminal under three rows, or no content at
            // all. Leave the latched geometry alone rather than asking
            // `show` for a zero-row viewport, which it would accept as a
            // full-screen region -- a released viewport stays released and
            // emits no bytes.
            return self.installed().is_some();
        }
        self.resize(self.rows(), height);
        // The rows painted are `fit_viewport_rows`' slice, whose `len()` is
        // the very height just latched: there is no second count to drift.
        self.viewport.paint(
            fit_viewport_rows(content, self.rows()),
            live::terminal_columns(),
        );
        self.installed().is_some()
    }

    /// Finding F16: apply a terminal resize.
    ///
    /// The region is **re-set**, never patched: the teardown for the old
    /// geometry, then the setup for the new one, which is exactly
    /// [`release_for_suspend`] followed by the geometry half of
    /// [`reinstall_after_resume`] with new dimensions. DECSTBM margins are
    /// absolute row numbers, so there is no such thing as adjusting one.
    ///
    /// Three things follow from that and are pinned by this module's tests:
    ///
    /// * a resize to the **same** dimensions emits nothing, so a spurious
    ///   `SIGWINCH` cannot flicker the pinned row;
    /// * the guard's own `rows`/`viewport_rows` are **updated**, or the next
    ///   suspend's reinstall would put back the pre-resize geometry;
    /// * crossing below F19's `rows >= viewport_rows + 2` bound **releases**
    ///   the viewport and leaves it released, and a later resize back above
    ///   the bound re-installs it.
    ///
    /// No row is painted here. A resize happens mid-read, so the row's content
    /// is the caller's current draft rather than the status, and the caller
    /// repaints immediately afterwards -- painting the status first would show
    /// a frame of the wrong thing.
    ///
    /// Returns whether a viewport is pinned once the resize has been applied.
    ///
    /// [`release_for_suspend`]: BridgeGuard::release_for_suspend
    /// [`reinstall_after_resume`]: Self::reinstall_after_resume
    pub(crate) fn resize(&self, rows: u16, viewport_rows: u16) -> bool {
        // Exactly the suspend pair with new dimensions, minus the paint --
        // and *one* pair rather than four branches, because both halves are
        // already inert where a plan says they should be: `hide` writes
        // nothing without a latched geometry (so `Install` emits the setup
        // alone) and `show` writes nothing below F19's bound (so `Release`
        // emits the teardown alone). The only arm that needs its own
        // treatment is `Unchanged`, which must emit no bytes whatsoever.
        let plan = plan_resize(self.installed(), rows, viewport_rows);
        self.rows.store(rows, std::sync::atomic::Ordering::Relaxed);
        self.viewport_rows
            .store(viewport_rows, std::sync::atomic::Ordering::Relaxed);
        if plan != ResizePlan::Unchanged {
            self.viewport.hide();
            self.viewport.show(rows, viewport_rows);
        }
        self.installed().is_some()
    }

    /// The geometry currently pinned, or `None` when nothing is.
    ///
    /// Derived rather than stored: `install` and `resize` only ever hand
    /// `show` the dimensions kept here, and `show` latches a geometry exactly
    /// when it clears F19's bound -- so the bound applied to the stored
    /// dimensions *is* whether a viewport is pinned, and there is no second
    /// flag that could contradict it.
    fn installed(&self) -> Option<(u16, u16)> {
        let (rows, viewport_rows) = (self.rows(), self.viewport_rows());
        (rows >= viewport_rows.saturating_add(2)).then_some((rows, viewport_rows))
    }
}

/// The `SIGWINCH` latch (finding F16).
///
/// The handler may do **nothing** but store into this: it is the only
/// async-signal-safe operation available to it. No allocation, no locking, no
/// printing, and above all no escape sequences -- a handler that painted would
/// interleave its bytes into whatever write the interrupted thread was in the
/// middle of.
static RESIZE_PENDING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Record that the terminal was resized. This is the whole of what the
/// `SIGWINCH` handler does, exposed under its own name so the reader's
/// flag-and-check contract can be driven from a test without raising a real
/// signal into a blocking read.
pub(crate) fn note_resize() {
    // A relaxed store would do -- the flag is the only thing the handler
    // touches -- but `SeqCst` costs nothing at this rate and keeps the latch
    // ordered against the reader's `swap` without anyone having to reason
    // about it.
    RESIZE_PENDING.store(true, std::sync::atomic::Ordering::SeqCst);
}

/// Take the pending-resize flag, clearing it: `true` exactly once per
/// [`note_resize`] burst.
///
/// Clearing on read is what keeps one resize from being handled twice, and
/// coalescing is correct rather than lossy -- the flag carries no dimensions,
/// so the handler re-probes the terminal and several notifications collapse
/// into one re-latch.
pub(crate) fn take_resize() -> bool {
    RESIZE_PENDING.swap(false, std::sync::atomic::Ordering::SeqCst)
}

/// Install the `SIGWINCH` handler.
///
/// The read loop blocks in `read()`, and a signal interrupts it with `EINTR`,
/// which the reader already retries on -- so the signal needs no self-pipe and
/// no `poll`: waking the read *is* the delivery mechanism, and the flag is how
/// the reader learns why it woke.
pub(crate) fn install_sigwinch_handler() {
    /// The handler itself: one atomic store and nothing else. Every other
    /// operation -- allocating, locking, printing, writing an escape
    /// sequence -- is not async-signal-safe.
    extern "C" fn handler(_signal: libc::c_int) {
        note_resize();
    }

    // SAFETY: `sigaction` with a zeroed `struct sigaction` whose handler is a
    // plain `extern "C"` function doing one atomic store, and a null
    // `oldact`. Nothing here can unwind into the signal context.
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = handler as *const () as libc::sighandler_t;
        libc::sigemptyset(&mut action.sa_mask);
        // ============================================================
        // `sa_flags = 0`, and **`SA_RESTART` must never be added here.**
        //
        // This is the whole reason the handler is installed with
        // `sigaction` rather than the one-line `libc::signal`: on
        // Linux/glibc `signal()` implies `SA_RESTART`, and with that flag
        // set a `SIGWINCH` arriving while the reader blocks in `read()`
        // *restarts the read* instead of returning `EINTR`. The handler
        // would still set the flag, but the read loop would never wake to
        // check it, so the viewport would silently never resize -- while
        // every unit test in this module still passed, because raising a
        // signal from the same thread never blocks in `read`.
        //
        // Waking the read *is* this feature's delivery mechanism (which is
        // why it needs no self-pipe and no `poll`). Do not "simplify" this
        // to `libc::signal`, and do not add `SA_RESTART`: only a human at a
        // real terminal, resizing the window mid-draft, would notice.
        // ============================================================
        action.sa_flags = 0;
        libc::sigaction(libc::SIGWINCH, &action, std::ptr::null_mut());
    }
}

/// The byte string a dying signal handler writes, composed at compile time.
///
/// Exactly [`kitsu_tui::write_restore_sequences`]'s stream, in the same
/// order (region reset, show cursor, bracketed paste off, title pop), spelled
/// as one `const` so the handler performs **one** `write(2)` of a buffer it
/// does not have to build. Composing it at run time would mean formatting or
/// allocating inside a signal handler, which is exactly what is not allowed
/// there -- and calling the helper itself is no better, because `io::Write`
/// on `Stderr` takes a lock.
const SIGNAL_RESTORE: &[u8] = b"\x1b[r\x1b[?25h\x1b[?2004l\x1b[23;2t";

/// The same stream with the alternate screen's leave **appended**, for a
/// signal taken while a surface is holding it (card **F94**).
///
/// Two `const`s and a choice between them, rather than one `const` plus a
/// second `write(2)`: the handler's one write is what keeps the restore
/// atomic against a terminal that may be read by the shell the instant the
/// process dies. Composed from the two existing `const`s at compile time, so
/// this is not a third copy of either sequence -- `LEAVE_ALT_SCREEN` is the
/// one `FullscreenSegment::leave` writes.
///
/// **Appended, never interleaved.** The four sequences in `SIGNAL_RESTORE`
/// are global to the terminal rather than per-screen-buffer, so the order
/// relative to the screen switch does not change what the user is left with,
/// and keeping `SIGNAL_RESTORE` a prefix is what lets every existing
/// assertion about the handler's bytes go on holding.
const SIGNAL_RESTORE_WITH_SCREEN: [u8; SIGNAL_RESTORE.len() + LEAVE_ALT_SCREEN.len()] = {
    let mut out = [0u8; SIGNAL_RESTORE.len() + LEAVE_ALT_SCREEN.len()];
    let mut i = 0;
    while i < SIGNAL_RESTORE.len() {
        out[i] = SIGNAL_RESTORE[i];
        i += 1;
    }
    let mut j = 0;
    while j < LEAVE_ALT_SCREEN.len() {
        out[SIGNAL_RESTORE.len() + j] = LEAVE_ALT_SCREEN[j];
        j += 1;
    }
    out
};

/// The signal kitsu is raising **on itself** right now, or `0`.
///
/// Set by [`raise_in_process`] around its `raise(2)` and read by the dying
/// signal handler, which returns without doing anything when the two match.
/// That is what keeps turn cancellation working: in raw mode `ISIG` is
/// cleared, so Ctrl+C reaches the editor as a byte, and the during-turn
/// collector re-raises the real `SIGINT` in-process precisely so the
/// `tokio::signal::ctrl_c()` arm of the turn's `select!` fires. A handler
/// that restored the terminal and died on *that* signal would turn every
/// mid-turn Ctrl+C into an exit.
///
/// The flag needs no window of tolerance: `raise(2)` runs the handler
/// synchronously on the raising thread before it returns, so the store, the
/// handler's load and the clear are strictly ordered with no interleaving to
/// reason about.
static SELF_RAISED: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);

/// The termios the terminal had before raw mode, if a raw guard is holding
/// it, published for the dying signal handler.
///
/// `Drop` is what normally puts this back, and a signal is exactly the exit
/// path `Drop` does not run on, so the snapshot has to be reachable from a
/// handler -- which rules out a `Mutex` (locking is not async-signal-safe).
/// An `UnsafeCell` behind a flag is the whole of the synchronisation, and it
/// is sound here because the only writers are the raw guard's `engage` and
/// `drop`, which run on one thread at a time and never while the handler is
/// reading: the handler's reader is the same thread, interrupted.
struct CanonicalTermios {
    /// `MaybeUninit` because a `termios` has no const-constructible value and
    /// `valid` already says whether this one has been written.
    modes: std::cell::UnsafeCell<std::mem::MaybeUninit<libc::termios>>,
    valid: std::sync::atomic::AtomicBool,
}

// SAFETY: see `CanonicalTermios`. Access is gated on `valid`, the writers are
// the raw guard's engage/drop on a single thread, and the only other reader
// is a signal handler running on a thread that is itself inside one of those
// windows or outside both.
unsafe impl Sync for CanonicalTermios {}

static CANONICAL_TERMIOS: CanonicalTermios = CanonicalTermios {
    modes: std::cell::UnsafeCell::new(std::mem::MaybeUninit::uninit()),
    valid: std::sync::atomic::AtomicBool::new(false),
};

/// Publish the pre-raw-mode termios snapshot for the dying signal handler.
///
/// Called by `RawStdinGuard::engage` with what `tcgetattr` returned, i.e.
/// the modes a restore owes the terminal.
pub(crate) fn publish_canonical_termios(modes: &libc::termios) {
    // SAFETY: the flag is cleared first, so a handler that interrupts the
    // copy reads nothing rather than a half-written struct.
    CANONICAL_TERMIOS
        .valid
        .store(false, std::sync::atomic::Ordering::SeqCst);
    unsafe { (*CANONICAL_TERMIOS.modes.get()).write(*modes) };
    CANONICAL_TERMIOS
        .valid
        .store(true, std::sync::atomic::Ordering::SeqCst);
}

/// Withdraw the snapshot: the raw guard has already restored it itself, so a
/// handler must not `tcsetattr` a stale copy over whatever owns the terminal
/// now (rustyline's readline, or an approval prompt).
pub(crate) fn forget_canonical_termios() {
    CANONICAL_TERMIOS
        .valid
        .store(false, std::sync::atomic::Ordering::SeqCst);
}

/// Raise `signal` on this process with the dying-signal handler suppressed
/// for the duration.
///
/// Every in-process `raise` kitsu performs is a *request* -- cancel the turn,
/// suspend, quit -- addressed to a `select!` arm or to the signal's own
/// default behaviour, never a terminal restore. See [`SELF_RAISED`].
pub(crate) fn raise_in_process(signal: libc::c_int) {
    SELF_RAISED.store(signal, std::sync::atomic::Ordering::SeqCst);
    // SAFETY: raising a signal on this process; the handlers reachable from
    // here are this module's own and the runtime's.
    unsafe { libc::raise(signal) };
    SELF_RAISED.store(0, std::sync::atomic::Ordering::SeqCst);
}

/// Install the restore that runs on the exit paths `Drop` does not cover
/// (requirement N5): a panic, `SIGINT` and `SIGTERM`.
///
/// # The panic path goes **through** `kitsu_tui::install_panic_restore`
///
/// Deliberately, and this is the only thing installed here that is not a
/// `sigaction`. That helper *chains* the hook it finds rather than replacing
/// it, and the hook it finds is `main`'s broken-pipe guard -- so after this
/// call a panic runs the terminal restore, then the broken-pipe guard, then
/// the default report. A hook installed here instead would have to replace
/// one of those two, and replacing the guard would silently resurrect exit
/// 101 on a closed stdout.
///
/// # The signal handler does not terminate by itself
///
/// It restores the terminal and then re-raises on the default disposition,
/// which is what ends the session. It is inert for a signal kitsu raised on
/// itself ([`SELF_RAISED`]), because that raise is how a turn is cancelled.
///
/// Installed once per process, and only by the inline path: classic mode
/// pins no scroll region and holds raw mode only inside rustyline's own
/// readline, so it has nothing here to give back and keeps the dispositions
/// it was started with.
pub(crate) fn install_exit_restore() {
    /// Restore the terminal, then die the way the signal says.
    ///
    /// **Everything in here is async-signal-safe**, which is why it does not
    /// reuse the panic helper's body: that one calls
    /// `crossterm::terminal::disable_raw_mode` (a lock and an allocation) and
    /// writes through `io::Write` on `Stderr` (a lock). What runs here is an
    /// atomic load, one `write(2)` of a `const` buffer, `tcgetattr`-free
    /// `tcsetattr` of an already-captured struct, `sigaction` and `raise` --
    /// all on POSIX's async-signal-safe list, with no allocation, no locking
    /// and no formatting.
    extern "C" fn handler(signal: libc::c_int) {
        if SELF_RAISED.load(std::sync::atomic::Ordering::SeqCst) == signal {
            return;
        }
        // Card **F94**: which of the two precomposed streams this terminal
        // is owed. One more atomic load and a choice between two `const`s --
        // no allocation, no locking, no formatting, and still one `write(2)`.
        let restore: &[u8] = if kitsu_tui::alternate_screen_taken() {
            &SIGNAL_RESTORE_WITH_SCREEN
        } else {
            SIGNAL_RESTORE
        };
        // SAFETY: a single `write` of a `'static` buffer to a descriptor this
        // process owns; a short write or an error is not worth handling while
        // the process is dying.
        unsafe {
            libc::write(
                libc::STDERR_FILENO,
                restore.as_ptr().cast::<libc::c_void>(),
                restore.len(),
            );
        }
        if CANONICAL_TERMIOS
            .valid
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            // SAFETY: `valid` says the cell holds a complete snapshot taken
            // by `tcgetattr` on this very descriptor.
            unsafe {
                libc::tcsetattr(
                    libc::STDIN_FILENO,
                    libc::TCSANOW,
                    (*CANONICAL_TERMIOS.modes.get()).as_ptr(),
                );
            }
        } else {
            // Card **F96**: there is no snapshot, and the terminal may still
            // be raw -- which is exactly the state a session started in
            // fullscreen dies in. `RawStdinGuard` is the only publisher, and
            // fullscreen installs no inline viewport, so its raw mode is
            // **rustyline's own** and nothing in this process holds the modes
            // it displaced.
            //
            // So the three line flags a user needs are turned back on in
            // place rather than restored from a copy: canonical reads, kernel
            // echo, and job-control signals. It is not the full termios the
            // shell had -- nobody in this process knows that -- but it is the
            // difference between a shell the user can type into and one they
            // have to run `reset` on blind.
            //
            // Still async-signal-safe: `tcgetattr` and `tcsetattr` are both
            // on POSIX's list, the struct is a stack local, and there is no
            // allocation, no lock and no formatting. A failing `tcgetattr`
            // leaves the terminal as the signal found it, which is the state
            // this branch is trying to improve on and never worse.
            //
            // SAFETY: both calls write through a valid pointer to one
            // `termios` on a descriptor this process owns.
            unsafe {
                let mut modes: libc::termios = std::mem::zeroed();
                if libc::tcgetattr(libc::STDIN_FILENO, &mut modes) == 0 {
                    modes.c_lflag |= libc::ICANON | libc::ECHO | libc::ISIG;
                    libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &modes);
                }
            }
        }
        // And now die as the signal says. Putting the default disposition
        // back before re-raising is what makes this a restore rather than an
        // exit: the process's status is the signal's, not an exit code this
        // module invented, and a second delivery arriving mid-restore cannot
        // loop through the handler again.
        //
        // SAFETY: a zeroed `sigaction` with `SIG_DFL`, an empty mask and no
        // flags, followed by a raise on this process.
        unsafe {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = libc::SIG_DFL;
            action.sa_flags = 0;
            libc::sigemptyset(&mut action.sa_mask);
            libc::sigaction(signal, &action, std::ptr::null_mut());
            libc::raise(signal);
        }
    }

    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        kitsu_tui::install_panic_restore();
        for signal in [libc::SIGINT, libc::SIGTERM] {
            // SAFETY: a zeroed `sigaction` filled in before use, with a
            // handler that is async-signal-safe (see above). `sa_flags = 0`
            // for the same reason `install_sigwinch_handler` uses it: no
            // `SA_RESTART`, so a signal arriving while the inline reader
            // blocks in `read()` surfaces rather than being swallowed by a
            // restarted syscall -- though here the handler never returns to
            // that read anyway.
            unsafe {
                let mut action: libc::sigaction = std::mem::zeroed();
                action.sa_sigaction = handler as *const () as libc::sighandler_t;
                action.sa_flags = 0;
                libc::sigemptyset(&mut action.sa_mask);
                libc::sigaction(signal, &action, std::ptr::null_mut());
            }
        }
    });
}

impl Drop for BridgeGuard {
    /// Restore the previous surface **before** releasing the region, so the
    /// last bytes the viewport's descriptor sees are exactly the region reset
    /// and no transcript line can arrive through the seam after it.
    fn drop(&mut self) {
        // The cadence painter first: it holds an `Arc` to this viewport and
        // runs on another thread, so it must be unpublished before the region
        // is released rather than after.
        *lock(&STATUS_PAINTER) = None;
        // Card T3-S7a: and the modal painter's handle, for the same reason --
        // a late approval paint must not reach a released viewport.
        *lock(&MODAL_PANE) = None;
        set_surface(self.previous_surface.clone());
        live::set_live_rows_enabled(self.previous_live_rows);
        self.pane.viewport.hide();
        // Last, because `live_mode` must not answer `classic` while a region
        // is still pinned: the flag describes the terminal, so it is cleared
        // only once the terminal has been given back.
        INLINE_INSTALLED.store(false, std::sync::atomic::Ordering::Release);
    }
}

/// Which keys the inline reader answers **itself**, rather than handing to the
/// editor (card T4-S2).
///
/// Resolved once, where `[tui]` is resolved, and published through
/// [`set_inline_keys`]: the reader is reached through two call frames that
/// carry no configuration, and threading a copy down each of them would make
/// the chord prefix a parameter of functions that have no other business with
/// it.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct InlineKeys {
    /// The control byte `[tui] prefix` names, or `None` when the spelling is
    /// not one this reader can recognise — in which case the chord route is
    /// simply absent rather than silently bound to something else.
    pub(crate) prefix: Option<u8>,
    /// `[tui] left_arrow_opens_agents`: whether the two-press `←` route exists
    /// at all. It is a *route* switch, not a feature switch — the chord keeps
    /// working with it off.
    pub(crate) left_arrow_opens_agents: bool,
}

impl InlineKeys {
    /// No chord prefix and no `←` route: what the reader's own unit tests run
    /// under, so every keystroke they feed reaches the editor exactly as it
    /// did before this card.
    pub(crate) fn disabled() -> Self {
        Self::default()
    }
}

/// The published [`InlineKeys`], or `None` before `[tui]` has been resolved.
static INLINE_KEYS: std::sync::Mutex<Option<InlineKeys>> = std::sync::Mutex::new(None);

/// Publish the resolved key configuration for the inline reader.
pub(crate) fn set_inline_keys(keys: InlineKeys) {
    *lock(&INLINE_KEYS) = Some(keys);
}

/// The published key configuration, or [`InlineKeys::disabled`] when `[tui]`
/// was never resolved (a non-unix build, or a carrier that never installs a
/// renderer). Defaulting to *disabled* rather than to the shipped defaults is
/// deliberate: an unresolved config must not conjure a binding the user's
/// config file could have turned off.
pub(crate) fn inline_keys() -> InlineKeys {
    lock(&INLINE_KEYS).unwrap_or_else(InlineKeys::disabled)
}

/// The control byte a `ctrl+<letter>` key spelling sends.
///
/// `[tui] prefix` is prose (`"ctrl+x"`), and a terminal sends one byte, so one
/// of the two has to be translated. Only the `ctrl+<letter>` family is
/// accepted, because that is the only family whose byte is unambiguous: a
/// function key or an `alt+` combination arrives as an escape *sequence*, and
/// answering one would mean a second escape-sequence parser beside the
/// editor's.
pub(crate) fn chord_prefix_byte(spelling: &str) -> Option<u8> {
    let lowered = spelling.trim().to_ascii_lowercase();
    let letter = lowered
        .strip_prefix("ctrl+")
        .or_else(|| lowered.strip_prefix("ctrl-"))
        .or_else(|| lowered.strip_prefix("c-"))
        .or_else(|| lowered.strip_prefix("^"))?;
    let mut chars = letter.chars();
    let key = chars.next()?;
    if chars.next().is_some() || !key.is_ascii_lowercase() {
        return None;
    }
    // `ctrl+a` is 0x01, so the byte is the letter's position in the alphabet.
    Some(key as u8 - b'a' + 1)
}

/// The letter that, after the chord prefix, opens the agent view (A1).
const AGENT_VIEW_CHORD_KEY: u8 = b'a';

/// `<prefix> d`: detach from the attached agent (card **T4-S6a**, design
/// \u{00a7}4.7). It sits beside [`AGENT_VIEW_CHORD_KEY`] because the filter
/// treats the two the same way: the letter is lifted out of the byte stream,
/// and what it *means* is the reader's to decide.
const DETACH_CHORD_KEY: u8 = b'd';

/// `ESC [ D`, the left arrow as every terminal sends it, and what the editor
/// reads as backward-char.
const LEFT_ARROW: &[u8] = b"\x1b[D";
/// `Shift+Left` / `Shift+Right` as a terminal sends them (`1;2` is the Shift
/// modifier). Unlike plain `LEFT_ARROW` these are unambiguous presses with no
/// empty-composer affordance, so they are lifted out whole and mean "cycle the
/// attached agent" (previous / next in display order). They never reach the
/// editor, so Shift+arrow is inert as cursor motion -- deliberately, that is
/// the whole key.
const SHIFT_LEFT: &[u8] = b"\x1b[1;2D";
const SHIFT_RIGHT: &[u8] = b"\x1b[1;2C";
/// The four paging keys as a terminal sends them, and the press each one
/// means to [`kitsu_tui::fullscreen::FullscreenView`].
///
/// `PgUp`/`PgDn` are the `~`-terminated family; `Ctrl+Home`/`Ctrl+End` are the
/// **modified** forms of the finals plain `Home`/`End` send, and the
/// parameters are the only thing that tells the two pairs apart -- the editor's
/// own `b'H'`/`b'F'` arms ignore them, which is why the modified forms are
/// `Home`/`End` everywhere this table is not consulted.
const PAGE_KEYS: [(&[u8], kitsu_tui::fullscreen::Page); 4] = [
    (b"\x1b[5~", kitsu_tui::fullscreen::Page::Up),
    (b"\x1b[6~", kitsu_tui::fullscreen::Page::Down),
    (b"\x1b[1;5H", kitsu_tui::fullscreen::Page::Oldest),
    (b"\x1b[1;5F", kitsu_tui::fullscreen::Page::Newest),
];

/// `Ctrl+O`: the transcript overlay's key (requirement **R6**).
///
/// One literal control byte, so unlike the four paging sequences it has no
/// partial-sequence case and is not behind the configurable `[tui] prefix`.
const TRANSCRIPT_KEY: u8 = 0x0f;

/// The bracketed-paste opening delimiter.
const PASTE_START: &[u8] = b"\x1b[200~";
/// The bracketed-paste closing delimiter.
const PASTE_END: &[u8] = b"\x1b[201~";

/// One thing the key filter found in a chunk.
#[derive(Clone, Debug, Eq, PartialEq)]
enum KeyItem {
    /// Bytes for the editor, verbatim and in order.
    Bytes(Vec<u8>),
    /// One left-arrow keystroke, lifted out so the reader can ask whether the
    /// composer is empty *at this point in the chunk* before deciding whether
    /// it is cursor movement or half of the two-press affordance.
    Left,
    /// `Shift+Left` (`-1`) / `Shift+Right` (`+1`): step the attached agent to
    /// the previous / next in display order. Lifted out like [`KeyItem::Open`]:
    /// resolving the step needs the registry and the session attachment, which
    /// this filter does not hold, so it is carried to the caller as a delta.
    CycleAgent(isize),
    /// `<prefix> a`: open the agent view.
    Open,
    /// `<prefix> d`: detach from the attached agent (card **T4-S6a**,
    /// design §4.7). Lifted out for `Open`'s reason: the letter must not
    /// reach the draft, and the *decision* about whether anything is attached
    /// belongs to the reader and not to this filter.
    Detach,
    /// One paging press, lifted out of the stream so none of its bytes reach
    /// the editor: `Ctrl+Home` would otherwise be read as plain `Home` and
    /// move the caret. Only produced when the reader was given a paging sink
    /// ([`KeyFilter::pages`]).
    Page(kitsu_tui::fullscreen::Page),
    /// `<prefix> 1..9`: attach to the agent at that **1-based ordinal over
    /// A2's display order** (card **T4-S6-ordinal**, design §4.7's
    /// "attach agent N").
    ///
    /// The payload is the **ordinal**, never an id: ids pass 9 after nine
    /// spawns and go *wrong* rather than dead once `#1` is pruned and `#11`
    /// exists. Resolving the ordinal needs the registry, which this filter
    /// does not have and must not grow -- so it is carried out to the caller
    /// exactly as [`KeyItem::Open`] carries the decision about the overlay.
    ///
    /// `<prefix> 0` is **not** this item: the ordinal is 1-based, so `0` is
    /// inert like any other chord letter.
    Attach(usize),
    /// `Ctrl+O`: open the transcript overlay (requirement **R6**).
    ///
    /// Lifted out for [`KeyItem::Open`]'s reason, and a **literal** control
    /// byte rather than a chord letter: R6 spells the key, so it is not
    /// behind the configurable `[tui] prefix`. Measured before it was a key
    /// here: `0x0f` reached the editor, whose `0x00..=0x1f` arm dropped it,
    /// so the draft and the caret were untouched and nothing repainted.
    ///
    OpenTranscript,
}

/// Splits a read chunk into [`KeyItem`]s: the chord prefix and its letter are
/// taken out of the stream, left arrows are lifted out, and everything else is
/// passed through untouched.
///
/// # Why this is not a second keymap
///
/// It recognises exactly three things — the prefix byte, `ESC [ D`, and the
/// two bracketed-paste delimiters — and forwards every other byte to the
/// editor, which remains the only keymap. The paste delimiters are tracked for
/// one reason: inside a paste the payload is *text*, so a `0x18` or an
/// `ESC [ D` in pasted content must reach the editor as content and not be
/// read as a chord.
///
/// A chunk that ends part-way through one of those sequences **holds** the
/// partial bytes until the next read, so a left arrow split across two reads
/// is still one keystroke. The held bytes are released in front of the next
/// chunk, so the editor sees the same byte stream either way.
struct KeyFilter {
    keys: InlineKeys,
    /// The prefix byte has arrived and the next byte is its chord letter.
    /// Carried across reads, because a user's two keystrokes are routinely two
    /// chunks.
    pending_prefix: bool,
    /// Inside a bracketed-paste block.
    in_paste: bool,
    /// A partial sequence held back from the end of the previous chunk.
    held: Vec<u8>,
    /// Whether the four paging sequences are keys here at all.
    ///
    /// Off unless the reader was handed a paging sink: a surface with nowhere
    /// to send a press has no business lifting one out of the stream, and with
    /// this off every byte of all four sequences reaches the editor, which is
    /// inline's and classic's behaviour unchanged.
    pages: bool,
}

impl KeyFilter {
    fn new(keys: InlineKeys) -> Self {
        Self {
            keys,
            pending_prefix: false,
            in_paste: false,
            held: Vec::new(),
            pages: false,
        }
    }

    /// The press `data` starts with, and its length in bytes.
    fn page_press(data: &[u8]) -> Option<(kitsu_tui::fullscreen::Page, usize)> {
        PAGE_KEYS
            .iter()
            .find(|(sequence, _)| data.starts_with(sequence))
            .map(|(sequence, press)| (*press, sequence.len()))
    }

    /// Whether `data` is a strict, non-empty prefix of `pattern`: the chunk
    /// ran out mid-sequence and the rest may still arrive.
    fn is_partial(data: &[u8], pattern: &[u8]) -> bool {
        data.len() < pattern.len() && pattern.starts_with(data)
    }

    fn feed(&mut self, chunk: &[u8]) -> Vec<KeyItem> {
        let mut data = std::mem::take(&mut self.held);
        data.extend_from_slice(chunk);
        let mut items: Vec<KeyItem> = Vec::new();
        let mut bytes: Vec<u8> = Vec::new();
        let mut index = 0usize;
        while index < data.len() {
            let rest = &data[index..];
            if self.in_paste {
                if rest.starts_with(PASTE_END) {
                    self.in_paste = false;
                    bytes.extend_from_slice(PASTE_END);
                    index += PASTE_END.len();
                } else if Self::is_partial(rest, PASTE_END) {
                    self.held = rest.to_vec();
                    break;
                } else {
                    bytes.push(data[index]);
                    index += 1;
                }
                continue;
            }
            if self.pending_prefix {
                self.pending_prefix = false;
                if let Some(item) = match data[index] {
                    AGENT_VIEW_CHORD_KEY => Some(KeyItem::Open),
                    DETACH_CHORD_KEY => Some(KeyItem::Detach),
                    // Card **T4-S6-ordinal**: the nine digits, each the
                    // **ordinal** it spells. `b'0'` is deliberately not in
                    // the range: the ordinal is 1-based, so `0` names no
                    // position, and it falls to the inert arm below rather
                    // than becoming either an alias for the first row or the
                    // first half of a multi-digit entry whose meaning would
                    // be invisible until the next keystroke (**F75**).
                    digit @ b'1'..=b'9' => Some(KeyItem::Attach(usize::from(digit - b'0'))),
                    _ => None,
                } {
                    if !bytes.is_empty() {
                        items.push(KeyItem::Bytes(std::mem::take(&mut bytes)));
                    }
                    items.push(item);
                }
                // Any other letter is inert. Dropping it rather than typing it
                // into the draft is what makes a mistyped chord a no-op
                // instead of a stray character in the user's prompt.
                index += 1;
                continue;
            }
            if Some(data[index]) == self.keys.prefix {
                self.pending_prefix = true;
                index += 1;
                continue;
            }
            // Requirement **R6**. After the prefix check, so a session that
            // configured this byte as its chord prefix keeps the chord, and
            // inside the non-paste branch only: in a paste the payload is
            // *text*, and a control byte in pasted content is never a key
            // (the editor's in-paste arm drops it rather than drafting it).
            if data[index] == TRANSCRIPT_KEY {
                if !bytes.is_empty() {
                    items.push(KeyItem::Bytes(std::mem::take(&mut bytes)));
                }
                items.push(KeyItem::OpenTranscript);
                index += 1;
                continue;
            }
            if data[index] == 0x1b {
                if rest.starts_with(PASTE_START) {
                    self.in_paste = true;
                    bytes.extend_from_slice(PASTE_START);
                    index += PASTE_START.len();
                    continue;
                }
                if rest.starts_with(LEFT_ARROW) {
                    if !bytes.is_empty() {
                        items.push(KeyItem::Bytes(std::mem::take(&mut bytes)));
                    }
                    items.push(KeyItem::Left);
                    index += LEFT_ARROW.len();
                    continue;
                }
                // Shift+Left/Right: complete presses, matched before the
                // partial check for `LEFT_ARROW`'s reason. They share the
                // `ESC [ 1 ; 2` prefix with nothing the editor decodes, so
                // lifting them whole never steals a cursor key. Only lifted
                // when agent routes are active (a chord prefix is configured):
                // under `InlineKeys::disabled()` — the fullscreen and overlay
                // readers — they pass through like any unbound sequence
                // instead of becoming an agent action those readers can't
                // handle (review F3).
                if self.keys.prefix.is_some() && rest.starts_with(SHIFT_LEFT) {
                    if !bytes.is_empty() {
                        items.push(KeyItem::Bytes(std::mem::take(&mut bytes)));
                    }
                    items.push(KeyItem::CycleAgent(-1));
                    index += SHIFT_LEFT.len();
                    continue;
                }
                if self.keys.prefix.is_some() && rest.starts_with(SHIFT_RIGHT) {
                    if !bytes.is_empty() {
                        items.push(KeyItem::Bytes(std::mem::take(&mut bytes)));
                    }
                    items.push(KeyItem::CycleAgent(1));
                    index += SHIFT_RIGHT.len();
                    continue;
                }
                // Before the partial check below, for `LEFT_ARROW`'s reason:
                // a complete sequence is a press, and only an incomplete one
                // waits for the rest of itself.
                if self.pages {
                    if let Some((press, length)) = Self::page_press(rest) {
                        if !bytes.is_empty() {
                            items.push(KeyItem::Bytes(std::mem::take(&mut bytes)));
                        }
                        items.push(KeyItem::Page(press));
                        index += length;
                        continue;
                    }
                }
                if Self::is_partial(rest, PASTE_START)
                    || Self::is_partial(rest, LEFT_ARROW)
                    || Self::is_partial(rest, SHIFT_LEFT)
                    || Self::is_partial(rest, SHIFT_RIGHT)
                    || (self.pages
                        && PAGE_KEYS
                            .iter()
                            .any(|(sequence, _)| Self::is_partial(rest, sequence)))
                {
                    self.held = rest.to_vec();
                    break;
                }
            }
            bytes.push(data[index]);
            index += 1;
        }
        if !bytes.is_empty() {
            items.push(KeyItem::Bytes(bytes));
        }
        items
    }
}

/// The paging press `data` starts with, and its length in bytes.
///
/// Requirement **R6**: the transcript overlay runs its own key loop over the
/// raw bytes, exactly as the agent view's overlay does, and it reads the same
/// four sequences this module already decodes for the inline reader. Exposed
/// rather than re-tabulated: a second copy of [`PAGE_KEYS`] is how the two
/// surfaces come to disagree about which byte string `Ctrl+End` is.
pub(crate) fn page_key(data: &[u8]) -> Option<(kitsu_tui::fullscreen::Page, usize)> {
    KeyFilter::page_press(data)
}

/// Whether `data` ends part-way through one of the four paging sequences, so
/// the overlay's key loop holds it for the next read rather than resolving the
/// `ESC` as a close (finding **F65**: a multi-byte key is not one byte).
pub(crate) fn partial_page_key(data: &[u8]) -> bool {
    PAGE_KEYS
        .iter()
        .any(|(sequence, _)| KeyFilter::is_partial(data, sequence))
}

/// Re-pin the DECSTBM region and repaint the status row, for a full-screen
/// surface that has just handed the terminal back (card T4-S2's overlay).
///
/// Only control bytes and the pinned row are written, so the transcript region
/// is left exactly as the alternate-screen restore found it. It goes through
/// the published pane for [`paint_approval_modal`]'s reason: the caller may
/// hold no [`BridgeGuard`].
pub(crate) fn reinstall_pinned_region() {
    let Some(pane) = lock(&MODAL_PANE).clone() else {
        return;
    };
    pane.viewport.show(pane.rows(), pane.viewport_rows());
    repaint_status_row();
}

/// How one inline idle-prompt read ended.
///
/// T0-S5b. The reader is the inline mode's answer to rustyline: with the
/// classic live row stood down (finding F15) nothing paints a draft, so inline
/// mode has no idle input path at all. This gives it one, built on the *same*
/// [`kitsu_editor::TypeaheadEditor`] the during-turn collector uses, so the
/// keymap is shared rather than reimplemented.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum InlineRead {
    /// Enter: the submitted line, without its terminator.
    Submitted(String),
    /// Ctrl+D on an empty draft, or a read that returned zero bytes.
    Eof,
    /// Ctrl+C on an empty draft. A non-empty draft is cleared instead and the
    /// read continues, which is what the editor already does.
    Interrupted,
    /// Card **T4-S2**: the agent view overlay was asked for, by the chord
    /// `<prefix> a` or by a second `\u{2190}` on an untouched empty composer,
    /// with the draft as it stood.
    ///
    /// The reader stays **pure** for exactly [`InlineRead::Suspended`]'s
    /// reason: the overlay owns the terminal, the alternate screen and its own
    /// key loop, none of which belong in a function whose whole value is being
    /// testable without one. So the keystroke is reported and the draft handed
    /// back, and the caller re-enters the reader with it afterwards.
    OpenAgentView(String),
    /// Ctrl+Z (finding F21): the draft as it stood when the byte arrived.
    ///
    /// The reader stays **pure** — it raises no signal and touches no
    /// terminal — so it reports the keystroke and hands the draft back. The
    /// caller orchestrates the suspend and then re-enters the reader with
    /// this draft as its `initial`, which is why typing survives a
    /// suspend/resume cycle.
    Suspended(String),
    /// Card **T4-S6a**: `\u{2190}` on an empty composer **while attached**, the
    /// return path requirement A5 names beside `/detach`.
    ///
    /// The reader stays pure for [`InlineRead::Suspended`]'s reason: leaving
    /// attachment prints a milestone, replays a recap and re-labels the
    /// composer, none of which belong in a function whose value is being
    /// testable without a terminal.
    ///
    /// The draft **is** carried back, exactly as [`InlineRead::Suspended`]
    /// and [`InlineRead::OpenAgentView`] carry it, and the caller replays it
    /// into the next read. Finding **F76**: this variant has two routes, and
    /// only one of them has an empty-composer precondition. `\u{2190}` is
    /// reached only on an empty draft, so its payload is empty; `Ctrl+X d`
    /// has no such precondition and exists *because* `\u{2190}` is cursor
    /// movement once there is text to move over, so detaching with the chord
    /// must not cost the user the line they typed. The earlier comment here
    /// -- "only ever reached on an empty one" -- was true of `\u{2190}`
    /// alone and became wrong when the chord was given the same variant.
    Detached(String),

    /// Card **T4-S6-ordinal**: `<prefix> 1..9`, the chord that attaches to
    /// the agent at a **1-based ordinal over A2's display order**.
    ///
    /// Carries the ordinal and the draft. The reader stays **pure** for
    /// [`InlineRead::OpenAgentView`]'s reason, and here it is not merely
    /// tidiness: resolving an ordinal needs the **registry**, which the
    /// reader has no business holding -- design §4.7 names exactly this
    /// coupling as the card's difficulty. So the keystroke is reported, and
    /// the caller resolves it against the rows the view would display
    /// (`kitsu_tui::agents_view::nth_displayed` over the **filtered** set)
    /// and attaches, or does nothing at all when no row holds that position.
    ///
    /// The draft is carried for **F76**'s reason: the chord has no
    /// empty-composer precondition, so it must not cost the user the line
    /// they typed.
    AttachOrdinal(usize, String),

    /// `Shift+Left` / `Shift+Right`: cycle the attached agent to the previous
    /// (`-1`) or next (`+1`) in display order. Carries the delta and the draft,
    /// pure for [`InlineRead::AttachOrdinal`]'s reason: resolving a step needs
    /// the registry and the session attachment, which the reader does not hold.
    /// The chord has no empty-composer precondition, so the draft is carried
    /// back for **F76**'s reason — cycling must not cost a typed line.
    CycleAgent(isize, String),
    /// `Ctrl+O`: open the transcript overlay over the cell ring (requirement
    /// **R6**).
    ///
    /// Carries the draft for [`InlineRead::OpenAgentView`]'s reason: the
    /// overlay takes the screen this reader is painting on, so the whole
    /// sequencing is the caller's, and the draft comes back through the same
    /// replay a suspend uses so the key cannot cost the user a character.
    ///
    OpenTranscript(String),
}

/// Read one line from `input` through the shared editor, repainting through
/// `paint` after every change.
///
/// Deliberately generic over the reader and the painter: no real descriptor,
/// no termios, no viewport. Raw mode is the caller's business (it reuses
/// `RawStdinGuard`, which leaves `c_oflag` alone so `ONLCR` keeps turning
/// line feeds into CR LF), and `paint` is where the viewport row is drawn.
/// That keeps the whole keymap-to-draft path unit-testable and leaves the PTY
/// test with only the wiring to prove.
///
/// `paint` receives `(draft, cursor, hint, hint_reserved)` -- the current
/// draft, the cursor's column **within** the draft as a char offset, and the
/// row's hint slot -- in the same order and with the same meanings as
/// `kitsu_render::live::live_input_update`, so the inline row and the classic
/// live row cannot drift in *what* they are told about a draft.
/// `history` seeds the up/down walk.
// The reader's own unit tests are its only callers now: production resolves
// `[tui]` and goes through `read_line_inline_with_keys`. Kept as the
// routes-off entry point, which is what makes those tests measure the editor's
// keymap and nothing this card added.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn read_line_inline<R: std::io::Read>(
    input: &mut R,
    paint: &mut dyn FnMut(&str, usize, &str, bool),
    history: &[String],
    on_resize: &mut dyn FnMut(),
) -> std::io::Result<InlineRead> {
    read_line_inline_with_keys(
        input,
        paint,
        history,
        on_resize,
        InlineKeys::disabled(),
        None,
        None,
        None,
    )
}

/// What the **first** `\u{2190}` on an untouched empty composer puts in the hint
/// slot: that another press is needed, and what it would open.
///
/// A two-press key whose first press is silent is a key that did nothing, so
/// this is the affordance rather than decoration. Plain ASCII words, not an
/// arrow glyph: the slot is text on a row whose styling tier is the
/// renderer's, and the hint has to read the same in all of them.
const LEFT_ARM_HINT: &str = "  press left again for the agent view";

/// The composer's prompt label, given the session's base prompt and the agent
/// it is attached to (card **T4-S6a**, design §4.5: “the prompt label changes
/// to `agent 3>`”).
///
/// One function because the label is the **only** standing evidence that a
/// submitted line goes to a background agent rather than to the model, and
/// that evidence is what makes `\u{2190}` safe to overload (see
/// [`read_line_inline_with_keys`]): the context the keystroke depends on is on
/// screen in front of the caret, not something the user did minutes ago.
///
/// It **replaces** the base rather than decorating it: the base prompt is
/// configurable and `agent 3> kitsu> ` would be absurd.
///
/// Returns the base unchanged while detached, which is all this card ships;
/// the attached arm is what T4-S6a's test pins.
pub(crate) fn composer_prompt(base: &str, attached: Option<u64>) -> String {
    match attached {
        Some(id) => format!("agent {id}> "),
        None => base.to_string(),
    }
}

/// [`read_line_inline`] with card T4-S2's key routes live.
///
/// The two routes are kept out of the editor entirely, and that separation is
/// the point rather than tidiness. `kitsu-editor`'s `history_prev` emits
/// `EditorAction::RecallRequest` only on an *empty, walk-free* buffer, and the
/// arm below declines it so `\u{2191}` walks history at the idle prompt. If the
/// `\u{2190}` route shared that empty-draft or recall state — by arming through
/// an editor action, or by consuming the key that follows — the first `\u{2191}`
/// would stop walking history, which is the regression a user notices
/// immediately. So the arming state is one local counter in *this* loop, fed
/// by [`KeyFilter`] from the raw byte stream, and the editor is never told that
/// a left arrow happened at all when the composer is empty (where backward-char
/// is a no-op anyway).
///
/// The counter is reset by **any** other item in the chunk, so the armed state
/// belongs to an uninterrupted run of `\u{2190}`s on an untouched empty
/// composer. Arming until the view opens would make a keystroke's meaning
/// depend on something the user did minutes ago and can no longer see.
///
/// # `pages`: the fullscreen paging sink, and why it is not an [`InlineRead`]
///
/// `Some(sink)` turns the four paging keys on and is where each press is
/// delivered; `None` leaves every byte of them to the editor, which is what
/// inline and classic pass. The gate is the sink's own presence rather than a
/// field on [`InlineKeys`] because the two are the same fact: a surface with
/// nowhere to send a press has no business lifting one out of the stream.
///
/// A press is **not** an [`InlineRead`] variant, although
/// [`InlineRead::Suspended`] and friends are, and the difference is that
/// those keystrokes hand the terminal to something else while a page press
/// does not. An outcome ends the read, and `read_chat_line_fullscreen`
/// re-enters with the draft as `initial`, which is replayed as *bytes*
/// through a `Cursor` — so the caret lands at the end of the draft, the
/// editor is new, the `KeyFilter` is new and any reverse search is gone.
/// Paging with the caret mid-draft would then move the caret on every press.
/// A sink keeps one read, one editor and one draft across a whole run of
/// presses.
#[allow(clippy::too_many_arguments)]
pub(crate) fn read_line_inline_with_keys<R: std::io::Read>(
    input: &mut R,
    paint: &mut dyn FnMut(&str, usize, &str, bool),
    history: &[String],
    on_resize: &mut dyn FnMut(),
    keys: InlineKeys,
    attached: Option<u64>,
    pages: Option<&mut dyn FnMut(kitsu_tui::fullscreen::Page)>,
    vertical: Option<kitsu_editor::VerticalMotion>,
) -> std::io::Result<InlineRead> {
    use kitsu_editor::{EditorAction, FeedOutcome, TypeaheadEditor};

    let mut pages = pages;

    /// The hint slot, derived exactly as `main.rs`'s `update_live_row`
    /// derives it for the classic row: the reverse-search status while Ctrl+R
    /// is active (reserved, so it keeps its columns against a long matched
    /// draft), the inline slash-completion suffix otherwise (unreserved).
    ///
    /// `search_status` already carries its own two-space separator, so the
    /// renderer must not add another one.
    ///
    /// `armed` — one `\u{2190}` seen and the next one would open the agent view
    /// — takes the slot, reserved, so the affordance keeps its columns.
    fn hint_of(editor: &TypeaheadEditor, armed: bool) -> (String, bool) {
        if armed {
            return (LEFT_ARM_HINT.to_string(), true);
        }
        let search_status = editor.search_status();
        let hint_reserved = search_status.is_some();
        let hint = search_status.unwrap_or_else(|| crate::idle_slash_hint(&editor.buffer_string()));
        (hint, hint_reserved)
    }

    let mut editor = TypeaheadEditor::new(history.to_vec());
    // Stage 1B: the framed composer's ↑/↓ walk its visual rows first. Only the
    // consumer that paints through the shared layout passes a motion.
    editor.set_vertical_motion(vertical);
    let mut filter = KeyFilter::new(keys);
    // The sink is the gate: with one, the four paging sequences are keys and
    // are lifted out of the stream; without one they are bytes like any
    // other.
    filter.pages = pages.is_some();
    // How many `\u{2190}`s of the current uninterrupted run have been seen. A
    // fresh read starts disarmed, so a press that opened the view cannot leak
    // into the next line.
    let mut left_presses = 0u8;
    // The first frame, before any byte arrives: the row must show the (possibly
    // empty) draft rather than whatever the previous paint left there.
    let (mut last_hint, mut last_reserved) = hint_of(&editor, false);
    paint(
        &editor.buffer_string(),
        editor.cursor(),
        &last_hint,
        last_reserved,
    );

    let mut buffer = [0u8; 1024];
    loop {
        let count = match input.read(&mut buffer) {
            // A descriptor that is done, not an empty raw read: the terminal
            // is configured VMIN=1, so zero bytes means end of input.
            Ok(0) => return Ok(InlineRead::Eof),
            Ok(count) => count,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {
                // Finding F16: a `SIGWINCH` is delivered by interrupting this
                // very read, so a wake is where the flag is checked. A wake
                // with no resize pending is some other signal and is retried
                // silently, without a frame.
                if take_resize() {
                    on_resize();
                    // The row's content is the live draft, not the status: the
                    // resize deliberately painted nothing, so the first frame
                    // at the new geometry is drawn here.
                    let (hint, hint_reserved) = hint_of(&editor, left_presses == 1);
                    paint(
                        &editor.buffer_string(),
                        editor.cursor(),
                        &hint,
                        hint_reserved,
                    );
                    last_hint = hint;
                    last_reserved = hint_reserved;
                }
                continue;
            }
            Err(error) => return Err(error),
        };
        // Fed verbatim *within each item*: the editor owns the keymap, the
        // escape-sequence state machine and the bracketed-paste block, so the
        // reader must not look for terminators itself (that would split a
        // paste at its newlines). With both routes off, `KeyFilter` yields one
        // `Bytes` item holding the whole chunk, so this is the pre-T4-S2 call.
        let mut changed = false;
        let mut actions: Vec<EditorAction> = Vec::new();
        let mut open_agent_view = false;
        for item in filter.feed(&buffer[..count]) {
            match item {
                KeyItem::Bytes(bytes) => {
                    // Any keystroke that is not a `\u{2190}` disarms.
                    left_presses = 0;
                    let outcome = editor.feed(&bytes);
                    changed |= outcome.changed;
                    actions.extend(outcome.actions);
                }
                KeyItem::Left => {
                    // Card **T4-S6a**: attachment is asked **before** the
                    // press count, and that order is the whole decision. A5
                    // makes `\u{2190}` on an empty composer the return path,
                    // and an implementation that counted presses first would
                    // turn `\u{2190}\u{2190}` in one read into a detach *and*
                    // a view. The rule is deliberately not gated by
                    // `left_arrow_opens_agents`: A1 scopes that key to the
                    // agent view's route alone, and A5 names this one with no
                    // condition. The carried draft is empty because this arm
                    // is only reached on an empty one -- which is true here
                    // and is *not* true of the chord below (finding F76).
                    if attached.is_some() && editor.buffer_string().is_empty() {
                        return Ok(InlineRead::Detached(String::new()));
                    }
                    // The composer's emptiness is asked *here*, after every
                    // earlier item in the chunk has been applied, so a
                    // `x<BS><left><left>` chunk is judged on the draft the
                    // backspace left behind and not on the one the chunk
                    // started with.
                    if keys.left_arrow_opens_agents && editor.buffer_string().is_empty() {
                        left_presses = left_presses.saturating_add(1);
                        if left_presses >= 2 {
                            left_presses = 0;
                            open_agent_view = true;
                            break;
                        }
                        // Nothing about the draft changed; the hint slot did,
                        // and the repaint below is driven off that.
                    } else {
                        // A non-empty draft: this is cursor movement and
                        // nothing else, so the bytes go to the editor exactly
                        // as they arrived and the affordance disarms.
                        left_presses = 0;
                        let outcome = editor.feed(LEFT_ARROW);
                        changed |= outcome.changed;
                        actions.extend(outcome.actions);
                    }
                }
                KeyItem::Open => {
                    left_presses = 0;
                    open_agent_view = true;
                    break;
                }
                // Requirement **R6**: the transcript overlay's route. The
                // reader stays pure for `KeyItem::Open`'s reason -- the
                // overlay takes the screen this reader paints on -- so the
                // keystroke is reported and the draft handed back, returning
                // *at* this item so bytes typed after it belong to the next
                // read. The draft is carried rather than dropped, which is
                // **F76**'s replay: the key must not cost the user a
                // character.
                KeyItem::OpenTranscript => {
                    // No disarm of the `\u{2190}\u{2190}` count, unlike every
                    // other arm: this one returns, so the whole reader state
                    // dies with it.
                    return Ok(InlineRead::OpenTranscript(editor.buffer_string()));
                }
                // Card **T4-S6a**: `<prefix> d`. With an agent attached this
                // is the detach route (`InlineRead::Detached`); with nothing
                // attached it is **inert and silent**, exactly as every other
                // chord letter is today -- a chord is a slip, and `/detach` is
                // where a deliberate detach gets an answer.
                // One paging press. It does **not** end the read: the draft,
                // its editor, its key filter and any reverse search have to
                // survive a whole run of presses, which is what a sink buys
                // over an outcome.
                KeyItem::Page(press) => {
                    left_presses = 0;
                    if let Some(sink) = pages.as_mut() {
                        sink(press);
                    }
                    // The frame the sink just repainted covered the composer
                    // row, and nothing about the draft changed, so this is the
                    // only thing that can put the row back -- the same reason
                    // Ctrl+L's `Redraw` arm sets it.
                    changed = true;
                }
                // Card **T4-S6-ordinal**: `<prefix> 1..9`. The reader stays
                // pure for `KeyItem::OpenTranscript`'s reason, and here it is
                // not merely tidiness: resolving a position needs the
                // registry, which this reader has no business holding. So the
                // **ordinal** is reported and the draft handed back (**F76**:
                // the chord has no empty-composer precondition), returning
                // *at* this item so bytes typed after it belong to the next
                // read. No disarm of the `\u{2190}\u{2190}` count, like the
                // transcript arm: this returns, so the reader's state dies
                // with it. There is no attachment precondition either --
                // unlike `KeyItem::Detach` -- because while attached the
                // chord *switches* agent.
                KeyItem::Attach(ordinal) => {
                    return Ok(InlineRead::AttachOrdinal(ordinal, editor.buffer_string()));
                }
                KeyItem::CycleAgent(delta) => {
                    return Ok(InlineRead::CycleAgent(delta, editor.buffer_string()));
                }
                KeyItem::Detach => {
                    left_presses = 0;
                    if attached.is_some() {
                        // Finding **F76**: the chord has no empty-composer
                        // precondition, so whatever is typed is carried back
                        // for the loop to replay -- `buffer_string` is the
                        // draft the next read starts from, exactly as the
                        // agent-view route below carries it.
                        return Ok(InlineRead::Detached(editor.buffer_string()));
                    }
                }
            }
        }
        if open_agent_view {
            return Ok(InlineRead::OpenAgentView(editor.buffer_string()));
        }
        for action in actions {
            match action {
                EditorAction::Submit(line) => return Ok(InlineRead::Submitted(line)),
                EditorAction::Eof => return Ok(InlineRead::Eof),
                EditorAction::Interrupt => return Ok(InlineRead::Interrupted),
                EditorAction::RecallRequest => {
                    // Up on an empty, walk-free draft offers the *during-turn*
                    // collector first refusal for the prompt queue. The idle
                    // reader has no queue to recall from, so it declines and
                    // takes the plain history walk; without this, Up would do
                    // nothing at all here.
                    let mut forced = FeedOutcome::default();
                    editor.history_prev_forced(&mut forced);
                    changed |= forced.changed;
                }
                // Finding F21: report the keystroke and hand the draft back,
                // returning *at* this action rather than draining the rest of
                // the chunk -- bytes typed after the Ctrl+Z belong to the
                // resumed read, not to this one. Nothing is raised and nothing
                // is painted: the signal and the terminal are the caller's.
                EditorAction::Suspend => return Ok(InlineRead::Suspended(editor.buffer_string())),
                // Tab accepts the inline slash hint, if one is showing --
                // parity with the during-turn collector's arm in `main.rs`.
                //
                // The hint is recomputed from the buffer here rather than
                // reusing `hint_of`: that helper returns the reverse-search
                // *status* while a search is armed, and the status is display
                // text (`  ⌕ reverse-search '..'`), so appending it would
                // type the indicator into the user's prompt. Actions are
                // processed after the whole `feed` returns, so a later Ctrl+R
                // in the same chunk can have armed the status by the time this
                // arm runs -- the difference is reachable, not theoretical.
                EditorAction::CompleteRequest => {
                    let buffer = editor.buffer_string();
                    let hint = crate::idle_slash_hint(&buffer);
                    if !hint.is_empty() {
                        editor.replace_buffer(&format!("{buffer}{hint}"));
                        // Set explicitly rather than leaning on the
                        // hint-difference repaint below: an accept that leaves
                        // a non-empty hint would not change the slot, and the
                        // row must still show the accepted draft.
                        changed = true;
                    }
                }
                // Ctrl+L (requirement R8): repaint the pinned row from the
                // editor's unchanged state. Routed through the loop's own
                // repaint rather than painting here so the row is composed
                // exactly once per chunk and from the state the *whole*
                // chunk left behind -- a paint inlined here would be
                // overwritten by the one below whenever the same read also
                // carried draft bytes. The editor reports `changed: false`
                // (nothing about the draft changed), so this is the only
                // thing that can answer the keystroke.
                EditorAction::Redraw => changed = true,
                // Ctrl+\ stays inert on purpose: its conventional action is to
                // re-raise SIGQUIT, which terminates *and dumps core*, and this
                // process holds API tokens and conversation text in memory --
                // writing those to disk because of a stray keystroke is worse
                // than the key doing nothing.
                EditorAction::Quit => {}
            }
        }
        // Repaint when the draft changed OR when the hint slot changed.
        //
        // The second half is not redundant. Entering and leaving reverse
        // search happens to set `outcome.changed` today, so the reserved
        // status would repaint without this -- but that is the editor's
        // goodwill, not a contract: an editor change that stops flagging a
        // search transition as a draft change would silently strand a stale
        // (or missing) status on the row, with nothing to catch it. Comparing
        // the painted `(hint, hint_reserved)` pair against the last one makes
        // the row's hint a function of editor state rather than of which
        // transitions the editor chooses to report.
        let (hint, hint_reserved) = hint_of(&editor, left_presses == 1);
        if changed || hint != last_hint || hint_reserved != last_reserved {
            paint(
                &editor.buffer_string(),
                editor.cursor(),
                &hint,
                hint_reserved,
            );
            last_hint = hint;
            last_reserved = hint_reserved;
        }
    }
}

/// The five terminal-and-process effects a Ctrl+Z suspend performs, behind a
/// seam so that their **order** is observable (finding F21).
///
/// The order is the whole defect. Stopping the process with the DECSTBM scroll
/// region still set, or with termios still raw, hands the shell back a
/// terminal whose scrolling area is a subset of the screen; and since the real
/// `stop_process` blocks inside `raise(SIGTSTP)` until the job is
/// foregrounded again, no end-to-end test can observe the sequence from
/// outside. A trait keeps the one property that matters -- which call comes
/// before the stop -- checkable without a terminal.
///
/// It is deliberately this small. It exists to make one call order observable,
/// not to abstract the terminal.
pub(crate) trait SuspendEffects {
    /// Clear the pinned rows and release the scroll region.
    fn release_viewport(&mut self);
    /// Put the terminal back in canonical mode with `ISIG` on.
    fn restore_terminal_modes(&mut self);
    /// `raise(SIGTSTP)`. Blocks until the job is foregrounded again.
    fn stop_process(&mut self);
    /// Re-engage raw input mode; `false` if it could not be re-engaged.
    fn reengage_terminal_modes(&mut self) -> bool;
    /// Set the region again and repaint the status row.
    fn reinstall_viewport(&mut self);
}

/// Run one suspend/resume cycle, returning whether the terminal came back.
///
/// Release the viewport, restore the terminal modes, stop, re-engage,
/// reinstall. Steps 1 and 2 **both** precede the stop: the region must be
/// gone and the terminal canonical before the process stops, or the shell
/// inherits a broken terminal. If re-engaging fails the viewport is not
/// reinstalled and `false` comes back, so the caller bails out of the read
/// rather than painting into a terminal it cannot read from.
///
/// Follow-up card note: an end-to-end PTY test of this is deliberately out of
/// scope here. Detecting that the child actually stopped needs
/// `waitpid(WNOHANG | WUNTRACED)`; the existing harness's `try_wait` does not
/// pass `WUNTRACED`, so it reports a stopped child as still running.
pub(crate) fn suspend_and_resume(effects: &mut dyn SuspendEffects) -> bool {
    effects.release_viewport();
    effects.restore_terminal_modes();
    effects.stop_process();
    if !effects.reengage_terminal_modes() {
        return false;
    }
    effects.reinstall_viewport();
    true
}

/// The shortest bordered palette that is still a box: the top border, one
/// content row, and the footer separator. Below this height there is no
/// palette at all -- a frame with no lid, or a lid with no frame, is worse
/// than nothing.
pub(crate) const PALETTE_MIN_ROWS: u16 = 3;

/// T0-S15: the bordered slash-command palette as **rows**, already sized to
/// the height the viewport will grant.
///
/// `buffer` is the composer's raw buffer, slash included: anything that is not
/// a single-token slash prefix (an empty buffer, ordinary text, a buffer with
/// whitespace or a newline) has no palette, exactly as `ChatHelper::hint`
/// decides in `main.rs`. The content itself comes from
/// [`crate::slash_palette_rows`] -- there is deliberately no second palette
/// renderer, because two that drift would be worse than one.
///
/// Returns at most `max_rows` complete lines, none of them newline-terminated,
/// where the first is always the top border and the last always the footer
/// separator. When `max_rows` cannot hold [`PALETTE_MIN_ROWS`] the answer is
/// **empty**: never a partial box. Height is taken out of the *entries*, and
/// the trailing "N more" row counts everything actually hidden, whether the
/// `SLASH_PALETTE_ROWS` cap or the height is what hid it.
pub(crate) fn palette_rows(
    buffer: &str,
    rich: bool,
    inner_width: usize,
    max_rows: u16,
) -> Vec<String> {
    if max_rows < PALETTE_MIN_ROWS {
        return Vec::new();
    }
    // Exactly `ChatHelper::hint`'s rule: a single-token slash prefix, no
    // newline, no whitespace anywhere after the slash.
    let Some(prefix) = buffer.strip_prefix('/') else {
        return Vec::new();
    };
    if buffer.contains('\n') || prefix.chars().any(char::is_whitespace) {
        return Vec::new();
    }

    // The one palette renderer, which the classic Rustyline hint joins at its
    // own height. Everything that is palette *text* -- the labels, the two
    // frame character sets, the `no matching command` line and the
    // row-selection step -- lives there, so there is nothing here to drift
    // from it.
    crate::slash_palette_rows(prefix, rich, inner_width, max_rows)
}

/// The inline composer's completion host.
///
/// This is the whole of the inline host: a delegate. Every candidate comes
/// from [`crate::ComposerCompletion`], which is also the classic Rustyline
/// `ChatHelper`'s only source, so the two cannot disagree -- there is nothing
/// here that could drift. The same holds for the re-hosted hinter and
/// highlighter below.
///
/// Nothing in production drives the inline composer yet -- that is a later
/// card -- so the only constructions are in the parity tests; the seam is
/// kept here, beside `palette_rows`, rather than in a test module.
#[allow(dead_code)]
pub(crate) struct InlineComposerHost {
    completion: crate::ComposerCompletion,
    /// Rich terminals get color; `NO_COLOR` keeps the composer unstyled.
    rich: bool,
}

impl InlineComposerHost {
    #[allow(dead_code)]
    pub(crate) fn new(completion: crate::ComposerCompletion, rich: bool) -> Self {
        Self { completion, rich }
    }
}

impl kitsu_tui::composer::CompletionSource for InlineComposerHost {
    fn complete(&self, line: &str, pos: usize) -> kitsu_tui::composer::Completion {
        self.completion.candidates(line, pos)
    }
}

impl kitsu_tui::composer::ComposerDecoration for InlineComposerHost {
    /// The inline row's hint is the **row** hint, not the classic multi-row
    /// deck: F20's second correction established that the palette and the
    /// row suffix are different things with different causes. The palette
    /// reaches the inline composer through [`palette_rows`] instead.
    fn hint(&self, line: &str) -> String {
        crate::idle_slash_hint(line)
    }

    fn highlight_hint(&self, hint: &str) -> String {
        crate::highlight_composer_hint(hint, self.rich)
    }
}

/// Classify one agent thread into the agent view's A2 group.
///
/// This is the **only** mapping from the thread lifecycle onto
/// [`AgentViewState`]: `Blocked` is the group that needs you, `Done` splits on
/// whether its handoff is still `Ready` (nobody has looked) and the two
/// terminal failures stay distinct. The order comes from
/// [`AgentViewState::order`] and is never restated here.
// T4-S2's overlay is this mapping's first non-test caller, so the deliberate
// `allow(dead_code)` the T2-S3 lifecycle used is gone with this card.
pub(crate) fn agent_view_state_of(
    phase: thread::AgentThreadPhase,
    handoff: thread::AgentHandoffState,
) -> kitsu_render::AgentViewState {
    use kitsu_render::AgentViewState;
    match phase {
        thread::AgentThreadPhase::Blocked => AgentViewState::NeedsInput,
        thread::AgentThreadPhase::Running => AgentViewState::Working,
        thread::AgentThreadPhase::Done => match handoff {
            // `Ready` is the one case where a result is still waiting to be
            // read. `Unavailable` is not an unviewed result: there is nothing
            // left to observe, so it joins the viewed group.
            thread::AgentHandoffState::Ready => AgentViewState::DoneUnviewed,
            thread::AgentHandoffState::Acknowledged | thread::AgentHandoffState::Unavailable => {
                AgentViewState::DoneViewed
            }
        },
        thread::AgentThreadPhase::Failed => AgentViewState::Failed,
        thread::AgentThreadPhase::Cancelled => AgentViewState::Cancelled,
    }
}

/// Map a validated agent-thread DTO onto one agent-view row (T4-S1).
///
/// **What the DTO cannot supply.** `AgentThreadSnapshot` carries no token
/// counts and no activity label — those live on `AgentTelemetry`, reached
/// through `AgentProgress`, which the DTO deliberately does not embed. So this
/// mapping leaves `tokens`, `last_activity` and `last_activity_age` as `None`
/// rather than inventing a zero, and a caller that has the telemetry fills
/// them in afterwards.
// T4-S2's overlay is this mapping's first non-test caller, so the deliberate
// `allow(dead_code)` the T2-S3 lifecycle used is gone with this card.
pub(crate) fn agent_row_from_thread_dto(
    dto: &thread::AgentThreadSnapshotDto,
) -> kitsu_tui::agents_view::AgentRow {
    let thread = &dto.thread;
    kitsu_tui::agents_view::AgentRow {
        state: agent_view_state_of(thread.phase, thread.handoff),
        id: thread.id,
        name: thread.name.clone(),
        task: thread.task.clone(),
        model: thread.model.clone(),
        profile: thread.profile.clone(),
        // Absent, not zero: a zero here would read as "no tokens used" and
        // "no activity", which is a different claim from "not known here".
        last_activity: None,
        last_activity_age: None,
        tokens: None,
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::sync::{Arc, Mutex};

    use crate::subagent::thread;
    use kitsu_render::live;
    use kitsu_render::surface::{active_surface, set_surface, Surface};
    use serial_test::serial;

    use super::{
        install, palette_rows, suspend_and_resume, BridgeGuard, SuspendEffects, TuiMode,
        PALETTE_MIN_ROWS, SIGNAL_RESTORE,
    };
    // Card T4-S6a's surfaces, in their own `use` so the list above is the
    // one the earlier cards wrote.
    use super::{
        composer_prompt, read_line_inline_with_keys, InlineKeys, KeyFilter, KeyItem, LEFT_ARROW,
    };
    // Card T5-S1c-iii: the view's own press type. The decoder's side is what
    // this crate pins; the anchor's eleven transitions are
    // `kitsu_tui::fullscreen`'s and are pinned there.
    use super::enter_fullscreen;
    use kitsu_tui::fullscreen::Page;
    // Card R6-c's prerequisite (**F102**): the prompt feed, the rebuild both
    // ring readers go through, and the second surface that keeps cells.
    use super::{frame_ring, note_submitted_prompt, FullscreenTap, PromptOrigin};

    /// A sink whose buffer outlives the surface that owns it, so bytes written
    /// through a `Box<dyn Write + Send>` stay observable after the surface is
    /// dropped. `Surface` is `Send + Sync`, hence `Arc<Mutex<..>>`.
    #[derive(Clone, Default)]
    struct SharedSink(Arc<Mutex<Vec<u8>>>);

    impl SharedSink {
        fn new() -> Self {
            Self::default()
        }

        fn bytes(&self) -> Vec<u8> {
            self.0.lock().expect("sink mutex").clone()
        }

        fn text(&self) -> String {
            String::from_utf8_lossy(&self.bytes()).into_owned()
        }

        fn is_empty(&self) -> bool {
            self.bytes().is_empty()
        }
    }

    impl Write for SharedSink {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().expect("sink mutex").extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// Read the live-row setting without changing it: `set_live_rows_enabled`
    /// returns the previous value, so setting it to anything and immediately
    /// putting back what came out is a non-mutating probe. There is no getter
    /// in the pinned API and this card does not add one.
    fn live_rows_enabled() -> bool {
        let previous = live::set_live_rows_enabled(false);
        live::set_live_rows_enabled(previous);
        previous
    }

    /// RAII restore for the **two** pieces of process-global state these tests
    /// touch: the surface slot and the live-row setting. Restoring in `Drop`
    /// means a panicking assertion cannot leak an injected surface into the
    /// rest of this test binary, where it would silently swallow every later
    /// transcript line, nor leave the live row disabled for other tests.
    ///
    /// Tests are additionally `#[serial]`: both pieces of state are
    /// process-wide, and `cargo test` runs this binary's tests on many threads,
    /// so two bridge tests overlapping would see each other's surface. This is
    /// the same hazard `kitsu-render`'s `surface_slot_lock` guards against
    /// there; here `serial_test` is already a dev-dependency, so it does the
    /// serialising.
    struct GlobalState {
        surface: Option<Arc<dyn Surface>>,
        live_rows: bool,
    }

    impl GlobalState {
        fn capture() -> Self {
            Self {
                surface: Some(active_surface()),
                live_rows: live_rows_enabled(),
            }
        }
    }

    impl Drop for GlobalState {
        fn drop(&mut self) {
            if let Some(surface) = self.surface.take() {
                set_surface(surface);
            }
            live::set_live_rows_enabled(self.live_rows);
        }
    }

    /// The bytes `InlineSurface::show` emits for this geometry, derived rather
    /// than hardcoded: scroll region `1..=bottom` and the cursor parked on the
    /// region's own last row, where `bottom = rows - viewport_rows`.
    fn expected_show(rows: u16, viewport_rows: u16) -> String {
        let bottom = rows - viewport_rows;
        format!("\x1b[1;{bottom}r\x1b[{bottom};1H")
    }

    /// The bytes a one-row `paint` emits for this geometry: position absolutely
    /// on the first viewport row, clear it, write the text, re-park.
    fn expected_status_paint(rows: u16, viewport_rows: u16, status: &str) -> String {
        let bottom = rows - viewport_rows;
        let row = bottom + 1;
        format!("\x1b[{row};1H\x1b[2K{status}\x1b[{bottom};1H")
    }

    /// Install over two fresh sinks, returning the guard and both sinks.
    fn install_inline(
        rows: u16,
        viewport_rows: u16,
        status: &str,
    ) -> (Option<BridgeGuard>, SharedSink, SharedSink) {
        install_inline_with_ring(rows, viewport_rows, DEFAULT_TEST_RING, status)
    }

    /// The cell-ring bound the helper above installs with: a bound every
    /// existing test is indifferent to, so only the tests that name a bound
    /// of their own depend on one.
    const DEFAULT_TEST_RING: usize = 64;

    /// As [`install_inline`], with the cell-ring bound spelled out.
    fn install_inline_with_ring(
        rows: u16,
        viewport_rows: u16,
        cell_ring_lines: usize,
        status: &str,
    ) -> (Option<BridgeGuard>, SharedSink, SharedSink) {
        let out = SharedSink::new();
        let err = SharedSink::new();
        let guard = install(
            TuiMode::Inline,
            rows,
            viewport_rows,
            cell_ring_lines,
            status,
            Box::new(out.clone()),
            Box::new(err.clone()),
        );
        (guard, out, err)
    }

    /// Test 2. Installing in inline mode writes the viewport's control bytes
    /// on the **err** sink — the live row's own descriptor — and nothing at all
    /// on stdout: the region set and cursor park, then the painted status row.
    #[test]
    #[serial]
    fn install_inline_emits_the_region_and_the_status_row_on_stderr() {
        let _state = GlobalState::capture();
        let (guard, out, err) = install_inline(24, 3, "status: idle");
        assert!(guard.is_some(), "inline mode installs a guard");

        let expected = expected_show(24, 3) + &expected_status_paint(24, 3, "status: idle");
        assert_eq!(err.text(), expected);
        assert!(
            out.is_empty(),
            "no viewport control bytes may reach stdout: {:?}",
            out.text()
        );
    }

    /// Test 3. The install actually redirects the renderer's seam:
    /// `kitsu_render::tool_activity` is a stderr transcript writer that routes
    /// through `active_surface`, so after installing its line must land in the
    /// **injected** err sink rather than the process's own stderr.
    ///
    /// `tool_activity` is the cleanest probe of the three stderr writers: it is
    /// public, takes a single string, and emits exactly one line, so the only
    /// thing the assertion can be sensitive to is where that line went.
    #[test]
    #[serial]
    fn install_inline_routes_a_stderr_transcript_line_into_the_injected_sink() {
        let _state = GlobalState::capture();
        let (guard, out, err) = install_inline(24, 3, "status: idle");
        let guard = guard.expect("inline mode installs a guard");
        let before = err.bytes().len();

        kitsu_render::tool_activity("read_file(src/main.rs)");

        let tail = err.text()[before..].to_string();
        assert!(
            tail.contains("read_file(src/main.rs)"),
            "the tool-activity line must reach the injected sink: {tail:?}"
        );
        assert!(
            tail.ends_with("\r\n"),
            "the viewport terminates transcript lines with CR LF: {tail:?}"
        );
        assert!(out.is_empty(), "a stderr line must not reach stdout");
        drop(guard);
    }

    /// Test 4. The F15 suppression, asserted on the setting itself rather than
    /// on an absence of bytes: installing disables the live row, and dropping
    /// the guard restores exactly the value that was there before.
    #[test]
    #[serial]
    fn install_inline_suppresses_the_live_row_and_the_guard_restores_it() {
        let _state = GlobalState::capture();
        let previous = live::set_live_rows_enabled(true);
        assert!(live_rows_enabled(), "precondition: the live row is enabled");

        let (guard, _out, _err) = install_inline(24, 3, "status: idle");
        let guard = guard.expect("inline mode installs a guard");
        assert!(
            !live_rows_enabled(),
            "the viewport owns the bottom rows, so the live row must stand down"
        );

        drop(guard);
        assert!(
            live_rows_enabled(),
            "dropping the guard restores the previous live-row setting"
        );

        // And the restore is of the *previous* value, not a hardcoded `true`.
        live::set_live_rows_enabled(false);
        let (guard, _out, _err) = install_inline(24, 3, "status: idle");
        let guard = guard.expect("inline mode installs a guard");
        assert!(!live_rows_enabled());
        drop(guard);
        assert!(
            !live_rows_enabled(),
            "a guard installed over a disabled live row leaves it disabled"
        );

        live::set_live_rows_enabled(previous);
    }

    /// Test 5. Dropping the guard releases the scroll region and puts the
    /// displaced surface back, so a transcript line written afterwards no
    /// longer reaches the injected sink.
    #[test]
    #[serial]
    fn dropping_the_guard_resets_the_region_and_restores_the_previous_surface() {
        let _state = GlobalState::capture();
        let (guard, _out, err) = install_inline(24, 3, "status: idle");
        let guard = guard.expect("inline mode installs a guard");

        let before = err.bytes().len();
        drop(guard);
        // Only that the region is released, not the exact teardown sequence:
        // `InlineSurface::hide` owns those bytes and pins them against its own
        // geometry, and this assertion used to duplicate them as a literal.
        // That snapshot broke the moment F18 made `hide` blank its rows and
        // park the cursor -- a change this test has no stake in. Assert the
        // bridge's own concern instead, which is that teardown happened at all.
        let tail = err.text()[before..].to_string();
        assert!(
            tail.contains("\x1b[r"),
            "the guard releases the DECSTBM region on the way out, got {tail:?}"
        );

        let after_drop = err.bytes().len();
        kitsu_render::tool_activity("read_file(src/main.rs)");
        assert_eq!(
            err.bytes().len(),
            after_drop,
            "the seam is restored, so nothing further reaches the injected sink: {:?}",
            err.text()[after_drop..].to_string()
        );
    }

    /// Test 6. The no-gate path is inert: classic mode installs nothing,
    /// leaves the surface slot pointing at whatever it already held, and does
    /// not touch the live-row setting.
    #[test]
    #[serial]
    fn install_classic_is_inert() {
        let _state = GlobalState::capture();
        let expected_live = live_rows_enabled();
        let before = active_surface();

        let out = SharedSink::new();
        let err = SharedSink::new();
        let guard = install(
            TuiMode::Classic,
            24,
            3,
            // A bound classic must not spend: it constructs no surface at all.
            DEFAULT_TEST_RING,
            "status: idle",
            Box::new(out.clone()),
            Box::new(err.clone()),
        );

        assert!(guard.is_none(), "classic mode installs no guard");
        assert!(
            Arc::ptr_eq(&before, &active_surface()),
            "classic mode must leave the active surface untouched"
        );
        assert_eq!(
            live_rows_enabled(),
            expected_live,
            "classic mode must leave the live-row setting untouched"
        );
        assert!(
            out.is_empty() && err.is_empty(),
            "and write no bytes at all"
        );
    }

    // ---------------------------------------------------------------------
    // T5-S1b: the cell ring goes live.
    //
    // Requirement **N4**. The ring is an opt-in field on the inline surface,
    // so making it live is one decision in one place: `install` builds the
    // viewport, and it is the only production constructor of one. These tests
    // therefore read the ring back through the guard that `install` returned,
    // which is the same window the overlay will use -- never from the
    // configuration, and never from the surface's bytes (finding **F71**).
    //
    // Classic is not re-pinned here: `install_classic_is_inert` above already
    // asserts that classic mode constructs no surface at all, and it now does
    // so while being handed a bound. That classic *cannot* construct a ring is
    // stronger than any of this and is not a test: `kitsu-render` has no
    // dependency on `kitsu-tui`, so the ring is unnameable from the classic
    // renderer at compile time.
    // ---------------------------------------------------------------------

    use kitsu_render::surface::Fd;
    use kitsu_tui::cells::CellRing;
    use kitsu_tui::fullscreen::FullscreenView;

    /// Write `lines` through whatever surface is installed right now, which is
    /// the seam the whole transcript takes: `kitsu-render`'s writers reach the
    /// surface slot, never the viewport directly.
    fn through_the_seam(lines: &[(Fd, &str)]) {
        let surface = active_surface();
        for (fd, text) in lines {
            surface.transcript_line(*fd, text);
        }
    }

    /// The ring's retained lines as `(fd, styled bytes)`, which is what every
    /// assertion below compares.
    fn retained(guard: &BridgeGuard) -> Vec<(Fd, String)> {
        guard
            .cell_lines()
            .expect("installing the inline viewport in production feeds a cell ring")
            .iter()
            .map(|line| (line.fd(), line.styled().to_string()))
            .collect()
    }

    /// **The ring is fed in production.** Installing the inline viewport --
    /// the one production construction of a surface -- builds a ring, and the
    /// lines that cross the seam afterwards are in it.
    ///
    /// Not vacuous twice over: the injected sink is asserted to have received
    /// both lines, so the writes really happened and the ring is being read
    /// after a transcript it could have missed; and the retained list is
    /// compared by value, so a ring that exists and retains nothing fails.
    #[test]
    #[serial]
    fn installing_the_inline_viewport_feeds_a_cell_ring() {
        let _state = GlobalState::capture();
        let (guard, out, err) = install_inline_with_ring(24, 1, 16, "status: idle");
        let guard = guard.expect("inline mode installs a guard");
        let err_before = err.bytes().len();

        through_the_seam(&[(Fd::Stdout, "assistant"), (Fd::Stderr, "tool")]);

        // Each line on its own descriptor, which is also why the ring is
        // fd-tagged: the two sinks are what the surface actually split.
        let err_tail = err.text()[err_before..].to_string();
        assert!(
            out.text().contains("assistant"),
            "precondition: the stdout line crossed the seam: {:?}",
            out.text()
        );
        assert!(
            err_tail.contains("tool"),
            "precondition: the stderr line crossed the seam: {err_tail:?}"
        );
        assert_eq!(
            retained(&guard),
            vec![
                (Fd::Stdout, "assistant".to_string()),
                (Fd::Stderr, "tool".to_string()),
            ],
            "the installed viewport's ring is N4's sole record of the transcript"
        );
    }

    /// **The configured bound reaches the ring.** A non-default
    /// `cell_ring_lines` produces a ring that evicts at exactly that bound, so
    /// the configuration key has a reader in fact and not only in name.
    ///
    /// Two different bounds in one test on purpose: a single bound is
    /// satisfied by any constant the install might hardcode, including
    /// `TuiConfig::DEFAULT_CELL_RING_LINES`, whereas two are satisfied only by
    /// a value that travelled from the caller.
    #[test]
    #[serial]
    fn the_configured_bound_is_the_bound_the_installed_ring_evicts_at() {
        let _state = GlobalState::capture();
        for bound in [2_usize, 3] {
            let (guard, _out, _err) = install_inline_with_ring(24, 1, bound, "status: idle");
            let guard = guard.expect("inline mode installs a guard");
            through_the_seam(&[
                (Fd::Stdout, "one"),
                (Fd::Stdout, "two"),
                (Fd::Stdout, "three"),
                (Fd::Stdout, "four"),
            ]);
            let kept = retained(&guard)
                .into_iter()
                .map(|(_, text)| text)
                .collect::<Vec<_>>();
            let expected = ["one", "two", "three", "four"][4 - bound..]
                .iter()
                .map(|text| (*text).to_string())
                .collect::<Vec<_>>();
            assert_eq!(
                kept, expected,
                "a ring installed with a bound of {bound} retains the newest {bound} lines"
            );
            drop(guard);
        }
    }

    /// **The evicted lines' bytes are retained nowhere.** The achievable
    /// byte-level claim of **F52**/**F84b**, computed from the ring's contents
    /// and never from the allocator: the retained bytes are the retained
    /// lines' and no others, so the sum is the same however many lines were
    /// pushed past the bound.
    #[test]
    #[serial]
    fn the_live_rings_retained_bytes_are_the_retained_lines_and_no_others() {
        let _state = GlobalState::capture();
        let mut sums = Vec::new();
        for pushes in [8_usize, 40] {
            let (guard, _out, _err) = install_inline_with_ring(24, 1, 4, "status: idle");
            let guard = guard.expect("inline mode installs a guard");
            let lines = (0..pushes)
                .map(|n| format!("line {n:04}"))
                .collect::<Vec<_>>();
            through_the_seam(
                &lines
                    .iter()
                    .map(|text| (Fd::Stdout, text.as_str()))
                    .collect::<Vec<_>>(),
            );
            let kept = retained(&guard);
            assert_eq!(kept.len(), 4, "the bound holds at {pushes} pushes");
            sums.push(kept.iter().map(|(_, text)| text.len()).sum::<usize>());
            drop(guard);
        }
        assert_eq!(
            sums[0], sums[1],
            "five times the traffic retains the same bytes: the evicted lines' \
             bytes are held nowhere"
        );
    }

    /// **A stored line is the surface's bytes verbatim, styling included**
    /// (N4, explicitly) -- on the *live* path, not only on the unit one. The
    /// overlay repaints these rows and the ring is the only copy, so a feed
    /// that stored the stripped form would decolour the transcript
    /// irreversibly.
    #[test]
    #[serial]
    fn the_live_cell_ring_stores_the_surfaces_styled_bytes_verbatim() {
        let _state = GlobalState::capture();
        let (guard, _out, _err) = install_inline_with_ring(24, 1, 16, "status: idle");
        let guard = guard.expect("inline mode installs a guard");

        through_the_seam(&[(Fd::Stderr, "\x1b[1mtool\x1b[0m")]);

        let lines = guard
            .cell_lines()
            .expect("installing the inline viewport in production feeds a cell ring");
        let line = lines.last().expect("the styled line was retained");
        assert_eq!(
            line.styled(),
            "\x1b[1mtool\x1b[0m",
            "the SGR bytes are stored as written: they can be stripped later \
             and never reconstructed"
        );
        assert_eq!(
            line.plain(),
            "tool",
            "and the stripped form is derived on demand, not stored"
        );
    }

    /// **The reader and the feed, together.** Each half can work while the
    /// pair does not, so this pins the composition: what the fullscreen view
    /// paints is the bytes the *installed* surface wrote, styling intact, in
    /// order.
    ///
    /// The view takes a `CellRing`, and the guard hands out the retained
    /// lines, so the ring the view reads is rebuilt here through
    /// `CellRing::push` -- the same call the surface's own feed makes, with the
    /// same `(fd, styled bytes)` the surface produced. Nothing in the
    /// round-trip can invent styling that the feed did not store, which is the
    /// property under test.
    #[test]
    #[serial]
    fn the_fullscreen_frame_paints_what_the_installed_surface_wrote() {
        let _state = GlobalState::capture();
        let (guard, _out, _err) = install_inline_with_ring(24, 1, 16, "status: idle");
        let guard = guard.expect("inline mode installs a guard");

        let written = [
            (Fd::Stdout, "plain row"),
            (Fd::Stderr, "\x1b[31mred row\x1b[0m"),
            (Fd::Stdout, "last row"),
        ];
        through_the_seam(&written);

        let lines = guard
            .cell_lines()
            .expect("installing the inline viewport in production feeds a cell ring");
        let mut ring = CellRing::new(16);
        for line in &lines {
            ring.push(line.fd(), line.styled());
        }
        let frame = FullscreenView::new().frame(&ring, 3, 80);
        assert_eq!(
            frame,
            written
                .iter()
                .map(|(_, text)| kitsu_render::fit_row_to_width(text, 80))
                .collect::<Vec<_>>(),
            "the view over the live ring shows the surface's own rows, styling \
             included, oldest first"
        );
        assert!(
            frame.iter().any(|row| row.contains("\x1b[31m")),
            "precondition: the frame really carries SGR bytes, so the equality \
             above is not between two stripped forms: {frame:?}"
        );
    }

    // ---------------------------------------------------------------------
    // T0-S5b: the inline idle-prompt reader.
    //
    // These tests are pure: a chunked in-memory reader stands in for stdin and
    // a closure stands in for the viewport, so none of them touch
    // process-global state and none of them need `#[serial]`. What they pin is
    // that bytes reach the shared `TypeaheadEditor` and that the paint callback
    // reflects the result — the keymap itself is already covered by
    // `kitsu-editor`'s own tests, so only a representative subset appears here.
    // ---------------------------------------------------------------------

    use super::{install_sigwinch_handler, note_resize, read_line_inline, take_resize, InlineRead};

    /// A reader that hands out one chunk per `read`, so a test can control how
    /// input is split across reads the way a real terminal does. Returns
    /// `Ok(0)` once drained, which the reader must treat as EOF.
    struct Chunks(std::collections::VecDeque<Vec<u8>>);

    impl Chunks {
        fn new<const N: usize>(chunks: [&[u8]; N]) -> Self {
            Self(chunks.iter().map(|chunk| chunk.to_vec()).collect())
        }

        /// One byte per `read`, which is what `RawStdinGuard` actually sets up:
        /// it writes `VMIN = 1`, so a keystroke is a read. Feeding a whole
        /// script as a single slice would make every per-keystroke paint
        /// assertion vacuous, so the default helper below uses this.
        fn bytewise(bytes: &[u8]) -> Self {
            Self(bytes.iter().map(|byte| vec![*byte]).collect())
        }
    }

    impl std::io::Read for Chunks {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let Some(chunk) = self.0.pop_front() else {
                return Ok(0);
            };
            assert!(
                chunk.len() <= buf.len(),
                "test chunk larger than the buffer"
            );
            buf[..chunk.len()].copy_from_slice(&chunk);
            Ok(chunk.len())
        }
    }

    /// Drive the reader over `bytes` with no history, returning the outcome and
    /// every `(draft, cursor)` the paint callback was given, in order.
    fn read_bytes(bytes: &[u8]) -> (InlineRead, Vec<(String, usize)>) {
        read_with_history(bytes, &[])
    }

    fn read_with_history(bytes: &[u8], history: &[String]) -> (InlineRead, Vec<(String, usize)>) {
        drive(Chunks::bytewise(bytes), history)
    }

    /// One paint frame: the **whole** widened seam the reader hands its
    /// painter -- `(draft, cursor, hint, hint_reserved)`, in the same order
    /// and with the same meanings as `kitsu_render::live::live_input_update`,
    /// so the inline row and the classic live row cannot drift in *what*
    /// they are told about a draft.
    type Frame = (String, usize, String, bool);

    /// Run the reader over an explicit chunking, keeping every frame.
    fn drive_frames(mut input: Chunks, history: &[String]) -> (InlineRead, Vec<Frame>) {
        let mut frames: Vec<Frame> = Vec::new();
        let outcome = {
            let mut paint = |draft: &str, cursor: usize, hint: &str, hint_reserved: bool| {
                frames.push((draft.to_string(), cursor, hint.to_string(), hint_reserved))
            };
            read_line_inline(&mut input, &mut paint, history, &mut || {}).expect("inline read")
        };
        (outcome, frames)
    }

    /// Run the reader over an explicit chunking, projecting each frame down to
    /// `(draft, cursor)` for the tests that predate the hint slot.
    fn drive(input: Chunks, history: &[String]) -> (InlineRead, Vec<(String, usize)>) {
        let (outcome, frames) = drive_frames(input, history);
        (
            outcome,
            frames
                .into_iter()
                .map(|(draft, cursor, _, _)| (draft, cursor))
                .collect(),
        )
    }

    /// The reader's frames for `bytes` with no history. Input ends without a
    /// terminator, so the read ends in `Eof` -- the frames are the subject.
    fn frames(bytes: &[u8]) -> Vec<Frame> {
        drive_frames(Chunks::bytewise(bytes), &[]).1
    }

    /// The last frame, which is the row as it stands when input runs out.
    fn last_frame(bytes: &[u8]) -> Frame {
        frames(bytes).pop().expect("at least one paint")
    }

    fn submitted(outcome: &InlineRead) -> &str {
        match outcome {
            InlineRead::Submitted(line) => line,
            other => panic!("expected a submission, got {other:?}"),
        }
    }

    /// Is `wanted` a subsequence of `paints`? The reader may coalesce or add
    /// repaints (an initial empty paint, a repaint after submit), so the
    /// assertions below pin the *order* of the interesting frames rather than
    /// an exact frame list.
    fn paints_in_order(paints: &[(String, usize)], wanted: &[(&str, usize)]) -> bool {
        let mut it = paints.iter();
        wanted
            .iter()
            .all(|(draft, cursor)| it.any(|(d, c)| d == draft && c == cursor))
    }

    /// Test 7. Printable bytes insert and a carriage return submits: a raw-mode
    /// terminal sends CR for Enter, never LF, so CR is the case that matters
    /// most. The submitted line excludes the terminator.
    #[test]
    fn inline_read_inserts_printables_and_submits_on_carriage_return() {
        let (outcome, paints) = read_bytes(b"/help\r");
        assert_eq!(submitted(&outcome), "/help");
        assert!(
            paints_in_order(&paints, &[("/", 1), ("/h", 2), ("/help", 5)]),
            "every keystroke must be painted with its cursor column: {paints:?}"
        );
    }

    /// Test 8. A line feed submits too: a scripted or `\n`-sending carrier must
    /// not stall waiting for a CR.
    #[test]
    fn inline_read_also_submits_on_a_line_feed() {
        let (outcome, _) = read_bytes(b"hi\n");
        assert_eq!(submitted(&outcome), "hi");
    }

    /// Test 9. A representative slice of the shared keymap reaches the editor
    /// through the reader: ←, Home/End, Backspace, Delete and the Ctrl+U/K/W
    /// kills. Each is checked by what it leaves in the submitted line, which is
    /// the editor's buffer, so this pins delivery rather than re-testing the
    /// keymap.
    #[test]
    fn inline_read_delivers_the_editor_keymap() {
        // Backspace (0x7f) deletes behind the cursor.
        assert_eq!(submitted(&read_bytes(b"abc\x7f\r").0), "ab");
        // Left arrow then an insert lands before the last char.
        assert_eq!(submitted(&read_bytes(b"ac\x1b[DB\r").0), "aBc");
        // Home (CSI H) then an insert goes to the front; End (CSI F) returns.
        assert_eq!(submitted(&read_bytes(b"bc\x1b[Ha\x1b[Fd\r").0), "abcd");
        // Delete (CSI 3~) removes ahead of the cursor.
        assert_eq!(submitted(&read_bytes(b"abc\x1b[H\x1b[3~\r").0), "bc");
        // Ctrl+U kills to the start, Ctrl+K to the end, Ctrl+W a word behind.
        assert_eq!(submitted(&read_bytes(b"abc\x15xyz\r").0), "xyz");
        assert_eq!(submitted(&read_bytes(b"abcdef\x1b[H\x1b[C\x0b\r").0), "a");
        assert_eq!(submitted(&read_bytes(b"one two\x17\r").0), "one ");
    }

    // ---- T5-S1c-iii: the four paging keys ----

    /// The bytes the four paging keys send, named once so no test below
    /// spells one of them twice.
    ///
    /// `PgUp`/`PgDn` are the `~`-terminated family (`ESC[5~`, `ESC[6~`);
    /// `Ctrl+Home`/`Ctrl+End` are the **modified** forms of the same finals
    /// plain `Home`/`End` send, which is the collision this card settles.
    const PG_UP: &[u8] = b"\x1b[5~";
    const PG_DN: &[u8] = b"\x1b[6~";
    const CTRL_HOME: &[u8] = b"\x1b[1;5H";
    const CTRL_END: &[u8] = b"\x1b[1;5F";
    /// Plain `Home` and `End`, which the editor's `dispatch_csi` reads as
    /// `move_home`/`move_end` on the bare final byte -- and reads *the same
    /// way* for the `1;5` parameter today, because its `b'H'`/`b'F'` arms
    /// ignore the parameters its `b'C'`/`b'D'` arms inspect. Measured: a
    /// `TypeaheadEditor` fed `"abc"` then `ESC[1;5H` reports
    /// `(buffer "abc", cursor 0, changed true)`.
    const PLAIN_HOME: &[u8] = b"\x1b[H";
    const PLAIN_END: &[u8] = b"\x1b[F";

    /// Drive the reader over `input` **with** a paging sink, returning the
    /// outcome, every press the sink was handed in order, and every frame.
    fn read_paging(input: Chunks) -> (InlineRead, Vec<Page>, Vec<Frame>) {
        let mut input = input;
        let mut presses: Vec<Page> = Vec::new();
        let mut frames: Vec<Frame> = Vec::new();
        let outcome = {
            let mut paint = |draft: &str, cursor: usize, hint: &str, hint_reserved: bool| {
                frames.push((draft.to_string(), cursor, hint.to_string(), hint_reserved));
            };
            let mut sink = |press: Page| presses.push(press);
            read_line_inline_with_keys(
                &mut input,
                &mut paint,
                &[],
                &mut || {},
                InlineKeys::disabled(),
                None,
                Some(&mut sink),
                None,
            )
            .expect("inline read")
        };
        (outcome, presses, frames)
    }

    /// The same, bytewise: `RawStdinGuard` writes `VMIN = 1`, so a keystroke
    /// is a read and an escape sequence is four or six of them.
    fn read_paging_bytewise(bytes: &[u8]) -> (InlineRead, Vec<Page>, Vec<Frame>) {
        read_paging(Chunks::bytewise(bytes))
    }

    /// Everything after a submission, as one byte string, for the tests that
    /// script several sequences in a row.
    fn script(parts: &[&[u8]]) -> Vec<u8> {
        parts.iter().flat_map(|part| part.iter().copied()).collect()
    }

    // ---- R6: Ctrl+O, the transcript overlay's route ----

    /// `Ctrl+O`, as a terminal sends it: one literal control byte, which is
    /// why it has no partial-sequence case the way the paging keys do.
    const CTRL_O: &[u8] = b"\x0f";

    /// **Test: `Ctrl+O` ends the read as `OpenTranscript` and carries the
    /// draft.**
    ///
    /// Measured first (**F99**), because the obvious assertion is green
    /// today and proves nothing: fed `abc` then `0x0f`, the reader reports
    /// `Eof` with frames `[("",0),("a",1),("ab",2),("abc",3)]` -- the byte
    /// reaches the editor, whose `0x00..=0x1f` arm drops it, so there is **no
    /// fourth frame** and the draft and caret are already untouched. "Ctrl+O
    /// leaves the draft alone" is therefore satisfied by the key not
    /// existing.
    ///
    /// What is red is the pair: the outcome, and the draft *carried on it*.
    /// Asserting them together is also what pins the replay -- an
    /// `OpenTranscript(String::new())` would lose the user's three
    /// characters, which is the **F76** shape the agent-view chord already
    /// pays for.
    ///
    /// The route runs under `InlineKeys::disabled()`, which is the design
    /// claim: R6 spells `Ctrl+O`, so unlike the agent view's `<prefix> a`
    /// this key is **not** behind the configurable chord prefix and is not
    /// switchable off with it.
    #[test]
    fn ctrl_o_ends_the_read_as_open_transcript_and_carries_the_draft() {
        let typed = script(&[b"abc", CTRL_O]);
        let (outcome, frames) = read_bytes(&typed);
        assert_eq!(
            outcome,
            InlineRead::OpenTranscript("abc".to_string()),
            "Ctrl+O opens the transcript overlay and hands the draft back for \
             the caller to replay"
        );
        assert_eq!(
            frames.last(),
            Some(&("abc".to_string(), 3)),
            "and the key put nothing in the draft and moved no caret: the \
             last frame is still the three typed characters"
        );
    }

    /// **Test: `Ctrl+O` inside a bracketed paste does not open the overlay.**
    ///
    /// The hazard the paging keys have a test for: inside a paste the payload
    /// is *text*, and a control byte in pasted content must not be read as a
    /// key.
    ///
    /// Measured, and it corrected this test: the editor does **not** take a
    /// pasted `0x0f` as content either -- fed `ESC[200~ a 0x0f b ESC[201~`
    /// it reports frames `[("",0),("a",1),("ab",2)]`, because its in-paste
    /// arm drops `0x00..=0x1f` to keep C1 controls out of the draft. So the
    /// claim is not "a pasted Ctrl+O is content"; that is a thing this system
    /// never does, and asserting it would be a specification of a fixture
    /// (**F61**). The claim is that it is **not a key**: the paste is still a
    /// paste, and `ab` is still the draft.
    ///
    /// Not vacuous, and specifically not green today: the paste half alone is
    /// satisfied by `Ctrl+O` not existing, so the same byte is fed again
    /// **outside** the paste in the same read, where it must open the
    /// overlay. A filter that recognised `Ctrl+O` everywhere fails the first
    /// assertion; one that recognised it nowhere fails the second.
    #[test]
    fn ctrl_o_is_not_a_key_inside_a_bracketed_paste() {
        let pasted = script(&[b"\x1b[200~", b"a", CTRL_O, b"b", b"\x1b[201~"]);
        let (outcome, frames) = read_bytes(&pasted);
        assert_ne!(
            outcome,
            InlineRead::OpenTranscript("ab".to_string()),
            "a pasted Ctrl+O must not open the overlay"
        );
        assert_eq!(
            frames.last(),
            Some(&("ab".to_string(), 2)),
            "and the paste is still the paste it was"
        );

        let then_typed = script(&[b"\x1b[200~", b"a", CTRL_O, b"b", b"\x1b[201~", CTRL_O]);
        let (outcome, _) = read_bytes(&then_typed);
        assert_eq!(
            outcome,
            InlineRead::OpenTranscript("ab".to_string()),
            "the same byte after the paste ends is the key again, carrying \
             the pasted draft"
        );
    }

    /// **Test: the four presses reach the sink, and none of them touches the
    /// draft.**
    ///
    /// The card's core. Measured first, because a test derived from a
    /// description of the defect can be satisfied by the defect (**F99**):
    /// `ESC[5~` and `ESC[6~` are *already* inert in the editor -- its
    /// `dispatch_csi` `b'~'` arm has no `5` or `6` parameter and falls through
    /// to `_ => {}` -- so "`PgUp` leaves the draft alone" is green today and
    /// proves nothing on its own. What is red today is both halves together:
    /// the sink is handed nothing at all, and `Ctrl+Home`/`Ctrl+End` *do*
    /// move the caret, to 0 and back to 3.
    ///
    /// So the assertion is the pair -- the exact press list **and** the draft
    /// with its caret where the user left it.
    #[test]
    fn fullscreen_paging_keys_reach_the_sink_and_leave_the_draft_alone() {
        // `abc` then one left arrow: the caret sits at column 2, which is
        // where a `Ctrl+Home` that leaked to the editor would not leave it.
        let typed = script(&[b"abc\x1b[D", PG_UP, PG_DN, CTRL_HOME, CTRL_END]);
        let (outcome, presses, frames) = read_paging_bytewise(&typed);

        assert_eq!(
            presses,
            vec![Page::Up, Page::Down, Page::Oldest, Page::Newest],
            "each of the four sequences must be lifted out of the byte stream \
             and delivered to the view's own press type, in the order typed"
        );

        let (draft, cursor, _, _) = frames.last().expect("at least one paint").clone();
        assert_eq!(
            (draft.as_str(), cursor),
            ("abc", 2),
            "and none of them may reach the editor: a paging key moves the \
             frame, never the caret. `Ctrl+Home` is `ESC[1;5H` and plain \
             `Home` is `ESC[H`, and the editor's `b'H'` arm ignores the \
             parameters -- so today the press lands on column 0. Frames: \
             {frames:?}"
        );
        assert!(
            matches!(outcome, InlineRead::Eof),
            "a press must not end the read: the draft and its editor have to \
             survive a whole run of them, got {outcome:?}"
        );
    }

    /// **Test: the plain forms still move the caret.**
    ///
    /// The other half of the collision, and the half a decoder that matched
    /// on the final byte alone would break. **F65**'s family: an arrow key is
    /// not one byte, and `ESC[1;5H` and `ESC[H` differ only in the
    /// parameters. This is the existing keymap test's `Home`/`End` line
    /// asserted again *with paging switched on*, which is the configuration
    /// it was never asserted under.
    #[test]
    fn fullscreen_paging_leaves_plain_home_and_end_as_cursor_movement() {
        let typed = script(&[b"bc", PLAIN_HOME, b"a", PLAIN_END, b"d\r"]);
        let (outcome, presses, _) = read_paging_bytewise(&typed);

        assert_eq!(
            submitted(&outcome),
            "abcd",
            "plain `Home` (`ESC[H`) and plain `End` (`ESC[F`) are cursor \
             movement on this surface too: only the **modified** form pages"
        );
        assert!(
            presses.is_empty(),
            "and neither of them is a press: a decoder that keyed on the \
             final byte `H`/`F` would page here and lose the caret, got \
             {presses:?}"
        );
    }

    /// **Test: the modified forms do not reach the editor.**
    ///
    /// The collision's red half, asserted on its own so it does not hide
    /// behind the press list: this one fails today at the *caret* and would
    /// still fail for an implementation that delivered the press and let the
    /// bytes through as well. Measured: `ESC[1;5H` on `"abc"` leaves
    /// `(buffer "abc", cursor 0, changed true)`, because `dispatch_csi`'s
    /// `b'H'` arm takes no parameters -- unlike its `b'C'`/`b'D'` arms, which
    /// match `1;5` for the word jumps.
    #[test]
    fn ctrl_home_and_ctrl_end_must_not_reach_the_editor_in_fullscreen() {
        for sequence in [CTRL_HOME, CTRL_END] {
            // `abc` then one left arrow parks the caret at column 2, which is
            // neither end: a leak to `move_home` lands on 0 and a leak to
            // `move_end` lands on 3.
            let typed = script(&[b"abc\x1b[D", sequence]);
            let (_, _, frames) = read_paging_bytewise(&typed);
            let (draft, cursor, _, _) = frames.last().expect("at least one paint").clone();
            assert_eq!(
                (draft.as_str(), cursor),
                ("abc", 2),
                "{} must move the frame and not the caret; frames: {frames:?}",
                String::from_utf8_lossy(sequence).escape_debug()
            );
        }
    }

    /// **Test: every press in a run is delivered, once each.**
    ///
    /// **F97**: a test that proves a press happened does not prove presses
    /// keep happening, and the winning assertion there was a **count**. A
    /// one-shot latch, a press coalesced with its neighbour, or a sink armed
    /// only on the first key of a chunk all satisfy "`PgUp` pages" and fail
    /// here.
    ///
    /// Five `PgUp`s then five `PgDn`s, bytewise, so each is its own `read`
    /// and the `KeyFilter` has to hold four partial sequences per press.
    #[test]
    fn a_run_of_page_presses_reaches_the_sink_once_each() {
        let mut typed: Vec<u8> = Vec::new();
        for _ in 0..5 {
            typed.extend_from_slice(PG_UP);
        }
        for _ in 0..5 {
            typed.extend_from_slice(PG_DN);
        }
        let (_, presses, _) = read_paging_bytewise(&typed);

        assert_eq!(
            presses.len(),
            10,
            "ten keystrokes are ten presses: {presses:?}"
        );
        assert_eq!(
            presses,
            [Page::Up; 5]
                .into_iter()
                .chain([Page::Down; 5])
                .collect::<Vec<_>>(),
            "in order, and with no press standing in for the one before it"
        );
    }

    /// **Test: a press split across reads is still one press.**
    ///
    /// **F65** by name: an arrow key is not one byte, and the overlay's close
    /// key ate the first arrow for exactly this reason. `ESC[1;5H` is six
    /// bytes and a terminal is free to deliver them in any grouping, so the
    /// chunking the `read` happens to see must not change the meaning --
    /// which is the discipline `KeyFilter::is_partial` already applies to
    /// `ESC[D` and to the paste delimiters.
    ///
    /// The payload bytes must also not leak: a filter that gave up on a
    /// partial would hand `5~` or `1;5H` to the editor as **text**.
    #[test]
    fn a_page_press_split_across_reads_is_still_one_press() {
        let (outcome, presses, frames) = read_paging(Chunks::new([
            b"a", b"\x1b", b"[5", b"~", b"\x1b[1", b";5H", b"\r",
        ]));

        assert_eq!(
            presses,
            vec![Page::Up, Page::Oldest],
            "two keystrokes split four ways are still two presses"
        );
        assert_eq!(
            submitted(&outcome),
            "a",
            "and not one byte of either sequence may reach the draft as text: \
             a filter that abandoned a partial would type `5~` or `1;5H` into \
             the user's line. Frames: {frames:?}"
        );
    }

    /// **Test: a paging sequence inside a bracketed paste is content.**
    ///
    /// The rule `KeyFilter` already states for the chord byte and for
    /// `ESC[D`, applied to the new sequences: inside a paste the payload is
    /// *text*, so a pasted transcript carrying `ESC[5~` must not page the
    /// frame. The editor's own `dispatch_csi` honours only the paste
    /// terminator while `in_paste`, so the sequences are dropped there
    /// rather than inserted -- what is asserted here is the press count,
    /// which is the thing a paste could wrongly produce.
    #[test]
    fn a_paged_sequence_inside_a_paste_is_content_and_not_a_press() {
        let typed = script(&[b"\x1b[200~x", PG_UP, CTRL_END, b"y\x1b[201~\r"]);
        let (outcome, presses, _) = read_paging_bytewise(&typed);

        assert!(
            presses.is_empty(),
            "a paste's payload is text: pasting a captured transcript must \
             not move the frame, got {presses:?}"
        );
        assert_eq!(
            submitted(&outcome),
            "xy",
            "and the paste's own characters still land in the draft"
        );
    }

    /// The ring's retained lines as `(is a prompt, stripped text)`, which is
    /// the pair the prompt-feed tests below compare: a mark with no text
    /// beside it would pass for a feed that marked an entry it never stored.
    fn marked(guard: &BridgeGuard) -> Vec<(bool, String)> {
        guard
            .cell_lines()
            .expect("installing the inline viewport in production feeds a cell ring")
            .iter()
            .map(|line| (line.is_prompt(), line.plain()))
            .collect()
    }

    /// A surface that only records what it is handed, for the tap test below:
    /// the tap's job is to *see* bytes, so its inner surface must be one whose
    /// writes are observable without a terminal.
    #[derive(Clone, Default)]
    struct SurfaceSpy(Arc<Mutex<Vec<(Fd, String)>>>);

    impl SurfaceSpy {
        fn written(&self) -> Vec<(Fd, String)> {
            self.0.lock().expect("spy").clone()
        }
    }

    impl Surface for SurfaceSpy {
        fn transcript_line(&self, fd: Fd, text: &str) {
            self.0.lock().expect("spy").push((fd, text.to_string()));
        }
    }

    /// **Test: a submitted prompt reaches the installed surface's cell ring**
    /// (**F102**). The storage half can work while nothing feeds it, so this
    /// pins the *feed*: the ring is **N4**'s sole source for the `Ctrl+O`
    /// overlay, and a prompt that never arrives there is a transcript with one
    /// speaker no matter what `CellLine` can represent.
    ///
    /// Driven through `note_submitted_prompt` -- the function `run_chat` calls
    /// at the one point every route a submitted line can take converges -- and
    /// not through a hand-built surface, because the property is that the
    /// *installed* surface answers it (**F69**: drive the real path, not a
    /// predicate beside it).
    ///
    /// Not vacuous: a transcript line is written through the same seam first,
    /// so the ring is being read after traffic it could have missed, and the
    /// retained list is compared by value, so a ring that exists and retains
    /// no prompt fails.
    #[test]
    #[serial]
    fn a_submitted_prompt_reaches_the_installed_surfaces_cell_ring() {
        let _state = GlobalState::capture();
        let (guard, _out, _err) = install_inline_with_ring(24, 1, 16, "status: idle");
        let guard = guard.expect("inline mode installs a guard");

        through_the_seam(&[(Fd::Stderr, "* session ready")]);
        note_submitted_prompt("explain the ring", PromptOrigin::User);

        assert_eq!(
            marked(&guard),
            vec![
                (false, "* session ready".to_string()),
                (true, "explain the ring".to_string()),
            ],
            "the installed surface retains the user's line, marked the user's"
        );
    }

    /// **Test: and it lands there before the answer it provoked.** The overlay
    /// repaints the ring in order, so a prompt appended after the turn it
    /// started would sit beside the wrong answer -- and the answer streams in
    /// while nothing else is being read, which is precisely why the order must
    /// be pinned rather than assumed.
    ///
    /// The pin is structural: `note_submitted_prompt` is called on the
    /// single-threaded path between the read and the turn, so this asserts
    /// that the prompt *precedes* the stream, not that it races it well.
    #[test]
    #[serial]
    fn a_submitted_prompt_lands_in_the_ring_before_the_answer_it_provoked() {
        let _state = GlobalState::capture();
        let (guard, _out, _err) = install_inline_with_ring(24, 1, 16, "status: idle");
        let guard = guard.expect("inline mode installs a guard");

        note_submitted_prompt("what is N4?", PromptOrigin::User);
        // The turn's own output, through the seam the whole transcript takes.
        through_the_seam(&[
            (Fd::Stderr, "* thinking"),
            (Fd::Stdout, "N4 bounds the ring."),
        ]);

        assert_eq!(
            marked(&guard),
            vec![
                (true, "what is N4?".to_string()),
                (false, "* thinking".to_string()),
                (false, "N4 bounds the ring.".to_string()),
            ],
            "the prompt is in the ring ahead of the first byte of its answer"
        );
    }

    /// **Test: and noting it writes no bytes on either descriptor** -- the
    /// opposite failure, and the expensive one. The composer erases the
    /// submitted row, so feeding the prompt as a `transcript_line` would put a
    /// line on the terminal that the inline renderer's contract says is not
    /// there (**R3**: one write per transcript line) and change the byte stream
    /// of every session, which is the shape **F92** was rejected for.
    ///
    /// **F71**: asserted on the painted bytes, both sinks, not on a flag.
    #[test]
    #[serial]
    fn noting_a_submitted_prompt_writes_no_bytes_on_either_descriptor() {
        let _state = GlobalState::capture();
        let (guard, out, err) = install_inline_with_ring(24, 1, 16, "status: idle");
        let _guard = guard.expect("inline mode installs a guard");
        let out_before = out.bytes();
        let err_before = err.bytes();

        note_submitted_prompt("paste-looking secret: hunter2", PromptOrigin::User);

        assert_eq!(
            out.bytes(),
            out_before,
            "the prompt is recorded, never written: stdout is untouched"
        );
        assert_eq!(
            err.bytes(),
            err_before,
            "and so is stderr, where the viewport's own bytes go"
        );
    }

    /// **Test: a slash command is retained as the user's prompt.** A
    /// transcript showing `/compact`'s report with no visible `/compact` is
    /// the same half-feature this card exists to fix, and the command is
    /// already kept in `history`. The feed must therefore **not** grow a
    /// `starts_with('/')` exclusion -- which is the tempting one, and the one
    /// this test forbids.
    ///
    /// The slash-dispatch arm in `run_chat` is downstream of the feed, so
    /// retaining it costs nothing and changes no dispatch.
    #[test]
    #[serial]
    fn a_slash_command_is_retained_as_the_users_own_prompt() {
        let _state = GlobalState::capture();
        let (guard, _out, _err) = install_inline_with_ring(24, 1, 16, "status: idle");
        let guard = guard.expect("inline mode installs a guard");

        note_submitted_prompt("/compact", PromptOrigin::User);
        through_the_seam(&[(Fd::Stderr, "* compacted 12 messages")]);

        assert_eq!(
            marked(&guard),
            vec![
                (true, "/compact".to_string()),
                (false, "* compacted 12 messages".to_string()),
            ],
            "the command the user typed is theirs, and sits above its report"
        );
    }

    /// **Test: an injected priority command is never marked the user\'s.**
    ///
    /// `run_chat` synthesizes `/resume <id>` after a fork and `/retry` on its
    /// own, and both join the same `line` binding a typed line does. Marking
    /// them would put a false claim inside the one record the overlay reads,
    /// and a transcript is a document: **F77**. That `add_history_entry` has
    /// carried the same quirk for longer is not a licence to carry it onto a
    /// new surface -- inherited is not justified.
    ///
    /// Asserted as "**no** retained line is the user\'s", which is true under
    /// either reading of the exclusion -- the injected command left out of the
    /// ring entirely, or retained unmarked beside its output. The decision was
    /// "do not mark", so that is exactly what this pins and nothing more
    /// (**F61**).
    #[test]
    #[serial]
    fn an_injected_priority_command_is_never_marked_the_users_prompt() {
        let _state = GlobalState::capture();
        let (guard, _out, _err) = install_inline_with_ring(24, 1, 16, "status: idle");
        let guard = guard.expect("inline mode installs a guard");

        // Both of `run_chat`'s injections, and `/retry` is deliberately one a
        // user could also type: the provenance is the parameter's, never the
        // text's.
        note_submitted_prompt("/resume 7f3a", PromptOrigin::Injected);
        note_submitted_prompt("/retry", PromptOrigin::Injected);
        through_the_seam(&[(Fd::Stderr, "* retrying the last turn")]);

        let marks = marked(&guard);
        assert!(
            marks.iter().all(|(is_prompt, _)| !is_prompt),
            "kitsu's own words are never the user's in the transcript: {marks:?}"
        );
    }

    /// **Test: a prompt the user broke across lines with `Alt+Enter` is one
    /// marked cell per line** -- driven from the **bytes**, through the real
    /// decoder and the real feed, not from a string a test wrote (**F69**).
    ///
    /// `ESC CR` is `Alt+Enter`, which the editor inserts as a literal newline
    /// (`kitsu-editor` lib.rs:336) rather than submitting; the trailing `CR`
    /// submits. Measured before asserting (**F99/F100**): the reader returns
    /// `"first line\nsecond line"` for these bytes today.
    #[test]
    #[serial]
    fn an_alt_enter_prompt_reaches_the_ring_as_one_marked_cell_per_line() {
        let _state = GlobalState::capture();
        let (guard, _out, _err) = install_inline_with_ring(24, 1, 16, "status: idle");
        let guard = guard.expect("inline mode installs a guard");

        let outcome = read_bytes(b"first line\x1b\rsecond line\r").0;
        assert_eq!(
            submitted(&outcome),
            "first line\nsecond line",
            "precondition: Alt+Enter drafted one line with an embedded newline"
        );
        note_submitted_prompt(submitted(&outcome), PromptOrigin::User);

        assert_eq!(
            marked(&guard),
            vec![
                (true, "first line".to_string()),
                (true, "second line".to_string()),
            ],
            "one cell per line, every one the user's: a cell with an embedded \
             newline is painted as one row the frame cannot fit"
        );
    }

    /// **Test: and so is a prompt pasted with newlines in it** -- the other
    /// input path, because a rule is only as wide as the narrowest input that
    /// feeds it (**F70**).
    ///
    /// Inside `ESC[200~` ... `ESC[201~` the payload is *text*, so its newlines
    /// insert instead of submitting, and the editor collapses `CR LF` into one
    /// newline (`kitsu-editor` lib.rs:126). Measured before asserting
    /// (**F99/F100**): this paste and its `LF`-only twin both draft
    /// `"first line\nsecond line"`, so the `\r` cannot reach a cell and no
    /// assertion here depends on one.
    #[test]
    #[serial]
    fn a_pasted_multi_line_prompt_reaches_the_ring_as_one_marked_cell_per_line() {
        let _state = GlobalState::capture();
        let (guard, _out, _err) = install_inline_with_ring(24, 1, 16, "status: idle");
        let guard = guard.expect("inline mode installs a guard");

        let outcome = read_bytes(b"\x1b[200~first line\r\nsecond line\x1b[201~\r").0;
        assert_eq!(
            submitted(&outcome),
            "first line\nsecond line",
            "precondition: the paste drafted one line with an embedded newline"
        );
        note_submitted_prompt(submitted(&outcome), PromptOrigin::User);

        assert_eq!(
            marked(&guard),
            vec![
                (true, "first line".to_string()),
                (true, "second line".to_string()),
            ],
            "a pasted block is split and marked exactly as an Alt+Enter one is"
        );
    }

    /// **Test: the frame's ring keeps the prompt marks of the lines it
    /// repacks.** `frame_ring` is the chokepoint *both* readers go through --
    /// the fullscreen frame and the `Ctrl+O` overlay -- and it rebuilds the
    /// ring from `(fd, styled)` alone, so a mark stored faithfully by the feed
    /// is dropped here and the overlay sees one speaker anyway.
    ///
    /// This is the half that a correct feed does not fix. It is asserted on
    /// the repacked ring's own lines, which is what a reader is handed.
    #[test]
    fn frame_ring_keeps_the_prompt_marks_of_the_lines_it_repacks() {
        let mut source = kitsu_tui::cells::CellRing::new(8);
        source.push_prompt("explain the ring");
        source.push(Fd::Stdout, "the ring is bounded");
        source.push_prompt("by what?");

        let repacked = frame_ring(&source.lines());

        assert_eq!(
            repacked
                .lines()
                .iter()
                .map(|line| (line.is_prompt(), line.plain()))
                .collect::<Vec<_>>(),
            vec![
                (true, "explain the ring".to_string()),
                (false, "the ring is bounded".to_string()),
                (true, "by what?".to_string()),
            ],
            "the frame and the overlay both read through this rebuild: a mark \
             lost here is lost to every reader"
        );
    }

    /// **Test: the fullscreen tap records a submitted prompt too, and
    /// forwards no bytes for it.** The tap's ring is the *only* ring while a
    /// fullscreen segment holds the screen -- `install` is inert in that mode,
    /// so there is no inline surface behind it -- which makes this a second
    /// surface the feed must reach, and `Surface::note_prompt`'s default is to
    /// do nothing. An overlay that shows prompts inline and not in fullscreen
    /// is **F89**/**F84** in the direction that bites: the key works on one
    /// surface and quietly does nothing on the other.
    ///
    /// The displaced surface is asserted untouched as well, because the tap's
    /// every other method forwards, and forwarding this one would write the
    /// prompt onto the alternate screen the composer just cleared it from.
    #[test]
    fn the_fullscreen_tap_records_a_submitted_prompt_without_forwarding_bytes() {
        let spy = SurfaceSpy::default();
        let tap = FullscreenTap {
            inner: Arc::new(spy.clone()),
            ring: std::sync::Mutex::new(kitsu_tui::cells::CellRing::new(8)),
        };

        tap.transcript_line(Fd::Stdout, "a streamed row");
        tap.note_prompt("what did that do?");

        assert_eq!(
            tap.lines()
                .iter()
                .map(|line| (line.is_prompt(), line.plain()))
                .collect::<Vec<_>>(),
            vec![
                (false, "a streamed row".to_string()),
                (true, "what did that do?".to_string()),
            ],
            "the tap's ring is the only ring in fullscreen, so the prompt must \
             reach it"
        );
        assert_eq!(
            spy.written(),
            vec![(Fd::Stdout, "a streamed row".to_string())],
            "and the prompt is recorded, not forwarded: the composer already \
             cleared it from the screen"
        );
    }

    /// **Test: the fullscreen ring holds a streamed answer, and now a buffered
    /// one too — and the line bound is applied on the same path.**
    ///
    /// Measured, not reasoned. This test was written when the ring was fed only
    /// from the surface seam and `kitsu_render::print_message` wrote straight
    /// to the process descriptor without consulting `active_surface`, so an
    /// answer printed whole at the end of a round could never enter it.
    ///
    /// **Amended when E11 was rebased onto main, because that premise is no
    /// longer true.** Main's fix for a long reply's destroyed last line routes
    /// `print_message` through `active_surface().write_block`, so a buffered
    /// answer now reaches the ring as well. The limitation F145 recorded as
    /// holding "by construction" was removed upstream, by a different change
    /// with a different motive.
    ///
    /// Why it still matters for **F51**: with tools advertised — the default —
    /// an ordinary answer used to be buffered, and F51's deliverable is that
    /// such answers stream and so arrive here through `print_fragment`. That
    /// is the first assertion, and it is unchanged. Only the second assertion's
    /// claim about the *other* path is restated to what is now measurable.
    ///
    /// The third assertion is for **F52**: the new content is pushed through
    /// the same `CellRing::push` as everything else, so the ring's **line**
    /// bound governs it identically — a documented limit, not a new unbounded
    /// one. The bound is still in lines, so F52's open question about a single
    /// pathological line is untouched by this.
    #[test]
    #[serial]
    fn the_fullscreen_ring_holds_a_streamed_answer_and_now_a_buffered_one_too() {
        let _state = GlobalState::capture();
        let tap = Arc::new(FullscreenTap {
            inner: Arc::new(SurfaceSpy::default()),
            ring: std::sync::Mutex::new(kitsu_tui::cells::CellRing::new(2)),
        });
        set_surface(tap.clone());

        kitsu_render::print_fragment("streamed answer\n");
        kitsu_render::print_message("buffered answer\n");

        let held: Vec<String> = tap.lines().iter().map(|line| line.plain()).collect();
        assert!(
            held.iter().any(|line| line.contains("streamed answer")),
            "a streamed answer reaches the ring through the surface seam: {held:?}"
        );
        assert!(
            held.iter().any(|line| line.contains("buffered answer")),
            "a buffered answer reaches the ring too, since main routed \
             print_message through the surface seam to stop a long reply's \
             last line being destroyed: {held:?}"
        );

        // The bound: this ring holds two lines, so a third streamed line
        // evicts the oldest exactly as it does for any other content.
        kitsu_render::print_fragment("second\nthird\n");
        let held: Vec<String> = tap.lines().iter().map(|line| line.plain()).collect();
        assert_eq!(
            held,
            vec!["second".to_string(), "third".to_string()],
            "the ring's line bound governs streamed answers on the same path \
             as everything else: {held:?}"
        );
    }

    /// **Test: the paging anchor lives on the segment, and the segment's own
    /// `Drop` is the only thing that clears it.**
    ///
    /// Findings **F94/F96/F98**: the paths out of fullscreen are enumerable
    /// and every one of them goes through this type's single [`Drop`] -- a
    /// `/tui` swap, a `run_chat` return, a panic. An anchor kept in a static
    /// or beside the segment would be a second thing to put back, so this
    /// pins the anchor's *home* rather than its arithmetic: the eleven
    /// transitions are `kitsu_tui::fullscreen`'s and are pinned there.
    ///
    /// **F71**: the press is checked on the painted bytes as well as on the
    /// flag, because a flag that flips while the frame stands still is the
    /// defect this card exists to prevent.
    #[test]
    #[serial]
    fn the_paging_anchor_lives_on_the_segment_and_its_drop_is_the_clearer() {
        /// The frame rows in one chunk of painted bytes, in order.
        ///
        /// Every frame this type writes is `ESC[2J ESC[H` (the entry paint
        /// adds the screen and title enables in front) followed by one row per
        /// CRLF, so the rows are what follows the last cursor home.
        fn frame_rows(painted: &str) -> Vec<String> {
            painted
                .rsplit("\x1b[H")
                .next()
                .unwrap_or(painted)
                .split("\r\n")
                .filter(|row| !row.is_empty())
                .map(str::to_string)
                .collect()
        }

        let _state = GlobalState::capture();

        // Sixty lines into a 24-row window: the entry frame is `rows - 1` of
        // them, so there are thirty-seven lines above it for a press to
        // reveal. NON-VACUOUS, and the reason the oldest and newest line
        // texts below are distinguishable.
        let mut ring = CellRing::new(60);
        for row in 1..=60 {
            ring.push(Fd::Stdout, &format!("ROW_{row:02}"));
        }
        let lines = ring.lines();

        let sink = SharedSink::new();
        let mut segment = enter_fullscreen(Box::new(sink.clone()), &lines, 24, 80, 10_000);
        assert!(
            segment.is_following(),
            "NON-VACUOUS: a fresh segment follows the newest line, or a press \
             has nothing to change"
        );
        let entry = sink.text();
        assert!(
            entry.contains("ROW_60"),
            "NON-VACUOUS: the entry frame must be the newest lines; the \
             segment wrote:\n{}",
            entry.escape_debug()
        );

        let before = sink.bytes().len();
        segment.page(Page::Up, 24, 80);
        assert!(
            !segment.is_following(),
            "one `PgUp` with thirty-seven lines above the window pins the \
             anchor (transition 1), and the anchor it pins is this segment's"
        );
        let painted = String::from_utf8_lossy(&sink.bytes()[before..]).into_owned();
        let paged_frame = frame_rows(&painted);
        assert!(
            !paged_frame.is_empty() && !painted.contains("ROW_60"),
            "and the press must repaint the frame from the new anchor: the \
             frame does not follow the ring here, so nothing else will ever \
             paint it, and the newest line has to leave the screen. The \
             segment wrote, in answer to the press:\n{}",
            painted.escape_debug()
        );
        assert_ne!(
            paged_frame,
            frame_rows(&entry),
            "by value, so a press that flipped the anchor and repainted the \
             same rows fails. How far one press moves is `page_step`'s, and it \
             is pinned in `kitsu_tui::fullscreen` -- naming a row number here \
             would be a second home for one piece of arithmetic."
        );

        // **And the anchor survives a repaint.** The frame is constructed per
        // paint, so an anchor read from a fresh view would silently un-pin on
        // the next line the session writes -- paging that lasts until the next
        // output. One line through the seam, one `repaint_if_the_ring_grew`,
        // and the *same rows* have to come back: a snap to following would
        // show the newest line instead, which is what this test's name
        // promises and what nothing else covers.
        let before_growth = sink.bytes().len();
        through_the_seam(&[(Fd::Stdout, "APPENDED_WHILE_PINNED")]);
        segment.repaint_if_the_ring_grew(24, 80);
        let repainted = String::from_utf8_lossy(&sink.bytes()[before_growth..]).into_owned();
        assert_eq!(
            frame_rows(&repainted),
            paged_frame,
            "a repaint must come from the segment's anchor and not from a \
             fresh view: same anchor, same rows. The segment wrote:\n{}",
            repainted.escape_debug()
        );
        assert!(
            !segment.is_following(),
            "and the growth must not have re-latched following (transition 10 \
             is suppressed while pinned)"
        );

        let after_press = sink.bytes().len();
        segment.page(Page::Newest, 24, 80);
        assert!(
            segment.is_following(),
            "`Ctrl+End` re-latches following (transition 9)"
        );
        let relatched = String::from_utf8_lossy(&sink.bytes()[after_press..]).into_owned();
        assert!(
            relatched.contains("ROW_60"),
            "and repaints the newest line back onto the screen:\n{}",
            relatched.escape_debug()
        );

        // The anchor does not outlive the segment: pin one, drop it, and the
        // next segment over the same ring follows again. A static or a
        // process-global anchor passes every assertion above and fails here.
        segment.page(Page::Oldest, 24, 80);
        assert!(!segment.is_following(), "pinned at the oldest line");
        drop(segment);

        let second = SharedSink::new();
        let next = enter_fullscreen(Box::new(second.clone()), &lines, 24, 80, 10_000);
        assert!(
            next.is_following(),
            "the paging state is the segment's: a new one must start at the \
             newest line and not inherit the last segment's anchor"
        );
        assert!(
            second.text().contains("ROW_60"),
            "...which its entry frame shows; it wrote:\n{}",
            second.text().escape_debug()
        );
        drop(next);
    }

    /// **Test: inline and classic page nothing, and the four sequences keep
    /// today's meaning there.**
    ///
    /// The scope negative, and it is pinned by what the *absence* of a sink
    /// does rather than by a source read: production's inline call site
    /// passes `None`, so the reader with `None` is inline's reader.
    ///
    /// Measured, so the assertions are today's behaviour and not a guess at
    /// it: `ESC[5~` and `ESC[6~` are swallowed by the editor with
    /// `changed: false` and no action, and `ESC[1;5H`/`ESC[1;5F` move the
    /// caret to the start and to the end. The second pair is what makes this
    /// test bite: a decoder that recognised the paging keys in the reader
    /// *unconditionally* would pass every other test on this card and break
    /// `Ctrl+Home` for every inline user.
    #[test]
    fn inline_pages_nothing_and_keeps_todays_meaning_for_the_four_sequences() {
        // `PgUp`/`PgDn` are inert: the draft and the caret are untouched and
        // the read continues.
        let typed = script(&[b"abc", PG_UP, PG_DN, b"\r"]);
        let (outcome, frames) = drive_frames(Chunks::bytewise(&typed), &[]);
        assert_eq!(
            submitted(&outcome),
            "abc",
            "`PgUp`/`PgDn` are inert at the inline composer, as they are \
             today: frames {frames:?}"
        );

        // `Ctrl+Home`/`Ctrl+End` are `Home`/`End` there, which is what the
        // editor's parameter-blind `b'H'`/`b'F'` arms make them.
        let typed = script(&[b"bc", CTRL_HOME, b"a", CTRL_END, b"d\r"]);
        let (outcome, _) = drive_frames(Chunks::bytewise(&typed), &[]);
        assert_eq!(
            submitted(&outcome),
            "abcd",
            "and `Ctrl+Home`/`Ctrl+End` still reach the editor in inline, \
             where there is no frame for them to move: these keys are live \
             only while fullscreen holds the screen, which is also why §4.7 \
             keeps them out of `keys::BINDINGS`"
        );
    }

    /// Test 10. ↑/↓ walk the `history` argument, and the paint callback shows
    /// the recalled line.
    ///
    /// Load-bearing detail: on an empty, walk-free draft the editor emits
    /// `EditorAction::RecallRequest` rather than walking, because the
    /// during-turn collector wants first refusal for the prompt queue. The idle
    /// reader has no queue, so it must decline by calling
    /// `history_prev_forced` — otherwise ↑ does nothing at the idle prompt.
    #[test]
    fn inline_read_walks_the_history_argument_with_the_arrows() {
        let history = vec!["first".to_string(), "second".to_string()];

        let (outcome, paints) = read_with_history(b"\x1b[A\r", &history);
        assert_eq!(
            submitted(&outcome),
            "second",
            "one Up from an empty draft recalls the most recent entry"
        );
        assert!(
            paints_in_order(&paints, &[("second", 6)]),
            "the recalled line must be painted: {paints:?}"
        );

        let (outcome, _) = read_with_history(b"\x1b[A\x1b[A\r", &history);
        assert_eq!(
            submitted(&outcome),
            "first",
            "two Ups walk one further back"
        );

        let (outcome, _) = read_with_history(b"\x1b[A\x1b[A\x1b[B\r", &history);
        assert_eq!(submitted(&outcome), "second", "Down walks forwards again");
    }

    /// Test 11. Ctrl+D on an empty draft ends input.
    #[test]
    fn inline_read_reports_eof_for_ctrl_d_on_an_empty_draft() {
        assert_eq!(read_bytes(b"\x04").0, InlineRead::Eof);
    }

    /// Test 12. Ctrl+D on a **non-empty** draft is not EOF. The editor already
    /// decides this and what it does is delete-ahead (readline's behaviour), so
    /// the reader must keep reading and the deletion must show up in the draft.
    #[test]
    fn inline_read_treats_ctrl_d_on_a_non_empty_draft_as_delete_ahead() {
        let (outcome, _) = read_bytes(b"ab\x1b[D\x04\r");
        assert_eq!(
            submitted(&outcome),
            "a",
            "Ctrl+D mid-draft deletes the char under the cursor and keeps reading"
        );
        // And with the cursor at the end there is nothing to delete, so it is
        // simply inert rather than an EOF.
        let (outcome, _) = read_bytes(b"ab\x04\r");
        assert_eq!(submitted(&outcome), "ab");
    }

    /// Test 13. Ctrl+C with a non-empty draft abandons the draft and keeps
    /// reading; the paint callback must show the cleared row, so the user can
    /// see the draft went away.
    #[test]
    fn inline_read_clears_a_non_empty_draft_on_ctrl_c_and_keeps_reading() {
        let (outcome, paints) = read_bytes(b"abc\x03xy\r");
        assert_eq!(submitted(&outcome), "xy");
        assert!(
            paints_in_order(&paints, &[("abc", 3), ("", 0), ("xy", 2)]),
            "the cleared draft must be painted between the two drafts: {paints:?}"
        );
    }

    /// Test 14. Ctrl+C on an empty draft is the interrupt. The reader only
    /// reports it; re-raising `SIGINT` (raw mode masks `ISIG`) stays with the
    /// caller, exactly as in the during-turn collector.
    #[test]
    fn inline_read_reports_interrupted_for_ctrl_c_on_an_empty_draft() {
        assert_eq!(read_bytes(b"\x03").0, InlineRead::Interrupted);
        // Also after a draft was cleared by a first Ctrl+C: the second one
        // interrupts, which is the two-press behaviour users expect.
        assert_eq!(read_bytes(b"abc\x03\x03").0, InlineRead::Interrupted);
    }

    // ---------------------------------------------------------------------
    // The hint slot (the third element of the seam).
    //
    // `update_live_row` in `main.rs` computes the classic row's hint as:
    // the reverse-search status when Ctrl+R is active (RESERVED, so it keeps
    // its columns even when the draft is windowed), and otherwise the inline
    // slash-completion suffix from `typeahead_slash_hint` (unreserved, so a
    // tight row drops it). The inline reader must derive exactly the same
    // two things from the same editor, or the two renderers disagree about
    // what the user is being shown.
    //
    // Tab completion over that hint is covered by its own group below.
    // Out of scope on purpose: the bordered multi-row slash palette.
    // ---------------------------------------------------------------------

    /// Test 16. A bare slash prefix yields the completion remainder, and it
    /// is **unreserved**: at an idle prompt `/s` suggests `save`, the first
    /// registry match, so the hint is the suffix `ave` alone -- the typed text
    /// is never repeated into the hint.
    ///
    /// Card T0-S17, finding F30: this used to expect `teer`, because the idle
    /// prompt inherited the during-turn ranking that puts `steer` first. That
    /// ranking is for commands acting on a RUNNING turn, and no turn runs
    /// here. The during-turn row still ranks that way; see
    /// `typeahead_slash_hint_prefers_mid_turn_actionable_commands`.
    #[test]
    fn inline_read_paints_the_slash_completion_suffix_as_an_unreserved_hint() {
        let (draft, cursor, hint, reserved) = last_frame(b"/s");
        assert_eq!(draft, "/s");
        assert_eq!(cursor, 2);
        assert_eq!(hint, "ave", "the hint is the remainder, not the command");
        assert!(
            !reserved,
            "a completion hint must not claim columns from the draft"
        );
    }

    /// Test 17. The three no-hint cases, which are `typeahead_slash_hint`'s
    /// own rules and must not be re-invented here: text that is not a slash
    /// prefix, a prefix followed by whitespace (the command is settled and
    /// its arguments are being typed), and a bare `/`.
    #[test]
    fn inline_read_paints_no_hint_when_the_draft_is_not_a_slash_prefix() {
        for bytes in [&b"hello"[..], &b"/s "[..], &b"/"[..]] {
            let (draft, _, hint, reserved) = last_frame(bytes);
            assert_eq!(hint, "", "no hint belongs on draft {draft:?}",);
            assert!(!reserved, "an empty hint is never reserved: {draft:?}");
        }
    }

    /// Test 18. **The headline.** Ctrl+R reverse search works at the inline
    /// idle prompt today but its status indicator has nowhere to go, so the
    /// user is left in a mode they cannot see. The status belongs in the hint
    /// slot, **reserved**, exactly as `update_live_row` puts it there.
    #[test]
    fn inline_read_paints_the_reverse_search_status_as_a_reserved_hint() {
        let history = vec!["deploy the thing".to_string()];

        // Ctrl+R alone: the search is open with an empty query.
        let (_, _, hint, reserved) = drive_frames(Chunks::bytewise(b"\x12"), &history)
            .1
            .pop()
            .expect("at least one paint");
        assert!(
            hint.contains("⌕ reverse-search ''"),
            "entering search must show its status: {hint:?}"
        );
        assert!(
            reserved,
            "the search status must be reserved -- it may not be dropped when \
             the matched draft competes for columns"
        );

        // Ctrl+R then a query: the status carries the query, and the draft is
        // the match the editor walked to.
        let (draft, _, hint, reserved) = drive_frames(Chunks::bytewise(b"\x12dep"), &history)
            .1
            .pop()
            .expect("at least one paint");
        assert_eq!(draft, "deploy the thing");
        assert!(
            hint.contains("⌕ reverse-search 'dep'"),
            "the status must track the query: {hint:?}"
        );
        assert!(reserved);

        // Ctrl+G leaves the search, and the status goes with it.
        let (_, _, hint, reserved) = drive_frames(Chunks::bytewise(b"\x12dep\x07"), &history)
            .1
            .pop()
            .expect("at least one paint");
        assert_eq!(hint, "", "leaving search clears the status: {hint:?}");
        assert!(!reserved);
    }

    // ---------------------------------------------------------------------
    // Tab at the idle prompt: accepting the showing slash hint.
    //
    // Parity with the during-turn collector's `CompleteRequest` arm in
    // `main.rs`, which takes the buffer, computes `typeahead_slash_hint` on
    // it, and -- only when that is non-empty -- replaces the buffer with
    // buffer + hint and asks for a redraw. Nothing else: no completion list,
    // no cycling, no `@file` paths.
    //
    // The trap this group exists for: the inline reader's hint slot is the
    // slash suffix OR the reverse-search status, and the status is display
    // text (`  ⌕ reverse-search 'dep'`, leading spaces and all). An
    // implementation that appends "the hint the row is showing" rather than
    // `typeahead_slash_hint(&buffer)` would type that status into the user's
    // prompt. The reverse-search test below drives the orderings where the
    // two differ, including a single read in which the Tab is processed only
    // after a later Ctrl+R in the same chunk has already armed the status.
    // ---------------------------------------------------------------------

    /// Test 21. The headline: `/s` then Tab leaves the draft completed.
    ///
    /// The accepted text is the draft **plus** the suffix, never the suffix
    /// alone and never the typed prefix twice; the cursor lands at the end;
    /// and the hint slot is now empty, because `/steer` is a whole command
    /// and `idle_slash_hint` has nothing left to offer.
    #[test]
    fn inline_read_tab_accepts_the_showing_slash_hint() {
        let (draft, cursor, hint, reserved) = last_frame(b"/s\t");
        assert_eq!(draft, "/save", "Tab accepts the suffix the row was showing");
        assert_eq!(cursor, 5, "the cursor follows the accepted text");
        assert_eq!(
            hint, "",
            "an exactly-completed command leaves no further hint: {hint:?}"
        );
        assert!(!reserved);
    }

    /// Test 22. The row repaints after an accepted Tab, so the user sees the
    /// completed draft rather than the stale one: the `/s` + `teer` frame
    /// must be followed by a `/steer` frame.
    #[test]
    fn inline_read_tab_repaints_the_row_with_the_accepted_draft() {
        let painted = frames(b"/s\t");
        let hinted = painted
            .iter()
            .position(|(draft, _, hint, _)| draft == "/s" && hint == "ave")
            .expect("the hint is painted before Tab is pressed");
        assert!(
            painted[hinted + 1..]
                .iter()
                .any(|(draft, cursor, _, _)| draft == "/save" && *cursor == 5),
            "accepting a hint must repaint the row: {painted:?}"
        );
    }

    /// Test 23. Tab with no hint showing does nothing at all: the draft is
    /// unchanged and nothing is submitted. These are `typeahead_slash_hint`'s
    /// own empty cases -- plain text, a bare `/`, a settled command with
    /// arguments being typed, and a command already complete.
    #[test]
    fn inline_read_tab_does_nothing_when_no_hint_is_showing() {
        for (bytes, expected) in [
            (&b"hello\t"[..], "hello"),
            (&b"/\t"[..], "/"),
            (&b"/s \t"[..], "/s "),
            (&b"/steer\t"[..], "/steer"),
        ] {
            let (outcome, painted) = drive(Chunks::bytewise(bytes), &[]);
            let (draft, _) = painted.last().expect("at least one paint").clone();
            assert_eq!(
                draft, expected,
                "Tab must leave {expected:?} untouched when no hint is showing"
            );
            assert_eq!(
                outcome,
                InlineRead::Eof,
                "Tab is never a submission: {expected:?}"
            );
        }
    }

    /// Test 24. Tab is not submission. Accepting a hint leaves the read
    /// running, and the following Enter submits the **completed** text -- the
    /// two halves of the contract in one drive.
    #[test]
    fn inline_read_tab_continues_the_read_and_enter_submits_the_completed_draft() {
        let (outcome, _) = read_bytes(b"/s\t\r");
        assert_eq!(
            submitted(&outcome),
            "/save",
            "Enter after Tab submits the completed draft"
        );
    }

    /// Test 25. **The trap.** Tab while a reverse search is active must never
    /// insert the search status into the draft.
    ///
    /// What Tab *does* mid-search is the editor's own rule, not a new one
    /// invented here: a control byte during a search accepts the current
    /// match and leaves the search, and the re-fed Tab then raises
    /// `CompleteRequest` against that accepted draft. Since a history line is
    /// not a bare slash prefix, `idle_slash_hint` is empty and Tab adds
    /// nothing -- the draft is the match, the status is gone from the row,
    /// and the read continues.
    #[test]
    fn inline_read_tab_during_a_reverse_search_never_inserts_the_search_status() {
        let history = vec!["deploy the thing".to_string()];

        let (outcome, painted) = drive_frames(Chunks::bytewise(b"\x12dep\t"), &history);
        let (draft, cursor, hint, reserved) = painted.last().expect("at least one paint").clone();
        assert_eq!(
            draft, "deploy the thing",
            "Tab accepts the match the search walked to, and nothing more"
        );
        assert_eq!(cursor, "deploy the thing".chars().count());
        assert_eq!(
            hint, "",
            "the status leaves the row with the search: {hint:?}"
        );
        assert!(!reserved);
        assert_eq!(outcome, InlineRead::Eof, "Tab mid-search is no submission");

        // The orderings in which a naive "append whatever the row is showing"
        // differs from `typeahead_slash_hint(&buffer)`: the Tab's action is
        // processed only after the whole read is fed, so a later Ctrl+R in the
        // same chunk leaves the status armed at exactly that moment.
        for chunk in [
            &b"\x12dep\t"[..],
            &b"\t\x12dep"[..],
            &b"/s\t\x12dep"[..],
            &b"\x12dep\tx"[..],
        ] {
            let (_, painted) = drive_frames(Chunks::new([chunk]), &history);
            for (draft, _, _, _) in &painted {
                assert!(
                    !draft.contains('⌕') && !draft.contains("reverse-search"),
                    "the search status is display text and must never enter the draft \
                     (chunk {:?}): {draft:?}",
                    String::from_utf8_lossy(chunk)
                );
            }
        }

        // And it must not reach a submission either.
        let (outcome, _) = drive(Chunks::new([b"\x12dep\t", b"\r"]), &history);
        assert_eq!(submitted(&outcome), "deploy the thing");
    }

    /// Test 19. The hint reaches the pinned row dimmed, immediately after the
    /// draft, through `kitsu-render`'s composition rather than a `format!`
    /// here -- the cursor column stays before the hint so typing continues
    /// over it.
    #[test]
    #[serial]
    fn paint_draft_renders_an_unreserved_hint_dimmed_after_the_draft() {
        let _state = GlobalState::capture();
        let (guard, _out, err) = install_inline(24, 1, "status: idle");
        let guard = guard.expect("inline mode installs a guard");
        guard.paint_draft("kitsu> ", "/s", 2, "teer", false);

        let painted = err.text();
        assert!(
            painted.contains("kitsu> /s\u{1b}[2mteer"),
            "the hint belongs dimmed right after the draft on the pinned row: {painted:?}"
        );
    }

    /// Test 20. The reserved case on the real row: a draft long enough to
    /// claim every column must not push the reverse-search status off the
    /// row. That is the entire purpose of the reserved flag, and the reason
    /// this path composes through `kitsu-render` instead of concatenating.
    #[test]
    #[serial]
    fn paint_draft_keeps_a_reserved_hint_against_a_long_draft() {
        let _state = GlobalState::capture();
        let (guard, _out, err) = install_inline(24, 1, "status: idle");
        let guard = guard.expect("inline mode installs a guard");
        let draft = "x".repeat(500);
        guard.paint_draft(
            "kitsu> ",
            &draft,
            draft.chars().count(),
            "  ⌕ reverse-search 'match'",
            true,
        );

        let painted = kitsu_render::sanitize_terminal_text(&err.text());
        assert!(
            painted.contains("⌕ reverse-search 'match'"),
            "the reserved status must survive a windowed draft: {painted:?}"
        );
        assert!(
            painted.contains('…'),
            "the draft must window to make room: {painted:?}"
        );
    }

    /// Test 15. A bracketed-paste block stays **one** draft: its newlines
    /// insert literally instead of submitting at the first one. The editor
    /// implements this; what is pinned here is that the reader does not defeat
    /// it by looking for `\r`/`\n` itself before handing bytes over.
    #[test]
    fn inline_read_keeps_a_bracketed_paste_as_one_draft() {
        let (outcome, _) = read_bytes(b"\x1b[200~one\ntwo\x1b[201~\r");
        assert_eq!(
            submitted(&outcome),
            "one\ntwo",
            "a paste's newlines are content, not Enter"
        );

        // Same block arriving split across reads, including a split *inside*
        // the paste's start marker, which is how a slow link delivers it.
        let (outcome, _) = drive(
            Chunks::new([b"\x1b[20", b"0~one\r\ntwo", b"\x1b[201~", b"\r"]),
            &[],
        );
        assert_eq!(
            submitted(&outcome),
            "one\ntwo",
            "the paste survives being split across reads (CR LF collapses to one newline)"
        );
    }

    /// Test 16. An escape sequence split across two reads is still one key:
    /// the editor is stateful across `feed` calls, so the reader must feed what
    /// it got rather than requiring whole sequences per read.
    #[test]
    fn inline_read_handles_an_escape_sequence_split_across_reads() {
        let (outcome, _) = drive(Chunks::new([b"ac\x1b", b"[D", b"B\r"]), &[]);
        assert_eq!(submitted(&outcome), "aBc");
    }

    // ---------------------------------------------------------------------
    // Finding F19: `install` is the policy gate for a terminal too short to
    // hold one scrolling transcript row plus the viewport.
    //
    // The bound is `rows >= viewport_rows + 2`: DECSTBM requires the bottom
    // margin to exceed the top, so with a top of 1 the bottom must be at least
    // 2, and `bottom = rows - viewport_rows` must therefore reach 2. A `+ 1`
    // bound would admit `ESC[1;1r`, a one-row region terminals ignore just as
    // they ignore `ESC[1;0r` -- the guard would then swap the surface and stand
    // the live row down with nothing actually pinned, which is worse than no
    // guard. Below the bound the viewport is not
    // installed at all and the session keeps the classic renderer: the classic
    // live row needs no scroll region and degrades gracefully on any geometry,
    // whereas installing a viewport that cannot work would constrain the
    // terminal and show nothing. A refused install must therefore leave the
    // process exactly as it found it -- no bytes, the surface slot untouched,
    // and the live row still enabled.
    // ---------------------------------------------------------------------

    /// The geometries below cannot fit a usable scrolling region plus the
    /// viewport. `rows == 0` is among them -- a failed `TIOCGWINSZ` probe can
    /// report it, and it is the case that used to produce `ESC[1;0r` -- as are
    /// `(2, 1)` and `(4, 3)`, which would leave the one-row region `ESC[1;1r`.
    const TOO_SHORT: [(u16, u16); 12] = [
        (0, 0),
        (1, 0),
        (0, 1),
        (0, 3),
        (1, 1),
        (2, 1),
        (1, 3),
        (2, 3),
        (3, 3),
        (4, 3),
        (3, 24),
        (24, 23),
    ];

    /// Test 18. The refusal itself, over the exact boundary cases.
    #[test]
    #[serial]
    fn install_inline_refuses_a_terminal_too_short_for_a_transcript_row() {
        let _state = GlobalState::capture();
        let expected_live = live::set_live_rows_enabled(true);
        for (rows, viewport_rows) in TOO_SHORT {
            let before = active_surface();
            let (guard, out, err) = install_inline(rows, viewport_rows, "status: idle");
            assert!(
                guard.is_none(),
                "install({rows}, {viewport_rows}) must decline: the terminal cannot hold a \
                 usable scrolling region (bottom margin >= 2) plus the viewport"
            );
            assert!(
                out.is_empty() && err.is_empty(),
                "a refused install writes nothing at all, got out {:?} err {:?}",
                out.text(),
                err.text()
            );
            assert!(
                Arc::ptr_eq(&before, &active_surface()),
                "a refused install must leave the surface slot untouched ({rows}, {viewport_rows})"
            );
            assert!(
                live_rows_enabled(),
                "a refused install must leave the classic live row enabled \
                 ({rows}, {viewport_rows})"
            );
        }
        live::set_live_rows_enabled(expected_live);
    }

    /// Test 19. The smallest workable geometry still installs, and the region
    /// it emits is a valid one: a bottom margin of at least the top margin of
    /// 1, with the viewport's row below it rather than on top of it.
    #[test]
    #[serial]
    fn install_inline_succeeds_at_the_smallest_workable_geometry() {
        let _state = GlobalState::capture();
        // rows == viewport_rows + 2: two transcript rows (1 and 2), the
        // viewport on row 3.
        let (guard, out, err) = install_inline(3, 1, "status: idle");
        let guard = guard.expect("rows == viewport_rows + 2 is workable and installs");

        let expected = expected_show(3, 1) + &expected_status_paint(3, 1, "status: idle");
        assert_eq!(err.text(), expected);
        assert!(
            err.text().starts_with("\x1b[1;2r"),
            "the region's bottom margin must exceed its top of 1: {:?}",
            err.text()
        );
        for ignored in ["\x1b[1;0r", "\x1b[1;1r"] {
            assert!(
                !err.text().contains(ignored),
                "{ignored:?} is a DECSTBM real terminals ignore, which is what finding F19 \
                 is about: {:?}",
                err.text()
            );
        }
        assert!(out.is_empty(), "nothing reaches stdout");
        drop(guard);

        // And with a taller viewport at the same bound.
        let (guard, _out, err) = install_inline(5, 3, "status: idle");
        let guard = guard.expect("rows == viewport_rows + 2 installs for any viewport height");
        assert!(err.text().starts_with("\x1b[1;2r"));
        drop(guard);
    }

    // ---------------------------------------------------------------------
    // The paste latency measurement the milestone record owes.
    // ---------------------------------------------------------------------

    /// A 10 KiB paste payload with embedded newlines and tabs, deterministic so
    /// the assertion can compare content exactly. No carriage returns: the
    /// editor collapses CR LF inside a paste to one newline, which would make
    /// the length assertion about that collapse rather than about the paste.
    fn paste_payload(len: usize) -> String {
        let mut payload = String::with_capacity(len + 64);
        let mut line = 0usize;
        while payload.len() < len {
            payload.push_str(&format!("line {line:04}\tcolumn two\tcolumn three\n"));
            line += 1;
        }
        payload.truncate(len);
        // Never end on a partial escape-looking tail or a trailing newline-less
        // cut mid-word is fine, but a payload must stay valid UTF-8; all bytes
        // above are ASCII, so truncation is safe.
        payload
    }

    /// Test 20. A 10 KiB bracketed-paste block stays **one** draft and arrives
    /// intact, and the whole read completes well inside a generous wall-clock
    /// bound.
    ///
    /// The bound is about shape, not microseconds: a quadratic insert path
    /// (re-scanning or re-allocating the draft per byte) turns 10 KiB into tens
    /// of millions of operations and blows any bound by orders of magnitude,
    /// while the linear path finishes in single-digit milliseconds. Five
    /// seconds is therefore loose enough never to flake on a loaded machine or
    /// under an unoptimised debug build, and still catches the pathology this
    /// measurement exists to rule out.
    ///
    /// The payload is fed in 1 KiB chunks, which is what a real terminal
    /// delivers: `read_line_inline`'s buffer is 1024 bytes, so a read can never
    /// hand the editor more than that however the paste was written.
    #[test]
    fn inline_read_keeps_a_ten_kib_paste_as_one_draft_without_pathological_latency() {
        const LEN: usize = 10 * 1024;
        let payload = paste_payload(LEN);
        assert_eq!(payload.len(), LEN);
        assert!(payload.contains('\n') && payload.contains('\t'));

        let mut stream = Vec::new();
        stream.extend_from_slice(b"\x1b[200~");
        stream.extend_from_slice(payload.as_bytes());
        stream.extend_from_slice(b"\x1b[201~");
        stream.extend_from_slice(b"\r");

        let chunks: Vec<Vec<u8>> = stream.chunks(1024).map(|chunk| chunk.to_vec()).collect();
        let mut input = Chunks(chunks.into_iter().collect());

        let mut paints: usize = 0;
        let started = std::time::Instant::now();
        let outcome = {
            let mut paint =
                |_draft: &str, _cursor: usize, _hint: &str, _reserved: bool| paints += 1;
            read_line_inline(&mut input, &mut paint, &[], &mut || {}).expect("inline read")
        };
        let elapsed = started.elapsed();

        // One draft: the whole payload arrives as a single submission with its
        // newlines intact, not split at the first one.
        let line = submitted(&outcome);
        assert_eq!(
            line.len(),
            LEN,
            "the paste must submit as one draft of its full length, not be split at a newline"
        );
        assert_eq!(line, payload, "the paste's content must be exact");
        assert!(
            line.contains("line 0000\tcolumn two"),
            "tabs and the first line survive"
        );
        assert!(paints >= 1, "the draft is painted at least once");

        assert!(
            elapsed < std::time::Duration::from_secs(5),
            "a 10 KiB paste took {elapsed:?}, which indicates pathological (quadratic) \
             behaviour rather than a slow machine"
        );
    }

    /// Test 17. A read returning zero bytes is EOF: the descriptor is closed
    /// (a piped carrier draining, or the terminal going away), so the reader
    /// must not spin on it.
    #[test]
    fn inline_read_reports_eof_when_the_reader_is_drained() {
        assert_eq!(read_bytes(b"").0, InlineRead::Eof);
        // Including mid-draft: unterminated typing then a closed descriptor.
        assert_eq!(read_bytes(b"half-typed").0, InlineRead::Eof);
    }

    // ---------------------------------------------------------------------
    // Finding F21: Ctrl+Z at an inline idle prompt.
    //
    // `RawStdinGuard` clears `ISIG`, so Ctrl+Z (0x1a) arrives as a byte. The
    // shared editor already turns it into `EditorAction::Suspend`, so the
    // keymap needs nothing; what is missing is a reader outcome, a terminal
    // release/reinstall pair on the guard, and the caller's ordering.
    //
    // The division of labour these tests pin: the reader is **pure** (it
    // reports the keystroke and hands the draft back, raising nothing and
    // writing nothing), the guard owns the terminal bytes, and the caller
    // sequences release -> termios restore -> raise -> re-engage -> reinstall
    // -> resume. The hazard being designed out is stopping the process with
    // the scroll region still set: the shell would then inherit a terminal
    // whose scrolling area is a subset of the screen.
    // ---------------------------------------------------------------------

    /// The bytes `InlineSurface::hide` emits for this geometry, derived the
    /// same way `expected_show` is: every pinned row cleared in ascending
    /// order, the region released, then the cursor parked on the first (now
    /// blank) viewport row, immediately below the transcript.
    fn expected_hide(rows: u16, viewport_rows: u16) -> String {
        let bottom = rows - viewport_rows;
        let mut expected = String::new();
        for i in 0..viewport_rows {
            let row = bottom + 1 + i;
            expected.push_str(&format!("\x1b[{row};1H\x1b[2K"));
        }
        expected.push_str("\x1b[r");
        expected.push_str(&format!("\x1b[{};1H", bottom + 1));
        expected
    }

    fn suspended(outcome: &InlineRead) -> &str {
        match outcome {
            InlineRead::Suspended(draft) => draft,
            other => panic!("expected a suspend, got {other:?}"),
        }
    }

    /// Test 21. Ctrl+Z reports `Suspended` **carrying the draft**, on an empty
    /// draft as well as a typed one, and the reader returns at the keystroke
    /// rather than reading on: the trailing bytes below would make a
    /// read-on reader submit `abzz`.
    #[test]
    fn inline_read_reports_suspended_with_the_draft_for_ctrl_z() {
        let (outcome, _) = read_bytes(b"hello\x1a");
        assert_eq!(
            suspended(&outcome),
            "hello",
            "Ctrl+Z must hand the draft back so the resume can replay it"
        );

        let (outcome, _) = read_bytes(b"\x1a");
        assert_eq!(
            suspended(&outcome),
            "",
            "Ctrl+Z on an empty draft suspends with an empty draft, not an interrupt"
        );

        let (outcome, _) = read_bytes(b"ab\x1azz\r");
        assert_eq!(
            suspended(&outcome),
            "ab",
            "the reader returns at Ctrl+Z; a reader that kept going would submit \"abzz\""
        );
    }

    /// Test 22. A multi-line bracketed paste survives the suspend intact,
    /// newlines included: the draft is whatever the editor holds, and the
    /// paste is one draft (test 15), so Ctrl+Z must not split or truncate it.
    #[test]
    fn inline_read_preserves_a_multi_line_paste_across_a_suspend() {
        let (outcome, _) = read_bytes(b"\x1b[200~one\ntwo\x1b[201~\x1a");
        assert_eq!(
            suspended(&outcome),
            "one\ntwo",
            "a pasted multi-line draft is returned whole"
        );

        // And with typing on either side of the paste, so the whole buffer is
        // carried rather than only the pasted block.
        let (outcome, _) = read_bytes(b"a\x1b[200~one\ntwo\x1b[201~b\x1a");
        assert_eq!(suspended(&outcome), "aone\ntwob");
    }

    /// Test 23. The reader stays pure when it suspends.
    ///
    /// Two things are pinned, one of them by the test merely finishing: if the
    /// reader raised `SIGTSTP` itself, this test process would **stop here**
    /// and the suite would hang rather than fail, so reaching the assertions
    /// at all proves the signal is the caller's. The explicit assertion is the
    /// other half: the last frame the painter saw is the draft that was
    /// returned, so the reader did not try to tear the row down itself either.
    #[test]
    fn inline_read_suspends_without_raising_the_signal_or_painting_a_teardown() {
        let (outcome, paints) = read_bytes(b"draft\x1a");
        assert_eq!(suspended(&outcome), "draft");
        assert_eq!(
            paints.last().map(|(draft, _)| draft.as_str()),
            Some("draft"),
            "the reader hands the row over as it stood; the teardown is the \
             caller's business: {paints:?}"
        );
    }

    /// Test 24. The resume mechanism: the draft `Suspended` returned replays
    /// into the next read and submits unchanged.
    ///
    /// This is the same replay the caller already uses for a carried-over
    /// draft (`Cursor::new(initial).chain(stdin())`), so the round trip is
    /// what makes typing survive a suspend/resume cycle.
    #[test]
    fn a_suspended_draft_replays_into_the_next_read_unchanged() {
        let (outcome, _) = read_bytes(b"/model qwen\x1a");
        let draft = suspended(&outcome).to_string();

        let mut replay = draft.clone().into_bytes();
        replay.push(b'\r');
        let (outcome, _) = read_bytes(&replay);
        assert_eq!(
            submitted(&outcome),
            "/model qwen",
            "re-entering the reader with the preserved draft must submit it intact"
        );
    }

    /// Test 25. Ctrl+backslash stays inert **by design**, not by accident.
    ///
    /// The conventional action is to re-raise `SIGQUIT`, whose default
    /// disposition terminates *and dumps core*. This process holds API tokens
    /// and conversation text in memory, so writing that to disk because of a
    /// stray keystroke is a worse outcome than the key doing nothing. The
    /// editor emits `EditorAction::Quit` for 0x1c; the reader swallows it, the
    /// draft is untouched, and the read continues.
    #[test]
    fn inline_read_leaves_ctrl_backslash_deliberately_inert() {
        let (outcome, _) = read_bytes(b"ab\x1ccd\r");
        assert_eq!(
            submitted(&outcome),
            "abcd",
            "Ctrl+backslash must neither end the read nor disturb the draft"
        );
    }

    /// Test 26. `release_for_suspend` emits exactly `hide`'s teardown: the
    /// pinned rows cleared, then the region released, then the park below the
    /// transcript. Derived from the geometry, not hardcoded.
    ///
    /// The clears-before-release ordering is spelled out independently of the
    /// helper, because it is the half that matters: releasing the region homes
    /// the cursor, so a clear emitted afterwards would address the wrong row.
    #[test]
    #[serial]
    fn release_for_suspend_emits_exactly_the_viewport_teardown() {
        let _state = GlobalState::capture();
        let (guard, out, err) = install_inline(24, 3, "status: idle");
        let guard = guard.expect("inline mode installs a guard");
        let before = err.text().len();

        guard.release_for_suspend();

        let tail = err.text()[before..].to_string();
        assert_eq!(
            tail,
            expected_hide(24, 3),
            "release must be the full teardown, got {tail:?}"
        );
        let release = tail.find("\x1b[r").expect("the region is released");
        for row in [22, 23, 24] {
            let clear = tail
                .find(&format!("\x1b[{row};1H\x1b[2K"))
                .unwrap_or_else(|| panic!("row {row} must be cleared: {tail:?}"));
            assert!(
                clear < release,
                "row {row} must be cleared while the region is still set: {tail:?}"
            );
        }
        assert!(out.is_empty(), "no viewport control bytes reach stdout");
        drop(guard);
    }

    /// Test 27. `reinstall_after_resume` emits exactly what `install` emits
    /// after the gate: the region set, the cursor parked, and the status row
    /// repainted.
    #[test]
    #[serial]
    fn reinstall_after_resume_emits_exactly_the_install_sequence() {
        let _state = GlobalState::capture();
        let (guard, out, err) = install_inline(24, 3, "status: idle");
        let guard = guard.expect("inline mode installs a guard");
        guard.release_for_suspend();
        let before = err.text().len();

        guard.reinstall_after_resume();

        let tail = err.text()[before..].to_string();
        assert_eq!(
            tail,
            expected_show(24, 3) + &expected_status_paint(24, 3, "status: idle"),
            "reinstall must be install's own sequence, got {tail:?}"
        );
        assert!(out.is_empty(), "no viewport control bytes reach stdout");
        drop(guard);
    }

    /// Test 28. **The round-trip contract**, which is the whole of this card's
    /// terminal half: a release followed by a reinstall leaves the terminal in
    /// the same state as a fresh install, byte for byte on the sink.
    ///
    /// Compared against a second, independently installed guard rather than
    /// against a literal, so the assertion cannot drift from whatever
    /// `install` actually emits.
    #[test]
    #[serial]
    fn a_release_and_reinstall_round_trip_matches_a_fresh_install() {
        let _state = GlobalState::capture();

        let (fresh, _out, fresh_err) = install_inline(24, 3, "status: idle");
        let fresh = fresh.expect("inline mode installs a guard");
        let fresh_install = fresh_err.text();
        drop(fresh);

        let (guard, _out, err) = install_inline(24, 3, "status: idle");
        let guard = guard.expect("inline mode installs a guard");
        let before = err.text().len();
        guard.release_for_suspend();
        guard.reinstall_after_resume();

        let round_trip = err.text()[before..].to_string();
        assert_eq!(
            round_trip,
            expected_hide(24, 3) + &fresh_install,
            "a suspend/resume round trip must put the terminal back exactly as a \
             fresh install left it, got {round_trip:?}"
        );
        drop(guard);
    }

    /// Test 29. The round trip is repeatable: suspending twice in one session
    /// must work, so the reinstall has to restore the latched geometry rather
    /// than leave the viewport permanently unpinned after the first release.
    ///
    /// This is the assertion that catches the obvious implementation of
    /// `release_for_suspend` as a bare `hide()`: `hide` *takes* the latched
    /// geometry, so a `reinstall` that does not carry the original
    /// `(rows, viewport_rows)` itself would emit nothing at all the second
    /// time round.
    #[test]
    #[serial]
    fn release_and_reinstall_can_be_repeated_within_one_session() {
        let _state = GlobalState::capture();
        let (guard, _out, err) = install_inline(24, 3, "status: idle");
        let guard = guard.expect("inline mode installs a guard");

        let expected = expected_hide(24, 3)
            + &expected_show(24, 3)
            + &expected_status_paint(24, 3, "status: idle");

        for round in 1..=3 {
            let before = err.text().len();
            guard.release_for_suspend();
            guard.reinstall_after_resume();
            assert_eq!(
                err.text()[before..].to_string(),
                expected,
                "round {round} of a suspend/resume cycle must emit the same bytes as the first"
            );
        }
        drop(guard);
    }

    /// Test 30. Resuming repaints the **status** row, not the draft that was
    /// on screen when the suspend happened. The caller repaints the draft
    /// itself at step 6, when it re-enters the reader; what the reinstall owes
    /// is a row that is correct on its own rather than a stale composer.
    #[test]
    #[serial]
    fn reinstall_after_resume_repaints_the_status_row_not_a_stale_draft() {
        let _state = GlobalState::capture();
        let (guard, _out, err) = install_inline(24, 1, "status: idle");
        let guard = guard.expect("inline mode installs a guard");
        guard.paint_draft("> ", "half-typed", 10, "", false);

        let before = err.text().len();
        guard.release_for_suspend();
        guard.reinstall_after_resume();

        let tail = err.text()[before..].to_string();
        assert!(
            tail.contains("status: idle"),
            "the resumed row must read the status text: {tail:?}"
        );
        assert!(
            !tail.contains("half-typed"),
            "the stale draft must not be repainted by the reinstall: {tail:?}"
        );
        drop(guard);
    }

    /// Test 31. A guard released for a suspend and then **dropped** without a
    /// reinstall — Ctrl+Z, then EOF on resume — still restores both pieces of
    /// process-global state. The screen is already torn down by the release,
    /// so the only thing `Drop` still owes is the seam and the live row.
    #[test]
    #[serial]
    fn dropping_a_guard_released_for_a_suspend_still_restores_the_global_state() {
        let _state = GlobalState::capture();
        live::set_live_rows_enabled(true);
        let before_surface = active_surface();

        let (guard, _out, err) = install_inline(24, 3, "status: idle");
        let guard = guard.expect("inline mode installs a guard");
        guard.release_for_suspend();
        drop(guard);

        assert!(
            Arc::ptr_eq(&before_surface, &active_surface()),
            "the displaced surface must be restored even after a release"
        );
        assert!(
            live_rows_enabled(),
            "the live row must come back even after a release"
        );
        let after = err.bytes().len();
        kitsu_render::tool_activity("read_file(src/main.rs)");
        assert_eq!(
            err.bytes().len(),
            after,
            "and the seam is restored, so nothing further reaches the injected sink"
        );
    }

    /// A `SuspendEffects` that records the order it is called in, so the one
    /// property the seam exists for is pinned by a test rather than by review.
    #[derive(Default)]
    struct RecordingEffects {
        calls: Vec<&'static str>,
        reengage_succeeds: bool,
    }

    impl SuspendEffects for RecordingEffects {
        fn release_viewport(&mut self) {
            self.calls.push("release");
        }
        fn restore_terminal_modes(&mut self) {
            self.calls.push("restore");
        }
        fn stop_process(&mut self) {
            self.calls.push("stop");
        }
        fn reengage_terminal_modes(&mut self) -> bool {
            self.calls.push("reengage");
            self.reengage_succeeds
        }
        fn reinstall_viewport(&mut self) {
            self.calls.push("reinstall");
        }
    }

    /// Test 32. The whole reason the seam exists (finding F21): the scroll
    /// region must be gone and the terminal canonical **before** the process
    /// stops. Getting this order wrong is worse than not handling Ctrl+Z at
    /// all, because the shell is then handed a terminal whose scrolling area is
    /// a subset of the screen — and the stop is a blocking call, so no later
    /// cleanup can rescue it.
    #[test]
    fn suspending_releases_the_region_and_the_terminal_before_stopping() {
        let mut effects = RecordingEffects {
            reengage_succeeds: true,
            ..Default::default()
        };

        assert!(suspend_and_resume(&mut effects));
        assert_eq!(
            effects.calls,
            vec!["release", "restore", "stop", "reengage", "reinstall"]
        );

        // Stated as the invariant and not just the sequence, so the failure
        // message says what is actually wrong if someone reorders it.
        let stop = effects.calls.iter().position(|c| *c == "stop").unwrap();
        let release = effects.calls.iter().position(|c| *c == "release").unwrap();
        let restore = effects.calls.iter().position(|c| *c == "restore").unwrap();
        assert!(
            release < stop && restore < stop,
            "the region must be released and the terminal restored before the \
             process stops; a stopped process cannot clean up after itself: {:?}",
            effects.calls
        );
    }

    /// Test 33. If raw mode cannot be re-engaged after the resume, stop there:
    /// reinstalling the viewport over a terminal that is still canonical would
    /// pin a row nothing can type into, and the caller turns the `false` into a
    /// plain error instead.
    #[test]
    fn a_failed_reengage_after_resume_does_not_reinstall_the_viewport() {
        let mut effects = RecordingEffects {
            reengage_succeeds: false,
            ..Default::default()
        };

        assert!(!suspend_and_resume(&mut effects));
        assert_eq!(
            effects.calls,
            vec!["release", "restore", "stop", "reengage"]
        );
        assert!(
            !effects.calls.contains(&"reinstall"),
            "a failed reengage must not go on to pin a viewport: {:?}",
            effects.calls
        );
    }
    // ---------------------------------------------------------------------
    // Verification card: reverse search (Ctrl+R) at an inline idle prompt.
    //
    // Finding F20 listed reverse search among the things the inline prompt
    // loses. These tests establish what actually happens. The editor maps
    // 0x12 to `search_enter_or_cycle` and resolves the whole of search mode
    // *internally* -- it emits no `EditorAction` for it -- so the reader has
    // nothing to swallow and reverse search is live at the inline prompt.
    // Both of F20's claims about this prompt were wrong, and both are now
    // settled in code: reverse search was never lost (the editor resolves it
    // internally), and the `⌕ reverse-search '..'` indicator -- originally
    // unshowable because the paint callback was `(draft, cursor)` -- is
    // carried by the hint slot since T0-S9. Tab is no longer dropped either,
    // as of T0-S10.
    //
    // What the last test here guards is subtler and did NOT go away when the
    // seam was widened: the status is **display text** and must never become
    // draft text. That matters more now than when it was written, because Tab
    // exists -- an implementation that appended "the current hint" would type
    // the indicator into the user's prompt.
    // ---------------------------------------------------------------------

    /// History for the search tests, oldest first, with two `cargo` entries so
    /// cycling has somewhere to go.
    fn reverse_search_history() -> Vec<String> {
        vec![
            "cargo build".to_string(),
            "git status".to_string(),
            "cargo test".to_string(),
        ]
    }

    /// Test 34. Ctrl+R at an inline prompt really searches: the query narrows
    /// to the newest matching history entry and the match is painted as the
    /// draft.
    ///
    /// Note the two-Enter shape, which is the editor's readline-compatible
    /// behaviour and not a quirk of the reader: the first Enter *accepts* the
    /// match into the draft and leaves search mode, the second submits it.
    #[test]
    fn inline_read_runs_a_reverse_search_on_ctrl_r() {
        let history = reverse_search_history();
        let (outcome, paints) = read_with_history(b"\x12cargo\r\r", &history);
        assert_eq!(
            submitted(&outcome),
            "cargo test",
            "Ctrl+R must narrow history to the newest match and submit it; \
             paints: {paints:?}"
        );
        assert!(
            paints_in_order(&paints, &[("cargo test", "cargo test".len())]),
            "the match must be painted as the draft while searching: {paints:?}"
        );
        assert!(
            !paints.iter().any(|(draft, _)| draft == "cargo"),
            "the query must go to the search, not into the draft: {paints:?}"
        );
    }

    /// Test 35. A second Ctrl+R cycles to the next-older match, so the walk is
    /// not a single lookup.
    #[test]
    fn inline_read_cycles_to_older_matches_on_repeated_ctrl_r() {
        let history = reverse_search_history();
        let (outcome, _) = read_with_history(b"\x12cargo\x12\r\r", &history);
        assert_eq!(
            submitted(&outcome),
            "cargo build",
            "a second Ctrl+R must cycle past the newest match to the older one"
        );
    }

    /// Test 36. Ctrl+G aborts the search and restores the pre-search draft, so
    /// an abandoned search costs the user nothing.
    #[test]
    fn inline_read_restores_the_draft_when_a_reverse_search_is_cancelled() {
        let history = reverse_search_history();
        let (outcome, _) = read_with_history(b"half\x12cargo\x07\r", &history);
        assert_eq!(
            submitted(&outcome),
            "half",
            "Ctrl+G must put the pre-search draft back, not the match"
        );
    }

    /// Test 37. The accepted match is editable before it is submitted: search
    /// leaves ordinary draft text behind, not a special mode.
    #[test]
    fn inline_read_lets_an_accepted_search_match_be_edited_before_submitting() {
        let history = reverse_search_history();
        let (outcome, _) = read_with_history(b"\x12build\r --release\r", &history);
        assert_eq!(submitted(&outcome), "cargo build --release");
    }

    /// Test 38. **The real loss**, pinned so the record is honest: the search
    /// works, but the reader's paint seam carries only `(draft, cursor)`, so
    /// the `⌕ reverse-search '..'` status the during-turn collector renders in
    /// its hint slot never reaches the inline viewport row. Nothing painted
    /// while searching mentions it.
    #[test]
    fn inline_read_never_lets_the_search_status_become_draft_text() {
        let history = reverse_search_history();
        let (_, paints) = read_with_history(b"\x12cargo\r\r", &history);
        assert!(
            !paints
                .iter()
                .any(|(draft, _)| draft.contains("reverse-search")),
            "the search status is display text: it belongs in the hint slot \
             (T0-S9) and must never appear as draft text, or Tab would type \
             the indicator into the prompt: {paints:?}"
        );
    }

    // ---------------------------------------------------------------------
    // Finding F16: a resized terminal.
    //
    // Geometry is latched once at install and never updated, and DECSTBM
    // margins are absolute row numbers, so after a resize the pinned row is in
    // the wrong place (or off-screen) and `paint` keeps addressing rows from
    // the stale geometry.
    //
    // Two seams are pinned here. `note_resize`/`take_resize` are the
    // flag-and-check contract the `SIGWINCH` handler and the read loop share --
    // the handler may do nothing but store a bool, which is all that is
    // async-signal-safe. `BridgeGuard::resize` is the re-latch: the teardown
    // for the old geometry then the setup for the new one, which is exactly
    // the suspend pair with new dimensions.
    // ---------------------------------------------------------------------

    /// Test F16-1. The flag is a one-shot latch: set by the handler's only
    /// permitted action, cleared by the reader's check, and false again
    /// afterwards so one resize is handled once.
    #[test]
    #[serial]
    fn note_resize_latches_and_take_resize_clears_it() {
        // Drain anything an earlier test left behind, so this reads the latch
        // rather than a leftover.
        let _ = take_resize();
        assert!(
            !take_resize(),
            "the flag must be clear when nothing has been noted"
        );

        note_resize();
        assert!(
            take_resize(),
            "a noted resize must be visible to the reader"
        );
        assert!(
            !take_resize(),
            "taking the flag clears it, so one resize is handled exactly once"
        );

        // Coalescing: the flag carries no dimensions, so several
        // notifications collapse into one re-latch against a fresh probe.
        note_resize();
        note_resize();
        note_resize();
        assert!(take_resize(), "several notes still latch");
        assert!(
            !take_resize(),
            "and they coalesce into one, rather than queueing three re-latches"
        );
    }

    /// Test F16-2. A real `SIGWINCH` reaches the latch.
    ///
    /// This is as far as an in-process test can go: it proves the handler is
    /// installed and sets the flag, but **not** that a signal delivered while
    /// the reader blocks in `read()` surfaces as `EINTR` rather than being
    /// restarted by `SA_RESTART`. That depends on how the handler is
    /// installed and can only be exercised against a real terminal or a PTY.
    #[test]
    #[serial]
    fn a_raised_sigwinch_sets_the_resize_flag() {
        let _ = take_resize();
        install_sigwinch_handler();
        // SAFETY: raising a signal in-process whose handler has just been
        // installed and does nothing but store into an `AtomicBool`.
        let raised = unsafe { libc::raise(libc::SIGWINCH) };
        assert_eq!(raised, 0, "raise(SIGWINCH) failed");
        assert!(
            take_resize(),
            "a delivered SIGWINCH must set the flag the read loop checks; if this \
             fails the handler is not installed, or it is not storing into the latch"
        );
    }

    /// Test F16-3. A resize to the same dimensions emits nothing at all.
    ///
    /// A resize notification carries no dimensions, so a redundant one is the
    /// common case: re-latching unconditionally would tear the region down and
    /// set it back up on every spurious `SIGWINCH`, which the user sees as the
    /// pinned row flickering.
    #[test]
    #[serial]
    fn resize_to_the_same_dimensions_writes_nothing() {
        let _state = GlobalState::capture();
        let (guard, out, err) = install_inline(24, 3, "status: idle");
        let guard = guard.expect("inline mode installs a guard");
        let before = err.text().len();

        let installed = guard.resize(24, 3);

        assert!(
            installed,
            "the viewport is still pinned after a no-op resize"
        );
        assert_eq!(
            &err.text()[before..],
            "",
            "a resize to the same geometry must emit no bytes at all"
        );
        assert!(out.is_empty(), "no viewport control bytes reach stdout");
        drop(guard);
    }

    /// Test F16-4. A resize above the bound re-sets the region: the teardown
    /// for the **old** geometry, then the setup for the **new** one.
    ///
    /// Patching a margin is not an option -- DECSTBM takes absolute row
    /// numbers -- and the expected bytes are therefore literally
    /// `expected_hide(old) + expected_show(new)`, the same two halves a
    /// suspend/resume round trip uses, with different dimensions.
    #[test]
    #[serial]
    fn resize_relatches_by_tearing_down_the_old_geometry_and_setting_up_the_new() {
        let _state = GlobalState::capture();
        let (guard, out, err) = install_inline(24, 3, "status: idle");
        let guard = guard.expect("inline mode installs a guard");
        let before = err.text().len();

        let installed = guard.resize(40, 3);

        assert!(
            installed,
            "a resize above the bound keeps a viewport pinned"
        );
        let tail = err.text()[before..].to_string();
        assert_eq!(
            tail,
            expected_hide(24, 3) + &expected_show(40, 3),
            "a resize must emit the old geometry's teardown then the new geometry's \
             setup, and nothing else -- no status paint, because the caller repaints \
             the live draft immediately afterwards. Got {tail:?}"
        );
        assert!(out.is_empty(), "no viewport control bytes reach stdout");

        // And the new geometry is what gets painted: a 40-row terminal with a
        // 3-row viewport paints its first row at 38, not at 22.
        let before = err.text().len();
        guard.paint_draft("> ", "", 0, "", false);
        assert_eq!(
            &err.text()[before..],
            expected_status_paint(40, 3, "status: idle"),
            "paint must address the resized geometry, not the latched one"
        );
        drop(guard);
    }

    /// Test F16-5. Shrinking re-latches too, which is the dangerous direction:
    /// the old bottom margin may now be past the bottom of the screen.
    #[test]
    #[serial]
    fn resize_relatches_when_the_terminal_shrinks() {
        let _state = GlobalState::capture();
        let (guard, out, err) = install_inline(40, 1, "status: idle");
        let guard = guard.expect("inline mode installs a guard");
        let before = err.text().len();

        assert!(guard.resize(10, 1), "10 rows still clears the bound");

        assert_eq!(
            err.text()[before..].to_string(),
            expected_hide(40, 1) + &expected_show(10, 1),
            "shrinking must release the old region before setting the new one, or the \
             margins are left off-screen"
        );
        assert!(out.is_empty(), "no viewport control bytes reach stdout");
        drop(guard);
    }

    /// Test F16-6. **The decision this card makes.** A resize that takes the
    /// terminal below F19's `rows >= viewport_rows + 2` bound releases the
    /// viewport -- region reset, rows cleared, nothing pinned -- and leaves it
    /// released while the terminal is too short.
    ///
    /// A pinned row that cannot be drawn correctly is worse than no pinned
    /// row, and the alternative -- keeping a region whose bottom margin no
    /// longer fits the screen -- is exactly the corruption F19 was about.
    #[test]
    #[serial]
    fn resize_below_the_f19_bound_releases_the_viewport_and_pins_nothing() {
        let _state = GlobalState::capture();
        let (guard, out, err) = install_inline(24, 1, "status: idle");
        let guard = guard.expect("inline mode installs a guard");
        let before = err.text().len();

        let installed = guard.resize(2, 1);

        assert!(
            !installed,
            "2 rows cannot hold a two-row scrolling region plus a pinned row"
        );
        assert_eq!(
            err.text()[before..].to_string(),
            expected_hide(24, 1),
            "the release must be the full teardown for the geometry that *was* \
             pinned: rows cleared, region reset, cursor parked below the transcript"
        );

        // Released means released: nothing is painted into a terminal with no
        // region, and a further resize that is still too short adds nothing.
        let before = err.text().len();
        guard.paint_draft("> ", "draft", 5, "", false);
        assert!(!guard.resize(1, 1), "still too short, still unpinned");
        assert_eq!(
            &err.text()[before..],
            "",
            "while released, nothing addresses rows that were never pinned"
        );
        assert!(out.is_empty(), "no viewport control bytes reach stdout");
        drop(guard);
    }

    /// Test F16-7. **The round trip**: tall, then too short, then tall again
    /// ends with a correctly installed viewport, and the short interval emitted
    /// a proper release rather than leaving a stale region behind.
    #[test]
    #[serial]
    fn a_resize_round_trip_through_a_too_short_terminal_ends_correctly_installed() {
        let _state = GlobalState::capture();
        let (guard, out, err) = install_inline(24, 1, "status: idle");
        let guard = guard.expect("inline mode installs a guard");
        let before = err.text().len();

        assert!(!guard.resize(2, 1), "too short: released");
        let short = err.text()[before..].to_string();
        assert_eq!(
            short,
            expected_hide(24, 1),
            "the short interval must emit a proper release: {short:?}"
        );

        let before = err.text().len();
        assert!(
            !guard.resize(3, 2),
            "3 rows with a 2-row viewport is still short"
        );
        assert_eq!(
            &err.text()[before..],
            "",
            "a second too-short resize emits nothing: there is nothing left to release"
        );

        let before = err.text().len();
        assert!(guard.resize(30, 1), "tall again: re-installed");
        assert_eq!(
            err.text()[before..].to_string(),
            expected_show(30, 1),
            "re-installing is the setup half alone -- there is no old region to tear \
             down, so a teardown here would clear rows the viewport does not own"
        );

        // Proof it is genuinely pinned again, at the *new* size.
        let before = err.text().len();
        guard.paint_draft("> ", "", 0, "", false);
        assert_eq!(
            &err.text()[before..],
            expected_status_paint(30, 1, "status: idle"),
            "the re-installed viewport paints at the geometry it was re-installed with"
        );
        assert!(out.is_empty(), "no viewport control bytes reach stdout");
        drop(guard);
    }

    /// Test F16-8. A resize must **update** the guard's stored geometry, not
    /// just emit bytes: `BridgeGuard` keeps `rows`/`viewport_rows` so a
    /// suspend's reinstall can re-latch (finding F21), and a resize that left
    /// them alone would put the pre-resize geometry back after the next Ctrl+Z.
    #[test]
    #[serial]
    fn resize_updates_the_geometry_the_next_reinstall_uses() {
        let _state = GlobalState::capture();
        let (guard, _out, err) = install_inline(24, 3, "status: idle");
        let guard = guard.expect("inline mode installs a guard");
        guard.resize(40, 3);
        let before = err.text().len();

        guard.release_for_suspend();
        guard.reinstall_after_resume();

        assert_eq!(
            err.text()[before..].to_string(),
            expected_hide(40, 3)
                + &expected_show(40, 3)
                + &expected_status_paint(40, 3, "status: idle"),
            "a suspend after a resize must round-trip the *resized* geometry"
        );
        drop(guard);
    }

    /// One step of a scripted read: bytes, or a signal that interrupts the
    /// read the way `SIGWINCH` does.
    #[derive(Clone, Copy)]
    enum Step {
        Bytes(&'static [u8]),
        /// A `SIGWINCH`: the handler's `note_resize` runs, then the blocked
        /// `read` fails with `EINTR`. That is the real delivery order, and the
        /// reason the reader needs no self-pipe -- waking the read *is* the
        /// notification.
        Winch,
        /// An `EINTR` with **no** resize pending: some other signal. The
        /// reader must retry silently, exactly as it does today.
        Eintr,
    }

    /// A reader driven by [`Step`]s, so an interrupted read is scriptable
    /// without a real signal arriving mid-`read`.
    struct Signalled(std::collections::VecDeque<Step>);

    impl Signalled {
        fn new<const N: usize>(steps: [Step; N]) -> Self {
            Self(steps.into_iter().collect())
        }
    }

    impl std::io::Read for Signalled {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            match self.0.pop_front() {
                None => Ok(0),
                Some(Step::Winch) => {
                    note_resize();
                    Err(std::io::Error::from(std::io::ErrorKind::Interrupted))
                }
                Some(Step::Eintr) => Err(std::io::Error::from(std::io::ErrorKind::Interrupted)),
                Some(Step::Bytes(chunk)) => {
                    assert!(
                        chunk.len() <= buf.len(),
                        "test chunk larger than the buffer"
                    );
                    buf[..chunk.len()].copy_from_slice(chunk);
                    Ok(chunk.len())
                }
            }
        }
    }

    /// Drive the reader over scripted steps, recording paints and resize
    /// callbacks into **one** ordered log, so their interleaving is observable.
    fn drive_signalled<const N: usize>(steps: [Step; N]) -> (InlineRead, Vec<String>) {
        let log = std::cell::RefCell::new(Vec::<String>::new());
        let mut input = Signalled::new(steps);
        let outcome = {
            let mut paint = |draft: &str, _cursor: usize, _hint: &str, _reserved: bool| {
                log.borrow_mut().push(format!("paint:{draft}"));
            };
            let mut on_resize = || log.borrow_mut().push("resize".to_string());
            read_line_inline(&mut input, &mut paint, &[], &mut on_resize).expect("inline read")
        };
        (outcome, log.into_inner())
    }

    /// Test F16-9. The reader's half of the contract: when a read is
    /// interrupted and the flag is set, the reader handles the resize once,
    /// clears the flag, and goes on reading.
    #[test]
    #[serial]
    fn the_reader_handles_a_pending_resize_when_a_read_is_interrupted() {
        let _ = take_resize();
        let (outcome, log) = drive_signalled([Step::Winch, Step::Bytes(b"x\r")]);

        assert_eq!(
            submitted(&outcome),
            "x",
            "the read continues after the resize"
        );
        assert_eq!(
            log.iter().filter(|event| *event == "resize").count(),
            1,
            "the resize is handled exactly once: {log:?}"
        );
        assert!(
            !take_resize(),
            "the reader must clear the flag, or every later wake would re-latch"
        );
    }

    /// Test F16-10. The draft survives a resize, and the repaint that follows
    /// one shows the **current draft** rather than the status row.
    ///
    /// Ordering is the assertion: the frame immediately after the resize is
    /// the draft as it stood when the signal arrived. A resize that painted
    /// the status would flash the wrong row at a user in mid-sentence, and one
    /// that painted nothing would leave the row blank at the new geometry.
    #[test]
    #[serial]
    fn the_reader_repaints_the_current_draft_after_a_resize() {
        let _ = take_resize();
        let (outcome, log) = drive_signalled([
            Step::Bytes(b"h"),
            Step::Bytes(b"e"),
            Step::Bytes(b"l"),
            Step::Winch,
            Step::Bytes(b"lo\r"),
        ]);

        assert_eq!(
            submitted(&outcome),
            "hello",
            "the draft must survive the resize untouched: the reader keeps reading"
        );
        let at = log
            .iter()
            .position(|event| event == "resize")
            .unwrap_or_else(|| panic!("the resize must be handled: {log:?}"));
        assert_eq!(
            log.get(at + 1).map(String::as_str),
            Some("paint:hel"),
            "the frame right after a resize must repaint the current draft at the new \
             geometry: {log:?}"
        );
    }

    /// Test F16-11. An `EINTR` with no resize pending is retried silently --
    /// the behaviour the reader already has, pinned so the new flag check
    /// cannot turn every other signal into a re-latch.
    #[test]
    #[serial]
    fn an_interrupted_read_without_a_pending_resize_is_retried_silently() {
        let _ = take_resize();
        let (outcome, log) = drive_signalled([
            Step::Bytes(b"a"),
            Step::Eintr,
            Step::Eintr,
            Step::Bytes(b"b\r"),
        ]);

        assert_eq!(submitted(&outcome), "ab");
        assert!(
            !log.iter().any(|event| event == "resize"),
            "no resize was pending, so none may be handled: {log:?}"
        );
        assert_eq!(
            log,
            // The initial empty frame, then the one keystroke. The submitting
            // chunk paints nothing: the reader returns at the `Submit` action.
            vec!["paint:".to_string(), "paint:a".to_string()],
            "a bare EINTR adds no frame at all: {log:?}"
        );
    }

    // ---------------------------------------------------------------------
    // T0-S14: the viewport's height follows its content.
    //
    // `install` is still called with a hardcoded `1` from `main.rs`, and the
    // bordered slash palette needs several rows that grow and shrink as the
    // user types. The height therefore becomes a property of what a caller
    // hands the viewport rather than a constant.
    //
    // The two decisions pinned here:
    //
    //   * the height follows the content, clamped to `terminal_rows - 2`
    //     (F19's bound) -- a request that is too tall is **shortened**, not
    //     refused, because some rows fit;
    //   * when rows must be dropped they go from the **top**, so the composer
    //     (the bottom row) always survives.
    //
    // And the invariants: an unchanged height emits no region bytes, a changed
    // height re-latches through `resize`'s own path, the number of rows painted
    // always equals the latched height, and a suspend after a height change
    // round-trips the *current* height.
    // ---------------------------------------------------------------------

    /// The bytes a multi-row `paint` emits for this geometry: each row
    /// positioned absolutely from the first viewport row down, cleared and
    /// written, then the cursor re-parked inside the region. Derived rather
    /// than literal, exactly as `expected_show`/`expected_status_paint` are.
    fn expected_rows_paint(rows: u16, viewport_rows: u16, content: &[&str]) -> String {
        let bottom = rows - viewport_rows;
        let mut expected = String::new();
        for (i, row) in content.iter().enumerate() {
            let line = bottom as usize + 1 + i;
            expected.push_str(&format!("\x1b[{line};1H\x1b[2K{row}"));
        }
        expected.push_str(&format!("\x1b[{bottom};1H"));
        expected
    }

    fn owned(content: &[&str]) -> Vec<String> {
        content.iter().map(|row| row.to_string()).collect()
    }

    /// Test S14-1. The regression net: one row of content still produces
    /// exactly the bytes it produces today -- the same single positioned,
    /// cleared, written row and the re-park, with **no** region bytes, because
    /// the height did not change from the one `install` latched.
    #[test]
    #[serial]
    fn paint_rows_of_one_row_is_byte_identical_to_todays_one_row_paint() {
        let _state = GlobalState::capture();
        let (guard, out, err) = install_inline(24, 1, "status: idle");
        let guard = guard.expect("inline mode installs a guard");
        let before = err.text().len();

        assert!(guard.paint_rows(&owned(&["status: idle"])));

        assert_eq!(
            err.text()[before..].to_string(),
            expected_status_paint(24, 1, "status: idle"),
            "one row of content must emit exactly the bytes the one-row viewport \
             emits today"
        );
        assert!(out.is_empty(), "no viewport control bytes reach stdout");
        drop(guard);
    }

    /// Test S14-2. An unchanged height must not re-latch: no DECSTBM set, no
    /// release, no cleared rows beyond the paint's own -- exactly as a
    /// same-dimensions resize is `Unchanged` today. Otherwise every keystroke
    /// would tear the region down and set it back up, which the user sees as
    /// the whole viewport flickering.
    #[test]
    #[serial]
    fn paint_rows_at_an_unchanged_height_emits_no_region_bytes() {
        let _state = GlobalState::capture();
        let (guard, _out, err) = install_inline(24, 3, "status: idle");
        let guard = guard.expect("inline mode installs a guard");
        // Latch three rows first, then repaint the same height twice.
        guard.paint_rows(&owned(&["palette", "entry", "> draft"]));
        let before = err.text().len();

        guard.paint_rows(&owned(&["palette", "entry", "> draft!"]));
        guard.paint_rows(&owned(&["palette", "entry", "> draft!!"]));

        let tail = err.text()[before..].to_string();
        assert!(
            !tail.contains("\x1b[1;"),
            "a repaint at an unchanged height must set no scroll region: {tail:?}"
        );
        assert!(!tail.contains("\x1b[r"), "nor release one: {tail:?}");
        assert_eq!(
            tail,
            expected_rows_paint(24, 3, &["palette", "entry", "> draft!"])
                + &expected_rows_paint(24, 3, &["palette", "entry", "> draft!!"]),
            "and the two repaints are exactly two paints: {tail:?}"
        );
        drop(guard);
    }

    /// Test S14-3. Growing re-latches, and it is the same operation a terminal
    /// resize performs with a different second argument: the old geometry's
    /// teardown, the new geometry's setup, then the rows -- which must land at
    /// the **new** absolute lines (on a 10-row terminal a 3-row viewport starts
    /// at row 8, not at row 10).
    #[test]
    #[serial]
    fn paint_rows_growing_relatches_and_paints_at_the_new_absolute_lines() {
        let _state = GlobalState::capture();
        let (guard, out, err) = install_inline(10, 1, "status: idle");
        let guard = guard.expect("inline mode installs a guard");
        let before = err.text().len();

        assert!(guard.paint_rows(&owned(&["palette", "entry", "> draft"])));

        assert_eq!(
            err.text()[before..].to_string(),
            expected_hide(10, 1)
                + &expected_show(10, 3)
                + &expected_rows_paint(10, 3, &["palette", "entry", "> draft"]),
            "a taller request re-latches -- teardown then setup, never a patched \
             margin -- and then paints at the new geometry"
        );
        assert!(out.is_empty(), "no viewport control bytes reach stdout");
        drop(guard);
    }

    /// Test S14-4. Shrinking re-latches too, and the rows move back down: a
    /// one-row viewport on a 10-row terminal is row 10. A shrink that skipped
    /// the re-latch would leave the region's bottom margin two rows high and a
    /// stale painted row stranded above the composer.
    #[test]
    #[serial]
    fn paint_rows_shrinking_relatches_and_paints_at_the_new_absolute_lines() {
        let _state = GlobalState::capture();
        let (guard, out, err) = install_inline(10, 1, "status: idle");
        let guard = guard.expect("inline mode installs a guard");
        guard.paint_rows(&owned(&["palette", "entry", "> draft"]));
        let before = err.text().len();

        assert!(guard.paint_rows(&owned(&["> draft"])));

        assert_eq!(
            err.text()[before..].to_string(),
            expected_hide(10, 3)
                + &expected_show(10, 1)
                + &expected_rows_paint(10, 1, &["> draft"]),
            "a shorter request re-latches at the smaller height and paints there"
        );
        assert!(out.is_empty(), "no viewport control bytes reach stdout");
        drop(guard);
    }

    /// Test S14-5. **The headline.** Five rows requested on a terminal that can
    /// hold three: the request is clamped rather than refused, and the three
    /// rows that remain are the **last** three, in order -- the composer, the
    /// row the user is typing into, is never the row that goes.
    #[test]
    #[serial]
    fn paint_rows_clamps_a_too_tall_request_and_keeps_the_last_rows() {
        let _state = GlobalState::capture();
        // Five rows: two must stay scrolling above the viewport (F19), so the
        // usable height is three.
        let (guard, out, err) = install_inline(5, 1, "status: idle");
        let guard = guard.expect("inline mode installs a guard");
        let before = err.text().len();

        let pinned = guard.paint_rows(&owned(&["one", "two", "three", "four", "> composer"]));

        assert!(
            pinned,
            "a request too tall for the terminal is shortened, not refused: some \
             rows fit, so a viewport stays pinned"
        );
        assert_eq!(
            err.text()[before..].to_string(),
            expected_hide(5, 1)
                + &expected_show(5, 3)
                + &expected_rows_paint(5, 3, &["three", "four", "> composer"]),
            "the viewport latches three rows and paints the LAST three of the five \
             requested, in order -- rows are dropped from the top so the composer \
             survives"
        );
        assert!(out.is_empty(), "no viewport control bytes reach stdout");
        drop(guard);
    }

    /// Test S14-6. The property: the number of rows painted always equals the
    /// latched height. Painting more would put the extra rows inside the scroll
    /// region, where they scroll away with the transcript -- and painting fewer
    /// would leave a stale row pinned.
    ///
    /// Finding F27: a full repaint repairs its own damage, so the count is taken
    /// from a **second** paint at the same height, which emits the paint alone
    /// with no teardown clears mixed into it.
    #[test]
    #[serial]
    fn paint_rows_never_paints_more_rows_than_the_viewport_is_tall() {
        let _state = GlobalState::capture();
        for rows in 3u16..=12 {
            for requested in 1usize..=8 {
                let content: Vec<String> = (0..requested).map(|i| format!("row {i}")).collect();
                let expected = (requested as u16).min(rows - 2);

                let (guard, _out, err) = install_inline(rows, 1, "status: idle");
                let guard = guard.expect("inline mode installs a guard");
                guard.paint_rows(&content);
                let before = err.text().len();
                guard.paint_rows(&content);
                let tail = err.text()[before..].to_string();

                assert!(
                    !tail.contains("\x1b[1;") && !tail.contains("\x1b[r"),
                    "the second paint at the same height must not re-latch \
                     ({rows}, {requested}): {tail:?}"
                );
                assert_eq!(
                    tail.matches("\x1b[2K").count(),
                    expected as usize,
                    "paint_rows on a {rows}-row terminal with {requested} rows requested \
                     must draw exactly {expected} rows -- any extra lands inside the \
                     scroll region and scrolls away with the transcript: {tail:?}"
                );
                // And they are the last `expected` rows, at the right lines.
                let kept: Vec<&str> = content[requested - expected as usize..]
                    .iter()
                    .map(String::as_str)
                    .collect();
                assert_eq!(
                    tail,
                    expected_rows_paint(rows, expected, &kept),
                    "and at the latched geometry's own lines ({rows}, {requested})"
                );
                drop(guard);
            }
        }
    }

    /// Test S14-7. A height change followed by a suspend/resume round trip must
    /// reinstall the **current** height, not the one `install` was given --
    /// `BridgeGuard` stores its dimensions, so this is the same trap
    /// `resize_updates_the_geometry_the_next_reinstall_uses` pinned for a
    /// terminal resize.
    #[test]
    #[serial]
    fn paint_rows_updates_the_height_the_next_reinstall_uses() {
        let _state = GlobalState::capture();
        let (guard, _out, err) = install_inline(24, 1, "status: idle");
        let guard = guard.expect("inline mode installs a guard");
        guard.paint_rows(&owned(&["palette", "entry", "> draft"]));
        let before = err.text().len();

        guard.release_for_suspend();
        guard.reinstall_after_resume();

        assert_eq!(
            err.text()[before..].to_string(),
            expected_hide(24, 3)
                + &expected_show(24, 3)
                + &expected_status_paint(24, 3, "status: idle"),
            "a suspend after a height change must round-trip the *current* height"
        );
        drop(guard);
    }

    /// Test S14-8. Each row is still width-fitted (T0-S13), so a long palette
    /// row cannot wrap into its neighbour -- the fitting is `paint`'s and must
    /// not be bypassed by the taller viewport.
    #[test]
    #[serial]
    fn paint_rows_width_fits_every_row_so_none_can_wrap_into_its_neighbour() {
        let _state = GlobalState::capture();
        let (guard, _out, err) = install_inline(24, 1, "status: idle");
        let guard = guard.expect("inline mode installs a guard");
        let long = "x".repeat(live::terminal_columns() + 40);
        let before = err.text().len();

        guard.paint_rows(&owned(&[&long, "> draft"]));

        let tail = err.text()[before..].to_string();
        let painted = tail
            .split("\x1b[2K")
            .nth(1)
            .unwrap_or_else(|| panic!("a row is painted: {tail:?}"));
        let body = painted
            .split("\x1b[")
            .next()
            .expect("the row's text precedes the next position");
        assert!(
            kitsu_render::sanitize_terminal_text(body).chars().count() <= live::terminal_columns(),
            "an over-long palette row must be cut to the terminal's width, not \
             wrapped into the row below: {} columns",
            kitsu_render::sanitize_terminal_text(body).chars().count()
        );
        drop(guard);
    }

    // ---------------------------------------------------------------------
    // T0-S15: the bordered slash palette as data.
    //
    // T0-S14 settled that a viewport too short for its content drops the
    // **topmost** rows, so the composer survives. For a list that is right;
    // for a bordered box it removes the top border and its ` slash commands `
    // label, leaving a frame with no lid. The palette therefore has to be
    // *built to fit* the height it will be granted -- hence
    // `BridgeGuard::granted_rows`, which answers "how many rows would I get?"
    // before the content is laid out, and `palette_rows`, which lays it out to
    // that budget.
    //
    // Nothing is wired here: no reader, no keystroke, no repaint policy. This
    // is the data, and the next card decides when it appears.
    // ---------------------------------------------------------------------

    /// The classic palette's rows, as `slash_palette` renders them for the
    /// same prefix and width. `slash_palette` opens with a bare `\n` (it is a
    /// Rustyline hint appended under the composer) and ends with no newline,
    /// so the rows are its lines with that leading blank dropped.
    ///
    /// Derived from `slash_palette` itself rather than written out, so the two
    /// palettes cannot drift apart without a test failing.
    fn classic_rows(prefix: &str, rich: bool, inner_width: usize) -> Vec<String> {
        crate::slash_palette(prefix, rich, inner_width)
            .trim_start_matches('\n')
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn is_top_border(row: &str, rich: bool) -> bool {
        row.starts_with(if rich { '\u{256d}' } else { '+' })
            && row.ends_with(if rich { '\u{256e}' } else { '+' })
            && row.contains("slash commands")
    }

    fn is_footer(row: &str, rich: bool) -> bool {
        row.starts_with(if rich { '\u{251c}' } else { '+' })
            && row.ends_with(if rich { '\u{2524}' } else { '+' })
            && row.contains("type to filter")
    }

    /// The trailing "N more" row's count, when there is one.
    fn more_count(rows: &[String]) -> Option<usize> {
        rows.iter()
            .find(|row| row.contains("more \u{2014} keep typing"))
            .map(|row| {
                let digits: String = row
                    .chars()
                    .skip_while(|character| !character.is_ascii_digit())
                    .take_while(char::is_ascii_digit)
                    .collect();
                digits
                    .parse()
                    .unwrap_or_else(|_| panic!("the more row carries a count: {row:?}"))
            })
    }

    /// Command rows: everything that is neither frame nor the "N more" row.
    fn entry_rows(rows: &[String], rich: bool) -> usize {
        rows.iter()
            .filter(|row| {
                !is_top_border(row, rich)
                    && !is_footer(row, rich)
                    && !row.contains("more \u{2014} keep typing")
            })
            .count()
    }

    const PALETTE_WIDTH: usize = crate::COMPOSER_SURFACE_DEFAULT_INNER_WIDTH;

    /// Test S15-1. `granted_rows` is a **query** on the guard, not a second
    /// copy of F19's bound: for every terminal height and request it answers
    /// exactly the height `paint_rows` goes on to latch for the same request.
    /// The palette must be able to size itself in advance without computing
    /// the clamp -- a caller-side clamp would be free to drift from the one
    /// the paint actually applies.
    #[test]
    #[serial]
    fn granted_rows_answers_exactly_what_paint_rows_latches() {
        let _state = GlobalState::capture();
        for rows in 3u16..=12 {
            for requested in 1u16..=10 {
                let (guard, _out, err) = install_inline(rows, 1, "status: idle");
                let guard = guard.expect("inline mode installs a guard");

                let granted = guard.granted_rows(requested);
                assert_eq!(
                    granted,
                    requested.min(rows - 2),
                    "a {requested}-row request on a {rows}-row terminal is clamped to \
                     F19's bound, not refused"
                );

                // And it agrees with the paint: the second paint at the same
                // height emits the paint alone (finding F27), so the number of
                // cleared rows is the latched height.
                let content: Vec<String> = (0..requested).map(|i| format!("row {i}")).collect();
                guard.paint_rows(&content);
                let before = err.text().len();
                guard.paint_rows(&content);
                assert_eq!(
                    err.text()[before..].matches("\x1b[2K").count(),
                    granted as usize,
                    "granted_rows({requested}) on a {rows}-row terminal must equal the \
                     rows paint_rows draws"
                );
                drop(guard);
            }
        }
    }

    /// Test S15-2. **The headline.** The box is always well formed: if any
    /// rows come back, the first is the top border and the last is the footer
    /// separator -- for every height from the minimum upward, both frames, and
    /// prefixes matching zero, one, few and many commands. This is the
    /// invariant T0-S14's drop-from-the-top rule would violate if the palette
    /// were built at full size and then clamped.
    #[test]
    fn palette_rows_is_a_well_formed_box_at_every_height_it_returns_one() {
        for buffer in ["/", "/s", "/mod", "/model", "/zzz"] {
            for rich in [true, false] {
                for max_rows in PALETTE_MIN_ROWS..=14 {
                    let rows = palette_rows(buffer, rich, PALETTE_WIDTH, max_rows);
                    assert!(
                        !rows.is_empty(),
                        "{buffer:?} at {max_rows} rows (rich={rich}) fits a box, so one \
                         must come back"
                    );
                    assert!(
                        is_top_border(&rows[0], rich),
                        "the first row must be the top border with its label \
                         ({buffer:?}, {max_rows}, rich={rich}): {rows:?}"
                    );
                    assert!(
                        is_footer(rows.last().expect("non-empty"), rich),
                        "the last row must be the footer separator ({buffer:?}, \
                         {max_rows}, rich={rich}): {rows:?}"
                    );
                    assert!(
                        rows.len() <= max_rows as usize,
                        "{} rows exceeds the {max_rows} granted ({buffer:?})",
                        rows.len()
                    );
                    assert!(
                        rows.iter().all(|row| !row.contains('\n')),
                        "each row is one complete line with no newline: {rows:?}"
                    );
                }
            }
        }
    }

    /// Test S15-3. Below a minimal box -- the top border, one content row and
    /// the footer -- there is **no** palette rather than a broken one. A lid
    /// with no frame, or a frame with no lid, is worse than nothing.
    #[test]
    fn palette_rows_declines_a_height_too_small_for_a_minimal_box() {
        assert_eq!(PALETTE_MIN_ROWS, 3, "top border + one content row + footer");
        for buffer in ["/", "/mod", "/zzz"] {
            for rich in [true, false] {
                for max_rows in 0..PALETTE_MIN_ROWS {
                    assert!(
                        palette_rows(buffer, rich, PALETTE_WIDTH, max_rows).is_empty(),
                        "{max_rows} rows cannot hold a box, so {buffer:?} must yield no \
                         palette at all (rich={rich})"
                    );
                }
                assert_eq!(
                    palette_rows(buffer, rich, PALETTE_WIDTH, PALETTE_MIN_ROWS).len(),
                    PALETTE_MIN_ROWS as usize,
                    "and at exactly the minimum it is exactly a minimal box"
                );
            }
        }
    }

    /// Test S15-4. Entries are what gets squeezed, never the frame: as the
    /// granted height shrinks the number of command rows falls monotonically
    /// while the border and footer survive every step.
    #[test]
    fn palette_rows_squeezes_entries_and_never_the_frame() {
        let rich = true;
        let mut previous = usize::MAX;
        for max_rows in (PALETTE_MIN_ROWS..=14).rev() {
            let rows = palette_rows("/", rich, PALETTE_WIDTH, max_rows);
            let entries = entry_rows(&rows, rich);
            assert!(
                entries <= previous,
                "entries must not grow as the height shrinks ({max_rows} rows): \
                 {entries} after {previous}"
            );
            previous = entries;
            assert!(
                is_top_border(&rows[0], rich) && is_footer(rows.last().expect("non-empty"), rich),
                "the frame survives every squeeze ({max_rows} rows): {rows:?}"
            );
        }
        assert!(
            entry_rows(
                &palette_rows("/", rich, PALETTE_WIDTH, PALETTE_MIN_ROWS),
                rich
            ) < entry_rows(&palette_rows("/", rich, PALETTE_WIDTH, 14), rich),
            "a minimal box shows strictly fewer commands than a generous one"
        );
    }

    /// Test S15-5. The "N more" row is **honest**. Entries hidden by the
    /// height count exactly as much as entries hidden by `SLASH_PALETTE_ROWS`:
    /// at every height, either every match is on screen and there is no more
    /// row, or the more row's count is precisely the number of matches that
    /// are not. Pinned against prefixes matching many commands at small
    /// heights, which is where a count copied from the classic palette's
    /// `matches.len() - SLASH_PALETTE_ROWS` would quietly lie.
    #[test]
    fn palette_rows_more_count_reflects_entries_hidden_by_the_height() {
        for (buffer, prefix) in [("/", ""), ("/s", "s"), ("/m", "m"), ("/mod", "mod")] {
            let total = crate::matching_slash_commands(prefix).len();
            for rich in [true, false] {
                for max_rows in PALETTE_MIN_ROWS..=14 {
                    let rows = palette_rows(buffer, rich, PALETTE_WIDTH, max_rows);
                    let entries = entry_rows(&rows, rich);
                    match more_count(&rows) {
                        Some(more) => assert_eq!(
                            more,
                            total - entries,
                            "{buffer:?} at {max_rows} rows shows {entries} of {total} \
                             commands, so the more row must say {} (rich={rich}): {rows:?}",
                            total - entries
                        ),
                        None => assert_eq!(
                            entries, total,
                            "{buffer:?} at {max_rows} rows has no more row, so every one \
                             of its {total} matches must be on screen (rich={rich}): \
                             {rows:?}"
                        ),
                    }
                }
            }
        }
    }

    /// Test S15-6. Content parity with the classic Rustyline palette: for a
    /// height that holds the whole thing, the rows are byte-for-byte the text
    /// `slash_palette` produces for the same prefix and width -- asserted
    /// against `slash_palette` itself, never against literals, so the two
    /// cannot drift. A generous height must not add rows either.
    #[test]
    fn palette_rows_are_the_classic_palettes_rows_where_both_fit() {
        for (buffer, prefix) in [("/", ""), ("/s", "s"), ("/mod", "mod"), ("/zzz", "zzz")] {
            for inner_width in [30, PALETTE_WIDTH] {
                for rich in [true, false] {
                    let classic = classic_rows(prefix, rich, inner_width);
                    let exact = u16::try_from(classic.len()).expect("a small box");
                    assert_eq!(
                        palette_rows(buffer, rich, inner_width, exact),
                        classic,
                        "{buffer:?} at its exact height (width {inner_width}, rich={rich}) \
                         must be the classic palette's own rows"
                    );
                    assert_eq!(
                        palette_rows(buffer, rich, inner_width, exact + 5),
                        classic,
                        "and extra height adds nothing ({buffer:?}, width {inner_width}, \
                         rich={rich})"
                    );
                }
            }
        }
    }

    /// Test S15-7. The `rich` flag picks the frame, matching `slash_palette`:
    /// Unicode box drawing when rich, pure ASCII when not -- and no stray
    /// Unicode frame character survives into the ASCII form.
    #[test]
    fn palette_rows_frame_follows_the_rich_flag() {
        let rich = palette_rows("/mod", true, PALETTE_WIDTH, 6);
        assert!(
            rich.iter()
                .all(|row| row.starts_with(['\u{256d}', '\u{2502}', '\u{251c}'])),
            "every rich row opens with a box-drawing character: {rich:?}"
        );

        let plain = palette_rows("/mod", false, PALETTE_WIDTH, 6);
        assert!(
            plain.iter().all(|row| row.starts_with(['+', '|'])),
            "every ASCII row opens with + or |: {plain:?}"
        );
        assert!(
            plain
                .iter()
                .all(|row| !row.contains(['\u{256d}', '\u{2502}', '\u{251c}', '\u{256e}'])),
            "no Unicode frame leaks into the ASCII palette: {plain:?}"
        );
        assert!(
            plain.iter().any(|row| row.contains("/model")),
            "and it is still the same content: {plain:?}"
        );
    }

    /// Test S15-8. No palette at all for a buffer that is not a single-token
    /// slash prefix: an empty buffer, ordinary text, a command with arguments,
    /// a multi-line paste, a leading space. These are exactly the cases
    /// `ChatHelper::hint` declines in `main.rs`, and the two must agree.
    #[test]
    fn palette_rows_is_empty_for_anything_that_is_not_a_slash_prefix() {
        for buffer in [
            "",
            "model",
            "ordinary prompt",
            "/model qwen",
            "/mod ",
            " /mod",
            "/mo\nde",
            "line\n/mod",
        ] {
            for rich in [true, false] {
                assert!(
                    palette_rows(buffer, rich, PALETTE_WIDTH, 14).is_empty(),
                    "{buffer:?} is not a slash prefix, so it opens no palette \
                     (rich={rich})"
                );
            }
        }
    }

    // ---------------------------------------------------------------------
    // T0-S16: the palette, wired. The last card for R4.
    //
    // T0-S15 built the palette as *data* (`palette_rows`) and the height
    // *query* (`granted_rows`); T0-S14 built the multi-row paint
    // (`paint_rows`). Nothing joined them up: the reader still paints through
    // the one-row `paint_draft`. These tests pin the join, through one new
    // seam -- `BridgeGuard::paint_composer`, which is the whole composer for
    // one draft: the palette's rows, if the draft has one, followed by the
    // composer row as the **last** row.
    //
    // The seam is on the guard rather than inside `read_line_inline`'s paint
    // closure on purpose. The closure is the *caller's* (the test's, in this
    // module), so a reader-level test would be asserting on its own painter
    // and could not observe a partial repaint at all. On the guard the bytes
    // one call emits are observable, which is what finding F27 says to
    // measure: a full repaint repairs its own damage, so only the emission of
    // a *single* paint can tell a stale palette from a fresh one.
    // ---------------------------------------------------------------------

    /// The content `paint_composer` must paint, derived the only way that
    /// leaves a bordered box well formed: ask what height the viewport will
    /// grant **first**, spend one row of it on the composer, and build the
    /// palette into what is left. Never build the palette full and let
    /// `fit_viewport_rows` clamp it -- that drops rows from the top, which for
    /// a box is its lid.
    fn expected_composer_content(
        guard: &BridgeGuard,
        prompt: &str,
        draft: &str,
        cursor: usize,
        hint: &str,
        hint_reserved: bool,
        rich: bool,
    ) -> Vec<String> {
        let columns = live::terminal_columns();
        // Everything the terminal would grant, less the composer's own row.
        let budget = guard.granted_rows(u16::MAX).saturating_sub(1);
        let mut content = palette_rows(
            draft,
            rich,
            crate::composer_surface_inner_width(Some(columns)),
            budget,
        );
        let composed = live::LiveDraft {
            prompt: prompt.to_string(),
            buffer: draft.to_string(),
            cursor,
            hint: hint.to_string(),
            hint_reserved,
        };
        content.push(live::compose_input_row(&composed, columns).0);
        content
    }

    /// The bytes those rows paint at this geometry.
    fn expected_composer_paint(rows: u16, content: &[String]) -> String {
        let borrowed: Vec<&str> = content.iter().map(String::as_str).collect();
        expected_rows_paint(
            rows,
            u16::try_from(content.len()).expect("a viewport height fits a u16"),
            &borrowed,
        )
    }

    /// The absolute screen lines a run of bytes positioned, cleared and wrote,
    /// in emission order: every row write is `ESC[<line>;1H` then `ESC[2K`
    /// then the text, so each clear is preceded by exactly its own address.
    ///
    /// This is the F27 instrument. The *count* is what a partial repaint gets
    /// wrong: a reader that repaints through the one-row path emits one row
    /// per keystroke no matter how tall the viewport is, and the palette above
    /// it keeps showing matches for a prefix the user has already changed.
    fn painted_lines(bytes: &str) -> Vec<u16> {
        bytes
            .match_indices("\x1b[2K")
            .map(|(at, _)| {
                let before = &bytes[..at];
                let start = before.rfind("\x1b[").unwrap_or_else(|| {
                    panic!("a clear with no cursor address before it: {bytes:?}")
                });
                before[start + 2..]
                    .strip_suffix(";1H")
                    .unwrap_or_else(|| panic!("a clear not preceded by a row address: {bytes:?}"))
                    .parse()
                    .unwrap_or_else(|_| panic!("a row address that is not a number: {bytes:?}"))
            })
            .collect()
    }

    /// The prefixes the narrowing tests type, with the palette height each
    /// implies on a terminal tall enough for all of it. Derived from the
    /// registry rather than written down, so adding a command cannot silently
    /// turn these into assertions about nothing.
    fn palette_height(prefix: &str) -> usize {
        palette_rows(
            prefix,
            true,
            crate::COMPOSER_SURFACE_DEFAULT_INNER_WIDTH,
            u16::MAX,
        )
        .len()
    }

    /// Test S16-1. **The headline.** A slash prefix at the inline prompt
    /// paints the palette's rows followed by the composer row, in that order,
    /// and the viewport grows to hold both -- the top border on the topmost
    /// pinned row, the footer separator directly above the composer.
    #[test]
    #[serial]
    fn paint_composer_puts_the_palette_above_the_composer_and_grows_the_viewport() {
        let _state = GlobalState::capture();
        let (guard, out, err) = install_inline(24, 1, "status: idle");
        let guard = guard.expect("inline mode installs a guard");
        let before = err.text().len();

        let content = expected_composer_content(&guard, "kitsu> ", "/co", 3, "", false, true);
        let height = u16::try_from(content.len()).expect("height");
        assert!(
            height > 1,
            "the fixture is only meaningful if `/co` has a palette at all: {content:?}"
        );

        assert!(guard.paint_composer("kitsu> ", "/co", 3, "", false, true));

        let tail = err.text()[before..].to_string();
        assert_eq!(
            tail,
            expected_hide(24, 1)
                + &expected_show(24, height)
                + &expected_composer_paint(24, &content),
            "a slash prefix must re-latch taller and paint the palette above the composer"
        );
        // Independently of the derivation above: what landed on screen is a
        // box whose lid is the topmost pinned row and whose last row is the
        // composer.
        let painted: Vec<String> = tail
            .split("\x1b[2K")
            .skip(1)
            .map(|chunk| {
                chunk
                    .split_once("\x1b[")
                    .map_or(chunk, |(text, _)| text)
                    .to_string()
            })
            .collect();
        let rows: Vec<&String> = painted.iter().filter(|row| !row.is_empty()).collect();
        assert!(
            is_top_border(rows[0], true),
            "the topmost pinned row must be the palette's lid, but it is {:?}",
            rows[0]
        );
        assert!(
            is_footer(rows[rows.len() - 2], true),
            "the footer separator must sit directly above the composer, but \
             that row is {:?}",
            rows[rows.len() - 2]
        );
        assert!(
            rows[rows.len() - 1].contains("/co"),
            "and the composer must be the last row, but it is {:?}",
            rows[rows.len() - 1]
        );
        assert!(out.is_empty(), "no viewport control bytes reach stdout");
        drop(guard);
    }

    /// Test S16-2. Typing narrows the list, so the viewport's height follows
    /// it down: `/co` matches three commands and `/cont` one, so the second
    /// paint re-latches **shorter** and the entries left are only the ones
    /// that still match.
    #[test]
    #[serial]
    fn paint_composer_narrows_the_palette_as_the_prefix_grows() {
        let _state = GlobalState::capture();
        assert!(
            palette_height("/cont") < palette_height("/co"),
            "the fixture needs a prefix that genuinely narrows: /co gives {} rows \
             and /cont {}",
            palette_height("/co"),
            palette_height("/cont")
        );
        let (guard, _out, err) = install_inline(24, 1, "status: idle");
        let guard = guard.expect("inline mode installs a guard");

        let wide = expected_composer_content(&guard, "kitsu> ", "/co", 3, "", false, true);
        guard.paint_composer("kitsu> ", "/co", 3, "", false, true);
        let before = err.text().len();

        let narrow = expected_composer_content(&guard, "kitsu> ", "/cont", 5, "", false, true);
        guard.paint_composer("kitsu> ", "/cont", 5, "", false, true);

        assert!(
            narrow.len() < wide.len(),
            "narrowing must shorten the content: {} rows then {}",
            wide.len(),
            narrow.len()
        );
        let height = u16::try_from(narrow.len()).expect("height");
        let tail = err.text()[before..].to_string();
        assert_eq!(
            tail,
            expected_hide(24, u16::try_from(wide.len()).expect("height"))
                + &expected_show(24, height)
                + &expected_composer_paint(24, &narrow),
            "a narrowed list must re-latch at the shorter height and repaint there"
        );
        assert!(
            !tail.contains("/compact"),
            "and must not leave an entry that no longer matches: {tail:?}"
        );
        drop(guard);
    }

    /// Test S16-3. The other direction, which matters more: a palette that
    /// appears and never leaves is worse than none. Clearing the slash prefix
    /// **and** making it no longer a bare single-token slash prefix (a space
    /// is enough) both dismiss it and put the viewport back to one row.
    #[test]
    #[serial]
    fn paint_composer_dismisses_the_palette_and_returns_the_viewport_to_one_row() {
        let _state = GlobalState::capture();
        for (draft, cursor) in [("/co ", 4usize), ("", 0)] {
            let (guard, _out, err) = install_inline(24, 1, "status: idle");
            let guard = guard.expect("inline mode installs a guard");
            let open = expected_composer_content(&guard, "kitsu> ", "/co", 3, "", false, true);
            guard.paint_composer("kitsu> ", "/co", 3, "", false, true);
            let before = err.text().len();

            guard.paint_composer("kitsu> ", draft, cursor, "", false, true);

            let tail = err.text()[before..].to_string();
            // Count over the PAINT only, not the whole tail: the re-latch's
            // teardown clears every row of the old viewport, row 24 among
            // them, so counting across both double-counts and no
            // implementation could reach 1. S16-4 splits the same way.
            let paint = tail
                .split_once(&expected_show(24, 1))
                .map(|(_, after)| after.to_string())
                .unwrap_or_else(|| tail.clone());
            assert_eq!(
                painted_lines(&paint)
                    .into_iter()
                    .filter(|line| *line == 24)
                    .count(),
                1,
                "dismissing to {draft:?} must leave exactly the bottom row \
                 painted: {tail:?}"
            );
            assert!(
                tail.starts_with(&expected_hide(
                    24,
                    u16::try_from(open.len()).expect("height")
                )),
                "the taller region must be torn down: {tail:?}"
            );
            assert!(
                tail.contains(&expected_show(24, 1)),
                "and a one-row viewport latched again: {tail:?}"
            );
            assert!(
                !tail.contains("slash commands") && !tail.contains("type to filter"),
                "and no frame may survive the dismissal: {tail:?}"
            );
            drop(guard);
        }
    }

    /// Test S16-4. **Finding F27, applied.** One keystroke must redraw
    /// **every** pinned row, not just the composer.
    ///
    /// This is asserted on what a *single* `paint_composer` emits, and the
    /// load-bearing assertion is the row **count**: F27 recorded that a full
    /// repaint repairs its own damage, so a test that paints everything and
    /// then reads the screen cannot see a partial repaint at all. A reader
    /// that repaints through the one-row `paint_draft` -- which is what the
    /// code does today -- emits exactly one row here however tall the viewport
    /// is, leaving the palette above the composer showing matches for a prefix
    /// the user has already changed.
    #[test]
    #[serial]
    fn paint_composer_repaints_every_pinned_row_on_one_keystroke() {
        let _state = GlobalState::capture();
        let (guard, _out, err) = install_inline(24, 1, "status: idle");
        let guard = guard.expect("inline mode installs a guard");
        // The palette is open and latched before the keystroke under test.
        guard.paint_composer("kitsu> ", "/co", 3, "", false, true);
        let before = err.text().len();

        let content = expected_composer_content(&guard, "kitsu> ", "/cont", 5, "", false, true);
        let height = u16::try_from(content.len()).expect("height");
        assert!(height >= 4, "the fixture needs a real box: {content:?}");

        guard.paint_composer("kitsu> ", "/cont", 5, "", false, true);

        // Only the paint's own rows, with the re-latch's teardown clears
        // dropped: the teardown is `expected_hide`, whose length is known.
        let tail = err.text()[before..].to_string();
        let paint = tail
            .split_once(&expected_show(24, height))
            .map(|(_, rest)| rest.to_string())
            .unwrap_or_else(|| panic!("the keystroke must latch {height} rows: {tail:?}"));
        let lines = painted_lines(&paint);
        let expected: Vec<u16> = (24 - height + 1..=24).collect();
        assert_eq!(
            lines,
            expected,
            "one keystroke must redraw every pinned row from the top border down \
             to the composer, in order; it redrew {} of {height} rows ({lines:?}) \
             -- a repaint that touches only the composer leaves a stale palette \
             above it: {paint:?}",
            lines.len()
        );
        drop(guard);
    }

    /// Test S16-5. A resize with the palette open keeps the palette's height.
    ///
    /// `main.rs`'s `on_resize` calls `bridge.resize(rows, 1)` -- a hardcoded
    /// one row, which silently collapses an open palette to its bottom row and
    /// leaves the rest of the box inside the scroll region, where the next
    /// transcript line scrolls it away. `resize_rows` is the same re-probe
    /// with the *current* height preserved.
    #[test]
    #[serial]
    fn resize_rows_preserves_an_open_palettes_height() {
        let _state = GlobalState::capture();
        let (guard, _out, err) = install_inline(24, 1, "status: idle");
        let guard = guard.expect("inline mode installs a guard");
        let content = expected_composer_content(&guard, "kitsu> ", "/co", 3, "", false, true);
        let height = u16::try_from(content.len()).expect("height");
        guard.paint_composer("kitsu> ", "/co", 3, "", false, true);
        let before = err.text().len();

        assert!(guard.resize_rows(30));

        let tail = err.text()[before..].to_string();
        assert_eq!(
            tail,
            expected_hide(24, height) + &expected_show(30, height),
            "a resize must re-latch at the taller terminal with the palette's \
             own height, not collapse it to one row"
        );
        // And the height really is latched, not merely emitted: the next paint
        // lands on the new terminal's rows for that height.
        let after = err.text().len();
        guard.paint_composer("kitsu> ", "/co", 3, "", false, true);
        assert_eq!(
            painted_lines(&err.text()[after..]),
            (30 - height + 1..=30).collect::<Vec<_>>(),
            "the repaint after the resize must use the preserved height"
        );
        drop(guard);
    }

    /// Test S16-6. A terminal too short for the whole palette gets a
    /// **smaller box**, not a clamped one: the rows are requested at the
    /// height that will be granted, so the lid survives.
    ///
    /// Six rows leaves four for the viewport (F19), one of which is the
    /// composer, so the palette is exactly `PALETTE_MIN_ROWS` -- a top border,
    /// one content row and the footer.
    #[test]
    #[serial]
    fn paint_composer_on_a_short_terminal_yields_a_smaller_well_formed_box() {
        let _state = GlobalState::capture();
        let (guard, _out, err) = install_inline(6, 1, "status: idle");
        let guard = guard.expect("inline mode installs a guard");
        let before = err.text().len();

        let content = expected_composer_content(&guard, "kitsu> ", "/co", 3, "", false, true);
        assert_eq!(
            content.len(),
            usize::from(PALETTE_MIN_ROWS) + 1,
            "the fixture: a minimal box plus the composer"
        );
        assert!(
            is_top_border(&content[0], true) && is_footer(&content[content.len() - 2], true),
            "and it is a box with both a lid and a footer: {content:?}"
        );

        assert!(guard.paint_composer("kitsu> ", "/co", 3, "", false, true));

        let tail = err.text()[before..].to_string();
        assert_eq!(
            tail,
            expected_hide(6, 1)
                + &expected_show(6, PALETTE_MIN_ROWS + 1)
                + &expected_composer_paint(6, &content),
            "a short terminal must get a box built to fit, never a full box \
             with its top rows clamped off"
        );
        drop(guard);
    }

    /// Test S16-7. Below `PALETTE_MIN_ROWS + 1` there is no palette at all:
    /// just the composer, and not one row of frame. A partial frame -- a
    /// footer with no lid, or a lid with no footer -- is worse than no
    /// palette.
    #[test]
    #[serial]
    fn paint_composer_below_the_palette_minimum_paints_only_the_composer() {
        let _state = GlobalState::capture();
        for rows in 3..=PALETTE_MIN_ROWS + 2 {
            let (guard, _out, err) = install_inline(rows, 1, "status: idle");
            let guard = guard.expect("inline mode installs a guard");
            let before = err.text().len();

            guard.paint_composer("kitsu> ", "/co", 3, "", false, true);

            let tail = err.text()[before..].to_string();
            assert_eq!(
                painted_lines(&tail).len(),
                1,
                "a {rows}-row terminal cannot hold a box and a composer, so the \
                 composer is all there is: {tail:?}"
            );
            assert!(
                !tail.contains('\u{256d}')
                    && !tail.contains('\u{251c}')
                    && !tail.contains("slash commands"),
                "and no partial frame may be drawn on a {rows}-row terminal: {tail:?}"
            );
            assert!(
                tail.contains("/co"),
                "the composer itself must still be painted: {tail:?}"
            );
            drop(guard);
        }
    }

    /// Card T0-S17, finding F30. `/s` at the idle prompt hints `ave`, which
    /// completes the lead palette entry, so the row gets its palette instead
    /// of standing down.
    #[test]
    #[serial]
    fn paint_composer_opens_a_palette_for_a_single_letter_prefix() {
        let _state = GlobalState::capture();
        let (guard, _out, err) = install_inline(24, 1, "status: idle");
        let guard = guard.expect("inline mode installs a guard");
        let hint = crate::idle_slash_hint("/s");
        assert_eq!(hint, "ave", "the idle hint must complete the lead entry");
        let before = err.text().len();

        guard.paint_composer("kitsu> ", "/s", 2, &hint, false, true);

        let tail = err.text()[before..].to_string();
        assert!(
            tail.contains('\u{256d}') && tail.contains("slash commands"),
            "/s must now open the palette box: {tail:?}"
        );
        assert!(
            tail.contains("save"),
            "and the box must lead with the registry's first /s match: {tail:?}"
        );
        drop(guard);
    }

    /// Test S16-8. The regression net for the whole card: a draft that is not
    /// a slash prefix paints exactly the bytes it paints today -- one row,
    /// byte for byte `paint_draft`'s, with no region bytes, because the height
    /// did not change.
    ///
    /// This is the test that must keep passing while the seven above go green,
    /// together with the existing `paint_draft_*` byte assertions.
    #[test]
    #[serial]
    fn paint_composer_of_a_plain_draft_is_byte_identical_to_paint_draft() {
        let _state = GlobalState::capture();
        for (draft, cursor, hint, reserved) in [
            ("half-typed", 10usize, "", false),
            // Card T0-S17, finding F30: `/s` with the mid-turn hint `teer`
            // used to land here, because the disagreeing hint stood the
            // palette down. At the idle prompt `/s` now hints `ave`, which
            // agrees with the lead entry, so it is a *palette* case --
            // pinned by `paint_composer_opens_a_palette_for_a_single_letter_prefix`.
            // `/context` replaces it: a whole command name, still one row.
            ("/context", 8, "", false),
            ("hello /co", 9, "", false),
        ] {
            let (guard, _out, err) = install_inline(24, 1, "status: idle");
            let guard = guard.expect("inline mode installs a guard");
            guard.paint_draft("kitsu> ", draft, cursor, hint, reserved);
            let expected = err.text();
            let before = expected.len();

            guard.paint_composer("kitsu> ", draft, cursor, hint, reserved, true);

            let tail = err.text()[before..].to_string();
            assert_eq!(
                tail,
                expected_status_paint(
                    24,
                    1,
                    &live::compose_input_row(
                        &live::LiveDraft {
                            prompt: "kitsu> ".to_string(),
                            buffer: draft.to_string(),
                            cursor,
                            hint: hint.to_string(),
                            hint_reserved: reserved,
                        },
                        live::terminal_columns()
                    )
                    .0
                ),
                "a draft with no palette must paint exactly today's one row"
            );
            drop(guard);
        }
    }

    /// Requirement N5, the wiring. Nothing in production installs any
    /// restore for a signal today: the only `sigaction` this binary performs
    /// is `SIGWINCH`'s, so `SIGINT` and `SIGTERM` are still at `SIG_DFL` and
    /// the process dies without releasing the viewport.
    ///
    /// Asserted by **querying the disposition**, which is what makes this
    /// non-vacuous without raising anything: `sigaction` with a null `act`
    /// reports what is installed, and `SIG_DFL` is exactly the state that
    /// kills the process before any restore can run. The test raises no
    /// signal (that would terminate the test binary under the default
    /// disposition it is checking for) and asserts no particular exit code,
    /// which is the PTY tests' business.
    ///
    /// The panic half is **not** re-asserted here: chaining is pinned by
    /// `kitsu_tui`'s own `install_panic_restore_chains_the_previous_hook`,
    /// and the panic hook is process-global, so a second test that replaced
    /// it would race the other 800-odd tests in this binary. What this test
    /// does owe the reader is the requirement: `install_exit_restore` must
    /// reach the panic path through that helper, not through a hook of its
    /// own that drops `main`'s broken-pipe guard.
    /// `SIGNAL_RESTORE` must be byte-identical to what
    /// `kitsu_tui::write_restore_sequences` writes.
    ///
    /// The duplication is forced, not careless: a signal handler cannot write
    /// through `io::Write` (it locks and may allocate), so the handler needs a
    /// precomposed `const`. But two copies of one byte sequence with nothing
    /// comparing them is this project's most-repeated defect shape -- F2's
    /// suggester table, F38's four unchecked literals, F45's two vocabularies,
    /// the `/agent` args string that was stale in three files at once. This
    /// test is the comparison, so a sequence added to the helper and forgotten
    /// here fails immediately instead of leaving a signal exit that restores
    /// less than a panic does.
    #[test]
    // **F107**: not optional. `#[serial]` excludes other `#[serial]` tests, not
    // plain ones, and this test reaches the process-global surface slot through
    // PRODUCTION code rather than `SurfaceProbe`, so it can install over a
    // serialised test's probe mid-run. Measured: 3 failures in 421 unguarded
    // runs of `--bins`, 0 in 400 with this guard.
    #[serial_test::serial]
    fn the_handler_byte_string_equals_the_restore_helper_output() {
        let mut helper = Vec::new();
        kitsu_tui::write_restore_sequences(&mut helper).expect("writing into a Vec cannot fail");
        assert_eq!(
            SIGNAL_RESTORE,
            helper.as_slice(),
            "the handler's precomposed bytes and the restore helper have \
             drifted: the handler writes {:?} but the helper writes {:?}",
            String::from_utf8_lossy(SIGNAL_RESTORE),
            String::from_utf8_lossy(&helper),
        );
    }

    #[test]
    fn install_exit_restore_takes_sigint_and_sigterm_off_the_default_disposition() {
        fn handler_of(signal: libc::c_int) -> libc::sighandler_t {
            // SAFETY: a query only -- null `act`, valid out-pointer.
            unsafe {
                let mut current: libc::sigaction = std::mem::zeroed();
                assert_eq!(
                    libc::sigaction(signal, std::ptr::null(), &mut current),
                    0,
                    "query the disposition of signal {signal}"
                );
                current.sa_sigaction
            }
        }

        super::install_exit_restore();

        for (signal, name) in [(libc::SIGINT, "SIGINT"), (libc::SIGTERM, "SIGTERM")] {
            let installed = handler_of(signal);
            assert_ne!(
                installed,
                libc::SIG_DFL,
                "{name} must not be left at SIG_DFL: the default disposition \
                 terminates the process before anything can release the \
                 viewport's scroll region or restore termios"
            );
            assert_ne!(
                installed,
                libc::SIG_IGN,
                "{name} must not be ignored either -- a signal that does \
                 nothing is not a restore"
            );
        }
    }

    // ---------------------------------------------------------------------
    // Card **T3-S12**: the status row's frame budget and `--reduced-motion`
    // (notes **N3** and **N9**).
    //
    // **FAILING-FIRST.** Both measurements below were taken on this tree
    // before a line of them was written, with a throwaway probe in this very
    // module, and both are reported here as numbers rather than as beliefs
    // (F43/F47):
    //
    // * **Reduced motion is ignored by the painter.** With
    //   `configure_terminal_modes(false, true, false)` in force, a turn
    //   running and *nothing changed but the clock*, `repaint_status_row`
    //   emitted a full row paint one second later:
    //   `ESC[24;1H ESC[2K example-model-v1 · ■□□□□□□□ 1% · 2s ESC[23;1H`.
    //   That is N9's "periodic age refresh", still refreshing. The code path
    //   is `repaint_status_row` -> `StatusSource::compose` ->
    //   `StatusSource::fields`, whose `elapsed` is
    //   `turn_start.map(Instant::elapsed)`; nothing between the flag and the
    //   row consults `reduced_motion`, and the dedupe in `repaint_status_row`
    //   compares *composed rows*, so a row whose only changed segment is
    //   elapsed is a changed row.
    //
    // * **The 60 fps budget is a property of the caller, not of the
    //   painter.** `run_live_typeahead` calls the painter on a 50 ms
    //   `libc::poll` tick (20 fps), and T3-S5b's dedupe suppresses an
    //   unchanged row, so in production the budget holds. It does **not**
    //   hold of `repaint_status_row` itself: a loop that changed one
    //   non-elapsed field (the queue depth) on every call produced **54003
    //   paints in 209 ms**, about 258 000 fps, because the dedupe only ever
    //   catches a byte-identical row. N3 asks for coalescing, and coalescing
    //   that lives in the caller's sleep is coalescing only for as long as
    //   nobody adds a second caller -- which this epic has already done once
    //   (the approval modal, T3-S7a, paints the same row through
    //   `clear_approval_modal`).
    //
    // What these tests deliberately do **not** pin:
    //
    // * **Synchronized output (`CSI ?2026 h/l`) is deferred**, and
    //   [`synchronized_output_is_not_emitted_until_a_capability_probe_exists`]
    //   pins the deferral rather than the feature. Reason: the only in-band
    //   way to learn whether a terminal implements mode 2026 is DECRQM
    //   (`CSI ?2026$p`) and then *reading a reply* on stdin -- and stdin here
    //   is owned by `run_live_typeahead`'s raw-mode poll loop, which is also
    //   serving approval reads (T3-S7b). A probe that waits for a report it
    //   may never get is a hang; a probe with a timeout is a sleep on the
    //   first frame; and emitting the mode unconditionally leaves `?2026h` as
    //   visible garbage on a terminal that does not know it. Until the
    //   reader can be asked for a DECRPM reply, the honest state is: do not
    //   emit it. The row is one line, so tearing is not a live defect.
    // * The agent and queue tags under reduced motion. They are **state**,
    //   not animation: `[2 blocked]` appearing is an event the user must act
    //   on, and N9 names only the spinner, counter flashes and the periodic
    //   age refresh. Suppressing them would hide a blocked agent, so the
    //   reduced-motion test's positive control is precisely that a queue-depth
    //   change still repaints.
    // ---------------------------------------------------------------------

    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::{repaint_status_row, StatusCounts, StatusSource, STATUS_FRAME_INTERVAL};

    /// Process-wide reduced-motion override, restored on the way out.
    ///
    /// `kitsu_render` exposes a setter and no getter, and this binary's only
    /// other caller of `configure_terminal_modes` is `main` (which never runs
    /// under `cargo test`), so the value to restore is the documented default:
    /// every flag off. Tests that use this are `#[serial]` for the same reason
    /// the surface slot's tests are.
    struct ReducedMotion;

    impl ReducedMotion {
        fn on() -> Self {
            kitsu_render::configure_terminal_modes(false, true, false);
            Self
        }

        fn off() {
            kitsu_render::configure_terminal_modes(false, false, false);
        }
    }

    impl Drop for ReducedMotion {
        fn drop(&mut self) {
            Self::off();
        }
    }

    /// The viewport geometry these tests install over: a 24-row terminal with
    /// one pinned row, so the row is 24 and the park row is 23.
    const T3_S12_ROWS: u16 = 24;
    const T3_S12_VIEWPORT_ROWS: u16 = 1;

    /// The prefix `InlineSurface::paint` writes once per painted row --
    /// absolute position then erase-line -- derived from
    /// [`expected_status_paint`] rather than retyped, so counting these counts
    /// frames.
    fn paint_marker() -> String {
        let painted = expected_status_paint(T3_S12_ROWS, T3_S12_VIEWPORT_ROWS, "");
        let park = format!("\x1b[{};1H", T3_S12_ROWS - T3_S12_VIEWPORT_ROWS);
        painted
            .strip_suffix(&park)
            .expect("a one-row paint ends by parking the cursor")
            .to_string()
    }

    /// How many row paints `text` contains.
    fn row_paints(text: &str) -> usize {
        text.matches(&paint_marker()).count()
    }

    /// A live status row installed over a 24x1 inline viewport, with a
    /// turn running and a queue depth the test can move.
    ///
    /// Returns the guard, the stderr sink, the source and the queue-depth
    /// cell. The usage numbers are the PTY suite's own fixture model and a
    /// realistic window, so the composed row is a real sample.
    fn live_status_fixture() -> (BridgeGuard, SharedSink, Arc<StatusSource>, Arc<AtomicUsize>) {
        let (guard, _out, err) = install_inline(T3_S12_ROWS, T3_S12_VIEWPORT_ROWS, "status: idle");
        let guard = guard.expect("inline mode installs a guard");
        let queued = Arc::new(AtomicUsize::new(0));
        let cell = queued.clone();
        let source = StatusSource::new(Box::new(move || StatusCounts {
            queued: cell.load(Ordering::Relaxed),
            ..Default::default()
        }));
        source.set_usage("example-model-v1", 1_000, 100_000);
        guard.enable_live_status(source.clone());
        source.begin_turn();
        (guard, err, source, queued)
    }

    /// Note **N9**, both halves, in one test because the second is what keeps
    /// the first from being satisfied by an over-broad fix.
    ///
    /// 1. **Under `--reduced-motion` the row does not repaint merely because
    ///    time passed.** A second and a bit elapses with every field but the
    ///    clock untouched, and the painter writes **nothing**.
    /// 2. **It still repaints on a real state change.** The queue depth moves
    ///    from 0 to 3 and the row is painted, carrying `[3 queued]`. A fix
    ///    that froze the row outright would hide a blocked agent, which is the
    ///    opposite of what N9 asks for.
    ///
    /// Half 1 is a negative assertion, so the test carries its own control
    /// (the eight-finding family): with reduced motion **off**, the same
    /// sleep and the same pair of calls *do* produce a paint, which proves the
    /// sleep really crosses a one-second boundary and the row really would
    /// have changed. Without that control, "nothing was emitted" could mean
    /// nothing was ever wired.
    #[test]
    #[serial]
    fn reduced_motion_suppresses_the_elapsed_tick_but_not_a_state_change() {
        let _state = GlobalState::capture();
        let _motion = ReducedMotion::on();
        let (guard, err, _source, queued) = live_status_fixture();

        // Establish a painted row, then let only the clock move.
        repaint_status_row();
        let before = err.text().len();
        std::thread::sleep(std::time::Duration::from_millis(1_100));
        repaint_status_row();
        let window = err.text()[before..].to_string();
        assert_eq!(
            window,
            "",
            "under --reduced-motion the row must not repaint because a second \
             passed (N9's periodic age refresh), got {:?}",
            window.escape_debug().to_string()
        );

        // Half 2: a real state change still reaches the screen.
        let before = err.text().len();
        queued.store(3, Ordering::Relaxed);
        repaint_status_row();
        let window = err.text()[before..].to_string();
        assert_eq!(
            row_paints(&window),
            1,
            "a queue-depth change is state, not animation, and must still \
             paint exactly once, got {:?}",
            window.escape_debug().to_string()
        );
        let visible = kitsu_render::sanitize_terminal_text(&window);
        assert!(
            visible.contains("[3 queued]"),
            "and the painted row must carry the new depth, got {visible:?}"
        );
        assert!(
            visible.contains("example-model-v1"),
            "and still be the status row, got {visible:?}"
        );

        // The control: the same wait, the same two calls, reduced motion off.
        ReducedMotion::off();
        repaint_status_row();
        let before = err.text().len();
        std::thread::sleep(std::time::Duration::from_millis(1_100));
        repaint_status_row();
        let window = err.text()[before..].to_string();
        assert_eq!(
            row_paints(&window),
            1,
            "control: with motion allowed the elapsed tick must paint, so the \
             assertion above is about the flag and not about dead wiring, got \
             {:?}",
            window.escape_debug().to_string()
        );
        drop(guard);
    }

    /// Note **N3**'s frame budget, measured at the painter rather than at its
    /// caller: **no more than 60 row paints per second**, however often
    /// `repaint_status_row` is called and however often the fields move.
    ///
    /// The budget is computed from the window this loop actually occupied, not
    /// from a magic number: 60 fps allows `ceil(seconds * 60)` frames, plus
    /// one for the frame at the window's start, which is owed immediately
    /// (a rate limiter that made the *first* frame wait would show a stale row
    /// for 16 ms on every keystroke).
    ///
    /// Three positive controls, so neither half can pass vacuously:
    ///
    /// * the loop really churned (thousands of calls, every one of them with
    ///   a different queue depth than the last);
    /// * at least one frame got through, so the budget is not satisfied by
    ///   painting nothing;
    /// * **the last value is not lost.** Coalescing may drop intermediate
    ///   frames; it may not leave the final state unpainted. After the churn
    ///   the depth is set once more and the painter is given a tick's worth of
    ///   wall clock and a further call, after which the screen must carry it.
    #[test]
    #[serial]
    fn status_row_paints_stay_inside_the_sixty_frame_budget() {
        let _state = GlobalState::capture();
        ReducedMotion::off();
        let (guard, err, _source, queued) = live_status_fixture();

        let before = err.text().len();
        let start = std::time::Instant::now();
        let mut calls = 0usize;
        while start.elapsed() < std::time::Duration::from_millis(250) {
            // Alternate between two depths: each call composes a row that
            // differs from the one before, so T3-S5b's byte-identity dedupe
            // can never be what holds the rate down.
            queued.store(calls % 2, Ordering::Relaxed);
            repaint_status_row();
            calls += 1;
        }
        let window_secs = start.elapsed().as_secs_f64();
        let window = err.text()[before..].to_string();
        let painted = row_paints(&window);
        let allowed = (window_secs * 60.0).ceil() as usize + 1;

        assert!(
            calls > 1_000,
            "control: the loop must really churn, got {calls} calls in \
             {window_secs:.3}s"
        );
        assert!(
            painted >= 1,
            "control: the budget must be met by coalescing, not by painting \
             nothing -- {calls} calls produced no frame at all"
        );
        assert!(
            painted <= allowed,
            "N3: {painted} row paints in {window_secs:.3}s exceeds 60 fps \
             ({allowed} allowed); {calls} calls"
        );

        // And the last state is not lost to the coalescing.
        queued.store(7, Ordering::Relaxed);
        repaint_status_row();
        std::thread::sleep(std::time::Duration::from_millis(40));
        repaint_status_row();
        let visible = kitsu_render::sanitize_terminal_text(&err.text());
        assert!(
            visible.contains("[7 queued]"),
            "coalescing may drop intermediate frames but must not leave the \
             final state unpainted; screen bytes: {:?}",
            err.text().escape_debug().to_string()
        );
        drop(guard);
    }

    /// Note **N3**'s synchronized-output clause, **deferred on purpose**: no
    /// paint may emit `CSI ?2026 h` or `l` until something can establish that
    /// the terminal implements the mode.
    ///
    /// This is not a style ban (F41) -- it is the byte-level contract of the
    /// deferral argued at the top of this block: mode 2026 can only be
    /// detected by a DECRQM report read from stdin, stdin belongs to the
    /// type-ahead loop, and a terminal that does not implement the mode shows
    /// the sequence instead of obeying it. Emitting it blind is the one
    /// outcome worse than not having it. When a probe exists, the test to
    /// write is "the mode is emitted *after* a positive DECRPM report and
    /// never without one", and this test is what that one replaces.
    ///
    /// Positive controls: the same stream must contain the viewport's region
    /// and at least two row paints, so the absence is an absence in a stream
    /// that was really written.
    #[test]
    #[serial]
    fn synchronized_output_is_not_emitted_until_a_capability_probe_exists() {
        let _state = GlobalState::capture();
        ReducedMotion::off();
        let (guard, err, _source, queued) = live_status_fixture();

        repaint_status_row();
        queued.store(2, Ordering::Relaxed);
        repaint_status_row();
        let text = err.text();

        assert!(
            row_paints(&text) >= 2,
            "control: the session must really have painted rows, got {:?}",
            text.escape_debug().to_string()
        );
        assert!(
            text.contains(&expected_show(T3_S12_ROWS, T3_S12_VIEWPORT_ROWS)),
            "control: and really have pinned the viewport, got {:?}",
            text.escape_debug().to_string()
        );
        for mode in ["\x1b[?2026h", "\x1b[?2026l"] {
            assert_eq!(
                text.matches(mode).count(),
                0,
                "synchronized output is deferred until a capability probe \
                 exists: {:?} must not be emitted; stream {:?}",
                mode.escape_debug().to_string(),
                text.escape_debug().to_string()
            );
        }
        drop(guard);
    }

    // ---------------------------------------------------------------------
    // Card **T4-S6-status**, requirement A5 / design §4.5: the
    // `attached · agent N` segment, measured in the bytes the painter writes.
    //
    // **FAILING-FIRST.** `StatusFields::attached` and `StatusCounts::attached`
    // exist (this card's authorised production edits) and nothing composes or
    // forwards them, so both tests below are red.
    //
    // The surface is `repaint_status_row` -> `InlineSurface::paint` -> stderr:
    // the same path the 50 ms cadence paints the pinned row on, so what these
    // tests read is what reaches the terminal. Finding **F71**: reading the
    // attachment back off `StatusSource::fields` would pass against a row
    // that never mentions it.
    // ---------------------------------------------------------------------

    /// The fixture of [`live_status_fixture`] plus the **session binding** the
    /// composer loop owns, behind the handle the counts closure reads.
    ///
    /// The closure is a *pull*: the attachment is read at paint time from the
    /// one cell, never copied into the source, which is why a detach needs no
    /// second notification and the row cannot drift from the prompt label
    /// (**F75**).
    #[allow(clippy::type_complexity)]
    fn live_status_fixture_attached() -> (
        BridgeGuard,
        SharedSink,
        Arc<StatusSource>,
        Arc<AtomicUsize>,
        Arc<std::sync::Mutex<Option<u64>>>,
    ) {
        let (guard, _out, err) = install_inline(T3_S12_ROWS, T3_S12_VIEWPORT_ROWS, "status: idle");
        let guard = guard.expect("inline mode installs a guard");
        let queued = Arc::new(AtomicUsize::new(0));
        let attached: Arc<std::sync::Mutex<Option<u64>>> = Arc::new(std::sync::Mutex::new(None));
        let (queue_cell, attach_cell) = (queued.clone(), attached.clone());
        let source = StatusSource::new(Box::new(move || StatusCounts {
            queued: queue_cell.load(Ordering::Relaxed),
            attached: *attach_cell.lock().expect("the session binding"),
            ..Default::default()
        }));
        source.set_usage("example-model-v1", 1_000, 100_000);
        guard.enable_live_status(source.clone());
        source.begin_turn();
        (guard, err, source, queued, attached)
    }

    /// Past the painter's frame budget (N3), so the next `repaint_status_row`
    /// is not swallowed by it.
    fn past_the_frame_budget() {
        std::thread::sleep(STATUS_FRAME_INTERVAL * 3);
    }

    /// The segment is **painted**, it **names the agent**, and it **tracks
    /// the binding in both directions** — attach paints it, detach takes it
    /// away. Nothing between the binding and the bytes but production.
    ///
    /// The detach half is deliberately driven by clearing the binding and
    /// nothing else, because that is the whole of what the composer loop's
    /// detach arm does (`main.rs`, `InlineRead::Detached`: `*attached = None`)
    /// — so if the row needed telling separately, it would be a second copy
    /// and this test would stay red.
    #[test]
    #[serial]
    fn the_status_row_paints_the_attachment_and_stops_when_the_binding_is_cleared() {
        let _state = GlobalState::capture();
        ReducedMotion::off();
        let (guard, err, _source, _queued, attached) = live_status_fixture_attached();

        // Detached: the row is painted, and it is the whole row.
        repaint_status_row();
        let visible = kitsu_render::sanitize_terminal_text(&err.text());
        assert!(
            visible.contains("example-model-v1"),
            "control: the detached row must be painted, got {visible:?}"
        );
        assert!(
            !visible.contains("attached"),
            "nothing is attached, so the row must not say so, got {visible:?}"
        );

        // Attach: the binding moves and nothing else is told.
        *attached.lock().expect("the session binding") = Some(3);
        let before = err.text().len();
        past_the_frame_budget();
        repaint_status_row();
        let window = err.text()[before..].to_string();
        let visible = kitsu_render::sanitize_terminal_text(&window);
        assert_eq!(
            row_paints(&window),
            1,
            "attaching is state, not animation, and must paint once, got {:?}",
            window.escape_debug().to_string()
        );
        assert!(
            visible.contains("attached \u{b7} agent 3"),
            "design §4.5: the status row shows `attached \u{b7} agent 3`, got \
             {visible:?}"
        );
        assert!(
            visible.contains("example-model-v1"),
            "and it is still the status row, got {visible:?}"
        );

        // A second agent: the row names *which*, so the segment cannot be a
        // fixed "attached" with the id painted from somewhere else.
        *attached.lock().expect("the session binding") = Some(11);
        let before = err.text().len();
        past_the_frame_budget();
        repaint_status_row();
        let visible = kitsu_render::sanitize_terminal_text(&err.text()[before..]);
        assert!(
            visible.contains("attached \u{b7} agent 11") && !visible.contains("agent 3"),
            "the segment names the agent the binding holds, got {visible:?}"
        );

        // Detach: clearing the binding is the whole of the composer loop's
        // detach arm, and the segment must go with it.
        *attached.lock().expect("the session binding") = None;
        let before = err.text().len();
        past_the_frame_budget();
        repaint_status_row();
        let window = err.text()[before..].to_string();
        let visible = kitsu_render::sanitize_terminal_text(&window);
        assert_eq!(
            row_paints(&window),
            1,
            "detaching must repaint the row, got {:?}",
            window.escape_debug().to_string()
        );
        assert!(
            !visible.contains("attached"),
            "A5: after a detach the row must not claim an attachment, got \
             {visible:?}"
        );
        assert!(
            visible.contains("example-model-v1"),
            "and the row that comes back is the whole row, not an empty one, \
             got {visible:?}"
        );
        drop(guard);
    }

    /// **Decided here, because §4.5 does not say: the segment survives a
    /// repaint, and survives the attached agent finishing.**
    ///
    /// *Repaint.* The row is recomposed from the source on every paint
    /// (`repaint_status_row`, `clear_approval_modal`), never remembered, so a
    /// repaint triggered by something else entirely — here a queue-depth
    /// change — must still carry the attachment. A segment that was painted
    /// once and lost on the next frame would make §4.5's legibility argument
    /// for `←` hold only until the next keystroke.
    ///
    /// *The agent finishing.* The attachment is the **user's**, not the
    /// agent's: it is derived from the composer's session binding and the
    /// registry is not consulted, so an agent reaching a terminal state while
    /// attached does **not** silently detach. The precedent is T4-S6a's own
    /// ruling that leaving the agent view is not detaching — only `←`,
    /// `Ctrl+X d` and `/detach` are — and the alternative is worse: the
    /// prompt label is derived from the same binding, so a row that dropped
    /// the segment on its own would disagree with the caret in front of the
    /// user. This test pins it at the only place that can be pinned here: the
    /// row's content depends on the binding and on nothing about the agent,
    /// so no registry state can take the segment away. The registry's side of
    /// it belongs to T4-S6b/c, which own the stream and the recap.
    #[test]
    #[serial]
    fn the_attachment_segment_survives_a_repaint_caused_by_anything_else() {
        let _state = GlobalState::capture();
        ReducedMotion::off();
        let (guard, err, source, queued, attached) = live_status_fixture_attached();

        *attached.lock().expect("the session binding") = Some(4);
        repaint_status_row();
        let visible = kitsu_render::sanitize_terminal_text(&err.text());
        assert!(
            visible.contains("attached \u{b7} agent 4"),
            "control: the attachment must reach the first paint, got {visible:?}"
        );

        // A repaint caused by an unrelated state change, with the binding
        // untouched.
        queued.store(2, Ordering::Relaxed);
        let before = err.text().len();
        past_the_frame_budget();
        repaint_status_row();
        let window = err.text()[before..].to_string();
        let visible = kitsu_render::sanitize_terminal_text(&window);
        assert_eq!(
            row_paints(&window),
            1,
            "control: the queue depth moved, so the row must repaint, got {:?}",
            window.escape_debug().to_string()
        );
        assert!(
            visible.contains("[2 queued]"),
            "control: carrying the new depth, got {visible:?}"
        );
        assert!(
            visible.contains("attached \u{b7} agent 4"),
            "and the attachment survives a repaint it did not cause, got \
             {visible:?}"
        );

        // And the composed row is a function of the binding alone: the same
        // binding composes the same segment however many times it is asked,
        // which is what "recomposed, never remembered" means.
        let cols = live::terminal_columns();
        let once = kitsu_render::sanitize_terminal_text(&source.compose(cols));
        let twice = kitsu_render::sanitize_terminal_text(&source.compose(cols));
        assert_eq!(once, twice, "the row is composed, not consumed");
        assert!(
            once.contains("attached \u{b7} agent 4"),
            "and composing it again still names the agent, got {once:?}"
        );
        drop(guard);
    }

    // ---- T4-S1: the agent view's pure model, across the crate boundary ----

    /// A valid thread DTO, built from `thread.rs`'s own `blocked_snapshot`
    /// fixture so its `retained_bytes` accounting is real rather than guessed;
    /// `validate_default` is asserted below, which proves it.
    fn thread_dto(
        phase: thread::AgentThreadPhase,
        handoff: thread::AgentHandoffState,
    ) -> thread::AgentThreadSnapshotDto {
        let terminal = !matches!(
            phase,
            thread::AgentThreadPhase::Running | thread::AgentThreadPhase::Blocked
        );
        thread::AgentThreadSnapshotDto::from_snapshot(&thread::AgentThreadSnapshot {
            id: 7,
            task: "summarize the crate".to_string(),
            model: "qwen2.5-coder".to_string(),
            profile: Some("reviewer".to_string()),
            name: None,
            phase,
            waiting_for: None,
            handoff,
            elapsed_ms: 12_500,
            result_bytes: None,
            // Derived from the phase, not fixed: `validate` requires
            // `terminal == timeline.closed` and forbids open steering once
            // terminal, so a fixture with these hard-coded is only a valid DTO
            // for non-terminal phases. Corrected at review in T4-S1.
            steering_open: !terminal,
            pending_steering: 0,
            timeline: thread::AgentTimelineSnapshot {
                entries: vec![thread::AgentTimelineEntry {
                    sequence: 1,
                    elapsed_ms: 10,
                    kind: thread::AgentTimelineKind::UserTask,
                    round: None,
                    text: "summarize the crate".to_string(),
                    truncated: false,
                }],
                dropped_entries: 0,
                retained_bytes: 19,
                closed: terminal,
            },
        })
    }

    /// Every `(phase, handoff)` pair maps onto exactly one A2 group, and the
    /// viewed/unviewed split is driven by the handoff and nothing else.
    #[test]
    fn the_thread_dto_maps_every_phase_and_handoff_pair_to_one_agent_view_state() {
        use kitsu_render::AgentViewState;
        use thread::AgentHandoffState::{Acknowledged, Ready, Unavailable};
        use thread::AgentThreadPhase::{Blocked, Cancelled, Done, Failed, Running};

        let expected: [(
            thread::AgentThreadPhase,
            thread::AgentHandoffState,
            AgentViewState,
        ); 15] = [
            (Blocked, Unavailable, AgentViewState::NeedsInput),
            (Blocked, Ready, AgentViewState::NeedsInput),
            (Blocked, Acknowledged, AgentViewState::NeedsInput),
            (Running, Unavailable, AgentViewState::Working),
            (Running, Ready, AgentViewState::Working),
            (Running, Acknowledged, AgentViewState::Working),
            // The split A2 asks for: `Ready` means nobody has looked yet.
            (Done, Ready, AgentViewState::DoneUnviewed),
            (Done, Acknowledged, AgentViewState::DoneViewed),
            // Nothing to observe at all is not an unviewed result.
            (Done, Unavailable, AgentViewState::DoneViewed),
            (Failed, Unavailable, AgentViewState::Failed),
            (Failed, Ready, AgentViewState::Failed),
            (Failed, Acknowledged, AgentViewState::Failed),
            (Cancelled, Unavailable, AgentViewState::Cancelled),
            (Cancelled, Ready, AgentViewState::Cancelled),
            (Cancelled, Acknowledged, AgentViewState::Cancelled),
        ];

        let mut checked_dtos = 0_usize;
        for (phase, handoff, state) in expected {
            assert_eq!(
                super::agent_view_state_of(phase, handoff),
                state,
                "phase {} + handoff {}",
                phase.label(),
                handoff.label()
            );
            // The classifier is asserted over all fifteen pairs above, because
            // it is a total function over two enums and defensive totality is
            // correct there. The DTO, however, *forbids* seven of them:
            // `validate` requires `terminal == timeline.closed`, a handoff iff
            // terminal, and no open steering once terminal. So a non-terminal
            // phase with a handoff, and a terminal phase with none, are states
            // the DTO declares unrepresentable -- and an existing test,
            // `thread::tests::validate_treats_blocked_as_non_terminal`, pins
            // exactly that ("a blocked thread has no handoff").
            //
            // Asserting a fixture is valid for those seven would require
            // loosening an invariant another test holds. The row mapping is
            // therefore exercised only over the eight representable pairs,
            // and the count is asserted below so this guard cannot silently
            // skip everything. Corrected at review in T4-S1; the implementer
            // stopped here rather than edit a test or an invariant.
            let terminal = !matches!(phase, Blocked | Running);
            let representable = terminal != (handoff == Unavailable);
            if !representable {
                continue;
            }
            checked_dtos += 1;
            let dto = thread_dto(phase, handoff);
            dto.validate_default()
                .expect("the fixture must be a valid DTO");
            assert_eq!(
                super::agent_row_from_thread_dto(&dto).state,
                state,
                "the row's state must agree with the classifier"
            );
        }

        assert_eq!(
            checked_dtos, 8,
            "exactly eight of the fifteen pairs are representable DTOs; a guard \
             that skipped more would stop exercising the row mapping"
        );

        // Non-vacuous: all six A2 groups are actually produced by the table.
        let produced = expected
            .iter()
            .map(|(_, _, state)| *state)
            .collect::<std::collections::HashSet<AgentViewState>>();
        assert_eq!(produced.len(), 6, "every A2 group must be reachable");

        // And reachable through a *valid* DTO, not merely through the
        // classifier: the eight representable pairs must still cover all six.
        let through_dtos = expected
            .iter()
            .filter(|(phase, handoff, _)| {
                let terminal = !matches!(phase, Blocked | Running);
                terminal != (*handoff == Unavailable)
            })
            .map(|(_, _, state)| *state)
            .collect::<std::collections::HashSet<AgentViewState>>();
        assert_eq!(
            through_dtos.len(),
            6,
            "every A2 group must be reachable through a DTO the validator accepts"
        );
    }

    /// The DTO → row mapping: what the DTO has travels, and what it cannot
    /// supply stays absent.
    #[test]
    fn an_agent_row_from_a_thread_dto_carries_its_fields_and_leaves_telemetry_absent() {
        let dto = thread_dto(
            thread::AgentThreadPhase::Running,
            thread::AgentHandoffState::Unavailable,
        );
        dto.validate_default().expect("valid fixture");
        let row = super::agent_row_from_thread_dto(&dto);
        assert_eq!(row.id, 7);
        assert_eq!(row.task, "summarize the crate");
        assert_eq!(row.model, "qwen2.5-coder");
        assert_eq!(row.profile.as_deref(), Some("reviewer"));
        assert_eq!(row.name, None);
        assert_eq!(
            row.head(),
            "summarize the crate",
            "with no name the head is the task"
        );
        // `AgentThreadSnapshot` has no token counts and no activity label, so
        // the mapping must leave them absent rather than report a zero.
        assert_eq!(row.tokens, None, "the DTO cannot supply token counts");
        assert_eq!(row.last_activity, None, "the DTO cannot supply an activity");
        assert_eq!(row.last_activity_age, None);

        // A named agent's name reaches the row and wins the head.
        let mut named = dto.thread.clone();
        named.name = Some("crate-reviewer".to_string());
        let row = super::agent_row_from_thread_dto(&thread::AgentThreadSnapshotDto::from_snapshot(
            &named,
        ));
        assert_eq!(row.name.as_deref(), Some("crate-reviewer"));
        assert_eq!(row.head(), "crate-reviewer");
    }

    /// **Anti-fork (F2/F38/F45/F57).** `/jobs view` and the agent view print
    /// different words — A2 says "done (unviewed)", the text deck says
    /// "done, result ready" — but they must not disagree about *which* groups
    /// exist or *what order* they come in. The headings below are the literal
    /// `GROUPS` array in `format_jobs_view`; this asserts each one's position
    /// equals the shared `AgentViewState::rank`.
    #[test]
    fn the_jobs_view_group_order_cannot_fork_from_the_shared_agent_view_order() {
        use crate::subagent::{AgentProgress, AgentProgressStatus, AgentTelemetry};
        use kitsu_render::AgentViewState;

        // Read from `format_jobs_view`, not invented.
        const JOBS_VIEW_GROUPS: [&str; 6] = [
            "needs input",
            "working",
            "done, result ready",
            "done, collected",
            "failed",
            "cancelled",
        ];
        assert_eq!(
            JOBS_VIEW_GROUPS.len(),
            AgentViewState::order().len(),
            "the two surfaces must have the same number of groups"
        );

        let progress = |status: AgentProgressStatus| AgentProgress {
            id: 1,
            task: "summarize the crate".to_string(),
            profile: None,
            status,
            waiting_for: None,
            elapsed_ms: 0,
            result_bytes: None,
            last_activity_ms_ago: None,
            telemetry: AgentTelemetry::default(),
        };

        // Every case `jobs_view_group` can produce, and the A2 group it is.
        let cases: [(AgentProgressStatus, bool, AgentViewState); 6] = [
            (
                AgentProgressStatus::Blocked,
                true,
                AgentViewState::NeedsInput,
            ),
            (AgentProgressStatus::Running, true, AgentViewState::Working),
            (
                AgentProgressStatus::Done,
                false,
                AgentViewState::DoneUnviewed,
            ),
            (AgentProgressStatus::Done, true, AgentViewState::DoneViewed),
            (AgentProgressStatus::Failed, true, AgentViewState::Failed),
            (
                AgentProgressStatus::Cancelled,
                true,
                AgentViewState::Cancelled,
            ),
        ];

        let mut seen = std::collections::HashSet::new();
        for (status, collected, state) in cases {
            let heading = crate::jobs_view_group(&progress(status), collected);
            let position = JOBS_VIEW_GROUPS
                .iter()
                .position(|group| *group == heading)
                .unwrap_or_else(|| panic!("`{heading}` is not one of the known /jobs view groups"));
            assert_eq!(
                state.rank(),
                position,
                "`{heading}` sits at {position} in /jobs view but ranks {} in the agent view",
                state.rank()
            );
            assert!(seen.insert(heading), "`{heading}` was produced twice");
        }
        assert_eq!(
            seen.len(),
            JOBS_VIEW_GROUPS.len(),
            "the table must cover every /jobs view group, or a reorder could hide here"
        );
    }

    // ------------------------------------------------------------------
    // Card T4-S6a: **attach/detach -- what attachment is, and the keys
    // that enter and leave it.** Requirement A5, design §4.5 and §4.7.
    //
    // Every test under this banner FAILS today: `KeyFilter` does not know
    // the detach letter, the reader does not know what `attached` means,
    // and `composer_prompt` returns the base prompt whatever it is given.
    //
    // **The `←` rule this card settles.** T4-S2 bound `←` on an empty
    // composer to the agent view, two presses, and decided that any other
    // keystroke disarms the chord. A5 now wants `←` to *detach*. The rule
    // pinned here:
    //
    // * **While attached, the first `←` on an empty composer detaches**, and
    //   never reaches the two-press count. Two reasons, and the first is the
    //   one T4-S2 made the two-press rule for. T4-S2's objection to a durable
    //   armed state was that it makes a keystroke's meaning depend on
    //   something the user did minutes ago *and can no longer see*;
    //   attachment is the opposite case -- it is on screen in front of the
    //   caret for as long as it lasts (`composer_prompt`, pinned below, and
    //   the status row's `attached · agent N`), so the context that decides
    //   this keystroke is legible at the moment it is pressed. Second, the
    //   two costs are not comparable: opening a full-screen overlay by
    //   accident interrupts you, while detaching by accident costs one
    //   keystroke to undo and **stops nothing** (§4.5: stopping is `x` or
    //   `/jobs stop`, never a detach).
    // * A **non-empty** draft keeps `←` as cursor movement while attached,
    //   exactly as detached. A draft is content; this is unchanged.
    // * The rule does **not** consult `[tui] left_arrow_opens_agents`. A1
    //   says that key "disables that route alone", and the route it names is
    //   the agent view's; A5 names `←` as the return path without
    //   conditioning it. With the key off and nothing attached, `←` on an
    //   empty composer stays plain backward-char -- a no-op at column 0.
    //
    // **`Ctrl+C` is deliberately absent.** §4.5 asks for `Ctrl+C` =
    // clear draft / detach, and finding **F54** records that "`Ctrl+C` is
    // swallowed at an approval prompt" is measured but *unexplained* -- the
    // cause is probably the harness rather than the code. The clear-draft
    // half already ships in `kitsu-editor` (`0x03` clears a non-empty
    // buffer, `lib.rs:529`) and is not this card's to re-pin; the detach
    // half would bind the one byte whose signal behaviour is unresolved, so
    // it waits for a human with a real terminal.
    // ------------------------------------------------------------------

    /// The default `[tui] prefix` as the byte a terminal sends for it.
    const DETACH_PREFIX: u8 = 0x18;

    /// Both routes live, as a shipped default configuration has them.
    fn attach_keys() -> InlineKeys {
        InlineKeys {
            prefix: Some(DETACH_PREFIX),
            left_arrow_opens_agents: true,
        }
    }

    /// Drive the reader with the key routes live and a stated attachment.
    fn read_attached(input: Chunks, keys: InlineKeys, attached: Option<u64>) -> InlineRead {
        let mut input = input;
        let mut paint = |_: &str, _: usize, _: &str, _: bool| {};
        read_line_inline_with_keys(
            &mut input,
            &mut paint,
            &[],
            &mut || {},
            keys,
            attached,
            None,
            None,
        )
        .expect("inline read")
    }

    /// **The `←` rule, positive half.** One `←` on an empty composer while
    /// attached detaches; it does not arm anything and it does not wait for a
    /// second press.
    ///
    /// Both controls are in this test rather than beside it, because each is
    /// what stops the claim being vacuous in one direction: a reader that
    /// returned `Detached` for every `←` would pass the first assertion and
    /// fail the other two.
    #[test]
    fn left_arrow_detaches_on_the_first_press_while_attached() {
        assert_eq!(
            read_attached(Chunks::bytewise(LEFT_ARROW), attach_keys(), Some(3)),
            InlineRead::Detached(String::new()),
            "A5: `\u{2190}` on an empty composer returns from an attached agent, \
             on the first press"
        );
        // CONTROL 1: a non-empty draft keeps the arrow as cursor movement,
        // attached or not. `ac` then `\u{2190}` then `B` submits `aBc` only if
        // the cursor really moved.
        assert_eq!(
            submitted(&read_attached(
                Chunks::bytewise(b"ac\x1b[DB\r"),
                attach_keys(),
                Some(3),
            )),
            "aBc",
            "a draft is content: `\u{2190}` in a non-empty draft is backward-char \
             while attached too"
        );
        // CONTROL 2: with nothing attached the same single press detaches
        // nothing -- it arms T4-S2's affordance and the read continues, so the
        // drained reader reports EOF.
        assert_eq!(
            read_attached(Chunks::bytewise(LEFT_ARROW), attach_keys(), None),
            InlineRead::Eof,
            "NON-VACUOUS: one `\u{2190}` with nothing attached is still T4-S2's \
             first press and nothing else"
        );
    }

    /// **The binding conflict, settled.** Two `←`s in one read while attached
    /// are a detach and **not** the agent view: the first press consumed the
    /// keystroke, so there is no second press to count.
    ///
    /// This is the case T4-S2's `LEFT_TWICE` constant exists for -- the whole
    /// affordance inside a single `read` -- and the one an implementation that
    /// counted presses before asking about attachment would get wrong.
    #[test]
    fn two_left_arrows_in_one_read_detach_rather_than_opening_the_agent_view() {
        assert_eq!(
            read_attached(Chunks::new([b"\x1b[D\x1b[D"]), attach_keys(), Some(5),),
            InlineRead::Detached(String::new()),
            "attached, `\u{2190}\u{2190}` is one detach and no view"
        );
        // NON-VACUOUS: the same two bytes with nothing attached still open the
        // view, so this test measures the attachment and not the chunking.
        assert_eq!(
            read_attached(Chunks::new([b"\x1b[D\x1b[D"]), attach_keys(), None),
            InlineRead::OpenAgentView(String::new()),
            "detached, `\u{2190}\u{2190}` is still T4-S2's agent view"
        );
    }

    /// **`left_arrow_opens_agents = false` does not disable the return path.**
    /// A1 scopes that key to the agent view's route; A5 names `←` as the way
    /// back with no condition on it.
    #[test]
    fn left_arrow_detaches_while_attached_even_with_the_agent_view_route_off() {
        let route_off = InlineKeys {
            prefix: Some(DETACH_PREFIX),
            left_arrow_opens_agents: false,
        };
        assert_eq!(
            read_attached(Chunks::bytewise(LEFT_ARROW), route_off, Some(7)),
            InlineRead::Detached(String::new()),
            "A5's return path is not the agent view's route and is not gated \
             by its switch"
        );
        // NON-VACUOUS: with the route off and nothing attached the arrow is
        // plain backward-char, which `ab` + `\u{2190}` + `c` proves by order.
        assert_eq!(
            submitted(&read_attached(
                Chunks::bytewise(b"ab\x1b[Dc\r"),
                route_off,
                None,
            )),
            "acb",
            "with the route off the arrow reaches the editor as before"
        );
    }

    /// Finding **F76**: **`Ctrl+X d` with text typed detaches and keeps the
    /// line.**
    ///
    /// `InlineRead::Detached` was a unit variant documented as "only ever
    /// reached on an empty" composer, which is `\u{2190}`'s precondition and
    /// not the chord's. The chord exists *because* `\u{2190}` is cursor
    /// movement once there is something to move over -- so it is the only
    /// detach route a non-empty draft has, and discarding the draft is the
    /// one thing it must not do.
    #[test]
    fn the_detach_chord_carries_a_non_empty_draft_back() {
        assert_eq!(
            read_attached(Chunks::bytewise(b"keep me\x18d"), attach_keys(), Some(3)),
            InlineRead::Detached("keep me".to_string()),
            "F76: `Ctrl+X d` with a draft typed detaches and carries the draft"
        );
        // The cursor need not be at the end: the chord carries the *draft*,
        // not the tail after the caret.
        assert_eq!(
            read_attached(Chunks::bytewise(b"ab\x1b[D\x18d"), attach_keys(), Some(3)),
            InlineRead::Detached("ab".to_string()),
            "the whole draft survives, wherever the caret was left"
        );
        // NON-VACUOUS: the payload is the draft and not a constant -- an
        // empty composer still detaches with an empty one.
        assert_eq!(
            read_attached(Chunks::bytewise(b"\x18d"), attach_keys(), Some(3)),
            InlineRead::Detached(String::new()),
            "NON-VACUOUS: on an empty composer the chord carries nothing back"
        );
        // NON-VACUOUS, the other way: with nothing attached the same bytes
        // are inert, so this measures the chord and not the typing -- the
        // draft stays a draft and the drained reader reports EOF.
        assert_eq!(
            read_attached(Chunks::bytewise(b"keep me\x18d"), attach_keys(), None),
            InlineRead::Eof,
            "NON-VACUOUS: unattached, `Ctrl+X d` detaches nothing"
        );
    }

    /// Finding **F76** with **F72**'s caution: a **pasted, multi-byte** draft
    /// survives the chord too.
    ///
    /// The composer is not one of the editor lines that bypass the decoder,
    /// so a bracketed paste reaches the draft as text (the filter's own test
    /// above pins that), and the payload must be the same bytes back -- a
    /// `char`-indexed truncation or a lossy re-encode would show here and
    /// nowhere else.
    #[test]
    fn the_detach_chord_carries_a_pasted_multi_byte_draft_back() {
        const PASTE: &[u8] = "\x1b[200~caf\u{e9} \u{2014} \u{3088}\x1b[201~".as_bytes();
        const DRAFT: &str = "caf\u{e9} \u{2014} \u{3088}";
        assert_eq!(
            read_attached(Chunks::new([PASTE, b"\x18d"]), attach_keys(), Some(4)),
            InlineRead::Detached(DRAFT.to_string()),
            "F76/F72: a pasted multi-byte draft comes back byte for byte"
        );
        // NON-VACUOUS: the same paste submitted is the same string, so the
        // claim above is about the detach payload and not about the paste
        // having arrived intact.
        assert_eq!(
            submitted(&read_attached(
                Chunks::new([PASTE, b"\r"]),
                attach_keys(),
                Some(4),
            )),
            DRAFT,
            "NON-VACUOUS: the paste itself reaches the draft intact"
        );
    }

    // ------------------------------------------------------------------
    // Card **T4-S6-ordinal**: `Ctrl+X 1..9`.
    //
    // **Measured first (F99/F100).** Today a digit after the prefix produces
    // **no `KeyItem` at all**: `KeyFilter::feed(b"\x181")` is empty, and the
    // reader's outcome for `\x181hi\r` is `Submitted("hi")` -- the digit is
    // swallowed by the "any other letter is inert" arm, so it does not even
    // reach the draft. The chord is therefore inert and silent today, and
    // every assertion below is on behaviour that does not exist yet.
    //
    // **Scope.** The chord is the **composer's**, exactly as `Ctrl+X a` and
    // `Ctrl+X d` are: this filter is where the prefix is a key at all. Inside
    // the agent view the overlay's own decoder never sees `0x18` as a prefix
    // (its `plain()` admits `0x20..=0x7e` only) and a bare digit falls into
    // `handle_overlay_input`'s `OverlayKey::Char(_) => {}` catch-all, so the
    // view keeps selecting with `↑`/`↓`. No third scope is invented.
    // ------------------------------------------------------------------

    /// **`Ctrl+X 1..9`.** Nine digits, each lifted out of the byte stream as
    /// the **ordinal** it is, through the same filter that already lifts
    /// `Ctrl+X a` and `Ctrl+X d` (**F69**: this is the real decoder, not a
    /// predicate beside it).
    ///
    /// The controls are the rules the new digits must not break (**F68** and
    /// T4-S2's): a bare digit is content, a chord split across two reads is
    /// still one keystroke, a chord inside a bracketed paste is text, and the
    /// two letters that already route still route.
    #[test]
    fn the_key_filter_lifts_the_ordinal_chord_digits_one_through_nine() {
        for ordinal in 1usize..=9 {
            let chord = format!("\x18{ordinal}");
            assert_eq!(
                KeyFilter::new(attach_keys()).feed(chord.as_bytes()),
                vec![KeyItem::Attach(ordinal)],
                "design \u{00a7}4.7: `Ctrl+X {ordinal}` carries the ordinal \
                 {ordinal}, which is a position and never an id"
            );
        }
        // Split across two reads, as two keystrokes routinely are.
        let mut split = KeyFilter::new(attach_keys());
        assert_eq!(
            split.feed(b"\x18"),
            Vec::new(),
            "the prefix alone yields nothing"
        );
        assert_eq!(
            split.feed(b"4"),
            vec![KeyItem::Attach(4)],
            "and the digit in the next read completes the chord"
        );
        // **`Ctrl+X 0`: decided inert.** The ordinal is 1-based, so `0` names
        // no position; it is not an alias for the first row (that is `1`) and
        // not a prefix of `10` (which would make `Ctrl+X 1` ambiguous until
        // the next keystroke -- **F75**'s shape, invisible at the keypress).
        // It is dropped exactly as any other non-chord letter is, so it does
        // not reach the draft either.
        assert_eq!(
            KeyFilter::new(attach_keys()).feed(b"\x180"),
            Vec::new(),
            "`Ctrl+X 0` is inert: 0 is not an ordinal, and the digit is \
             dropped rather than typed"
        );
        // CONTROL: a bare digit is a character in the draft.
        assert_eq!(
            KeyFilter::new(attach_keys()).feed(b"3"),
            vec![KeyItem::Bytes(b"3".to_vec())],
            "NON-VACUOUS: without the prefix, `3` is content"
        );
        // CONTROL: inside a bracketed paste the same two bytes are text.
        let pasted = b"\x1b[200~\x183\x1b[201~";
        assert_eq!(
            KeyFilter::new(attach_keys()).feed(pasted),
            vec![KeyItem::Bytes(pasted.to_vec())],
            "a pasted chord is pasted text, not a chord"
        );
        // CONTROL: the two letters that already route are unchanged.
        assert_eq!(
            KeyFilter::new(attach_keys()).feed(b"\x18a"),
            vec![KeyItem::Open],
            "`Ctrl+X a` is unchanged by the digits"
        );
        assert_eq!(
            KeyFilter::new(attach_keys()).feed(b"\x18d"),
            vec![KeyItem::Detach],
            "`Ctrl+X d` is unchanged by the digits"
        );
    }

    /// Shift+Left/Right lift out whole as agent-cycle steps, survive a split
    /// read, never steal a plain arrow, and — the load-bearing one — are text
    /// inside a bracketed paste so a pasted escape sequence never moves agents.
    #[test]
    fn input_box_setting_defaults_on_and_honours_explicit_opt_out() {
        use crate::tui_bridge::input_box_setting;
        use std::ffi::OsStr;
        // Unset: the production default (on) is what applies.
        assert!(input_box_setting(None, true));
        assert!(
            !input_box_setting(None, false),
            "test builds default to the row"
        );
        // Explicit values win over the default either way.
        assert!(!input_box_setting(Some(OsStr::new("0")), true));
        assert!(!input_box_setting(Some(OsStr::new("false")), true));
        assert!(input_box_setting(Some(OsStr::new("1")), false));
        assert!(input_box_setting(Some(OsStr::new("true")), false));
        // Junk falls back to the default rather than guessing.
        assert!(input_box_setting(Some(OsStr::new("yes please")), true));
    }

    /// Shift+Left/Right lift out whole as agent-cycle steps, survive a split
    /// read, never steal a plain arrow, and — the load-bearing one — are text
    /// inside a bracketed paste so a pasted escape sequence never moves agents.
    #[test]
    fn the_key_filter_lifts_shift_arrows_as_agent_cycle_steps() {
        assert_eq!(
            KeyFilter::new(attach_keys()).feed(b"\x1b[1;2D"),
            vec![KeyItem::CycleAgent(-1)],
            "Shift+Left steps to the previous agent"
        );
        assert_eq!(
            KeyFilter::new(attach_keys()).feed(b"\x1b[1;2C"),
            vec![KeyItem::CycleAgent(1)],
            "Shift+Right steps to the next agent"
        );
        // Split across reads — the prefix is held, not resolved as Esc.
        let mut split = KeyFilter::new(attach_keys());
        assert_eq!(split.feed(b"\x1b[1;2"), Vec::new(), "partial sequence held");
        assert_eq!(
            split.feed(b"C"),
            vec![KeyItem::CycleAgent(1)],
            "completed next read"
        );
        // CONTROL: a plain left arrow is still the editor's backward-char, not a cycle.
        assert_eq!(
            KeyFilter::new(attach_keys()).feed(b"\x1b[D"),
            vec![KeyItem::Left],
            "plain Left is unchanged"
        );
        // Review F3: with agent routes off (the fullscreen/overlay readers run
        // under InlineKeys::disabled()), Shift+Left is NOT an agent action —
        // it passes through as bytes, so it can never end those sessions.
        assert_eq!(
            KeyFilter::new(InlineKeys::disabled()).feed(b"\x1b[1;2D"),
            vec![KeyItem::Bytes(b"\x1b[1;2D".to_vec())],
            "disabled keys never lift Shift+Left into CycleAgent"
        );
        // CONTROL (copy-paste safety): the same bytes inside a bracketed paste
        // are text, never a cycle — pasting content must never switch agents.
        let pasted = b"\x1b[200~\x1b[1;2D\x1b[201~";
        assert_eq!(
            KeyFilter::new(attach_keys()).feed(pasted),
            vec![KeyItem::Bytes(pasted.to_vec())],
            "a pasted Shift+Left is pasted text, not a cycle"
        );
    }

    /// The reader reports the ordinal and **carries the draft** (**F76**: the
    /// chord has no empty-composer precondition, so it must not cost the user
    /// the line they typed), returning *at* the chord so bytes typed after it
    /// belong to the next read.
    ///
    /// It reports the **ordinal** and not an agent, and that is the card's
    /// whole shape: resolving a position needs the registry, and the reader
    /// has none -- design §4.7 names exactly that coupling.
    #[test]
    fn the_ordinal_chord_reports_the_ordinal_and_carries_the_draft() {
        assert_eq!(
            read_attached(
                Chunks::new([b"typed".as_slice(), b"\x183", b"later\r"]),
                attach_keys(),
                None,
            ),
            InlineRead::AttachOrdinal(3, "typed".to_string()),
            "the chord reports ordinal 3 and hands the draft back, and the \
             bytes after it belong to the next read"
        );
        // Attaching is reached from **both** states: with nothing attached it
        // is how the user attaches, and while attached it is how they switch.
        assert_eq!(
            read_attached(
                Chunks::new([b"\x187".as_slice(), b"\r"]),
                attach_keys(),
                Some(42),
            ),
            InlineRead::AttachOrdinal(7, String::new()),
            "the chord has no attachment precondition: while attached it \
             switches agent"
        );
        // CONTROL, and today's measured behaviour preserved: `Ctrl+X 0` is no
        // route, and its digit does not reach the draft.
        assert_eq!(
            read_attached(
                Chunks::new([b"\x180".as_slice(), b"\r"]),
                attach_keys(),
                Some(42),
            ),
            InlineRead::Submitted(String::new()),
            "NON-VACUOUS: `Ctrl+X 0` is inert, and its digit is not typed \
             into the draft"
        );
        // CONTROL: a bare digit is typed.
        assert_eq!(
            read_attached(
                Chunks::new([b"3".as_slice(), b"\r"]),
                attach_keys(),
                Some(42),
            ),
            InlineRead::Submitted("3".to_string()),
            "NON-VACUOUS: without the prefix the digit is just a character"
        );
    }

    /// **`Ctrl+X d`.** The chord is lifted out of the byte stream by the one
    /// filter that already lifts `Ctrl+X a`, so the letter can never reach the
    /// draft and the *decision* about whether anything is attached stays in
    /// the reader.
    ///
    /// The three controls are the rules this new letter must not break
    /// (**F68** and T4-S2's): a bare `d` is content, a chord split across two
    /// reads is still one keystroke, and `Ctrl+X d` **inside a bracketed
    /// paste** is text.
    #[test]
    fn the_key_filter_lifts_the_detach_chord_out_of_the_byte_stream() {
        assert_eq!(
            KeyFilter::new(attach_keys()).feed(b"\x18d"),
            vec![KeyItem::Detach],
            "design \u{00a7}4.7: `Ctrl+X d` detaches, so the filter must lift it"
        );
        // Split across two reads, as two keystrokes routinely are.
        let mut split = KeyFilter::new(attach_keys());
        assert_eq!(
            split.feed(b"\x18"),
            Vec::new(),
            "the prefix alone yields nothing"
        );
        assert_eq!(
            split.feed(b"d"),
            vec![KeyItem::Detach],
            "and the letter in the next read completes the chord"
        );
        // CONTROL: a bare `d` is a character in the draft.
        assert_eq!(
            KeyFilter::new(attach_keys()).feed(b"d"),
            vec![KeyItem::Bytes(b"d".to_vec())],
            "NON-VACUOUS: without the prefix, `d` is content"
        );
        // CONTROL: inside a bracketed paste the same two bytes are text
        // (**F68**: a chunk of bytes is not a sequence of intentions).
        let pasted = b"\x1b[200~\x18d\x1b[201~";
        assert_eq!(
            KeyFilter::new(attach_keys()).feed(pasted),
            vec![KeyItem::Bytes(pasted.to_vec())],
            "a pasted chord is pasted text, not a chord"
        );
        // CONTROL: the letter that already existed still routes.
        assert_eq!(
            KeyFilter::new(attach_keys()).feed(b"\x18a"),
            vec![KeyItem::Open],
            "`Ctrl+X a` is unchanged by a second chord letter"
        );
    }

    /// **The prompt label is the standing evidence of attachment** (design
    /// §4.5), and it is what makes the `←` rule above legible rather than
    /// modal: the context the keystroke depends on is in front of the caret.
    ///
    /// The label **replaces** the base prompt rather than decorating it: the
    /// base is configurable and `agent 12> kitsu> ` would be absurd.
    #[test]
    fn the_composer_prompt_names_the_attached_agent() {
        assert_eq!(
            composer_prompt("> ", Some(3)),
            "agent 3> ",
            "design \u{00a7}4.5: the prompt label changes to `agent 3>`"
        );
        assert_eq!(
            composer_prompt("kitsu> ", Some(12)),
            "agent 12> ",
            "the label replaces the base prompt, and an id is not one digit"
        );
        // NON-VACUOUS: detached, the base prompt is handed back untouched, so
        // the label cannot be a constant.
        assert_eq!(
            composer_prompt("kitsu> ", None),
            "kitsu> ",
            "detached, the composer is the session's own prompt"
        );
    }

    // =====================================================================
    // Card **T4-S8**, requirement **A8**: the ambient counter's latency.
    //
    // **FAILING-FIRST.** A8 gives the ambient background-agent counter 250 ms
    // from a registry change "in TUI modes". During a turn that is met:
    // `run_live_typeahead` re-pulls the counts and repaints once per
    // `AMBIENT_TICK_POLL_MS` tick. At the **idle composer** it is not met at
    // all: `read_line_inline_with_keys` blocks in `read()` with no timeout,
    // and nothing else repaints between turns, so a registry change while the
    // user sits at the prompt reaches the screen only when the user types,
    // resizes or submits. A8's "next prompt cycle" carve-out is granted to
    // `classic` and not to the TUI.
    //
    // **No clock is paused and nothing sleeps**, deliberately: there are four
    // open flakes in this tree and a timing test that measures elapsed time is
    // the fifth. Both halves below are structural. The latency is governed by
    // *when a paint happens*, not by any clock inside the composer — the
    // counts are a pull, read through `StatusSource`'s closure at paint time
    // — so the cadence's interval, named, is the whole of what A8 constrains.
    // =====================================================================

    /// An **idle** live-status fixture: the same wiring as
    /// [`live_status_fixture`] with no `begin_turn`, so `elapsed` is `None`
    /// and the row is the between-turns row A8's TUI half is about. The cell
    /// stands in for the registry's `blocked` count, exactly as
    /// `live_status_fixture`'s cell stands in for the queue depth.
    fn idle_ambient_fixture() -> (BridgeGuard, SharedSink, Arc<StatusSource>, Arc<AtomicUsize>) {
        let (guard, _out, err) = install_inline(T3_S12_ROWS, T3_S12_VIEWPORT_ROWS, "status: idle");
        let guard = guard.expect("inline mode installs a guard");
        let blocked = Arc::new(AtomicUsize::new(0));
        let cell = blocked.clone();
        let source = StatusSource::new(Box::new(move || StatusCounts {
            blocked: cell.load(Ordering::Relaxed),
            ..Default::default()
        }));
        source.set_usage("example-model-v1", 1_000, 100_000);
        guard.enable_live_status(source.clone());
        (guard, err, source, blocked)
    }

    /// The inert title publisher the cadence's **pre-existing** tests pass:
    /// they assert the painted row and nothing about the title, so the
    /// publisher they hand the step must do nothing and their assertions are
    /// unchanged. Card **H2-cadence** added the parameter; the publisher is a
    /// generic so the compiler forces the one production caller to supply a
    /// real one, which is what keeps `publish_cadence_state_title` from being
    /// a function nothing drives (finding **F50**).
    fn no_cadence_title() -> impl Fn(usize, usize) {
        |_, _| {}
    }

    /// **The acceptance test, and it is red.** The idle composer must have an
    /// ambient cadence, and that cadence must fit A8's 250 ms.
    ///
    /// Half 1 is the **non-vacuity control** and passes today: with a cadence
    /// tick simulated by hand, a change to the counts does reach the pinned
    /// row, as the row the painter actually wrote. So the pull path, the
    /// composition and the surface are all already right, and the one thing
    /// A8 is missing at idle is a caller — which is what makes half 2's
    /// assertion reachable by wiring alone (`registry.subscribe()`, or a
    /// bounded timeout on the idle read) rather than by a rewrite.
    ///
    /// Half 2 is the failing half: [`idle_ambient_refresh_interval`] reports
    /// `None`, because there is no cadence between turns at all.
    ///
    /// The row is asserted at **120 columns** as well as at the terminal's
    /// own, finding **F78**: the status row's drop order is not monotonic in
    /// width, so the segment is checked at a width where it demonstrably
    /// fits rather than assumed to survive every width.
    #[test]
    #[serial]
    fn the_idle_composer_must_have_an_ambient_cadence() {
        let _state = GlobalState::capture();
        ReducedMotion::off();
        let (guard, err, source, blocked) = idle_ambient_fixture();

        // Precondition: an idle row, and no tag on it, because nothing is
        // blocked. Pinned on the composed row rather than on an absence of
        // bytes, finding **F71**/**F41**.
        let cols = live::terminal_columns();
        let idle_row = source.compose(cols);
        let tag = live::agents_tag(1, 0).expect("1 blocked agent has a tag at all");
        assert!(
            !idle_row.contains(tag.trim_end()),
            "precondition: nothing is blocked, so the idle row carries no \
             ambient tag: {idle_row:?}"
        );

        // Half 1, the control. A registry change, then one cadence tick by
        // hand: the tag reaches the pinned row.
        blocked.store(1, Ordering::Relaxed);
        past_the_frame_budget();
        let before = err.text().len();
        repaint_status_row();
        let tail = err.text()[before..].to_string();
        assert_eq!(
            tail,
            expected_status_paint(
                T3_S12_ROWS,
                T3_S12_VIEWPORT_ROWS,
                &live::compose_status_row(
                    &live::StatusFields {
                        model: "example-model-v1".to_string(),
                        used: 1_000,
                        effective: 100_000,
                        elapsed: None,
                        blocked: 1,
                        ..Default::default()
                    },
                    cols,
                ),
            ),
            "one cadence tick must paint the idle row the counts describe"
        );
        assert!(
            tail.contains(tag.trim_end()),
            "and that row carries the ambient tag: {tail:?}"
        );
        // F78: and at 120 columns, a width where the segment fits.
        let wide = live::compose_status_row(
            &live::StatusFields {
                model: "example-model-v1".to_string(),
                used: 1_000,
                effective: 100_000,
                elapsed: None,
                blocked: 1,
                ..Default::default()
            },
            120,
        );
        assert!(
            wide.contains(tag.trim_end()),
            "the ambient tag fits at 120 columns: {wide:?}"
        );

        // Half 2, **the failing half**: nothing calls that tick at idle.
        let interval = super::idle_ambient_refresh_interval().expect(
            "A8: the ambient counter MUST update within 250 ms of a registry change in TUI \
             modes, so the idle composer needs a cadence -- today it blocks in `read()` with \
             no timeout and a blocked background agent changes nothing on screen until the \
             user types",
        );
        assert!(
            interval <= super::AMBIENT_LATENCY_BUDGET,
            "A8 gives the ambient counter {:?}; the idle cadence runs at {interval:?}",
            super::AMBIENT_LATENCY_BUDGET,
        );
        assert!(
            interval > std::time::Duration::ZERO,
            "a zero interval is a busy loop, not a cadence"
        );
        drop(guard);
    }

    /// Card **T4-S8**, requirement **A8**, and the **behavioural** half the
    /// seam's doc comment asks for: an idle registry change reaches the
    /// painted surface **with no byte read**.
    ///
    /// Why this test and not just the interval: making
    /// [`idle_ambient_refresh_interval`] return `Some(..)` costs one line and
    /// holds nothing, which is finding **F50**'s failure exactly -- a
    /// requirement passing a test while nothing implements it. So this drives
    /// the real cadence, [`idle_ambient_cadence_step`], the same function the
    /// session's one `spawn_idle_ambient_cadence` call runs for the life of
    /// the session, and asserts the **bytes the painter wrote** (finding
    /// **F71**: a visibility claim goes through the surface that paints it,
    /// never through state).
    ///
    /// The registry is stood in for by a plain `watch` channel, which is what
    /// `AgentRegistry::subscribe` hands out: the wake carries the counts and
    /// the fixture's cell carries the pull the painter reads, which is how
    /// production is wired (`main.rs` projects `(blocked, running)` off the
    /// snapshot, and the `StatusSource` closure re-reads the registry at
    /// paint time). One registry change moves both.
    ///
    /// **Nothing reads a byte**, and that is the point: the composer stays
    /// blocked in `read()` throughout -- no input is fed to any reader here,
    /// no decoder and no editor line is touched (**F72**/**F68**).
    ///
    /// **No latency is measured.** Both wakes are already-ready `changed()`
    /// awaits, so `block_on` returns as soon as the step has painted; the only
    /// sleep is the in-tree frame-budget helper that clears note **N3**'s
    /// coalescing window, plus the step's own one-frame retry.
    #[test]
    #[serial]
    fn the_idle_ambient_cadence_paints_a_registry_change_with_no_byte_read() {
        let _state = GlobalState::capture();
        ReducedMotion::off();
        let (guard, err, _source, blocked) = idle_ambient_fixture();
        let cols = live::terminal_columns();
        let tag = live::agents_tag(1, 0).expect("1 blocked agent has a tag at all");

        // The interval the cadence actually defers by, inside A8's budget.
        let interval =
            super::idle_ambient_refresh_interval().expect("the idle composer has a cadence");
        assert!(
            interval <= super::AMBIENT_LATENCY_BUDGET,
            "A8 gives the ambient counter {:?}; the idle cadence defers by {interval:?}",
            super::AMBIENT_LATENCY_BUDGET,
        );

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("a current-thread runtime for the cadence step");
        let (sender, mut changes) = tokio::sync::watch::channel((0usize, 0usize));
        let counts = |pair: &(usize, usize)| *pair;
        let mut seen = *changes.borrow_and_update();
        past_the_frame_budget();

        // A **bare wake** first: the registry published, but nothing the row
        // shows moved. No frame -- the same rule the composer applies to a
        // resize-less signal wake, and what reduced motion and N3's budget
        // both depend on.
        let before = err.text().len();
        sender
            .send((0, 0))
            .expect("the cadence is still subscribed");
        assert!(
            runtime.block_on(super::idle_ambient_cadence_step(
                &mut changes,
                &counts,
                &mut seen,
                &no_cadence_title(),
            )),
            "the cadence keeps running while the registry is alive"
        );
        assert_eq!(
            err.text().len(),
            before,
            "a bare wake must paint no frame: {:?}",
            err.text()[before..].escape_debug().to_string()
        );

        // Now a real one: an agent blocks while the user sits at the prompt.
        blocked.store(1, Ordering::Relaxed);
        sender
            .send((1, 0))
            .expect("the cadence is still subscribed");
        let before = err.text().len();
        assert!(runtime.block_on(super::idle_ambient_cadence_step(
            &mut changes,
            &counts,
            &mut seen,
            &no_cadence_title(),
        )));
        let tail = err.text()[before..].to_string();
        assert_eq!(
            tail,
            expected_status_paint(
                T3_S12_ROWS,
                T3_S12_VIEWPORT_ROWS,
                &live::compose_status_row(
                    &live::StatusFields {
                        model: "example-model-v1".to_string(),
                        used: 1_000,
                        effective: 100_000,
                        elapsed: None,
                        blocked: 1,
                        ..Default::default()
                    },
                    cols,
                ),
            ),
            "the cadence alone -- no keystroke, no resize, no submitted line --              must put the idle row the registry describes on screen"
        );
        assert!(
            tail.contains(tag.trim_end()),
            "and that painted row carries A8's ambient tag: {tail:?}"
        );

        // And it stops when the registry is gone rather than spinning.
        drop(sender);
        assert!(
            !runtime.block_on(super::idle_ambient_cadence_step(
                &mut changes,
                &counts,
                &mut seen,
                &no_cadence_title(),
            )),
            "a closed registry ends the cadence"
        );
        drop(guard);
    }

    // -----------------------------------------------------------------------
    // Card H2-title: the OSC terminal title, and `[tui] title` gating it.
    //
    // FAILING-FIRST. `state_title` returns the empty string and
    // `write_state_title` writes nothing, so every assertion below fails on
    // this commit; the fullscreen pair fails in the `title = false`
    // direction, where the stack is pushed and popped today regardless of the
    // key (**F139**).
    // -----------------------------------------------------------------------

    /// Restores `[tui] title`'s process-global on the way out, so a test that
    /// turns it off cannot leave it off for the next one. The same discipline
    /// [`GlobalState`] applies to the surface slot, and the reason these two
    /// tests are `#[serial]` as well.
    struct TitleGate(bool);

    impl TitleGate {
        fn set(on: bool) -> Self {
            let was = super::title_enabled();
            super::set_title_enabled(on);
            Self(was)
        }
    }

    impl Drop for TitleGate {
        fn drop(&mut self) {
            super::set_title_enabled(self.0);
        }
    }

    /// **The state word is the state file's, and the count is rendered the one
    /// way H2 left open.**
    ///
    /// `process_state` already owns the precedence — *"blocked outranks
    /// working"* — and it is already tested where it lives. So the state word
    /// is never spelled out here: each expectation is built **from**
    /// `process_state`'s own return, which is what makes a second, divergent
    /// state computation in `state_title` fail this test rather than pass it.
    ///
    /// The rendering decisions, pinned because H2 specifies the alternation
    /// and not the layout: the count is always present (`0 agents`, never
    /// omitted), and it is singular at exactly one.
    #[test]
    fn the_state_title_takes_its_word_from_process_state_and_always_carries_a_count() {
        use super::state_title;
        use crate::agent_state_file::process_state;

        // (running, blocked, turn_active, live agents, expected count text)
        let cases = [
            (0usize, 0usize, false, 0usize, "0 agents"),
            (1, 0, false, 1, "1 agent"),
            (0, 0, true, 0, "0 agents"),
            (2, 1, true, 3, "3 agents"),
            (0, 2, false, 2, "2 agents"),
        ];
        for (running, blocked, turn_active, agents, count) in cases {
            let word = process_state(running, blocked, turn_active);
            let title = state_title(word, agents);
            assert_eq!(
                title,
                format!("kitsu · {word} · {count}"),
                "the title is `kitsu · <state> · <N agent(s)>` with the state \
                 word `process_state` returned for ({running}, {blocked}, \
                 {turn_active}) and never a second computation of it"
            );
        }

        // NON-VACUOUS, and the half that would survive a `state_title` that
        // ignored its argument: the three words are distinguishable, and the
        // blocked case above is one `process_state` only returns because
        // blocked outranks a running agent and an active turn at once.
        assert_eq!(process_state(2, 1, true), "blocked");
        assert_ne!(
            state_title(process_state(2, 1, true), 3),
            state_title(process_state(2, 0, true), 3),
            "a blocked process and a working one must not read the same"
        );
    }

    /// **The OSC 2 sequence, and `[tui] title = false` writing no bytes at
    /// all.**
    ///
    /// Bidirectional by mutation, because there are two failure modes: a
    /// title that is never written, and a title written on a run that asked
    /// for none.
    ///
    /// The hostile state word pins that `kitsu_tui::sanitize_title` is
    /// **used** — it is already tested where it lives, including that it
    /// cannot escape its own OSC sequence, so what is asserted here is that
    /// the boundary is crossed on this path and not what the sanitiser does.
    #[test]
    fn the_osc_title_is_written_once_when_enabled_and_never_when_the_key_is_off() {
        use super::{state_title, write_state_title};

        let mut on: Vec<u8> = Vec::new();
        write_state_title(&mut on, true, "working", 2);
        assert_eq!(
            String::from_utf8_lossy(&on),
            format!("\x1b]2;{}\x1b\\", state_title("working", 2)),
            "one OSC 2 sequence, ST-terminated, carrying exactly the title \
             `state_title` renders"
        );

        let mut off: Vec<u8> = Vec::new();
        write_state_title(&mut off, false, "working", 2);
        assert!(
            off.is_empty(),
            "`[tui] title = false` writes no bytes at all, but wrote {:?}",
            String::from_utf8_lossy(&off)
        );

        let mut hostile: Vec<u8> = Vec::new();
        write_state_title(&mut hostile, true, "idle\x1b]0;pwned\x07", 0);
        let text = String::from_utf8_lossy(&hostile).into_owned();
        let payload = text
            .strip_prefix("\x1b]2;")
            .and_then(|rest| rest.strip_suffix("\x1b\\"))
            .unwrap_or_else(|| panic!("one OSC 2 sequence, but wrote {:?}", text));
        assert!(
            !payload.contains('\x1b') && !payload.contains('\x07'),
            "the title must go through `sanitize_title`, so no control byte \
             can reach the terminal inside it; payload {:?}",
            payload
        );
    }

    /// **`[tui] title = false` suppresses the title *stack* too.**
    ///
    /// **F139**'s measurement: `enter_fullscreen` pushes `CSI 22;2t` and the
    /// segment's `Drop` pops `CSI 23;2t` in production **without consulting
    /// the key**, so a user who set `title = false` gets the stack pushed
    /// anyway. Both directions are asserted over the same geometry, and the
    /// alternate screen is checked in the off case as the non-vacuity anchor:
    /// the segment must still have run and still have taken the screen, or
    /// the absence of the title bytes would prove nothing.
    #[test]
    #[serial]
    fn a_fullscreen_segment_pushes_the_title_stack_only_when_the_title_key_is_on() {
        let _state = GlobalState::capture();

        let push = String::from_utf8_lossy(super::PUSH_TITLE).into_owned();
        let pop = String::from_utf8_lossy(super::POP_TITLE).into_owned();

        {
            let _gate = TitleGate::set(true);
            let sink = SharedSink::new();
            let segment = enter_fullscreen(Box::new(sink.clone()), &[], 24, 80, 16);
            assert!(
                sink.text().contains(&push),
                "with `title = true` the segment pushes the title stack, as \
                 it does today; it wrote:\n{}",
                sink.text().escape_debug()
            );
            drop(segment);
            assert!(
                sink.text().contains(&pop),
                "...and pops it on the way out; it wrote:\n{}",
                sink.text().escape_debug()
            );
        }

        {
            let _gate = TitleGate::set(false);
            let sink = SharedSink::new();
            let segment = enter_fullscreen(Box::new(sink.clone()), &[], 24, 80, 16);
            drop(segment);
            let text = sink.text();
            assert!(
                text.contains("\x1b[?1049h") && text.contains("\x1b[?1049l"),
                "NON-VACUOUS: the segment still ran and still took the \
                 alternate screen, so the missing title bytes below are the \
                 key's doing; it wrote:\n{}",
                text.escape_debug()
            );
            assert!(
                !text.contains(&push) && !text.contains(&pop),
                "`title = false` must push and pop no title stack (F139); it \
                 wrote:\n{}",
                text.escape_debug()
            );
            assert!(
                !text.contains("\x1b]2;"),
                "...and set no title either; it wrote:\n{}",
                text.escape_debug()
            );
        }
    }

    // -----------------------------------------------------------------------
    // Card **H2-cadence**: the state title goes stale mid-turn.
    //
    // FAILING-FIRST. `write_cadence_state_title` writes nothing and
    // `idle_ambient_cadence_step` never calls the publisher it is handed, so
    // both tests below fail on this commit.
    //
    // **The premise this card rests on, confirmed by name rather than
    // assumed** (the shipped docs said the opposite, finding **F64**):
    // `spawn_idle_ambient_cadence` is *"deliberately ungated"* per its own
    // rustdoc, its one production caller sits beside the session's
    // `StatusSource`, the step dedupes on `(blocked, running)` -- the title's
    // own count inputs -- and nothing in it consults a turn flag, so it fires
    // mid-turn. There **is** a general change hook in a chat session.
    // -----------------------------------------------------------------------

    /// **The word, and the one transition the three publish sites cannot
    /// cover.**
    ///
    /// Measured rather than assumed. Mid-turn `turn_active` is `true`, so the
    /// word is `blocked > 0 ? "blocked" : "working"` and the counts alone
    /// determine it in every case **but one**: an agent finishing mid-turn
    /// into `(0, 0)`, where the true word is `"working"` and a cadence that
    /// assumed `turn_active = false` writes `"idle"`. That is the card's own
    /// symptom, so the flag is plumbed and this test is what pins it -- the
    /// first case below is red against a publisher that ignores the flag,
    /// which is the bidirectional mutation for this decision.
    ///
    /// The word is never spelled out: each expectation is built **from**
    /// `process_state`'s return, so a second computation of *"blocked
    /// outranks working"* inside the cadence fails this rather than passing.
    /// Note the argument order -- the cadence's pair is `(blocked, running)`,
    /// `process_state` takes `(running, blocked, ..)`, and a transposition is
    /// exactly what the asymmetric cases here catch.
    #[test]
    fn the_cadence_title_carries_the_turns_word_and_is_suppressed_by_the_gate() {
        use super::{state_title, write_cadence_state_title};
        use crate::agent_state_file::process_state;

        // (blocked, running, turn_active)
        let cases = [
            // The transition the three sites miss: nothing live, turn active.
            (0usize, 0usize, true),
            // The same counts at the idle prompt, which must read `idle`.
            (0, 0, false),
            // An agent blocks mid-turn: `blocked` outranks the active turn.
            (1, 0, true),
            // Asymmetric, so a transposed pair cannot pass: one running.
            (0, 1, true),
            (0, 1, false),
            (2, 1, true),
        ];
        for (blocked, running, turn_active) in cases {
            let mut buffer: Vec<u8> = Vec::new();
            write_cadence_state_title(&mut buffer, true, blocked, running, turn_active);
            let word = process_state(running, blocked, turn_active);
            assert_eq!(
                String::from_utf8_lossy(&buffer),
                format!("\x1b]2;{}\x1b\\", state_title(word, blocked + running)),
                "the cadence writes the same OSC 2 title the three publish \
                 sites do, with `process_state`'s word for (running {running}, \
                 blocked {blocked}, turn_active {turn_active}) and the live \
                 count `blocked + running`"
            );
        }

        // NON-VACUOUS, and the half that fails a cadence which drops the turn
        // flag: at `(0, 0)` the mid-turn title and the idle one must differ,
        // and the difference is exactly `working` against `idle`.
        assert_eq!(process_state(0, 0, true), "working");
        assert_eq!(process_state(0, 0, false), "idle");
        let mut mid: Vec<u8> = Vec::new();
        let mut idle: Vec<u8> = Vec::new();
        write_cadence_state_title(&mut mid, true, 0, 0, true);
        write_cadence_state_title(&mut idle, true, 0, 0, false);
        assert_ne!(
            mid, idle,
            "an agent finishing mid-turn must not leave the title reading \
             `idle` while the turn is still running"
        );

        // The gate, the other direction: `[tui] title = false` or any surface
        // the title may not touch writes **no bytes at all** from this new
        // publish point either (requirement N1). The gate itself stays
        // `title_enabled() && title_surface_allows_osc()` and is evaluated in
        // `publish_cadence_state_title`; this pins that the writer honours it.
        let mut off: Vec<u8> = Vec::new();
        write_cadence_state_title(&mut off, false, 1, 2, true);
        assert!(
            off.is_empty(),
            "a disallowed surface must receive nothing, but received {:?}",
            String::from_utf8_lossy(&off)
        );
    }

    /// **The cadence publishes the title once per real change, beside the
    /// repaint.**
    ///
    /// Drives the real [`idle_ambient_cadence_step`], the same function the
    /// session's one `spawn_idle_ambient_cadence` call runs for the life of
    /// the session, and asserts three decisions this card had to pin:
    ///
    /// * **A bare wake publishes nothing.** The step's dedupe is on
    ///   `(blocked, running)`, the title's own inputs, so a wake that moves
    ///   the registry's generation and nothing the title shows must not write
    ///   a title either.
    /// * **Once per change, not twice.** The step repaints twice -- note
    ///   **N3**'s retry, because a paint inside the 1/60 s window returns
    ///   without recording the key. The title has no such window: it is an
    ///   unconditional write, so a second one is pure duplicate bytes. The
    ///   count is pinned at exactly one.
    /// * **Beside the repaint, not inside it.** The painted row is asserted
    ///   on the same wake, so the title cannot have been folded into
    ///   `repaint_status_row` -- which is inert in classic mode and in any
    ///   session that never enabled a live status row, and would therefore
    ///   leave a styled `classic` session exactly as stale as before.
    #[test]
    #[serial]
    fn the_idle_ambient_cadence_publishes_the_state_title_once_per_change() {
        let _state = GlobalState::capture();
        ReducedMotion::off();
        let (guard, err, _source, blocked) = idle_ambient_fixture();
        let cols = live::terminal_columns();

        let published: Arc<std::sync::Mutex<Vec<(usize, usize)>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorder = {
            let published = published.clone();
            move |blocked: usize, running: usize| {
                published
                    .lock()
                    .expect("the recorded publishes")
                    .push((blocked, running));
            }
        };

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("a current-thread runtime for the cadence step");
        let (sender, mut changes) = tokio::sync::watch::channel((0usize, 0usize));
        let counts = |pair: &(usize, usize)| *pair;
        let mut seen = *changes.borrow_and_update();
        past_the_frame_budget();

        // A bare wake: the registry published, the title's inputs did not
        // move. No title, and no frame.
        let before = err.text().len();
        sender.send((0, 0)).expect("the cadence is subscribed");
        assert!(runtime.block_on(super::idle_ambient_cadence_step(
            &mut changes,
            &counts,
            &mut seen,
            &recorder,
        )));
        assert!(
            published.lock().expect("the recorded publishes").is_empty(),
            "a bare wake must publish no title: {:?}",
            published.lock().expect("the recorded publishes")
        );
        assert_eq!(
            err.text().len(),
            before,
            "NON-VACUOUS control: a bare wake paints no frame either, so the \
             step really did take its dedupe path"
        );

        // A real change: an agent blocks while a turn is running.
        blocked.store(1, Ordering::Relaxed);
        sender.send((1, 0)).expect("the cadence is subscribed");
        let before = err.text().len();
        assert!(runtime.block_on(super::idle_ambient_cadence_step(
            &mut changes,
            &counts,
            &mut seen,
            &recorder,
        )));

        assert_eq!(
            *published.lock().expect("the recorded publishes"),
            vec![(1usize, 0usize)],
            "the cadence publishes the title **exactly once** for one change, \
             carrying the `(blocked, running)` pair it deduped on -- not twice, \
             although it repaints twice for note N3's retry"
        );

        // And beside the repaint, not inside it: the row the cadence paints
        // is unchanged by the title riding along.
        let tail = err.text()[before..].to_string();
        assert_eq!(
            tail,
            expected_status_paint(
                T3_S12_ROWS,
                T3_S12_VIEWPORT_ROWS,
                &live::compose_status_row(
                    &live::StatusFields {
                        model: "example-model-v1".to_string(),
                        used: 1_000,
                        effective: 100_000,
                        elapsed: None,
                        blocked: 1,
                        ..Default::default()
                    },
                    cols,
                ),
            ),
            "the cadence still paints the row the registry describes, so the \
             title is published **beside** `repaint_status_row` and has not \
             been folded into it -- which would be inert in classic mode"
        );
        assert!(
            !tail.contains("\x1b]2;"),
            "...and the title is not written onto the viewport's own surface, \
             which a classic session never installs; it wrote:\n{}",
            tail.escape_debug()
        );

        // **The inertness trap, pinned.** With the viewport released there is
        // no installed status painter, which is the state a `classic` session
        // is in for its whole life -- and `repaint_status_row` is inert there
        // *"to keep the classic byte stream identical."* A title folded into
        // the repaint would inherit that inertness and a styled `classic`
        // session would still go stale mid-turn, which is the trap this
        // section exists to catch: the publish must still happen, and the
        // surface must still receive nothing.
        drop(guard);
        let before = err.text().len();
        sender.send((2, 0)).expect("the cadence is subscribed");
        assert!(runtime.block_on(super::idle_ambient_cadence_step(
            &mut changes,
            &counts,
            &mut seen,
            &recorder,
        )));
        assert_eq!(
            *published.lock().expect("the recorded publishes"),
            vec![(1usize, 0usize), (2usize, 0usize)],
            "with no live status row -- a `classic` session -- the title is \
             still published, because it rides beside the repaint and not \
             inside it"
        );
        assert_eq!(
            err.text().len(),
            before,
            "NON-VACUOUS: and the painter really was inert, so the publish \
             above cannot have come from it"
        );

        drop(sender);
    }
}
