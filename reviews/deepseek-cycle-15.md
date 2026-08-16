# Cycle 15 review: `drop_old_exchanges_detailed` (lines 2915–2957)

**Scope**: `crates/kitsu-core/src/orchestrator.rs` lines 2915–2957 only.
**Caveat**: I have NOT read any caller, callee, or constant definition (`KEEP_RECENT_EXCHANGES`,
`user_message_indices`, `TRIM_MARKER`, `ChatMessage`, `size` closure origin, the
anchor/span helpers, or `force_all`'s callers). Proposals that depend on those are
explicitly hedged.

---

## 1. Panic risk at the last-exchange boundary when `KEEP_RECENT_EXCHANGES` could be zero

**Lines 2940 and 2951**:
```
2939	        let start = user_idxs[first_actual + e];
2940	        let end = user_idxs[first_actual + e + 1];
```
```
2950	    let drop_start = user_idxs[first_actual];
2951	    let drop_end = user_idxs[first_actual + e];
```

The guard at lines 2928–2930 returns `0` when `actual <= KEEP_RECENT_EXCHANGES`.
If `KEEP_RECENT_EXCHANGES == 0` and `actual > 0`, that guard does **not** trigger,
so `max_drop = actual`. On the last loop iteration (`e = actual - 1`):

- Line 2940 accesses `user_idxs[first_actual + actual]`, which is index `user_idxs.len()` → **out of bounds**.
- Even if the loop exits earlier (non-`force_all` path), line 2951 would access `user_idxs[first_actual + e]` with `e > 0`, which is still in bounds for `e <= max_drop`, but
  the loop body never runs when `max_drop == 0` (line 2938: `e < max_drop` is false
  at `e == 0`), so the `end` panic only fires when `force_all || total > target` and
  `max_drop ≥ 1`.

**Hedge**: I cannot see the definition of `KEEP_RECENT_EXCHANGES`. If it is **always ≥ 1**
at this callsite, no panic occurs today. If it is ever configurable or compile-time
zero, this is a latent off-by-one/index-out-of-bounds.

---

## 2. Returned count does not distinguish “dropped everything” from “met budget” on the `force_all` path

**Line 2956**:
```
2956	    u32::try_from(e).unwrap_or(u32::MAX)
```

When `force_all` is `true`, the loop runs until `e == max_drop` (lines 2938, 2931):
```
2931	    let max_drop = actual - KEEP_RECENT_EXCHANGES;
2938	    while (force_all || total > target) && e < max_drop {
```

After the loop, the function returns `e` (the count of dropped exchanges) with **no
signal** that the loop exhausted `max_drop` rather than satisfying the token target.
A caller that receives, say, `3` cannot know whether the budget is now met or
whether every droppable exchange was removed and the budget may still be exceeded.

**Hedge**: The callers of `drop_old_exchanges_detailed` are outside the lines I read.
They may inspect `messages.len()` or re-check `total` themselves after the call,
which would make this ambiguity benign. If the callers rely on the return value
*alone* for budget-satisfaction decisions, this is a semantic gap.

---

## 3. Marker token adjustment on first drop is unconditional but `total` may already be stale

**Lines 2942–2944**:
```
2942	        if e == 0 && !marker_present {
2943	            total = total.saturating_add(size(&ChatMessage::user(TRIM_MARKER)));
2944	        }
```

When the loop enters *solely* because of `force_all` (not `total > target`),
`total` is adjusted upward by the marker cost on the first iteration, then
further decremented by each exchange's tokens on line 2941:
```
2941	        total = total.saturating_sub(messages[start..end].iter().map(&size).sum::<u64>());
```

The resulting `total` is carried through the loop but is **never used** on the
`force_all` path after the loop exits. This is harmless (no downstream effect
for this range), but the adjustment + subtraction work is dead computation when
`force_all` is the sole driver. A reader may wonder whether `total` is later
consumed by something outside this function.

**Hedge**: If callers outside this range re-read `total` from the mutable reference
after the call, this “dead” computation on the `force_all` path is not dead at all.
If they ignore it, it is wasted work.

---

## 4. `e` is a `usize` incremented inside a `while` loop whose guard depends on `e` — safe but subtle

**Lines 2937–2946**:
```
2937	    let mut e = 0;
2938	    while (force_all || total > target) && e < max_drop {
2939	        let start = user_idxs[first_actual + e];
2940	        let end = user_idxs[first_actual + e + 1];
2941	        total = total.saturating_sub(messages[start..end].iter().map(&size).sum::<u64>());
2942	        if e == 0 && !marker_present {
2943	            total = total.saturating_add(size(&ChatMessage::user(TRIM_MARKER)));
2944	        }
2945	        e += 1;
2946	    }
```

The loop counter `e` is used **both** as the exit guard (`e < max_drop`) **and**
inside the body (`start = user_idxs[first_actual + e]` on line 2939). This is
correct today because `e` is only incremented at the very end (line 2945). However,
a future insertion of a `continue` or early `e += 1` between lines 2939 and 2945
would desynchronise the index from the iteration count. A small clarity improvement:
rename `e` to `dropped` and compute `start = user_idxs[first_actual + dropped]`
only once at the top, making the dual role obvious.

No functional defect — purely a maintainability note from the visible code.
