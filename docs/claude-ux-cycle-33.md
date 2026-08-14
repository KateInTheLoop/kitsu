# Claude UX cycle 33 — scoped local-Ollama recovery

Date: 2026-08-14
Seed: `992633`
Boundary: Unix epoch `1786741200`

## Scored evidence

All workers gated the same immutable post-Cycle-32 image and left the shared
build target released. The scored image was 154,331,912 bytes with SHA-256
`a39b8f2297a4225bb1a9ac716b12cb9b93ce7a95b9c70ed27851f2ec231b140d`;
its embedded source revision was `104fb1dc79ea`.

- The deterministic context/UX lane scored **100/100**. Every provider request
  carried the revised artifact-free wording, retained one historical pointer
  without granting its read root, bounded a 12,232-byte read to 8,033 bytes,
  committed the good turn, discarded a blank turn, installed the last committed
  answer through `-o`, and preserved a clean resumable transcript. It created no
  new artifact generation and changed neither the old sidecar nor the sentinel.
- The deterministic Hugging Face-shaped fallback scored **100/100**. Hosted
  process, user-service, and managed credentials were all absent, so it sent no
  remote request and classified that limitation as infrastructure. Its isolated
  local lifecycle passed managed-auth precedence/logout, two authenticated
  requests, bounded a 20,080-byte read to 7,936 bytes, created no automatic
  artifact/read root, atomically installed the exact answer, and reaped every
  child.
- The randomized edit lane was **not scored** because its sole Kitsu, Codex, and
  Claude attempts all stopped before a model response. Kitsu could not see host
  Ollama from the isolated network namespace; Codex likewise had no visible
  Ollama server, while Claude had no credential. Kitsu nevertheless emitted a
  complete typed provider/postcondition failure, changed no target or sentinel,
  and created no session or automatic-artifact path. This is failure-path and
  infrastructure evidence, not model-quality evidence.

The pre-boundary harness briefly encountered `/tmp` exhaustion. Capacity was
restored before the final hash gates; no scored child had launched and no lane
was rerun. The context lane isolated its mock from an unrelated host Ollama
listener rather than connecting to or stopping that process.

## Selected improvement

The randomized failure showed a precise remediation gap. For a selected local
Ollama profile, Kitsu reported only the generic advice to inspect gateway URL,
network access, and provider availability. The same evidence established that
the configured endpoint was loopback and that the actionable remedies were to
start or verify `ollama serve` or select another gateway.

Provider diagnostics now reduce trusted gateway configuration to one
secret-free boolean: the selected profile is named/flavored `ollama` and the URL
host is structurally loopback. Connection errors for that exact case identify
local Ollama and give those remedies. All other profiles retain the generic
message. The formatter still never interpolates provider-controlled detail or
the configured URL, which may contain userinfo or query credentials.

Loopback recognition accepts `localhost`, any parsed `127.0.0.0/8` address, and
`::1`; it rejects prefix lookalikes such as `localhost.example.test`. The scoped
context is applied consistently to one-shot, scripted, piped, and interactive
chat, compaction and model-catalog slash commands, standalone model listing,
and both online doctor probes.

## Regression coverage

- A unit regression covers loopback parsing, starter-style `flavor = "openai"`
  with the selected `ollama` profile name, explicit `flavor = "ollama"`, remote
  Ollama profiles, non-Ollama loopback profiles, hostname lookalikes, and a URL
  containing userinfo/query secrets. It proves specialized and generic messages
  omit raw provider detail and URL components.
- A CLI regression drives unreachable loopback Ollama through JSONL `ask`,
  catalog-only `doctor --online`, and `kitsu models`. It asserts the scoped
  remedy, exact exit/error framing, final provider-failure accounting, and
  absence of the configured URL credential from every surface.

## Verification

The focused unit and CLI regressions pass. All **783** workspace tests pass
(255 Kitsu unit, 2 provenance, 183 CLI, 100 core, 61 provider, 32 render, and
150 tool tests), as do formatting, warning-denied Clippy, published-schema
parsing, and diff checks. The fully verified pre-checkpoint debug image is
160,484,192 bytes with SHA-256
`a91f0cb6109aa84c1c7f8724c2b2b2e2cd11bc76cfbdb1847cad7bbafb5c7dfc`.
The checkpoint changes embedded source provenance, so the following cycle pins
and reports the rebuilt post-commit image separately.
