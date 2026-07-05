# Context-Window Management — Stages of Development

- **Status:** Draft / proposed
- **Date:** 2026-07-05
- **Design:** [context-management-design.md](context-management-design.md) — read
  first for the problem statement and target architecture.

This document breaks the target design into independently shippable stages. Each
stage is a self-contained increment that leaves kitsu working and better than
before; later stages are optional and can be deferred without stranding earlier
ones. Order is by value-for-effort: **Stage 1 fixes the actual failure mode.**

Legend — **Effort:** S (≤ ½ day) · M (~1–2 days) · L (~3+ days).

---

## Stage 1 — Model-aware token budgeting  *(Effort: M · Priority: highest)*

**Goal.** Stop small-window models from hard-failing on context length. Budget in
tokens against the *actual* model window instead of a fixed 128 KiB byte count.

**Why first.** kitsu's core audience is local models at 4 K–32 K tokens, where the
current 128 KiB (~32 K-token) byte budget can exceed the whole window — so kitsu
never trims and the gateway rejects the request. This is the real bug; everything
else is refinement.

**Scope / changes.**
1. `ContextMeter` (kitsu-core): estimate current request tokens as
   `last_prompt_tokens + Σ bytes/4` over messages appended since the last
   `Usage`; pure `bytes/4` before the first response.
2. Record the last response's `prompt_tokens` in the agent loop (we already
   receive `ChatDelta::Usage`).
3. Window discovery (`ContextWindow`): config/CLI → `/v1/models` metadata (Ollama
   exposes it) → conservative default (8 K).
4. `effective = window − max(max_output_tokens, MIN_RESERVE)`; trim target =
   `effective − min(window/10, 2 K)`.
5. Rewrite `enforce_context_budget` to trim against the **token target** (still
   tool-result elision only in this stage — the tier ladder comes later).
6. Add `context_window` to the gateway profile config (optional) and a
   `--context-window` CLI override.
7. Emit a `kitsu-context: {used, effective, window, tier}` stderr line.

**Files.** `crates/kitsu-core/src/orchestrator.rs` (meter, budget rewrite),
`crates/kitsu-providers/src/openai_compat.rs` (surface context length from
`list_models`), `crates/kitsu/src/{cli,config,main}.rs` (flag + profile field +
wiring), `crates/kitsu-render/src/lib.rs` (`emit_context`).

**Tests.**
- Unit: `ContextMeter` estimate with/without a usage anchor; window resolution
  precedence; `effective`/target math; trim stops at target and preserves
  system + last user.
- Integration: a small `--context-window 2000` run trims and still succeeds
  (wiremock or a small Ollama model); a `kitsu-context:` line is emitted.

**Acceptance.** A session that previously failed with a context-length error on a
4 K model instead trims and completes. Budget scales with `--context-window`.
`cargo test` + clippy green.

**Risks.** `/v1/models` context-length field is gateway-specific (parse
defensively, fall back). Token estimate is approximate — the reserve/buffer
absorb the error.

---

## Stage 2 — Tool-result offload + turn-level trimming  *(Effort: M · Priority: high)*

**Goal.** Handle the dominant context consumer in coding sessions (big file
reads / greps) cheaply and reversibly, and trim whole stale turns — not just tool
results — when needed.

**Scope / changes.**
1. **Tier 0 offload.** When a tool result exceeds a per-tool cap, write the full
   result to the scratch dir and replace the in-context block with a pointer +
   short preview: `[output NN KB saved to <path> — re-read with fs_read_file]`.
   Reuses the permission-gated write path; generalizes today's 64 KiB read
   truncation from "drop" to "offload" (reversible).
2. Per-tool caps (chars): a small table, e.g. read/grep ~20 K, default ~50 K,
   mirroring Claude Code's ratios.
3. **Tier 2 turn trimming.** After tool-result elision, if still over target,
   drop the oldest user/assistant *pairs* (never system, never the most recent K
   turns, never split a tool_use/tool_result pair) until under target.
4. Order the cascade: offload (at execution) → elide old tool results → drop old
   turns; stop as soon as under target; report the tier in `kitsu-context:`.

