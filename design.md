# Design

## 0. Companion documents

This document describes the architecture. Detailed specifications are in:

- `docs/config-schema.md` — full TOML config schema with defaults and validation rules
- `docs/types.md` — concrete Rust type definitions (`ChatMessage`, `ToolCall`, `ChatDelta`, etc.)
- `docs/provider-trait.md` — Provider trait signature and per-provider endpoint mapping
- `docs/tool-protocol.md` — tool calling modes, text-protocol format, orchestrator loop, built-in tool specs
- `docs/ux-design.md` — color palette, glyphs, code block rendering, personality, competitive design

---

## 1. Overview

`kitsu-cli` is a Rust CLI that routes user prompts to a local LLM provider (Ollama or vLLM) and optionally augments the prompt with local context via safe tools (filesystem, git, grep). It is designed around a small number of stable interfaces:

- **Provider**: “how we talk to a model runtime”
- **Orchestrator**: “how we run the chat/tool loop”
- **Tool runtime**: “how we expose safe local capabilities”
- **Renderer**: “how we display responses”

## 2. High-level architecture

```
+------------------+         +-----------------------+
|   CLI (kitsu)    |  calls  |     kitsu-core         |
| clap + config    +-------->+ orchestrator + sessions|
+------------------+         +----+------------------+
                                  |
                                  | uses
                                  v
                           +------+------------------+
                           |   kitsu-providers       |
                           | Provider trait + impls  |
                           +------+------------------+
                                  |
                                  | HTTP
                                  v
                     +------------+--------------+
                     | Ollama REST | vLLM /v1/*  |
                     +------------+--------------+

(Optional tool calling loop)

+------------------+    executes     +------------------+
|  kitsu-tools     +<--------------->+  local system     |
| tool registry    |                 | fs/git/rg/exec    |
+------------------+                 +------------------+
```

## 3. Cargo workspace structure

Recommended workspace:

- `crates/kitsu` — binary crate (CLI entry)
- `crates/kitsu-core` — orchestrator, sessions, prompt building, tool loop
- `crates/kitsu-providers` — provider trait + `OllamaProvider`, `OpenAICompatProvider`
- `crates/kitsu-tools` — tool registry, policy engine, built-in tools
- `crates/kitsu-render` — terminal rendering + JSON output formatting. Its
  writers sit behind a `Surface` seam (`surface.rs`): `ClassicSurface` is
  today's behaviour, and a second backend can be installed without touching
  callers. The accessor returns a cloned `Arc` out of a short-lived read
  guard, so a caller structurally cannot hold the lock across a call — which
  matters because the live-row suspension is reentrant, notices nest inside
  it, and approval prompters run synchronously under `spawn_blocking`.
  `write_block` carries a multi-line block under **one** suspension, because
  lifting the row per line would clear and repaint between lines and change
  the emitted bytes; the backend decides whether to lift at all, since the row
  lives on stderr and a redirected stdout cannot disturb it. `StreamBuffer`
  takes the block's writer rather than owning an `io::stdout()` handle, so the
  seam genuinely carries assistant text and a second backend receives it; the
  remaining caveats are that a failed write is swallowed, as it was against
  stdout, and the returned byte count is computed from the formatted text
  rather than from what the writer accepted.
- `crates/kitsu-editor` — the pure input keymap and line editor (printable
  insert, motions, kills, history walk with a draft stash, reverse-i-search,
  bracketed paste, lone-Escape resolution). Dependency-free and deliberately
  a library rather than a module of the binary: `crates/kitsu` is binary-only,
  so a library crate cannot import from it, and the planned terminal UI needs
  the same editor the binary's input collector uses.
- `crates/kitsu-tui` — the terminal UI: pinned `ratatui 0.29` plus
  `crossterm 0.28` (0.30 of ratatui requires a newer Rust than this workspace
  declares). Currently carries only the terminal lifecycle — raw mode, window
  title push and pop, and restoration on drop, interrupt and panic — because
  giving the terminal back on every exit path has to be right before anything
  is drawn on it. `viewport.rs` adds `InlineSurface`, a `Surface` backend that
  pins a bottom region with `DECSTBM` and paints its rows by absolute
  positioning while transcript lines flow upward. Its sinks are injectable per
  descriptor so the whole mechanism is testable in process, and it emits CRLF
  rather than LF: it lives behind raw mode, which clears `ONLCR`, so each
  backend supplies its own line ending.

This separation keeps provider logic independent from CLI parsing and makes it easier to add:
- new providers (e.g., llama.cpp server)
- new tools (e.g., tree-sitter navigation)
- future MCP integration

## 4. Data model

### 4.1 Messages

Internal message type (provider-agnostic):

- `role`: `system | user | assistant | tool`
- `content`: text
- `name`: optional (for tools)
- `tool_calls`: optional (native mode)

Providers translate internal messages into their required JSON schema.

### 4.2 Tool calls

A tool call has:

- `tool_name`
- `args` (JSON)
- `call_id` (string, used to correlate request/response)

