# Kitsu TUI and agent view — design and requirements

- **Status:** Draft v2.1 (revised after reviews `reviews/tui-design-review-1.md`
  and `reviews/tui-plan-review-1.md`)
- **Date:** 2026-10-12
- **Branch:** `features/tui_herdr_improvements`
- **Builds on:** `docs/agent_deck_ui.md`, `docs/subagents-design.md`,
  `docs/ux-design.md`, `docs/ui-parity-plan.md`, `kitsu-render::live`,
  `crates/kitsu/src/subagent.rs` (`AgentRegistry`, thread timelines, steering),
  `crates/kitsu-tools/src/permissions.rs` and
  `crates/kitsu-tools/src/builtins/command.rs` (the two approval gates).
- **Supersedes:** the `docs/claude-code-comparison.md` §8 non-goal "Custom TUI /
  Ink-style renderer … don't trade it for an alt-screen". The reason behind
  that decision (native scrollback and copy-paste) is preserved here: the
  default renderer keeps the transcript in native scrollback; the alternate
  screen is used only for overlays, and fullscreen is opt-in.
- **Research inputs:** Claude Code interactive-mode, fullscreen-rendering, and
  agent-view docs; Codex CLI TUI (ratatui, inline viewport, `insert_history`);
  herdr (agent multiplexer: sidebar states, prefix keys, socket API,
  detach/attach). Sources are listed in §9.

Kitsu's console today is a line-oriented REPL: rustyline at the idle prompt, a
small live region during a turn (status row plus up to three queued-preview
rows), and background agents visible only as scrolling `/jobs` receipts. Claude
Code and Codex both run a real terminal UI with a pinned composer, overlays,
and a transcript view; Claude Code adds an agent view for switching between
sessions; herdr adds a sidebar that knows which agent is working, blocked, or
done. This document analyzes the four tools, picks the parts worth adopting,
and specifies the requirements and a staged design for bringing them to kitsu
without breaking its automation and degradation invariants.

---

## 1. Problem statement

1. **Layout.** All output scrolls through one stream. The composer moves with
   the output, notices splice between lines, and a long tool result pushes the
   prompt off screen. There is no stable place for status, no way to scroll
   the transcript inside the app, and no overlay for pickers or diffs.
2. **Agent switching.** Background agents (`spawn_agent_bg`) are first-class in
   the registry (status, telemetry, timeline, steering, handoff) but second
   class on screen: the user runs `/jobs`, reads a deck, runs `/jobs show N`,
   then `/jobs steer N …`. There is no way to "go look at agent 3", watch it
   live, answer it, and come back.
3. **Blocked agents cannot ask.** Detached agents run under
   `scope_without_interactive_prompts`, so both approval gates
   (`Permissions::resolve` for paths, the `CommandMode::Ask` branch in
   `command.rs` for shell commands) return `PolicyDenied` instead of prompting
   (`docs/functionality.md` §permissions). The user has no "needs input" state
   to react to. herdr and Claude Code both treat *blocked* as the most
   important state in the whole UI.
4. **Idle notices are deferred.** With no rustyline `ExternalPrinter` by
   default (input-latency regression, `main.rs` ~16401), a finished background
   agent is reported only at the next prompt cycle unless
   `KITSU_LIVE_NOTICES=1`. A TUI with its own event loop removes this trade-off.
5. **Herding across tools.** Users already run kitsu next to Claude Code and
   Codex under tmux or herdr. Kitsu emits nothing a multiplexer can read, so in
   herdr it classifies as `unknown`.

### Goal

Give kitsu a real TUI with a pinned composer and native-scrollback transcript,
an **agent view** for dispatching, watching, steering, attaching to, and
answering background agents, and the hooks that let an outer herder (herdr,
tmux, Claude Code agent view) see kitsu's state. Keep every current
non-interactive contract byte-identical.

### Non-goals (this design)

- Replacing tmux/herdr. Kitsu herds **its own** agents; it does not host
  foreign processes in panes.
- Multi-machine or remote attach; a local socket API (open question 6).
- Mouse-first interaction (mouse is optional polish in Stage 4).
- Breaking changes to the JSONL protocol `kitsu.chat.v1`. Additive optional
  events are allowed by the protocol but none is required by this design.
- A subagent "ask the user a free-form question" tool (open question 5). The
  subagent contract today says "Do not ask questions"; Stage 1 covers
  *permission* questions only.

---

## 2. Comparative analysis

### 2.1 Kitsu today

| Surface | Implementation | Assessment |
|---|---|---|
| Idle input | rustyline 14, `ChatHelper` (completer for `@file` via `attachments::complete` and the slash palette, hinter, highlighter) in `main.rs` | Solid editing; no live notices by default; cannot draw outside the prompt line. |
| During-turn input | raw-mode `TypeaheadEditor` + `kitsu-render::live` region (spinner, draft, `[N queued]`, up to three queued-preview rows) | Already a mini TUI: cursor-addressed region, suspend/repaint around transcript writes, width-aware windowing. This is the seed of the composer. Mid-turn Tab only accepts the inline slash hint; `↑` on an empty draft recalls the newest queued prompt. |
| Transcript | assistant text on **stdout**, all chrome (receipts, notices, live row) on **stderr**; `LineStyler` (markdown, syntect, diff) | Good styling; lives in native scrollback, which we want to keep. The fd split is a contract (`kitsu chat 2>/dev/null` yields answers only). |
| Background agents | `AgentRegistry` with `watch` snapshots (`generation/running/terminal/failed`), per-agent `AgentTelemetry`, bounded redacted `AgentTimeline`, steering `mpsc`, handoff states (`Unavailable/Ready/Acknowledged`), profiles | The data model for an agent view mostly exists. Missing: a *blocked* phase, an answer channel back to the agent, a live timeline tail, blocked/unacknowledged counts in the watch snapshot. |
| Permissions | one shared `Permissions`; `Mode::Ask` prompts through a sync `Prompter::ask` bridged with `spawn_blocking`; a tokio `task_local!` bool (`INTERACTIVE_PROMPTS_ALLOWED`) makes detached agents fail closed in both gates | Any "blocked agent" design must plug in at these two gates, not in tool handlers. |
| Agent UI | `/jobs [list\|show\|steer\|close-steering\|stop\|stop-all\|prune]`, `/agents` (alias of `/jobs`), `/agent [list\|show\|steer\|stop\|close]` (one thread), `/profiles`; inline `◈ agent deck —` receipts | Honest and scriptable but passive: the user polls. |
| Degradation | styled TTY / non-TTY unicode / `--plain` ASCII; `--reduced-motion` keeps styling but freezes animation; assistive mode; JSONL owns stdout | Stronger than either baseline. Must survive the TUI unchanged. |
| Persistence | `SavedSession { messages, model, name, project_context, … }` | Messages only; tool receipts, notices and deck lines are not stored. A fullscreen transcript cannot be rebuilt from disk. |
| Code shape | `crates/kitsu/src/main.rs` is 23.7k lines and owns the REPL, slash registry, agent deck rendering | A TUI cannot be added there; it needs its own crate. |

### 2.2 Claude Code

