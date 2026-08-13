# Cycle 20 audit — trouble receipts, impossible pregrants, comment-accuracy comparison

Binary: `target/debug/kitsu` (`kitsu 0.1.0 (b70bffc21b49-dirty, source commit 2026-08-12)`,
built 18:44, audited 18:47 same day). Worktree synced to `b70bffc` (tip of
`worktree-agent-a9cbf0a8880e587b9`, mirroring the shared repo's
`dogfood/codex-ux-cycle-6` HEAD), working tree clean before and after.

HF token used inline per test invocation, redacted below as `hf_***REDACTED***`.

---

## TEST 1 — trouble counts in run summary: VERDICT = fix 1 confirmed, fix 2 confirmed-but-unreachable in piped mode

First attempt (no `--allow-write`) produced no denial at all — the model saw no
write tool registered and refused in plain text ("The functions at hand do not
allow me to perform this task."), so it was rerun with `--allow-write .` to
register the write tool and force a real out-of-root denial, per the task's own
"you MUST attempt the tool call" instruction.

Command:
```
printf '...' | HF_TOKEN=*** ./target/debug/kitsu --config ./kitsu.toml --gateway huggingface \
  --max-output-tokens 1500 --timeout-secs 240 --yes chat --allow-path . --allow-write . 2>&1
```

Verbatim receipt:
```
◆ Write(/etc/kitsu-audit.txt)
  ◇ error: PolicyDenied — policy denied: write denied: '/etc/kitsu-audit.txt' is outside the allowed write root(s) ('/home/mmpg/workspace/kitsu/kitsu2/kitsu') — grant it with --allow-write <dir> (or /allow-write <dir> mid-session)
The tool call is denied because the file path "/etc/kitsu-audit.txt" is outside the allowed write root(s) ('/home/mmpg/workspace/kitsu/kitsu2/kitsu'). To grant write access to this directory, use the --allow-write <dir> option (or /allow-write <dir> mid-session).
kitsu-usage: {"prompt_tokens":6112,"completion_tokens":104,"total_tokens":6216}
kitsu-context: {"used":3178,"effective":129572,"window":131072}
~ provisional assistant output was DISCARDED and was not committed: 1 tool call(s) were denied by local policy during the turn
! error: 1 tool call(s) were denied by local policy during the turn
```
Exit code: 1.

**Fix 1 (untruncated ◇ error, ellipsis only >360 chars): CONFIRMED.** The `◇
error:` line is 226 chars (measured), fully renders the remedy clause ("grant
it with --allow-write <dir> ..."), and correctly carries no ellipsis since it's
under the 360-char threshold.

**Fix 2 (", N denied tool call(s)" in run summary): code confirmed correct, but
unreachable in this exact invocation shape.** No `~ run summary:` line appears
at all in the piped-single-turn output above — the process exits via the
top-level `! error:` path before `emit_run_receipt` (which owns the "N denied
tool call(s)" logic) is ever called.

Traced in `crates/kitsu/src/main.rs`: the piped one-shot handler (around line
10544) calls `strict_turn_failure`, and on `Some(...)` (policy_errors > 0) it
calls `finish_assistant_disposition(..., Discarded, ...)` and `return
Err(mapped)` directly (line ~10601) — never falling through to the
`emit_run_receipt` call that exists later in the same function (~line 10643,
reached only on `TurnOutcome::Completed(Ok(_))` with no strict failure).

To verify fix 2's logic is otherwise correct, the identical prompt/denial was
re-run via `--script` (which unconditionally calls `emit_run_receipt` in
`finalize_scripted_run` regardless of `primary_error`/`body_completed`):
```
~ provisional assistant output was DISCARDED and was not committed: 1 tool call(s) were denied by local policy during the turn
.../test1_script.txt:1: strict scripted turn failed: 1 tool call(s) were denied by local policy during the turn
kitsu-context-rollback: incomplete turn removed from conversation state
~ failed turn rolled back; completed file changes, if any, remain on disk
script summary: 1 failure(s)
~ run summary: 0 turn(s) ok / 1 failed, 0 file(s) read, 0 file(s) written (0 change(s)), 1 denied tool call(s), session not saved
! error: script failed: all 1 turn(s) or command(s) failed (--allow-turn-errors accepts partial success, not total failure)
```
This confirms fix 2's `", 1 denied tool call(s)"` renders exactly as designed
— **when reached**. In `--script` mode it is reached (unconditional
`emit_run_receipt`); in piped/single-turn mode (this audit's TEST 1 shape) it
is not, because the strict-failure branch returns early. **New finding for
cycle 21: fix 2 has a path-dependent gap — piped chat's denial path never
prints a run summary of any kind, so a scripted caller parsing stderr for the
denied-count line will see nothing on the exact invocation shape this audit's
own TEST 1 uses.**

Known cycle-20 queue item #1 (denied turn discards a correct final answer) is
confirmed still present and is the root cause of the above: the entire
provisional turn — including the model's own accurate self-report of the
denial — is discarded and not committed, exiting non-zero with the correct
narrative unread by any downstream consumer of stdout.

---

## TEST 2 — impossible pregrant fails at startup: VERDICT = fix 3 confirmed

**2a — impossible pregrant (executable outside every command root):**
```
printf 'x' | ./target/debug/kitsu --gateway ollama --model qwen2.5-coder:latest --context-window 8192 \
  chat --enable-commands --allow-command '{"argv":["/home/.../target/debug/kitsu","doctor"],"cwd":"/tmp"}' \
  --command-root /tmp
```
Verbatim receipt:
```
! error: --allow-command executable '/home/mmpg/workspace/kitsu/kitsu2/kitsu/target/debug/kitsu' is outside every command/runtime root and can never run under containment — add its directory with --command-root or --command-runtime-root ~/workspace/kitsu/kitsu2/kitsu/target/debug
```
Exit code: 1, failed immediately (no model call, no inference spend). Names
the exact executable and the exact fix (`--command-root` /
`--command-runtime-root <dir>`), matching the spec precisely.

**2b — system binary control case:** launched from `/tmp` (so the pregrant's
implicit cwd — the launch directory, since no explicit `cwd` was given —
matches `--command-root /tmp`) with `--allow-command '["git","--version"]'`:
result was no startup error; the run proceeded to the model call and hit the
15s "still waiting on the model" heartbeat (Ollama endpoint not actually
available in this environment) before the 25s harness timeout killed it —
i.e., it reached inference exactly as expected, no pregrant-validation
rejection.

Note: launching the identical `git --version` pregrant from the repo/worktree
directory (rather than `/tmp`) produces a **different, pre-existing** startup
error unrelated to fix 3 — the pregrant's default cwd (launch directory)
itself falls outside `--command-root /tmp`:
```
! error: invalid --allow-command ["git","--version"]: policy denied: command cwd '.../kitsu2/kitsu' is outside the approved command roots ('/tmp'); the launch directory .../kitsu2/kitsu is outside the approved command roots (/tmp) — pass {"argv":["git","--version"],"cwd":"/tmp"} to bind the grant to a root explicitly
```
This is correct, expected, orthogonal behavior (cwd-root confinement, not
executable-root confinement) — flagging only so it isn't mistaken for a fix-3
regression by a future auditor reproducing this test from a different launch
directory.

**Fix 3: CONFIRMED.** Impossible executable-outside-roots pregrants fail
closed at startup with an actionable message; valid system-binary pregrants
are unaffected and reach the model.

---

## TEST 3 — comparison task (comment-accuracy review of `context.rs`)

Task: "Review the doc comments at the top of crates/kitsu-core/src/context.rs
against what the code actually does; report up to 2 inaccuracies; change
nothing."

| | claude CLI | kitsu + HF Llama-3.3-70B |
|---|---|---|
| Wall time | **40.844s** (`time` real) | **~12.7s** (timestamp delta; consistent with cycle-19's ~12s baseline for this model) |
| Files written | 0 (confirmed via `git diff --stat` — clean) | 0 (`~ run summary: ... 0 file(s) written (0 change(s))`) |
| Files read (receipt) | not disclosed by plain `claude -p` output | 3 (matches 3× `Read(…/context.rs)` tool-call lines in the transcript — honest, though redundant: the same 378-line file read three times across nested skill/subagent calls for a single-file, single-file-scope task) |
| Quality | High: 2 well-evidenced findings, each with exact line numbers and a cited existing test | Weak-moderate: 1 essentially incorrect finding, 1 shallow-but-defensible finding, no line numbers, no test citations |

**claude's findings** (both defensible; finding 1 is exactly right):
1. `MIN_OUTPUT_RESERVE`'s "Always leave at least this many tokens" overclaims
   — `ContextBudget::new` clamps the reserve to `window / 2`, so windows under
   1,024 tokens reserve less than 512. Cited `context.rs:159-164` and the
   file's own test `budget_saturates_on_a_tiny_window` (lines 370-377).
2. The module header's "never a full re-tokenize" is not unconditional —
   `estimate()`'s `_ =>` fallback arm (line 105) and `breakdown()` both do a
   full chars/token recompute whenever there's no valid anchor. More of a
   close/narrow reading than an outright error, but grounded in the code with
   an exact line cite.

**kitsu's findings** (both weaker):
1. Claims `DEFAULT_CONTEXT_WINDOW`'s "small" framing is inaccurate because
   "8,192 is a relatively large value" — this doesn't hold up: 8,192 is
   deliberately small relative to real model windows (the file's own
   `ContextBudget` tests use 32,768 and comment `#[3.2]` explicitly frames it
   as under-filling small windows), so this finding is not a real
   inaccuracy.
2. Claims `MIN_OUTPUT_RESERVE`'s "Always" doesn't hold — directionally the
   *same* real bug claude found (the `window/2` clamp), but stated vaguely
   ("may result in fewer tokens... available") with no line number, no
   mechanism (the clamp), and no test citation — a much shallower catch of
   the same issue.

Independently verified ground truth by reading `context.rs:143-174` and the
test module (`context.rs:257-` area, `budget_clamps_reserve_to_half_window_on_tiny_windows`
/ `budget_saturates_on_a_tiny_window`): the `window/2` clamp is real and does
make the "Always" doc comment inaccurate on tiny windows — so claude's finding
1 (and kitsu's finding 2, in weaker form) is the one genuine inaccuracy either
model surfaced with confidence; claude's finding 2 is a reasonable secondary
catch; kitsu's finding 1 is not a real inaccuracy.

**Verdict: claude wins decisively on quality (precise, line-cited, one
uncontestable real bug found) at ~3.2x the wall time; kitsu is much faster but
delivered one wrong finding and one shallow-but-correct-direction finding, and
spent 3 redundant reads of a 378-line file for a single-file task routed
through an unnecessary subagent hop.**

---

## Ranked top-5 remaining gaps (for cycle 21)

1. **Piped/single-turn chat's denial path never prints any run summary —
   fix 2's "N denied tool call(s)" line is unreachable on exactly this
   audit's TEST 1 invocation shape.** `strict_turn_failure` in the piped
   one-shot handler (`crates/kitsu/src/main.rs` ~line 10544-10601) returns
   `Err` directly, bypassing the `emit_run_receipt` call reachable only from
   the `Committed` success path (~line 10643). Fix 2's logic is correct and
   verified working in `--script` mode (`finalize_scripted_run` calls
   `emit_run_receipt` unconditionally) — the gap is piped-mode-specific.
   Straightforward fix: call `emit_run_receipt`-equivalent from the
   strict-failure branch too before returning `Err`, or route through a
   shared finalizer.

2. **Carried, still open — cycle-19 gap #1 / this cycle's queue item #1:** a
   policy-denied turn destroys a correct/accurate final answer (the model's
   own truthful denial narrative is discarded and unreadable downstream).
   `--allow-turn-errors` only flips the exit code, not the disposition.
   Directly caused this cycle's TEST 1 to need a second invocation and is the
   root cause of gap #1 above.

3. **Redundant subagent delegation inflates cost/latency for trivial
   single-file tasks.** TEST 3's kitsu run read the same 378-line file three
   times (once per skill+Read pair) via an `Agent(...)` subagent hop for a
   task that needed exactly one read and one written answer — 22,970 +
   16,156 = ~39K cumulative tokens against ~7K of actual file content. Worth
   a heuristic: skip subagent delegation for single-file, bounded-size review
   asks.

4. **Model quality gap on close-reading tasks remains the dominant factor,
   not tooling.** Llama-3.3-70B via HF produced one factually wrong critique
   ("8,192 is relatively large") that a stronger model wouldn't make; this
   is a model-choice problem more than a kitsu-UX problem, but it means
   comment-accuracy/code-review genres are a weak fit for the current HF
   default model without a "verify your claim against the actual numeric
   comparison" nudge in the system prompt.

5. **Carried, unresolved:** tool-error receipts elsewhere may still
   hard-truncate in other code paths not covered by fix 1's message (fix 1
   only verified for the `PolicyDenied` write-outside-root message shape);
   capped-read disclosure, worktree gitdir binds, Llama python_tag parsing,
   G5 elision, credential filter, resume picker, and M1 nudge remain queued
   from cycle 19 and were not re-touched this cycle.
