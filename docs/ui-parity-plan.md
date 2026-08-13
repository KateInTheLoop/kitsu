# UI parity plan — kitsu vs Claude Code CLI vs Codex CLI

Date: 2026-08-12. Baseline: `5f3b2bc`-dirty on `dogfood/codex-ux-cycle-6`.
Line numbers below were verified on this baseline; if they have drifted,
grep for the named function.

This document is self-contained: an implementation agent needs no other
context to execute one queue item. Research inputs: a live PTY run of
`./target/debug/kitsu --gateway ollama --model qwen2.5-coder:latest
--context-window 8192 chat` (banner, spinner, `/help`, `/status`, `/theme`,
`/context`, streamed turn, stats footer), `claude -p 'say hi'`, the full
`crates/kitsu-render/src/lib.rs`, `docs/ux-design.md`,
`docs/ui_guidelines.md`, `docs/agent_deck_ui.md`, and the cycle reports
(`docs/chat-ux-cycle-6.md` … `-22.md`, incl. the Codex side-by-side audit
`docs/chat-ux-cycle-6-audits/raw/01-codex-parity.md` and the cycle-10
day-in-the-life audit).

## Comparative assessment

### Output writing quality

- **Claude Code**: polished conversational prose; headings, bullets, and
  code render as styled terminal markdown. `-p` mode prints exactly the
  final answer, zero noise.
- **Codex**: terse and technical — `file:line` references, one-line
  timing/tokens footers, "No files were modified." explicit closing lines.
