# Context-Window Management — Design

- **Status:** Draft / proposed
- **Date:** 2026-07-05
- **Scope:** How kitsu should manage the model's finite context window across a
  multi-turn `chat` session and tool-heavy agent loops.
- **Companion:** [context-management-stages.md](context-management-stages.md) —
  the staged development plan derived from this design.

---

## 1. Problem statement

kitsu runs an agent loop (`kitsu-core::orchestrator`) that appends to a growing
`Vec<ChatMessage>` every round: user turns, assistant turns, tool calls, and tool
results. The whole vector is re-sent to the gateway on every request. Left
unbounded it eventually exceeds the model's context window and the gateway
rejects the request (HTTP 400 "prompt too long" / context-length errors), failing
the turn outright.

### Current mechanism

`enforce_context_budget(messages, max_bytes)` (orchestrator.rs) runs before each
request. It:

- Sums `byte_size()` of all messages.
- If over `max_bytes` (hardcoded **128 KiB** in all three call sites), walks
  oldest-first and **elides `Tool` messages only**, replacing their body with the
  stub `"[older tool output elided]"`.
- Never touches the system prompt, the last user message, or any
  user/assistant message.

### Why this is inadequate

1. **Model-blind.** 128 KiB is a fixed byte count. It has no relationship to the
   actual model's window. kitsu's primary audience is **local models via Ollama,
   which commonly have 4 K–32 K token windows**. 128 KiB ≈ ~32 K tokens, so the
   budget can be **larger than the model's entire window** — meaning kitsu never
   trims, and the gateway hard-fails instead. The mechanism silently does nothing
   on exactly the models it most needs to protect.
2. **Bytes, not tokens.** Byte size is a poor proxy for tokens (code, JSON, and
   Unicode all tokenize very differently), and the gateway budgets in tokens. We
   also **already receive exact token usage** from every response (`Usage` /
   `ChatDelta::Usage`) and don't use it for budgeting.
3. **Only elides tool results.** Long assistant/user turns and a long back-and-
   forth are never trimmed. A conversation with no large tool outputs but many
   turns grows without bound until it fails.
4. **No headroom for output.** The budget doesn't reserve space for the model's
   response, so a request that just fits the window leaves no room to answer.
5. **One blunt tier.** Eliding a tool body to a stub is lossy and irreversible
   within the session, yet it's the *only* tool we have — there is no cheaper
   reversible option (offload-and-pointer) and no higher-fidelity option
   (summarize) when the task context itself must be shed.

### Goal

Replace the single byte-based elision with a **model-aware, token-based, tiered**
strategy that (a) never lets a session hard-fail on context length when it can be
avoided, (b) prefers cheap reversible shedding over lossy summarization, and
(c) works acceptably from 4 K local models up to 1 M cloud models.

---

## 2. Analysis

### 2.1 What kitsu already has to build on

- **Real token usage** per response: the OpenAI-compatible `usage` object
  (`prompt_tokens`, `completion_tokens`, `total_tokens`) is parsed into `Usage`
  and surfaced as `ChatDelta::Usage`. This is an *authoritative* anchor for how
  many tokens the last request actually cost.
- **A models endpoint**: `OpenAICompatProvider::list_models()` (`/v1/models`).
  Several gateways — notably **Ollama** — expose the model's context length in
  model metadata, so the window can often be discovered at runtime.
- **`max_output_tokens`** is already a CLI flag and request field, so the output
  reservation is available.
- **Filesystem tools** (`fs_read_file`, `fs_write_file`) and a scratch area — so
  the "offload a big tool result to disk, leave a pointer the model can re-read"
  pattern is directly implementable.
- A single choke point (`enforce_context_budget`, called once per round in
  `run_agent_turn`) where the whole strategy can live.

### 2.2 How Claude Code does it (reference implementation)

Claude Code (analyzed from source) treats context management as a **cascade of
increasingly expensive, decreasingly reversible tiers**, run in order every turn.
If an earlier tier gets under the threshold, the later ones no-op. Cheap and
reversible first; lossy LLM summarization only as a last resort.

