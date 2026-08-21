# Kitsu Current Architecture

Status: source-audited from runtime-source pin
`986fba971ec99c9af5dcd8f69a48f507aae486af`, with accepted implementation
updates through Cycle 134 plus the explicit Cycle 129 quarantine repair.

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
| Guided setup | A private transaction normalizes bounded non-secret local/hosted/custom proposals, requires explicit consent before effect-capable directives, and owns acquire/validate/create-new/rollback receipts. A private Linux publication adapter renders parseable secret-free TOML, performs descriptor-relative same-directory create-new installation with explicit sync truth, and retains exact concrete effects or typed cleanup residue. Its pinned-parent advisory lock serializes cooperating Kitsu publishers, while exact receipt-fd/name classification preserves unknown or displaced ownership. One private coordinator composes those owners for an absolute local, credential-free, offline target. A pure picker retains bounded catalog provenance/uncertainty and binds proposals to exact typed profile/adapter/wire namespaces; one selected wrapper admits only explicit exact-namespace proposals and retains their complete truth through rejection, completion, and recovery. | `crates/kitsu/src/config/onboarding.rs`; `crates/kitsu/src/config/publication.rs`; `crates/kitsu/src/config/local_setup.rs`; `crates/kitsu/src/config/model_picker.rs` | Local transaction/publication/explicit-selection composition is proven but unmounted. The advisory lock is not conditional unlink or containment against noncooperating/privileged same-UID writers. No UI, credential adapter, provider/catalog effect, config activation, first-success flow, hosted/custom coordinator, or non-Linux publisher exists. |
| Agent loop | Core owns bounded context/rounds, physical provider attempts, sequential tool execution, finalization, cancellation, and runtime events. The neutral run module owns identity/outcome values and a monotonic lifecycle that rejects duplicate/out-of-order/post-terminal transitions. `AuthorityInputClosed` is wired as a loop interruption; all inference carriers publish through one process-scoped `RunLifecycle`. The interactive host aggregates successful, failed, cancelled, interrupted, retried, queued, and detached operations rather than using its last turn. Cleanup derives nonzero residue from actual join statuses, the active-session receipt separates addressability from preserved/committed/failed persistence, and an unproven fallback is explicitly `NotObserved`. One evaluator maps immutable publication to exact carrier exit. | `crates/kitsu-core/src/orchestrator.rs`; `crates/kitsu-core/src/run.rs`; `crates/kitsu/src/process_exit.rs`; `crates/kitsu/src/main.rs` | E02 is accepted without changing JSONL v1 or session files. `NotObserved` prevents an unexpected fallback from claiming clean success. X004 remains non-replaceable because no alternate runtime adapter runs the suite. |
| Provider port | `Provider` exposes health, chat stream, attempt telemetry, and model catalog operations using Kitsu request/event types. Provider-neutral declarations, immutable snapshots, current owners, model-bound handles, and catalog-only handles fence physical entry. Each snapshot/handle can project one versioned, bounded, secret-free receipt containing route identity, optional model, auth-source class, exact time/retry/catalog policy, bound-generation retry, current-generation semantic replay, and disabled fallback. `/status`, JSONL run start, and opted-in doctor expose exactly this projection. | `crates/kitsu-providers/src/lib.rs`; `crates/kitsu-providers/src/route.rs`; `crates/kitsu-providers/tests/provider_route_receipt.rs`; `crates/kitsu/src/main.rs` | This compiled seam has truthful current-generation admission and diagnostics. Compatible Chat and native Messages pass one product-level success/fault/cancellation/status/doctor/offline suite, so X001 is replaceable without claiming a runtime ABI. |
| Provider construction | A closed `adapter = "compatible-chat" | "anthropic-messages"` field selects one compiled implementation behind `CompiledProvider`; flavor remains display/backward-compatibility metadata and legacy flavor-only profiles retain compatible Chat. Chat and inference doctor use model-bound immutable generations. Compatible top-level `models` and catalog doctor use catalog-only generations; native catalog selection fails locally before transport. Both adapters pass generic route-handle and product-surface harnesses, and a live CLI fixture proves exact native request/header selection. | `crates/kitsu-providers/src/compiled.rs`; `crates/kitsu-providers/src/openai_compat.rs`; `crates/kitsu-providers/src/anthropic_messages.rs`; `crates/kitsu-providers/tests/provider_route_conformance.rs`; `crates/kitsu/src/main.rs`; `crates/kitsu/tests/cli.rs` | E04 is accepted without runtime loading, executable config, or flavor inference. New profiles behind these adapters remain data-only; a new dialect requires compiled code and the same conformance gates. |
| Provider resilience | Both physical adapters use one private retry owner for transient 408/429/502-504/520-524/529 classification, capped `Retry-After` plus jitter, one end-to-end deadline, and send/body/backoff cancellation guards; generic 500 and admitted body failures stay final. Nested `providers[]` context discovery and a minimum floor are supported. Every production chat/catalog/compaction/subagent/doctor send is bound to one frozen auth/model/policy generation, and JSONL attempt evidence carries its generation where emitted. Native Messages requires explicit `message_stop`; supplies bounded `max_tokens = 4096` when the CLI option is absent and rejects explicit zero pre-send; bounds HTTP/event bodies and tool arguments; and passes malformed, midstream, timeout, cancellation, retry, and common conformance fixtures. | `crates/kitsu-providers/src/retry.rs`; `crates/kitsu-providers/src/compiled.rs`; `crates/kitsu-providers/src/openai_compat.rs`; `crates/kitsu-providers/src/anthropic_messages.rs`; provider/core/CLI tests | E04 and X001 are accepted. Catalog/account reporting remain truthfully unavailable on native Messages rather than emulated through another wire. |
| Tool registry | Descriptor-free and described admission share one name key, return `Result`, reject same/cross-path duplicates before per-entry mutation, and preserve the first entry. Provider requests freeze exact descriptors and handler identities. A separate private external path validates source/incarnation/revision/tool identity and descriptor fingerprints, publishes one whole snapshot through exact monotonic fences, and binds one echo handler only to the current full identity/fingerprint. Its default-disabled declaration requires separate exact host and caller decisions before paired grants exist. Builtin composition propagates a typed CLI user error. | `crates/kitsu-tools/src/lib.rs`; `crates/kitsu-tools/src/external/identity.rs`; `crates/kitsu-tools/src/external/publication.rs`; `crates/kitsu-tools/src/external/source.rs`; `crates/kitsu-tools/src/external/handler.rs`; `crates/kitsu-tools/src/builtins/mod.rs`; `crates/kitsu/src/main.rs` | A001 remains complete for deterministic builtin admission. External admission/handler paths are provider-free, private, and unmounted; product authority derivation, registry/request inventory, configuration, and routing remain absent. |
| Builtin tools | Read/write/command capability is assembled by omission under separate permissions. Bounded search, symbol, outline, manifest, document, image-metadata, audit, and secret-scan readers exist. A fixed-corpus harness records the current-tool baseline, a threshold-backed selector comparison, and denied/replaced/revoked authority oracles. The measured selector is now a compiled but unmounted read-only contributor: discovery metadata ranks only, while current permission-gated reads reconstruct all emitted evidence. | `crates/kitsu-tools/src/builtins/mod.rs:23-137`; `crates/kitsu-tools/src/semantic.rs`; `crates/kitsu-tools/tests/semantic_evidence.rs`; `crates/kitsu-tools/tests/structural_context.rs`; builtin tests | The structural-navigation gap is measured on one tiny corpus and its authority/staleness experiment passes without stale or denied leakage. No registry, configuration, provider, session, cache, automatic context, or user-visible invocation mounts the contributor. |
| Command execution | Exact argv/cwd authority remains separate from filesystem roots. Bubblewrap containment, cleared/projected environment, bounds, process groups, cancellation kill, and reaping remain one-shot command behavior. The private external-child proof binds exact phase/I/O/environment/network/group policy plus bounded non-overlapping RO/RW filesystem-content roots. Its Linux x86-64 backend executes a capped, hashed, fully sealed native ELF memfd through raw `execveat`, applies descriptor-pinned cwd/roots through Landlock ABI>=3, and retains parent-built seccomp, bounded pipes, real clocks, group kill/direct wait/group-ESRCH, exit/EOF/crash truth, and one strictly serial multi-frame owner. | `crates/kitsu-tools/src/builtins/command.rs`; `crates/kitsu-tools/src/external/authority.rs`; `crates/kitsu-tools/src/external/supervisor.rs`; `crates/kitsu-tools/src/external/backend.rs`; `crates/kitsu-tools/src/external/serial.rs` | The external backend remains private, unmounted, Linux x86-64, and one process at a time. Landlock is content access control, not mount isolation/path invisibility/stat/access/metadata or host signal/ptrace confinement; this is neither a general sandbox nor X002 acceptance. Private MCP/handler composition exists, but product authority and routing remain required. |
| Filesystem mutation | Canonical/descriptor checks, atomic install, exact preconditions, journaling, verified effects, and conflict-aware undo remain Kitsu-owned. | `crates/kitsu-tools/src/permissions.rs`; `crates/kitsu-tools/src/builtins/fs.rs` | Semantic context or external sources must use these authorization/mutation primitives, never replace them with fuzzy or in-place writes. |
| Skills | Local bounded skills are discovered as untrusted instructions/resources with path/provenance checks; effects still cross ordinary tools. | `crates/kitsu/src/skills.rs:359,824-900,1514` | This is a declarative data plane, not executable configuration or a plugin loader. |
| Events/JSONL | Core runtime events are projected by the CLI into versioned `kitsu.chat.v1`; the writer serializes whole records, flushes, and fails terminally. One neutral `RunSpec` supplies JSONL, private generations, and human completion. Human timing and JSONL `final_summary` carry the same exact run outcome/assistant tuple. Every `run_started` now adds the complete provider-route receipt while attempts carry the correlated generation; the new field is optional in the schema so legacy records remain valid. | `crates/kitsu-core/src/run.rs`; `crates/kitsu/src/main.rs`; `crates/kitsu-render/src/lib.rs`; `crates/kitsu-render/src/jsonl.rs`; `docs/schemas/kitsu-chat-v1.schema.json` | E02 is accepted for the default runtime and E04's two-adapter product suite proves receipt/attempt parity. Alternate host/runtime conformance remains open. |
| Sessions | Production v1 still atomically stores one current `SavedSession` message vector plus gateway/model/project context. Fork copies the complete snapshot/artifact tree to a fresh identity without grants. The E03 ADR assigns target ownership; `kitsu-core::session` supplies the bounded neutral revision/HEAD contract and exact expected-revision admission. Public current-v1 reads and active save cross private transitional ports with unchanged bytes. Provider-free K04/K07/K08 fixtures pin logical publication, durability, stale conflict, and fencing. Private binary modules define the disjoint physical-v1 store, closed whole-operation canonical payload, and one transaction that validates/encodes before effects, pre-admits under the session fence, installs immutable bytes, rechecks artifact closure before HEAD CAS, and reports exact publication plus candidate residue truth. Strict current-v1 input can become only a non-branchable `Genesis` legacy baseline; pure successor assembly consumes neutral terminal/closed facts. Artifact-free and one complete artifact-bearing tool/effect successor pass fenced publication. The physical store owns session-scoped immutable artifacts plus revision manifests. Physical schema revision 2 makes every successor bind its exact verified parent-manifest digest; candidate install refuses an absent parent and HEAD CAS rechecks the link. Canonical publication and selected-HEAD open use one artifact predicate. A private lookup envelope pairs verified payload only with neutral `Present`; typed physical/logical/artifact faults remain payload-free and never fall back. A 4,096-revision bounded reader follows only the digest-bound HEAD-to-root chain, ignores unselected candidates, and fails on missing, retargeted, cyclic, or over-limit ancestry. Physical reads now separate a descriptor-pinned HEAD-bound manifest envelope from canonical payload verification; that envelope contains no trusted payload. The read-only recovery planner walks only bound envelopes, retains every skipped corrupt revision in selected-to-source order, and reports an exact blocker when no source closes. Canonical logical schema revision 2 owns the bounded recovery ledger. Pure assembly copies a fully verified source value into a fresh revision whose exact parent is the corrupt selected revision, clears stale compaction, and binds source revision/HEAD, selection, skip reasons, and commit time. A private recovery transaction holds the writer fence while it rechecks exact selected/source manifest digests, installs the assembled child against the selected envelope without reading corrupt parent bytes, rechecks artifacts, and CASes that envelope to the new revision. Candidate and HEAD-sync cuts report installed-unselected or published-unconfirmed truth; stale envelope CAS fails. A pure branch assembler now validates a committed turn, creates a fresh create-only destination revision, copies its exact chronological operation prefix and only referenced artifact facts, binds full source/branch lineage, preserves the historical creation anchor, and clears source-only compaction/recovery state. No authority state exists in the payload. | `crates/kitsu/src/session.rs`; `crates/kitsu/src/session/physical.rs`; `crates/kitsu/src/session/canonical.rs`; `crates/kitsu/src/session/transaction.rs`; `crates/kitsu-core/src/session.rs`; `crates/kitsu-core/tests/session_k04.rs`; `crates/kitsu-core/tests/session_k07.rs`; `crates/kitsu-core/tests/session_k08.rs`; `scripts/check-session-boundary.sh`; `docs/roadmap/e03-canonical-session-adr.md` | All new helpers remain deliberately unmounted, so production current-v1 remains authoritative and rollback. Branch artifact copying/publication, production mounting, complete K01-K12, and adapter parity remain open; X005 still fails. |
| External tools/hosts | No MCP client, bidirectional host adapter, daemon, or second principal is mounted. Private inventory, authority, lifecycle, phase-supervisor, sealed-exec/Landlock Linux backend, strictly serial line transport, and one composite tools-only MCP owner remain unmounted. The composite owns exact initialize, response-free initialized notification, list, and call ordering over one proof-bounded child. One private echo source validates an exact non-paginated list, publishes host identity/fingerprint truth, requires paired grants from a default-disabled exact host/caller admission seam, performs one no-replay call, withdraws, and waits for clean shutdown. A Linux x86-64 test-only peer proves the full path. JSONL is one-way output. | `crates/kitsu-tools/src/external`; workspace manifests and product registry search | X002-X004 remain failed. Production has no source configuration, caller/subagent decision derivation, admitted-source-to-child factory, registry/request inventory mount, restart, alternate-platform enforcement, or product routing. |

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
  remains current-v1 because no product constructor calls this factory. Cycle
  094 makes `ActiveSessionStore` a closed current-v1/bound-local sum. A private
  constructor can retain the bound local owner, stable id/name staging, and
  sessions root. Its current representation maps snapshot commits and
  installed-unconfirmed warnings back to the exact existing product contract.
  Its canonical representation explicitly refuses legacy snapshot save, rename,
  reset, and sidecar artifact routing before effects. Artifact-path and rename
  calls are fallible so no canonical state can masquerade as current-v1. Both
  lock domains remain held until the active owner drops. Public create and
  resume constructors still build only the current-v1 variant, so this cycle
  changes no product persistence route. Cycle 095 adds one private exact-id
  activation result that owns both this bound active owner and the exact resume
  snapshot selected after activation. Current replacement between an earlier
  load and activation cannot leak the stale value; canonical projection remains
  bound to the exact owned HEAD. Invalid identity fails before opening the
  sessions root. The coupled result exposes only read-only identity, name,
  artifact-route, and saved-value views, with no consuming extractor or store
  reference that could mutate behind the saved value. Product construction and
  routing remain current-v1. Cycle 096 replaces the old store-only public resume
  constructor with `ActiveSessionResume`. It owns an exact current-v1 snapshot
  beside the active owner that re-read and compared it after lease acquisition,
  then permits one explicit product split. Startup and in-process durable resume
  both use this handoff, so stale preloaded bytes cannot enter runtime while an
  owner silently protects different data. Read-only no-session resume is
  unchanged, and no composition identity or physical namespace is created.
  Cycle 097 makes the exact private bound activation return this same result
  shape and removes its duplicate wrapper. The snapshot remains readable only
  while the result owns its active store. Its consuming split is current-v1
  specific and rejects either bound source, so neither bound current nor
  canonical state can expose a product store before an explicit admission card.
  No product constructor or on-disk route changes. Cycle 098 adds a private
  type-state gate over that result. It admits only bound-current activation,
  whose snapshot save path is already supported, and yields its owner/data pair.
  Bound canonical activation returns a typed missing-operation-writer refusal,
  changes no HEAD, and releases both fences without yielding either field.
  Missing state remains effect-free. Product routing is still current-v1.
  Cycle 099 selects the bound-current route for durable startup resume. Selector
  listing/resolution and activation share one pinned sessions descriptor; only
  the exact snapshot returned under both fences crosses into runtime. Current
  exact/name/last/auto semantics remain, while an exact canonical ID returns the
  operation-writer refusal before yielding product parts. Bound-current reset
  now creates a fresh current-v1 owner before replacing and dropping the old
  bound owner, preserving `/new`; canonical reset still refuses. In-process
  `/resume` remains current-v1 until Cycle 100. Cycle 100 mounts the same
  bound-current admission as a staged in-process target. The old owner remains
  active and the target owner remains fenced while gateway/directory/system
  validation, current-session persistence, context-window discovery, and the
  immutable provider-generation replacement can still fail. Each refusal drops
  the staged target and preserves the old owner. After the last fallible step,
  the target result is consumed once and replaces the old owner without another
  path read. Stateless no-session resume retains its read-only route.
