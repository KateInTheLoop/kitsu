# Input box + copy-box: requirements & analysis

Captured 2026-10-20 from live testing on stock macOS Terminal. Branch context:
`feat/tui-rendering`. The framed composer and stabilization fixes are now
implemented; visual navigation and copy-box work remain planned. The
observations and proposed phasing below describe the original baseline; the
recorded product decisions remain applicable.

## What the user wants (Claude-CLI-style input)
A persistent, **bordered, auto-expanding** input box — like Claude Code's `❯`
framed region — replacing today's single pinned status row. Plus a **copy-box**:
a large paste collapses into an inline `[Pasted text #N +M lines]` token you can
edit/remove as a unit, with a gray hint line underneath.

---

## Observed bugs today (the starting point)
1. **No input region at an empty prompt.** At idle, the composer paints only the
   status meter line (`<model> · ■□□□ 0%`), not a prompt row. There is no box.
   - Cause: `compose_row` returns `status_row()` when the draft is empty
     (`live.rs`), so nothing shows `kitsu>` until you type.
2. **Cursor stranded above `kitsu>` and does not move.** With an empty draft the
   composer emits no cursor column (`cursor_col = None` → the `\x1b[{col}G` at
   `live.rs:1022` is skipped), so the hardware cursor rests wherever the last
   write left it — the row above. Stock Terminal.app.
   - This is why "cursor is above kitsu and doesn't move": there is no owned
     caret position at idle, and a single-line draft never repositions the row.
3. **"Line, not box" while typing.** Bordered context rows (`│ …`) are only
   produced for MULTI-line drafts (`compose_draft_context_rows`); a single typed
   line is just `kitsu> text`. No top/bottom border, no width frame.

All three share one root: **there is no persistent input-box surface.** kitsu
pins one bottom row (plus dim context rows for multiline); Claude CLI draws a
standalone framed rectangle that always exists.

---

## Requirement A — persistent bordered input box
- A framed region (`╭─…─╮ / │ … │ / ╰─…─╯` or `❯` + rule, TBD style) that is
  **always visible**, even empty, with the caret inside it from launch.
- **Auto-expands in height** as lines are added (cap to terminal height, then
  scroll — the `DRAFT_CONTEXT_ROWS` budget already added gives the height-grow;
  the border + always-on is the missing part).
- **Auto-expands/fits width** to the terminal (full-width rule like the sample).
- The caret is owned by the box and positioned correctly on every keystroke
  (fixes bug 2) and moves with arrows/Home/End/word-motion.
- Status meter (`<model> · ■□□□`) stays, but as a line BELOW or ABOVE the box,
  not instead of it.
- Must not break: the pinned-viewport repaint model, Ctrl+O transcript overlay,
  the agent-view overlay, suspend/resume, resize (SIGWINCH), classic/fullscreen
  modes (box is inline-mode only; classic stays rustyline).

