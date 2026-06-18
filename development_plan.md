# Development plan

This plan is structured as phases with clear deliverables and integration checkpoints.
Each phase follows the TDD workflow defined in `docs/agent-roles.md`.

Agent abbreviations used below: **TT** = Tester, **DV** = Developer, **RM** = Rust Maintainer, **IT** = Integration Tester.

---

## Dependency decisions (locked)

These are pinned to avoid agents making conflicting choices:

| Concern | Crate | Notes |
|---|---|---|
| Async runtime | `tokio` (full features) | Multi-thread scheduler |
| HTTP client | `reqwest` (with `stream` feature) | Tokio-native, SSE streaming |
| CLI parsing | `clap` v4 (derive API) | |
| Serialization | `serde` + `serde_json` | |
| Config | `toml` crate + `serde` | |
| Async traits | Native async fn in traits (Rust 1.75+) | `async-trait` crate removed — use native. For `Box<dyn Provider>` object safety, use a manual wrapper or `dyn_async_trait` only if needed; evaluate in Phase 0. |
| Streaming | `tokio-stream` + `futures` | `Stream` trait |
| Error types | `thiserror` (libs) + `anyhow` (bin) | |
| Logging / tracing | `tracing` + `tracing-subscriber` | |
| Markdown rendering | `termimad` | Prototype palette conflict in Phase 1; see Phase 1 risk note |
| REPL input | `rustyline` | History, arrow keys, Ctrl+A/E — required for fox-spirit UX |
| Git operations | `git` subprocess | No native dep; simpler |
| Grep | `rg` subprocess | Hard dep; `repo.grep` is unregistered (not available) when `rg` absent |
| HTTP mock (tests) | `wiremock` | For provider unit/integration tests |
| Time | `chrono` | Audit log timestamps (RFC 3339) |
| ANSI color | `owo-colors` | Cross-platform, `IsTerminal`-aware |
| Syntax highlighting | `syntect` | Code block highlighting; feature-flagged (`syntax-highlight`) |
| Terminal width | `terminal_size` | Code block border sizing |
| TTY detection | `std::io::IsTerminal` | Rust 1.70+, no extra dep |

---

## Phase -1 — Design gap resolution (pre-coding)

**Owner**: Rust Maintainer
**Blocks**: Phase 0 start. No code is written until all blocking gaps are closed.

The following issues from `docs/review-feedback.md` must be resolved first. Each item requires
a doc update; no production code is written in this phase.

### Blocking gaps (must close)

