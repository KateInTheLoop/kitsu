# Claude UX cycle 50 — actionable checkpoint lookup failures

Date: 2026-08-14
Seed: `992650`
Boundary: Unix epoch `1786773600`

## Frozen release

All lanes independently gated the immutable post-Cycle-49 binary at
`target/cycle50-pin/kitsu`: SHA-256
`700271af63480c11b1df449a9c8fb6753da99716bec0032d4fd10f830b4cd19a`,
154,686,744 bytes, mode `0555`, source revision `377d19762436`. The pinned
configuration was mode `0444` with SHA-256
`a7752d4264b2d8914ed51ed57486d30b3cf05d131f65aabd7c3bc3865961a297`.
Each lane froze its oracle before the boundary, launched once from the common
pin, captured raw evidence before adjudication, reaped every child, and made no
shared-tree change or rerun.

## Evidence and attribution

- The provider-free checkpoint lane discovered the selected product gap for
  **94/100**. `/checkpoints` truthfully returned an empty typed inventory, then
  `/checkpoint 54007` emitted a user failure and the run completed its exact
  ten-event JSONL lifecycle with no provider, tool, assistant, session, or
  filesystem activity. Kitsu had already constructed the actionable message
  `checkpoint #54007 is not available; run /checkpoints to list recoverable
  checkpoints`, but printed it directly and returned a generic error marker.
  The JSONL script reducer therefore emitted only `command failed: /checkpoint
  54007`. The frozen oracle's expected exit code `2` was a harness error;
  documented local user failures correctly exit `1`, so that mismatch was
  adjudicated without rerunning. Raw stdout SHA-256:
  `d256b243040288d532b853b5f6471a66cc7aaa8ec5a5a3af5a584b9bf187edaf`;
  adjudication SHA-256:
  `5ff3ec503905283583e8d14efb888cce05ee33ede47a6e68b06ddb6e6241998e`.
- The reasoning-clear/resume lane passed its frozen behavior for **99/100**.
  `/reasoning high` survived an in-process `/clear`, while a fresh process
  resuming the new durable session correctly returned to the gateway default.
  Three exact requests, old/new transcript isolation, session resolution,
  counters, privacy, sentinels, and process cleanup passed. Its separate larger
  observability gap is that `run_started` does not expose effective reasoning
  effort. Evidence SHA-256:
  `6e3df0db8e2970620ef075c7d8ed879a8c4d52d1178626f2778eb02bd2d65de3`;
  clear/resume JSONL SHA-256 values:
  `e696789efd654085290be29b7f74597bfe13aa7d66db08a622d9cfa77c10d503`
  and
  `d6a8c7edcf03bda2e9b616ceac453b1905b66fa7efa853832dc32b946a6959ea`.
- The strict structured-output/readiness lane passed for **100/100** after
  correcting one harness-only serialized-session comparison. One authenticated
  no-tools request used native strict JSON Schema, committed an exact atomic
  51-byte output, retained a private resumable session, and produced no
  artifact, authority leak, secret leak, fixture change, or unreaped child.
  Report SHA-256:
  `ca52003da7869aba3a787630fcdc413da92c78eb31551a80f3ac0ffe8e0dc870`;
  raw JSONL SHA-256:
  `7acb755d5a76263f080cd118f4495725144c54bde27300ed3803b310e6f2293d`.
  Hosted Hugging Face remained `not_run: infrastructure`: credential process,
  service, managed presence, and an approved exact-pin route were absent and
  `request_sent=false`.

No standalone comparator was substituted without a genuinely analogous,
authorized verified-checkpoint JSONL lifecycle.

## Selected improvement

The missing-checkpoint branch used `eprintln!` for the precise host diagnosis
and then returned `SlashResult::Error`. Human interactive use could see the
first print, but scripted JSONL correctly suppressed stderr and had only the
generic error marker available. The result was truthful but unnecessarily
non-actionable machine output.

The branch now returns `SlashResult::UserFailure` carrying that same bounded,
host-authored message. Script reducers retain it in the typed `error.message`;
human scripts render it once with source location. Exit status, failure kind,
checkpoint authority, inventory behavior, and provider-free execution are
unchanged.

## Regression and verification

The new provider-free regression drives `/checkpoints`, missing
`/checkpoint 54007`, and `/quit`. It requires the exact ten-event JSONL order,
empty typed inventory, actionable user error at line 2, exit code `1`, one user
failure, one successful operation, clean stderr, and no provider request. Its
human twin requires the same message exactly once with the script path/line and
forbids the generic fallback.

All **796** workspace tests pass (256 Kitsu unit, 2 provenance, 191 CLI,
100 core, 65 provider, 32 render, and 150 tool tests). Formatting,
warning-denied Clippy, schema parsing, and diff checks also pass.
