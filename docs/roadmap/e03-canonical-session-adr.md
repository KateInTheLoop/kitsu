# ADR: canonical session revision and logical HEAD

Status: design contract accepted in Cycle 050 and neutral logical values
accepted in Cycle 051; no production format or store behavior changes yet.

Owner: E03. Depends on E02's causal run outcome. This ADR refines the
K01-K12/X005 gate in the [mechanism ledger](mechanism-ledger.md) and supersedes
no current v1 snapshot behavior until a later migration card is accepted.

## Decision

Every restart-authoritative session mutation publishes one complete,
self-validating canonical revision. The revision contains its selected logical
HEAD; neither a filename, file tail, directory order, database row, cache,
listing index, compaction summary, export, nor UI state may select it.

Publication is compare-and-swap against an expected opaque revision. A writer
lease excludes simultaneous active writers, but the lease is not a revision and
does not replace the expected-revision recheck. A stale writer returns a typed
conflict without staging or publishing state.

The initial implementation remains a private compiled file store. E03 first
defines a private store port and mounts this same contract against it. X005 does
not pass until the complete K01-K12 suite also runs unchanged against one named
independent compiled store adapter. Runtime store plugins are not required and
remain out of scope.

## Terms that must not collapse

| Term | Meaning | Never use as a substitute |
|---|---|---|
| `SessionId` | Stable logical conversation identity. | Name, path, process ID, or store row ID. |
| `StoreId` | Opaque identity of one configured physical store namespace. | Directory path or backend label. |
| `RevisionId` | Store-minted opaque nonzero identity for one canonical publication. It is not ordered and is not a content hash. | Timestamp, ordinal, file inode, byte offset, digest, or logical HEAD. |
| `ExpectedRevision` | `Missing` for no-clobber creation or one exact `RevisionId` for replacement. | Lease ownership, last-read timestamp, or “latest.” |
| `TurnId` | Opaque identity for one admitted logical operation recorded in canonical history. | Message index, provider attempt, tool call ID, or run ID. |
| `LogicalHead` | `Genesis` or one validated terminal `TurnId` selected inside the canonical revision. | Last array element, file tail, projection checkpoint, or active UI row. |
| Canonical digest | Integrity checksum over the canonical payload and its immutable reference closure. | Authentication, authority, or `RevisionId`. |
| Projection checkpoint | Disposable `{revision, head, policy/generation}`-bound acceleration state. | Canonical history or selected HEAD. |
| Commit receipt | Observation of publication and durability for one base/new revision pair. | Semantic run outcome or delivery success. |

Opaque IDs are bounded, closed values. Their initial textual encoding will be
versioned and validated before filesystem or adapter lookup. Ordering comes
only from validated parent links and canonical operation order, never lexical
comparison of IDs.

Cycle 051 implements that vocabulary in `kitsu-core::session`: bounded typed
`StoreId`, `SessionId`, `RevisionId`, and `TurnId`; explicit
`ExpectedRevision` and `LogicalHead`; terminal-bound canonical operations and
history; closed lookup states; and publication receipts that remain separate
from semantic outcomes. The versioned JSON vectors reject unknown fields,
future contract revisions, unsafe identities, unknown parent heads,
duplicate turn/run identities, invalid conflicts, and no-op exact revisions.
`scripts/check-session-boundary.sh` keeps this logical layer free of physical
store, provider/tool, Tokio, environment, filesystem, process, and presentation
dependencies. This is a contract seam, not a new persistence implementation.

## Canonical logical value

The private Rust shape may evolve before implementation, but it must preserve
this field ownership:

```text
CanonicalSessionRevisionV1 {
  logical_schema,
  store_id,
  session_id,
  revision_id,
  parent_revision: Missing | RevisionId,
  canonical_digest,
  created_at,
  committed_at,
  metadata: { name?, launch_directory, requested_gateway, requested_model,
              project_context_source? },
  lineage: Root | Fork { source_store, source_session,
                         source_revision, source_head, branch_turn },
  operations: [CanonicalOperationV1 ...],
  selected_head: Genesis | TurnId,
  artifact_manifest: [ImmutableArtifactRefV1 ...],
  compaction_binding: None | ContextProjectionRefV1,
}
```

