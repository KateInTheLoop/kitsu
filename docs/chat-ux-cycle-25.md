# Chat UX dogfood — cycle 25 (claude CLI baseline; UI & functionality focus)

Date: 2026-08-12

Baseline: `39f8510` on `dogfood/codex-ux-cycle-6`. Cycle 25 ran the
rotation's Haiku slot as a pure verification cycle over cycle 24's ships.

## Verification verdicts (all PASS, live on HF/ollama)

- **Spawn-cap teeth:** forced storm with cap 0 — two refusals, then the
  withdrawal receipt verbatim, model completed INLINE with its remaining
  tools, exit 0. The cycle-23 failure shape is dead.
- **/cost billed truth:** a denied turn no longer erases spend — cumulative
  3633+16=3649 tokens across 2 turns survived the failure, arithmetic
  spot-checked, "subagent spend included" present, run summary truthful.
- **Footer duration contract:** present on TTY, correctly ABSENT in piped
  runs; kitsu-usage records unchanged.
- **Glossary comparison (new genre):** both CLIs accurate; kitsu receipts
  honest (2 reads, 0 writes).

Gates unchanged from cycle 24 (no code changes this cycle).

## Ranked gaps (queued for cycle 26)

1. /cost vs /context accounting drift (carried; one truth, disclosed).
2. Cross-gateway cost attribution when /model switches providers mid-run
   (new, from the verifier).
3. Denial-final disposition redesign (dedicated cycle; carried).
4. Carried: capped-read true size, worktree gitdir binds, python_tag
   parsing, /doctor endpoint line, credential filter, /memory, timer /loop.

## Report index

1. [Verification evidence](chat-ux-cycle-25-audits/raw/01-verify.md)

## Loop state

Hourly main loop (:23) + UI-parity loop (:53; items 1-2 landed, item 3
tool-receipt durations next). Rotation: cycle 25 = Haiku (this file),
cycle 26 = Fable.
