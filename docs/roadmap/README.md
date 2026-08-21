# Kitsu Improvement Delivery Backlog

Status: active on `improvements/harness-roadmap`

Imported: 2026-08-17

Branch base: `a2ba2d5c98b17da7290a2b6b8b24c83bc2529763`

Cycle protocol: [`cycle-protocol.md`](cycle-protocol.md)

Invariant test routing: [`invariant-tests.md`](invariant-tests.md)

Mechanism learning ledger: [`mechanism-ledger.md`](mechanism-ledger.md)

Post-E01 evidence baseline: [`post-e01-evidence-baseline.md`](post-e01-evidence-baseline.md)

Current architecture: [`current-architecture.md`](current-architecture.md)

Current comparison: [`current-comparison.md`](current-comparison.md)

Program status: [`program-status.md`](program-status.md)

Parallel delivery after the Cycle 100 checkpoint:
[`parallel-delivery-plan.md`](parallel-delivery-plan.md)

Provider wire fixture contract:
[`e04-primary-wire-fixtures.md`](e04-primary-wire-fixtures.md)

Provider route value contract:
[`e04-provider-route-contract.md`](e04-provider-route-contract.md)

Status, context, and usage truth contract:
[`e08-telemetry-decks.md`](e08-telemetry-decks.md)

## Purpose and baseline rule

This is the Kitsu-owned implementation backlog translated from the ten-harness
research. It defines milestones, epics, user stories, dependencies, acceptance
evidence, and the 30-minute delivery loop. Accepted work is recorded under
[`cycles/`](cycles/).

The research pin is `6c713333`. Committed Kitsu `main` was `a2ba2d5` when this
backlog was written, 67 commits ahead of that pin. The live Kitsu worktree also
had ten unrelated modified files. Planning may inspect them for collision risk,
but no story may treat them as shipped behavior or absorb them into the future
branch without an explicit ownership decision.

## Product outcome

Kitsu remains a small, local-first Rust CLI with explicit authority, mandatory
command containment, offline/auditable configuration, bounded model and tool
work, versioned automation, safe sessions, and exact edits. The program adds
replaceable integrations at five typed boundaries without changing those
defaults or creating a universal plugin system.

The word “user” below includes interactive users, automation operators,
maintainers, and adapter authors. A maintainer-facing story is valid only when
it protects an observable product property or removes demonstrated coupling.

## Preserved invariants

Every epic inherits these acceptance requirements:

- Filesystem reads, writes, commands, credentials, skills, and host controls
  remain separate authority domains.
- Commands remain disabled by default and fail closed without supported OS
  containment. Approval is not treated as containment.
- Config discovery remains offline and performs no implicit package, network,
  credential, or state mutation.
- Sessions never restore runtime grants, credentials, executables, or child
  processes.
- Failed and cancelled turns keep Kitsu's conversation rollback and truthful
  retained-effect reporting.
- JSONL remains versioned, identity-bearing, fail-stop, and consistent with
  process exits `0/1/2/130`.
- Optional integrations create no child, network request, state write, schema
  overhead, or startup delay while disabled.
- Model and tool outputs, context, events, pipes, files, time, retries, and
  shutdown are bounded.
- New stores, providers, sources, and hosts adapt to Kitsu-owned types. The core
  never imports their concrete protocol, config, credential, or persistence
  types.

Current core dependencies on the concrete tool registry and `kitsu-render` are
baseline coupling, not proof of the target host/tool ports. E02/E10 may extract
them only when an executed fixture proves the boundary.

## Architectural thesis, milestones, and dependency map

The program borrows mechanisms rather than plugin models. Its three integration
spines are immutable provider generations, a canonical session revision/HEAD,
and a bounded child supervisor. The detailed source-by-source transfer and
rejection decisions live in the [mechanism learning ledger](mechanism-ledger.md).

| Milestone | Epics | Outcome |
|---|---|---|
| M0 — trustworthy starting point | E00-E01 | Current evidence pin, regression envelope, Ollama doctor fix, deterministic registry admission |
| M1 — causal execution foundation | E02 | One identity-bearing run outcome across carriers and terminal phases |
| M2 — immutable provider spine | E04 | Compatible Chat plus one distinct compiled wire and one route generation across all sends |
| M3 — canonical session spine | E03 | Revisioned logical HEAD, recovery, compaction lineage, and safe branch-at-turn |
| M4 — bounded child spine | E06 | Private supervisor plus disabled-by-default, tools-only stdio MCP experiment |
| M5 — measured context and onboarding | E07, E05, E08 | Evidence-driven semantic context, transactional picker/setup, truthful telemetry decks, and progressive docs |
| M6 — later media evidence | E09 | Bounded multimodal input only across truthful capability paths |
| M7 — replaceability certification | E10 | Boundary-by-boundary conformance decision with default-path parity and rollback evidence |

```text
E00 -> E01 -> E02
          \-> E04 -> E05 -> E08
           \      \--------> E08-S05
           \-> E03 --------> E08-S06
           \-> E06 --------> E08
           \-> E07 --------> E08
               E04 --------> E09
E01 + E02 + E03 + E04 + E05 + E06 + E07 + E08 + E09 -> E10
```

E02 may start before E01 only for provider-free fixture design; production
integration waits for deterministic registry admission. After E02, the serial
queue prioritizes E04's provider spine, then E03's session spine, then E06's
supervisor/MCP spine. E06 requires E01 and the cancellation/terminal parts of
E02, even though delivery priority keeps it after E03. E07 follows the three
spines; E05/E08 then use the stabilized provider and external-tool surfaces.
E08-S05 consumes E04 route/capability receipts, while E08-S06 waits for E03's
canonical revision and fencing before it publishes durable usage observations.
Provider-free DTO and renderer fixtures may be prepared earlier, but neither
story may infer missing provider/account data or bypass its owning spine.
E09 requires two truthful capability paths from E04. No desktop, IDE, listener,
daemon, or control plane is invented solely to improve X003 breadth.

## Definition of Ready

A story is ready only when it has:

1. one user-visible or invariant-preserving outcome;
2. an owning epic and five-boundary mapping;
3. named source files or a bounded discovery card;
4. deterministic tests that do not require credentials or billable inference;
5. explicit authority, failure, compatibility, and rollback behavior;
6. no collision with uncommitted work in the selected implementation tree;
7. a decomposition into green, independently useful 30-minute cards.

## Definition of Done

A story is done only when:

- requirements and non-goals are recorded;
- the relevant test fails for the intended reason before implementation, unless
  the card is documentation/refactoring-only;
- implementation passes targeted tests and the story's invariant regressions;
- review checks authority, failure isolation, boundedness, versioning,
  observability, compatibility, and rollback;
- formatting and warning gates pass for the affected crates;
- machine contracts and docs are updated together when externally observable;
- every accepted card has a focused commit and cycle record;
- the epic exit suite passes before the epic is marked complete.

## E00 — Program baseline and safety envelope

Status: accepted through Cycle 010; the clean base, invariant routes, mechanism
ledger, content-pinned delta audit, current architecture/comparison/program
views, and cycle-receipt check are Kitsu-owned and mutually consistent.

Outcome: maintainers can prove which Kitsu revision is being changed and which
existing strengths every later story must preserve.

| Story | User story | Requirements | Acceptance evidence | Size |
|---|---|---|---|---|
| E00-S01 — clean implementation baseline | As a maintainer, I want the improvement branch based on one clean committed Kitsu revision so research and implementation claims are reproducible. | Resolve ownership of the ten dirty files; create `improvements/harness-roadmap` only after backlog approval; record base SHA, Rust version, tree status, and research delta; never move or discard user edits implicitly. | Branch/worktree is clean at creation; recorded SHA equals its merge base; dirty `main` remains byte-for-byte and status-for-status unchanged. | 1 cycle |
| E00-S02 — invariant test manifest | As a maintainer, I want one executable list of the authority, containment, JSONL, session, edit, credential, and cancellation regressions that gate every epic. | Map existing tests before adding new ones; distinguish per-card targeted gates from milestone workspace gates; name platform-specific skips/fail-closed behavior. | Every preserved invariant maps to at least one existing or explicitly missing test; commands run provider-free; manifest links source/test owners. | 1-2 cycles |
| E00-S03 — live delta audit | As a planner, I want research rebased from `6c713333` to the branch base so completed work is not scheduled twice. | Pin the non-versioned research inputs without rewriting their frozen/historical claims; audit all five boundaries and candidate rows; identify committed retry/context/tool/recovery changes; keep source-backed and runtime-observed labels distinct. | Kitsu-owned evidence baseline, current architecture, gap status, backlog, and program status agree on the same pin; frozen research inputs retain content hashes; audit checks pass. | 2-4 cycles |
| E00-S04 — cycle records | As a reviewer, I want each 30-minute loop to leave a compact decision and verification receipt. | Use the cycle template below; keep raw output out of prose; record incomplete/rejected cards without a production commit. | A script or review check proves every improvement commit has one cycle record naming story, tests, result, and next card. | 1 cycle |
| E00-S05 — mechanism transfer ledger | As a maintainer, I want every comparative lesson translated into a narrow Kitsu mechanism or explicit rejection so ecosystem breadth cannot smuggle in a plugin model. | Map all ten sources to a gap, safe mechanism, and do-not-copy boundary; name the three spines, dependency-safe order, hard exclusions, and executable gates. | Ledger, epic requirements, milestone order, and cycle queue agree; all local links resolve; no source-backed claim is presented as executed Kitsu evidence. | 1 cycle |

