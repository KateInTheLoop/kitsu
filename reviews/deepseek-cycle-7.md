# `summarize_or_fallback` review (orchestrator.rs:2545-2660)

## Proposal 1 — Empty summary completion mislabeled as `ProviderFailure`

**Lines 2627–2633:**

```rust
2627	                        SummaryTextCollection::Complete { usage, .. } => {
2628	                            if let Some(usage) = usage {
2629	                                record_compaction_usage(cfg, round, turn_usage, usage)?;
2630	                            }
2631	                            *failures = failures.saturating_add(1);
2632	                            Some(SummaryFallbackReason::ProviderFailure)
2633	                        }
```

This arm catches `Complete` responses whose text is empty or whitespace-only (the preceding arm at line 2599‑2600 guards on `!text.trim().is_empty()`).  The fallback reason emitted is `ProviderFailure`, which conflates two distinct conditions: the provider genuinely failed (connection error, API error — the `Failed` arm at line 2634) versus the provider returned a valid but empty completion.  A caller inspecting `progress.fallback_reason` cannot distinguish “the model produced nothing useful” from “the provider call itself errored.”

**Recommendation:**  If `SummaryFallbackReason` has an `EmptyResponse` variant (or one can be added), use it here.  Failing that, `ResponseTooLarge` (used at line 2625 for a non‑fitting rebuild) is semantically closer — an empty summary cannot reduce the history — than `ProviderFailure`.

**Caveat:**  I have not read the definition of `SummaryFallbackReason`; the existence of a suitable variant is unverified.

---

## Proposal 2 — Silent no‑op path when fallback trim removes zero exchanges

**Lines 2651–2658:**

```rust
2651	    let current = structural_request_tokens(messages, schema_json, has_schema);
2652	    let removed = drop_old_exchanges_detailed(messages, current, target, message_tokens, true);
2653	    if removed > 0 {
2654	        meter.invalidate();
2655	        progress.exchanges_removed = removed;
2656	        progress.outcome = Some(CompactionOutcome::SummaryFallbackTrim);
2657	        progress.fallback_reason = fallback;
2658	    }
```

When `removed == 0`, nothing is written to `progress.outcome` or `progress.fallback_reason`.  The function returns `Ok(())` (line 2659), and the caller sees whatever `progress` held before entering the function — possibly a stale outcome from a prior compaction round.  This makes it impossible for the caller to distinguish “we entered the fallback path and found nothing to trim” from “the function never ran (or returned early at line 2562).”

**Recommendation:**  Add an `else` branch that records a no‑op outcome, e.g.:

```rust
} else {
    progress.outcome = Some(CompactionOutcome::SummaryFallbackTrim);
    progress.fallback_reason = fallback;
    // exchanges_removed stays 0
}
```

This preserves the fallback reason for diagnostics even when zero exchanges were dropped.

**Caveat:**  I have not read the definition of `CompactionOutcome`; if `SummaryFallbackTrim` implies `exchanges_removed > 0`, a distinct variant (e.g. `SummaryFallbackNoOp`) may be needed instead.

---

## Proposal 3 — Circuit breaker gates the pure `bounded_summary_request` computation, obscuring cause

**Lines 2565–2569:**

```rust
2565	    let fallback = if *failures >= MAX_COMPACT_FAILURES {
2566	        Some(SummaryFallbackReason::CircuitBreaker)
2567	    } else {
2568	        match bounded_summary_request(cfg, &messages[span.prefix_end..span.recent_start]) {
2569	            None => Some(SummaryFallbackReason::RequestTooLarge),
```

When the circuit breaker trips (`failures >= MAX_COMPACT_FAILURES`), the function skips the call to `bounded_summary_request` and emits `CircuitBreaker` immediately.  If the request would also have been `RequestTooLarge`, that more‑specific cause is never recorded.  The caller sees `CircuitBreaker` and cannot tell whether the final straw was a size problem or a provider‑side failure.

**Recommendation:**  If the diagnostic precision matters (e.g. for telemetry or operator alerting), compute `bounded_summary_request` unconditionally before the circuit‑breaker check, then choose the fallback reason as `RequestTooLarge` when the request doesn’t fit, falling back to `CircuitBreaker` only when the request *would* have fit but we are rate‑limiting.

```rust
let bounded = bounded_summary_request(cfg, &messages[span.prefix_end..span.recent_start]);
let fallback = if bounded.is_none() {
    Some(SummaryFallbackReason::RequestTooLarge)
} else if *failures >= MAX_COMPACT_FAILURES {
    Some(SummaryFallbackReason::CircuitBreaker)
} else {
    // … proceed with the request …
};
```

This adds a pure‑computation cost (negligible) in exchange for a more accurate `fallback_reason`.

**Caveat:**  If `MAX_COMPACT_FAILURES` is small and callers treat `CircuitBreaker` and `RequestTooLarge` identically (both trigger the same fallback trim), this change has no behavioural impact — it is purely a diagnostic improvement.