`CanonicalOperationV1` records one admitted run identity, user input, ordered
assistant/tool conversation facts, semantic terminal outcome, assistant
disposition, retained-effect references, and the before/after logical head.
Cancelled, interrupted, failed, and discarded operations can therefore remain
restart-authoritative without pretending they advanced model context. A head
may advance only through a causally closed exchange: every tool call has one
matching result, all referenced artifacts are in the manifest, child/effect
settlement required by E02 is terminal, and the assistant disposition agrees
with the terminal outcome.

The canonical history is never destructively replaced by compaction. A context
summary is a disposable projection bound to exact source revision, selected
head, included operation closure, route/capability/model/estimator generations,
budget, and a loss manifest. Missing or mismatched projection facts yield
`StaleProjection`; Kitsu rebuilds from canonical history or fails explicitly.

The metadata fields are resume *intent*, not authority. Resume revalidates the
current working directory, provider route/model, project context, tools,
artifacts, credentials, and policy. Permission grants, approval leases,
credential bytes, provider runtime objects, command policies, and executable
tool inventory are structurally absent.

## Current v1 field disposition

| Current value | Target owner | Migration rule |
|---|---|---|
| `SavedSession.version` | Canonical logical schema only. | v1 decodes with the existing strict rules, then an explicit migrator creates a v2 candidate; physical/export versions remain separate. |
| `id`, `created_at`, `name` | Canonical identity/metadata. | Preserve exactly after validation. A rename is a revisioned metadata mutation and cannot rewrite transcript bytes in place. |
| `updated_at` | Canonical commit metadata. | Recomputed only for an accepted new revision; listing order is a projection of validated revisions. |
| `directory`, `gateway`, `model`, `project_context` | Canonical resume intent. | Preserve as inert facts, then revalidate against current composition before use. |
| `messages` | Canonical operation/history facts plus derived model-context projection. | Migration validates role/tool causality. The saved system prompt remains historical input, but current capabilities/context replace it in the derived request projection. |
| `SessionSummary`, `last`, names index | Disposable listing/selector projection. | Rebuild only from validated canonical revisions. Corrupt target selection is typed, never silently treated as an earlier HEAD. |
| `SnapshotCommit.path/file` | Optional physical file-store evidence. | Capability-gated receipt field; no portable store API requires a path or descriptor. |
| `<id>.artifacts` | Immutable referenced artifact closure plus unreferenced physical candidates. | A revision may reference only already-durable, validated entries. Orphans are never canonical and require bounded recovery policy. |
| Archive directory and export files | Physical lifecycle state and versioned projections. | Archive/restore/delete publish typed receipts/tombstones; export cannot become canonical merely by being newest. |

Current `SavedSession` persists no terminal outcome, lineage revision, selected
head, artifact manifest, or compaction binding. Those facts cannot be inferred
from the final message, snapshot path, artifact directory, or process exit
during migration.

## Store contract and typed states

The private store port deals only in validated logical values and opaque
receipts. At minimum, lookup returns exactly one of:

```text
Missing { tombstone_revision? }
Present { revision, head, canonical_value }
Corrupt { session, observed_identity?, reason_class }
Unsupported { logical_schema | physical_schema | required_capability }
Conflict { expected, observed }
```

Provider-controlled or persisted text is never copied into a reason class.
`Corrupt` never falls back implicitly. Verified last-good recovery is an
explicit operation that retains/quarantines the corrupt current artifact,
validates the same session lineage and every reference in an earlier revision,
and publishes a new recovery revision with a receipt. If no candidate closes,
recovery remains `Corrupt`.

Mutation admission follows one sequence:

```text
resolve store/session -> read and validate current revision
-> acquire/confirm writer fence -> re-read and validate
-> compare expected revision -> validate complete candidate/reference closure
-> stage without overwrite -> sync candidate and required artifacts
-> atomically publish complete canonical revision -> sync publication owner
-> optionally build disposable projections -> return commit receipt
```

Delete publishes a canonical tombstone with store/session identity, previous
revision, and its own opaque revision before physical reclamation. Lookup may
project that tombstone as `Missing`, but expected-revision comparison and
recovery retain it so a stale writer cannot resurrect the deleted generation.
Archive and restore are canonical availability-metadata revisions, not
directory moves that independently redefine session truth.

No user input, tool result, terminal outcome, compaction summary, artifact
reference, fork child, rename, or selected HEAD becomes restart-authoritative
before the canonical publication step. Projection failure after publication is
reported but cannot undo or hide the canonical revision.

