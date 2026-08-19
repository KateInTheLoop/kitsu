# E04 primary wire fixture contract

Status: Cycle 028 accepts the compatible-Chat control, distinct Messages text
fixture, normalized text/usage/stop outcome, and wrong-adapter rejection. Cycle
029 adds correlated tool-use/result rounds, HTTP and mid-stream errors,
malformed events, and exact error-body/SSE-line ceilings across both configured
paths. The remaining E04-S01 cases are deliberately pending.

## Primary source pin

The fixtures are reviewed static test vectors, not a runtime dependency on
mutable web documentation. They were checked on 2026-08-19 UTC against:

- OpenAI's `POST /chat/completions` API reference:
  <https://developers.openai.com/api/reference/resources/chat/subresources/completions/methods/create>
- Anthropic's `POST /v1/messages` API reference:
  <https://platform.claude.com/docs/en/api/messages/create>
- Anthropic's typed Messages streaming sequence:
  <https://platform.claude.com/docs/en/build-with-claude/streaming>
- Anthropic's HTTP and streamed error contract:
  <https://platform.claude.com/docs/en/api/errors>
- Anthropic's client tool-use/result correlation contract:
  <https://platform.claude.com/docs/en/agents-and-tools/tool-use/handle-tool-calls>

The review date records when these source claims were checked. Later fixture
changes require a new review receipt and must not silently rewrite historical
acceptance evidence.

## Distinctness contract

The neutral request is one system instruction, one user message, streaming
enabled, and a 64-token output bound. Exact wire differences are observable:

| Property | Compatible Chat control | Anthropic Messages fixture |
|---|---|---|
| Path | `/v1/chat/completions` | `/v1/messages` |
| Required revision header | none in the fixture | `anthropic-version: 2023-06-01` |
| System instruction | `messages[]` item with role `system` | top-level `system` |
| Stream envelope | unnamed `data:` chunks with `choices`, then `[DONE]` | named typed events from `message_start` through `message_stop` |
| Usage | Chat prompt/completion/total fields | Messages input/output fields requiring normalization |

The static fixtures normalize to the same Kitsu-owned deltas: text `hello`,
usage `8 + 3 = 11`, and terminal stop. The current compatible-Chat adapter must
match its own request and stream exactly. Pointing it at the Messages fixture
must produce no neutral delta and end with a typed invalid-response error; EOF
is not completion.

This is protocol evidence, not adapter promotion. No production provider type,
configuration, credential behavior, capability claim, or send path changes in
Cycle 028, and X001 remains failed.

## E04-S01 scenario ledger

| Scenario | Card | State after Cycle 028 |
|---|---|---|
| Exact request, streaming text, usage, explicit stop | C1 | accepted for Chat control; pinned for Messages |
| Wrong-adapter request/stream rejection | C1 | accepted |
| Tool declaration, tool use, and tool result | C2 | accepted for Chat control; exact correlated Messages blocks and `input_json_delta` pinned; wrong adapter emits no partial call |
| HTTP and mid-stream typed errors | C2 | accepted for Chat control and distinct static shapes; status/body remain bounded and wrong decoder fails closed |
| Malformed event and wrong-dialect EOF | C2 | accepted with zero neutral delta |
| Error-body and SSE-line bounds | C2 | accepted across both configured paths at 64 KiB and 4 MiB |
| Aggregate-stream and tool-argument bounds | C2 | existing Chat production gates mapped; native Messages gate remains required before adapter acceptance |
| Timeout and cancellation with no post-terminal work | C3 | pending |
| Unsupported image capability fails before send | C3 | pending; capability contract belongs to E04-S02 |

Provider-free gate:

```bash
cargo test -p kitsu-providers --test provider_wire_matrix
```

Next card: E04-S01-C3 adds provider-free timeout/cancellation cuts, maps every
existing aggregate/tool-argument ceiling into the common suite, and defines the
unsupported-image zero-send handoff to E04-S02 before production adapter code.
