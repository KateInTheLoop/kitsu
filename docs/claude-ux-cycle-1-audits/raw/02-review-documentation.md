# Cycle 1 lane B: verified code/documentation review UX

- Date: 2026-08-10/11 (America/Los_Angeles)
- Target: `57889b2a096993d5fcdd20d95abb2dd13af9e0ef` versus its parent
- Products: Claude Code 2.1.227 and the branch-local debug build of Kitsu

## Executive result

Kitsu has the clearer default trust boundary for a one-shot review: `review --commit` resolves the revision on the host, excludes private Kitsu state, prepares a bounded Git diff, and gives the model zero tools. Its help explains those guarantees in one place. Claude Code has the stronger general automation surface: explicit tool allow/deny lists, permission modes, session-persistence control, text/JSON/stream-JSON output, JSON Schema validation, effort selection, and a hard USD budget.

The largest Kitsu gap is review completeness. This 25-file, 7,172-addition commit exceeded Kitsu's 256 KiB prefix cap. Kitsu labeled the evidence `truncated`, but neither supplied an omitted-file/range manifest nor reviewed the remaining chunks. Files late in Git's ordering were therefore outside the model's evidence. The second-largest gap is automation hygiene: `--jsonl review` copied that entire bounded prompt—including the diff—into `turn_started.input`. The observed failed run emitted roughly 68k tool-output tokens before a provider response existed.

No model completed the matched review within the available harness window. Accordingly, model finding quality, false-positive rate, and source-link accuracy are **not scored**, and this audit cannot responsibly certify that the target commit has no release blocker. The results below compare verified CLI behavior, help, source, tests, and failure UX only.

## Safety and method

- Both disposable repositories were detached at the exact target commit and were clean before each attempt.
- Claude was restricted to `Read`, `Grep`, `Glob`, and Git read commands; edit/write/web tools were denied, permissions used `dontAsk`, and session persistence was disabled.
- Kitsu used the top-level host-verified `review --commit` path. The observed `provider_request` reported `tools_offered: 0`; source also constructs an empty tool registry for this mode.
- No credentials, auth files, or config contents were inspected or recorded. Config paths below are not credential material.
- Initial `/tmp` clones/config disappeared during an overnight environment refresh. Identical local clones were recreated from the shared repository; neither clone was modified or committed.

## Exact sanitized commands and outcomes

### Discoverability

```sh
claude --version
claude --help
```

Outcome: Claude Code 2.1.227. Top-level help immediately says `-p/--print` is noninteractive and exposes `--output-format text|json|stream-json`, `--json-schema`, `--max-budget-usd`, `--effort`, `--tools`, `--allowedTools`, `--disallowedTools`, `--permission-mode`, `--no-session-persistence`, partial streaming, and cancellation-adjacent/background controls. It also discovers `ultrareview` as a cloud multi-agent review command. The `-p` help unusually and usefully warns that noninteractive mode skips the workspace-trust dialog and that invalid settings can be silently ignored.

```sh
/home/mmpg/workspace/kitsu/kitsu/target/debug/kitsu --help
/home/mmpg/workspace/kitsu/kitsu/target/debug/kitsu review --help
```

Outcome: Kitsu puts `review` in the seven-command top-level menu and labels it “Review host-verified repository evidence in a tool-free model turn.” Review help gives working/staged/unstaged/base/commit scopes, path filtering, instructions, JSONL, plain/reduced-motion modes, session options, examples, and an explicit final trust statement: exactly one scope, private state excluded, revisions validated, no model tools. This is less setup to discover than constructing a safe generic Claude prompt/tool policy.

### Matched whole-commit review

```sh
claude -p \
  'Review the latest Kitsu cycle commit for correctness, security, automation truthfulness, documentation consistency, and developer UX. Return only evidence-backed findings ordered by severity with exact file/line references; explicitly say when no release blocker is found. Do not modify files.' \
  --model haiku --effort low --max-budget-usd 0.40 \
  --permission-mode dontAsk \
  --tools 'Read,Grep,Glob,Bash' \
  --allowedTools 'Read,Grep,Glob,Bash(git diff *),Bash(git show *),Bash(git status *),Bash(git rev-parse *)' \
  --disallowedTools 'Edit,Write,NotebookEdit,WebFetch,WebSearch' \
  --no-session-persistence --output-format json
```

