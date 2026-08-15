# Claude UX cycle 26 — separate action and structured finalization

Date: 2026-08-14
Seed: `992626`
Boundary: Unix epoch `1786726800`

## Scored evidence

All three workers pinned the same pre-change image before preparing their
fixtures, SHA-256
`9fcb1d268dd89b72de54d860f061c49a45242fc9fa640d91385a86d8589fd3f1`.
Every scored Kitsu leg ran from a private copy and released the shared target
immediately.

- The randomized edit task scored **85/100**. Kitsu sent one request with both
  tools and the strict response format; Qwen made no tool call, returned a
  schema-valid placeholder, and left the required target unchanged. The host
  postcondition correctly rejected completion with exit `1`, an unchanged
  fingerprint, `postcondition_failures: 1`, and no `-o` replacement.
- The deterministic two-target creation lifecycle scored **98/100** and
  isolated the transport problem. It completed two verified writes and an
  exact structured answer, but all three requests carried the strict response
  format: `structured_output: [true, true, true]`, including both action
  rounds. That repeated constraint is unnecessary context overhead and, as the
  randomized leg showed, can bias a smaller model toward producing a
  schema-shaped answer before acting.
- The Hugging Face-shaped local lifecycle scored **95/100**. Its new
  existing-target/read-authority preflight passed exactly. A valid read and
  conditional write also succeeded, but the fixture rejected Kitsu's
  documented tool-free max-round finalization envelope; Kitsu retained the
  verified effect, reported the provider failure, and preserved `-o`. This was
  a harness expectation error, not a product failure. Hosted HF remained
  `not_run: infrastructure` because neither credential gate was populated, so
  no remote request was sent.

The standalone Codex control was `not_scored: infrastructure`: its isolated
environment did not resolve the absolute installed CLI and exited `127` before
provider contact. No comparator fallback or scored rerun was used.

## Selected improvement

When `--output-schema` and `chat --require-write` are paired, Kitsu now splits
each bounded model turn into two explicit phases:

1. tool-capable action requests omit the provider response format; and
2. after the model stops calling tools, one tool-free request applies the
   strict schema and asks only for the final answer.

The provisional action response stays in model context but is never rendered,
emitted as committed assistant output, saved, or eligible for `-o`. The final
request still receives independent local schema validation, and the existing
required-write journal/fingerprint postcondition remains authoritative before
the host installs the final-message artifact. Both phases share the original
whole-turn deadline and max-round budget. A one-round configuration is rejected
before `run_started` or provider contact.

The finalization boundary fails closed in two additional cases: native tool
calls returned after tools were withheld are rejected without execution, and
the context meter invalidates its prior tool-schema token anchor before
budgeting the different response schema. The latter prevents a near-full
context from undercounting a large final schema.

## Regression and release gate

Core and end-to-end regressions prove:

- exact request vectors `structured_output: [false, false, true]` and
  tools-present/tools-present/tools-absent;
- hidden provisional action text and exact final committed output;
- verified required writes precede atomic `-o` installation;
- impossible `--max-rounds 1` fails with zero provider requests;
- an unoffered finalization tool call never reaches its handler; and
- a changed, oversized final schema is remeasured and rejected before the
  second provider request despite real usage telemetry from the action round.

The post-change gate passed formatting, schema parsing, `git diff --check`, all
**762** workspace tests, and warning-denied Clippy. The resulting Cycle 27
debug binary is 153,500,200 bytes with SHA-256
`b21a3781785bed6b9d54513cf1b4dcd9ea08d92f0470592675d57740df2a085e`.
