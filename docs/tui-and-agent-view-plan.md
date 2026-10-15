# Kitsu TUI and agent view — implementation plan

- **Status:** Draft v2 (revised after review `reviews/tui-plan-review-1.md`)
- **Date:** 2026-10-12
- **Branch:** `features/tui_herdr_improvements`
- **Design:** `docs/tui-and-agent-view-design.md` (v2.1). Requirement IDs
  (R*, A*, B*, H*, N*) below refer to it.
- **Delivery protocol:** `docs/roadmap/cycle-protocol.md`: a **card** is one
  30-minute cycle ending in one green commit and never spans two cycles. This
  plan is written at **story** granularity (2–8 cycles each) because that is
  the level at which scope and acceptance are stable; each story names its
  **first card** (the smallest failing test) and is sliced into further cards
  at execution time. Cycle records cite `E11 / T<n>-S<m> / C<k>`.
- **IDs:** epic **E11** (E10 is already "five-boundary certification" in
  `docs/roadmap/README.md`); milestones **T0–T5** (program milestones M0–M7
  are taken).
- **Gates (every card):** `cargo test --workspace --no-fail-fast` in **debug
  and release**; `cargo clippy --workspace --all-targets`; `cargo fmt
  --check`; the named invariant tests the card touches. CI
  (`.github/workflows/rust.yml`) runs only `cargo test` in both profiles on
  Ubuntu, so clippy/fmt/`--locked` are plan gates, not CI gates, until a
  separate CI card adds them. Dependency-touching cards add `cargo test
  --locked` and the MSRV check (§9). Milestone exits add the PTY matrix (§9)
  and, from T3, a Windows `cargo check`.
- **Do not cite `scripts/check-roadmap.sh`** as acceptance: its default base
  commit `a2ba2d5…` is not present in this repository (`git cat-file` fails),
  and it requires one cycle file per commit since that base, which this
  branch's merge history cannot satisfy. Cycle records follow the template in
  `cycle-protocol.md` instead.

The plan is a **proof of concept first**, then five milestones. The PoC is
time-boxed, invisible unless `KITSU_TUI=inline` is set, asks only renderer
questions, and ends with a go/no-go record. T1 (hooks, classic polish) and T2
(blocked agents) ship value even if the renderer PoC says no-go.

---

## 1. What the code gives us (baseline facts)

Checked on the branch; line numbers are from the current tree.

| Fact | Where | Consequence |
|---|---|---|
| `crates/kitsu` is **binary-only**: `[[bin]]`, no `src/lib.rs`; `typeahead_editor`, `subagent`, `attachments` are private `mod`s of `main.rs` | `crates/kitsu/Cargo.toml:15-17`, `main.rs:4-19` | A library crate `kitsu-tui` that `kitsu` depends on **cannot** import those types. The editor must move to a library crate first (T0-S1); agent rows and completion sources cross the boundary as plain structs/traits. |
| The orchestrator renders by calling `kitsu-render` globals: `print_fragment` (`orchestrator.rs:1730`), `tool_activity` (`:2051`), `tool_result` (`:2202`) | `crates/kitsu-core/src/orchestrator.rs` | Orchestrator output can be redirected behind a `Surface` seam without touching the orchestrator. |
| `main.rs` has **349 `eprintln!` and 43 `println!`** outside tests (`handle_jobs_command` alone 28) that write to the terminal directly; exactly one `main.rs` site uses `with_live_row_suspended` (`:13666`); `kitsu-render` has 8 closure callers of it and `StreamBuffer::push` writes straight to `io::stdout()` | `main.rs`, `lib.rs:388,418,1117,1154,1434,1463,1519,1668`, `lib.rs:219-234` | **This is the real seam risk.** Those writes run on the REPL thread between prompts (slash commands) or inside already-suspended notices. A pinned viewport must either wrap the slash dispatcher in the suspend/repaint protocol or keep a permanent scroll region so stray writes flow into scrollback. T0 measures this (Q3). |
| `with_live_row_suspended` is reentrant via `SUSPEND_DEPTH`; `PromptSafeAsyncNotice` nests it; prompters run synchronously under `spawn_blocking` | `live.rs:697-711`, `lib.rs:1117`, `permissions.rs` `async fn prompt` | A `Surface` lookup must never hold a lock across a `Surface` call (std `RwLock::read` may deadlock on recursion while a writer waits). |
| `TypeaheadEditor::feed(&mut self, bytes: &[u8])` is **byte-fed**; `main.rs:14243` feeds raw bytes; the collector enables bracketed paste (`:13819`) and yields stdin when `APPROVAL_INPUT_ACTIVE > 0` (`:13920, :14158, :14336`); `CliPrompter::ask` reads stdin directly via `read_approval_line` (`:19404`) | `typeahead_editor.rs:281`, `main.rs` | The TUI reads raw stdin bytes itself (crossterm for output/size only) **and** must honor `APPROVAL_INPUT_ACTIVE` or two readers race. The editor module has no `use crate::` imports, so it moves verbatim. |
| `/agents` alias arm forwards only an empty argument (`list`) or a numeric one (`show N`); anything else errors (`usage: /agents [id]`). `/agent` forwards its argument verbatim to `handle_jobs_command`. Mid-turn `/jobs` goes to `handle_active_jobs_command` (`list\|show\|steer\|stop`) | `main.rs:18728-18736`, `:18745-18752`, `:13720`, `:13556-13568` | New subcommands must land in **three** places: `handle_jobs_command`, `handle_active_jobs_command`, and the alias arm (plus the `SLASH_COMMANDS` spec rows at `:5194`, `:5202`). |
| PTY harness `run_plain_interactive_chat_pty` spawns the `kitsu` binary with `--plain --gateway example --no-stream`; an SSE streaming mock exists (`cli.rs:2791`); no terminal-emulator helper (`vt100`, `expectrl`) is in `Cargo.lock` | `crates/kitsu/tests/cli.rs:70-130` | In-process rendering tests need injectable sinks; real-PTY styled streaming tests are new work (T3-S1). |
| `AgentRegistry` built at `main.rs:15007`; `spawn_agent_bg_tool_with_binder` wired at `:15116` with `quiet = true` (`:15115`); `background_agent_notice` (`:10799`) consumed at `:16423`, `:16496`; `subscribe()` at `subagent.rs:706`; `rename`/`answer`/`tail_timeline` do not exist; private `attach` at `:681` | `main.rs`, `subagent.rs` | T1/T2/T4 registry additions are new methods, not edits. |
| `AgentRegistrySnapshot` is `Copy + Default`, built by struct literal at `subagent.rs:1185` and in tests `main.rs:21853-21881`; `AgentThreadSnapshot` literals at `thread.rs:700`, `main.rs:20632`; exhaustive matches in `agent_progress_status_label` (`main.rs:10844`), `format_jobs` (`:10909`), `format_agent_thread` (`:11095`) | `subagent.rs`, `thread.rs`, `main.rs` | Adding fields/variants breaks these sites; their files belong in the story's Files column. |
| Approval gates: `Permissions::resolve` `Mode::Ask` (`permissions.rs:1623`) and `CommandMode::Ask` (`command.rs:1687`), both calling `interactive_prompts_allowed()` (`kitsu-tools/src/lib.rs:61-65`, default `true` when unscoped). `scope_without_interactive_prompts` is used by `subagent.rs:1977` and five `semantic.rs` sites (`:136, :678, :702, :741, :775`); scope tests at `permissions.rs:2064`, `command.rs:3578` | `crates/kitsu-tools` | The prompt route must keep `scope_without_interactive_prompts` ⇒ denied for all seven callers; only the new registry route changes behavior. |
| `AgentThreadSnapshotDto::validate()` rejects any `schema_version != AGENT_THREAD_SCHEMA_VERSION` (`thread.rs:234-238`) | `thread.rs` | ~~A T2 reader (v2) would reject v1 state files written by a still-running T1 process~~ — **corrected in T2-S3: this conflated two independent schemas.** `agent_state_file.rs` has its own (`kitsu.agents.v1`) and never embeds the thread DTO, so bumping the thread schema cannot affect state files and there is no cross-process reader of the thread DTO at all. The thread DTO is emitted only in-process, into `/jobs --json`. T1-S5's "reader tolerates `schema_version ≤ current`" refers to the **state file's** schema, not this one. |
| `ratatui 0.30.2` requires Rust **1.88**; `0.29.0` requires 1.74; `crossterm 0.29` 1.63; `vt100 0.16` 1.70. **Corrected in T0-S2:** `vt100 0.16` is unusable here — it needs `unicode-width ^0.2.1` while `ratatui 0.29` pins `unicode-width =0.2.0` exactly, so the resolver refuses; `vt100 = "0.15"` resolves 0.15.2 with the same `Parser`/`screen` API the tests use. Workspace `resolver = "2"` ignores `rust-version`; MSRV is 1.80 (`Cargo.toml:14`); no 1.80 toolchain installed locally; release builds are `--locked` on linux/macOS/Windows (`release.yml`) | `cargo info`, `Cargo.toml:2,14`, `rustup toolchain list` | Pin `ratatui = "0.29"` and the crossterm it uses (0.28 at the time of writing; confirm one crossterm with `cargo tree -d`), `vt100 = "0.15"` as dev-dep (0.16 conflicts — see the previous cell). **Superseded by finding F9:** the 1.80 gate was never meetable — `clap 4.6`, `zeroize 1.9`, `indexmap 2.14`, `time 0.3.55` and others were already pinned at edition-2024 releases that Cargo 1.80 cannot parse, so the honest gate is that the new crate must not *further* raise the floor, checked against the resolved versions directly. |
| No `XDG_*`/`dirs` usage anywhere; only `host_home_dir()` (`permissions.rs:552`); roadmap invariants: "Config discovery … performs no implicit … state mutation" and "Optional integrations create no … state write … while disabled" | grep; `docs/roadmap/README.md:68-69, 76` | The state file must be interactive-only by default with explicit opt-in for automation, and needs macOS/Windows directory fallbacks and file permissions. |
| `SavedSession` (`session.rs:55-70`) persists `messages` plus id/name/model/project_context; no tool receipts or notices | `crates/kitsu/src/session.rs` | Transcript overlay and fullscreen read an in-memory cell ring; slash-command output that bypasses the seam is **not** in the ring (documented limitation until routed). |
| Tests: `kitsu-render/src/lib.rs` 43, `live.rs` 26, `typeahead_editor.rs` 39; `kitsu-render` has 41 top-level `pub fn` in `lib.rs` + 5 in `live.rs`; 16 live-region call sites outside `live.rs` (`with_live_row_suspended\|live_input_`), 19 including `live_queue_state`/`Spinner::start` | grep | Regression net exists; the extracted editor must carry its 39 tests. |
| Working tree: `crates/kitsu-tools/src/builtins/fs.rs` is **modified and uncommitted** and predates this work; the design/plan/review docs and two doc fixes are ours | `git status` | Per `docs/roadmap/README.md` Definition of Ready #6, the `fs.rs` change needs an ownership decision from the maintainer before T0 commits; it is never absorbed into E11 commits. |