Restricted-sandbox outcome: after 180.6s, exit 1 with a valid single JSON result, `terminal_reason: "api_error"`, `result: "API Error: Unable to connect to API (ENOTIMP)"`, zero turns, zero tokens, and `$0` cost. The failure was harness network confinement, not a product review result. Escalated retries were stopped after the execution/bridge queue exceeded the audit timebox; no Claude finding text was produced and no cost was incurred in the recorded attempt.

```sh
/home/mmpg/workspace/kitsu/kitsu/target/debug/kitsu \
  --config /tmp/kitsu-claude-bridge.toml \
  --jsonl --plain --reasoning-effort low \
  --timeout-secs 300 --max-output-tokens 8000 \
  review --commit 57889b2a096993d5fcdd20d95abb2dd13af9e0ef \
  --instructions 'Review the latest Kitsu cycle commit for correctness, security, automation truthfulness, documentation consistency, and developer UX. Return only evidence-backed findings ordered by severity with exact file/line references; explicitly say when no release blocker is found. Do not modify files.'
```

Restricted-sandbox outcome: exit 2 in 0.174s after localhost connection failure. Even on failure, Kitsu emitted a coherent 12-event transaction: `run_started`; `turn_started`; `provider_request` (`tools_offered: 0`); a terminal connection `provider_attempt`; discarded `assistant_disposition`; typed error; error `turn_finished`; and error `final_summary` with `provider_failures: 1`. This is excellent failure truthfulness.

However, `turn_started.input` contained the complete generated review prompt and all available diff evidence. The host capture reported about 68,212 output tokens despite there being no model answer. Its header truthfully identified the exact requested/resolved commit and `truncated`, and its tail ended with `...[diff truncated at 256 KiB]`, but it did not enumerate omitted files or line ranges.

### Bounded fallback

The same prompt was retried in both products with the shared path filter `crates/kitsu/src/main.rs`. Kitsu used:

```sh
/home/mmpg/workspace/kitsu/kitsu/target/debug/kitsu \
  --config /home/mmpg/workspace/kitsu/kitsu/tests/dogfood/results/claude-cycle-harness/kitsu.toml \
  --plain --no-stream --reasoning-effort low \
  --timeout-secs 300 --max-output-tokens 8000 \
  review --commit 57889b2a096993d5fcdd20d95abb2dd13af9e0ef \
  crates/kitsu/src/main.rs \
  --instructions '<the exact matched review prompt above>'
```

The bridge took about 27 minutes even for the harness's trivial smoke request. Both bounded product calls were therefore cancelled by the audit controller without usable assistant output. This is recorded as **comparison unavailable due to harness/bridge latency**, not as a Kitsu or Claude latency finding. Cancellation happened outside the product processes, so product-native Ctrl+C semantics were not evaluated.

## CLI UX comparison

