//! The renderer's output seam (T0-S3a).
//!
//! Every transcript line kitsu prints is a byte contract, so the seam is
//! deliberately thin: the pure formatters in [`crate`] produce the line's
//! bytes and a [`Surface`] decides where those bytes land. The classic
//! terminal backend ([`ClassicSurface`]) does exactly what the writers did
//! before the seam existed — lift the live row, `eprintln!` the line — so
//! routing a writer through the seam cannot change a single byte.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, OnceLock, RwLock};

use crate::live;

/// Which process stream a transcript line belongs on.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Fd {
    /// Assistant text.
    Stdout,
    /// Meta: tool activity, results, milestones, notices.
    Stderr,
}

/// A backend for the renderer's transcript lines.
pub trait Surface: Send + Sync {
    /// Emit one already-formatted transcript line on `fd`.
    ///
    /// `text` carries the line's final bytes (styling included) and must be
    /// written verbatim, with no re-styling, trimming or re-sanitizing.
    ///
    /// An implementation must **not** call [`set_surface`] from inside this
    /// method: the slot's write lock is not reentrant, so a backend swapping
    /// itself out from within its own write would self-deadlock.
    fn transcript_line(&self, fd: Fd, text: &str);

    /// Emit a *block* of already-formatted bytes on `fd` under at most one
    /// live-row suspension.
    ///
    /// Some writers are not line writers: they emit several lines, or a
    /// partial line, that must reach the terminal as one unit. Routing those
    /// per line would open one suspension per line and, with a drawn row,
    /// clear and repaint between them — different bytes. `block` is handed a
    /// writer and may write any number of lines (or none) to it.
    ///
    /// # Default implementation
    ///
    /// The default buffers the block and forwards each resulting line to
    /// [`Surface::transcript_line`]. That keeps the trait implementable by a
    /// backend that only knows about lines, but it **loses the
    /// single-suspension property**: a line-oriented backend that lifts a row
    /// per `transcript_line` will lift it once per line of the block. A
    /// backend for which that matters — any alternative backend painting over
    /// the same terminal — should override this method and bracket the whole
    /// block itself, as [`ClassicSurface`] does.
    fn write_block(&self, fd: Fd, block: &mut dyn FnMut(&mut dyn std::io::Write)) {
        let mut bytes: Vec<u8> = Vec::new();
        block(&mut bytes);
        let text = String::from_utf8_lossy(&bytes);
        for line in text.lines() {
            self.transcript_line(fd, line);
        }
    }

    /// Offer the backend the queued-prompt preview rows that belong directly
    /// above the status row (requirement R5, plan row T3-S5).
    ///
    /// `rows` carries `kitsu_render::live::compose_stack_rows`' output, whole
    /// and in order: already sanitized, styled and clamped per row. A backend
    /// that implements this paints them verbatim — it must not filter,
    /// re-wrap or re-clamp them, since the row count is the viewport's
    /// region arithmetic.
    ///
    /// # Default implementation
    ///
    /// **Nothing.** A backend with no pinned bottom rows has nowhere to put
    /// previews, and the classic renderer already composes its own from
    /// `live_queue_state`; forwarding these rows to
    /// [`Surface::transcript_line`] would scroll the previews into the
    /// transcript and change classic's bytes. Only a backend that owns
    /// bottom rows overrides this.
    fn queue(&self, rows: &[String]) {
        let _ = rows;
    }

    /// Record the user's own submitted prompt, **writing no bytes.**
    ///
    /// The composer paints the submitted line and erases it, so the prompt is
    /// on no descriptor and in no backend's retained cells (**F102**): this is
    /// the seam through which a backend that keeps cells can learn of it
    /// without changing a single emitted byte, which is why it is not
    /// `transcript_line`.
    ///
    /// # Default: **nothing**
    ///
    /// A backend that retains nothing has nothing to record, exactly as
    /// [`Surface::queue`] has nowhere to paint. Only a backend that owns a
    /// cell ring overrides this -- and a *wrapper* that displaces such a
    /// backend must forward it, or the prompt is dropped for the whole time
    /// the wrapper is installed.
    fn note_prompt(&self, text: &str) {
        let _ = text;
    }

