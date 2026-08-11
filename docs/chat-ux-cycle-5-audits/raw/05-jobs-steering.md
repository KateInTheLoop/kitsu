# Audit 5 — jobs, queue, steering, and exit

Baseline: clean `af20c26`. A real PTY and scripted runs exercised detached work,
eight-job admission, progress, stop/prune, a full FIFO, steering, partial input,
notices, EOF, quit choices, new-session reset, and retries.

## What worked

- Job nine failed closed without evicting the eight admitted jobs.
- Running jobs exposed model/activity/capacity without response bodies.
- Stop-all cancelled and joined all tasks; running records could not be pruned.
- Unopened results were surfaced on wait/cancel/EOF/new instead of disappearing.
- `/steer` bypassed a full FIFO, rolled back the active turn, and prioritized the
  corrected request. Queue edit/promote/remove/clear were behaviorally correct.
- Partial plain input survived async completion without transcript corruption.

## Findings

1. **Medium — job and exit controls are not immediate during foreground
   inference.** `/jobs stop[-all]` and `/quit cancel` enter the ordinary FIFO;
   only `/steer` and `/queue` use the active control plane (`main.rs:6268`).
2. **Medium — `/new` emits a stale cross-session completion notice.** Results
   are surfaced and the registry resets, then the old snapshot baseline produces
   `background agents finished` pointing to an empty new registry
   (`main.rs:7484,7626`).
3. **Medium — queue feedback loses chronological order.** Success notices and
   refusal errors are stored separately, so an earlier overflow error prints
   after later successful controls (`main.rs:6292,7996`). The captured count also
   reports final occupancy rather than accepted input count.
4. **Low — running `/jobs show` exposes less progress than `/jobs list`.**
5. **Low — notices still say `/agents` and collapse terminal outcome classes.**
6. **Low — queue-full documentation says wait while runtime refuses overflow.**

Top priorities: immediate operational controls, reset notice state across
sessions, and one ordered type-ahead outcome stream.
