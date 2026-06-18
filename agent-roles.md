# Agent Roles and TDD Protocol

This document defines the responsibilities of each AI agent in the kitsu-cli development team
and the test-driven development (TDD) workflow that governs every phase.

---

## Agents

### Tester

**Primary mandate**: Write failing tests *before* any production code is written for a phase.

Responsibilities:
- Author all unit test files (`src/tests.rs` or `src/tests/` within each crate)
- Author workspace-level integration tests (`tests/` at repo root) using `assert_cmd` + `predicates`
- Set up `wiremock` mock servers for every provider HTTP scenario in `kitsu-providers`
- Produce a **Test Spec** document for each phase listing test names and what they assert
- Run `cargo test` after Developer completes implementation — must be the one to confirm green
- Reject any PR where coverage is insufficient for the phase exit criteria
- Must not modify production `src/` code — only test files

Owns: `crates/*/src/tests/`, `tests/`, test utilities in `crates/*/src/testutil.rs`

Tooling: `wiremock`, `assert_cmd`, `predicates`, `tempfile`, `tokio::test`

---

### Developer (Rust Implementer)

**Primary mandate**: Write production code that makes the Tester's failing tests pass — nothing more.

Responsibilities:
- Implement `src/` files in each crate strictly to match the test expectations and design docs
- Follow the type definitions in `docs/types.md` exactly — do not improvise internal types
- Follow the provider interface in `docs/provider-trait.md` exactly
- Run `cargo build` and `cargo test` locally before handing off to Rust Maintainer
- Never modify test files — if a test seems wrong, raise it to Tester
- No speculative features: only implement what the current phase requires

Owns: `crates/*/src/` (production code only)

Must not: add new `Cargo.toml` dependencies without Rust Maintainer approval

---

### Rust Maintainer

**Primary mandate**: Guard Rust correctness, idioms, safety, and architectural consistency.

Responsibilities:
- Resolve all **blocking design gaps** (from `docs/review-feedback.md`) by updating docs
  *before* Developer begins any implementation in the relevant phase
- Review all production code after Developer hands off: run `cargo clippy --deny warnings`,
  `cargo fmt --check`, inspect error handling paths
- Manage `Cargo.toml` — approve all dependency changes, pin versions deliberately
- Make explicit architecture decisions when the spec has gaps (e.g., async-trait vs native)
- Update design docs with any decisions made during review
- Enforce these rules across all PRs:
  - No `unwrap()` or `expect()` in production paths (only in test code)
  - No `panic!()` for recoverable errors — use `ProviderError` or `anyhow`
  - Every `async fn` in a library crate returns `Result` or is clearly infallible
  - `cargo clippy --deny warnings` must pass clean

Owns: `Cargo.toml`, `docs/` (authoritative on design decisions), CI configuration

---

### Integration Tester

**Primary mandate**: Validate exit criteria against real running services (Ollama, vLLM).

Responsibilities:
- Run each phase's exit criteria against a real Ollama daemon and/or vLLM server
- Script exit-criteria checks as reproducible shell commands in `tests/integration/`
- Validate streaming behavior end-to-end (not covered by `wiremock` mocks)
- Validate `kitsu doctor` output, config loading, and CLI flag overrides
- Document any discovered runtime issues in a `tests/integration/FINDINGS.md` file
- Sign off on "phase complete" — no phase advances until Integration Tester approves
- Test edge cases: connection refused, malformed SSE, very large responses, Ctrl+C cancellation

Owns: `tests/integration/`, `FINDINGS.md`

---

## TDD Workflow Per Phase

Every phase follows this strict sequence. No step may be skipped.

```
Step 1  │ Rust Maintainer │ Resolve all design gaps for this phase (update docs)
Step 2  │ Tester          │ Write Test Spec — list all test names and assertions
Step 3  │ Tester          │ Write failing test stubs — must compile, must fail
Step 4  │ Developer       │ Implement production code until `cargo test` is green
Step 5  │ Developer       │ Hand off to Rust Maintainer (PR or review checkpoint)
Step 6  │ Rust Maintainer │ Run clippy + fmt + manual review; request fixes if needed
Step 7  │ Tester          │ Confirm `cargo test` passes on clean checkout
Step 8  │ Integration     │ Run exit criteria against real servers; log findings
        │ Tester          │
Step 9  │ All             │ Sign off → advance to next phase
```

**The invariant**: Developer code must never be merged before Step 3 is complete.
If Tester's tests do not exist yet, Developer waits.

---

## Shared Rules

- **No `unwrap()` in production paths.** Test code may use it.
- **No silent failures.** Every error path either propagates via `Result` or logs via `tracing`.
- **No new dependencies** added to `Cargo.toml` without Rust Maintainer sign-off.
- **Docs are authoritative.** If code contradicts `docs/types.md` or `docs/provider-trait.md`,
  the code is wrong — unless Rust Maintainer has explicitly updated the doc first.
- **Tests are immutable** after hand-off to Developer. Tester may add tests; only Tester edits tests.
- **Exit criteria are binary.** Either all criteria pass or the phase is not complete.

---

## Phase Ownership Summary

| Phase | Rust Maintainer Pre-work | Tester owns | Developer owns | Integration Tester |
|---|---|---|---|---|
| Phase -1 (gaps) | All 4 blocking gaps resolved, docs updated | — | — | — |
| Phase 0 | Workspace structure approved | Test scaffolding, empty test stubs | All crate skeletons, config loader, CLI | `kitsu --help`, `doctor --print-config` |
| Phase 1 | `rustyline` added; `/model` semantics defined | `wiremock` provider tests, `assert_cmd` CLI tests | `OpenAICompatProvider`, streaming render, `ask`, `chat` | Live `kitsu ask` against vLLM |
| Phase 2 | Ollama quirks confirmed | `wiremock` Ollama tests | `OllamaProvider`, `models list` | Live `kitsu ask` against Ollama |
| Phase 3 | `repo_context_max_bytes` added to schema; `rg` fallback decided | Context tool tests, size limit tests | `kitsu-tools` context tools, `diff explain`, `repo summary` | Live `kitsu diff explain` |
| Phase 4 | Tool loop edge cases spec'd | Tool loop tests, audit log tests, text-protocol parser tests | Orchestrator tool loop, policy engine, audit logger | Live tool calling round-trip |
| Phase 5 | SIGPIPE strategy confirmed | Exit code tests, shell completion smoke tests | Hardening, completions, CI | Full regression on both providers |
