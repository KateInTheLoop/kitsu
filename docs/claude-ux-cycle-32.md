# Claude UX cycle 32 — artifact-free historical context

Date: 2026-08-14
Seed: `992632`
Boundary: Unix epoch `1786739400`

## Scored evidence

All workers used the immutable Cycle 32 pin directly and left the shared build
target released. The scored image was 154,331,912 bytes with SHA-256
`a9131ca0370a303d21ff003362dad1df73d0f871681c5a912f33ef865a89f036`.

- The deterministic context/UX lane scored **99/100**. It resumed a real
  artifact-bearing session under CLI-disabled artifact storage, read a 12,232
  byte file through the generic inline bound, ran a 9,037 byte command inline,
  committed one turn, discarded a later blank turn, and atomically installed
  the last committed answer through `-o`. The historical sidecar and its one
  pointer remained byte-identical; no new generation, pointer, lock, global
  artifact entry, or managed read root appeared. The only product finding was
  a low-severity system-prompt contradiction: the prompt denied that any saved
  path existed even though resumed context truthfully retained the historical
  pointer.
- The Hugging Face-shaped lane was **not scored**. The outer sandbox denied its
  localhost mock before bind. Hosted credential gates were also false, so no
  remote request was sent. The Kitsu path that did run emitted truthful
  disabled-policy and provider-failure receipts, created no artifact path,
  preserved every fixture hash, and reaped every child. This is
  harness/infrastructure evidence, not a product result.
- The randomized large-output lane was **not scored**. Its sole Kitsu attempt
  could not reach local Ollama through the outer network namespace and stopped
  before model or tool activity. Kitsu changed no fixture or automatic
  artifact path. The isolated comparison preflights also showed that Codex
  created SQLite/cache state despite `--ephemeral`, while Claude created
  configuration, telemetry, and temporary state despite
  `--no-session-persistence`; neither comparator reached inference. These are
  direct surface observations, not scored task outcomes.

## Selected improvement

Artifact-free mode promises that the current run creates no new automatic
artifact and grants no managed artifact read root. It does not rewrite durable
history or delete evidence from earlier runs. The model instruction now states
that exact boundary: no new saved full-output path is created; historical
artifact pointers may remain in resumed context; and no managed artifact read
root is granted. It no longer tells the model that a saved path categorically
does not exist.

This wording matches the existing machine receipts and storage behavior:
`existing_artifacts_unchanged` remains true,
`model_artifact_read_root_granted` remains false, and a historical pointer can
still be useful after a later explicitly authorized run without being usable
in the artifact-free run itself.

## Deterministic regression coverage

The CLI suite now exercises both Cycle 32 paths without relying on a local
provider:

- A default durable run offloads a real oversized file result into its session
  sidecar. The same session is then resumed with `--no-artifacts`. The test
  verifies the revised provider-visible instruction, the retained historical
  pointer, disabled-policy receipts, absence of a managed artifact read root,
  and a byte-for-byte identical sidecar inventory before and after resume.
- A single artifact-free run reads an oversized file and executes an exact
  pregranted command that emits 106,062 stdout bytes and 105,051 stderr bytes.
  The 32 KiB tail boundary of each stream begins inside a multibyte UTF-8
  scalar. Kitsu drains both streams, retains exactly 64 KiB per stream, reports
  exact original/retained/truncation counters, sends valid bounded JSON to the
  next provider request, and creates neither a global artifact generation nor
  a session sidecar.

The large-output integration uses an explicit 262,144-token test window so it
tests artifact-policy reduction rather than the independent context-compaction
layer. Existing core tests continue to cover the smaller 8 KiB generic bound,
background results, cancellation, and the default artifact-retaining policy.

## Verification

Formatting, published-schema parsing, staged and unstaged diff checks, all
**781** workspace tests, and warning-denied Clippy pass. The resulting Cycle 33
debug candidate is 154,331,912 bytes with SHA-256
`ec52af09c71f40f4aa66cd3e2484b264bd0ff56398ba5820b456630e0e976dd8`.
