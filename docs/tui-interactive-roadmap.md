# Interactive TUI upgrade — roadmap & status

Branch: `feat/tui-option-picker` (off `main` + openpty macOS test fix).
Everything local, **not pushed**. This file is the single source of truth for
where we are and what is left.

## Guiding constraints (from the user — do not violate)
1. **Copy-paste is sacred.** Navigation/keys must NEVER fight copy-paste. Any
   key feature must be inert inside a bracketed paste (payload between
   `ESC[200~`/`ESC[201~` is text, never a command). Every new key gets a
   "pasted bytes stay text" test.
2. **Reuse, don't reinvent.** kitsu hand-decodes raw stdin bytes (not
   crossterm events) and has battle-tested overlay machinery: `OverlayDecoder`,
   `decode_overlay_keys`, `AgentSelection`, `ESC_SEQUENCE_GRACE` (50ms lone-Esc
   grace), `RawStdinGuard`, alternate-screen paint, panic/signal restore. Build
   on these.
3. **Overlays are Inline-mode only.** Classic (rustyline) and Fullscreen keep
   current behavior; new interactive surfaces decline outside Inline+tty.
4. **tmux:** agent-switching means kitsu does NOT need tmux internally. Plan is
   to auto-DETECT tmux and coexist (for the rest of sysadmin work), not require
   or fight it.

## Architecture cheat-sheet (verified file:line, may drift)
- Slash dispatch: `handle_slash` `main.rs:~23391`; `/model` arm `~24663`,
  `/models` `~24469`. `/jobs view` (`~15463`) is the precedent for launching a
  raw-mode+alt-screen overlay FROM a slash command.
- Key decode (composer): `KeyFilter::feed` `tui_bridge.rs:~2506`; lifted
  sequences table near `PAGE_KEYS` `~2382`. Transport enum `InlineRead`
  `~2664`. Composer loop handlers `main.rs:~6525` (Detach/AttachOrdinal/Cycle).
- Overlay loop template: `run_transcript_overlay`/`run_transcript_key_loop`
  `main.rs:~6660` (smallest), `run_agent_view_overlay` `~9617` (full).
- Selection model to reuse: `AgentSelection` `agents_view.rs:179`;
  `display_order`/`nth_displayed` `agents_view.rs:102/130`.
- Permission prompts: `parse_permission_answer` `main.rs:~25xxx` (THE parser),
  `Decision`/`CommandDecision` 3-way, `CliPrompter`. Agents overlay
  `OverlayPrompt::Decision` already receives Up/Down (currently no-op).
- `model_picker.rs` is a heavy `pub(super)` validation layer — NOT an
  interactive widget; do not try to reuse it for the loop.

---

## DONE (committed on this branch)
- [x] **openpty macOS/BSD test build fix** (`71cec80`). Prereq: tests didn't
      compile on mac (`null()` → `null_mut()`).
- [x] **Shift+Left/Right agent cycling** (`4d2d87f`). Decode → `CycleAgent(±1)`
      → `apply_inline_cycle_agent`/`cycle_target`. Tests incl. pasted-stays-text.
      STATUS: believed working; NOT yet confirmed live by the user.
- [~] **Arrow-scroll model picker, bare `/model`** (`1a4d97b` + scroll `977f4a7`).
      `run_model_picker` + pure `model_picker_step`/`model_picker_rows`/
      `scroll_window`. Tests pass. **NOT user-ready — see "Testing findings".**

## Testing findings (2026-10-18 — features NOT ready)
What the user hit while playing with the installed binary:
1. **`/models` does nothing new.** The picker was wired ONLY to bare `/model`.
   `/models` (the verb the user naturally reached for) still prints the static
   text list. → Picker must also open from `/models` with no filter.
2. **Could not scroll.** The binary the user tested predated the scroll commit
   (`977f4a7`); also the fullscreen approach felt wrong (see redesign).
3. **The installed binary lagged the branch** more than once — always rebuild +
   `cargo install --path crates/kitsu --force --locked` AND have the user
   confirm `kitsu doctor`'s source commit before testing.

## NEW design direction for the picker (user, supersedes current impl)
The fullscreen alternate-screen takeover is the wrong feel. Instead:
- **Render a POPUP** over the composer area (a few rows pinned above the input),
  NOT a whole-screen `?1049h` takeover. Un-render it after the user picks, back
  to the normal composer.
- **Type-to-filter inside the popup** (live substring narrowing, like the
  `/models <substring>` flow but interactive) — reuse `filter_model_catalog`.
- **Tab** support (complete/accept the highlighted model).
- Arrows still move; Enter picks; Esc cancels.
- Still must obey: copy-paste sacred (pasted bytes are text), Inline-only,
  decline elsewhere.
