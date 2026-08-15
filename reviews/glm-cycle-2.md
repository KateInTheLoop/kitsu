# Cycle-2 review — `kitsu-providers/src/openai_compat.rs`

Scope: the four requested ranges (`build_body` ~180–260, `parse_completion` ~562–680,
`models_cache` usage ~1190–1240, `model_context_length` + `validate_models_catalog`
~1271–1360) plus the `Usage`/`WireRequest` definitions needed to ground the
proposals. Every quote was re-read from that exact range immediately before
citing. Ranked by value/risk; **P1** first.

---

## P1 — Surface `cached_tokens` / `prompt_tokens_details` from gateway usage

### Evidence
`Usage` (read from `types.rs`) carries only three fields:
```rust
pub struct Usage {
    #[serde(default)]
    pub prompt_tokens: u64,
    #[serde(default)]
    pub completion_tokens: u64,
    #[serde(default)]
    pub total_tokens: u64,
}
```
and is surfaced in both paths:
- Non-streaming, in `parse_completion`:
  ```rust
  if let Some(usage) = resp.usage {
      if !usage.is_empty() {
          out.push(ChatDelta::Usage(usage));
      }
  }
  ```
- The wire `usage` field is typed as `Option<Usage>` on `StreamChunk` (range ~230) and on the
  non-streaming `CompletionResponse` (consumed above).

### Problem
OpenAI and most compatible gateways (vLLM `--enable-prefix-caching`, the example AI gateway)
report prompt-cache hits as `usage.prompt_tokens_details.cached_tokens` (and, increasingly,
`completion_tokens_details.reasoning_tokens`). Because serde **deserializes only the fields
present on the struct**, every other key — including the entire `*_details` object — is
silently dropped. Kitsu therefore cannot tell a cached prompt from a paid one, and the
context-budget/telemetry that consumes `Usage` systematically understates true billed input
cost. For a CLI that manages a context window against token budgets, "cached" is a
first-class quantity, not a nice-to-have.

