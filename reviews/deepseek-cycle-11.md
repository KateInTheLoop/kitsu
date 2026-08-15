# Code review: `format_command_result` (lines 1796‑1850)

## Scope
Single ranged read of `crates/kitsu-core/src/orchestrator.rs` ll. 1796‑1850.
Proposals are ranked by likely impact; every assertion about behaviour outside
this range is explicitly hedged.

---

### 1. `TimedOut` arm suspiciously matches `output.signal` — possible copy‑paste
**Lines 1806‑1809:**
```
1806	        CommandTermination::TimedOut => match output.signal {
1807	            Some(signal) => format!("timeout (signal {signal})"),
1808	            None => "timeout".to_string(),
1809	        },
```

The `TimedOut` arm destructures `output.signal`, exactly like the `Signaled`
arm above it (ll. 1802‑1805).  This is surprising: a timeout termination would
more naturally report a deadline or elapsed duration, not a signal.  **Hedge:**
I have not read the definitions of `CommandOutput` or `CommandTermination` and
cannot confirm whether `output.signal` is genuinely the right field — the
struct might reuse `signal` for “the signal we sent to implement the timeout.”
If the behaviour is intentional, add an inline comment explaining *why* signal
is the relevant datum.  If it is a copy‑paste error, replace it with a
timeout‑specific field (or at minimum drop the `(signal {signal})` suffix).

---

### 2. `elapsed_ms` has no bounding, unlike every other field
**Line 1812:**
```
1812	    let mut fields = vec![completion, format!("{} ms", output.elapsed_ms)];
```

Every other value rendered into `fields` is either passed through
`append_command_stream_summary` (which appears to bound stdout/stderr),
`bounded_human_value(…, 160)` (artifact error), `bounded_human_value(…, 480)`
(containment hint), or `terminal_safe_inline` (artifact path).  `elapsed_ms` is
the sole unbounded field.  **Hedge:** I do not know the type of `elapsed_ms`.
If it is a plain integer (`u64`/`i64`) the practical risk is low; if it is a
floating‑point or high‑precision duration type, a single field could produce a
surprisingly long decimal string (e.g. `12345678.987654321 ms`).  Consider
applying a decimal‑places cap or a `bounded_human_value`‑style guard here too.

---

### 3. `{:?}` debug formatting on `bounded_human_value(…)` likely adds spurious quoting
**Lines 1831‑1836 and 1843‑1848:**
```
1832	        fields.push(format!(
1833	            "artifact status={:?}",
1834	            bounded_human_value(status, 160)
1835	        ));
```
```
1844	        fields.push(format!(
1845	            "full output={:?}",
1846	            terminal_safe_inline(&artifact.reference)
1847	        ));
```

Both call‑sites use `{:?}` (Debug) on what are almost certainly string‑like
return values from the bounding helpers.  If `bounded_human_value` and
`terminal_safe_inline` return `String` or `&str`, `{:?}` will wrap the result
in extra quote characters, producing output like
`artifact status="some value"` instead of `artifact status=some value`.
**Hedge:** I have not read the signatures of either helper.  If they return a
type whose `Debug` is intentionally different from its `Display`, this is fine;
otherwise replace `{:?}` with `{}` for both.

---

### 4. Comment at ll. 1840‑1842 is valuable but could also note the “|”‑join contract
**Lines 1840‑1842:**
```
1840	    // The artifact path is the one pointer to the complete output, so it is
1841	    // never elided (every other field is bounded instead) and stays last:
1842	    // when a terminal wraps the receipt, the line ends with a copyable path.
```

This is an excellent bit of design documentation.  Consider extending it (or
adding a one‑liner above `fields.join(" | ")`) to state explicitly that **the
returned string is always a single line** and that the `" | "` separator is
part of the stable presentation contract — this helps future maintainers
understand that they must not introduce embedded newlines into any field.
