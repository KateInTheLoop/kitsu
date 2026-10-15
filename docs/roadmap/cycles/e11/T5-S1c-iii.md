# E11 / T5-S1c-iii — paging, and a test that was only true of its fixture

- **UTC start/end:** 2026-10-16T00:45Z / 2026-10-15T22:21Z
- **Branch/base:** `features/tui_herdr_improvements` / `024e3f9`
- **Epic/story/card:** E11 / **T5-S1c-iii**, the last of three. **Fullscreen is
  complete.** Records **F100** and its resolution.
- **Requirement:** §4.7's advertisement decision; R1's fullscreen surface.

- **What shipped.** `PgUp`/`PgDn` page the retained transcript and
  `Ctrl+Home`/`Ctrl+End` jump to the oldest retained line and back to following —
  moving the **frame**, not the caret, and **only** while fullscreen holds the
  screen. Paging pins the view, so new output stops dragging it to the bottom.

- **F100 — `Ctrl+Home` *is* `Home` today, which made one guard load-bearing.**
  Measured: the editor's `b'H'`/`b'F'` arms ignore CSI parameters where `b'C'`/`b'D'`
  right above them inspect them (`"1;5" | "1;3"` for word jumps). So `ESC[1;5H`
  and `ESC[H` are **the same key** to it, and `ESC[5~`/`ESC[6~` are **swallowed**.

  **Consequence:** an implementer recognising the four sequences
  *unconditionally* in the shared reader would pass every positive test on this
  card and **silently break `Ctrl+Home` for every inline user.** So *"these keys
  do nothing new in inline"* is **load-bearing, not decorative** — which is not
  how a no-regression guard usually reads. And **"PgUp does not disturb the
  draft" was green before the card**, because the editor already ignored it;
  **F99**'s practice caught that, and every red assertion pins *delivery* or the
  caret instead.

- **The ninth unsatisfiable assertion, and the two fixes I refused.** The anchor
  test wanted ROW_01 on screen after one `Page::Up`; the arithmetic gives
  ROW_16..ROW_38, and ROW_01 needs **1.6 windows** — `Oldest`'s job, with
  one-window-per-press pinned green elsewhere. **Satisfying it meant redefining
  the key and breaking two passing tests.** Reproduced independently.

  I rejected *assert the row the step reaches* (it **duplicates `page_step`**,
  already pinned in `kitsu-tui` — two homes for one arithmetic fact) and *shrink
  the ring so one press reaches the oldest* (**true by fixture**, and the test
  would then read as a specification that is false of the product). **A test that
  is only true for its fixture is worse than one that is wrong, because it looks
  like a specification.**

  **Authorised instead: assert the intent** — the frame left the newest line, the
  paged frame **differs from the entry frame by value** (no row named, no
  arithmetic duplicated), and **after a repaint the same frame is painted again**,
  which is what the test's name promises and nothing covered.

- **The non-vacuity proof is the best this epic has produced.** The implementer
  **mutated the production line** — reverted the repaint to the fresh-view form it
  had before the card — and the new assertion failed with exactly its own
  message, then passed again when restored. **So it demonstrably covers the
  one-line change it exists for**, a change every earlier test had passed over.

  **And the seam I warned might not exist already did:** `through_the_seam` writes
  to `active_surface()`, which while the segment lives **is its own tap** — so the
  ring grows and the repaint fires through **production's gate**, no test-only
  hook. That let it pin the **whole frame by value** rather than the top row I
  asked for. It also added, unasked, `!segment.is_following()` after the growth —
  pinning that **append-while-following is suppressed while pinned**, a negative
  the view's table describes and nothing had checked from the production side.

- **Three design choices recorded as right.** A **sink** rather than a reader
  outcome, with a measured reason: an outcome **ends the read**, and re-entry
  replays the draft as *bytes*, so paging-by-outcome would **jerk the caret to the
  end on every press** and hand each one a fresh editor, a fresh key filter and a
  lost reverse-search. **The gate being the sink's presence** rather than a flag —
  *a surface with nowhere to send a press has no business lifting one out of the
  stream.* And **matching the full byte string including parameters**, the only
  thing keeping `Ctrl+Home` separable from `Home`.

- **The advertisement, as decided and not relitigated.** No row in
  `keys::BINDINGS`, no second table: §4.7 keeps computed-label chords out because
  they never reach the swept dispatch, and these have a third reason — **they are
  live only while fullscreen holds the screen**, so a static row would advertise
  them in two renderers where they do nothing. `/tui`'s own `details` names all
  four **and says they move the frame, not the caret**, because naming the keys
  without that would advertise an ambiguity given plain `Home` still moves the
  caret.

- **Acceptance commands/results, both profiles:**
  ```
  cargo test -p kitsu --bins, both       937 passed, 0 failed
  cargo test -p kitsu --test tui, both    75 passed, 0 failed
  cargo test -p kitsu --test cli         245 passed
  cargo test -p kitsu-tui / -render      114 / 138
  cargo test -p kitsu-editor              42 passed (+9)
  cargo test -p kitsu-tools --lib        413 passed
  cargo test -p kitsu --test paste_latency  4 passed
  cargo clippy --workspace --all-targets -- -D warnings   0 warnings
  cargo clippy --workspace -- -D warnings                 0 warnings
  ```
  A partial sequence split across **five** reads is still one press (**F65**). No
  flake. The maintainer's `fs.rs` is at `54 20` (**F33**).

- **Documentation (standing requirement, in this commit):** the **CHANGELOG** and
  **`docs/functionality.md`** lose *"still missing: paging"* and gain what the
  keys do, that they move the frame rather than the caret, that they are inert in
  the other renderers, and that a paged sequence inside a paste is content.
  **F100** recorded with its resolution and the mutation proof.

- **Commit:** see below.
- **Residual risk:** **no test covers `Ctrl+Z` on this surface**, and
  attached-agent routing and the `agent N>` label are absent in fullscreen — both
  known gaps, neither a regression. **T4-S9** still needs the ring. Five open
  flakes. **F72**, the ordinal chord, the `r`-decision paste arm, the mid-turn
  counter freeze, **F67**, **F62**, **F60**, **F57**, **F53**, **F54**, **F51**,
  **F52** are open. **Q1's terminal matrix still needs a human.**
- **Next:** **T5-S3** (`[` dumps the ring to native scrollback, `v` to `$EDITOR`)
  is the next numbered story; **T4-S9**'s transcript overlay is now unblocked
  since the ring is fed and read.