    /// Echo the user's submitted prompt into the transcript so they can see
    /// what they sent, and record it marked in the ring for the `Ctrl+O`
    /// overlay (what [`Surface::note_prompt`] did alone).
    ///
    /// `prompt` is the composer's prefix (e.g. `kitsu> `), prepended to the
    /// first line only, so the echoed line matches what classic leaves on
    /// screen. Multi-line prompts echo one line each.
    ///
    /// # Default: **nothing**
    ///
    /// The classic renderer reads the submitted line through the kernel's own
    /// echo — ICANON leaves `kitsu> …` on the terminal already — so echoing it
    /// again here would double it. Only a backend whose composer *erases* the
    /// submitted row (the inline viewport) overrides this; a wrapper that
    /// displaces such a backend must forward it.
    fn echo_prompt(&self, prompt: &str, text: &str) {
        let _ = (prompt, text);
    }
}

/// Does a write on `fd` share a terminal with the live status row, and so
/// require the row to be lifted around it?
///
/// The row is painted on **stderr**, so a stderr write always shares its
/// terminal with the row. A stdout write shares it only when stdout is itself
/// a styled terminal: under `kitsu chat > file` the stdout bytes land in the
/// file and cannot disturb the row, and lifting it anyway would insert a
/// clear/repaint cycle and change the emitted *stderr* bytes. That asymmetry
/// already exists in `print_fragment`; this preserves it, in one testable
/// place.
pub(crate) fn suspends_for(fd: Fd, stdout_styled: bool) -> bool {
    matches!(fd, Fd::Stderr) || stdout_styled
}

/// The classic terminal backend: the live row is lifted for the write and
/// repainted after, exactly as the unrouted writers did.
pub struct ClassicSurface;

impl Surface for ClassicSurface {
    fn transcript_line(&self, fd: Fd, text: &str) {
        live::with_live_row_suspended(|| match fd {
            Fd::Stdout => println!("{text}"),
            Fd::Stderr => eprintln!("{text}"),
        });
    }

    /// The block runs against the real process descriptor, inside one
    /// suspension when [`suspends_for`] says the write shares the row's
    /// terminal and directly otherwise. One suspension for the whole block is
    /// what makes this byte-identical to the unrouted writers, which each
    /// wrapped their whole multi-line body in a single
    /// `with_live_row_suspended`.
    fn write_block(&self, fd: Fd, block: &mut dyn FnMut(&mut dyn std::io::Write)) {
        // `run` is `mut` because it mutably borrows `*block`.
        let mut run = || {
            match fd {
                Fd::Stdout => {
                    let mut out = std::io::stdout();
                    block(&mut out);
                    let _ = std::io::Write::flush(&mut out);
                }
                Fd::Stderr => {
                    let mut out = std::io::stderr();
                    block(&mut out);
                    let _ = std::io::Write::flush(&mut out);
                }
            };
        };
        if suspends_for(fd, crate::stdout_style()) {
            live::with_live_row_suspended(run);
        } else {
            run();
        }
    }
}

fn slot() -> &'static RwLock<Arc<dyn Surface>> {
    static SLOT: OnceLock<RwLock<Arc<dyn Surface>>> = OnceLock::new();
    SLOT.get_or_init(|| RwLock::new(Arc::new(ClassicSurface)))
}

/// The installed surface, detached from the slot.
///
/// The read guard lives only for the clone and is dropped before this returns,
/// so a caller can never hold it across a [`Surface`] call. That is what keeps
/// the seam safe for reentrant suspensions, nested async notices, synchronous
/// approval prompters under `spawn_blocking` and orchestrator tokio tasks: a
/// writer holds an `Arc`, never a lock, so a concurrent [`set_surface`] cannot
/// be starved behind an in-flight write.
pub fn active_surface() -> Arc<dyn Surface> {
    let guard = slot()
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let surface = Arc::clone(&*guard);
    drop(guard);
    surface
}

/// Install `surface`, returning the one it displaced.
pub fn set_surface(surface: Arc<dyn Surface>) -> Arc<dyn Surface> {
    let mut guard = slot()
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    std::mem::replace(&mut *guard, surface)
}

