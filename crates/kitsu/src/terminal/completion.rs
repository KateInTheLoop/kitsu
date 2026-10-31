//! Shared composer completion, hints, and palette layout.

use kitsu_tools::Permissions;

use crate::attachments;
use crate::command_registry::matching_slash_commands;
use crate::command_registry::SlashCommandSpec;
use crate::presentation::terminal_inline;
use crate::subagent;

/// The **one** composer completion source, used by both hosts.
///
/// The classic Rustyline `ChatHelper` and the inline composer's
/// `kitsu_tui::composer::CompletionSource` implementation in
/// [`tui_bridge`](crate::tui_bridge) both call
/// [`candidates`](ComposerCompletion::candidates) and neither computes
/// anything of its own. This is the F2/F38/F45 lesson applied up front: two
/// implementations that happen to agree are worth much less than one that
/// cannot disagree.
///
/// The launch directory is held explicitly rather than read from
/// `std::env::current_dir` inside the call, so the candidate list is a pure
/// function of its inputs and both hosts can be compared in a unit test.
pub(crate) struct ComposerCompletion {
    pub(crate) permissions: std::sync::Arc<Permissions>,
    /// The directory relative `@file` mentions resolve against.
    pub(crate) base: std::path::PathBuf,
}

impl ComposerCompletion {
    pub(crate) fn new(permissions: std::sync::Arc<Permissions>, base: std::path::PathBuf) -> Self {
        Self { permissions, base }
    }

    /// The production construction: attachments resolve from the process
    /// launch directory. This is the only place that directory is read, which
    /// is why `attachments` exposes only the explicit-base
    /// [`complete_from`](attachments::complete_from).
    pub(crate) fn at_launch_directory(permissions: std::sync::Arc<Permissions>) -> Self {
        Self::new(
            permissions,
            std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
        )
    }

    /// Candidates for `line` with the cursor at byte offset `pos`.
    ///
    /// `start` is the byte offset the replacement begins at, exactly
    /// Rustyline's contract, so the inline composer splices `start..pos`.
    pub(crate) fn candidates(&self, line: &str, pos: usize) -> kitsu_tui::composer::Completion {
        use kitsu_tui::composer::{Candidate, Completion};

        let Some(typed) = line.get(..pos).and_then(|text| text.strip_prefix('/')) else {
            // Not a slash line: the cursor may sit in an `@path` mention.
            let Some((start, candidates)) =
                attachments::complete_from(line, pos, &self.base, self.permissions.as_ref())
            else {
                return Completion::none();
            };
            if candidates.is_empty() {
                // Nothing to offer carries no start, so two hosts with
                // nothing to offer compare equal.
                return Completion::none();
            }
            return Completion {
                start,
                candidates: candidates
                    .into_iter()
                    .map(|(display, replacement)| Candidate::new(display, replacement))
                    .collect(),
            };
        };
        if typed.chars().any(char::is_whitespace) {
            // The command is already complete; its arguments are not ours.
            return Completion::none();
        }
        Completion {
            // A slash candidate is the whole line.
            start: 0,
            candidates: matching_slash_commands(typed)
                .into_iter()
                .map(|spec| {
                    Candidate::new(
                        format!("{:<24} {}", spec.syntax(), spec.description),
                        format!(
                            "/{}{}",
                            spec.name,
                            if spec.args.is_empty() { "" } else { " " }
                        ),
                    )
                })
                .collect(),
        }
    }
}

/// The one styling pass for the composer's hint, shared by the classic
/// Rustyline `Highlighter` and the inline composer's re-hosted one.
///
/// Styling must preserve the display width Rustyline computed from the
/// unstyled hint, which is why there is one function and not two.
pub(crate) fn highlight_composer_hint(hint: &str, rich: bool) -> String {
    if !rich {
        return hint.to_string();
    }
    // Bright cyan separates the transient command deck; the persistent
    // model/directory footer is dimmer. Styling preserves the display width
    // Rustyline calculated from the unstyled hint.
    hint.rsplit_once('\n').map_or_else(
        || format!("\x1b[2;36m{hint}\x1b[0m"),
        |(deck, status)| format!("\x1b[1;96m{deck}\x1b[0m\n\x1b[2;36m{status}\x1b[0m"),
    )
}