---

## 2. Milestone map

```
T0  Renderer PoC (env-gated, time-boxed)     ── answers Q1..Q4, go/no-go record
 ├─ T1  Herding hooks + classic polish        ── ships regardless of T0
 ├─ T2  Blocked agents (permission-only)      ── ships regardless of T0
 └─ (go) T3  Inline renderer, release quality
            └─ T4  Agent view: peek, attach, dispatch
                  └─ T5  Fullscreen, agent_wait, herdr manifest, default flip
```

T1 and T2 run in parallel with T0 (different files). T2-S7 (hook surfacing)
follows T1. T3 starts only after the T0 **go** record. Each milestone ends with
an exit gate, a rollback statement, and a cycle record naming residual risk.

| Milestone | Design stage | Stories | Cycles (30 min) | Calendar |
|---|---|---|---|---|
| T0 PoC | — | 6 | 20–30 | 1.5–2.5 days, hard stop at 3 |
| T1 Hooks + classic polish | Stage 0 | 7 | 14–20 | ~1.5 days |
| T2 Blocked agents | Stage 1 | 7 | 20–28 | ~2 days |
| T3 Inline renderer | Stage 2 | 13 | 45–60 | 4–5 days |
| T4 Agent view | Stage 3 | 10 | 35–50 | 3.5–5 days |
| T5 Fullscreen + polish | Stage 4 | 8 | 20–28 | 2–2.5 days |

---

## 2b. Progress

Delivered on `features/tui_herdr_improvements`, newest last. Each story has a
record under `docs/roadmap/cycles/e11/`; out-of-scope discoveries are in
`findings.md` there.

