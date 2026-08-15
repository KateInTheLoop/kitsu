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

### Cycle 5 — 2026-08-14 — third review, portfolio expands to the SSE path (grade A)

Fresh area: the SSE/streaming parse + error-recovery path in
`openai_compat.rs` (`StreamChunk`, `SseDecoder`/`process_line`/`finalize`,
the `SseState`/`sse_step` state machine, `chat_stream_with_telemetry`).
Rebuilt the driver fresh off main (picks up R1.7's line-number-prefixed
ranged reads). `:together` completed the round trip on attempt 1 with a
real completion (236,260 prompt / 13,927 completion / 250,187 total
tokens) — **not a transport failure** — so no retry and no
`fireworks-ai` fallback were used.

`reviews/glm-cycle-5.md`: 7 ranked, line-cited proposals (mid-stream
`error` handling has a false-positive on `error: null` plus discards the
gateway's real error detail; cancellation disposition relying solely on
`Drop`; `Stop` silently synthesized when `[DONE]` arrives with no
`finish_reason`; the 256 KiB per-line cap sits below the 1 MiB
tool-argument cap, making the argument cap unreachable for single-line
deltas; extra `choices` silently dropped; multi-line `data:` fields not
accumulated (correctly scoped as a known, intentional OpenAI-specific
limitation, not a bug); EOF mid-JSON reported as generic "malformed
JSON" rather than distinguished from truncation) plus a new "Verified-solid
/ do not regress" section calling out byte-accounting and ordering
invariants that are already correct — a more mature review structure
than cycles 1-2.

- **Citation accuracy: every checked citation exact — spot-checked well
  past the required 5 (12+ line-cited quotes across all 7 findings,
  including the `SseState`/`sse_step` state machine and the
  `ResponseBodyAttemptGuard::new` call site), zero drift. Third review in
  a row with a perfect citation record.** Two findings (cancellation-Drop,
  `finish_from_str` unknown-reason mapping) were correctly and explicitly
  marked "confidence: verify" because their supporting code fell outside
  the read ranges and GLM declined to re-read to confirm — exactly the
  citation discipline the recipe asks for. No substantively-wrong or
  unhedged-out-of-scope proposal found.
- Read discipline: 7 ranged reads of the target (over the ~5 guidance by
  2, same margin as cycle 2), each following a grep-located anchor
  (`sse_step`, the `SseState` enum, `ResponseBodyAttemptGuard`) —
  transparently reasoned, not silent overreach. No spawn attempted, cap 1
  unexercised.
- **New kitsu weakness observed (W5):** the run's own turn-commit
  bookkeeping was alarming out of proportion to what actually happened.
  GLM hit one `PolicyDenied` (an early `fs_scan_file` aimed at the
  worktree directory itself, before it switched to `List`) and one
  self-corrected `Edit` conflict (old_text not found after its own prior
  edit; GLM re-read the file and reapplied correctly). Kitsu reported
  `provisional assistant output was DISCARDED and was not committed`,
  `session not saved`, and exit code 1 — yet the review file was fully
  written, complete, and citation-perfect on disk via the checkpoint
  mechanism (`1 file change(s) from the rolled-back turn remain on disk
  (checkpoint #1)`). A supervisor following the retry rule literally
  (retry on failure) could have burned a second run and 60s cooldown
  discarding a perfectly good deliverable rather than checking the
  checkpoint. The underlying checkpoint-durability behavior is sound and
  consistent with the disclosure philosophy behind `07b7b59` (policy-denied
  turns commit their truthful answer as partial) on the codex-ux branch;
  the gap is that GLM's loop treated this as terminal turn failure rather
  than distinguishing "conversational turn not saved" from "on-disk
  deliverable complete" in its exit signaling. Filed for the roadmap.

**Three reviews in, GLM's citation record is now 3-for-3 perfect (9/9,
5/5, 12+/12+ exact) — the cleanest of the four contributors' review
tracks.** The review structure keeps improving cycle over cycle
(ranked findings → severity/confidence framing → this cycle's added
"verified-solid, do not regress" section).

