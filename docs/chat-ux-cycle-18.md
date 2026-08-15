# Chat UX dogfood — cycle 18 (claude CLI baseline; UI & functionality focus)

Date: 2026-08-12

Baseline: `27abab6` on `dogfood/codex-ux-cycle-6`. Cycle 18 ran the
rotation's Fable slot: ship-first on the receipts-aggregation pair, then a
Fable auditor on a NEW task genre (changelog from git history) that
exercised the command path end-to-end on hosted HF.

## Shipped this cycle

**Shared run-read counter (cycle-17 gap #1).** One Arc counter spans the
main conversation and every spawned subagent (`AgentConfig.read_counter`,
same sharing pattern as the spawn budget), so the run receipt's
`N file(s) read` includes delegated reads. Live-verified: a qwen run that
delegated its read to a subagent reports `1 file(s) read`.

**Truncation-marker accounting (cycle-17 gap #2).** The "off-by-one" was
the byte-cap truncation marker counted as a line: read receipts now exclude
marker lines and disclose `, capped at the byte limit` explicitly.

Gates: fmt clean, clippy clean, workspace green (233 bin / 121 CLI with the
known PTY-environmental failure unchanged / 91 core / 138 tools).

## Task results (changelog from git log, both CLIs)

- **Run A, claude:** 26.1 s, grade A (accurate, genuinely user-facing).
- **Run B, kitsu + HF Llama-70B (as specified):** 2.9 s, F — PolicyDenied:
  the model echoed kitsu's own tilde-elided banner path as `cwd`, and
  `resolve_cwd` does no tilde expansion. Kitsu poisoned its own model.
- **Run B2 (absolute cwd):** F — git exit 128: the auditor worktree's
  `.git` FILE points at a gitdir outside the sandbox bind roots.
- **Run B3 (plain repo):** 5.4 s, C- — clean command→write chain, truthful
  receipts throughout (`exit 0 | 6 ms | un-elided artifact path`;
  `0 file(s) read, 1 file(s) written`), mediocre prose.
- Regression sweep: no window false alarm; read counter honest everywhere.

## Ranked new gaps (queued for cycle 19)

1. **Tilde cwd self-sabotage**: expand leading `~/` in `resolve_cwd` (or
   advertise absolute paths; or default cwd to the first command root).
2. **Sandbox vs linked worktrees**: parse `.git`-file `gitdir:`/`commondir`
   pointers and bind them read-only so git works in worktrees.
3. **Environment-shaped denials are fatal**: one spurious PolicyDenied
   discarded the turn with 24 rounds unused — feed such denials back as
   recoverable receipts instead.
4. **Denial receipts should name the approved roots**, not just the bad cwd.
5. **Llama `<|python_tag|>name(...)` tool syntax** lands as raw text —
   parse it or fire a corrective receipt like the skill-confusion recovery.
6. Carried: G5 elision, help polish, credential-filter redesign, resume
   picker, M1 nudge, range-total disclosure.

## Report index

1. [Changelog day task + command-path findings](chat-ux-cycle-18-audits/raw/01-changelog-task.md)

## Loop state

2-hour local cron loop live. Rotation: cycle 18 = Fable (this file), cycle
19 = Opus. Cycle 19 starts with the command-path pair (tilde cwd + denial
recoverability) — both are cheap, high-frequency papercuts the audit hit on
its first command.
