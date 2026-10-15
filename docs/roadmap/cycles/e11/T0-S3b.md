# E11 / T0-S3b — `write_block`, the suspension rule, and the routed paths

- **UTC start/end:** 2026-10-13T20:37Z / 2026-10-13T21:24Z
- **Branch/base:** `features/tui_herdr_improvements` / `94fa446`
- **Epic/story/card:** E11 / T0-S3b. The second half of the split T0-S3.
- **Requirement:** none directly; this is the seam the T0 renderer needs.

- **What this card actually achieves, stated precisely**, because the obvious
  reading would be wrong: `write_block` exists, the suspension decision has
  moved into the backend, and `render_error` genuinely carries its bytes
  through the seam. But **stdout content still does not flow through it** — see
  the gap below. The plan and the code now say so in three places rather than
  letting the routing imply otherwise.

- **Evidence and expected failure:** 9 tests written first; eight errors
  including `method write_block is not a member of trait Surface` and
  `unresolved import super::surface::suspends_for`. The author verified all
  nine against a throwaway implementation (102 passing) and reverted it.

- **The subtlety that decided the design.** `print_fragment` suspends the live
  row on its styled-stdout branch and not on the other, and that asymmetry is
  correct rather than an oversight: the row is painted on **stderr**, so when
  stdout is redirected to a file those bytes cannot disturb it, and lifting it
  anyway would insert a clear-and-repaint cycle that changes the emitted
  *stderr* bytes. A `write_block` that always suspended would have broken
  `kitsu chat > file` on a terminal. The rule is therefore derived by the
  backend, not passed in by callers:

  ```rust
  pub(crate) fn suspends_for(fd: Fd, stdout_styled: bool) -> bool {
      matches!(fd, Fd::Stderr) || stdout_styled
  }
  ```

  Pure and injectable, because `stdout_style()` reads a real descriptor and
  cannot be driven from a test. Its truth table is pinned with the reason in
  each assertion message.

- **The gap, which is the most important thing in this record.** The test
  author found, and I verified, that `StreamBuffer::push` and
  `flush_unstyled` write `io::stdout()` **themselves** rather than any writer
  passed in. So after routing:
  - `print_fragment`'s styled branch hands an **empty** block to a backend;
  - `finish_line` with a buffered partial line emits that line through the
    real descriptor while only the trailing newline reaches the writer.

  For `ClassicSurface` the bytes are identical — same descriptor, same order,
  inside the one suspension — so this card is byte-identity-safe. But the seam
  **is not yet substitutable for stdout**, and a terminal-UI backend plugged in
  today would receive nothing for assistant text, which is the single thing it
  most needs. That is a trap rather than a gap, because the routing makes the
  path look handled. It is recorded as finding **F12**, as blocking plan card
  **T0-S3c**, and — most usefully — as a `GAP — read this before plugging in a
  non-classic backend` comment at both routing sites, since that is what the
  next implementer will actually read. T0-S4 and T0-S5 must not assume stdout
  flows through the seam until T0-S3c lands.

  Threading the writer was deliberately **not** bundled here: it touches the
  line styler and both functions' byte-count returns, which is exactly the
  drift risk that splitting T0-S3 existed to avoid.

- **One constraint the card did not anticipate.** `write_block` needs a
  **default body** on the trait, because three existing test types
  (`Recorder`, `Reentrant`, `Gate`) implement `Surface` and are tests that may
  not be modified; a required method would not compile. The default buffers the
  block and forwards each line to `transcript_line`, so those three still see
  every byte, and its doc says plainly that a backend not overriding it loses
  the single-suspension property.

- **Tests added or changed:** 9 added, none changed. The regression guard for
  the previous card's finding is the sharpest: `render_error` must route
  **exactly one** block, not one call per line, and the block's bytes must equal
  the newline-terminated concatenation of `error_lines`. A second test samples
  the suspension depth after each line of a three-line block and asserts it
  rises at most one level and returns to its starting value. The partial-line
  case pins both halves of the tricky semantic: nothing is written on the
  styled tier, yet both output-tracking atomics are still set, because a turn
  whose only output is a partial line must still count as having streamed.

- **Review findings and fixes:**
  1. Verified by reading rather than by report: the flush the implementer
     relocated out of the raw branch now happens inside `write_block`'s own
     closure, which is what `with_live_row_suspended` receives — so the
     write-then-flush ordering is preserved **inside** the suspension on the
     styled branch and outside it on the raw branch, matching today exactly.
     That was the one line of the three routed bodies not literally verbatim,
     and the implementer flagged it rather than letting it pass unremarked.
  2. Verified the atomics are still stored before the `write_block` calls and
     outside the blocks, and that no removed line touches `StreamBuffer`, so
     the deferred threading provably did not happen by accident.

- **Acceptance commands/results:**
  ```
  cargo test -p kitsu-render            102 passed  (93 baseline + 9)
  cargo test -p kitsu-render --release  102 passed
  cargo test -p kitsu --bins            637 passed  (unchanged)
  cargo test -p kitsu --bins --release  637 passed
  cargo test -p kitsu --test cli        229 passed  (unchanged)
  cargo clippy --workspace --all-targets  0 warnings
  cargo fmt -p kitsu-render -- --check  clean
  workspace total, all targets         1824  (1815 + 9, nothing lost)
  ```

- **Documentation (standing requirement, in this commit):** `design.md`
  describes `write_block`, the one-suspension-per-block rule and the F12
  caveat; finding F12 is recorded; the plan gains T0-S3c marked **blocking**
  and its progress table records S3b done. **No `CHANGELOG.md` entry and no
  `docs/ux-design.md` change, deliberately:** every byte is identical, so
  nothing user-visible moved.

- **Commit:** see below.
- **Residual risk:** F12 dominates — the seam cannot carry stdout yet, and the
  next two cards depend on it. Beyond that, the styled tier of two tests
  cannot execute under `cargo test` because `stdout_style()` reads a real
  descriptor, so those tests assert the correct expectation for whichever tier
  they run on and the byte-changing half (the suspension decision) is pinned
  through `suspends_for` instead. Real descriptor bytes remain uncapturable;
  only an end-to-end PTY test would show write ordering against the row's
  escapes, which is T3-S1's job.
- **Next card:** T0-S3c — thread the block's writer through
  `StreamBuffer::push` and `flush_unstyled` so the seam can actually carry
  stdout, preserving the styler's fence state and both byte-count returns.
