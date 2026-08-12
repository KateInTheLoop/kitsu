# Cycle 12: Kitsu JSONL vs Claude Code CLI stream-json — Automation Protocol Comparison

**Verdict: For CI embedding, Claude CLI stream-json is consumer-friendly; Kitsu JSONL is model-focused.**

---

## Executive Summary

Both protocols emit line-delimited JSON suitable for automation pipelines, but with fundamentally different design philosophies:

- **Kitsu JSONL** (`--jsonl`): Rich, exhaustive model-operation telemetry with strict ordering guarantees, versioned schema, and resumability via `run_id`/`session_id`. Targets downstream agents and model-driven workflows.
- **Claude Code CLI stream-json** (`--output-format stream-json --verbose`): Initialization metadata, rate limits, streamed assistant messages, and a final result summary. Targets human operators and CI systems that need the answer fast.

---

## Protocol Comparison Matrix

| Aspect | Kitsu JSONL | Claude stream-json |
|--------|-------------|-------------------|
| **Schema** | `kitsu.chat.v1` (versioned) | No formal versioning; type discriminator only |
| **Reserved envelope fields** | `schema`, `type`, `run_id` (enforced) | `type` (loose discriminator) |
| **Resumability** | `run_id` + `session_id` (both in events) | `session_id` (first event only) |
| **Event types** | 21 event types (provider requests, attempts, deltas, tool calls, commands, rollback, evidence) | 3 type families: `system`, `rate_limit_event`, `assistant`, `result` |
| **Provider telemetry** | Full attempt lifecycle (retry count, delay, deadline, category, outcome) | Hidden; only final result visible |
| **Streaming deltas** | `assistant_delta` records ordered per turn | `assistant.content[].text` in single payload |
| **Error contract** | Exit codes: 0 (ok), 1 (local/policy), 2 (provider) | Exit code in `result.is_error`; no per-failure categorization |
| **Final answer** | Must concatenate `assistant_delta` + `assistant` payload | Direct `result.result` field (string) |
| **Cost visibility** | Absent | `total_cost_usd` per invocation |
| **Tool calls/rounds** | Discrete `tool_started`/`tool_finished` with round numbers | Not visible (no tool-use events) |
| **Output reliability** | Write-flush contract; broken-pipe is terminal | No explicit atomicity guarantee documented |
| **stdout vs stderr** | stdout reserved for JSONL only | Mixed (tee captures both) |
| **Consumer burden** | Must parse 21 event types, track turn_id/run_id state | Parse 3 event types; straightforward final result |

---

## Event Vocabulary & Clarity

### Kitsu JSONL: Comprehensive & Strict

**Ordered event sequence:**
```
run_started
  ├─ model_catalog (optional)
  ├─ turn_started
  │   ├─ provider_request
  │   ├─ provider_attempt (retry loop)
  │   ├─ assistant_delta* (streamed chunks)
  │   ├─ usage
  │   ├─ assistant (final text or null if rolled back)
  │   ├─ assistant_disposition (committed | discarded | committed_partial)
  │   ├─ tool_started (per tool invocation)
  │   │   └─ tool_finished
  │   ├─ rollback_retained_changes (if failed & rolled back)
  │   └─ turn_finished
  ├─ command_started / command_result / command_finished (for slash commands)
  └─ final_summary
```

**What a CI consumer sees instantly:**
- Run identity (`run_id`)
- Model identity + session resumability
- Per-turn input/output
- Retry behavior & provider errors (categorized: `rate_limited`, `authentication`, `server_error`, etc.)
- Tool execution details (argv, cwd, exit code, stderr tail)
- Durable session state after turn success

**Example: Reconstructing a turn outcome**
```
1. Wait for turn_started (has turn_id, input text)
2. Collect provider_attempt events (track retries, deadlines)
3. Collect assistant_delta events (stream response incrementally)
4. Read assistant_disposition (committed? discarded?)
5. Read turn_finished (success? failure reason?)
```

### Claude stream-json: Minimalist & Opaque

**Event sequence:**
```
system (init metadata: cwd, tools, models, session_id)
rate_limit_event (quota info)
assistant (thinking content)
assistant (text content)
result (final summary: is_error, result, cost, usage breakdown)
```

**What a CI consumer sees at the end:**
- Final answer in `result.result`
- Total cost (`total_cost_usd`)
- Input/output token counts
- Cache hit metrics
- Model used
- Did it succeed? (`is_error`)

**What is opaque:**
- Retry attempts and their timing
- Provider error categories
- Tool use (if any)
- Whether the model streamed or was truncated
- Session continuity (session_id in line 1, never repeated)

---

