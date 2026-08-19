# Improvement Program Status

Status: E00 and E01 accepted; E02 in progress through Cycle 024 on
`improvements/harness-roadmap`.

| Field | Current value |
|---|---|
| Clean implementation base | `a2ba2d5c98b17da7290a2b6b8b24c83bc2529763` |
| Frozen research subject | `6c7133334dadf66a545888f4068ac82585a2768f` |
| Post-E01 runtime source pin | `986fba971ec99c9af5dcd8f69a48f507aae486af` |
| Accepted cycles/commits after this cycle | 24 / 24 |
| Complete replaceability gates | 0 / 5 |
| Closed bounded defects/transfers | KB-001 Ollama alias; A001 registry admission/coupling |
| Original main worktree | Same ten user-owned modified paths; untouched |
| Active next epic | E02 — one run identity and causal terminal outcome |

## Epic status

| Epic | State | Evidence/next gate |
|---|---|---|
| E00 baseline/safety | **Accepted** | Clean base, invariant map, mechanism ledger, content-pinned delta, current views, cycle check |
| E01 bounded defects | **Accepted** | Cycles 003-005 and 007-008; 219 tool, 308 CLI, 146 core tests at last affected-code acceptance |
| E02 causal run/outcome | **In progress** | One process owner now covers migrated JSONL carriers and child-free inference fallback; human-phase, post-start fallback, interactive aggregate, and exit-evaluation gates remain |
| E04 provider spine | Planned after E02 | Primary wire fixtures, capability types, `X001-G1-AUX`, distinct compiled dialect |
| E03 session spine | Planned after E04 | Canonical revision/HEAD ADR, CAS/fencing, K01-K12, recovery, branch-at-turn |
| E06 child/MCP spine | Planned after E03; depends on E01/E02 | Bounded supervisor, pre-spawn admission, disabled tools-only echo experiment |
| E07 semantic context | Planned after three spines | Fixed-corpus relevance/token budget plus authority/stale-edit gates |
| E05/E08 onboarding/docs | Planned after provider/tool surfaces stabilize | Transactional setup/picker and PF-10 observability/docs |
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
| 024 | this document's commit | Process-singleton lifecycle and truthful child-free inference fallback |

## Current green gates

- Affected runtime suites: 221 `kitsu-tools`, 317 Kitsu binary, 202 CLI, six
  carrier-matrix, 73 renderer, and 155 `kitsu-core` tests (147 unit plus eight
  neutral run-contract tests), zero failures.
- Workspace/all-target check passes with ordinary flags and with
  `RUSTFLAGS=-Dunused-must-use`.
- Workspace/all-target Clippy passes with warnings denied; formatting and diff
  checks pass.
- Original dirty main status remains the same ten paths recorded in Cycle 001.
- Frozen research worktree is clean at its exact pin; eight non-versioned
  research decision inputs are SHA-256 pinned.
- `scripts/check-roadmap.sh` verifies sequential receipts, required fields, and
  one cycle file per improvement commit on this active branch.

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
fallback cut. Cycles 025-027 carry neutral facts through human noninteractive
paths, define interactive aggregation, and run the independent exit evaluation
before provider work begins.

No provider, session, MCP, semantic-context, onboarding, or multimodal feature
jumps this foundation because all depend on truthful causal outcome and joined
cleanup.
