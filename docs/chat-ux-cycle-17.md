# Chat UX dogfood — cycle 17 (claude CLI baseline; UI & functionality focus)

Date: 2026-08-12

Baseline: `a01bd14` on `dogfood/codex-ux-cycle-6`. Cycle 17 ran the
rotation's Haiku slot as a compact ship-and-verify cycle on the receipts
pair serving the M1 (write-without-read) guard.

## Shipped this cycle (all verified live by the Haiku auditor)

**Run-receipt read counter (cycle-16 gap #3).** Every non-interactive run
summary now renders `N file(s) read` unconditionally —
`1 file(s) written, 0 file(s) read` is the visible signature of a model
summarizing sources it never opened. `AgentSessionState.run_reads`
accumulates successful `fs_read_file` calls across the run; scripted mode
mirrors per-turn so cancelled scripts still report honestly.
Verified: read run shows `1 file(s) read`, no-read control shows `0`.

**Ranged-read disclosure (cycle-16 gap #2).** `fs_read_file` receipts
disclose their slice: `◇ N lines (ranged read 1-4)` / `(ranged read
100-end)` — cycle-16's silent self-starvation path now leaves a mark.
Verified live on HF.

Gates: fmt clean, clippy clean, workspace green (233 bin / 121 CLI with the
known PTY-environmental failure unchanged / 91 core / 138 tools).

## Comparison sanity (docs proofread, no-write task)

claude ~60 s vs kitsu+HF Llama-3.3-70B ~4.5 s; both sets of findings valid;
both receipts truthfully `0 file(s) written`.

## Ranked gaps (queued for cycle 18)

1. **Subagent reads don't increment the run counter** — reads inside
   spawned agents display but the parent's `run_reads` stays 0, blunting
   the M1 guard for delegating models (subagent state is separate; needs
   aggregation via the registry telemetry which already counts tool_count).
2. **Ranged-read off-by-one**: `5 lines` reported for a 1-4 range — audit
   the fs slice or lines() accounting.
3. **Range receipts lack the total** (`of 1905`) — needs the fs layer to
   disclose total lines cheaply.
4. Carried: G5 path elision, help polish batch, credential-filter redesign
   (openai/anthropic hints still self-suppress), resume picker, M1 nudge
   design proper.

## Report index

1. [Receipts verification + comparison](chat-ux-cycle-17-audits/raw/01-receipts-verify.md)

## Loop state

2-hour local cron loop live. Rotation: cycle 17 = Haiku (this file), cycle
18 = Fable. Cycle 18 starts with subagent read aggregation (gap #1) and the
off-by-one (gap #2).
