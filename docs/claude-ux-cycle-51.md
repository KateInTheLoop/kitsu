# Claude UX cycle 51 — scripted queue recovery

Date: 2026-08-14
Seed: `992651`
Boundary: Unix epoch `1786775400`

## Frozen release

All lanes independently gated the immutable post-Cycle-50 binary at
`target/cycle51-pin/kitsu`: SHA-256
`5a3e29d9c6692d1e1cfcf883c635b6be397fcdd8e2d9f277be1112275bfaa3f7`,
154,686,808 bytes, mode `0555`, source revision `bbf7b8e3cb1b`. The pinned
configuration was mode `0444` with SHA-256
`a7752d4264b2d8914ed51ed57486d30b3cf05d131f65aabd7c3bc3865961a297`.
Each lane froze its oracle before the boundary, launched once from the common
pin, captured raw evidence before adjudication, reaped every child, and made no
shared-tree change or rerun.

## Evidence and attribution

- The staged-diff metadata lane passed its exact provider-free behavior for
  **97/100**. `/diff staged ledger a650.txt` returned the exact 205 host-derived
  bytes, target and revision metadata, truncation state, byte count, and
  `model_disclosed=false` in a seven-event JSONL lifecycle. There was no model,
  tool, session, root grant, mutation, stderr, or leaked evidence. Its separate
  low-severity gap is that the result omits the parsed `path_filter`, requiring
  automation to correlate/reparse the command envelope for paths with spaces.
  The private fixture commit used Kate as both author and committer. Raw stdout
  SHA-256:
  `940fa177bf8ab9335a144f9f5e2411fc0bc96542201343e863e9dd61306d8989`;
  adjudication SHA-256:
  `7997d1f017b6c27ab25d340af38bf85c9005ea3f6fc554130db9ca16355e293c`.
- The scripted queue lane discovered the selected product gap for **97/100**.
  Two model turns committed around `/queue list`; the queue command made no
  provider request, emitted one typed user failure, and the second turn retained
  the clean first-turn history. The final summary was partial with one user
  failure, two successful operations, and a resumable two-turn session whose
  canonical transcript excluded the slash command. The raw error nevertheless
  said `session commands require interactive chat`, even though queue control is
  not session management and a script-native alternative exists. The frozen
  harness expected an obsolete disposition field name and provider exit code;
  raw evidence correctly used `status` and local `EXIT_USER=1`, so those two
  harness assumptions were adjudicated without rerunning. Evidence SHA-256:
  `6d308dcfb5ff95183ac928838e8769d842973e3fc729ff34f24920664711f88a`;
  raw JSONL SHA-256:
  `c4691d7dc3bd6bfd03b52c6ddcfaf9dd2e9c5d4cb09fd1beba82417f04eddf50`.
- The invalid-schema recovery/readiness lane passed for **100/100**. Two exact
  authenticated native-strict requests offered no tools. The invalid output
  failed closed with a typed schema location, discarded disposition, exact
  counters, and byte-preserved preexisting output; the recovery committed an
  exact atomic 53-byte schema-valid result. Auth lifecycle, private modes,
  redaction, artifact suppression, integrity, and process cleanup passed.
  Report SHA-256:
  `ad8e3fd23df195c63640705d413d576de8f6e007d3efd1e6e42ea4c270932fa8`;
  invalid/recovery JSONL SHA-256 values:
  `774f8e639405c05d1cf41bf85f4b928f375eb9f35ea1eda382018eef39092418`
  and
  `e451ff1026fd57e5e88b592cc3c8aac6a12355589bb14a3106ac9c183c756e1f`.
  Hosted Hugging Face remained `not_run: infrastructure`: credential process,
  service, managed presence, and an approved exact-pin route were absent and
  `request_sent=false`.

No standalone comparator was substituted without a genuinely analogous,
authorized scripted live-queue lifecycle.

## Selected improvement

The script reducer grouped `SlashResult::QueueAction` with `/new`, `/resume`,
`/fork`, export, archive, restore, and delete. It therefore used the common
`session commands require interactive chat` error, which accurately rejected
the operation but mislabeled it and gave automation no way to translate queue
intent into a valid script.

Queue actions now have their own reducer branch and message:
`queue commands require interactive chat; put prompts on later script lines`.
The command remains a typed local user failure with exit code `1`; strict versus
accepted-partial behavior, counters, provider isolation, and session semantics
are unchanged. True session commands retain their existing message.

## Regression and verification

The new provider-free regression runs `/status`, `/queue list`, and `/quit`
under `--allow-turn-errors`. It requires the exact ten-event JSONL sequence,
queue-specific typed user message at line 2, partial status, one failure, one
successful operation, disabled session state, clean stderr, and no provider
request. Its human twin requires the same located message exactly once and
forbids the old session-command wording. Functionality and JSONL protocol docs
now state the script-native recovery.

All **797** workspace tests pass (256 Kitsu unit, 2 provenance, 192 CLI,
100 core, 65 provider, 32 render, and 150 tool tests). Formatting,
warning-denied Clippy, schema parsing, and diff checks also pass.
