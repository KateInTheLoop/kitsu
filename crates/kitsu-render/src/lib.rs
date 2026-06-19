//! kitsu-render — terminal rendering. MVP: stream text fragments to stdout.
//! Full UX (palette, code blocks, spinners) is later phases (design §8).

use std::io::{self, IsTerminal, Write};

use owo_colors::OwoColorize;

/// Cached at startup: is stdout a TTY?
pub fn is_tty() -> bool {
    io::stdout().is_terminal()
}

/// Is stdin a TTY? Chat uses this to decide interactive loop vs one-shot.
pub fn stdin_is_tty() -> bool {
    io::stdin().is_terminal()
}

/// Print a streamed text fragment immediately (no newline, flush).
pub fn print_fragment(s: &str) {
    let mut out = io::stdout();
    let _ = out.write_all(s.as_bytes());
    let _ = out.flush();
}

/// The interactive chat prompt. Fox-orange on a TTY, plain otherwise.
pub fn prompt_string() -> String {
    if is_tty() {
        format!("{} ", "kitsu>".truecolor(255, 140, 0).bold())
    } else {
        "kitsu> ".to_string()
    }
}

/// Emit a trailing newline after a streamed turn so the next prompt is clean.
pub fn finish_line() {
    let mut out = io::stdout();
    let _ = out.write_all(b"\n");
    let _ = out.flush();
}
