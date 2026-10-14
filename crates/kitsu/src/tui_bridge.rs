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

use kitsu_render::live;
use kitsu_render::surface::{set_surface, Surface};
use kitsu_tui::viewport::InlineSurface;

/// Which renderer a session runs: the classic line renderer, or the inline
/// viewport preview.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TuiMode {
    /// The renderer kitsu has always had.
    Classic,
    /// The pinned inline viewport from `kitsu-tui`.
    Inline,
}

/// Decide the mode from one environment value, failing closed.
///
/// The match is **byte-exact and case-sensitive, with no trimming**: only
/// `"inline"` selects the viewport. A typo must leave the user in the renderer
/// they already had, and a forgiving match would make the set of values that
/// silently switch renderers unbounded.
pub(crate) fn mode_from_value(value: Option<&std::ffi::OsStr>) -> TuiMode {
    match value {
        Some(value) if value.as_encoded_bytes() == b"inline" => TuiMode::Inline,
        _ => TuiMode::Classic,
    }
}

/// A session-lifetime guard that puts back everything [`install`] displaced.
///
/// Both pieces of state are process-global, so the guard carries the
/// **previous** values rather than restoring defaults: the surface slot's
/// former occupant and the live-row setting as it was found.
pub(crate) struct BridgeGuard {
    /// The viewport, kept so `Drop` can release its scroll region after the
    /// seam has been pointed back at the displaced surface.
    viewport: Arc<InlineSurface>,
    /// The surface that was in the slot before the viewport went in.
    previous_surface: Arc<dyn Surface>,
    /// The live-row setting as it was found, which may itself be `false`.
    previous_live_rows: bool,
    /// The status text [`install`] painted, so the row can fall back to it
    /// whenever there is no draft to show.
    status: String,
    /// The terminal height [`install`] was given.
    ///
    /// Finding F21: the guard carries its own geometry because
    /// `InlineSurface::hide` *takes* the latched `(bottom, viewport_rows)`.
    /// A release therefore leaves the surface with nothing to pin, so the
    /// reinstall must supply the geometry again from here — and it has to be
    /// durable rather than consumed, because a session can suspend more than
    /// once.
    rows: u16,
    /// The pinned row count [`install`] was given, kept for the same reason
    /// as [`rows`](Self::rows).
    viewport_rows: u16,
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
    viewport.paint(&[status.to_string()]);

    // Finding F15: the viewport owns the bottom rows, so the live row stands
    // down for as long as it is installed.
    let previous_live_rows = live::set_live_rows_enabled(false);
    let previous_surface = set_surface(viewport.clone());

    Some(BridgeGuard {
        viewport,
        previous_surface,
        previous_live_rows,
        status: status.to_string(),
        rows,
        viewport_rows,
    })
}

impl BridgeGuard {
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
    pub(crate) fn paint_draft(&self, prompt: &str, draft: &str) {
        let row = if draft.is_empty() {
            self.status.clone()
        } else {
            format!("{prompt}{draft}")
        };
        self.viewport.paint(&[row]);
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
        self.viewport.hide();
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
        self.viewport.show(self.rows, self.viewport_rows);
        self.viewport.paint(std::slice::from_ref(&self.status));
    }
}

