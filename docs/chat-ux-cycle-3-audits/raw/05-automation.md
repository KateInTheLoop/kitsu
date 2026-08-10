# Audit #5 — JSONL automation at `dead112`

Read-only audit. Local mock execution was blocked for this auditor by sandbox loopback isolation, so runtime conclusions are separated from source/test evidence.

## Direct observation

`target/debug/kitsu --config /tmp/kitsu-ux-live-config.toml --jsonl models` exited 2 under sandbox isolation and emitted three valid events in order: `run_started`, typed provider `error`, and error `final_summary`. The connectivity failure was environmental, not a product defect.

## Findings

1. **High — JSONL output failure is silently ignored.** `emit_jsonl` ignores serialization, newline-write, and flush failures. A closed pipe or full filesystem can lose events, including the final summary, while Kitsu continues inference and may report success.
2. **High — scripted JSONL duplicates prompts and commands onto stderr.** `transcript_line` writes full sanitized input before checking JSONL, producing noisy duplicate potentially sensitive content.
3. **High — retries are not represented as physical provider attempts.** Logical `provider_request` events can hide multiple transport attempts, under-reporting throttling, latency, deadline use, and possible cost.
4. **Medium — schema discoverability is prose-only.** There is no event-field catalog, ordering grammar, compatibility policy, complete sample stream, or JSON Schema.
5. **Medium — script session lifecycle remains rejected.** Rejection is structured, but automation cannot manage saved continuity through the script protocol.

## Strong existing coverage

Tests cover success framing, provider/preflight/parse errors, help/version rejection, SIGINT exit 130 across modes, command/turn closure during cancellation, strict incomplete turns, allow-turn-errors, and typed model failures. They were inspected but not rerun by this auditor.

## Top three

1. Make JSONL emission fallible and terminate nonzero on serialization/write/flush/EPIPE failure.
2. Suppress human transcript echoes and routine diagnostics under JSONL.
3. Emit and count every physical provider attempt/retry.

No repository files were edited or committed by the auditor.
