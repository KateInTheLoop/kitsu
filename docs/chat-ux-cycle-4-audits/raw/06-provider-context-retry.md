# Audit 6 — provider, model, context, retry, and redaction

Baseline: `c485edb`. A localhost-only OpenAI-compatible mock covered catalog
normalization, switching, doctor, context discovery/cache/pins, concurrent cache
writes, retry/deadline/cancellation, malformed responses, redaction, and manual
compaction. Only fake tokens were used.

## What worked

- Catalogs are sorted/deduplicated, filters are case-insensitive, zero matches
  are consistent, and an invalid selection does not mutate state.
- Context provenance is unusually strong: conservative cold default, pending
  probe, live refresh, disk-cache age/TTL, CLI/profile pinning, model-switch
  re-resolution, and a detailed compaction ledger.
- Human and JSONL output expose every completed physical retry, status category,
  chosen delay, Retry-After use, remaining deadline, and SIGINT cancellation.
- Central diagnostics withheld all injected response bodies, credentials,
  userinfo, query values, and static-token sentinels.

## Ranked findings

1. **High — online doctor can be green for an absent selected model.** Against a
   normalized catalog of `alpha`, `beta`, and `zeta`, doctor accepted
   `absent-model`, exited zero, and reported the raw duplicate-inclusive count.
   The ordinary model surface reported three unique IDs.
2. **Medium — streaming attempt telemetry declares terminal success at HTTP
   headers.** A response emitted one fragment and then stalled. Kitsu printed a
   terminal successful provider attempt, partial answer text, and only later a
   timeout whose turn was rolled back. Header acceptance is not end-to-end
   completion.
3. **Medium — successful JSONL `/model` and `/compact` mutations have no result
   payload.** Automation sees command start/ok but not old/new model, context
   source, changed/no-op, exchanges removed, or tokens saved unless it issues a
   later `/context` query.
4. **Low — malformed-catalog diagnostics are safely redacted but over-coarsened.**
   Invalid JSON, missing `data`, and invalid `id` all collapse to one message;
   bounded reason codes would remain safe and more actionable.

## Top improvements

1. Validate the exact effective model against the normalized catalog in doctor.
2. Distinguish header acceptance from body completion/failure and mark rolled
   back streamed fragments.
3. Emit typed state-mutation JSONL results.

