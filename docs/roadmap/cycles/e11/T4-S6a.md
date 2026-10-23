# E11 / T4-S6a — attachment, rejected for having no reader

- **UTC start/end:** 2026-10-15T08:20Z / 2026-10-15T12:35Z
- **Branch/base:** `features/tui_herdr_improvements` / `c6a16d5`
- **Epic/story/card:** E11 / T4-S6a, split from T4-S6. **Rejected once; this is
  the accepted rework.**
- **Requirement:** **A5**, amended. Records **F75**, **F76**; and **F73**,
  **F74** came out of writing it.

- **What shipped.** `Enter`/`→` on the selected row attaches and closes the
  view; the composer prompt names the agent; `←` on an empty composer,
  `Ctrl+X d` and the new `/detach` return to the session. Attaching a terminal
  agent **takes custody of its result** (A7), through the **one** acknowledging
  call site T4-S3 left. Detaching stops nothing. **Leaving the view with
  `q`/`Esc` does not detach** — a distinction the rework added unasked, and
  right.

- **T4-S6 split into three, and one proposed seam was rejected.** (a) is this
  card; (b) the routed composer needs a live correction channel — the fixture
  problem that split T4-S4b; (c) the 64 KiB buffer and recap is a disjoint
  surface. **The `⛩` milestones were declined as a card**: over an existing
  `milestone_line` helper they would pin two format strings and prove nothing,
  and **F71** requires a visibility claim to go through the surface that paints
  it — so they were distributed into (b) and (c). **Declining to write a test
  that would pass by construction, for the fourth time in this epic.**

- **I rejected the first attempt (F75), and the gap was mine as much as the
  agents'.** `OverlayState::attaching` was **written** by the overlay and
  **never read in production**; the composer hardcoded `attached: None`. All
  twelve tests passed, because they drove the reader with `Some(3)` and the
  overlay's state directly: **each half was tested and nothing tested that they
  were connected.**

  **Why a rejection and not a follow-up:** before the card, `Enter`/`→` were
  not decoded at all, so they did nothing. After it, `Enter` **closed the view**
  and still did nothing — *worse*, because closing the view implies success. **A
  key that silently does nothing is a missing feature; a key that appears to work
  is a defect.**

  **How it got through:** the test author designed the gap in, flagged it, and
  justified it as "inventing a carrier for session state would be a second source
  of truth". **I read that and briefed the implementer without challenging it.**
  The reason does not hold: attachment must **outlive** the overlay, so the
  composer loop is not a second home for it but **the only** one. A necessary
  carrier had acquired a principled-sounding objection, and I was the one
  positioned to catch it.

  Fifth occurrence in this epic of one pattern — a registered command nobody
  dispatched (**F57**), a mirror that stopped reflecting (**F69**), a legality
  rule no keystroke could satisfy (**F70**), a message only the test could see
  (**F71**), and **a state nobody read**.

- **The rework's demonstration is the reusable part.** I asked for one test that
  fails when the halves are disconnected; the agent produced **two** and said
  which was weaker. Against a reconstructed pre-wiring tree it got **six compile
  errors** — and it labelled that the weaker proof itself, because a test that
  fails to compile has not shown it *notices* the defect. So it also no-op'd the
  carrier's body, recreating F75 exactly, and got a runtime failure on the
  load-bearing assertion (`left: None, right: Some(1)`).

  **When a test exists to catch a wiring gap, re-create the gap rather than
  deleting the wiring** — deletion proves the test compiles against the wiring,
  not that it detects its absence. Same method as the F69 follow-up's
  break-it-on-purpose gate, applied without being asked.

- **Decisions carried, each reviewed.** `←` detaches on the **first** press
  while attached, asked **before** the press counter (so `←←` in one read is one
  detach and no view), **not** gated by `[tui] left_arrow_opens_agents`, and a
  non-empty draft keeps `←` as cursor movement. The legibility argument is what
  distinguishes this from the armed state T4-S2 refused: **attachment is on
  screen in front of the caret**, so the context is legible at the moment of the
  press. `Ctrl+X d` is inert and silent with nothing attached while `/detach` is
  **answered** — a chord is a slip, a submitted line is an intention (T2-S5b) —
  and `/detach` is registered **and dispatched** (**F57**).

- **Three things kept out, each for a reason rather than a schedule.**
  **`Ctrl+C`**: byte `0x03` untouched, because **F54** leaves its signal path
  unexplained; A5's clause now says so instead of promising it. **The status
  segment**: needs a `StatusFields` field, and that struct has **12 construction
  sites, none using `..Default::default()`** (**F74**) — its own card, test
  first. **`Ctrl+X 1..9`**: the digit cannot be an id (ids pass 9, and go
  **wrong** once `#1` is pruned and `#11` exists), so it must be an **ordinal
  over A2's display order**, needing the registry where there is none.

- **Acceptance commands/results:**
  ```
  cargo test -p kitsu --bins, both profiles   883 passed, 0 failed  (871 + 12)
  cargo test -p kitsu --test tui, both         61 passed
  cargo test -p kitsu-tui / -editor            87 / 42 (+9 integration)
  cargo test -p kitsu --test cli              245 passed
  cargo test -p kitsu --test paste_latency       4 passed
  cargo test -p kitsu-render --lib            130 passed
  cargo test -p kitsu-tools --lib             413 passed
  cargo clippy --workspace --all-targets        0 warnings
  cargo clippy --workspace                      0 warnings
  ```
  No flake of any kind; the unidentified `--test tui` flake has now gone **ten**
  runs without reproducing, still unnamed. The maintainer's `fs.rs` is at
  `54 20` (**F33**).

- **Documentation (standing requirement, in this commit):** **A5** carries the
  three-way split, the `←` rule with its legibility reasoning, the
  chord-versus-command division, and the `Ctrl+X 1..9` constraint; **and §4.5's
  `Ctrl+C` clause now says the detach half is deliberately not shipped pending
  F54**, so the requirement stops describing a key the product does not bind.
  **README's command table gained a `/detach` row** — the table the standing
  requirement warns drifts silently. `CHANGELOG.md` and
  `docs/functionality.md` describe attachment as a user meets it. **F75** and
  **F76** recorded; the plan row became **six** honest rows.

- **Commit:** see below.
- **Residual risk:** **F76** — `Ctrl+X d` discards a non-empty draft, because
  `InlineRead::Detached` carries none and its comment was written for `←`'s
  empty-composer precondition. Carded. **T4-S6b/c**, the status segment and the
  ordinal chord are carded. **F72**, the **unidentified PTY flake**, the
  `r`-decision paste arm, **F67**'s open question, **F62**, **F60**, **F57**,
  **F53**, **F54**, **F50**, **F51**, **F52** are open. **Q1's terminal matrix
  still needs a human.**
- **Next:** **T4-S6b** is sequential on this card and needs the live-thread
  fixture; **F76** or the status segment are smaller and unblocked.
