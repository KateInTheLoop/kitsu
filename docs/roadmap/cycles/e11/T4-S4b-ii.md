# E11 / T4-S4b-ii — the steer line, and the hazard of fixing a paste bug twice

- **UTC start/end:** 2026-10-15T04:05Z / 2026-10-15T08:23Z
- **Branch/base:** `features/tui_herdr_improvements` / `c709791`
- **Epic/story/card:** E11 / T4-S4b-ii. **Tests were written two cards earlier
  and deliberately left failing**, so this card needed no test-writing step.
- **Requirement:** **A3**, amended with the sub-mode as shipped. Records **F69**.

- **What shipped.** `s` opens a real editable line with a caret over the
  existing `kitsu-editor`: `Enter` sends, `Esc` cancels and sends nothing, an
  empty correction is refused, and a pasted multi-line correction is **content**
  that waits for the user's own `Enter`.

- **The hazard I flagged, and the answer that is better than the one I
  suggested.** F68's decoder **strips** bracketed-paste delimiters and yields
  plain text. Feeding that text through the editor would let an **unbracketed**
  newline reach the submit path — F68 re-entering one layer down by a different
  route, and it would have passed the prompt-level test while the paste path
  stayed wrong.

  I suggested re-bracketing or an insertion path. **The implementer chose
  better:** while the line is open it **bypasses the decoder entirely** and
  feeds the raw chunk to the editor, so the editor sees the terminal's real
  `ESC[200~`/`ESC[201~` and enforces *a paste never submits* as the one parser
  that owns the rule. Verified: no `OverlayInput::Paste` → `SteerLine` edge
  exists anywhere.

  **The argument I had not anticipated, and it is the decisive one:**
  `OverlayKey::Char` is **ASCII-only by construction** — its own doc comment
  says a half-decoded UTF-8 character has no business there. So routing the line
  through the decoder would have discarded exactly the split-UTF-8 handling that
  justified reusing the editor in the first place. Re-bracketing would also have
  required re-encoding ordinary keys back into bytes: **a third representation of
  the same input**.

- **A sharp detail that would have been a bug.** Opening the line **mid-chunk**
  drops the remainder of that read, with a comment saying why: those bytes were
  decoded as keys *before the line existed*, and letting them fall through to the
  row list could have run `x`. That is F68's lesson applied to a boundary F68
  itself did not cover.

- **F69, flagged by the implementer rather than hidden.** The registry-level
  tests drive the loop through `OverlayUnderTest`, a model that mirrors the
  loop's arms **and whose comment says so**. This card changed the real `s`;
  the model still routes `s` to the naive prompt it replaced. So **the model's
  paste test now describes the model, not the product.**

  **Not a reject:** the behaviour is still pinned on the real path by a stronger
  route (the editor's own test), so coverage moved rather than vanished and
  nothing was accepted on a test that passes while the product is wrong. But a
  mirror that no longer reflects **cannot fail when the two disagree** —
  agreement is not among the things it checks. That is **F57**'s hole with the
  pieces swapped. A nearby symptom from the same cause:
  `OverlayTextPrompt::Steer` is now constructed **only by the model**, so it
  carries a test-only `allow(dead_code)` — a production variant kept alive by a
  test is a smell, not a fix. Both are one follow-up card, now on the plan.

- **Acceptance commands/results:**
  ```
  cargo test -p kitsu-editor, both profiles    42 passed  (41 + 1)
  cargo test -p kitsu-tui, both profiles       86 passed  (84 + 2)
  cargo test -p kitsu --bins, both profiles   857 passed  (856 + 1), 0 failed
  cargo test -p kitsu --test tui, both         60 passed  (unchanged)
  cargo test -p kitsu --test cli              245 passed  (unchanged)
  cargo test -p kitsu --test paste_latency       4 passed
  cargo test -p kitsu-render --lib            130 passed
  cargo test -p kitsu-tools --lib             413 passed
  cargo clippy --workspace --all-targets        0 warnings
  ```
  **The whole workspace is green for the first time since T4-S4b's tests were
  written.** The `allow(dead_code)` on `SteerLine` is gone. The maintainer's
  `fs.rs` is at `54 20` (**F33**).

- **Documentation (standing requirement, in this commit):** **A3** carries the
  sub-mode as shipped — the editor reuse and its F45 reasoning, `ESC` under
  F65's grace with the "spent only when held" detail, **the decoder bypass with
  both reasons**, the mid-chunk drop, the empty refusal, and the echo's
  control-character flattening and caret windowing. **F69** recorded with its
  F57 lineage, and its follow-up is a plan row. `CHANGELOG.md` and
  `docs/functionality.md` describe what a user does.

- **Commit:** see below.
- **Residual risk:** **F69's model divergence** is the live one — the next person
  to change `s` will read `OverlayUnderTest` as authoritative. **F67's open
  question** (whether `/jobs prune` should print a result before discarding it)
  is unmade. **F62**, **F60**, F39's test-side fix (**seven** occurrences),
  **F57**, **F53**, **F54**, **F50**, **F51**, **F52** are open. **Q1's terminal
  matrix still needs a human.**
- **Next:** **T4-S5** — the filter line (`/`), whose grammar T4-S1 already
  settled and pinned; it is the last key A3 lists that has no binding. F69's
  follow-up should go first or alongside, since T4-S5 adds another prompt arm to
  the loop the model is supposed to mirror.
