# Chat UX dogfood — cycle 6 audit checkpoint

Date: 2026-08-10

Baseline: `57889b2` on `dogfood/codex-ux-cycle-6`. Four independent auditors
exercised the current Kitsu binary: a hands-on side-by-side comparison with the
locally installed OpenAI Codex CLI 0.147.0 (three small authenticated `codex
exec` runs, read-only sandbox, disposable fixtures), a real-task dogfood pass
(documentation review, scripted bug fix, session lifecycle, failure modes), a
visual-identity audit against the fox/kitsune design intent, and a backlog
verification pass. Kitsu runs used local Ollama `qwen2.5-coder:latest`; the host
CPU was heavily loaded, so model latency/quality deltas were scored as
environmental, not CLI faults. Auditors made no repository edits.

## Cross-validated top findings

### 1. The model is never told its working directory or granted roots

`chat_system_prompt` omits cwd and the granted read/write roots, so a piped
`--allow-path $PWD chat "Explain this codebase"` produces a model that asks the
user for file paths while Codex explores unprompted. Highest-leverage small fix
in the cycle.

Acceptance: with a granted root, a piped "explain this codebase" turn makes at
least one filesystem tool call without the user naming a path; the system prompt
lists cwd and each granted root with its authority level.

### 2. Deadline expiry destroys completed work

`--timeout-secs` is one all-rounds budget; on expiry the whole turn — completed
tool rounds and ~90% of an actively streaming answer — is rolled back and
labeled DISCARDED (exit 2). Codex has no comparable cliff. Preserve completed
rounds/partial text with an honest partial disposition instead of discarding.

Acceptance: a turn that times out mid-stream after successful tool rounds ends
with visible partial output, a disposition that says partial/timeout (not
discarded), and JSONL that lets automation distinguish partial from failed.

### 3. A recovered edit conflict still fails the whole scripted turn

When the model recovers from an `fs_edit_file` precondition conflict and
completes the edit, scripted mode still rolls the turn back and exits 1 — while
the file *did* change on disk and the advertised `/undo 1` checkpoint is
unreachable in a fresh process. `--allow-turn-errors` overcorrects to exit 0
even when everything failed.

Acceptance: a scripted turn whose final tool round succeeds is committed;
checkpoint hints are only printed when actionable from the current entry point.

### 4. Context-window truth is not cross-checked

Config said 8192 while the Ollama server actually served 4096; kitsu printed
`used:4299` next to `prompt_tokens:4096` without warning, silently truncating an
attachment head. Cross-check the resolved window against runtime evidence and
warn when the provider's real window is smaller.

Acceptance: when reported `prompt_tokens` hits a smaller server window than the
resolved one, the next preflight warns and the discrepancy appears in
`/context` provenance.

### 5. Success-path provider telemetry is noise

`~ inference provider attempt 1/3: success; HTTP 200; ...` prints after every
good turn and can fuse onto the last streamed character (missing newline
discipline between stdout stream and stderr status). Retry telemetry belongs on
failure paths (or verbose mode) only.

### 6. Errors are unstyled and frightening

TTY errors print four unstyled lines with scary DISCARDED text and even usage
stats for failed turns, although a styled red notice path already exists in
kitsu-render. Errors should be one weighted red notice plus an actionable hint.

### 7. Kitsu's mascot is a cat, and its glyphs are Claude Code's

The banner art and all six spinner frames are cat kaomoji; the only fox mark is
the 狐 kanji. None of the documented identity glyphs exist (no 🦊, no ⛩, no
`◆ ◇ ◈ ◉` diamonds, one of six palette colors), and the tool/result/compaction
glyphs are `⏺ ⎿ ✻` — Claude Code's exact set — so kitsu reads as a clone.
`docs/ux-design.md`, referenced by design.md as the full UX spec, does not
exist. The design audit ships an 8-item ranked identity pack (fox banner and
spinner, diamond tool glyphs, torii milestones, weighted unified permission
prompt, named palette with amber/ember context thresholds, grouped `/help`)
with exact glyphs, `--plain`/non-TTY fallbacks, and acceptance tests.

### 8–10. Standing backlog confirmed against the current binary

`/diff` still requires widening model read authority (host-owned evidence should
not enter model read roots); `--allow-command` pregrants are hard-bound to the
launch cwd (need an object form with explicit cwd); orphaned `runtime-*`
artifact directories accumulate across runs with no cross-run listing or GC.
All seven `docs/issues/ISSUE-*` items were re-verified as genuinely resolved.

