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
  **Grounded twice now (cycle-5 supervisor 2026-08-14, then cycle-5
  attempt-2 Kimi-K3 2026-08-14):** both agree `Usage`
  (`crates/kitsu-providers/src/types.rs:136-144`) has exactly
  `prompt_tokens`/`completion_tokens`/`total_tokens` with no
  `prompt_tokens_details` field, so serde silently drops
  `cached_tokens` before any Rust code sees it, uniformly across the
  streaming (`StreamChunk.usage`) and non-streaming
  (`CompletionResponse.usage`) paths in `openai_compat.rs`, both of
  which deserialize into the same `Usage` type. Kimi's attempt-2 review
  (`reviews/kimi-cycle-5.md`) goes further than the attempt-1
  supervisor grounding: it also flags that `Usage::is_empty()`
  (`types.rs:148-150`) treats an all-zero triple as "not reported",
  which would silently discard a legitimate cache-hit-only chunk
  (`prompt_tokens: 0`, nonzero `cached_tokens`) even after a
  `cached_tokens` field is added — a real edge case neither the
  standing backlog item nor the attempt-1 grounding had identified.
  Proposed fix (Kimi, cited `types.rs` ~129-168, `openai_compat.rs`
  ~283-618, precise locations spot-checked by the supervisor and found
  structurally correct but with line numbers frequently drifting
  20-48 lines in the 240-620 range — see cycle-5 attempt-2 log entry):
  add `Option<PromptTokensDetails { cached_tokens: u64 }>` to `Usage`,
  a `cached_tokens()` accessor, extend `Usage::add` to sum it, and
  widen the two capture-site gates to
  `!u.is_empty() || u.cached_tokens() > 0`. Still unchecked — no code
  change made yet.
- [x] fixed on main (f3d579c) — ranged reads in the `fs_read_file` tool
  used to cap at 64 KiB measured from the *absolute start of the file*,
  not from the requested range's start line, so a range whose
  `start_line` fell past the 64 KiB absolute mark returned almost
  nothing regardless of the requested range's width. Fixed by
  `read_capped_line_range` (`crates/kitsu-tools/src/builtins/fs.rs:372-421`
  on main), which streams to the requested lines and caps relative to
  the served range. **Confirmed under real load in cycle 2:** both
  cycle-2 attempts read `orchestrator.rs` (3221 lines / ~128 KiB) in
  three wide ranges (1-1100, 1100-2200, 2200-3221) with zero thrash
  reads, versus 6-7 near-empty reads per attempt in cycle 1. See
  `reviews/kimi-cycle-1.md` for the original trace and
  `reviews/kimi-cycle-2.md` for the confirmation.
- [x] fixed on main (f3d579c) — the `served {X} KiB of {Y} KiB total`
  disclosure used to measure `served_bytes` *before* the line-range
  slice, reporting the pre-slice absolute read instead of what the
  caller actually received. Fixed at
  `crates/kitsu-tools/src/builtins/fs.rs:530` (main), which now computes
  `served_bytes` from `content.text` post-slice. Two regression tests
  were added in the same commit (a range starting past 64 KiB serves
  the requested lines uncapped; disclosures report served bytes) —
  covers the gap the cycle-1 backlog item asked for.
- [ ] Retry/backoff on `HTTP 429` gives up after 3 attempts with short,
  fixed-feeling backoffs (~210ms, ~420ms) even when the per-turn
  deadline has ~298 of 300 seconds still unused. Consider honoring
  `Retry-After` more aggressively or scaling backoff to use more of the
  remaining `--timeout-secs` budget before declaring the turn a terminal
  failure and discarding all provisional work.
- [ ] A turn that fails after burning substantial tool-call rounds is
  discarded wholesale (`"provisional assistant output was DISCARDED"`) —
  none of the reads already performed are preserved for a resumed turn,
  so a retry re-pays the full cost of re-listing/re-reading everything.
  Consider allowing a retried turn to resume from prior tool results
  within the same process, or at least surfacing a hint to the operator
  that a `--resume` session would have preserved them.
  **Grounded (cycle-5 supervisor, 2026-08-14):** `Usage` at
  `crates/kitsu-providers/src/types.rs:136-144` carries only
  prompt/completion/total, so serde silently drops
  `prompt_tokens_details.cached_tokens` before any Rust code sees it;
  both streaming (`openai_compat.rs:252-535` range) and non-streaming
  (`openai_compat.rs:616-618`) paths decode into that type.
- [ ] **New, cycle 2:** a turn cancelled by the outer `--timeout-secs`
  wall clock (`error: chat request timed out after 120s`) prints zero
  `kitsu-usage`/`kitsu-context` receipts, even though the provider call
  itself reported `cancelled; HTTP 200; cancelled; terminal=true;
  107590ms deadline remaining` — i.e. kitsu's own per-attempt inference
  budget still had ~108s left when the *outer* wall clock cut it off.
  Operators are left with zero token-cost visibility into a turn that
  ran ~120s of real inference. Consider (a) reporting whatever partial
  usage the provider returns on a cancelled call, and (b) reconciling
  the outer `--timeout-secs` wall clock against the inner per-attempt
  deadline so one doesn't silently starve the other.
- [ ] **New, cycle 2:** the model's own `Agent(...)` sub-agent tool is
  expensive (64-71s wall time per spawn in both cycle-2 attempts) and
  unbounded — nothing in kitsu stops the model from spawning it for
  work outside the task's stated scope (both cycle-2 attempts read
  every file the task named cleanly, then spawned `Agent(...)` to go
  chasing "EXACT file:line" citations in `crates/kitsu/src/{cli,config,
  main}.rs`, files the task never mentioned). That single detour is
  what exhausted the wall-clock budget in attempt 1 and drove ~80% of
  attempt 2's 425,660 prompt tokens. Consider a cheaper mechanism for
  "confirm this line number" than a full sub-agent spawn, and/or a
  soft per-turn budget on sub-agent tool calls that's visible to the
  model.
- [ ] **New, cycle 2:** with a reasoning model and a `--max-output-tokens`
  budget sized for the *visible* answer (6000, per the loop's own
  "generous, >= 4000" guidance), hidden reasoning tokens can still
  exhaust it before any visible output is emitted — cycle 2 attempt 2
  hit `response truncated: model hit its output length limit` twice
  before the turn was discarded with `completion_tokens: 8852` against
  a 6000 budget (so completion tokens exceeded the nominal cap, all
  apparently spent on reasoning/tool-call scaffolding rather than the
  requested file). This is precisely the "output-budget headroom for
  reasoning models" risk the standing task asks Kimi to write about;
  kitsu itself has no separate reasoning-token headroom knob or warning
  when a reasoning-capable model is selected with a budget sized only
  for final output.
- [ ] **New, cycle 2:** kitsu's own context-offload path (large tool
  results written to `/tmp/kitsu-offload-*/...txt` with a 500-char
  in-context preview, "model can fs_read_file it") can trigger
  self-inflicted re-reads: in both cycle-2 attempts the model, having
  triggered an offload on its own out-of-scope read, then issued
  further `fs_read_file` calls against the *offloaded copy* of the file
  it had just read, in shrinking ranges — a second, milder echo of
  cycle 1's thrash pattern, this time caused by the offload preview
  being too thin for the model to tell it already had the content
  nearby. Worth checking whether the preview could hint the byte range
  already covered, or whether the model's own prior read should be
  reusable without a second tool round-trip.

## Cycle log

