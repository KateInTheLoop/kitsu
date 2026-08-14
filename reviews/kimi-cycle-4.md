# Review: `crates/kitsu-core/src/context.rs` (cycle 4)

Scope: this file only. It contains the measurement primitives (`ContextMeter`,
`ContextBudget`, `message_tokens`, `estimate_str`); trimming/compaction itself
lives elsewhere (per the doc comment at lines 10–11), so "compaction
circuit-breaker" concerns are addressed here at the measurement layer that any
circuit breaker would have to trust: anchor validity and estimate sanity.

## Ranked proposals

### 1. Anchor validity is length-only; add a content fingerprint or generation counter

- **Lines:** 100–107 (`estimate`), 111–113 (`anchored`), 122–124
  (`anchor_includes_schema`), 79–81 (`record`), 83–93 (`invalidate`).
- **What:** The anchor is trusted whenever `sent_len <= messages.len()` (line
  102). That checks *count*, not *identity*: an in-place edit of a message
  inside the anchored prefix (queue/steering edits, a rewritten tool result)
  keeps the length, so `estimate` silently reports `anchor_tokens + tail` for
  content the anchor never measured. The `invalidate` doc (lines 83–90)
  acknowledges the reshape hazard but pushes enforcement entirely onto callers;
  nothing in this file detects a missed `invalidate()`.
- **Change:** Store a cheap prefix fingerprint in the anchor — e.g. a
  `u64` hash (FNV/xxhash) over the leading `sent_len` messages' role +
  content lengths (or contents) computed in `record` (lines 79–81) — and
  recompute/compare it in the guard at line 102. Cheaper alternative: a
  generation counter owned by the conversation, bumped on *any* mutation
  (append included), with the anchor storing the generation at record time;
  append-only growth can be exempted by tracking an append-only counter
  separately.
- **Why it wins:** Converts the worst failure mode in the file — a silently
  wrong estimate that the trim cascade and `/context` then treat as real API
  usage — into a safe fallback (full recompute, line 105). This is the
  measurement-side circuit breaker a compaction loop needs: it must know when
  its "real usage" anchor no longer describes reality.
- **Risk:** Hashing the prefix on every `estimate` is O(prefix); mitigate by
  hashing only lengths (near-free) or by the generation-counter design, which
  is O(1) but requires discipline from the message-list owner.

### 2. No sanity check on recorded usage; clamp or distrust implausible anchors

- **Lines:** 79–81 (`record`), 100–107 (`estimate`).
- **What:** `record` accepts any `prompt_tokens` a gateway returns and
  `estimate` then treats it as authoritative. A buggy/proxying gateway that
  returns 0, cached, or aggregate usage poisons every subsequent estimate
  until the next `record`, and the trim cascade would under-trim (overflow
  risk) or over-trim (needless compaction) accordingly.
