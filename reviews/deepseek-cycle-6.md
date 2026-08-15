# compact_context review — orchestrator.rs lines 2041–2077

## Proposal 1 — Don't silently swallow tool-descriptor serialization errors (🔴 high)

**Location**: line 2056

```rust
let schema_json = serde_json::to_string(tools.descriptors()).unwrap_or_default();
```

`unwrap_or_default()` turns a serialization failure into the empty string `""`, which is then passed straight into `enforce_context_budget_recording`. A broken schema (e.g. a custom tool descriptor that fails `Serialize`) would silently become an empty tools list in the compaction preflight, causing the model to see zero tools with no indication anything went wrong.

**Recommendation**: either propagate the error (return `Err(AgentError::Serialization(...))`) or at minimum emit a `tracing::warn!` with the error before falling back to `""`.

---

## Proposal 2 — `usage` local is allocated but never surfaced (🟡 medium)

**Location**: line 2057

```rust
let mut usage = kitsu_providers::Usage::default();
```

This is passed by `&mut` into `enforce_context_budget_recording` (line 2058–2076), which likely populates token counts, but the function then discards `usage` when it returns the `CompactionReport`. If the caller or the report should reflect what the compaction attempt actually consumed, this data is lost.

**Recommendation**: fold the usage into the returned `CompactionReport` (add a field), or confirm it's genuinely not needed and replace with `&mut Usage::default()` inline to make the intent explicit.

---

## Proposal 3 — Magic number for retry count (🟡 medium)

**Location**: line 2067

```rust
        1,
        CompactionReason::ManualRequest,
```

The literal `1` is the retry count passed as the second-to-last positional argument. It's unclear whether "1" means one attempt total or one retry. A named constant or a call-site comment would make this self-documenting.

**Recommendation**: extract to a named local, e.g. `const COMPACT_RETRIES: u32 = 1;`, and pass that instead.

---

## Proposal 4 — Accept `&Registry` when only descriptors are needed (🟢 low)

**Location**: line 2042 (`tools: &Registry`) and lines 2055–2056

```rust
    let has_schema = !tools.descriptors().is_empty();
    let schema_json = serde_json::to_string(tools.descriptors()).unwrap_or_default();
```

Both calls reach through to `Registry::descriptors()` and nothing else on `tools` is used. Narrowing the parameter to `&[ToolDescriptor]` would reduce coupling and make the function's real dependencies explicit (the caller precomputes descriptors once if needed).

**Recommendation**: change the signature to accept `descriptors: &[ToolDescriptor]` and have the caller provide them.
