# Cycle 27 audit — per-model cost attribution, inline markdown styling, docs cross-reference

Date: 2026-08-13
Baseline verified: worktree `00bb7f9` == main repo `refs/heads/dogfood/codex-ux-cycle-6` `00bb7f9c6dbbe19357752f69e513a5ad12274997`
(`render: inline markdown styling — bold, tinted code, dimmed bullets (ui-parity item 4)`).
Binary: `/home/mmpg/workspace/kitsu/kitsu2/kitsu/target/debug/kitsu` (Aug 13 01:26).
HF token supplied inline-env only; redacted as `***` throughout.

---

## TEST 1 — per-model cost attribution on Hugging Face

### Verdict: **PASS on the feature, with one blocking usability defect in the path that reaches it**

#### 1a. As specified (`/model openai/gpt-oss-120b:fastest`) — FAILED before the feature could run

Script:

```
Reply with exactly: one
/model openai/gpt-oss-120b:fastest
Reply with exactly: two
/cost
```

Command:

```
HF_TOKEN=*** target/debug/kitsu chat --script t1.script \
  --config kitsu.toml --gateway huggingface --max-output-tokens 400 --timeout-secs 300
```

Verbatim (stderr):

```
command> /model openai/gpt-oss-120b:fastest
model 'openai/gpt-oss-120b:fastest' is not in the gateway catalog - run /models
.../t1.script:2: command failed: /model openai/gpt-oss-120b:fastest
user> Reply with exactly: two
kitsu-usage: {"prompt_tokens":9382,"completion_tokens":2,"total_tokens":9384}
command> /cost
session usage: 65452 prompt + 163 completion = 65615 total token(s) across 2 turn(s), subagent spend included (meta-llama/Llama-3.3-70B-Instruct via huggingface)
context window: 9383 of 130560 effective token(s) used (7%), window 131072 — /context for provenance
```

The switch was refused, so the run stayed single-model and `/cost` correctly showed
no bins. `kitsu models --gateway huggingface` confirms the catalog carries the bare
id only:

```
meta-llama/Llama-3.3-70B-Instruct
openai/gpt-oss-120b
openai/gpt-oss-20b
openai/gpt-oss-safeguard-20b
```

**This is a real defect, not a test artifact.** `kitsu.toml` ships the
provider-pin syntax as documented usage on line 59:

```
# Model = "<org>/<model>", optionally pin a provider: "<org>/<model>:together".
```

The HF router accepts `<model>:<provider>` and `<model>:fastest` as routing
policy suffixes; kitsu's `/model` catalog membership check does exact-string
matching against `/v1/models`, which never lists suffixed variants. So kitsu
refuses an id its own config file tells users to write. `--model` at launch is
presumably unaffected (it does not go through the catalog gate), which makes the
behaviour inconsistent between the flag and the slash command.

#### 1b. With the bare id (`/model openai/gpt-oss-120b`) — feature PASSES cleanly

Verbatim (stderr, elided tool noise):

```
user> Reply with exactly: one
... 9x workspace_diff ...
kitsu-usage: {"prompt_tokens":56070,"completion_tokens":230,"total_tokens":56300}
kitsu-context: {"used":8200,"effective":130560,"window":131072}
command> /model openai/gpt-oss-120b
~ switched to openai/gpt-oss-120b (context window pinned by gateway profile context_window; unchanged)
user> Reply with exactly: two
kitsu-usage: {"prompt_tokens":8371,"completion_tokens":27,"total_tokens":8398}
kitsu-context: {"used":8372,"effective":130560,"window":131072}
command> /cost
session usage: 64441 prompt + 257 completion = 64698 total token(s) across 2 turn(s), subagent spend included (openai/gpt-oss-120b via huggingface)
  meta-llama/Llama-3.3-70B-Instruct: 56070 prompt + 230 completion = 56300 total
  openai/gpt-oss-120b: 8371 prompt + 27 completion = 8398 total
context window: 8372 of 130560 effective token(s) used (6%), window 131072 — /context for provenance
~ kitsu meters tokens, not currency — multiply by your gateway's rates; provider-side prompt-cache discounts are not itemized on the OpenAI-compatible wire
~ run summary: 4 turn(s) ok, 0 file(s) read, 0 file(s) written (0 change(s)), session not saved
```

Arithmetic check — bins reconcile to the total exactly, on all three columns:

| | prompt | completion | total |
|---|---|---|---|
| Llama-3.3-70B-Instruct | 56070 | 230 | 56300 |
| gpt-oss-120b | 8371 | 27 | 8398 |
| **sum** | **64441** | **257** | **64698** |
| header total | 64441 | 257 | 64698 |

