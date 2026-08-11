# Chat UX dogfood — cycle 9 audit checkpoint

Date: 2026-08-11

Baseline: `5a4ff51` on `dogfood/codex-ux-cycle-6`. Two auditors (running on
Haiku per the cycle model rotation, with intentionally small scopes):
first-run onboarding after cycle 8's `kitsu init` change plus context-
compaction UX, and an "explain this diff" parity comparison against Codex CLI
(2 small runs) plus command-discovery polish. Kitsu runs on local Ollama
`qwen2.5-coder:latest`. Auditors made no repository edits.

## Validated

Compaction UX passed end-to-end: trim is appropriately silent, summarize
shows the ⛩ milestone, `/context` provenance reconciles before and after
`/compact`, eligibility logic is correct, and JSONL compaction events match
the protocol doc. Onboarding is accurate post-cycle-8: the generated config
works, doctor output is truthful offline and online, and the model-not-found
path gives a working remedy. `kitsu review` beat Codex on ergonomics for the
diff-explain task: one command, host-verified auto-evidence, no flags.

## Findings (cycle-9 fix scope)

1. **Piped chat has no evidence path for "explain my changes" (high, design
   needed)**: the same question that `kitsu review` answers in one command
   leaves piped `chat` blind — the model cannot see the diff, flails, spawns
   agents, and times out. Deferred to a Fable/Opus cycle: design a bounded
   `workspace_diff` model-callable tool reusing `WorkspaceEvidenceAuthority`
   with the same provider-disclosure contract. Recorded in backlog.
2. **Small models misuse `spawn_agent_bg` (medium)**: both auditors watched
   qwen spawn background agents for trivial requests, burning rounds and
   timeouts. Add one system-prompt line scoping agent spawning to explicitly
   parallel or long-running work.
3. **List-surface consistency (medium)**: empty states mix tenses and only
   some teach the next step; `/jobs list` uses `#`, `/queue list` uses `~`,
   `/checkpoints` uses nothing — no shared identity per docs/ux-design.md.
4. **Typo suggestions stop at top level (low)**: `/hlep` suggests `/help`,
   but `/jobs lst` prints bare usage with no close-match suggestion.
5. **`/help` column alignment breaks on long-arg commands (low).**
6. **Onboarding nit (low)**: the generated config's `model = "llama3.2"`
   blocks the quick start when that model is not pulled; the error remedy
   works but the generated file itself could name the escape hatch.

Also noted (model-fault ledger, no action): codex caught a pre-existing type
error kitsu's review missed — static-analysis depth is the local model's
limit, not a CLI defect.

## Cycle-9 fix scope

Items 2–6 (all Haiku-sized): system-prompt spawn guidance; a consistency
pass over empty states, list glyphs, subcommand typo suggestions, and /help
alignment; an explanatory comment in the generated config. Plus session
search from the standing backlog: `kitsu sessions [filter]` and `/sessions
[filter]` substring filtering, mirroring `/models`. Item 1 goes to backlog
for a high-capability cycle.

## Implementation outcome

One Haiku fix pass closed items 2–6 plus session search (this commit): the
system prompt scopes `spawn_agent_bg` to explicitly parallel or long-running
work when the tool is registered; empty states across list surfaces follow
one "what is empty — how to populate it" pattern; `/checkpoints` and
`/queue list` adopt the ◈ deck header design (`--plain` ASCII unchanged);
`/jobs` subaction typos get close-match suggestions; `/help` caps and wraps
the syntax column; `kitsu sessions [filter]` and `/sessions [filter]` narrow
by case-insensitive substring; and the generated config documents its model
line. Maintainer review corrected two items before commit: the `/jobs` empty
state no longer instructs the user to call a model-only tool (the cycle-7
anti-pattern), and the README session rows were updated. Known scope trims:
subaction suggestions landed for `/jobs` only (`/queue` deferred), and test
coverage for items 4–6 is thinner than the pass-A–D standard.

## Release verification

Final tree: fmt clean, workspace clippy clean (`-D warnings`), 644 passing
tests (218 bin unit, 116 CLI integration, 84 core, 58 provider, 136
tools/permissions, 30 render, 2 provenance); the known PTY-environmental
failure remains pre-existing. Gates were rerun by the maintainer after the
two corrections.

## Report index

1. [Onboarding and compaction](chat-ux-cycle-9-audits/raw/01-onboarding-compaction.md)
2. [Diff explain and discovery](chat-ux-cycle-9-audits/raw/02-diff-discovery.md)

## Checkpoint

Committed before implementation. Model rotation: cycle 9 = Haiku;
cycle 10 = Fable.
