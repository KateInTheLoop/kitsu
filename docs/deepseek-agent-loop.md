# DeepSeek-V4 worker loop — kitsu drives a second hosted frontier model

> **LIVE 2026-08-15: cycle 8 — grade A, 2nd zero-drift citation cycle,
> and a scope-range bug in the cron assignment caught before dispatch.**
> The cron-supplied range for `enforce_context_budget` was wrong and was
> corrected to the function's real span (2074-2106) before handoff.
> Process held a 5th clean cycle (1 read, 1 write, no over-read, no
> spawn); citations were exact on quotes and labels for the 2nd cycle
> running. Cycle 5 still shows that tiny-scope review discipline does
> not automatically transfer to implementation work.

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

### Cycle 8 — 2026-08-15 — scope-range bug caught pre-dispatch; review still A

**Supervisor scope correction before dispatch:** the cron-assigned range
(`enforce_context_budget` @ 2078-2122) was wrong. Verified against the
worktree source: `enforce_context_budget` actually spans lines
**2074-2106** (doc comment `2074-2075`, `#[cfg(test)]` `2076`,
`#[allow(...)]` `2077`, signature `2078`, body through closing brace
`2106`) — a `#[cfg(test)]`-only test wrapper, not production logic. The
cron's 2108-2122 tail is not part of this function at all: 2108-2116 is
an unrelated struct `CompactionProgress`, and 2118-2122 are doc comments
belonging to the next function, `enforce_context_budget_recording`.

Process: 5th clean cycle running — exactly 1 ranged read (33 lines,
matches assignment exactly, zero over-read) → 1 write, no spawn
attempted, exit 0. kitsu-usage 15,034 / 2,295 / 17,329 — cheapest cycle
yet.

- **Citations letter-perfect again — quotes and labels, 2nd flawless
  cycle after cycle 7.** All 4 citations (lines 2103, 2102, 2078+2091,
  2077) verified byte-for-byte against the source.
- **Hedging correctly calibrated.** Proposals 1 and 2 properly hedge on
  unread callee behavior. Proposals 3 and 4 rely only on directly
  visible, in-range facts, including the 12 positional arguments at the
  call site.
- **Minor flaw:** the review file self-reports `2025-07-15`, a wrong
  date. It does not affect the proposals' substance.
- Content is necessarily thinner than cycles 6-7 because the corrected
  target is a small test shim, but all four proposals are legitimate,
  scoped test-coverage or design observations.

**Ramp status:** unchanged — DeepSeek remains a strong tiny-scope
reviewer, now 5-for-5 on clean process (cycles 3, 4, 6, 7, 8) and
2-for-2 on zero-drift citations (cycles 7, 8). It remains review-only
until R4.10 addresses implementation type-chasing. Future cron authors
should verify function ranges against the live worktree before dispatch;
this is the second stale-range assignment after cycle 4.

### Cycle 9 — 2026-08-15 — 3rd zero-drift citation cycle; assigned range overshot the function, DeepSeek self-corrected

Target: `record_progress` (`crates/kitsu-core/src/orchestrator.rs`),
a fresh 27-line ledger-recording function no prior cycle had touched.
Pre-dispatch check confirmed the *start* of the assigned range (2336)
and that the *next function*, `compaction_span`, starts at 2385 — but
did not confirm the assigned range's *end* actually tracked
`record_progress`'s own closing brace. It didn't: `record_progress`
closes at line **2362**; lines 2363-2384 are unrelated trailing content
(a doc comment + four `const` declarations + the `CompactionSpan`
struct) that happen to sit between it and `compaction_span`. The
assigned 2336-2384 read range was therefore ~22 lines wider than the
actual function — the same class of pre-dispatch verification gap
cycles 4 and 8 hit, this time on the *end* boundary rather than the
start. DeepSeek was not told about the extra trailing content and was
not misled by it: its own review header cites the function's true span
(`2336-2362`), and all four proposals' citations land inside 2338-2361
— it implicitly self-corrected the oversized range without comment.

**Setup hiccup (supervisor-side, not billed against DeepSeek's
process):** the first invocation was run from the main checkout's
default cwd, so the prompt's relative path
`crates/kitsu-core/src/orchestrator.rs` resolved against
`/home/mmpg/workspace/kitsu/kitsu2/kitsu` instead of the worktree,
tripping `PolicyDenied` (that root isn't in `--allow-path`) before any
read happened — a supervisor cwd mistake, not a transport failure or a
DeepSeek scope violation, so it did not consume the cycle's one
transport retry. It did burn a small real API call (usage
9,589/493/10,082 prompt/completion/total) that produced no output file
before failing closed — filed below as a kitsu-usage weakness. Re-run
with `cd` into the worktree first: exit 0, no discard.

- **Process: 6th clean review cycle running.** Exactly 1 ranged read
  (2336-2384, 49 lines, matching the assigned — if overshot — range
  exactly) → 1 write, no re-read, no spawn attempt (cap 1 unexercised).
  kitsu-usage (successful run) `{"prompt_tokens":16322,"completion_tokens":1814,"total_tokens":18136}`.
  Run summary: "1 turn(s) ok, 1 file(s) read, 1 file(s) written (1
  change(s))".
