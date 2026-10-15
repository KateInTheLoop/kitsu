//! The agent view's **pure model** (T4-S1): plain rows, grouping, ordering and
//! the filter grammar. No terminal, no ratatui widget, no registry — a `Vec` of
//! rows in, a `Vec` of groups out, so A2's ordering and A3's filter are
//! testable without a PTY.
//!
//! # The state taxonomy is not defined here
//!
//! [`kitsu_render::AgentViewState`] owns A2's six states and their order. It
//! lives in `kitsu-render` because the binary's `/jobs view` text deck needs
//! the same order and `kitsu-tui` cannot depend on `kitsu` (F5). This module
//! only *consumes* the order; it must never hard-code a second one.

use std::time::Duration;

use kitsu_render::AgentViewState;

/// One row of the agent view. Every field requirement **A2** names is present:
/// the state (which carries the label), the id, the name or task head, the
/// model and profile, the last activity with its age, and the tokens.
///
/// `last_activity`, `last_activity_age` and `tokens` are `Option` because the
/// agent-thread DTO cannot supply them — they live on `AgentTelemetry`, not on
/// `AgentThreadSnapshot` — so a row built from the DTO alone leaves them
/// absent rather than inventing a zero.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentRow {
    pub state: AgentViewState,
    pub id: u64,
    /// The display name, when the agent has been named.
    pub name: Option<String>,
    /// The spawning task, used for the head when there is no name.
    pub task: String,
    pub model: String,
    pub profile: Option<String>,
    pub last_activity: Option<String>,
    pub last_activity_age: Option<Duration>,
    pub tokens: Option<u64>,
}

impl AgentRow {
    /// The character budget for a task head, ellipsis included. The value is
    /// the implementer's to confirm against the overlay's real column width;
    /// the test only pins that it is wide enough to be a head.
    pub const TASK_HEAD_CHARS: usize = 48;

    /// A2's "name or task head": the name when the agent has one, otherwise
    /// the first line of the task, bounded to [`AgentRow::TASK_HEAD_CHARS`].
    pub fn head(&self) -> String {
        if let Some(name) = &self.name {
            return name.to_string();
        }
        let first_line = self.task.lines().next().unwrap_or_default();
        if first_line.chars().count() <= Self::TASK_HEAD_CHARS {
            return first_line.to_string();
        }
        // The ellipsis is part of the budget, so a truncated head is exactly
        // as wide as an untruncated one at the limit.
        let mut head = first_line
            .chars()
            .take(Self::TASK_HEAD_CHARS - 1)
            .collect::<String>();
        head.push('\u{2026}');
        head
    }
}

/// One A2 group: a state and the rows in it, in the order they arrived.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentGroup {
    pub state: AgentViewState,
    pub rows: Vec<AgentRow>,
}

/// Group rows by state into A2's fixed order. Empty groups are **omitted**,
/// matching the `/jobs view` text deck, so the view never prints a heading
/// with nothing under it. Row order inside a group is the input order.
pub fn group_rows(rows: Vec<AgentRow>) -> Vec<AgentGroup> {
    // One bucket per state, in A2's order as `AgentViewState::order` gives it:
    // the order is never restated here.
    let order = AgentViewState::order();
    let mut buckets: Vec<AgentGroup> = order
        .into_iter()
        .map(|state| AgentGroup {
            state,
            rows: Vec::new(),
        })
        .collect();
    for row in rows {
        let rank = row.state.rank();
        buckets[rank].rows.push(row);
    }
    // Empty groups are omitted, as `/jobs view` omits them: the view never
    // prints a heading with nothing under it.
    buckets.retain(|group| !group.rows.is_empty());
    buckets
}

/// The ids of `rows` in the order the view **displays** them: A2's groups in
/// A2's order, rows in input order within a group. The selection walks this
/// and never the input order, so moving down goes to the row below the one on
/// screen.
fn display_order(rows: &[AgentRow]) -> Vec<u64> {
    group_rows(rows.to_vec())
        .into_iter()
        .flat_map(|group| group.rows.into_iter().map(|row| row.id))
        .collect()
}

/// What a press of the two-press stop key answered.
///
/// `Confirmed` carries the id it is confirming, so the caller fires at the
/// agent the second press was typed on rather than re-reading the selection at
/// fire time — between the two there may have been a repaint.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StopPress {
    /// The first press: nothing has been stopped, and the key is now armed.
    Armed,
    /// The second press on the same agent: stop this one.
    Confirmed(u64),
    /// There is no selected agent, so there is nothing to arm. Its own answer
    /// rather than a silent `Armed` that would fire on the next row to appear.
    NoSelection,
}

/// The agent view's selection: **which agent** the keys act on.
///
/// # Why an id and not a row index
///
/// Rows re-group under the cursor while the view is open: an agent that
/// finishes moves from `working` to `done (unviewed)`, which is a *later*
/// group in A2's order, so a row index would quietly come to mean a different
/// agent. The user's next keystroke may be destructive, so the selection is
/// the agent, and only a repaint that cannot find that agent any more moves
/// the selection at all.
///
/// # Clamping, not wrapping
///
/// `↑` on the first row stays on the first row. Wrapping would carry the
/// cursor from `needs input` — the group that needs you — to `cancelled` in
/// one keystroke, possibly while the stop key is armed on the row left behind.
///
/// # Custody
///
/// Nothing here reads a result or touches a handoff: the type holds an id, a
/// display index and an arming, and A7's acknowledgement stays the peek's
/// alone.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AgentSelection {
    /// The selected agent, absent only while the view has no rows.
    selected: Option<u64>,
    /// The selected row's display index at the last sync or move. Consulted
    /// only when the selected agent has vanished: the selection then falls to
    /// whatever row holds this index, clamped to the new last row, because
    /// that is where the user's eye already is.
    index: usize,
    /// The agent the stop key is armed on, if any. Keyed by agent for the same
    /// reason the selection is: an arming must never migrate to another row.
    armed: Option<u64>,
}

impl AgentSelection {
    pub fn new() -> Self {
        Self::default()
    }

    /// The selected agent, or `None` when the view has no rows.
    pub fn selected(&self) -> Option<u64> {
        self.selected
    }

    /// Reconcile the selection with a freshly built row list — every repaint.
    ///
    /// A repaint is not a keystroke: it keeps the selection on its agent and
    /// keeps the stop key's arming. It disarms only when the armed agent is no
    /// longer the selected one, which is exactly when firing would hit someone
    /// else.
    pub fn sync(&mut self, rows: &[AgentRow]) {
        let order = display_order(rows);
        if order.is_empty() {
            self.selected = None;
            // Back to the first row when rows come back, rather than staying
            // absent for the rest of the session.
            self.index = 0;
        } else if let Some(position) = self
            .selected
            .and_then(|id| order.iter().position(|row| *row == id))
        {
            self.index = position;
        } else {
            self.index = self.index.min(order.len() - 1);
            self.selected = Some(order[self.index]);
        }
        if self.armed != self.selected {
            self.armed = None;
        }
    }

    /// `↓`: the next displayed row, clamping on the last one.
    pub fn move_down(&mut self, rows: &[AgentRow]) {
        self.move_by(rows, 1);
    }

