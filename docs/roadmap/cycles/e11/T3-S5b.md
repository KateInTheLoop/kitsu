# E11 / T3-S5b — the status row becomes visible

- **UTC start/end:** 2026-10-14T16:05Z / 2026-10-14T17:14Z
- **Branch/base:** `features/tui_herdr_improvements` / `8277767`
- **Epic/story/card:** E11 / T3-S5b — the wiring **T3-S5 deliberately scoped
  out**, taken next because three cards had shipped machinery no user could
  reach.
- **Requirement:** **R5** in production at last. Records **F51**.

- **What shipped.** The inline viewport's pinned row stops saying `kitsu` and
  becomes R5's live status row — model, eight-cell context meter with its
  percentage, elapsed turn time, `[N queued]`, ambient agent tag — refreshed
  while a turn runs. Measured on a real run: `example-model-v1 · ■■□□□□□□ 32% · 0s`.

- **Why this card jumped the plan's order.** T3-S3 built one completion source,
  T3-S4 pinned history sharing, T3-S5 built the row composer and the queue seam
  — and **none of it was reachable**. Deferring the wiring once was good scoping;
  deferring it twice would have started accumulating code whose only consumer is
  its own test. This card turns three cards of machinery into something a user
  sees, and it is the first user-visible change in four.

- **The design's good idea was not in my brief: three fields, three cadences.**
  I asked for "live `StatusFields` the guard can re-read". The implementer
  separated them by *how they change*:
  - **model and usage are pushed** at turn boundaries, by the code that owns the
    message history;
  - **the turn's start instant** is set and cleared at those boundaries, and is
    `None` between turns — which is why the idle row needs no cadence at all;
  - **queue depth and agent counts are pulled** through a closure on every
    paint, *not* stored, because the main loop drains the queue **between**
    turns, so a captured depth is stale at exactly the moment the idle row is
    painted. A closure can only report what is true at paint time.

  That last distinction is the kind of thing a test would not have caught for
  weeks: the stale value would have appeared only on the idle row, only after a
  mid-turn submission, and only until the next keystroke.

- **Two details that keep the cost honest.** An unchanged row emits **zero
  bytes** — without that dedupe a 50 ms tick would write twenty identical rows a
  second — which is the same rule the ambient counter already followed. And the
  meter reads the **same** session usage and effective budget `/context` renders,
  so the row and the command cannot disagree: the one-source principle applied
  **before** a second reader existed rather than after they drifted.

- **The cadence is the existing 50 ms input poll tick**, verified by reading that
  it genuinely runs for the whole of every inline turn, beside the call that
  already keeps the ambient counter inside A8's budget. No new thread, no new
  timer, no dependency. `read_line_inline` was **not** given a poll timeout: its
  timeout-free `read()` is the `SIGWINCH` delivery mechanism, and the handler
  carries a comment forbidding `SA_RESTART` for exactly that reason.

- **Four placeholder pins, not two — my inventory was wrong, and the way it was
  wrong is a rule I have written down.** I briefed "two existing tests pin the
  placeholder" after reading the **first few lines of a grep**. There were four.
  The test author found the other two, left them alone because the brief said
  two, and **stopped to tell me** — correct on both counts. I then authorised all
  four.

  The rule this project already records is *never assert absence from a
  truncated search*; I asserted a **count** from a truncated one, which is the
  same error wearing a different hat. The lesson is not "grep more carefully" —
  it is that any claim about a set needs the whole set in front of you.

- **The vacuity trap, caught before it bit.** One pin asserted the row was
  **not** `"kitsu"` after teardown, proving the row gets cleared. After wiring
  that would still pass — a real status row is also not `"kitsu"` — so it would
  have held even if teardown left frozen debris on screen. The author
  **measured** the exit screen rather than guessing (row 24 is genuinely empty,
  because the closing summary is wider than `COLS`, wraps, and scrolls the
  screen up one), pinned `== ""`, and added a mid-run assertion that a status row
  was there to clear — so an inline mode that never painted one cannot satisfy
  it. That is the fifth member of this project's vacuous-assertion family and the
  **first spotted in advance**.

- **F51, found by measurement and traced by me.** The author observed that an
  inline streamed answer arrives in a **single burst after the turn ends** and
  labelled it an open question rather than guessing. It is F47's mechanism:
  fragment printing is suppressed whenever tools were offered, so tool-call JSON
  cannot leak into the transcript. The comment there says the final round
  withholds tools "so final answers still stream live" — true of a round
  *following* a tool call, but **an ordinary single-round answer with tools
  enabled, the default, has tools offered in that very round, so nothing
  streams.** The product streams least in the common case. Pre-existing,
  identical in classic mode, deliberate at the line level, and a **product
  decision rather than a bug** — recorded with three options and fixed nowhere.

- **Acceptance commands/results:**
  ```
  cargo test -p kitsu --test tui, both profiles   22 passed each  (19 + 3)
  the status-row tests, 10 runs per profile        40/40 green, ±0.03s wall
  cargo test -p kitsu --bins / --release         821 passed each  (unchanged)
  cargo test -p kitsu --test cli                 245 passed       (unchanged)
  cargo test -p kitsu-tui / -p kitsu-render       60 / 124        (unchanged)
  cargo test -p kitsu-editor / tools --lib        39 / 413        (unchanged)
  cargo clippy --workspace --all-targets            0 warnings
  workspace total, all targets                   2128 passed, 0 failed
  ```
  Verified independently. The maintainer's `fs.rs` is at `54 20` (**F33**).

- **Documentation (standing requirement, in this commit):** the first
  user-visible change in four cards, so it earns the full set — `README.md` and
  `CHANGELOG.md` describe what a user now sees including the drop order,
  `docs/functionality.md` adds the refresh tick, the dedupe and the shared usage
  source, and **`ux-design.md`** gains the three-cadence design and the
  zero-bytes rule, as its own code-and-doc-together rule demands.

- **Commit:** see below.
- **Residual risk:** `InlineComposerHost` keeps its `#[allow(dead_code)]`, and
  the implementer **proved** why: all three construction sites are inside
  `#[cfg(test)]`, and the inline reader drives `TypeaheadEditor` and
  `idle_slash_hint` **directly**, never `kitsu_tui::composer`. Making the
  composer the inline reader is its own card; the status row neither helps nor
  hinders it. **F50** (A8 untested), **F51** (streaming), and **F49**'s two items
  are open. **Q1's terminal matrix still needs a human.**
- **Next:** T3-S6 (the bounded, fd-tagged cell ring, whose risk gate is the N4
  memory bound) — and the composer-as-inline-reader card, which is what finally
  removes the last dead-code marker in this milestone.
