# ISSUE-006 — Only the first text-protocol tool call in a turn is executed

- **Status:** RESOLVED (fixed + regression-tested; verified in loop iteration 1)
- **Severity:** Medium-High (breaks multi-file generation — a core coding workflow)
- **Found by:** dogfooding, Stage 1 (persona `power_01`: "write 3 files")
- **Area:** `kitsu-core` orchestrator (`parse_text_tool_call`)

## Symptom
Asked to generate a 3-file project, the model emitted three `fs_write_file`
tool calls as text in ONE message:

```
{"name":"fs_write_file","arguments":{"path":".../main.py", ...}}
{"name":"fs_write_file","arguments":{"path":".../calc.py", ...}}
{"name":"fs_write_file","arguments":{"path":".../test_calc.py", ...}}
```

`parse_text_tool_call` extracts only the **first** balanced JSON object, so at most
one call runs. (In this run the first object was also malformed JSON — the model
emitted `'\''` and double-escaped `\\n` — so `serde_json` rejected it and **zero**
files were written; the next turn's "read calc.py" then failed.) Net: multi-file
generation silently produced nothing.

## Root cause
- `parse_text_tool_call` returns a single `ToolCall` from the first `{…}` only.
- Structured (non-text) providers already return multiple `tool_calls`; the loop
  runs them all. The text-protocol path is the gap.

## Proposed fix
- Parse **all** balanced top-level JSON objects from the assistant text (scan
  forward past each closing brace), keep those whose `name` matches an advertised
  tool, and return `Vec<ToolCall>` — the loop already executes a vec.
- Give each a distinct `call_id` (`textcall-0`, `-1`, …).
- Tolerate minor model JSON quirks where safe, but do NOT silently "fix" content;
  a malformed call should surface as a tool error fed back (so the model retries),
  not vanish. (Consider: if no object parses but tools were advertised and the
  text is clearly call-shaped, feed back a "your tool call was malformed" tool
  result to prompt a retry.)

## Verification
- Unit: a turn whose text contains 3 valid `fs_write_file` JSON objects runs 3
  tool calls (extend the ISSUE-001 tests).
- Live: re-run `power_01` → main.py/calc.py/test_calc.py all created.
