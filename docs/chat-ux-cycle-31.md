# Chat UX dogfood — cycle 31 (claude CLI baseline; UI & functionality focus)

Date: 2026-08-13

Baseline: `dea58ef` on `dogfood/codex-ux-cycle-6`. Cycle 31 ran the
rotation's Opus slot as a ship cycle over the two carried items.

## Shipped this cycle

**Capped reads disclose served-of-total (carried since cycle 19/29).**
The truncation marker now reads `served 64 KiB of 639 KiB total; re-read a
line range for the rest`, and the `◇` receipt quotes it:
`◇ 1732 lines, capped: served 64 KiB of 639 KiB total · 1ms`. Both the
model and the human see the gap size. (Implementation note: the first
attempt appended the disclosure before `cap_output`, which trims the tail
and stamps its own generic marker — the disclosure is now appended after
capping, a deliberate ~100-byte overflow.)

**Foreground subagents run quiet (cycle-15 #3 + cycle-27 double print).**
Their inner narration no longer corrupts the answer channel and their
final text arrives once, through the parent; the `◆ Agent(…)` header and
`◇` receipt remain the progress surface. Background agents were already
quiet.

Gates: fmt clean, clippy clean, workspace green (233 bin / 121 CLI with
the known PTY-environmental failure unchanged / 95 core / 139 tools).

## Ranked gaps (queued for cycle 32)

1. Live-verify both on HF (capped receipt text; single-print delegation).
2. Polish backlog: ember role/theme rename, worktree gitdir binds,
   python_tag parsing, /doctor endpoint line, credential-filter redesign,
   /memory equivalent, timer /loop.

## Loop state

Hourly main loop (:23) + UI-parity loop (:53; items 1-8 landed, item 9
line-buffered streaming styling — the first M item — next). Rotation:
cycle 31 = Opus (this file), cycle 32 = Sonnet.
