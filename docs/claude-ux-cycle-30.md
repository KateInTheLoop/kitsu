# Claude UX cycle 30 — clean resume and pre-finalization exact repair

Date: 2026-08-14
Seed: `992630`
Boundary: Unix epoch `1786734000`

## Scored evidence

All three workers pinned the Cycle 29 candidate before fixture preparation,
SHA-256
`8f3a5f3755c8b48fa986f3de3f8180697c8bf36b321567e6bd6ff3a9f7f246bc`.
They then released the shared target and scored only their private copies.

- The Hugging Face-shaped lifecycle scored **100/100**. The exact-digest
  mismatch took three requests with the intended action/action/final phase
  vector, failed closed, retained the observed write, preserved `-o`, and left
  one resumable session with one submitted turn and no provider-only handoff
  messages. Provider-free list/show/resume made no model requests. Hosted HF was
  `not_run: infrastructure`: both credential gates were false and no remote
  request was attempted.
- The deterministic failure-then-resume lifecycle scored **100/100**. The
  failed postcondition snapshot contained only the original prompt, real tool
  call/result, and committed structured answer. A later real resume request
  received that clean context plus its new prompt, produced the expected
  answer, and advanced the same session from one to two turns. Provisional
  action text and both host handoff messages were absent before and after
  resume.
- The randomized line edit scored **90/100**. The model made the requested
  semantic edit but retained one trailing newline. Kitsu compared actual
  `fe8196b1…49fd` with expected `50df8875…a71b`, emitted the typed exact-write
  postcondition failure, withheld `-o`, and left one clean resumable turn. Its
  final provider request had the expected two-message provisional/nudge suffix;
  the durable session had neither. A slash-only resumed `/context` query made
  no provider request and did not mutate the transcript.

The sole standalone Codex `0.147.0` control reached its bounded transport path,
but the outer environment denied WSS and HTTPS. It ended in `turn.failed`
without model/tool activity, fixture mutation, retry, or substitute and is
classified `not_scored: infrastructure`.

## Selected improvement

Cycle 30 repeatedly exposed a narrower acceptance-efficiency problem: Kitsu
already knew from descriptor-pinned host evidence that the model's latest write
missed an exact SHA-256, but still spent the next tool-free strict-schema round
before rejecting the run. The selected change adds a synchronous host-owned
readiness gate to deferred structured finalization.

When all ordinary mutation invariants pass and only an exact digest is wrong,
Kitsu keeps the existing tools available for one bounded correction request if
both that request and a later finalizer still fit `--max-rounds`. The correction
contains only resolved path plus expected/actual SHA-256 facts already covered
by the completion contract, grants no new authority, and is removed from
canonical history after the provider request captures it. A typed
`structured_finalization_deferred` event records the decision before the repair
request. Missing writes, unverifiable state, and exhausted budgets keep the
existing fail-closed finalization/postcondition behavior.

The write journal's descriptor-pinned verifier now includes the currently
active attributed turn as well as finalized history. This makes mid-turn
readiness evidence available without a second pathname open while preserving
the same terminal verifier and tamper detection.

## Regression coverage

Core loop tests prove that:

- an exact mismatch produces one typed deferral and another tool-capable,
  schema-free request;
- the final request remains tool-free and schema-constrained;
- repair and finalization scaffolding is absent from canonical history while
  real tool calls/results remain;
- usage anchors are invalidated after request-local context removal; and
- the gate is not consulted when doing so would consume the only reserved
  finalizer round.

CLI integration creates a wrong exact artifact, observes the typed mismatch,
performs a conditional correcting write, then successfully finalizes and
atomically installs `-o`. It verifies the five-request phase vector, exact event
fields/order, expected and actual digests, descriptor-verified final match, and
a durable one-turn transcript that contains both real writes but none of the
three host/provisional scaffolding messages. The existing exact-mismatch durable
test now pins a three-round budget to cover the exhaustion path explicitly.

The published JSONL schema defines the additive event and excludes it from the
forward-compatible catch-all. README, functionality, protocol, and dogfood
rotation documentation describe its ordering, authority, durability, and
bounded-budget semantics.

## Verification

Formatting, JSON-schema parsing, `git diff --check`, all **773** workspace tests,
and warning-denied Clippy pass. The resulting Cycle 31 debug binary is
154,383,120 bytes with SHA-256
`10793b5fa06a5cf5d7ace36e55f3be49dc2b5049de389a5784fcf7bbdd6d5d38`.
