# Kitsu Current Architecture

Status: source-audited from runtime-source pin
`986fba971ec99c9af5dcd8f69a48f507aae486af`, with accepted implementation
updates through Cycle 092.

The exact provenance and frozen research-input hashes are in the
[post-E01 evidence baseline](post-e01-evidence-baseline.md). This document
supersedes the frozen research architecture for current planning; it does not
rewrite or invalidate that historical source audit.

## Ownership and dependency shape

```text
                         kitsu binary
 config / composition / sessions / carriers / JSONL / skills / subagents
          |                 |                 |               |
          v                 v                 v               v
     kitsu-core       kitsu-providers    kitsu-tools     kitsu-render
   agent/context       one neutral       registry +      terminal +
       loop              trait          authority/tools  JSONL writer

kitsu-core  -> kitsu-providers + kitsu-tools + kitsu-render
kitsu-tools -> kitsu-providers::ToolDescriptor
```

The five-crate layout is unchanged. `kitsu-core` still imports the concrete
tool registry and renderer (`crates/kitsu-core/Cargo.toml`), so the current
provider trait is a useful compiled seam but the core is not yet a host-neutral
kernel.

| Area | Current owner/behavior | Evidence at the pin | Consequence |
|---|---|---|---|
| Product composition | The binary owns config, auth selection, provider construction, tool/skill/profile registries, sessions, carriers, JSONL adaptation, and user interaction. | `crates/kitsu/src/main.rs:1231,4022,7875-7970,13041`; 21,062 lines | Product invariants have one ultimate owner, but extraction must be fixture-driven because lifecycle and presentation are interleaved. |
| Agent loop | Core owns bounded context/rounds, physical provider attempts, sequential tool execution, finalization, cancellation, and runtime events. The neutral run module owns identity/outcome values and a monotonic lifecycle that rejects duplicate/out-of-order/post-terminal transitions. `AuthorityInputClosed` is wired as a loop interruption; all inference carriers publish through one process-scoped `RunLifecycle`. The interactive host aggregates successful, failed, cancelled, interrupted, retried, queued, and detached operations rather than using its last turn. Cleanup derives nonzero residue from actual join statuses, the active-session receipt separates addressability from preserved/committed/failed persistence, and an unproven fallback is explicitly `NotObserved`. One evaluator maps immutable publication to exact carrier exit. | `crates/kitsu-core/src/orchestrator.rs`; `crates/kitsu-core/src/run.rs`; `crates/kitsu/src/process_exit.rs`; `crates/kitsu/src/main.rs` | E02 is accepted without changing JSONL v1 or session files. `NotObserved` prevents an unexpected fallback from claiming clean success. X004 remains non-replaceable because no alternate runtime adapter runs the suite. |
| Provider port | `Provider` exposes health, chat stream, attempt telemetry, and model catalog operations using Kitsu request/event types. Provider-neutral declarations, immutable snapshots, current owners, model-bound handles, and catalog-only handles fence physical entry. Each snapshot/handle can project one versioned, bounded, secret-free receipt containing route identity, optional model, auth-source class, exact time/retry/catalog policy, bound-generation retry, current-generation semantic replay, and disabled fallback. `/status`, JSONL run start, and opted-in doctor expose exactly this projection. | `crates/kitsu-providers/src/lib.rs`; `crates/kitsu-providers/src/route.rs`; `crates/kitsu-providers/tests/provider_route_receipt.rs`; `crates/kitsu/src/main.rs` | This compiled seam has truthful current-generation admission and diagnostics. Compatible Chat and native Messages pass one product-level success/fault/cancellation/status/doctor/offline suite, so X001 is replaceable without claiming a runtime ABI. |
| Provider construction | A closed `adapter = "compatible-chat" | "anthropic-messages"` field selects one compiled implementation behind `CompiledProvider`; flavor remains display/backward-compatibility metadata and legacy flavor-only profiles retain compatible Chat. Chat and inference doctor use model-bound immutable generations. Compatible top-level `models` and catalog doctor use catalog-only generations; native catalog selection fails locally before transport. Both adapters pass generic route-handle and product-surface harnesses, and a live CLI fixture proves exact native request/header selection. | `crates/kitsu-providers/src/compiled.rs`; `crates/kitsu-providers/src/openai_compat.rs`; `crates/kitsu-providers/src/anthropic_messages.rs`; `crates/kitsu-providers/tests/provider_route_conformance.rs`; `crates/kitsu/src/main.rs`; `crates/kitsu/tests/cli.rs` | E04 is accepted without runtime loading, executable config, or flavor inference. New profiles behind these adapters remain data-only; a new dialect requires compiled code and the same conformance gates. |
| Provider resilience | Both physical adapters use one private retry owner for transient 408/429/502-504/520-524/529 classification, capped `Retry-After` plus jitter, one end-to-end deadline, and send/body/backoff cancellation guards; generic 500 and admitted body failures stay final. Nested `providers[]` context discovery and a minimum floor are supported. Every production chat/catalog/compaction/subagent/doctor send is bound to one frozen auth/model/policy generation, and JSONL attempt evidence carries its generation where emitted. Native Messages requires explicit `message_stop`; supplies bounded `max_tokens = 4096` when the CLI option is absent and rejects explicit zero pre-send; bounds HTTP/event bodies and tool arguments; and passes malformed, midstream, timeout, cancellation, retry, and common conformance fixtures. | `crates/kitsu-providers/src/retry.rs`; `crates/kitsu-providers/src/compiled.rs`; `crates/kitsu-providers/src/openai_compat.rs`; `crates/kitsu-providers/src/anthropic_messages.rs`; provider/core/CLI tests | E04 and X001 are accepted. Catalog/account reporting remain truthfully unavailable on native Messages rather than emulated through another wire. |
| Tool registry | Descriptor-free and described admission share one name key, return `Result`, reject same/cross-path duplicates before per-entry mutation, and preserve the first entry. Builtin composition propagates a typed CLI user error. | `crates/kitsu-tools/src/lib.rs:69-121`; `crates/kitsu-tools/src/builtins/mod.rs`; `crates/kitsu/src/main.rs:4022-4057` | A001 is complete for deterministic per-entry admission. Batch transactionality, provenance, source identity, and lifecycle are not implied. |
| Builtin tools | Read/write/command capability is assembled by omission under separate permissions. Bounded search, symbol, outline, manifest, document, image-metadata, audit, and secret-scan readers exist. | `crates/kitsu-tools/src/builtins/mod.rs:23-137`; builtin tests | The structural-navigation gap is narrow now; semantic reference-graph ranking remains absent. |
| Command execution | Exact argv/cwd authority remains separate from filesystem roots. Bubblewrap containment, cleared/projected environment, bounds, process groups, cancellation kill, and reaping remain one-shot command behavior. | `crates/kitsu-tools/src/builtins/command.rs`; 4,900 lines | E06 may extract private lifecycle mechanisms, but a long-lived MCP child needs its own bidirectional supervisor and tests. Process separation alone is not containment. |
| Filesystem mutation | Canonical/descriptor checks, atomic install, exact preconditions, journaling, verified effects, and conflict-aware undo remain Kitsu-owned. | `crates/kitsu-tools/src/permissions.rs`; `crates/kitsu-tools/src/builtins/fs.rs` | Semantic context or external sources must use these authorization/mutation primitives, never replace them with fuzzy or in-place writes. |
| Skills | Local bounded skills are discovered as untrusted instructions/resources with path/provenance checks; effects still cross ordinary tools. | `crates/kitsu/src/skills.rs:359,824-900,1514` | This is a declarative data plane, not executable configuration or a plugin loader. |
| Events/JSONL | Core runtime events are projected by the CLI into versioned `kitsu.chat.v1`; the writer serializes whole records, flushes, and fails terminally. One neutral `RunSpec` supplies JSONL, private generations, and human completion. Human timing and JSONL `final_summary` carry the same exact run outcome/assistant tuple. Every `run_started` now adds the complete provider-route receipt while attempts carry the correlated generation; the new field is optional in the schema so legacy records remain valid. | `crates/kitsu-core/src/run.rs`; `crates/kitsu/src/main.rs`; `crates/kitsu-render/src/lib.rs`; `crates/kitsu-render/src/jsonl.rs`; `docs/schemas/kitsu-chat-v1.schema.json` | E02 is accepted for the default runtime and E04's two-adapter product suite proves receipt/attempt parity. Alternate host/runtime conformance remains open. |
| Sessions | Production v1 still atomically stores one current `SavedSession` message vector plus gateway/model/project context. Fork copies the complete snapshot/artifact tree to a fresh identity without grants. The E03 ADR assigns target ownership; `kitsu-core::session` supplies the bounded neutral revision/HEAD contract and exact expected-revision admission. Public current-v1 reads and active save cross private transitional ports with unchanged bytes. Provider-free K04/K07/K08 fixtures pin logical publication, durability, stale conflict, and fencing. Private binary modules define the disjoint physical-v1 store, closed whole-operation canonical payload, and one transaction that validates/encodes before effects, pre-admits under the session fence, installs immutable bytes, rechecks artifact closure before HEAD CAS, and reports exact publication plus candidate residue truth. Strict current-v1 input can become only a non-branchable `Genesis` legacy baseline; pure successor assembly consumes neutral terminal/closed facts. Artifact-free and one complete artifact-bearing tool/effect successor pass fenced publication. The physical store owns session-scoped immutable artifacts plus revision manifests. Physical schema revision 2 makes every successor bind its exact verified parent-manifest digest; candidate install refuses an absent parent and HEAD CAS rechecks the link. Canonical publication and selected-HEAD open use one artifact predicate. A private lookup envelope pairs verified payload only with neutral `Present`; typed physical/logical/artifact faults remain payload-free and never fall back. A 4,096-revision bounded reader follows only the digest-bound HEAD-to-root chain, ignores unselected candidates, and fails on missing, retargeted, cyclic, or over-limit ancestry. Physical reads now separate a descriptor-pinned HEAD-bound manifest envelope from canonical payload verification; that envelope contains no trusted payload. The read-only recovery planner walks only bound envelopes, retains every skipped corrupt revision in selected-to-source order, and reports an exact blocker when no source closes. Canonical logical schema revision 2 owns the bounded recovery ledger. Pure assembly copies a fully verified source value into a fresh revision whose exact parent is the corrupt selected revision, clears stale compaction, and binds source revision/HEAD, selection, skip reasons, and commit time. A private recovery transaction holds the writer fence while it rechecks exact selected/source manifest digests, installs the assembled child against the selected envelope without reading corrupt parent bytes, rechecks artifacts, and CASes that envelope to the new revision. Candidate and HEAD-sync cuts report installed-unselected or published-unconfirmed truth; stale envelope CAS fails. A pure branch assembler now validates a committed turn, creates a fresh create-only destination revision, copies its exact chronological operation prefix and only referenced artifact facts, binds full source/branch lineage, preserves the historical creation anchor, and clears source-only compaction/recovery state. No authority state exists in the payload. | `crates/kitsu/src/session.rs`; `crates/kitsu/src/session/physical.rs`; `crates/kitsu/src/session/canonical.rs`; `crates/kitsu/src/session/transaction.rs`; `crates/kitsu-core/src/session.rs`; `crates/kitsu-core/tests/session_k04.rs`; `crates/kitsu-core/tests/session_k07.rs`; `crates/kitsu-core/tests/session_k08.rs`; `scripts/check-session-boundary.sh`; `docs/roadmap/e03-canonical-session-adr.md` | All new helpers remain deliberately unmounted, so production current-v1 remains authoritative and rollback. Branch artifact copying/publication, production mounting, complete K01-K12, and adapter parity remain open; X005 still fails. |
| External tools/hosts | No MCP dependency/client, long-lived child supervisor, bidirectional host adapter, daemon, or second principal is compiled. JSONL is one-way output. | workspace manifests and source search | X002-X004 remain failed; absence preserves the disabled path and avoids ambient authority. |

