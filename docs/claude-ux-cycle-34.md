# Claude UX cycle 34 — typed local-Ollama recovery

Date: 2026-08-14
Seed: `992634`
Boundary: Unix epoch `1786744800`

## Scored evidence

All workers gated the same immutable post-Cycle-33 image and left the shared
build target released during scoring. The scored image was 154,353,200 bytes
with SHA-256
`f53188db19c63c0839a2fa64996847b373203f366ffe915d50f1a8f789e19a9b`;
its embedded source revision was `2fb7bafae0f6`.

- The deterministic context/UX lane scored **100/100** after manual
  adjudication of three stale harness assumptions. A scripted first turn
  committed, its owned loopback listener then closed, and the second turn
  received a terminal connection failure. Kitsu emitted the scoped,
  secret-free local-Ollama remedy, preserved the exact first answer through
  `-o`, kept a one-turn resumable transcript, reported one accepted provider
  failure under `--allow-turn-errors`, and leaked none of the configured URL's
  userinfo/query decoys. The stale checks concerned JSONL stderr, the full-path
  scripted turn id, and the persisted system message; raw event order and
  product behavior were correct.
- The deterministic Hugging Face-shaped lane scored **100/100**. Real process,
  user-service, and managed credentials were absent, so it sent no hosted
  request and classified that limitation as infrastructure. Its local control
  proved that only loopback Ollama receives the specialized remedy: an
  HF-named loopback profile retained generic gateway guidance. Managed-auth
  precedence/logout, bounded artifact-free inference, atomic `-o`, durable
  session state, secret redaction, and child reaping all passed.
- The randomized search lane was **not run**. Its final pin and fixture gates
  passed, but the lane-owned provider could not bind inside the sandbox
  (`EPERM`) before Kitsu launched. It neither connected to the unrelated host
  listener nor retried. This is harness infrastructure evidence and contributes
  no product or model inference.

## Selected improvement

The human recovery was correct, but JSONL automation could distinguish the
trusted local-Ollama case only by parsing English. `provider_attempt` identified
the transport category, while the terminal `error` had no stable code for the
more specific host-owned diagnosis.

Connection failures for the already-scoped loopback-Ollama case now add:

```json
{
  "diagnostic_code": "local_ollama_unreachable",
  "recovery_actions": [
    "start_ollama_serve",
    "verify_ollama_serve",
    "select_another_gateway"
  ]
}
```

The fields are additive to the existing typed error. They do not change the
message, exit `2`, provider-failure accounting, retry behavior, or event order.
They are derived only from trusted gateway configuration and contain no URL,
port, userinfo, query, provider response, or model text. Remote Ollama and
non-Ollama loopback failures omit both fields and keep generic recovery.

The shared `CliError` JSONL payload builder carries the diagnostic consistently
through one-shot, scripted, piped, and top-level catalog failures. Historical
`kitsu.chat.v1` consumers remain valid because both fields are optional; the
published schema requires them as a pair when either is present and bounds every
current enum value.

## Regression coverage

- The unit regression covers selected-name and flavor-based Ollama profiles,
  structural loopback parsing, remote Ollama, non-Ollama loopback, hostname
  lookalikes, and a URL containing userinfo/query secrets. It asserts the exact
  code/actions for the positive case and field absence for every generic
  control.
- The CLI regression drives both JSONL `ask` and scripted chat against an
  unreachable loopback endpoint. It checks exact typed fields, exit/final
  provider accounting, empty JSONL stderr, the unchanged human message, and
  absence of the URL credential. Existing doctor and catalog assertions retain
  their scoped secret-free prose checks.
- The render/schema regression pins the diagnostic enum, the three recovery
  actions, and their paired-field dependency in the published schema.

## Verification

All **783** workspace tests pass (255 Kitsu unit, 2 provenance, 183 CLI, 100
core, 61 provider, 32 render, and 150 tool tests). Formatting, warning-denied
Clippy, published-schema parsing, and diff checks also pass. The fully verified
pre-checkpoint debug image is 154,383,248 bytes with SHA-256
`359416dcccbef483e03a83f507fcd42f6d74182840f9053aab24e05e984f27ba`.
The checkpoint changes embedded source provenance, so the next cycle must gate
the rebuilt post-commit image rather than reuse this dirty build.
