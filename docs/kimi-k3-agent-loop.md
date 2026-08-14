# Kimi-K3 worker loop — kitsu drives a hosted frontier model on real work

> **LOOP RE-ARMED 2026-08-14 with a pre-flight gate** (was paused after
> cycle 3's `HTTP 402`). The router reports *monthly included credits
> depleted* — this does not self-heal; it needs purchased HF credits or
> a fresh token. Each hourly tick now runs a single cheap probe request
> first: on 402 the tick ends silently (no agent spawned, no tokens
> billed); on success the next ramp cycle launches immediately.
> Cycle-3's positive finding stands: kitsu classifies the 402 terminal
> on the FIRST attempt with an actionable message.

## Success ramp (start here when credits return)

Three failed cycles shared one root pattern: the task was too wide for
one turn's budget. To make Kimi a *contributor*, cycles now ramp:

1. **Cycle 4 — bank the first success.** ONE file only:
   `crates/kitsu-core/src/context.rs` (378 lines, one read). Deliverable
   `reviews/kimi-cycle-4.md`: ranked, line-cited proposals for that file
   alone (estimate accuracy, budget policy, compaction breaker). No
   other reads permitted. Target < 40K total tokens.
2. **Cycle 5 — usage/caching slice.** `crates/kitsu-providers/src` usage
   parsing only: propose surfacing `cached_tokens` + catalog/window
   caching, citing the actual parsing code.
   **Attempt 1 FAILED 2026-08-14** (worktree commit d3d0ba5): all three
   scoped reads clean, then completion tokens overshot the 14,000 cap
   (14,322 spent, 100% invisible reasoning, zero visible output,
   discarded). **Next tick RETRIES cycle 5** — the worktree log already
   has a cycle-5 entry, so the supervisor must read the ramp status
   here, not just count log entries. Retry changes: `--max-output-tokens
   24000`; if that fails too, split the task (5a: locate the
   cached_tokens drop only; 5b: ranked caching proposals only).
3. **Cycle 6 — synthesis.** Re-read own cycle-4/5 reviews plus targeted
   orchestrator ranges; produce the full caching + context-window review
   the loop originally wanted, now grounded in two banked passes.

Rules (updated after cycle-5 attempt 1): timeout 300; output budget
scales with task shape — >= 14000 for one-file reviews, **>= 24000 for
any multi-file or two-part analytical task** (reasoning spend grows with
task complexity: 8.8K spent at cycle 2, 14.3K at cycle 5); NO sub-agent
spawns; write the file before polishing.

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
- **Worktree builds NEVER target the tmpfs** (cycle-7 incident: /tmp is
  a 5.7G tmpfs, shared with a concurrent session's 4.9G build area, and
  a fresh kitsu target dir needs ~12G — the build filled the tmpfs and
  starved BOTH sessions' output capture). Any cargo command in the
  worktree must set `CARGO_TARGET_DIR=/home/mmpg/.cache/kimi-worktree-target`.
- Supervisors never spawn helper agents and never pause to await
  notifications — every wait is a synchronous `sleep` in a Bash call
  (two supervisors have stalled the loop this way).

> **RAMP COMPLETE — 2026-08-14, cycle 6 synthesis banked at grade A**
> (commit 744b2da on dogfood/kimi-k3). Three consecutive deliverables:
> cycle 4 (A), cycle 5 attempt 2 (B+), cycle 6 (A) — after three
> consecutive pre-ramp failures. Cycle 6: 6 tool calls exactly as
> scoped, 42.4K total tokens, completion spend 5,991/24,000 (25% — the
> LOWEST of the ramp despite the largest input; synthesis-over-own-work
> appears cheaper in hidden reasoning than cold-read analysis). Genuine
> merge: 8 CARRIED + 3 NEW orchestrator-grounded proposals, citations
> spot-checked (context.rs exact; orchestrator ≤15-line drift). The
> loop's founding goal — the definitive caching + context-window
> review — exists at reviews/kimi-cycle-6.md on the work branch.
> **FRONTIER ROOT-CAUSE COMPLETE — 2026-08-14 (user-directed).** Why
> did open-ended tasks fail while spoon-fed ramp cycles succeeded?
> Five tracked runs (A-E) + 10 router probes answered it. TWO causes:
> (1) COST — hidden reasoning burn; the router honors
> `reasoning_effort` (low: 542→82 completion tokens, tool-calling
> intact) and `chat_template_kwargs {thinking:false}` (→26); kitsu's
> existing `--reasoning-effort low` flag was simply never used by this
> loop. (2) RELIABILITY — after long exploration (~11+ rounds), Kimi
> emits its trailing Write in native `<|...|>` token format; weak
> providers (featherless-class) mangle it into text debris (observed
> debris contained the Write's file path), stronger ones (:together)
> silently drop or the instruction drifts. Run E proved the fix: pin
> `:together` + bound exploration ("at most N files") + write-FIRST
> mandate → deliverable written cleanly (5 tool calls, 95K tokens).
> kitsu now also detects the debris and re-prompts the destroyed call
> (commit 20264cb). **THE FRONTIER RECIPE** for open-ended Kimi work
> through kitsu: `--model moonshotai/Kimi-K3:together`,
> `--reasoning-effort low` for exploration tasks (default thinking for
> hard synthesis), bounded exploration in the prompt, write-early flow.
> **LOOP RE-ARMED**: next ticks run open-ended cycles under the recipe
> (implementation-grade tasks from the P1-P11 review are fair game).

> **FIRST BANKED DELIVERABLE — 2026-08-14, ramp cycle 4.** Credits
> returned; the narrow-scope ramp worked exactly as designed: one Read,
> one Write, 59s, 23,899 tokens (vs 187K-434K in the failed cycles),
> zero spawns, zero truncation. `reviews/kimi-cycle-4.md` (commit
> `47a95ff` on dogfood/kimi-k3): 5 ranked proposals on context.rs, every
> line citation independently verified accurate, grade A. Ramp advanced
> to cycle 5 (providers usage/caching slice).

## Improvement backlog (fed by supervisor observations)

Kimi-authored NEW proposals from cycle 6 synthesis (grade A; also see
the top-5 priority list in reviews/kimi-cycle-6.md — P1 prompt_cache_key,
P2 anchor fingerprint, P3 cached_tokens, P6 evidence-clamps-budget,
P4 estimator overhead):

- [ ] P6: `ContextWindowEvidence`/`context_window_shortfall` diagnose
  server-side truncation but never feed back into the session budget —
  convert the shipped diagnosis into a budget clamp.
- [ ] P7: `anchor_includes_schema` is a bare bool; mid-session tool-
  registry changes fool the anchor — track a schema fingerprint.
- [ ] P11: `ensure_chat_request_fits` re-serializes tool schemas on
  every check — cache the serialized size.

Kimi-authored proposals from cycle 5 attempt 2 (banked 2026-08-14 at
B+, commit 0751f0e on dogfood/kimi-k3; spot-checked — content accurate,
line numbers drift 20-48 lines in one region of openai_compat.rs):

- [ ] Surface `cached_tokens`: add a `PromptTokensDetails` field to
  `Usage` (types.rs:136-144) with accessor, extend `Usage::add`, and
  widen `is_empty()` — Kimi's design is backward-compatible and matches
  the attempt-1 grounding.
- [ ] `Usage::is_empty()` would discard a legitimate cache-hit-only
  chunk (zero prompt/completion, nonzero cached_tokens) even after a
  naive cached_tokens fix — Kimi-found edge case the human grounding
  missed; fix together with the above.
- [ ] Add `prompt_cache_key` to `WireRequest` (verified absent) so
  providers that key prompt caches can be steered.
- [ ] Cache parsed `model_context_length` across runs (window discovery
  re-parses every start).
- [ ] Tool-UX (cycle-5 supervisor): the Write tool's blind-overwrite
  guard surprises models mid-turn ('requires expected_previous_content'
  → an extra Read/Write round-trip); state the read-before-overwrite
  precondition in the tool description up front.

Kimi-authored proposals from cycle 4 (verified citations; triage into
kitsu-core work as capacity allows):

- [ ] `context.rs:100-107` — anchor validity is length-only; an in-place
  edit inside the anchored prefix keeps a stale anchor "valid". Add a
  prefix fingerprint or generation counter.
- [ ] `context.rs:79-81` — gateway-reported `prompt_tokens` is trusted
  unsanitized; one bad value poisons estimates until the next record.
  Distrust anchors diverging >~2.5x from the full estimate.
- [ ] `context.rs:166` — trim buffer `min(window/10, 2048)` goes flat
  above 20,480-token windows while absolute estimate error keeps
  growing; scale the cap with the window.

- [ ] `--model` override drops the gateway profile's `context_window`
  (observed 2026-08-13: `ask` with `--model moonshotai/Kimi-K3` fell back
  to window 8192; explicit `--context-window` was required).
- [ ] Surface provider `cached_tokens` (HF router reports it in usage)
  in `kitsu-usage:` records and `/cost`, so caching wins are measurable.
  **Grounded (cycle-5 supervisor, 2026-08-14):** `Usage` at
  `crates/kitsu-providers/src/types.rs:136-144` carries only
  prompt/completion/total, so serde silently drops
  `prompt_tokens_details.cached_tokens` before any Rust code sees it;
  both streaming (`openai_compat.rs:252-535` range) and non-streaming
  (`openai_compat.rs:616-618`) paths decode into that type.
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

(appended by the supervisor each cycle, newest last; the per-cycle
detail for cycles 1–7 also lives in this doc's copy on `dogfood/kimi-k3`)

### Cycle 7 — 2026-08-14 — FIRST CODE CONTRIBUTION (grade A-)

Kimi implemented its own cycle-6 P3 design: surface `cached_tokens`
from `prompt_tokens_details` in `kitsu-providers` (`types.rs`, +32/-2 —
`PromptTokensDetails` struct, `Usage.prompt_tokens_details`,
`cached_tokens()` accessor, `is_empty()` widened, `add()` merges cached
tokens). Committed as `eca129f` on `dogfood/kimi-k3`, pushed.

- Single attempt, 58.7s wall, 5 tool calls, completion 2,150/24,000
  (9% — lowest of the loop), zero truncation, zero debris events.
- Gates: `cargo fmt --check` clean; `cargo test -p kitsu-providers`
  58 passed / 0 failed. Fully additive; no struct-literal breakage.
- Docked from A: no new unit test for the added logic, and redundant
  double-`Option` nesting vs its own single-`Option` design.
- Infra incident (not Kimi's fault): first `cargo test` filled the /tmp
  tmpfs (worktree default `target/` ≈12G vs 5.7G mount) and briefly
  broke Bash across sessions — hence the CARGO_TARGET_DIR rule above.

**Verdict: the frontier recipe is validated for code work, not just
review/analysis. Kimi is now a contributing agent.** Next cycles:
continue P-backlog implementation (P1 `prompt_cache_key` or a unit test
for the P3 diff are natural next tasks); consider requiring a test with
every code diff as part of the task brief.
