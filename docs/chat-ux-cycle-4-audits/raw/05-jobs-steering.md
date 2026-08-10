# Audit 5 — background jobs, steering, queueing, and exit

Baseline: `c485edb`. Source and focused test evidence were combined with an
isolated PTY/mock probe. The PTY harness could not reliably progress beyond its
first-byte interaction in every case, so findings that were not reproduced
end-to-end are explicitly source-backed rather than claimed as live failures.

## What worked

- Background admission is bounded and atomic; cancellation is joined.
- Running jobs expose elapsed/model/activity/tool/write/usage telemetry without
  exposing model bodies.
- `/steer` is token-bound to the active turn and bypasses a full future-prompt
  queue; queue entries can be inspected, edited, promoted, removed, or cleared.
- Rollback cancels only jobs started by the affected turn.
- Approval input is isolated from type-ahead collection, asynchronous notices
  use the prompt-safe path, and EOF/quit do not silently abandon running jobs.

## Ranked findings

1. **High — waiting for jobs at quit does not make their results durable.**
   `/quit wait` and EOF wait for terminal agents, then persist only chat
   messages. Any terminal job result the user did not open remains only in the
   in-memory registry and disappears across resume.
2. **Medium — job control entered during generation is not immediate.** Only
   `/steer` and `/queue` use the active-turn control plane. `/jobs show` and
   `/jobs stop` become ordinary queued prompts and may execute after the work
   they were intended to inspect or cancel.
3. **Medium — completion notices omit terminal class.** A notice says a job
   finished but not whether it succeeded, failed, or was cancelled, and points
   users toward the older `/agents` wording.
4. **Low — running `/jobs show` is less informative than `/jobs list` in a few
   fields, and explicit exit choices remain more discoverable in help than at
   the moment EOF occurs.**

## Top improvements

1. Persist or require acknowledgement of unviewed terminal job results.
2. Route `/jobs show|stop|stop-all` through the immediate control plane.
3. Render done/failed/cancelled notices with concise recent activity.

