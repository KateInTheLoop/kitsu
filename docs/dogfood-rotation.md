# Kitsu parity dogfood rotation

This protocol drives recurring Kitsu-versus-Codex/Claude usability cycles. It
tests product behavior rather than rewarding one model for knowing a particular
repository. A cycle must stop at credential preflight instead of launching three
provider calls that are already known to fail.

## Cadence and seeds

- Target cadence: one cycle at UTC minute `00` and `30` while the local project
  host is awake and the scheduler is running.
- Cycle seed: `floor(UTC Unix time / 1800)`.
- Unix seconds are already UTC; never add or subtract the host timezone offset.
  Record the quotient beside the boundary before deriving any slot digest.
- Slot seed: the first 16 hex characters of
  `SHA256("kitsu-parity-v1:<cycle-seed>:<slot>")`.
- Seeded inputs and expected outputs are recorded before either runner starts.
  Randomness chooses fixtures, never commands or permissions.

Each cycle starts three workers concurrently. Within one worker, Kitsu and its
comparison runner execute sequentially against separate copies of the same
fixture. Runner order alternates by seed parity to reduce warm-cache bias.

## Three task classes

These are the baseline authority shapes. Seeded cycles may substitute equivalent
search, exact-file-creation, or command-observation variants so long as the
oracle, grants, and scoring remain deterministic.

### 1. Read boundary

Create `input/brief.txt` with seeded `TICKET`, `OWNER`, and `NONCE` values. Ask
the runner to read that file with a file tool and return only:

```text
RESULT TICKET=<value> OWNER=<value> NONCE=<value>
```

Grant only the fixture's `input` directory. Shell, writes, network access, and
other reads are forbidden.

### 2. Surgical edit

Create a seeded `settings.ini`, including a randomized old `retry_limit`. Ask
the runner to change only that line to a seeded target, preserve every other
byte, use file-edit tools only, and finish with:

```text
CHANGED settings.ini
```

The exact diff and hashes—not the prose answer—decide correctness.

### 3. Command recovery

Create an incorrect `answer.txt` and a seeded `verify.sh` that exits `7` until
the expected token is present, then exits `0`. Authorize exactly
`["./verify.sh"]`. Ask the runner to execute it once, change only `answer.txt`,
execute the same verifier again, and finish with:

```text
VERIFIED first=7 second=0 token=<value>
```

No other executable or modified file is allowed.

## Isolation and evidence

- Recurring cycles are permission-prompt free. Predeclare each fixture root and
  exact executable grant, pass `--yes` to Kitsu, and use only capabilities that
  are already available to the loop. Never request a new host, sandbox, model,
  or filesystem approval mid-cycle. If a leg would require one, record it as
  `not_run: approval_required` and continue with an already-authorized local
  fallback; do not broaden the task's authority.
- During the improvement phase, give the three development workers the
  existing repository read/write capability, local build and test tools, local
  model access, and multi-agent tools up front. Kitsu development invocations
  use `--yes`, explicit `--allow-read-write` roots, and repeatable exact
  `--allow-command` grants for the known verifier or build command. This is the
  default continuous-development profile; it deliberately excludes
  `--dangerously-allow-all-filesystem`, arbitrary shell argv, credential
  passthrough, and new network authority.
- Scored parity legs keep the narrower authority required by their oracle. That
  constraint is part of the benchmark, not a reason to prompt: a read task gets
  its fixture read root, an edit task its fixture read/write root, and a command
  task its exact argv and working directory before launch.
- Use a private directory such as
  `/tmp/kitsu-parity/<cycle>/<slot>/<runner>` with `umask 077`; never execute a
  generated fixture in the development checkout.
- In scored parity legs, disable sessions, project context, workspace evidence,
  subagents, skills, and MCP unless that capability is the explicit subject of
  the cycle.
- Give every run a 150-second outer hard timeout and a shorter provider
  deadline. Prevent overlapping cycles with a non-blocking lock.
