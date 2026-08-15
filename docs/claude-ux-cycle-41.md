# Claude UX cycle 41 — complete JSONL operation counts

Date: 2026-08-14
Seed: `992641`
Boundary: Unix epoch `1786757400`

## Scored evidence

All three lanes gated the same immutable post-Cycle-40 binary: SHA-256
`f9f07318ffa36e7d6e3c62fb2fc59d392d16e5af8426072b2e527cbbf33877d6`,
154,675,576 bytes, mode `0555`, source revision `cb50766c5400`. The pinned
configuration was mode `0444` with SHA-256
`a7752d4264b2d8914ed51ed57486d30b3cf05d131f65aabd7c3bc3865961a297`.

- The provider-free randomized lane ran four strict and accepted-partial
  human/JSONL children covering both closest-command and `/help` recovery.
  Every exact message, source line, event order, counter, output-preservation,
  zero-effect, and process-reap assertion passed. It scored **98/100** because
  neither strict nor partial `final_summary` exposed the human receipt's exact
  successful-operation count. Evidence SHA-256:
  `b9268e53b88d96151b6a9e31da0f0e1760d851e6a3cee9208cc9492b0057f8d0`.
- The context/session lane committed one tool-free model turn, then accepted a
  seeded unknown command and clean `/quit`. The exact actionable error,
  resumable one-turn session, atomic output, empty worktree, redaction, and
  complete reaping all passed. Its terminal summary was truthful but omitted
  the exact count, for **99/100**. Evidence SHA-256:
  `18f2a0c804f6664066e26444e5a328c5406b8118edfad4b2e0ae39dc217280e2`.
- The Hugging Face-shaped lane verified the same mixed lifecycle through an
  owned authenticated local adapter, including bounded artifact-free reads,
  two resumable sessions, exact output, auth precedence/logout, and full
  reaping. Hosted HF remained `not_run` infrastructure because process,
  user-service, and managed-credential gates were false and no remote request
  was sent. It scored **96/100** solely for the same missing summary field.
  Evidence SHA-256:
  `fced05c23885758c2249c527158fd00e2877fc2cf878ae1f54d4f4b7d886109c`.

No standalone Codex or Claude score was substituted: neither exposed an
honest provider-free equivalent for Kitsu's mixed scripted lifecycle.

## Selected improvement

The Cycle-40 actionable-error postfix passed every scored branch. All three
lanes then independently found that `final_summary` omitted the exact number
of successful operations. Replaying prior events is not equivalent: `/quit`
has an `ok` command frame but is control flow and deliberately does not count
as a successful operation in the human receipt.

New JSONL summaries include additive `successful_operations`. Scripted runs
publish their existing authoritative semantic count. Catalog and ask/one-shot
runs record one only after a successful catalog result or committed assistant
disposition. A committed operation remains counted if a later session,
completion-postcondition, or `-o` installation step fails. Older v1 streams
without the optional field mean that the count is unknown, never zero.

## Regression and verification

Regression coverage now proves zero for an all-failure script; one for a
`/status` + typo + `/quit` partial script; two for a query plus model turn; one
for catalog and ask success; and one when a committed answer is followed by a
required-write or output-install failure. The published schema accepts the
optional non-negative integer, while README and protocol documentation define
its semantic and compatibility rules.

All **790** workspace tests pass (256 Kitsu unit, 2 provenance, 185 CLI,
100 core, 65 provider, 32 render, and 150 tool tests). Formatting,
warning-denied Clippy, published-schema parsing, and diff checks also pass.