- Cycle 101 freezes one owned, non-cloneable tool inventory for every provider
  request. The schemas advertised to the provider and the immutable handler
  identities used for structured calls, recovered text calls, and output
  offload remain the same snapshot for the whole response. It does not yet add
  source identity, generations, withdrawal, external processes, or MCP.
- Cycle 102 adds an inert private child-launch request, exact host-authored
  authority profile, and staged preparation boundary. Disabled, denied,
  unsupported, malformed, and profile-mismatched inputs return before any host
  observation. An admitted identity is then canonicalized, hash-verified, and
  containment-preflighted in order. The resulting value is still unspawned;
  no production process API or external-tool composition exists.
- Cycle 103 adds a provider-free MCP wire codec pinned to revision
  `2025-11-25`. Its bounded newline decoder is fail-stop; its client has one
  positive-integer request in flight and accepts only initialize/initialized,
  tools/list, and tools/call. It advertises no ambient client capability and
  rejects non-tools server capabilities and changing inventories. Raw tool
  results remain unpublished. The codec owns no process, transport, deadline,
  configuration, inventory, or product route.
- Cycle 104 adds closed external source, incarnation, inventory-revision, and
  tool identity types plus a transactional candidate validator. Advertised
  names do not participate as identities. Bounded validated descriptors receive
  a domain-separated SHA-256 over Kitsu's versioned canonical bytes; malformed,
  duplicate, and synthetic collision cases yield no inventory. Revision
  monotonicity, CAS publication, withdrawal, and child-origin admission remain
  absent.
