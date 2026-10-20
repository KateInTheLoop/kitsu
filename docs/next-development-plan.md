# kitsu — next development plan

Written 2026-10-20 after reviewing `feat/tui-rendering` (24 commits on `main`,
all local, authored CatherinaInTheLoop). Supersedes the "still open" list in
`tui-interactive-roadmap.md`. Review evidence: `docs/review-feat-tui-rendering.md`
(written by the verification pass; findings below are cited by its numbers).

## Where we are

Shipped on the branch: `--effort` presets, interactive/`--yes` cwd read+write,
macOS Seatbelt command containment + live tests, model popup picker, Shift+←/→
agent cycle, resume id on exit, markdown rendering, and the framed input box
(now the default; `KITSU_INPUT_BOX=0` opts out).

What changed in the picture:
- The code review found 10 issues. Two are holes in the Seatbelt sandbox and one
  ends the session on a keypress. The branch should not be pushed or merged
  until those are fixed.
- Item "caret across wraps" from the old list is bigger than an untested path:
  Up/Down are hard-bound to history recall (`kitsu-editor/src/lib.rs:372,572`),
  so you cannot move up through your own multi-line draft at all.
- Item "default vs flag" is done.

## Rule for all TUI work

Pure geometry first, with unit tests. Then wire it and verify on a real
terminal (stock Terminal.app, plus iTerm2 and tmux before anything is called
done). No terminal change is reported as working until it has been seen.

---

## Milestone 0 — Stabilize the branch (do first)

Verified by the aggregation pass (`docs/review-feat-tui-rendering.md`): 9 of
10 confirmed. Severity HIGH 3 (1, 3, 4 below), MEDIUM 4, LOW 3. Two sub-claims
did not hold: the launchd spawn-to-unsandboxed escape is PLAUSIBLE, not proven
(`launchctl list` worked, `launchctl submit` failed on this macOS); the "CLI
help is stale" part of #10 is REJECTED (help is already correct; only two code
comments are stale). Confirmed live: writes to $HOME are denied, but `~/.zshrc`
is readable and `~/.ssh` is listable from inside the sandbox.

Fix order (verified): cheap user-visible breaks first, then the sandbox, then
polish — **3, 4, 5, 1+2, 7, 6, 8+9, 10**.

