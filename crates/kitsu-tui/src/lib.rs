//! `kitsu-tui` is kitsu's terminal-user-interface crate: the alternate-screen
//! runtime that the interactive agent-herding UI will be drawn into.
//!
//! This card carries the **terminal lifecycle alone** — entering and leaving
//! the terminal, title save/restore, title sanitisation and panic-time
//! restoration. Nothing is rendered yet; no widgets, no layout, no event loop.
//! The one invariant it exists to guarantee is that kitsu always gives the
//! terminal back, on every exit path including a panic and an interrupt.
//!
//! # Terminal-title save and restore is not portable
//!
//! [`TuiRuntime::enter`] pushes the window title onto the terminal's title
//! stack with `CSI 22;2t` and [`TuiRuntime::leave`] pops it with `CSI 23;2t`.
//! **Inside tmux and GNU screen both sequences are silently ignored**, which
//! is precisely where agent-herding sessions tend to live. The consequence is
//! narrow but should be stated plainly: under a multiplexer the title restore
//! is a **no-op**, so a title kitsu sets stays until something else sets it.
//! An ignored sequence is inert rather than broken — the stream is pure
//! control bytes and prints nothing — so nothing is corrupted either way.
//!
//! There is no better portable option. OSC 2 has no query form, so the old
//! title cannot be read back and re-set by hand; the title stack is the only
//! mechanism terminals offer, and it is the multiplexer that drops it. A
//! possible later addition is to also set the tmux *pane* title when `$TMUX`
//! is set, which tmux does honour; this card deliberately does not change the
//! bytes it writes.
//!
//! # Title sanitisation
//!
//! Titles are untrusted. [`sanitize_title`] strips every control character —
//! `ESC`, `BEL` and NUL among them — collapses whitespace runs and bounds the
//! result, and [`TuiRuntime::enter`] applies it to its own argument rather
//! than trusting the caller. The title will eventually carry agent counts and
//! activity labels derived from model-driven work, so this is the boundary at
//! which an escape sequence could otherwise reach the user's terminal.

pub mod composer;
pub mod status;
pub mod viewport;

use std::io::{IsTerminal, Write};

/// Push the window title onto the terminal's title stack.
const PUSH_TITLE: &[u8] = b"\x1b[22;2t";
/// Pop the window title back off the terminal's title stack.
const POP_TITLE: &[u8] = b"\x1b[23;2t";
/// Show the cursor again.
const SHOW_CURSOR: &[u8] = b"\x1b[?25h";
/// Turn bracketed-paste reporting off.
const PASTE_OFF: &[u8] = b"\x1b[?2004l";

/// The longest title, in characters, that [`sanitize_title`] will return.
const TITLE_MAX_CHARS: usize = 256;

/// Owns the terminal between [`enter`](TuiRuntime::enter) and
/// [`leave`](TuiRuntime::leave), and gives it back on drop.
///
/// The writer is generic so the lifecycle can be exercised against an
/// in-memory buffer instead of a real terminal; production use passes a
/// [`std::io::Stderr`].
#[derive(Debug)]
pub struct TuiRuntime<W: Write> {
    out: W,
    /// Whether `enter` has run without a matching `leave`. This is the flag
    /// that makes `leave` idempotent and `Drop` a no-op after an explicit
    /// `leave`.
    entered: bool,
    /// Whether *this* runtime put the terminal into raw mode, so that only it
    /// takes it back out again.
    raw: bool,
}

impl<W: Write> TuiRuntime<W> {
    /// Wrap `out`. Nothing is written and no terminal state changes until
    /// [`enter`](Self::enter) is called.
    pub fn new(out: W) -> Self {
        Self {
            out,
            entered: false,
            raw: false,
        }
    }

    /// Take the terminal: save the window title, optionally set a new one,
    /// and enable raw mode when there is a real terminal to enable it on.
    ///
    /// `title` is passed through [`sanitize_title`] here; the caller is not
    /// trusted to have done it.
    ///
    /// Raw mode is enabled **only** when the process is attached to a
    /// terminal. Tests, pipes and scripted runs are not, and there `enter`
    /// still returns `Ok` after writing its sequences: the bytes are inert
    /// when nothing interprets them, and failing would make both the piped
    /// path and this type's own tests unusable.
    ///
    /// Calling `enter` again while already entered rewrites the title
    /// sequences but does not stack a second restore.
    pub fn enter(&mut self, title: Option<&str>) -> std::io::Result<()> {
        self.out.write_all(PUSH_TITLE)?;
        if let Some(raw_title) = title {
            let safe = sanitize_title(raw_title);
            self.out.write_all(b"\x1b]2;")?;
            self.out.write_all(safe.as_bytes())?;
            self.out.write_all(b"\x1b\\")?;
        }
        self.out.flush()?;
        self.entered = true;

        // The writer is generic and opaque, so terminal-ness is asked of the
        // process's own stderr — the stream the interactive UI is drawn to.
        if !self.raw && std::io::stderr().is_terminal() {
            crossterm::terminal::enable_raw_mode()?;
            self.raw = true;
        }
        Ok(())
    }

