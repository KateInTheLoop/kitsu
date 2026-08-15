# GLM-5.2 worker loop — kitsu drives a fourth hosted frontier model

> **LIVE 2026-08-14.** A working HF token (Inference-Providers scope) is
> at `~/.cache/kitsu-hf-token`; each tick reads it there and probes once
> — on 401/402/403 or missing file the tick ends silently. Probe
> confirmed `zai-org/GLM-5.2:together` returns a structurally-intact
> forced tool call, hidden reasoning under the key **`reasoning`**,
> `--reasoning-effort low` honored (26 reasoning tokens), and
> `prompt_tokens_details.cached_tokens` present.

Fourth contributor in the collaboration (2026-08-14, user direction):
Claude (main session, orchestrator + reviewer), Kimi-K3
(`docs/kimi-k3-agent-loop.md`), DeepSeek-V4 (`docs/deepseek-agent-loop.md`),
and GLM-5.2 (this loop). Model: **`zai-org/GLM-5.2`** via the HF router
(Zhipu AI; newest GLM line as of 2026-08-14). Providers observed:
novita, together, fireworks-ai, featherless-ai, zai-org, scaleway,
baseten, deepinfra. **Goal: bring GLM-5.2 to Kimi-K3's level of kitsu
support and contribution** — same ramp, same recipe, same guardrails.

## Inherited frontier recipe (every prior lesson applied from day one)

GLM starts with everything Kimi and DeepSeek took many cycles to learn:

- **Pin a provider**: `--model zai-org/GLM-5.2:together` (together fixed
  Kimi's trailing-tool-call corruption; re-evaluate in cycle 1 if it
  misbehaves).
- **Cap hidden reasoning**: `--reasoning-effort low` for
  exploration/review (load-bearing, not just cost — an uncapped tight
  budget gets consumed by reasoning and returns empty content).
  `--max-output-tokens 24000`, `--timeout-secs 300`.
- **Round budget is separate from token budget**: `--max-rounds 30`
  (the default 10 throttled a design-correct many-edit Kimi cycle).
