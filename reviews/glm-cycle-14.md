# Cycle 14 Review — `parse_completion` (openai_compat.rs)

**Scope:** `parse_completion`, lines 599–658 of `crates/kitsu-providers/src/openai_compat.rs`.
**Read to verify proposals:** the function body; its wire types
`CompletionResponse` (307–313), `CompletionChoice` (315–321),
`CompletionMessage` (323–329), `CompletionToolCall` (331–337);
`finish_from_str` (339–345); the parallel streaming decoder `SseDecoder`
(448–576) for cross-path consistency; and `Usage`/`ChatDelta` in
`types.rs` (136–172). The `FinishReason` enum definition was **not** read —
propositions depending on its variants are hedged.

---

## P0 — `"error": null` is rejected here but ignored by the streaming path

**Evidence.** The non-streaming guard at line 602 is:

```
602	    if value.get("error").is_some() {
```

It treats *any* `error` key — including JSON `null` — as a hard failure and
returns the fixed string at line 604:

```
603	        return Err(ProviderError::InvalidResponse(
604	            "provider returned an error object in a successful completion response".to_string(),
605	        ));
```

The streaming decoder handles the same wire shape with an explicit null
filter and a comment explaining why:

```
466	        if let Some(err) = value.get("error").filter(|e| !e.is_null()) {
```

```
470	            // is NOT an error and must be ignored (some gateways emit `"error": null`).
```

**Impact.** A gateway that emits `"error": null` on a *successful*
non-streaming completion (the exact class the SSE comment at line 470 calls
out) is accepted when streamed but rejected with a misleading "provider
returned an error object" message when not streamed. The two paths share one
wire shape (per the module doc comment, lines 1–3) so they must agree on
`null`-error handling. This is the highest-value fix: it is a verifiable
divergence between two code paths over the same input, with a concrete
failure mode on real gateways.

**Fix.** Mirror the streaming guard:

```rust
if value.get("error").filter(|e| !e.is_null()).is_some() {
```

---

## P1 — Degenerate tool calls (empty name + id) are emitted unconditionally

**Evidence.** The semantic-choice check at line 617 counts any non-empty
`tool_calls` vector as semantic, even a single entry whose fields are all
`null`:

```
617	                || !choice.message.tool_calls.is_empty()
```

The loop at lines 628–637 then emits *every* entry, substituting empty
strings for any missing field:

```
628	            for tc in choice.message.tool_calls {
629	                let f = tc.function.unwrap_or(DeltaToolCallFn {
630	                    name: None,
631	                    arguments: None,
632	                });
633	                out.push(ChatDelta::ToolCall(ToolCall {
634	                    tool_name: f.name.unwrap_or_default(),
635	                    args: f.arguments.unwrap_or_default(),
636	                    call_id: tc.id.unwrap_or_default(),
637	                }));
```

`CompletionToolCall` (lines 331–337) has `id: Option<String>` and
`function: Option<DeltaToolCallFn>`, both `#[serde(default)]`, so a wire
object `{}` deserializes cleanly and reaches the loop. The result is a
`ChatDelta::ToolCall` with `tool_name: ""`, `args: ""`, `call_id: ""`.

The streaming path guards the same emission at lines 566–570:

```
566	        for acc in std::mem::take(&mut self.accs) {
567	            if !acc.name.is_empty() || !acc.id.is_empty() {
568	                out.push(ChatDelta::ToolCall(acc.into_tool_call()));
569	            }
570	        }
```

**Impact.** Because the `tool_calls` field is `Vec` with `#[serde(default)]`
(lines 327–328), an untrusted gateway response such as
`{"choices":[{"message":{"tool_calls":[{}]}}]}` passes the semantic check at
line 617 (the vector is non-empty) and produces a fully empty `ToolCall`.
The streaming path skips the fully-empty case; this path does not. A
downstream tool dispatcher would receive a call to a tool named `""`.