    /// Give the terminal back: show the cursor, turn bracketed paste off, pop
    /// the saved window title, and leave raw mode if this runtime entered it.
    ///
    /// Idempotent — the restore bytes are written at most once per `enter`,
    /// and calling it without a preceding `enter` does nothing.
    ///
    /// This returns `()` even though it performs I/O, so a failed restore is
    /// invisible. That is deliberate, not an oversight: `leave` is the body of
    /// [`Drop`], and a destructor must neither panic nor propagate. There is
    /// also nothing useful to do about a terminal that will not take its own
    /// escape sequences, and the only place left to report it to is the
    /// terminal itself.
    pub fn leave(&mut self) {
        if !self.entered {
            return;
        }
        self.entered = false;

        if self.raw {
            // Swallowed deliberately: see the note above.
            let _ = crossterm::terminal::disable_raw_mode();
            self.raw = false;
        }

        // Every result below is swallowed deliberately: see the note above.
        let _ = self.out.write_all(SHOW_CURSOR);
        let _ = self.out.write_all(PASTE_OFF);
        let _ = self.out.write_all(POP_TITLE);
        let _ = self.out.flush();
    }

    /// Whether the terminal is currently held.
    pub fn is_entered(&self) -> bool {
        self.entered
    }
}

impl<W: Write> Drop for TuiRuntime<W> {
    fn drop(&mut self) {
        self.leave();
    }
}

/// Make `raw` safe to put inside an OSC 2 title sequence.
///
/// Every control character is dropped — `ESC` and `BEL` so the title cannot
/// terminate its own sequence and start another, NUL and the rest because a
/// title is a single line of text. Whitespace runs collapse to one space, the
/// result is trimmed, and it is bounded to 256 characters so an oversized
/// label cannot flood the terminal.
pub fn sanitize_title(raw: &str) -> String {
    let mut out = String::new();
    let mut pending_space = false;
    for ch in raw.chars() {
        if ch.is_control() {
            continue;
        }
        if ch.is_whitespace() {
            // Only materialises once a non-space follows, which also trims
            // both ends.
            pending_space = !out.is_empty();
            continue;
        }
        if pending_space {
            if out.chars().count() + 1 >= TITLE_MAX_CHARS {
                break;
            }
            out.push(' ');
            pending_space = false;
        }
        if out.chars().count() >= TITLE_MAX_CHARS {
            break;
        }
        out.push(ch);
    }
    out
}