- **Citations letter-perfect — 3rd zero-drift cycle (after 7, 8).**
  Spot-checked all citations against ground truth: line 2348
  (`sequence: 0,`), lines 2346-2347 (`let outcome = progress.outcome?;`
  / `Some(ledger.push(CompactionRecord {`), lines 2347-2361 (the
  13-field struct literal, open-to-close), line 2338
  (`cfg: &AgentConfig,`), and line 2350 (`configured_mode:
  cfg.compaction,`) — every quote and every line-number label matches
  the source exactly. R1.7 holding, third cycle running.
- **Hedging correctly calibrated, no unhedged wrong claims.** Proposal
  1 (high — hardcoded `sequence: 0` at 2348 may be a latent ledger bug)
  hedges appropriately on `CompactionLedger::push`'s unread behavior.
  Proposal 2 (high — `progress.outcome?` at 2346 silently drops the
  whole compaction attempt with no ledger entry, log, or telemetry when
  `outcome` is `None`) is stated as fact and is directly verifiable
  from the read range alone — correctly unhedged. Proposal 4 (low —
  `cfg: &AgentConfig` is threaded through solely to read
  `cfg.compaction` at line 2350) is also correctly unhedged for the
  same reason: the single usage is fully visible in-range. Proposal 3
  (medium — the 13-field struct literal at 2347-2361 lists every field
  positionally with no `..` update syntax, brittle to struct
  evolution) is the one soft spot: it frames this as "every field of
  `CompactionProgress` is individually splatted into `CompactionRecord`,"
  which overstates the 1:1-ness — several `CompactionRecord` fields
  (`tokens_before`, `tokens_after`, `messages_before`,
  `messages_after`, `sequence`, `reason`, `configured_mode`) come from
  other function parameters, not from `progress` at all, and whether
  `CompactionProgress` has additional unused fields is unverifiable
  from this range. The proposal does hedge this ("I have not read
  either struct definition. If they are intended to stay 1:1, this is
  cosmetic; if they already have diverged fields...") — imprecise
  framing but not an unhedged false-fact claim like cycle 6's proposal
  2, so not flagged as a correctness dock, only as a precision note.

**Grade: A-.** Matches cycles 7-8's clean-process, zero-citation-drift
bar, and adds a new positive data point: DeepSeek stayed inside the
*true* function boundary even when handed a read range ~75% wider than
the function itself, rather than treating the extra trailing consts/
struct as fair game for citations. Docked half a grade from straight A
only for proposal 3's loose "every field" framing (adequately hedged,
not wrong, just imprecise) — otherwise indistinguishable in quality
from cycle 7/8's best work.

**kitsu weakness noted (not yet filed to roadmap):** a `PolicyDenied`
read failure that occurs before any file is touched still bills a real,
non-trivial API call (9,589 prompt tokens here) with zero useful
output — the turn fails closed correctly, but the cost isn't free. A
caller who mis-scopes `--allow-path` relative to their invocation cwd
pays for it in tokens, not just wall-clock. Low severity (this was a
supervisor error, not a kitsu bug), but a candidate for a future
R-item: resolve/validate policy-denied paths pre-flight so a doomed
turn can fail before the first model call rather than after.

**Ramp status:** unchanged — DeepSeek remains a STRONG tiny-scope
reviewer, now 6-for-6 on clean process (cycles 3, 4, 6, 7, 8, 9) and
3-for-3 on zero-drift citations (cycles 7, 8, 9). Still review-only
until R4.10 (implementation type-chasing, per cycle 5, is unaddressed).
**Process note for future cron/supervisor authors (2nd instance of this
specific gap):** confirming a range's start and confirming the *next*
function's start is not sufficient to confirm the range matches the
*target* function's actual end — non-function content (consts, doc
comments, unrelated structs) can sit between a function's closing brace
and the next `fn`. Verify the target function's own closing brace
directly, not just its neighbor's opening line.

### Cycle 10 — 2026-08-15 — clean process, but the zero-drift citation streak breaks (one off-by-one, twice)

