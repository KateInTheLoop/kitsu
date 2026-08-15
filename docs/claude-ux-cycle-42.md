# Claude UX cycle 42 — successful-operation postfix

Date: 2026-08-14
Seed: `992642`
Boundary: Unix epoch `1786759200`

## Frozen release

All lanes gated the immutable post-Cycle-41 binary at
`target/cycle42-pin/kitsu`: SHA-256
`cadcde7cf453cb6ca2f3df3fdaa4ce3691d21e56f93081f765afff1813288b0c`,
154,677,240 bytes, mode `0555`, source revision `c7f4d2433683`. The pinned
configuration was mode `0444` with SHA-256
`a7752d4264b2d8914ed51ed57486d30b3cf05d131f65aabd7c3bc3865961a297`.
Every scored or attempted wrapper launched at or after the boundary, used the
common pin only, and made no shared-tree change or rerun.

## Evidence and attribution

- The provider-free matrix's two eligible controls passed. A total-failure
  script exited `1` with `successful_operations: 0`; an accepted-partial
  script exited `0` with `successful_operations: 1` even though both `/status`
  and control-flow `/quit` had `ok` command frames. The intended later
  postcondition case was not product-scored: its harness granted write-only
  authority for an existing required target, so Kitsu correctly failed closed
  before `run_started` with exact read/write-authority guidance. All three
  children were reaped and every protected file remained unchanged. The two
  covered controls scored **100/100**; the matrix as a whole is
  `not_scored: harness_authority_misconfiguration`. Evidence SHA-256:
  `7ce71bfcc0586c9dc1b0e2ae7bd5b6a9227f5fcce25a60d7a9fcd809f2bbc970`.
- The context/session wrapper stopped before a Kitsu attempt. Its sealed
  preexisting-output fixture contained two trailing line feeds while the
  runner oracle expected one; the runner therefore exited `70` without an
  attempt marker, mock, provider request, capture, session, or work effect.
  This is `not_scored: harness_fixture_mismatch`, with no rerun. Evidence
  SHA-256:
  `491c753b0af267da78c72fcb3c4752e8201c43dee09ef5e006c81422a0e203d8`.
- The Hugging Face-shaped lane passed **100/100**. A committed model operation
  followed by an accepted `/statsu` failure ended `partial` with
  `failures: 1`, `user_failures: 1`, and `successful_operations: 1`; the human
  receipt independently reported one success and one failure. Exact actionable
  recovery, event order, empty JSONL stderr, atomic outputs, two resumable
  sessions, bounded artifact-free reads, managed/environment auth precedence,
  logout, redaction, integrity, and process reaping all passed. Real hosted HF
  remained unavailable infrastructure because every credential gate was false
  and `request_sent` remained false. Evidence SHA-256:
  `9533bb661aa9a730964f0619c7db41d39716788372c6055b76e5aaef6df7562c`.

No standalone comparator was substituted without an honest isolated analogue.

## Disposition

The exercised `successful_operations` postfix is correct, including the
important committed-operation-then-later-failure edge. Neither invalid harness
leg supplies product evidence, so this cycle makes no runtime change and does
not invent a Kitsu defect from a fixture or authority mistake. The unexercised
postcondition edge moves to the next frozen cycle with an existing target under
an exact read/write root (or an absent target under write-only authority).
