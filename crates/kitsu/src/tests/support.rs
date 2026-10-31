//! Shared private unit-test fixtures.

use super::*;

// ---- R6-a: the transcript overlay's byte loop ----
//
// These drive `run_transcript_key_loop` itself, which is the point
// (**F69**): `kitsu_tui::transcript`'s key-space sweep proves the
// *predicate* `closes(Esc)` is true and proves nothing about whether any
// byte ever becomes a `Press::Esc`. It did not, and the footer advertised
// `Esc` anyway (**F75**).

/// A reader that hands over one scripted chunk per `read`, as a terminal
/// in `VMIN=1` raw mode does: a keystroke is a read and an escape
/// sequence can be split across two of them (**F65**).
///
/// The remaining-chunk count is shared so the loop's grace closure can be
/// the honest question "is there another chunk already waiting?", which is
/// what `stdin_waiting` asks `poll` in production.
#[cfg(unix)]
pub(super) struct ChunkReader {
    pub(super) chunks: std::collections::VecDeque<Vec<u8>>,
    pub(super) remaining: std::rc::Rc<std::cell::Cell<usize>>,
}

#[cfg(unix)]
impl ChunkReader {
    pub(super) fn new(chunks: &[&[u8]]) -> Self {
        let chunks: std::collections::VecDeque<Vec<u8>> =
            chunks.iter().map(|chunk| chunk.to_vec()).collect();
        let remaining = std::rc::Rc::new(std::cell::Cell::new(chunks.len()));
        Self { chunks, remaining }
    }
}

#[cfg(unix)]
impl std::io::Read for ChunkReader {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        match self.chunks.pop_front() {
            Some(chunk) => {
                self.remaining.set(self.chunks.len());
                out[..chunk.len()].copy_from_slice(&chunk);
                Ok(chunk.len())
            }
            None => Ok(0),
        }
    }
}

/// Run the real key loop over `chunks` on an 8-line ring and a 6-row
/// window, returning how it ended, the overlay's follow state and every
/// painted frame.
#[cfg(unix)]
pub(super) fn drive_transcript(chunks: &[&[u8]]) -> (TranscriptExit, bool, Vec<Vec<String>>) {
    let mut ring = kitsu_tui::cells::CellRing::new(16);
    for n in 0..16 {
        ring.push(kitsu_render::surface::Fd::Stdout, &format!("line {n}"));
    }
    let mut overlay = kitsu_tui::transcript::TranscriptOverlay::new();
    let mut input = ChunkReader::new(chunks);
    let remaining = std::rc::Rc::clone(&input.remaining);
    let mut frames: Vec<Vec<String>> = Vec::new();
    let exit = {
        let mut geometry = || (6u16, 40usize);
        // The grace, answered honestly rather than always-false: a chunk
        // already queued is exactly what `poll` reports as waiting, so a
        // sequence split across two reads is resolved as a sequence and a
        // lone `ESC` with nothing behind it is resolved as the press.
        let mut waiting = || remaining.get() > 0;
        let mut paint = |frame: &[String]| frames.push(frame.to_vec());
        run_transcript_key_loop(
            &mut input,
            &mut overlay,
            &ring,
            &mut geometry,
            &mut waiting,
            &mut paint,
        )
    };
    (exit, overlay.is_following(), frames)
}

/// Run the real key loop over `chunks` on a ring that holds **prompt
/// blocks**, for R6-c's `{`/`}`, and return the same triple
/// `drive_transcript` does.
///
/// A separate helper rather than a change to that one: its four existing
/// tests are about the close and paging bytes and their fixture has no
/// prompts in it, which is exactly the "nothing to jump between" case
/// those tests should keep exercising.
///
/// The fixture and its arithmetic (**F61**), since the assertions below
/// are stated in ordinals: prompts at 0, and at 3-4 as one two-row block;
/// eight retained lines, nothing evicted; the geometry is 6 rows, so the
/// overlay's `content_rows` is 5, `follow_top` is `8 - 5 = 3`, the opening
/// content is ordinals 3..=7 and the jump cursor is 3.
#[cfg(unix)]
pub(super) fn drive_transcript_with_prompts(
    chunks: &[&[u8]],
) -> (TranscriptExit, bool, Vec<Vec<String>>) {
    use kitsu_render::surface::Fd;

    let mut ring = kitsu_tui::cells::CellRing::new(16);
    ring.push_prompt("first question");
    ring.push(Fd::Stdout, "answer one");
    ring.push(Fd::Stdout, "answer two");
    ring.push_prompt("second question\nwith a tail");
    ring.push(Fd::Stdout, "answer three");
    ring.push(Fd::Stdout, "answer four");
    ring.push(Fd::Stdout, "answer five");
    assert_eq!((ring.len(), ring.evicted()), (8, 0));
    assert_eq!(
        ring.prompt_blocks(),
        vec![0, 3],
        "the fixture really holds two prompt blocks"
    );

    let mut overlay = kitsu_tui::transcript::TranscriptOverlay::new();
    let mut input = ChunkReader::new(chunks);
    let remaining = std::rc::Rc::clone(&input.remaining);
    let mut frames: Vec<Vec<String>> = Vec::new();
    let exit = {
        let mut geometry = || (6u16, 40usize);
        let mut waiting = || remaining.get() > 0;
        let mut paint = |frame: &[String]| frames.push(frame.to_vec());
        run_transcript_key_loop(
            &mut input,
            &mut overlay,
            &ring,
            &mut geometry,
            &mut waiting,
            &mut paint,
        )
    };
    (exit, overlay.is_following(), frames)
}

// ---- R6-b: `/` search and the `n`/`N` match walk, in the byte loop ----
//
// Driven through `run_transcript_key_loop` and not through a predicate
// beside it (**F69**/**F105**): `kitsu_tui::transcript`'s own tests prove
// the overlay's search policy answers a submitted query, and prove nothing
// about whether any byte ever reaches it. `/`, `n` and `N` all already
// fall inside this loop's `0x20..=0x7e` printable arm, so their behaviour
// today was **measured before anything was asserted** (**F99**/**F100**):
// each becomes a `Press::Char`, `closes` declines it, the loop reads
// again, and the painted frames are **byte-identical** either side of the
// byte -- inert keys, exactly as `{`/`}` were on the previous card.

