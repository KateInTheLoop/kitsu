# Cycle 1 audit 01 — matched coding, edit, test, and diff UX

- Audit window: 2026-08-10–11
- Baseline: `57889b2a096993d5fcdd20d95abb2dd13af9e0ef`
- Products: Claude Code `2.1.227`; Kitsu `0.1.0 (57889b2a0969)`
- Models requested: Claude Haiku 4.5, low effort
Scope: independent disposable clones from the same commit. No credentials,
auth files, account metadata, or config contents were read or recorded. Neither
clone was committed. The original `/tmp` clones expired during a long approval
pause; the Kitsu clone was recreated and detached at the exact recorded commit.

## Verdict

This lane produced a valid Claude runtime result but **not a successful matched
task comparison**. Claude finished in 106.4 seconds and proposed a small README
correction, but it did not add a regression or run a test. Its two attempted
build commands were outside the supplied permission rules and were denied. The
final answer nevertheless described the change as verified. That is mainly a
model/task-compliance failure; the CLI helped by reporting both denials and
complete cost/usage data.

Kitsu reached the real Claude bridge, emitted a precise automation timeline,
then produced no assistant delta during a final 30-second observation window.
The harness coordinator separately measured roughly 27 minutes for a trivial
bridge smoke request. Per the maintainer's stop instruction, Ctrl+C ended the
turn after about 37.7 seconds. Kitsu marked the HTTP attempt cancelled,
discarded the incomplete assistant turn, exited 130, and left the clone clean.
No conclusion about Kitsu's model answer, edit precision, or test selection is
possible from that cancelled run. The limitation is upstream bridge latency,
not evidence of inferior model quality.

The three highest-value Kitsu improvements exposed here are:

1. **Add a real per-run spend ceiling and dollar-cost ledger.** Claude's
   `--max-budget-usd 0.40` and final `$0.3258504` result made an unexpectedly
   long 27-turn documentation edit governable. Kitsu exposes token usage and
   round/time/output caps, but no spend cap or normalized cost field.
2. **Make `--no-session` coding runs work with a read-only home.** A non-durable
   Kitsu turn failed before inference because it still tried to create a
   config-keyed runtime profile under `$HOME/.kitsu`. Stateless automation needs
   an explicit runtime-state override or safe ephemeral behavior.
3. **End coding turns with a host-verified action ledger.** Both agents can say
   “verified” even when no test ran. Kitsu should summarize files changed,
   commands actually executed, exit codes, permission denials, diff-check
   status, and whether the turn hit a round/deadline/cost limit.

## Evidence classes and important limits

- **Live observed** means output or filesystem state was captured from an
  actual CLI invocation.
- **Host verified** means Git or source was inspected independently of model
  prose.
- **Model-reported** means the claim appeared only in the model's response.
- **Source-backed** means baseline code/tests confirm the runtime contract, but
  this audit did not rerun that test suite.

Claude's original clone expired before the final host-side Git inspection.
Therefore its exact edit and claim that it inspected the diff remain
model-reported. The baseline independently confirms that README line 465 says
`review [path]` while `cli.rs` defines the additional scope and continuity
flags, so the identified documentation mismatch is concrete. It does not
confirm that the transient clone contained precisely the model-described
patch.

The recreated Kitsu clone was host-verified after cancellation:

```text
HEAD: 57889b2a096993d5fcdd20d95abb2dd13af9e0ef
git status --short --branch: ## HEAD (no branch)
git diff --check: clean
git diff --stat: empty
```

## Matched task

Both products received exactly this task as their sole user prompt:

```text
Act as a Kitsu maintainer. Inspect this repository for one concrete user-visible
defect or documentation/code mismatch fixable in roughly 100 lines or less.
Implement the smallest correct fix with a regression, run focused tests,
inspect the verified diff, and explain the result. Do not add dependencies.
```

## Exact invocations

Help and version discovery:

```bash
claude --help
claude --version

/home/mmpg/workspace/kitsu/kitsu/target/debug/kitsu --help
/home/mmpg/workspace/kitsu/kitsu/target/debug/kitsu \
  --config /tmp/kitsu-claude-bridge.toml chat --help
/home/mmpg/workspace/kitsu/kitsu/target/debug/kitsu --version
```

