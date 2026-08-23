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
- **COMMIT IDENTITY**: every commit authored exactly `CatherinaInTheLoop
  <CatherinaInTheLoop@pm.me>`; commit with `git
  -c user.name=CatherinaInTheLoop -c user.email=CatherinaInTheLoop@pm.me
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

### Cycle 9 — 2026-08-15 — token whitespace trimmed (own cycle-8 finding, grade A)

Implemented GLM's own cycle-8 finding 3 in `EnvBearerAuth::header()`
(`crates/kitsu-providers/src/auth.rs`): the env token was used verbatim
with only an `is_empty` guard, so a token with a trailing space/tab
passed the guard and produced a header the gateway would reject —
indistinguishable from a wrong token. Finding 1 (`NotUnicode`→`Missing`)
was skipped as already implemented cross-model by the Kimi loop; finding
2 (`Malformed` misattribution) was skipped as low-reachability (needs a
risky enum ripple into a concurrently-edited crate); finding 4
(zeroize) was skipped as low priority — finding 3 was the correct,
highest-reachability, single-function pick, chosen by the supervisor
ahead of the run. Driver rebuilt fresh from main immediately before the
run. Success on `:together` attempt 1 — no transport failure, no retry,
no provider fallback, no spawn attempted (cap 1 unexercised).

**The fix, exactly as specified, single arm, no ripple:** the match arm
`Ok(v) if !v.is_empty() => v` became `Ok(v) if !v.trim().is_empty() =>
v.trim().to_string()`, with the `_ => Missing` arm and every surrounding
line untouched. No signature change, no new enum variant, no refactor —
the diff is 3 changed lines plus an updated comment in `auth.rs`, plus
two new tests in `auth_tests.rs`. Confirmed against the pre-run read of
the arm on disk that GLM mirrored the real code exactly (matched the
supervisor's own line-59 grep verbatim).

**Read/tool discipline: 4 file reads** (a 1–100 ranged read of
`auth.rs` covering `header()`, a full read of `auth_tests.rs` for the
`EnvGuard`/`#[serial]` style, a small post-edit ranged re-read of
`auth.rs` 58–73 to confirm the edit landed, and a 140–210 ranged
re-read of `auth_tests.rs` to confirm test placement), plus two
lightweight non-`Read` lookups (`find_symbol`, `read_manifest` to
confirm `serial_test` is already a dev-dependency before relying on
`#[serial]`). Within the ~5-read guidance. Wrote early — the `auth.rs`
edit landed right after the two required reads, before any of the
verification reads. `kitsu-usage: {"prompt_tokens":61344,
"completion_tokens":3956,"total_tokens":65300}`. `kitsu-context:
{"used":10502,"effective":107072,"window":131072}`. Run summary: "1
turn(s) ok, 4 file(s) read, 2 file(s) written (2 change(s)), session
not saved" — clean exit, no PolicyDenied, no discard.

**Two new `#[serial]` tests, mirroring the existing `EnvGuard` style,
each independently verified fail-without-impl by the supervisor** (not
just by GLM's own in-session reasoning): `test_header_trims_surrounding_
whitespace_from_token` (env `"token123\t"`, asserts the exact
`HeaderValue` bytes `b"Bearer token123"` with no trailing tab) and
`test_header_whitespace_only_env_is_missing_error` (env `" \t"`, asserts
`AuthError::Missing`). Supervisor stashed only `auth.rs` (kept the new
tests), ran them against the unfixed arm, and both failed as required —
`test_header_trims_surrounding_whitespace_from_token` on an
`assert_eq!` byte mismatch (untrimmed bytes end in `0x09`) and
`test_header_whitespace_only_env_is_missing_error` on `expect_err`
panicking because `header()` unexpectedly returned `Ok(Some(...))`.
Stash-popped and reran: both pass, 6/6 in the `header()` test group,
75/75 in `kitsu-providers` overall (73 + 2 new).

**One genuine (if minor) flaw: GLM's live chain-of-thought mispredicted
its own tests' failure mode.** In-session, GLM reasoned that
`HeaderValue::from_str` would reject the raw tab byte and return
`Err(AuthError::Malformed)` for both untrimmed cases. That is
factually wrong — SP and TAB are valid bytes inside an HTTP header
*value* (only the header *name* and control/non-ASCII bytes are
restricted) — and it directly contradicts cycle 8's own review finding
3, which correctly stated this exact fact about the same dependency.
The supervisor's independent fail-without-impl run confirms the actual
failure mode is different from GLM's stated prediction (an `Ok` value
with untrimmed bytes, not an `Err(Malformed)`) — but the tests
themselves are still genuinely discriminating (fail on old code, pass
on new) and the end deliverable is unaffected. Worth flagging because
it is the second cycle in a row (after cycle 8's unhedged-claim slip)
where GLM's stated reasoning about `HeaderValue`/dependency internals
didn't hold up to independent scrutiny, even though neither slip
changed the correctness of the shipped artifact.

**Workspace-wide verification:** `cargo build --workspace` clean;
`cargo clippy --workspace --all-targets` clean, zero warnings; `cargo
test -p kitsu-providers` 75/75; `cargo test --workspace` — only the one
known PTY flake (`test_query_only_interactive_session_does_not_create_a_
snapshot`, `crates/kitsu/tests/cli.rs`) red, unrelated to this diff, no
new env-test race introduced (the new tests are correctly `#[serial]`
and did not need supervisor intervention); `cargo fmt --all -- --check`
passed clean, no reformatting needed.

**Grade: A.** All required criteria hold: the token is trimmed, an
all-whitespace value still reports `Missing`, the change is genuinely
single-function with no signature/enum/refactor creep, and the tests
are real — independently verified fail-without-impl by the supervisor,
not just asserted by GLM. The one process flaw (mispredicting the
tests' own failure mode against a dependency whose real behavior cycle
8 had already correctly documented) is a repeat of the same class of
lapse as cycle 8's unhedged claim, so this is graded A on substance
with the same "not flawless on process" caveat carried forward.

