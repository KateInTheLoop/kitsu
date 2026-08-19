# E02 Carrier-Neutral Run Contract

Status: E02-S02 accepted, E02-S03 live integration covers ask, scripted, and
piped JSONL, and E02-S04 compatible projection started through Cycle 022;
workspace-internal Rust contract with human/JSONL projections, not a persisted
format or compatibility promise.

Implementation: `crates/kitsu-core/src/run.rs`. Boundary gate:
`scripts/check-run-boundary.sh`. Behavioral contract:
`crates/kitsu-core/tests/run_contract.rs`.

## Why this exists

The Cycle 011-014 fixtures proved two facts that presentation-owned state cannot
represent truthfully:

1. JSONL has a stable process-run identity while human carriers do not expose
   the same semantic identity.
2. EOF at an authority prompt was reduced to ordinary denial, allowing a
   provider follow-up even though the target contract requires interruption
   with zero later send/effect.

This module introduces only the neutral values needed to carry those facts. It
does not move provider calls, tools, signals, terminal output, persistence, or
process-exit policy into core.

## Types and invariants

| Type | Current responsibility | Deliberately absent |
|---|---|---|
| `RunId` | Non-empty, at most 128 bytes, machine-safe `[A-Za-z0-9._:-]` identity | Randomness, clock, PID, host/session identity, serialization |
| `RunSpec` | Requires a `RunId` before it can create any event or terminal | CLI arguments, input, signal, provider, tool, store, renderer |
| `RunEvent` | Always carries its originating spec's identity | Phase ordering, publication, JSONL shape |
| `RunEventKind` | `Accepted` or typed `Interrupted(AuthorityInputClosed)` | EOF inference, denial text, exit code |
| `TerminalOutcome` | Keeps semantic class and assistant disposition independent | Cleanup, persistence, delivery, process exit |
| `RunLifecycle` | Enforces `accepted -> settled -> joined -> persisted -> delivered -> published` without mutating rejected transitions | Providers, tools, carriers, stores, clocks, process exit |
| `PublishedRun` | Immutable semantic outcome plus typed cleanup, persistence, and delivery states | Rendering, serialization, exit-code inference |

`RunOutcomeClass` contains exactly the fixture-proven semantic classes:
success, accepted partial, user/provider/tool fault, cancellation, and
interruption. `RunAssistantDisposition` independently records committed,
committed-partial, discarded, or absent output. A provider fault with committed
partial output therefore remains representable without being rewritten as
success.

## Current integration

The CLI allocates one `RunSpec` per process before argument parsing can accept
work, using its existing PID/time source, then validates that generated value
through `RunId`. JSONL framing and private runtime/offload generation names read
the same neutral identity. Identity generation remains a host responsibility;
`kitsu-core` does not read time, process state, environment, or randomness.

Filesystem and command approval now distinguish an explicit denial from a
closed authority-input channel. Explicit `n` remains `PolicyDenied` and may be
returned to the provider for recovery. EOF/input failure becomes typed
`Interrupted`; core closes the active tool event, returns
`AuthorityInputClosed`, and performs no later provider send or tool effect.

Human model-command completion now constructs one `TerminalOutcome` from the
same process `RunSpec`, typed CLI failure class, and last assistant disposition.
The renderer receives only a presentation DTO: redirected `kitsu-timing` JSON
adds `run_id`, `outcome`, and `assistant`, while a TTY gets the same exact tuple
in its compact terminal line. Sequential human processes prove distinct IDs;
human and JSONL fixtures agree on success, provider fault, and tool/policy fault
classification. Presentation does not generate, infer, or mutate these values.

Cycle 018 adds the provider-free phase owner without wiring it to carriers yet.
The first settlement is immutable; later cleanup/persistence/delivery failures
remain independent fields. Cleanup residue requires a nonzero unjoined count,
publication is single, and duplicate/out-of-order/post-terminal transitions
fail before mutation. Carrier integration and fault cuts remain later E02-S03
cards, so current presentation still owns the live sequence.

Cycle 019 projects `TerminalOutcome` into two optional `kitsu.chat.v1`
`final_summary` fields: `outcome` and `assistant`. New writers always emit the
canonical labels; old records without them remain valid. The carrier fixture
derives the historical result from `status`, failure counters, and the last
`assistant_disposition`, then rejects an explicit field that contradicts that
evidence. This is additive schema evolution, not a version bump, and the JSONL
writer's existing terminal-on-write-failure behavior is unchanged.

Cycle 020 makes the common lifecycle authoritative for the deliberately narrow
`ask --jsonl` terminal path. `ask` constructs an empty tool registry and has no
child registry or session store, so cleanup is exactly `Joined` and persistence
is exactly `NotRequested`. The final-summary write is the delivery cut: success
records `Delivered`; sink failure records `Failed`; both then yield one
immutable `PublishedRun`, and the original semantic outcome/assistant state is
unchanged. Scripted and piped finalization remain outside this tranche because
their background and snapshot outcomes must be carried explicitly rather than
inferred from presentation state.

Cycle 021 carries scripted facts into the same publisher. Background work is
bounded and observed before semantic settlement because its result is part of
the script outcome; cancellation statuses then produce `Joined` or a nonzero
`Residue` from their actual `joined` flag. The active-session receipt tracks
persistence independently from addressability: disabled is `NotRequested`, an
unchanged pending/resumed snapshot is `Preserved`, successful installation is
`Committed`, and pre- or post-install save/observation failure is `Failed`.
Post-install failure retains the resumable session ID. The final error/summary
write is one delivery cut and the lifecycle publishes once even when an earlier
JSONL write made the sink terminal. Piped and human presentation paths remain
outside this tranche.

Cycle 022 completes the three noninteractive inference carriers' JSONL terminal
publication. Attachment preflight, background failure, strict failure, session
save, required-write, output-install, success, provider failure, and cancellation
branches all enter one piped publisher with explicit cleanup and active-snapshot
persistence. Rollback cancellation now offers a status-preserving API alongside
the compatible ID-only projection; malformed/unattached `joined: false` truth
therefore becomes nonzero residue. Earlier event-sink failure remains fail-stop
and prevents later work before terminal settlement, while a failure inside the
terminal delivery closure is phase data and still yields one immutable internal
publication.

## Coupling gate

CI rejects the following imports in `run.rs`: CLI/session modules; provider,
renderer, and tool crates; serde/JSONL; Tokio; environment, filesystem, I/O,
process, or time APIs. The checker accepts an alternate source path so its
negative fixture can prove a renderer reference fails.

This narrow source gate protects the new contract; it does not claim existing
`kitsu-core::orchestrator` is already renderer- or tool-independent. The broader
replaceability gate remains E10.

## Next slices

1. Audit interactive and top-level management carriers against the E02 exit
   matrix; close E02 only where actual cleanup/persistence/delivery evidence is
   complete, and decompose any remaining gap before E04 begins.
2. Extend projection parity only from measured carrier gaps; do not invent a
   second schema version while additive `kitsu.chat.v1` evolution is sufficient.
