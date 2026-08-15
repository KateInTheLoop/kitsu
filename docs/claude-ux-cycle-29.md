# Claude UX cycle 29 — canonical structured-finalization history

Date: 2026-08-14
Seed: `992629`
Boundary: Unix epoch `1786732200`

## Scored evidence

All three workers pinned the Cycle 28 candidate before preparing their fixtures,
SHA-256
`f564998fc8e2cd525f332858b9e14eb9bb50a1414668a11a1805c2e5835d3ac1`.
Every scored Kitsu leg used only its private pin and released the shared target.

- The Hugging Face-shaped lifecycle scored **100/100**. Missing auth failed
  before transport, managed auth remained private, the exact-digest success and
  mismatch cases produced truthful receipts, wrong output preserved `-o`, all
  children were reaped, and no secret or sentinel changed. Hosted HF remained
  `not_run: infrastructure` because the process and user-service credential
  gates were both false; no remote request was sent.
- The randomized edit task scored **90/100**. The model produced semantically
  correct JSON but retained one trailing newline: actual SHA-256
  `2c0dc4ed…0982` versus exact oracle `b4954bd1…eedb`. Kitsu correctly reported
  `expected_sha256_matches: false`, exited `1`, retained the diagnostic artifact,
  and preserved the old final-message sink. This independently validates that
  Cycle 28's exact raw-byte acceptance contract fails closed on a one-byte miss.
- The deterministic two-target task scored **94/100**. One exact artifact
  matched and the second contained a newline instead of being empty; Kitsu's
  postcondition result, counters, nonzero exit, retained effects, and `-o`
  preservation all matched the oracle. Provider-free inspection then found that
  its single submitted script line was listed as two durable turns: the snapshot
  contained the provisional action assistant and Kitsu's synthetic
  `The tool-action phase is complete…` user message in addition to the real tool
  transcript and final assistant.

The sole standalone Codex `0.147.0` control reached its bounded transport path,
but the outer environment denied both WSS and HTTPS. It ended in `turn.failed`
without model/tool activity or fixture mutation and was
`not_scored: infrastructure`; there was no retry or substitute.

## Selected improvement

Deferred structured finalization now treats its provisional assistant and
host-authored finalization nudge as provider-request-only scaffolding. The final
provider request still receives both messages. After that request has captured
its context, Kitsu removes only those two internal messages, invalidates the
context-meter anchor that described the larger request, and appends the final
assistant to canonical conversation history.

Real action-phase assistant tool calls and tool results remain in memory and in
durable snapshots. Successful sessions and resumable sessions whose run later
fails an exact-write postcondition therefore preserve useful recovery evidence,
but one submitted prompt is again exactly one user turn. Historical snapshots
are not rewritten and `SessionStore` does no content-based filtering.

## Regression coverage

The core two-phase loop regression now proves all of these simultaneously:

- the final provider request sees both the provisional assistant and host nudge;
- the action tool call and correlated tool result remain in canonical history;
- neither internal handoff message remains after the final response;
- the committed final assistant is the terminal history message; and
- removing request-local scaffolding invalidates the stale usage anchor.

CLI integration coverage uses provider-free `sessions --json` and
`sessions --json show` after both a successful structured required-write run and
a schema-valid run whose exact-write postcondition fails. Both list exactly one
turn, exclude the two internal messages, retain the tool transcript and final
assistant, and preserve the existing `-o` failure contract.

Documentation now states that structured-finalization scaffolding is never
saved, restored, or counted as another user turn. The dogfood protocol requires
future durable combined-schema fixtures to verify that invariant directly.

## Verification

Formatting, JSON-schema parsing, `git diff --check`, all **768** workspace tests,
and warning-denied Clippy pass. The resulting Cycle 30 debug binary is
153,767,456 bytes with SHA-256
`8f3a5f3755c8b48fa986f3de3f8180697c8bf36b321567e6bd6ff3a9f7f246bc`.
