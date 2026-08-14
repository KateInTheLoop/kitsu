# Kimi-K3 cycle 5 — run failed on output-budget truncation, no deliverable from the model

**Status: FAILED (kitsu/model interaction, not a transport failure — no retry
taken).** Kimi-K3 read both named files completely and cleanly, then was
truncated by `--max-output-tokens 14000` before writing anything. All
provisional output was discarded per kitsu's turn-commit semantics.
`reviews/kimi-cycle-5.md` (this file) is written by the supervisor, not the
model. The cycle-5 usage/caching review of `crates/kitsu-providers/src`
carries over, unattempted, to the next cycle 5 retry.

## What happened

Single attempt (no retry — see "Why no retry" below):

| | attempt 1 |
|---|---|
| wall time | 106.8s |
| tool calls | 3 (`Read` types.rs whole; `Read` openai_compat.rs 1-850; `Read` openai_compat.rs 850-1694) — 0 list calls, 0 `Agent` spawns |
| ranged-read behavior | both `openai_compat.rs` ranges returned full width instantly (`0ms` each) — the range-cap fix continues to hold under load |
| visible model output | one line only: "I'll read the two files now — all three ranges in parallel." Nothing else was ever emitted before truncation. |
| `kitsu-usage` prompt/completion/total | 19,620 / 14,322 / 33,942 |
| `kitsu-context` used/effective/window | 17,007 / 117,072 / 131,072 |
| truncation warnings | 1 — `response truncated: model hit its output length limit` |
| outcome | `provisional assistant output was DISCARDED and was not committed: model output was truncated by its length limit`; run summary `0 turn(s) ok / 1 failed, 3 file(s) read, 0 file(s) written (0 change(s))`; process exit code 2 |

`completion_tokens` (14,322) came in *over* the nominal 14,000 cap (hidden
reasoning plus scaffolding), yet produced zero characters of the requested
review — the entire budget was consumed invisibly. This is a materially
worse outcome than cycle 4's success on the *same* 14,000 cap: cycle 4 read
one 378-line file and finished in 6,282 completion tokens with a full
deliverable; cycle 5 asked for two files totaling 1,866 lines (types.rs 172
+ openai_compat.rs 1,694) with a two-part analytical task (locate/diagnose a
drop, then rank proposals), and burned more than double cycle 4's completion
tokens on reasoning alone before hitting the wall.

## Why no retry

The loop's rules permit "one run + at most one retry **on transport
failure**". This attempt reached the provider, read both files
successfully, and was truncated by kitsu's own output-length enforcement —
an HTTP-200-all-the-way execution outcome, not a connection/rate-limit/5xx
transport failure (contrast cycle 1's `429` or cycle 3's `402`, both of
which were transport-layer and did warrant the permitted retry). Retrying
with an identical command would very likely reproduce the same truncation,
since nothing about the failure was transient. No retry was taken; this
cycle is recorded as a single-attempt failure.

## Grounded analysis of the standing task, done by the supervisor since the model did not reach it

To keep this failure record useful despite no model deliverable, the
supervisor verified where usage is parsed and what cycle 5 would have found,
using the same two files the model was given:

- **`crates/kitsu-providers/src/types.rs:136-144`** — `Usage` has exactly
  three fields: `prompt_tokens`, `completion_tokens`, `total_tokens`, each
  `#[serde(default)]`, `Deserialize`d directly from the gateway's JSON
  `usage` object. There is no `deny_unknown_fields` attribute, so any extra
  JSON key the gateway sends — including HF's `prompt_tokens_details.
  cached_tokens` — is silently dropped by serde during deserialization; it
  never reaches Rust code to be ignored, checked, or logged. `Usage::add`
  (`types.rs:153-157`) sums only these three fields across multi-round
  turns, so even a later fix that captured `cached_tokens` per-response
  would additionally need `add()` extended or cached tokens would still be
  lost on merge.
- **`crates/kitsu-providers/src/openai_compat.rs`** — usage flows through
  two paths: streaming (struct field `usage: Option<Usage>` at line 252/294
  and again as `StreamState::usage` at line 373-374; populated at line
  449-453 when a chunk carries it, most gateways send it only on the final
  chunk; surfaced as `ChatDelta::Usage` at line 534-535) and non-streaming
  (`resp.usage` surfaced as `ChatDelta::Usage` at line 616-618, gated on
  `!usage.is_empty()`). Both paths deserialize straight into the same
  `Usage` type from `types.rs`, so the drop described above applies
  identically to streaming and non-streaming responses.
- This confirms the standing backlog item ("surface provider `cached_tokens`
  ... in `kitsu-usage:` records") is real and precisely located: the fix is
  a new `Option<CachedTokenDetails>`-shaped field on `Usage`
  (`types.rs:137-144`) fed by a `prompt_tokens_details` struct with its own
  `#[serde(default)]` `cached_tokens: u64`, then threading it through
  `Usage::add` and into whatever renders `kitsu-usage:` lines (not in either
  file the model was scoped to — likely `kitsu-render` per cycle 3's
  citation of `crates/kitsu-render/src/lib.rs`).
- This run's own `kitsu-usage:` line (`{"prompt_tokens":19620,
  "completion_tokens":14322,"total_tokens":33942}`) has no `cached_tokens`
  key, consistent with (not new proof of) the drop above — this was a
  fresh, single-turn session with no cache-eligible repeated prefix, so
  absence here doesn't distinguish "dropped" from "never offered."

The 2-4 further ranked caching proposals the task asked for (model catalog/
window discovery caching, retry/telemetry observations grounded in the two
files) were **not** produced by the supervisor — that synthesis belongs to
the model's read of the full 1,694-line `openai_compat.rs`, most of which
the supervisor did not re-read end-to-end for this failure record. They
remain open for the next cycle-5 attempt.

## Follow-up required before the next attempt

- Raise `--max-output-tokens` further (e.g. 20,000-24,000) for any task that
  asks the model to read *and analyze two* non-trivial files, not just one —
  cycle 4's 14,000 cap was sufficient for a single 378-line file with a
  simpler ask; cycle 5's two-file, two-part analytical ask exhausted the
  same cap on reasoning alone.
- Consider splitting cycle 5 further: one attempt to just locate and
  line-cite the `cached_tokens` drop (small, cycle-4-shaped), a second to
  produce the ranked caching proposals — mirroring the ramp's own strategy
  of narrowing scope until a turn reliably completes.
- This is the first cycle where **completion_tokens exceeded the nominal
  cap** (14,322 vs 14,000) rather than merely approaching it — worth noting
  precisely for the open reasoning-headroom backlog item, since it shows
  the overrun is not bounded by the cap being "close" to sufficient.
