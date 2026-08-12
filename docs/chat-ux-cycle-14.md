# Chat UX dogfood — cycle 14 (claude CLI baseline; UI & functionality focus)

Date: 2026-08-12

Baseline: `11336b0` on `dogfood/codex-ux-cycle-6`. Cycle 14 ran the rotation's
Fable slot, ship-first: session ergonomics landed before the audit so the
auditors exercised them live. Agents: a Fable day-task auditor (docs review of
functionality.md's sessions sections, both CLIs, plus a full walkthrough of
the new naming UX), a Haiku config-journey critic (init/doctor/broken-config
recovery vs claude), and — added mid-cycle at the maintainer's request — a
Sonnet Hugging Face gateway auditor driving kitsu against
router.huggingface.co with a real token.

## Shipped this cycle

**Session naming + cwd-scoped resume (cycle-13 gaps #2/#3).**
`--name <name>` names the live durable session; `kitsu sessions rename
<id|name> [name]` names, renames, or clears one at rest; names render
name-first in listings, resolve in every resume selector, and are refused
when duplicate or reserved (`last`, `auto`, `s-…`). `--resume auto` picks the
newest session recorded in the current working directory (claude `-c`
equivalent). Resume announcements show the name; docs updated.

**Config typo hardening (config-journey audit #1, a real latent bug).**
Unknown keys inside `[gateways.*]` and `auth` blocks are now rejected loudly
(serde `deny_unknown_fields`); the parse error surfaces serde's
"unknown field `token_env`, expected one of …" hint — field NAMES only,
values still never echoed. Previously `token_env = "X"` was silently ignored
and auth fell back to `$KITSU_TOKEN`.

**Doctor terminal guidance (config-journey audit #4).** A healthy doctor run
now ends "no blocking issues found — you're ready: run `kitsu chat`".

**Naming polish from the day-task audit (same cycle):** a mistyped session
name now gets the friendly "no session named 'X'" error instead of leaking a
raw filesystem path (id-shaped selectors alone may fall through to direct
reads); exact-id commands (`delete`/`archive`/`restore`) tell you how to look
up the id when handed a name; empty-list hints teach `--name`;
`sessions resume --help` documents name/auto.

Gates: fmt clean, clippy clean, workspace green (232 bin / 121 CLI with the
known PTY-environmental failure unchanged / 89 core / 138 tools).

## Task results (docs-review of sessions docs, identical prompt both runs)

- **Run A, `claude -p`:** 74.2 s — fixed exactly the 2 pre-identified stale
  claims, explicitly declared what it left alone. Clean scoped diff.
- **Run B, kitsu + qwen:** 296.2 s, 0 edits — pure model failure (delegation
  loop, then "no changes required" confabulation against a doc with 3 real
  stale claims). Kitsu receipts stayed truthful ("0 file(s) written").
- **New-UX walkthrough:** naming judged "genuinely good" — announcement,
  name-first listing, teaching footer, rename-by-current-name, `auto` ≈
  claude `-c`. Resume-by-name worked (126 s turn); `--resume auto` 1.9 s.

## Loop-infrastructure note

The cycle stalled ~2 h mid-audit on permission prompts: `Bash(*)`-style
allowlist rules match nothing — the valid allow-everything form is the bare
tool name (`"Bash"`, `"Edit"`, …). Fixed in the loop's local settings and
recorded in loop memory; agent worktrees get the file re-seeded at launch.

## Config-journey verdict (Haiku critic)

Kitsu's doctor judged more comprehensive than `claude doctor`; fresh-start
and happy-path journeys clean; broken-TOML and wrong-endpoint errors
actionable. Gaps shipped this cycle: silent unknown-key acceptance (#1,
fixed), missing next-step guidance (#4, fixed). Remaining: offline doctor
cannot catch dead endpoints (suggests `doctor --online` — standing G6), auth
method names unexplained in errors, safe-redacted source excerpts for parse
errors.

## Hugging Face gateway audit (Sonnet, added mid-cycle)

**Headline: the acid test is positive.** HF-routed
`meta-llama/Llama-3.3-70B-Instruct` emitted genuine structured `tool_calls`
every time (clean `◆ Read(notes.txt)` narration), self-corrected through two
tool errors, and landed a correct, verified multi-step file edit — a tier the
local-qwen baseline never reached. Cycles 11–13's text-protocol pain is a
local-model ceiling, not a kitsu ceiling. Catalog, chat, JSONL, and error
paths all work through router.huggingface.co with a single `HF_TOKEN`.

Kitsu-side gaps it surfaced (ranked): (1) the HF profile's `refresh_hint`
self-suppresses on 401 — its text trips kitsu's own credential-leak filter
(`://` + `hf_`), so the user sees no hint at all; (2) the
context-window-mismatch heuristic false-fires on nearly every HF turn because
the profile lacks `context_window` and the 8192 local-model default is wrong
for hosted 70B/120B models; (3) provider 400/404 bodies are swallowed whole —
model-not-found vs no-provider-serves-it are indistinguishable; (4) mismatch
warnings quote token counts from a different round than the adjacent
`kitsu-usage` line, unlabeled; (5) even the 70B spawned a subagent for a
trivial pong prompt — the over-tooling pull is not qwen-specific.

## Ranked gaps (queued for cycle 15)

1. **Interactive resume picker** (carried, now unblocked by naming): bare
   `--resume` on a TTY should offer the session list.
2. **Bare-JSON skill invocations** commit verbatim as answers with no
   "ignored" receipt — extend text-protocol recovery to recognize skill
   names (`kitsu-core/src/orchestrator.rs`).
3. **Deadline-burn visibility** (carried): heartbeat should show elapsed
   against --timeout-secs.
4. Config polish: explain auth methods in the unsupported-method error;
   safe-redacted excerpt for TOML parse errors.
5. HF-path fixes: un-suppress the HF `refresh_hint` (reword hint or narrow
   the leak filter), ship a real `context_window` in the HF profile, bounded
   sanitized excerpts for provider 4xx bodies, round-labeled mismatch
   warnings.

## Standing backlog (unchanged)

final_summary usage totals, piped-session hint, subagent narration on
stdout, help fatigue, branding/examples/init items, G5 receipt elision, G6
doctor --online.

## Report index

1. [Docs-review day task + naming walkthrough](chat-ux-cycle-14-audits/raw/01-docs-sessions.md)
2. [Config journey](chat-ux-cycle-14-audits/raw/02-config-journey.md)
3. [HF Inference Providers gateway](chat-ux-cycle-14-audits/raw/03-hf-gateway.md)

## Loop state

2-hour local cron loop live. Rotation: cycle 14 = Fable (this file), cycle
15 = Opus. Cycle 15 starts with the interactive resume picker and the
skill-JSON recovery extension.
