# Verified editing and review evidence

Kitsu's review surfaces must display evidence read from Git, the filesystem, or
the host-owned write journal. Assistant-authored claims about changed files are
not review evidence.

## Review targets

`workspace::review_evidence_excluding` accepts an explicit `ReviewTarget`:

| Target | Evidence |
| --- | --- |
| `WorkingTree` | Index and working tree relative to `HEAD`, plus untracked files |
| `Staged` | Index relative to `HEAD`; untracked files are not included |
| `Unstaged` | Working tree relative to the index, plus untracked files |
| `Base(revision)` | Current index and working tree relative to the merge-base of `HEAD` and the resolved revision, plus untracked files |
| `Commit(revision)` | The historical patch introduced by exactly the resolved commit |
| `Checkpoint(report)` | The bounded snapshot-derived diff retained for a write checkpoint |

Interactive `/diff` and `/review` accept these scopes as
`working`, `staged`, `unstaged`, `base <revision>`, `commit <revision>`, or
`checkpoint <id>`, followed by an optional literal path. `/review` is always a
no-tools turn. The top-level `kitsu review` command exposes every live Git scope
for one-shot human or JSONL automation and supports additional user focus
instructions plus opt-in saved-session continuity.

`/diff` displays evidence locally. `/review` and `kitsu review` disclose it to
the provider without granting standing filesystem read access. Each disclosure
announces its bounded byte count and scope. JSONL event fields and ordering
are defined in the [protocol reference](jsonl-protocol.md).

Live Git revisions are resolved as commits with `git rev-parse --verify
--end-of-options`. The resulting hexadecimal object ID, rather than the
user-entered revision, is passed to subsequent Git commands. Git is invoked
with argv directly and never through a shell. Diff pathspecs follow `--` and
use Git's literal pathspec form.

Callers must pass every Kitsu-owned configuration, session, and artifact path
through `excluded`. Exclusions apply to tracked, staged, historical, and
untracked evidence. Untracked symlinks report their existence but never expose
their targets or target contents. Every returned live or checkpoint diff is
bounded to 256 KiB and records whether truncation occurred.

User-entered paths should first be converted with
`workspace::pathspec_from_directory`; this preserves the launch-directory
meaning while rejecting paths outside the repository.

## Workspace diff tool

`workspace_diff` lets the model request the same Git evidence during chat.
It is registered only inside a worktree outside Kitsu-private state, including
in scripted and piped chat. It is omitted from tool-free reviews and
background-agent tool sets. `chat --no-tools`, `--no-workspace-evidence`, or
top-level `workspace_evidence = false` disables its registration.

For example:

```json
{"scope": {"base": "main"}, "path": "src"}
```

`scope` defaults to `"working"`. It also accepts `"staged"`, `"unstaged"`,
`{"base": "REV"}`, and `{"commit": "REV"}`. Checkpoints remain host-only.
The optional literal `path` narrows evidence relative to the launch directory;
it cannot select another repository or escape the bound root. The result
contains `target`, `resolved_revision`, `merge_base`, `truncated`, and `diff`,
with the same size cap and private-path exclusions as the other review surfaces.

Each call discloses evidence to the provider and announces that disclosure.
It neither consults nor changes filesystem read roots, and it raises no
filesystem approval prompt. It cannot modify files, checkpoints, or artifacts.
JSONL emits `workspace_evidence_disclosure` between the matching tool events;
see the [protocol reference](jsonl-protocol.md) for its payload.

## Write checkpoints

Each `JournalTurnReport` and `JournalChangeReport` carries a unified diff
derived from the exact before/after snapshots used by the undo journal. The
journal never accepts assistant-authored diff text. Binary contents are
omitted, terminal control characters in paths and contents are escaped, and
the aggregate diff is capped at 64 KiB. Per-change diff bodies share a separate
64 KiB budget so a checkpoint report cannot grow with the number of files.

Use `WriteJournal::checkpoint_summaries` for a cheap list UI and
`WriteJournal::checkpoint_preview(id)` for one full bounded preview. Undo still
performs identity, content, metadata, permission, and conflict validation; a
preview is evidence, not authorization to restore a file.

## Streaming private artifacts

Large host-owned output can be streamed without retaining it in memory:

1. Call `create_unique_private_artifact(directory, stem)` with a protected,
   host-selected directory and a non-secret stem.
2. Stream bytes through the returned `PrivateArtifactWriter`, which implements
   `std::io::Write`.
3. Publish the path only after consuming `finish()` successfully.
4. Call `abort()` on explicit cancellation, or drop the writer; unfinished
   artifacts are removed automatically.

On Unix, allocation pins and traverses the directory without following
symlinks, uses exclusive/no-follow creation, and sets mode `0600`. The API is a
host library facility and is not registered as a model-callable tool. A command
artifact sink is responsible for framing stdout versus stderr if both streams
share one artifact. Session-backed command output lives beside its snapshot;
stateless command runs retain a unique run directory under protected Kitsu
state so their published artifact references remain valid after process exit.
