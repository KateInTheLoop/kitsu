# Changelog

All notable changes to kitsu are documented here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and
this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).
While the major version is `0`, the CLI surface, config schema, and JSONL event
shapes may change in any minor release.

## [Unreleased]

### Fixed
- A session commit could fail with `session '<id>' is already active in this or
  another process` when nothing else held it. The session lock is advisory and
  kernel-held per open file description, and a descriptor marked close-on-exec
  is still duplicated into a child between `fork` and `exec` — so any tool call,
  agent hook or `git` invocation running on another thread could make an
  uncontended lock appear taken. Both lock sites now retry briefly before
  reporting a conflict, which leaves genuine cross-process contention reported
  exactly as before. Under a sustained burst of subprocess spawning the retry
  can still be exhausted, so this reduces the window rather than closing it.
- The same false conflict affected three more places, each failing quietly
  rather than loudly: pinning a config target could report the target busy; an
  artifact generation that was finished and reclaimable could be reported as
  still in use, so it was never cleaned up; and taking an artifact generation
  lock could report failure and skip work. All five lock sites now share one
  implementation, so there is a single place for this to be right.
- In the styled terminal, the **last line of a streamed answer could vanish**
  when the model's final chunk did not end in a newline — the usual shape — so
  an answer could appear to end a line early. The line was written and then
  painted over by the live status row.
- Cancelling a background agent that was waiting for a permission left **no
  record of how the request ended** — `/jobs show` kept the request with nothing
  after it, so the timeline said the agent had asked and never said what
  happened. Every way a request can end is now recorded.
- `[hooks] on_agent_state` exported a stale activity label (often `started`) as
  `KITSU_AGENT_WAITING_FOR` for an agent that was in fact waiting for a
  permission, so a hook was told the state was `blocked` but given a nonsense
  reason.

### Added
#### Background agents that can ask

- `[agents] blocking = "ask"` lets a background agent **park** when one of its
  own tools needs a permission you have not granted, instead of being refused.
  It reports as `blocked`, appears first in `/jobs view` under `needs input`,
  and waits; answering applies exactly the grant the foreground prompt would
  have applied, through the same code, so no new permission path exists.
  `[agents] block_timeout_secs` bounds the wait at thirty minutes by default,
  after which the agent ends as failed-interrupted — an unattended run cannot
  hang on a question.

  **The default is `fail_closed` everywhere**, which is exactly today's
  behaviour, so nothing changes unless you opt in. Scripted, piped and JSONL
  runs are unaffected even then: they never reach the question, because their
  permission mode short-circuits before the route is consulted.

  Answer a parked agent with **`/jobs reply <id> <y|a|n>`** — allow once, allow
  always, or deny — from an idle prompt *or* while a turn is running, and with
  either spelling (`/agent` and `/agents` forward it too). It takes the same
  keys the foreground approval prompt takes, including the long forms `yes`,
  `always` and `no`, because it calls the same parser rather than a second copy.
  A denial releases the agent to carry on with the refusal, exactly as the
  foreground prompt would. Replying to an agent that is not waiting says so
  rather than silently doing nothing.
- A parked background agent is now visible **outside** the process: the state
  file reports process state `blocked` with a per-agent `waiting_for`,
  `[hooks] on_agent_state` fires `STATE=blocked` with the same value, and
  `kitsu agents --json` reports both. All three use one machine vocabulary —
  `read_file`, `write_file`, `run_command` — so a tool can match a single
  spelling, and all three name the **kind** of permission and never the path,
  URL or command line. Answering an agent removes the reason rather than leaving
  a stale one behind.
- A background agent's bounded timeline now records **each permission it asked
  for and how that ended** — allowed once, allowed always, denied, timed out, or
  cancelled
  — so `/jobs show` still shows that an agent asked, after it has been answered.
  `/jobs explain` reports a pending permission on its `waiting on:` line instead
  of the agent's last tool activity. These entries name the kind of permission
  ("approval to write a file") and deliberately **never** the path, URL or
  command line, keeping the timeline's existing promise that tool arguments are
  never copied into an inspection record.
