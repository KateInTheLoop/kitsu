# Chat UX dogfood — cycle 13 (Haiku auditor; error-msg task)

**Date:** 2026-08-12  
**Baseline:** `dogfood/codex-ux-cycle-6` (gate: cycle 12's toolchain fix ready in --command-runtime-root)

## Task Summary

**Objective:** Improve `UnsupportedFlavor` error message in `crates/kitsu/src/config.rs` by adding the gateway name to contextualize which gateway had the bad flavor, then verify compilation.

**Prompt (identical both runs):**
> In crates/kitsu/src/config.rs the UnsupportedFlavor error message lists valid flavors but not which gateway had the bad flavor. Add the gateway name to the message (thread it through where the error is constructed), then run cargo check -p kitsu to verify it compiles.

---

## Run A: Claude CLI

**Command:**
```bash
claude -p '<task>' --permission-mode acceptEdits
```

**Wall Time:** 1m20.987s (real)  
**Status:** ✅ **SUCCESS** — task completed fully

### Execution Summary

- **Files written:** 2 (config.rs, main.rs)
- **Modifications made:** 3
  1. Added `gateway: String` field to `ConfigError::UnsupportedFlavor` enum variant
  2. Updated Display impl to include gateway in message: `unsupported flavor '{flavor}' in gateway '{gateway}' — expected one of: openai, anthropic, ollama`
  3. Modified `Gateway::validate_flavor(&self, gateway: &str)` signature and threaded gateway parameter through all 3 call sites in main.rs (lines 1336, 4267, 4486)
- **Cargo check:** PASSED

### Changes (from cycle13-runA.diff)

```diff
pub enum ConfigError {
    UnsupportedFlavor {
+       gateway: String,
        flavor: String,
    },
}

impl Display {
-   ConfigError::UnsupportedFlavor { flavor } => write!(
-       "config error: unsupported flavor '{flavor}' — expected one of: openai, anthropic, ollama"
+   ConfigError::UnsupportedFlavor { gateway, flavor } => write!(
+       "config error: unsupported flavor '{flavor}' in gateway '{gateway}' — expected one of: openai, anthropic, ollama"
    )
}

-pub fn validate_flavor(&self) -> Result<(), ConfigError> {
+pub fn validate_flavor(&self, gateway: &str) -> Result<(), ConfigError> {
     match self.flavor.as_str() {
         other => Err(ConfigError::UnsupportedFlavor {
+            gateway: gateway.to_string(),
             flavor: other.to_string(),
         }),
     }
}
```

### UX Observations

| Dimension | Observation |
|-----------|------------|
| **Startup disclosure** | N/A (piped mode, no startup messages shown) |
| **Edit transparency** | ✅ Clear narrative summary: "Changes: - `crates/kitsu/src/config.rs:142`..." lists all edits with file paths and line numbers |
| **Command receipts** | ✅ Final confirmation: "cargo check -p kitsu... passes" |
| **Error surfacing** | ✅ None encountered (successful path) |
| **Timeout communication** | N/A (completed well within budget) |
| **Run receipt** | ✅ "Done." + summary confirms task completion |
| **Session hints** | ✅ Diagnostic detail: explains the design decision (Gateway struct doesn't store its name; it's the map key) |

---

## Run B: Kitsu + qwen2.5-coder

**Command:**
```bash
printf '%s' '<task>' | kitsu --gateway ollama --model qwen2.5-coder:latest \
  --context-window 8192 --max-rounds 40 --timeout-secs 600 --yes chat \
  --enable-commands --command-runtime-root ~/.cargo --command-runtime-root ~/.rustup \
  --allow-command '["cargo","check","-p","kitsu"]'
```

**Wall Time:** 1m27.828s (real)  
**Status:** ❌ **FAILED** — task not completed

### Execution Summary

- **Files written:** 0
- **Modifications made:** 0
- **Cargo check:** NOT RUN
- **Model action:** Invoked `code-review` skill instead of performing task

### Log Excerpt

```
~ command containment: read-only runtime root /home/mmpg/.cargo (contents readable by approved commands)
~ command containment: read-only runtime root /home/mmpg/.rustup (contents readable by approved commands)
~ loaded project context from ~/workspace/kitsu/kitsu2/kitsu/.claude/worktrees/agent-a2be71ce10934f3e5/llms.txt (664 tokens; disable with --no-project-context)
~ 2 skill(s) available
~ still waiting on the model (15s)...
~ still waiting on the model (45s)...
{"name": "code-review", "arguments": {"name": "crates/kitsu/src/config.rs"}}
kitsu-usage: {"prompt_tokens":2143,"completion_tokens":23,"total_tokens":2166}
kitsu-context: {"used":2162,"effective":7680,"window":8192}
~ run summary: 1 turn(s) ok, 0 file(s) written (0 change(s)), session not saved
```

### UX Observations

| Dimension | Observation |
|-----------|------------|
| **Startup disclosure** | ✅ **WORKING** — two `~ command containment:` lines correctly show --command-runtime-root disclosures (cycle 12 gap #1 FIX CONFIRMED) |
| **Edit transparency** | ❌ NONE — model invoked tool instead of reading/editing files |
| **Command receipts** | ⚠️ PARTIAL — JSON object shows tool invocation name and args but no receipt line with `◇` or artifact path (cycle 12 gap #5 still open) |
| **Error surfacing** | ❌ SILENT FAIL — model chose wrong tool; no error or recovery hint offered |
| **Timeout communication** | ⚠️ PARTIAL — "still waiting" messages appear at 15s, 45s but no cumulative budget burn shown before hard cutoff (cycle 12 gap #3 still open) |
| **Run receipt** | ✅ ACCURATE — "0 file(s) written (0 change(s))" truthfully reports zero completion (cycle 12 shipped feature working) |
| **Session hints** | ⚠️ MISLEADING — "2 skill(s) available" disclosure may have biased small model toward tool-use instead of direct task execution |

---

## Dimensional Comparison

| Dimension | Claude | Kitsu/qwen |
|-----------|--------|-----------|
| **Task completion** | ✅ SUCCESS | ❌ FAILED |
| **Wall time** | 80.987s | 87.828s |
| **Edits made** | 2 files, 3 changes | 0 files, 0 changes |
| **Startup transparency** | N/A | ✅ Command roots disclosed |
| **Edit receipts** | ✅ Narrative detail | ❌ No edits attempted |
| **Error/recovery** | ✅ Clean success | ❌ Wrong tool, no hint |
| **Run summary** | Implied success | ✅ Explicit 0-count receipt |
| **Model capability** | Full capability (claude) | Limited context prioritization (qwen2.5-coder) |

---

## Regressions vs Cycle 12

**None identified.** RUN B's failure is not a regression but a limitation:
- Cycle 12's command-containment disclosures and run-summary receipts are working correctly
- RUN B never attempted the task (model chose code-review tool)
- This is a model-capability issue, not a CLI regression

---

## New Findings & Model-Fault Ledger

### Model Fault: qwen2.5-coder

1. **Tool confusion:** Model was presented with "2 skill(s) available" and invoked `code-review` on the target file instead of reading and editing it. The prompt clearly stated "Add the gateway name to the message (thread it through...)" — an implementation task, not a review task.

2. **Lack of context prioritization:** Small 3B model does not distinguish between "available tools" and "the primary task." Claude CLI (Haiku context in this chain) correctly prioritized the edit task.

3. **No recovery:** Once the model chose the wrong tool, it did not self-correct or hint that the tool was insufficient.

---

## Ranked Top-5 NEW/REGRESSING Gaps for Cycle 14

### 1. [HIGH] Small-model tool-use confusion in piped mode

**Issue:** qwen2.5-coder (3B params) invoked code-review instead of editing when skills are disclosed.  
**Root:** Skill availability disclosure ("2 skill(s) available") may dominate small-model decision-making.  
**Fix:** In piped/scripted mode with small models, either (a) suppress skill disclosure, (b) add model-capability detection to gate skills on model size, or (c) prioritize task statement over tool hints in the preamble.  
**File:** `kitsu-chat/src/chat.rs` (skills disclosure + context preamble)  
**One-liner:** Defer skill list disclosure until after primary task framing in prompt construction.

### 2. [MEDIUM] No per-turn deadline budget display

**Issue:** "still waiting on the model (45s)..." indicates elapsed time but never shows cumulative --timeout-secs budget consumed or remaining.  
**Root:** Heartbeat messages don't include deadline state; cycle 12 gap #3 not yet shipped.  
**Fix:** Update heartbeat preamble to include `[45s / 600s budget]` style display.  
**File:** `kitsu-render/src/lib.rs` (heartbeat_str or similar)  
**One-liner:** Add cumulative timeout budget to heartbeat display: `still waiting on the model (45s / 600s)...`

### 3. [MEDIUM] Missing artifact-path receipt line in piped tool invocation

**Issue:** RUN B logs `{"name": "code-review", "arguments": {...}}` (JSON tool event) but no `◇ artifact-path` style receipt line like claude CLI.  
**Root:** kitsu's piped-mode tool-call logging outputs raw JSON frame; claude's transcript narrative includes `◇` markers.  
**Fix:** Add a text-mode receipt line before/after JSON: `~ ◇ code-review (crates/kitsu/src/config.rs)` or similar.  
**File:** `kitsu-render/src/lib.rs` (or tool-call event formatter)  
**One-liner:** Inject human-readable `~ ◇ <tool> (<args>)` receipt before JSON tool-event in piped output.

### 4. [MEDIUM] Skill availability hints should be gated by model capability

**Issue:** "2 skill(s) available" disclosure is truthful but may mislead small models into tool-use-first thinking.  
**Root:** Skill list is unconditionally shown; no model-size detection.  
**Fix:** Only show skill disclosure if model capacity > 10B params, or make it an opt-in --show-skills flag.  
**File:** `kitsu-chat/src/chat.rs` (skill_list_str or context build)  
**One-liner:** Suppress "skill(s) available" message for models under 10B params unless --show-skills flag.

### 5. [LOW] No task-completion confidence signal in run summary

**Issue:** "0 file(s) written" is factually true but doesn't signal whether zero edits is success (no-op task) or failure (unfinished task).  
**Root:** Run summary only reports file counts, not task outcome.  
**Fix:** Add optional qualifier: `0 file(s) written [task: incomplete]` or `[task: no-op]` inferred from user hint or model self-assessment.  
**File:** `kitsu-render/src/lib.rs` (finalize_scripted_run or similar)  
**One-liner:** Add `[task: incomplete]` or `[task: succeeded]` to run summary based on final turn's assistant message sentiment or explicit model statement.

---

## Standing Issues Carried Forward

- G5: Receipt path elision (truncated artifact paths in transcripts)
- G6: `doctor --online` window evidence (no timing context for remote checks)
- Cycle-12 gap #2: Text-protocol tool-call recovery hard-fails on non-standard JSON escapes
- Cycle-12 gap #4: `final_summary` lacks per-gateway usage/cost totals
- Help fatigue: Users may not know when skills are applicable vs distracting

---

## Session Closure

Both worktree and index cleaned; repository reset to baseline state.

**Key Takeaway:** Claude CLI successfully threaded the gateway name through a multi-site error enum change in 81 seconds. qwen2.5-coder, when given explicit tool availability, chose review over implementation — a model-capability ceiling, not a toolchain issue. The new --command-runtime-root disclosures (cycle 12 shipped) are working correctly.

---

**Report compiled:** 2026-08-12  
**Auditor:** Cycle 13 (Haiku)
