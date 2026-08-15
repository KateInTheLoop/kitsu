# Cycle 13 review — `format_tool_result` (lines 1730-1794)

Scope: single ranged read of `crates/kitsu-core/src/orchestrator.rs:1730-1794`.
No caller or callee code was read; assertions about external behavior are hedged.

---

## 1. Silent truncation of `detail` without ellipsis or indicator (lines 1738-1741)

```
1738	                    .lines()
1739	                    .next()
1740	                    .unwrap_or("");
1741	                let detail: String = detail.chars().take(180).collect();
```

Two combined lossy operations: only the **first line** of `detail` is kept (line 1739), and that line is then hard-truncated to **180 chars** (line 1741). Neither cut appends an ellipsis or marker. A caller receiving `"error: timeout — connection to 10.0.0.1:443 was refused after retry 3/3 with backoff-ms=2000; last error: recv(ECONNREFUSED) on fd-19; pid-8842 exited code=1; recent session-msg-count=0; dns-resolution-cached=tr"` has no way to know the detail was truncated — it looks like a complete but garbled message.

**Proposal:** Append `…` (U+2026) when `detail.chars().count() > 180` or when `detail.lines().count() > 1`, e.g.:
```rust
let detail: String = detail.chars().take(180).collect();
let truncated = detail.chars().count() > 180 || …;
if truncated { format!("error: {kind} — {detail}…") } else { … }
```

---

## 2. `repo_grep` empty-content edge case produces "0 matches" (lines 1776-1782)

```
1776	        "repo_grep" => {
1777	            if content == "(no matches)" {
1778	                "no matches".to_string()
1779	            } else {
1780	                let n = content.lines().count();
1781	                format!("{n} match{}", if n == 1 { "" } else { "es" })
1782	            }
```

If the grep tool returns an **empty string** for no matches (rather than the literal `"(no matches)"`), line 1780 counts 0 lines and produces `"0 matches"` — ambiguous between "zero results" and "a buggy empty payload." **I cannot verify what the actual grep tool returns for a no-hit case without reading its implementation**, but if it emits `""` or `"\n"`, this branch would miss it. A `content.is_empty()` guard before line 1779 would remove the ambiguity.

**Proposal:** Add an early return for empty/whitespace-only `content`:
```rust
if content == "(no matches)" || content.trim().is_empty() {
    "no matches".to_string()
}
```

---

## 3. Default arm emits "0 lines" for empty content (lines 1789-1792)

```
1789	        _ => {
1790	            let n = content.lines().count();
1791	            format!("{n} line{}", if n == 1 { "" } else { "s" })
1792	        }
```

Any tool name not explicitly matched and returning an empty string (`""`) will render as `"0 lines"` — a degenerate and uninformative label. This could happen for new or unknown tools, or for tools that legitimately produce no output. A minimum bound (e.g., `"empty"` when `n == 0`) or an explicit `"0 lines (empty)"` string would be clearer.

**Proposal:** Special-case `n == 0`:
```rust
let n = content.lines().count();
if n == 0 {
    "empty".to_string()
} else {
    format!("{n} line{}", if n == 1 { "" } else { "s" })
}
```

---

## 4. `fs_write_file`/`fs_edit_file` fallback says "written" on JSON-parse failure (line 1775)

```
1756	        "fs_write_file" | "fs_edit_file" => serde_json::from_str::<serde_json::Value>(content)
1757	            .ok()
1758	            .map(|value| { … })
1775	            .unwrap_or_else(|| "written".to_string()),
```

If the tool result `content` is **not valid JSON** (e.g. a tool bug, a truncated write, or an unexpected error string), the `.ok()` on line 1756 yields `None` and line 1775 falls back to `"written"`. This is **potentially misleading**: a failed write whose result payload is malformed JSON would be reported to the orchestrator as though it succeeded. **I cannot verify what guarantees the write/edit tool makes about its result format** — it may always emit valid JSON on both success and error paths — but if it ever doesn't, this fallback silently papers over the failure.

**Proposal:** Distinguish the fallback:
```rust
.unwrap_or_else(|| "written (unparseable result)".to_string())
```

At minimum the label should not imply success when the result is unreadable.