## Versioning & Compatibility

**Kitsu:** Explicit schema versioning.
- `schema: "kitsu.chat.v1"` in every event — contract frozen.
- Consumers validate. New fields okay within v1; new types okay within v1.
- **Removal or type change** requires new schema (e.g., `kitsu.chat.v2`).
- **Risk:** One old consumer paired with new kitsu = silent drops of unknown event types, but the schema version is in every line.

**Claude:** No versioning field.
- Events identified by `type` alone.
- Consumers are expected to ignore unknown fields and skip unknown types.
- **Risk:** An old CI system parsing `--output-format stream-json` will silently miss new rate-limit fields or future event types.
- **Mitigation:** Claude documents breaking changes in release notes; Kitsu bakes the version into the event stream.

**Winner for automation:** Kitsu (explicit versioning).

---

## Error & Exit-Code Contract

### Kitsu
- **Exit code semantics:**
  - `0` → `final_summary.status: "ok"` (all operations succeeded)
  - `1` → Local policy/tool denial or script turn failure
  - `2` → Provider/incomplete-model failure (retries exhausted, timeout, auth rejected)
- **Granularity:** `final_summary.counters` break down provider failures, tool failures, policy denials separately.
- **Per-turn:**  `turn_finished.error` (if failed) + `turn_finished.error_category` (provider | incomplete | tool | policy | user).
- **CI use:** An automation can see at a glance: did the model fail (exit 2), or did the local environment deny it (exit 1)?

### Claude
- **Exit code:** Always 0 if run completes (even with errors recorded).
- **Error signal:** In the JSON, `result.is_error` and `result.terminal_reason` (e.g., `"completed"`, `"api_error"`).
- **Granularity:** No per-failure category; only `modelUsage` breakdown and the final `stop_reason`.
- **CI use:** A pipeline must parse JSON to know if the model request failed; it can't use `echo $?`.

