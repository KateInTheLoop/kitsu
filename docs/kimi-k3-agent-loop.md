# Kimi-K3 worker loop — kitsu drives a hosted frontier model on real work

> **LOOP RE-ARMED 2026-08-14 with a pre-flight gate** (was paused after
> cycle 3's `HTTP 402`). The router reports *monthly included credits
> depleted* — this does not self-heal; it needs purchased HF credits or
> a fresh token. Each hourly tick now runs a single cheap probe request
> first: on 402 the tick ends silently (no agent spawned, no tokens
> billed); on success the next ramp cycle launches immediately.
> Cycle-3's positive finding stands: kitsu classifies the 402 terminal
> on the FIRST attempt with an actionable message.
>
> **2026-08-14 post-cycle-7: the HF token was ROTATED** (401 on
> `whoami-v2` — auth failure, not credit depletion; `/v1/models` is
> public, so listings still succeed without auth). The pre-flight gate
> treats 401 exactly like 402: end the tick silently. Both this loop
> and the DeepSeek loop (`docs/deepseek-agent-loop.md`) resume
> automatically once a fresh token is present in the environment.

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

### Cycle 8 — 2026-08-14 — closes the cycle-7 gap (grade A)

Kimi added the unit tests its cycle-7 P3 diff was docked for lacking:
4 tests in `crates/kitsu-providers/src/types.rs` (+53/-0) —
`cached_tokens_returns_nested_value`, `cached_tokens_zero_when_details_absent`,
`is_empty_false_for_cache_hit_only_usage`, `add_merges_cached_tokens`.
Committed as `333c835` on `dogfood/kimi-k3`, pushed.

- Single attempt, **6.6s wall** (fastest of the loop), 2 tool calls,
  completion **610/24,000 (2.5% — new low)**, zero debris.
- Gates: `cargo fmt --check` clean; `cargo test -p kitsu-providers`
  62 passed / 0 failed (58 pre-existing + 4 new). Tests are genuine —
  each fails if the corresponding P3 behavior regresses; assertions
  spot-checked against exact line numbers, no citation drift.
- No new kitsu weaknesses. Reinforces the standing finding: task
  narrowness (one known file + a fully-specified deliverable) predicts
  cost and reliability more than task type does.

**Kimi is now 3-for-3 on code/test work (cycles 7, 8) after the review
ramp. The P3 contribution is complete and fully tested.** Next: P1
`prompt_cache_key`, or another P1–P11 implementation slice.

### Cycle 9 — 2026-08-14 — P1 prompt_cache_key (grade B)

Kimi implemented P1 (prompt-cache affinity hint): `prompt_cache_key:
Option<String>` on `ChatRequest`, threaded into `WireRequest` with
`#[serde(skip_serializing_if = "Option::is_none")]`, `.as_deref()` in
`build_body`, plus two genuine tests (`..serializes_when_some`,
`..omitted_when_none`) asserting on the serialized body. Committed
`f52c5d7`; a follow-up `0e895ae` (main-session cleanup) restored the
kitsu-core test target. Author verified Kate on both.

- Single attempt, 14 tool calls, completion 2,735/24,000 (11%), zero
  debris/truncation. `kitsu-providers` 64 tests pass (62 + 2 new).
- **Docked to B for two real problems:**
  1. *Cross-crate compile break.* Adding a non-`Default` field to
     `ChatRequest` broke two struct-literal sites in
     `orchestrator.rs` (E0063). Crate-scoped `--allow-write` was
     correct, but "additive" implicitly promises a linking workspace.
     The supervisor added the mechanical `prompt_cache_key: None` at
     both sites. **Lesson: adding a struct field ⇒ verify with `cargo
     build/clippy --workspace`, never just the touched crate.**
  2. *Sub-agent-spawn violation.* Kimi spawned `Agent(...)` twice
     despite the explicit prohibition — the prompt-text ban alone did
     not hold. **Lesson: enforce structurally with `--agent-spawn-cap
     0` when a brief forbids spawns.**
- **Pre-existing defect uncovered (not cycle 9's):** cycle 7's P3
  `Usage` field left four `Usage { .. }` literals in
  `orchestrator_tests.rs` missing `prompt_tokens_details` — the
  kitsu-core *test target* had not compiled since cycle 7, masked by
  crate-scoped verification through cycles 7–9. Fixed in `0e895ae`
  (kitsu-core: 95 tests pass). This is why the two lessons above are
  now standing rules.

Backlog: P1 ✅, P3 ✅ (+tests). Next: P2 anchor content fingerprint or
P6 evidence-clamps-budget. Both cron and supervisor briefs now carry
`--agent-spawn-cap 0` + `--workspace` verification.

### Cycle 10 — 2026-08-14 — P2 anchor content fingerprint (grade B)

Kimi implemented P2 correctly: the anchor tuple grew a private `u64`
`prefix_fingerprint()` — a `DefaultHasher` over role discriminant +
content/tool-call **lengths only** (never message bytes — genuinely
cheap as the spec required). `record()` now takes the sent `messages`
slice; `estimate()`/`anchored()`/`anchor_includes_schema()` require the
fingerprint to match before the fast path, else fall back to safe full
recompute. No new `pub` surface. Committed `e3bc6d4` (Kate verified),
pushed. `--agent-spawn-cap 0` held — zero spawn attempts.

- kitsu-core 98 tests pass (95 + 3 new): unchanged prefix uses the
  anchor; an in-place content edit that PRESERVES message count now
  invalidates it (the exact P2 bug — supervisor confirmed the test fails
  on the old length-only code); tail-append still uses the anchor.
- **Docked to B — incomplete delivery (not a design fault).** Kimi's
  algorithm and all three tests were correct *as authored in its final
  answer*, but the turn hit the default **`--max-rounds 10`** — exhausted
  by 11 sequential `Edit`s to one file — before writing the orchestrator
  call-site fix, two test call-site fixes, and the 3 new tests. The
  supervisor applied them verbatim from Kimi's own authored text.
  Completion tokens were only 8,863/24,000 (37%): **round budget and
  token budget are independent limits — token headroom is not round
  headroom.** kitsu weakness recorded; fix applied below.
- **Pre-existing defect uncovered (not cycle 10's):** a `Usage {..}`
  literal in `kitsu/src/subagent.rs:1866` missing `prompt_tokens_details`
  — the same cycle-7 drift class, this time in the `kitsu` crate's test
  target, undetected until workspace-wide verification. Fixed in the same
  commit.
- **Flake noted:** `kitsu-tools …read_file_caps_input_before_allocating_the_whole_file`
  failed once on the branch (confirmed pre-existing via stash, unrelated
  to P2); it passes on the main branch's newer kitsu-tools. Likely
  load/environment-sensitive; revisit if it recurs.

**Fix applied to both cron and supervisor briefs:** gateway flags now
include **`--max-rounds 30`** so a design-correct many-edit cycle is not
throttled on edit count. Backlog: P1 ✅, P2 ✅, P3 ✅. Next: P6
evidence-clamps-budget or P4 estimator overhead.

### Cycle 11 — 2026-08-14 — P4 estimator overhead (grade A-)

Kimi implemented P4 correctly and completely in `context.rs`:
`PER_MESSAGE_TOKENS = 3` (conservative low end per the spec), and a
genuine two-rate ASCII/non-ASCII estimator (`ascii_split` +
`estimate_mixed`) — ASCII stays byte-identical to the old math, non-ASCII
counts ~1 token/char so CJK is no longer undercounted. 3 genuine tests
(overhead applied, CJK 12 chars: old=9 → new=12, ASCII unchanged); 8
pre-existing exact-value tests honestly recomputed by hand (spot-checked
correct). Committed `1afb464` (Kate verified), pushed. `--agent-spawn-cap
1` held — zero spawn attempts.

- Single successful run, 19 tool calls, completion 7,085/24,000 (30%).
  (First attempt used `kitsu ask` with chat-only flags → instant clap
  error, corrected within the retry budget at zero token cost — a
  supervisor slip, not Kimi's.)
- **Bonus: a genuine latent kitsu bug found and fixed.** The estimator
  change tripped 4 orchestrator tests, whose root cause was a real
  zero-margin defect in `orchestrator.rs::bounded_summary_request`: the
  compaction summarizer budgeted its OWN request at a flat `bytes/4` with
  no headroom, so a request built to the cap now measured a few tokens
  over (from `PER_MESSAGE_TOKENS` + a hardcoded em-dash now correctly
  counted) and was rejected before reaching the provider — **silently
  downgrading every summarize-mode compaction to drop-fallback.** The
  supervisor diagnosed it numerically and rewrote the budget in token
  space (`estimate_str` of the fixed prompt + `PER_MESSAGE_TOKENS`;
  `CHARS_PER_TOKEN`/`PER_MESSAGE_TOKENS` widened to `pub(crate)`). All 80
  orchestrator tests pass. This is a shipped correctness fix, not just
  test churn.
- **Flake now consistent:** `kitsu-tools …read_file_caps_input_before_allocating_the_whole_file`
  now reproduces reliably on the branch (cycle 10 saw it once). Confirmed
  pre-existing via stash, unrelated to P4; passes on main. Likely
  resource/timing-sensitive under the now-four concurrent worktree
  builds. Worth a dedicated look if it spreads to main.

Backlog: P1 ✅ P2 ✅ P3 ✅ P4 ✅. Kimi is 5-for-5 on code work since the
ramp (cycles 7-11). Next: P6 evidence-clamps-budget, P5 anchor sanity
clamp, or P7/P10/P11.

### Cycle 12 — 2026-08-14 — P5 anchor sanity clamp (grade A-)

Kimi implemented P5 correctly and completely in `context.rs` (+166/-14):
`anchor_plausible(anchor, full)` — exact `u128` integer-ratio comparison,
inclusive band `[0.4x, 2.5x]` of the anchor-free full estimate (no
floats, boundary-exact, overflow-safe). `estimate()` now always computes
the full estimate and, when a structurally-valid anchor falls outside the
band, discards it FOR THAT CALL ONLY (stored anchor untouched — advisory,
never mutated, never errors) and bumps a new `anchor_rejections`
telemetry counter. Committed `ac3d5bf` (Kate verified), pushed. cap-1
held — zero spawn attempts.

- Single attempt, 15 tool calls, completion 8,311/24,000 (35%). 3 genuine
  tests (plausible anchor trusted; anchors 0 and 100x both reject + fall
  back + counter increments; exact boundary 16/17/99/100 trusted, 15/101
  rejected). 5 pre-existing round-number-anchor tests honestly recomputed.
- Cross-file fix (expected pattern): one orchestrator test relied on an
  implausible anchor (500 vs ~8-token fixture) being trusted to force a
  synthetic overflow — now correctly rejected by P5. Fixed by padding the
  fixture + a plausible-but-over-target anchor. Honest fix, not tolerance
  widening. kitsu-core 104/104.
- **Flake escalating:** `kitsu-tools …read_file_caps_input_before_allocating_the_whole_file`
  now reproduces **3/3 consecutive cycles** on the branch (stash-confirmed
  pre-existing, unrelated to P5), yet passes on main's newer kitsu-tools.
  No longer plausibly "just load-sensitive" — likely a real code
  difference between the branch's older kitsu-tools and main. **Needs a
  dedicated diff investigation** before the kimi branch is merge-ready.

**Backlog: P1 ✅ P2 ✅ P3 ✅ P4 ✅ P5 ✅. Kimi is 6-for-6 on code work
(cycles 7-12).** Next: P6 evidence-clamps-budget, P7 anchor schema
fingerprint, P10 model-context caching, or P11 tool-schema re-serialization.
