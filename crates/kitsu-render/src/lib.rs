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

/// Stateful streaming buffer for styled output on TTY.
/// Accumulates text into a line buffer; when a complete line is formed (ends with `\n`),
/// styles it via LineStyler and writes it. Partial lines are buffered until completed
/// or explicitly flushed (unstyled).
///
/// This buffer is ONLY used when stdout is a styled TTY. Non-TTY or plain-mode streaming
/// bypasses this entirely, preserving byte-identical output. This is a deliberate tradeoff:
/// styled TTY output becomes line-granular (stalled at line boundaries) instead of
/// token-granular, but preserves the buffered-message styling semantics on the live path.
#[derive(Debug, Default)]
struct StreamBuffer {
    buffer: String,
    styler: LineStyler,
}

impl StreamBuffer {
    /// Push text into the buffer, styling and writing complete lines to stdout.
    /// Returns the total bytes written to stdout.
    /// Partial lines (not ending with `\n`) are buffered and NOT written.
    fn push(&mut self, s: &str) -> usize {
        let mut written = 0;
        self.buffer.push_str(s);

        // Process complete lines (those ending with \n)
        while let Some(newline_pos) = self.buffer.find('\n') {
            let line = &self.buffer[..newline_pos];
            let styled_line = self.styler.style_line(line);
            let _ = io::stdout().write_all(styled_line.as_bytes());
            written += styled_line.len();
            let _ = io::stdout().write_all(b"\n");
            written += 1;
            self.buffer.drain(..=newline_pos);
        }

        written
    }

    /// Flush any remaining partial line UNSTYLED and reset fence state.
    /// Called at turn boundaries (finish_line).
    fn flush_unstyled(&mut self) -> usize {
        let mut written = 0;
        if !self.buffer.is_empty() {
            let _ = io::stdout().write_all(self.buffer.as_bytes());
            written = self.buffer.len();
            self.buffer.clear();
        }
        self.styler.reset();
        written
    }
}

