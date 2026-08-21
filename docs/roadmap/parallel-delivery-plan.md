# Parallel delivery after Cycle 118

Status: the first post-118 wave plus Cycles 126-128 are accepted on
`improvements/post-118`; active lanes are Cycle 129's publication-fixture
isolation, Cycle 130's real-child MCP peer proof, and Cycle 131's model-picker
namespace binding.

## Objective

Parallelize evidence, fixtures, and narrow mechanisms without weakening Kitsu's
authority boundaries or turning the roadmap into several competing histories.
`improvements/post-118` is the next integration line. The pushed
`improvements/e06-integration` branch remains the immutable review checkpoint.
Candidate branches may build and review in parallel, but only a green
integration commit receives the next sequential cycle receipt.

## Branch and receipt rules

1. Every candidate branch starts from the exact accepted Cycle 118 commit in a
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

## Active post-118 wave

Seven dependency-ordered packets run in this wave. They advance a proven owner or
presentation seam by one bounded step and deliberately stop before product-wide
composition.

| Candidate branch | Packets | Owned outcome | Gate before integration |
|---|---|---|---|
| `improvements/e06-serial-transport` | E06-S01-C7 | Retain one sealed/authorized child owner across multiple strictly serial bounded frames | One in-flight request, line/idle deadlines, EOF/crash refusal, cancellation/reap, residue, and unchanged Cycle118 containment |
| `improvements/e05-local-coordinator` | E05-S01-C3 | Compose the pure transaction with real create-new publication for the credential-free local path | Exact fences/receipts, no parent/overwrite, rollback, cancellation/collision cuts, and zero provider/credential/UI effects |
| `improvements/e08-context-window` | E08-S05-C6 | Mount exact offline `/context window` from one typed current observation | Bare/refresh compatibility, width/accessibility matrix, redirected output, exact syntax, zero sends, and honest JSONL nonclaim |
| `improvements/e07-selector-experiment` | E07-S01-C2 | Compare one pure deterministic structural selector with the frozen current-tool baseline | Same corpus/rubric/budgets, byte-repeatability, denied/stale nonclaims, and an explicit promote-or-reject result |
| `improvements/e05-model-picker` | E05-S02-C1 | Stage one pure offline-capable model picker over an immutable provenance-bearing catalog snapshot | Bounded normalization/dedupe/filter/ranking, unavailable/stale/offline truth, narrow projection, no effects, and no inferred capabilities |
| `improvements/e08-usage-aliases` | E08-S05-C7 | Add truthful compatibility aliases over the typed current-session usage observation | Bare/session parity, legacy `/cost` compatibility decision, zero external effects, exact syntax, and honest JSONL behavior |
| `improvements/e07-authority-evidence` | E07-S01-C3 | Measure selector behavior under denied, symlinked, revoked, and source-changed evidence | Existing permission/tool APIs, deterministic artifact, no stale/cache leak, and an explicit pass/reject result |

E06 authority admission still precedes every observation and spawn. Cycle 118
now executes the exact capped sealed native bytes and binds explicit Landlock
content roots; display names and path re-opens are never authority. The serial
packet must preserve that proof and may not yet compose MCP. MCP remains limited
to future local stdio `initialize`, `tools/list`, and `tools/call`; resources,
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

The remaining safe overlap still contains semantic-selector comparisons,
onboarding picker/UI cards, telemetry aliases/automation/history, and
multimodal evidence. This is capacity, not a promise to promote a mechanism
without its measured gate.

## Merge order and conflict control

The current merge order is E06 serial ownership, E05 local coordinator, E08
context-window presentation, the E07 measured selector result, the pure E05
model-picker result, then the dependent E08 alias and E07 authority/staleness
cards, unless review exposes a dependency or shared-file conflict.
MCP codec/backend composition follows only after serial ownership is accepted.
Guided UI and model-picker effect integration follow only after the local
coordinator and pure picker are accepted. No more than one integration commit is
prepared at a time; every accepted commit rebases or cherry-picks the remaining
candidates and reruns their focused gates plus the final workspace gate.

That seven-packet order is now accepted through Cycle 125. The next parallel
wave keeps the same serialized integration rule: E06 composes the pinned
tools-only codec with the accepted serial owner privately, while E08 may add an
offline coarse context-source deck whose unsupported buckets remain explicitly
unavailable. Neither packet may mount an external tool, invent an automation
payload, or consume an unowned provider, account, filesystem, or persistence
truth.

Cycle 128 promotes only the measured selector mechanism into a compiled,
unmounted read-only contributor. Current authorized reads reconstruct every
emitted line and current definition/reference classification after ranking;
the frozen 5-to-3-round and 1,456-to-1,134-byte result remains unchanged. A
later product mount still requires explicit trigger/root ownership,
cancellation/deadlines, context accounting, and canonical projection rules.

The integration review rejects name-only identities, ambient runtime loading,
implicit install/update/discovery, remembered cross-session authority,
fail-open hooks, timeout-by-abandonment, EOF-as-success, and any projection or
file tail treated as canonical HEAD.

## Checkpoint and acceleration measure

Cycle 118 is the stable pushed review base and `improvements/post-118` is the
clean next integration branch. Parallel work accelerates discovery and
implementation latency, while serialized acceptance preserves bisectability and
one truthful roadmap. Progress is measured by accepted gates and integrated
packets—not by multiplying the estimated gate matrix into hundreds of supposed
features.