Epic exit: branch baseline and research pin agree; preserved-invariant test
manifest and mechanism ledger exist; program audit and the baseline acceptance
commands pass.

## E01 — Bounded defects and deterministic tool admission

Status: accepted through Cycle 008; Ollama alias truth, deterministic registry
admission, CLI propagation, descriptor/handler parity, and the ignored-result
CI gate are green on the improvement branch.

Outcome: local Ollama diagnostics are truthful and tool composition cannot
silently publish duplicate or mismatched handlers/schemas.

Dependencies: E00-S01 and E00-S02.

| Story | User story | Requirements | Acceptance evidence | Size |
|---|---|---|---|---|
| E01-S01 / C001 — Ollama alias truth | As a local Ollama user, I want `doctor --online` to accept the untagged model used by Kitsu's starter profile when Ollama advertises the same model as `:latest`. | Equivalence only for structurally local Ollama; untagged selected ID may match exactly one `:latest`; explicit tags and generic gateways stay exact; catalog check sends no chat request; human and JSON agree. | Five cases from `reports/kitsu-baseline.md`, plus ambiguity/empty-ID tests; exit `0` for the valid alias and `1` for genuine mismatch. | 1-2 cycles |
| E01-S02 / A001 — deterministic registry admission | As a maintainer, I want duplicate tool names rejected before publication so the model schema and invoked handler cannot diverge. | Both registration paths return `Result`; same-path and cross-path collisions fail before mutation; descriptor-free test tools remain supported; composition errors reach CLI before the first provider request. | Four duplicate-order tests preserve the first entry; descriptor/handler parity tests pass; a provider-free composition-boundary test proves the failure cannot contact a provider. | 2-3 cycles |
| E01-S03 — registry coupling guard | As an adapter author, I want CI to reject schema/handler drift and unhandled registration failures. | Check every advertised descriptor has exactly one handler; deny ignored registration results; no provenance, namespace, contributor framework, or public plugin API in this slice. | `RUSTFLAGS=-Dunused-must-use` check, focused registry tests, current capability guards, workspace check, Clippy, and format pass. | 1 cycle |

Epic exit: KB-001 is closed; A001 is integrated on the current base; no provider
or authority behavior changes except truthful preflight failure.

## E02 — One run identity and causal terminal outcome

Fixture contract: [`e02-carrier-matrix.md`](e02-carrier-matrix.md). Neutral type
contract: [`e02-run-contract.md`](e02-run-contract.md). Exit audit:
[`e02-terminal-ownership-audit.md`](e02-terminal-ownership-audit.md).

Status: accepted through Cycle 027. E02-S01/S02 were accepted through Cycle 017.
Neutral identity/outcome values now drive typed authority interruption and human completion receipts;
confirmation EOF causes zero later provider sends/effects, explicit denial
stays recoverable, and sequential human processes expose distinct run IDs plus
the same success/provider/tool classifications as JSONL. Terminal phase
ownership remains E02-S03; Cycle 018 adds its provider-free monotonic phase
model before any carrier is migrated. Cycle 019 starts E02-S04 by adding the
same neutral outcome and exact assistant disposition to `final_summary` without
invalidating historical `kitsu.chat.v1` records. Cycle 020 routes the bounded,
tool-free, session-free `ask --jsonl` terminal path through the lifecycle and
cuts successful versus failed summary delivery without semantic reclassification.
Cycle 021 adds actual cleanup-residue and active-snapshot persistence inputs to
the scripted JSONL terminal publication, including installed-but-failed saves.
Cycle 022 routes every piped JSONL terminal branch through the same publisher
and preserves rollback cancellation's exact `joined` status instead of reducing
it to an ID before cleanup classification. Cycle 023 audits the remaining
ownership boundary and retains four evidence-backed gates: one process owner,
presentation-neutral facts, interactive aggregation, and independent exit
evaluation. Management commands are explicitly outside this inference-run
contract rather than silently treated as completed carriers. Cycle 024 replaces
the migrated helpers' temporary owners with one process-scoped lifecycle,
accepts it before inference preflight, rejects a second cross-path publication,
and routes structurally child-free JSONL fallback through that owner. A first
foreground provider-turn marker prevents later fallback from guessing cleanup.
Cycle 025 carries exact cleanup and active-snapshot persistence from human ask,
review, piped, and scripted carriers into the same process owner. Human timing
is now a fallible delivery cut, while an unexpected post-start fallback records
cleanup as `NotObserved` and therefore cannot certify clean success. Interactive
aggregation remains the bounded Cycle 026 exit. Cycle 026 then publishes that
aggregate: a recovered failure plus later success is partial rather than
last-turn success; steering replacement is not itself a fault; cancellation and
authority interruption remain typed; background cleanup and active-session
persistence reach the same process lifecycle before human timing delivery.
Cycle 027 assigns all seven carrier identities before inference preflight and
uses one production evaluator over immutable publication for both human and
JSONL exits. The independent provider-free matrix passes 77 semantic/phase/exit
vectors with no residual card. X004 remains a failed complete replaceability
gate because no named alternate runtime adapter executes the same suite.

Outcome: interactive, ask, piped, script, and JSONL carriers expose the same
semantic run result even when delivery, cancellation, cleanup, or persistence
fails.

Dependencies: E00; production types after E01.

| Story | User story | Requirements | Acceptance evidence | Size |
|---|---|---|---|---|
| E02-S01 — carrier fixture | As an automation operator, I want identical semantic outcomes across Kitsu entrypoints so I never infer success from EOF or presentation. | Provider-free scripted provider/tool fixtures; sequential same-session runs; confirmation EOF; provider/tool/preflight fault; cancel before/active/queued; blocked/EPIPE sink; cleanup residue; duplicate/cross-run terminal; exact exits. | One table asserts identity, class, terminal count, committed/discarded state, persistence, delivery, and `0/1/2/130` for every carrier. | 3-5 cycles |
| E02-S02 — private run contract | As a host adapter author, I want `RunSpec`, identity-bearing `RunEvent`, and `TerminalOutcome` owned below presentation so carriers cannot reclassify results. | Extract only fixture-proven divergence; core has zero CLI/env/signal/exit/store dependencies and zero renderer/JSONL types; identity assigned before acceptance. | Compile-time coupling check plus existing behavior fixtures; changing carrier changes no agent-loop source. | 4-6 cycles |
| E02-S03 — terminal phase truth | As an operator, I want to distinguish semantic settlement, child/background join, persistence, delivery, and process exit. | Monotonic `settled -> joined -> persisted -> delivered`; delivery failure never rewrites semantic settlement; typed cleanup residue; zero work after terminal. | Fault injection at each phase yields one terminal and truthful phase receipts; duplicate publication is rejected. | 3-4 cycles |
| E02-S04 — JSONL projection parity | As a JSONL consumer, I want `kitsu.chat.v1` to remain compatible while projecting the common outcome. | Additive fields only unless a separately approved version bump; writer remains fail-stop; exact assistant disposition and final summary preserved. | Schema/examples validate; old fixtures parse; human and JSONL exits/outcomes agree; blocked writer stops later provider/tool work. | 2-3 cycles |

Epic exit: X004 can be evaluated with an independent deterministic harness.
X003 remains “prepared,” not product-certified, until a real second host/principal
exists; the fixture is not misrepresented as product demand.

## E03 — Canonical session revision, recovery, and branch-at-turn

Outcome: every restart-authoritative session change has one atomic revision and
logical HEAD; users can recover and branch without restoring authority or
cutting an incomplete exchange.

Dependencies: E02 terminal outcome model.

