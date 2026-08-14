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
static TURN_ASSISTANT_OUTPUT: AtomicBool = AtomicBool::new(false);
static FORCE_PLAIN: AtomicBool = AtomicBool::new(false);
static FORCE_SCREEN_READER: AtomicBool = AtomicBool::new(false);
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
            '✻' | '⏺' | '◆' | '⛩' => output.push('*'),
            '⎿' | '◇' => output.push('>'),
            '◉' => output.push('o'),
            '◈' => output.push('#'),
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
        || FORCE_SCREEN_READER.load(Ordering::Relaxed)
        || std::env::var_os("KITSU_PLAIN").is_some()
        || std::env::var_os("KITSU_SCREEN_READER").is_some()
        || std::env::var_os("NO_COLOR").is_some()
        || std::env::var("TERM").is_ok_and(|term| term == "dumb")
}

/// Whether interactive input should use Kitsu's literal, line-oriented reader.
///
/// This is deliberately narrower than [`plain_mode`]: `NO_COLOR` controls
/// rendering only and must not silently remove Rustyline history, completion,
/// or bracketed multi-line paste. Explicit plain/screen-reader modes and a
/// `dumb` terminal retain the escape-free canonical input contract.
pub fn assistive_input_mode() -> bool {
    FORCE_PLAIN.load(Ordering::Relaxed)
        || FORCE_SCREEN_READER.load(Ordering::Relaxed)
        || std::env::var_os("KITSU_PLAIN").is_some()
        || std::env::var_os("KITSU_SCREEN_READER").is_some()
        || std::env::var("TERM").is_ok_and(|term| term == "dumb")
}

