# Status, context, and usage truth contract

Status: scoped by E08-S05-C0 in Cycle 030; implementation is planned after its
owning provider/session inputs.

## Decision

Kitsu will improve its existing `/status`, `/context`, and `/cost` surfaces
through shared presentation-neutral snapshots. It will add `/usage`, retain
`/cost` as a compatibility alias, and add focused context views. This is E08
observability work, not a new architectural spine or a universal provider
billing API.

The inspiration is deliberately narrow:

- Codex demonstrates a compact status deck and daily/weekly/cumulative usage
  views. Its official command reference describes `/status` as local session
  telemetry and `/usage` as authenticated Codex account activity. The latter
  is product-specific and cannot be treated as a portable provider contract.
- The user-supplied Claude Code `/context window` example demonstrates a useful
  visual separation between occupied context, free space, source categories,
  and lazily available tools. It is UX evidence, not proof of a portable
  Claude API.

Primary OpenAI reference, reviewed 2026-08-19:
<https://learn.chatgpt.com/docs/developer-commands?surface=cli>.

## Existing foundation and measured gaps

Kitsu already has more than a command shell to build on:

- `/status` reports version, session/autosave, gateway/model/reasoning,
  directory/Git, redacted endpoint, auth presence, context provenance,
  conversation, compaction, tools, permissions, commands, artifacts, agents,
  and a structured JSONL result.
- `/context [refresh]` reports provider-anchored or estimated utilization,
  source rows, window source/cache/discovery, runtime mismatch evidence, and
  compaction history.
- `/cost` reports current-process prompt/completion/total usage, subagent
  inclusion, per-model bins, and live context position.
- normalized provider `Usage` already carries optional cached-token data; the
  renderer already supports terminal width, Unicode/ASCII, `NO_COLOR`, plain,
  screen-reader, redirected, and JSONL modes.

The remaining gaps are precise:

- `/status` is a long fixed block instead of an adaptive deck;
- `/context` has no `/context window` or `/context sources` focused view;
- `/cost` has no structured result and its name suggests currency even though
  Kitsu has no portable pricing truth;
- current tallies lose cached-token, request-purpose, missing-report coverage,
  and future route-generation attribution;
- there is no durable daily/weekly/cumulative Kitsu activity history;
- status cannot yet show E04 `{adapter, profile, generation}` or E03 canonical
  revision/HEAD because those contracts do not exist yet.

## Three distinct truth domains

These domains must not be collapsed into one number or one provider-shaped
type.

1. **Effective status** is Kitsu-owned current state: version, route identity,
   model/capabilities, session/revision, directory, effective authority,
   command containment, tool-source inventory, and cached provenance. It is
   always available offline.
2. **Context occupancy** is the current request/session window: effective
   window, reserved output, used/free space, source categories, compaction, and
   provenance. It may mix provider-reported anchors with bounded estimates, so
   every field carries a truth class.
3. **Usage activity** is provider-reported token activity observed by Kitsu.
   Current-session totals are transient. Daily/weekly/cumulative views come
   from Kitsu's bounded local observation journal. Neither is an account bill,
   credit balance, provider-wide activity log, or rate-limit service.

Every quantitative field uses one of these labels:

| Truth class | Meaning |
|---|---|
| `authoritative` | Kitsu owns the identity, authority, or committed state. |
| `reported` | A named provider response supplied the value. |
| `derived` | Exact arithmetic over authoritative or reported inputs. |
| `estimated` | A bounded Kitsu estimate, such as tokenizer-independent context rows. |
| `unavailable` | Unsupported, not configured, stale beyond policy, or not reported. |

`unavailable` never becomes zero. Local observations must not be described as
“billed truth”: cancelled or rolled-back work can consume reported tokens, and
some providers omit usage entirely.

## Command and compatibility contract

```text
/status
/context
/context window
/context sources
/context refresh
/usage
/usage session
/usage daily
/usage weekly
/usage cumulative
/cost                 alias of /usage session
```