- **Change:** In `record` (or in `estimate`'s anchor branch, line 102),
  compare the anchor against the anchor-free full estimate of the same
  `messages` and, if it diverges beyond a generous band (e.g. outside
  0.4×–2.5×), ignore the anchor and fall back to the full estimate. Emit a
  telemetry counter when this trips.
- **Why it wins:** A self-contained circuit breaker: bad external data degrades
  to the chars/token heuristic instead of propagating. The fallback path
  already exists (line 105), so the change is small and local.
- **Risk:** Legitimate tokenizer differences (chars/4 is rough — see proposal
  3) can trip the band on unusual content; keep the band wide and make this
  advisory (fall back, never error) so the cost of a false trip is only a
  full recompute.

### 3. `message_tokens` ignores per-message overhead and counts bytes, not chars

- **Lines:** 35–43 (`message_tokens`), 16–18 (rate constants), 29–31
  (`estimate_str`).
- **What:** Two systematic biases. (a) No per-message constant: real chat
  tokenizers add role/separator overhead per message (typically ~3–4 tokens),
  so histories with many short messages are underestimated. (b) `s.len()`
  (lines 30, 36, 40) is *bytes*: CJK text is ~3 bytes per token-bearing
  character, so the 4-bytes-per-token rate (line 16) underestimates CJK-heavy
  content by roughly a third; conversely dense ASCII is roughly right.
- **Change:** Add a small `PER_MESSAGE_TOKENS` constant in `message_tokens`
  (line 35), and either count `chars()` instead of bytes or use a two-rate
  heuristic (ASCII vs non-ASCII byte fraction) inside `estimate_str`. Keep the
  JSON denser rate (line 18) as-is.
- **Why it wins:** The full-estimate fallback (line 105) and `breakdown`
  (lines 128–140) both rest entirely on `message_tokens`; fixing its bias
  improves every unanchored path and the `/context` display, and shrinks the
  error band the proposal-2 sanity check must tolerate.
- **Risk:** A per-message constant overestimates for providers without that
  overhead; err small (2–3 tokens) so the bias stays conservative (slight
  over-fill is safer than overflow, matching the philosophy at lines 20–21).

### 4. Trim buffer does not scale with estimate error on large windows

- **Lines:** 166 (`buffer`), 155–158 (doc), 159–173 (`ContextBudget::new`).
- **What:** `buffer = min(window/10, 2048)` (line 166). The 2048 cap is reached
  at a 20 480-token window; beyond that the buffer is flat while the
  *absolute* error of the chars/token estimate grows with prompt size. On a
  1M-token window filled to `target`, a ±5 % estimate error is ~40k tokens —
  20× the buffer — so `effective` (line 165) can be exceeded despite trimming
  to `target`.
- **Change:** Scale the cap with the window (e.g. `min(window/10, 16_384)`),
  or better, derive the buffer from the estimate's known worst-case error for
  the current fill level (`buffer = max(2048, target * ERROR_FRACTION)` with
  `ERROR_FRACTION` ~5 %), capped at `window/10`.
- **Why it wins:** Makes the headroom policy honest: the buffer exists to
  absorb estimation error, and that error is proportional to fill, not
  constant. Large-window models are exactly where the current flat cap bites.
- **Risk:** A larger buffer wastes usable window on huge-context models;
  mitigate by capping at `window/10` and, once proposal 3 lands, lowering
  `ERROR_FRACTION` as the estimate improves.

### 5. Budget reserve and the `max_output` actually sent can disagree

- **Lines:** 159–165 (`reserve` computation and clamp), 24–25
  (`MIN_OUTPUT_RESERVE`), 20–22 (`DEFAULT_CONTEXT_WINDOW`).
- **What:** The reserve is `max(max_output, 512).min(window/2)` (lines
  160–164). The `window/2` clamp protects the prompt but silently desynchronizes
  the budget from the request: with `window = 8 192` (the default, line 22)
  and `max_output = Some(8 192)`, the budget reserves 4 096 while the provider
  is still told it may emit 8 192 — a full-length reply overflows the window
  the budget promised. Separately, the 512 floor (line 25) is thin for
  reasoning-style models whose hidden reasoning tokens consume output budget.
- **Change:** Expose the clamped reserve on `ContextBudget` (e.g. a
  `pub reserve: u64` or `clamped_max_output()`) so the caller sends a
  `max_output` consistent with the budget, and consider raising
  `MIN_OUTPUT_RESERVE` or making it model-aware.
- **Why it wins:** The budget's invariant `target <= effective <= window`
  (asserted in the test at the end of the file) only holds end-to-end if the
  reply is actually bounded by the reserve; today that invariant is enforced
  on the prompt side but merely assumed on the output side.
- **Risk:** API surface change (new field/method); callers that ignore it are
  no worse off than today. Raising the floor costs prompt room on tiny
  windows, but the existing half-window clamp (line 164) bounds that.

## Notes

- Strengths worth keeping: the schema-aware anchor flag (lines 63–68, 122–124)
  and its regression tests, the documented `invalidate` contract (lines
  83–93), and saturation-safe arithmetic throughout `ContextBudget::new`
  (lines 165–167).
- No compaction loop lives in this file; proposals 1 and 2 are the
  measurement-layer circuit breakers that a compaction circuit breaker
  elsewhere should consume before trusting `estimate`.