(appended by the supervisor each cycle, newest last; the per-cycle
detail for cycles 1–7 also lives in this doc's copy on `dogfood/kimi-k3`)

### Cycle 1 (2026-08-13)

**Run stats.** Two attempts, both failed (one run + the one permitted
transport-failure retry; per the loop's cost bound, the run half then
stopped). Both ended in the same way: the turn was discarded after
`HTTP 429` from the HF router persisted through 3 retries, with ~298s of
the 300s `--timeout-secs` budget still unused at the point of failure.

| | attempt 1 | attempt 2 (retry) |
|---|---|---|
| wall time | 23.1s | 30.3s |
| tool calls | 11 (3 list + 8 read) | 15 (4 list + 11 read) |
| wasted ranged reads (thrash) | 6 (`1620-*`, each 1 line back) | 7 (`1530-*`/`1531-*`/`1532-*`, 1-3 lines back) |
| `kitsu-usage` prompt_tokens | 186,397 | 243,044 |
| `kitsu-usage` completion_tokens | 1,176 | 1,483 |
| `kitsu-usage` total_tokens | 187,573 | 244,527 |
| `kitsu-context` used/effective/window | 25,856 / 125,072 / 131,072 | 26,359 / 125,072 / 131,072 |
| outcome | discarded, HTTP 429 x3 | discarded, HTTP 429 x3 |

Combined: 432,100 tokens burned across both attempts, zero lines of
deliverable content produced. Total real time across both attempts:
~53.4s — fast because rate-limit failures short-circuited before the
timeout budget was ever really used.

**Deliverable grade: F (no deliverable).** Kimi-K3 never reached the
"write the review" step in either attempt; both turns were discarded by
kitsu before commit. `reviews/kimi-cycle-1.md` is a supervisor-authored
failure record, not a model-authored caching/context-window review. The
standing task (rank caching and context-window proposals against
`context.rs`/`orchestrator.rs`/`kitsu-providers/src`) carries over to
cycle 2 unchanged.

**Weaknesses observed (ranked):**

1. **Ranged-read byte cap is absolute-file-position-based, not
   range-relative** (root cause of the failure). Repro: read
   `crates/kitsu-core/src/orchestrator.rs` (3221 lines / ~128 KiB) with
   `start_line=1620, end_line=2100` — a wide, valid range — and observe
   it return exactly 1 line, because the 64 KiB cap in
   `crates/kitsu-tools/src/builtins/fs.rs:333-363` is already exhausted
   by byte 65,536 (line ~1533) before the range filter at
   `fs.rs:445-468` ever runs. This single bug produced 6-7 wasted tool
   rounds per attempt and is the direct cause of the token blow-up and
   subsequent rate-limit failure below.
2. **429 retry/backoff doesn't use the available deadline.** Repro:
   run any piped chat against a rate-limited hosted gateway with
   `--timeout-secs 300`; kitsu gives up after 3 attempts and backoffs
   under 1 second combined, discarding the turn with ~298s of budget
   still on the clock. The error message even says "increase
   --timeout-secs" — but a larger timeout wouldn't have helped here,
   since the 3-attempt cap (not the deadline) was the binding
   constraint.
3. **Failed turns discard all prior tool-call work.** Repro: same as
   above — a turn that performed 8-11 tool calls and read most of a
   target file loses all of it on final-round failure; a retry (as
   required by the loop's cost bound) starts from zero and re-reads
   everything, doubling the token cost for no new information the
   second time around (186K → 243K prompt tokens between attempt 1 and
   2 despite reading essentially the same files).
4. **The `served X KiB of Y KiB total` disclosure is measured pre-slice.**
   Repro: request a narrow line range on a large file; the "served"
   figure in `fs.rs:474-480` reflects bytes read before the range filter,
   not bytes actually returned to the caller — confusing for a model (or
   a human) trying to reason about why a narrow range returned almost
   nothing.
5. This cycle could not evaluate the actual review-writing behavior of
   Kimi-K3 through kitsu, so no supervision evidence exists yet for the
   loop's original targets (caching proposal quality, context-window
   proposal quality, honesty of file:line citations). Carries to cycle 2.

**Improvements to schedule:** fix the `fs_read_file` ranged-read cap
placement (backlog item above) before running cycle 2 — it is very
likely to reproduce the same thrash-then-429 failure again, since
`orchestrator.rs` is the same size and the standing task prompt directs
the model at it the same way. Consider also lowering the per-attempt
token cost of the standing prompt (e.g. permit `wc -l`-style line-count
disclosure up front so the model doesn't need to discover file length by
binary search) as a mitigation independent of the fix.

### Cycle 2 (2026-08-13)

**Run stats.** Two attempts (one run + the one permitted retry, per the
loop's cost bound), both discarded before commit, for two different
root causes — neither a repeat of cycle 1's ranged-read bug.

| | attempt 1 (no explicit `--timeout-secs`, so CLI default `120` applied) | attempt 2 (retry, `--timeout-secs 300`) |
|---|---|---|
| wall time | 120.0s | 238.1s |
| tool calls | 21 (2 list + 17 read + 1 `Agent` spawn) | 15 (2 list + 11 read + 1 `Agent` round, incl. 3 parallel sub-agents in one round) |
| ranged-read thrash on `orchestrator.rs`/target files | **0** — three wide ranges (1-1100, 1100-2200, 2200-3221) each returned full-width, no cap stalls | **0** — same three ranges, same result |
| off-task detour | listed+read `crates/kitsu/src/{cli,config}.rs` (never named in task), 1 `Agent` spawn (65.2s) | listed+read `crates/kitsu/src/main.rs` (never named in task), 1 `Agent` spawn (64.7s) + 3 parallel `Agent` spawns (70.9s) |
| `kitsu-usage` prompt/completion/total | **none printed** — turn cancelled before a usage line was ever emitted | 425,660 / 8,852 / 434,512 |
| `kitsu-context` used/effective/window | none printed | 57,452 / 125,072 / 131,072 |
| outcome | discarded, outer 120s wall-clock timeout (`inference provider attempt 1/3: cancelled; ...; 107590ms deadline remaining` — the *inner* per-attempt budget still had time left) | discarded, `model output was truncated by its length limit` (`--max-output-tokens 6000` exhausted, 2x `response truncated` warnings) |

Combined accounted tokens: 434,512 (attempt 1 reported none despite ~120s
of real inference — a receipts gap, see backlog). Both figures are far
above the loop's "low tens of thousands of tokens" cost bound, driven
almost entirely by the unplanned `crates/kitsu/src` detour rather than
by the three files the task actually named.

**Deliverable grade: F (no deliverable), same as cycle 1 but for new
reasons.** `reviews/kimi-cycle-2.md` is a supervisor-authored failure
record. The model never reached the "write the review" step in either
attempt. Every read of the task's named files (`context.rs`,
`orchestrator.rs`, `crates/kitsu-providers/src`) was clean, correctly
scoped, and — critically — free of cycle 1's ranged-read thrash. What
sank both attempts instead was the model spawning its own `Agent(...)`
sub-agent tool to chase file:line citations in files the task never
named (`crates/kitsu/src/{cli,config,main}.rs`), which alone accounted
for the bulk of both the wall-clock and token overrun. The standing
caching/context-window review carries over to cycle 3 unchanged.

**Ranged-read fix: confirmed held under real load.** Verified against
the *main checkout's* `crates/kitsu-tools/src/builtins/fs.rs` (the
worktree's own checkout is still pre-fix; the binary run in this cycle
was built from main, which has the fix at commit `f3d579c`):
`read_capped_line_range` (`fs.rs:372-421`) streams to the requested
range and caps relative to it, dispatched from `fs.rs:515-522` whenever
`start_line`/`end_line` is present, with the `served`/`total` disclosure
at `fs.rs:530` now computed post-slice. Both cycle-2 attempts read the
same ~128 KiB `orchestrator.rs` in the same three wide ranges cycle 1
thrashed on, and both came back full-width with zero stalls. **The
served-of-total disclosures matched reality this time** — no ranged
read under-reported what it actually returned, unlike cycle 1.

**Weaknesses observed (ranked):**

1. **A wall-clock-cancelled turn reports zero cost receipts.** Repro:
   run a tool-heavy turn against a slow/queued provider without an
   explicit `--timeout-secs` (CLI default is 120s, confirmed at
   `crates/kitsu/src/cli.rs:95` in the worktree checkout); when the
   outer wall clock cuts the turn off, no `kitsu-usage`/`kitsu-context`
   line is ever printed, even though the provider's own status line
   (`inference provider attempt 1/3: cancelled; HTTP 200; cancelled;
   terminal=true; 107590ms deadline remaining`) shows real inference
   time was spent and the *inner* per-attempt deadline still had ~108s
   left when the *outer* deadline killed it. An operator has no way to
   see what a cancelled-but-not-yet-billed-looking turn actually cost.
2. **The model's `Agent(...)` sub-agent tool is expensive and
   unbounded.** Repro: give kitsu a scoped task naming exact files; in
   both cycle-2 attempts the model read every named file cleanly, then
   spawned `Agent(...)` (65-71s per spawn, including three *parallel*
   spawns in one round in attempt 2) to verify citations in files the
   task never mentioned. Nothing in kitsu bounds or surfaces sub-agent
   cost to the model mid-turn; the detour alone consumed the entire
   wall-clock budget in attempt 1 and ~80% of the token budget in
   attempt 2.
3. **Reasoning-token consumption can exhaust `--max-output-tokens` with
   zero visible output.** Repro: run a reasoning-capable model
   (Kimi-K3) with `--max-output-tokens 6000` on a tool-heavy turn;
   attempt 2 hit `response truncated: model hit its output length
   limit` twice and finished with `completion_tokens: 8852` (over the
   nominal 6000 cap) with no deliverable ever written. kitsu has no
   separate headroom knob or warning for reasoning models whose hidden
   reasoning eats the same budget as final output — this is exactly one
   of the context-window-management risks the standing task itself asks
   the model to analyze.
4. **The offload-preview path can trigger a second-order,
   self-inflicted thrash.** Repro: read a large out-of-scope file so it
   gets offloaded (`/tmp/kitsu-offload-*/...txt`, 500-char in-context
   preview); in both attempts the model then issued further
   `fs_read_file` calls against its *own offloaded output* in shrinking
   ranges (700→400→350→150 lines) rather than recognizing it already
   had the content. Milder than cycle 1's thrash but the same underlying
   shape: the model unsure how much of a file it already has visibility
   into.