- Bare `/usage` means `session`; it is useful before durable history ships.
- Bare `/context` preserves the current detailed behavior. `window` is the
  compact visual occupancy view and `sources` emphasizes provenance rows.
- `/status`, `/context`, and `/usage` are read-only and offline. They create no
  provider request, child, catalog refresh, route replacement, authority grant,
  or session snapshot.
- `/context refresh` retains its existing explicit window-discovery network
  behavior and must disclose the attempt/result.
- Existing `/status` and `/context` JSON keys evolve additively. `/cost` remains
  accepted for scripts and interactive users and receives the same structured
  result as `/usage session`.

## Portable snapshots

`StatusSnapshot`, `ContextSnapshot`, and `UsageSnapshot` are Kitsu-owned DTOs.
Renderers consume them; they do not rediscover state, read config, inspect
environment variables, query a provider, or parse prior terminal text.

Portable fields include:

- active session/run identity and, after E03, canonical revision and logical
  HEAD;
- after E04, adapter/profile/generation, wire revision, model, reasoning, and
  declared capabilities;
- host-observed directory/Git status;
- effective permission mode, root/protection counts, exact command grants, and
  containment state;
- frozen tool inventory/source generations and, after E06, supervised MCP
  health;
- context window/effective budget/reserved output/free space, category values,
  compaction, source, freshness, and truth class;
- Kitsu-observed provider-reported prompt/completion/cached/total tokens,
  grouped by model, route generation, request purpose, and subagent/foreground;
- coverage: requests attempted, responses reporting usage, missing reports,
  outcome classes, and observation time;
- bounded local daily/weekly/cumulative aggregates.

Non-portable fields include account email/organization, plan, credits, billing
spend, invoices, exact provider rate-limit remainder/reset, provider-wide
activity outside Kitsu, and product-specific login state. A future source for
those fields must be separately named, documented from a primary API, admitted
behind explicit authentication and refresh, and represented as timestamped
provider-specific data. It is not part of E08-S05/S06.

## Human and automation presentation

Human layouts adapt without changing meaning:

| Width/mode | Required behavior |
|---|---|
| 72+ columns | Bordered, grouped deck with compact progress visualization. |
| 40-71 columns | Stacked sections; identities wrap or middle-elide with labels intact. |
| Below 40 columns | Simple key/value rows; no essential field disappears. |
| Plain, screen-reader, redirected | Stable ASCII labels and values; no ANSI or width dependency. |
| Styled Unicode | Blocks, borders, and colors are decorative; text and percentages carry identical information. |

JSONL continues the existing
`command_started -> command_result -> command_finished` framing. Result bodies
use `kind: status|context|usage` plus an additive `schema_revision`. Terminal
box text is never the data contract. Historical usage points are bounded and
include interval boundaries, truth source, observation coverage, completeness,
and route/model identity.

## Story and card plan

### E08-S05 — truthful session telemetry decks (6-9 cycles)

1. Extract the three DTOs and truth-class vocabulary from current builders;
   freeze additive JSON vectors before changing layout.
2. Extend usage tallies with cached tokens, request purpose,
   foreground/subagent attribution, route generation, and reported/missing
   counters; count reported failed/cancelled attempts honestly.
3. Add `/usage [session]`; make `/cost` a compatibility alias with identical
   typed output and remove currency/billing language from local totals.
4. Add adaptive `/status` rendering over the shared snapshot.
5. Add `/context window|sources` while preserving bare `/context` and explicit
   refresh semantics.
6. Run the complete portable provider/failure/presentation/PF-10 matrix and
   update executable slash-command documentation.

The first high-value slice—adaptive `/status`, `/context window`, and `/usage
session`—is 4-7 cycles once E04's identity inputs are stable.

### E08-S06 — bounded local usage history (6-9 cycles)

