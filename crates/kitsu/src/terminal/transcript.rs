//! Terminal transcript navigation and search.

#[cfg(unix)]
use crate::terminal::overlay_input::stdin_waiting;
#[cfg(unix)]
use crate::terminal::overlay_input::ESC_SEQUENCE_GRACE;
#[cfg(unix)]
use crate::tui_bridge;

/// Open the transcript overlay over the cell ring and run it until `q` or
/// `Esc` closes it (requirement **R6**, first piece).
///
/// # Reachable from both surfaces, which is what `take_alternate_screen` says
///
/// `Ctrl+O` is read by the one inline reader both surfaces use, so the overlay
/// has two call sites: the inline idle prompt, which is on the primary screen
/// and must therefore **take** the alternate screen for the overlay, and the
/// fullscreen idle prompt, which is already holding one and must not take a
/// second (a second `ESC[?1049h` would have no counterpart in the leave).
/// Finding **F89**: a capability that compiles is not one that is reachable,
/// and one route is not two.
///
/// # The anchor is the overlay's own
///
/// [`kitsu_tui::transcript::TranscriptOverlay`] holds its own
/// `FullscreenView`, so paging here moves nothing the fullscreen segment
/// owns; on close the fullscreen caller repaints from the segment's anchor and
/// the user is back on the frame they left.
///
/// # The dying-signal restore
///
/// On the inline route this is the one thing on screen when a signal arrives,
/// so [`kitsu_tui::note_alternate_screen`] is stored **before** the enter
/// bytes and cleared **after** the leave, exactly as the agent view's overlay
/// orders the same pair (**F94**): the straight-line leave below never runs
/// for a panic or a dying signal, and the handler installed by
/// `tui_bridge::install_exit_restore` -- which the inline surface's own
/// install site registers before any idle prompt can be reached -- gives the
/// screen back instead. Raw mode is the caller's: both call sites already hold
/// a `RawStdinGuard`, and nesting a second one would publish the *raw* modes
/// as the canonical ones for that handler.
#[cfg(unix)]
pub(crate) fn run_transcript_overlay(
    lines: &[kitsu_tui::cells::CellLine],
    take_alternate_screen: bool,
) {
    use std::io::Write as _;

    // Nothing is drawn into a carrier that cannot be drawn into, and nothing
    // is written before the decision, exactly as the agent view's overlay
    // declines.
    if !kitsu_render::stdin_is_tty() || !kitsu_render::stderr_styled() {
        return;
    }

    let mut err = std::io::stderr();
    if take_alternate_screen {
        kitsu_tui::note_alternate_screen(true);
        let _ = err.write_all(b"\x1b[?1049h\x1b[2J\x1b[H\x1b[?25l\x1b[?2004h");
    } else {
        // The screen is the fullscreen segment's and stays its; only the
        // caret is the overlay's business, since the overlay has none.
        let _ = err.write_all(b"\x1b[?25l");
    }
    let _ = err.flush();

    // The key loop is its own function, and not for tidiness: it is the
    // thing the byte-level tests drive (finding **F69**). A test that mirrored
    // this loop instead of running it would be a second decoder agreeing with
    // itself -- which is exactly how the `Esc` defect this seam was extracted
    // for went unseen: the key-space sweep in `kitsu_tui::transcript` proves
    // the *predicate* answers `Esc`, and proves nothing about whether anything
    // ever hands it one.
    let ring = tui_bridge::frame_ring(lines);
    let mut overlay = kitsu_tui::transcript::TranscriptOverlay::new();
    let mut stdin = std::io::stdin();
    let _exit = run_transcript_key_loop(
        &mut stdin,
        &mut overlay,
        &ring,
        // Probed per frame, so a `SIGWINCH` that wakes the read is answered at
        // the new geometry.
        &mut || {
            let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));
            (rows, cols as usize)
        },
        // The agent view's own grace and its own `poll`, not a second copy:
        // the only honest way to tell a lone `ESC` from the first byte of an
        // arrow is time, and a second implementation of that is how two
        // surfaces come to disagree about what `Esc` means.
        &mut || stdin_waiting(ESC_SEQUENCE_GRACE),
        &mut |frame: &[String]| {
            // Absolute addressing and a cleared row each, for the agent
            // view's reason: the overlay owns the whole screen while it is
            // up, and a painted row that scrolled would cost a frame row.
            let mut painted = String::from("\x1b[2J");
            for (index, row) in frame.iter().enumerate() {
                painted.push_str(&format!("\x1b[{};1H\x1b[2K{row}", index + 1));
            }
            let _ = err.write_all(painted.as_bytes());
            let _ = err.flush();
        },
    );

    let _ = err.write_all(b"\x1b[?25h");
    if take_alternate_screen {
        let _ = err.write_all(b"\x1b[?1049l");
        let _ = err.flush();
        // After the bytes (**F94**): a signal in this window writes a second
        // leave onto a primary screen rather than none onto an alternate one.
        kitsu_tui::note_alternate_screen(false);
        // The region and the pinned status row, for the agent view overlay's
        // reason: a terminal that keeps per-screen margins hands back an
        // unpinned one.
        tui_bridge::reinstall_pinned_region();
    } else {
        let _ = err.flush();
    }
}