**Decision needed for 1+2:** the open read and exec boundaries were deliberate
(the profile's own comments say so). Is "a granted command can read your home
directory" acceptable for a locally-approved command? Recommendation: no — deny
the secret paths explicitly and narrow mach-lookup, since the gateway JWT lives
in `~/.zshrc`.

| # | Issue | Why first | Fix direction |
|---|---|---|---|
| 1 | Seatbelt `allow file-read*` + `allow mach-lookup` | Contained command can read ~/.ssh, ~/.aws, kitsu tokens, and launch an unsandboxed process via launchd | Keep reads broadly open (dyld needs it — a read allow-list SIGABRTs) but add explicit `deny file-read*` for secret paths: `~/.ssh`, `~/.aws`, `~/.gnupg`, `~/.config/gcloud`, `~/Library/Keychains`, kitsu config + state dirs. Replace `allow mach-lookup` with a global-name allowlist found empirically (what git/sh/cargo need). Live tests must prove a secret read fails and `open`/`osascript` cannot escape. |
| 2 | Seatbelt `allow process*` | Child can exec any binary; Linux can't | Restrict `process-exec*` to the system dirs + command roots + runtime roots (the same set bwrap mounts). Keep `process-fork`. Live test: `/bin/sh -c /opt/homebrew/bin/x` denied unless that dir is a root. |
| 3 | Shift+←/→ in fullscreen → `Eof` | Ends the session, loses the draft | Gate `CycleAgent` decoding on enabled keys, and give the fullscreen loop an explicit inert arm. Test: KeyFilter with disabled keys never yields `CycleAgent`. |
| 4 | Box parks the cursor inside the pinned region | Transcript writes land in the box | Park back in the scroll region before any transcript write; only own the caret while the composer is reading. Same mechanism the single-row composer uses. |
| 5 | Status row gone at idle; 3-row box left pinned during a turn | Lost status/meter; broken frame pieces | Paint the status row above the box at idle (this is decision 4 — status meter above the box). Post-read reset collapses back to one row. |
| 6 | Caret row from a fresh `terminal_rows()` and unclamped box height | Wrong row on short terminals / mid-resize | Clamp the box to the latched grant before composing; keep the top border by dropping content rows, not the frame; compute the caret from the latched height. |
| 7 | Model popup Tab and Ctrl-C never fire | Header advertises keys that do nothing | Decode 0x09 and 0x03 in `decode_overlay_keys` (or a popup-local decoder). Test through the real decoder, not hand-built keys. |
| 8 | `*` italic eats glob paths (`src/*.rs`) | Wrong paths shown | Word-boundary rule for `*` like `_`: closing `*` must not be followed by an alphanumeric, opening must not be preceded by one. |
| 9 | Heading style lost after an inline span | Half-styled headings | Re-apply the heading SGR after each inner reset, or style segments individually. |
| 10 | `cwd_grant` docs/help stale | Behavior change was intended; docs say otherwise | Update the doc comment, `--help`, and README to say interactive mode can write inside the launch dir and still prompts outside it. |

Also in Milestone 0:
- **Linux compile check.** The Seatbelt refactor changed shared code and the
  Bubblewrap backend, and has never been compiled for Linux. Install the
  `x86_64-unknown-linux-gnu` target (or run in a Linux container/CI) and run the
  bwrap live tests. Must pass before push.
- Re-run the full suites; the only allowed failures are the known pre-existing
  macOS ones.

Exit criteria: all CRITICAL/HIGH findings fixed with tests, Linux build green,
live check of the box on Terminal.app + iTerm2 + tmux.

---

## Milestone 1 — Make the input box a real editor

1. **Up/Down move within the draft.** In a multi-line draft, ↑/↓ move the caret
   by visual row (respecting soft wraps). History recall only happens when the
   caret is already on the first row (↑) or last row (↓) — the Claude CLI
   behavior. Needs the editor to know visual-row geometry, so the wrap math
   (`input_box_content` / `locate_caret_in_segments`) becomes shared between
   render and editor rather than render-only.
2. **Caret across wraps, verified.** Arrow through wrapped rows both ways,
   Home/End per visual row vs per logical line (decide), word motion across a
   wrap boundary.
3. **Resize.** Box re-wraps on SIGWINCH without leaving frame fragments.
4. **Legacy renderer.** Decide whether `KITSU_INPUT_BOX=0` stays supported. If
   yes, fix its "cursor above `kitsu>`" bug (no caret at an empty draft). If
   no, remove the flag and the dead path after one release.

## Milestone 2 — Copy-box

Decisions are already made (`tui-input-box-requirements.md`): collapse a paste
of ≥2 lines or ≥10 words; kitsu-banner style; edit in a popup (Esc exits, no
nested paste); status meter above the box.

1. **Token model in the editor.** The buffer stays `Vec<char>` for typed text
   and gains paste spans: `[Pasted text #N +M lines]` as one display unit
   backed by the full text. Cursor treats a span as one unit; Backspace/Delete
   at its edge removes the whole span. Detection only inside bracketed paste.
2. **Submit expands** spans to their full text. The `messages` history and the
   API never see the placeholder. Test this first — it's the correctness line.
3. **Popup edit.** A key on a span (or entering it with the caret) opens an
   overlay editor over the backing text, reusing the model-picker overlay
   machinery. Esc saves and closes. Plain text only inside.
4. **Key choice for "open span".** Must avoid Ctrl+←/→ (word motion),
   Shift+←/→ (agent cycle), Ctrl+X chords, Ctrl+O, Ctrl+R. Proposal: Enter
   while the caret sits on a span opens it; Tab cycles spans. Decide before
   building.

## Milestone 3 — Gray hint footer

One dim line under the box with the current affordances (`↵ send · ⇧↵ newline ·
Tab next paste · Esc …`), context-sensitive. Small once Milestones 1–2 define
the keys.

## Milestone 4 — Later (from the roadmap, unchanged order)

- Codex-style expandable diff windows with `+N/−M` per file (reuse `/diff`
  evidence).
- Multi-window / popup framework, optional fox side pane, navigation help.
- tmux auto-detect (coexist, no double alt-screen).
- Windows containment backend (AppContainer / restricted token) in the
  `default_containment_backend` slot; WSL2 uses the Linux path.
- Integration tests on the Linux side to mirror the five macOS live tests.

---

## Housekeeping (any time, small)

- **Push identity.** Before the first push, confirm the github.com keychain
  account `mmpg-x86` is personal, or log `gh` into the personal github.com
  account. Commits are already all CatherinaInTheLoop; the corporate `gh`
  login is for github.sie.sony.com only.
- **Branches.** Decide on `backup/*` and the older `feat/local-autodetect`,
  `fix/*` branches. Nothing deleted without a yes.
- **PR shape.** `feat/tui-rendering` contains the whole stack. Either one PR, or
  split into the existing branch layers (effort → picker/agent-cycle →
  Seatbelt → rendering) so the security-sensitive Seatbelt change gets its own
  review.
- **Docs.** README + CHANGELOG entries for the box default, cwd write, resume
  hint, markdown rendering.

## Suggested next step

Milestone 0, items 1–3 (both Seatbelt holes and the fullscreen session-ending
key), each with a test, then the Linux compile check. Then the box items (4–6)
with you at the terminal.
