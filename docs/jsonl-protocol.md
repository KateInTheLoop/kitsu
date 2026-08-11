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
  whether continuity was resumed, whether successful work is durable,
  `session_snapshot_state` (`disabled`, `pending_until_first_success`, or
  `resumable`), and whether current system context replaced saved context.
- A turn begins with `turn_started` and ends with exactly one `turn_finished`.
- `provider_request` represents one logical model request in an agent round.
- Provider requests, attempts, and usage carry `purpose: "inference"` for the
  answer-producing loop or `purpose: "compaction"` for a hidden summarizer
  call. A scripted manual `/compact` associates those records with its `line`
  and `command`, between `command_started` and `command_finished`; automatic
  compaction uses the surrounding `turn_id` and round.
- Each completed physical HTTP send produces one `provider_attempt`
  disposition. It is nonterminal only when `retry_scheduled` commits to a later
  attempt; final rejection, transport/body failure, success, and cancellation
  are terminal. If that scheduled attempt is cancelled or loses the deadline
  during backoff, a statusless terminal event for its attempt number closes the
  earlier nonterminal disposition even though no HTTP send began. Retries never
  add another `provider_request`.
- Streaming text that is safe to display appears as ordered `assistant_delta`
  records. Text from a tool-capable round is held until Kitsu has ruled out a
  text-protocol tool call, so raw tool JSON is never mislabeled as an answer.
  Concatenating one turn's deltas reproduces its final `assistant.text`, but
  consumers must treat both as provisional until the matching
  `assistant_disposition` says `committed`. Strict tool/policy or incomplete-turn
  rollback instead closes them as `discarded` with an optional reason.
- When the inference deadline (`--timeout-secs`) expires after real streamed
  output was already delivered, the one-shot paths (`ask`, piped chat, review)
  do not destroy it: a final marker delta (`[stopped: deadline expired …]`) and
  an `assistant` payload are emitted, and the disposition closes as
  `committed_partial` with a reason. The turn still ends with an `error` event
  and exit code 2 — `committed_partial` changes only how the delivered text is
  classified, never the failure status. Scripted (`--script`) turns keep strict
  `discarded` rollback so later turns continue from consistent history.
- `context_window_mismatch` is emitted at most once per resolved window when a
  provider usage report shows the server accepted markedly fewer prompt tokens
  than the request was estimated to contain — evidence that the server's real
  context window is smaller than the resolved one and the input was silently
  truncated. It carries `reported_prompt_tokens`, `estimated_sent_tokens`, and
  `resolved_window` (plus `turn_id` when turn-scoped). Kitsu never rewrites the
  resolved window from this evidence; `/context` reports it as provenance.
- `tool_started` is followed by exactly one matching `tool_finished`, including
  cancellation.
- A detached job whose terminal result was not already consumed is emitted as a
  `background_result` before `final_summary`; successful detached work is never
  silently lost when a script exits. An unacknowledged failed job also makes
  strict automation nonzero.
- Script query commands may place one structured `command_result` between their
  matching `command_started` and `command_finished`; status, permissions,
  context, diff, model-catalog, model-switch, compaction, session inventory,
  skills, profiles, reasoning, tools, help, checkpoint inventory, command
  artifacts, and background-job/thread results never disappear into human
  stderr. Status, permissions, and session inventory retain a compatibility
  `text` field alongside typed fields. Job mutations report both whether state
  changed and the resulting bounded registry/thread snapshot.
- `assistant` follows that turn's runtime events when final text is available.
- `assistant_disposition` follows the assistant payload and closes its
  provisional output transaction before `turn_finished`.
- `final_summary` is the final event for a normally writable sink.

`final_summary.status` is `ok` when every requested operation succeeded,
`partial` when failures were explicitly accepted with `--allow-turn-errors`
and at least one turn or command succeeded, `error` for an unsuccessful run,
and `cancelled` after interruption. `--allow-turn-errors` accepts partial
success only: a script where every turn and command failed still ends with a
run-level error and a nonzero exit.
Its counters distinguish provider/incomplete-model failures, ordinary tool
failures, local policy denials, user/command failures, and cancellations.
Local tool and policy failures exit 1; provider or incomplete-model failures
exit 2.

Events associated with a turn carry `turn_id`. Provider and tool runtime events
also carry the one-based agent `round`. Provider operations initiated by a
scripted slash command instead carry the command's one-based `line` and exact
`command`, so automation never needs a synthetic turn identity.

Command tools use native structured telemetry rather than making consumers
parse nested JSON strings. Their `args`/`output` fields contain stable
`[structured command ...]` placeholders and the optional `command` object
contains argv/cwd/timeout, termination, elapsed time, byte/truncation counters,
safe output tails, executable-resolution provenance, ambient-authority flags,
and any host-owned full-output artifact reference. Artifact capture has a
separate serialized-file byte ceiling while stdout/stderr continue draining.
`captured_bytes` is raw command data retained in the artifact,
`artifact_bytes` is the exact completed on-disk JSONL size including framing and
hex/JSON expansion, and `limit_bytes` bounds that serialized size. `truncated`
discloses omitted output independently of the smaller inline preview.

If cancellation drops an active command, its matching `tool_finished` carries
`cancelled: true` and native `command.termination: "cancelled"`. Captures are
marked incomplete and no unfinished artifact is published. The legacy-named
`artifact_error` field is an artifact-status diagnostic: it distinguishes no
artifact/no cleanup, a sink-reported cleanup request whose result was not
independently verified, and a cleanup failure. It never claims that an unlink
was verified when the executor cannot prove it.

## Safe provider-attempt telemetry

`provider_attempt` contains only a one-based attempt number, configured maximum,
safe category, numeric HTTP status when available, retry timing, remaining
deadline, and outcome. It never contains URLs, headers, tokens, request bodies,
prompts, response bodies, or model text.

`terminal` means no later physical send will occur for that logical request.
The OpenAI-compatible transport does not call header acceptance terminal
success: `body_completed` is the successful terminal outcome, while a bounded
read/decoding failure is `body_failed`. `response_accepted` remains reserved for
adapters that expose a separate nonterminal header phase. `retry_after_used` is
true only when a valid `Retry-After` value governed the selected delay rather
than the local exponential-backoff floor.
An in-flight send interrupted by caller cancellation uses the `cancelled`
category and outcome, with no status or retry delay.

Human mode derives attempt notices from the same runtime events, so retry
counts, status class, delay, terminal outcome, and remaining deadline cannot
silently diverge from JSONL. Those notices retain the same secret-safe fields
and never include provider-controlled body or URL text.