/// The bound on one foreground recap buffer, in **bytes** (design §4.5).
///
/// **Bytes, not lines, and deliberately not `kitsu_tui::cells::CellRing`'s
/// bound.** The ring that already exists is bounded by line *count*, which is
/// the hazard **F52** names: a single pathological line is still unbounded
/// memory. §4.5 specifies 64 KiB, which is a memory figure, so the bound here
/// is accounted in the unit the guarantee is stated in.
pub const RECAP_BUFFER_BYTES: usize = 64 * 1024;

/// The note that heads a recap whose buffer overflowed (design §4.5,
/// *"overflow noted"*).
///
/// **F52's sharp point:** a bounded buffer that silently shortens what it
/// kept is the quiet loss the bound exists to prevent. The buffer drops its
/// **oldest** lines, so the gap is at the top and this note sits exactly
/// where the missing output was.
pub fn recap_overflow_note(dropped_lines: usize) -> String {
    format!(
        "{dropped_lines} earlier line{} dropped: the recap buffer's \
         {} KiB filled",
        if dropped_lines == 1 { "" } else { "s" },
        RECAP_BUFFER_BYTES / 1024,
    )
}

/// A [`Surface`] that retains what it is handed instead of painting it, for
/// replay later (design §4.5: foreground output produced while the composer
/// is attached to a background agent).
///
/// It wraps the surface it displaced so a detach can put that one back, and
/// it **never** calls into it: a write returns as soon as the bytes are
/// retained, with no reader to wait for, which is what makes attaching unable
/// to pause the foreground turn (requirement A5).
pub struct BufferedSurface {
    inner: Arc<dyn Surface>,
    held: Mutex<Held>,
}

/// What one buffering attachment has retained so far.
#[derive(Default)]
struct Held {
    /// The retained lines, oldest first. Eviction is from the front, so the
    /// gap a full buffer leaves is at the top, where the note goes.
    lines: VecDeque<(Fd, String)>,
    /// The sum of `lines`' byte lengths — the quantity the bound is on.
    bytes: usize,
    /// How many lines the bound has cost this attachment, counting both the
    /// ones evicted whole and a single line clamped for being larger than the
    /// whole buffer.
    dropped: usize,
}

impl BufferedSurface {
    /// Begin buffering over `inner`, the surface this one displaces.
    pub fn new(inner: Arc<dyn Surface>) -> Self {
        Self {
            inner,
            held: Mutex::new(Held::default()),
        }
    }

    /// The displaced surface, to reinstall on detach.
    pub fn inner(&self) -> Arc<dyn Surface> {
        Arc::clone(&self.inner)
    }

    /// How many bytes are retained now. Never above [`RECAP_BUFFER_BYTES`].
    pub fn buffered_bytes(&self) -> usize {
        self.held().bytes
    }

    /// The retained state, with a poisoned lock recovered rather than
    /// propagated: a panic in some other writer must not turn the foreground's
    /// next line into a second panic.
    fn held(&self) -> std::sync::MutexGuard<'_, Held> {
        self.held
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// The recap, oldest first, and empty when nothing was buffered.
    ///
    /// When anything was dropped the first entry is
    /// [`recap_overflow_note`] on [`Fd::Stderr`].
    pub fn take_recap(&self) -> Vec<(Fd, String)> {
        let mut held = self.held();
        let dropped = std::mem::take(&mut held.dropped);
        held.bytes = 0;
        let lines = std::mem::take(&mut held.lines);
        drop(held);

        if lines.is_empty() && dropped == 0 {
            return Vec::new();
        }
        let mut recap = Vec::with_capacity(lines.len() + 1);
        if dropped > 0 {
            recap.push((Fd::Stderr, recap_overflow_note(dropped)));
        }
        recap.extend(lines);
        recap
    }
}

