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
  `--context-window 131072`, generous `--max-output-tokens` (>= 4000;
  it is a reasoning model — small budgets return empty answers).
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

(appended by the supervisor each cycle, newest last)

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