/// Run the real key loop over `chunks` on a ring that holds **matchable
/// text on both speakers**, for R6-b's `/`, and return the same triple
/// `drive_transcript` does.
///
/// A third helper rather than a change to either existing one, for the
/// reason the second was added: those fixtures are about the close bytes,
/// the paging bytes and the prompt jumps, and their rings have no match in
/// them -- which is the "nothing to search for" case they should keep
/// exercising.
///
/// The fixture and its arithmetic (**F61**), since the assertions below
/// are stated in ordinals:
///
/// | ordinal | row | |
/// |---|---|---|
/// | 0 | `find me first` | **prompt**, and a **match** |
/// | 1 | `answer one` | |
/// | 2 | `find me second` | a **match**, on the session's side |
/// | 3..=8 | `answer two` .. `answer seven` | |
///
/// Nine retained lines, nothing evicted; the geometry is 6 rows, so
/// `content_rows` is 5, `follow_top` is `9 - 5 = 4`, the opening content
/// is ordinals `4..=8` and the cursor is 4.
#[cfg(unix)]
pub(super) fn drive_transcript_with_matches(
    chunks: &[&[u8]],
) -> (TranscriptExit, bool, Vec<Vec<String>>) {
    use kitsu_render::surface::Fd;

    let mut ring = kitsu_tui::cells::CellRing::new(16);
    ring.push_prompt("find me first");
    ring.push(Fd::Stdout, "answer one");
    ring.push(Fd::Stdout, "find me second");
    for name in ["two", "three", "four", "five", "six", "seven"] {
        ring.push(Fd::Stdout, &format!("answer {name}"));
    }
    assert_eq!((ring.len(), ring.evicted()), (9, 0));
    let matches: Vec<usize> = ring
        .lines()
        .iter()
        .enumerate()
        .filter(|(_, line)| line.plain().contains("find me"))
        .map(|(index, _)| index)
        .collect();
    assert_eq!(
        matches,
        vec![0, 2],
        "the fixture really holds two matches, one of them the user's own \
             prompt"
    );

    let mut overlay = kitsu_tui::transcript::TranscriptOverlay::new();
    let mut input = ChunkReader::new(chunks);
    let remaining = std::rc::Rc::clone(&input.remaining);
    let mut frames: Vec<Vec<String>> = Vec::new();
    let exit = {
        let mut geometry = || (6u16, 40usize);
        let mut waiting = || remaining.get() > 0;
        let mut paint = |frame: &[String]| frames.push(frame.to_vec());
        run_transcript_key_loop(
            &mut input,
            &mut overlay,
            &ring,
            &mut geometry,
            &mut waiting,
            &mut paint,
        )
    };
    (exit, overlay.is_following(), frames)
}

/// The opening content rows of `drive_transcript_with_matches`'s fixture,
/// ordinals 4..=8, stated once so every assertion below that means "the
/// frame did not move" is stated against the same five rows.
#[cfg(unix)]
pub(super) const OPENING: [&str; 5] = [
    "answer three",
    "answer four",
    "answer five",
    "answer six",
    "answer seven",
];

pub(super) fn telemetry_route_receipt_fixture() -> ProviderRouteReceipt {
    serde_json::from_value(serde_json::json!({
        "receipt_revision": 1,
        "route": {
            "contract_revision": 1,
            "adapter": "openai_compatible",
            "profile": "profile-v1-0123456789abcdef0123456789abcdef",
            "generation": 3,
            "wire_revision": "openai_chat_completions_v1",
            "capabilities": { "revision": 1, "supported": ["usage"] }
        },
        "model": "fixture-model",
        "auth_source": "environment",
        "policy": {
            "inference_retries": 0,
            "catalog_retries": 0,
            "inference_timeout": { "seconds": 30, "nanoseconds": 0 },
            "catalog_timeout": { "seconds": 10, "nanoseconds": 0 },
            "advisory_discovery_timeout": { "seconds": 2, "nanoseconds": 0 },
            "catalog": "unavailable",
            "retry_generation": "bound_generation",
            "semantic_replay_generation": "current_generation",
            "fallback": "disabled"
        }
    }))
    .expect("provider-free route receipt fixture")
}

pub(super) fn call(id: &str) -> kitsu_providers::ToolCall {
    kitsu_providers::ToolCall {
        tool_name: "read_file".to_string(),
        args: "{}".to_string(),
        call_id: id.to_string(),
    }
}

pub(super) fn chat_helper() -> ChatHelper {
    chat_helper_completing_in(
        Arc::new(Permissions::for_roots(&[], &[]).unwrap()),
        std::env::current_dir().unwrap(),
    )
}

/// `chat_helper` with the completion source's launch directory chosen, so
/// the `@file` half of the parity test needs no `set_current_dir`.
pub(super) fn chat_helper_completing_in(
    permissions: Arc<Permissions>,
    base: std::path::PathBuf,
) -> ChatHelper {
    ChatHelper {
        completion: ComposerCompletion::new(permissions, base),
        rich_palette: true,
        live_model_window: Arc::new(std::sync::Mutex::new((
            "test-model".to_string(),
            Some(8_000),
        ))),
        directory: "~/project".to_string(),
    }
}

/// The classic host's answer, in the shared vocabulary.
///
/// Everything here is the Rustyline *mapping* -- building a `Context` and
/// turning `Pair` back into `Candidate`. No candidate is chosen here.
pub(super) fn classic_candidates(
    helper: &ChatHelper,
    line: &str,
    pos: usize,
) -> kitsu_tui::composer::Completion {
    use rustyline::completion::Completer as _;

    let history = rustyline::history::DefaultHistory::new();
    let ctx = rustyline::Context::new(&history);
    let (start, pairs) = helper.complete(line, pos, &ctx).unwrap();
    kitsu_tui::composer::Completion {
        start,
        candidates: pairs
            .into_iter()
            .map(|pair| kitsu_tui::composer::Candidate::new(pair.display, pair.replacement))
            .collect(),
    }
}

/// The inline host's answer.
pub(super) fn inline_candidates(
    permissions: Arc<Permissions>,
    base: std::path::PathBuf,
    line: &str,
    pos: usize,
) -> kitsu_tui::composer::Completion {
    use kitsu_tui::composer::CompletionSource as _;

    tui_bridge::InlineComposerHost::new(ComposerCompletion::new(permissions, base), true)
        .complete(line, pos)
}

// -----------------------------------------------------------------------
// Card T3-S4: the history file, shared between the two renderers.
//
// There is exactly one history implementation and exactly one owner of the
// file: rustyline's `FileHistory`. `chat::run_chat` loads it,
// adds every submitted line on the path common to both readers, and saves
// it. The inline reader does not parse or write the file at all --
// `terminal::input::read_idle_chat_line` hands
// it `editor.history().iter().cloned().collect()`, a snapshot taken afresh
// on every read.
//
// So "shared" here is a *delegation* claim, and these tests pin it as one:
// the bytes on disk are rustyline's, and the list the inline reader walks
// is the list rustyline would have walked itself.
// -----------------------------------------------------------------------

/// The list `read_idle_chat_line` hands the inline reader, taken the same
/// way `terminal::input::read_idle_chat_line` takes it.
#[cfg(unix)]
pub(super) fn inline_history_snapshot(history: &rustyline::history::DefaultHistory) -> Vec<String> {
    history.iter().cloned().collect()
}

