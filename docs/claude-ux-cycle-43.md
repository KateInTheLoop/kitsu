# Claude UX cycle 43 — explicit postcondition output preservation

Date: 2026-08-14
Seed: `992643`
Boundary: Unix epoch `1786761000`

## Scored evidence

All three lanes gated the same immutable post-Cycle-42 binary: SHA-256
`71d3166f270fb627c68078c9e303284b2dc4f400e13873cee3abf287dc1e9b40`,
154,677,240 bytes, mode `0555`, source revision `cce10d164a15`. The pinned
configuration was mode `0444` with SHA-256
`a7752d4264b2d8914ed51ed57486d30b3cf05d131f65aabd7c3bc3865961a297`.
Every lane used the common pin only, launched once after the boundary, reaped
its process group, and made no shared-tree change or rerun.

- The corrected provider-free lane granted an exact parent read/write root for
  its existing required target. `/permissions` and `/status` completed, `/quit`
  remained excluded, and the later required-write failure ended with
  `successful_operations: 2`, `postcondition_failures: 1`, and exit `1`.
  Target, preexisting `-o`, sentinel, and inventories remained exact; stderr
  was empty and there was no provider/model/tool activity. It scored
  **100/100**. Evidence SHA-256:
  `838bd593f0d41b1c1b5d6c3c1bf912bece451aaee0744a0bcf44b3f65806e37a`.
- The context/session lane committed one model turn, completed `/permissions`,
  excluded `/exit`, and then failed the unmet write postcondition. The summary
  retained `successful_operations: 2`; the named one-turn session remained
  resumable, while the required target and preexisting `-o` retained exact
  inode, mode, bytes, and hashes. Its product score was **99/100** because the
  summary did not state why the requested output was preserved. One stored
  harness check separately used internal-looking tool labels rather than the
  actual public schema names; raw authority evidence passed and no product
  deduction was assigned to that mismatch. Evidence SHA-256:
  `420642b223431594dd1317900e95fb4d814e42aff7e724564cc55363f233d809`.
- The Hugging Face-shaped lane committed one bounded model operation, then
  failed the missing required write. It ended with
  `successful_operations: 1`, a typed postcondition error, exit `1`, and a
  resumable session; the target and preexisting output were unchanged. Auth,
  bounded artifact-free reads, redaction, integrity, and process reaping all
  passed for **100/100**. Hosted HF remained unavailable infrastructure because
  every credential gate was false and `request_sent` remained false. Evidence
  SHA-256:
  `993d7221553205b5ff3dc03d0d17b35527a9ddbef0b0b87542c22372a5c6075e`.

No standalone comparator was substituted without an honest isolated analogue.

## Selected improvement

Cycle 43 confirmed the successful-operation count across the corrected
postcondition edge. It also exposed a narrower summary-only ambiguity. With
`-o` requested and a committed assistant available, the failed required-write
gate correctly prevented installation and preserved the destination, but
`final_summary.output_last_message_state` was absent. Automation had to replay
postcondition events and remember the original CLI invocation to explain the
missing output receipt.

JSONL now emits the additive state `"preserved_postcondition_failure"` when a
requested output is withheld by a failed completion postcondition after a
committed assistant. The existing `"preserved_no_committed_assistant"` state
retains priority when no full commit exists. Successful installation and runs
without `-o` still omit the field. Older v1 absence means that the preservation
reason is unknown.

## Regression and verification

The preexisting-destination regression now requires exact preservation plus
the new state. A focused companion proves that an absent destination remains
absent with the same state, while an otherwise identical invocation without
`-o` emits no output-state field. Existing no-commit and successful-install
tests guard compatibility. The published schema now accepts both additive
states, and README/protocol documentation defines the preservation and older
consumer contract.

All **791** workspace tests pass (256 Kitsu unit, 2 provenance, 186 CLI,
100 core, 65 provider, 32 render, and 150 tool tests). Formatting,
warning-denied Clippy, published-schema parsing, and diff checks also pass.