/// Rustyline helper for slash-command completion. Multi-line paste is kept
/// verbatim: shortening it from a highlighter corrupts Rustyline's cursor-width
/// accounting (especially for Unicode), so visual collapsing belongs in a
/// future editor renderer that can preserve buffer geometry.
pub(crate) struct ChatHelper {
    /// The shared completion source. `impl Completer` must route through this
    /// and keep only the mapping to `rustyline::completion::Pair`.
    pub(crate) completion: ComposerCompletion,
    /// Rich terminals get Unicode borders plus color. `NO_COLOR` keeps the
    /// Rustyline editor but selects an unstyled ASCII palette; explicit
    /// plain/screen-reader modes bypass Rustyline entirely.
    pub(crate) rich_palette: bool,
    /// Shared with `/model`, resume, and subagent dispatch so the persistent
    /// composer footer reflects the model the next turn will actually use.
    pub(crate) live_model_window: subagent::LiveModelWindow,
    pub(crate) directory: String,
}

pub(crate) const SLASH_PALETTE_ROWS: usize = 8;
pub(crate) const COMPOSER_SURFACE_MAX_WIDTH: usize = 70;
pub(crate) const COMPOSER_SURFACE_DEFAULT_INNER_WIDTH: usize = COMPOSER_SURFACE_MAX_WIDTH - 2;
pub(crate) const SLASH_DESCRIPTION_MIN_INNER_WIDTH: usize = 40;

