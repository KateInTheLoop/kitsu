# Parallel delivery after Cycle 100

Status: approved launch topology from the accepted Cycle 100 checkpoint.

## Objective

Parallelize evidence, fixtures, and narrow mechanisms without weakening Kitsu's
authority boundaries or turning the roadmap into several competing histories.
`improvements/harness-roadmap` remains the accepted integration line. Candidate
branches may build and review in parallel, but only a green squash/integration
commit receives the next sequential cycle receipt.

## Branch and receipt rules

1. Every candidate branch starts from the exact accepted Cycle 100 commit in a
   separate linked worktree. The original dirty `main` worktree is never used.
2. A packet owns a bounded file set, requirements, red test, implementation,
   review notes, acceptance commands, rollback, and one draft commit. Draft
   commits are not accepted roadmap cycles.
3. The integration owner reviews the diff, reruns the packet and invariant
   gates, squashes it with the next `cycles/NNN.md` receipt, and updates the sole
   cycle ledger. Candidate branches never edit `program-status.md`, the cycle
   ledger, or another lane's receipt.
4. Shared surfaces—`main.rs`, `session.rs`, root configuration, JSONL schema,
   Cargo workspace membership, and roadmap ledgers—remain integration-owned.
   Candidate work exposes pure modules or narrow APIs until its integration
   card.
5. A failing packet is reshaped or discarded. It never blocks another lane and
   never earns a cycle number merely because implementation time was spent.

## Immediate E06 split

The first launch focuses on the highest-priority missing spine and uses three
independent packets before product composition.

| Candidate branch | Packets | Owned outcome | Gate before integration |
|---|---|---|---|
| `improvements/e06-inventory` | P01-P03 | Frozen per-request builtin inventory; source/incarnation/revision/fingerprint identity; pre-publication authority request | Static parity, collision, mutation, withdrawal, subagent intersection, and zero-provider fixtures |
| `improvements/e06-supervisor` | P04-P06 | Private long-lived child lifecycle owner over Kitsu's existing containment; admitted absolute executable identity; bounded pipes/cancel/reap | `X002-ADMIT-0`, block/flood/descendant/crash/EOF/timeout/drop fixtures, and unchanged one-shot command containment |
| `improvements/e06-mcp-codec` | P07-P09 | Provider-free bounded JSON-RPC framing and tools-only initialize/list/call state machine | Fragmentation, oversize, malformed/duplicate/late response, deadline, cancellation, and zero ambient capability fixtures |
| `improvements/e06-integration` | P10+ | Disabled-by-default pinned echo child, atomic inventory publication/withdrawal, required/optional receipts | `PF-06`, `PF-07`, crash/no-replay, descendant reap, offline doctor no-spawn, and full workspace gates |

Authority admission precedes executable discovery and spawn. An executable is
identified by an admitted absolute path plus content digest; display names are
never authority. EOF, crash, cancellation, reload, and shutdown withdraw the
whole source generation atomically and join or report bounded residue. MCP is
limited to local stdio `initialize`, `tools/list`, and `tools/call`; resources,
prompts, sampling, HTTP, package installation, and executable discovery remain
out of scope.

## Evidence lanes that can overlap

These lanes may prepare pure DTOs, renderers, fixtures, and benchmarks while E06
is running. They may not consume an identity or persistence contract that its
owning spine has not published.

| Candidate branch | Safe early work | Deferred integration dependency |
|---|---|---|
| `improvements/e08-telemetry-decks` | Truth-class DTOs, additive JSON vectors, width/accessibility render fixtures, `/usage session` vocabulary | Durable history waits for E03; MCP health waits for E06 |
| `improvements/e07-semantic-evidence` | Fixed-corpus benchmark, denied-path corpus, deterministic scoring and token budgets, read-only structural experiment | Product contributor only after measured win and authority/staleness review |
| `improvements/e05-onboarding` | Pure no-clobber transaction model, offline catalog cache fixtures, picker ranking and narrow-terminal views | Credential/config publication and live switching remain integration-owned |
| `improvements/e09-multimodal-evidence` | Threat contract and hostile PNG/JPEG corpus only | Product bytes wait for two truthful image-capable provider paths and demand evidence |

The initial safe overlap contains 23 bounded cards: four telemetry foundations,
eight semantic benchmark/selector cards, eight onboarding transaction/picker
cards, and three multimodal evidence cards. This is capacity, not a promise to
promote all four features.

## Merge order and conflict control

E06 integrates inventory identity first, supervisor admission/lifecycle second,
codec third, then the disabled echo composition. Telemetry DTOs, semantic
benchmarks, and onboarding transaction values can integrate between those cards
when their file ownership is disjoint. No more than one integration commit is
prepared at a time; every accepted commit rebases the remaining candidates and
runs their focused gates again.

The integration review rejects name-only identities, ambient runtime loading,
implicit install/update/discovery, remembered cross-session authority,
fail-open hooks, timeout-by-abandonment, EOF-as-success, and any projection or
file tail treated as canonical HEAD.

## Checkpoint and acceleration measure

Cycle 100 is the stable review base. Parallel work accelerates discovery and
implementation latency, while serialized acceptance preserves bisectability and
one truthful roadmap. Progress is measured by accepted gates and integrated
packets—not by multiplying the estimated gate matrix into hundreds of supposed
features.