Target: `context_window_shortfall` (`crates/kitsu-core/src/orchestrator.rs`),
a small pure predicate (`fn(reported: u64, estimated_sent: u64, window: u64)
-> bool`) no prior cycle had touched — arithmetic-heavy (overflow/underflow,
threshold logic) and reviewable in isolation, matching DeepSeek's strength.
Pre-dispatch check (3rd instance of this exact gap, per the process note
above) found the assigned 791-835 range again overshot the true function:
the function's own closing brace is at line **806**, not 835 — lines 808-835
are the unrelated `TurnCompletion`/`AgentTurnStatus` definitions and
`run_agent_turn`'s doc comment. Corrected the range to **785-806** (the
function's own doc comment plus body, 22 lines) before dispatch, so DeepSeek
was never handed the oversized range this time.

- **Process: 7th clean review cycle running.** Exactly 1 ranged read
  (785-806, 22 lines, matching the corrected range exactly) → 1 write, no
  re-read, no spawn attempt (cap 1 unexercised). kitsu-usage (successful
  run) `{"prompt_tokens":16876,"completion_tokens":3755,"total_tokens":20631}`.
  Run summary: "1 turn(s) ok, 1 file(s) read, 1 file(s) written (1
  change(s))". No transport retry needed.
- **Citation drift found — streak ends at 3 (cycles 7, 8, 9).** Spot-checked
  all citations against ground truth. Proposals 1, 2 and the standalone
  quote of line 805 (`clamped_boundary || (wide_margin && plausible_clamp)`)
  are letter-perfect. Proposals 3 and 4 both cite `let plausible_clamp =
  reported.saturating_mul(4) >= window;` as **line 803**; the true line is
  **804**. Root cause is visible in Proposal 3's own quoted block: DeepSeek
  compressed the three-line comment spanning true lines 801-803 (`//
  estimator overshooting an accurate tokenizer (hosted routers report` /
  `// exact counts) — cycle-14's HF audit saw this warning on nearly
  every` / `// turn. Stay quiet rather than crying wolf.`) into two
  ellipsis-joined display lines (`// estimator overshooting an accurate
  tokenizer … Stay quiet rather than` / `// crying wolf.`), which silently
  drops one line of verbatim quoting and shifts every subsequent line
  number in that block down by one. Proposal 3's own header still states
  the correct overall range ("lines 798-804"), so the drift is confined to
  the in-body per-line labels on the elided quote, not the citation's
  bounding claim.
