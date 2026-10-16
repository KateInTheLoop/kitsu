# Kitsu TUI and agent view — design and requirements

- **Status:** **Implemented through milestone T3** (T3-S13). Requirements R1-R5, R7, R8, N2-N5, N8, N9 and B1-B5 are shipped or amended in place with the reason; R6 (transcript overlay), A1-A8 (the agent view) and the `fullscreen` renderer remain for T4 and T5. **Q1's live terminal matrix is still unrun and gates making `inline` a default rather than an opt-in.** Previously: Draft v2.1 (revised after reviews `reviews/tui-design-review-1.md`
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

  **R2 is complete as of T5-S2.** `fullscreen` was never *missing* — it was a
  **deliberate, multiply-pinned refusal** (**F90**): an arm whose own comment
  called it *"a mode the design's grammar has"*, twelve call sites of the
  accepted-modes literals, four test functions and an asserted `/help` string
  saying it *"is not available yet"*. **Retiring that boundary, not filling a
  gap, was the card's actual cost** — seven test collisions, six authorised
  changes by name, and two stops.

  All three spellings now accept it, and **they cross together on purpose**:
  the code calls them *"the one spelling `/tui`, `--tui` and `[tui] mode` all
  use"*, and a mode you can switch to but cannot start in would be incoherent.
  The four advertised surfaces — `/tui`'s args/syntax, `/help tui`'s details,
  `--tui`'s possible values, and `[tui] mode`'s error — are pinned as **one
  vocabulary**, with an added ban on the details still announcing fullscreen as
  unavailable: *a renderer that ships behind a help string saying it has not
  shipped is a half-done card.*

  **Completed in T3-S9, which decided three things R2 left open.** All three are
  user-visible rules, not implementation details, so they belong here:
  - **`fullscreen` is refused** until T5's renderer exists, naming what is
    accepted — `/tui fullscreen is not available yet; accepted: classic,
    inline` — the same vocabulary `[tui] mode`'s error uses. A value that
    parsed and silently behaved as `inline` is the quiet coercion this epic has
    refused everywhere else.

    **Lifted in T5-S1b+T5-S2: the renderer exists and `fullscreen` is accepted**
    in all four spellings (`/tui fullscreen`, `--tui fullscreen`, `[tui] mode`,
    and bare `/tui`'s report). The **rule** that outlived the refusal is the one
    above it: a renderer kitsu does not have is refused by name rather than
    coerced. **F90** is why this pin is amended rather than deleted — *"X is
    missing" and "X is refused" look identical from outside and are opposite
    facts*, and a deleted pin would leave no trace that the absence had been
    deliberate and multiply enforced.
  - **Mid-turn `/tui` is refused**, not queued and not deferred to the turn
    boundary. The reason is specific: the mid-turn dispatcher's default
    fall-through **enqueues the line as a future prompt to the model**, so an
    unhandled `/tui inline` would be *sent to the model as text* — see **F57**.
    A swap landing silently seconds later is also indistinguishable from the one
    the user would get by retyping.
  - **Bare `/tui` reports the live renderer**, doubling as the usage line, and
    must track the renderer actually running rather than the start mode.

  **A switch is narrated** (`~ switched to the … renderer; the conversation is
  unchanged`), because R2's guarantee is that the conversation survives and a
  silent switch leaves the user unable to tell a working switch from a lost one.
  Switching into a terminal too short for the viewport's minimum height stays
  classic and says so; geometry is re-probed on every install, since no viewport
  listens for resize during a classic segment.

  **"Classic is byte-identical" is scoped to sessions that never switched.**
  Once any inline segment has existed, the signal restore installed with it
  (N5) stays registered for the life of the process, so a later `SIGINT` writes
  a region reset even in classic. That is harmless — it is a reset of a region
  nothing has pinned — but it is not byte-identity.
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

  **Amended at R6-a's review, twice, after measuring the ring.** Split into four
  cards (R6-a overlay+scroll+close, R6-b search, R6-c prompt jumps, R6-d the two
  hand-offs), and **two clauses above are not buildable as written**:

  **`{`/`}` prompt jumps have nothing to jump between (F102).** `CellLine` carries
  only `fd: Fd` and `styled: String` -- **no role marker** -- and production feeds
  the ring from exactly two `transcript_line` paths. The user's submitted line is
  painted by the composer and then **erased**, so it never becomes a transcript
  line: **the ring holds no prompts at all.** Since **N4** makes the ring the sole
  source for this overlay, `{`/`}` is unsatisfiable until the *feed* marks prompt
  boundaries. **R6-c is blocked on that feed change, not on overlay work**, and the
  honest consequence is wider than the key: **the overlay shows one speaker.**
  Inferring prompts from `Fd` is refused -- `Stderr` is tools and diagnostics, not
  the user.

  **F102 RESOLVED: the feed now marks prompts, and R6-c is unblocked.** `CellLine`
  carries a `prompt` flag, set only through `CellRing::push_prompt`, which splits on
  `\n` so a multi-line or pasted prompt keeps one cell per line -- both frames are
  one `fit_row_to_width` per cell, so a single multi-line cell would have garbled
  them. Prompts file as `Stdout`, the conversation, not `Stderr`, the tools. Three
  feeds carry it: the foreground submission, the fullscreen tap (whose ring is the
  **only** ring in that mode), and a **delivered** attached submission -- a refused
  one is handed back to the composer and was never said. **Injected commands are not
  marked**: the words are kitsu's, and a transcript marking them as the user's is
  the same false claim as a doc that says a command works when it does not.

  **The mark had to be preserved at `frame_ring` as well as stored**, because that
  rebuild is the chokepoint both readers go through; a mark stored faithfully and
  dropped there would have left **every** reader seeing one speaker, with the feed
  perfectly correct.

  **`[`'s "fullscreen only" is a description, not a restriction, and nearly a
  contradiction (F103).** Native scrollback is the one thing the alternate screen
  does not have: bytes written while `ESC[?1049h` is in effect are discarded by the
  terminal on `ESC[?1049l`. So a dump performed *while fullscreen holds the screen
  goes nowhere.* The clause can only mean the key is **offered** only in fullscreen
  and the dump **leaves the alternate screen first**, or defers to the leave -- which
  makes `[` a terminal-state hand-off like `v`, not a write, and is why both land in
  one card.

  **Amended again, before R6-d is carded: the two hand-offs MUST be gated, because
  the ring now retains what the user pasted.** Once the feed marks prompts (the
  card that unblocks R6-c), the ring holds the user's own lines, and a user may
  paste a credential into the composer.

  A submitted line **already** reaches `history` on disk and persisted `messages`,
  so the ring itself is **strictly weaker** than what the session already keeps.
  **`[` is not.** It moves that text out of kitsu's address space and into the
  terminal emulator's scrollback -- a buffer kitsu **does not own, cannot bound and
  cannot clear**, and which `tmux capture-pane`, `script` and terminal logging
  routinely capture. Before this, `[` could only dump what the session itself
  printed; after it, it dumps **what the user pasted**. That is a different
  exposure class, not more of the same. `v` writes it to a `$EDITOR` temp file, and
  `/` (R6-b) makes it findable by anyone at the keyboard.

  **This is the exact reversal of F68**, which withholds a swallowed paste burst's
  **count but never its text** precisely because the clipboard may hold a secret.
  Retaining prompts contradicts that unless the hand-offs are gated, so: **R6-d
  ships with an explicit confirmation, or with an opt-out that excludes prompt rows
  from the dump -- and R6-d is not carded until which one is decided.** R6-b needs
  no gate: searching in-process memory exposes nothing the overlay does not already
  paint.

  **R6 is COMPLETE except R6-d.** R6-a (overlay, scroll, close), R6-prompts (the
  feed, which resolved F102), R6-c (`{`/`}`) and R6-b (`/`, `n`/`N`) have shipped.
  **R6-b measured two things the requirement had assumed away.** First, cost: a
  full pass over a 10 000-line ring allocates a string per line and takes
  **4.1 ms release / 13.7 ms debug** (9.8/45.4 with one 1 MB pasted cell), so
  **search runs on submit, not per keystroke** — a per-keystroke search would blow
  N2's 100 ms budget on a short query. Second, `plain()` is **exactly the glyphs of
  the painted row**, so *searched is painted*; the only divergence is **width**,
  because `fit_row_to_width` cuts at `cols` and a match past the right edge still
  counts — width-dependent matches would mean one query finding different things
  in two window sizes.
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

- **A1** `Ctrl+X a` or `/jobs view` MUST open the agent view overlay; `←` on an
  empty composer MUST open it too (first press shows a hint, second opens),
  configurable off. `←` is distinct from the existing `↑`-on-empty queue recall
  and never consumes a draft.

  **Corrected in T4-S2: two of this requirement's clauses described work that
  had already shipped.** It used to say the `/agents` alias "today forwards only
  an empty or numeric argument" and that `handle_active_jobs_command` "gains the
  same subcommands". Verified false on both counts — the alias maps empty to
  `list`, a number to `show <n>`, and **everything else verbatim**, with a
  comment saying `/jobs` owns the suggester; and the mid-turn handler already had
  `view`, `explain` and `reply`, with tests. The widening was a **no-op**, and the
  only missing piece was the overlay at the far end. Dispatch was therefore not
  restructured.

  **The chord is read from `[tui] prefix`**, not hard-coded: a configured
  `ctrl+t` leaves the default `Ctrl+X a` inert, and an unrecognisable spelling
  yields **no chord route** rather than a silent rebinding. `←` is gated by
  `[tui] left_arrow_opens_agents`, which disables that route alone.

  **The two-press `←` disarms on any other keystroke.** The armed state belongs
  to an uninterrupted run of `←`s on an untouched empty composer, so typing and
  deleting back to empty requires two presses again. The alternative — arming
  until the view opens — would make a keystroke's meaning depend on something the
  user did minutes ago and can no longer see.

  **How `↑` is protected.** The arming state is a single local in the inline
  reader, not an editor action and not derived from the editor's empty-buffer
  predicate; `crates/kitsu-editor` was **not modified at all**, so
  `RecallRequest` and the deliberate decline that makes `↑` walk history are
  byte-for-byte unchanged. A byte filter between the read and the editor
  recognises only the prefix byte, `ESC[D` and the two bracketed-paste
  delimiters, forwarding everything else — and it tracks the paste delimiters
  precisely so a `0x18` or an `ESC[D` **inside pasted text arrives as content,
  not as a chord**. A sequence split across two reads is held and released in
  front of the next chunk, so the editor sees the same byte stream either way.

  **`/jobs view` now differs by renderer:** the overlay in a TUI mode, the text
  deck in `classic`. The deck is **not** also printed in inline mode — it would
  land in the transcript beneath the overlay, unseen but still scrolling the
  user's history.

  **Known gap:** the overlay takes the **alternate screen unconditionally**, so
  `[tui] alternate_screen = never` does not yet get the in-viewport overlay this
  design describes elsewhere. That path is unexercised and `alternate_screen`
  remains a key without a consumer.

  **Closed in F62 by refusal, not by implementation.** The key had **no consumer
  at all** — all three values did the same nothing, so `auto` was accidentally
  right and **`never` was a lie**. Both doors that produce it are now **refused by
  name**: the config value at load, and `--no-alt-screen` in argv validation, which
  fires for **every** subcommand including `doctor` — deliberately, because
  `doctor` is where a user goes to learn whether their setup is sound, and a global
  flag refused on some subcommands and accepted on others is a rule the user must
  memorise. `auto` and `always` keep working: they describe today's behaviour.

  The messages say **"not available yet"**, never "not valid" — the value is a
  coherent thing to want and kitsu intends to honour it, and **F90** applies.
  **Recognised spellings and accepted values are now different sets**: a typo still
  gets all three in the vocabulary error, while the refusal lists the two accepted.
  And `AlternateScreen::Never` **survives as a variant** with its dead-code
  allowance, because deleting it would turn *"recognised, not yet available"* into
  *"not a value"* — the same inversion F90 names.

  **T5-S4 owns the implementation**, and it must also settle a disagreement this
  sweep found: `:1408` says the flag forces **`inline`** while `README` said it
  forces **`never`**. Those are different claims — a renderer versus a screen
  value — and forcing a renderer would also disable fullscreen, a much larger
  promise than *"do not take the alternate screen"*.
- **A2** Rows MUST be grouped by state in this order: *Needs input*, *Working*,
  *Done (unviewed)*, *Done (viewed)*, *Failed*, *Cancelled*. Each row: state
  label, id, name or task head, model/profile, last activity + age, tokens.

  **Amended in T4-S1 on two points, both found by implementing it.**

  **(a) A row cannot be built from the thread snapshot alone.** Verified: the
  snapshot carries **neither tokens nor last activity** — those live on the
  telemetry, reached through `AgentProgress`, which the thread DTO deliberately
  does not embed. So a row is assembled from the snapshot **plus**
  `AgentProgress`, and the three fields the snapshot cannot supply are
  **`Option`, returning `None` rather than a zero**. A zero would be a lie: it
  reads as "no tokens used" rather than "not known here", and in a view whose
  job is telling you which agent needs attention, a confident wrong number is
  worse than an absent one.

  **(b) The order is a single source, and the labels deliberately differ from
  `/jobs view`'s.** `AgentViewState` in `kitsu-render` owns the order —
  `rank()` is read *out of* `order()` rather than restated — and it lives in
  that crate rather than the viewport crate because the existing `DeckState` is
  there and the new enum is a **refinement** of it (four glyph levels to six
  groups). `/jobs view` keeps its own wording (`done, result ready` versus this
  view's `done (unviewed)`); what a test enforces is that both surfaces agree on
  **which** groups exist and **in what order**, by looking up each of
  `/jobs view`'s own headings and comparing its index with `rank()`. Empty
  groups are omitted, matching `/jobs view` — one behaviour, not two.

  "Viewed" versus "unviewed" is the existing `AgentHandoffState`: `Ready` means
  nobody has looked, `Acknowledged` means somebody has. `Unavailable` on a done
  thread means there is nothing left to observe and is treated as viewed —
  though note it is **not a representable DTO** (see **F61**), so that arm is
  defensive rather than reachable.
- **A3** Keys: `↑/↓` move, `Enter`/`→` attach, `Space` peek, `+` dispatch (**added in T4-S7**; A6 named no key and this list was full — see A6 for why `+`), `s` steer
  (prompt for text), `r` reply (only when blocked), `x` stop (twice to prune),
  `n` rename, `p` mark result acknowledged, `/` filter, `?` help, `Esc`
  close. Filter grammar: `s:<state> n:<text> o:<text> p:<profile>`.

  **`↑/↓ move` was under-specified; T4-S4a decided these points and pinned each.
  The first is a safety property, not a preference.**

  - **The selection is an agent, not a row index.** Rows re-group constantly: an
    agent finishing moves from `working` to `done (unviewed)`, which in A2's
    order is a *later* group, so an index-keyed selection would silently
    retarget the user's next `x` or `p` onto a **different agent**. Keying on the
    agent id makes that impossible, and a test pins a regroup that really
    reorders so an index-keyed model lands on the wrong agent and fails.
  - **The selection clamps; it does not wrap.** `↑` on the first row stays.
    Wrapping would carry the cursor from `needs input` — the group that needs
    you — to `cancelled` in one keystroke, possibly while a stop is armed on the
    row left behind. With no rows nothing is selected, and neither key invents a
    selection.
  - **When the selected agent vanishes** (pruned), the selection falls to
    whatever row now holds its old display index, clamped to the new last row,
    and is `None` only when the list empties. That is where the user's eye
    already is; `None` would make the next keystroke a silent no-op.
  - **`x` is two presses per row, and moving the selection disarms it.** This
    extends T4-S2's rule for the two-press `←` to the case T4-S2 did not have:
    an `x` armed on one agent and firing on another after a `↓` is the one way
    this affordance stops an agent nobody asked to stop. A **repaint keeps** the
    arming — a repaint is not a keystroke, and an agent finishing under the
    cursor must not cancel a half-typed stop — but a repaint that **loses** the
    armed agent disarms.
  - **Moving the selection acknowledges nothing.** T4-S3 made peek the single
    call site that takes custody of a result; selection must not become a
    second one, or the guard on unviewed results stops meaning anything. Pinned
    in both directions, with `p` on a row just moved over as the positive
    control.
  - **`r` is not a text prompt, and `s` is.** `AgentRegistry::answer` takes a
    `Decision`, so `r` reads a decision through the **one** shared
    `parse_permission_answer` (the parser **F45** was closed by consolidating);
    only `s` needs a line of text. The row's `s` *(prompt for text)* applies to
    `s` alone.
  - **`n` rename legality**, which this requirement did not state: the name is
    trimmed, must be non-empty, at most `AgentRegistry::NAME_MAX_CHARS`, and may
    contain **no control characters**; **non-ASCII is legal** — the rule is
    control characters, not ASCII-only. **Any phase is renameable**, terminal
    included, since the agent whose result you are about to read is the one worth
    labelling. Each refusal is **distinct** (`Empty`, `TooLong`, `Unprintable`,
    `NotFound`), and a refused rename leaves the previous name intact.
    Deliberately **not** `safe_inline_label`: that helper coerces empty to
    `"unknown"` and truncates with an ellipsis, which is right for a
    model-produced label and is exactly the silent rejection this project
    refuses for a name a human just typed.

    **`n` only became able to honour "non-ASCII is legal" in F70's card.** The
    rule was stated here, the registry implemented it and its own tests pinned
    it — but the key read its draft through the **ASCII-only**
    `OverlayKey::Char`, so typing `café` renamed the agent to `caf`,
    **silently**. The rule and its enforcement agreed; nothing checked whether
    the only path a user has could reach it. **A validation rule is only as wide
    as the narrowest input path that feeds it** (**F70**) — the third time this
    epic paid for that blind spot, after **F57** and **F69**.

    `n` now reads its line through `kitsu-editor` on the same bypass as `s` and
    `/`, which makes the rename line an **innermost frame**: `Esc` steps out of
    it first, and **a refused rename is painted on that frame and leaves the line
    open carrying the text** — the same contract `/`'s refused term has, so the
    message and the text it is about are on screen together and one wrong
    character is corrected rather than a 48-character name retyped. Painting it
    anywhere else would repeat **F71**, a message the test can read and the user
    can never see.

  **`x` was under-specified, and T4-S4b settled it against a hazard this
  requirement did not anticipate.**

  - **The reading.** `x` arms; the **second press stops a live agent and prunes
    an already-terminal one.** That is how "stop (twice to prune)" is satisfied
    without a third press and without changing T4-S4a's arming. The premise was
    verified rather than assumed: `cancel`'s terminal predicate and `prune`'s
    `Running` predicate are the **exact complement** (both `Running | Blocked`),
    so "stop would answer `AlreadyTerminal`" and "prune would answer `Pruned`"
    describe the same set. The implementation **sequences** the stop and then the
    prune rather than reading a phase and then calling, so nothing can finish in
    the gap.
  - **`x` refuses to prune a terminal agent whose result is still unviewed**,
    reports that the result is waiting, and prunes on the identical press once
    `Space` or `p` has collected it. The reason is **F67**: `prune` removes the
    record, and the guard that surfaces unviewed results can only iterate records
    that still exist — so pruning discards the result **and the guard cannot
    know**.
  - **This is deliberately asymmetric with `/jobs prune <id>`,** which does
    discard such a result today. That is an explicit command naming a specific
    id, and its JSON path returns the terminal payload, so automation keeps the
    result; only the interactive path loses it. **The second press of an arming
    is neither explicit nor id-naming.** The asymmetry is the point: *the same
    registry call is safe behind a typed id and unsafe behind a keystroke.*
    Whether `/jobs prune` should also print the result before discarding it is an
    open product question recorded in **F67**, not something the key works
    around.
  - **Every action key has a no-selection report**, never silence — the same
    reasoning that gave T4-S4a's `press_stop` a `NoSelection` answer rather than
    a silent `Armed`. Likewise every failure case is **distinguishable** (`r` on
    an agent that is not blocked, `x` on an already-terminal one, `p` with
    nothing to acknowledge, `n` with an illegal name), because a human typing
    into a racy world must be able to tell *nothing happened* from *it worked*.
  - **Nothing in a paste is ever a key (F68).** This is a **safety** clause, not
    a convenience one. The overlay reads raw bytes and processes a whole chunk
    at once, and it never asked the terminal to bracket pastes — so before this
    rule a paste was indistinguishable from typing, and **one pasted line of
    prose was shown to approve a parked filesystem write and then stop the
    agent** (`allow_once`, 34 keys from one chunk). Pasted text must never
    become a privileged action.

    - **Inside a text prompt** (`n`'s name, `s`'s correction, `/`'s filter) the
      whole payload is content, **newlines included**; the prompt stays open and
      the user presses `Enter` themselves. This keeps **one** answer to "what is
      a byte inside a paste?" across the binary: `kitsu-editor` already holds
      that a bracketed paste never submits, and T4-S2 decided a prefix byte
      inside a paste is content.

      **This clause holds only where the terminal brackets pastes — see F72.**
      All three lines read their bytes through `kitsu-editor` on a bypass that
      skips the decoder, for three good reasons (a non-ASCII value is legal and
      `OverlayKey::Char` is ASCII-only, a bracketed paste must not submit, and a
      multi-byte character really arrives split across reads). But the
      **unbracketed-burst** fallback F68 added lives in the **decoder**, so these
      lines do not get it: on a terminal that does not report pastes the editor
      submits at the paste's first newline and the rest stays in the line. **The
      fix for one hazard removed the mitigation for another**, which is worth
      saying rather than leaving the clause looking unconditional.

      **The safety half is unaffected, verified by reading:** an open line's
      branch in `overlay_feed_chunk` ends with `return false`, so the **whole
      chunk** is consumed by the line and the decoder is never reached — a
      mid-chunk submit's tail **cannot** become keys, and F68's `r`+`y` and
      double-`x` hazards stay closed. The residual gap is early submission, not
      privileged action.
    - **Over the row list, and inside `r`'s decision read, a paste is dropped
      entirely** — never an action, and the decision read stays open rather than
      resolving to a pasted letter.
    - **Detection is two-sided**, because one side is not always available: the
      overlay emits `ESC[?2004h` itself (it owns the terminal, as
      `tui_bridge::KeyFilter` does without rustyline), **and** treats a chunk
      yielding more than one printable key as a burst rather than typing — the
      latter is what protects a terminal that does not report pastes at all.
    - **The burst rule is a heuristic and fails safe:** a fast typist, or a
      terminal that batches bytes, can have two quick presses read as one burst
      and dropped. That direction is deliberate, but it meets T2-S5b's rule that
      a human must be able to tell *nothing happened* from *it worked*, so a
      swallowed burst **says so**, names how many characters it ignored, and
      says how to make a paste mean something (`n` opens a name, `s` a
      correction).
    - **It reports the count and never the text.** The payload is the user's
      clipboard and may hold a secret, so painting it into the frame would be a
      new disclosure — the same reasoning as **F40**, where the timeline's
      protection had to become structural rather than accidental. Inside `r`'s
      decision read the paste is silent **by construction**: an open prompt owns
      the report row, and the read's own `y allow once, a allow always, n deny`
      line is the honest thing to show while it is still open.
    - **The overlay enables bracketed paste but does not disable it on exit**,
      deliberately. `RawStdinGuard` owns that pair (`engage` writes `?2004h`,
      `Drop` writes `?2004l`). At the inline idle reader the overlay runs
      *inside* the caller's still-live guard, so writing `?2004l` on close would
      strip bracketed paste from the composer underneath **for the rest of the
      session** — a regression in the surface that needs it most. The enable is
      belt-and-braces by the surface that depends on it; the disable stays with
      the owner.

    **F65 and F68 are one root with opposite signs**, and both belong in any
    future key work: *an arrow key is not one byte*, and *a chunk of bytes is
    not a sequence of intentions*. A loop that treats every decoded key as a
    deliberate keystroke is safe only while no key does anything.

  - **`s`'s text sub-mode (T4-S4b-ii, shipped).** `s` opens a real editable line
    over the existing `kitsu-editor`, not a second line editor — **F45** was
    closed by removing a duplicate parser of the same bytes, and this is the
    same rule. `kitsu-tui` paints it (`steer_prompt_rows`) and cannot reach the
    editor; `main.rs`, where the loop runs, can.
    - **`ESC` cancels the line and sends nothing**, under the same 50 ms grace
      **F65** introduced. Inside this overlay `Esc` already means *step back out
      of the frame you are in*, and the steer line is one more frame; two
      meanings for `ESC` on one surface is the F65 ambiguity itself. Only one
      surface resolves a held `ESC` at a time, and the grace is **spent only
      when an `ESC` is actually held**, so an idle line costs no repaint spin.
    - **While the line is open it reads the raw bytes itself, bypassing the
      overlay decoder** — so the editor sees the terminal's real
      `ESC[200~`/`ESC[201~` and enforces *a bracketed paste never submits* as
      the one parser that owns that rule. This is the deliberate answer to a
      hazard: F68's decoder **strips** the delimiters and yields plain text, so
      feeding that text back through the editor would let an **unbracketed**
      newline reach the submit path — F68 re-entering one layer down. A second
      reason seals it: `OverlayKey::Char` is **ASCII-only by construction**, so
      routing the line through the decoder would discard exactly the split-UTF-8
      handling that justified reusing the editor.
    - **Opening the line mid-chunk drops the rest of that read.** Those bytes
      were decoded as keys before the line existed, and letting them fall
      through to the row list could run `x`. A normal read carries one
      keystroke.
    - **An empty correction is refused.** The guard is key-level but constructs
      the registry's own `Empty` error, so the wording is identical to
      `/jobs steer`'s.
    - **The echo flattens control characters one-for-one and windows around the
      caret**, so a pasted newline cannot paint an unbudgeted row and a
      correction longer than the terminal still shows the cursor instead of
      pushing it off the edge. Its decisions
    are pinned by tests but not implemented: `ESC` cancels the line under the
    same 50 ms grace T4-S4a introduced, a newline **inside a paste is content**
    (consistent with T4-S2's prefix-byte rule), and an empty correction is the
    registry's own `Empty` refusal rather than a key-level guard. The input is
    the existing `kitsu-editor` line editor, reached from `main.rs` where the
    overlay loop runs — `kitsu-tui` depends on `kitsu-render` only and cannot
    reach it, and a second line editor there would be a second parser of the
    same bytes, which is what **F45** was closed by removing.

  **`/` was listed and unbound until T4-S5, which decided these four points.**

  - **The filter's rows are the selection's rows. This is a safety property.**
    The selection is keyed on an agent id, and a filter **hides rows without the
    agents going away** — so `AgentSelection::sync` is handed the **filtered**
    rows. Handed the full rows it would leave the selection on a **hidden**
    agent, and the dispatch would then let `x`, `r`, `n` or `p` act on an agent
    **not on screen**: the **F68** family exactly, a privileged action with no
    visible intent. Consequences, each pinned: a filter that hides the selected
    agent **moves the selection to a visible row**; a filter matching nothing
    leaves **no** selection, so `x` reports the no-selection line rather than
    stopping the agent that *used* to be selected. **Clearing the filter does not
    restore the old selection** — a remembered-and-restored selection is hidden
    state steering a destructive key, which is the same hazard with one more
    step.
  - **The filter line reads raw bytes through `kitsu-editor`**, joining `s`'s
    bypass, rather than the decoded text prompt. `n:café` is a plausible **and
    legal** filter, and `OverlayKey::Char` is **ASCII-only by construction**, so
    a decoded-key prompt cannot express it — see **F70**, where that same gap is
    a live defect in the `n` key. The editor also brings the paste rule in the
    same place as every other text field (**F68** satisfied without a second
    decision) and handles a multi-byte character **split across reads**, which
    byte-by-byte typing really produces. **F45**: a second line editor here
    would be a second parser of the same bytes.
  - **An invalid term is refused, the rows are left alone, and the line stays
    open**, reporting **the parser's own message**. The two rejected readings:
    *filtering to nothing* shows an empty view with nothing saying why — and in
    this view an empty view can mean failing to show the agent waiting on an
    answer; *closing the line* makes the user retype a filter that is one
    character wrong. An **empty** line clears the filter, since the empty filter
    already matches everything.
  - **The filter survives a regroup and does not survive a close.** It lives in
    the overlay's state, so repaints and an agent finishing keep it in force; a
    reopen starts fresh, so the view always opens showing everything — a filter
    outliving its screen would hide agents for a reason nothing on screen
    explains, and the keys would act inside that hidden state. **`Esc` cancels
    the line and leaves a filter already in force untouched:** cancelling is not
    clearing, or `Esc` would do two things at once and abandoning a half-typed
    term would lose the view you set up.

  **The filter grammar was under-specified; T4-S1 decided these four points and
  pinned each to an exact message.**
  - **`o:` was never defined here.** It matches the **task/objective**
    substring, while `n:` matches the **name** — each confined to its own field,
    asserted in both directions, so a name filter cannot match a task.
  - **`s:` had no vocabulary.** It is `needs-input`, `working`, `done-unviewed`,
    `done-viewed`, `failed`, `cancelled`, read back off the states' own wire
    spellings. `s:done` is an **error listing all six** rather than an alias for
    both done groups — strict plus discoverable, chosen to minimise invention; a
    friendlier alias is a defensible amendment.
  - **A malformed filter is an error, never ignored.** Unknown key, unkeyed bare
    text, a key with no value, an unknown state, and a repeated key are all
    rejected by name. The reasoning is specific to this view: a silently dropped
    term shows the **wrong agents**, and here that means failing to show the one
    waiting on an answer. Empty input matches everything.
  - **A repeated key is an error**, not last-wins.
- **A4** Peek panel MUST show the bounded redacted timeline that `/jobs show`
  shows today, the pending approval request if blocked, and the result if
  done. It MUST never show tool arguments or raw tool output (existing
  redaction rule).

  **Satisfied by construction, confirmed in T4-S3 — this is not a filter.** The
  snapshot's `waiting_for` is an `AgentApprovalKind`, a **unit enum**, so a path
  or argv word is **not representable**; timeline text is redacted at the source
  and the DTO's `validate` **rejects** any entry whose text is not already
  redacted; and approval entries are written as `approval to <kind> …`, kind
  only. So rendering the same snapshot satisfies the clause, and the panel reuses
  `/jobs show`'s own renderers — `format_agent_thread`, `pending_approval_request`,
  `peek_result` and `sanitize_terminal_text` — in the same order, rather than a
  second renderer that could drift. See **F40** for why the protection had to be
  made structural, and **F52**/**F47** for what the ring does and does not hold.

  A later reader should not add a scrubbing filter here: redaction deliberately
  leaves paths untouched, so a panel that scrubbed them wholesale would hide a
  legitimate result, and a test pins exactly that by requiring a result
  containing a path to show it.
- **A5** Attach MUST stream the agent's timeline live into the main viewport
  with an `⛩ attached to agent N` milestone, let the composer send steering
  (or an approval answer when blocked), and `←` on an empty composer or
  `/detach` return with a recap of events that happened while attached.
  Attaching never pauses the foreground turn; the foreground turn's output is
  buffered and replayed on detach.

  **The routed composer (T4-S6b, shipped) routes on submit-time state, with the
  registry as final adjudicator and no refusal that destroys text.**
  - **Blocked at submit → answer**, through the **one** shared
    `parse_permission_answer` (**F45** was closed by retiring a second answer
    parser; `r`'s decision read uses the same one). A line that parser rejects is
    **refused with nothing sent** — the agent stays blocked, which is the one
    state it can still be answered correctly from.
  - **Anything else → steer**, and `AgentRegistry::steer` adjudicates: an agent
    that blocked or finished in the gap refuses **in its own words** rather than
    being corrected or answered off a stale read.
  - **Every refusal sends nothing and hands the line back** as the next read's
    draft (**F76**), with the reason painted through the renderer's surface
    (**F71**). In the implementation "refused" and "keeps the line" are the
    **same boolean**, so they cannot drift.
  - **Open-time routing was rejected** because it can only ever be wrong — a
    steer the registry must refuse, or an answer taken from a stale block.
    Submit-time plus "a refusal keeps the text" makes the user's worst case **one
    extra keystroke with their words intact**, which is what stops the race being
    a design problem.
  - **A slash command stays a command while attached** (and a blank line stays
    blank). Otherwise an attached session could never leave by `/detach`.
  - **No new wording except the milestone**: `/jobs reply`'s own receipt on an
    answer, `/jobs steer`'s own on a steer, `r`'s own refusal when a correction
    reaches a blocked agent, and the registry's own `Display` for steer refusals.

  **The `⛩` attach milestone is painted here**, in the one seam both attach
  routes pass through, once per **change** of binding — nothing on a declined
  exit, nothing on a re-attach to the same agent. Its text **names the route**,
  not just the agent: *attached to agent N; your next line steers it, or answers
  it while it is blocked*. The reason is **F82**: this route adds **no key** — it
  changes what `Enter` means — so **the only thing that can enumerate it is the
  moment it begins.** A key table cannot advertise a route that has no key.

  **The inline composer does not share F72's unbracketed-paste gap** — checked,
  not assumed. `KeyFilter` owns the bracketed-paste state machine, so a paste
  arrives as **one draft** and a pasted multi-line correction routes as one
  line; F72 applies to the overlay's four bypass lines only. `parse_permission_answer`
  also strips paste markers, so a pasted `y` still answers.

  **T4-S6 was split into three, and A5 therefore lands in three pieces.**
  **T4-S6a** (shipped) is attachment itself and the keys that enter and leave
  it; **T4-S6b** is the routed composer, which needs a live thread with an open
  correction channel — the fixture problem that split T4-S4b; **T4-S6c** is the
  64 KiB buffer and the recap replay, a disjoint surface (the output sink, not
  key routing). The `⛩` milestones were deliberately **not** given a card of
  their own: over an existing `milestone_line` helper that would pin two format
  strings and prove nothing, and **F71** requires a visibility claim to go
  through the surface that paints it — so the attach milestone belongs with (b),
  where something can attach, and the recap milestone is (c)'s header.

  **`←` detaches on the first press, on an empty composer, and is not gated by
  `[tui] left_arrow_opens_agents`.** T4-S2 refused a durable armed state because
  it makes a keystroke's meaning depend on something the user did minutes ago
  **and can no longer see**; attachment is the opposite — it is on screen in
  front of the caret for as long as it lasts, so **the context is legible at the
  moment of the press.** The costs are asymmetric too: opening a full-screen
  overlay by accident interrupts you, while detaching by accident costs one
  keystroke and **stops nothing**. A non-empty draft keeps `←` as cursor
  movement. The config key is not consulted because A1 scopes it to "that route
  alone" — the agent view's — while this clause names `←` as the return path
  unconditionally. **`←←` in a single read is one detach and no view**: an
  implementation that counts presses before asking about attachment gets that
  wrong, and a test is named for it.

  **With nothing attached, `Ctrl+X d` is inert and silent while `/detach` is
  answered.** A chord is a slip; a submitted line is an intention with somewhere
  to put the answer (T2-S5b).

  **`Ctrl+X 1..9` is not shipped, and the reason is a constraint, not a
  schedule.** §4.7 says "attach agent N", but the digit **cannot be an id**:
  ids pass 9 after nine spawns, so a digit-keyed id binding would be dead for
  `#10` and — worse — **wrong** rather than dead once `#1` is pruned and `#11`
  exists. It must therefore be a **1-based ordinal over A2's display order**,
  which means resolving it against the registry at the composer's byte filter, a
  point that has no registry. That coupling is its own card.
- **A6** Dispatch from the agent view composer: plain text spawns a background
  agent with the default profile; `@<profile> <task>` selects a profile.
  Dispatch is admitted by the registry limits (`AgentRegistryLimits`,
  default 8 running / 128 records), **not** by the model's per-turn
  `SpawnBudget`, which exists to stop storming models.

  **Two things this clause got wrong, corrected in T4-S7.** It says *composer*,
  and the view has no composer — it has **modal edited lines**, so the dispatch
  is the view's **fourth** (beside `s`'s correction, `n`'s name and `/`'s
  filter). And it names **no key**, while A3's list was full, so one was chosen
  here: **`+`**.

  **`+` was picked for a property, not for taste.** It is `0x2b`, **outside the
  CSI final-byte range `0x40..=0x7e`** — the same property `/` was chosen for,
  and one that `r`, `s`, `n`, `p` and `x` do not have. That is **F65** (*an arrow
  key is not one byte*) applied to a new binding instead of rediscovered by it.
  The key is exported as `DISPATCH_KEY` so the help row, the overlay arm and the
  tests read one letter and no test spells the character.

  **The dispatch line inherits F72** with the other three: these lines take raw
  bytes on the byte bypass, so the decoder's unbracketed-burst fallback does not
  protect them, and on a terminal that does not bracket pastes a pasted task
  submits at its first newline. Recorded, not fixed here.

  **A dispatch does not move the selection**, on T4-S4a's safety argument: a
  silent move would repoint the next `x` or `p` at an agent the user never chose.

  **How "not the `SpawnBudget`" is actually satisfied:** by **not plumbing it**.
  All twelve of its references were enumerated, and **every consumer is a
  model-facing tool family calling `admit` as its first act** — nothing else in
  the tree consults it. So `view_dispatcher`'s signature is
  `spawn_agent_bg_tool_with_binder`'s **minus the budget**, and the claim is
  pinned in the **positive** form rather than by omission: one budget and one
  registry, where the model's spawn spends the turn's only credit, **the same
  handler is then refused**, and the view's dispatch still succeeds with the
  budget untouched.

  **The over-capacity refusal is painted on the dispatch frame** and the line
  stays open carrying the task, exactly as `n`'s and `/`'s refusals are
  (**F71**) — a refusal painted under the row list would be off-screen while a
  line is open.
- **A7** Viewing a done agent (peek or attach) MUST transition its handoff to
  `Acknowledged`, exactly as `/jobs show` does (`peek_result` +
  `acknowledge_result`), so the exit guard and strict-automation rules are
  unchanged.

  **Clarified in T4-S3, because "the exit guard" invites a wrong inference.**
  The guard on unviewed results **surfaces** them, it does not refuse the exit:
  on the way out the session prints `~ surfaced N previously unopened
  background result(s) before exit`. Only **running or blocked** agents refuse
  `/quit` (`exit blocked: N background agent(s) still running`). So peeking a
  done agent means its result is **not reprinted at exit**, and an unviewed one
  still is — that is the round trip, and it is what the tests pin. See **F64**:
  the name is accurate and long-established, but it implies refusal, and both a
  plan row and one of my briefs inferred refusal from it.

  **Peeking a blocked agent acknowledges nothing**, because there is no result
  to take custody of: `acknowledge_result` returns `NotReady` for a handoff of
  `Unavailable` and does not mutate. **Opening the view, and repainting its
  rows, acknowledge nothing either** — the peek is the single acknowledging call
  site. Whoever adds selection state must preserve that: moving over a row must
  not take custody of its result, or the guard stops meaning anything.

  **A failed agent is not exempt.** `AgentThreadState::finish` sets
  `handoff = Ready` for `Failed` and `Cancelled` exactly as for `Done`, and the
  guard keys on the **handoff**, never the phase.
- **A8** The ambient counter (R5) MUST update within 250 ms of a registry
  change in TUI modes. In `classic` it refreshes on the live status row once
  per input poll tick while a turn runs, and the idle prompt keeps reporting
  at the next prompt cycle, which is the pre-existing notice behavior.

  **This clause was violated at the TUI idle composer until T4-S8, and the
  carve-out in its second sentence is why nobody noticed.** That latitude is
  granted to **`classic`** by its own wording; the TUI idle composer never had
  it. Yet `sync_agent_counts` and `repaint_status_row` were called from **one
  loop only** — the *during-a-turn* typeahead — while the idle read blocks with
  **no timeout**, waking only on a resize and then explicitly retrying *without
  a frame*. The idle row **does** carry the ambient tag, so **a background agent
  that blocked while the user sat at the prompt changed nothing on screen until
  a keystroke.** That is what R5 and the whole T2 blocked-agent surfacing exist
  to prevent.

  **The argument against it was already in the tree**, which is the part worth
  remembering. The recorded rationale for having no idle cadence says the idle
  prompt *"has nothing to tick"* — **true of the elapsed clock, false of the
  ambient tag** — while the reduced-motion logic states the opposite case
  exactly: these tags stay visible *"because they are state and not animation:
  freezing them would hide a blocked agent."* Same reasoning, same segment,
  applied to reduced motion and not to idle. **A justification true of one
  segment is not true of the row.**

  **The fix is a subscriber, not a poll.** `registry.subscribe()` wakes a task
  that repaints the row; the counts are already a pull, so nothing is pushed and
  the composer stays blocked in `read()`. A read timeout was rejected because
  `read_line_inline_with_keys` is generic over `R: Read` and driven by in-memory
  cursors in tests, so giving it an fd would mean changing the signature of the
  one function that owns the decoder and the editor lines' byte bypass (**F68**,
  **F72**). **A bare wake paints no frame**: the step compares the projected
  counts against what it last showed and returns without painting when they
  match, so reduced motion and N3's 1/60 s budget are untouched.

  **`AMBIENT_TICK_POLL_MS` and `AMBIENT_LATENCY_BUDGET` now put A8's numbers in
  the suite**, so raising the during-turn tick past 250 ms **fails by name** —
  **F50**'s loud guard, which that finding proposed against a constant that did
  not yet exist.

  **Still open (not A8's fault, but adjacent):** mid-turn, the approval arm
  `continue`s **before** the counts are re-pulled, so while an approval modal is
  up the counter freezes and that modal's own row shows a stale count. Carded,
  and deliberately untested: the new cadence may refresh it **incidentally**, and
  an accidental fix is not a tested one.

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

  **Shipped in H2-title, with two deviations stated rather than hidden.** The
  state-file clause was already built; the title clause had been specified since
  this epic began and **never implemented** — `tui_bridge` pushed and popped the
  title *stack* in production **without consulting `tui.title`**, and the OSC 2
  string was written nowhere, because the only code that would write it has no
  non-test caller (deliberately: **F93**, routing through it also enables raw mode
  and would staircase several hundred direct writers). So `tui.title` gated
  **neither** half — an **unimplemented requirement wearing a config key**, which
  **F139** records as a different diagnosis from a key whose referent was never
  specified.

  **Rendering, which H2 left as alternation:** `kitsu · <state> · <N> agent[s]`.
  The count is **never omitted at zero** — H2's audience is a parser, and a title
  that changes shape at zero makes *"no agents"* and *"stale title"*
  indistinguishable from outside; `1 agent` is singular because people read it
  too. The state word comes from `agent_state_file::process_state` **verbatim**, so
  the *blocked outranks working* precedence stays in one home.

  **Deviation 1 — cadence.** H2 specifies the *state file* updates "on every
  registry generation change". **The title does not**: the registry's watch channel
  is subscribed only behind an off-by-default environment variable, so there is no
  general change hook in a chat session. The title rides the three
  `publish_agent_state` sites — session boundary, idle prompt, turn start — and is
  therefore **accurate at the prompt and at the start of a turn, and can go stale
  during one.** Closing that needs a change signal this design does not currently
  provide.

  **Deviation 2 — closed by H2-stack, with two residuals.** H2-title shipped with
  the title outliving the process outside fullscreen, because `enter_fullscreen` was
  the only thing that pushed the title stack. **H2-stack makes the push belong to
  the session**: one push when the session first sets a title, one pop when it ends,
  and `enter_fullscreen` pushes only when the session has not — which also makes the
  dying-signal restore's long-unconditional **pop** the balancing one rather than a
  pop against an empty stack.

  **That design was forced, not chosen.** An existing test pins `PUSH == 1` **and**
  `POP == 1` across a fullscreen→classic switch, so a session push plus an
  unconditional fullscreen push is two and two and fails it. **And the session push
  must carry the surface gate, not only the key gate** — the trap version, gated on
  the key alone, **passes every pre-existing negative**, because on a plain
  fullscreen surface a session push merely *replaces* the segment push and the count
  stays one. It took a new classic-surface guard to make that visible at all.

  **Residual 1:** under a **multiplexer** the title still outlives the process, in
  every mode. The sequences are dropped, OSC 2 has no query form, and the old title
  therefore cannot be read back and restored by hand. H2 names tmux as the audience,
  so this is accepted rather than fixed.

  **Residual 2:** a styled **`classic`** session killed by a **signal** still leaks
  its title, because classic installs no dying-signal handler — deliberately, and
  pinned by a test asserting that a classic session emits no viewport bytes.
  Closing it would trade a guarantee for a cosmetic one.

  **Unpinned byte-order consequence:** tuple bindings drop in reverse declaration
  order, so on a styled exit *from* fullscreen the session pop is written before the
  segment's leave-alt-screen. The segment's own pop is suppressed in that case, so
  the counts hold at one and one, and **no test pins the interleaving.**
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

  **Shipped in T5-S6, which settled two things H5 left open.**

  **`done` is the registry's `terminal` predicate, not the `Done` variant** --
  `!matches!(phase, Running | Blocked)`. Had it been the variant alone, a `done`
  wait on an agent that **failed or was cancelled** would have burned the whole
  timeout waiting for a state that can never arrive. By the same reasoning the
  converse returns at once: waiting for `blocked` on an agent that has gone
  terminal is unreachable, so the tool says so instead of holding the round open.
  A `done` wait on a *parked* agent does run its timeout out, because `blocked` is
  non-terminal and the agent is still live.

  **"Bounded by `--max-rounds`" is a conjunction of two independent bounds, not
  something the tool enforces.** `max_rounds` lives only on
  `kitsu_core::AgentConfig` and is enforced only in the orchestrator loop; a tool
  handler cannot see it. So the timeout bounds **one call** and `--max-rounds`
  bounds **how many calls**, since each `agent_wait` is one round of the caller's
  turn. Worst-case turn time is `max_rounds × 30 min`, which is why the clamp has
  a ceiling. The round cap is already covered by `kitsu-core`'s own test, and
  T5-S6 deliberately wrote **no** assertion for it: any test in the tool's file
  would assert the **absence of a parameter**, which is structural and vacuous.

  **A timeout is an `Ok` outcome, not an error**, naming the state not reached, the
  timeout actually used, and the agent's current status. An unknown id *is* an
  error and is returned immediately, because a typo must not cost a round. And a
  **zero spawn cap unregisters it** -- not because it consumes budget (it does not)
  but because at a zero cap no agent can exist to wait on, and a tool that cannot
  succeed must not be advertised.

### 3.5 Non-functional and invariants (N)

- **N1** Non-TTY, `--plain`/`NO_COLOR`, assistive (`--screen-reader`), and
  JSONL output MUST stay byte-identical to today.

  **Clarified in F120, after I misread it as a blanket ban.** N1 lists four
  surfaces; **an interactive TTY run is not among them**, so a human-facing line
  on a TTY is not an N1 question at all. And for the four it does list, N1
  constrains **the bytes of the shapes that exist today**, not the set of shapes:
  **a new additive event `"type"`, emitted only on a path that is currently
  wrong, leaves every existing event byte-identical.** That is how
  `tool_results_dropped` ships without violating it, and it is strictly less
  exposure than mutating `turn_finished`/`final_summary` or touching `status`.

  The reading has a limit worth stating: a consumer asserting *"exactly these
  types appear"* is affected. That is accepted deliberately — **a run that
  currently drops a tool result and reports success is not a shape N1 was written
  to protect**, and the alternative is preserving a lie for byte-stability. The
  conditional-field precedent (`final_summary` adds `output_last_message_state`
  only when `Some`) remains the fallback if a field is ever preferred to an event.

  **Clarified again in F122: which descriptor does "non-TTY" mean?** N1 did not say,
  and it matters — the codebase distinguishes them with **separate functions**
  (`is_tty()` is `io::stdout().is_terminal()`; `stderr_is_tty()` is its own), and
  the three inline composites in the binary all key on **stdin** while N1's own
  parenthetical says *"stdout is a TTY"*. Those differ for a real invocation such
  as `kitsu chat <prompt> | tee`.

  **The resolution follows from what N1 protects, which is consumers**, and a
  consumer can redirect either descriptor independently. So **"non-TTY" means: for
  each descriptor, when *that* descriptor is not a terminal, *its* bytes stay
  identical.** It is not one global condition.

  **Consequently a human-facing line, which goes to stderr, is gated on
  `stderr_styled() && is_tty()`** — both descriptors being terminals. That is
  **stricter than the minimum** the paragraph above requires (stderr alone would
  do), and deliberately so: a run with stdout redirected is being consumed by
  something, and a mid-turn human notice is noise to it. The looser gate can be
  adopted later if a case appears; the stricter one cannot break a consumer that
  exists today. The TUI is entered only when
  `stderr_styled() && stdout is a TTY && !plain_mode() && !assistive && !jsonl`.
- **N2** Idle input latency MUST not regress: paste of 10 KiB into the
  composer completes in under 100 ms (the rustyline `ExternalPrinter` lesson).
  Measured by a benchmark, not a CI gate (see Stage 2).
- **N3** Frame budget: coalesce redraws to ≤ 60 fps; use synchronized output
  (`CSI ?2026 h/l`) when the terminal supports it; never redraw history.

  **Amended in T3-S12 on two points, because the original said what but not
  where.**

  **(a) The budget is enforced in the painter, not inherited from a caller.**
  Before this card it held only because one caller slept 50 ms between ticks.
  Measured: a loop moving a single **non-elapsed** field defeated the
  byte-identity dedupe entirely and reached roughly **109 000 frames per second
  in debug and 400 000 in release**. After the fix both profiles sit flat at
  **60**. That matters because the epic had already grown a **second** author of
  this row (`clear_approval_modal`), and a budget that lives in one caller's
  sleep is not a budget.

  **Coalescing means collapse the intermediate frames and keep the latest —
  never discard what arrived during the window.** The implementation records the
  dedupe key only when it actually paints, so a suppressed call consumes
  nothing and the next call past the window recomposes from the fields **as they
  stand then**. Nothing is latched and no timer thread exists. The dedupe is
  checked **before** the window, so an unchanged row still costs zero bytes and
  does not burn a frame.

  **(b) Synchronized output is DEFERRED, with the reason recorded.** Mode 2026
  can only be detected in band with DECRQM and **reading the reply on stdin** —
  which the input loop owns in raw mode while also serving approval reads. A
  probe that waits for a report that may never arrive is a **hang**; with a
  timeout it is a **sleep on the first frame**; emitting the mode blind leaves
  `?2026h` as **visible garbage** on terminals that do not implement it. The row
  is one line, so tearing is not a live defect. Two tests pin that the bytes are
  **never emitted**, each documenting the test to write once a probe exists:
  emitted after a positive report, never without one.
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

  **What the ring is a record of — never stated until T5-S2 had to measure it
  (F92).** It is the **streamed** conversation, and **not**: the binary's own
  diagnostics, receipts or command errors (`eprintln!` expands to
  `std::eprintln!` and goes straight to fd 2), nor a **buffered** answer
  (`print_message` writes directly to stdout inside the live-row suspension).
  Because text is buffered rather than streamed **whenever tools are advertised
  that round**, and interactive chat advertises six by default, **what the
  scrollback contains depends on whether the model was offered tools** — the same
  answer is in the ring or not accordingly. That is the fact a user scrolling
  back in fullscreen meets, and it sharpens **F51** from the other side: that
  finding recorded buffering as a decision about *visible* behaviour, and it also
  decides what the scrollback **holds**.

  **Routing the binary's diagnostics into the ring was considered and rejected**
  (**F92**): it looks like a one-place macro change and would re-route several
  hundred call sites, change **inline** line endings and **classic** bytes,
  reorder diagnostics against `println!`, and make the ring far fuller than the
  sizing here assumed — with **309 tests** pinning those bytes. It is a
  requirements change about what the ring records, disguised as a wiring fix.

  **The reader lands before the feed (T5-S1, F86).** This clause says the ring is
  not fed until something reads it — so the **order of work follows from the
  requirement**: the fullscreen view (T5-S1a) ships first and is the reader;
  making the ring live (T5-S1b) comes second. Shipping the feed first would spend
  up to ten thousand styled lines during the window before a reader existed —
  **the exact expenditure this clause forbids.**

  **One byte-level claim *is* testable, and one is not.** "Retained bytes ≤ K for
  a fixed K" needs the per-line clamp F52 declines, and an **RSS** assertion is
  worse than untestable — it is *false*, since one pathological line defeats it by
  design, and it is environment-dependent besides. But **the sum of the retained
  lines' own byte lengths is identical after 10 000 pushes and after 5 000** —
  computed from the ring's contents rather than the allocator, so deterministic
  and parallelism-independent. That pins what RSS was reaching for: **the evicted
  lines' bytes are not retained anywhere.** Recorded because "no byte-level claim
  is testable" was an overcorrection of mine (**F85**).

  **The ring is an opt-in field on the TUI backend, not a process global**, and
  `classic` cannot construct one: the renderer crate has no dependency path to
  the viewport crate, which makes "classic pays nothing for this" a
  **compile-time** property rather than a runtime guard. The ring is therefore
  **not fed in production until R6's overlay reads it** — feeding an unread ring
  would spend up to ten thousand styled lines of memory for no benefit.

  **The limitation list, added in T3-S10, in descending severity.** N4 calls the
  ring "the sole source"; these are the ways that is not yet literally true.

  1. **Output that bypasses the surface is permanently absent from the ring, and
     this is accepted.** There are **421** direct `eprintln!`/`println!` sites in
     `main.rs`: about **251** are chat-path output a user could reasonably expect
     in the overlay (`handle_slash` 114, `run_chat` 100, `handle_jobs_command`
     37), and about **132** are on paths where no viewport can exist at all (the
     subcommands and scripted runners), so those can never be a gap. **F17**
     established this is visually harmless — a direct write inherits the parked
     cursor and the terminal scrolls the region whoever wrote the bytes — so it
     is a **data-completeness** gap, not a corruption one. `/export` is the route
     to what the overlay cannot show, the same escape hatch this requirement
     already names for evicted content.

     **No test asserts the count**, deliberately: a `== 421` assertion fails on
     every new diagnostic, which is noise rather than protection, and it would
     make the 132 irrelevant sites indistinguishable from the 251 that matter.
     The number belongs here; the test belongs on the mechanism.

     The cheap lever, when R6 gives completeness a reader, is to **wrap the slash
     dispatcher** — one site covering 114 of them — and it should be spent then.

  2. **A block that ends without a newline is retained as a short ring line**,
     not held back until terminated. The ring is the sole record, so holding a
     partial back means text already on the terminal is missing from the overlay,
     and a final partial never terminated would be lost permanently — exactly
     the shape of **F47**. Retaining it short can split one logical line into two
     entries: visible, recoverable, never lost.

  3. **(Fixed in T3-S10, recorded because it was nearly shipped.)**
     `write_block` went *through* the seam and fed the ring **nothing**, because
     it overrides a trait default that would have forwarded each line to the fed
     path. It carries the **streamed assistant reply** and every `! error:`
     notice, so the ring would have held every tool line and no assistant text —
     the exact inverse of the gap anyone was looking for.
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
  pass by construction, which is why none was written.

  **A third registration gap closed in T5-S1c-ii (F98).** The `/tui` switch
  **into** fullscreen called `enter_fullscreen` and an `eprintln!` **and nothing
  else** — so the **classic → fullscreen** route left the session holding the
  alternate screen on the **default signal disposition**. Every existing signal
  test entered fullscreen by a route that happened to have a handler already
  (from inline, or by starting in fullscreen per F96), so **the one route nobody
  had taken was the one with the gap.** There are now **three** registration
  sites — startup, the inline re-install, and the fullscreen switch — all
  `Once`-gated, so the later ones are no-ops.

  **The lesson, after three cards each finding one of these:** F94 taught that
  **exit paths are enumerable**. F98 is the other half — **the paths *into* a
  state are enumerable too, and a restore installed on entry is only as good as
  the entries that install it.**

  **Status as of F94+F96's card: every clause is met on every exit path, for both
  surfaces that take the alternate screen.** The dying-signal handler now chooses
  between two compile-time-composed constants on one `AtomicBool` load, so it
  gives the alternate screen back **only when it is held** — writing
  `ESC[?1049l` unconditionally was tried and **refused by measurement**, because
  it is not a no-op on the primary screen (it restores the cursor to its saved
  slot, so a `SIGTERM` in an ordinary session would drop the shell's next prompt
  on top of the user's transcript). The panic hook covers both surfaces,
  including a panic off the main thread, and clears the flag after writing so a
  take still yields exactly one leave.

  **Two holes were closed, and the second was worse than the first.** A dying
  signal left the **agent view's** alternate screen taken **since T4-S2** — five
  cards before fullscreen, which inherited the hole rather than introducing it.
  And a session **started** in fullscreen installed **no handler at all**
  (**F96**), because the handler was installed only where the pinned region was
  and `install` is deliberately inert in fullscreen: `Ctrl+C` there restored
  nothing, leaving the terminal in **raw mode** as well as the alternate screen.
  **That intersection of two individually correct decisions is what neither a
  per-card test nor a per-requirement review looks at.**

  **And "the handler is missing" turned out to be two defects with one symptom.**
  Installing it was not enough: `CANONICAL_TERMIOS` is published only by
  `RawStdinGuard::engage`, and a fullscreen-start session's raw mode is
  **rustyline's**, so the handler had nothing to restore. It now falls back to
  `tcgetattr` + re-enabling `ICANON|ECHO|ISIG` + `tcsetattr` — not the shell's
  full termios, which nothing in this process knows, but the difference between a
  usable shell and `reset` typed blind. Only a test asserting the **terminal's
  observable state** separated the two; the bytes were already right.

  **Superseded status note (T5-S2), kept for the record:**
  `install_exit_restore` gives back the **inline region only**, so a dying signal
  or a panic while fullscreen is active **leaves the alternate screen taken** and
  the user's terminal in it. This requirement demands restore on *every* exit
  path including panics and signals; that holds for inline and does not yet hold
  for fullscreen. **Carded, not quietly patched** — it is a real hole in a mode
  this epic now ships, and it is not what T5-S2's tests pin.

  **And why fullscreen does not go through `TuiRuntime` at all (F93).** Beyond
  the correction below: `enter` also calls **crossterm's `enable_raw_mode`, which
  clears `OPOST`**, and this binary has several hundred direct
  `println!`/`eprintln!` writers — the agent-view overlay's own comment already
  records that this would *"staircase all of them down the screen"*. It would
  also nest a second raw mode under `RawStdinGuard`, whose dying-signal restore
  snapshots termios. So the fullscreen segment writes the four sequences itself
  and leaves termios alone. **The general shape: a runtime that bundles two
  capabilities is unusable by a caller that wants one of them** — this
  requirement's title and alt-screen clauses looked like one lifecycle, and one
  of them drags termios with it.

  **Corrected again in T5-S2, because the narrowing was still wrong about the
  mechanism.** `TuiRuntime::enter` writes `PUSH_TITLE`, the optional OSC 2 title
  and raw mode — **it does not write `ESC[?1049h` and never has** (verified: that
  sequence appears only in `main.rs`, in the agent-view overlay, which has taken
  the alternate screen since T4-S2). So the runtime delivers **one** of this
  requirement's two clauses, the **title**, and **the alternate screen is code the
  switch writes.** A findings entry claiming otherwise stood for three cards and
  travelled into two briefs before an agent read the function body; see **F91**.

  **Narrowed in T5-S1a:** this previously said the clauses become real "when T5's
  fullscreen renderer uses that runtime". **It is not the renderer.** The
  fullscreen view is **pure by design** — it holds no terminal, writes nothing,
  and never enters an alternate screen; that purity is what lets auto-follow and
  the paging keys be asserted through painted rows instead of a pty (**F71**).
  The caller is **the mode switch** (T5-S2), which is the only thing that can
  select fullscreen and therefore the only thing that can call `enter`. Recorded
  because the renderer shipped without touching a terminal, and a reader of the
  old sentence would have gone looking for the alt screen in the wrong file.

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

  **Clarified in T3-S12, because this listed what stops and not what must
  keep going.** "Static" must not be read as "frozen". Under reduced motion the
  status row **does not repaint merely because time passed** — the elapsed
  segment's periodic refresh is the animation this requirement names. But the
  **queue tag and the ambient agent tag keep updating**, because those are
  **state, not animation**: `[2 blocked]` appearing is an event the user must
  act on, and suppressing it to make the row quiet would hide a blocked agent —
  defeating the very affordance R5 and A8 exist for.

  The implementation draws that line in the **dedupe key** rather than in what
  is painted: the key is the row recomposed with `elapsed: None`, so a
  clock-only change compares equal and no paint follows, while every other field
  still changes the key. The bytes written are always the real row **including**
  elapsed, so when a state change does paint, the age shown is current rather
  than stale by construction. **Reduced motion removes the periodic refresh, not
  the field.**

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
  the `/jobs` deck does, tinted per theme. `◉` is the glyph for *blocked* (it is
  the approval glyph, and a blocked agent **is** a pending approval; plain maps
  it to `o`).

  **Amended in the marker story, which closes F34 and F3.** This paragraph used
  to forbid reusing `◆`/`◇`/`◈` as row-state glyphs, "so plain mode never
  renders a row as `tool>` or `result>`" — a real hazard, since
  `ascii_structural_markers` maps `◆→*`, `◇→>`, `◈→#`, and `#` is exactly what
  a plain section header prints.

  **But the ban was the wrong instrument, and the shipped deck had already
  broken it**: `/jobs` has used `◈` for result-ready rows since D1. The hazard
  **cannot occur through `deck_marker`**, because it checks `plain` **first**
  and returns a bracketed ASCII token (`[blk]`, `[run]`, `[rdy]`, `[err]`,
  `[stp]`) — so a row glyph never reaches the mapper at all. The rule is
  enforced **structurally** rather than by prohibition.

  So the agent view and the `/jobs` deck share **one** marker vocabulary,
  `kitsu_render::deck_marker`, which the view reaches through
  `AgentViewState::deck_state()` — **one function, not two tables that agree
  today**, because two vocabularies for the same data is the drift this epic has
  five findings about. `◈` is consequently shared between a section header and a
  *done (unviewed)* row in the glyph tiers, deliberately, and disambiguated
  structurally: a header is unindented and word-only, a row is indented and
  starts `#<id>`.

  `DeckState::Blocked` exists as of that story — this paragraph previously
  asserted `kitsu-render` had no blocked state, which is no longer true — so a
  blocked agent is no longer rendered identically to a busy one. All four
  surfaces agree: `/jobs list`, `/jobs view`, `/jobs show` and `/jobs explain`
  print `◉` or `[blk]`.

  Any new glyph must still be added to `docs/ux-design.md` and
  `ascii_structural_markers` in the same change.

### 4.5 Attach semantics

Attach does not move the agent to the foreground or take its stdin; it is a
**view** plus a routed composer. While attached:

- composer text → `steer(id, text)`; if the agent is blocked, the composer
  offers the option keys first (`y/a/n`) and free text as a steering message.
- foreground orchestrator output is buffered (bounded, 64 KiB; overflow noted)
  and replayed with a `⛩ detached from agent N — recap:` milestone on return.

  **T4-S6c settled three things this clause left open, and completes A5.**

  - **The milestone is the recap's *header*, and is suppressed when nothing was
    buffered.** A detach with an empty buffer paints **nothing at all** — not a
    bare header. Three reasons: the clause itself makes the milestone the thing
    the recap is *"replayed with"*; §4.5 separately settles that **the two keys
    are silent and the command answers**, so a bare milestone on `←`/`Ctrl+X d`
    would break that silence rule; and a header ending in a colon with nothing
    under it reads as a bug. The code returns before painting the header, so the
    naive "header, then lines" shape does not exist.
  - **The bound is bytes, and deliberately NOT the line-bounded cell ring.**
    **F52** is the finding that *"N4 said memory and specified lines; a single
    huge line is still unbounded"*, and the ring has no byte accounting anywhere.
    A **structural** reason settles it independently: the buffer decorates
    `Surface`, which lives in `kitsu-render`, while `CellRing` lives in
    `kitsu-tui` — and **`kitsu-render` does not depend on `kitsu-tui`**, so the
    ring is unreachable from it however it is bounded. **`cell_ring_lines` still
    has no reader and F52 stays open** for the overlay and the `[` dump; it is
    decided only for this buffer.
  - **Overflow is *marked*, not merely bounded.** The note is the recap's
    **first** entry and **counts the lines the bound cost**; eviction is
    oldest-first, so the gap is at the top where the note sits and F52's
    "silently eats the middle" cannot happen. Pinned at **two** levels, and the
    painted one is the point (**F71**): *a recap that computed the note and
    replayed only the survivors would pass every unit test and still show the
    user an unmarked hole.*
  - **A clamped styled line is cut on a `char` boundary and ends with a reset** —
    F52's *second* caution, which a **byte** bound makes real for the first time
    in this epic. It is `fit_row`'s existing rule. The test caught a prototype
    that cut on a boundary without resetting, which would have coloured the rest
    of the session.

  **A fourth way an attachment ends, which no key triggers (F84).** The clause
  names `←` and `/detach`, and `Ctrl+X d` arrived in T4-S6a — but **re-binding to
  a different agent through the view ends an attachment with no detach key at
  all.** The previous agent's recap is replayed **under that agent's own header,
  before the new agent's attach milestone**, and **order is the assertion**: the
  other order **attributes one agent's turn to another**. All four transitions
  call one replay.

  **Attaching never pauses the foreground turn**, and the buffer cannot make it:
  the bound is held by **evicting**, never by refusing a write; there is no
  channel to signal "wait" through (`transcript_line` returns `()`); and the
  displaced surface is never called from any `Surface` method.
- `Ctrl+C` keeps the existing editor rule: a non-empty draft is cleared; on an
  empty draft it **detaches** (it never stops the agent — stopping is explicit
  via `x` in the view or `/jobs stop`). `Ctrl+Z` keeps its existing meaning
  (suspend kitsu) and is not a detach key.

  **`Ctrl+X d` carries the draft; `←` does not need to (F76).** The two routes
  are **not interchangeable**, and the asymmetry is the point: `←` detaches only
  on an **empty** composer, so with text typed it is cursor movement and the
  chord is the **only** route a half-written line has. `InlineRead::Detached`
  therefore carries the composer's contents, and the loop replays them into the
  next read.

  The variant's doc comment used to say the draft was not carried back *"because
  this outcome is only ever reached on an empty one"* — **true of `←`, false of
  the chord.** A variant documented by the narrowest route that reaches it: the
  comment was accurate when written and became wrong when a second caller
  arrived, which is **F69**'s and **F70**'s mechanism one level down, in the type
  system. The cheap alternative — giving the chord an empty-composer precondition
  so the comment became true — was **rejected**, because with an empty composer
  the chord is **already** redundant (`←` detaches on the first press and is not
  gated by `[tui] left_arrow_opens_agents`), so narrowing it would have left the
  chord with no case of its own and the non-empty case with no route at all.

  **`/detach` is wired (F77, F79).** It answers `detached from agent N; it keeps
  running` and records `{"kind":"detach","agent":N}`; with nothing attached it
  answers `not attached to a background agent` as an error. **The division is
  deliberate: the two keys are silent and the command answers.** `←` and
  `Ctrl+X d` deliver their feedback where the eye already is — the prompt label
  changing under the caret — while a submitted line moves the eye into the scroll
  region, which is the surface with somewhere to put a reply (T2-S5b). The
  success line repeats the "stops nothing" clause because it is the last moment
  a user who read `/detach` as "cancel the agent" can be told otherwise.

  **F79, fixed in the same card:** `/jobs view` kept only the overlay's `shown`
  and **discarded the attachment**, so attaching from that route silently
  attached to nothing while the chord and arrow routes worked. Attachment now
  works from every route into the view. The lesson is recorded in F79: **when a
  function starts returning something new, every caller is a suspect, and callers
  are enumerable** — this one changed from `-> bool` to `-> AgentViewExit` and
  had exactly two callers.

  **Superseded note (F77, kept for the record):** Its arm is
  unconditional — it always answers `not attached to a background agent` —
  because the slash dispatcher has no access to the composer's session binding.
  The arm's own comment says *"nothing in this card can attach"*, which was
  **true when written**, before the rework that made attachment work; it was
  never revisited. Wiring it means threading that binding through
  `handle_slash_command`'s call sites, which is a signature change and its own
  card. `←` on an empty composer and `Ctrl+X d` both work today. **The lesson,
  recorded in F77: when a rework makes something possible that was previously
  impossible, every arm written while it was impossible is suspect.**

  **The detach half of the `Ctrl+C` clause is deliberately NOT shipped, pending F54.**
  The clear-draft half already exists in `kitsu-editor` (`0x03` clears a
  non-empty buffer and emits `Interrupt` on an empty one) and is untouched. But
  **F54** records that "`Ctrl+C` is swallowed at an approval prompt" is
  **measured and unexplained** — most likely a harness artefact, since the child
  is probably not in the pty's foreground process group, and an echoed `^C` with
  no signal delivered is exactly that signature, while the code comment at the
  approval handoff says `ISIG` is restored. **Binding the detach half would mean
  writing product behaviour on top of an unresolved measurement of the one byte
  whose signal path is in doubt.** One minute in a real terminal settles it;
  until someone has that minute, this clause describes an intention and not the
  product. Two cards in a row were told not to map `\x03`, and did not.
- **T4-S6-status settled four things §4.5 did not state.**
  - **Position:** the attachment is the **leftmost content segment** —
    `attached · agent N · <model> · <meter> · <elapsed> [queued] [tag]`.
  - **Claim priority: second**, right after the ambient tag and above the queue
    tag, the model, elapsed and the meter. The full drop order is now
    `meter, elapsed, model, [N queued], attached · agent N, ambient tag`.
  - **A repaint keeps it.** The row is recomposed from the source on every paint
    and never remembered, so a segment lost on the next frame would make this
    section's legibility argument for `←` hold only until the next keystroke.
    Pinned both ways: a repaint caused by *something else* (the queue depth
    moving) must still carry the attachment, and composing twice from the same
    binding yields the same row — **composed, not consumed**.
  - **An agent finishing while attached does NOT detach.** The segment is
    derived from the user's binding and the registry is never consulted, which
    follows A5's ruling that leaving the view is not detaching. The alternative
    is worse: the prompt label comes off the **same** binding, so a
    self-detaching row would disagree with the caret in front of the user.

  **The segment is derived at paint time, never cached** (**F75**'s lesson):
  `StatusSource::fields()` pulls it through the same closure as the queue and
  blocked counts, so a detach needs no notification — clearing the composer's
  binding *is* the whole of it, and the next composition reads nothing. A
  snapshot stored anywhere would need telling on detach, and the detach path
  offers **no publication point** to tell it from.

  **F78: the row's drop order is not monotonic in width.** A clipped model
  segment **reappears at 44–42 columns** once the row is busier, because each
  segment is offered what the higher-ranked ones left and dropping the
  thirteen-column queue tag frees room. No special case was added to "fix" this,
  and **no future card should assert a prefix or no-revive property on this
  row.**

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

**Three bindings this section promises do not exist (recorded in T4-S10).** There
is no `Ctrl+X o`, no `Ctrl+X ?` and no `Ctrl+X 1..9`: the composer's chord
decoder binds exactly **two** letters, `a` (open the agent view) and `d`
(detach). `Ctrl+X 1..9` is carded separately and cannot be an id — ids pass 9,
and go *wrong* rather than dead once early agents are pruned — so it must be an
ordinal over the display order. **`Ctrl+L` does exist**, in both halves, citing
R8.

So this section's sentence about one table consumed by the help overlay and
`/help keys` is satisfied today by **exactly those two readers**, and should not
be read as promising a third surface. **The chords are deliberately not in that
table yet**: they never reach the overlay's dispatch — a different decoder on a
different surface — so T4-S10's keyboard sweep, which is the only thing making
the table more than a literal, cannot reach them. Their labels are also not
static, since the prefix is configurable, so a chord row's key column is
computed rather than written. Folding them in would buy one table and lose the
guard on half of it.

### 4.8 Configuration

`Config` is `#[serde(deny_unknown_fields)]` and flat today (`agent_spawn_cap`,
`theme`, `gateways`). The new sections are optional nested structs, each also
`deny_unknown_fields`:

```toml
[tui]
mode = "inline"             # classic | inline | fullscreen
alternate_screen = "auto"   # auto | always; `never` recognised but refused (F62)
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
`background_blocked` under JSONL (**closed in ADR-2**: its own event, never a
new `status` value — see `docs/roadmap/cycles/e11/decisions.md`).
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

### Requirement traceability (derived, T5-S8)

Generated by matching each requirement id the design **defines** against every plan row that
**cites** it. All **35** defined requirements are cited by at least one row — derived, not asserted.

**There is deliberately no “shipped” column.** Status and citations live in *different tables* in
the plan, and the status vocabulary collides with domain vocabulary — `T2-S1`'s detail row contains
the word *Blocked* as a **request state**, not a status. A regex over the plan's prose reported **17
requirements unshipped**, which is false. Requirement→row is derivable; requirement→status is not,
and a mechanically-derived status column would have been a fabrication (F138).

| requirement | cited by plan rows |
|---|---|
| **A1** | `T1-S3`, `T3-S5`, `T4-S2` |
| **A2** | `T1-S3`, `T3`, `T4-S1`, `T4-S6-ordinal` |
| **A3** | `T3`, `T4-S1`, `T4-S4`, `T4-S4a`, `T4-S6-ordinal` |
| **A4** | `T4-S3` |
| **A5** | `T4-S6`, `T4-S6c` |
| **A6** | `T4-S7` |
| **A7** | `T4-S3` |
| **A8** | `T1-S1`, `T1-S2`, `T4-S8` |
| **B1** | `T2-S1` |
| **B2** | `T2-S1`, `T2-S2` |
| **B3** | `T2-S4` |
| **B4** | `T2-S5`, `T2-S7` |
| **B5** | `T2-S3` |
| **H1** | `T1-S5`, `T2-S7`, `T5-S4` |
| **H2** | `T1-S4` |
| **H3** | `T1-S6`, `T2-S7` |
| **H4** | `T5-S7` |
| **H5** | `T5-S6` |
| **N1** | `F120`, `F122`, `F126`, `T1-S2`, `T1-S4`, `T2-S5` |
| **N2** | `R6-b`, `T3-S13` |
| **N3** | `T3-S12` |
| **N4** | `R6-prompts`, `T3-S10`, `T3-S6`, `T5-S1a`, `T5-S1b` |
| **N5** | `F94`, `T3-S8`, `T5-S1b`, `T5-S1c`, `T5-S1c-ii` |
| **N6** | `T3-S11`, `T5-S4` |
| **N7** | `T1-S6` |
| **N8** | `T2-S6` |
| **N9** | `T3-S12` |
| **R1** | `R1`, `T5-S1c-i`, `T5-S1c-ii`, `T5-S8` |
| **R2** | `R2`, `T3-S9`, `T5-S1b` |
| **R3** | `R3` |
| **R4** | `R4`, `T3-S3`, `T3-S4` |
| **R5** | `R5`, `T3-S5` |
| **R6** | `R6`, `R6-a`, `R6-b`, `R6-c`, `R6-d`, `R6-prompts`, `T3-S6`, `T4-S6c`, `T4-S9`, `T5-S3` |
| **R7** | `R7`, `T3-S7a`, `T3-S7c` |
| **R8** | `R8`, `T3-S8` |
