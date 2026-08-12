# Chat UX dogfood — cycle 16 (claude CLI baseline; UI & functionality focus)

Date: 2026-08-12

Baseline: `9b75247` on `dogfood/codex-ux-cycle-6`. Cycle 16 ran the
rotation's Sonnet slot as a focused verify-fix cycle: one deep Sonnet
verification auditor instead of the usual pair (composition varies by
design), aimed squarely at the cycle-15 headline.

## Shipped this cycle

**Offload pressure gating (cycle-15 gap #1).** Tool results over 8 KiB are
swapped for a 500-char preview ONLY under real context pressure: keeping the
result inline must not push the estimated prompt past half the effective
budget (`offload_pressure`, with a three-regime unit test: roomy window
inline, tight window offloads, unknown window stays conservative). The
cycle-15 starvation case — a 1828-line read discarded at 1.5 k of 131 k
tokens — now rides inline (live-verified: the same probe carries
`prompt_tokens: 22268`).

**Honest offload receipts (cycle-15 gap #2).** The `◇` receipt is computed
before substitution but printed after the decision, and discloses the swap:
`◇ 1828 lines → context got a 500-char preview; full text at <path> (model
can fs_read_file it)`. A receipt can no longer describe content the model
never received.

**Receipt noun polish (cycle-15 gap #5):** `fs_list_dir` reports
`N entries`, not `N lines`.

Gates: fmt clean, clippy clean, workspace green (233 bin / 121 CLI with the
known PTY-environmental failure unchanged / 91 core / 138 tools).

## Verification results (Sonnet auditor, kitsu on HF Llama-3.3-70B)

- **Small-window disclosure test: FIXED, confirmed.** Under
  `--context-window 8192` the new receipt appeared verbatim, the pointer
  path was complete (not G5-elided), and the model's answer was accurate
  *given the disclosed preview* — the design intent, working.
- **Cycle-15 task re-run: could not exercise the fix** — the 70B wrote its
  file without reading anything, twice (3.3 s / 2.2 s, 0/5 facts correct,
  fabricated glyph vocabulary). This is M1, a distinct model-behavior fault
  now reproduced 4/4 across cycles 15-16, and it needs a kitsu-side answer
  (see queue #1) rather than an offload answer.
- Regression sweep: HF window false alarm stays gone; run summary truthful;
  no skill-confusion receipts (the 70B never uses the text form).
- Auditor's "offload_pressure has no unit tests" claim is stale: its
  worktree predates the (then-uncommitted) fix commit; the test shipped with
  the fix.

## Ranked gaps (queued for cycle 17)

1. **M1 — write-without-read**: a model that never opened a source file can
   still Write a "summary" of it, and only the missing read counter hints at
   it. Design a kitsu-side guard: e.g. a run-receipt read counter (gap #3)
   plus a nudge when the first Write in a turn cites files never read.
2. **Self-starved range reads**: under a small window the model sometimes
   reads `end_line: 4` and dodges the offload threshold — starvation with no
   receipt at all. Consider disclosing requested-vs-total lines in read
   receipts (`◇ lines 1-4 of 1905`).
3. **Run-summary read counter** (carried): render reads and
   `tool_results_elided` so "wrote without reading" is visible at a glance.
4. **G5 path elision** (carried): `◆ Read(…/src/lib.rs)` drops the crate
   segment while the offload pointer below shows the full path.
5. Help polish batch (carried from cycle 15) + credential-filter redesign so
   openai/anthropic template hints stop self-suppressing.

## Report index

1. [Offload verification](chat-ux-cycle-16-audits/raw/01-offload-verify.md)

## Loop state

2-hour local cron loop live. Rotation: cycle 16 = Sonnet (this file), cycle
17 = Haiku. Cycle 17 starts with the read-counter + range-read disclosure
pair — both are receipts work that directly serves the M1 guard.
