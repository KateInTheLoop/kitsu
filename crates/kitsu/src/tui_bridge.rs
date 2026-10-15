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
    status: &str,
    out: Box<dyn std::io::Write + Send>,
    err: Box<dyn std::io::Write + Send>,
) -> Option<BridgeGuard> {
    match mode {
        // Inert: no bytes, no surface swap, and the live-row setting untouched.
        TuiMode::Classic => return None,
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

    let viewport = Arc::new(InlineSurface::new(out, err));
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

    Some(BridgeGuard {
        pane,
        previous_surface,
        previous_live_rows,
        live_status: std::cell::RefCell::new(None),
    })
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
}

impl StatusSource {
    /// A source whose tags are read through `counts` on every paint.
    pub(crate) fn new(counts: Box<dyn Fn() -> StatusCounts + Send + Sync>) -> Arc<Self> {
        Arc::new(Self {
            usage: std::sync::Mutex::new(StatusUsage::default()),
            turn_start: std::sync::Mutex::new(None),
            counts,
            painted: std::sync::Mutex::new(None),
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
        }
    }

    /// Compose the row at `cols`. Nothing is cached: the point of the type is
    /// that the row is built from the fields as they stand.
    fn compose(&self, cols: usize) -> String {
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
    let Some(pane) = lock(&MODAL_PANE).clone() else {
        return false;
    };
    let mut rows = kitsu_render::approval_modal(header, details, options);
    if let Some(cue) = cue {
        rows.push(cue.to_string());
    }
    pane.paint_rows(&rows)
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
    let row = match lock(&STATUS_PAINTER).clone() {
        Some((_, source)) => {
            source.invalidate();
            source.compose(live::terminal_columns())
        }
        None => pane.status.clone(),
    };
    pane.paint_rows(std::slice::from_ref(&row));
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
    let row = source.compose(cols);
    {
        let mut painted = lock(&source.painted);
        if painted.as_deref() == Some(row.as_str()) {
            return;
        }
        *painted = Some(row.clone());
    }
    viewport.paint(std::slice::from_ref(&row), cols);
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
    pub(crate) fn paint_composer(
        &self,
        prompt: &str,
        draft: &str,
        cursor: usize,
        hint: &str,
        hint_reserved: bool,
        rich: bool,
    ) -> bool {
        let columns = live::terminal_columns();
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
            self.status_row()
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
    /// [`release_for_suspend`]: Self::release_for_suspend
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
        // SAFETY: a single `write` of a `'static` buffer to a descriptor this
        // process owns; a short write or an error is not worth handling while
        // the process is dying.
        unsafe {
            libc::write(
                libc::STDERR_FILENO,
                SIGNAL_RESTORE.as_ptr().cast::<libc::c_void>(),
                SIGNAL_RESTORE.len(),
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
    }
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
    /// Ctrl+Z (finding F21): the draft as it stood when the byte arrived.
    ///
    /// The reader stays **pure** — it raises no signal and touches no
    /// terminal — so it reports the keystroke and hands the draft back. The
    /// caller orchestrates the suspend and then re-enters the reader with
    /// this draft as its `initial`, which is why typing survives a
    /// suspend/resume cycle.
    Suspended(String),
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
pub(crate) fn read_line_inline<R: std::io::Read>(
    input: &mut R,
    paint: &mut dyn FnMut(&str, usize, &str, bool),
    history: &[String],
    on_resize: &mut dyn FnMut(),
) -> std::io::Result<InlineRead> {
    use kitsu_editor::{EditorAction, FeedOutcome, TypeaheadEditor};

    /// The hint slot, derived exactly as `main.rs`'s `update_live_row`
    /// derives it for the classic row: the reverse-search status while Ctrl+R
    /// is active (reserved, so it keeps its columns against a long matched
    /// draft), the inline slash-completion suffix otherwise (unreserved).
    ///
    /// `search_status` already carries its own two-space separator, so the
    /// renderer must not add another one.
    fn hint_of(editor: &TypeaheadEditor) -> (String, bool) {
        let search_status = editor.search_status();
        let hint_reserved = search_status.is_some();
        let hint = search_status.unwrap_or_else(|| crate::idle_slash_hint(&editor.buffer_string()));
        (hint, hint_reserved)
    }

    let mut editor = TypeaheadEditor::new(history.to_vec());
    // The first frame, before any byte arrives: the row must show the (possibly
    // empty) draft rather than whatever the previous paint left there.
    let (mut last_hint, mut last_reserved) = hint_of(&editor);
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
                    let (hint, hint_reserved) = hint_of(&editor);
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
        // Fed verbatim: the editor owns the keymap, the escape-sequence state
        // machine and the bracketed-paste block, so the reader must not look
        // for terminators itself (that would split a paste at its newlines).
        let outcome = editor.feed(&buffer[..count]);
        let mut changed = outcome.changed;
        for action in outcome.actions {
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
        let (hint, hint_reserved) = hint_of(&editor);
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

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::sync::{Arc, Mutex};

    use kitsu_render::live;
    use kitsu_render::surface::{active_surface, set_surface, Surface};
    use serial_test::serial;

    use super::{
        install, palette_rows, suspend_and_resume, BridgeGuard, SuspendEffects, TuiMode,
        PALETTE_MIN_ROWS, SIGNAL_RESTORE,
    };

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
        let out = SharedSink::new();
        let err = SharedSink::new();
        let guard = install(
            TuiMode::Inline,
            rows,
            viewport_rows,
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
}
