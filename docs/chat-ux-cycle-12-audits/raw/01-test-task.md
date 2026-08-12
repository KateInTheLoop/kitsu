# Cycle 12 — UX audit: "add a unit test + run cargo test" (UI + functionality)

Date: 2026-08-12
Worktree: `/home/mmpg/workspace/kitsu/kitsu2/kitsu/.claude/worktrees/agent-a27b40249399eb589`
Binary under test: `/home/mmpg/workspace/kitsu/kitsu2/kitsu/target/debug/kitsu` (prebuilt from the
MAIN checkout). Method and cycle-11's five known docs-review gaps are in
`docs/chat-ux-cycle-11-audits/raw/01-docs-review.md`; none of those five are re-reported here
except where explicitly noted as "persists, unregressed."

## The task (run identically twice, a coding task this time — not docs)

> Add one focused unit test for the `task_preview` helper in `crates/kitsu/src/subagent.rs`
> (covering the 60-char truncation with the `…` suffix), then run
> `cargo test -p kitsu task_preview` to prove it passes.

`task_preview` (worktree `crates/kitsu/src/subagent.rs:1441-1450`):
```rust
fn task_preview(task: &str) -> String {
    let safe_task = thread::sanitize_and_redact_text(task);
    let first = safe_task.lines().next().unwrap_or("").trim();
    let short: String = first.chars().take(60).collect();
    if first.chars().count() > 60 {
        format!("{short}…")
    } else {
        short
    }
}
```

## Exact commands

**Run A** (Claude Code CLI, headless):
```bash
time claude -p 'Add one focused unit test for the task_preview helper in crates/kitsu/src/subagent.rs \
covering the 60-char truncation with the … suffix, then run cargo test -p kitsu task_preview and show \
it passing.' --permission-mode acceptEdits 2>&1 | tee scratchpad/cycle12-runA-claude.log
```

**Run B** (kitsu + qwen2.5-coder via Ollama, exact task-spec command — no command execution granted):
```bash
time printf '%s' 'Add one focused unit test for the task_preview helper in crates/kitsu/src/subagent.rs \
covering the 60-char truncation with the … suffix, then run cargo test -p kitsu task_preview and show \
it passing.' | kitsu --gateway ollama --model qwen2.5-coder:latest --context-window 8192 \
  --max-rounds 40 --timeout-secs 600 --yes chat 2>&1 | tee scratchpad/cycle12-runB-kitsu.log
```

**Run B2** (supplementary — command execution enabled, to see the UX moment Run B never reached):
```bash
time printf '%s' '<same prompt>' | kitsu --gateway ollama --model qwen2.5-coder:latest \
  --context-window 8192 --max-rounds 40 --timeout-secs 600 \
  chat --yes --enable-commands --allow-command '["cargo","test","-p","kitsu","task_preview"]' \
  2>&1 | tee scratchpad/cycle12-runB2-commands.log
```
`--enable-commands` / `--allow-command` were confirmed to exist via `kitsu chat --help` before use, per
the task's discoverability instruction — see gap discussion below.

## Wall times and outcomes

| Run | Wall time | Exit | Diff | Test actually passed? |
|---|---|---|---|---|
| A — `claude -p` | **1m16.095s** | 0 | 12-line real diff, 1 new `#[test]` | **Yes** — log shows `test subagent::tests::task_preview_truncates_long_tasks_with_ellipsis ... ok` |
| B — kitsu + qwen (no commands) | **1m37.540s** | 0 | **0 bytes** | **No** — model never wrote a test in this file; turn ended after one malformed tool call (see gap 2) |
| B2 — kitsu + qwen (`--enable-commands`) | **10m0.063s** (hit the 600s deadline) | nonzero (`error: chat request timed out after 600s`) | **0 bytes** | **No** — `cargo test` was actually invoked but failed with a toolchain error unrelated to the test itself (see gap 1); turn then timed out on a later round |

Artifacts: `cycle12-runA-claude.log`, `cycle12-runA.diff` (12 lines), `cycle12-runB-kitsu.log`,
`cycle12-runB.diff` (0 bytes), `cycle12-runB2-commands.log`.

Run A's diff (genuine, passing test):
```diff
+    #[test]
+    fn task_preview_truncates_long_tasks_with_ellipsis() {
+        let task = "x".repeat(61);
+        let preview = task_preview(&task);
+        assert_eq!(preview, format!("{}…", "x".repeat(60)));
+        assert_eq!(preview.chars().count(), 61);
+
+        // Exactly 60 chars fits without the suffix.
+        let exact = "y".repeat(60);
+        assert_eq!(task_preview(&exact), exact);
+    }
```

## Per-dimension comparison

