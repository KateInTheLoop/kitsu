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
- New ask/chat/review producers include `run_started.artifact_policy`. Its
  `mode` is `automatic` or `disabled`; `source` is `default`, `config`, or
  `cli`; and the remaining booleans state whether new automatic command/tool
  files and a model-readable artifact root are active. In disabled mode,
  `existing_artifacts_unchanged` is true. This receipt describes automatic
  storage only, not sessions, credentials, `-o`, schemas, required writes, or
  explicit model filesystem operations. Historical v1 records may omit it.
- A turn begins with `turn_started` and ends with exactly one `turn_finished`.
- `provider_request` represents one logical model request in an agent round.
  Inference and compaction requests include `model`, the exact model selected
  for that logical request. This stays authoritative across an in-session
  `/model` switch. Model-catalog requests omit it because a catalog lookup is
  not routed to a model. The field is additive in v1: omission in an older
  inference or compaction record means unknown.
  New producers include `structured_output`; it is true only when that request
  carried a provider-native strict JSON Schema response format. The schema body
  is never duplicated in this event. CLI producers also include
  `request_timeout_ms`, the configured budget independently applied to each
  logical provider request. When a scripted or piped turn has a separate
  enclosing deadline, `turn_remaining_ms` samples that decreasing turn budget
  immediately before the request. Historical or embedded producers may omit
  these additive fields when the corresponding budget is unknown.
- Provider requests and attempts carry `purpose: "inference"` for the
  answer-producing loop, `purpose: "compaction"` for a hidden summarizer, or
  `purpose: "model_catalog"` for an explicit catalog lookup. Usage records
  exist only for inference and compaction. A top-level `kitsu models` lookup
  uses `operation_id: "models:1"`; scripted `/models` and `/model ...` lookups
  use their `line` and `command`, between `command_started` and
  `command_finished`. Manual `/compact` uses the same command identity, while
  automatic compaction uses the surrounding `turn_id` and round. Catalog
  requests have `message_count: 0`, `tools_offered: 0`, and
  `structured_output: false`.
- Each completed physical HTTP send produces one `provider_attempt`
  disposition. It is nonterminal only when `retry_scheduled` commits to a later
  attempt; final rejection, transport/body failure, success, and cancellation
  are terminal. If that scheduled attempt is cancelled or loses the deadline
  during backoff, a statusless terminal event for its attempt number closes the
  earlier nonterminal disposition even though no HTTP send began. Retries never
  add another `provider_request`. New producers include
  `deadline_scope: "provider_request"`: `remaining_deadline_ms` belongs to the
  current logical request and can reset on the next round. When an enclosing
  scripted or piped deadline exists, `turn_remaining_ms` is sampled again at
  the attempt disposition and never regains the per-request budget.
- Model-catalog cache hits still have a logical `provider_request` but no
  `provider_attempt`, because no HTTP send occurred. An uncached catalog GET
  has `max_attempts: 1`; its bounded body read and catalog-shape validation are
  part of that attempt, so malformed or oversized successful bodies terminate
  as `body_failed` rather than being mislabeled as transport success.
- When a connection failure belongs to a selected Ollama profile whose
  configured host is structurally loopback, the terminal `error` also carries
  `diagnostic_code: "local_ollama_unreachable"` and the bounded
  `recovery_actions` values `start_ollama_serve`, `verify_ollama_serve`, and
  `select_another_gateway`. These optional fields describe only facts derived
  from trusted configuration; they never contain the configured URL, provider
  detail, or credentials. Remote Ollama and non-Ollama loopback failures omit
  them and retain generic gateway recovery. Historical v1 streams may omit both
  fields, so consumers must still accept the human `message` as opaque text.
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
- `rollback_retained_changes` is emitted after a turn's `turn_finished` when
  that turn failed (or was cancelled) and was rolled back out of conversation
  state while completed file changes it made remain on disk. It carries the
  verified `checkpoints` ids, `change_count`, a bounded `paths` list (plus
  `omitted_paths` for the rest), and `note_injected: true`: a host-authored
  system message enumerating the same retained paths was appended to the
  surviving conversation state so the next turn's model knows about edits it
  no longer remembers making. No diff bodies are included; when the
  `workspace_diff` tool is registered the note points the model at it for
  exact contents.
