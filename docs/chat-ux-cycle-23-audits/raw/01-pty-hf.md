# Cycle 23 audit — PTY `/resume` picker, `/doctor`, HF↔ollama parity sweep

Auditor: cycle-23 (Opus slot). Date: 2026-08-13.
Baseline: `e785e9f57f55` on `dogfood/codex-ux-cycle-6` (worktree already at main-repo HEAD; sync was a no-op).
Binary: `/home/mmpg/workspace/kitsu/kitsu2/kitsu/target/debug/kitsu` — reports `0.1.0 (e785e9f57f55-dirty, source commit 2026-08-12)`.
Standing directive honored: every functionality check below ran through the **HF gateway** (`router.huggingface.co`, `meta-llama/Llama-3.3-70B-Instruct`), with ollama used only as the parity control. Token redacted as `hf_***` throughout.

| Test | Verdict |
|---|---|
| 1 — PTY `/resume` picker | **PASS** (picker works end-to-end under a real PTY; one honesty nit) |
| 2 — `/doctor` slash | **PASS WITH A SERIOUS DEFECT** (ships and matches the CLI, but `online` clears an invalid credential) |
| 3 — HF provider-parity sweep | **FAIL** (same script: clean on ollama, hard turn failure on HF; plus two meter contradictions) |
| 4 — error-copy review vs `claude -p` | **LOSS** (kitsu 11× faster, substantively wrong; claude specific and correct) |

---

## TEST 1 — PTY walkthrough of the `/resume` picker

PTY was **not** flaky in this environment. `pty.fork()` + `select` loop worked on the first attempt; script at
`/tmp/claude-1000/-home-mmpg-workspace-kitsu-kitsu2-kitsu/4bcdba01-4607-42f8-9806-877b35a31a49/scratchpad/pty_resume.py`,
captures at `pty_resume.out` / `pty_resume2.out`. (Note for the record: the known PTY-environmental CLI-test failure did
not reproduce as an obstacle to driving the binary interactively.)

Setup — two durable named sessions created via piped HF chat first:

```
~ session name: cycle23-pick
~ session s-1786595346159811255-3883444-0 autosave planned; a new id becomes resumable after the first successful turn or state-changing command (snapshots never store grants; invocation grants: 0 read, 0 write)
ok
kitsu-usage: {"prompt_tokens":839,"completion_tokens":2,"total_tokens":841}
kitsu-context: {"used":840,"effective":130560,"window":131072}
```

### Verbatim picker output (PTY, `TERM=xterm-256color`, `KITSU_PLAIN=1`)

```
* kitsu chat - meta-llama/Llama-3.3-70B-Instruct via huggingface | /help for commands | Ctrl+D exits
kitsu - gateway: huggingface | model: meta-llama/Llama-3.3-70B-Instruct | directory: /tmp/.../scratchpad | tools: 8
kitsu> /resume
saved sessions:
  [1] cycle23-second (s-1786595352841364118-3883493-0)  2026-08-13T04:29:15.122Z  1 turn(s)
      Reply with exactly: two
  [2] cycle23-pick (s-1786595346159811255-3883444-0)  2026-08-13T04:29:06.837Z  1 turn(s)
      Reply with exactly: ok
  [3] s-1786459292594577016-2717952-0  2026-08-11T14:50:40.482Z  1 turn(s)
      Now run the test suite with the command ["python…
  [4] s-1786458722698667316-2702806-0  2026-08-11T14:39:24.038Z  1 turn(s)
      Read ISSUE.md in this project and tell me your p…
  [5] s-1786447822937380529-2539788-0  2026-08-11T11:57:02.894Z  2 turn(s)
      the test suite fails, find the root cause and ex…
  [6] s-1786425829215446720-2228745-0  2026-08-11T05:23:49.215Z  3 turn(s)
      My project codename is ZEPHYR-42. Remember it. R…
  [7] s-1786425779803918809-2228312-0  2026-08-11T05:23:43.250Z  3 turn(s)
      My project codename is ZEPHYR-42. Remember it. R…
  [8] s-1786425698575751131-2226626-0  2026-08-11T05:22:11.563Z  1 turn(s)
      Call fs_edit_file on /tmp/cycle6-audit2/bugfix/s…
  [9] s-1786425400547712948-2223102-0  2026-08-11T05:19:46.793Z  2 turn(s)
      My favorite number is 42. Acknowledge in five wo…
  … 1 more — /sessions lists all
resume which? [1-9, name, id, last, auto; Enter = last, q = cancel] 1
~ rebuilt saved system context from current tools, project metadata, and bounded skill provenance
* resumed s-1786595352841364118-3883493-0 (saved grants were not restored; runtime grants reset; invocation grants retained: 1 read, 0 write)
```

