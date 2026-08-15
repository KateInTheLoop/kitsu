# Claude UX cycle 44 — output-preservation postfix

Date: 2026-08-14
Seed: `992644`
Boundary: Unix epoch `1786762800`

## Frozen release

All lanes gated the immutable post-Cycle-43 binary at
`target/cycle44-pin/kitsu`: SHA-256
`f29e7d1ab3ed82e50e0a87783d38d60a6f626ddd365f8f015f81fd40e34ec145`,
154,677,984 bytes, mode `0555`, source revision `1c1d0eba2bd7`. The pinned
configuration was mode `0444` with SHA-256
`a7752d4264b2d8914ed51ed57486d30b3cf05d131f65aabd7c3bc3865961a297`.
Every supported leg launched once after the boundary, used the common pin only,
captured before adjudication, reaped its process group, and made no shared-tree
change or rerun.

## Evidence and attribution

- The provider-free lane verified state priority with an existing destination.
  `/status` completed and `/quit` remained control flow; the later unmet write
  postcondition ended with `successful_operations: 1` and
  `output_last_message_state: "preserved_no_committed_assistant"`. Target,
  output, sentinel, and inventory were exact; stderr was empty and there was no
  provider/model/tool activity. The supported leg scored **100/100**. Two
  committed-assistant variants were explicitly
  `not_run: sandbox_loopback_infrastructure` after socket creation was denied
  before any attempt; no approval, listener probe, binary substitution, or
  product claim was made. Evidence SHA-256:
  `92de024506ab5c280ca6de4d2dad449bceaed4ed35a80d38b03ac07c250d7ee6`.
- The context/session lane covered the absent-path branch. One model turn and
  `/status` completed, `/exit` was excluded, then the absent required target
  failed its postcondition. The absent `-o` remained absent and the terminal
  state was `"preserved_postcondition_failure"`; the one-turn named session
  remained resumable. All 32 authority, event-order, counter, redaction,
  inode/hash, and reap assertions passed for **100/100**. Evidence SHA-256:
  `03268fcab465b799f4356c787b290607030bbb1719903d13864412fb5b2a355a`.
- The Hugging Face-shaped lane covered a preexisting destination after a
  committed model turn. It produced the same new state, exact postcondition
  counters, no installation receipt, and unchanged required target/output,
  while retaining a resumable bounded session. Auth lifecycle, artifact
  suppression, privacy, integrity, and reaping passed for **100/100**. Real
  hosted HF remained unavailable infrastructure because every credential gate
  was false and `request_sent` remained false. Evidence SHA-256:
  `c5284d63b8c89d3c02b900849b3ba7ca0be2e0a1561391d52496c533f7959e7a`.

No standalone comparator was substituted without an honest isolated analogue.

## Disposition

The postfix passes for preexisting and absent destinations, committed and
uncommitted runs, semantic operation counting, session durability, event
ordering, counters, and filesystem preservation. No evidence-backed runtime
gap remains in this lifecycle, so Cycle 44 makes no product change. The next
cycle returns to varied randomized discovery rather than extending this surface
speculatively.
