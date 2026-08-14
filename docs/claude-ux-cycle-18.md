# Codex/Claude UX cycle 18: truthful terminal session receipts

## Challenge

Run three seeded, permission-prompt-free comparisons at UTC boundary
`1786690800` (cycle seed `992606`), validate Cycle 17's managed-auth lifecycle,
retest scoped content-search parity, and distinguish planned from materialized
durable session IDs before implementing one bounded product improvement.

The scored pre-fix binary was SHA-256
`3f6bb95dfd8bbca67ba7c6c1cee76c4b6900694c20109197f3de1b6f22399cec`.
Every scored launch used a private pin plus a launch-time shared-binary hash
gate, one attempt, explicit least-authority roots, and hard process bounds. No
benchmark requested a permission expansion, exposed a credential, or mutated
the shared tree.

## Seeded comparison

| Task | Kitsu | Comparator | Attribution |
|---|---:|---:|---|
| Exercise managed login, status, local validation, environment precedence, and logout | 100/100 | Claude surface check; official Codex contract | Kitsu matched the eight-step oracle, protected and removed the managed token correctly, authenticated exactly one loopback request, and emitted no secret-derived evidence. A real Hugging Face credential was absent, so no remote request was made. |
| Locate four exact records across five read-only files | 90/100 | Codex 95/100 provisional | Kitsu listed and read every file, then hit its real 90-second turn deadline before final output. Codex used one bounded text search and returned the exact oracle. The repeated product gap is the absence of a root-confined content-search tool for ordinary read roots. |
| Distinguish a failed fresh session from a failed resumed session | not scored | deterministic oracle | The first mock request reached the harness, but `/tmp` filled before its receipt could be recorded or answered. The run was invalidated as infrastructure, not scored or rerun. Existing evidence and local regressions still supported the selected fix. |

The managed-auth digest was
`b333a20548c3195807053061f4eff96753ab333567c7e54ed1f571f9077e4b09`.
The one scored lifecycle started at `1786690859`, completed in 3.497 seconds,
and matched all eight expected results: managed login, managed status, one
loopback inference doctor request, valid environment override, empty-variable
fallback to managed storage, malformed-variable fail-closed behavior, logout,
and missing final status. The credential directory was owner-only mode `0700`;
its sole regular file was owner-only mode `0600`, single-link, and not a
symlink. Logout left the directory empty. All 11 children and the mock were
reaped. The process and user-service `HF_TOKEN` presence gates were both false,
so `request_sent=false` for real Hugging Face and no permission prompt was
issued.

Claude exposed login, status, and logout with parseable status JSON. A local
standalone Codex executable remained unavailable; the
[official OpenAI authentication documentation](https://developers.openai.com/codex/auth/)
describes login, status, logout, stdin credential flows, and file/keyring/auto
storage. OS-keyring or provider-native browser/device login remains optional
future parity work, not a defect in the bounded managed-file contract shipped
in Cycle 17.

The search digest was
`2cd34c806b4cfbd84e45a322181a57b3a0d2cce69556aba479caa49336005bbc`.
The fixture contained five files, 80 lines, and 3,279 bytes. Kitsu offered only
`fs_read_file` and `fs_list_dir`, listed all files, read all five in full, and
entered a third provider round before the enforced whole-turn deadline ended
the run at 90.016 seconds. Exit `2`, discarded disposition,
`provider_failures=1`, hashes, inventory, and cleanup all agreed. Codex used one
literal `grep`, surfaced only the four matching rows, and returned the exact
oracle. Its score is provisional because it was an embedded control rather
than a standalone Codex CLI run.

The session-receipt digest was
`29cbb164d67a692159f58a68f3ec8b6f437437db70e9097ffa94973884a76b8f`.
The wrapper launched at `1786690800.000611` with both binary hashes correct,
but the first mock receipt append failed with `ENOSPC`; the mock could not
return its whitespace oracle and the resumed case never ran. No rerun occurred.
The preseeded session and outside sentinel remained hash-identical, no fresh
JSON snapshot appeared, and every child was reaped. This is solely harness
infrastructure. A disposable isolated Cargo target had filled the 5.7 GiB
`/tmp` filesystem while linking the workspace; it was removed afterward, and
subsequent verification used the normal bounded build target.

## Selected improvement

`final_summary` now carries an additive `session_snapshot_state` with one of:

- `disabled` when continuity is off;
- `pending_until_first_success` when a fresh durable ID is planned but no
  snapshot has been installed;
- `resumable` when the active ID resolves to an installed snapshot.

The terminal `session_id` is non-null only in the `resumable` state. A failed,
empty, or query-only fresh run therefore cannot direct automation to a file
that does not exist. Its planned ID remains in `run_started` for correlation,
paired with the pending state. A failed turn after `--resume` retains the
existing ID and `resumable` state because the earlier durable transcript still
exists.

Kitsu keeps the planned ID and state in one locked terminal-receipt value so
the fields cannot drift. New durable sessions initialize pending; resumed
sessions initialize resumable; successful atomic snapshot installation
promotes the receipt to resumable; and `/clear` rotates it back to pending for
the new context. A pre-install save failure leaves a fresh receipt pending, or
preserves an already resumed receipt. If the session API reports that a
snapshot was installed before a later durability/cleanup error, the receipt is
truthfully resumable even though the requested operation still fails.

The JSON Schema declares the new state and nullable ID but does not add either
field to the v1 required list. Historical `kitsu.chat.v1` summaries therefore
remain valid, and older consumers may ignore the additive fields. README,
functionality, and protocol documentation now state the materialization
contract explicitly.

## Verification

New and extended regressions prove:

- empty and query-only fresh durable scripts emit a pending state, null terminal
  ID, and no session inventory entry;
- a failed first model turn emits the same receipt and cannot create a snapshot;
- a failed resumed turn retains its exact resolvable ID and unchanged saved
  transcript;
- a successful post-`/clear` turn promotes the new ID to resumable;
- a pre-install autosave failure remains pending/null with an empty inventory;
- disabled/parse-error summaries emit null plus `disabled`;
- the new v1 schema fields are typed but optional for historical compatibility.

Verification completed without benchmark permission prompts:

- complete CLI integration suite: 150 passed;
- complete workspace suite: 716 passed;
- `cargo fmt --all -- --check`;
- `cargo clippy --workspace --all-targets -- -D warnings`;
- staged and unstaged `git diff --check`;
- fresh debug build.

The post-fix debug binary is SHA-256
`c41d7ff1148cec0c0da0b9eadc3c809015cd0042ff0066d61c588ff6b33534ed`.

## Next backlog

1. Add a bounded, root-confined content-search tool for ordinary allowed read
   roots, with literal/regex modes, protected-path filtering, pinned-root race
   resistance, deterministic limits, and structured truncation evidence.
2. Run real Hugging Face inference after a user-controlled managed credential
   is present; continue to fail closed without prompting or discovering one.
3. Consider an OS-keyring/automatic storage backend while retaining the atomic
   `0600` file fallback and secret-free status receipts.
4. Add strict stateless artifact suppression and explicit timer-scope labels.