The commit result distinguishes:

- `NotPublished { base_revision }`: no visible canonical mutation;
- `Published { base_revision, new_revision, head, durability = confirmed }`;
- `PublishedUnconfirmed { base_revision, candidate_revision, observed_file? }`:
  install occurred but the required durability observation failed. Callers must
  reload and must not claim rollback or durable success.

A crash at a physical cut may reopen to the complete old or complete new
revision according to the platform's durability contract, never a mixed value.
The adapter must expose enough deterministic fault hooks to prove every cut.

## Logical HEAD, branching, and lineage

`selected_head` is part of the canonical revision and must resolve to `Genesis`
or a terminal causally closed operation in that same value. A metadata-only,
failed, cancelled, or discarded operation may create a new revision while
leaving the selected head unchanged.

Branch-at-turn validates the requested source operation against the source
revision and selected-history ancestry. It reserves the source against deletion
while staging, copies only the exact closed prefix and referenced immutable
artifacts, creates a fresh session/revision identity with zero restored
authority, and publishes the child no-clobber. Its lineage binds source store,
session, revision, head, and branch turn. A mid-tool boundary, missing artifact,
stale source revision, cycle, or destination collision leaves no visible child.

Forking the current complete history is the same operation with `branch_turn =
source_head`; it is not a separate weaker path.

## Version, migration, export, and rollback

Versions are independent:

1. canonical logical schema;
2. physical store/layout schema;
3. artifact-reference schema;
4. context/listing projection schema;
5. export/import schema.

A store may change its physical schema without changing the logical value. A
projection may be deleted and rebuilt without changing revision or HEAD. An
export is a bounded disclosure projection with source store/session/revision/
head and loss metadata; import validates its whole migration and artifact plan,
then publishes a fresh local session/revision no-clobber. Backend switch follows
the same export/import semantics and re-resolves all authority.

The first migration reads strict current v1 snapshots and publishes a separate
v2 candidate; it never edits v1 bytes in place. Unknown/future logical or
physical versions fail before write. Downgrade cannot silently discard fields:
it requires an explicit versioned export with a loss manifest or is refused.
Exact source revert remains the rollback until the v2 migration is released;
after release, rollback is a new canonical revision selecting a validated prior
head, never a pointer or projection rewrite.

## Stable provider-free K01-K12 matrix

These IDs retain the accepted research matrix while adding the canonical-HEAD
qualifications required by the current roadmap. Every implementation receives
the same logical fixtures and fault hooks.

