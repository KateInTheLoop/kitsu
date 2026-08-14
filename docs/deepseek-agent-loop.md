# DeepSeek-V4 worker loop — kitsu drives a second hosted frontier model

> **LIVE 2026-08-14: cycle 1 ran and banked (grade B).** A fresh HF token
> was present at cycle-1 spawn time (the earlier birth-blocking rotation
> is no longer in effect for this token). See the Cycle log below.

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
