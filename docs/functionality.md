# Kitsu — supported functionality

> Status: 2026-08-10. This is a source- and test-backed description of the
> current CLI. Hosted-provider availability, credentials, billing, and the
> quality of a particular model remain external to this contract.

Kitsu is one OpenAI-compatible CLI for hosted gateways and local servers. Its
top-level entry points are:

| Command | Behavior |
|---|---|
| `kitsu init [--path PATH]` | No-clobber, credential-free starter config creation. |
| `kitsu doctor [--online]` | Read-only config/auth/context/state/tool-policy diagnostics; network checks are opt-in. |
| `kitsu ask <prompt>` | One checked model turn; streaming by default. |
| `kitsu review [path]` | One tool-free turn over host-verified working/staged/unstaged/base/commit evidence, with optional JSONL and session continuity. |
| `kitsu models [filter]` | Query the configured model catalog without requiring a configured model ID. |
| `kitsu sessions [action]` | Provider-free list/show/resume/fork/export/archive/restore/delete operations, with `--json`. |
| `kitsu chat [prompt]` | Interactive chat on a TTY, deterministic `--script` chat, or one piped turn. |

See the [README CLI reference](../README.md#cli-reference) for every flag and
gateway recipe.

## Chat interface

Interactive chat uses normal terminal scrollback rather than an alternate-screen
TUI. Rustyline supplies history, editing keys, bracketed multi-line paste, and
Tab completion. `/help` and completion share one command registry. `@path`
completion is additionally restricted to already granted, non-protected paths
and refuses symlink-directory traversal.

The implemented chat controls include:

- Inspection: `/status`, `/context [refresh]`, `/permissions`, `/tools`, scoped
  `/diff`, `/checkpoints`, `/checkpoint <id>`, `/jobs`, and `/sessions`.
- Model/context: `/model`, `/models`, `/compact`, `/clear`, and `/retry`.
- Files/workflow: `/allow`, `/allow-write`, `/allow-read-write`,
  `/revoke-read`, `/revoke-write`, `/revoke-command`, `/save`, scoped `/review`,
  and `/undo [id]`.
- Local context: `/skills` and `/skill`; bounded UTF-8 skill resources are
  captured with no-follow reads and loading them grants no filesystem access.
- Scheduling: editable `/queue` controls plus active-turn `/steer` on Unix.
- Session lifecycle: `/new`, `/resume`, `/fork`, `/export`, `/archive`,
  `/restore`, and `/delete`.

`/status` reports the active session, endpoint, model, directory/Git state,
project-context source, context usage, tool and permission state, command roots,
background agents, cumulative compaction savings, and the last compaction
outcome/fallback. `/context` provides the more detailed source-by-source token
estimate and compaction ledger.

`Ctrl+C` cancels an active provider/tool turn, rolls its conversation and context
meter back, and makes the submitted prompt available to `/retry`. Completed file
side effects remain on disk and are reported rather than hidden. At an idle
prompt, `Ctrl+C` clears the input. `/quit wait` waits for background jobs and
`/quit cancel` cancels and joins them; an unqualified exit refuses to discard
live work.

### Prompt queue and platform behavior

The queue is an eight-entry FIFO with list/add/remove/edit/move/promote/clear
operations; the legacy `/queue <prompt>` shorthand still appends. On Unix,
Kitsu owns complete lines typed
while a foreground turn is running and places them into this queue; partial input
remains in the terminal buffer, and permission prompts temporarily own stdin. If
the queue is full, the collector waits for a slot rather than evicting work. A
`/steer <correction>` line uses a separate turn-scoped channel: it rolls back the
partial conversation, retains/reports completed file effects, and restarts that
turn before queued follow-ups. Corrections never become later prompts implicitly.

Automatic type-ahead collection is not implemented on non-Unix platforms.
`/queue <prompt>` still works there when the normal prompt is available, but text
entered during generation remains subject to terminal/OS buffering.

## File and developer workflows

Interactive chat advertises read, edit, and write tools and asks for paths as
needed. The launch directory is read-granted; writes begin ungranted. Scripted
and piped runs advertise filesystem capabilities only when corresponding
`--allow-path`, `--allow-write`, or `--allow-read-write` roots were supplied.
`--allow-write` is write-only; the combined flag is required for readback.
`--yes` suppresses prompts while preserving current-root confinement and never
creates a capability. Only `--dangerously-allow-all-filesystem` bypasses roots
for enabled filesystem tools, and protected Kitsu-private paths still win.

`/permissions` separates invocation and runtime roots, reports invocation roots
revoked for the current session, and lists protected reads/writes plus narrow
Kitsu-managed internal reads. `/revoke-read <exact-root>` and
`/revoke-write <exact-root>` remove only exact user-controlled grants; they
cannot weaken protected/internal state. A new/resumed/forked session restores
invocation roots and drops runtime grants. Exact command grants are numbered,
labeled invocation/runtime, and revocable with `/revoke-command <number>`.
Detached agents share active grants but cannot invoke either interactive
prompter; ungranted background work fails closed without racing foreground
stdin.

- `fs_read_file`, `fs_list_dir`, and `repo_grep` inspect granted paths.
- `fs_write_file` performs a bounded whole-file create/replace.
- `fs_edit_file` is advertised only when both read and write capabilities are
  present. It replaces exact UTF-8 `old_text`; the precondition must match
  exactly once unless `replace_all` is set, and a concurrent change before
  commit causes a conflict instead of an overwrite.
- Successful model writes are grouped into checkpoints. Kitsu reports bounded,
  snapshot-derived unified diffs and an exact recovery command. `/checkpoints`
  lists them, `/checkpoint <id>` previews one, and `/undo [id]` restores the
  selected checkpoint only when its files still match. Undo is blocked while
  background agents are active.
- `/diff` derives bounded working/staged/unstaged+untracked/base/commit/checkpoint
  evidence from Git, the journal, and filesystem. `/review` submits exactly that
  evidence in a no-tools turn. `kitsu review` is the one-shot automation surface.

Developer commands are an independent, explicit capability. `--enable-commands`
registers `command_exec`; roots come from `--command-root` or the launch
directory. It accepts a literal argv array and an explicit cwd—never a shell
string—and returns bounded stdout/stderr plus exit, signal, timeout, truncation,
elapsed-time metadata, safe head/tail previews, and a retained protected `0600`
full-output artifact. Interactive invocations have their own approval
prompt; automation must use exact `--allow-command` grants. `--yes` and
filesystem grants do not approve commands. Command roots constrain only the
requested working directory: approved processes retain the ambient host user's
filesystem, network, environment, and process authority and are not sandboxed.
Session grants also bind the executable observed at approval time; replacement
or changed `PATH` resolution invalidates the grant before execution.
Default command timeout is 120 seconds, the requestable maximum is 600 seconds,
and retained stdout and stderr are each capped at 64 KiB.

`/jobs` exposes bounded running/record capacity, status, elapsed time, model,
usage, tool/write counts, safe recent activity, failure class, full terminal
result retrieval, cancel+join, and pruning. `/agents` remains a compatibility
alias. Background notices use a redraw-safe external printer in plain and
styled modes.

### `@file` attachments

Prompts may contain `@path`, `@path:line`, or `@path:start-end`. Before the user
turn is committed, each mention passes the normal canonical read policy and
protected-path checks. Attachments are regular UTF-8 text only, reject NUL/binary
data, are deduplicated, and are delimited as untrusted data in the expanded
prompt. Selected content is capped at 32 KiB per attachment and 64 KiB per
prompt. A bounded line range streams from a larger file without retaining the
whole file, and reading stops after the requested end line. Kitsu prints the
canonical attachment/range summary. `/retry` re-reads the submitted mentions.

## Sessions and artifacts

Interactive chat autosaves private snapshots and input history under Kitsu's
state directory. Project-local configs receive isolated stable profile
directories under `~/.kitsu`; no session directory is created in the project.
Snapshots contain the conversation/tool transcript, not credentials or runtime
permission grants. `--no-session` disables both snapshots and prompt history.
It does not discard full-output artifacts from explicitly approved developer
commands: those remain in a unique protected runtime-state directory so a
one-shot command never reports a dead recovery path.

Scripted and piped chat are stateless by default. `--session` explicitly
creates a durable conversation, and `--resume [id|last]` resumes and autosaves
in interactive, scripted, or piped modes. Each successful automation turn is
saved before the next one; a save failure makes the automation command fail.
`--no-session` conflicts with both continuity forms. Resume/new-session
boundaries drop runtime grants and restore current invocation roots; no saved
session can restore authority.

The top-level lifecycle/inventory commands operate without constructing a
provider or parsing the selected config. The resolved config path chooses the
state namespace. `list` (also the default), `show`, `fork`, `export`,
`archive`, `restore`, and `delete` support scripting through `--json`.
`sessions resume` launches normal provider-backed chat on a TTY; outside a TTY
or with `--json` it resolves the session and returns exact argv without making
a network call. A missing namespace lists as an empty collection rather than an
error.

- `/resume last` selects the most recently saved *other* session. Resume requires
  the saved working directory and gateway and starts with fresh permissions.
- `/fork [id]` copies a session to a fresh ID and switches to it with fresh
  runtime authority. Persistent tool-output artifacts are copied and exact
  stored pointers are rewritten.
- `/export <file> [markdown|json]` never overwrites the primary destination. If
  the session has artifacts it also creates a sibling named by appending
  `.artifacts` to the complete destination filename (for example,
  `chat.md.artifacts`) and rewrites exact exported pointers to that sidecar.
  Existing primary or sidecar destinations are refused.
- `/archive` and `/restore` move inactive snapshots and their artifacts through
  recoverable private archive storage. `/delete` requires an exact inactive live
  ID and permanently removes its validated snapshot/artifacts.
- Leases prevent concurrent activation or lifecycle mutation of the same saved
  session. Lifecycle commands reject symlinked or irregular private entries.

## Context management

The CLI always gives the orchestrator a token window. Resolution is:

1. `--context-window`;
2. gateway-profile `context_window`;
3. a fresh 24-hour on-disk discovery entry for endpoint + models path + model;
4. the enforced conservative default, 8192 tokens.

On a cold cache, best-effort `/v1/models` discovery runs asynchronously with a
separate two-second deadline and can populate a later run; it never delays the
current inference. Explicit `models`/`/models` requests use the configured CLI
timeout. Within one provider instance, successful catalogs are cached for 60
seconds and eligible failures for five seconds. An unpinned `/model` switch
resolves the new model's window before switching; a CLI/profile-pinned window is
kept. `/context refresh` explicitly refreshes unpinned metadata, preserves the
current known window when discovery fails, and reports cache-write status.

The context meter estimates cold requests structurally, then anchors subsequent
estimates on real provider `prompt_tokens`. It includes tool schemas and reserves
reply space. Large successful tool results are stored as private artifacts with
a re-readable pointer and preview. Under pressure Kitsu elides old tool bodies,
then drops or summarizes eligible whole exchanges while preserving the leading
system context and two newest exchanges. Every concrete provider request gets a
final local fit check; protected recent context that cannot fit returns an
actionable local error instead of being sent oversized.

`--compaction trim` is deterministic. `summarize` uses a separately bounded,
non-streaming summarizer request and accepts a summary only if the rebuilt
history fits; otherwise it records a reason and falls back to trimming. Three
consecutive summary failures open a circuit breaker. `/compact` forces the same
machinery below the automatic threshold and restores the original conversation
if it is cancelled or errors.

See [context-management-design.md](context-management-design.md) for the exact
budget and telemetry contract.

## Automation, output, and exit status

`--script` skips comments/blank lines, treats each normal line as a turn, and
supports verbatim `<<<`/`>>>` multi-line turns. Slash commands work, except
interactive-only queue/session transitions. Failed or cancelled turns roll back
conversation state while preserving completed filesystem changes. Scripts are
strict by default; `--allow-turn-errors` accepts partial completion.

`init --json`, `doctor --json`, and `sessions --json` provide single
machine-readable documents for management workflows. They reject `--jsonl`,
whose event protocol is reserved for model-turn automation.

`--jsonl` is the machine protocol. It disables provider streaming and emits one
`kitsu.chat.v1` JSON object per stdout line: run/turn/command framing,
provider/tool/usage events, assistant text, typed errors, and exactly one
`final_summary`. It rejects interactive TTY chat and human help/version output.
Human diagnostics may still use stderr, but the event stream contains enough
status and counts to determine the run outcome. Error events classify ordinary
failures as `user`, `provider`, or `cancelled`; specialized turn/tool errors may
use a more specific kind.

Without JSONL, stdout contains assistant text only. Human metadata is on stderr;
redirected turns retain stable `kitsu-usage:` and `kitsu-context:` records.

| Exit | Meaning |
|---:|---|
| `0` | Success, including a partial script explicitly accepted with `--allow-turn-errors`. |
| `1` | CLI/input/config/missing-auth, attachment, permission/policy, context-fit, or slash-command failure. |
| `2` | Provider/HTTP/timeout/agent-loop failure, or a strict incomplete turn caused by output truncation, exhausted rounds, or failed tool calls. |
| `130` | Ctrl+C cancellation. |

## Terminal and portability notes

`KITSU_PLAIN=1`, `NO_COLOR=1`, or `TERM=dumb` selects plain rendering.
Renderer-owned banners, progress, context tables, telemetry, tool activity, and
approval prompts then use ASCII structural markers without ANSI/OSC styling.
Model output is always stripped of terminal control strings, but arbitrary safe
Unicode in model text, paths, and user data is preserved. `KITSU_REDUCED_MOTION=1`
uses a visible static status instead of the animated spinner.

Unix builds additionally use descriptor-bound command cwd, process-group command
cancellation, and descriptor-relative no-follow mutation/session operations.
Portable branches validate paths and snapshots and cancel/reap the direct child,
but retain platform pathname races and cannot kill an entire descendant process
group. Operations requiring an unavailable race-free atomic primitive fail
closed rather than weakening the conflict guarantee.

## Verification boundary

The repository contract is local and reproducible: formatting, Clippy, workspace
unit/integration tests, focused CLI/JSONL/context/session/security regressions,
and release audits must pass before a cycle commit. Live hosted inference is a
separate smoke test because it depends on external credentials, billing,
provider availability, and model behavior; it is never implied by the local
suite.