## What changed from the frozen research pin

- The 67 committed base delta materially improves retry classification,
  context-window discovery, timeout transcript truth, bounded context offload,
  command-containment portability, and read/code-navigation breadth.
- E01 closes the loopback Ollama `:latest` diagnostic defect and the bounded
  deterministic registry-admission transfer, including CLI propagation and CI
  coupling guards.
- E02 makes seven inference carriers publish one causal outcome and exact
  terminal phases; E04-S01/S02/S03 pin two wire shapes and bind every current
  production provider send to an immutable compatible-Chat or catalog-only
  generation; E04-S04 through Cycle 046 proves and explicitly composes a native
  Messages generation with frozen auth, truthful capabilities, bounded retry
  policy, and legacy-safe selection. Cycle 049's shared product harness closes
  the remaining success/fault/cancellation/status/doctor conformance cell.
- E03-S01 defines and compiles the store-neutral canonical session vocabulary;
  Cycles 052-053 route current-v1 read/list and active save through private ports
  without changing bytes, selectors, corruption handling, or physical commit
  evidence. Cycle 054 adds exact expected-revision admission and proves every
  stale/missing mismatch remains zero-effect before a store is entered. Cycle
  055 executes K08's create/update/stale/retry sequence in a provider-free
  in-memory fixture without changing production v1. Cycle 056 shares that
  fixture across K04 cuts and preserves exact old/new visibility plus durability
  truth at the logical publication boundary. Cycle 057 adds K07 session-bound
  fence generations and proves wrong/lost/replaced tokens are zero-effect while
  an independent session continues. Cycle 058 compiles the disjoint physical-v1
  paths and records, solving dot-component, case-folding, length, no-clobber,
  version, digest, and identity-mismatch ambiguity without mounting migration.
  Cycles 059-060 add descriptor-pinned immutable candidate installation and
  writer-fenced exact-revision HEAD CAS. Cycle 061 adds a separately versioned,
  closed canonical payload for whole terminal operations, exact artifact/effect
  closure, and compaction ancestry/generations; strict current-v1 input retains
  its exact source digest and reports unavailable terminal/artifact facts rather
  than inferring them. Cycle 062 composes deterministic payload bytes, immutable
  candidate installation, writer-fenced pre-admission/recheck, HEAD publication,
  and exact publication/residue receipts without mounting production behavior.
  Cycle 063 makes the current-v1 policy explicit: exact legacy transcript bytes
  are digest-bound into a non-branchable `Genesis` baseline with a fixed loss
  manifest, while unresolved offload artifacts and malformed causality refuse
  migration. No authority is restored and no production caller is mounted.
  Cycle 064 composes that pure preparation with the existing file transaction;
  migration is create-only, repeat/existing HEAD conflicts are zero-allocation,
  candidate collisions preserve the first bytes, and every tested cut leaves
  caller-owned v1 bytes unchanged.
  Cycle 065 adds a pure new-operation successor assembler. A neutral terminal
  exclusively determines HEAD advance/retention; exact messages, retained
  effects, and immutable artifact references must close; revision/time lineage
  advances; and the prior compaction projection is never relabeled as current.
  Cycle 066 sends only an artifact-free assembled text successor through the
  fenced transaction. Confirmed update bytes/receipt, stale zero-allocation,
  installed-unselected residue, new-visible unconfirmed HEAD, and artifact-base
  pre-effect refusal are all explicit.
  Cycle 067 adds session-scoped immutable physical artifacts: portable encoded
  paths, a closed manifest, staged no-overwrite install, exact descriptor-pinned
  reload, and collision/tamper/symlink/durability failure truth. Cycle 068 binds
  every canonical artifact fact to exact installed schema, identity, length,
  digest, and media type before candidate allocation and rechecks the closure
  after installation before HEAD CAS. One complete tool/result/effect operation
  now publishes with old-or-new cut truth. Cycle 069 opens the selected canonical
  HEAD through exact logical/physical binding and the same artifact predicate;
  missing, malformed, tampered, and self-consistent mismatched closure is typed
  read failure and never implicit fallback. Cycle 070 pairs the verified value
  with the neutral lookup state and classifies current malformed, identity,
  causality, reference, digest, and supported-dimension future-schema evidence.
  Cycle 071 carries the same typed evidence through physical HEAD/revision open,
  preserves decoded revision identity, and separates invalid selected HEAD from
  broken operation causality without message parsing.
  Cycle 072 advances the private physical schema to revision 2, binds every
  successor to its exact parent-manifest digest, and verifies a bounded selected
  lineage without allowing unselected candidates to become HEAD. Cycle 073 adds
  a read-only last-good plan over that chain: it crosses logically corrupt
  ancestors, selects only a fully verified revision, refuses future schemas, and
  never replaces or masks the selected HEAD. Cycle 074 splits verified manifest
  envelopes from payload verification, allowing the same plan to cross missing
  or digest-broken payloads while refusing an unverified or future manifest.
  Cycle 075 advances the canonical logical schema to revision 2, records exact
  ordered skip/blocker evidence, and assembles recovery as a fresh causal child
  of the corrupt selected revision from only a fully verified source value.
  Cycle 076 publishes that child privately under the existing writer fence and
  exact selected envelope, preserving installed-unselected and
  published-unconfirmed cut truth without ever selecting the old source.
  Cycle 077 adds pure branch-at-committed-turn assembly with exact prefix,
  lineage, referenced-artifact filtering, and fresh destination identity.
  Cycle 078 adds verified destination-scoped artifact copying: source bytes and
  metadata must verify before any effect, destination publication is atomic and
  no-overwrite, and missing/tampered/same-session/collision cases fail closed.
  Cycle 079 composes branch assembly and artifact copying under deterministically
  ordered source/destination fences. It rechecks exact source and destination
  closure before create-only HEAD CAS; known stale/collision cases are zero-copy,
  pre-HEAD cuts expose no destination HEAD and report exact immutable residue,
  and HEAD-sync uncertainty exposes only the complete new branch.
  Cycle 080 allows retry only from exact residue for the same assembled branch.
  Existing artifacts/candidates must match every identity, digest, schema,
  parent, and byte; their owning namespaces are re-synced and records reopened
  before ordinary source/closure/HEAD rechecks. Tampered residue remains blocked.
  Cycle 081 adds a pure production-mount admission oracle. From an already-typed
  current-v1 observation and neutral canonical lookup it admits only empty,
  current-v1-only, or canonical-only state. Unavailable/future/conflict state,
  a canonical tombstone, and dual presence remain distinct rejections carrying
  both observations. The oracle owns no path or effect and remains unmounted.
  Cycle 082 feeds it using one descriptor-pinned session root. Current-v1 file
  observation is bounded and byte-preserving. Canonical observation has a
  separate open-existing-only path: a missing root is ordinary absence, while
  partial, wrong-store, future-schema, corrupt-HEAD, and symlinked layouts are
  typed refusal. It creates no namespace, record, permission change, sync,
  repair, staging entry, or lock. Production read/write routing remains unchanged.
  Cycle 083 makes selection single-pass. A current-only result owns its parsed
  snapshot plus exact observed bytes; a canonical-only result owns the already-
  verified logical payload. Empty owns nothing, and rejection owns only typed
  admission evidence, so dual/corrupt state cannot leak a selectable value.
  Later pathname mutation does not rewrite the selection, while a new selection
  sees the new fault. The large canonical value is boxed privately. Production
  activation revalidation and routing remain unchanged. Cycle 084 adds the pure
  canonical resume projection. It first revalidates the closed payload, binds
  its result to exact source revision and selected HEAD, preserves resume
  metadata and provider-neutral message/tool-call facts, and appends only the
  chronological operations on selected-HEAD ancestry. Discarded or otherwise
  unselected operations remain canonical audit facts and are reported as
  omitted rather than reintroduced to model context. Fixed legacy losses stay
  explicit. Artifact-bearing history is rejected because `SavedSession` cannot
  preserve immutable artifact identity. The projection reads or writes nothing,
  grants no authority, and is not yet mounted. Cycle 085 adds a private
  canonical activation owner. It acquires the legacy session lease followed by
  the canonical writer fence, retains both plus the descriptor-pinned store,
  and only then rechecks current-v1 absence and the ordinary verified canonical
  HEAD/payload/artifact lookup. Exact source revision, selected HEAD, and full
  logical payload must still equal the selection. A newly appeared current-v1
  file, newer HEAD, corrupt/replaced payload, busy fence, missing store, or
  identity mismatch refuses activation. Only bounded existing lock-file effects
  occur; no session payload, HEAD, artifact, or authority changes. Production
  routing remains unchanged. Cycle 086 supplies current-v1 activation with the
  same lock order. While the legacy lease is held, it initializes the missing
  canonical namespace relative to the pinned session descriptor, or adopts only
  a fully valid existing store, then acquires the canonical fence. It re-reads
  and byte-compares the selected current snapshot and requires canonical HEAD to
  remain absent. A valid byte replacement, published canonical HEAD, wrong
  store, busy fence, or partial concurrent namespace refuses. Exclusive root
  creation plus strict reopen prevents fail-open repair, and an ancestor-swap
  fixture proves initialization cannot be redirected. Store/lock initialization
  is the only effect; current bytes and canonical HEAD remain unchanged. The
  private variants are not yet a production activation owner. Cycle 087 adds
  that total private owner and dispatcher. Current and canonical concrete
  activations are boxed beneath one lifetime owner; its resume snapshot is
  borrowed and an explicit clone remains data, never activation or HEAD
  evidence. A receipt binds exact store/session and current-v1 or canonical
  revision/selected-HEAD source. Empty and rejected selection evidence returns
  unchanged without locks or store creation; concrete activation faults remain
  nested and typed. Both lock domains remain held until the total owner drops.
  Cycle 088 gives that owner a private representation-aware save boundary.
  Canonical activation retains the exact selected logical value, assembles and
  projects its successor before effects, and then reuses its owned writer fence.
  The transaction reverifies the retained value, binds the new manifest to the
  selected parent-manifest digest, and publishes with exact-envelope CAS. A
  successful or published-unconfirmed result advances the retained base and
  borrowed resume projection; any not-published result leaves both unchanged.
  Wrong fences, stale HEAD, a different valid value under the same revision,
  and artifact-bearing projection refuse before candidate allocation. The
  current-v1 variant returns a typed snapshot-save requirement instead of
  creating canonical state. Cycle 089 completes the other representation-aware
  half. Current-v1 activation retains a cloned pinned session-directory
  descriptor, validates stable version/session/creation identity, and encodes a
  successor before effects. It rechecks exact retained source bytes and
  canonical-HEAD absence both before staging and after temporary-file sync,
  immediately before descriptor-relative rename. Confirmed and
  installed-unconfirmed writes advance retained state; invalid candidates and
  pre-install races do not. A directory-sync cut therefore reports a durability
  warning without misrepresenting the installed value, while ancestor
  replacement cannot retarget publication. Canonical activation rejects this
  snapshot operation without effects. Cycle 090 moves the total activation
  behind one non-cloneable private live-store owner. Callers borrow its resume
  snapshot and source receipt and submit one closed current-snapshot or
  canonical-operation save intent. Receipts preserve the representation; a
  mismatched intent is effect-free. The wrapper retains both fences across
  successful saves and releases them only on drop, and exposes no activation
  extractor. Cycle 091 moves the production active lifetime behind one
  non-cloneable `ActiveSessionStore`. Autosave, required save, clear/reset,
  startup and in-process resume, and artifact routing own that type; its sole
  implementation delegates exactly to `SessionStore`, creates no canonical
  namespace, and preserves current-v1 bytes, installed-inode evidence, leases,
  names, and reset behavior. Catalog/list/export/archive/delete remain static
  lifecycle operations rather than active save authority. At the Cycle 091
  boundary, a stable explicit local canonical store identity and production
  mount routing remained open. Cycle 092 supplies the former as an unmounted,
  owner-private schema-1
  composition record beneath the pinned sessions directory. It mints a bounded
  opaque `StoreId`, installs create-only, adopts only a valid concurrent winner,
  and distinguishes confirmed from installed-unconfirmed creation. Malformed,
  future, unknown-field, invalid, symlinked, permissive, or multiply linked
  records fail closed without repair; ancestor replacement cannot redirect the
  write. Cycle 093 binds that record through one private local live-store
  factory. With no identity, an empty/unavailable session and any orphan
  physical namespace fail before publication. An eligible current-v1 selection
  acquires the identity, then the same exact `StoreId` drives a fresh full
  selection and total activation. The returned non-cloneable owner retains the
  identity receipt beside the dual-fenced live store and exposes only borrowed
  identity, activation, and session evidence plus the closed save API. Existing
  identity/physical mismatches and corrupt identity bytes are never repaired;
  exact create, save, drop, and reopen preserve one binding. Product persistence
  remains current-v1 because no product constructor calls this factory.