| Case | Required assertion |
|---|---|
| K01 save/load and whole operation | System/user/assistant/tool facts, resume intent, terminal outcome, head, compaction binding, and artifact references round-trip exactly. Cuts after prompt, each tool call/result, assistant output, terminal settlement, and compaction staging reopen to the complete old or new revision; no partial operation or summary-only history appears. |
| K02 list/select and logical HEAD | Listing is deterministic (`committed_at DESC`, then session ID), `last` retains the current real-conversation preference, and exact ID/name ambiguity is typed. Delete every listing/context projection and rebuild: the same validated revision and selected HEAD result. A corrupt selected target is not skipped as recovery. |
| K03 versions and corruption | Older supported logical input migrates explicitly; future logical, physical, projection, artifact, and export schemas are distinguished. Malformed payload, ID/revision/digest mismatch, invalid head, broken tool causality, and missing required reference return typed `Corrupt`/`Unsupported` before mutation. |
| K04 physical commit cuts | Inject before candidate write, after write, after candidate sync, after artifact sync, after atomic install, after publication-directory sync, and before receipt. Visibility is exactly old or new; the result distinguishes not-published, published-unconfirmed, and confirmed commit identity. |
| K05 fork and branch-at-turn | Current-head fork and an earlier valid turn create fresh no-clobber identities, bind exact source store/session/revision/head/turn, preserve a closed prefix, copy only referenced artifacts, and restore zero authority. Mid-tool, stale, missing-artifact, collision, and injected copy/publish failures leave source unchanged and no visible partial child. |
| K06 delete/archive/restore | Empty/`last`/prefix/active selectors reject; a live lease or stale expected revision rejects; exact inactive operations are durable and receipt-bearing. Partial cleanup reports residue, tombstones prevent stale resurrection, and recoverable duplicates never create two logical HEADs. |
| K07 one writer and independent progress | Two activations for one session admit exactly one writer. Save/rename/delete/archive/fork races cannot overwrite or resurrect an active identity; independent session IDs continue. Lease loss is fenced before publication. |
| K08 fresh revision and expected-HEAD CAS | Load A, publish B, then publish from A: the stale writer gets `Conflict { expected: A, observed: B }` with zero mutation. Retry from B succeeds as one new opaque revision. Delete/recreate of the same session label has a distinct store/session/revision closure. |
| K09 idempotence, recovery, and ordering | Repeated reads/list/rebuilds are mutation-free; identical saves still have explicit revision semantics; every fork is fresh; second delete is `Missing`. Orphan candidates/projections never win by timestamp or filename. Explicit last-good recovery is deterministic, same-lineage, fully validated, and receipt-bearing. |
| K10 cancellation | Pre-admission cancellation performs no I/O. Before publication, cancellation aborts and cleans bounded staging. Once atomic publication starts, cancellation waits for the truthful receipt/reload result. A synchronous adapter declares cancellation unsupported instead of simulating abandonment. |
| K11 fresh authority | Seed live grants, approval leases, credential sentinels, tool/source generations, and executor counters, then load/fork/import/switch stores. Persisted state contains none of them; restore executes/resolves nothing; the next effect uses only current principal/policy/credential/tool admission. |
| K12 receipts, artifacts, export/import, and adapter parity | Save/fork/branch/delete/recovery receipts carry opaque store/session/base/new revision, head, lineage/outcome, durability, residue, and optional capability-gated file identity. Missing/symlinked/swapped/tampered artifacts fail closed. Versioned export/import and backend switch preserve canonical value or declare exact loss, publish no-clobber, and reproduce the same result on both named adapters. |

K01-K12 is one matrix, not twelve implementation-specific test modules. The
current file adapter may add physical assertions, but adapter-specific evidence
cannot weaken or replace any backend-neutral assertion.

## Coupling and authority rules

- `kitsu-core` may consume canonical session/run values by value but imports no
  filesystem, database, store adapter, config, credential, permission, or
  projection implementation.
- Product composition chooses the store and revalidates current authority.
  Storage cannot grant tools, resolve credentials, execute historical calls,
  choose provider fallback, or mint semantic success.
- Artifact authenticity/authorization remains with Kitsu's artifact and
  permission owners. The store persists validated immutable references and
  reports physical observations only.
- E02 owns semantic run outcome. E03 records the immutable outcome and owns its
  persistence receipt; it does not reclassify outcome from messages, EOF, file
  presence, or process exit.
- Every public human/JSONL session receipt eventually projects from the same
  canonical revision/commit receipt. Presentation does not read a private path
  or listing index to reconstruct truth.

## Rejected alternatives

- Add `revision` to the current mutable JSON file but keep last-message HEAD.
- Treat the per-session lease, inode, content hash, timestamp, array length,
  SQLite row, or projection checkpoint as the revision or selected HEAD.
- Make compaction summary/history, exported Markdown/JSON, or artifact directory
  authoritative when canonical state is absent.
- Skip a corrupt current revision during ordinary list/resume and silently use
  an earlier file.
- Restore saved grants, credentials, provider runtime, tool inventory, or
  approval decisions for convenience.
- Introduce a database, event log, or runtime plugin before the logical contract
  and current-file fault suite demand it.

## Delivery sequence after this ADR

1. **Complete in Cycle 051:** add provider-free opaque ID, logical-head,
   canonical-operation, receipt, and typed-state values with fail-closed
   validation and fixed vectors.
2. Extract the current file store behind a private port without changing v1
   behavior; mount shared read/list/current invariant cases first.
3. Add expected-revision publication and deterministic K04/K07/K08 fault hooks,
   then migrate whole-operation outcome publication.
4. Add explicit v1-to-v2 migration, retained revision generations, projection
   rebuild, and verified last-good recovery.
5. Add branch-at-turn, artifact closure, export/import, and full K01-K12.
6. Mount one genuinely independent compiled store adapter before X005 can pass.

Cycle 050 accepts only this ownership and fixture contract. It does not claim
E03-S01 complete until the neutral value vocabulary and ambiguity checks land,
and it does not change the X005 failure score.
