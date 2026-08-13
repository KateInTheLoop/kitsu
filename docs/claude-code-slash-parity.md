# Slash-command parity: kitsu vs Claude Code

Date: 2026-08-12. Baseline: Claude Code CLI's most-used slash commands,
mapped against kitsu's registry (the `/help` single source of truth).
Shipped in this pass: `/cost` and `/loop`.

## Parity table

| Claude Code | kitsu | Status |
|---|---|---|
| `/help` | `/help` | ✅ parity (grouped registry, aliases folded) |
| `/clear` | `/new` | ✅ parity (fresh session, grants reset) |
| `/compact` | `/compact` | ✅ parity (+ explicit trim/summarize modes) |
| `/context` | `/context [refresh]` | ✅ parity+ (provenance and cache-state disclosure) |
| `/cost` | `/cost` | ✅ NEW — cumulative session tokens across turns; kitsu meters tokens, not currency (gateway pricing varies; provider-side prompt-cache discounts are not itemized on the OpenAI-compatible wire) |
| `/status` | `/status` | ✅ parity+ (session, endpoint, window provenance, grants) |
| `/model` | `/model` | ✅ parity (live window re-discovery on switch) |
| `/permissions` | `/permissions` | ✅ parity+ (grants, protected paths, command containment) |
| `/resume` | `/resume <id\|name\|last\|auto>` | ✅ parity+ (names, cwd-scoped auto); interactive picker still queued |
| `/rewind` | `/undo`, `/checkpoints` | ✅ parity (journal-verified checkpoints) |
| `/export` | `/export` + `sessions export` | ✅ parity (markdown/JSON) |
| `/agents` | `/agents`, `/jobs`, `/agent` | ✅ parity+ (deck, steering, per-thread control) |
| `/add-dir` | `/allow`, `/allow-write`, `/allow-read-write` | ✅ parity (finer-grained) |
| `/review` | `kitsu review` + `/review` | ✅ parity (host-verified evidence) |
| `/doctor` | `kitsu doctor` (CLI only) | 🔶 partial — `/status` covers session health; in-session doctor queued |
| `/init` | `kitsu init` (CLI only) | 🔶 partial — config bootstrap is a CLI concern by design |
| `/memory` | project context (llms.txt/AGENTS.md, auto-loaded) | 🔶 partial — no in-session memory editor; queued for evaluation |
| `/vim` | — | ❌ not planned (no modal editor) |
| `/mcp` | — | ❌ out of scope until kitsu grows MCP |
| `/hooks` | — | ❌ out of scope (no hook system yet) |
| `/login` | gateway auth via env/config | ➖ different model (no account) |
| (loop workflows) | `/loop <count> <prompt>` | ✅ NEW — queues N copies (2-20) through the prompt queue; interactive chat only (scripts repeat lines natively) |

## Shipped in this pass

- **`/cost`** — cumulative `prompt + completion = total` tokens and turn
  count for the session (accumulated in `AgentSessionState.run_usage` by the
  orchestrator), with an honest note about currency and cache discounts.
- **`/loop <count> <prompt>`** — enqueues N copies of a plain prompt through
  the existing bounded prompt queue (capacity-aware, partial-queue reported,
  `/queue` inspects, `/queue clear` aborts). Interactive-only by the queue's
  design; scripted runs express loops as repeated lines.

## Queued next (parity backlog)

1. Interactive `/resume` picker (long-standing).
2. In-session `/doctor` (read-only diagnostics without leaving chat).
3. `/memory`-equivalent: view/edit the discovered project context in-session.
4. Timer-based `/loop` (every N minutes) — needs REPL timer infrastructure;
   the count-based form ships first deliberately.
