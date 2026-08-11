# Cycle 7 — Audit 1: git-centric developer workflows and multi-file changes

Date: 2026-08-11
Baseline: kitsu `6c047a0` (`target/debug/kitsu`), codex-cli 0.147.0.
Model for all kitsu runs: local Ollama `qwen2.5-coder:latest`.
Fixtures: `/tmp/cycle7-audit1/` (`gitfix-*` = dirty tree, 2 unstaged + 1 staged file;
`refactor-*` = 4-file python package + pytest suite). Raw logs in
`/tmp/cycle7-audit1/logs/`. No edits were made to the kitsu repo.

Codex budget used: **2 of 3** authenticated `codex exec` runs (task 1 read-only,
task 2 workspace-write). Task 3 used `--help` only (free).

**Environment caveat.** The Ollama host was shared with at least one other
cycle-7 auditor for most of this session (concurrent `target/debug/kitsu` PIDs
observed against the same server). Single kitsu turns took 2–8 minutes.
All latency and model-quality deltas below are scored as **environmental**, not
kitsu faults. Every finding is a CLI/UX behavior reproducible independently of
model speed. `rg` (ripgrep) is **not installed** on this host, so `repo_grep` was
unavailable to kitsu; codex used `rg` where present and degraded gracefully when
it was not.

---

## Ranked gaps

### 1. There is no verified-diff tool for the model, and `/diff` is gated on granting the *model* read authority

Severity: **high** (blocks the single most common git prompt).

The whole tool roster is `fs_read_file, fs_list_dir, fs_write_file,
fs_edit_file, repo_grep, command_exec, spawn_agent, spawn_agents,
spawn_agent_bg, agent_result, skill, reviewer, read_file` — **no VCS tool of any
kind**. The model cannot see `HEAD`, cannot see the index, and cannot compute a
diff. Kitsu's host-verified diff machinery exists, but only behind `/diff`,
`/review` and the `kitsu review` subcommand, i.e. only where the *user* drives
it — never as something the model can reach mid-conversation.

Reproduction (kitsu, the natural prompt):

```bash
cd /tmp/cycle7-audit1/gitfix-kitsu
echo "review my uncommitted changes and draft a commit message" \
  | kitsu --config .../kitsu.toml --gateway ollama --model qwen2.5-coder:latest \
    chat --allow-path /tmp/cycle7-audit1/gitfix-kitsu --timeout-secs 300 --max-rounds 20 --plain
```

kitsu behavior: 3m46s, one `fs_list_dir` call, then it gave up on the diff and
emitted a raw (malformed, hallucinated) tool call as its answer — see gap 8.
Exit 0. The user gets no diff, no review, no commit message.

Reproduction (the gating half):

```bash
cd /tmp/cycle7-audit1/gitfix-kitsu
printf '/diff working\n/diff staged\n' > d.txt
kitsu ... chat --script d.txt --plain --no-session   # NO --allow-path
```

```
cannot show diff: read permission required for /tmp/cycle7-audit1/gitfix-kitsu:
policy denied: read denied: '...' is outside the allowed read root(s) (none granted)
  — grant it with --allow-path <dir> (or /allow <dir> mid-session)
script summary: 2 failure(s)   → exit 1
```

`/diff` is *host-owned, host-verified* evidence produced by kitsu's own git
plumbing. It should not require handing the model read authority over the repo.
Today, to look at your own diff you must first widen the model's read roots.
This is cycle-6 backlog item 8, still open, and this cycle's task is exactly
where it bites.

Codex behavior: no setup at all. First shell call of the run was
`git status --short --branch && git diff --stat && git diff --cached --stat`,
second was the full `git diff` / `git diff --cached`, all inside `--sandbox
read-only`. Total 2m12s, no approvals, no flags.

Proposed acceptance test: (a) `/diff` and `/checkpoint` succeed on a repo the
user launched kitsu in, with **no** `--allow-path` grant, and `/permissions`
lists the diff source as host-owned evidence rather than a model read root; (b)
a read-granted `chat` turn asking "review my uncommitted changes" makes at least
one call to a diff-producing tool (or auto-attaches the verified working-tree
diff) without the user naming a scope.

---

### 2. `spawn_agent_bg` is offered in scripted/piped mode, and using it is unconditionally fatal there

Severity: **high** (silent 8-minute total loss).

Reproduction:

```bash
cd /tmp/cycle7-audit1/gitfix-kitsu
printf 'review my uncommitted changes and draft a commit message\n' > t1.txt
kitsu ... chat --allow-path $PWD --enable-commands \
  --allow-command '["git","status","--short"]' --allow-command '["git","diff"]' \
  --allow-command '["git","diff","--cached"]' \
  --script t1.txt --timeout-secs 300 --max-rounds 20 --plain
```

kitsu behavior (`logs/1b-commands.log`), **8m02s, EXIT=2, zero useful output**:

```
tool> spawn_agent_bg
result> 1 line
Task started in the background, id: 1. You can retrieve the result later using
`agent_result(1)`, or check on progress by running `/agents`.
script ended with 1 unfinished background agent(s); detached work is not considered successful automation
~ cancelled background agent(s) started by the rolled-back turn: #1
~ background job #1 result (...): cancelled: cancelled because the parent turn was rolled back
script summary: 1 failure(s)
! error: script failed: 1 turn(s) or command(s) failed
```

Three compounding problems:

* All four subagent tools (`spawn_agent`, `spawn_agents`, `spawn_agent_bg`,
  `agent_result`) are registered unconditionally
  (`crates/kitsu/src/main.rs:7972-8004`) with no opt-out flag. In scripted mode
  the model is handed a tool whose use is guaranteed to fail the run.
* Kitsu does not **join** the live agent at script end; it declares the run
  failed *because* the agent is unfinished, rolls the turn back, and then
  cancels the agent it just condemned. The README promises live jobs are joined
  before interactive exit; scripted mode does the opposite.
* The advice that reaches the user (`check on progress by running /agents`) is
  unusable — the process has already exited.

Note it also **out-competed `command_exec`**: with three git argv pre-approved
and explicitly enabled, the model still chose to detach. A 9-tool roster with 4
delegation tools crowds a small model off the task; codex's roster on the same
job is essentially `shell` + `apply_patch`.

Codex behavior: no analogue. `codex exec` has no background-delegation tool
exposed to the model.

Proposed acceptance test: in `--script`/piped mode, either the background
subagent tools are not registered, or a turn that ends with live agents joins
them (bounded by the turn deadline) and commits their results; in no case does
kitsu cancel work it is simultaneously blaming for the failure. A regression
test asserts scripted `spawn_agent_bg` → `background_result{joined:true}` and
exit 0.

---

### 3. Broken pipe panics: exit **101** from every kitsu subcommand

Severity: **high** for CI/dev shells; trivially fixable.

Reproduction (no model, no config needed):

```bash
kitsu --help                      2>&1 | head -3 >/dev/null; echo ${PIPESTATUS[0]}   # 101
kitsu --config kitsu.toml doctor  2>&1 | head -2 >/dev/null; echo ${PIPESTATUS[0]}   # 101
kitsu --config kitsu.toml sessions --json 2>&1 | head -3 >/dev/null; echo ${PIPESTATUS[0]}   # 101
kitsu ... chat --script d.txt --plain 2>&1 | head -N >/dev/null; echo ${PIPESTATUS[0]}   # 101 for N=5,20,80
```

101 is the Rust panic exit code. README's documented taxonomy is `0` / `1` / `2`
/ `130` only (README:852) — 101 is undocumented and indistinguishable from a
real crash. `| head`, `| less` (quit early), and `| grep -q` are everyday
developer git workflows, and `set -o pipefail` scripts will treat this as a hard
failure.

Codex behavior: `codex exec --help | head -3` → 0; `codex --version | head -1` →
0; `git diff | head -3` → 0.

Note the JSONL spec deliberately requires nonzero exit on a broken JSONL sink
(`docs/jsonl-protocol.md`, "Output reliability") — that is fine and correct, but
it should be a *typed* exit (1 or 2) with a one-line notice, not an unwind, and
it should not apply to `--help`.

Proposed acceptance test: for every subcommand, `kitsu <cmd> 2>&1 | head -1`
exits with a documented code (0 for `--help`/`--version`, typed nonzero for a
truncated JSONL sink) and never 101; no panic message is printed.

---

### 4. In human mode the deliverable goes to stderr and stdout is empty

Severity: **medium-high** for automation.

Reproduction:

```bash
cd /tmp/cycle7-audit1/gitfix-kitsu
kitsu ... chat --allow-path $PWD --script d.txt --plain --no-session \
  >/tmp/diff.out 2>/tmp/diff.err
wc -c /tmp/diff.out /tmp/diff.err     # 0 bytes stdout, 2361 bytes stderr
```