Two renderers. **Classic** keeps the conversation in native scrollback with an
inline composer. **Fullscreen** (`/tui fullscreen`) draws on the alternate
screen, keeps only visible messages in the render tree, adds mouse support, a
transcript mode (`Ctrl+O`) with `less`-style search, `[` to dump the
conversation into native scrollback, `v` to open it in `$EDITOR`, and a diff
panel beside the conversation. Users switch renderers mid-session.

**Agent view** (`claude agents`) is a separate screen listing background
sessions grouped by state (*Pinned, Ready for review, Needs input, Working,
Completed*), with a dispatch composer at the bottom, a **peek panel** (`Space`)
to read and reply without attaching, `Enter` to attach, `←` on an empty prompt
to detach back, filters (`s:blocked a:reviewer n:login`), rename/pin/stop, and
an ambient `← 2 agents` footer counter inside a normal session. A supervisor
daemon keeps sessions alive across detach; dispatched sessions get their own
git worktree by default. JSON listing via `claude agents --json`. Chords use
the readline prefix `Ctrl+X` (`Ctrl+X Ctrl+K` stop subagents, `Ctrl+X Ctrl+E`
editor); `Ctrl+B` backgrounds a task and tmux users press it twice.

**Take:** state grouping with *Needs input* on top, peek-and-reply, attach/detach
with a recap on return, ambient awaiting-input counter, filter grammar,
"dump to native scrollback" escape hatch, renderer switchable at runtime,
`Ctrl+X` as chord prefix.
**Leave:** daemon/supervisor and worktree isolation (later; kitsu's agents are
in-process tasks), mouse-first features, PR tracking.

### 2.3 Codex CLI

ratatui + crossterm, a **custom Terminal fork with an inline viewport**: the
composer and status live in a small bottom viewport; finished history cells are
written *above* the viewport with DEC scroll regions (`insert_history.rs`) so
they land in real terminal scrollback and native search/select keep working.
Alternate screen is used only for overlays (transcript, diff) and is
configurable (`tui.alternate_screen = auto|always|never`, `--no-alt-screen`).
Synchronized-update atomic draws, coalesced frame scheduler. Approval flows
are keyboard-driven modals in the bottom pane. Terse `exec … succeeded (12ms)`
accounting.

