# Audit #6 — developer-command UX/security at `dead112`

Read-only audit. Live command execution was blocked for this auditor by loopback approval, so findings distinguish observed help from source/test behavior.

## Severity-ranked findings

1. **High — human command rendering discards useful telemetry.** `CommandOutput` has argv, cwd, termination, exit, signal, streams, truncation, and elapsed time, but the generic renderer effectively shows `command_exec` and `1 line`.
2. **High — truncation keeps only the first 64 KiB and provides no tail or full artifact.** Diagnostics commonly appear at the end, and command-only automation has no read pointer to discarded output.
3. **High — exact argv/cwd grants do not pin executable identity.** PATH binaries, relative scripts, or mutable executables can change after approval while matching the stored string tuple.
4. **High — command roots are not a sandbox, but disclosure is weak.** They constrain cwd only; children retain ambient host authority except stripped loader variables.
5. **Medium-high — scripted pre-grants authorize only the launch directory.** Noninteractive users cannot declare an exact argv/cwd pair for a monorepo subdirectory.
6. **Medium-high — JSONL command events contain serialized previews, not native fields.** Consumers must parse embedded JSON strings and still cannot obtain complete output.
7. **Medium-high — JSONL output failures are silently ignored.** Partial event streams may be treated as success.
8. **Medium — there is no running-command status/stop UX.** The tool is blocking and output is not streamed.
9. **Medium — `/tools` and `/permissions` do not answer what can execute now.** They omit exact grants, provenance, limits, ambient-authority warning, and reset behavior.
10. **Medium — individual command grants cannot be listed/revoked.** Session reset exists internally but there is no management surface.

## Strengths

- Literal argv avoids implicit shell parsing.
- Prompts show escaped argv, canonical cwd, and timeout.
- `--yes` does not approve commands.
- Unix cwd execution is descriptor-bound and revalidated.
- Dynamic-loader injection variables are stripped.
- Exit, signal, timeout, duration, and truncation are truthful internally.
- Stdin is null; stdout/stderr capture is bounded and independent.
- Timeout/cancellation kill process groups and reap children with regression coverage.

## Top three

1. Add a command-specific renderer with argv/cwd, progress, exit/signal/timeout, duration, tail, truncation, and a protected full-output artifact.
2. Make command authority auditable: list/revoke exact grants, explain ambient authority, support explicit argv/cwd pre-grants, and pin or qualify executable identity.
3. Promote command metadata to native reliable JSONL events and fail the run on JSONL output errors.

No repository files were edited or committed by the auditor.
