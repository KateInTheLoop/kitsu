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

### Cycle 3 — 2026-08-14 — GLM's FIRST CODE CONTRIBUTION (grade B)

Implemented P10 (its own cycle-2 proposal). Commit `2c0c796` (Kate
verified), pushed. **Implementation genuinely correct:** a
`context_lengths: Mutex<HashMap<String, Option<u64>>>` memo inside
`ModelsCacheEntry`, parsing extracted into a pure
`context_length_from_catalog()`, `model_context_length` checks the memo
before parsing; the memo lives inside the cache entry so TTL
refill/invalidation replaces it wholesale — contract genuinely preserved.

- **JOURNAL-CAP FIX VALIDATED ON A REAL MODEL:** GLM edited
  `openai_compat.rs` (63 KiB, near the old 64 KiB cap) DIRECTLY across 4
  in-place edits with **zero journal-limit errors** — the first
  production confirmation of last cycle's R1.6 fix on the exact file
  class it was built for. 5 reads, 5 edits, no re-read loops, cap-1
  unexercised, no spawn.
- **Docked to B — superficial tests (new weakness).** GLM's 3 tests
  asserted HTTP-call-count (`.expect(1)` on a wiremock stub), which
  proves "no re-FETCH" (already true pre-P10) — NOT "no re-PARSE" (the
  actual P10 claim). The supervisor VERIFIED this empirically: stashed
  the impl, GLM's tests still passed against unmemoized code. Replaced
  with 3 genuine tests (corrupted-cache: mutate the cached body, assert
  the memo still returns the pre-corruption value; TTL-refill re-parses;
  unknown-model → None). All 6 pass. **Lesson for GLM's code ramp: a test
  must prove the actual claim, not a proxy for it** — its review-side
  rigor didn't fully transfer to test design yet.
- Narrow non-blocking race filed as follow-up (memo could inherit a value
  from a fetch superseded during a concurrent TTL refill).
- Weaknesses: W3 CLI friction (--allow-path/--allow-write under `chat`
  only, third hit — roadmap R4.7); W4 the superficial-test pattern above.

**GLM is a code contributor (first diff landed, correct design). It is
2-for-2 A on reviews + 1 code cycle at B (tests were the gap, not the
code).** Next code cycle: reinforce test-writing (prove the claim
directly), a fresh P-item Kimi hasn't taken (P8 trim-buffer scaling, or
P11 tool-schema re-serialization).

### Cycle 4 — 2026-08-14 — P3 fast-fail (grade A) — TEST-RIGOR GAP CLOSED

Implemented its own cycle-2 proposal P3: `build_body` now returns
`Result<WireRequest, ProviderError>` and FAST-FAILS on a non-object tool
`parameters` with an actionable message ("tool <name>: parameters must
be a JSON Schema object") before serialization. Commit `05e469a` (Kate
verified), pushed. Success on `:together` attempt 1, cap-1 held, no spawn.

- **Caller ripple handled BY GLM itself:** it located the sole production
  call site (`chat_stream_with_telemetry`) and changed `build_body(&req)`
  → `build_body(&req)?` unprompted, and found the real split test module
  (`openai_compat_tests.rs` via `#[path]`) the brief mis-pointed.
- **THE HEADLINE — genuine tests, independently verified.** 3 tests
  (non-object params → Err with exact message; valid object → Ok; no
  tools → Ok). The supervisor reverted ONLY the impl and the crate
  **fails to compile** against GLM's tests (expect_err doesn't exist once
  build_body is infallible) — the tests are structurally inseparable from
  the claim, the exact opposite of cycle 3's proxy tests. **The
  test-rigor lesson landed on the first retry.**
- Supervisor fixes were mechanical only (an unused import from GLM's
  for-loop rewrite; added `Debug` to the 7 Wire* structs so expect_err
  compiles) — not implementation bugs. kitsu-providers 67/67. 10 reads
  (over the ~5 guidance, but for a disclosed reason: the split test file).