/// The classic renderer's ↑/↓ walk, over rustyline's own history.
///
/// This mirrors `rustyline::edit::State::edit_history_next`: the index
/// starts at `len()` (the live draft), ↑ decrements and reads
/// `get(idx, SearchDirection::Reverse)`, ↑ at index 0 is a no-op, and ↓
/// past the newest entry restores the backed-up draft. `ups` Ups are
/// followed by `downs` Downs; the visible draft is returned after each
/// keystroke.
///
/// **Fidelity limit, deliberate.** Up is bound to
/// `Cmd::LineUpOrPreviousHistory`, which tries `edit_move_line_up` first
/// and only walks history when that fails -- i.e. when the draft has no
/// line above the cursor. This helper models the walk, so every draft it
/// is given must be single-line at the moment the key arrives. Callers
/// keep to that; a multi-line draft is where the two renderers genuinely
/// part company (the editor has no intra-draft line motion) and that
/// belongs with the multi-row composer, not here.
#[cfg(unix)]
pub(super) fn classic_history_walk(
    history: &rustyline::history::DefaultHistory,
    draft: &str,
    ups: usize,
    downs: usize,
) -> Vec<String> {
    use rustyline::history::{History as _, SearchDirection};

    let mut index = history.len();
    let mut backup = String::new();
    let mut line = draft.to_string();
    let mut seen = Vec::new();
    for _ in 0..ups {
        if history.is_empty() || index == 0 {
            seen.push(line.clone());
            continue;
        }
        if index == history.len() {
            backup = line.clone();
        }
        index -= 1;
        if let Some(found) = history.get(index, SearchDirection::Reverse).unwrap() {
            index = found.idx;
            line = found.entry.into_owned();
        }
        seen.push(line.clone());
    }
    for _ in 0..downs {
        if history.is_empty() || index == history.len() {
            seen.push(line.clone());
            continue;
        }
        index += 1;
        if index < history.len() {
            if let Some(found) = history.get(index, SearchDirection::Forward).unwrap() {
                index = found.idx;
                line = found.entry.into_owned();
            }
        } else {
            line = backup.clone();
        }
        seen.push(line.clone());
    }
    seen
}

/// The inline renderer's ↑/↓ walk, over the snapshot it is handed.
///
/// The `RecallRequest` arm mirrors `tui_bridge.rs:651`: at the idle prompt
/// there is no prompt queue, so the reader declines the recall point and
/// takes the plain walk. Without that arm the first Up at an idle inline
/// prompt would do nothing, and the two renderers would not agree.
#[cfg(unix)]
pub(super) fn inline_history_walk(
    snapshot: &[String],
    draft: &str,
    ups: usize,
    downs: usize,
) -> Vec<String> {
    let mut editor =
        kitsu_editor::TypeaheadEditor::with_draft(snapshot.to_vec(), draft.to_string());
    let mut seen = Vec::new();
    for bytes in
        std::iter::repeat_n(&b"\x1b[A"[..], ups).chain(std::iter::repeat_n(&b"\x1b[B"[..], downs))
    {
        let outcome = editor.feed(bytes);
        for action in &outcome.actions {
            if *action == kitsu_editor::EditorAction::RecallRequest {
                let mut forced = kitsu_editor::FeedOutcome::default();
                editor.history_prev_forced(&mut forced);
            }
        }
        seen.push(editor.buffer_string());
    }
    seen
}

#[cfg(unix)]
pub(super) fn resolve(
    editor: &mut kitsu_editor::TypeaheadEditor,
    bytes: &[u8],
    queue: &mut prompt_queue::PromptQueue,
    dispatched: &mut Vec<String>,
) -> (ActionEffects, TypeaheadCaptureResult) {
    let outcome = editor.feed(bytes);
    let mut result = TypeaheadCaptureResult::default();
    let effects = resolve_editor_actions(
        editor,
        outcome,
        &mut result,
        |line, _| dispatched.push(line.to_string()),
        || {
            let queued = queue.status().queued;
            queued
                .checked_sub(1)
                .and_then(|newest| queue.remove(newest))
                .map(|prompt| (prompt, queued - 1))
        },
    );
    (effects, result)
}

/// Rust `Debug` spellings that must never reach a human deck surface: the
/// JSONL beside them already carries a clean DTO string for each.
pub(super) const DECK_DEBUG_LEAKS: [&str; 7] = [
    "AllowList(",
    "[Running]",
    "Unavailable",
    "Some(",
    "None(",
    "Medium",
    "CamelCase",
];

pub(super) fn assert_no_debug_leak(rendered: &str) {
    for leak in DECK_DEBUG_LEAKS {
        assert!(
            !rendered.contains(leak),
            "human deck output leaked the Rust Debug form {leak}: {rendered}"
        );
    }
}

/// Byte offset of one `/jobs view` group heading, matched as a whole line
/// at the documented two-space indent with its count parenthesis, so a
/// heading nested inside a row or missing its count never satisfies it.
pub(super) fn jobs_view_heading_offset(rendered: &str, heading: &str) -> Option<usize> {
    rendered.find(&format!("\n  {heading} ("))
}

pub(super) fn jobs_view_heading_count(rendered: &str, heading: &str) -> Option<usize> {
    let start = jobs_view_heading_offset(rendered, heading)? + 3 + heading.len() + 2;
    let rest = &rendered[start..];
    let end = rest.find(')')?;
    rest[..end].parse::<usize>().ok()
}

/// The detail continuation line for one agent row, trimmed of its indent so
/// the flat deck and the grouped view can be compared on content alone.
pub(super) fn jobs_detail_line(rendered: &str) -> &str {
    rendered
        .lines()
        .find(|line| line.contains("| tokens "))
        .unwrap_or_else(|| panic!("no agent detail line in: {rendered}"))
        .trim()
}

// -----------------------------------------------------------------
// T2-S3 (B5): the `Blocked` vocabulary on every /jobs surface. Nothing
// enters `Blocked` in this story, so each fixture is built by hand.
// -----------------------------------------------------------------

pub(super) fn blocked_progress(id: u64) -> subagent::AgentProgress {
    subagent::AgentProgress {
        id,
        task: "summarize the crate".to_string(),
        profile: Some("reviewer".to_string()),
        status: subagent::AgentProgressStatus::Blocked,
        waiting_for: None,
        elapsed_ms: 12_500,
        result_bytes: None,
        last_activity_ms_ago: Some(3_200),
        telemetry: subagent::AgentTelemetry {
            model: "qwen2.5-coder".to_string(),
            last_activity: Some("tool read".to_string()),
            ..Default::default()
        },
    }
}

/// Build the `(progress, thread)` pair `/jobs explain` renders from. The
/// two snapshots the command passes always describe the same agent, so the
/// helper keeps them consistent.
pub(super) fn explain_fixture(
    id: u64,
    status: subagent::AgentProgressStatus,
    phase: subagent::thread::AgentThreadPhase,
    handoff: subagent::thread::AgentHandoffState,
) -> (
    subagent::AgentProgress,
    subagent::thread::AgentThreadSnapshot,
) {
    let progress = subagent::AgentProgress {
        id,
        task: "summarize the crate".to_string(),
        profile: Some("reviewer".to_string()),
        status,
        waiting_for: None,
        elapsed_ms: 12_500,
        result_bytes: Some(42),
        last_activity_ms_ago: Some(3_200),
        telemetry: subagent::AgentTelemetry {
            model: "qwen2.5-coder".to_string(),
            last_activity: Some("tool read".to_string()),
            ..Default::default()
        },
    };
    let thread = subagent::thread::AgentThreadSnapshot {
        id,
        task: "summarize the crate".to_string(),
        model: "qwen2.5-coder".to_string(),
        profile: Some("reviewer".to_string()),
        name: None,
        phase,
        waiting_for: None,
        handoff,
        elapsed_ms: 12_500,
        result_bytes: Some(42),
        steering_open: true,
        pending_steering: 2,
        timeline: subagent::thread::AgentTimelineSnapshot {
            entries: vec![subagent::thread::AgentTimelineEntry {
                sequence: 1,
                elapsed_ms: 10,
                kind: subagent::thread::AgentTimelineKind::UserTask,
                round: None,
                text: "summarize the crate".to_string(),
                truncated: false,
            }],
            dropped_entries: 0,
            retained_bytes: 19,
            closed: false,
        },
    };
    (progress, thread)
}

