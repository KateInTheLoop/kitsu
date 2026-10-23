# E11 / T4-S6-status — the attachment on the status row, and a command I said worked

- **UTC start/end:** 2026-10-15T09:10Z / 2026-10-15T13:09Z
- **Branch/base:** `features/tui_herdr_improvements` / `a335e1b`
- **Epic/story/card:** E11 / T4-S6-status, carded out of T4-S6a.
- **Requirement:** **A5** / **§4.5**, amended on five points. Records **F77**,
  **F78**; **corrects F74**.

- **What shipped.** The status row carries `attached · agent N` as its leftmost
  content segment, second to claim room after the ambient tag. It is **derived at
  paint time** — `StatusSource::fields()` pulls it through the same closure as
  the queue and blocked counts, with **no field on `StatusSource`** and no
  snapshot anywhere.

  That matters because of **F75**: a stored copy would need telling on detach,
  and **the detach path offers no publication point** — clearing the composer's
  binding *is* the whole of it. The main test asserts the row and the prompt
  label against **each other** after every transition and never tells the row
  anything, so a cache stays red.

- **Why this card jumped the plan order.** A5's risk mitigation named the status
  segment, so attachment had shipped with half its visibility — and the
  **`←`-detaches-on-first-press rule rests on attachment being legible at the
  moment of the press**, which until now the prompt label argued alone.

- **F77 — the worst review failure of this epic, and it is mine.** `/detach`'s
  arm is **unconditional**: it always answers `not attached to a background
  agent`. Its own comment explains why — *"Nothing in this card can attach, so
  there is one answer to give"* — which was **true when written**, before I
  rejected the first attempt for F75 and the rework made attachment work. **The
  arm was never revisited.**

  **And I had committed three documents saying it worked**: README's command
  table, the CHANGELOG entry, and `docs/functionality.md`. All three were false.
  Corrected in this commit.

  So the standing documentation requirement was satisfied **procedurally** —
  three files in the same commit — and **violated in substance**. *A doc update
  is only worth what its claims are worth*, and a reviewer writing docs from a
  brief rather than from the code is how a false claim acquires the authority of
  a committed file.

  It is also **F75 repeating inside the card that fixed F75**: I demanded the
  wiring, verified overlay → `AgentViewExit` → `apply_agent_view_exit` →
  binding → composer, and **never checked the third consumer of that binding.**
  Sixth occurrence in this epic of *the thing we checked was not the thing that
  matters*. The narrow lesson: **when a rework makes something possible that was
  previously impossible, every arm written while it was impossible is suspect** —
  that comment was a dated statement of an assumption, and it should have been a
  grep.

- **F74 corrected, and the way it was wrong matters.** I recorded "12
  construction sites, none using `..Default::default()`" and authorised adding a
  `Default` impl. In fact the struct **already derived `Default`**, there are
  **nine** literal constructions, **six already use functional update**, and
  adding a field broke **exactly two** places. **I produced that number with a
  grep and trusted its shape** — `grep -c` over a type name counts mentions in
  text, not constructions in code. *A count is evidence about text until someone
  reads the code.* The real tax was smaller and is now paid: three exhaustive
  sites converted, one left partly explicit because the values it spells out are
  what it documents.

- **F78 — measured, not reasoned.** The row's drop order is **not monotonic in
  width**: a clipped model segment **reappears at 44, 43 and 42 columns** once
  the row is busier, because each segment is offered what the higher-ranked ones
  left and dropping the thirteen-column queue tag frees room. Found because the
  test author wrote the intuitive prefix/no-revive assertion **first** and
  discovered it unsatisfiable — the **sixth** unsatisfiable assertion in this
  epic. The surviving test pins the claim order exactly
  (`painted == (cols >= tag + sep + label)` across 20..=200, derived from the
  tag's own text) and says nothing about the model. **No special case was added
  to make the row monotonic.**

- **Acceptance commands/results:**
  ```
  cargo test -p kitsu --bins, both profiles   886 passed  (883 + 3)
  cargo test -p kitsu-tui, both profiles       90 passed  (87 + 3)
  cargo test -p kitsu --test tui, both         61 passed
  cargo test -p kitsu --test cli, both        245 passed
  cargo test -p kitsu-editor                   42 passed (+9 integration)
  cargo test -p kitsu-render --lib            130 passed
  cargo test -p kitsu-tools --lib             413 passed
  cargo test -p kitsu --test paste_latency       4 passed
  cargo clippy --workspace --all-targets        0 warnings
  cargo clippy --workspace                      0 warnings
  ```
  The red state's `field 'attached' is never read` is gone. Every assertion was
  **proved reachable before handover** by a throwaway prototype the test author
  then reverted (**F61**). No flake; the unidentified `--test tui` flake has now
  gone **eleven** runs without reproducing, still unnamed. The maintainer's
  `fs.rs` is at `54 20` (**F33**).

- **Documentation (standing requirement, in this commit):** **§4.5** gained the
  segment's position and claim priority, the repaint and agent-finishing rulings
  with their reasons, the derive-don't-cache rule with F75's lesson, and F78's
  non-monotonicity warning. **`/detach` is now marked unwired in §4.5, README,
  CHANGELOG and `docs/functionality.md`** — the three false claims corrected and
  the requirement told the truth. **F74 corrected in place**, **F77** and
  **F78** recorded, F77 carded.

- **Commit:** see below.
- **Residual risk:** **F77** (`/detach` unwired) is carded and is a signature
  change across the dispatch sites. **F76** (`Ctrl+X d` discards a draft),
  **T4-S6b/c**, the ordinal chord, **F72**, the **unidentified PTY flake**, the
  `r`-decision paste arm, **F67**'s open question, **F62**, **F60**, **F57**,
  **F53**, **F54**, **F50**, **F51**, **F52** are open. **Q1's terminal matrix
  still needs a human.**
- **Next:** **F77** is the honest next card — it is a shipped command that does
  not work, and it is now documented as such in four places.