- **Spawn cap = 1, not 0**: cap 0 crashes the turn with `HTTP 400` if
  the model attempts a spawn (roadmap R4.8, DeepSeek cycle 2); cap 1
  degrades gracefully. ALSO forbid spawns in prose ("do NOT spawn
  sub-agents; do all reads/edits yourself").
- **Scope reads tightly + hard read-cap**: name the exact files/ranges;
  "read AT MOST those ranges then you MUST write; do not re-read; write
  early and refine" (DeepSeek cycle 2 burned a whole cycle over-reading).
- **Citation discipline for reviews**: re-read each exact line range
  immediately before citing it and quote the real text; never cite from
  memory (DeepSeek cycle 1 drifted 30-40 lines in the back half).
- **Workspace-wide verification**: a struct-field or estimator change
  can break construction/test sites in OTHER crates; run `cargo
  build/clippy/test --workspace`, never crate-scoped.

## Cycle 1 (first live tick): probe + bank a success

Phase A — supervisor runs 3 router probes (curl, ~1K tokens; record in
the log, never the token): (1) reasoning key + does `reasoning_effort
low` shrink it; (2) forced tool call intact from `:together`; (3) usage
shape (`cached_tokens` present?). [Already spot-checked at spec-creation:
reasoning key is `reasoning`, tool call intact, cached_tokens present —
re-confirm in the run and log exact numbers.]

Phase B — kitsu run, ONE file review: `crates/kitsu-core/src/context.rs`
→ `reviews/glm-cycle-1.md` (ranked, line-cited proposals). **This is the
same task Kimi cycle 4 banked at grade A and DeepSeek cycle 1 at grade
B — a direct THREE-WAY cross-model comparison** on identical work.

Later cycles: alternate review and implementation from the shared
backlog (Kimi doc's P1–P11); never assign the same item to two
contributors in the same hour. Kimi has shipped P1, P2, P3 (and P4 in
flight) — GLM should take fresh items.

## Rules

- **Branch**: all work on `dogfood/glm-5.2`, worktree at
  `/home/mmpg/.cache/glm-worktree` (MAIN DISK — never /tmp; the tmpfs is
  5.7G, a kitsu target needs ~12G). Any cargo command uses
  `CARGO_TARGET_DIR=/home/mmpg/.cache/glm-worktree-target`.
- **Gateway**: `--gateway huggingface --model zai-org/GLM-5.2:together
  --context-window 131072 --timeout-secs 300 --max-output-tokens 24000
  --reasoning-effort low --max-rounds 30 --agent-spawn-cap 1`.
- **COMMIT IDENTITY**: every commit authored exactly `KateInTheLoop
  <315232649+KateInTheLoop@users.noreply.github.com>`; commit with `git
  -c user.name=KateInTheLoop -c user.email=315232649+KateInTheLoop@users.noreply.github.com
  commit ...`; never any other -c user.email; verify author after commit.
- **Token**: supervisor reads it from `~/.cache/kitsu-hf-token`; env/header
  ONLY, never into any file, commit, log, or doc.
- **Read scope**: `--allow-path <worktree>`; write access only to the
  worktree's `reviews/` (widen to a named crate only for an
  implementation cycle).
- **Cost bounds**: one kitsu run per cycle, one retry on transport
  failure with a 60s synchronous cooldown; scoped prompts.
- **Supervisor**: ONE Sonnet agent per cycle. Runs kitsu itself, captures
  stderr receipts, grades the deliverable (with citation spot-checks),
  appends the cycle log + observed kitsu weaknesses on the work branch,
  commits on `dogfood/glm-5.2` (Kate identity, Claude trailer), pushes,
  verifies main clean. Supervisors never spawn helper agents and never
  pause to await notifications — every wait is a synchronous `sleep`.

## Cycle log

(appended by the supervisor each cycle, newest last)

### Cycle 1 — 2026-08-14 — GLM debut FAILED (transient provider transport; grade F)

Probes all green: reasoning under `message.reasoning` / `delta.reasoning`;
`reasoning_effort low` gives a modest ~10-30% reasoning shrink (143-184
vs 199 tokens — not the dramatic drop the spec's spot-check saw, logged
as a discrepancy) and a tight `max_tokens` still burns the whole budget
on reasoning (recipe warning confirmed real); forced tool call intact
from `:together`; `cached_tokens` present (0 on short prompts).

**Phase B produced no deliverable — provider transport, not GLM.** Build
clean. Attempt 1: GLM read the file, then `:together` returned **HTTP 503
three times** → kitsu gave up (turn correctly discarded). 60s cooldown,
one retry: read again, then **HTTP 200 with an unparseable body**
(`invalid response; HTTP 200; body failed`). Retry budget exhausted, no
third attempt. GLM never advanced past its first read in either attempt —
citation spot-check N/A. This is a **different failure class** than
Kimi's tool-call corruption or DeepSeek's over-reading: pure `:together`
backend instability (two failure modes in ~2 min). No spawn attempted
(cap 1 unexercised).

**Provider re-check (post-cycle, curl only):** all four candidate
providers now answer 3/3 with tool calls intact — `together`,
`fireworks-ai`, `novita`, `zai-org`. So the cycle-1 failure was a
**transient blip**, not a persistent `:together` problem. **Decision:
keep the `:together` pin** (proven recipe provider, healthy now); if
cycle 2 ALSO fails on transport, switch the pin to `fireworks-ai`.

**kitsu weaknesses filed:**
- W1 (→ roadmap R4.9): `invalid response; HTTP 200; body failed` does
  not disclose WHICH parse step/chunk failed — this cycle couldn't
  confirm root cause because the error is opaque. A disclosing parse
  error would have.
- W2: fail-closed discard on transport error means attempt-1's real
  progress (a full read, ~625 completion tokens) buys nothing on the
  same-turn retry — no partial-turn caching across retries.

**Ramp status:** 0 banked for GLM after cycle 1 (Kimi many, DeepSeek 1@B).
Cycle 2 retries Phase B on the same file; if `:together` flakes again,
switch to `fireworks-ai`.

### Cycle 1 RETRY — 2026-08-14 — GLM banks its first deliverable (grade A)

`:together` was healthy this time — SUCCESS on attempt 1, no retry, no
provider fallback needed. `reviews/glm-cycle-1.md`: 12 ranked, line-cited
proposals across estimate accuracy, budget policy, compaction. Commit
`4feb13c` (Kate verified), pushed. Probes reconfirmed: reasoning on
`message.reasoning`, `reasoning_effort low` ~19% shrink + tight-budget
hazard real, tool calls intact, `cached_tokens` present.

- **Citation accuracy: 9/9 EXACT (spot-checked, incl. back half — lines
  16/36/102/123/166 all verbatim). Zero drift — the CLEANEST of the three
  models.** (DeepSeek cycle 1 drifted 30-40 lines in its back half; this
  is the discipline DeepSeek lacks.)
- kitsu-usage 44,501 / 22,370 / 66,871. cap-1 unexercised — no spawn
  attempted. GLM self-corrected a flagged "raw chat-template control
  marker" write within the same turn (a positive kitsu disclosure
  pattern, not a weakness).