Bins also match the per-turn `kitsu-usage` records verbatim. No drift.

**Window re-discovery on model switch renders well.** The switch line is a single
honest sentence that names *why* the window did not move — `context window pinned
by gateway profile context_window; unchanged` — instead of silently re-probing or
silently keeping a stale number. The subsequent `/cost` context line reflects the
new turn's usage (8372) rather than the peak, which is correct for a "current
occupancy" reading.

**Two rendering nits on an otherwise good line:**

- The header's trailing parenthetical `(openai/gpt-oss-120b via huggingface)` sits
  on the *combined-total* line but names only the currently-active model. With
  bins printed directly underneath, it reads as if 64698 tokens were spent on
  gpt-oss-120b. It should say something like `(2 models via huggingface)` when
  bins are present.
- The bins are unlabelled — a bare two-space indent. A `by model:` lead-in would
  make the block self-describing when pasted into an issue.

#### 1c. Incidental but expensive: tool-storm on a trivial prompt

Both Test-1 runs show the same pathology on turn 1. For the prompt
`Reply with exactly: one`, Llama-3.3-70B called `workspace_diff` **nine times in
a row**, each disclosing the identical 2607 bytes:

```
◆ workspace_diff
~ active Kitsu config/private state omitted from workspace evidence
~ disclosing 2607 bytes of host-verified evidence to meta-llama/Llama-3.3-70B-Instruct; no read root granted
  ◇ 1 line · 40ms
[... x9, byte-identical ...]
```

Cost: **56070 prompt tokens** for a two-word answer, versus **8371** for the
identical second turn once the storm did not recur — a 6.7x blowup. The model is
the proximate cause, but kitsu has the loop-detection seat: nine identical
zero-argument calls returning a byte-identical 1-line result is trivially
detectable. A "this tool returned the same result as the previous call" short-
circuit (or a per-turn repeat cap on argument-identical calls, as exists for
`agent_spawn_cap`) would cap the damage. The `~ disclosing …` receipt is honest
and correct; it is just printed nine times.

---

## TEST 2 — inline markdown styling, PTY vs piped

### Verdict: **PASS** — styling present on a TTY, absent when piped, markers retained in both

Harness: `python3` `pty.fork()` driving interactive chat, raw bytes captured
(`/tmp/.../scratchpad/run_pty_harness3.py`). Run on `--gateway huggingface
--model openai/gpt-oss-120b --no-workspace-evidence --max-output-tokens 400`,
cwd = scratchpad. Prompt: `Answer with: a **bold** word, some `inline_code`, and
a 3-item bullet list.`

(An initial attempt on the gateway default Llama-3.3-70B never produced an
answer: it spawned an `Agent(...)` and then attempted `Write(example.txt)` on a
pure-text prompt. Same spurious-tool-use family as 1c. Re-run on gpt-oss-120b.)

### PTY capture — ANSI present

`grep -c $'\x1b'` → **19** lines containing ESC; 98 ESC bytes total in a
2226-byte capture. The answer region, verbatim `repr()`:

```
\x1b[2K\x1b[1m**Success**\x1b[0m\r\n
\r\n
\x1b[38;2;255;253;240m`npm install`\x1b[39m\r\n
\r\n
\x1b[2m-\x1b[0m\x1b[2m \x1b[0mFirst item\r\n
\x1b[2m-\x1b[0m\x1b[2m \x1b[0mSecond item\r\n
\x1b[2m-\x1b[0m\x1b[2m \x1b[0mThird item\r\n
\x1b[2m  ↳ 0.4s · 1,112 input · 87 output · 1,129 / 130,560 context (0% used)\x1b[0m\r\n
```

Every expectation is met:

- **bold span** — `ESC[1m` … `ESC[0m` wraps `**Success**`, and the `**`
  markers are **retained** in the visible text.
- **inline code** — `ESC[38;2;255;253;240m` (theme cream tint) … `ESC[39m`
  wraps `` `npm install` ``, backticks **retained**.
- **bullet markers** — each `-` is dimmed with `ESC[2m` … `ESC[0m`, the item
  text left at default weight, marker **retained**.

One micro-nit: the marker and its following space are emitted as two adjacent
dim spans (`\x1b[2m-\x1b[0m\x1b[2m \x1b[0m`) where one span covering `"- "`
would do. Cosmetically identical, 8 wasted bytes per bullet.

### Piped capture — no ANSI

