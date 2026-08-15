# Claude UX cycle 8: recoverable output limits

## Challenge

Ask a local Ollama model for ten full-sentence review tips while limiting Kitsu
to 12 output tokens. Exercise Claude's corresponding one-shot resource-cap
failure with a deliberately tiny `--max-budget-usd` value, then compare how
each CLI helps the user recover.

## Observation

Kitsu correctly printed the partial answer, marked it truncated, rolled the
strict turn back, and exited `2`. Its follow-up said only that output had been
`DISCARDED` because a length limit was reached, even though Kitsu knew the
launch used `--max-output-tokens 12`.

Claude failed with `Exceeded USD budget (0.0001)`. That message at least
preserved the exact limiting value, but it did not offer a concrete retry
command or suggested replacement value.

## Improvement

Length-limit failures now distinguish two cases:

- an explicit `--max-output-tokens N` is reported by name and gets a
  deterministic `2N` retry suggestion;
- a gateway-owned limit asks the user to set an explicit larger
  `--max-output-tokens <n>`.

Human output describes already-visible text as provisional instead of using an
alarming all-caps `DISCARDED` label. Strict semantics do not change: the turn is
not committed and exits `2`. JSONL keeps its stable
`assistant_disposition.status = "discarded"` contract and carries the recovery
instruction in the reason field.

## Verification

A unit test covers explicit and gateway-owned caps. A WireMock integration test
exercises human and JSONL `-p` runs, verifies the provider received
`max_tokens: 12`, asserts the exact `24` retry guidance, and proves exit and
disposition semantics remain strict. Replaying the original challenge against
the rebuilt binary and local Ollama produced the actionable guidance and exit
code `2`.