Everything the cycle-22 note promised is there: **name-first** rows (named sessions lead with the name, id in parens;
unnamed fall back to the id), numbered `[1]`–`[9]`, a preview line, the 9-item cap with a `/sessions` pointer, and a
prompt that spells out every accepted input. The `1` resolved to the correct session and the resume banner is explicit
about grants not being restored. This is a genuine improvement over `claude --resume`'s chooser in one respect: kitsu
states the grant-reset policy inline, which claude's picker does not.

`/status` after the resume, sessions enabled — correctly reflects the resumed session:

```
  id           s-1786595352841364118-3883493-0
  autosave     saved
  gateway      huggingface
  model        meta-llama/Llama-3.3-70B-Instruct
  endpoint     https://router.huggingface.co/v1/chat/completions
  auth         bearer-env HF_TOKEN (present)
  context      1398 / 130472 used (1%, 129074 left; window 131072)
  conversation 1 user | 2 assistant | 1 tool
```

**Nit (F6 below).** The same walkthrough under `--no-session` prints `* resumed s-1786595352841364118-3883493-0` and
then `/status` reports `id disabled` / `autosave disabled`. The history *is* loaded, so neither line is a lie in
isolation, but read together they contradict. `/status` should say something like `id  s-… (resumed, autosave off)`.

Non-TTY behavior is **unchanged**, as designed:

```
command> /resume
s_resume.txt:1: session commands require interactive chat
```

**Verdict: PASS.** The oldest open parity item is real, works under a PTY on the HF gateway, and its scripted contract holds.

---

## TEST 2 — `/doctor` slash command

### Offline, verbatim (HF gateway, `--script`)

```
command> /doctor
kitsu 0.1.0 (e785e9f57f55-dirty, source commit 2026-08-12)
[ok] config (--config) ~/workspace/kitsu/kitsu2/kitsu/kitsu.toml — parsed successfully
[info] gateway/model: huggingface/meta-llama/Llama-3.3-70B-Instruct
[info] auth: bearer variable HF_TOKEN is present
[info] context window: 131072 tokens (gateway profile)
[info] project context: no llms.txt, AGENTS.md, or CLAUDE.md found
[ok] runtime state: ~/.kitsu/profiles/v1-fnv128-88f520cbe5f54b12c8d6a7e08efd5eff — writable
[info] provider check: not requested (offline default)
[info] tools: filesystem=interactive chat prompts; automation is deny-by-default unless roots are granted commands=disabled unless explicitly enabled; enabled commands require exact approval plus mandatory fail-closed OS containment
doctor: no blocking issues found — you're ready: run `kitsu chat` (or `kitsu ask "…"`)
```

### Online, verbatim

```
command> /doctor online
...
[info] provider check: reachable
```

`/doctor online` output is **byte-identical** to standalone `kitsu doctor --online --gateway huggingface`. Good: the
slash reuses `run_doctor` rather than reimplementing it, so the two can't drift.

### Judged against `claude doctor`

`claude doctor` reports installation health only — version, commit, platform, install path, auto-update channel, "No
installation issues found." kitsu's report is **substantially more useful**: it covers config provenance (`--config` vs
env vs `~/.kitsu`), the resolved gateway/model, credential presence, context-window value *and* its source, project
context, runtime-state writability, and the tool/permission posture. On breadth kitsu clearly wins.

### …but the online probe is dishonest — **F1, the headline finding**

Re-run with a **deliberately invalid** token (`HF_TOKEN=hf_BOGUSTOKENVALUE000`):

```
command> /doctor online
[info] auth: bearer variable HF_TOKEN is present
[info] provider check: reachable
doctor: no blocking issues found — you're ready: run `kitsu chat` (or `kitsu ask "…"`)
```

The very next real turn with that same token:

```
~ inference provider attempt 1/3: authentication; HTTP 401; request rejected; terminal=true; 119833ms deadline remaining
! error: chat request authentication/access was rejected (HTTP 401) - refresh the configured credential and confirm model access; configured refresh hint: create a fine-grained Inference Providers key under huggingface.co settings, then export HF_TOKEN
```

So the one question a user runs `/doctor online` to answer — *will my next request work?* — is answered "yes, you're
ready" when the answer is no. The `--json` form **knows better** and hides it from the text renderer:

```json
"online": {
    "chat_route_probed": false,
    "effective_model": "meta-llama/Llama-3.3-70B-Instruct",
    "effective_model_available": true,
    "models": 131,
    "requested": true,
    "scope": "model_catalog_only",
    "status": "reachable"
}
```

Two separate defects fall out:

- **F1** — the human text prints only `status: reachable` and drops `scope: model_catalog_only` /
  `chat_route_probed: false`, then concludes `no blocking issues found — you're ready`. HF's `/v1/models` is
  effectively unauthenticated, so "reachable" here means nothing about the credential. The text must surface the scope
  (`reachable (model catalog only — chat route and credential not probed)`) and must not emit an unqualified
  "you're ready" for an unprobed credential.
- **F2** — the JSON asserts `"auth": { "valid": true }` for a token that 401s seconds later. Nothing probed validity;
  the field is fabricated. It should be `null`/absent unless the chat route was actually exercised.

Also: the text drops the endpoint (`https://router.huggingface.co/v1/chat/completions`) and `models: 131`, both present
in the JSON. A doctor should print **what it probed**, not just a verdict.

### F4 — the `tools:` line is an unparseable run-on

`crates/kitsu/src/main.rs:1591` formats `"[info] tools: filesystem={} commands={}"`, and both values are multi-clause
prose containing their own semicolons. The result reads as one sentence with a missing separator:

```
... deny-by-default unless roots are granted commands=disabled unless explicitly enabled; ...
```

Needs two lines, or at minimum ` | ` between the fields.

**Verdict: PASS WITH A SERIOUS DEFECT.** The command ships, is well-scoped, reuses the CLI path, and beats
`claude doctor` on breadth — but `online` currently green-lights a credential that cannot authenticate.

---

## TEST 3 — HF ↔ ollama provider-parity sweep

One script (`s_parity.txt`), run identically on both gateways with `--max-output-tokens 600 --no-session`:

```
Reply with exactly this word and nothing else: parity
/cost
/theme ocean
/context
/help resume
```

### The diff

| Surface | HF (Llama-3.3-70B) | ollama (qwen2.5-coder) | Same? |
|---|---|---|---|
| The model turn | 8 `Agent(...)` spawns, spawn cap hit, 6 failed tool calls, **turn discarded + rolled back**, script exit ≠ 0 | answered `parity` inline, clean | **NO** |
| `/cost` usage line | `0 prompt + 0 completion = 0 total token(s) across 0 turn(s)` after ~9.8k tokens really spent | `664 prompt + 2 completion = 666 total … across 1 turn(s)` | **NO** |
| `/cost` window line | `110 of 130472 effective … (0%), window 131072` — sane for 131k | `666 of 7592 … (8%), window 8192` | yes (format) |
| `/theme ocean` | `⛩ theme: ocean — deep-sea blues and surf teal` | identical | yes |
| `/context` table | identical layout, provenance, cache/probe lines | identical | yes |
| `/help resume` | identical text, and it documents the new picker | identical | yes |
| Tool schemas registered | 4 (spawn_agent family), 493 tok | 4, 493 tok | yes |
| Exit | `! error: script failed: 1 turn(s) or command(s) failed` | `~ run summary: 5 turn(s) ok` | **NO** |

The slash-command surfaces are genuinely provider-neutral — `/theme`, `/context`, `/help` are byte-identical, and the
131k window renders correctly everywhere. The divergence is entirely in the **agent loop** and the **accounting**.

### F3 — the delegation heuristic is catastrophically worse on HF (verbatim)

```
user> Reply with exactly this word and nothing else: parity
◆ Agent("Reply with exactly this word and nothing else: parity")
parity
kitsu-usage: {"prompt_tokens":105,"completion_tokens":2,"total_tokens":107}
  ◇ 1 line
◆ Agent("print 'parity'")
parity
  ◇ 1 line
◆ Agent("print "parity"")
  ◇ error: ExecError — execution error: agent spawn cap reached: this turn already started 2 of 2 allowed subagents. Do the remaining work yourself, inline, without spawning agents; the user can raise the cap with `agent_spawn_cap` in the config or --agent-spawn-cap.
◆ Agent("print('parity')")
  ◇ error: ExecError — execution error: agent spawn cap reached ...
◆ Agent("print("parity")")
  ◇ error: ExecError — execution error: agent spawn cap reached ...
◆ Agent("echo parity")
  ◇ error: ExecError — execution error: agent spawn cap reached ...
◆ Agent("echo "parity"")
  ◇ error: ExecError — execution error: agent spawn cap reached ...
◆ Agent("echo parity")
  ◇ error: ExecError — execution error: agent spawn cap reached ...
parity
kitsu-usage: {"prompt_tokens":9630,"completion_tokens":191,"total_tokens":9821}
~ provisional assistant output was DISCARDED and was not committed: 6 tool call(s) failed during the turn (0 local policy denial(s))
s_parity.txt:1: strict scripted turn failed: 6 tool call(s) failed during the turn (0 local policy denial(s))
kitsu-context-rollback: incomplete turn removed from conversation state
```