**Winner for automation:** Kitsu (exit codes encode error semantics; CI doesn't need to parse JSON to route failures).

---

## Resumability & Session IDs

### Kitsu
- **`run_id`:** Unique per invocation; in every event.
- **`session_id`:** Unique per saved session (multi-turn history); present in `run_started`, absent if `--no-session` or piped.
- **`resumed`:** Boolean in `run_started` indicating whether this run is continuing a saved session.
- **Automatic persistence:** With `--session` or in interactive mode, successful turns auto-save.
- **Resume syntax:** `kitsu chat --resume <id>` or `kitsu chat --resume last`.
- **CI benefit:** A CI system can capture the `session_id` from `run_started`, save it, and later `--resume` to continue multi-turn inference within the same conversation state.

### Claude
- **`session_id`:** Present in the first event (`system.session_id`).
- **Resume:** Not exposed in `stream-json` output; handled via `claude --resume <id>`.
- **Persistence:** Sessions are cloud-backed (not suitable for ephemeral CI containers).
- **CI limitation:** An offline CI system has no mechanism to resume; each `claude -p` is a new session.

**Winner for offline automation:** Kitsu (explicit session resumability; Claude assumes cloud continuity).

---

## Consumer Ergonomics: CI Embedded Workflows

### Kitsu: Multi-round Agent-Aware
**Scenario:** A CI job spawns kitsu to fix linting errors, then commits the result.

```bash
# Capture session for potential resume
session=$(jq -r '.session_id' /tmp/kitsu-events.jsonl | head -1)
# Loop through events
jq -r 'select(.type == "turn_finished")' /tmp/kitsu-events.jsonl | \
  jq -e '.error' && exit 1  # Exit on turn error
# Extract final answer
jq -r '.text' /tmp/kitsu-events.jsonl | \
  grep -o 'assistant_delta' | tail -1 | jq -r '.text'
# Save session for future resume
echo "session=$session" >> /tmp/ci-state
```

**CI-native:** Retry logic visible (provider_attempt), tool invocations visible (tool_started/finished), session resumable.

### Claude: Single-Turn Answer-Focused
**Scenario:** A CI job asks for a summary, gets the answer, exits.

```bash
# Run and capture output
claude -p "summarize src/" --output-format stream-json --verbose > /tmp/claude-out.jsonl
# Extract final answer
jq -r '.result' /tmp/claude-out.jsonl | tail -1
# Done (no session resumability)
```

**CI-native:** Stateless, predictable cost (in JSON), single-turn by design.

---

## Workspace Evidence & Model-Visible Tools

### Kitsu
- **`workspace_diff` tool:** Host-verified diffs disclosed via `workspace_evidence_disclosure` events.
- **Event between tool_started/finished:** Marks the moment diff bytes entered the model context.
- **CI benefit:** Automation can audit which file changes the model saw.

### Claude
- **No equivalent in stream-json:** Tool use is hidden from the output stream.
- **CI limitation:** No visibility into whether the model accessed filesystem or made changes.

**Winner:** Kitsu (transparent tool use & evidence audit).

---

## Truncation, Timeouts & Partial Success

### Kitsu
- **`committed_partial`:** A disposition when the deadline expires after streaming began.
  - The delivered text (up to the timeout) is marked `committed_partial`, not `discarded`.
  - `turn_finished` still ends with `error`, exit code 2.
  - **CI value:** A partial answer is better than nothing; automation can use it.
- **`rollback_retained_changes`:** After a failed turn, any file edits remain on disk but are removed from conversation history.
  - Model is told via injected note what files changed.
  - **CI value:** No silent loss of side effects.

### Claude
- **Truncation:** Handled silently; only visible in token counts.
- **Timeout:** Raises an error; the partial answer is lost.
- **CI limitation:** No `committed_partial` equivalent.

**Winner:** Kitsu (partial results preserved; side effects auditable).

---

## Static Analysis Summary

### Kitsu JSONL Strengths
1. **Explicit versioning** in every event → forward compatibility.
2. **Rich provider telemetry** (retries, deadlines, categories) → CI can diagnose failures.
3. **Exit code semantics** → no JSON parsing to route errors.
4. **Session resumability** in JSON → multi-turn CI workflows.
5. **Tool & workspace-evidence transparency** → audit trail for model actions.

### Kitsu JSONL Weaknesses
1. **21 event types** → consumer must handle complexity.
2. **No cost visibility** → CI budgeting harder.
3. **Ordering-dependent** → a single out-of-order event breaks streaming consumers.
4. **Session persistence** on-disk only → no cloud continuity.

### Claude stream-json Strengths
1. **Minimal event types** (3 families) → easy to parse.
2. **Cost visible** → CI can track spend per invocation.
3. **Cloud-native** → session resumability via `claude --resume`.
4. **Stateless** → no local session files to manage.

### Claude stream-json Weaknesses
1. **No versioning** → silent drops if new event types added.
2. **Opaque provider retries** → CI can't diagnose transient failures.
3. **No exit-code semantics** → must parse JSON to know if model failed.
4. **Tool use hidden** → no audit trail.
5. **Requires `--verbose`** → documentation does not clearly state this requirement.
6. **No session ID in subsequent events** → only in first line (fragile parsing).

---

## Top 5 Kitsu Automation-Protocol Gaps vs Claude CLI

### Ranked by CI Impact

| Rank | Gap | Kitsu Limitation | One-Line Fix |
|------|-----|------------------|-------------|
| **1** | **Cost tracking** | No cost field in JSONL events | Add optional `cost_usd`, `input_tokens`, `output_tokens` to `provider_attempt` and `final_summary`. |
| **2** | **Model metadata visibility** | Model ID only in `run_started`; no per-attempt model info for fallback scenarios | Repeat `model` field (or fallback chain) in `provider_attempt` events. |
| **3** | **Session ID in result events** | `session_id` in `run_started` only; no way to extract it from `final_summary` without stateful parsing | Add `session_id` to `final_summary` (or keep it in run_started and document the parsing pattern). |
| **4** | **Documentation: `--verbose` requirement** | Help text for `--jsonl` does not mention that machine consumers should ignore routine diagnostics on stderr | Add to help: "Routine diagnostics and spinners are suppressed; fatal diagnostics may use stderr—consumers should ignore all stderr in automation." |
| **5** | **Consumer-ready final-answer field** | Must concatenate `assistant_delta` texts + check `assistant_disposition` for validity | Add top-level `final_answer` field to `final_summary` (populated only if `status: "ok"` and no rollback). |

---

## Protocol Verdict Matrix

### For a CI system deciding which CLI to embed

| Use Case | Kitsu | Claude | Winner |
|----------|-------|--------|--------|
| Single-turn question (e.g., `"summarize src/"`) | Yes, but overkill | Yes, minimal | **Claude** |
| Multi-turn with session resumption | Yes, explicit | No, cloud-only | **Kitsu** |
| Audit trail (tool use, file changes) | Yes, transparent | No, hidden | **Kitsu** |
| Cost per invocation tracking | No native field | Yes, `total_cost_usd` | **Claude** |
| Error routing (retry vs policy vs model) | Yes, categorized | No, all opaque | **Kitsu** |
| Offline operation | Yes, local sessions | Partial (no resume) | **Kitsu** |
| Speed (minimal events) | No (21 types) | Yes (3 types) | **Claude** |
| Stateless (no session cache) | No (auto-saves) | Yes | **Claude** |
| Explicit versioning | Yes (`kitsu.chat.v1`) | No | **Kitsu** |
| Streaming deltas (TTFB) | Yes, incremental | Yes (in thinking) | **Tie** |

---

## Recommendation

**For a CI pipeline embedding Claude:**

If the job is **single-turn, cost-conscious, and stateless** → **Claude stream-json** is lighter and cost-tracking is built-in.

```bash
claude -p "your prompt" --output-format stream-json --verbose \
  | jq '.result' | tail -1
```

**For multi-turn, audit-heavy, or model-experimentation workloads** → **Kitsu JSONL** is richer; absorb the 21 event types and invest in parsing.

```bash
kitsu --gateway ollama --model your-model --jsonl ask "your prompt" \
  | jq -r 'select(.type == "assistant_delta") | .text' | tr -d '\n'
```

---

## Appendix: Raw Event Captures

### Claude stream-json (cycle12-claude-streamjson.log)
**5 lines; 6.9 KB complete run**
- Line 1: `system` (init: cwd, session_id, tools available, MCP servers, model, version 2.1.228)
- Line 2: `rate_limit_event` (quota status: allowed, reset time, overage disabled)
- Line 3: `assistant` event with thinking (empty thinking block, model: claude-fable-5)
- Line 4: `assistant` event with text response: `{"type":"text","text":"pong"}`
- Line 5: `result` (summary: is_error: false, duration_api_ms: 6067, total_cost_usd: 0.1648, usage breakdown, session_id, ttft_ms: 5211)

**Event structure clarity:**
```json
{
  "type": "result",
  "is_error": false,
  "duration_ms": 5234,
  "total_cost_usd": 0.16475700000000001,
  "usage": {
    "input_tokens": 2,
    "output_tokens": 21,
    "cache_creation_input_tokens": 7356,
    "cache_read_input_tokens": 15975
  },
  "modelUsage": {
    "claude-fable-5": { "inputTokens": 2, "outputTokens": 21, "costUSD": 0.164165 }
  },
  "session_id": "4c656451-28e2-40bd-b97c-a46e95aa1d8f"
}
```

**Key insight:** Complete metadata in first and last lines; streaming content in middle. Cost and cache hit rates are explicit. Session ID appears only once (fragile for pipe-based consumers).

### Kitsu JSONL (cycle12-kitsu-jsonl.log, complete)
**10 lines; 2.4 KB complete run (half the size of Claude)**
- Line 1: `run_started` (schema, type, run_id, gateway, model, session_id: null, mode: piped)
- Line 2: `turn_started` (turn_id, input text)
- Line 3: `provider_request` (round, message_count, tools_offered)
- Line 4: `provider_attempt` (attempt: 1, status: 200, category: success, outcome: body_completed, terminal: true)
- Line 5: `usage` (prompt_tokens: 1778, completion_tokens: 2, total_tokens: 1780)
- Line 6: `assistant_delta` (text: "pong")
- Line 7: `assistant` (text: "pong")
- Line 8: `assistant_disposition` (status: committed)
- Line 9: `turn_finished` (status: ok, tool_errors: 0, policy_errors: 0)
- Line 10: `final_summary` (status: ok, provider_failures: 0, tool_failures: 0)

**Event structure clarity:**
```json
{
  "type": "provider_attempt",
  "attempt": 1,
  "max_attempts": 3,
  "category": "success",
  "status": 200,
  "outcome": "body_completed",
  "terminal": true,
  "retry_delay_ms": null,
  "remaining_deadline_ms": 40238
}
```

**Final summary:**
```json
{
  "type": "final_summary",
  "status": "ok",
  "provider_failures": 0,
  "tool_failures": 0,
  "policy_failures": 0,
  "user_failures": 0,
  "cancelled_failures": 0
}
```

**Key insight:** Every event has `schema: "kitsu.chat.v1"` (versioning explicit). Provider retry logic visible (attempt count, deadline, outcome categorization). Smaller payload than Claude (2.4 KB vs 6.9 KB) due to no cache metadata. No cost field present.

---

## Conclusion

**Kitsu JSONL** is a production-grade protocol for model-driven automation: explicit versioning, comprehensive error categorization, and session resumability make it suitable for CI systems that need auditability and multi-turn workflows.

**Claude stream-json** is purpose-built for single-turn CI tasks: minimal, cost-visible, and cloud-ready.

Neither protocol is "better"; they serve different architectural goals. Choose based on whether your CI needs **transparency & multi-turn** (Kitsu) or **simplicity & cost-tracking** (Claude).
