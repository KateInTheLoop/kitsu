# Claude UX cycle 22 — structured final output

Date: 2026-08-14
Seed: `992610`
Boundary: Unix epoch `1786719600`

## Scored evidence

All three workers used the same private pre-change binary, SHA-256
`e35ca6a6b59f00501ceb2d8d34fd1c91966a42a906eb7ce829600fcfa4c02d3a`.
Each launched once after the boundary with predeclared authority and reaped all
children. No worker used or modified the shared development target.

- The ask/final-message lifecycle scored Kitsu **100/100**. Its exact 28-byte
  UTF-8 answer was atomically installed mode `0600`; the body-free receipt,
  commit disposition, byte count, SHA-256, and event ordering all agreed.
- The Hugging Face-shaped local lifecycle scored **97/100**. Managed auth,
  one authenticated tool-free request, exact 49-byte JSON, atomic final-message
  output, logout, and failure preservation all passed. Hosted HF was
  `not_run: infrastructure` because both boolean credential gates were false;
  no remote request was sent.
- The seeded create task scored Kitsu **71/100** and the controlled comparator
  **95/100 provisional**. Kitsu's `-o` contract passed, but the model committed
  109 bytes of fenced pretty JSON, never created the requested task file, and
  still exited 0. The pre-change parser rejected `--output-schema` as unknown,
  so Kitsu could not distinguish plausible prose from valid machine output.

The common product gap was therefore structured-output acceptance, not final
message capture. The comparator was provisional because no standalone `codex`
binary was installed.

## Selected improvement

Kitsu now accepts `--output-schema <PATH>` on non-interactive ask, review,
piped chat, and scripted chat. The implementation is deliberately two-layered:

1. Before setup or provider contact, Kitsu descriptor-reads one regular,
   non-symlink schema file (maximum 64 KiB), validates its supported strict
   object shape, and records the exact source byte count and SHA-256.
2. Each main inference request sends the schema as OpenAI-compatible
   `response_format.type = "json_schema"` with `strict: true`. Compaction and
   child-agent requests do not inherit it.
3. Kitsu independently parses and validates the exact terminal assistant text
   before its commit disposition. Markdown fences, malformed JSON, or a schema
   mismatch are discarded as typed `output_schema` errors with exit 2.
4. Because validation precedes commit, invalid output cannot enter durable
   conversation state or replace `-o`; completed tool filesystem effects remain
   reported under the existing rollback contract.

The schema source is host-owned input. It is protected from model filesystem
tools, rejected when visible through a writable command root or read-only
runtime root, and never exposed by path or body in JSONL. A new
`output_schema` preflight receipt carries its byte count/hash and enforcement
facts; `provider_request.structured_output` identifies constrained requests.
The exact response-format wrapper is included in context-budget checks.

## Regression coverage

Coverage includes native wire serialization, core request/event forwarding,
schema-byte context budgeting, supported validation and local references,
bounded source reads, symlink rejection, invalid-schema no-request behavior,
valid commit plus atomic `-o`, fenced-output discard plus destination
preservation, retained-and-reported tool effects on schema rejection,
filesystem-tool protection, and command-root rejection.

The post-change gate passed `cargo fmt --all -- --check`, JSON-schema parse,
`git diff --check`, all **749** workspace tests, and warning-denied Clippy.
The resulting debug binary is 153,203,152 bytes with SHA-256
`ded7d71588f90c860959457776f4738a44c46dc0e3e1202ff0a482b062f35769`.