This is cycle-22's queued gap #3, but it is far more than a redundancy nit on a hosted provider:

1. The correct answer `parity` was produced **three separate times** and thrown away.
2. The spawn-cap error text tells the *model* to work inline, and the model ignored it six times in a row — the cap
   message is not a control, only a suggestion. Nothing escalates to a hard stop or strips the spawn tools from the
   next round.
3. **Strict-turn policy converts a cosmetic model quirk into a red exit code.** A user scripting kitsu against HF gets
   a failed run for asking a trivial question.
4. Cost: ~9.8k prompt tokens for one word.

Even with an explicit instruction not to delegate, the same model still delegates once:

```
user> What is 2+2? Answer with just the number, inline, do not delegate.
◆ Agent("What is 2+2? Answer with just the number, inline, do not delegate.")
4
kitsu-usage: {"prompt_tokens":114,"completion_tokens":2,"total_tokens":116}
  ◇ 1 line
4
kitsu-usage: {"prompt_tokens":1757,"completion_tokens":43,"total_tokens":1800}
```

Note also the answer `4` is printed **twice** (once as sub-agent output, once as the final answer) — a display gap.

### F5 — `/cost` hides money that was actually spent

Two independent leaks, both visible above:

- **Rollback zeroes the meter.** After the HF turn above burned ≥9,821 tokens on the wire, `/cost` reported
  `session usage: 0 prompt + 0 completion = 0 total token(s) across 0 turn(s)`. Rolling the *conversation* back is
  correct; rolling the *billing* back is not. The provider charged for it.
- **Sub-agent spend is excluded.** In the 2+2 run the sub-agent spent 116 tokens and the parent 1,800; `/cost` reports
  1,800. Real spend was 1,916. On HF, where delegation fires constantly, this under-reports systematically.

`/cost`'s own disclaimer ("kitsu meters tokens, not currency") makes the token count the *entire* value of the command,
so both leaks hit the one number it exists to provide.

### F7 — `/cost` and `/context` publish contradicting "used" numbers

Provider-**independent** (reproduced on both gateways, same instant, no turn in between):

HF:
```
command> /cost
context window: 110 of 130472 effective token(s) used (0%), window 131072 — /context for provenance
command> /context
used 603 / 130472 effective  [■□□□□□□□□□□□□□□□]  0%   headroom: 129869
  tool schemas (4)         493  admitted    spawn_agent, spawn_agents, spawn_agent_bg, agent_result
```

ollama:
```
command> /cost
context window: 110 of 7592 effective token(s) used (1%), window 8192 — /context for provenance
command> /context
used 603 / 7592 effective  [■□□□□□□□□□□□□□□□]  7%   headroom: 6989
```

The delta is exactly the 493-token tool-schema row. `/cost` uses `meter.estimate(messages)`, and
`crates/kitsu/src/main.rs:10209` notes *"Tool schemas ride the request's `tools` field, not the system message"* — so
the schemas are on the wire and consume the window, but `/cost` omits them. `/context` is the honest one. On the 8k
ollama window this is the difference between reporting **1%** and **7%** — a 7× understatement of pressure on exactly
the small-window configuration where the number matters.

A second, related inconsistency: after a turn, `/context`'s **own rows do not sum to its own total** —
`110 + 493 + 17 + 41 + 8 = 669` under a stated `used 905` on HF (`110 + 493 + 14 + 2 = 619` under `666` on ollama).
Whatever per-message framing overhead accounts for the remainder is invisible, so the provenance table — whose entire
purpose is provenance — cannot be reconciled.

**Verdict: FAIL.** The presentation layer is provider-neutral and that is a real achievement; the agent loop and the
accounting are not.

---

## TEST 4 — random comparison task: error-message copy review

Prompt (identical): *"Read the error strings in crates/kitsu/src/config.rs (the ConfigError Display impl) and propose
improved wording for the two least helpful ones; change nothing."*

