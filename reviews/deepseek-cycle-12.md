# deepseek-cycle-12: `parse_text_tool_calls` (orchestrator.rs:1443–1504)

## Proposal 1 — Malformed-detection substring match is fragile (correctness)
**Lines 1464–1467:**
```
1464	            if candidate.contains("\"name\"")
1465	                && descriptors
1466	                    .iter()
1467	                    .any(|d| candidate.contains(&format!("\"{}\"", d.name)))
```

`candidate.contains(&format!("\"{}\"", d.name))` performs a raw substring search on the unparsed JSON text. A quoted tool name appearing anywhere in the candidate — including inside a *value* string (e.g. `"description": "call the grep tool"`) or as a substring of a longer word — triggers the `malformed` counter. Short tool names are especially prone: a tool named `"ask"` would match inside `"task"`, `"mask"`, or any string containing those three letters. Likewise, `"\"name\""` on line 1464 matches even if the key lives in a nested object that isn't a tool-call envelope.

**Recommendation:** Parse the candidate as a generic `serde_json::Value` first (the existing `parsed` attempt already does this on line 1456). Inspect the parsed structure — only count it as malformed when the top-level value is an object, has a `"name"` key whose value is a string, and that string equals a registered descriptor name, but the full parse into the expected shape ultimately fails. This eliminates false positives from tool-name mentions in prose or nested payloads.

## Proposal 2 — `"arguments": null` produces the string `"null"` instead of `"{}"` (correctness)
**Lines 1487–1491:**
```
1487	        let args = match obj.get("arguments").or_else(|| obj.get("parameters")) {
1488	            Some(serde_json::Value::String(s)) => s.clone(),
1489	            Some(v) => v.to_string(),
1490	            None => "{}".to_string(),
1491	        };
```

When the model emits `"arguments": null` (or `"arguments": 42`, `"arguments": true`), line 1489 fires: `v.to_string()` on a `serde_json::Value::Null` produces the Rust string `"null"`. Downstream callers that parse `args` as JSON will see the literal token `null` — not the same as the default empty-object `"{}"` on line 1490. An omitted-arguments field (`None` arm) is handled correctly; an explicitly-null field is handled differently for no clear reason. The same applies to `"parameters"`. This asymmetry can cause callers to trip over `null` where they expect a JSON object string.

**Recommendation:** Treat `serde_json::Value::Null` the same as `None` (default to `"{}"`). For other non-string, non-object scalars (numbers, booleans), either default to `"{}"` as well, or at minimum log a warning so the unexpected shape surfaces in diagnostics rather than being silently stringified.

## Proposal 3 — `arguments` silently shadows `parameters` when both are present (clarity)
**Line 1487:**
```
1487	        let args = match obj.get("arguments").or_else(|| obj.get("parameters")) {
```

If a model emits both `"arguments"` and `"parameters"` keys, `or_else` takes `arguments` and the `parameters` value is discarded without any signal. This is not a correctness bug (picking one is reasonable), but it hides potentially-conflicting input that could help diagnose model confusion. _(Hedge: I cannot see whether callers or telemetry elsewhere surface this — it may be caught upstream. From this range alone, the discard is silent.)_

**Recommendation:** When both keys are present and non-equal, either log a warning or include a note in the returned `TextToolRecovery` so the caller can surface the ambiguity.

## Proposal 4 — No early exit when text contains zero registered tool names (performance)
**Lines 1455–1458:**
```
1455	    for candidate in extract_json_objects(text) {
1456	        let parsed = serde_json::from_str::<serde_json::Value>(&candidate).or_else(|_| {
1457	            serde_json::from_str::<serde_json::Value>(&relax_json_escapes(&candidate))
1458	        });
```

Every JSON blob extracted from `text` is fed through one or two `serde_json::from_str` calls, even when the input text mentions no registered tool name at all (common for free-form assistant answers). _(Hedge: I cannot see the implementation of `extract_json_objects` — it may already filter aggressively. If it returns every `{…}` block in prose-heavy text, the parse cost is non-trivial.)_

**Recommendation:** Before entering the candidate loop, scan `text` once for substring matches against registered descriptor names (or check that at least one descriptor name appears). If none appear, return `TextToolRecovery { calls: vec![], malformed: 0, skill_confusion: None }` immediately. This is the common case for non-tool answers and avoids serde overhead entirely.