impl Surface for BufferedSurface {
    /// Retain the line and return. There is nothing to wait on: no bound that
    /// refuses, no channel with a receiver, and no call into the displaced
    /// surface, so a foreground turn writing into a full buffer is never
    /// paused by the attachment (requirement A5).
    fn transcript_line(&self, fd: Fd, text: &str) {
        let text = clamp_to_bound(text);
        let mut held = self.held();
        if text.clamped {
            held.dropped += 1;
        }
        // Evict oldest-first until the new line fits. A line that is itself
        // within the bound always fits once enough has gone, so this ends.
        while held.bytes + text.text.len() > RECAP_BUFFER_BYTES {
            match held.lines.pop_front() {
                Some((_, evicted)) => {
                    held.bytes -= evicted.len();
                    held.dropped += 1;
                }
                None => break,
            }
        }
        held.bytes += text.text.len();
        held.lines.push_back((fd, text.text));
    }
}

/// One line, cut to the bound if it was larger than the whole buffer.
struct Clamped {
    text: String,
    clamped: bool,
}

/// Cut `text` to [`RECAP_BUFFER_BYTES`] if it exceeds it, on a `char`
/// boundary and ending in a reset.
///
/// A byte bound can land in the middle of an SGR escape, and a severed escape
/// colours every later line of the session, so a clamped line is terminated
/// with `\x1b[0m` — the same rule `fit_row` applies when it clamps a styled
/// row. The cut also has to be a `char` boundary or `String::truncate` would
/// panic on it.
fn clamp_to_bound(text: &str) -> Clamped {
    const RESET: &str = "\x1b[0m";
    if text.len() <= RECAP_BUFFER_BYTES {
        return Clamped {
            text: text.to_string(),
            clamped: false,
        };
    }
    let mut cut = RECAP_BUFFER_BYTES - RESET.len();
    while cut > 0 && !text.is_char_boundary(cut) {
        cut -= 1;
    }
    let mut kept = text[..cut].to_string();
    kept.push_str(RESET);
    Clamped {
        text: kept,
        clamped: true,
    }
}

#[cfg(test)]
mod recap_tests {
    use std::sync::{Arc, Mutex};

    use super::{recap_overflow_note, BufferedSurface, Fd, Surface, RECAP_BUFFER_BYTES};

    /// A surface that must never be written to, so a test can claim the
    /// buffer withholds rather than forwards.
    struct NeverPaints;

    impl Surface for NeverPaints {
        fn transcript_line(&self, fd: Fd, text: &str) {
            panic!("buffered output must not reach the displaced surface: {fd:?} {text:?}");
        }
    }

    /// A surface that records what it is handed, to stand in for the one a
    /// `BufferedSurface` displaces.
    #[derive(Default)]
    struct Recorder(Mutex<Vec<(Fd, String)>>);

    impl Surface for Recorder {
        fn transcript_line(&self, fd: Fd, text: &str) {
            self.0
                .lock()
                .expect("recorder")
                .push((fd, text.to_string()));
        }
    }

    /// **Design §4.5: the recap replays what the foreground produced while
    /// attached, in order, on the stream each line was written to.**
    ///
    /// FAILING-FIRST: `take_recap` returns nothing today.
    ///
    /// The fd is part of the claim. Assistant text is `Fd::Stdout` and meta
    /// is `Fd::Stderr`; a recap that replayed everything as meta would put a
    /// redirected transcript's content on the wrong descriptor.
    #[test]
    fn the_recap_replays_the_buffered_lines_in_order_with_their_streams() {
        let buffer = BufferedSurface::new(Arc::new(NeverPaints));
        buffer.transcript_line(Fd::Stderr, "* ran a tool");
        buffer.transcript_line(Fd::Stdout, "the answer");
        buffer.transcript_line(Fd::Stderr, "* done");

        assert_eq!(
            buffer.take_recap(),
            vec![
                (Fd::Stderr, "* ran a tool".to_string()),
                (Fd::Stdout, "the answer".to_string()),
                (Fd::Stderr, "* done".to_string()),
            ],
            "the recap is what the foreground produced, in the order it did"
        );
        // Taken, not copied: a second detach has nothing left to replay, so
        // the same output cannot be shown twice.
        assert!(
            buffer.take_recap().is_empty(),
            "a drained buffer replays nothing a second time"
        );
    }

    /// **Nothing buffered replays nothing** — the claim the recap header's
    /// suppression rests on.
    #[test]
    fn a_buffer_that_was_handed_nothing_has_an_empty_recap() {
        let buffer = BufferedSurface::new(Arc::new(NeverPaints));
        assert!(buffer.take_recap().is_empty());
        assert_eq!(buffer.buffered_bytes(), 0);
    }

