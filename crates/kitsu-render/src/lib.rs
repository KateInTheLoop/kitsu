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

/// Announce a tool call to **stderr** (meta, not assistant text) as a compact,
/// friendly line — e.g. `⏺ Read(deck1.md)`. Fox-orange marker on a TTY.
pub fn tool_activity(header: &str) {
    if is_tty() {
        eprintln!("{} {header}", "⏺".truecolor(255, 140, 0));
    } else {
        eprintln!("⏺ {header}");
    }
}

/// The result summary line under a tool call — e.g. `  ⎿ 128 lines`. Dimmed on a
/// TTY. Goes to stderr so stdout stays assistant-text only.
pub fn tool_result(summary: &str) {
    let line = format!("  ⎿ {summary}");
    if is_tty() {
        eprintln!("{}", line.dimmed());
    } else {
        eprintln!("{line}");
    }
}

/// Emit a single machine-readable token-usage line to **stderr** (never stdout,
/// which carries only assistant text). Format is a stable, greppable prefix
/// followed by compact JSON so external consumers can parse it:
///
/// ```text
/// kitsu-usage: {"prompt_tokens":21,"completion_tokens":9,"total_tokens":30}
/// ```
pub fn emit_usage(prompt_tokens: u64, completion_tokens: u64, total_tokens: u64) {
    let mut err = io::stderr();
    let _ = writeln!(
        err,
        "kitsu-usage: {{\"prompt_tokens\":{prompt_tokens},\"completion_tokens\":{completion_tokens},\"total_tokens\":{total_tokens}}}"
    );
    let _ = err.flush();
}
