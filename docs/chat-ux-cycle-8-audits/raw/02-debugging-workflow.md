# Cycle 8 — Auditor 2: the debugging workflow ("my tests fail, help me find out why")

Date: 2026-08-11
Baseline: kitsu `c66af4b` (`target/debug/kitsu`), codex-cli 0.147.0.
Model for all kitsu runs: local Ollama `qwen2.5-coder:latest` (7.6B, Q4_K_M, CPU).
Codex model: `gpt-5.6-sol` (hosted, `approval: never`).

Fixture: `/tmp/cycle8-audit2/fixture-kitsu/` and `/tmp/cycle8-audit2/fixture-codex/`
(identical copies, each its own git repo, one commit). A small Python module:

```python
def in_inclusive_range(value, low, high):
    """Return True if value lies within [low, high], inclusive on both ends."""
    return low <= value < high        # bug: excludes the high bound
```

4 pytest tests, 2 pass / 2 fail. One test is deliberately misleadingly named
`test_in_inclusive_range_handles_all_edges` — its low-edge assertion passes and
masks the fact that only the high-edge assertion actually catches the bug.

Codex budget used: **2 of 3** authenticated `codex exec` runs (task 1 read-only
investigation, task 2 `resume`+fix). One additional attempt hit a CLI syntax
error (`--sandbox` not accepted on `exec resume`) and returned in 39ms without
contacting the model — not counted against budget. Task 3 used `--help` output
only, as instructed.

No edits were made to the kitsu repo itself.

---

## Headline: this cycle's real story is command containment, not tool discovery

