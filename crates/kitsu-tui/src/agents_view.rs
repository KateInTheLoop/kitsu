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
fn agent_line(row: &AgentRow) -> String {
    let mut line = format!(
        "    {} #{} {}",
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
            out.push(agent_line(row));
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
        // `space` is listed because T4-S3 wired it and a key that works while
        // being unadvertised is half-shipped (F38's lesson, applied to keys).
        // This literal list is itself the thing T4-S10 replaces with the one
        // bindings table; adding a correct entry to it is not new duplication,
        // whereas leaving a working key out is a defect a user meets.
        (
            "space",
            "peek the selected agent; counts as collecting its result",
        ),
        ("?", "show this help"),
        ("esc", "leave the help, then close the view"),
    ] {
        out.push(format!("  {key:<6} {what}"));
    }
    fit_all(out, cols)
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
}
