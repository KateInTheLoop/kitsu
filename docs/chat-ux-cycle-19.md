# Chat UX dogfood — cycle 19 (claude CLI baseline; UI & functionality focus)

Date: 2026-08-12

Baseline: `55a24a9` on `dogfood/codex-ux-cycle-6`. Cycle 19 ran the
rotation's Opus slot: ship-first on the cycle-18 command-path pair, then an
Opus auditor on a new genre — README quick-start walkthrough validation.

## Shipped this cycle

**Tilde cwd expansion (cycle-18 gap #1).** `resolve_cwd` expands a leading
`~`/`~/…` against $HOME before resolution — kitsu's own tilde-elided
display paths no longer poison model-supplied cwds. Auditor-proved with a
literal `~` directory on disk: `cwd="~"` resolved to the real home, not
`./~`.

**Recoverable environment errors (cycle-18 gap #3, partial).**
Inaccessible/non-directory cwds are execution errors, not policy denials —
recoverable mid-turn under the existing final-round rule. Genuine
outside-roots refusals stay fatal (authority stance unchanged). The audit
shows the remaining half: a real denial anywhere in the turn still discards
a correct final answer at commit (queued as cycle-20 #1).

**Denials name the approved roots (cycle-18 gap #4).** Reaches the model in
full; the HUMAN receipt truncates at 180 chars (queued, cycle-20 #3).

Gates: fmt clean, clippy clean, workspace green (233 bin / 121 CLI with the
known PTY-environmental failure unchanged / 91 core / 139 tools).

## Task results (README quick-start walkthrough, both CLIs)

- **Run A, claude:** 7 m 05.8 s, grade A — executed 12+ README commands;
  all three friction findings reproduced byte-for-byte on independent
  re-verification.
- **Run B, kitsu + HF Llama-70B:** 12.05 s, grade F — delegated, hit one
  denial, then quoted README without executing anything (~80% model fault;
  ~20% brief fault — its pre-approved commands could never have run, see
  gap #2).
- Fix verdicts: tilde FIXED; env-error recoverability half-fixed;
  roots-naming full for the model / truncated for the human. Regression
  sweep clean.

## Ranked new gaps (queued for cycle 20)

1. **A policy-denied turn destroys a correct final answer** —
   `--allow-turn-errors` only flips the exit code; route denial-final
   answers to a CommittedPartial disposition instead.
2. **Startup accepts impossible pregrants**: an `--allow-command` whose
   executable sits outside every command root can never run — root-check
   the executable at pregrant time (fail closed at startup, like cwd).
3. **Tool-error receipts hard-truncate at 180 chars**, first line only, no
   ellipsis — this cycle's own remedy text got eaten; append `…` and keep
   the remedy clause.
4. **Capped reads never disclose true size** — the model read 1,731 of
   15,682 lines and said DONE; render `1731 of 15682 line(s), capped…` in
   receipt AND tool result.
5. **Run summary hides denied/failed tool calls** — append
   `, N denied / M failed tool call(s)` when non-zero (including
   subagents').
6. Carried: worktree gitdir binds, Llama python_tag parsing, G5 elision,
   help polish, credential filter, resume picker, M1 nudge.

## Report index

1. [Quick-start walkthrough audit](chat-ux-cycle-19-audits/raw/01-onboarding.md)

## Loop state

2-hour local cron loop live. Rotation: cycle 19 = Opus (this file), cycle
20 = Sonnet. Cycle 20 starts with gaps #3+#5 (receipt truncation +
denied/failed counts — small, honest-receipts theme) and #2 (impossible
pregrants fail closed at startup).
