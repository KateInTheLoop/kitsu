# Chat UX dogfood — cycle 5 audit checkpoint

Date: 2026-08-10

Baseline: `af20c26` on `dogfood/top10-improvements`. Ten independent auditors
used the current Kitsu binary across onboarding, coding, provider-free JSONL,
permissions, jobs, providers, accessibility, skills, Codex parity, and an
adversarial release pass. They used disposable `/tmp` fixtures and localhost
no-auth mocks; no hosted inference, real token, or billable provider was used.
Auditors made no repository edits.

The dedicated comparison used current official OpenAI documentation for
[Codex CLI](https://learn.chatgpt.com/docs/codex/cli),
[subagents](https://learn.chatgpt.com/docs/agent-configuration/subagents), and
[approvals and security](https://learn.chatgpt.com/docs/agent-approvals-security).

## Ten most urgent improvements

### 1. Put commands behind an OS-enforced sandbox

Exact argv, cwd, and executable fingerprint approval limits accidental command
selection but does not contain an approved process. Add a fail-closed OS backend
that restricts filesystem, network, process visibility, and inherited secrets;
retain exact-command approval as the separate consent layer. Bind command
artifacts to aggregate quotas, retention, listing, and pruning.

Acceptance: an approved command cannot read outside its sandbox roots, reach the
network by default, inspect unrelated host processes, or inherit arbitrary
credentials; unsupported platforms fail closed with an actionable diagnostic.

### 2. Make background agents inspectable threads

Jobs expose strong body-free telemetry and terminal results, but users cannot
enter a worker thread, inspect its evolving transcript, or steer that specific
worker. Add `/agent list|show|switch|steer|stop|close`, explicit result handoff,
and durable thread ownership without leaking partial bodies into `/jobs`.

Acceptance: a user can inspect and steer one running agent without cancelling
the foreground or exposing another agent's transcript, then return to the main
thread with a clear consolidated result.

### 3. Add specialist agent profiles and reasoning controls

Expose reusable profiles with per-agent model, reasoning effort, instructions,
tool set, and sandbox/permission defaults. Reasoning-only delegation must work
with an empty local capability set instead of requiring a dummy filesystem or
command grant.

Acceptance: a read-only reviewer and a tool-free reasoning worker can be
selected independently, `/reasoning` is visible and effective, and children
inherit only the profile's explicitly authorized capabilities.

### 4. Bound positional and piped input in the end-to-end deadline

`chat <PROMPT>` can wait for stdin EOF before starting inference, so a held-open
non-TTY stdin defeats `--timeout-secs`. Resolve positional input immediately,
make stdin-consumption policy explicit, and cover input acquisition, retries,
provider work, and shutdown with one observable deadline/cancellation contract.

Acceptance: a positional prompt starts without waiting for unrelated open stdin;
held-open-pipe tests complete within the configured deadline and emit a closed
human/JSONL terminal outcome.

### 5. Make build provenance reliably follow Git

Multiple auditors observed a stale `target/debug/kitsu` after an ordinary
`cargo build`; only a targeted clean rebuild embedded `af20c26`. Make build
script dependencies track the actual repository Git directory, symbolic ref,
packed refs, index, and dirty inputs across worktrees.

Acceptance: build at commit A, advance a fixture repository to B, run ordinary
`cargo build`, and prove `--version` reports B without `cargo clean`.

### 6. Make streamed and final output transactional

A strict tool/policy failure can emit an assistant answer before rollback and
then classify the turn as failed. Mark streamed deltas/assistant text provisional
until commit, or emit an explicit discard event and visibly invalidate human
output. Preserve the original tool failure and verified disk rollback.

Acceptance: automation can distinguish committed from discarded text; human
output never leaves a false success claim standing after strict rollback.

### 7. Make durable sessions and JSONL commands tell the same truth

A query-only `--session` run announces a durable session ID and success but
creates no resumable snapshot. `/checkpoints` and scripted `/help` can succeed
without a result payload. Either materialize the announced session or explicitly
label it pending/non-durable; add typed checkpoint inventory and help results.

Acceptance: every reported durable ID is immediately listable/resumable, and
every successful JSONL command has a causally paired result or a typed no-result
contract.

### 8. Make onboarding and doctor prove the path users will run

Top-level help still suggests `kitsu init` followed by bare `kitsu doctor`, even
though init defaults to `./kitsu.toml` and doctor defaults to the user profile.
Unify the canonical path, make no-auth JSON distinguish credential presence from
readiness, and make online doctor either probe the configured chat route or say
precisely that only the model catalog was checked.

Acceptance: clean-home `init -> doctor -> first chat` follows copied commands;
doctor cannot report full provider readiness when only `/models` is healthy.

### 9. Make active controls immediate, ordered, and accessible

During a hung foreground turn, `/jobs stop`, `/jobs stop-all`, and `/quit cancel`
sit behind the prompt FIFO; queue notices and refusal errors are reordered.
`/new` can emit a stale completion notice from the previous session, and plain
assistive Ctrl+C does not immediately restore a fresh prompt. Route operational
controls through the active-turn channel and preserve one ordered notice stream.

Acceptance: job/quit controls take effect during inference, queue acknowledgements
retain input order, session switches reset notice baselines, and plain Ctrl+C
atomically clears the line and redraws an escape-free prompt.

### 10. Treat skill and resumed context metadata as bounded untrusted data

Project skill descriptions enter the system prompt before explicit load;
discovery has per-file but no aggregate count/index/resource budget; saved system
context can advertise stale skills while current tools differ. Bound and frame
metadata, lazily load resources, persist hashes/source provenance, and rebuild
the active system prompt on resume. Use structured framing for skill resources.

Acceptance: repository skill metadata cannot provide unbounded hidden
instructions or startup I/O; resume discloses changed/removed sources and the
system prompt matches the active capability set.

## Implementation outcome

The implementation pass closed all ten priorities on the same branch, without
hosted inference or credentials:

1. Developer commands now require both exact argv/cwd consent and a successful,
   fingerprinted Linux Bubblewrap preflight. Writable command roots, read-only
   runtime mounts, isolated network/PID/IPC/UTS/user namespaces, capability
   dropping, and a fixed environment are enforced; unsupported or changed
   backends fail closed. Full-output artifacts have per-file and aggregate
   count/byte ceilings plus `/artifacts` inventory/pruning.
2. Background work has bounded redacted thread snapshots, targeted steering,
   stop/close semantics, explicit result handoff/acknowledgement, and `/agent`
   plus richer `/jobs` JSON. Partial model bodies and tool payloads are excluded
   from the inspection timeline.
3. The request path transports optional reasoning effort, `/reasoning` changes
   it for later turns, and validated built-in profiles provide a read-only
   reviewer and an intentionally tool-free reasoning worker. Profile-selected
   children receive only their intersected registry and instructions.
4. A positional chat prompt starts without consulting stdin. Piped input,
   attachment expansion, and the one-shot model turn share one deadline; script
   file acquisition is cancellable and bounded. Held-open-stdin regressions
   cover both paths.
5. Build invalidation follows the real/common Git directories, worktree HEAD,
   index, loose/packed/reftable refs, and source/ignore inputs. Ordinary and
   linked-worktree fixtures prove A-to-B and dirty-to-clean provenance changes
   without `cargo clean`.
6. Streaming is explicitly transactional: JSONL closes provisional
   `assistant_delta`/`assistant` output with a committed or discarded
   `assistant_disposition`; human mode visibly invalidates rolled-back text.
7. `run_started` labels session snapshots disabled, pending until first success,
   or resumable. Help, tools, checkpoint inventory, profiles, reasoning,
   artifacts, and thread controls produce paired structured command results.
8. Generated quick-start commands use the actual config path. Doctor separates
   credential requirement/presence/readiness, normalizes and validates the
   selected model catalog, and labels `--online` as catalog-only rather than a
   chat-route probe.
9. Active-turn job steering/stopping and `/quit cancel` bypass the prompt FIFO;
   notices preserve one input-order stream, session switches discard stale
   notice baselines, and plain assistive Ctrl+C returns to an escape-free prompt.
10. Skill discovery/loading now has deterministic count, manifest, index,
    resource, and omission budgets; project metadata is framed as untrusted;
    explicit loads retain SHA-256 provenance. Resume always rebuilds current
    system context and revalidates every persisted `/skill` snapshot, replacing
    drifted content and removing unavailable sources with a disclosure.

The cycle is intentionally stopped after this implementation/release gate; no
sixth hourly audit starts automatically.

## Release verification

The final integrated tree passed 558 local tests: 184 Kitsu unit tests, 2 Git
provenance fixtures, 84 CLI integration tests, 79 core tests, 58 provider
tests, 21 renderer tests, and 130 tool/permission tests. The strict workspace
Clippy gate (`-D warnings` with all targets and features), formatting check,
diff whitespace check, and published JSON Schema parse also passed. These gates
used offline dependencies and local fixtures; they did not call hosted models.

## Important follow-ups

Host-owned `/diff` should not require widening model read authority; exact
command pregrants need an explicit cwd; stateless command artifacts need a
retention UX; model/session selection should be searchable; and plain Markdown
could gain a screen-reader-oriented rendering mode. These remain important but
did not displace the ten cross-cutting correctness and Codex-parity items above.

## Report index

1. [Onboarding and sessions](chat-ux-cycle-5-audits/raw/01-onboarding-sessions.md)
2. [Coding and recovery](chat-ux-cycle-5-audits/raw/02-coding-recovery.md)
3. [Provider-free JSONL automation](chat-ux-cycle-5-audits/raw/03-jsonl-automation.md)
4. [Permissions and command authority](chat-ux-cycle-5-audits/raw/04-permissions-authority.md)
5. [Jobs, queue, and steering](chat-ux-cycle-5-audits/raw/05-jobs-steering.md)
6. [Provider and context](chat-ux-cycle-5-audits/raw/06-provider-context.md)
7. [Terminal accessibility](chat-ux-cycle-5-audits/raw/07-terminal-accessibility.md)
8. [Skills, attachments, and context](chat-ux-cycle-5-audits/raw/08-skills-attachments.md)
9. [Official Codex comparison](chat-ux-cycle-5-audits/raw/09-codex-comparison.md)
10. [Adversarial release gate](chat-ux-cycle-5-audits/raw/10-adversarial-release.md)

## Rebase checkpoint

At audit start the branch pointed at clean `af20c26`, local `main` remained
`513151a`, and no remote/upstream was configured. This report is committed before
implementation so later local branch changes can be reviewed and rebased without
losing the independent evidence.