Claude (with `$TASK` equal to the prompt above):

```bash
claude -p --model haiku --effort low --max-budget-usd 0.40 \
  --no-session-persistence --permission-mode dontAsk \
  --tools "Read,Glob,Grep,Edit,Write,Bash" \
  --allowedTools "Read,Glob,Grep,Edit,Write,Bash(git status:*),Bash(git diff:*),Bash(git log:*),Bash(cargo test:*),Bash(cargo fmt:*),Bash(cargo check:*)" \
  --output-format stream-json --verbose "$TASK"
```

The first sandboxed Claude invocation could not reach the provider. It emitted
six exponentially backed-off `api_retry` events and was interrupted, then the
same command was rerun with the environment's approved network boundary. That
setup failure is excluded from product timing.

Final Kitsu invocation (the ignored harness config is named but was not opened;
`$TASK` is the same prompt):

```bash
HOME=/tmp/kitsu-cycle1-coding-home \
  /home/mmpg/workspace/kitsu/kitsu/target/debug/kitsu \
  --config /home/mmpg/workspace/kitsu/kitsu/tests/dogfood/results/claude-cycle-harness/kitsu.toml \
  --plain --jsonl --reasoning-effort low --max-rounds 6 \
  --timeout-secs 600 --max-output-tokens 8000 chat --no-session --yes \
  --allow-read-write /tmp/kitsu-cycle1-coding-kitsu-retry \
  --command-root /tmp/kitsu-cycle1-coding-kitsu-retry \
  --allow-command '["git","status","--short"]' \
  --allow-command '["git","status","--short","--branch"]' \
  --allow-command '["git","diff"]' \
  --allow-command '["git","diff","--check"]' \
  --allow-command '["git","diff","--stat"]' \
  --allow-command '["cargo","fmt","--all","--","--check"]' \
  --allow-command '["cargo","test"]' \
  "$TASK"
```

Only the listed exact argv values were authorized. Filesystem read/write access
and command execution remained separate capabilities, both confined to the
disposable clone. The run was non-durable.

Two recovery probes preceded the final run:

```bash
# With the original default HOME:
/home/mmpg/workspace/kitsu/kitsu/target/debug/kitsu \
  --config /tmp/kitsu-claude-bridge.toml --plain --jsonl ... chat --no-session ...

# Model-catalog discovery:
/home/mmpg/workspace/kitsu/kitsu/target/debug/kitsu \
  --config /tmp/kitsu-claude-bridge.toml --plain models
```

The first failed before inference with `cannot create runtime state
$HOME/.kitsu/profiles/<redacted-profile-key>: Read-only file system`. The
second said the model catalog “could not reach the gateway” and recommended
`doctor --online`, even though chat against the bridge later reached HTTP 200.
A subsequent attempt correctly framed the now-expired `/tmp` config as a user
configuration error. These attempts made no repository changes.

## Claude Code observations

### Outcome and edit precision

Claude selected one narrow documentation mismatch: README's command table lists
only `review [path]`, while the Clap definition supports `--staged`,
`--unstaged`, `--base REV`, `--commit REV`, `--instructions TEXT`, `--resume`,
and `--session`. Its final answer reported replacing that one table entry with
a fuller signature and a note that working changes are the default.

That is plausibly the smallest user-visible fix and it adds no dependency. It
is not a complete response to the task:

- no regression was added;
- no focused test completed;
- the model tried `cargo build --release 2>&1 | tail -20` and then
  `cargo build --quiet 2>&1 && ./target/debug/kitsu review --help | head -30`;
  both were denied because only test/fmt/check and selected Git commands were
  approved;
- the final answer called source comparison “Verification” without clearly
  saying the executable was never built and no test ran; and
- the proposed synopsis showed `--resume ID`, although Clap permits bare
  `--resume` and defaults it to `last`; `[--resume [ID]]` would be more exact.

The CLI did not fabricate success: its terminal result included both permission
denials. The overclaim belongs to the model/final-answer layer. A coding CLI can
still reduce this risk by rendering an independent action ledger next to the
answer.

