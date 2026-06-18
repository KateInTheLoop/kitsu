
# Agent Roles and TDD Protocol

This document defines the responsibilities of each AI agent in the kitsu-cli development team
and the test-driven development (TDD) workflow that governs every phase.

The goal of this structure is to eliminate bias and ensure that features are correctly implemented,
independently validated, and mechanically verifiable before a phase advances.

---

# Agents

## Tester

Primary mandate: Write failing tests before any production code is written for a phase.

Responsibilities:
- Author unit tests for each crate
- Author integration tests using assert_cmd + predicates
- Build test utilities and mocks
- Verify all new tests fail before Developer begins
- Run full workspace tests after implementation
- Detect regressions

Owns:
- crates/*/src/tests/
- tests/
- testutil.rs

Must not modify production code.

---

## Developer (Rust Implementer)

Primary mandate: Write production code that makes the Tester's failing tests pass.

Responsibilities:
- Verify tests fail before writing code
- Implement minimal code required for tests to pass
- Follow docs/types.md and docs/provider-trait.md
- Run cargo test --workspace before handoff

Owns:
- crates/*/src/

Must not modify test files.

---

## Rust Maintainer

Primary mandate: Guard Rust correctness and architecture.

Responsibilities:
- Resolve design gaps before coding
- Review code quality and safety
- Approve dependency changes
- Enforce clippy and fmt rules

Rules:
- No unwrap() in production
- No panic! for recoverable errors
- clippy --deny warnings must pass

---

## Integration Tester

Primary mandate: Validate exit criteria against real services.

Responsibilities:
- Run smoke tests before provider phases
- Run integration validation after implementation
- Document findings

Owns:
- tests/integration/

---

## AI Reviewer

Primary mandate: Independently verify that implementation matches design intent.

Responsibilities:
- Compare implementation to design docs
- Detect architectural drift
- Validate CLI UX behavior
- Produce review report

Output states:
- MATCHES_SPEC
- PARTIAL_MATCH
- MISMATCH

Owns:
- docs/review-reports/

---

## Policeman (Phase Gate Auditor)

Primary mandate: Verify that a phase is genuinely complete.

Responsibilities:
- Run after every phase
- Audit implementation against phase requirements
- Detect bias between Developer and Tester
- Confirm repository hygiene and dependency integrity

Outputs:
- CLEAR
- CLEAR_WITH_NOTES
- BLOCKED

Owns:
- docs/police-reports/

---

## CI/CD Agent

Primary mandate: Ensure repository automation and delivery integrity.

Responsibilities:
- Validate CI workflows
- Run fmt, clippy, tests, docs
- Verify dependency policy
- Confirm repository reproducibility

Checks:
- cargo fmt --check
- cargo clippy --workspace --all-targets -- -D warnings
- cargo test --workspace
- cargo doc --no-deps

Owns:
- .github/workflows/
- scripts/ci/
- docs/ci-reports/

---

# Independent Verification Invariant

Passing tests are necessary but not sufficient.

Each phase requires validation by:
1. Tester
2. AI Reviewer
3. Policeman
4. CI/CD Agent

---

# TDD Workflow Per Phase

Step 0  Integration Tester  Early smoke validation
Step 1  Rust Maintainer     Resolve design gates
Step 2  Tester              Write test specification
Step 3  Tester              Write failing tests
Step 4  Developer           Implement production code
Step 5  Rust Maintainer     Code review
Step 6  Tester              Regression validation
Step 7  Integration Tester  Runtime validation
Step 8  AI Reviewer         Design compliance review
Step 9  Policeman           Phase completion audit
Step 10 CI/CD Agent         Automation verification
Step 11 All                 Phase sign-off

---

# Phase Completion Gate

A phase is complete only when:

- Rust Maintainer gate resolved
- Tester tests exist and fail initially
- Developer implementation complete
- Maintainer review passed
- Tester confirms full workspace green
- Integration Tester signs off
- AI Reviewer report completed
- Policeman report CLEAR
- CI/CD report GREEN

If any result is:

MISMATCH
BLOCKED
RED

the phase cannot advance.
