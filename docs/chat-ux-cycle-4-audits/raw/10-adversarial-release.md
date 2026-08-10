# Audit 10 — adversarial release gate

Baseline: clean `c485edb`. `cargo test --workspace` passed all 462 tests. The
review used no real provider credentials and made no source changes.

Release verdict at audit time: **no-go pending one High availability/security
fix**.

## Validated strengths

JSONL is durable and ordered; filesystem authority is separated and protected;
edits require read plus write; undo is conflict-aware; commands use literal
argv, bounded retained output, executable revalidation, and process reaping;
sessions use private pinned storage and never restore grants; explicit
attachments and skill resources are bounded/no-follow; background cancellation
is joined; and terminal/context hardening has broad coverage.

## Ranked findings

1. **High — provider-controlled bodies can exhaust process memory.** Nonstreamed
   chat, model catalogs, and error paths use unbounded response collection.
   Streaming buffers an arbitrary newline-free SSE record and arbitrary tool
   argument fragments. Deadlines bound time, not bytes. Add body, SSE line,
   aggregate tool-argument, catalog, and error-body limits with typed failures.
2. **Medium — no global resource budget spans sync fan-out and background
   agents.** Eight synchronous tasks plus eight background jobs and the
   foreground can drive roughly seventeen nested provider requests.
3. **Medium — explicit skill preload is disabled by `--no-project-context`.**
4. **Medium — a final background completion can remain invisible at shutdown.**
5. **Medium — executable fingerprint checking retains a narrow final
   stat-to-spawn pathname race.**
6. **Medium — interactive autosave failure looks like a successful saved turn.**
7. **Medium — JSONL `/jobs`, `/sessions`, and `/skills` lack result payloads.**
8. **Low — context-cache persistence is unbounded and non-atomic.**
9. **Low — stateless command artifacts have no retention policy.**
10. **Low — `/save` follows ordinary path semantics and overwrites without an
    explicit force choice.**

## Top improvements

1. Bound every provider-controlled response and streaming accumulator.
2. Add one shared agent concurrency/token budget.
3. Split automatic project context from explicit user skill selection.

