# Review: `format_tool_header` (orchestrator.rs:1653–1726)

## 1. `spawn_agents`: count/labels mismatch when a task element is non-string (medium)

Lines 1692 and 1696–1701 build `count` from the raw array length but build `labels` by silently dropping non‑string elements:

```
1692	        let count = tasks.map_or(0, Vec::len);
1693	        let labels = tasks
1694	            .map(|tasks| {
1695	                tasks
1696	                    .iter()
1697	                    .filter_map(|task| task.as_str())
1698	                    .map(|task| format!("\"{}\"", bounded_human_value(task, 48)))
1699	                    .collect::<Vec<_>>()
1700	                    .join(", ")
1701	            })
1702	            .unwrap_or_default();
1703	        return format!("{friendly}(x{count}: {labels})");
```

If `tasks` is `["alpha", 42, "beta"]`, `count` is 3 but labels renders `"alpha", "beta"` (two entries). The header then reads e.g. `Agent(x3: "alpha", "beta")` — a confusing off-by-one appearance. The count should reflect the number of *displayable* labels, or non‑string elements should be rendered as `?` placeholders so the count stays truthful.

## 2. `spawn_agents`: no overall length bound on the labels aggregate (low)

The per‑task bound at line 1698 (`bounded_human_value(task, 48)`) limits each individual task description, but the joined string at line 1700–1703 has no total length cap. A large batch (say 20 tasks) produces a very long header line that can dominate the deck:

```
1698	                    .map(|task| format!("\"{}\"", bounded_human_value(task, 48)))
...
1703	        return format!("{friendly}(x{count}: {labels})");
```

Consider adding an overall truncation guard (e.g. “x12: "do X", "do Y", … (+9 more)”) before the final format.

## 3. `spawn_agents`: non‑array `tasks` value yields a bare “x0:” (low)

When the `"tasks"` key is present in the JSON but its value is *not* an array (e.g. a string or null), `tasks` becomes `None` and `count` is 0 at line 1692, while `labels` is `""` at line 1702. The output is e.g. `Agent(x0: )`:

```
1691	        let tasks = v.get("tasks").and_then(|x| x.as_array());
1692	        let count = tasks.map_or(0, Vec::len);
...
1702	            .unwrap_or_default();
1703	        return format!("{friendly}(x{count}: {labels})");
```

This is an honest representation of an invalid payload, but an explicit `"(invalid tasks)"` fallback (similar to the `command_exec` arm at line 1662) would make the anomaly immediately obvious in the deck rather than looking like a zero‑task dispatch.

## 4. `agent_result`: float `id` silently renders as `?` (info / hedge)

Lines 1707–1710:

```
1706	        let id = v
1707	            .get("id")
1708	            .and_then(serde_json::Value::as_u64)
1709	            .map(|id| id.to_string())
1710	            .unwrap_or_else(|| "?".to_string());
```

`as_u64` returns `None` for a JSON float (e.g. `1.0`). In practice the agent‑tools always produce integer IDs, so this is harmless — but I **cannot verify the callers** from this range alone, and if any code path feeds a float `"id"` the header silently degrades to `AgentResult(#?)`. A fallback through `as_f64` → `as_u64` or an explicit `"(bad id)"` would guard that path, but it may be unnecessary given the known callers.