**GLM is now 2-for-2 A on reviews AND 2-for-2 on code (B→A, test rigor
fixed). It has fully reached Kimi-level contribution — reviews AND
tested code.** Next: alternate — a fresh REVIEW of a new area (build its
portfolio, avoid Kimi's P9/P11) then implement from it; e.g. review the
SSE/streaming parse + error-recovery path in openai_compat.rs.

### Cycle 5 — 2026-08-14 — SSE/streaming review (grade A, 3rd perfect-citation review)

Reviewed the SSE/streaming parse + error-recovery path in
`openai_compat.rs` → reviews/glm-cycle-5.md, 7 ranked line-cited
findings. Commit `6335347` (Kate verified), pushed. cap-1 unexercised.

- **Citation accuracy: 12+/12+ EXACT** (supervisor verified nearly every
  quoted block, incl. SseState/sse_step, finish_sse, the
  ResponseBodyAttemptGuard call site) — GLM's THIRD perfect-citation
  review running (9/9, 5/5, 12+/12+). Zero drift. Two out-of-scope
  findings correctly marked "confidence: verify" — the exact discipline
  DeepSeek lacks. This run used the R1.7-enabled binary (ranged reads now
  line-numbered), reinforcing the perfect record.
- Finding 1 (an `error: null` false-positive path + discarded gateway
  error detail in the stream) is the strongest candidate for GLM's next
  CODE cycle.
- kitsu-usage 236,260 / 13,927 / 250,187 (a large 250K-token round trip).
  7 reads (2 over the ~5 guidance, grep-anchored, transparently reasoned).

**kitsu weakness W5 (reinforces R4.11, new angle):** the run hit a
recovered `PolicyDenied` (an early fs_scan_file misfire on the directory)
+ a self-corrected Edit conflict, and kitsu marked the WHOLE turn
`DISCARDED`/`not committed`/`session not saved`, exit 1 — even though the
deliverable was fully written, complete, and citation-perfect on disk via
the checkpoint mechanism. So a turn that RECOVERS from tool errors and
produces a good committed deliverable is still classified a failure. This
is too aggressive: (a) it wrongly signals failure (a literal
retry-on-failure would waste a run discarding a GOOD deliverable — the
supervisor correctly did NOT retry), and (b) combined with R4.11 the
files are on disk despite "discarded". Candidate roadmap refinement:
don't discard a turn for RECOVERED tool errors when a final answer/file
was produced.

**GLM is 3-for-3 A on reviews, 2-for-2 on code — a fully proven
contributor.** Next code cycle: implement cycle-5 finding 1 (the
error:null false positive) with the proven test-rigor discipline.

### Cycle 6 — 2026-08-14 — implements its own cycle-5 finding 1 (grade A) — also advances R4.9

Implemented cycle-5's finding 1: `process_line`'s SSE mid-stream `error`
check had a false positive (`"error": null` wrongly rejected, since
`value.get("error")` returns `Some(&Value::Null)` for JSON `null`) and
discarded the gateway's real error detail on a genuine error, surfacing
only a fixed string. Driver rebuilt fresh from the main checkout
immediately before the run. Success on `:together` attempt 1 — no
transport failure, no retry, no provider fallback needed.

**The implementation is correct and matches the endorsed fix exactly:**
`value.get("error").filter(|e| !e.is_null())` ignores a null error;
when a real error is present, `err.get("message").and_then(|m|
m.as_str()).or_else(|| err.as_str()).unwrap_or(<fixed fallback>)`
extracts the gateway's own text, and the error is now
`ProviderError::InvalidResponse(format!("provider error in SSE stream:
{detail}"))` — a genuine non-null error object still returns `Err`, so
the fail-closed contract is fully preserved.

**THE HEADLINE — tests-fail-without-impl independently verified.** GLM
wrote 3 tests exactly per the mandate: (a) `"error": null` alongside a
valid content delta now parses and emits the delta; (b) `{"error":
{"message":"rate limited"}}` returns an `Err` whose text contains "rate
limited"; (c) a normal chunk with no `error` field still parses
(regression guard). The supervisor `git stash`ed ONLY the implementation
file (`openai_compat.rs`), kept GLM's tests, and re-ran them against the
old code: **(a) and (b) both FAIL** — (a) with the old fixed
`InvalidResponse("provider returned an error object...")`, (b) with the
same fixed string (no "rate limited" substring) — while (c) passes as
expected (pre-existing happy path, unaffected either way). Implementation
was then restored and the full suite re-verified green. This is
structurally the same rigor bar cycle 4 set (a claim a test cannot pass
by accident), now on a fresh corner of `openai_compat.rs`.

**A genuine workspace-wide regression was caught and required a
non-mechanical supervisor judgment call (new territory — prior cycles'
supervisor fixes were purely mechanical: unused imports, missing
`Debug` derives, `cargo fmt`).** `cargo test --workspace` turned up ONE
unexpected red beyond the known PTY flake:
`test_sse_decoder_rejects_malformed_error_and_empty_success_payloads`,
a pre-existing test (from the `af20c26` "fourth Codex-style UX cycle"
commit, predating GLM entirely) that fed
`{"error":{"message":"secret provider detail"}}` through the decoder and
asserted the *old* fixed string appeared AND that
`"secret provider detail"` did **not** appear in the error text — i.e.
it encoded the exact opposite of what this cycle's task asked GLM to
build. The supervisor did not blindly "fix the test to pass": it first
checked whether this was a deliberate secret-redaction boundary being
crossed. Findings: (1) the codebase already discloses full provider
response-body text for diagnostics on any non-2xx HTTP status
(`openai_compat.rs`, the `!status.is_success()` branch reads and returns
the body verbatim, with an explicit "never contains the (sensitive)
token" comment scoped to the auth token specifically, not to provider
text in general); (2) actual token/credential redaction is a separate,
untouched, still-passing guard
(`test_token_never_in_error_display` + `EnvBearerAuth`/`set_sensitive`
in `auth.rs`). So surfacing `error.message` on the 200-with-embedded-
error path is consistent with, not a departure from, the codebase's
existing diagnostic-disclosure posture — the stale test encoded the
*old*, now-intentionally-changed behavior, not a security invariant.
The supervisor updated only that one assertion (kept the "malformed
JSON" case as-is; changed the error-object case to assert the message
text now surfaces, with a comment cross-referencing GLM's new
`process_line`-level test for the same contract). Re-verified: same
old-vs-new stash procedure applied to this test too — it also fails
against the unfixed implementation, confirming it is now coupled to the
real behavior, not a stale proxy. **Lesson for future GLM code cycles:**
a task-scoped read of only the target function can miss an existing
test elsewhere in the same test module that encodes contrary intent
about the very behavior being changed — worth a `grep`-for-related-
assertions pass (e.g. search the test file for the literal error string
being replaced) before finalizing, or leaving this as an explicit
supervisor-side workspace-test check going forward (as done here).

**Read/edit discipline: 6 `Read` calls (1 over the ~5 guidance, the 6th
a disclosed post-edit verification re-read of lines 454-476) + 1
`fs_scan_file`, 2 edits (`openai_compat.rs` impl, `openai_compat_tests.rs`
tests).** No re-reading loops. No spawn attempted, cap 1 unexercised.
`kitsu-usage: {"prompt_tokens":70673,"completion_tokens":2320,
"total_tokens":72993}`. `kitsu-context: {"used":10905,
"effective":107072,"window":131072}`. Run summary: "1 turn(s) ok, 6
file(s) read, 2 file(s) written (2 change(s))" — clean exit 0, no
transport issue, no recovered tool error (unlike cycle 5's W5).

**Workspace-wide verification (after the supervisor's test fix):**
`cargo build --workspace` clean; `cargo clippy --workspace --all-targets`
clean, zero warnings; `cargo test -p kitsu-providers` 70/70 (was 69/70
before the stale-test fix); `cargo test --workspace` — only the one
known PTY flake (`test_query_only_interactive_session_does_not_create_a_snapshot`)
red, re-confirmed via `git stash` to fail identically on the pre-change
tip (pre-existing sandbox limitation, unrelated to this diff); `cargo
fmt --all -- --check` clean, no reformatting needed.

**This also advances roadmap R4.9** (disclosing response-parse errors):
the SSE-embedded-error path now tells the caller *why* a mid-stream
error occurred instead of only that one occurred, the same class of
improvement R4.9 calls for on the HTTP-level parse-failure path.

**Grade: A.** All three grading criteria hold: (1) the null-error false
positive is fixed; (2) the gateway's real detail now surfaces
(opaque → diagnosable); (3) a genuine non-null error object still fails
the stream. Tests are genuine and independently verified fail-without-
impl for both required cases. The one wrinkle — a pre-existing test
encoding the old behavior — was caught by workspace-wide verification
exactly as the recipe's own "workspace-wide verification" rule
anticipates, and resolved with a documented, non-mechanical judgment
call rather than silently overridden.

**GLM is now 3-for-3 A on reviews and 3-for-3 on code (B → A → A) — the
test-rigor discipline holds for a second cycle running, and this cycle
additionally demonstrates GLM implementing its own reviewed, graded-A
finding end-to-end cleanly on the first attempt.** No new kitsu
weaknesses filed (W1-W5 from prior cycles still open, none re-triggered
this cycle — no recovered tool errors, no discard). Next cycle: a fresh
review (build portfolio further, e.g. the retry/backoff or
model-discovery paths not yet covered) or a fresh P-item implementation
(P8 trim-buffer scaling, P11 tool-schema re-serialization) — either
continues the established alternation.

### Cycle 7 — 2026-08-15 — implements its own cycle-5 finding 4 (grade A) — argument cap now reachable

Implemented cycle-5's finding 4: `MAX_SSE_LINE_BYTES` (256 KiB) was
*smaller* than `MAX_STREAMED_TOOL_ARGUMENT_BYTES` (1 MiB), so a gateway
emitting a full tool-call argument delta on a single SSE `data:` line
was always rejected by the line ceiling (`ResponseTooLarge{part:
SseLine}`) before the 1 MiB argument budget was ever consulted — the
argument cap was unreachable for single-line deltas, and the failure
carried the wrong part label. Driver rebuilt fresh from the main
checkout immediately before the run. Success on `:together` attempt 1 —
no transport failure, no retry, no provider fallback needed.

**The fix matches GLM's own cycle-5 proposal exactly:** `MAX_SSE_LINE_BYTES`
raised from `256 * 1024` to `4 * 1024 * 1024` (only that one constant
changed — `MAX_SSE_STREAM_BYTES` and `MAX_STREAMED_TOOL_ARGUMENT_BYTES`
both untouched). GLM's own reasoning trace (visible in the transcript)
derived the margin correctly: worst-case JSON-string escaping can double
a raw argument's bytes (~2 MiB for a 1 MiB argument), plus ~150 bytes of
envelope/prefix overhead, so 4 MiB clears the true worst case with
headroom while staying well under the 16 MiB stream ceiling — sound,
shown-work engineering judgment, not a round-number guess.

**THE HEADLINE — all three mandated tests, independently verified
fail-without-impl.** GLM wrote exactly the three required tests: (a)
`test_sse_decoder_accepts_large_single_line_tool_argument_within_budget`
— a 320 KiB single-line argument (over the old 256 KiB line cap, under
the 1 MiB budget) now parses and the tool call surfaces at `finalize`;
(b) `test_sse_decoder_still_rejects_line_above_raised_ceiling` — a line
over the new 4 MiB cap is still rejected as `SseLine`, and — the
clever part — the assertion pins the literal `4 * 1024 * 1024` rather
than the `MAX_SSE_LINE_BYTES` symbol specifically so the test cannot
trivially pass against both old and new code; (c)
`test_sse_decoder_oversized_single_line_argument_hits_argument_cap` — an
argument 1 KiB over the 1 MiB budget, wrapped in a line that fits the
raised cap, is now rejected with `ToolArguments` (not `SseLine`) — the
argument cap is REACHABLE. The supervisor `git stash`ed ONLY the impl
constant change (kept GLM's tests), re-ran each of the three tests
individually, and confirmed **all three fail against the unfixed 256
KiB constant**: (a) fails its own setup assertion (`the full line must
fit the new raised line cap`); (b) fails on `left: 262144, right:
4194304`; (c) fails its own setup assertion (`oversized-argument line
must fit the line cap so the argument cap fires`) — none of the three
could pass by accident against old code. Implementation was then
restored (`git apply` of the saved impl-only patch) and the full
`kitsu-providers` suite re-verified green (73/73, including the 3 new
tests). This is the fourth cycle running (3, 4, 6, 7 — B→A→A→A) holding
the test-rigor bar the recipe demands.

**Read/edit discipline: 14 `Read` calls (well over the ~5 guidance) + 4
`Edit` calls across 2 files.** The overrun was for a disclosed, honest
reason visible in the transcript: GLM initially wrote its three new
tests calling `push_bytes` with an incorrect two-argument signature
(mirroring `process_line`'s `&mut out` pattern instead of `push_bytes`'s
actual `fn push_bytes(&mut self, bytes: &[u8]) -> Result<Vec<ChatDelta>,
ProviderError>`), caught the mismatch itself by re-reading the real
signature, and re-read the inserted block twice more to apply and
verify the fix — genuine self-correction, not aimless re-reading, but
real overrun nonetheless (worth flagging: this is the second-highest
read count of the cycles logged so far, only under cycle 5's 7). No
spawn attempted, cap 1 unexercised. `kitsu-usage:
{"prompt_tokens":280944,"completion_tokens":21865,"total_tokens":302809}`
— by far the largest completion-token spend of any GLM cycle so far
(prior largest was cycle 5's 13,927), consistent with the extra
reasoning-and-fix-up reads. `kitsu-context: {"used":26229,
"effective":107072,"window":131072}`. Run summary: "1 turn(s) ok, 14
file(s) read, 2 file(s) written (2 change(s))" — clean exit 0, no
transport issue, no recovered tool error, no PolicyDenied (unlike cycle
5's W5).

**Workspace-wide verification:** `cargo build --workspace` clean;
`cargo clippy --workspace --all-targets` clean, zero warnings; `cargo
test -p kitsu-providers` 73/73 (all 3 new tests included); `cargo fmt
--all -- --check` found one whitespace-only diff in GLM's new test
block (a multi-line `.expect()`/`.finalize()` chain kept on fewer lines
than rustfmt wants) — `cargo fmt --all` applied it, zero semantic
change, re-verified clean after. `cargo test --workspace` — only the
one known PTY flake
(`test_query_only_interactive_session_does_not_create_a_snapshot`,
`crates/kitsu/tests/cli.rs`) red; re-run in isolation, fails identically,
confirmed unrelated to this diff (interactive-session PTY test, nowhere
near `openai_compat.rs`).

**Grade: A.** All three grading criteria hold: (1) the raised line cap
makes the 1 MiB argument cap reachable for single-line deltas — test
(c) proves it fires with the correct `ToolArguments` label; (2) the line
cap remains a real, bounded ceiling — test (b) proves it still rejects,
now at 4 MiB; (3) the margin is reasoned and reasonable (>= 1 MiB +
worst-case escaping + envelope, well under the 16 MiB stream cap — the
supervisor's own arithmetic matches GLM's). Tests are genuine and
independently verified fail-without-impl for all three required cases,
individually confirmed one at a time (not just as a batch).

**GLM is now 3-for-3 A on reviews and 4-for-4 on code (B→A→A→A) — this
is the third cycle in a row (4, 6, 7) with an independently-verified
fail-without-impl test suite, and the second cycle in a row implementing
its own graded-A review finding end-to-end cleanly on the first
attempt.** No new kitsu weaknesses filed (W1-W5 from prior cycles still
open, none re-triggered this cycle). Remaining open cycle-5 findings not
yet implemented: finding 5 (extra `choices` silently dropped via
`.next()`) and finding 6 (multi-line `data:` fields not accumulated,
known OpenAI-specific-assumption limitation) — either is a candidate
fresh P-item alongside P8/P11 for the next code cycle; alternatively, a
fresh review of the retry/backoff or model-discovery paths continues the
review-portfolio side of the alternation.

### Cycle 8 — 2026-08-15 — fourth review, portfolio expands to auth (grade A)

Fresh review target per the alternation (cycle 7 was code): the PRIMARY
`header()` method — `impl AuthProvider for EnvBearerAuth` in
`crates/kitsu-providers/src/auth.rs` (lines 59–95), the code that turns
an env-var bearer token into the HTTP `Authorization` header. Confirmed
unreviewed before the run (only an incidental cross-reference to
`auth.rs`'s `set_sensitive`/redaction guard in cycle 6's finding, not a
review of it). Driver rebuilt fresh from the main checkout immediately
before the run. Success on `:together` attempt 1 — no transport
failure, no retry, no provider fallback needed.

**Read/edit discipline: 2 `Read` calls — a single ranged read of
`auth.rs` lines 1–96 (covering the trait, struct, `new()`, and
`header()` in one shot) plus one ranged read of `error.rs` lines 55–80
(the `AuthError` enum). Well under the ~5-read guidance, the tightest
read count of any review cycle so far. No spawn attempted, cap 1
unexercised.** `kitsu-usage: {"prompt_tokens":17861,
"completion_tokens":8986,"total_tokens":26847}` — the smallest total
spend of any cycle logged so far. `kitsu-context: {"used":8006,
"effective":107072,"window":131072}`. Run summary: "1 turn(s) ok, 2
file(s) read, 1 file(s) written (1 change(s)), session not saved" —
clean exit 0, no transport issue, no recovered tool error, no
PolicyDenied.

**Citation spot-check: every citation verified exact against the
worktree source, quotes and line numbers both.** Checked line-by-line
against `auth.rs` and `error.rs`: auth.rs:62 (`let token = match
std::env::var(&self.env) {`), :63 (`Ok(v) if !v.is_empty() => v,`),
:64–69 (the `_` arm returning `AuthError::Missing`), :73 (`HeaderName::
from_bytes(...)`), :74–76 (`Malformed { env: self.env.clone() }`), :81
(`format!("{} {}", self.scheme, token)`), :82–86 (the `trim_start`/
`as_str` branch), :88–90 (`HeaderValue::from_str(trimmed).map_err(...)`),
:92 (`value.set_sensitive(true)`) all match verbatim. error.rs:64 and
:70 (the `Missing`/`Malformed` `#[error(...)]` Display strings) and
:65/:71 (their field shapes) also match verbatim. Supporting citations
(auth.rs:1–2 module doc, :17 trait contract, :33–37 pub struct fields,
:51–52 `new()` defaults, :60–61 and :72 comments) all check out too. No
misquote, no wrong line number found anywhere in the review.

**Four ranked, line-cited proposals, all technically sound:**

1. **`VarError::NotUnicode` misreported as `Missing` (highest
   priority).** auth.rs:62's `match` wildcards both `VarError::
   NotPresent` and `VarError::NotUnicode(_)` into `AuthError::Missing`
   (auth.rs:64–69), whose Display (error.rs:64) says "is not set" —
   false when the var is present but non-UTF-8. Verified correct: this
   is a genuine diagnostic-accuracy bug, exactly GLM's demonstrated
   strength (cycle 6's finding was the same class — opaque vs.
   diagnosable error text). Properly hedged: notes it did not read any
   caller matching on `AuthError::Missing` specifically, so the
   downstream effect of reclassifying the branch is not fully verified,
   only that the message becomes more accurate.
2. **`Malformed { env }` misattributes non-token failures.** auth.rs:
   73–77 fails on `self.header` (not the token) but reports "token in
   ${env} is malformed"; auth.rs:88–90 can fail on `self.scheme`, same
   mislabel. Verified correct against the code and against error.rs:70's
   literal Display string. Hedge is precise and self-aware: notes `new()`
   (auth.rs:51–52) only ever sets valid defaults so neither site fires
   in the default-config path — only affects callers customizing the
   `pub` fields (auth.rs:33–37), which it did not read.
3. **Token whitespace not trimmed → silent 401s.** Claims trailing
   SP/TAB in an env value passes `is_empty` and `HeaderValue::from_str`
   (auth.rs:88), producing an invisible-to-the-user malformed header.
   Verified true against the actual `http`-crate `HeaderValue`
   byte-validator (SP=0x20 and TAB=0x09 are permitted anywhere in the
   value, no leading/trailing restriction) — **but this is a claim about
   an external dependency's behavior that GLM did not read this
   session**, stated as fact ("HeaderValue::from_str ... which permits
   SP and TAB") rather than hedged per the CRITICAL instruction. The
   claim happens to be correct, so nothing is wrong in the review, but
   it is the one place this cycle where the hedge discipline slipped —
   worth naming so it doesn't compound if future cycles reason about
   unread dependency internals more speculatively.
4. **(low, correctly ranked and hedged) Secret strings not zeroized.**
   Explicitly flagged as hardening only, explicitly notes it did not
   read `Cargo.toml` to confirm a `zeroize` dependency exists.

Two non-proposal notes (redundant `to_lowercase()`, `set_sensitive`
correctly satisfies the trait contract) are both accurate and correctly
kept out of the ranked list.

**Grade: A.** Scope discipline was the tightest of any review cycle (2
reads, one already covering the whole target file); every citation
checks out verbatim; all four proposals are technically sound and
ranked sensibly (diagnostic-accuracy bug > error-attribution bug >
robustness gap > hardening nice-to-have), matching the security/
correctness judgment GLM has shown since cycle 1. The one flaw — an
unhedged claim about `HeaderValue::from_str`'s unread internals in
finding 3 — is minor (the claim is independently verifiable and
correct) but is a real, if small, breach of the CRITICAL hedging
instruction, so this cycle is not a flawless A on process, only on
substance.

**GLM is now 4-for-4 A on reviews and 4-for-4 on code (B→A→A→A code;
A→A→A→A reviews) — the auth path is now the fourth distinct area
reviewed (env/HTTP setup, an OpenAI-compat file, the SSE streaming
path, now `auth.rs`'s header construction), continuing the
review-portfolio expansion the recipe calls for.** No new kitsu
weaknesses filed (W1–W5 from prior cycles still open, none re-triggered
this cycle — no recovered tool errors, no discard, no PolicyDenied).
Open candidates for future cycles: implement finding 1 or 2 from this
review (both are small, well-scoped, security-relevant fixes suitable
for a code cycle) alongside the still-open cycle-5 findings 5/6 and
P8/P11; on the review side, `auth.rs`'s two other `header()` impls
(`StaticAuth` at ~141, `NoAuth` at ~168) and `expiry_status()` remain
unreviewed, as do the retry/backoff and model-discovery paths.

**Ramp status:** GLM holds a perfect grade record across 8 cycles (4
reviews, 4 code) with a demonstrated pattern of tight scope discipline,
accurate citations, and now a first (minor) hedging-discipline lapse to
watch in future review cycles — otherwise no change to its top-tier
standing.