- An unknown `/jobs` subcommand now names what you typed even when it is too
  far from any real one to suggest a correction — previously a far miss printed
  only the usage line, leaving you to spot your own typo in it.
- An interrupted background agent now says **why**: the failure reads
  `run interrupted before completion: authority input closed before a decision`
  where it previously stopped at the first clause. The old text is still the
  prefix, so anything matching it keeps matching.



#### Terminal UI (preview)

- In the agent view, **`↑`/`↓` move the selection**, which now actually works:
  the arrow keys used to close the view and leak their escape sequence into the
  prompt underneath. The selection follows **the agent**, not the row position,
  so an agent finishing and re-grouping can never silently move your next
  keystroke onto a different one; it clamps at both ends rather than wrapping,
  and moving it never collects a result.
- In the agent view, **`Space` peeks** the selected agent: its bounded redacted
  timeline, the permission it is waiting on if blocked, and its result or failure
  if finished. Peeking counts as collecting the result, exactly as `/jobs show`
  does, so a peeked result is not reprinted when you exit — while one you never
  looked at still is. Peeking a blocked agent collects nothing, and merely
  opening the view collects nothing.
- **The agent view opens.** In the inline viewport, `Ctrl+X a`, `/jobs view` (or
  `/agents view`), or pressing **←** twice on an empty composer opens an overlay
  listing your background agents grouped by state — agents needing input first —
  with `?` for keys and `Esc` to close. The chord follows `[tui] prefix`, and
  `[tui] left_arrow_opens_agents = false` turns off the ← route while leaving the
  chord. The first ← only hints, so it cannot open by accident, and ← on a
  non-empty draft still just moves the cursor. `↑` history recall is unchanged.
  In classic mode `/jobs view` keeps printing the text deck as before.
- A background agent that is **waiting for your answer now has its own marker**
  in `/jobs list`, `/jobs view`, `/jobs show` and `/jobs explain` — `◉`, or
  `[blk]` under `--plain`. It previously shared the busy agent's marker, so the
  one agent needing you looked exactly like the ones that did not. No other
  state's marker changed.
- `--reduced-motion` (and `KITSU_REDUCED_MOTION=1`) now affects the inline
  viewport, not just the wait indicator: the status row stops repainting just
  because the clock moved. It still updates the moment something actually
  changes — a prompt queued, an agent needing input — because hiding that to
  keep the row quiet would defeat the point of the tag.
- The viewport's status row is now capped at 60 repaints a second in the painter
  itself, rather than relying on how often its caller happens to ask. Collapsed
  frames keep the newest state rather than dropping it.
- **`/tui [classic|inline]`** switches the renderer mid-session without losing
  the conversation, and reports the live renderer when called bare. `fullscreen`
  is refused until it exists, and `/tui` typed during a turn is refused rather
  than queued — it is accepted between turns.
- **Ctrl+L repaints** the inline viewport's pinned row.
- Fixed: interrupting the inline viewport with **Ctrl+C**, or the process taking
  a `SIGTERM` or panicking, used to leave the terminal with a **pinned scroll
  region** — output stayed trapped in part of the screen until you ran `reset`.
  Every one of those paths now releases the region, restores line settings,
  turns bracketed paste off and shows the cursor, and then lets the signal
  finish the process as it would have. Turn cancellation with Ctrl+C is
  unaffected. `kill -9` still cannot restore anything, by its nature.
- In the inline viewport, answering a permission prompt no longer leaves the
  typed letter and a stray `> ` in your transcript: the answer is read on the
  keypress and the cue is drawn in the modal, so the transcript keeps only the
  `◉ permission —` record. Classic mode still echoes the answer as it always
  has. Note that in the viewport a *pasted* answer is denied rather than
  accepted, and the long forms `yes`/`always`/`no` need classic or
  `/jobs reply`.