- Cycle 105 adds one non-cloneable lifecycle owner constructed only from the
  single-use prepared-launch proof. It owns one injected spawn attempt, retains
  EOF/idle as nonterminal observations, performs join before classifying natural
  exit, signals before joining cancel/shutdown, and retains typed residue plus a
  drop-cleanup report. The effect backend remains a synchronous provider-free
  fixture; no production process or durable report sink exists.
- Cycle 106 adds a presentation-neutral, current-session telemetry snapshot.
  Route/profile/generation, optional session/run identity, effective authority,
  context occupancy, usage coverage and categories, and portable cost are each
  one truth-class/provenance value. Unavailable has no value. Provider-reported
  currency is the only admitted cost; Kitsu does not derive price. The DTO is
  unmounted, and current run tallies cannot yet populate complete response
  coverage or cached-token facts.
- Cycle 117 mounts that typed observation on exact `/status`. One captured route
  receipt and one synchronous snapshot drive adaptive human output; runtime
  version and cwd are authoritative local facts. The command performs no
  provider, account, Git, filesystem, or persistence lookup. Existing JSONL
  keys/nesting remain stable and its text field is the deterministic redirected
  view of the same observation. Account limits/prices and durable history remain
  unavailable.
- Cycle 107 adds one private atomic publication owner for complete validated
  external inventories. Initial publication is create-only. A successor must
  match the exact source incarnation and advance by exactly one without
  overflow; replacement and withdrawal are explicitly fenced by the complete
  current source lifetime and revision. Stale, replayed, skipped, wrapping, or
  wrong-source transitions preserve the identical borrowed snapshot. No live
  handler registry, child, MCP transport, configuration, or product route uses
  it yet.
