# Chat UX dogfood — cycle 11 (claude CLI baseline; UI & functionality focus)

Date: 2026-08-11

Baseline: `4174146` on `dogfood/codex-ux-cycle-6`. Cycle 11 ran on the model
rotation's Opus slot and, for the first time, compared kitsu against **Claude
Code CLI** (previous cycles used Codex CLI). Two auditors ran in parallel: an
Opus day-task auditor (documentation-accuracy review of README's `ask`/`review`
sections, performed on both CLIs; kitsu drove local qwen2.5-coder via Ollama)
and a Haiku fresh-eyes critic (first-contact audit of the non-LLM surfaces:
help, doctor, init, errors, sessions/artifacts, branding).

## Task results (docs-review, identical prompt both runs)

- **Run A, `claude -p`:** 83.9 s, 2 real README fixes.
- **Run B, kitsu + qwen2.5-coder:** 217.0 s, exit 0, **zero edits, no
  receipt** — a silent no-op that looked like success.

## Shipped this cycle

**G3 — per-turn agent spawn cap (carried from cycle 10).** New core type
`SpawnBudget` (kitsu-core): the orchestrator resets it at each
main-conversation turn (`AgentConfig.spawn_budget`; subagent configs carry
none, so nested turns never refresh the parent's). All three spawn tools
(`spawn_agent`, `spawn_agents`, `spawn_agent_bg`) draw from it; exhaustion
returns a model-visible tool error phrased as an instruction ("do the
remaining work yourself, inline"). `spawn_agents` admits what fits and labels
refused tasks individually in the collected output. Config key
`agent_spawn_cap`, flag `--agent-spawn-cap N` (0 disables spawning). Defaults:
8 interactive, **2 scripted/piped** — cycle 10 showed qwen spawning 6
background agents on a read-only planning turn. Tests: budget
admit/saturate/reset (core), cap-refusal and partial-admission (bin).

**First-contact torii greeting (Haiku audit #4 + standing "⛩ never appears in
real work" backlog).** Interactive chat now opens with one milestone line at
the session gate: `⛩ kitsu chat — <model> via <gateway> · /help for commands ·
Ctrl+D exits`. Non-interactive modes are untouched.

Gates: fmt clean, clippy clean, workspace tests green (230 bin / 121 CLI with
the known PTY-environmental failure unchanged / 86 core / 58 provider / 137
tools / 31 render / 2 provenance).

## Ranked gaps vs claude CLI (audit synthesis, queued for cycle 12)

1. **No end-of-run receipt in non-interactive runs** — a 3m37s no-op exits 0
   silently. Print a `~ run summary: N rounds, M tool calls, K files written…`
   line on every non-interactive exit. (Opus #1; the direct cause of Run B's
   silence-as-success.)
2. **Piped chat discards the conversation** — no session id, no resume hint.
   Auto-persist or say `session not saved — rerun with --session`. (Opus #2)
3. **Foreground subagent narration is unlabeled and on stdout**, corrupting
   the answer channel — quiet foreground subagents, re-emit labelled `◆
   agent#1 ▸` progress on stderr. (Opus #3)
4. **Text-protocol tool calls from small models are dumped as the answer**
   (raw `{"name":…}` JSON as prose; reproduced live during this cycle's smoke
   test) — suppress and surface `ignored a text-form tool call…`. (Opus #4)
5. **Round-blind progress + content-free receipts** — heartbeat restarts each
   round; `◇ 2 lines` for a 3-minute agent call. Turn-scoped heartbeat
   (`round 3/40, 2m10s`) and previews/durations in receipts. (Opus #5)
6. **Help-screen fatigue** — every subcommand's help repeats 73+ lines of
   global options; factor into a footer reference. (Haiku #1)
7. Remaining Haiku items: branding invisible outside chat; no examples for
   complex flags; `init` gives no gateway-selection guidance after writing the
   config.

## Standing backlog (unchanged)

G5 receipt path elision, G6 `doctor --online` context-window evidence, G2b
pip user-site mount, syntect code boxes on the streaming path, agent-deck
front door, live-turn /jobs output, screen-reader mode, stdout deliverable in
human one-shot mode.

## Report index

1. [Docs-review day task, claude vs kitsu](chat-ux-cycle-11-audits/raw/01-docs-review.md)
2. [First-contact critique of non-LLM surfaces](chat-ux-cycle-11-audits/raw/02-first-contact.md)

## Loop state

2-hour loop live (local cron, session-scoped). Model rotation: cycle 11 =
Opus (this file), cycle 12 = Sonnet. Cycle 12 starts with gap #1 (run-summary
receipt) — it is the top item from both the audit ranking and the live
reproduction.