**GLM is now 4-for-4 A on reviews and 5-for-5 on code (B→A→A→A→A) — the
fifth cycle in a row (4, 6, 7, 9, counting this one) with an
independently-verified fail-without-impl test suite, and the third
cycle implementing its own graded-A review finding end-to-end cleanly
on the first attempt.** No new kitsu weaknesses filed (W1–W5 from prior
cycles still open, none re-triggered — no recovered tool error, no
discard, no PolicyDenied). Two cycles in a row now show a live-reasoning
slip about unread/misremembered dependency internals (`HeaderValue`
byte-permissiveness) without affecting the shipped artifact — worth
watching as a pattern rather than acting on yet. Open candidates for
future cycles: cycle-8 finding 2 (`Malformed` misattribution, still
skipped pending the concurrently-edited-crate risk clearing), cycle-5
findings 5/6, P8/P11; on the review side, `StaticAuth`/`NoAuth`
`header()` impls, `expiry_status()`, and the retry/backoff and
model-discovery paths remain unreviewed.

**Ramp status:** GLM holds a perfect grade record across 9 cycles (4
reviews, 5 code). Top-tier standing unchanged; the two-cycle pattern of
live-reasoning slips about dependency internals (cycle 8's unhedged
claim, cycle 9's mispredicted test-failure mode) is noted for the next
review cycle to watch, not yet a downgrade since neither slip has
touched a shipped artifact's correctness.

### Cycle 10 — 2026-08-15 — fifth review, model-catalog parsing (grade A) — the hedging-slip pattern breaks

Fresh review target per the alternation (cycle 9 was code): the PURE
parse function `context_length_from_catalog` in
`crates/kitsu-providers/src/openai_compat.rs` (lines 1348–1385, ~38
lines, confirmed on disk before the run) — a models-catalog JSON
extractor that resolves a model's advisory context length via
exact-id/unique-loose-match/lone-entry fallback, then a fixed key
precedence. Confirmed unreviewed: `reviews/` held only cycles 1, 2, 5,
8 before this run. GLM was permitted to also glance at the
`loosely_matches` helper (~1195) for context. Driver rebuilt fresh from
main immediately before the run. Success on `:together` attempt 1 — no
transport failure, no retry, no provider fallback, no spawn attempted
(cap 1 unexercised).

**Read discipline: 2 file reads** ("1 turn(s) ok, 2 file(s) read, 1
file(s) written (1 change(s)), session not saved" — clean exit, no
PolicyDenied, no discard), well under the ~5-read guidance and wrote
early (`reviews/glm-cycle-10.md` was the sole write). One of the two
reads evidently spanned slightly past the pinned range, since the
review also precisely cites `get_models_response`'s doc comment at
line 1219 — correctly flagged as incidental, out-of-scope context
("context I happened to see"), not asserted as a verified fact.

**Citation spot-check: every citation verified exact against the
worktree source.** The full reproduced function block (lines
1348–1385) matches the source verbatim line-for-line. The
`loosely_matches` worked example is not just quoted but independently
re-derived: GLM traced `loosely_matches("gpt-4-turbo", "gpt-4")`
through the actual branch logic (`short`/`long` selection, `pos ==
0` at line 1207, `long.as_bytes()[5] == '-'` at line 1209) to correctly
show both `"gpt-4-turbo"` and `"gpt-4-mini"` loosely match `"gpt-4"`,
making `(loose.next(), loose.next())` at line 1367 land on `(Some,
Some)` — genuine reasoning, not paraphrase, and every line number
(1195, 1207, 1209, 1367) checks out exactly. The incidental
`get_models_response` line-1219 citation ("`model_context_length` is
advisory and gives up on any error") also matches the source verbatim.
No misquote, no wrong line number found anywhere in the review.

**Four ranked, line-cited proposals, all technically sound:**

1. **(highest) Lone-entry fallback binds a totally unrelated id.**
   Line 1369's `_ => (entries.len() == 1).then(|| &entries[0])?` accepts
   the sole catalog entry with zero regard for its id whenever the
   loose-match iterator yields 0 or ≥2 hits — silently accepting a
   worse match (no match at all) in the single-entry case than the
   ambiguous-loose-match case the same function explicitly rejects one
   branch earlier. Verified correct against the code; properly hedged
   (did not read the caller, notes the branch may exist on purpose for
   single-model local servers whose id doesn't string-match).
2. **`as_u64` silently drops float/string-encoded windows.** Line 1379
   only accepts `serde_json::Value::as_u64`, so a catalog emitting
   `8192.0` or `"32768"` would silently fall through to `None`.
   **Explicitly and correctly hedged this time**: "Per serde_json's
   documented behavior (not verified here — I did not read
   serde_json)..." — the exact discipline that slipped in cycles 8 and
   9 (both unhedged/mispredicted claims about `HeaderValue` internals)
   held cleanly here. Also hedges the caller-severity question via the
   incidental `get_models_response` line-1219 doc comment, correctly
   distinguishing "loses advisory info" from "mis-budgets."
3. **(clarity, cosmetic) Double `loose.next()` idiom is correct but
   opaque** — suggests naming the pattern or adding an inline comment;
   no behavior change, honestly ranked lowest apart from proposal 4.
4. **(minor) Exact-match `find` returns the first duplicate id** —
   correctly notes this is almost certainly a non-issue (duplicate ids
   are a server bug) and hedges the key-precedence-correctness claim
   since it verified no real server's behavior.

**Grade: A.** Tight scope discipline (2 reads, wrote early), a
substantively strong finding (proposal 1 is a genuine, previously
undetected correctness gap that inverts the function's own stated
ambiguity policy), and — the headline of this cycle — **the two-cycle
hedging-slip pattern from cycles 8/9 did not recur.** Where cycle 8 use
an unhedged factual claim about unread `HeaderValue` internals and
cycle 9 mispredicted its own tests' failure mode against the same
dependency, this cycle's structurally identical opportunity (a claim
about `serde_json::Value::as_u64`'s float/string behavior) was
explicitly and correctly hedged as unverified. Also notable: the one
piece of incidental out-of-scope material it picked up (the
`get_models_response` doc line) was cited precisely and hedged rather
than smuggled in as scope-verified fact — a positive sign for the
hedging discipline generally, not just on the dependency-internals
axis specifically.

**GLM is now 5-for-5 A on reviews and 5-for-5 on code (10-for-10
overall) — the model-catalog parsing path is the fifth distinct area
reviewed (env/HTTP setup, an OpenAI-compat parse path, the SSE
streaming path, auth.rs's header construction, now catalog parsing).**
No new kitsu weaknesses filed (W1–W5 from prior cycles still open, none
re-triggered — no recovered tool error, no discard, no PolicyDenied).
Open candidates for future cycles: implement this cycle's proposal 1
(the lone-entry-id gap) as the next code cycle — small, single-function,
well-hedged, matches the proven pattern of implementing GLM's own
graded-A findings; cycle-8 finding 2, cycle-5 findings 5/6, and P8/P11
remain open; on the review side, `StaticAuth`/`NoAuth` `header()`
impls, `expiry_status()`, `validate_models_catalog` (the function
immediately below this cycle's target), and the retry/backoff paths
remain unreviewed.

**Ramp status:** GLM holds a perfect grade record across 10 cycles (5
reviews, 5 code). Top-tier standing unchanged, and the dependency-
internals hedging concern flagged after cycles 8/9 is now resolved
rather than escalating — this cycle explicitly hedged the structurally
identical claim it would have been natural to assert outright. No
downgrade; the watch item is closed pending any recurrence.

### Cycle 11 — 2026-08-15 — implements its own cycle-10 finding 1 (grade A) — the lone-entry mis-budget is closed

Implemented GLM's own cycle-10 review proposal 1 (highest-ranked,
supervisor-verified correct ahead of the run) in
`context_length_from_catalog` (`crates/kitsu-providers/src/openai_compat.rs`,
~1348–1385). **The bug:** the `_` arm of the exact/loose match —
`_ => (entries.len() == 1).then(|| &entries[0])?` — accepted the sole
catalog entry whenever the loose-match iterator yielded 0 or ≥2 hits,
with zero regard for whether that lone entry's id had anything to do
with the requested model. A gateway returning exactly one catalog
entry queried for an unrelated model name silently inherited that
entry's context length — a wrong-window mis-budget, and a strictly
*worse* outcome than the ambiguous-multi-match case the same function
explicitly rejects one branch earlier. Driver rebuilt fresh from main
immediately before the run. Success on `:together` attempt 1 — no
transport failure, no retry, no provider fallback, no spawn attempted
(cap 1 unexercised).

**The fix, exactly as specified, single function, no ripple:** the `_`
arm became `_ => return None`, deleting the id-blind lone-entry
acceptance outright. The `(Some(only), None) => only` arm immediately
above — which already accepts a lone entry that *does* exact- or
loosely-match — is untouched, so the legitimate single-model-server
case still resolves. No signature change, no touch to the exact/loose
logic. GLM also updated both the function's doc comment and the
inline comment that stated "fall back to a lone entry, whatever its
id," replacing them with an explicit statement that there is no
lone-entry fallback and why. Diff: `openai_compat.rs` 11 lines changed
(2 comment blocks + the one-line fix); `openai_compat_tests.rs` +52
lines (one repurposed test, two new).

**Read discipline: 10 `Read` calls** (function body, `loosely_matches`
helper, two `fs_scan_file`/`find_symbol` probes that found no
`#[test]` in-file, a directory `List`, then discovery that tests live
in `openai_compat_tests.rs`, followed by targeted reads of that file's
existing catalog-test style and the lone-entry test specifically),
plus 7 lightweight non-`Read` lookups (`find_symbol`, `fs_scan_file`,
`List`) and 3 `Edit` calls. This is double the ~5-read guidance in
raw `Read` count, but the overage is explained rather than sloppy:
this crate keeps its test module in a sibling file rather than an
inline `#[cfg(test)]` block, so GLM spent several calls discovering
*where* the tests lived before it could read them, then read twice
more to precisely mirror an existing test's structure before editing
it. Wrote early relative to that discovery cost — the impl edit
landed as soon as the two core function reads were done, before any
test-file exploration. `kitsu-usage:
{"prompt_tokens":195418,"completion_tokens":16542,"total_tokens":211960}`.
`kitsu-context: {"used":21590,"effective":107072,"window":131072}`.
Run summary: "1 turn(s) ok, 10 file(s) read, 2 file(s) written (2
change(s)), session not saved" — clean exit, no PolicyDenied, no
discard.

**Three tests, one repurposed from the bug-encoding original, all
independently verified by the supervisor:**
1. `test_model_context_length_lone_nonmatching_entry_is_none` —
   repurposed from the old `test_model_context_length_lone_entry_fallback`
   (which had asserted the buggy `Some(16384)`); now asserts `None` for
   a single `"served-model"` entry queried as `"whatever"`.
2. `test_model_context_length_lone_matching_entry_resolves` — new;
   confirms a lone entry that *does* exact- or loosely-match (queried
   as `"served-model"` and as `"served"`) still resolves to
   `Some(16384)`, guarding against over-correcting into rejecting every
   single-entry catalog.
3. `test_model_context_length_exact_match_among_many` — new; confirms
   an exact match among three entries is unaffected by the change.

GLM could not run `cargo test` itself (no shell/test-execution tool in
its toolset) and said so explicitly, instead tracing the pre-edit
control flow by hand to predict the fail-before/pass-after transition
for test 1 — a materially honest substitute for execution, not an
unhedged claim. **The supervisor independently ran the proof this
cycle's own mandate required:** stashed only `openai_compat.rs` (kept
the tests), ran test 1 against the unfixed `_` arm — it failed exactly
as GLM predicted (`assertion left == right failed: left: Some(16384),
right: None`) — then restored the fix and reran: passed. All three new/
repurposed tests, plus the full 62-test `openai_compat_tests` module,
pass on the fixed tree (`cargo test -p kitsu-providers openai_compat_tests`
→ 62 passed, 0 failed, 15 filtered).

**Workspace-wide verification:** `cargo build --workspace` clean;
`cargo clippy --workspace --all-targets` clean, zero warnings; `cargo
test --workspace` — only the one known PTY flake
(`test_query_only_interactive_session_does_not_create_a_snapshot`,
`crates/kitsu/tests/cli.rs`) red, and the supervisor independently
confirmed it is pre-existing and unrelated to this diff by stashing
both changed files and rerunning the same test in isolation — identical
failure with or without the fix. `cargo fmt --all -- --check` found one
formatting diff in the new test block (multi-line `assert_eq!`
wrapping); ran `cargo fmt --all` to normalize it, noted here as the fix
rather than silently absorbed.

**Grade: A.** All required criteria hold: a lone non-matching entry now
returns `None`, a lone matching entry still resolves, exact/loose
multi-entry behavior is untouched, the change is genuinely
single-function with no signature/logic creep beyond the one arm, the
stale comment was updated rather than left inconsistent, and the tests
are real — independently verified fail-without-impl by the supervisor,
not just asserted by GLM. The only mild process note is the doubled
read count, fully explained by the tests-in-a-sibling-file layout
rather than any scope drift or re-reading of already-seen content.

**GLM is now 5-for-5 A on reviews and 6-for-6 on code (11-for-11
overall) — the fourth cycle in a row (7, 9, 10-review, 11) with either
an independently-verified fail-without-impl test suite or (for
cycle 10) a citation spot-check catching zero errors, and the fourth
cycle implementing its own graded-A review finding end-to-end cleanly
on the first attempt (after cycles 6, 7, 9).** No new kitsu weaknesses
filed (W1–W5 from prior cycles still open, none re-triggered — no
recovered tool error, no discard, no PolicyDenied; the one gap, GLM
having no test-execution tool, is a known toolset limitation already
priced into the supervisor's independent-verification step, not a new
finding). Open candidates for future cycles: cycle-10 findings 2
(`as_u64` float/string silent drop), 3, 4; cycle-8 finding 2, cycle-5
findings 5/6, P8/P11 remain open; on the review side,
`StaticAuth`/`NoAuth` `header()` impls, `expiry_status()`,
`validate_models_catalog`, and the retry/backoff paths remain
unreviewed.

**Ramp status:** GLM holds a perfect grade record across 11 cycles (5
reviews, 6 code). Top-tier standing unchanged.

### Cycle 12 — 2026-08-15 — sixth review, the catalog FETCH path (grade A) — perfect citation record extends to 6-for-6

Fresh area, alternation held (cycle 11 was code): the catalog-fetching half of
`openai_compat.rs`, distinct from cycle 10's catalog-*parsing* review
(`context_length_from_catalog`) and cycle 8's `auth.rs`. Target:
`get_models_response`, lines 1220-1270 — pre-flight re-grepped
(`grep -n 'fn get_models_response'`) and read in full to confirm the exact
range before spawning: the function opens at line 1220 and its closing
`    }` sits at line 1270 exactly as pinned, zero drift from the assigned
range. Driver rebuilt fresh from the main checkout immediately before the
run (`CARGO_TARGET_DIR=/home/mmpg/.cache/glm-worktree-target cargo build -p
kitsu`, clean, fresh binary mtime confirmed). Ran from inside the worktree.
Success on `:together` attempt 1 — no transport failure, no retry, no
provider fallback needed, no spawn attempted (cap 1 unexercised).

**Read discipline: excellent — 4 `Read` calls (under the ~5 guidance) plus
one `fs_scan_file` and one `find_symbol` lightweight lookup, 1 `Write`.**
Sequence: read the target function, read the surrounding struct/TTL-const
context, a `fs_scan_file` probe, a third `Read`, then a self-narrated
`find_symbol` to locate `validate_models_catalog` ("I have not read
`fetch_models_response`, `validate_models_catalog`, or `auth.header()`. Let
me locate `validate_models_catalog` since it directly determines the error
class mapped at line 1245") before a final targeted `Read` of it, then wrote
the review once — no re-reading loops, no over-scope drift. `kitsu-usage:
{"prompt_tokens":54568,"completion_tokens":4007,"total_tokens":58575}`.
`kitsu-context: {"used":10440,"effective":107072,"window":131072}`. Run
summary: "1 turn(s) ok, 4 file(s) read, 1 file(s) written (1 change(s)),
session not saved" — clean exit, no `PolicyDenied`, no discard.

**Grade: A.** `reviews/glm-cycle-12.md` opens with an explicit scope/hedge
preamble naming exactly what was read in-file (the function itself,
`MODELS_CACHE_TTL`/`MODELS_NEGATIVE_CACHE_TTL` at lines 68-69, the
`OpenAICompatProvider`/`ModelsCacheEntry` definitions at lines 26-53, and
`validate_models_catalog` at 1393-1410) versus what was explicitly NOT read
(`fetch_models_response`, `self.auth.header()`/`AuthProvider`,
`model_context_length`) — the cleanest up-front hedge declaration of any
cycle so far, ahead of any individual proposal needing it. 3 ranked,
line-cited proposals:
- **P1** — every cache write (line 1267,
  `context_lengths: std::sync::Mutex::new(HashMap::new())`) allocates a
  fresh empty memo map, including on the far-more-frequent 5s negative-cache
  refreshes where the map can never be populated (outcome is `Err`) — pure
  allocation waste on the hot path, plus a positive→positive refresh
  silently discards any warm memo. Explicitly hedged on `model_context_length`
  call frequency ("If `model_context_length` is called rarely... this drops
  to P3 cosmetics"). The illustrative proposal snippet has a minor bug of
  its own (references `e.context_lengths` inside a closure where `e` is no
  longer bound after the first `.and_then` consumed it) — a sketch-only
  slip, not applied to the tree, but worth noting as the first citation-
  adjacent rough edge in six reviews.
- **P2** — the auth-before-cache ordering (line 1231's `self.auth.header()`
  ahead of the line 1236 lock) is correct and matches the block comment's
  stated invariant verbatim, but is enforced only by source-order, not by
  types; proposes binding the result (`let _header = ...`) so a future
  refactor can't silently reorder it. Correctly hedges `auth.header()`'s
  actual cost/semantics as unread.
- **P3** — a 200-with-structurally-invalid-body is converted to
  `Err(InvalidResponse(...))` and negatively cached for 5s using kitsu's own
  diagnostic text rather than the server's raw body; also notes the flat 5s
  TTL applies identically to a transient 503 and a permanent 400 with no
  distinction. Correctly hedges which `ProviderError` variant a real 5xx
  produces, since `fetch_models_response` was not read.

**Citation spot-check: every citation exact, zero drift — sixth review in a
row with a perfect record.** Re-verified directly against the file:
line 1231 (`self.auth.header().map_err(ProviderError::Auth)?;`), lines
1236-1241 (the cache-hit block, verbatim including the `if entry.stored_at
.elapsed() < entry.ttl` guard), the 1228-1230 block comment ("A cached
catalog must never turn a now-missing credential into an apparent
healthcheck success. Validate current auth before consulting the cache;
auth failures themselves are deliberately never cached." — exact), lines
1243-1248 and 1256-1260 (the outcome match and negative-cache error arms),
lines 1263-1268 (the cache-write literal), the struct-doc quote "Fresh on
every cache invalidation, so it inherits the existing TTL contract."
(lines 50-51, exact), the two TTL consts (`Duration::from_secs(60)` /
`Duration::from_secs(5)`, lines 68-69, exact), and `validate_models_catalog`
's three error paths (lines 1393-1412, matching GLM's description of
malformed-JSON / missing-data-array / missing-string-id exactly). No
substantively-wrong claim and no unhedged out-of-scope assertion found —
every claim touching `fetch_models_response`, `self.auth.header()`,
`AuthProvider`, or `model_context_length` carries an explicit "hedged"
tag, exactly per the prompt's mandate.

**Observed kitsu weaknesses / notes:** none new this cycle — clean single-
attempt run, no transport issue, no `PolicyDenied`, no discard, no spawn
attempt. W1-W5 from prior cycles still open, none re-triggered.

**GLM is now 6-for-6 A on reviews and 6-for-6 on code (12-for-12 overall),
citation record 6-for-6 perfect on reviews.** Next code cycle: a fresh item
from this review's own P1 (negative-cache `HashMap` churn) is a reasonable
small-scope candidate, or pull from the still-open backlog (cycle-10
findings 2-4, cycle-8 finding 2, cycle-5 findings 5/6, P8/P11). Remaining
unreviewed surface: `fetch_models_response`, `StaticAuth`/`NoAuth`
`header()` impls, `expiry_status()`, and the retry/backoff paths.

**Ramp status:** GLM holds a perfect grade record across 12 cycles (6
reviews, 6 code). Top-tier standing unchanged.

### Cycle 13 — 2026-08-15 — seventh code cycle, implements GLM's own cycle-12 P1 (grade A)

Alternation held (cycle 12 was a review): implements GLM's own highest-
ranked cycle-12 proposal — the `get_models_response` cache write
(`crates/kitsu-providers/src/openai_compat.rs`, lines 1263-1268)
unconditionally reset `context_lengths` to an empty `HashMap` on every
refresh, discarding the warm per-model memo even on a positive→positive
refresh returning the identical catalog. The prompt supplied the crux
constraint explicitly, since GLM's own cycle-12 illustrative snippet for
this exact idea had a bug (referenced a consumed binding across a
`.and_then`): the memo may be carried across a refresh **only** when the
new catalog text exactly equals the previous entry's catalog text (both
`Ok`/successful) — otherwise it must be cleared, because carrying it across
a *changed* catalog would serve stale context lengths. Driver rebuilt fresh
from main immediately before the run
(`CARGO_TARGET_DIR=/home/mmpg/.cache/glm-worktree-target cargo build -p
kitsu`). Ran from inside the worktree. Success on `:together` attempt 1 —
no transport failure, no retry, no fallback provider needed, no spawn
attempted (cap 1 unexercised).

**The fix**, applied exactly where specified — a `carried_context_lengths`
value extracted *before* the `*cache = ...` reassignment so the immutable
borrow of `cache` ends first:
```rust
let carried_context_lengths = {
    let prev = cache.as_ref();
    match (&outcome, prev) {
        (Ok((new_status, new_text)), Some(prev_entry))
            if new_status.is_success()
                && matches!(
                    &prev_entry.outcome,
                    Ok((prev_status, prev_text))
                        if prev_status.is_success() && prev_text == new_text
                ) =>
        {
            prev_entry.context_lengths.lock().unwrap().clone()
        }
        _ => HashMap::new(),
    }
};
*cache = ttl.map(|ttl| ModelsCacheEntry {
    stored_at: tokio::time::Instant::now(),
    ttl,
    outcome: outcome.clone(),
    context_lengths: std::sync::Mutex::new(carried_context_lengths),
});
```
Both sides of the comparison require `is_success()`, so a success↔failure
flip also starts fresh, not just a text change. No struct, signature, or
call-site changes — the diff is confined to this one function, plus an
in-place update to a now-stale doc comment in `model_context_length`
(the old comment claimed the memo is "fresh on every cache invalidation,"
no longer accurate; GLM updated it to describe the conditional carry
without being asked, since the prompt's "do not change struct/signature/
call sites" left this comment fair game).

**Read discipline: 14 `Read`/probe calls** (find_symbol x3, fs_scan_file x3,
Read x8) — over the "~5 ranged reads" guidance, though several were sub-10-
line visibility/import checks (private-method reachability from the child
test module, `HashMap` import, module declaration) rather than re-reads of
already-seen content; no redundant full-function re-reads. Wrote early — the
edit landed after the first batch of context reads, then two additional
narrow reads to sanity-check test-module visibility before adding tests.
`kitsu-usage: {"prompt_tokens":291165,"completion_tokens":74787,"total_tokens":365952}`
— prompt tokens ~5.3x cycle 12's 54568, likely from loading both source and
test-file context plus the longer, more constrained prompt; worth watching
if this becomes a pattern. `kitsu-context: {"used":21666,"effective":107072,"window":131072}`.
Run summary: "1 turn(s) ok, 14 file(s) read, 2 file(s) written (2
change(s)), session not saved" — clean exit, no `PolicyDenied`, no discard.

**Tests: 2 new unit tests, both genuinely proving the targeted behavior —
independently verified by this supervisor, not just GLM's self-report.**
- `test_context_length_memo_preserved_on_same_catalog_refresh` — memoizes
  `{"m": Some(8192)}`, forces TTL expiry, refreshes via `get_models_response`
  directly against an *identical* catalog, then asserts the memo directly
  (not a proxy return value) still holds `Some(Some(8192))`.
  **Independently confirmed to FAIL without the fix**: supervisor ran
  `git stash push -- crates/kitsu-providers/src/openai_compat.rs` (isolating
  the source edit from the test file), reran this test, and observed the
  exact predicted failure — `assertion left == right failed: ... left: None,
  right: Some(Some(8192))` — then restored the stash and reconfirmed it
  passes with the fix applied. This is the test that proves the actual
  behavior change.
- `test_context_length_memo_cleared_on_different_catalog_refresh` — same
  setup but the refresh returns a *different* catalog (`context_length`
  16384 vs 8192); asserts the memo is empty post-refresh and that a
  follow-up `model_context_length` call returns the new 16384, not a stale
  8192. GLM explicitly and correctly flagged, unprompted, that this test
  **cannot fail against the pre-edit code** — the old code clears the memo
  unconditionally on every refresh, so it incidentally also satisfies
  "cleared on a changed catalog." It is a regression guard against a naive
  "always carry" fix (which would serve the stale 8192), not a fail-before/
  pass-after test. Supervisor independently confirmed this: rerunning it
  against the stashed (pre-fix) source, it also passed — matching GLM's own
  disclosure exactly, no misrepresentation.

Both tests pass with the fix in place (independently reran after `git
stash pop`). `cargo build --workspace`, `cargo clippy --workspace
--all-targets` (zero warnings — no borrow/lifetime issue from the
borrow-then-reassign pattern, no unused-import), and `cargo test --workspace`
(121/122 in `cli.rs` + all else green; the one failure,
`test_query_only_interactive_session_does_not_create_a_snapshot`, is the
known PTY flake — reproduced in isolation on a solo rerun, unrelated to this
diff) all pass. `cargo fmt --all -- --check` initially flagged two
call-chain wrapping diffs in the new test file (GLM's manual formatting of
a 3-segment method chain didn't match rustfmt's line-length rule);
supervisor ran `cargo fmt --all` to fix, then reconfirmed `--check` clean.

**Grade: A.** Correct fix, precisely matching the assigned crux (staleness
guard confirmed via spot-check: the carry condition requires both outcomes
`Ok`/successful AND exact text match — a changed catalog unconditionally
falls through to `HashMap::new()`, no regression), confined to the single
function plus one comment, no struct/signature/call-site drift, genuine
tests with an independently-verified fail-before/pass-after result for the
primary case and an honest, correct disclosure of the regression-guard
nature of the secondary case. Read count ran over guidance but stayed
purposeful (visibility/reachability checks, not re-reads).

**Observed kitsu weaknesses / notes:** prompt-token usage this cycle
(291165) is well above the recent baseline (cycle 12: 54568) despite a
similarly-scoped single-function fix — plausibly the two-file context (impl
+ pre-existing test file) plus a longer, more detailed prompt; not yet
established as a trend. No `fmt` gate is wired into the kitsu run itself,
so GLM's output needed a manual `cargo fmt --all` pass by the supervisor —
consistent with prior cycles' W-series notes that formatting isn't
self-enforced. No transport issue, no `PolicyDenied`, no discard, no spawn
attempt this cycle.

**GLM is now 7-for-7 A on code and 6-for-6 A on reviews (13-for-13
overall).** Remaining unreviewed surface for future review cycles:
`fetch_models_response`, `StaticAuth`/`NoAuth` `header()` impls,
`expiry_status()`, and the retry/backoff paths. Remaining backlog for
future code cycles: cycle-10 findings 2-4, cycle-8 finding 2, cycle-5
findings 5/6, P8/P11, and cycle-12's P2 (bind `auth.header()`'s result) and
P3 (raw-body negative-cache text) as smaller candidates.

**Ramp status:** GLM holds a perfect grade record across 13 cycles (6
reviews, 7 code). Top-tier standing unchanged.

### Cycle 14 — 2026-08-15 — eighth review, the non-streaming completion parser (grade B+) — first confirmed citation-numbering slip

Fresh review target per the alternation (cycle 13 was code): the pure
parse function `parse_completion` in
`crates/kitsu-providers/src/openai_compat.rs` (lines 599–660, ~62 lines,
confirmed on disk immediately before the run) — turns a non-streaming
chat-completion JSON body into `Vec<ChatDelta>`: choice/message
extraction, tool-call reconstruction, usage surfacing, finish-reason
mapping. Distinct from every prior review target (SSE streaming path
cycle 5, `auth.rs` cycle 8, catalog parse cycle 10, catalog fetch+cache
cycle 12/13). Driver rebuilt fresh from main immediately before the run.
Success on `:together` attempt 1 — no transport failure, no retry, no
provider fallback, no spawn attempted (cap 1 unexercised, prompt's
"do NOT spawn sub-agents" honored).

**Read discipline: 5 `Read` calls plus 2 `find_symbol` + 1 `List`** ("1
turn(s) ok, 5 file(s) read, 1 file(s) written (1 change(s)), session not
saved" — clean exit, no `PolicyDenied`, no discard). Wrote early (the
review file was the sole write). Scope ran wide: the four `Read` calls on
`openai_compat.rs` (595–675, 1–200, 400–595, 280–400) cover almost the
entire 675-line file, well past "~5 ranged reads max" in spirit even
though the literal read count matched. Justified in part — it needed the
wire-type structs, `finish_from_str`, and the SSE decoder for cross-path
comparison — but it means GLM held far more line-numbered context in
view at once than the target function, which is the most plausible
proximate cause of this cycle's citation slips (see below).

**Citation spot-check: substantive/textual quotes all verbatim-correct,
but four confirmed line-number mislabelings — the first citation defect
in 14 cycles.** Every *quoted code text* matches the source exactly,
character-for-character, at some real line in the file — GLM did not
fabricate or paraphrase any line. But several of the *line numbers
attached to those quotes* are wrong, i.e. GLM cited a number adjacent to
the correct one rather than the one the read-tool prefix actually
carried:
- The P1 code-quote block (`for tc in choice.message.tool_calls {`
  through `}));`) is labeled 628–637; the identical text is actually at
  629–638 in the source — every one of the 10 quoted lines is
  mislabeled by exactly −1. The accompanying prose ("the loop at lines
  628–637") repeats the same shift and additionally undercounts the
  loop's true span (629–639, since the for-loop's own closing `}` at
  639 falls outside the cited range).
- The "Notes" section states `choice.message.tool_calls` "is moved at
  line 628" — line 628 in the actual source is `}` (closing the prior
  `if let Some(text) = ...` block); the move happens at line 629.
- Prose states `parse_completion` "calls it [`finish_from_str`] at line
  641" — line 641 is `.finish_reason` (mid-chain); the actual call
  `.map(finish_from_str)` is at line 643 (off by −2). The paired
  claim about the streaming path calling it "at line 541" is exactly
  correct (verified: `self.finish = Some(finish_from_str(&fr));`).
- The "Notes" section cites "the `None` arm at lines 644–648"; the
  actual `None => { ... }` arm is lines 646–650 (off by −2 at both
  ends — 644–645 are actually the tail of the `Some` arm's match
  expression).
- A P2 aside claims `arguments: Option<String>`'s optionality is "legal
  under `#[serde(default)]` at line 302/304"; those are the *field*
  lines (`name: Option<String>,` / `arguments: Option<String>,`); the
  actual `#[serde(default)]` attributes sit one line above each, at 301
  and 303.

All other citations verified exactly: P0's 602/603–605 quote block, the
streaming-path null-filter at 466 and its comment at 470, the semantic-
choice line 617, the streaming tool-call-skip guard at 566–570, the
argument-accumulation line 536, `finish_from_str` itself at 339–345, the
four wire-type struct ranges (307–313/315–321/323–329/331–337, all
exact), `Usage::is_empty` at types.rs 148–150, and the SSE usage-capture
block at 488–491. The review's own closing line — "Each proposal quotes
the exact lines verbatim with their absolute line numbers" — is
therefore not fully accurate; supervisor is flagging this explicitly per
instruction, since the CRITICAL prompt clause was "copy it from the
prefix," and five instances (one entire 10-line block plus three isolated
mislabels) did not.

