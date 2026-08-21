# Improvement Program Status

Status: E00, E01, E02, E03-S01, E03-S04, and E04 are accepted; E03-S02 is
implemented through C41, E03-S04 through Cycle 076, and E03-S05 is in progress
through Cycle 080; E06-S01 is implemented through C4, E06-S02 through C3, and
E06-S03 has begun through C1;
E08 telemetry observability was scoped in Cycle 030 and its presentation-neutral
current-session snapshot is implemented through Cycle 106. E06-S02 now includes
provider-free monotonic inventory publication through Cycle 107, and E06-S01
binds exact runtime policy into preparation through Cycle 108 and consumes its
phase budgets in a provider-free supervisor through Cycle 111. E08-S05 now has
truthful current-run usage coverage and cached-token presence through Cycle 109,
plus adaptive presentation-only decks through Cycle 110.

| Field | Current value |
|---|---|
| Clean implementation base | `a2ba2d5c98b17da7290a2b6b8b24c83bc2529763` |
| Frozen research subject | `6c7133334dadf66a545888f4068ac82585a2768f` |
| Post-E01 runtime source pin | `986fba971ec99c9af5dcd8f69a48f507aae486af` |
| Accepted cycles/commits after this cycle | 111 / 111 |
| Complete replaceability gates | 1 / 5 |
| Closed bounded defects/transfers | KB-001 Ollama alias; A001 registry admission/coupling |
| Original main worktree | Same ten user-owned modified paths; untouched |
| Active epic/card | Cycle 112 — E08-S05-C4: collect one coherent live snapshot and mount exact offline `/usage session` without inventing JSONL/account truth |

## Epic status

| Epic | State | Evidence/next gate |
|---|---|---|
| E00 baseline/safety | **Accepted** | Clean base, invariant map, mechanism ledger, content-pinned delta, current views, cycle check |
| E01 bounded defects | **Accepted** | Cycles 003-005 and 007-008; 219 tool, 308 CLI, 146 core tests at last affected-code acceptance |
| E02 causal run/outcome | **Accepted** | One process owner, exact phase facts, interactive aggregate, and one human/JSONL exit evaluator pass the independent seven-carrier matrix; X004 still lacks a named alternate runtime adapter |
| E04 provider spine | **Accepted** through Cycle 049 | Thirty-one wire, two common route-handle, and one two-adapter product-surface conformance suite cover protocol evidence, secret-free identity, immutable request models, exact successor publication, old-work drain, status/JSONL/doctor receipts, success/fault/cancellation attempt correlation, and offline no-construction. The distinct native adapter supplies a bounded `max_tokens` default and rejects explicit zero before send. X001 passes without runtime loading or executable configuration |
| E03 session spine | **In progress**; E03-S01/S04 accepted, E03-S02 through C41, E03-S05 through C4 | Startup and in-process durable resume now select exact bound-current state. In-process target ownership is staged through every fallible precommit step and swaps once only after provider publication; canonical still refuses without an operation writer. Remaining lifecycle integration, K01-K12, and X005 remain open |
| E06 child/MCP spine | **In progress** through E06-S01-C4, E06-S02-C3, and E06-S03-C1; depends only on accepted E01/E02 | Validated inventories publish atomically through exact fences. Single-use preparation now carries exact capped runtime policy into a provider-free phase supervisor with checked deadlines, idempotent drain, termination-before-kill/reap clocks, and owned residue. Effects remain injected; next add a real bounded backend only behind these proofs. No product MCP route exists |
| E07 semantic context | Planned after three spines | Fixed-corpus relevance/token budget plus authority/stale-edit gates |
| E05 onboarding | Planned after provider surfaces stabilize | Transactional setup/picker and PF-10 first-success gates |
| E08 telemetry/docs | **In progress** through E08-S05-C3 | Pure current-session DTOs label every value with truth/provenance; run tallies retain usage coverage/cached presence; a private renderer provides adaptive styled and stable accessible/plain projections. Product snapshot collection and slash routing remain separate; durable history still consumes E03 fencing |
| E09 multimodal | Deferred | Requires two truthful capability paths and measured product demand |
| E10 certification | Blocked by design until implementations exist | Run complete default/alternate X001-X005 suites; no inference-based promotion |

## Accepted cycle ledger