#[cfg(unix)]
pub(crate) fn current_terminal_columns() -> Option<usize> {
    for descriptor in [libc::STDERR_FILENO, libc::STDOUT_FILENO, libc::STDIN_FILENO] {
        let mut size = libc::winsize {
            ws_row: 0,
            ws_col: 0,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // SAFETY: `size` is initialized writable storage for TIOCGWINSZ and
        // the standard descriptors remain owned by the process.
        if unsafe { libc::ioctl(descriptor, libc::TIOCGWINSZ, &mut size) } == 0 && size.ws_col > 0 {
            return Some(usize::from(size.ws_col));
        }
    }
    None
}

#[cfg(not(unix))]
pub(crate) fn current_terminal_columns() -> Option<usize> {
    std::env::var("COLUMNS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|columns| *columns > 0)
}

pub(crate) fn composer_surface_inner_width(columns: Option<usize>) -> usize {
    columns.map_or(COMPOSER_SURFACE_DEFAULT_INNER_WIDTH, |columns| {
        columns.min(COMPOSER_SURFACE_MAX_WIDTH).saturating_sub(2)
    })
}

pub(crate) fn fit_palette_text(text: &str, width: usize) -> String {
    fit_palette_text_with_padding(text, width, ' ')
}

pub(crate) fn fit_palette_text_with_padding(text: &str, width: usize, padding: char) -> String {
    use unicode_width::UnicodeWidthChar as _;

    if width == 0 {
        return String::new();
    }

    let mut fitted = String::new();
    let mut used = 0_usize;
    let mut truncated = false;
    for character in text.chars() {
        let character_width = character.width().unwrap_or(0);
        if used.saturating_add(character_width) > width {
            truncated = true;
            break;
        }
        fitted.push(character);
        used += character_width;
    }
    if truncated {
        let ellipsis = '…';
        let ellipsis_width = ellipsis.width().unwrap_or(1);
        while used.saturating_add(ellipsis_width) > width {
            let Some(character) = fitted.pop() else {
                break;
            };
            used = used.saturating_sub(character.width().unwrap_or(0));
        }
        if used.saturating_add(ellipsis_width) <= width {
            fitted.push(ellipsis);
            used += ellipsis_width;
        }
    }
    let padding_width = padding.width().unwrap_or(1).max(1);
    while used.saturating_add(padding_width) <= width {
        fitted.push(padding);
        used += padding_width;
    }
    fitted
}

/// The bordered slash-command palette as **rows**, laid out to `max_rows`.
///
/// The single renderer for the palette's text: the labels, both frame
/// character sets, the `no matching command` line and the row-selection step
/// exist here and nowhere else. [`slash_palette`] is the classic Rustyline
/// hint's join over it, and `tui_bridge::palette_rows` is the inline
/// viewport's -- the same relationship `kitsu_render::live::compose_input_row`
/// has to `compose_row`.
///
/// Every row is a complete line and none is newline-terminated; the first is
/// always the top border and the last always the footer separator, so the
/// result is a well-formed box at any height. `max_rows` is the whole box's
/// height and must be at least `3` (border, one content row, footer); a
/// smaller value is treated as `3`, because a partial frame is worse than a
/// minimal one. Height is taken out of the *entries*, and the trailing
/// "N more" row counts everything actually hidden, whether the
/// [`SLASH_PALETTE_ROWS`] cap or the height is what hid it.
pub(crate) fn slash_palette_rows(
    prefix: &str,
    rich: bool,
    inner_width: usize,
    max_rows: u16,
) -> Vec<String> {
    // A box needs its lid, a content row and its footer; below that a caller
    // must decline the palette entirely rather than ask for a partial frame.
    let max_rows = max_rows.max(3);
    let top_label = " slash commands ";
    let footer_label = " type to filter \u{b7} Tab to complete ";
    let (top_left, horizontal, top_right, vertical, separator_left, separator_right) = if rich {
        (
            '\u{256d}', '\u{2500}', '\u{256e}', '\u{2502}', '\u{251c}', '\u{2524}',
        )
    } else {
        ('+', '-', '+', '|', '+', '+')
    };
    let framed = |text: &str| {
        format!(
            "{vertical}{}{vertical}",
            fit_palette_text(text, inner_width)
        )
    };

    let mut rows = vec![format!(
        "{top_left}{}{top_right}",
        fit_palette_text_with_padding(top_label, inner_width, horizontal)
    )];

    let matches = matching_slash_commands(prefix);
    // The content budget is everything but the top border and the footer
    // separator; there is no bottom border, because the composer status line
    // supplies the `\u{2570}\u{2026}\u{256f}` that closes the box.
    let budget = usize::from(max_rows - 2);
    if matches.is_empty() {
        rows.push(framed(
            " no matching command \u{2014} /help lists everything",
        ));
    } else {
        let entry = |spec: &SlashCommandSpec| {
            if inner_width >= SLASH_DESCRIPTION_MIN_INNER_WIDTH {
                // The *syntax* in the label slot whenever it fits that slot,
                // so a command's argument vocabulary is visible on the one
                // surface a user reads while typing the command. A long
                // argument list (`/agent`, `/jobs`) keeps the bare name
                // instead: it would fill the row by itself and crowd the
                // description out, which is the compact branch's trade-off,
                // not this one's.
                format!(" /{:<22} {}", spec.name, spec.description)
            } else {
                format!(" {}", spec.syntax())
            }
        };
        if matches.len() <= SLASH_PALETTE_ROWS.min(budget) {
            // Everything fits: no "more" row to carry, so the entries take
            // the whole budget.
            rows.extend(matches.iter().map(|spec| framed(&entry(spec))));
        } else {
            // One content row goes to the "more" line, so the entries get
            // `max_rows - 3`. At the minimum height that is zero entries and
            // a "more" row carrying the full match count: a well-formed,
            // honest minimal box.
            let shown = SLASH_PALETTE_ROWS.min(usize::from(max_rows - 3));
            rows.extend(matches.iter().take(shown).map(|spec| framed(&entry(spec))));
            rows.push(framed(&format!(
                " \u{2026} {} more \u{2014} keep typing to narrow the list",
                matches.len() - shown
            )));
        }
    }

    rows.push(format!(
        "{separator_left}{}{separator_right}",
        fit_palette_text_with_padding(footer_label, inner_width, horizontal)
    ));
    rows
}

/// Whether the bordered palette would tell the inline composer's row anything
/// its own hint slot does not already say.
///
/// The palette is a menu and the hint is the keystroke's answer, so the two
/// must not contradict each other on one screen. Two cases say no:
///
/// * the buffer is not a bare single-token slash prefix -- exactly
///   [`idle_slash_hint`]'s and `ChatHelper::hint`'s own rule;
/// * the prefix already **is** a command's full name (`/context`), so there is
///   no choice left to offer and the row is the whole answer.
///
/// The row's hint itself is no longer consulted: the idle prompt ranks by
/// registry order, so [`idle_slash_hint`] always completes the palette's
/// first entry and the two cannot disagree.
///
/// A prefix that matches nothing does get the box, because its single
/// `no matching command` row is the only thing that says so.
pub(crate) fn slash_palette_adds_to_the_row(buffer: &str, _hint: &str) -> bool {
    let Some(prefix) = buffer.strip_prefix('/') else {
        return false;
    };
    if buffer.contains('\n') || prefix.chars().any(char::is_whitespace) {
        return false;
    }
    let matches = matching_slash_commands(prefix);
    let Some(lead) = matches.first().map(|spec| spec.name) else {
        return true;
    };
    lead != prefix
}

/// A bounded, editor-owned palette rendered as a multi-line Rustyline hint.
/// Keeping it in the hint layout means every keystroke redraws/clears it with
/// the input buffer instead of leaving stale rows in terminal scrollback.
///
/// A thin join over [`slash_palette_rows`] at the classic height, which is
/// whatever the content needs: the Rustyline hint has no height bound of its
/// own, so it asks for every row the palette would ever produce.
pub(crate) fn slash_palette(prefix: &str, rich: bool, inner_width: usize) -> String {
    let mut palette = String::from("\n");
    palette.push_str(&slash_palette_rows(prefix, rich, inner_width, u16::MAX).join("\n"));
    palette
}

pub(crate) fn composer_status(
    model: &str,
    directory: &str,
    rich: bool,
    inner_width: usize,
) -> String {
    let model = terminal_inline(model);
    let directory = terminal_inline(directory);
    let separator = if rich { " │ " } else { " | " };
    let content = fit_palette_text(
        &format!(" model {model}{separator}cwd {directory}"),
        inner_width,
    );
    if rich {
        format!("\n╰{content}╯")
    } else {
        format!("\n+{content}+")
    }
}

pub(crate) struct SlashPaletteHint(pub(crate) String);

impl rustyline::hint::Hint for SlashPaletteHint {
    fn display(&self) -> &str {
        &self.0
    }

    // The palette is informational. Right-arrow must never paste its borders
    // and descriptions into the user's prompt; Tab owns completion.
    fn completion(&self) -> Option<&str> {
        None
    }
}

pub(crate) fn bracketed_paste_enabled(assistive_input: bool) -> bool {
    !assistive_input
}

impl rustyline::completion::Completer for ChatHelper {
    type Candidate = rustyline::completion::Pair;

    fn complete(
        &self,
        line: &str,
        pos: usize,
        _ctx: &rustyline::Context<'_>,
    ) -> rustyline::Result<(usize, Vec<Self::Candidate>)> {
        // Nothing is chosen here: every candidate comes from the one shared
        // source, and all that is left is the mapping to Rustyline's own
        // `Pair`. There is deliberately no second completion implementation.
        let offered = self.completion.candidates(line, pos);
        Ok((
            offered.start,
            offered
                .candidates
                .into_iter()
                .map(|candidate| rustyline::completion::Pair {
                    display: candidate.display,
                    replacement: candidate.replacement,
                })
                .collect(),
        ))
    }
}
impl rustyline::hint::Hinter for ChatHelper {
    type Hint = SlashPaletteHint;

    fn hint(&self, line: &str, _pos: usize, _ctx: &rustyline::Context<'_>) -> Option<Self::Hint> {
        let inner_width = composer_surface_inner_width(current_terminal_columns());
        let mut deck = match line.strip_prefix('/') {
            Some(typed) if !line.contains('\n') && !typed.chars().any(char::is_whitespace) => {
                slash_palette(typed, self.rich_palette, inner_width)
            }
            _ => String::new(),
        };
        let model = self
            .live_model_window
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .0
            .clone();
        deck.push_str(&composer_status(
            &model,
            &self.directory,
            self.rich_palette,
            inner_width,
        ));
        Some(SlashPaletteHint(deck))
    }
}
impl rustyline::validate::Validator for ChatHelper {}
impl rustyline::Helper for ChatHelper {}

impl rustyline::highlight::Highlighter for ChatHelper {
    fn highlight_hint<'h>(&self, hint: &'h str) -> std::borrow::Cow<'h, str> {
        if self.rich_palette {
            // One styling pass, shared with the inline composer's re-hosted
            // highlighter; there is deliberately no second one.
            std::borrow::Cow::Owned(highlight_composer_hint(hint, true))
        } else {
            std::borrow::Cow::Borrowed(hint)
        }
    }
}

