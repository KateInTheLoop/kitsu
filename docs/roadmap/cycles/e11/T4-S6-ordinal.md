# E11 / T4-S6-ordinal — attach by position, never by id

- **UTC start/end:** 2026-10-16T19:40Z / 2026-10-16T12:41Z
- **Branch/base:** `features/tui_herdr_improvements` / `775f3ce`
- **Epic/story/card:** E11 / **T4-S6-ordinal**, carded since T4 and the **last
  fully-buildable plan row**. Records **F137**. **Rejected once (F106 verbatim).**

- **What shipped.** `Ctrl+X 1`…`Ctrl+X 9` attach to the agent at that **displayed
  position** — counted over the agent view's own grouping and filter — from the
  composer, without opening the view.

- **The test-design trap, and the fixture that closes it.** If a fixture's ids equal
  their display positions, the test passes whether the code reads the ordinal or the
  id: **satisfied by the defect**, the shape that has cost this epic eleven vacuous
  or unsatisfiable assertions. The author built ids `7, 3, 11, 2, 5, 9`, a filter
  leaving four, and a display order of `#3, #7, #11, #5` — so **ordinal 1 selects
  id 3**, **id 2 exists but is filtered out**, and the test asserts *"no ordinal
  equals the id it selects"* **first**, before any behaviour.

- **The accessor, and why not the obvious one.** `nth_displayed(rows, ordinal)`
  rather than a public `display_order() -> Vec`: the vector would hand out **a
  second indexing convention**, leaving every call site to decide whether `1` means
  `[0]` and whether out-of-range clamps, wraps or does nothing. `checked_sub`
  answers `0` before the subtraction; `get` rather than an index makes past-the-end
  `None` instead of a clamp. **Built over the existing private `display_order`** —
  4 call sites, the same order `sync`/`move_by` walk — so **the ordinal can never
  disagree with the row the selection mark sits on**, proved by driving the real
  `AgentSelection` rather than a copy.

- **My brief said no display-order accessor existed. One did** — twenty-seventh
  wrong brief, and the correction improved the design.

- **The inert cases, each with a reason.** `Ctrl+X 0` is inert **and undrafted**:
  not an alias for the first row, and **not a prefix of `10`**, because multi-digit
  entry would make `Ctrl+X 1`'s meaning **wait for the next keystroke** — F75's
  invisible-at-the-keypress shape. Past the end is `None` and **must not clamp**:
  *acting on an agent the user did not count off the screen is worse than doing
  nothing*. Twelve agents get nine digits with **no wrap and no modulo**, both of
  which attach to the wrong agent in silence. **The cap is in the decoder, not the
  accessor** — my ruling: nine-wide is a property of the chord, not of display
  order.

- **Rejected once, and the reason is a repeat.** The implementation was green while
  the chord was **advertised nowhere a user could see** — three occurrences, all
  comments. **F106 verbatim**, the second time this epic. **And partly my error:** I
  said I would write the advertisement, but the string is **production code** and
  `Ctrl+O`'s shipped **with a test**. Corrected to test-then-string; the new test
  also asserts the **negative** over `wrap`, `ctrl+x 10`, `two digits`, `modulo`,
  since a user believing in either would address the wrong agent in silence.

- **Two things the author did unasked.** It **refined its own mutation claim
  downward** — a partial id-first mutation fails only **three** of four, because
  the twelve-agent fixture uses ids `100..` so none can equal an ordinal, which
  exposes a real gap in that test's discriminating power. And it made
  **re-attaching idempotent** by routing through `apply_agent_view_exit`, which acts
  only on a change of binding.

- **Acceptance, both profiles:**
  ```
  cargo test -p kitsu --bins                   979 passed, 0 failed
  cargo test -p kitsu-tui --lib                156 passed, 0 failed
  --workspace --exclude kitsu 1153 · --test cli 256 · --test tui 75
  paste_latency 4 · carrier_matrix 7 · build_provenance 2
  cargo clippy --workspace, both, -D warnings   0 warnings
  ```
  The maintainer's `fs.rs` is at `54 20`. No flakes.

- **Documentation (standing requirement, in this commit):** CHANGELOG;
  `functionality.md` beside the existing `Ctrl+X a` description; **README's
  `/jobs view` row**, which already named `Ctrl+X a` — both homes **derived from
  where the sibling chords are documented** rather than guessed (F114's positive
  check); the plan's row; and `/tui`'s `details` string with its guard test.

- **Commit:** see below.
- **Next:** the implementable plan rows are **done**. What remains is **R6-d**
  (gated on a human decision), **T5-S7** (blocked on herdr's manifest spec),
  **T5-S4** and **F72** (both need Q1's live terminal matrix), **T5-S5** (optional
  mouse wheel), **T5-S8** (ADR-style decision records — writable now), and the
  open flakes **F130**/**F133** plus the uncharacterised eleventh.
