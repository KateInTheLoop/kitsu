# Codex/Claude UX cycle 16: incomplete final-response detection

## Challenge

Run three seeded, permission-prompt-free comparisons at UTC boundary
`1786687200` (cycle seed `992604`), validate Cycle 15's recovered-error
accounting, exercise Hugging Face authentication readiness, and test a command
recovery task before implementing one bounded product improvement.

The continuous-development profile allowed repository and private-fixture
reads/writes, local builds/tests/models, and agent orchestration without
approval dialogs. Each scored Kitsu run still received only its declared roots,
commands, and tools. No run received arbitrary shell, unrestricted host
filesystem, credential, or new-network authority. This keeps automation
prompt-free while preserving the workspace-write/approval separation described
in the
[official OpenAI approval and sandbox documentation](https://learn.chatgpt.com/docs/agent-approvals-security).

The scored pre-fix binary was SHA-256
`91731257818adb5403be479af79fc0bb7ca128b0ac9b8e7d27b5469b1e2e5b11`.
Every scored launch passed an immediate hash gate, ran once, used a hard outer
bound, and was reaped without a permission request or fallback rerun.

## Seeded comparison

| Task | Kitsu | Comparator | Attribution |
|---|---:|---:|---|
| Aggregate one recovered tool error and one terminal policy error | 100/100 | deterministic oracle | Kitsu emitted the exact raw, recovered, and legacy failure totals; committed only the successful first turn; rolled back the denied second turn; and kept the durable snapshot isolated. Cycle 15's ledger fix is verified. |
| Inspect Hugging Face authentication readiness and persistence | 80/100 | Claude 95/100 | Kitsu's diagnostics were truthful and secret-safe, but it only supports environment/static authentication. Claude exposes login, status, and logout, while Kitsu has no durable credential lifecycle and therefore cannot recover a shell-only token after reboot. |
| Recover from a failing verifier, repair one file, and verify again | 22/100 | Codex 95/100 provisional | Codex produced verifier exits `[7, 0]` and the exact oracle. Kitsu observed exit `7`, failed an overwrite, read the old answer, then committed a blank terminal response with exit `0` without repairing the file or rerunning the verifier. This is a product correctness defect, not a harness failure. |

The error-ledger fixture digest was
`072c54169be25c722658a49941e3b07d6b6885cdad357473b7fedd4285521dc1`.
The run started at `1786687200.000169`, exited `0` in 25 ms, and emitted 32
valid `kitsu.chat.v1` events. Its exact terminal totals were
`status=partial`, `failures=1`, `provider_failures=0`, `tool_failures=0`,
`policy_failures=1`, `tool_errors=2`, `policy_errors=1`,
`recovered_tool_errors=1`, and `recovered_policy_errors=0`. The first turn
repaired an invalid tool call and committed; the second encountered a policy
denial and was discarded. The durable session contained exactly the first
answer and neither the second marker nor answer. Evidence is retained at
`/tmp/kitsu-parity/992604/error_ledger/evidence/summary.json`.

The Hugging Face readiness digest was
`34a10579e65e4f89a9c69ff77ea7d19196d1611e75f8086e4736c598eace30fa`.
`HF_TOKEN` was absent from the process, and the user-service environment was
checked only through an already approved secret-safe projection. Offline
doctor, inference doctor, and the one approved Hugging Face command all stopped
at the local missing-credential preflight; `request_sent=false` and the mock
request count stayed zero. No credential value, length, or hash was printed.
Claude 2.1.232 exposed `auth login`, `auth status`, and `auth logout`, and its
projected status reported an active login. A standalone `codex` executable was
unavailable.

The command-recovery digest was
`2b9217a8cd10e5170a25a8fdd3a49a5089065bf1fc0d898c423c4f755baa2f41`.
Kitsu's one run exited `0` in 78.433 seconds after the verifier failed once,
one file overwrite conflicted, and a read succeeded. It never edited
`answer.txt` or issued the required second verifier call. Nevertheless, its
terminal JSONL claimed `status=ok`, `failures=0`, `tool_errors=1`, and
`recovered_tool_errors=1` because the provider returned an empty final assistant
message. The external oracle remained failed. The controlled in-process Codex
leg changed only `answer.txt`, produced verifier exits `[7, 0]`, and matched the
expected completion; its score remains provisional because no standalone Codex
CLI was installed.

Kitsu also retained command evidence under its private `~/.kitsu` runtime
state. That path is host-owned, private, and not selectable by the model, so it
was not an authority escape. It did violate this benchmark's literal
no-outside-fixture-write rule and exposes a separate stateless-mode gap. The
apparently increasing `remaining_deadline_ms` values were per-provider-request
budgets nested inside an independently enforced whole-turn timeout; the issue
there is scope naming, not a reproduced deadline escape.

## Selected improvement

Kitsu no longer treats a terminal empty or whitespace-only assistant response
as successful completion. In strict `ask`, review, piped-chat, and scripted
surfaces, it is now an incomplete model turn:

- exit code `2`;
- JSONL `error.kind="incomplete_turn"`;
- `assistant_disposition.status="discarded"` with reason
  `model returned no final assistant text`;
- `turn_finished.status="error"` and one provider failure in the final summary;
- no blank assistant message or failed conversational turn persisted.

The existing failure priority remains intact: policy denial, an unrecovered
tool error, and length/max-round termination take precedence over blank-output
classification. Earlier tool errors in an incomplete turn remain visible in
the raw ledger but are not marked recovered, because the turn did not commit.
`--allow-turn-errors` can make a mixed script partial, but it cannot make an
all-blank script successful.

Interactive chat applies the same final-output predicate without adopting the
strict automation policy for other error classes. A blank final response now
rolls back conversational state, reports that any already completed filesystem
effects remain inspectable, cancels/reports child work, avoids autosaving the
blank response, and retains the prompt for `/retry`. The terminal message
explicitly says the turn was rolled back and names `/retry`.

Empty assistant content on an intermediate provider response that contains
tool calls remains valid. Validation occurs only after the agent loop returns a
terminal assistant result. Successful tool side effects also remain real and
visible if the later terminal response is incomplete; Kitsu rolls back the
conversation, not external filesystem history.

Retry guidance uses the configured output cap when available, suggesting twice
the current value or higher, and otherwise recommends an explicit larger cap,
a revised prompt, or a model that produces visible output. README,
functionality, and JSONL protocol documentation now state these semantics. The
v1 schema remains unchanged because provisional/intermediate assistant payloads
must still permit an empty string.

## Verification

New deterministic regressions prove:

- a malformed tool call followed by a successful read and whitespace-only
  final response exits `2`, discards the assistant, records one raw and zero
  recovered tool errors, and reports `incomplete_turn`;
- empty final responses have the same typed failure contract in `ask` and
  piped chat;
- interactive plain-terminal mode rolls back, prints `/retry` guidance, keeps
  retry state, and creates no durable snapshot;
- whitespace is rejected by the shared strict-failure predicate while a
  non-empty response still commits a recovered tool error.

Verification completed without approval prompts:

- all focused blank-response and existing recovered-error regressions: passed;
- complete CLI integration suite: 141 passed;
- complete workspace suite: 703 passed;
- `cargo fmt --all -- --check`;
- `cargo clippy --workspace --all-targets -- -D warnings`;
- staged and unstaged `git diff --check`;
- fresh debug build.

The post-fix debug binary is SHA-256
`c5221de98e006ab4dc059bb6d7e0208250b0ddaa88909982a55e62b08284e2ad`.

## Next backlog

1. Add a safe durable `kitsu auth login|status|logout` lifecycle for Hugging
   Face, with environment-over-store precedence, no token on argv, no-echo TTY
   or `--token-stdin` input, private atomic storage, redacted status, and
   provider-free regressions. Then pass `doctor --inference` and run three real
   Hugging Face legs when a user-controlled token is available.
2. Add `--no-artifacts` or a strict stateless mode that suppresses private
   command-artifact persistence while retaining truthful bounded inline
   evidence.
3. Add an explicit timer scope to provider-request deadline telemetry, or emit
   both provider-attempt and outer-turn remaining budgets.
4. Add terminal `session_snapshot_state` to `final_summary` so automation can
   distinguish pending, resumable, and disabled persistence without a second
   query.
5. Extend incomplete-output coverage with mixed success/blank
   `--allow-turn-errors`, policy-plus-blank priority, successful write plus blank
   final output, and an interactive `/retry` completion round trip.