| Story | State | Commit |
|---|---|---|
| T1-S1 reserved registry counts | done | `0ca9d99` |
| T1-S2 ambient live-row agent tag | done | `24c22a4` |
| T1-S3 `/jobs view` | done | `59b3bb8` |
| T1-S3 `/jobs explain` | done | `06b2508` |
| T1-S4 state file writer | done | `6fbaaa2` |
| T1-S4 config key and wiring | done | `eadf6ab` |
| T1-S5 `kitsu agents` reader | done | this commit |
| T1-S6 `on_agent_state` hook | done | this commit |
| T1-S7 docs sweep and records | folded into each story | — |
| T0-S1 editor extracted to its own crate | done | `7f3133c` |
| T0-S2 TUI crate and terminal lifecycle | done | this commit |
| T0-S3a `Surface` seam, stderr line writers | done | this commit |
| T0-S3b `write_block`, the suspension rule, `render_error` routed | done | this commit |
| T0-S3c writer threaded; the seam carries stdout | done | this commit |
| T0-S4 inline viewport, region flow and painting | done | this commit |
| T0-S5a F15 settled, gate and surface install | done | this commit |
| T0-S5b styled PTY harness and the Q3 measurement | done | this commit |
| T0-S5c fix the F18 teardown defect | done | this commit |
| T0-S5d idle input path | done | f9cdd75 |
| T0-S6 go/no-go record, F19 fix, Q4 measured | done | fabbee3 |
| T0-S7 Ctrl+Z suspend/resume (F21) | done | 516aaa6 |
| T0-S8 verify F20 claims, close F22, fix F23 flake | done | be95229 |
| T0-S9 the inline hint slot (search status, slash hint) | done | 85a8cab |
| T0-S10 Tab accepts the inline slash hint | done | 4a3840c |
| T0-S11 viewport resize, F16 resize half (F24, F25 raised) | done | 9a5e789 |
| T0-S12 end-to-end resize tests, F25 closed, F24 in CI | done | 3d087c8 |
| T0-S13 escape-aware row fitting for `paint`, F16 closed | done | this commit |
| T0-S14 variable viewport height, so the composer can grow | done | this commit |
| T0-S15a palette rows as data, built to the granted height | done | this commit |
| T0-S16 palette wiring, repaint-all, `on_resize` height, F29 refactor | done | c1aa851 |
| T0-S17 idle completion ranking, F30 closed | done | 7efe8ae |
| T0-S18 fork/flock fence race, F28 closed as a production bug | done | this commit |
| T0-S19 the three remaining flock sites, F32 closed | done | 187e120 |
| T2-S1 `PromptRoute` task-local seam | done | c4bd57b |
| T2-S3 `Blocked` vocabulary + validator + schema v2 | done, **taken before S2** | 68c7e54 |
| T2-S2 both gates consult the route (absorbs S1's gate half) | done | 14a9c85 |
| T2-S4 registry parks, answers, times out | done | cdac378 |
| T2-S5a `agents.blocking` + `block_timeout_secs` + spawn wiring | done | 57a2e90 |
| T2-S5b `/jobs reply` in all **seven** sites (F38), F2 closed by test | done | 13f12ea |
| T2-S6 approval entries in `/jobs show` and `/jobs explain`, argument-free by construction (F40) | done | 420be74 |
| T2-S7 blocked state on the state file, `on_agent_state` and `kitsu agents --json`, one machine vocabulary (F44, F45) | done | 74ae7f3 |
| F43 fix: a cancelled approval is recorded (was recorded as nothing); five of five resolutions | done | 6e843de |
| T3-S1 streaming/tool-calling PTY harness + 5 tests; found and fixed F47 (erased final line) | done | 90570eb |
| T3-S2 `[tui]` table (6 keys), `--tui`, `--no-alt-screen`, `KITSU_TUI` retired, doctor names a bad key | done | ea1d609 |
| T3-S3 one completion source for both hosts; `@file` in the inline composer; closes F20's list half, **not** the palette | done | 11cf2d1 |
| T3-S4 history sharing pinned by test; **no code needed**, plan row amended (F49) | done | 4d85e4a |
| T3-S5 `compose_status_row` + `Surface::queue` seam, drop order now in R5; row not yet wired | done | 8277767 |
| T3-S5b **wiring**: the live row replaces the `"kitsu"` placeholder, refreshed on the 50 ms tick (F51 noted) | done | bbf7570 |
| T3-S6 bounded fd-tagged cell ring in `kitsu-tui`; **not fed in production until R6 reads it**; N4 amended (F52) | done | 6c91f42 |
| T3-S7a approval modal in the bottom pane + inline receipt; R7 clarified, `--plain` excluded (F53) | done | 6584101 |
| T3-S7b in-loop approval read; answer off the transcript; F55 starvation fixed, **F45 closed** | done | b62e3ed |
| T3-S8 `Ctrl+L` + restore on SIGINT/SIGTERM/panic; N5 amended (two clauses vacuous); resize pinned, not rebuilt | done | e2654f4 |
| T3-S9 `/tui classic\|inline` runtime switch; R2 completed; F57 reachability, F58 unsatisfiable test | done | d4d213b |
| T3-S10 census answered (421, triaged); **`write_block` ring gap fixed**; F17 corrected, N4 limitation list | done | 4d567bf |
| T3-S11 Windows half measured (renderer crates clean; F59 corrects the premise); **matrix still needs a human** | partial | 6d22d79 |
| T3-S12 reduced motion reaches the viewport; 60 fps enforced **in the painter** (was ~109k-400k fps); sync output deferred with reason | done | eeaa51e |
| T3-S13 N2 measured (**5 ms vs 100 ms budget**); exit gate run clause by clause, **one clause has no test (F60)**; doc sweep | done | aae5874 |
| **T3 complete.** T4-S1 agent-view model: `AgentViewState` single source, filter grammar, DTO mapping; A2/A3 amended; F61 | done | 76bfc7c |
| marker story (between T4-S1 and T4-S2): `DeckState::Blocked`, one vocabulary via `deck_state()`; **F34 and F3 closed**, §4.4 amended | done | bd9d845 |
| T4-S2 agent view overlay opens/closes; `[tui] prefix` + `left_arrow_opens_agents` get their first consumer; A1's two stale clauses corrected | done | e791fa2 |
| T4-S3 peek (`Space`) reusing `/jobs show`'s renderers; A4 shown satisfied by construction, A7 clarified; **F64** (my second unverified premise) | done | 7da0ce8 |
| T4-S4a selection keyed on the agent, `↑/↓` clamping, `x` arming, `AgentRegistry::rename`, `p`'s taxonomy; A3 amended; **F65**, **F66** | done | this commit |
| T4-S4b-i action keys `r`/`x`/`n`/`p`; **rejected once for F68** (a paste executed itself as keys) and reworked with a paste-aware decoder | done | this commit |
| T4-S4b-ii `s`'s text sub-mode over `kitsu-editor`; the open line bypasses the overlay decoder so the editor's own paste rule applies; **F69** | done | this commit |
| F69 follow-up: the loop's input dispatch extracted to one `handle_overlay_input` the tests call; the mirror retired, `OverlayTextPrompt::Steer` deleted; **three** silent divergences erased | done | this commit |
| **F39 resolved** — the byte-identity flake made deterministic (11/20 failures standalone → 50 green runs), with a guard that fails loudly if the ordering ever inverts | done | this commit |
| T4-S5 the filter line (`/`) over `kitsu-editor`; the filter governs the selection's rows (a safety property); **F71** | done | this commit |
| **F70 fixed**: `n` reads its line through `kitsu-editor`; `OverlayPrompt::Text` and `OverlayTextPrompt` deleted, so no decoded key can fill a name; **F72** recorded | done | this commit |
| **F72**: an unbracketed paste submits early in all three editor lines (`s`, `/`, `n`) — the decoder's burst fallback does not reach them | **not started** | — |

> **`KITSU_TUI` is retired as of T3-S2.** Mentions of it in the T0-T2 rows and
> requirement text below are **historical** — they record how the preview was
> gated while those stories ran. The renderer is now selected by `--tui`, then
> `[tui] mode`, then the `classic` default, and no environment variable
> participates. Where an older row's acceptance says "env only", read it as
> satisfied at the time and superseded here.


With T1-S6 done, **milestone T1 is complete**: kitsu now exposes its
background agents to the outside three ways — a grouped and explainable deck
inside a session, an advisory state file plus a reader for anything outside the
process, and a push hook on every transition.

T0 has begun with its blocking prerequisite: the editor is now a library
crate, so the renderer crate planned in T0-S2 can use it.

T1 was taken before T0 deliberately: it is independent of the renderer and
ships value whatever the proof of concept concludes. T1-S7 as a separate
story is obsolete — documentation now lands in the same commit as its
feature, so there is nothing left for it to collect.

---

## 3. T0 — Renderer proof of concept

### Goal

Prove, in the real binary behind `KITSU_TUI=inline` (env only; no config,
no `/tui`, no docs), that a pinned viewport can coexist with kitsu's existing
output paths. Questions:

- **Q1 — Scrollback flow.** With a permanent DEC scroll region covering all
  rows above the viewport, does transcript text written **normally on its own
  fd** (stdout for assistant text, stderr for chrome) scroll into native
  scrollback in order, with no duplicates or lost lines, in xterm-class
  terminals and tmux? If a terminal does not push region-scrolled lines to
  scrollback, fall back to Codex-style per-cell `insert_history`. (R3, N6)
- **Q2 — ratatui coexistence.** Does ratatui `Viewport::Inline` tolerate the
  scroll region / manual repaint, or does its cursor bookkeeping break (Codex
  forked `Terminal`)? Fallback: paint the viewport manually (as `live.rs`
  does) and use ratatui only for alternate-screen overlays.
- **Q3 — Stray writers.** Wrapping the **slash dispatcher** once in the
  suspend/repaint protocol, plus the already-suspended async notices: does
  `/jobs`, `/help`, `/status` output at idle leave the viewport intact?
  Census the remaining unsuspended idle writers (there are 392 direct
  `eprintln!/println!` in `main.rs`; most are inside slash handlers). Record
  what is left and whether it is tolerable for T3.
- **Q4 — Input latency.** Raw-byte input through the extracted editor inside
  the TUI loop: 10 KiB paste < 100 ms (N2), with `APPROVAL_INPUT_ACTIVE`
  honored so a foreground approval prompt can still read stdin.

### Scope

In: editor extraction, one new crate, `Surface` seam, env gate, viewport,
status row with spinner only. Out: config, `/tui`, completion lists, approval
modal, resize reflow, agent overlay, blocked agents, hooks, docs, Windows.

### Stories

| Story | Deliverable | First card | Files | Acceptance |
|---|---|---|---|---|
| T0-S1 | Extract `typeahead_editor.rs` verbatim into new library crate `crates/kitsu-editor` (no new deps); `main.rs` uses `kitsu_editor::…` at its 20 reference sites | Move the file + fix paths; 39 editor tests green in the new crate | `Cargo.toml`, `crates/kitsu-editor/*`, `crates/kitsu/Cargo.toml`, `main.rs` | `cargo test -p kitsu-editor` 39 green; `cargo test -p kitsu` unchanged; no behavior change |
| T0-S2 | Add `crates/kitsu-tui` with `ratatui = "0.29"`, its crossterm (0.28), `vt100 = "0.15"` (dev — 0.16 conflicts with ratatui's exact `unicode-width` pin); `TuiRuntime::enter/leave` (raw mode, title set/restore, restore on `Drop`, `SIGINT`, panic hook) — nothing drawn yet. **Windows:** crate compiles everywhere but the bridge is `cfg(unix)` until T3 | Failing test: `leave()` idempotent and restore sequence emitted exactly once | `Cargo.toml`, `Cargo.lock`, `crates/kitsu-tui/*` | `cargo test --locked` both profiles; `cargo tree -d` shows one crossterm; the resolved `ratatui`/`crossterm`/`vt100` versions each require a Rust older than the workspace's declared floor, verified with `cargo info` (the `cargo +1.80.0` check itself is impossible — finding F9); build-time delta recorded in the commit |
| T0-S3a | `Fd` + a one-method `Surface` trait (`transcript_line(Fd, &str)`) in `kitsu-render`, `ClassicSurface` reproducing today's bytes, and the global slot whose accessor **returns a cloned `Arc` out of a short-lived read guard**, so a caller structurally cannot hold the lock across a `Surface` call; route the stderr line writers only (`tool_activity`, `tool_result_with_weight`, `milestone`) | Failing test: a recording `Surface` sees exactly the calls `tool_activity` makes today, with the expected text derived from `tool_activity_line` rather than hardcoded; plus two deadline-bounded reentrancy tests | `crates/kitsu-render/src/{lib,surface}.rs` | **Byte-identical**: all 82 render tests, 637 bin tests and 229 CLI tests unchanged; nested suspend inside a `Surface` call completes; a concurrent `set_surface` completes |
| T0-S3b | Route the **streaming stdout path** through the seam: `print_fragment` and `finish_line`, which own a line-buffering styler state machine, the `ASSISTANT_OUTPUT_WRITTEN` / `TURN_ASSISTANT_OUTPUT` atomics and byte-count returns; then the notice path, and **`render_error`, which needs a `transcript_lines(Fd, &[String])` method or an explicit suspension-group API**: its body prints N lines under ONE suspension, so routing it per line would open N suspensions and, with a drawn row, change the emitted ANSI bytes — the T0-S3a test author caught this and correctly refused to route it. Split out of T0-S3 deliberately — bundling it with the easy writers would have risked byte drift in exactly the code where byte-identity matters most | Failing test: a recording `Surface` sees one call per completed line, and the partial-line and turn-boundary semantics are preserved | `crates/kitsu-render/src/{lib,surface}.rs` | Byte-identical across the same three suites; the streaming and buffered paths still agree; the two atomics still gate the trailing newline |
| T0-S3c | **Thread the block's writer through `StreamBuffer::push` and `flush_unstyled`** so the seam can actually carry stdout. Finding F12: those methods write `io::stdout()` themselves, so after T0-S3b a backend receives an *empty* block for a completed assistant line and only the trailing newline from `finish_line`. Byte-identical for `ClassicSurface`, and therefore invisible — the routing makes the path look handled. ~~T0-S4 and T0-S5 must not assume stdout flows through the seam until this lands.~~ **Resolved: they may.** | Failing test: a recording backend receives the styled line's bytes in the block, not an empty one | `crates/kitsu-render/src/lib.rs` | Byte-identical across all three suites; the styler's fence state and both functions' byte-count returns preserved |
| T0-S4 | `InlineSurface` with **injectable per-fd sinks**: permanent scroll region (rows 1..rows−N) while shown; viewport rows painted by absolute positioning; cursor parked inside the region after each paint; `transcript_line` writes on the given sink | Failing test: in-process, alternating stdout/stderr lines through `vt100::Parser` → the viewport rows stay put under absolute positioning and transcript lines scroll within the region | `crates/kitsu-tui/src/viewport.rs` | **Acceptance corrected per finding F13:** the original text required `vt100` scrollback to hold every line, which is impossible — `Grid::scroll_up` pushes to scrollback only when no scroll region is active, and the viewport sets one, so region-scrolled rows are discarded. That is Q1 appearing early: whether a *real* terminal retains them is terminal-dependent and belongs to T0-S6's live matrix. So: the screen half is asserted in-process, the scrollback half is asserted **negatively** (a test pins that the emulator drops those rows, recording the limitation in code), and the `insert_history` fallback stays deferred until T0-S6 reports rather than being built speculatively |
| T0-S5a | Settle **F15** — `kitsu-render` gains `set_live_rows_enabled` and `live_rows_supported` consults it, so the viewport **replaces** the live row rather than fighting it over the same cells — plus the `KITSU_TUI=inline` gate, installing and restoring `InlineSurface` through the seam, and a session-lifetime guard. All unit-tested through `InlineSurface`'s injectable sinks, so no terminal is needed | Failing test: the gate's truth table fails closed, install redirects the seam into an injected sink and suppresses the live row, and dropping the guard restores both | `crates/kitsu-render/src/live.rs`, `crates/kitsu/src/tui_bridge.rs` (new, unix), `main.rs` startup | Live row disabled writes nothing and re-enabling restores it; classic mode is wholly inert; baselines 111 / 637 / 22 unchanged |
| T0-S5b | **The styled PTY harness, and the measurement it makes possible.** Pulled forward from T3-S1 because the existing harness hardcodes `--plain`, which disables styling and the viewport outright. The card's deliverable is infrastructure plus an *answer*: finding **F17** says the seam's unit tests prove routing, not exclusivity, and `main.rs` holds 392 direct writes that bypass it — so nobody yet knows whether a stray write corrupts the viewport or flows harmlessly into the scroll region, which is question **Q3**. The author was told to measure and report rather than force a pass | A classic-mode test proving the harness itself works, then an inline-mode test asserting the region is set and the viewport row survives `/help` | `crates/kitsu/tests/tui.rs` (new), `vt100` as a `kitsu` dev-dep | Classic test green; the inline measurement reported with the actual screen whether or not it passes |
| T0-S5c | Act on T0-S5b's measurement: if stray writes corrupt the viewport, wrap the slash dispatcher in the seam's suspension (cheap, one site) or route the offending writers (expensive, hundreds) — the measurement decides which, which is why it comes first | Failing test: the inline-mode case T0-S5b leaves red, if any | `main.rs` slash dispatch | The inline viewport survives `/help`, `/jobs` and `/status` at idle |
| T0-S5d | The idle input path: raw stdin bytes to `kitsu_editor::TypeaheadEditor`, Enter submitting through the existing prompt path, and the reader pausing while an approval prompt owns stdin (reuse the collector's yield on `APPROVAL_INPUT_ACTIVE`). This is what makes inline mode usable rather than a preview, since the live row now stands down and nothing paints a draft | Failing PTY test: typing at an idle prompt in inline mode echoes in the viewport and submits | `tui_bridge.rs`, `main.rs` | Typed input appears and submits; an approval prompt still reads stdin; 10 KiB paste benchmark reported |
| T0-S6 | Q2 experiment on top of S4/S5 (ratatui `Viewport::Inline` vs manual paint); writer census for Q3; **go/no-go record** with terminal matrix (xterm-class, tmux, zellij with/without alt screen), paste numbers, mechanism choice (region-flow vs insert_history), and design §4.2 updated accordingly | Record skeleton | `docs/roadmap/cycles/<n>.md`, design doc | Record exists; `KITSU_TUI` unset ⇒ classic byte-identical (fixtures) |

### Exit criteria (go)

- Q1 passes in at least one xterm-class terminal **and** tmux; zellij result
  recorded (may require `--no-alt-screen` later).
- Q2 decided (A or B) with the observed failure, if any, in the record.
- Q3: after the dispatcher wrap, no viewport corruption in the three PTY
  tests; the census lists ≤ 20 stray idle writers or a one-card plan to wrap
  them.
- Q4: 10 KiB paste < 100 ms, or a specific fix identified.

**No-go** (any of: region-flow and insert_history both unreliable in tmux;
latency unfixable; stray writers unmanageable): keep T0-S1 (editor crate) and
T0-S3 (seam, classic-only) if green, **delete** `crates/kitsu-tui`,
`tui_bridge.rs`, the env gate, and the lock entries in one commit; re-plan T3
as "classic + alt-screen overlays only" or "fullscreen-only"; record it.

### Rollback (T0)

`KITSU_TUI` unset ⇒ classic path through the seam only; byte-identical
fixtures from T0-S3 are the proof. Full rollback = the no-go deletion commit.

---

## 4. T1 — Herding hooks and classic polish (design Stage 0)

Independent of the renderer; `main.rs` slash handlers, `subagent.rs`, one new
module, docs **in the same stories**.

| Story | Deliverable | First card | Files | Acceptance / req |
|---|---|---|---|---|
| T1-S1 | `AgentRegistrySnapshot.{blocked, unacknowledged}` (populated by T2 when it lands; 0 until then); `background_agent_notice` text unchanged | Failing test on `unacknowledged` after `acknowledge_result` | `subagent.rs:421,1185`, `main.rs` tests `:21853-21881` | Existing notice tests green; new count tests (A8) |
| T1-S2 | **Classic live row** gains a dimmed `← N agents` tag beside `[N queued]` during a turn (registry watch), per design §4.6 | Failing width-sweep test in `live.rs` | `kitsu-render/src/live.rs`, `main.rs` collector | Width sweep 40..200 no wrap; non-TTY/plain byte-identical (A8 classic, N1) |
| T1-S3 | `/jobs view` (grouped deck, A2 text form) and `/jobs explain <id>` in **all three dispatch sites** (`handle_jobs_command`, `handle_active_jobs_command`, the `/agents` alias arm widened to forward `view\|explain\|reply`); `SLASH_COMMANDS` rows for `agents`/`agent` args updated; `/help` text; `ux-design.md` agent-deck entry updated in the same story | Failing snapshot test of plain `/jobs view` | `main.rs:11148, 13556, 18728, 5194, 5202`, `docs/ux-design.md`, `docs/functionality.md` | Snapshot tests; `/agents view` and mid-turn `/jobs view` both work; JSONL `/help` lists both (A1 alias, A2) |
| T1-S4 | State file writer: `agents.state_file = auto\|on\|off` (**`auto` = interactive TTY sessions only**, so `--script`/piped/JSONL runs write nothing unless `on`); path `$XDG_RUNTIME_DIR/kitsu/<pid>.json`, fallbacks `$TMPDIR/kitsu-<uid>/` (macOS/Linux without XDG) and `%LOCALAPPDATA%\kitsu\run\` (Windows); dir 0700, file 0600, write tmp + rename; content = process state + redacted thread DTO array; removed on clean exit; OSC title (`tui.title`, env-gated until T3 config) | Failing test: writer helper produces the expected JSON and permissions in a temp dir | new `crates/kitsu/src/agent_state_file.rs`, `main.rs`, `config.rs` | Unit tests on path selection per platform (cfg); PTY run shows the file; `--script` run shows **no** file; DTOs `validate_default()` (H2, N1) |
| T1-S5 | `kitsu agents --json [--all]`: read state files, liveness via `libc::kill(pid, 0)` on unix (Windows: `liveness: "unknown"` in T1), stale files skipped; **reader tolerates `schema_version ≤ current`** (validates known versions, reports others as `schema_unsupported`) | Failing parser test with stale-pid and old-schema fixtures | `cli.rs`, `main.rs`, `agent_state_file.rs` | Unit + `--script` integration (H1) |
| T1-S6 | `[hooks] on_agent_state` runner (local command; env `KITSU_AGENT_ID/STATE/WAITING_FOR`); `Config` nested struct; a failing hook never fails the turn | Failing test: transition fires the hook once | `config.rs`, `main.rs` | Scripted transition test; no network (H3, N7) |
| T1-S7 | Docs: `functionality.md` (view/explain, state file, hook), `agent_deck_ui.md` pointer, cycle records; design doc status line | — | `docs/` | Cycle record per template |

**Exit gate:** workspace tests both profiles; plain/JSONL fixtures
byte-identical; `--script` run shows no state file; PTY run shows one.
**Rollback:** `agents.state_file = off`, `on_agent_state` unset; the `/jobs`
subcommands are additive.

---

## 5. T2 — Blocked agents, permission-only (design Stage 1)

| Story | Deliverable | First card | Files | Acceptance / req |
|---|---|---|---|---|
| T2-S1 | `PromptRoute` task-local (`Foreground\|Denied\|Registry{agent_id, sender}`) in `kitsu-tools`; `interactive_prompts_allowed()` ≡ `route == Foreground`; `scope_without_interactive_prompts` keeps setting `Denied` (all seven callers unchanged); new `scope_with_registry_prompts` | Failing test: `Registry` route in `Permissions::resolve` yields a `Blocked` request and resumes on `AllowOnce` | `kitsu-tools/src/lib.rs:44-62`, `permissions.rs:1623` | `cargo test -p kitsu-tools` whole crate green incl. `permissions.rs:2064`; three-case test (y/a/n) for the fs gate (B1, B2) |
**Scope note:** the shipped **T2-S1** was *narrower* than this row. The row
scopes the task-local **and** routing `Permissions::resolve`, with a three-case
y/a/n test for the fs gate. The commit (`c4bd57b`) delivered only the seam —
the `PromptRoute` enum, the `interactive_prompts_allowed()` equivalence and the
scope functions — and deliberately left the `sender` off the `Registry` variant,
because defining it needs the request and answer types. **T2-S2 therefore
absorbs this row's gate half**, and does both gates together: they are
symmetric, they share the request/answer types, and B2 specifies both. Nothing
regressed in the meantime because nothing in production sets a `Registry` route
— verified, the only callers of `scope_with_registry_prompts` are tests.

**Order note:** S3 is taken **before** S2. S2 defines the channel an approval
request travels on, and that channel's destination *is* the `Blocked` state S3
introduces — building the channel first would mean inventing its destination
twice. S3 is also fully self-contained (types, validator, schema bump) and
unblocks nothing else, so moving it earlier costs nothing. The rest of the
milestone keeps the planned order.

| T2-S2 | Command gate: `CommandMode::Ask` consults the route; `CommandApprovalRequest` travels the same channel; `always` applies the exact argv/cwd grant via existing code; **`Registry`-routed command test** added | Failing three-case command test | `command.rs:1687`, tests near `:3578` | `cargo test -p kitsu-tools` green (B2) |
| T2-S3 | Three vocabularies + validator in one story: `AgentStatus::Blocked`, `AgentProgressStatus::Blocked`, `AgentThreadPhase::Blocked`; `validate()` rules (`terminal = phase ∉ {Running, Blocked}` …); `AGENT_THREAD_SCHEMA_VERSION = 2`; optional `name`; label functions and struct literals updated | Failing DTO round-trip test for a blocked thread | `subagent.rs`, `thread.rs:153,194,276,700`, `main.rs:10844,10909,11095,20632`, state-file reader (accepts v1 and v2) | `validate_default()` for blocked DTO; `/jobs` JSON `status: "blocked"`; `debug_assert` holds (B5) |
| T2-S4 | Registry `answer(id, Decision)`; timeout `agents.block_timeout` → `Interrupt` → `AuthorityInputClosed` → `Failed(interrupted)`; `/jobs stop` on a blocked agent resolves `Interrupt` | Failing tokio time-paused timeout test | `subagent.rs`, `config.rs` | Timeout + stop tests (B3) |
| T2-S5 | `/jobs reply <id> <y\|a\|n>` and `/agent reply` in **all three dispatch sites** (idle, mid-turn `handle_active_jobs_command`, alias arm); `agents.blocking = ask\|fail_closed` with default **`fail_closed` everywhere until T3** flips the TUI-mode default; explicit `ask` works in classic via `/jobs reply` — that is T2's standalone value | Failing mid-turn PTY/unit test: reply during a running turn releases the agent | `main.rs:11148,13556,18728,5194,5202`, `config.rs` | Plain/scripted/JSONL defaults byte-identical; `background_result` unchanged (B4, N1) |
| T2-S6 | `AgentTimelineKind::Approval` entries for request/answer; shown in `/jobs show` and `/jobs explain`; redaction test (path/argv only) | Failing timeline DTO test | `thread.rs`, `main.rs` | Redaction proven (N8) |
| T2-S7 (after T1) | Hooks surface: state file `waiting_for`, `on_agent_state` `STATE=blocked`, `kitsu agents --json` | Failing scripted transition test | from T1 | (B4, H1–H3) |

**Exit gate:** provider-free integration test (fake provider): spawn → blocked
→ reply → done, both profiles; JSONL strict fixtures unchanged.
**Rollback:** `agents.blocking = fail_closed` (default) restores today's
behavior exactly; the route defaults to `Denied` in every existing scope.

---

## 6. T3 — Inline renderer, release quality (design Stage 2)

Starts after the T0 **go** record.

| Story | Deliverable | First card | Files | Acceptance / req |
|---|---|---|---|---|
| T3-S1 | **Test infrastructure first:** `run_styled_streaming_chat_pty` (binary under PTY, styled, streaming, tool-calling SSE mock), vt100 screen assertions with deadlines (never byte timing), stdout/stderr split capture | One classic-mode test using the harness | `crates/kitsu/tests/tui.rs` (new) | Harness reused by every later PTY acceptance |
| T3-S2 | Config `[tui]` nested struct (`mode`, `alternate_screen`, `prefix`, `title`, `left_arrow_opens_agents`, `cell_ring_lines`), `--tui`, `--no-alt-screen`; env gate removed; `kitsu doctor` names an unknown key | Failing config tests | `config.rs`, `cli.rs`, `main.rs` | Unknown-key message test (§4.8) |
| T3-S3 | Composer parity: `CompletionSource` trait in `kitsu-tui`, implemented in `tui_bridge.rs` over `attachments::complete` and the slash palette; hinter/highlighter re-hosted; shared functions used by rustyline `ChatHelper` too | Failing pure test: same candidates for `@fi` and `/st` from both hosts | `kitsu-tui/src/composer.rs`, `ChatHelper`'s `Completer`/`Hinter`/`Highlighter` impls and the `rl.set_helper` construction in `run_chat` (**find by name — the former `main.rs:5928-5980` reference had drifted to an unrelated reader**) | Candidates equal; 39 editor tests green (R4) |
| T3-S4 | **History file shared**: history stays rustyline's — it loads, records and saves; the inline reader is handed a per-read snapshot. **Amended in T3-S4: no parser in `kitsu-tui`** (F49) | Round-trip test on a fixture history file (passes on arrival; there was nothing to fail) | `main.rs` tests only | ↑ walks the same entries in both renderers (R4) |
| T3-S5 | Status row (`model · meter · elapsed · [N queued] · [N blocked]`/`[N agents]`) and queue preview rows via `Surface::queue`. The ambient tag uses R5's **bracketed** form from the one `agents_tag` composer; **`← N agents` stays forbidden until A1's left-arrow affordance ships in T4**, per R5. The row composer lives in `kitsu-render` (it must serve both renderers; `kitsu-tui` cannot be depended on by `kitsu-render`), so `status.rs` is a caller that composes nothing | Failing width-sweep test | `kitsu-render/src/live.rs` (composer), `kitsu-tui/src/status.rs` (caller) | No wrap 40..200 cols, and the documented drop order (R5) |
| T3-S6 | Cell ring (bounded, fd-tagged) fed by the surface | Failing eviction test | `cells.rs` | Memory bound test (N4) |
| T3-S7a | Approval modal **renders** in the bottom pane: same option line from the one `approval_options_with_mode` composer, same `◉ permission —` receipt, command provenance rows kept. **Protocol unchanged.** Composition in `kitsu-render/src/lib.rs`, painting in `kitsu/src/tui_bridge.rs` beside `repaint_status_row`, prompters at `main.rs` `CliPrompter`/`CliCommandPrompter` (find by name) | Failing PTY approval round-trip | see above | `sanitize` of receipt equals classic (R7) |
| T3-S7b | Replace the `APPROVAL_INPUT_ACTIVE` protocol with an in-loop modal: **13 uses, four spin-wait loops** where the collector yields stdin, plus the claim/release pair. Owns echo placement, since the protocol keeps the approval read canonical-with-echo | Failing PTY test that the typed answer is not echoed outside the pane | `main.rs` (`APPROVAL_INPUT_ACTIVE` sites) | No stdin-ownership regression |
| T3-S7c | **New requirement needed:** commit the submitted prompt to the inline transcript. Classic's `kitsu> …` row is **not bytes kitsu emits** — it is rustyline's prompt plus the kernel's cooked-mode echo — so inline parity means *authoring a line classic never authored*, with its own text and style decisions | — (needs a requirement first) | `kitsu/src/tui_bridge.rs` | Must not be smuggled under R7's receipt clause |

> **T3-S7 was split at review.** The row bundled rendering with a concurrency restructure of stdin ownership, and the risky half was not the user-visible half. Rendering was proven independent: by the time the modal paints, the approval arm has already restored canonical mode "for the approval reader" and called `live_input_end()` (`main.rs:15073`), so the collector is parked and the draft row torn down — painting there is purely additive. The original row's `kitsu-tui/src/overlay/approval.rs` was **impossible**: `kitsu-tui` depends only on `kitsu-render` and cannot see `PermissionRequest`. Its `main.rs:19461,19504` anchors had drifted to checkpoint JSON.

| T3-S8 | Resize reflow (region reset + repaint, no duplicate lines); `Ctrl+L`; restore on panic/SIGINT/SIGTERM | Failing PTY `TIOCSWINSZ` test | `viewport.rs`, `lib.rs` | No duplicate cell; `kill -INT` restores (R8, N5) |
| T3-S9 | `/tui classic\|inline` runtime switch (seam swap between turns) | Failing PTY switch test | `main.rs`, `surface.rs` | Transcript continues (R2 partial; fullscreen in T5) |
| T3-S10 | Stray-writer follow-up from the T0 census: wrap or route the remaining idle writers; documented limitation list for the cell ring | Failing test per wrapped site | `main.rs` | Census count reaches zero or the remainder is documented (Q3) |
| T3-S11 | Terminal matrix + fallbacks: tmux, zellij (`--no-alt-screen`), screen; Windows Terminal = classic fallback until T5. **Windows check, measured in T3-S11:** `kitsu-tui`, `kitsu-render` and `kitsu-editor` all `cargo check --target x86_64-pc-windows-gnu` **clean**; the whole-binary check needs a **MinGW cross-compiler** for `ring`'s build script — the row's "(no linker needed)" was wrong, because `cargo check` skips linking but still runs build scripts (**F59**). `cfg(unix)` narrowing (4 in `tui_bridge.rs`, 85 in `main.rs`) deliberately **not attempted** without a compiler to prove it; `viewport.rs` has none. **The live matrix still needs a human** (Q1) | — | `viewport.rs`, `tui_bridge.rs` | Matrix in the cycle record; renderer crates Windows-green (N6 partial) |
| T3-S12 | Frame coalescing (≤ 60 fps), synchronized-update probe, `--reduced-motion` behavior | Failing scheduler unit test | `lib.rs` | PTY under `--reduced-motion` static (N3, N9) |
| T3-S13 | N2 benchmark (test binary timing 10 KiB paste through the PTY, reported not gating); docs: `ux-design.md` surface section, `functionality.md` (`/tui`, flags), `ui_guidelines.md`; design status → "implemented through T3" | — | `tests/`, `docs/` | Numbers in the PR text |

**Exit gate:** per-fd sanitize equality on the streamed two-tool PTY run; all
fixtures byte-identical in classic/plain/JSONL; both profiles; `cargo test
--locked`; Windows `cargo check`.
**Rollback:** `--tui classic` / `tui.mode = "classic"`; seam stays.

---

## 7. T4 — Agent view: peek, attach, dispatch (design Stage 3)

| Story | Deliverable | First card | Files | Acceptance / req |
|---|---|---|---|---|
| T4-S1 | Pure model in `kitsu-tui`: `AgentRow`/`AgentGroup` plain structs, grouping, ordering, filter grammar; `tui_bridge.rs` maps `AgentThreadSnapshotDto` → `AgentRow` | Failing table-driven grouping test | `kitsu-tui/src/agents_view.rs`, `tui_bridge.rs` | (A2, A3 filter) |
| T4-S2 | Overlay: list, `↑/↓`, `?`, `Esc`; opened by `Ctrl+X a`, `/jobs view` (TUI mode), `←`-twice on empty composer (config); `←` never consumes a draft; `↑` recall untouched | Failing PTY open/close test | `overlay/agents.rs`, `tui_bridge.rs` | (A1) |
| T4-S3 | Peek (`Space`): redacted timeline, pending request, result; viewing acknowledges via `peek_result` + `acknowledge_result` | Failing test: **a peeked result is not surfaced at exit, an unviewed one still is** — *amended in T4-S3: the original read "unviewed failed agent still refuses", but nothing refuses `/quit` for a finished agent; only `Running`/`Blocked` block it, and unviewed results are **surfaced** at exit, not refused (**F64**)* | `overlay/agents.rs` | (A4, A7) |
| T4-S4 | Keys → registry calls: `r` reply, `s` steer, `x` stop/prune, `n` rename (new `AgentRegistry::rename`), `p` acknowledge | Failing outcome tests | `subagent.rs`, overlay | (A3) |
| T4-S5 | `tail_timeline(id) -> broadcast::Receiver` (cap 256; lag → "n skipped") | Failing lag test | `subagent.rs`, `thread.rs` | Timeline bounds unchanged |
| T4-S6a | Attachment and the keys that enter and leave it: `Enter`/`→`, `←`, `Ctrl+X d`, `/detach`; **rejected once for F75** (the attachment had no reader) and reworked with a carrier out of the overlay | done | this commit |
| T4-S6b | The routed composer: `steer` when working, `y`/`a`/`n` through the one `parse_permission_answer` when blocked; the `⛩ attach` milestone | **not started** — needs a live correction channel, the fixture problem that split T4-S4b | — |
| T4-S6c | The bounded 64 KiB foreground buffer and the recap replay on detach | **not started** — disjoint surface (the output sink); may need R6's cell ring | — |
| **F76 fixed** | `InlineRead::Detached` carries the draft and `Ctrl+X d` keeps a line in progress; the cheap narrowing was rejected because the chord's only non-redundant case is exactly that one | done | this commit |
| T4-S6-status | The `attached · agent N` status segment, derived at paint time; **F74** corrected (nine constructions, three exhaustive, `Default` already derived), **F78** measured | done | this commit |
| **F77 + F79 fixed** | `/detach` wired through a 17th `&mut Option<u64>` on `handle_slash`; `/jobs view` no longer discards the attachment (**F79**, the third consumer of the same exit value); a `#[cfg(test)]` shim removed on review | done | this commit |
| T4-S6-ordinal | `Ctrl+X 1..9` as a **1-based ordinal over A2's display order**, never an id | **not started** | — |
| ~~T4-S6~~ | Attach/detach: `⛩` milestones, routed composer (`steer`, `y/a/n` when blocked), 64 KiB foreground buffer replayed with recap; `Ctrl+X d`, `←`, `/detach`; `Ctrl+C` = clear draft / detach | Failing fake-provider integration test | `tui_bridge.rs`, overlay, `status.rs` | (A5) |
| T4-S7 | Dispatch from the agent view with `+` (`task` or `@profile task`), admitted by `AgentRegistryLimits`, **not** `SpawnBudget` — the view's **fourth** edited line | done, this commit | `main.rs`, `kitsu-tui/src/agents_view.rs`, `subagent.rs` — *corrected in T4-S7: the row named `tui_bridge.rs`, which is the **inline** composer and is untouched here; T4-S6b is the card that lands there* | (A6) |
| T4-S8 | Ambient counter ≤ 250 ms in TUI — **A8 was violated at the idle composer**, now driven by `registry.subscribe()`; **F81** | done, this commit | `main.rs`, `tui_bridge.rs` — *corrected: `status.rs` holds no timing, and no "paused clock" exists to pause* | (A8) |
| **F81 residual** | Mid-turn the approval arm `continue`s before the counts are re-pulled, so the counter freezes while an approval modal is up | **not started** — adjacent to **F54** | — |
| T4-S9 | Transcript overlay (`Ctrl+O`): scroll, `/`, `n/N`, `{ }`, `v`, `q` over the cell ring | Failing search/jump unit test | `overlay/transcript.rs` | (R6) |
| T4-S10 | Single bindings table → help overlay + `/help keys` — **note:** the help literal now has **six** rows to absorb (`up/dn`, `space`, `/`, `+`, `?`, `esc`), one added by each of T4-S3, T4-S4a, T4-S5 and T4-S7; docs (`ux-design.md` glyph `◉` row rule, `functionality.md`) | Failing bindings-table test | `kitsu-tui/src/keys.rs`, `docs/` | (§4.7) |

**Exit gate:** fake-provider integration (two agents, one blocks, view shows
it first, reply, attach, detach, acknowledged); exit guard unchanged; both
profiles.
**Rollback:** `tui.left_arrow_opens_agents = false`; overlay is additive;
`--tui classic` hides it entirely.

---

## 8. T5 — Fullscreen, agent_wait, herdr manifest, default flip (design Stage 4)

| Story | Deliverable | First card | Files | Acceptance / req |
|---|---|---|---|---|
| T5-S1 | Fullscreen renderer over the cell ring (alt screen, auto-follow, `PgUp/PgDn`, `Ctrl+Home/End`) | Failing 5 000-line RSS test | `kitsu-tui/src/fullscreen.rs` | (N4) |
| T5-S2 | `/tui fullscreen` runtime switch (completes R2) | Failing PTY switch test | `main.rs`, `surface.rs` | (R2) |
| T5-S3 | `[` dump ring to native scrollback; `v` to `$EDITOR` | Failing equality test | `overlay/transcript.rs` | (R6) |
| T5-S4 | Windows Terminal fallback (inline → fullscreen when regions misbehave); `--no-alt-screen` forces inline; Windows liveness in `kitsu agents --json` | Failing decision-function test | `viewport.rs`, `agent_state_file.rs` | (N6, H1) |
| T5-S5 | Mouse wheel in fullscreen (optional) | — | `fullscreen.rs` | Manual |
| T5-S6 | `agent_wait(id, until, timeout)` tool; no `SpawnBudget` use | Failing until/timeout tests | `subagent.rs` | (H5) |
| T5-S7 | herdr manifest under `integrations/herdr/` + README | — | `integrations/` | Manual (H4) |
| T5-S8 | Decision records: default renderer flip (R1), JSONL `background_blocked` (open q. 7), socket API (open q. 6); traceability table appended to the design | — | docs | ADR-style records |

**Rollback:** `tui.mode = "inline"` or `"classic"`.

---

## 9. Cross-cutting rules

- **Kill switch at every stage:** `--tui classic`, `tui.mode = "classic"`,
  `KITSU_TUI` unset (T0–T2). Classic goes through the seam only; byte-identical
  fixtures from T0-S3 on are its proof.
- **Seam lock discipline:** load an `Arc<dyn Surface>` from a short read
  guard; never hold the guard across a `Surface` call; swap only between turns
  on the REPL thread.
- **Invariants cited in every card:** N1 byte-identical tiers; sanitize
  round-trip; redaction (N8); exit guard for unviewed results;
  `debug_assert!(validate_default())`; `scope_without_interactive_prompts` ⇒
  denied for all seven callers.
- **Both cargo profiles** on every card; `--locked` on dependency cards;
  MSRV sanity on dependency cards — not `cargo +1.80.0 check`, which cannot
  pass (finding F9), but confirmation that a newly added dependency's own
  `rust-version` is no higher than the existing tree's effective floor; Windows `cargo check` at T3 exit and later.
- **PTY matrix (milestone exits):** xterm-class terminal, tmux, zellij
  (`--no-alt-screen`), screen; results in the cycle record.
- **Provider-free acceptance:** fake/wiremock providers only; the ollama smoke
  (qwen2.5-coder) is a manual extra and never trusted on kitsu's own success
  report.
- **Docs move with code:** `ux-design.md` ("when the code and this file
  disagree, fix one of them in the same change") and `functionality.md` are
  updated in the story that changes the surface; trailing doc stories only
  collect cycle records and status lines.
- **Dirty tree:** `crates/kitsu-tools/src/builtins/fs.rs` is a pre-existing
  uncommitted change not owned by E11; the maintainer decides (commit
  separately or stash) before the first E11 commit. The design, plan, review
  records, and the two doc fixes from the design review form E11's first docs
  commit.
- **No orchestrator edits** unless the T0 record says Q3 demanded them.

## 10. Risks specific to the plan

| Risk | Where it bites | Plan response |
|---|---|---|
| ratatui/crossterm raise MSRV or build time; `--locked` Windows release | T0-S2, T3 exit | Pins verified against each crate's own `rust-version` (the 1.80 toolchain check is impossible — F9); `cfg(unix)` bridge until the Windows `cargo check` passes; build delta recorded |
| ratatui inline viewport fights the scroll region (Q2) | T0-S6 | Pre-decided fallback: manual viewport + ratatui overlays only |
| Region-flow does not reach scrollback in some terminal (Q1) | T0-S4/S6 | `insert_history` fallback implemented in S4; matrix decides |
| 392 stray writers corrupt the viewport (Q3) | T0-S5, T3-S10 | Dispatcher wrap first; census; route the remainder or document |
| `Surface` indirection regresses classic or deadlocks | T0-S3 | Byte-identical fixtures + reentrancy test; lock discipline rule |
| Two stdin readers during approvals | T0-S5, T3-S7 | Honor `APPROVAL_INPUT_ACTIVE` in T0; modal replaces it in T3 |
| State file violates the "no state write while disabled" invariant | T1-S4 | `auto` = interactive only; automation writes nothing unless `on` |
| Schema skew between running processes | T1-S5, T2-S3 | Reader tolerates older schema versions |
| Styled PTY flakiness | T3-S1 onward | vt100 screen-state assertions with deadlines; "known PTY failure" convention made explicit per test |
| Scope creep into free-form agent questions or sockets | T2, T5 | Design non-goals / open questions; stories cite them as out of scope |

## 11. Traceability (design requirement → stories)

| Req | Stories |
|---|---|
| R1 | T3-S2, T5-S8 |
| R2 | T3-S9, T5-S2 |
| R3 | T0-S4, T0-S5, T3 exit |
| R4 | T3-S3, T3-S4, T3-S5, T3-S7 |
| R5 | T3-S5 |
| R6 | T4-S9, T5-S3 |
| R7 | T3-S7 |
| R8 | T3-S8 |
| A1 | T1-S3 (alias widening), T4-S2 |
| A2 | T1-S3, T4-S1 |
| A3 | T4-S1, T4-S4 |
| A4 | T4-S3 |
| A5 | T4-S6 |
| A6 | T4-S7 |
| A7 | T4-S3 |
| A8 | T1-S1, T1-S2, T4-S8 |
| B1 | T2-S1, T2-S5 |
| B2 | T2-S1, T2-S2 |
| B3 | T2-S4 |
| B4 | T2-S5, T2-S7 |
| B5 | T2-S3 |
| H1 | T1-S5, T5-S4 |
| H2 | T1-S4 |
| H3 | T1-S6 |
| H4 | T5-S7 |
| H5 | T5-S6 |
| N1 | T0-S3, T1-S4, T2-S5 |
| N2 | T0-S5, T3-S13 |
| N3 | T3-S12 |
| N4 | T3-S6, T5-S1 |
| N5 | T0-S2, T3-S8 |
| N6 | T0-S6, T3-S11, T5-S4 |
| N7 | T1-S6 (hook runs a local command; test asserts no network client is constructed) |
| N8 | T2-S6, T4-S3 |
| N9 | T3-S12 |

## 12. Definition of done (epic E11)

- Every requirement above maps to an accepted story; the table is re-checked
  at each milestone exit.
- Classic, plain, non-TTY, assistive, and JSONL fixtures are byte-identical to
  the pre-E11 baseline.
- A user can: start in inline mode, see `← 1 agent` change when a background
  agent blocks, press `Ctrl+X a`, read the request, reply `y`, attach, watch,
  detach with a recap, and herdr shows kitsu's state without screen scraping.
