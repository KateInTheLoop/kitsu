# Chat UX dogfood — cycle 3 audit checkpoint

Date: 2026-08-10

Baseline: `dead112` on `dogfood/top10-improvements`, five commits ahead of
`main` (`513151a`). Ten independent auditors exercised differentiated Kitsu
surfaces using local no-auth mocks, isolated temporary state, CLI/PTY probes,
and focused source/test evidence. No hosted inference, credentials, or billable
calls were used. Runtime claims blocked by loopback approval are labeled as
source-derived in the individual reports.

The dedicated Codex comparison used only the
[official OpenAI Developer commands documentation](https://learn.chatgpt.com/docs/developer-commands?surface=cli).

## Baseline fix review

The five new commits add build provenance, actionable timeout/policy messages,
read support for write roots, redirected-output heartbeat feedback, a 120-second
default deadline, and advisory content-quality warnings.

Baseline gates:

- 362 workspace tests passed.
- Workspace Clippy with warnings denied passed.
- Diff whitespace checks passed.
- No Hugging Face token was present in the repository.
- Rustfmt check failed in two mechanical locations.

The review also reproduced stale incremental dirty-build provenance, found that
the displayed “build” date is actually the commit date, identified inconsistent
read/write capability documentation, and found raw special-token and plain-mode
Unicode issues in the new write-warning path. Full evidence is in
[the baseline review](chat-ux-cycle-3-audits/00-baseline-fix-review.md).

## Ten most urgent improvements

### 1. Make filesystem authority safe and unsurprising

`--yes` must auto-approve only within explicit capability roots. Global
filesystem bypass needs a separately named, unmistakably dangerous flag.
Restore genuine read/write independence (or expose an explicit combined
capability), align CLI and slash semantics, and expand `/permissions` with grant
provenance, protected reads, exact command grants, ambient-command warnings,
and revocation.

Acceptance: `--allow-write ./out --yes` cannot read or write outside `./out`;
write-only does not disclose existing content; a separate bypass is explicit;
effective authority can be listed and revoked.

### 2. Make JSONL a durable, quiet protocol

Replace best-effort printing with one fallible serialized emitter. Propagate
serialization/write/flush/EPIPE failures to a nonzero exit, suppress script
prompt/command echoes and routine human diagnostics, document field/order and
compatibility contracts, and represent every physical provider attempt.

Acceptance: a closed consumer cannot yield a false success; stdout remains one
event per line; default stderr contains no duplicated prompt; one final summary
is emitted when the sink remains available; retries are countable.

### 3. Make developer commands observable and auditable

Render argv, cwd, elapsed time, exit/signal/timeout, truncation, and separate
stdout/stderr. Preserve head and tail plus a protected full-output artifact.
Promote those fields to native JSONL events, support explicit argv/cwd
pre-grants, disclose ambient host authority, list/revoke grants, and bind or
qualify executable identity.

Acceptance: a failed long build retains its final diagnostic and full output;
humans and automation see the same structured termination facts; authority is
inspectable before execution.

### 4. Show and protect verified edits

After each writing turn, automatically show a bounded verified unified diff
linked to its checkpoint and recovery command. Add checkpoint list/preview and
targeted undo. Require a stale-content precondition for replacing an existing
file through `fs_write_file`. Sanitize protocol-marker warnings and consider a
review/quarantine gate for suspicious output.

Acceptance: later user edits cannot be overwritten by a stale full-file write;
the user sees exact observed hunks without trusting model prose; any checkpoint
can be inspected and a safe target can be selected.

### 5. Add unified background-job control

Provide `/jobs` or an expanded `/agents` surface with stop-one/stop-all,
concurrency/cost limits, elapsed/model/status, recent activity/output, tool and
write counts, usage, failure class, and pruning. Make quit/EOF choices explicit
instead of requiring an implicit Ctrl+D then Ctrl+C sequence.

Acceptance: no detached agent is uncontrollable; users can identify what is
running, inspect progress, stop it, and exit with an explicit wait/cancel choice.

### 6. Separate active steering from queued follow-ups

Create an in-flight steering channel distinct from the next-turn FIFO. Add
queue remove/edit/reorder/clear/promote controls and visible key/command hints.
Route every asynchronous notification through a prompt-safe renderer in styled
and plain modes; expose discoverable plain and reduced-motion flags.

Acceptance: urgent corrective input can affect the current task; queued input
is editable; background completion cannot corrupt a partially typed prompt;
PTY tests cover normal/plain/reduced modes.

### 7. Make sessions first-class outside the REPL

Add top-level list/show/resume/fork/export/archive/restore/delete commands with
machine-readable output and friendly empty states. Add explicit opt-in durable
continuity for script/piped execution. Keep saved authority out of snapshots,
but clearly summarize which invocation-level grants remain and offer reviewed
permission profiles/re-grants.

Acceptance: recovery and housekeeping require no provider call; automation can
resume a selected thread predictably; `--no-session` visibly proves that
snapshot/history persistence is off.

### 8. Add onboarding diagnostics and trustworthy provenance

Implement `kitsu init` and `kitsu doctor` for config creation/search paths,
gateway/model selection, auth-variable presence, provider reachability,
context/cache source, session storage, and tool-policy checks. Make version
provenance rebuild correctly for dirty sources and label commit/build dates
truthfully. Restructure help around progressive disclosure and expose slash
command groups before entering chat.

Acceptance: a missing config offers a safe next command; doctor identifies the
failing layer without exposing secrets; dirty/stale binaries self-identify;
`models --help` is not dominated by unrelated agent flags.

### 9. Expose provider attempts, safe errors, and context provenance

Emit retry attempt, safe status category, delay/`Retry-After`, and remaining
deadline in human and JSONL modes. Use one sanitized actionable error formatter
for interactive, ask, piped, script, and model-catalog paths. Expose context
window source, cache age/TTL, discovery state, and refresh; align slash and
standalone catalog ordering/no-match semantics.

Acceptance: a three-request retry reports three attempts; URLs/bodies cannot
leak credentials; timeout guidance is equally actionable in every mode; users
can tell whether the window came from CLI, config, cache, discovery, or default.

### 10. Expand verified review targets and noninteractive review

Support staged/unstaged scopes, base branches, commits/ranges, checkpoints, and
custom review instructions while retaining verified evidence and no-write
review turns. Provide a first-class noninteractive review/exec entry point with
resumable JSONL and explicit final-output/schema contracts.

Acceptance: users can review `--base main`, one commit, only uncommitted work,
or one checkpoint without manufacturing a temporary working tree; automation
gets a stable final result contract.

## Audit report index

1. [Onboarding and diagnostics](chat-ux-cycle-3-audits/raw/01-onboarding.md)
2. [Editing, verified diffs, undo, and output](chat-ux-cycle-3-audits/raw/02-editing.md)
3. [Sessions and privacy](chat-ux-cycle-3-audits/raw/03-sessions.md)
4. [Permissions and authority](chat-ux-cycle-3-audits/raw/04-permissions.md)
5. [JSONL and automation](chat-ux-cycle-3-audits/raw/05-automation.md)
6. [Developer commands](chat-ux-cycle-3-audits/raw/06-commands.md)
7. [Background agents and steering](chat-ux-cycle-3-audits/raw/07-background-agents.md)
8. [Terminal accessibility](chat-ux-cycle-3-audits/raw/08-accessibility.md)
9. [Provider and model UX](chat-ux-cycle-3-audits/raw/09-provider-models.md)
10. [Official Codex DX comparison](chat-ux-cycle-3-audits/raw/10-codex-comparison.md)

## Rebase checkpoint

At audit start and after all ten reports, the repository had no remote, no
upstream tracking branch, and no newer local ref. `main` remained at `513151a`
and the dogfood branch at `dead112`; rebasing onto `main` at that instant would
therefore be a no-op. The reports are checkpointed before re-checking refs so a
parallel agent's later commits can be integrated without losing audit work.
