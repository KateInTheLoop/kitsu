# Cycle 3 jobs and steering post-integration audit

Date: 2026-08-10

Scope: background-agent progress, queue ownership, live steering, terminal
notices, shutdown, and portability.

## Findings and disposition

| Severity | Finding | Disposition |
| --- | --- | --- |
| High | Running jobs stayed at zero tokens/tools/writes until termination because their event log was summarized only after the turn. | Fixed: background reports publish body-free live usage and activity into `AgentRegistry`; `/jobs` shows recent activity while withholding partial model text. |
| High | A full FIFO stopped reading stdin, so `/steer` could be delayed and target a later turn; collector startup also raced turn creation. | Fixed: the steering turn exists before collection, control lines bypass queue capacity, and ordinary overflow is explicit rather than retained in the terminal buffer. |
| High | Background completion notices used Rustyline's external printer while canonical type-ahead owned stdin, which could split partially entered input. | Fixed: notices are deferred while type-ahead is active and flushed after ownership returns; renderer fallback is retained. |
| Medium | `/queue remove`, `edit`, and `promote` typed during generation became queued prompts themselves. | Fixed: queue-control commands execute in the collector rather than entering the prompt FIFO. |
| Medium | `/jobs stop` could report completion before a blocking filesystem mutation reached the journal. | Fixed: stop paths join the job and drain committed mutation reports before announcing completion. |
| Medium | EOF implicitly waits for jobs, while `/quit wait|cancel` is explicit. | Open UX choice; behavior is documented and remains deterministic. |
| Medium | Running-job limits are fixed and have no token/cost budget. | Open enhancement; concurrency and retained-record bounds still prevent unbounded admission. |
| Medium | Type-ahead steering is Unix-only. | Documented portability residual; non-Unix remains safe but lacks live steering. |
| Low | Notice strings carried their own `~`, producing a doubled renderer prefix. | Fixed at the source formatter. |

## Privacy and observability constraints

Live job telemetry contains model name, aggregate token usage, tool count,
successful filesystem-mutation count, and bounded safe activity labels. It does
not retain provider bodies, tool arguments/output, completion IDs, call IDs, or
partial assistant prose.

## Release recommendation

GO. The input-ownership blockers are fixed, focused queue/steering/background
tests pass, and the prior loopback PTY smoke exercised ordinary plain-mode
steering. Retain the non-Unix limitation and the deferred-notice drain race as
documented next-cycle work.
