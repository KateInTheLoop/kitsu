# Thirty-minute improvement cycle

Every cycle owns one card small enough to reach a green, reviewable state. A
story may need multiple cards, but a card never spans two active cycles.

| Minute | Phase | Required output |
|---:|---|---|
| 0-4 | Requirements | Story/card ID, outcome, evidence, in/out scope, authority/failure/version/rollback notes |
| 4-10 | Test | Smallest deterministic failing test or validation and its expected failure |
| 10-20 | Development | Minimum implementation satisfying the test; no unrelated cleanup |
| 20-24 | Review | Complete diff review for correctness, invariants, bounds, compatibility, secrets/paths, and unrelated changes |
| 24-28 | Acceptance | Targeted tests, affected-crate check/Clippy, format/diff, and named invariant regressions |
| 28-30 | Commit | One focused green commit, cycle record, state, residual risk, and next card |

## Rules

- Every accepted cycle ends in exactly one focused green commit.
- If acceptance cannot finish, do not commit red/WIP production code. Split the
  card into an independently useful fixture, ADR, test, or implementation slice
  and record the no-commit result.
- A test-only commit is valid only when it adds a reusable reviewed fixture and
  the suite remains green. Do not commit a deliberately failing test.
- Move scope expansion to a later card.
- Review the full diff, including generated and lock files.
- Use targeted gates per card. Run workspace/all-target, security, JSONL, and
  platform-relevant gates at epic exits and before rebase/merge.
- Acceptance is provider-free, credential-free, and non-billable unless a
  separately approved manual smoke test explicitly says otherwise.
- Preserve unrelated user changes. Do not stash, reset, overwrite, or absorb
  them without an explicit ownership decision.

## Cycle record

```markdown
# Improvement Cycle NNN

- UTC start/end:
- Branch/base:
- Epic/story/card:
- Requirement:
- Evidence and expected failure:
- In scope / out of scope:
- Authority/failure/version/rollback notes:
- Tests added or changed:
- Implementation:
- Review findings and fixes:
- Acceptance commands/results:
- Commit:
- Residual risk:
- Next card:
```

## Commit convention

Examples:

```text
fix(doctor): accept local Ollama implicit latest alias
refactor(tools): reject duplicate registry admission
test(runtime): add causal carrier outcome fixture
feat(session): publish canonical expected revision
feat(provider): add immutable Anthropic route generation
experiment(mcp): supervise local stdio echo tools
docs(roadmap): record improvement cycle 001
```

The body names the story/card, user outcome, important non-goal, and acceptance
commands. The accepted cycle record belongs in the same commit.