- Cycle 108 binds one exact runtime policy across the inert child request, host
  authority profile, and single-use prepared proof. Five phase budgets and four
  I/O limits are non-zero and capped; the frame limit fits stdin and stdout.
  Environment inheritance is forbidden and the ordered unique projection is
  aggregate-bounded, NUL-free, and debug-redacted. Owned process group, denied
  network, and terminate-group-and-reap timeout behavior are mandatory. These
  are admission facts only; no production spawn or backend enforcement exists.
- Cycle 109 extends current-run tallies with reported/missing provider-response
  coverage and cached-prompt-token presence. One request-local tracker uses
  physical attempt ordinals to deduplicate retry and terminal observations;
  HTTP rejections are completed missing-usage responses, transport-only faults
  are not, and usage remains reported if a later stream item fails. Fixed all,
  inference, compaction, and future model-catalog buckets project through a pure
  session-usage adapter. Catalog population, product snapshot mounting, slash
  routing, rendering, JSONL, cost, and durable history remain absent.
- Cycle 110 adds a private presentation-only renderer over the typed snapshot.
  Styled destinations select bounded wide, stacked, or narrow layouts and stay
  within one-cell through wide terminal widths, including double-width Unicode.
  Redirected, plain, no-color, and screen-reader destinations share one stable
  escape-free format. Truth/provenance travels with every rendered fact; cost is
  omitted unless already provider-reported. No snapshot collector, slash route,
  JSONL field, provider/account/MCP call, persistence, or history is added.