Status: E03-S01 and E03-S04 are accepted, E03-S02 is implemented through C33,
and E03-S05 is in progress through Cycle 080. The
canonical ADR and neutral logical value layer assign transcript operations,
semantic terminal outcomes, selected logical
HEAD, resume intent, lineage, artifact closure, and compaction binding to one
self-validating revision. It separates logical/store/artifact/projection/export
versions; requires expected-revision CAS in addition to the writer lease;
defines typed corrupt/future/conflict and publication states; preserves fresh
authority; and gives K01-K12 stable backend-neutral meanings. Bounded opaque
identities, logical HEAD/history, typed lookup states, and publication receipts
now have closed, versioned, fail-closed vectors under a store-neutral dependency
guard. Production v1 bytes are unchanged and X005 still fails. Current `load`,
`load_other`, and `list` cross a private read port; active autosave crosses its
private write partner with exact pre-install versus installed-unconfirmed
evidence. A store-neutral exact-revision admission now accepts only
`Missing`/absent or identical `Exact`/present pairs and returns a typed conflict
before effects for every mismatch. Other mutation paths retain direct v1
behavior; production revision minting/publication is not yet mounted.
An independent test-only memory store now composes admission, deterministic
opaque successor minting, publication, and exact receipts through the K08
create/update/stale/retry sequence. It is conformance evidence, not a second
product store or an X005 promotion.
A shared provider-free memory harness now also executes bounded K04 cuts:
before-candidate and staged-candidate outcomes keep the old canonical revision
and report not-published; post-publication exposes the new revision as
published-unconfirmed; owner confirmation exposes the same new revision as
confirmed.
The provider-free K07 fixture adds exact session-bound fence generations: a
second writer for one session is refused, a different session proceeds, and
lost/replaced/wrong-session tokens fail before mint or publication.
The private compiled file-store v1 contract now fixes a disjoint namespace,
portable reversible identity components, create-new store/revision records,
exact payload/manifest digest bindings, descriptor-pinned candidate I/O, and a
real per-session OS writer fence around exact-revision atomic HEAD replacement.
It has no production caller and changes no current-v1 bytes.
Strict current-v1 history now has one create-only private migration path: exact
legacy baseline preparation feeds the same fenced file transaction with
`ExpectedRevision::Missing`. Repeat/existing HEAD attempts allocate nothing,
colliding orphan bytes are preserved, and source v1 bytes remain rollback at
every tested cut.
A pure successor assembler now adds one complete operation from a neutral E02
terminal plus bounded message/effect/artifact facts. It derives HEAD movement,
requires fresh revision/time lineage and complete closure, retains legacy
baseline/lineage, and drops the obsolete compaction binding. It remains
unmounted.
The first physical successor composition is restricted to artifact-free text.
Its only cross-module constructor cannot express tools/effects/artifacts, an
artifact-bearing base refuses before effects, and exact/stale/candidate/HEAD
transaction cuts pass without changing production behavior.
Physical v1 now has a separate session-scoped immutable artifact namespace.
Closed manifests bind independent artifact schema, exact identity, length,
SHA-256, and media type; staged descriptor-pinned install/read is no-overwrite
and reports collision, tamper, symlink, and durability truth. Canonical
publication now compares every artifact fact to exact verified physical state
both before candidate allocation and after install before HEAD CAS. One complete
tool/result/effect operation passes exact missing/mismatch/tamper/cut fixtures;
canonical HEAD open now decodes and binds the selected revision and reapplies
that same exact closure. Only an absent HEAD is missing; selected logical,
binding, or artifact corruption never falls back. A lossless lookup envelope now
pairs verified payload with `Present`, maps current corruption reasons exactly,
and distinguishes future logical/artifact/physical schemas. Physical HEAD and
revision reads now preserve typed missing, malformed, identity, digest, and
future-schema evidence, including the selected revision whenever the HEAD can
be decoded. Invalid selected logical HEAD is distinct from broken causality.
Physical schema revision 2 now binds every successor manifest to the exact
verified parent-manifest digest. Candidate install requires that parent before
allocation and HEAD CAS rechecks the link. A bounded HEAD-to-root lineage reader
follows only these links, ignores unselected orphans, and fails closed on a
missing, retargeted, cyclic, or over-limit ancestor. A read-only recovery planner
now follows the same exact chain across one or more logically corrupt revisions,
selects only the first fully verified earlier revision, and leaves both ordinary
lookup and physical HEAD unchanged. Missing, already-valid, unavailable, and
future-schema outcomes stay explicit. A descriptor-pinned manifest envelope is
now verified separately from canonical payload bytes. Missing or digest-broken
selected and intermediate payloads may be crossed only through exact HEAD/child-
bound manifest digests; an unverified or future-logical manifest stops the plan.
Canonical logical schema revision 2 now owns a bounded recovery ledger. An
available plan retains every skipped corrupt revision and reason in exact
selected-to-source order; an unavailable plan retains that prefix plus the exact
root, digest, schema, ancestor-state, cycle, or bound blocker. Pure recovery
assembly copies only the fully verified source state into a fresh revision whose
exact parent is the corrupt selected revision, clears stale compaction, and
binds source revision/HEAD, selection, ordered evidence, and time. Ordinary
lookup remains corrupt until an explicit recovery transaction publishes.
A private recovery transaction now acquires the session writer fence, repeats
exact selected-envelope admission, retains selected and source manifest digests
through reopen, installs the assembled causal child without reading corrupt
parent bytes, rechecks artifact closure, and CASes only that exact envelope.
Confirmed publication, installed-unselected candidate cuts, and
published-unconfirmed HEAD-sync cuts retain exact receipts; unavailable or stale
plans allocate/select nothing. This path remains unmounted, so production
current-v1 behavior and rollback bytes are unchanged.
A private pure mount-admission oracle now consumes an already-classified
current-v1 observation and the neutral canonical lookup state. It admits only
empty, current-v1-only, or canonical-only truth; corrupt/future/conflict state,
canonical tombstones, and dual presence are distinct fail-closed results. It
owns no paths or I/O and is not connected to production routing.
A read-only observer now feeds that oracle from one descriptor-pinned session
root. Current-v1 reads are bounded and classify malformed, identity, and future
versions without changing bytes. Canonical open succeeds only for a complete
pre-existing store and performs no create, chmod, sync, repair, lock, or write;
missing, partial, future, wrong-store, corrupt-HEAD, and symlinked namespaces
stay typed and fail closed. The selected payload is not yet retained or routed.
Single-pass selection now retains the admitted value from that same observation:
current-v1 carries both the parsed snapshot and its exact source bytes; canonical
carries the already-verified logical payload. Empty carries nothing, and every
rejection—including dual presence—contains only typed admission evidence. A
backing path may change afterward without rewriting the retained value; a new
selection sees the new fault. Production routing remains unchanged.
A pure branch-at-committed-turn assembler now accepts only a committed operation
boundary, creates a fresh destination session/revision with `Missing` parent,
copies the exact chronological closed prefix and only its referenced artifact
facts, and binds source store/session/revision/current HEAD plus branch turn.
It preserves the historical creation anchor needed by carried legacy evidence,
uses a monotonic destination commit time, and clears source-only compaction and
recovery state. No permission/grant state exists in the canonical payload.
A physical copy primitive now opens and verifies an immutable source artifact
before effects, rewrites only its destination session identity, and delegates
publication to the descriptor-pinned create-only installer. Missing/tampered
source, same-session copy, and destination collision create no replacement.
A private whole-branch transaction now holds both session fences in stable
identity order, rebinds the exact caller-observed source, preflights destination
HEAD/revision/artifact absence, copies only the assembled closure, installs the
root candidate, and rechecks source plus destination closure before HEAD CAS.
Known stale/collision cases are zero-copy. Injected artifact/candidate/recheck
cuts retain typed immutable residue but no destination HEAD; a HEAD-sync cut
selects only the complete branch as published-unconfirmed.
Exact pre-HEAD residue is now retryable without deletion: an artifact or
candidate is adopted only after full byte/digest/identity comparison to the
same assembled branch, explicit namespace durability confirmation, and a second
descriptor-pinned reopen. Name-only, byte-different, malformed, or tampered
residue remains a collision with destination HEAD absent.

| Story | User story | Requirements | Acceptance evidence | Size |
|---|---|---|---|---|
| E03-S01 — canonical revision ADR | As a maintainer, I want one precise owner for transcript, terminal outcome, selected head, resume metadata, lineage, compaction binding, and artifact references. | Separate logical schema, physical store, projection/index, and export versions; define opaque revision, expected-head conflict, corrupt/future states, commit cut, migration, rollback. | ADR maps every restart-authoritative field to exactly one revision and every cache/index to disposable projection; no implementation ambiguity remains. | 2-3 cycles |
| E03-S02 — current file-store CAS | As a concurrent session user, I want stale writers rejected instead of overwriting a newer complete session. | Lease/fence plus expected revision recheck at publish; staged no-overwrite install; one revision contains a whole turn and its terminal outcome. | Stale writers receive typed conflict; crash cuts yield exactly old or new revision; current session fixtures remain compatible. | 4-7 cycles |
| E03-S03 — K01-K12 fault matrix | As an operator, I want session behavior proven at prompt, tool, terminal, compaction, fsync, artifact, and HEAD-publication boundaries. | Inject every specified cut; corrupt/future data fails closed; projection deletion/rebuild selects identical HEAD; migration/export/fork tested. | Full K01-K12 passes on current store and, before certification, one named adapter; no partial fork or mixed revision. | 5-8 cycles |
| E03-S04 — verified last-good recovery | As a user with a malformed newest snapshot, I want Kitsu to recover only to a fully verified earlier generation and explain what happened. | Never skip corrupt fragments within a canonical revision; recovery selection is deterministic; current corrupt artifact remains quarantined/recoverable; grants stay fresh. | Valid previous generation resumes with receipt; no previous generation returns typed corrupt; recovery cannot select a different logical lineage silently. | 3-4 cycles |
| E03-S05 — branch at committed turn | As a user, I want `sessions fork --at-turn N` so I can explore alternatives from an exact committed boundary. | Boundary validates complete user/tool/assistant exchange and artifact closure; lineage binds source ID/revision/HEAD; no overwrite; provider-free; fresh authority. | Valid boundary copies exact prefix/artifacts; mid-tool, missing-artifact, corrupt, stale, and destination-collision cases leave source/destination unchanged. | 4-6 cycles |
| E03-S06 — canonical compaction ledger | As a user, I want compaction to remain a validated projection of canonical history so resume/model switch cannot hide what was removed. | Bind compaction to source revision, selected model capability/tokenizer, policy, and HEAD; canonical history remains exportable; stale sidecar ignored with receipt. | Branch/retry/resume/model-switch tests preserve tool boundaries and rebuild or reject stale compaction deterministically. | 3-5 cycles |

