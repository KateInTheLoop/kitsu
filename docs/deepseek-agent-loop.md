# DeepSeek-V4 worker loop — kitsu drives a second hosted frontier model

> **LIVE 2026-08-14 — cycle 1 banked (grade B).** A working HF token
> (Inference-Providers scope) is at `~/.cache/kitsu-hf-token`; each tick
> reads it there and probes once — on 401/402/403 or missing file the
> tick ends silently. DeepSeek-V4-Pro reaches the `:together` provider,
> tool calls come back intact, and `--reasoning-effort low` is
> load-bearing (see cycle 1). See the Cycle log for the standing
> weakness backlog this loop has produced.

Started 2026-08-14 at user direction: three-contributor collaboration —
Claude (main session, orchestrator + reviewer), Kimi-K3
(`docs/kimi-k3-agent-loop.md`), and DeepSeek-V4 (this loop). Model:
**`deepseek-ai/DeepSeek-V4-Pro`** via the HF router (newest DeepSeek
line as of 2026-08-14; dated snapshots `V4-Pro-0813` / `V4-Flash-0731`
also exist — prefer the unpinned Pro alias unless a snapshot proves
more stable). Providers observed: novita, together, fireworks-ai,
baseten, deepinfra.

## Inherited frontier recipe (Kimi lessons applied from day one)

These are the rules Kimi took 7 cycles to learn. DeepSeek starts with
all of them:

- **Pin a provider**: `--model deepseek-ai/DeepSeek-V4-Pro:together`
  (together was the provider whose tool-call parsing fixed Kimi's
  trailing-call corruption; re-evaluate in cycle 1 if it misbehaves).
- **Cap hidden reasoning**: pass `--reasoning-effort low` for
  exploration/review tasks; raise only for synthesis. Budget
  `--max-output-tokens 24000`, `--timeout-secs 300`.
- **Scope reads tightly**: name the exact files in the prompt; no
  open-ended exploration; NO sub-agent spawns; **write the deliverable
  file early, polish after** (trailing tool calls after long turns are
  the highest-corruption moment).
- **Ramp, don't sprint**: cycle 1 banks a small one-file success before
  any open-ended work is permitted.

## Cycle 1 (first live tick): probe + bank a success

Phase A — supervisor runs 3 direct router probes (curl, ~1K tokens
total) and records in the cycle log: (1) does V4-Pro return
`reasoning_content` and does `reasoning_effort` shrink it; (2) does a
forced tool call come back structurally intact from `:together`; (3)
usage shape (`prompt_tokens_details.cached_tokens` present?).

Phase B — kitsu run, ONE file only: review
`crates/kitsu-core/src/context.rs`, deliverable
`reviews/deepseek-cycle-1.md` (ranked, line-cited proposals). This is
the same file Kimi's cycle 4 banked (grade A) — giving us a direct
same-task quality comparison between the two contributors.

Later cycles: alternate review and implementation tasks from the shared
backlog (see the Kimi doc's P1–P11); never assign the same item to both
contributors in the same hour.

## Rules

- **Branch**: all work on `dogfood/deepseek-v4`, in a worktree at
  `/home/mmpg/.cache/deepseek-worktree` (MAIN DISK — never under /tmp;
  the tmpfs is 5.7G and a kitsu target dir needs ~12G). Any cargo
  command uses `CARGO_TARGET_DIR=/home/mmpg/.cache/deepseek-worktree-target`.
- **Gateway**: `--gateway huggingface --model
  deepseek-ai/DeepSeek-V4-Pro:together --context-window 131072
  --timeout-secs 300 --max-output-tokens 24000 --reasoning-effort low`.
- **Token**: the supervisor receives `HF_TOKEN` via environment at
  spawn time. NEVER write the token into any file, commit, or log.
- **Read scope**: `--allow-path <worktree>` only; write access only to
  the worktree's `reviews/` directory (widen to a named crate only for
  an implementation cycle, as Kimi cycle 7 did).
- **Cost bounds**: one kitsu run per cycle, one retry on transport
  failure; scoped prompts keep a cycle in the low tens of thousands of
  tokens.
