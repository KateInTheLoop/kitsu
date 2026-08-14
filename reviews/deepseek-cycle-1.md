# Review: `crates/kitsu-core/src/context.rs` — Cycle 1

**Scope**: `ContextMeter`, `ContextBudget`, and the compaction circuit-breaker
measurement layer. 378 lines.

---

## Rank 1: Estimate drift under anchor — no calibration of the chars/token heuristic

**Lines**: 96–102 (`ContextMeter::estimate` anchored branch), 19–21
(`CHARS_PER_TOKEN`), 31–42 (`message_tokens`).

When the meter is anchored, it trusts the real `prompt_tokens` for the prefix
and **blindly applies the crude `CHARS_PER_TOKEN` heuristic to the tail**
(line 100). Because the anchor itself embeds both a real token count (`tokens`)
and the full prefix message count (`sent_len`), the meter holds enough data to
compute the *actual* chars/token ratio the real tokenizer produced for that
prefix. It never does.

**Impact**: If the real tokenizer is substantially different from the 4-char
heuristic (e.g. a multilingual model, heavy code, or a BPE vocabulary that
handles whitespace aggressively), the tail estimate systematically diverges from
the prefix anchor, yielding a composite number whose error grows linearly with
the tail size. The drift is worse right when accuracy matters most: long
conversations where the tail (post-anchor) is large.

**Concrete proposal**: In `record`, optionally compute and store
`prefix_chars / tokens` (or, more robustly, the prefix token count per
`message_tokens` vs the real count). In `estimate`, blend the anchored-tail
estimate by re-weighting `message_tokens` output with that observed ratio. A
simpler first step: at least log a telemetry counter when the anchor's own
chars/token ratio deviates >20% from the 4-char default, so operators can see
how often this happens.

---

## Rank 2: `ContextMeter::breakdown` is always anchor-free, producing inconsistent numbers vs `estimate()`

**Lines**: 127–138 (`breakdown`).

The doc comment on line 128 says this is intentional: "pure estimate; ignores
the usage anchor". But a user calling both `estimate()` and `breakdown()` on
the same message list will see `estimate()` returning an anchor-informed total
while `breakdown().total()` returns a different (purely chars/token) total.
The `/context` command surfaces `breakdown()` per-role numbers alongside the
budget comparison, so in anchored state the reported per-role attribution does
not reconcile with the actual estimate driving the compaction decision.

**Impact**: Confusing telemetry and potential user-facing inconsistency in
`/context`. Less important for correctness (compaction gating uses `estimate`,
not `breakdown`) but undermines trust when a user cross-checks.