Epic exit: X005 default store passes K01-K12. “Replaceable” still requires a
named second store adapter through the same private port; a second JSONL/file
layout is sufficient, dynamic loading is not required.

## E04 — Truthful provider dialects and immutable route generations

Outcome: Kitsu retains generic Chat Completions while proving a second genuine
wire dialect behind the same Kitsu-owned port and capability contract.

Dependencies: E02 run outcome; E00 delta audit.

Status: E04-S01/S02/S03/S04/S05 and E04 are accepted through Cycle 049.
Kitsu now owns versioned secret-free route values, bounded opaque profile
derivation, explicit compatible-Chat capabilities, one initial immutable
snapshot whose inference/catalog runtimes share a frozen credential and route
policy, and a current-generation owner whose bound handles admit every primary
chat and catalog call before entering those runtimes. Replacement fault cuts
prove stale zero-send, exact-successor publication, failed-publication
retention, and admitted-old-work drain. Default `/model` and resumed-session
model changes now publish exactly one successor before mutating live session
state; foreground and subagent operations bind their effective model once for
the whole logical run, including explicit profile overrides. Catalog-only
management generations carry no invented model or inference runtime, advertise
only catalog capability, and bind top-level `models` plus explicit doctor
probes. Primary-reviewed compatible-Chat and structurally distinct Anthropic
Messages fixtures run fully offline. A distinct native Messages implementation
now matches the exact text request/header and normalizes streamed text, usage,
terminal reason, translates exact prior tool use/results, and correlates
bounded fragmented tool arguments. Its typed HTTP, malformed/midstream, size-bound, timeout, and
cancellation fixtures now pass on the native implementation with exactly one
send. Native one-attempt telemetry now closes every send/body lifecycle with
one common terminal event. A fixture-only native route factory freezes auth
once, publishes an exact streaming/tools/usage declaration, rejects unsupported
catalog policy it cannot honor, and attributes admitted attempts to its
generation. Both compiled adapters now share one bounded transient-status,
`Retry-After`, jitter, end-to-end deadline, and attempt-lifecycle mechanism.
Native retry-to-success, deadline refusal, backoff cancellation, and admitted-
body no-retry cases pass without live selection. One generic route-handle
harness makes both compiled adapters satisfy identical neutral text/tool
output, common capability, generation-attributed terminal attempt, catalog-
absence, exact-successor, and stale zero-send assertions. A closed explicit
adapter field now selects native Messages for live chat and inference doctor;
legacy flavor-only profiles retain compatible Chat and native catalog fails
before transport. One identical product harness now drives both adapters
through success, typed HTTP failure, status, JSONL, doctor, and offline paths;
a second correlates SIGINT cancellation to the same run receipt. The harness
found and closed the native omitted-`max_tokens` defect with a bounded 4,096
default and explicit-zero pre-send refusal. X001 now passes.
A versioned route receipt binds exact route identity, optional
model, auth-source class, retry/time/catalog policy, bound-generation retry,
current-generation semantic replay, and disabled fallback without a runtime or
secret-bearing field. Status, JSONL run start, and opted-in doctor project it;
attempt generations correlate, while offline doctor constructs no route.

The accepted distinct dialect is Anthropic Messages. An endpoint alias, Ollama
profile, or compatible proxy does not count as a distinct wire.

| Story | User story | Requirements | Acceptance evidence | Size |
|---|---|---|---|---|
| E04-S01 — primary wire fixtures | As an adapter maintainer, I want protocol fixtures derived from primary specifications so wire claims are testable without live credentials. | Request, streaming text, tool use/result, usage, errors, timeout, cancellation, unsupported image, and malformed response; bound all bodies/events. | Compatible Chat control and distinct fixtures fail against the wrong adapter and run fully offline. | 3-5 cycles |
| E04-S02 — capability/revision types | As composition, I want explicit wire and model capabilities so labels cannot select hidden behavior. | Kitsu-owned adapter/profile/generation IDs; wire revision; tools, image, structured output, usage, stream, catalog capabilities; known stale/unsupported fails pre-send. | Serialization/version tests; no display-name switch in physical send paths; zero send on capability failure. | 3-4 cycles |
| E04-S03 — immutable route generation | As an operator, I want every request and auxiliary call bound to one endpoint/wire/model/auth/cache/retry/replay generation. | Freeze before awaits; same-label replacement publishes once; old work drains/cancels by declared policy; failed admission leaves prior generation active. | `X001-G1-AUX` mutates during auth, retry, replay, catalog, title, compaction, and cancellation without cross-generation sends. | 5-8 cycles |
| E04-S04 — distinct compiled adapter | As a user, I want to use the named native dialect without changing agent-loop code or losing tool/error/usage semantics. | All wire translation adapter-local; credentials provided by composition; common normalized stream; no runtime loading. | Both adapters pass the common suite; core has zero concrete wire/config/auth/catalog imports; exact rollback is source revert. | 6-10 cycles |
| E04-S05 — provider receipts and diagnostics | As an operator, I want secret-free route/capability/generation receipts so I can explain every physical attempt. | Report adapter/profile/generation, wire/capability revision, auth source class, retry/replay/fallback; never token, header, body, or secret hash. | JSONL and doctor fixtures are bounded, compatible, and correlate all sends to one generation. | 2-4 cycles |

Epic exit: X001 passes with compatible Chat plus the distinct adapter through one
port; runtime provider loading remains rejected.

## E05 — Transactional guided connection and model selection

Outcome: a fresh user can configure, inspect, select, and switch models without
hand-editing TOML or risking current working configuration.

Dependencies: E04 capability/generation model.

| Story | User story | Requirements | Acceptance evidence | Size |
|---|---|---|---|---|
| E05-S01 — no-clobber connect preset | As a new user, I want guided local/hosted/custom setup that creates a valid profile without exposing a secret or overwriting config. | Start offline; network only after explicit selection; credentials via managed hidden input/stdin, never argv/output/config; preview normalized effect; atomic no-clobber commit. | Fresh-home local and custom mock paths succeed; decline, invalid input, permission cancellation, network failure, collision, and interrupted write leave no partial config/credential. | 4-6 cycles |
| E05-S02 — unified model picker | As a user, I want one capability-aware picker across configured gateways so I can choose a valid model without memorizing IDs. | Separate configured profiles from optional catalog refresh; active model pinned; filter, recents/favorites optional only after basic selection; unsupported capabilities visible before selection. | Two-mock catalogs, offline/stale catalog, large bounded list, no-catalog gateway, current-model, redirected-output, and narrow-terminal cases agree across interactive/JSONL. | 4-7 cycles |
| E05-S03 — atomic profile/model switch | As a user, I want a failed switch to leave my current session and route usable. | Stage and validate target generation; define transcript, system prompt, context meter, compaction, subagent, and pending-turn semantics; publish once; never cross profile credentials. | Failure before publication leaves old route byte-for-byte active; success changes one generation; active/queued/background cases are deterministic. | 4-7 cycles |
| E05-S04 — onboarding readiness receipt | As an unattended operator, I want setup to finish with a truthful offline/catalog/inference readiness result. | Offline doctor default; catalog and billable inference remain separate explicit scopes; alias truth from E01; capability mismatch named. | Human/JSON reports and exits agree; no unexpected send; secret scan clean; first-success task needs no TOML editing. | 2-3 cycles |

Epic exit: controlled novice and automation fixtures complete first success and
switch recovery without implicit mutation, covering E05's applicable `PF-10`
offline/configuration/permission/terminal cases.

## E06 — Restricted stdio MCP external-tool experiment

Outcome: Kitsu can evaluate external tool demand without granting ambient
authority, adding implicit installation, or confusing process lifecycle with
containment.

Dependencies: E01 deterministic registry; E02 cancellation/terminal phases.

