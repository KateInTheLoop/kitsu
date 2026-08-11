//! kitsu-render — terminal rendering. MVP: stream text fragments to stdout.
//! Full UX (palette, code blocks, spinners) is later phases (design §8).

pub mod jsonl;

use std::io::{self, IsTerminal, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

use owo_colors::OwoColorize;

const ESC: char = '\u{1b}';
const CSI: char = '\u{009b}';
const OSC: char = '\u{009d}';
const ST: char = '\u{009c}';
static ASSISTANT_OUTPUT_WRITTEN: AtomicBool = AtomicBool::new(false);
static FORCE_PLAIN: AtomicBool = AtomicBool::new(false);
static FORCE_REDUCED_MOTION: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum EscapeState {
    #[default]
    Ground,
    Escape,
    EscapeIntermediate,
    Csi,
    Osc,
    OscEscape,
    ControlString,
    ControlStringEscape,
}

/// Incremental terminal-control scrubber used by streamed assistant output.
///
/// Escape sequences often span provider chunks, so sanitizing each fragment in
/// isolation is insufficient: `ESC` may arrive in one chunk and `[31m` in the
/// next. The state is retained until [`finish_line`] establishes the next turn
/// boundary.
#[derive(Debug, Default)]
struct TerminalTextSanitizer {
    state: EscapeState,
}

fn is_bidi_format_control(ch: char) -> bool {
    matches!(
        ch,
        // Arabic Letter Mark, left/right marks, embeddings, overrides, and
        // isolates. These characters can make an untrusted model/path/status
        // appear to say something different from its logical byte order.
        '\u{061c}'
            | '\u{200e}'..='\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2066}'..='\u{2069}'
            // Deprecated directional controls are still interpreted by some
            // terminal stacks and should be made visible too.
            | '\u{206a}'..='\u{206f}'
    )
}

fn is_invisible_format_control(ch: char) -> bool {
    matches!(
        ch,
        // Unicode Cf characters which are invisible/zero-width or change how
        // adjacent glyphs are joined. Variation selectors are intentionally
        // not included: they are marks rather than format controls and are
        // needed for ordinary emoji presentation.
        '\u{00ad}'
            | '\u{0600}'..='\u{0605}'
            | '\u{06dd}'
            | '\u{070f}'
            | '\u{0890}'..='\u{0891}'
            | '\u{08e2}'
            | '\u{180e}'
            | '\u{17b4}'..='\u{17b5}'
            | '\u{200b}'..='\u{200d}'
            | '\u{2060}'..='\u{2064}'
            | '\u{feff}'
            | '\u{fff9}'..='\u{fffb}'
            | '\u{110bd}'
            | '\u{110cd}'
            | '\u{13430}'..='\u{1345f}'
            | '\u{1bca0}'..='\u{1bca3}'
            | '\u{1d173}'..='\u{1d17a}'
            | '\u{e0001}'
            | '\u{e0020}'..='\u{e007f}'
    )
}

impl TerminalTextSanitizer {
    fn push(&mut self, input: &str) -> String {
        let mut output = String::with_capacity(input.len());
        for ch in input.chars() {
            match self.state {
                EscapeState::Ground => match ch {
                    ESC => self.state = EscapeState::Escape,
                    CSI => self.state = EscapeState::Csi,
                    OSC => self.state = EscapeState::Osc,
                    // DCS, SOS, PM, and APC control strings. These are less
                    // common than OSC but have the same terminal-injection
                    // properties and are terminated by ST.
                    '\u{0090}' | '\u{0098}' | '\u{009e}' | '\u{009f}' => {
                        self.state = EscapeState::ControlString;
                    }
                    // Do not silently preserve directional controls: an ASCII
                    // marker exposes the exact logical character without
                    // letting a terminal reorder the surrounding text.
                    _ if is_bidi_format_control(ch) => {
                        output.push_str(&format!("[U+{:04X}]", ch as u32));
                    }
                    // Unicode line/paragraph separators are printable scalar
                    // values but several terminal/front-end stacks treat them
                    // as physical row boundaries.
                    '\u{2028}' | '\u{2029}' => {
                        output.push_str(&format!("[U+{:04X}]", ch as u32));
                    }
                    // Other Unicode format controls are invisible and carry no
                    // useful terminal content. Drop them rather than allowing
                    // zero-width names/statuses to spoof visible identifiers.
                    _ if is_invisible_format_control(ch) => {}
                    '\n' | '\t' => output.push(ch),
                    _ if !ch.is_control() => output.push(ch),
                    _ => {}
                },
                EscapeState::Escape => match ch {
                    '[' => self.state = EscapeState::Csi,
                    ']' => self.state = EscapeState::Osc,
                    'P' | 'X' | '^' | '_' => self.state = EscapeState::ControlString,
                    ESC => {}
                    '\u{20}'..='\u{2f}' => self.state = EscapeState::EscapeIntermediate,
                    // A newline or tab is content, not part of a valid
                    // single-character escape sequence. Preserve it while
                    // still discarding the preceding ESC.
                    '\n' | '\t' => {
                        output.push(ch);
                        self.state = EscapeState::Ground;
                    }
                    _ => self.state = EscapeState::Ground,
                },
                EscapeState::EscapeIntermediate => match ch {
                    '\u{20}'..='\u{2f}' => {}
                    ESC => self.state = EscapeState::Escape,
                    '\n' | '\t' => {
                        output.push(ch);
                        self.state = EscapeState::Ground;
                    }
                    _ => self.state = EscapeState::Ground,
                },
                EscapeState::Csi => match ch {
                    '\u{40}'..='\u{7e}' => self.state = EscapeState::Ground,
                    ESC => self.state = EscapeState::Escape,
                    '\n' | '\t' => output.push(ch),
                    _ => {}
                },
                EscapeState::Osc => match ch {
                    '\u{07}' | ST => self.state = EscapeState::Ground,
                    ESC => self.state = EscapeState::OscEscape,
                    _ => {}
                },
                EscapeState::OscEscape => match ch {
                    '\\' | '\u{07}' | ST => self.state = EscapeState::Ground,
                    ESC => {}
                    _ => self.state = EscapeState::Osc,
                },
                EscapeState::ControlString => match ch {
                    ST => self.state = EscapeState::Ground,
                    ESC => self.state = EscapeState::ControlStringEscape,
                    _ => {}
                },
                EscapeState::ControlStringEscape => match ch {
                    '\\' | ST => self.state = EscapeState::Ground,
                    ESC => {}
                    _ => self.state = EscapeState::ControlString,
                },
            }
        }
        output
    }

    fn reset(&mut self) {
        self.state = EscapeState::Ground;
    }
}

fn stream_sanitizer() -> &'static Mutex<TerminalTextSanitizer> {
    static SANITIZER: OnceLock<Mutex<TerminalTextSanitizer>> = OnceLock::new();
    SANITIZER.get_or_init(|| Mutex::new(TerminalTextSanitizer::default()))
}