Tool results are serialized as JSON and appended to the conversation as a `tool` role message.

## 5. Provider adapters

### 5.1 Provider trait

The provider interface should support:

- `healthcheck()`
- `chat(request, stream)`
- (optional) `list_models()`

A `ChatRequest` contains:

- `model`
- `messages`
- `tools` (optional, for native tool calling)
- `temperature`, `top_p`, `max_tokens` (optional)

### 5.2 Ollama integration

**Transport**: HTTP requests to Ollama local daemon.

Key behaviors:

- Use Ollama chat endpoints (or generate endpoints if needed).
- Streaming: read incremental chunks and forward to renderer.
- Tool calling:
  - Ollama support varies by model and configuration.
  - Prefer **text tool protocol** as a reliable fallback.

### 5.3 vLLM integration

**Transport**: OpenAI-compatible HTTP API.

- Endpoint: `/v1/chat/completions`
- Streaming: SSE/chunked; parse incremental deltas
- Tool calling:
  - Where supported, pass `tools` in OpenAI format and parse returned `tool_calls`.

## 6. Tool calling modes

### 6.1 Native mode (OpenAI-style tools)

If provider supports:

- request field `tools: [{type:"function", function:{name,description,parameters}}]`
- response contains `tool_calls`

Then `kitsu-core`:

1. Sends request with `tools`.
2. On `tool_calls`, validates tool + args.
3. Executes tool(s) in `kitsu-tools` runtime.
4. Appends `tool` messages with results.
5. Continues until no more tool calls.

### 6.2 Text tool protocol (fallback)

A strict protocol that any model can follow. Example format:

```
<<tool:fs.read_file>>
{"path":"src/main.rs","range":{"start":1,"end":120}}
<<end>>
```

Rules:

- Tool block must be standalone and parseable.
- JSON must be strict (no trailing commas).
- Only one tool block per assistant turn in MVP (later: allow multiple).

`kitsu-core` parses the assistant message:

- If valid tool request → run tool → append `tool` result → continue.
- Otherwise treat response as final answer.

## 7. Tool runtime and safety

### 7.1 Policy engine

Policy should be enforced centrally:

- Allowed read paths: explicit allowlist
- Allowed execution: disabled by default
- Limits:
  - max bytes returned by tools
  - max results from grep
  - timeouts per tool

### 7.2 Built-in tools

MVP tools:

- `fs.read_file`
  - validate path inside allowlist
  - support line range; enforce size limits

- `fs.list_dir`
  - list directory contents with optional depth (depth=1 in MVP)

- `repo.grep`
  - wrap `rg` for speed
  - sanitize arguments; enforce max results

- `git.diff`
  - either use `gix` or `git diff` subprocess
  - enforce size limits

### 7.3 Audit log

Log schema (JSON Lines suggested):

- `ts`
- `tool`
- `args` (optional redaction)
- `status`
- `elapsed_ms`
- `error` (if any)

## 8. Rendering and UX

Full UX specification is in `docs/ux-design.md`. Summary of principles:

- **Personality**: kitsu is a fox-spirit companion — quick, precise, charming. Not a chatbot dump.
- **Color palette**: fox orange (`FOX`), amber, cream, teal, sage, ember — all defined in `kitsu-render`.
  Disabled automatically when stdout is not a TTY.
- **Streaming**: native mode prints tokens live; text-protocol mode shows an animated spinner during
  buffering, then renders the full response.
- **Code blocks**: rounded box borders with language tag and optional syntax highlighting (`syntect`).
  In non-TTY mode: plain fenced markdown — maximally copy-paste friendly.
- **Tool status**: inline diamond glyphs (`◆ ◇ ◈ ◉`) replace verbose log lines; timing shown.
- **`--json` mode**: all decoration suppressed; stable JSON schema to stdout; errors to stderr.
- **Non-TTY / pipe mode**: no ANSI, no spinners, no box-drawing — clean plain text throughout.

## 9. Error handling strategy

- Provider errors:
  - timeouts, connection refused, invalid JSON
  - show actionable messages and suggest `kitsu doctor`

- Tool errors:
  - denied by policy
  - file not found
  - grep failure
  - include enough detail without leaking sensitive paths unintentionally

## 10. Async and concurrency model

- **Runtime**: `tokio` multi-thread scheduler, initialized in `crates/kitsu/main.rs`
- **Provider HTTP**: `reqwest` async client; streaming responses use `bytes_stream()` → line/SSE parser
- **Signal handling**: `tokio::signal::ctrl_c()` future races against the streaming future; cancellation via `tokio::select!` with request abort handle
- **Tool execution**: synchronous subprocess via `std::process::Command` inside `tokio::task::spawn_blocking`; timeout via `tokio::time::timeout`

## 11. Extensibility

Planned extensions:

- plugin system for new tools (signed/allowlisted)
- MCP client/server adapter
- embeddings + lightweight RAG index (optional)
- richer TUI (ratatui) for browsing contexts