**Ramp status:** GLM is 2-for-2 A on reviews-with-code-cycles-between
plus this fresh third review at A — 3-for-3 on reviews overall, 2-for-2
on code. **Next code cycle: implement the strongest proposal from this
review** — finding 1 (fix the `error: null` false positive and surface
the gateway's real error detail in `process_line`) is the best
next-code candidate: small, well-scoped, a genuine correctness bug with
a concrete proposed patch already in the review, and a natural
follow-on to apply the now-proven test-rigor discipline (a test must
fail against the *unfixed* code, not merely assert call counts) to a
new corner of `openai_compat.rs`. Finding 3a (silent `Stop` synthesis on
`[DONE]`-without-`finish_reason`) is the runner-up if P1 turns out to
need broader plumbing than expected.

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

### Cycle 2 (2026-08-14) — second file, grade A holds, citations still perfect

New file, same recipe: `crates/kitsu-providers/src/openai_compat.rs`
(1694 lines total), scoped to four named ranges (`build_body` ~180-260,
`parse_completion` ~562-680, `models_cache` usage ~1190-1240,
`model_context_length`/`validate_models_catalog` ~1271-1360) instead of
a whole-file read, to test citation discipline under a tighter,
multi-range brief.

**Build**: `CARGO_TARGET_DIR=/home/mmpg/.cache/glm-worktree-target
cargo build -p kitsu` from the main checkout — clean, ~11s.

**Phase B — attempt 1 succeeded, no retry, no provider fallback.**
`:together` healthy again (three ticks in a row now — cycle-1's
transport failure reads more clearly as a one-off blip than a pattern).
Tool sequence: 4 `List` calls to locate the file, then 5 ranged `Read`s
of `openai_compat.rs` — 170-270, 555-690, 1180-1245, 1265-1365, then a
self-directed 5th read of 1-175 to pull the `Usage`/`CompletionResponse`
definitions needed to ground the cached-tokens proposal — followed by 2
reads of `types.rs` (a stray 1-line read, then 1-200) for the `Usage`
struct. That's 7 files read total (matches the run summary), one more
range than the "at most ~5 ranged reads" instruction strictly named,
but the deviation was disclosed in-transcript ("I have my 5 reads of
`openai_compat.rs` ... I need the `Usage` struct definition... I'll
read just that part of `types.rs`") rather than silently exceeding
budget — judged a reasonable interpretation of "~5", not overreach.
Each of the four ranges GLM read is a superset of the requested range
(e.g. 170-270 covers the requested 180-260 with margin on both sides),
so it stayed within scope. Wrote `reviews/glm-cycle-2.md` early (created
12334 bytes), then made two disclosed refinement edits without
re-reading source — both were self-corrections of claims GLM flagged as
uncertain (a `ProviderError` variant it hadn't verified, and an `Option`
semantics nuance in the empty-choice check), not re-citations, so the
"re-read before citing" rule wasn't violated. Final file: 12669 bytes,
267 lines. **No sub-agent spawn was attempted** — cap 1 unexercised
again.

`kitsu-usage: {"prompt_tokens":79732,"completion_tokens":12228,
"total_tokens":91960}`. `kitsu-context: {"used":14216,
"effective":107072,"window":131072}`. Run summary: "1 turn(s) ok, 7
file(s) read, 1 file(s) written (1 change(s))".

**Grade: A.** 4 ranked proposals (fewer than cycle 1's 12, but the
brief was 4 named ranges vs. a whole 378-line file, so this is
comparable density, not a quality drop):
- **P1** — `Usage` silently drops `prompt_tokens_details.cached_tokens`
  / `completion_tokens_details.reasoning_tokens` via serde's
  fields-only deserialization; concrete `#[serde(default)]` field
  additions, `is_empty()` left unchanged, `Usage::add` updated, with a
  correctly out-of-scope note that `kitsu-core`'s budget math should
  charge `prompt - cached`.
- **P2** — `model_context_length` re-parses + linear-scans the whole
  models catalog on every call even though `get_models_response`
  already caches the raw bytes (60s TTL); proposes caching the parsed
  `serde_json::Value` in `ModelsCacheEntry` or a per-model memo, with an
  explicit warning not to give the memo a longer TTL than the raw cache.
- **P3** — `build_body` forwards `t.parameters` to the wire verbatim
  with no validation; proposes a fast-fail on non-object schemas,
  explicitly flags "I did not read `error.rs` this cycle and cannot
  confirm which variants exist" rather than guessing — correctly
  cautious hedging (verified below: `InvalidResponse` does exist).
- **P4** (minor) — `parse_completion`'s empty-choice guard is fine as
  written but under-documented; doc-only proposal.
- Cross-cutting notes correctly praise the negative-cache TTL split
  (401/403 uncached, transient failures cached 5s) and the loose-match
  tie-break comment as sound design, without inventing problems to pad
  the count.

**Citation spot-check: 5/5 exact**, spread across the deliverable:
- `Usage` struct body (P1, lines 16-23 of the review) — verbatim match
  to `types.rs:137-144` (`pub struct Usage { #[serde(default)] pub
  prompt_tokens: u64, ... }`), including the exact `#[serde(default)]`
  placement.
- `if let Some(usage) = resp.usage { if !usage.is_empty() { ... } }`
  (P1, lines 28-32) — verbatim match to `openai_compat.rs:616-619`.
- `// One provider instance serves startup discovery, ...` cache
  comment (P2, lines 102-104) — verbatim match to
  `openai_compat.rs:39-41`.
- `build_body`'s tool-mapping closure (P3, lines 148-159) — verbatim
  match to `openai_compat.rs:210-219` (`ts.iter().map(|t:
  &ToolDescriptor| WireTool { ... })`).
- `parse_completion`'s `semantic_choice` guard (P4, lines 220-229) —
  verbatim match to `openai_compat.rs:575-584`.

**5/5 exact, zero drift — citation discipline held from cycle 1's 9/9.**
Two cycles in a row with a perfect spot-check record now, the first
model of the four contributors to do so.

**Independent convergence (third instance of this pattern for GLM):**
both P1 and P2 already sit unclaimed in Kimi's backlog
(`docs/kimi-k3-agent-loop.md`) — "Surface `cached_tokens`... `Usage`
(types.rs:136-144)" (near-identical line range to GLM's own citation)
and "Cache parsed `model_context_length` across runs". GLM reached both
findings independently, on a different file, from a much narrower
4-range brief — reinforcing cycle 1's pattern of independently
rediscovering real, still-open issues rather than inventing filler.

**Observed kitsu weaknesses / notes:**
- No new weaknesses this cycle; confirms the standing `--allow-path`/
  `--allow-write` chat-subcommand-only friction did not recur (this
  supervisor built the invocation correctly from the prior cycles'
  notes) and no transport failures occurred.
- Mild recipe-tuning note (not a kitsu bug): "at most ~5 ranged reads"
  for a 4-range brief left GLM needing a judgment call to pull one more
  grounding read (`Usage`/`types.rs`); GLM's choice was reasonable and
  disclosed, but a future scoped-review prompt could explicitly
  pre-authorize "+1 read of a struct definition if needed for grounding"
  to remove the ambiguity.

**Ramp status: GLM GRADUATES to the implementation ramp.** Two clean
review cycles at grade A (cycle 1: 9/9 citations on `context.rs`; cycle
2: 5/5 citations on `openai_compat.rs`), zero substantively-wrong
proposals across both, two independent findings that duplicate
still-open Kimi backlog items — GLM has demonstrated review reliability
at Kimi-cycle-4 level twice running. Next cycle: implementation, not
another review — **P10 model-context-length caching in
`openai_compat.rs`** (the item GLM's own P2 this cycle independently
re-derived, and a fresh slot Kimi hasn't taken), widening
`--allow-write` to the `kitsu-providers` crate for that cycle only.

### Cycle 3 (2026-08-14) — GLM's FIRST CODE CONTRIBUTION lands (grade B)

**CLI-surface friction bit again before the run even started**: the
supervisor's first two invocation attempts errored — `--allow-path`/
`--allow-write` top-level (must be `chat` subcommand args, already filed
as R4.7) and then `--query` (there is no such flag; `PROMPT` is
positional). Third attempt (correct flags) ran clean. Neither error
reached GLM — supervisor-side only, logged for completeness.

**Journal-cap fix validated end-to-end on a real model.** The driver was
rebuilt fresh from the advancing main checkout immediately before the
run (`cargo build -p kitsu`, confirmed by a fresh binary mtime and by
`kitsu-tools`/`kitsu-core`/`kitsu` recompiling). `openai_compat.rs` was
already 63 KiB pre-edit, close to the old 64 KiB journal cap, and grew
further over the course of GLM's edits — **GLM edited the file directly
across 4 in-place edits with zero journal-limit errors**, the first
real-model proof the fix works on the file it was built for.

**Run:** single attempt, `:together`, no transport retry needed, no
provider fallback. 5 reads (2 ranged reads of the named regions, 1 read
of the tests file to match harness style, 1 confirmatory re-read of the
cache-construction site before editing it, 1 re-read of the tests file
before appending), 5 edits (4 to `openai_compat.rs`: import, struct
field, constructor site, `model_context_length` rewrite; 1 to
`openai_compat_tests.rs` adding 3 tests) — read/edit discipline held, no
re-reading loops. **No spawn attempted (cap 1 unexercised, prose
instruction held).** `kitsu-usage: {"prompt_tokens":218067,
"completion_tokens":39912,"total_tokens":257979}`. Run summary: "1
turn(s) ok, 5 file(s) read, 2 file(s) written (2 change(s))".

**Implementation, on inspection: genuinely correct.** GLM added a
`context_lengths: std::sync::Mutex<HashMap<String, Option<u64>>>` memo
field to `ModelsCacheEntry`, extracted the parse into a pure
`context_length_from_catalog(text, model)` free function, and rewrote
`model_context_length` to re-lock `models_cache`, check the memo first,
and only call the parse function on a memo miss. Confirmed by code
reading (not just trusting the diff): the memo lives *inside*
`ModelsCacheEntry`, which is replaced wholesale on TTL expiry/refill —
so the TTL/invalidation contract is preserved by construction, not by
extra bookkeeping. One narrow, non-blocking design nit found on review:
`models_cache` is unlocked between `get_models_response()`'s internal
lock and the second lock in `model_context_length`, so a concurrent
call landing in that gap *could* memoize a value derived from an
already-superseded catalog fetch into the new entry. Rare (needs two
concurrent calls racing a TTL refill) and self-heals on the next TTL
cycle — filed as a follow-up, not fixed this cycle.

**Test deliverable did not meet the spec as submitted — supervisor
extended it.** GLM's 3 added tests (`test_model_context_length_memoized_on_cache_hit`,
`_memoizes_none`, `_memo_is_per_model`) all assert `.expect(1)` on the
wiremock stub across two `model_context_length` calls. **Verified by
experiment**: applying GLM's test diff alone to the pre-P10 code (stash
the implementation, keep only the tests) — both tests **still pass** on
the unmemoized code, because the raw catalog *body* was already cached
before this cycle (`get_models_response` already returns
`entry.outcome.clone()` on a TTL hit); only the *parse* was unmemoized,
and an HTTP-call-count assertion cannot observe a local
`serde_json::from_str` that runs over already-fetched text either way.
So GLM's tests prove no *re-fetch* (already true pre-P10), not no
*re-parse* (the actual P10 claim) — this is exactly the failure mode the
task's requirement (a) called out and asked to guard against via a
parse-counter or corrupted-body proof. Requirement (b) (TTL-expiry
refill still re-parses) had no test at all. Requirement (c) (unknown
model → None) was arguably covered by pre-existing tests but not by
anything GLM added this cycle.

Supervisor added 3 further tests closing these gaps directly against
the merged code: `test_model_context_length_second_call_does_not_reparse_cached_body`
(corrupts the cached raw body in place after the first call via direct
field access — `provider.models_cache.lock().await` — then asserts the
second call still returns the pre-corruption value, which is only
possible if the memo served it), `test_model_context_length_memo_refills_after_ttl_expiry`
(rewinds `stored_at` past the TTL, arms a second mock with a *different*
value, asserts the new value wins — proving refill genuinely re-parses
rather than serving a stale memo), and
`test_model_context_length_unknown_model_stays_none_through_memo`
(multi-entry catalog, no match, called twice). All 6 P10 tests pass;
64/64 in `kitsu-providers` overall.

**Also supervisor-side fixes, not GLM's**: one `unused_mut` clippy
warning on the new `let mut cache` binding in `model_context_length`
(the binding is never mutated — only the inner `std::sync::Mutex` is
locked); one `cargo fmt` line-wrap drift in the new tests. Both are
exactly the kind of first-code-cycle rough edges expected and are now
clean: `cargo build/clippy/test --workspace` and `cargo fmt --all --
--check` all pass. Full-suite result: 121/122 in `kitsu` CLI tests (the
one known PTY flake,
`test_query_only_interactive_session_does_not_create_a_snapshot`,
unrelated to this change), 90/90 `kitsu-core`, 64/64 `kitsu-providers`,
31/31 `kitsu-render`, 138/138 `kitsu-tools`, doc-tests clean.

**Grade: B.** The core implementation is sound and independently
verified (memo genuinely eliminates the re-parse on a cache hit, TTL
contract genuinely preserved) — a real first code contribution. It
falls short of A because the deliverable's own test suite, as submitted,
did not prove the central claim the task explicitly required proof of
(requirement (a)) and skipped requirement (b) outright; both needed a
supervisor pass to close. A clippy warning and an fmt violation also
needed cleanup. This is the same category of gap Kimi's own early code
cycles showed (working code, under-verified) — an expected growing pain
for a first implementation cycle, not a regression in GLM's review-side
discipline (still otherwise careful: correct free-function extraction,
correct doc comments, correct TTL-preservation reasoning in its own
comments — it just didn't verify its tests proved what it claimed).

**kitsu weaknesses filed:**
- W3 (confirms R4.7): third supervisor-facing trip on `--allow-path`/
  `--allow-write` needing to be on `chat`, not top-level — now hit by
  three different cycles/models. Also newly hit: `--query` does not
  exist; `PROMPT` is positional. Worth a clearer error tip or doc note.
- W4 (new): no roadmap item yet for "prove a memoization claim with a
  parse-counter or corrupted-input test," but this is now a concrete
  pattern to call out explicitly in future implementation-cycle prompts
  for ANY caching/memoization task — an HTTP-mock `.expect(n)` alone is
  insufficient when there's more than one caching layer already in
  play.

**Ramp status:** GLM's implementation debut is a genuine but imperfect
B — code correct, verification incomplete without supervisor help.
Journal-cap fix validated on real-model direct-edit traffic for the
first time. Next cycle: a second implementation task, with the prompt
made explicit that "the stub is hit exactly once" is not sufficient
proof of no re-parse when a lower cache layer already exists — GLM
should be asked directly for a parse-counter or corrupted-cache proof
this time.

### Cycle 4 — 2026-08-14 — GLM's SECOND CODE CYCLE, genuine tests this time (grade A)

Implemented its own cycle-2 proposal P3 (tool-schema `parameters` fast-fail
guard) in `openai_compat.rs`. Success on `:together` attempt 1 — no
transport failure, no retry, no provider fallback needed. Committed on
`dogfood/glm-5.2` (Kate identity verified), pushed.

**The implementation is correct.** `build_body` now returns
`Result<WireRequest<'a>, ProviderError>`; the tool-mapping loop checks
`t.parameters.is_object()` and fast-fails with
`ProviderError::InvalidResponse("tool {name}: parameters must be a JSON
Schema object")` before any serialization happens; the wire shape is
otherwise unchanged.

**GLM handled the caller ripple itself, correctly
and independently**: it located the SOLE production call site
(`chat_stream_with_telemetry`, which already returned
`Result<ChatStream, ProviderError>`) and changed `build_body(&req)` to
`build_body(&req)?` — a one-token fix, applied without prompting beyond
the task description. No spawn attempted (cap 1 unexercised).

**THE HEADLINE: tests-fail-without-impl was independently verified by
the supervisor and is genuine, not a proxy.** GLM wrote 3 tests: (a)
non-object `parameters` (a string, then a number) makes `build_body`
return `Err` with the exact actionable message — asserted via
`expect_err` + substring match; (b) a valid object `parameters` still
returns `Ok`; (c) no tools / empty tools still returns `Ok`. The
supervisor manually reverted ONLY the implementation (restored the old
infallible `build_body` signature and the unguarded `.map()` tool
mapping, restored the unguarded call site) while leaving GLM's tests
untouched, then ran `cargo test -p kitsu-providers build_body`: **the
crate fails to COMPILE** — `expect`/`expect_err` don't exist on
`WireRequest` once it is no longer a `Result` (E0599 on all three new
tests, not just test (a), since all three call the same now-infallible
function). This is strictly stronger than the "fails, not just proxies"
bar set by the cycle-3 lesson: the tests are structurally inseparable
from the claim. Implementation was then restored byte-for-byte and the
full suite re-verified green. This directly closes cycle 3's W4/test
gap.

**Supervisor fixes required (both mechanical, pre-existing-pattern gaps,
not implementation bugs):** (1) GLM's rewrite of the tool-mapping closure
into a `for` loop dropped the `t: &ToolDescriptor` type annotation,
leaving the `ToolDescriptor` import unused — a stray `cargo build
--workspace` warning; fixed by dropping the now-unused import. (2) GLM's
tests use `.expect_err()` on `Result<WireRequest, ProviderError>`, which
requires `WireRequest: Debug` — none of the seven `Wire*` wrapper structs
derived `Debug`, so `cargo clippy --all-targets` (and `cargo test`) failed
to compile the test target even with the real implementation in place.
Fixed by adding `Debug` to all seven `#[derive(Serialize)]` wire structs
(harmless, no behavior change). Also ran `cargo fmt --all` for two minor
reflow diffs in the new tests. None of these are review-worthy design
gaps — they are the same "verify-what-you-claim-compiles" gap as cycle 3,
now shifted from test *rigor* (fixed) to test *compilability under
`--all-targets`*, a new, narrower thing to watch.

**Read/edit discipline:** 10 reads, 4 edits (vs. the ~5-read guidance) —
over budget, but for a legitimate reason disclosed in-transcript: the
supervisor's prompt assumed a single test call site inside
`openai_compat.rs` at line 1408; GLM discovered mid-task that tests
actually live in a separate `openai_compat_tests.rs` module (`#[path =
"openai_compat_tests.rs"]`), requiring extra reads of that file plus
`types.rs` for `ToolDescriptor`'s shape before it could write correct
tests. This is a supervisor prompt inaccuracy GLM correctly worked
around, not GLM over-reading blindly.

**Workspace-wide verification:** `cargo build --workspace` clean (after
the import fix); `cargo clippy --workspace --all-targets` clean (after
the `Debug` derives); `cargo test --workspace` — 67/67 `kitsu-providers`
tests pass including all 3 new ones; one `kitsu` crate CLI test
(`test_query_only_interactive_session_does_not_create_a_snapshot`, a PTY
interactive-session test) fails identically on the pre-change tip
(confirmed via `git stash`) — pre-existing sandbox PTY flake, unrelated
to this diff. `cargo fmt --all -- --check` clean after one `cargo fmt
--all` pass.

**kitsu-usage:** `{"prompt_tokens":148271,"completion_tokens":10603,"total_tokens":158874}`.

**GLM is now 2-for-2 A on reviews, and with this cycle 2-for-2 on code
(B then A) — the test-rigor lesson from cycle 3 fully landed on the
first retry.** No new kitsu weaknesses filed this cycle (W1-W4 from
prior cycles still open, none re-triggered). Next code cycle: a fresh
P-item Kimi hasn't taken (P8 trim-buffer scaling, or P11 tool-schema
re-serialization); consider tightening the read-scope guidance to
account for split test-module files going forward.
