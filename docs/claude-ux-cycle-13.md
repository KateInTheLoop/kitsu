# Codex/Claude UX cycle 13: one deadline per scripted turn

## Challenge

Run three seeded, permission-prompt-free Kitsu comparisons at UTC seed
`992601`, then select one bounded product improvement. Hugging Face inference
remained gated because `HF_TOKEN` was unavailable; no remote Hugging Face
request was sent. The model-driven legs therefore used the documented local
Ollama fallback with `qwen2.5-coder`.

The loop used the continuous-development authority profile requested by the
user: repository and private-fixture reads/writes, local builds/tests/models,
and agent orchestration were available without approval dialogs. Kitsu parity
legs still received only their predeclared fixture roots, exact commands, and
`--yes`; no run received unrestricted host filesystem, arbitrary command,
credential, or new-network authority. This matches the safe Codex automation
shape described in the
[official approval and sandbox documentation](https://learn.chatgpt.com/docs/agent-approvals-security):
workspace-write confines mutation to the working area while a never-ask policy
suppresses approval prompts.

The standalone `codex` executable was not available on the ordinary host
command path, so the comparison side used controlled in-process Codex workers.
Those scores remain provisional.

## Seeded comparison

All fixtures and oracles were recorded before the boundary. The scored Kitsu
binary was SHA-256
`31911431db9aa945eed7e45e2413b408881b3e537d9544ad3be8be3a7ba1c373`.

| Task | Kitsu | Comparator | Attribution |
|---|---:|---:|---|
| Assemble two seeded fragments into one exact file | 90/100 | Codex 93/100 provisional | Kitsu exposed exactly five fixture-scoped file tools and emitted truthful timeout evidence, but local qwen returned no first response within 45 seconds. Provider/model outcome miss. |
| Recover an incorrect answer with one exact verifier | 90/100 | Codex 91/100 provisional | Kitsu exposed exactly six in-scope tools, timed out before a tool call, and left every fixture hash unchanged. The Codex runner completed the `14 -> 0` verifier sequence but had broader ambient authority. |
| Preserve a durable session across `/clear` | N/A | Claude fallback unscored | A stale benchmark config contained the unrelated key `retry_limit`. Kitsu correctly rejected it before session/provider access; this is harness/config failure, not a parity result. |

The assemble digest was
`27e65e2def7a1bc049d17967d07e44cb38bffe052fdcceda20df1e9edde138ac`;
its expected output hash was
`845cea7c1b8e1bb579d2b47f47249666a5bee4f292c5414cd4878abab98144b7`.
Kitsu exited `2` after `45,009 ms`, made no tool call or write, and agreed across
its discarded assistant disposition, provider error, final summary, and
process status. The Codex fixture contained exactly the expected 74-byte file.

The recovery digest was
`bdc2a5010675c1758061aef758434f5a639955f42512a4e416728eaac57a4598`;
the seeded verifier's initial exit was `14`. Kitsu exited `2` after `45,018 ms`
with eight parseable JSONL records and no tool or command event. The Codex
fixture alone changed `answer.txt`, then the exact verifier exited zero.
Verifier and sentinel hashes stayed unchanged in both fixtures.

The lifecycle digest was
`ed45021c1cc3a34203a1113096f2194669d96cc38bd577f977b1855e6378a532`.
Kitsu rejected the unknown gateway key in 16 ms with exit `1`; the mock received
no request, its runtime home/export directories remained empty, its sentinel
was unchanged, and the child was reaped. The exact evidence is retained at
`/tmp/kitsu-parity/992601/clear/evidence/report.md` with report SHA-256
`3874e60a6bb85c8a547e2f742e1d4152f0a4761a48d10ef81cb8477e6d80d9f5`.
The error path scored 90/100 diagnostically, but it is not a parity score and
was not rerun.

No comparison disclosed a credential, wrote outside its fixture, requested an
approval, or left a live descendant.

## Selected improvement

Cycle 12 captured a scripted turn whose first slow provider round completed,
ran a tool, and then received a fresh `--timeout-secs` allowance for its second
provider request. A nominal 45-second turn could therefore consume 45 seconds
per round. Cycle 13's local provider timed out before its first response, so it
did not independently exercise that defect; the change is attributed to the
preserved Cycle 12 evidence and a deterministic regression, not to the two
Cycle 13 outcome misses.

Every scripted model turn now has one outer inference deadline across all of
its provider and tool rounds. When the budget expires, Kitsu drops the active
future, waits for already-entered journal mutations, cancels descendants using
the existing bounded cleanup path, rolls conversation state back, marks any
assistant output discarded, emits the typed provider timeout, and preserves
normal final-summary/exit agreement. A tool call no longer resets the clock for
the follow-up request.

CLI help, the README, and the functionality contract now describe that same
per-turn deadline.

## Verification

The new wiremock regression gives a scripted turn a one-second budget. Its
first response arrives after 700 ms and requests `fs_read_file`; that tool must
finish successfully. The second provider request also has a 700 ms response
delay. Both individual requests fit a one-second per-request timeout, but their
combined 1.4 seconds do not fit the turn budget. Kitsu now launches the second
request, exits `2` at the original deadline, emits a discarded disposition and
typed provider timeout, and ends with `provider_failures: 1`.

Verification completed without approval prompts:

- focused deadline regression: 1 passed;
- complete CLI integration suite: 137 passed;
- complete workspace suite: 697 passed;
- `cargo fmt --all -- --check`;
- `cargo clippy --workspace --all-targets -- -D warnings`;
- staged and unstaged `git diff --check`.

The post-fix debug binary is SHA-256
`9e7ca24e162e69a81eb47221cf5c68101a142ed8a0657b8385cefcb0085ebd40`.

## Next backlog

1. Restore Hugging Face authentication through a user-controlled secret
   channel, pass `doctor --inference`, and run three real HF legs. Add a durable
   login/status/logout lifecycle so authentication survives a reboot without
   copying a token into logs or configuration.
2. Make `/clear` preserve the old durable snapshot and atomically rotate to a
   new session ID before clearing context. The source audit reconfirmed that
   both current drivers otherwise save the shortened transcript under the same
   ID and destroy resumable history.
3. Count individual failed tool calls in `final_summary`; a later provider
   timeout currently leaves consumers to reduce the full event stream.
4. Generate or validate each benchmark config from a typed template before the
   boundary so stale task-fixture keys cannot invalidate a scored leg.
5. Add an unscored local-model readiness/warm-up probe before scheduled Ollama
   legs so cold startup does not consume the whole scored turn.