| Dimension | Claude Code | Kitsu | Assessment |
|---|---|---|---|
| Evidence preparation | Agent can inspect the complete commit/repository using narrowly allowed read/search/Git tools. | Host resolves the revision and supplies immutable, tool-free Git evidence, but keeps only the first 256 KiB. | Kitsu wins trust; Claude wins reachable coverage for large commits. |
| Trust-boundary clarity | Powerful but user-assembled: tool inventory, allow/deny patterns, permission mode, and a warning that `-p` skips workspace trust. | Dedicated command states verified scope, private-state exclusion, revision validation, and no tools; runtime confirmed zero offered tools. | Kitsu materially better by default. |
| Setup friction | One authenticated command, but a safe review requires several permission flags; `ultrareview` is discoverable but is a separate cloud workflow. | One dedicated subcommand after gateway config; concise examples cover common scopes. | Kitsu better once configured; Claude has simpler hosted auth. |
| Streaming/progress | Text/JSON/stream-JSON and partial-message modes are explicit; single JSON intentionally waits for completion. | JSONL emitted run/turn/provider lifecycle immediately and human mode announces verified scope. The enormous `turn_started` record obscures that progress. | Both capable; Kitsu needs compact events. |
| Result structure | Optional JSON Schema can require a stable finding shape; result JSON includes usage and USD cost. | JSONL lifecycle is strong, but the actual review remains one unvalidated assistant string. | Claude ahead. |
| Source linking | Prompt can require references, but they remain model-authored unless a consumer validates them. | Same; Kitsu knows the evidence but does not validate or type returned path/line references. | Tie, with a Kitsu opportunity. |
| Context/provenance | Tool trace can show what was read; JSON result reports session/model/usage. Exact commit provenance must be established through prompt/tools. | Exact target and resolved SHA are host-owned and visible; truncation is disclosed. Coverage/omission provenance is missing. | Kitsu ahead on identity, behind on completeness. |
| Cancellation | Not exercised. Help exposes background/session controls but not a review-specific cancellation contract. | Not exercised. Documentation promises structured cancellation and final summary. | Unscored. |
| Automation usability | Hard spend cap, effort, no persistence, JSON/stream-JSON, schema validation, and cost metadata. | Stable versioned event transaction and typed failures, but no hard cost cap, finding schema, or compact input policy. | Claude ahead overall. |

## Source-backed observations

1. The evidence cap is a literal 256 KiB prefix plus a marker (`crates/kitsu/src/workspace.rs:16-29`). Commit evidence runs `git show` once, then applies `cap_diff`; it does not generate a coverage manifest or subsequent chunks (`crates/kitsu/src/workspace.rs:741-780`).
2. The verified prompt includes the bounded diff verbatim (`crates/kitsu/src/main.rs:3023-3028`). JSONL then includes that full prompt verbatim again as `turn_started.input` (`crates/kitsu/src/main.rs:7892-7896`). The v1 schema requires an unbounded string for that field (`docs/schemas/kitsu-chat-v1.schema.json:191-200`).
3. The dedicated review runner grants only a read root, supplies no write roots, disables commands, and uses an initial review turn (`crates/kitsu/src/main.rs:384-414`). The prompt is marked with `TurnToolMode::None`, which is covered by a unit test (`crates/kitsu/src/main.rs:10771-10794`).
4. Help's trust claims are concrete and discoverable (`crates/kitsu/src/cli.rs:187-220`). The live failure corroborated `tools_offered: 0` and emitted a discarded assistant disposition rather than presenting partial output as success.
5. Existing truncation coverage only asserts that a huge diff is bounded and contains “diff truncated”; it does not assert file/range coverage metadata or a fail/continue policy (`crates/kitsu/src/workspace.rs:1209-1216`).

## Ranked Kitsu gaps and regression proposals

### P0 — Review every changed file, or fail closed on incomplete evidence

Current prefix truncation can yield a polished “no blocker” answer without showing the model later files. Build a host-owned manifest first (path, status, hunk count, byte count), split evidence on file/hunk boundaries, review all chunks, then run a tool-free synthesis over typed chunk findings. If the configured context/cost ceiling cannot cover every chunk, exit nonzero unless the user explicitly accepts partial review.

Regression: create a commit larger than 256 KiB with a sentinel defect only in the lexicographically last file. Assert the last file is reviewed or the command ends with typed `incomplete_evidence`; it must never emit an ordinary successful review.

### P0 — Stop echoing full review evidence in default JSONL

`turn_started.input` duplicates up to 256 KiB of source in automation logs, creates giant first records, and broadens data retention beyond the provider request. For review mode, emit user instructions plus host-owned metadata (`evidence_id`, SHA-256, bytes, files, truncated/complete), with full evidence only behind an explicit debug/unsafe flag. Introduce a v2 event or optional redacted representation rather than silently changing v1 semantics.

