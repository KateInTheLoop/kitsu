# E11 / T1-S3 second card — `/jobs explain <id>`

- **UTC start/end:** 2026-10-13T16:41Z / 2026-10-13T17:06Z
- **Branch/base:** `features/tui_herdr_improvements` / `59b3bb8`
- **Epic/story/card:** E11 / T1-S3 / second card. This completes story T1-S3.
- **Requirement:** A2, plus herdr's `agent explain` from §2.4 of the design.
  `/jobs show` answers "everything about this agent" and dumps the bounded
  timeline. Nothing answered the operational question: why is this agent in
  this state, since when, and what is it waiting on.

- **The property that made this card worth doing carefully.** `/jobs show`
  calls `acknowledge_result` for a non-running agent. If `explain` had been
  built on top of it, diagnosing a stalled or failed agent would have silently
  marked its result collected, defeating the guard that refuses to exit while
  a detached result is unobserved. The card is therefore split into a pure
  renderer plus a read-only lookup wrapper, so that the no-acknowledgement
  property is testable at the layer where it can actually be violated rather
  than only assertable by reading the dispatcher.

- **Evidence and expected failure:** 14 tests written first. Observed:

  ```
  error[E0425]: cannot find function `format_agent_explain` in this scope
  error[E0425]: cannot find function `agent_explain_view` in this scope
  ```

  12 occurrences, plus one end-to-end test failing behaviourally with
  `left: 0, right: 2` because `/jobs explain 7` and `/agents explain 7` were
  still rejected by the subcommand table. The author again stubbed both
  functions to prove the behavioural assertions (11 passed, 2 failed, the two
  being the guards on production tables it must not edit) and reverted the
  stub.

- **In scope:** the pure renderer, the read-only wrapper, the `explain` arm in
  both dispatchers, the suggester and registry entries, the usage strings, and
  the documentation.
- **Out of scope:** the `close` entry still missing from the suggester
  (finding F2), any per-id progress accessor, and `/jobs reply`, which belongs
  to T2.

- **Authority/failure/version/rollback notes:** no authority surface, no
  mutation, no schema change — the JSONL payload gains only a new `action`
  value on an existing event shape, which the protocol permits within v1.
  Rollback is removal of the `explain` arms.

- **Tests added or changed:** 14 added, none changed. Per-phase output pinned
  for running, done-uncollected, done-collected, failed-with-class and
  failed-without-class; the no-activity case pinned to exact words; label
  order asserted as a strictly increasing subsequence rather than by presence;
  the whole output asserted `is_ascii()` after stripping the tier-aware
  marker; `explain` asserted to contain none of the timeline or task-dump text
  `show` emits, so the two provably answer different questions; `None` for an
  unknown id; and the headline test asserting the handoff stays `Ready`, the
  unacknowledged count is unchanged, and the grouped view still files the
  agent under `done, result ready` across an `explain` call. End-to-end, a
  non-JSONL scripted run asserts both `/jobs explain 7` and `/agents explain 7`
  reach the id lookup rather than the subcommand rejection.

- **Implementation:** `format_agent_explain(progress, thread)` is pure and
  builds one activity string shared by the running and terminal branches, so
  an empty parenthetical cannot leak. `agent_explain_view(registry, id)` is
  three read-only calls. Dispatch mirrors each handler's own `show` arm for
  argument rules so the two cannot diverge, and the payload deliberately
  carries no `result` key, since there is no read of result text anywhere on
  the path.

- **Review findings and fixes:**
  1. **Inconsistent elapsed formats, caught by the test author.** My spec
     asked for activity ages at one decimal, but the deck already renders the
     same fact through `kitsu_render::format_wait_elapsed`, which truncates to
     whole seconds. `/jobs view` would have said `3s ago` and `/jobs explain`
     `3.2s ago` for one agent. Corrected to the shared formatter, with run
     durations keeping one decimal because that is this file's own precedent
     for that different quantity. Both are now asserted and each rejects the
     other's format, and a comment at the site tells a later reader not to
     unify them.
  2. **Invented vocabulary, also caught by the author.** `show` renders
     pending steering as `2 queued`; my spec said `2 pending`. One fact, two
     words, same label. Corrected to `queued`.
  3. Verified by reading rather than by report: the `explain` arm contains no
     call to `acknowledge_result`, `result`, `peek_result`, `steer`, `prune`
     or `cancel`, and the wrapper is three pure reads. Confirmed the whole
     production diff deletes exactly three lines, all of them usage or args
     strings, so `format_agent_thread`, `format_jobs` and `format_jobs_view`
     are provably untouched.
  4. Accepted as noted by the implementer: the dispatcher reads
     `thread_snapshot(id)` a second time to build the payload's `thread`
     field, because the wrapper returns only a string. A harmless extra lock
     and clone; widening the wrapper's return type to avoid it would couple
     rendering to payload construction for no benefit.

- **Acceptance commands/results:**
  ```
  cargo test -p kitsu --bins             610 passed  (598 baseline + 12)
  cargo test -p kitsu --bins --release   610 passed
  cargo test -p kitsu --test cli         222 passed  (221 baseline + 1)
  cargo clippy --workspace --all-targets   0 warnings
  cargo fmt -p kitsu -- --check          clean
  ```
  `cargo fmt --all -- --check` still reports the four pre-existing
  `crates/kitsu-tools` diffs (finding F1), unchanged.

- **Documentation:** `docs/functionality.md` gains `/jobs explain` including
  the read-only guarantee and why it matters; `docs/ux-design.md` gains its
  line labels, fixed order, ASCII rule and the two-ages rule.

- **Commit:** see below.
- **Residual risk:** neither dispatcher's success payload nor the mid-turn
  arm is unit-tested, because both need an async handler plus a write journal
  or a runtime handle and no neighbouring test drives them; both were verified
  by reading, and the argument rules were copied from each handler's own
  `show` arm to limit divergence. Pinning the success payload needs a scripted
  run against a registry holding a real finished agent, which the script
  harness cannot currently produce — the same blocker recorded as finding F4.
  The `last activity` line also depends on `last_activity_ms_ago` being
  populated for terminal agents, which contradicts its doc comment
  (finding F7); a comment at the site records it.
- **Next card:** T1-S4 — the per-process agent state file, gated
  interactive-only so automation writes nothing, with platform fallbacks,
  restrictive permissions, atomic replacement, and a reader that tolerates
  older schema versions.