- **Proposal 3's core claim is also doubtful, independent of the citation
  drift.** It asserts `plausible_clamp` "computes the opposite condition"
  from what the comment describes, i.e. that the name is inverted. Working
  the boolean algebra from the read range alone: the comment's "stay quiet"
  case is `reported < window/4` (implied clamp window under a quarter of
  resolved); `plausible_clamp = reported.saturating_mul(4) >= window` is
  exactly `reported >= window/4` — the *negation* of the stay-quiet case,
  which is precisely what a variable named "the clamp hypothesis is
  plausible" should mean, and precisely what the final `wide_margin &&
  plausible_clamp` conjunct needs to correctly gate on. There is no
  inversion; the naming and the boolean algebra agree. This claim was
  fully checkable from the 22-line range alone (no hedge was owed or
  given), so it counts as a soft-wrong claim, not an under-hedged one.
  Proposals 1, 2, and 4 hold up: 1 (wide_margin dead zone below
  estimated_sent=256, line 796) is correct arithmetic, appropriately
  hedged on real-world plausibility of tiny estimates; 2 (doc comment at
  788-790 omits the `plausible_clamp` quarter-window conjunct entirely) is
  correct and directly verifiable in-range; 4 (`saturating_mul(4)` at 804
  silently absorbs a `reported > u64::MAX/4` overflow into "plausible")
  is correct and correctly hedged on whether callers bound `reported`
  upstream.

**Grade: B+.** Process discipline stayed perfect — one ranged read
covering exactly the corrected range, one write, no spawning, clean
run — and 3 of 4 proposals (1, 2, 4) are accurate, well-hedged, and
useful. Docked from the A/A- band because this is the first cycle with
an actual citation-line drift (803 vs. 804, twice) rather than an
imprecise-but-hedged framing, and because Proposal 3's central claim is
a checkable-in-range misread of the boolean logic, not merely loosely
worded. Root cause for both: compressing multi-line source text with an
ellipsis before re-numbering is risky — future prompts could add "quote
each source line verbatim on its own labeled line; do not elide or merge
lines even when shortening the surrounding prose" to close this off.

**Ramp status:** DeepSeek remains a STRONG tiny-scope reviewer — process
discipline is now 7-for-7 clean (cycles 3, 4, 6, 7, 8, 9, 10) — but the
citation-fidelity streak drops to 3-for-4 (cycles 7, 8, 9 clean; cycle 10
one repeated off-by-one). Still review-only until R4.10 (implementation
type-chasing, per cycle 5, is unaddressed). The range-overshoot gap (now
hit on cycles 4, 8, 9) continues to require supervisor pre-dispatch
verification of the target function's own closing brace, not just its
neighbor's opening line; cycle 10's pre-check caught it before dispatch,
so DeepSeek was not exposed to the oversized range this time.

### Cycle 11 — 2026-08-15 — first PRODUCTION formatting function; zero-drift streak restored (grade A)

Target: `format_command_result` (`crates/kitsu-core/src/orchestrator.rs`),
a 55-line production formatter (`fn format_command_result(output:
&CommandOutput) -> String`) that renders exit status, elapsed time, and
stdout/stderr truncation-disclosure flags for display — the first cycle to
review a genuinely production display/formatting function rather than a
predicate, wrapper, or ledger-recording helper. Pre-dispatch check confirmed
the cron-supplied range (1796-1850) was exact: the function's own closing
brace is at line 1850 and the next function (`append_command_stream_summary`)
starts at line 1852, so no correction was needed this time.

- **Process: 8th clean review cycle running.** Exactly 1 ranged read
  (1796-1850, 55 lines, matching the assigned range exactly) → 1 write, no
  re-read, no spawn attempt (cap 1 unexercised), exit 0, no transport retry
  needed. kitsu-usage `{"prompt_tokens":17710,"completion_tokens":3274,
  "total_tokens":20984}`. Run summary: "1 turn(s) ok, 1 file(s) read, 1
  file(s) written (1 change(s))".
- **Citations 4/4 letter-perfect — zero-drift streak restored after cycle
  10's off-by-one.** Spot-checked every quoted block against the source:
  lines 1806-1809 (the `TimedOut => match output.signal` arm), line 1812
  (`let mut fields = vec![completion, format!("{} ms", output.elapsed_ms)];`),
  lines 1832-1834 and 1844-1847 (the two `{:?}`-formatted `fields.push`
  calls), and lines 1840-1842 (the artifact-path design comment) all match
  the source verbatim, on both text and line-number labels. Every quoted
  block was reproduced as a complete contiguous span with no ellipsis or
  line-dropping — the cycle-10 prompt fix ("quote each source line
  verbatim... do not elide or merge lines") held.
- **Hedging correctly calibrated on all 4 proposals, and the hedged claims
  checked out.** Supervisor spot-verified the out-of-range facts DeepSeek
  hedged on (not required for grading, but done here for calibration
  data): (1) `output.signal` is `Option<i32>` and is genuinely the same
  field read by both the `Signaled` and `TimedOut` arms (line 212 in the
  `CommandOutput` struct) — proposal 1's copy-paste suspicion is a
  legitimate, correctly-hedged catch ("I have not read the definitions of
  `CommandOutput`... cannot confirm whether reuse is intentional"); (2)
  `elapsed_ms` is `u64` (line 213) — proposal 2 correctly hedged that "if
  it is a plain integer... the practical risk is low," which is exactly
  right; (3) both `bounded_human_value` (line 1642) and
  `terminal_safe_inline` (line 1616) return `String` — proposal 3's claim
  that `{:?}` on a `String`-returning helper adds spurious quote
  characters is **substantively correct**, and was appropriately hedged
  ("I have not read the signatures of either helper"). No unhedged
  out-of-scope claims, and no substantively-wrong claims of any kind this
  cycle. Proposal 4 (documentation nit on the join-contract comment,
  lines 1840-1842) needed no hedge and got none, correctly.

**Grade: A.** Matches cycles 7 and 9's best-cycle bar: perfect process,
perfect citations, well-calibrated hedging, and — uniquely this cycle —
every hedged claim was independently verified to be factually correct
once the supervisor checked the out-of-range code, showing DeepSeek's
hedged inferences about unread callee/struct behavior are not just
cautious but actually well-reasoned from the visible call-site evidence
(e.g. inferring `String`-typed returns from `format!`/string-building
usage patterns in-range). First cycle on a true production
formatting/display function (vs. predicates, test wrappers, or
ledger-recording helpers in cycles 8-10), and the discipline held.

**Ramp status:** DeepSeek remains a STRONG tiny-scope reviewer — process
discipline is now 8-for-8 clean (cycles 3, 4, 6, 7, 8, 9, 10, 11), and
citation fidelity is 4-for-5 (cycles 7, 8, 9, 11 clean; cycle 10 the lone
off-by-one, now with a demonstrated fix holding for a full cycle). Still
review-only until R4.10 (implementation type-chasing, per cycle 5, is
unaddressed). No range-overshoot this cycle — the cron's pre-measured
1796-1850 range was exact on the first check.
