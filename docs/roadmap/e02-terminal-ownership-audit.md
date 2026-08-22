# E02 Terminal Ownership Audit

Status: accepted. Cycle 027 closed the final exit-evaluation card after the
independent seven-carrier harness passed against the production evaluator.

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
| `ask --jsonl` after setup | Typed status and assistant disposition | Structurally joined; persistence not requested | Final JSONL summary | Shared process lifecycle | Accepted through C6 |
| scripted JSONL | Typed aggregate status | Actual child statuses and active-snapshot receipt | Terminal error/summary closure | Shared process lifecycle | Accepted through C6 |
| piped JSONL | Typed branch status | Actual rollback/exit child statuses and active-snapshot receipt | Terminal error/turn/summary closure | Shared process lifecycle | Accepted through C6 |
| human ask/review/piped/scripted | Typed process outcome and assistant DTO in `main` | Carrier handoff supplies structural or observed cleanup and active-snapshot persistence | Fallible human timing receipt | Shared process lifecycle | Accepted through C7; text unchanged |
| interactive chat | Typed multi-operation aggregate; success plus any recoverable non-success is partial | Status-preserving rollback/exit cleanup, detached terminal snapshots, and active-session persistence | Fallible human timing receipt | Shared process lifecycle | Accepted through C8 and C2 |
| inference preflight/top-level fallback | `CliError` class and assistant state | Pre-start is structurally joined; missing post-start evidence is `NotObserved`; snapshot receipt is independent | JSONL summary or fallible human timing | Shared process lifecycle | `NotObserved` cannot certify clean success |

## Cycle 023 findings that authorized the bounded work

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
| E02-S03-C7 — presentation-neutral facts (accepted Cycle 025) | Carry semantic, cleanup, persistence, and delivery inputs independently of JSONL; migrate human ask/review/piped/scripted without changing their text or JSONL v1. | Human/JSONL paired fixtures agree on terminal fields; blocked delivery never reclassifies settlement. |
| E02-S03-C8 — interactive aggregate (accepted Cycle 026) | Define process aggregation for successful, failed, cancelled, interrupted, retried, and queued turns; join background work and type session persistence before publication. | PTY fixtures cover clean exit, recoverable fault then exit, active/queued cancel, authority EOF, residue, and save failure. |
| E02-S04-C2 — exit evaluation (accepted Cycle 027) | Run the independent seven-carrier matrix against the process owner and close E02 only if every applicable phase/exit cell is observed. | X004 evaluation receipt, exact exits, one terminal, no silent cells; otherwise one evidence-backed residual card. |

The decomposition adds no public schema, session format, plugin seam, provider
dialect, or management-command lifecycle. E04 begins only after the E02 exit
evaluation, because immutable provider generations depend on one authoritative
run outcome rather than a presentation-local terminal.

## Cycle 027 exit decision

The process now assigns one explicit carrier identity immediately after argument
parsing: ask, piped, and scripted human/JSONL plus interactive human. Every
successful lifecycle publication is retained once beside the process owner.
`process_exit` is the single evaluator for both human and JSONL completion:

- a prior nonzero causal status remains primary;
- clean automation maps success/partial to 0, user/tool/interruption to 1,
  provider fault to 2, and cancellation to 130;
- cleanup other than `Joined`, failed persistence, or incomplete/failed
  delivery turns an otherwise-zero status into 1;
- recoverable interactive per-turn outcomes remain in the terminal receipt and
  do not make a deliberate clean `/quit` fail.

The independent carrier test includes the production evaluator source directly,
constructs immutable `PublishedRun` values without a provider, and executes 77
exact exit vectors across all seven carriers. The original 119-cell declaration
still has no omitted cells, and live CLI fixtures retain JSONL preflight, human/
JSONL timing, piped, cancellation, sink, and interactive PTY behavior. E02 is
therefore accepted with no residual card. This evaluates X004's default Kitsu
runtime; it does not promote X004 to replaceable because no named alternate
runtime adapter executes the suite.