5. **The literal cycle-2 command (no explicit `--timeout-secs`) fails
   fast against the CLI's 120s default** as soon as the model does
   anything beyond the minimum scoped reads — the supervisor deviated
   from the literal spec on the retry (`--timeout-secs 300`, matching
   cycle 1's own precedent) specifically to give the model room to
   finish; even then it failed for a different reason (finding 3). The
   loop's standing-task guidance doesn't currently pin a
   `--timeout-secs` value, which made this cycle's attempt-1 failure
   mode a process gap as much as a kitsu one.

**Improvements to schedule:** (a) surface partial/cancelled-call usage
so timed-out turns aren't a total token-cost blackout (weakness 1); (b)
give the model (or the operator) visibility into cumulative sub-agent
time/token cost mid-turn, and/or cap sub-agent depth or count per turn
(weakness 2); (c) consider a reasoning-headroom option — either a
separate hidden-reasoning budget or a warning when `--max-output-tokens`
looks small relative to a known reasoning model (weakness 3); (d) make
the offload preview disclose enough (e.g. line-range coverage) that a
re-read of the model's own offloaded output isn't the default move
(weakness 4); (e) pin `--timeout-secs 300` explicitly in this loop's
standing task/rules doc so cycle 3 doesn't repeat attempt 1's
easily-avoidable timeout.

### Cycle 3 (2026-08-13)

**Run stats.** Two attempts (one run + the one permitted retry, per the
loop's cost bound), both rejected by the Hugging Face router before any
inference began — a new failure mode, distinct from cycles 1 and 2, and
external to kitsu.

| | attempt 1 | attempt 2 (retry, after 60s cooldown) |
|---|---|---|
| wall time | <1s | <1s |
| tool calls | 0 | 0 |
| outcome | `HTTP 402` client error, `terminal=true`, on the *first* of 3 internal attempts | identical: `HTTP 402`, `terminal=true`, first attempt |
| `kitsu-context` | `{"used":1908,"effective":117072,"window":131072}` (pre-flight estimate, printed) | identical |
| `kitsu-usage` | none printed (no tokens billed — request never reached inference) | none printed |

Both attempts used the run command specified by the authoritative rules
doc (`--timeout-secs 300 --max-output-tokens 14000`), piping the same
scoped, spawn-forbidding task prompt (whole-file read of `context.rs`,
three fixed ranges for `orchestrator.rs`, directory-list-then-targeted-
read for `kitsu-providers/src`). Combined tokens burned: effectively
zero — the router rejected both requests before any prompt tokens were
processed.

**Deliverable grade: F (no deliverable), for a new, external reason.**
`reviews/kimi-cycle-3.md` is a supervisor-authored failure record, not a
model-authored caching/context-window review — the model never received
the task prompt in either attempt. Unlike cycles 1 (ranged-read thrash
then `429`) and 2 (wall-clock/output-budget exhaustion after real tool
calls), cycle 3's failure happened at the HTTP layer before kitsu ever
sent the request body: `client error; HTTP 402; request rejected;
terminal=true` on the very first internal attempt. This is a Hugging
Face account billing/quota condition for the `HF_TOKEN` used against
`moonshotai/Kimi-K3`, not a kitsu code defect. The standing
caching/context-window review carries over to cycle 4 unchanged, and
should not be re-attempted until HF billing/quota is confirmed clear —
retrying blind will reproduce this same sub-second failure.

**Positive finding: 402 handling is correct and cheap.** kitsu classified
the `402` as `terminal=true` on the first of its three internal retry
slots and stopped immediately, rather than spending two more retries and
their backoff delays (contrast cycle 1's `429` handling, which burned all
3 attempts with near-zero effective backoff before discarding a turn with
298s of timeout budget still unused). The surfaced error — `"chat request
was rejected for billing or quota (HTTP 402) - check provider credits and
billing settings"` — is produced at `crates/kitsu/src/main.rs:1402` and is
immediately actionable; the supervisor did not need to inspect any code
to diagnose the failure class. No new kitsu weakness is filed for the 402
path itself.

**Receipt behavior clarified (not a bug).** `kitsu-context:` printed
(`used:1908`) on both attempts even though the call never reached the
provider, while `kitsu-usage:` printed nothing on either attempt. Traced
to `crates/kitsu-render/src/lib.rs:1770-1776`: `usage` and `context` are
independent `Option`s passed to the turn-stats renderer, and
`write_context_record` (a local, pre-flight estimate) fires whenever
`context` is `Some` regardless of whether `usage` (which only exists
after a provider response carries billed tokens) is `Some`. This cycle's
zero `kitsu-usage` output is truthful — no tokens were ever billed — and
should **not** be conflated with the cycle-2 backlog item below (turns
that spent real inference time and *still* printed zero usage); that
item remains open and unconfirmed either way by this cycle's evidence.

**Weaknesses observed (ranked):** none new this cycle attributable to
kitsu itself — the sole failure mode (HF billing/quota `402`) is external
and kitsu's handling of it was fast, correctly classified as terminal,
and clearly reported. All backlog items below carry over unchanged from
cycle 2; none were confirmed or refuted by this cycle's evidence (the run
never got far enough to exercise the ranged-read path, the sub-agent
spawn path, the reasoning-headroom path, or the offload-preview path —
zero tool calls were made in either attempt).

**Improvements to schedule:** (a) before scheduling cycle 4, verify HF
router billing/quota status for the loop's `HF_TOKEN` against
`moonshotai/Kimi-K3` — this is an operational/account task, not a code
change; (b) all cycle 1/2 backlog items (a-e above) remain open and
untouched by this cycle; carry forward as-is.

### Cycle 4 (2026-08-14)

**FIRST BANKED DELIVERABLE.** After three straight failed cycles (1:
ranged-read thrash → `429`; 2: unbounded sub-agent detour → wall-clock/
output-limit exhaustion; 3: HF `402` billing block), pre-flight passed
(`HTTP 200`) and the ramp's deliberately narrow cycle-4 task — one named
file, one read, one write, no sub-agents — succeeded on the **first
attempt, no retry needed**.

**Run stats.**

| | attempt 1 (no retry required) |
|---|---|
| wall time | 59s |
| tool calls | 2 (1 `Read`, 1 `Write`) — zero extraneous reads, zero list calls, zero `Agent` spawns |
| `kitsu-usage` prompt/completion/total | 17,617 / 6,282 / 23,899 |
| `kitsu-context` used/effective/window | 8,911 / 117,072 / 131,072 |
| truncation warnings | 0 |
| outcome | committed: `reviews/kimi-cycle-4.md`, 133 lines / 8,207 bytes, 5 ranked proposals |

Total tokens (23,899) came in well under the cycle's 40K target and far
below cycles 1-2's 187K-434K burns — confirming the ramp's core thesis:
scoping the task to one named file with no sub-agent path eliminates
essentially all of the failure surface the first three cycles hit.

**Deliverable grade: A (pass).** `reviews/kimi-cycle-4.md` is a genuine,
model-authored review of `context.rs` alone: 5 ranked proposals (task
asked for 3-6), each with exact line citations, a concrete change, a
stated reason it wins, and a stated risk — no vague advice. **Every cited
line range was spot-checked against the actual file
(`/tmp/kimi-k3-cycle1/crates/kitsu-core/src/context.rs`) and found
accurate — zero hallucinated citations.** Spot-check detail: `estimate`
100-107, `anchored` 111-113, `anchor_includes_schema` 122-124, `record`
79-81, `invalidate` 83-93, `message_tokens` 35-43, rate constants 16-18,
`estimate_str` 29-31, `buffer` line 166, `ContextBudget::new` 159-173,
`MIN_OUTPUT_RESERVE` 24-25, `DEFAULT_CONTEXT_WINDOW` 20-22, anchor-field
doc 63-68 — all confirmed to describe exactly what those lines contain.
Numeric reasoning was also checked: proposal 5's worked example
(`window=8192, max_output=Some(8192)` → `reserve = 8192.max(512).min(4096)
= 4096`) is arithmetically correct against `ContextBudget::new`'s actual
formula. Actionable-vs-vague ratio: 5/5 proposals actionable (concrete
code change named, not just "improve accuracy"). Top proposals worth
acting on:

1. **`crates/kitsu-core/src/context.rs:100-107`** — anchor validity is
   length-only (`sent_len <= messages.len()`), so an in-place edit inside
   the anchored prefix (steering edit, rewritten tool result) keeps the
   anchor "valid" while silently describing content it never measured.
   Proposed fix: a cheap prefix fingerprint or generation counter checked
   alongside the length guard.
2. **`crates/kitsu-core/src/context.rs:79-81` + `:100-107`** — no sanity
   bound on gateway-reported `prompt_tokens`; a buggy/proxying gateway
   value poisons every estimate until the next `record`. Proposed fix:
   distrust an anchor that diverges from the anchor-free full estimate
   beyond a wide band (e.g. 0.4x-2.5x), falling back to full recompute.
3. **`crates/kitsu-core/src/context.rs:166`** — trim buffer is
   `min(window/10, 2048)`, flat above a 20,480-token window even though
   absolute chars/token estimate error grows with prompt size; proposed
   scaling the cap (or deriving it from fill-proportional error) so
   large-context models get proportionally more headroom.

**Weaknesses/observations (none rise to a new backlog item — this was a
clean run):**

1. **None of cycles 1-3's failure modes were exercised, by design.** No
   ranged-read thrash (only one whole-file read, no ranges), no `429`/
   `402` (pre-flight already confirmed `200`), no sub-agent spawn (task
   forbade it and the model complied — first cycle where the prohibition
   actually held), no wall-clock cancellation, no truncation. This cycle
   provides zero new evidence on backlog items 1, 2, 5, 6, 7, 8, 10
   below — they carry forward unconfirmed, not because they were
   refuted but because the narrow task never touched those paths.
2. **The widened `--max-output-tokens 14000` (vs. cycle 2's 6000) appears
   to have been sufficient headroom for a reasoning model on a
   read-one-write-one task**: `completion_tokens: 6282` against the
   14000 cap, zero truncation warnings, full deliverable produced. This
   is operational mitigation, not a kitsu fix — backlog item 9
   (no separate reasoning-headroom knob/warning) remains open as a real
   gap; a larger or more reasoning-heavy task at the same cap could still
   exhaust it the way cycle 2 did at 6000.
3. **`kitsu-usage` did not report `cached_tokens`**, consistent with
   backlog item 2 (still open, unconfirmed either way — this run may
   simply not have had cacheable content given it was a fresh session).
4. **Backlog item 7 (cancelled turns print zero usage) was not
   exercised** — no cancellation occurred this cycle; still open per
   cycle 3's note that it remains unconfirmed/unrefuted.

**Ramp verdict: ADVANCE to cycle 5.** The first-success gate is cleared —
Kimi-K3 has a genuine, accurately-cited, banked contribution. Proceed to
the cycle-5 task (usage/caching slice over `crates/kitsu-providers/src`)
per the Success ramp at the top of this doc.

### Cycle 5 (2026-08-14)

**FAILED — output-budget truncation, no retry taken.** Pre-flight passed
(HTTP 200); the cycle-5 usage/caching task (read `types.rs` whole +
`openai_compat.rs` in two ranges, then write a two-part review) reached the
model cleanly but was truncated by `--max-output-tokens 14000` before any
visible output was produced. This is **not** a second consecutive banked
deliverable — the cycle-4→5 streak is broken.

**Run stats.**

| | attempt 1 (no retry — see below) |
|---|---|
| wall time | 106.8s |
| tool calls | 3 (`Read` types.rs whole, `Read` openai_compat.rs 1-850, `Read` openai_compat.rs 850-1694) — 0 list calls, 0 `Agent` spawns |
| ranged-read behavior | both ranges on the 1,694-line file returned full width instantly (0ms each) — the range-cap fix (confirmed cycle 2, re-confirmed cycle 4) held again under this cycle's load |
| visible output before truncation | one sentence ("I'll read the two files now — all three ranges in parallel."), then nothing |
| `kitsu-usage` prompt/completion/total | 19,620 / 14,322 / 33,942 — **completion_tokens exceeded the nominal 14,000 cap** (first cycle where this happened, not merely approached it) |
| `kitsu-context` used/effective/window | 17,007 / 117,072 / 131,072 |
| truncation warnings | 1 (`response truncated: model hit its output length limit`) |
| outcome | provisional output DISCARDED, 0 files written, exit code 2 |

**Why no retry.** The loop's rules permit a retry only "on transport
failure" (HTTP-layer errors like cycle 1's `429` or cycle 3's `402`). This
attempt was HTTP-200 throughout — both files were read successfully, and
the failure was kitsu's own output-length enforcement, not a network/rate-
limit/billing fault. Retrying the identical command would very likely
reproduce the same truncation, so no retry was taken; the cycle is recorded
as a single-attempt failure per the rules as written.

**Deliverable grade: F (no model deliverable).** `reviews/kimi-cycle-5.md`
is a supervisor-authored failure record (following the cycle 1/3
precedent), not a Kimi-authored review — there were no model citations to
spot-check. To keep the record useful, the supervisor independently
grounded the `cached_tokens`-drop half of the task directly in the same two
files: `crates/kitsu-providers/src/types.rs:136-144` defines `Usage` with
exactly `prompt_tokens`/`completion_tokens`/`total_tokens`, no
`deny_unknown_fields`, so HF's `prompt_tokens_details.cached_tokens` is
silently dropped by serde during deserialization before any Rust code sees
it; both the streaming path (`openai_compat.rs:252/294/373-374/449-453/
534-535`) and non-streaming path (`openai_compat.rs:616-618`) decode into
that same `Usage` type, so the drop is uniform across both. The 2-4 ranked
caching proposals the task asked for were **not** attempted by the
supervisor (that synthesis needs the full 1,694-line file read the model
never got to use) and remain open for the next attempt.

**Weaknesses observed (ranked):**

1. **Reasoning-token exhaustion can now overshoot the cap, not just
   approach it.** Repro: same command class as cycle 2 (reasoning model,
   tool-heavy but small turn) except this time `completion_tokens: 14322`
   *exceeded* `--max-output-tokens 14000` outright, versus cycle 2's
   `8852` against `6000` (also an overshoot, but on a much smaller,
   already-known-insufficient budget) and cycle 4's clean finish at
   `6282`/`14000`. This reconfirms the still-open backlog item
   ("reasoning models can exhaust `--max-output-tokens` entirely on hidden
   reasoning") with a second, independent data point on a *generous*
   budget (14,000, more than double cycle 2's), showing the failure mode
   scales with task complexity (two files + two-part analytical ask) more
   than with the nominal cap size.
2. **A turn that read 100% of its scoped material still loses everything
   on truncation.** Repro: this attempt performed exactly the 3 reads the
   task named, cleanly, then was truncated — reconfirming the open backlog
   item that failed turns discard all prior tool-call work wholesale, this
   time with the smallest possible "wasted work" case (3 clean, correctly-
   scoped reads, zero thrash) still fully discarded.
3. **The range-cap fix continues to hold under real load** — both ranges
   of the 1,694-line `openai_compat.rs` came back full-width in 0ms,
   matching cycles 2 and 4's confirmation. No new evidence needed here;
   noted only because this cycle's file was the largest single ranged-read
   target attempted so far (1,694 lines vs cycle 2's 3,221-line file read
   in three ranges, so still smaller, but the largest under the *current*,
   narrower ramp scope).

**Backlog status:** no items resolved this cycle. Reconfirmed (not newly
opened): the reasoning-headroom item (now with two independent overshoot
data points) and the failed-turn-discards-everything item. The
`cached_tokens` backlog item is grounded more precisely (exact struct and
line numbers identified above) but remains unchecked — no code change was
made, and this run's own `kitsu-usage:` line predictably had no
`cached_tokens` key (fresh single-turn session, not proof either way).

**Ramp verdict: RETRY cycle 5** (not advance to cycle 6). Before the next
attempt: raise `--max-output-tokens` further (e.g. 20,000-24,000) for any
task asking the model to read *and analyze* two non-trivial files, or split
the task further (locate-and-cite the drop as one narrow attempt, produce
the ranked caching proposals as a second) — mirroring the ramp's own
strategy of narrowing scope until a turn reliably completes, applied one
notch further given this cycle's evidence that two-file analytical tasks
need materially more budget than cycle 4's single-file, single-ask task.

### Cycle 5 — attempt 2 (2026-08-14)

**SECOND BANKED DELIVERABLE.** Pre-flight passed (`HTTP 200`). Retry
changes per the attempt-1 verdict: `--max-output-tokens` raised
14,000 → 24,000, plus a brevity instruction added to the task prompt
("Think briefly — a short plan then act; long deliberation wastes your
output budget") on top of the existing sub-agent prohibition. Both
changes together produced a clean, complete run.

**Run stats.**

| | attempt 2 (single run, no transport retry needed) |
|---|---|
| wall time | 71s (kitsu's own run; a first launch was killed at 120s by the *supervisor's own* shell-timeout wrapper, not by kitsu — see "Harness note" below) |
| tool calls | 5 (`Read` types.rs whole, `Read` openai_compat.rs 1-850, `Read` openai_compat.rs 850-1694, `Write` reviews/kimi-cycle-5.md [failed: conflict], `Read` reviews/kimi-cycle-5.md, `Write` reviews/kimi-cycle-5.md [succeeded]) — 0 list calls, 0 `Agent` spawns, exactly the two files named and nothing else |
| ranged-read behavior | both `openai_compat.rs` ranges returned full width in 0-1ms — range-cap fix holds under load again |
| `kitsu-usage` prompt/completion/total | 84,961 / 8,129 / 93,090 — **completion_tokens 8,129 against the 24,000 cap (34% utilization, well under, zero truncation warnings)**; prompt_tokens are 4.3x attempt 1's 19,620, plausibly from the extra failed-write/read-back round-trip re-sending cumulative conversation history across one more round, though the exact mechanism wasn't traced |
| `kitsu-context` used/effective/window | 25,445 / 107,072 / 131,072 |
| truncation warnings | 0 |
| outcome | committed: `reviews/kimi-cycle-5.md` overwritten, 184 lines / 8,330 bytes, full two-part review; run summary "1 turn(s) ok, 4 file(s) read, 1 file(s) written (1 change(s)), 1 failed tool call(s), session not saved" |

Total tokens (93,090) landed just under the cycle's own "< 100K total
tokens" target. The key ramp datum: raising the cap and adding the
brevity instruction were not isolated from each other in this test
(both changed at once), but the completion-token spend (8,129) stayed
far under this cycle's 24,000 cap and nowhere near attempt 1's
14,322/14,000 overshoot — modestly above cycle 4's single-file
6,282, consistent with a two-file, two-part task costing somewhat more
than a one-file task, but with none of the runaway-reasoning behavior
attempt 1 showed. Suggests the combination of headroom + explicit
brevity instruction is an effective mitigation for this task shape.

**Harness note (not a kitsu weakness).** The first launch of this
attempt was killed by the *supervisor's* own Bash-tool default 120s
timeout, not by kitsu's `--timeout-secs 300` — kitsu had already
completed 3 clean reads and one failed `Write` (see below) when
terminated. The partially-written state was verified safe (the target
review file was confirmed byte-identical to its pre-run content before
the second, successful launch), and the second launch with a longer
harness timeout completed normally. Filed here for the record, not as
a kitsu backlog item, since the cause was external tooling, not kitsu.

**Deliverable grade: B+ (strong pass, with a real citation-precision
caveat).** `reviews/kimi-cycle-5.md` is a genuine Kimi-authored
two-part review satisfying both halves of the task:

- **(a) Drop location + fix.** Correctly identifies both parse sites
  (`StreamChunk.usage` for streaming, `CompletionResponse.usage` for
  non-streaming) deserializing into the shared 3-field `Usage`
  (`types.rs`), correctly explains the serde silent-drop mechanism, and
  additionally identifies a real edge case the attempt-1 supervisor
  grounding missed: `Usage::is_empty()` would still discard a
  cache-hit-only chunk (all-zero prompt/completion/total but nonzero
  `cached_tokens`) even after a naive fix. The proposed edit (new
  `PromptTokensDetails` struct + `Option` field + `cached_tokens()`
  accessor + extended `Usage::add` + widened `is_empty` gates) is
  concrete, additive/backward-compatible (`#[serde(default)]`), and
  correctly notes `ChatDelta::Usage` already carries the struct through
  with no trait-signature changes needed.
- **(b) Ranked proposals.** 3 proposals (task allowed 2-4): (1) add
  `prompt_cache_key` to `ChatRequest`/`WireRequest` — **verified true**,
  `grep` for `prompt_cache_key`/`cache_control` across both files
  returns zero hits, so the gap is real; (2) widen the `is_empty()`
  gate — follows directly from (a); (3) cache parsed
  `model_context_length` results alongside the raw catalog body —
  grounded in the real `models_cache` mutex-held-across-fetch structure.
  Explicitly rejects two ideas as ungrounded (`cache_control`
  breakpoints, response memoization) rather than padding the list —
  good discipline, matches the task's "skip anything you cannot
  ground" instruction.
- **Citation-precision spot-check (the reason this isn't an A).**
  Every cited region was checked against the actual worktree file.
  `types.rs` citations are accurate to within a few lines throughout
  (e.g. `WireMessage` struct cited "~133-141", actual 133-141 — exact).
  `openai_compat.rs` citations split into two bands: citations near the
  file's start (`WireRequest` "~107-126" vs actual 109-126;
  `WireMessage` exact) and in the 1070-1290 region (`sse_step`
  "~1090-1145" vs actual 1077-1149, contained; `model_context_length`
  "~1200-1250" vs actual 1271-on, in the right neighborhood) are close.
  Citations in the 240-620 region drifted materially: `StreamChunk.usage`
  cited "~283-294" (actual struct at 244-253 — the cited range instead
  lands near `CompletionResponse`'s real location); the usage-capture
  line in `process_line` cited "~430-436" (actual capture at 449-455);
  `finalize`'s `ChatDelta::Usage` emission cited "~488-491" (actual
  534-537); `CompletionResponse.usage` cited "~337-342" (actual struct
  at 289-295); its surfacing in `parse_completion` cited "~590-595"
  (actual 616-620). None of these are hallucinated — the *described
  content* at each citation (field names, gate logic, struct shape) is
  correct — but the *line numbers* are off by 20-48 lines for roughly
  five of the ~14 distinct citations in this file, despite every
  citation being hedged with "~" and despite the model having just read
  both files with kitsu's own line-numbered `Read` output. Given the
  task's explicit ask for "line-cited edits" and "cite lines you
  actually read", this is a real quality gap, not a minor rounding
  issue — an engineer following these citations directly would land on
  the wrong code in about a third of cases in that file region.

**Agreement vs. attempt-1 supervisor grounding.** Kimi's analysis
agrees with the attempt-1 supervisor's grounding
(`types.rs:136-144`, no `deny_unknown_fields`, uniform drop across
both wire paths) on every substantive point, and extends it with one
concrete new finding neither the standing backlog item nor attempt 1
identified: the `is_empty()` gate would independently discard
cache-hit-only usage chunks. No divergence or contradiction found.

**Weaknesses observed (ranked):**

1. **Citation line-number drift under real load, most concentrated in
   one file region.** Repro: same command class as this cycle; ask a
   reasoning model to line-cite specific structs/functions across a
   ~1,700-line file it just read via two ranged `Read` calls (each with
   kitsu's own line-numbered output) and independently verify the
   citations. About a third of the citations in the file's 240-620 line
   region drifted 20-48 lines from the true location, while citations
   elsewhere in the same file (and in the smaller, 172-line companion
   file) were accurate to within a few lines. This is a genuine model
   behavior kitsu cannot fix directly, but it's worth tracking across
   cycles as a data point on how much to trust unverified line
   citations from this model at this file size — carries forward as a
   new observation, not a kitsu code defect.
2. **The write tool's blind-overwrite guard cost one extra round-trip.**
   Repro: instruct the model to "overwrite" an existing file with
   `Write` directly (as this cycle's prompt did); the first `Write`
   attempt failed with `write conflict: replacing an existing file
   requires expected_previous_content; read it first or use
   fs_edit_file` (an `ExecError`, 1ms). The model recovered within the
   same turn (`Read` then `Write` succeeded, "1 earlier tool error(s)
   were recovered within the turn"), so the turn wasn't lost, but the
   round-trip added a `Read` of the ~6.9KB existing file it didn't
   otherwise need, growing prompt tokens. The tool's own description
   apparently doesn't warn the model up front that overwriting a
   file it hasn't read in *this* turn requires a prior `Read` — worth
   considering whether the `Write` tool description could state this
   precondition explicitly so a model that already intends to overwrite
   doesn't have to discover it via a failed call.
3. **Reasoning-headroom mitigation (cap + brevity instruction together)
   worked for this task shape** — third data point on the open
   backlog item, and the first cycle-5-scoped one that did *not*
   overshoot: 8,129/24,000 (34%), vs. attempt 1's 14,322/14,000 (102%)
   and cycle 2's 8,852/6,000 (147%). This is operational mitigation
   (bigger cap + explicit prompt instruction), not a kitsu code change,
   so the underlying backlog item (no separate reasoning-headroom
   knob/warning in kitsu itself) remains open — but it's useful
   evidence that prompt-level brevity instructions measurably help,
   independent of raising the cap.

**Backlog status:** `cached_tokens` item updated above with Kimi's
additional `is_empty()`-gate finding (still unchecked — no code
change made). No other backlog items resolved this cycle; the
write-tool round-trip observation above is filed as a new,
low-severity item.

**Ramp verdict: ADVANCE to cycle 6.** The retry succeeded on the first
attempt after the prescribed budget raise plus a brevity instruction.
Two banked deliverables now exist (`reviews/kimi-cycle-4.md`,
`reviews/kimi-cycle-5.md`); proceed to cycle 6 synthesis per the
Success ramp.

### Cycle 6 (2026-08-14)

**THIRD BANKED DELIVERABLE — RAMP COMPLETE.** Pre-flight was not run
separately this cycle (the prior tick's `HTTP 200` confirmation on
cycle 5 attempt 2 stood); the run went straight to the synthesis task
and succeeded on the **first attempt, no retry needed** — the cleanest
run of the ramp so far.

**Run stats.**

| | attempt 1 (no retry required) |
|---|---|
| tool calls | 6 (`Read` kimi-cycle-4.md whole, `Read` kimi-cycle-5.md whole, `Read` context.rs whole, `Read` orchestrator.rs 700-900, `Read` orchestrator.rs 2100-2350, `Write` reviews/kimi-cycle-6.md) — zero extraneous reads, zero list calls, zero `Agent` spawns, zero write conflicts |
| ranged-read behavior | both orchestrator.rs ranges (201 and 251 lines) returned full width in 0ms — range-cap fix holds under load again, fourth confirmation (cycles 2, 4, 5, now 6) |
| `kitsu-usage` prompt/completion/total | 36,473 / 5,991 / 42,464 — **completion_tokens 5,991 against the 24,000 cap (25% utilization)**, well under, zero truncation warnings |
| `kitsu-context` used/effective/window | 19,270 / 107,072 / 131,072 |
| truncation warnings | 0 |
| write conflicts | 0 — direct `created` on the first `Write` call, unlike cycle 5 attempt 2's failed-then-recovered overwrite (expected: cycle 6 targets a brand-new file, so the blind-overwrite guard never triggers) |
| outcome | committed: `reviews/kimi-cycle-6.md`, 250 lines / 15,890 bytes, 11 ranked proposals + top-5 list |

Total tokens (42,464) landed at roughly 21% of the cycle's 200K target —
the most token-efficient cycle of the ramp per unit of deliverable
content (250 lines / 15.9 KB, more than either source review alone).
Wall time was not separately timed by the supervisor this cycle (no
`time` wrapper was used); every individual tool-call receipt reported
0-2ms, so the bulk of elapsed time was model generation, and the run
completed comfortably inside the 300s `--timeout-secs` deadline with no
harness-timeout risk (the harness's own Bash timeout was left at the
tool default, which was sufficient — no repeat of cycle 5 attempt 2's
120s-wrapper kill).

**Deliverable grade: A (pass, strongest citation precision of the
ramp).** `reviews/kimi-cycle-6.md` is a genuine synthesis, not a
re-listing: 8 proposals CARRIED (5 from cycle 4: anchor fingerprint,
gateway-usage sanity clamp, trim-buffer scaling, message-token
overhead/bytes-vs-chars, reserve-vs-max_output mismatch; 3 from cycle
5: `prompt_cache_key`, `cached_tokens` parsing + `is_empty()` edge
case, model-context-length caching) plus 3 **NEW**, explicitly grounded
in the orchestrator ranges just read: P6 (`ContextWindowEvidence` /
`context_window_shortfall` diagnose truncation but never feed back into
the session budget), P7 (`anchor_includes_schema` is a bare bool, so a
mid-session tool-registry change fools it — the same count-vs-identity
flaw as the carried P2, one level up), and P11 (`ensure_chat_request_fits`
re-serializes tool schemas on every provider-boundary check). Every
CARRIED/NEW label is present and correct; nothing is mislabeled.

**Spot-check results.** All carried `context.rs` citations
(`message_tokens` 35-43, `record` 79-81, `invalidate` 83-93, `estimate`
100-107, `anchored` 111-113, `anchor_includes_schema` 122-124,
`ContextBudget::new` 159-173, trim `buffer` line 166,
`DEFAULT_CONTEXT_WINDOW` 20-22, `MIN_OUTPUT_RESERVE` 24-25,
`estimate_str` 29-31) verified **exact** against the file read this
cycle. All three NEW orchestrator citations verified against the actual
700-900/2100-2350 ranges and found accurate to within single digits to
~15 lines — materially tighter than cycle 5's 20-48 line drift in its
dense 240-620 region: `AgentSessionState` (cited ~745-770, actual
struct 756-777, doc comment from 746), `ContextWindowEvidence` (cited
772-791, actual 779-796 — exact for the doc/struct span), `context_
window_shortfall` (cited 793-820, actual fn 804-819, doc from 798 —
contained), `request_measurement` (cited ~2165-2212, actual fn
2172-2217 — tight), `ensure_chat_request_fits` doc line (cited ~2218-
2233, actual fn 2233-2252, "last-line guard used at every provider
boundary" comment quoted verbatim from line 2232). One quick
cross-file spot-check also confirmed P1's premise (`prompt_cache_key`/
`cache_control`, carried from cycle 5): zero grep hits across
`kitsu-providers/src`, and `WireRequest` (openai_compat.rs 109-126) has
no such field — accurate.

**This completes the ramp's founding goal.** Three banked deliverables
now exist on `dogfood/kimi-k3` — cycle 4 (A, single-file), cycle 5
attempt 2 (B+, two-file usage/caching), cycle 6 (A, synthesis) — the
exact three-stage arc the Success ramp was designed to prove: narrow
scope to bank a first success, widen one notch with headroom lessons
applied, then synthesize both into the review the loop originally
wanted. The loop went from three straight F cycles (1-3, all external
or infrastructure failures: ranged-read cap bug, unbounded sub-agent
detour, HF 402 billing block) to three consecutive real deliverables
once the task shape was disciplined to the model's demonstrated
capability.

**Weaknesses observed (ranked):** none new this cycle — it was the
cleanest run of the ramp (no thrash, no truncation, no write conflict,
no spawn, no cancellation).

1. **Completion-token spend continues to track task complexity, not a
   fixed floor, and this is now the fourth data point on that curve.**
   Cycle 2 (single wide read + off-task detour): 8,852. Cycle 4
   (one file, one read, one write): 6,282. Cycle 5 attempt 1 (two files,
   two-part ask, no brevity instruction): 14,322 (overshot its 14,000
   cap). Cycle 5 attempt 2 (same two-file, two-part ask, brevity
   instruction added): 8,129. Cycle 6 (five reads across four files
   including two prior reviews, one synthesis write, brevity
   instruction present): 5,991 — the **lowest** completion spend of the
   ramp despite reading the most material (2 review files + 1 whole
   source file + 2 orchestrator ranges = ~1,043 lines of source input,
   more than any prior cycle). This suggests synthesis-style tasks
   (re-arranging and citing already-produced content, versus generating
   novel analysis from a cold read) may be cheaper in hidden-reasoning
   tokens than first-pass analytical tasks of similar or smaller input
   size — worth testing directly in a future cycle if the loop
   continues past the ramp, but not something to file as a kitsu defect
   (it's a property of the task shape, not kitsu's handling of it).
2. **Zero write conflicts this cycle, as expected** — cycle 5 attempt
   2's backlog item (write tool's blind-overwrite guard costing a
   round-trip) is about *overwriting an existing file*; cycle 6 wrote a
   brand-new file, so the guard's precondition never applied. This
   cycle neither confirms nor refutes that backlog item; it remains
   open, unchanged.

**Backlog status:** no items resolved or newly opened this cycle. The
`cached_tokens` item (with its `is_empty()`-gate refinement) and the
write-tool round-trip item both carry forward unchanged from cycle 5;
neither was touched by code changes, only re-cited by the cycle-6
synthesis.

**Ramp verdict: RAMP COMPLETE.** All three stages of the Success ramp
delivered a graded, citation-verified artifact (A, B+, A). The standing
loop can now choose: (a) retire the ramp and treat future ticks as
maintenance/backlog-triage cycles referencing `reviews/kimi-cycle-6.md`
as the definitive review, (b) start a *new* ramp on a different focus
area (e.g. the compaction/summarization cascade, or the tool-schema/
sub-agent cost-visibility gaps cycle 2 surfaced), or (c) use a future
cycle to actually implement one or more of P1-P11 against `kitsu-core`/
`kitsu-providers` now that they're ranked and grounded. No further
"cycle N+1" is implied by the ramp itself — the next tick should pick
one of these three explicitly rather than defaulting to "cycle 7" with
no stated task.

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
- **"Flake" root-caused and FIXED (commit 1bb6b81, Kate):** not a flake —
  a stale test assertion. `read_file_caps_input_before_allocating_the_whole_file`
  asserted the OLD truncation marker and a `+64` length bound, but the
  branch's code already emits the cycle-31 served-of-total disclosure
  (longer). The branch got the new disclosure code without the matching
  test update; main had updated both. Mirrored main's assertion (substring
  marker + new suffix + `+256` bound). The kimi branch's kitsu-tools test
  target is now clean — merge-blocker removed. Unrelated to any Kimi cycle.

**Backlog: P1 ✅ P2 ✅ P3 ✅ P4 ✅ P5 ✅. Kimi is 6-for-6 on code work
(cycles 7-12).** Next: P6 evidence-clamps-budget, P7 anchor schema
fingerprint, P10 model-context caching, or P11 tool-schema re-serialization.

### Cycle 13 — 2026-08-14 — P6 evidence-clamps-budget (grade B+)

Kimi implemented P6: after a two-strike shortfall for the same resolved
window, clamp the EFFECTIVE SESSION BUDGET toward the reported size —
never rewriting config or the resolved window. Pure
`clamp_window_for_shortfall(resolved, reported, count)` with
`MIN_VIABLE_WINDOW_FLOOR = 2048`, exact reuse of the `reported*4 >=
resolved` plausible_clamp guard, two-strike gate. 6 tests (4 Kimi + 2
supervisor integration). Committed `366dfc3` (Kate verified), pushed.
Never-rewrite contract verified by reading (cfg.context_window /
resolved_window are only read/compared, never assigned). kitsu-core
110/110, all crates green (cycle-12's flake fix held).

**MAJOR kitsu bug uncovered (root cause of cycles 9-12's "supervisor
mechanical fixes"):** the journaled Edit/Write snapshots the WHOLE
existing file before editing, hard-capped at `DEFAULT_MAX_BYTES = 64 KiB`
(`kitsu-tools/src/permissions.rs:28`), with no CLI override. `orchestrator.rs`
is 129 KiB — **over 2x the cap — so NO model can Edit/Write that file at
all**, regardless of diff size; every edit fails `existing file exceeds
the 65536-byte journal limit`. This silently forced the supervisor to
hand-apply every orchestrator.rs change in cycles 9-13. Attempt 1 died on
it (round budget burned + timeout); the retry had Kimi edit only
context.rs (36 KiB, under cap) while the supervisor applied the
orchestrator wiring. **Filed as a top-priority fix — being fixed on main
this turn so all loops can edit large files (openai_compat.rs is 63 KiB,
about to cross the cap and block GLM's P10 cycle).**

- Docked to B+ (from A-) for the larger-than-usual supervisor wiring
  share — but that was forced by the journal-cap blocker, not a Kimi
  shortfall; the design and the clamp function are Kimi's and correct.
  cap-1 absorbed one attempt-1 spawn attempt (errored on its own
  preflight, no crash) and zero on the retry.

**Backlog: P1-P6 ✅. Kimi is 7-for-7 on code work (cycles 7-13).** Next:
P7 anchor schema fingerprint (context.rs, under cap), P11, or another
context.rs-scoped item — AVOID orchestrator.rs direct edits until the
journal-cap fix lands in the built binary.

### Cycle 14 — 2026-08-14 — P7 anchor schema fingerprint (grade A-)

Kimi implemented P7: a fifth anchor-tuple field, `schema_fingerprint(schema_json)
-> u64` (a `DefaultHasher` over the full serialized schema — reasoned as
still cheap since schema JSON is small and built once/round, and catches
same-length tool swaps a length-only hash would miss). `record()` gained
a `schema_json: &str` param; `anchor_includes_schema()` now requires the
stored fingerprint to match the current schema's before treating the
anchor as schema-covered — closing the count-vs-identity gap for tools,
the same flaw P2 fixed for message content. Committed `9a6b8b5` (Kate
verified), pushed. cap-1 held, zero spawns. kitsu-core 113/113.

- **SECOND production validation of the journal-cap fix (R1.6):** Kimi
  edited `orchestrator.rs` (129 KiB) DIRECTLY — the file no model could
  touch through cycle 13 — with NO journal-limit error. (GLM validated it
  on the 63 KiB openai_compat.rs the same hour; this confirms it on the
  largest file.)
- 3 genuine tests (unchanged schema keeps the fast path; a changed tool
  set via distinct SCHEMA_A/SCHEMA_B fixtures forces schema re-estimation;
  no-schema anchor unaffected) + 3 pre-existing record() call-site updates.
- Docked to A- for one mechanical completion (see below) + hashing the
  full schema string vs the brief's suggested length-only style (a
  documented, defensible departure — catches more).
- **Supervisor-tooling lesson (not Kimi's fault):** the corrected `kitsu
  chat` run was killed by the SUPERVISOR'S OWN Bash-tool 2-minute default
  timeout, NOT kitsu's `--timeout-secs 300`. Kimi had already located the
  one remaining old call site (context.rs:622) and was about to fix it
  when killed; the supervisor applied that one arg. **Standing fix:
  supervisors must run the kitsu invocation with an explicit Bash timeout
  >= kitsu's --timeout-secs (e.g. 330000 ms for --timeout-secs 300), so a
  healthy long run is not cut off at the Bash default.**

**Backlog: P1-P7 ✅. Kimi is 8-for-8 on code work (cycles 7-14).** Next:
P8 trim-buffer scaling, P11 tool-schema re-serialization, or P9 — all now
freely editable (orchestrator.rs direct edits work post-R1.6).

### Cycle 15 — 2026-08-14 — P8 trim-buffer scales with error (grade A)

Kimi made the headroom buffer scale with fill instead of capping flat.
`ERROR_FRACTION = 0.05` const; `buffer = (effective * ERROR_FRACTION)
.max(2048).min(window/10)`. **Circularity resolved cleanly** — based on
`effective` (= window − reserve), computed BEFORE the buffer, so it never
depends on `target` (= effective − buffer). Commit `b2faeb0` (Kate
verified), pushed. Single attempt, 7 tool calls, completion 2,769/24,000
(11.5%), cap-1 held, zero spawns, purely additive.

- Small windows genuinely unchanged (32k→1,433, 8k→384, all under the
  2,048 floor) — none of the 4 pre-existing budget tests touched. Large
  window scales: 1,000,000 → buffer 49,974 (above floor, below the
  window/10=100k cap). The invariant target ≤ effective ≤ window holds.
- 3 genuine tests (floor-on-small, scales-on-large with exact 49,974,
  invariant-holds). kitsu-core 116/116, workspace green. No kitsu
  weaknesses this cycle. **Cleanest implementation cycle yet — a clear A.**

**Backlog: P1-P8 ✅. Kimi is 9-for-9 on code work (cycles 7-15).** Next:
P9 (budget-reserve/max_output consistency, context.rs+orchestrator.rs),
P11 (schema re-serialization cache, synergizes with P7), or a fresh item.

### Cycle 16 — 2026-08-14 — P9 budget/max_output consistency (grade A)

Kimi fixed the disagreement between the reply reserve and the max_output
actually SENT. `context.rs`: `ContextBudget` grew `pub reserve` +
`clamped_max_output()`, plus a free `clamp_max_tokens_to_reserve(Option<u32>,
&ContextBudget)` that clamps a caller's max_tokens DOWN to the reserve
(already-fitting values untouched, None→None). `orchestrator.rs` (~1112,
edited directly): rewrites max_tokens through the clamp before the
`ChatRequest` is built, so the number the provider is told and the number
the budget measured are now the same. Commit `99c850e` (Kate verified),
pushed. First attempt, cap-1 held, zero spawns.

- Test (a) proves window=8192/max_output=Some(8192) → reserve 4096, rewrite
  8192→4096, effective == 8192−4096; (b) already-fitting value unchanged +
  a smaller value never inflated; (c) None stays None. kitsu-core 119/119,
  purely additive. **Grade A.**

**kitsu weaknesses (two, both real):**
1. **Confirms R4.12 from a SECOND loop this hour** (GLM W5 was the first):
   an early self-recovered `PolicyDenied` (read a directory instead of a
   file, twice at the start) poisoned the WHOLE turn — the CLI discarded
   the correct clean final answer (`provisional assistant output was
   DISCARDED`) even though every later action incl. the summary succeeded;
   the edits survived only via the retained-checkpoint mechanism. The
   supervisor notes the cycle-30 "policy-denied turns commit their truthful
   answer as partial" fix (07b7b59) does NOT cover this self-recovered-
   early-failure shape. **Two independent models hit R4.12 in one hour —
   promote it.**
2. **NEW (→ roadmap R1.8): debris-detector false positive on Edit.** The
   template-debris scanner fired 3× on `Edit(orchestrator.rs)` with
   "content contains a raw model chat-template control marker" — a false
   positive: the file's OWN debris-detector code contains literal
   `<|im_start|>` etc. marker strings AS DATA, so editing near that region
   trips the scanner on pre-existing file content, not injected garbage.
   Editing any file that contains template markers as data hits this.

**Backlog: P1-P9 ✅. Kimi is 10-for-10 on code work (cycles 7-16).** Next:
P11 (schema re-serialization cache — synergizes with Kimi's own P7), or
the unwrap_or_default() serialization-swallow sweep (4 sites in
orchestrator.rs, DeepSeek-flagged; defensive), or a fresh item.

### Cycle 17 — 2026-08-14 — P11 schema-estimate cache (grade C+ — design sound, delivery incomplete)

**This closes out Kimi's original P1–P11 backlog**, but not cleanly: this
is the first cycle since the ramp where the delivered diff did not
compile and shipped zero tests, requiring substantial supervisor repair.
Recorded honestly below rather than smoothed over.

Kimi's design (correct, and preserved as-authored): `AgentSessionState`
grew `pub schema_cache: Option<(u64, String, u64)>` — (fingerprint,
schema_json, token estimate) for the tool schema, keyed by Kimi's own P7
`schema_fingerprint()` (no new hashing scheme, no registry-generation
counter, exactly as scoped). A new `cached_schema_estimate()` helper:
fingerprint the current schema, return the cached estimate verbatim on a
match, else `estimate_str()` fresh and overwrite the cache. Wired into
`ensure_chat_request_fits` (the exact "last-line guard used at every
provider boundary" the review named) via a new `schema_cache: &mut
Option<(u64, String, u64)>` parameter, and into `compact_context`'s own
schema build. `request_measurement`/`structural_request_tokens` were
refactored to take a precomputed `schema_tokens: u64` instead of
re-deriving it from `schema_json`/`has_schema` on every call — a genuine
bonus: `enforce_context_budget_recording` used to call `estimate_str`
up to 8x per invocation on an unchanged schema; it now computes it once
per call and threads the value through.

**What went wrong:** single attempt, 59 tool calls (mostly `fs_scan_file`
probes plus ~17 `Edit`s), completion 9,900/24,000 measured after the
FIRST round alone (usage receipt appeared at line 125 of a 1,619-line
stderr log — most of the run happened after it). Two early
SELF-RECOVERED `PolicyDenied` errors (a relative-path read and a
directory read of `crates/kitsu-providers/src`, both immediately
recovered from) poisoned the turn per the now-familiar R4.12 pattern —
"2 tool call(s) were denied by local policy" drove exit 1 and a
COMMITTED-AS-PARTIAL answer — but unlike prior R4.12 sightings (cycles
14, 16) where the underlying edit was actually finished, this time the
turn was ALSO genuinely cut off mid-work: the model's own trailing
reasoning stopped mid-sentence ("Wait — the `token_before` is computed
before `schema_tokens` is defined. Let me check the order in
`enforce_context_budget_recording`:") with no further tool call —
Kimi had spotted its own bug and was about to fix it when the turn
ended. Per the brief's explicit guidance, this was NOT retried as a
transport failure (it wasn't one); the disk state was verified and
repaired instead, one retry being reserved for actual transport faults.

**Supervisor repair required (larger than any prior cycle):**
1. **Compile-breaking bug**, exactly the one Kimi's own last thought
   named: `token_before = structural_request_tokens(messages,
   schema_tokens)` was emitted BEFORE `let schema_tokens = ...` in
   `enforce_context_budget_recording` — moved the `let` above its use.
2. **Two missed call-site updates** left over from the
   `structural_request_tokens`/`request_measurement` signature changes:
   `fit_summary_rebuild` and `summarize_or_fallback` still had
   `schema_json: &str, has_schema: bool` params calling the OLD 3-arg
   `structural_request_tokens` — rethreaded both to the new
   `schema_tokens: u64` shape and dropped the now-dead params (fixing a
   clippy `unused_variables` warning as a result).
3. **Five stale test call sites** in `orchestrator_tests.rs`
   (`request_measurement(&[], &c, "", false, None, ...)`) missing the new
   `schema_tokens` argument — fixed with `0` (all five pass `has_schema:
   false`, so this is the mathematically correct value, not a stub).
4. **`clippy::too_many_arguments` on `request_measurement`** (now 8
   params) — added `#[allow(...)]`, matching the exact precedent already
   used 4x elsewhere in this file for equivalent measurement-plumbing
   functions.
5. **Zero tests were authored by Kimi this cycle** (the turn was cut off
   before reaching them) — all 3 required tests are supervisor-authored:
   `test_cached_schema_estimate_reuses_cache_on_matching_fingerprint`
   (seeds the cache with a SENTINEL estimate under the real fingerprint,
   asserts it comes back verbatim through both `cached_schema_estimate`
   directly AND `ensure_chat_request_fits` across two consecutive calls —
   proves an actual cache hit, not a coincidental match, since no real
   `estimate_str` pass could ever produce the sentinel value),
   `test_cached_schema_estimate_recomputes_on_fingerprint_mismatch`
   (changed tool set → different fingerprint → fresh `estimate_str`,
   cache overwritten, old stale value never returned), and
   `test_cached_schema_estimate_correctness_matches_fresh_estimate` (cold
   and warm calls both equal a fresh `estimate_str` on the same schema —
   the cache never returns a wrong number). All three pass; grounded
   directly in Kimi's own `cached_schema_estimate`/`ensure_chat_request_fits`
   functions, not rewritten logic.

**Verification (all clean after repair):** `cargo build --workspace`,
`cargo clippy --workspace --all-targets` (zero warnings), `cargo fmt --all
-- --check` (one reformat needed on the supervisor-authored tests,
applied), `cargo test --workspace --no-fail-fast`: kitsu-core 122/122
(119 pre-existing + 3 new), kitsu-providers 64/64, kitsu-render 51/51,
kitsu-tools 141/141, kitsu 251/251 + 2 doctests, only the pre-known PTY
flake (`test_query_only_interactive_session_does_not_create_a_snapshot`)
red, unrelated and reproducing since cycles 10-15.

**Grade C+.** Docked well below the cycle 9-16 A/B range: this is the
first cycle where the delivered diff would not compile and the
required-by-brief unit tests did not exist in Kimi's own output at all —
a materially larger supervisor share than the mechanical completions of
cycles 10/13/14 (which relocated or lightly adapted Kimi-authored text).
The credit that keeps this above a flat fail: the underlying cache
design is entirely Kimi's, correctly reuses P7 without inventing a new
mechanism, the bug it ran out of budget before fixing was one it had
*already correctly diagnosed*, and the refactor's mechanical shape
(threading `schema_tokens: u64`) was consistently applied everywhere
except the two missed sites — this reads as a genuinely interrupted
turn, not a wrong design.

**kitsu weaknesses (two):**
1. **R4.12 sighted a THIRD time this loop** (cycles 14, 16, now 17): an
   early self-recovered `PolicyDenied` (here: a relative path read + a
   directory read, both auto-recovered) still discards/exit-1's the
   whole turn even when most of the turn's later work is sound. This
   cycle sharpens the earlier finding — it is not just a discard of an
   otherwise-COMPLETE answer (cycles 14/16's shape); here it likely also
   correlates with (or contributes to) truncated completion, since the
   turn's last visible token is mid-sentence with no `[truncated:
   model output length limit reached]` marker. Whether the two are
   causally linked (does entering the poisoned/partial-commit path
   somehow shorten remaining budget or change stop handling?) is worth a
   dedicated look — three sightings in one loop is no longer noise.
2. **No visibility into WHY a turn stopped mid-sentence.** The stderr
   receipt names `2 tool call(s) were denied by local policy` as the
   error but gives no signal distinguishing "policy-poisoned but
   otherwise complete" (cycles 14/16) from "policy-poisoned AND ran out
   of rounds/tokens mid-edit" (this cycle) — the supervisor had to infer
   the latter from the trailing reasoning text ending mid-thought with no
   truncation marker. A completion-status field on the receipt (e.g.
   distinguishing `policy-denied-but-complete` from
   `policy-denied-and-incomplete`) would let a supervisor triage without
   reading the full transcript.

**This completes Kimi's original P1–P11 backlog: P1 ✅ P2 ✅ P3 ✅ P4 ✅
P5 ✅ P6 ✅ P7 ✅ P8 ✅ P9 ✅ P10 (GLM's item, out of scope) P11 ✅.** Kimi
is 10-for-11 on code work since the ramp (cycles 7–17), with cycle 17
being the first incomplete delivery. Next: either a fresh item from
GLM/DeepSeek-adjacent backlogs, or (per this cycle's own finding) a
focused look at the R4.12 self-recovered-policy-denial pattern now that
it has reproduced three times.

### Cycle 18 — 2026-08-15 — FIRST CROSS-MODEL implementation (grade A)

Kimi implemented DeepSeek's cycle-7 review finding 1
(`reviews/deepseek-cycle-7.md` on the DeepSeek worktree): in
`summarize_or_fallback`, the `SummaryTextCollection::Complete { usage,
.. }` arm that catches an EMPTY/whitespace-only completion (sibling of
the non-empty arm guarded by `!text.trim().is_empty()`) was labeling its
fallback `SummaryFallbackReason::ProviderFailure` — conflating "provider
errored" with "provider succeeded but returned nothing." Kimi's fix,
exactly as scoped: a new `SummaryFallbackReason::EmptyCompletion`
variant (enum ~line 546, doc comment "The provider completed
successfully but returned no usable summary text"; also tightened
`ProviderFailure`'s own doc to "transport or stream error" so the two
can't drift back together), used in the empty-`Complete` arm (~line
3022) instead of `ProviderFailure`. Grepped the workspace for exhaustive
matches and found exactly the one predicted: `compaction_fallback_name`
in `crates/kitsu/src/main.rs` (~line 2387), a JSONL string map — added
`EmptyCompletion => "empty_completion"`. Purely additive: no function
signature changed, `Failed`/`IncompleteResponse` arms untouched.
Committed `7efc9a0` (Kate verified), pushed.

- **Diff: +130/-2 across 3 files** (`orchestrator.rs` +6/-2,
  `orchestrator_tests.rs` +125, `main.rs` +1). Genuinely small —
  the ripple was exactly the one predicted site, nothing more.
- **2 genuine tests**, both exercising `summarize_or_fallback` end-to-end
  (not just the enum): `summarize_or_fallback_empty_completion_uses_empty_completion_reason`
  scripts a `MockProvider` returning whitespace-only text + `Done(Stop)`
  and asserts `fallback_reason == Some(EmptyCompletion)` (would fail
  against the old code, which always produced `ProviderFailure` here) —
  plus asserts the trim fallback still ran and left `TRIM_MARKER`.
  `summarize_or_fallback_provider_failure_still_uses_provider_failure_reason`
  scripts a `FallibleMockProvider` erroring with `ProviderError::Connection`
  and asserts the reason is still `Some(ProviderFailure)`, proving the
  new variant didn't swallow real failures. Both pass in isolation
  (`cargo test -p kitsu-core summarize_or_fallback`: 2 passed).
- **Tool-call shape:** 12 Reads + 4 Edits + 22 `fs_scan_file` calls (38
  total) — more than the "~5 reads" the prompt asked for (Kimi grepped
  extensively for the exhaustive-match site and re-scanned its own edits
  for debris before finishing) but nowhere near cycle 17's 59-call
  overrun; the turn finished at round budget with headroom, completion
  8,142/24,000 tokens (34%), no truncation. **Stayed small in outcome**
  (diff size, correctness) even though the process was chattier than
  ideal.
- **Zero spawn attempts** — `--agent-spawn-cap 1` never exercised.
- **One false-positive debris warning** (R1.8, third sighting: cycles
  16, and now 18) — the trailing `Edit(orchestrator_tests.rs)` fired
  "content contains a raw model chat-template control marker" because
  the file already contains literal `<|im_start|>` as test DATA (a
  pre-existing `sanitize_warning` test at line ~1641), unrelated to
  Kimi's actual edit location. Content verified clean on disk; not a
  real corruption. R1.8 keeps reproducing whenever a model edits near
  literal template-marker strings used as test fixtures.
- **Verification:** `cargo build --workspace` clean; `cargo clippy
  --workspace --all-targets` warning-free (no new non-exhaustive-match
  lint — Kimi's own match update covered it); `cargo fmt --all --check`
  clean (no reformat needed); `cargo test --workspace` — 122 passed / 1
  failed in the `kitsu` crate's `cli.rs`
  (`test_query_only_interactive_session_does_not_create_a_snapshot`, a
  PTY "did not exit" panic). **Confirmed pre-existing and unrelated:**
  stashed the diff and reran the same test standalone against
  unmodified `ac77340` — it fails identically. Environment/PTY flake,
  not caused by this cycle.

**Backlog: P1–P11 ✅ (original backlog complete since cycle 17). Kimi is
now 11-for-12 on code work (cycles 7–18) and has landed its first
CROSS-MODEL contribution — DeepSeek reviewed, Kimi implemented, both on
their own worktrees.** No new kitsu weaknesses this cycle beyond the
R1.8 debris false-positive reconfirmation. Next: another cross-model
finding (DeepSeek proposals 2/3 in the same review — the silent
zero-`removed` no-op path, and the circuit-breaker-vs-RequestTooLarge
diagnostic precedence — or a GLM-adjacent finding), keeping scope to a
single function/match-ripple per cycle.

### Cycle 19 — 2026-08-15 — cross-model #2: DeepSeek cycle-7 proposal 2, `SummaryFallbackNoOp` (grade B+ — correct fix, but the described bug turned out to be currently unreachable; test premise had to be repaired)

Kimi's 2nd cross-model implementation: DeepSeek reviewed
`summarize_or_fallback`'s fallback tail in cycle-7, Kimi implements. Task:
the tail block `if removed > 0 { ...; progress.outcome =
Some(SummaryFallbackTrim); ... }` wrote NOTHING to `progress` when
`removed == 0`, so a caller could see a STALE outcome from a prior
compaction round. Single attempt, exit 0, clean tool-call shape: 2
`find_symbol` + 7 `Read` + 5 `Edit` = **14 tool calls**, well inside the
"~5 reads" budget and far under cycle 17/18's counts. Kimi read the
`CompactionOutcome` enum first, chose **option (b)** (additive
`CompactionOutcome::SummaryFallbackNoOp`, not an else-branch reusing an
existing variant), and correctly found and updated BOTH exhaustive-match
ripple sites without being told the exact line numbers:
`CompactionLedger::push`'s match on `record.outcome` in orchestrator.rs
itself (`ToolResultsElided | SummaryFallbackNoOp => {}`, i.e. a no-op
fallback counts as neither a summary nor a trim) and
`compaction_outcome_name` in `crates/kitsu/src/main.rs` (new arm
`"summary_fallback_no_op"`). Diff: orchestrator.rs +9/-1, main.rs +1,
orchestrator_tests.rs +115 (pre-repair). Zero spawn attempts
(`--agent-spawn-cap 1` never exercised). `kitsu-usage`:
119,811/3,913/123,724 (completion 16% of the 24,000 cap, zero
truncation). One more R1.8 false-positive debris warning (5th sighting:
cycles 16, 18, 19) on the `orchestrator_tests.rs` edit — file content
verified clean.

**The fix itself is correct and genuinely additive** — no signature
changes, no call-site refactors, both match sites covered, `cargo build
--workspace` / `clippy --workspace --all-targets` / `cargo fmt --all --
--check` all clean with zero supervisor changes needed to the production
code.

**But `cargo test --workspace` caught a real problem in Kimi's own test**,
`summarize_or_fallback_zero_removed_records_noop_not_stale_outcome`: it
seeded a stale prior outcome, called `summarize_or_fallback` with a
4-exchange conversation and a "generous" `target = 64_000`, and asserted
`SummaryFallbackNoOp` — but got `SummaryFallbackTrim` with 2 exchanges
actually removed. Root-caused rather than patched blindly: the tail's
`drop_old_exchanges_detailed(messages, current, target, message_tokens,
true)` call hardcodes `force_all: true`, whose while-loop condition
`(force_all || total > target) && e < max_drop` makes `target`
**irrelevant** whenever `force_all` is true — it unconditionally drops
every droppable exchange up to `max_drop`. And `max_drop =
actual - KEEP_RECENT_EXCHANGES`, gated by the SAME "too few exchanges"
threshold (`actual <= KEEP_RECENT_EXCHANGES`, `KEEP_RECENT_EXCHANGES =
2`) that `compaction_span()` already uses for its own early return
(`let Some(span) = compaction_span(messages) else { return Ok(()); }`)
at the top of `summarize_or_fallback`. No message mutation happens
between that early-return check and the tail call. **Consequence: by the
time the tail's `force_all: true` call runs, at least one exchange is
*always* still droppable and WILL be dropped — `removed == 0` is
currently unreachable from `summarize_or_fallback`'s own call site.**
DeepSeek's finding (and this cycle's brief) didn't verify reachability
before describing it as a live bug; it isn't one today, though the fix
remains correct, harmless, additive defensive code that immediately
stops a real staleness bug the moment `force_all`'s hardcoding or these
two thresholds ever drift apart independently.

**Supervisor repair:** replaced the failing test (not a retry — this
was a test-logic problem, not a transport failure, so R4.12/W5 doesn't
apply and no rerun was warranted) with two tests that pin down the
actual, verifiable facts instead of an unreachable end-to-end path:
`drop_old_exchanges_detailed_force_all_zero_only_when_nothing_droppable`
(direct test of the helper: returns 0 for a too-small conversation even
with `target = 0`; returns `> 0` for a real droppable span even with
`target = total * 100`, proving `force_all` ignores `target`) and
`compaction_ledger_does_not_count_summary_fallback_no_op_as_a_trim`
(direct `CompactionLedger::push` test: a `SummaryFallbackNoOp` record
increments `total_compactions` but neither `trims` nor `summaries`,
proving the ledger match-arm update is correct, not merely
compiling). Kimi's own `summarize_or_fallback_removed_path_still_records_summary_fallback_trim`
test (the removed > 0 regression guard) passed unmodified and was kept.
**Final test count: 5 new/kept tests**, all passing, all grounded in the
actual diff (2 supervisor-authored + 1 Kimi-authored kept + the 2
pre-existing cycle-18 EmptyCompletion/ProviderFailure tests still
green).

**Verification (final, all clean):** `cargo build --workspace`; `cargo
clippy --workspace --all-targets` zero warnings; `cargo fmt --all --
--check` clean (no reformat needed pre- or post-repair); `cargo test
--workspace --no-fail-fast`: kitsu-core 126/126 (was 125 pre-repair with
1 failing), kitsu-providers/kitsu-render/kitsu-tools all green, kitsu
122/123 — only the pre-known PTY flake
`test_query_only_interactive_session_does_not_create_a_snapshot`
(cycles 10-18), confirmed unrelated. Diff: orchestrator.rs +11/-1,
main.rs +1, orchestrator_tests.rs +149 across the 3 files.

**Grade B+.** Docked from A: the code fix and match-ripple discovery
were both flawless and delivered in a genuinely small tool-call budget,
but the cross-model review this cycle implemented described a bug whose
reachability was never verified — a gap in DeepSeek's original review
this cycle exposed rather than repeated, and worth feeding back to
future DeepSeek cycles ("verify a described branch is actually
reachable through the real call graph before filing it, not just
plausible from reading the code"). Kimi is now 12-for-13 on code work
(cycles 7-19) with 2-for-2 on cross-model implementations, both requiring
supervisor repair (cycle 18: main-branch collision on an unrelated file;
cycle 19: an unreachable-path test). No new kitsu weaknesses this cycle
beyond the R1.8 debris false-positive's 5th reconfirmation. Next: a
DIFFERENT cross-model finding — DeepSeek cycle-7 proposal 3
(circuit-breaker-vs-RequestTooLarge diagnostic precedence) or a
GLM-adjacent finding — and when scoping it, the supervisor should
pre-verify reachability of the described branch before handing Kimi the
brief, to avoid repeating this cycle's test-repair overhead.

### Cycle 20 (2026-08-15) — cross-model #3: GLM cycle-8 finding 1, `AuthError::Malformed` for non-UTF-8 env vars (grade A — clean single-attempt, first Kimi work in kitsu-providers)

Kimi's 3rd cross-model implementation, and its **first in
`kitsu-providers`** (every prior cross-model cycle — 18, 19 — was in
`kitsu-core`): GLM reviewed `EnvBearerAuth::header()`
(`crates/kitsu-providers/src/auth.rs`) in its cycle-8 review and found
that the `_` catch-all arm of `match std::env::var(&self.env)` collapsed
`Err(VarError::NotPresent)`, `Err(VarError::NotUnicode(_))`, and
`Ok("")` into the same `AuthError::Missing` ("is not set"), which is
false for the `NotUnicode` case — the variable IS set, just holding
non-UTF-8 bytes, so the operator is told to "set" something that
already exists, hiding the real (encoding) fix. Fix (verified minimal
by the supervisor before handing off): route `NotUnicode` to the
already-existing `AuthError::Malformed { env }` variant (Display: "auth
error: token in ${env} is malformed" — accurate, no new variant, no
`main.rs` exhaustive-match change needed since `Malformed` was already
handled there).

**Run stats.** Single attempt, exit 0, no retry, no spawn attempted
(`--agent-spawn-cap 1` never exercised). Tool-call shape: 3 `Read`
(`auth.rs` ranged 55-96, `error.rs` ranged 62-72, `auth_tests.rs`
whole) + 2 `Edit` (`auth.rs`, `auth_tests.rs`) = **5 tool calls total**
— the smallest cross-model cycle so far (18: 38, 19: 14, 20: 5), and
well inside the "~5 reads" budget the brief allowed. `kitsu-usage`:
26,220 / 1,598 / 27,818 (completion 7% of the 24,000 cap, zero
truncation). `kitsu-context`: used 8,160 / effective 107,072 / window
131,072. No debris-warning false positive this cycle (R1.8 did not
reproduce — the edit sites don't touch any template-marker test
fixture).

**Harness note (process gap, not a Kimi or kitsu-code weakness).** The
literal EXECUTION brief specified `kitsu ask ... --allow-path ...
--allow-write ...`. Both flags no longer exist on `ask` in the current
build, and — more importantly — `kitsu ask` registers **no filesystem
tools at all**: `run_ask` builds an empty `Registry` and never calls
`register_base_tools` (that call exists only in `run_chat`); `ask` is a
deliberately tool-free, single-turn path (`kitsu-tools/src/lib.rs:82-83`,
"the descriptor-free `ask` path"). A first attempt with `kitsu ask
--yes "<prompt>"` confirmed this empirically: it returned in one round
with a trivial reply, `kitsu-usage: 395/197/592`, zero tool-call
receipts, and zero file changes — the model was never offered a way to
read or write anything. `--allow-path`/`--allow-write`/
`--allow-read-write` are `kitsu chat`-only flags; `chat` also supports a
single piped/non-TTY turn (`[PROMPT]` positional + `< /dev/null`
stdin), so the corrected invocation was `kitsu chat --gateway
huggingface --model moonshotai/Kimi-K3:together --context-window 131072
--timeout-secs 300 --max-output-tokens 24000 --reasoning-effort low
--max-rounds 30 --agent-spawn-cap 1 --allow-path /tmp/kimi-k3-cycle1
--allow-read-write /tmp/kimi-k3-cycle1/crates/kitsu-providers --yes
"<prompt>" < /dev/null`, which ran cleanly on the first try. Filed here
for the record so future cycles' EXECUTION templates say `chat`, not
`ask`, when the task requires file reads/writes — not filed as a new
kitsu backlog item since `ask` being tool-free is by design, not a bug.

**Deliverable: exactly the prescribed minimal fix, verified on disk.**
`git diff` matches the brief precisely:

```rust
let token = match std::env::var(&self.env) {
    Ok(v) if !v.is_empty() => v,
    Err(std::env::VarError::NotUnicode(_)) => {
        return Err(AuthError::Malformed { env: self.env.clone() })
    }
    _ => { /* Missing, unchanged */ }
};
```

No new enum variant, no signature change, no refactor — `NotPresent`
and empty-string both still fall through the `_` arm to `Missing`
unchanged. Diff: `auth.rs` +5/-0, `auth_tests.rs` +44/-0.

**2 genuine new tests**, both `#[serial]` and mirroring the file's
existing `EnvGuard`/serialization style:
`test_header_non_utf8_env_is_malformed_not_missing` (`#[cfg(unix)]`,
sets the test env var to `[0xFF, 0xFE]` via `OsStr::from_bytes`,
restores it manually since `EnvGuard` is `String`-based and can't hold
non-UTF-8, then asserts both `matches!(err, Malformed { .. })` and
`!matches!(err, Missing { .. })`) and
`test_header_absent_env_still_missing` (confirms the untouched path
still reports `Missing`). Full `kitsu-providers` auth suite: 15/15
passing, including both new tests.

**Independently confirmed fail-without-fix (supervisor-verified, not
taken on Kimi's word).** `git stash push -- auth.rs` (keeping the new
tests), reran `cargo test -p kitsu-providers
test_header_non_utf8_env_is_malformed_not_missing`: **failed** exactly
as expected —
`non-UTF-8 token must map to AuthError::Malformed, got Missing { env:
"KITSU_TEST_AUTH_TOKEN", hint: Some("run \`refresh-token\` to refresh
the gateway JWT") }`. Restored the fix (`git stash pop`) and reran —
green again. The new test is genuine, not a tautology.

**Verification (all clean).** `cargo build --workspace`: clean. `cargo
clippy --workspace --all-targets`: zero warnings (the `Ok(v) if
!v.is_empty()` guard leaves the match non-exhaustive without a trailing
`_`, which the code already had and Kimi didn't touch — no new
non-exhaustive-match lint). `cargo fmt --all -- --check`: clean, no
reformat needed. `cargo test --workspace`: 122 passed / 1 failed — only
the pre-known PTY flake `test_query_only_interactive_session_does_not_
create_a_snapshot` (cycles 10-19, environment/PTY "did not exit" panic,
unrelated to this diff); `cargo test -p kitsu-providers`: 66/66 passed
including the 2 new tests, no env-test race observed (the new
non-UTF-8 test is `#[serial]` and restores the prior env value/absence
exactly like `EnvGuard`, so it doesn't interfere with neighboring
`#[serial]` tests).

**Grade A.** Correct (NotUnicode → Malformed, NotPresent/empty →
Missing, verified by spot-check and by the independent
fails-without-the-fix rerun), single-function + zero-ripple (no new
variant, no signature/refactor, `main.rs:1444`'s existing `Malformed`
arm needed no change), genuinely small (5 tool calls — the smallest
cross-model cycle yet), and a genuine, non-tautological test. Kimi is
now 13-for-14 on code work (cycles 7-20) and 3-for-3 on cross-model
implementations (18: DeepSeek→Kimi in kitsu-core, needed a
main-branch-collision repair; 19: DeepSeek→Kimi in kitsu-core, needed a
reachability-driven test repair; 20: GLM→Kimi, **first clean cross-model
cycle needing zero supervisor repair**). No new kitsu weaknesses this
cycle beyond the process-only harness note above. Next: continue
cross-model work — DeepSeek cycle-7 proposal 3 (circuit-breaker-vs-
RequestTooLarge diagnostic precedence, still open) or GLM's remaining
cycle-8 findings, whichever the supervisor can verify as reachable and
minimal before handing off.
