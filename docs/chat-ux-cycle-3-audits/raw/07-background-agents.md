# Audit #7 — background agents and interactive control

Baseline: `dead112`. Read-only audit. Loopback approval was interrupted, so runtime-only claims are marked unverified.

## What works

- `spawn_agent_bg` starts a quiet task, returns a deterministic ID, and stores running/done/failed/cancelled state.
- `/agents` lists ID/status/task; `/agents <id>` returns current status or final result.
- Foreground rollback/Ctrl+C cancels only agents created by that turn, aborting and joining before terminal status.
- EOF waits for detached work and permits Ctrl+C cancellation; script/piped modes reject unfinished detached work rather than silently succeeding.
- The visible prompt queue is a bounded eight-entry FIFO; overflow refuses without eviction. Unix type-ahead pauses when full or while approvals own stdin.
- Styled notifications use Rustyline's prompt-aware external printer.

## Severity-ranked findings

1. **High — no user-facing cancellation or resource limit for background agents.** There is no `/agents cancel`, `/stop`, pause, cancel-all, or registry concurrency cap for repeated background spawns.
2. **High — no active-turn steering distinct from queueing.** Every complete line waits in the same FIFO until the current turn finishes.
3. **High — plain-mode async notices are not prompt-safe.** Raw concurrent `eprintln!` can splice notices into streaming or partially typed input. This is inferred, not freshly PTY-reproduced.
4. **Medium-high — exit behavior is inconsistent.** `/quit` refuses while agents run; Ctrl+D waits and a second Ctrl+C cancels. The choice is not direct or discoverable.
5. **Medium — `/agents` lacks progress.** No elapsed time, model, recent output/activity, tool/write count, usage, or failure classification.
6. **Medium — queue management is append/show only.** There is no remove, edit, reorder, clear, or promote.
7. **Medium — registry growth is unbounded for a session.** Terminal records have no prune/archive control.
8. **Low-medium — automatic type-ahead is Unix-only.** Other platforms get a materially different interaction model.

## Top three

1. Add `/agents stop <id|all>` or unified `/jobs`, progress/output, and explicit concurrency/cost limits.
2. Add a distinct in-flight steering path plus queue edit/remove controls and visible hints.
3. Route all async notices through one prompt-safe renderer, with PTY coverage for partial input, streaming, exit waiting, and cancellation.

No repository files were edited or committed by the auditor.
