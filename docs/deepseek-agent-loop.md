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
