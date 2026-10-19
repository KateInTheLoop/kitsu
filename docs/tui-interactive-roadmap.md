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
- [x] **Shift+Left/Right agent cycling** (`4d2d87f`). `ESC[1;2D`/`ESC[1;2C`
      lifted in `KeyFilter` → `KeyItem::CycleAgent(±1)` →
      `InlineRead::CycleAgent` → `apply_inline_cycle_agent`/`cycle_target`
      (clamp, detached→first). Tests: decode (incl. pasted-stays-text),
      `cycle_target` logic.
- [x] **Arrow-scroll model picker for bare `/model`** (`1a4d97b`).
      `run_model_picker` + pure `model_picker_step`/`model_picker_rows`, reuses
      overlay primitives; pick flows through existing switch path. Tests: step
      movement/clamp/pick/cancel, row rendering/truncation.

## TODO — ordered

### 1. Rendering pass (NEXT — user asked to look at rendering after features)
Investigate and improve rendering. Known/suspected issues to probe:
- Full-repaint-every-frame overlays (`\x1b[2J` + absolute addressing each loop)
  — flicker on large frames? Consider diff/damage-based repaint.
- Escape-sequence leakage observed in test output (`[?25h[?2004l[23;2t`) — is a
  surface failing to restore modes on some exit path?
- Long model lists in the picker: no scrolling yet (only truncation to width,
  and rows capped at terminal height). Add vertical scroll/paging for >screen
  lists.
- Consistency of the three modes (classic/inline/fullscreen) and whether the
  picker/cycle degrade cleanly in each.

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