- IMPLEMENTATION LEAD: this is closer to the composer's own completion path
  (`kitsu-tui::composer::Composer`, `kitsu-editor`) + a small pinned-pane popup
  via `tui_bridge`'s `MODAL_PANE`/`paint_approval_modal` machinery than to the
  fullscreen agent-view overlay. Revisit which primitive to reuse before coding.
- The current `run_model_picker` (fullscreen) stays in the tree as WIP but is
  NOT the shipping design; likely replaced.

## SEPARATE BUG (not TUI) — auto-mode read/write refusals
The user's recurring pain running kitsu as a plain CLI agent (opus-5-5 run):
the model **refused to read kernel source or write files** and stopped with a
plan, citing: `$OUT` never set to a path, the only granted root was
**read-only**, git blocked (macOS, expected), and it had "nowhere confirmed to
write". The user: *"always issues with reading files and writing! it should
work by default, in auto mode!"*
- Likely causes to investigate (NOT yet diagnosed):
  - The launch grants (`--allow-path` read-only vs `--allow-read-write`) didn't
    cover the OUT dir, OR the prompt's `$OUT` wasn't expanded so the model never
    had a concrete path.
  - Deny-by-default in scripted/piped mode means no write happens without an
    explicit `--allow-write`/`--allow-read-write` root — the model then correctly
    refuses. "Auto mode" needs the grants wired so reads+writes to the work dir
    just work.
  - `--dangerously-skip-permissions` / a proper grant set may be the intended
    "auto mode"; confirm what the user expects "by default" to mean.
- It also **listed a forbidden `rfc/v2/` sibling** (saw filenames, read none) —
  read-scoping wasn't tight enough. Revisit the grant scoping recipe.
- See memory: kitsu-macos-no-commands, kitsu-reliability-notes (#4 budget),
  kitsu-effort-presets. This belongs on the reliability-notes ledger too.

## TODO — ordered

### 0. Picker redesign to popup + type/Tab (was "model picker"; now reopened)
Per NEW design direction above. Supersedes the fullscreen `run_model_picker`.

### 1. Rendering pass (in progress)
- Flicker: investigated — overlays build one string + single atomic write, so
  no local flicker; the leading `\x1b[2J` is load-bearing (wipes stale rows).
  The escape bytes seen in test output were `SIGNAL_RESTORE` (`tui_bridge.rs`
  ~2014) firing on the 193 pre-existing panicking tests = restore WORKING, not
  a bug. NOT refactoring the three paint loops (they work).
- Picker scroll: DONE (`scroll_window`). Will carry into the popup redesign.
- Remaining: verify clean degrade across classic/inline/fullscreen.


### 2. Colorful expandable/minimizable diff windows (Codex-style) — AFTER rendering
- Show diffs as framed, colorful windows per file, expandable/minimizable.
- Header per file: path + `+N / -M` lines added/removed.
- Model the window state on the overlay/`OverlayState` pattern; reuse selection
  for focusing a file; Enter/→ expand, Esc/← minimize.
- Source of diffs: `/diff` already computes host-verified diff evidence
  (`main.rs` review/diff path) — reuse that, don't recompute.

### 3. Multi-window / popup framework (big — design first)
- Multiple windows, popups, resizable (smaller/bigger) panes.
- A left/right side pane that can host a decorative animated "pet fox",
  toggleable by a `--flag` and by `x` at runtime.
- A navigation/help popup explaining how to move between / close windows.
- HARD: window navigation keys must not collide with copy-paste (constraint 1)
  or with the composer's existing chords (Ctrl+X …, Shift+arrows, Ctrl+O).
- Likely needs a real layout/region manager above today's single pinned
  viewport — this is the largest piece; plan before coding.

### 4. tmux auto-detect
- Detect `$TMUX` / `$TERM` and adjust (passthrough sequences, no double alt-
  screen). Coexist; don't require. kitsu's own agent-switching replaces the
  need for tmux panes inside kitsu.

### 5. Follow-ups deferred from feature round 1
- `[Pasted N characters]` placeholder for multi-line/large pastes (needs a
  paste-span/display-vs-content split in `kitsu-editor` buffer — invasive;
  only reliable inside the bracketed-paste block, `editor lib.rs:~403`).
- Permission y/a/n prompts → arrow-select (agents overlay already gets Up/Down;
  foreground `CliPrompter` is the harder path).
- Extend the arrow picker to other list commands (`/skills`, `/sessions`,
  `/theme`, `/reasoning`).

## Test / verify notes
- Build: `cargo build` (clean). Bin tests: `cargo test --bin kitsu`.
- **193 pre-existing bin-test failures** on macOS (session/skills/attachments/
  offload/artifact — symlink/tempdir/credential family). Confirmed identical on
  the base commit, so they are NOT regressions. Our new tests all pass.
- New-feature tests: `cargo test --bin kitsu model_picker`,
  `... cycle_target`, `... shift_arrows`.
