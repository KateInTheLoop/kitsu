# Chat UX dogfood — cycle 12 (claude CLI baseline; UI & functionality focus)

Date: 2026-08-12

Baseline: `22a9f71` on `dogfood/codex-ux-cycle-6`. Cycle 12 ran the rotation's
Sonnet slot with a new agent composition: a Sonnet day-task auditor (coding
task — add a unit test for `task_preview` — on both CLIs, kitsu driving local
qwen2.5-coder) and a Haiku automation-protocol critic (kitsu `--jsonl` vs
`claude -p --output-format stream-json`, as a CI-embedding decision).

## Task results (add-a-unit-test, identical prompt both runs)

- **Run A, `claude -p`:** 76.1 s, test written and genuinely passing.
- **Run B, kitsu + qwen (default, no commands):** 97.5 s, zero diff — the
  model could not run cargo and never wrote the test.
- **Run B2, kitsu + qwen (`--enable-commands`):** hit the full 600 s deadline
  with zero diff — the command sandbox fakes `$HOME`, so rustup/cargo cannot
  find a toolchain (see gap 1).

## Shipped this cycle

**Non-interactive run receipt (cycle-11 gap #1).** Every piped and scripted
chat run now ends with a one-line stderr receipt:
`~ run summary: N turn(s) ok, K file(s) written (M change(s))[, A background
agent(s) (F failed)], session <id|not saved>`. Zero-count qualifiers stay
silent so a clean exit does not read as an incident. Cycle 11's
silence-as-success failure mode (3m37s no-op exiting 0 wordlessly) is now
impossible. Scripted runs print it from `finalize_scripted_run` (success and
failure); piped runs on the committed path.

**`final_summary.session_id` (cycle-12 automation audit #3).** The JSONL
terminal frame now carries the durable session id (`null` when nothing was
persisted) — a CI consumer learns where the conversation went from the frame
it already parses. Documented in docs/jsonl-protocol.md.

Gates: fmt clean, clippy clean, workspace tests green (230 bin / 121 CLI with
the known PTY-environmental failure unchanged / 86 core).

## Automation-protocol verdict (Haiku critic)

Kitsu is AHEAD of claude CLI on: exit-code semantics (0/1/2 vs always-0),
explicit per-event schema versioning (`kitsu.chat.v1`), retry transparency,
and payload economy (2.4 KB vs 6.9 KB for the same prompt). Behind on: cost
visibility (no `cost_usd`/token fields in `final_summary`), per-attempt model
metadata, and a consumer-ready `final_answer` field.

## Ranked new gaps (queued for cycle 13)

1. **Command sandbox fakes `$HOME`, breaking toolchains** — rustup/cargo can
   never resolve under `--enable-commands`, which defeats the edit→test→fix
   loop entirely (ties into standing G2b). `kitsu-tools/src/builtins/command.rs`.
2. **Text-protocol tool-call recovery hard-fails on non-standard JSON
   escapes** (e.g. `\'`) and dumps raw JSON as the answer — worse, malformed
   `spawn_agent` attempts bypass the new spawn-cap accounting entirely.
   `kitsu-core/src/orchestrator.rs` (`parse_text_tool_calls`).
3. **No cumulative deadline visibility** — heartbeats back off per round but
   never show `--timeout-secs` budget burn before the hard cutoff (Run B2
   died at 600 s with no warning). `kitsu-render/src/lib.rs`.
4. **`final_summary` lacks usage/cost totals** (automation #1) — add token
   totals (cost needs per-gateway pricing the config does not know).
5. **Content-free tool receipts** (`◇ 15 lines`) for plain tools, and
   `final_answer` missing from `final_summary` (automation #5).

## Standing backlog (unchanged)

G5 receipt path elision, G6 `doctor --online` window evidence, cycle-11 gaps
#2 piped-session hint / #3 subagent narration on stdout / #6 help fatigue,
Haiku cycle-11 branding/examples/init items.

## Report index

1. [Unit-test day task, claude vs kitsu](chat-ux-cycle-12-audits/raw/01-test-task.md)
2. [Automation protocol: JSONL vs stream-json](chat-ux-cycle-12-audits/raw/02-automation-protocol.md)

## Loop state

2-hour local cron loop live. Rotation: cycle 12 = Sonnet (this file), cycle
13 = Haiku. Cycle 13 starts with gap 1 (sandbox `$HOME`/toolchain) — it
blocks the core dogfood workflow — and gap 2's escape-tolerant recovery.
Haiku fix passes get tighter review per standing note.
