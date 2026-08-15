# DeepSeek-V4 worker loop — kitsu drives a second hosted frontier model

> **LIVE 2026-08-14: cycle 6 returned to the review lane (grade B+).**
> Tiny single-function scope discipline held a third clean time (1
> read, 1 write, no over-reading, no spawn) on `compact_context`, and
> quoted citation TEXT was letter-perfect again — but this cycle
> shipped its first non-top-ranked proposal stated as unhedged fact
> that was objectively wrong on in-scope information (a false "usage
> is discarded" claim; `compact_context` returns the callee's result
> directly, so nothing is discarded). Proposal 1 independently
> re-derived the same `unwrap_or_default()`-swallows-schema-errors bug
> class that cycle 5 fixed in a sibling function. See the Cycle 6 entry
> for the full citation spot-check. Cycle 5 (DeepSeek's first
> IMPLEMENTATION attempt, grade C+) showed tiny-scope discipline does
> NOT transfer to implementation — DeepSeek made ~36 read/scan calls
> against a 2-read cap there; see that entry for the full account.

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

(appended by the supervisor each cycle, newest last)

### Cycle 1 (2026-08-14) — probe + bank a success. Grade: B

**Phase A — router probes** (`deepseek-ai/DeepSeek-V4-Pro:together`, direct
curl, no kitsu):