/// The labelled lines of an explanation, in order, as `(label, value)`.
/// The header line is excluded, and every other line must be a labelled
/// one at the documented two-space indent.
pub(super) fn explain_labels(rendered: &str) -> Vec<(String, String)> {
    rendered
        .lines()
        .skip(1)
        .map(|line| {
            assert!(
                line.starts_with("  ") && !line.starts_with("   "),
                "explain lines sit at a two-space indent: {line:?}"
            );
            let (label, value) = line
                .trim_start()
                .split_once(": ")
                .unwrap_or_else(|| panic!("unlabelled explain line: {line:?}"));
            (label.to_string(), value.to_string())
        })
        .collect()
}

pub(super) fn explain_label_value<'a>(
    labels: &'a [(String, String)],
    label: &str,
) -> Option<&'a str> {
    labels
        .iter()
        .find(|(name, _)| name == label)
        .map(|(_, value)| value.as_str())
}

pub(super) fn retained_note_test_report(
    id: u64,
    paths: &[&str],
) -> kitsu_tools::builtins::fs::JournalTurnReport {
    kitsu_tools::builtins::fs::JournalTurnReport {
        id,
        label: format!("turn {id}"),
        changes: paths
            .iter()
            .enumerate()
            .map(
                |(index, path)| kitsu_tools::builtins::fs::JournalChangeReport {
                    path: std::path::PathBuf::from(path),
                    change: "updated",
                    before_bytes: Some(10),
                    after_bytes: 20,
                    first_changed_line: index + 1,
                    unified_diff: "DIFF-BODY-MUST-NOT-LEAK".to_string(),
                    diff_truncated: false,
                },
            )
            .collect(),
        unified_diff: "DIFF-BODY-MUST-NOT-LEAK".to_string(),
        diff_truncated: false,
    }
}

pub(super) async fn record_test_checkpoint(
    journal: &kitsu_tools::builtins::fs::WriteJournal,
    policy: Arc<Permissions>,
    label: &str,
    path: &std::path::Path,
    before: &str,
    after: &str,
) -> kitsu_tools::builtins::fs::JournalTurnReport {
    let start = journal.begin_attributed_turn(label);
    assert!(start.previous.is_none());
    let (_, handler) =
        kitsu_tools::builtins::fs::write_file_tool_with_journal(policy, Some(journal.clone()));
    let args = serde_json::json!({
        "path": path,
        "content": after,
        "expected_previous_content": before,
    })
    .to_string();
    journal
        .scope_turn(&start.token, handler(args))
        .await
        .expect("journaled write");
    journal.drain_mutations().await;
    journal
        .finish_attributed_turn(&start.token)
        .expect("checkpoint")
}

/// Parse exactly as `main` does — through ArgMatches so `apply_effort` can
/// distinguish user-set budget flags from defaulted ones.
pub(super) fn parse_with_effort(argv: &[&str]) -> Cli {
    // Hermetic: drop the KITSU_EFFORT env binding so a developer's own
    // `export KITSU_EFFORT=max` can't leak in. Thread-safe, unlike
    // mutating the process environment under parallel tests.
    let matches = Cli::command()
        .mut_arg("effort", |arg| arg.env(None))
        .try_get_matches_from(argv)
        .expect("effort fixture parses");
    let mut cli = Cli::from_arg_matches(&matches).expect("effort fixture builds");
    cli.apply_effort(&matches);
    cli
}

/// Scoped env-var guard; restores the prior value (or unsets) on drop.
pub(super) struct EnvVarGuard {
    pub(super) key: &'static str,
    pub(super) prev: Option<String>,
}

impl EnvVarGuard {
    pub(super) fn set(key: &'static str, value: &str) -> Self {
        let prev = std::env::var(key).ok();
        std::env::set_var(key, value);
        Self { key, prev }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        match &self.prev {
            Some(v) => std::env::set_var(self.key, v),
            None => std::env::remove_var(self.key),
        }
    }
}

/// `CliError` deliberately doesn't derive `Debug` (see its definition),
/// so tests unwrap through this helper instead of `.unwrap()`.
pub(super) fn expect_ok<T>(result: Result<T, CliError>) -> T {
    result.unwrap_or_else(|error| panic!("{error}"))
}

// ------------------------------------------------------------------
// T2-S5b: `/jobs reply <id> <y|a|n>` releases a parked agent.
//
// FAILING-FIRST. No dispatcher has a `reply` arm yet, so every test
// below currently fails: the two dispatch sites fall through to their
// unknown-subcommand branch, and the discoverability strings and the
// suggester table do not mention `reply`.
// ------------------------------------------------------------------

/// Park the exact request a path gate would park for `agent_id`, and hand
/// back the oneshot the gate itself is waiting on. Mirrors the registry's
/// own `park_path_request` fixture, which is private to `subagent`.
#[cfg(unix)]
pub(super) fn park_a_path_request(
    sender: &kitsu_tools::PromptRequestSender,
    agent_id: u64,
) -> tokio::sync::oneshot::Receiver<Decision> {
    let (reply, answer) = tokio::sync::oneshot::channel();
    sender
        .send(kitsu_tools::PromptRequest {
            agent_id,
            payload: kitsu_tools::PromptRequestPayload::Path(kitsu_tools::PermissionRequest {
                action: kitsu_tools::Action::Write,
                path: "/work/notes.txt".to_string(),
                grant_dir: "/work".to_string(),
            }),
            reply,
        })
        .expect("the registry owns the receiver and keeps it open");
    answer
}

/// A registry holding exactly one agent parked on a permission request,
/// plus the gate's oneshot. Yields rather than sleeps, so it is safe under
/// a paused clock.
#[cfg(unix)]
pub(super) async fn registry_with_one_blocked_agent() -> (
    Arc<subagent::AgentRegistry>,
    u64,
    tokio::sync::oneshot::Receiver<Decision>,
) {
    let registry = subagent::AgentRegistry::new();
    let id = registry.start("needs an answer".to_string()).unwrap();
    let sender = registry.prompt_sender();
    let answer = park_a_path_request(&sender, id);
    for _ in 0..1_000 {
        let blocked = registry
            .progress_snapshot()
            .agents
            .into_iter()
            .any(|agent| agent.id == id && agent.status == subagent::AgentProgressStatus::Blocked);
        if blocked {
            return (registry, id, answer);
        }
        tokio::task::yield_now().await;
    }
    panic!("agent #{id} never became blocked after a request was parked");
}

// ------------------------------------------------------------------
// Card T3-S9: `/tui classic|inline` as a runtime switch.
//
// `/tui` is NOT a command yet (`SLASH_COMMANDS` has no `tui` entry and
// the dispatcher has no arm), so every test below this banner fails.
// ------------------------------------------------------------------

/// The refusal a `/tui` typed as type-ahead **during a turn** gets.
///
/// The decision this card takes, and the reason: the default fall-through
/// in `TypeaheadDispatcher::dispatch` is `queue.enqueue(line)`, so an
/// unhandled `/tui inline` becomes a **future prompt sent to the model** —
/// strictly worse than doing nothing. Queuing the *switch* instead was
/// rejected because the mode a user asks for mid-turn is a mode they want
/// to see now, and a swap that lands silently several seconds later at the
/// turn boundary is indistinguishable from the switch they would get by
/// typing it again. So: refused, named, and the renderer untouched.
pub(super) const TUI_MID_TURN_REFUSAL: &str =
    "/tui is accepted only between turns; the renderer is not swapped mid-turn";

