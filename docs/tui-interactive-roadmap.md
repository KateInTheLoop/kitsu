# Interactive TUI upgrade — roadmap & status

Historical implementation notes, started on `feat/tui-option-picker` and now
carried on `feat/tui-rendering`. Everything stays local, **not pushed**.
Current status, remaining work, and acceptance criteria are maintained in
[next-development-plan.md](next-development-plan.md); its milestone order and
status supersede the older lists below. The guiding constraints still apply.

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

## DONE — auto-mode read/write fix (`c3e1f02`, authored KateInTheLoop)
Root cause (diagnosed): `--yes` suppressed prompts but granted NO roots and
registered NO fs tools in non-interactive mode, so an automation run could
neither read nor write — the model refused correctly ("nowhere to write").
There was simply no scoped no-prompt read+write mode.
FIX: `--yes` now grants the launch cwd as a read+write root and registers the
fs tools. Scoped to cwd only (NOT the global `--dangerously-skip-permissions`
bypass); writes outside cwd still denied; config/state protected; commands
still off. Pure `cwd_grant` helper + unit test (integration suite blocked on
macOS). Verified live: `kitsu --yes chat "create hello.txt"` writes in cwd
with no prompts; write outside cwd is PolicyDenied.
USAGE for the kernel task: run kitsu from the OUT dir (or a dir containing both
OUT and the read inputs) with `--yes`, instead of read-only `--allow-path`.
STILL OPEN from that run: read-scoping let the model LIST a forbidden `rfc/v2/`
sibling (filenames only). Tighten the grant recipe if "don't read X" matters.

## TODO — ordered

### 0. Picker redesign to popup + type/Tab — DONE (`6cd05a0`, CatherinaInTheLoop)
Shipped: popup over the composer via `MODAL_PANE` (not fullscreen). Type to
filter, ↑/↓ move, Tab completes selection into filter, Enter picks, Esc/Ctrl-C
cancel; `scroll_window` for long lists; paste only edits the filter (copy-paste
safe). `/model` and `/models [filter]` (inline+tty) open it pre-filtered; pick
delegates to the `/model <id>` switch path (boxed async recursion). Non-tty/
classic/fullscreen/scripted keep the text list. New `tui_bridge::paint_modal_rows`
+ `modal_pane_budget`; `clear_approval_modal` restores the composer. Pure
`model_popup_step`/`model_popup_view`/`scroll_window` unit-tested. Fullscreen
`run_model_picker`/`model_picker_rows` removed.
NOT yet confirmed live by the user — needs a hands-on try (`/model`, `/models`,
`/models opus`).

--- original design notes (kept for reference) ---
Superseded the fullscreen `run_model_picker`/`model_picker_rows`.

MECHANISM (verified in code):
- Render into the pinned `MODAL_PANE` (tui_bridge.rs:1273), NOT the alt screen.
  `paint_approval_modal` (tui_bridge.rs:1292) proves arbitrary rows can be
  painted over the composer from anywhere; the pane auto-grows its viewport to
  content height (clamped by `granted_rows`), and `clear_approval_modal`
  (tui_bridge.rs:1327) restores the composer status row on close.
- `MODAL_PANE` is published for the whole `BridgeGuard` lifetime (install,
  tui_bridge.rs:203), so it IS live during `handle_slash` (post-submit).
- Still engage `RawStdinGuard` (reader's raw guard is dropped by the time
  handle_slash runs, same as the fullscreen picker did), and reuse
  `decode_overlay_keys` + `ESC_SEQUENCE_GRACE` + `stdin_waiting`.
- NOT the slash palette (`slash_palette_rows`, main.rs:6022): that is sync,
  command-names only (bails on whitespace after `/`), no arrow-select, and
  cannot do the async catalog fetch. Right idea (type-to-filter+Tab over the
  composer), wrong vehicle for argument-side async lists.

NEW CODE:
- `tui_bridge::paint_modal_rows(&[String]) -> bool` (thin sibling of
  paint_approval_modal; or generalise it). Reuse `clear_approval_modal` to
  restore.
- Rework `run_model_picker` into a pinned-pane loop holding filter String +
  selected index. Keys: printable→append to filter (re-filter, clamp sel);
  Backspace→trim filter; ↑/↓→move; Tab→complete sel into filter/accept; Enter→
  pick; Esc/Ctrl-C→cancel. Paste = filter text (never commands) → copy-paste safe.
- Pure `model_popup_view(models, filter, selected, cols, max_rows) ->
  (Vec<String> rows, filtered_len)` = filter_model_catalog + scroll_window
  (already written) + render. UNIT TEST this (the integration suite can't run
  on macOS). Keep/relocate the `scroll_window` + its test.
- Wiring: `/model` (no arg) and `/models [filter]` on inline+tty open the popup
  (pre-filtered by [filter]); pick flows through the EXISTING switch path (one
  switch code path). Non-tty/classic/fullscreen/scripted → today's text list.

DECISION (user): `/models <text>` opens the popup PRE-FILTERED (not the static
text list) in interactive mode. Static list stays for scripted/piped/classic.

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
