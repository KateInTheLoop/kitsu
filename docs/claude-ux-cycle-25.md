# Claude UX cycle 25 — readable required-write replacements

Date: 2026-08-14
Seed: `992625`
Boundary: Unix epoch `1786725000`

## Scored evidence

All three workers pinned and ran the Cycle 24 postcondition image, SHA-256
`19cd23ac362cf27d575e611f63c7075814199197e384ee89327a973d5ea250a0`,
from private copies. No scored run depended on a later shared build.

- The randomized recover task scored **85/100**. Qwen again returned
  schema-valid placeholder JSON without calling a tool, but `--require-write`
  rejected the false completion with an exact unchanged-file fingerprint,
  `postcondition_failures: 1`, exit `1`, and no `-o` replacement.
- The deterministic piped lifecycle scored **84/100**. It selected a
  write-then-revert case, but exposed a configuration contradiction before
  that branch could execute: the existing target had write-only authority,
  while Kitsu's conflict-safe replacement tool required prior contents that
  the model was forbidden to read. Receipts and preservation behavior were
  otherwise exact.
- The Hugging Face-shaped local lifecycle scored **90/100**. Its negative case
  passed. The intended positive fixture made the same write-only existing-file
  mistake and was rejected safely. Hosted HF remained
  `not_run: infrastructure` because neither credential gate was populated, so
  no remote request was sent.

Standalone Codex `0.147.0` initialized with isolated writable state and reached
`thread.started` and `turn.started`, but the outer environment blocked both
WebSocket and HTTPS provider traffic. It exited after its built-in retries with
no tool or fixture activity and was classified `not_scored: infrastructure`.

## Selected improvement

Kitsu now rejects an existing `--require-write` target before `run_started` or
provider contact unless the model has read authority for the resolved file.
The error explains that safe replacement needs `--allow-read-write` (or a
matching `--allow-path` alongside write authority). Write-only authority
remains valid for a target that is absent at launch, so creation workflows keep
least privilege.

This check uses the same resolved target as the write contract. It neither
widens permissions nor discloses contents, and it prevents spending a provider
round on a task whose only registered replacement mechanism cannot succeed.

## Regression and release gate

End-to-end regressions cover all three capability states:

1. existing target plus `--allow-write` fails before provider contact;
2. absent target plus `--allow-write` still creates and verifies successfully;
3. existing target plus `--allow-read-write` performs read, conflict-safe
   replacement, journal verification, and successful completion.

The post-change gate passed formatting, schema parsing, `git diff --check`, all
**756** workspace tests, and warning-denied Clippy. The resulting debug binary
is 153,475,904 bytes with SHA-256
`9fcb1d268dd89b72de54d860f061c49a45242fc9fa640d91385a86d8589fd3f1`.

The next evidence-backed experiment should separate schema-constrained final
answer formatting from pending tool action when `--output-schema` and
`--require-write` are combined. That is a new bounded optimization; the current
fail-closed postcondition remains the source of truth.
