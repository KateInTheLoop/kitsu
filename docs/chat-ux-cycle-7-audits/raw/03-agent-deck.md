# Cycle 7 — Audit 3: the agent deck (background agents, /jobs, /queue, /steer) + identity round 2

Auditor: UX auditor 3. Date: 2026-08-11.
Binary: `target/debug/kitsu` — `kitsu 0.1.0 (6c047a0ee69f, source commit 2026-08-10)`.
Gateway: local Ollama `qwen2.5-coder:latest` via repo `kitsu.toml`. Host CPU heavily
loaded (single foreground turn = 90–250 s); all latency treated as environmental.
Fixtures under `/tmp/cycle7-audit3/`. PTY captures via `script -qec`. No repository
edits. No Codex runs (docs knowledge only, per brief).

Design intent read first: `docs/agent_deck_ui.md`, `docs/design.md` §Agent Deck,
`docs/subagents-design.md`, `docs/ux-design.md`, `docs/ui_guidelines.md`,
`docs/chat-ux-cycle-6.md`, README §Slash commands / §Background agents.

---

## 0. Reachability map — what the agent deck can and cannot do without a TTY

The brief asked for this explicitly. Measured, not inferred.

| Surface | Interactive TTY | `--script` / piped | `--jsonl` | Notes |
|---|---|---|---|---|
| `/profiles` | yes | yes | yes (rich DTO) | JSONL payload is strictly better than the human view (F5) |
| `/jobs list\|show\|steer\|stop\|stop-all\|prune\|close-steering` | yes | yes | yes | full parity |
| `/agent …` | yes | yes | yes | identical handler to `/jobs` (F12) |
| `/agents [id]` | yes | yes | yes | alias; `[id]` ⇒ `show` |
| **spawn a background agent** | **no user command** | **no user command** | **no user command** | model-only tool `spawn_agent_bg` (F2) |
| `/queue list\|add\|remove\|edit\|move\|promote\|clear` | yes | **rejected** | **rejected** | "session commands require interactive chat" (F8) |
| `/steer <correction>` | only as type-ahead during a live turn | **rejected** | **rejected** | at an idle prompt it errors even interactively |
| live `/jobs` during a turn | Unix only, **output deferred to turn end** | n/a | n/a | F3 |
| live `/queue` during a turn | Unix only, output deferred | n/a | n/a | F3 |
| joining background agents at exit | `/quit wait`, Ctrl+D, `/quit` all join | **script end cancels them** and fails the run | same | F2b |

Repro for the gating (2 s, no model):

```
cd /tmp/cycle7-audit3
printf '/queue list\n/queue add x\n/steer go left\n' > gate.txt
kitsu --config <repo>/kitsu.toml --gateway ollama chat --script gate.txt --no-session
# gate.txt:1: session commands require interactive chat
# gate.txt:2: session commands require interactive chat
# /steer is accepted only while a response is running; …
```

The reachability gap itself is the finding: **the two deck controls that shape *what
runs next* (`/queue`, `/steer`) are the only two commands with no non-interactive
path at all**, while every read-only inspection command is fully scriptable. A JSONL
frontend can watch the deck but can never load it.

---

## 1. Findings

Severity: **Critical** (feature does not work) · **High** (blocks the advertised
workflow) · **Medium** (wrong/confusing but workable) · **Low** (polish).

### F1 — [Critical] Both built-in agent profiles are dead on the default local gateway

`spawn_agent_bg({task, profile:"reviewer"})` fails in **0.1 s** with HTTP 400 and
never runs a single round.

Repro (`/tmp/cycle7-audit3/s3.txt`, output `s3.out`):

```
cd /tmp/cycle7-audit3
cat > s3.txt <<'EOF'
Call the spawn_agent_bg tool right now with task "Read /tmp/cycle7-audit3/fix/parser.rs and list its bugs" and profile "reviewer". Do not do the work yourself.
/jobs list
EOF
kitsu --config <repo>/kitsu.toml --gateway ollama --model qwen2.5-coder:latest \
  chat --script s3.txt --allow-path /tmp/cycle7-audit3 --no-session \
  --timeout-secs 400 --max-rounds 4
```

Observed:

```
◆ spawn_agent_bg
  ◇ 1 line
Sure! I've started a background subagent to review …            <- model reports success
jobs: 1 total | 0 running | 0 done | 1 failed | 0 cancelled
  #1 [failed] Review /tmp/cycle7-audit3/fix/parser.rs and identify any bug…
      0.1s | model qwen2.5-coder:latest | tokens 0/0/0 | tools 0 | writes 0
      | last turn failed; 0 tool errors | failure provider_client_error
      recent started -> provider request round 1 -> provider attempt 1/3: client error, rejected
failed: subagent failed: provider request failed (HTTP 400)
```

Cause, confirmed two ways:

1. `crates/kitsu/src/subagent.rs:1655` — the background spawn path sets
   `cfg.reasoning_effort = Some(...)` **unconditionally** for any selected profile
   (`reviewer` ⇒ `High`, `reasoning` ⇒ `Medium`). There is no `None`/omit path:
   `AgentReasoningEffort` has no "inherit/omit" variant, so *every* profile injects
   a `reasoning_effort` field into the request.
2. The same field on the top-level CLI reproduces the identical 400:

   ```
   kitsu --gateway ollama --model qwen2.5-coder:latest --reasoning-effort high ask "say hi"
   # ~ inference provider attempt 1/3: client error; HTTP 400; request rejected; terminal=true
   # ! error: chat request was rejected (HTTP 400) - verify the selected model, gateway flavor, …
   ```