- Cycle 111 adds a private provider-free phase supervisor over the exact
  prepared-child policy. Checked monotonic startup/request/drain/kill/reap
  deadlines expire at equality; cancellation/shutdown are idempotent; group
  termination precedes fresh kill/reap clocks; and clock/effect/reap/drop faults
  transfer a non-cloneable child into typed residue. No OS backend, containment
  enforcement, natural-exit composition, configuration, MCP, or product route
  exists.
- Cycle 112 mounts exact `/usage session` on one coherent in-memory snapshot.
  It reads the current route receipt once, samples effective authority, context,
  run/session identity, and usage without an await or external effect, and
  renders only the existing adaptive usage section. Missing reports/cached
  attribution/cost remain unavailable. JSONL command-result parity, aliases,
  history, account data, and full status/context migration remain absent.
- Cycle 113 adds a test-only deterministic semantic-evidence baseline. Three
  fixed mixed-language vectors exercise only current read-only tools and freeze
  a 3,279-byte canonical JSONL artifact across independent temporary roots.
  Locate, explain, and change-evidence plans pass at 10,000 relevance basis
  points with exact round/token/tool-byte observations. Parser and runner caps
  bound every vector dimension and stop later calls on output overflow. This is
  neither a production contributor nor evidence that a semantic selector helps.