Regression: review a diff containing a unique canary. Default JSONL must contain the evidence hash/size but not the canary; an explicit include-input mode may contain it. Cap every event's serialized size.

### P1 — Add typed, schema-validated findings

Kitsu wraps lifecycle events well but leaves the valuable payload as free-form text. Add `--output-format findings-json` (or a built-in review schema) with severity, title, path, start/end line, rationale, evidence excerpt/hash, and blocker boolean. Reject or separately report invalid model output; retain human rendering as a view over the same structure.

Regression: mock valid, malformed, missing-line, and unknown-severity responses. Assert stable typed findings, deterministic nonzero validation failures, and exact human/JSON parity.

### P1 — Host-validate every source reference

Kitsu already owns the exact patch and can verify whether a cited path and line/hunk were present. Mark findings `verified_reference: true|false`; do not let out-of-evidence citations masquerade as evidence-backed results. This directly reduces false-positive review noise.

Regression: inject one citation inside a supplied hunk, one to an omitted hunk, and one nonexistent path. Only the first may be called verified.

### P1 — Add hard spend budgets and cost/usage closure

Claude's `--max-budget-usd` makes unattended review safer, and its final JSON reports cost. Kitsu exposes output tokens and deadlines but not spend. Add an optional gateway pricing table and `--max-cost-usd`; always put prompt/completion/cache tokens in `final_summary`, and include cost when pricing is known. For unknown/local pricing, say `cost: null, reason: pricing_unknown` rather than guessing.

Regression: a mock multi-chunk review crosses a tiny budget between chunks, stops before the next send, and ends `budget_exhausted` with exact usage and incomplete-evidence state.

### P1 — Emit a first-class evidence-prepared event

Do not bury provenance inside a model prompt. Emit resolved target/base/parent, complete/truncated state, included and omitted files/hunks, byte/token estimates, excluded private paths count (not names), and evidence hash before inference. Add `review --evidence-only --json` so users and CI can inspect the boundary without contacting a provider.

Regression: snapshot human, plain, and JSONL provenance for working/staged/base/commit/path-filter scopes and prove no protected path or content appears.

### P2 — Make progress stageful and compact

Human review should distinguish “preparing verified evidence,” “reviewing chunk 2/6,” and “synthesizing findings,” with elapsed time and an immediate hint when coverage is partial. JSONL should carry the same stages as small events. Styled mode can use a restrained kitsune trail/tail motif; `--plain` remains ASCII and `--reduced-motion` static.

Regression: golden-terminal snapshots for styled/plain/reduced-motion modes plus an event-order test covering prepare, chunks, synthesis, and completion/cancellation.

### P2 — Make review cancellation a dedicated contract

Exercise cancellation during Git evidence preparation, provider streaming, between chunks, and synthesis. Report which evidence/chunks completed, discard provisional assistant text, and close with exactly one cancelled `final_summary`. A cancelled partial review must not persist as a successful resumable review.

Regression: deterministic SIGINT injection at every stage, asserting bounded exit time, no orphan process, no committed provisional finding, and truthful session state.

### P2 — Offer safe review presets without flag archaeology

Add documented presets such as `--strict` (complete evidence required, typed findings, validated references) and `--ci` (plain JSONL, no session, hard incomplete-review failure). Keep the explicit flags discoverable in `review --help` and print the expanded policy in `run_started`.

Regression: help/snapshot tests ensure presets expand to stable, visible policy and never enable tools, writes, commands, or persistence.

## Maintainer recommendation

Treat P0 evidence completeness and JSONL input minimization as release-quality gaps in the review UX, not cosmetic backlog. They affect whether automation can trust a successful review and whether source content is unnecessarily replicated into logs. Implement those first with tests. The typed findings/reference validation and cost budget are the next highest-value parity work. Fox/Japanese-folklore styling should land only as a human-mode layer over the same truthful stage model, with plain, reduced-motion, and JSONL behavior kept deterministic.
