# Codex/Claude UX cycle 19: native bounded content search

## Challenge

Run three seeded, permission-prompt-free comparisons at UTC boundary
`1786714200` (cycle seed `992607`), verify the managed-auth and terminal-session
receipt fixes, exercise exact stale-edit recovery, and close the repeated
content-search gap that made Kitsu read whole fixture sets and consume an extra
provider round.

The scored pre-fix binary was SHA-256
`c41d7ff1148cec0c0da0b9eadc3c809015cd0042ff0066d61c588ff6b33534ed`.
Every scored Kitsu launch used a private pin plus a launch-time shared-binary
hash gate, one attempt, exact least-authority roots, and hard process bounds.
No benchmark requested permission expansion, exposed credentials, mutated the
shared tree, or left a child running.

## Seeded comparison

| Task | Kitsu | Comparator | Attribution |
|---|---:|---:|---|
| Recheck the full managed-credential lifecycle and discovery surface | 100/100 | Claude surface check; official Codex contract | Kitsu matched all eight lifecycle outcomes, authenticated exactly one local catalog request, protected then removed the managed token, and emitted no secret-derived evidence. Both real Hugging Face credential gates were false, so no remote request was eligible or sent. |
| Recheck fresh, resumed, query-only, and disabled terminal session receipts | 100/100 | deterministic control oracle | All four terminal receipts, inventories, rollback dispositions, and exit codes matched. The Cycle 18 gap is closed. |
| Recover from a required stale Python edit, inspect the file, then apply a two-line correction | 95/100 | Codex 95/100 provisional | Kitsu recovered and produced behaviorally equivalent code with truthful raw/recovered error accounting, but placed `.upper()` in the return expression instead of the byte-oracle assignment. Codex matched the exact source oracle but remained an embedded control. |

The managed-auth digest was
`c42d10e61a5971b71e72225011d280d06a8f96484e5187b09745c68244bfa684`.
The run started at `1786714223`, completed in 2.002 seconds, and matched the
eight-step oracle: managed login, managed status, one authenticated local
catalog doctor request, environment precedence, empty-environment managed
fallback, malformed-environment fail-closed behavior, logout, and missing
final status. Top-level `auth` discovery and the exact `kitsu auth --help` hint
were present. The credential directory was owner-only mode `0700`; its one
regular file was mode `0600`, single-link, and not a symlink. Logout left it
empty. All 12 children and the mock were reaped. Process and user-service
`HF_TOKEN` presence were both false, so real Hugging Face remained an
infrastructure gate rather than a product failure.

