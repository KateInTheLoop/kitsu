# Cycle 14 audit 01 — docs review task (sessions portion of functionality.md) + NEW session-naming UX

Date: 2026-08-12. Auditor model: Fable. Binary: freshly built `target/debug/kitsu`
(includes new `--name` / `sessions rename` / `--resume <name|auto>`).
Worktree: `.claude/worktrees/agent-a85d1387444272cf4` (pinned at a cycle-5-era
commit; `docs/functionality.md` was synced from the main repo's current copy
before both runs so the task reviewed the *current* doc — run diffs below are
against that synced baseline, not the stale worktree HEAD).

## Task (performed twice)

"Review the sessions portions of docs/functionality.md against the actual CLI
behavior (kitsu sessions --help, chat --help). Identify stale or missing claims
and fix up to 2 of them in docs/functionality.md."

Auditor pre-established ground truth so the runs could be graded. Genuinely
stale claims in the doc: (a) both sessions-subcommand enumerations omit the new
`rename`; (b) `--resume [id|last]` misses `name` and `auto`; (c) `--name` is
entirely undocumented.

## Commands and wall times

| Run | Command (abbrev) | Wall time | Outcome |
|---|---|---|---|
| A | `time claude -p '…' --permission-mode acceptEdits` | **74.2 s** | Fixed exactly the 2 pre-identified stale claims; correct scoped diff (`cycle14-runA.diff`) |
| B | `time kitsu --gateway ollama --model qwen2.5-coder --context-window 8192 --max-rounds 40 --timeout-secs 600 --yes chat --name cycle14-docs < prompt` | **296.2 s** | 0 edits (`cycle14-runB.diff` empty); model delegated, confabulated; kitsu receipts truthful throughout |

Run-B prompt note: the parent's command text referenced "these help excerpts"
without including any; the auditor materialized trimmed sessions-relevant
excerpts of `sessions --help` / `chat --help` into the piped prompt (kitsu's fs
tools cannot execute `--help`, and the 8192-token window forces trimming).
Environment note: qwen2.5-coder ran CPU-only (`size_vram: 0`) with ollama
runtime context 4096 while kitsu budgeted 8192 (`--context-window` is
budgeting-only by design), and a leftover 8h-old kitsu chat plus a concurrent
`--jsonl` kitsu chat shared the same ollama — model latency, not kitsu, is the
wall-time story.

## Audit-harness stall note (meta, not a kitsu defect)

The audit itself stalled ~2 h between runs: compound shell commands
(`sleep && tail`, waiter loops with `kill -0 … &`) were refused by the
worktree-isolation verifier as "too complex to verify", and permission prompts
blocked bare tools until the coordinator added bare-tool allow rules to the
worktree's `.claude/settings.local.json`. kitsu timings are unaffected (each
run wrapped in `time` around the process itself).

## Run A evidence (trimmed)

claude's report was fully truthful — both edits verified in `cycle14-runA.diff`:
added `rename` to the two enumerations; rewrote the `--resume` sentence to
`[id|name|last|auto]` with the `last`/`auto` distinction and `--name`. It
explicitly listed what it noticed but left alone (list-filter arg undocumented;
would not guess whether a `/rename` slash command exists since help can't
confirm it). Zero tool noise in `-p` mode.

## Run B evidence (trimmed)

```text
~ session name: cycle14-docs
~ session s-…-3335749-0 autosave planned; … (invocation grants: 0 read, 0 write)
~ still waiting on the model (15s)... / (45s)... / (1m45s)...   <- backoff works, no budget shown
◆ Agent("Review the sessions portions of docs/functionality.md …")   <- qwen delegates whole task
{ "name": "code-review", "arguments": { "file_path": "docs/functionality.md" } }  <- subagent's raw JSON committed as answer text
  ◇ 6 lines
◆ Agent(reviewer: "Review the sessions portions …")
  ◇ Started background agent #1. It runs detached — fetch its result later with agent_result(1) …
~ input ended with 1 background agent(s) running; joining them (up to 600s; Ctrl+C cancels, --agent-join-timeout adjusts)
~ still waiting 15s — 1 background agent(s) running, 0 finished
~ joined 1 background agent(s) before exit
~ background job #1 result (…): Could you please provide me with the specific sessions portion from `docs/functionality.md` …
~ run summary: 1 turn(s) ok, 0 file(s) written (0 change(s)), 1 background agent(s), session s-…-3335749-0
```