| Dimension | Claude Code (`-p`) | kitsu (Run B / B2) | Verdict |
|---|---|---|---|
| Startup feedback | Nothing until the answer | `~ loaded project context …` + `~ 2 skill(s) available` | kitsu wins (unregressed from cycle 11) |
| Streaming / heartbeat | Silent 76s | Backed-off heartbeat; in B2 a single slow round showed a real exponential backoff `15s → 45s → 1m45s → 3m45s` before returning — the fix from `heartbeat_backoff` is working *within* a round | kitsu wins on visibility; still round-blind across rounds (cycle 11 gap 5, unregressed) |
| Edit-tool transparency | Final message narrates files touched; diff matches | N/A in B (no edit attempted); not reached in B2 | claude wins by default here since kitsu never got to an edit |
| Command-execution UX (denial) | N/A (`-p` has full exec by default) | Run B (no `--enable-commands`): the model never tried a shell/command tool at all — it went straight to `spawn_agent` instead, so **no denial message was ever surfaced**, because the tool simply wasn't offered/visible. There is no proactive "commands are disabled; use --enable-commands" hint anywhere in the transcript. | **New nuance**: kitsu doesn't tell a model (or a human) that a command tool *exists but is off* — the tool is just absent from the schema, so the model can't know to ask, and the user gets no discoverability nudge either. Mitigated by the fact `--enable-commands` genuinely exists and is easy to grep in `--help` |
| Command-execution UX (when enabled) | n/a | **Excellent live transparency**: `◆ Command(argv="cargo" "test" "-p" "kitsu" "task_preview"; cwd=...)` then `◇ exit 1 \| 16 ms \| stderr 247 B \| tail="..." \| full output="<artifact path>"` — exit code, duration, byte counts, truncated tail, and a pointer to the full structured JSONL artifact | **kitsu wins decisively** on this dimension — this is genuinely better than anything `claude -p` shows by default |
| Error / timeout surfacing | n/a (no errors) | Two distinct, both good in isolation: (1) command failure surfaced faithfully via the tail above; (2) hard turn timeout: `! error: chat request timed out after 600s - raise the budget with --timeout-secs <n> (e.g. --timeout-secs 1800) for a slow, queued, or tool-heavy request` — actionable, names the exact flag and a concrete alternative value | Tie/kitsu wins on message quality; but see gap 3 — no warning *before* the cliff |
| End-of-run receipts | Structured close narrating what was done | **Nothing** in Run B (silent exit 0 after a swallowed tool call); B2 ends on the timeout error line only, no round/tool/file tally | claude wins decisively (cycle 11 gap 1, unregressed — and B2 shows the receipt gap also hides *why* 10 minutes were spent: 3× `skill`, 1 failed `Command`, no visible round counter) |
| Session persistence hints | Persisted/resumable | Discarded, no notice | claude wins (cycle 11 gap 2, unregressed) |
| Spawn-cap fix (shipped last cycle) | n/a | **Not exercised.** The model's only `spawn_agent` attempt in Run B never reached the cap-counting logic at all — it died earlier at JSON-recovery (gap 2 below). B2 never called `spawn_agent`. So the storm-prevention fix's own instrumentation is unverified by this cycle | Open question, not a regression |

## Transcript evidence (trimmed)

**Run B, complete stdout+stderr** (the entire 1m37s run):
```
~ loaded project context from …/agent-a27b40249399eb589/llms.txt (664 tokens; disable with --no-project-context)
~ 2 skill(s) available
~ still waiting on the model (15s)...
~ still waiting on the model (45s)...
```json
{"name": "spawn_agent", "arguments": {"task": "Cargo new task_preview_test\ncd task_preview_test\necho \'use std::cmp;\nfn task_preview(input: &str) -> String {\n ... }\n\'\nCargo test -p task_preview_test"}}
```
kitsu-usage: {"prompt_tokens":1806,"completion_tokens":204,"total_tokens":2010}
kitsu-context: {"used":1978,"effective":7680,"window":8192}
```
Exit 0. No file touched, no test added, no cargo invocation, no receipt line — just the raw
```` ```json ```` block printed as if it were the model's final answer.

**Root cause, confirmed against source** (`crates/kitsu-core/src/orchestrator.rs`): `extract_json_objects`
(~L1191) correctly scans for a balanced top-level `{…}` anywhere in the text — the surrounding
markdown fence doesn't defeat it, and it does find the object. But `parse_text_tool_calls` (~L1151)
then feeds the candidate straight to `serde_json::from_str::<Value>`, which is strict JSON and
**rejects `\'` as an invalid escape** (confirmed independently — `\'` is not a legal JSON escape;
only `\" \\ \/ \b \f \n \r \t \uXXXX` are). The model's task string contained `\'` (escaping single
quotes the way a shell script would, not JSON), so the *entire* object — despite having a
perfectly valid, real tool name (`spawn_agent`) and being otherwise well-formed — is silently
discarded. `synthesized_call` stays `false`, so the raw text (fence and all) prints as the
"answer" with zero indication a tool call was ever attempted.

