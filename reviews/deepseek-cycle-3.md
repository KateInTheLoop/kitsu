# Cycle 3 Review: `request_measurement` + `check_measurement`

**Scope**: `crates/kitsu-core/src/orchestrator.rs` lines 1954–2012 (~58 lines)
**Reviewed**: `request_measurement` (L1954–1999) and `check_measurement` (L2001–2012)

---

## Proposal 1 — [Correctness · Medium] `check_measurement` gates on `target`, potentially ignoring the max-tokens reservation

`check_measurement` tests `used > target` (L2002):

> `if measurement.used > measurement.target {`

In the token path `target` is populated via `ContextBudget::new(window, max_tokens)` (L1976). If `ContextBudget::target` returns the *full* window rather than `effective` (i.e. `window - max_tokens`), then a request whose `used` fits within `target` but exceeds `effective` passes this guard — leaving zero token budget for the response. The `effective` field is computed and carried in `RequestMeasurement` (L1981) but never consulted by the guard.

**Recommendation**: When `measurement.effective` is `Some`, check `used > effective` instead of (or in addition to) `used > target`. If `target` is already the effective window, rename `effective` to avoid confusion and document the contract on `ContextBudget`.

---

## Proposal 2 — [Correctness · Low] Byte-path silently discards `max_tokens`

In the token path the `max_tokens` parameter is threaded into `ContextBudget::new(window, max_tokens)` (L1976), shrinking the effective budget. In the byte path (`cfg.context_window` is `None`, L1987–1998) the parameter is entirely unused:

> `target: cfg.max_context_bytes as u64,`

A caller that sets `max_tokens: Some(4096)` but omits a token window gets no reservation and may blow past the model's output limit. The asymmetry is implicit and undocumented.

**Recommendation**: Either (a) document that `max_tokens` is a no-op without a context window, or (b) subtract a byte estimate of `max_tokens` from `target` in the byte path for consistency. Option (a) is safer short-term; option (b) would need a bytes-per-token heuristic.

---

## Proposal 3 — [Clarity] `schema_json` is a dead parameter when `has_schema` is false

The signature (L1954–1958) requires both `schema_json: &str` and `has_schema: bool`:

> `fn request_measurement(`
> `    messages: &[ChatMessage],`
> `    cfg: &AgentConfig,`
> `    schema_json: &str,`
> `    has_schema: bool,`

When `has_schema` is false, `schema_json` is never read (the `if has_schema` guards at L1966 and L1988 prevent it). This forces every caller to conjure a dummy `&str` for the no-schema case — a minor API smell.

**Recommendation**: Collapse into `schema_json: Option<&str>`. The two guards become `if let Some(s) = schema_json`. Callers pass `None` when there is no schema, eliminating the dummy value and making the intent explicit at every call site.

---

## Proposal 4 — [Simplification · Low] Two-path branching duplicates `RequestMeasurement` construction

The `if let Some(window)` / `else` branches (L1965–1999) each build a full `RequestMeasurement` struct, repeating the field names and order. The only differences are the arithmetic for `used`, `target`, `window`, and `effective`. A single `RequestMeasurement { … }` construction with conditional expressions for those four fields would halve the physical struct literals and make the structural identity of both paths obvious.

**Recommendation**: Compute `(used, target, window, effective, unit)` in a small match/if-let that binds locals, then construct one `RequestMeasurement` at the bottom. This also makes it harder for the two paths to drift out of sync on fields like `unit`.

---

## Summary

| # | Severity | Area |
|---|----------|------|
| 1 | Medium | `check_measurement` may check full window instead of effective budget |
| 2 | Low | `max_tokens` silently ignored in byte path |
| 3 | Low | Redundant `has_schema` bool / dead `schema_json` parameter |
| 4 | Low | Duplicated struct construction across two branches |
