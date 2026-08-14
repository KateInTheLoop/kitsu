# Cycle-6 definitive review: caching & context-window management (kitsu)

Scope: synthesizes cycle-4 (`crates/kitsu-core/src/context.rs`) and cycle-5
(`crates/kitsu-providers/src/types.rs` + `openai_compat.rs`) reviews, verified
against `crates/kitsu-core/src/orchestrator.rs` lines 700–900 (turn state,
`ContextWindowEvidence`, meter wiring) and 2100–2350 (`request_measurement`,
`ensure_chat_request_fits`, `compact_context`, `enforce_context_budget_recording`).
Each proposal is marked **CARRIED (cycle N)** or **NEW (orchestrator)**. Line
numbers refer to the files as read this cycle.

## How the pieces fit (verified this cycle)

- `context.rs` provides the measurement primitives: `ContextMeter` (anchor +
  tail estimate), `ContextBudget` (window → effective/target), `message_tokens`.
- `orchestrator.rs` consumes them: `request_measurement` (~lines 2165–2212)
  calls `meter.estimate(messages)` and adds `estimate_str(schema_json)` unless
  `meter.anchor_includes_schema(messages)`; `ensure_chat_request_fits`
  (~lines 2218–2233) is the last-line guard at every provider boundary;
  `AgentSessionState` (~lines 745–770) persists `meter`, `compact_failures`,
  and `compaction_ledger` across turns so the anchor survives.
- `orchestrator.rs` already ships a partial sanity check cycle-4 asked for:
  `ContextWindowEvidence` + `context_window_shortfall` (~lines 772–820) detect
  "server accepted far fewer prompt tokens than estimated" and warn once — but
  they never feed back into the meter or budget (see P6).

## Ranked proposals (merged, deduped)

### P1. Send a cache key / prefix-cache affinity hint on requests — CARRIED (cycle 5)

- **Lines:** `openai_compat.rs` ~107–126 (`WireRequest`), ~250–262
  (`build_body`); `types.rs` ~89–102 (`ChatRequest`).
- **What:** Requests carry only `model/messages/tools/stream/stream_options/
  temperature/reasoning_effort/max_tokens`. No `prompt_cache_key`
  (OpenAI/HF-router) or any affinity hint, so retries
  (`chat_stream_with_telemetry`, ~1290–1500) and multi-round tool turns re-send
  the identical long prefix with no way for sharded gateway caches to pin it.
- **Change:** Add `pub prompt_cache_key: Option<String>` to `ChatRequest` and a
  matching `#[serde(skip_serializing_if = "Option::is_none")]` field on
  `WireRequest`, populated by the caller (e.g. session id).
- **Why:** Directly raises the prefix-cache hit rate — the single biggest
  user-visible caching lever (cost + latency). Two additive fields.

### P2. Anchor validity is length-only; add a content fingerprint or generation counter — CARRIED (cycle 4)

- **Lines:** `context.rs` 100–107 (`estimate`), 111–113 (`anchored`), 122–124
  (`anchor_includes_schema`), 79–81 (`record`), 83–93 (`invalidate`).
