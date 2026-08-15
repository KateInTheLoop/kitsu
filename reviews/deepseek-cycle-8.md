# Review: `enforce_context_budget` (test wrapper)

**Scope:** `crates/kitsu-core/src/orchestrator.rs` lines 2074–2106.  
**Reviewed:** 2025-07-15, single ranged read of the cited lines only.  
**Status:** `#[cfg(test)]` — compiles only in test builds, not production.

---

## Summary

This is a thin test-facing wrapper around `enforce_context_budget_recording`. It
forwards all 8 caller-supplied arguments unchanged and hardcodes the remaining
4 arguments that the real function also requires: a fresh default
`CompactionLedger`, a fixed `CompactionReason::ContextTargetExceeded`, a
literal `1`, and a literal `false`. The doc comment accurately states its
purpose: exercise the production preflight without constructing a persistent
ledger.

---

## Proposals

### 1. Hardcoded `false` final argument — one code path only

> Line 2103: `false,`

The wrapper always passes `false` as the last positional argument to
`enforce_context_budget_recording`. **If** the callee uses that boolean to
select between two distinct behaviour branches (e.g. simulate vs. commit, dry
run, or "allow tool calls"), then every test that goes through this wrapper can
only exercise the `false` side — the `true` path gets zero coverage from the
convenience wrapper.

**Suggestion:** Either document at line 2074–2075 *which* mode is selected and
why it is sufficient for preflight, or add a parameter (or a second wrapper)
that lets a test opt into `true`.  Alternatively, if the callee ignores that
argument in practice, note that explicitly in the doc comment so a future
reader doesn't waste time tracing it.

### 2. Hardcoded `CompactionReason::ContextTargetExceeded` — only one reason

> Line 2102: `CompactionReason::ContextTargetExceeded,`

Every call through this wrapper is tagged with the same compaction reason.
**If** `enforce_context_budget_recording` branches on the reason (e.g. different
trimming strategies, different telemetry), then tests relying on this wrapper
never exercise those other branches.

**Suggestion:** Document that this wrapper is explicitly for the
context-budget-exceeded path (the name already implies it), and note that tests
for other reasons should call `enforce_context_budget_recording` directly. That
keeps the wrapper simple while preventing the false impression that it is a
general-purpose test entry point.

### 3. Name is one suffix away from the callee — discoverability hazard

> Line 2078: `async fn enforce_context_budget<P>(`  
> Line 2091: `enforce_context_budget_recording(`

The wrapper name drops the `_recording` suffix. In a codebase search,
`enforce_context_budget` will return both this `#[cfg(test)]` wrapper and
possibly comments/mentions of the production function. A reader who lands here
first might briefly mistake it for the real implementation.

**Suggestion:** Not a bug, but consider either (a) prefixing with `test_` so
the intent is unambiguous even without reading the attribute, or (b) adding a
one-liner to the doc comment at line 2074: *“This is a convenience wrapper; see
`enforce_context_budget_recording` for the production implementation.”*

### 4. The `#[allow(clippy::too_many_arguments)]` is on the wrapper, not the root cause

> Line 2077: `#[allow(clippy::too_many_arguments)]`

The wrapper has 8 parameters because the callee has at least 12. The lint
suppression is a symptom of the callee's signature, not the wrapper's design.
This is harmless but could be mildly misleading in a future refactor — someone
might think *this* function is the complex one.

**Suggestion:** Optional: move the `#[allow]` to the callee if it also needs
it, and refactor the wrapper's parameter list into a small struct or tuple if
the callee's signature ever changes. For now, document that the allow exists
solely because the wrapper mirrors the callee's already-large signature.
