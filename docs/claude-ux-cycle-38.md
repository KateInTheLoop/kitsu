# Claude UX cycle 38 — explicit no-output receipts

Date: 2026-08-14
Seed: `992638`
Boundary: Unix epoch `1786752000`

## Scored evidence

All three lanes gated the same immutable post-Cycle-37 binary: SHA-256
`459ee57c6976a73b3d6f49d9b931e192141a5937bc682295c03b877a131c8640`,
154,663,976 bytes, mode `0555`, source revision `7c7a18eecd5e`. The pinned
configuration was mode `0444` with SHA-256
`a7752d4264b2d8914ed51ed57486d30b3cf05d131f65aabd7c3bc3865961a297`.

- The provider-free randomized lane ran an absent-destination human script and
  a preexisting-destination JSONL script. Both exited `0` in 10 ms or less,
  created or replaced no output, used no provider/tool authority, and reaped
  cleanly. JSONL ended with
  `output_last_message_state: "preserved_no_committed_assistant"`; human stderr
  contained the matching notice exactly once. Its raw scorer incorrectly
  required that notice to be the *only* stderr line even though `/status`, the
  run receipt, artifact policy, and timing are legitimate diagnostics. The
  adjudicated product score was **100/100**. Evidence SHA-256:
  `5ecfddc74354d2222c270df581979da86ee0120d6d9592b7002a681506d4f5bd`.
- The context/session lane ran a durable query-only JSONL script. Its exact ten
  events contained no provider, assistant, disposition, or output-install
  record. The preexisting destination kept its bytes and inode; the planned
  session remained pending with no transcript or resumable ID; `/permissions`
  showed zero model authority. It scored **100/100**. Evidence SHA-256:
  `fc260c76d6d0782959d427db39f4c1f3927279e0a66576a384b2a566df10aac2`.
- The Hugging Face-shaped lane authenticated to an owned local adapter, retained
  a bounded 7,936-byte read preview under `--no-artifacts`, then returned only
  whitespace. Kitsu emitted provisional output followed by an authoritative
  discarded disposition, exited `2`, preserved the old `-o`, reported the new
  summary state, leaked no credential or artifact path, and reaped everything.
  Hosted HF remained `not_run` infrastructure because process, user-service,
  and managed credentials were all absent; `request_sent` was false. Two raw
  scorer failures were stale fixture/assistant assumptions, so the adjudicated
  product score was **100/100**. Evidence SHA-256:
  `8b1237f0e2f680d0f451f271b7870e1fb8fe61325760e4f7d8fa8dae2bd02360`.

No standalone Codex or Claude score was substituted: neither exposed a
genuinely analogous provider-free no-assistant/output-last-message workflow in
the isolated environment.

## Selected improvement

The new output-preservation contract passed. Its human evidence exposed an
adjacent wording defect: the same `/status` output said the conversation had
zero user and assistant turns, but the terminal run receipt called the one
successful slash command `1 turn(s) ok`. `ScriptRunStatus` intentionally uses
one success count for model turns and slash commands (including when deciding
whether `--allow-turn-errors` accepted partial success), so labeling that count
as turns was false.

The receipt now calls the combined count **successful operations** and reports
failures as a separate correctly pluralized count. This preserves exit status,
partial-success semantics, JSONL, and every counter while making query-only,
mixed-command, and model-turn scripts truthful. The internal field and method
names use the same operation terminology so future call sites do not reintroduce
the category error.

## Regression and verification

The query-only output-preservation regression now requires
`~ run summary: 1 successful operation` and rejects the historical
`turn(s) ok` wording while retaining all destination/no-op assertions. The
README documents that human receipts combine completed model turns and slash
commands as operations. All **788** workspace tests pass (255 Kitsu unit, 2
provenance, 184 CLI, 100 core, 65 provider, 32 render, and 150 tool tests).
Formatting, warning-denied Clippy, published-schema parsing, and diff checks
also pass.
