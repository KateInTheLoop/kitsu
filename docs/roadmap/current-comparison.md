# Current Harness Comparison

Status: Kitsu-owned comparison from post-E01 runtime pin `986fba9`, updated
through accepted Cycle 050; frozen research inputs and hashes are listed in
[`post-e01-evidence-baseline.md`](post-e01-evidence-baseline.md).

The detailed source-by-source transfer decisions remain in the
[mechanism ledger](mechanism-ledger.md). This view answers only what Kitsu now
leads on, trails on, and has actually closed.

## Current verdict

Kitsu continues to lead on explicit authority, OS-backed command containment,
offline/auditable configuration, fail-stop versioned JSONL, atomic session
snapshots without restored grants, failed-turn truth, and exact atomic edits.

It continues to trail on native provider breadth/capabilities, guided model
onboarding, canonical session lineage/HEAD, external tools/MCP, semantic
context selection, multimodal input, and proven multi-surface operation.

The main correction to the frozen comparison is that Kitsu now has useful
bounded structural navigation. The remaining context gap is semantic
reference-graph selection and token-budget ranking, not the absence of all
code navigation.

## Gap disposition now

| Gap | Current evidence | Disposition/owner |
|---|---|---|
| Ollama untagged versus `:latest` doctor mismatch | Closed by Cycles 003-004 with local-only, human/JSON, and zero-chat coverage | **Closed**; E01-S01 |
| Duplicate tool schema/handler registration | Closed for A001's per-entry contract by Cycles 005, 007, 008 | **Closed**; E01-S02/S03 |
| Provider retry/context discovery | Wider bounded retry and nested context-window discovery landed in the 67-commit delta; Cycles 032-046 bind chat, retry, replay, catalog, compaction, subagent, top-level, and doctor sends to immutable generations for either explicitly selected compiled adapter. Cycles 047-049 define and expose the complete bounded route receipt, then run identical success/fault/cancellation/status/doctor/offline product fixtures against both adapters | **Closed and conformance-proven**; E04/X001 accepted |
| Structural code navigation | `repo_grep`, `find_symbol`, `outline_file`, and `read_manifest` exist under read authority | **Narrow gap closed**; semantic selector remains E07 |
| Timeout/recovery truth | Completed tool rounds survive piped deadline failure; JSONL framing improved | **Partial**; causal run phases remain E02 and session recovery/HEAD remains E03 |
| Provider dialect breadth | A distinct native Messages text/tool/usage/terminal implementation passes exact wire, fault/lifecycle, telemetry, frozen-auth route, truthful-capability, generation-attribution, bounded retry/deadline, backoff-cancellation, generic route-handle, and product-surface harnesses with compatible Chat. An explicit closed adapter selector drives live chat and inference doctor; legacy flavor-only profiles retain compatible Chat, and native catalog requests fail before transport. The complete secret-free route receipt round-trips and projects through status/JSONL/doctor | **Closed for the two compiled dialects**; E04/X001 accepted |
| Guided onboarding/model choice | Config and model listing exist, but setup is still manual and not transactional | **Open**; E05 after E04 |
| Session lineage/branch-at-turn | Whole-history fresh-ID fork exists. Cycle 050 closes the ownership ambiguity with one canonical revision/selected-HEAD ADR, five separate version planes, typed CAS/corruption/publication states, explicit recovery, fresh authority, and stable K01-K12 meanings; production v1 has not implemented them | **Design accepted, implementation open**; E03-S01 continues |
| MCP/external tools | No client or supervisor is compiled | **Open experiment**; E06 after E02 |
| Semantic context | No tree-sitter/reference graph/PageRank token-fit selector | **Open experiment**; E07 fixed corpus |
| Multimodal input | Image metadata reader exists; model requests remain text/tool oriented | **Deferred**; E09 after truthful capabilities |
| Multi-surface operation | No named second host/principal | **Open/no invented demand**; X003 may remain prepared only |

## Five-boundary scoreboard

Result: **1/5 completely replaceable**. X001 executed against compatible Chat
and native Messages; X002-X005 retain their exact missing cells.

| Boundary | Score | Why |
|---|---:|---|
| X001 provider | **Pass** | Immutable current-send generations plus two explicitly selectable compiled wires pass exact wire/fault/retry/terminal-attempt/frozen-route parity and one shared product suite covering receipt-correlated success, HTTP failure, cancellation, status, JSONL, inference doctor, and offline no-construction |
| X002 tool/source | Fail | Registry integrity improved, but no admitted/supervised external source or two-source suite |
| X003 host/UI | Fail | No named independent host/principal or shared carrier identity fixture |
| X004 automation/runtime | Fail | Neutral run/outcome phases and an independent seven-carrier default-runtime evaluator now exist; no named alternate runtime adapter runs the suite |
| X005 session/store | Fail | Strong atomic file snapshots, but no canonical revision/logical HEAD, K01-K12, or second store |

## Comparative borrowing rule

Borrow narrow mechanisms: Pi/OpenCode/Crush for provider normalization and
selection, Codex/OpenHands/Qwen for fencing/revisions/HEAD, Cline for bounded
child phases and canonical-history projections, Aider/Goose for read-only
semantic context, and DeepSeek for effect-owned rollback/conformance design.

Continue rejecting executable configuration, implicit install/update, ambient
in-process extensions, name-only identity, broad remembered approvals,
fail-open/unjoined hooks, process-separation-as-containment, EOF/idle-as-success,
and DB-row/file-tail/projection-as-canonical-HEAD.

## Evidence boundary

This comparison is source/test-backed plus the provider-free acceptance tests
recorded in Cycles 003-050. The full X001 suite executed against both required
compiled implementations; no live provider, credential, billable request, or
subject-project runtime was needed. It does not claim execution of X002-X005.
Scores change only when the named complete gate passes against both required
implementations.
