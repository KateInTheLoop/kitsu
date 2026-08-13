# Chat UX dogfood — cycle 30 (claude CLI baseline; UI & functionality focus)

Date: 2026-08-13

Baseline: `81eb392` on `dogfood/codex-ux-cycle-6`. Cycle 30 ran the
rotation's Fable slot as the long-reserved DEDICATED cycle for the
denial-final disposition redesign — carried since the cycle-19 audit and
promoted in nearly every cycle since.

## Shipped this cycle

**Policy-denied turns commit their truthful final answer as PARTIAL.**
When a piped turn completes a real final answer but strict evaluation
finds policy denials, the answer is no longer destroyed: the disposition
is `CommittedPartial` (the deadline path's existing vocabulary), the
session persists when durable, and the human sees
`~ the answer above is COMMITTED AS PARTIAL: <issue>; the run still fails
so automation notices`. The AUTOMATION CONTRACT IS UNCHANGED: same exit
code (live-verified exit 1), same JSONL `error` and `turn_finished`
frames, same failed-run receipt. Provider-kind strict failures
(incomplete turns) keep the full rollback — there is no real answer to
preserve. Scripted multi-turn rollback semantics are deliberately
untouched (transactional turns are a different contract).

Live verification (HF Llama, /etc write attempt with a real write tool
registered): denial receipt, partial-commit disclosure, run summary
`0 turn(s) ok / 1 failed … 1 denied tool call(s)`, error line, exit 1 —
every layer telling the same story. The model was refused, adapted,
answered honestly, and honesty is no longer punished.

Gates: fmt clean, clippy clean, workspace green (233 bin / 121 CLI with
the known PTY-environmental failure unchanged / 95 core / 139 tools).

## Ranked gaps (queued for cycle 31)

1. Capped-read true-size disclosure — carried, now the top item.
2. Subagent-path double print — carried, minor.
3. "ember" role/theme rename polish idea; worktree gitdir binds;
   python_tag parsing; /doctor endpoint line; credential-filter redesign;
   /memory equivalent; timer /loop — carried.

## Loop state

Hourly main loop (:23) + UI-parity loop (:53; items 1-7 landed, item 8
banner eliding next). Rotation: cycle 30 = Fable (this file), cycle 31 =
Opus.