1. Define an idempotent, body-free `UsageObservation` keyed by run/request
   identity, route generation, purpose, outcome, reported fields, and observed
   time.
2. Publish no-overwrite observations through E03's revision/fencing/recovery
   mechanisms; aggregates remain disposable projections.
3. Add bounded interval aggregation with explicit local timezone and UTC
   boundaries plus coverage/completeness.
4. Add `/usage daily`, `/usage weekly`, and `/usage cumulative` human views.
5. Add bounded structured points and additive schema/examples for automation.
6. Pass duplicate/crash/corrupt/future/rebuild, retention, timezone/DST, and
   incomplete-report acceptance before promotion.

## Acceptance matrix

| Axis | Required cases |
|---|---|
| Provider truth | Compatible Chat, distinct Messages, and a fixture that omits usage. |
| Lifecycle | Model switch, generation replacement, compaction, subagent work, rollback, cancellation, provider failure, and failed delivery. |
| Context | Known/unknown window, reported anchor, rejected anchor, estimates, shortfall clamp, fresh/stale/missing cache, reserved output. |
| Terminal | Widths 24/40/60/80/120; Unicode/CJK/control text; styled, `NO_COLOR`, plain, screen-reader, redirected. |
| Automation | Existing command framing; additive status/context fields; structured usage; bounded points; query-only commands create no session snapshot. |
| Security/offline | Secret-bearing endpoint/config fixtures; no secret/account identity; zero network except explicit `/context refresh`; zero new authority/effects. |
| History | Duplicate IDs, old-or-new crash cuts, corrupt/future data, projection deletion/rebuild, timezone/DST, retention ceiling, incomplete provider reports. |

Current test owners to extend are `crates/kitsu/src/main.rs`,
`crates/kitsu-render/src/lib.rs`, `crates/kitsu-core/src/orchestrator.rs`,
`crates/kitsu-providers/src/types.rs`,
`crates/kitsu-providers/tests/provider_wire_matrix.rs`,
`crates/kitsu/tests/cli.rs`, and, after E03, `crates/kitsu/src/session.rs` plus
the K01-K12 fixtures.

## Security, failure, and rollback rules

- Do not estimate missing provider usage for billing-like totals or infer
  context, price, quota, or capability from a model name.
- Show source, `as_of`, freshness, and missing-report coverage where relevant.
- Preserve endpoint userinfo/query redaction. Disclose auth presence/source
  class only—never tokens, headers, bodies, secret hashes, or account identity.
- Persist no prompts, responses, tool output, credentials, endpoints, or raw
  provider payloads in usage history.
- Query commands never widen authority or mutate the route/session.
- A corrupt journal fails closed or recovers through E03's verified contract;
  a projection, file tail, cache, or renderer is never canonical truth.
- Each card is source-revertable. Snapshot fields are additive; `/cost` remains
  until a separately reviewed compatibility removal. Durable history ships
  only after its schema/version/retention and rollback rules pass crash cuts.

## Hard exclusions

- scraping Codex, Claude, provider web UIs, or undocumented account endpoints;
- a universal billing/account interface that pretends providers expose the
  same facts;
- ambient plugin, script, or executable telemetry collectors;
- default network calls from `/status`, `/context`, or `/usage`;
- currency, credit, cache-discount, or rate-limit guesses from token counts;
- raw content, credentials, secrets, or account identity in snapshots/history;
- unbounded lifetime scans or unbounded calendar/JSON payloads;
- ANSI, Unicode blocks, colors, or box geometry as machine semantics;
- an account-usage implementation on the critical path without a named
  provider, explicit auth contract, primary evidence, and separate approval.

## Schedule impact

This addition is 12-18 cycles: six to nine nominal loop-hours and roughly two
to three realistic focused implementation days. It does not interrupt E04:
Cycle 030 records this contract, and Cycle 031 resumes E04-S01-C3. DTO/renderer
fixtures can overlap later provider/session work, but live route fields wait
for E04 and durable history waits for E03.
