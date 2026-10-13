# E11 / T0-S3c — thread the writer so the seam carries stdout (closes F12)

- **UTC start/end:** 2026-10-13T21:07Z / 2026-10-13T21:51Z
- **Branch/base:** `features/tui_herdr_improvements` / `fd65e82`
- **Epic/story/card:** E11 / T0-S3c. The card F12 created, and the last of the
  three the original T0-S3 became.
- **Requirement:** none directly. This unblocks T0-S4 and T0-S5, which could
  not legitimately proceed while the seam carried no assistant text.

- **What was wrong.** `StreamBuffer::push` and `flush_unstyled` wrote
  `io::stdout()` themselves, so after T0-S3b routed `print_fragment` and
  `finish_line` through `write_block`, a backend received an **empty** block
  for assistant text and a bare newline from `finish_line`. For the classic
  backend the bytes were identical, which is precisely why it passed every
  gate: the defect was invisible to the test suite and would have surfaced as
  "the terminal UI shows no assistant output" two cards later.

- **Evidence and expected failure:** 9 tests written first.
  `error[E0061]: this method takes 1 argument but 2 arguments were supplied`,
  at nine new call sites. The author verified all nine against a throwaway
  implementation (111 passing) and reverted it.

- **The testing insight that made this card tractable.** The styled branch of
  `print_fragment` cannot be driven from a test, because `stdout_style()`
  reads a real descriptor and `cargo test` captures stdout — the previous card
  had to assert whichever tier happened to run. Once the methods take a
  writer, that workaround is unnecessary: the threading is directly
  unit-testable by handing a `Vec<u8>` to a locally constructed buffer, with no
  tier dependency at all. Six of the nine tests take that form and hit exactly
  what the card changes.

- **A correction to my own survey, caught by the test author.** I told it that
  no test called either method. **That was wrong**: eleven calls across three
  existing tests use the old signatures. I had truncated my own grep at twenty
  matches and concluded from an incomplete list — the callers sat past the
  cutoff. Having a separate author who re-derives rather than trusting the
  brief is what surfaced it, and it is the second time in this epic that a
  signature change turned out to touch test call sites I had declared clean.
  **Lesson recorded: never pass a "nothing calls this" claim to an agent
  without showing the untruncated command.**

  Those eleven edits were then authorised explicitly and narrowly, and audited
  on return: of the 52 removed lines in the diff, every one is either a
  threaded signature, a stale comment, or one of the eleven bare calls.
  **Zero assertions, expected values or test names changed** — verified by
  grepping the removed lines for assertions, not by accepting the claim.

- **In scope:** both signatures, both call sites, removal of the direct stdout
  flush, the rewritten call-site comments, and the eleven arity edits.
- **Out of scope:** write-error reporting and the byte-count semantics, both
  left as they were against stdout.

- **The comments were rewritten, not deleted.** Both routing sites carried a
  `GAP — read this before plugging in a non-classic backend` block describing
  the problem this card fixes. Leaving them would have been worse than having
  none: a future reader would believe a resolved limitation still applied and
  might redo this work or avoid the seam. Each now states what is true — the
  block carries the styled line, or the partial line followed by its newline —
  and keeps the two caveats that genuinely remain: a failed write is still
  swallowed, and the returned byte count is computed from the formatted text
  rather than from what the writer accepted, so a short-writing sink would make
  the count a lie.

- **Tests added or changed:** 9 added; eleven existing lines edited for arity
  only. The sharpest is the fence-state test, because that is the one place
  threading could change behaviour rather than plumbing: a fenced diff block is
  split across three `push` calls and the interior line must arrive
  diff-styled. The author made it **non-vacuous deliberately**, asserting first
  that the fenced and unfenced renderings of the same line actually differ, so
  the test fails if styling were ever a no-op rather than passing silently. A
  companion proves `flush_unstyled` resets the styler by pushing a fence,
  flushing, then showing the next line is no longer styled as fenced. There is
  also a source-text guard asserting the `impl StreamBuffer` block contains no
  `io::stdout()`, so the decoupling cannot silently regress.

- **Acceptance commands/results:**
  ```
  cargo test -p kitsu-render            111 passed  (102 baseline + 9)
  cargo test -p kitsu-render --release  111 passed
  cargo test -p kitsu --bins            637 passed  (unchanged)
  cargo test -p kitsu --bins --release  637 passed
  cargo test -p kitsu --test cli        229 passed  (unchanged)
  cargo clippy --workspace --all-targets  0 warnings
  cargo fmt -p kitsu-render -- --check  clean
  workspace total, all targets         1833  (1824 + 9, nothing lost)
  ```
  Verified independently: zero `io::stdout()` references remain inside
  `impl StreamBuffer`.

- **Documentation (standing requirement, in this commit):** finding F12 is
  marked **RESOLVED** with what remains; `design.md`'s caveat is replaced by
  what is now true; the plan's progress table records the story done and its
  "T0-S4 and T0-S5 must not assume stdout flows through the seam" warning is
  struck through as resolved. **No `CHANGELOG.md` entry and no
  `docs/ux-design.md` change, deliberately:** every byte kitsu prints is
  unchanged, so nothing user-visible moved.

- **Commit:** see below.
- **Residual risk:** a failed write is swallowed, as it was against stdout, so
  a non-classic backend with a closed sink would silently drop output; the byte
  counts are computed from the formatted text and are ignored at both call
  sites, so a short-writing sink would make them inaccurate. Neither is a
  regression. The styled branch of `print_fragment` still cannot be exercised
  end-to-end under `cargo test`; its threading is pinned by the direct unit
  tests and the source guard instead, and an end-to-end PTY test belongs to
  T3-S1.
- **Next card:** T0-S4 — the inline viewport with injectable per-fd sinks,
  which can now legitimately assume the seam carries stdout.