**Four ranked, line-cited proposals — substantively sound and honestly
hedged despite the numbering slip:**
1. **(P0, highest) `"error": null` is a hard failure here but explicitly
   ignored (with a code comment calling out the exact gateway behavior)
   by the streaming path.** A genuine, verifiable divergence between two
   code paths sharing one wire shape (per the module doc comment, lines
   1–3, also verified exact) — the strongest finding this cycle, no
   hedge needed since both sides were read.
2. **(P1) Degenerate all-empty tool calls (`{}`) pass the semantic-
   choice check and are emitted unconditionally**, unlike the streaming
   path's non-empty-name-or-id guard. Correctly hedged on the
   downstream tool-dispatch consumer, which GLM did not read.
3. **(P2) `arguments` defaults to `""` rather than `"{}"`** — correctly
   self-downgraded: GLM noticed the streaming path shares the exact same
   default, so this is not a divergence, and it explicitly hedged the
   downstream JSON-parsing assumption.
4. **(P3) `finish_from_str`'s wildcard silently collapses
   `"content_filter"` (and anything else unrecognized) to `Stop`.**
   Strongly and correctly hedged: GLM explicitly states it did not read
   the `FinishReason` enum and cannot confirm a better variant exists,
   making the whole proposal conditional.

**Grade: B+.** Downgraded from the run of A's specifically for the
citation-numbering defects above — a real, confirmed break of the
established "quote it, don't recall it" discipline that Cycles 1–13 held
perfectly, including the closing summary's now-inaccurate self-claim of
verbatim line fidelity. Everything else in the review is strong: all
four proposals are technically correct where checked, ranked sensibly
(the one unhedgeable-and-true divergence is P0, the ones needing
downstream context are properly caveated), and no claim was asserted as
fact beyond what GLM actually read — this is a numbering-precision
lapse, not a hedging lapse or a substantive-correctness lapse. No
misquoted *text* was found anywhere in the review.

