# ADR: canonical session revision and logical HEAD

Status: design contract accepted in Cycle 050, neutral logical values accepted
in Cycle 051, and the private compiled physical layout/record contract accepted
in Cycle 058. Cycle 059 adds private descriptor-pinned candidate I/O and exact
verification; Cycle 060 adds verified HEAD reads and writer-fenced physical CAS.
Neither migration nor production store behavior has changed yet. Cycle 061
adds the closed logical payload codec and strict current-v1 migration input;
Cycle 062 composes validated bytes with immutable candidate install and fenced
HEAD publication behind one still-unmounted transaction. Cycle 063 defines the
only accepted current-v1 logical import: an exact, loss-bearing legacy baseline
at `Genesis`; it still has no production publication caller.
Cycle 064 composes that preparation with the private physical transaction as a
create-only operation while keeping production behavior unmounted.
Cycle 065 adds a pure, unmounted successor assembler for one complete new
operation from neutral E02 terminal truth and closed transcript/effect facts.
Cycle 066 composes only its artifact-free text subset with fenced file
publication; artifact-bearing bases remain structurally refused.
Cycle 067 adds private physical immutable artifact records and I/O. Cycle 068
composes their exact closure with private canonical publication for one complete
tool/result/effect operation. Cycle 069 applies the same closure to canonical
HEAD open. Cycle 070 maps that private result to the neutral lookup vocabulary
without separating `Present` from its verified payload. Cycle 071 types the
earlier physical HEAD/revision envelope and invalid selected HEAD. Cycle 072
advances the private physical schema to revision 2, binds exact parent-manifest
digests, and verifies bounded selected lineage. Production is unmounted.

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

`canonical_digest` is deliberately not serialized inside the bytes it hashes.
The physical revision manifest owns the exact canonical-payload digest and
length; because the payload contains every immutable artifact digest/reference,
that outer digest binds the complete logical closure without a recursive or
mutable checksum field.

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

Cycle 063 therefore permits only a typed legacy baseline, not reconstructed
operations. It binds the exact source snapshot digest, source session and update
time, preserves the validated semantic message sequence, selects `Genesis`, and
starts with no canonical operations. Its fixed loss manifest declares
`terminal_outcomes_unavailable`, `logical_heads_unavailable`, and
`turn_lineage_unavailable`. The baseline is not a valid branch turn. Recognized
offload pointers or artifact references require an explicit immutable resolver
and otherwise refuse before candidate creation. No persisted provider, model,
directory, message, or legacy metadata restores authority.

## Physical file-store v1 contract

Cycle 058 freezes a private compiled physical schema without mounting it. It is
versioned independently from both current `SavedSession.version = 1` and the
canonical logical schema. Existing `<session-id>.json`, `.locks`, archive, and
artifact paths remain untouched and authoritative until explicit migration.

```text
<current session root>/.kitsu-store-v1/
  store.json
  revisions/
    id-<lowercase base32 session identity>/
      id-<lowercase base32 revision identity>/
        manifest.json
        canonical.json
  artifacts/
    id-<lowercase base32 session identity>/
      id-<lowercase base32 artifact identity>/
        manifest.json
        content.bin
  heads/
    id-<lowercase base32 session identity>.json
  staging/
  .locks/
```

Identity components use lowercase RFC 4648 base32 without padding and an `id-`
prefix. The encoding is reversible, maps `.`/`..` to ordinary non-special
components, remains collision-free on case-insensitive filesystems, and keeps a
maximum 128-byte neutral identity below the portable 255-byte filename limit.
Raw identities, display names, paths, timestamps, hashes, and ordinals are never
used as physical components or revision identities.

`store.json` is create-new and binds one opaque `StoreId`. Each revision
directory is staged privately and installed create-new; `manifest.json` binds
physical/logical schema revisions, store/session/revision identity, exact parent
expectation, canonical byte length, and lowercase SHA-256 of the exact immutable
`canonical.json` bytes. The current head record binds store/session/revision and
the exact manifest-byte SHA-256. It intentionally contains no logical HEAD: the
selected `LogicalHead` remains inside the validated canonical payload.