Claude exposed login, status, and logout with parseable status JSON. A local
standalone Codex executable remained unavailable; the
[official OpenAI authentication documentation](https://developers.openai.com/codex/auth/)
describes login, status, logout, stdin credential flows, and file/keyring/auto
storage.

The session-receipt digest was
`e1348395e76c26055e4ceaee240c1b2576dfb12e19b145688b1fd6cd5f2a32a1`.
The first and only wrapper launched at `1786714200.000145`. A failed fresh turn
reported `session_id:null` plus `pending_until_first_success` and had no
showable snapshot. A failed resumed turn retained
`s-seed-992607-existing` plus `resumable`, and its transcript hash remained
`b94d5ac6…16b1`. Query-only durable work returned null/pending with no
snapshot; disabled continuity returned null/disabled. Both failed turns emitted
`discarded` and `incomplete_turn`. Exactly two loopback requests matched, the
inventory retained only the productive preseeded session, and all children
were reaped.

The stale-edit digest was
`cc08dbcda9ccaa769c1b530d94513440a91b11ad85f7ebc91764dba2480913c8`.
Kitsu's first edit failed without mutation, it read the exact 160-byte source,
then made one successful edit and returned the exact completion receipt. The
72.653-second run exited zero with a committed disposition,
`tool_errors=1`, and `recovered_tool_errors=1`. Its final hash
`469aca7b…31f2a` differed from the strict `333f6bf1…c90b` oracle only because
`team.strip().upper()` was expressed as `team.strip()` followed by
`normalized.upper()` in the return. Behavior was equivalent. Codex reproduced
the required stale failure, read once, and applied the byte-exact correction.
The Kitsu deduction is model instruction adherence; scoped authority,
precondition safety, lifecycle, completion, and evidence all passed.

## Selected improvement

On Unix, `repo_grep` is now an always-registered in-process UTF-8 content-search
tool. It no longer probes for or shells out to `rg`, so isolated runners and
minimal containers retain the same read functionality as a full developer
shell. A provider-free post-build smoke test with `PATH=/nonexistent` advertised
exactly `fs_read_file`, `fs_list_dir`, and `repo_grep`.

The native walker starts from the already-authorized canonical root, pins it
with a no-follow descriptor, and traverses children with descriptor-relative
`openat` calls. It sorts raw entry names for deterministic output; skips hidden
entries, symlinks, devices, sockets, FIFOs, binary/non-UTF-8 data, and files over
1 MiB; and revalidates every opened regular file against Kitsu's protected-path
and protected-inode policy. Kitsu-private paths and hard-link aliases therefore
remain hidden, while narrow internal artifact roots still work. An ancestor
rename or symlink swap cannot redirect the pinned search capability.

Search accepts regular expressions or `literal:true`. Pattern length is capped
at 4,096 bytes, and `max_count` is a global matching-line bound from 1 through
1,000 (default 100). Native work is additionally bounded to 10,000 files,
20,000 directory entries, 64 levels, 16 MiB total scanned bytes, 1 MiB per file,
10 seconds, and the existing tool-output ceiling. Cancellation is checked
during traversal and line matching. Reaching a bound returns an explicit
receipt with the stop reason and observed counters; it never silently presents
partial output as complete or claims no matches beyond an unfinished search.

Non-Unix builds retain the fixed-argv ripgrep backend and advertise it only when
`rg` is actually available. The shared descriptor documents that its matching
limit is global on the native Unix path. README and functionality documentation
record the platform behavior, limits, literal mode, and skip rules.

## Verification

New regressions prove:

- Unix registration advertises bounded content search without consulting
  `PATH`, while agent/command authority remains independent;
- literal metacharacters and nested paths produce deterministic output;
- `max_count` is global and emits a typed truncation receipt;
- invalid regexes and unbounded arguments fail before filesystem access;
- oversized files cannot starve later source files, and an explicitly selected
  oversized file gets a truthful skip receipt;
- hidden entries, symlinks, protected paths, and a private hard-link alias
  created after protection registration cannot leak;
- an ancestor swap continues searching the pinned authorized tree; and
- cancellation stops before consuming the selected file.

Verification completed without permission prompts:

- complete `kitsu-tools` suite: 147 passed;
- complete CLI integration suite: 150 passed;
- complete workspace suite: 725 passed;
- `cargo fmt --all -- --check`;
- `cargo clippy --workspace --all-targets -- -D warnings`;
- staged and unstaged `git diff --check`;
- fresh debug build and isolated-`PATH` provider-free tool-surface smoke test.

The post-fix debug binary is SHA-256
`eb3145510fec2fa93d842715810c5839e92a7919606e705e8fecacc86424d42e`.

## Next backlog

1. Re-run the multi-file search oracle against this post-fix binary and compare
   provider rounds, model-visible bytes, exact outcome, and timeout behavior.
2. Run real Hugging Face inference when a user-controlled credential becomes
   present; continue to fail closed without prompting or discovering one.
3. Consider `.gitignore`-aware native traversal without weakening descriptor
   pinning, plus a native no-reparse backend on non-Unix hosts.
4. Consider an OS-keyring/automatic credential backend while retaining the
   atomic `0600` file fallback and secret-free receipts.
5. Add strict stateless artifact suppression and explicit timer-scope labels.