| Story | User story | Requirements | Acceptance evidence | Size |
|---|---|---|---|---|
| E06-S01 / A004 — bounded child supervisor | As a runtime maintainer, I want one private long-lived child owner that bounds spawn, environment, pipes, descendants, cancellation, and reap. | Separate `AuthorityProfile`; `X002-ADMIT-0` rejects executable discovery/spawn before authority admission; process group; bounded stdin/stdout/stderr; timeout; idempotent shutdown; typed residue; preserve one-shot command semantics. | Real child covers block, flood, descendant pipe, ignore-cancel, crash, clean EOF, timeout, drop, reload, shutdown, env/fs/network denial and zero pre-admission spawn; command containment regression passes. | 6-10 cycles |
| E06-S02 — frozen tool-source snapshot | As the agent loop, I want an immutable advertised tool inventory so a source cannot change schemas or handlers mid-turn. | Host-authored source/tool identity, schema fingerprint, incarnation and inventory revision; collision before publication; atomic withdrawal; per-subagent authority intersection. | Built-ins plus fixture source pass `PF-06`/`PF-07` collision, snapshot, mutation, withdrawal, child-authority, lifecycle, disabled-path, and zero-provider tests. | 4-6 cycles |
| E06-S03 — tools-only stdio MCP core | As a user, I want explicitly configured local MCP tools available without package installation or unrelated MCP capabilities. | Initialize/list/call only; one namespaced echo tool first; pinned executable/config; required/optional/disabled; protocol deadline and bounds; no HTTP/resources/prompts/sampling. | Primary protocol fixtures plus real echo child; disabled never spawns; required blocks before inference; optional degrades with receipt. | 6-10 cycles |
| E06-S04 — crash, withdrawal, and no replay | As a user, I want a crashed tool server withdrawn immediately and a possibly side-effecting call never guessed safe to replay. | EOF/crash removes descriptor and handler atomically; generation/incarnation on calls; explicit new attempt only; cancel/reload/shutdown join or name residue. | Exit-before-journal, exit-after-journal, duplicate response, late response, reconnect, cancellation, and stale-incarnation tests produce no orphan call or advertised dead tool. | 4-7 cycles |
| E06-S05 — extension receipts and doctor | As an operator, I want secret-free effective authority, source identity, tools, health, and shutdown receipts without doctor starting children by default. | Doctor metadata-only unless explicit active preflight; additive JSONL inventory; path/hash disclosure policy; required/optional distinction. | Offline doctor performs no spawn/network; active fixture reports exact bounded status; old JSONL fixtures remain compatible. | 3-4 cycles |
| E06-S06 — promotion decision | As a product owner, I want evidence before MCP becomes a supported surface. | Compare task value, startup/default cost, security failures, lifecycle reliability, maintenance burden; record retain/reshape/reject. | Separate ADR; prototype success alone cannot promote; removal leaves no config/session migration burden. | 1-2 cycles |

Epic exit: B001 plus `X002-ADMIT-0`, `PF-06`, and `PF-07` acceptance matrices
pass. Same-process npm/Python, Rust dylibs, HTTP MCP, marketplaces, hooks,
providers, and public plugin APIs remain out of scope.

## E07 — Measured semantic context and structural navigation

Outcome: Kitsu adds context intelligence only when it improves representative
repository tasks over its current search, symbol, outline, and manifest tools.

Dependencies: E00 baseline. Provider-free benchmark preparation may start
early, but delivery priority keeps the experiment after E04, E03, and E06 and
it cannot displace a failing correctness or authority gate.

| Story | User story | Requirements | Acceptance evidence | Size |
|---|---|---|---|---|
| E07-S01 — repository task benchmark | As a product owner, I want a deterministic baseline for locate/explain/change tasks so new context features are evidence-driven. | Mix languages/repo sizes; fixed prompts and budgets; measure success, rounds, prompt tokens, tool bytes, latency; no live-provider dependency for core scoring. | Current tool suite produces reproducible baseline and bounded artifacts; evaluator rubric reviewed before experiments. | 3-5 cycles |
| E07-S02 — semantic repo-map experiment | As a coding user, I want high-value symbols/files ranked into a token budget when that beats manual discovery. | Pure provider-neutral tree-sitter/reference-graph/PageRank contributor; deterministic ranking/rendering; hard token/byte cap; no authority expansion; cached projection keyed by revision. | A/B against baseline; denied paths never enter graph/output; stale/corrupt cache rebuilds; stale edit targets still require Kitsu's exact mutation checks; no task regression beyond agreed threshold. | 5-8 cycles |
| E07-S03 — promote, reshape, or reject | As a maintainer, I want the experiment removed if it does not improve task outcomes enough to justify complexity. | Predeclare thresholds; inspect failure modes and language bias; keep current primitives. | Decision record with measurements; accepted implementation has ongoing regression suite, rejected one leaves no production code. | 1-2 cycles |

Epic exit: measured decision, not assumed feature parity with Aider/Goose.

## E08 — Progressive documentation, executable onboarding, and telemetry

Outcome: users reach first success and authoritative task guidance quickly, and
can inspect route, authority, context, and Kitsu-observed usage through compact
truthful terminal and automation surfaces while Kitsu keeps its unusually
precise safety and automation contracts.

Dependencies: DOC-R001 already approved; final root rewrite after E05/E06
surface stability. E08-S05 consumes E04 capability/generation receipts;
E08-S06 consumes E03 canonical revision/fencing. E06 health is additive only
after the supervised tool-source contract exists.

| Story | User story | Requirements | Acceptance evidence | Size |
|---|---|---|---|---|
| E08-S01 — canonical ownership skeleton | As a reader, I want each subject to have one authoritative page so root and deep docs cannot drift. | Create routing index and destination skeletons; freeze content/anchor ownership map; move nothing yet. | G6 ownership/link checker passes; every old H2/subtopic has one destination and incoming route. | 3-5 cycles |
| E08-S02 — generated and executable reference | As a user, I want CLI/slash/tool/schema examples generated or tested so documentation matches the binary. | Generate Clap/slash/tool reference; validate JSONL examples; tag/test shell and TOML fences; fresh HOME/no-network fixtures. | G1-G5 and G7-G8 checks run in CI; unexpected state/network/secret fails. | 5-9 cycles |
| E08-S03 — move detailed content | As a task user, I want focused gateway/auth/authority/tool/chat/session/context/automation pages. | Populate sole owners before deleting root text; retain compatibility anchors/routes; no duplicate normative defaults. | Link/anchor/ownership checks and relevant behavioral fixtures pass after each move. | 8-14 cycles |
| E08-S04 — root rewrite | As a new user, I want scope, install, local first success, authority, common tasks, and automation visible without reading an exhaustive manual. | Follow DOC-R001 outline; preserve credential-free path and pre-authority warnings; no line-count target. | G1-G8 plus timed novice lookup tasks pass; root's first 100 lines achieve the defined first success. | 4-7 cycles |
| E08-S05 — truthful session telemetry decks | As a terminal or automation user, I want one offline, typed view of the active route, authority, context, and current-session usage so attractive output never hides provenance or uncertainty. | Extract presentation-neutral status/context/usage snapshots; classify each quantity as authoritative, reported, derived, estimated, or unavailable; add `/usage [session]`, retain `/cost` as an alias, add `/context window|sources`, and render adaptively without changing JSONL into terminal text. Consume E04/E06/E03 identities only after those owners publish them. | Compatible Chat, distinct Messages, and usage-omitting fixtures; route/model switch, compaction, subagent, rollback, cancellation, and failed-attempt cases; widths 24/40/60/80/120; styled/plain/screen-reader/redirected/JSONL; offline and secret-bearing inputs all agree on the same typed snapshot. | 6-9 cycles |
| E08-S06 — bounded local usage history | As a user of any supported provider or model, I want daily, weekly, and cumulative Kitsu-observed token activity without confusing it with account billing, credits, or limits. | Publish idempotent body-free usage observations keyed by run/request identity through E03 fencing; preserve reported/missing coverage, outcome, purpose, model and route generation; rebuild bounded aggregates as projections; add `/usage daily|weekly|cumulative`. A future account-quota source requires a separately named adapter and explicit refresh and is not on this path. | Old-or-new crash cuts, duplicate IDs, corrupt/future data, projection deletion/rebuild, incomplete provider usage, multi-model/generation attribution, timezone/DST boundaries, maximum retention, stable redirected output, and bounded JSONL points pass provider-free. | 6-9 cycles |

Epic exit: DOC-R001 G1-G8 and the applicable `PF-10` fresh-home, redirected,
narrow-terminal, accessibility, snippet, telemetry-truth, and
progressive-navigation cases pass; old links are mapped and safety/automation
contract coverage is not reduced. `/status`, `/context`, and `/usage` make no
network request; only the already-explicit `/context refresh` may discover
window metadata.

## E09 — Bounded multimodal image input

Outcome: supported providers receive explicitly authorized PNG/JPEG content
under hard disclosure and resource bounds; unsupported models fail before send.

Dependencies: E04 two truthful capability paths and explicit product priority.

| Story | User story | Requirements | Acceptance evidence | Size |
|---|---|---|---|---|
| E09-S01 — evidence and threat contract | As a security reviewer, I want exact MIME, dimension, decompression, metadata, provider-disclosure, logging, persistence, and unsupported behavior before bytes are sent. | Second accepted provider/source; choose encode/pass-through policy; define caps and redaction; no path disclosure to provider unless intended. | ADR plus hostile corpus; zero-send unsupported and invalid cases. | 2-4 cycles |
| E09-S02 — bounded image attachment | As a user, I want to attach a permitted PNG/JPEG and see a truthful disclosure receipt. | Reuse secure attachment authority; verify magic/MIME; cap input/decoded dimensions/output; no external decoder process unless separately contained. | Valid, spoofed, truncated, huge-dimension, decompression-bomb, symlink, protected path, cancel, and duplicate cases. | 5-8 cycles |
| E09-S03 — adapter/session/JSONL integration | As an operator, I want image capability, sends, session behavior, and automation receipts consistent across adapters. | Adapter-local wire mapping; explicit session storage/export policy; body-free JSONL receipts; retry/replay remains same generation. | Two adapters run supported/unsupported fixtures; no image bytes in diagnostic/log metadata; resume/export behavior exact. | 4-7 cycles |