/// Chain a terminal restore onto the current panic hook.
///
/// The previously installed hook still runs — this wraps it rather than
/// replacing it — so backtraces and any custom reporting are preserved. The
/// restore writes the same sequences as [`TuiRuntime::leave`] straight to
/// stderr, because a panicking process cannot reach the `TuiRuntime` that
/// owns the writer, and leaves raw mode if the terminal is in it.
pub fn install_panic_restore() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // All errors swallowed: this runs while the process is already dying,
        // and the only place to report to is the terminal being restored.
        let _ = crossterm::terminal::disable_raw_mode();
        let mut err = std::io::stderr();
        let _ = err.write_all(SHOW_CURSOR);
        let _ = err.write_all(PASTE_OFF);
        let _ = err.write_all(POP_TITLE);
        let _ = err.flush();
        previous(info);
    }));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::io::Write;
    use std::rc::Rc;

    /// Title-stack push, written by `enter`.
    const PUSH: &[u8] = b"\x1b[22;2t";
    /// Title-stack pop, written by `leave`.
    const POP: &[u8] = b"\x1b[23;2t";
    const SHOW_CURSOR: &[u8] = b"\x1b[?25h";
    const PASTE_OFF: &[u8] = b"\x1b[?2004l";

    /// A writer whose buffer outlives the `TuiRuntime` that holds it, so the
    /// bytes written by `Drop` are still observable after the runtime is gone.
    #[derive(Clone, Default)]
    struct SharedSink(Rc<RefCell<Vec<u8>>>);

    impl SharedSink {
        fn new() -> Self {
            Self::default()
        }
        fn bytes(&self) -> Vec<u8> {
            self.0.borrow().clone()
        }
    }

    impl Write for SharedSink {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.borrow_mut().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
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
    fn enter_without_title_writes_only_the_title_push() {
        let sink = SharedSink::new();
        let mut rt = TuiRuntime::new(sink.clone());
        rt.enter(None).expect("enter must succeed off a terminal");
        assert_eq!(sink.bytes(), PUSH.to_vec());
    }

    #[test]
    fn enter_with_title_writes_push_then_osc_set_title() {
        let sink = SharedSink::new();
        let mut rt = TuiRuntime::new(sink.clone());
        rt.enter(Some("kitsu")).expect("enter must succeed");
        let mut want = PUSH.to_vec();
        want.extend_from_slice(b"\x1b]2;kitsu\x1b\\");
        assert_eq!(sink.bytes(), want);
    }

    #[test]
    fn leave_writes_the_three_restore_sequences_in_order() {
        let sink = SharedSink::new();
        let mut rt = TuiRuntime::new(sink.clone());
        rt.enter(None).unwrap();
        rt.leave();
        let bytes = sink.bytes();
        let tail = &bytes[PUSH.len()..];
        let mut want = SHOW_CURSOR.to_vec();
        want.extend_from_slice(PASTE_OFF);
        want.extend_from_slice(POP);
        assert_eq!(tail, want.as_slice(), "show cursor, paste off, title pop");
    }

    /// The named test for this card.
    #[test]
    fn leave_twice_emits_restore_bytes_exactly_once() {
        let sink = SharedSink::new();
        let mut rt = TuiRuntime::new(sink.clone());
        rt.enter(None).unwrap();
        rt.leave();
        rt.leave();
        let bytes = sink.bytes();
        assert_eq!(count(&bytes, SHOW_CURSOR), 1);
        assert_eq!(count(&bytes, PASTE_OFF), 1);
        assert_eq!(count(&bytes, POP), 1);
    }

    #[test]
    fn drop_after_enter_restores_once_and_drop_after_leave_adds_nothing() {
        let sink = SharedSink::new();
        {
            let mut rt = TuiRuntime::new(sink.clone());
            rt.enter(None).unwrap();
        }
        let dropped = sink.bytes();
        assert_eq!(count(&dropped, POP), 1, "Drop restores exactly once");

        let sink2 = SharedSink::new();
        let after_leave;
        {
            let mut rt = TuiRuntime::new(sink2.clone());
            rt.enter(None).unwrap();
            rt.leave();
            after_leave = sink2.bytes();
        }
        assert_eq!(
            sink2.bytes(),
            after_leave,
            "Drop after an explicit leave writes nothing further"
        );
        assert_eq!(count(&sink2.bytes(), POP), 1);
    }

    #[test]
    fn drop_without_enter_writes_nothing() {
        let sink = SharedSink::new();
        drop(TuiRuntime::new(sink.clone()));
        assert!(sink.bytes().is_empty());
    }

    #[test]
    fn is_entered_tracks_enter_and_leave() {
        let sink = SharedSink::new();
        let mut rt = TuiRuntime::new(sink);
        assert!(!rt.is_entered());
        rt.enter(None).unwrap();
        assert!(rt.is_entered());
        rt.leave();
        assert!(!rt.is_entered());
    }

    #[test]
    fn enter_is_ok_when_the_writer_is_not_a_terminal() {
        let mut rt = TuiRuntime::new(Vec::<u8>::new());
        assert!(rt.enter(Some("not a tty")).is_ok());
    }

    #[test]
    fn sanitize_title_strips_controls_and_bounds_length() {
        let out = sanitize_title("a\x1bb\x07c\0d\ne\tf");
        assert!(
            !out.chars().any(char::is_control),
            "no control characters survive: {out:?}"
        );
        assert!(!out.contains('\x1b'));
        assert!(!out.contains('\x07'));
        assert!(!out.contains('\0'));

        let long = sanitize_title(&"x".repeat(10_000));
        assert!(
            long.chars().count() <= 256,
            "length is bounded, got {}",
            long.chars().count()
        );

        // Whitespace collapses rather than leaving ragged runs.
        assert_eq!(sanitize_title("two   \t  words"), "two words");
    }

    #[test]
    fn sanitize_title_cannot_escape_its_own_osc_sequence() {
        let hostile = "x\x1b\\\x1b]0;pwned\x07";
        let out = sanitize_title(hostile);
        // Stated as its own assertion: neither OSC terminator can appear.
        assert!(!out.contains('\x1b'), "no ESC, so no ST terminator");
        assert!(!out.contains('\x07'), "no BEL terminator");
        assert!(!out.contains("\x1b\\"));
        let wrapped = format!("\x1b]2;{out}\x1b\\");
        assert_eq!(
            wrapped.matches('\x1b').count(),
            2,
            "exactly the opening OSC and the closing ST: {wrapped:?}"
        );
    }

    #[test]
    fn the_full_enter_leave_stream_prints_no_visible_text() {
        let sink = SharedSink::new();
        {
            let mut rt = TuiRuntime::new(sink.clone());
            rt.enter(Some("kitsu 3 agents")).unwrap();
            rt.leave();
        }
        let mut parser = vt100::Parser::new(24, 80, 0);
        parser.process(&sink.bytes());
        let contents = parser.screen().contents();
        assert!(
            contents.trim().is_empty(),
            "pure control sequences must print nothing, got {contents:?}"
        );
    }

    #[test]
    fn install_panic_restore_chains_the_previous_hook() {
        // CAVEAT: the panic hook is process-global. This test replaces it and
        // must put the original back, or every other test in this binary runs
        // under a leaked hook.
        let original = std::panic::take_hook();

        // The hook must be Send + Sync, so the flag is an Arc<AtomicBool>.
        let flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag_in_hook = flag.clone();
        std::panic::set_hook(Box::new(move |_| {
            flag_in_hook.store(true, std::sync::atomic::Ordering::SeqCst);
        }));

        install_panic_restore();

        let _ = std::panic::catch_unwind(|| panic!("boom"));

        assert!(
            flag.load(std::sync::atomic::Ordering::SeqCst),
            "install_panic_restore must chain, not replace, the previous hook"
        );
        std::panic::set_hook(original);
    }
}
