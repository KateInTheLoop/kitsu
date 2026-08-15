# Claude UX cycle 47 — reachable filtered session search

Date: 2026-08-14
Seed: `992647`
Boundary: Unix epoch `1786768200`

## Frozen release

All lanes independently gated the immutable post-Cycle-46 binary at
`target/cycle47-pin/kitsu`: SHA-256
`faf9baaaa62aad0751c15959b2dfbac45919f79519a86aa8467c0055c58070f5`,
154,686,240 bytes, mode `0555`, source revision `6b033779aba4`. The pinned
configuration was mode `0444` with SHA-256
`a7752d4264b2d8914ed51ed57486d30b3cf05d131f65aabd7c3bc3865961a297`.
The scored lanes used only the common pin, captured raw evidence before
adjudication, launched at most once after the boundary, reaped their process
groups, and made no shared-tree change or rerun.

## Evidence and attribution

- The provider-free empty-namespace lane discovered the regression for
  **79/100**. `/sessions 05cd03e4` reached `run_started` but produced a typed
  user error instead of the documented guided empty result. `/permissions`
  and `/quit` still completed, the final summary truthfully reported one
  failure and one successful operation, no provider or model event occurred,
  no private session state was created, and every process was reaped. Its
  secondary expectation that `--yes` reports deny rather than scoped mode was
  a harness error and did not cause the search failure. Evidence SHA-256:
  `043f125cdbf00a835ff194a372550d5810852d07a8b5b27d5305ee1235e1f9d1`.
- The context/session lane independently reproduced the same defect against a
  real one-turn named session for **91/100**. Top-level mixed-case name search,
  provider-free list/show/name resolution, the durable transcript, atomic
  `-o`, redaction, and cleanup all passed, but both scripted
  `/sessions <filter>` commands were rejected before the implemented catalog
  branch. The final summary was accurately `error` with two user failures and
  one successful operation. Evidence SHA-256:
  `b1d88f0a3b895d2a55d911def9e2188fe3be657d337fe69273d6f2544edcdd9f`;
  raw JSONL SHA-256:
  `717412cf608c271f7838b7b8a71f901160ff8569386d06c4e30eb29db2f95927`.
- The Hugging Face lane is `not_run: harness_interrupted`. Its launcher update
  was interrupted before a scored child was created, leaving attempt count
  zero, no report or raw capture, empty runtime/output directories, and no
  Kitsu or mock child. The credential-eligibility gate was not reached and
  `request_sent=false`; no local, hosted, model, provider, or product inference
  is drawn from that lane.

No Codex or Claude run was substituted without a genuinely analogous,
authorized provider-free session-search surface.

## Selected improvement

README already promised `/sessions [filter]`, and the runtime already carried
`ListSessions(String)`, case-insensitive filtering, a typed session-catalog
payload, current-session marking, and guided misses. A stale generic
no-argument guard nevertheless rejected every nonempty `/sessions` argument,
making that implementation unreachable from both human and JSONL scripts.
The slash-command registry also advertised the command as argument-free.

The guard now permits the existing filter path. Slash help advertises
`/sessions [filter]` and says that ID, name, and preview are searchable. README,
functionality, and JSONL protocol documentation use the same contract. No
session snapshot or JSONL event schema changed.

## Regression and verification

One provider-free regression starts with a fresh private namespace, runs a
filtered miss, and requires a successful typed empty catalog without creating
the session directory. A second end-to-end regression creates one named turn,
then proves mixed-case named hit and guided miss behavior in JSONL and human
modes, exact total/filtered counts, current-session identity and marker,
successful-operation accounting, provider-free execution, updated slash help,
and exclusion of query commands from the durable transcript. The existing
unit test now covers case-insensitive ID, name, and preview matching.

All **794** workspace tests pass (256 Kitsu unit, 2 provenance, 189 CLI,
100 core, 65 provider, 32 render, and 150 tool tests). Formatting,
warning-denied Clippy, and diff checks also pass.