- **What:** The anchor is trusted whenever `sent_len <= messages.len()` (line
  102) — a count check, not identity. An in-place edit inside the anchored
  prefix (queue/steering edits, rewritten tool result) keeps the length, so
  `estimate` silently reports `anchor + tail` for content the anchor never
  measured. The `invalidate` doc (83–90) pushes enforcement onto callers;
  nothing detects a missed `invalidate()`. Orchestrator cross-check:
  `request_measurement` (~2183–2195) trusts this guard verbatim ("Any mutation
  of covered messages invalidates the anchor first" — a comment, not a check).
- **Change:** Store a cheap prefix fingerprint in the anchor — a `u64` hash
  (FNV/xxhash) over the leading `sent_len` messages' role + content lengths,
  computed in `record` and compared in the line-102 guard — or an O(1)
  generation counter owned by the conversation, bumped on any mutation.
- **Why:** Converts the worst failure mode — a silently wrong estimate that the
  trim cascade (`enforce_context_budget_recording`, ~2290+) and `/context`
  treat as real API usage — into a safe full-recompute fallback (line 105).
- **Risk:** Hashing is O(prefix); hashing only lengths is near-free. The
  generation counter is O(1) but needs discipline from the message-list owner.

### P3. Parse and surface `cached_tokens` (and stop dropping the rest of `usage`) — CARRIED (cycle 5)

- **Lines:** `types.rs` ~129–140 (`Usage`), ~142–161 (`impl Usage`);
  `openai_compat.rs` ~283–294 (`StreamChunk.usage`), ~337–342
  (`CompletionResponse.usage`), ~430–436 and ~488–491 (stream capture/emit),
  ~590–595 (`parse_completion`).
- **What:** `Usage` has only the three flat counters; serde silently discards
  `prompt_tokens_details.cached_tokens`, `completion_tokens_details.
  reasoning_tokens`, and gateway cost fields at the `from_value` calls (~415,
  ~566). Separately, the `is_empty()` gates (~432, ~591) drop a cache-hit-only
  chunk (`prompt_tokens: 0`, nonzero `cached_tokens`) as if it were silence.
- **Change:** Add `PromptTokensDetails { cached_tokens }` + `Option` field on
  `Usage` (stays `Copy`); add `cached_tokens()` accessor; extend `add()` to sum
  the breakdown across rounds; change both capture gates to
  `!u.is_empty() || u.cached_tokens() > 0`. Wire shape is unchanged
  (`#[serde(default)]`), no provider-trait signature change; kitsu-core's
  per-turn aggregation (`Usage::add`, consumed via `state.run_usage`,
  orchestrator.rs ~766) and the usage-record writer just read the accessor.
- **Why:** You cannot improve what you cannot see — this is the telemetry
  prerequisite for P1 and for any cache-hit-rate reporting in `kitsu-usage:`.
- **Risk:** None on the wire; keep `is_empty()`'s documented contract by
  gating at the two capture sites instead of changing the method.

### P4. `message_tokens` ignores per-message overhead and counts bytes, not chars — CARRIED (cycle 4)

- **Lines:** `context.rs` 35–43 (`message_tokens`), 16–18 (rates), 29–31
  (`estimate_str`).
- **What:** (a) No per-message constant: real chat tokenizers add ~3–4 tokens
  of role/separator overhead per message, so histories of many short messages
  are underestimated. (b) `s.len()` (lines 30, 36, 40) is bytes: CJK text is
  ~3 bytes per token-bearing char, so the 4-bytes/token rate underestimates
  CJK-heavy content by roughly a third.
- **Change:** Add a small `PER_MESSAGE_TOKENS` constant (err low, 2–3, so the
  bias stays conservative per the philosophy at lines 20–21), and count
  `chars()` or use a two-rate ASCII/non-ASCII heuristic in `estimate_str`.
- **Why:** Every unanchored path rests on this: the full-estimate fallback
  (line 105), `breakdown` (128–140) behind `/context`, and the orchestrator's
  `structural_request_tokens` (~2153–2162). Shrinks the error band P5's sanity
  check must tolerate.
- **Risk:** Slight overestimate for providers without per-message overhead;
  over-fill is safer than overflow.

### P5. Sanity-check recorded usage; clamp or distrust implausible anchors — CARRIED (cycle 4), partially pre-implemented at orchestrator level

- **Lines:** `context.rs` 79–81 (`record`), 100–107 (`estimate`);
  orchestrator.rs ~793–820 (`context_window_shortfall`).
- **What:** `record` accepts any `prompt_tokens` a gateway returns and
  `estimate` treats it as authoritative; a buggy/proxying gateway returning 0
  or aggregate usage poisons every estimate until the next `record`. The
  orchestrator already detects one side of this — `context_window_shortfall`
  flags `reported << estimated` with a wide-margin/clamp-boundary heuristic and
  a `plausible_clamp` wolf-guard — but only to warn once and annotate
  `/context` provenance; the bad anchor still drives the trim cascade.
- **Change:** In `record` or the line-102 anchor branch, compare the anchor
  against the anchor-free full estimate and, outside a generous band
  (0.4×–2.5×), ignore the anchor and fall back to full estimate; emit a
  telemetry counter when it trips. Reuse the `context_window_shortfall`
  thresholds so the two detectors agree.
- **Why:** Self-contained circuit breaker; the fallback path already exists.
- **Risk:** Legitimate tokenizer differences can trip the band — keep it wide
  and advisory (fall back, never error).

### P6. NEW (orchestrator): `ContextWindowEvidence` never feeds back into the budget or meter

- **Lines:** orchestrator.rs ~772–791 (evidence struct, "never silently
  rewrites the resolved window"), ~793–820 (shortfall heuristic), ~763–765
  (`window_evidence` preserved across rollbacks).
- **What:** Evidence that the backend's real window is smaller than the
  resolved one is recorded, warned once, and shown in `/context` — then
  ignored. A session against an Ollama `num_ctx = 4096` behind an 8192 config
  keeps budgeting against 8192 forever, so every turn flirts with silent
  server-side truncation that Kitsu has already diagnosed.
- **Change:** After repeated (≥2) shortfall evidences for the same resolved
  window, clamp the effective budget to `max(reported, MIN viable)` for the
  rest of the session — surfaced as a provenance note, never written back to
  config. This preserves the "never silently rewrite" contract (the resolved
  window is untouched; the *session budget* adapts, exactly what budgets are
  for).
- **Why:** Turns a diagnostic into a fix; highest user impact of the
  orchestrator-level findings because silent truncation degrades every answer.
- **Risk:** A false-positive clamp wastes window; the existing
  `plausible_clamp` guard (~813–819) plus a two-strike rule keeps this rare.

### P7. NEW (orchestrator): `anchor_includes_schema` is a bool — tool-set changes between turns fool it

- **Lines:** `context.rs` 63–68 (anchor tuple), 122–124
  (`anchor_includes_schema`); orchestrator.rs ~2183–2195 (schema tokens added
  unless the anchor "already carried them").
- **What:** The anchor records *whether* the anchored request included tool
  schemas, not *which* schemas. If the tool registry changes mid-session (tools
  enabled/disabled, subagent profile switch), an anchor recorded with schema
  still suppresses the schema estimate even though the new schema differs —
  the same count-vs-identity flaw as P2, one level up.
- **Change:** Store a cheap schema fingerprint (length of `schema_json`, or a
  `u64` hash) alongside `had_schema` in the anchor; `anchor_includes_schema`
  returns true only when the fingerprint matches the current `schema_json`.
  `ensure_chat_request_fits` (~2218–2233) already serializes the schema on
  every check, so the fingerprint is free.
- **Why:** Prevents systematic under-estimation exactly when tools change —
  the sessions (subagent-heavy) that can least afford overflow.
- **Risk:** None; additive field on the anchor tuple.

### P8. Trim buffer does not scale with estimate error on large windows — CARRIED (cycle 4)

- **Lines:** `context.rs` 166 (`buffer`), 155–158 (doc), 159–173
  (`ContextBudget::new`).
- **What:** `buffer = min(window/10, 2048)` caps at a 20 480-token window;
  beyond that the buffer is flat while absolute estimate error grows with
  fill. On a 1M window at `target`, ±5 % error is ~40k tokens — 20× the
  buffer — so `effective` can be exceeded despite trimming to `target`.
- **Change:** `buffer = max(2048, target * ERROR_FRACTION)` with
  `ERROR_FRACTION` ~5 %, capped at `window/10`; lower the fraction once P4
  lands.
- **Why:** Makes the headroom policy honest: the buffer exists to absorb
  estimation error, which is proportional to fill, not constant.
- **Risk:** Larger buffer wastes window on huge-context models; the
  `window/10` cap bounds it.

### P9. Budget reserve and the `max_output` actually sent can disagree — CARRIED (cycle 4)

- **Lines:** `context.rs` 159–165 (reserve + clamp), 24–25
  (`MIN_OUTPUT_RESERVE`), 20–22 (`DEFAULT_CONTEXT_WINDOW`).
- **What:** Reserve is `max(max_output, 512).min(window/2)`. With the default
  8192 window and `max_output = Some(8192)`, the budget reserves 4096 while the
  provider is told it may emit 8192 — a full-length reply overflows the window
  the budget promised. The 512 floor is also thin for reasoning-style models.
- **Change:** Expose the clamped reserve on `ContextBudget` (`pub reserve` or
  `clamped_max_output()`) so the caller sends a consistent `max_output`;
  consider raising or model-aware-izing `MIN_OUTPUT_RESERVE`. Orchestrator
  cross-check: `request_measurement` (~2189) passes `req.max_tokens` into
  `ContextBudget::new` but nothing rewrites `req.max_tokens` from the clamp.
- **Why:** The invariant `target <= effective <= window` only holds end-to-end
  if the reply is actually bounded by the reserve.
- **Risk:** Additive API surface; callers that ignore it are no worse off.

### P10. Cache the parsed per-model context length, not just the raw catalog body — CARRIED (cycle 5)

- **Lines:** `openai_compat.rs` ~40–49 (`models_cache`), ~61–63 (TTLs),
  ~1200–1250 (`model_context_length` re-parses + re-scans per call), ~1160
  (lock held across fetch).
- **What:** The raw catalog text is cached, but every `model_context_length`
  call re-parses the full JSON and re-runs the exact/loose match.
- **Change:** Add a `Mutex<HashMap<String, Option<u64>>>` keyed by model id,
  populated by `model_context_length`.
- **Why:** Pure win, no wire change; ranked low because these calls are
  advisory and rare (startup discovery, `/model`).

### P11. NEW (orchestrator, minor): `ensure_chat_request_fits` re-serializes tool schemas at every provider boundary

- **Lines:** orchestrator.rs ~2218–2233 (`serde_json::to_string(tools)` per
  call), ~2168–2172 (`estimate_str(schema_json)` per measurement).
- **What:** Every preflight serializes the full tool descriptor set and
  re-estimates its tokens, though the schema changes only when the registry
  changes.
- **Change:** Cache `(schema_json, estimate_str(schema_json))` per registry
  generation in `AgentSessionState` (which already persists per session,
  ~745–770); the same cache can hold the fingerprint P7 needs.
- **Why:** Removes repeated O(schema) work from the hot path of every round;
  synergizes with P7.
- **Risk:** Must invalidate on registry mutation; a generation counter on the
  registry keeps it trivial.

## Notes

- Strengths worth keeping (verified across all three files): the schema-aware
  anchor flag and its regression tests (`context.rs` 63–68, 122–124, tests at
  end of file); the documented `invalidate` contract (83–93); saturation-safe
  arithmetic in `ContextBudget::new` (165–167); `AgentSessionState` persisting
  meter + circuit breaker across turns (orchestrator.rs ~745–760); the
  wolf-guarded `context_window_shortfall` heuristic (~793–820); and the byte
  fallback path with its own unit/provenance (~2196–2210) when no window is
  resolved.
- Deliberate non-proposals (rejected as ungrounded, carried from cycle 5):
  Anthropic-style `cache_control` breakpoints (no evidence any targeted
  gateway consumes them) and response-body memoization (contradicts the
  streaming design and bounded-body contract).
- Byte-mode sessions (no resolved window) get no usage anchor at all
  (orchestrator.rs ~2196–2210) — accepted as designed; not a proposal.

## Top-5 priorities by user impact

1. **P1** — send `prompt_cache_key`/affinity: the only change that directly makes prompts cheaper and faster on cache-aware gateways.
2. **P2** — anchor content fingerprint: stops silently wrong "real usage" estimates from driving trims and `/context`.
3. **P3** — parse `cached_tokens`: the telemetry prerequisite that makes all caching measurable (and fixes cache-hit-only chunks vanishing).
4. **P6** — let `ContextWindowEvidence` clamp the session budget: converts an already-shipped diagnosis into a fix for silent server-side truncation.
5. **P4** — per-message overhead + chars-not-bytes: fixes systematic under-estimation for many-short-message and CJK histories on every unanchored path.
