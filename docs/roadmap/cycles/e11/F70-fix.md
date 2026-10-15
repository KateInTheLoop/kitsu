# E11 / F70 — a rule the keyboard could not reach, and two unsatisfiable assertions

- **UTC start/end:** 2026-10-15T07:30Z / 2026-10-15T11:23Z
- **Branch/base:** `features/tui_herdr_improvements` / `e7675e3`
- **Epic/story/card:** E11 / **F70's fix**. Records **F72**.
- **Requirement:** **A3** amended — `n` now *implements* a rule A3 always
  stated, and the paste clause gained an honest caveat.

- **What shipped.** `n` reads its line through `kitsu-editor` on the byte
  bypass beside `s` and `/`. A non-ASCII name round-trips, including with a
  multi-byte character **torn across two reads**, and reaches the painted row
  **through the snapshot**. `OverlayPrompt::Text` and `OverlayTextPrompt` are
  **deleted** — verified zero occurrences — so no decoded key or decoded paste
  can ever fill a name again. `r`'s `Decision` is a single-key ASCII read and
  correctly stays decoded.

- **The defect, and why it survived a card I accepted.** T4-S4a wrote into A3
  that non-ASCII is legal; the registry implemented exactly that and pinned it.
  But `n` filled its draft from `OverlayKey::Char`, which is **ASCII-only by
  construction**, so typing `café` renamed the agent to `caf` **silently**.
  **The rule and its enforcement agreed; nothing checked whether the only path a
  user has could reach it.** *A validation rule is only as wide as the narrowest
  input path that feeds it* — the third time this epic paid for that blind spot
  (**F57**, **F69**, now **F70**).

- **F71's trap was anticipated and avoided.** The rename line is an innermost
  frame, so a refusal painted under the row list would have been **off-screen
  while the line is open** — readable by a test collecting from state, invisible
  to the user. Refusals are painted **on the rename frame**, which also keeps the
  text they are about on screen, and `n` clears a stale report when it opens so
  the frame only shows a refusal of what was typed into it. The tests assert
  through `overlay_frame_rows`, the loop's own chain, which is the only way to
  assert "on screen" without writing an F69 mirror.

- **F72 — the finding, traced before I authorised anything.** An unbracketed
  paste makes these lines submit at the first newline, because F68's
  **unbracketed-burst fallback lives in the decoder** and all three lines bypass
  the decoder by design. **The fix for one hazard removed the mitigation for
  another.**

  **The safety half I verified myself**, since the shape resembled the defect
  that cost this epic a rejection: an open line's branch in
  `overlay_feed_chunk` ends with `return false`, so the **whole chunk** is
  consumed by the line and the decoder is never reached — a mid-chunk submit's
  tail **cannot** become keys. **F68 stays closed**; the residual gap is early
  submission, not privileged action. Carded, not fixed here: the honest fix
  changes a parser three surfaces depend on, and F68's rejection is the precedent
  for how that goes wrong.

- **Two unsatisfiable assertions, both adjudicated by me rather than built to.**
  This is the **third and fourth** in the epic, after **F58** and **F61**.

  1. The `TooLong` refusal row is **87 columns**; the harness paints at **80**
     and every frame row is fitted. The only implementation satisfying
     `row.trim() == expected` would emit a row **wider than the terminal** into
     a pinned viewport — exactly what fitting exists to prevent. **The
     implementer stopped and asked rather than do that.** I widened that one
     assertion's paint width to 120 via a new `feed_and_frame_at`, because the
     test's subject is **visibility on the right frame** and width-fitting is
     already pinned by `the_steer_line_fits_a_narrow_terminal`.
  2. The `Unprintable` case typed `"two\nlines"` **unbracketed**, which F72
     means the editor submits at the newline — so the control character the case
     is about could never reach `rename`. Delivered as a **bracketed** paste
     instead, which is what a real paste in this surface is.

  **Both were test defects, not product defects**, and in both the product was
  already correct. Noting the pattern: an implementer told "make the tests pass"
  and *not* allowed to edit them is how an unsatisfiable assertion surfaces as a
  question instead of as a quietly changed product.

- **Test changes, all authorised explicitly:** the §3 paste test re-expressed
  with a **bracketed** paste and renamed (it named a type now deleted), with
  every assertion byte-identical; `OverlayUnderTest::draft()`'s **body**
  re-pointed at the rename line, since "delete `Text`" and "touch no test"
  cannot both hold literally — signature and meaning unchanged; and my two
  adjudications above. No other test touched.

- **Acceptance commands/results:**
  ```
  cargo test -p kitsu --bins, both profiles   871 passed, 0 failed  (866 + 5)
  cargo test -p kitsu-tui                      87 passed
  cargo test -p kitsu-editor                   42 passed (+9 integration)
  cargo test -p kitsu --test tui, threads=1    60 passed
  cargo test -p kitsu --test cli              245 passed
  cargo test -p kitsu --test paste_latency       4 passed
  cargo test -p kitsu-render --lib            130 passed
  cargo test -p kitsu-tools --lib             413 passed
  cargo clippy --workspace --all-targets        0 warnings
  cargo fmt (four crates) --check              clean
  ```
  `OverlayTextPrompt` occurrences: **0**. No flake of any kind; the unidentified
  `--test tui` flake has now gone **nine** runs without reproducing — still not
  proof of absence, and its name was never captured. The maintainer's `fs.rs` is
  at `54 20` (**F33**).

- **Documentation (standing requirement, in this commit):** **A3** records that
  `n` now implements the rule it always stated, with F70's lesson and the
  innermost-frame contract; **and its paste clause gained the caveat it was
  missing** — the clause holds only where the terminal brackets pastes, which
  makes the requirement honest about **F72** instead of describing a nicer
  product than exists. **F72** recorded and carded. `CHANGELOG.md` and
  `docs/functionality.md` describe the fix as a user meets it.

- **Commit:** see below.
- **Residual risk:** **F72** is open and carded. The **unidentified PTY flake**
  is open and unnamed. The `r`-decision-read paste arm remains **unasserted**
  (F69 follow-up). **F67's open question** is unmade. **F62**, **F60**, **F57**,
  **F53**, **F54**, **F50**, **F51**, **F52** are open. **Q1's terminal matrix
  still needs a human.**
- **Next:** **T4-S6** onward — every key A3 lists is bound and now reachable.