    /// `↑`: the previous displayed row, clamping on the first one.
    pub fn move_up(&mut self, rows: &[AgentRow]) {
        self.move_by(rows, -1);
    }

    fn move_by(&mut self, rows: &[AgentRow], delta: isize) {
        // Moving is a keystroke, so it disarms whatever the stop key was armed
        // on — an `x` armed on one agent must not fire on another because a
        // `↓` happened in between.
        self.armed = None;
        let order = display_order(rows);
        if order.is_empty() {
            // Neither key invents a selection for an empty view.
            self.selected = None;
            self.index = 0;
            return;
        }
        let current = self
            .selected
            .and_then(|id| order.iter().position(|row| *row == id))
            .unwrap_or_else(|| self.index.min(order.len() - 1));
        let next = if delta < 0 {
            current.saturating_sub(1)
        } else {
            (current + 1).min(order.len() - 1)
        };
        self.index = next;
        self.selected = Some(order[next]);
    }

    /// Whether the stop key is armed, so the surface can say so.
    pub fn stop_armed(&self) -> bool {
        self.armed.is_some()
    }

    /// Any other keystroke: the arming goes away, as it does for T4-S2's
    /// two-press `←`.
    pub fn disarm(&mut self) {
        self.armed = None;
    }

    /// A press of the stop key: the first arms, the second on the same agent
    /// confirms, and confirming disarms so a third press arms again rather
    /// than firing again.
    pub fn press_stop(&mut self) -> StopPress {
        let Some(id) = self.selected else {
            return StopPress::NoSelection;
        };
        if self.armed == Some(id) {
            self.armed = None;
            return StopPress::Confirmed(id);
        }
        self.armed = Some(id);
        StopPress::Armed
    }
}

/// A parsed A3 filter: `s:<state> n:<text> o:<text> p:<profile>`. Absent keys
/// do not constrain; present keys are combined with AND.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AgentFilter {
    pub state: Option<AgentViewState>,
    pub name: Option<String>,
    pub objective: Option<String>,
    pub profile: Option<String>,
}

impl AgentFilter {
    pub fn matches(&self, row: &AgentRow) -> bool {
        /// Case-insensitive substring, over a field that may be absent.
        /// Absent is not the empty string: a row with no name cannot be
        /// matched by `n:`.
        fn contains(haystack: Option<&str>, needle: &str) -> bool {
            haystack.is_some_and(|value| {
                value
                    .to_lowercase()
                    .contains(needle.to_lowercase().as_str())
            })
        }
        if let Some(state) = self.state {
            if row.state != state {
                return false;
            }
        }
        if let Some(name) = &self.name {
            // `n:` reads the name and only the name.
            if !contains(row.name.as_deref(), name) {
                return false;
            }
        }
        if let Some(objective) = &self.objective {
            // `o:` reads the task and only the task.
            if !contains(Some(row.task.as_str()), objective) {
                return false;
            }
        }
        if let Some(profile) = &self.profile {
            if !contains(row.profile.as_deref(), profile) {
                return false;
            }
        }
        true
    }
}

/// Why a filter string was rejected. A malformed filter is an **error**, never
/// a silently dropped term: in this view a dropped term shows the wrong agents,
/// which means failing to show the one that needs you.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FilterError {
    message: String,
}