**Files.** `crates/kitsu-tools/src/builtins/fs.rs` + a small offload helper
(scratch path, pointer message), `crates/kitsu-core/src/orchestrator.rs` (cascade
ordering + turn trimming), tool-result plumbing so the orchestrator can offload.

**Tests.**
- Offload: an oversized tool result becomes a pointer; the pointed-to file exists
  and round-trips via `fs_read_file`.
- Turn trimming preserves system + last K turns and never splits a tool_use /
  tool_result pair; a many-turn no-tool session stays under target.

**Acceptance.** A grep/read-heavy session stays within a small window by
offloading, and the model can recover offloaded content by re-reading. A long
chat with no big tool output no longer grows unbounded.

**Risks.** Offloading must respect the write permission model (write into the
session scratch dir, not arbitrary paths). Pairing invariants must hold or some
gateways reject orphaned tool messages — cover with tests.

---

## Stage 3 — Summarizing compaction (gated)  *(Effort: L · Priority: medium)*

**Goal.** When trimming would evict the *active task*, preserve it by summarizing
the evicted span instead of dropping it — Claude Code's auto-compact, scaled down
and quality-gated.

**Scope / changes.**
1. **Trigger.** Only when Tier 2 would have to drop *recent* turns to hit target
   (i.e. cheap shedding is exhausted and the live task is at risk).
2. **Summarize.** One LLM call (tools denied, streaming off) with a scaled
   version of Claude Code's structured prompt: primary intent, files & code
   touched, decisions/errors-and-fixes, pending tasks, current work, and an exact
   next step. Cap the summary output tokens.
3. **Rebuild.** `system → [compaction summary message] → recent kept turns`; if a
   file was actively being edited, re-read it fresh into context.
4. **Boundary + circuit breaker.** Print a visible `✻ context compacted` boundary;
   stop attempting compaction after **3 consecutive failures** in a session.
5. **Gating.** Off by default on unknown/small models; enable via config
   (`compaction = "summarize"`) and/or a model-capability heuristic. Trimming
   (Stages 1–2) remains the fallback when compaction is off or fails.
6. A `/compact` slash command to trigger it manually.

**Files.** `crates/kitsu-core/src/orchestrator.rs` (trigger + rebuild + breaker),
a new `compaction` module (prompt + summarize call), `crates/kitsu/src/main.rs`
(`/compact`, config gate), `crates/kitsu-render` (boundary line).

**Tests.**
- Unit: prompt assembly; rebuild ordering (system + summary + recent kept);
  circuit breaker stops after 3 failures; pairing invariants after rebuild.
- Integration (capable model / HF): a long session compacts and the model
  continues the task coherently (references the summary, not lost).

**Acceptance.** On a capable model, a session that would otherwise lose its task
to trimming instead compacts and continues. On a small/unknown model, compaction
stays off and Stage 1–2 trimming still prevents hard failure.

**Risks.** Summary quality is model-dependent — hence gating. The summarization
call costs latency/tokens; the trigger must be conservative so it fires rarely.

---

## Sequencing & dependencies

```
Stage 1  (token budget + window)      ← ships the fix; no dependencies
   │
Stage 2  (offload + turn trimming)    ← builds on Stage 1's meter/target
   │
Stage 3  (summarizing compaction)     ← builds on Stage 2's trimming as fallback
```

- **Stage 1 is independently valuable** and should ship first; it resolves the
  actual failure mode on small-window models.
- **Stage 2** is the highest-leverage follow-up for coding workloads (read/grep
  heavy) and is reversible/lossless, so low-risk.
- **Stage 3** is the biggest lift and the most model-dependent; defer until 1–2
  are proven, and keep it opt-in.

## Cross-cutting

- **Dogfooding.** Extend `tests/dogfood` with a small-`--context-window` scenario
  that drives a long multi-turn session and asserts (a) no context-length
  failure and (b) `kitsu-context:` shows trimming/compaction firing.
- **Docs.** Update `README.md` (a "Context window" section: how the budget is
  derived, `context_window` config, `/compact`) and `docs/architecture.md` as
  each stage lands.
- **Backwards compatibility.** Keep a coarse byte guard as a last-resort fallback
  for gateways that report neither `usage` nor a window, so behavior never
  regresses for them.
