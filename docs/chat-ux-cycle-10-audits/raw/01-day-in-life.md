# Cycle 10 — Audit 1: A day in the life of an OSS maintainer (Codex CLI vs kitsu)

Binary: kitsu 0.1.0 (c54e094c4bd9), copied to /tmp/cycle10-audit1/kitsu before the fix agent's rebuild.
Gateway: `--gateway ollama --model qwen2.5-coder:latest --timeout-secs 300` (local; kitsu.toml declares context_window 8192).
Codex: codex-cli 0.147.0, gpt-5.6-sol, reasoning max. 2 of 3 allowed runs used (orientation, implementation); run 3 kept in reserve.
Machine note: a concurrent fix agent was rebuilding the repo's target/ the whole time (load avg ~6.5, ollama 100% CPU) — all kitsu wall-clock numbers are inflated by that, and by the model tier gap. Latency deltas are environmental; *behavioral* deltas are the findings.

Fixture: /tmp/cycle10-audit1/proj-template — "tinyver", a dependency-free Python semver
library (8 files + .gitignore, 2-commit git history, green 6-test suite). ISSUE.md (#7)
asks for a bugfix (compare() uses lexicographic string comparison → "1.10.0" < "1.9.0",
and skips validation) and a feature (bump(text, part)). Clones: proj-codex, proj-kitsu.
Logs: /tmp/cycle10-audit1/logs/ (codex-orient.log, codex-impl.log, kitsu-orient.log, kitsu-impl.log).

---

## Narrative

### Morning — Orientation ("read ISSUE.md and tell me the plan")

**Codex (run 1, read-only sandbox): 86.6s, exit 0.** Ten visible `exec` steps: found
ISSUE.md via `rg --files`, read every relevant file with `nl -ba`, read `git log -p`,
ran the existing suite, probed the bug live with an inline Python snippet
(`compare('1.10.0','1.9.0') => -1`, `compare('junk','1.0.0') => 1`). It hit the
read-only sandbox wall (pytest needs a writable tmpdir), *explained* it, and found a
write-free verification path (`PYTHONPATH=. python3 tests/test_tinyver.py`). Final
answer: crisp 6-step plan with file/line links and the correct root cause (compare()
bypasses the validating parse()). Worktree clean. Time-to-first-useful-output ~5s.

**Kitsu (scripted chat, --allow-path, --session): 502s, exit 2.** Opens with a good
receipt (session id + "invocation grants: 1 read, 0 write"). `◆ Read(ISSUE.md)` /
`◇ 36 lines` receipts are clear. Then qwen spawned SIX background agents for a
read-only planning question — including agent #6 "commit and push all modifications"
despite "do not make any changes yet". The plan text landed at ~280s and was
delegation theater ("Background Agent #1 will analyze…"); script exit then joined
stragglers for another ~220s with zero per-agent progress (only the main turn's
"~ still waiting on the model (Ns)…" heartbeat). Joined results were garbage (raw
tool-call JSON as text, a JavaScript bump() for a Python repo, "Please provide the
code for review"); 3 subagents timed out at the provider. Kitsu's accounting was
exemplary: per-job labels, `! error:` lines, "retained 3 failed background agent
result(s); unacknowledged detached failures are not successful automation", script
summary, exit 2. Read-only confinement held — worktree untouched even with a
commit-tasked agent. End-of-turn `kitsu-usage` / `kitsu-context` (used 2275 /
effective 7680 / window 8192) receipts are genuinely useful; codex prints one bare
"tokens used" number.

### Afternoon — Implementation (feature + fix, tests must pass)

**Kitsu (scripted, --allow-read-write + --enable-commands + --command-root +
object-form --allow-command for `python3 -m pytest -q`): 547.9s, exit 1.**
Flag setup was heavy but coherent (4 permission flags to authorize "edit this
project and run its tests"); no prompts fired — every grant was honored as
pre-declared, and the pre-approved argv ran without any approval friction.
Turn 1: model read all three files directly (no agent storm this time), then:
- `◆ Edit(tinyver.py)` → `◇ error: InvalidArgs — edit conflict: old_text was not
  found; file was not modified` (model fault; receipt excellent).
- `◆ Edit(tests/test_tinyver.py)` → `◇ updated · 1715 bytes` — but the payload was
  markdown-fenced diff garbage with literal `\n` sequences.
- The pre-approved pytest command ran: `◇ exit 1 | 23 ms | stderr tail="/usr/bin/
  python3: No module named pytest"` — pytest works on the host but is invisible
  inside command containment (user-site ~/.local not reachable).
- Then the strict-turn machinery fired: "provisional assistant output was DISCARDED…
  1 tool call(s) failed during the turn", `kitsu-context-rollback: incomplete turn
  removed from conversation state`, "failed turn rolled back; completed file
  changes, if any, remain on disk", plus a `◈ verified file checkpoint #1` receipt
  showing the exact snapshot-derived diff of the corrupted test file.
Turn 2 ("run the suite, fix until green"): model reran pytest (same module error),
then told the *user* to `pip3 install pytest` and stopped. Script exit 1.

**End state on disk (ground truth):** tinyver.py unchanged (no fix, no bump()),
tests/test_tinyver.py corrupted — the suite regressed from 6 passed to a pytest
*collection error* (SyntaxError). Kitsu never claimed success anywhere; its own
receipts are the only reason the corruption was visible without opening files.

**Codex (run 2, workspace-write): 65.9s, exit 0.** Clean 3-file diff: compare()
rewritten through parse() with tuple comparison, textbook bump() with resets and
ValueError, 7 new tests incl. subTest-parameterized malformed-input cases, tidy
Keep-a-Changelog entry under Unreleased. Reported "13 passed, 2 subtests" — and the
host re-run confirmed exactly that. What it reported matched disk perfectly.

### Evening — Wrap-up ("summarize what changed, draft CHANGELOG entry")

Not executed. The environment (shared CPU with the rebuild agent; ~9 min per kitsu
run) exhausted the session budget, and the coordinator called time. The planned
probe — whether the model grounds the summary in `/diff` / the new `workspace_diff`
tool vs stale in-session memory — is unrun; note that after this session's turn-1
rollback, "in-session memory" would have been actively *wrong* (the corrupting edit
was erased from conversation state but persists on disk), which makes the grounding
question sharper, not moot. Recommended as the first probe of cycle 11.

---

## Per-gap findings

### G1 — Failed-turn rollback silently diverges conversation state from disk state
- **Repro:** scripted chat, strict turns; a turn makes one successful `fs_edit_file`
  write then any other tool call fails → turn is discarded and removed from context
  ("kitsu-context-rollback"), but the successful write persists ("completed file
  changes, if any, remain on disk"). The next turn's model has no memory of the edit
  it made, and nothing re-presents the retained changes to it. In this run the
  retained edit was file-corrupting garbage; turn 2 then reasoned about a worktree
  it couldn't know about.
- **Codex:** no rollback concept — the model always sees its own failed attempts and
  iterates on them; disk and context never diverge.
- **Severity:** high (automation correctness; worse-than-start worktrees exit
  "honestly failed" but repair requires out-of-band knowledge).
- **Acceptance test:** given a strict scripted turn that fails after ≥1 completed
  file change, (a) the next turn's context contains a host-authored note enumerating
  retained changed paths (+ checkpoint id), and (b) the end-of-run summary lists
  "N file change(s) from failed turns remain on disk; /undo or `kitsu artifacts`/
  checkpoint restore to revert". Optional: `--revert-files-on-turn-failure` flag
  restoring checkpoint #N-1 automatically in scripted mode.

### G2 — Command containment hides the host's Python user-site; no remedy hint
- **Repro:** host `python3 -m pytest -q` passes (pytest pip-installed under
  ~/.local); same argv pre-approved and run contained → `No module named pytest`,
  exit 1, twice. Model gave up and told the user to install pytest.
- **Codex:** workspace-write sandbox ran the identical command first try.
- **Severity:** high (the canonical test-runner loop — the core of the
  implement-verify cycle — fails out of the box on pip-user toolchains; `--command-env
  PYTHONPATH` is documented in help, but nothing at failure time points to it, and
  PYTHONPATH alone may not fix a missing mount of ~/.local).
- **Acceptance test:** a contained command failing with stderr matching
  `No module named X` (or `command not found`) produces a host diagnostic naming the
  concrete remedies (`--command-env`, mount/root additions) the way the
  outside-root executable denial already names the directory to add; and/or
  `--allow-command` preflights the argv at grant time and warns before the session
  starts burning model rounds on it.

### G3 — Agent-spawn storm on a plain planning turn, despite cycle-9 prompt guidance
- **Repro:** orientation script, read-only grant, "do not make changes yet" → 6
  background agents (analysis, design, 2× implementation, review, commit+push),
  serialized on one local endpoint; 3 provider timeouts; plan answer delayed ~4×;
  exit join added up to `--agent-join-timeout` more.
- **Codex:** no subagent facility to misuse; explored inline, 86.6s end-to-end.
- **Severity:** medium-high (root cause is the model, but kitsu offers an unbounded
  footgun to models known to misuse it; cycle 9's system-prompt line demonstrably
  does not restrain qwen2.5-coder).
- **Acceptance test:** with a config/flag cap (e.g. `max_background_agents = 2`,
  scripted default low), the 7th..Nth spawn returns a tool error the model sees;
  additionally, spawning a write-intent agent in a session whose grants are
  read-only yields an immediate host warning receipt rather than a slow downstream
  denial. Cap overridable; interactive default may stay generous.

### G4 — Progress blindness while joining/running background agents
- **Repro:** during the 6-agent orientation join, the transcript shows only the main
  turn's "~ still waiting on the model (Ns)…" heartbeat, then a single "input ended
  with 4 background agent(s) running; joining them (up to 300s…)" line, then
  silence until results dump. No per-agent state, elapsed, or completion ticks.
- **Codex:** every exec/tool step streams inline with duration; nothing runs
  invisibly.
- **Severity:** medium (in a long session the user cannot distinguish progress from
  wedge; also 13+ stacked heartbeat lines between receipts drown the ◆/◇ signal in
  non-TTY transcripts).
- **Acceptance test:** during join, a periodic line per still-running agent
  (`~ agent #3 still running (120s)`) and a completion tick (`◇ agent #3 done`);
  in non-TTY output, consecutive heartbeats for the same wait collapse or step up
  exponentially (10s, 30s, 60s, …).

### G5 — `full output=` artifact paths are elided into uselessness in ◇ receipts
- **Repro:** command receipt prints `full output="/home/mmpg/.kitsu/profiles/
  v1-fnv128-…/sessions/….artifacts/runtime-…/c..."` — truncated mid-filename, so the
  one pointer to the full stdout/stderr cannot be copied or opened.
- **Codex:** prints output inline (bounded), no artifact indirection needed.
- **Severity:** low-medium (the artifact system is a differentiator; the receipt
  defeats it exactly when output is big enough to matter).
- **Acceptance test:** the artifact path is never truncated (wrap instead), or the
  receipt prints a short stable id resolvable via `kitsu artifacts <id>`; a
  terminal-width-60 render still yields a usable pointer.

### G6 — Configured context window vs served context window silently disagree
- **Repro:** kitsu.toml sets `context_window = 8192`; `ollama ps` during the run
  showed the model loaded with CONTEXT 4096. kitsu-context receipts budgeted
  against 7680 effective while the server truncates at 4096 — silent quality loss
  attributed to the model.
- **Codex:** n/a (hosted).
- **Severity:** medium (kitsu's context budgeting is a headline feature; it is only
  as truthful as an unverifiable config number; the config comment even warns
  ollama won't report it).
- **Acceptance test:** `kitsu doctor` (online) probes the effective served window
  (e.g. a padded prompt probe or reading `ollama ps`-equivalent API when the
  gateway is ollama) and warns on mismatch with the configured `context_window`.

---

## Ranked top-5

1. **G1** rollback/disk divergence after failed strict turns (correctness of the
   whole scripted-automation story).
2. **G2** contained test-runner can't see host toolchain + no failure-time remedy
   (breaks the edit→test→fix loop that defines the product).
3. **G3** unbounded agent spawning by small models on trivial turns (cost the
   orientation 6× codex's latency and produced zero value).
4. **G4** join/heartbeat progress blindness + heartbeat stacking in transcripts.
5. **G5** elided artifact paths in command receipts.

(G6 next; it is a doctor-feature ask rather than a session-UX defect.)

## Parity-or-better (kitsu vs codex, observed this session)

- **Failure honesty:** exit codes 2/1 with per-turn and per-agent accounting
  ("unacknowledged detached failures are not successful automation") — codex exec
  gives you exit 0 and prose. Kitsu never overstated its end state; its report
  matched disk both runs. Codex also matched disk — parity on truthfulness, kitsu
  better on machine-readable failure signaling.
- **◈ verified checkpoint receipts with snapshot-derived diffs:** the corrupted
  test edit was visible in the transcript itself, no `git diff` needed — codex has
  nothing equivalent; this is kitsu's best receipt and it earned its keep today.
- **Per-turn `kitsu-usage` / `kitsu-context` lines** vs codex's single final token
  count.
- **Grant receipts:** "invocation grants: 1 read, 0 write" up front; confinement
  held against an agent explicitly tasked to commit and push. Codex's sandbox also
  held (pytest tmpdir failure) — parity on enforcement, kitsu better on disclosure.
- **Command receipts:** exact argv, cwd, exit, duration, stderr tail — as good as
  codex's exec lines, plus durable artifact storage (see G5 for the blemish).
- **Session continuity surface:** autosave + `sessions list` with cwd + first-turn
  preview; grants-never-restored policy stated inline at session start.
- **Pre-approved command ergonomics:** object-form `--allow-command` bound to a cwd
  ran with zero prompts, exactly as designed.

## Fox identity in real work

◆/◇ are learnable inside one session and pair visually; ◈ (durable
custody/checkpoint) appeared once and was immediately the most important line in
the transcript — the semantics (filled=live, half=custody, hollow=spent) held up in
practice. ⛩ never appeared (no compaction/resume milestone this session), so its
learnability is untested here. The identity blurs not from the glyphs but from the
`~` heartbeat: on a slow local model, runs of 10-15 "~ still waiting" lines between
receipts dominate the scroll and make ◆/◇ landmarks hard to scan back to (G4).
`--plain` fallback mapping (`* > o # *`) verified present in the render crate.

## Cycle 6-9 regression check

- Cycle-8 failure-accounting behavior: present and correct (retained detached
  failures fail the script). No regression.
- Cycle-9 spawn-guidance system-prompt line: present in intent but empirically
  insufficient for qwen2.5-coder (G3) — efficacy gap, not a regression.
- Cycle-9 `workspace_diff` backlog item: now implemented in main.rs
  (`parse_workspace_diff_request`, disclosure events); untested this session
  (wrap-up unrun) — flag for cycle 11.
- Session autosave/list/resume surfaces from earlier cycles behaved as documented
  (config-scoped listing correctly attributed sessions per config).

## Model-fault ledger (environmental; not kitsu defects)

- qwen spawned 6 background agents for a read-only planning question, one tasked to
  commit+push against explicit instruction (kitsu confinement held).
- Hallucinated a JavaScript/package.json `bump()` for a Python repo; another agent
  emitted raw tool-call JSON as its "result"; a third replied "Please provide the
  code for review".
- 3 of 6 subagents timed out at the provider (single CPU-bound ollama serializing
  seven concurrent conversations; host under build load).
- `fs_edit_file` old_text mismatch on tinyver.py (never fixed the actual bug).
- Wrote markdown-fenced diff text with literal `\n` into the test file (the
  corruption in G1's scenario).
- Abandoned the "iterate until green" loop after one error, advising the user to
  `pip3 install pytest` instead of trying `python3 -m unittest` or asking for env.
- Ollama served num_ctx 4096 despite the model supporting more (see G6 for the
  kitsu-side ask); local-tier latency inflated all kitsu timings (~86s vs ~502s
  orientation, ~66s vs ~548s implementation — not a like-for-like model comparison).

## Raw data

- Fixture + logs: /tmp/cycle10-audit1/ (proj-template, proj-codex, proj-kitsu,
  logs/, scripts/).
- Kitsu sessions: s-1786458722698667316-2702806-0 (orientation),
  s-1786459292594577016-2717952-0 (implementation, includes checkpoint #1).
- Codex sessions: 019ff13c-b716-7151-… (orientation); implementation run in
  codex-impl.log. Codex quota: 2 of 3 small runs consumed.
