# ISSUE-002 — Raw tool-call JSON is echoed to the user before it's recognized

- **Status:** OPEN
- **Severity:** Medium (ugly in coding workflows; every tool-using turn on a
  text-protocol model shows raw JSON)
- **Found by:** dogfooding, Stage 1 (scn_02/04/05/06 against local Ollama)
- **Area:** `kitsu-core` orchestrator + streaming render

## Symptom
When a model emits a tool call as text (the ISSUE-001 case), the raw JSON is
printed to the transcript before kitsu recognizes and runs it, e.g.:

```
{"name": "fs_write_file", "arguments": {"path": ".../greet.py", "content": "print(\"hi\")"}}File "..." has been created...
```

The tool *does* run (correct), but the user sees the tool-call JSON as if it were
the answer. In `--no-stream` mode the synthesized turn's returned text is also the
raw JSON.

## Root cause
- Streaming: `print_fragment` prints text deltas live, so the JSON is on screen
  before the loop inspects it for a text-protocol call.
- The synthesized turn's returned `last_assistant_text` is the raw JSON, so the
  non-streaming path prints it too.

## Proposed fix
- **Non-streaming:** when a text-protocol call is synthesized, don't surface the
  raw JSON — return the model's *final* (post-tool) text instead (already the last
  round's text), and keep assistant history content empty (already done).
- **Streaming:** when tools are advertised, detect a tool-call shape early — if the
  streamed text (trimmed) begins with `{`, a ```` ```json ```` fence, or
  `<tool_call>`, buffer it instead of printing live; on turn end, if it parsed as a
  tool call, print nothing (run it silently) — otherwise flush the buffered text.
  This keeps live streaming for normal answers and hides only tool-call JSON.

## Verification
- Re-run scn_04/scn_06: the transcript shows the tool's effect/confirmation, not
  the raw JSON.
- Unit: a streaming turn whose text is a tool call emits no `print_fragment` for
  the JSON (assert via a capture hook or the returned text).