/// Commands that act on the RUNNING turn, in preference order. While a turn
/// is live these outrank plain registry order in the inline hint: `/s`
/// should suggest `steer` (usable now), not `save` (queued for later).
pub(crate) const MID_TURN_HINT_PRIORITY: [&str; 5] = ["steer", "queue", "jobs", "quit", "exit"];

/// Inline completion hint for the live editor: the remainder of the best
/// registry slash command matching the typed `/prefix` — mid-turn-actionable
/// commands first, then registry order (empty when the buffer is not a bare
/// slash prefix or nothing matches).
pub(crate) fn typeahead_slash_hint(buffer: &str) -> String {
    slash_hint_ranked(buffer, |rest| {
        MID_TURN_HINT_PRIORITY
            .iter()
            .copied()
            .find(|name| name.starts_with(rest) && name.len() > rest.len())
            .or_else(|| {
                matching_slash_commands(rest)
                    .iter()
                    .map(|spec| spec.name)
                    .find(|name| name.len() > rest.len())
            })
    })
}

/// Inline completion hint for the IDLE prompt: the remainder of the *first*
/// registry match for the typed `/prefix`, in plain registry order.
///
/// Deliberately a second function rather than a parameter on
/// [`typeahead_slash_hint`]: the name is how a call site declares which
/// policy it wants, and there is no default to inherit silently --
/// inheriting the during-turn default silently at the idle prompt was
/// finding F30. [`MID_TURN_HINT_PRIORITY`] describes commands that act on a
/// RUNNING turn, and no turn runs here, so it has no claim.
///
/// Unlike the during-turn ranking this does not skip over an exact match:
/// registry order's lead for `/agent` is `agent` itself, so the hint is
/// empty even though `agents` would complete it. That keeps the invariant
/// `prefix + hint == matching_slash_commands(prefix).first().name`, which is
/// what lets the palette trust the row's hint.
pub(crate) fn idle_slash_hint(buffer: &str) -> String {
    slash_hint_ranked(buffer, |rest| {
        matching_slash_commands(rest).first().map(|spec| spec.name)
    })
}

/// The structural rules both slash hints share -- strip the slash, reject an
/// empty, whitespace-bearing or multi-line buffer -- with `ranking` choosing
/// which matching command the hint completes. A ranking that returns the
/// prefix itself yields an empty hint.
pub(crate) fn slash_hint_ranked(
    buffer: &str,
    ranking: impl Fn(&str) -> Option<&'static str>,
) -> String {
    let Some(rest) = buffer.strip_prefix('/') else {
        return String::new();
    };
    if rest.is_empty() || rest.contains(char::is_whitespace) {
        return String::new();
    }
    ranking(rest)
        .filter(|name| name.len() > rest.len())
        .map(|name| name[rest.len()..].to_string())
        .unwrap_or_default()
}
