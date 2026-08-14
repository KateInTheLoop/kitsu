# Claude UX cycle 28 — exact artifact acceptance

Date: 2026-08-14
Seed: `992628`
Boundary: Unix epoch `1786730400`

## Scored evidence

All three workers pinned the same Cycle 27 image before preparing their
fixtures, SHA-256
`8d752d9ba0510720c410af2cfda4a296a2c41d2cb8c21bc4e8174823ae42beb0`.
Every scored Kitsu leg used only its private pin and released the shared target.

- The deterministic wrong-content lifecycle scored **70/100**. The model made
  one verified write and produced a schema-valid final answer, but the resulting
  30-byte file had SHA-256
  `09cbfb654429d9559bc63e7a22d149b63c5f72f9a58a2fea725b2b472b61a311`
  rather than the immutable 22-byte oracle
  `0343571aec1964602c0a7904992a4ffe66875cbd2629f710d5a2de086a1b455d`.
  Kitsu truthfully reported the journaled net mutation, marked the existing
  postcondition satisfied, committed the answer, installed `-o`, and exited
  zero. Provider-free help confirmed there was no exact hash/content assertion.
- The randomized edit task scored **80/100**. Its request phase vector remained
  correct: three unconstrained tool-capable action rounds followed by one
  tool-free structured finalizer. The model read and edited the target, but
  substituted literal field names for required values and left a forbidden
  trailing newline. The observed artifact SHA `11a4c14b…9494` did not match
  oracle `7697001d…7ffa`; mutation-only `--require-write` nevertheless reported
  success and installed the committed final message. This independently
  reproduced the same acceptance gap on a different task.
- The Hugging Face-shaped local lifecycle scored **100/100**. The Cycle 27
  tool-free-finalization diagnostic passed exactly: an illegal final tool call
  executed zero times, the action effect remained, `-o` stayed unchanged, and
  the typed cause/message/counters agreed. Hosted HF remained
  `not_run: infrastructure` because both user-controlled credential gates were
  false and no remote request was sent.

The sole standalone Codex `0.147.0` control reached its bounded network path but
the outer environment denied WSS and HTTPS. It ended in `turn.failed` without a
model response, tool call, or fixture mutation and was
`not_scored: infrastructure`; there was no retry or substitute comparator.

## Selected improvement

Scripted and piped chat now accept repeatable
`--require-write-sha256 <PATH=64HEX>`. The option implies the existing
host-verified model-write, net-change, latest-write identity/content, authority,
and existing-target read contract, then requires the exact final raw bytes to
match the supplied SHA-256. It performs no newline, encoding, JSON, or text
normalization and grants no filesystem authority.

The parser splits on the final `=` so paths may contain `=`, accepts ASCII hex
case-insensitively, and normalizes it lowercase. Malformed input, conflicting
expectations for one canonical target, and an existing launch artifact already
equal to its expected digest fail before `run_started` or provider contact. A
digest mismatch remains the compatible typed postcondition/user failure: exit
`1`, `postcondition_failures: 1`, retained filesystem evidence, and no `-o`
replacement. It is not waived by `--allow-turn-errors`; a previously committed
durable transcript can remain resumable for correction while the run summary
stays truthful and nonzero.

JSONL keeps the v1 `condition` and `verification` fields and adds optional
normalized `expectations` plus per-result `expected_sha256` and
`expected_sha256_matches`. The existing `final_sha256` remains the observed
digest, so consumers can compare both without receiving expected content.

## Verification hardening and regressions

`WriteJournal` now derives the final digest from the same descriptor-pinned
postimage used to compare current identity, metadata, and contents with the
latest verified model write. The former second pathname open is no longer on
the success path, closing a check-to-digest replacement window without granting
the model a read capability.

Regression coverage proves:

- uppercase input normalization and final-`=` parsing, including paths with
  `=`;
- an absent exact target can be created and accepted before atomic `-o`;
- a wrong but genuinely journaled/net-changed artifact fails with both hashes,
  retains the effect, and preserves the old `-o`;
- initial-equals-expected and conflicting canonical duplicates make zero
  provider requests;
- generic `--require-write` remains compatible with empty expectation fields;
  and
- the same-snapshot journal observation sees both the accepted postimage and a
  later conflicting mutation truthfully.

The post-change gate passed formatting, JSON-schema parsing,
`git diff --check`, all **767** workspace tests, and warning-denied Clippy.
The resulting Cycle 29 debug binary is 153,689,488 bytes with SHA-256
`f564998fc8e2cd525f332858b9e14eb9bb50a1414668a11a1805c2e5835d3ac1`.
