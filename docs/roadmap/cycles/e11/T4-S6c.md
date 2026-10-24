# E11 / T4-S6c — the recap buffer, and the transition with no route

- **UTC start/end:** 2026-10-15T16:00Z / 2026-10-15T17:39Z
- **Branch/base:** `features/tui_herdr_improvements` / `68a9ab8`
- **Epic/story/card:** E11 / T4-S6c, the last of T4-S6's three. **A5 is
  complete.** Records **F84**, **F84b**.
- **Requirement:** **A5** / **§4.5**, amended on four points.

- **What shipped.** While attached, the foreground turn's output is held in a
  **byte-bounded 64 KiB** buffer in `kitsu-render/src/surface.rs` and replayed
  on return under `⛩ detached from agent N — recap:`. Overflow is **counted and
  noted at the top**. A detach with nothing buffered paints **nothing**.

- **The card was not blocked after all, and the reason is better than mine.** The
  split that created it feared a deferral past R6 if the implementer "reached for
  the cell ring". I argued from **F52** — the ring is line-bounded and §4.5 asks
  for bytes. The test author found the **structural** argument and I verified it:
  the buffer decorates `Surface`, which lives in `kitsu-render`; `CellRing`
  lives in `kitsu-tui`; and **`kitsu-render` does not depend on `kitsu-tui`.**
  So the ring is **unreachable** from a `Surface` decorator however it is
  bounded — there was never anything to defer. **`cell_ring_lines` still has no
  reader and F52 stays open** for the overlay and the `[` dump.

- **F84 — the transition with no route, which my brief missed.** I wrote "the
  three detach routes must all replay" and cited **F79** (*callers are
  enumerable*). There is a **fourth** way an attachment ends and **no key
  triggers it**: `apply_agent_view_exit` **re-binds to a different agent**, and
  the first attachment's buffer has nowhere to go. The author's first prototype
  **dropped it silently**, and it surfaced only as **cross-test interference**
  through a process-global slot — an unrelated existing test left a stale buffer
  behind. **Luck that does not repeat.**

  Now pinned with **order as the assertion**: the previous agent's recap under
  **its own** header, **before** the new agent's attach milestone, because the
  other order **attributes one agent's turn to another**. Verified: the replay
  sits at `main.rs:8452`, before `*attached = Some(id)` at `:8454`.

  **The lesson is narrower than F79's.** F79 is about **callers**; this is about
  **states**. I enumerated the ways a user *asks* to leave an attachment and
  missed the way an attachment *ends without being left*. **Enumerating the
  routes out of a state is not enumerating the transitions out of it** — a route
  is something a user does, a transition is anything that changes the state,
  including being replaced. A brief organised around user actions cannot see the
  difference.

- **F84b — a byte bound made an old caution real, and it caught its own
  prototype.** F52's *second* caution had never mattered: truncating styled bytes
  can **sever an SGR sequence**. A byte bound makes it live, so a clamped line is
  cut on a `char` boundary and **ends with a reset** — `fit_row`'s existing
  rule. **The test failed against the author's first prototype**, which cut on a
  boundary without resetting; without it a severed escape would have coloured the
  rest of the session.

- **Overflow is marked at two levels, and the painted one is the point.** The
  note is the recap's **first** entry and counts the lines the bound cost;
  eviction is oldest-first so the gap sits where the note is. **F71** is why both
  levels exist: *a recap that computed the note and replayed only the survivors
  would pass every unit test and still show the user an unmarked hole.*

- **A detach with nothing buffered paints nothing**, argued from §4.5 itself: the
  milestone is what the recap is *"replayed with"* — its header — and §4.5
  separately settles that **the two keys are silent and the command answers**, so
  a bare milestone would break the silence rule. Verified the code returns before
  the header, so the naive shape does not exist.

- **No backpressure, by construction.** A5 says attaching never pauses the
  foreground. The author enumerated the three ways backpressure could exist and
  closed each: the bound is held by **evicting**, never refusing; `transcript_line`
  returns `()` so there is no channel to signal "wait"; and the displaced surface
  is **never** called from any `Surface` method. The eviction loop has a
  `None => break` arm so it terminates even against a line at the bound.

- **Acceptance commands/results:**
  ```
  cargo test -p kitsu-render --lib, both profiles   138 passed, 0 failed  (130 + 8)
  cargo test -p kitsu --bins, both profiles         922 passed, 0 failed  (917 + 5)
  cargo test -p kitsu --test tui, both               61 passed
  cargo test -p kitsu --test cli                    245 passed
  cargo test -p kitsu-tui / -editor                  99 / 42 (+9)
  cargo test -p kitsu-tools --lib                   413 passed
  cargo test -p kitsu --test paste_latency            4 passed
  cargo clippy --workspace --all-targets -- -D warnings   exit 0
  cargo clippy --workspace -- -D warnings                 exit 0
  ```
  The author **prototyped the whole feature to 922/922 and 138/138 before handing
  over**, reverted it, and **left the prototype on disk for the implementer** —
  the ninth card to use that method. Suites run one at a time (**F80**); no
  flake. The maintainer's `fs.rs` is at `54 20` (**F33**).

- **Documentation (standing requirement, in this commit):** **§4.5** records the
  milestone-as-header-and-suppressed rule with its three reasons, the byte bound
  **and both reasons the ring is wrong**, overflow being marked rather than
  counted, the clamped-line reset, **F84's fourth transition** with order as the
  assertion, and the no-backpressure construction. **F84** and **F84b**
  recorded. **F52 is explicitly left open** so nobody reads this card as having
  closed it. `CHANGELOG.md` and `docs/functionality.md` describe what a user
  loses and keeps. `ux-design.md` needs **no** new glyph — the header reuses one
  already registered.

- **Commit:** see below.
- **Residual risk:** **T4-S9** (the transcript overlay) is the only T4 story
  left and genuinely needs R6's cell ring. The stale-blocked-read path from
  T4-S6b still has no test. Four open flakes. The ordinal chord, **F72**, the
  `r`-decision paste arm, the mid-turn counter freeze, **F67**, **F62**,
  **F60**, **F57**, **F53**, **F54**, **F51**, **F52** are open. **Q1's terminal
  matrix still needs a human.**
- **Next:** T4 is complete but for **T4-S9**, which R6 gates. **T5-S1** (the
  fullscreen renderer over the cell ring) is the natural next milestone and is
  the epic's biggest remaining unknown.
