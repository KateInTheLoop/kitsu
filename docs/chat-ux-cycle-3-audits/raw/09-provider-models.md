# Audit #9 — provider/model UX, commit `dead112`

Read-only throughout: no repository edits or commits. `git status --short` was empty. All traffic used `127.0.0.1` mocks with `auth = none`; no hosted inference or token was used.

## Direct runtime evidence

### Model listing and filtering

```sh
target/debug/kitsu --config /tmp/kitsu-ux-live-config.toml models
```

Exit 0:

```text
mock-developer-model
```

```sh
... models developer
... models MODEL
```

Both exited 0 and returned `mock-developer-model`, directly verifying case-insensitive filtering.

```sh
... models no-such-model
```

Exit 0:

```text
no models matched 'no-such-model'
```

```sh
target/debug/kitsu --config /tmp/kitsu-ux-live-config.toml --jsonl models
```

Emitted valid `run_started`, `model_catalog`, and `final_summary` events. `model_catalog` contained `count:1` and `models:["mock-developer-model"]`.

Against the two-model error fixture:

```sh
target/debug/kitsu --config /tmp/kitsu-error-config.toml models
```

returned sorted standalone output:

```text
mock-alt
mock-good
```

### Interactive model UX

```sh
target/debug/kitsu --config /tmp/kitsu-ux-live-config.toml chat --no-session
```

Observed:

- `/help` clearly documents `/models [filter]`, `/model <name>`, `/context`, and `/status`.
- `/models`:

  ```text
  ~ 1 model(s) available:
    mock-developer-model
  switch with: /model <id>
  ```

- `/models MODEL` normalized the displayed filter and matched successfully.
- `/models zzz-no-match`:

  ```text
  ~ 0 model(s) matching 'zzz-no-match':
  no matches - run /models without a filter to see the full catalog
  ```

- `/model mock-developer-model`:

  ```text
  ~ switched to mock-developer-model (context window pinned by --context-window / config; unchanged)
  ```

- `/model missing-model`:

  ```text
  model 'missing-model' is not in the gateway catalog - run /models
  ```

- `/status` afterward still showed `mock-developer-model`, proving invalid selection was non-mutating.

### Context disclosure

`/context` directly showed:

```text
model: mock-developer-model   window: 32768
used 1993 / 32256 effective ... 6% ... headroom: 30263
```

It also broke down base prompt, project context, skill index, tool schemas, and conversation.

`/status` showed config path, private-state path, endpoint, auth state, and context budget:

```text
config       /tmp/kitsu-ux-live-config.toml
endpoint     http://127.0.0.1:18181/v1/chat/completions
auth         none (no credential required)
context      1993 / 32256 used ... window 32768
```

The supplied config explicitly pins `context_window = 32768`, so cold discovery and disk-cache reuse could not be runtime-tested honestly with that profile.

### Error actionability and transactional recovery

The existing `/tmp/kitsu-error-mock.py` was run locally. Its catalog advertises `mock-good` and `mock-alt`.

A scripted run switched to `mock-alt`, then triggered a deterministic HTTP 404. Kitsu produced:

```text
gateway rejected model 'mock-alt' (HTTP 404) - run /model <valid-id> and check the gateway's available models.
  detail: {"error": {"message": "model not found for request"}}
kitsu-context-rollback: failed turn removed from conversation state
~ failed turn rolled back; completed file changes, if any, remain on disk
```

The next successful request contained `NORMAL_BEFORE` and `NORMAL_AFTER` but not the failed prompt, directly verifying rollback.

A connection failure produced exit 2:

```text
connection error: error sending request for url (http://127.0.0.1:18082/v1/models)
```

### Retry behavior

```sh
target/debug/kitsu --config /tmp/kitsu-error-config.toml \
  --no-stream --timeout-secs 10 ask 'RATE retry-path'
```

Exit 0 after the fixture’s third request:

```text
OK model=mock-good users=["RATE retry-path"] requests=3
```

Elapsed process time was about 0.50s. Neither stderr nor stdout disclosed either 429, attempt count, or backoff.

The equivalent `--jsonl` run also reached `requests=3`, but emitted only one `provider_request` and no retry event. Thus an automation consumer cannot tell that three billable/network attempts occurred.

### Deadline/error-rendering behavior

```sh
... --no-stream --timeout-secs 0 ask 'deadline-zero'
```

Exit 2:

```text
kitsu-context: {"used":4,"effective":7680,"window":8192}
request timed out
```

`--jsonl` correctly classified this as:

```json
{"type":"error","kind":"provider","message":"request timed out","exit_code":2}
```