- Cycle 114 stages a private effect-free guided-setup transaction under the
  configuration owner. Bounded local, hosted, and custom inputs normalize into
  non-secret previews; explicit consent is the first effect directive. Fenced
  acquire, validate, create-new publish, and reverse rollback receipts preserve
  primary failure/cancellation/collision truth and transfer incomplete owned
  residue to a recovery owner. No UI, provider/catalog/credential adapter,
  config write, model picker, or product route exists.
- Cycle 115 adds a private real Linux effects backend behind the prepared-child
  proof and phase owner. Parent-built seccomp state, inherited-FD closure,
  cleared capabilities, bounded joined pipes, real clocks, group termination,
  direct wait, and exit/EOF/crash fixtures pass. The mechanism stays unmounted
  and one-shot; serial MCP composition, configuration, and every X002 product
  gate remain absent.
- Cycle 118 replaces pathname execution with a capped exact-byte sealed native
  ELF and raw `execveat`, and binds descriptor-pinned cwd/roots to one unique
  Landlock preparation token. Authority mismatch and every unsupported setup
  fail before target code. The mechanism remains Linux x86-64, private, and
  one-shot; it provides content access control but not mount isolation, path or
  metadata secrecy, host signal/ptrace confinement, or a general sandbox.
- Cycle 119 retains one such sealed and content-confined child across multiple
  strictly serial bounded newline frames. The private owner enforces one
  in-flight request, full request write before response, fixed line and
  progress-reset idle deadlines, cumulative pipe/frame/count caps, EOF/crash
  refusal, and common group/reap/residue cleanup. It is opaque transport, not
  MCP composition or a product route.
