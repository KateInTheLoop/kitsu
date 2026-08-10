# Audit 10: Codex developer-experience comparison

Baseline: `dead112` on `dogfood/top10-improvements`. Read-only audit; no hosted inference. Codex claims use only the [official OpenAI Developer commands documentation](https://learn.chatgpt.com/docs/developer-commands?surface=cli).

## Hands-on Kitsu evidence

- Top-level help exposes only `ask`, `models`, and `chat`; `kitsu sessions --help` is unrecognized.
- The PTY harness passed exit, queue-notice, FIFO execution, two-response, and `--no-session` checks.
- `/status`, `/permissions`, `/tools`, `/queue`, `/agents`, `/models`, `/model`, `/diff`, `/review`, `/compact`, `/sessions`, `/new`, and `/fork` produced coherent results.
- `/ps` suggested `/new`, and `/stop` suggested `/help`; both suggestions are misleading.
- Empty-home `/resume` leaked a long missing-directory filesystem error instead of a friendly empty state.
- Persistent save/fork/archive round-trip was not completed within the audit timebox.

## Strengths

- `/status` gives unusually strong local operational transparency.
- `/diff` is verified from Git/filesystem evidence, and `/review` refuses to invent work on a clean tree.
- Model discovery/filtering and invalid-model recovery are direct.
- The bounded eight-entry FIFO passed PTY dogfooding.
- Developer-command authority is independent, literal, and exact.
- `/compact` exposes meaningful context numbers.

## Severity-ranked gaps

1. **Critical: `--yes` has dangerously surprising filesystem semantics.** Once filesystem tools are registered it removes confinement to supplied roots. Codex gives global bypass an unmistakably dangerous name; Kitsu should preserve root boundaries under `--yes`.
2. **High: no active-turn steering channel.** Codex distinguishes current-turn injection from next-turn queueing; Kitsu's tested type-ahead waits in one FIFO.
3. **High: session continuity lacks first-class navigation.** Kitsu has exact-ID flows, no top-level session command family or picker, and a poor empty-resume error.
4. **High: background work lacks unified inspection and cancellation.** `/agents` has no `/ps`/`/stop` equivalent, recent-output tail, or thread selector.
5. **Medium-high: automation is fragmented.** `ask`, `chat --script`, and JSONL do not form a unified resumable noninteractive workflow with final-output/schema contracts.
6. **Medium-high: review targeting is narrow.** `/review [path]` cannot target a base branch, commit, uncommitted scope, or custom instructions.
7. **Medium: `/permissions` is inspection-only.** It cannot switch safe presets in-session.
8. **Medium: discovery remains text/ID driven rather than selector driven.** Models, sessions, permissions, and agents lack searchable pickers.
9. **Low-medium: unknown-command suggestions are syntactic rather than semantic.** `/ps -> /new` and `/stop -> /help` are worse than no suggestion.
10. **Low-medium: no provider usage/rate-limit surface.** Local status is excellent but has no analogous provider view.

## Top five

1. Make `--yes` preserve configured roots; add a separately named global filesystem-bypass flag and safe permission presets.
2. Separate active-turn steering from next-turn FIFO queueing.
3. Add searchable session selection, names, friendly empty states, and top-level lifecycle commands.
4. Build a unified `/jobs` or `/ps` view for agents and commands with output, state, selection, and cancellation.
5. Add a first-class noninteractive execution/review surface with JSONL resume, final-output/schema support, and richer review targets.
