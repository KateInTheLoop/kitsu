# E11 / T2-S5b — the answer, and the loop closes

- **UTC start/end:** 2026-10-14T10:00Z / 2026-10-14T11:42Z
- **Branch/base:** `features/tui_herdr_improvements` / `57a2e90`
- **Epic/story/card:** E11 / T2-S5b — the answer half of plan row **T2-S5**.
- **Requirement:** **B2**'s answer path. **Closes F2**; records **F38** and
  **F39**.

- **What shipped.** `/jobs reply <id> <y|a|n>` — allow once, allow always, or
  deny — from an idle prompt, during a running turn, and through the `/agent`
  and `/agents` spellings. A parked agent was previously releasable only by the
  timeout or by cancellation; **milestone T2's loop is now closed**, and the
  last `allow(dead_code)` in the milestone is gone.

- **Seven sites, not three (F38).** The plan has said "all three dispatch sites"
  since T1-S3, and that counts only the sites that *execute* a subcommand. Four
  more decide whether anyone can find it: the typo suggester, two advertised
  `args:` strings, and the usage lines. I enumerated them before briefing.

  The diagnosis matters more than the list: sites 1-3 are `match` arms, so the
  work drives you to them, while 4-7 are **string and array literals that no
  compiler, type or test checks**. Compare T2-S3, which touched *more* places
  but every one was an exhaustive `match` — there the compiler produced the work
  list.

- **F2 is closed by a test rather than by vigilance.** It has been open since
  the first review of this epic: the suggester table had drifted from the
  dispatcher. My own F38 note proposed a seven-item checklist. The test author
  did better — `every_jobs_subcommand_is_its_own_nearest_suggestion` requires
  **all ten** dispatched subcommands to be their own nearest suggestion, so the
  table cannot drift one entry at a time again, which is exactly how F2 arose.
  The implementer fixed the table, not the test.

- **The alias drift was in three files.** `/agent` advertised **five** of the
  ten subcommands it forwards. The test author repaired the `args:` string in
  code — and deliberately left `reply` off it, so the drift test would fail on
  exactly one token and nothing else. Writing the docs I found the README's own
  command-table row carried the **same** stale subset, plus prose describing
  only those five: a third copy, in a third file, of a list already wrong twice.

  I replaced that row with `/agent [any /jobs subcommand]` — what the
  `/agents` row already said, and which **cannot drift again because it states
  the relationship instead of enumerating the set**. That is the cheap version
  of the single-source fix, and it generalises: where a list is derived rather
  than authoritative, say so instead of copying it.

- **One parser, not two.** The implementer calls `parse_permission_answer` —
  the foreground approval prompt's own parser — rather than open-coding a match,
  which is why `yes`, `ALWAYS` and `No` work with no extra code and why the two
  prompts cannot drift apart. The same reasoning that made the palette renderer
  and the row composer shared rather than duplicated. A pleasing consequence:
  `Interrupt` is unreachable from `/jobs reply`, because no spelling maps to it.

- **Review findings and fixes:**
  1. **A behaviour widening the tests forced, flagged rather than slipped in.**
     An unknown `/jobs` subcommand too far from any real one to suggest a
     correction previously printed only the usage line, never naming what you
     typed. It now always names it, with the did-you-mean clause only when a
     suggestion exists. User-visible, so it earned a changelog line.
  2. The implementer spelled out `agent_answer_outcome_label` rather than
     deriving it from `Debug` and lowercasing, because that would have produced
     `notblocked` instead of the pinned `not_blocked`. I checked whether the
     same trick mangles another command's labels and it does not — though I did
     notice one payload emits CamelCase outcome values where this one emits
     snake_case, a wire inconsistency worth knowing but not worth widening this
     card for.
  3. **F39, and the mistake is mine.** My verification run returned 236/1 where
     the implementer reported 237/0 — and **I re-ran to find out which test it
     was, which destroyed the only record of its name.** Six subsequent runs
     were clean and no failure named itself. Recorded as *one in seven,
     unidentified*, explicitly not *absent*, because F23 (a 7% flake invisible
     to manual runs) and F28 (a "flake" that was a real `fork`/`flock`
     production race) both punished the comfortable reading. The rule I wrote
     down: **keep the failing run's output before doing anything else — a re-run
     is not an investigation, it is deleting the evidence.**

- **Acceptance commands/results:**
  ```
  cargo test -p kitsu --bins / --release    771 passed each  (760 + 11)
  cargo test -p kitsu --test cli           237 passed        (233 + 4)
  pseudo-terminal reply tests, 10 runs       0 failures, `needs input` never missed
  cargo test -p kitsu --test tui            13 passed        (unchanged)
  cargo test -p kitsu-tools --lib          413 passed        (unchanged)
  cargo test -p kitsu-tui / -p kitsu-render 48 / 124         (unchanged)
  cargo clippy --workspace --all-targets      0 warnings
  workspace total, all targets             2049 passed, 0 failed
  ```
  Caveat recorded above: one unexplained CLI failure in seven full-suite runs,
  unidentified. The maintainer's `fs.rs` is at `54 20`, verified, with no
  write-mode formatter run in `kitsu-tools` (**F33**).

- **Documentation (standing requirement, in this commit):** `README.md` (the
  `/jobs` row gains `reply`, and the `/agent` row becomes drift-proof),
  `CHANGELOG.md` (the command, plus the unknown-action widening),
  `docs/functionality.md` (the command, that it works mid-turn and through both
  aliases, and that it shares the foreground parser). **F2 closed**, **F38** and
  **F39** recorded, plan progress updated.

- **Commit:** see below.
- **Residual risk:** **F39** is the live one — unidentified, and the next card
  in this area should keep its logs. Sites 5-7 of F38 remain guarded only by
  review, so the single-source-of-truth refactor for subcommand metadata is
  still worth a card. **Milestone T2 is otherwise complete**: a background agent
  can park on a permission it needs, be seen, be answered from anywhere, and
  cannot hang.
- **Next:** **T2-S6** (approval timeline entries and redaction) and **T2-S7**
  (the hooks surface) remain in this milestone; then **T3**, **T4** — the agent
  view, where subagent switching lives — and **T5**.
