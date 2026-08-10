# Audit #10 — dedicated Codex DX comparison at `dead112`

Verdict: Kitsu has strong underlying safety, context, diff, model, and queue machinery, but Codex remains noticeably better at steering ongoing work and navigating sessions/tasks. The most urgent issue is Kitsu’s surprising `--yes` contract: it removes filesystem-root confinement once filesystem tools are registered.

All Codex claims below come only from the [official OpenAI Developer commands page](https://learn.chatgpt.com/docs/developer-commands?surface=cli).

## Exact hands-on Kitsu evidence

Repository remained clean on `dogfood/top10-improvements` at `dead112`.

- `target/debug/kitsu --help`
  - Only top-level commands are `ask`, `models`, and `chat`.
  - `--yes` explicitly says that, once a filesystem tool is registered, every path it touches is approved “with NO confinement” to `--allow-path`/`--allow-write`.
- `target/debug/kitsu chat --help`
  - Exposes `--script`, `--jsonl`, `--resume`, `--no-session`, guarded command roots, and exact JSON-argv command grants.
- `target/debug/kitsu sessions --help`
  - Exit 1: `unrecognized subcommand 'sessions'`.
- `python3 /tmp/kitsu-pty-client.py`
  - Passed all checks:
    - `exit_zero`
    - `captured_notice`
    - `queued_execution`
    - `two_responses`
    - `no_session_state`
  - This proves type-ahead FIFO behavior against the localhost mock without hosted inference.
- Interactive `/help`
  - Clear, aligned descriptions for 31 commands.
- Interactive `/status`
  - Reported build `dead112`, session ID state, gateway, model, Git branch/change count, config, private-state path, endpoint/auth, context usage, compaction, tools, permissions, commands, and agents.
- `/permissions`
  - Clearly showed ask mode, read/write roots, protected writes, and command status.
- `/tools`
  - Listed nine tools, including filesystem tools and foreground/background agent tools.
- `/queue`
  - Reported `0 / 8 prompts queued`.
- `/agents`
  - Reported no background agents.
- `/ps`
  - `unknown command: /ps - did you mean /new?`
- `/stop`
  - `unknown command: /stop - did you mean /help?`
  - Those suggestions are actively misleading.
- `/models mock`
  - Found `mock-developer-model`.
- `/model nope`
  - Rejected the unknown ID and directed the user to `/models`.
- `/model mock-developer-model`
  - Switched successfully and disclosed that the configured context window remained pinned.
- `/diff`
  - `verified workspace diff (1 line(s)): (working tree clean)`.
- `/review`
  - Correctly refused: `nothing to review: working tree clean`.
- `/compact`
  - Clearly reported that nothing was eligible and included current/target tokens.
- `/sessions`
  - Friendly `no saved sessions`.
- `/new` under `--no-session`
  - Started a fresh conversation and explicitly said autosave was disabled.
- `/fork` under `--no-session`
  - Correctly refused because autosave was disabled.
- `/resume` with an empty temporary home
  - Leaked a long missing-directory filesystem error instead of saying there were no resumable sessions.

I did not complete a persistent save/fork/archive round-trip before the timebox; session parity conclusions beyond the above are based on exposed command surfaces, not a claimed runtime pass.

## What Kitsu already does well

- `/status` is arguably stronger than the documented Codex status for local operational transparency.
- `/diff` uses verified Git/filesystem evidence and covers staged, unstaged, and untracked state—the same important scope OpenAI documents for Codex.
- `/review` refuses to invent a review when the tree is clean.
- Model discovery/filtering and invalid-model handling are direct and understandable.
- The eight-entry FIFO is real, bounded, visible, and passed PTY dogfooding.
- Developer commands have an excellent independent authority model: literal argv, explicit roots, and exact noninteractive grants.
- `/compact` and context telemetry expose meaningful numbers rather than a generic success message.

## Severity-ranked gaps

1. **Critical — `--yes` has dangerously surprising filesystem semantics.**
   It removes confinement to supplied roots after tools are registered. The warning is explicit, but the familiar name suggests “skip prompts,” not “allow every non-protected path.” Official Codex documents a separately named `--dangerously-bypass-approvals-and-sandbox`/`--yolo` mode. Kitsu should preserve root confinement under `--yes` and require an unmistakably dangerous flag for global approval.

2. **High — No active-turn steering channel.**
   Codex officially distinguishes Enter-to-inject into the current turn from Tab-to-queue the next turn. Kitsu’s tested behavior queues complete type-ahead lines for a later turn.
   **Inference:** urgent corrections therefore consume the rest of the current inference and an additional turn instead of redirecting work immediately.

3. **High — Session continuity lacks first-class navigation.**
   Codex documents searchable `/resume`, current-chat `/fork`, `/rename`, `/side`, current-session `/archive`, plus top-level resume/fork/archive commands. Kitsu uses exact IDs and has no top-level session command family or picker. Its empty `/resume` error exposes an internal path.

4. **High — Background work is not controllable enough.**
   Codex documents `/ps` with recent output and `/stop` cancellation, plus `/agent` thread switching and `/side`. Kitsu offers `/agents [id]`, but no unified job view, output tail, stop control, or active-thread selector. The bad `/ps` and `/stop` suggestions make discovery worse.

5. **Medium-high — Automation is fragmented.**
   Kitsu exposes `ask`, `chat --script`, and `--jsonl`; Codex documents one `exec` workflow with JSONL, `exec resume`, final-message output, and output-schema validation. Kitsu lacks equivalent resumable noninteractive continuity and structured final-output contracts.

6. **Medium-high — Review targeting is narrow.**
   Kitsu `/review [path]` handles the current working tree. Codex documents `review --base`, `--commit`, `--uncommitted`, and custom review instructions.

7. **Medium — `/permissions` is inspection-only.**
   `/permissions auto` produces usage text. Codex documents an in-session picker for Auto, Read Only, and named permission profiles.

8. **Medium — Discovery is textual rather than selector-driven.**
   Kitsu’s help and Tab completion are good, but models, sessions, permissions, and agents lack searchable pickers.
   **Inference:** exact-ID workflows will become cumbersome with large Hugging Face catalogs or many sessions.

9. **Low-medium — Command suggestions need semantic filtering.**
   `/ps → /new` and `/stop → /help` are more confusing than a plain “unknown command.”

10. **Low-medium — No provider usage/rate-limit surface.**
    Codex documents rate-limit/statusline visibility. Kitsu’s excellent local status has no analogous provider-usage view.

## Top five urgent parity improvements

1. Make `--yes` preserve configured roots; add a separately named, strongly warned global filesystem-bypass mode, and let `/permissions` switch safe presets.
2. Add distinct active-turn steering and next-turn queue controls, with visible key hints and state.
3. Add searchable session selection, names, friendly empty states, and top-level `sessions/resume/fork/archive/restore/delete` commands.
4. Build a unified `/jobs` or `/ps` interface covering agents and commands, with recent output, state, selection, and cancellation.
5. Add a first-class noninteractive `exec`/`review` surface with JSONL resume, final-output/schema support, and base/commit/uncommitted review targets.

No repository files were edited and no commit was created.
