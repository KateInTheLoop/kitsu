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
    eprintln!(
        "  {}",
        "/ for commands · Tab to complete · Ctrl+D to exit".dimmed()
    );
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

/// Announce a summarizing compaction to stderr, like Claude Code's boundary, so
/// the user knows older history was replaced by a summary.
pub fn compaction_boundary() {
    let line = "✻ context compacted (older turns summarized)";
    if is_tty() {
        let (r, g, b) = FOX;
        eprintln!("{}", line.truecolor(r, g, b));
    } else {
        eprintln!("{line}");
    }
}

fn grouped_count(value: u64) -> String {
    let digits = value.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, ch) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

fn human_turn_stats(
    usage: Option<(u64, u64, u64)>,
    context: Option<(u64, u64, u64)>,
) -> Option<String> {
    let mut parts = Vec::new();
    if let Some((prompt, completion, _total)) = usage {
        parts.push(format!("{} input", grouped_count(prompt)));
        parts.push(format!("{} output", grouped_count(completion)));
    }
    if let Some((used, effective, _window)) = context {
        let pct = if effective == 0 {
            0
        } else {
            used.saturating_mul(100).saturating_div(effective).min(999)
        };
        parts.push(format!(
            "{} / {} context ({pct}% used)",
            grouped_count(used),
            grouped_count(effective)
        ));
    }
    (!parts.is_empty()).then(|| format!("  ↳ {}", parts.join(" · ")))
}

/// Emit end-of-turn usage and context telemetry. Interactive terminals get one
/// compact, human-readable status line; redirected output retains the stable
/// `kitsu-usage:` and `kitsu-context:` JSON records for automation.
pub fn emit_turn_stats(usage: Option<(u64, u64, u64)>, context: Option<(u64, u64, u64)>) {
    if is_tty() {
        if let Some(line) = human_turn_stats(usage, context) {
            eprintln!("{}", line.dimmed());
        }
        return;
    }
    if let Some((prompt, completion, total)) = usage {
        emit_usage(prompt, completion, total);
    }
    if let Some((used, effective, window)) = context {
        emit_context(used, effective, window);
    }
}

/// Machine-readable context-utilization line to **stderr** (companion to
/// `kitsu-usage:`), emitted per turn so sessions and the dogfood harness can see
/// how full the window is and whether trimming is near.
pub fn emit_context(used: u64, effective: u64, window: u64) {
    let mut err = io::stderr();
    let _ = writeln!(
        err,
        "kitsu-context: {{\"used\":{used},\"effective\":{effective},\"window\":{window}}}"
    );
    let _ = err.flush();
}

/// Render the `/context` breakdown to stderr: model · window · a used/effective
/// bar with headroom · per-category token counts. `note` annotates how the
/// window was resolved (empty to omit).
///
/// Each `rows` entry is `(source, tokens, policy, why)` — the provenance of one
/// context contribution, so the breakdown is honest about *what* is loaded and
/// *why*, not just totals.
pub fn render_context(
    model: &str,
    window: u64,
    note: &str,
    effective: u64,
    used: u64,
    rows: &[(&str, u64, &str, &str)],
) {
    let (r, g, b) = FOX;
    let pct = if effective > 0 {
        (used.saturating_mul(100) / effective).min(999)
    } else {
        0
    };
    let headroom = effective.saturating_sub(used);
    const CELLS: u64 = 16;
    let filled = if effective > 0 {
        (used.saturating_mul(CELLS) / effective).min(CELLS)
    } else {
        CELLS
    };
    let bar: String = (0..CELLS)
        .map(|i| if i < filled { '■' } else { '□' })
        .collect();
    let win = if note.is_empty() {
        format!("{window}")
    } else {
        format!("{window} ({note})")
    };
    eprintln!("model: {model}   window: {win}");
    let bar_disp = if is_tty() {
        format!("{}", bar.truecolor(r, g, b))
    } else {
        bar
    };
    eprintln!("used {used} / {effective} effective  [{bar_disp}]  {pct}%   headroom: {headroom}");
    let header = format!(
        "  {:<20} {:>7}  {:<10}  {}",
        "source", "tokens", "policy", "why"
    );
    if is_tty() {
        eprintln!("{}", header.dimmed());
    } else {
        eprintln!("{header}");
    }
    for (label, tokens, policy, why) in rows {
        if *tokens > 0 || !why.is_empty() {
            eprintln!("  {label:<20} {tokens:>7}  {policy:<10}  {why}");
        }
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

#[cfg(test)]
mod tests {
    use super::{grouped_count, human_turn_stats};

    #[test]
    fn groups_token_counts_for_people() {
        assert_eq!(grouped_count(0), "0");
        assert_eq!(grouped_count(999), "999");
        assert_eq!(grouped_count(1_000), "1,000");
        assert_eq!(grouped_count(123_456_789), "123,456,789");
    }

    #[test]
    fn combines_usage_and_context_into_one_status_line() {
        assert_eq!(
            human_turn_stats(Some((11, 3, 14)), Some((2_000, 8_000, 8_192))),
            Some("  ↳ 11 input · 3 output · 2,000 / 8,000 context (25% used)".into())
        );
    }

    #[test]
    fn omits_status_line_without_stats() {
        assert_eq!(human_turn_stats(None, None), None);
    }
}