**Take:** inline viewport + native scrollback as the default (it preserves
kitsu's "transcript is just text" contract and plays well in tmux/herdr),
alternate screen for overlays only, synchronized output, alt-screen
configurable per terminal. Approval as a bottom-pane modal rather than a
spliced prompt.
**Leave:** nothing structural; Codex has no multi-agent view to borrow.

### 2.4 herdr

A Rust agent multiplexer, "tmux rebuilt around agents". Sidebar lists
workspaces, tabs, panes, and detected agents with five states: `working`,
`blocked`, `done` (finished and not yet viewed), `idle` (viewed), `unknown`.
State comes from screen-manifest matching at the bottom of the pane, native
integration hooks (Codex, Claude), and foreground-process detection. Prefix
keybindings (`ctrl+b` then `v` split, `c` tab, `n`/`p`, `z` zoom, `q` detach,
`?` help). Detach keeps a server running; reattach is exact. A local socket
API and CLI (`herdr agent start|prompt --wait|wait --until blocked|read`) let
one agent drive another. Plugins subscribe to lifecycle events. Toast
notifications. Worktree automation. Known limits: no shared memory between
agents, no approval workflow, no persistent task graph.

**Take:** the five-state model including *done-unviewed vs idle* (kitsu already
has `handoff: Ready` vs `Acknowledged`, which maps exactly), a state list that
is one chord away plus an always-visible counter, a prefix-key model so
bindings do not collide with the editor, a `wait --until` primitive for
agent-to-agent coordination, `agent explain` (why is it in this state), and
**native integration**: kitsu should advertise its own state so herdr
classifies it without screen scraping.
**Leave:** hosting arbitrary processes, remote machines, a persistent sidebar
pane, worktrees (defer), the socket API (open question 6).

### 2.5 Summary of adopted parts

| Part | From | Kitsu mapping |
|---|---|---|
| Inline viewport, history into native scrollback | Codex | `kitsu-tui` renderer; transcript stays plain text; stdout/stderr split kept |
| Alt screen only for overlays, configurable | Codex, Claude fullscreen | `tui.alternate_screen = auto\|always\|never`, `--no-alt-screen` |
| Transcript mode with search, `[` dump to scrollback | Claude fullscreen | overlay over the in-memory cell ring (§4.2) |
| Agent view grouped by state, *Needs input* first | Claude agent view | overlay backed by `AgentRegistry` snapshots |
| Peek and reply without attaching | Claude agent view | `Space` on a row; reply routes to `steer` or the approval answer channel |
| Attach / detach with recap | Claude agent view | `Enter`/`←`; recap from the agent's bounded timeline |
| Ambient `← N agents` counter | Claude | status row segment, also in classic mode |
| Five agent states incl. done-unviewed vs idle | herdr | `Running/Blocked/Done+Ready/Done+Acknowledged/Failed/Cancelled` |
| Prefix-key chords, `?` help | herdr, Claude | `Ctrl+X` prefix (configurable); unbound in `TypeaheadEditor` today |
| `wait --until`, `explain` | herdr | `agent_wait` tool, `/jobs explain N` |
| Native state advertisement | herdr integrations | OSC title, per-process state file, `kitsu agents --json` reading those files, state hook |
| Filter grammar `s: n: o: p:` | Claude | agent view filter line |
| Bottom-pane approval modal | Codex | replaces spliced `◉ permission` prompt in TUI mode |
| Renderer switch at runtime | Claude | `/tui classic\|inline\|fullscreen` |

---

## 3. Requirements

Numbered so stages and tests can cite them. **MUST/SHOULD/MAY** as usual.

### 3.1 Functional — renderer (R)

- **R1** Kitsu MUST offer three renderers: `classic` (today's REPL),
  `inline` (pinned composer, history in native scrollback), `fullscreen`
  (alternate screen, self-scrolled transcript). `inline` becomes the default
  on a styled interactive TTY one release after Stage 2 ships; `classic`
  remains available forever.
- **R2** `/tui [classic|inline|fullscreen]` MUST switch at runtime without
  losing the conversation; `--tui <mode>` and `[tui] mode` in `kitsu.toml`
  set the start mode.
- **R3** In `inline` mode every finished transcript cell MUST be written to
  native scrollback exactly once, on the **same fd** `classic` uses (assistant
  text → stdout, receipts/notices → stderr), as the same text, so per fd
  `sanitize_terminal_text(inline output) == classic plain output`. All cursor
  control (scroll regions, synchronized updates, cursor moves) goes to stderr,
  as `kitsu-render::live` does today.
- **R4** The composer MUST keep every current editing feature: history,
  Ctrl+R search, slash hint and Tab completion, the `@file` and slash-palette
  completion lists, hint and highlight now hosted by rustyline's `ChatHelper`,
  **[Status, T0-S16] R4 is met apart from two named items.** Met: history
  (including across sessions — it reuses rustyline's own persistence), Ctrl+R
  search with its status indicator, the slash hint, **Tab accepting that
  hint**, and the **bordered slash-command palette**, which shares the classic
  prompt's own renderer rather than reimplementing it. Still absent: **`@file`
  path completion** and **highlighting** of the draft. Neither needs more
  viewport machinery — the palette's arrival settled the geometry questions
  (F16's resize and truncation halves, F19's minimum height) — so both are
  ordinary features rather than blockers. None of this blocks the opt-in
  preview; what still gates making it the default is Q1, the live terminal
  matrix.
  bracketed paste as one draft, Alt+Enter newline, Esc clears draft,
  draft-aware Ctrl+C, `↑`-on-empty queue recall, queue preview rows,
  `[N queued]`, approval handoff.
- **R5** A persistent **status row** MUST show: model, context meter, elapsed
  turn time, `[N queued]`, and an ambient background-agent tag carrying the
  count of agents needing input, else the running count. It is one line and
  never scrolls. **Tag form:** `classic` renders it bracketed beside its
  neighbour (`[1 blocked] [2 queued] kitsu> …`, blocked outranking running,
  hidden when both are zero), because the `←` glyph Claude Code uses is a
  live hint for the left-arrow key and classic has no such binding; a TUI
  mode MAY adopt `← N agents` once A1's left-arrow affordance exists, and
  not before. Promising a key that does nothing is the kind of dishonest
  decoration the rest of this design avoids.

  **Drop order, added in T3-S5 because R5 did not state one.** When the row
  cannot fit, segments are dropped in this order, first to go listed first:
  **context meter, elapsed, model, `[N queued]`, ambient tag.** The model is
  **truncated before it is dropped**. The reasoning, so an edge case can be
  judged rather than guessed: decoration yields to content, as the spinner
  already yields to the draft; the meter goes first because `/context` renders
  it on demand; elapsed follows because the spinner itself carries elapsed past
  five seconds; the model is long and static; and the ambient tag is last
  standing because a blocked agent is the one thing in the row the user must
  act on.

  This order was derived from the classic row's precedent in T3-S5 and labelled
  an inference at the time rather than passed off as a requirement. It is stated
  here because a drop order that lives only in a test's doc comment is at the
  wrong altitude for something every implementer of this row must honour.
- **R6** Transcript overlay (`Ctrl+O`): scroll, `/` search, `n`/`N`, `{`/`}`
  prompt jumps, `[` dump the cell ring to native scrollback (fullscreen only),
  `v` open in `$EDITOR`, `q`/`Esc` close.
- **R7** Approval prompts in TUI modes MUST render as a bottom-pane modal
  with the same `[y] allow once · [a] always (<dir>) · [n] deny` option line
  (command approvals keep their provenance rows) and the same `◉ permission —`
  text in the transcript receipt afterwards.

  **Clarified in T3-S7a, after implementing it.** The "receipt afterwards" is
  **new behaviour in the viewport only, and that is not a parity break.** In
  `classic` the prompt *is* the transcript line: it is printed into the scroll
  region and stays there, so the record already exists. The pane modal is
  **transient**, so the viewport must author a receipt to leave the same trace
  classic leaves by accident of where it prints. Classic's bytes are therefore
  unchanged, and a test pins that it emits exactly one `◉ permission —` line per
  approval.

  Ordering matters and is easy to get wrong: **the receipt must be written
  before the modal is cleared.** A transcript line is text plus CRLF with no
  addressing, so it lands at the parked cursor; tearing the modal down first
  re-parks the cursor and the receipt is glued to the status row instead of
  following the newline the kernel echoed with the answer.

  **Where the answer is read, added in T3-S7b.** Where a pane is published the
  answer is read **in-loop**, on the keypress, while the input collector still
  holds its raw-mode guard — so the kernel echoes nothing and the answer never
  reaches the transcript — and the `> ` cue is painted as a row of the modal.
  Where no pane is published the read stays **canonical with echo**, which
  classic and `--plain` rely on for line editing, backspace and the `yes` /
  `always` / `no` long forms. That asymmetry is a **stated contract**, not an
  implementation detail: a test pins that classic still echoes the answer into
  its transcript, which is what catches a bypass that reaches too far.

  Two consequences of the in-loop read, both deliberate: the long forms are
  **unreachable by keypress** in the viewport (they remain available in classic
  and in `/jobs reply`, which share the one parser), and a **bracketed paste**
  at a viewport approval begins with `ESC`, which the parser rejects — so a
  pasted answer is **denied** rather than accepted. Failing closed is the right
  direction for a permission prompt, but it is a behaviour difference worth
  knowing.

  **`--plain` is excluded.** The plain prompter has its own unstyled text
  (`permission> …`) and `--plain` is a **byte-contract tier**, so routing it
  through the shared modal composer would change bytes that other requirements
  promise are stable. `--plain --tui inline` is reachable today — the viewport
  installs on `mode` alone, not on styling — and in that combination the prompt
  stays on stderr and **R7 is not satisfied**. See **F53**: the better fix is
  probably for `--plain` to resolve to `classic` rather than for the plain
  prompter to grow a modal, but that is a mode-resolution decision and not R7's
  to make.
- **R8** `Ctrl+L` MUST repaint. Resize MUST reflow the viewport without
  duplicating history lines.

### 3.2 Functional — agent view (A)

- **A1** `Ctrl+X a` or `/jobs view` MUST open the agent view overlay (the
  `/agents` alias arm, which today forwards only an empty or numeric
  argument, is widened to forward `view|explain|reply`; mid-turn `/jobs` has
  its own handler, `handle_active_jobs_command`, which gains the same
  subcommands); `←` on an empty composer MUST open
  it too (first press shows a hint, second opens), configurable off. `←` is
  distinct from the existing `↑`-on-empty queue recall and never consumes a
  draft.
- **A2** Rows MUST be grouped by state in this order: *Needs input*, *Working*,
  *Done (unviewed)*, *Done (viewed)*, *Failed*, *Cancelled*. Each row: state
  label, id, name or task head, model/profile, last activity + age, tokens.
- **A3** Keys: `↑/↓` move, `Enter`/`→` attach, `Space` peek, `s` steer
  (prompt for text), `r` reply (only when blocked), `x` stop (twice to prune),
  `n` rename, `p` mark result acknowledged, `/` filter, `?` help, `Esc`
  close. Filter grammar: `s:<state> n:<text> o:<text> p:<profile>`.
- **A4** Peek panel MUST show the bounded redacted timeline that `/jobs show`
  shows today, the pending approval request if blocked, and the result if
  done. It MUST never show tool arguments or raw tool output (existing
  redaction rule).
- **A5** Attach MUST stream the agent's timeline live into the main viewport
  with an `⛩ attached to agent N` milestone, let the composer send steering
  (or an approval answer when blocked), and `←` on an empty composer or
  `/detach` return with a recap of events that happened while attached.
  Attaching never pauses the foreground turn; the foreground turn's output is
  buffered and replayed on detach.
- **A6** Dispatch from the agent view composer: plain text spawns a background
  agent with the default profile; `@<profile> <task>` selects a profile.
  Dispatch is admitted by the registry limits (`AgentRegistryLimits`,
  default 8 running / 128 records), **not** by the model's per-turn
  `SpawnBudget`, which exists to stop storming models.
- **A7** Viewing a done agent (peek or attach) MUST transition its handoff to
  `Acknowledged`, exactly as `/jobs show` does (`peek_result` +
  `acknowledge_result`), so the exit guard and strict-automation rules are
  unchanged.
- **A8** The ambient counter (R5) MUST update within 250 ms of a registry
  change in TUI modes. In `classic` it refreshes on the live status row once
  per input poll tick while a turn runs, and the idle prompt keeps reporting
  at the next prompt cycle, which is the pre-existing notice behavior.

### 3.3 Functional — blocked agents (B)

- **B1** A detached agent whose action needs a **permission** (path or
  command) MUST enter a new `Blocked { request }` phase instead of failing
  closed, when `agents.blocking = ask` (default in interactive TUI modes). In
  `classic`, piped, scripted, and JSONL runs the default stays `fail_closed`.
  Free-form questions from the model are out of scope (non-goals).
  **[Amendment, T2-S5a] In scripted, piped and JSONL runs this is stronger than
  a default: asking is structurally impossible.** `main.rs` computes
  `interactive = stdin_is_tty() && !scripted && !forced_one_shot`, and a
  non-interactive run gets `Mode::Deny`, whose arm in `Permissions::resolve`
  returns `self.denied(..)` **before** `prompt_route()` is ever consulted. So no
  value of `agents.blocking` can make those runs park an agent — the route is
  not read on that path at all. That makes B4's "JSONL output is unchanged" and
  N1 enforced by structure rather than by configuration discipline, and it is
  why T2's end-to-end proof has to run on a pseudo-terminal rather than under
  `--script`.
- **B2** The request is produced by the two existing gates (`Permissions::
  resolve` and the `CommandMode::Ask` branch in `command.rs`): instead of the
  `interactive_prompts_allowed()` bool, both consult a task-local
  **prompt route** (§4.4). An answer applies `allow once / always / deny`
  through the same code paths the foreground `Prompter` uses (for `always`,
  `insert_minimal_root` into the shared runtime roots, or the exact argv/cwd
  command grant), so no escalation path is added.
- **B3** A blocked agent MUST time out to `Failed(interrupted)` after
  `agents.block_timeout` (default 30 min), mapped to
  `ToolInterruption::AuthorityInputClosed` as `Decision::Interrupt` is today,
  so headless runs cannot hang.
- **B4** Blocked state MUST surface in: status row counter, agent view group,
  `/jobs` deck and `/jobs` JSON `status`, the thread DTO `phase`, and the
  outer-herder hooks (H1–H3). JSONL output is unchanged: JSONL runs default to
  `fail_closed`, and `background_result` keeps its `status` vocabulary
  (`done|failed|cancelled|running`). If a later stage allows `ask` under
  JSONL, the phase MUST appear as a new optional event type
  (`background_blocked`), never as a new `status` value.
- **B5** Adding `Blocked` touches three vocabularies and one validator, all of
  which MUST change together: `AgentStatus` (`subagent.rs`),
  `AgentProgressStatus` (drives the deck and `/jobs` JSON),
  `AgentThreadPhase` (the DTO), and `AgentThreadSnapshotDto::validate()`,
  whose lifecycle rules become: `terminal = phase ∉ {Running, Blocked}`;
  `handoff == Unavailable` iff non-terminal; `timeline.closed == terminal`;
  `steering_open` only when non-terminal. Because a closed serde enum gains
  a value, `AGENT_THREAD_SCHEMA_VERSION` bumps to 2; `name` is added as
  `#[serde(default, skip_serializing_if = "Option::is_none")]`. The
  `debug_assert!` on every DTO in `/jobs` JSON must keep passing.

### 3.4 Functional — herding hooks (H)

- **H1** `kitsu agents --json [--all]` MUST list the background agents of
  every live kitsu process on this machine by reading the state files in H2
  (pid liveness-checked, stale files ignored). Agents are in-process tasks and
  thread DTOs are never persisted today, so the state file is the only channel
  without a socket (open question 6). Output is an array of
  `{pid, cwd, session_id, threads: [AgentThreadSnapshotDto…]}`.
- **H2** Kitsu MUST set the terminal title (OSC 0/2) to
  `kitsu · working|blocked|idle|N agents` when `tui.title = true` (default on
  TTY), so herdr/tmux can classify by title; and MUST write
  `$XDG_RUNTIME_DIR/kitsu/<pid>.json` on every registry generation change,
  containing the process state plus the redacted thread DTO array (bounded by
  the existing timeline limits), removed on clean exit.
- **H3** A `[hooks] on_agent_state = "<cmd>"` MUST run on agent state
  transitions, mirroring herdr's plugin events and Claude Code's
  `Notification` hook. Local command only (N7). Three decisions settled while
  implementing, recorded here because the original wording left them open:
  - **Exact variable names** are `KITSU_AGENT_ID`, `KITSU_AGENT_STATE` and
    `KITSU_AGENT_WAITING_FOR`, added to the inherited environment. The
    original `KITSU_AGENT_ID/STATE/WAITING_FOR` shorthand was ambiguous.
  - **No shell.** The configured string is split into argv and executed
    directly, with no expansion, so a stray metacharacter cannot become
    execution and behaviour does not depend on which shell is installed. A
    user wanting interpolation points this at a script, or writes `sh -c`
    themselves — their explicit choice rather than kitsu's default.
  - **Output discarded and never awaited.** Standard input, output and error
    are all null, because a hook writing to stderr would splice into the live
    status row and break the byte-identical guarantee N1 gives plain,
    non-TTY and assistive output. The child is reaped with a bounded
    runtime, and a spawn error, nonzero exit or timeout is dropped silently:
    a notification must never cost a turn.
  - **Cadence** follows the state file: transitions are observed at turn
    boundaries, so a hook fires once per transition, never once per
    observation. Mid-turn reactivity is the same deferred watcher card.
- **H4** Kitsu SHOULD ship a herdr integration manifest (`herdr-plugin.toml`
  or screen manifest) under `integrations/herdr/` so kitsu is detected natively.
- **H5** New tool `agent_wait(id, until: done|blocked, timeout)` MUST let a
  foreground model block on a background agent's state (herdr's
  `agent wait --until`). It spawns nothing, so it does not consume the
  `SpawnBudget`; it is bounded by `--max-rounds` and its own timeout.

### 3.5 Non-functional and invariants (N)

- **N1** Non-TTY, `--plain`/`NO_COLOR`, assistive (`--screen-reader`), and
  JSONL output MUST stay byte-identical to today. The TUI is entered only when
  `stderr_styled() && stdout is a TTY && !plain_mode() && !assistive && !jsonl`.
- **N2** Idle input latency MUST not regress: paste of 10 KiB into the
  composer completes in under 100 ms (the rustyline `ExternalPrinter` lesson).
  Measured by a benchmark, not a CI gate (see Stage 2).
- **N3** Frame budget: coalesce redraws to ≤ 60 fps; use synchronized output
  (`CSI ?2026 h/l`) when the terminal supports it; never redraw history.
- **N4** Memory in `fullscreen` MUST be bounded by a configurable **cell
  ring** (default 10 000 lines, oldest evicted), not by conversation length.
  Kitsu persists only `messages`, so the ring is the sole source for the
  transcript overlay and the `[` dump; evicted content is reachable through
  `/export`.

  **Amended in T3-S6, because the original wording overpromised.** The bound is
  in **lines, not bytes.** Ten thousand lines of unbounded length is unbounded
  memory, so "memory MUST be bounded" was true only under an assumption about
  line length that nothing enforces. A single pathological line — a model
  emitting one enormous unbroken row — still costs whatever it costs. Closing
  that would need a per-line byte clamp, which **truncates content the ring is
  the only copy of**, so it is a deliberate trade and not an oversight to be
  quietly patched. See **F52**.

  **A stored line is the surface's bytes verbatim, styling included**, with the
  stripped form derived on demand. The overlay repaints these rows, so storing
  stripped text would silently decolour it; and since the ring is the only copy,
  styling can be stripped later but never reconstructed. The `--plain` tier's
  structural-marker substitution is **not** applied at storage — it is an output
  transform, not a storage one.

  **The ring is an opt-in field on the TUI backend, not a process global**, and
  `classic` cannot construct one: the renderer crate has no dependency path to
  the viewport crate, which makes "classic pays nothing for this" a
  **compile-time** property rather than a runtime guard. The ring is therefore
  **not fed in production until R6's overlay reads it** — feeding an unread ring
  would spend up to ten thousand styled lines of memory for no benefit.
- **N5** Terminal restore MUST happen on every exit path including panics and
  `SIGINT` (raw mode off, alt screen left, bracketed paste off, title reset).

  **Amended in T3-S8, because the "known today" note understated it.** Before
  this card, `SIGINT`, `SIGTERM` and a panic left the viewport's **DECSTBM
  scroll region pinned** — worse than leaving raw mode, because the terminal
  stays visually broken until `reset`. `kill -9` remains the one path that
  cannot restore, by definition. The restore now runs from a signal handler
  that performs **one `write(2)` of a precomposed `const`** plus a `tcsetattr`
  of a snapshot captured when raw mode was entered — both async-signal-safe —
  and then puts the default disposition back and re-raises, so the process dies
  **by the signal** rather than by an exit code this module invented.

  **The handler must not terminate, and that is a hard constraint, not a
  style.** In raw mode Ctrl+C reaches the editor as a byte and the during-turn
  collector **re-raises `SIGINT` in-process** so the turn is cancelled; tokio's
  `ctrl_c()` goes through a registry whose handler **chains to the previously
  installed one**, so a restore-and-exit handler would be invoked on every
  in-turn cancellation and kill the session mid-cancel. A flag records the
  signal kitsu raises on itself, and the handler is inert for it — sound with no
  tolerance window, because `raise(2)` runs the handler synchronously on the
  raising thread.

  **Two clauses of this requirement are vacuous today, not met.** The alt-screen
  and window-title clauses describe a runtime (`kitsu_tui::TuiRuntime`) that has
  **no caller in the binary**, so nothing pushes a title or enters the alt
  screen and nothing has to restore them. A test asserting their restore would
  pass by construction, which is why none was written. They become real when
  T5's fullscreen renderer uses that runtime.

  The restore is installed **only in the viewport**, so classic keeps the signal
  dispositions it started with.
- **N6** Works under tmux, zellij, screen, Windows Terminal; `inline` degrades
  to `fullscreen` where DEC scroll regions misbehave (Codex's Windows
  fallback); `--no-alt-screen` forces `inline`.
- **N7** No new network surface. Hooks run local commands only; H1 reads
  files; there is no socket. (Stages 0 and 4 cite this.)
- **N8** Security: peek/attach show only redacted timelines; replies to blocked
  agents go through the two existing gates and are journaled like foreground
  approvals.

  **Amended in T2-S6, because "redacted" was the wrong word for what protects
  these entries.** An approval entry records the **kind** of permission and
  nothing else -- "approval to write a file" -- never the path, URL or command
  line. It has to, because redaction here cannot help: `sanitize_and_redact_text`
  strips terminal controls and *credential* forms (key prefixes, `key=value`,
  `Bearer`, URL userinfo) and leaves filesystem paths and argv words completely
  untouched, as a test now pins. Redaction answers "is this a secret?", not "is
  this private?", and the snapshot validator uses that same function -- so an
  entry carrying a path would have reached `/jobs show`, `/jobs explain` and the
  JSONL snapshot while passing validation.

  The guarantee is therefore structural, not filtered: the request's payload is
  converted to an `AgentApprovalKind` at the boundary and no argument travels to
  the recording site, and that kind is a **unit enum with no argument-bearing
  field**, so a future change cannot carry a path into a timeline entry without
  altering a public type. See **F40**. The "journaled like foreground approvals"
  clause holds with the same caveat: the journal records that an approval
  happened and how it was decided, not what was asked for in detail.
- **N9** `--reduced-motion` MAY enter TUI modes (it keeps styled output by
  definition, `cli.rs`); there it disables spinner animation, counter flashes,
  and periodic age refresh. Its output is not covered by N1's byte-identical
  rule.

---

## 4. Design

### 4.1 Crate layout

```
crates/kitsu-editor/         NEW — typeahead_editor.rs moved verbatim (pure keymap/editor,
                             39 tests); used by the classic collector and by kitsu-tui
crates/kitsu-tui/            NEW — ratatui 0.29 + crossterm; owns the terminal while active
  src/lib.rs                 TuiRuntime: enter/leave, event loop, frame scheduler
  src/viewport.rs            pinned viewport: scroll-region flow, insert_history fallback,
                             injectable per-fd sinks (testable in-process with vt100)
  src/composer.rs            kitsu_editor::TypeaheadEditor + a CompletionSource trait
                             (implemented in the bin over attachments::complete and the
                             slash palette), hinter, highlighter
  src/status.rs              status row model → one Line
  src/cells.rs               bounded cell ring (N4)
  src/overlay/{transcript,agents,approval,help}.rs
  src/agents_view.rs         rows/groups/filter over plain AgentRow/AgentGroup structs
  src/title.rs               OSC title + state file (H2)
crates/kitsu/src/tui_bridge.rs   NEW — maps registry DTOs → AgentRow, implements
                             CompletionSource, wires the slash registry; cfg(unix) until
                             the Windows check passes
crates/kitsu-render          Surface seam + LineStyler reused for cells
crates/kitsu-tools           prompt route task-local (replaces the bool), both gates
```

`crates/kitsu` is a **binary-only** crate (no `src/lib.rs`), so `kitsu-tui`
cannot import `TypeaheadEditor`, `subagent`, or `attachments` from it. The
editor therefore moves to `crates/kitsu-editor` (it has no imports from the
binary), and everything else crosses the boundary as plain structs or traits
implemented in `tui_bridge.rs`.

`kitsu-render` stays the single source of text styling; `kitsu-tui` only
decides *where* lines go. `kitsu-render::live` becomes the `classic` backend
behind a small `Surface` trait so both backends receive the same calls
(`transcript_line(fd, text)`, `set_spinner`, `set_draft`, `set_queue`,
`notice`, `approval_request`).

### 4.2 Rendering model (inline, default)

Codex's approach, adapted to kitsu's two-fd transcript:

```
┌ native scrollback ───────────────────────────────┐
│ … finished cells: assistant text via stdout,     │
│   receipts/notices via stderr, same bytes as     │
│   classic …                                      │
├ viewport (bottom N rows, cursor-addressed, stderr)┤
│ ↳ next: refactor tests          (queue previews) │
│ ( ･ᴥ･ )ﾉ foraging (12s)   ◇ Read(src/lib.rs)     │  ← live row
│ kitsu> fix the failing test_|                    │  ← composer
│ qwen2.5-coder · ■■■■□□□□ 41% · 4.2s · [2 queued] · ← 1 agent │  ← status
└──────────────────────────────────────────────────┘
```

- A **cell** is one logical transcript item (streamed assistant paragraph,
  tool header, tool result, notice, milestone) tagged with its fd. Two
  mechanisms get it into native scrollback; the PoC (plan T0, Q1) decides:
  - **Region flow (preferred):** while the viewport is shown, a DEC scroll
    region (`DECSTBM`) covers every row above it and the cursor is parked
    inside the region after each viewport paint. Transcript text is then
    written **normally on its own fd** (stdout for assistant text, stderr for
    chrome) and scrolls into scrollback like today; the viewport rows below
    the region are painted by absolute positioning on stderr.
    **An in-process emulator cannot settle this.** `vt100` pushes rows to its
    scrollback only when no scroll region is active, so with the viewport's
    region set it discards them (finding F13) — which is itself a faithful
    model of a terminal that does not retain region-scrolled lines. So the
    screen half of the mechanism is verifiable in process, and whether a
    **real** terminal keeps the transcript is strictly a live-matrix question
    for the proof of concept. That is Q1, and it is why the fallback below
    stays unbuilt until the matrix reports.

    **T0-S6 verdict: region flow is the chosen mechanism, and the fallback is
    not built.** The two mechanisms trade against each other on exactly two
    questions: insert-history buys native scrollback (Q1) at the price of
    routing every transcript writer through the seam (Q3). Q3 was measured and
    answered — a writer that bypasses the seam flows into the region harmlessly,
    in cooked *and* raw input mode, because a direct write inherits the cursor
    parked inside the region and the terminal scrolls that region on a line feed
    whoever wrote the bytes. That removed region flow's cost without removing
    insert-history's price, so region flow wins on the evidence in hand. **Q1
    remains unmeasured and is the one input that could overturn this**; the
    matrix in `docs/roadmap/cycles/e11/terminal-matrix.md` states in advance
    what each outcome means, so the decision cannot be rationalised after the
    fact. Keeping the fallback unbuilt is therefore the deliberate position, not
    an omission.
  - **Insert-history (Codex-style fallback):** on cell completion, set the
    region, write the cell's text on its own fd inside a synchronized update,
    flush, reset — the interleaving `with_live_row_suspended` already relies
    on (clear stderr row → write stdout line → repaint).

  Either way the text is `LineStyler` output, identical to classic.
- **Stray writers.** `main.rs` has 392 direct `eprintln!/println!` sites
  (mostly slash-command output) that bypass `kitsu-render`. They run on the
  REPL thread between prompts, so the slash dispatcher is wrapped **once** in
  the suspend/repaint protocol; async notices already go through
  `PromptSafeAsyncNotice` → `with_live_row_suspended`. Output that bypasses
  the seam reaches native scrollback but **not** the cell ring; the transcript
  overlay and fullscreen document that limitation until those sites are
  routed (plan T3-S10).
- Every inserted cell is also appended to the **cell ring** (N4), which feeds
  the transcript overlay, the `[` dump, and fullscreen mode.
- Overlays (transcript, agent view, help) enter the alternate screen, draw,
  and leave it on close; the inline viewport repaints. `tui.alternate_screen =
  never` renders overlays inside the viewport area instead (shorter, but works
  in zellij scrollback).
- `fullscreen` mode is the same widgets with a transcript widget over the cell
  ring replacing native scrollback; stdout still receives the assistant text
  so `2>/dev/null` and pipes keep their meaning when stdout is not the TTY
  (in which case N1 keeps the TUI off anyway).

### 4.3 Event loop

```
select! {
  key/resize  <- crossterm EventStream
  orch event  <- orchestrator channel (fragment, tool header/result, usage, stats)
  registry    <- AgentRegistry::subscribe() (watch) + tail_timeline(id) (attach)
  approvals   <- prompt-route requests from both gates (foreground modal + blocked agents)
  tick        <- 100 ms spinner / 10 s age refresh (both off under --reduced-motion)
}
→ reduce into AppState → mark dirty → frame scheduler draws at most every 16 ms
```

Input bytes are handled by the existing pure `TypeaheadEditor`, moved to
`crates/kitsu-editor` so both the binary and `kitsu-tui` can use it; the
keymap and its 39 tests carry over. What does **not** carry over for free is
rustyline's `ChatHelper`: the `@file` completer (`attachments::complete`), the
slash-palette completion list, the hinter and the highlighter must be
re-hosted in `composer.rs` on top of the editor's `CompleteRequest` action,
which today only accepts the inline slash hint. The idle prompt stops using
rustyline in TUI modes (rustyline keeps `classic`); the history file format is
shared.

**Amended in T3-S4, because "shared" was read as "reimplemented".** The history
file **and its implementation** stay rustyline's. In `inline`, rustyline's
*composer* never runs but its *history* still does: it loads the file, the inline
reader is handed a snapshot of the loaded entries (re-taken on every read, so it
is fresh each turn), every submitted line is recorded on the path common to both
readers, and rustyline writes the file. That is why cross-session history works
in the viewport — a thing F20 twice recorded as missing when it never was — and
why **nothing in `kitsu-tui` needs to know the file exists**. A loader there
would be a second parser for a format rustyline owns and a second writer for one
file; see **F49**.

### 4.4 Agent view and blocked agents

Backed mostly by existing data: `AgentRegistry::snapshot()`,
`thread_snapshots()`, `AgentTelemetry`, `AgentHandoffState`. New pieces:

- **Prompt route (kitsu-tools).** Replace the `task_local! INTERACTIVE_PROMPTS_
  ALLOWED: bool` with `task_local! PROMPT_ROUTE: PromptRoute` where
  `PromptRoute = Foreground | Denied | Registry { agent_id, sender }`.
  `scope_without_interactive_prompts` keeps its name and sets `Denied`
  (unchanged behavior for every current caller); a new
  `scope_with_registry_prompts(agent_id, sender, fut)` sets `Registry`. Both
  gates (`Permissions::resolve` `Mode::Ask`, `command.rs` `CommandMode::Ask`)
  match on the route: `Foreground` → existing `Prompter` path; `Denied` →
  existing `PolicyDenied` text; `Registry` → build the same
  `PermissionRequest` / `CommandApprovalRequest`, send it with a `oneshot`
  through `sender`, and `await` the answer as a `Decision`
  (`AllowOnce|AllowAlways|Deny|Interrupt`). The registry marks the thread
  `Blocked { request }` on send and clears it on answer. Timeout (B3) or
  agent cancel resolves `Interrupt`, which the gates already map to
  `AuthorityInputClosed`. Grants are applied by the gate itself through the
  existing `insert_minimal_root` / command-grant code, under the existing
  per-allowlist `RwLock`s; foreground and background prompts need no further
  serialization because the background path never touches stdin and grant
  insertion is idempotent.
- **Registry additions.** `AgentThreadPhase::Blocked`, `AgentStatus::Blocked`,
  `AgentProgressStatus::Blocked` (B5); `answer(id, Decision) ->
  AgentAnswerOutcome`; `rename(id, name)` and `name: Option<String>` on the
  snapshot/DTO; `AgentRegistrySnapshot` gains `blocked` and `unacknowledged`
  counts (the status counter and `background_agent_notice` consume the
  snapshot, which today has only `generation/running/terminal/failed`);
  `tail_timeline(id) -> broadcast::Receiver<AgentTimelineEntry>` (capacity 256;
  on `Lagged(n)` the recap says `n entries skipped`) — named to avoid the
  existing private `AgentRegistry::attach(id, JoinHandle)`;
  `AgentTimelineKind::Approval` for request/answer events so the recap and
  `/jobs show` can show them.
- Group ordering and filter parsing live in `kitsu-tui::agents_view` as pure
  functions over DTOs so they are unit-testable without a terminal.
- **Row indicators.** Rows use the state **word** (`blocked`, `working`,
  `done`, `viewed`, `failed`, `cancelled`) as the primary indicator, exactly as
  the `/jobs` deck does, tinted per theme. The only row glyph is `◉` for
  *blocked* (it is the approval glyph; plain maps it to `o`). `◆`/`◇` keep
  their `/profiles` capability meaning and `◈` stays the deck/section header;
  none of them is reused as a row-state glyph, so plain mode never renders a
  row as `tool>` or `result>`. Any new glyph must be added to
  `docs/ux-design.md` and `ascii_structural_markers` in the same change.

### 4.5 Attach semantics

Attach does not move the agent to the foreground or take its stdin; it is a
**view** plus a routed composer. While attached:

- composer text → `steer(id, text)`; if the agent is blocked, the composer
  offers the option keys first (`y/a/n`) and free text as a steering message.
- foreground orchestrator output is buffered (bounded, 64 KiB; overflow noted)
  and replayed with a `⛩ detached from agent N — recap:` milestone on return.
- `Ctrl+C` keeps the existing editor rule: a non-empty draft is cleared; on an
  empty draft it **detaches** (it never stops the agent — stopping is explicit
  via `x` in the view or `/jobs stop`). `Ctrl+Z` keeps its existing meaning
  (suspend kitsu) and is not a detach key.
- the prompt label changes to `agent 3>` and the status row shows
  `attached · agent 3`.

### 4.6 Classic mode improvements that ship regardless

Cheap wins so `classic` users (and herdr users) benefit before Stage 2:

- the live status row gains the ambient background-agent tag, driven by the
  registry watch, in the bracketed form R5 specifies (A8 classic path).
- new `/jobs` subcommands in all three dispatch sites (idle
  `handle_jobs_command`, mid-turn `handle_active_jobs_command`, and the
  `/agents` alias arm, widened to forward them): `view` (grouped deck as in
  A2, printed inline), `explain <id>` (herdr's `agent explain`:
  why this state, since when, what it is waiting on), `reply <id> <y|a|n>`;
  `/agent reply <id> <y|a|n>` as the per-thread form.
- H1–H3 hooks (title, state file, `kitsu agents --json`, `on_agent_state`).
- `docs/functionality.md` sentence "Styled mode uses a redraw-safe external
  printer" is stale (the printer is opt-in via `KITSU_LIVE_NOTICES`); fix it.

### 4.7 Keybindings

Prefix `Ctrl+X` (configurable `[tui] prefix`): readline's chord prefix, which
Claude Code also uses (`Ctrl+X Ctrl+K`, `Ctrl+X Ctrl+E`), and a control byte
the `TypeaheadEditor` ignores today (its `0x00..=0x1f` catch-all), so no
existing binding moves. `Ctrl+B` is **not** used: it is readline/typeahead
backward-char. Chords apply only in TUI modes; `classic` keeps rustyline's
default map.

| Chord | Action |
|---|---|
| `Ctrl+X a` | agent view |
| `Ctrl+X o` / `Ctrl+O` | transcript overlay (`Ctrl+O` is unbound in the typeahead today) |
| `Ctrl+X d` | detach from attached agent |
| `Ctrl+X 1..9` | attach agent N |
| `Ctrl+X ?` | help overlay listing active bindings |
| `←` on empty composer | agent view (twice; configurable) |
| `Ctrl+L` | repaint (unbound in the typeahead today) |

Everything else keeps today's editor keymap (`Ctrl+Z` suspend, `Ctrl+\` quit,
`Ctrl+C` draft-aware, `↑` recall). Bindings are declared in one table consumed
by the help overlay and `/help keys` so docs cannot drift.

### 4.8 Configuration

`Config` is `#[serde(deny_unknown_fields)]` and flat today (`agent_spawn_cap`,
`theme`, `gateways`). The new sections are optional nested structs, each also
`deny_unknown_fields`:

```toml
[tui]
mode = "inline"             # classic | inline | fullscreen
alternate_screen = "auto"   # auto | always | never
prefix = "ctrl+x"
title = true
left_arrow_opens_agents = true
cell_ring_lines = 10000

[agents]
blocking = "ask"            # ask | fail_closed   (fail_closed outside TUI modes)
block_timeout = "30m"

[hooks]
on_agent_state = ""         # command; env KITSU_AGENT_ID/STATE/WAITING_FOR
```

`agent_spawn_cap` stays flat (no alias churn). Forward-compat note: any new
key, flat or nested, makes an older kitsu reject the file, because `Config`
denies unknown fields; the release notes must say so.

CLI: `--tui <mode>`, `--no-alt-screen`.

---

## 5. Stages

Effort: S ≤ ½ day · M 1–2 days · L 3+ days. Each stage is independently
shippable and gated by `cargo test --workspace --no-fail-fast` in debug and
release plus a PTY smoke.

### Stage 0 — Herding hooks and classic polish (S/M)
A8 (classic), H1, H2, H3, N7; `AgentRegistrySnapshot.{blocked,unacknowledged}`
(blocked stays 0 until Stage 1); `/jobs view|explain`; the `functionality.md`
fix (§4.6); amend `docs/claude-code-comparison.md` §8 (Supersedes note).
*Acceptance:* `kitsu agents --json` validates every embedded DTO with
`validate_default()`; the state file appears/disappears with the process in a
`--script` run; title changes observed via `script`; hook fires once per
transition; plain/JSONL byte-identical fixtures unchanged.

### Stage 1 — Blocked agents, permission-only (M)
B1–B5. Prompt route in `kitsu-tools`, both gates, `answer`, timeout,
`/jobs reply`, `AgentTimelineKind::Approval`, schema version 2.
*Acceptance:* unit: a detached agent requesting an ungranted write enters
`Blocked`, `reply y` lets it finish, `reply a` widens the shared runtime root
exactly as the foreground `always` does, `reply n` yields the same
`PolicyDenied` text as today; same three cases for a `command_exec` approval;
timeout → `Failed(interrupted)` with `AuthorityInputClosed`; every DTO still
passes `validate_default()`; `--plain`, scripted and JSONL defaults remain
fail-closed and byte-identical.

### Stage 2 — `kitsu-tui` inline renderer (L)
R1–R5, R7, R8, N1–N3, N5, N6, N9. Surface trait; classic backend wraps
`kitsu-render::live`; inline backend with insert-history on stderr; composer
adapter re-hosting `ChatHelper` features; cell ring; status row; approval
modal; `/tui`, `--tui`, `--no-alt-screen`.
*Test infrastructure deliverable:* the existing PTY harness
(`run_plain_interactive_chat_pty`) runs `--plain --no-stream`, under which the
TUI never starts; add a **styled, streaming** PTY harness driving a
tool-calling wiremock provider.
*Acceptance:* PTY run of a streamed turn with two tool calls: stdout after
sanitize equals classic stdout, stderr receipts after sanitize equal classic
stderr; resize mid-turn leaves no duplicate lines; `kill -INT` restores the
terminal; tmux and zellij smoke; N2 benchmark (10 KiB paste) reported in the
PR, gating manually, not in CI; all existing render/CLI tests green.

### Stage 3 — Agent view, peek, attach (L)
A1–A7, N8, R6 (transcript overlay shares the overlay machinery).
*Acceptance:* pure-function tests for grouping/filter/row text; integration
test with fake provider: spawn two bg agents, one blocks, agent view shows it
first, `r` reply unblocks, attach streams the timeline, detach prints the
recap and marks handoff `Acknowledged`; dispatch beyond `max_running` is
refused by the registry (not the spawn budget); exit guard still refuses with
an unviewed failed agent.

### Stage 4 — Fullscreen renderer and polish (M)
Fullscreen mode over the cell ring (N4), `[` dump, `v` editor, mouse wheel
scrolling, herdr manifest (H4), `agent_wait` tool (H5), decision on
`background_blocked` under JSONL (open question 7).
*Acceptance:* a 5 000-line session renders with RSS flat once the ring is
full; `[` output equals the ring contents; `agent_wait` unit tests for both
`until` values and timeout.

Sequencing: 0 → 1 → 2 → 3 → 4. Stage 1 is deliberately before the TUI so the
*blocked* state exists when the view arrives; Stage 0 ships value to herdr and
tmux users immediately.

---

## 6. Risks

| Risk | Mitigation |
|---|---|
| DEC scroll-region insert misbehaves on some terminals (Windows Terminal, old tmux) | Codex precedent: detect and fall back to fullscreen; `--no-alt-screen`; keep `classic` forever |
| Two input stacks (rustyline classic, TUI composer) drift | Both drive the pure `TypeaheadEditor`; completion/hint logic moves to shared functions called by both `ChatHelper` and `composer.rs` |
| Blocked agents reopen the "competing stdin" problem that fail-closed solved | Answers come only through the registry, never stdin; the route is task-local, so every existing `scope_without_interactive_prompts` caller is unchanged; default off outside TUI |
| Interleaving stdout cells with stderr cursor control | Same technique `with_live_row_suspended` uses today; flush order is the contract; PTY test asserts per-fd equality |
| `main.rs` growth | All TUI code in `kitsu-tui`; bridge file ≤ 1k lines; slash handlers call registry methods, not render code |
| Attach confuses users about which turn their text goes to | Prompt label changes to `agent 3>`; status row shows `attached · agent 3`; `⛩` milestones on both ends |
| Performance regression at idle (the ExternalPrinter lesson) | N2 benchmark in the Stage 2 PR; frame coalescing; no per-byte redraws |
| New config sections break older binaries | Unavoidable with `deny_unknown_fields`; documented in release notes; `kitsu doctor` names the offending key |
| Schema bump of the thread DTO | Version 2 is additive in meaning; the DTO is not persisted today, so only `/jobs` JSON consumers see it |

---

## 7. Open questions

1. Should `inline` become default one release after Stage 2, or stay opt-in
   longer? (Lean: one release.)
2. Agent names: auto-generate from task head like Claude Code, or require
   `n` rename? (Lean: auto head, renameable.)
3. A blocked agent's `a` (always) widens the shared session allowlist, as the
   foreground does, because it is the same `Permissions` object. Confirm this
   is desired, or restrict background answers to `y`/`n`.
4. `agent_wait` does not consume the spawn budget (it spawns nothing). Confirm.
5. Free-form questions from a subagent (`ask_user` tool) would contradict the
   current `SUBAGENT_SYSTEM` prompt ("Do not ask questions"). Add later, or
   never?
6. A local socket API (herdr-style `kitsu agent prompt --wait`) is the natural
   successor to the state file; it is a new local surface and needs its own
   design.
7. Should `agents.blocking = ask` ever be allowed under JSONL, with an additive
   `background_blocked` event? (Lean: no until a consumer asks.)
8. Worktree isolation per background agent (Claude Code, herdr) is out of
   scope here; decide whether it belongs with `docs/workspace-diff-tool.md`.

---

## 8. Glossary

- **cell** — one logical transcript item; the unit inserted into scrollback
  and appended to the cell ring.
- **cell ring** — bounded in-memory list of rendered cells (N4).
- **viewport** — the bottom rows kitsu redraws in `inline` mode.
- **overlay** — a temporary full-area screen (alt screen when allowed).
- **blocked** — a background agent waiting on a permission answer.
- **prompt route** — the task-local that tells the two approval gates where
  to send a request: foreground prompter, registry, or nowhere (deny).
- **handoff** — kitsu's existing result-acknowledgement state; maps to
  herdr's done-unviewed vs idle.

## 9. Sources

- Claude Code docs: interactive mode (`code.claude.com/docs/en/interactive-mode`;
  `Ctrl+B` backgrounds tasks, `Ctrl+X Ctrl+K` / `Ctrl+X Ctrl+E` chords),
  fullscreen rendering (`/docs/en/fullscreen`), agent view (`/docs/en/agent-view`).
- Codex CLI: `openai/codex` `codex-rs/tui` (ratatui, inline viewport,
  `insert_history.rs`, `tui.alternate_screen`, PR #8555), CLI reference
  (`developers.openai.com/codex/cli/reference`).
- herdr: flaviocopes.com/herdr, x-cmd.com/install/herdr, innfactory.ai
  herdr overview (states, keybindings, socket API, integrations, plugins).
- Kitsu: `docs/agent_deck_ui.md`, `docs/subagents-design.md`,
  `docs/ux-design.md`, `docs/ui-parity-plan.md`, `docs/functionality.md`,
  `docs/jsonl-protocol.md`, `docs/claude-code-comparison.md` §8,
  `crates/kitsu-render/src/live.rs`, `crates/kitsu-render/src/lib.rs`,
  `crates/kitsu/src/subagent.rs`, `crates/kitsu/src/subagent/thread.rs`,
  `crates/kitsu/src/typeahead_editor.rs`, `crates/kitsu/src/config.rs`,
  `crates/kitsu/src/session.rs`, `crates/kitsu-tools/src/lib.rs`,
  `crates/kitsu-tools/src/permissions.rs`,
  `crates/kitsu-tools/src/builtins/command.rs`, `crates/kitsu/src/main.rs`.
- Review: `reviews/tui-design-review-1.md` (findings and disposition).
