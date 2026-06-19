# Kitsu — Supported Functionality

> Status as of 2026-06-19. This documents what the **MVP actually does today**,
> verified against the live AI gateway. Planned-but-not-built features are listed
> separately under [Not yet supported](#not-yet-supported) so the line is clear.

Kitsu is a multi-gateway model CLI: **one binary, many models**. You point it at a gateway
profile and a model id, and it streams the completion. The same binary talks to Mistral,
Claude, and any OpenAI-compatible endpoint with no code changes — only config/flags differ.

---

## Commands

| Command | What it does | Status |
|---|---|---|
| `kitsu ask <prompt>` | Single-turn question; streams the answer to stdout | ✅ Working |
| `kitsu chat` | Interactive REPL (multi-turn, slash commands, optional tools) | ✅ Working |
| `kitsu --help` / `kitsu <cmd> --help` | Usage | ✅ Working |
| `kitsu --version` | Version | ✅ Working |

## Global flags

| Flag | Meaning | Default |
|---|---|---|
| `--gateway <name>` | Which `[gateways.<name>]` profile to use | `default_gateway` from config |
| `--model <id>` | Override the model id (beats the gateway's default) | gateway's `model` |
| `--config <path>` | Config file location | `$KITSU_CONFIG`, then `~/.kitsu/config.toml` |
| `--no-stream` | Request a single non-streamed completion | streaming on |
| `--temperature <f>` | Sampling temperature. **Omitted from the request unless set** | unset (not sent) |
| `--allow-path <p>` | (`chat`) Allow read-only fs tools under this path (repeatable) | none (tools disabled) |

> **Why `--temperature` is off by default:** newer Anthropic models on Bedrock reject a
> `temperature` field (`HTTP 400: temperature is deprecated for this model`). Omitting it by
> default keeps kitsu compatible across all models; pass `--temperature 0.2` when a model accepts it.

---

## Interactive chat (`kitsu chat`)

A line-based REPL that prints to the terminal's normal scrollback, so **copy-paste and
scroll-wheel work natively** (no alt-screen TUI). Powered by `rustyline`: history (persisted to
`~/.kitsu/history`), arrow keys, Ctrl+A/E/W, and bracketed multi-line paste.

- **Multi-turn**: conversation history is retained across turns.
- **Slash commands**: `/model <name>` (switch model, keep history), `/clear` (reset to system
  prompt), `/help`, `/quit`.
- **Ctrl+C** cancels the in-flight streaming response and returns to the prompt; **Ctrl+D** (or
  Ctrl+C at an empty prompt) exits.
- **Non-TTY stdin** (`echo "hi" | kitsu chat`) runs a single turn and exits — same shape as `ask`.
- **Tools**: pass `--allow-path <dir>` to enable read-only filesystem tools (`fs_read_file`,
  `fs_list_dir`, and `repo_grep` if `rg` is installed). The model can call them during a turn;
  every access is checked against the allowlist (deny-by-default, `..`/symlink-safe). Tool names
  use underscores (not dots) so they pass Bedrock's `[a-zA-Z0-9_-]+` constraint across all gateways.

---

## Gateways & models

### How model routing works

A **gateway profile** bundles `{ base_url, chat_path, model, flavor, auth }`. Because the example
gateway exposes one OpenAI-compatible route (`/v1/chat/completions`) for *all* its
models, switching models is just `--model <id>` against the same `example` profile — no new profile,
no code change.

### Verified working models (via gateway)

| Model id | Provider | Tier | Verified |
|---|---|---|---|
| `example-model-v1` | Mistral (Bedrock) | high | ✅ chat + code-gen |
| `claude-sonnet-4-6` | Anthropic (Bedrock) | med | ✅ chat |
| `claude-opus-4-8` | Anthropic (Bedrock) | high | ✅ chat + code-gen (use without `--temperature`) |

Other Anthropic ids on the same route (`claude-opus-4-7`, `claude-haiku-4-5-*`, etc.) should work
identically — they share the `/v1/chat/completions` endpoint.

### Provider flavors

| Flavor | Wire shape | Status |
|---|---|---|
| `openai` | OpenAI chat/completions — covers OpenAI, vLLM, Mistral, **gateway, Anthropic-on-Bedrock** | ✅ Implemented |
| `anthropic` | Anthropic-native `/v1/messages` (top-level system, content blocks) | ⏳ Accepted in config, **not implemented** (not needed — Anthropic works via `openai` flavor on the gateway) |
| `ollama` | Ollama NDJSON | ⏳ Config-accepted, not implemented |

---

## Authentication

| Method (`auth.method`) | Behavior | Status |
|---|---|---|
| `bearer-env` | Reads token from env var (`auth.env`, default `ANTHROPIC_AUTH_TOKEN`); sends `Authorization: Bearer <token>` | ✅ Working |
| `static` | Inline token in config (discouraged; redacted in output) | ✅ Implemented |
| `none` | No auth header (local vLLM/Ollama) | ✅ Implemented |

Security & expiry behavior:
- The token is read from the env var **at request time**, marked sensitive, and **never logged,
  persisted, or printed** in errors.
- **Expiry**: read from `ANTHROPIC_TOKEN_EXPIRES` (RFC3339) only — no JWT decoding. If expired,
  kitsu prints a stderr warning and **sends anyway** (the gateway's real 401 is authoritative).
- **Missing token** → exit 1 with: `auth error: $ANTHROPIC_AUTH_TOKEN is not set — run refresh-token …`

---

## The agent loop

`kitsu ask` runs one user turn through the orchestrator loop (`kitsu-core::run_agent_turn`):

1. Send messages to the model.
2. If the model emits tool calls → execute each via the tool `Registry`, append results, repeat.
3. Stop when the model returns a final answer, or a guard trips.

Guards / behavior:
- **`max_rounds` cap** (default 10) — hard backstop against infinite tool loops.
- **Context budget** (default 128 KiB) — oldest tool-result messages truncated first; system
  prompt and latest user message are never dropped.
- **Error-as-tool-result** — a failing tool is serialized back to the model as a tool message so
  it can recover, rather than aborting the run.

> **Today's limit:** the tool `Registry` is empty and tool *descriptors are not advertised* to the
> model, so in practice `ask` runs as a single round (text in → text out). The multi-round loop
> machinery exists and is tested, but there are no built-in tools wired in yet (see below).

---

## Output & exit codes

- Streaming output prints tokens to stdout as they arrive; `--no-stream` buffers and prints once.
- Errors go to stderr; the token is never echoed.

| Exit code | Meaning |
|---|---|
| `0` | Success |
| `1` | User/config/auth error (bad args, unknown gateway, missing token) |
| `2` | Provider error (gateway 401/403, HTTP error, timeout) |

---

## Examples

```bash
# Mistral via the gateway (uses default_gateway = "example")
kitsu ask --config ./kitsu.toml --gateway example "Explain a mutex in one sentence."

# Claude Opus 4.8 — same gateway, just a different model (no --temperature!)
kitsu ask --config ./kitsu.toml --gateway example \
  --model claude-opus-4-8 \
  "Write a Python prime checker. Raw code only."

# Capture generated code to a file
kitsu ask --gateway example --model claude-sonnet-4-6 \
  "Write a bash script that backs up ~/notes. Raw code only." > /tmp/backup.sh
```

Minimal config (`kitsu.toml`):

```toml
default_gateway = "example"

[gateways.example]
flavor      = "openai"
base_url    = "https://gateway.example.com"
chat_path   = "/v1/chat/completions"
models_path = ""
model       = "example-model-v1"
auth = { method = "bearer-env", env = "ANTHROPIC_AUTH_TOKEN", expires_env = "ANTHROPIC_TOKEN_EXPIRES", refresh_hint = "run `refresh-token` to refresh the gateway JWT" }
```

---

## Not yet supported

These are designed (`docs/architecture.md`) or planned (`development_plan.md`) but **not built**:

- **Built-in tools** (`fs.read_file`, `fs.write_file`, `fs.list_dir`, `repo.grep`, `git.diff`) and
  **tool descriptor advertising** — required for kitsu to write files / run tools on its own
  (currently it returns code as text; you redirect to a file). *(Phase 3–4)*
- **Interactive `chat` REPL** (history, `/model`, `/clear`) — currently a stub. *(Phase 1)*
- **Anthropic-native and Ollama flavors** — config-accepted but not wired (Anthropic already works
  via the `openai` flavor on the gateway). *(Phase 2 / post-MVP)*
- **`--json` structured output**, **`doctor`**, **`models list`**, **shell completions**,
  **policy engine + audit log**. *(Phases 2–5)*
- **Retry/backoff** is implemented for 429/503; **Ctrl+C cancellation** is not yet.

## Known gotchas

- **`temperature`** — omit it (don't pass `--temperature`) for newer Anthropic models, or you get
  `HTTP 400: temperature is deprecated`.
- **Token staleness in long sessions** — `refresh-token` refreshes your login shell, not an
  already-running process; re-source or read the fresh token from `~/.zshrc`.
