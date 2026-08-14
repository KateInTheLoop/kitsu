# Codex/Claude UX cycle 20: auditable top-level review disclosure

## Challenge

Run three seeded, permission-prompt-free comparisons at UTC boundary
`1786716000` (cycle seed `992608`), recheck hosted-provider readiness, exercise
the new native content-search surface, and compare Kitsu's top-level verified
review framing with an automation-oriented control.

The scored pre-fix binary was SHA-256
`eb3145510fec2fa93d842715810c5839e92a7919606e705e8fecacc86424d42e`.
Every eligible Kitsu launch used a private pin plus a launch-time shared-binary
hash gate, one attempt, exact least-authority roots, and hard process bounds.
No comparison requested permission expansion, exposed credentials, staged
shared-tree changes, or left a child running.

## Seeded comparison

| Task | Kitsu | Comparator | Attribution |
|---|---:|---:|---|
| Run a randomly selected staged-only verified review and audit exactly what entered the provider prompt | 95/100 | deterministic Codex/Claude-style control, 100/100 provisional | Kitsu used the exact private-filtered 273-byte diff, granted no read root, exposed no tools, and returned the exact oracle. Its JSONL stream lacked a typed top-level disclosure receipt, forcing automation to parse the raw provider prompt. |
| Recheck missing-auth fail-closed behavior and managed local catalog/inference flows | 100/100 local fallback | Claude auth surface; official Codex contract | Process and user-service Hugging Face credentials were absent, so hosted inference was correctly not run. Missing auth failed before `run_started`; the managed local flow authenticated exactly one catalog request and one minimal tool-free inference request with no credential leakage. |
| Re-run the native multi-file content-search parity oracle without `rg` | not scored | Codex 95/100 provisional | The comparator matched all four rows. The Kitsu executor remained in its launcher/authorization layer, produced no capture, and was aborted after 564.8 seconds. Kitsu launch could not be established, so this is a harness/infrastructure failure and supports no product or model inference. |

The review digest was
`c6a5616f272cc2bb135495246fb375ef7a849070d326084c91190f700608438f`.
It deterministically selected the staged scope. The one scored Kitsu process
launched at `1786716000.251280`, exited zero after 15.383 ms, and emitted a
committed, tool-free JSONL lifecycle with all failure counters zero. Its
host-verified staged evidence was 273 bytes with SHA-256
`4c5f3b495317f44407924c03f2871b4a7cac552f52c458d8956fb3e761c21df8`;
the provider prompt contained those bytes exactly and omitted active Kitsu
configuration, unstaged, untracked, and outside markers. The answer matched the
oracle SHA-256
`2e76647527233eb26541e4b8edd6fb0d83a6815e40b215ce888ce474cd815a03`.
Before this cycle's fix, the event order jumped directly from `run_started` to
`turn_started`; no typed disclosure frame existed.

The Hugging Face digest was
`7ffd275c27ec8d73e599d5fee79670fd585faad646009e6b8d33508ff6d4e2de`.
Both credential-presence gates were false, so real hosted inference was
ineligible and no remote request was sent. The missing-auth prompt emitted only
typed error and terminal-summary frames, exited one, and did not reach the
mock. A managed local lifecycle then matched all 11 oracle assertions: exact
authenticated model-catalog and minimal tool-free chat requests, environment
precedence, malformed-environment fail-closed behavior, protected `0700`/`0600`
storage, and complete logout. All 15 children and the mock were reaped. The
local fallback scored 100/100; hosted Hugging Face remains an infrastructure
gate until a user-controlled credential is present.

The search digest was
`993bdc8d9c3023f8bd40b7d1e05955eb9400a731d88bba03a4c48c2c5362b661`.
The Codex control used one fixed-string `/usr/bin/grep` call over five files,
surfaced four matching rows, and returned the exact oracle SHA-256
`0c675866b73593e4562c0af59390a47e9857a648fcb0697f31b7e531601037ef`.
The intended Kitsu process never produced stdout or stderr capture files and no
JSONL lifecycle exists. It was therefore recorded as `not_scored`, not as a
zero-round or failed Kitsu run. Twin fixtures, all hashes, and the outside
sentinel remained unchanged; process checks confirmed no Kitsu child remained.

## Selected improvement

Top-level `kitsu review --jsonl` now emits exactly one typed
`review_disclosure` after `run_started` and before `turn_started`. The event
records `source: "top_level_review"`, the resolved review target and revision
metadata, truncation state, exact disclosed byte count, and the invariant facts
that evidence entered the model prompt while no model read root was granted.
It deliberately does not repeat the diff body.

The implementation reuses the same disclosure-field builder as scripted
`/review` and model-callable workspace evidence. This keeps the three surfaces
shape-compatible while preserving their distinct source labels and framing:
top-level review is process-scoped, scripted review remains command-scoped, and
workspace evidence remains nested between its tool frames. Human review output
is unchanged.

The published `kitsu.chat.v1` schema includes the new known event and excludes
it from the forward-compatible fallback branch. This is additive: historical
streams remain valid, and existing consumers that ignore unknown v1 events can
continue to do so.

## Verification

New regressions prove:

- top-level review emits exactly one disclosure event before the turn and
  provider request;
- its target, nullable revision fields, truncation flag, disclosure booleans,
  and byte count are exact;
- the byte count equals the actual diff code block in the provider prompt;
- no filesystem tools or read roots are granted; and
- scripted review, top-level review, and workspace evidence retain the same
  auditable metadata shape with distinct source labels.

Verification completed without permission prompts:

- focused top-level review CLI regression: passed;
- shared disclosure-shape unit regression: passed;
- published JSON schema regression: passed;
- complete workspace suite: 725 passed;
- `cargo fmt --all -- --check`;
- JSON schema parse validation;
- `cargo clippy --workspace --all-targets -- -D warnings`;
- staged and unstaged `git diff --check`; and
- fresh debug build.

The post-fix debug binary is SHA-256
`900777005c54d69c30a597e360c132c77acaf9d0518f2cd31ebcc7635557e88d`.

## Next backlog

1. Re-run the isolated no-`rg` multi-file search oracle against the post-fix
   binary with a launcher that produces an explicit pre-exec receipt, while
   preserving the one-attempt rule.
2. Add an automation-safe `-o`/`--output-last-message` surface that atomically
   records only the last committed assistant answer and emits a typed JSONL
   receipt, matching the useful Codex non-interactive workflow without giving
   the model authority over the destination.
3. Run real Hugging Face inference when a user-controlled credential becomes
   present; continue to fail closed without prompting or discovering one.
4. Consider an OS-keyring/automatic credential backend while retaining the
   atomic `0600` file fallback and secret-free receipts.
5. Add strict stateless artifact suppression and explicit timer-scope labels.
