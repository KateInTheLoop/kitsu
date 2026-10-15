//! The inline viewport (T0-S4): a small pinned region at the bottom of the
//! terminal holding the composer and status, with finished transcript lines
//! flowing upward out of that region.
//!
//! The shape is Codex's. Rather than taking the alternate screen and owning
//! every cell, [`InlineSurface`] sets a DECSTBM scroll region covering
//! everything *above* the viewport, parks the cursor inside it, and then writes
//! transcript lines exactly as the classic surface would. Lines scroll within
//! the region; the viewport rows below it are never touched by that scrolling
//! and are repainted by absolute positioning instead. The terminal's own
//! scrollback, search and selection therefore keep working on the transcript,
//! which is the entire reason not to use the alternate screen.
//!
//! # Finding F13: region-scrolled lines and native scrollback
//!
//! Whether a row scrolled out of the *top* of a DECSTBM region is retained in
//! the terminal's scrollback is **terminal-dependent**, and it is the design's
//! open question Q1. `vt100 0.15.2`, the emulator these tests drive, models a
//! terminal where it is **not**: `Grid::scroll_up` only pushes the removed row
//! onto its scrollback when `!scroll_region_active()`, and a pinned viewport
//! makes that predicate true by construction. The tests below therefore assert
//! the *screen* half of the mechanism and pin the scrollback limitation
//! negatively, so it is recorded in code. Confirming what real terminals do
//! belongs to T0-S6's live matrix.
//!
//! # Each backend supplies its own line ending
//!
//! `transcript_line` writes the line's bytes plus `\r\n`, **carriage return
//! included**. That is not symmetry with `ClassicSurface`, which emits a bare
//! line feed and is right to: it runs in cooked mode, where the kernel's
//! `ONLCR` adds the return. The inline viewport lives behind
//! [`TuiRuntime::enter`](crate::TuiRuntime::enter), which enables raw mode and
//! so clears `ONLCR`; there a lone LF moves down without returning to column
//! zero and every transcript line staircases to the right until it wraps. A
//! backend knows its own terminal mode, so this one emits the return itself,
//! as every terminal UI does. `vt100` models exactly the raw-mode behaviour —
//! its LF is `row_inc_scroll(1)` and nothing more — which is why the emulator
//! tests below feed plain line text and prove the contract rather than
//! compensating for it.
//!
//! `write_block` makes no line-ending guarantee at all: the block writes its
//! own bytes verbatim and owns their endings, so a block that emits lines for
//! a raw-mode terminal must put the returns in itself.

use std::io::Write;
use std::sync::Mutex;

use kitsu_render::surface::{Fd, Surface};

/// The two output descriptors plus the latched viewport geometry.
///
/// One mutex guards both sinks and the geometry: the sinks are
/// `Box<dyn Write + Send>` and so are not `Sync` on their own, and
/// [`Surface`] is `Send + Sync`. The lock is taken for the duration of a
/// write and never across a call back into user code — see
/// [`InlineSurface::write_block`].
struct Inner {
    out: Box<dyn Write + Send>,
    err: Box<dyn Write + Send>,
    /// The latched viewport geometry: `(bottom, viewport_rows)`, where
    /// `bottom` is the last row of the scrolling transcript region, 1-based,
    /// as [`InlineSurface::show`] computed it. `None` until `show` has run.
    ///
    /// One field rather than two so the pair cannot desynchronise: `hide` must
    /// clear exactly the rows `show` pinned, and a separately stored row count
    /// could outlive or contradict the bottom row it belongs to.
    geometry: Option<(u16, u16)>,
    /// The bounded cell ring, or `None` on a surface built with
    /// [`InlineSurface::new`] (requirement **N4**, plan row T3-S6).
    ///
    /// It lives behind the *same* mutex as the sinks rather than one of its
    /// own so that the retained order cannot disagree with the written order:
    /// two threads writing transcript lines serialise on this lock, and a
    /// separate ring lock would let their pushes land in the opposite order
    /// from their bytes.
    cells: Option<crate::cells::CellRing>,
}

impl Inner {
    /// The sink for `fd`.
    fn sink(&mut self, fd: Fd) -> &mut (dyn Write + Send) {
        match fd {
            Fd::Stdout => &mut *self.out,
            Fd::Stderr => &mut *self.err,
        }
    }
}

/// What a resize should do to an inline viewport (finding F16).
///
/// DECSTBM margins are **absolute row numbers**, so a latched geometry is
/// wrong the instant the terminal is resized: shrinking strands the pinned row
/// off-screen or leaves margins past the bottom of the screen, growing leaves
/// the region covering only part of the window, and either way
/// [`InlineSurface::paint`] goes on addressing rows from the stale geometry.
///
/// The decision is kept in this pure value so the policy -- in particular what
/// happens when a resize crosses finding F19's `rows >= viewport_rows + 2`
/// bound in either direction -- is testable without a terminal, a signal or a
/// blocking read.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResizePlan {
    /// Emit nothing: either the dimensions did not change, or the viewport is
    /// already released and the terminal is still too short to pin one.
    ///
    /// The first half is what keeps a spurious `SIGWINCH` from flickering the
    /// pinned row.
    Unchanged,
    /// A viewport is pinned and stays pinned at a new size: tear the old
    /// geometry down and set the new one up, so both margins and the pinned
    /// rows are right for the new size. Never patch one margin in place.
    Relatch { rows: u16, viewport_rows: u16 },
    /// A viewport is pinned and the new size is below F19's bound: release it
    /// -- region reset, rows cleared, nothing pinned -- and stay released
    /// while the terminal is too short. A pinned row that cannot be drawn
    /// correctly is worse than no pinned row, and keeping a region whose
    /// margins exceed the screen is exactly the corruption F19 was about.
    Release,
    /// No viewport is pinned and the new size clears F19's bound: install one.
    /// This is the second half of the round trip -- a terminal made too short
    /// and then tall again ends up correctly pinned.
    Install { rows: u16, viewport_rows: u16 },
}

/// Decide what a resize to `rows` x `viewport_rows` should do, given the
/// dimensions currently installed (`None` when nothing is pinned).
///
/// The bound is [`InlineSurface::show`]'s own, `rows >= viewport_rows + 2`, and
/// must stay that way: a plan that says install where `show` declines would
/// leave the caller believing a region is pinned when none is.
pub fn plan_resize(installed: Option<(u16, u16)>, rows: u16, viewport_rows: u16) -> ResizePlan {
    // A spurious notification carries no dimensions, so the redundant resize
    // is the common case and has to be the cheapest one: nothing at all.
    if installed == Some((rows, viewport_rows)) {
        return ResizePlan::Unchanged;
    }
    // `show`'s own bound, and `saturating_add` for the same reason it uses
    // one: a nonsensical `viewport_rows` near `u16::MAX` must decline rather
    // than wrap into an apparently workable geometry.
    let workable = rows >= viewport_rows.saturating_add(2);
    match (installed.is_some(), workable) {
        (true, true) => ResizePlan::Relatch {
            rows,
            viewport_rows,
        },
        (true, false) => ResizePlan::Release,
        (false, true) => ResizePlan::Install {
            rows,
            viewport_rows,
        },
        // Released and still too short: there is nothing to tear down and
        // nothing that can be pinned, so no bytes.
        (false, false) => ResizePlan::Unchanged,
    }
}

/// The usable viewport height on a `terminal_rows`-high terminal for a caller
/// that wants `requested` rows drawn (T0-S14).
///
/// F19's bound still rules -- the region needs two scrolling rows above the
/// viewport -- so the usable maximum is `terminal_rows - 2`. A request above
/// that is **clamped, not refused**: a palette too tall for the window is
/// shortened, and only a terminal that cannot pin even one row ends up with no
/// viewport at all (which is `plan_resize`'s `Release`, already built).
///
/// Pure, for the same reason [`plan_resize`] is: the property that the height
/// latched is always one [`InlineSurface::show`] would accept has to be
/// checkable without a terminal.
///
pub fn clamp_viewport_rows(terminal_rows: u16, requested: u16) -> u16 {
    // `saturating_sub` is the whole of the degenerate case: a terminal under
    // three rows has no usable height at all, and zero is the honest answer
    // rather than a geometry `show` would decline.
    requested.min(terminal_rows.saturating_sub(2))
}

/// The rows of `content` that survive the clamp on a `terminal_rows`-high
/// terminal: the **last** [`clamp_viewport_rows`]`(terminal_rows,
/// content.len())` of them, in order.
///
/// Rows are dropped from the **top**. The composer is the bottom row and the
/// palette sits above it, so the row the user is typing into must be the last
/// thing to go; losing it to make room for a list of suggestions would be the
/// wrong trade.
///
/// Returning the slice rather than a count is deliberate: its `len()` *is*
/// the height, so the two cannot disagree even in here.
pub fn fit_viewport_rows(content: &[String], terminal_rows: u16) -> &[String] {
    // `content.len()` may exceed `u16::MAX`; the clamp's own ceiling is well
    // under that, so saturating the cast cannot change the answer.
    let height = clamp_viewport_rows(
        terminal_rows,
        u16::try_from(content.len()).unwrap_or(u16::MAX),
    );
    &content[content.len() - height as usize..]
}

