# ISSUE-007 — Repeated identical failing tool calls burn all rounds (no loop-breaker)

- **Status:** OPEN
- **Severity:** Medium (robustness + latency; wastes rounds/tokens, causes timeouts under load)
- **Found by:** dogfooding, full regression (persona `adversarial_02` timed out >150s)
- **Area:** `kitsu-core` orchestrator loop
- **Note:** anticipated in the original design review ("Error-as-tool-result has no
  loop-breaker" — P2). Now confirmed live.

## Symptom
A model that repeatedly calls a tool that keeps failing the same way (e.g. reading
a denied path over and over) is never stopped early — it consumes every one of
`max_rounds` model inferences before the loop's backstop fires. On a slow/loaded
local model each round is ~25s, so the turn ran past the 150s case timeout.

## Root cause
`error-as-tool-result` (design §5) correctly feeds a failing tool's error back and
continues, but there is no detection of *repeated identical failures* — the same
`(tool_name, args)` producing the same error round after round just loops until
`max_rounds`.

## Impact
- Wasted model inferences (latency + tokens) when a model fixates on a denied or
  impossible tool call.
- Under CPU load, tool-heavy turns approach/exceed timeouts.
- Not caused by the ISSUE-001/002/006 changes (pre-existing); those fixes did make
  multi-call turns do more *legitimate* work per turn, which compounds the latency.

## Proposed fix
- Track a signature of each failing tool call `(tool_name, args_hash) -> consecutive
  failure count`. If the same call fails N times (e.g. 2) in a turn, stop offering
  that tool (or short-circuit the turn to a final answer) with a note fed back:
  "repeated identical failure — stop retrying and explain/adjust."
- Optionally cap total tool executions per turn independently of `max_rounds`.

## Verification
- Unit: a mock where the model always emits the same failing tool call → the loop
  breaks after the Nth identical failure (fewer than `max_rounds` rounds), returns
  gracefully.
- Live: the denied-read scenario finishes quickly with a clear explanation instead
  of looping to the round cap.