- In the inline viewport, **permission prompts now appear as a modal in the
  bottom pane** instead of scrolling past in the transcript, with the same
  option line as the classic prompt and, for command approvals, the same
  provenance rows (working directory, executable, timeout, authority). Once you
  answer, the viewport leaves the same `◉ permission —` line in the transcript
  that classic leaves, so the record of what was asked and allowed is identical.
  Classic output is unchanged, and `--plain` keeps its existing prompt.
- The inline viewport's pinned bottom row is now a **live status row** instead of
  the word `kitsu`: the model, a compact eight-cell context meter with its
  utilisation percentage, the elapsed time in the current turn, `[N queued]` when
  prompts were submitted mid-turn, and the background-agent tag. It refreshes
  while a turn runs, never wraps between 40 and 200 columns, sheds segments in a
  fixed order as the terminal narrows (the agent tag is last to go), and is
  cleared on exit. Classic mode is unchanged.
- The inline viewport's composer gained **`@file` path completion** and hint
  highlighting. It offers the same candidates in the same order as the classic
  prompt, because both read from one completion source rather than two
  implementations kept in step by hand.
- `--tui inline`, or `[tui] mode = "inline"` in the config, opts into an inline
  viewport: a status row pinned to the bottom of the terminal with the
  transcript scrolling above it, instead of one status row repainted in place.
  The flag wins over the config key and the config key over the `classic`
  default; no environment variable selects it. The rest of the `[tui]` table —
  `alternate_screen`, `prefix`, `title`, `left_arrow_opens_agents` and
  `cell_ring_lines` — is parsed and validated now, and a mistyped key or value
  is an error that **names the key** instead of being quietly ignored. Adding a
  `[tui]` table makes older kitsu builds reject the config file. The idle prompt inside it is kitsu's own
  editor, so typing is echoed on the pinned row rather than by the terminal,
  and a draft is carried over rather than dropped. The gate is matched exactly
  — only `inline` selects it — and with the variable unset output is
  byte-identical to before, so no existing session changes.

  Resizing the terminal re-pins the viewport: the row follows the new bottom
  and the transcript scrolls in the full new height, with your draft intact. If
  a resize leaves the window too short to hold a scroll region plus the row,
  the viewport releases entirely rather than pinning a row it cannot draw, and
  re-installs when there is room again.

  Ctrl+Z suspends from the inline prompt, releasing the pinned row and the
  terminal's scrolling restriction first so the shell is handed back an
  ordinary terminal, then restoring both — and your unsubmitted draft — on
  `fg`. Ctrl+C clears the line and keeps the session, as it does in classic
  mode. Ctrl+\ is deliberately inert, because its usual action dumps core and
  this process holds credentials and conversation text in memory.

  Command history persists across sessions in inline mode, Ctrl+R reverse
  search works with its status line on the pinned row, and Tab completes a
  slash command from the hint beside the draft — the same behaviour the
  during-turn row has.

  Typing a slash prefix opens the **bordered command palette** above the
  composer. It is the same palette the classic prompt draws, from the same
  renderer, so the two cannot drift. The viewport grows to hold it and narrows
  as you type; on a window too short for the full list the box is **built
  smaller** rather than clipped, so it never loses its border; and it closes
  as soon as the prefix stops being one. Resizing the terminal keeps it open at
  the right height.

  Completions at the inline prompt are ranked by command order, so the hint,
  what Tab inserts, and the palette's first entry always agree — typing `/s`
  suggests `save` and opens the palette, where it previously suggested `steer`
  and opened nothing. The during-turn input row is unchanged: while a turn is
  running, commands that act on that turn still come first.

  Still missing from the inline prompt: `@file` path completion, and syntax
  highlighting of the draft.

#### Background agents and herding

- `/jobs view` groups the agent deck by state — agents needing input first,
  then working, then finished results split by whether you have read them,
  then failed and cancelled — with empty groups omitted, so one row that needs
  you does not hide among many that do not.
