# GLM-5.2 worker loop — kitsu drives a fourth hosted frontier model

> **LIVE 2026-08-14.** A working HF token (Inference-Providers scope) is
> at `~/.cache/kitsu-hf-token`; each tick reads it there and probes once
> — on 401/402/403 or missing file the tick ends silently. Probe
> confirmed `zai-org/GLM-5.2:together` returns a structurally-intact
> forced tool call, hidden reasoning under the key **`reasoning`**,
> `--reasoning-effort low` honored (partial shrink — 143-188 reasoning
> tokens vs ~199 uncapped on a 4-word prompt; not the dramatic cut
> the spec-creation spot-check implied), and
> `prompt_tokens_details.cached_tokens` present (key always present,
> value 0 in every sample so far — no cache hits observed).
> **Cycle 1 status: probes green, kitsu run RED (transport), then
> RETRIED GREEN.** Cycle 1 burned both attempts on two straight
> `:together` transport failures before any file review completed.
> The retry tick found `:together` healthy, produced
> `reviews/glm-cycle-1.md` on attempt 1 with zero transport errors, and
> banked it at **grade A** with a perfect 5/5 (spot-checked further to
> 9/9) citation-accuracy record — the cleanest citation discipline of
> the three non-Anthropic contributors so far on this exact
> `context.rs` task. Ramp: 1 review banked for GLM-5.2.

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

### Cycle 1 (2026-08-14) — probes green, kitsu run RED, no deliverable banked

**Phase A — router probes (curl, direct against
`https://router.huggingface.co/v1/chat/completions`, model
`zai-org/GLM-5.2:together`):**

1. **Reasoning key + `reasoning_effort: low` shrink.** Hidden reasoning
   rides `choices[0].message.reasoning` (and, in streaming, per-chunk
   `delta.reasoning`). On an identical 4-word prompt ("What is 2+2?
   Answer in one word."): uncapped → 199 reasoning tokens (203
   completion total); `reasoning_effort: "low"` → 143 and 184 reasoning
   tokens across two repeats (147 and 188 completion total). So `low`
   *does* shrink reasoning, but only ~10-30%, not the near-elimination
   the spec-creation spot-check's "26 reasoning tokens" figure implied
   — that number does not reproduce today. Treat `reasoning_effort low`
   as a moderate cost lever, not a hard cap; keep `--max-output-tokens`
   generous regardless (confirmed separately: at `max_tokens: 100` with
   `reasoning_effort low`, GLM burned the entire 100-token budget on
   reasoning and returned zero content, `finish_reason: "length"` —
   the inherited-recipe warning about tight budgets is very real).
2. **Forced tool call.** `tool_choice: "required"` against a single
   `get_weather` function returned a well-formed call:
   `finish_reason: "tool_calls"`, one entry with a populated
   `id` (`call_01a002f05aa87a13acd6ec4f`), `type: "function"`, correct
   `function.name`, and valid-JSON `function.arguments`
   (`{"location":"Paris, France"}`). `:together` tool-call shape intact.
3. **Usage shape.** `usage.prompt_tokens_details.cached_tokens` is
   present on every response observed (3 samples), always `0` — no
   evidence of provider-side caching kicking in on these short,
   non-repeated prompts. `completion_tokens_details.reasoning_tokens`
   is also always present.

**Phase B — kitsu run.** Built clean:
`CARGO_TARGET_DIR=/home/mmpg/.cache/glm-worktree-target cargo build -p
kitsu` (25s, no warnings of note) from the main checkout. Ran
`kitsu chat` (not a bare top-level `--allow-path`/`--prompt` — those
flags live on the `chat` subcommand, not the top level; the first
invocation attempt errored on that before the real run) with the full
Rules-mandated flag set, prompting GLM to read
`crates/kitsu-core/src/context.rs` alone and write
`reviews/glm-cycle-1.md`.

