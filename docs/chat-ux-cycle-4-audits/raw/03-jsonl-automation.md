# Audit 3 — JSONL and automation

Baseline: `c485edb`. Ten ask, piped, script, resume, retry, tool, command,
SIGINT, and broken-pipe scenarios produced 103 JSONL records. Every writable
stream passed the published schema plus ordering and pairing checks. No hosted
provider or repository mutation was used.

## What worked

- Ask, piped, scripted, durable resume, provider retry, filesystem tool,
  command, and cancellation streams remained parseable and causally ordered.
- SIGINT exited 130 with a cancelled physical attempt and terminal summary.
- Closing stdout after `run_started` failed the process before any provider send.
- Routine JSONL stderr stayed empty, and turn/tool/command frames paired in the
  exercised cases.

## Ranked findings

1. **High — JSONL disables model streaming.** Agent configuration forces
   `stream=false`, so automation gets no live assistant deltas even when the
   provider supports streaming. Long turns appear frozen until one final body
   arrives.
2. **High — command result payloads are only weakly typed.** `/status` and
   `/permissions` return `{kind,text}` human blobs. The schema requires little
   per-kind structure and has no conformance corpus proving query and mutation
   payload compatibility.
3. **High-medium — local policy failures are counted as provider failures.** A
   tool `PolicyDenied` outcome increments `provider_failures`, obscuring whether
   retries, credentials, or local authority caused the failure.
4. **Medium — script turn identifiers expose absolute host script paths.**
5. **Low — help permits diagnostics on stderr although the actual runtime keeps
   it reserved for framing failure.**

## Top improvements

1. Stream versioned `assistant_delta` events without compromising framing.
2. Define typed schemas for every supported query and mutation result.
3. Separate provider, tool, policy, user, and cancellation failure accounting.