/// Remove terminal control sequences and unsafe control characters from
/// untrusted text while preserving printable Unicode, newlines, and tabs.
///
/// CSI/SGR sequences, OSC hyperlinks/titles, other ECMA-48 control strings,
/// C0/C1 controls, carriage returns, and DEL are removed. This function treats
/// `input` as a complete message; [`print_fragment`] additionally carries
/// parser state across streamed chunks.
pub fn sanitize_terminal_text(input: &str) -> String {
    TerminalTextSanitizer::default().push(input)
}

fn sanitize_terminal_inline(input: &str) -> String {
    sanitize_terminal_text(input)
        .chars()
        .map(|ch| if matches!(ch, '\n' | '\t') { ' ' } else { ch })
        .collect()
}

fn ascii_structural_markers(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '·' => output.push('|'),
            '—' | '–' | '─' | '□' => output.push('-'),
            '│' => output.push('|'),
            '■' => output.push('#'),
            '↳' => output.push_str("->"),
            '…' => output.push_str("..."),
            '✻' | '⏺' => output.push('*'),
            '⎿' => output.push('>'),
            '╭' | '╮' | '╰' | '╯' => output.push('+'),
            _ => output.push(ch),
        }
    }
    output
}

fn sanitize_plain_inline(input: &str) -> String {
    ascii_structural_markers(&sanitize_terminal_inline(input))
}

/// Cached at startup: is stdout a TTY?
pub fn is_tty() -> bool {
    io::stdout().is_terminal()
}

fn stderr_is_tty() -> bool {
    io::stderr().is_terminal()
}

/// Whether terminal decoration should be replaced with stable ASCII output.
pub fn plain_mode() -> bool {
    FORCE_PLAIN.load(Ordering::Relaxed)
        || std::env::var_os("KITSU_PLAIN").is_some()
        || std::env::var_os("NO_COLOR").is_some()
        || std::env::var("TERM").is_ok_and(|term| term == "dumb")
}

/// Configure process-wide terminal accessibility overrides before worker
/// threads or renderers are started. Environment variables remain supported;
/// these flags make the same behavior discoverable from the CLI.
pub fn configure_terminal_modes(plain: bool, reduced_motion: bool) {
    FORCE_PLAIN.store(plain, Ordering::Relaxed);
    FORCE_REDUCED_MOTION.store(reduced_motion, Ordering::Relaxed);
}

fn reduced_motion_mode() -> bool {
    FORCE_REDUCED_MOTION.load(Ordering::Relaxed)
        || std::env::var_os("KITSU_REDUCED_MOTION").is_some()
}

fn stderr_style() -> bool {
    stderr_is_tty() && !plain_mode()
}

fn stdout_style() -> bool {
    is_tty() && !plain_mode()
}

/// Is stdin a TTY? Chat uses this to decide interactive loop vs one-shot.
pub fn stdin_is_tty() -> bool {
    io::stdin().is_terminal()
}

/// Print a streamed text fragment immediately (no newline, flush).
pub fn print_fragment(s: &str) {
    let sanitized = stream_sanitizer()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .push(s);
    if !sanitized.is_empty() {
        ASSISTANT_OUTPUT_WRITTEN.store(true, Ordering::Release);
    }
    let mut out = io::stdout();
    let _ = out.write_all(sanitized.as_bytes());
    let _ = out.flush();
}