/// Configure process-wide terminal accessibility overrides before worker
/// threads or renderers are started. Environment variables remain supported;
/// these flags make the same behavior discoverable from the CLI.
pub fn configure_terminal_modes(plain: bool, reduced_motion: bool, screen_reader: bool) {
    FORCE_PLAIN.store(plain, Ordering::Relaxed);
    FORCE_REDUCED_MOTION.store(reduced_motion, Ordering::Relaxed);
    FORCE_SCREEN_READER.store(screen_reader, Ordering::Relaxed);
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
        TURN_ASSISTANT_OUTPUT.store(true, Ordering::Release);
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
        TURN_ASSISTANT_OUTPUT.store(true, Ordering::Release);
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

/// Named kitsune palette (design.md §8, docs/ux-design.md). Truecolor RGB,
/// used only when the destination is a styled TTY.
/// Fox-orange identity accent used across the UI.
pub const FOX: (u8, u8, u8) = (255, 140, 0);
/// Warnings and high (>=80%) context utilization.
pub const AMBER: (u8, u8, u8) = (255, 191, 0);
/// Highlight text.
pub const CREAM: (u8, u8, u8) = (255, 253, 240);
/// Informational notices and links.
pub const TEAL: (u8, u8, u8) = (54, 117, 136);
/// Success notices.
pub const SAGE: (u8, u8, u8) = (155, 175, 131);
/// Errors and critical (>=95%) context utilization.
pub const EMBER: (u8, u8, u8) = (228, 87, 46);

/// Kitsu's own structural glyph set (design.md §8). Defined once here so every
/// surface uses the same symbols; `--plain` maps each to stable ASCII via
/// [`ascii_structural_markers`].
/// Tool call marker (`--plain`: the `tool>` prefix).
pub const GLYPH_TOOL_CALL: &str = "◆";
/// Tool result marker (`--plain`: the `result>` prefix).
pub const GLYPH_TOOL_RESULT: &str = "◇";
/// Approval/permission prompt marker (`--plain`: `o`).
pub const GLYPH_APPROVAL: &str = "◉";
/// Receipt/section marker (`--plain`: `#`).
pub const GLYPH_RECEIPT: &str = "◈";
/// Torii session-boundary milestone marker (`--plain`: `*`).
pub const GLYPH_TORII: &str = "⛩";

/// Approximate terminal display width of one char: 2 for CJK/emoji ranges the
/// banner and its dynamic values can reasonably contain, else 1. Control and
/// zero-width characters are removed by sanitization before this runs.
fn char_display_width(ch: char) -> usize {
    match ch as u32 {
        0x1100..=0x115F
        | 0x2E80..=0xA4CF
        | 0xAC00..=0xD7A3
        | 0xF900..=0xFAFF
        | 0xFE30..=0xFE4F
        | 0xFF00..=0xFF60
        | 0xFFE0..=0xFFE6
        | 0x1F300..=0x1FAFF
        | 0x20000..=0x3FFFD => 2,
        _ => 1,
    }
}

fn display_width(s: &str) -> usize {
    s.chars().map(char_display_width).sum()
}

/// Content cells between the banner box's side bars.
const BANNER_INNER_WIDTH: usize = 56;

/// A Codex-style startup banner with kitsu's fox mascot and a Japanese accent
/// (狐 = kitsune). Printed to stderr at chat start. On a non-TTY it degrades to
/// a single plain line so piped output stays clean.
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
    let tools_str = if tools.is_empty() {
        "none (read-only chat)".to_string()
    } else if tools.join(", ").chars().count() > 64 {
        format!("{} enabled (use /tools)", tools.len())
    } else {
        tools.join(", ")
    };
    // Closed box: top/bottom rules share one width and rows pad to the right
    // bar. A row whose value overflows the box keeps only the left bar rather
    // than truncating information.
    let boxed = |plain_row: &str, styled_row: &str| {
        let width = display_width(plain_row);
        if width <= BANNER_INNER_WIDTH {
            let pad = " ".repeat(BANNER_INNER_WIDTH - width);
            eprintln!("  {bar}{styled_row}{pad}{bar}");
        } else {
            eprintln!("  {bar}{styled_row}");
        }
    };
    let row = |label: &str, value: &str| {
        let plain_row = format!("  {label:<9}   {value}");
        let styled_row = format!("  {}   {value}", format!("{label:<9}").dimmed());
        boxed(&plain_row, &styled_row);
    };
    let title = "🦊 kitsu 狐";
    // "╭─ " + title + " " + fill + "╮" spans BANNER_INNER_WIDTH + 2 cells.
    let fill = (BANNER_INNER_WIDTH + 1).saturating_sub(4 + display_width(title));
    eprintln!();
    eprintln!(
        "  {} {} {}",
        "╭─".truecolor(r, g, b),
        title.truecolor(r, g, b).bold(),
        format!("{}╮", "─".repeat(fill)).truecolor(r, g, b)
    );
    boxed(
        r"   /\   /\",
        &format!("{}", r"   /\   /\".truecolor(r, g, b)),
    );
    boxed(
        "  ( ･ᴥ･ )   multi-gateway LLM familiar",
        &format!(
            "  {}   {}",
            "( ･ᴥ･ )".truecolor(r, g, b),
            "multi-gateway LLM familiar".dimmed()
        ),
    );
    boxed("", "");
    row("gateway", &gateway);
    row("model", &model);
    row("directory", &dir);
    row("tools", &tools_str);
    eprintln!(
        "  {}",
        format!("╰{}╯", "─".repeat(BANNER_INNER_WIDTH)).truecolor(r, g, b)
    );
    eprintln!(
        "  {}",
        "Enter / for commands · Tab completes prefixes · Ctrl+D exits".dimmed()
    );
    eprintln!();
}

/// The interactive chat prompt. Fox-orange on a TTY, plain otherwise.
pub fn prompt_string() -> String {
    if stdout_style() {
        let (r, g, b) = FOX;
        format!("{} ", "kitsu>".truecolor(r, g, b).bold())
    } else {
        "kitsu> ".to_string()
    }
}

/// Whether stderr output is currently styled (TTY and not plain). Callers that
/// assemble their own stderr text (e.g. `/help`) can use this instead of
/// re-deriving the mode.
pub fn stderr_styled() -> bool {
    stderr_style()
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
        let (r, g, b) = match self.level {
            AsyncNoticeLevel::Info => TEAL,
            AsyncNoticeLevel::Success => SAGE,
            AsyncNoticeLevel::Warning => AMBER,
            AsyncNoticeLevel::Error => EMBER,
        };
        format!("{}", line.truecolor(r, g, b))
    }
}

/// Render a terminal failure to stderr as one weighted error notice: the first
/// line carries the `! error:` marker (ember-red on a styled TTY); any
/// following hint lines pass through untinted. `--plain`/non-TTY receive the
/// same text with no ANSI.
pub fn render_error(message: &str) {
    for line in error_lines(message) {
        eprintln!("{line}");
    }
}

/// The [`render_error`] payload as lines, for callers that own the printing
/// (e.g. to apply JSONL-mode suppression).
pub fn error_lines(message: &str) -> Vec<String> {
    error_display_lines(message, stderr_style(), plain_mode())
}

