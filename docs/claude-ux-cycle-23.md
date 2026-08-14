# Claude UX cycle 23 — structured-output acceptance

Date: 2026-08-14
Seed: `992611`
Boundary: Unix epoch `1786721400`

## Scored evidence

All three workers launched one hash-gated lifecycle from private copies of the
same post-Cycle-22 binary, SHA-256
`62e241644e19a535ba8fef2689b324a5f08c25a5220e65a8ed9e92c08b86cb03`.
Every Kitsu child and mock was bounded and reaped; no worker used the shared
development binary after pinning.

- The deliberate schema-mismatch ask scored **98/100**. Kitsu sent the exact
  native response format, independently rejected valid JSON that violated a
  seeded `const`, discarded it with exit 2, emitted no final-message receipt,
  and preserved the prior mode-`0600` destination byte-for-byte.
- The Hugging Face-shaped lifecycle scored **98/100**. Managed login, native
  structured valid-output commit, invalid-output discard/preservation, logout,
  exact authorization, and process cleanup all passed. Hosted HF remained
  `not_run: infrastructure` because both boolean credential gates were false;
  no remote request was attempted.
- The seeded create task scored **76/100**. All Kitsu schema, receipt, local
  validation, isolation, and atomic-output mechanics passed, but the local
  model made no tool calls, invented schema-valid state, and left the required
  task artifact absent. This was model behavior: a structural schema cannot by
  itself prove that claimed external state exists.

A standalone Codex `0.147.0` binary was discovered during the third leg. Its
single comparator attempt failed before provider contact because the outer
sandbox made its in-process app-server state read-only; the fixture and output
remained unchanged. That leg was `not_scored: harness/infrastructure` and was
not retried.

## Selected improvement

The two successful failure-oracle legs independently exposed an observability
gap. The typed event said `kind: "output_schema"`, but its mismatch location and
keyword existed only in prose, while a `final_summary`-only consumer saw the
failure folded into `provider_failures`.

Kitsu now emits three additive fields on structured-output error events:

- `schema_sha256` identifies the exact preflighted contract;
- `instance_path` identifies the failing JSON value (`$` is the root); and
- `validation_keyword` names the failed constraint, such as `const` or
  `json_syntax`.

Every final summary also emits `output_schema_failures`. It is explicitly a
subset of `provider_failures`, retaining the existing v1 outcome and exit-code
classification while letting summary-only consumers distinguish successful
provider transport followed by local acceptance failure. All new protocol
fields remain optional in the published v1 schema so historical streams stay
valid.

The local validator also now applies JSON Schema numeric equality to `const`,
`enum`, and `uniqueItems` without collapsing large adjacent integers, checks
annotation types and duplicate enum values during preflight, restricts local
references to validated schema locations, and escapes object member names in
reported instance paths.

## Regression coverage

Coverage exercises a successful structured commit with a zero summary counter,
a valid-JSON `const` mismatch with exact typed fields, malformed JSON with
`json_syntax`, scripted retained filesystem effects, unchanged `-o` output,
historical-schema compatibility, numeric equality, and unsafe schema preflight.

The post-change gate passed `cargo fmt --all -- --check`, JSON-schema parse,
`git diff --check`, all **749** workspace tests, and warning-denied Clippy. The
resulting debug binary is 153,221,000 bytes with SHA-256
`bff0607e2a5d3667ee1a13ecb7d24f70d679237a81da842ffbf03c04a682a712`.