- **Supervisor**: ONE Sonnet agent per cycle. It (a) runs kitsu itself,
  (b) captures stderr receipts as evidence, (c) grades the deliverable,
  (d) appends the cycle log entry on the work branch with observed
  kitsu weaknesses, (e) files follow-ups. Supervisors never spawn
  helper agents and never pause to await notifications — every wait is
  a synchronous `sleep` in a Bash call.
- **Orchestration**: the main Claude session is scheduler and reviewer
  for all three contributor lanes; the loops stay disjoint (UI parity
  on the main checkout, Kimi and DeepSeek each on their own branch and
  worktree).

## Cycle log

(appended by the supervisor each cycle, newest last; per-cycle detail
also lives in this doc's copy on `dogfood/deepseek-v4`)

### Cycle 1 — 2026-08-14 — DeepSeek debut (grade B)

Branch `dogfood/deepseek-v4` off clean `origin/main` (ff329eb); same
`context.rs` review task Kimi banked at A in its cycle 4, for a direct
cross-model comparison. Commit `8cac2fa`, Kate identity verified, pushed.

**Phase A — router format probes (`DeepSeek-V4-Pro:together`):**
- Hidden reasoning is returned under the key **`reasoning`** (the spec
  guessed `reasoning_content`; kitsu should tolerate both). `--reasoning-effort
  low` shrinks it measurably (24 vs 49 reasoning tokens) and is
  **load-bearing, not just cost**: without it a tight `max_tokens` was
  fully consumed by reasoning → empty `content`, `finish_reason:length`.
- Forced tool call returns structurally intact from `:together` (valid
  id/type/name, well-formed JSON args) — one clean sample.
- `prompt_tokens_details.cached_tokens` present (0 on one-off calls);
  `completion_tokens_details.reasoning_tokens` also present.

**Phase B — review:** 8 concrete, non-hallucinated proposals (spotted
the doc-comment "circuit breaker" that isn't implemented, binary
`had_schema` losing granularity, a stale buffer cap). kitsu-usage
31,447 / 4,817 / 36,264 — under the 40K target, cheaper than Kimi
cycle 4. **Docked to B:** citation accuracy degrades past the first
third (buffer-cap cited at "line 203", actually 166; `ContextBudget::new`
at "186–205" vs real 159–173), and the model's own unprompted
"verification" pass rubber-stamped the drift instead of catching it.

**Comparison to Kimi cycle 4 (A):** Kimi's citations were letter-perfect
with per-proposal risk analysis; DeepSeek shipped more proposals but
weaker grounding and no risk sections. **Kimi cycle 4 remains the higher
bar.** DeepSeek's ramp lesson mirrors Kimi's: it needs the write-first,
cite-carefully discipline; a citation-verification step in the brief may
help next cycle.

**kitsu weaknesses filed (backlog for the main capability loop):**
- W1: top-level `kitsu --allow-path/--allow-write/--agent-spawn-cap`
  fail without hinting the flag lives under the `chat` subcommand — a
  clear-error-with-suggestion opportunity (cost the supervisor one retry).
- W2: a response with empty `content` + `finish_reason:length` +
  `reasoning_tokens` near the cap is a silent-truncation footgun — kitsu
  should emit a warning ("output budget consumed entirely by hidden
  reasoning; raise --max-output-tokens or lower --reasoning-effort").

### Cycle 2 — 2026-08-14 — FAILED to ship (no deliverable)

Fresh review target (orchestrator trim/summarize path) with a mandatory
re-read-before-cite discipline added to fix cycle-1's citation drift.
**Both kitsu attempts died before writing `reviews/deepseek-cycle-2.md`,
so the citation-discipline hypothesis is still untested.** Commit
`7ae4723` records the failure log only.

- Attempt 1: DeepSeek tried to spawn 6 parallel read subagents against
  `--agent-spawn-cap 0`, was denied, retried spawning, kitsu withheld
  spawn tools for the rest of the turn — then the very next provider call
  returned a terminal `HTTP 400` and killed the turn.
- Retry: no spawns, but it ignored "named ranges only / write early" —
  35 reads, near-full-file passes over the 2957-line file, never called
  write, hit the 300s timeout, turn correctly discarded (no usage
  billed). Per the one-retry rule, no third attempt.
- Grade: incomplete/F on this cycle's own terms (failure to ship, not a
  bad review). DeepSeek's over-reading + spawn-happiness are the ramp
  gaps to fix next.

**kitsu weaknesses filed (backlog):**
- W3 (**real bug, promoted to roadmap R4.8**): after kitsu withholds
  spawn tools mid-turn following repeated `--agent-spawn-cap 0` refusals,
  the next provider request in the same turn is rejected `HTTP 400` —
  a request-shape break from the mid-turn tool-list mutation. `cap 0`
  should mean spawn tools are never advertised, not advertised-then-
  withheld. This affects the Kimi loop too (same flag).
- W4: no mid-turn nudge when a model burns many rounds without ever
  calling write, despite an explicit "write early" instruction.
- W5: timeout-discard is correct but lets an over-reading model burn the
  whole cycle with zero salvageable output.

**Ramp status:** cycle 1 banked (B), cycle 2 failed to ship. Next cycle:
keep the review-lane task but (a) explicitly forbid spawns in prose AND
keep cap 0 only once R4.8 is fixed — until then, drop to
`--agent-spawn-cap 1` for DeepSeek so a stray spawn attempt degrades
instead of crashing; (b) hard-cap reads ("read at most these N ranges,
then you MUST write") to curb over-reading.

### Cycle 2 RETRY — 2026-08-14 — FAILED again (no deliverable)

Ran with the mitigations: `--agent-spawn-cap 1`, explicit no-spawn prose,
a strict prose read-cap, and re-read-before-cite discipline. Commit
`b80ee21` records the failure. DeepSeek is now **1-for-5 on kitsu
invocations** (cycle 1 B; cycles 2 original ×2 + this retry ×2 all
shipless).

- **`--agent-spawn-cap 1` worked:** both attempts still called
  `spawn_agent` despite the prose ban, but cap-1 absorbed each cleanly —
  no HTTP 400, no crash. This confirms the W3 crash is specific to the
  cap-0 deny→retry→withhold sequence, not to spawning-while-capped.
- **The prose read-cap did NOT work:** attempt 1 made ~15 reads (incl. a
  broad 1-400 sweep nowhere near the target functions past line 1950) + 2
  out-of-scope context.rs reads; the retry was worse (~28 reads, dupes).
  Neither ever called `write`; both hit the 300s timeout and were
  discarded (nothing billed). **Prose caps are 0-for-3 against this model.**
- Citation discipline still untested — no deliverable has ever existed.
- Grade: incomplete/F.

**New weakness W6 (→ roadmap R4.10):** kitsu has a technical spawn cap
but no technical read/tool-call cap; a compulsive over-reader can only be
stopped by a `--max-tool-calls-per-turn`-style lever, not prose.

**Ground truth for future spot-checks** (worktree orchestrator.rs):
`request_measurement` 1954-2014, `ensure_chat_request_fits` 2015-2040,
`compact_context` 2041-2122, `enforce_context_budget_recording`
2123-2384, `summarize_or_fallback` 2545-2661.

**Next cycle (3): shrink the scope drastically** — assign ONE ~60-line
function (e.g. `request_measurement` alone, lines 1954-2014) so we can
tell whether DeepSeek's failure is scope-sizing or a deeper
exploration-discipline problem. If it fails on a single 60-line function
too, DeepSeek may not be viable as a contributor under the current kitsu
levers until R4.10 (technical read cap) lands.

### Cycle 3 — 2026-08-14 — SUCCESS on tiny scope (grade A-)

**The experiment answered the question: over-reading VANISHED at tiny
scope.** Scoped to one ~60-line unit (`request_measurement` 1954-1999 +
`check_measurement` 2001-2012), DeepSeek did **exactly 1 read (the single
1950-2012 ranged read) then 1 write, shipped first attempt** — a complete
reversal from cycles 2/retry's 15-28 reads and 4-for-4 shipless runs,
with materially the SAME prose read-cap wording. The only variable that
changed was scope size (plus a more mechanical "read exactly once"
phrasing). Commit `3568249` (Kate verified), pushed. kitsu-usage 12,985 /
3,536 / 16,521 — fast and cheap. cap-1 unexercised (no spawn attempted).

- **Deliverable grade A-.** 4 ranked proposals in reviews/deepseek-cycle-3.md.
  Citation spot-check: every direct quote LETTER-PERFECT against ground
  truth (a sharp reversal from cycle 1's drift). Docked half a grade
  because the top-ranked proposal was substantively wrong (it hypothesized
  a bug in `check_measurement`'s budget enforcement that doesn't exist —
  `target = effective − buffer` is always the stricter bound) — but it was
  honestly HEDGED as conditional ("If X, then Y") about an out-of-scope
  dependency it correctly did not read. Proposals 2-4 correct.

**Verdict: scope-sizing is (at least) the dominant lever — DeepSeek IS
viable when given a tiny, single-function scope.** n=1, so not conclusive;
next cycle should replicate at the same tiny scope on a DIFFERENT function
before widening. R4.10 (technical read cap) still worth building — it
would let DeepSeek self-recover on larger scopes instead of needing the
scope hand-shrunk each time.

**Ramp status:** DeepSeek 2-for-6 (cycle 1 B, cycle 3 A-), but 2-for-2
once the scope is single-function-tight. The path to contribution is
narrow-scope tasks. Cycle 4: replicate on another ~60-line function
(e.g. `ensure_chat_request_fits`) to confirm the pattern, then consider a
tiny single-function IMPLEMENTATION task.

### Cycle 4 — 2026-08-14 — REPLICATED, n=2 confirmed (grade A-)

Same tiny-scope discipline on a different function (`ensure_chat_request_fits`
— the supervisor caught that the assigned 2015-2077 range actually
spanned into an unrelated `compact_context`; the real function is only
2015-2034, so it corrected the scope to 2013-2034 to keep the
single-function experiment honest). **Pattern replicated exactly: 1
source read, wrote first attempt, zero over-reads, no spawn attempt.**
kitsu-usage 27,081 / 4,684 / 31,765. Commit `fcfbbeb` (Kate verified),
pushed.

- Grade A-. 4 ranked proposals, **none substantively wrong** (an
  improvement over cycle 3's one bad top proposal). Citation quoted TEXT
  letter-perfect on every citation — but the line-number LABELS drifted
  ~1 line throughout (cited 2013 vs actual 2014, 2019 vs 2020, etc.).
- **New cross-model finding (candidate roadmap R1.7):** the ~1-line drift
  is caused by `fs_read_file`'s ranged read (`read_capped_line_range`)
  returning raw joined text with NO line-number prefixes — the model must
  self-count from `start_line` and undercounted by one (the range's first
  line was blank). This is why RANGED-read reviewers (DeepSeek) drift
  while WHOLE-file reviewers (GLM, counting from line 1) don't. Prefixing
  ranged reads with absolute line numbers (cat -n style) would likely
  eliminate this drift class for every model — but carries an
  edit-matching tradeoff (models may copy the prefix into `old_text`), so
  it needs care.

**Verdict CONFIRMED (n=2): tiny single-function scope is DeepSeek's
reliable viability lever — 3-for-3 there, 3-for-7 overall.** Cycle 5 =
DeepSeek's FIRST IMPLEMENTATION cycle: a tiny, self-contained
single-function change with `--allow-write` on the crate. The journal-cap
bug that blocked orchestrator.rs edits is FIXED on main (edit/write now
use the 1 MiB write cap), so a fresh binary can edit it — but keep
DeepSeek's first code target small and self-contained regardless.

### Cycle 5 — 2026-08-14 — first code cycle, PARTIAL (grade C+)

Implemented its own cycle-4 proposal #1: `ensure_chat_request_fits`'s
`serde_json::to_string(tools).unwrap_or_default()` silently turns a
serialization failure into an empty `schema_json` (the request undercounts
while the real schema still ships). **The code fix is correct and
minimal** (propagates via `AgentError::Loop` — DeepSeek's cycle-4
proposal had invented a non-existent `AgentError::internal`, corrected in
the brief). Committed `162a969` (Kate verified), pushed. THIRD production
validation of the journal-cap fix: DeepSeek edited orchestrator.rs
(129 KiB) directly, no journal-limit error.

- **Grade C+, and the reason is the important finding: implementation
  breaks DeepSeek's scope discipline.** Its tiny-scope wins (cycles 3-4)
  were REVIEWS — no type-chasing. Implementation forced it to chase
  `ChatRequest`/`ToolDescriptor`/`AgentError` definitions across 4 files:
  **~36 tool calls (16 Read, 18 fs_scan_file, 2 List) vs the "at most 2
  reads" cap**, then the 300s timeout. The over-reading returns the moment
  it can't stay inside one already-understood function.
- The mandated test never shipped — and the supervisor verified the test
  strategy is **unconstructible**: `serde_json` can't fail to serialize a
  `ToolDescriptor` today (params is a Value that can't hold non-finite
  floats), so no legitimate input triggers the guarded path. No hollow
  test fabricated (good rigor); the fix is defensive against a
  currently-impossible failure.
- Also over-reached the diff (bumped fn to `pub(crate)`, added an unused
  import) — reverted by the supervisor to keep scope tight.

**New weaknesses:** W7 — no technical read cap hurts IMPLEMENTATION
specifically (reinforces roadmap R4.10); W8 (→ roadmap R4.11) — a turn
DISCARDED on timeout still leaves its file edits on disk ("discarded"
implies rollback but files changed) — kitsu should surface a "files were
modified" notice on a discarded turn.

**Ramp verdict:** DeepSeek is a reliable TINY-SCOPE REVIEWER (3-for-3 at
A-/B on single-function reviews) but NOT yet a reliable implementer —
implementation's inherent type-chasing reignites its over-reading, which
only a technical read cap (R4.10) will contain. **Role decision: keep
DeepSeek on tiny-scope REVIEWS (its proven contribution mode) until R4.10
lands; revisit implementation after.** Cycle 6 = a tiny-scope review of a
fresh single function.

### Cycle 6 — 2026-08-14 — tiny-scope review, process clean but accuracy slips (B+)

Reviewed `compact_context` (2041-2077). **Process perfect for the 3rd
clean review running: exactly 1 ranged read → 1 write, no over-read, no
spawn.** Cheapest cycle yet (kitsu-usage 14,204 / 2,338 / 16,542). Commit
`0121b43` (Kate verified), pushed. Citation QUOTES letter-perfect; labels
drift ~1 line (R1.7, judged on quotes).

- **Proposal 1 (high) is a genuine, valuable catch:** DeepSeek
  independently re-derived the same `unwrap_or_default()`-swallows-
  serialization-errors bug class it FIXED in cycle 5's sibling
  (`ensure_chat_request_fits`) — confirming it is a **recurring
  anti-pattern in orchestrator.rs** (candidate for a systematic sweep;
  see backlog note).
- **Accuracy slip (why B+, not A-):** Proposal 2 (medium) is
  substantively WRONG and, for the first time, UNHEDGED — it claims the
  `usage` local is discarded, but `compact_context`'s body is a bare
  tail-expression return of `enforce_context_budget_recording(...)`, so
  nothing is discarded. (Cycle 3's wrong proposal was at least hedged.)
  Proposal 3 mislabels an out-of-scope positional arg (`1` called a
  "retry count"; it's `round`) though the underlying code-smell holds.
- **DeepSeek's profile is now clear:** rock-solid PROCESS discipline on
  tiny scope, but CONTENT accuracy is B+/A- with occasional
  confidently-wrong claims about things outside the single function it
  read. **Cron fix: instruct DeepSeek to HEDGE any claim it cannot verify
  from the one assigned range** (it keeps reasoning about callers/callees
  it did not read). Ramp verdict unchanged: tiny-scope reviews until R4.10.

**Backlog note (recurring anti-pattern):** `unwrap_or_default()` silently
swallowing `serde_json::to_string` failures appears in multiple
orchestrator.rs functions (fixed in `ensure_chat_request_fits` cycle 5;
flagged again around `compact_context`). Worth a Kimi sweep to find and
guard all instances.
### Cycle 7 — 2026-08-14 — BEST CYCLE (grade A): R1.7 + hedging fixes both landed

Reviewed `summarize_or_fallback` (2545-2661). Process perfect for the 4th
clean cycle: 1 ranged read → 1 write, no over-read, no spawn, exit 0 (no
discard this time). kitsu-usage 17,152 / 4,922 / 22,074. Commit `05f4fa7`
(Kate verified), pushed.

- **R1.7 VALIDATED FOR DEEPSEEK — the loop closes.** All citations
  letter-perfect on BOTH quoted text AND line-number labels — the FIRST
  cycle with ZERO label drift. The absolute line-number prefixes shipped
  in R1.7 (from DeepSeek's OWN cycle-4 finding) eliminated the ~1-line
  self-count error that drifted every prior ranged-read cycle. A clean
  find→fix→validate arc across the loops.
- **Hedging fix landed too.** DeepSeek hedged ONLY genuinely out-of-range
  dependencies (a SummaryFallbackReason/CompactionOutcome variant, the
  value of MAX_COMPACT_FAILURES, caller behavior) and did NOT hedge
  in-range facts — **no unhedged wrong claims**, a clean reversal of
  cycle 6's failure mode. The cron hedging instruction worked.
- All 3 proposals substantively correct (verified): (1 high) the
  `Complete { usage, .. }` arm @2627 labels an empty completion
  `ProviderFailure`, conflating "errored" with "succeeded but empty";
  (2 med) zero-drop path leaves `progress.outcome` stale; (3 low)
  circuit-breaker records `CircuitBreaker` even when the request was also
  oversized.

**Ramp verdict UPGRADED:** DeepSeek is now a STRONG tiny-scope reviewer —
process rock-solid (4 clean cycles), citations perfect (R1.7), hedging
correct. Both diagnosed weaknesses are fixed. Grades: B, A-, A-, B+, A.
It remains review-only until R4.10 (implementation type-chasing unchanged),
but as a reviewer it now matches GLM's discipline. (Minor: `--prompt`
isn't a flag on `kitsu chat` — prompt is positional; same CLI-friction
family as R4.7.)

### Cycle 8 — 2026-08-15 — enforce_context_budget review (grade A, 5th clean)

Reviewed `enforce_context_budget` (turned out to be a small `#[cfg(test)]`
wrapper, 2074-2106 — the supervisor caught that the cron's 2078-2122 range
was wrong and spanned into an unrelated struct + the next fn's doc; a
lower-value target than intended, steer to PRODUCTION functions next).
Process perfect: 1 ranged read (33 lines) → 1 write, no over-read, no
spawn, exit 0. Cheapest cycle yet: kitsu-usage 15,034 / 2,295 / 17,329.
Commit `fabe302` (Kate verified), pushed.

- **Citations 4/4 letter-perfect on quotes AND labels — 2nd flawless
  R1.7 cycle running** (7, 8 both A). Hedging well-calibrated: hedged the
  callee-behavior proposals ("if the callee..."), and correctly did NOT
  hedge "the callee takes 12 args" because the 12 positional args are
  directly countable from the read range. No substantively-wrong or
  unhedged-out-of-scope claims.
- Minor cosmetic: the review self-dated "2025-07-15" (hallucinated;
  harmless, doesn't affect substance).

**DeepSeek is a consistent A tiny-scope reviewer now (7, 8 both A after
the R1.7 + hedging fixes). Steady, cheap, reliable.** Its findings feed
the other loops (Kimi is implementing DeepSeek's cycle-7 finding). Next:
a fresh PRODUCTION function (not a test wrapper).
