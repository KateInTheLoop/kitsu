# JSONL automation protocol

Kitsu's `--jsonl` stream uses schema identifier `kitsu.chat.v1`. Every record is
one UTF-8 JSON object followed by exactly one line feed. Embedded newlines are
JSON-escaped and never split a record. The machine-readable contract is
[`schemas/kitsu-chat-v1.schema.json`](schemas/kitsu-chat-v1.schema.json).

## Output reliability

The emitter serializes a complete record in memory, writes the record and its
line feed, then flushes. Serialization, write, flush, and broken-pipe failures
are terminal for that emitter and must produce a nonzero process exit. After a
failure Kitsu must not attempt another JSONL write: the consumer could already
have received a partial record. Consequently, `final_summary` is guaranteed
only while the output sink remains writable.

`stdout` is reserved for JSONL records. Routine prompts, command echoes,
spinners, and human diagnostics must be suppressed in JSONL mode. Fatal local
diagnostics may use `stderr`, but must not duplicate prompts or expose provider
request/response content.

## Envelope and compatibility

Every event has these reserved fields:

- `schema`: exactly `kitsu.chat.v1`;
- `type`: the event discriminator;
- `run_id`: stable for the process invocation.

Payloads cannot override reserved fields. Consumers must ignore unknown fields
and unknown event types. Kitsu may add optional fields and event types within
v1. Removing a field, changing a field's meaning/type, or changing ordering
guarantees requires a new schema identifier.

## Ordering

- `run_started` is the first event after CLI validation; its `mode` is one of
  `ask`, `script`, `piped`, `review`, or `models`.
- Chat/review runs report the resolved `model`, exact `session_id` (or null),
  whether continuity was resumed, and whether successful work is durable.
- A turn begins with `turn_started` and ends with exactly one `turn_finished`.
- `provider_request` represents one logical model request in an agent round.
- Each started physical HTTP send for that logical request produces one
  `provider_attempt`, immediately after it completes or is cancelled. A retry
  adds an attempt event; it does not add another `provider_request`.
- `tool_started` is followed by exactly one matching `tool_finished`, including
  cancellation.
- Script query commands may place one structured `command_result` between their
  matching `command_started` and `command_finished`; status, permissions,
  context, diff, and model-catalog queries never disappear into human stderr.
- `assistant` follows that turn's runtime events when final text is available.
- `final_summary` is the final event for a normally writable sink.

`final_summary.status` is `ok` when every requested operation succeeded,
`partial` when failures were explicitly accepted with `--allow-turn-errors`,
`error` for an unsuccessful run, and `cancelled` after interruption.

Events associated with a turn carry `turn_id`. Provider and tool runtime events
also carry the one-based agent `round`.

Command tools use native structured telemetry rather than making consumers
parse nested JSON strings. Their `args`/`output` fields contain stable
`[structured command ...]` placeholders and the optional `command` object
contains argv/cwd/timeout, termination, elapsed time, byte/truncation counters,
safe output tails, executable-resolution provenance, ambient-authority flags,
and any host-owned full-output artifact reference.

If cancellation drops an active command, its matching `tool_finished` carries
`cancelled: true` and native `command.termination: "cancelled"`. Captures are
marked incomplete, no unfinished artifact is published, and `artifact_error`
confirms that cleanup was requested. Cleanup is best-effort on process failure,
so the protocol does not falsely claim that an unlink was verified.

## Safe provider-attempt telemetry

`provider_attempt` contains only a one-based attempt number, configured maximum,
safe category, numeric HTTP status when available, retry timing, remaining
deadline, and outcome. It never contains URLs, headers, tokens, request bodies,
prompts, response bodies, or model text.

`terminal` means no later physical send will occur for that logical request.
`response_accepted` means the retry loop accepted a successful HTTP response;
the returned streaming body can still report a later decoding or connection
error. `retry_after_used` is true only when a valid `Retry-After` value governed
the selected delay rather than the local exponential-backoff floor.
An in-flight send interrupted by caller cancellation uses the `cancelled`
category and outcome, with no status or retry delay.

Human mode derives attempt notices from the same runtime events, so retry
counts, status class, delay, terminal outcome, and remaining deadline cannot
silently diverge from JSONL. Those notices retain the same secret-safe fields
and never include provider-controlled body or URL text.
