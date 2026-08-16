# Cycle 10 review: `context_window_shortfall` (orchestrator.rs:791-806)

**Scope**: lines 785–806 only. Caller/callee semantics (units of `reported`,
`estimated_sent`, `window`; how the `bool` return is consumed) were **not**
read, so every proposal that depends on those is hedged.

---

## Proposal 1 — `wide_margin` dead zone for tiny `estimated_sent` (line 796)

```
796:    let wide_margin = shortfall >= (estimated_sent / 8).max(256);
```

When `estimated_sent < 256`, the right-hand side is 256, but `shortfall` ≤
`estimated_sent - 1` < 256, so `wide_margin` is **always false**. The
`clamped_boundary` path (line 797) could still catch a shortfall, but the
wide-margin path is silently dead for very small windows.

**Hedge**: I cannot tell whether sub-256 windows occur in practice for this
codebase; if callers guarantee `estimated_sent ≥ 1024` this is harmless.
If tiny windows *can* occur, consider:

```rust
let threshold = if estimated_sent < 256 {
    estimated_sent / 8  // fall back to the proportion, however small
} else {
    (estimated_sent / 8).max(256)
};
let wide_margin = shortfall >= threshold;
```

Or, more concisely, cap the threshold at `estimated_sent` so it can never
exceed the maximum possible shortfall:

```rust
let wide_margin = shortfall >= (estimated_sent / 8).max(256).min(estimated_sent);
```

(If `estimated_sent` is 0 the early guard on line 792 would have returned
false, but `estimated_sent` could still be 1..255.)

---

## Proposal 2 — misleading doc on `||` vs “AND” (lines 788-790)

```
788:	/// resolved window AND either miss the estimate by a wide margin (>= 1/8 of the
789:	/// estimate, minimum 256 tokens) or sit exactly on a power-of-two boundary of
790:	/// at least 2048 — the shape of a clamped server-side `num_ctx`.
```

The prose reads like **(below window) AND (wide-margin OR power-of-two)**.
That’s what the code implements on line 805:

```
805:    clamped_boundary || (wide_margin && plausible_clamp)
```

…but it also adds `plausible_clamp` (line 804) which is **not** mentioned in
the doc comment at all. A reader of only the doc would miss the quarter-window
squelch. The doc should mention the third conjunct, e.g.:

> “…AND the reported count is at least **one quarter of the resolved window**
> (otherwise the estimator likely overshot).”

---

## Proposal 3 — `plausible_clamp` name is inverted relative to the doc (lines 798-804)

```
798:    // A clamp hypothesis claims the server's real window IS `reported`. When
799:    // that implied window is under a quarter of the resolved one and sits on
800:    // no clamp-shaped boundary, the far likelier story is the chars/4
801:    // estimator overshooting an accurate tokenizer … Stay quiet rather than
802:    // crying wolf.
803:    let plausible_clamp = reported.saturating_mul(4) >= window;
```

The variable is called `plausible_clamp`, but it is **true** when the clamp
hypothesis is plausible (reported ≥ window/4) — which means we should *not*
stay quiet. The inline comment correctly describes the “stay quiet” case (under
a quarter), then the variable computes the opposite condition. Renaming to
`not_estimator_noise` or `above_quarter_window` would align the code with the
comment and remove the double-negative mental gymnastics for future readers.

---

## Proposal 4 — `saturating_mul(4)` on line 804 silently absorbs garbage

```
803:    let plausible_clamp = reported.saturating_mul(4) >= window;
```

If `reported > u64::MAX / 4` (roughly 4.6 × 10¹⁸ — unrealistic in normal
operation but possible with a corrupted provider response), the multiplication
saturates to `u64::MAX` and makes `plausible_clamp` unconditionally true,
bypassing the estimator-noise guard. A defensive `checked_mul` that falls back
to `false` (or logs a warning) would make corruption easier to spot.

**Hedge**: I don’t know whether the caller already validates `reported` against
a sane upper bound before passing it here. If it does, this is a non-issue.

---

**Summary**: the logic is careful and well-documented. The biggest practical
risk is Proposal 1 (tiny-window dead zone), though its real-world impact
depends entirely on caller constraints I did not read.