// --- card T4-S4b-i, finding F68: a paste is not a sequence of keys ---
//
// The action keys of T4-S4b bound effects to bare letters: `r` opens the
// decision read whose next key *answers a pending permission*, and `x`
// twice stops a live agent or prunes a terminal one. The overlay reads raw
// bytes and does not enable bracketed paste, so a pasted correction is
// indistinguishable from typing and every byte of it becomes a key: paste
// "retry the write" over the row list and `r` then `y` approves a
// filesystem write the user never approved.
//
// That is **F65**'s root with the opposite sign. F65: an arrow key is not
// one byte. F68: a chunk of bytes is not a sequence of intentions.
//
// Where each assertion lives, and why:
//
// * **The decoder** answers "what does this chunk *mean*", which is the
//   only place the paste/typing distinction exists at all — the loop has
//   nothing else to go on. [`OverlayDecoder`] is pure, so the meaning of
//   a pasted chunk is pinned with no terminal involved.
// * **A real `AgentRegistry`** answers "did anything *happen*", because a
//   permission still being pending is a registry fact. T4-S3 and T4-S4a
//   pinned the action keys here for the reason that has not changed: a
//   fixture PTY session has **zero agents**, so every key bound to the
//   selected agent meets the no-selection case there and no permission
//   can be answered at all.
// * **The PTY** keeps routing (`tests/tui.rs`): that the keys are the
//   view's and reach no composer. It cannot see a permission.
//
// The registry tests drive the decoder's output through the loop's own
// dispatch, [`handle_overlay_input`] -- the one `match` on
// [`OverlayInput`] in the binary. They used to drive a test-local model
// that mirrored those arms instead, and T4-S4b-ii made it diverge while
// every test still passed: a mirror that no longer reflects **cannot
// fail** when the two disagree, because agreement is not among the things
// it checks (finding **F69**). [`OverlayUnderTest`] is now only the
// terminal's half -- a `read` of the descriptor and the reports the frame
// would have painted -- so there is nothing left to diverge.

/// The terminal's half of `run_agent_view_overlay`, and nothing else: one
/// `read` of the descriptor, the decoder, and the reports the frame would
/// have painted. Every key and every paste is handled by the loop's own
/// [`handle_overlay_input`], against the loop's own [`OverlayState`], so
/// the registry effect of a chunk of bytes is observable without a tty
/// and without a second copy of the arms.
///
/// The `Paste` arms it exercises are F68's specification: content inside a
/// text prompt, and nothing that acts over the row list or inside `r`'s
/// decision read.
#[cfg(unix)]
pub(super) struct OverlayUnderTest {
    pub(super) state: OverlayState,
    pub(super) decoder: OverlayDecoder,
    pub(super) reports: Vec<String>,
}

/// The width the loop would have read from the terminal. Only the peek
/// panel and the painted rows use it, and neither is asserted here.
#[cfg(unix)]
pub(super) const OVERLAY_TEST_COLS: usize = 80;

#[cfg(unix)]
impl OverlayUnderTest {
    pub(super) fn new() -> Self {
        Self {
            state: OverlayState::new(),
            decoder: OverlayDecoder::new(),
            reports: Vec::new(),
        }
    }

    /// One `read` of the descriptor, decoded and applied -- the body of
    /// the loop's `for input in decoder.feed(..)`, with the report row
    /// collected instead of painted.
    pub(super) fn feed(&mut self, registry: &subagent::AgentRegistry, chunk: &[u8]) {
        self.apply(registry, chunk);
        if let Some(line) = self.state.report.take() {
            self.reports.push(line);
        }
    }

    /// One `read` of the descriptor, decoded and applied, with the report
    /// left **in the state** exactly as the loop leaves it there.
    ///
    /// [`OverlayUnderTest::feed`] takes the report out afterwards, to
    /// collect it; [`OverlayUnderTest::feed_and_frame`] leaves it in,
    /// because the frame is painted from it. Both go through this one
    /// body, so there is one byte path here and not two (**F69**).
    ///
    /// Returns what the loop's `overlay_feed_chunk` returned -- whether
    /// the view is closing -- which card T4-S6a's attach needs and the
    /// earlier cards' callers ignore.
    pub(super) fn apply(&mut self, registry: &subagent::AgentRegistry, chunk: &[u8]) -> bool {
        let rows = overlay_visible_rows(registry, &self.state);
        self.state.selection.sync(&rows);
        // The loop's own byte path, whole: the open line's bypass and the
        // decoder's keys through the one dispatch (**F69**). The report
        // row is collected instead of painted; the loop paints it and
        // keeps the last one, and nothing in the dispatch reads it back.
        overlay_feed_chunk(
            registry,
            &mut self.state,
            &mut self.decoder,
            &rows,
            OVERLAY_TEST_COLS,
            chunk,
        )
    }

    /// One `read`, returning whether the view is closing (card T4-S6a).
    /// The same single body as every other helper here: `apply` is the
    /// loop's byte path and this adds nothing to it (**F69**).
    pub(super) fn feed_closing(
        &mut self,
        registry: &subagent::AgentRegistry,
        chunk: &[u8],
    ) -> bool {
        self.apply(registry, chunk)
    }

    /// One `read`, then **the rows the loop would paint afterwards** —
    /// the loop's own two steps in the loop's own order, through the
    /// loop's own two functions (**F69**).
    ///
    /// This is the only honest way to assert that a message is *on
    /// screen*: `self.reports` says a message exists, and **F71** is the
    /// finding that those are different claims.
    /// Paint at a caller-chosen width. A *visibility* assertion needs a
    /// frame wide enough to hold the row it is about: every frame row goes
    /// through `fit_row_to_width`, so at 80 columns an 87-column refusal is
    /// truncated and `row.trim() == expected` becomes arithmetically
    /// unsatisfiable (F58). Width-fitting itself is pinned separately by
    /// `the_steer_line_fits_a_narrow_terminal`, so widening here loses
    /// nothing.
    pub(super) fn feed_and_frame_at(
        &mut self,
        registry: &subagent::AgentRegistry,
        chunk: &[u8],
        cols: usize,
    ) -> Vec<String> {
        self.apply(registry, chunk);
        let rows = overlay_visible_rows(registry, &self.state);
        overlay_frame_rows(&self.state, &rows, cols)
    }

    pub(super) fn feed_and_frame(
        &mut self,
        registry: &subagent::AgentRegistry,
        chunk: &[u8],
    ) -> Vec<String> {
        self.apply(registry, chunk);
        let rows = overlay_visible_rows(registry, &self.state);
        self.state.selection.sync(&rows);
        overlay_frame_rows(&self.state, &rows, OVERLAY_TEST_COLS)
    }

    /// The loop's 50 ms grace, resolved: the *waiting* is the terminal's
    /// and the resolution is not, so the tests take the same two
    /// functions the loop takes (**F69**). Returns whether the view is
    /// closing.
    pub(super) fn quiet_tick(&mut self) -> bool {
        if !overlay_escape_held(&self.state, &self.decoder) {
            return false;
        }
        let closing = overlay_resolve_held_escape(&mut self.state, &mut self.decoder);
        if let Some(line) = self.state.report.take() {
            self.reports.push(line);
        }
        closing
    }

