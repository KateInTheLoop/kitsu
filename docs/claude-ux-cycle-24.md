# Claude UX cycle 24 — verified task effects

Date: 2026-08-14
Seed: `992624`
Boundary: Unix epoch `1786723200`

## Scored evidence

All workers pinned the same pre-change image before the boundary, SHA-256
`194763ad71d24e78334bd992e52548edc8ebf0d77570c7e8e060387812d5b417`.
They used only private copies after pinning; later shared-tree builds did not
affect the scored image.

- The nested-schema piped lifecycle scored **99/100**. Kitsu rejected the
  deliberately wrong enum at the correctly escaped path
  `$/result~1data~0id/status`, emitted the schema hash and keyword, incremented
  `output_schema_failures`, preserved the prior final-message destination, and
  reaped every child.
- The Hugging Face-shaped local lifecycle scored **100/100**. Managed login,
  valid structured commit, invalid structured discard, atomic output
  preservation, exact native response formats, logout, and all process bounds
  passed. Hosted HF remained `not_run: infrastructure`: neither the process nor
  user-service boolean gate contained a credential, so no remote request was
  sent.
- The seeded recover task exercised the opposite case. Kitsu's schema,
  response-format, local acceptance, receipts, counters, and `-o` mechanics all
  passed, but the local model made zero tool calls, returned schema-valid
  placeholders, left the corrupt task file unchanged, and exited 0. This
  repeated Cycle 23's schema-valid/no-effect model behavior.

The standalone Codex `0.147.0` comparator now initialized successfully with an
isolated writable state/cache namespace and reached `thread.started` and
`turn.started`. Its sole attempt could not reach the ChatGPT websocket or HTTPS
endpoint through the outer network sandbox, exhausted its own retries, and
exited without touching the fixture. That leg was
`not_scored: infrastructure` and was not retried.

## Selected improvement

Scripted and piped chat now accept repeatable `--require-write <PATH>` host
postconditions. The option never grants authority: each exact target must
already resolve inside an explicit `--allow-write` or `--allow-read-write`
root, otherwise Kitsu fails before provider contact.

For each target Kitsu records a bounded launch fingerprint, discloses the
resolved path to the model in a host-enforced system instruction, joins
foreground/background work, and then requires all three facts:

1. the write journal contains a verified model-filesystem mutation for the
   exact resolved path;
2. final contents differ from process launch; and
3. current identity, metadata, and contents still match the journal's latest
   descriptor-pinned post-write snapshot.

The fingerprint read uses the same descriptor-relative, no-follow path
machinery as atomic writes, including a final-symlink race regression. A
missing or reverted effect exits 1 even with `--allow-turn-errors`, emits a
typed `postcondition` error, increments the additive
`final_summary.postcondition_failures` subset of `user_failures`, and never
replaces `-o`. JSONL emits one `completion_postcondition` receipt when armed
and one after evaluation; satisfied evaluation is ordered before the
host-owned final-message installation.

## Regression coverage

Coverage proves exact journal verification, external post-write mutation
detection, final-symlink refusal, missing-authority preflight with zero provider
requests, successful piped write/fingerprint/receipt/output ordering, and the
original schema-valid zero-tool scripted failure with prior `-o` preservation.

The post-change gate passed formatting, JSON parse, `git diff --check`, all
**754** workspace tests, and warning-denied Clippy. The resulting debug binary
is 153,468,608 bytes with SHA-256
`19cd23ac362cf27d575e611f63c7075814199197e384ee89327a973d5ea250a0`.
