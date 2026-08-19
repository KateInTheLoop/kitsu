# Improvement Program Status

Status: E00, E01, E02, E04-S01, and E04-S02 are accepted through Cycle 033;
E08 telemetry observability was scoped in Cycle 030 on
`improvements/harness-roadmap`.

| Field | Current value |
|---|---|
| Clean implementation base | `a2ba2d5c98b17da7290a2b6b8b24c83bc2529763` |
| Frozen research subject | `6c7133334dadf66a545888f4068ac82585a2768f` |
| Post-E01 runtime source pin | `986fba971ec99c9af5dcd8f69a48f507aae486af` |
| Accepted cycles/commits after this cycle | 33 / 33 |
| Complete replaceability gates | 0 / 5 |
| Closed bounded defects/transfers | KB-001 Ollama alias; A001 registry admission/coupling |
| Original main worktree | Same ten user-owned modified paths; untouched |
| Active epic/card | E04 / E04-S03-C1 (Cycle 034) — immutable route snapshot and initial generation |

## Epic status

| Epic | State | Evidence/next gate |
|---|---|---|
| E00 baseline/safety | **Accepted** | Clean base, invariant map, mechanism ledger, content-pinned delta, current views, cycle check |
| E01 bounded defects | **Accepted** | Cycles 003-005 and 007-008; 219 tool, 308 CLI, 146 core tests at last affected-code acceptance |
| E02 causal run/outcome | **Accepted** | One process owner, exact phase facts, interactive aggregate, and one human/JSONL exit evaluator pass the independent seven-carrier matrix; X004 still lacks a named alternate runtime adapter |
| E04 provider spine | **In progress**; E04-S01/S02 accepted through Cycle 033 | Sixteen wire, six route-contract, three derivation/declaration, and two composition cases cover protocol evidence, secret-free identity, truthful current capabilities, and flavor-independent adapter declaration; live publication, `X001-G1-AUX`, and compiled adapter remain |
| E03 session spine | Planned after E04 | Canonical revision/HEAD ADR, CAS/fencing, K01-K12, recovery, branch-at-turn |
| E06 child/MCP spine | Planned after E03; depends on E01/E02 | Bounded supervisor, pre-spawn admission, disabled tools-only echo experiment |
| E07 semantic context | Planned after three spines | Fixed-corpus relevance/token budget plus authority/stale-edit gates |
| E05 onboarding | Planned after provider surfaces stabilize | Transactional setup/picker and PF-10 first-success gates |
| E08 telemetry/docs | **Scoped** through Cycle 030; implementation waits on owned inputs | E08-S05 live telemetry consumes E04 receipts; E08-S06 durable usage consumes E03 fencing; progressive docs follow stable surfaces |
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
| 033 | this document's commit | Bounded opaque profile derivation and truthful current compatible-Chat declaration |

## Current green gates

- The provider-wire matrix now has sixteen offline tests covering exact text
  and correlated tool rounds, structural Chat/Messages divergence,
  wrong-adapter rejection, HTTP/mid-stream/malformed faults, path-level hard
  bounds, timeout/cancellation terminality, and the capability handoff. Full
  affected-suite counts are recorded in the cycle receipt.
- Six provider-route tests pin a stable secret-free v1 JSON vector, opaque
  profile identity, nonzero generation/revisions, truthful adapter/wire pairs,
  all six deterministic capabilities, fail-closed decoding, and zero-send
  stale/unsupported admission. Core still does not import provider route values.
- Three derivation/declaration and two CLI composition tests pin the
  domain-separated SHA-256 vector, input bounds, canonical path identity,
  route-content independence, exact compatible-Chat capabilities, and the fact
  that `anthropic` remains a display/config label until a native adapter ships.
  The CLI retains the declaration privately but does not yet publish a live
  generation receipt.
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
must not falsely claim the reserved native Messages adapter.
Cycle 034 begins E04-S03 with one immutable snapshot owning the declaration,
endpoint/model/auth-source, retry/catalog policy, and initial generation before
any send path is migrated.

No provider, session, MCP, semantic-context, onboarding, or multimodal feature
jumps this foundation because all depend on truthful causal outcome and joined
cleanup.
