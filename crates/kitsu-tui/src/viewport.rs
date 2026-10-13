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
    /// The last row of the scrolling transcript region, 1-based, as
    /// [`InlineSurface::show`] computed it. `None` until `show` has run.
    bottom: Option<u16>,
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
                bottom: None,
            }),
        }
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
    pub fn show(&self, rows: u16, viewport_rows: u16) {
        let bottom = rows.saturating_sub(viewport_rows);
        let mut inner = self.lock();
        inner.bottom = Some(bottom);
        // Deliberately swallowed: there is nothing to report a failed escape
        // sequence to but the terminal that would not take it.
        let _ = write!(inner.err, "\x1b[1;{bottom}r\x1b[{bottom};1H");
        let _ = inner.err.flush();
    }

    /// Release the scroll region, giving the whole screen back to scrolling.
    pub fn hide(&self) {
        let mut inner = self.lock();
        inner.bottom = None;
        let _ = inner.err.write_all(b"\x1b[r");
        let _ = inner.err.flush();
    }

    /// Repaint the pinned rows: for each row, position absolutely, clear the
    /// line, write the row's text; then park the cursor back inside the
    /// region.
    ///
    /// Called before [`show`](Self::show) this is a **no-op**: without a
    /// latched `bottom` there is no region to paint beside and no row to park
    /// on, and emitting absolute positions against a guess would scribble over
    /// the transcript. Doing nothing is the only choice that cannot corrupt
    /// the screen.
    pub fn paint(&self, viewport: &[String]) {
        let mut inner = self.lock();
        let Some(bottom) = inner.bottom else {
            return;
        };
        for (i, row) in viewport.iter().enumerate() {
            let line = bottom as usize + 1 + i;
            // No width fitting: a row wider than the terminal wraps into the
            // next viewport row. Recorded as a known gap for a later card.
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

    #[test]
    fn hide_releases_the_region() {
        let (surface, sink) = shared();
        surface.show(24, 3);
        let before = sink.bytes().len();
        surface.hide();
        assert_eq!(&sink.bytes()[before..], b"\x1b[r");
    }

    #[test]
    fn paint_positions_clears_and_writes_each_row_then_reparks() {
        let (surface, sink) = shared();
        // 10 rows, 2 viewport rows: bottom = 8, viewport occupies rows 9 and
        // 10, and the cursor goes back to row 8 inside the region.
        surface.show(10, 2);
        let before = sink.bytes().len();
        surface.paint(&["composer".to_string(), "status".to_string()]);
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
        surface.paint(&[
            "> composer".to_string(),
            "status: 2 agents".to_string(),
            "hint: ctrl-c".to_string(),
        ]);
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
}