### Proposal (ranked, concrete)
1. Add optional fields with `#[serde(default)]` to `Usage`:
   ```rust
   pub struct Usage {
       #[serde(default)] pub prompt_tokens: u64,
       #[serde(default)] pub completion_tokens: u64,
       #[serde(default)] pub total_tokens: u64,
       #[serde(default)]
       pub prompt_tokens_details: Option<PromptTokenDetails>,
       #[serde(default)]
       pub completion_tokens_details: Option<CompletionTokenDetails>,
   }
   #[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
   pub struct PromptTokenDetails { #[serde(default)] pub cached_tokens: u64 }
   #[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
   pub struct CompletionTokenDetails { #[serde(default)] pub reasoning_tokens: u64 }
   ```
   This is contract-bearing (`types.rs` header: "AUTHORITATIVE CONTRACT … Changes here are a
   design decision"), so it ships with a doc note on `Usage` and at least one test vector per
   provider that emits these fields.
2. Keep `is_empty()` *unchanged* — `cached_tokens` is part of `prompt_tokens`, not separate,
   so the zero-check stays `prompt/completion/total == 0`. Avoids re-introducing a "ghost"
   non-empty Usage that the `if !usage.is_empty()` guard at line ~624 would otherwise drop.
3. Update `Usage::add` to sum `cached_tokens`/`reasoning_tokens` across multi-round turns so
   the aggregate shown in telemetry is correct.
4. Downstream (out of scope here, but noted): `kitsu-core`'s context-budget math should
   charge `prompt_tokens - cached_tokens` (or a provider-configurable discount) rather than
   the full prompt count, otherwise prefix caching looks free at the API but still consumes
   the local window budget.

### Risk
Low. Additive, `#[serde(default)]` keeps old gateways working; the only behavior change is
that *more* data surfaces, which is exactly what the guard at `if !usage.is_empty()` already
protects against surfacing spuriously.

---

## P2 — Cache parsed context length, not just the raw catalog

### Evidence
`model_context_length` re-parses the catalog body on **every** call:
```rust
let (status, text) = self.get_models_response().await.ok()?;
if !status.is_success() {
    return None;
}
let json: serde_json::Value = serde_json::from_str(&text).ok()?;
let entries = json.get("data")?.as_array()?;
```
`get_models_response` returns the cached `(StatusCode, String)` outcome (range ~1190–1240),
so the network call is deduplicated — but the `serde_json::from_str` + array scan + per-entry
`loosely_matches` walk runs on each invocation. `get_models_response` caches the *raw text*
with a 60s positive TTL (`MODELS_CACHE_TTL`), and the cache comment explicitly frames the
cache as serving "startup discovery, `/models`, and `/model`":
```rust
// One provider instance serves startup discovery, `/models`, and `/model`.
// Cache their shared raw catalog response so those paths do not issue
// duplicate requests or repeatedly stall on the same failed endpoint.
```

### Problem
`model_context_length` is called advisory during background discovery and is also on the
hot path whenever the context manager asks "what's the window for *this* model?" Re-parsing
the whole catalog (which for some gateways is large) on each call is wasted work, and the
loose-match scan is O(entries) per model. The raw-text cache already pays for the bytes; it
just doesn't pay for the parse.

### Proposal
1. Inside the positive-cache branch of `get_models_response` (or in a small
   `model_context_length` memo keyed by `(model, stored_at)`), cache the parsed
   `serde_json::Value` alongside the raw `(StatusCode, String)` in `ModelsCacheEntry`:
   ```rust
   struct ModelsCacheEntry {
       stored_at: tokio::time::Instant,
       ttl: Duration,
       outcome: Result<(StatusCode, String), ProviderError>,
       parsed: Option<serde_json::Value>, // Some only on validated success
   }
   ```
   `validate_models_catalog` (lines ~1344–1361) already proves the envelope is sound before
   the positive TTL is assigned, so storing the parsed value there is safe.
2. Alternatively (simpler, smaller blast radius): memoize
   `model_context_length` results per model in a second small mutex, invalidated whenever
   `models_cache` is repopulated. This keeps the raw-text cache as the single source of truth
   and avoids touching `ModelsCacheEntry`'s shape.
3. Either way, document that the parsed/memo cache is invalidated by the same TTL as the raw
   cache — do **not** introduce a second, longer TTL that could disagree with reality after a
   gateway-side model swap.

### Risk
Low–medium. Touches the cache shape and the locking story; the comment at ~1190 already
warns about stampede, so any change must preserve "hold the mutex across the request." A
per-model memo behind its own lock avoids disturbing that invariant.

---

## P3 — Tool-schema (`parameters`) is forwarded verbatim; add a fast-fail guard

### Evidence
`build_body` serializes tool descriptors straight through:
```rust
let tools = req.tools.as_ref().map(|ts| {
    ts.iter()
        .map(|t: &ToolDescriptor| WireTool {
            kind: "function",
            function: WireToolFn {
                name: &t.name,
                description: &t.description,
                parameters: &t.parameters,
            },
        })
        .collect()
});
```
with `WireToolFn.parameters: &serde_json::Value` (range ~140) and
`ToolDescriptor.parameters: serde_json::Value` (types.rs).

### Problem
`parameters` is forwarded with no validation. If it is `null`, a bare string, or a JSON
schema missing `type`, many OpenAI-compatible gateways reject the *entire* request with an
opaque 400 that surfaces to the user as a generic `InvalidResponse`. Worse, some gateways
(historically vLLM, and some proxy layers) silently accept a malformed schema and then
produce tool calls with missing/invalid argument structure — a failure mode far harder to
diagnose than a fast client-side rejection.

### Proposal
1. Cheapest, highest-leverage: in the `build_body` tool map, fast-fail when
   `t.parameters` is not an object/array (i.e., reject `null`/scalars) by returning a
   `Result` from `build_body` and mapping the failure to a
   provider error. **Caveat:** I did not read `error.rs` this cycle and cannot confirm which
   variants exist; `InvalidResponse` is the closest I *saw* used, but it is semantically a
   *response* error — a dedicated `InvalidRequest`/bad-tool variant is likely warranted. Confirm
   against `error.rs` before implementing. Keep the wire shape unchanged otherwise.
   ```rust
   fn build_body<'a>(req: &'a ChatRequest) -> Result<WireRequest<'a>, ProviderError> {
       …
       let tools = match req.tools.as_ref() {
           Some(ts) => {
               let mut out = Vec::with_capacity(ts.len());
               for t in ts {
                   if !t.parameters.is_object() {
                       return Err(ProviderError::InvalidResponse(format!(
                           "tool `{}` parameters must be a JSON object", t.name
                       )));
                   }
                   out.push(WireTool { … });
               }
               Some(out)
           }
           None => None,
       };
       …
   }
   ```
2. Optional deeper guard: require `parameters.type` to be `"object"` (the OpenAI tools
   contract), since a schema whose root isn't an object can't describe function arguments at
   all. This is stricter and risks rejecting some intentional edge schemas, so gate it
   behind a config flag or make it a warning rather than a hard fail.
3. Tests: add a vector with `parameters = Value::Null` and one with a non-object schema,
   asserting the error is raised before any HTTP call.

### Risk
Medium. Changing `build_body` to fallible ripples to all its callers (chat streaming +
non-streaming paths). Prefer the narrow object-check in (1) over the stricter (2); (2) can be
staged behind a flag. Document the contract on `ToolDescriptor.parameters` in `types.rs`
("must be a JSON Schema object") so the fast-fail isn't a surprise.

---

## P4 — (Minor) `parse_completion` "empty choice" detection conflates content and structural validity

### Evidence
```rust
let semantic_choice = choice.message.content
    .as_ref()
    .is_some_and(|text| !text.is_empty())
    || !choice.message.tool_calls.is_empty()
    || choice.finish_reason.is_some();
if !semantic_choice {
    return Err(ProviderError::InvalidResponse(
        "completion response contained an empty choice".to_string(),
    ));
}
```

### Problem
The check treats "no content, no tool calls, no finish_reason" as malformed. That is correct
for the common case, but a gateway that returns a *valid* empty assistant turn (content
elided, `finish_reason: "stop"`) passes because `finish_reason.is_some()` short-circuits —
which is fine — (Note: `content: null` or absent deserializes to `None`, and `content: ""` to `Some("")`; in
*both* cases `content.as_ref().is_some_and(|text| !text.is_empty())` is false, so the content
arm contributes nothing — the turn only passes via tool_calls or finish_reason.) The subtle
gap: a response with `content: ""`/null, no tool_calls, and a `null`/absent `finish_reason`
is rejected as "empty choice" even though some gateways
legitimately emit `content: ""` + `finish_reason` absent when they truncate. Worth a doc
comment clarifying the intended semantics so future readers don't "fix" it into a regression.

### Proposal
No code change required; add a comment that the guard is intentionally permissive
(`finish_reason` alone satisfies it) and that an empty-but-present `content` with no
`finish_reason` is treated as malformed *deliberately* (to surface provider bugs early). If
we later see a real gateway that emits `content: ""` + absent `finish_reason` legitimately,
promote this to a `warn`-and-continue rather than an error.

### Risk
None; documentation only.

---

## Cross-cutting notes (not ranked proposals)

- **No deep review of retry/timeout plumbing was done** — out of the requested ranges. The
  visible shape (`MAX_RETRY_DELAY`, `MAX_RETRY_JITTER`, `parse_retry_after` capping at 30s,
  `read_response_bounded` with Content-Length-as-hint) is sound on its face; a dedicated pass
  over the retry loop and the physical-attempt telemetry would be the next cycle.
- **`models_cache` negative-cache logic is well-considered**: 401/403 is deliberately *not*
  cached (`_ => None`), and connection/timeout/malformed is cached for 5s
  (`MODELS_NEGATIVE_CACHE_TTL`), which is the right call to avoid sticky auth failures.
- **`model_context_length` loose-match tie-break** (`(Some(only), None)`) is a nice
  defensive choice; the comment at ~1290 ("an ambiguous loose match … is worse than giving
  up") is exactly the right instinct for advisory data.