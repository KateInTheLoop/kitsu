# E11 / T0-S5d — the inline idle input path

- **UTC start/end:** 2026-10-13T23:42Z / 2026-10-14T00:35Z
- **Branch/base:** `features/tui_herdr_improvements` / `922935f`
- **Epic/story/card:** E11 / T0-S5d
- **Requirement:** R1, R3. Resolves **F14**; records **F20** (amended) and
  **F21**.

- **What shipped.** Inline mode now has an idle prompt. Typing is echoed on the
  pinned viewport row rather than by the terminal, the draft submits through the
  existing prompt path, and an unsubmitted draft carried over from the
  during-turn editor is replayed as if typed, so the "typing is never dropped"
  contract survives the switch. Before this card the viewport showed a static
  string while Rustyline still owned the prompt.

- **The question I settled before briefing anyone, which collapsed the card's
  risk.** F14 said a raw-mode backend must emit CRLF, and the open worry was
  what would happen to the binary's several hundred direct `println!` writers
  once a session held raw mode — potentially routing hundreds of call sites.
  There turn out to be **two** raw modes in this tree: crossterm's, which goes
  through `cfmakeraw` and clears `OPOST`/`ONLCR`, and the live row's own
  `RawStdinGuard`, which touches **only `c_lflag`** and leaves the output
  flags alone. Reusing `RawStdinGuard` means the terminal still supplies the
  carriage return, so **nothing staircases and no writer needs routing**. The
  test author measured it on a real pseudo-terminal rather than taking my word:
  with the guard demonstrably engaged, the child emits `\n` and the master
  reads `\r\n`. A regression test holds it, and its precondition fails if
  Rustyline is still running the prompt, so it cannot quietly degrade into
  re-measuring the cooked path. **This also completes the Q3 answer that T0-S5b
  could only give for cooked mode.**

- **Evidence and expected failure:** 11 pure unit tests on the reader plus 2
  pseudo-terminal tests. Failures: `not yet implemented: the inline
  idle-prompt reader` ×11, and the two end-to-end tests printing the screen —
  one showing the draft echoed by Rustyline *inside* the scroll region with the
  slash palette open, the other refusing to measure at all while Rustyline owned
  the prompt. The author verified every assertion was satisfiable by temporarily
  implementing the body, then reverted it.

- **The trap, which a naive implementation loses silently.** Up-arrow on an
  empty draft makes the editor emit `RecallRequest` rather than walking
  history — it exists so the during-turn collector gets first refusal for the
  prompt queue. The idle reader has no queue, so it must decline by calling
  `history_prev_forced`, or Up-arrow does nothing at all. The test author found
  it and pinned it; the implementer confirmed it by reading the editor
  (`history_prev` returns early whenever the buffer is empty and the cursor is
  `None`) and mirrored the collector's own decline path.

- **Review findings and fixes:**
  1. **I rejected the interrupt semantics and changed them.** The implementer
     re-raised `SIGINT` on an empty-draft Ctrl+C, faithfully following the
     during-turn collector — and flagged, to its credit, that this might differ
     from classic mode. It does, and worse than it guessed: **every**
     `ctrl_c()` in this binary sits inside a `select!` scoped to one
     operation, so no process-lifetime handler exists, and at the *first* idle
     prompt the raise takes the default disposition and **kills kitsu**.
     Classic mode prints `^C (input cleared; Ctrl+D or /quit exits)` and keeps
     the session. The collector raises because a turn is in flight and the
     signal is what cancels it; at an idle prompt there is nothing to cancel.
     Removed the raise — the existing `Interrupted` arm already does the right
     thing — and recorded the reasoning in the code.
  2. Removing it left the raw-mode guard bound but unused, which clippy caught.
     Renamed to `_raw` **rather than `_`**, with a comment: a bare `_` drops
     the guard immediately and would restore canonical mode before the first
     keystroke.
  3. Accepted a design choice the implementer made that the card did not
     anticipate, because it is right: the pinned row shows the **status** while
     the draft is empty and the composer only once something is typed. An
     always-on `kitsu> ` would claim the row for an empty editor, and an
     existing test samples that row mid-read with an empty draft.

- **Acceptance commands/results:**
  ```
  cargo test -p kitsu --bins / --release    654 passed each  (643 + 11)
  cargo test -p kitsu --test tui / --release  6 passed each  (4 + 2)
  cargo test -p kitsu --test cli           229 passed        (unchanged)
  cargo test -p kitsu-tui / -p kitsu-render 28 / 112         (unchanged)
  cargo clippy --workspace --all-targets      0 warnings
  cargo fmt -p kitsu -- --check               clean
  workspace total, all targets             1873 passed, 0 failed
  ```
  Verified independently: no `enable_raw_mode` call exists anywhere on the
  binary's path, so `c_oflag` is untouched; and the binary reaches
  `kitsu-tui` only through `InlineSurface`, never `TuiRuntime`.

- **Documentation (standing requirement, in this commit).** This is the first
  card where the preview is actually usable, so unlike the previous three it
  does get user-facing documentation. `README.md` documents `KITSU_TUI=inline`
  where the other render modes are described, including that the value is
  matched exactly and that an unset variable changes nothing — and, deliberately,
  the full list of what the preview prompt *lacks*. `CHANGELOG.md` gains a
  "Terminal UI (preview)" subsection under `[Unreleased]` naming both the
  feature and the gaps, because a changelog entry that advertises only the good
  half invites bug reports for known holes. `docs/ux-design.md` gains the
  status-versus-composer rule, the input-flags-only raw mode and why it matters,
  the Ctrl+C divergence and its reason. **F14** marked resolved with the
  measurement; **F20** amended after the author's transcript showed Rustyline's
  loss is wider than I first recorded — the slash-command palette, not history,
  is the most visible casualty, and restoring it forces the viewport past one
  row; **F21** opened for Ctrl+Z. Plan status updated.

- **Commit:** see below.
- **Residual risk:** **F20 is now a milestone exit condition, not a detail** —
  inline mode cannot become a default while it lacks the palette, completion and
  persistent history, and the palette drags the multi-row composer in with it.
  **F21**: Ctrl+Z is swallowed, so inline mode does not honour a shell
  convention; the real work there is releasing the scroll region before the
  process stops and reinstalling it on resume, since otherwise the shell
  inherits a terminal with a region still set. The reader also cannot reach the
  editor's lone-Escape path, which needs a timed poll rather than a blocking
  read. And the narrow systemic exposure: any future code reaching for
  crossterm's raw mode on the binary's path would clear `OPOST` and break every
  direct writer at once.
- **Next card:** T0-S6 — the go/no-go record with the terminal matrix, which
  can now answer Q1-Q4 with measurements rather than estimates.
