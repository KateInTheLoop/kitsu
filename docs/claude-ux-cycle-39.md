# Claude UX cycle 39 — truthful partial timing

Date: 2026-08-14
Seed: `992639`
Boundary: Unix epoch `1786753800`

## Scored evidence

All three lanes gated the same immutable post-Cycle-38 binary: SHA-256
`4bb24a27d73ae16e01ddc27909234fa63512a7cdce1c40d2fcc167878f9a490d`,
154,664,896 bytes, mode `0555`, source revision `f7b075239b99`. The pinned
configuration was mode `0444` with SHA-256
`a7752d4264b2d8914ed51ed57486d30b3cf05d131f65aabd7c3bc3865961a297`.

- The provider-free randomized lane ran `/permissions`, `/status`, an unknown
  command, and `/quit` under `--allow-turn-errors`. Human mode reported exactly
  two successful operations and one failure. JSONL ended `partial` with one
  user failure, no conversational/provider/tool events, and no session. Both
  process groups exited `0` in 11 ms and were fully reaped. It scored **99/100**;
  the only finding was the adjacent legacy text `1 failure(s)`. Evidence
  SHA-256: `a3f5bb1c402f0b8474ec40ad6af148084e4b3d98e873a381f023695c866a10c0`.
- The context/session lane committed one authenticated model turn, then
  accepted a failing `/quit later` command. Its output file and one-turn
  durable session contained only the committed model result; the failed slash
  command was absent. All children were reaped and decoys remained absent. The
  run receipt truthfully said one successful operation and one failure, but the
  final redirected timing record said `status: "ok"` even though the accepted
  run was partial. Its raw harness had one unrelated substring false negative;
  the evidence-backed score was **96/100**. Evidence SHA-256:
  `82789bf989aaa3fc32c8d400a189b011136fd40a2cb3478dc37149b834e86298`.
- The Hugging Face-shaped lane combined a slash command with two committed
  model turns through an owned local adapter. Authentication precedence,
  bounded artifact-free reads, atomic output, sessions, redaction, and process
  reaping all passed. Hosted HF remained `not_run` infrastructure because no
  process, user-service, or managed credential existed and no remote request
  was sent. Its only raw scorer failure expected deny mode despite deliberately
  granting one read root; the adjusted product score was **100/100**. Evidence
  SHA-256: `82d975c30178919f57f56bb7ae0506682ea5812981ff166299d5eb7427baf880`.

No standalone Codex or Claude score was substituted: neither had an honest,
isolated equivalent for the mixed slash-command/model lifecycle.

## Selected improvement

An accepted partial script exits `0`, so the process-level timing footer could
not distinguish it from a clean run even though the JSONL final summary already
did. Kitsu now records the exact host acceptance decision and renders timing as
`partial` (or `partially completed in` on a terminal). Strict failures remain
`error`, cancellation remains `cancelled`, clean success remains `ok`, and exit
codes are unchanged.

The same terminal-summary path now formats `1 failure` and plural failures
naturally. A shared formatter keeps the legacy script summary and the newer run
receipt consistent without changing structured counters.

## Regression and verification

The mixed `--allow-turn-errors` CLI regression now requires a zero exit, the
successful assistant result, singular failure wording, no `failure(s)`, and a
redirected timing status of `partial`. Render tests cover terminal and
redirected partial timing, while a unit test pins both singular and plural
failure phrases. README and functionality documentation enumerate all four
timing states and explain the accepted-partial case.

All **789** workspace tests pass (256 Kitsu unit, 2 provenance, 184 CLI,
100 core, 65 provider, 32 render, and 150 tool tests). Formatting,
warning-denied Clippy, published-schema parsing, and diff checks also pass.
