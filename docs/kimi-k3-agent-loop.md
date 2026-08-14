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
- [ ] **New, confirmed 2026-08-13 (cycle 1):** ranged reads in the
  `fs_read_file` tool cap at 64 KiB measured from the *absolute start of
  the file*, not from the requested range's start line. A ranged read
  whose `start_line` falls past the 64 KiB absolute mark returns almost
  nothing regardless of the requested range's width. Root cause:
  `crates/kitsu-tools/src/builtins/fs.rs:333-363` (`read_capped_text`)
  applies the byte cap before `crates/kitsu-tools/src/builtins/fs.rs:445-468`
  slices out the requested line range; the cap needs to be relative to
  the range, not the file start. Verified: `orchestrator.rs` line 1533
  begins at byte 65,678 (just past 65,536 = 64 KiB), matching exactly
  where every "full" and ranged read stalled. See
  `reviews/kimi-cycle-1.md` for the full trace.
- [ ] The `served {X} KiB of {Y} KiB total` disclosure in
  `crates/kitsu-tools/src/builtins/fs.rs:474-480` measures `served_bytes`
  *before* the line-range slice is applied, so it reports the pre-slice
  absolute read, not what the caller actually received — misleading on
  its own even before the cap-placement bug above.
- [ ] Add an `orchestrator_tests.rs` case for a ranged read whose
  `start_line` sits past the file's first 64 KiB, so the bug above (and
  any fix) is regression-tested; the existing ranged-read tests
  (`crates/kitsu-core/src/orchestrator_tests.rs:1514-1532`) only use
  small files and never exercise this path.
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
