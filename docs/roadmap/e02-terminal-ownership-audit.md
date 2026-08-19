# E02 Terminal Ownership Audit

Status: Cycle 023 evidence; C6 process ownership accepted in Cycle 024; E02
remains open for C7/C8 and exit evaluation.

## Scope decision

E02 owns inference-capable process runs: `ask`, top-level `review`, and chat in
piped, scripted, and interactive forms. These paths set
`MODEL_COMMAND_ACTIVE`, can produce a model-authored assistant disposition, and
must preserve one causal outcome across carrier presentation.

Host-management commands (`init`, `doctor`, `auth`, `models`, `sessions`, and
`artifacts`) do not create an inference turn or assistant disposition and remain
outside E02. `models` may perform provider catalog I/O, but discovery is not an
inference run; its transactional selection and observability gates belong to
E04/E05. Help and parse-only output are also outside E02. This is a semantic
boundary, not an exemption from their own error, cancellation, and output
tests.

“Top-level” below therefore means `main`'s inference-run fallback, not the
management commands.

## Current ownership ledger

| Path | Semantic result | Cleanup/persistence evidence | Delivery cut | Lifecycle owner | Exit finding |
|---|---|---|---|---|---|
| `ask --jsonl` after setup | Typed status and assistant disposition | Structurally joined; persistence not requested | Final JSONL summary | Fresh lifecycle inside the ask summary helper | Locally complete, but not process-singleton |
| scripted JSONL | Typed aggregate status | Actual child statuses and active-snapshot receipt | Terminal error/summary closure | Fresh lifecycle in scripted finalization | Locally complete, but not process-singleton |
| piped JSONL | Typed branch status | Actual rollback/exit child statuses and active-snapshot receipt | Terminal error/turn/summary closure | Fresh lifecycle in each piped terminal helper call | Locally complete, but not process-singleton |
| human ask/review/piped/scripted | Typed process outcome and assistant DTO in `main` | Facts exist inside the carrier but are not carried to `main` | Human timing receipt | No lifecycle publication | Phase truth bypassed |
| interactive chat | Process success/error and last assistant DTO in `main` | Background work is joined at loop exit; session/history outcomes are not a typed terminal fact | Human timing receipt | No lifecycle publication | Aggregate policy and phase truth are missing |
| inference preflight/top-level fallback | `CliError` class and assistant state | No accepted child/session work for early failures, but the fallback does not express this | Direct JSONL error/summary or human error/timing | No lifecycle publication | Phase truth bypassed |

## Findings

1. `finalize_terminal_with_delivery` constructs a new accepted
   `RunLifecycle` on every call. The core type rejects a second publication on
   one instance, but current carrier helpers do not share one instance. Two
   helpers can therefore each publish internally for the same process run. No
   current branch is proven to do so; control-flow convention is not the
   required singleton enforcement.
2. `main` owns the fallback JSONL terminal for errors before a carrier emits a
   summary. Parse-only errors are out of scope, but inference configuration,
   authentication, and setup failures use the same fallback and emit no
   lifecycle phases.
3. Human noninteractive carriers have the same semantic DTO as JSONL but do not
   pass their cleanup, persistence, or delivery facts through the phase owner.
   Renderer choice currently changes whether lifecycle publication occurs.
4. Interactive chat is a multi-operation process. A failed or cancelled turn
   is recoverable inside the REPL and the process may later exit zero. E02 must
   define the aggregate result independently from “last assistant” before it
   can publish a truthful process terminal.
5. The current core lifecycle is still valuable and its transition tests are
   sound. The failing boundary is host ownership and fact propagation, not the
   monotonic state machine.

## Bounded decomposition

| Card | Requirement | Acceptance gate |
|---|---|---|
| E02-S03-C6 — process terminal owner (accepted Cycle 024) | Accept one lifecycle before inference work; every migrated finalizer uses the same owner; a second/cross-path publication is rejected; early inference fallback publishes with structurally truthful facts. | Provider-free duplicate/cross-path and preflight cuts prove exactly one publication and zero work after it. |
| E02-S03-C7 — presentation-neutral facts | Carry semantic, cleanup, persistence, and delivery inputs independently of JSONL; migrate human ask/review/piped/scripted without changing their text or JSONL v1. | Human/JSONL paired fixtures agree on terminal fields; blocked delivery never reclassifies settlement. |
| E02-S03-C8 — interactive aggregate | Define process aggregation for successful, failed, cancelled, interrupted, retried, and queued turns; join background work and type session persistence before publication. | PTY fixtures cover clean exit, recoverable fault then exit, active/queued cancel, authority EOF, residue, and save failure. |
| E02-S04-C2 — exit evaluation | Run the independent seven-carrier matrix against the process owner and close E02 only if every applicable phase/exit cell is observed. | X004 evaluation receipt, exact exits, one terminal, no silent cells; otherwise one evidence-backed residual card. |

The decomposition adds no public schema, session format, plugin seam, provider
dialect, or management-command lifecycle. E04 begins only after the E02 exit
evaluation, because immutable provider generations depend on one authoritative
run outcome rather than a presentation-local terminal.