| Cycle | Commit | Outcome |
|---:|---|---|
| 001 | `32449e7` | Clean linked worktree, Kitsu-owned backlog and loop |
| 002 | `8bcb878` | Preserved-invariant test routing |
| 003 | `254a8d4` | Narrow local Ollama implicit-`latest` matcher |
| 004 | `41cf17d` | Human/JSON/no-chat Ollama regression closure |
| 005 | `61a7195` | Typed same-path deterministic registry admission |
| 006 | `336d2ea` | Comparative mechanism ledger and three-spine order |
| 007 | `34e128f` | Cross-path preservation and typed CLI propagation |
| 008 | `986fba9` | Complete builtin parity and ignored-result CI gate |
| 009 | `030ed34` | Post-E01 Git/content-hash evidence baseline |
| 010 | `9b635dd` | Current architecture/comparison/status and E00 close |
| 011 | `72b5fac` | Provider-free carrier matrix and deterministic oracle |
| 012 | `9c4d2d8` | Success/provider/tool/preflight and JSONL carrier outcomes |
| 013 | `3bc6327` | Cancellation, delivery, cleanup, and terminal fault mapping |
| 014 | `310cc4a` | Approval-input starvation fix and final discovery fixtures |
| 015 | `bea3394` | Neutral run identity/event/outcome contract and boundary gate |
| 016 | `9ba4dee` | Zero-later-send authority interruption with recoverable explicit denial |
| 017 | `c16c778` | Neutral identity/outcome/assistant tuple in human completion receipts |
| 018 | `c1790a6` | Provider-free monotonic terminal lifecycle and immutable publication |
| 019 | `c28c184` | Compatible neutral outcome/assistant fields in JSONL final summaries |
| 020 | `2e8a20c` | Live ask lifecycle publication across successful/failed JSONL delivery |
| 021 | `c6d5144` | Scripted lifecycle publication with actual cleanup and persistence truth |
| 022 | `6ed375b` | Piped terminal lifecycle with status-preserving rollback cleanup |
| 023 | `46dfde6` | E02 terminal-ownership audit and evidence-backed exit decomposition |
| 024 | `217f0f8` | Process-singleton lifecycle and truthful child-free inference fallback |
| 025 | `c3b7b36` | Presentation-neutral human terminal facts and fallible delivery |
| 026 | `3aeec6d` | Interactive multi-operation aggregate and process publication |
| 027 | `d818558` | Seven-carrier production exit evaluator and E02 acceptance |
| 028 | `b4baf8b` | Primary-reviewed compatible-Chat and distinct Messages wire controls |
| 029 | `a1d70f7` | Correlated tool rounds, typed faults, malformed events, and path bounds |
| 030 | `96394f7` | Truthful status/context/usage taxonomy, stories, dependencies, and PF-10 matrix |
| 031 | `5b18de6` | One-send timeout/cancel terminal cuts, aggregate/tool bounds, and image-capability handoff |
| 032 | `a866702` | Versioned route identity/capability values and pure stale/unsupported admission |
| 033 | `c26fac4` | Bounded opaque profile derivation and truthful current compatible-Chat declaration |
| 034 | `70ab219` | Initial immutable compatible-Chat snapshot with shared frozen auth and route policy |
| 035 | `d95bf0b` | Current-generation primary handles, exact replacement cuts, and optional-catalog preservation |
| 036 | `84f9515` | Model-bound logical runs and atomic default/resumed successor publication |
| 037 | `09f38a5` | Immutable-generation physical-attempt evidence and auxiliary mutation matrix |
| 038 | `c021e37` | Truthful catalog-only management routes and doctor inference binding |
| 039 | `024b036` | Native Messages text request and streamed terminal normalization |
| 040 | `3fddc26` | Native Messages tool history translation and bounded correlation |
| 041 | `86f96d5` | Native Messages HTTP, stream, bound, timeout, and cancellation parity |
| 042 | `9299647` | Native Messages send/body terminal attempt ownership |
| 043 | `03ac9ab` | Frozen truthful native route generation and common admission |
| 044 | `d5ea219` | Shared bounded retry and native frozen-generation parity |
| 045 | `53e68dc` | Generic two-adapter route-handle conformance |
| 046 | `c78018e` | Explicit legacy-safe live native composition |
| 047 | `c4cbb0a` | Complete bounded secret-free route receipt value |
| 048 | `0ba0715` | Additive status/JSONL/doctor route receipt projection |
| 049 | `791014f` | Two-adapter product conformance and X001 acceptance |
| 050 | `1c9e5af` | Canonical session revision/HEAD ADR and stable K01-K12 contract |
| 051 | `fcf848b` | Neutral canonical session values and store-boundary gate |
| 052 | `02ea526` | Private current-v1 read port with unchanged behavior |
| 053 | `c73f734` | Private current-v1 active-save port and commit truth |
| 054 | `fa51190` | Store-neutral exact revision admission and zero-effect conflict |
| 055 | `5a7e0c2` | Provider-free K08 logical publication and receipt fixture |
| 056 | `992e93e` | Provider-free K04 old-or-new and durability cut matrix |
| 057 | `996180d` | Provider-free K07 session-bound writer fencing |
| 058 | `d265ac2` | Compiled physical-v1 layout and closed record contract |
| 059 | `e038ac5` | Descriptor-pinned immutable candidate I/O and verification |
| 060 | `f8c5e59` | Writer-fenced exact-revision physical HEAD CAS |
| 061 | `6f42e79` | Closed canonical payload and blocked current-v1 migration input |
| 062 | `971def4` | Provider-free canonical candidate/HEAD transaction composition |
| 063 | `9b92d4c` | Explicit loss-bearing current-v1 legacy baseline |
| 064 | `3c2fac0` | Create-only provider-free legacy migration composition |
| 065 | `2c2f156` | Pure neutral whole-operation successor assembly |
| 066 | `2100189` | Artifact-free assembled-successor file transaction |
| 067 | `221c25c` | Session-scoped immutable physical artifact closure |
| 068 | `388e394` | Exact canonical-to-physical artifact publication closure |
| 069 | `ccb05c9` | Verified canonical HEAD and artifact-closure read |
| 070 | `bc42bb9` | Lossless neutral canonical lookup classification |
| 071 | `fac367f` | Typed physical lookup envelope and invalid-HEAD classification |
| 072 | `18d4eeb` | Parent-manifest digest chain and bounded selected-lineage verification |
| 073 | `da377bc` | Read-only verified last-good recovery planning |
| 074 | `54dd67d` | HEAD-bound manifest-envelope recovery across physical payload faults |
| 075 | `e886f5c` | Causal recovery revision assembly and complete plan evidence |
| 076 | `fef69d2` | Fenced new-revision recovery publication and exact cut truth |
| 077 | `bd00774` | Pure branch-at-committed-turn prefix and closure assembly |
| 078 | `617b836` | Verified destination-scoped no-overwrite branch artifact copying |
| 079 | `6c5c109` | Dual-fenced create-only branch transaction and exact cut residue |
| 080 | `d94542a` | Exact verified branch-residue adoption and retry |
| 081 | `835317c` | Pure single-truth current-v1/canonical mount admission |
| 082 | `f72d483` | Pinned mutation-free dual-representation observation |
| 083 | `206ecd4` | Exact single-pass selected session value |
| 084 | `b527552` | Pure revision/HEAD-bound canonical resume projection |
| 085 | `dfcfc26` | Dual-fenced exact canonical activation revalidation |
| 086 | `232c611` | Pinned dual-fenced exact current-v1 activation |
| 087 | `da90ece` | Total lock-owning activation and exact source receipt |
| 088 | `932c7e8` | Exact held-fence canonical live-save boundary |
| 089 | `97707e1` | Exact pinned current-v1 live-save boundary |
| 090 | `4f87e0b` | Non-cloneable total private live-store owner |
| 091 | `dcfd108` | Current-v1-preserving production active-session ownership handoff |
| 092 | `632a198` | Stable explicit local canonical store identity |
| 093 | `fafe58f` | Exact local composition/physical activation binding |
| 094 | `bb8118d` | Closed bound-local production-owner variant |
| 095 | `869b40f` | Exact selected resume and bound-owner coupling |
| 096 | `98ad3e7` | Exact current-v1 product resume handoff |
| 097 | `db35d47` | Common exact current/bound resume result |
| 098 | `9f08a50` | Snapshot-writable bound-current admission |
| 099 | `bc2569d` | Pinned bound-current startup resume route |
| 100 | `6c250fe` | Staged bound-current in-process owner swap and parallel checkpoint |
| 101 | `379512e` | Frozen provider-request tool schema and handler inventory |
| 102 | `fc90c30` | Exact zero-effect child admission and ordered preparation |
| 103 | `d87119f` | Pinned bounded tools-only MCP codec |
| 104 | `74ba671` | Closed external source identities and descriptor fingerprints |
| 105 | `a8462e6` | Single-use joined external child lifecycle truth |
| 106 | `0b7b882` | Typed current-session telemetry truth and provenance |
| 107 | `6dec4c4` | Exact atomic external-inventory publication and withdrawal |
| 108 | `90101b0` | Exact bounded child runtime policy identity and preparation |
| 109 | `4b9d167` | Truthful provider usage coverage and cached-token accounting |
| 110 | this document's commit | Adaptive truthful current-session telemetry decks |
| 111 | this document's commit | Authorized monotonic child-phase supervision and owned residue truth |