Kitsu behavior here is largely excellent: the join-on-exit disclosure, the
joined-result surfacing, and the run summary (0 files written) are all truthful.
The one kitsu-side miss: the bare `{"name":"code-review",…}` blob was committed
verbatim as answer text with **no** "ignored malformed text-form tool call"
receipt — `parse_text_tool_calls` only recognizes registered *tool* names, and
`code-review` is a *skill* name (crates/kitsu-core/src/orchestrator.rs:1404,
name check ~:1434), so the cycle-13 recovery never triggers on the single most
common small-model fault this cycle actually produced.

## New-session-UX walkthrough (all exercised live)

- **Startup announcement:** `~ session name: cycle14-docs` printed immediately;
  duplicate names are refused with a good message naming both remedies
  (crates/kitsu/src/main.rs:9458-9463).
- **Listing:** name leads the row — `cycle14-docs (s-1786544355051449747-3335749-0)
  2026-08-12T… 1 turn(s) ollama/qwen2.5-coder:latest` + directory + preview;
  footer teaches `--resume <id|name|last|auto>` AND `sessions rename`. `--json`
  includes a `name` field. Genuinely good readability.
- **Resume by name (piped, real turn):** `⛩ resumed session 'cycle14-docs'
  (s-…)` with grants disclosure. Worked first try (126.2 s, model-bound).
- **Rename:** `kitsu sessions rename cycle14-docs cycle14-done` → `named
  session s-… 'cycle14-done' — resume it with kitsu chat --resume cycle14-done`.
  Accepts the *current name* as selector. Nit: message says "named", not
  "renamed from 'cycle14-docs'", so a rename is indistinguishable from a first
  naming in scrollback.
- **`sessions resume auto` / `sessions resume <name>` (non-TTY):** both resolve
  and print exact argv without network — but `sessions resume --help` still
  documents the argument as "Session id; defaults to `last`" only.
- **`--resume auto` (piped, real turn):** resolved the worktree cwd to the
  renamed session — `⛩ resumed session 'cycle14-done' (s-…)` — in 1.9 s wall
  (ollama prompt cache hot); `cycle14-resume-auto.log`.
