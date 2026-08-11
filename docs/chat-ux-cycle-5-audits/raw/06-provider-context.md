# Audit 6 — provider, model, and context

Baseline: clean `af20c26`. Live catalog/filter/JSONL/doctor scenarios were paired
with current-source review of body telemetry, mutation payloads, context cache,
retry, and compaction.

## What worked

- Catalog output was sorted, filterable, normalized, and machine-readable.
- Doctor exposed the effective model, availability, and normalized model count.
- Doctor was read-only and created no runtime state.
- Current code has body completion/failure/cancellation telemetry, bounded
  malformed-catalog reasons, typed `/model` and `/compact` results, and ordered
  assistant deltas.

## Findings

1. **Medium — `doctor --online` calls a catalog check “provider reachable.”** It
   verifies `/models` and effective-model presence, but never sends a chat-route
   request. A healthy catalog plus broken `chat_path` can be all green
   (`main.rs:1463`).
2. **Low — doctor collapses context-cache failure states.** Miss, stale,
   corrupt, unreadable, and absent all become the conservative default through
   the option-returning reader (`main.rs:1409`), while chat has richer cache
   inspection around `main.rs:3924`.

Top priorities: either name catalog reachability precisely or add an opt-in
minimal chat probe; include structured cache state/age/TTL/reason; retain live
regressions for body stall, Retry-After, SIGINT, refresh, and compaction.
