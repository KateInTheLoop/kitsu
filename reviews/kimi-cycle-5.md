# Cycle-5 review: usage parsing & caching (kitsu-providers)

Scope: `crates/kitsu-providers/src/types.rs` (172 lines) and
`crates/kitsu-providers/src/openai_compat.rs` (1694 lines) only. Line numbers
refer to those files as read.

## (a) Where usage is parsed, what is dropped, and how to surface `cached_tokens`

### Parse sites

The `usage` payload is deserialized directly into the shared `Usage` struct at
exactly two wire types in `openai_compat.rs`:

1. **Streaming**: `StreamChunk.usage` — `openai_compat.rs` ~lines 283–294:
   ```rust
   #[serde(default)]
   usage: Option<Usage>,
   ```
   captured in `SseDecoder::process_line` (~lines 430–436):
   ```rust
   if let Some(u) = chunk.usage {
       if !u.is_empty() {
           self.usage = Some(u);
       }
   }
   ```
   and emitted in `SseDecoder::finalize` (~lines 488–491) as
   `ChatDelta::Usage(usage)` immediately before `ChatDelta::Done`.

2. **Non-streaming**: `CompletionResponse.usage` — ~lines 337–342, surfaced in
   `parse_completion` (~lines 590–595) with the same `is_empty()` gate, again as
   `ChatDelta::Usage` before `Done`.

Both paths deserialize into `types.rs` `Usage` (~lines 129–140):

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub prompt_tokens: u64,
    #[serde(default)]
    pub completion_tokens: u64,
    #[serde(default)]
    pub total_tokens: u64,
}
```

### What is dropped

Because `Usage` has no `prompt_tokens_details` field and serde ignores unknown
fields by default, **everything else in the wire `usage` object is silently
discarded**, including:

- `prompt_tokens_details.cached_tokens` (returned by the HF router, OpenAI,
  DeepSeek, vLLM with prefix caching, etc.)
- `completion_tokens_details.reasoning_tokens`
- any gateway-specific cost fields.

The drop happens at the `serde_json::from_value(value)` calls in
`process_line` (~line 415) and `parse_completion` (~line 566) — the nested
object is parsed into `serde_json::Value` and then thrown away by the typed
conversion.

A second, subtler drop: the `is_empty()` gates (`types.rs` ~lines 144–148;
used at `openai_compat.rs` ~lines 432 and ~591) treat an all-zero triple as
"not reported". A gateway that reports *only* `cached_tokens` (e.g. a full
prefix-cache hit billed as 0 prompt tokens) is currently indistinguishable
from silence and its usage chunk is discarded entirely.

### Proposed edits (line-cited)

1. **`types.rs`, after line ~140 — extend `Usage`:**

   ```rust
   /// Nested prompt-token breakdown reported by some gateways (OpenAI,
   /// HF router, vLLM). Absent when the gateway does not report it.
   #[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
   pub struct PromptTokensDetails {
       #[serde(default)]
       pub cached_tokens: u64,
   }
   ```
   and add to `Usage`:
   ```rust
   #[serde(default)]
   pub prompt_tokens_details: Option<PromptTokensDetails>,
   ```
   `Usage` stays `Copy` (the nested struct is `Copy`), so no call-site
   ownership changes are needed.

2. **`types.rs` `impl Usage` (~lines 142–161):**
   - Add an accessor so downstream crates never touch the nested option:
     ```rust
     pub fn cached_tokens(&self) -> u64 {
         self.prompt_tokens_details.map_or(0, |d| d.cached_tokens)
     }
     ```
   - Extend `add()` (~lines 156–160) to sum the breakdown across multi-round
     turns (this is the exact aggregation point kitsu-core uses per turn):
     ```rust
     pub fn add(&mut self, other: Usage) {
         self.prompt_tokens += other.prompt_tokens;
         self.completion_tokens += other.completion_tokens;
         self.total_tokens += other.total_tokens;
         let cached = self.cached_tokens() + other.cached_tokens();
         self.prompt_tokens_details = Some(PromptTokensDetails { cached_tokens: cached });
     }
     ```
     (Setting `Some` unconditionally is fine: a zero sum serializes as 0 and
     preserves the existing "all-zero means unreported" convention.)
   - Deliberately **do not** change `is_empty()` to include `cached_tokens`:
     a cache-hit-only chunk would then mark usage as "reported" while
     prompt/completion stay 0, breaking the documented contract at
     ~lines 125–128. Instead, if cache-only chunks matter, gate on
     `!u.is_empty() || u.cached_tokens() > 0` at the two capture sites
     (`openai_compat.rs` ~lines 432 and ~591).

3. **`openai_compat.rs`: no parse-site changes required.** `StreamChunk.usage`
   (~line 291) and `CompletionResponse.usage` (~line 340) deserialize into
   `Usage` directly, so the nested field flows through automatically once the
   struct grows it. Only the optional `is_empty` gate tweak above touches this
   file.

4. **Into kitsu-usage records (interface note):** `ChatDelta::Usage(Usage)`
   (`types.rs` ~line 168) already carries the full struct to the consumer, so
   kitsu-core's per-turn aggregation (`Usage::add`) and the kitsu-usage record
   writer need only read `usage.cached_tokens()` and store it as a new
   `cached_tokens: u64` field. No provider-trait signature change is needed —
   the type change is additive and backward-compatible on the wire
   (`#[serde(default)]`).