- **Wrong-name error (the big miss):** `chat --resume no-such-name` →
  `! error: cannot resume chat: cannot read session
  '/home/mmpg/.kitsu/sessions/no-such-name.json': No such file or directory (os
  error 2)` (exit 1). A friendly branch EXISTS ("no session named 'X' — see
  /sessions or `kitsu sessions`", crates/kitsu/src/session.rs:231-233) but is
  unreachable for hyphen/underscore names: `valid_id` (session.rs:165) accepts
  the exact same charset as names, so `[] if valid_id(selector) =>
  Ok(selector)` (session.rs:230) treats every mistyped name as a phantom id and
  leaks the raw path. Same hole in `sessions delete cycle14-done` (name given
  where exact id required) → same raw-path error instead of "delete requires an
  exact id; the session named 'cycle14-done' is s-…".
- **Discoverability:** the empty-list hint (`start one with kitsu chat
  --session or /new`, session.rs:204/2099/2179, main.rs:978/996) never mentions
  `--name`; a user would only find naming after already having sessions (the
  populated-list footer does teach it) or by reading `chat --help`.
- **Cleanup:** `sessions delete s-1786544355051449747-3335749-0` succeeded; final
  `kitsu sessions` shows the namespace empty again.

## Per-dimension vs claude

| Dimension | kitsu (this build) | claude CLI | Verdict |
|---|---|---|---|
| Session naming ergonomics | `--name` at creation, announced at startup, name-first listing, rename by current name, teaching footer | No user naming; `--resume` picker shows auto-summaries | **kitsu ahead** on explicit naming; picker parity still open (cycle-13 #1) |
| Resume ergonomics | `--resume <id|name|last|auto>` all work incl. piped; `sessions resume` prints exact argv non-TTY | bare `--resume` interactive picker, `-c` for cwd-latest | Parity-ish: kitsu now has `auto` ≈ `-c`; still no picker |
| Error quality (wrong name) | Raw fs path + os error 2; friendly branch unreachable (session.rs:230) | Picker sidesteps the problem | **claude ahead** — top gap this cycle |
| Docs-edit transparency | n/a (0 edits) — but receipts said exactly that | Clear final report, accurate line refs, disclosed what it left alone | claude ahead (model-bound) |
| Receipts truthfulness | Perfect: "0 file(s) written (0 change(s))", joined-agent result surfaced verbatim | Truthful summary, verified against diff | Tie — kitsu receipts held under model failure |
| Error surfacing | Malformed *skill*-call JSON silently became answer text (no ignored-receipt) | n/a | kitsu gap (orchestrator.rs:1404) |
| Progress visibility | Heartbeat backoff 15s/45s/1m45s, join countdown line; still no `[elapsed/budget]` | `-p` silent then done | carried cycle-13 #6, not new |

## qwen model-fault ledger (not kitsu defects)

1. Delegated the entire task to `spawn_agent` instead of reading the doc
   (round 1), then to a *background* agent (round 2) — never used fs tools.
2. Subagent emitted `{"name":"code-review","arguments":{…}}` — invoking a skill
   as if it were a tool (cycle-13 #5 pattern again).
3. Background agent's "result" was a question ("Could you please provide me
   with the specific sessions portion…") — asked for content it had tools to read.
4. On resume, confabulated "No changes required; the sessions documentation
   matches the CLI behavior accurately" — it had never read the doc; doc had 3
   real stale claims (claude found and fixed 2).

## Ranked top-5 NEW gaps (with one-line fixes)

1. **Mistyped session names produce a leaked-internals error instead of the
   existing friendly one** — `valid_id` accepts the name charset, so the
   `[] if valid_id(selector)` fall-through (crates/kitsu/src/session.rs:230,
   with :165) turns every wrong name into `cannot read session
   '~/.kitsu/sessions/<name>.json' … (os error 2)`. Fix: only fall through to
   id-resolution when the selector matches the generated-id shape
   (`s-<digits>-<digits>-<digits>`); otherwise emit the session.rs:231 "no
   session named" error.
2. **Bare-JSON *skill* invocations are committed as the final answer with no
   receipt** — `parse_text_tool_calls` only matches registered tool names
   (crates/kitsu-core/src/orchestrator.rs:1404); qwen's dominant fault this
   cycle (`{"name":"code-review",…}`) sails through. Fix: when a balanced JSON
   object has a `"name"` matching a *known skill*, count it malformed and send
   corrective feedback ("code-review is a skill — read it, don't call it").
3. **Exact-id commands give the raw-path error when handed a name** —
   `sessions delete cycle14-done` should say "delete requires an exact id; the
   session named 'cycle14-done' is s-…" (resolve the name for the hint even
   while refusing it; crates/kitsu/src/session.rs `exact_id` ~:244).
4. **`sessions resume --help` is stale against its own behavior** — the arg is
   documented "Session id; defaults to `last`" yet accepts names and `auto`
   (verified non-TTY): update the clap doc-comment for the resume `[ID]` arg in
   crates/kitsu/src/main.rs to `<id|name|last|auto>`.
5. **`--name` is undiscoverable at the moment it matters most** — the
   empty-list hint (session.rs:204/2099/2179, main.rs:978/996) says only
   `--session or /new`; append `(name it with --name <name>)` so first-session
   users learn naming before they have an unnamed pile.

Also observed, below top-5: rename's success message says "named" even when it
renamed (lose the old name in scrollback); lifecycle subcommand `--help` pages
bury 2 relevant lines under ~25 inherited inference flags (`--temperature`,
`--agent-spawn-cap`, … on provider-free commands) — sharpest instance yet of
standing help-fatigue item cycle-11 #6.

## Doc-truth ledger (for whoever lands the doc fix in main)

Run A's two fixes are correct and worth cherry-picking to the real
`docs/functionality.md` (see `cycle14-runA.diff`): add `rename` to both
enumerations; `--resume [id|name|last|auto]` + `--name`. Still undocumented
after those: the `sessions list` filter argument.

## Cleanup state

Worktree reset (`git checkout -- docs/functionality.md`, clean); session
deleted; `kitsu sessions` reports none. Artifacts: `cycle14-runA-claude.log`,
`cycle14-runA.diff`, `cycle14-runB-kitsu.log`, `cycle14-runB.diff` (empty),
`cycle14-resume-auto.log`, `functionality-baseline.md` in this scratchpad.
