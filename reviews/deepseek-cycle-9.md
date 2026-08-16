# Cycle 9 review — `record_progress` (orchestrator.rs:2336‑2362)

## 1. Hardcoded `sequence: 0` is almost certainly wrong

**Line 2348:** `sequence: 0,`

Every `CompactionRecord` pushed through this function gets `sequence: 0`. If
`CompactionLedger::push` is expected to overwrite or compute a sequence number,
passing `0` is at best misleading and at worst a latent bug where a future
refactor of `push` stops overriding it and suddenly every record shares the
same sequence. If `push` does *not* assign its own sequence, then the ledger
cannot distinguish records and sort/display logic tied to `sequence` is broken.

**Hedge:** I have not read `CompactionLedger::push` or any caller, so I cannot
assert whether `push` overrides this field. If it does, remove the `0`
placeholder (use `Default` or an `uninit` sentinel) to make the contract
explicit; if it does *not*, the ledger must compute the next sequence here.

## 2. `progress.outcome?` silently discards the entire compaction attempt

**Lines 2346‑2347:**
```
let outcome = progress.outcome?;
Some(ledger.push(CompactionRecord { … }))
```

When `progress.outcome` is `None` the function returns `None` with no ledger
entry, no log, and no telemetry. A compaction that ran but produced no outcome
(e.g. an error or an early bail-out) disappears from the audit trail. The
caller receives `None` and has no way to distinguish “didn’t attempt” from
“attempted but outcome was missing.”

**Hedge:** The caller may handle this case by inspecting `CompactionProgress`
directly before calling — I have not read the call sites. If it does not,
consider always pushing a record (perhaps with `outcome` as `Option`) or
logging a warning before the early return so failed attempts are visible.

## 3. Field-by-field destructuring is brittle to struct evolution

**Lines 2347‑2361:** Every field of `CompactionProgress` is individually
splatted into `CompactionRecord`. If either struct gains a new field, the
compiler will not warn here — the field is simply ignored in the mapping
(if `CompactionProgress` grows) or defaulted out (if `CompactionRecord` grows).
A destructure bind (`let CompactionProgress { outcome, fallback_reason, … } =
progress;`) followed by a struct literal that uses `..` would make omissions
visible (unused-variable warnings) or explicit (`..Default::default()`).

**Hedge:** I have not read either struct definition. If they are intended to
stay 1:1, this is cosmetic; if they already have diverged fields the silent
mismatch may have already caused bugs.

## 4. Receives `&AgentConfig` but touches only `cfg.compaction`

**Line 2338:** The function accepts the full `AgentConfig` reference solely to
forward `cfg.compaction` as `configured_mode` (line 2350). This couples a
narrow compaction helper to a type that likely carries provider, model,
retry, and tool configuration. Passing only the needed enum variant would
narrow the dependency and make unit-test setup lighter.

**Hedge:** Without seeing callers, I cannot tell whether they already have a
convenient `AgentConfig` on hand and this is the pragmatic choice. If so, this
is low priority.
