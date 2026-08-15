# Chat UX dogfood — cycle 26 (claude CLI baseline; UI & functionality focus)

Date: 2026-08-13

Baseline: `8ef64ae` on `dogfood/codex-ux-cycle-6`. Cycle 26 ran the
rotation's Fable slot as a tight ship cycle.

## Shipped this cycle

**One context-accounting truth (cycle-23 gap #4 / cycle-25 #1).**
`session_context_used` is now the single figure both `/cost` and `/context`
report: the meter's provider-anchored estimate when available, else the
provenance rows (system sources + tool schemas) plus the live conversation
breakdown — `/cost` no longer reads the bare message meter that missed
tool-schema tokens (the exact 110-vs-603 drift the cycle-23 audit caught).
When the anchored total differs from the rows' chars/4 sum, `/context`
says so explicitly: "rows sum to N (chars/4 estimates); used total is
provider-anchored (±D adjustment)". Live-verified: kitsu-context record,
/cost line, and /context used all agree (1779), difference disclosed.

Gates: fmt clean, clippy clean, workspace green (233 bin / 121 CLI with
the known PTY-environmental failure unchanged / 94 core / 139 tools).

## Ranked gaps (queued for cycle 27)

1. Cross-gateway cost attribution (/model switching providers mid-run) —
   carried from cycle 25.
2. Denial-final disposition redesign (dedicated cycle) — carried.
3. Carried: capped-read true size, worktree gitdir binds, python_tag
   parsing, /doctor endpoint line, credential filter, /memory, timer /loop.

## Loop state

Hourly main loop (:23) + UI-parity loop (:53; items 1-3 landed, item 4
inline markdown styling next). Rotation: cycle 26 = Fable (this file),
cycle 27 = Opus.
