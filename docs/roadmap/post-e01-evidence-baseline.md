# Post-E01 Evidence Baseline

Status: pinned for E00-S03 at Kitsu commit
`986fba971ec99c9af5dcd8f69a48f507aae486af`.

This receipt separates three things that earlier comparison prose could blur:
the frozen research subject, committed Kitsu changes since that subject, and
the accepted improvement-branch changes. It is an inventory, not a
replaceability certification.

## Evidence labels

- **Frozen research** means source/test-source evidence rooted at the clean
  detached Kitsu worktree `6c7133334dadf66a545888f4068ac82585a2768f`.
- **Committed delta** means a Git diff or test in Kitsu history after that pin.
- **Accepted branch evidence** means a focused improvement commit with a cycle
  receipt and its recorded provider-free acceptance commands.
- **Executed conformance** requires the named X001-X005 fixture against the
  default and independent implementation. Source presence, unit tests, two
  labels, or a trait do not satisfy it.
- **Runtime-observed** is reserved for an executed product carrier/fixture, not
  a source inspection or unit test. No new X001-X005 runtime conformance result
  is claimed by this baseline.

## Git and workspace receipt

| Item | Pinned value | Verification |
|---|---|---|
| Frozen Kitsu subject | `6c7133334dadf66a545888f4068ac82585a2768f` | `research/workspaces/kitsu-baseline` was detached/clean at that exact commit |
| Clean implementation base | `a2ba2d5c98b17da7290a2b6b8b24c83bc2529763` | merge-base with the frozen subject is exactly `6c713333`; Cycle 001 recorded clean creation |
| Frozen-to-base delta | 67 commits; 31 changed paths; 9,457 insertions; 2,743 deletions | `git rev-list --count`, `git diff --numstat`, and commit-log inventory |
| Post-E01 branch pin | `986fba971ec99c9af5dcd8f69a48f507aae486af` | eight focused commits above the implementation base; branch clean before this audit |
| Base-to-post-E01 delta | 8 commits; 19 changed paths; 1,744 insertions; 103 deletions | `git rev-list --count` and `git diff --numstat` |
| Original main worktree | ten modified paths | status still matches Cycle 001 exactly; no stash/reset/absorption |
| Toolchain used for branch gates | `rustc 1.93.0`; `cargo 1.93.0` | local version output on 2026-08-18 UTC |

The A001 experimental worktree is deliberately not a baseline: it is at
`cb41b0309cc538756e886404ecbfc9bcfb446d8f` with five modified paths. Its
accepted decision record is pinned below, and Cycles 005, 007, and 008 supply
the clean-branch implementation evidence.

## Non-versioned research-input hashes

The research root has no Git metadata. Its architecture file calls itself a
frozen-baseline description, and `HARNESS_VS_KITSU.md` calls itself historical.
They remain immutable inputs for this program; Kitsu-owned current-state docs
supersede rather than overwrite them.

All hashes are SHA-256:

| Research input (under `../harnesss/research`) | Digest |
|---|---|
| `architecture/KITSU_CURRENT_ARCHITECTURE.md` | `df5ad47445d3d2f0b835fd11f2fd9b4835fb11375a9da79bddabf4c043ef72f6` |
| `HARNESS_VS_KITSU.md` | `bae7520f55ef636908b64be14fb1cd21ffc5220bdcd5126f4c5227c12ffc042d` |
| `architecture/IMPROVEMENT_BACKLOG.md` | `18877b20996b5a2ed5d1d145c61c9ca83063c8ede6f79b7e19049bd415b93fe0` |
| `PROGRAM_STATUS.md` | `3df6ee2045c6a35034eeaead4b479cab1c990c0b012ce45dfbe23951e0f4898c` |
| `comparison/adjudicator/FINAL_COMPARISON.md` | `5233e1380b2c2624a801e30f56dde56d64f14c7462187e3bddcaaa3d6cd3b439` |
| `architecture/implementations/A001.md` | `b9565f92e88973b634edb3dfbae779d314a333b74fb0df0be3acaf62a26e8ef8` |
| `architecture/KITSU_TARGET_ARCHITECTURE.md` | `61f0c561dd2d0b7a9eb85a5673cf30d29af170181ccd008befb0f37c581816d6` |
| `architecture/PATTERN_MATRIX.md` | `0c57fa49d13298e68f3c020a51a634ad923e6e9565cc9640dc56cd26ae5abad3` |