Epic exit: bounded image path passes two adapters. Other media and image
generation remain separate proposals.

## E10 — Five-boundary certification

Outcome: maintainers can state exactly which boundaries are replaceable, which
are prepared, and which remain product-gated, using executed evidence rather
than architecture shape alone.

Dependencies: all promoted epics.

| Story | User story | Requirements | Acceptance evidence | Size |
|---|---|---|---|---|
| E10-S01 — automated coupling checks | As a maintainer, I want CI to reject concrete integration leakage into core. | Checks for provider wire/config/auth/catalog, MCP/supervisor, renderer/JSONL/terminal, CLI/env/signal, and store/filesystem/DB types by boundary. | Intended violation fixture makes each check fail; default and independent adapters compile/run without core edits. | 3-5 cycles |
| E10-S02 — default/alternate conformance | As a product owner, I want every claimed replaceable boundary proven by the default and a named independent adapter. | Run X001-X005 suites; compiled alternatives are acceptable; no dynamic loading requirement. | Score each contract, coupling, authority, isolation, version, observability, and conformance field with linked run evidence. | 5-10 cycles |
| E10-S03 — disabled/default-path parity | As an existing user, I want no cost or behavior change when optional integrations are off. | Compare process tree, network, state, events, schemas, startup, and representative output to milestone baseline. | Byte equality where contractual; otherwise approved semantic diff; zero unexpected process/network/state/schema overhead. | 2-4 cycles |
| E10-S04 — architecture and release decision | As a release owner, I want an honest promote/defer/reject decision and rollback for each boundary. | Update target/current architecture, backlog, ADRs, migration and rollback; do not call prepared boundaries replaceable. | Independent review plus full workspace/security/release suite; research audit clean. | 2-3 cycles |

Important: without a real second host/principal, X003 may finish this program as
“prepared and fixture-proven,” not “replaceable.” The plan must not invent a
desktop, IDE, listener, or service solely to improve the score.

## Thirty-minute implementation loop

The normative loop, acceptance rules, cycle-record template, and commit
convention live in [`cycle-protocol.md`](cycle-protocol.md). Every accepted
cycle ends in one focused green commit; incomplete cards are split without a
red/WIP production commit.

## Initial cycle queue

This is the deterministic starting queue after backlog approval. A cycle may be
split further during its requirements phase, but later epics cannot jump their
gates merely because a card appears attractive.

| Cycle | Card | Intended green commit |
|---:|---|---|
| 001 | E00-S01-C1 | Create branch safely, import approved plan/protocol, record clean base and preserved dirty-main status |
| 002 | E00-S02-C1 | Add invariant test manifest from current tests |
| 003 | E01-S01-C1 | Add alias/mismatch/ambiguity doctor tests and minimal local-Ollama matcher |
| 004 | E01-S01-C2 | Add human/JSON doctor fixtures and close KB-001 gates |
| 005 | E01-S02-C1 | Rebase registry error/types and same-path duplicate tests |
| 006 | E00-S05-C1 | Add the mechanism ledger, three-spine thesis, hard exclusions, gates, and revised order |
| 007 | E01-S02-C2 | Add cross-path/no-mutation/composition-boundary propagation |
| 008 | E01-S03-C1 | Add descriptor/handler and unused-result coupling gates; close E01 |
| 009 | E00-S03-C1 | Pin post-E01 evidence baseline and audit committed delta |
| 010 | E00-S03-C2 | Update current architecture/comparison/backlog/status and close E00 |
| 011 | E02-S01-C1 | Define carrier matrix and provider-free deterministic driver |
| 012 | E02-S01-C2 | Cover success, provider/tool/preflight faults, and exact exits |
| 013 | E02-S01-C3 | Map current cancellation, sink, join, and terminal-uniqueness evidence; retain measured gaps |
| 014 | E02-S01-C4 | Close confirmation/queued-cancel/residue discovery and prove the minimal EOF/human-identity seams |
| 015 | E02-S02-C1 | Introduce private identity/run types for first fixture-proven divergence |
| 016 | E02-S02-C2 | Propagate authority-input EOF as a zero-later-send interruption while explicit denial remains recoverable |
| 017 | E02-S02-C3 | Project neutral run identity/outcome into human carrier receipts and close the private contract |
| 018 | E02-S03-C1 | Add monotonic settlement/join/persist/deliver phase model |
| 019 | E02-S04-C1 | Project common outcome into JSONL compatibly and close first carrier tranche |
| 020 | E02-S03-C2 | Route one bounded noninteractive finalization path through the phase owner and add fault cuts |
| 021 | E02-S03-C3 | Model and migrate scripted join/persistence outcomes without guessing phase state |
| 022 | E02-S03-C4 | Route piped JSONL finalization through typed cleanup/persistence/delivery inputs |
| 023 | E02-S03-C5 | Audit remaining interactive/top-level terminal ownership and close or decompose the E02 exit gate |
| 024 | E02-S03-C6 | Add one process terminal owner and route duplicate plus early inference fallback cuts through it |
| 025 | E02-S03-C7 | Carry presentation-neutral terminal facts through human noninteractive carriers |
| 026 | E02-S03-C8 | Define and publish the interactive multi-operation aggregate terminal |
| 027 | E02-S04-C2 | Evaluate the seven-carrier exit matrix and close or retain one measured E02 gap |
| 028 | E04-S01-C1 | Add primary-spec control/distinct-wire provider fixtures |
| 029 | E04-S01-C2 | Extend both wire fixtures through tools, typed faults, malformed input, and hard bounds |
| 030 | E08-S05-C0 | Scope the portable status/context/usage truth model, compatibility aliases, dependencies, exclusions, and PF-10 matrix |
| 031 | E04-S01-C3 | Close provider-free timeout, cancellation, aggregate-bound, and unsupported-capability fixture cuts |
| 032 | E04-S02-C1 | Add private versioned adapter/profile/generation, wire-revision, and capability value contracts with serialization vectors |
| 033 | E04-S02-C2 | Derive opaque profile IDs and truthful current-route declarations without changing physical send behavior |
| 034 | E04-S03-C1 | Define one immutable route snapshot that owns declaration, endpoint/model/auth-source, retry/catalog policy, and initial generation |
| 035 | E04-S03-C2 | Add a current-generation route handle and migrate primary inference/catalog admission without exposing secrets |
| 036 | E04-S03-C3 | Bind model changes to exact successor publication and rebind future primary work without crossing generations |
| 037 | E04-S03-C4 | Add generation-aware attempt evidence and close the retry/replay/catalog/compaction/subagent/cancellation mutation matrix |
| 038 | E04-S03-C5 | Bind top-level models and explicit doctor probes to a truthful management route while preserving catalog-without-model behavior |
| 039 | E04-S04-C1 | Add a compiled native Messages text/usage/terminal adapter against the exact primary fixture without selecting it in live composition |
| 040 | E04-S04-C2 | Add native Messages tool-use/result request translation and bounded streamed argument correlation without live selection |
| 041 | E04-S04-C3 | Run the native Messages adapter through HTTP, stream-fault, size-bound, timeout, and cancellation parity without live selection |
| 042 | E04-S04-C4 | Add native one-attempt telemetry with terminal success, rejection, decode-failure, timeout, and cancellation dispositions |
| 043 | E04-S04-C5 | Construct a frozen native Messages generation with truthful capabilities and exercise it through the common route handle |
| 044 | E04-S04-C6 | Add bounded native transient retry/backoff and cancellation parity inside one frozen generation before live selection |
| 045 | E04-S04-C7 | Run both compiled adapters through one provider-neutral route-handle conformance harness before live selection |
| 046 | E04-S04-C8 | Add explicit compiled native adapter selection in composition while preserving legacy compatible-Chat defaults |
| 047 | E04-S05-C1 | Define one complete bounded secret-free route receipt and prove fail-closed decoding plus immutable generation truth |
| 048 | E04-S05-C2 | Project the bounded receipt through status, JSONL run start, and opted-in doctor with offline no-construction proof |
| 049 | E04-S05-C3 | Run identical two-adapter product conformance, close native default-token divergence, and accept X001 |
| 050 | E03-S01-C1 | Accept the canonical revision/logical-HEAD ADR, current-v1 ownership map, and stable K01-K12 fixture contract |
| 051 | E03-S01-C2 | Add neutral canonical session identities, HEAD/history, typed states, publication receipts, and a store-neutral boundary gate |
| 052 | E03-S02-C1 | Route unchanged current-v1 load, load-other, and list behavior through a private read port and provider-free selector fixture |
| 053 | E03-S02-C2 | Route unchanged current-v1 active save through a private write port while preserving exact install evidence |
| 054 | E03-S02-C3 | Add store-neutral expected-revision admission with typed conflict and provider-free zero-effect evidence |
| 055 | E03-S02-C4 | Execute K08 create, update, stale conflict, no-clobber create, and retry through an independent in-memory revision fixture |
| 056 | E03-S02-C5 | Execute provider-free K04 pre-candidate, staged, published-unconfirmed, and confirmed visibility cuts |
| 057 | E03-S02-C6 | Prove K07 same-session writer exclusion, token/session binding, lost-fence refusal, and independent-session progress |
| 058 | E03-S02-C7 | Compile the physical file-store v1 namespace, reversible identity encoding, immutable revision manifest, and fenced head-pointer contract |
| 059 | E03-S02-C8 | Add descriptor-pinned store create/open, staged no-replace revision installation, and exact-byte verification |
| 060 | E03-S02-C9 | Add descriptor-pinned verified HEAD reads and writer-fenced expected-revision atomic replacement |
| 061 | E03-S02-C10 | Add the closed whole-operation canonical payload and strict blocked current-v1 migration input |
| 062 | E03-S02-C11 | Compose deterministic canonical bytes, immutable candidate install, and fenced HEAD CAS with exact cut/residue truth |
| 063 | E03-S02-C12 | Define an exact current-v1 legacy baseline with fixed loss facts and typed artifact refusal |
| 064 | E03-S02-C13 | Compose strict legacy preparation with create-only fenced publication and exact rollback/collision truth |
| 065 | E03-S02-C14 | Assemble one pure canonical successor operation from neutral terminal and closed transcript/effect facts |
| 066 | E03-S02-C15 | Publish only an artifact-free assembled successor through exact stale/candidate/HEAD transaction cuts |
| 067 | E03-S02-C16 | Add session-scoped immutable artifact records plus descriptor-pinned no-overwrite install and verification |
| 068 | E03-S02-C17 | Bind complete canonical tool/effect publication to exact installed artifact closure before candidate and HEAD publication |
| 069 | E03-S02-C18 | Decode and bind selected canonical HEAD plus exact artifact closure, refusing corruption without older-revision fallback |
| 070 | E03-S02-C19 | Pair canonical read with neutral lookup state and classify current corruption plus logical/artifact schema dimensions |
| 071 | E03-S02-C20 | Type physical HEAD/revision read evidence and distinguish invalid selected HEAD from broken operation causality |
| 072 | E03-S02-C21 | Bind successor manifests to exact parent digests and verify bounded selected lineage without orphan promotion |
| 073 | E03-S04-C1 | Derive a read-only verified last-good plan across digest-bound logical corruption without changing selected HEAD |
| 074 | E03-S04-C2 | Split trusted manifest envelopes from payload verification and recover-plan across physical payload faults |
| 075 | E03-S04-C3 | Assemble a fresh causal recovery revision with ordered skip evidence and exact unavailable blockers |
| 076 | E03-S04-C4 | Publish recovery under the writer fence and exact selected envelope with old-or-new receipt truth |
| 077 | E03-S05-C1 | Assemble a fresh branch revision with exact committed prefix, lineage, and referenced artifact closure |
| 078 | E03-S05-C2 | Copy referenced physical artifacts through verified destination-scoped no-overwrite publication |
| 079 | E03-S05-C3 | Publish a complete branch create-only under ordered source/destination fences with exact cut residue |
| 080 | E03-S05-C4 | Resume branch publication only from exact re-synced artifact and candidate residue |
| 081 | E03-S02-C22 | Define pure deterministic current-v1/canonical mount admission with explicit unavailable, tombstone, and dual-presence refusal |
| 082 | E03-S02-C23 | Observe both representations through one pinned root without creating, repairing, locking, or changing either store |
| 083 | E03-S02-C24 | Retain exactly one admitted current-v1 or verified canonical value from the same observation without a second path read |
| 084 | E03-S02-C25 | Derive a revision/HEAD-bound artifact-free resume value from exact selected ancestry without changing production routing |
| 085 | E03-S02-C26 | Own legacy and canonical fences while revalidating the exact selected canonical value before activation |
| 086 | E03-S02-C27 | Initialize the canonical lock domain through the pinned root and dual-fence exact current-v1 activation |
| 087 | E03-S02-C28 | Dispatch every selected representation into one lock-owning activation and exact source receipt |
| 088 | E03-S02-C29 | Save canonical successors through the total activation's held fence and exact parent envelope |
| 089 | E03-S02-C30 | Save current-v1 successors through the total activation with exact pre-install byte and canonical-absence checks |
| 090 | E03-S02-C31 | Encapsulate both representation-aware save paths behind one non-cloneable private live-store owner |
| 091 | E03-S02-C32 | Move production active-session ownership behind one current-v1-preserving handoff seam |
| 092 | E03-S02-C33 | Publish and reopen one explicit owner-private local canonical store identity without mounting it |
| 093 | E03-S02-C34 | Bind the local composition identity to exact physical-store selection and total live activation without product routing |
| 094 | E03-S02-C35 | Admit the bound local live owner as a closed ActiveSessionStore variant while product constructors remain current-v1 |
| 095 | E03-S02-C36 | Couple one private bound active owner to the exact current/canonical resume snapshot selected under its fences |
| 096 | E03-S02-C37 | Route startup and in-process current-v1 resume through one exact owner-plus-snapshot product handoff |
| 097 | E03-S02-C38 | Unify current-v1 and private bound activation behind one exact resume-result shape without route selection |
| 098 | E03-S02-C39 | Admit only snapshot-writable bound-current resume and reject canonical state before product routing |
| 099 | E03-S02-C40 | Mount durable startup resume on the pinned bound-current route with explicit canonical refusal |
| 100 | E03-S02-C41 | Stage an exact bound-current in-process target and swap the active owner only after every fallible precommit step |

