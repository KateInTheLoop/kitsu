# Audit 09: provider and model UX

Baseline: `dead112`. Read-only; all traffic used local `auth = none` fixtures.

## Direct runtime evidence

- `models`, case-insensitive filters, and JSONL catalog output succeeded. A zero-match standalone filter exited 0 with a clear message.
- Interactive `/models`, filtered listing, `/model` validation, and non-mutating invalid selection worked cleanly.
- `/context` and `/status` disclosed the configured 32,768-token budget and prompt composition. The supplied profile pins the window, so cold discovery/cache reuse was not runtime-tested.
- A scripted 404 produced an actionable invalid-model message and rolled back the failed prompt; the next successful request omitted it.
- A deterministic 429 path succeeded on its third request after about 0.5 seconds, but human output disclosed no retry and JSONL emitted only one `provider_request`.
- `--timeout-secs 0` was generic in `ask`, piped chat, and JSONL (`request timed out`), while interactive chat included the budget and recovery flag.
- A connection failure exposed reqwest's raw URL rather than the centralized redacted endpoint display.

## Severity-ranked findings

1. **High: retries are invisible and JSONL under-reports provider attempts.** Three actual requests produced one provider-request event, obscuring latency, throttling, deadline use, and possible cost.
2. **High: safe/actionable error rendering differs by mode.** Interactive timeout guidance is strong, but other modes lose the deadline and remedy. Raw connection URLs may bypass endpoint redaction.
3. **Medium: context/cache provenance is invisible.** `/status` and `/context` show the number but not whether it came from CLI, profile, disk cache, discovery, or fallback, nor cache age or refresh state.
4. **Medium: catalog semantics differ by surface.** Standalone output sorts and accepts zero matches; slash output preserves provider order and can treat zero matches as a script failure.
5. **Medium: the heartbeat is generic.** It cannot distinguish connect, retry backoff, first-byte wait, or slow streaming.
6. **Low: slash-command model UX itself is strong.** Discovery, filtering, recovery, validation, and pinned-window disclosure worked.

Provider 404s, 429s, queues, gating, billing, and unavailable models are external. Kitsu owns classification, safe detail, retry/deadline state, and remedies.

## Top three

1. Emit retry telemetry in human and JSONL modes: attempt, safe status category, delay, `Retry-After`, and remaining total deadline. Emit one event per real network attempt.
2. Route all modes through one sanitized actionable provider-error formatter, preserving budget/remedy and redacting URL and body secrets.
3. Expose context-window provenance and cache state (`cli|profile|disk-cache|live-discovery|default`), age/TTL, pending/failure, and refresh; align slash/standalone catalog semantics.
