# E11 / R6-prompts — the ring retains the user's prompts (F102 resolved)

- **UTC start/end:** 2026-10-16T04:05Z / 2026-10-16T01:05Z
- **Branch/base:** `features/tui_herdr_improvements` / `53be36b`
- **Epic/story/card:** E11 / **R6-prompts** (Piece 1 of the F102 fix). **Resolves
  F102**, **unblocks R6-c**, records **F107**-**F109**.
- **Requirement:** R6 (amended), N4.

- **Why this card and not the next numbered row.** R6-a shipped an overlay that
  **showed one speaker**. Searching half a transcript (R6-b) is worth less than
  making it whole, and the same feed change was the **only** thing blocking R6-c.

- **What shipped.** `CellLine` carries a `prompt` flag, set only through
  `CellRing::push_prompt`, which splits on `\n` -- **one cell per line**, because
  both frames are one `fit_row_to_width` per cell and a single multi-line cell
  would have garbled them. Prompts file as `Stdout` (the conversation) and not
  `Stderr` (tools and meta). **Three feeds**: the foreground submission, the
  fullscreen tap -- whose ring is the **only** ring in that mode -- and a
  **delivered** attached submission.

- **This was not the F92 blast radius, and measuring said so.** `CellLine` has
  **one construction site, private fields and no public constructor**, so **no
  test in the workspace can build one**; adding a field was a one-site change.
  The final production diff is four sites across three files with **one
  deletion**. `push_retained(&CellLine)` takes a *line*, not a triple, so the mark
  can only come from a line that already carries it -- the no-public-constructor
  property survives.

- **`frame_ring` was the catch that mattered, and it was not in my brief.** It is
  the chokepoint **both** readers go through and it rebuilt from `(fd, styled)`
  alone. A mark stored faithfully and dropped there would have left the feed
  perfectly correct and **every reader still seeing one speaker** -- and R6-c with
  nothing to jump between. **F79**: when a function starts carrying something new,
  every caller is a suspect.

- **Four decisions, each with a reason that outlives the card.** Slash commands
  **are** retained (a report with no visible command is half a transcript).
  **Injected** `/resume`/`/retry` are **never marked**: the words are kitsu's, and
  the text cannot carry provenance because a user may type the same words -- so an
  explicit `PromptOrigin` is captured where `priority_command.take()` happens.
  A **refused** attached submission is not retained: it was handed back to the
  composer and **never said**. And `Stdout` for prompts, so a reader taking
  `Stdout` gets the conversation and one taking `Stderr` gets the tools.

- **F108 -- the link no in-process test could see.** The injected-command test is
  satisfied by *retained unmarked*, by *not retained*, **and by a feed that never
  fires**; the author said so and **declined to claim more**, which is what made
  the gap visible. The implementer closed it by **mutating a live binary**: a real
  pty session logged `User /help`, then the single binding at the capture point was
  flipped, the binary **rebuilt**, and the identical session logged `Injected
  /help` -- **byte-identical text**, so the origin comes from the binding and not
  the text. Plus both arms proven load-bearing (1 failure vs 6).

- **F107 -- a mutating reader, and a fix that was 60% of one.** `kitsu-tui`'s
  `ALTERNATE_SCREEN_TAKEN` is a process global, and **`write_restore_sequences`
  clears it while reading it.** The two obvious tests were serialised on a
  test-local lock; a **third**, which calls that function only to check sequence
  ordering and asserts nothing about `ESC[?1049l`, **had been clearing its
  neighbour's flag and passing**. 2 deviations in 250 runs became 0 in 250 + 80.
  **Three process-globals are now implicated in flake families**; the production
  fix is carded, the locks are named as mitigations. Also: `#[serial]` would not
  have compiled -- `serial_test` is not a dependency, and the belief that it was
  came from grepping the **word**, which matched a prose comment (**F74** on a word
  match).

- **Acceptance commands/results, both profiles:**
  ```
  cargo test -p kitsu --bins, both        955 passed, 0 failed
  cargo test -p kitsu-tui --lib, both     127 passed, 0 failed
  cargo test -p kitsu --test tui           75 · --test cli 245
  kitsu-render 138 · kitsu-editor 42 (+9) · kitsu-tools 413 · paste_latency 4
  cargo clippy --workspace, both, -D warnings    0 warnings
  ```
  20 consecutive default-parallelism runs fully green. All three routes proven in
  **live pty sessions**, not only in tests. The maintainer's `fs.rs` is at `54 20`.

- **Documentation (standing requirement, in this commit):** **F102 marked resolved
  in R6**; the plan gains this row and **R6-c moves from BLOCKED to unblocked**; and
  **the two user-facing claims I wrote one commit ago are corrected** -- the
  CHANGELOG's *"your own prompts are absent"* and `functionality.md`'s *"It also
  does not hold your own prompts"*. **F109** records the rule that emerged: R6-a's
  **cycle record was deliberately left false-looking**, because a dated record of
  what was believed then is the only honest account of why R6-a shipped a
  one-speaker overlay.

- **Commit:** see below.
- **Residual risk:** **F52 now has an adversarial input** -- user paste is the one
  input nothing in the process limits, and a single multi-megabyte line with no
  newline becomes one unbounded cell in a ring bounded by *lines*. **R6-d stays
  gated**: the requirement forbids carding it until the dump's
  confirmation-or-opt-out is decided, because `[` would move a pasted credential
  into a scrollback kitsu cannot bound or clear. **F107's production fix** is the
  next natural card. **R6-b** still needs a twelfth transition on an
  eleven-row table. Ten flakes, three globals. **Q1's terminal matrix needs a
  human.**
- **Next:** **R6-c** (now unblocked -- decide whether adjacent marked rows
  coalesce; recommended yes, since a multi-line prompt is several marked cells),
  **F107's production fix**, or **R6-b**.