    /// The rows the frame would be painting: what `/`'s filter leaves
    /// visible, which is also what the selection is synced against.
    pub(super) fn visible(
        &self,
        registry: &subagent::AgentRegistry,
    ) -> Vec<kitsu_tui::agents_view::AgentRow> {
        overlay_visible_rows(registry, &self.state)
    }

    /// Which agent the action keys would act on, after the same sync the
    /// loop does every repaint.
    pub(super) fn selected(&mut self, registry: &subagent::AgentRegistry) -> Option<u64> {
        let rows = overlay_visible_rows(registry, &self.state);
        self.state.selection.sync(&rows);
        self.state.selection.selected()
    }

    pub(super) fn draft(&self) -> Option<&str> {
        self.state.renaming.as_ref().map(SteerLine::draft)
    }
}

/// The bracketed-paste delimiters, as every terminal that reports pastes
/// sends them. Spelled here rather than imported because the overlay has
/// no copy of them yet — enabling `ESC[?2004h` is part of F68's fix.
#[cfg(unix)]
pub(super) const OVERLAY_PASTE_START: &[u8] = b"\x1b[200~";
#[cfg(unix)]
pub(super) const OVERLAY_PASTE_END: &[u8] = b"\x1b[201~";

/// A real correction someone would paste into a steer line, and the reason
/// F68 is a rejection: its bytes contain `r`, then `y`, then `x` twice,
/// then a newline with more text after it (**F58** — the sample is the
/// expectation's source, not a letter picked to make a point).
#[cfg(unix)]
pub(super) const PASTED_CORRECTION: &str = "retry the exxtract\nand fix the bug";

// --- card T4-S5: the filter line (`/`), requirement A3's last unbound key
//
// Where each assertion lives follows the split that has held for four
// cards: the help row is `kitsu-tui`'s (it is painting), key *routing* is
// the PTY's, and everything a filter *does* is here, against a real
// registry — a fixture PTY session has zero agents, so a filter could not
// hide one there.
//
// The two decisions this card had to take, and the reasoning each test
// carries:
//
// * **The filter's rows are the selection's rows.** `AgentSelection::sync`
//   is handed the **filtered** rows, not the registry's. Handing it the
//   full rows would leave the selection on an agent the filter has hidden,
//   and then `x`, `r` or `n` would act on an agent **not on screen** —
//   a privileged action with no visible intent, which is **F68**'s family
//   and cost this epic a rejection. `sync` already does the right thing
//   with a shorter list: it falls to the row holding the old display index
//   and reports `None` only when the list is empty.
// * **The line is an editor line, not a decoded prompt.** `OverlayKey::Char`
//   is ASCII-only by construction (`0x20..=0x7e`), while a name may be
//   non-ASCII — `AgentRegistry::rename` refuses control characters and
//   nothing else — so a filter line fed decoded keys could not express
//   `n:café`, a filter for a name the registry accepts. The steer line
//   already reads raw bytes through `kitsu-editor` for exactly this
//   reason (card T4-S4b-ii), and a second line editor here would be a
//   second parser of the same bytes (**F45**).

/// Three agents whose rows differ in the fields `/`'s grammar reads: the
/// task (`o:`) and the name (`n:`). Real task text, which is also the
/// source of every expectation below rather than a letter picked to make
/// a point (**F58**).
#[cfg(unix)]
pub(super) fn registry_with_three_agents() -> (Arc<subagent::AgentRegistry>, [u64; 3]) {
    let registry = subagent::AgentRegistry::new();
    let first = registry
        .start("alpha: audit the overlay decoder".to_string())
        .unwrap();
    let second = registry
        .start("beta: rewrite the repaint loop".to_string())
        .unwrap();
    let third = registry
        .start("gamma: measure the paste latency".to_string())
        .unwrap();
    (registry, [first, second, third])
}

/// Type one filter into an open line, byte by byte, the way a terminal
/// delivers typing: one `read` per keystroke.
#[cfg(unix)]
pub(super) fn type_filter(
    overlay: &mut OverlayUnderTest,
    registry: &subagent::AgentRegistry,
    text: &str,
) {
    for byte in text.as_bytes() {
        overlay.feed(registry, &[*byte]);
    }
}

// --- card F70: the `n` key must be able to type a name the registry accepts
//
// The defect, observed live: `OverlayKey::Char` is ASCII-only by
// construction (`plain()` maps `0x20..=0x7e` and drops every other byte)
// and `n` fills its draft solely from `OverlayKey::Char(typed)`, while
// `AgentRegistry::rename` refuses only empty, over-length and
// `char::is_control` names. Typing `n` then `café` renamed the agent to
// `caf`. **A validation rule is only as wide as the narrowest input path
// that feeds it** (F70).
//
// The fix shape is T4-S5's, one card old: read the line's raw bytes
// through `kitsu-editor` on the byte bypass in `overlay_feed_chunk` that
// `s` and `/` already take. That brings non-ASCII, a multi-byte character
// split across reads and F68's paste rule in one move.
//
// Two decisions these tests take, so the implementer does not have to
// guess:
//
// * **A refused rename keeps the frame the name was typed on.** This is
//   **F71** exactly: once the rename line is an innermost frame like the
//   filter's, a refusal left in `state.report` is painted *under the row
//   list*, which is not on screen while a line is open — observable to a
//   test that reads `state.report`, invisible to the user. So every
//   refusal below is asserted against [`overlay_frame_rows`], the real
//   frame chain, and the line stays **open carrying the text**, exactly
//   as `/`'s refused term does (`OverlayLine::restore` exists for it), so
//   the one wrong character is corrected rather than the name retyped.
// * **`r`'s decision read is not one of these.** `OverlayPrompt::Decision`
//   is a single-key read of `y`/`a`/`n`, all ASCII, and legitimately stays
//   decoded. Nothing here touches it.

/// One line typed into an open overlay line, byte by byte, the way a
/// terminal delivers typing: **one `read` per byte**, so a multi-byte
/// character really does arrive split across two reads.
///
/// `apply` and not `feed`, because these tests read the report off the
/// painted frame and `feed` takes it out of the state to collect it.
#[cfg(unix)]
pub(super) fn type_line(
    overlay: &mut OverlayUnderTest,
    registry: &subagent::AgentRegistry,
    text: &str,
) {
    for byte in text.as_bytes() {
        overlay.apply(registry, &[*byte]);
    }
}

/// The name in F70's own report, which the registry accepts and the
/// keyboard cannot express (**F58**: the sample is where the expectation
/// comes from).
#[cfg(unix)]
pub(super) const NON_ASCII_NAME: &str = "café";

/// A name one character past `AgentRegistry::NAME_MAX_CHARS`, built from
/// real task text rather than a run of one letter (**F58**).
#[cfg(unix)]
pub(super) fn over_long_name() -> String {
    let sample = "audit the overlay decoder and the repaint loop too";
    let name: String = sample
        .chars()
        .take(subagent::AgentRegistry::NAME_MAX_CHARS + 1)
        .collect();
    assert_eq!(
        name.chars().count(),
        subagent::AgentRegistry::NAME_MAX_CHARS + 1,
        "NON-VACUOUS: the sample is long enough to be over the limit by \
             exactly one character"
    );
    name
}

