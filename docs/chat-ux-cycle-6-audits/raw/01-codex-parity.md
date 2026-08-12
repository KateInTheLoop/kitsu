# Cycle 6 — Audit 1: kitsu vs OpenAI Codex CLI UX parity

Date: 2026-08-10. Auditor: UX auditor 1.
Tools: codex v0.147.0 (ChatGPT login, model gpt-5.6-sol, 3 billable runs) vs kitsu 0.1.0 (57889b2a0969) on local Ollama qwen2.5-coder:latest (CPU-only host, 12 cores, 11 GB RAM).
Fixture: /tmp/cycle6-audit1/mathrepo — tiny git repo (cli.py, stats.py with a variance bug, README.md); copy at mathrepo-kitsu for kitsu's write run.

Caveat on raw latency and answer quality: codex ran a frontier cloud model; kitsu ran a 7B Q4 model on CPU (45s–5min per round). Wall-clock and answer-quality deltas are mostly the model, not the CLI, and are excluded from severity judgments except where kitsu's own behavior (deadline handling, discarded output) amplifies them.

---

## Task 1 — "Explain this codebase"

Reproduction:

```
cd /tmp/cycle6-audit1/mathrepo
codex exec --sandbox read-only "Explain this codebase in at most 5 bullet points."

cd /tmp/cycle6-audit1/mathrepo
/home/mmpg/workspace/kitsu/kitsu2/kitsu/target/debug/kitsu \
  --config /home/mmpg/workspace/kitsu/kitsu2/kitsu/kitsu.toml \
  --gateway ollama --model qwen2.5-coder:latest \
  chat --allow-path /tmp/cycle6-audit1/mathrepo \
  "Explain this codebase in at most 5 bullet points." </dev/null
```

Codex: printed a header (workdir, model, provider, approval, sandbox, session id), announced a plan, ran `rg --files` + `sed` itself, and produced 5 accurate bullets with clickable `file:line` references and a token count. 24s.

Kitsu: the model replied **"I need access to the codebase files to perform an explanation. Please provide file paths"** — despite `--allow-path` granting the whole fixture. Zero tool calls, 0 files read.

### GAP 1: model is never told the working directory or granted roots (HIGH)

(a) Repro: the two commands above — the kitsu prompt deliberately says "this codebase" without a path.
(b) Codex: injects workdir/environment context; the agent explores unprompted ("this codebase" resolves correctly).
(c) Kitsu: `chat_system_prompt` (crates/kitsu/src/main.rs:4717) lists tool names and says "use the tools when the user refers to a local file or directory", but never states the cwd or the granted read/write roots. With a prompt that doesn't embed an absolute path, the model has nothing to anchor on and punts back to the user. Retrying with the absolute path spelled out in the prompt made the model list and (attempt to) read files.
(d) Severity: **HIGH** — "explain this codebase" is the canonical first prompt; deny-by-default piped mode plus a root-blind system prompt makes the flagship flow fail even when the user did grant access.
(e) Acceptance test: `kitsu chat --allow-path $PWD "Explain this codebase" </dev/null` in a 3-file fixture must produce at least one `fs_list`/`fs_read_file` call targeting the granted root (assert via `--jsonl` tool events), and the reply must mention at least 2 fixture filenames. Unit-level: `chat_system_prompt` output must contain the launch cwd and every granted root.

### GAP 2: total-deadline timeout discards the whole multi-round turn (HIGH)

(a) Repro: same explain prompt with `--timeout-secs 300` on the CPU host: round 1 took 280s, the List tool ran, then round 2 was cancelled at the deadline with `provisional assistant output, if any, was DISCARDED and was not committed` and exit 2. `time` showed 5m0s spent for nothing kept.
(b) Codex: no user-facing total turn deadline; a slow turn just takes longer, and every intermediate command + output has already been printed/streamed, so nothing is lost.
(c) Kitsu: `--timeout-secs` is a single budget covering all rounds of a turn; on expiry all rounds — including successful tool work — are thrown away. The error text is good (suggests a bigger `--timeout-secs`) but the sunk cost is total. On slow local models the default 120s is nearly always fatal for tool turns; even 300s failed here.
(d) Severity: **HIGH** for local-model users — the combination "default too small + all-or-nothing discard" turns every first tool-heavy run into a 2–5 minute failure.
(e) Acceptance test: run a turn that completes round 1 (one tool call) and then hits the deadline in round 2. Expected: exit is still nonzero, but stdout/JSONL carries the completed tool rounds and a partial marker (e.g. `[stopped: deadline]` plus the round-1 tool results), and stderr suggests `--resume` to continue — instead of `DISCARDED`. Alternative accepted fix: make `--timeout-secs` per-round (attempt) with a separate turn cap.

