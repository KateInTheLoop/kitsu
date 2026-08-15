# Claude UX cycle 31 — artifact-free inference

Date: 2026-08-14
Seed: `992631`
Boundary: Unix epoch `1786735800`

## Scored evidence

All workers pinned the Cycle 30 candidate before preparing fixtures, SHA-256
`10793b5fa06a5cf5d7ace36e55f3be49dc2b5049de389a5784fcf7bbdd6d5d38`.
The host was nearly out of `/tmp` space, so workers removed only disposable
private binary copies from completed cycles and kept every fixture and evidence
record. They then released the shared target and used the one remaining pinned
image.

- The deterministic repeated-mismatch lifecycle scored **100/100**. Two exact
  SHA-256 mismatches each produced a typed, authority-free repair handoff; the
  third write matched, the tool-free structured finalizer succeeded, and `-o`
  installed atomically. Provider-free list/show and a real resume proved that
  provisional replies, repair instructions, digests, and the finalization
  nudge never entered the durable transcript.
- The Hugging Face-shaped lifecycle scored **100/100**. Its five-stage repair
  path and three-round exhaustion path both matched the transport and receipt
  oracle. The successful path installed exact structured output. The exhausted
  path retained the observed write, failed the postcondition, and preserved the
  prior `-o` destination. Hosted HF remained `not_run: infrastructure` because
  both credential gates were false; no remote request was sent.
- The randomized benchmark leg was **not scored**. Its sole Kitsu attempt
  reached the first provider request, but the outer harness denied access to
  the local Ollama endpoint before any model or tool activity. The fixture,
  output destination, and sentinel stayed byte-identical, every process was
  reaped, and the no-rerun rule was honored. This is infrastructure evidence,
  not a product result.

## Selected improvement

A read-only three-worker storage audit found two independent automatic artifact
writers: full command-output capture and oversized successful-tool offload.
Disabling only one leaves an incomplete privacy promise; disabling the offload
directory without another bound can also send an unbounded result into later
provider context.

Kitsu now exposes one immutable run policy through global `--no-artifacts` or
top-level `artifact_storage = false`. It suppresses only new, automatic
Kitsu-managed command-output and oversized tool-result files. It deliberately
does not suppress session snapshots, credentials, explicit `-o`, explicit
exports, schema inputs, required-write outputs, attachments, or
model-authorized task writes. The CLI disable wins over configuration, and an
unknown top-level configuration key fails closed so a misspelled privacy key
cannot silently restore storage.

Disabled runs do not construct a command artifact sink, allocate or rotate an
artifact generation, sweep old generations, grant a managed artifact read
root, or offload large tool results. Existing artifact-bearing sessions remain
read-only and unchanged. Durable runs may still save session JSON without a
sidecar; stateless runs do not prepare artifact state merely because commands
are enabled.

## Bounded inline evidence

Command execution continues to drain both pipes and preserve its native
head/tail previews, exact byte totals, retained counts, truncation flags,
completion state, exit/signal data, and timing. Generic and background tool
results use a deterministic UTF-8-safe 8 KiB head/tail representation whose
marker states original, retained, and omitted raw-byte counts. The bounded
representation is installed before another provider request or background
result is retained.

JSONL makes deliberate loss distinguishable from failure. `run_started`
includes the effective policy and source. `tool_finished` includes original and
retained byte counts, truncation, and an artifact status. Command receipts use
`suppressed_by_policy` when no full-output artifact was attempted; sink and
offload failures retain their separate failure states. Human startup,
`/status`, `/permissions`, `/context`, `/artifacts`, and doctor surfaces all
describe the same policy and explicitly say that existing artifacts and
explicit destinations are unaffected.

## Regression coverage

Core tests prove the UTF-8 boundary and byte-count invariant and show that an
oversized tool result is bounded before the next request without creating an
offload pointer. CLI integration proves that:

- a large command has truthful inline/truncation receipts and creates no
  artifact generation;
- a large generic tool result can enter a durable session without a sidecar;
- `artifact_storage = false` is reported by doctor; and
- default mode still retains the complete private command artifact and its
  recovery metadata.

Configuration tests cover the false value and misspelling rejection. Existing
session, cancellation, command-reaping, output-schema, required-write, `-o`,
artifact-management, and provider-error tests remain unchanged and green.

## Verification

Formatting, JSON-schema parsing, `git diff --check`, all **779** workspace tests,
and warning-denied Clippy pass. The resulting Cycle 32 debug binary is
154,331,912 bytes with SHA-256
`a9131ca0370a303d21ff003362dad1df73d0f871681c5a912f33ef865a89f036`.