- These changes close X001. X002-X005 remain failed until their named default
  and independent implementations execute complete conformance suites.

## Current extension planes

| Need | Current mechanism | Trust/process boundary | Status |
|---|---|---|---|
| Native behavior | Compiled Rust modules registered by product composition | Same process, binary authority | Retain; no dylib/runtime loader |
| Static instructions/resources | Local skills | Untrusted data; no inherent effect authority | Retain and reuse provenance rules |
| Provider dialect | Compiled `Provider` trait, compatible Chat and native Messages adapters | Same process; network/auth provider-owned | X001 replaceable; closed compiled registration only |
| External runtime tools | None | None | E06 experiment only after E02 lifecycle gates |
| Host/UI protocol | CLI carriers plus one-way JSONL | Consumer cannot service synchronous calls | No named second host; X003/X004 fail |
| Persistence adapter | Private file snapshot implementation | Local private state | No port/revision/HEAD conformance; X005 fails |

## Three spines and extraction order

1. E02 first establishes causal run identity/outcome and monotonic terminal
   phases with provider-free fault fixtures.
2. E04 binds provider/auth/retry/catalog/compaction work to immutable route
   generations and proves one distinct compiled wire.
3. E03 makes one canonical session revision and logical HEAD authoritative.
4. E06 extracts a bounded child lifecycle owner and evaluates tools-only stdio
   MCP, disabled by default.

Semantic context, onboarding, multimodal input, and any additional host consume
those spines. They do not create alternate authority, mutation, session, or
terminal systems.

## Current non-claims

- A trait or two flavor labels alone do not make providers replaceable; X001's
  executed two-adapter product suite does.
- Deterministic registry admission does not make external tool sources safe.
- JSONL does not constitute a bidirectional host or shared run contract.
- Atomic snapshots and whole-history fork do not constitute canonical HEAD.
- Search/symbol/outline tools do not constitute semantic repository mapping.
- Process separation, user approval, idle, or EOF do not prove containment or
  terminal success.