After the documentation-only Cycle 030, Cycle 031 finishes E04's primary wire
fixture contract. Cycle 032 begins capability/revision value types before
generation publication and adapter wiring. Cycle 033 makes current composition
declare the adapter and wire it actually constructs and derives
collision-resistant profile identity without storing display labels. Cycle 034
consumes that declaration into an initial immutable snapshot, freezes auth once,
and constructs inference/catalog runtimes from one endpoint/model/policy input.
Cycle 035 adds current-generation admission and the first replacement/drain
fault cuts while preserving optional-catalog absence. Cycle 036 binds default
model selection to successor publication and snapshots scoped profile models
per logical operation. Cycle 037 attributes every chat-owned physical attempt
to its immutable generation and closes retry/replay/catalog/compaction/
subagent/cancellation cuts. Cycle 038 replaces the three deliberately raw
management calls with model-bound inference or catalog-only generations and
closes E04-S03. Cycle 039 begins the compiled native Messages adapter with the
smallest complete text/usage/terminal wire slice, including explicit
message-stop truth and zero-send tool refusal. Cycle 040 adds exact tool
translation, pre-send input validation, correlated argument reconstruction,
and the 1 MiB argument bound. Cycle 041 proves the existing HTTP, stream, bound,
timeout, and cancellation fixtures on the actual adapter. Cycle 042 adds
terminal attempt observations without enabling live selection. Cycle 043 binds
the native adapter to an exact capability declaration and frozen-auth
snapshot/handle, proves generation-attributed admission and local zero-send
rejection, and refuses retry or catalog policy it cannot yet honor. Cycle 044
adds bounded retry/backoff and cancellation parity inside that generation
before any config flavor can select it. Cycle 045 makes both compiled adapters
pass one provider-neutral route-handle conformance harness before composition
can select the native wire. Cycle 046 adds an explicit, auditable native adapter
selector without silently changing legacy flavor behavior. Cycle 047 defines
the private receipt consumed by status, JSONL, and doctor; Cycle 048 projects
it additively and correlates attempts without changing the v1 required fields.
Cycle 049 runs final two-adapter receipt/attempt conformance, closes the one
discovered native default-token divergence, and accepts E04 plus X001. Cycle
050 accepts E03's canonical revision/logical-HEAD ADR and stable K01-K12 fixture
contract without changing v1. Cycle 051 adds the neutral opaque ID, HEAD,
canonical-operation/history, receipt, and typed-state values with fail-closed
vectors plus an enforced store-neutral dependency boundary. Cycle 052 begins a
private port around unchanged current-v1 read/list behavior. The queue then
advances through E03. Cycle 052 lands that read-only port and records rather
than hides v1's corrupt-entry listing omission; Cycle 053 extracts the active
save/commit side without changing bytes. Cycle 054 adds provider-free
expected-revision admission and stale zero-write conflict. Cycle 055 composes
that admission with revision minting and receipts in an in-memory K08 fixture.
Cycle 056 adds provider-free K04 publication-cut and durability truth. Cycle
057 adds K07 writer-fence loss and independent-session progress. Cycle 058
defines the physical revision envelope and no-clobber layout. Cycle 059 adds
descriptor-pinned create/open, immutable staged candidate installation, and
exact-byte verification without mounting production behavior. Cycle 060 adds
verified HEAD reads, per-session OS writer fencing, exact parent/revision
admission, atomic create/replace, and new-visible installed-unconfirmed truth.
Cycle 061 adds that complete closed payload plus a strict current-v1 decoder
that preserves exact source bytes while refusing to invent terminal outcomes or
artifact closure. Cycle 062 composes payload validation, immutable candidate
installation, and fenced HEAD CAS as one provider-free transaction, including
stale zero-allocation, candidate residue/collision, and published-unconfirmed
truth. Cycle 063 admits current-v1 history only as an exact digest-bound
`Genesis` baseline with a fixed terminal/head/lineage loss manifest, no
canonical operations, and explicit unresolved-artifact refusal. Cycle 064
composes preparation with no-clobber publication while retaining exact v1
rollback bytes, including existing-HEAD zero-allocation, conflicting-orphan
preservation, refusal before effects, and installed-unselected cut truth. Cycle
065 completes the pure neutral whole-operation assembly seam. Cycle 066
composes an artifact-free assembled successor with fenced file publication and
proves exact update, stale zero-allocation, candidate residue, and HEAD
durability truth. Cycle 067 defines descriptor-pinned immutable artifact
closure before this path can widen. Cycle 068 binds canonical artifact facts to
exact verified physical records before candidate/HEAD publication. The queue
then verifies selected-HEAD closure in Cycle 069, maps the neutral lookup in
Cycle 070, types its complete physical envelope in Cycle 071, and binds and
verifies retained selected lineage in Cycle 072. Cycle 073 begins recovery with
a mutation-free last-good plan for logically corrupt selected ancestry; Cycle
074 extends the trusted input split to selected and intermediate physical payload
faults. Cycle 075 defines a pure new recovery revision and complete skipped/
blocker evidence. Cycle 076 composes envelope-only selected-parent admission,
writer fencing, immutable install, HEAD CAS, and exact recovery receipts before
production mounting. Cycle 077 begins pure branch-at-committed-turn prefix,
lineage, and artifact-closure assembly. Cycle 078 adds verified no-overwrite
artifact copying as a separately accepted effect boundary. Cycle 079 adds the
source/destination-fenced create-only branch publication transaction. Cycle 080
adds exact residue recovery/idempotent retry for pre-HEAD cuts; then the queue
returns to E03-S02 in Cycle 081 with a pure mount-admission oracle. Cycle 082
observes current-v1 and canonical presence without creating or changing either
store. Cycle 083 retains the exact selected value from that same observation
without a second path read. Cycle 084 adds a pure canonical-to-resume projection:
metadata and provider-neutral messages follow only selected-HEAD ancestry, the
source revision/HEAD and omitted audit operations remain explicit, and artifact-
bearing history refuses because the current resume DTO cannot preserve its
immutable identity. Cycle 085 adds fence-bound canonical activation: it owns the
legacy lease and canonical fence in one order, keeps the store descriptor-pinned,
rechecks current-v1 absence, and accepts only the same fully verified canonical
payload. Cycle 086 adds corresponding current-v1 activation: under the legacy
lease it exclusively initializes or strictly adopts the canonical store, owns
that fence, rechecks exact current bytes, and requires canonical HEAD absence.
Partial concurrent stores refuse without repair. Cycle 087 unifies both variants
behind one boxed total owner with borrowed resume access and exact store/session/
source/revision/HEAD receipt; empty and rejected selections remain typed and
effect-free. Cycle 088 adds the first representation-aware live-save boundary.
Canonical activations retain their exact logical base, reuse their already-held
writer fence, bind a successor to the exact selected parent envelope, and update
their borrowed resume projection only after old-or-new publication truth is
known. Current-v1 activation returns a typed snapshot-writer requirement, and
artifact-bearing projection refuses before allocation. Cycle 089 supplies the
current-v1 half: activation retains a clone of the pinned directory descriptor,
validates stable snapshot identity, stages atomic bytes, and immediately before
rename rechecks both the exact retained source and canonical-HEAD absence.
Confirmed and installed-unconfirmed results advance the retained value; all
pre-install refusals preserve it. Cycle 090 encapsulates the complete activation
behind one non-cloneable private live-store owner. One save-intent/receipt API
routes both representations; cross-representation intent is effect-free, both
fences remain owned across valid writes, and both release only when the live
store drops. Cycle 091 moves production active-session ownership behind a
non-cloneable `ActiveSessionStore`. Autosave, clear, reset, startup resume,
in-process resume, and artifact routing use that one slot, while its only
implementation delegates to current-v1 and creates no canonical namespace.
Static catalog and lifecycle operations remain separate. Cycle 092 adds the
stable explicit local canonical store identity. A bounded schema-1 owner-private
record is create-only beneath the pinned sessions descriptor; valid concurrent
winners are adopted, corruption/future schema/unknown fields/symlinks/shared or
permissive files fail closed, and installed-unconfirmed durability stays
explicit. It remains unmounted and creates no canonical namespace. Cycle 093
binds it through one private local factory. The factory rejects an orphan
physical namespace before identity publication, uses the same `StoreId` for
dual-representation selection and activation, and retains identity evidence
beside the non-cloneable live owner. Empty, unavailable, mismatched, and corrupt
state remains effect-free or unchanged; valid current-v1 activation creates or
reopens only the exactly matching physical store. Product constructors still
select current-v1. Cycle 094 admits this bound owner as a closed private
`ActiveSessionStore` variant without selecting it in product routing. Current
bound state preserves snapshot saves and installed-unconfirmed evidence through
the existing product-shaped API. Canonical bound state refuses legacy rename,
sidecar allocation, reset, and snapshot projection without changing HEAD.
Artifact routing and rename are now fallible at the owner boundary. Public
create/resume constructors still select current-v1 and create no canonical
namespace. Cycle 095 adds an exact-id-only private activation that returns the
current or canonical resume snapshot selected under the same retained owner.
The coupled value exposes no consuming owner or writable-store accessor;
invalid identities fail before I/O, current replacement cannot return a stale
pre-activation value, and canonical projection remains bound to the owned HEAD.
Cycle 096 moves product resume handoff behind one owner-plus-snapshot
activation result while its only selected implementation remains current-v1.
That handoff now revalidates the snapshot under the legacy lease, owns it beside
the active store until one explicit product split, and is used by both startup
and in-process durable resume. Stale inputs refuse before runtime replacement;
no-session read-only resume and current-v1 bytes remain unchanged. Cycle 097
replaces the duplicate private bound wrapper with that same exact result shape.
Current product handoff alone can split into runtime parts; bound current and
canonical activations retain their owner/snapshot coupling and refuse that split
until explicit admission. Cycle 098 adds that type-state admission. Bound-current
returns a private product-writable result whose save path preserves exact current
bytes and durability truth. Canonical state returns a typed operation-writer
refusal, leaves HEAD unchanged, and releases both fences; missing state remains
effect-free. Cycle 099 mounts durable startup resume on one pinned selector-to-ID
and bound-current activation. Exact/name/`last`/`auto` return only the snapshot
selected under the owner; exact canonical state refuses before runtime parts.
Bound-current reset atomically hands off to a fresh current-v1 owner so `/new`
remains usable; canonical reset still refuses. Cycle 100 applies the same route
to in-process `/resume`. The target owner remains staged beside the old owner
while Kitsu validates gateway, directory, system context, current-session
persistence, context discovery, and provider-generation replacement. Any
failure drops only the target; success consumes the exact target snapshot and
performs one infallible active-owner swap. The polished checkpoint then feeds
the dependency-safe candidate branches in the
[parallel delivery plan](parallel-delivery-plan.md); integration acceptance and
cycle numbering remain serialized.
E08-S05's high-value `/status`, `/context window`, and `/usage session` slice is
4-7 cycles once its E04-owned identity inputs are ready; complete
telemetry/history work adds 12-18 cycles. Provider-free fixture design may
overlap, but it cannot displace a failing correctness or authority gate.

## Branch baseline receipt

Cycle 001 created `improvements/harness-roadmap` from committed `main` at
`a2ba2d5c98b17da7290a2b6b8b24c83bc2529763` in a clean linked worktree. The
original `main` worktree retained its ten pre-existing modified files and was
not stashed, reset, overwritten, or absorbed. See [`cycles/001.md`](cycles/001.md)
for the acceptance receipt.

## Deferred and rejected work

Deferred until a named consumer/evidence gate exists: bidirectional host/app
server, lifecycle hooks, WASM/WASI, a Kitsu-specific external protocol, control
plane, and further media types.

Rejected for this program: same-process full-authority npm/Python/script
plugins, a general Rust dylib ABI, runtime provider/TUI loading, implicit
marketplace install/update, name-only identity, remembered cross-session
authority, fail-open/unjoined hooks, timeout-by-abandonment, process separation
presented as containment, EOF/idle/UI state presented as success, a DB/file
tail/index presented as canonical HEAD, and any discovery path that executes or
fetches before explicit admission. The complete rationale is in the
[mechanism learning ledger](mechanism-ledger.md).