| | kitsu + HF | `claude -p` |
|---|---|---|
| Wall clock | **4.48 s** | 51.47 s |
| Tool rounds | 1 read + 1 skill | (not shown) |
| Prompt tokens | 16,740 | — |
| Targets chosen | one real, one **nonexistent** | both real, correctly ranked |
| Citations | none | `config.rs:229`, `config.rs:222`, `config.rs:48`, `config.rs:351`, `config.rs:35` |
| Correctness | **wrong** | correct |

kitsu was **11.5× faster** and that is a genuine, repeatable advantage. But the content does not hold up:

- Its second target — `"config error: unknown field \`x\`, expected one of …"` — **is not a `ConfigError` Display
  string at all**. It appears only inside a doc comment at `config.rs:157` describing serde's hint behavior. The task
  said "the ConfigError Display impl"; kitsu reviewed a comment.
- Its proposed fix for target #1 was *"config error: TOML parse failed at line X, column Y"* — which is **already a
  distinct existing variant** (`config.rs:190`). It proposed regressing an existing message onto a different one.
- The remaining advice is filler: *"Please check the documentation for the correct field names."*
- No file:line anywhere, so nothing is verifiable without re-reading the file.

`claude -p` found the two that a maintainer would agree on, and found them for the right reasons:

- `ModelRequired` (`config.rs:229`) — *"provider.model is required — set it with --model <name>"* names a key that
  **does not exist in this schema** (it is `model` under `[gateways.<name>]`), and because `Gateway` is
  `deny_unknown_fields`, a user who follows the message gets a *second* error. It is also the only one of the nine
  variants missing the `config error:` prefix, breaking grep-ability.
- `AuthEnvRequired` (`config.rs:222`) — "env name" is ambiguous between *the variable's name* and *the variable being
  set*, and it names neither the config key nor the table it lives in.

Both critiques check out against the source, and claude flagged `StaticTokenRequired` as an explicit runner-up with a
reason for the ranking. Concrete replacement strings were supplied for both.

**Grading for specificity: claude clear win; kitsu clear loss.** Model capability (Llama-3.3-70B vs Opus) is the
dominant factor and that is not kitsu's fault. But one part **is** harness-attributable and cheap to fix: kitsu's system
prompt never asks for `file:line` citations, so its reviews are unverifiable by construction. Every claude answer in
this loop cites; no kitsu answer in this loop has.

**Verdict: LOSS on quality, WIN on latency.**

---

## RANKED TOP-5 GAPS

1. **`/doctor online` green-lights an invalid credential.** With a bogus `HF_TOKEN` it prints
   `provider check: reachable` + `no blocking issues found — you're ready`, and the next turn 401s. The JSON already
   carries `scope: model_catalog_only` / `chat_route_probed: false`; the text drops both. Surface the scope, drop the
   unqualified "you're ready", and stop emitting `auth.valid: true` for an unprobed credential. *(F1 + F2)*

2. **Redundant delegation is a hard failure on HF, not a nit.** Llama-3.3-70B spawned agents 8× for a one-word prompt,
   hit the cap, and strict-turn policy discarded a correct answer and failed the script with a non-zero exit. The
   spawn-cap error only *asks* the model to stop. Make the cap structural — strip the spawn tools after the cap, and
   don't fail a turn whose only failures are cap-refusals when a valid final answer was produced. *(F3)*

3. **`/cost` under-reports real spend two ways:** a rolled-back turn resets usage to `0 … across 0 turn(s)` after
   ~9.8k tokens were billed, and sub-agent tokens are never added to the parent total. Bill on the wire, not on the
   committed conversation. *(F5)*

4. **`/cost` and `/context` disagree about `used` by the full tool-schema size** (110 vs 603 — 1% vs 7% on an 8k
   window), and `/context`'s own rows don't sum to its own total (669 vs 905). Provider-independent. Make `/cost` reuse
   `/context`'s accounting and add the missing framing-overhead row. *(F7)*

5. **`/doctor` text drops what it probed and mangles its tool line.** No endpoint URL, no model-catalog count (both in
   the JSON), and `main.rs:1591` joins two multi-clause prose fields with a bare space, producing
   `… roots are granted commands=disabled unless …`. Also queue the cheap Test-4 fix: instruct the model to cite
   `file:line`, so kitsu's reviews become checkable. *(F4 + Test 4)*

Runner-up (F6): after `/resume` under `--no-session`, the banner says `resumed s-…` while `/status` says
`id disabled` — reconcile the two.

---

## Housekeeping

- Sessions created by this audit (`cycle23-pick`, `cycle23-second`) were deleted; `kitsu sessions list` returned to its
  pre-audit 8 entries.
- Worktree clean; nothing in the repo was modified. All artifacts live in the scratchpad.
