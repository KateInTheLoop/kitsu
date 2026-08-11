# Audit 10 — adversarial release gate

Baseline: clean `af20c26`. The auditor used the real Kitsu binary and approved
localhost mocks; no hosted provider or real token was used.

## Confirmed strengths

- Human invalid-response diagnostics were fast, actionable, and body-free.
- JSONL failure emitted seven parseable records with one run ID, causal provider
  request/attempt, typed provider error, closed turn, and final summary.
- Routine stderr stayed silent; summary counters distinguish provider, policy,
  tool, cancellation, and user failures.

## Release finding

**High — positional non-TTY chat can hang before the inference deadline.**

Repro:

```text
target/debug/kitsu --config /tmp/kitsu-ux-live-config.toml \
  --plain --timeout-secs 5 chat --no-session \
  'Cycle-five success-path probe; answer briefly.'
```

The process remained blocked for more than 800 seconds until externally
interrupted. A positional prompt must not wait for unrelated stdin EOF, and the
five-second inference deadline currently does not bound this pre-provider phase.

Release verdict: **HOLD**.

Top priorities: start positional input immediately with held-open stdin; define
one end-to-end deadline across input/provider/retry/shutdown; maintain a local
human/JSONL release smoke suite for success, failure, cancellation, and open
stdin.