/// An inline viewport: a pinned region at the bottom of the terminal with the
/// transcript scrolling in a DECSTBM region above it.
///
/// See the module documentation for the mechanism and for the line-ending
/// contract `transcript_line` implements.
pub struct InlineSurface {
    inner: Mutex<Inner>,
}

impl InlineSurface {
    /// Wrap the two descriptors. Nothing is written until [`show`](Self::show).
    pub fn new(out: Box<dyn Write + Send>, err: Box<dyn Write + Send>) -> Self {
        Self {
            inner: Mutex::new(Inner {
                out,
                err,
                geometry: None,
                cells: None,
            }),
        }
    }

    /// Wrap the two descriptors **and** feed a bounded cell ring of
    /// `cell_ring_lines` lines (requirement **N4**, plan row T3-S6).
    ///
    /// Opt-in on purpose: a surface built with [`new`](Self::new) retains
    /// nothing and pays nothing, which is what every non-inline caller wants.
    /// The inline install uses this constructor, so inline mode always retains.
    pub fn with_cell_ring(
        out: Box<dyn Write + Send>,
        err: Box<dyn Write + Send>,
        cell_ring_lines: usize,
    ) -> Self {
        Self {
            inner: Mutex::new(Inner {
                out,
                err,
                geometry: None,
                // The bound is the caller's: `CellRing` defines no constants of
                // its own, so nothing here can drift from
                // `TuiConfig::{MIN,DEFAULT,MAX}_CELL_RING_LINES`.
                cells: Some(crate::cells::CellRing::new(cell_ring_lines)),
            }),
        }
    }

    /// The retained cells, oldest first, or `None` when this surface was built
    /// without a ring.
    // Reader-less for the same reason `with_cell_ring` is; see its note.
    #[allow(dead_code)]
    pub fn cells(&self) -> Option<Vec<crate::cells::CellLine>> {
        self.lock().cells.as_ref().map(|ring| ring.lines())
    }

    /// Take the lock, ignoring poisoning: a panic inside a write leaves the
    /// sinks usable, and refusing to draw afterwards would only strand the
    /// terminal in a pinned region.
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Pin a `viewport_rows`-high region at the bottom of a `rows`-high
    /// terminal: set the scroll region to rows `1..=bottom`, where
    /// `bottom = rows - viewport_rows`, and park the cursor on its last row.
    ///
    /// The region is set **once**, here: `transcript_line` must never re-emit
    /// it, because DECSTBM homes the cursor and would undo the park.
    ///
    /// # Too short to pin: write nothing, latch nothing (finding F19)
    ///
    /// A terminal needs `rows >= viewport_rows + 2` for this to mean anything,
    /// and the `+ 2` is **not** a spare row for comfort. DECSTBM requires the
    /// bottom margin to *exceed* the top, so with a top of 1 the bottom must be
    /// at least 2, i.e. `bottom = rows - viewport_rows >= 2`. At `+ 1` the
    /// region would be `ESC[1;1r` -- one row -- which real terminals ignore for
    /// exactly the reason they ignore the `ESC[1;0r` a saturating subtraction
    /// used to produce. **Do not relax this to `+ 1`:** a guard that admits a
    /// geometry the terminal discards is worse than no guard, because the code
    /// then believes a region is pinned when none is, and `paint` goes on
    /// addressing rows 1.. inside the transcript.
    ///
    /// Below the bound this writes nothing and latches no geometry. Latching
    /// nothing is the load-bearing half: it leaves [`hide`](Self::hide) and
    /// [`paint`](Self::paint) on their existing "no geometry" paths, both
    /// already inert, so neither needs a second copy of this bound.
    pub fn show(&self, rows: u16, viewport_rows: u16) {
        // `saturating_add` so a nonsensical `viewport_rows` near `u16::MAX`
        // declines rather than wrapping into an apparently workable bound.
        if rows < viewport_rows.saturating_add(2) {
            return;
        }
        let bottom = rows - viewport_rows;
        let mut inner = self.lock();
        inner.geometry = Some((bottom, viewport_rows));
        // Deliberately swallowed: there is nothing to report a failed escape
        // sequence to but the terminal that would not take it.
        let _ = write!(inner.err, "\x1b[1;{bottom}r\x1b[{bottom};1H");
        let _ = inner.err.flush();
    }

    /// Blank the pinned rows, release the scroll region, then park the cursor
    /// on the first (now blank) viewport row, immediately below the
    /// transcript.
    ///
    /// The order is load-bearing and is the whole of finding F18. The clears
    /// come first, while the geometry is still latched and the rows are still
    /// addressable. Releasing the region with `ESC[r` **homes the cursor**, so
    /// anything written afterwards — including `main`'s closing run summary,
    /// emitted once the bridge guard has dropped — would land on row 1 and
    /// overwrite the top of the transcript. The final park puts the cursor
    /// where such a write belongs, making it land correctly by construction
    /// rather than by luck of ordering; it has to follow the clears, because
    /// parking on a row that still holds painted text would merely move the
    /// same bug down the screen.
    ///
    /// With nothing shown this is wholly inert — no rows to clear, no row to
    /// park on and no region to release — so a second `hide` writes nothing.
    pub fn hide(&self) {
        let mut inner = self.lock();
        let Some((bottom, viewport_rows)) = inner.geometry.take() else {
            return;
        };
        // Deliberately swallowed, as in `show`: a failed escape sequence has
        // nowhere to be reported but the terminal that would not take it.
        for i in 0..viewport_rows {
            let row = bottom + 1 + i;
            let _ = write!(inner.err, "\x1b[{row};1H\x1b[2K");
        }
        let _ = inner.err.write_all(b"\x1b[r");
        let _ = write!(inner.err, "\x1b[{};1H", bottom + 1);
        let _ = inner.err.flush();
    }

    /// Repaint the pinned rows: for each row, position absolutely, clear the
    /// line, write the row's text; then park the cursor back inside the
    /// region.
    ///
    /// Called before [`show`](Self::show) this is a **no-op**: without a
    /// latched geometry there is no region to paint beside and no row to park
    /// on, and emitting absolute positions against a guess would scribble over
    /// the transcript. Doing nothing is the only choice that cannot corrupt
    /// the screen.
    /// # Width fitting (finding F16, truncation half)
    ///
    /// Every row passes through [`fit_row_to_width`] against `cols` before it
    /// is written, so no painted row can wrap into the row below it. With a
    /// one-row viewport an over-long row merely wrapped untidily; with the
    /// multi-row composer and the slash palette it wraps *into and corrupts*
    /// the next pinned row, which is why the fitting is here and not left to
    /// each caller.
    ///
    /// A row already within `cols` visible columns is written byte-for-byte as
    /// before -- that is `fit_row_to_width`'s first rule and what keeps the
    /// paint byte sequences above unchanged.
    ///
    /// `cols` is a **parameter** rather than part of the latched geometry:
    /// `InlineSurface` lives in `kitsu-tui` and must not probe the terminal
    /// itself (a second probe is a second source of truth), and a column count
    /// latched at `show` would be stale after a resize for exactly the reason
    /// the row geometry is. Callers pass
    /// [`kitsu_render::live::terminal_columns`], the shared probe the live row
    /// already uses.
    pub fn paint(&self, viewport: &[String], cols: usize) {
        let mut inner = self.lock();
        let Some((bottom, _)) = inner.geometry else {
            return;
        };
        for (i, row) in viewport.iter().enumerate() {
            let line = bottom as usize + 1 + i;
            let row = kitsu_render::fit_row_to_width(row, cols);
            let _ = write!(inner.err, "\x1b[{line};1H\x1b[2K{row}");
        }
        let _ = write!(inner.err, "\x1b[{bottom};1H");
        let _ = inner.err.flush();
    }
}

impl Surface for InlineSurface {
    /// `text` then CR LF on the sink for `fd`, and nothing else — no escape
    /// bytes whatever. The carriage return is this backend's own, because the
    /// viewport runs in raw mode where `ONLCR` no longer supplies it.
    fn transcript_line(&self, fd: Fd, text: &str) {
        let mut inner = self.lock();
        let sink = inner.sink(fd);
        let _ = sink.write_all(text.as_bytes());
        let _ = sink.write_all(b"\r\n");
        let _ = sink.flush();
        // After the write, inside the lock already held for it: the lock is not
        // widened by one byte, the bytes reach the terminal before any
        // bookkeeping, and the ring's order is the written order by
        // construction. The ring is fed the line's bytes verbatim -- the CRLF
        // is this backend's line ending, not part of the line.
        if let Some(ring) = inner.cells.as_mut() {
            ring.push(fd, text);
        }
    }