## Also noted

`kitsu review <path>` on a clean tree dead-ends without suggesting
`--commit HEAD` or `@file` chat; `sessions export x.json` writes Markdown by
default; preflight prints three inconsistent budget numbers; `last` can silently
retarget to a fresh fork; sessions remain unsearchable; screen-reader rendering
mode still missing. Parity-or-better confirmations: verified snapshot diffs,
checkpoints and working `/undo`, typed exit codes and JSONL failure taxonomy,
actionable errors, resume story, `--plain`/`--reduced-motion` discipline, and a
streaming escape-sanitizer that beats Codex.

## Cycle-6 implementation scope

Fix pass A (correctness): items 1–4 plus the fused stdout/stderr newline bug.
Fix pass B (identity & polish): items 5–7 via the design audit's ranked pack.
Backlogged for a later cycle: items 8–10, review dead-end hint, export format
inference, preflight budget consistency, session search, screen-reader mode.

## Implementation outcome

Two sequential fix passes closed the cycle scope on this branch:

**Pass A (correctness, committed `7a941fb`):** the chat system prompt names the
cwd and every granted root with its authority and tells the model to explore;
deadline expiry in ask/piped/review preserves partial streamed output with an
explicit `committed_partial` JSONL disposition instead of discarding delivered
work (scripted turns intentionally keep rollback for history consistency);
recovered edit-conflict turns commit, unreachable `/undo` hints are suppressed,
and `--allow-turn-errors` no longer exits 0 when every turn failed; provider
`prompt_tokens` evidence below the resolved context window warns once and shows
in `/context` provenance; streamed stdout is always newline-terminated before
stderr status. 19 regression tests. Descoped: cross-process checkpoint
persistence (the false `/undo` advertisement it caused was removed instead).

**Pass B (fox/kitsune identity, this commit):** success-path provider telemetry
silenced (failures still report); errors render as one weighted EMBER notice
with actionable hints and no failed-turn stats; the documented glyph set is
real — `◆` tool call, `◇` result, `◈` receipt, `◉` approval, `⛩` milestones —
replacing the borrowed `⏺ ⎿ ✻`; the cat mascot is gone: fox kaomoji banner and
spinner frames, 🦊 title, closed box; one weighted unified permission prompt
design for filesystem and command approval (leading-slash display bug fixed);
`/help` renders six headed groups from the single shared registry; the six
documented palette colors exist as named constants with amber/ember context-
meter thresholds. `docs/ux-design.md` now exists as the actual spec.
`--plain`/`NO_COLOR`/non-TTY remain byte-stable ASCII and `kitsu.chat.v1`
JSONL is unaffected. 12 more regression tests. Descoped: syntect code-block
boxes; CREAM reserved.

## Release verification

Final tree: fmt clean, workspace clippy clean (`-D warnings`, all targets and
features), and 493 passing tests (190 bin unit, 2 provenance, 92 CLI
integration, 84 core, 58 provider, 29 render, 130 tools/permissions). One
PTY-dependent test (`test_query_only_interactive_session_does_not_create_a_
snapshot`) fails in this sandboxed environment; it was verified to fail
identically on the untouched baseline `20d57c1` before either pass. Maintainer
spot-checks on a live pty confirmed the fox banner, and that `NO_COLOR` output
contains zero escape sequences. No hosted inference or credentials were used.

## Backlog (maintainer decision)

Carried to a later cycle: decouple host-owned `/diff` from model read
authority; explicit cwd for `--allow-command` pregrants; cross-run artifact
retention/GC; `kitsu review <path>` clean-tree dead-end hint; `sessions export`
format inference; preflight budget-number consistency; session search;
screen-reader rendering mode; syntect code-block boxes.

## Report index

1. [Codex CLI side-by-side](chat-ux-cycle-6-audits/raw/01-codex-parity.md)
2. [Real-task dogfood](chat-ux-cycle-6-audits/raw/02-real-tasks.md)
3. [Fox/kitsune design audit](chat-ux-cycle-6-audits/raw/03-fox-design.md)
4. [Backlog verification](chat-ux-cycle-6-audits/raw/04-backlog.md)

## Checkpoint

This report is committed before implementation so later branch changes can be
reviewed against the independent evidence. Model rotation for spawned agents:
cycle 6 ran on Fable; cycle 7 rotates to Opus, then Sonnet, Haiku, repeat.