## Current green gates

- The provider-wire matrix now has thirty-one offline tests covering exact text
  and correlated tool rounds, structural Chat/Messages divergence,
  wrong-adapter rejection, HTTP/mid-stream/malformed faults, path-level hard
  bounds, timeout/cancellation terminality, and the capability handoff. Three
  native cases additionally prove exact Messages request/header and normalized
  text/tool/usage/terminal output, EOF-as-failure, malformed prior-input
  zero-send, the streamed tool-argument ceiling, typed/bounded native HTTP
  failures, malformed/midstream events, line/aggregate ceilings, timeout, and
  cancellation with one send. Native telemetry additionally proves one
  terminal common event for completed body, rejection, decode failure, header
  timeout/cancellation, and accepted-body cancellation. Its frozen-route case
  adds exact capability truth, generation attribution, catalog absence, and
  structured-output zero-send refusal. Native retry cases add transient
  retry-to-success with one generation, deadline refusal, backoff cancellation,
  and terminal admitted-body no-retry. Full
  affected-suite counts are recorded in the cycle receipt.
- Six provider-route tests pin a stable secret-free v1 JSON vector, opaque
  profile identity, nonzero generation/revisions, truthful adapter/wire pairs,
  all six deterministic capabilities, fail-closed decoding, and zero-send
  stale/unsupported admission. Core still does not import concrete adapters.
- Four derivation/declaration and live CLI composition tests pin the
  domain-separated SHA-256 vector, input bounds, canonical path identity,
  route-content independence, exact compatible-Chat/native capabilities, and
  the fact that `anthropic` remains a display/config label while only the
  closed `adapter` field selects a physical wire. Live native chat and doctor
  hit `/v1/messages`; native catalog refusal is local and zero-send.
- Two common route-conformance instantiations run the same generic assertions
  against compatible Chat and native Messages: normalized text/tool output,
  streaming/tools/usage capabilities, generation-one terminal attempts,
  catalog absence without a send, exact successor publication, and stale
  zero-send all agree without adapter branches in the harness.
  The CLI now publishes the same bounded receipt through current status, all
  JSONL run starts, and opted-in doctor checks; offline doctor constructs none.
- Four provider-route receipt fixtures pin one stable v1 vector with exact
  identity, optional model, auth-source class, lossless time budgets, retry
  counts, bound-generation retry, current-generation semantic replay, and
  disabled fallback. Runtime endpoint/credential material is structurally
  absent; future/unknown/zero/oversized/catalog-contradictory values fail, and
  old bound versus current successor receipts keep their exact generations.
- Four snapshot and twelve route-handle cases plus auth,
  compatible-Chat/catalog factories, and CLI source tests pin generation one,
  bounded model/time policy, exact chat-versus-catalog capabilities,
  shared endpoint/path/policy construction, and one sensitive credential across
  later environment mutation. `ChatSetup` owns one current publisher; ask/chat,
  foreground/subagent/compaction inference, model catalog, context discovery,
  and resume paths validate bound generation before runtime entry. Optional
  catalog absence remains a truthful no-send `None`. Default and resumed model
  changes publish an exact successor; foreground/subagent/profile work retains
  one model-bound handle for its logical run.
- Route handles now decorate transport-only attempt events with their admitted
  generation. Provider-free fixtures prove old inference and catalog retries
  drain entirely on generation one after generation two is published;
  cancellation retains generation one; semantic `/retry` binds generation two;
  compaction, runtime catalog JSONL, and bounded subagent telemetry preserve the
  same evidence. Top-level `models` now emits the same generation attribution.
  Catalog-only handles have no model, inference runtime, or chat method; stale
  replacement is zero-send. Explicit doctor inference and catalog probes bind
  matching immutable routes, while offline doctor still constructs none.
- One product-level harness now executes identical compatible-Chat and native-
  Messages success, HTTP-fault, status, JSONL, inference-doctor, and offline-
  doctor assertions. A second identical harness sends SIGINT through both
  adapters and correlates the cancelled attempt with the run-start receipt.
  The only divergence it exposed was native Messages omitting required
  `max_tokens` when the CLI option was absent; omission now maps to a bounded
  4,096-token wire default while explicit zero fails before transport. This
  completes `X001-G1-AUX` and makes X001 the first passing replaceability gate.
- Workspace/all-target check passes with ordinary flags and with
  `RUSTFLAGS=-Dunused-must-use`.
- Workspace/all-target Clippy passes with warnings denied; formatting and diff
  checks pass.
- Original dirty main status remains the same ten paths recorded in Cycle 001.
- Frozen research worktree is clean at its exact pin; eight non-versioned
  research decision inputs are SHA-256 pinned.
- `scripts/check-roadmap.sh` verifies sequential receipts, required fields, and
  one cycle file per improvement commit on this active branch.
- E08-S05/S06 now have a provider-neutral command/truth contract: local status
  and context remain offline; usage is Kitsu-observed provider reporting, never
  inferred billing/account truth; durable calendar views wait for E03.
- E03 now has an accepted ownership ADR before persistence changes. It forbids
  paths, tails, rows, timestamps, digests, leases, projections, and exports from
  acting as revision or logical HEAD; maps current v1 fields and missing facts;
  defines typed `Missing/Present/Corrupt/Unsupported/Conflict` reads and exact
  not-published/published-unconfirmed/confirmed commit results; and freezes the
  backend-neutral K01-K12 meanings used by every later store adapter.
- Six provider-free session-contract cases now pin bounded typed identities,
  terminal-driven HEAD advance/retention, exact versioned history, ancestry
  and duplicate rejection, closed lookup states, and publication receipts.
  Adversarial decoding rejects future revisions, unknown fields, malformed
  operation heads, and invalid CAS states. CI enforces the store-neutral module
  boundary, and a negative fixture proves the guard fails on concrete core
  orchestration dependencies.