- **Attempt 1**: GLM issued one `Read` tool call (full 378-line file,
  0ms) and began composing a response ("I'll start by reading the
  entire file once to understand its structure."), then the gateway
  returned **HTTP 503** three times in a row before kitsu gave up:
  `kitsu-usage: {"prompt_tokens":3116,"completion_tokens":625,
  "total_tokens":3741}`, `kitsu-context: {"used":7015,
  "effective":107072,"window":131072}`. The provisional assistant
  output was discarded (turn never committed) per kitsu's
  fail-closed behavior — correct, but it means the 625 completion
  tokens bought nothing.
- **60s synchronous cooldown**, then **Attempt 2 (the one permitted
  retry)**: GLM again issued one `Read` (full file), then kitsu logged
  `inference provider attempt 1/3: invalid response; HTTP 200; body
  failed; terminal=true` — the provider returned 200 but kitsu could
  not parse the body. `kitsu-usage: {"prompt_tokens":3116,
  "completion_tokens":69,"total_tokens":3185}`,
  `kitsu-context: {"used":6998,"effective":107072,"window":131072}`.
  Error surfaced to the user: "chat request received an invalid
  provider response - verify the gateway flavor and API
  compatibility."
- Retry budget (Rules: one retry, 60s cooldown) was now exhausted.
  **No third attempt was made.** `reviews/glm-cycle-1.md` was never
  written — `ls reviews/` after both attempts shows the directory
  still empty.
- Diagnostic follow-up (curl, not a kitsu run, to avoid spending
  budget): re-checked whether kitsu's streaming parser mishandles the
  common OpenAI-compatible "trailing usage-only chunk with empty
  `choices: []`" shape that `:together` emits at stream end — grepped
  `crates/kitsu-providers/src/openai_compat.rs` and
  `openai_compat_tests.rs`; that shape is explicitly handled and
  covered by an existing test (`openai_compat_tests.rs:837-841`,
  "Final chunk carries usage with empty choices"). So the attempt-2
  failure is **not** that known-good code path; root cause is most
  likely transient `:together` backend flakiness (two different
  failure modes — 503, then a malformed/truncated 200 body — within
  ~2 minutes of each other) rather than a confirmed kitsu parsing bug.
  Flagged as unresolved, not closed.

**No sub-agent spawn was attempted** — GLM never got past the initial
file read in either attempt (agent-spawn-cap 1 was never exercised).

**Grade: F (no deliverable).** Cannot run the mandated 5-citation
spot-check because there is no `reviews/glm-cycle-1.md` to check.

**Three-way comparison note:** Kimi cycle 4 (`reviews/kimi-cycle-4.md`,
grade A) and DeepSeek cycle 1 (`reviews/deepseek-cycle-1.md`, grade B)
both banked a real deliverable on this exact task on their first
attempt. GLM cycle 1 is the first of the four contributors to fail to
produce anything — not because of prompting or model reasoning quality
(GLM never got a chance to write; it was still on its first `Read`
both times) but because of provider-transport instability on
`:together` specifically. This is a **different failure class** from
Kimi's early trailing-tool-call corruption (which was semantic/format,
fixed by pinning `:together`) and from DeepSeek's early over-reading
(prompting fix). GLM's problem, on this evidence, sits one layer lower
in the stack. Re-evaluating the provider pin (per the inherited
recipe's own "re-evaluate in cycle 1 if it misbehaves" clause) is
warranted before cycle 2 if the pattern repeats — candidates from the
spec's observed-providers list: novita, fireworks-ai, zai-org.

**Observed kitsu weaknesses / notes (repro included):**
- kitsu's fail-closed discard-on-error behavior is correct (no
  half-written turn got committed to history), but it means a
  mid-turn transport failure after real tool-call progress (one full
  378-line read, ~600-700 completion tokens spent) is a complete
  write-off — no partial credit, no checkpoint to resume from. Not a
  bug, but a cost-efficiency gap worth a future roadmap item: could
  kitsu cache the tool-call results already gathered in a turn across
  a same-turn retry, instead of re-issuing the identical `Read` from
  scratch on attempt 2?