### Parity notes (task 1)

- Kitsu's `~ inference provider attempt 1/3: success; HTTP 200 ...` line and `kitsu-usage`/`kitsu-context` trailers are genuinely useful diagnostics codex lacks in plain exec mode.
- Kitsu's tool-call rendering `⏺ List(…/mathrepo)  ⎿ 4 lines` is compact and readable — parity with codex's `exec ... succeeded` blocks.
- Answer quality (kitsu's model hallucinated a `<tool_response>` HTML wrapper and guessed from filenames) is the 7B model's fault, but kitsu rendered the raw pseudo-markup without any sanity note — cosmetic, LOW.

---

## Task 2 — "Find and fix the bug in stats.py"

Reproduction:

```
cd /tmp/cycle6-audit1/mathrepo
codex exec --sandbox read-only "Find the bug in stats.py and propose a minimal fix as a unified diff. Do not modify any files."

cd /tmp/cycle6-audit1/mathrepo-kitsu
kitsu ... --timeout-secs 1800 chat --allow-read-write /tmp/cycle6-audit1/mathrepo-kitsu \
  "Find and fix the bug in /tmp/cycle6-audit1/mathrepo-kitsu/stats.py. Read the file first, then apply a minimal fix." </dev/null
```

Codex (26s): correct diagnosis (population variance dividing by n−1; ZeroDivisionError for singletons), a clean fenced `diff --git` hunk, and an explicit closing line "No files were modified." The read-only sandbox is a first-class propose-only mode, and `codex apply` exists to later `git apply` the produced diff to the working tree — a propose→apply workflow kitsu has no equivalent for.

Kitsu (~15 min wall clock, 3 rounds on CPU Ollama — model-bound, not CLI-bound): applied the **identical correct fix**. Transcript showed `⏺ Read(…stats.py) ⎿ 16 lines`, then `⏺ Edit(…stats.py) ⎿ updated · 360 bytes`, the model's summary, and then — a pleasant surprise — a **verified file checkpoint block**: `verified file checkpoint #1`, per-file byte delta and first-changed-line, a **verified snapshot-derived unified diff**, and `/undo 1 restores this checkpoint if its files still match`. `git diff` in the fixture confirmed the same one-line fix codex proposed. Exit 0.

Presentation verdict: post-hoc, kitsu's applied-change presentation (verified diff + checkpoint + undo hint) is at parity with codex and arguably better (codex read-only shows a proposed diff but codex in write mode has no checkpoint/undo). What kitsu lacks is the *propose-only* half of the workflow.

### GAP 3: no propose-only write mode / no `kitsu apply` (MED)

(a) Repro: commands above; try to get kitsu to *propose* a fix without touching disk while still using its edit tooling — the only options are "no write grant" (model can't stage anything; edits fail) or "write grant" (edits land immediately).
(b) Codex: `--sandbox read-only` = "look but don't touch"; the deliverable is a diff; `codex apply` later `git apply`s the latest produced diff. Propose → review → apply is a first-class loop.
(c) Kitsu: capability roots are read-only or writable; there is no mode where `fs_edit_file` succeeds from the model's view but materializes as a pending diff instead of a disk write. The checkpoint/`/undo` system is after-the-fact recovery, not before-the-fact review, and there is no `kitsu apply <artifact>` for a diff produced by a read-only run.
(d) Severity: **MED** — the applied-diff + undo path is genuinely good, but cautious users and CI reviewers expect the codex-style "diff before it lands" contract.
(e) Acceptance test: `kitsu chat --allow-path <root> --propose-writes "fix the bug in stats.py" </dev/null` exits 0 with disk untouched (`git diff` empty), prints a unified diff, and stores it as an artifact; `kitsu apply <artifact-id>` then applies it and `git diff` matches the printed diff.

---

## Task 3 — Automation surface: `codex exec --json` vs `kitsu --jsonl chat`

Reproduction:

```
cd /tmp/cycle6-audit1/mathrepo
codex exec --json --sandbox read-only -o /tmp/last.txt "What does cli.py do? Answer in one short sentence."

kitsu ... --jsonl chat --session "Reply with the single word OK." </dev/null
```

Codex JSONL (10.5s): `thread.started` → `turn.started` → `item.started`/`item.completed` per item, where a `command_execution` item carries the argv, live `status: in_progress`, then `aggregated_output` and `exit_code`; final `turn.completed` with usage incl. cached-token split. `-o FILE` writes just the final message — trivially scriptable. `--output-schema FILE` can constrain the final answer to a JSON Schema.

Kitsu JSONL: versioned `schema: kitsu.chat.v1` events — `run_started` (gateway/model/session id/snapshot state), `turn_started`, `provider_request` (round, tools_offered), `provider_attempt` (attempt/category/outcome/remaining_deadline_ms), `assistant_disposition`, `error` (with exit_code and the same actionable message as human mode), `turn_finished`, `final_summary` with failure taxonomy (user/provider/tool/policy/cancelled). Retry/attempt observability is richer than codex's.

### GAP 4: no "final answer only" convenience in automation (MED)

(a) Repro: try to get just the assistant's answer from `kitsu --jsonl chat` — you must jq-filter the event stream for the assistant text event; there is no `-o`/`--output-last-message`, and no `--output-schema` structured-answer mode.
(b) Codex: `-o file` (plain final message) + `--output-schema` (JSON-Schema-shaped final answer) + plain `exec` already ends with the bare final message on stdout.
(c) Kitsu: human mode interleaves diagnostics on stdout (waiting lines, attempt lines, kitsu-usage/kitsu-context trailers go to stdout in piped mode), so "capture the answer" needs either JSONL+jq or trailer-stripping. Fine for platform builders, friction for shell one-liners.
(d) Severity: **MED** — the JSONL schema itself is at/above parity; the missing convenience layer is what scripts feel first.
(e) Acceptance test: `kitsu chat -o /tmp/ans.txt "..." </dev/null` (or `--final-only`) writes exactly the assistant's final text (no trailers) to the file/stdout; `diff <(cat /tmp/ans.txt) <(expected)` clean.

### Parity notes (task 3)

- Kitsu exit codes are typed (provider=2 etc.) and surfaced in the JSONL `error` event — better than codex's mostly-0/1.
- Kitsu's `final_summary` failure taxonomy has no codex equivalent — parity-plus for CI gating.
- `tools_offered: 4` appears even in a no-grant piped run; events don't say *which* tools or which roots are active. LOW: add granted-root and tool-name fields to `run_started`.

---

## No-extra-run comparisons

### Onboarding / first-run

- `kitsu init` → creates a no-secret `kitsu.toml` (0600) and prints the exact next two commands; `kitsu doctor` is read-only, checks config/auth/context/state, and explains tool posture. Codex: `codex login` (ChatGPT OAuth) + `codex doctor`. Parity; kitsu's no-secret starter + doctor chain is arguably cleaner for airgapped/local use.
- Interactive banner (via pty): kitsu shows gateway, model, **directory**, tool count, and key-hints (`/` commands, Tab, Ctrl+D). Parity with the codex TUI header. Ironic note: the human sees the directory but the model doesn't (Gap 1).

### Progress/status while the model works

- Codex exec: prints each shell command as it runs with duration and output excerpt.
- Kitsu piped: `~ still waiting on the model (10s)...` every 15s, then per-round `provider attempt` lines and `⏺ Tool(...)` markers. Adequate, honest, and `--plain`/`--reduced-motion` accessibility flags exceed codex. Minor gap: no token streaming indication in piped mode, and waiting lines don't say which round you're in (e.g. `round 2/10`). LOW.
  Acceptance test: piped tool turn shows `~ round 2/10: waiting on the model (15s)` style lines.

### Approval prompts

- Codex: sandbox-policy model (`read-only`/`workspace-write`/`danger-full-access`, `--approve-for-me`, approval "never" in exec).
- Kitsu: capability-root model with separated read/write/read-write roots, `--yes` (suppress prompts, keep confinement), exact-argv command approval + OS containment for `command_exec`, `/permissions`, `/revoke-*`. Kitsu is more granular and fails closed; at parity or better. The cost is discoverability: getting a first useful scripted run needs `--allow-path` *and* a path-bearing prompt (Gap 1 compounds this).

### Resume

- Codex: `codex resume` (picker, `--last`, per-cwd filtering), `codex fork`, `codex exec resume`.
- Kitsu: verified end-to-end: `kitsu chat --session "My favorite number is 42..."` then `kitsu chat --resume last "What is my favorite number?"` piped → answered `42`; the resume header explains autosave and states that grants are never restored. `sessions list` shows timestamp, turn count, gateway/model, cwd, and first-prompt excerpt; `sessions resume` outside a TTY prints the exact `cd ... && kitsu ... chat --resume <id>` command plus a JSON-ready exact argv. Parity-plus overall (piped resume, offline management, no-grant-restore policy, printed resume command). Residual niggles: sessions are scoped to the config file path (`sessions list` under a different `--config` shows nothing), which surprises. LOW.
  Acceptance test: `kitsu sessions list` run with a different `--config` at least mentions that other configs hold sessions (or a `--all-configs` flag exists).

### Slash-command discovery

- Kitsu `/help` lists ~45 commands with one-line docs; Tab completion advertised at startup. Parity or better vs codex TUI's slash popup. Not directly comparable in exec mode.

### Error messages

- Unknown gateway: kitsu lists the defined gateways. Missing config: kitsu prints a 4-line quickstart. Bad model: kitsu maps HTTP 404 to "verify the catalog with `kitsu models` or /models, run /model <valid-id>". These are better than codex's generic stderr errors. Parity-plus.

### Context-window footgun (observed, kitsu-adjacent)

`ollama ps` shows the server running the model with CONTEXT=4096 while kitsu's gateway profile claims 8192 (`kitsu-context: {"effective":7680,"window":8192}`). Kitsu budgets against a window the server will silently truncate. The `--context-window` flag exists and the help text even names Ollama's 4096 default, but nothing cross-checks at runtime. MED.
Acceptance test: when the Ollama gateway is active, kitsu queries the served context (e.g. `/api/ps` or model info) and warns `configured window 8192 > server context 4096` before the first turn.

---

## Ranked top-5 gaps

1. **HIGH — Gap 1: model never told cwd/granted roots** → "explain this codebase" fails outright in scripted mode. One-line prompt fix in `chat_system_prompt` (main.rs:4717).
2. **HIGH — Gap 2: total-deadline timeout discards completed tool rounds** → slow-model users pay minutes and keep nothing; keep partial transcript + suggest resume, or make the deadline per-round.
3. **MED — Gap 3: no propose-only write mode + no `kitsu apply`** → codex's read-only-propose → `apply` loop has no kitsu equivalent; kitsu's applied-diff/checkpoint/undo presentation is otherwise at or above parity.
4. **MED — Gap 4: no `-o`/final-answer-only output for automation** → shell scripts must jq the event stream or strip trailers; codex has `-o` and `--output-schema`.
5. **MED — context-window mismatch not detected against live Ollama server** (8192 configured vs 4096 served) → silent truncation risk on the most common local backend.

## Where kitsu is at parity or better

- Error messages (unknown gateway / missing config / 404 model) are more actionable than codex's.
- JSONL failure taxonomy, typed exit codes, retry/attempt observability: better for CI than codex's item stream.
- Session management offline (`sessions list/export/archive`), `--resume` in piped mode, no-grant-restore policy.
- Permission granularity (read vs write vs read-write roots, exact-argv command approval, OS containment) and accessibility flags (`--plain`, `--reduced-motion`).
- Onboarding: `init` → `doctor` → `chat` chain with a no-secret starter config; interactive banner shows directory and tool count.
- `/help` breadth, `/checkpoints`/`/undo`/`/steer`/`/queue` have no codex-TUI equivalents.
- Applied-change presentation: verified snapshot-derived diff + checkpoint + `/undo` hint after every write turn (observed in task 2) — codex has nothing comparable once a write lands.
- Resume: piped `--resume last` verified working; `sessions resume` prints an exact reproducible resume command when not on a TTY.
