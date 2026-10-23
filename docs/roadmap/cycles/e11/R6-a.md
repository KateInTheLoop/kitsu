# E11 / R6-a — the transcript overlay, rejected twice

- **UTC start/end:** 2026-10-16T02:10Z / 2026-10-15T23:31Z
- **Branch/base:** `features/tui_herdr_improvements` / `f27b47e`
- **Epic/story/card:** E11 / **R6-a**, first of four. R6 was **split four ways at
  this card's review** and **amended twice**; `T4-S9` and `T5-S3` are superseded.
  Records **F102**-**F106** and F101's third instance.

- **What shipped.** `Ctrl+O` opens a transcript overlay over the transcript the
  cell ring retains, from the inline composer **and** from fullscreen. The four
  paging keys scroll it; `q`/`Esc` close it, replaying the draft and leaving the
  frame behind it exactly where it was.

- **The design, decided at review.** `TranscriptOverlay` **holds** a
  `FullscreenView` and delegates every scroll: **one anchor implementation, two
  instances.** Drift is two implementations of one rule; two instances of one
  implementation cannot disagree about the rule, and they **must** be separate,
  because the overlay's position and the frame behind it are different facts --
  and from the inline surface there is no fullscreen frame to share with at all.
  The codebase had already settled this shape: **two rings, one view over them.**
  The footer row comes **out of** the window (`content_rows == rows - 1`), because
  a footer over a transcript row costs the user a line of what they opened the
  overlay to read.

- **R6 amended twice, on measurement (F102, F103).** `{`/`}` prompt jumps are
  **unsatisfiable**: `CellLine` carries only `fd` + `styled`, and the submitted
  line is painted by the composer then **erased**, so **the ring holds no prompts
  at all** -- the overlay shows one speaker. R6-c is blocked on a *feed* change.
  And `[`'s *"dump to native scrollback, fullscreen only"* is near-contradictory:
  bytes written under `ESC[?1049h` are **discarded** on `ESC[?1049l`, so the dump
  must leave the alternate screen first, making it a hand-off, not a write.

- **Rejected once (F105): `Esc` did not close the overlay.** The partial-hold runs
  **before** the ESC arm, and a bare `ESC` is a strict prefix of all four paging
  sequences, so it was held and the loop went back to a blocking read. Advertised
  in the footer. **And the sweep that should have caught it tested the predicate in
  the view crate, never the byte loop** -- a total enumeration of the keyboard that
  proved the wrong layer answers `Esc`. **F69** at a worse angle: not a mirror that
  diverged, but a different layer, which looks even more like coverage.

  Fixed by extracting `run_transcript_key_loop` so the test drives the real
  decoder, resolving the lone `ESC` with **the agent view's own grace** -- the
  existing function and the one 50 ms constant, verified defined once. **The
  honest signal is time; a chunk boundary is not a signal.**

  **The proof is bidirectional:** restore the old decoder and the lone-`ESC` tests
  fail `Drained != Closed`; disable the `ESC [` skip and the split-arrow test fails
  `Closed != Drained`. Opposite failure modes, both pinned, which one direction
  cannot do.

- **Rejected twice (F106): `Ctrl+O` was advertised nowhere.** Every occurrence was
  a comment or a test string -- **F82** verbatim. **An in-situ footer cannot
  advertise its own entrance**: it is visible only once the overlay is open. The
  home needed no decision, only reading -- `/help keys` ends by pointing at
  `/help tui` for *"the composer's own chords"*, and `Ctrl+O` is one, read by the
  same reader as the paging keys. The test restates the footer's guard on the new
  string, so **the advertisement cannot promise R6-b/c/d** (**F77**'s failure).

- **Acceptance commands/results, both profiles:**
  ```
  cargo test -p kitsu --bins, both        944 passed, 0 failed
  cargo test -p kitsu-tui, both           122 passed, 0 failed
  cargo test -p kitsu --test tui, both     75 passed, 0 failed
  cargo test -p kitsu --test cli          245 passed
  kitsu-render 138 · kitsu-editor 42 (+9) · kitsu-tools 413 · paste_latency 4
  cargo clippy --workspace, both, -D warnings    0 warnings
  ```
  The maintainer's `fs.rs` is at `54 20` (**F33**).

- **Documentation (standing requirement, in this commit):** R6 amended twice in
  the design; the plan restructured into **R6-a..d** with both old rows retired
  **visibly**; `CHANGELOG` and `functionality.md` gain the overlay, the keys, and
  **the honest limitation that the ring holds neither diagnostics nor your own
  prompts**; the requirement-to-story map repointed. **F101's third instance fixed:
  `functionality.md` still said "paging is not wired" ten lines from the paragraph
  I added last card describing paging working.**

- **Commit:** see below.
- **Residual risk:** **R6-b** needs a **twelfth transition** on `fullscreen.rs`'s
  eleven-row table (`Anchor::Pinned { top }` is private and `page()` takes only the
  four presses, so "jump to the match" has no way in). **R6-c is blocked on the
  feed.** **R6-d** faces more than one path out of *"editor running"* -- non-zero
  exit, exec failure, `SIGTSTP` from inside it, a signal to kitsu while the child
  owns the screen -- and `$EDITOR` is read nowhere in the tree today. **Ten open
  flakes**, two now pointing at the process-global active-surface slot (**F104**).
  **F72**, the ordinal chord, the `r`-decision paste arm, the mid-turn counter
  freeze, **F67**, **F62**, **F60**, **F57**, **F53**, **F54**, **F51**, **F52**
  open. **Q1's terminal matrix still needs a human.**
- **Next:** **R6-b** (search), or **R6-d** (the two hand-offs). **R6-c stays
  blocked** until the feed marks prompts -- which is also what would stop the
  overlay showing one speaker, so it is worth more than its row suggests.
