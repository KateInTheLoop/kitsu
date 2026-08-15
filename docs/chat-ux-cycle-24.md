# Chat UX dogfood — cycle 24 (claude CLI baseline; UI & functionality focus)

Date: 2026-08-12

Baseline: `ee1d808` on `dogfood/codex-ux-cycle-6`. Cycle 24 ran the
rotation's Sonnet slot as a tight ship cycle (hourly cadence); live
verification hands to cycle 25's auditor per the ship-first pattern.

## Shipped this cycle

**Spawn-cap teeth (cycle-23 gap #1).** After two refused spawn attempts in
one turn, the spawn tools are WITHHELD for the turn's remainder (the
ISSUE-007 loop-breaker pattern applied to spawning): later rounds advertise
fewer tools, the user sees a withdrawal receipt, and the model gets an
explicit "spawn tools are now WITHDRAWN — work inline" notice while keeping
its other tools. Regression test proves round three advertises only
non-spawn tools even when the storm varies its task args (which is exactly
why the identical-signature breaker never caught it).

**/cost billed truth (cycle-23 gap #2, both halves).** RunTallies gains
prompt/completion/total/turns counters, accumulated at turn end in the
orchestrator — so a rolled-back turn can no longer erase billed tokens, and
because subagent configs share the same Arc, delegated spend counts too.
`/cost` now reads the tallies and says "subagent spend included".

Gates: fmt clean, clippy clean, workspace green (233 bin / 121 CLI with the
known PTY-environmental failure unchanged / 93 core / 139 tools).

## Ranked gaps (queued for cycle 25)

1. Live-verify both fixes on the HF gateway (storm scenario + rollback
   /cost check) — cycle-23's storm repro is the acceptance test.
2. /cost vs /context accounting drift (one truth, disclosed) — carried.
3. Denial-final disposition redesign (dedicated cycle) — carried.
4. Carried: capped-read true size, worktree gitdir binds, python_tag
   parsing, /doctor endpoint line, credential-filter redesign, /memory
   equivalent, timer /loop.

## Loop state

Hourly main loop (:23) + UI-parity implementation loop (:53; item 1 G5
landed at ee1d808, item 2 turn-footer duration next). Rotation: cycle 24 =
Sonnet (this file), cycle 25 = Haiku.