Only the small head record is replaceable, and only after an exact session-bound
writer-fence confirmation plus expected-revision re-read. A staged or installed
revision directory is not canonical merely because it exists; only a validated
head record can select it. Revision/store installation is no-clobber. Unknown
fields, unknown record kinds, future physical versions, zero logical versions,
invalid sizes/digests, self-parenting, and physical-location identity mismatch
fail closed before publication. Store initialization and revision candidate I/O
now use descriptor-relative no-follow opens, bounded exact-byte reads, synced
create-new files, a private staging directory, and OS-level atomic no-replace
directory installation. Reads verify the manifest location plus exact canonical
length/digest and return the exact manifest-byte digest for future HEAD binding.
HEAD reads validate the closed pointer, physical location, exact manifest digest,
and canonical candidate before returning a revision. Publication requires a
held per-session OS writer fence, reloads the current verified HEAD, applies the
neutral exact-revision admission, checks the candidate's parent and receipt,
reconfirms the fence, and atomically creates or replaces only the small HEAD
file. Pre-install failure preserves the old pointer; post-replace directory-sync
failure reports installed/new-visible but unconfirmed. The physical helper has
no production caller.

Artifact directories are session-scoped and installed create-new through the
same private staging owner. Their independently versioned closed manifest binds
store/session/artifact identity, logical artifact schema, exact content length,
lowercase SHA-256, and optional bounded media type. Content is capped at 16 MiB.
Install writes and syncs content plus manifest before an atomic no-replace
directory rename; exact open uses descriptor-relative no-follow reads and
rechecks physical identity, length, content digest, and manifest digest.
Collision never overwrites the first bytes. Target-sync failure is
installed-unconfirmed; missing, future schema, retarget, digest tamper, and
symlink fail closed. The canonical transaction compares every referenced fact's
schema, ID, length, SHA-256, and media type against a verified installed record
under the writer fence before candidate allocation, then reopens and compares
the entire closure after candidate installation and before HEAD CAS. A missing,
tampered, or unequal precheck allocates no candidate; a failed recheck leaves a
confirmed installed-unselected candidate and the old HEAD.

Canonical HEAD open first requires the physical HEAD and immutable revision to
verify, decodes the selected logical payload, and binds its logical schema,
store, session, revision, and parent to that physical envelope. It then reopens
every referenced artifact and applies the same exact comparison used by
publication. Only an absent physical HEAD returns `None`; invalid logical bytes,
binding disagreement, missing closure, content tamper, or self-consistent but
different physical artifact facts return a typed private read error for the
selected revision. The reader never scans or silently selects an older candidate.

The private lookup envelope carries exactly one neutral state and an optional
verified payload. `Missing` and every `Corrupt`/`Unsupported` state carry no
payload; `Present` carries the same revision and logical HEAD as its decoded
payload. Current typed evidence maps malformed logical bytes to `Malformed`,
envelope disagreement to `IdentityMismatch`, invalid closed operation structure
to `BrokenCausality`, absent required artifact bytes to `MissingReference`, and
length/digest/exact-closure disagreement to `DigestMismatch`. Future logical,
artifact, and artifact-manifest physical schemas map to their explicit schema
dimensions. Zero schema is corruption, never unsupported and never a panic.

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
private schema change is still explicit: revision 2 adds the required parent-
manifest digest to successor records; revision-1 stores are unsupported rather
than decoded as revision 2. No production store uses either revision yet. A
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

The initial current-v1 candidate is a root revision whose selected logical HEAD
is `Genesis`. Historical messages live only in the typed legacy baseline; they
do not masquerade as terminal operations and cannot be branch points. Later
canonical operations may extend that revision normally, while the fixed source
loss record remains auditable. Migration publication must still be no-clobber
and leave the exact v1 source bytes available for rollback.