**Concrete proposal**: When anchored, distribute the anchor's residual (`tokens -
sum(message_tokens(prefix))`) proportionally across the prefix roles and then
add the tail per-role estimate. This keeps `breakdown().total() == estimate()`
in all states.

---

## Rank 3: No circuit-breaker measurement exists — only a budget target

**Lines**: 173–205 (`ContextBudget`), and the file header lines 1–7.

The file's opening doc (line 3) advertises "compaction circuit-breaker
measurement layer", but no such layer is implemented. `ContextBudget` computes a
`target` (line 203–204) and leaves it at that. There is:

- No tracking of how many consecutive rounds have been at/above the target.
- No hysteresis or back-off on repeated compactions that fail to bring usage
  under target.
- No recording of how much the last compaction actually saved (token delta).
- No detection of a "compaction spiral" where each round trims content only to
  immediately overflow again.

**Impact**: The compaction cascade (Stage 2+, outside this file) operates
blindly. It has no signal about whether its last trimming round actually helped,
whether it's fighting a conversation that grows faster than it can trim, or
whether the budget's static `buffer = min(window/10, 2048)` (line 203) is
appropriate for this conversation's actual growth rate. This is the
highest-impact gap in the file when viewed from the "compaction circuit-breaker"
remit.

**Concrete proposal**: Add a `CompactionTelemetry` struct (or extend
`ContextBudget`) that tracks:
1. `last_pre_estimate: u64` and `last_post_estimate: u64` — the before/after of
   the most recent compaction round, set by the cascade and read by the budget
   on the next round.
2. `consecutive_overflow_rounds: u32` — incremented each round that `estimate >
   target`, reset when under.
3. A `circuit_breaker_hit() -> bool` that returns true when
   `consecutive_overflow_rounds >= 3`, signalling the cascade should escalate
   (e.g. from trim to summarize) or the caller should warn the user.

Keep this minimal and measurement-only; the cascade remains the actor. But give
it data.

---

## Rank 4: `had_schema` is binary but tool schema size varies across rounds

**Lines**: 60–71 (anchor tuple), 81–83 (`record`), 115–126
(`anchor_includes_schema`).

The anchor records only *whether* tools were present, not how many or at what
token cost. If round N offers 2 small tools (400 tokens of schema) and round
N+1 offers 15 large tools (3000 tokens), `anchor_includes_schema` returns
`true` after round N, but the real overhead for round N+1 is 2600 tokens higher
than what the anchor captured. The caller would skip adding the schema estimate
(line 122 doc), undercounting by thousands of tokens.

**Impact**: Silent undercount, growing with tool count. Defensive callers must
either always add the schema estimate (defeating the purpose of
`anchor_includes_schema`) or accept the risk.

**Concrete proposal**: Replace `bool` with `Option<u64>` — the estimated token
cost of the tool schemas in the anchored request. `anchor_includes_schema` would
return `Some(cost)` instead of `true`, and callers can compare against the
current round's schema estimate and add the delta. A null `Option` preserves the
existing "no tools" case without overhead.

---

## Rank 5: `message_tokens` ignores `reasoning_content` / thinking tokens

**Lines**: 38–50 (`message_tokens`).

If `ChatMessage` carries a `reasoning_content` (or similar) field — common in
extended-thinking models — `message_tokens` does not account for it.
Assistant messages with reasoning blocks can be orders of magnitude larger than
their visible `content`, and this function would silently undercount them by
a factor of 10–100×.

**Impact**: For reasoning models (Claude extended thinking, DeepSeek-R1, o1,
Gemini thinking), the estimate is catastrophically low. The budget appears
healthy while the real token count has already exhausted the window.

**Concrete proposal**: Check whether `kitsu_providers::ChatMessage` already has
a `reasoning_content` field. If yes, estimate it at the prose rate and add it
to the assistant total. If not, add a TODO referencing the provider struct
definition and the model discovery `supports_reasoning` flag.

---

## Rank 6: Anchor tuple is opaque — use a named struct

**Lines**: 68 (`Option<(u64, usize, bool)>`).

The three-element tuple `(tokens, sent_len, had_schema)` is used in multiple
match arms (lines 97, 108, 117) with positional destructuring. Adding a fourth
field (e.g. schema token count from Rank 4, or a prefix ratio from Rank 1) would
require touching every match site and is error-prone.

**Impact**: Maintenance friction; no correctness bug today.

**Concrete proposal**:

```rust
struct Anchor {
    prompt_tokens: u64,
    sent_len: usize,
    had_schema: bool,
}
```

Replace the tuple in `ContextMeter::anchor` and update the three match arms.

---

## Rank 7: `ContextBudget::new` accepts zero window silently

**Lines**: 186–205.

If model discovery produces `window = 0` (e.g. a gateway configuration bug),
`new()` returns `(0, 0, 0)` and every estimate will appear to be over budget,
triggering compaction on every round. There is no `debug_assert`, no log
warning, and no fallback to `DEFAULT_CONTEXT_WINDOW`.

**Impact**: Degraded experience (constant unnecessary compaction) with no
diagnostic signal.

**Concrete proposal**: Add `debug_assert!(window > 0, "ContextBudget: window is zero")`
on entry. In release builds, fall back to `DEFAULT_CONTEXT_WINDOW` when window
is 0, and consider logging a rate-limited warning.

---

## Rank 8: `buffer = min(window/10, 2048)` has no model-specific or evidence-based tuning

**Lines**: 203–204.

The fixed 2048-token cap on the buffer means a 128K-window model gets a 2048
buffer (only 1.6% of window) while a 32K-window model also gets 2048 (6.4%).
This asymmetry doesn't match the reality that larger-window models typically
have proportionally larger output buffers and looser headroom requirements.
The 2048 cap likely came from the era when 8K–32K was standard.

**Impact**: For 128K+ models, the trim target is unnecessarily close to the
effective limit, causing compaction to fire more often than needed.

**Concrete proposal**: Replace the fixed 2048 cap with a proportional minimum:
`max(window / 20, 512).min(window / 10)`, or allow the cap to be overridden via
the model's resolved profile (if `kitsu_providers` exposes a per-model
configuration). At minimum, raise the cap to 4096 and note the rationale.

---

## Summary

| Rank | Area                          | Severity | Category        |
|------|-------------------------------|----------|-----------------|
| 1    | Estimate drift (no calibration) | Medium   | Estimate accuracy |
| 2    | `breakdown` vs `estimate` inconsistency | Low | Budget policy |
| 3    | Missing circuit-breaker telemetry | High | Compaction |
| 4    | `had_schema` binary vs variable overhead | Medium | Estimate accuracy |
| 5    | Reasoning content ignored    | High     | Estimate accuracy |
| 6    | Opaque anchor tuple           | Low      | Code quality |
| 7    | Zero-window silent pass-through | Low   | Budget policy |
| 8    | Buffer cap stale for 128K+    | Medium   | Budget policy |
