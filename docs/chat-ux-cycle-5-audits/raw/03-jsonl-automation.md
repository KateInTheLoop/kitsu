# Audit 3 — provider-free JSONL automation

Baseline: clean `af20c26`. The auditor deliberately used only provider-free
paths so every command completed inside the sandbox.

## What worked

- Nine help/version/parse-error combinations emitted exactly
  `error -> final_summary`, exited 1, and kept routine stderr empty.
- Empty/malformed scripts, whitespace stdin, unknown commands, `/retry`, strict
  versus allow-errors, and exit counters were truthful and framed.
- Status, permissions, context, sessions, skills, and jobs returned structured
  results; `--no-session` wrote no state.
- `/dev/full` and closed pipes failed promptly and stopped later work.

## Findings

1. **High — query-only durable runs announce an unusable session.** A fresh
   `--session --script` containing `/status` exits 0 and `run_started` includes
   `durable_session:true` plus an ID, but `sessions --json` remains empty. The
   persistence guard is at `main.rs:5695`; the durable claim is emitted around
   `main.rs:6514,7009`.
2. **Medium — JSONL `/checkpoints` has no result.** It emits start/finish/summary
   but no inventory payload because the command returns `Continue(None)` after
   suppressed human output (`main.rs:8654`).
3. **Low — scripted `/help` reports success with no help or error**
   (`main.rs:8549`).

Top priorities: make durable IDs immediately resumable or explicitly pending,
add a typed checkpoint inventory, and define structured scripted help.