fn stream_buffer() -> &'static Mutex<StreamBuffer> {
    static BUFFER: OnceLock<Mutex<StreamBuffer>> = OnceLock::new();
    BUFFER.get_or_init(|| Mutex::new(StreamBuffer::default()))
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
///
/// On a styled TTY, text is accumulated into a line buffer and styled per-line
/// via the streaming LineStyler. Complete lines are written immediately; partial
/// lines are held until a newline arrives. This trades token-granular output for
/// line-granular output, but enables the same markdown styling as the buffered
/// `print_message` path.
///
/// On non-TTY or plain-mode output, text is sanitized and written immediately,
/// maintaining byte-identical output to the current behavior.
pub fn print_fragment(s: &str) {
    if stdout_style() {
        // Styled TTY: SANITIZE FIRST (model output is untrusted — control
        // sequences must never reach the terminal, styled path included),
        // then line-buffer the sanitized text for styling. Output flags set
        // when text enters the pipeline (old semantics), not only on line
        // completion — a turn whose only output is a partial line must still
        // count as having streamed output.
        let sanitized = stream_sanitizer()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(s);
        if !sanitized.is_empty() {
            ASSISTANT_OUTPUT_WRITTEN.store(true, Ordering::Release);
            TURN_ASSISTANT_OUTPUT.store(true, Ordering::Release);
        }
        stream_buffer()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(&sanitized);
        let _ = io::stdout().flush();
    } else {
        // Non-TTY or plain: use existing sanitizer path (byte-identical)
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

/// Style inline markdown elements in a line: bold, inline code, and list markers.
/// Respects code fence state (does not style inside fenced code blocks).
/// Unclosed markers are left unstyled.
fn style_inline_markdown(line: &str, in_fence: bool) -> String {
    if in_fence {
        return line.to_string();
    }

    let (r, g, b) = theme().highlight;
    let mut out = String::with_capacity(line.len() * 2);
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;

    // Handle list markers at the start of the line (after optional indent)
    {
        let trimmed = line.trim_start();
        let indent_len = line.len() - trimmed.len();
        let trimmed_chars: Vec<char> = trimmed.chars().collect();
        if trimmed_chars.len() >= 2 {
            if (trimmed_chars[0] == '-' || trimmed_chars[0] == '*') && trimmed_chars[1] == ' ' {
                // Unordered list: "- " or "* "
                out.push_str(&line[..indent_len]);
                out.push_str(&format!("{}", trimmed_chars[0].to_string().dimmed()));
                out.push_str(&format!("{}", trimmed_chars[1].to_string().dimmed()));
                i = indent_len + 2;
            } else if trimmed_chars[0].is_ascii_digit() {
                // Ordered list: "1. " etc
                let mut j = 0;
                while j < trimmed_chars.len() && trimmed_chars[j].is_ascii_digit() {
                    j += 1;
                }
                if j < trimmed_chars.len() - 1
                    && trimmed_chars[j] == '.'
                    && trimmed_chars[j + 1] == ' '
                {
                    // Valid ordered list marker
                    out.push_str(&line[..indent_len]);
                    for ch in &trimmed_chars[..j] {
                        out.push_str(&format!("{}", ch.to_string().dimmed()));
                    }
                    out.push_str(&format!("{}", trimmed_chars[j].to_string().dimmed()));
                    out.push_str(&format!("{}", trimmed_chars[j + 1].to_string().dimmed()));
                    i = indent_len + j + 2;
                }
            }
        }
    }

    // Style only CLOSED spans (the spec: unclosed markers pass through
    // unstyled). Each span is pushed as ONE styled chunk, not per-char
    // escapes.
    while i < chars.len() {
        if chars[i] == '*' && i + 1 < chars.len() && chars[i + 1] == '*' {
            if let Some(close) = find_pair(&chars, i + 2, &['*', '*']) {
                let span: String = chars[i + 2..close].iter().collect();
                out.push_str(&format!("{}", format!("**{span}**").bold()));
                i = close + 2;
                continue;
            }
            out.push_str("**");
            i += 2;
        } else if chars[i] == '`' {
            if let Some(close) = find_pair(&chars, i + 1, &['`']) {
                let span: String = chars[i + 1..close].iter().collect();
                out.push_str(&format!("{}", format!("`{span}`").truecolor(r, g, b)));
                i = close + 1;
                continue;
            }
            out.push('`');
            i += 1;
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }

    out
}

/// Index where `needle` next occurs in `chars` at or after `from`, or None.
fn find_pair(chars: &[char], from: usize, needle: &[char]) -> Option<usize> {
    if needle.is_empty() || chars.len() < needle.len() {
        return None;
    }
    (from..=chars.len() - needle.len()).find(|&idx| chars[idx..idx + needle.len()] == *needle)
}

/// Stateful line styler for per-line markdown formatting.
/// Tracks code fence state across multiple `style_line` calls,
/// enabling streaming output with the same styling rules as buffered `format_message`.
///
/// The fence state machine is essential: styling must respect code fence boundaries
/// across line boundaries. `reset()` ends the current turn and clears fence state.
#[derive(Debug, Default)]
struct LineStyler {
    fence: Option<String>,
}

impl LineStyler {
    /// Style a single line of markdown text according to fence state.
    /// Updates fence state if the line contains fence markers.
    /// Returns the styled line (without newline).
    fn style_line(&mut self, line: &str) -> String {
        // Check for fence marker
        if let Some(language) = line.trim().strip_prefix("```") {
            if self.fence.is_some() {
                self.fence = None;
            } else {
                self.fence = Some(language.trim().to_string());
            }
            return format!("{}", line.dimmed());
        }

        let in_fence = self.fence.is_some();

        // Apply styling based on fence state
        if self.fence.as_deref() == Some("diff") || self.fence.as_deref() == Some("patch") {
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
            style_inline_markdown(line, in_fence)
        }
    }

    /// Reset fence state for the next turn (e.g., at line completion).
    fn reset(&mut self) {
        self.fence = None;
    }
}

fn format_message(s: &str, styled: bool) -> String {
    if !styled {
        return s.to_string();
    }
    let mut styler = LineStyler::default();
    let mut out = String::with_capacity(s.len());
    for segment in s.split_inclusive('\n') {
        let line = segment.trim_end_matches('\n');
        let newline = if segment.ends_with('\n') { "\n" } else { "" };
        let rendered = styler.style_line(line);
        out.push_str(&rendered);
        out.push_str(newline);
    }
    out
}

/// Named kitsune palette (design.md §8, docs/ux-design.md). Truecolor RGB,
/// used only when the destination is a styled TTY. These constants are the
/// DEFAULT theme's values; live rendering reads the active [`Theme`] instead
/// so `/theme` can restyle the whole surface at runtime.
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

/// One color theme: the six palette roles every styled surface draws from.
/// Structural glyphs are theme-independent (they are vocabulary, not skin).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Theme {
    pub name: &'static str,
    pub description: &'static str,
    /// Identity accent (prompt, banner, milestones).
    pub accent: (u8, u8, u8),
    /// Warnings and high context utilization.
    pub warn: (u8, u8, u8),
    /// Highlight text.
    pub highlight: (u8, u8, u8),
    /// Informational notices and links.
    pub info: (u8, u8, u8),
    /// Success notices.
    pub success: (u8, u8, u8),
    /// Errors and critical context utilization.
    pub error: (u8, u8, u8),
}

/// Built-in themes. `kitsune` is the identity default; the rest restyle the
/// same roles without touching glyphs or layout.
pub const THEMES: &[Theme] = &[
    Theme {
        name: "kitsune",
        description: "fox-orange identity default",
        accent: FOX,
        warn: AMBER,
        highlight: CREAM,
        info: TEAL,
        success: SAGE,
        error: EMBER,
    },
    Theme {
        name: "sakura",
        description: "cherry-blossom pinks over plum",
        accent: (244, 128, 170),
        warn: (255, 183, 120),
        highlight: (255, 245, 248),
        info: (142, 88, 143),
        success: (156, 194, 145),
        error: (224, 82, 99),
    },
    Theme {
        name: "ocean",
        description: "deep-sea blues and surf teal",
        accent: (64, 156, 255),
        warn: (255, 196, 84),
        highlight: (235, 248, 255),
        info: (0, 168, 204),
        success: (88, 205, 165),
        error: (255, 99, 92),
    },
    Theme {
        name: "forest",
        description: "moss green with lantern amber",
        accent: (96, 176, 80),
        warn: (230, 178, 60),
        highlight: (244, 250, 236),
        info: (110, 140, 205),
        success: (140, 200, 120),
        error: (210, 90, 60),
    },
    Theme {
        name: "ember",
        description: "coal reds and warm grays",
        accent: (235, 94, 40),
        warn: (250, 165, 70),
        highlight: (252, 244, 236),
        info: (170, 140, 130),
        success: (180, 190, 120),
        error: (255, 60, 60),
    },
    Theme {
        name: "mono",
        description: "single-ink minimal (still styled; use --plain for ASCII)",
        accent: (200, 200, 200),
        warn: (230, 230, 230),
        highlight: (255, 255, 255),
        info: (170, 170, 170),
        success: (210, 210, 210),
        error: (245, 245, 245),
    },
];

static ACTIVE_THEME: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// The active theme (defaults to `kitsune`).
pub fn theme() -> &'static Theme {
    let index = ACTIVE_THEME.load(std::sync::atomic::Ordering::Acquire);
    THEMES.get(index).unwrap_or(&THEMES[0])
}

/// Switch the active theme by name. Unknown names list the valid set.
pub fn set_theme(name: &str) -> Result<&'static Theme, String> {
    match THEMES.iter().position(|theme| theme.name == name) {
        Some(index) => {
            ACTIVE_THEME.store(index, std::sync::atomic::Ordering::Release);
            Ok(&THEMES[index])
        }
        None => Err(format!(
            "unknown theme '{name}' — available: {}",
            THEMES
                .iter()
                .map(|theme| theme.name)
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// Format a single theme listing row for `/theme` output.
/// Returns exactly the current byte-compatible format for `plain`, or styled with
/// the theme's accent color on the name and a six-dot palette swatch when `styled`.
pub fn theme_row(theme: &Theme, active: bool, styled: bool, plain: bool) -> String {
    let marker = if active { "*" } else { " " };
    let name = theme.name;
    let description = theme.description;

    if plain {
        format!(" {marker} {name:<8} {description}")
    } else {
        let formatted = format!(" {marker} {name:<8} {description}");
        if styled {
            // Tint the name in the theme's accent color
            let colored_name = format!(
                "{}",
                name.truecolor(theme.accent.0, theme.accent.1, theme.accent.2)
            );
            // Pad to 8 chars display width (names are ASCII)
            let padding = " ".repeat(8_usize.saturating_sub(name.len()));

            // Build the swatch: ● in each of the six colors, space-separated
            let dot = "●";
            let dot_accent = format!(
                "{}",
                dot.truecolor(theme.accent.0, theme.accent.1, theme.accent.2)
            );
            let dot_warn = format!(
                " {}",
                dot.truecolor(theme.warn.0, theme.warn.1, theme.warn.2)
            );
            let dot_highlight = format!(
                " {}",
                dot.truecolor(theme.highlight.0, theme.highlight.1, theme.highlight.2)
            );
            let dot_info = format!(
                " {}",
                dot.truecolor(theme.info.0, theme.info.1, theme.info.2)
            );
            let dot_success = format!(
                " {}",
                dot.truecolor(theme.success.0, theme.success.1, theme.success.2)
            );
            let dot_error = format!(
                " {}",
                dot.truecolor(theme.error.0, theme.error.1, theme.error.2)
            );
            let swatch =
                format!(" {dot_accent}{dot_warn}{dot_highlight}{dot_info}{dot_success}{dot_error}");

            format!(" {marker} {colored_name}{padding} {description}{swatch}")
        } else {
            formatted
        }
    }
}

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

/// Fit a banner value into a given display width.
/// Short values are returned exactly; long values are elided middle-out
/// with a leading `…` keeping the tail (the informative end of a path).
/// Returns a string with display_width ≤ width.
fn fit_banner_value(value: &str, width: usize) -> String {
    if display_width(value) <= width {
        return value.to_string();
    }

    if width == 0 {
        return String::new();
    }

    // Elide middle-out: keep leading `…` and tail
    let available_for_tail = width.saturating_sub(1); // `…` takes 1 display width

    if available_for_tail == 0 {
        return "…".to_string();
    }

    let chars: Vec<char> = value.chars().collect();
    let mut current_width = 0;
    let mut tail_start = chars.len();

    // Iterate from the end, include characters that fit
    for i in (0..chars.len()).rev() {
        let ch_width = char_display_width(chars[i]);
        if current_width + ch_width <= available_for_tail {
            current_width += ch_width;
            tail_start = i;
        } else {
            break;
        }
    }

    let mut result = String::from("…");
    result.push_str(&chars[tail_start..].iter().collect::<String>());
    result
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
    let (r, g, b) = theme().accent;
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

    // Calculate available width for values: "  " (2) + label (9) + "   " (3) = 14 chars
    let value_width = BANNER_INNER_WIDTH.saturating_sub(14);

    // Fit values to available width to keep the box closed
    let gateway = fit_banner_value(&gateway, value_width);
    let model = fit_banner_value(&model, value_width);
    let dir = fit_banner_value(&dir, value_width);
    let tools_str = fit_banner_value(&tools_str, value_width);

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
        let (r, g, b) = theme().accent;
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
            AsyncNoticeLevel::Info => theme().info,
            AsyncNoticeLevel::Success => theme().success,
            AsyncNoticeLevel::Warning => theme().warn,
            AsyncNoticeLevel::Error => theme().error,
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
        let (r, g, b) = theme().error;
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

/// Fox-flavored but professional verb phrases for spinner rotation. "thinking"
/// is always first — the initial phrase shown when the spinner starts.
/// Each verb is lowercase, ASCII-safe, and ≤ 20 characters.
const SPINNER_VERBS: [&str; 6] = [
    "thinking",
    "sniffing the trail",
    "foraging",
    "circling back",
    "tracking patterns",
    "listening intently",
];

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
        let (r, g, b) = theme().accent;
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
            const VERBS: [&str; 6] = SPINNER_VERBS;
            let (r, g, b) = theme().accent;
            let mut i = 0usize;
            let start_time = std::time::Instant::now();
            // Avoid a distracting flash for fast local responses.
            tokio::time::sleep(std::time::Duration::from_millis(180)).await;
            let frame_width = FRAMES.iter().map(|frame| display_width(frame)).max();
            loop {
                let frame = FRAMES[i % FRAMES.len()];
                // Pad the sweeping tail to one width so a shorter frame never
                // leaves residue from a longer predecessor on the status line.
                let pad = " ".repeat(frame_width.unwrap_or(0) - display_width(frame));
                let dots = DOTS[i % DOTS.len()];

                // Rotate verb every ~4s (every 25 frames at 160ms per frame).
                let verb_index = (i / 25) % VERBS.len();
                let verb = VERBS[verb_index];

                // Show elapsed time after 5s.
                let elapsed = start_time.elapsed();
                let elapsed_suffix = if elapsed.as_secs() >= 5 {
                    format!(" ({})", format_wait_elapsed(elapsed))
                        .dimmed()
                        .to_string()
                } else {
                    String::new()
                };

                // Clear to end of line: verbs and the elapsed suffix vary in
                // length, and a shrinking line must not leave residue.
                eprint!(
                    "\r  {}{pad} {}{}{}\x1b[K",
                    frame.truecolor(r, g, b),
                    format!("kitsu is {}", verb).dimmed(),
                    elapsed_suffix,
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
    // Flush any partial line from the streaming buffer UNSTYLED
    // and reset its fence state.
    stream_buffer()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .flush_unstyled();

    // Reset the non-TTY sanitizer state as well
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
        let (r, g, b) = theme().accent;
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
            let (r, g, b) = theme().error;
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
        let (r, g, b) = theme().accent;
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
            let (r, g, b) = theme().accent;
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
            Self::Running | Self::ResultReady => theme().accent,
            Self::Failed => theme().error,
            Self::Stopped => theme().warn,
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
        let (r, g, b) = theme().highlight;
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

/// Format elapsed duration for the turn stats footer.
/// Format: <1s → `0.9s`; <60s → `4.2s`; longer → reuse `format_wait_elapsed`.
fn format_turn_duration(elapsed: std::time::Duration) -> String {
    let secs = elapsed.as_secs_f64();
    if secs < 60.0 {
        format!("{:.1}s", secs)
    } else {
        format_wait_elapsed(elapsed)
    }
}

fn human_turn_stats(
    usage: Option<(u64, u64, u64)>,
    context: Option<(u64, u64, u64)>,
    elapsed: Option<std::time::Duration>,
    ascii: bool,
) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(duration) = elapsed {
        parts.push(format_turn_duration(duration));
    }
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
pub fn emit_turn_stats(
    usage: Option<(u64, u64, u64)>,
    context: Option<(u64, u64, u64)>,
    elapsed: Option<std::time::Duration>,
) {
    let mut err = io::stderr();
    let destination_is_tty = err.is_terminal();
    let _ = write_turn_stats(&mut err, destination_is_tty, usage, context, elapsed);
}

/// End-of-turn telemetry for a FAILED turn. A person at a terminal gets no
/// stats line — a turn that produced nothing should not be billed a summary —
/// while redirected output keeps the stable `kitsu-usage:`/`kitsu-context:`
/// records so automation still observes what the failed attempt consumed.
pub fn emit_failed_turn_stats(
    usage: Option<(u64, u64, u64)>,
    context: Option<(u64, u64, u64)>,
    elapsed: Option<std::time::Duration>,
) {
    let mut err = io::stderr();
    if err.is_terminal() {
        return;
    }
    let _ = write_turn_stats(&mut err, false, usage, context, elapsed);
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
    elapsed: Option<std::time::Duration>,
) -> io::Result<()> {
    write_turn_stats_with_mode(
        destination,
        destination_is_tty,
        plain_mode(),
        usage,
        context,
        elapsed,
    )
}

fn write_turn_stats_with_mode<W: Write>(
    destination: &mut W,
    destination_is_tty: bool,
    plain: bool,
    usage: Option<(u64, u64, u64)>,
    context: Option<(u64, u64, u64)>,
    elapsed: Option<std::time::Duration>,
) -> io::Result<()> {
    if destination_is_tty {
        if let Some(line) = human_turn_stats(usage, context, elapsed, plain) {
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
    let (r, g, b) = theme().accent;
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
            p if p >= 95 => theme().error,
            p if p >= 80 => theme().warn,
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
        fit_banner_value, format_message, grouped_count, human_turn_stats,
        milestone_line_with_mode, sanitize_plain_inline, sanitize_terminal_inline,
        sanitize_terminal_text, section_heading, spinner_mode, static_spinner_line, theme_row,
        write_static_spinner, write_turn_stats, write_turn_stats_with_mode, AsyncNoticeLevel,
        DeckState, LineStyler, PromptSafeAsyncNotice, RenderMode, SpinnerMode, StreamBuffer,
        TerminalTextSanitizer, AMBER, CREAM, EMBER, FOX, SPINNER_FRAMES, SPINNER_VERBS, THEMES,
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

        // Test bold styling
        let bold_message = "This is **bold text** in a line\n";
        let bold_rendered = format_message(bold_message, true);
        assert!(
            bold_rendered.contains('\x1b'),
            "TTY rendering should include bold"
        );
        assert_eq!(sanitize_terminal_text(&bold_rendered), bold_message);

        // Test inline code styling
        let code_message = "Use `inline_code()` function here\n";
        let code_rendered = format_message(code_message, true);
        assert!(
            code_rendered.contains('\x1b'),
            "TTY rendering should include inline code"
        );
        assert_eq!(sanitize_terminal_text(&code_rendered), code_message);

        // Test list markers
        let list_message = "- First item\n* Second item\n1. Third item\n";
        let list_rendered = format_message(list_message, true);
        assert!(
            list_rendered.contains('\x1b'),
            "TTY rendering should include list markers"
        );
        assert_eq!(sanitize_terminal_text(&list_rendered), list_message);

        // Test unclosed bold marker (must be left completely unstyled)
        let unclosed_bold = "This has an unclosed **bold marker\n";
        let unclosed_rendered = format_message(unclosed_bold, true);
        assert_eq!(
            unclosed_rendered, unclosed_bold,
            "no styling on unclosed **"
        );
        assert_eq!(sanitize_terminal_text(&unclosed_rendered), unclosed_bold);

        // Test unclosed code marker (must be left completely unstyled)
        let unclosed_code = "This has an unclosed `code marker\n";
        let unclosed_code_rendered = format_message(unclosed_code, true);
        assert_eq!(
            unclosed_code_rendered, unclosed_code,
            "no styling on unclosed `"
        );
        assert_eq!(
            sanitize_terminal_text(&unclosed_code_rendered),
            unclosed_code
        );

        // Test combined markdown
        let combined = "# Title\n\nSome **bold** and `code` with a list:\n- Item 1\n* Item 2\n";
        let combined_rendered = format_message(combined, true);
        assert!(
            combined_rendered.contains('\x1b'),
            "TTY rendering should be styled"
        );
        assert_eq!(sanitize_terminal_text(&combined_rendered), combined);

        // Test that styling is not applied inside code fences
        let fenced = "```rust\n**not bold**\n`not code`\n```\n";
        let fenced_rendered = format_message(fenced, true);
        assert_eq!(sanitize_terminal_text(&fenced_rendered), fenced);

        // Test plain mode returns original text
        assert_eq!(format_message(bold_message, false), bold_message);
        assert_eq!(format_message(list_message, false), list_message);
    }

    #[test]
    fn streaming_buffer_accumulates_and_styles_complete_lines() {
        // Test the exact spec case: feeding "```di", "ff\n+add", "ed\n```\n"
        // should yield the +added line in green SGR.
        let mut buffer = StreamBuffer::default();

        // Accumulate first fragment: "```di" (incomplete line, no newline)
        buffer.push("```di");
        assert!(
            !buffer.buffer.is_empty(),
            "incomplete line should be buffered"
        );

        // Continue with "ff\n" which completes the fence marker line
        buffer.push("ff\n");
        assert_eq!(buffer.buffer, "", "complete line should be flushed");

        // Next: "+add" (incomplete diff line, no newline)
        buffer.push("+add");
        assert!(
            !buffer.buffer.is_empty(),
            "incomplete line should be buffered"
        );

        // Complete it with "ed\n"
        buffer.push("ed\n");
        assert_eq!(buffer.buffer, "", "complete diff line should be flushed");

        // Finally: "```\n" (closing fence)
        buffer.push("```\n");
        assert_eq!(buffer.buffer, "", "fence marker should be flushed");
    }

    #[test]
    fn streaming_buffer_flushes_unstyled() {
        // Test that flush_unstyled outputs partial lines without styling
        let mut buffer = StreamBuffer::default();

        // Accumulate a partial line (no newline)
        buffer.push("partial line");
        assert!(!buffer.buffer.is_empty(), "should have buffered partial");

        // Flush unstyled
        buffer.flush_unstyled();
        assert_eq!(buffer.buffer, "", "buffer should be empty after flush");
        assert_eq!(buffer.styler.fence, None, "fence state should be reset");
    }

    #[test]
    fn streaming_buffer_respects_fence_state_across_lines() {
        // Test that fence state is maintained across multiple line_style calls
        let mut buffer = StreamBuffer::default();

        // Push a diff fence opening
        buffer.push("```diff\n");
        assert_eq!(buffer.styler.fence.as_deref(), Some("diff"));

        // Push a diff line
        buffer.push("+added line\n");
        // The +added line should have been styled (green), and fence is still active

        // Push another diff line
        buffer.push("-removed line\n");
        // The -removed line should have been styled (red), fence still active

        // Close the fence
        buffer.push("```\n");
        assert_eq!(buffer.styler.fence, None, "fence should be closed");
    }

    #[test]
    fn line_styler_tracks_fence_state() {
        // Test that LineStyler correctly tracks fence state
        let mut styler = LineStyler::default();
        assert_eq!(styler.fence, None, "initial state: no fence");

        // Open a diff fence
        let line1 = styler.style_line("```diff");
        assert_eq!(styler.fence.as_deref(), Some("diff"));
        assert!(
            line1.contains('\x1b'),
            "fence marker should be dimmed (styled)"
        );

        // Style a diff line (+)
        let line2 = styler.style_line("+added");
        assert!(line2.contains('\x1b'), "+added should be green (styled)");
        assert_eq!(styler.fence.as_deref(), Some("diff"));

        // Style another diff line (-)
        let line3 = styler.style_line("-removed");
        assert!(line3.contains('\x1b'), "-removed should be red (styled)");
        assert_eq!(styler.fence.as_deref(), Some("diff"));

        // Close the fence
        let line4 = styler.style_line("```");
        assert!(line4.contains('\x1b'), "fence marker should be dimmed");
        assert_eq!(styler.fence, None, "fence should be closed");

        // Reset for next turn
        styler.reset();
        assert_eq!(styler.fence, None, "reset should clear fence");
    }

    #[test]
    fn line_styler_preserves_styling_invariants() {
        // Test that styled lines round-trip via sanitize
        let mut styler = LineStyler::default();

        // Test heading
        let heading = "# Main Title";
        let styled_heading = styler.style_line(heading);
        assert_eq!(sanitize_terminal_text(&styled_heading), heading);

        // Test bold
        let bold = "This is **bold text** here";
        let styled_bold = styler.style_line(bold);
        assert_eq!(sanitize_terminal_text(&styled_bold), bold);

        // Test inline code
        let code = "Use `function()` like this";
        let styled_code = styler.style_line(code);
        assert_eq!(sanitize_terminal_text(&styled_code), code);

        // Test list marker
        let list = "- First item";
        let styled_list = styler.style_line(list);
        assert_eq!(sanitize_terminal_text(&styled_list), list);
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
        let duration = std::time::Duration::from_millis(4200);
        assert_eq!(
            human_turn_stats(
                Some((11, 3, 14)),
                Some((2_000, 8_000, 8_192)),
                Some(duration),
                false
            ),
            Some("  ↳ 4.2s · 11 input · 3 output · 2,000 / 8,000 context (25% used)".into())
        );
    }

    #[test]
    fn omits_status_line_without_stats() {
        assert_eq!(human_turn_stats(None, None, None, false), None);
    }

    #[test]
    fn telemetry_uses_the_explicit_destination_tty_state() {
        let duration = std::time::Duration::from_millis(4200);
        let mut redirected = Vec::new();
        write_turn_stats(
            &mut redirected,
            false,
            Some((11, 3, 14)),
            Some((2_000, 8_000, 8_192)),
            Some(duration),
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
            Some(duration),
        )
        .unwrap();
        let terminal = String::from_utf8(terminal).unwrap();
        assert_eq!(
            terminal,
            "  -> 4.2s | 11 input | 3 output | 2,000 / 8,000 context (25% used)\n"
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
    fn spinner_verbs_are_professional_and_fox_flavored() {
        assert!(!SPINNER_VERBS.is_empty(), "SPINNER_VERBS must not be empty");
        assert_eq!(
            SPINNER_VERBS[0], "thinking",
            "first verb must always be 'thinking'"
        );
        for (i, verb) in SPINNER_VERBS.iter().enumerate() {
            assert!(
                verb.len() <= 20,
                "verb at index {} is too long: {verb:?} ({} chars)",
                i,
                verb.len()
            );
            assert!(
                verb.is_ascii(),
                "verb at index {} is not ASCII-safe: {verb:?}",
                i
            );
            assert!(
                verb.chars().all(|c| c.is_lowercase() || !c.is_alphabetic()),
                "verb at index {} contains uppercase letters: {verb:?}",
                i
            );
        }
    }

    #[test]
    fn display_width_accounts_for_wide_identity_glyphs() {
        assert_eq!(display_width("kitsu"), 5);
        assert_eq!(display_width("🦊 kitsu 狐"), 11);
        assert_eq!(display_width("( ･ᴥ･ )"), 7);
    }

    #[test]
    fn fit_banner_value_keeps_short_values_exact() {
        assert_eq!(fit_banner_value("short", 20), "short");
        assert_eq!(fit_banner_value("exact", 5), "exact");
    }

    #[test]
    fn fit_banner_value_elides_long_values_with_leading_ellipsis() {
        let long = "~/very/deep/nested/path/to/some/file";
        let fitted = fit_banner_value(long, 20);
        assert!(fitted.starts_with("…"), "should start with …");
        assert!(display_width(&fitted) <= 20, "should fit in width");
        assert!(fitted.contains("file"), "should keep the tail (file)");
    }

    #[test]
    fn fit_banner_value_respects_display_width_with_wide_chars() {
        let wide = "狐 kitsu 狐 kitsu 狐 kitsu";
        let fitted = fit_banner_value(wide, 15);
        assert!(display_width(&fitted) <= 15, "should respect display_width");
        assert!(
            fitted.starts_with("…") || fitted == wide,
            "should elide or keep exact"
        );
    }

    #[test]
    fn fit_banner_value_negative_case_renders_byte_identical_for_short_values() {
        // Verify that short values that fit within the width are returned exactly
        let short = "~/.local/project";
        assert_eq!(
            fit_banner_value(short, 42),
            short,
            "short value should be byte-identical"
        );
        // Also verify the typical directory value width calculation
        // Available width: BANNER_INNER_WIDTH (56) - 14 (overhead) = 42
        let typical = "~/workspace/kitsu";
        assert_eq!(
            fit_banner_value(typical, 42),
            typical,
            "typical directory should fit unchanged"
        );
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

    #[test]
    fn theme_row_renders_styled_with_palette_swatch() {
        let kitsune_theme = &THEMES[0]; // kitsune theme

        // Plain mode: byte-exact current format
        let plain_row = theme_row(kitsune_theme, true, false, true);
        assert_eq!(plain_row, " * kitsune  fox-orange identity default");

        let plain_row_inactive = theme_row(kitsune_theme, false, false, true);
        assert_eq!(
            plain_row_inactive,
            "   kitsune  fox-orange identity default"
        );

        // Non-TTY (styled=false): same as plain for text, but no SGR codes
        let non_tty_row = theme_row(kitsune_theme, true, false, false);
        assert_eq!(non_tty_row, " * kitsune  fox-orange identity default");

        // Styled TTY mode: contains name's accent SGR and palette swatch
        let styled_row = theme_row(kitsune_theme, true, true, false);

        // Should contain the SGR code for the accent color
        let accent_sgr = sgr(kitsune_theme.accent);
        assert!(
            styled_row.contains(&accent_sgr),
            "styled row should contain accent SGR"
        );

        // Should contain bullet dots (but they'll be colored)
        assert!(styled_row.contains('●'));

        // Should contain the theme name and description
        assert!(styled_row.contains("kitsune"));
        assert!(styled_row.contains("fox-orange identity default"));

        // Sanitized version should match plain text (dots and color codes removed)
        let sanitized = sanitize_terminal_text(&styled_row);
        assert!(sanitized.contains("kitsune"));
        assert!(sanitized.contains("fox-orange identity default"));
        // Dots get sanitized (● is kept as-is by sanitize, so we should see them)
        assert!(sanitized.contains('●'));
    }

    #[test]
    fn theme_row_handles_all_themes() {
        // Verify that all themes produce valid rows
        for (idx, theme) in THEMES.iter().enumerate() {
            let plain_row = theme_row(theme, idx == 0, false, true);
            let styled_row = theme_row(theme, idx == 0, true, false);

            // Plain row should match the expected format
            let marker = if idx == 0 { "*" } else { " " };
            assert!(plain_row.starts_with(&format!(" {marker} ")));
            assert!(plain_row.contains(theme.name));
            assert!(plain_row.contains(theme.description));

            // Styled row should contain the theme name and description
            assert!(styled_row.contains(theme.name));
            assert!(styled_row.contains(theme.description));

            // Styled row should contain bullet dots
            assert!(styled_row.contains('●'));
        }
    }
}