**Observed kitsu weaknesses / notes:** no new transport, `PolicyDenied`,
or discard weaknesses this cycle. Read/token usage was efficient
(`kitsu-usage: {"prompt_tokens":73051,"completion_tokens":12371,
"total_tokens":85422}`, `kitsu-context: {"used":15088,"effective":107072,
"window":131072}` — well below cycle 13's outlier). The read-tool line-
number prefixes are the ground truth kitsu hands the model; nothing in
kitsu itself appears to have corrupted them (this supervisor's
independent reads confirm the *source* line numbers are exactly what the
review should have cited) — the slip looks like a model-side transcription
error made while holding ~4 large overlapping ranged reads of the same
file in context at once, not a kitsu tooling defect. Worth watching
whether wide, overlapping ranged-read sessions correlate with future
citation drift.

**GLM is now 7-for-7 A on code and 6-for-7 on reviews (13-for-14
overall, one B+).** Remaining unreviewed surface for future review
cycles: `fetch_models_response`, `StaticAuth`/`NoAuth` `header()` impls,
`expiry_status()`, and the retry/backoff paths. Remaining backlog for
future code cycles: this cycle's P0 (`"error": null` parity fix) is a
strong, small, well-verified next code-cycle candidate; also cycle-10
findings 2-4, cycle-8 finding 2, cycle-5 findings 5/6, P8/P11, and
cycle-12's P2/P3.

**Ramp status:** GLM's perfect-grade streak ends at 13 cycles; cycle 14
is graded B+ for confirmed citation-numbering errors (not substantive or
hedging errors). Standing remains strong — top-tier contributor — but no
longer flawless; this is the first mark on the record and worth
monitoring for recurrence rather than treating as noise.

### Cycle 15 — 2026-08-15 — non-streaming `error: null` parity fix (grade A) — implements its own cycle-14 P0

Implemented GLM's own cycle-14 finding P0 in `parse_completion`
(`crates/kitsu-providers/src/openai_compat.rs:602`): the non-streaming
completion parser rejected ANY response body carrying an `error` key,
including `"error": null` (a JSON null is still `Some`), diverging from
the streaming path (line 466), which already ignores a null error via
`.filter(|e| !e.is_null())`. Fix (the only line touched):
`value.get("error").is_some()` → `value.get("error").is_some_and(|e|
!e.is_null())`. Signature and everything else unchanged. Commit
`f3c38d4`, pushed. Success on `:together` attempt 1, exit 0, cap-1 held,
no spawn attempted.

- **Correctness: all three criteria hold.** `"error": null` alongside a
  valid choice now parses (content delta + `Done(Stop)`); a real
  `"error": {...}` object still returns `InvalidResponse`; the fix now
  matches the streaming path's established parity (confirmed by GLM
  reading line 466 briefly, per the brief's instruction, and
  independently re-confirmed by the supervisor before dispatch).