## (b) Further ranked caching proposals (grounded only in these two files)

### 1. (Highest) Send a cache key / enable prefix-cache affinity on requests

`WireRequest` (`openai_compat.rs` ~lines 107–126) sends exactly
`model/messages/tools/stream/stream_options/temperature/reasoning_effort/
max_tokens` — see `build_body` (~lines 250–262). There is no
`prompt_cache_key` (OpenAI/HF-router supported) or any cache-affinity hint, so
gateways that shard their prompt cache by an explicit key cannot be leveraged,
and retries (`chat_stream_with_telemetry` retry loop, ~lines 1290–1500) and
multi-round tool turns re-send the identical long prefix with no affinity
signal. **Proposal:** add `pub prompt_cache_key: Option<String>` to
`ChatRequest` (`types.rs` ~lines 89–102) and a matching
`#[serde(skip_serializing_if = "Option::is_none")]` field on `WireRequest`,
populated by the caller (e.g. session id). This directly increases the hit
rate of the very `cached_tokens` metric proposed in (a). Value: high; cost:
two additive fields.

### 2. Make cache-hit-only usage chunks survive the `is_empty()` gate

As noted in (a), `SseDecoder::process_line` (~lines 430–436) and
`parse_completion` (~lines 590–595) drop usage where all three counters are
zero. On gateways with aggressive prefix caching, a full-cache-hit completion
can legitimately report `prompt_tokens: 0` with nonzero
`prompt_tokens_details.cached_tokens`; today that signal vanishes. **Proposal:**
after edit (a).1 lands, change both gates to
`if !u.is_empty() || u.cached_tokens() > 0`. Value: medium-high (correctness
of cache telemetry); cost: two one-line edits. Ranked below (1) because it
only matters once (a) exists.

### 3. Cache the *parsed* per-model context length, not just the raw catalog body

`models_cache` (~lines 40–49, TTLs at ~lines 61–63) caches the raw catalog
text, and `model_context_length` (~lines 1200–1250) re-parses the full JSON
and re-runs the exact/loose-match scan on every call. The cache lock is even
held across the fetch (~line 1160) to prevent stampedes, so the design already
accepts a small critical section. **Proposal:** add a second
`Mutex<HashMap<String, Option<u64>>>` (or store a parsed index in
`ModelsCacheEntry`) keyed by model id, populated by `model_context_length`.
Value: low-medium — these calls are advisory and rare (startup discovery,
`/model`), which is why this ranks last; but it is a pure win with no wire
behavior change, grounded entirely in the existing cache structure at
~lines 1140–1195.

### Non-proposals (considered and rejected as ungrounded)

- Anthropic-style `cache_control` breakpoints on messages: `WireMessage`
  (~lines 133–141) has no such field, but nothing in these two files shows any
  gateway Kitsu targets consuming it — only the generic OpenAI-compatible
  shape. Skipped per "skip anything you cannot ground".
- Response-body memoization: contradicts the explicit streaming design
  (`sse_step`, ~lines 1090–1145) and the bounded-body contract
  (~lines 355–362). Not a caching win worth the correctness risk.
