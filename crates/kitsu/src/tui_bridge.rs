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
                // Tab completion at the idle prompt is deliberately out of
                // scope for the preview, and Ctrl+Z/Ctrl+\ have no outcome
                // variant yet, so both are swallowed rather than guessed at.
                EditorAction::CompleteRequest | EditorAction::Suspend | EditorAction::Quit => {}
            }
        }
        if changed {
            paint(&editor.buffer_string(), editor.cursor());
        }
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::io::Write;
    use std::sync::{Arc, Mutex};

    use kitsu_render::live;
    use kitsu_render::surface::{active_surface, set_surface, Surface};
    use serial_test::serial;

    use super::{install, mode_from_value, BridgeGuard, TuiMode};

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

    /// Test 17. A read returning zero bytes is EOF: the descriptor is closed
    /// (a piped carrier draining, or the terminal going away), so the reader
    /// must not spin on it.
    #[test]
    fn inline_read_reports_eof_when_the_reader_is_drained() {
        assert_eq!(read_bytes(b"").0, InlineRead::Eof);
        // Including mid-draft: unterminated typing then a closed descriptor.
        assert_eq!(read_bytes(b"half-typed").0, InlineRead::Eof);
    }
}