The private composition borrows current-v1 bytes, prepares the complete
candidate before effects, and always enters publication with
`ExpectedRevision::Missing`. An already-present canonical HEAD therefore fails
before candidate allocation. A same-revision orphan collision is classified and
preserves the first immutable bytes. A cut after candidate installation reports
that exact unselected residue; no cut edits or deletes the v1 source.

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
2. **Initial read/write extraction complete in Cycles 052-053:** route current
   load/load-other/list and active save behind private ports without changing v1
   behavior. Other lifecycle mutations remain direct until their canonical
   operation is introduced.
3. **Admission comparison complete in Cycle 054:** `Missing` admits only no
   observed revision and `Exact(A)` admits only observed `A`; every mismatch is
   a typed zero-effect conflict. **Logical K08 path complete in Cycle 055:** an
   independent in-memory fixture composes admission, opaque successors, exact
   receipts, stale zero-effect rejection, and retry. **Logical K04 cuts complete
   in Cycle 056:** pre-candidate and staged candidate remain not-published/old;
   post-publication is new/published-unconfirmed; confirmation is new/confirmed.
   **Logical K07 fencing complete in Cycle 057:** exact session-bound generations
   exclude a second writer, permit independent sessions, and reject lost,
   replaced, or wrong-session tokens before mint/publication. Next define the
   **Physical layout/record contract complete in Cycle 058:** the disjoint
   namespace, reversible path encoding, create-new store/revision records,
   replace-under-fence head pointer, byte/digest bindings, and fail-closed fixed
   vectors are compiled but not mounted. **Candidate I/O complete in Cycle
   059:** initialize or reopen one exact store identity without clobbering,
   install privately staged revisions with no-replace rename and joined cleanup,
   verify exact stored bytes, and preserve installed-unconfirmed sync truth.
   **Physical HEAD CAS complete in Cycle 060:** bind a held per-session OS fence,
   verified current pointer, neutral expected revision, exact candidate parent,
   and manifest digest to atomic create/replace with old-or-new visibility and
   installed-unconfirmed truth. **Closed canonical payload and migration input
   complete in Cycle 061:** a binary-private codec validates bounded resume
   intent, lineage, whole terminal operations, role/tool causality, exact
   selected-head ancestry, exact immutable artifact closure, retained effects,
   and compaction generation bindings. A separate strict current-v1 decoder
   preserves the exact source digest/transcript, rejects unknown nested fields
   and broken causality, and always reports missing terminal facts; recognized
   offload pointers add an artifact-closure blocker. It has no publication API,
   so neither final messages nor file/process state can become invented terminal
   truth. Next compose this payload with candidate install and fenced HEAD CAS
   in one provider-free transaction before mounting production behavior.
   **Provider-free physical transaction complete in Cycle 062:** deterministic
   canonical encoding and receipt construction precede every physical effect;
   one exact session fence pre-admits the expected revision before candidate
   allocation, candidate installation is immutable, and physical CAS repeats
   admission immediately before HEAD publication. Success returns confirmed or
   published-unconfirmed neutral receipts. Every prepublication failure returns
   exact not-published base truth plus none, confirmed/unconfirmed unselected,
   or conflicting-collision candidate residue. Stale work allocates no orphan;
   equal and different same-revision collisions preserve the first bytes. The
   transaction remains private and unmounted. **Explicit current-v1 legacy
   baseline complete in Cycle 063:** strict input becomes a root `Genesis`
   candidate with exact source digest, timestamp, resume metadata, and closed
   transcript plus the fixed three-fact loss manifest. It creates no operations,
   invents no terminal result or branch point, restores no authority, and
   refuses unresolved offload artifacts. Closed-payload validation rejects a
   malformed loss list, source identity mismatch, and invalid causality. Next
   compose this preparation with the physical transaction in a no-clobber
   migration fixture while retaining exact v1 rollback bytes.
   **Provider-free migration composition complete in Cycle 064:** one private
   method performs strict pure preparation followed by the existing transaction
   with `ExpectedRevision::Missing`. Exact publication stores the deterministic
   baseline bytes; repeat/already-present migration allocates no candidate;
   conflicting same-revision residue remains untouched; artifact refusal occurs
   before identity/effects; and an injected post-candidate cut reports the exact
   orphan while the borrowed v1 bytes and HEAD remain unchanged. The seam is
   still unmounted. Next assemble one new whole operation from neutral E02
   terminal/transcript/effect facts before any live save switches ownership.
   **Pure whole-operation assembly complete in Cycle 065:** the assembler first
   validates the canonical base, requires a fresh opaque revision and monotonic
   commit time, converts the neutral E02 terminal, derives after-HEAD solely
   from assistant disposition, and appends one bounded message/effect/artifact
   closure. Success/committed advances to its exact turn; a provider fault with
   discarded output records the operation but retains the prior selected HEAD.
   Missing artifacts, terminal/disposition disagreement, same revision, and
   backwards time refuse. The legacy baseline and lineage remain exact, while
   the old revision-bound compaction projection is cleared. Next compose an
   artifact-free assembled successor with the file transaction and stale/cut
   fixtures before physical artifact publication is designed.
   **Artifact-free successor transaction complete in Cycle 066:** the only
   cross-module operation-input constructor creates one user fact plus an
   optional assistant fact and no tools, retained effects, or artifacts. The
   composition also rejects any artifact-bearing base before fence/candidate.
   A confirmed update stores a closed payload whose parent, new revision, and
   selected HEAD equal the receipt; stale base allocates no candidate; a cut
   after candidate keeps the old HEAD and reports confirmed unselected residue;
   and a HEAD-directory sync cut exposes the new revision as published-
   unconfirmed. Next define descriptor-pinned immutable artifact installation
   and verification before widening this physical composition.
   **Physical immutable artifact closure complete in Cycle 067:** a portable
   session/artifact namespace and closed record bind identity, independent
   artifact schema, length, SHA-256, and media type. Private staged install is
   synced and no-overwrite; verified open is descriptor-pinned and exact.
   Missing, collision, future schema, physical retarget, content tamper, symlink,
   and target-sync cuts have explicit results, including preserved first bytes
   and installed-unconfirmed residue. Next compose canonical artifact facts with
   exact installed-record verification before candidate/HEAD publication.
   **Canonical artifact publication complete in Cycle 068:** one closed
   tool-call/result/effect operation binds its canonical artifact fact to the
   exact installed physical schema, identity, length, digest, and media type.
   Closure is verified before candidate allocation and rechecked after install
   before HEAD CAS. Missing, tampered, or mismatched closures are not published;
   the post-candidate recheck cut retains old HEAD plus exact confirmed residue.
   Next require verified artifact closure whenever canonical HEAD is opened.
   **Verified canonical HEAD open complete in Cycle 069:** the selected physical
   candidate is decoded and bound to its logical schema/store/session/revision/
   parent, then every artifact is reopened through the publication predicate.
   True HEAD absence alone returns no value. Invalid logical bytes, binding
   disagreement, missing closure, digest tamper, and an independently valid but
   unequal physical replacement all fail on the selected HEAD without fallback.
   Next project these private read failures into the neutral typed lookup state.
   **Neutral canonical lookup complete in Cycle 070:** verified `Present` remains
   paired with its exact decoded payload; true absence is `Missing`; malformed,
   identity, causality, reference, and digest evidence maps to exact `Corrupt`
   reasons; and future logical, artifact, and artifact-manifest physical schemas
   map to dimensioned `Unsupported`. Zero schemas remain malformed. Next type the
   earlier physical HEAD/revision envelope failures and split invalid-HEAD from
   other logical-causality validation before any production mounting.
   **Typed physical lookup envelope complete in Cycle 071:** descriptor-pinned
   HEAD/revision reads preserve missing-reference, malformed, identity, digest,
   and future-physical-schema evidence without parsing display text. A decoded
   HEAD carries its selected revision through every later failure. Schema zero
   remains malformed; future physical schemas remain `Unsupported`; an unknown
   selected logical HEAD is `InvalidHead`, while invalid operation ancestry is
   `BrokenCausality`. Every failure returns no payload, writes nothing, and does
   not scan an older candidate. Next verify exact retained revisions and bounded
   lineage without allowing a candidate or projection to become canonical HEAD.
   **Exact retained lineage complete in Cycle 072:** physical schema revision 2
   requires every successor manifest to name the SHA-256 of its exact verified
   parent manifest. Candidate installation refuses an absent/unverified parent;
   HEAD CAS rechecks that digest against the admitted selected parent. A bounded
   reader follows only this HEAD-to-root chain, validates every canonical value
   and artifact closure, ignores installed orphans, and returns no partial
   lineage for missing, retargeted, cyclic, or over-limit ancestry. Schema
   revision 1 is explicitly unsupported; production current-v1 files remain
   untouched. Next derive a read-only last-good recovery plan from the physical
   chain without publishing or silently changing HEAD.
   **Read-only logical recovery plan complete in Cycle 073:** the transaction
   reports `Missing`, `NotNeeded`, `Available`, or `Unavailable` without writing
   any file or changing canonical selection. For a current logically corrupt
   selected revision it follows only exact parent revision plus bound manifest
   digest, crosses additional corrupt ancestors, and returns the first revision
   whose logical value and artifact closure verify completely. Future selected
   schemas are never downgraded; ordinary lookup continues to report the corrupt
   selected HEAD. Next split verified manifest-envelope reading from canonical
   payload verification so a selected physical payload/digest fault can expose
   only its already-bound parent link to the same bounded planner.
   **Manifest-envelope recovery complete in Cycle 074:** HEAD reading first
   verifies the selected revision manifest and its HEAD-bound digest while
   retaining the descriptor-pinned revision directory. Canonical payload
   existence, regular-file shape, length, and digest remain a separate upgrade;
   the envelope carries no trusted payload. Recovery may cross missing or
   digest-broken selected and intermediate payloads only through captured exact
   parent-manifest links. An unverified manifest or future logical manifest
   stops traversal. Ordinary lookup still fails on the selected corruption and
   planning performs no write. Next define a pure new recovery revision with
   complete skipped-revision and unavailable-blocker evidence; recovery remains
   a new fenced publication, never an in-place HEAD pointer rollback.
   **Causal recovery assembly complete in Cycle 075:** canonical logical schema
   revision 2 owns a bounded recovery ledger. The planner records every corrupt
   selected/intermediate revision and typed reason in exact traversal order;
   unavailable results additionally identify the exact root exhaustion,
   missing/different digest, unsupported logical manifest, ancestor state, cycle, or
   traversal bound. Pure assembly validates the fully verified source, creates
   a distinct revision whose exact parent is the corrupt selected revision,
   copies source operations/artifacts/HEAD, clears stale compaction, and binds
   source/selection/skips/time. Recovery never makes an old revision HEAD.
   Next publish this value through envelope-only selected-parent admission,
   immutable install, writer-fenced CAS, and exact old-or-new receipts.
   **Fenced recovery publication complete in Cycle 076:** a private transaction
   acquires the session writer fence, re-admits the exact selected HEAD envelope,
   and retains both selected and verified-source manifest digests across plan
   and reopen. Physical candidate installation may bind that verified envelope
   without reading corrupt parent payload bytes; the recovery-only CAS rechecks
   the same HEAD envelope immediately before selecting the new causal child.
   Artifact closure is checked before and after candidate install. Unavailable
   ancestry allocates nothing; the candidate cut preserves corrupt HEAD plus an
   exact installed-unselected residue; a HEAD-sync cut exposes only the new
   revision as `PublishedUnconfirmed`; confirmed success returns exact
   base/new/HEAD/durability. Stale envelope CAS fails, and the verified old
   source is never selected directly. The path remains unmounted. Next define
   pure branch-at-committed-turn prefix, lineage, and artifact-closure assembly.
   **Pure branch assembly complete in Cycle 077:** the assembler accepts only an
   operation whose terminal committed an assistant boundary. It validates the
   complete source, truncates the exact chronological operation prefix through
   that turn, filters immutable artifact facts to IDs referenced by retained
   messages/effects, and creates a fresh destination session/revision with a
   missing parent. Fork lineage binds source store/session/revision/current HEAD
   and the branch turn; decoded payload validation requires fresh identities and
   branch turn equal to the committed selected HEAD. The historical creation
   anchor is retained so carried SHA-bound legacy evidence remains chronological;
   destination commit time is new and monotonic. Source compaction and recovery
   ledgers are cleared, and no authority state exists to copy. The next bounded
   steps are no-overwrite artifact copying and create-only publication cuts.
   **Verified branch artifact copying complete in Cycle 078:** the physical
   store first opens and verifies the immutable source manifest and content,
   then preserves schema, artifact identity, media type, and exact bytes while
   rewriting only the destination session identity through its existing atomic
   create-only installer. Same-session copy is rejected. Missing or tampered
   source leaves no destination; collision preserves the first exact artifact.
   The bounded review deliberately leaves multi-artifact rollback and branch
   candidate/HEAD publication to Cycle 079 under separate source/destination
   fences.
   **Create-only branch publication complete in Cycle 079:** one private
   transaction acquires both session fences in stable identity order, reopens
   and exactly compares the caller-observed source, proves destination HEAD,
   requested revision, and every target artifact absent, copies only assembled
   closure, installs the root candidate, and rechecks source plus destination
   closure before destination HEAD CAS. Stale source and known destination
   collisions are zero-copy. Artifact-copy, candidate, and closure-recheck cuts
   expose no destination HEAD and retain exact immutable residue; a HEAD-sync
   cut selects only the complete branch with published-unconfirmed durability.
   Next make those exact pre-HEAD residues safely resumable without accepting a
   name-only collision or overwriting any byte.
   **Exact branch-residue retry complete in Cycle 080:** artifact and candidate
   preflight distinguishes missing, exact, and conflicting records before new
   copying. Exact means the same store/session/revision/parent/schema, manifest
   digest, canonical bytes, and artifact fact. Adoption holds the destination
   fence, re-syncs target and owning namespaces, then reopens and compares again
   before source/closure rechecks and HEAD CAS. Confirmed and previously
   unconfirmed artifact-only or candidate-bearing cuts converge to the same
   branch. Malformed, byte-different, or tampered residue remains unselected and
   is never deleted or overwritten.
   **Pure mount admission complete in Cycle 081:** one path-free classifier
   admits only clean empty, current-v1-only, or canonical-only observations.
   Current-v1 malformed/identity/future-version evidence and canonical
   corrupt/future/conflict evidence never collapse to missing. A canonical
   tombstone blocks both empty and legacy routes, and valid dual presence is a
   separate ambiguity. Rejection retains both typed observations. No store is
   opened, created, migrated, selected, or written by this card.
   **Mutation-free observation complete in Cycle 082:** both representations
   are inspected relative to one already-pinned session root. Current-v1 open is
   no-follow, bounded, identity/version classified, and byte-preserving. The
   canonical store has a distinct open-existing-only constructor that requires
   its record and every namespace to pre-exist; it never creates, repairs,
   secures, syncs, stages, or locks. Absent canonical root is clean missing;
   partial, future-schema, wrong-store, corrupt selected state, and symlinked
   roots remain typed rejection. Valid canonical-only and dual-presence paths
   reuse the ordinary verified HEAD/closure lookup. No selected value is yet
   returned to production.
   **Exact single-pass selection complete in Cycle 083:** the same observation
   now retains exactly one admitted value. Current-v1 owns the parsed
   `SavedSession` and exact bounded source bytes; canonical owns the payload that
   passed ordinary HEAD, manifest, logical, identity, and artifact-closure
   verification. Empty has no value. Unavailable, tombstoned, and dual-present
   results expose only typed rejection. Tests change the backing current or
   canonical path after selection: the retained value remains exact and a new
   selection sees the fault. The canonical variant is boxed to keep the private
   enum bounded. Production projection and activation revalidation are still
   required before mounting.
   **Pure canonical resume projection complete in Cycle 084:** the derived
   current-v1-shaped value is bound to the exact canonical source revision and
   selected HEAD; that DTO is never itself HEAD evidence. Resume metadata and
   provider-neutral system/user/assistant/tool facts preserve exact order,
   tool-call arguments, and correlation IDs. Only the chronological operation
   ancestry of the selected HEAD enters model context. Discarded or otherwise
   unselected operations remain in canonical audit history and are named by the
   projection instead of silently reappearing as prompts. The fixed legacy loss
   manifest stays explicit. Any immutable artifact manifest yields a typed
   refusal because `SavedSession` has no lossless artifact-identity field. The
   function performs no I/O or authority restoration and remains unmounted;
   activation must next revalidate its revision/HEAD binding under the owning
   fence.
   **Dual-fenced canonical activation complete in Cycle 085:** the compatibility
   boundary owns the legacy session lease and then the canonical writer fence in
   one stable order, retaining both and the pinned physical store for the entire
   activation lifetime. After both exclusions are held it requires current-v1
   to remain absent, reruns ordinary canonical HEAD/payload/artifact verification,
   and compares the complete observed payload with the selected value. A newer
   HEAD, corrupt replacement, newly present legacy snapshot, missing/wrong store,
   or busy fence fails closed. Projection refusal happens before lock effects;
   successful and post-lock failure paths create only the existing bounded lock
   records and never change session bytes, HEAD, artifacts, or authority. This
   path remains private. Current-v1 activation must receive the same two-domain
   exclusion before production routing can change.
   **Dual-fenced current-v1 activation complete in Cycle 086:** after acquiring
   the legacy lease, activation creates the missing canonical lock domain only
   through the already-pinned session descriptor. It uses exclusive root
   creation; an existing or concurrently won root is adopted only through the
   complete mutation-free store verifier, so partial state is never filled in or
   repaired. With the canonical fence held, activation re-reads and byte-compares
   the exact selected current snapshot and requires canonical HEAD absence. A
   replaced valid snapshot, newly published HEAD, wrong store, partial namespace,
   or busy lock fails closed. Ancestor replacement cannot redirect creation.
   The returned owner retains both locks and the store; initialization changes
   only the bounded store/lock namespace, not session truth or authority. Next a
   total private owner must normalize both activation variants before routing.
   **Total activation owner complete in Cycle 087:** one dispatcher consumes
   every selection variant. Current and canonical successes are boxed beneath a
   single owner that retains their store and both OS exclusions; callers receive
   only borrowed resume data plus an exact store/session/source receipt. Current
   source is explicit; canonical source additionally carries revision and
   selected HEAD. An explicit snapshot clone remains data and cannot substitute
   for the owner or receipt. Empty and rejected selection evidence returns
   unchanged with zero effects, while current/canonical activation faults remain
   distinct nested errors. The next boundary must transfer this owner into a
   live session store whose save behavior is explicitly representation-aware.
   **Held-fence canonical save boundary complete in Cycle 088:** the total owner
   routes canonical operations through its retained logical base, pinned store,
   and already-owned writer fence; it never tries to reacquire that fence.
   Successor assembly and resume projection happen before effects. Admission
   then verifies the supplied fence identity, exact selected parent envelope,
   and complete retained logical value before allocating a candidate. Candidate
   installation binds the parent manifest digest and publication uses envelope
   CAS. Published and published-unconfirmed results advance the owner's base and
   projection; not-published cuts do not. Current-v1 returns a typed requirement
   for its snapshot writer, and artifact-bearing projection refuses with zero
   allocation. The next boundary must implement exact current-v1 save ownership
   before production routing can consume the total activation.
4. Add explicit v1-to-v2 migration, retained revision generations, projection
   rebuild, and verified last-good recovery.
5. Add branch-at-turn, artifact closure, export/import, and full K01-K12.
6. Mount one genuinely independent compiled store adapter before X005 can pass.

Cycle 050 accepts only this ownership and fixture contract. It does not claim
E03-S01 complete until the neutral value vocabulary and ambiguity checks land,
and it does not change the X005 failure score.
