# ISSUE-001 — Tool calls emitted as text are not executed (breaks file generation with local models)

- **Severity:** High (blocks the file-generation / coding workflow on local models)
- **Found by:** dogfooding smoke test, Stage 0 (`chat --allow-write --script` against local Ollama `qwen2.5-coder`)
- **Area:** `kitsu-core` orchestrator / `kitsu-providers` tool-call handling

## Symptom
Asking the model to generate a file with `fs_write_file` produces, as the
assistant's **text response**:

```
{"name": "fs_write_file", "arguments": {"path": ".../hello.py", "content": "print(\"Hello, world!\")"}}
```

kitsu treats this as ordinary assistant text, prints it, and ends the turn. **No
tool runs; no file is written.**

## Root cause
The model returns the tool call as `content` text with `tool_calls: null` and
`finish_reason: "stop"`. Confirmed at the source, not a kitsu parsing bug:

- Ollama OpenAI-compat (`/v1/chat/completions`): `content` = the JSON, `tool_calls: null`.
- Ollama **native** (`/api/chat`): same — `content` = the JSON, `tool_calls: null`.
- `qwen2.5-coder:latest` advertises the `tools` capability, yet its template
  emits tool calls as plain JSON text rather than the structured field.

Many local models (via Ollama) behave this way. kitsu currently only executes
**structured** `tool_calls`; a text-form call is invisible to the orchestrator.

## Impact
- **File generation** (the key coding feature) does not work with local models
  that emit text-form tool calls — i.e. a large share of Ollama models.
- Read tools (`fs_read_file`, etc.) have the same limitation with these models.
- Capable hosted models that emit structured `tool_calls` are unaffected (the
  write tool + policy are correct and unit-tested).

## Proposed fix — text-protocol tool-call fallback
When a turn returns **no** structured tool calls, attempt to parse the assistant
text as a tool call and, if it matches an advertised tool, execute it — the
"text-protocol" mode anticipated in the original design (`docs/architecture.md`
referenced a tool-protocol/text-protocol format).

Sketch:
- After collecting the assistant message, if `tool_calls` is empty, scan the
  text for a JSON object (bare, or inside a ```json fence, or a
  `<tool_call>…</tool_call>` block — the common local-model shapes) with
  `name` + `arguments`/`parameters`.
- If `name` matches a registered descriptor, synthesize a `ToolCall`, run it,
  feed the result back, and continue the loop (exactly as for structured calls).
- Guard: only when tools are advertised; ignore non-matching JSON; cap attempts
  per turn to avoid loops.

## Verification plan
- Unit: orchestrator round where the mock provider returns text-form tool JSON →
  the tool executes and the result is fed back.
- Live: re-run the file-generation scenario against local Ollama → `hello.py` is
  actually created.
- Regression: keep a scenario in the catalog that asserts the file exists.