- Public current-v1 load, load-other, and list now cross one private read port.
  A provider-free in-memory fixture pins `last`, exact-name, `auto`, active
  exclusion, and zero-read self-selection behavior. The current file fixture
  pins valid listing plus exact corrupt-read failure while retaining and naming
  the legacy omission of corrupt entries from list projections. All 30 session
  module cases pass; production bytes and mutation paths are unchanged.
- Active current-v1 save now crosses one private write port. A provider-free
  fixture proves exact candidate transfer, pre-install refusal with no receipt,
  and future-version refusal before adapter entry. Existing descriptor-backed
  cases prove successful exact-inode receipt and post-install sync failure retain
  the installed inode. Bytes and replace behavior are unchanged; rename/fork/
  archive/restore/delete remain direct.
- `ExpectedRevision::admit_observed` now owns the backend-neutral comparison:
  `Missing` matches only absence and `Exact(A)` only observed `A`. Present-on-
  create, changed, and disappeared observations return an exact typed conflict.
  A provider-free counter fixture proves all conflict paths perform zero store
  effects; current v1 is not falsely assigned a revision or mounted to CAS.
- An independent test-only memory store composes that admission with opaque
  successor allocation, selected HEAD publication, and a confirmed commit
  receipt. K08 create A, update B, stale A-versus-B, retry from B to C, and
  no-clobber create all preserve exact revision/mint/publication counts. This is
  logical conformance evidence only; it is not a named product store.
- The memory fixture is now shared by K04 and K08. K04 cuts before candidate and
  after staging leave the old revision/HEAD canonical and return not-published;
  a cut after logical publication returns the exact new receipt as published-
  unconfirmed; confirmation returns that new revision as confirmed. Candidate
  allocation never makes an uninstalled revision canonical.
- K07 adds a provider-free session-bound fence table. Exactly one token owns a
  session while another session can acquire and publish independently. Tokens
  for another session, revoked tokens, and generations replaced by a successor
  fail before mint/publication; only the exact current generation advances the
  existing revision and HEAD.
- The private compiled physical-v1 contract places new state under a disjoint
  `.kitsu-store-v1` namespace. Lowercase unpadded base32 makes every neutral ID
  reversible, case-folding-safe, non-special, and portable in length. Store and
  revision records are create-new; only a manifest-digest-bound head pointer is
  replaceable after fence/revision recheck. Fixed vectors reject future/unknown,
  malformed digest/size, self-parent, and physical identity mismatch. Nothing is
  mounted or written yet, so current-v1 behavior remains exact.

## Next delivery tranche