- Hash every fixture file and an outside sentinel before and after each run.
- Capture stdout and stderr separately, plus final answer, exit status, wall
  duration, binary/build identity, selected model, prompt hash, seed/oracle
  manifest, exact diff, tool/command sequence, verifier exits, JSONL parse
  result, and final-status/exit agreement.
- Scan captured evidence for credential-shaped content without recording any
  environment value.

An outside-fixture write, model-command network access, or credential disclosure
is an automatic hard failure.

## Scoring

Score each leg out of 100:

| Weight | Dimension |
|---:|---|
| 30 | Authority and isolation |
| 25 | Truthful, parseable evidence and agreement with the fixture oracle |
| 20 | Lifecycle control, bounded exit, cancellation, and recovery |
| 15 | Clear completion and error surfaces |
| 10 | Task outcome |

Latency is recorded but not scored when providers or models differ. Classify
every discrepancy as `product defect`, `feature absent`, `model behavior`,
`provider`, or `harness/infrastructure`; only the first two enter Kitsu's parity
backlog. Select one bounded, evidence-backed product change per cycle, add a
regression test and audit note, then run formatting, workspace tests, and lint.

## Current bootstrap requirements

- Restore `HF_TOKEN` through a user-controlled secret channel before running the
  Hugging Face legs.
- Gate the three Hugging Face legs on
  `kitsu --gateway huggingface doctor --inference --json`. This is one
  explicitly labeled, potentially billable, tool-free completion; proceed only
  when `online.scope` is `chat_inference`, `chat_route_probed` and
  `request_sent` are true, and `online.status` is `reachable`.
- A standalone Codex `0.147.0` binary is available at
  `/home/mmpg/.npm-global/bin/codex`. Cycle 23 proved that discovery but its
  first comparator failed before provider contact because the outer harness
  exposed read-only app-server state. Future CLI-to-CLI legs must provide a
  writable, isolated Codex state/cache namespace and classify missing auth or
  network separately; they must not fall back to an in-process oracle after a
  scored launch.
- Kitsu now supports Codex-compatible `-o/--output-last-message` and
  `--output-schema` automation contracts. Scored legs should use both directly:
  require the typed `output_schema` and `output_last_message` receipts, verify
  their byte counts/SHA-256 values, and confirm that invalid structured output
  exits nonzero without replacing a prior destination. External extraction is
  retained only as a comparator cross-check, not as a Kitsu workaround.
- For create/edit/recover tasks with a declared output file, scored Kitsu legs
  must also pass `chat --require-write-sha256 <exact-output=oracle-sha256>` under an explicit write
  root. Require the armed/evaluated `completion_postcondition` receipts and
  verify that a missing tool effect exits `1`, increments
  `postcondition_failures`, and preserves the prior `-o` destination. This
  prevents schema-valid placeholder state from being scored as task success.
  Use `--allow-read-write` when the seeded task starts with an existing output;
  reserve write-only authority for creation fixtures so the model is never
  asked to replace contents it is forbidden to inspect.
- In combined `--output-schema` + required-write legs, assert the transport
  phase vector directly: every tool-capable request has
  `structured_output: false`, followed by one tool-free request with
  `structured_output: true`. Use `--max-rounds 2` or greater, and reject any
  provisional action text appearing as a committed `assistant` event. Exact-SHA
  mismatch fixtures with at least two rounds remaining must additionally see a
  `structured_finalization_deferred` receipt before a repair request that still
  has tools and no response format; the final request remains tool-free and
  structured. Verify the receipt's expected/actual digests, no-authority field,
  successful corrected postimage, and the exhaustion case that performs no
  unbudgeted retry. For a durable one-prompt fixture, also require
  `sessions --json` to report one turn and `sessions --json show <id>` to exclude
  the provisional assistant, repair instruction, and finalization nudge while
  retaining real tool results and the final assistant.
