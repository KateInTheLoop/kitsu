# E11 / R6-b — `/` search, and two assumptions the requirement had made for us

- **UTC start/end:** 2026-10-16T09:05Z / 2026-10-16T04:22Z
- **Branch/base:** `features/tui_herdr_improvements` / `587d9f1`
- **Epic/story/card:** E11 / **R6-b**. **R6 is complete except R6-d, which stays
  gated.** Records **F115**.
- **Requirement:** R6 (amended three times), **N2** (the 100 ms budget).

- **What shipped.** `/` opens a query line in the overlay's chrome row; `Enter`
  runs the search; `n`/`N` walk the matches, newer and older.

- **The cost measurement decided the design.** A full `plain()` pass over a
  10 000-line ring is **4.1 ms release / 13.7 ms debug**, and **9.8 / 45.4 ms**
  with one 1 MB pasted cell (**F52**'s input). Scanning `styled()` without the
  stripper is **195 µs**, so the cost is **essentially all allocation**. A
  20-character query typed at speed is ~0.9 s of scanning in debug; **three pasted
  characters alone would blow N2's 100 ms budget.** So **search runs on submit**,
  storing absolute ordinals, and `n`/`N` are `cursor ± 1` with no allocation.

  **Pinned behaviourally, not by timing** — a line appended after the submit must
  not join the walk — so there is no flaky timing test, and **nothing was added to
  `paste_latency`** because no latency claim is being made. The per-keystroke cost
  was removed instead of measured.

- **My F113 worry was wrong, in the useful direction.** `plain()` is **exactly the
  glyphs of the painted row**, so searching it matches what the user sees *better*
  than searching the styled bytes would, and `ascii_structural_markers` staying
  unapplied is **correct** — it is a `--plain` tier transform, and applying it
  would make a character the user can see fail to match itself. **The real
  divergence is width**: `fit_row_to_width` cuts at `cols`, so a match past the
  right edge is searched and off-screen, and **it still counts** — width-dependent
  matches would mean one query finding different things in two window sizes.
  Unexpected find, now documented: **`push` does not split on newlines**, so a
  line kitsu writes can hold an embedded `\n` in one cell where `push_prompt`
  splits.

- **The rejected first draft of the paste fix matters more than the fix.**
  `b"/find me\nq"` **closed the overlay** and searched nothing. The signal was
  already on the wire and being thrown away — the alternate-screen route writes
  `ESC[?2004h` and the loop **swallowed the paste markers whole**. Fixed on the
  markers, so a newline inside a paste is **content** (R6-a's rule). The author's
  own first draft — *discard the rest of the chunk after any newline* — was
  **rejected because it resolves the ambiguity from a chunk boundary**, which this
  loop's own header calls wrong (**F65**), and would eat a `q` typed fast enough to
  share a read with the `Enter`. Mutation D2 proved the two rules distinguishable.

- **The footer trade, measured both ways.** `Ctrl+Home/End` **lost its
  `oldest/newest` gloss**: with it the row is 91 columns and **at 80 the fit cuts
  `q/Esc close` clean off**; without it, 77 columns and the exit key survives with
  three to spare. **Losing the key that leaves the surface is worse than an
  unglossed key pair.** It does weaken T5-S1c-iii's rule that naming those keys
  without saying they move the *frame* advertises an ambiguity — mitigated, not
  ignored: inside the overlay the query line takes only characters and backspace,
  so **those keys have no competing caret meaning there**, and `/tui`'s details
  still carry the full explanation.

- **Five mutations, all restored, three of them bidirectional**, including one that
  proved a test which *passes today* still bites (`repeat_search` stubbed to
  `true`). The author also caught an **unsatisfiable assertion in their own work
  before hand-off** by doing the ordinals rather than assuming them — the eleventh
  for the epic's ledger and the second caught pre-hand-off.

- **Acceptance commands/results, both profiles:**
  ```
  cargo test -p kitsu-tui --lib, both    152 passed, 0 failed
  cargo test -p kitsu --bins, both       961 passed, 0 failed
  --test tui 75 · --test cli 245 · render 138 · editor 42 (+9) · tools 413
  paste_latency 4 · cargo clippy --workspace, both, -D warnings   0 warnings
  ```
  Footer: `PgUp/PgDn scroll  {/} prompt  / search  n/N match  Ctrl+Home/End
  q/Esc close` — and it still names **nothing it cannot answer**: `dump`,
  `editor`, `scrollback` and a literal `[` stay absent while R6-d is gated. No
  flake in either profile. The maintainer's `fs.rs` is at `54 20`.

- **Documentation (standing requirement, in this commit):** CHANGELOG,
  `functionality.md`, **README** (per F114's positive check — the `/tui` row's
  overlay clause gains search), **`ux-design.md`** (the searchable-versus-visible
  rule, which sits exactly on the `kitsu-render` boundary that file owns), R6
  marked **complete except R6-d**, and the plan's R6-b row.

- **F115 also records that markdown emphasis defeats my own sweep.** The docs read
  *"are `**not** wired yet"*, and F110's literal `not wired` pattern finds
  **zero** where a tolerant `not\*{0,2} wired` finds it — a sentence that **must**
  be found when R6-d ships. **Third false reading from my two mechanical checks**
  (compound row name, bracketed enumeration, now markdown). The ratio still
  justifies them, but: **they nominate, a human adjudicates, and a clean sweep is
  not evidence of a clean document.**

- **Commit:** see below.
- **Residual risk:** **R6-d remains gated** by the requirement until the dump's
  confirmation-or-opt-out is decided. **F72**'s unmarked-LF case is open and now
  reaches a second surface. **F52** has an adversarial input and this card
  measured its cost (a 1 MB cell more than doubles a full scan). The eleventh
  flake is uncharacterised. **F107-b** (four tests leak `FOREGROUND_RECAP`) is not
  done. **Q1's terminal matrix needs a human.**
- **Next:** **F72** (now two surfaces), **F107-b**, characterising the eleventh
  flake, or **T5-S4**…**T5-S8** (Windows fallback, `agent_wait`, the herdr
  manifest, decision records). **R6-d needs a human decision, not a card.**
