# DeepSeek-V4 worker loop — kitsu drives a second hosted frontier model

> **LIVE 2026-08-14: cycle 1 banked (grade B); cycle 2 and its retry have
> now both produced no deliverable (4 of 4 kitsu invocations against the
> orchestrator review task have failed to write a file).** A fresh HF
> token was present at cycle-1 spawn time (the earlier birth-blocking
> rotation is no longer in effect for this token). See the Cycle log
> below.

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
