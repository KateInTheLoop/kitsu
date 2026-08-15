# Claude UX cycle 48 — truthful archived-session recovery

Date: 2026-08-14
Seed: `992648`
Boundary: Unix epoch `1786770000`

## Frozen release

All lanes independently gated the immutable post-Cycle-47 binary at
`target/cycle48-pin/kitsu`: SHA-256
`0070b33d546b04e2660c3073c0125ccf34dbbe6c3bf2405478d2118605d07d32`,
154,686,160 bytes, mode `0555`, source revision `cbd107c9d8a3`. The pinned
configuration was mode `0444` with SHA-256
`a7752d4264b2d8914ed51ed57486d30b3cf05d131f65aabd7c3bc3865961a297`.
Each lane froze its oracle before the boundary, launched exactly once from the
common pin, captured raw evidence before adjudication, reaped every child, and
made no shared-tree change or rerun.

## Evidence and attribution

- The provider-free reasoning-state lane passed for **100/100** in 9 ms. Four
  set/query/reset/query operations produced the exact 16-event JSONL lifecycle,
  `successful_operations: 4`, no failures, no session, and no provider, tool,
  assistant, or filesystem activity. Two raw scorer misses were harness-only:
  the product's stable reset label was the clearer `gateway default`, and the
  runtime inventory correctly remained equal to its precreated empty
  directories rather than literal `[]`. Existing-evidence adjudication made no
  rerun. Raw stdout SHA-256:
  `e27805872b3497f574ccf5aa2f1221d77a856bea9bbdd2e93a87faa73f9f7a05`;
  adjudication SHA-256:
  `7c540652a08286514051382c15bc1e12dba2f4962f256cbdd214701475cbb9cf`.
- The named archive/restore lane passed all frozen lifecycle assertions for
  **97/100**. One 603-byte durable snapshot stayed byte-identical across live,
  archive, and restore states (SHA-256
  `52e22b39970730a78f6370b74b4c34e8f1a4a1910ffb7b7ed2762d9b0dd72293`),
  retained its name and one-turn transcript, and became provider-free resumable
  again after restore. While archived, the live inventory was empty and name
  resume correctly failed. Its compatibility text then incorrectly recommended
  `/resume` for the archived session rather than the required restore action.
  Evidence SHA-256:
  `1794e9dd49d6c17683bdf25e3cc4490a80d479616f65ade6b79c08e6bd0b600f`.
- The artifact-free command/readiness lane passed for **100/100**. Its sole
  wrapper made exactly two authenticated local requests and one contained
  command. Both 140,505-byte streams were bounded to 65,536 bytes with head and
  tail retained, centers omitted, and no automatic artifact. The 38-byte final
  answer was committed atomically to a mode-`0600` host-owned output outside
  command authority; session, auth precedence/logout, redaction, containment,
  counters, sentinel, and reap checks all passed. Report SHA-256:
  `f7ae993fac38b0e8ebdfca5f65126ad17dfd0e6e6d97e8c8ad1dae6f5c9a4bfb`;
  raw JSONL SHA-256:
  `18162c6d944ed46a4a360388c6d1c60d92ade7fa9bdfa160011d4d716c629d18`.
  Hosted Hugging Face remained `not_run: infrastructure`: process, service,
  managed credential, and approved exact-pin route were absent and
  `request_sent=false`.

No Codex or Claude run was substituted without a genuinely analogous,
authorized provider-free lifecycle.

## Selected improvement

Archiving intentionally removes a snapshot from the live inventory, and both
exact-ID and name resume correctly refuse it until restoration. The bare
`/sessions` empty state nevertheless said to use `/resume` to continue an
archived session. This sent users directly back to the operation that had just
failed and contradicted the archive receipt's executable `/restore <id>`
recovery path.

The empty state now says to restore an archived snapshot with
`/restore <exact-id>`. README and functionality documentation clarify that
archived snapshots are absent from the live inventory and require
`kitsu sessions restore <exact-id>` (or the slash equivalent) before resume.
The JSONL protocol defines the same compatibility-text contract. No stored or
event schema changed.

## Regression and verification

The durable session-catalog regression now archives its sole named session,
requires provider-free resume-by-name to fail, and checks that an empty JSONL
`/sessions` result recommends `/restore <exact-id>` without mentioning
`/resume`. It then restores the exact ID, resolves the retained name, and
requires the complete `sessions show --json` snapshot to be byte-identical to
its pre-archive form. The focused formatter test enforces the same truthful
empty-state wording.

All **794** workspace tests pass (256 Kitsu unit, 2 provenance, 189 CLI,
100 core, 65 provider, 32 render, and 150 tool tests). Formatting,
warning-denied Clippy, schema parsing, and diff checks also pass.
