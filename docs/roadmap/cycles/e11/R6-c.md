# E11 / R6-c — `{`/`}` jump between prompts

- **UTC start/end:** 2026-10-16T05:40Z / 2026-10-16T01:47Z
- **Branch/base:** `features/tui_herdr_improvements` / `e6c2255`
- **Epic/story/card:** E11 / **R6-c**, third of four. Records **F110**, **F111**.
- **Requirement:** R6 (amended twice), §4.7.

- **What shipped.** `{` and `}` jump to the previous and next prompt in the
  `Ctrl+O` overlay. Unblocked by the previous card, which gave the ring something
  to jump between.

- **The seam, built once for two cards.** `pin_to(ordinal)` is "put the frame on
  this absolute line", clamped — deliberately **not** prompt-aware, because
  **R6-b's `/` needs the same thing** and will call it with a match ordinal.
  `prompt_blocks()` lives beside the marks; `jump` is the policy composed from the
  two. One anchor, one clamp, no second anchor concept.

- **Coalescing by adjacency.** A prompt is split one cell per line (both frames
  are one `fit_row_to_width` per cell), so without coalescing `}` would step
  through the *lines* of one prompt. **A multi-line prompt is one stop** — pinned
  with an `assert_ne!` against the row-stepping frame **by value**, and the
  mutation that drops the coalescing reddens **10** tests across four modules.

- **The overrule, which generalises.** The author first let `}` pin onto a block
  already at the frame's top. **Overruled on their own principle**: they had
  already ruled that a jump with no target leaves the follow alone, *because
  pinning before discovering it has no target breaks the follow for nothing* — and
  a target that clamps onto the cursor **cannot move the frame either**. What makes
  it more than taste is the shape of the failure: the pinning answer is
  **invisible at the keypress and visible later**, as output that silently stopped
  arriving (**F75**'s second kind, delayed). The author then wrote the assertion
  that catches it — after a declined jump, an appended line must still appear.
  The accepted rule is **direction-symmetric by construction** and needs no
  special case.

- **Two details worth keeping.** `pin_to` clamps **through `top`** rather than
  rewriting the arithmetic, so the clamp has one home and the ordinal is clamped
  at call time (a raw pin would be re-clamped against a moving `follow_top` on
  every repaint). And `prompt_blocks` handles **partial eviction for free**: it
  emits `evicted() + index` for a marked cell whose predecessor is unmarked, so a
  block whose head was evicted is reported at its oldest surviving row with no
  special case.

- **Accepted limitation:** two submissions with **nothing printed between them**
  are one stop. Adjacency is the only rule a bool-per-cell affords; splitting them
  needs a block id. It has one home in the code (`prompt_blocks`) and one in the
  tests, and the docs point at both.

- **Acceptance commands/results, both profiles:**
  ```
  cargo test -p kitsu-tui --lib, both     139 passed, 0 failed
  cargo test -p kitsu --bins, both        957 passed, 0 failed
  --test tui 75 · --test cli 245 · render 138 · editor 42 (+9) · tools 413
  paste_latency 4 · cargo clippy --workspace, both, -D warnings   0 warnings
  ```
  The footer reads `PgUp/PgDn scroll  {/} prompt  Ctrl+Home/End oldest/newest
  q/Esc close` and still names **nothing it cannot answer** — `search`, `dump`,
  `editor`, `scrollback` and a literal `[` remain forbidden, since R6-b and R6-d
  are unbuilt. The maintainer's `fs.rs` is at `54 20`.

- **One flake, third sighting:** `the_recap_holds_only_what_the_foreground_
  produced_while_attached` failed once in my verification, passed in isolation,
  and passed 957/0 three times. Captured to its own file. **Third independent
  sighting in a third card, and neither the author nor the implementer saw it — I
  did.** Consistent with **F107** throughout.

- **Documentation (standing requirement, in this commit):** `functionality.md`
  and the `CHANGELOG` gain the keys, the one-stop rule, and the
  does-nothing-rather-than-break-the-follow rule; the plan's R6-c row is done and
  names the shared seam. **F110 fixed a stale CHANGELOG claim** — *"Paging keys
  are not wired yet"*, sitting immediately after the paragraph describing those
  keys working, the twin of a sentence I fixed in another file three cards ago.
  **F101's rule was wrong and is corrected: grep every live document, not the one
  you are editing.**

- **Commit:** see below.
- **Residual risk:** **F107's production fix is now the most valuable card left**
  — three process-globals, three flake families, and this one test has cost four
  verification runs. **R6-b** is next in R6 and inherits `pin_to`. **R6-d stays
  gated** until the dump's confirmation-or-opt-out is decided. **F52** has an
  adversarial input (user paste is unbounded). Ten flakes. **F72**, the ordinal
  chord, the `r`-decision paste arm, the mid-turn counter freeze, **F67**,
  **F62**, **F60**, **F57**, **F53**, **F54**, **F51** open. **Q1's terminal
  matrix needs a human.**
- **Next:** **F107's production fix** (recommended — its interest is compounding),
  or **R6-b** (search, which now has its seam).
