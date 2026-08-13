# Chat UX dogfood — cycle 28 (claude CLI baseline; UI & functionality focus)

Date: 2026-08-13

Baseline: `732b23a` on `dogfood/codex-ux-cycle-6`. Cycle 28 ran the
rotation's Sonnet slot as a ship cycle over the cycle-27 audit trio.

## Shipped this cycle

**`/model` accepts provider-pin suffixes (cycle-27 #1).** The catalog check
validates the BASE id and accepts `:fastest`/`:cheapest`/`:provider`
suffixes — kitsu's own config recipes stopped being rejected by kitsu.

**Successful-repeat guard (cycle-27 #2).** After three identical successful
calls in a turn, further identical calls are answered with a short reminder
instead of re-executing — the 9x workspace_diff storm that cost 6.7x tokens
now stops at three. Regression test proves calls 4-5 short-circuit and the
reminder reaches the model. (Complements the failure breaker and the spawn
teeth: all three storm shapes now have guards.)

**Lenient numeric args (cycle-27 #3).** `start_line`/`end_line` accept both
`1` and `"1"`; a non-numeric string gets a repair-hint error naming the fix.

Gates: fmt clean, clippy clean, workspace green (233 bin / 121 CLI with the
known PTY-environmental failure unchanged / 95 core / 139 tools).

## Ranked gaps (queued for cycle 29)

1. Live-verify the trio on HF (pin switch, storm stop, quoted args).
2. Denial-final disposition redesign (dedicated) — carried.
3. Carried: capped-read true size, worktree gitdir binds, python_tag,
   /doctor endpoint, credential filter, /memory, timer /loop, subagent-path
   double print.

## Loop state

Hourly main loop (:23) + UI-parity loop (:53; items 1-5 landed, item 6
spinner next). Rotation: cycle 28 = Sonnet (this file), cycle 29 = Haiku.
