# Audit 2 — coding, review, and recovery

Baseline: clean `af20c26`. A disposable Git repository and localhost mock drove
attachments, permission failures, malformed tool arguments, diff/review scopes,
and empty recovery paths.

## What worked

- Quoted attachments with spaces and line ranges were exact and JSON-framed.
- Policy denial and malformed arguments remained typed and strict/nonzero even
  when the model later claimed recovery.
- Working/unstaged evidence and tool-free scoped review were host verified.
- Checkpoint/undo empty states were explicit; source tests retain conflict-aware
  targeted undo and trusted model-visible reversal markers.

## Findings

1. **High — ordinary Cargo build left stale embedded provenance.** HEAD was
   `af20c26`, but `target/debug/kitsu --version` remained `f6b4548-dirty`; an
   ordinary `cargo build --bin kitsu` did no work. Only a targeted clean rebuild
   embedded the new commit. The intended invalidation is in `build.rs:17-36`.
2. **Medium — slash `/diff` requires a persistent model read grant.** Without
   `--allow-path`, host-owned verified diff failed; adding it also registered six
   model filesystem tools. Top-level review already separates these authorities
   (`main.rs:8711`).
3. **Low — policy diagnostics expose the internal runtime artifact root.**
4. **Low — plain review can splice usage telemetry onto a model answer lacking a
   final newline; JSONL is unaffected.**

Top priorities: reliable Git build invalidation, host-only bounded diff evidence,
and a maintained full-protocol local coding mock.