```
printf 'Answer with: a **bold** word, some `inline_code`, and a 3-item bullet list.\n' \
  | HF_TOKEN=*** target/debug/kitsu chat --config kitsu.toml --gateway huggingface \
    --model openai/gpt-oss-120b --no-workspace-evidence --max-output-tokens 400 --timeout-secs 300
```

stdout, 69 bytes, `grep -c $'\x1b'` → **0**, `count(b'\x1b')` → **0**:

```
b'**Success** `echo "Hello"`  \n\n- Item one  \n- Item two  \n- Item three\n'
```

stderr, 236 bytes, also 0 ESC bytes:

```
kitsu-usage: {"prompt_tokens":579,"completion_tokens":61,"total_tokens":640}
kitsu-context: {"used":596,"effective":130560,"window":131072}
~ run summary: 1 turn(s) ok, 0 file(s) read, 0 file(s) written (0 change(s)), session not saved
```

Clean TTY-detection split, and the piped bytes are the model's markdown passed
through byte-for-byte — safe for `| jq`, `> file.md`, and diffing.

---

## TEST 3 — comparison: docs cross-reference check (read-only)

Prompt (identical to both):
`Verify that file paths referenced in the first 60 lines of docs/functionality.md
exist in the repo; report any that do not; change nothing.`

Ground truth (established independently): lines 1–60 of `docs/functionality.md`
contain exactly one file-path reference, `../README.md#cli-reference` on line 21.
`/home/mmpg/workspace/kitsu/kitsu2/kitsu/README.md` exists; `## CLI reference` is
at `README.md:477`. Everything else in that range is command/slash-command
documentation, not paths.

### Wall time

| Tool | real | user | sys |
|---|---|---|---|
| `claude -p` | **15.170s** | 1.328s | 0.426s |
| `kitsu` + HF (Llama-3.3-70B) | **12.358s** | 0.063s | 0.020s |
| `kitsu` + HF, `--agent-spawn-cap 0` | **9.933s** | 0.078s | 0.008s |

kitsu was faster in both configurations — 19% faster as configured, 34% faster
once barred from spawning a pointless subagent. Note the `user` column: kitsu
burns ~0.06s of local CPU against claude's 1.33s, so essentially all of kitsu's
wall time is provider latency.

### claude output (verbatim)

```
All referenced paths check out — nothing is missing.

The first 60 lines of `docs/functionality.md` contain exactly one file reference: the link to `../README.md#cli-reference` at line 21. Relative to `docs/`, that resolves to the repo-root `README.md`, which exists, and its `#cli-reference` anchor is valid too (the `## CLI reference` heading is at `README.md:477`). Everything else in those lines is command/flag documentation (`kitsu init`, `/status`, etc.), not file paths. No changes made.
```

Spot-check 2 claims:
1. "`../README.md#cli-reference` at line 21 … resolves to repo-root `README.md`, which exists" — **correct** (line 21 confirmed, file confirmed).
2. "the `## CLI reference` heading is at `README.md:477`" — **correct** to the line
   (`grep -n` returns exactly `477:## CLI reference`).

Accuracy: **2/2**, plus an unrequested-but-useful anchor validation and an
explicit statement of what was excluded and why.

### kitsu output (verbatim)

```
The file paths referenced in the first 60 lines of docs/functionality.md are:

* ../README.md

All of these paths exist in the repository.
The file paths referenced in the first 60 lines of docs/functionality.md are:

* ../README.md

All of these paths exist in the repository.
```

Spot-check 2 claims:
1. "the file paths referenced … are: `../README.md`" — **correct and complete**;
   no path was missed and nothing non-path was misclassified as one.
2. "All of these paths exist in the repository" — **correct**.

Accuracy: **2/2** on the question asked. Less depth than claude (no line number,
no anchor check, no statement of what was excluded), but nothing wrong. Both
tools honored "change nothing" — kitsu's own receipt confirms it:
`0 file(s) written (0 change(s))`.

### Defects surfaced by this run

**(a) The answer printed twice.** Reproduced once, on the subagent path. Cause
not isolated: the parent turn spent 208–210 completion tokens for a ~40-token
answer, consistent with the model itself echoing the subagent's verbatim result,
and the `--agent-spawn-cap 0` re-run produced a single clean copy. Tests 1 and 2
show no duplication anywhere in the renderer. So the honest read is *model
behaviour on the subagent hand-back path*, not a kitsu render bug — but kitsu is
the only party that can see "the final message is a verbatim repeat of a tool
result I already have" and is well placed to collapse it.