`/diff`'s 65-line verified patch, `/permissions`, `/tools`, and command echoes
all land on stderr. `kitsu chat --script show-diff.txt > my.patch` produces an
empty file. README frames stderr as "human diagnostics" and promises "stdout
stays clean for piping" — but for `/diff` the patch **is** the payload, not a
diagnostic, and nothing is left on stdout to pipe.

(This is also what makes gap 3 visible: the panic only reproduces when stderr is
merged into the pipe, because that is where all the bytes are.)

Codex behavior: `codex exec` prints the agent's result to stdout, plus
`-o/--output-last-message FILE` writes the final answer to a file directly.

Proposed acceptance test: `kitsu chat --script <file-with-/diff> > out.patch`
leaves a byte-identical `git diff` on stdout, and `git apply --check out.patch`
succeeds; only spinners/status remain on stderr.

---

### 5. `--allow-command` is hard-bound to the launch cwd and hard-fails the invocation

Severity: **medium-high** (blocks monorepo / repo-parent workflows).

Reproduction:

```bash
cd /tmp/cycle7-audit1                       # parent of the repo
kitsu ... chat --allow-read-write /tmp/cycle7-audit1/refactor-kitsu \
  --command-root /tmp/cycle7-audit1/refactor-kitsu \
  --allow-command '["python3","-m","pytest","-q"]' \
  --script perm.txt --plain --no-session
```

```
! error: invalid --allow-command: policy denied: command cwd '/tmp/cycle7-audit1'
  is outside the approved command roots
exit=1
```

The pregrant is silently bound to the *process* cwd, which is not the command
root the user just declared. The run dies before any work, and the message does
not say what to do (either `cd` into the root, or add the launch dir as a
command root — which widens containment). There is no object form of
`--allow-command` carrying an explicit cwd, and `kitsu` has no `-C/--cd`.

Codex behavior: `codex exec -C <dir>` sets the working root, `--add-dir` adds
extra writable dirs; approvals are not cwd-bound this way.

This is cycle-6 backlog item 9, now with a concrete repro and a user-visible
hard failure rather than a theoretical one.

Proposed acceptance test: `--allow-command '{"argv":["python3","-m","pytest"],
"cwd":"<root>"}'` (object form) is accepted from any launch directory; the
string-array form keeps working; and the current error gains an actionable hint
naming both remedies.

---

### 6. The model is never told which commands are pre-approved

Severity: **medium**.

`command_exec`'s tool description
(`crates/kitsu-tools/src/builtins/command.rs:2817`) explains the argv contract
and the containment model but **never enumerates the approved argv**, and
`chat_system_prompt` (`crates/kitsu/src/main.rs:4987`) only says "Use
command_exec's literal argv/cwd interface for relevant builds, tests, and
linters". In `--script`/piped mode there is no approval prompt, so any argv the
model invents fails closed. The model is asked to guess an exact string array.
Kitsu already knows the list — `/permissions` prints it beautifully, with
executable fingerprints — it just never shows it to the model.

Codex behavior: not applicable in the same way (the sandbox permits arbitrary
argv within the policy), but the effect is that codex never has to guess.

Proposed acceptance test: with `--allow-command '["python3","-m","pytest","-q"]'`,
the system prompt (or the `command_exec` description) contains that exact argv
and its cwd; a scripted turn asked to run the tests issues that argv on the
first attempt.

---

### 7. Verified diff evidence is index-blind

Severity: **medium**.

Reproduction:

```bash
cd /tmp/cycle7-audit1/gitfix-kitsu
kitsu ... review --unstaged --instructions "Draft a conventional-commit message for these changes." --plain
kitsu ... review          --instructions "Draft a commit message." --plain
```

Neither the default `working_tree` scope nor `--unstaged` tells the model which
hunks are staged. `--staged` / `--unstaged` / default are three mutually
exclusive one-shot views, and the evidence block carries no index annotation.
Consequences observed:

* `--unstaged` (`logs/1c-review.log`, 2m26s, exit 0) produced a **false
  positive** created purely by the scope split: *"The addition of
  `store.inventory.reserve` in the README is not mentioned anywhere in the diff
  and appears out of context"* — `reserve` was in the staged half.