/// Render a complete assistant message with lightweight terminal Markdown and
/// diff styling. Markdown markers are retained so buffered and streaming output
/// have the same structure once terminal styling is stripped.
pub fn print_message(s: &str) {
    let sanitized = sanitize_terminal_text(s);
    let rendered = format_message(&sanitized, stdout_style());
    if !rendered.is_empty() {
        ASSISTANT_OUTPUT_WRITTEN.store(true, Ordering::Release);
    }
    let mut out = io::stdout();
    let _ = out.write_all(rendered.as_bytes());
    let _ = out.flush();
}

fn format_message(s: &str, styled: bool) -> String {
    if !styled {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut fence: Option<String> = None;
    for segment in s.split_inclusive('\n') {
        let line = segment.trim_end_matches('\n');
        let newline = if segment.ends_with('\n') { "\n" } else { "" };
        if let Some(language) = line.trim().strip_prefix("```") {
            if fence.is_some() {
                fence = None;
            } else {
                fence = Some(language.trim().to_string());
            }
            out.push_str(&format!("{}", line.dimmed()));
            out.push_str(newline);
            continue;
        }
        let rendered = if fence.as_deref() == Some("diff") || fence.as_deref() == Some("patch") {
            if line.starts_with('+') && !line.starts_with("+++") {
                format!("{}", line.green())
            } else if line.starts_with('-') && !line.starts_with("---") {
                format!("{}", line.red())
            } else if line.starts_with("@@") {
                format!("{}", line.cyan())
            } else {
                line.to_string()
            }
        } else if line.starts_with("### ") || line.starts_with("## ") || line.starts_with("# ") {
            format!("{}", line.bold())
        } else {
            line.to_string()
        };
        out.push_str(&rendered);
        out.push_str(newline);
    }
    out
}

/// Fox-orange (kitsune) accent colour used across the UI.
const FOX: (u8, u8, u8) = (255, 140, 0);

/// A Codex-style startup banner with kitsu's fox/cat mascot and a Japanese
/// accent (狐 = kitsune). Printed to stderr at chat start. On a non-TTY it
/// degrades to a single plain line so piped output stays clean.
pub fn chat_banner(gateway: &str, model: &str, dir: &str, tools: &[String]) {
    let plain = plain_mode();
    let clean = |value: &str| {
        if plain {
            sanitize_plain_inline(value)
        } else {
            sanitize_terminal_inline(value)
        }
    };
    let gateway = clean(gateway);
    let model = clean(model);
    let dir = clean(dir);
    let tools: Vec<String> = tools.iter().map(|tool| clean(tool)).collect();
    if plain {
        eprintln!(
            "kitsu - gateway: {gateway} | model: {model} | directory: {dir} | tools: {}",
            tools.len()
        );
        return;
    }
    if !stderr_is_tty() {
        eprintln!(
            "kitsu — gateway: {gateway} · model: {model} · directory: {dir} · tools: {}",
            tools.len()
        );
        return;
    }
    let (r, g, b) = FOX;
    let bar = "│".truecolor(r, g, b);
    let rule: String = "─".repeat(42);
    let tools_str = if tools.is_empty() {
        "none (read-only chat)".to_string()
    } else if tools.join(", ").chars().count() > 64 {
        format!("{} enabled (use /tools)", tools.len())
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
    row("gateway", &gateway);
    row("model", &model);
    row("directory", &dir);
    row("tools", &tools_str);
    eprintln!("  {}", format!("╰{rule}────").truecolor(r, g, b));
    eprintln!(
        "  {}",
        "Enter / for commands · Tab completes prefixes · Ctrl+D exits".dimmed()
    );
    eprintln!();
}

/// The interactive chat prompt. Fox-orange on a TTY, plain otherwise.
pub fn prompt_string() -> String {
    if stdout_style() {
        format!("{} ", "kitsu>".truecolor(255, 140, 0).bold())
    } else {
        "kitsu> ".to_string()
    }
}

/// Severity of a notice produced while an interactive input prompt may be
/// visible. Markers are deliberately ASCII in every mode.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AsyncNoticeLevel {
    Info,
    Success,
    Warning,
    Error,
}

/// Sanitized, complete-line payload for a redraw-capable prompt printer.
///
/// Constructing a notice does not write to the terminal. Interactive callers
/// pass [`Self::prompt_frame`] to their editor's external-printer/redraw API in
/// both plain and styled modes; this avoids an asynchronous `eprintln!` cutting
/// through the user's current input buffer. Non-interactive callers can use
/// [`Self::write_line`] between prompts.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PromptSafeAsyncNotice {
    level: AsyncNoticeLevel,
    message: String,
}

impl PromptSafeAsyncNotice {
    pub fn new(level: AsyncNoticeLevel, message: impl Into<String>) -> Self {
        Self {
            level,
            message: message.into(),
        }
    }

    pub fn info(message: impl Into<String>) -> Self {
        Self::new(AsyncNoticeLevel::Info, message)
    }