- Minor supervisor-side friction (not a kitsu bug): the CLI surface
  puts `--allow-path`/`--allow-write`/the prompt positional under the
  `chat` subcommand only, not the top-level `kitsu` binary — an
  invocation built from `kitsu --gateway ... --prompt "..."` at the
  top level fails fast with a clear `unexpected argument` usage error
  (good error message, just a supervisor-authoring pitfall worth
  noting in the spec's recipe for future cycles).
- The "invalid response; HTTP 200; body failed" error message names
  the symptom but not which parse step failed or how many bytes/chunks
  were received before the failure — a next-token diagnostic (e.g.
  "N chunks parsed OK, chunk N+1 failed: <reason>") would have let this
  supervisor confirm or rule out the empty-choices-chunk hypothesis
  directly instead of via source inspection.

**Ramp status:** still 0 reviews banked for GLM-5.2 (Kimi: many;
DeepSeek: 1 at grade B). Cycle 2 should retry Phase B with the same
scope; if `:together` fails a third consecutive time across cycles,
switch the pin per the recipe's re-evaluation clause.

### Cycle 1-retry (2026-08-14) — GLM debut BANKED, grade A, citations perfect

**Phase A — router probes re-confirmed (curl, same endpoint, same
model):**

1. **Reasoning key + shrink.** `reasoning` key confirmed again (both
   with and without `reasoning_effort`). On a "17 * 24, think step by
   step" prompt: uncapped → 205 reasoning tokens (300-token budget
   fully consumed, `finish_reason: "length"`, empty visible answer);
   `reasoning_effort: "low"` → 166 reasoning tokens (finishes normally,
   content present). ~19% shrink — a moderate lever, consistent with
   cycle 1's "10-30%, not dramatic" finding; the earlier "26 reasoning
   tokens" spec-creation figure still does not reproduce. Confirms the
   recipe's tight-budget warning again: an uncapped reasoning stream
   can eat the whole `max_tokens` and return nothing.
2. **Forced tool call.** `tool_choice` pinned to `get_weather` returned
   a clean `finish_reason: "tool_calls"`, one well-formed call
   (`id: call_01a0031bfea17351a3903ba3`, valid JSON args
   `{"city":"Paris"}`). `:together` tool-call shape intact, second
   confirmation.
3. **Usage shape.** `prompt_tokens_details.cached_tokens` present on
   every response; tested a cold/warm repeat of an identical 75-token
   prompt 2s apart — still `0` both times (second call's
   `reasoning_tokens` dropped 150→32, suggesting the *model* varies
   effort a little between runs even at a fixed `reasoning_effort`,
   but no provider-side prompt cache hit was observed at this length).

**Phase B — kitsu run, attempt 1 succeeded, no retry needed.** Build
clean (`CARGO_TARGET_DIR=/home/mmpg/.cache/glm-worktree-target cargo
build -p kitsu`, ~9s, cached). Repeated the same CLI-surface fix noted
in cycle 1 (the flags live on `kitsu chat`, not the top-level binary).
`:together` was healthy this tick — **zero transport errors**, so the
provider-switch fallback (`fireworks-ai`) was never needed and the pin
stays at `:together`.

