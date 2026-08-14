# Kimi-K3 worker loop — kitsu drives a hosted frontier model on real work

Started 2026-08-13 at user direction. Kimi-K3 (moonshotai/Kimi-K3 via the
Hugging Face router, `--gateway huggingface`) is treated as an **extra
agent that helps us out**: kitsu runs it on real improvement tasks against
kitsu's own codebase, while a **Sonnet supervisor agent** starts the run,
watches kitsu's behavior, and turns what it sees into scheduled work.

Every cycle is therefore two experiments at once:
1. **The work itself** — Kimi-K3 produces a concrete deliverable
   (currently: review-driven improvement proposals).
2. **Dogfooding under supervision** — the supervisor records kitsu
   weaknesses observed while driving a hosted reasoning model (UX gaps,
   dishonest receipts, context mishandling, tool-loop failures) and
   schedules improvements.

## Standing task for Kimi-K3 (via kitsu)

Review kitsu's provider/caching and context-window handling and propose
improvements. Focus areas, in order:

1. **Caching** — where kitsu could cache more: provider-side prompt
   caching (HF router forwards `cached_tokens` in usage — kitsu ignores
   it today), model catalog/window discovery, tool schemas, system
   context re-sent per turn, session resume costs.
2. **Context-window management** — `crates/kitsu-core/src/context.rs`
   (ContextMeter, ContextBudget, compaction circuit breaker) and the
   orchestrator's trim/summarize paths: eviction policy quality,
   token-estimate accuracy vs provider-anchored usage, window discovery
   when `--model` overrides the profile (known bug: override falls back
   to 8192 instead of profile/discovered window), headroom policy for
   reasoning models whose hidden reasoning spends output budget.

Deliverable per cycle: `reviews/kimi-cycle-<N>.md` on the work branch —
concrete, file-referenced proposals ranked by value; no vague advice.

## Rules

- **Branch**: all Kimi-cycle work happens on `dogfood/kimi-k3` (worktree
  under `/tmp`, never in the main checkout, which stays on the UX branch).
- **Gateway**: `--gateway huggingface --model moonshotai/Kimi-K3`,
  `--context-window 131072`, `--timeout-secs 300` (cycle 2: the default
  120s wall clock cancelled a healthy run), and generous
  `--max-output-tokens` (>= 12000; it is a reasoning model — cycle 2
  burned 8,852 completion tokens against a 6000 cap with zero visible
  output). Tell the model NOT to spawn sub-agents (cycle 2: unprompted
  `Agent(...)` spawns at 65-71s each sank both attempts).
- **Token**: the supervisor receives `HF_TOKEN` via environment at spawn
  time. NEVER write the token into any file, commit, or log.
- **Read scope**: grant kitsu read access only to the repo checkout
  (`--allow-path <worktree>`); write access only to the worktree's
  `reviews/` directory.
- **Cost bounds**: one kitsu run per cycle, one retry on transport
  failure; keep prompts scoped (name the exact files to read) so a cycle
  stays in the low tens of thousands of tokens.
- **Supervisor**: a Sonnet agent. It must (a) run kitsu itself (not
  reimplement the task), (b) capture kitsu's stderr receipts as evidence,
  (c) grade the deliverable briefly, (d) append observed kitsu weaknesses
  with reproduction notes to `docs/kimi-k3-agent-loop.md` under the cycle
  log on the work branch, and (e) file concrete follow-ups in the
  backlog section below.
- **Never** run the Kimi loop and edit the same files from the main
  session simultaneously; the branches keep the work disjoint.

## Improvement backlog (fed by supervisor observations)

- [ ] `--model` override drops the gateway profile's `context_window`
  (observed 2026-08-13: `ask` with `--model moonshotai/Kimi-K3` fell back
  to window 8192; explicit `--context-window` was required).
- [ ] Surface provider `cached_tokens` (HF router reports it in usage)
  in `kitsu-usage:` records and `/cost`, so caching wins are measurable.
- [ ] **Cycle 2:** wall-clock-cancelled turns print zero `kitsu-usage`/
  `kitsu-context` receipts even when real inference time was spent —
  a total cost blackout for timed-out turns. Surface partial usage (or
  an explicit "usage unknown: cancelled mid-flight" receipt) on
  cancellation.
- [ ] **Cycle 2:** model-driven sub-agent spawns are unbounded in cost
  visibility mid-turn — nothing surfaces cumulative sub-agent spend to
  the model (or the operator) while the turn runs; spawn-cap teeth only
  count refusals, not seconds/tokens burned by granted spawns.
- [ ] **Cycle 2:** reasoning models can exhaust `--max-output-tokens`
  entirely on hidden reasoning (8,852 spent vs 6000 cap, zero visible
  output, two truncation warnings). Add a reasoning-headroom warning or
  an explicit hint in the truncation message naming the reasoning spend.
- [ ] **Cycle 2:** offload-preview re-read thrash — the model re-read its
  own offloaded output in shrinking ranges instead of recognizing it had
  the content; the 500-char preview should disclose coverage ("preview
  of N KiB; full content already returned to you as ...").

## Cycle log

(appended by the supervisor each cycle, newest last)
