# Chat UX dogfood — cycle 29 (claude CLI baseline; UI & functionality focus)

Date: 2026-08-13

Baseline: `8e85a01` on `dogfood/codex-ux-cycle-6`. Cycle 29 ran the
rotation's Haiku slot as a verification cycle over the cycle-28 trio.

## Verification verdicts (all PASS, live on HF)

- **Provider-pin acceptance:** `/model openai/gpt-oss-120b:fastest`
  accepted (cycle 27 rejected it); /cost attributes under the pinned id.
- **Quoted numeric args:** `"1"`/`"3"` accepted —
  `◇ 3 lines (ranged read 1-3) · 0ms`; the negative probe surfaces the
  repair hint.
- **Successful-repeat guard:** five engineered workspace_diff calls → 3
  real executions (12-13 ms each), calls 4-5 short-circuit at 0 ms.
- **Naming-review comparison:** claude 34.7 s with genuinely deeper
  code-grounded reasoning (found the ember color-role/theme-name
  collision) vs kitsu+HF 14.6 s with surface-level consistency notes —
  model-quality gap, not CLI; receipts honest both sides.

## Ranked gaps (queued for cycle 30)

1. Denial-final disposition redesign (the last big structural item —
   dedicated cycle).
2. Capped-read true-size disclosure — carried.
3. Subagent-path double print — carried, minor.
4. Interesting naming note from the comparison: "ember" is both a palette
   ROLE and a THEME name — consider renaming the theme (hearth/coal) in a
   future polish pass.

## Report index

1. [Trio verification + comparison](chat-ux-cycle-29-audits/raw/01-trio.md)

## Loop state

Hourly main loop (:23) + UI-parity loop (:53; items 1-6 landed, item 7
theme swatches next). Rotation: cycle 29 = Haiku (this file), cycle 30 =
Fable. Cycle 30 is the dedicated denial-final disposition cycle.