/// How the transcript overlay's key loop ended.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TranscriptExit {
    /// A close key was pressed: `q`, or `Esc`.
    Closed,
    /// The descriptor was drained or errored. A descriptor that cannot deliver
    /// the key which closes the overlay closes it rather than spinning.
    Drained,
}

/// The transcript overlay's key loop: paint, read, decode, repeat until a
/// close key or a drained descriptor.
///
/// Extracted from [`run_transcript_overlay`] so the **real** decoder is what
/// the tests drive (finding **F69**): `input` is any reader, the geometry and
/// the paint are closures, and `more_input_waiting` is the lone-`ESC` grace --
/// production passes the agent view's own `stdin_waiting(ESC_SEQUENCE_GRACE)`.
/// Nothing in here touches a terminal, which is what makes a byte-level
/// assertion possible at all.
///
/// # Why a lone `ESC` needs the grace rather than a chunk boundary
///
/// `ESC` is the first byte of every sequence this loop drops and of the two
/// `PgUp`/`PgDn` presses it answers, so a chunk that ends on one is ambiguous
/// (**F65**: a multi-byte key is not one byte). Resolving it by "nothing
/// followed it in this chunk" is **wrong**, and measurably so: a lone `Esc`
/// keypress delivers exactly `[0x1b]`, which is a strict prefix of all four
/// paging sequences, so the partial-hold took it and the loop went back to a
/// blocking read -- `Esc` did nothing until the user pressed something else,
/// while the footer advertised it (**F75**: a key that appears to work is a
/// defect, not a missing feature). The grace is the agent view's answer to the
/// same ambiguity and this is the same mechanism, not a second one.
#[cfg(unix)]
pub(crate) fn run_transcript_key_loop<R: std::io::Read>(
    input: &mut R,
    overlay: &mut kitsu_tui::transcript::TranscriptOverlay,
    ring: &kitsu_tui::cells::CellRing,
    geometry: &mut dyn FnMut() -> (u16, usize),
    more_input_waiting: &mut dyn FnMut() -> bool,
    paint: &mut dyn FnMut(&[String]),
) -> TranscriptExit {
    // The bracketed-paste markers the alternate-screen route turns on with
    // `ESC[?2004h`. They were already being consumed whole by the generic
    // sequence arm below -- the arm runs to the first `0x40..=0x7e` byte and
    // `~` is `0x7e` -- so the signal was on the wire and thrown away. R6-a's
    // lesson is what it is used for here: a sequence, or a newline, **inside**
    // a paste is content and not a keypress.
    const PASTE_BEGIN: &[u8] = b"\x1b[200~";
    const PASTE_END: &[u8] = b"\x1b[201~";

    let mut buffer = [0u8; 256];
    // A sequence split across reads is still one keystroke (**F65**), so the
    // tail of a chunk that ran out mid-sequence waits for the rest of itself.
    let mut held: Vec<u8> = Vec::new();
    // Whether the bytes being decoded are inside a bracketed paste, and
    // whether the query line has already taken its one line from it: a query
    // line is one line, so the paste's first newline ends the query and the
    // remainder of the **paste** -- not of the chunk, which would be the
    // boundary rule **F65** calls wrong -- is discarded.
    let mut pasting = false;
    let mut paste_spent = false;
    loop {
        let (rows, cols) = geometry();
        paint(&overlay.frame(ring, rows, cols));

        // The grace, before the next read can block on a key the user has
        // already pressed.
        if !held.is_empty() && !more_input_waiting() {
            let lone_escape = held == [0x1b];
            held.clear();
            if lone_escape {
                // R6-b: `Esc` out of an open query line is the way out of the
                // query, and leaves the overlay the user is still reading up.
                // Closing here would put the surface one press away from
                // vanishing on anyone who changed their mind mid-query. The
                // grace is still the one mechanism (**F65**); only what the
                // resolved `Esc` means depends on the mode.
                if overlay.search_query().is_some() {
                    overlay.cancel_search();
                    continue;
                }
                return TranscriptExit::Closed;
            }
            // An incomplete sequence whose rest never came: dropped, never
            // read as the keys its bytes spell.
            continue;
        }

        let count = match input.read(&mut buffer) {
            Ok(0) => return TranscriptExit::Drained,
            Ok(count) => count,
            // A signal woke the read (a `SIGWINCH`, typically): repaint and
            // carry on, which also re-probes the geometry.
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return TranscriptExit::Drained,
        };
        let mut data = std::mem::take(&mut held);
        data.extend_from_slice(&buffer[..count]);
        let mut index = 0usize;
        while index < data.len() {
            let rest = &data[index..];
            // The paste markers first, and before the paging table: inside a
            // paste every other sequence is content, so the markers are the
            // one pair of sequences that still mean something there.
            if rest.starts_with(PASTE_BEGIN) {
                pasting = true;
                paste_spent = false;
                index += PASTE_BEGIN.len();
                continue;
            }
            if rest.starts_with(PASTE_END) {
                pasting = false;
                paste_spent = false;
                index += PASTE_END.len();
                continue;
            }
            // The remainder of a paste whose first newline has already closed
            // the query line: discarded byte by byte, so nothing behind that
            // newline is dispatched as a key.
            if pasting && paste_spent {
                index += 1;
                continue;
            }
            if !pasting {
                // The four paging sequences, through the bridge's own table:
                // a second copy is how two surfaces come to disagree about
                // which bytes `Ctrl+End` is.
                if let Some((press, length)) = tui_bridge::page_key(rest) {
                    overlay.page(press, ring, rows);
                    index += length;
                    continue;
                }
                if tui_bridge::partial_page_key(rest) {
                    held = rest.to_vec();
                    break;
                }
            }
            let byte = data[index];
            index += 1;
            // A newline inside a paste is query **content**, not a submit and
            // not a keypress: it ends the one line the query line holds, and
            // everything behind it in the paste goes above. Measured before
            // this existed: `b"/find me\nq"` ended `Closed`, with the `0x0a`
            // dropped as a control byte and the `q` behind it dispatched as
            // the close key -- a multi-line paste taking the overlay away from
            // under the user and searching nothing.
            if pasting && matches!(byte, 0x0a | 0x0d) {
                paste_spent = overlay.search_query().is_some();
                continue;
            }
            if byte == 0x1b {
                // An `ESC` with bytes behind it that start some *other*
                // sequence is that sequence and not a close: its bytes are
                // dropped rather than read as keys.
                if data.get(index) == Some(&b'[') || data.get(index) == Some(&b'O') {
                    index += 1;
                    while let Some(byte) = data.get(index) {
                        index += 1;
                        if (0x40..=0x7e).contains(byte) {
                            break;
                        }
                    }
                    continue;
                }
                // An `ESC` that ends the chunk is still ambiguous: hold it for
                // the grace above rather than guessing from the boundary.
                if index == data.len() {
                    held = vec![0x1b];
                    break;
                }
            }
            // R6-b: while the query line is open it is a **mode**, and the
            // surface's own keys are text. `q` especially: without this the
            // word `query` cannot be searched for and the overlay vanishes
            // mid-typing. The scan is the submit's and only the submit's,
            // which is why nothing here touches the ring.
            //
            // Outside a paste a bare `0x0a` is read as a submit, exactly as
            // `0x0d` is: with no paste markers this loop cannot tell a pasted
            // newline from a pressed `Enter` at all, which is **F72** proper
            // and stays open.
            //
            // **Corrected (F116).** An earlier version of this comment blamed
            // "the inline route never writes `?2004h`", which is backwards
            // twice over. The enable sits in the `take_alternate_screen`
            // branch above, and it is the *inline* route that passes `true`
            // (inline has no alternate screen, so the overlay takes it); the
            // *fullscreen* route passes `false` because its segment already
            // holds the screen. And either way a `RawStdinGuard` is held at
            // both call sites -- it writes `?2004h` on engage and `?2004l` on
            // drop -- so markers are present on any terminal that supports
            // them. What is left of F72 here is narrower than that comment
            // claimed: a terminal that *ignores* `?2004h` sends no markers,
            // and then this loop genuinely cannot tell paste from typing.
            if overlay.search_query().is_some() {
                match byte {
                    0x0d | 0x0a => {
                        overlay.submit_search(ring, rows);
                    }
                    0x7f | 0x08 => overlay.backspace_search(),
                    0x1b => overlay.cancel_search(),
                    byte if (0x20..=0x7e).contains(&byte) => {
                        overlay.push_search_char(byte as char);
                    }
                    // Every other control byte is inert, as it is outside a
                    // query: the query line holds what the user can see.
                    _ => {}
                }
                continue;
            }
            let press = match byte {
                0x1b => kitsu_tui::keys::Press::Esc,
                byte if (0x20..=0x7e).contains(&byte) => kitsu_tui::keys::Press::Char(byte as char),
                // Every other control byte is inert here, which is what keeps
                // the footer honest: it advertises the keys this loop answers.
                _ => continue,
            };
            // R6-c: the two prompt jumps the overlay's footer advertises.
            // Both bytes already fell inside the printable arm above, so
            // before this they became a `Press::Char` that `closes` declined
            // and the loop read again -- inert while the footer named them,
            // which is **F75**. Dispatched here rather than beside the loop,
            // for `page`'s reason (**F69**): the capability has to be
            // reachable from the real decoder. The repaint is the loop's own,
            // at the top of the next pass, exactly as a paging press is
            // repainted.
            let jump = match press {
                kitsu_tui::keys::Press::Char('{') => Some(kitsu_tui::fullscreen::Jump::Prev),
                kitsu_tui::keys::Press::Char('}') => Some(kitsu_tui::fullscreen::Jump::Next),
                _ => None,
            };
            if let Some(jump) = jump {
                overlay.jump(jump, ring, rows);
                continue;
            }
            // R6-b: `/` opens the query line, and `n`/`N` walk the matches
            // the last submitted query found -- `n` newer, `N` older, through
            // R6-c's own `Jump` and not a second direction concept. All three
            // bytes already fell inside the printable arm above, so before
            // this they became a `Press::Char` that `closes` declined and the
            // loop read again: inert keys the footer was about to advertise,
            // which is **F75**. The repaint is the loop's own, at the top of
            // the next pass, exactly as a paging press is repainted.
            match press {
                kitsu_tui::keys::Press::Char('/') => {
                    overlay.begin_search();
                    continue;
                }
                kitsu_tui::keys::Press::Char('n') => {
                    overlay.repeat_search(kitsu_tui::fullscreen::Jump::Next, ring, rows);
                    continue;
                }
                kitsu_tui::keys::Press::Char('N') => {
                    overlay.repeat_search(kitsu_tui::fullscreen::Jump::Prev, ring, rows);
                    continue;
                }
                _ => {}
            }
            if kitsu_tui::transcript::closes(press) {
                return TranscriptExit::Closed;
            }
        }
    }
}