    pub fn success(message: impl Into<String>) -> Self {
        Self::new(AsyncNoticeLevel::Success, message)
    }

    pub fn warning(message: impl Into<String>) -> Self {
        Self::new(AsyncNoticeLevel::Warning, message)
    }

    pub fn error(message: impl Into<String>) -> Self {
        Self::new(AsyncNoticeLevel::Error, message)
    }

    pub fn level(&self) -> AsyncNoticeLevel {
        self.level
    }

    /// One sanitized physical line using the current stderr/plain mode.
    pub fn line(&self) -> String {
        self.line_with_mode(stderr_style(), plain_mode())
    }

    /// Payload expected by a redraw-capable external printer. The surrounding
    /// newlines delimit the notice; the printer owns prompt restoration.
    pub fn prompt_frame(&self) -> String {
        format!("\n{}\n", self.line())
    }

    /// Deliver through an editor-specific redraw callback without coupling the
    /// renderer to a particular line-editing crate.
    pub fn publish_with<E>(
        &self,
        mut redraw: impl FnMut(String) -> Result<(), E>,
    ) -> Result<(), E> {
        redraw(self.prompt_frame())
    }

    /// Write a complete sanitized line where no interactive prompt is live.
    pub fn write_line<W: Write>(&self, destination: &mut W) -> io::Result<()> {
        self.write_line_with_mode(destination, stderr_style(), plain_mode())
    }

    /// Deterministic destination-aware variant for adapters and tests.
    pub fn write_line_with_mode<W: Write>(
        &self,
        destination: &mut W,
        styled: bool,
        plain: bool,
    ) -> io::Result<()> {
        writeln!(destination, "{}", self.line_with_mode(styled, plain))?;
        destination.flush()
    }

    fn line_with_mode(&self, styled: bool, plain: bool) -> String {
        let message = if plain {
            sanitize_plain_inline(&self.message)
        } else {
            sanitize_terminal_inline(&self.message)
        };
        let (marker, label) = match self.level {
            AsyncNoticeLevel::Info => ("~", ""),
            AsyncNoticeLevel::Success => ("+", ""),
            AsyncNoticeLevel::Warning => ("!", "warning: "),
            AsyncNoticeLevel::Error => ("!", "error: "),
        };
        let line = format!("{marker} {label}{message}");
        if !styled || plain {
            return line;
        }
        match self.level {
            AsyncNoticeLevel::Info => format!("{}", line.truecolor(255, 140, 0)),
            AsyncNoticeLevel::Success => format!("{}", line.green()),
            AsyncNoticeLevel::Warning => format!("{}", line.yellow()),
            AsyncNoticeLevel::Error => format!("{}", line.red()),
        }
    }
}

/// A "vibing kitsu" spinner shown on stderr while the model is working (the wait
/// before the first token). Animates a cat/fox kaomoji in fox-orange on a TTY.
/// A redirected/piped stderr gets a periodic plain-text heartbeat instead of
/// nothing, so a slow-but-working scripted run against a local model is
/// distinguishable from a hung one. Plain and reduced-motion modes show a
/// static status. Stops (and erases an animated/styled-static line) on
/// `stop()` or drop.
pub struct Spinner {
    handle: Option<tokio::task::JoinHandle<()>>,
    clear_line: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SpinnerMode {
    /// Redirected/piped stderr (`--script`, non-interactive): no cursor
    /// tricks, but a slow-but-working scripted run against a local model can
    /// sit silent for minutes with zero indication it hasn't hung — so print
    /// a periodic plain heartbeat line instead of nothing at all.
    Heartbeat,
    StaticPlain,
    StaticStyled,
    Animated,
}

fn spinner_mode(destination_is_tty: bool, plain: bool, reduced_motion: bool) -> SpinnerMode {
    if !destination_is_tty {
        SpinnerMode::Heartbeat
    } else if plain {
        SpinnerMode::StaticPlain
    } else if reduced_motion {
        SpinnerMode::StaticStyled
    } else {
        SpinnerMode::Animated
    }
}

fn static_spinner_line(styled: bool) -> String {
    if styled {
        let (r, g, b) = FOX;
        format!(
            "  {} {}",
            "*".truecolor(r, g, b),
            "kitsu is thinking".dimmed()
        )
    } else {
        "* kitsu is thinking".to_string()
    }
}

fn write_static_spinner<W: Write>(destination: &mut W, styled: bool) -> io::Result<()> {
    writeln!(destination, "{}", static_spinner_line(styled))?;
    destination.flush()
}

impl Spinner {
    /// Start the animation (must be called within a tokio runtime). Returns an
    /// inert handle on a non-TTY. Plain and reduced-motion terminals get a
    /// visible static status instead of losing progress feedback entirely.
    pub fn start() -> Self {
        let mode = spinner_mode(stderr_is_tty(), plain_mode(), reduced_motion_mode());
        match mode {
            SpinnerMode::Heartbeat => {
                let handle = tokio::spawn(async move {
                    // Long enough that a normal request never logs anything;
                    // only a genuinely slow round (the exact case with no other
                    // progress signal — heavy tool-calling on a local model)
                    // does. Repeats so a multi-minute wait doesn't go silent.
                    const FIRST_BEAT: std::time::Duration = std::time::Duration::from_secs(10);
                    const INTERVAL: std::time::Duration = std::time::Duration::from_secs(15);
                    tokio::time::sleep(FIRST_BEAT).await;
                    let mut waited = FIRST_BEAT;
                    loop {
                        eprintln!("~ still waiting on the model ({}s)...", waited.as_secs());
                        tokio::time::sleep(INTERVAL).await;
                        waited += INTERVAL;
                    }
                });
                return Self {
                    handle: Some(handle),
                    clear_line: false,
                };
            }
            SpinnerMode::StaticPlain => {
                // A newline keeps plain mode escape-free; unlike a cursor-
                // controlled status this line intentionally remains in the
                // transcript when the response starts.
                let _ = write_static_spinner(&mut io::stderr(), false);
                return Self {
                    handle: None,
                    clear_line: false,
                };
            }
            SpinnerMode::StaticStyled => {
                // Reduced-motion output is a complete physical line. Leaving a
                // carriage-return status pending allowed an asynchronous
                // provider/job notice to merge into it before `clear()` ran.
                // A newline is stable for screen readers and cannot be spliced
                // with the next notice.
                let _ = write_static_spinner(&mut io::stderr(), true);
                return Self {
                    handle: None,
                    clear_line: false,
                };
            }
            SpinnerMode::Animated => {}
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
            // Avoid a distracting flash for fast local responses.
            tokio::time::sleep(std::time::Duration::from_millis(180)).await;
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
            clear_line: true,
        }
    }