Auditor 1 (cycle 7) flagged the object-form `--allow-command` pregrant as
implementation-ready backlog. This audit was asked to verify it works as
documented for the most natural CI use case — running the project's own test
runner. **It does not, for any interpreter-based toolchain**, and the reason
is two independent, undocumented containment requirements that only surface
at runtime via `PolicyDenied`, discovered here by forcing the model to make
the exact pre-approved call directly (bypassing the separate problem that the
model won't discover the grant on its own — see gap 3).

---

## Ranked gaps

### 1. CRITICAL — mandatory command containment strips the environment, breaking every interpreter-based test runner even when all filesystem roots are correctly granted

Severity: **critical** — this is the single blocking failure for "run my
tests" automation, the core primitive of the debugging workflow this cycle
exists to audit.

Repro (JSONL, non-interactive, exact object-form pregrant, executable's own
directory *also* granted as a `--command-root` to clear gap 2 first):

```bash
cd /tmp/cycle8-audit2/fixture-kitsu
kitsu --config kitsu.toml --gateway ollama --model qwen2.5-coder:latest \
  --timeout-secs 300 --max-rounds 5 --jsonl \
  chat --enable-commands \
  --command-root /tmp/cycle8-audit2/fixture-kitsu \
  --command-root /home/mmpg/.local/bin \
  --command-root /home/mmpg/.local/lib \
  --allow-command '{"argv":["pytest","-v"],"cwd":"/tmp/cycle8-audit2/fixture-kitsu"}' \
  --no-session \
  'Call command_exec right now with argv ["pytest","-v"] and cwd "/tmp/cycle8-audit2/fixture-kitsu" — that exact invocation is pre-approved. Just call it.'
```

`tool_finished` for `command_exec`:

```json
{"success": true, "command": {
  "ambient_authority": {"inherits_environment": false, "filesystem_confined_to_command_roots": true, "network_confined": true},
  "executable": {"observed_path": "/home/mmpg/.local/bin/pytest", "resolution": "ambient_path"},
  "exit_code": 1,
  "stderr_tail": "Traceback (most recent call last):\n  File \"/home/mmpg/.local/bin/pytest\", line 5, in <module>\n    from pytest import console_main\nModuleNotFoundError: No module named 'pytest'\n"
}}
```

The command itself ran (`success: true` at the transport level — the tool
executed and returned a real exit code), but pytest cannot import itself
because `inherits_environment: false` clears the process environment
entirely, not just the filesystem view. Adding every plausibly-relevant
directory as a `--command-root` (`~/.local/bin`, `~/.local/lib`,
`/usr/lib/python3.13`, `/usr`) does not help, because the interpreter's
module resolution depends on environment state (`PYTHONPATH`, `HOME`,
`VIRTUAL_ENV`, `PYTHONUSERBASE`, `NODE_PATH`, …), not just filesystem
visibility, and **no `--allow-command`/`--command-root`/`chat` flag exists to
pass through even a single named environment variable.**

Contrast — the identical pregrant pattern with a self-contained static binary
succeeds cleanly:

```bash
--allow-command '{"argv":["ls","-la"],"cwd":"/tmp/cycle8-audit2/fixture-kitsu"}'
```
→ `exit_code: 0`, full `stdout_tail` with the directory listing, `success: true`.

So the containment model is sound for `ls`/`git`/`cat`-class tools and broken
for `pytest`/(almost certainly) `npm test`/`cargo test` with a non-vendored
toolchain — i.e. broken for the overwhelming majority of real projects' test
commands, which is exactly what this cycle's workflow needs.

Proposed acceptance test: `--allow-command` (or a new `--command-env
VAR[=value]`/`--command-env-passthrough VAR` flag) can run a venv- or
nvm-managed test command (e.g. `pytest -v` against a project with a local
`.venv`, or `npm test` against a project with `node_modules`) and the command
completes with the project's own exit code and real stdout — not a
`ModuleNotFoundError`/`command not found` artifact of the sandbox itself. At
minimum, `doctor`/`--allow-command` validation should detect "approved
executable is a script requiring an interpreter search path that isn't
mounted" and warn at startup rather than let it fail silently mid-run.

---

### 2. HIGH — the executable's own directory must separately be an approved command root, and this is undiscoverable except via a runtime `PolicyDenied`

Severity: **high** — compounds gap 1; makes the documented pregrant recipe
non-functional as written for any tool not physically inside the project's
`--command-root`.

Repro: run exactly the `--enable-commands --command-root <project-dir>
--allow-command '{"argv":["pytest","-v"],"cwd":"<project-dir>"}'` recipe shown
in `kitsu chat --help` / README, with no extra roots:

```
"detail":"policy denied: mandatory command containment (ExecutableOutsideVisibleRoots) is unavailable: approved executable '/home/mmpg/.local/bin/pytest' is outside every command/runtime root; add its narrow toolchain directory as a read-only runtime root; install/enable bubblewrap or disable the command tool","error":"PolicyDenied"
```

The error message itself is well-written and actionable once you see it. The
problem is that neither `--command-root`'s help text nor `--allow-command`'s
help text nor the README's pregrant examples mention that the resolved
executable's directory is a *second*, independent root requirement on top of
the working-directory root and the exact argv/cwd grant. A CI author
following the documented recipe verbatim gets a clean, well-formed
`PolicyDenied` on their first real test run, with no way to have anticipated
it from `--help`.

Proposed acceptance test: `kitsu chat --help` / `--allow-command`'s
description, or `kitsu doctor`, states the executable-root requirement
explicitly, ideally with a one-line "did you mean: also pass `--command-root
$(dirname $(command -v pytest))`" style hint attached directly to the
`PolicyDenied` error (the error already names the missing path — it should
suggest the fix inline, the way `context-overflow` errors already do per the
cycle-7 backlog note).

---

### 3. HIGH — pregranted commands are invisible to the model; it can only guess, and a wrong guess costs a denied round-trip

Severity: **high** — this is the reason the read-only investigation (task 1)
never ran the tests at all, and the reason the "fix it" turn's first
spontaneous tool call (task 2) was a wrong guess.

Source-level confirmation (independent code-search agent, this session):
`command_exec`'s tool schema description
(`crates/kitsu-tools/src/builtins/command.rs:2817-2827`) is generic ("Run one
explicitly approved developer command as a literal argv array... Exact
approval is separate from mandatory OS containment") and never enumerates
`--allow-command` grants. `chat_system_prompt`
(`crates/kitsu/src/main.rs:5520-5579`) appends one boilerplate sentence when
commands are enabled ("Use command_exec's literal argv/cwd interface for
relevant builds, tests, and linters...") independent of whether any pregrant
exists. `PromptEnvironment::from_grants` only classifies filesystem
read/write roots into the prompt — it structurally never receives
`session.command_roots` or the exact-grant list. `grep` of
`crates/kitsu-core/src/orchestrator.rs` for `pregrant`/`exact_grant` returns
zero hits: the request-building code that talks to the model never injects
grant contents. The only place grants are surfaced is `/permissions`'
JSONL/human status output — session metadata, not a chat message.

Observed consequence, task 1 (`/tmp/cycle8-audit2/kitsu-task1-plain.log`,
7m07s wall clock): with `--enable-commands --command-root
<dir> --allow-command '{"argv":["pytest","-v"],"cwd":"<dir>"}'` all present,
the model made exactly two tool calls in the whole session — `List(.)` and
`Read(./test_calc.py)` — and never attempted `command_exec` at all, answering
purely from the misleadingly-named test's inline comment.

Observed consequence, task 2 first attempt
(`/tmp/cycle8-audit2/kitsu-task2-plain.log`): the model's one spontaneous
command guess was `Command(argv="pytest" "./test_calc.py"; cwd=".")` —
plausible, and wrong on both argv and cwd relative to the actual grant —
denied instantly:
`policy denied: command "pytest" "./test_calc.py" in '.../fixture-kitsu' has not been approved`.

Proposed acceptance test: when `--allow-command` grants exist, the system
prompt (or the tool description) lists the exact pre-approved argv/cwd pairs
verbatim, so a tool-capable model can choose to call one without guessing.
`/permissions`-style provenance already exists for the human/JSONL surface —
threading the same data into the one place that actually reaches the model
(the prompt builder) closes the gap with no new plumbing.

---

### 4. MEDIUM — `--resume <ID>` and `--session` cannot be combined, contradicting the documented "implies" relationship

Severity: **medium** — a CLI papercut, but a very likely one for any script
that defensively passes both flags (as this audit's own first attempt did,
following the README's own phrasing).

Repro:

```bash
kitsu chat --resume s-...-0 --session --enable-commands ... "now fix it and prove the fix"
```
```
! error: error: the argument '--resume [<ID>]' cannot be used with '--session'
```

README, `--resume` row: *"Resume and autosave a saved session in
interactive, scripted, or piped chat... `--resume implies this`"* (referring
to `--session`). The documented behavior is that `--resume` already turns on
session persistence; the actual behavior is that naming both explicitly is a
hard `clap` conflict rather than the harmless no-op the "implies" language
promises.

Proposed acceptance test: `kitsu chat --resume <id> --session ...` succeeds
identically to `kitsu chat --resume <id> ...` (or the help/README text is
corrected to say the two are mutually exclusive, whichever is the intended
design).

---

### 5. MEDIUM — even 5x–10x the default timeout is not reliably enough for a tool-heavy resumed turn on a local model, and the failure mode is a full turn loss

Severity: **medium** — mostly a restatement of an existing, documented
caveat, but with concrete fresh numbers from a *realistic* two-tool-call turn
(not a synthetic worst case), and one new detail: raising the timeout does
not fix non-determinism in whether the model uses tools at all (see the
model-fault ledger — the identical prompt at a higher timeout produced a
zero-tool-call prose answer instead of another timeout).

Timeline, task 2, same resumed session, three consecutive attempts:

| attempt | `--timeout-secs` | outcome | wall time |
|---|---|---|---|
| 1 | 600 (5x default) | provider deadline expired mid-round after 2 failed tool calls; turn discarded, file untouched | ~10m (backgrounded, hit deadline) |
| 2 | 1200 (10x default) | completed in-budget, but the model wrote a prose "here's how you'd fix it" walkthrough with **zero tool calls** — no edit, no test rerun | 6m10s |
| 3 | 900, highly directive prompt | two `fs_edit_file` calls, both denied (`old_text was not found` — model never read `calc.py` first, so it hallucinated the current source); turn discarded | ~6m |

None of the three produced a working fix; all three consumed a full local
inference budget for zero durable progress. This matches the README's own
warning that tool-heavy local rounds "routinely exceed even 300s," but the
audit's own numbers show that raising the budget alone does not convert a
failed round into a successful one when the underlying issue is tool-use
discipline (see model-fault ledger) — worth surfacing because a naive
"just raise `--timeout-secs`" fix, which is the documented remediation, does
not actually solve this failure mode.

Proposed acceptance test: no new kitsu behavior required here beyond
continuing to track this as a known local-model-latency caveat; flagging
because this cycle's evidence shows the caveat has more bite for *resumed,
multi-tool-call* turns than for the single-shot turns earlier caveats were
measured against.

---

## Task 3 — automation angle: how would a CI bot script this?

**Investigation step ("do the tests fail, why").** `--jsonl` gives a CI bot
everything it needs and more, once gaps 1–3 above are worked around: ordered
`tool_started`/`tool_finished` events, and for `command_exec` specifically a
fully-typed `command` object (`argv`, `cwd`, `exit_code`, `signal`,
`stdout_tail`/`stderr_tail`, byte counts, truncation flags, artifact
reference, `ambient_authority` flags) — richer, structured telemetry that a
shell-parsing equivalent in codex's plain-text `exec` transcript does not
give for free (codex's transcript is human-readable log text; kitsu's JSONL
is schema-typed). This is a genuine kitsu strength for the automation angle,
*conditional on* gaps 1–3 being fixed or worked around (e.g. running tests
from inside a fully self-contained `--command-root`, such as a venv or
statically-linked binary vendored into the repo).

**Fix step ("prove the fix").** Verified directly (once the model was given
copy-paste-exact `old_text`/`new_text` so it stopped hallucinating file
content — see model-fault ledger): a successful `fs_edit_file` call's
`tool_finished.output` is a JSON-encoded string:
```json
{"bytes":223,"change":"updated","edited":"/tmp/.../calc.py","previous_bytes":222,"replacements":1,"warning":null}
```
This *is* machine-parseable (path, change kind, byte delta, replacement
count) — a CI bot can already detect "a file was mutated and which one"
without a dedicated event type. What it does **not** give: a diff/patch
preview or the "first changed line" number that human-mode prose gets
(README: "Writing turns report verified files/first changed lines"), and it
is a string-encoded ad hoc payload rather than a first-class typed field the
way `command` is for `command_exec`.

**Does the backlogged file-mutation event actually matter for this
workflow?** Assessed with live evidence: **not as a blocker.** A CI bot
scripting "fix it and prove it" has two independent, better evidence sources
already available and arguably more trustworthy than parsing model-tool
telemetry: (1) `fs_edit_file`'s existing `output` string, adequate for "did
something change and what path," and (2) running `git status --short`/`git
diff` as the bot's own post-turn step (or via kitsu's host-verified `/diff`,
once its model-read-root gating — flagged by cycle-7 auditor 1 — is
resolved), which is authoritative regardless of whether the model used
`fs_edit_file`, `fs_write_file`, or a shell redirect via `command_exec`. The
backlog item still has residual value — promoting the ad hoc string to a
typed schema field, and attaching an actual diff hunk to the event so a JSONL
consumer never needs a second host command — but it is a polish item for
this specific workflow, not a functional gap. Recommend re-ranking it
accordingly relative to gaps 1–3 above, which *are* functional blockers.

**Codex automation surface (help output only, no run):** `codex exec --json`
prints the same structured event stream as interactive `exec`; `codex exec
resume <id|--last> [PROMPT] --json` resumes non-interactively. One
inconsistency worth noting for fairness: `codex exec resume` does **not**
expose `--sandbox` as a direct flag (confirmed: `error: unexpected argument
'--sandbox' found`) — the equivalent override is `-c
sandbox_mode="workspace-write"`. A CI script that assumes symmetry between
`codex exec` and `codex exec resume` flag surfaces will hit this the same way
this audit did. Not a kitsu finding, but relevant context for a fair
side-by-side.

---

## Session continuity: `kitsu --resume` vs `codex exec resume`

Both tools resume cleanly and both correctly re-establish working context.
Differences observed:

- **Grants.** kitsu explicitly does *not* restore saved permission grants on
  resume ("saved grants were not restored; runtime grants reset") — by
  design, and clearly announced in the resume banner. Every resumed
  invocation in this audit had to re-supply `--allow-command`/`--allow-read-write`
  from scratch. Codex's `-c sandbox_mode=...` override is the equivalent
  per-resume re-statement; codex does not print an equivalent "here's what
  carried over vs. reset" banner — it's silent about it either way.
- **Context re-verification.** Codex's resumed turn re-ran `git status
  --short && sed -n calc.py` (0ms) before touching anything, catching up on
  real state rather than trusting the prior turn's memory. Kitsu's resumed
  turn prints "rebuilt saved system context from current tools, project
  metadata, and bounded skill provenance" — a comparable mechanism — but
  never re-read `calc.py` in this audit's three attempts before trying to
  edit it, which is what produced the hallucinated-`old_text` failures (see
  gap discussion above; this is scored as a model-fault, not a harness
  fault, since kitsu's `fs_edit_file` precondition correctly rejected the
  bad edit rather than corrupting the file).
- **CLI ergonomics.** kitsu: gap 4 above (`--resume`+`--session` conflict).
  Codex: the `--sandbox` asymmetry noted above. Roughly a wash.

---

## Model-fault ledger (qwen2.5-coder:latest — not kitsu bugs)

Kept separate per the audit brief. All of the following are attributable to
a small local model's weak tool-use discipline, and in every case kitsu's
harness behaved correctly and safely around the failure (denied a
non-matching command, rejected a stale edit precondition, discarded a
turn with failed tool calls rather than reporting false success):

1. Never attempted to run the pregranted `pytest` command during the
   read-only investigation — answered correctly, but by reading the
   misleading test comment rather than by executing anything.
2. First spontaneous "fix it" attempt guessed a plausible but wrong
   argv/cwd (`pytest ./test_calc.py` in `cwd="."`) instead of the exact
   granted invocation — denied.
3. Second "fix it" attempt (at a longer timeout) produced a prose-only
   "here's how you'd fix it" tutorial with **zero tool calls** — never
   edited the file, never ran anything, despite write and command grants
   being present.
4. Third "fix it" attempt (explicit, directive prompt naming the exact tool
   calls to make) tried `fs_edit_file` twice with a hallucinated `old_text`
   it never verified by reading `calc.py` first — both edits correctly
   rejected by kitsu's edit-precondition guard.
5. In a separate short TTY-capture run, asked to "list the files... one
   word answer," the model answered `calclib/` — a fabricated, wrong
   directory name unrelated to the actual (correctly executed) `List(.)`
   result.
6. The fix *did* succeed once the audit gave the model exact copy-paste
   `old_text`/`new_text` and a highly directive prompt — confirming these
   are model-discipline failures, not harness capability gaps: kitsu can
   run this workflow end-to-end (`fs_edit_file` → `pytest -v` → 4 passed)
   when the model actually uses its tools correctly.

---

## Fox identity in a tool-heavy session

Prior finding (cycle 6): "kitsu's mascot is a cat, and its glyphs are Claude
Code's." **Confirmed fixed, with fresh evidence.** A styled-TTY capture (via
`script`, real pty) of a short tool-using session shows: the closed banner
box with `🦊 kitsu 狐`, fox-ears/face ASCII mascot, and FOX-orange
(`38;2;255;140;0`) truecolor throughout; the spinner correctly cycles the
documented fox kaomoji frames (`( ･ᴥ･ )ﾉ` → `( ･ᴥ･ )～` → `( ･ᴥ･ ) ～` →
`( ﾉᴥﾉ )～`) with "kitsu is thinking" and an accumulating tilde trail; a tool
call/result pair renders as `◆ List(.)` / dim `◇ 5 lines`, matching the
documented glyph table exactly (not Claude Code's `⏺ ⎿ ✻`). This is a
genuine, verified parity-or-better item — cycle 7 Pass C's identity work
holds up under direct pty inspection.

Caveats, honestly scoped: this sample is small (one banner, one spinner run,
one tool-call/result pair) — not the "dozens of receipts" scale the brief
asked about, because reproducing that would have required a full styled-TTY
capture of one of the 6–10 minute multi-round investigation runs, which this
audit's time budget did not stretch to. Two things worth a follow-up rather
than claiming as findings here: (1) `◆` (tool call) and `◇` (tool result) are
visually close diamond variants at small terminal font sizes — plausible
legibility risk at real receipt density, unverified; (2) whether the spinner
or heartbeat behaves differently while a `command_exec` call is actually
running long (vs. the near-instant `ls`/`pytest` calls this audit's JSONL
runs happened to produce, both under 30ms) was not directly observed.

Separately: kitsu's non-TTY/`--plain` heartbeat (`~ still waiting on the
model (Ns)...`) is honest and legible but gives strictly less mid-turn
visibility than codex's default `exec` transcript, which streams the model's
short preamble sentence plus the full command and its real output inline as
each step happens. kitsu's human-mode tool receipts are deliberately terse
(`tool> Read(./test_calc.py)` / `result> 25 lines` — line counts, not
content) — a defensible noise-control choice, but for *this* workflow
specifically (watching a debugging investigation unfold) it means a user
cannot see what a command actually printed without waiting for the model to
relay it in its final answer, or switching to `--jsonl` and reading
`stdout_tail` by hand. Not added to the ranked list (it's a legibility/detail
trade-off, not a functional break), but worth the implementation team's
attention if "watchability" of tool-heavy runs becomes a design goal.

---

## Top 5, ranked

1. **CRITICAL** — mandatory command containment strips the environment,
   breaking pytest (and by strong inference, any interpreter-based test
   runner with external dependencies) even with correct filesystem roots
   granted. No env passthrough mechanism exists. (Gap 1)
2. **HIGH** — a second, undocumented containment requirement (executable's
   own directory must be a separate approved root) makes the documented
   `--command-root`/`--allow-command` recipe fail on first real use for any
   toolchain not vendored inside the project directory. (Gap 2)
3. **HIGH** — pregranted commands are structurally invisible to the model
   (confirmed at the source level: neither the tool schema nor the system
   prompt reference grant contents), so the model can only guess an
   invocation and risk denial — directly observed causing both a skipped
   investigation and a denied fix attempt. (Gap 3)
4. **MEDIUM** — `--resume` + `--session` together is a hard CLI error,
   contradicting the README's documented "implies" relationship. (Gap 4)
5. **MEDIUM** — timeout budgets up to 10x the default are not a reliable
   fix for tool-heavy resumed local-model turns; the failure mode is total
   turn loss, and raising the budget does not address the underlying
   tool-use non-determinism. (Gap 5)

Honorable mention, re-scored down from where the task brief implied it might
land: the backlogged JSONL file-mutation event is **not a blocker** for this
workflow — existing `fs_edit_file`/`fs_write_file` output already carries
enough machine-parseable signal (path, change kind, byte delta), and a CI
bot's own post-turn `git diff` remains the more authoritative evidence
source regardless. Recommend treating it as polish, ranked below gaps 1–5,
not alongside them.

---

## Parity-or-better notes (kitsu vs. codex, honest accounting)

- **Fox identity**: confirmed fixed and holding up under real pty
  inspection — better than the state cycle 6 found.
- **Structured command telemetry**: kitsu's `--jsonl` `command` object
  (argv/cwd/exit_code/stdout_tail/artifact reference/ambient-authority
  flags) is richer and more directly machine-consumable for CI purposes than
  parsing codex's plain-text `exec` transcript would be — a genuine
  automation-angle win, once containment gaps 1–2 are addressed.
- **Edit safety**: `fs_edit_file`'s exact-precondition guard correctly
  refused two hallucinated edits rather than corrupting the file or
  fabricating a false-success receipt — the harness behaved exactly as
  designed under model failure, in both cases.
- **Where kitsu currently loses on this specific workflow**: codex's hosted
  model completed the identical read-only investigation in 33s and the
  identical fix-and-prove step in 21.5s, both fully autonomous (no
  pregrant plumbing, no containment errors — the run-as-you-go sandbox
  granted `/tmp`, `$TMPDIR`, and workdir by default and codex's model reads
  files, diffs, and runs tests without being told to). This gap is
  primarily model-quality and hosted-vs-local, explicitly out of scope for
  kitsu UX scoring per the audit brief — but the containment friction (gaps
  1–3) is real kitsu-side friction layered on top of it, and would still
  block a *strong* model from running this project's actual test suite
  under the documented pregrant recipe.
