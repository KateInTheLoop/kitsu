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

/// Fox-orange (kitsune) accent colour used across the UI.
const FOX: (u8, u8, u8) = (255, 140, 0);

/// A Codex-style startup banner with kitsu's fox/cat mascot and a Japanese
/// accent (狐 = kitsune). Printed to stderr at chat start. On a non-TTY it
/// degrades to a single plain line so piped output stays clean.
pub fn chat_banner(gateway: &str, model: &str, dir: &str, tools: &[String]) {
    if !is_tty() {
        eprintln!("kitsu — gateway: {gateway} · model: {model}");
        return;
    }
    let (r, g, b) = FOX;
    let bar = "│".truecolor(r, g, b);
    let rule: String = "─".repeat(42);
    let tools_str = if tools.is_empty() {
        "none (read-only chat)".to_string()
    } else {
        tools.join(", ")
    };
    let row = |label: &str, value: &str| {
        let l = format!("{label:<9}");
        eprintln!("  {}  {}   {}", bar, l.dimmed(), value);
    };
    eprintln!();
    eprintln!(
        "  {} {} {}",
        "╭─".truecolor(r, g, b),
        "kitsu 狐".truecolor(r, g, b).bold(),
        rule.truecolor(r, g, b)
    );
    eprintln!("  {}   {}", bar, r"/\_/\".truecolor(r, g, b));
    eprintln!(
        "  {}  {}   {}",
        bar,
        "( o.o )".truecolor(r, g, b),
        "multi-gateway LLM familiar".dimmed()
    );
    eprintln!("  {}   {}", bar, "> ω <".truecolor(r, g, b));
    eprintln!("  {}", bar);
    row("gateway", gateway);
    row("model", model);
    row("directory", dir);
    row("tools", &tools_str);
    eprintln!("  {}", format!("╰{rule}────").truecolor(r, g, b));
    eprintln!("  {}", "/help for commands · Ctrl+D to exit".dimmed());
    eprintln!();
}

/// The interactive chat prompt. Fox-orange on a TTY, plain otherwise.
pub fn prompt_string() -> String {
    if is_tty() {
        format!("{} ", "kitsu>".truecolor(255, 140, 0).bold())
    } else {
        "kitsu> ".to_string()
    }
}

/// A "vibing kitsu" spinner shown on stderr while the model is working (the wait
/// before the first token). Animates a cat/fox kaomoji in fox-orange. No-op on a
/// non-TTY. Stops (and erases its line) on `stop()` or drop.
pub struct Spinner {
    handle: Option<tokio::task::JoinHandle<()>>,
}

impl Spinner {
    /// Start the animation (must be called within a tokio runtime). Returns an
    /// inert handle on a non-TTY.
    pub fn start() -> Self {
        if !is_tty() {
            return Self { handle: None };
        }
        let handle = tokio::spawn(async move {
            // Kitsu is vibing while the model does the hard work.
            const FRAMES: [&str; 6] = [
                "(=^･ω･^=)",
                "(=^･ｪ･^=)",
                "(=ↀωↀ=)",
                "(=^‥^=)",
                "ฅ(^•ω•^)ฅ",
                "(=①ω①=)",
            ];
            const DOTS: [&str; 4] = ["   ", "･  ", "･･ ", "･･･"];
            let (r, g, b) = FOX;
            let mut i = 0usize;
            loop {
                let frame = FRAMES[i % FRAMES.len()];
                let dots = DOTS[i % DOTS.len()];
                eprint!(
                    "\r  {} {}{}",
                    frame.truecolor(r, g, b),
                    "kitsu is thinking".dimmed(),
                    dots.dimmed()
                );
                let _ = io::stderr().flush();
                i += 1;
                tokio::time::sleep(std::time::Duration::from_millis(160)).await;
            }
        });
        Self {
            handle: Some(handle),
        }
    }

    /// Stop the animation and erase its line.
    pub fn stop(mut self) {
        self.clear();
    }

    fn clear(&mut self) {
        if let Some(h) = self.handle.take() {
            h.abort();
            eprint!("\r{}\r", " ".repeat(34));
            let _ = io::stderr().flush();
        }
    }
}

impl Drop for Spinner {
    fn drop(&mut self) {
        self.clear();
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
