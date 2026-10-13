# Changelog

All notable changes to kitsu are documented here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and
this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).
While the major version is `0`, the CLI surface, config schema, and JSONL event
shapes may change in any minor release.

## [Unreleased]

### Added

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
