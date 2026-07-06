# Subagents — Design

- **Status:** Draft / proposed
- **Date:** 2026-07-05
- **Builds on:** the agent loop (`run_agent_turn` / `run_agent_turn_with_state` +
  `AgentSessionState`), the permission model (`Permissions`), skills
  (`SkillSet` + `--skill`), and context management (design docs in `docs/`).

kitsu can't spawn sub-tasks today — asking it to "spawn an agent to review X"
just does the work inline (or nothing). This adds **subagents**: a tool the model
can call to run a nested agent loop on an isolated task and get back only the
result. The headline value is **context isolation** — a big exploration runs in
its own window and returns a small conclusion, so the parent conversation stays
lean (context management by another means) — plus task decomposition and, later,
parallelism and monitoring.

---

## 1. Problem statement

- **No decomposition / delegation.** Every step shares one conversation and one
  context window. A large sub-task (read 20 files, research an API) pollutes the
  main context with intermediate tool output the user never needed.
- **The user's ask.** "We cannot monitor and spawn agents at the moment which is
  a miss." Two capabilities: *spawn* (run a sub-task) and *monitor* (watch
  long-running / background ones).
- **Skills should reach subagents.** A spawned agent should inherit the parent's
  skills and honor `--skill` (already noted when `--skill` landed).

### Goal

A `spawn_agent` tool that runs a nested, **isolated** agent loop with **inherited**
capabilities (provider, model, permissions, tools, skills, context budget),
returns its conclusion to the parent, and cannot escalate privileges or recurse
without bound. Synchronous first; parallel and background/monitored later.

---

## 2. Design

### 2.1 The `spawn_agent` tool

```
spawn_agent(task: string, [skill: string]) -> string
```

When the model calls it:
1. Build a **fresh** message list: `system(subagent prompt [+ inherited skills])`
   + `user(task)`. The subagent starts with a clean context — none of the
   parent's history.
2. Run the agent loop (`run_agent_turn`) with the subagent's own
   `AgentSessionState` (fresh meter + compaction breaker), the **inherited**
   provider/model/tools/permissions/context config.
3. Return the subagent's final answer as the tool result. Only this conclusion
   enters the parent's context — the subagent's intermediate tool calls and
   reads do not.

### 2.2 Isolation (the point)

- The subagent has its **own conversation and context window**. The parent never
  sees the subagent's rounds — only the returned string.
- This is context management by delegation: e.g. "review these 12 files" reads
  them in the subagent's window and returns a one-paragraph review; the parent
  spends ~a paragraph, not 12 files.
- The subagent's context is trimmed/compacted by the same cascade, independently.

### 2.3 Inheritance

The subagent inherits, so it's as capable as the parent (minus recursion):

| Inherited | How |
|---|---|
| Provider + model | shared (the same gateway connection) |
| **Permissions** | the **same `Permissions`** (`Arc`) — same allowlists, same prompt; grants made in either propagate. No escalation: a subagent can't read/write what the parent couldn't. |
| Tools (fs read/write, grep) | same registry entries |
| **Skills** | the skill index + `skill` tool, and any `--skill` preloads |
| Context budget | same window / compaction mode |

The subagent's tool set **excludes `spawn_agent` itself** in the first cut (see
depth, §2.4).

### 2.4 Depth & recursion safety

- **Depth cap.** The first cut allows **depth 1**: a subagent does not get the
  `spawn_agent` tool, so it cannot spawn further. (A later `max_depth` counter can
  allow bounded nesting.)
- **Round/turn caps.** The subagent runs under the same `--max-rounds` backstop
  and the loop-breaker, so it can't run forever.
- **Permission gate stays.** Every filesystem action the subagent takes still
  goes through the shared permission model.

### 2.5 Result & observability

- The tool returns the subagent's final text. The prompt asks the subagent to end
  with a concise conclusion suitable as a delegated result.
- Activity display: `⏺ spawn_agent("review foo.rs")`, the subagent's own tool
  activity shown **indented** (so nested work is visible but distinct), then
  `  ⎿ <one-line result summary>`.
- The subagent's token usage is attributed in `/context` provenance (a `subagent`
  source / tool-result line) so its cost is honest and visible.

### 2.6 Result of a failed subagent

A subagent that errors (provider failure, hits max rounds with no answer) returns
an error-as-tool-result (design §5) to the parent, which can adapt — never
crashes the parent turn.

---

## 3. Security summary

- **No privilege escalation:** subagents share the parent's `Permissions`; they
  can't touch paths the parent couldn't, and writes still prompt.
- **Bounded recursion:** depth-1 (no `spawn_agent` in subagents) + max-rounds.
- **No new network surface:** subagents use the same provider; no auto-fetch, no
  new endpoints.
- **Skills stay local:** inherited from the same local `SkillSet`.

---

## 4. Non-goals (first cut)

- Background / detached agents and a monitoring UI (that's Stage C).
- Cross-process or remote agents.
- Arbitrary-depth recursion.
- A separate model per subagent (inherits the parent's; a `--agent-model` could
  come later).

---

## 5. Stages of development

**Effort:** S (≤ ½ day) · M (~1–2 days) · L (~3+ days).

### Stage A — Synchronous `spawn_agent` tool  *(M · first)*
- Add the `spawn_agent(task)` tool (registered in `chat` when tools are enabled),
  capturing an `Arc` provider + a subagent tool registry (fs + skills, **no**
  `spawn_agent`) + shared `Permissions` + inherited context config.
- Run a nested `run_agent_turn` with a fresh message list + subagent system
  prompt; return the final text. Depth-1 only.
- Activity display: the spawn header + indented sub-activity + a result summary.
- Inherit skills (index + tool) and honor `--skill`.
- **Tests:** a mock-provider unit test that `spawn_agent` runs a nested loop and
  returns the child's answer without leaking child rounds into the parent; a
  permission test that the subagent can't exceed the parent's allowlist; a live
  check ("spawn an agent to review this file").

### Stage B — Parallel / multiple subagents  *(M)*
- Allow the model to spawn several subagents whose results are collected; run
  them concurrently (bounded) since each has isolated state.
- A `spawn_agents(tasks: [string])` batch form, or concurrent handling of
  multiple `spawn_agent` calls in one round.
- **Tests:** N concurrent subagents complete and their results are all fed back;
  isolation holds (no shared mutable state corruption).

### Stage C — Background + monitoring  *(L)*
- Detached subagents that run in the background with a task registry; a
  `/agents` command (and a tool) to list status / fetch results; the user's
  "monitor" ask. Needs async task management + a status store.
- **Tests:** a backgrounded subagent's status transitions and result retrieval.

### Sequencing

```
A (synchronous spawn_agent)   ← the core capability + isolation
  └ B (parallel)              ← reuses A's isolated-state design
  └ C (background + monitor)  ← the biggest lift; the "monitor" half
```

Start with **A** — it delivers delegation + context isolation, the highest-value
part, and everything (session state, permissions, skills, `--skill`) is already in
place to support it. B and C build on A.

## 6. Open questions

1. Does the subagent get **write** tools by default, or read-only unless the
   parent had `--allow-write`? (Lean: inherit exactly the parent's set.)
2. Result shape — raw final text, or a light structured wrapper (status + result)
   so the parent/model can tell success from "couldn't finish"?
3. Should `/context` show a live subagent as a nested block while it runs, or only
   attribute its cost after it returns? (Start: after.)
4. Naming: `spawn_agent` vs `task` vs `delegate` — pick one the model reliably
   invokes.