Cycles 011-027 build E02 in bounded order. Cycles 011-014 completed carrier
discovery across success/provider/tool/preflight, cancellation, sink/terminal,
confirmation, queued-cancel, and cleanup-residue cases. The mapping proves two
production divergences—authority EOF lacked a zero-send interruption and human
carriers lacked run identity. Cycles 015-017 close both through one neutral
contract. Cycle 018 adds the unwired monotonic terminal phase owner. Cycle 019
projects the common outcome and assistant disposition into compatible JSONL.
Cycle 020 makes the structurally child-free/session-free ask finalizer the first
live lifecycle carrier. Cycle 021 adds scripted cleanup/persistence truth
without inferring from exit or session ID; Cycle 022 applies that seam to every
piped JSONL terminal branch. Cycle 023 proves that those helpers still own
separate lifecycle instances and that early inference fallback plus human paths
bypass phase publication. Cycle 024 adds the one process owner and child-free
fallback cut. Cycle 025 carries exact cleanup/persistence facts through human
noninteractive paths, makes timing delivery fallible, and records unproven
post-start cleanup as `NotObserved`. Cycle 026 defines the interactive aggregate,
preserves retry/queue/cancel/interruption and background/session facts, and
publishes it through the process owner. Cycle 027 assigns all seven carriers
before inference preflight, evaluates exit only from the immutable publication,
and passes the independent provider-free semantic/phase matrix. E02 is accepted.
Cycle 028 pins the current compatible-Chat control and a genuinely distinct
Anthropic Messages text wire from primary specifications, proves their exact
request/event divergence, and makes wrong-adapter EOF fail closed. Cycle 029
adds exact correlated tool-result histories and fragmented tool calls, distinct
HTTP/mid-stream error bodies, malformed events, and 64 KiB error-body plus 4
MiB SSE-line rejection on both configured paths. Documentation-only Cycle 030
scopes adaptive `/status`, focused `/context`, `/usage session` plus bounded
local history without inventing portable account/billing truth. Cycle 031
closes E04-S01 with one-send timeout/cancellation terminal events, 16 MiB
aggregate and 1 MiB tool-argument cuts, a native-Messages overflow vector, and
an unsupported-image zero-send observation. Cycle 032 adds private versioned
identity, wire-revision, and capability value contracts before send wiring.
Cycle 033 derives opaque profile identity and maps current configuration to the
compatible-Chat route it actually constructs; a configured `anthropic` flavor
must not falsely select the incomplete native Messages adapter.
Cycle 034 begins E04-S03 with one immutable snapshot owning the declaration,
endpoint/model/auth-source, retry/catalog policy, both compatible-Chat runtimes,
and initial generation. Cycle 035 introduces a current-generation route owner
and bound handle, migrates primary inference/catalog/context calls, proves
stale and invalid replacement are zero-send/no-mutation, and lets admitted old
work drain. Cycle 036 makes every request model agree with its immutable handle,
publishes default and resumed model changes before live session mutation, and
resolves one scoped binding per subagent run after profile selection. Cycle 037
puts generation attribution outside adapter-authored transport events and
closes the chat-owned retry/replay/catalog/compaction/subagent/cancellation
mutation matrix. Cycle 038 gives catalog management a capability-narrow
snapshot with no model or inference runtime, migrates top-level `models` and
explicit doctor catalog through it, and binds the billable doctor inference
probe to a model-bearing generation. E04-S03 is accepted; Cycle 039 starts the
compiled native Messages text/usage/terminal path without enabling it in live
composition. The exact request/header, normalized text/usage/terminal stream,
explicit message-stop requirement, bounded bodies/events, and zero-send tool
refusal now pass. Cycle 040 adds native tool-use/result translation, pre-send
input validation, bounded block-index correlation, and the 1 MiB streamed
argument ceiling while keeping production selection disabled. Cycle 041 runs
the actual native adapter through the remaining pinned fault and lifecycle
fixtures; all pass without implementation churn or a second send. Cycle 042
adds exact terminal attempt observations before route composition: send and
accepted-body guards make cancellation explicit, and a 2xx header is never
success. Cycle 043 constructs the first immutable native generation and common
route binding while keeping configuration selection disabled. Its exact
streaming/tools/usage declaration, once-frozen auth, zero-retry/no-catalog
policy, generation-attributed attempts, optional-catalog absence, and
structured-output zero-send refusal all pass offline. Cycle 044 adds the
bounded retry/backoff lifecycle without crossing that frozen generation. It
extracts compatible Chat's transient taxonomy, capped jitter, deadline, and
attempt guards into a provider-neutral owner; native 529-to-success, deadline
refusal, backoff cancellation, and admitted malformed-body no-retry cuts pass.
Cycle 045 runs both compiled adapters through one common route harness.
The generic harness passes unchanged for compatible Chat and native Messages,
covering neutral text/tool deltas, shared capabilities, generation-attributed
terminal attempts, optional-catalog absence, exact successor publication, and
stale zero-send. Cycle 046 adds explicit live composition without changing
legacy flavor defaults implicitly. One closed compiled enum delegates the
provider port, immutable route declarations select exact wires, native doctor
uses its route identity to choose streaming, and unsupported native catalog
discovery fails locally before transport. E04-S04 is accepted.
Cycle 047 begins E04-S05 with the complete provider-neutral receipt value.
Snapshot, inference handle, catalog handle, and current owner can project the
same bounded facts without serializing a runtime. Exact vectors and replacement
fixtures prove that receipt generation follows its immutable binding rather
than whichever generation is current when it is rendered.
Cycle 048 projects that value without widening it. `/status` reads the current
generation, including an immediately published `/model` successor. All three
JSONL run-entry paths capture their start generation, provider attempts
correlate by generation, and doctor distinguishes offline no-construction from
an opted-in bound inference or catalog route. The v1 JSONL field is additive,
so legacy records remain valid.
Cycle 049 runs the same product surface against both compiled adapters. Success,
typed HTTP failure with body redaction, cancellation, status, JSONL start,
inference doctor, and offline doctor all preserve one immutable receipt/attempt
generation. The red harness found and closed the native omitted-`max_tokens`
defect. Static audits confirm core has no concrete adapter import, composition
has no dynamic loader, and Kitsu has no title-generation provider call to bind.
E04 and `X001-G1-AUX` are accepted; Cycle 050 starts E03's canonical session
revision/HEAD contract.
Cycle 050 accepts that contract without mutating v1. One self-validating
canonical revision owns transcript operations, semantic terminal outcomes,
selected logical HEAD, resume intent, lineage, immutable artifact references,
and compaction binding. Expected-revision recheck complements the writer lease;
five schemas remain independently versioned; projections and exports are never
canonical; corrupt-current recovery is explicit; and K01-K12 now names stable
whole-operation, physical-cut, concurrency, artifact, authority, recovery, and
adapter-parity assertions. Cycle 051 implements the neutral opaque values,
logical history, typed states, publication receipts, fail-closed decoding, and
store-neutral coupling guard. Cycle 052 extracts a private store port around
unchanged current-v1 reads and listing before any format migration. Cycle 053
extracts current v1 active save/commit without claiming revision CAS. Cycle 054
adds provider-free expected-revision admission and stale zero-write conflict.
Cycle 055 composes admission, opaque successor minting, publication, and receipt
truth in an in-memory K08 fixture before changing production bytes. Cycle 056
adds provider-free K04 cut points and confirmed versus published-unconfirmed
durability truth. Cycle 057 adds writer-fence loss and independent-session
progress. Cycle 058 defines the physical revision envelope and no-clobber layout
without mounting it. Cycle 059 adds descriptor-pinned store create/open, exact
manifest/payload digest verification, staged OS-level no-replace installation,
collision cleanup, and installed-unconfirmed durability truth. Cycle 060 adds
verified HEAD reads, a real per-session OS writer fence, exact current/parent
admission, atomic create/replace publication, and new-visible post-install sync
truth. Cycle 061 adds a bounded closed canonical codec for whole terminal
operations, selected ancestry, exact artifact/effect closure, and compaction
generation bindings. Its separate strict current-v1 decoder preserves the exact
source digest/transcript but exposes missing-terminal and artifact-resolution
blockers, so it cannot publish an inferred history. Cycle 062 composes these
logical bytes with immutable candidate installation and fenced HEAD CAS in one
provider-free transaction, including stale zero-allocation, orphan/collision,
and published-unconfirmed truth. Cycle 063 permits only a typed, digest-bound
legacy baseline at `Genesis`: it preserves the exact validated transcript and
declares unavailable terminal outcomes, logical heads, and turn lineage. It
creates no operations or authority, and unresolved artifact closure refuses.
Cycle 064 composes that candidate with the no-clobber transaction while keeping
the exact current-v1 bytes as rollback. Exact baseline publication, repeat/
already-present zero-allocation refusal, conflicting-orphan preservation,
pre-candidate artifact refusal, and post-candidate residue all pass without
mounting production behavior. Cycle 065 next assembles one complete new
operation from neutral E02 terminal/transcript/effect facts.
Cycle 065 adds that pure assembler: committed success selects its exact turn;
discarded provider fault retains the prior HEAD; message/effect/artifact closure,
fresh revision, and monotonic time are mandatory; stale compaction is cleared.
Cycle 066 next sends an artifact-free assembled successor through the existing
fenced transaction with stale and crash-cut evidence. That composition now
passes exact confirmed update, stale zero-allocation, pre-HEAD residue, and
post-HEAD unconfirmed visibility; artifact-bearing history refuses before store
effects. Cycle 067 next defines physical immutable artifact closure.
Cycle 067 lands that private closure: portable encoded paths, closed identity/
schema/length/digest/media records, staged no-overwrite install, exact read, and
missing/collision/future/retarget/tamper/symlink/durability-cut truth. Cycle 068
binds one complete canonical tool/result/effect operation to those records.
Every schema/identity/length/digest/media fact must match before candidate
allocation and is rechecked after candidate install before HEAD CAS. Missing,
tampered, and unequal closure allocates nothing; the recheck cut preserves old
HEAD with an exact confirmed installed-unselected candidate. Cycle 069 next
opens only the selected canonical revision, decodes and binds its logical schema/
store/session/revision/parent to the physical envelope, and applies the shared
artifact predicate. True HEAD absence is distinct from invalid logical bytes,
identity disagreement, missing closure, digest tamper, and self-consistent but
unequal physical replacement; none scans or falls back. Cycle 070 next maps
these private results into the neutral `Missing`/`Present`/`Corrupt`/
`Unsupported` lookup vocabulary. Cycle 070 retains the decoded payload only with
its exact `Present` revision/HEAD, maps malformed, identity, causality, reference,
and digest evidence to typed corruption, and maps future logical, artifact, and
artifact-manifest physical schemas to explicit dimensions. Zero schema is
malformed. Cycle 071 next types failures that happen before canonical decode in
the physical HEAD/revision envelope and separates invalid logical HEAD from
other closed-operation causality failures. Cycle 071 carries typed missing-
reference, malformed, identity, digest, and future-schema evidence through that
physical envelope while preserving a decoded selected revision. It maps an
unknown selected logical HEAD to `InvalidHead` while keeping operation ancestry
failures as `BrokenCausality`. Cycle 072 next verifies exact retained revisions
and enumerates bounded lineage without treating a candidate, directory tail, or
projection as canonical HEAD. Cycle 072 advances the private physical schema to
revision 2 so every successor binds its exact verified parent-manifest digest;
install requires the parent and CAS rechecks the link. Its bounded reader follows
only HEAD-to-root digest links, validates every canonical/artifact closure,
ignores orphans, and returns no partial lineage for missing, retargeted, cyclic,
or over-limit ancestry. Cycle 073 next derives a read-only verified last-good
recovery plan without publication or implicit HEAD replacement. Cycle 073 walks
that exact chain across one or more logically corrupt revisions, returns the
first fully verified earlier revision, reports missing/valid/unavailable/future
states explicitly, and leaves ordinary lookup plus physical HEAD unchanged.
Cycle 074 next separates verified manifest-envelope access from canonical-byte
verification so selected payload/digest faults can use the same bounded plan
without trusting corrupted bytes. Cycle 074 keeps the descriptor-pinned revision
directory and verified manifest distinct from untrusted payload bytes. Missing or
digest-broken selected and intermediate payloads can now reach a fully verified
ancestor through exact manifest links; manifest retargeting and future logical
ancestry remain unavailable, ordinary lookup stays corrupt, and HEAD is unchanged.
Cycle 075 defines the pure new recovery revision and complete skipped/blocker
evidence required before any explicit fenced publication can exist. It advances
the private canonical logical schema to revision 2 and makes the
recovery ledger restart-authoritative. Available plans retain every corrupt
revision from the selected HEAD toward the verified source in exact order;
unavailable plans retain the same prefix plus the exact root, digest, schema,
ancestor-state, cycle, or bound blocker. Pure assembly copies only the verified
source value into a fresh revision whose exact parent remains the corrupt
selected revision, clears stale compaction, and records selected/source/HEAD/
time evidence. It never moves HEAD backward. Cycle 076 composes this value
with envelope-only selected-parent admission, the writer fence, immutable
candidate install, HEAD CAS, and exact old-or-new recovery receipts. Both the
selected and verified-source manifest digests survive planning through reopen.
The transaction can recover across a missing selected payload without trusting
it, while stale envelope CAS, unavailable ancestry, and the candidate cut leave
the corrupt selected HEAD unchanged. Successful publication selects only a new
recovery revision; a post-HEAD sync cut exposes that same new revision with
`PublishedUnconfirmed`, never an old source revision. Cycle 077 begins
branch-at-committed-turn with pure prefix/lineage/artifact-closure assembly. It
accepts only a committed operation, truncates the exact chronological prefix,
filters the immutable artifact manifest to facts referenced by that prefix, and
binds source store/session/revision/current HEAD plus branch turn into fresh
destination lineage. The historical creation anchor remains valid for a carried
legacy baseline, while the new commit time is monotonic. Compaction and recovery
logs are source-only derived/provenance state and are cleared. Cycle 078 adds a
verified destination-scoped artifact-copy primitive: it rejects same-session
copy, fully verifies source manifest/content before effects, preserves schema,
media type, and bytes while rewriting session identity, and publishes only via
the existing atomic no-overwrite installer. Missing or tampered source creates
nothing; destination collision preserves the first exact artifact. Review split
whole-branch transaction ownership into Cycle 079, which next adds source and
destination fencing plus create-only candidate/HEAD publication cuts.
Cycle 079 composes the pure branch and verified-copy boundaries into one private
transaction. It acquires source/destination fences in stable identity order,
reopens the caller-observed source exactly, proves destination HEAD/revision and
the complete artifact set absent, copies only branch closure, installs a root
candidate, then rechecks source and destination closure before create-only HEAD
CAS. Known stale/collision cases copy nothing. Post-copy/candidate/recheck cuts
retain exact artifact/candidate residue but no destination HEAD; a HEAD-sync cut
selects the fully verifiable branch as `PublishedUnconfirmed`. Cycle 080 next
admits only exact matching residue for safe retry after those pre-HEAD cuts.
Cycle 080 makes those cuts restartable without deletion or overwrite. A retry
preflights the complete destination set, accepts only physical records whose
identity, schema, parent, manifest digest, canonical bytes, and artifact facts
match the same assembled branch, re-syncs the target plus owning namespaces,
and reopens before adoption. Artifact-only, confirmed-candidate, unconfirmed-
artifact, and unconfirmed-candidate residue all converge to the same complete
branch; tampering remains blocked with HEAD missing. Cycle 081 returns to the
production-mount prerequisite and defines deterministic current-v1/canonical
admission before any live write routing changes.
Cycle 081 supplies that decision as a total path-free function. Clean missing,
current-v1-only, and canonical-only states are the only admissions. Legacy
malformed/identity/future-version observations, canonical corrupt/unsupported/
conflict observations, canonical tombstones, and valid dual presence stay
distinct fail-closed results retaining both inputs. The card opens and mutates
nothing. Cycle 082 next builds the mutation-free observation seam without
implicitly creating the canonical namespace.
Cycle 082 opens both formats relative to one pinned root. Current-v1 observation
is no-follow, bounded, version/identity classified, and byte-preserving. The
canonical store opens only if its complete record and namespaces already exist;
absence is missing, while partial, wrong-store, future-schema, corrupt selected,
and symlinked state remains a typed rejection. Whole-tree snapshots prove no
namespace, byte, staging entry, or lock changes. Cycle 083 next retains the exact
selected value from the same observation so a later route cannot introduce a
second-read race.
Cycle 083 retains the exact admitted value from that observation. Current-v1
owns its parsed snapshot and original bytes; canonical owns the verified logical
payload. Empty and all rejected cases own no selectable value. Backing-path
mutation after selection cannot rewrite the retained object, and a new selection
detects the changed state. Cycle 084 next builds the pure canonical resume
projection; production activation must still revalidate under its owning fence.
Cycle 084 derives that value without I/O. It revalidates the closed payload,
binds the result to exact source revision and selected HEAD, maps resume metadata
and provider-neutral message/tool-call facts in chronological selected-ancestry
order, and reports fixed legacy losses plus audit operations omitted from model
context. Artifact-bearing history fails with a typed refusal because the current
resume DTO cannot retain immutable artifact identity. Cycle 085 next adds exact
fence-bound activation revalidation; live read/save routes remain unchanged.
Cycle 085 acquires the existing legacy session lease and canonical writer fence
in one stable order and retains both with the descriptor-pinned store. Only
after both are owned does it recheck current-v1 absence, rerun full canonical
HEAD/payload/closure verification, and compare the exact selected logical value.
A newly published HEAD, corrupt payload replacement, newly present current-v1
snapshot, wrong/missing store, or busy fence refuses. Cycle 086 next supplies
the corresponding two-domain current-v1 activation; production routes still do
not consume either private activation.
Cycle 086 acquires the legacy lease, exclusively initializes the missing
canonical namespace relative to the pinned root (or strictly verifies a complete
concurrent store), then owns the canonical fence. It byte-compares the selected
current snapshot and requires canonical HEAD absence. Valid current replacement,
canonical publication, partial/wrong store, lock contention, and ancestor
retargeting fail closed without repair or session-byte changes. Cycle 087 next
normalizes both private variants into one activation owner before live routing.
Cycle 087 provides that total dispatcher. Current and canonical activations are
boxed under one owner that exposes only borrowed resume data and an exact store/
session/source receipt; canonical receipts include revision and selected HEAD.
Empty and rejected selections return their original typed evidence without
effects, and concrete activation failures remain nested. Both fences stay owned
until the total activation drops. Cycle 088 adds a canonical save method to that
total owner. It assembles and projects without effects, verifies the retained
logical base against the selected physical candidate, and installs/CASes the
successor against the exact parent envelope through the already-held fence.
Successful and published-unconfirmed commits advance the retained base and
borrowed resume projection; not-published outcomes leave both unchanged. Wrong
fences, stale or same-revision replaced envelopes, and artifact-bearing
projection refuse before candidate allocation. Current-v1 returns a typed
snapshot-writer requirement, so it cannot accidentally create dual truth.
Cycle 089 implements that exact current-v1 save half. Activation clones the
pinned session directory descriptor before taking ownership, validates that a
successor preserves version/id/creation identity, and encodes it before write
effects. It checks retained bytes and canonical absence before staging and again
after the temporary file is synced immediately before descriptor-relative
rename. A clean commit or installed-but-directory-sync-unconfirmed commit
advances the retained snapshot and exact source bytes; invalid candidates,
pre-install replacements, and canonical appearance do not. Ancestor replacement
cannot redirect the write, and canonical activation rejects a current snapshot
without effects. Cycle 090 wraps that complete activation as the only private
live-store boundary. Its one owned save intent returns a representation-tagged
receipt, while a mismatched intent returns the existing typed requirement with
zero tree change. Borrowed resume data and the activation receipt cannot escape
the owner; both the legacy lease and canonical writer fence remain held across
successful current and canonical writes and are released only on drop. The
underlying activation constructor and save methods are module-private.
Production remains unchanged. Cycle 091 introduces one non-cloneable production
`ActiveSessionStore` slot. Every active autosave, required save, clear/reset,
startup resume, in-process resume, and artifact-directory call now consumes that
type. Its only implementation delegates to the existing current-v1 store, so
snapshot bytes, exact installed-inode evidence, lease behavior, names, reset,
and resume validation remain unchanged and no canonical namespace is created.
Catalog/list/export/archive/delete stay on the static lifecycle surface. Cycle
092 adds that explicit stable local identity as a separate unmounted schema-1
record. It mints one bounded opaque `StoreId`, installs owner-only bytes
create-only beneath the pinned sessions descriptor, and exactly reopens them.
A loser adopts only a completely valid concurrent winner. Malformed, zero,
future, unknown-field, invalid-identity, symlink, permissive, or multiply linked
state is never repaired. A visible install followed by directory-sync failure
returns the exact identity with a durability warning, and ancestor replacement
cannot redirect publication. No physical canonical namespace is created. Cycle
093 binds that composition identity to physical selection and activation through
one private factory. With no identity, it refuses an orphan physical namespace,
empty session, or unavailable current snapshot before publication. Otherwise it
uses the same exact `StoreId` for a fresh selection and total activation, then
retains the identity receipt beside the non-cloneable dual-fenced live owner.
Current-v1 save and reopen preserve the same composition and `store.json`
identity. Existing mismatch and corrupt identity bytes remain unchanged. Product
constructors still select only the current-v1 `ActiveSessionStore`. Cycle 094
adds the bound owner as a closed private variant. Current-bound state maps clean
and installed-unconfirmed snapshot commits to the existing product contract and
retains both fences. Canonical-bound state refuses legacy rename, reset, sidecar
artifact paths, and snapshot saves without moving HEAD. Rename and artifact
routing are fallible at the owner boundary, while public create/resume
constructors remain byte-exact current-v1 and create no canonical namespace.
Cycle 095 adds an exact-id-only private activation result that owns both the
bound `ActiveSessionStore` and the exact resume snapshot selected after local
identity binding and dual-fence activation. A current snapshot replaced after a
prior load returns only the replacement; canonical projection remains bound to
the owned HEAD. Invalid identity fails before filesystem effects. The wrapper
exposes read-only id/name/artifact and saved-value views, but neither a consuming
owner nor a store reference whose `&self` save could make the snapshot stale.
Public create/resume constructors still select current-v1. Cycle 096 routes
both startup and in-process durable resume through one public current-v1 handoff
that owns the revalidated snapshot and its active store until a single explicit
product split. Stale preloaded data fails under the lease before runtime state
changes, while no-session resume keeps its read-only behavior. The old public
store-only resume constructor is gone, so product call sites cannot discard the
exact snapshot half. Cycle 097 removes the duplicate bound-only wrapper and
returns the same `ActiveSessionResume` from exact bound activation. Its snapshot
remains borrowed while the owner holds both fences. The only consuming split is
explicitly current-v1 and rejects either bound source, releasing locks without exposing a
writable store. Product call sites remain on the current constructor. Cycle 098
adds admission for bound-current as a private type-state wrapper. It inspects the
exact owned activation receipt: current-v1 becomes product-writable and saves an
exact successor through the bound owner; canonical returns
`CanonicalOperationWriterUnavailable` before yielding either field. Refusal
leaves HEAD unchanged and releases both lock domains, while missing state creates
nothing. Product call sites remain current-only. Cycle 099 exposes the admitted
route only to durable startup. A single pinned
sessions descriptor resolves exact/name/`last`/`auto` to an ID and activates it;
the preliminary catalog value never crosses into runtime. Product messages use
only the exact snapshot owned under both fences. Exact canonical state returns a
bounded operation-writer error and yields no parts. Review also closed a reset
regression: bound-current `/new` now creates a fresh current-v1 owner before
dropping the old bound owner, while canonical reset remains unsupported. Cycle
100 performs that in-process activation as a staged transaction. The target and
old owners coexist while gateway/directory/system checks, current-session save,
context discovery, and provider route publication can fail. Every refusal drops
only the target. Success consumes the exact target once, swaps the owner without
a second read, and preserves the read-only no-session path. The consolidated
checkpoint then opens bounded parallel candidate packets under
`parallel-delivery-plan.md`; only the integration branch assigns accepted cycle
numbers and updates this ledger.
Cycle 101 begins the E06 integration sequence with one owned request inventory.
The provider sees descriptors from that exact snapshot, and every structured or
recovered call plus output-offload decision resolves through its same immutable
handler identities. A later registry composition cannot change an in-flight
request. The inventory is deliberately non-cloneable at its public boundary;
static and descriptor-free routes retain their existing behavior. No external
source identity, process, protocol, configuration, or product authority is
introduced.
Cycle 102 adds only the private admission and preparation seam for the future
child supervisor. Disabled and denied entries short-circuit before version or
syntax inspection; invalid paths, digest, argv, working directory, and every
profile mismatch cause zero injected host operations. An exact admitted value
then permits canonicalization, digest verification, and containment preflight
in that order, stopping at the first fault. The prepared identity exposes no
spawn operation and is not connected to configuration or product composition.
Cycle 103 adds the protocol-only third foundation. A fail-stop decoder bounds
newline-delimited JSON before parsing. The client pins MCP `2025-11-25`, emits
empty client capabilities, requires the tools server capability, rejects every
other capability and `tools.listChanged=true`, and permits only one positive
integer-ID initialize/list/call request at a time. It validates the response
envelope and minimum list/call shape but deliberately does not validate or
publish descriptors. There is still no process, transport, deadline,
configuration, live inventory, or product MCP path.
Cycle 104 adds the identity values the publication layer must consume. Source,
incarnation, revision, and tool IDs remain distinct even when display names are
unchanged; the revision is non-zero but does not yet claim monotonic admission.
The validator stages the whole bounded inventory, rejects duplicate host IDs and
advertised names, validates descriptor shape and resource limits, and detects a
synthetic digest collision before returning anything. Fingerprints use one
domain-separated, length-framed Kitsu canonicalization with sorted object keys
and ordered arrays. No configuration, child trust, publication, withdrawal,
process, or MCP composition is introduced.
Cycle 105 adds the causal owner that a future process implementation must obey.
Ready owns one non-cloneable prepared launch; start consumes it for exactly one
injected spawn. EOF and idle update running observations only. A natural exit
must pass through the owner's join effect before zero exit becomes stopped or a
non-zero/signal becomes crash. Cancel and shutdown signal then join once and
become idempotent after terminal state. Join residue keeps the child token owned
so drop retries cleanup and records exact signal/join truth. The current effects
are synchronous fixtures and the drop report is not yet durably retained.
Cycle 106 adds the neutral data contract behind the requested status, context,
and usage decks. A closed tagged value ensures missing data carries provenance
but no value. The snapshot safely projects the existing provider route receipt,
optional session/run IDs, explicit effective-authority counts, context window
source and arithmetic, provider usage coverage and token categories, and
portable cost. Review removed the generic cost escape hatch: only an explicit
provider-reported currency observation can populate cost, and Kitsu never
derives it from a model name or token count. The module has no renderer, CLI,
JSONL, persistence, provider call, filesystem, or process dependency.
Cycle 107 adds the monotonic publication half of the external inventory
boundary. One non-cloneable owner borrows or consumes only complete validated
snapshots. Initial publication is create-only; ordinary updates require the
exact same source lifetime and immediate revision; explicit replacement and
withdrawal require the exact current fence. Refused transitions preserve the
same allocated snapshot, and maximum revision cannot wrap. This remains
provider-free and unmounted: it owns no handlers, child, transport, MCP codec,
configuration, or product authority.
Cycle 108 binds the complete bounded runtime policy into the same inert request,
host profile, and prepared-launch proof. Startup, request, drain, kill, and reap
budgets plus stdin, stdout, stderr, and frame limits are non-zero and capped;
the frame must fit both protocol streams. The ordered environment projection is
clear-only, unique, aggregate-bounded, NUL-free, and debug-redacted. Owned
process group, denied network, and terminate-group-then-reap timeout behavior
are mandatory. No real process consumes these values yet, so this is exact
admission truth rather than a containment claim.
Cycle 109 supplies the missing current-run accounting prerequisite for the
telemetry decks. A request-local tracker correlates physical attempt ordinals,
counts completed HTTP rejections once as missing usage, preserves a usage report
through a later partial failure, and never treats a transport-only fault as a
completed response. Totals and fixed inference/compaction/future-catalog
coverage buckets preserve cached-token `Some(0)` versus absence. A pure adapter
feeds the existing session usage DTO; it is not mounted into product collection,
slash routing, rendering, JSONL, persistence, account data, pricing, or cost.
Cycle 110 adds the presentation-only half without weakening that boundary. A
private product helper renders only the already-typed snapshot into bounded
wide, stacked, narrow, and tiny styled layouts or one stable escape-free format
for redirected, plain, no-color, and screen-reader destinations. Every fact
keeps its truth/provenance label, unavailable remains explicit, and cost is
omitted unless the snapshot already carries a provider-reported currency value.
The helper remains unmounted and performs no collection, provider/account/MCP
call, filesystem access, persistence, JSONL emission, or slash routing.
Cycle 111 adds the provider-free phase owner behind the authorized preparation
proof. Startup and one request each receive an exact deadline; timeout,
cancellation, and shutdown enter the same bounded drain, process-group
termination, kill, and reap sequence. Termination happens before fresh kill and
reap clocks begin. Clock regression/overflow, injected effect faults, drop, and
reap expiry preserve non-cloneable child ownership in typed residue; a spawn
failure owns no child. Natural exit/EOF/crash truth still belongs to the
existing lifecycle and future backend composition. No OS process, containment
enforcement, configuration, MCP transport, inventory publication, or product
route is added.

No provider, session, MCP, semantic-context, onboarding, or multimodal feature
jumps this foundation because all depend on truthful causal outcome and joined
cleanup.
