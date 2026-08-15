# Chat UX dogfood — cycle 23 (claude CLI baseline; UI & functionality focus)

Date: 2026-08-12

Baseline: `e785e9f` on `dogfood/codex-ux-cycle-6`. Cycle 23 ran the
rotation's Opus slot at hourly cadence, under the new standing directive:
functionality checks run through the HF GATEWAY, not just local ollama.

## Shipped this cycle

**In-session `/doctor [online]`** — the full read-only diagnostics without
leaving chat; `online` adds the opt-in probes. Audited as byte-identical to
the CLI and broader than `claude doctor`.

**`/doctor online` honesty fix (same cycle, from the audit's #1).** The
probe validates the model-catalog route only, and the text now says so:
`reachable (N models in catalog) — catalog route only; credentials and the
chat route are NOT validated until a real turn`, with the ready line
qualified the same way. Previously a user with an INVALID credential was
told "you're ready" while the JSON knew better. Also fixed the bare-space
field join in the tools line (audit #5b).

Gates: fmt clean, clippy clean, workspace green (233 bin / 121 CLI with the
known PTY-environmental failure unchanged / 92 core / 139 tools).

## Audit verdicts (Opus)

- **PTY `/resume` picker: PASS** — real pty.fork() walkthrough on the HF
  gateway; name-first numbered rows, legend, resolution, /status confirms;
  non-TTY refusal unchanged.
- **HF↔ollama parity sweep: slash surfaces PASS** (byte-identical /theme,
  /context, /help; 131k window correct) — **agent loop/accounting FAIL**
  (see queue).
- **Error-copy comparison:** kitsu+HF 4.5 s vs claude 51.5 s, but claude
  wins on quality decisively (cited real variants; kitsu's model critiqued
  a doc comment). Audit-rubric note: instruct file:line citations.

## Ranked gaps (queued for cycle 24)

1. **Delegation storms are a hard failure on HF**: 8 spawns for a one-word
   prompt, cap refusals ignored 6×, correct answer discarded 3×. The cap
   needs teeth beyond asking — e.g. after N refused spawns in a turn,
   withhold spawn tools for the remainder of the turn (the ISSUE-007
   loop-breaker pattern applied to spawning).
2. **/cost under-reports twice**: a rolled-back turn resets run_usage
   (billed tokens vanish from the total) and subagent tokens never reach
   the parent — route both through RunTallies-style shared accounting.
3. **/cost vs /context disagree** by the tool-schema size, and /context's
   rows don't sum to its own total — one accounting truth, disclosed.
4. `/doctor` text drops the endpoint/catalog details it probed (partially
   improved this cycle; endpoint still absent).
5. Carried: denial-final disposition (dedicated cycle), capped-read true
   size, worktree gitdir binds, python_tag parsing, G5 (in the UI-parity
   queue), credential-filter redesign, /memory equivalent, timer /loop.

## Report index

1. [PTY picker + HF sweep + comparison](chat-ux-cycle-23-audits/raw/01-pty-hf.md)

## Loop state

Hourly main loop (:23) + offset UI-parity implementation loop (:53, one
haiku item per tick from docs/ui-parity-plan.md). Rotation: cycle 23 =
Opus (this file), cycle 24 = Sonnet. Cycle 24 starts with spawn-cap teeth
(gap #1) and the /cost accounting pair (gap #2).