- Cycle 120 composes the private guided-setup transaction with the private
  descriptor-relative create-new publisher for one local, credential-free,
  offline path. Exact transaction fences and a same-ID concrete cleanup ledger
  preserve collision, logical rollback, pre-install stage cleanup, and
  incomplete recovery as distinct ownership. No UI, activation, provider,
  catalog, credential, hosted, or custom route is mounted.
- Cycle 121 mounts exact `/context window` from the same typed current-session
  observation used by the telemetry decks. One in-memory route receipt and one
  synchronous snapshot feed a context-only adaptive renderer; existing bare
  and refresh behavior is unchanged, and no provider/account/filesystem/store
  effect or new JSONL result schema is introduced.
- Cycle 122 measures a test-only deterministic structural selector against the
  frozen mixed-language baseline. It preserves 10,000-basis-point relevance
  and forbidden-evidence refusal while reducing aggregate rounds from 5 to 3
  and canonical tool bytes from 1,456 to 1,134. The result promotes only a
  further authority/staleness experiment; no production contributor exists.
- Cycle 123 stages a private immutable offline-capable model-picker snapshot.
  Closed catalog availability/source/freshness/provenance and three-state
  membership feed bounded deterministic filtering, ranking, narrow projection,
  and a proposal only. It performs no provider, cache, filesystem, credential,
  config, route, session, capability, UI, or product mutation.
- Cycle 124 folds bare `/usage`, `/usage session`, and empty-args-only `/cost`
  compatibility onto one typed current-session usage observation and renderer.
  Help and lifecycle events retain exact spelling; JSONL remains lifecycle-only
  with no result or stderr. No account, provider, monetary, filesystem, store,
  or history source is consulted.
- Cycle 125 makes the test-only structural selector reconstruct every emitted
  path and line from bounded current authorized reads after ranking. Hostile
  denied/private/symlink candidates leak nothing, same-invocation replacement
  yields only revalidated current truth, and revocation refuses with zero
  evidence. The original promotion metrics remain unchanged; no production
  contributor, cache, mutation, or task-completion claim is added.
- Cycle 126 composes the pinned tools-only MCP state machine with one private
  strictly serial child owner. Proof-derived newline/payload/count/deadline
  bounds cover initialize, the response-free initialized notification, list,
  and calls; all client/decoder/transport/cleanup faults fail closed with exact
  primary and residue truth. It remains unmounted with no inventory, config,
  handler, product route, or wider MCP vocabulary.
- Cycle 127 mounts exact offline `/context sources` from retained typed startup
  entries, current role-based messages, budget arithmetic, and compaction
  accounting. Unsupported attachment, on-demand-skill, memory, and response-
  schema buckets remain unavailable; no aggregate/source-total or JSONL result
  is invented. Existing context forms remain unchanged.
- Cycle 128 compiles the measured structural selector as an unmounted read-only
  contributor. It inherits existing `Permissions`, suppresses interactive
  prompts, bounds prompt/hits/files/paths/output, and reconstructs all emitted
  lines plus role counts from current authorized reads after ranking. Frozen
  benchmark artifacts and the 5-to-3-round / 1,456-to-1,134-byte result remain
  unchanged; no registry, configuration, provider, session, cache, mutation,
  or automatic context path invokes it.
- Cycle 129 isolates test-only publication, rollback, and pre-install-hook
  fault plans behind must-use generation guards. Early return and unwind clear
  only their own unconsumed plan, and older guards cannot erase newer plans.
  This removes cross-fixture full-suite contamination without changing any
  production setup publication, collision, cleanup, ledger, or rollback path.
