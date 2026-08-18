# E02 Carrier-Neutral Run Contract

Status: Cycle 016 authority-interruption integration; workspace-internal Rust
contract, not a persisted format, JSONL schema, or compatibility promise.

Implementation: `crates/kitsu-core/src/run.rs`. Boundary gate:
`scripts/check-run-boundary.sh`. Behavioral contract:
`crates/kitsu-core/tests/run_contract.rs`.

## Why this exists

The Cycle 011-014 fixtures proved two facts that presentation-owned state cannot
represent truthfully:

1. JSONL has a stable process-run identity while human carriers do not expose
   the same semantic identity.
2. EOF at an authority prompt is currently reduced to ordinary denial, allowing
   a provider follow-up even though the target contract requires interruption
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
Human mode still does not display the run identity or project the neutral
terminal outcome; that remains the next card rather than a capability claimed
by this integration.

## Coupling gate

CI rejects the following imports in `run.rs`: CLI/session modules; provider,
renderer, and tool crates; serde/JSONL; Tokio; environment, filesystem, I/O,
process, or time APIs. The checker accepts an alternate source path so its
negative fixture can prove a renderer reference fails.

This narrow source gate protects the new contract; it does not claim existing
`kitsu-core::orchestrator` is already renderer- or tool-independent. The broader
replaceability gate remains E10.

## Next slices

1. Project the same `RunId` and outcome into human run receipts without making presentation
   canonical.
2. Add monotonic settlement, cleanup, persistence, delivery, and terminal
   publication around the neutral outcome; then project compatibly into JSONL.