* The default `working_tree` review (`logs/1d-review-default.log`) reviews all
  three files but never mentions the index at all, so the drafted message
  silently assumes you will `git add -A`.

Codex behavior: it noticed on its own and answered the real question —

> The index is mixed: only `store/inventory.py` is staged. For all changes,
> after staging the cart and README: `feat: add discounts and inventory
> reservations` … If committing only what is currently staged:
> `feat(inventory): add stock reservations`

Proposed acceptance test: `kitsu review` (default scope) labels each file/hunk
`staged` or `unstaged` in the verified evidence, and the JSONL `command_result`
for `/diff working` carries a per-path index state; a commit-message prompt on a
mixed index yields a message scoped to what is actually staged, or an explicit
statement of which files must be staged first.

---

### 8. A hallucinated / malformed tool call is published verbatim as the assistant's answer, exit 0

Severity: **medium**.

Reproduction: the run in gap 1. The final answer streamed to the user was, in
full:

```
{"name": "spawn_agents_bg", "arguments": {"tasks":["Review the uncommitted changes in the following files:\n- /tmp/cycle7-audit1/gitfix-kitsu/README.md\n- /tmp/cycle7-audit1/gitfix-kitsu/store/\n\nDraft a commit message based on these changes."]}
kitsu-usage: {"prompt_tokens":1776,"completion_tokens":131,"total_tokens":1907}
```

`spawn_agents_bg` is a near-miss for the real `spawn_agents` / `spawn_agent_bg`,
and the JSON is truncated (missing closing brace). `docs/jsonl-protocol.md`
promises "Text from a tool-capable round is held until Kitsu has ruled out a
text-protocol tool call, so raw tool JSON is never mislabeled as an answer" —
the guarantee holds only for well-formed calls to *registered* names. Everything
else is shipped to the user as prose, with `EXIT=0` and a usage line implying
success.

Codex behavior: never observed; malformed calls are retried inside the loop.

Proposed acceptance test: a turn whose final assistant text parses as a JSON
object with a `name`/`arguments` shape is not committed as an answer — kitsu
either retries the round with an unknown-tool error (naming the closest
registered tool) or fails the turn with a typed `tool`-class error; never exit 0
with JSON as the answer.

---

### 9. JSONL has no file-mutation event

Severity: **medium-low**.

`kitsu.chat.v1` (`docs/schemas/kitsu-chat-v1.schema.json`) has 19 event types.
`tool_started` carries `args` as an **opaque JSON string**; `tool_finished`
carries `output` as an opaque string. There is no `file_change` /
`files_written` summary, so "which files did this run touch?" requires a
consumer to know each fs tool's private argument schema and re-parse it.

Codex behavior: one typed `file_change` item per patch —
`{"type":"file_change","changes":[{"path":".../cli.py","kind":"update"}, …×4]}`
— which is exactly the CI-consumable answer.

(Conversely, kitsu's `command` sub-objects on `tool_started`/`tool_finished` are
typed and structured, where codex hands you a raw `aggregated_output` blob. See
parity notes.)

Proposed acceptance test: a scripted turn that writes/edits N files emits a
`file_change`-class event listing each canonical path and `created|updated|
deleted`, and a checkpoint id; concatenating those events reconstructs the
turn's write set without parsing tool args.

---

### 10. Carry-overs re-confirmed on this fixture

* `kitsu review` on a clean tree still dead-ends:
  `! error: nothing to review for verified target working_tree` (exit 1), with
  no hint toward `--commit HEAD` / `--base <rev>` / `@file` chat. Codex's
  `codex exec review` has `--uncommitted`, `--base`, `--commit`, `--title`.
* Nine tools are registered for a simple git question, four of them delegation
  tools, with no `--no-agents`-style opt-out (see gap 2).

---

## Task 2 — multi-file refactor, side by side

Fixture: `notify/{sender,queue,cli,__init__}.py` + `tests/test_notify.py`,
3 passing tests. Same prompt to both tools:

> Rename the class `MailSender` to `MessageSender` and its method `send_mail` to
> `send`. Update every file in this project including the tests, then run
> `python3 -m pytest -q` and make sure it passes.

### Codex (1 run, `--sandbox workspace-write`) — 1m10s, exit 0, tests green

Ten shell calls plus one patch, in this shape:

```
cmd exit 1 : rg --files -g 'AGENTS.md' … && rg -n "MailSender|send_mail"   (rg glob syntax error)
cmd exit 0 : rg --files | sed -n '1,200p'; git status --short              (recovered, different approach)
cmd exit 0 : rg -n "MailSender|send_mail" .; sed -n '1,240p' notify/*.py   (read all call sites)
patch      : [cli.py, queue.py, sender.py, test_notify.py]                 (ONE atomic 4-file patch)
cmd exit 0 : rg -n "MailSender|send_mail" || true; rg -n "MessageSender"   (verify no stale refs)
cmd exit 0 : python3 -m pytest -q                              → 3 passed
cmd exit 0 : git diff --check && git diff -- <4 files> && git status --short
cmd exit 0 : find notify/__pycache__ tests/__pycache__ -type f
cmd exit 1 : unlink <5 files>                                  (unlink takes one operand)
cmd exit 0 : for artifact in <5 files>; do …                   (recovered)
```

Ergonomics worth copying:

* **Atomic multi-file patch.** All four files change in one `file_change` item,
  so there is no window where the tree is half-renamed.
* **Grep-before / grep-after.** It enumerated call sites first and re-grepped
  for stale references after, before running tests.
* **It ran the exact command the user named**, then `git diff --check`.
* **Two command failures (exit 1) did not fail the run.** They came back as tool
  results; the model adapted (glob syntax → different rg invocation; `unlink`
  multi-operand → shell loop) and the turn completed normally.
* **It cleaned up its own build artifacts** (`__pycache__` .pyc files it had
  created by running pytest) so the working tree diff stayed clean.

Final tree: 4 files modified, `3 passed`, no stale `MailSender`/`send_mail`.

### Kitsu

Reproduction:

```bash
cd /tmp/cycle7-audit1/refactor-kitsu
printf '%s\n' "$PROMPT" > t2.txt
kitsu ... chat --allow-read-write $PWD --enable-commands \
  --allow-command '["python3","-m","pytest","-q"]' \
  --script t2.txt --timeout-secs 300 --max-rounds 30 --plain --no-session
```

KITSU_TASK2_RESULT_PLACEHOLDER

---

## Task 3 — automation surface for CI (help/docs only, no codex quota)

| capability | `codex exec` | `kitsu` |
|---|---|---|
| machine event stream | `--json` (`thread.started`, `turn.started/completed`, `item.started/completed` with typed `agent_message` / `command_execution` / `file_change` items, `usage`) | `--jsonl`, schema id `kitsu.chat.v1`, **19 typed event types**, published JSON Schema (`docs/schemas/kitsu-chat-v1.schema.json`) + a written protocol spec with ordering guarantees |
| final answer to a file | `-o/--output-last-message FILE` | **none** (`/save <file>` is a slash command needing write authority and a script line) |
| structured/constrained answer | `--output-schema FILE` (JSON Schema over the final response) | **none** |
| run in another directory | `-C/--cd DIR`, `--add-dir DIR` | **none** — must `cd` first, and `--allow-command` then hard-fails (gap 5) |
| sandbox posture | one flag: `--sandbox read-only\|workspace-write\|danger-full-access` | compose `--allow-path` / `--allow-write` / `--allow-read-write` / `--yes` / `--enable-commands` / `--command-root` / `--allow-command` (finer-grained, but no one-flag preset) |
| multi-turn determinism | `exec resume <id>` / `--last` (append-only) | **`--script FILE`** — one turn per line, `<<< >>>` multi-line turns, slash commands, replayable |
| partial-success policy | none | `--allow-turn-errors` (exit 0 on partial, still nonzero if everything failed) |
| exit-code taxonomy | not documented in help | documented `0/1/2/130` (plus undocumented 101, gap 3) |
| review subcommand | `exec review --uncommitted\|--base\|--commit\|--title` | `review --staged\|--unstaged\|--base\|--commit [PATH] --instructions` |
| isolation from user config | `--ignore-user-config`, `--ignore-rules`, `--ephemeral` | `--no-project-context`, `--no-session` |
| color/plain | `--color always\|never\|auto` | `--plain`, `--reduced-motion`, `NO_COLOR`, `KITSU_PLAIN` |

