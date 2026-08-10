# Chat UX dogfood — cycle 4 audit checkpoint

Date: 2026-08-10

Baseline: `c485edb` on `dogfood/top10-improvements`. Ten independent auditors
exercised onboarding, coding, automation, permissions, jobs, providers,
accessibility, context/skills, Codex parity, and adversarial release behavior.
They used isolated temporary homes/repositories and localhost no-auth mocks; no
hosted inference, real token, or billable provider call was used. Auditors made
no repository edits.

Baseline gates:

- `cargo test --workspace`: 462 tests passed.
- Working tree was clean before reports were saved.
- The branch had no remote or upstream; local `main` remained `513151a`.

The dedicated comparison used only official OpenAI documentation for
[Codex CLI](https://learn.chatgpt.com/docs/codex/cli),
[subagents](https://learn.chatgpt.com/docs/agent-configuration/subagents), and
[approvals/security](https://learn.chatgpt.com/docs/agent-approvals-security).

## Ten most urgent improvements

### 1. Secure and explain implicit project context and skills

Automatic `llms.txt`/`AGENTS.md`/`CLAUDE.md` loading must never follow a
repository symlink outside the authorized project. Read implicit context with
descriptor-pinned no-follow traversal, preserve source identity/provenance in
saved sessions, show project versus user skill origin, and keep explicit user
`--skill` selection working under `--no-project-context`. Project-controlled
`always` skills must not silently escalate automatic injection.

Acceptance: a linked project context file cannot disclose an outside secret;
explicit user skills remain loadable when automatic project context is off;
duplicate skill origin and automatic-injection policy are visible.

### 2. Bound every provider-controlled allocation

Apply explicit byte ceilings to nonstreaming completions, model catalogs, error
bodies, SSE records, aggregate stream buffers, and accumulated tool arguments.
Return typed, safely redacted failures and count actual bytes without echoing
untrusted bodies.

Acceptance: a huge body, newline-free SSE record, or fragmented tool argument
fails within a fixed memory bound and deadline; catalog and completion success
remain unaffected below the limit.

### 3. Make undo part of conversational truth

After a successful checkpoint undo, append a trusted model-visible event that
identifies the reversed checkpoint and files, invalidates prior mutation tool
claims, and tells the model to re-read before further edits. Preserve the
verified disk rollback and conflict checks.

Acceptance: the next provider request cannot reasonably interpret an undone
edit as still present, and the UI links the undo marker to its checkpoint.

### 4. Make doctor a real readiness gate

Normalize/deduplicate the live catalog and validate the exact effective model.
Treat an explicitly requested but skipped online probe as incomplete/nonzero,
align `init`'s next command with the file it wrote, and expose bounded reason
codes for malformed catalogs without response-body leakage.

Acceptance: `doctor --online` cannot pass when the first configured request will
fail model-not-found; its model count matches `models`; the generated quick
start inspects the generated config.

### 5. Make durable continuity select useful work

Do not create a durable snapshot until the first successful turn or explicit
save. Include cwd in session JSON and runnable resume guidance. Persist or
require acknowledgement of terminal background-job results before quit, and
surface interactive autosave failure as a persistent dirty state rather than a
transient warning.

Acceptance: failed/empty starts never replace a useful `last`; resume guidance
works outside the original directory; completed unviewed work and unsaved turns
cannot silently disappear.

### 6. Stream JSONL assistant output

Keep provider streaming enabled in JSONL and emit versioned
`assistant_delta` records through the same fallible serialized sink. Preserve
one-line framing, event ordering, terminal summaries, backpressure behavior,
and cancellation semantics.

Acceptance: long automation turns expose live deltas; a closed sink stops
provider/tool mutation; concatenating deltas reproduces the committed answer.

### 7. Give JSONL typed results and truthful failure accounting

Define structured payloads for status, permissions, context, diff, model
catalog/switch, compaction, sessions, skills, and jobs. Separate provider,
tool, policy, user, and cancellation failures in events and summaries, backed by
a schema conformance corpus.

Acceptance: automation never has to parse a human text blob or issue a follow-up
query to learn whether a mutation changed state; local policy denial is not
reported as provider failure.

### 8. Make in-flight telemetry end-to-end truthful

Do not label HTTP header acceptance as terminal provider success. Add body
completion/failure state, mark rolled-back streamed fragments, and preserve
known partial stdout/stderr, executable identity, elapsed time, signal, and
artifact cleanup status when a command is cancelled.

Acceptance: every physical attempt has one accurate terminal disposition, and
SIGINT never fabricates zero captured bytes after output was observed.

### 9. Make attachments natural and non-spoofable

Parse quoted paths, common sentence punctuation, spaces, parentheses, and line
ranges without treating prose punctuation as filename bytes. Encode attachment
boundaries and metadata structurally so file contents cannot forge an end
marker. Retain permission, no-follow, UTF-8, range, and size checks.

Acceptance: `@app.py:1-2.` works as expected; delimiter-looking file contents
cannot impersonate host instructions; ambiguous input fails locally with a
precise correction.

### 10. Close terminal-control and authority-observability gaps

Strip or visibly encode bidi/zero-width format characters across every dynamic
terminal field, give reduced-motion status a complete exclusive line, and add a
truly escape-free assistive editor mode. Reject unresolved exact command grants
and render their observed executable path/fingerprint/cwd. Observe newly created
private-file identities at creation time rather than relying on a later scan.

Acceptance: Unicode direction controls cannot visually rewrite IDs/argv; reduced
motion never merges status with notices; `/permissions` cannot call a missing
executable an active grant; protected-file aliases remain denied.

## Important strategic follow-ups

These did not displace the ten release-correctness items above, but remain
significant: OS-enforced command sandboxing, inspectable subagent threads,
heterogeneous agent profiles/models, a shared sync/background resource budget,
immediate `/jobs` control during generation, searchable session/model pickers,
atomic bounded context-cache persistence, command-artifact retention, and
create-new-by-default `/save`.

## Audit report index

1. [Onboarding and sessions](chat-ux-cycle-4-audits/raw/01-onboarding-sessions.md)
2. [Coding, editing, review, and commands](chat-ux-cycle-4-audits/raw/02-coding-editing-commands.md)
3. [JSONL and automation](chat-ux-cycle-4-audits/raw/03-jsonl-automation.md)
4. [Permissions and authority](chat-ux-cycle-4-audits/raw/04-permissions-authority.md)
5. [Jobs and steering](chat-ux-cycle-4-audits/raw/05-jobs-steering.md)
6. [Provider, context, and retry](chat-ux-cycle-4-audits/raw/06-provider-context-retry.md)
7. [Terminal accessibility](chat-ux-cycle-4-audits/raw/07-terminal-accessibility.md)
8. [Skills, attachments, and context](chat-ux-cycle-4-audits/raw/08-skills-attachments-context.md)
9. [Official Codex comparison](chat-ux-cycle-4-audits/raw/09-codex-comparison.md)
10. [Adversarial release gate](chat-ux-cycle-4-audits/raw/10-adversarial-release.md)

## Rebase checkpoint

At audit start, `dogfood/top10-improvements` pointed at `c485edb`, had no
configured upstream, and the local `main` branch remained `513151a`. Reports
are saved before implementation and before re-checking refs so work introduced
by another agent can be rebased or integrated without losing the audit record.