- **Tests genuinely prove the behavior change, not a proxy — supervisor
  independently verified fail-without-impl.** Two new tests added
  directly on `parse_completion` in `openai_compat_tests.rs`:
  (a) `test_parse_completion_null_error_alongside_valid_choice_parses_ok`
  — a body with `"error": null` + a valid choice parses OK, yielding the
  content delta and `Done(Stop)`; (b)
  `test_parse_completion_real_error_object_still_rejected` — a body with
  a real `"error": {...}` object still returns `InvalidResponse`. The
  supervisor `git stash push`-ed ONLY `openai_compat.rs` (keeping the
  test file, which lives in a separate `openai_compat_tests.rs`), reran
  test (a) alone against the unfixed impl: it panicked exactly as
  predicted —
  `InvalidResponse("provider returned an error object in a successful
  completion response")` at the `.expect()` call — a genuine failure,
  not a compile error or a vacuous pass. Test (b) was run the same way
  and passed even against the unfixed code (expected: the old code
  already rejected real error objects too — it's a regression guard
  against over-correction, not itself proof of the fix, and the run
  summary above states this distinction plainly). Stash was then popped
  and the full fixed suite reverified: kitsu-providers 81/81 (79
  pre-existing + 2 new).
- **Checked for stale assumptions:** grepped the test file for any
  pre-existing test that encoded the OLD `"error": null` rejection as
  intended behavior — none found. The one pre-existing rejection test
  (`test_nonstream_completion_rejects_error_and_empty_choice_objects`)
  uses a real `{"message": ...}` payload, unaffected by the fix.
