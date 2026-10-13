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
    })
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
        assert_eq!(
            &err.text()[before..],
            "\x1b[r",
            "the guard releases the DECSTBM region on the way out"
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
}