impl FilterError {
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl std::fmt::Display for FilterError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

/// Parse an A3 filter string. Whitespace-separated `key:value` terms; an empty
/// or whitespace-only string is the empty filter, which matches every row.
pub fn parse_filter(input: &str) -> Result<AgentFilter, FilterError> {
    /// The accepted keys, named in every rejection so the user never has to
    /// guess the grammar from a bare "invalid filter".
    const KEYS: &str = "use s:<state>, n:<text>, o:<text> or p:<profile>";

    fn reject(message: String) -> FilterError {
        FilterError { message }
    }

    let mut filter = AgentFilter::default();
    let mut seen: Vec<&str> = Vec::new();
    for term in input.split_whitespace() {
        let Some((key, value)) = term.split_once(':') else {
            return Err(reject(format!(
                "filter term `{term}` has no key \u{2014} {KEYS}"
            )));
        };
        if !matches!(key, "s" | "n" | "o" | "p") {
            return Err(reject(format!(
                "unknown filter key `{key}` \u{2014} {KEYS}"
            )));
        }
        if value.is_empty() {
            return Err(reject(format!("filter key `{key}` needs a value")));
        }
        if seen.contains(&key) {
            return Err(reject(format!("filter key `{key}` appears twice")));
        }
        seen.push(key);
        match key {
            "s" => {
                // The vocabulary is `AgentViewState`'s own, listed back in the
                // rejection so it cannot drift from what is accepted.
                let states = accepted_state_words();
                let state = AgentViewState::from_wire(&value.to_lowercase()).ok_or_else(|| {
                    reject(format!("unknown state `{value}` \u{2014} use {states}"))
                })?;
                filter.state = Some(state);
            }
            "n" => filter.name = Some(value.to_string()),
            "o" => filter.objective = Some(value.to_string()),
            "p" => filter.profile = Some(value.to_string()),
            _ => unreachable!("the key was checked against the accepted set above"),
        }
    }
    Ok(filter)
}

/// The `s:` vocabulary as prose (`a, b, c or d`), read off
/// [`AgentViewState::wire`] so the words a rejection offers are exactly the
/// words the parser accepts.
fn accepted_state_words() -> String {
    let words = AgentViewState::order().map(AgentViewState::wire);
    let (last, head) = words.split_last().expect("order lists six states");
    format!("{} or {last}", head.join(", "))
}

/// The one token every frame of the overlay prints, on its first row.
///
/// The design calls the surface "the agent view overlay" throughout (§3.2 A1,
/// §4.4), and these two words are what the PTY measurement reads to decide
/// whether the view is on screen at all, so they live here rather than being
/// spelled per frame.
pub const OVERLAY_TITLE: &str = "agent view";

/// The hint the title row carries: the two keys this card actually answers.
///
/// Deliberately ASCII. The overlay's only non-ASCII characters are the state
/// markers, and those come from [`kitsu_render::deck_marker`], which already
/// degrades to a stable ASCII tier; a separator glyph invented here would have
/// no such tier.
const OVERLAY_KEYS_HINT: &str = "? keys   esc close";

/// What the overlay says when nothing has been spawned yet.
const OVERLAY_EMPTY: &str = "no agents yet";

/// One group heading: the state's own A2 label and the member count.
///
/// Headings are unglyphed and two-space indented, exactly as `/jobs view`'s
/// text deck prints them, so the two surfaces read the same.
fn group_heading(group: &AgentGroup) -> String {
    format!("  {} ({})", group.state.label(), group.rows.len())
}

/// One agent's row: the state marker, the id, A2's head, the model and the
/// profile, then the activity and the tokens when they are known.
///
/// The marker comes from [`kitsu_render::deck_marker`] applied to
/// [`AgentViewState::deck_state`] and from nowhere else. That is the whole
/// reason this function exists rather than a `format!` at the call site: a
/// second marker table here is the F2/F38/F45/F57 drift family, and it is also
/// what keeps a *blocked* agent from rendering identically to a busy one.
fn agent_line(row: &AgentRow, selected: bool) -> String {
    // The selected row is marked in the row's own indent, and the mark is the
    // `*` `kitsu_render`'s profile list already uses for "the active row of a
    // list" — no new glyph enters the project's vocabulary, and in particular
    // no second *state* marker table appears next to `deck_marker`'s. It
    // replaces an indent space rather than being inserted, so a marked row is
    // exactly as wide as the row it marks and cannot wrap into the row below.
    let indent = if selected { "  * " } else { "    " };
    let mut line = format!(
        "{indent}{} #{} {}",
        kitsu_render::deck_marker(row.state.deck_state()),
        row.id,
        row.head()
    );
    line.push_str(&format!("  {}", row.model));
    if let Some(profile) = &row.profile {
        line.push_str(&format!("  [{profile}]"));
    }
    if let Some(activity) = &row.last_activity {
        line.push_str(&format!("  {activity}"));
        if let Some(age) = row.last_activity_age {
            line.push_str(&format!(" ({}s ago)", age.as_secs()));
        }
    }
    if let Some(tokens) = row.tokens {
        line.push_str(&format!("  {tokens} tok"));
    }
    line
}

/// The overlay's list frame: the title row, then A2's groups in A2's order.
///
/// Every row is plain text, already fitted to `cols` by
/// [`kitsu_render::fit_row_to_width`], and the caller positions them
/// absolutely — this function knows nothing about the terminal.
pub fn overlay_rows(rows: Vec<AgentRow>, cols: usize) -> Vec<String> {
    overlay_rows_selected(rows, cols, None)
}

/// The overlay's list frame with the selected agent's row marked.
///
/// `selected` is an **agent id**, never a row index, for the reason
/// [`AgentSelection`] gives: rows re-group under the cursor, so an index would
/// retarget the mark — and the user's next key — onto a different agent. An id
/// with no row on screen (filtered away, or pruned between the selection and
/// the paint) marks nothing rather than marking some other row.
///
/// With `None` this is byte-identical to [`overlay_rows`], which is the one
/// renderer: there is no second frame layout here, only the row indent.
///
/// Painting a frame — marked or not — reads rows and nothing else. It takes no
/// custody of any result (A7: the peek is the only acknowledging call site).
pub fn overlay_rows_selected(
    rows: Vec<AgentRow>,
    cols: usize,
    selected: Option<u64>,
) -> Vec<String> {
    let count = rows.len();
    let mut out = vec![
        title_row(
            &format!(
                "{OVERLAY_TITLE} \u{2014} {count} agent{}",
                if count == 1 { "" } else { "s" }
            ),
            cols,
        ),
        String::new(),
    ];
    if count == 0 {
        out.push(format!("  {OVERLAY_EMPTY}"));
        return fit_all(out, cols);
    }
    for group in group_rows(rows) {
        out.push(group_heading(&group));
        for row in &group.rows {
            out.push(agent_line(row, selected == Some(row.id)));
        }
        out.push(String::new());
    }
    fit_all(out, cols)
}

/// The overlay's help frame: the keys this card answers, and nothing it does
/// not. The title row still carries [`OVERLAY_TITLE`], because the help is a
/// frame *of* the view rather than a different surface.
pub fn overlay_help_rows(cols: usize) -> Vec<String> {
    let mut out = vec![
        title_row(&format!("{OVERLAY_TITLE} \u{2014} keys"), cols),
        String::new(),
    ];
    for (key, what) in [
        (
            "up/dn",
            "move the selection; moving disarms a half-pressed stop",
        ),
        // `space` is listed because T4-S3 wired it and a key that works while
        // being unadvertised is half-shipped (F38's lesson, applied to keys).
        // This literal list is itself the thing T4-S10 replaces with the one
        // bindings table; adding a correct entry to it is not new duplication,
        // whereas leaving a working key out is a defect a user meets.
        (
            "space",
            "peek the selected agent; counts as collecting its result",
        ),
        // The action keys A3 binds (card T4-S4b). Each line says what the key
        // does to the *selected* agent, because that is the only thing any of
        // them acts on, and `x` says it is two presses: the first changes
        // nothing in the registry, so a user who does not know that reads it
        // as a key that failed.
        (
            "r",
            "reply to the selected agent: the next key is y, a or n",
        ),
        (
            "s",
            "steer the selected agent: type a correction, enter sends",
        ),
        (
            "x",
            "stop the selected agent; press twice (the second press prunes a \
             finished one)",
        ),
        ("n", "rename the selected agent: type a name, enter applies"),
        ("p", "acknowledge the selected agent's result"),
        ("?", "show this help"),
        ("esc", "leave a prompt or the help, then close the view"),
    ] {
        out.push(format!("  {key:<6} {what}"));
    }
    fit_all(out, cols)
}

/// The caret the steer line draws, because the overlay hides the terminal's
/// own cursor (there is no caret anywhere else in this view). Exported so a
/// test can find it without hard-coding the implementer's glyph.
pub const STEER_CARET: char = '\u{2588}';

/// The frame for `s`'s text sub-mode (card T4-S4b, requirement A3's
/// *`s` steer (prompt for text)*).
///
/// The overlay has no caret and hides the cursor, so the echo and the caret
/// are **drawn**: `draft` is shown verbatim and [`STEER_CARET`] is placed at
/// `cursor`, a **char** offset into `draft` (the editor's own unit). The frame
/// also advertises how to submit and how to cancel, because a sub-mode a user
/// cannot get out of is worse than no sub-mode.
///
/// `id` is on the frame: the agent being steered is the one the keystroke was
/// typed on, and steering has a real effect, so the surface says which.
pub fn steer_prompt_rows(id: u64, draft: &str, cursor: usize, cols: usize) -> Vec<String> {
    let _ = (id, draft, cursor, cols);
    unimplemented!("card T4-S4b: the steer line's frame")
}

/// The row list with one **report line** under it: what the last action key
/// did (card T4-S4b). `None` is the frame exactly as T4-S4a painted it.
///
/// One report is always **one row**: a report built from registry text (a
/// steering receipt, a name, a refusal) must not be able to grow the frame or
/// push rows off it.
pub fn overlay_rows_with_report(
    rows: Vec<AgentRow>,
    cols: usize,
    selected: Option<u64>,
    report: Option<&str>,
) -> Vec<String> {
    let mut out = overlay_rows_selected(rows, cols, selected);
    if let Some(report) = report {
        // Every control character becomes a space *before* the fit: a newline
        // in registry text (a name a human typed, a receipt) would otherwise
        // paint a second row the frame never budgeted for, pushing the rows
        // above it off the bottom. One report is one row, by construction.
        let flattened = report
            .chars()
            .map(|character| {
                if character.is_control() {
                    ' '
                } else {
                    character
                }
            })
            .collect::<String>();
        out.push(kitsu_render::fit_row_to_width(
            &format!("  {flattened}"),
            cols,
        ));
    }
    out
}

/// The title row: the frame's own heading on the left and the key hint right
/// aligned, or the heading alone on a terminal too narrow for both.
fn title_row(heading: &str, cols: usize) -> String {
    let needed = heading.chars().count() + OVERLAY_KEYS_HINT.chars().count() + 2;
    if cols < needed {
        return heading.to_string();
    }
    let gap = cols - heading.chars().count() - OVERLAY_KEYS_HINT.chars().count();
    format!("{heading}{}{OVERLAY_KEYS_HINT}", " ".repeat(gap))
}

/// Fit every row to the terminal's width, so no overlay row can wrap into the
/// row below it.
fn fit_all(rows: Vec<String>, cols: usize) -> Vec<String> {
    rows.into_iter()
        .map(|row| kitsu_render::fit_row_to_width(&row, cols))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{group_rows, parse_filter, AgentFilter, AgentRow};
    use kitsu_render::AgentViewState;
    use std::time::Duration;

    /// A fully populated row: every A2 field present, so a test that reads a
    /// field cannot pass by reading an absent one.
    fn full_row(id: u64, state: AgentViewState) -> AgentRow {
        AgentRow {
            state,
            id,
            name: Some("reviewer".to_string()),
            task: "summarize the crate".to_string(),
            model: "qwen2.5-coder".to_string(),
            profile: Some("review".to_string()),
            last_activity: Some("reading lib.rs".to_string()),
            last_activity_age: Some(Duration::from_millis(12_500)),
            tokens: Some(4_096),
        }
    }

    fn row(id: u64, state: AgentViewState) -> AgentRow {
        AgentRow {
            name: None,
            ..full_row(id, state)
        }
    }

    /// A2's six states in A2's own order, written out here so the expectation
    /// lives in the test and not in the code under test. The implementation's
    /// `AgentViewState::order()` must agree with this.
    const A2_ORDER: [AgentViewState; 6] = [
        AgentViewState::NeedsInput,
        AgentViewState::Working,
        AgentViewState::DoneUnviewed,
        AgentViewState::DoneViewed,
        AgentViewState::Failed,
        AgentViewState::Cancelled,
    ];

    /// The ordering itself: A2's six groups come back in A2's order whatever
    /// order the rows arrive in, and `rank`/`order` agree with each other so a
    /// consumer cannot read a different order out of the same type.
    #[test]
    fn the_agent_view_groups_rows_by_state_in_a2s_order() {
        // Deliberately the reverse of A2's order, so a pass cannot come from
        // the implementation echoing its input.
        let shuffled = vec![
            row(1, AgentViewState::Cancelled),
            row(2, AgentViewState::Failed),
            row(3, AgentViewState::DoneViewed),
            row(4, AgentViewState::DoneUnviewed),
            row(5, AgentViewState::Working),
            row(6, AgentViewState::NeedsInput),
        ];
        let groups = group_rows(shuffled);
        assert_eq!(
            groups
                .iter()
                .map(|group| group.state)
                .collect::<Vec<AgentViewState>>(),
            A2_ORDER.to_vec(),
            "groups must come back in A2's order"
        );
        assert_eq!(
            groups.iter().map(|group| group.rows.len()).sum::<usize>(),
            6,
            "every row must land in exactly one group"
        );
        assert_eq!(AgentViewState::order().to_vec(), A2_ORDER.to_vec());
        for (index, state) in A2_ORDER.into_iter().enumerate() {
            assert_eq!(
                state.rank(),
                index,
                "rank disagrees with order for {state:?}"
            );
        }
    }

    /// The table the row asks for: a set of rows in, the groups out. Includes
    /// the empty-group case (omitted, matching `/jobs view`) and the
    /// several-rows-per-group case.
    #[test]
    fn the_agent_view_grouping_table_omits_empty_groups_and_keeps_input_order() {
        struct Case {
            name: &'static str,
            rows: Vec<AgentRow>,
            expected: Vec<(AgentViewState, Vec<u64>)>,
        }

        let cases = vec![
            Case {
                name: "no rows at all yields no groups",
                rows: Vec::new(),
                expected: Vec::new(),
            },
            Case {
                name: "one state present yields exactly one group",
                rows: vec![row(9, AgentViewState::Working)],
                expected: vec![(AgentViewState::Working, vec![9])],
            },
            Case {
                name: "empty groups between two populated ones are omitted",
                rows: vec![
                    row(3, AgentViewState::Cancelled),
                    row(1, AgentViewState::NeedsInput),
                ],
                expected: vec![
                    (AgentViewState::NeedsInput, vec![1]),
                    (AgentViewState::Cancelled, vec![3]),
                ],
            },
            Case {
                name: "rows keep their input order inside a group",
                rows: vec![
                    row(30, AgentViewState::Working),
                    row(10, AgentViewState::Working),
                    row(20, AgentViewState::Working),
                ],
                expected: vec![(AgentViewState::Working, vec![30, 10, 20])],
            },
            Case {
                name: "done splits by whether the result has been viewed",
                rows: vec![
                    row(2, AgentViewState::DoneViewed),
                    row(1, AgentViewState::DoneUnviewed),
                    row(4, AgentViewState::DoneViewed),
                ],
                expected: vec![
                    (AgentViewState::DoneUnviewed, vec![1]),
                    (AgentViewState::DoneViewed, vec![2, 4]),
                ],
            },
            Case {
                name: "failed and cancelled are separate groups in that order",
                rows: vec![
                    row(5, AgentViewState::Cancelled),
                    row(6, AgentViewState::Failed),
                ],
                expected: vec![
                    (AgentViewState::Failed, vec![6]),
                    (AgentViewState::Cancelled, vec![5]),
                ],
            },
        ];

        for case in cases {
            let groups = group_rows(case.rows);
            let actual = groups
                .into_iter()
                .map(|group| {
                    (
                        group.state,
                        group.rows.iter().map(|row| row.id).collect::<Vec<u64>>(),
                    )
                })
                .collect::<Vec<(AgentViewState, Vec<u64>)>>();
            assert_eq!(actual, case.expected, "case: {}", case.name);
        }
    }

    /// A2's group labels, in A2's own words, and distinct from one another so
    /// two groups can never print the same heading.
    #[test]
    fn each_a2_group_has_its_own_label_and_wire_spelling() {
        let labels = A2_ORDER
            .into_iter()
            .map(AgentViewState::label)
            .collect::<Vec<&str>>();
        assert_eq!(
            labels,
            vec![
                "needs input",
                "working",
                "done (unviewed)",
                "done (viewed)",
                "failed",
                "cancelled",
            ]
        );
        let wires = A2_ORDER
            .into_iter()
            .map(AgentViewState::wire)
            .collect::<Vec<&str>>();
        assert_eq!(
            wires,
            vec![
                "needs-input",
                "working",
                "done-unviewed",
                "done-viewed",
                "failed",
                "cancelled",
            ]
        );
        for state in A2_ORDER {
            assert!(
                state.label().is_ascii() && state.wire().is_ascii(),
                "group vocabulary stays ASCII in every output tier: {state:?}"
            );
            assert_eq!(
                AgentViewState::from_wire(state.wire()),
                Some(state),
                "wire spelling must round-trip for {state:?}"
            );
        }
    }

    /// Every field A2 lists is readable off a row. A row that silently dropped
    /// `tokens` or the activity age would group correctly and still be wrong.
    #[test]
    fn an_agent_row_carries_every_field_a2_names() {
        let row = full_row(7, AgentViewState::NeedsInput);
        assert_eq!(row.state.label(), "needs input");
        assert_eq!(row.id, 7);
        assert_eq!(row.head(), "reviewer");
        assert_eq!(row.model, "qwen2.5-coder");
        assert_eq!(row.profile.as_deref(), Some("review"));
        assert_eq!(row.last_activity.as_deref(), Some("reading lib.rs"));
        assert_eq!(row.last_activity_age, Some(Duration::from_millis(12_500)));
        assert_eq!(row.tokens, Some(4_096));
    }

    /// A2's "name or task head". The budget is the implementation's to choose,
    /// so the bound is asserted against the public constant rather than a
    /// number invented here — but it must be wide enough to be a head.
    #[test]
    fn the_row_head_is_the_name_or_a_bounded_single_line_task_head() {
        // A head narrower than 16 characters is not a head. Const-asserted so
        // narrowing the budget is a compile error, not a silent regression.
        const _: () = assert!(AgentRow::TASK_HEAD_CHARS >= 16);

        let named = full_row(1, AgentViewState::Working);
        assert_eq!(named.head(), "reviewer", "a named agent shows its name");

        let unnamed = row(2, AgentViewState::Working);
        assert_eq!(
            unnamed.head(),
            "summarize the crate",
            "an unnamed agent shows its task, unchanged when it already fits"
        );

        let multiline = AgentRow {
            task: "first line of the task\nsecond line\nthird".to_string(),
            ..row(3, AgentViewState::Working)
        };
        let head = multiline.head();
        assert_eq!(
            head, "first line of the task",
            "the head is the first line only"
        );
        assert!(!head.contains('\n'), "a head never carries a newline");

        let long = AgentRow {
            task: "q".repeat(AgentRow::TASK_HEAD_CHARS * 3),
            ..row(4, AgentViewState::Working)
        };
        let head = long.head();
        assert_eq!(
            head.chars().count(),
            AgentRow::TASK_HEAD_CHARS,
            "an over-long task head is bounded to the budget"
        );
        assert!(
            head.ends_with('\u{2026}'),
            "a truncated head says so with an ellipsis, got {head:?}"
        );
        assert!(
            head.starts_with("qqqq"),
            "a truncated head keeps the start of the task, got {head:?}"
        );
    }

    /// A3's grammar: all four keys, each one constraining only its own field,
    /// and several keys combining with AND.
    #[test]
    fn the_filter_grammar_accepts_all_four_keys_and_combines_them_with_and() {
        let target = AgentRow {
            name: Some("Reviewer".to_string()),
            task: "summarize the CRATE".to_string(),
            profile: Some("review".to_string()),
            ..full_row(1, AgentViewState::NeedsInput)
        };
        let other = AgentRow {
            name: Some("builder".to_string()),
            task: "run the build".to_string(),
            profile: Some("default".to_string()),
            ..full_row(2, AgentViewState::Working)
        };

        // Each key on its own, matching case-insensitively.
        let single: [(&str, bool, bool); 4] = [
            ("s:needs-input", true, false),
            ("n:review", true, false),
            ("o:crate", true, false),
            ("p:REVIEW", true, false),
        ];
        for (input, hits_target, hits_other) in single {
            let filter = parse_filter(input).expect("well-formed filter");
            assert_eq!(filter.matches(&target), hits_target, "input: {input}");
            assert_eq!(filter.matches(&other), hits_other, "input: {input}");
        }

        // The parsed shape, so a term cannot leak into the wrong field.
        assert_eq!(
            parse_filter("s:working n:build o:build p:default").expect("well-formed filter"),
            AgentFilter {
                state: Some(AgentViewState::Working),
                name: Some("build".to_string()),
                objective: Some("build".to_string()),
                profile: Some("default".to_string()),
            }
        );

        // AND: every present term must hold.
        let both = parse_filter("s:needs-input p:review").expect("well-formed filter");
        assert!(both.matches(&target));
        assert!(!both.matches(&other));
        let contradictory = parse_filter("s:needs-input p:default").expect("well-formed filter");
        assert!(
            !contradictory.matches(&target) && !contradictory.matches(&other),
            "terms combine with AND, not OR"
        );

        // `n:` reads the name and nothing else; `o:` reads the task.
        let name_only = parse_filter("n:summarize").expect("well-formed filter");
        assert!(
            !name_only.matches(&target),
            "n: must not fall through to the task"
        );
        let objective_only = parse_filter("o:reviewer").expect("well-formed filter");
        assert!(
            !objective_only.matches(&target),
            "o: must not fall through to the name"
        );

        // An unnamed row cannot be matched by `n:`, and an unprofiled row
        // cannot be matched by `p:` — absent is not empty-string.
        let bare = AgentRow {
            name: None,
            profile: None,
            ..full_row(3, AgentViewState::Working)
        };
        assert!(!parse_filter("n:reviewer").unwrap().matches(&bare));
        assert!(!parse_filter("p:review").unwrap().matches(&bare));

        // The empty filter constrains nothing.
        for input in ["", "   ", "\t"] {
            let filter = parse_filter(input).expect("the empty filter is well-formed");
            assert_eq!(filter, AgentFilter::default(), "input: {input:?}");
            assert!(filter.matches(&target) && filter.matches(&other) && filter.matches(&bare));
        }
    }

    /// A malformed filter is **rejected**, with a message that names the bad
    /// token and the accepted vocabulary. Dropping the term instead would show
    /// the wrong agents, and in this view that means hiding the one that is
    /// waiting on an answer.
    #[test]
    fn a_malformed_filter_is_an_error_naming_the_bad_token_and_the_accepted_keys() {
        const KEYS: &str = "use s:<state>, n:<text>, o:<text> or p:<profile>";
        const STATES: &str =
            "use needs-input, working, done-unviewed, done-viewed, failed or cancelled";

        let cases: [(&str, String); 7] = [
            ("q:foo", format!("unknown filter key `q` \u{2014} {KEYS}")),
            (
                "s:working q:foo",
                format!("unknown filter key `q` \u{2014} {KEYS}"),
            ),
            (
                "reviewer",
                format!("filter term `reviewer` has no key \u{2014} {KEYS}"),
            ),
            ("n:", "filter key `n` needs a value".to_string()),
            ("s:", "filter key `s` needs a value".to_string()),
            ("s:done", format!("unknown state `done` \u{2014} {STATES}")),
            ("n:alpha n:beta", "filter key `n` appears twice".to_string()),
        ];

        for (input, expected) in cases {
            let error = parse_filter(input)
                .map(|filter| format!("{filter:?}"))
                .expect_err(&format!("`{input}` must be rejected, not ignored"));
            assert_eq!(error.message(), expected, "input: {input}");
            assert_eq!(error.to_string(), expected, "input: {input}");
        }

        // Non-vacuous: the shapes above are the *only* rejected ones here, and
        // every A2 state word is accepted by `s:`.
        for state in A2_ORDER {
            let filter = parse_filter(&format!("s:{}", state.wire()))
                .unwrap_or_else(|error| panic!("s:{} rejected: {error}", state.wire()));
            assert_eq!(filter.state, Some(state));
        }
    }

    // --- card T4-S4: selection, and the keystroke that arms a destructive key ---
    //
    // A3 names `↑/↓ move` and five keys that act on "the selected agent", and
    // selection state does not exist yet: T4-S3's `Space` peeks the *first*
    // row because there is nothing to consult. These tests pin the selection
    // model, which is pure — rows in, a selected id out — so the whole of it
    // is answerable here and the PTY is left to answer only key routing (the
    // split T4-S3 made, for the reason it gave: a fixture session has zero
    // agents).
    //
    // # The two decisions A3 does not make, made here
    //
    // **Clamping, not wrapping.** `↑` on the first row stays on the first row.
    // Wrapping would carry the cursor from `needs input` — the group that
    // needs you — to `cancelled`, the group that needs nobody, in one
    // keystroke, and it would do so while `x` may be armed on the row left
    // behind. T4-S2's two-press `←` already set this project's taste for
    // keystrokes with no surprising long-range effect.
    //
    // **Selection is an agent, not a row index.** Rows re-group under the
    // selection constantly: an agent finishing moves from `working` to `done
    // (unviewed)`, which in A2's order is a *later* group, so an index-based
    // selection would quietly retarget the user's next `x` or `p` onto a
    // different agent. Keying on the id makes that impossible. When the
    // selected agent is gone entirely (pruned) the selection falls to whatever
    // row now holds its old display index, clamped to the last row, because
    // that is the row the user's eye is already on.

    /// The display order the selection walks: `group_rows` flattened. Written
    /// out in the test rather than read off the model, so a model that walked
    /// its *input* order instead of A2's grouped order fails here.
    fn display_ids(rows: &[AgentRow]) -> Vec<u64> {
        group_rows(rows.to_vec())
            .into_iter()
            .flat_map(|group| group.rows.into_iter().map(|row| row.id))
            .collect()
    }

    /// `↑/↓` move the selection through the view's **displayed** order, and
    /// clamp at both ends rather than wrapping.
    ///
    /// The clamp is asserted from both ends and with a press to spare, so a
    /// model that wrapped, or that ran off the end into `None`, fails. The
    /// non-vacuity is the middle of the test: the same calls really do move
    /// the selection when there is somewhere to move to.
    #[test]
    fn the_selection_moves_with_the_arrow_keys_and_clamps_at_both_ends() {
        // Deliberately not in A2's order, so walking the input order is wrong.
        let rows = vec![
            row(11, AgentViewState::Cancelled),
            row(22, AgentViewState::NeedsInput),
            row(33, AgentViewState::Working),
            row(44, AgentViewState::NeedsInput),
        ];
        let order = display_ids(&rows);
        assert_eq!(
            order,
            vec![22, 44, 33, 11],
            "non-vacuous: the displayed order really differs from the input \
             order, so the walk below is a claim about grouping"
        );

        let mut selection = super::AgentSelection::new();
        selection.sync(&rows);
        assert_eq!(
            selection.selected(),
            Some(order[0]),
            "a fresh selection sits on the first displayed row, which is what \
             T4-S3's `Space` already peeks"
        );

        // Down through every row.
        for expected in &order[1..] {
            selection.move_down(&rows);
            assert_eq!(selection.selected(), Some(*expected));
        }
        // Two presses past the end: it stays, it does not wrap and it does not
        // become absent.
        for _ in 0..2 {
            selection.move_down(&rows);
            assert_eq!(
                selection.selected(),
                Some(*order.last().expect("four rows")),
                "`\u{2193}` on the last row clamps: it must not wrap to the \
                 first and must not clear the selection"
            );
        }
        // Back up through every row.
        for expected in order.iter().rev().skip(1) {
            selection.move_up(&rows);
            assert_eq!(selection.selected(), Some(*expected));
        }
        for _ in 0..2 {
            selection.move_up(&rows);
            assert_eq!(
                selection.selected(),
                Some(order[0]),
                "`\u{2191}` on the first row clamps"
            );
        }

        // One row: both keys are no-ops that keep the selection, rather than
        // either key clearing it.
        let single = vec![row(7, AgentViewState::Working)];
        let mut selection = super::AgentSelection::new();
        selection.sync(&single);
        for _ in 0..2 {
            selection.move_down(&single);
            selection.move_up(&single);
        }
        assert_eq!(selection.selected(), Some(7));

        // No rows: nothing is selected, and neither key invents a selection.
        let mut selection = super::AgentSelection::new();
        selection.sync(&[]);
        assert_eq!(selection.selected(), None, "an empty view selects nothing");
        selection.move_down(&[]);
        selection.move_up(&[]);
        assert_eq!(selection.selected(), None);
    }

    /// What the selection means when the list changes under it: it is the
    /// **agent**, not the row.
    ///
    /// Three cases, each a thing that really happens while the view is open:
    /// the selected agent finishes and changes group; another agent finishes
    /// and shifts the selected one's display index; the selected agent is
    /// pruned away. The third is the only one in which the selection moves to
    /// a different agent, and it moves to a named one rather than to `None`.
    #[test]
    fn the_selection_follows_its_agent_when_the_list_regroups_and_falls_down_when_it_vanishes() {
        let working = vec![
            row(1, AgentViewState::NeedsInput),
            row(2, AgentViewState::Working),
            row(3, AgentViewState::Working),
        ];
        assert_eq!(
            display_ids(&working),
            vec![1, 2, 3],
            "non-vacuous: the starting display order is this"
        );

        // Case 1: the selected agent changes group. `#2` is selected at
        // display index 1; it finishes, which in A2's order moves it *after*
        // `#3`. Index-keyed selection would now be pointing at `#3`.
        let mut selection = super::AgentSelection::new();
        selection.sync(&working);
        selection.move_down(&working);
        assert_eq!(selection.selected(), Some(2));
        let regrouped = vec![
            row(1, AgentViewState::NeedsInput),
            row(2, AgentViewState::DoneUnviewed),
            row(3, AgentViewState::Working),
        ];
        assert_eq!(
            display_ids(&regrouped),
            vec![1, 3, 2],
            "non-vacuous: finishing really did move #2 past #3 in the display \
             order, so an index-keyed selection would land on #3"
        );
        selection.sync(&regrouped);
        assert_eq!(
            selection.selected(),
            Some(2),
            "the selection is the agent it was put on, wherever its row went: \
             a `x` or `p` typed now must reach #2 and not #3"
        );

        // Case 2: a *different* agent moves, shifting the selected one's
        // display index without the selection changing agent.
        let mut selection = super::AgentSelection::new();
        selection.sync(&working);
        selection.move_down(&working);
        selection.move_down(&working);
        assert_eq!(selection.selected(), Some(3));
        let other_finished = vec![
            row(1, AgentViewState::NeedsInput),
            row(2, AgentViewState::DoneUnviewed),
            row(3, AgentViewState::Working),
        ];
        selection.sync(&other_finished);
        assert_eq!(
            selection.selected(),
            Some(3),
            "#3 is still the selected agent although its display index changed \
             from 2 to 1"
        );

        // Case 3: the selected agent is gone. The selection falls to whatever
        // row now holds its old display index — the next one down — rather
        // than to nothing and rather than silently to the top.
        let mut selection = super::AgentSelection::new();
        selection.sync(&working);
        selection.move_down(&working);
        assert_eq!(selection.selected(), Some(2));
        let pruned = vec![
            row(1, AgentViewState::NeedsInput),
            row(3, AgentViewState::Working),
        ];
        assert_eq!(display_ids(&pruned), vec![1, 3]);
        selection.sync(&pruned);
        assert_eq!(
            selection.selected(),
            Some(3),
            "a pruned selection falls to the row that took its display index"
        );

        // Case 3b: the *last* row is pruned, so there is no row at its old
        // index. The selection clamps to the new last row.
        let mut selection = super::AgentSelection::new();
        selection.sync(&working);
        selection.move_down(&working);
        selection.move_down(&working);
        assert_eq!(selection.selected(), Some(3));
        let tail_pruned = vec![
            row(1, AgentViewState::NeedsInput),
            row(2, AgentViewState::Working),
        ];
        selection.sync(&tail_pruned);
        assert_eq!(
            selection.selected(),
            Some(2),
            "pruning the last row clamps the selection to the new last row"
        );

        // Case 3c: everything is gone.
        selection.sync(&[]);
        assert_eq!(selection.selected(), None);
        // And it comes back on the first row when rows come back, rather than
        // staying absent for the rest of the session.
        selection.sync(&working);
        assert_eq!(selection.selected(), Some(1));
    }

    /// A selection the user cannot see is not a selection. Exactly one
    /// rendered row is marked, the marked row still carries everything the
    /// unmarked one did, and the frame for *no* selection is byte-identical to
    /// the frame [`super::overlay_rows`] already produces — so the two existing
    /// callers of that function are unaffected and no second renderer appears.
    #[test]
    fn exactly_one_rendered_row_is_marked_and_an_unselected_frame_is_the_old_frame() {
        let rows = vec![
            row(1, AgentViewState::NeedsInput),
            row(2, AgentViewState::Working),
            row(3, AgentViewState::Working),
        ];
        const COLS: usize = 100;

        let plain = super::overlay_rows(rows.clone(), COLS);
        assert_eq!(
            super::overlay_rows_selected(rows.clone(), COLS, None),
            plain,
            "with nothing selected the frame is exactly the frame \
             `overlay_rows` already prints, so T4-S2's and T4-S3's \
             measurements of it are untouched"
        );

        for selected in [1u64, 2, 3] {
            let marked = super::overlay_rows_selected(rows.clone(), COLS, Some(selected));
            assert_eq!(
                marked.len(),
                plain.len(),
                "marking a row adds no rows and drops none"
            );
            let differing = plain
                .iter()
                .zip(marked.iter())
                .enumerate()
                .filter(|(_, (before, after))| before != after)
                .map(|(index, _)| index)
                .collect::<Vec<usize>>();
            assert_eq!(
                differing.len(),
                1,
                "exactly one row of the frame may change when agent \
                 #{selected} is selected; frame:\n{}",
                marked.join("\n")
            );
            let changed = &marked[differing[0]];
            assert!(
                plain[differing[0]].contains(&format!("#{selected}")),
                "the row that changed must be agent #{selected}'s own row, \
                 but it was {:?}",
                plain[differing[0]]
            );
            // The mark is decoration, not replacement: the row still says
            // everything it said, in the same spelling.
            for token in [format!("#{selected}"), "summarize the crate".to_string()] {
                assert!(
                    changed.contains(&token),
                    "the selected row still carries {token:?}: {changed:?}"
                );
            }
            assert_eq!(
                changed.chars().count(),
                plain[differing[0]].chars().count(),
                "the selected row is still fitted to the terminal's width, so \
                 it cannot wrap into the row below it"
            );
        }

        // An id that is not on screen marks nothing — a filtered-away or
        // just-pruned selection must not paint a mark on some other row.
        assert_eq!(
            super::overlay_rows_selected(rows, COLS, Some(9_999)),
            plain,
            "an id with no row on screen marks no row at all"
        );
    }

    /// `x` is two presses per row, and **any other keystroke disarms** — the
    /// answer T4-S2 gave for the two-press `←`, followed here rather than a
    /// second answer being invented.
    ///
    /// Moving the selection disarms too, which is the case T4-S2 did not have:
    /// an `x` armed on one agent must not fire on another because the user
    /// pressed `↓` in between. That is the one way this affordance could stop
    /// an agent nobody asked to stop.
    #[test]
    fn two_stop_presses_on_one_row_confirm_and_anything_else_disarms() {
        let rows = vec![
            row(1, AgentViewState::NeedsInput),
            row(2, AgentViewState::Working),
        ];

        let mut selection = super::AgentSelection::new();
        selection.sync(&rows);
        assert!(
            !selection.stop_armed(),
            "nothing is armed when the view opens"
        );
        assert_eq!(
            selection.press_stop(),
            super::StopPress::Armed,
            "the first `x` arms and stops nothing"
        );
        assert!(
            selection.stop_armed(),
            "and it says so, so the user can tell"
        );
        assert_eq!(
            selection.press_stop(),
            super::StopPress::Confirmed(1),
            "the second `x` on the same row confirms, naming the agent it acts \
             on rather than `the selection` at the time of firing"
        );
        assert!(
            !selection.stop_armed(),
            "confirming disarms, so a third `x` arms again rather than firing \
             again"
        );
        assert_eq!(selection.press_stop(), super::StopPress::Armed);

        // Any other keystroke disarms.
        let mut selection = super::AgentSelection::new();
        selection.sync(&rows);
        assert_eq!(selection.press_stop(), super::StopPress::Armed);
        selection.disarm();
        assert!(!selection.stop_armed());
        assert_eq!(
            selection.press_stop(),
            super::StopPress::Armed,
            "after a disarm the next `x` arms: it does not fire"
        );

        // Moving the selection disarms, and the row moved to is not armed.
        let mut selection = super::AgentSelection::new();
        selection.sync(&rows);
        assert_eq!(selection.press_stop(), super::StopPress::Armed);
        selection.move_down(&rows);
        assert_eq!(selection.selected(), Some(2));
        assert!(
            !selection.stop_armed(),
            "`\u{2193}` disarms: an `x` armed on #1 must not fire on #2"
        );
        assert_eq!(selection.press_stop(), super::StopPress::Armed);
        assert_eq!(
            selection.press_stop(),
            super::StopPress::Confirmed(2),
            "and the two presses on the new row confirm it, so the disarm \
             above is a disarm and not a jam"
        );

        // A re-sync that keeps the selected agent keeps the arming: a repaint
        // is not a keystroke, and an agent finishing under the cursor must not
        // silently cancel a stop the user has half-typed.
        let mut selection = super::AgentSelection::new();
        selection.sync(&rows);
        assert_eq!(selection.press_stop(), super::StopPress::Armed);
        selection.sync(&[
            row(1, AgentViewState::NeedsInput),
            row(2, AgentViewState::DoneUnviewed),
        ]);
        assert_eq!(
            selection.press_stop(),
            super::StopPress::Confirmed(1),
            "a repaint that kept the selected agent kept its arming"
        );

        // But a re-sync that *loses* the selected agent disarms, because the
        // selection now points at a different agent.
        let mut selection = super::AgentSelection::new();
        selection.sync(&rows);
        assert_eq!(selection.press_stop(), super::StopPress::Armed);
        selection.sync(&[row(2, AgentViewState::Working)]);
        assert_eq!(selection.selected(), Some(2));
        assert!(
            !selection.stop_armed(),
            "the armed agent is gone, so the arming is gone with it"
        );

        // With nothing selected there is nothing to arm, and that is its own
        // answer rather than a silent `Armed`.
        let mut selection = super::AgentSelection::new();
        selection.sync(&[]);
        assert_eq!(selection.press_stop(), super::StopPress::NoSelection);
        assert!(!selection.stop_armed());
    }

    // --- card T4-S4b: the action keys' two new pure frames ---

    /// Card T4-S4b. **The steer line echoes what was typed and draws a
    /// caret**, because the overlay hides the terminal's cursor and has none
    /// of its own.
    ///
    /// Four things are pinned: the draft appears verbatim, the caret is
    /// exactly one and sits at the cursor's char offset, the agent being
    /// steered is named, and the way out is advertised.
    #[test]
    fn the_steer_line_echoes_the_draft_and_draws_one_caret_at_the_cursor() {
        let frame = super::steer_prompt_rows(7, "slow down", 9, 80);
        let joined = frame.join("\n");
        assert!(
            joined.contains("slow down"),
            "the draft is echoed verbatim: {joined:?}"
        );
        assert!(
            joined.contains('7'),
            "the frame names the agent being steered, because steering has a \
             real effect on it: {joined:?}"
        );
        assert_eq!(
            joined.matches(super::STEER_CARET).count(),
            1,
            "exactly one caret: {joined:?}"
        );
        for way_out in ["esc", "enter"] {
            assert!(
                joined.to_lowercase().contains(way_out),
                "a sub-mode must advertise how to submit and how to cancel, \
                 and {way_out:?} is missing: {joined:?}"
            );
        }

        // The caret sits where the cursor is, not merely somewhere. Measured
        // as the echo row's text on either side of it, so this is a claim
        // about position and not about the glyph being present.
        for (cursor, before, after) in [
            (0usize, "", "slow down"),
            (4, "slow", " down"),
            (9, "slow down", ""),
        ] {
            let frame = super::steer_prompt_rows(7, "slow down", cursor, 80);
            let echo = frame
                .iter()
                .find(|row| row.contains(super::STEER_CARET))
                .unwrap_or_else(|| panic!("no row carries the caret: {frame:?}"));
            let at = echo
                .find(super::STEER_CARET)
                .expect("the row was found by its caret");
            assert!(
                echo[..at].ends_with(before),
                "with the cursor at {cursor} the caret must follow {before:?}; \
                 echo row: {echo:?}"
            );
            assert!(
                echo[at + super::STEER_CARET.len_utf8()..].starts_with(after),
                "with the cursor at {cursor} the caret must precede {after:?}; \
                 echo row: {echo:?}"
            );
        }
    }

    /// Card T4-S4b: the steer line fits its terminal like every other overlay
    /// frame. A row that wrapped would push the frame down and leave the row
    /// underneath it painted twice.
    #[test]
    fn the_steer_line_fits_a_narrow_terminal() {
        let long = "x".repeat(200);
        let frame = super::steer_prompt_rows(7, &long, long.chars().count(), 40);
        assert!(!frame.is_empty(), "NON-VACUOUS: a frame really was built");
        for row in &frame {
            assert_eq!(
                kitsu_render::fit_row_to_width(row, 40),
                *row,
                "every row is already fitted to 40 columns; this one is not: \
                 {row:?}"
            );
        }
    }

    /// Card T4-S4b. **One report is one row**, and the row list under it is
    /// unchanged.
    ///
    /// The report is built from registry text — a steering receipt, a name a
    /// human typed, a refusal — so a newline in it must not be able to grow
    /// the frame and push rows off the bottom.
    #[test]
    fn a_report_line_adds_exactly_one_row_and_leaves_the_rows_alone() {
        let rows = vec![
            full_row(1, AgentViewState::Working),
            full_row(2, AgentViewState::NeedsInput),
        ];
        let plain = super::overlay_rows_with_report(rows.clone(), 80, Some(1), None);
        assert_eq!(
            plain,
            super::overlay_rows_selected(rows.clone(), 80, Some(1)),
            "with no report the frame is exactly T4-S4a's"
        );

        let reported =
            super::overlay_rows_with_report(rows.clone(), 80, Some(1), Some("stopped agent #1"));
        assert_eq!(
            reported.len(),
            plain.len() + 1,
            "a report is one row: {reported:?}"
        );
        assert_eq!(
            reported[..plain.len()],
            plain[..],
            "and it is added under the rows rather than replacing any of them"
        );
        assert!(
            reported[plain.len()].contains("stopped agent #1"),
            "the report says what happened: {:?}",
            reported[plain.len()]
        );

        let multiline = super::overlay_rows_with_report(
            rows.clone(),
            80,
            Some(1),
            Some("renamed to\nsomething\nelse"),
        );
        assert_eq!(
            multiline.len(),
            plain.len() + 1,
            "a report carrying newlines is still one row, or it would push \
             rows off the frame: {multiline:?}"
        );
        for row in &multiline {
            assert!(
                !row.contains('\n'),
                "and no row carries a newline into the paint: {row:?}"
            );
        }
    }

    /// Card T4-S4b, applying F38's lesson to keys as T4-S3 did for `space`: a
    /// key that works while being unadvertised is half-shipped. Every key
    /// A3 binds is in the help.
    #[test]
    fn the_help_advertises_every_key_the_view_binds() {
        let help = super::overlay_help_rows(100).join("\n").to_lowercase();
        for (key, what) in [
            ("r", "reply"),
            ("s", "steer"),
            ("x", "stop"),
            ("n", "rename"),
            ("p", "acknowledge"),
        ] {
            assert!(
                help.contains(what),
                "the help must say what `{key}` does, and {what:?} is not in \
                 it:\n{help}"
            );
        }
        // NON-VACUOUS: the keys T4-S3 and T4-S4a advertised are still there,
        // so a help screen that had simply stopped listing anything would not
        // pass this test by listing nothing.
        for already in ["peek", "move"] {
            assert!(
                help.contains(already),
                "the keys already advertised are still advertised: {already:?}\n{help}"
            );
        }
        // `x` is two presses and the help must say so, because the first
        // press does nothing visible to the registry and a user who does not
        // know that reads it as a key that failed.
        assert!(
            help.contains("twice") || help.contains("two"),
            "the help says `x` is two presses:\n{help}"
        );
    }
}
