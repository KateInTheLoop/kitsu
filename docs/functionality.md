# Kitsu — supported functionality

> Status: 2026-08-13. This is a source- and test-backed description of the
> current CLI. Hosted-provider availability, credentials, billing, and the
> quality of a particular model remain external to this contract.

Kitsu is one OpenAI-compatible CLI for hosted gateways and local servers. Its
top-level entry points are:

| Command | Behavior |
|---|---|
| `kitsu` | Default interactive chat entry point. |
| `kitsu -p <prompt>` | Top-level one-shot prompt shorthand; stream the answer and exit. |
| `kitsu init [--path PATH]` | No-clobber, credential-free starter config creation. |
| `kitsu doctor [--online\|--inference]` | Offline config/auth/context/state/tool-policy diagnostics by default. `--online` performs a non-billable catalog-only check; `--inference` sends one explicitly labeled, potentially billable, tool-free completion. |
| `kitsu auth <login\|status\|logout>` | Provider-free, config-scoped managed bearer credentials. Login uses hidden TTY input or explicit `--token-stdin`; status is secret-free; logout removes only the managed value. |
| `kitsu ask <prompt>` | One checked model turn; streaming by default. |
| `kitsu review [path]` | One tool-free turn over host-verified working/staged/unstaged/base/commit evidence, with optional JSONL and session continuity. |
| `kitsu models [filter]` | Query the configured model catalog without requiring a configured model ID. |
| `kitsu sessions [action]` | Provider-free list/show/resume/fork/export/archive/restore/delete operations, with `--json`; fork receipts identify the exact resolved source session. |
| `kitsu artifacts [list\|prune <id\|all>]` | Host-only, provider-free inventory and reclamation of command-artifact storage left by earlier runs, with `--json`. |
| `kitsu agents [--all] [--json]` | Host-only, provider-free listing of the background agents every kitsu process on this machine is publishing, read from the advisory per-process state files. Processes that are gone are skipped unless `--all`; a file from a newer kitsu is reported as schema-unsupported rather than dropped; nothing is created, modified, or deleted. |
| `kitsu chat [prompt]` | Interactive chat on a TTY, deterministic `--script` chat, or one piped turn. |