    /// Stop progress feedback. Cursor-controlled lines are erased; an
    /// escape-free plain static line remains in the transcript.
    pub fn stop(mut self) {
        self.clear();
    }

    fn clear(&mut self) {
        if let Some(h) = self.handle.take() {
            h.abort();
        }
        if std::mem::take(&mut self.clear_line) {
            eprint!("\r\x1b[2K");
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
    stream_sanitizer()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .reset();
    if !ASSISTANT_OUTPUT_WRITTEN.swap(false, Ordering::AcqRel) {
        return;
    }
    let mut out = io::stdout();
    let _ = out.write_all(b"\n");
    let _ = out.flush();
}

/// Announce a tool call to **stderr** (meta, not assistant text) as a compact,
/// friendly line — e.g. `⏺ Read(deck1.md)`. Fox-orange marker on a TTY.
pub fn tool_activity(header: &str) {
    let plain = plain_mode();
    let header = if plain {
        sanitize_plain_inline(header)
    } else {
        sanitize_terminal_inline(header)
    };
    if stderr_is_tty() && !plain {
        eprintln!("{} {header}", "⏺".truecolor(255, 140, 0));
    } else if plain {
        eprintln!("tool> {header}");
    } else {
        eprintln!("⏺ {header}");
    }
}

/// The result summary line under a tool call — e.g. `  ⎿ 128 lines`. Dimmed on a
/// TTY. Goes to stderr so stdout stays assistant-text only.
pub fn tool_result(summary: &str) {
    let plain = plain_mode();
    let summary = if plain {
        sanitize_plain_inline(summary)
    } else {
        sanitize_terminal_inline(summary)
    };
    let line = format!("  ⎿ {summary}");
    if stderr_is_tty() && !plain {
        eprintln!("{}", line.dimmed());
    } else if plain {
        eprintln!("result> {summary}");
    } else {
        eprintln!("{line}");
    }
}

/// Render a verified workspace diff to stderr. This output is generated from
/// Git/filesystem state, not from assistant prose.
pub fn render_diff(diff: &str) {
    let diff = sanitize_terminal_text(diff);
    let line_count = diff.lines().count();
    eprintln!("verified workspace diff ({line_count} line(s)):");
    // Keep an interactive prompt usable after a repository-wide diff. Scripts
    // and redirected captures still receive the full bounded evidence; people
    // can narrow an interactive view with `/diff <path>`.
    let display_limit = if stderr_is_tty() { 240 } else { usize::MAX };
    for line in diff.lines().take(display_limit) {
        if stderr_style() {
            if line.starts_with('+') && !line.starts_with("+++") {
                eprintln!("{}", line.green());
            } else if line.starts_with('-') && !line.starts_with("---") {
                eprintln!("{}", line.red());
            } else if line.starts_with("@@") {
                eprintln!("{}", line.cyan());
            } else if line.starts_with("diff --git") || line.starts_with("new file") {
                eprintln!("{}", line.bold());
            } else {
                eprintln!("{line}");
            }
        } else {
            eprintln!("{line}");
        }
    }
    if line_count > display_limit {
        eprintln!(
            "... {} line(s) omitted from the terminal view; use /diff <path> to narrow it",
            line_count - display_limit
        );
    }
}

/// Announce a summarizing compaction to stderr, like Claude Code's boundary, so
/// the user knows older history was replaced by a summary.
pub fn compaction_boundary() {
    if plain_mode() {
        eprintln!("* context compacted (older turns summarized)");
    } else if stderr_style() {
        let (r, g, b) = FOX;
        eprintln!(
            "{}",
            "✻ context compacted (older turns summarized)".truecolor(r, g, b)
        );
    } else {
        eprintln!("✻ context compacted (older turns summarized)");
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
    ascii: bool,
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
    (!parts.is_empty()).then(|| {
        if ascii {
            format!("  -> {}", parts.join(" | "))
        } else {
            format!("  ↳ {}", parts.join(" · "))
        }
    })
}

/// Emit end-of-turn usage and context telemetry. Interactive terminals get one
/// compact, human-readable status line; redirected output retains the stable
/// `kitsu-usage:` and `kitsu-context:` JSON records for automation. Formatting
/// follows stderr's TTY state because stderr is the destination.
pub fn emit_turn_stats(usage: Option<(u64, u64, u64)>, context: Option<(u64, u64, u64)>) {
    let mut err = io::stderr();
    let destination_is_tty = err.is_terminal();
    let _ = write_turn_stats(&mut err, destination_is_tty, usage, context);
}

/// Write end-of-turn telemetry to an explicit destination.
///
/// Callers that route telemetry somewhere other than stderr can pass that
/// destination's real TTY state. This avoids accidentally choosing human versus
/// machine formatting from an unrelated stream such as stdout.
pub fn write_turn_stats<W: Write>(
    destination: &mut W,
    destination_is_tty: bool,
    usage: Option<(u64, u64, u64)>,
    context: Option<(u64, u64, u64)>,
) -> io::Result<()> {
    write_turn_stats_with_mode(
        destination,
        destination_is_tty,
        plain_mode(),
        usage,
        context,
    )
}

fn write_turn_stats_with_mode<W: Write>(
    destination: &mut W,
    destination_is_tty: bool,
    plain: bool,
    usage: Option<(u64, u64, u64)>,
    context: Option<(u64, u64, u64)>,
) -> io::Result<()> {
    if destination_is_tty {
        if let Some(line) = human_turn_stats(usage, context, plain) {
            if plain {
                writeln!(destination, "{line}")?;
            } else {
                writeln!(destination, "{}", line.dimmed())?;
            }
        }
    } else {
        if let Some((prompt, completion, total)) = usage {
            write_usage(destination, prompt, completion, total)?;
        }
        if let Some((used, effective, window)) = context {
            write_context_record(destination, used, effective, window)?;
        }
    }
    destination.flush()
}

fn write_context_record<W: Write>(
    destination: &mut W,
    used: u64,
    effective: u64,
    window: u64,
) -> io::Result<()> {
    writeln!(
        destination,
        "kitsu-context: {{\"used\":{used},\"effective\":{effective},\"window\":{window}}}"
    )
}

fn write_usage<W: Write>(
    destination: &mut W,
    prompt_tokens: u64,
    completion_tokens: u64,
    total_tokens: u64,
) -> io::Result<()> {
    writeln!(
        destination,
        "kitsu-usage: {{\"prompt_tokens\":{prompt_tokens},\"completion_tokens\":{completion_tokens},\"total_tokens\":{total_tokens}}}"
    )
}

/// Machine-readable context-utilization line to **stderr** (companion to
/// `kitsu-usage:`), emitted per turn so sessions and the dogfood harness can see
/// how full the window is and whether trimming is near.
pub fn emit_context(used: u64, effective: u64, window: u64) {
    let mut err = io::stderr();
    let _ = write_context_record(&mut err, used, effective, window);
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
    let plain = plain_mode();
    let lines = context_lines(
        model,
        window,
        note,
        effective,
        used,
        rows,
        RenderMode {
            styled: stderr_is_tty() && !plain,
            plain,
        },
    );
    for line in lines {
        eprintln!("{line}");
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RenderMode {
    styled: bool,
    plain: bool,
}

fn context_lines(
    model: &str,
    window: u64,
    note: &str,
    effective: u64,
    used: u64,
    rows: &[(&str, u64, &str, &str)],
    mode: RenderMode,
) -> Vec<String> {
    let (r, g, b) = FOX;
    let clean = |value: &str| {
        if mode.plain {
            sanitize_plain_inline(value)
        } else {
            sanitize_terminal_inline(value)
        }
    };
    let model = clean(model);
    let note = clean(note);
    let pct = if effective > 0 {
        (used.saturating_mul(100) / effective).min(999)
    } else {
        0
    };
    let headroom = effective.saturating_sub(used);
    const CELLS: u64 = 16;
    let filled = if effective > 0 {
        let cells = (used.saturating_mul(CELLS) / effective).min(CELLS);
        if used > 0 && cells == 0 {
            1
        } else {
            cells
        }
    } else {
        CELLS
    };
    let (filled_cell, empty_cell) = if mode.plain {
        ('#', '-')
    } else {
        ('■', '□')
    };
    let bar: String = (0..CELLS)
        .map(|i| if i < filled { filled_cell } else { empty_cell })
        .collect();
    let win = if note.is_empty() {
        format!("{window}")
    } else {
        format!("{window} ({note})")
    };
    let mut lines = vec![format!("model: {model}   window: {win}")];
    let bar_disp = if mode.styled {
        format!("{}", bar.truecolor(r, g, b))
    } else {
        bar
    };
    lines.push(format!(
        "used {used} / {effective} effective  [{bar_disp}]  {pct}%   headroom: {headroom}"
    ));
    let header = format!(
        "  {:<20} {:>7}  {:<10}  {}",
        "source", "tokens", "policy", "why"
    );
    if mode.styled {
        lines.push(format!("{}", header.dimmed()));
    } else {
        lines.push(header);
    }
    for (label, tokens, policy, why) in rows {
        if *tokens > 0 || !why.is_empty() {
            let label = clean(label);
            let policy = clean(policy);
            let why = clean(why);
            lines.push(format!("  {label:<20} {tokens:>7}  {policy:<10}  {why}"));
        }
    }
    lines
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
    let _ = write_usage(&mut err, prompt_tokens, completion_tokens, total_tokens);
    let _ = err.flush();
}

#[cfg(test)]
mod tests {
    use super::{
        context_lines, format_message, grouped_count, human_turn_stats, sanitize_plain_inline,
        sanitize_terminal_inline, sanitize_terminal_text, spinner_mode, static_spinner_line,
        write_static_spinner, write_turn_stats, write_turn_stats_with_mode, AsyncNoticeLevel,
        PromptSafeAsyncNotice, RenderMode, SpinnerMode, TerminalTextSanitizer,
    };

    #[test]
    fn strips_terminal_sequences_and_unsafe_controls() {
        let input = concat!(
            "safe",
            "\x1b[31m",
            "red",
            "\x1b[0m",
            "\x1b]8;;https://example.test\x1b\\",
            "link",
            "\x1b]8;;\x07",
            "\0\r\x08\x7f",
            "\tline\n",
            "\u{009b}2J",
            "done",
            "\u{009d}title\u{009c}",
            "!",
            "\x1bPprivate payload\x1b\\",
            "after"
        );

        assert_eq!(
            sanitize_terminal_text(input),
            "saferedlink\tline\ndone!after"
        );
    }

    #[test]
    fn preserves_printable_unicode_newlines_and_tabs() {
        assert_eq!(
            sanitize_terminal_text("狐 says:\tこんにちは\n🦊"),
            "狐 says:\tこんにちは\n🦊"
        );
    }

    #[test]
    fn exposes_bidi_controls_and_removes_invisible_format_controls() {
        let input = "model-\u{202e}gpj.exe\u{202c}-\u{200b}safe\u{200d}\u{2028}row";
        assert_eq!(
            sanitize_terminal_text(input),
            "model-[U+202E]gpj.exe[U+202C]-safe[U+2028]row"
        );
        assert!(!sanitize_terminal_text(input).contains('\u{202e}'));
        assert!(!sanitize_terminal_text(input).contains('\u{200b}'));
    }

    #[test]
    fn inline_terminal_fields_cannot_create_extra_rows() {
        assert_eq!(
            sanitize_terminal_inline("model\nspoofed\trow\x1b[2J"),
            "model spoofed row"
        );
    }

    #[test]
    fn async_notice_is_one_sanitized_line_in_plain_and_styled_modes() {
        let notice =
            PromptSafeAsyncNotice::warning("agent done\nforged row\tfield\u{1b}]0;owned\u{7}");
        let plain = notice.line_with_mode(false, true);
        assert_eq!(plain, "! warning: agent done forged row field");
        assert!(!plain.contains('\n'));
        assert!(!plain.contains('\u{1b}'));

        let styled = notice.line_with_mode(true, false);
        assert!(styled.contains('\u{1b}'));
        assert_eq!(
            sanitize_terminal_text(&styled),
            "! warning: agent done forged row field"
        );
        assert_eq!(notice.level(), AsyncNoticeLevel::Warning);
    }

    #[test]
    fn async_notice_external_frame_is_delivered_as_one_redraw_operation() {
        let notice = PromptSafeAsyncNotice::info("background agent finished");
        let mut delivered = Vec::new();
        notice
            .publish_with(|frame| {
                delivered.push(frame);
                Ok::<(), std::convert::Infallible>(())
            })
            .unwrap();
        assert_eq!(delivered.len(), 1);
        assert!(delivered[0].starts_with('\n'));
        assert!(delivered[0].ends_with('\n'));
        assert_eq!(delivered[0].lines().count(), 2);

        let mut raw = Vec::new();
        PromptSafeAsyncNotice::error("failed")
            .write_line_with_mode(&mut raw, false, true)
            .unwrap();
        assert_eq!(String::from_utf8(raw).unwrap(), "! error: failed\n");
    }

    #[test]
    fn plain_inline_metadata_replaces_unicode_structural_markers() {
        assert_eq!(
            sanitize_plain_inline("written · 42 bytes — done … ↳ detail"),
            "written | 42 bytes - done ... -> detail"
        );
    }

    #[test]
    fn streaming_sanitizer_tracks_split_csi_and_osc_sequences() {
        let mut sanitizer = TerminalTextSanitizer::default();

        assert_eq!(sanitizer.push("before\x1b"), "before");
        assert_eq!(sanitizer.push("[31"), "");
        assert_eq!(sanitizer.push("mred\x1b]"), "red");
        assert_eq!(sanitizer.push("0;owned\x1b"), "");
        assert_eq!(sanitizer.push("\\after"), "after");
    }

    #[test]
    fn reset_ends_an_unterminated_streamed_control_string() {
        let mut sanitizer = TerminalTextSanitizer::default();
        assert_eq!(sanitizer.push("shown\x1b]unterminated"), "shown");
        assert_eq!(sanitizer.push("still hidden"), "");

        sanitizer.reset();
        assert_eq!(sanitizer.push("next turn"), "next turn");
    }

    #[test]
    fn buffered_markdown_retains_streaming_structure() {
        let message = "# Heading\n\n```diff\n+added\n-removed\n@@ hunk\n```\n";
        let rendered = format_message(message, true);

        assert!(rendered.contains('\x1b'), "TTY rendering should be styled");
        assert_eq!(sanitize_terminal_text(&rendered), message);
        assert_eq!(format_message(message, false), message);
    }

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
            human_turn_stats(Some((11, 3, 14)), Some((2_000, 8_000, 8_192)), false),
            Some("  ↳ 11 input · 3 output · 2,000 / 8,000 context (25% used)".into())
        );
    }

    #[test]
    fn omits_status_line_without_stats() {
        assert_eq!(human_turn_stats(None, None, false), None);
    }

    #[test]
    fn telemetry_uses_the_explicit_destination_tty_state() {
        let mut redirected = Vec::new();
        write_turn_stats(
            &mut redirected,
            false,
            Some((11, 3, 14)),
            Some((2_000, 8_000, 8_192)),
        )
        .unwrap();
        assert_eq!(
            String::from_utf8(redirected).unwrap(),
            concat!(
                "kitsu-usage: {\"prompt_tokens\":11,\"completion_tokens\":3,\"total_tokens\":14}\n",
                "kitsu-context: {\"used\":2000,\"effective\":8000,\"window\":8192}\n"
            )
        );

        let mut terminal = Vec::new();
        write_turn_stats_with_mode(
            &mut terminal,
            true,
            true,
            Some((11, 3, 14)),
            Some((2_000, 8_000, 8_192)),
        )
        .unwrap();
        let terminal = String::from_utf8(terminal).unwrap();
        assert_eq!(
            terminal,
            "  -> 11 input | 3 output | 2,000 / 8,000 context (25% used)\n"
        );
        assert!(terminal.is_ascii());
        assert!(!terminal.contains('\x1b'));
    }

    #[test]
    fn plain_context_uses_ascii_markers_and_scrubs_dynamic_fields() {
        let rows = [("system\x1b[31m", 4, "keep\x1b]0;title\x07", "required\r")];
        let output = context_lines(
            "model\x1b[2J",
            16,
            "known\x1b[0m",
            16,
            4,
            &rows,
            RenderMode {
                styled: false,
                plain: true,
            },
        )
        .join("\n");

        assert!(output.is_ascii(), "{output:?}");
        assert!(!output.contains('\x1b'));
        assert!(output.contains("model: model   window: 16 (known)"));
        assert!(output.contains("[####------------]"));
        assert!(output.contains("system"));
        assert!(output.contains("keep"));
        assert!(output.contains("required"));
    }

    #[test]
    fn reduced_motion_selects_a_visible_static_status() {
        assert_eq!(spinner_mode(false, false, true), SpinnerMode::Heartbeat);
        assert_eq!(spinner_mode(true, false, true), SpinnerMode::StaticStyled);
        assert_eq!(spinner_mode(true, true, true), SpinnerMode::StaticPlain);
        assert_eq!(spinner_mode(true, true, false), SpinnerMode::StaticPlain);
        assert_eq!(spinner_mode(true, false, false), SpinnerMode::Animated);

        // A redirected/piped stderr (--script, non-interactive) always gets the
        // plain heartbeat, regardless of --plain/reduced-motion — there's no
        // cursor to control either way, so those flags don't apply.
        assert_eq!(spinner_mode(false, true, false), SpinnerMode::Heartbeat);
        assert_eq!(spinner_mode(false, false, false), SpinnerMode::Heartbeat);

        let plain_status = static_spinner_line(false);
        assert_eq!(plain_status, "* kitsu is thinking");
        assert!(plain_status.is_ascii());
        assert!(!plain_status.contains('\x1b'));

        let styled_status = static_spinner_line(true);
        assert_eq!(
            sanitize_terminal_text(&styled_status),
            "  * kitsu is thinking"
        );
        // `Spinner::start` emits both static modes with `eprintln!`; unlike an
        // animated cursor line, neither remains pending for a notice to merge.
        assert!(matches!(
            spinner_mode(true, false, true),
            SpinnerMode::StaticStyled
        ));
        let mut output = Vec::new();
        write_static_spinner(&mut output, true).unwrap();
        assert_eq!(
            sanitize_terminal_text(std::str::from_utf8(&output).unwrap()),
            "  * kitsu is thinking\n"
        );
        assert!(!output.contains(&b'\r'));
    }
}