- `context_window_mismatch` is emitted at most once per resolved window when a
  provider usage report shows the server accepted markedly fewer prompt tokens
  than the request was estimated to contain — evidence that the server's real
  context window is smaller than the resolved one and the input was silently
  truncated. It carries `reported_prompt_tokens`, `estimated_sent_tokens`, and
  `resolved_window` (plus `turn_id` when turn-scoped). Kitsu never rewrites the
  resolved window from this evidence; `/context` reports it as provenance.
- `tool_started` is followed by exactly one matching `tool_finished`, including
  cancellation. New completions include exact `output_bytes`, raw
  `output_retained_bytes`, `output_truncated`, and `artifact_status`
  (`not_needed`, `retained`, `suppressed_by_policy`, `unavailable`, or
  `failed`). Marker/pointer bytes are excluded from the raw retained count.
  Artifact status is independent of tool success.
- When the model calls the `workspace_diff` tool (registered in chat when the
  launch directory is a Git worktree; disabled by `--no-workspace-evidence` or
  the `workspace_evidence = false` config key), the call is framed as a normal
  `tool_started`/`tool_finished` pair and exactly one
  `workspace_evidence_disclosure` event is emitted between them — the
  `review_disclosure` field set generalized to the tool surface, plus
  `source: "workspace_diff"`. It marks the moment bounded host-verified diff
  bytes entered the model prompt; `model_read_root_granted` is always `false`
  (see docs/workspace-diff-tool.md).
- A top-level `kitsu review --jsonl` emits exactly one `review_disclosure`
  after `run_started` and before the review's `turn_started`. It carries the
  same bounded evidence metadata as scripted `/review`, plus
  `source: "top_level_review"`; it never repeats the diff body and
  `model_read_root_granted` is always `false`.
- With `--output-schema`, one `output_schema` receipt appears after
  `run_started` and before the first `turn_started`. It records the fixed
  provider format/name, exact source byte count and SHA-256, successful bounded
  schema preflight, mandatory local final-answer validation, and the facts that
  neither schema content nor source path is included in the receipt and the
  model has no filesystem authority over the source. A committed disposition
  under this receipt proves the exact assistant text passed local validation.
  Malformed JSON or a schema mismatch instead produces
  `error.kind: "output_schema"`, a discarded disposition, exit `2`, and no
  `output_last_message` installation. New producers also put the receipt's
  `schema_sha256`, the JSON-Pointer-like `instance_path` (`$` is the root), and
  the failing `validation_keyword` directly on that error event; v1 consumers
  must tolerate their absence in historical streams.
