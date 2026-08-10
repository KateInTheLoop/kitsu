# Cycle 3 automation release audit

Date: 2026-08-10

Scope: JSONL framing, provider-attempt telemetry, durable session continuity,
scripted slash commands, command evidence, and cancellation.

## Findings and disposition

| Severity | Finding | Disposition |
| --- | --- | --- |
| High | A physical provider send dropped by Ctrl+C had no terminal `provider_attempt`. | Fixed: cancellation-safe attempt guards emit a secret-free `cancelled` category/outcome. |
| High | `run_started` preceded resume resolution, omitted the session ID, and could report the pre-resume model. | Fixed: chat/review/script/piped runs emit the resolved model plus `session_id`, `resumed`, and durability fields after session resolution. |
| High | Successful scripted query commands such as `/status`, `/context`, `/diff`, and `/models` disappeared in JSONL because their human stderr was suppressed. | Fixed: query commands may emit one structured `command_result` inside their command frame. |
| High | A durable-session save failure could return after `command_started` without a matching `command_finished`. | Fixed in the command persistence path; a deterministic failing-save integration regression is still desirable. |
| Medium-high | Provider and tool records were drained only after a turn, so retry and tool-start telemetry was not live. | Fixed: the retained event log also supports a live observer; human and JSONL frontends consume the same causal events. |
| Medium | Cancelling `command_exec` produced only generic tool cancellation evidence. | Fixed: the terminal event now includes native `termination: cancelled`, elapsed time, incomplete capture flags, a withheld partial-artifact reference, and truthful best-effort cleanup provenance. |
| Medium | Command-only large output could publish an offload pointer when `fs_read_file` was unavailable. | Fixed: offload pointers require that the read tool is both registered and advertised. |
| High | The published schema defined `command_result` twice (native command payload and slash-command event), and the forward-compatible branch also matched the known event. | Fixed: payload/event definitions have distinct names, known events are excluded, and a renderer regression locks the contract. |
| High | A failed live JSONL write was retained but did not stop the active agent, allowing later sends or mutations without observable events. | Fixed: the fallible live observer requests an orchestrator abort, checked before provider sends and tool execution; focused regressions prove both boundaries. |

## Protocol invariants checked

- Every successful JSONL write is flushed through a process-long fallible
  emitter; a broken output sink makes the process fail nonzero.
- Every `turn_started`, `command_started`, `tool_started`, and physical provider
  attempt has a typed terminal outcome, including cancellation.
- `final_summary` remains the final event whenever the output sink is writable.
- Provider telemetry excludes URLs, headers, bodies, prompts, model text, and
  credentials.
- Command telemetry exposes literal argv and bounded safe evidence without
  reconstructing a shell command.

## Release recommendation

GO. The full workspace test and strict Clippy gates passed, and the independent
post-fix review found no open release blocker. Extra structured results for
additional slash-query surfaces remain a next-cycle enhancement.