- **Stayed tight vs. cycle 14's over-read.** This was a CODE cycle
  (cycle 14 was a review, so tool-call profiles aren't directly
  comparable), but worth recording: GLM used 8 `Read` calls (ranged: the
  target function 590–660, the streaming null-filter 455–485, two
  apparently-exploratory ranges at 300–330 and 1760–1790 that turned out
  unnecessary for the one-line fix, a struct range 775–790, the file's
  import header 1–35, one capped read of the 78 KiB
  `openai_compat_tests.rs`, and a final 930–955 to place the new tests
  near the existing sibling test) plus 6 lightweight `find_symbol`/
  `fs_scan_file` grep-style calls — more raw tool calls than the ~5
  ranged-read guidance, though each individual read was narrow and nothing
  approached cycle 14's whole-file re-read pattern. No citation-drift risk
  here since the deliverable is code + tests, not quoted prose, so the
  extra exploration cost tool-calls/tokens but not correctness.
  kitsu-usage: `{"prompt_tokens":186206,"completion_tokens":2713,
  "total_tokens":188919}`, kitsu-context:
  `{"used":27348,"effective":107072,"window":131072}` — a fairly large
  prompt-token count for a one-line fix, consistent with the read count
  above.
- **Supervisor-only mechanical fix:** `cargo fmt --all` reformatted one
  line in GLM's second new test (a two-line `let error = ...` wrapped to
  fit rustfmt's width) — a pure style nit, not a correctness or logic
  change. `cargo build/clippy/test --workspace` all clean; the only test
  failure was the pre-known PTY `cli.rs` flake
  (`test_query_only_interactive_session_does_not_create_a_snapshot`,
  unrelated to `openai_compat.rs`, reproduced in isolation too — an
  environment/PTY-availability issue in this sandbox, not caused by this
  change).

**No new kitsu weaknesses observed this cycle.** Transport, spawn cap,
and turn-commit behavior all behaved as expected on the first attempt.

**GLM is now 8-for-8 A on code and 6-for-7 on reviews (14-for-15
overall, one B+, review-then-implement alternation working as intended
— a genuine finding from a review turned into a genuinely tested fix
one cycle later).** Next: alternate back to a fresh review of new
surface (`fetch_models_response`, `StaticAuth`/`NoAuth` `header()`
impls, `expiry_status()`, retry/backoff paths — per cycle 14's
remaining-surface note) or pick up the next-ranked backlog item
(cycle-10 findings 2-4, cycle-8 finding 2, cycle-5 findings 5/6,
cycle-12 P2/P3).

**Ramp status:** GLM holds a strong 14-for-15 overall record (8-for-8 on
code cycles, unbroken). Cycle 14's citation-numbering slip stands alone;
cycle 15's code discipline (correct fix, genuine fail-without-impl-
verified tests, no stale-assumption misses) shows no carryover effect
from the prior cycle's grade dip.