- With scripted or piped `chat --require-write PATH`, a
  target that already exists must have both explicit read and write authority;
  a write-only existing target is rejected before `run_started` or provider
  contact, while a not-yet-created target may use write-only authority. A
  `completion_postcondition` receipt with `phase: "armed"` appears after
  `run_started` and before the first turn. It names the resolved target(s), the
  journal-plus-final-state verification policy, and explicitly records that
  the assertion granted no write authority. After turns and joined background
  work finish, a second receipt with `phase: "evaluated"` reports per-path
  verified-write, net-change, current-postimage, byte-count, and SHA-256 facts.
  `status: "satisfied"` precedes any `output_last_message`; `status: "failed"`
  is followed by `error.kind: "postcondition"`, exit `1`, no `-o` replacement,
  and `final_summary.postcondition_failures: 1`. That additive counter is a
  subset of `user_failures`; historical v1 summaries may omit it.
  `--require-write-sha256 PATH=64HEX` implies this same mutation contract and
  adds an exact raw-byte digest assertion without granting authority. New
  producers expose normalized lowercase `{path, sha256}` rows in the optional
  `expectations` array and optional per-result `expected_sha256` /
  `expected_sha256_matches` fields; generic `--require-write` rows use `null`
  match fields. The evaluated digest and latest-write match come from one
  descriptor-pinned postimage. A digest mismatch still uses
  `error.kind: "postcondition"`, exit `1`, and the same counter, preserves the
  retained file effect, and emits no `output_last_message`. It is a run-level
  finalization failure: an earlier committed durable transcript may remain
  resumable even though `final_summary.status` is `error`.
  When `--output-schema` is also active, action-round `provider_request` events
  have tools and `structured_output: false`; the reserved finalization request
  has no tools and `structured_output: true`. If an exact expectation is the
  only failed invariant on an active descriptor-verified write and the budget
  still fits one repair plus finalization, Kitsu emits
  `structured_finalization_deferred` after that action response and before the
  next `provider_request`. It carries `round`, `repair_round`, fixed
  `cause: "required_write_sha256_mismatch"`, expected/actual digest targets,
  `finalization_round_reserved: true`, and explicit disclosure/no-authority
  booleans. The repair request keeps tools and omits the schema. With no such
  capacity Kitsu proceeds to the reserved finalizer and the terminal
  postcondition remains fail-closed.
  A preceding provisional action response is neither emitted as `assistant` nor
  committed. The provisional assistant, any host-authored repair instruction,
  and Kitsu's finalization nudge exist only in their provider requests: none is
  saved, restored, or counted as a durable user turn. The real action
  tool-call/tool-result transcript and the committed final assistant remain in
  history. Both phases share one existing deadline and max-round budget, and
  `--max-rounds 1` is rejected before `run_started` because it cannot contain
  action plus finalization.
  If a model nevertheless returns a native tool call during the tool-free
  finalization request, Kitsu executes none of those calls, discards the turn,
  and exits `2`. The provider-class `error` includes the optional host-owned
  `cause: "tool_call_during_tool_free_finalization"` and an explicit
  no-execution message; this cause never contains provider-controlled text.
- With `-o/--output-last-message`, a successful host-owned atomic installation
  emits one `output_last_message` after the selected committed turn finishes
  and before `final_summary`. It carries `turn_id`, the requested `destination`,
  exact UTF-8 `bytes`, the lowercase `sha256` of those exact bytes, and constant `atomic: true`,
  `assistant_committed: true`, and `content_included: false`; the assistant body
  remains in the ordinary `assistant` event and is never duplicated.
  `model_write_authority: false` makes explicit that this is a host-owned sink,
  not a model filesystem grant. A script
  selects its last fully committed turn. Discarded and `committed_partial`
  payloads do not produce this receipt or replace the destination.
  When `-o` was requested but the run produced no fully committed assistant,
  the destination remains exactly as it was (including remaining absent) and
  `final_summary.output_last_message_state` is
  `"preserved_no_committed_assistant"`. Human output says
  `--output-last-message unchanged: no committed assistant message`. When a
  committed assistant exists but a required-write completion postcondition
  fails before installation, the same field is
  `"preserved_postcondition_failure"`; the destination again remains exactly
  as it was. This additive summary field is absent after a successful
  installation and when `-o` was not requested. Older v1 streams may omit it,
  which means the preservation reason is unknown.
- A detached job whose terminal result was not already consumed is emitted as a
  `background_result` before `final_summary`; successful detached work is never
  silently lost when a script exits. An unacknowledged failed job also makes
  strict automation nonzero.
  Scripted and piped exits **join** live background agents (bounded by
  `--agent-join-timeout`, default `--timeout-secs`) instead of discarding them,
  so a detached job that finishes during the join reports its real terminal
  status with `joined: true`. Only work that outlives the bound is cancelled,
  and its `background_result.result` then carries a shutdown-policy reason
  (`cancelled by shutdown policy: ...`) — never the rolled-back-turn reason,
  which belongs exclusively to agents discarded with a failed parent turn.
