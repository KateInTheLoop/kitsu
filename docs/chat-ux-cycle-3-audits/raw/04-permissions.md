# Audit #4 — permission/security UX at `dead112`

Read-only audit; no repository edits. The live scripted probe was interrupted, so runtime tool-call behavior is unverified and findings use matching help/source evidence.

## Severity-ranked findings

1. **High — `--allow-write` silently grants read access.** `run_chat` appends every write root to read roots. CLI help describes only writes, README says capabilities are independent, and in-session `/allow-write` grants only write. Users can unintentionally expose pre-existing files.
2. **High — `--yes` is an unusually broad permission bypass.** Once a filesystem capability is registered it approves any non-protected path. Documentation is honest, but the conventional name still suggests unattended confirmation rather than removal of containment.
3. **Medium-high — command-root confinement is not process sandboxing.** Exact argv/cwd, no shell parsing, timeouts, cleanup, caps, and loader-variable stripping are strong, but an approved command retains ordinary user filesystem/network/environment authority. Prompt and `/permissions` do not state this clearly.
4. **Medium — `/permissions` is not a complete authority audit.** It omits protected reads, grant provenance, exact command grants, base versus session grants, and the command-sandbox warning. There is no `/revoke`.
5. **Medium-low — reset wording can overstate freshness.** Runtime grants clear, but invocation-level filesystem roots, launch-directory read, and explicit command pre-grants remain active.

## Working controls

- Approval answers require exact recognized tokens and pasted prose fails closed.
- Filesystem/command prompts serialize stdin ownership.
- Private configuration/state is protected under broad grants and `--yes`.
- Internal artifact reads are narrow and cleared on session switches.
- Traversal, symlink, protected-destination, and command-cwd replacement defenses fail closed.
- Filesystem denials identify path, roots, and recovery flags.
- `--yes` neither registers tools nor approves commands.

## Top three

1. Restore read/write independence or rename the capability; make CLI, slash commands, help, docs, and tests agree.
2. Replace unrestricted `--yes` with scoped auto-approval and a clearly dangerous explicit bypass.
3. Expand `/permissions` with provenance, exact command grants, protected reads, sandbox warnings, and `/revoke`.

No repository files were edited or committed by the auditor.