| Tier | Mechanism | Cost | Reversible |
|---|---|---|---|
| 0 | **Per-tool result cap at execution.** A tool result over its cap is written to disk; the in-context block becomes a `<persisted-output>` pointer + a ~2 KB preview. Read self-bounds at 25 K tokens / 0.25 MB. Bash 30 K chars, Grep 20 K, default 50 K. | free | yes — model re-reads the path |
| 1 | **Per-message aggregate budget.** Parallel tools summing > 200 K chars in one wire message → offload the largest fresh results to disk. | free | yes |
| 2 | **Micro-compact.** Clear the *bodies* of old tool results in place (keep the last ~5), replacing with `"[Old tool result content cleared]"`. Fires on a 60-min prompt-cache-cold gap. No LLM call. | free | on disk in transcript |
| 3 | **Context-collapse.** A projection over full history that folds old reads/greps into re-expandable stubs, kept in a separate store. | free | yes |
| 4 | **Auto-compact.** A one-shot **LLM summarization** of the conversation into a fixed 9-section summary; the post-compact context keeps a boundary marker + the summary + the **5 most-recent files re-read fresh** + plan/skills. | 1 API call | no (one-way) |

**Token accounting** is a **hybrid**: anchor on the last *real* API `usage`
(`input + cache_read + cache_creation + output`), then add a **~4-chars/token
estimate** of only the messages appended since that response. It never
re-tokenizes the whole history.

**Window + thresholds** (200 K-model defaults):

- Window resolution order: explicit env override → `[1m]` name suffix (1 M) →
  per-model capability table → **200 K default fallback**.
- `effective = window − min(max_output_tokens, 20 K reserve)`.
- Auto-compact fires at `effective − 13 K` (~90 % of effective).
- Warning to the user at `threshold − 20 K`; a hard blocking limit at
  `effective − 3 K` reserves room for a manual compact.
- **Circuit breaker:** stop attempting compaction after **3 consecutive
  failures** (avoids burning API calls in a doomed loop).

Key design lessons we adopt:

1. **Order tiers cheap-reversible → expensive-lossy.** Never summarize if
   dropping/offloading a few stale tool results would do.
2. **Budget in tokens against the real window**, anchored on actual usage.
3. **Reserve output headroom** off the top before computing "context left".
4. **Cap tool results at the source**; for coding workloads, reads/greps are the
   dominant context consumer, and offload-with-pointer is both cheap and
   reversible.
5. **A summary must carry the task forward** — intent, files, pending work,
   verbatim next step — and re-attach the actual files in play, or the model
   loses the thread.
6. **Guard the machinery** with a failure circuit breaker.

### 2.3 What is different for kitsu (constraints)

- **Small windows are the common case.** Ollama models at 4 K–32 K make Stage 1
  (model-aware token budgeting) not a nicety but the fix for the actual failure
  mode. Claude Code's audience is 200 K+; kitsu's is often 50× smaller.
- **No prompt cache semantics** on most OpenAI-compat gateways, so the cache-TTL
  motivations behind micro-compact timing don't apply; kitsu triggers purely on
  token pressure.
- **Summarization quality scales with the model.** A 3 B local model summarizes
  poorly; the auto-compact tier must be **quality-gated** (opt-in / capability-
  gated), whereas trimming and offload work regardless of model.
- **No per-model capability table.** kitsu must discover the window from
  `/v1/models` when possible and otherwise fall back to config/default.

---

## 3. Target implementation

A single, model-aware, token-based cascade living behind the existing
`enforce_context_budget` call site in `run_agent_turn`, plus a source-level tool-
result cap. Tiers, cheapest first:

### 3.1 Token accounting (`ContextMeter`)

A small helper that estimates the current request's token cost the Claude-Code
way:

```
tokens ≈ last_usage.prompt_tokens            // authoritative anchor, if we have one
       + Σ estimate(msg) for messages appended since that usage was recorded
estimate(msg) ≈ ceil(byte_len / 4)           // 4 chars/token; /2 for dense JSON args
```

- The orchestrator already sees `ChatDelta::Usage`; record the running
  `prompt_tokens` after each response as the anchor.
- Before the first response (no anchor yet), fall back to a pure `bytes/4`
  estimate of the whole message set.

