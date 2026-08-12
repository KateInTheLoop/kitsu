# Chat UX dogfood — cycle 8 audit checkpoint

Date: 2026-08-11

Baseline: `c66af4b` on `dogfood/codex-ux-cycle-6`. Three auditors (running on
Sonnet per the cycle model rotation): a post-ship validation of everything
cycles 6–7 shipped, a debugging-workflow parity comparison against Codex CLI
(2 small authenticated runs), and a documentation-drift audit verifying doc
claims against the live binary. Kitsu runs used local Ollama
`qwen2.5-coder:latest` under heavy host contention; model latency/quality was
scored as environmental. Auditors made no repository edits.

## Post-ship validation: clean bill

Every cycle-6 and cycle-7 headline fix was confirmed working live, including
profile spawns on Ollama, joined script exits, clean broken pipes, host-
evidence disclosure matching docs/jsonl-protocol.md byte-for-byte, object-form
pregrants surviving `/new` (verified through a real interactive pty), the full
artifacts round trip against a planted orphan, `committed_partial`, the
context-window mismatch warning (fired against the real 8192/4096 mismatch),
and zero-ESC `--plain`/`NO_COLOR`. No functional defects found.

## New findings (cycle-8 fix scope)

### 1. Contained commands get an unusable environment (critical)

Mandatory Linux containment runs approved commands with a fixed environment so
minimal that `python3 -m pytest` fails with `ModuleNotFoundError` even when
every relevant directory is an approved command root. Developer toolchains
need a workable-but-safe environment: extend the fixed set with the
non-secret basics toolchains require and add an explicit opt-in pass-through
for named variables, keeping deny-by-default for everything else.

### 2. Pregranted commands are invisible to the model (high)

Neither the tool schema nor the system prompt mentions the exact argv/cwd
grants supplied via `--allow-command`, so the model guesses — and skipped
running the test suite entirely in the audit. Disclose the pre-approved
commands in the system prompt alongside the cwd/roots disclosure added in
cycle 6 (this also closes cycle-7 git-workflow gap 6).

### 3. Containment's binary-dir rule is undocumented (high)

A second rule — the executable's own directory must separately be an approved
root — surfaces only as a runtime `PolicyDenied`. The denial must name the
missing root and the flag to grant it, and the rule belongs in README and
`--help`.

### 4. `--resume` + `--session` is a hard error the README contradicts (med)

README says resume implies durable continuation; the CLI rejects the
combination. Make the flags compose (explicit `--session` with `--resume` is
a no-op confirmation, not an error).

### 5. Documentation drift (ten mechanical corrections)

Worst items from the docs audit: the README permission-prompt transcript
still shows the Claude Code `⏺` glyph that docs/ux-design.md explicitly
disclaims (live UI: `◉ permission`); `--agent-join-timeout` missing from the
README global-flags table; `docs/agent_deck_ui.md` describes a split-pane UI
that was never built and contradicts the implemented inline deck;
`/quit`/`/exit` primary/alias documented backwards; docs/ux-design.md has no
Agent Deck entry; `kitsu init` emits `flavor = "ollama"`/`127.0.0.1`
disagreeing with every README recipe; `llms.txt` omits `artifacts` and its
doc index is frozen at cycle 3; `/help`'s `/jobs` row omits real subactions;
docs/functionality.md's status date is stale. jsonl-protocol.md and the CLI
reference table held up with no drift.

## Deferred

Long resumed tool-heavy turns remain non-deterministic on a slow local model
even at 10× timeout — environmental at core; revisit only if it reproduces on
a healthy host. The backlogged JSONL file-mutation event was re-scored as not
needed for the debugging workflow. Standing backlog (cycle 7 list) otherwise
unchanged: session search, screen-reader mode, syntect code boxes, agent-deck
front door and live-turn output, deck identity round 2, stdout deliverable in
human one-shot mode.

## Implementation outcome

One Sonnet fix pass closed the cycle scope (this commit):

1. **Containment environment**: root cause was `HOME` redirection breaking
   Python user-site resolution, proven by raw-bwrap reproduction — not a
   missing mount. New repeatable `--command-env NAME[=VALUE]`: bare names are
   read from kitsu's environment and refused for secret-shaped names
   (`*KEY*`/`*TOKEN*`/`*SECRET*`/`*PASSWORD*`, case-insensitive); explicit
   `NAME=value` is always allowed; values merge over the fixed base set and
   are disclosed in `/permissions` and JSONL with secret-shaped values
   redacted. Deny-by-default inheritance unchanged.
2. **Pregrant disclosure**: the system prompt now lists exact pre-approved
   commands with their cwd (capped at 20 with an overflow note), inherited by
   `/new` and resume rebuilds.
3. **Teaching denials**: the executable-outside-roots denial names the
   missing directory and the `--command-root` flag; rule documented.
4. **`--resume` composes with `--session`** as the README promised.
5. **All ten docs-drift corrections applied**, including the README
   permission transcript (`◉`), the `agent_deck_ui.md` rewrite (implemented
   inline deck, future ideas preserved), `kitsu init` config aligned with the
   README recipe and verified end-to-end against a mock endpoint, and a
   refreshed `llms.txt`.

## Release verification

Final tree: fmt clean, workspace clippy clean (`-D warnings`), 642 passing
tests (216 bin unit, 116 CLI integration, 84 core, 58 provider, 136
tools/permissions including real-bwrap reproductions, 30 render, 2
provenance). The known PTY-environmental failure remains pre-existing.
Maintainer review focused on the containment change: secret-name refusal,
disclosure redaction, and the unchanged deny-by-default posture were verified
in the diff and test suite.

## Report index

1. [Post-ship validation](chat-ux-cycle-8-audits/raw/01-postship-validation.md)
2. [Debugging workflow vs Codex](chat-ux-cycle-8-audits/raw/02-debugging-workflow.md)
3. [Documentation drift](chat-ux-cycle-8-audits/raw/03-docs-drift.md)

## Checkpoint

Committed before implementation. Model rotation: cycle 8 = Sonnet;
cycle 9 = Haiku.