3. Control: the **same spawn with no `profile` field runs perfectly**
   (`s4.txt` / `s4.out`) — `#1 [running] … tokens 348/75/423 | tools 1 |
   recent started -> provider request round 1 -> … -> started tool fs_read_file -> … -> provider request round 2`.

This is the exact hazard the CLI already documents and avoids — `--reasoning-effort`
is *"Omitted by default so non-reasoning endpoints receive no extra request field"* —
and the profile path violates it. Net effect: on Ollama (README's flagship local
gateway, and the config's `default_gateway`), **the profile system advertised by
`/profiles` and by the tool schema is 100 % non-functional**, and the failure is
invisible at spawn time — the model told the user the job had started.

Aggravating: the receipt shows nothing (`◆ spawn_agent_bg` / `◇ 1 line`), so the user
sees a confident success message and only learns of the failure if they later run
`/jobs`. Failure text carries no remediation (`--reasoning-effort`/gateway hint).

**Acceptance:** `AgentReasoningEffort` gains an `Inherit`/`Unset` default; a profile
that does not explicitly request an effort sends no `reasoning_effort` field.
`spawn_agent_bg({task:"…", profile:"reviewer"})` against the bundled `ollama` profile
reaches at least provider round 1 and one tool call. A profile that *does* request an
effort and gets HTTP 400 surfaces one EMBER line naming the profile, the rejected
field, and `/profiles`. Regression test: a mock provider asserting the serialized
request body for a profile-spawned background agent contains no `reasoning_effort`
key unless the profile set one.

### F2 — [High] There is no user-facing way to start a background agent

The deck has list, show, steer, stop, stop-all, close-steering, prune, and a profile
catalog — and **no spawn**. `/help`'s "agents & jobs" group is:

```
/agent [list|show <id>|steer <id> <text>|stop <id>|close <id>]   inspect and control one background agent thread
/jobs  [list|show <id>|steer <id> <text>|stop <id>|stop-all|close <id>]
/profiles                                                        show available specialist background-agent profiles
```

The only spawn path is persuading the model to call `spawn_agent_bg`. On a 7B local
model that took **85 s and an explicit "call the tool right now, do not do the work
yourself" instruction**; a natural phrasing ("review this file in the background")
usually just does the work inline. `/profiles`' own footer admits the design —
`background agents select one with spawn_agent_bg({task, profile})` — i.e. the help
text for a user-facing command explains a tool the user cannot call.

This is the single largest gap against `docs/agent_deck_ui.md` ("task orchestration")
and against a Codex-class expectation: the user is the orchestrator, not a supplicant
to the model's tool-calling reliability.

**Acceptance:** `/agent spawn [<profile>] <task>` (and its `/jobs spawn` alias)
creates a background thread from the interactive prompt **and** from `--script`,
prints one launch receipt naming id + profile + task, emits a
`command_result {"kind":"jobs","action":"spawn","id":N,"profile":…}`, and the new id
appears in the next `/jobs list`. Unknown profile ⇒ one error naming `/profiles`,
exit `EXIT_USER`, no thread created.

### F2b — [High] A script cancels healthy background agents at exit instead of joining them

Same run as F1's control (`s4.out`). The agent was mid-round-2 and healthy:

```
script ended with 1 unfinished background agent(s); detached work is not considered successful automation
~ cancelled background agent(s) started by the rolled-back turn: #1
~ background job #1 result (Count the functions in …):
cancelled: cancelled because the parent turn was rolled back
script summary: 1 failure(s)
```

Two problems. (a) `crates/kitsu/src/main.rs:6361` calls
`cancel_agents_and_report_writes(registry, 0, journal)` at script end — the scripted
path **cancels**, while the interactive path **joins** (`await_background_agents`,
main.rs:8940 and 9556). README promises "`Ctrl+D` or `/quit` does not silently
discard live background agents" but says nothing about `--script`, where the discard
is exactly what happens. (b) The message reuses `report_cancelled_agents`' rollback
wording — the turn *committed*; nothing was rolled back. The text is simply false.

Workaround exists but is undocumented: a trailing `/quit wait` line in the script
does join them — verified (`s6.txt` / `s6.out`):

```
command> /quit wait
exit requested with wait while background jobs are running; waiting for 1 background agent(s) to finish (Ctrl+C cancels them)
```

Nothing in `/help` (`/exit [wait|cancel]` — "exit chat with an explicit
background-job policy"), README, or the failure message connects `wait` to the
scripted-cancellation problem it solves.

And the wait itself is **completely silent**: `await_background_agents`
(main.rs:6076-6100) prints that one line and then blocks on the registry watch with
no heartbeat, no spinner, and no per-agent progress — measured at several minutes of
dead terminal on this host while agent #1 worked through its rounds. This is the same
blind spot as F3, on the exit path: the deck is invisible exactly while you are
waiting for it. (D3's status row should cover this case too.)

**Acceptance:** script end joins unfinished background agents by default (with a
`~ waiting for N background agent(s)…` line and a bounded `--agent-join-timeout`),
or, if cancelling stays the default, the message reads
`script ended while N background agent(s) were still running; cancelled — add
'/quit wait' to join them instead` and never claims a rollback. JSONL emits a
distinct `background_cancelled_at_exit` reason rather than the rollback reason
string.

### F3 — [High] The deck is invisible during the one period it matters — a running turn

Kitsu *does* accept `/jobs …`, `/queue …`, `/steer …` and `/quit cancel` as
type-ahead during a live turn (Unix), and executes them in the collector thread.
But every result is stored in `TypeaheadCaptureResult.outcomes`
(`crates/kitsu/src/main.rs:7441-7482`) and **only printed after the turn ends**
(`main.rs:9427-9438`). PTY evidence (`pty3.raw`, spinner frames stripped):

```
kitsu> /queue add alpha task
~ 1 / 8 prompts queued
  ( ･ᴥ･ ) ～ kitsu is thinking･･ /queue list          <- typed, echoed into the spinner row
  ( ･ᴥ･ ) ～ kitsu is thinking･･ /queue promote 2      <- nothing printed
  … (all further commands silent for the whole turn) …
~ inference provider attempt 1/3: cancelled
~ captured type-ahead: 2 / 8 prompts queued
~ 1 / 8 prompts queued
~ 1 / 8 prompts queued
  [1] beta task
queue refused: prompt queue index 1 is out of range (1 queued)
…                                                    <- everything arrives at once, at the end
```

Repro:

```
cd /tmp/cycle7-audit3
printf '%s\n' /profiles '/queue add alpha task' '/jobs list' '/queue list' /quit > pty3.in
while IFS= read -r l; do printf '%s\n' "$l"; sleep 1.2; done < pty3.in \
  | script -qec "kitsu --config <repo>/kitsu.toml --gateway ollama chat --no-session" pty3.raw
```

Compounding it, the TTY spinner (`crates/kitsu-render/src/lib.rs:769-793`) carries
**no elapsed time and no deck state** — just four fox frames and
`kitsu is thinking`. (The *non-TTY* heartbeat does have `(Ns)`; the TTY does not.)
So during a 4-minute foreground turn with three agents running, the terminal shows
one animated line that says nothing, and asking it anything shows nothing until the
turn ends. An async external-printer path already exists and is used for background
completion notices (`main.rs:8829-8845`) — the plumbing is there, it just isn't used
for typed deck commands.

**Acceptance:** `/jobs list` typed during a running turn prints its listing within
1 s, above the status row, via `PromptSafeAsyncNotice`/the external printer, and is
not repeated in the post-turn block. The animated status line shows elapsed time and,
when non-zero, running-agent and ready-result counts.

### F4 — [High] `/jobs steer` silently means "throw away this agent's work and restart it"

`crates/kitsu/src/subagent.rs:1378-1413`: on a correction the in-flight
`run_agent_turn` future is **dropped** and the loop restarts with a *fresh*
transcript (`system + task + latest correction`). Consequences, none of them
surfaced:

- All of the agent's completed rounds, tool reads, and partial answer are discarded.
  On a local model that is routinely minutes of work.
- `latest_correction` is **overwritten**, not accumulated — send two corrections and
  only the newest is ever in the transcript. Earlier ones are silently dropped even
  though the receipt counted them as "pending".
- The receipt is neutral to the point of misleading:
  `~ steering queued for background agent #1 (sequence 1; 1 pending)`.
  "Queued" implies "will be taken into account", not "restarted from zero".
- `/jobs show` reports `steering: open (N pending)` and never "applied", so the user
  cannot tell whether a correction landed.

Foreground `/steer` is honest by comparison — it says
`partial response rolled back and the turn will restart before queued prompts` — but
even there the correction text is never echoed back, so the user cannot confirm what
kitsu heard.

**Acceptance:** `/jobs steer N <text>` prints the restart semantics and the discarded
elapsed time; `/jobs show N` gains `steering: applied <k> · pending <n> · last "<text>"`;
JSONL steer receipt gains `"restarts_thread": true` and `"discarded_ms"`. Foreground
`/steer` echoes a bounded preview of the accepted correction.

### F5 — [Medium-High] The human deck renders Rust `Debug`; the JSONL renders a proper DTO

`/profiles` on a styled TTY, in `--plain`, and in `--script` — byte-identical, all three:

```
background-agent profiles:
  reasoning | model current | reasoning Medium | tools none
  reviewer | model current | reasoning High | tools AllowList(["fs_read_file", "fs_list_dir"])
  background agents select one with spawn_agent_bg({task, profile})
```

The same command in `--jsonl` emits, per profile: `instructions`, `reasoning_effort:
"high"`, `tool_policy: {"mode":"allow_list","tools":[…]}`, and
`permission_defaults: {inherit_session, read_roots, write_roots, developer_commands}`.

So the machine view is *strictly richer and better formatted* than the human view.
Concretely wrong in the human view:

- `AllowList([...])` — raw `format!("{:?}", profile.tool_policy)` (main.rs:6054).
- `reasoning Medium` / `High` — `{:?}` on the enum, CamelCase, while `/reasoning`
  uses lowercase `medium|high` and JSONL uses `"high"`.
- `model current` for both profiles is a placeholder that reads like a model id.
- `instructions` — the one field that tells you *what the profile does* — is dropped.
- `inherit_session` (the difference between `reviewer` and `reasoning` that actually
  matters for permissions) is dropped.
- The trailing hint is indented **at the same level as the profile rows**, so it
  parses as a third profile named "background".

Same class of leak in `/jobs show` (`format_agent_thread`, main.rs:5657):
`agent #1 [Running] … handoff: Unavailable` and timeline rows
`[3 +1.2s Provider] …` — CamelCase Rust variants in user-facing text where the DTO
already carries snake_case.

Note also that README calls `reviewer` "read-only", but with no `--allow-path` the
rendered policy is `AllowList([])` — i.e. *no tools at all*, silently, because the
profile's registry is intersected with the invocation's real read authority. Honest
behaviour, invisible explanation.

**Acceptance:** no `/profiles`, `/jobs`, or `/agent` human output contains
`AllowList(`, `[Running]`, `Unavailable`, or any other CamelCase enum; every rendered
label equals its JSONL DTO string. `/profiles` shows each profile's instruction line
and whether it inherits session grants, and explains an empty tool list
("no tools — this run granted no read roots"). Golden-output tests for styled,
`--plain`, and JSONL.

### F6 — [Medium] Live-turn errors are all labelled `queue refused:`, and the live `/jobs` subset contradicts `/help`

`main.rs:9436` prints every `TypeaheadOutcome::QueueError` as `queue refused: …`,
including `/jobs` errors, producing (verbatim, `pty3.raw`):

```
queue refused: active-turn controls: /jobs [list|show <id>|steer <id> <text>|stop <id>|stop-all]
```

Three defects in one line: the channel label is wrong (`/jobs prune all` is not a
queue operation); `prune`/`close`/`close-steering` are silently unavailable mid-turn
(`handle_active_jobs_command`, main.rs:7261-7338, has no arm for them); and the usage
string it prints differs from both `/help`'s `/jobs […|close <id>]` and the idle
handler's `/jobs […|close-steering <id>|…|prune <id|all>]`. Three different published
grammars for one command.

**Acceptance:** `/jobs` errors print `jobs refused: …`; the live subset either
supports the full grammar or names the excluded verbs explicitly ("prune and
close-steering are available at an idle prompt"); one shared usage string is derived
from the registry so `/help`, completion, idle errors, and live errors cannot drift.

### F7 — [Medium] Queue index errors leak 0-based internal indices into a 1-based UI

Verbatim from `pty3.raw` with one prompt queued:

```
/queue promote 2  ->  queue refused: prompt queue index 1 is out of range (1 queued)
/queue move 1 2   ->  queue refused: cannot move queued prompt from 0 to 1 (1 queued)
```

`parse_queue_position` (main.rs:9635) subtracts 1; `execute_queue_command`
(main.rs:9711) re-adds `+1` for `Remove`/`Edit` messages but forwards
`Promote`/`Move` errors via `error.to_string()` unchanged, and
`crates/kitsu/src/prompt_queue.rs:85,131` format the raw internal index. So the user
types 2, is told about "index 1", and `move 1 2` becomes "from 0 to 1".

**Acceptance:** every user-visible queue position is 1-based; a unit test asserts
that no message emitted for input `promote 2` / `move 1 2` contains `0` or `1` as a
position when the user typed `2` and `1 2`.

### F8 — [Medium] `/queue` and `/steer` have no non-interactive path at all

See §0. Even the read-only `/queue list` is rejected as a "session command"
(main.rs:6917-6930 lumps `QueueAction` with `/new`, `/resume`, `/fork`, `/export`,
`/archive`, `/restore`, `/delete`). The message names a category the user never
chose, gives no reason, and offers no alternative — while the neighbouring `/jobs`,
`/profiles`, and `/agent` are fully scriptable and JSONL-typed.

**Acceptance:** `/queue list|add|remove|edit|move|promote|clear` works in `--script`
and emits a typed `command_result {"kind":"queue", …}`; `/steer` in a script gives a
specific error ("`/steer` targets a running turn; a script has no concurrent turn —
use `/queue add` to sequence work") rather than the generic session-command refusal.

### F9 — [Medium] `/queue add X` at an idle prompt runs X immediately

There is no way to stage a batch. The interactive loop dequeues before reading a new
line (main.rs:8913-8930), so `/queue add alpha` at an idle prompt starts `alpha` on
the next iteration. PTY evidence: `~ 1 / 8 prompts queued` is immediately followed by
the spinner. Combined with F8 (no scripted queue) there is **no way at all** to load
a work list and then start it — the core batching move a Codex-class user expects.

**Acceptance:** either a `/queue hold` / `/queue run` pair, or `/queue add` at idle
states plainly that the queue drains immediately (`~ queued 'alpha' — running now`),
and a scripted queue (F8) covers the batch case.

### F10 — [Medium] The spawn receipt is anonymous — no task, no profile, no id

Verbatim from both spawn runs, on stderr:

```
◆ spawn_agent_bg
  ◇ 1 line
```

`format_tool_header` (`crates/kitsu-core/src/orchestrator.rs:1426`) special-cases
`command_exec` and `repo_grep`, then falls back to `args["path"]`. The four
agent tools (`spawn_agent`, `spawn_agents`, `spawn_agent_bg`, `agent_result`) have no
`path`, so they render as a bare name. The tool *returns* `Started background agent
#1 …`, and `format_tool_result` collapses that to `1 line`. So the single most
important event in the deck — "a thread was launched, id N, doing X" — produces two
lines that say nothing.

`docs/subagents-design.md` §2.5 specifies exactly the missing thing:
`⏺ spawn_agent("review foo.rs")` … `⎿ <one-line result summary>`.

**Acceptance:** `◆ Agent(#3 reviewer: "review crates/…/subagent.rs")` on spawn and
`◇ started · /jobs show 3` as the result line; `spawn_agents` shows the batch size
and each task label; `agent_result` shows `#3 → done, 1.4 KB` or `#3 → still running`.
Extend `format_tool_header` with the agent-tool arm and cover it with the existing
header unit tests.

### F11 — [Medium] Synchronous `spawn_agent` sub-activity is not visually nested

`docs/subagents-design.md` §2.5: "the subagent's own tool activity shown **indented**
(so nested work is visible but distinct)". `bg_cfg.quiet = true` is set only for the
*background* path (main.rs:7992); the synchronous `spawn_agent` inherits
`quiet: cli.jsonl` (main.rs:2064) ⇒ `false` on a TTY. A nested subagent therefore
prints `◆ Read(...)` / `◇ 42 lines` at the *same* indent as the parent's own tool
calls, with no marker of whose work it is. Two agents' receipts interleave
indistinguishably.

**Acceptance:** a synchronous `spawn_agent` turn renders the child's tool receipts
indented one level under the spawn header, and `--plain` prefixes them
(`  tool> …` / `  result> …`); a test asserts the child's lines carry the extra
indent while the parent's do not.

### F12 — [Medium] `/agent`, `/agents`, `/jobs` are three published grammars over one handler

`main.rs:10450-10470`: `/agent <arg>` forwards `arg` verbatim to
`handle_jobs_command` — the same function `/jobs` uses. So `/agent stop-all`,
`/agent prune all`, and `/agent close-steering 2` all work, and `/agent list` prints
the whole fleet, despite README describing `/agent` as "**one** bounded, redacted
agent timeline". `/agents [id]` is a third spelling. Published grammars:

| Source | `/jobs` grammar |
|---|---|
| README | `list\|show\|steer\|close-steering\|stop\|stop-all\|prune ID\|prune all` |
| `/help` + Tab completion (`SLASH_COMMANDS`, main.rs:2358) | `list\|show\|steer\|stop\|stop-all\|close <id>` |
| idle usage error (main.rs:5896) | `list\|show\|steer\|close-steering\|stop\|stop-all\|prune <id\|all>` |
| live-turn usage error (main.rs:7336) | `list\|show\|steer\|stop\|stop-all` |

`close` and `prune` are the same arm (`"prune" | "close" =>`), but only one name is
in `/help` and only the other is in the usage text.

**Acceptance:** one grammar string derived from the registry feeds `/help`,
completion, and every usage error; `/agent` either becomes a genuine single-thread
view (`/agent <id>` ⇒ show) or is documented as an alias like `/agents`.

### F13 — [Low-Medium] No subcommand or job-id completion

`Completer::complete` (main.rs:2701) returns nothing as soon as the typed text
contains whitespace. So Tab completes `/jo` → `/jobs`, and then goes dead: none of
the 8 `/jobs` verbs, 7 `/queue` verbs, 2 profile names, or live job ids are
completable. The deck's whole vocabulary is memorization-only.

**Acceptance:** `/jobs <Tab>` offers the verbs, `/jobs show <Tab>` offers live ids
with a task preview, `/agent spawn <Tab>` offers profile names; `--plain` (canonical
line reader) is unaffected.

### F14 — [Low-Medium] `/queue <typo>` silently becomes a queued prompt

`parse_queue_command`'s fallback arm (main.rs:9697) preserves the `/queue <prompt>`
shorthand, so `/queue lst` enqueues a model turn whose text is `lst`, and
`/queue promot 1` enqueues `promot 1`. Top-level slash commands suggest close matches
on a typo; the queue's own subcommands do not, and the failure mode is a wasted
model turn.

**Acceptance:** a first word within edit distance 2 of a known verb produces
`did you mean /queue promote?` and does not enqueue; anything else keeps the
shorthand.

### F15 — [Low] `/status` reduces the whole deck to one number

`agents       0 running` — no done/failed/ready-result counts, no oldest-running age,
no capacity. `/jobs list`'s header already computes all of it.

### F16 — [Low] Type-ahead echo lands inside the spinner row

Terminal echo writes the typed line at the cursor, which the spinner is repainting
with `\r`, producing `( ･ᴥ･ ) ～ kitsu is thinking･･ /queue list` (see F3 capture).
Cosmetic but it is the deck's primary interaction surface. A reserved composer line
below the status row (or suspending the spinner repaint while `foreground_typeahead_active`
sees pending input) fixes it.

### F17 — [Backlog, confirmed] Streaming output receives **no** markdown or code styling

`kitsu_render::print_fragment` (lib.rs:277) writes sanitized bytes straight through;
`format_message`'s dimmed fences / bold headings / diff colouring (lib.rs:305-341)
runs only from `print_message`, which the orchestrator calls **only on the
non-streamed path** (`orchestrator.rs:799,1082`). Streaming is the default. Measured
on a real pty (`cb_stream.raw`, `cat -A`, spinner lines removed):

```
^[[2K```$
$
fn add_two(a: i32, b: i32) -> i32 {$
    a + b$
}$
```$
^[[2m  ↳ 54 input · 30 output · 68 / 7,680 context (0% used)^[[0m$
```

The only escape bytes in the entire answer are the spinner's `\e[2K` and the dim
stats line. A code answer on a styled TTY is byte-for-byte `cat`.

The same prompt with `--no-stream` on the same pty (`cb_nostream.raw`) proves the
divergence — the fences *are* styled there:

```
^[[2K^[[2m```rust^[[0m        <- opening fence, dimmed
…
^[[2m```^[[0m                  <- closing fence, dimmed
```

So `/review`'s diff colouring and the dimmed-fence/bold-heading treatment are
invisible unless the user passes `--no-stream`, which nothing suggests they should.

Sub-defect found in the same capture: `format_message`'s fence tracker is a naive
toggle, so a ```` ``` ```` inside a rustdoc `///` comment flips it and mis-scopes the
rest of the message. Any boxing work must use a fence tracker that respects the
opening fence's length/indent. See proposal **D7**.

---

## 2. Ranked top-5

1. **F1 — built-in agent profiles are dead on the default gateway** (Critical).
   `/profiles` advertises two workers; both 400 instantly on Ollama because profiles
   unconditionally send `reasoning_effort`. The whole profile feature is unusable
   out of the box, and the failure is reported to the user as success.
2. **F2 + F2b — the deck has no front door and no exit door**. No user command
   starts an agent (model-only `spawn_agent_bg`), and a scripted run cancels healthy
   agents at exit while claiming a rollback that never happened.
3. **F3 — the deck is invisible while a turn runs.** Live `/jobs` output is buffered
   to turn end and the TTY status line shows neither elapsed time nor agent state, so
   the monitoring half of "spawn and monitor" does not exist in practice.
4. **F4 — `/jobs steer` destroys the agent's work without saying so**, keeps only the
   newest correction, and never confirms application.
5. **F5 — the human deck is a Rust `Debug` dump while the JSONL beside it is a clean
   DTO** (`AllowList([...])`, `[Running]`, `handoff: Unavailable`), and drops the
   profile fields (`instructions`, `inherit_session`) that decide which profile to use.

---

## 3. Against `docs/agent_deck_ui.md` (the design intent)

The doc is 18 lines and promises four things plus a layout. Scorecard:

| Doc promise | Status | Evidence |
|---|---|---|
| "concurrent agents" | **met** | `max_running: 8`, `max_records: 128`; `spawn_agents` batch form exists |
| "structured attention system" | **not met** | one flat `#N [status]` text list, no glyphs, no colour, no ordering by attention, no "needs you" state; nothing surfaces during a turn (F3) |
| "tmux-like detach/attach" | **half met** | *detach* is real and well-built (bounded, redacted per-thread timeline, `quiet` background rendering, permission fail-closed). **Attach does not exist** — there is no way to watch a running agent's output; `/jobs show` explicitly withholds it ("partial response text withheld until the job is terminal") |
| "task orchestration" | **not met from the user's side** | the user cannot dispatch, only the model can (F2); the queue cannot be pre-loaded (F8/F9) |
| "Agents \| Output" two-column layout | **not attempted** | single-column stderr text; no panes, no split |
| `design.md` §19 "detach/attach workflows similar to tmux" | same as above | |

**Where the implementation exceeds the doc** — and these are genuinely strong, worth
protecting in any redesign:

- **Bounded, redacted thread state** (`subagent/thread.rs`): a versioned, validated
  DTO with explicit event/byte/entry caps, no tool args, no provider bodies, no call
  ids. Nothing in the doc asked for this; it is the right foundation for a real deck
  and for persistence.
- **Honest live telemetry per agent**: model, prompt/completion/total tokens, tool
  count, write count, `last_activity`, and a `recent` activity chain
  (`started -> provider request round 1 -> … -> started tool fs_read_file -> …`)
  while withholding partial model text. That is better observability than the doc's
  "logs" column implies.
- **Capacity as a first-class number** (`7 running slot(s), 127 record slot(s)`).
- **Terminal-handoff discipline**: unobserved results are printed before a session
  switch or exit, emitted as `background_result` in JSONL, and an unseen *failed* job
  makes strict automation non-zero. The doc says nothing about result custody.
- **Permission posture**: background agents never claim stdin, cannot open approval
  prompts, and fail closed — spelled out in README and enforced via
  `scope_without_interactive_prompts`.
- **Steering exists at all**, both foreground and per-thread, with a bounded steering
  channel and an explicit close. The doc never mentions steering.

Net: the *state model* is well ahead of the doc; the *presentation and the user's
control surface* are well behind it.

---

## 4. Against Codex CLI (conceptual; no billable runs)

From documentation knowledge only, and from what a Codex-class user carries into
kitsu:

- **The user dispatches parallel work, not the model.** Codex's parallel story is
  user-driven (multiple `codex exec` invocations / cloud tasks the user starts and
  lists). Kitsu inverts this: only the model may spawn. Nothing else in this audit
  matters as much (F2).
- **A task list is a first-class, always-visible object.** Codex-class TUIs keep a
  persistent status region that survives while work runs. Kitsu's equivalent
  (`/jobs list`) is a one-shot dump you can only read between turns (F3).
- **Non-interactive parity is expected.** A Codex user assumes anything they can do
  interactively they can script. Kitsu meets this for inspection and misses it for
  `/queue` and `/steer` — the two *control* verbs (F8).
- **Steering appends, it does not restart.** Interrupt-and-redirect in a Codex-shaped
  UI is understood as "add guidance to the running thing". Kitsu restarts from zero
  and calls it "queued" (F4). Even if restart stays the mechanism, the word must
  change.
- **Rendering is expected to be typed, not `Debug`-printed.** (F5.)
- **Where kitsu already wins:** typed exit codes and a JSONL failure taxonomy for
  detached work; result custody (nothing silently lost at exit); explicit capacity
  limits; a redaction contract on thread state; and a permission model that makes
  detached agents fail closed instead of prompting from the background. If F1–F5 are
  fixed, kitsu's deck is *more* trustworthy than the parallel-work story a Codex user
  arrives with — it is currently just unreachable.

---

## 5. Design proposals — identity round 2 (max 8)

All glyphs come from the set `docs/ux-design.md` already defines (`◆ ◇ ◈ ◉ ⛩`) and
the six named colours (`FOX`, `AMBER`, `CREAM`, `TEAL`, `SAGE`, `EMBER`). No new
symbol vocabulary is introduced; `CREAM` finally gets its reserved use in D1.

### D1 — The deck as a `◈` section with fill-state status glyphs

Replace the flat `#N [status]` list. Fill level carries state, which is already the
established semantics (`◆` filled = action, `◇` hollow = spent result, EMBER = error):

```
◈ agent deck — 2 running · 1 ready · 1 failed · 6 slots free        (◈ FOX, header CREAM bold)
  ◆ #1  reviewer    1m04s   reading crates/kitsu/src/main.rs        (◆ FOX)
        1.2k/340 tok · 6 tools · 0 writes
  ◆ #2  —           0m12s   provider round 2                        (◆ FOX)
  ◈ #3  reasoning   0m41s   ready · 1.4 KB — /jobs show 3           (◈ FOX: a result awaiting custody)
  ◇ #4  reviewer    0m00s   failed · provider_client_error          (◇ EMBER)
  ◇ #5  —           2m10s   stopped                                 (◇ AMBER)
```

`◈` doubles correctly here: it is already "receipts / a durable artifact exists", and
an unacknowledged result *is* exactly that. Cancelled uses AMBER (warning), failed
EMBER (error), consistent with the context-meter thresholds.

`--plain` / `NO_COLOR` / non-TTY (zero escape bytes, ASCII only):

```
# agent deck - 2 running, 1 ready, 1 failed, 6 slots free
  [run]  #1  reviewer   1m04s  reading crates/kitsu/src/main.rs
  [rdy]  #3  reasoning  0m41s  ready, 1.4 KB - /jobs show 3
  [err]  #4  reviewer   0m00s  failed, provider_client_error
  [stp]  #5  -          2m10s  stopped
```

**Acceptance:** with four agents in four states, styled TTY output contains exactly
one `◈` header line and one status glyph per row with FOX/FOX/EMBER/AMBER
respectively; `--plain` output for the same state contains zero `\x1b` bytes and the
four `[run]/[rdy]/[err]/[stp]` markers; the `command_result` JSONL payload is
unchanged from today's `jobs_snapshot_payload`.

### D2 — `⛩` as the thread-boundary glyph: a launch receipt for `/agent spawn`

`⛩` is already "the session passing a gate". A new agent thread is the same kind of
crossing, one level down. Pairs with F2's new command:

```
⛩ #3 dispatched — reviewer · "review crates/kitsu/src/subagent.rs"     (⛩ + id FOX, rest dim)
```

and on completion, from the async notifier:

```
⛩ #3 returned — done · 1.4 KB · 2m41s — /jobs show 3
```

This replaces today's `~ 1 background agent(s) finished - /agents for results`, which
uses a plural count and points at the deprecated alias.

`--plain`: `* agent 3 dispatched - reviewer - "review crates/…"` /
`* agent 3 returned - done, 1.4 KB, 2m41s - /jobs show 3` (the documented `⛩`→`*`
mapping).

**Acceptance:** `/agent spawn reviewer <task>` prints exactly one `⛩` line on a
styled TTY and one `*` line in `--plain`, both naming id, profile, and a bounded task
preview; completion prints one `⛩ … returned` line per agent (not an aggregate
count); torii lines never appear inside tool receipts (existing rule).

### D3 — A live deck in the status row (the "structured attention system")

Fold elapsed time and deck state into the existing fox spinner, which currently
carries neither:

```
  ( ･ᴥ･ )～  kitsu is thinking 1m12s · ◆2 · ◈1        (kaomoji FOX, text dim, ◆/◈ FOX)
```

`◆2` = two agents running, `◈1` = one result waiting for you. Both suppressed when
zero, so a session with no agents keeps today's exact line plus the elapsed clock.

`--reduced-motion`: identical content as one static styled line, repainted only on a
state change. `--plain`: `* kitsu is thinking 72s | 2 agents running | 1 result ready`.
Non-TTY heartbeat: `~ still waiting on the model (72s); 2 background agent(s) running...`.

The same row covers the exit/join wait (`/quit wait`, Ctrl+D with live agents), which
is silent today:

```
  ( ･ᴥ･ )～  waiting for 1 background agent 2m41s · ◆1 — Ctrl+C cancels
```

**Acceptance:** with two running agents and one unacknowledged result, the styled
status line contains the elapsed value and both counts; with zero agents it is
byte-identical to today's frame apart from the elapsed field; `--plain` and non-TTY
variants contain zero escape bytes and the same three facts; a `/quit wait` that
blocks for more than 10 s produces at least one progress line naming the agent it is
waiting on.

### D4 — Live deck commands print immediately, above the status row

Route `TypeaheadOutcome` through the existing `PromptSafeAsyncNotice` +
`create_external_printer()` path already used for background completion notices,
instead of buffering to turn end (F3). `/jobs` errors get their own label:
`jobs refused: …` (EMBER) vs `queue refused: …` (EMBER) vs `steering refused: …`.
The status row repaints below the printed block.

**Acceptance:** typing `/jobs list` during a running turn prints the listing within
1 s, the spinner resumes beneath it, and the post-turn block does not repeat it; on a
terminal where `create_external_printer()` fails, output still lands (deferred) and
is printed exactly once.

### D5 — Steering that tells the truth

```
⛩ #3 steering accepted — restarting the thread with your correction
   discarding 1m12s of work · completed file writes kept · correction: "focus on the parser only"
```

and in `/jobs show 3`:

```
  steering: applied 2 · pending 0 · last "focus on the parser only" (+1m12s)
```

`--plain`: `* agent 3 steering accepted - thread restarts; 1m12s discarded; writes kept`.
Foreground `/steer` gains the same correction echo.

**Acceptance:** steering a running job prints the restart wording and the discarded
elapsed time; `/jobs show` distinguishes applied from pending; the JSONL steer receipt
gains `"restarts_thread": true` and `"discarded_ms"`; sending two corrections in
flight reports that only the latest is carried (or accumulates them — either way, it
says which).

### D6 — Render the deck from the DTO, never from `{:?}`

`/profiles` becomes:

```
◈ background-agent profiles
  ◆ reviewer    effort high · tools fs_read_file, fs_list_dir · inherits session grants
       Review the delegated evidence critically; return prioritized findings.
  ◇ reasoning   effort medium · no tools · isolated context
       Reason from the supplied task only; return a self-contained conclusion.
  ~ start one with /agent spawn <profile> <task>
```

(`◆` = has tools / can act; `◇` = reasoning-only. The hint gets the TEAL `~` notice
prefix instead of sitting at profile indent.) When a profile's allowlist is empty
because the run granted no read roots, the row says
`no tools — this run granted no read roots` rather than `AllowList([])`.
`/jobs show` switches phase, handoff, and timeline kinds to the JSONL snake_case
spellings (`running`, `unavailable`, `provider`).

`--plain`: `# background-agent profiles` + `- reviewer | effort high | tools fs_read_file, fs_list_dir | inherits session grants`.

**Acceptance:** golden-output tests for styled / `--plain` / JSONL assert that no
human deck output contains `AllowList(`, `Some(`, `[Running]`, or `Unavailable`, and
that every human label string equals the corresponding DTO field value.

### D7 — Rounded, language-tagged, syntax-highlighted code boxes (the syntect backlog) — applied to the streaming path

Today (measured, F17) a fenced block on a styled TTY has **zero** escape bytes.
Proposed styled-TTY rendering:

```
  ╭─ rust ──────────────────────────────────────────────╮      (border dim, "rust" FOX)
  │ fn add(a: i32, b: i32) -> i32 {                      │      (syntect-highlighted body)
  │     a + b                                            │
  │ }                                                    │
  ╰──────────────────────────────────────────────────────╯
```

Details that make it shippable:

- Same rounded box vocabulary as the banner (`╭ ╮ ╰ ╯ ─ │`), so kitsu has one box
  idiom, not two. Border dim; the language tag in FOX; untagged fences render
  `─ code ─`.
- Highlight theme constrained to ANSI/256 output mapped onto the named palette plus
  terminal defaults — no rainbow. `diff`/`patch` keeps today's green/red/cyan
  (unchanged, still correct).
- **Streaming**: buffer only *inside* a fence, flushing one complete line at a time,
  so prose latency is untouched and the box closes as the fence closes. An
  unterminated fence at end-of-stream is flushed with a closing border. This also
  fixes the streaming/`--no-stream` divergence in F17 as a side effect — the two
  paths finally render the same thing.
- Width: box width = min(terminal width, longest line + 2); lines longer than the
  terminal are emitted without a right border rather than truncated (the banner's
  established overflow rule).
- `/save` and session export continue to consume the raw text; the box is
  presentation only.

`--plain` / `NO_COLOR` / `KITSU_PLAIN` / `TERM=dumb` / non-TTY: **byte-identical to
today** — literal ``` fences, no box, no colour.

**Value judgement (asked for):** high, and higher than its "polish" label suggests.
Kitsu's answers are mostly code; a styled pty currently renders them exactly as `cat`
would, so the fox banner is the *only* moment of identity in a coding session. This
is also the one remaining place where kitsu is measurably behind every peer CLI, and
it repairs a real inconsistency (diff colouring today only works with `--no-stream`,
which nobody passes). Recommend it as the highest-value item in this proposal set
after the correctness fixes.

**Acceptance:** a streamed styled-TTY answer containing a ```rust block renders a
closed rounded box with `rust` in the header and at least one non-default colour
inside; the same answer with `--plain`, with `NO_COLOR=1`, and piped to a file is
byte-identical to the pre-change bytes; an answer whose fence is never closed still
emits every character and a closing border; a ```diff block keeps today's colours;
`/save` writes the unboxed source; a message containing a ```` ``` ```` inside a
`///` doc comment does not mis-scope the remainder (today's toggle bug, F17).

### D8 — Fox-spirit thread names alongside ids

Each background thread gets a stable name from a fixed 32-word kitsune/kodama list
(`kori`, `nogi`, `hoshi`, `yuki`, `shion`, `haru`, `sora`, `tsuki`, …), displayed as
`#3 hoshi`. Ids stay canonical; names are an *additional* accepted selector
(`/jobs steer hoshi …` ≡ `/jobs steer 3 …`). Three concurrent agents become
referable in speech, in the model's prose, and in scrollback — which numbers alone
never manage. ASCII-only, so `--plain` output is identical.

**Acceptance:** names are unique within a session and stable for a thread's lifetime;
both `#3` and `hoshi` resolve in every `/jobs`/`/agent` verb; JSONL keeps `id` as the
key and adds a separate `"name"` field; `--plain` output is byte-identical to styled
apart from glyphs/colour.

---

## 6. Fixtures and artifacts

Under `/tmp/cycle7-audit3/`:

| File | What it shows |
|---|---|
| `s1.txt` / (stdout) | scripted read-only sweep; `/queue` and `/steer` refusals (§0) |
| `s2.txt` | `/tools` + `/profiles` with a read grant — the `AllowList([...])` leak (F5) |
| `s3.txt` / `s3.out` | **F1**: `profile:"reviewer"` background agent fails HTTP 400 in 0.1 s |
| `s4.txt` / `s4.out` | control: profile-less background agent runs fine; **F2b** cancel-at-exit |
| `s5.txt` | `--plain` vs `--jsonl` deck output — the DTO/`Debug` gap (F5) |
| `s6.txt` / `s6.out` | `/quit wait` as the undocumented scripted join (F2b) |
| `pty3.raw` / `pty3.in` / `feed.sh` | **F3/F6/F7/F16**: live-turn deck commands on a real pty |
| `cb_stream.raw` | **F17**: streamed code block on a pty — zero style bytes |
| `cb_nostream.raw` | **F17** control: `--no-stream` on the same pty *does* dim the fences |
| `fix/parser.rs` | the review fixture |

Repro note for PTY work: `script -qec "cmd" out.raw < file` makes kitsu's stdin a
*file* (piped single-turn mode). To exercise the interactive loop, pipe into
`script`'s own stdin and pace the lines — `feed.sh` does this; without a delay
`script` reaches EOF before rustyline reads the second line and kitsu exits on Ctrl+D.