fn error_display_lines(message: &str, styled: bool, plain: bool) -> Vec<String> {
    let message = if plain {
        ascii_structural_markers(&sanitize_terminal_text(message))
    } else {
        sanitize_terminal_text(message)
    };
    let mut lines = message.lines();
    let first = format!("! error: {}", lines.next().unwrap_or_default());
    let mut output = Vec::new();
    if styled && !plain {
        let (r, g, b) = EMBER;
        output.push(format!("{}", first.truecolor(r, g, b).bold()));
    } else {
        output.push(first);
    }
    output.extend(lines.map(str::to_string));
    output
}

/// A "vibing kitsu" spinner shown on stderr while the model is working (the wait
/// before the first token). Animates a fox kaomoji in fox-orange on a TTY.
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

/// The animated fox-kaomoji frames (kitsune, not neko): fixed ᴥ-muzzle face
/// with a sweeping tail. Plain/reduced-motion modes never see these.
const SPINNER_FRAMES: [&str; 4] = ["( ･ᴥ･ )ﾉ", "( ･ᴥ･ )～", "( ･ᴥ･ ) ～", "( ﾉᴥﾉ )～"];

/// Backed-off cadence shared by line-based waiting heartbeats (cycle-10 G4):
/// 15s, 30s, 60s, then every 120s. A long wait stays visibly alive without
/// `~ still waiting` lines stacking 10-15 deep between receipts and drowning
/// the ◆/◇ landmarks in the transcript.
pub fn heartbeat_backoff(beat_index: u32) -> std::time::Duration {
    const CADENCE_SECS: [u64; 3] = [15, 30, 60];
    std::time::Duration::from_secs(
        CADENCE_SECS
            .get(beat_index as usize)
            .copied()
            .unwrap_or(120),
    )
}