## Requirement B — copy-box (paste as an editable block)
- **Threshold:** a paste becomes a copy-box when it is "more than a couple of
  words". Proposal: collapse when the paste is **≥ 2 lines OR ≥ ~N words**
  (N≈10, TBD). Shorter pastes insert inline as text (today's behavior).
  - Only reliably detectable inside a **bracketed paste** block
    (`ESC[200~`…`ESC[201~`); unbracketed terminals can't distinguish paste from
    typing. kitsu already brackets (`editor lib.rs` in_paste path).
- **Display token:** `[Pasted text #N +M lines]` (or `[N words]`), rendered
  inline where the paste landed, gray/dim. `#N` is a per-session counter.
- **Edit-into:** navigate INTO the box to edit its contents — via Ctrl+arrow or
  Shift+arrow (TBD which; must not collide with word-motion `Ctrl+←/→` already
  bound, or the Shift+←/→ agent-cycle already shipped — likely a dedicated key).
- **Delete as a unit:** Backspace/Delete at the box's leading/trailing boundary
  removes the ENTIRE box, not one char.
- **Gray hint line** below the input box (like the sample's `⏵⏵ auto mode…`
  footer) showing the active affordance, dim color.
- On **submit**, the box expands back to its full pasted text (the model gets
  the real content, never the placeholder).

## Requirement C — gray hint line
- A dim (theme "dim"/gray) one-line hint below the input box, context-sensitive
  (e.g. "↵ send · Ctrl+arrow edit paste · Ctrl+D exit"). kitsu has hint plumbing
  (`LiveDraft.hint`) but it's inline on the row, not a separate footer line.

---

## Architecture analysis (why this is a real project, not a tweak)

### The hard constraint: the editor buffer is a flat `Vec<char>`
`kitsu-editor::TypeaheadEditor` is `buffer: Vec<char>` + `cursor: usize`
(`lib.rs:112`). It is simultaneously the content AND (1:1) what renders. A
copy-box needs a **span model**: the buffer must distinguish literal text from
one-or-more "paste spans" that display collapsed (`[Pasted …]`) but submit
expanded, with:
- cursor arithmetic that treats a collapsed span as width-1 for motion but
  stores full text;
- backspace/delete at a span boundary removing the whole span;
- an "enter the span" edit mode;
- windowing/soft-wrap that accounts for collapsed spans.
This touches every insert/delete/motion path in the editor — the most
load-bearing input component. It is the invasive change flagged earlier.

### The rendering side: a new box surface
Today's composer is `compose_row` (one status/prompt row) + `compose_draft_context_rows`
(dim `│` rows for multiline) painted into the pinned bottom viewport
(`live.rs` redraw_locked, `tui_bridge` paint_composer). A real box needs:
- a persistent framed region that exists at idle (empty draft → still a box);
- top/bottom borders + full-width rules + the caret owned inside;
- height that grows with content and scrolls past a cap;
- the status meter relocated to its own line;
- a separate dim hint footer line.
This is a rewrite of the composer paint path, not a parameter change — and it's
exactly the area that can only be verified by watching it live (every prior
blind edit to cursor/layout risked regressions).

### Collisions to respect
- **Copy-paste sanctity:** no mouse capture; selection stays native.
- **Key bindings already taken:** `Ctrl+←/→` = word motion; `Shift+←/→` = agent
  cycle (shipped); `Ctrl+X …` = chords; `Ctrl+O` = transcript; `Ctrl+R` =
  reverse search. The "enter the paste box" key must avoid all of these — a new
  dedicated binding, or a modal "paste-edit" mode entered from the box boundary.
- **Round-trip / unstyled tier:** the submitted text and the `messages` history
  must always be the full expanded paste, never the placeholder.

---

## Proposed phasing (each independently shippable + live-verifiable)
1. **Persistent bordered box + caret fix** (Req A, bugs 1–3). Biggest visual
   win; fixes the stranded cursor. Pure rendering + caret ownership; no editor
   span model yet. Verify live (Terminal.app, iTerm2, tmux, resize).
2. **Gray hint footer line** (Req C). Small once the box exists.
3. **Copy-box: display + submit-expand** (Req B, read-only token): collapse a
   bracketed paste ≥ threshold into `[Pasted #N +M]`, expand on submit. No
   edit-into yet; backspace-at-boundary removes the whole span. This is the
   editor span-model change — the invasive core work.
4. **Copy-box: edit-into** (Req B, enter/exit the span to edit). Last, builds on
   the span model.

### Decisions (made 2026-10-20)
1. **Threshold:** collapse a paste into a copy-box when it is **≥ 2 lines OR
   ≥ 10 words**. Shorter pastes insert inline as text.
2. **Box style:** `╭─ … ─╮ / │ … │ / ╰─ … ─╯` frame matching kitsu's kitsune
   banner, **full-width** rule (the Claude `❯`-box look, kitsu-skinned).
3. **Copy-box editing = a separate POPUP overlay, NOT in-line span editing.**
   The copy-box is a display token in the main input. To view/edit it, open a
   popup overlay showing the full pasted text; **Esc exits** (saving edits).
   **No recursive paste-box inside** the popup — it holds plain full text only.
   This sidesteps the flat-`Vec<char>` span-model problem for EDITING: the main
   buffer only needs to store the token + its backing text, and the popup is a
   normal text editor over that backing string. Boundary-delete still removes
   the whole token. Reuses the alt-screen/MODAL_PANE overlay machinery already
   built for the model picker / agent view.
4. **Status meter goes ABOVE the input box.**

### Non-goals / risks
- Classic + fullscreen modes keep current behavior (box is inline-only).
- The whole thing needs live iteration on real terminals; it cannot be
  validated from unit tests alone (the pure row-builders can be, the caret
  placement and resize cannot).