## Committed delta classification

The 67-commit delta is real but concentrated: provider retry/context discovery,
deadline transcript truth, command-containment portability/CI, bounded local
read/navigation tools, recall/offload tuning, and extensive reviews. The eight
improvement commits add planning/evidence plus the two bounded fixes. The
following statuses prevent broad commit volume from being mistaken for a
closed architectural gap.

| Area | Committed evidence after `6c713333` | Status at post-E01 pin | What remains |
|---|---|---|---|
| Provider retry | Deadline-bounded attempt telemetry and retry handling now cover 408, 429, 502-504, 520-524, and 529; generic 500 stays final. | **Partially closed**, source/test-backed | No immutable adapter/profile/generation binding across retry and auxiliary sends; no capacity fallback taxonomy; only one concrete wire adapter. |
| Context discovery/retention | Context-window discovery reads nested `providers[]`; implausibly small windows are floored; configurable offload cap and timeout-round preservation landed. | **Partially closed**, source/test-backed | No semantic reference graph/ranker; no canonical compaction lineage; auxiliary discovery is not generation-bound. |
| Code navigation | Bounded `find_symbol`, `outline_file`, `read_manifest`, and native repository search are registered under read authority. | **Structural gap closed narrowly**; semantic gap open | Heuristic navigation is not Aider-style tree-sitter/reference-graph/PageRank selection and does not satisfy the semantic-context corpus gate. |
| Recovery | Completed tool rounds survive piped deadline failure with truthful JSONL framing; exact edit/undo and command reaping remain strong. | **Partially closed**, source/test-backed | Malformed-current last-good session recovery, canonical revision/HEAD, stale-writer fencing, durable compaction history, and branch-at-turn remain open. |
| Ollama doctor `:latest` | Cycles 003-004 add narrow loopback-Ollama alias equivalence plus human/JSON/no-chat fixtures. | **Closed on branch** | No general alias/fallback policy is implied; explicit tags and generic gateways remain exact. |
| Tool registry A001 | Cycles 005, 007, and 008 add typed same/cross-path rejection, first-entry preservation, fallible CLI composition, complete builtin parity, and ignored-result CI. | **Closed on branch for A001's bounded contract** | Multi-tool batch transactionality, provenance/namespaces, contributor lifecycle, and external tools remain separate work. |

## Five-boundary result at this pin

All five complete conformance gates still fail. This is unchanged from the
research verdict even though prerequisite mechanisms improved.

| Boundary | Result | Delta that matters | Still-missing decisive gate |
|---|---|---|---|
| X001 provider | **Fail** | Better retry/context discovery; Ollama diagnostic fix | Distinct compiled dialect, explicit capabilities, immutable generation across every primary/auxiliary send, and common conformance suite |
| X002 tools/source | **Fail** | Deterministic registry admission and structural read tools | Bounded supervisor, pre-spawn authority admission, MCP lifecycle evidence, source identity, and a named independent source |
| X003 host/UI | **Fail** | No qualifying committed host/principal delta | Named second host/principal and shared identity/terminal carrier fixture; one-way JSONL is not a host adapter |
| X004 automation/runtime | **Fail** | Better timeout/JSONL truth | Host-neutral run/outcome contract plus provider-free carrier fault matrix and independent harness |
| X005 session/store | **Fail** | Atomic snapshots/forks remain strong; timeout transcript recovery improved | Canonical revision/logical HEAD, expected-head fencing, K01-K12, branch-at-turn, last-good recovery, and a named store adapter |

## Consequences for the queue

1. Do not reschedule the closed Ollama defect or bounded A001 transfer.
2. Treat code navigation as present; schedule only measured semantic selection.
3. Carry retry/context/recovery improvements into fixtures as baseline behavior,
   not as proof of immutable generations, causal outcomes, or canonical HEAD.
4. Keep X001-X005 at fail until their complete named gates execute.
5. Use the next cycle to publish Kitsu-owned current architecture and gap status
   from this pin while leaving the hashed research snapshot untouched.