- `/jobs explain <id>` answers why one agent is in its state, since when, and
  what it is waiting on, drawn from the phase, elapsed time, last activity,
  failure class, steering and handoff already recorded. It is read-only:
  unlike `/jobs show` it never acknowledges a ready result, so diagnosing an
  agent cannot quietly satisfy the guard that refuses to exit on unobserved
  results.
- `/agents` now forwards any subcommand to `/jobs`, instead of rejecting
  everything that is neither empty nor a job id, so both names reach the same
  surface.
- `[agents] state_file` publishes an advisory per-process JSON hint that an
  outside terminal multiplexer can read to tell whether this kitsu is idle,
  working, or blocked, with the same redacted thread records `/jobs` shows.
  `auto`, the default, writes only in an interactive terminal session, so
  scripted, piped, `ask` and JSONL runs write nothing at all.
- `kitsu agents` lists the background agents every kitsu process on this
  machine is publishing, in human text or as JSON with `--json`. Processes
  that have exited are skipped unless `--all`. It is deliberately
  forgiving of the future: a state file written by a newer kitsu is reported
  as schema-unsupported rather than dropped, an unrecognised field does not
  make a file unreadable, and an individual thread record this binary cannot
  validate is counted instead of blanking the whole listing — a newer sibling
  process can never blind the reader. It creates, modifies, and deletes
  nothing.
- `[hooks] on_agent_state` runs a local command when a background agent
  changes state, exporting the agent id, its new state, and what it is waiting
  on. It is executed directly rather than through a shell and performs no
  variable expansion, so configuration text cannot become shell execution; its
  output is discarded so it cannot disturb kitsu's own rendering; and kitsu
  never waits for it, so a slow or failing hook cannot cost a turn.
- The live status row carries a dimmed ambient tag during a turn — `[N blocked]`
  or `[N agent(s)]`, blocked outranking running — so background work stays
  visible after its one-off completion notice has scrolled away.

### Changed
- `/jobs --json` reports `thread.schema_version` as **2**, and a thread object
  may now carry an optional `thread.name`. The version rose because the agent
  phase vocabulary gained a `blocked` value, which a closed enum cannot accept
  without one. `name` is omitted entirely when unset rather than sent as null,
  so existing readers see no new key unless a name exists. Nothing reports
  `blocked` yet — no agent can enter that state until a later change — so the
  only difference visible today is the version number and the optional field.



- The background-agent registry snapshot now also carries an unacknowledged
  count, so a watcher can tell "a result is waiting for you" from "an agent
  finished at some point", which the terminal count alone could not express.


## [0.1.0] - 2026-08-23

First public release.

### Added

#### Gateways and providers

- Config-driven gateway profiles: every endpoint is described in
  `~/.kitsu/config.toml` as `[gateways.<name>]`. There are no hardcoded
  endpoints, keys, or providers in the binary.
- OpenAI-compatible `chat/completions` transport, which covers OpenAI, Hugging
  Face Inference Providers, corporate gateways, and local runtimes such as
  Ollama and vLLM.
- A distinct native Anthropic Messages adapter, including a bounded
  `max_tokens` default and rejection of an explicit zero before send.
- Shipped starter profiles for Ollama, vLLM, a generic custom gateway, Hugging
  Face, Qwen via the Hugging Face router, OpenAI, and Anthropic.
- Credential handling per gateway (`none`, bearer-from-environment, and managed
  storage) with `kitsu auth`, plus refresh hints that describe how to mint a new
  token without ever quoting a secret.
- Model listing (`kitsu models`) and offline diagnostics (`kitsu doctor`) for
  configuration, auth, cache, and runtime.

#### Chat and non-interactive modes

- Interactive multi-turn coding chat (`kitsu`, `kitsu chat`).
- One-shot prompting (`kitsu -p "..."`, `kitsu ask`), piped chat, and scripted
  multi-turn chat.