See the [README CLI reference](../README.md#cli-reference) for every flag and
gateway recipe.

Chat, review, and model-catalog provider work validates its local credential
source before loading project context, registering tools, emitting run-start
telemetry, or contacting a gateway. Missing or malformed credentials are
user/configuration errors (exit `1`), and configured recovery prose is shown
only after terminal sanitization, length bounding, and credential-pattern
filtering.

Connection failures are provider errors (exit `2`) and never interpolate the
provider's URL/body detail. When the selected profile is named or flavored
`ollama` and its configured URL host is structurally loopback (`localhost`, a
`127.0.0.0/8` address, or `::1`), the bounded remedy identifies local Ollama,
suggests starting or verifying `ollama serve`, and offers another gateway as an
alternative. Non-loopback and non-Ollama profiles retain the generic gateway,
network-access, availability, and `doctor --online` guidance. In JSONL mode the
same scoped failure adds `diagnostic_code: "local_ollama_unreachable"` and
bounded recovery-action enums, so automation does not need to parse prose;
generic connection failures omit those optional fields.

JSONL catalog discovery does not hide its network lifecycle. Top-level
`kitsu models` and scripted `/models` or `/model ...` emit a logical
`provider_request` with `purpose: "model_catalog"`; each uncached physical GET
then emits one bounded, secret-free `provider_attempt` before the catalog or
typed error. Top-level events use `operation_id: "models:1"`, while scripted
events keep their command line and text. A provider-local cache hit emits no
attempt because it performs no send.

For `bearer-env`, a non-empty configured environment value wins over the
managed credential. A malformed/non-Unicode environment value fails closed and
never falls back. Managed values are bounded, atomically replaced, `0600`
single-link files below a `0700` config-scoped namespace. Auth management never
contacts the provider or reports a token, length, hash, or path; only
`doctor --inference` checks remote validity. Developer-command roots that
intersect the managed namespace are rejected before a model turn. Login also
requires an absolute `HOME`, so a credential is never written to temporary
fallback state.

`doctor --inference` is the readiness gate for unattended hosted-provider
work. It sends one non-streaming request with no tools, no project context, one
short user message, and a one-token output cap. Its JSON report uses
`online.scope = "chat_inference"`, sets `chat_route_probed`, `request_sent`, and
`potentially_billable` explicitly, and classifies authentication, forbidden
access, billing, model/route rejection, quota, timeout, and transport failures
without echoing provider response bodies. It conflicts with catalog-only
`--online`, so every invocation has one unambiguous network scope.

## Chat interface

Interactive chat uses normal terminal scrollback rather than an alternate-screen
TUI. Rustyline supplies history, editing keys, bracketed multi-line paste, and
Tab completion. Typing `/` opens a bounded, bordered palette below the input;
typing narrows it and Tab completes. Rich mode uses a colored Unicode boundary,
while `NO_COLOR` keeps Rustyline with an uncolored ASCII boundary. `/help`, the
palette, and completion share one command registry. A persistent composer footer
shows the active model and the launch directory; it reads the shared live model
state, so `/model` updates it on the next redraw. Both composer surfaces use
Unicode display-cell widths, cap at 70 columns, and shrink to the live terminal
width. Below 42 terminal columns, the palette drops descriptions and keeps only
command syntax so boundaries do not wrap. `@path`
completion is additionally restricted to already granted, non-protected paths
and refuses symlink-directory traversal.

The implemented chat controls include:

- Inspection: `/status`, `/context [refresh]`, `/permissions`, `/tools`, scoped
  `/diff`, `/checkpoints`, `/checkpoint <id>`, `/jobs`, `/agent`, `/profiles`,
  `/artifacts`, and `/sessions [filter]`; session filters match IDs, names, and
  previews case-insensitively.
- Model/context: `/model`, `/models`, `/reasoning`, `/compact`, `/clear`, and
  `/retry`.
- Files/workflow: `/allow`, `/allow-write`, `/allow-read-write`,
  `/revoke-read`, `/revoke-write`, `/revoke-command`, `/save`, scoped `/review`,
  and `/undo [id]`.
- Local context: `/skills` and `/skill`; bounded UTF-8 skill resources are
  captured with no-follow reads and loading them grants no filesystem access.
  Discovery, aggregate manifest/resource bytes, directory entry count, and
  rendered index size all have deterministic ceilings. Source/resource SHA-256
  snapshots make later drift explicit, and project metadata is framed as
  untrusted until an explicit load.
- Scheduling: editable `/queue` controls plus active-turn `/steer`, `/jobs
  steer|stop|stop-all`, and `/quit cancel` on Unix.
- Cumulative session token usage: `/cost` (tokens + turn count; kitsu
  meters tokens, not currency). Repeat a prompt N times: `/loop <count>
  <prompt>` (2-20, via the bounded prompt queue; interactive chat only).
- Session lifecycle: `/new`, `/resume`, `/fork`, `/export`, `/archive`,
  `/restore`, and `/delete`.

JSONL `/help` exposes the same complete command registry as human help. Every
command object includes `alias_of`: the canonical command name for aliases such
as `/agents` and `/quit`, or `null` for canonical commands.

`/status` reports the active session, endpoint, model, directory/Git state,
project-context source, context usage, tool and permission state, command roots,
background agents, cumulative compaction savings, and the last compaction
outcome/fallback. `/context` provides the more detailed source-by-source token
estimate and compaction ledger.

`Ctrl+C` cancels an active provider/tool turn, rolls its conversation and context
meter back, and makes the submitted prompt available to `/retry`. Completed file
side effects remain on disk and are reported rather than hidden. At an idle
prompt, `Ctrl+C` clears the input. `/quit wait` waits for background jobs and
`/quit cancel` immediately cancels the foreground turn, clears queued prompts,
cancels and joins background jobs, and exits; an unqualified exit refuses to
discard live work.

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
Script files have no live input queue: put follow-up prompts on later non-slash
lines. A scripted `/queue` command fails as a user error with that recovery hint
instead of being mislabeled as a session-management command.

Automatic type-ahead collection is not implemented on non-Unix platforms.
`/queue <prompt>` still works there when the normal prompt is available, but text
entered during generation remains subject to terminal/OS buffering.

## File and developer workflows

Interactive chat advertises read, edit, and write tools and asks for paths as
needed. The launch directory is read-granted; writes begin ungranted. Scripted
and piped runs advertise filesystem capabilities only when corresponding
`--allow-path`, `--allow-write`, or `--allow-read-write` roots were supplied.
`--allow-write` is write-only; the combined flag is required for readback.
`chat --no-tools` removes every model-facing filesystem, command, skill-loader,
and agent tool for the invocation while retaining client-side context,
explicitly preloaded skills, attachments, and session continuity. It conflicts
with write and command capability flags so a grant is never silently ignored.
`--allow-path` remains compatible to authorize client-side project-context or
attachment reads in scripted and piped chat without advertising read tools.
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
stdin. `[agents] blocking` chooses between that and parking: `fail_closed`
(the default, and the behaviour just described) refuses the operation and lets
the agent carry on, while `ask` makes the agent report as `blocked` and wait
for a human answer, which then applies through the same grant code the
foreground prompter uses — `allow always` installs the identical minimal root
or exact argv/cwd grant, so no second permission path exists.
`/jobs reply <id> <y|a|n>` answers a parked agent — allow once, allow always,
or deny — and works from an idle prompt, during a running turn, and through the
`/agent` and `/agents` spellings, which forward every `/jobs` subcommand
verbatim. It accepts exactly the keys the foreground approval prompt accepts
(`y|yes`, `a|always`, `n|no`, case-insensitively) because it calls that same
parser, so the two cannot drift. A denial releases the agent to continue with
the refusal rather than interrupting it, and replying to an agent that is not
waiting reports that instead of silently succeeding.
`[agents] block_timeout_secs` bounds that wait at 1800 seconds by default,
after which the agent ends failed-interrupted rather than waiting forever. The
unit is in the key name deliberately and a bare `block_timeout` is rejected,
never interpreted. `ask` cannot affect scripted, piped or JSONL runs: those
resolve permissions in a mode that returns before the prompt route is
consulted, so the question is never reached — a structural property, not a
default.

- `fs_read_file`, `fs_list_dir`, and `repo_grep` inspect granted paths.
  On Unix, `repo_grep` is an always-available in-process UTF-8 content search:
  it traverses pinned no-symlink handles in deterministic order and applies
  global match, file, scanned-byte, time, and output bounds. It accepts regular
  expressions by default or exact text with `literal: true`; reaching a bound
  produces an explicit truncation receipt rather than a silent partial result.
  Hidden entries and files larger than 1 MiB are skipped. Non-Unix builds use
  the fixed-argv `rg` fallback until an equivalent no-reparse walker exists and
  omit the tool when that dependency is unavailable.
- `fs_write_file` performs a bounded whole-file create/replace.
- `fs_edit_file` is advertised only when both read and write capabilities are
  present. It replaces exact UTF-8 `old_text`; the precondition must match
  exactly once unless `replace_all` is set, and a concurrent change before
  commit causes a conflict instead of an overwrite.
- Successful model writes are grouped into checkpoints. Kitsu reports bounded,
  snapshot-derived unified diffs and an exact recovery command. `/checkpoints`
  lists them, `/checkpoint <id>` previews one, and `/undo [id]` restores the
  selected checkpoint only when its files still match. A missing checkpoint
  retains the host-authored `/checkpoints` recovery hint in both human and
  JSONL script output. Undo is blocked while background agents are active.
- `/diff` derives bounded working/staged/unstaged+untracked/base/commit/checkpoint
  evidence from Git, the journal, and filesystem. `/review` submits exactly that
  evidence in a no-tools turn. `kitsu review` is the one-shot automation surface.
  All three are **host-owned**: the evidence is produced by Kitsu's own `git`,
  not by a model tool, so they require no filesystem grant and never create one.
  Scope is fixed by the repository root of the launch directory and a path
  argument can only narrow it; a workspace inside Kitsu-private state is refused
  outright. `/diff` renders to the terminal only; `/review` and `kitsu review`
  perform one bounded disclosure of that diff to the provider and say so
  (`~ disclosing N bytes …; no read root granted`). `/permissions` reports the
  evidence root, its private exclusions, and that it is not a model read root.
  In JSONL mode, top-level `kitsu review` emits one typed `review_disclosure`
  after `run_started` and before `turn_started`, recording the scope, byte
  count, truncation state, and absence of a model read grant without repeating
  the diff body.
  This differs from `@file` attachments, which do consume model read authority
  because their scope is user-nameable.

Developer commands are an independent, explicit capability. `--enable-commands`
registers `command_exec`; roots come from `--command-root` or the launch
directory. It accepts a literal argv array and an explicit cwd—never a shell
string—and returns bounded stdout/stderr plus exit, signal, timeout, truncation,
elapsed-time metadata, safe head/tail previews, and a retained protected `0600`
full-output artifact by default. Inline-only artifact policy retains the same
bounded previews and counters but deliberately creates no full-output file. The
completed serialized artifact is capped at 16 MiB,
including JSONL framing and hex expansion; it reports separately how many raw
stdout/stderr bytes were retained, marks truncation, and keeps draining both
child pipes. Interactive invocations have their own approval
prompt; automation must use exact `--allow-command` grants, either as a JSON
argv array bound to the launch directory or as
`{"argv":[…],"cwd":"/abs/dir"}` bound to an explicit directory inside a command
root. Both forms are exact argv **and** exact cwd, are validated at startup
(failing closed with the object syntax named when the launch directory is
outside the roots), and are re-applied to their own recorded cwd across `/new`
and `/resume`. `--yes` and
filesystem grants do not approve commands. Exact approval is followed by a
mandatory Linux Bubblewrap boundary: approved command roots are writable,
required runtime paths are read-only, network/PID/IPC/UTS namespaces and the
environment are isolated, and capabilities are dropped. Backend absence,
replacement, or unsupported platforms fail closed before a prompt or child
launch.
Session grants also bind the executable observed at approval time; replacement
or changed `PATH` resolution invalidates the grant before execution. The
approved executable's own directory is a second, independent containment root
requirement on top of the cwd root; a denial for a toolchain outside every
`--command-root` names the exact missing directory and the flag to add.
Contained commands receive a fixed non-secret environment (`PATH`, `HOME`,
`LANG`, `LC_ALL`, `TERM`, `TMPDIR`, `USER`, `LOGNAME`); `--command-env
NAME[=VALUE]` opts one variable at a time into that set (bare `NAME` reads
kitsu's own environment and is refused for secret-shaped names; `NAME=value`
always allows an explicit literal), which is what makes interpreter-based
toolchains (pytest, npm test, ...) with an env-driven module search path work
under mandatory containment. Every exact `--allow-command` pregrant is also
named verbatim (argv and cwd) in the system prompt sent to the model, so it
can call one directly instead of guessing an invocation.
Windows command execution currently fails closed before approval because Kitsu
does not yet have equivalent containment, loader-extension resolution, and
executable-image validation; it never substitutes a pathname-only fingerprint.
Default command timeout is 120 seconds, the requestable maximum is 600 seconds,
retained stdout and stderr are each capped at 64 KiB. Each protected full-output
artifact has a 16 MiB serialized ceiling; the sink enforces 32 retained/in-flight
artifacts and 256 MiB aggregate reservation, deterministically pruning the
oldest verified artifact. `/artifacts` lists usage and prunes by ID or all.

`/jobs` exposes bounded running/record capacity, status, elapsed time, model,
usage, tool/write counts, safe recent activity, failure class, terminal result
handoff, targeted steering, joined cancellation, and pruning. `/jobs view`
presents the same deck grouped by state, with agents needing input first and
empty groups omitted. A blocked agent carries its own marker -- `◉`, or `[blk]`
under `--plain` -- in every agent surface, so the one waiting on an answer is
distinguishable from the ones merely working; the marker vocabulary is shared
with the deck rather than duplicated. `/jobs explain <id>` answers why one agent is in its
state, since when, and what it is waiting on, drawn from the phase, elapsed
time, last activity, failure class, steering and handoff already recorded; it
is read-only and, unlike `/jobs show`, never acknowledges a ready result, so
diagnosing an agent cannot quietly satisfy the unobserved-result exit guard.
`/agent show`
adds a bounded redacted event timeline; tool arguments/output and partial model
bodies are never copied into that inspection record. That timeline also records
each permission a background agent asked for and how it was decided -- allowed
once, allowed always, denied, timed out, or cancelled -- so an answered agent
still shows that it asked, and a cancelled one shows that its request was never
answered. Those entries name the **kind** of permission and nothing else
("approval to write a file"), never the path, URL or command line: the request's
payload is converted to a kind at the boundary and no argument travels to the
recording site, which is how the sentence above stays true of them. A blocked
agent's `/jobs explain` reports the pending permission on its `waiting on:`
line in preference to its last tool activity. Built-in `/profiles`
select a read-only reviewer or a tool-free reasoning worker, and background
spawns can name a profile without inheriting broader capabilities. `/agents`
remains a compatibility alias: a bare call lists, a numeric argument shows
that thread, and any other argument is forwarded verbatim to `/jobs`, so every
subcommand is reachable through either name.

Background agents run in-process, so nothing outside kitsu can observe them.
Under `--reduced-motion` the viewport's status row no longer repaints because
time passed -- the elapsed segment stops ticking -- while the queue and
background-agent tags keep updating, since those are state changes rather than
animation. The row is also coalesced to at most 60 repaints per second in the
painter, so a burst of field changes collapses to the newest value instead of
one paint each; the newest value is never dropped.
`/tui [classic|inline]` switches the renderer between turns and keeps the
conversation: what was printed before the switch stays on screen exactly once,
and the viewport's live status row returns with it. Bare `/tui` reports the
renderer actually running. `fullscreen` is refused until that renderer exists,
naming what is accepted, and `/tui` during a turn is refused rather than queued,
because the mid-turn fall-through would otherwise send it to the model as a
prompt. Switching into a terminal shorter than the viewport's minimum stays
classic and says so.
`Ctrl+L` repaints the viewport's pinned row. Every exit path from the viewport
releases the scroll region and restores the terminal: a normal quit, a panic,
`SIGINT` and `SIGTERM`. The signal path writes one precomposed escape sequence
and restores the line settings captured when raw mode was entered, then re-raises
the signal with its default disposition, so the process still dies by the signal
and in-turn Ctrl+C cancellation keeps working. `kill -9` cannot be restored from.
In the viewport the answer is read on the keypress while the input collector
still holds raw mode, so nothing is echoed and the transcript keeps only the
receipt; the cue is a row of the modal. Classic reads the answer canonically and
still echoes it, which is what keeps line editing and the `yes`/`always`/`no`
long forms working there. One parser serves the filesystem approval, the command
approval and `/jobs reply`, so the accepted answers cannot drift apart.
In the inline viewport a permission prompt renders as a bottom-pane modal
rather than scrolling past: the same option line the classic prompt shows, from
the same composer, and for a command approval the same provenance rows. Because
the modal is transient, the viewport writes the `◉ permission —` receipt into the
transcript once you answer, which is the line classic leaves in place by virtue
of printing it there; the sanitized text is identical either way. `--plain`
keeps its own unstyled prompt and no modal.
In the inline viewport the pinned bottom row carries the model, a compact
context meter, the elapsed time in the current turn, `[N queued]` and the
background-agent tag, refreshed on the same 50 ms input tick that keeps the
ambient counter current, and deduplicated so an unchanged row writes no bytes.
The meter reads the same session usage and effective budget `/context` reports,
so the two cannot disagree. It never wraps between 40 and 200 columns and sheds
segments in the order R5 documents.
In the inline viewport, slash completion works: Tab accepts the showing hint,
and typing a slash prefix opens the bordered command palette above the composer.
**`@file` path completion is not yet wired there** -- the shared completion
source and its parity test exist, and a sweep over every prefix of every
registered command requires the classic prompt, the shared source and the inline
host to agree, so they cannot drift once connected; but nothing in production
drives the inline host yet. A 10 KiB bracketed paste at the idle inline prompt
stays a single draft and is accepted in single-digit milliseconds, measured by
`crates/kitsu/tests/paste_latency.rs` against requirement N2's 100 ms budget.
`--tui <classic|inline>` and the `[tui]` table select the renderer: `mode`
(default `classic`), `alternate_screen` (`auto|always|never`, forced to `never`
by `--no-alt-screen`), `prefix` (default `ctrl+x`), `title`,
`left_arrow_opens_agents`, and `cell_ring_lines` (default 10000, range
1-1000000). The flag wins over the config key, the config key over the default,
and **no environment variable participates**. A misspelled key or an invalid
value is a startup error that names the key and lists what it accepts -- never a
silent fallback -- and `kitsu doctor` reports it as a blocking issue. The error
names the key and the permitted values but never echoes the offending value,
because a config file can hold credentials. `fullscreen` appears in the design
but is not accepted yet.
`[agents] state_file` publishes a small advisory JSON hint, one file per running
process, that an outside tool such as a terminal multiplexer can read to tell
whether this kitsu is `idle`, `working`, or `blocked`, alongside the same
redacted thread records `/jobs` shows. A background agent parked on a permission
raises the process state to `blocked`, is counted in `counts.blocked`, and
carries a `waiting_for` naming the **kind** of permission it needs --
`read_file`, `write_file` or `run_command`, the same vocabulary the
`on_agent_state` hook and `kitsu agents --json` use, and never the path or
command line. Answering the agent removes the key rather than leaving a stale
reason for a poller to read. `auto` (the default) writes only in an
interactive terminal session, so scripted, piped, `ask` and JSONL runs write
nothing at all; `on` writes regardless of how kitsu was invoked, and `off`
never writes. The file lives at `$XDG_RUNTIME_DIR/kitsu/<pid>.json`, falling
back to `$TMPDIR/kitsu-<uid>/` and then `/tmp/kitsu-<uid>/`; the directory is
created `0700` and the file `0600`, a directory that is not a directory, not
owned by this user, or group/world-writable is refused, and a symlink standing
in for it is never followed. It is replaced atomically, refreshed at turn
boundaries rather than continuously, and removed on clean exit. It is a hint,
not a source of truth: it may be stale, absent, or left behind by a process
that died, and writing it never affects a turn — every failure is silent.

`[hooks] on_agent_state` runs a local command when a background agent changes
state, with `KITSU_AGENT_ID`, `KITSU_AGENT_STATE` and `KITSU_AGENT_WAITING_FOR`
added to the inherited environment. An agent that parks on a permission reports
`KITSU_AGENT_STATE=blocked` with `KITSU_AGENT_WAITING_FOR` naming the **kind**
of permission -- `read_file`, `write_file` or `run_command` -- and never the
path, URL or command line it asked about; a hook runs an external program, so
the reason travels as a fixed vocabulary that cannot carry an argument. The same
three words appear in the state file and in `kitsu agents --json`, so a tool can
match on one spelling across all three. Releasing the agent fires a second
transition with its running state and no waiting reason. The command is split into arguments and
executed **directly — never through a shell, and with no variable expansion**,
so a stray metacharacter in the config cannot become execution and the
behaviour does not depend on which shell is installed; point it at a script, or
write `sh -c` yourself if you want interpolation. Its standard input, output
and error are all discarded, because a hook that printed would splice into the
live status row and break the byte-identical output tiers. kitsu never waits
for it, bounds how long it may run, reaps it, and silently ignores a spawn
failure, a nonzero exit, and a timeout: a notification must never cost a turn.
A hook fires once per transition rather than once per observation, at the same
turn boundaries the state file refreshes on, and it is independent of
`[agents] state_file` — either may be used without the other. Styled mode prints background-agent notices
between prompt cycles by default; `KITSU_LIVE_NOTICES=1` opts into a live
redraw-safe external printer at the cost of idle input latency. Literal
plain/assistive input defers notices until the current line is submitted.
Any terminal result not opened through `/jobs show` is surfaced before a session
switch or exit. Scripted JSONL emits the same retained evidence as
`background_result`; an unacknowledged failed detached job makes strict
automation fail.

One per-turn spawn budget covers synchronous `spawn_agent`, parallel
`spawn_agents`, and detached `spawn_agent_bg` calls. It defaults to eight in
interactive chat and two in scripted or piped automation, and can be overridden
by `--agent-spawn-cap` or `agent_spawn_cap` in config. A zero cap unregisters
all four multi-agent schemas, including `agent_result`, so `/tools`, the system
prompt, `provider_request.tools_offered`, and the actual provider body agree
that multi-agent work is disabled; unrelated filesystem, command, skill, and
workspace-evidence tools remain available.

### `@file` attachments

Prompts may contain `@path`, `@path:line`, `@path:start-end`, or
`@path#Lstart-Lend`. Sentence punctuation, parentheses, and quoted paths such as
`@"docs/my guide.md":10-20` are accepted. Before the user turn is committed,
each mention passes the normal canonical read policy and protected-path checks.
Attachments are regular UTF-8 text only, reject NUL/binary data, and are
deduplicated. Selected content is capped at 32 KiB per attachment and 64 KiB per
prompt. A bounded line range streams from a larger file without retaining the
whole file, and reading stops after the requested end line. The expanded prompt
uses one length-prefixed JSON value so file content cannot spoof attachment
boundaries. Kitsu prints the canonical attachment/range summary. `/retry`
re-reads the submitted mentions.

## Sessions and artifacts

Interactive chat autosaves private snapshots and input history under Kitsu's
state directory. Project-local configs receive isolated stable profile
directories under `~/.kitsu`; no session directory is created in the project.
Snapshots contain the conversation/tool transcript, not credentials or runtime
permission grants. `--no-session` disables both snapshots and prompt history.
It does not discard full-output artifacts from explicitly approved developer
commands: those remain in a unique protected runtime-state directory so a
one-shot command never reports a dead recovery path. `--no-artifacts` is the
separate storage control; combine both flags for session-stateless automation
with no new automatic command/tool-result files.

Scripted and piped chat are stateless by default. `--session` explicitly
creates a durable conversation, and `--resume [id|name|last|auto]` resumes and
autosaves in interactive, scripted, or piped modes — `auto` picks the newest
session recorded in the current working directory. Sessions can carry a
human-memorable name: `--name <name>` names (or renames) the live durable
session, `kitsu sessions rename <id|name> [name]` names, renames, or clears
one at rest, and names resolve anywhere a resume selector is accepted. Names
are 1-40 characters of letters, digits, `-` or `_`; `last`, `auto`, and
id-shaped (`s-…`) values are reserved, and duplicates are refused so a name
never becomes ambiguous. Forks receive a fresh id and no name, preserving the
source name as its unique resume selector until the user explicitly names the
fork. Each successful automation turn is
saved before the next one; a save failure makes the automation command fail.
`--resume` already implies `--session`, so passing both explicitly is a
harmless no-op confirmation rather than an error; `--no-session` conflicts
with both continuity forms. Resume/new-session
boundaries drop runtime grants and restore current invocation roots; no saved
session can restore authority.
`/clear` uses the same authority boundary but preserves history first: it saves
the active durable transcript, atomically acquires a fresh unnamed session ID,
then clears context, background-result state, write checkpoints, and runtime
grants. A save or lease failure leaves the old ID and in-memory context
untouched. Human output names both IDs and preserved counts; scripted JSONL
emits a typed `clear_session_boundary` command result. The prior ID remains
resumable, while the fresh ID is `pending_until_first_success` until a model
turn publishes it. Under `--no-session`, the receipt explicitly reports that
discarded in-memory history was not saved.
A newly requested durable session is not published until its first successful
model turn or a slash command that changes durable conversation/model state.
Failed, empty, and query-only starts therefore cannot displace a productive
conversation from `last`; legacy zero-turn snapshots remain addressable by
exact ID.

The top-level lifecycle/inventory commands operate without constructing a
provider or parsing the selected config. The resolved config path chooses the
state namespace. `list` (also the default), `show`, `rename`, `fork`,
`export`, `archive`, `restore`, and `delete` support scripting through
`--json`.
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
  recoverable private archive storage. Archived snapshots are absent from the
  live inventory and cannot be resumed until `/restore <exact-id>` (or
  `kitsu sessions restore <exact-id>`) succeeds. `/delete` requires an exact
  inactive live ID and permanently removes its validated snapshot/artifacts.
- Leases prevent concurrent activation or lifecycle mutation of the same saved
  session. Lifecycle commands reject symlinked or irregular private entries.

Command-artifact generations survive process exit by design, so they accumulate
across runs. `kitsu artifacts` lists and prunes them without a provider and
outside any chat: no model authority is involved and none is created. Removal
requires an exact `runtime-<pid>-<nanos>-<gen>` or `<session-id>.artifacts`
name, a real directory rather than a symlink, and proof of non-liveness through
an advisory lock the kernel releases on process death — pids are reused, so they
are never trusted. Chat startup sweeps only provably empty orphans (including
sidecars whose session never committed a snapshot) and otherwise prints a
threshold notice rather than deleting anything. In-session `/artifacts` reports
what other runs left on disk alongside this session's own usage.

Global `--no-artifacts`, or top-level config `artifact_storage = false`, selects
an inline-only run policy. It creates no command-output sink, offload generation,
or durable-session artifact sidecar; skips automatic orphan sweeping; and never
grants the model a managed artifact read root. Commands retain their bounded
stdout/stderr previews and keep draining to completion. Oversized generic and
background-agent results are reduced to a deterministic UTF-8-safe head/tail
representation that states original, retained, and omitted raw byte counts; no
pointer implies recoverability. Existing sidecars/pointers remain byte-unchanged
but receive no read grant in that run. Sessions, managed credentials, `-o`,
schema inputs, required-write files, explicit model writes, and explicit
lifecycle/export commands are outside this automatic-storage policy.
`/status`, `/permissions`, `/artifacts`, doctor JSON, and JSONL
`run_started.artifact_policy` disclose the effective mode and source.

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

Human catalog views are bounded to 12 rows and pin the exact active model before
the alphabetical preview, matching the footer's current state even when a large
catalog would otherwise hide it. Filter hints use the correct top-level or slash
syntax. Redirected top-level stdout and JSONL keep every normalized matching ID
for automation; a missing configured model still does not block discovery.

The context meter estimates cold requests structurally, then anchors subsequent
estimates on real provider `prompt_tokens`. It includes tool schemas and reserves
reply space. Large successful tool results are stored as private artifacts with
a re-readable pointer and preview by default; inline-only artifact policy keeps
an explicitly lossy bounded head/tail preview instead. Under pressure Kitsu elides old tool bodies,
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

`--jsonl` is the machine protocol. Provider streaming remains enabled unless
`--no-stream` is set. Ordered `assistant_delta` records are provisional until a
terminal `assistant_disposition` marks their turn `committed` or `discarded`;
strict rollback can therefore never masquerade as a successful answer. It emits
one `kitsu.chat.v1` JSON object per stdout
line: run/turn/command framing, provider/tool/usage events, assistant text,
typed errors, and exactly one `final_summary`. It rejects interactive TTY chat
and human help/version output. Human diagnostics may still use stderr, but the
event stream contains enough status and counters to determine the run outcome.
Top-level verified review additionally emits one `review_disclosure` before
its turn starts, so automation can audit the provider disclosure without
parsing the raw prompt.
`-o/--output-last-message <PATH>` gives non-interactive ask, review, piped chat,
and scripted chat a separate host-owned final-answer artifact without changing
stdout. After model and background work ends, Kitsu atomically installs the
exact bytes of the last fully committed assistant message (`0600` on Unix); a
script selects its last committed turn. Discarded, cancelled, blank,
length-limited, and committed-partial responses never replace the destination.
JSONL emits one body-free `output_last_message` receipt after the selected turn
and before `final_summary`, including the exact byte count and SHA-256 plus an
explicit `model_write_authority: false`. The destination is excluded from model
filesystem tools and cannot intersect Kitsu's protected config/state namespace.
`--output-schema <PATH>` adds fail-closed structured output to those same four
automation surfaces. Kitsu accepts one regular, non-symlink schema file up to
64 KiB, validates its supported strict-object shape before provider contact,
sends it through the OpenAI-compatible `json_schema` response format, and then
validates the exact terminal assistant text locally. Only valid JSON matching
the schema can receive a committed disposition or replace `-o`; malformed,
fenced, or mismatched output is discarded and exits `2`. The source file is
protected from model filesystem tools and must be outside any enabled command
or read-only runtime root. JSONL emits a body-free `output_schema` preflight
receipt and marks constrained provider requests with `structured_output: true`.
Schema-rejection errors carry the exact schema SHA-256, instance path, and
validation keyword; `final_summary.output_schema_failures` counts this subset
of `provider_failures` without changing the existing outcome classification.
Scripted and piped chat also accept repeatable `--require-write <PATH>`
completion assertions. The option grants no authority and therefore requires
the exact resolved path to be inside an explicit filesystem write root. If a
target already exists, it must also have model read authority (normally from
`--allow-read-write`, or a matching `--allow-path` plus `--allow-write`);
otherwise Kitsu exits before provider contact because safe replacement needs
the prior contents. Write-only authority remains sufficient for creating an
absent target. Kitsu
adds the resolved targets to the system instruction, then after joined model
and background work verifies a retained filesystem-tool journal mutation, a
net content change from process launch, and an unchanged latest post-write
identity/content snapshot. A missing effect is a user/postcondition failure
(exit `1`) that never installs `-o`; `--allow-turn-errors` does not waive it.
JSONL brackets the run with `completion_postcondition` armed/evaluated receipts
and exposes the additive `final_summary.postcondition_failures` subset of
`user_failures`.
Repeatable `--require-write-sha256 <PATH=64HEX>` strengthens that same contract
with an exact final raw-byte digest. It grants no authority, uses the final `=`
as its delimiter (so paths may contain `=`), accepts case-insensitive hex and
normalizes it lowercase, and retains every ordinary journal/net-change/read
requirement. Malformed or conflicting canonical expectations and an existing
target already equal to its expected digest fail before provider contact. The
evaluated digest is derived from the same descriptor-pinned postimage used to
verify the latest journal write, rather than a second pathname open. A mismatch
is a non-waivable user/postcondition failure (exit `1`), preserves the observed
filesystem effect, and withholds `-o`. JSONL exposes additive expectation and
match fields. Because this check finalizes the whole run after turns and joined
background work, a durable transcript that was already committed can remain
resumable for correction while `final_summary.status` truthfully remains
`error`.
When `--output-schema` is paired with either required-write flag, at least two model
rounds are required. Kitsu omits the provider response format while tools are
offered, treats a no-tool action response as provisional, then withholds tools
and applies the strict schema to one reserved finalization round. The action and
final rounds stay inside the same whole-turn timeout and `--max-rounds` cap.
Before consuming the finalizer, Kitsu reuses the descriptor-pinned active-turn
journal observation for exact-SHA requirements. If an otherwise valid model
write has the wrong digest and both a repair round and the reserved finalizer
remain, Kitsu emits `structured_finalization_deferred` and keeps the existing
tools available with a bounded host correction containing expected/actual
digests. The correction grants no authority. It may repeat only while the
existing round budget can still reserve finalization; exhaustion preserves the
ordinary finalization and terminal postcondition failure. Missing or
unverifiable writes do not trigger this eager gate.
The provisional assistant, repair correction, and host-authored finalization
nudge are provider-only request scaffolding that is removed before conversation
state is committed, so one submitted prompt remains one durable user turn. Real
tool calls/results and the committed final assistant remain resumable. The final
locally valid answer still cannot replace `-o` unless the independent
required-write evaluation succeeds. A native tool call returned after tools are
withheld is never executed; the provider-class failure carries the safe typed
cause `tool_call_during_tool_free_finalization` instead of suggesting a generic
gateway-compatibility problem.
`final_summary.duration_ms` is a monotonic process-entry-to-summary duration on
success, error, and cancellation; consumers do not need an external timer.
Its `*_failures` counters classify failed requested operations/turns, while
`tool_errors` counts individual foreground tool-call errors, `policy_errors`
is its authority-denial subset, and `recovered_*` identifies errors belonging
to turns that ultimately committed.
Error events distinguish user/provider/cancelled failures, ordinary tool
failures, and local policy denials. Status, permissions, context, diff, model,
compaction, session-inventory, skill, and job commands return typed
`command_result` payloads rather than requiring stderr parsing.
`run_started.session_snapshot_state` distinguishes stateless/disabled,
pending-until-first-success, and immediately resumable continuity. Resuming
always rebuilds the leading system context from the current project, skills,
tools, model, and permission surface instead of trusting a stale saved system
message. `final_summary` repeats the terminal snapshot state and includes a
session ID only when that state is `resumable`; a planned ID whose first turn
failed remains visible for correlation in `run_started` but is never presented
as a resumable terminal destination.

Length-limited strict turns keep their partial assistant text visible but roll
back and exit `2`. When the request used `--max-output-tokens N`, human errors
and JSONL disposition reasons name the current value and suggest retrying with
`2N` or higher. If the gateway supplied the limit, the recovery text instead
asks for an explicit larger `--max-output-tokens <n>`. The JSONL terminal status
remains `discarded`; recovery guidance does not weaken the commit contract.
Likewise, an empty or whitespace-only terminal assistant message is an
incomplete model turn, never evidence that a tool workflow finished. Strict
ask/review/piped/script runs discard it and exit `2`; interactive chat rolls it
back, preserves `/retry`, and reports any completed filesystem effects that
remain on disk. Empty intermediate assistant content attached to real tool
calls remains valid.

For one-shot chat, `--timeout-secs` is one deadline across piped-input reading,
attachment preflight, retries, response-body streaming, and the model turn. A
positional prompt is consumed immediately and never waits for an unrelated open
stdin handle. Script file acquisition is also bounded. Each scripted turn gets
one inference deadline across all of its provider and tool rounds; a tool call
does not reset the timer before the follow-up provider request.

Without JSONL, stdout contains assistant text only. Human metadata is on stderr.
Inference-capable commands end with elapsed time: terminals receive a compact
secondary status row, while redirected runs receive
`kitsu-timing: {"duration_ms":...,"status":"..."}`. The status is `ok`,
`partial`, `error`, or `cancelled`; accepted partial scripts use `partial` even
though `--allow-turn-errors` preserves exit code `0`. Redirected turns retain
the existing stable `kitsu-usage:` and `kitsu-context:` records. Provider-free
management commands remain free of the timing receipt.

| Exit | Meaning |
|---:|---|
| `0` | Success, including a partial script explicitly accepted with `--allow-turn-errors`. |
| `1` | CLI/input/config/missing-auth, attachment, permission/policy, local tool, context-fit, or slash-command failure. |
| `2` | Provider/HTTP/timeout/agent-loop failure, or a strict incomplete turn caused by output truncation or exhausted rounds. |
| `130` | Ctrl+C cancellation. |

## Terminal and portability notes

`--plain`/`KITSU_PLAIN=1`, `--screen-reader`/`KITSU_SCREEN_READER=1`, and
`TERM=dumb` select plain rendering plus literal canonical line input. The input
path uses no raw mode, cursor control, bracketed-paste negotiation, completion,
or history editing; notices are deferred until a line returns. It is
intentionally line-oriented: each newline submits a turn instead of preserving
a pasted multi-line draft as one prompt. `--screen-reader` is the discoverable
assistive-technology contract; `--plain` remains suitable for stable terminal
transcripts.

`NO_COLOR=1` selects the same ASCII, no-animation rendering but does not change
the rich Rustyline editor, so history, completion, and bracketed multi-line paste
continue to work; the automatic slash palette and persistent model/directory
footer remain available with ASCII boundaries. Literal plain/screen-reader
input omits both cursor-controlled surfaces and keeps `/help` available. In
every mode, model output is stripped of terminal control
strings while arbitrary safe Unicode in model text, paths, and user data is
preserved. `--reduced-motion`/`KITSU_REDUCED_MOTION=1` uses a visible static
status instead of the animated spinner.

Linux builds additionally use mandatory Bubblewrap command containment.
Unix builds use descriptor-bound command cwd, process-group command cancellation,
and descriptor-relative no-follow mutation/session operations.
Portable branches validate paths and snapshots and cancel/reap the direct child,
but retain platform pathname races and cannot kill an entire descendant process
group. Operations requiring an unavailable race-free atomic primitive fail
closed rather than weakening the conflict guarantee. In particular, implicit
project context, filesystem skills, and `@file` attachment reads/completion are
disabled on non-Unix platforms until equivalent component-pinned traversal is
available, and developer commands are disabled without a supported containment
backend.

## Verification boundary

The repository contract is local and reproducible: formatting, Clippy, workspace
unit/integration tests, focused CLI/JSONL/context/session/security regressions,
and release audits must pass before a cycle commit. Live hosted inference is a
separate smoke test because it depends on external credentials, billing,
provider availability, and model behavior; it is never implied by the local
suite.