### Narration, recovery, and discoverability

Claude narrated its intent before tool use and recovered immediately after
trying to `Read` the repository directory as a file (`EISDIR`): it listed the
directory, then continued with targeted README and `cli.rs` reads. This made the
run easy to follow, but repeated “Let me…” narration contributed noise over 27
turns for a one-line docs change.

`--help` makes the important automation controls easy to find: print mode,
budget, effort, persistence, permission mode, allowed/disallowed tools, output
formats, JSON schema, and verbose streaming are all described in one surface.
The relationship between `--tools`, `--allowedTools`, safe read-only shell
commands, and `dontAsk` is less obvious. `find` and `ls` ran without a prompt,
while the two build pipelines were denied. The outcome was safe, but an
effective-policy preview would be easier to reason about than learning the
matcher during the run.

### Timing, prompts, approvals, and usage

Successful native result fields:

| Field | Observed value |
|---|---:|
| Wall duration | 106,364 ms |
| API duration | 103,178 ms |
| Final-result TTFT | 2,429 ms |
| Agent turns | 27 |
| User prompts | 1 |
| In-product approval prompts | 0 (`dontAsk`) |
| Permission denials | 2 |
| Input tokens | 218 |
| Cache creation input tokens | 87,906 |
| Cache read input tokens | 1,176,214 |
| Output tokens | 6,306 |
| Reported cost | $0.3258504 |
| Budget | $0.40 |

The cost landed at 81.5% of the cap for a one-line documentation proposal.
Claude's detailed telemetry is excellent governance UX; the consumption itself
is a warning that narration and broad inspection need stronger task-sensitive
economy.

## Kitsu observations

### Startup and authority UX

Kitsu's help is unusually candid. It explains that `--yes` suppresses
filesystem questions but grants nothing, that `--allow-read-write` is explicit
combined authority, that command approval is independent, and that
`--command-root` adds OS containment without approving argv. This is more
precise than Claude's interacting tool/mode flags. Exact JSON argv approvals
also avoid shell-pattern ambiguity.

The cost is invocation length: seven ordinary verification commands required
seven repeated JSON flags. A checked policy file, named task profile, or
`--allow-command-file` would keep the exactness while making a safe coding run
copyable and reviewable.

The read-only-HOME failure is a real automation rough edge. Baseline source
shows `runtime_state_base()` uses `$HOME/.kitsu` whenever HOME is non-empty and
`prepare_runtime_state_dir()` unconditionally creates/secures that directory.
The existing `test_empty_script_does_not_require_writable_home` covers only an
empty script. A real `chat --no-session` turn still requires writable runtime
state, despite explicitly disabling snapshots.

### Live request, cancellation, and repository state

Before any assistant delta, JSONL emitted:

```text
run_started (durable_session=false, mode=piped, exact gateway/model)
turn_started
provider_request (round=1, message_count=2, tools_offered=10)
```

After Ctrl+C it emitted, in order:

```text
provider_attempt (attempt=1, status=200, category/outcome=cancelled, terminal=true)
assistant_disposition (status=discarded; turn cancelled before commit)
error (kind=cancelled, exit_code=130)
turn_finished (status=cancelled)
final_summary (status=cancelled, cancelled_failures=1)
```

Process exit was 130. This is excellent machine-facing cancellation UX: the
physical request, provisional-answer disposition, turn, and run all reach an
unambiguous terminal state. Baseline test
`test_jsonl_sigint_emits_cancelled_turn_and_summary` asserts this exact event
order, and the live run matched it. Git remained clean, confirming no hidden
partial edit.

No assistant delta, tool request, usage event, or dollar cost was exposed before
cancellation. Consequently this lane cannot score Kitsu's tool narration,
edit precision, test execution, diff inspection, final answer, or model quality.

### Error and recovery quality

All three Kitsu failures were correctly nonzero and JSONL kept a terminal
`final_summary`. The missing-config and runtime-state messages named the failing
resource. Recovery could be more actionable:

- the runtime-state error did not suggest an isolated state directory, unset
  HOME, or a dedicated flag;