**Run B2, key excerpt** (command execution enabled):
```
◆ Command(argv="cargo" "test" "-p" "kitsu" "task_preview"; cwd="…/agent-a27b40249399eb589")
  ◇ exit 1 | 16 ms | stderr 247 B tail="... wasn't specified explicitly, and no default is configured.
  help: run 'rustup default stable' to download the latest stable release of Rust and set it as your
  default toolchain." | full output="/home/mmpg/.kitsu/artifacts/runtime-.../command-output-0.txt"
```
The command tool built the *exact right argv* and kitsu ran it faithfully — but it failed on a
toolchain lookup, not a test failure. Root cause, confirmed against source
(`crates/kitsu-tools/src/builtins/command.rs:56-68`):
```rust
pub const COMMAND_CONTAINMENT_ENVIRONMENT: &[(&str, &str)] = &[
    ("HOME", "/tmp/kitsu-home"),
    ("LANG", "C.UTF-8"), ("LC_ALL", "C.UTF-8"), ("LOGNAME", "kitsu"),
    ("PATH", "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"),
    ("TERM", "dumb"), ("TMPDIR", "/tmp"), ("USER", "kitsu"),
];
```
Every contained command gets a **fixed, fake `$HOME=/tmp/kitsu-home`**, not the real user's home.
The host's real rustup is correctly configured (`rustup show` → `stable-x86_64-unknown-linux-gnu
(default)`, settings in `~/.rustup/settings.toml`) — but inside the sandbox, rustup looks for
`$HOME/.rustup/settings.toml`, finds nothing at `/tmp/kitsu-home/.rustup/`, and refuses to pick a
toolchain. The suggested fix in its own error message (`rustup default stable`) is a **dead end**
inside this sandbox: even if the model ran that command first, `/tmp/kitsu-home` is very likely
re-provisioned fresh per invocation (no bind-mount of the real `~/.rustup`/`~/.cargo`), so nothing
persists between one `Command()` call and the next.

## Model-fault ledger (qwen2.5-coder:latest — NOT kitsu UX faults)

- **Delegation instead of direct work (Run B).** First and only move was `spawn_agent`, never a
  direct file read/write, mirroring cycle 11's "agent storm on a two-file task" pattern.
- **Shell-style escaping inside JSON arguments.** Emitted `\'` for single quotes as if writing a
  shell heredoc, which is invalid JSON — a plausible small-model habit (bash muscle memory bleeding
  into a JSON-emission context) rather than a deliberate malformed call.
- **Markdown-fenced tool call.** Wrapped the JSON in ```` ```json ```` instead of a bare structured
  tool_call — happened to be harmless here because `extract_json_objects` scans past fences anyway.
- **Redundant `skill` calls (Run B2).** Called `skill` three separate times across the run (each
  returning "15 lines"), interleaved with one `Command` call, without ever writing the test file —
  looping/indecisive behavior typical of a small model losing the thread over many rounds.
- **Never wrote the test file in either run.** In neither Run B nor Run B2 did qwen ever attempt an
  `fs_write_file`/edit call on `crates/kitsu/src/subagent.rs` — the actual deliverable was never
  attempted, only delegated (B) or partially executed via `cargo test` on a file that doesn't exist
  yet (B2).
- **Slow rounds.** ~45s-3m45s per round on an 8k window; B2 burned the full 600s timeout across ~4
  rounds without finishing.

## Ranked top-5 NEW (or newly-nuanced) kitsu UI/functionality gaps

1. **`--enable-commands` containment silently breaks any toolchain manager that depends on a real
   `$HOME` (rustup, and by extension cargo) — a showstopper for kitsu's *own* dogfood task of
   running `cargo test`.** The fixed `HOME=/tmp/kitsu-home` (and minimal `PATH`) means rustup can
   never find a default toolchain, and the error's own suggested fix doesn't persist across
   invocations. This is not a hypothetical: it reproduced on the *first* real `cargo test` this
   audit tried under `--enable-commands`, in kitsu's own Rust repo.
   *Fix:* bind-mount (or copy) the real `~/.rustup` and `~/.cargo` into the sandbox `$HOME` for
   Rust-toolchain workflows, or at minimum document the `--command-env RUSTUP_HOME=... CARGO_HOME=...`
   workaround explicitly (today's `--command-env` help text doesn't mention toolchain managers at
   all) and extend the fixed `PATH` to include a cargo/rustup shim directory when granted.
   *Where:* `crates/kitsu-tools/src/builtins/command.rs:56-68` (`COMMAND_CONTAINMENT_ENVIRONMENT`).

2. **Text-protocol tool-call recovery hard-fails on any non-standard JSON escape (e.g. `\'`), even
   when the tool name and structure are otherwise perfectly valid — and silently prints the whole
   payload (markdown fence included) as if it were the model's real answer.** `extract_json_objects`
   already does the hard part correctly (balanced-brace scan through fences); `parse_text_tool_calls`
   then throws the baby out with the bathwater on a strict `serde_json::from_str` failure. This is a
   materially different, more silent failure mode than cycle 11's "unknown tool name" gap — here the
   tool (`spawn_agent`) was real and callable.
   *Fix:* on strict-parse failure, fall back to extracting `"name"`/`"arguments"` with a
   lenient/sanitizing pass (e.g., un-escape or drop unrecognized `\X` sequences) before giving up;
   and when giving up, emit `~ ignored a malformed tool call for '<name>' (invalid JSON: <reason>)`
   instead of printing the raw text as the answer.
   *Where:* `crates/kitsu-core/src/orchestrator.rs`, `parse_text_tool_calls` (~L1151) and
   `extract_json_objects` (~L1191).