// ------------------------------------------------------------------
// Card T4-S6a: **attach** -- the agent view's half of requirement A3's
// `Enter`/`→`, and requirement A7's custody question.
//
// Every test under this banner FAILS today: `Enter` is inert over the
// row list, `→` is not decoded at all, and `OverlayState::attaching`
// is written by nobody.
//
// **Why attach is `OverlayState::attaching` and a close**, rather than
// a step of its own: the key's whole effect is to name the agent the
// composer underneath is now routed to. The stream it shows goes to the
// **main viewport** (A5), so a full-screen overlay left up over it
// would hide the thing the user attached to see.
// ------------------------------------------------------------------

/// The one line `Enter` and `→` leave when there is nothing to attach to.
///
/// Deliberately **not** `agent_view_no_selection`'s wording: that helper
/// interpolates the keystroke as a character (`` `x` has nothing to act
/// on``), which works for the printable action keys and for neither of
/// these two. A row carrying a raw `\r` would also corrupt the frame it
/// is painted on.
#[cfg(unix)]
pub(super) const ATTACH_WITHOUT_SELECTION: &str =
    "nothing is selected, so there is no agent to attach to";

// ---- Card T4-S7, requirement A6: dispatch from the view composer ----
//
// What is pinned here is the overlay's half: the key that opens the
// fourth line, what the line hands the dispatcher, and that every refusal
// is painted **on the frame the user is looking at** (**F71**). What
// *admits* a dispatch is the registry's and is pinned in `subagent.rs`,
// where a provider fixture exists -- the same split T4-S4b and T4-S6b
// were made along, and for the same reason: a dispatch that really starts
// an agent needs a provider, and the registry fixture here has none.

/// The dispatch key as the bytes a terminal delivers for it, read off the
/// one constant the help row and the overlay's arm also read. The letter
/// is not spelled in these tests: that is `agents_view`'s to choose, and a
/// test that re-spelled it would be a second source of truth for it.
#[cfg(unix)]
pub(super) fn dispatch_key() -> Vec<u8> {
    kitsu_tui::agents_view::DISPATCH_KEY
        .to_string()
        .into_bytes()
}

/// A dispatcher that records what it was handed and answers however the
/// test says, without a provider anywhere near it.
///
/// This is the seam the card exists to create: the overlay holds a
/// [`subagent::ViewDispatcher`] and nothing else, so everything the view
/// does with a dispatch is observable here, and whether the *registry*
/// admits it is a question for `subagent.rs`.
/// What a recording dispatcher saw: one `(task, profile)` per dispatch.
#[cfg(unix)]
pub(super) type DispatchesSeen = Arc<Mutex<Vec<(String, Option<String>)>>>;

#[cfg(unix)]
pub(super) fn recording_dispatcher(
    answer: Result<u64, String>,
) -> (subagent::ViewDispatcher, DispatchesSeen) {
    let seen: DispatchesSeen = Arc::new(Mutex::new(Vec::new()));
    let recorded = seen.clone();
    let dispatcher: subagent::ViewDispatcher = Arc::new(move |task, profile| {
        recorded
            .lock()
            .expect("the recording dispatcher")
            .push((task, profile));
        answer.clone()
    });
    (dispatcher, seen)
}

// ---- Card T4-S10: the one bindings table ------------------------------
//
// §4.7: "bindings are declared in one table consumed by the help overlay
// and `/help keys` so docs cannot drift". The table is
// `kitsu_tui::keys::BINDINGS` and the help overlay's agreement with it is
// pinned in `kitsu-tui`, where both live.
//
// The third reader is the **dispatch**, and it cannot be pinned that way:
// `handle_overlay_input` matches on `OverlayKey`, a type private to this
// binary, so a table in `kitsu-tui` cannot be the `match` and cannot call
// it either. Finding **F57** is exactly the trap that leaves: a single
// source of truth makes every reader agree with it and says nothing about
// whether anything acts on what they read, and the guard that reads the
// same source it is checking cannot fail. So the guard here reads
// **neither** source: it sweeps the key space the overlay's decoder can
// produce, presses every key, and asks the painted frame whether anything
// happened.

/// The width the T4-S10 sweep paints at. Wide on purpose: a truncated
/// frame can hide the difference a key made and make the sweep claim a
/// bound key is inert (**F58**, **F78**).
#[cfg(unix)]
pub(super) const BINDINGS_SWEEP_COLS: usize = 200;

// ------------------------------------------------------------------
// Card **T4-S6b**: **the routed composer while attached**, requirement
// A5, plus the `\u{26E9}` attach milestone (design \u{a7}4.5).
//
// Every test under this banner FAILS today: `attached_route_target`
// answers `None` for everything, `route_attached_submission` sends
// nothing and paints nothing, and `apply_agent_view_exit` paints no
// milestone.
//
// **The steer-versus-answer rule this card settles.** A5 says the
// composer "sends `steer`, or an approval answer when the agent is
// blocked" and does not say *when* that is decided. The race is real:
// you are attached to a working agent, you type a correction, the agent
// blocks on a permission while you are typing, and you press Enter.
// Routing by the state at **open** time would send a steer the registry
// must refuse; routing by the state at **submit** time would feed a
// carefully typed correction to `parse_permission_answer`, which does not
// read it as `y`/`a`/`n`. Neither is acceptable on its own, so the rule
// is submit-time state **plus a refusal that destroys nothing**:
//
// * the state is read **once**, at submit time;
// * **blocked** \u{2192} answered, by [`parse_permission_answer`] and nothing
//   else (**F45**: there is one parser, and this card does not add a
//   third); a line that parser does not accept is **refused with nothing
//   sent**, which leaves the agent in the one state from which it can
//   still be answered correctly;
// * **anything else** \u{2192} steered, and the *registry* adjudicates: an
//   agent that blocked or finished in the gap refuses in its own words
//   rather than being answered or corrected from a stale read;
// * **every** refusal hands the line back as the next read's draft
//   (**F76**) and paints its reason through the renderer's surface
//   (**F71**), so the user keeps their words and reads why on the frame
//   they are looking at.
//
// The user is told by the receipts `/jobs steer` and `/jobs reply`
// already print, and refused in the words `r`'s decision read already
// uses -- derived here rather than respelled, so this route cannot drift
// from the two that exist.
//
// **What enumerates the route (F82).** The route adds no key: it changes
// what `Enter` means, and the only moment that change is announced is the
// attach itself. That is why the `\u{26E9}` milestone belongs to this card
// and is asserted to **name the route**, not merely the agent.
// ------------------------------------------------------------------

/// Every transcript line written through the renderer's surface seam,
/// with the stream it was written on.
///
/// **F71**: a visibility claim has to go through the surface that paints.
/// A test that read a returned `String` would pass against a route that
/// computed a refusal and painted nothing -- which is exactly T4-S8's
/// vacuity result -- so the painted bytes are the assertion here.
#[cfg(unix)]
#[derive(Default)]
pub(super) struct PaintedLines(
    pub(super) std::sync::Mutex<Vec<(kitsu_render::surface::Fd, String)>>,
);