- `kitsu review` for a tool-free model turn over host-verified repository
  evidence.
- Versioned JSON Lines automation events (`--jsonl`), an atomic
  last-message file (`--output-last-message`), and locally validated structured
  output against a bounded JSON Schema (`--output-schema`).
- Accessibility and terminal-compatibility modes: `--plain`, `--screen-reader`,
  `--reduced-motion`, and the matching `KITSU_*` and `NO_COLOR` variables.

#### Tools and permissions

- Filesystem tools (`fs_read_file`, `fs_list_dir`, `repo_grep`, `fs_edit_file`,
  `fs_write_file`) plus format-specific readers for CSV, PDF, XLSX, DOCX, PPTX,
  notebooks, HTML, and images.
- A permission model with a Balanced default posture: reads inside the launch
  directory are automatic, reads outside it and all writes prompt. Pre-grant
  with `--allow-path`, `--allow-write`, or `--allow-read-write`; read and write
  authority stay independent.
- `--yes` for unattended runs that keeps confinement, the explicit
  `--dangerously-allow-all-filesystem` global bypass, and
  `--dangerously-skip-permissions` (alias `--yolo`) for fully unattended
  automation. Kitsu-private paths and `/permissions protect` guards are enforced
  in every mode.
- Host-owned evidence is separated from model authority: `/diff`, `/review`,
  `kitsu review`, and `/checkpoint` are produced by Kitsu's own `git` and need no
  read grant. The model-callable `workspace_diff` tool exposes the same bounded,
  Kitsu-private-excluding evidence.
- Explicitly enabled developer-command execution with independent command
  authority, command roots, argument validation, containment, and
  secret-like environment filtering. Filesystem permissions never enable or
  approve commands.
- Per-turn undo backed by a write journal that refuses to restore a path whose
  identity, metadata, or contents changed underneath it.

#### Sessions, context, and delegation

- Durable, resumable sessions with branching and recovery (`kitsu sessions`).
- Context budgeting with trim or summarize compaction, project context via
  `llms.txt`, and `@file` attachments bounded by read authority.
- Skills: local instruction bundles the model loads on demand.
- Subagents: synchronous delegation, parallel fan-out, and detached background
  agents with explicit result handoff (depth-1; subagents cannot spawn further).
- Telemetry decks for `/status`, `/context window`, `/context sources`, and
  `/usage`, where every value is labelled with its truth and provenance.

#### Packaging

- Release pipeline producing `.deb`, `.rpm`, x86-64 and aarch64 Linux musl
  tarballs, a macOS universal tarball, and a Windows build from a `v*` tag push,
  with a version guard that fails fast when the tag and manifest disagree.

### Fixed

- Background-agent acknowledgement no longer vanishes from release builds. The
  state-mutating `acknowledge_handoff()` call sat inside `debug_assert!`, whose
  expression is not evaluated when debug assertions are off, so in the shipped
  binary a fetched result stayed `Ready` forever and finished agent output was
  re-printed at shutdown. CI now runs the suite in both debug and release,
  because no debug-profile test can observe this class of bug.
- Small local context windows are usable. Kitsu probes the window a runtime is
  actually serving and clamps down to it, advertises a lean tool tier below
  16,384 tokens so the schema set cannot crowd out the system prompt, and no
  longer silently overfills a window that Ollama truncates from the front.
- `fs_edit_file` refuses a replacement whose newlines arrived double-escaped
  rather than writing them verbatim and reporting success.
- `repo_grep` accepts quoted scalars (`"max_count": "100"`), matching every other
  tool argument struct, so a small model does not lose a tool round to a value
  that was never ambiguous.
- A denied read path names the granted roots and, for a relative path, the
  directory it was resolved against, instead of reporting only that the path
  does not exist.

[0.1.0]: https://github.com/KateInTheLoop/kitsu/releases/tag/v0.1.0
