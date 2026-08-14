# Claude UX cycle 9: completion timing

## Challenge

Send the same one-sentence terminal-summary prompt through Kitsu's Hugging Face
gateway and Claude's tool-free one-shot JSON mode. Compare whether the final
completion surface tells a user or automation client how long the run took.

## Observation

Hugging Face rejected Kitsu's inference request with HTTP 401 in roughly 0.18
seconds. Kitsu classified the authentication failure and showed the remaining
deadline, but neither its final human error nor its JSONL `final_summary`
reported total elapsed time. The remaining-deadline value is attempt-local and
requires consumers to know the configured timeout and calculate a difference.

Claude completed the matched prompt and its result object directly exposed
`duration_ms`, `duration_api_ms`, time to first token, and cost. Not every Kitsu
gateway reports cost or token-arrival timestamps, but Kitsu can always measure
its own total monotonic duration.

## Improvement

Inference-capable Kitsu commands now finish with one timing receipt:

- terminals get a dim compact row such as `completed in 1.2 s`, `failed after
  162 ms`, or `cancelled after ...`;
- redirected human stderr gets stable `kitsu-timing` JSON with `duration_ms` and
  `status`;
- every JSONL `final_summary` gets an additive `duration_ms` field.

The clock starts at process entry and uses `Instant`, so wall-clock corrections
cannot make it move backward. Provider-free management commands do not print a
model-run timing footer, and stdout remains assistant-text-only.

## Verification

A renderer unit test covers compact terminal formatting and the exact
redirected record. A WireMock integration test imposes a 40 ms provider delay,
then proves both human and JSONL durations include it and confirms `doctor`
remains free of timing noise. The existing length-limit failure regression now
checks `status: error` in both output modes. Replaying the original Hugging Face
failure against the rebuilt binary produced a 162 ms error receipt and retained
exit code `2`.
