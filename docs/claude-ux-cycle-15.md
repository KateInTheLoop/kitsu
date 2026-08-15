# Codex/Claude UX cycle 15: recovered tool-error accounting

## Challenge

Run three seeded, permission-prompt-free comparisons at UTC boundary
`1786685400` (cycle seed `992603`), then implement one bounded product
improvement. Hugging Face remained gated because `HF_TOKEN` was unavailable;
no remote Hugging Face request was attempted and no credential value was
inspected or logged. The model-driven create leg used local Ollama, while the
policy and session-lifecycle legs used deterministic local mock providers.

The continuous-development profile exposed repository and private-fixture
reads/writes, local builds/tests/models, and agent orchestration without
approval dialogs. Each scored Kitsu run still received only its declared
fixture roots, exact tools, and `--yes`; none received arbitrary shell,
unrestricted host filesystem, credential, or new-network authority. This
follows the workspace-write/approval separation described in the
[official OpenAI approval and sandbox documentation](https://learn.chatgpt.com/docs/agent-approvals-security).
The standalone `codex` executable was unavailable, so controlled in-process
Codex comparators remain provisional.

The scored pre-fix binary was SHA-256
`8dfce0ef8d38f5ec2442858fa489dfc43154d3e77630026402cee8eea91e62a3`.
Two unscored, tool-free local warm-ups returned the exact marker in 4.017
seconds and 448 ms, with no file, session, or tool activity.

## Seeded comparison

| Task | Kitsu | Comparator | Attribution |
|---|---:|---:|---|
| Create one exact JSON manifest from a seven-field blueprint | 95/100 | Codex 93/100 provisional | Both produced the exact 128-byte oracle and touched only the declared output. Kitsu's provider-request timer increased across rounds, but the run remained inside the separately enforced single scripted-turn deadline; naming these two scopes remains a telemetry-clarity backlog item, not a reproduced deadline escape. |
| Deny an outside read, then inspect allowed evidence | 100/100 | Codex 91/100 provisional | Kitsu exposed the denial, completed the allowed read, discarded the otherwise exact final answer, and exited `1` because a policy denial is intentionally terminal. No product defect. |
| Re-test named durable `/clear` after Cycle 14 | 99/100 | controlled Codex oracle unscored | Old and new IDs, name uniqueness, context isolation, typed receipt, inventory, and resumability all passed. The remaining minor gap is that `final_summary` does not repeat the terminal session snapshot state. |

The create digest was
`bc018d8fa7176127763d0c77a794a094871dc4bd479088679fb2e0091a63e67f`.
The single Kitsu run started after its hash gate, exited `0` in 56.256 seconds,
and exposed exactly `fs_read_file`, `fs_list_dir`, `fs_write_file`, and
`fs_edit_file`. It read the absolute blueprint once, created only
`output/manifest.json`, and returned exactly
`CREATED output/manifest.json`. Kitsu and Codex outputs both matched oracle
SHA-256 `d861c19a6f6c2a5b8b22ae64091a38046b1a9a137ff7853ca5f0e67b170996d4`;
both input hashes and outside sentinel
`d9443d7a91bce4116975769ecda50b7d68bc209deda8d4c1135dcb247d3d320f`
remained unchanged. Kitsu created the private fixture output as mode `0600`.

The policy digest was
`8be7859374ebeb9c6ce82abbd976cb5df7208ea2b2e5a0beab4257a82a28a9df`.
Kitsu exited `1` in 25 ms with 20 parseable JSONL events and exactly three
provider requests. Its forbidden read returned `PolicyDenied` without content;
the allowed read returned the exact record; the final text was emitted and then
explicitly discarded. The turn reported `tool_errors=1` and
`policy_errors=1`; the old final summary truthfully classified one terminal
policy failure but exposed no run-level individual-error total. All fixture,
decoy, prompt, config, mock, binary, and sentinel hashes remained unchanged.

The named-clear run began at `1786685400.000220` and completed in 53.7 ms. Its
old and new IDs differed, the old name resolved uniquely to the preserved
transcript, the fresh session was unnamed, the second request excluded the old
marker, and inventory contained exactly two one-turn sessions. The final
summary used the new ID. Exact evidence is retained at
`/tmp/kitsu-parity/992603/clear_named/evidence/report.md`, SHA-256
`53b83c980f9eecd6e4b31ff231be681b109ba27ed8d9048d0707f8fefc8ad423`.

No comparison requested an approval, disclosed a credential, changed an
outside sentinel, reran a scored leg, or left a live descendant.

## Selected improvement

Cycle 14 reproduced an exact edit recovery where `turn_finished.tool_errors=1`
but `final_summary.tool_failures=0`, forcing automation to reduce every event
to learn that a call failed and was repaired. Cycle 15's policy leg confirmed
the inverse case: individual and terminal failure counts are different useful
facts. Kitsu now preserves both without changing the existing outcome contract.

Every new `final_summary` includes four additive counters:

- `tool_errors`: individual foreground tool-call errors across attempted turns,
  including policy-denied calls;
- `policy_errors`: the authority-denial subset of `tool_errors`;
- `recovered_tool_errors`: those errors belonging to turns that ultimately
  committed;
- `recovered_policy_errors`: the corresponding policy subset, currently always
  zero because policy denial remains terminal.

The existing `failures`, `tool_failures`, and `policy_failures` fields retain
their exact meaning and compatibility: they count failed requested operations
or turns, with the existing exclusive classification. `--allow-turn-errors`
does not relabel a failed turn as recovered.

A saturating `u64` `ToolErrorTotals` ledger aggregates model-turn status for
scripted, piped, and `ask` runs. Scripted turns record their counters inside the
turn boundary before failed state is rolled back, so `/retry`, strict failure,
provider failure, and cancellation cannot erase evidence. Only a strict-success
turn that receives a committed assistant disposition contributes to recovered
totals. Background-agent internals are not silently folded into the foreground
ledger.

New producers always emit all four fields. They are properties but not required
fields in the published `kitsu.chat.v1` schema, so historical v1 streams remain
valid and consumers can treat absent values as zero. README, functionality, and
protocol documentation distinguish failed-operation counts from individual
error counts and define recovery by commit disposition.

## Verification

The recovered-call integration regression now exercises both scripted and piped
JSONL. In each mode it proves a malformed read followed by a corrected read
commits with `failures=0`, `tool_failures=0`, `tool_errors=1`, and
`recovered_tool_errors=1`. The strict tool/policy regression proves failed turns
retain raw counts with both recovered counters at zero. Provider-failure tests
prove `ask` and piped summaries emit explicit zero values when no tool call ran.
A unit regression proves raw/recovered multi-turn ledger aggregation, and the
published-schema regression parses the additive schema.

Verification completed without approval prompts:

- focused recovered, strict-failure, ask/piped provider, ledger, and schema
  regressions: passed;
- complete CLI integration suite: 138 passed;
- complete workspace suite: 700 passed;
- `cargo fmt --all -- --check`;
- `cargo clippy --workspace --all-targets -- -D warnings`;
- staged and unstaged `git diff --check`;
- fresh debug build.

The post-fix debug binary is SHA-256
`91731257818adb5403be479af79fc0bb7ca128b0ac9b8e7d27b5469b1e2e5b11`.

## Next backlog

1. Restore Hugging Face authentication through a user-controlled secret
   channel, pass `doctor --inference`, and run three real HF legs. Add a durable
   login/status/logout lifecycle so auth survives reboot without repository
   secrets or log exposure.
2. Add terminal `session_snapshot_state` to `final_summary`, so a consumer can
   distinguish pending, resumable, and stateless completion without a second
   session query.
3. Rename provider-attempt `remaining_deadline_ms` to identify it as the nested
   request budget, or add a separate outer-turn remaining budget, so increasing
   per-round values cannot be mistaken for a reset of the enforced turn bound.
4. Add a mixed ordinary-tool-plus-policy regression and a multi-turn
   `--allow-turn-errors`/`/retry` regression for the new ledger invariants.
5. Generate every benchmark gateway config from a typed template and keep
   equivalent absolute fixture roots for all runners.
