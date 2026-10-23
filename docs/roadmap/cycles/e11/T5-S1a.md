# E11 / T5-S1a — the fullscreen view, and a row whose test already existed

- **UTC start/end:** 2026-10-15T17:00Z / 2026-10-15T17:58Z
- **Branch/base:** `features/tui_herdr_improvements` / `40fd6ed`
- **Epic/story/card:** E11 / **T5-S1a**, the first of T5-S1's three. Records
  **F85**, **F86**.
- **Requirement:** **N4**, amended; **N5**'s "vacuous today" note narrowed.

- **What shipped.** `crates/kitsu-tui/src/fullscreen.rs`: a **pure** view over
  the cell ring, with auto-follow and the paging keys as **one** state machine.
  It holds no terminal and writes nothing. **No user-visible behaviour ships
  here** — the view has no caller yet — so `CHANGELOG.md` and
  `docs/functionality.md` correctly say nothing, rather than announcing something
  a user cannot reach.

- **T5-S1 split three ways, and two of the three findings are corrections to
  me.**

- **F85 — the row's named acceptance test already existed.** I challenged
  *"failing 5 000-line **RSS** test"* as flaky and as asserting a property
  **F52** disclaims. Both true, and the test author found the better objection:
  **`cells.rs:177` has had a 5 000-push bound test since T3-S6.** Verified. So
  the row's criterion was satisfied **two milestones before the card ran**, and
  following it literally would have produced a **duplicate** — in the one form
  that is both flaky and *false*, since one pathological line defeats a byte
  bound by design.

  **A plan row's acceptance can be satisfied before its card runs, and the row
  will not say so.** The row is written once; the suite moves every card. I
  questioned the test's *shape* and never asked whether it existed.

  **And I overcorrected on bytes.** I said bytes were off the table while F52 is
  open. A byte claim *is* available, just not from the allocator: **the retained
  lines' own byte lengths sum identically after 10 000 pushes and after 5 000**,
  computed from contents — deterministic, allocator- and parallelism-independent,
  and pinning exactly what RSS reached for. Only "retained bytes ≤ K" stays
  untestable. N4 now records both.

- **F86 — the feed lands after the reader, and N4 already said so.** I suggested
  "the ring becomes live" might be the natural *first* piece because it is the
  bigger change. N4 forbids it: the ring is *"not fed until something reads it —
  feeding an unread ring would spend up to ten thousand styled lines of memory
  for no benefit."* Shipping the feed first would spend that memory in the window
  before a reader existed. **Bigger, but second.**

  **And the alternate screen needs no code at all.** `TuiRuntime::enter`/`leave`
  already do the lifecycle and panic/signal restore already cover N5; what is
  missing is a **caller**, which only the mode switch can be. So **N5's note was
  wrong** — it said the clauses become real when "T5's fullscreen renderer uses
  that runtime", but the renderer is pure and never touches a terminal. Narrowed.

- **F84 applied before the bug, not after it.** One card after *enumerating the
  routes out of a state is not enumerating the transitions out of it*, the test
  author wrote an **eleven-row transition table against four keys**, with **three
  transitions no key triggers**: the **re-latch** (paging down onto the newest
  line resumes following, or the view silently stops one window short of the
  bottom), **append while following**, and **eviction past the anchor**.

  **That third one changed the design:** `Anchor::Pinned` stores an **absolute**
  line ordinal, not an index into the retained lines, because **an index would
  drift under eviction while the anchor looked unchanged** — state moving
  underneath something that believes it still points at the same place.
  Verified: `Pinned { top: usize }`, and eviction **clamps without releasing**
  (`top.clamp(oldest, follow_top)`), so a pinned view whose lines are gone shows
  the oldest that remain and **does not start chasing the newest**.

- **Acceptance commands/results:**
  ```
  cargo test -p kitsu-tui, both profiles      113 passed, 0 failed  (99 + 14)
  cargo test -p kitsu --bins                  922 passed
  cargo test -p kitsu-render --lib            138 passed
  cargo test -p kitsu-editor                   42 passed (+9)
  cargo test -p kitsu --test tui, threads=1    61 passed
  cargo test -p kitsu --test cli              245 passed
  cargo test -p kitsu --test paste_latency       4 passed
  cargo test -p kitsu-tools --lib             413 passed
  cargo clippy --workspace --all-targets -- -D warnings   exit 0
  cargo clippy --workspace -- -D warnings                 exit 0
  ```
  Verified: `kitsu-render` gained **nothing** (empty diff), so "classic pays
  nothing" remains a **compile-time** property; the ring is **still not live**
  (`with_cell_ring`/`cells` keep their `allow(dead_code)`, `install`'s signature
  untouched); the four paging keys are **not** in `keys::BINDINGS`, which is the
  *agent view's* table guarded by a sweep over *that* decoder. Suites one at a
  time (**F80**); no flake. The maintainer's `fs.rs` is at `54 20` (**F33**).

- **A process note I should record against myself.** The test author prototyped
  the whole implementation, got 14/14 on the first run, reverted, and **left the
  prototype on disk** — and I authorised the implementer to read it. The
  implementer disclosed that it shipped that logic "essentially as written",
  having traced the eleven rows and the eviction arithmetic by hand first, and
  said it owns the result.

  That is honest and the gates hold, but **the independence the two-agent split
  exists for was partly spent**: the second agent was closer to a reviewer than
  an author. This is the **second** card where a left-behind prototype did that
  (T4-S6c was the first). Worth watching: the split's value is two independent
  readings of the same requirement, and a prototype hands the second reader an
  answer before it forms its own.

- **Commit:** see below.
- **Residual risk:** **T5-S1b** (the ring goes live) is next and will touch
  `install`'s signature, which has **dozens** of call sites in `tui_bridge.rs`'s
  own test module. The four paging keys have **no table and no sweep** — T5-S2
  owes either a second table or a recorded reason they live outside the first.
  **T4-S9** still needs the ring fed. Four open flakes. **F72**, the ordinal
  chord, the `r`-decision paste arm, the mid-turn counter freeze, **F67**,
  **F62**, **F60**, **F57**, **F53**, **F54**, **F51**, **F52** are open.
  **Q1's terminal matrix still needs a human.**
- **Next:** **T5-S1b**, the feed — small, and its order is now fixed by N4.