    /// **Design §4.5 / F52: overflow is *noted*, not silently absorbed.**
    ///
    /// FAILING-FIRST.
    ///
    /// This is the assertion F52 asked for in its own words: *"the assertion
    /// worth writing is that a clamped line is marked, not merely
    /// `len() <= cap`"*. A buffer that quietly ate its oldest lines would
    /// satisfy the byte bound below and fail here, which is the whole point —
    /// a recap with a hole in it and no note is a transcript that lies about
    /// what the foreground did.
    ///
    /// The note names a **count**, so it is also non-vacuous about how much
    /// went: a marker reading "some output was dropped" would pass a bare
    /// substring ban (**F41**) while telling the user nothing.
    #[test]
    fn an_overflowing_recap_is_headed_by_a_note_counting_what_was_dropped() {
        let buffer = BufferedSurface::new(Arc::new(NeverPaints));
        // Lines of a known size, so the dropped count is arithmetic and not
        // an observation of the implementation.
        const LINE: usize = 1024;
        let line = |n: usize| format!("{n:04} {}", "x".repeat(LINE - 5));
        // Sixty-five 1 KiB lines into a 64 KiB buffer: at least one must go.
        let pushed = RECAP_BUFFER_BYTES / LINE + 1;
        for n in 0..pushed {
            buffer.transcript_line(Fd::Stdout, &line(n));
        }

        let recap = buffer.take_recap();
        let (note_fd, note) = recap
            .first()
            .cloned()
            .expect("an overflowed recap is not empty");
        assert_eq!(
            note_fd,
            Fd::Stderr,
            "the note is meta about the transcript, not transcript"
        );
        let kept = recap.len() - 1;
        assert_eq!(
            note,
            recap_overflow_note(pushed - kept),
            "F52: the recap says how many lines the bound cost it"
        );
        // The note is about a real gap: the oldest line is gone and the
        // newest is kept, so what survived is the end of the turn the user
        // is returning to.
        let bodies: Vec<&String> = recap[1..].iter().map(|(_, text)| text).collect();
        assert!(
            !bodies.contains(&&line(0)),
            "the oldest line is the one that went"
        );
        assert_eq!(
            bodies.last().copied(),
            Some(&line(pushed - 1)),
            "and the newest is retained: a recap is read for what happened last"
        );
    }

    /// **The bound is 64 KiB of bytes**, held against one pathological line
    /// as well as against many ordinary ones — the case a line-counted ring
    /// cannot hold (**F52**).
    ///
    /// FAILING-FIRST, and the retention assertions are what stop a buffer
    /// that discards everything from passing a bound test.
    #[test]
    fn the_buffer_is_bounded_in_bytes_including_against_one_huge_line() {
        let buffer = BufferedSurface::new(Arc::new(NeverPaints));
        for n in 0..200 {
            buffer.transcript_line(Fd::Stdout, &format!("line {n} {}", "y".repeat(1000)));
        }
        assert!(
            buffer.buffered_bytes() <= RECAP_BUFFER_BYTES,
            "the bound is a byte bound: {} retained",
            buffer.buffered_bytes()
        );
        assert!(
            buffer.buffered_bytes() > 0,
            "non-vacuous: a buffer that retains nothing is bounded and useless"
        );
        assert!(
            !buffer.take_recap().is_empty(),
            "non-vacuous: the bound is held by evicting, not by refusing"
        );

        // One line larger than the whole buffer. A ring bounded by line
        // count would retain it whole -- F52's hazard exactly -- so the
        // byte bound has to hold here or it is not a memory bound.
        let buffer = BufferedSurface::new(Arc::new(NeverPaints));
        buffer.transcript_line(Fd::Stdout, &"z".repeat(RECAP_BUFFER_BYTES * 3));
        assert!(
            buffer.buffered_bytes() <= RECAP_BUFFER_BYTES,
            "one huge line must not outweigh the bound: {} retained",
            buffer.buffered_bytes()
        );
        let recap = buffer.take_recap();
        assert_eq!(
            recap.first().map(|(fd, _)| *fd),
            Some(Fd::Stderr),
            "F52: and the loss it caused is noted, not silent: {recap:?}"
        );
    }

