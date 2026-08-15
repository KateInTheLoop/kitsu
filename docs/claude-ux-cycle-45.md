# Claude UX cycle 45 — per-request model identity

Date: 2026-08-14
Seed: `992645`
Boundary: Unix epoch `1786764600`

## Frozen release

All lanes gated the immutable post-Cycle-44 binary at
`target/cycle45-pin/kitsu`: SHA-256
`9cd0579408afa2f9f0eb47995c2da79d93a8e8b1a02e2b8c19d89e722a4dc6da`,
154,677,984 bytes, mode `0555`, source revision `538d76e3e2a1`. The pinned
configuration was mode `0444` with SHA-256
`a7752d4264b2d8914ed51ed57486d30b3cf05d131f65aabd7c3bc3865961a297`.
Each lane made at most one post-boundary launch, captured raw evidence before
adjudication, reaped its process group, used no `target/debug` substitution,
and made no shared-tree change or rerun.

## Evidence and attribution

- The context/session lane completed the scored model-switch lifecycle in
  9.64 ms for **99/100**. It committed one turn with the initial model,
  validated and applied `/model`, reported the switched model through
  `/status`, committed a second turn, and saved exactly two clean durable turns.
  The two authenticated POST bodies carried distinct exact model IDs. The
  intervening cached model-catalog lookup emitted one logical request and no
  physical attempt. All 23 events, hashes, redaction checks, session/resume
  checks, and process reaping passed. Evidence SHA-256:
  `693a7f4bc19f9e1876fdd53c2aee659fb5ef053058ad4e9adf358843cc89bad5`.
- The provider-free read-authority lane was
  `not_scored: harness_cli_misconfiguration`: its sole invocation used the
  nonexistent `--allow-read` spelling instead of `--allow-path`. Kitsu failed
  before `run_started` with a typed user error and final summary, made no
  provider request or mutation, and reaped cleanly. Its proposed authority
  observability probe is therefore discarded. Evidence SHA-256:
  `9999c3027f81ef1041095a809b02803132c260d229a38b5896c9b87ca80e83a7`.
- The Hugging Face-shaped command lane placed fresh `-o` inside its writable
  command root. Kitsu correctly rejected that authority overlap before
  `run_started`, provider contact, or mutation; output remained absent and its
  sentinel stayed exact. This is a harness-oracle error, not a product defect,
  and the intended command outcome is unscored. Auth lifecycle, privacy,
  framing, and reaping checks passed. Hosted HF remained unavailable
  infrastructure because every credential gate was false and
  `request_sent=false`. Evidence SHA-256:
  `fd7a4296520c1c5c1040bb0f1473e1b797ff038734cc68fb877dfed22a100e9b`.

No standalone comparator was substituted without an honest isolated analogue.

## Selected improvement

The physical requests in the clean lane named the initial and switched models,
but both corresponding JSONL `provider_request` records omitted model identity.
An automation consumer therefore had to replay `run_started` plus every
intervening model-switch command to attribute each logical request.

Inference and compaction `provider_request` events now include the exact
nonempty `model` selected for that request. Model-catalog requests omit the
field because they are not routed to a model. The field is additive in v1;
absence in an older inference or compaction record means unknown.

## Regression and verification

A wire-level CLI regression runs two turns around `/model`, requires the JSONL
model vector to equal the two actual POST-body model values, and proves the
cached catalog request remains model-less and attempt-less. Core regressions
cover inference and compaction producers, while the published schema and its
renderer guard accept the optional nonempty field. README and protocol docs
define the new and historical-consumer behavior.

All **792** workspace tests pass (256 Kitsu unit, 2 provenance, 187 CLI,
100 core, 65 provider, 32 render, and 150 tool tests). Formatting,
warning-denied Clippy, published-schema parsing, and diff checks also pass.