Reading of this for a CI git workflow: **kitsu's event stream is the better
contract, and its runner is the worse harness.** A CI job that wants "run this
review, give me the diff and the verdict as a file" is one flag away in codex
(`-o out.txt`, optionally `--output-schema verdict.json`) and needs a wrapper in
kitsu (parse `--jsonl` for the last `assistant` event, because human-mode stdout
is empty — gap 4). A CI job that wants to operate on a subdirectory of a
monorepo is `-C sub` in codex and currently impossible-without-`cd` in kitsu,
with a hard error if a command pregrant is involved (gap 5). Conversely, a CI
job that wants a **deterministic replayable multi-turn** script, or wants to
distinguish `committed` / `committed_partial` / `discarded` outcomes, or wants
per-command structured exit/signal/timeout metadata, is well served by kitsu and
not served at all by codex.

Highest-leverage additions for CI parity, in order: `-C/--cd`, an
`--output-last-message`-equivalent, and stdout that actually carries the
deliverable.

---

## Cycle-6 verification (kitsu only)

VERIFY_PLACEHOLDER

---

## Parity-or-better (honest notes)

These are places where kitsu is genuinely ahead of codex on this task angle, and
should not be regressed while fixing the above.

1. **Host-verified diffs are a real trust property codex lacks.** Codex's diff
   is whatever `git diff` printed inside a model-authored shell string; kitsu's
   `/diff` and `review` produce evidence the *host* computed, with an explicit
   `target` (`working_tree` / `staged` / `unstaged` / `base` / `commit` /
   `checkpoint`), `resolved_revision`, `merge_base`, and a `truncated` flag. In
   JSONL that arrives as a typed `command_result` with `kind:"diff"` — a CI
   consumer gets the patch as a field, not by scraping `aggregated_output`.
2. **`/permissions` is far beyond anything codex prints.** It shows read/write
   roots with invocation-vs-session provenance, revoked roots, Kitsu-private
   protected paths, each exact argv grant numbered with its cwd, the resolved
   executable (`observed /usr/bin/python3.13`), the fingerprint binding and
   whether it is revalidated before execution, and the full Bubblewrap mount
   table (`rw <root>`, `ro /usr`, `ro /lib`, …) with namespace isolation stated.
   Codex tells you the sandbox mode and nothing else.
3. **Mandatory OS containment that fails closed.** Commands run under a
   fingerprinted bwrap preflight; exact approval never substitutes for it. The
   containment table was rendered correctly on this host and the preflight
   succeeded without friction — no bwrap-related failures were seen in any run.
4. **Deterministic `--script` multi-turn replay.** Codex has no equivalent;
   `exec resume` is append-only and interactive-shaped. For dogfooding and for
   CI regression suites this is kitsu's strongest automation primitive.
5. **A versioned, published JSONL schema with written ordering guarantees.**
   `kitsu.chat.v1` + `docs/jsonl-protocol.md` + a JSON Schema file is a real
   compatibility contract. Codex's `--json` is undocumented shape-by-example.
6. **`--allow-turn-errors` partial-success semantics** (exit 0 only if *some*
   turn succeeded) is a nuance codex does not express at all.
7. **`kitsu review` did the job the tool is for.** Given the right scope flag it
   returned severity-ranked findings plus a conventional-commit message in one
   tool-free, no-approval, no-filesystem-grant turn (exit 0). That is a cleaner
   trust posture than codex spawning five shell commands to reach the same
   place — the problem is discovery (gap 1), not the feature.
8. **Error text is actionable where it exists.** `read denied: … — grant it with
   --allow-path <dir> (or /allow <dir> mid-session)` names the exact remedy.
   The cycle-6 EMBER notice style renders correctly in `--plain`.

---

## Reproduction index

| log | run |
|---|---|
| `logs/codex1.jsonl`, `logs/codex1-last.txt` | codex, task 1 (read-only) |
| `logs/codex2.jsonl`, `logs/codex2-last.txt` | codex, task 2 (workspace-write) |
| `logs/1b-commands.log` | kitsu task 1, `--enable-commands` + git pregrants |
| `logs/1c-review.log` | kitsu `review --unstaged` |
| `logs/1d-review-default.log` | kitsu `review` (default `working_tree`) |
| `logs/2a-refactor.log` | kitsu task 2 refactor |
| `logs/diff.out`, `logs/diff.err` | stdout/stderr split for `/diff` |
| `logs/cwdbind.out` | `--allow-command` cwd binding failure |
| `logs/clean.out` | `review` clean-tree dead-end |
| `logs/perm.err` | `/tools` + `/permissions` for the refactor setup |
| `logs/v1-roots.log`, `logs/v2*-timeout*.log` | cycle-6 verification runs |