**Three-way comparison on the identical context.rs task:**
- Kimi cycle 4 (A): 5 items, tighter/architectural, explicit per-proposal
  risk framing.
- DeepSeek cycle 1 (B): 8 items, caught a real gap the others missed
  (`reasoning_content` ignored) but citations drifted badly.
- **GLM cycle 1 (A): 12 items, cleanest citations (9/9), independently
  converged on Kimi's P0 anchor-fragility finding PLUS a distinct
  CJK/byte-vs-char estimate bug (which is exactly what Kimi later shipped
  as P4).** GLM matches Kimi's grade on its first banked cycle.

**kitsu weakness filed:** CLI-surface friction — `--allow-path`/`--allow-write`
belong to the `chat` subcommand only; three supervisors have now tripped
on placing them top-level (already roadmap R4.7).

**Ramp status:** GLM 1-for-1 once transport cooperated (F on the
transport-killed first attempt doesn't count against the model). **Grade
A debut — GLM has reached Kimi-cycle-4 review level.** Next cycle: a
second review OR begin the implementation ramp (a fresh P-item Kimi
hasn't taken — P7/P10/P11), applying the same recipe.

### Cycle 2 — 2026-08-14 — second review confirms consistency (grade A) → GRADUATES

Fresh file — the provider usage/caching path in `openai_compat.rs`.
Success on `:together` attempt 1 (3 ticks healthy now — cycle 1's
transport failure was a one-off). `reviews/glm-cycle-2.md`: 4 ranked,
line-cited proposals (cached_tokens surfacing, context-length-cache
efficiency, tool-schema fast-fail, empty-choice guard). Commit `3870a9c`
(Kate verified), pushed. cap-1 unexercised.

- **Citation accuracy: 5/5 EXACT — zero drift, holding cycle 1's perfect
  record two cycles running.** No substantively-wrong proposals; one
  claim about a `ProviderError` variant was correctly HEDGED rather than
  guessed (the variant does exist — good caution).
- Read discipline held: 5 ranged reads of the target + 2 disclosed
  grounding reads of `types.rs` (7 total, one over the ~5 guidance but
  transparently reasoned, not silent overreach).
- **Third instance of GLM independently converging on real, unclaimed
  issues:** two of its four proposals (cached_tokens, context-length
  caching) duplicate still-open items in Kimi's backlog — found from
  scratch.

**GLM is 2-for-2 at grade A with a perfect citation record. It has
matched Kimi's review quality. GRADUATING to the implementation ramp.**
Cycle 3 = GLM's first CODE cycle: implement **P10** (cache the parsed
per-model context length instead of re-parsing the catalog JSON per
call) in `openai_compat.rs` — which is exactly the context-length-cache
efficiency fix GLM proposed in this very cycle (implementing its own
finding, as Kimi did with P3). Widen `--allow-write` to `kitsu-providers`
for that cycle; apply the full write-first recipe + workspace-wide
verification.