- **Kitsu**: receipt/error writing is a genuine strength — `! error:` lines
  carry remedies, run summaries count reads/writes/denials honestly, the
  cycle-10 auditor rated failure accounting "exemplary". But assistant text
  shows raw markdown markers (`**`, `` ` ``, `-`) untreated except bold `#`
  headings, so long model answers *read* less finished than either baseline.

**Verdict: kitsu ahead on meta-text (receipts, errors, telemetry), behind on
assistant-text presentation.**

### Streaming feel

- **Claude Code**: streams markdown that is styled as it arrives, including
  syntax-highlighted code blocks; spinner cycles verbs with elapsed seconds
  and a token counter.
- **Codex**: prints completed blocks; minimal but never noisy.
- **Kitsu**: token-level streaming via `print_fragment`
  (`crates/kitsu-core/src/orchestrator.rs:1127`) is **completely
  unstyled** — the light markdown/diff treatment lives only on the buffered
  `print_message` path (`kitsu-render/src/lib.rs` `format_message`,
  ~line 306). The fox-kaomoji spinner is a strong identity moment but shows
  no elapsed time and one fixed phrase ("kitsu is thinking").

**Verdict: kitsu's raw latency-to-first-byte feel is fine; the styling gap
between streamed and buffered turns is the single biggest visual parity gap.**

### Code and diff rendering

- **Claude Code**: syntax-highlighted fenced blocks; boxed diffs with +/-
  coloring and file headers.
- **Codex**: clean fenced `diff --git` hunks; propose-then-apply workflow.
- **Kitsu**: `format_message` colors ```diff fences (+green/−red/@@cyan) and
  dims fence lines — buffered path only; no syntax highlighting anywhere
  (syntect is a standing backlog item since cycle 6). `/diff`'s
  `render_diff` (`kitsu-render/src/lib.rs:1120`) colors hunks and bolds
  `diff --git` but has no summary header or per-file stat line.

**Verdict: functional but flat; syntect and a diff header are the two
missing pieces.**

### Progress and status

- **Claude Code**: spinner verbs, tool-call cards with durations, progress
  trees for subagents, a persistent statusline.
- **Codex**: concise `exec … succeeded (12ms)` lines — timing on every
  action; this is what the early cycle audits praised as "minimal noise,
  always accountable".
- **Kitsu**: `◆ Read(…)` / `  ◇ 128 lines` receipts are compact and were
  rated learnable-in-one-session; `command_exec` results carry `N ms` but
  fs/grep/agent tools carry **no duration**; the `↳ N input · M output ·
  context` footer has **no wall-clock time**; heartbeats back off correctly
  (15/30/60/120s) but the agent-join heartbeat prints as an untinted bare
  `eprintln!` (`crates/kitsu/src/main.rs:7596-7614`); `◆ Read(…/src/lib.rs)`
  elides the crate segment (G5, carried since cycle 10).

**Verdict: structurally at parity, behind both baselines on timing
visibility; G5 is a long-standing honesty wart.**

### Color, layout, graphics

- **Claude Code**: themed truecolor UI, boxed panels, todo trees.
- **Codex**: nearly colorless by design; header block + plain text.
- **Kitsu**: six truecolor themes live-switchable via `/theme`, an owned
  glyph vocabulary (◆ ◇ ◉ ◈ ⛩), a closed-box banner with the fox mascot, a
  tinted 16-cell context meter, and a disciplined 3-tier degradation
  (styled TTY → non-TTY unicode → `--plain` ASCII) that neither baseline
  matches. Observed gaps: a long directory path overflows the banner box
  (right bar dropped — the PTY run showed exactly this); the `/theme` list
  is plain text with no visual preview of what each theme looks like.

**Verdict: kitsu's theming/degradation architecture is *ahead* of both
baselines; polish gaps are small and local.**

### Interaction ergonomics

- **Claude Code**: rich slash surface, `--resume` picker, keybindings,
  statusline customization.
- **Codex**: exec-first ergonomics — `-o` final-message capture,
  `--output-schema`.
- **Kitsu**: broad slash registry with grouped `/help`, `/help <command>`,
  `/theme`, `/cost`, `/loop`, and (cycle 22) an interactive `/resume`
  picker. Remaining known gaps are tracked in the cycle queue (in-session
  `/doctor`, `-o` final-only capture) and are functionality, not rendering —
  they stay out of this queue.

**Verdict: near parity; nothing queue-worthy here that isn't already in the
dogfood-loop backlog.**

---

## Implementation queue

Rules for implementers: each item is independently shippable in one session.
Take the **topmost unchecked** item only. Respect the three output tiers
(styled TTY / non-TTY unicode / `--plain` ASCII, see `docs/ux-design.md`):
every new decoration must degrade, and sanitize-round-trip invariants
(`sanitize_terminal_text(styled) == plain_text`) must hold. Update
`docs/ux-design.md` when you change a surface it documents.

- [x] **1. G5: stop eliding the crate segment in tool receipt paths** (S) — done 2026-08-12
  `short_path` (`crates/kitsu-core/src/orchestrator.rs:1999`) keeps only the
  last two path segments, so `crates/kitsu-render/src/lib.rs` renders as
  `…/src/lib.rs` — every `src/lib.rs` in a workspace looks identical.
  Keep the last **three** segments (`…/kitsu-render/src/lib.rs`), preserving
  the existing exact-short-absolute-path behavior (`/etc/hostname` stays
  exact) and relative-path behavior.
  *Files*: `orchestrator.rs` `short_path` + `format_tool_header`; tests in
  `crates/kitsu-core/src/orchestrator_tests.rs` (`tool_activity_formatting`,
  line ~1241).
  *Acceptance*: unit tests assert `short_path("/a/b/c/d/e.rs") ==
  "…/c/d/e.rs"`, `short_path("/etc/hostname") == "/etc/hostname"`,
  `short_path("a/b") == "a/b"`; `cargo test -p kitsu-core` green.

- [ ] **2. Wall-clock duration in the turn stats footer** (S)
  Codex ends every run with a timing line; Claude Code shows elapsed in its
  spinner. Kitsu's `↳ 1,311 input · 5 output · … context` footer has no
  time. Thread the turn's elapsed `Duration` into
  `emit_turn_stats`/`human_turn_stats`
  (`crates/kitsu-render/src/lib.rs:1278-1375`) as a new leading part:
  `↳ 4.2s · 1,311 input · …` (format: `<1s` → `0.9s`, `<60s` → `4.2s`, else
  reuse `format_wait_elapsed`). The turn start `Instant` exists in the
  orchestrator (`crates/kitsu-core/src/orchestrator.rs`, `started` field
  ~line 366); pass elapsed through the existing call sites. Non-TTY
  `kitsu-usage:`/`kitsu-context:` records must stay **byte-identical**
  (automation contract) — add no new machine record.
  *Acceptance*: extend `combines_usage_and_context_into_one_status_line` and
  `telemetry_uses_the_explicit_destination_tty_state` (footer contains the
  duration part on TTY; redirected output unchanged); `cargo test -p
  kitsu-render -p kitsu-core` green.

- [ ] **3. Durations on tool result receipts** (S)
  `command_exec` results already show `N ms`; `Read`/`List`/`Grep`/`Write`/
  `Edit` results show none. In the tool-dispatch loop
  (`crates/kitsu-core/src/orchestrator.rs:1330-1360`), capture
  `Instant::now()` around `tools.run(...)` and append `· 340ms` (or `· 2.1s`
  past 1s) to the summary passed to `kitsu_render::tool_result` — e.g.
  `◇ 128 lines · 34ms`. Skip appending for `command_exec` (already carries
  its own elapsed). Plain mode inherits the text via the existing `result>`
  path (`·` maps to `|` automatically).
  *Acceptance*: unit test on the new duration-formatting helper; a scripted
  run (`chat --script` with an `--allow-path` read task, or the existing
  orchestrator tests' fake tools) shows `result>`/`◇` lines ending in a
  duration; `cargo test` workspace green.

- [ ] **4. Inline markdown styling: bold, inline code, list bullets** (S)
  `format_message` (`crates/kitsu-render/src/lib.rs:306-344`) styles only
  `#`-headings and diff fences. Add, outside code fences: `**bold**` spans
  rendered bold, `` `inline code` `` spans tinted with `theme().highlight`,
  and list-marker lines (`- `, `* `, `1. ` after optional indent) with the
  marker itself dimmed. **Markers must be retained** (style the characters,
  do not remove them) so the invariant
  `sanitize_terminal_text(format_message(s, true)) == s` — already asserted
  by `buffered_markdown_retains_streaming_structure` — keeps holding.
  Unclosed markers pass through unstyled.
  *Acceptance*: extend that test with bold/inline-code/list input; add a
  case proving an unclosed `**` is left untouched; `cargo test -p
  kitsu-render` green.

- [ ] **5. Theme-aware agent-join heartbeats** (S)
  The join-wait heartbeat is a bare `eprintln!("~ still waiting …")`
  (`crates/kitsu/src/main.rs:7596-7614`), so on a styled TTY it is the only
  untinted `~` line — every other info notice goes teal via
  `PromptSafeAsyncNotice`. Route the heartbeat text through
  `PromptSafeAsyncNotice::info(...).write_line(&mut io::stderr())` (message
  without the leading `~ `, which the notice adds), keeping the exact same
  text content. Also sweep `main.rs` for any other bare `~ ` `eprintln!`
  waiting lines and convert them identically.
  *Acceptance*: `grep -n '"~ still waiting' crates/kitsu/src/main.rs` shows
  no bare eprintln remaining; a `--script` run's plain output still shows
  `~ still waiting …` byte-compatible lines; `cargo test -p kitsu` (121 CLI
  tests, known PTY failure allowed) green.

- [ ] **6. Spinner elapsed counter and verb rotation** (S)
  Claude Code's spinner shows a verb + elapsed seconds. Extend the animated
  spinner loop (`crates/kitsu-render/src/lib.rs`, `Spinner::start`,
  ~line 914-940): after 5s of waiting, append a dimmed ` (Ns)` elapsed
  suffix; rotate the phrase through a small fox-flavored but professional
  verb set every ~4s — e.g. "thinking", "sniffing the trail", "foraging",
  "circling back" — first phrase always "thinking". Static/plain/
  reduced-motion/heartbeat modes are **unchanged** (they already have their
  own contract).
  *Acceptance*: verbs live in a `const SPINNER_VERBS: [&str; N]`; a unit
  test asserts each is lowercase, ASCII-safe, ≤ 20 chars, and the set
  contains "thinking" first; `spinner_frames_are_fox_kaomoji_not_cat` still
  passes; `cargo test -p kitsu-render` green.

- [ ] **7. `/theme` list with live palette swatches** (S)
  The `/theme` listing is plain text. On a styled TTY, render each theme row
  with its name tinted in that theme's accent and a six-dot swatch —
  `● ● ● ● ● ●` in accent/warn/highlight/info/success/error truecolor —
  after the description. Non-TTY and `--plain` keep the current byte-exact
  text (no dots). Add a `theme_row(theme, active, styled, plain) -> String`
  helper in `kitsu-render` next to `THEMES` (`lib.rs:385`) and use it from
  the `/theme` handler in `crates/kitsu/src/main.rs` (grep `themes (active
  marked`).
  *Acceptance*: unit test — styled row contains the SGR for that theme's
  accent and `sanitize_terminal_text` of it contains the name+description;
  plain row equals today's text; scripted `/theme` output unchanged;
  workspace tests green.

- [ ] **8. Banner: elide the directory row and keep the box closed** (S)
  Observed live: a long cwd overflows `BANNER_INNER_WIDTH` (56) and the row
  drops its right bar, breaking the box. In `chat_banner`
  (`crates/kitsu-render/src/lib.rs:516-605`), when a **value** would
  overflow, elide it *middle-out with a leading `…`* keeping the tail (the
  informative part of a path) so the row fits and the right bar returns;
  apply `~`-for-home shortening to the directory value at the call site
  (`crates/kitsu/src/main.rs:10992`, reuse `shorten_home`, `main.rs:13591`).
  Never elide the gateway/model rows' *labels*; tools row already
  self-compacts.
  *Acceptance*: unit test a new `fit_banner_value(value, width)` helper
  (keeps short values exact; long values start with `…` and have
  `display_width ≤ width`); PTY smoke: `script -qec "…kitsu … chat
  'hi'" /dev/null` from a deep cwd shows every banner row ending in `│`;
  render tests green.

- [ ] **9. Styled streaming: line-buffered markdown on the live path** (M)
  The streamed path prints raw text; only buffered replies get styling
  (assessment: biggest parity gap). Give `kitsu-render` a stateful streaming
  renderer: `print_fragment` (`lib.rs:277`) accumulates into an internal
  line buffer (per the existing global-sanitizer pattern, `lib.rs:185`),
  emits each **completed** line through the same per-line styling rules as
  `format_message` (share the fence-state machine — extract it from
  `format_message` into a `LineStyler` struct), and flushes any partial line
  unstyled on `finish_line` (`lib.rs:986`), which also resets fence state.
  Only when stdout is a styled TTY — non-TTY/plain streaming must remain
  byte-identical to today (no buffering side effects beyond newline
  timing). Note the tradeoff in the doc comment: styled TTY output becomes
  line-granular instead of token-granular.
  *Acceptance*: new test — feeding `"```di"`, `"ff\n+add"`, `"ed\n```\n"`
  through the streaming renderer (styled) yields green `+added`;
  `sanitize_terminal_text` of the total output equals the input; existing
  streaming-sanitizer tests still pass; workspace green.

- [ ] **10. Syntax-highlighted code blocks (syntect) on the buffered path** (M)
  The standing backlog item since cycle 6. Add `syntect` (default features
  minus onig — use `fancy-regex` feature set to keep the build light) to
  `crates/kitsu-render/Cargo.toml`. In the fence branch of the line styler
  (see item 9; if item 9 is unshipped, do it inside `format_message`), when
  a fence declares a known language (`rust`, `py`/`python`, `js`, `sh`/
  `bash`, `toml`, `json`, …) highlight interior lines with
  syntect-to-truecolor; unknown language or `diff`/`patch` keep current
  behavior. Cache `SyntaxSet`/`Theme` in a `OnceLock`. `--plain`, non-TTY,
  and the sanitize round-trip invariant are unaffected (highlighting adds
  SGR only, never changes text).
  *Acceptance*: test — a ```rust fence with `fn main() {}` renders with ≥ 2
  distinct SGR color sequences when styled and exact original text when
  sanitized or unstyled; `cargo build` time impact noted in the commit
  message; workspace green.

- [ ] **11. Diff header and per-file stat line in `/diff` output** (S)
  `render_diff` (`crates/kitsu-render/src/lib.rs:1120-1151`) prints a bare
  `verified workspace diff (N line(s)):` count. Upgrade the header to the
  receipt form used elsewhere: `◈ verified workspace diff — N file(s), +A
  −R line(s)` (count `diff --git` occurrences and +/− lines, excluding
  `+++`/`---`), tinted with `theme().highlight` bold on a styled TTY, `#`
  prefix in plain mode (match `deck_header` behavior, `lib.rs:1231`). Keep
  the existing 240-line TTY cap and its trailer.
  *Acceptance*: unit test a `diff_summary(diff) -> (files, adds, dels)`
  helper on a two-file fixture diff; header renders in all three tiers with
  the sanitize round-trip holding; render tests green.

- [ ] **12. Tool-call header polish: accent verb, dim arguments** (S)
  `◆ Read(crates/kitsu-render/src/lib.rs)` currently tints only the `◆`;
  Claude Code's cards read faster because the verb and detail have distinct
  weights. In `tool_activity` (`crates/kitsu-render/src/lib.rs:1001-1016`),
  on a styled TTY render the tool verb (text before the first `(`) in
  `theme().accent` **bold** and the parenthesized argument text dimmed;
  headers without `(` render whole in accent. Plain/non-TTY output is
  byte-identical to today.
  *Acceptance*: refactor to a testable `tool_activity_line(header, styled,
  plain) -> String`; tests assert the styled form contains bold+accent SGR
  before `(`, dim after, and sanitizes back to `◆ Read(x)`; plain form still
  `tool> Read(x)`; workspace green.

- [ ] **13. Agent deck: last-activity progress rows for running agents** (M)
  Claude Code shows a progress tree for subagents; kitsu's `/jobs` deck rows
  are single lines. For each **Running** deck row, render one indented
  dimmed child row `  ↳ <last tool receipt or "no activity yet"> · <age>`
  using the most recent tool header the background agent executed. Find the
  deck row rendering via `deck_marker`/`deck_header` call sites in
  `crates/kitsu/src/main.rs` (grep `agent deck`) and the background-agent
  state in `crates/kitsu/src/subagent.rs` — if the last-tool-header is not
  yet tracked, add a `last_activity: Option<(String, Instant)>` updated from
  the existing per-agent tool event stream. Plain mode: `  -> …` (the `↳`
  already maps via `ascii_structural_markers`).
  *Acceptance*: unit test the row formatter; live check —
  `/jobs` during a running `spawn_agent_bg` shows the `↳` child line and
  finished agents show none; `cargo test -p kitsu` green (known PTY failure
  allowed).

## Protocol

For implementation agents executing this queue:

1. **Take the topmost unchecked item.** Do not skip ahead, cherry-pick, or
   batch: **one item per session/tick**, even if the next one looks easy.
2. Implement exactly the item's spec. If the listed line numbers have
   drifted, locate the code by the named function/string instead; if the
   spec has become impossible (surface removed/renamed), check the item off
   with a dated note `[x] 2026-MM-DD — obsolete: <why>` and stop (do not
   start the next item).
3. Run the item's acceptance test **and** the standing gates, all of which
   must stay green:
   - `cargo fmt --all -- --check` (clean)
   - `cargo clippy --workspace --all-targets` (zero warnings)
   - `cargo test --workspace` — green, with the one known PTY-environmental
     CLI test failure allowed (it predates this plan; anything else red is
     yours to fix before checking off).
4. Respect the invariants: three-tier degradation (`docs/ux-design.md`),
   sanitize round-trip (`sanitize_terminal_text(styled) == plain text`),
   stable machine records (`kitsu-usage:`, `kitsu-context:`, `result>`,
   `tool>`, JSONL `kitsu.chat.v1`) — automation contracts never change as a
   side effect of styling.
5. Update `docs/ux-design.md` if the item changes a documented surface, then
   edit **this file only** to mark the item `[x]` with the date (e.g.
   `- [x] (2026-08-13) **1. G5: …**`), and commit the item as one change.
