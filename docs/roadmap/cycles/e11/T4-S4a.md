# E11 / T4-S4a — selection that follows the agent, and the arrow key that closed the view

- **UTC start/end:** 2026-10-15T02:10Z / 2026-10-15T06:38Z
- **Branch/base:** `features/tui_herdr_improvements` / `7da0ce8`
- **Epic/story/card:** E11 / T4-S4a (split out of T4-S4).
- **Requirement:** **A3**, amended with six decisions it did not state. Records
  **F65**, **F66**.

- **The card was split, and I accepted the reasoning.** T4-S4 bundled selection
  with five action keys. They are **sequential, not parallel** — every action
  key's test must name "the selected agent", so none could be written before
  selection existed as a type; their production surfaces are **disjoint**
  (selection is pure, rows in and an id out; the keys are registry mutations and
  a text sub-mode); and their **risk gates differ**, so reviewing them together
  means reviewing neither. `r`/`s`/`x` are **T4-S4b**. The two pieces that could
  not be deferred without the next agent guessing came along: **`rename`'s
  signature** (the only new registry surface) and **`p`'s outcome taxonomy**.

- **A real defect, found by a test rather than asserted from my brief (F65).**
  The overlay's byte loop matched `0x1b` as **close**, and an arrow key is
  `ESC [ B` — three bytes. So the first `↓` **closed the view and fed `[B` into
  the composer underneath.** The view had been shipped (T4-S2) and peeked in
  (T4-S3) without this surfacing, because every key either card used is a single
  byte.

  This is the **mirror image of T4-S2's trap**, where a lone `ESC` at the idle
  composer stays *pending* until the next byte, which is then consumed as part of
  the sequence. Same ambiguity, opposite failure: the editor waits where the
  overlay acted. The fix carries state **across reads** (a read can split a
  sequence) and resolves a lone `ESC` after a **50 ms grace** matching the
  editor collector's own tick — with the `poll` retrying on `EINTR`, so **a
  SIGWINCH during an arrow is not read as a close**. Non-key bytes are dropped,
  never forwarded.

- **Selection follows the agent, not the row position — a safety property.**
  Rows re-group constantly: an agent finishing moves from `working` to
  `done (unviewed)`, a *later* group in A2's order, so a position-keyed cursor
  would silently retarget the user's next `x` or `p` onto a **different agent**.
  A test pins a regroup that really reorders, so an index-keyed model lands on
  the wrong agent and fails. The index survives only as the fallback for a
  **vanished** agent, where the selection falls to the row that took its place.

- **`x`'s arming extends T4-S2's rule to the case T4-S2 did not have:** moving
  the selection **disarms**, because an `x` armed on one agent and firing on
  another after a `↓` is the one way this affordance stops an agent nobody asked
  to stop. A **repaint keeps** the arming — a repaint is not a keystroke, and an
  agent finishing under the cursor must not cancel a half-typed stop — but a
  repaint that **loses** the armed agent disarms. `Confirmed(id)` carries its
  target so the firing names it rather than re-reading the selection.

- **The custody property survived, which was the gate.** T4-S3 made peek the
  single call site that takes custody of a result. Selection **never sees a
  registry**; the selected renderer only chooses a row indent; and
  `overlay_rows(rows, cols)` is now **literally** `overlay_rows_selected(rows,
  cols, None)`, so there is one renderer and the old frame is byte-identical.
  Both acknowledges-nothing tests pass with their `p` positive controls.

  **No new glyph:** the mark is the `*` `kitsu_render` already uses for an
  active list row, at the same character count so the row stays fitted — not a
  second state-marker table beside `deck_marker` (the drift **F34** closed).

- **`rename` reaches the row through the snapshot.** `AgentThreadSnapshot.name`
  already existed and already round-tripped; it was **hardcoded `None`**. Now
  the name flows thread → snapshot → DTO → row, so an overlay keeping a private
  name map fails its test. Legality is trimmed, non-empty, ≤ `NAME_MAX_CHARS`,
  **no control characters, non-ASCII legal**, any phase; refusals are distinct
  and a refused rename leaves the previous name intact. Deliberately **not**
  `safe_inline_label`, which coerces empty to `"unknown"` and truncates — right
  for a model-produced label, and exactly the silent rejection this project
  refuses for a name a human typed.

- **F66 recorded, not changed:** `NAME_MAX_CHARS` is a **renderer** constant
  (`AgentRow::TASK_HEAD_CHARS`), deliberately and with its rationale stated
  where it is defined — a name stands in for the task head, so it inherits the
  head's budget. The consequence a later reader should see coming is that
  changing the row's head width silently changes **what names are legal**. Noted
  so a future width change is recognised as a product change to validation.

- **Acceptance commands/results:**
  ```
  cargo test -p kitsu-tui, both profiles      82 passed  (78 + 4)
  cargo test -p kitsu --bins, both profiles  845 passed  (841 + 4)
  cargo test -p kitsu --test tui, both        58 passed  (57 + 1)
  cargo test -p kitsu --test cli             245 passed  (unchanged)
  cargo test -p kitsu --test paste_latency     4 passed  (unchanged)
  cargo test -p kitsu-render --lib           130 passed  (unchanged)
  cargo test -p kitsu-editor / -tools --lib   41 / 413   (unchanged)
  cargo clippy --workspace --all-targets       0 warnings
  ```
  The new PTY test ran **10/10** green at `--test-threads=1`, no flake. Verified
  in both profiles, and re-verified after my help-row edit. The maintainer's
  `fs.rs` is at `54 20` (**F33**).

- **Documentation (standing requirement, in this commit):** **A3 amended** with
  all six decisions — selection keyed on the agent (with the safety reasoning),
  clamping rather than wrapping, the fall-to-old-index rule, `x`'s two presses
  and disarm-on-move, that moving acknowledges nothing, that **`r` is not a text
  prompt and `s` is**, and `n`'s legality rules. **F65** and **F66** recorded.
  `README.md`'s `/jobs view` row, `CHANGELOG.md` and `docs/functionality.md`
  describe the keys; the overlay's help gained `↑/↓` for the same reason T4-S3
  added `space` — a working key missing from the help is a defect a user meets.

- **Commit:** see below.
- **Residual risk:** `r`/`s`/`x` are **not wired** (T4-S4b), so `rename`,
  `NAME_MAX_CHARS` and the outcome enum carry `allow(dead_code)` with a
  "bound by T4-S4b" note — the existing in-repo pattern, and what keeps clippy
  at zero. **F65's lesson applies to every remaining key card:** a card adding a
  key should ask what else starts with the same byte. **F62**, **F60**, F39's
  test-side fix, **F57**, **F53**, **F54**, **F50**, **F51**, **F52** are open.
  **Q1's terminal matrix still needs a human.**
- **Next:** **T4-S4b** — `r` (a three-key decision through the one shared
  `parse_permission_answer`), `x` (public `cancel`/`stop`, both `async`, so the
  synchronous byte loop needs a runtime handle — `handle_active_jobs_command` is
  the precedent), `n` wired to the method this card settled, and `s`'s text
  sub-mode, which is the card's real content.
