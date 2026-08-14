# Codex/Claude UX cycle 14: recoverable `/clear`

## Challenge

Run three seeded, permission-prompt-free comparisons at UTC boundary
`1786683600` (cycle seed `992602`), then implement one bounded product
improvement. Hugging Face remained gated because `HF_TOKEN` was unavailable;
no remote Hugging Face request was sent. The model-driven locate leg used the
documented local Ollama fallback, while the recovery and lifecycle legs used
deterministic local mock providers.

The continuous-development profile exposed repository and private-fixture
reads/writes, local builds/tests/models, and agent orchestration without
approval dialogs. Scored Kitsu legs still received only their declared fixture
roots, exact commands, and `--yes`; none received arbitrary shell, unrestricted
host filesystem, credential, or new-network authority. This follows the
workspace-write/approval separation described in the
[official OpenAI approval and sandbox documentation](https://learn.chatgpt.com/docs/agent-approvals-security).
The standalone `codex` executable was unavailable, so controlled in-process
Codex comparators remain provisional.

The scored pre-fix binary was SHA-256
`9e7ca24e162e69a81eb47221cf5c68101a142ed8a0657b8385cefcb0085ebd40`.
An unscored tool-free warm-up immediately before the boundary returned the
exact marker in 429 ms with no file, session, or tool activity.

## Seeded comparison

| Task | Kitsu | Comparator | Attribution |
|---|---:|---:|---|
| Locate one exact row in a seeded 32-row catalog | 82/100 | Codex 93/100 provisional | The prompt's relative path resolved from different launch directories. Kitsu correctly denied the outside-root read, discarded the unsupported answer, and exited consistently; outcome miss is harness/infrastructure. |
| Recover from one stale exact-edit precondition | 96/100 | Codex 91/100 provisional | Kitsu exposed five fixture-scoped file tools, fed the failed call back, corrected it, and committed the exact oracle. `final_summary.tool_failures=0` hides the recovered tool error already present in `turn_finished.tool_errors=1`; product evidence gap. |
| Preserve durable history across `/clear` | 84/100 | controlled Codex oracle unscored | Kitsu cleared provider context correctly but overwrote the old transcript under the same ID, made it non-resumable, and emitted no typed preservation receipt; product safety/UX defect. |

The locate digest was
`faa1e316b624892572456114193a0905a5f36108446e11be819e5a75a6ed6488`.
Both 1,832-byte catalogs and the outside sentinel stayed byte-identical. Kitsu
offered exactly `fs_read_file`, `fs_list_dir`, and `repo_grep`; its one denied
read, discarded assistant disposition, policy failure, exit `1`, and final
summary agreed. The provider-attempt `remaining_deadline_ms` field reports the
nested per-request timer, so its round-to-round increase does not disprove the
outer scripted-turn deadline. The deterministic Cycle 13 regression still
proves two individually valid 700 ms rounds cannot exceed one 1-second turn
budget. Naming both deadline scopes in telemetry remains backlog.

The recovery digest was
`f4b750e758526d5477e8abfe37f0ce213d001a5dd500101a2b75cc7dbea3058a`.
Kitsu exited `0` in 41 ms with 19 valid JSONL events and exactly three provider
requests. Its first `fs_edit_file` used the seeded stale value and failed
without mutation; its second used the corrected value and made one
replacement; its committed answer was exactly `RECOVERED settings.ini`.
Kitsu and Codex final hashes both matched oracle
`28229578…ab6c`; the sentinel remained `0a9d88e3…c5fd`.

The lifecycle seed digest was `89f417ebe1559877…24e2a`. The one scored run
started at `1786683600.000275`, completed without a timeout, and retained the
same session ID before and after `/clear`. The old marker was present before
the command and absent afterward; the post-clear marker replaced it. The
second provider request correctly excluded the old marker, proving context
clearing itself worked. Raw JSONL exposed only generic `command_started` and
`command_finished(status=ok)` events—no prior/new IDs, removed counts, or
preservation warning. Exact evidence is retained at
`/tmp/kitsu-parity/992602/clear_durable/evidence/report.md`, SHA-256
`2fc25cd6415f516893e08375dc1c1ba6fa680540284da2d7facecd81febda530`.

No comparison disclosed a credential, requested an approval, changed an
outside sentinel, or left a live descendant.

## Selected improvement

`/clear` is now a recoverable session boundary instead of an in-place message
mutation:

1. refuse the boundary while a background agent is running and surface any
   completed, unobserved results;
2. save the current durable transcript under its existing ID;
3. acquire a fresh session lease before changing the in-memory identity;
4. keep the old snapshot and name intact while the fresh session starts
   unnamed, preserving name uniqueness;
5. clear conversation, agent, checkpoint, and context-meter state; reset
   runtime filesystem/command grants while restoring invocation pregrants;
6. rotate private artifact storage, and resolve its path dynamically for every
   later scripted turn so the new context cannot write into the old sidecar;
7. emit a visible human boundary or typed `clear_session_boundary` JSONL with
   prior/new IDs, removed turn/message counts, preservation status, retained
   invocation-root counts, artifact status, and the fresh snapshot's explicit
   `pending_until_first_success` state.

A save or fresh-lease failure leaves messages, active ID, name, grants, agents,
and artifact destination unchanged. Under `--no-session`, the same receipt
states plainly that in-memory history was not saved. The old durable ID remains
resumable; a successful post-clear turn publishes the fresh ID.

## Verification

The atomic reset unit regression injects a fresh-lease failure and proves the
old ID, name, snapshot, and lease remain usable; the next successful reset
changes ID and clears the new session's name. The end-to-end wiremock regression
runs `old marker -> /clear -> new marker` in one durable JSONL script and proves:

- the typed receipt contains two different IDs and exact counts;
- the final summary uses the new ID;
- session inventory contains two productive one-turn snapshots;
- the old transcript contains only the old marker and the new transcript only
  the new marker;
- the second provider request contains no old marker.

Verification completed without approval prompts:

- focused atomic reset and scripted clear regressions: 2 passed;
- complete CLI integration suite: 138 passed;
- complete workspace suite: 699 passed;
- `cargo fmt --all -- --check`;
- `cargo clippy --workspace --all-targets -- -D warnings`;
- staged and unstaged `git diff --check`;
- fresh debug build.

The post-fix debug binary is SHA-256
`8dfce0ef8d38f5ec2442858fa489dfc43154d3e77630026402cee8eea91e62a3`.

## Next backlog

1. Restore Hugging Face authentication through a user-controlled secret
   channel, pass `doctor --inference`, and run three real HF legs. Add a durable
   login/status/logout lifecycle so auth survives reboot without writing a
   token to repository config or logs.
2. Keep terminal-turn `tool_failures` backward compatible, but add aggregate
   `tool_errors` and `recovered_tool_errors` to `final_summary` so recovered
   failures do not disappear at run level.
3. Make parity prompts use absolute granted fixture paths, or launch every
   runner from the same fixture root, so relative-path resolution cannot skew
   outcomes.
4. Distinguish outer turn budget from nested provider-request budget in
   telemetry instead of presenting both as an unqualified deadline.
5. Generate every benchmark gateway config from a typed template before its
   boundary and add direct output-file/schema controls for stricter parity with
   Codex automation.
