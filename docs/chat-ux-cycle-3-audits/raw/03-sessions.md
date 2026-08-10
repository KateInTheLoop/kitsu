# Audit #3 — session UX at `dead112`

Read-only audit; no repository files were changed.

## Observed evidence

- Root help exposes only `ask`, `models`, `chat`, and `help`; there is no top-level session command.
- `chat --help` exposes `--resume [ID]` and `--no-session`, says piped stdin is one turn, and frames `--script` as replay-oriented.
- Lifecycle operations exist only as `/new`, `/sessions`, `/resume`, `/fork`, `/export`, `/archive`, `/restore`, and `/delete`.
- Documentation says scripts reject interactive-only queue/session transitions, so CI/scripts cannot manage lifecycle through chat scripts.
- Source inspection shows snapshots are created only for interactive sessions when `--no-session` is absent; piped/scripted chat has no durable continuation.
- Resume/fork deliberately reset runtime permissions, with explicit messaging that saved permissions are never restored.
- Documented artifact behavior is strong: fork copies and rewrites persistent pointers; export creates a no-clobber sidecar; archive/restore keep snapshot and artifacts together.
- `--no-session` is documented to disable snapshots and prompt-history persistence.
- An isolated interactive probe was interrupted during execution approval, so end-to-end display and lifecycle mutations remain unverified by this auditor.

## Findings

1. **P1 — Session management is trapped inside an active chat.** Recovery and housekeeping require provider-configured interactive chat even when no inference is needed. Add `kitsu sessions list|show|resume|fork|export|archive|restore|delete` and machine-readable output.
2. **P1 — Scripted and piped workflows have no durable continuity.** Add explicit session/resume support for automation with predictable opt-in autosave semantics.
3. **P2 — Permission reset is safe but poorly recoverable.** Show a prominent reset summary and offer an explicit reviewed re-grant command or named permission profile without saving authority in snapshots.

Secondary improvements:

- Provide lifecycle help and empty-state guidance without a provider.
- Surface artifact counts/locations in session detail and export results.
- Add a status field proving `--no-session` disabled snapshots and history.
- Preserve no-clobber export, exact-ID deletion, leases, and fresh permissions.

No repository files were edited or committed by the auditor.
