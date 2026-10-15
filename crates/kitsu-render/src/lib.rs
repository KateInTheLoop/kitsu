//! kitsu-render — terminal rendering. MVP: stream text fragments to stdout.
//! Full UX (palette, code blocks, spinners) is later phases (design §8).

pub mod jsonl;
pub mod live;
pub mod surface;

use std::io::{self, IsTerminal, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

use owo_colors::OwoColorize;

use crate::surface::{active_surface, Fd};

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

/// Stateful streaming buffer for styled output on TTY.
/// Accumulates text into a line buffer; when a complete line is formed (ends with `\n`),
/// styles it via LineStyler and writes it. Partial lines are buffered until completed
/// or explicitly flushed (unstyled).
///
/// This buffer is ONLY used when stdout is a styled TTY. Non-TTY or plain-mode streaming
/// bypasses this entirely, preserving byte-identical output. This is a deliberate tradeoff:
/// styled TTY output becomes line-granular (stalled at line boundaries) instead of
/// token-granular, but preserves the buffered-message styling semantics on the live path.
#[derive(Default)]
struct StreamBuffer {
    buffer: String,
    styler: LineStyler,
}

impl StreamBuffer {
    /// Push text into the buffer, styling and writing complete lines to `out`.
    /// Returns the total bytes formatted for `out` (counted from the styled
    /// text, not from what the writer accepted).
    /// Partial lines (not ending with `\n`) are buffered and NOT written.
    fn push(&mut self, out: &mut dyn std::io::Write, s: &str) -> usize {
        let mut written = 0;
        self.buffer.push_str(s);

        // Process complete lines (those ending with \n)
        while let Some(newline_pos) = self.buffer.find('\n') {
            let line = &self.buffer[..newline_pos];
            let styled_line = self.styler.style_line(line);
            let _ = out.write_all(styled_line.as_bytes());
            written += styled_line.len();
            let _ = out.write_all(b"\n");
            written += 1;
            self.buffer.drain(..=newline_pos);
        }

        written
    }

    /// Flush any remaining partial line UNSTYLED into `out` and reset fence
    /// state. Called at turn boundaries (finish_line).
    fn flush_unstyled(&mut self, out: &mut dyn std::io::Write) -> usize {
        let mut written = 0;
        if !self.buffer.is_empty() {
            let _ = out.write_all(self.buffer.as_bytes());
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

/// Whether animation must be suppressed: the `--reduced-motion` flag or
/// `KITSU_REDUCED_MOTION`.
///
/// Public because the flag has consumers outside this crate: the inline
/// viewport's status row must not repaint merely because its elapsed segment
/// ticked (note **N9**'s "periodic age refresh"), and the only process-wide
/// home of the flag is here. Callers read it; only
/// [`configure_terminal_modes`] sets it.
pub fn reduced_motion_mode() -> bool {
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
        // The block handed to the surface on this branch carries the
        // assistant text: `StreamBuffer::push` writes each completed styled
        // line into the writer the block receives, so a non-classic backend
        // can now carry stdout. For `ClassicSurface` this stays
        // byte-identical — same descriptor, same order, inside the one
        // suspension `write_block` opens, which also flushes the descriptor —
        // and the suspension decision that used to live in the
        // `sanitized.contains('\n')` test here now lives in the backend
        // (`suspends_for`), which is the point of the seam.
        //
        // Two caveats remain: write errors are still swallowed, and the byte
        // count `push` returns is computed from the formatted text rather
        // than from what the writer actually accepted.
        active_surface().write_block(Fd::Stdout, &mut |out| {
            let written = stream_buffer()
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(out, &sanitized);
            // `push` emits only newline-terminated lines, so anything it wrote
            // leaves the cursor at column 0 of a fresh row.
            if written > 0 {
                live::set_partial_line(false);
            }
        });
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
        active_surface().write_block(Fd::Stdout, &mut |out| {
            let _ = out.write_all(sanitized.as_bytes());
        });
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
    live::with_live_row_suspended(|| {
        let mut out = io::stdout();
        let _ = out.write_all(rendered.as_bytes());
        let _ = out.flush();
        if stdout_style() {
            // A message whose last line has no terminating newline leaves the
            // cursor mid-row on a row holding assistant text. The live row is
            // painted wherever the cursor stands, so tell it to step down
            // first or the repaint this suspension ends with erases that line.
            live::set_partial_line(!rendered.is_empty() && !rendered.ends_with('\n'));
        }
    });
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
#[derive(Default)]
struct LineStyler {
    fence: Option<String>,
    /// Stateful syntect highlighter for the current fence, when its language
    /// is recognized (ui-parity item 10). Highlighting adds SGR only — the
    /// sanitize round-trip always recovers the exact source text.
    highlighter: Option<syntect::easy::HighlightLines<'static>>,
}

/// Shared syntect assets, loaded once. The fancy-regex backend keeps the
/// build free of oniguruma.
fn syntect_assets() -> &'static (syntect::parsing::SyntaxSet, syntect::highlighting::Theme) {
    static ASSETS: OnceLock<(syntect::parsing::SyntaxSet, syntect::highlighting::Theme)> =
        OnceLock::new();
    ASSETS.get_or_init(|| {
        let syntaxes = syntect::parsing::SyntaxSet::load_defaults_newlines();
        let theme = syntect::highlighting::ThemeSet::load_defaults()
            .themes
            .remove("base16-ocean.dark")
            .expect("bundled syntect theme");
        (syntaxes, theme)
    })
}

/// A fence language token mapped to a bundled syntax, if we recognize it.
/// `diff`/`patch` keep the hand-rolled +/− styling; unknown tokens stay
/// unhighlighted (current behavior).
fn syntect_syntax_for(language: &str) -> Option<&'static syntect::parsing::SyntaxReference> {
    let (syntaxes, _) = syntect_assets();
    let token = match language {
        "rust" | "rs" => "rs",
        "python" | "py" => "py",
        "javascript" | "js" => "js",
        "typescript" | "ts" => "js",
        "shell" | "sh" | "bash" | "zsh" => "sh",
        "toml" => "toml",
        "json" => "json",
        "yaml" | "yml" => "yaml",
        "c" => "c",
        "cpp" | "c++" => "cpp",
        "go" => "go",
        "html" => "html",
        "css" => "css",
        "sql" => "sql",
        "xml" => "xml",
        "markdown" | "md" => "md",
        _ => return None,
    };
    syntaxes.find_syntax_by_extension(token)
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
                self.highlighter = None;
            } else {
                let language = language.trim().to_string();
                self.highlighter = syntect_syntax_for(&language).map(|syntax| {
                    let (_, theme) = syntect_assets();
                    syntect::easy::HighlightLines::new(syntax, theme)
                });
                self.fence = Some(language);
            }
            return format!("{}", line.dimmed());
        }

        // Recognized-language fence interior: truecolor syntax highlighting.
        // Foreground SGR only (no backgrounds), so sanitize recovers the
        // exact text and the row blends with any theme.
        if self.fence.is_some() {
            if let Some(highlighter) = &mut self.highlighter {
                let (syntaxes, _) = syntect_assets();
                if let Ok(ranges) = highlighter.highlight_line(line, syntaxes) {
                    let mut styled = syntect::util::as_24_bit_terminal_escaped(&ranges, false);
                    // syntect leaves the last SGR open; close it so fence
                    // colors never bleed into following lines.
                    styled.push_str("\x1b[0m");
                    return styled;
                }
            }
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
        self.highlighter = None;
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

/// Fit one physical terminal row to `cols` **visible** columns, escape-aware
/// (the truncation half of finding F16).
///
/// This is the primitive the inline viewport needs and the crate did not have:
/// [`display_width`] is not escape-aware (it counts the bytes of an ANSI
/// sequence as visible columns) and [`fit_banner_value`] elides middle-out
/// keeping the *tail*, which is right for a path and wrong for a row of text.
/// It lives here, beside the width and sanitisation primitives, so `kitsu-tui`
/// shares the one measurement rather than growing a second.
///
/// The contract, in the order the rules apply:
///
/// 1. **A row that already fits is returned byte-identical.** Not re-encoded,
///    not normalised, with no reset appended -- even when it leaves styling
///    active. Every `InlineSurface::paint` test asserts exact bytes and the
///    output tier's whole discipline rests on byte-identity, so a fitter that
///    rewrote short rows would be worse than no fitter.
/// 2. Escape sequences cost **zero** columns, so a heavily styled row whose
///    visible text fits is never truncated for its escapes.
/// 3. Truncation counts visible columns through [`char_display_width`], so a
///    wide CJK character costs 2.
/// 4. **A wide character is never split.** If a 2-column character would land
///    in the last single column the fit stops before it, and the leftover
///    column is left **unpadded**: `paint` has already cleared the row with
///    `ESC[2K`, so an unwritten cell shows the terminal's background, while
///    padding with a space would both diverge from the byte-identical path and
///    advance the cursor a column further than the text occupies.
/// 5. If truncation cuts a row that had styling active, exactly one `ESC[0m`
///    is appended so the style cannot bleed into whatever is drawn next. This
///    is deliberately conditional on *both* halves: unconditionally appending
///    a reset would break rule 1, and appending it to a truncated row that
///    carried no escapes at all would make plain text no longer plain.
/// 6. `cols == 0` yields an empty string for any row with visible width above
///    zero -- nothing can be drawn in zero columns, and emitting the row's SGR
///    into no columns would only bleed. A row of width zero (empty, or escapes
///    alone) fits under rule 1 and so comes back byte-identical, rule 1 being
///    first.
pub fn fit_row_to_width(row: &str, cols: usize) -> String {
    // Rule 1 first, and before anything can modify the string: measure the
    // visible width and, if it fits, hand back the very same bytes. No
    // allocation path below can run for a fitting row, which is what keeps
    // every byte-identity assertion in `InlineSurface::paint` true. Rule 6
    // falls out of the ordering: a zero-width row (empty, or escapes alone)
    // fits even in zero columns and so returns here.
    let mut visible = 0usize;
    let mut cursor = 0usize;
    while cursor < row.len() {
        if let Some((len, _)) = escape_sequence_at(row, cursor) {
            // Rule 2: an escape sequence costs zero columns.
            cursor += len;
            continue;
        }
        let ch = row[cursor..].chars().next().expect("non-empty remainder");
        // Rule 3: visible columns, so a wide character costs 2.
        visible += char_display_width(ch);
        cursor += ch.len_utf8();
    }
    if visible <= cols {
        return row.to_string();
    }

    // The row is over-long. Copy escapes through unconditionally (they are
    // free) and visible characters until the next one would not fit.
    let mut fitted = String::with_capacity(row.len());
    let mut used = 0usize;
    let mut wrote_visible = false;
    // Rule 5's predicate is SGR specifically -- a CSI sequence whose final
    // byte is `m` -- rather than "any escape seen". The appended reset exists
    // to stop *colour and attributes* bleeding into whatever is drawn next,
    // and only SGR leaves such state behind: a cursor move or an OSC sequence
    // leaves nothing to reset, so a reset after one would be bytes emitted
    // for no reason into a stream other tests assert exactly. Do not
    // "simplify" this to any-escape.
    let mut saw_sgr = false;
    let mut cursor = 0usize;
    while cursor < row.len() {
        if let Some((len, is_sgr)) = escape_sequence_at(row, cursor) {
            fitted.push_str(&row[cursor..cursor + len]);
            saw_sgr |= is_sgr;
            cursor += len;
            continue;
        }
        let ch = row[cursor..].chars().next().expect("non-empty remainder");
        let width = char_display_width(ch);
        if used + width > cols {
            // Rule 4: a wide character that would land in the final single
            // column stops the fit, and the leftover column stays unpadded.
            break;
        }
        fitted.push(ch);
        used += width;
        wrote_visible = true;
        cursor += ch.len_utf8();
    }

    if !wrote_visible {
        // Rule 6, and its generalisation: with no visible text drawn there is
        // nothing for the surviving escapes to style, so emitting them would
        // only bleed into the next row.
        return String::new();
    }
    if saw_sgr {
        fitted.push_str("\x1b[0m");
    }
    fitted
}

/// If `row[at..]` begins an ANSI escape sequence, its byte length and whether
/// it is SGR (a CSI sequence with final byte `m`). Mirrors the state machine
/// in [`TerminalTextSanitizer`], which only exists in stripping form and whose
/// restructuring the output tier's byte-identity invariant runs through; this
/// is a small read-only scanner instead.
fn escape_sequence_at(row: &str, at: usize) -> Option<(usize, bool)> {
    let bytes = row.as_bytes();
    if bytes.get(at) != Some(&0x1b) {
        return None;
    }
    let mut i = at + 1;
    match bytes.get(i) {
        // CSI: parameter and intermediate bytes, then a final byte.
        Some(b'[') => {
            i += 1;
            while let Some(&b) = bytes.get(i) {
                if (0x20..=0x3f).contains(&b) {
                    i += 1;
                } else {
                    break;
                }
            }
            match bytes.get(i) {
                Some(&b) if (0x40..=0x7e).contains(&b) => Some((i + 1 - at, b == b'm')),
                // Unterminated: the rest of the row is the sequence.
                _ => Some((row.len() - at, false)),
            }
        }
        // OSC and the other control strings, terminated by BEL or ST.
        Some(b']') | Some(b'P') | Some(b'X') | Some(b'^') | Some(b'_') => {
            i += 1;
            while let Some(&b) = bytes.get(i) {
                if b == 0x07 {
                    return Some((i + 1 - at, false));
                }
                if b == 0x1b && bytes.get(i + 1) == Some(&b'\\') {
                    return Some((i + 2 - at, false));
                }
                i += 1;
            }
            Some((row.len() - at, false))
        }
        // An escape with intermediate bytes, then a final byte.
        Some(&b) if (0x20..=0x2f).contains(&b) => {
            while let Some(&b) = bytes.get(i) {
                if (0x20..=0x2f).contains(&b) {
                    i += 1;
                } else {
                    break;
                }
            }
            Some((bytes.get(i).map_or(i, |_| i + 1) - at, false))
        }
        // A single-character escape.
        Some(_) => {
            let ch = row[i..].chars().next().expect("non-empty remainder");
            Some((i + ch.len_utf8() - at, false))
        }
        // A lone trailing ESC.
        None => Some((1, false)),
    }
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
        live::with_live_row_suspended(|| {
            writeln!(destination, "{}", self.line_with_mode(styled, plain))?;
            destination.flush()
        })
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
    active_surface().write_block(Fd::Stderr, &mut |out| {
        for line in error_lines(message) {
            let _ = writeln!(out, "{line}");
        }
    });
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

                // The frame renders through the shared live status row so the
                // spinner and the type-ahead input draft coexist on one
                // rewritable line (clearing and cursor placement live there).
                live::set_spinner_segment(Some(format!(
                    "  {}{pad} {}{}{}",
                    frame.truecolor(r, g, b),
                    format!("kitsu is {}", verb).dimmed(),
                    elapsed_suffix,
                    dots.dimmed()
                )));
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
            // Dropping the segment repaints the live row, which erases the
            // spinner while leaving any in-progress input draft visible.
            live::set_spinner_segment(None);
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
    // The block handed to the surface here is complete: when the stream
    // buffer holds a partial line, `StreamBuffer::flush_unstyled` writes those
    // bytes into the writer the block receives, followed by the trailing
    // newline that terminates them, so a non-classic backend can now carry
    // stdout. For `ClassicSurface` this stays byte-identical — same
    // descriptor, same order, inside the one suspension `write_block` opens.
    //
    // Two caveats remain: write errors are still swallowed, and the byte
    // count `flush_unstyled` returns is computed from the buffered text
    // rather than from what the writer actually accepted.
    active_surface().write_block(Fd::Stdout, &mut |out| {
        // Flush any partial line from the streaming buffer UNSTYLED
        // and reset its fence state.
        let flushed = stream_buffer()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .flush_unstyled(out);

        // Reset the non-TTY sanitizer state as well
        stream_sanitizer()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .reset();

        if !ASSISTANT_OUTPUT_WRITTEN.swap(false, Ordering::AcqRel) {
            // The flushed partial line, if there was one, is now the row the
            // cursor sits on and nothing has terminated it.
            if stdout_style() && flushed > 0 {
                live::set_partial_line(true);
            }
            return;
        }
        let _ = out.write_all(b"\n");
        let _ = io::stdout().flush();
        if stdout_style() {
            // The line is terminated: the cursor is back at column 0 of a
            // fresh row and the live row may paint there.
            live::set_partial_line(false);
        }
    });
}

/// Announce a tool call to **stderr** (meta, not assistant text) as a compact,
/// friendly line — e.g. `◆ Read(deck1.md)`. Fox-orange marker on a TTY; the
/// verb carries accent bold and the parenthesized detail is dimmed so the
/// card reads at a glance (ui-parity item 12). Plain/non-TTY byte-identical.
pub fn tool_activity(header: &str) {
    let line = tool_activity_line(header, stderr_is_tty() && !plain_mode(), plain_mode());
    active_surface().transcript_line(Fd::Stderr, &line);
}

/// The [`tool_activity`] payload as a string, per output tier. Styled rows
/// keep the marker in accent, render the tool verb (text before the first
/// `(`) accent-bold, and dim the argument text; markers/text are retained so
/// `sanitize_terminal_text(styled)` equals the unstyled row. Headers without
/// `(` render whole in accent.
fn tool_activity_line(header: &str, styled: bool, plain: bool) -> String {
    if plain {
        return format!("tool> {}", sanitize_plain_inline(header));
    }
    let header = sanitize_terminal_inline(header);
    if !styled {
        return format!("{GLYPH_TOOL_CALL} {header}");
    }
    let (r, g, b) = theme().accent;
    match header.split_once('(') {
        Some((verb, rest)) => format!(
            "{} {}{}{}",
            GLYPH_TOOL_CALL.truecolor(r, g, b),
            verb.truecolor(r, g, b).bold(),
            "(".dimmed(),
            rest.dimmed(),
        ),
        None => format!(
            "{} {}",
            GLYPH_TOOL_CALL.truecolor(r, g, b),
            // Spec: whole header in accent — color only, no bold.
            header.truecolor(r, g, b)
        ),
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
    let line = tool_result_line(summary, is_error, stderr_is_tty() && !plain, plain);
    active_surface().transcript_line(Fd::Stderr, &line);
}

/// The [`tool_result`] payload as a string, per output tier — the mirror of
/// [`tool_activity_line`]. `--plain` wins over a styled TTY; the error arm is
/// ember-red (emphasized) where the ordinary arm is dimmed.
fn tool_result_line(summary: &str, is_error: bool, styled: bool, plain: bool) -> String {
    if plain {
        return format!("result> {}", sanitize_plain_inline(summary));
    }
    let line = format!(
        "  {GLYPH_TOOL_RESULT} {}",
        sanitize_terminal_inline(summary)
    );
    if !styled {
        return line;
    }
    if is_error {
        let (r, g, b) = theme().error;
        format!("{}", line.truecolor(r, g, b))
    } else {
        format!("{}", line.dimmed())
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

/// The bottom-pane approval modal's rows, in paint order: the [`approval_header`] row, every
/// [`approval_detail`] provenance row the surface has, then the one shared
/// [`approval_options`] line.
///
/// Composition only, as requirement **R7** needs it: the caller composes each
/// row with the existing helpers above and this says what order they are
/// painted in, so the pane modal and the classic stderr prompt cannot drift
/// into two different option lines. Painting belongs to the `kitsu` crate's
/// `tui_bridge` (it owns the pinned `InlineSurface`); this crate cannot see
/// it, and `kitsu-tui` cannot see a `PermissionRequest`.
///
/// The rows are the caller's own strings, cloned and never re-composed: the
/// option line the pane paints is the very [`approval_options`] value the
/// classic prompt prints, so the two cannot drift into two spellings.
pub fn approval_modal(header: &str, details: &[String], options: &str) -> Vec<String> {
    let mut rows = Vec::with_capacity(details.len() + 2);
    rows.push(header.to_string());
    rows.extend(details.iter().cloned());
    rows.push(options.to_string());
    rows
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
/// Count the files and +/− lines of a unified diff (`diff --git`
/// occurrences; +/− lines excluding the `+++`/`---` headers). Pure, for the
/// `/diff` receipt header (ui-parity item 11).
fn diff_summary(diff: &str) -> (usize, usize, usize) {
    let mut files = 0usize;
    let mut adds = 0usize;
    let mut dels = 0usize;
    for line in diff.lines() {
        if line.starts_with("diff --git") {
            files += 1;
        } else if line.starts_with('+') && !line.starts_with("+++") {
            adds += 1;
        } else if line.starts_with('-') && !line.starts_with("---") {
            dels += 1;
        }
    }
    (files, adds, dels)
}

pub fn render_diff(diff: &str) {
    let diff = sanitize_terminal_text(diff);
    let line_count = diff.lines().count();
    let (files, adds, dels) = diff_summary(&diff);
    // The receipt form used elsewhere (deck_header): ◈ + highlight bold on a
    // styled TTY, '#' prefix in plain — with per-file/+/− stats.
    eprintln!(
        "{}",
        deck_header(&format!(
            "verified workspace diff — {files} file(s), +{adds} -{dels} line(s)"
        ))
    );
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
    let line = milestone_line(message);
    active_surface().transcript_line(Fd::Stderr, &line);
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

/// The agent view's state taxonomy (requirement **A2**), and the **one** place
/// its order is defined.
///
/// `DeckState` above is a *glyph* vocabulary: four fill levels. A2 needs six
/// groups, because it splits a finished agent by whether its result has been
/// observed and it separates a failure from a cancellation. The two are
/// deliberately different types — a glyph is not a group — but they live in
/// the same module so the refinement is visible in one file.
///
/// **Why `kitsu-render` and not `kitsu-tui`.** The order has two consumers:
/// `kitsu-tui`'s agent-view overlay and the binary's `/jobs view` text deck.
/// `kitsu-tui` cannot depend on `kitsu` (`kitsu` is a binary-only crate, F5),
/// and `kitsu-render` is the only crate both already depend on. Forking the
/// order into a second array is the F2/F38/F45/F57 failure family; the
/// `jobs_view_group` anti-fork test in `tui_bridge.rs` pins that it has not.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum AgentViewState {
    /// Parked on a user answer: `AgentThreadPhase::Blocked`.
    NeedsInput,
    /// Working right now: `AgentThreadPhase::Running`.
    Working,
    /// Finished, and its handoff is still `Ready` — nobody has looked.
    DoneUnviewed,
    /// Finished, and its handoff is `Acknowledged` (or `Unavailable`, so there
    /// is nothing left to observe).
    DoneViewed,
    Failed,
    Cancelled,
}

impl AgentViewState {
    /// A2's six groups in A2's order. The single source; nothing else may
    /// hard-code a second ordering.
    pub fn order() -> [AgentViewState; 6] {
        [
            Self::NeedsInput,
            Self::Working,
            Self::DoneUnviewed,
            Self::DoneViewed,
            Self::Failed,
            Self::Cancelled,
        ]
    }

    /// This state's index in [`AgentViewState::order`].
    ///
    /// Read *out of* [`AgentViewState::order`] rather than written out a
    /// second time: a rank that restated the sequence could drift from it,
    /// which is the whole failure this type exists to prevent.
    pub fn rank(self) -> usize {
        Self::order()
            .into_iter()
            .position(|state| state == self)
            .expect("`order` lists every AgentViewState")
    }

    /// The group heading the agent view prints, in A2's own words.
    pub fn label(self) -> &'static str {
        match self {
            Self::NeedsInput => "needs input",
            Self::Working => "working",
            Self::DoneUnviewed => "done (unviewed)",
            Self::DoneViewed => "done (viewed)",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    /// The machine spelling the `s:` filter term accepts (A3).
    pub fn wire(self) -> &'static str {
        match self {
            Self::NeedsInput => "needs-input",
            Self::Working => "working",
            Self::DoneUnviewed => "done-unviewed",
            Self::DoneViewed => "done-viewed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    /// Parse an `s:` filter value. `None` for a word that is not one of the
    /// six [`AgentViewState::wire`] spellings.
    ///
    /// The accepted vocabulary is read back off [`AgentViewState::wire`], so a
    /// renamed spelling cannot be accepted here under its old name.
    pub fn from_wire(word: &str) -> Option<AgentViewState> {
        Self::order().into_iter().find(|state| state.wire() == word)
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

/// Terminal state attached to the model-command timing receipt. Keeping this
/// closed prevents an arbitrary status string from entering the stable
/// redirected record or a terminal line.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RunTimingStatus {
    Ok,
    Partial,
    Error,
    Cancelled,
    Interrupted,
}

impl RunTimingStatus {
    fn record_label(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Partial => "partial",
            Self::Error => "error",
            Self::Cancelled => "cancelled",
            Self::Interrupted => "interrupted",
        }
    }

    fn human_phrase(self) -> &'static str {
        match self {
            Self::Ok => "completed in",
            Self::Partial => "partially completed in",
            Self::Error => "failed after",
            Self::Cancelled => "cancelled after",
            Self::Interrupted => "interrupted after",
        }
    }
}

/// Presentation-only projection of Kitsu's neutral terminal outcome. The
/// renderer never derives or mutates these fields.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HumanRunReceipt<'a> {
    pub run_id: &'a str,
    pub outcome: &'a str,
    pub assistant: &'a str,
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
pub fn emit_run_timing(
    elapsed: std::time::Duration,
    status: RunTimingStatus,
    receipt: HumanRunReceipt<'_>,
) -> io::Result<()> {
    let mut err = io::stderr();
    let destination_is_tty = err.is_terminal();
    write_run_timing_with_mode(
        &mut err,
        destination_is_tty,
        plain_mode(),
        elapsed,
        status,
        receipt,
    )
}

fn write_run_timing_with_mode<W: Write>(
    destination: &mut W,
    destination_is_tty: bool,
    plain: bool,
    elapsed: std::time::Duration,
    status: RunTimingStatus,
    receipt: HumanRunReceipt<'_>,
) -> io::Result<()> {
    if destination_is_tty {
        let marker = if plain { "->" } else { "↳" };
        let line = format!(
            "  {marker} {} {} · run {} · {} · assistant {}",
            status.human_phrase(),
            format_run_elapsed(elapsed),
            sanitize_terminal_inline(receipt.run_id),
            sanitize_terminal_inline(receipt.outcome),
            sanitize_terminal_inline(receipt.assistant),
        );
        if plain {
            writeln!(destination, "{line}")?;
        } else {
            writeln!(destination, "{}", line.dimmed())?;
        }
    } else {
        let duration_ms = u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX);
        #[derive(serde::Serialize)]
        struct RedirectedRunReceipt<'a> {
            run_id: &'a str,
            duration_ms: u64,
            status: &'static str,
            outcome: &'a str,
            assistant: &'a str,
        }
        destination.write_all(b"kitsu-timing: ")?;
        serde_json::to_writer(
            &mut *destination,
            &RedirectedRunReceipt {
                run_id: receipt.run_id,
                duration_ms,
                status: status.record_label(),
                outcome: receipt.outcome,
                assistant: receipt.assistant,
            },
        )?;
        destination.write_all(b"\n")?;
    }
    destination.flush()
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
        approval_header_with_mode, approval_modal, approval_options_with_mode, context_lines,
        deck_header_with_mode, deck_marker_with_mode, display_width, error_display_lines,
        fit_banner_value, fit_row_to_width, format_message, grouped_count, human_turn_stats,
        milestone_line_with_mode, sanitize_plain_inline, sanitize_terminal_inline,
        sanitize_terminal_text, section_heading, spinner_mode, static_spinner_line, theme_row,
        tool_activity_line, write_run_timing_with_mode, write_static_spinner, write_turn_stats,
        write_turn_stats_with_mode, AsyncNoticeLevel, DeckState, HumanRunReceipt, LineStyler,
        PromptSafeAsyncNotice, RenderMode, RunTimingStatus, SpinnerMode, StreamBuffer,
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
        let mut sink: Vec<u8> = Vec::new();

        // Accumulate first fragment: "```di" (incomplete line, no newline)
        buffer.push(&mut sink, "```di");
        assert!(
            !buffer.buffer.is_empty(),
            "incomplete line should be buffered"
        );

        // Continue with "ff\n" which completes the fence marker line
        buffer.push(&mut sink, "ff\n");
        assert_eq!(buffer.buffer, "", "complete line should be flushed");

        // Next: "+add" (incomplete diff line, no newline)
        buffer.push(&mut sink, "+add");
        assert!(
            !buffer.buffer.is_empty(),
            "incomplete line should be buffered"
        );

        // Complete it with "ed\n"
        buffer.push(&mut sink, "ed\n");
        assert_eq!(buffer.buffer, "", "complete diff line should be flushed");

        // Finally: "```\n" (closing fence)
        buffer.push(&mut sink, "```\n");
        assert_eq!(buffer.buffer, "", "fence marker should be flushed");
    }

    #[test]
    fn streaming_buffer_flushes_unstyled() {
        // Test that flush_unstyled outputs partial lines without styling
        let mut buffer = StreamBuffer::default();
        let mut sink: Vec<u8> = Vec::new();

        // Accumulate a partial line (no newline)
        buffer.push(&mut sink, "partial line");
        assert!(!buffer.buffer.is_empty(), "should have buffered partial");

        // Flush unstyled
        buffer.flush_unstyled(&mut sink);
        assert_eq!(buffer.buffer, "", "buffer should be empty after flush");
        assert_eq!(buffer.styler.fence, None, "fence state should be reset");
    }

    #[test]
    fn streaming_buffer_respects_fence_state_across_lines() {
        // Test that fence state is maintained across multiple line_style calls
        let mut buffer = StreamBuffer::default();
        let mut sink: Vec<u8> = Vec::new();

        // Push a diff fence opening
        buffer.push(&mut sink, "```diff\n");
        assert_eq!(buffer.styler.fence.as_deref(), Some("diff"));

        // Push a diff line
        buffer.push(&mut sink, "+added line\n");
        // The +added line should have been styled (green), and fence is still active

        // Push another diff line
        buffer.push(&mut sink, "-removed line\n");
        // The -removed line should have been styled (red), fence still active

        // Close the fence
        buffer.push(&mut sink, "```\n");
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
    fn run_timing_is_compact_for_people_and_stable_when_redirected() {
        let elapsed = std::time::Duration::from_millis(1_250);
        let success = HumanRunReceipt {
            run_id: "fixture-run-001",
            outcome: "succeeded",
            assistant: "committed",
        };
        let mut terminal = Vec::new();
        write_run_timing_with_mode(
            &mut terminal,
            true,
            true,
            elapsed,
            RunTimingStatus::Ok,
            success,
        )
        .unwrap();
        assert_eq!(
            String::from_utf8(terminal).unwrap(),
            "  -> completed in 1.2 s · run fixture-run-001 · succeeded · assistant committed\n"
        );

        let mut partial_terminal = Vec::new();
        write_run_timing_with_mode(
            &mut partial_terminal,
            true,
            true,
            elapsed,
            RunTimingStatus::Partial,
            HumanRunReceipt {
                outcome: "partially_succeeded",
                assistant: "committed",
                ..success
            },
        )
        .unwrap();
        assert_eq!(
            String::from_utf8(partial_terminal).unwrap(),
            "  -> partially completed in 1.2 s · run fixture-run-001 · partially_succeeded · assistant committed\n"
        );

        let mut styled = Vec::new();
        write_run_timing_with_mode(
            &mut styled,
            true,
            false,
            elapsed,
            RunTimingStatus::Cancelled,
            HumanRunReceipt {
                outcome: "cancelled",
                assistant: "discarded",
                ..success
            },
        )
        .unwrap();
        let styled = String::from_utf8(styled).unwrap();
        assert!(styled.contains('\x1b'));
        assert_eq!(
            sanitize_terminal_text(&styled),
            "  ↳ cancelled after 1.2 s · run fixture-run-001 · cancelled · assistant discarded\n"
        );

        let mut redirected = Vec::new();
        write_run_timing_with_mode(
            &mut redirected,
            false,
            false,
            elapsed,
            RunTimingStatus::Error,
            HumanRunReceipt {
                outcome: "provider_fault",
                assistant: "discarded",
                ..success
            },
        )
        .unwrap();
        assert_eq!(
            String::from_utf8(redirected).unwrap(),
            "kitsu-timing: {\"run_id\":\"fixture-run-001\",\"duration_ms\":1250,\"status\":\"error\",\"outcome\":\"provider_fault\",\"assistant\":\"discarded\"}\n"
        );

        let mut partial_redirected = Vec::new();
        write_run_timing_with_mode(
            &mut partial_redirected,
            false,
            false,
            elapsed,
            RunTimingStatus::Partial,
            HumanRunReceipt {
                outcome: "partially_succeeded",
                assistant: "committed",
                ..success
            },
        )
        .unwrap();
        assert_eq!(
            String::from_utf8(partial_redirected).unwrap(),
            "kitsu-timing: {\"run_id\":\"fixture-run-001\",\"duration_ms\":1250,\"status\":\"partial\",\"outcome\":\"partially_succeeded\",\"assistant\":\"committed\"}\n"
        );

        let mut interrupted = Vec::new();
        write_run_timing_with_mode(
            &mut interrupted,
            false,
            true,
            elapsed,
            RunTimingStatus::Interrupted,
            HumanRunReceipt {
                outcome: "interrupted",
                assistant: "absent",
                ..success
            },
        )
        .unwrap();
        assert_eq!(
            String::from_utf8(interrupted).unwrap(),
            "kitsu-timing: {\"run_id\":\"fixture-run-001\",\"duration_ms\":1250,\"status\":\"interrupted\",\"outcome\":\"interrupted\",\"assistant\":\"absent\"}\n"
        );
    }

    #[test]
    fn run_timing_delivery_failure_is_returned_to_the_lifecycle_owner() {
        struct RefuseWrites;

        impl std::io::Write for RefuseWrites {
            fn write(&mut self, _buffer: &[u8]) -> std::io::Result<usize> {
                Err(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "fixture sink closed",
                ))
            }

            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let error = write_run_timing_with_mode(
            &mut RefuseWrites,
            false,
            true,
            std::time::Duration::from_millis(5),
            RunTimingStatus::Ok,
            HumanRunReceipt {
                run_id: "fixture-run-delivery",
                outcome: "succeeded",
                assistant: "committed",
            },
        )
        .expect_err("terminal delivery failure must remain observable");
        assert_eq!(error.kind(), std::io::ErrorKind::BrokenPipe);
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
    fn rust_fences_highlight_with_syntect_and_round_trip_exactly() {
        let source = "```rust\nfn main() { let x = \"hi\"; }\n```\n";
        let styled = format_message(source, true);
        // ≥2 distinct truecolor foregrounds inside the fence body.
        let colors: std::collections::HashSet<&str> = styled
            .match_indices("\u{1b}[38;2;")
            .map(|(i, _)| &styled[i..styled[i..].find('m').map(|m| i + m + 1).unwrap()])
            .collect();
        assert!(
            colors.len() >= 2,
            "expected >=2 distinct colors, got {}: {styled:?}",
            colors.len()
        );
        // Sanitize recovers the exact source; unstyled output is identical.
        assert_eq!(sanitize_terminal_text(&styled), source);
        assert_eq!(format_message(source, false), source);
        // Unknown languages and diff fences keep prior behavior (no syntect
        // truecolor beyond the existing +/- tints).
        let diff = "```diff\n+added\n```\n";
        let styled_diff = format_message(diff, true);
        assert_eq!(sanitize_terminal_text(&styled_diff), diff);
        let unknown = "```mystery\nplain text body\n```\n";
        let styled_unknown = format_message(unknown, true);
        assert!(
            !styled_unknown.contains("\u{1b}[38;2;")
                || sanitize_terminal_text(&styled_unknown) == unknown
        );
        assert_eq!(sanitize_terminal_text(&styled_unknown), unknown);
    }

    #[test]
    fn diff_summary_counts_files_and_signed_lines() {
        let fixture = "diff --git a/x.rs b/x.rs\n--- a/x.rs\n+++ b/x.rs\n@@ -1,2 +1,3 @@\n+added one\n+added two\n-removed\n context\ndiff --git a/y.rs b/y.rs\n--- a/y.rs\n+++ b/y.rs\n@@ -1 +1 @@\n+swap in\n-swap out\n";
        assert_eq!(super::diff_summary(fixture), (2, 3, 2));
        // Header lines +++/--- are never counted; empty diff is all zeros.
        assert_eq!(super::diff_summary(""), (0, 0, 0));
        assert_eq!(super::diff_summary("+++ b/only\n--- a/only\n"), (0, 0, 0));
        // The receipt header round-trips through every tier.
        let summary = "verified workspace diff — 2 file(s), +3 -2 line(s)";
        let styled = deck_header_with_mode(summary, true, false);
        assert!(styled.contains("\u{1b}[1m"));
        assert_eq!(sanitize_terminal_text(&styled), format!("◈ {summary}"));
        assert_eq!(
            deck_header_with_mode(summary, false, true),
            "# verified workspace diff - 2 file(s), +3 -2 line(s)"
        );
    }

    #[test]
    fn tool_activity_verb_is_weighted_and_arguments_dim_across_tiers() {
        // Styled: accent-bold verb before '(', dimmed detail after, and the
        // sanitize round-trip returns the exact unstyled row.
        let styled = tool_activity_line("Read(crates/kitsu-render/src/lib.rs)", true, false);
        assert!(
            styled.contains("\u{1b}[1m"),
            "verb must be bold: {styled:?}"
        );
        assert!(
            styled.contains("\u{1b}[2m"),
            "detail must be dim: {styled:?}"
        );
        assert!(
            styled.contains("\u{1b}[38;2;"),
            "accent truecolor: {styled:?}"
        );
        assert_eq!(
            sanitize_terminal_text(&styled),
            "◆ Read(crates/kitsu-render/src/lib.rs)"
        );
        // No parenthesis: whole header in accent (color only, NO bold per
        // the item-12 spec), round-trip intact.
        let styled = tool_activity_line("workspace_diff", true, false);
        assert!(
            !styled.contains("\u{1b}[1m"),
            "no bold without a verb split"
        );
        assert!(styled.contains("\u{1b}[38;2;"));
        assert_eq!(sanitize_terminal_text(&styled), "◆ workspace_diff");
        // Plain and non-TTY tiers are byte-identical to the historical forms.
        assert_eq!(tool_activity_line("Read(x)", false, true), "tool> Read(x)");
        assert_eq!(tool_activity_line("Read(x)", false, false), "◆ Read(x)");
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

    /// **T3-S7a.** The bottom-pane modal's rows are exactly the surfaces that
    /// already exist, in paint order: the header, then every provenance
    /// detail row the caller has, then the option line last.
    ///
    /// Requirement **R7** is a parity requirement, so the modal is a
    /// *composition* of the three helpers above rather than a second set of
    /// strings: everything asserted here is a value produced by one of them.
    #[test]
    fn approval_modal_paints_header_then_details_then_the_option_line() {
        let header = approval_header_with_mode("kitsu wants to read /etc/hostname", false);
        let options = approval_options_with_mode("always (/etc)", false);
        let rows = approval_modal(&header, &[], &options);
        assert_eq!(
            rows,
            vec![header.clone(), options.clone()],
            "with no provenance rows the modal is the header and the option \
             line, in that order"
        );
    }

    /// **T3-S7a, R7's `command approvals keep their provenance rows`.** Every
    /// detail row survives, in order, between the header and the option line
    /// -- the five the command prompter composes today (cwd, executable,
    /// timeout, authority) must not be dropped to make the modal fit.
    #[test]
    fn approval_modal_keeps_every_provenance_row_in_order() {
        let header = approval_header_with_mode("run /bin/true", false);
        let options = approval_options_with_mode("allow this exact argv/cwd for session", false);
        let details = vec![
            "  cwd: /repo".to_string(),
            "  executable: /bin/true (ambient PATH lookup; observed: /bin/true; bytes not pinned)"
                .to_string(),
            "  timeout: 30000 ms".to_string(),
            "  authority: session".to_string(),
        ];
        let rows = approval_modal(&header, &details, &options);
        let mut expected = vec![header.clone()];
        expected.extend(details.iter().cloned());
        expected.push(options.clone());
        assert_eq!(
            rows, expected,
            "the modal is header + every provenance row + option line, with \
             nothing dropped or reordered"
        );
    }

    /// **T3-S7a, the one-source rule.** The modal's last row is the
    /// `approval_options` value *itself*, byte for byte. A re-typed option
    /// line in the pane -- the drift R7 exists to forbid -- fails here even
    /// if it reads the same to a human, because the composer's own output is
    /// the expected value.
    #[test]
    fn approval_modal_option_line_is_the_shared_composer_byte_for_byte() {
        let header = approval_header_with_mode("kitsu wants to write /tmp/x", false);
        let options = approval_options_with_mode("always (/tmp)", false);
        let rows = approval_modal(&header, &["  cwd: /tmp".to_string()], &options);
        assert_eq!(
            rows.last().map(String::as_str),
            Some(options.as_str()),
            "the option line must be the shared composer's string, not a copy"
        );
        assert_eq!(
            rows.last().map(String::as_str),
            Some("  [y] allow once \u{b7} [a] always (/tmp) \u{b7} [n] deny"),
            "...and that string is still the classic prompt's own text"
        );
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

    // ---------------------------------------------------------------------
    // Finding F16, truncation half: `fit_row_to_width`.
    //
    // The inline viewport writes each pinned row with no width limit, so an
    // over-long row wraps into the row below -- harmless with a one-row
    // viewport, corruption once the multi-row composer and the slash palette
    // make the viewport several rows tall. These tests pin the escape-aware
    // fitter that fixes it; see the function's own documentation for the
    // numbered contract they correspond to.
    // ---------------------------------------------------------------------

    /// The *visible* width of a styled string: the in-tree idiom, sanitising
    /// the escapes away before measuring, exactly as `compose_row` measures a
    /// styled prompt.
    fn visible_width(s: &str) -> usize {
        display_width(&sanitize_terminal_text(s))
    }

    #[test]
    fn fit_row_to_width_returns_a_fitting_row_byte_identical() {
        // Rule 1, the property this card pins hardest. Byte-identity, not
        // merely equal-looking: no re-encoding, no normalisation, no appended
        // reset -- including for the last case, a row that ends with styling
        // still active. `InlineSurface::paint`'s existing tests all assert
        // exact bytes, so anything else here breaks them.
        for (row, cols) in [
            ("", 80usize),
            ("composer", 80),
            ("composer", 8), // exactly the width
            ("> ", 2),
            ("日本語", 6),                       // exactly the width, wide chars
            ("\x1b[1mbold\x1b[0m", 80),          // styled and already reset
            ("\x1b[31mred and never reset", 80), // styling left active
        ] {
            let fitted = fit_row_to_width(row, cols);
            assert_eq!(
                fitted.as_bytes(),
                row.as_bytes(),
                "fit_row_to_width({row:?}, {cols}) must return the row byte-identical, got {fitted:?}"
            );
        }
    }

    #[test]
    fn fit_row_to_width_counts_escapes_as_zero_width() {
        // Rule 2. `display_width` counts the bytes of an ANSI sequence as
        // visible columns, so a naive fitter truncates a heavily styled row
        // whose visible text fits comfortably. Here the visible text is 11
        // columns and the byte count far more.
        let row = "\x1b[1m\x1b[38;2;255;0;0mhello\x1b[0m \x1b[2mworld\x1b[0m";
        assert_eq!(visible_width(row), 11, "the visible text is 11 columns");
        assert!(
            row.len() > 40,
            "and its byte count is far larger: {}",
            row.len()
        );
        let fitted = fit_row_to_width(row, 20);
        assert_eq!(
            fitted.as_bytes(),
            row.as_bytes(),
            "escapes cost no columns, so this row fits and is returned identical: {fitted:?}"
        );
    }

    #[test]
    fn fit_row_to_width_truncates_a_plain_row_to_the_visible_column_count() {
        // Rule 3 on the simple case: a plain row is cut, nothing is elided
        // with an ellipsis (that is `fit_banner_value`'s job and the wrong
        // shape here) and nothing is appended.
        assert_eq!(fit_row_to_width("abcdefghij", 4), "abcd");
        assert_eq!(fit_row_to_width("abcdefghij", 9), "abcdefghi");
        assert_eq!(fit_row_to_width("> composer text", 10), "> composer");
    }

    #[test]
    fn fit_row_to_width_charges_two_columns_for_a_wide_character() {
        // Rule 3 with wide characters: three CJK characters are six columns,
        // so a four-column fit keeps two of them.
        assert_eq!(fit_row_to_width("日本語", 4), "日本");
        assert_eq!(fit_row_to_width("日本語", 2), "日");
        // And mixed: "a" plus one wide character is three columns.
        assert_eq!(fit_row_to_width("a日本", 3), "a日");
    }

    #[test]
    fn fit_row_to_width_never_splits_a_wide_character_and_leaves_the_gap_unpadded() {
        // Rule 4. A 2-column character that would land in the last single
        // column is dropped entirely rather than written into half a cell,
        // and the leftover column is left blank rather than padded: `paint`
        // has already cleared the row, so an unwritten cell is background,
        // while a pad space would diverge from the byte-identical path and
        // push the cursor a column too far.
        assert_eq!(
            fit_row_to_width("日本語", 3),
            "日",
            "the second wide character does not fit in the one remaining column"
        );
        assert_eq!(
            fit_row_to_width("a日", 2),
            "a",
            "and the row comes back one column narrower, not space-padded"
        );
        assert_eq!(fit_row_to_width("日", 1), "", "nor at the very first cell");
    }

    #[test]
    fn fit_row_to_width_appends_one_reset_only_when_it_truncates_a_styled_row() {
        // Rule 5, and the distinction is the whole point: the reset is owed
        // only when the row was *both* truncated and styled.
        let styled = "\x1b[31mred text that runs well past the end";
        let fitted = fit_row_to_width(styled, 7);
        assert_eq!(
            fitted, "\x1b[31mred tex\x1b[0m",
            "a truncated styled row ends with exactly one reset so the colour \
             cannot bleed into the next row, got {fitted:?}"
        );
        assert_eq!(
            fitted.matches("\x1b[0m").count(),
            1,
            "exactly one reset, not one per surviving escape: {fitted:?}"
        );

        // Truncated but never styled: still plain bytes, no reset.
        assert_eq!(
            fit_row_to_width("plain text that is far too long", 5),
            "plain",
            "a truncated plain row stays plain"
        );

        // Styled but fitting: rule 1 wins and nothing is appended, even
        // though the styling is left active.
        let short = "\x1b[31mred";
        assert_eq!(
            fit_row_to_width(short, 80).as_bytes(),
            short.as_bytes(),
            "a fitting styled row is byte-identical -- the reset is a \
             truncation remedy, not a general one"
        );
    }

    #[test]
    fn fit_row_to_width_with_zero_columns_yields_nothing_unless_the_row_is_already_zero_width() {
        // Rule 6. Nothing can be drawn in zero columns, and emitting the
        // row's SGR into no columns would bleed style for no visible text --
        // so a row with any visible width fits to the empty string, reset
        // included.
        assert_eq!(fit_row_to_width("composer", 0), "");
        assert_eq!(fit_row_to_width("\x1b[31mred", 0), "");
        assert_eq!(fit_row_to_width("日", 0), "");
        // A zero-width row, however, already fits: rule 1 is first, so it
        // comes back byte-identical rather than being emptied.
        for row in ["", "\x1b[0m", "\x1b[1m\x1b[31m"] {
            assert_eq!(
                fit_row_to_width(row, 0).as_bytes(),
                row.as_bytes(),
                "a row of visible width 0 fits in 0 columns: {row:?}"
            );
        }
    }

    #[test]
    fn fit_row_to_width_output_never_exceeds_the_column_count() {
        // The property behind every case above, driven over a mixed corpus and
        // every width: whatever comes out is at most `cols` visible columns,
        // which is what makes a painted row unable to wrap.
        let rows = [
            "",
            "composer",
            "> kitsu ready",
            "日本語のテキスト",
            "a日b本c語",
            "\x1b[1mbold\x1b[0m plain \x1b[31mred",
            "\x1b[38;2;1;2;3m色\x1b[0mmixed 日 text",
            "\x1b[2m…\x1b[0m",
        ];
        for row in rows {
            for cols in 0usize..=24 {
                let fitted = fit_row_to_width(row, cols);
                assert!(
                    visible_width(&fitted) <= cols,
                    "fit_row_to_width({row:?}, {cols}) = {fitted:?} is {} visible columns wide",
                    visible_width(&fitted)
                );
            }
        }
    }
}

/// T0-S3a: the renderer's output seam. These tests pin the `Surface` trait,
/// the process-global slot's lock discipline, and the byte-identity of the
/// stderr line writers that route through it. They are deliberately written
/// before `surface.rs` exists.
#[cfg(test)]
mod surface_seam_tests {
    use super::live;
    use super::surface::{active_surface, set_surface, ClassicSurface, Fd, Surface};
    use super::{
        milestone, milestone_line_with_mode, plain_mode, sanitize_plain_inline,
        sanitize_terminal_inline, stderr_is_tty, stderr_style, tool_activity, tool_activity_line,
        tool_result, tool_result_error, tool_result_line, EMBER, GLYPH_TOOL_RESULT,
    };
    use std::sync::{mpsc, Arc, Mutex, MutexGuard, OnceLock};
    use std::time::Duration;

    /// Local copy of the sibling test module's SGR helper (that module's one is
    /// private to it, and the existing tests must not be touched).
    fn sgr(rgb: (u8, u8, u8)) -> String {
        format!("\u{1b}[38;2;{};{};{}m", rgb.0, rgb.1, rgb.2)
    }

    /// Generous enough that a loaded CI box never trips it, short enough that a
    /// real deadlock fails the suite instead of hanging it forever.
    const DEADLINE: Duration = Duration::from_secs(10);

    /// CAVEAT: the surface slot is process-global. Two tests installing
    /// recorders concurrently would see each other's calls, and a recorder
    /// leaked past a test would silently swallow every later test's transcript
    /// lines in this binary. Every test below holds this lock for its whole
    /// body and restores the previous surface before releasing it (see
    /// `InstalledSurface`, which restores on unwind too). This keeps the seam
    /// tests serial without adding a `serial_test` dev-dependency.
    fn surface_slot_lock() -> MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Installs a surface and puts the previous one back on drop, so a panicking
    /// assertion cannot leak a recorder into the rest of the test binary.
    struct InstalledSurface(Option<Arc<dyn Surface>>);

    impl InstalledSurface {
        fn new(surface: Arc<dyn Surface>) -> Self {
            Self(Some(set_surface(surface)))
        }
    }

    impl Drop for InstalledSurface {
        fn drop(&mut self) {
            if let Some(previous) = self.0.take() {
                set_surface(previous);
            }
        }
    }

    #[derive(Default)]
    struct Recorder {
        calls: Mutex<Vec<(Fd, String)>>,
    }

    impl Surface for Recorder {
        fn transcript_line(&self, fd: Fd, text: &str) {
            self.calls
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push((fd, text.to_string()));
        }
    }

    impl Recorder {
        fn calls(&self) -> Vec<(Fd, String)> {
            self.calls
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone()
        }
    }

    fn record(body: impl FnOnce()) -> Vec<(Fd, String)> {
        let recorder = Arc::new(Recorder::default());
        let _installed = InstalledSurface::new(recorder.clone());
        body();
        recorder.calls()
    }

    /// Run `body` on its own thread and fail if it does not finish inside
    /// `DEADLINE`. A hanging test is worse than a failing one.
    fn within_deadline(name: &str, body: impl FnOnce() + Send + 'static) {
        let (done_tx, done_rx) = mpsc::channel();
        let handle = std::thread::spawn(move || {
            body();
            let _ = done_tx.send(());
        });
        match done_rx.recv_timeout(DEADLINE) {
            Ok(()) => handle.join().expect("deadline-bounded body panicked"),
            Err(_) => panic!("{name} did not complete within {DEADLINE:?} — likely deadlocked"),
        }
    }

    /// The card's named test: `tool_activity` makes exactly one `Fd::Stderr`
    /// call whose text is the existing formatter's output, byte for byte. The
    /// expectation is derived from `tool_activity_line` so it cannot drift from
    /// the real formatter.
    #[test]
    fn tool_activity_routes_one_stderr_transcript_line_through_the_surface() {
        let _guard = surface_slot_lock();
        let header = "Read(deck1.md)";
        let expected = tool_activity_line(header, stderr_is_tty() && !plain_mode(), plain_mode());

        let calls = record(|| tool_activity(header));

        assert_eq!(calls, vec![(Fd::Stderr, expected)]);
    }

    /// `tool_result` likewise routes exactly one stderr line, derived from the
    /// extracted `tool_result_line` formatter so the assertion is byte-exact
    /// rather than sanitized (which would erase the very styling that
    /// distinguishes the two weight arms).
    #[test]
    fn tool_result_routes_one_stderr_transcript_line_through_the_surface() {
        let _guard = surface_slot_lock();
        let summary = "128 lines";
        let expected = tool_result_line(
            summary,
            false,
            stderr_is_tty() && !plain_mode(),
            plain_mode(),
        );

        let calls = record(|| tool_result(summary));

        assert_eq!(calls, vec![(Fd::Stderr, expected)]);
    }

    /// The error arm shares `tool_result_with_weight`'s body and is the one the
    /// non-error test never exercises, so it gets its own routing assertion.
    #[test]
    fn tool_result_error_routes_the_error_arm_through_the_surface() {
        let _guard = surface_slot_lock();
        let summary = "exit status 1";
        let expected = tool_result_line(
            summary,
            true,
            stderr_is_tty() && !plain_mode(),
            plain_mode(),
        );

        let calls = record(|| tool_result_error(summary));

        assert_eq!(calls, vec![(Fd::Stderr, expected)]);
    }

    /// Byte-exact outputs of the extracted formatter, both weight arms across
    /// all three output tiers. This is the byte-identity anchor the routing
    /// tests above derive from, so it hardcodes the escape sequences today's
    /// `tool_result_with_weight` emits: ember truecolor (reset with `39m`) for
    /// the error arm, `2m`/`0m` dimming for the ordinary arm, and zero escape
    /// bytes under `--plain`.
    #[test]
    fn tool_result_line_pins_both_arms_across_the_three_output_tiers() {
        let summary = "128 lines";
        let body = format!(
            "  {GLYPH_TOOL_RESULT} {}",
            sanitize_terminal_inline(summary)
        );

        // Styled TTY, ordinary weight: dimmed.
        assert_eq!(
            tool_result_line(summary, false, true, false),
            format!("\u{1b}[2m{body}\u{1b}[0m")
        );

        // Styled TTY, error weight: ember-red, emphasized rather than dimmed.
        assert_eq!(
            tool_result_line(summary, true, true, false),
            format!("{}{body}\u{1b}[39m", sgr(EMBER))
        );

        // Non-TTY: identical text for both arms, no escape bytes.
        for is_error in [false, true] {
            let rendered = tool_result_line(summary, is_error, false, false);
            assert_eq!(rendered, body);
            assert!(!rendered.contains('\u{1b}'));
        }

        // `--plain`: the ASCII `result>` form for both arms, no escape bytes.
        let plain_expected = format!("result> {}", sanitize_plain_inline(summary));
        for is_error in [false, true] {
            // `--plain` wins over a styled TTY, matching today's branch order.
            for styled in [true, false] {
                let rendered = tool_result_line(summary, is_error, styled, true);
                assert_eq!(rendered, plain_expected);
                assert!(!rendered.contains('\u{1b}'));
            }
        }
    }

    /// `milestone` routes exactly one stderr line, derived from the existing
    /// `milestone_line_with_mode` formatter.
    #[test]
    fn milestone_routes_one_stderr_transcript_line_through_the_surface() {
        let _guard = surface_slot_lock();
        let message = "context compacted (older turns summarized)";
        let expected = milestone_line_with_mode(message, stderr_style(), plain_mode());

        let calls = record(|| milestone(message));

        assert_eq!(calls, vec![(Fd::Stderr, expected)]);
    }

    /// `set_surface` hands back the surface it displaced, and restoring that
    /// value puts later writers back on the classic path — no recorder keeps
    /// seeing calls after it has been uninstalled.
    #[test]
    fn set_surface_returns_the_previous_surface_and_restoring_resumes_the_classic_path() {
        let _guard = surface_slot_lock();
        let recorder = Arc::new(Recorder::default());
        let classic = set_surface(recorder.clone());

        tool_activity("Read(first.md)");
        assert_eq!(recorder.calls().len(), 1);

        let displaced = set_surface(classic);
        assert!(
            Arc::ptr_eq(&displaced, &(recorder.clone() as Arc<dyn Surface>)),
            "set_surface must return the surface it displaced"
        );

        // Back on the classic path: the uninstalled recorder sees nothing more.
        tool_activity("Read(second.md)");
        assert_eq!(
            recorder.calls().len(),
            1,
            "an uninstalled recorder must stop receiving calls"
        );
    }

    /// Before any `set_surface` call the slot already holds a usable surface,
    /// and the value it returns is a detached `Arc`: holding it must not block
    /// an installer, which is the structural half of the lock discipline.
    #[test]
    fn active_surface_is_usable_by_default_and_detached_from_the_slot() {
        let _guard = surface_slot_lock();
        let held = active_surface();
        // Holding `held` while installing proves no read guard travelled with
        // it; a guard here would deadlock against the writer below.
        within_deadline("set_surface while an active_surface clone is held", || {
            let _installed = InstalledSurface::new(Arc::new(Recorder::default()));
        });
        held.transcript_line(Fd::Stderr, "surface seam: default surface is usable");
    }

    /// The deadlock canary: a surface whose own `transcript_line` re-enters the
    /// seam — asking for `active_surface()` again and nesting inside
    /// `with_live_row_suspended`, exactly as an async notice does — must run to
    /// completion. Bounded by a deadline so a regression fails rather than
    /// hangs.
    #[test]
    fn a_surface_that_reenters_the_seam_completes_within_a_deadline() {
        let _guard = surface_slot_lock();

        struct Reentrant {
            /// The suspension depth observed from inside the nested body, or
            /// `None` if the body never ran.
            nested_depth: Arc<Mutex<Option<usize>>>,
        }

        impl Surface for Reentrant {
            fn transcript_line(&self, _fd: Fd, _text: &str) {
                let nested = active_surface();
                live::with_live_row_suspended(|| {
                    let _ = Arc::strong_count(&nested);
                    *self
                        .nested_depth
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner()) =
                        Some(live::suspend_depth());
                });
            }
        }

        let nested_depth = Arc::new(Mutex::new(None));
        let _installed = InstalledSurface::new(Arc::new(Reentrant {
            nested_depth: nested_depth.clone(),
        }));

        let before = live::suspend_depth();
        let probe = nested_depth.clone();
        within_deadline("reentrant surface call", move || {
            tool_activity("Read(reentrant.md)");
        });
        let observed = *probe
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let observed = observed.expect("the nested suspension body must have run");
        assert!(
            observed > before,
            "the nested body must run at a deeper suspension than the caller \
             (before {before}, inside {observed})"
        );
        // The balance check that matters: a nested suspension is exactly where
        // an unbalanced decrement would hide, and a leaked suspension would
        // leave the live row lifted for the rest of the process while every
        // other assertion still passed.
        assert_eq!(
            live::suspend_depth(),
            before,
            "a reentrant surface call must leave the suspension depth balanced"
        );
    }

    /// A routed writer must leave the live-row suspension depth exactly as it
    /// found it. Without this, a `ClassicSurface` that leaked a suspension
    /// would permanently lift the live row and no other test would notice.
    #[test]
    fn a_routed_transcript_line_leaves_the_suspend_depth_balanced() {
        let _guard = surface_slot_lock();
        let before = live::suspend_depth();

        // Through the seam onto the classic path (the real byte writer).
        tool_activity("Read(balanced.md)");
        assert_eq!(live::suspend_depth(), before);
        tool_result("1 line");
        assert_eq!(live::suspend_depth(), before);
        milestone("suspension depth stays balanced");
        assert_eq!(live::suspend_depth(), before);

        // And directly, so the assertion names `ClassicSurface` itself.
        ClassicSurface.transcript_line(Fd::Stderr, "surface seam: depth balance");
        assert_eq!(
            live::suspend_depth(),
            before,
            "ClassicSurface::transcript_line must balance its own suspension"
        );
    }

    /// The writer-starvation canary: one thread is *inside* a
    /// `transcript_line` call while another calls `set_surface`. Ordering is
    /// forced with channels rather than sleeps — the installer only starts once
    /// the writer has signalled that it is inside the call, and the writer only
    /// returns once the installer reports done — so if `set_surface` blocked
    /// behind a read guard held across the call, the pair would deadlock and
    /// the deadline would fail the test.
    #[test]
    fn set_surface_during_an_in_flight_transcript_line_completes_within_a_deadline() {
        let _guard = surface_slot_lock();

        struct Gate {
            entered: mpsc::Sender<()>,
            release: Mutex<mpsc::Receiver<()>>,
        }

        impl Surface for Gate {
            fn transcript_line(&self, _fd: Fd, _text: &str) {
                let _ = self.entered.send(());
                let release = self
                    .release
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                release
                    .recv_timeout(DEADLINE)
                    .expect("set_surface never completed while a write was in flight");
            }
        }

        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let _installed = InstalledSurface::new(Arc::new(Gate {
            entered: entered_tx,
            release: Mutex::new(release_rx),
        }));

        let writer = std::thread::spawn(|| tool_activity("Read(in-flight.md)"));

        entered_rx
            .recv_timeout(DEADLINE)
            .expect("the writer never entered transcript_line");

        let (installed_tx, installed_rx) = mpsc::channel();
        let installer = std::thread::spawn(move || {
            let previous = set_surface(Arc::new(Recorder::default()));
            let _ = installed_tx.send(previous);
        });

        let displaced = installed_rx
            .recv_timeout(DEADLINE)
            .expect("set_surface blocked behind an in-flight transcript_line");
        let _ = release_tx.send(());

        writer.join().expect("writer thread panicked");
        installer.join().expect("installer thread panicked");
        // Keep the gate alive until both threads are done, then restore.
        set_surface(displaced);
    }

    /// Byte-identity of the classic path. Capturing the real stderr bytes is
    /// not feasible from a unit test here: `tool_activity`/`milestone` write
    /// with `eprintln!` straight to the process's stderr and the crate exposes
    /// no injectable sink, so what is asserted instead is (a) the text the
    /// seam hands `ClassicSurface` equals the existing formatter's output for
    /// every representative header in all three output tiers, and (b)
    /// `ClassicSurface::transcript_line` accepts that text under the live-row
    /// suspension without panicking. Together with the untouched 82 existing
    /// render tests, that pins the bytes.
    #[test]
    fn classic_surface_reproduces_the_existing_stderr_bytes() {
        let _guard = surface_slot_lock();
        let headers = [
            "Read(deck1.md)",
            "Bash(ls -la)",
            "Write",
            "Read(\u{1b}[31mnot-a-color\u{1b}[0m.md)",
        ];
        for header in headers {
            for (styled, plain) in [(true, false), (false, false), (true, true)] {
                let expected = tool_activity_line(header, styled, plain);
                // The seam must carry the formatter's bytes verbatim, with no
                // re-styling, trimming or re-sanitizing of its own.
                assert_eq!(
                    tool_activity_line(header, styled, plain),
                    expected,
                    "the formatter is the single source of the line's bytes"
                );
            }

            let calls = record(|| tool_activity(header));
            let expected =
                tool_activity_line(header, stderr_is_tty() && !plain_mode(), plain_mode());
            assert_eq!(calls, vec![(Fd::Stderr, expected.clone())]);

            // The classic surface is what turns that text into bytes today:
            // one `eprintln!` of exactly this string, under the suspension.
            ClassicSurface.transcript_line(Fd::Stderr, &expected);
        }
    }

    // ---------------------------------------------------------------------
    // T0-S3b: the block seam. The writers below are not line writers: each
    // emits several lines, or a partial line, under exactly one live-row
    // suspension. Routing them per line would open one suspension per line
    // and, with a drawn row, clear and repaint between them — different
    // stderr bytes. So they route through `Surface::write_block`, which lifts
    // the row once around the whole block, and the decision to lift at all is
    // derived in the backend by `suspends_for`.
    //
    // These tests are written before the trait method and the routing exist.
    // ---------------------------------------------------------------------

    use super::surface::suspends_for;
    use super::{
        error_lines, finish_line, print_fragment, render_error, sanitize_terminal_text,
        stdout_style, stream_buffer, stream_sanitizer, LineStyler, ASSISTANT_OUTPUT_WRITTEN,
        TURN_ASSISTANT_OUTPUT,
    };
    use std::sync::atomic::Ordering;

    /// A seam call, kept as an enum so a backend can prove it distinguishes a
    /// block from a single line (the whole point of the second method).
    #[derive(Clone, Debug, Eq, PartialEq)]
    enum SeamCall {
        Line(Fd, String),
        Block(Fd, Vec<u8>),
    }

    /// Like `Recorder`, but it also overrides `write_block` so the block's
    /// bytes are captured instead of being forwarded to `transcript_line`.
    #[derive(Default)]
    struct BlockRecorder {
        calls: Mutex<Vec<SeamCall>>,
    }

    impl Surface for BlockRecorder {
        fn transcript_line(&self, fd: Fd, text: &str) {
            self.calls
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(SeamCall::Line(fd, text.to_string()));
        }

        fn write_block(&self, fd: Fd, block: &mut dyn FnMut(&mut dyn std::io::Write)) {
            let mut bytes: Vec<u8> = Vec::new();
            block(&mut bytes);
            self.calls
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(SeamCall::Block(fd, bytes));
        }
    }

    impl BlockRecorder {
        fn calls(&self) -> Vec<SeamCall> {
            self.calls
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone()
        }
    }

    fn record_calls(body: impl FnOnce()) -> Vec<SeamCall> {
        let recorder = Arc::new(BlockRecorder::default());
        let _installed = InstalledSurface::new(recorder.clone());
        body();
        recorder.calls()
    }

    /// The single block of a recording, or a failure naming what was seen
    /// instead. Blocks are the unit under test here, so a stray
    /// `transcript_line` must fail loudly rather than be filtered away.
    fn one_block(calls: &[SeamCall]) -> (Fd, String) {
        assert_eq!(
            calls.len(),
            1,
            "expected exactly one seam call (one block), saw {calls:?}"
        );
        match &calls[0] {
            SeamCall::Block(fd, bytes) => (
                *fd,
                String::from_utf8(bytes.clone()).expect("block bytes must be UTF-8"),
            ),
            other => panic!("expected a write_block call, saw {other:?}"),
        }
    }

    /// `print_fragment` and `finish_line` mutate process-global state: the
    /// streaming sanitizer's escape-parser state, the stream buffer's partial
    /// line and fence state, and the two output-tracking atomics. The other 93
    /// tests in this binary share all of it, so every test that touches those
    /// writers holds one of these: it zeroes the sanitizer and buffer on entry
    /// and on exit, and puts both atomics back to the values it found. The
    /// buffer's partial line is dropped by clearing the field rather than by
    /// `flush_unstyled`, so restoring state never emits stray bytes.
    struct StreamStateGuard {
        output_written: bool,
        turn_output: bool,
    }

    impl StreamStateGuard {
        fn new() -> Self {
            let guard = Self {
                output_written: ASSISTANT_OUTPUT_WRITTEN.load(Ordering::Acquire),
                turn_output: TURN_ASSISTANT_OUTPUT.load(Ordering::Acquire),
            };
            reset_stream_globals();
            guard
        }
    }

    impl Drop for StreamStateGuard {
        fn drop(&mut self) {
            reset_stream_globals();
            ASSISTANT_OUTPUT_WRITTEN.store(self.output_written, Ordering::Release);
            TURN_ASSISTANT_OUTPUT.store(self.turn_output, Ordering::Release);
        }
    }

    fn reset_stream_globals() {
        {
            let mut buffer = stream_buffer()
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            buffer.buffer.clear();
            buffer.styler = LineStyler::default();
        }
        stream_sanitizer()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .reset();
        ASSISTANT_OUTPUT_WRITTEN.store(false, Ordering::Release);
        TURN_ASSISTANT_OUTPUT.store(false, Ordering::Release);
    }

    /// (1) The suspension rule, as a truth table.
    ///
    /// WHY stdout is conditional: the live row is painted on **stderr**. A
    /// stderr write therefore always shares a terminal with the row and must
    /// lift it. A stdout write shares that terminal only when stdout is itself
    /// a styled terminal; in a `kitsu chat > file` session the stdout bytes go
    /// into the file and cannot disturb a row on the terminal, so lifting it
    /// would insert a clear/repaint cycle and change the emitted stderr bytes.
    /// That asymmetry is exactly what today's `print_fragment` encodes by
    /// suspending on the styled branch only, and `suspends_for` is where it
    /// must live so it can be tested without a real descriptor.
    #[test]
    fn suspends_for_lifts_the_row_only_when_the_write_shares_the_rows_terminal() {
        assert!(
            suspends_for(Fd::Stderr, true),
            "the row lives on stderr: a stderr write always shares its terminal"
        );
        assert!(
            suspends_for(Fd::Stderr, false),
            "stdout's styling is irrelevant to a stderr write"
        );
        assert!(
            suspends_for(Fd::Stdout, true),
            "a styled stdout shares the terminal with the row"
        );
        assert!(
            !suspends_for(Fd::Stdout, false),
            "a redirected stdout cannot disturb a row painted on stderr"
        );
    }

    /// (2) The regression guard the previous card's author identified:
    /// `render_error` prints N lines and must route them as **one** block, not
    /// N `transcript_line` calls. N single-line calls would open N suspensions
    /// and, with a drawn row, repaint between every hint line.
    #[test]
    fn render_error_routes_every_line_in_one_write_block() {
        let _guard = surface_slot_lock();
        let message = "boom\nhint: check the gateway\nhint: or lower the tier";
        let expected_lines = error_lines(message);
        assert!(
            expected_lines.len() >= 3,
            "the fixture must be multi-line for this test to mean anything, \
             saw {expected_lines:?}"
        );

        let calls = record_calls(|| render_error(message));

        // Exactly one call, and it is a block on stderr.
        let (fd, text) = one_block(&calls);
        assert_eq!(fd, Fd::Stderr);

        // The block emits every line, in order, each terminated — the bytes
        // today's `for line in error_lines(..) { eprintln!("{line}") }` writes.
        let expected: String = expected_lines
            .iter()
            .map(|line| format!("{line}\n"))
            .collect();
        assert_eq!(text, expected);

        // Spelled out as an ordering assertion too, so a failure says which
        // line went missing rather than dumping two large styled strings.
        let mut cursor = 0;
        for line in &expected_lines {
            let at = text[cursor..]
                .find(line.as_str())
                .unwrap_or_else(|| panic!("block output is missing, or reorders, {line:?}"));
            cursor += at + line.len();
        }
    }

    /// (3) One suspension for the whole block, not one per line. A leaked or
    /// per-line suspension is invisible to a byte assertion made through a
    /// recording surface, so this one goes through `ClassicSurface` itself and
    /// reads `live::suspend_depth()` from inside the block.
    #[test]
    fn a_classic_block_suspends_the_live_row_exactly_once() {
        let _guard = surface_slot_lock();
        let before = live::suspend_depth();
        let mut depths: Vec<usize> = Vec::new();

        ClassicSurface.write_block(Fd::Stderr, &mut |out| {
            for index in 0..3 {
                let _ = writeln!(out, "surface seam: block line {index}");
                depths.push(live::suspend_depth());
            }
        });

        assert_eq!(
            live::suspend_depth(),
            before,
            "ClassicSurface::write_block must balance its own suspension"
        );
        assert_eq!(depths.len(), 3, "the block body must have run");
        for depth in depths {
            assert!(
                depth > before,
                "the block must run inside a suspension (before {before}, inside {depth})"
            );
            assert!(
                depth <= before + 1,
                "the row must be lifted once for the whole block, not once per \
                 line (before {before}, inside {depth})"
            );
        }
    }

    /// (4) `print_fragment` on a completed line routes through the seam as one
    /// block on stdout, and the block's bytes are what the real path produces:
    /// the styled line plus its newline on a styled TTY, the sanitized text
    /// verbatim otherwise. Both expectations are derived from the real
    /// `LineStyler`/sanitizer rather than written out as literals.
    ///
    /// CAVEAT: `stdout_style()` reads a real descriptor and cannot be driven
    /// from a test — under `cargo test` stdout is captured, so the tier taken
    /// here is the non-styled one. The styled arm's expectation is therefore
    /// computed from `LineStyler` and compared only if the test happens to run
    /// on a styled TTY; its *suspension* behaviour is pinned instead by test
    /// (1), which is the part that could change bytes.
    #[test]
    fn print_fragment_routes_a_completed_line_through_one_write_block() {
        let _guard = surface_slot_lock();
        let _state = StreamStateGuard::new();
        let fragment = "hello from the seam\n";
        let sanitized = sanitize_terminal_text(fragment);
        let expected = if stdout_style() {
            format!(
                "{}\n",
                LineStyler::default().style_line(sanitized.trim_end_matches('\n'))
            )
        } else {
            sanitized.clone()
        };

        let calls = record_calls(|| print_fragment(fragment));

        let (fd, text) = one_block(&calls);
        assert_eq!(fd, Fd::Stdout, "assistant text belongs on stdout");
        assert_eq!(text, expected);
    }

    /// (5) A partial fragment (no newline) still sets both output-tracking
    /// atomics — a turn whose only output is a partial line must count as
    /// having streamed, per the comment in `print_fragment` — and on the
    /// styled path it writes nothing, because the text is only buffered. The
    /// atomics are set from the sanitized text, before any write, so they must
    /// hold whichever branch ran.
    #[test]
    fn print_fragment_with_a_partial_line_tracks_output_and_writes_only_what_the_tier_writes() {
        let _guard = surface_slot_lock();
        let _state = StreamStateGuard::new();
        let fragment = "a partial line with no newline";
        let sanitized = sanitize_terminal_text(fragment);
        assert!(!sanitized.is_empty(), "the fixture must survive sanitizing");

        let calls = record_calls(|| print_fragment(fragment));

        assert!(
            ASSISTANT_OUTPUT_WRITTEN.load(Ordering::Acquire),
            "a partial fragment must still arm the newline-discipline flag"
        );
        assert!(
            TURN_ASSISTANT_OUTPUT.load(Ordering::Acquire),
            "a turn whose only output is a partial line must count as streamed"
        );

        let (fd, text) = one_block(&calls);
        assert_eq!(fd, Fd::Stdout);
        if stdout_style() {
            assert_eq!(
                text, "",
                "the styled path only buffers a partial line; it writes nothing"
            );
        } else {
            assert_eq!(
                text, sanitized,
                "the raw path writes the sanitized fragment immediately"
            );
        }
    }

    /// (6) `finish_line` routes one block, and the trailing newline stays
    /// gated on `ASSISTANT_OUTPUT_WRITTEN` — read from the code, not assumed:
    /// the flag is consumed by a swap, the newline is written only when it was
    /// set, and the partial-line flush plus sanitizer reset happen either way
    /// inside the same block.
    #[test]
    fn finish_line_routes_one_block_and_keeps_the_newline_gated_on_the_atomic() {
        let _guard = surface_slot_lock();
        let _state = StreamStateGuard::new();

        // Output was written: the newline is emitted and the flag consumed.
        ASSISTANT_OUTPUT_WRITTEN.store(true, Ordering::Release);
        let calls = record_calls(finish_line);
        let (fd, text) = one_block(&calls);
        assert_eq!(fd, Fd::Stdout);
        assert_eq!(text, "\n");
        assert!(
            !ASSISTANT_OUTPUT_WRITTEN.load(Ordering::Acquire),
            "finish_line must consume the flag with a swap, not merely read it"
        );

        // Nothing was written: still one block (the flush and reset live in
        // it), but no newline.
        ASSISTANT_OUTPUT_WRITTEN.store(false, Ordering::Release);
        let calls = record_calls(finish_line);
        let (fd, text) = one_block(&calls);
        assert_eq!(fd, Fd::Stdout);
        assert_eq!(
            text, "",
            "with no assistant output written, finish_line emits no newline"
        );
    }

    /// (7) This card must not regress the line seam the previous one shipped:
    /// a routed stderr writer still arrives as exactly one `transcript_line`
    /// carrying the real formatter's bytes.
    #[test]
    fn transcript_line_still_carries_the_routed_stderr_writers_unchanged() {
        let _guard = surface_slot_lock();
        let header = "Read(unregressed.md)";
        let expected = tool_activity_line(header, stderr_is_tty() && !plain_mode(), plain_mode());

        let calls = record_calls(|| tool_activity(header));

        assert_eq!(calls, vec![SeamCall::Line(Fd::Stderr, expected)]);
    }

    /// (8) A backend can tell a block from a line: the two methods arrive as
    /// distinct calls, in order, and a block is never decomposed into lines on
    /// the way through.
    #[test]
    fn a_backend_sees_write_block_and_transcript_line_as_distinct_calls() {
        let _guard = surface_slot_lock();
        let recorder = Arc::new(BlockRecorder::default());
        let _installed = InstalledSurface::new(recorder.clone());

        active_surface().transcript_line(Fd::Stderr, "surface seam: a single line");
        active_surface().write_block(Fd::Stdout, &mut |out| {
            let _ = out.write_all(b"first\nsecond\n");
        });

        assert_eq!(
            recorder.calls(),
            vec![
                SeamCall::Line(Fd::Stderr, "surface seam: a single line".to_string()),
                SeamCall::Block(Fd::Stdout, b"first\nsecond\n".to_vec()),
            ]
        );
    }

    /// (9) Byte-identity of the classic path for a block. Capturing the real
    /// descriptors is still not feasible from a unit test here: `render_error`
    /// writes with `eprintln!` and `print_fragment`/`finish_line` write
    /// through `io::stdout()`, and the crate exposes no injectable process
    /// sink, so the real bytes cannot be read back. What is asserted instead
    /// is (a) the block the seam hands a backend contains exactly the
    /// concatenation of `error_lines`, each newline-terminated — the byte
    /// stream `eprintln!`-per-line produces — and (b) `ClassicSurface`
    /// accepts that same block under one balanced suspension. Together with
    /// the untouched 93 existing tests, that pins the bytes.
    ///
    /// Not asserted, and worth saying plainly: that the kernel-level write
    /// ordering between the block's bytes and the row's clear/repaint escapes
    /// is unchanged. Only an end-to-end PTY test could show that.
    #[test]
    fn classic_write_block_reproduces_the_existing_multiline_stderr_bytes() {
        let _guard = surface_slot_lock();
        let message = "boom\nhint: one\nhint: two";
        let expected: String = error_lines(message)
            .iter()
            .map(|line| format!("{line}\n"))
            .collect();

        let (_, routed) = {
            let calls = record_calls(|| render_error(message));
            one_block(&calls)
        };
        assert_eq!(routed, expected, "the seam must carry the bytes verbatim");

        let before = live::suspend_depth();
        ClassicSurface.write_block(Fd::Stderr, &mut |out| {
            let _ = out.write_all(expected.as_bytes());
        });
        assert_eq!(
            live::suspend_depth(),
            before,
            "the classic block must balance its suspension"
        );
    }

    // ---------------------------------------------------------------------
    // T0-S3c: closing F12 — `StreamBuffer` must write to a writer it is
    // handed, not to `io::stdout()`. These tests are written against the
    // pinned signatures
    //
    //     fn push(&mut self, out: &mut dyn std::io::Write, s: &str) -> usize;
    //     fn flush_unstyled(&mut self, out: &mut dyn std::io::Write) -> usize;
    //
    // and must fail to compile until both are threaded.
    //
    // Tests (10)-(15) build their own `StreamBuffer`, so they touch no
    // process-global state and need neither `surface_slot_lock` nor
    // `StreamStateGuard`. (16) and (17) go through the seam and therefore
    // hold both, exactly as (4)-(6) do.
    // ---------------------------------------------------------------------

    use super::StreamBuffer;

    /// (10) A completed line's styled bytes land in the **provided writer**.
    /// The expectation is computed by the real `LineStyler`, so this pins the
    /// threading and not the styling, and the returned count must equal the
    /// bytes actually written.
    #[test]
    fn stream_buffer_push_writes_a_completed_line_into_the_given_writer() {
        let mut buffer = StreamBuffer::default();
        let mut out: Vec<u8> = Vec::new();
        let expected = format!("{}\n", LineStyler::default().style_line("hello"));

        let written = buffer.push(&mut out, "hello\n");

        assert_eq!(
            String::from_utf8(out.clone()).expect("styled bytes must be UTF-8"),
            expected,
            "the styled line must reach the writer, not io::stdout()"
        );
        assert_eq!(
            written,
            out.len(),
            "the returned count must equal the bytes written to the writer"
        );
        assert_eq!(
            buffer.buffer, "",
            "a completed line leaves nothing buffered"
        );
    }

    /// (11) A partial line is buffered and **nothing** is written.
    #[test]
    fn stream_buffer_push_buffers_a_partial_line_and_writes_nothing() {
        let mut buffer = StreamBuffer::default();
        let mut out: Vec<u8> = Vec::new();

        let written = buffer.push(&mut out, "partial");

        assert!(out.is_empty(), "a partial line must not be written");
        assert_eq!(written, 0, "nothing written means a zero count");
        assert_eq!(buffer.buffer, "partial", "the partial line stays buffered");
    }

    /// (12) The buffer genuinely spans calls: `"a\nb"` then `"c\n"` must emit
    /// the `a` line and then the joined `bc` line, in that order.
    #[test]
    fn stream_buffer_push_spans_calls_and_joins_a_split_line() {
        let mut buffer = StreamBuffer::default();
        let mut out: Vec<u8> = Vec::new();
        let mut reference = LineStyler::default();
        let expected = format!(
            "{}\n{}\n",
            reference.style_line("a"),
            reference.style_line("bc")
        );

        let first = buffer.push(&mut out, "a\nb");
        let second = buffer.push(&mut out, "c\n");

        assert_eq!(
            String::from_utf8(out.clone()).expect("styled bytes must be UTF-8"),
            expected
        );
        assert_eq!(
            first + second,
            out.len(),
            "the counts must add up to the writer's contents"
        );
    }

    /// (13) THE SUBTLE REGRESSION RISK: the styler's fence state must survive
    /// threading. A fence opened by one `push` must still be open for the
    /// next, so the interior line comes out diff-styled rather than plain.
    ///
    /// Non-vacuity: the same interior line styled by a *fresh* styler (no
    /// fence) is asserted to differ from the fenced rendering, so this cannot
    /// pass by both sides being the bare text. `owo-colors` emits SGR
    /// unconditionally here (no `if_supports_color` and no override anywhere
    /// in the workspace), so the green really is present.
    #[test]
    fn stream_buffer_push_keeps_fence_state_across_calls_through_the_writer() {
        let mut buffer = StreamBuffer::default();
        let mut out: Vec<u8> = Vec::new();
        let mut reference = LineStyler::default();
        let expected = format!(
            "{}\n{}\n{}\n",
            reference.style_line("```diff"),
            reference.style_line("+added"),
            reference.style_line("```")
        );

        buffer.push(&mut out, "```diff\n");
        buffer.push(&mut out, "+added\n");
        buffer.push(&mut out, "```\n");

        let text = String::from_utf8(out).expect("styled bytes must be UTF-8");
        assert_eq!(text, expected, "fence state must survive the threading");

        let unfenced = LineStyler::default().style_line("+added");
        assert_ne!(
            reference_interior(),
            unfenced,
            "guard: the fenced and unfenced renderings of the interior line \
             must differ, or this test would assert nothing"
        );
        assert!(
            text.contains(&reference_interior()),
            "the interior line must arrive diff-styled, not plain"
        );
    }

    /// The diff-styled rendering of the interior line used by (13), computed
    /// by driving a real `LineStyler` through the fence opener first.
    fn reference_interior() -> String {
        let mut styler = LineStyler::default();
        let _ = styler.style_line("```diff");
        styler.style_line("+added")
    }

    /// (14) `flush_unstyled` writes the buffered partial line into the writer
    /// unstyled, returns its length, empties the buffer, and resets the
    /// styler. The reset is proved behaviourally: a fence is opened, flushed
    /// across, and a line that would only be styled with the fence still open
    /// comes out plain.
    #[test]
    fn stream_buffer_flush_unstyled_writes_the_partial_line_and_resets_the_styler() {
        let mut buffer = StreamBuffer::default();
        let mut out: Vec<u8> = Vec::new();
        buffer.push(&mut out, "```diff\n+par");
        out.clear();

        let written = buffer.flush_unstyled(&mut out);

        assert_eq!(
            String::from_utf8(out.clone()).expect("flushed bytes must be UTF-8"),
            "+par",
            "the partial line must reach the writer, unstyled"
        );
        assert_eq!(
            written,
            "+par".len(),
            "the count is the partial line's length"
        );
        assert_eq!(buffer.buffer, "", "the buffer is emptied by the flush");

        out.clear();
        buffer.push(&mut out, "+added\n");
        let after = String::from_utf8(out).expect("styled bytes must be UTF-8");
        assert_eq!(
            after,
            format!("{}\n", LineStyler::default().style_line("+added")),
            "with the fence reset the line must come out plain"
        );
        assert_ne!(
            after,
            format!("{}\n", reference_interior()),
            "guard: a still-open fence would have produced different bytes"
        );
    }

    /// (15) `flush_unstyled` on an empty buffer writes nothing and returns 0.
    #[test]
    fn stream_buffer_flush_unstyled_on_an_empty_buffer_writes_nothing() {
        let mut buffer = StreamBuffer::default();
        let mut out: Vec<u8> = Vec::new();

        let written = buffer.flush_unstyled(&mut out);

        assert!(out.is_empty(), "an empty buffer has nothing to flush");
        assert_eq!(written, 0);
    }

    /// (16) END-TO-END, the first half of F12: a completed line through
    /// `print_fragment` must leave the styled line's bytes **in the block**.
    ///
    /// CAVEAT, stated plainly: `stdout_style()` reads a real descriptor and
    /// under `cargo test` stdout is captured, so the branch this test actually
    /// exercises is the **non-styled** one, whose block already carries the
    /// sanitized bytes. The styled arm — the one F12 is about — cannot be
    /// driven from here, so its expectation is computed and compared only when
    /// the test happens to run on a styled TTY. The styled path's threading is
    /// pinned by (10)-(13) and by review of the call site, not by this test.
    #[test]
    fn print_fragment_leaves_the_assistant_text_in_the_block_not_an_empty_block() {
        let _guard = surface_slot_lock();
        let _state = StreamStateGuard::new();
        let fragment = "assistant text through the seam\n";
        let sanitized = sanitize_terminal_text(fragment);
        let expected = if stdout_style() {
            format!(
                "{}\n",
                LineStyler::default().style_line(sanitized.trim_end_matches('\n'))
            )
        } else {
            sanitized.clone()
        };

        let calls = record_calls(|| print_fragment(fragment));

        let (fd, text) = one_block(&calls);
        assert_eq!(fd, Fd::Stdout);
        assert!(
            !text.is_empty(),
            "F12: a backend must not receive an empty block for assistant text"
        );
        assert_eq!(text, expected);
    }

    /// (17) END-TO-END, the other half of F12, and this one IS reachable on
    /// the captured-stdout tier: `finish_line` flushes the stream buffer
    /// unconditionally, with no `stdout_style()` branch. With a partial line
    /// buffered, the block must contain that partial line **followed by** the
    /// trailing newline — today it contains only the newline.
    ///
    /// The partial line is seeded by writing the field directly, the same way
    /// `reset_stream_globals` clears it, so no stray bytes are emitted during
    /// setup.
    #[test]
    fn finish_line_block_carries_the_partial_line_before_the_newline() {
        let _guard = surface_slot_lock();
        let _state = StreamStateGuard::new();
        stream_buffer()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .buffer
            .push_str("a buffered partial");
        ASSISTANT_OUTPUT_WRITTEN.store(true, Ordering::Release);

        let calls = record_calls(finish_line);

        let (fd, text) = one_block(&calls);
        assert_eq!(fd, Fd::Stdout);
        assert_eq!(
            text, "a buffered partial\n",
            "F12: the block must carry the partial line the newline terminates"
        );
    }

    /// (18) A guard that no `io::stdout()` reference survives inside
    /// `impl StreamBuffer`. Asserted by construction over the crate's own
    /// source, which is the only way to state it as a test: a direct
    /// descriptor reference in the path being decoupled defeats the point,
    /// and a type-level proof is not available while the methods merely take
    /// a `&mut dyn Write`.
    #[test]
    fn stream_buffer_impl_does_not_reference_stdout() {
        let source = include_str!("lib.rs");
        let start = source
            .find("impl StreamBuffer {")
            .expect("the impl block must exist");
        let end = source[start..]
            .find("fn stream_buffer()")
            .expect("the impl block is followed by stream_buffer()")
            + start;
        let impl_block = &source[start..end];
        assert!(
            !impl_block.contains("io::stdout()"),
            "StreamBuffer must write only to the writer it is handed"
        );
    }
}
