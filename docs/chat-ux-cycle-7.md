# Chat UX dogfood — cycle 7 audit checkpoint

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

## Implementation outcome

After capacity returned, two Opus fix passes closed the cycle on this branch:

**Pass C (agent-deck correctness, committed `cb4ff84`):** built-in profiles
inherit reasoning-effort omission instead of force-sending it (fixing the
critical dead-profiles bug) and failed spawns render as errors with
remediation; scripted/piped exits join live background agents (bounded by
`--agent-join-timeout`) instead of cancelling them with a false rollback
blame, and piped runs surface previously-dropped background results; broken
pipes exit cleanly instead of panicking; `/jobs`, `/agent`, and `/profiles`
render from the DTO in the fox design language instead of Rust Debug dumps;
`/jobs steer` receipts state the real restart semantics. 13 regression tests.

**Pass D (backlog implementation, this commit):** `--allow-command` accepts
`{"argv":[...],"cwd":"..."}` with startup validation, `/permissions`
provenance, and the `/new`-rebinding latent bug fixed; `/diff` and `/review`
run under a host-only `WorkspaceEvidenceAuthority` — no model read roots
required or granted, `run_review`'s silent self-grant removed, provider
disclosure explicitly announced (human line + JSONL `review_disclosure`),
`/permissions` gains a host-evidence section; `kitsu review` clean-tree
dead-end now suggests `--commit HEAD` and `@file`; `sessions export` infers
JSON from the destination extension; context-overflow errors spell out
window/budget/trim-target arithmetic; new `kitsu artifacts list|prune` with
flock-based liveness, startup sweep of provably-empty orphans (both
`runtime-*` generations and `s-*.artifacts` sidecars), and an `/artifacts`
cross-run section. 25 more regression tests. No descopes.

## Release verification

Final tree: fmt clean, workspace clippy clean (`-D warnings`), 623 passing
tests (206 bin unit, 112 CLI integration, 84 core, 58 provider, 130
tools/permissions, 30 render, 2 provenance, +artifact-crate units). The known
PTY-environmental failure was re-verified as pre-existing at each pass.
Maintainer live checks: scripted `/diff` produces evidence with zero grants;
`artifacts list`/`prune --dry-run` identify orphans without deleting;
`kitsu-tools` untouched across both passes, so the model-facing permission
boundary is provably unchanged.

## Remaining backlog

Session search; screen-reader rendering mode; syntect code-block boxes on the
streaming path (with the doc-comment fence-tracker fix); agent-deck front
door (`/agent spawn`), live-turn `/jobs` output, steering correction
accumulation; deck identity proposals D1–D8 round 2; human-mode one-shot
deliverable on stdout (git-workflow gap 4); JSONL file-mutation event.

## Loop state at interruption

The hourly loop and its cron job were stopped when the spend limit was hit.
Model rotation stands at: cycle 7 = Opus (audits done, fixes pending),
cycle 8 = Sonnet. To resume after raising the limit: re-run the /loop prompt;
the next cycle should start with the fix pass for the findings above before
new audits.
