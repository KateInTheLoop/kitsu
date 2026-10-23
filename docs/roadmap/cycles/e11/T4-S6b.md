# E11 / T4-S6b — the routed composer, and a blocker that had already dissolved

- **UTC start/end:** 2026-10-15T15:00Z / 2026-10-15T17:06Z
- **Branch/base:** `features/tui_herdr_improvements` / `ab8259b`
- **Epic/story/card:** E11 / T4-S6b, split from T4-S6. Records **F83**.
- **Requirement:** **A5**, amended with the routing rule and the milestone.

- **What shipped.** While attached, a submitted line belongs to the agent: it
  **steers** it, or **answers** it with `y`/`a`/`n` while it is blocked. The
  **`⛩` attach milestone** is painted once per change of binding and **names
  the route**. Slash commands stay commands, so `/detach` always works.

- **The blocker had dissolved, and nobody checked (F83).** T4-S4b and T4-S6a both
  deferred on the grounds that a steerable agent needs helpers *unreachable from
  `main.rs`'s test module*. True — and the helpers were reachable all along
  **from `subagent.rs`**, where **a passing test already drove a steerable agent
  end to end**. T4-S7 had already proved the pattern.

  **A deferral records where someone was standing when they hit the wall, and is
  re-read later as a property of the wall.** The fix was to stand somewhere else.

- **The race was dissolved, not decided.** A5 never said whether routing reads
  the agent's state at open time or submit time, and **both can be wrong** —
  open-time can send a steer the registry must refuse; submit-time can hand a
  typed correction to a parser expecting `y`/`a`/`n`. The resolution was to
  **make being wrong cheap**: submit-time state, the registry as final
  adjudicator, and **every refusal sends nothing and hands the line back**
  (**F76**), with the reason painted where the eye is (**F71**). Worst case: one
  extra keystroke, words intact. In the code **"refused" and "keeps the line" are
  the same boolean**, so they cannot drift.

- **A case I did not raise**, found by the test author: **a slash command stays a
  command while attached** (and a blank line stays blank), or an attached session
  could never leave by `/detach`.

- **F82's question, answered better than I asked it.** I asked what enumerates a
  new user-visible route. **This route adds no key — it changes what `Enter`
  means — so the only thing that can enumerate it is the moment it begins.** That
  is why the milestone names the route rather than just the agent; a key table
  cannot advertise a keyless route, and bolting it on would have been the wrong
  fix for the right rule.

- **F72 checked, not assumed.** The inline composer does **not** share the gap:
  `KeyFilter` owns the bracketed-paste state machine, so a paste arrives as one
  draft and a pasted multi-line correction routes as one line. F72 is the
  overlay's four bypass lines only.

- **The risk no test could catch, and the review that paid for itself.** The call
  from the composer's `Submitted` arm cannot be driven without a real tty, so
  **no test would have caught its absence** — the **F75**/**F77**/**F79** shape.
  The test author said so and specified the arm; I read it. It is correct:
  matched by reference (the non-routed path must hand the outcome on), sitting at
  `main.rs:6421` **before** the `ChatReadline::Line` construction at `:6445`,
  in the loop that owns `carried`, with exactly one call site.

  **And reading it found something else: a stale comment three lines above.** From
  T4-S6a: *"the `⛩` milestone and the recap are **not** this card's … they land
  with T4-S6b and T4-S6c."* **This card is T4-S6b and it shipped the milestone**,
  so the comment became wrong at the moment of the commit that made it wrong.
  That is **F77** verbatim — `/detach`'s *"nothing in this card can attach"*,
  which cost three false documentation claims. **A comment that names a future
  card dates itself, and nothing fails when it expires.** Corrected, with the
  reason in the comment.

- **Acceptance commands/results:**
  ```
  cargo test -p kitsu --bins, both profiles   917 passed, 0 failed  (910 + 7)
  cargo test -p kitsu --test tui, both         61 passed
  cargo test -p kitsu --test cli              245 passed
  cargo test -p kitsu-tui / -editor            99 / 42 (+9)
  cargo test -p kitsu-render --lib            130 passed
  cargo test -p kitsu-tools --lib             413 passed
  cargo test -p kitsu --test paste_latency       4 passed
  cargo clippy --workspace --all-targets -- -D warnings   exit 0
  cargo clippy --workspace -- -D warnings                 exit 0
  ```
  Four `allow(dead_code)` markers removed. The test author **prototyped the
  implementation to 917/0 in both profiles before handing over**, then reverted
  byte-for-byte — the eighth card to use that method — and it caught one
  unsatisfiable pin (a racy receipt field). Suites run one at a time (**F80**);
  no flake. The maintainer's `fs.rs` is at `54 20` (**F33**).

- **Documentation (standing requirement, in this commit):** **A5** records the
  submit-time rule, the refusal-keeps-the-line guarantee as one boolean, why
  open-time was rejected, that slash commands stay commands, the milestone's text
  and **why it names the route** (F82), and that the inline composer is outside
  F72. **F83** recorded — the dissolved blocker and the dated comment.
  `CHANGELOG.md` and `docs/functionality.md` describe what a user types and what
  happens to it when the agent moves under them.

- **Commit:** see below.
- **Residual risk:** the **stale-blocked-read** path (parser accepted,
  `registry.answer` came back `NotBlocked`) follows the rule and adds no wording
  but **has no test** — flagged by the implementer as its only judgement call.
  **T4-S6c** (the 64 KiB buffer and the recap milestone) remains; **T4-S9** needs
  R6's cell ring. Four open flakes. The ordinal chord, **F72**, the
  `r`-decision paste arm, the mid-turn counter freeze, **F67**, **F62**,
  **F60**, **F57**, **F53**, **F54**, **F51**, **F52** are open. **Q1's terminal
  matrix still needs a human.**
- **Next:** **T4-S6c** — the bounded foreground buffer and the recap replay, the
  last piece of A5 and the home of the detach milestone.