- The Cycle 129 acceptance repair closes a real ctime/classification defect
  exposed by the clean full suite. The publisher holds a nonblocking advisory
  lock on the pinned parent across mutation and rollback, rebaselines only the
  ctime caused by its own quarantine rename, retains uid/gid and all other
  exact identity facts, and classifies removal/displacement from the receipt fd
  plus both known names. Linux still has no conditional unlink-by-fd; arbitrary
  noncooperating or privileged same-UID namespace mutation remains explicitly
  outside this mechanism.
- Cycle 130 runs an exact tools-only MCP exchange against a test-only raw peer
  inside the sealed memfd/Landlock/seccomp child. One child owns initialize,
  notification, list, and call; EOF, crash, wrong ID, malformed JSON, cancel,
  shutdown, and descendant cleanup retain exact failure/reap truth. This is
  test evidence, not configuration, inventory, handler, or product mounting.
- Cycle 131 binds every private model-picker snapshot and proposal to an exact
  typed provider profile, adapter, and wire revision derived from a validated
  route declaration. Capability, generation, endpoint, auth, and model are not
  smuggled into namespace identity. Selection remains pure and unmounted.
- Cycle 132 composes one explicit exact-namespace proposal with the private
  local setup owner. Model replacement is exact and pre-consent; rejected,
  completed, failed, collision, rollback, and retry paths retain the entire
  non-cloneable proposal. No UI, catalog, credential, activation, current-route,
  or product effect is mounted.
- Cycle 133 binds one exact host-authored echo inventory to the private sealed
  MCP owner. A child list must match the compiled descriptor before whole-
  snapshot publication; both host and caller grants match full identity and
  fingerprint, and a possibly accepted call never replays. Publication is
  withdrawn before cleanup or Drop, while response success waits for clean
  child shutdown and cleanup failure retains non-success evidence/residue.
  Grant construction and all configuration/registry/product routes remain
  absent outside tests.
- Cycle 134 adds the sole private production grant-admission path. One raw echo
  declaration defaults disabled; disabled and denied requests stop before
  parsing, exact host identity admits the source, and a separate exact caller
  identity issues one paired host/caller grant accepted by the handler. The raw
  declaration is non-Debug/non-serialized and the admission path performs no
  preparation, spawn, registry, provider, filesystem, or product effect.
- These changes close X001. X002-X005 remain failed until their named default
  and independent implementations execute complete conformance suites.

## Current extension planes

| Need | Current mechanism | Trust/process boundary | Status |
|---|---|---|---|
| Native behavior | Compiled Rust modules registered by product composition | Same process, binary authority | Retain; no dylib/runtime loader |
| Static instructions/resources | Local skills | Untrusted data; no inherent effect authority | Retain and reuse provenance rules |
| Provider dialect | Compiled `Provider` trait, compatible Chat and native Messages adapters | Same process; network/auth provider-owned | X001 replaceable; closed compiled registration only |
| External runtime tools | Static request inventory; closed external identities/fingerprints; exact atomic external-inventory publication; exact bounded child policy/admission plus lifecycle, phase-supervisor, sealed-exec/Landlock Linux backend and strictly serial line-transport truth; bounded tools-only MCP codec; one exact private echo publication/handler owner; default-disabled exact host/caller grant admission | Same-process product handlers only; the backend/transport/handler/admission are private and unmounted on Linux x86-64, with explicit Landlock limits | E06 identity, child admission/policy, publication, lifecycle, phase, exact-exec/content authority, serial real-child, private MCP/echo-handler composition, exact grant admission, and one test-only sealed peer are proven; no product source configuration, child factory, registry/request mount, alternate-platform enforcement, or X002 acceptance |
| Host/UI protocol | CLI carriers plus one-way JSONL; typed current-session telemetry DTOs, truthful current-run usage coverage, adaptive renderer, typed offline `/status`, `/context window`, `/context sources`, and one usage view behind bare/session/legacy-cost forms | Consumer cannot service synchronous calls; mounted telemetry samples retained in-memory owners only and performs no provider/account/network/command-time-fs/persistence effect. `/status` preserves its legacy JSONL payload; context and usage forms deliberately have no new command-result contract. Context-source rows separate estimated, derived, authoritative, and unavailable truth without claiming an additive total | Truthful current status/usage/context-window/source slices and compatibility aliases are live, but no named second host, new usage automation schema, account data, exact tokenizer attribution, or durable history exists; X003/X004 fail |
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
