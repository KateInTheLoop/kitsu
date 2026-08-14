# Claude UX cycle 35 — observable model catalogs

Date: 2026-08-14
Seed: `992635`
Boundary: Unix epoch `1786746600`

## Scored evidence

All lanes gated the same clean post-Cycle-34 image before their one allowed
attempt. The scored binary was 154,383,248 bytes with SHA-256
`f9ab10a79abd7de662173fba3051af442f007e210ac0d5fe93bb32fae646b9a3`;
its embedded source revision was `77160755f6d1`. The pinned configuration had
SHA-256
`a7752d4264b2d8914ed51ed57486d30b3cf05d131f65aabd7c3bc3865961a297`.

- The seeded context/UX lane selected top-level `models` and scored **97/100**.
  Its owned loopback listener closed immediately before Kitsu's request. The
  command exited `2` in 6.24 ms, emitted the exact secret-free
  `local_ollama_unreachable` code and recovery actions, kept sessions disabled,
  preserved a preexisting unrelated `-o` file, leaked none of twelve endpoint
  decoys, and reaped every child. The three-event stream was
  `run_started -> error -> final_summary`: the real catalog GET had neither a
  logical request record nor a physical-attempt record.
- The deterministic Hugging Face-shaped lane scored **100/100**. No process,
  user-service, or managed HF credential was present, so no hosted request was
  authorized or sent. Its local controls proved that the typed diagnosis is
  absent for an HF-named loopback gateway and exact for Ollama; managed-auth
  precedence/logout, artifact-free bounded output, atomic `-o`, session state,
  redaction, and process reaping all passed.
- The randomized real-model lane selected a three-match repository search and
  scored **85/100**. Kitsu offered only the exact read-only root, returned one
  successful untruncated 242-byte `repo_grep` result containing all three
  matches, then cancelled round two at the 60-second turn deadline. It
  discarded provisional output, emitted truthful cancellation/failure
  receipts, wrote no `-o`, and reaped cleanly. The missing final answer was
  local model/provider latency, not false product success. That trace also
  suggests a lower-priority distinction between request- and turn-deadline
  remaining time.

Codex and Claude standalone inference were not substituted: the randomized
lane had no approved host-network route for the local Codex runtime and no
isolated Claude authentication. Their available surfaces therefore contribute
infrastructure/discoverability context only, not a scored outcome.

## Selected improvement

The typed Ollama postfix worked, but the catalog command violated the published
promise that every physical provider send has an observable attempt. Model
catalogs now use the same two-level transport contract as inference:

```text
run_started
provider_request  purpose=model_catalog, operation_id=models:1
provider_attempt  attempt=1, max_attempts=1, bounded disposition
model_catalog | error
final_summary
```

Top-level `kitsu models` uses `operation_id: "models:1"`. Scripted `/models`
and `/model ...` calls instead retain their enclosing `line` and `command`.
Catalog requests have no messages, tools, structured schema, or usage record.
The OpenAI-compatible adapter's new telemetry-aware catalog method reports one
terminal event per real GET and no attempt for a provider-local cache hit.

The attempt now spans the complete accepted-response lifecycle. A successful
bounded body and catalog-shape validation ends as `body_completed`; malformed
or oversized successful bodies end as `body_failed`; HTTP rejection,
connection/timeout failure, and cancellation retain their existing bounded
categories. Events contain no URL, headers, auth, request body, response body,
model IDs, or endpoint detail. Existing providers remain source-compatible via
the trait's default telemetry method, and historical v1 consumers remain valid
because the new identity and purpose extend existing event variants.

## Regression coverage

- Provider tests cover successful GET telemetry, a silent cache hit, HTTP 503
  rejection without response-body/URL leakage, malformed-success negative
  caching, oversized-body classification, and cancellation of an admitted
  in-flight GET.
- CLI tests pin exact top-level success, HTTP failure, loopback connection
  failure, and SIGINT ordering. They assert `models:1`, `model_catalog`, one
  physical attempt, bounded fields, the existing typed Ollama diagnostic, and
  final accounting.
- Scripted `/models` failure and cancellation tests assert that request/attempt
  records stay inside `command_started`/`command_finished` with exact line and
  command identity. The published schema/render regression pins the catalog
  identity pattern and purpose enums.

## Verification

All **787** workspace tests pass (255 Kitsu unit, 2 provenance, 183 CLI, 100
core, 65 provider, 32 render, and 150 tool tests). Formatting, warning-denied
Clippy, published-schema parsing, and diff checks also pass. The fully verified
pre-checkpoint debug image is 154,623,016 bytes with SHA-256
`4cd1538e94789bd6a9ecb71d905561bcb15f69fb940897c1e720e6dbf8052578`.
The checkpoint changes embedded source provenance, so the next cycle must gate
the rebuilt post-commit image rather than reuse this dirty build.