impl Drop for BridgeGuard {
    /// Restore the previous surface **before** releasing the region, so the
    /// last bytes the viewport's descriptor sees are exactly the region reset
    /// and no transcript line can arrive through the seam after it.
    fn drop(&mut self) {
        set_surface(self.previous_surface.clone());
        live::set_live_rows_enabled(self.previous_live_rows);
        self.viewport.hide();
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
/// `paint` receives the current draft and the cursor's column **within** the
/// draft as a char offset. `history` seeds the up/down walk.
pub(crate) fn read_line_inline<R: std::io::Read>(
    input: &mut R,
    paint: &mut dyn FnMut(&str, usize),
    history: &[String],
) -> std::io::Result<InlineRead> {
    use kitsu_editor::{EditorAction, FeedOutcome, TypeaheadEditor};

    let mut editor = TypeaheadEditor::new(history.to_vec());
    // The first frame, before any byte arrives: the row must show the (possibly
    // empty) draft rather than whatever the previous paint left there.
    paint(&editor.buffer_string(), editor.cursor());

    let mut buffer = [0u8; 1024];
    loop {
        let count = match input.read(&mut buffer) {
            // A descriptor that is done, not an empty raw read: the terminal
            // is configured VMIN=1, so zero bytes means end of input.
            Ok(0) => return Ok(InlineRead::Eof),
            Ok(count) => count,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
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
                // Tab completion at the idle prompt is deliberately out of
                // scope for the preview. Ctrl+\ stays inert on purpose: its
                // conventional action is to re-raise SIGQUIT, which terminates
                // *and dumps core*, and this process holds API tokens and
                // conversation text in memory -- writing those to disk because
                // of a stray keystroke is worse than the key doing nothing.
                EditorAction::CompleteRequest | EditorAction::Quit => {}
            }
        }
        if changed {
            paint(&editor.buffer_string(), editor.cursor());
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

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::io::Write;
    use std::sync::{Arc, Mutex};

    use kitsu_render::live;
    use kitsu_render::surface::{active_surface, set_surface, Surface};
    use serial_test::serial;

    use super::{
        install, mode_from_value, suspend_and_resume, BridgeGuard, SuspendEffects, TuiMode,
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

    /// Test 1. The gate's truth table, including the two cases that decide how
    /// it fails: a different case and a trailing space are **Classic**, so the
    /// match is byte-exact and a typo keeps the user in the renderer they
    /// already had.
    #[test]
    fn mode_from_value_selects_inline_only_for_the_exact_value() {
        assert!(matches!(
            mode_from_value(Some(OsStr::new("inline"))),
            TuiMode::Inline
        ));

        for value in [
            "", "classic", "Inline", "INLINE", "inline ", " inline", "1", "true",
        ] {
            assert!(
                matches!(mode_from_value(Some(OsStr::new(value))), TuiMode::Classic),
                "{value:?} must fail closed to classic"
            );
        }
        assert!(matches!(mode_from_value(None), TuiMode::Classic));
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

    use super::{read_line_inline, InlineRead};

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

    /// Run the reader over an explicit chunking.
    fn drive(mut input: Chunks, history: &[String]) -> (InlineRead, Vec<(String, usize)>) {
        let mut paints: Vec<(String, usize)> = Vec::new();
        let outcome = {
            let mut paint = |draft: &str, cursor: usize| paints.push((draft.to_string(), cursor));
            read_line_inline(&mut input, &mut paint, history).expect("inline read")
        };
        (outcome, paints)
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
            let mut paint = |_draft: &str, _cursor: usize| paints += 1;
            read_line_inline(&mut input, &mut paint, &[]).expect("inline read")
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
        guard.paint_draft("> ", "half-typed");

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
    // Contrast `EditorAction::CompleteRequest` (Tab), which the reader
    // explicitly drops: that loss is real, this one is not.
    //
    // What inline mode genuinely does not get is the `⌕ reverse-search '..'`
    // indicator: the paint callback's signature is `(draft, cursor)`, so
    // `TypeaheadEditor::search_status` has nowhere to go. The search works;
    // only its status line is invisible. The last test here pins that, so a
    // later card that widens the paint seam has a failing test to update.
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
    fn inline_read_cannot_paint_the_reverse_search_status_line() {
        let history = reverse_search_history();
        let (_, paints) = read_with_history(b"\x12cargo\r\r", &history);
        assert!(
            !paints
                .iter()
                .any(|(draft, _)| draft.contains("reverse-search")),
            "the paint seam has no hint slot, so the search status cannot be \
             shown inline; if this now fails the seam was widened and the \
             documented gap should be closed: {paints:?}"
        );
    }
}
