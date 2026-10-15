# E11 / T1-S4 second card — wiring the agent state file

- **UTC start/end:** 2026-10-13T17:37Z / 2026-10-13T18:07Z
- **Branch/base:** `features/tui_herdr_improvements` / `6fbaaa2`
- **Epic/story/card:** E11 / T1-S4 / second card. This completes story T1-S4.
- **Requirement:** H2, and N1 by construction (no terminal output at all).

- **Evidence and expected failure:** 13 tests written first — 6 on the
  decision, builder and guard, 4 on config parsing, 3 scripted end-to-end.
  Observed 26 compile errors (`unresolved import crate::config::StateFileMode`,
  `cannot find function should_write`, `cannot find StateFileGuard`) plus one
  behavioural failure:

  ```
  `state_file = "on"` must create the state directory at "..."
  ```

  The author again validated the contract with a throwaway stub (637 passing)
  and reverted it.

- **The end-to-end trick worth remembering.** The file is removed on clean
  exit, so a finished run cannot prove it was ever written. The *directory* is
  not removed, only the file, which makes the difference observable: a default
  scripted run leaves no directory at all, while an opted-in one leaves an
  empty `0700` directory. That is what lets the invariant be tested rather
  than merely asserted.

- **In scope:** the `[agents] state_file` key, the decision rule, the payload
  builder, the removal guard, the call sites, deletion of the module-wide
  `allow(dead_code)` obligated by the first card, and the documentation.
- **Out of scope:** a watcher task that rewrites on every registry change
  (a later card), and the reader `kitsu agents --json` (story T1-S5).

- **Authority/failure/version/rollback notes:** no authority surface, no
  network, no terminal output. Config gains an optional nested table, so an
  existing config still loads. Rollback is `state_file = "off"`, or removing
  the three call sites. Every writer error is discarded, so a hint file cannot
  affect a turn's outcome.

- **Tests added or changed:** 13 added, none changed. The decision is pinned
  across all seven carrier identities times all three modes plus an absent
  carrier, asserting explicitly that scripted, piped, ask and JSONL carriers
  are all false under `auto`, that `on` writes even for a JSONL script, and
  that `off` refuses even an interactive session. Config parsing is pinned to
  reject an unknown key and an invalid value as errors rather than silently
  defaulting, and to keep `agent_spawn_cap` flat while rejecting it inside the
  new table. The builder is checked field by field against the registry's own
  snapshot. The guard is checked to remove on drop, keep the directory, and
  not panic when the file is already gone.

- **Implementation:** one `should_write` gate consulted once, reusing the
  existing `InferenceCarrier::is_interactive` predicate (widened to
  `pub(crate)`, a one-line change and the only edit in that module) rather
  than restating the rule. Three write sites and one guard.

- **Review findings and fixes:**
  1. **My briefing was wrong, and the implementer caught it empirically.** I
     said the call sites live "in the interactive chat loop", assuming a
     scripted run drives the same loop. It does not: `run_chat` branches to a
     separate scripted driver before the REPL loop is ever entered, and the
     opt-in end-to-end test runs a script of `/jobs view` and `/quit`, which
     executes zero turns. REPL-only wiring compiled and still failed that
     test. The fix is a session-boundary write placed before any carrier
     driver is chosen — the zeroth turn boundary, and the only hint the
     scripted, piped and one-shot carriers publish — plus turn-start and
     REPL-top writes. The test author had already flagged the same tension
     between my wording and its own test; both were right.
  2. **Reviewer-applied formatting.** The implementer correctly refused to
     reformat an assertion inside another agent's test, leaving one rustfmt
     diff. I applied `cargo fmt -p kitsu` and verified the change is a single
     `assert_eq!` rewrapped across three lines with identical arguments.
  3. Verified by reading rather than by report: the `allow(dead_code)` is gone
     with no replacement anywhere and clippy is still silent, which proves
     every item in the module actually got wired rather than quietly
     stranded; the module contains no `eprintln!` or `println!`, so the three
     output tiers are untouched by construction; and exactly one line changed
     in `process_exit.rs`.
  4. Accepted: the end-of-turn refresh is implicit. Each post-turn arm either
     continues the loop, whose top republishes `idle`, or returns, where the
     guard removes the file. That avoids duplicating a write across roughly
     twenty outcome arms at the cost of the idle state lagging by one loop
     iteration, which no reader can observe.

- **Acceptance commands/results:**
  ```
  cargo test -p kitsu --bins             637 passed  (627 baseline + 10)
  cargo test -p kitsu --bins --release   637 passed
  cargo test -p kitsu --test cli         225 passed  (222 baseline + 3)
  cargo test -p kitsu --test cli --release  225 passed
  cargo clippy --workspace --all-targets   0 warnings
  cargo fmt -p kitsu -- --check          clean
  ```
  `cargo fmt --all -- --check` still reports only the four pre-existing
  `crates/kitsu-tools` diffs (finding F1).

- **Documentation:** `docs/functionality.md` gains the key, the three modes
  and what each means per carrier, the path and its fallbacks, the permission
  and symlink rules, atomic replacement, the turn-boundary refresh cadence,
  removal on exit, and that it is advisory rather than authoritative.

- **Commit:** see below.
- **Residual risk:** the hint refreshes at turn boundaries, not continuously,
  so a background agent that finishes or blocks mid-turn is not reflected
  until the turn ends. That is the gap a watcher card closes, and the
  documentation states the cadence rather than implying freshness. The
  ownership refusal remains untested (needs a second uid), and on non-unix the
  confidentiality guarantees degrade as recorded in the first card.
- **Next card:** T1-S5 — `kitsu agents --json`, the reader: enumerate live
  processes' state files, check liveness, skip stale files, and tolerate an
  older `schema` rather than rejecting the whole listing.