    /// Hand the block a writer and forward its bytes verbatim to the sink for
    /// `fd`, line endings included: the block owns its own endings.
    ///
    /// The block is run against a scratch buffer with **no lock held**, and
    /// only then is the lock taken to copy the bytes out. A block may call
    /// back into this surface, and holding the mutex across it would
    /// self-deadlock — the same reason `active_surface` clones its `Arc` out
    /// of a short guard.
    fn write_block(&self, fd: Fd, block: &mut dyn FnMut(&mut dyn std::io::Write)) {
        let mut bytes: Vec<u8> = Vec::new();
        block(&mut bytes);
        let mut inner = self.lock();
        let sink = inner.sink(fd);
        let _ = sink.write_all(&bytes);
        let _ = sink.flush();
        // After the write and flush, inside the lock already held for the copy:
        // a second acquisition would let two blocks land in the ring in the
        // opposite order from their bytes, and the ring is N4's sole record.
        // The block itself still ran with no lock held, above. The trait
        // default is deliberately not reused: forwarding to `transcript_line`
        // would add a CRLF per line and break the "the block owns its endings"
        // contract, which is why this override exists. The ring stores line
        // text without endings, so the scratch buffer is split here and only
        // the texts are pushed.
        //
        // A block whose bytes end without a newline is retained as a short
        // line rather than held back until some later block terminates it:
        // holding it would mean text that is on the terminal is absent from
        // the ring, and a partial final line that is never terminated (the
        // shape behind F47) would be lost from the overlay and the `[` dump
        // for good. Retaining it short can split one logical line across two
        // ring entries, which is visible and recoverable; losing it is not.
        if let Some(ring) = inner.cells.as_mut() {
            let text = String::from_utf8_lossy(&bytes);
            for line in text.lines() {
                ring.push(fd, line);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use kitsu_render::surface::{Fd, Surface};
    use std::io::Write;
    use std::sync::{Arc, Mutex};

    use super::InlineSurface;

    /// A sink whose buffer outlives the surface holding it, so bytes written
    /// through a `Box<dyn Write + Send>` are still observable afterwards.
    ///
    /// `Surface` is `Send + Sync`, so this is `Arc<Mutex<..>>` rather than the
    /// `Rc<RefCell<..>>` the `TuiRuntime` tests use.
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
            String::from_utf8(self.bytes()).expect("sink holds UTF-8")
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

    /// A surface with the two descriptors captured independently, which is the
    /// point of the per-descriptor design.
    fn split() -> (InlineSurface, SharedSink, SharedSink) {
        let out = SharedSink::new();
        let err = SharedSink::new();
        let surface = InlineSurface::new(Box::new(out.clone()), Box::new(err.clone()));
        (surface, out, err)
    }

    /// A surface whose two descriptors land on **one** stream, as they do on a
    /// real terminal, so call order is observable.
    fn shared() -> (InlineSurface, SharedSink) {
        let sink = SharedSink::new();
        let surface = InlineSurface::new(Box::new(sink.clone()), Box::new(sink.clone()));
        (surface, sink)
    }

    fn count(haystack: &[u8], needle: &[u8]) -> usize {
        if needle.is_empty() || haystack.len() < needle.len() {
            return 0;
        }
        haystack
            .windows(needle.len())
            .filter(|w| *w == needle)
            .count()
    }

    #[test]
    fn show_sets_the_region_above_the_viewport_then_parks_the_cursor() {
        let (surface, sink) = shared();
        // 24 rows with a 3-row viewport: the region is rows 1..=21 and the
        // cursor parks on 21, the region's own last row.
        surface.show(24, 3);
        assert_eq!(sink.bytes(), b"\x1b[1;21r\x1b[21;1H".to_vec());
    }

    #[test]
    fn show_writes_the_viewport_control_bytes_to_the_stderr_sink() {
        // The live status row kitsu-render paints is on stderr, so the
        // viewport's own control bytes belong on the same descriptor; nothing
        // of the viewport may leak into a redirected stdout.
        let (surface, out, err) = split();
        surface.show(24, 3);
        assert_eq!(err.bytes(), b"\x1b[1;21r\x1b[21;1H".to_vec());
        assert!(
            out.bytes().is_empty(),
            "stdout carries no viewport control bytes"
        );
    }

    /// The byte sequence `hide` owes a `rows`x`viewport_rows` geometry, built
    /// from the geometry rather than written out, so the test states the
    /// contract instead of a transcript of one terminal size.
    ///
    /// Order matters and is the whole of finding F18: every viewport row is
    /// positioned and cleared *first*, while the geometry is still latched and
    /// the cursor still addressable; then the region is released; then the
    /// cursor is parked on the first (now blank) viewport row, immediately
    /// below the transcript. `ESC[r` homes the cursor, so anything written
    /// afterwards — including `main`'s closing summary, printed after the
    /// bridge guard has dropped — would otherwise land on row 1 and overwrite
    /// the top of the transcript. The park is what makes a later direct write
    /// land in the right place by construction rather than by luck of ordering.
    fn expected_hide(rows: u16, viewport_rows: u16) -> Vec<u8> {
        let bottom = rows.saturating_sub(viewport_rows);
        let mut expected = Vec::new();
        for i in 0..viewport_rows {
            let row = bottom + 1 + i;
            expected.extend_from_slice(format!("\x1b[{row};1H\x1b[2K").as_bytes());
        }
        expected.extend_from_slice(b"\x1b[r");
        expected.extend_from_slice(format!("\x1b[{};1H", bottom + 1).as_bytes());
        expected
    }

    #[test]
    fn hide_clears_the_viewport_row_releases_the_region_then_parks_below_the_transcript() {
        let (surface, sink) = shared();
        // 24 rows with a 1-row viewport: bottom = 23, the viewport is row 24,
        // so exactly ESC[24;1H ESC[2K ESC[r ESC[24;1H.
        surface.show(24, 1);
        let before = sink.bytes().len();
        surface.hide();
        let tail = sink.bytes()[before..].to_vec();
        assert_eq!(
            tail,
            expected_hide(24, 1),
            "hide tail was {:?}",
            String::from_utf8_lossy(&tail)
        );
        assert_eq!(
            tail,
            b"\x1b[24;1H\x1b[2K\x1b[r\x1b[24;1H".to_vec(),
            "and the derived sequence is the literal one the card specifies"
        );
    }

    #[test]
    fn hide_clears_every_viewport_row_in_ascending_order_before_the_release() {
        let (surface, sink) = shared();
        // 10 rows, 3 viewport rows: bottom = 7, the viewport is rows 8, 9 and
        // 10. All three are cleared, so the loop is pinned rather than a single
        // hardcoded row.
        surface.show(10, 3);
        let before = sink.bytes().len();
        surface.hide();
        let tail = sink.bytes()[before..].to_vec();
        assert_eq!(
            tail,
            expected_hide(10, 3),
            "hide tail was {:?}",
            String::from_utf8_lossy(&tail)
        );

        // Spelled out independently of the helper: the clears precede the
        // release, and they ascend.
        let text = String::from_utf8(tail).expect("hide emits UTF-8");
        let release = text.find("\x1b[r").expect("the region is released");
        let clears = &text[..release];
        let mut at = 0usize;
        for row in [8, 9, 10] {
            let needle = format!("\x1b[{row};1H\x1b[2K");
            let found = clears
                .get(at..)
                .and_then(|rest| rest.find(&needle))
                .unwrap_or_else(|| {
                    panic!("row {row} is positioned and cleared before the release: {clears:?}")
                });
            at += found + needle.len();
        }
        assert_eq!(
            &text[release..],
            "\x1b[r\x1b[8;1H",
            "nothing but the release and the park follows the clears"
        );
    }

    #[test]
    fn hide_with_no_viewport_shown_writes_nothing() {
        // Without a latched geometry there are no rows to clear and no row to
        // park on, and there is no region to release either: `hide` is inert.
        let (surface, sink) = shared();
        surface.hide();
        assert!(
            sink.bytes().is_empty(),
            "hide before show emits no bytes at all, got {:?}",
            sink.text()
        );
    }

    #[test]
    fn hide_is_idempotent_and_emits_the_sequence_once() {
        // This documents rather than changes behaviour: `hide` clears the
        // latched geometry, and the second call therefore takes the same inert
        // path as `hide` before `show`. Pinned because the clearing loop makes
        // a double teardown newly destructive — a second pass would blank rows
        // whose geometry no longer holds.
        let (surface, sink) = shared();
        surface.show(24, 1);
        let before = sink.bytes().len();
        surface.hide();
        let once = sink.bytes()[before..].to_vec();
        surface.hide();
        assert_eq!(
            sink.bytes()[before..].to_vec(),
            once,
            "the second hide adds nothing"
        );
        assert_eq!(once, expected_hide(24, 1));
    }

    #[test]
    fn hide_writes_the_teardown_bytes_to_the_stderr_sink_only() {
        // Same descriptor discipline as `show`: nothing of the viewport, its
        // teardown included, may leak into a redirected stdout.
        let (surface, out, err) = split();
        surface.show(24, 1);
        let before = err.bytes().len();
        surface.hide();
        assert_eq!(&err.bytes()[before..], expected_hide(24, 1).as_slice());
        assert!(
            out.bytes().is_empty(),
            "stdout carries no viewport teardown bytes"
        );
    }

    #[test]
    fn hide_latches_the_viewport_row_count_so_it_can_clear_what_show_pinned() {
        // `hide` cannot clear rows it does not know about, so `Inner` must
        // remember the row count alongside the bottom row. One field holding
        // both keeps them from desynchronising.
        let surface = InlineSurface::new(Box::new(SharedSink::new()), Box::new(SharedSink::new()));
        assert_eq!(surface.lock().geometry, None, "nothing latched before show");
        surface.show(10, 3);
        assert_eq!(
            surface.lock().geometry,
            Some((7, 3)),
            "show latches (bottom, viewport_rows)"
        );
        surface.hide();
        assert_eq!(surface.lock().geometry, None, "hide unlatches the geometry");
    }

    #[test]
    fn show_and_paint_byte_sequences_are_unchanged_by_the_hide_fix() {
        // A guard for this card specifically: only `hide` changes. `show` still
        // sets the region and parks on its last row, and `paint` still
        // positions, clears and writes each viewport row before re-parking
        // inside the region.
        let (surface, sink) = shared();
        surface.show(24, 3);
        assert_eq!(sink.bytes(), b"\x1b[1;21r\x1b[21;1H".to_vec());
        let before = sink.bytes().len();
        surface.paint(&["composer".to_string()], 80);
        assert_eq!(
            &sink.bytes()[before..],
            b"\x1b[22;1H\x1b[2Kcomposer\x1b[21;1H"
        );
        let before = sink.bytes().len();
        surface.transcript_line(Fd::Stdout, "line");
        assert_eq!(&sink.bytes()[before..], b"line\r\n");
    }

    #[test]
    fn paint_positions_clears_and_writes_each_row_then_reparks() {
        let (surface, sink) = shared();
        // 10 rows, 2 viewport rows: bottom = 8, viewport occupies rows 9 and
        // 10, and the cursor goes back to row 8 inside the region.
        surface.show(10, 2);
        let before = sink.bytes().len();
        surface.paint(&["composer".to_string(), "status".to_string()], 80);
        assert_eq!(
            &sink.bytes()[before..],
            b"\x1b[9;1H\x1b[2Kcomposer\x1b[10;1H\x1b[2Kstatus\x1b[8;1H"
        );
    }

    #[test]
    fn transcript_line_writes_text_and_crlf_on_the_matching_sink_only() {
        // CR **and** LF: the viewport runs in raw mode, where the kernel no
        // longer translates. See the module docs.
        let (surface, out, err) = split();
        surface.transcript_line(Fd::Stdout, "assistant says hello");
        assert_eq!(out.text(), "assistant says hello\r\n");
        assert!(err.bytes().is_empty(), "stderr untouched by a stdout line");

        let (surface, out, err) = split();
        surface.transcript_line(Fd::Stderr, "tool: read_file");
        assert_eq!(err.text(), "tool: read_file\r\n");
        assert!(out.bytes().is_empty(), "stdout untouched by a stderr line");
    }

    #[test]
    fn the_region_is_set_once_by_show_and_never_per_transcript_line() {
        // Re-setting DECSTBM per line would move the cursor to the region's
        // top row every time and defeat the whole mechanism, so this is pinned
        // rather than left to inspection.
        let (surface, sink) = shared();
        surface.show(24, 3);
        let after_show = sink.bytes();
        assert_eq!(count(&after_show, b"r"), 1, "exactly the one region set");
        for i in 0..5 {
            surface.transcript_line(Fd::Stdout, &format!("line {i}"));
            surface.transcript_line(Fd::Stderr, &format!("meta {i}"));
        }
        let tail = sink.bytes()[after_show.len()..].to_vec();
        assert_eq!(
            count(&tail, b"\x1b["),
            0,
            "transcript lines emit no control sequences at all: {:?}",
            String::from_utf8_lossy(&tail)
        );
    }

    #[test]
    fn write_block_hands_the_block_the_matching_sink() {
        let (surface, out, err) = split();
        surface.write_block(Fd::Stdout, &mut |w: &mut dyn std::io::Write| {
            write!(w, "first\nsecond").expect("block write");
        });
        assert_eq!(out.text(), "first\nsecond");
        assert!(err.bytes().is_empty(), "stderr untouched by a stdout block");

        let (surface, out, err) = split();
        surface.write_block(Fd::Stderr, &mut |w: &mut dyn std::io::Write| {
            write!(w, "diff hunk").expect("block write");
        });
        assert_eq!(err.text(), "diff hunk");
        assert!(out.bytes().is_empty(), "stdout untouched by a stderr block");
    }

    /// Drive one emulator with both descriptors in call order and feed it a
    /// full viewport session: show, enough transcript to overflow the region,
    /// then a paint.
    ///
    /// The line texts are plain, with no carriage return of their own: the
    /// `\r\n` that keeps them from staircasing comes from the contract, which
    /// is what makes the screen assertions a proof of it.
    fn session_bytes(rows: u16, viewport_rows: u16, lines: u16) -> Vec<u8> {
        let (surface, sink) = shared();
        surface.show(rows, viewport_rows);
        for i in 1..=lines {
            let fd = if i % 2 == 1 { Fd::Stdout } else { Fd::Stderr };
            surface.transcript_line(fd, &format!("line {i:02}"));
        }
        surface.paint(
            &[
                "> composer".to_string(),
                "status: 2 agents".to_string(),
                "hint: ctrl-c".to_string(),
            ],
            80,
        );
        sink.bytes()
    }

    #[test]
    fn the_screen_keeps_the_viewport_and_scrolls_only_the_region() {
        let mut parser = vt100::Parser::new(24, 80, 0);
        parser.process(&session_bytes(24, 3, 40));
        let screen: Vec<String> = parser.screen().rows(0, 80).collect();
        assert_eq!(screen.len(), 24);

        // The viewport survived 40 lines of scrolling and holds exactly what
        // `paint` put there.
        assert_eq!(
            &screen[21..24],
            &[
                "> composer".to_string(),
                "status: 2 agents".to_string(),
                "hint: ctrl-c".to_string(),
            ]
        );

        // The 21-row region shows the *last* 21 lines, newest on its bottom
        // row: the content scrolled inside the region rather than over the
        // viewport.
        assert_eq!(
            screen[19], "line 40",
            "newest line, just above the parked cursor"
        );
        assert_eq!(
            screen[0], "line 21",
            "oldest surviving line at the region's top"
        );
        assert_eq!(
            screen[20], "",
            "the region's last row holds the parked cursor and is blank, the \
             final line's own newline having scrolled it up"
        );
        let region = screen[0..21].join("\n");
        assert!(
            !region.contains("line 01"),
            "the earliest lines scrolled out of the region: {region:?}"
        );
    }

    #[test]
    fn vt100_discards_region_scrolled_rows_instead_of_keeping_scrollback() {
        // Finding F13. `vt100 0.15.2`'s `Grid::scroll_up` pushes a removed row
        // onto its scrollback only when `!scroll_region_active()`, and a
        // pinned viewport makes a region active by definition, so rows that
        // scroll off the top of the region are simply dropped. This is a
        // property of the emulator's model of a terminal, not of this code:
        // whether a *real* terminal retains them is the design's open question
        // Q1 and is answered by T0-S6's live terminal matrix, not here. The
        // test is negative on purpose, so nobody rediscovers the limitation by
        // writing the impossible positive assertion.
        let mut with_region = vt100::Parser::new(24, 80, 100);
        with_region.process(&session_bytes(24, 3, 40));
        with_region.set_scrollback(10);
        assert_eq!(
            with_region.screen().scrollback(),
            0,
            "no scrollback accumulated under an active region, so the request clamps to 0"
        );

        // Contrast: the identical lines with no region set do accumulate, which
        // shows the assertion above is about the region and not the harness.
        let (surface, sink) = shared();
        for i in 1..=40 {
            surface.transcript_line(Fd::Stdout, &format!("line {i:02}"));
        }
        let mut no_region = vt100::Parser::new(24, 80, 100);
        no_region.process(&sink.bytes());
        no_region.set_scrollback(10);
        assert_eq!(
            no_region.screen().scrollback(),
            10,
            "without a region the same lines reach the emulator's scrollback"
        );
    }

    // ---------------------------------------------------------------------
    // Finding F19: a terminal too short for a transcript row plus the viewport.
    //
    // `show` used to compute `bottom = rows.saturating_sub(viewport_rows)`,
    // which saturates to 0 on a short terminal and emits `ESC[1;0r` -- an
    // invalid DECSTBM, because the bottom margin must exceed the top. Terminals
    // disagree about what to do with it and some ignore it outright, leaving no
    // region at all while this code believes one is installed; the same clamp
    // then paints the viewport at rows `1..=viewport_rows`, on top of the
    // region's own first rows instead of below them.
    //
    // DECSTBM needs the bottom margin to *exceed* the top, so with a top of 1
    // the bottom must be at least 2: a one-row region `ESC[1;1r` is ignored by
    // VT-series terminals for exactly the reason `ESC[1;0r` is. The bound is
    // therefore `rows >= viewport_rows + 2`, two scrolling transcript rows
    // above the viewport.
    //
    // The pinned decision: when the terminal cannot fit a usable scrolling
    // region *plus* the viewport -- that is, when
    // `rows < viewport_rows + 2` -- no viewport is installed and the session
    // keeps the classic renderer, which needs no scroll region and degrades on
    // any geometry. `show` is made defensive about the same bound here because
    // it is the code emitting the bytes: it writes nothing, latches nothing,
    // and so leaves `paint` and `hide` inert rather than addressing rows that
    // were never pinned.
    // ---------------------------------------------------------------------

    /// Every `(rows, viewport_rows)` pair below is too short for a usable
    /// scrolling region plus the viewport, `rows == 0` among them (a failed
    /// `TIOCGWINSZ` probe can plausibly report it). `(2, 1)` and `(4, 3)` are
    /// the cases that would leave `bottom == 1`, a single-row region real
    /// terminals ignore.
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

    #[test]
    fn show_writes_nothing_and_latches_nothing_on_a_geometry_too_short_for_a_transcript_row() {
        for (rows, viewport_rows) in TOO_SHORT {
            let (surface, sink) = shared();
            surface.show(rows, viewport_rows);
            assert!(
                sink.bytes().is_empty(),
                "show({rows}, {viewport_rows}) must emit no bytes at all, got {:?}",
                sink.text()
            );
            assert_eq!(
                surface.lock().geometry,
                None,
                "show({rows}, {viewport_rows}) must latch no geometry"
            );
        }
    }

    #[test]
    fn show_succeeds_at_the_smallest_workable_geometry() {
        // rows == viewport_rows + 2 is the pinned minimum: two scrolling
        // transcript rows, then the viewport. With rows = 3 and a 1-row
        // viewport the region is rows 1..=2 -- a bottom margin that genuinely
        // exceeds the top, so the terminal installs it -- the cursor parks on
        // row 2, and the viewport is row 3, below the region rather than over
        // it.
        let (surface, sink) = shared();
        surface.show(3, 1);
        assert_eq!(sink.bytes(), b"\x1b[1;2r\x1b[2;1H".to_vec());
        assert_eq!(surface.lock().geometry, Some((2, 1)));

        // And the same bound with a taller viewport.
        let (surface, sink) = shared();
        surface.show(5, 3);
        assert_eq!(sink.bytes(), b"\x1b[1;2r\x1b[2;1H".to_vec());
        assert_eq!(surface.lock().geometry, Some((2, 3)));
    }

    #[test]
    fn show_never_emits_a_region_whose_bottom_is_above_its_top() {
        // The property, driven over every small geometry rather than the
        // boundary cases alone: either nothing is emitted, or the emitted
        // DECSTBM has a bottom margin that genuinely *exceeds* its top margin
        // of 1, sits on screen, leaves room for the viewport below it, and has
        // the parked cursor inside it. `ESC[1;0r` and `ESC[1;1r` both fail that
        // and are the byte sequences finding F19 is about: a terminal ignores
        // either, so the code believes a region is pinned when none is.
        for rows in 0u16..=40 {
            for viewport_rows in 0u16..=8 {
                let (surface, sink) = shared();
                surface.show(rows, viewport_rows);
                let text = sink.text();
                if text.is_empty() {
                    assert!(
                        rows < viewport_rows + 2,
                        "show({rows}, {viewport_rows}) declined a workable geometry"
                    );
                    continue;
                }
                let bottom = text
                    .strip_prefix("\x1b[1;")
                    .and_then(|rest| rest.split_once('r'))
                    .map(|(bottom, _)| bottom.to_string())
                    .unwrap_or_else(|| panic!("show({rows}, {viewport_rows}) emitted {text:?}"));
                let bottom: u16 = bottom
                    .parse()
                    .unwrap_or_else(|_| panic!("non-numeric bottom margin in {text:?}"));
                assert!(
                    bottom >= 2,
                    "show({rows}, {viewport_rows}) emitted a region with bottom margin \
                     {bottom}: DECSTBM requires the bottom margin to exceed the top, so a \
                     bottom of 1 is a one-row region and a bottom of 0 is backwards -- real \
                     terminals ignore both and pin no region at all: {text:?}"
                );
                assert!(
                    bottom <= rows,
                    "show({rows}, {viewport_rows}) put the region's bottom off-screen: {text:?}"
                );
                assert!(
                    bottom + viewport_rows <= rows,
                    "show({rows}, {viewport_rows}) overlaps the viewport with the region: {text:?}"
                );
                assert!(
                    text.ends_with(&format!("\x1b[{bottom};1H")),
                    "the cursor must park on the region's last row: {text:?}"
                );
            }
        }
    }

    #[test]
    fn hide_stays_consistent_with_a_declined_show() {
        // Whatever `show` does on a degenerate geometry, `hide` must agree with
        // it: a declined show pinned no rows, so teardown has nothing to clear,
        // no region to release and no row to park on. Positioning anything here
        // would scribble on a screen the viewport never touched.
        for (rows, viewport_rows) in TOO_SHORT {
            let (surface, sink) = shared();
            surface.show(rows, viewport_rows);
            surface.hide();
            assert!(
                sink.bytes().is_empty(),
                "show({rows}, {viewport_rows}) then hide must emit nothing, got {:?}",
                sink.text()
            );
        }
    }

    #[test]
    fn paint_stays_consistent_with_a_declined_show() {
        // Same argument for `paint`: without a latched geometry it is already a
        // no-op, and a declined show must leave it one rather than letting the
        // old saturating clamp address rows 1.. inside the transcript.
        for (rows, viewport_rows) in TOO_SHORT {
            let (surface, sink) = shared();
            surface.show(rows, viewport_rows);
            surface.paint(&["> composer".to_string()], 80);
            assert!(
                sink.bytes().is_empty(),
                "show({rows}, {viewport_rows}) then paint must emit nothing, got {:?}",
                sink.text()
            );
        }
    }

    // ---------------------------------------------------------------------
    // Finding F16: the viewport has no resize handling.
    //
    // Geometry is latched once at install and never updated, and DECSTBM
    // margins are absolute row numbers. `plan_resize` is the pure half of the
    // fix: it takes the dimensions currently installed plus the new ones and
    // says what should happen, so the policy is pinned here rather than in a
    // signal handler nobody can drive from a unit test.
    //
    // The decided policy, which these tests pin:
    //
    //   * same dimensions            -> nothing at all (a spurious SIGWINCH
    //                                   must not flicker the pinned row);
    //   * new size, bound cleared    -> release the old geometry and set up
    //                                   the new one, never patch a margin;
    //   * resize below F19's bound   -> release and stay released;
    //   * released, still too short  -> nothing;
    //   * released, bound cleared    -> install.
    // ---------------------------------------------------------------------

    use super::{plan_resize, ResizePlan};

    #[test]
    fn plan_resize_does_nothing_when_the_dimensions_are_unchanged() {
        // The spurious-SIGWINCH case. A resize notification carries no
        // dimensions, so a redundant one is the common case, not the odd one:
        // re-latching here would tear the region down and set it back up on
        // every window focus change that happens to raise the signal, which
        // the user sees as the pinned row flickering.
        assert_eq!(
            plan_resize(Some((24, 1)), 24, 1),
            ResizePlan::Unchanged,
            "a resize to the same dimensions must emit nothing"
        );
        assert_eq!(
            plan_resize(Some((40, 3)), 40, 3),
            ResizePlan::Unchanged,
            "and the same with a taller viewport"
        );
    }

    #[test]
    fn plan_resize_relatches_when_the_terminal_changes_size_above_the_bound() {
        // Both directions. Shrinking is the dangerous one -- the old bottom
        // margin may now be past the bottom of the screen -- but growing is
        // just as wrong, leaving the transcript scrolling in a subset of the
        // window with dead space below it.
        assert_eq!(
            plan_resize(Some((24, 1)), 40, 1),
            ResizePlan::Relatch {
                rows: 40,
                viewport_rows: 1
            },
            "growing must re-latch so the region covers the whole new screen"
        );
        assert_eq!(
            plan_resize(Some((40, 1)), 24, 1),
            ResizePlan::Relatch {
                rows: 24,
                viewport_rows: 1
            },
            "shrinking must re-latch so the margins stay on screen"
        );
        // A changed viewport row count is a resize too: this is the seam the
        // multi-row composer and the slash palette will drive.
        assert_eq!(
            plan_resize(Some((24, 1)), 24, 3),
            ResizePlan::Relatch {
                rows: 24,
                viewport_rows: 3
            },
            "a changed pinned row count re-latches at the same height"
        );
    }

    #[test]
    fn plan_resize_releases_the_viewport_when_a_resize_crosses_below_the_bound() {
        // The decision this card makes: a pinned row that cannot be drawn
        // correctly is worse than no pinned row, so the viewport releases
        // rather than keeping a region whose margins no longer fit.
        for (rows, viewport_rows) in TOO_SHORT {
            assert_eq!(
                plan_resize(Some((24, 1)), rows, viewport_rows),
                ResizePlan::Release,
                "a resize to ({rows}, {viewport_rows}) is below F19's bound, so the \
                 viewport must release rather than keep a broken region"
            );
        }
    }

    #[test]
    fn plan_resize_stays_released_while_the_terminal_is_too_short() {
        // Released and still too short: nothing to tear down and nothing that
        // can be pinned, so no bytes. Without this, every SIGWINCH while a
        // window is dragged through a too-short size would emit a teardown for
        // a geometry that was never latched.
        for (rows, viewport_rows) in TOO_SHORT {
            assert_eq!(
                plan_resize(None, rows, viewport_rows),
                ResizePlan::Unchanged,
                "released and still too short at ({rows}, {viewport_rows}): no bytes"
            );
        }
    }

    #[test]
    fn plan_resize_reinstalls_once_a_later_resize_clears_the_bound() {
        // The other half of the round trip. A terminal dragged too short and
        // then tall again must end up with a correctly pinned viewport, not
        // permanently without one.
        assert_eq!(
            plan_resize(None, 24, 1),
            ResizePlan::Install {
                rows: 24,
                viewport_rows: 1
            },
            "a released viewport re-installs as soon as the terminal is tall enough"
        );
        assert_eq!(
            plan_resize(None, 3, 1),
            ResizePlan::Install {
                rows: 3,
                viewport_rows: 1
            },
            "including at the smallest workable geometry, rows == viewport_rows + 2"
        );
    }

    #[test]
    fn plan_resize_pins_a_geometry_exactly_when_show_would_emit_one() {
        // The property, driven over every small geometry: the plan may only
        // ask for a pinned viewport where `show` actually emits a region. A
        // plan that said install where `show` declines would leave the caller
        // believing a region is pinned when none is -- finding F19's defect
        // moved one layer up.
        for rows in 0u16..=40 {
            for viewport_rows in 0u16..=8 {
                let workable = rows >= viewport_rows + 2;
                for installed in [None, Some((24u16, 1u16)), Some((40u16, 3u16))] {
                    let plan = plan_resize(installed, rows, viewport_rows);
                    let pins = match plan {
                        ResizePlan::Relatch { .. } | ResizePlan::Install { .. } => true,
                        ResizePlan::Release | ResizePlan::Unchanged => false,
                    };
                    if installed == Some((rows, viewport_rows)) {
                        assert_eq!(
                            plan,
                            ResizePlan::Unchanged,
                            "plan_resize({installed:?}, {rows}, {viewport_rows}) must be inert"
                        );
                        continue;
                    }
                    assert_eq!(
                        pins,
                        workable,
                        "plan_resize({installed:?}, {rows}, {viewport_rows}) returned {plan:?}, \
                         but show() {} emit a region for that geometry",
                        if workable { "would" } else { "would not" }
                    );
                    // And a plan that pins must carry the *new* dimensions:
                    // carrying the old ones is the stale-geometry bug itself.
                    if let ResizePlan::Relatch {
                        rows: r,
                        viewport_rows: v,
                    }
                    | ResizePlan::Install {
                        rows: r,
                        viewport_rows: v,
                    } = plan
                    {
                        assert_eq!(
                            (r, v),
                            (rows, viewport_rows),
                            "the plan must carry the new dimensions, not the latched ones"
                        );
                    }
                    // Release only ever applies to a viewport there is
                    // something to release.
                    if plan == ResizePlan::Release {
                        assert!(
                            installed.is_some(),
                            "plan_resize({installed:?}, {rows}, {viewport_rows}) asked to \
                             release a viewport that was never pinned"
                        );
                    }
                }
            }
        }
    }

    // ---------------------------------------------------------------------
    // Finding F16, truncation half: `paint` fits each row to the terminal.
    //
    // `paint` used to write every row with no width limit, so a row wider
    // than the terminal wrapped into the next viewport row. With a one-row
    // viewport that was untidy; with the multi-row composer and the slash
    // palette it overwrites the row below. The fitting itself is pinned in
    // `kitsu_render::fit_row_to_width`; these tests pin the wiring, and the
    // existing byte-exact paint tests above are the regression net for rows
    // that already fit.
    // ---------------------------------------------------------------------

    /// Visible width of a styled row, the in-tree idiom: sanitise the escapes
    /// away, then measure.
    fn visible_cols(row: &str) -> usize {
        kitsu_render::sanitize_terminal_text(row).chars().count()
    }

    #[test]
    fn paint_leaves_a_row_within_the_terminal_width_byte_identical() {
        // The property the fitting may not cost us: a row that fits produces
        // exactly the bytes it produced before this card, the trailing park
        // included and with nothing appended to the row itself -- even when it
        // leaves styling active.
        let (surface, sink) = shared();
        surface.show(10, 2);
        let before = sink.bytes().len();
        surface.paint(&["composer".to_string(), "\x1b[31mstatus".to_string()], 80);
        assert_eq!(
            &sink.bytes()[before..],
            b"\x1b[9;1H\x1b[2Kcomposer\x1b[10;1H\x1b[2K\x1b[31mstatus\x1b[8;1H",
            "a fitting row is written byte-for-byte, with no reset appended"
        );
    }

    #[test]
    fn paint_truncates_an_over_long_row_so_it_cannot_wrap() {
        let (surface, sink) = shared();
        surface.show(10, 2);
        let before = sink.bytes().len();
        // 30 visible columns into a 12-column terminal.
        surface.paint(&["0123456789abcdefghijklmnopqrst".to_string()], 12);
        let tail = String::from_utf8(sink.bytes()[before..].to_vec()).expect("UTF-8");
        assert_eq!(
            tail, "\x1b[9;1H\x1b[2K0123456789ab\x1b[8;1H",
            "the row is cut to the terminal's width, got {tail:?}"
        );

        // And the property rather than the literal: the text written between
        // the clear and the park is at most `cols` visible columns, for every
        // width.
        for cols in 1usize..=40 {
            let (surface, sink) = shared();
            surface.show(10, 2);
            let before = sink.bytes().len();
            surface.paint(&["0123456789abcdefghijklmnopqrst".to_string()], cols);
            let tail = String::from_utf8(sink.bytes()[before..].to_vec()).expect("UTF-8");
            let body = tail
                .strip_prefix("\x1b[9;1H\x1b[2K")
                .and_then(|rest| rest.strip_suffix("\x1b[8;1H"))
                .unwrap_or_else(|| panic!("paint at {cols} cols emitted {tail:?}"));
            assert!(
                visible_cols(body) <= cols,
                "paint at {cols} cols wrote {} visible columns: {body:?}",
                visible_cols(body)
            );
        }
    }

    #[test]
    fn paint_keeps_an_over_long_row_from_disturbing_another_viewport_row() {
        // The case this card exists for, driven through the emulator because
        // wrapping is a property of the terminal and not of the byte stream.
        //
        // The row painted *after* an over-long one happens to repair the
        // damage, since it clears its own line; so the corruption shows where
        // a real multi-row viewport will show it -- a partial repaint. Here
        // the status row is painted once and the composer row alone is
        // repainted, exactly as a keystroke redraw would.
        let (surface, sink) = shared();
        surface.show(6, 2);
        surface.paint(&["> ".to_string(), "status: ok".to_string()], 12);
        surface.paint(&["0123456789abcdefghijklmnopqrst".to_string()], 12);
        let mut parser = vt100::Parser::new(6, 12, 0);
        parser.process(&sink.bytes());
        let screen: Vec<String> = parser.screen().rows(0, 12).collect();
        assert_eq!(
            screen[4], "0123456789ab",
            "the composer row holds exactly the terminal's width"
        );
        assert_eq!(
            screen[5], "status: ok",
            "and the status row below it survives the repaint rather than \
             being overwritten by the composer's overflow"
        );
    }

    #[test]
    fn paint_keeps_an_over_long_last_row_from_scrolling_the_whole_screen() {
        // The worse half of the same defect: the *last* pinned row sits on the
        // last row of the screen, so its overflow autowraps off the bottom and
        // scrolls everything -- the transcript above and the viewport itself
        // move up a row, which is precisely the corruption the viewport exists
        // to avoid.
        let (surface, sink) = shared();
        surface.show(6, 2);
        // Deliberately within the 12-column width: a longer line autowraps
        // and scrolls the region by itself, which would confound the very
        // thing asserted below. The transcript's own clipping is the
        // terminal's business; this test is about the viewport's last row.
        surface.transcript_line(Fd::Stdout, "transcript");
        surface.paint(
            &[
                "> composer".to_string(),
                "0123456789abcdefghijklmnopqrst".to_string(),
            ],
            12,
        );
        let mut parser = vt100::Parser::new(6, 12, 0);
        parser.process(&sink.bytes());
        let screen: Vec<String> = parser.screen().rows(0, 12).collect();
        assert_eq!(
            screen[2], "transcript",
            "the over-long pinned row caused no FURTHER scroll. One scroll is \
             expected and is the transcript's own newline: the cursor is \
             parked on the region's last row, so writing a line there scrolls \
             the region once by itself. If the last pinned row had wrapped off \
             the bottom instead of being cut, everything would have shifted \
             again and this would read one row higher"
        );
        assert_eq!(screen[4], "> composer", "the first pinned row stayed put");
        assert_eq!(
            screen[5], "0123456789ab",
            "and the last pinned row is cut rather than wrapping off the \
             bottom of the screen"
        );
    }

    #[test]
    fn paint_resets_style_after_truncating_a_styled_row_so_it_cannot_bleed() {
        // The bleed case: cutting a row mid-style leaves SGR active, and the
        // next row painted immediately after would inherit it. The reset is
        // owed by the fitter, so this is a wiring test -- what reaches the
        // sink must end the row's styling before the next position.
        let (surface, sink) = shared();
        surface.show(10, 2);
        let before = sink.bytes().len();
        surface.paint(
            &[
                "\x1b[31mred row that overflows the terminal".to_string(),
                "plain".to_string(),
            ],
            8,
        );
        let tail = String::from_utf8(sink.bytes()[before..].to_vec()).expect("UTF-8");
        let (first, rest) = tail
            .split_once("\x1b[10;1H")
            .unwrap_or_else(|| panic!("two rows are painted: {tail:?}"));
        assert!(
            first.ends_with("\x1b[0m"),
            "the truncated styled row ends with a reset: {first:?}"
        );
        assert_eq!(
            // Eight columns means eight: "red row" is seven, so the eighth
            // visible character -- the space -- is kept. Reserving a column
            // would contradict `fit_row_to_width("abcdefghij", 9)`.
            first,
            "\x1b[9;1H\x1b[2K\x1b[31mred row \x1b[0m",
            "and the whole first row is exactly that: {first:?}"
        );
        assert_eq!(rest, "\x1b[2Kplain\x1b[8;1H");

        // Through the emulator: the second row is not red.
        let mut parser = vt100::Parser::new(10, 8, 0);
        parser.process(&sink.bytes());
        let second = parser.screen().cell(9, 0).expect("row 10, column 1");
        assert!(
            !second.fgcolor().eq(&vt100::Color::Idx(1)),
            "the second row must not inherit the first row's colour"
        );
    }

    // ---------------------------------------------------------------------
    // T0-S14: a dynamic viewport height.
    //
    // The height follows the content a caller hands the viewport, clamped to
    // what the terminal can hold: F19's bound means the usable maximum is
    // `terminal_rows - 2`. A request above that is **clamped rather than
    // refused** -- a palette too tall for the window is shortened, not made to
    // take the whole viewport down with it -- and when rows must be dropped
    // they go from the **top**, so the composer, the bottom row, always
    // survives.
    //
    // Both decisions live in pure functions for the reason `plan_resize` does.
    // ---------------------------------------------------------------------

    use super::{clamp_viewport_rows, fit_viewport_rows};

    fn rows_of(labels: &[&str]) -> Vec<String> {
        labels.iter().map(|row| row.to_string()).collect()
    }

    #[test]
    fn clamp_viewport_rows_leaves_a_request_the_terminal_can_hold_alone() {
        // Two scrolling rows above the viewport is the whole of the bound, so
        // on 24 rows anything up to 22 passes through untouched -- and one row
        // (today's hardcoded height) must be untouched on every terminal that
        // can pin it at all.
        assert_eq!(clamp_viewport_rows(24, 1), 1);
        assert_eq!(clamp_viewport_rows(24, 5), 5);
        assert_eq!(clamp_viewport_rows(24, 22), 22, "rows - 2 is usable");
        assert_eq!(
            clamp_viewport_rows(3, 1),
            1,
            "the smallest workable geometry"
        );
    }

    #[test]
    fn clamp_viewport_rows_shortens_a_request_taller_than_the_terminal_can_hold() {
        // The decision: clamp, do not refuse. Some rows fit, so some rows are
        // drawn.
        assert_eq!(
            clamp_viewport_rows(5, 5),
            3,
            "a five-row palette on a five-row terminal is cut to the three rows \
             that fit below two scrolling rows"
        );
        assert_eq!(clamp_viewport_rows(24, 40), 22);
        assert_eq!(clamp_viewport_rows(4, 9), 2);
        // And where not even one row can be pinned the clamp is zero, which is
        // F19's release case rather than a geometry anyone installs.
        assert_eq!(clamp_viewport_rows(2, 1), 0);
        assert_eq!(clamp_viewport_rows(1, 3), 0);
        assert_eq!(clamp_viewport_rows(0, 1), 0);
    }

    #[test]
    fn fit_viewport_rows_drops_rows_from_the_top_so_the_composer_survives() {
        // The headline, pinned exactly as the card states it: five rows
        // requested on a terminal that can hold three, and the three that
        // remain are the **last** three, in order.
        let content = rows_of(&["one", "two", "three", "four", "> composer"]);
        assert_eq!(
            fit_viewport_rows(&content, 5),
            &content[2..],
            "the topmost rows go first: the composer is the bottom row and must \
             be the last thing dropped"
        );
        assert_eq!(
            fit_viewport_rows(&content, 5).last().map(String::as_str),
            Some("> composer")
        );
        // A request the terminal can hold is handed back whole.
        assert_eq!(fit_viewport_rows(&content, 24), &content[..]);
        // And where nothing can be pinned, nothing survives.
        assert!(fit_viewport_rows(&content, 2).is_empty());
    }

    #[test]
    fn fit_viewport_rows_always_matches_the_clamp_and_show_accepts_that_height() {
        // The property that makes the clamp worth having as a pure function,
        // and the same one `plan_resize_pins_a_geometry_exactly_when_show_would_emit_one`
        // pins for the resize path: the height a request ends up with is
        // always a height `show` would latch.
        for terminal_rows in 0u16..=16 {
            for requested in 0usize..=12 {
                let content: Vec<String> = (0..requested).map(|i| format!("row {i}")).collect();
                let height = clamp_viewport_rows(terminal_rows, requested as u16);
                let fitted = fit_viewport_rows(&content, terminal_rows);

                assert!(
                    height <= requested as u16,
                    "clamp_viewport_rows({terminal_rows}, {requested}) grew the request to \
                     {height}"
                );
                assert_eq!(
                    fitted.len(),
                    height as usize,
                    "fit_viewport_rows must hand back exactly the clamped height for \
                     ({terminal_rows}, {requested})"
                );
                assert_eq!(
                    fitted,
                    &content[requested - fitted.len()..],
                    "the surviving rows are the last ones, in order, for \
                     ({terminal_rows}, {requested})"
                );

                if height > 0 {
                    let (surface, sink) = shared();
                    surface.show(terminal_rows, height);
                    assert!(
                        !sink.bytes().is_empty(),
                        "clamp_viewport_rows({terminal_rows}, {requested}) returned {height}, \
                         a height show() declines to latch -- the clamp must never admit a \
                         geometry the terminal discards"
                    );
                    assert_eq!(
                        surface.lock().geometry,
                        Some((terminal_rows - height, height))
                    );
                } else {
                    assert!(
                        terminal_rows < 3 || requested == 0,
                        "clamp_viewport_rows({terminal_rows}, {requested}) refused a height \
                         the terminal could hold: at least one row fits"
                    );
                }
            }
        }
    }

    #[test]
    // The bare `\n` below is the subject of this assertion: a block owns its
    // own line endings and gets no CRLF added. `writeln!` would emit the same
    // bytes but hide the point, so the lint is wrong here rather than the test.
    #[allow(clippy::write_with_newline)]
    fn a_shared_sink_receives_both_descriptors_in_call_order() {
        let (surface, sink) = shared();
        surface.transcript_line(Fd::Stdout, "one");
        surface.transcript_line(Fd::Stderr, "two");
        surface.transcript_line(Fd::Stdout, "three");
        // The block owns its own bytes, line ending included, so it writes
        // the bare "four\n" it asked for.
        surface.write_block(Fd::Stderr, &mut |w: &mut dyn std::io::Write| {
            write!(w, "four\n").expect("block write");
        });
        assert_eq!(
            sink.text(),
            "one\r\ntwo\r\nthree\r\nfour\n",
            "the per-descriptor split must not reorder output"
        );
    }

    /// A surface built with [`InlineSurface::new`] feeds no ring: classic and
    /// any ring-less backend pay nothing for T3-S6.
    ///
    /// Not vacuous: the surface is written to first and the write is asserted
    /// to have produced bytes, so `cells()` is `None` on a surface that really
    /// did carry transcript lines.
    #[test]
    fn an_inline_surface_without_a_cell_ring_retains_no_cells() {
        let (surface, sink) = shared();
        surface.transcript_line(Fd::Stdout, "assistant");
        surface.transcript_line(Fd::Stderr, "tool");
        assert_eq!(sink.text(), "assistant\r\ntool\r\n");
        assert_eq!(
            surface.cells(),
            None,
            "no ring was asked for, so nothing is retained"
        );
    }

    /// Feeding the ring must not change a single byte: the same calls against
    /// a ring-fed surface and a ring-less one produce identical streams, and
    /// only the ring-fed one retains cells.
    ///
    /// Not vacuous: the shared sink is asserted non-empty, and the ring-fed
    /// surface is asserted to have retained both lines, so the byte equality
    /// is between two surfaces that both actually wrote.
    #[test]
    fn feeding_the_cell_ring_changes_no_bytes() {
        let plain_sink = SharedSink::new();
        let plain = InlineSurface::new(Box::new(plain_sink.clone()), Box::new(plain_sink.clone()));
        let ringed_sink = SharedSink::new();
        let ringed = InlineSurface::with_cell_ring(
            Box::new(ringed_sink.clone()),
            Box::new(ringed_sink.clone()),
            8,
        );
        for surface in [&plain, &ringed] {
            surface.show(24, 2);
            surface.transcript_line(Fd::Stdout, "assistant");
            surface.transcript_line(Fd::Stderr, "\x1b[1mtool\x1b[0m");
            surface.paint(&["row".to_string()], 80);
            surface.hide();
        }
        assert!(!plain_sink.bytes().is_empty(), "the control surface wrote");
        assert_eq!(
            ringed_sink.bytes(),
            plain_sink.bytes(),
            "the ring is a reader, not a writer"
        );
        let cells = ringed.cells().expect("a ring was asked for");
        assert_eq!(
            cells
                .iter()
                .map(|line| (line.fd(), line.styled().to_string()))
                .collect::<Vec<_>>(),
            vec![
                (Fd::Stdout, "assistant".to_string()),
                (Fd::Stderr, "\x1b[1mtool\x1b[0m".to_string()),
            ]
        );
        assert_eq!(plain.cells(), None);
    }

    /// **T3-S10. A writer that bypasses the surface reaches the terminal but
    /// is invisible to the cell ring.**
    ///
    /// This is the data-completeness half of finding **F17**. F17 measured
    /// that a direct `eprintln!`/`println!` is *visually* harmless in inline
    /// mode — it inherits the cursor, which is parked inside the DECSTBM
    /// region, so the terminal scrolls it away with the transcript. What that
    /// measurement cannot see is requirement **N4**: the ring is to be the
    /// sole source for the transcript overlay and the `[` dump, and it is fed
    /// only from [`Surface::transcript_line`]. So output that never reached
    /// the surface appeared on screen perfectly and is absent from the only
    /// in-process record of the screen.
    ///
    /// The bypassing writer is modelled by writing into a clone of the sink
    /// the surface holds, which is exactly the shape of the real thing: the
    /// bytes land on the same descriptor, in order, and the surface is never
    /// called. The ~421 direct writer sites in `main.rs` do this to the
    /// process's real fd 1 and 2.
    ///
    /// # Why this is not vacuous
    ///
    /// The same ring is proved to capture a seam write in the same test, on
    /// the same descriptor, in the same run: the retained line is asserted to
    /// be exactly the seam one, so "the bypassed line is missing" is a
    /// property of *how it was written* and not of a ring that retains
    /// nothing. Both lines are also asserted present in the byte stream, so
    /// the bypassing write really happened.
    ///
    /// This test **characterises** the limitation rather than demanding a fix:
    /// routing hundreds of writers is the expensive branch F17 ruled out, and
    /// the honest deliverable is a tested, documented gap.
    #[test]
    // The bare `\n` is the subject of this test: a direct `eprintln!` emits a
    // line feed and no carriage return, which is exactly what is being
    // modelled. `writeln!` would emit the same byte but hide the point.
    #[allow(clippy::write_with_newline)]
    fn a_writer_that_bypasses_the_surface_reaches_the_terminal_but_not_the_cell_ring() {
        let sink = SharedSink::new();
        let surface =
            InlineSurface::with_cell_ring(Box::new(sink.clone()), Box::new(sink.clone()), 8);
        surface.transcript_line(Fd::Stderr, "through the seam");
        // The bypassing writer: same descriptor, no surface call. This is
        // `eprintln!` in `main.rs`, which adds the bare line feed `ONLCR`
        // then turns into CR LF.
        {
            let mut bypass = sink.clone();
            write!(bypass, "bypassing the seam\n").expect("direct write");
        }
        assert_eq!(
            sink.text(),
            "through the seam\r\nbypassing the seam\n",
            "both writers must have reached the descriptor, in order"
        );
        let cells = surface.cells().expect("a ring was asked for");
        assert_eq!(
            cells
                .iter()
                .map(|line| (line.fd(), line.styled().to_string()))
                .collect::<Vec<_>>(),
            vec![(Fd::Stderr, "through the seam".to_string())],
            "the ring holds the seam write and nothing of the bypassing one, so \
             the overlay and the `[` dump cannot show output that bypassed the \
             surface"
        );
    }

    /// **T3-S10. `write_block` routes through the seam and is still missing
    /// from the cell ring — and it carries the streamed assistant text.**
    ///
    /// Unlike the test above, this gap is *not* a documented limitation to
    /// live with. [`InlineSurface::write_block`] copies the block's bytes to
    /// the sink and pushes nothing, while `kitsu_render::print_fragment` and
    /// its flush route **the assistant's streamed output** through
    /// `write_block` (`crates/kitsu-render/src/lib.rs`, the
    /// `active_surface().write_block(Fd::Stdout, ..)` calls), and
    /// `render_error` routes error notices through it on stderr. N4 makes the
    /// ring the sole source for the overlay, so as it stands the overlay
    /// would render a conversation with the assistant's own replies missing
    /// while every tool line was present.
    ///
    /// The expectation is derived from what `write_block` actually does: the
    /// block owns its line endings and the bytes are forwarded verbatim, so
    /// the byte stream is `"alpha\nbeta\n"` and the ring must gain the two
    /// lines **without their endings**, which is the form
    /// `transcript_line` stores (the CRLF is the backend's, not part of the
    /// line).
    ///
    /// # Why this is not vacuous, and not unsatisfiable
    ///
    /// The byte assertion fixes the only other way to pass — rewriting the
    /// block's bytes — so the ring must gain the lines without the stream
    /// changing. A ring-less surface is written the same way in the same test
    /// and asserted to retain nothing, so the fix cannot be "always allocate
    /// a ring".
    ///
    /// Deliberately **not** asserted: what a block ending without a newline
    /// should do. `print_fragment`'s unstyled branch can write a partial
    /// line, and whether the ring should hold it back until it is terminated
    /// or retain it as a short line is a design decision this test leaves
    /// open rather than pinning from guesswork.
    #[test]
    #[allow(clippy::write_with_newline)]
    fn write_block_feeds_the_cell_ring_the_lines_it_wrote() {
        let ringed_sink = SharedSink::new();
        let ringed = InlineSurface::with_cell_ring(
            Box::new(ringed_sink.clone()),
            Box::new(ringed_sink.clone()),
            8,
        );
        let plain_sink = SharedSink::new();
        let plain = InlineSurface::new(Box::new(plain_sink.clone()), Box::new(plain_sink.clone()));
        for surface in [&ringed, &plain] {
            surface.write_block(Fd::Stdout, &mut |out: &mut dyn std::io::Write| {
                write!(out, "alpha\nbeta\n").expect("block write");
            });
        }
        assert_eq!(
            ringed_sink.text(),
            "alpha\nbeta\n",
            "the block owns its line endings and they must still be forwarded \
             verbatim: feeding the ring is not a licence to change bytes"
        );
        assert_eq!(
            ringed_sink.bytes(),
            plain_sink.bytes(),
            "the ring is a reader, not a writer"
        );
        let cells = ringed.cells().expect("a ring was asked for");
        assert_eq!(
            cells
                .iter()
                .map(|line| (line.fd(), line.styled().to_string()))
                .collect::<Vec<_>>(),
            vec![
                (Fd::Stdout, "alpha".to_string()),
                (Fd::Stdout, "beta".to_string()),
            ],
            "`write_block` carries the streamed assistant text, so a ring that \
             only sees `transcript_line` is not N4's sole source for the overlay"
        );
        assert_eq!(
            plain.cells(),
            None,
            "a surface built without a ring still retains nothing"
        );
    }
}