/// Cumulative wait time for heartbeat lines: `45s`, then `4m30s` past a
/// minute, so a long transcript shows total elapsed wait at a glance.
pub fn format_wait_elapsed(elapsed: std::time::Duration) -> String {
    let secs = elapsed.as_secs();
    if secs < 60 {
        format!("{secs}s")
    } else {
        format!("{}m{:02}s", secs / 60, secs % 60)
    }
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
                    // does. The line-based render path cannot rewrite in
                    // place, so repeats back off exponentially (15s, 30s,
                    // 60s, then every 120s) with cumulative elapsed time,
                    // keeping a multi-minute wait visible without drowning
                    // the receipt landmarks (cycle-10 G4).
                    let mut waited = std::time::Duration::ZERO;
                    let mut beat = 0u32;
                    loop {
                        let interval = heartbeat_backoff(beat);
                        tokio::time::sleep(interval).await;
                        waited += interval;
                        beat += 1;
                        eprintln!(
                            "~ still waiting on the model ({})...",
                            format_wait_elapsed(waited)
                        );
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
            // Kitsu is vibing while the model does the hard work: a fox face
            // (ᴥ muzzle, no cat ω/ฅ) with a sweeping tail.
            const FRAMES: [&str; 4] = SPINNER_FRAMES;
            const DOTS: [&str; 4] = ["   ", "･  ", "･･ ", "･･･"];
            let (r, g, b) = FOX;
            let mut i = 0usize;
            // Avoid a distracting flash for fast local responses.
            tokio::time::sleep(std::time::Duration::from_millis(180)).await;
            let frame_width = FRAMES.iter().map(|frame| display_width(frame)).max();
            loop {
                let frame = FRAMES[i % FRAMES.len()];
                // Pad the sweeping tail to one width so a shorter frame never
                // leaves residue from a longer predecessor on the status line.
                let pad = " ".repeat(frame_width.unwrap_or(0) - display_width(frame));
                let dots = DOTS[i % DOTS.len()];
                eprint!(
                    "\r  {}{pad} {}{}",
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

/// Mark the start of one foreground model turn's stdout transaction. The
/// orchestrator calls this so [`turn_wrote_assistant_output`] answers "did
/// THIS turn stream anything" rather than remembering earlier turns.
pub fn begin_turn_output() {
    TURN_ASSISTANT_OUTPUT.store(false, Ordering::Release);
}

/// Whether the current (or just-finished) turn wrote assistant text to stdout.
/// Unlike the newline-discipline flag consumed by [`finish_line`], this
/// survives mid-turn status lines, so an error path can decide honestly
/// whether provisional output existed at all.
pub fn turn_wrote_assistant_output() -> bool {
    TURN_ASSISTANT_OUTPUT.load(Ordering::Acquire)
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
/// friendly line — e.g. `◆ Read(deck1.md)`. Fox-orange marker on a TTY.
pub fn tool_activity(header: &str) {
    let plain = plain_mode();
    let header = if plain {
        sanitize_plain_inline(header)
    } else {
        sanitize_terminal_inline(header)
    };
    if stderr_is_tty() && !plain {
        let (r, g, b) = FOX;
        eprintln!("{} {header}", GLYPH_TOOL_CALL.truecolor(r, g, b));
    } else if plain {
        eprintln!("tool> {header}");
    } else {
        eprintln!("{GLYPH_TOOL_CALL} {header}");
    }
}

/// The result summary line under a tool call — e.g. `  ◇ 128 lines`. Dimmed on a
/// TTY. Goes to stderr so stdout stays assistant-text only.
pub fn tool_result(summary: &str) {
    tool_result_with_weight(summary, false)
}

/// A failed tool call's result line. Ember-red on a styled TTY so an error is
/// emphasized rather than dimmed; identical to [`tool_result`] otherwise.
pub fn tool_result_error(summary: &str) {
    tool_result_with_weight(summary, true)
}

fn tool_result_with_weight(summary: &str, is_error: bool) {
    let plain = plain_mode();
    let summary = if plain {
        sanitize_plain_inline(summary)
    } else {
        sanitize_terminal_inline(summary)
    };
    let line = format!("  {GLYPH_TOOL_RESULT} {summary}");
    if stderr_is_tty() && !plain {
        if is_error {
            let (r, g, b) = EMBER;
            eprintln!("{}", line.truecolor(r, g, b));
        } else {
            eprintln!("{}", line.dimmed());
        }
    } else if plain {
        eprintln!("result> {summary}");
    } else {
        eprintln!("{line}");
    }
}

/// A section heading (e.g. one `/help` group). Bold when `styled`, verbatim
/// otherwise, so plain output keeps identical text with zero escape bytes.
pub fn section_heading(text: &str, styled: bool) -> String {
    if styled {
        format!("{}", text.bold())
    } else {
        text.to_string()
    }
}

/// The shared header of the two approval surfaces (filesystem and command):
/// `◉ permission — <request>`, with the glyph and word carrying bold
/// fox-orange weight on a styled TTY. `--plain` callers keep their existing
/// `permission>` text and must not call this.
pub fn approval_header(request: &str) -> String {
    approval_header_with_mode(request, stderr_style())
}

fn approval_header_with_mode(request: &str, styled: bool) -> String {
    let request = sanitize_terminal_inline(request);
    if styled {
        let (r, g, b) = FOX;
        format!(
            "{} — {request}",
            format!("{GLYPH_APPROVAL} permission")
                .truecolor(r, g, b)
                .bold()
        )
    } else {
        format!("{GLYPH_APPROVAL} permission — {request}")
    }
}

/// The aligned approval option line `[y] allow once · [a] <label> · [n] deny`,
/// with the answer letters bold on a styled TTY.
pub fn approval_options(always_label: &str) -> String {
    approval_options_with_mode(always_label, stderr_style())
}

fn approval_options_with_mode(always_label: &str, styled: bool) -> String {
    let always_label = sanitize_terminal_inline(always_label);
    let key = |k: &str| {
        if styled {
            format!("{}", format!("[{k}]").bold())
        } else {
            format!("[{k}]")
        }
    };
    format!(
        "  {} allow once · {} {always_label} · {} deny",
        key("y"),
        key("a"),
        key("n")
    )
}

/// A dimmed detail row beneath an approval header (cwd, executable, timeout).
pub fn approval_detail(text: &str) -> String {
    let text = sanitize_terminal_inline(text);
    if stderr_style() {
        format!("  {}", text.dimmed())
    } else {
        format!("  {text}")
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

/// One torii (⛩) milestone line for a session-boundary event — compaction,
/// `/new`, `/resume`, checkpoint `/undo`. The torii marks the session passing a
/// gate; at most one line per event, never in tool receipts. `--plain` renders
/// the same text behind the ASCII `*` marker.
pub fn milestone(message: &str) {
    eprintln!("{}", milestone_line(message));
}

/// The [`milestone`] payload as a string, for callers that route output.
pub fn milestone_line(message: &str) -> String {
    milestone_line_with_mode(message, stderr_style(), plain_mode())
}

fn milestone_line_with_mode(message: &str, styled: bool, plain: bool) -> String {
    if plain {
        format!("* {}", sanitize_plain_inline(message))
    } else {
        let line = format!("{GLYPH_TORII} {}", sanitize_terminal_inline(message));
        if styled {
            let (r, g, b) = FOX;
            format!("{}", line.truecolor(r, g, b))
        } else {
            line
        }
    }
}

/// Announce a summarizing compaction to stderr so the user knows older history
/// was replaced by a summary.
pub fn compaction_boundary() {
    milestone("context compacted (older turns summarized)");
}

/// The attention state of one agent-deck row (docs/ux-design.md D1). Fill level
/// carries the state, matching the established glyph semantics: `◆` filled =
/// live action, `◈` = a durable result awaiting custody, `◇` hollow = spent.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeckState {
    /// Working right now.
    Running,
    /// Finished; its result has not been collected yet.
    ResultReady,
    /// Finished by failing.
    Failed,
    /// Stopped by a user or by policy.
    Stopped,
}

impl DeckState {
    fn glyph(self) -> &'static str {
        match self {
            Self::Running => GLYPH_TOOL_CALL,
            Self::ResultReady => GLYPH_RECEIPT,
            Self::Failed | Self::Stopped => GLYPH_TOOL_RESULT,
        }
    }

    fn color(self) -> (u8, u8, u8) {
        match self {
            Self::Running | Self::ResultReady => FOX,
            Self::Failed => EMBER,
            Self::Stopped => AMBER,
        }
    }

    /// The stable ASCII marker `--plain`/`NO_COLOR`/non-TTY receive.
    fn plain_marker(self) -> &'static str {
        match self {
            Self::Running => "[run]",
            Self::ResultReady => "[rdy]",
            Self::Failed => "[err]",
            Self::Stopped => "[stp]",
        }
    }
}

/// The `◈` header of a deck section (`/jobs`, `/profiles`), in highlight cream.
/// `--plain` renders `# <summary>` with zero escape bytes.
pub fn deck_header(summary: &str) -> String {
    deck_header_with_mode(summary, stderr_style(), plain_mode())
}

fn deck_header_with_mode(summary: &str, styled: bool, plain: bool) -> String {
    if plain {
        return format!("# {}", sanitize_plain_inline(summary));
    }
    let line = format!("{GLYPH_RECEIPT} {}", sanitize_terminal_inline(summary));
    if styled {
        let (r, g, b) = CREAM;
        format!("{}", line.truecolor(r, g, b).bold())
    } else {
        line
    }
}

/// One deck row's state marker, already colored for the destination. Callers
/// append their own (already sanitized) row text after it.
pub fn deck_marker(state: DeckState) -> String {
    deck_marker_with_mode(state, stderr_style(), plain_mode())
}

fn deck_marker_with_mode(state: DeckState, styled: bool, plain: bool) -> String {
    if plain {
        return state.plain_marker().to_string();
    }
    if styled {
        let (r, g, b) = state.color();
        format!("{}", state.glyph().truecolor(r, g, b))
    } else {
        state.glyph().to_string()
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

/// Terminal state attached to the model-command timing receipt. Keeping this
/// closed prevents an arbitrary status string from entering the stable
/// redirected record or a terminal line.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RunTimingStatus {
    Ok,
    Error,
    Cancelled,
}

impl RunTimingStatus {
    fn record_label(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Error => "error",
            Self::Cancelled => "cancelled",
        }
    }

    fn human_phrase(self) -> &'static str {
        match self {
            Self::Ok => "completed in",
            Self::Error => "failed after",
            Self::Cancelled => "cancelled after",
        }
    }
}

fn format_run_elapsed(elapsed: std::time::Duration) -> String {
    let millis = u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX);
    if millis < 1_000 {
        format!("{millis} ms")
    } else if millis < 60_000 {
        format!("{}.{:01} s", millis / 1_000, (millis % 1_000) / 100)
    } else {
        format_wait_elapsed(elapsed)
    }
}

/// Emit one end-of-run timing receipt for model commands. A person at a
/// terminal gets the same compact secondary-row language as turn statistics;
/// redirected stderr gets a stable, greppable JSON record. Management-only
/// commands do not call this function.
pub fn emit_run_timing(elapsed: std::time::Duration, status: RunTimingStatus) {
    let mut err = io::stderr();
    let destination_is_tty = err.is_terminal();
    let _ = write_run_timing_with_mode(&mut err, destination_is_tty, plain_mode(), elapsed, status);
}

fn write_run_timing_with_mode<W: Write>(
    destination: &mut W,
    destination_is_tty: bool,
    plain: bool,
    elapsed: std::time::Duration,
    status: RunTimingStatus,
) -> io::Result<()> {
    if destination_is_tty {
        let marker = if plain { "->" } else { "↳" };
        let line = format!(
            "  {marker} {} {}",
            status.human_phrase(),
            format_run_elapsed(elapsed)
        );
        if plain {
            writeln!(destination, "{line}")?;
        } else {
            writeln!(destination, "{}", line.dimmed())?;
        }
    } else {
        let duration_ms = u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX);
        writeln!(
            destination,
            "kitsu-timing: {{\"duration_ms\":{duration_ms},\"status\":\"{}\"}}",
            status.record_label()
        )?;
    }
    destination.flush()
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

/// End-of-turn telemetry for a FAILED turn. A person at a terminal gets no
/// stats line — a turn that produced nothing should not be billed a summary —
/// while redirected output keeps the stable `kitsu-usage:`/`kitsu-context:`
/// records so automation still observes what the failed attempt consumed.
pub fn emit_failed_turn_stats(usage: Option<(u64, u64, u64)>, context: Option<(u64, u64, u64)>) {
    let mut err = io::stderr();
    if err.is_terminal() {
        return;
    }
    let _ = write_turn_stats(&mut err, false, usage, context);
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
    let filled_cells: String = (0..filled).map(|_| filled_cell).collect();
    let empty_cells: String = (filled..CELLS).map(|_| empty_cell).collect();
    let win = if note.is_empty() {
        format!("{window}")
    } else {
        format!("{window} ({note})")
    };
    let mut lines = vec![format!("model: {model}   window: {win}")];
    let bar_disp = if mode.styled {
        // Fill state reads by color as well as glyph shape: FOX below 80%
        // utilization, AMBER at >=80%, EMBER at >=95% — a quiet capacity
        // warning. Empty cells are dimmed so the boundary is visible.
        let (r, g, b) = match pct {
            p if p >= 95 => EMBER,
            p if p >= 80 => AMBER,
            _ => (r, g, b),
        };
        format!(
            "{}{}",
            filled_cells.truecolor(r, g, b),
            empty_cells.dimmed()
        )
    } else {
        format!("{filled_cells}{empty_cells}")
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
        approval_header_with_mode, approval_options_with_mode, context_lines,
        deck_header_with_mode, deck_marker_with_mode, display_width, error_display_lines,
        format_message, grouped_count, human_turn_stats, milestone_line_with_mode,
        sanitize_plain_inline, sanitize_terminal_inline, sanitize_terminal_text, section_heading,
        spinner_mode, static_spinner_line, write_run_timing_with_mode, write_static_spinner,
        write_turn_stats, write_turn_stats_with_mode, AsyncNoticeLevel, DeckState,
        PromptSafeAsyncNotice, RenderMode, RunTimingStatus, SpinnerMode, TerminalTextSanitizer,
        AMBER, CREAM, EMBER, FOX, SPINNER_FRAMES,
    };

    fn sgr(rgb: (u8, u8, u8)) -> String {
        format!("\x1b[38;2;{};{};{}m", rgb.0, rgb.1, rgb.2)
    }

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
    fn run_timing_is_compact_for_people_and_stable_when_redirected() {
        let elapsed = std::time::Duration::from_millis(1_250);
        let mut terminal = Vec::new();
        write_run_timing_with_mode(&mut terminal, true, true, elapsed, RunTimingStatus::Ok)
            .unwrap();
        assert_eq!(
            String::from_utf8(terminal).unwrap(),
            "  -> completed in 1.2 s\n"
        );

        let mut styled = Vec::new();
        write_run_timing_with_mode(
            &mut styled,
            true,
            false,
            elapsed,
            RunTimingStatus::Cancelled,
        )
        .unwrap();
        let styled = String::from_utf8(styled).unwrap();
        assert!(styled.contains('\x1b'));
        assert_eq!(
            sanitize_terminal_text(&styled),
            "  ↳ cancelled after 1.2 s\n"
        );

        let mut redirected = Vec::new();
        write_run_timing_with_mode(
            &mut redirected,
            false,
            false,
            elapsed,
            RunTimingStatus::Error,
        )
        .unwrap();
        assert_eq!(
            String::from_utf8(redirected).unwrap(),
            "kitsu-timing: {\"duration_ms\":1250,\"status\":\"error\"}\n"
        );
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
    fn spinner_frames_are_fox_kaomoji_not_cat() {
        for frame in SPINNER_FRAMES {
            assert!(frame.contains('ᴥ'), "fox muzzle missing in {frame:?}");
            assert!(!frame.contains('ω'), "cat mouth in {frame:?}");
            assert!(!frame.contains('ฅ'), "cat paws in {frame:?}");
        }
        // Frame widths stay within the padding budget applied at render time.
        let widths: Vec<usize> = SPINNER_FRAMES.iter().map(|f| display_width(f)).collect();
        let max = *widths.iter().max().unwrap();
        assert!(widths.iter().all(|w| max - w <= 2), "{widths:?}");
    }

    #[test]
    fn display_width_accounts_for_wide_identity_glyphs() {
        assert_eq!(display_width("kitsu"), 5);
        assert_eq!(display_width("🦊 kitsu 狐"), 11);
        assert_eq!(display_width("( ･ᴥ･ )"), 7);
    }

    #[test]
    fn own_glyph_set_maps_to_stable_ascii_in_plain_mode() {
        assert_eq!(sanitize_plain_inline("◆ ◇ ◉ ◈ ⛩"), "* > o # *");
        // The retired borrowed glyphs still degrade if they ever appear.
        assert_eq!(sanitize_plain_inline("⏺ ⎿ ✻"), "* > *");
    }

    #[test]
    fn milestone_is_a_torii_line_with_ascii_plain_fallback() {
        let plain =
            milestone_line_with_mode("context compacted (older turns summarized)", false, true);
        assert_eq!(plain, "* context compacted (older turns summarized)");
        assert!(plain.is_ascii());

        let unstyled = milestone_line_with_mode("resumed abc123", false, false);
        assert_eq!(unstyled, "⛩ resumed abc123");

        let styled = milestone_line_with_mode("resumed abc123", true, false);
        assert!(styled.contains(&sgr(FOX)));
        assert_eq!(sanitize_terminal_text(&styled), "⛩ resumed abc123");
    }

    #[test]
    fn error_notice_is_weighted_on_a_tty_and_stable_ascii_elsewhere() {
        let message = "chat request could not reach the gateway - run `kitsu doctor --online`";
        let styled = error_display_lines(message, true, false);
        assert_eq!(styled.len(), 1);
        assert!(styled[0].contains(&sgr(EMBER)));
        assert_eq!(
            sanitize_terminal_text(&styled[0]),
            format!("! error: {message}")
        );

        let plain = error_display_lines(message, false, true);
        assert_eq!(plain, vec![format!("! error: {message}")]);
        assert!(!plain[0].contains('\u{1b}'));

        // Extra lines are the actionable hint and stay untinted.
        let multi = error_display_lines("first\nhint line", true, false);
        assert_eq!(multi.len(), 2);
        assert_eq!(multi[1], "hint line");
        assert!(!multi[1].contains('\u{1b}'));
    }

    #[test]
    fn approval_prompt_shares_one_weighted_design() {
        let styled = approval_header_with_mode("kitsu wants to read /etc/hostname", true);
        assert!(styled.contains("◉ permission"));
        assert!(styled.contains("\x1b[1m"), "header must carry bold weight");
        assert!(styled.contains(&sgr(FOX)));
        assert_eq!(
            sanitize_terminal_text(&styled),
            "◉ permission — kitsu wants to read /etc/hostname"
        );

        let unstyled = approval_header_with_mode("run rm -rf /tmp/x", false);
        assert_eq!(unstyled, "◉ permission — run rm -rf /tmp/x");

        let options = approval_options_with_mode("always (/etc)", true);
        assert_eq!(
            sanitize_terminal_text(&options),
            "  [y] allow once · [a] always (/etc) · [n] deny"
        );
        assert!(options.contains("\x1b[1m"));
        assert!(!approval_options_with_mode("always (/etc)", false).contains('\u{1b}'));
    }

    #[test]
    fn section_heading_is_bold_only_when_styled() {
        assert_eq!(section_heading("permissions", false), "permissions");
        let styled = section_heading("permissions", true);
        assert!(styled.contains("\x1b[1m"));
        assert_eq!(sanitize_terminal_text(&styled), "permissions");
    }

    #[test]
    fn context_meter_tints_fill_by_utilization_thresholds() {
        let mode = RenderMode {
            styled: true,
            plain: false,
        };
        let low = context_lines("m", 8_000, "", 8_000, 2_000, &[], mode).join("\n");
        assert!(low.contains(&sgr(FOX)));
        assert!(low.contains("\x1b[2m"), "empty cells must be dimmed");
        assert!(!low.contains(&sgr(AMBER)));
        assert!(!low.contains(&sgr(EMBER)));

        let high = context_lines("m", 8_000, "", 8_000, 6_800, &[], mode).join("\n");
        assert!(high.contains(&sgr(AMBER)), "85% utilization tints amber");

        let critical = context_lines("m", 8_000, "", 8_000, 7_700, &[], mode).join("\n");
        assert!(
            critical.contains(&sgr(EMBER)),
            "96% utilization tints ember"
        );
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

    /// Cycle-10 G4: line-based heartbeats back off exponentially — 15s, 30s,
    /// 60s, then every 120s — so ten minutes of waiting produces a handful of
    /// lines instead of dozens, and each line's elapsed field is cumulative.
    #[test]
    fn heartbeat_cadence_backs_off_and_elapsed_is_cumulative() {
        use super::{format_wait_elapsed, heartbeat_backoff};
        let cadence: Vec<u64> = (0..6).map(|i| heartbeat_backoff(i).as_secs()).collect();
        assert_eq!(cadence, [15, 30, 60, 120, 120, 120]);

        // Ten minutes of waiting: 15+30+60 then 120-steps → 7 lines total.
        let mut waited = std::time::Duration::ZERO;
        let mut beats = 0u32;
        while waited < std::time::Duration::from_secs(600) {
            waited += heartbeat_backoff(beats);
            beats += 1;
        }
        assert!(
            beats <= 8,
            "ten minutes must not stack more than 8 heartbeat lines, got {beats}"
        );

        assert_eq!(
            format_wait_elapsed(std::time::Duration::from_secs(45)),
            "45s"
        );
        assert_eq!(
            format_wait_elapsed(std::time::Duration::from_secs(270)),
            "4m30s"
        );
        assert_eq!(
            format_wait_elapsed(std::time::Duration::from_secs(600)),
            "10m00s"
        );
    }

    #[test]
    fn deck_markers_carry_state_by_fill_and_degrade_to_stable_ascii() {
        // Styled TTY: fill level is the state, colour is the severity.
        assert_eq!(
            deck_marker_with_mode(DeckState::Running, true, false),
            format!("{}◆\u{1b}[39m", sgr(FOX))
        );
        assert_eq!(
            deck_marker_with_mode(DeckState::ResultReady, true, false),
            format!("{}◈\u{1b}[39m", sgr(FOX))
        );
        assert_eq!(
            deck_marker_with_mode(DeckState::Failed, true, false),
            format!("{}◇\u{1b}[39m", sgr(EMBER))
        );
        assert_eq!(
            deck_marker_with_mode(DeckState::Stopped, true, false),
            format!("{}◇\u{1b}[39m", sgr(AMBER))
        );

        // Non-TTY keeps the glyphs and drops the colour.
        assert_eq!(deck_marker_with_mode(DeckState::Running, false, false), "◆");

        // `--plain`/`NO_COLOR`: stable ASCII, zero escape bytes.
        for (state, marker) in [
            (DeckState::Running, "[run]"),
            (DeckState::ResultReady, "[rdy]"),
            (DeckState::Failed, "[err]"),
            (DeckState::Stopped, "[stp]"),
        ] {
            let rendered = deck_marker_with_mode(state, true, true);
            assert_eq!(rendered, marker);
            assert!(!rendered.contains('\u{1b}'));
        }

        let styled_header = deck_header_with_mode("agent deck — 2 running", true, false);
        assert!(styled_header.contains(&sgr(CREAM)));
        assert_eq!(
            sanitize_terminal_text(&styled_header),
            "◈ agent deck — 2 running"
        );
        let plain_header = deck_header_with_mode("agent deck — 2 running", true, true);
        assert_eq!(plain_header, "# agent deck - 2 running");
        assert!(!plain_header.contains('\u{1b}'));
    }
}