**Gap 1 — `--allow-path` implicitly enables tools** *(review item #1)*
- Decision: passing `--allow-path <path>` on the CLI implicitly enables tools for that invocation.
  No separate `--enable-tools` flag is needed.
- `tools.enabled` config key remains as an explicit opt-in for always-on tool use in config.
- Update: `config-schema.md` CLI flags table, `development_plan.md` Phase 0.

**Gap 2 — Text-protocol preamble display** *(review item #2)*
- Decision: preamble text (model reasoning before the tool block) is shown in STONE color as a
  "thinking" line before the tool status glyph. Postamble text (after the tool block) is shown
  as normal assistant output after the tool result.
- Update: `tool-protocol.md` under text-protocol section.

**Gap 3 — `model` as `Option<String>`** *(review item #3)*
- Decision: `provider.model` is `Option<String>` in config. Default is absent (not `""`).
  `--print-config` omits the field if unset. Error at invocation: `config error: provider.model
  is required — set it with --model <name>`.
- Update: `config-schema.md`, `types.md` (Config struct).

**Gap 4 — `auto` fallback contract** *(review item #4)*
- Decision: `ToolsNotSupported` returned from `chat_stream()` is the **only** signal that
  triggers text-protocol fallback in `auto` mode. Any other error (partial stream, connection
  drop, HTTP 5xx) propagates as `ProviderError` — no silent fallback.
- Update: `tool-protocol.md` under the `auto` mode row.

### Design decisions (needed before relevant phases)

**Gap 5 — `/model` mid-session semantics** *(review item #6, needed for Phase 1)*
- Decision: flush active tool loop state, keep conversation history as-is, re-print a one-line
  confirmation (`~ switched to llama3.2:3b`), continue with next user turn.
- Update: `ux-design.md` §4 slash commands table.

**Gap 6 — `repo_context_max_bytes` config key** *(review item #7, needed for Phase 3)*
- Add `repo_context_max_bytes = 32_768` to `[tools]` section of `config-schema.md`.
- Add corresponding `--max-context <bytes>` CLI flag to flags table.

**Gap 7 — `rg` fallback strategy** *(review item #9, needed for Phase 3)*
- Decision: `repo.grep` is **not registered** in the tool registry when `rg` is not found in PATH.
  If the model requests it, the response is a clear `ToolError::ExecError("rg not found in PATH;
  install ripgrep: https://github.com/BurntSushi/ripgrep")`. `kitsu doctor` warns on startup.

**Gap 8 — `tool_calls` JSON field naming** *(review item #8, needed for Phase 5)*
- Decision: rename to `tool_invocations` in `--json` output schema. Add `"source"` field:
  `"source": "model"` for model-requested calls, `"source": "internal"` for orchestrated ones.
- Update: `ux-design.md` §11.

### Minor fixes (resolve as encountered)

- `build_provider` unknown backend: use `unreachable!()` not `panic!()` *(review item #11)*
- `audit_log_path` `~` expansion: document in `config-schema.md` validation rules *(review item #12)*
- Add `--show-context` to CLI flags table as flag-only, default false *(review item #13)*
- Add note to `provider-trait.md` that `ChatRequest` has no `Serialize` derive *(review item #14)*
- Add SIGPIPE to Phase 5 deliverables *(review item #16)*
- Add stdin pipe detection to Phase 1 exit criteria *(review item #17)*

**Exit criteria for Phase -1**:
- All 4 blocking gaps closed with doc updates committed
- `docs/agent-roles.md` reviewed and approved by all agents
- `rustyline` added to `Cargo.toml` workspace dependencies

---

## Phase 0 — Project bootstrap

**TT pre-work**: Write empty test stubs in each crate (`#[test] fn placeholder() {}`) +
  `assert_cmd` smoke test that `kitsu --help` exits 0. Tests must compile and fail (or be ignored)
  before DV touches any `src/`.

**DV deliverables**:

- Cargo workspace `Cargo.toml` with member crates:
  - `crates/kitsu` (binary)
  - `crates/kitsu-core`
  - `crates/kitsu-providers`
  - `crates/kitsu-tools`
  - `crates/kitsu-render`
- All dependency crates added to workspace `Cargo.toml`
- CLI scaffolding with `clap` v4 derive:
  - Subcommands: `chat`, `ask`, `doctor`, `models`, `repo`, `diff` (stubs, no logic)
  - Global flags: `--provider`, `--base-url`, `--model`, `--tool-mode`, `--allow-path`,
    `--json`, `--no-color`, `--no-stream`, `--log-level`, `--show-context`
- Config loader:
  - Parses `~/.kitsu/config.toml` using schema defined in `docs/config-schema.md`
  - `provider.model` is `Option<String>` — absent = error at invocation, not at config load
  - CLI flag overrides applied on top
  - Validation rules enforced at startup (fail fast with actionable message)
  - `~` expansion for `log.audit_log_path` using `dirs::home_dir()` or env `HOME`
  - `kitsu doctor --print-config` prints effective config as TOML
- `tracing-subscriber` initialized from `log.level` config
- `.gitignore` for Rust projects (`target/`, `Cargo.lock` committed for bin)
- `kitsu-render` crate skeleton:
  - `Palette` struct — all color constants, TTY detection, `--no-color` flag propagation
  - `is_tty()` utility cached at startup
  - Stub `render_markdown()`, `render_code_block()`, `Spinner` type
- Bare invocation banner (`kitsu` with no subcommand) with fox-ear motif

**TT testing**:
- `assert_cmd`: `kitsu --help` exits 0 and output contains all subcommands
- `assert_cmd`: `kitsu doctor --print-config` exits 0 and prints valid TOML
- `assert_cmd`: bad config value produces exit code 1 with actionable message
- `assert_cmd`: missing `provider.model` at invocation produces exit code 1 with
  message containing `provider.model is required`
- `tempfile`-based config tests: config loader parses and validates every field
- Unit test: `~` in `audit_log_path` is expanded to absolute path

**RM review gates**:
- `cargo clippy --deny warnings` clean
- `cargo fmt --check` clean
- No `unwrap()`/`expect()` in any `src/` file outside test modules
- `async-trait` crate removed from `Cargo.toml`; native async fn in traits used throughout

**IT exit criteria**:
- `kitsu --help` shows all subcommands and flags
- `kitsu doctor --print-config` prints parsed config on a real machine
- Config validation rejects bad values with clear errors
- `cargo test` passes

---

## Phase 1 — Provider integration: vLLM (OpenAI-compatible)

**Rationale**: OpenAI-compatible API is the most reusable adapter; serves vLLM, LM Studio, and others.

**TT pre-work** (before DV writes any provider code):
- `wiremock` tests for `OpenAICompatProvider`:
  - `test_healthcheck_ok` — mock `GET /v1/models` returns 200 → `healthcheck()` returns `Ok(())`
  - `test_healthcheck_connection_refused` — no server → `ProviderError::Connection`
  - `test_chat_stream_text_response` — SSE stream with two content fragments + stop → collect `"hello world"`
  - `test_chat_stream_tool_calls` — SSE with `tool_calls` finish_reason → `FinishReason::ToolCalls`
  - `test_chat_stream_http_400` — 400 response → `ProviderError::InvalidResponse`
  - `test_chat_stream_http_429_retries` — 429 then 200 → succeeds after retry
  - `test_chat_stream_http_503_retries` — 503 × N then 200 → succeeds within `retries` count
  - `test_chat_stream_timeout` — delayed response beyond timeout → `ProviderError::Timeout`
  - `test_list_models_ok` — returns vec of model names
- `assert_cmd` tests:
  - `kitsu ask "hello"` against wiremock stub → prints response, exits 0
  - `kitsu ask --json "hello"` → stdout is valid JSON matching schema
  - `kitsu ask -` reads from piped stdin → prints response, exits 0
  - `echo "hello" | kitsu ask` → same as above
  - `kitsu doctor` against running mock → exits 0 with green provider line

**DV deliverables**:

- `OpenAICompatProvider` implementing the `Provider` trait:
  - `healthcheck()` via `GET /v1/models`
  - `chat_stream()` via `POST /v1/chat/completions` with SSE parsing
  - `list_models()` via `GET /v1/models`
  - Retry with exponential backoff for 429/503 (up to `provider.retries`)
- `kitsu-render` crate:
  - Streaming terminal output: consume `ChatStream`, print fragments as they arrive
  - `--no-stream` mode: buffer and print all at once
  - `--json` mode: collect full response, emit JSON to stdout
- `kitsu ask <prompt>` functional:
  - Builds single-turn `ChatRequest`, streams response
  - Reads from stdin when argument is `-` or stdin is a pipe (`std::io::IsTerminal`)
  - Supports `--json` output
- `kitsu chat` functional:
  - Interactive REPL loop using `rustyline` (history, arrow keys, Ctrl+A/E)
  - Detects non-TTY stdin → runs one turn and exits (no interactive loop)
  - Maintains in-memory `Vec<ChatMessage>` for the session
  - Ctrl+C cancels current streaming response (does not exit); second Ctrl+C exits
  - Empty input is ignored
  - `/model <name>` slash command: flush tool loop state, keep history, print confirmation
  - `/clear`, `/help` slash commands (stubs acceptable in Phase 1)
- `kitsu doctor`:
  - Checks provider URL reachable
  - Checks model name non-empty (or absent → warns to set `--model`)
  - Checks `rg` binary in PATH (warn if missing — `repo.grep` unavailable)
  - Checks `git` binary in PATH (warn if missing)

**Risk note — `termimad` palette conflict**: prototype `termimad` rendering in this phase with
the fox palette colors. If ANSI sequences clash, either configure `termimad`'s skin or handle
prose as plain text and route only code blocks through `kitsu-render` directly. Document
the outcome in `crates/kitsu-render/NOTES.md`.

**RM review gates**: same as Phase 0 plus verify Ctrl+C cancellation does not leak tasks.

**IT exit criteria**:
- `kitsu ask "hello"` works against a running vLLM server; streaming appears incrementally
- `echo "hello" | kitsu ask` works (stdin pipe)
- `kitsu chat` interactive session works with `rustyline` history
- `kitsu doctor` reports green for a healthy vLLM setup

---

## Phase 2 — Provider integration: Ollama

**TT pre-work** (before DV writes Ollama code):
- `wiremock` tests for `OllamaProvider` (mirror Phase 1 set):
  - `test_healthcheck_ok` — mock `GET /api/version` returns 200
  - `test_healthcheck_connection_refused`
  - `test_chat_stream_text_response` — NDJSON stream, `done: false` then `done: true`
  - `test_chat_stream_done_reason_length` → `FinishReason::Length`
  - `test_chat_stream_tools_not_supported` → `ProviderError::ToolsNotSupported` returned immediately
  - `test_list_models_ok` — `GET /api/tags` returns tag list
  - `test_list_models_empty`

**DV deliverables**:

- `OllamaProvider` implementing the `Provider` trait:
  - `healthcheck()` via `GET /api/version`
  - `chat_stream()` via `POST /api/chat` with NDJSON parsing
  - `list_models()` via `GET /api/tags`
  - Always returns `ProviderError::ToolsNotSupported` if native tools requested
- `kitsu models list` command:
  - Calls `provider.list_models()` and prints results
  - Graceful message if provider returns `None`
- `kitsu doctor` additions:
  - Checks Ollama endpoint and prints version string
  - Suggests `ollama serve` if connection refused

**IT exit criteria**:
- `kitsu ask` and `kitsu chat` work with Ollama
- `kitsu models list` shows available Ollama models

---

## Phase 3 — Context and repo commands

**TT pre-work**:
- Unit tests for each tool in `kitsu-tools`:
  - `fs.read_file`: reads file, enforces `max_file_bytes`, rejects paths outside allowlist
  - `fs.read_file` with line range: returns only requested lines
  - `fs.list_dir`: lists directory entries, fails on path outside allowlist
  - `repo.grep`: parses `rg --json` output; enforces `max_grep_results`; unregistered when `rg` absent
  - `git.diff`: captures subprocess output; enforces `max_file_bytes`
- Context size enforcement unit tests (in `kitsu-core`):
  - Content exceeding `repo_context_max_bytes` is truncated with a warning appended
- `assert_cmd` tests:
  - `kitsu diff explain` with a temp git repo containing a known diff → output contains expected keywords
  - `kitsu repo summary` with a temp repo → exits 0
  - `kitsu diff explain --show-context` → prints gathered context before response

**DV deliverables**:

- `kitsu-tools` crate:
  - `git.diff`: spawn `git diff` / `git diff --cached`, enforce `max_file_bytes`
  - `repo.grep`: spawn `rg --json`, sanitize args, enforce `max_grep_results`;
    **not registered when `rg` absent** — tool slot returns `ExecError` with install hint
  - `fs.read_file`: read file with optional line range, enforce `max_file_bytes`
  - `fs.list_dir`: list directory, depth=1 in MVP, enforce allowlist
  - Each returns `Result<String, ToolError>`
- `kitsu diff explain` command:
  - Gathers `git diff` (staged + unstaged, configurable)
  - Builds prompt: diff content + instruction
  - Sends to provider; streams response
  - `--show-context` flag prints gathered context before sending
- `kitsu repo summary` command:
  - Gathers: directory listing of repo root (depth 2), README if present, key config files
  - Enforces `tools.repo_context_max_bytes` limit; appends truncation warning if hit
  - `--show-context` flag prints gathered context
  - Sends to provider; streams response
- Context size enforcement utility in `kitsu-core`

**IT exit criteria**:
- `kitsu diff explain` summarizes git changes in this repo
- `kitsu repo summary` produces a repo overview
- Context size limits enforced and surfaced as warnings

---

## Phase 4 — Tool calling loop (native + text-protocol)

**TT pre-work**:
- Orchestrator unit tests (using mock `Provider` trait impl):
  - `test_tool_loop_native_single_round` — provider returns `ToolCalls`, tool executes, second round returns `Stop`
  - `test_tool_loop_native_max_rounds` — loop hits 10-round limit, returns error
  - `test_tool_loop_text_protocol_parse_valid` — `<<tool:fs.read_file>>{...}<<end>>` parsed correctly
  - `test_tool_loop_text_protocol_parse_malformed` — malformed JSON in tool block → `ToolError::ExecError`
  - `test_tool_loop_text_protocol_preamble_shown` — preamble text emitted to renderer in STONE color
  - `test_tool_loop_auto_fallback` — provider returns `ToolsNotSupported` → retry with text-protocol
  - `test_tool_loop_auto_no_other_fallback` — HTTP 5xx → propagates `ProviderError`, no fallback
- Policy engine unit tests:
  - `test_policy_path_allowed` — path inside allowlist → permitted
  - `test_policy_path_denied` — path outside allowlist → `PolicyDenied`
  - `test_policy_deny_exec` — `run.shell` always denied in MVP
- Audit log tests:
  - `test_audit_entry_written` — one tool invocation → one JSON line in temp log file
  - `test_audit_entry_denied` — denied tool → status `"denied"` in log
  - `test_audit_entry_elapsed` — `elapsed_ms` > 0 for real tool call

**DV deliverables**:

- `kitsu-tools` runtime additions:
  - Tool registry: `register(ToolDescriptor, HandlerFn)`
  - Policy engine: validate path allowlist, enforce `deny_exec`, apply size/timeout limits
  - Audit logger: write `AuditEntry` JSON Lines to `log.audit_log_path`; expand `~` at load time
  - `fs.list_dir` registered as built-in
- `kitsu-core` orchestrator tool loop:
  - Native mode: send `tools`, parse `tool_calls` from stream
  - Text-protocol mode: buffer response, parse `<<tool:NAME>>{json}<<end>>`;
    show preamble in STONE color before tool status glyph
  - `auto` mode: attempt native; on `ToolsNotSupported` only → retry as text-protocol
  - Max 10 rounds per user turn
  - System prompt injection for text-protocol mode
- `kitsu chat` uses tool loop when `tools.enabled = true` or `--allow-path` provided and `allowed_paths` non-empty
- `kitsu ask` uses tool loop (single turn; multi-round tool calls within that turn)

**IT exit criteria**:
- Model can request `fs.read_file` and `repo.grep` during `kitsu chat`
- Tool calls blocked when outside `allowed_paths` → audit log shows `"denied"`
- Audit log written for every tool invocation
- `auto` mode falls back to text-protocol with Ollama

---

## Phase 5 — Hardening and packaging

**TT pre-work**:
- Exit code tests (`assert_cmd`):
  - bad args → exit 1
  - provider unreachable → exit 2
  - tool execution error → exit 3
  - success → exit 0
- SIGPIPE test: pipe `kitsu ask` output into `head -1` → no panic, clean exit
- Shell completion smoke tests: `kitsu completions bash` → stdout is non-empty, valid bash
- Stdin pipe test already in Phase 1 (verify still passes)

**DV deliverables**:

- Exit codes:
  - `0` — success
  - `1` — user/usage error (bad args, config error)
  - `2` — provider error
  - `3` — tool execution error
- SIGPIPE: write layer uses `.ok()` pattern on all write calls; process exits cleanly on `EPIPE`
- Robust error messages throughout; all errors suggest `kitsu doctor` where relevant
- `--json` output mode stabilized:
  - `kitsu ask --json`: `{"response": "...", "model": "...", "tool_invocations": [...], "tokens": {...}}`
  - `"source": "model"` vs `"source": "internal"` on each invocation
  - `kitsu diff explain --json`, `kitsu repo summary --json`
- Shell completions via `clap_complete`:
  - bash, zsh, fish
  - `kitsu completions <shell>` command
- Documentation:
  - `docs/quickstart.md`: install, configure, first use; `rg` listed as hard dependency
  - `docs/tools-guide.md`: how to enable tools, allowlists, audit log
- Release artifacts:
  - `cargo install kitsu-cli` works
  - GitHub Actions CI: `cargo test`, `cargo clippy --deny warnings`, `cargo fmt --check` on push

**IT exit criteria**:
- Stable exit codes across all error scenarios
- Shell completions work for bash/zsh
- `cargo install` installs a working binary
- SIGPIPE: `kitsu ask "x" | head -1` exits without panic

---

## Phase 6 — Post-MVP extensions

These are out of scope for MVP. No agent should begin these until Phase 5 is signed off.

- Session persistence (`~/.kitsu/sessions/`, `kitsu session list|resume|delete`)
- Plugin system for custom tools (signed/allowlisted)
- MCP client adapter
- Embeddings + lightweight local RAG index
- `ratatui`-based TUI for context browsing
- Windows support
- Streaming text-protocol (partial buffering with regex scan)
- Per-repo config (`.kitsu/config.toml`)
- Environment variable config overrides

---

## Integration notes

### vLLM
- Server mode: `vllm serve <model> --port 8000`
- Endpoint: `http://localhost:8000/v1/chat/completions`
- Streaming: SSE with `data:` prefix, terminated by `data: [DONE]`

### Ollama
- Server: `ollama serve` (default port 11434)
- Streaming: NDJSON (`\n` separated), `"done": true` signals end
- Tool calling: not reliably supported; always use text-protocol fallback