**(b) Reproducible tool-schema rejection — the highest-value fix here.** In
**both** kitsu runs, the first `Read` call died identically:

```
◆ Read(docs/functionality.md)
  ◇ error: InvalidArgs — invalid arguments: invalid type: string "1", expected usize at line 1 column 51 · 0ms
```

The model emitted `"1"` (JSON string) where the schema wants `1` (integer). This
is the single most common LLM tool-call deviation in existence, and kitsu's serde
layer hard-rejects it, costing a full round-trip every time. Claude Code coerces
numeric strings silently. A `deserialize_with` that accepts `"1"` for integer
fields would delete this failure class outright. Note it burns a round *and* the
error text, while accurate, gives the model no repair instruction — a hint like
`expected an integer, got the string "1" — send 1 without quotes` would make
recovery deterministic instead of lucky.

**(c) Unhelpful error when `List` is aimed at a file.** Twice in the first run:

```
◆ List(docs/functionality.md)
  ◇ error: ExecError — execution error: list failed: Not a directory (os error 20) · 0ms
```

A raw errno leaked to the model. `docs/functionality.md is a file, not a
directory — use Read` would recover in zero extra rounds.

**(d) Good news worth recording.** The recovery receipt is honest and precise:

```
~ 2 earlier tool error(s) were recovered within the turn; final state committed
~ run summary: 1 turn(s) ok, 1 file(s) read, 0 file(s) written (0 change(s)), 2 failed tool call(s), session not saved
```

Failed calls are surfaced in the summary rather than buried, and the
`agent_spawn_cap` denial message is a model-model of actionable phrasing:
`Do the remaining work yourself, inline, without spawning agents; the user can
raise the cap with agent_spawn_cap in the config or --agent-spawn-cap.`

---

## Verdicts

| Test | Verdict |
|---|---|
| 1 — per-model cost attribution | **PASS** (bins exact, reconcile on all three columns; switch line honest) — **but** the specified `:fastest` provider-pin was rejected by `/model`'s catalog check, a defect against kitsu's own documented syntax |
| 2 — inline markdown styling | **PASS** — 19 ESC-bearing lines on PTY with bold/tinted-code/dimmed-marker spans and markers retained; 0 ESC bytes piped, markdown passed through byte-for-byte |
| 3 — docs cross-reference comparison | **PASS, kitsu faster** — 12.36s vs claude 15.17s (9.93s without a needless subagent); accuracy 2/2 spot-checks each; claude more thorough (anchor + line number), kitsu duplicated its answer once on the subagent path |

## Ranked remaining gaps

1. **Integer-typed tool arguments reject JSON strings** (`invalid type: string
   "1", expected usize`) — reproduced in 2/2 tool-using runs, costs a round every
   time, and the error carries no repair instruction. Coerce numeric strings in
   the tool-arg deserializer and add the fix to the message. Highest
   fix-value-to-effort ratio in this cycle.
2. **No repeat-call loop guard on tools** — nine byte-identical `workspace_diff`
   calls turned a two-word turn into 56070 prompt tokens (6.7x the same turn
   without the storm), and a separate run had a pure-text prompt trigger
   `Agent(...)` then `Write(example.txt)`. `agent_spawn_cap` already proves the
   pattern; extend it to argument-identical repeat calls, or short-circuit a call
   whose result is byte-identical to the immediately preceding one.
3. **`/model` catalog check rejects HF provider-pinned ids** (`<org>/<model>:fastest`,
   `:together`) that `kitsu.toml` line 59 documents as supported, while `--model`
   at launch presumably accepts them — inconsistent, and it blocks the exact
   cross-provider switching that cycle 25/26 queued as gap #1. Strip a trailing
   `:<provider>` suffix before the membership test, or downgrade the refusal to a
   warning.

Runners-up (below the cut, recorded for the queue): `/cost` header parenthetical
names only the active model while sitting above multi-model bins; bins lack a
`by model:` label; `List` on a file leaks a raw errno instead of "use Read";
duplicate final answer on the subagent hand-back path (likely model-side, but
collapsible host-side).

## Housekeeping

- Worktree clean: `git status --porcelain` empty, HEAD `00bb7f9`, unchanged from baseline.
- One session was auto-created by the interactive PTY run
  (`s-1786610444943856111-4120552-0`) and has been deleted; `sessions list` no
  longer shows it. All other runs reported `session not saved`.
- No files written to the repo by any model turn (`0 file(s) written` on every
  run); the attempted `Write(example.txt)` never completed.
- HF token appeared only as an inline environment assignment, never in a file or
  a saved artifact.