    /// **Requirement A5: attaching never pauses the foreground turn.**
    ///
    /// FAILING-FIRST on the retention half.
    ///
    /// A buffer that applied backpressure would be a violation, and
    /// backpressure needs something to wait on: a bound that refuses, a
    /// channel with a receiver, or a call into the displaced surface. None of
    /// the three exists here, and this drives the case that would expose all
    /// of them -- far more output than the bound, against a displaced
    /// surface that panics if it is ever reached -- to completion.
    #[test]
    fn writing_past_the_bound_neither_blocks_nor_reaches_the_displaced_surface() {
        let recorder = Arc::new(Recorder::default());
        let buffer = BufferedSurface::new(recorder.clone());
        for n in 0..20_000 {
            // Infallible and total: `transcript_line` returns `()`, so a full
            // buffer has no way to signal "wait" to its writer.
            buffer.transcript_line(Fd::Stdout, &format!("burst {n}"));
        }
        assert!(
            recorder.0.lock().expect("recorder").is_empty(),
            "while attached the foreground's output is withheld, not painted"
        );
        assert!(
            !buffer.take_recap().is_empty(),
            "non-vacuous: it was withheld because it was kept, not dropped"
        );
    }

    /// A multi-line writer (`write_block`) is buffered too.
    ///
    /// FAILING-FIRST. Tool output and diffs arrive this way, so a buffer that
    /// only caught `transcript_line` would lose most of a turn's meta.
    #[test]
    fn a_block_write_is_buffered_as_its_lines() {
        let buffer = BufferedSurface::new(Arc::new(NeverPaints));
        buffer.write_block(Fd::Stderr, &mut |out| {
            let _ = std::io::Write::write_all(out, b"first\nsecond\n");
        });
        assert_eq!(
            buffer.take_recap(),
            vec![
                (Fd::Stderr, "first".to_string()),
                (Fd::Stderr, "second".to_string()),
            ],
        );
    }

    /// **F52's own caution, written by the implementer that deliberately did
    /// not clamp: "truncating styled bytes can cut an SGR sequence in half,
    /// leaving the terminal in whatever colour the severed escape implied."**
    ///
    /// FAILING-FIRST. A byte bound on styled bytes makes that hazard real for
    /// the first time in this epic, and it is not a cosmetic one: a severed
    /// escape colours every later line of the session. `fit_row`'s clamp
    /// already appends `\x1b[0m` for the same reason, so this is the
    /// renderer's existing rule and not a new one.
    #[test]
    fn a_clamped_styled_line_is_cut_on_a_character_boundary_and_reset() {
        let buffer = BufferedSurface::new(Arc::new(NeverPaints));
        // A styled, multi-byte line far past the bound: the cut lands in the
        // middle of the colour, and well inside a `\u{3053}`.
        let huge = format!(
            "\x1b[38;2;255;0;0m{}\x1b[0m",
            "\u{3053}".repeat(RECAP_BUFFER_BYTES)
        );
        buffer.transcript_line(Fd::Stdout, &huge);

        let recap = buffer.take_recap();
        let (_, kept) = recap.last().expect("the clamped line is retained").clone();
        assert!(
            kept.len() <= RECAP_BUFFER_BYTES,
            "the clamp is a byte clamp: {} kept",
            kept.len()
        );
        assert!(
            kept.ends_with("\x1b[0m"),
            "F52: a clamped styled line ends reset, or the severed escape \
             colours the rest of the session"
        );
        // Cut on a character boundary: a half-written code point would make
        // the retained line invalid UTF-8 in spirit, and `String` would have
        // panicked on the truncate that produced it.
        assert!(
            kept.chars().count() > 1,
            "non-vacuous: something of the line survived"
        );
    }

    /// The displaced surface is handed back, since a detach has to put it
    /// into the slot again.
    #[test]
    fn the_buffer_hands_back_the_surface_it_displaced() {
        let recorder: Arc<dyn Surface> = Arc::new(Recorder::default());
        let buffer = BufferedSurface::new(Arc::clone(&recorder));
        assert!(Arc::ptr_eq(&buffer.inner(), &recorder));
    }
}
