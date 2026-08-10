# Audit #2 — developer editing UX at `dead112`

Repository remained untouched. Only `/tmp` fixtures were prepared. The live loopback write probe was interrupted before output, so live write/undo behavior is marked unverified.

## Severity-ranked findings

1. **P1 — Successful edits do not show a verified patch automatically.** `format_write_checkpoint` reports checkpoint ID, path, byte count, and first changed line, not exact before/after hunks. Model prose is not verified evidence; users must separately run `/diff`.
2. **P1 — Truncated command output loses the diagnostically important tail and has no full artifact.** Capture is prefix-oriented, and offloading occurs only after the tool result is already truncated.
3. **P1 — Human command/tool rendering hides execution metadata.** The result contains cwd, exit/signal, timeout, elapsed time, and truncation fields, but the generic human view does not render a Codex-like command card.
4. **P1 — Full-file writes lack an explicit stale-content precondition.** `fs_edit_file` has an exact precondition; `fs_write_file` can replace a file read at an older version. `/undo` is after-the-fact recovery.
5. **P1 — The new content-quality warning can re-inject the dangerous marker.** Its text includes raw `<|im_start|>` and tool results are fed back to the model.
6. **P2 — `/diff` and `/review` are verified but narrowly scoped.** There are no staged-only, base, commit/range, or checkpoint targets.
7. **P2 — `/undo` is safe but opaque.** There is no checkpoint list, preview, targeted undo, or dry run.
8. **P2 — `--allow-write` behavior is inconsistent across help and documentation.** Implementation implies read, but help describes only writes and README still calls grants independent.
9. **P2 — Corruption detection warns after persistence.** Suspicious content has already been committed; there is no rejection/quarantine review gate.
10. **P3 — Cancellation semantics are truthful but require manual recovery.** Output should identify affected checkpoint IDs/files and offer the exact recovery command.

## Positive findings

- `fs_edit_file` has exact-match and concurrent-change protection.
- Checkpoints reflect observed filesystem state.
- `/undo` refuses later conflicting changes.
- `/diff` is based on Git/filesystem evidence.
- `/review` gets verified evidence in a no-tools turn.
- Strict scripts document non-zero failed/incomplete outcomes.
- Private tool-output artifacts are write-protected.

## Hands-on evidence

- `target/debug/kitsu --version` returned `kitsu 0.1.0 (dead112, built 2026-08-10)`.
- `chat --help` confirmed strict scripts, JSONL, 120-second timeout, and filesystem/command separation.
- The deterministic mock was bound on port 18181.
- An attempted plain scripted write against `/tmp/kitsu-ux-work` was interrupted without output and supports no live conclusion.

## Top three urgent improvements

1. Automatically show a bounded verified patch after every edit, linked to its checkpoint and `/undo`.
2. Preserve full command output in protected artifacts and render argv/cwd/exit/duration/truncation clearly.
3. Make existing-file `fs_write_file` replacements conflict-aware and sanitize protocol-marker warnings before returning them to the model.

No repository files were edited or committed by the auditor.