1. **Reasoning field / effort cap.** V4-Pro returns hidden reasoning under
   the key `reasoning` (NOT `reasoning_content` as the spec guessed — note
   for kitsu's gateway parser if it special-cases key names per model).
   `reasoning_effort: "low"` measurably shrinks it: a trivial "2+2" prompt
   spent 24 `reasoning_tokens` with `low` vs 49 without (and without it, a
   50-token `max_tokens` cap was fully consumed by reasoning —
   `finish_reason: "length"`, empty `content`, answer never emitted). This
   confirms the inherited-recipe rule: on this model, skipping
   `--reasoning-effort` on a tight output budget doesn't just cost tokens,
   it can eat the entire budget before any visible content is produced.
2. **Forced tool call.** `tool_choice: "required"` against `:together`
   returned a structurally clean `tool_calls[0]`: valid `id`, `type:
   "function"`, correct `function.name`, and `function.arguments` as a
   well-formed JSON string (`{"city":"Paris"}`), `finish_reason:
   "tool_calls"`. No trailing corruption observed (unlike Kimi's early
   `:together` issues) — one clean sample only, not a guarantee under load.
3. **Usage shape.** `prompt_tokens_details.cached_tokens` is present (value
   `0` in all three probe responses — no cache hits on these one-off
   calls). `completion_tokens_details.reasoning_tokens` is also present and
   is the right place to read hidden-reasoning spend.

**Phase B — kitsu run** (`kitsu chat`, one turn, `--agent-spawn-cap 0`,
`--allow-path`/`--allow-write` scoped to the worktree and `reviews/`):
read `crates/kitsu-core/src/context.rs` once (378 lines), wrote
`reviews/deepseek-cycle-1.md` (8 ranked, "line-cited" proposals + summary
table), re-read its own draft once to self-check, done. No other files
read, no sub-agent spawns, no retries needed (first invocation succeeded;
note the initial launch failed with `unexpected argument '--allow-path'`
because those flags belong to the `chat` subcommand, not the top-level
`kitsu` command — a usage-doc gap worth flagging, see weaknesses below).

`kitsu-usage: {"prompt_tokens":31447,"completion_tokens":4817,"total_tokens":36264}`
`kitsu-context: {"used":12237,"effective":107072,"window":131072}`
— under Kimi cycle 4's <40K-token target for the same task.

**Grading — citation accuracy spot-check** (ground truth: the actual file
on disk):

- Correct: the anchor tuple `Option<(u64, usize, bool)>` cited at line 68
  (exact match); `anchor_includes_schema` cited at lines 115–126 (real:
  115–124, off by ≤2); `breakdown` cited at lines 127–138 (real: 126–140,
  close).
- Wrong, and drifting worse toward the back of the file: `CHARS_PER_TOKEN`
  cited at "19–21" (real: 15–16); the anchored branch of `estimate` cited
  at "96–102" with the tail-estimate line quoted as "line 100" (the quoted
  code is real line 103, `estimate`'s anchored match arm is real line
  102); `message_tokens` cited at "38–50" (real: 35–43, and 50 spills into
  the next struct); the buffer cap `let buffer = (window / 10).min(2_048)`
  cited at "line 203" / "lines 203–204" (real: **line 166** — a 37-line
  miss); `ContextBudget::new` cited at "186–205" (real: 159–173, also
  wrong — 186–205 actually lands inside the test module).
- The transcript shows the model performing a self-declared "verification"
  pass (re-reading its own draft and checking 5 citations against the
  source) that rubber-stamped all 5 as "✓", including at least one (the
  "line 100" quote) that is factually off by 3 lines against the file it
  had just read in full. The self-check was not a real cross-reference —
  it repeated its own claim and approved it.
- Content quality is otherwise solid: 8 concrete, mostly non-overlapping
  proposals (estimate drift/no calibration, breakdown/estimate
  inconsistency, missing compaction-circuit-breaker telemetry — correctly
  spotting that the file's own doc comment promises a "circuit breaker"
  that isn't implemented, binary `had_schema` losing schema-cost
  granularity, ignored reasoning-token accounting appropriately hedged
  as "check whether the field exists", opaque anchor tuple, silent
  zero-window pass-through, stale buffer cap for 128K+ windows). No
  fabricated function names or invented API surface.

**Comparison to Kimi cycle 4 (same task, same file, reviews/kimi-cycle-4.md
on dogfood/kimi-k3):** Kimi's citations are letter-perfect — every checked
range (`estimate` 100–107, `anchored` 111–113, `anchor_includes_schema`
122–124, `record` 79–81, `invalidate` 83–93, `message_tokens` 35–43, rate
constants 16–18, `estimate_str` 29–31, the buffer line 166, `doc` 155–158,
`ContextBudget::new` 159–173) matches the real file exactly, with zero
drift. Kimi shipped 5 tightly-scoped, risk-calibrated proposals (each with
an explicit "Risk" section); DeepSeek shipped 8 broader proposals with
good ideas but materially worse citation grounding and no explicit risk
analysis per item. Token cost is comparable (both under the 40K target).
**Grade: B** — genuine, well-scoped, non-hallucinated content that reads
the actual file and engages with its real design tension (measurement vs.
the promised circuit breaker), but "line-cited" is only reliably true for
roughly the first third of the citations; accuracy degrades over the
back half of the file, and the model's own verification pass did not
catch this. Kimi cycle 4 remains the higher bar (A) on the identical task.

**Kitsu weaknesses observed / follow-ups:**

- Docs gap: `--allow-path`/`--allow-write`/`--agent-spawn-cap` are `kitsu
  chat` subcommand flags, not top-level `kitsu` flags; a bare `kitsu
  --gateway ... --allow-path ...` fails with `unexpected argument
  '--allow-path'` before ever reaching a subcommand-usage hint that
  mentions those flags live under `chat`. Low-cost fix: top-level `--help`
  error text (or a "did you mean `kitsu chat --allow-path`?" hint) would
  have saved the retry.
- Model-side (not kitsu's fault, but worth a gateway-level mitigation):
  V4-Pro will happily consume an entire tight `max_tokens` budget on
  hidden reasoning and return empty `content` with `finish_reason:
  "length"` if `reasoning_effort` isn't capped. Confirms the inherited
  recipe's `--reasoning-effort low` rule is load-bearing for this model,
  not just cost hygiene — worth a kitsu warning if a response comes back
  with empty content, `finish_reason: length`, and a nonzero
  `reasoning_tokens` close to the requested cap (silent-truncation
  footgun users would otherwise blame on kitsu).

**Ramp status:** Cycle 1 banked (grade B, first pass "genuine review, not
hand-waving," per the ramp-banking bar Kimi's cycle 4 set). DeepSeek is
clear to proceed to alternating review/implementation tasks from the
shared P1–P11 backlog per the Rules above, same as Kimi post-cycle-4;
future review cycles should ask the model to re-verify its own line
citations against the tool-provided line numbers explicitly (not just
"re-read the draft") since the unprompted self-check this cycle was
insufficient.

### Cycle 2 — 2026-08-14 — citation-discipline fix attempt (no deliverable, grade F/incomplete)

Goal: re-run the review lane on a fresh scope (the
TRIM/SUMMARIZE/COMPACTION path in `crates/kitsu-core/src/orchestrator.rs`
— `enforce_context_budget_recording`, `summarize_or_fallback`,
`request_measurement`/`ensure_chat_request_fits`, `compact_context`,
`trim_tool_results[_counted]`) with a mandatory re-read-before-cite
discipline added to the prompt, specifically to test whether cycle 1's
back-half citation drift is fixable with process. Same flags as cycle 1
(`--gateway huggingface --model deepseek-ai/DeepSeek-V4-Pro:together
--context-window 131072 --timeout-secs 300 --max-output-tokens 24000
--reasoning-effort low --max-rounds 30 --agent-spawn-cap 0`), prompt told
the model to use ranged reads only over the named function line ranges,
write `reviews/deepseek-cycle-2.md` early, and re-read every line range
immediately before citing it.

**Neither attempt produced a deliverable file.** `reviews/deepseek-cycle-2.md`
was never created; only `deepseek-cycle-1.md` exists in `reviews/`.
**The citation-accuracy question this cycle set out to answer is
therefore unanswered — there was nothing to spot-check.**

- **Attempt 1:** the model tried to spawn 6 parallel read subagents
  against `--agent-spawn-cap 0`, got the expected denial, retried
  spawning again on a later tool call, and kitsu withheld the spawn
  tools for the rest of the turn after repeated refusals ("spawn cap
  refusals ignored — spawn tools withheld for the rest of this turn").
  The very next provider call then came back `HTTP 400 client error;
  request rejected; terminal=true` and the turn ended.
  `kitsu-usage: {"prompt_tokens":43772,"completion_tokens":1626,"total_tokens":45398}`,
  `kitsu-context: {"used":13360,"effective":107072,"window":131072}`. 11
  ranged `Read` tool calls happened before the rejection, all inside or
  near the requested function ranges.
- **Retry (after a 60s cooldown, the one retry the loop's rules allow):**
  no spawn attempts this time, but the model abandoned the "named
  function ranges only" instruction entirely — 35 `Read` tool calls,
  many duplicating the same ranges (e.g. `2153–2200` and `2190–2220`
  read twice each), plus two full sequential passes over roughly
  lines 1–2970 of the 2957-line file, and even one read of
  `crates/kitsu-core/src/context.rs` (explicitly out of scope, already
  reviewed in cycle 1). It never called a write tool. At the 300s
  `--timeout-secs` deadline the in-flight provider call was cancelled
  (`inference provider attempt 1/3: cancelled; HTTP 200; cancelled;
  terminal=true`) and kitsu correctly discarded the uncommitted
  provisional turn rather than saving a truncated answer. No
  `kitsu-usage` line was ever emitted for this attempt (the turn never
  reached a billed completion).

Per the loop's cost-bound rule ("one retry only on transport failure"),
no third attempt was made.

**Grade: incomplete / F on this cycle's own terms** — not because the
model produced a bad review, but because it never produced one. This is
a different failure mode than cycle 1 (which shipped genuine but
imperfectly-cited content); cycle 2 shipped nothing, so the citation
question is open, not resolved either way.

**Comparison to cycle 1:** cycle 1 succeeded on a single, moderately
sized function-dense file read mostly forward-to-back. Cycle 2 handed
DeepSeek a *spec* of several disjoint function ranges scattered across
~1000 lines, and the model's response — in both attempts — was to try
to parallelize the reads (blocked by the spawn cap) and then, when
forced to go serial, over-collect far past the requested ranges instead
of trusting the ranges it was given. The "write early" instruction from
the prompt was not followed in either attempt.

**kitsu weaknesses observed / follow-ups:**

- **W3 (new, priority):** after kitsu withholds spawn tools mid-turn
  (following repeated cap-0 refusals), the very next provider request in
  the same turn was rejected with a terminal `HTTP 400`. This reads like
  a request-shape bug introduced by the tool-list mutation mid-turn (the
  provider may be rejecting a `tool_choice`/message referencing a tool
  the withheld-tools request no longer declares, or a malformed followup
  after the synthetic tool-error message). Needs a kitsu-side repro:
  drive a scripted turn with `--agent-spawn-cap 0` and force ≥2 spawn
  attempts in one turn against `:together`, then inspect the outgoing
  request body on the call immediately after tool withholding kicks in.
  This is a correctness bug, not just a UX rough edge — it silently
  killed the entire cycle-2 first attempt.
- **W4 (new):** no incremental/partial-credit path for a turn that runs
  many tool rounds without ever calling a write tool. The prompt
  explicitly said "write the deliverable file EARLY... do not wait until
  the end," and the model ignored it under both attempts; kitsu has no
  mechanism (e.g. a round-count nudge like "you have used 20 of 30
  rounds and have not yet written output") to correct this mid-turn. A
  cheap version: inject a system reminder after N rounds with zero write
  tool calls, reminding the model of any explicit "write early"
  instruction in its own prompt.
- **W5 (new, minor):** `--timeout-secs 300` is tight for a many-small-
  ranged-read review turn once the model over-reads; the failure mode
  (full turn discarded, zero salvageable output, zero usage billed) is
  clean and honest but wastes the entire budget. Not a bug — the
  provisional-output discard on timeout is correct behavior — but the
  retry-cooldown rule combined with a hard per-cycle "one kitsu run"
  budget means an over-reading model can burn the whole cycle without a
  single scored deliverable, as happened here.
- W1 and W2 from cycle 1 (top-level-vs-`chat`-subcommand flag hinting;
  silent-truncation-on-reasoning-budget warning) remain open and are not
  restated in full here.

**Ramp status update:** cycle 2 does not move DeepSeek's ramp forward —
it is a regression in reliability (0 deliverables from 2 attempts) even
though the citation-discipline *prompt* itself was never actually
exercised. Next cycle should shrink scope further (fewer named function
ranges, or one function at a time) rather than repeat the same
multi-range ask, and should keep `--agent-spawn-cap 0` but consider
whether a single, calmer up-front instruction ("do not attempt to spawn
subagents; this run has none available") reduces the spawn-retry
behavior that preceded the W3 HTTP 400. The citation-discipline
hypothesis from this cycle's brief remains untested and should be
retried once a deliverable actually ships.

### Cycle 2 retry — 2026-08-14 — same task, mitigated flags, still 0 deliverables (grade F/incomplete)

Same orchestrator trim/summarize/compaction review task as cycle 2
(`enforce_context_budget_recording`, the summarize/compaction fallback,
`request_measurement`, `ensure_chat_request_fits`), deliverable
`reviews/deepseek-cycle-2.md`, with the two ramp-status mitigations from
the cycle 2 entry applied: **`--agent-spawn-cap 1`** (not `0`, so a
stray spawn degrades instead of triggering the W3 crash path) and a
prompt hard-capping reads ("read ONLY these ranges, AT MOST once each
... total reads: at most ~6 ranged reads ... write EARLY"), plus the
citation re-read-before-cite instruction carried over unchanged. Flags:
`--gateway huggingface --model deepseek-ai/DeepSeek-V4-Pro:together
--context-window 131072 --timeout-secs 300 --max-output-tokens 24000
--reasoning-effort low --max-rounds 30 --agent-spawn-cap 1`.

**Neither attempt produced a deliverable.** `reviews/deepseek-cycle-2.md`
still does not exist. This is DeepSeek's 4th consecutive shipless
invocation on this task (2 from the original cycle 2, 2 here); the
citation-discipline hypothesis remains completely untested three cycles
running.

- **Attempt 1:** ~15 `Read` calls, starting with a broad, out-of-brief
  sweep (`1–50`, `1–60`, `60–120`, `120–180`, `180–240`, `240–300`,
  `300–400` — none of those ranges contain any of the four named
  functions, which all live past line 1950) before it located the real
  targets and read around them (`1951–2070`, `2075–2380`, `2375–2670`,
  `2670–2780`, plus several small re-reads). It also **spawned one
  sub-agent** ("find the four target functions...") despite the prompt's
  explicit "Do NOT spawn sub-agents. Do every read yourself." — the
  spawn was allowed through `--agent-spawn-cap 1` cleanly (81.8s, 21
  lines back) with **no crash and no HTTP 400**, unlike cycle 2's
  cap-0 run. It then read two ranges of `crates/kitsu-core/src/context.rs`
  (explicitly out of scope, already reviewed in cycle 1). No write call
  was ever made. The turn hit the 300s timeout
  (`inference provider attempt 1/3: cancelled; HTTP 200; cancelled;
  terminal=true`) and was correctly discarded with no `kitsu-usage` line
  emitted (never reached a billed completion).
- **Retry (this cycle's own one allowed retry, per the loop's cost-bound
  rule):** worse over-reading — ~28 `Read` calls, including duplicate
  reads of the same ranges (e.g. `1962–2006` read twice, `2013–2025`
  overlapping a prior `2007–2030` read, `2127–2135` re-reading a subset
  of an already-read `2127–2180`), one more disobeyed **spawn** ("Search
  ... for the line numbers where these functions are defined", 111.2s,
  13 lines back — again absorbed cleanly by cap 1, no crash), and again
  two reads into out-of-scope `context.rs`. No write call. Same 300s
  timeout, same clean discard, no `kitsu-usage` line.

Per the loop's cost-bound rule, no third attempt was made.

**Grade: incomplete / F on this cycle's own terms**, same as the
original cycle 2 — not a bad review, no review at all.

**What the mitigations did and didn't fix:**
- **Fixed:** the W3 crash path. `--agent-spawn-cap 1` absorbed a
  disobeyed spawn attempt in both attempts with a clean tool response
  and no downstream `HTTP 400` — the mid-turn tool-list-mutation crash
  that killed cycle 2's first attempt did not recur. This is now decent
  evidence the crash is specifically tied to the cap-0 "deny, retry,
  deny-and-withhold" sequence, not to spawning-while-capped in general;
  R4.8 should reproduce with cap 0 specifically to confirm the isolate.
- **Not fixed:** DeepSeek still tried to spawn in both attempts despite
  an explicit, unambiguous prose prohibition ("Do NOT spawn sub-agents")
  — a model-adherence gap, not a kitsu bug, but each spawn cost 80–110s
  of the 300s budget, materially contributing to both timeouts.
- **Not fixed:** the "at most ~6 ranged reads" cap was not merely missed
  but blown by 2–4x in both attempts (15 and 28 reads respectively, the
  second attempt *more* over-read than the first despite being a retry
  with the same tightened prompt). Prose-only read caps have now failed
  in 3 of 3 tries across cycle 2 and this retry.
- **Not fixed:** "write the deliverable EARLY" was ignored in all 4
  invocations across both cycles; kitsu still has no mid-turn mechanism
  to surface or enforce this class of instruction (W4, unchanged).
- **Recurred:** both attempts here also strayed into `context.rs`,
  mirroring cycle 2's retry exactly — scope creep beyond the named file
  is a repeatable failure mode, not a one-off.

**kitsu weaknesses observed / follow-ups:**

- **W3 status: mitigation confirmed working.** `--agent-spawn-cap 1`
  is a safe near-term default for models that disobey spawn
  instructions — it avoids the cap-0 crash entirely. Recommend keeping
  cap 1 as the loop default until R4.8 lands, and only then
  re-testing cap 0 to verify the fix directly.
- **W6 (new):** kitsu has a technical spawn-count cap
  (`--agent-spawn-cap`) but no analogous technical *read-count* or
  *read-scope* cap. Every read-discipline instruction in this loop has
  been prose-only and prose-only read caps are now 0-for-3 against this
  model on this task. A `--max-tool-calls-per-turn <tool> <N>` style
  cap (or a narrower `--allow-path` that excludes files not named in a
  companion allowlist) would give the supervisor a real lever instead
  of relying on the model to self-limit.
- **W4 (recurred, unchanged):** still no mid-turn nudge when a model
  burns many rounds with zero write calls, despite explicit "write
  early" prompt instructions ignored in all 4 invocations so far.
- **W5 (recurred, unchanged):** timeout-discard is still correct and
  honest but still zero-salvage; two more full 300s budgets burned with
  nothing scored.
- **New observation:** disobeyed spawns are expensive even when
  harmless — 81.8s and 111.2s respectively, roughly a quarter to a
  third of the entire 300s timeout budget, on top of the model already
  over-reading. A prompt that both forbids spawning *and* explains why
  ("no sub-agents are needed or available for a task this size") did
  not stop the attempt; worth testing whether kitsu withholding spawn
  tools proactively for very small `--agent-spawn-cap` values (e.g. 1)
  when the prompt contains an explicit no-spawn instruction is
  feasible, though this borders on prompt-inspection scope creep for
  kitsu and may not be worth building.

**Ramp status update:** DeepSeek is now 1-for-5 on kitsu invocations
(cycle 1 succeeded at B; cycles 2 and 2-retry combine for 4 straight
shipless turns). The spawn-crash mitigation (cap 1) worked as intended
and should stay in place. The read-scope mitigation (prose cap) did not
work and needs a different approach next cycle: shrink the task to a
**single function** (e.g. just `request_measurement`, ~60 lines) rather
than four disjoint ranges, to test whether DeepSeek can stay disciplined
on a task an order of magnitude smaller before re-attempting the full
four-function ask. If a single-function cycle also fails to ship, that
points at a deeper DeepSeek-V4 exploration-discipline problem rather
than a scope-sizing problem, and the review-lane task type itself should
be reconsidered for this model in favor of more constrained,
single-read-then-answer tasks. The citation-discipline hypothesis from
cycle 2's brief remains completely untested and should not be
re-attempted until a deliverable actually ships.

### Cycle 3 (2026-08-14) — single ~60-line function, shipped. Grade: A-

**Scope-sizing experiment, result: over-reading vanished.** Target
shrunk to exactly `request_measurement` (`crates/kitsu-core/src/orchestrator.rs`
L1954–1999) + its helper `check_measurement` (L2001–2012), confirmed by
direct read of the worktree file before dispatch. Prompt repeated cycle
2's read-cap and write-early instructions but pinned them to a single
named range instead of four disjoint ones, and dropped `--agent-spawn-cap`
to 1 (unchanged from the retry). Attempt 1 shipped on the first try — no
retry needed.

**THE KEY RESULT: exactly 1 read, then write.** kitsu stderr shows one
`Read(…/orchestrator.rs)` call — `ranged read 1950-2012`, 63 lines — then
one `Write(…/reviews/deepseek-cycle-3.md)` call, then done. Run summary:
"1 turn(s) ok, 1 file(s) read, 1 file(s) written (1 change(s))". No
spawn attempt this time (cap 1 was never exercised — the model simply
didn't try). kitsu-usage: prompt_tokens 12,985 / completion_tokens 3,536
/ total 16,521 — far under the 24K output cap and well inside the 300s
timeout (turn completed in well under a minute). This is a complete
reversal from cycles 2/2-retry's 15–28 reads, 4-for-4 shipless, and it
happened with the *same* prose read-cap wording that was "0-for-3" at
larger scope — the only variable that changed was scope size.

**Deliverable — `reviews/deepseek-cycle-3.md`, 4 ranked proposals:**
1. *(Medium)* `check_measurement` gates on `measurement.target` and never
   consults `measurement.effective`, hypothesized to let a request that
   fits the full window but exceeds the max-tokens reservation pass the
   guard — explicitly hedged as conditional on `ContextBudget`'s
   internals, which are out of scope and correctly not read.
2. *(Low)* the byte-path (`cfg.context_window` is `None`) silently drops
   `max_tokens`, an asymmetric contract vs. the token path.
3. *(Low)* `has_schema: bool` + `schema_json: &str` is a redundant pair;
   collapse to `schema_json: Option<&str>`.
4. *(Low)* the two `if let Some(window)` / `else` branches duplicate the
   full `RequestMeasurement` struct literal; suggests a single
   construction with conditional fields.

**Citation spot-check: every direct quote is letter-perfect.** Checked
all five quoted fragments against the actual lines 1945–2019 (read
independently by the supervisor before dispatch):
- `if measurement.used > measurement.target {` — exact match, L2002.
- `target: cfg.max_context_bytes as u64,` — exact match, L1993.
- `fn request_measurement(` / `messages: &[ChatMessage],` /
  `cfg: &AgentConfig,` / `schema_json: &str,` / `has_schema: bool,` —
  exact match, L1954–1958, including indentation.
This is a sharp reversal from cycle 1's citation drift (buffer cap
mis-cited by 37 lines, `ContextBudget::new` mis-cited by a full range) —
zero drift here, on every checkable citation.

**One proposal is analytically wrong when checked against out-of-scope
ground truth, but honestly hedged.** Proposal 1's "bug" doesn't actually
exist: `ContextBudget::new` (`crates/kitsu-core/src/context.rs` L159–173,
outside the granted scope) defines `target = effective − buffer`, i.e.
`target` is *always* the stricter of the two bounds, so checking against
`target` alone is more conservative than checking `effective`, not less.
The model could not have known this without reading `context.rs`, which
it correctly did not do — and it phrased the finding as "If
`ContextBudget::target` returns X, then Y", not as an assertion. That is
the right way to flag an out-of-scope dependency; it just happened to
resolve to "no bug" once checked. Docked half a grade for the top-ranked
proposal being wrong in substance despite the process being sound;
proposals 2–4 are all correct and citation-accurate.

**Grade: A-.** Best citation accuracy of any DeepSeek cycle to date
(cycle 1 was B, docked specifically for drift); this cycle has zero
drift but loses ground on one proposal's real-world correctness.
Compares favorably to Kimi's citation-perfect A-grade context.rs review
in scope discipline, though this was a much smaller unit.

**Scope-sizing vs. exploration-discipline verdict: this data point
favors scope-sizing.** The only variables that changed between this
cycle and the 4-for-4-failed cycle-2/retry are (a) the named range
shrank from four disjoint multi-hundred-line spans in a 2,957-line file
to one contiguous ~60-line span, and (b) the prompt made the "read
exactly once" instruction more mechanically explicit ("ONE single
ranged read of that range"). Prose read-caps were "0-for-3" at the
larger scope and "1-for-1" here with materially the same prose style —
the shrink is the more likely explanatory variable, not a prompt
rewording alone, since cycle 2's retry already had an explicit numeric
prose cap ("~6 reads") and still blew it 2–4x. **Caveat: n=1.** This
does not rule out a discipline problem that only manifests past some
scope threshold, and it does not prove DeepSeek would stay disciplined
on a *review* task with more than one function, or on an *implementation*
task. Recommended next steps before declaring DeepSeek broadly viable:
(a) replicate at this same tiny scope once more to rule out a lucky
draw; (b) if that replicates, step scope size back up incrementally
(e.g. ~150 lines, ~400 lines) to find the discipline threshold rather
than jumping back to a 2,957-line file; (c) R4.10 (technical read cap)
is still worth building regardless — it would make the *next* cycle's
result deterministic instead of prompt-luck-dependent. DeepSeek is not
ruled out as a contributor, but it should stay on tightly-scoped,
single-unit review tasks until a second small-scope success replicates
this one.

**Ramp status update:** DeepSeek is now 2-for-6 on kitsu invocations
(cycle 1 B, cycle 2 ×2 + retry ×2 shipless, cycle 3 A-). First shipped
deliverable since cycle 1. Next cycle: replicate at the same tiny scope
(a different ~60-line function, e.g. `ensure_chat_request_fits` alone)
before widening scope, to confirm this wasn't a one-off.

### Cycle 4 — 2026-08-14 — REPLICATED tiny-scope success (grade A-)

**n=2 confirms the lever: scope size, not model discipline, was cycle
2's problem.** Target was `ensure_chat_request_fits`, but the supervisor
briefing's line range (2015-2077, "~62 lines," "next fn
`enforce_context_budget` @2078") was wrong against the actual worktree
file: the real function is only **lines 2015-2034 (20 lines)** — lines
2035-2077 are `compact_context`, an unrelated `pub async fn`, and
`enforce_context_budget` is further down still. The supervisor corrected
the assigned range to **2013-2034** (doc comment through closing brace,
22 physical lines) before running DeepSeek, to keep the single-function
scope intent intact rather than following a stale range into a second
function. Commit `<pending>` records this cycle.

- **Read count: exactly 1 source read** (`orchestrator.rs`, ranged
  2013-2034), matching cycle 3 precisely. It then wrote
  `reviews/deepseek-cycle-4.md` on the first attempt, read that file back
  once (self-check, not source), and made one `Edit` to tighten prose —
  zero additional source reads, zero re-reads of the target file. No
  spawn attempted; `--agent-spawn-cap 1` unexercised again. kitsu-usage
  27,081 / 4,684 / 31,765 — comparable to cycle 3's footprint.
- **Deliverable grade A-.** 4 ranked, severity-tagged proposals: (1) 🔴
  `serde_json::to_string(tools).unwrap_or_default()` silently drops the
  tools schema from the measurement on a serialization error, so the
  provider still gets the real (unmeasured) schema — a real, if
  narrow-trigger, silent-failure smell; (2) 🟡 `has_schema` and
  `schema_json` can disagree (`Some(vec![])` → `has_schema=false` but
  `schema_json="[]"`, a non-empty string) — verified true against the
  code; (3) 🟢 the nested `.map().unwrap_or_default()` chain conflates
  two distinct fallback cases, a fair readability critique; (4) 🔵 the
  "last-line guard" doc comment doesn't match the function's actual
  behavior (it's a pure delegator to
  `check_measurement(request_measurement(...))`), correctly scoped to
  not claim knowledge of functions it didn't read. **No proposal was
  substantively wrong this cycle** — a clean improvement over cycle 3,
  whose top-ranked proposal hypothesized a non-existent bug.
- **Citation spot-check: quoted TEXT letter-perfect on every citation**
  (all four code blocks match the source byte-for-byte, including
  indentation), but **line-number LABELS drifted by ~1 line on every
  citation** — the doc comment cited as "line 2013" (actual 2014),
  `has_schema` cited as "line 2019" (actual 2020), and the 5-line
  `schema_json` chain labeled "lines 2021–2024" (actual 2021-2025, one
  line short). kitsu's `fs_read_file` ranged read returns raw text with
  **no line-number prefixes** (confirmed in
  `crates/kitsu-tools/src/builtins/fs.rs`'s `read_capped_line_range` —
  it streams and joins stripped lines with no numbering), so the model
  must count lines itself from the requested `start_line`; it undercounted
  by one throughout, plausibly because the assigned range's first line
  (2013) was blank and got miscounted as the first content line. This is
  a real but minor regression from cycle 3's fully accurate line labels —
  docked half a grade, same as cycle 3 but for a different reason (there:
  one wrong bug claim; here: systematic off-by-one citation labels with
  otherwise-perfect text and substance).
- Minor, non-citation nit: proposal 1's suggested fix code calls
  `AgentError::internal(...)`, a constructor not verified to exist on the
  enum (out of the model's read scope by design — `AgentError` lives
  elsewhere in the file). Doesn't affect the finding's validity, only the
  literal patch snippet.

**Verdict: REPLICATED, n=2.** Both tiny-scope cycles (3 and 4) hit
exactly 1 source read + 1 successful write-first-attempt, zero
over-reading, zero spawn incidents, and A- grades — on two different
functions, with two different specific flaws (wrong-bug-claim vs.
citation-label drift) rather than a repeat of the same weakness. Scope
size is confirmed as the dominant lever for DeepSeek viability on kitsu.

**New candidate weakness (not yet filed to roadmap):** `fs_read_file`
ranged reads return no line-number prefixes, forcing every model to
self-count from `start_line`; both DeepSeek cycles that produced
line-cited output (1 and 4) show number drift while text drift is now
zero. A cheap fix — prefixing each served line with its absolute file
line number (`cat -n`-style) on ranged reads only — would likely remove
this whole class of citation error for every model, not just DeepSeek.

**Ramp status:** DeepSeek is 3-for-7 on kitsu invocations (cycle 1 B,
cycle 2 ×2 + retry ×2 shipless, cycle 3 A-, cycle 4 A-), and 3-for-3
once scope is single-function-tight. Per the plan, cycle 5 should try a
tiny single-function **implementation** task. The journal-cap bug that
made `orchestrator.rs` uneditable was fixed on `main` (edit/write now
share the 1 MiB write cap), so a fresh binary build can edit
`orchestrator.rs` directly — but DeepSeek's first code-writing cycle
should still prefer a small, self-contained function (or a small file)
over a large one, consistent with the scope-size lesson above.

### Cycle 5 — 2026-08-14 — first IMPLEMENTATION cycle, partial (grade C+)

Task: implement DeepSeek's own strongest cycle-4 proposal on
`ensure_chat_request_fits` (proposal #1 — `serde_json::to_string(tools)
.unwrap_or_default()` silently turns a serialization failure into an
empty `schema_json`, so `request_measurement` undercounts the request
while the real, unmeasured tools schema still ships to the provider)
plus a unit test that fails without the fix. Proposals #2/#3 were
derivative style nits and #4 was a doc-comment wording fix, so #1 was
the clear pick. The supervisor pre-corrected DeepSeek's own proposal
snippet, which called a nonexistent `AgentError::internal(...)`; the
prompt told DeepSeek to use the real `AgentError::Loop(String)` variant
instead.

**Journal-cap fix VALIDATED on a third model.** DeepSeek edited
`orchestrator.rs` (129 KiB) directly with no journal-limit error — 2
file-edit checkpoints landed (`orchestrator.rs` and, transiently,
`orchestrator_tests.rs`) even though the turn's final chat message was
later discarded. **New operational finding: checkpoint writes persist
to disk independent of whether the turn's own message commits** — "the
turn was discarded" does NOT mean "nothing changed"; a supervisor must
always check `git status`/`git diff` after a failed/timed-out turn, not
just the transcript.

**Read discipline did not transfer from review to implementation.**
Prompted with the same "at most 2 reads" cap that produced 1-read
successes in cycles 3 and 4, DeepSeek made **~36 tool calls before its
first edit** (16 `Read`, 18 `fs_scan_file`, 2 `List`, across
`orchestrator.rs`, `orchestrator_tests.rs`, `kitsu-providers/src/types.rs`,
and `lib.rs`) — chasing type definitions (`ChatRequest`, `ToolDescriptor`,
`AgentError`) it needed to write a compiling test. This refines, not
contradicts, the cycle-3/4 finding: **tiny scope disciplines
READ-ONLY review, but an implementation-plus-test task legitimately
needs cross-file type knowledge that a single-function read range
can't supply, and DeepSeek does not ration those follow-on reads even
under an explicit numeric cap.** No spawn was attempted (cap 1
unexercised — 0 `spawn_agent` calls in the transcript). The turn then
hit the 300s timeout mid-test-writing and was discarded; no usage was
billed (consistent with prior discarded-turn cycles).

**Scope crept beyond the target function, corrected by the
supervisor.** DeepSeek's checkpoint edits (a) fixed
`ensure_chat_request_fits` exactly as specified — correct, and (b)
bumped its visibility from private to `pub(crate)` and added an unused
import to `orchestrator_tests.rs`, apparently in preparation for a test
it never got to write. Both were unnecessary: `orchestrator_tests` is
declared via `#[path] mod orchestrator_tests;` **inside**
`orchestrator.rs`, making it a descendant module that already sees
`orchestrator`'s private items under Rust's ordinary visibility rules.
The supervisor reverted both, leaving a minimal diff scoped to exactly
the target function's body — matching the letter of "editing ONLY
`ensure_chat_request_fits`."

**The mandated unit test turned out to be unconstructible, not just
undelivered.** DeepSeek's own proposal (and the cycle brief) suggested
reproducing the serialization failure with a NaN/Infinite float inside
the tools schema. The supervisor verified experimentally (standalone
`serde_json` probe) that this is impossible through any safe public
API: `serde_json::Number::from_f64(f64::NAN)` and `::from_f64(f64::INFINITY)`
both return `None`, and parsing an overflow-exponent literal like
`"1e400"` is a hard parse error (`"number out of range"`), not a
lenient `Infinity` value. Since `ToolDescriptor::parameters` is a
`serde_json::Value` and `Value` cannot hold a non-finite number by
construction, **there is no legitimate input that makes
`serde_json::to_string(&Vec<ToolDescriptor>)` fail today** — the bug
proposal #1 fixes is real defensive correctness (never silently swallow
a `Result` you could propagate) but is currently unreachable dead code,
and no genuine "fails without the fix, passes with it" test exists for
it without changing `ToolDescriptor`'s type (out of scope). The
supervisor did not fabricate a hollow test and left the deliverable
without one, docking the grade accordingly rather than inflating it
with a test that would prove nothing.

**Verification (workspace-wide, after the supervisor's trim):**
`cargo build --workspace` clean (first attempt hit a spurious
`CommandMode::AutoAllow` non-exhaustive-match error from stale
incremental artifacts — `CARGO_TARGET_DIR` is intentionally shared
between the main-checkout driver rebuild and the worktree build per
spec, and this run shows that sharing can corrupt incremental state
across two different workspace roots with identically-named crates;
`cargo clean -p kitsu -p kitsu-tools -p kitsu-core -p kitsu-render`
resolved it cleanly on rebuild — a process note, not a kitsu bug).
`cargo clippy --workspace --all-targets` warning-free. `cargo test
--workspace --no-fail-fast`: every suite green except the known PTY
flake `test_query_only_interactive_session_does_not_create_a_snapshot`.
`cargo fmt --all -- --check` clean, no reformatting needed.

**Grade: C+ (partial).** The implemented fix is correct, minimal,
exactly matches the selected proposal, and compiles/tests/lints clean
workspace-wide — a real, if small, first code contribution. Docked
substantially because (a) DeepSeek never delivered the mandated test
(turn timed out mid-write), (b) the supervisor had to trim two
out-of-scope touches (visibility bump, unused import) before the diff
was acceptable, and (c) the supervisor, not DeepSeek, discovered that
the requested test strategy doesn't actually work — a finding that
should have been caught in review, not in verification.

**kitsu weaknesses filed (backlog):**
- W7: no technical read/tool-call cap exists yet (R4.10, filed cycle 2)
  — this cycle is further evidence it's needed for implementation
  tasks specifically, where prose "read at most N" caps that hold for
  review tasks stop working once the model needs supporting type
  definitions.
- W8: a discarded/timed-out turn still leaves file-edit checkpoints on
  disk with no summary of what changed printed to the user/supervisor
  — kitsu should surface a clear "N files were modified by this
  discarded turn" notice distinct from the per-checkpoint diff lines
  already shown, so a caller doesn't have to know to run `git status`
  themselves.

**Ramp status:** DeepSeek is now 3-for-8 review/implementation attempts
(review lane 3-for-3 once scope is single-function-tight: cycles 1 B,
3 A-, 4 A-; implementation lane 0-for-1 clean, 1-for-1 partial: cycle 5
C+ landed a correct fix but no test). The journal-cap fix is confirmed
working end-to-end for DeepSeek specifically (third model validated,
after Kimi and GLM). Next implementation cycle should (a) keep the
target function tiny and self-contained as here, (b) additionally scope
the test requirement to something DEMONSTRABLY constructible before
the cycle starts — the supervisor should verify a failing-test
reproduction is achievable BEFORE briefing DeepSeek, not after — and
(c) consider a longer `--timeout-secs` for implementation-plus-test
turns, since this one needed real exploration budget that a review-only
turn doesn't.

### Cycle 6 — 2026-08-14 — back on review lane, tiny scope holds again (grade B+)

Per the cycle-5 verdict (implementation reignites over-reading; keep
DeepSeek on tiny-scope reviews until R4.10), cycle 6 returned to the
review lane on a fresh single function: `compact_context`
(`crates/kitsu-core/src/orchestrator.rs`, ground truth lines 2041-2072,
doc comment 2038-2040). Prompt mandated exactly one ranged read
(2038-2077) then an immediate write. `--agent-spawn-cap 1` kept as a
safety net; no spawn was attempted. Commit `<see below>` on
`dogfood/deepseek-v4`.

**Process — discipline held perfectly:** exactly **1 read** (single
ranged read 2038-2077) and **1 write**, first attempt, no re-reads, no
spawn. kitsu-usage 14,204 / 2,338 / 16,542 (prompt/completion/total) —
the cheapest cycle yet. This is the third consecutive tiny-scope review
with clean 1-read/1-write execution (cycles 3, 4, 6), confirming the
scope-sizing lever again.

**Deliverable** (`reviews/deepseek-cycle-6.md`): 4 ranked proposals.

**Citation spot-check:** all 4 quoted code blocks are **letter-perfect**
against ground truth (exact text, whitespace included). Line-number
*labels* drifted as expected from the known ranged-read issue (R1.7,
filed cycle 4) — 3 of 4 labels off by exactly one line (mixed direction:
proposals 1/2/4-body cited one line high, proposal 4's parameter
citation one line low), one label (proposal 3) exactly correct. This
continues the pattern: DeepSeek's quoted TEXT is trustworthy, its
self-counted labels are not.

**Substantive correctness — mixed, the first review cycle with a real
non-top-ranked error:**
- **Proposal 1 (high, correct and valuable):** flags
  `serde_json::to_string(tools.descriptors()).unwrap_or_default()`
  silently swallowing a serialization failure into an empty schema
  string. This is the *same bug class* DeepSeek's own cycle-4 proposal
  found and cycle-5 fixed in the sibling function
  `ensure_chat_request_fits` — DeepSeek (no memory of prior cycles)
  independently re-derived the pattern in a second call site. Confirms
  it's a real, recurring anti-pattern in this file, not a one-off.
- **Proposal 2 (medium, substantively WRONG, unhedged):** claims the
  `usage` local is "discarded when the function returns the
  `CompactionReport`." Supervisor verified this is false and was
  verifiable from the assigned scope alone: `compact_context`'s body
  ends with `enforce_context_budget_recording(...).await` as its own
  tail expression — there is no post-processing step that could discard
  anything, the callee's result *is* the function's return value.
  (Confirmed further out-of-scope: `CompactionReport` has a `pub usage`
  field populated from `*turn_usage` — the same `usage` local — on
  every return path of the callee.) Unlike cycle 3's wrong proposal,
  this one was stated as fact rather than conditionally hedged.
- **Proposal 3 (medium, partially wrong):** calls the literal `1`
  (positional arg to `enforce_context_budget_recording`) an ambiguous
  "retry count." It is actually the `round: u32` parameter (a call-site
  detail outside the assigned range, so the mislabel is understandable)
  — but the underlying code-smell observation (an unlabeled positional
  literal in a 12-argument call) remains valid regardless of what it
  turns out to represent.
- **Proposal 4 (low, correct):** narrowing `tools: &Registry` to
  `&[ToolDescriptor]` since only `.descriptors()` is used — accurate
  within scope, reasonable, low-risk.

**Grade: B+.** Process (scope discipline, quote fidelity) matches
cycles 3-4's best showing, but this is the first review cycle with a
non-top-ranked proposal stated as unhedged fact and objectively wrong
on information fully inside the assigned scope — a step down from
cycle 4's "citations perfect, zero wrong proposals" and closer to (but
better-grounded than) cycle 1's citation-drift-driven B.

**Ramp verdict CONFIRMED further (n=3 clean review cycles: 3, 4, 6, all
1-read/1-write):** tiny single-function scope remains DeepSeek's
reliable lever for shipping *something* on review tasks, but "ships
cleanly" and "every proposal is correct" are separable axes — cycle 6
shows a clean-process cycle can still carry a confidently wrong claim.
Recommendation for cycle 7: add an explicit "for each proposal, state
what you verified from the read text vs. what you're inferring about
code you didn't read" instruction, to convert unhedged inferences (like
proposal 2 here) into flagged hedges (like cycle 3's hedged wrong
proposal, which the grading rubric treated more leniently). Role
decision unchanged: stay on tiny-scope reviews until R4.10 (technical
read cap) lands and an implementation retry is warranted.