Tool sequence: `Read` (full 378-line file) → `Write`
`reviews/glm-cycle-1.md` (created, 6875 bytes — **kitsu flagged a
warning**: "content contains a raw model chat-template control marker
— the model likely emitted protocol text instead of real file
content") → `Read` (ranged 29-185, targeted re-verification pass) →
`Write` `reviews/glm-cycle-1.md` (updated, 10745 bytes, clean). GLM
noticed the flagged marker itself, said so in its own narration ("I
literally wrote `<|...|>`... I'll fix that"), re-read its citations,
and overwrote with a clean version — the final on-disk file is 10745
bytes with no control-token leakage. **This is kitsu's disclosure
mechanism working as designed**: it didn't silently accept a corrupted
write, and the model used the warning to self-correct within the same
turn rather than needing a retry.

`kitsu-usage: {"prompt_tokens":44501,"completion_tokens":22370,
"total_tokens":66871}` (completion tokens ran close to the 24000 cap
but did not truncate — finished cleanly). `kitsu-context:
{"used":15363,"effective":107072,"window":131072}`. Run summary: "1
turn(s) ok, 2 file(s) read, 1 file(s) written (1 change(s))". **No
sub-agent spawn was attempted** (`agent-spawn-cap 1` unexercised — GLM
never called a spawn tool; the prose "do NOT spawn sub-agents" was
followed without needing the cap as a backstop).

**Grade: A.** `reviews/glm-cycle-1.md` delivers 12 ranked proposals
(P0-1..P2-4) cleanly organized under the three requested axes —
estimate accuracy (P0-1 byte-vs-char undercounting on CJK, P0-2 missing
per-message overhead, P1-3 `tool_call_id` billed at the wrong rate,
P2-4 tail omits schema tokens), budget policy (P1-4 buffer decoupled
from reserve, P2-1 `None` max_output under-reserves, P2-2 zero-buffer
on tiny windows), and the compaction breaker (P0-3 length-only anchor
validity relying on caller `invalidate()` discipline, P1-1
`anchor_includes_schema` inherits the same fragility, P1-2 `estimate`
discards `had_schema` so schema asymmetry is invisible to it, P2-3
`record` accepts an unbounded `sent_len`). Each proposal names root
cause, exact line(s), and a concrete fix — matches the prompt's
required structure precisely.

**Citation spot-check (headline result): 5/5 exact**, deliberately
spread front-to-back including the back half:
- Line 16 cited as `` const CHARS_PER_TOKEN: usize = 4; `` — exact.
- Line 36 cited as `` let text = m.content.len() + m.tool_call_id.as_ref().map_or(0, String::len); `` — exact.
- Line 102 (middle) cited as `` Some((tokens, sent_len, _)) if sent_len <= messages.len() => { `` — exact.
- Line 123 (back half) cited as `` matches!(self.anchor, Some((_, sent_len, had_schema)) if had_schema && sent_len <= messages.len()) `` — exact.
- Line 166 (back half) cited as `` let buffer = (window / 10).min(2_048); `` — exact.

Beyond the mandated 5, spot-checking further citations (lines 68, 80,
86-87, 163, 167) found the same result: every quote matched the file
verbatim, no drift anywhere including the back half. This is the
strongest citation discipline observed across all three non-Anthropic
contributors so far on this exact task.

**Three-way comparison** (same file, same task, all three deliverables
now read in full):
- **Kimi cycle 4 (`reviews/kimi-cycle-4.md`, grade A, 5 proposals).**
  Tighter and more architectural — leads with the same core finding GLM
  found independently (P0-3: length-only anchor validity is the
  file's central weakness) but goes further, proposing a concrete
  fingerprint/generation-counter fix with an explicit risk/cost
  trade-off per proposal (a "Why it wins" + "Risk" structure GLM's
  review lacks). Kimi's citations, spot-checked in its own cycle,
  were accurate.
- **DeepSeek cycle 1 (`reviews/deepseek-cycle-1.md`, grade B, 8
  proposals).** Broader surface area (8 items vs GLM's 12, but
  DeepSeek's are coarser-grained) and one genuinely sharp catch GLM
  missed (Rank 5: `message_tokens` ignores `reasoning_content`/thinking
  tokens entirely — a real gap neither GLM nor Kimi's review flagged).
  But DeepSeek's back-half citations drift badly: "Lines 173-205" for
  `ContextBudget` when the struct+impl actually span 143-174 (~30-line
  drift), "line 203-204" for the buffer computation that is actually
  at line 166 (~38-line drift). Re-verified those two here directly
  against the file — confirmed drifted, matching the spec's prior note
  that DeepSeek cycle 1 drifted 30-40 lines in the back half.
- **GLM cycle 1-retry: most granular breakdown (12 items), and the
  cleanest citation record of the three** — a genuine strength on the
  exact axis the recipe's "citation discipline" rule was written to
  fix. Depth of insight sits between the two: GLM independently found
  the same P0 anchor-fragility issue Kimi led with, plus a distinct,
  well-reasoned CJK/byte-vs-char estimate bug neither Kimi nor
  DeepSeek raised. It did not catch DeepSeek's `reasoning_content`
  omission or match Kimi's per-proposal risk/cost framing — so on pure
  insight depth this reads as strong-A/comparable-to-Kimi rather than
  clearly superior, but on citation accuracy specifically it is the
  best of the three banked reviews.

**Observed kitsu weaknesses / notes:**
- Confirms cycle 1's non-bug note: the CLI puts `--allow-path` /
  `--allow-write` / the prompt positional on `kitsu chat`, not the
  top-level `kitsu` binary; a supervisor building the invocation from
  the top level fails fast with a clear `unexpected argument` error.
  Worth promoting from a cycle-1 footnote into the Rules section's
  gateway-flag line so future supervisors don't re-pay this cost.
- The chat-template-marker disclosure (described above) is worth
  filing as a positive pattern, not a weakness: kitsu could have
  silently accepted the corrupted write; instead it surfaced a
  specific, actionable warning inline in the tool-result stream, and
  the model used it productively in the same turn. No roadmap item
  needed — flagging so it isn't lost.

**Ramp status:** 1 review banked for GLM-5.2 at grade A
(`reviews/glm-cycle-1.md`) — GLM has now matched Kimi's day-one outcome
(banked at grade A) and exceeded DeepSeek's day-one outcome (grade B),
after one failed and one successful tick. Cycle 2 should move to a
fresh backlog item (implementation or a different file), per the
recipe's "never assign the same item to two contributors in the same
hour" rule — this slot is now spent for `context.rs`.