3. **The heartbeat backs off nicely within a round but never surfaces how much of the total
   `--timeout-secs` budget has been consumed — so a 600s-budget turn gives zero warning before the
   hard cutoff.** B2's heartbeat correctly showed `15s → 45s → 1m45s → 3m45s` for one slow round,
   but nothing anywhere said "you're at 8m30s of a 10m turn budget" before the terminal
   `chat request timed out after 600s` error. This extends cycle 11 gap 5 (round-blind heartbeat,
   about `--max-rounds`) to a second, distinct budget dimension: wall-clock `--timeout-secs`.
   *Fix:* once cumulative turn time crosses some fraction (e.g. 70%) of `--timeout-secs`, switch the
   heartbeat line to include the budget, e.g. `~ still waiting on the model (round 4, 8m30s of 10m
   budget used)…`.
   *Where:* `Spinner`/heartbeat arm in `crates/kitsu-render/src/lib.rs`, fed by the round/turn-timeout
   accounting in `crates/kitsu-core/src/orchestrator.rs`.

4. **The spawn-cap storm-prevention fix (cap 2 in scripted mode, shipped last cycle) has an upstream
   blind spot: a malformed text-protocol tool call never reaches the cap-counting logic at all.**
   Because gap 2's JSON-escape failure happens *before* any call is dispatched, a model that emits
   several storm-style `spawn_agent` attempts with the same escaping habit would have every one of
   them silently discarded — never counted toward the cap, never reported, never denied with a
   cap-exceeded message. The safety net and the failure path are in different layers and don't talk
   to each other.
   *Fix:* count (or at least log) text-protocol call *attempts* — even ones that fail to parse —
   toward the same per-turn instrumentation used for the spawn cap, so a storming-but-malformed model
   is still visible in end-of-run receipts (once gap 1 from cycle 11 is fixed).
   *Where:* same `parse_text_tool_calls` path (`crates/kitsu-core/src/orchestrator.rs` ~L1151) plus
   wherever `agent_spawn_cap` is enforced in `crates/kitsu/src/subagent.rs`.

5. **Tool-result one-liners stay content-free and repeat-blind for plain in-repo tools, not just
   `Agent()` calls.** B2 called `skill` three times across the run, each rendered as an identical,
   uninformative `◇ 15 lines` — nothing distinguishes "fetched the same skill again" from "fetched a
   different skill," so a user skimming the transcript can't tell whether the model looped or made
   progress without opening the full log. This generalizes cycle 11 gap 5's `◇ 2 lines` complaint
   (there scoped to `Agent()`) to ordinary tool calls, and adds a new angle: no repeat/duplicate-call
   signal at all.
   *Fix:* give `tool_result` a first-line content preview (e.g. the skill name actually loaded) and
   flag immediate repeats of the same tool+args within a turn, e.g. `◇ 15 lines (repeat of round 2's
   call)`.
   *Where:* `tool_result` formatting in `crates/kitsu-render/src/lib.rs` (worktree ~L735, per cycle
   11's pointer), fed by the round loop in `crates/kitsu-core/src/orchestrator.rs`.

## Housekeeping

All three runs' logs/diffs captured to scratchpad. Run A's diff reverted with
`git checkout -- . && git clean -fd` before Run B. Run B and Run B2 produced zero file changes
(confirmed via `git status --porcelain` / `git diff --stat` after each), so no reset was needed
after them. Final `git status`: clean.