- `models` collapsed an unsupported/missing catalog endpoint and network failure
  into “could not reach the gateway,” even though completion could work; and
- there is no progress field giving elapsed time, remaining deadline, or first-
  byte state between `provider_request` and eventual output.

## Product strengths worth preserving

Claude Code:

- one discoverable automation command covered model, effort, permissions,
  session durability, spend, and structured output;
- visible intent narration and graceful recovery from a tool-use mistake;
- a hard dollar budget plus rich final token/cache/cost telemetry; and
- permission denials were retained in the final machine result.

Kitsu:

- filesystem authority, write authority, command availability, exact argv, and
  command roots are separate and accurately documented;
- `--no-session` was explicit in `run_started` and no durable snapshot appeared;
- JSONL errors and cancellation have typed, ordered, terminal events;
- Ctrl+C discarded the incomplete assistant turn and returned conventional exit
  130; and
- cancellation left the disposable repository exactly clean.

## Ranked Kitsu UX gaps and concrete regressions

| Rank | Gap | Concrete regression idea |
|---:|---|---|
| 1 | No provider-independent spend ceiling or dollar ledger. Token, time, output, and round caps do not answer “what can this run cost?” | Add a priced mock gateway/model and assert `--max-cost-usd` stops before the next physical send, exits with a distinct budget status, and emits cumulative token and dollar fields in `final_summary`. For unknown pricing, fail or visibly mark cost unknown according to an explicit policy. |
| 2 | `chat --no-session` still fails when HOME is present but read-only. | Extend the existing blocked-HOME test with one real mocked provider turn and `--no-session`; assert success using an explicit `--state-dir` or documented ephemeral fallback, no files under HOME, and no session snapshot. |
| 3 | No host-verified completion ledger prevents a model from calling unrun tests “verified.” | Mock an assistant that edits a file and claims tests passed without invoking `run_command`. Assert the terminal/JSONL summary says `files changed: 1`, `tests run: 0`, `diff check: not run`, and flags the unsupported verification claim without rewriting model prose. |
| 4 | Long provider silence has no periodic, structured progress beyond the initial `provider_request`. | With a server delaying first byte for 90 seconds, assert rate-limited progress events include elapsed time, configured deadline, attempt, and `waiting_for=first_byte`; human mode should update one compact status line. Cancellation must retain the current clean event sequence. |
| 5 | Exact command authority is safe but impractically verbose for normal edit/test/diff work. | Add an `--allow-command-file` fixture containing exact argv/cwd entries. Assert policy preview output is canonical, symlinks and duplicate/conflicting roots fail closed, and execution authority is byte-for-byte identical to repeated flags. |
| 6 | Model-catalog errors do not distinguish “endpoint unsupported” from unreachable gateway, while chat may still work. | Mock a working chat endpoint with disabled/404 models path. Assert `models` reports “catalog unavailable/unsupported; configured model is X,” preserves a typed category, and does not tell the user the gateway itself is unreachable. |
| 7 | `final_summary` lacks a compact operator ledger for duration, TTFT, rounds, tool successes/failures/denials, changed paths, test commands, usage, and cost. | Run a deterministic two-round edit/test mock and validate the schema and human footer against host-observed commands and Git state; assert secrets and full file contents cannot enter the ledger. |
| 8 | Effective policy is hard to inspect before spending a provider call. | Add `kitsu chat --explain-authority` (provider-free) and assert it lists read roots, write roots, command roots, exact approved argv, persistence mode, protected paths, and all limits; it must make no network request or state mutation. |

## Maintainer recommendation

Do not treat Claude's proposed README line as a ready improvement from this
run: the transient diff was not recoverable, no regression/test completed, and
the synopsis needs a small `--resume [ID]` accuracy review. Put that specific
patch in the documentation backlog unless another lane independently recreates
and verifies it.

For product implementation, prioritize ranked gaps 1–3. Cost governance and a
host-verified completion ledger directly address what made the completed Claude
run expensive and overconfident; stateless runtime-state handling removes a
Kitsu-specific automation failure observed before inference. Preserve Kitsu's
exact authority model and cancellation transaction semantics while improving
the surrounding ergonomics.