**Hedge.** I did not read the code that consumes `ChatDelta::ToolCall`
downstream, so I cannot confirm it mishandles an empty `tool_name` — but the
*inconsistency between the two paths* is verifiable from what I read, and the
non-streaming path is strictly less defensive.

**Fix.** Before line 633, skip entries where both name and id resolve to
empty, matching the streaming guard at line 567. A stricter option (require
a non-empty name) would diverge from streaming, so at minimum achieve parity.

---

## P2 — Tool-call `arguments` defaults to `""`, which is not valid JSON

**Evidence.** Line 635:

```
635	                    args: f.arguments.unwrap_or_default(),
```

`f.arguments` is `Option<String>` (line 304). `unwrap_or_default()` yields
`""` when the gateway omits `arguments` or sends `null`. An empty string is
not valid JSON; the conventional empty-arguments payload is `{}`.

**Impact.** If the gateway returns a tool call with `function: {name:
"foo"}` and no `arguments` key (legal under `#[serde(default)]` at line
302/304), the emitted `ToolCall.args` is `""`. Any downstream code that
parses `args` with `serde_json::from_str` would fail on `""` but succeed on
`{}`.

**Hedge.** I did not read the downstream tool-dispatch path, so I cannot
confirm it parses `args` as JSON rather than passing the raw string through.
The streaming path accumulates argument fragments the same way (line 536,
`acc.args.push_str(&args)`) and also defaults to `""`, so *both* paths share
this behavior — the proposal is about correctness of the default, not a
divergence between paths. If downstream already tolerates `""`, this is
lower priority than the rank suggests.

**Fix (if downstream expects JSON).** Replace `unwrap_or_default()` with
`unwrap_or_else(|| "{}".to_string())` at line 635.

---

## P3 — `finish_from_str` collapses unknown finish reasons to `Stop`

**Evidence.** Lines 339–345:

```
339	fn finish_from_str(s: &str) -> FinishReason {
340	    match s {
341	        "length" => FinishReason::Length,
342	        "tool_calls" => FinishReason::ToolCalls,
343	        _ => FinishReason::Stop,
344	    }
345	}
```

The wildcard arm at line 343 maps every unrecognized `finish_reason` to
`Stop`. OpenAI-compatible APIs can return `"content_filter"` (and other
values), which would be silently relabeled as a normal stop. This function
is shared: `parse_completion` calls it at line 641, and the streaming path
calls it at line 541.

**Hedge.** I did **not** read the `FinishReason` enum definition, so I
cannot assert whether a `ContentFilter` variant exists to map to. If the
enum only has `Stop`/`Length`/`ToolCalls`, the current collapse is an
inherent limitation of the type, not a bug in this function. The proposal
is conditional on the enum offering (or being extended to offer) a more
precise variant; without that, a fix here is impossible.

**Impact (conditional).** A `content_filter` finish is semantically
distinct from a model that chose to stop — callers may want to surface it
differently (e.g., avoid retrying, flag to the user). Collapsing it to
`Stop` loses that signal. Because both paths share `finish_from_str`, the
fix would improve both at once.

**Fix (if the enum supports it).** Add an explicit arm for
`"content_filter"` (and any other variant the enum defines) before the
wildcard, so only truly unrecognized strings fall through to `Stop`.

---

## Notes on things verified as correct

- **Partial moves compile.** `choice.message.content` is moved at line 624
  and `choice.message.tool_calls` at line 628; `choice.finish_reason` is
  then borrowed via `.as_deref()` at line 640. These are distinct fields of
  `choice` / `choice.message`, so Rust's partial-move rules permit it.
- **Usage handling matches the streaming path.** Lines 651–655 use
  `Usage::is_empty()` (defined at types.rs 148–150, which checks all three
  token fields are zero) to suppress an all-zero `Usage`, matching the SSE
  decoder at lines 488–491.
- **Empty-choices branch is sound.** When `choices` is absent or empty
  (`Vec` with `#[serde(default)]` at lines 309–310), `.next()` returns
  `None` and the `None` arm at lines 644–648 returns a clear error.