#[cfg(unix)]
impl kitsu_render::surface::Surface for PaintedLines {
    fn transcript_line(&self, fd: kitsu_render::surface::Fd, text: &str) {
        self.0
            .lock()
            .expect("painted lines")
            .push((fd, text.to_string()));
    }
}

/// Install a recorder over the surface slot, and put the displaced
/// surface back on drop: a panicking assertion must not leak a recorder
/// into the rest of this test binary, where it would swallow every later
/// transcript line. Tests using it are `#[serial]`, the slot being
/// process-global.
#[cfg(unix)]
pub(super) struct SurfaceProbe {
    pub(super) previous: Option<std::sync::Arc<dyn kitsu_render::surface::Surface>>,
    pub(super) painted: std::sync::Arc<PaintedLines>,
}

#[cfg(unix)]
impl SurfaceProbe {
    pub(super) fn install() -> Self {
        let painted = std::sync::Arc::new(PaintedLines::default());
        let previous = kitsu_render::surface::set_surface(painted.clone());
        Self {
            previous: Some(previous),
            painted,
        }
    }

    /// The stderr lines painted so far. Meta lines -- milestones,
    /// receipts, refusals -- are stderr by the seam's own contract
    /// (`Fd::Stderr`), and asserting the stream is part of the claim: a
    /// receipt on stdout would land in a redirected transcript file.
    pub(super) fn lines(&self) -> Vec<String> {
        self.painted
            .0
            .lock()
            .expect("painted lines")
            .iter()
            .map(|(fd, text)| {
                assert_eq!(
                    *fd,
                    kitsu_render::surface::Fd::Stderr,
                    "a meta line belongs on stderr: {text:?}"
                );
                text.clone()
            })
            .collect()
    }
}

#[cfg(unix)]
impl Drop for SurfaceProbe {
    fn drop(&mut self) {
        if let Some(previous) = self.previous.take() {
            kitsu_render::surface::set_surface(previous);
        }
    }
}

/// The exit an agent view leaves behind when `Enter`/`\u{2192}` attached to
/// `id`, built as [`AgentViewExit::leaving`] builds it.
#[cfg(unix)]
pub(super) fn attaching_exit(id: u64) -> AgentViewExit {
    AgentViewExit {
        shown: true,
        attached: Some(id),
    }
}

/// The milestone message for one attach: the `\u{26E9}` line's text, without
/// the glyph and styling [`kitsu_render::milestone_line`] adds.
///
/// **It names the route** (**F82**). The composer's `Enter` has just
/// changed meaning, no key was added that a key table could enumerate,
/// and this line is the one place that change is announced; a bare
/// "attached to agent 7" would leave the user to discover by experiment
/// that their next line leaves the session.
#[cfg(unix)]
pub(super) fn attach_milestone_message(id: u64) -> String {
    format!(
        "attached to agent {id}; your next line steers it, or answers it \
             while it is blocked"
    )
}

/// A ringed surface over the slot, put back on drop, with its retained
/// cells readable: the production `InlineSurface::with_cell_ring`, not a
/// double, because the claim is about what the surface the session
/// installs retains (**F69**).
///
/// `SurfaceProbe` cannot serve here -- `PaintedLines` keeps no cells -- and
/// the sinks are swallowed because these tests assert on the ring, while
/// the byte stream is pinned in `tui_bridge`.
#[cfg(unix)]
pub(super) struct RingProbe {
    pub(super) previous: Option<std::sync::Arc<dyn kitsu_render::surface::Surface>>,
    pub(super) surface: std::sync::Arc<kitsu_tui::viewport::InlineSurface>,
}

#[cfg(unix)]
impl RingProbe {
    pub(super) fn install() -> Self {
        let surface = std::sync::Arc::new(kitsu_tui::viewport::InlineSurface::with_cell_ring(
            Box::new(std::io::sink()),
            Box::new(std::io::sink()),
            16,
        ));
        let previous = kitsu_render::surface::set_surface(surface.clone());
        Self {
            previous: Some(previous),
            surface,
        }
    }

    /// The retained cells as `(is a prompt, stripped text)`.
    pub(super) fn marked(&self) -> Vec<(bool, String)> {
        self.surface
            .cells()
            .expect("a surface built with a cell ring retains cells")
            .iter()
            .map(|line| (line.is_prompt(), line.plain()))
            .collect()
    }
}

#[cfg(unix)]
impl Drop for RingProbe {
    fn drop(&mut self) {
        if let Some(previous) = self.previous.take() {
            kitsu_render::surface::set_surface(previous);
        }
    }
}

// ------------------------------------------------------------------
// Card T4-S6c -- the foreground buffer and the recap on detach.
//
// Design \u{a7}4.5's remaining clause: *"foreground orchestrator output is
// buffered (bounded, 64 KiB; overflow noted) and replayed with a
// `\u{26E9} detached from agent N -- recap:` milestone on return."*
//
// **Its own buffer, not the cell ring.** The ring
// (`kitsu_tui::cells::CellRing`) is bounded by line **count**, and
// **F52** is the finding that said so: *"N4 said memory and specified
// lines; a single huge line is still unbounded."* \u{a7}4.5 specifies 64
// KiB -- bytes -- so reusing the ring would reintroduce the hazard F52
// named. The byte-bounded buffer lives in `kitsu-render`, beside the
// `Surface` trait it decorates; the ring lives in `kitsu-tui`, which
// `kitsu-render` cannot depend on, so there is no shared mechanism to
// reach for even if the units agreed.
//
// **The header is painted only over content.** The two keys are silent
// by \u{a7}4.5's own division (*"the two keys are silent and the command
// answers"*) and the milestone is the recap's **header** -- \u{a7}4.5
// makes it part of the replay, not part of the detach. A header with
// nothing under it would both break that silence and claim a recap that
// does not exist.
// ------------------------------------------------------------------

/// The recap header's text: the `\u{26E9}` line without the glyph and
/// styling [`kitsu_render::milestone_line`] adds.
#[cfg(unix)]
pub(super) fn detach_recap_message(id: u64) -> String {
    format!("detached from agent {id} \u{2014} recap:")
}

/// One meta line the foreground orchestrator produces, written exactly
/// the way every routed writer in this binary writes one -- through the
/// surface slot, so whatever is installed there is what receives it.
#[cfg(unix)]
pub(super) fn foreground_meta(text: &str) {
    kitsu_render::surface::active_surface()
        .transcript_line(kitsu_render::surface::Fd::Stderr, text);
}

/// One read of the inline composer with the configured keymap, as the
/// `F76` test above drives it.
#[cfg(unix)]
pub(super) fn one_composer_read(attached: Option<u64>, bytes: &[u8]) -> tui_bridge::InlineRead {
    let keys = tui_bridge::InlineKeys {
        prefix: tui_bridge::chord_prefix_byte("ctrl+x"),
        left_arrow_opens_agents: true,
    };
    assert!(keys.prefix.is_some(), "the chord prefix must be bound");
    let mut input = std::io::Cursor::new(bytes.to_vec());
    let mut paint = |_: &str, _: usize, _: &str, _: bool| {};
    tui_bridge::read_line_inline_with_keys(
        &mut input,
        &mut paint,
        &[],
        &mut || {},
        keys,
        attached,
        None,
        None,
    )
    .expect("a cursor cannot fail to read")
}