A non-TTY `chat` invocation also said only `request timed out`.

The same failure in interactive TTY chat said:

```text
request timed out after 0s - raise the budget with --timeout-secs <n> (e.g. --timeout-secs 180) for slower or tool-heavy turns, especially on local models.
~ failed turn rolled back; completed file changes, if any, remain on disk
```

This is a directly observed cross-mode inconsistency.

## Source-supported evidence

- Retry loop handles only 429/503 and network timeouts, honors bounded `Retry-After`, applies jitter, and shares one total deadline: `crates/kitsu-providers/src/openai_compat.rs:529-627, 870-925`.
- It exposes no callback/event per HTTP attempt, explaining absent retry telemetry.
- Actionable timeout formatting exists in `chat_error_message`: `crates/kitsu/src/main.rs:4888-4917`.
- Generic `map_agent_error` reduces the same timeout to `request timed out`: `crates/kitsu/src/main.rs:4920-4930`.
- Standalone model filtering sorts/deduplicates and treats zero matches as success: `crates/kitsu/src/main.rs:1494-1557`.
- Slash listing preserves provider order and returns a failed slash-command result for zero matches: `crates/kitsu/src/main.rs:4395-4550, 4606-4657`.
- Context-window disk cache:
  - 24-hour TTL
  - private `context_window_cache.json`
  - opaque endpoint/model key
  - cold asynchronous discovery with conservative current-run fallback
  Source: `crates/kitsu/src/main.rs:1148-1349, 1447-1476`.
- Per-provider model catalog cache holds success for 60 seconds and eligible failures for five seconds; auth failures are deliberately not cached: `crates/kitsu-providers/src/openai_compat.rs:702-755`.
- `/status` endpoint redaction strips query values and userinfo: `crates/kitsu/src/main.rs:1304-1323`; static-token `Debug` redaction is in `crates/kitsu/src/config.rs:45-90`.
- Redirected stderr gets its first generic heartbeat after 10 seconds and then every 15 seconds: `crates/kitsu-render/src/lib.rs:347-441`. The fast fixtures did not cross this threshold, so runtime heartbeat cadence is source-supported, not directly observed.

## Severity-ranked findings

1. **High — retries are invisible and JSONL materially under-reports provider attempts.** A three-attempt request emitted one `provider_request`. This obscures latency, throttling, deadline consumption, and potential per-request cost.
2. **High — safe/actionable error rendering is inconsistent across modes.** Interactive timeout guidance is excellent, but `ask`, piped chat, and JSONL lose the deadline and remedy. The observed connection error also renders reqwest’s raw URL rather than the central `redact_endpoint` result; credential leakage from URL userinfo/query data is therefore a source-supported risk, though not tested with a real secret.
3. **Medium — context/cache provenance is not visible.** `/context` explains prompt composition, not whether the window came from CLI, profile, disk cache, live discovery, or the 8192 fallback. `/status` shows the number but no source, cache age, discovery state, or refresh control. The switch message also conflates CLI and config.
4. **Medium — catalog semantics differ by surface.** Standalone listing sorts and zero-match exits 0; slash listing preserves provider order and a scripted zero-match is counted as a command failure. Scripts therefore fail for a benign search that the standalone command accepts.
5. **Medium — heartbeat is generic and delayed.** Source provides useful liveness after 10 seconds, but it cannot say whether Kitsu is connecting, backing off after 429, waiting for first byte, or streaming slowly.
6. **Low — slash-command model UX itself is strong.** Discovery, filtering, recovery hints, validation, non-mutating invalid selection, and pinned-window disclosure all worked cleanly.

Provider-side 404s, 429s, queues, gating, billing, and unavailable models are external conditions. Kitsu owns clearly identifying those conditions, exposing retries/deadline state, preserving safe diagnostics, and suggesting the correct remedy.

## Top 3 urgent improvements

1. Emit explicit retry telemetry in human and JSONL modes: attempt/max, safe status category, chosen delay, `Retry-After` use, and remaining total deadline. JSONL needs one event per actual network attempt.
2. Route every mode through one sanitized actionable provider-error formatter. Preserve timeout budget/remedy, distinguish auth/gating/rate-limit/server/connection cases, and redact userinfo/query/token-like material in URLs and provider bodies.
3. Expose context-window provenance and cache state in `/status` and `/context`: `cli|profile|disk-cache|live-discovery|default`, cache age/TTL, pending/failed discovery, and an explicit safe refresh command. Align slash and standalone catalog ordering/no-match semantics at the same time.
