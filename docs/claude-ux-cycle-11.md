# Claude/Codex UX cycle 11: inference-grade Hugging Face readiness

## Challenge

Run the seeded three-worker parity rotation after a laptop restart, prefer the
Hugging Face gateway, and distinguish catalog availability from actual chat
permission without exposing credentials or spending three calls on a known
authentication failure.

`HF_TOKEN` was absent from both the Kitsu process and the user-session
environment. No credential value was read or printed, so the Hugging Face legs
stopped at preflight. The workers used the documented local Ollama fallback and
separate in-process Codex fixtures. A standalone Codex CLI was not available on
the host, so its lifecycle scores remain provisional.

## Seeded comparison

Cycle seed `992598` produced three isolated fixtures under `/tmp`. Every worker
hashed its fixture and an outside sentinel before and after the run.

| Task | Kitsu | Codex runner | Attribution |
|---|---:|---:|---|
| Read boundary | 89/100; safe failure | 91/100; exact result | `llama3.2` serialized numeric file-tool fields as strings, then guessed; Kitsu rejected the calls and discarded the guess. |
| Surgical edit | 99/100; exact one-line edit | 94/100; exact one-line edit | Product gap: spawn cap zero still advertised four unusable subagent tools. |
| Command recovery | 78/100; bounded timeout | 91/100; verifier exits 7 then 0 | Model never invoked the command and made four invalid edits. Product gaps: disabled agent schemas remained advertised and `final_summary.tool_failures` stayed zero despite four failed tool events. |

No outside-fixture write, unauthorized command, credential disclosure, or
untruthful committed answer occurred. The read and recovery outcomes are model
behavior rather than failures of Kitsu's permission boundary. The recovery
trace also exposed a deadline-label inconsistency: a cancelled provider attempt
reported substantial remaining time before the turn ended as a provider
timeout.

## Selected improvement

Catalog-only `doctor --online` cannot prove that a credential may call the chat
route. Kitsu now provides a separate `doctor --inference` readiness probe:

- it conflicts with catalog-only `--online`, making the requested scope
  unambiguous;
- it sends exactly one non-streaming, tool-free completion with no project
  context, a short user message, and `max_tokens = 1`;
- JSON reports `scope = "chat_inference"`, `chat_route_probed`, `request_sent`,
  and `potentially_billable` explicitly;
- missing local auth skips the request and retains the same sanitized recovery
  hint as normal inference startup;
- successful human output states what was verified without printing model
  content; and
- 401, 403, billing, model/route, quota, transport, timeout, and malformed
  response failures use bounded diagnostics that never echo provider bodies.

The recurring HF runner can now require a reachable inference report before it
launches the three task legs. This removes the earlier false-green state where
`GET /v1/models` succeeded but chat returned an access error.

## Verification

Wiremock integration coverage proves the exact minimal request shape, one
request per invocation, machine and human success receipts, secret-safe
classification for access/billing/model/quota failures, and a zero-request
missing-auth path. Focused doctor and missing-auth tests pass. Live local
preflight against the shipped Hugging Face profile returned exit `1` with
`chat_route_probed = false` and `request_sent = false`, as required, because
`HF_TOKEN` is still absent. The full workspace test suite, formatting check,
Clippy with warnings denied, and both staged and unstaged diff checks pass. A
real local Ollama probe then exited `0` with `scope = "chat_inference"`,
`chat_route_probed = true`, `request_sent = true`, and `status = "reachable"`.

## Next backlog

1. Restore `HF_TOKEN` through a user-controlled secret channel and run the new
   inference gate, then the three HF legs.
2. When `--agent-spawn-cap 0`, omit all subagent tool schemas from `/tools` and
   provider requests.
3. Count failed tool calls in `final_summary` and reconcile cancellation versus
   end-to-end timeout labels.
4. Add or locate a standalone Codex CLI for symmetric JSONL/exit comparisons.
5. Make `/clear` preserve the previous durable session instead of overwriting
   its transcript.
