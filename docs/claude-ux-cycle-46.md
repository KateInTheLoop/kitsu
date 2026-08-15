# Claude UX cycle 46 — exact session-fork lineage

Date: 2026-08-14
Seed: `992646`
Boundary: Unix epoch `1786766400`

## Frozen release

All lanes gated the immutable post-Cycle-45 binary at
`target/cycle46-pin/kitsu`: SHA-256
`f61757e3657934da0872689f160ca0c7e35d8a65de3efd2b194bcb9c805dab87`,
154,681,280 bytes, mode `0555`, source revision `bd86a94ceea1`. The pinned
configuration was mode `0444` with SHA-256
`a7752d4264b2d8914ed51ed57486d30b3cf05d131f65aabd7c3bc3865961a297`.
Supported lanes used the common pin only, captured raw evidence before
adjudication, launched at most once after the boundary, reaped their process
groups, and made no shared-tree change or rerun.

## Evidence and attribution

- The corrected provider-free recovery lane passed for **99/100** in 13 ms.
  `/tools` completed, one seeded unknown command produced an exact typed user
  failure, and `/permissions` plus `/status` continued successfully. The final
  summary was partial with one user failure and three successful operations.
  `--allow-path` exposed exactly the three read tools, no write or agent schema,
  and one read root; no provider, model, tool execution, artifact, session, or
  mutation occurred. Its evidence-backed secondary gap is that the enforced
  zero subagent cap is not directly present in `/permissions`. Evidence
  SHA-256:
  `1a14629a0289dcb298500c45482a232c673dc4648a61bfeca3461b174fc9db21`.
- The context/session lane completed source creation, `fork last`, and named
  source resume for **99/100**. The source ended with two clean durable turns;
  the unnamed fork retained exactly the first turn. Both IDs resolved
  provider-free, all model-request postfix checks passed, sentinels and work
  stayed exact, and every process was reaped. The successful JSON fork receipt
  named only the new session, not the exact source to which `last` resolved.
  Evidence SHA-256:
  `675575c429b984ac0bd2b52c6637f453ebeb845ec138b6cc96641b94fbb3ce12`.
- The Hugging Face lane is `not_run: harness_interrupted`. Setup was
  interrupted before the launcher was frozen or invoked, so scored attempts
  remained zero and no report or raw JSONL exists. Process inspection found no
  Kitsu or mock child, its runtime/output directories were empty, and
  `request_sent=false`; neither credential eligibility nor local/hosted
  behavior was exercised. No product, model, or provider inference is drawn.

No standalone comparator was substituted without an honest isolated analogue.

## Selected improvement

`sessions --json fork last` previously returned the new snapshot and
`permissions_restored: false`, but omitted the exact ID resolved from `last`.
Automation had to resolve and retain that selector out of band to establish
lineage, despite the fork itself already reading the exact source snapshot.

The session store now returns a committed fork receipt containing both the
resolved source ID and the new saved session. Top-level JSON adds
`source_session_id` while preserving every existing field. Human and
interactive receipts render `resolved-source-id -> fork-id`; permission grants
remain uncopied, the source retains its unique name, and the fork remains
unnamed. The durable snapshot schema is unchanged.

## Regression and verification

The core regression forks `last` and requires the exact source identity before
checking fresh ID, transcript, name, project-context, file mode, and provenance
preservation. The provider-backed CLI lifecycle now forks a uniquely known
source through `last`, asserts exact JSON lineage, uncopied permissions, source
name retention, unnamed fork, and subsequent branch divergence. A second
lifecycle covers exact-ID JSON lineage, and human output must include the
resolved source-to-fork pair. Existing artifact-copy and symlink-failure tests
continue through the lineage-aware API. README and functionality documentation
define the additive receipt.

All **792** workspace tests pass (256 Kitsu unit, 2 provenance, 187 CLI,
100 core, 65 provider, 32 render, and 150 tool tests). Formatting,
warning-denied Clippy, and diff checks also pass.