- Script query commands may place one structured `command_result` between their
  matching `command_started` and `command_finished`; status, permissions,
  context, diff, model-catalog, model-switch, compaction, session inventory,
  skills, profiles, reasoning, tools, help, checkpoint inventory, command
  artifacts, and background-job/thread results never disappear into human
  stderr. Status, permissions, and session inventory retain a compatibility
  `text` field alongside typed fields. Job mutations report both whether state
  changed and the resulting bounded registry/thread snapshot.
  `/sessions [filter]` matches session IDs, names, and previews
  case-insensitively. Its session-inventory result reports the total `count`,
  `filtered_count`, `current_session_id`, and matching `sessions`; a guided
  miss is still a successful provider-free query and is never saved into the
  conversation transcript. If the live inventory is empty, its compatibility
  text points archived-session recovery to `/restore <exact-id>`; archived
  snapshots cannot be resumed directly.
  Failed commands instead emit a typed `error` before `command_finished`.
  Unknown slash commands retain Kitsu's host-authored recovery in `message`:
  either the bounded closest-command suggestion or `(try /help)`. The message
  is terminal-safe and never contains provider-controlled error detail.
  The `permissions` result carries `commands.exact_grants[].cwd_source`
  (`"explicit"`, `"launch_directory"`, or `null` for a grant created at runtime
  through the approval prompt) and a `host_evidence` object describing the
  host-owned workspace-evidence root used by `/diff`, `/review`,
  `kitsu review`, and the model-callable `workspace_diff` tool:
  `repository_root`, `consumers` (including `"workspace_diff"` when the tool
  is registered), `model_read_root` (always `false` — host evidence never
  becomes a model read root), `prompt_disclosure`
  (`"explicit_user_command_only"`, or
  `"user_command_or_workspace_diff_tool"` when the tool is registered), and
  `excluded_private_paths`. Outside a Git worktree it reports
  `repository_root: null` plus `unavailable`.
  The `command_artifacts` result carries `cross_run` (`generation_count`,
  `bytes`) describing what earlier runs left on disk, alongside the
  in-process `usage`/`artifacts` fields.
  The `diff` result carries `evidence_bytes` and `model_disclosed: false`:
  `/diff` renders host-gathered bytes to the terminal only. `/review` instead
  emits a `review_disclosure` result inside its own command frame, before the
  turn that carries the bytes, with `model_disclosed: true`,
  `model_read_root_granted: false`, and `evidence_bytes`.
  Top-level `kitsu review` has no command frame, so its equivalent receipt is
  the process-scoped `review_disclosure` event described above.
  The `jobs`/`steer` receipt additionally carries `restarts_thread` (always
  `true`) and `discarded_ms`: steering does not append to the running attempt,
  it drops it and restarts the worker on a fresh transcript, discarding that
  many milliseconds of elapsed work. Completed file effects survive.
  When containment is `ready`, `commands.containment.environment` (cycle 8)
  lists every `(name, value)` pair the contained command actually receives —
  the fixed non-secret base set plus any `--command-env` pass-through, merged
  (pass-through wins on a name collision) — and
  `commands.containment.passthrough_environment` lists just the names of the
  caller-approved pass-through entries, a subset of the names in
  `environment`. A value for a secret-shaped name (case-insensitive
  `KEY`/`TOKEN`/`SECRET`/`PASSWORD` substring) is reported as `"<redacted>"`
  in both the JSONL event and `/permissions` text even though the contained
  process receives the real value — this is a disclosure-surface redaction,
  not a containment change.
  An agent profile's `reasoning_effort` may be `"inherit"` — the default — which
  means the spawn sends **no** `reasoning_effort` request field and keeps the
  parent invocation's policy (itself omitted unless `--reasoning-effort` was
  passed). `"none"` remains distinct: it explicitly sends the field. Both
  built-in profiles ship as `"inherit"` so they work on gateways that reject
  unknown request fields.
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
New producers also emit `final_summary.successful_operations`, the exact count
of completed model turns and successful query/action commands. Control flow
such as `/quit` is deliberately not counted even though its own
`command_finished.status` is `ok`. A committed operation remains counted when
a later host-side output, session, or completion-postcondition step makes the
run fail. The field is additive in v1: absence in an older stream means
unknown, never zero.
A terminal assistant payload containing no non-whitespace text is an
`incomplete_turn`: its provisional `assistant` event is closed by a
`discarded` disposition, its conversation state is rolled back, and strict
automation exits `2`. Empty content on an intermediate message that carries
tool calls remains valid.
Its counters distinguish provider/incomplete-model failures, structured-output
validation failures, ordinary tool failures, local policy denials,
user/command failures, and cancellations. `output_schema_failures` is the
subset of `provider_failures` where transport completed but Kitsu rejected the
terminal assistant bytes locally against `--output-schema`; this preserves the
existing provider-failure classification while making the cause visible to a
summary-only consumer.
The `*_failures` counters count failed requested operations or turns and remain
an exclusive outcome classification. `tool_errors` instead totals individual
foreground tool-call errors across all attempted turns; `policy_errors` is the
subset caused by local authority denials. `recovered_tool_errors` and
`recovered_policy_errors` count those errors only when their turn ultimately
received a committed assistant disposition. Accepting a failed turn with
`--allow-turn-errors` does not make it recovered. Because policy denials are
currently terminal, `recovered_policy_errors` is currently always zero. New
producers emit all four tool-error counters and `output_schema_failures`; v1
consumers must treat absent fields from older streams as zero.
Local tool and policy failures exit 1; provider or incomplete-model failures
exit 2. `final_summary.session_snapshot_state` reports the terminal durability
state: `disabled`, `pending_until_first_success`, or `resumable`.
`final_summary.session_id` is non-null only in the `resumable` state, so every
ID in the terminal receipt resolves to an installed snapshot. A failed, empty,
or query-only fresh session therefore ends with a null ID and
`pending_until_first_success`, while
a failed turn in an already resumed session retains its resolvable ID. The
planned fresh ID remains available in `run_started` for correlation without
being misrepresented as durable. These fields are additive in v1; consumers of
historical summaries must tolerate the absent state field.

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
discloses omitted output independently of the smaller inline preview. The
additive `command.artifact_status` uses the same vocabulary as the surrounding
tool event. Under `--no-artifacts` / `artifact_storage = false`, it is
`suppressed_by_policy`, no artifact or artifact-error path is emitted, and the
bounded command result remains usable inline.