### 3.2 Window discovery (`ContextWindow`)

Resolution order (first hit wins), computed once per session:

1. Explicit config / CLI (`context_window` on the gateway profile, or a flag).
2. `/v1/models` metadata for the active model when the gateway exposes a context
   length (Ollama does).
3. A conservative **default fallback** (e.g. 8 K) — deliberately small so we
   *under*-fill an unknown model rather than overflow it.

`effective_window = context_window − output_reserve`, where
`output_reserve = max(max_output_tokens, MIN_RESERVE)` (a floor, e.g. 512), so we
always leave room to answer. Trim target = `effective_window − BUFFER` with a
small buffer scaled to the window (e.g. `min(window/10, 2 K)`).

### 3.3 The cascade (per round, before sending)

1. **Tier 0 — cap tool results at the source.** When a tool returns, if its
   result exceeds a per-tool cap, write the full result to the scratch dir and
   replace the in-context content with a short pointer + preview
   (`[output NN KB — saved to <path>; re-read with fs_read_file]`). kitsu already
   truncates read output at 64 KiB; this generalizes it to *offload* rather than
   silently drop, making it reversible. (Reuses the permission-gated write path.)

2. **Tier 1 — trim/elide old tool results** (evolve the current mechanism to be
   token-driven): oldest-first, replace stale tool-result bodies with a stub
   until under target. Keep the last N (e.g. 3) tool results intact.

3. **Tier 2 — drop oldest whole turns.** If still over target, remove the oldest
   user/assistant *pairs* (never the system prompt, never the most recent K
   turns, never split a tool_use/tool_result pair), until under target.

4. **Tier 3 — summarizing compaction (gated).** If trimming would evict the
   active task (i.e. we'd have to drop recent turns), instead make one LLM call
   to summarize the evicted span into a compact note, then rebuild history as:
   `system → [compaction summary] → recent kept turns`. Adopt a scaled version of
   Claude Code's summary structure (intent, files & code touched, decisions,
   pending tasks, current work, exact next step). Re-read the most-recent file(s)
   the model was working with. Gate on model capability and a config flag; apply
   a **3-failure circuit breaker**.

Each tier reports how many tokens it freed; the cascade stops as soon as the
estimate is under target.

### 3.4 Observability

- A `kitsu-context:` stderr line (like `kitsu-usage:`) reporting
  `used/effective` tokens and which tier fired, so sessions are debuggable and
  the dogfood harness can assert on it.
- On Tier 3, print a visible boundary (e.g. `✻ context compacted`) as Claude Code
  does, so the user knows summarization happened.

### 3.5 Data-model touch points

- `AgentConfig`: replace `max_context_bytes: usize` with a richer
  `context: ContextConfig { window, output_reserve, buffer, compaction: enum }`,
  or keep `max_context_bytes` as a fallback and add the token fields alongside.
- `Provider`/gateway config: optional `context_window` field; `list_models()`
  extended to surface a context length when the gateway provides one.
- Orchestrator: record last `prompt_tokens`; replace the `enforce_context_budget`
  body with the cascade.

---

## 4. Non-goals

- Server-side / API-native context management (Anthropic's `context_management`
  beta) — not available on generic OpenAI-compat gateways.
- Prompt-cache-aware timing (no cache semantics to exploit here).
- A persistent cross-session "collapse store" — kitsu compaction is per-session.
- Perfect token counts — an estimate anchored on real usage is sufficient;
  exact re-tokenization per turn is not worth the cost/complexity.

---

## 5. Open questions

1. **Default fallback window** when `/v1/models` doesn't expose one — 8 K is safe
   but wastes headroom on big models. Prefer requiring the user to set
   `context_window` per profile, with 8 K only as a last resort?
2. **Tier 3 gating** — how to decide a model is "capable enough" to summarize
   well without a capability table? Options: config opt-in only; a size heuristic
   from the model id; or always-on with a quality disclaimer.
3. Should Tier 0 offload be **always on** (even under budget) for very large tool
   results, to keep single results from dominating a small window?
4. Whether to keep `max_context_bytes` as a coarse secondary guard for gateways
   that report no usage and no window.
