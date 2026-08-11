# Chat UX dogfood — cycle 7 audit checkpoint (interrupted)

Date: 2026-08-11

Baseline: `6c047a0` on `dogfood/codex-ux-cycle-6`. Cycle 7 launched four
auditors (running on Opus per the cycle model rotation): git-centric workflow
parity vs Codex CLI, post-ship validation of cycle 6, an agent-deck UX audit,
and a backlog design spike. The account's monthly spend limit was reached
mid-cycle: three reports were delivered (the post-ship validation auditor
produced none) and no fix pass ran. This checkpoint preserves the evidence;
the implementation pass is deferred to the next cycle.

## Headline findings (unimplemented — next cycle's queue)

1. **CRITICAL: both built-in agent profiles are dead on the default gateway.**
   Profile spawns unconditionally send `reasoning_effort`, which Ollama rejects
   (HTTP 400); the failure is misreported as success. Profile-less spawns work.
2. **The agent deck has no user front door and a hostile exit.** Only the model
   can spawn background agents; `--script` exit cancels healthy agents and
   misattributes the cause; `/quit wait` is undocumented and silent; live
   `/jobs` output is buffered until turn end; `/jobs steer` silently restarts
   the worker from zero; the human deck view is a Rust `Debug` dump.
3. **`spawn_agent_bg` is offered in scripted/piped mode where using it is
   unconditionally fatal**; broken-pipe panics exit 101 from every subcommand;
   human-mode one-shot deliverables go to stderr while stdout stays empty.
4. **Streamed code blocks carry zero styling** — streaming bypasses the
   formatter, so `/review` diff coloring is invisible by default; a fence
   inside a doc comment mis-scopes the tracker. The backlogged syntect box
   work is bigger than polish.
5. **Backlog designs are implementation-ready** (04-backlog-design.md):
   host-only evidence authority for `/diff`//`/review` (with a correction to
   cycle 6: `kitsu review` silently self-grants the repo root today), the
   object form for `--allow-command` with explicit cwd (plus a latent `/new`
   rebinding bug), and artifact GC — which must also sweep orphaned session
   sidecars (11 of 13 observed orphaned), not just `runtime-*` dirs.
   Recommended order: pregrant cwd → /diff decoupling → cheap wins
   (review dead-end hint, export format inference, preflight numbers) →
   artifact GC → session search.
6. Cycle-6 fixes were spot-verified by auditor 1: system-prompt roots and
   partial-on-timeout behave as shipped.

## Report index

1. [Git workflow parity](chat-ux-cycle-7-audits/raw/01-git-workflow.md)
2. (post-ship validation — not delivered; rerun next cycle)
3. [Agent deck UX](chat-ux-cycle-7-audits/raw/03-agent-deck.md)
4. [Backlog design spike](chat-ux-cycle-7-audits/raw/04-backlog-design.md)

## Loop state at interruption

The hourly loop and its cron job were stopped when the spend limit was hit.
Model rotation stands at: cycle 7 = Opus (audits done, fixes pending),
cycle 8 = Sonnet. To resume after raising the limit: re-run the /loop prompt;
the next cycle should start with the fix pass for the findings above before
new audits.