For non-command results in disabled mode, output larger than 8 KiB is replaced
before the next provider request by a deterministic UTF-8-safe head/tail value.
Its host marker names original, retained, and omitted raw bytes; there is no
recoverable path. This applies to tool errors and background-agent registry
results as well as successes. Small complete values use `not_needed` rather
than claiming suppression lost bytes. Existing sidecars and historical pointer
messages are preserved but receive no managed artifact read root in that run.

When a contained command exits nonzero and its bounded output matches a
missing-module/missing-command shape (`No module named`, `command not found`,
…), the optional `command.containment_hint` field carries a bounded
host-authored diagnosis naming the likely cause (the mandatory containment
environment) and the concrete remedies (`--command-env`, `--command-root`).
The same text reaches the model-visible tool result and the human receipt; it
is advisory only and never changes the command's status fields.

If cancellation drops an active command, its matching `tool_finished` carries
`cancelled: true` and native `command.termination: "cancelled"`. Captures are
marked incomplete and no unfinished artifact is published. The legacy-named
`artifact_error` field is an artifact-status diagnostic: it distinguishes no
artifact/no cleanup, a sink-reported cleanup request whose result was not
independently verified, and a cleanup failure. It never claims that an unlink
was verified when the executor cannot prove it.

## Safe provider-attempt telemetry

`provider_attempt` contains only a one-based attempt number, configured maximum,
safe category, numeric HTTP status when available, retry timing, explicitly
scoped remaining deadlines, and outcome. `remaining_deadline_ms` is always the
nested provider-request countdown; `turn_remaining_ms`, when present, is the
independently enforced enclosing-turn countdown. It never contains URLs,
headers, tokens, request bodies, prompts, response bodies, or model text.

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
