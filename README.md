# kitsu

A small, fast Rust CLI for talking to LLMs — **hosted or local** — through one
consistent interface. kitsu speaks the OpenAI `chat/completions` wire protocol,
which means it plugs into almost anything: OpenAI, Anthropic, Hugging Face, your
own corporate gateway, or a local server like **Ollama** or **vLLM**.

Everything is driven by config. There are **no hardcoded endpoints, keys, or
providers** in the binary — you describe each endpoint as a *gateway profile* and
select it at runtime.

- `kitsu ask "..."` — one-shot question, streamed answer.
- `kitsu chat` — interactive multi-turn REPL, with optional read-only filesystem
  tools.

---

## Table of contents

- [Install](#install)
- [Quick start](#quick-start)
- [How it works: gateway profiles](#how-it-works-gateway-profiles)
- [Where the config lives](#where-the-config-lives)
- [Add any OpenAI-compatible endpoint (the general recipe)](#add-any-openai-compatible-endpoint-the-general-recipe)
- [Provider recipes](#provider-recipes)
  - [Ollama (local)](#ollama-local)
  - [vLLM (local / self-hosted)](#vllm-local--self-hosted)
  - [Custom / corporate gateway](#custom--corporate-gateway)
  - [Hugging Face Inference Providers](#hugging-face-inference-providers)
  - [OpenAI](#openai)
  - [Anthropic](#anthropic)
- [Authentication](#authentication)
- [Credentials at a glance](#credentials-at-a-glance)
- [CLI reference](#cli-reference)
- [Tools in chat](#tools-in-chat)
- [Notes & gotchas](#notes--gotchas)

---

## Install

Build from source (Rust 1.80+):

```bash
cargo build --release
# binary at target/release/kitsu
```

For the examples below, either put `kitsu` on your `PATH` or run
`target/release/kitsu`.

---

## Quick start

kitsu ships with a ready-to-use [`kitsu.toml`](kitsu.toml) that defines several
example gateways. The default is **Ollama** (local, no credentials):

```bash
# 1. Start a local model
ollama serve
ollama pull llama3.2

# 2. Ask it something (uses default_gateway = "ollama")
kitsu --config ./kitsu.toml ask "Explain a mutex in one sentence."

# 3. Or chat interactively
kitsu --config ./kitsu.toml chat
```

Point at a different gateway with `--gateway`:

```bash
export HF_TOKEN=hf_...
kitsu --config ./kitsu.toml --gateway huggingface ask "Write a haiku about foxes."
```

Once you copy `kitsu.toml` to `~/.kitsu/config.toml` (see
[below](#where-the-config-lives)), you can drop the `--config` flag.

---

## How it works: gateway profiles

A **gateway profile** is a named block in the config describing one endpoint:

```toml
[gateways.myprofile]
flavor      = "openai"                       # wire protocol (openai chat/completions)
base_url    = "https://host.example.com"     # scheme + host (+ optional port)
chat_path   = "/v1/chat/completions"         # appended to base_url for chat
models_path = "/v1/models"                   # optional; endpoint's model list
model       = "some-model-id"                # default model for this profile
auth        = { method = "bearer-env", env = "MY_TOKEN" }
```

The request URL is simply `base_url + chat_path`. Select a profile with
`--gateway myprofile`, or make it the default:

```toml
default_gateway = "myprofile"
```

Override the model per call with `--model`:

```bash
kitsu --gateway huggingface --model "deepseek-ai/DeepSeek-V3:together" ask "hi"
```

That's the whole model. Adding a new provider is just adding a block — no code
changes, nothing baked into the binary.

---

## Where the config lives

kitsu resolves the config file in this order:

1. `--config <path>` flag
2. `$KITSU_CONFIG` environment variable
3. `~/.kitsu/config.toml` (default)

To use kitsu without passing `--config` every time:

```bash
mkdir -p ~/.kitsu
cp kitsu.toml ~/.kitsu/config.toml
```

Chat history is stored next to the config (e.g. `~/.kitsu/history`).

---

## Add any OpenAI-compatible endpoint (the general recipe)

If a service exposes an OpenAI-style `POST /v1/chat/completions`, kitsu can use
it. The recipe is always the same:

1. **Find the base URL and chat path.** Most are `https://<host>` +
   `/v1/chat/completions`.
2. **Find the auth.** A hosted API needs a bearer token; a local server usually
   needs none.
3. **Add a profile** and pick a `model` the endpoint serves.

```toml
[gateways.mygateway]
flavor      = "openai"
base_url    = "https://api.myprovider.com"
chat_path   = "/v1/chat/completions"
models_path = "/v1/models"                   # or "" if it has no model-list route
model       = "model-the-endpoint-serves"
auth        = { method = "bearer-env", env = "MYGATEWAY_TOKEN" }
```

```bash
export MYGATEWAY_TOKEN=...
kitsu --gateway mygateway ask "hello"
```

Not sure which models are available? If the endpoint has a models route, kitsu
reads it from `models_path` (useful for scripting / discovery).

---

## Provider recipes

All recipes below are already present in the bundled `kitsu.toml`.

### Ollama (local)

Ollama serves an OpenAI-compatible API on port 11434. No credentials.

```toml
[gateways.ollama]
flavor      = "openai"
base_url    = "http://localhost:11434"
chat_path   = "/v1/chat/completions"
models_path = "/v1/models"
model       = "llama3.2"
auth        = { method = "none" }
```

```bash
ollama serve && ollama pull llama3.2
kitsu --gateway ollama ask "hi"
```

### vLLM (local / self-hosted)

```toml
[gateways.vllm]
flavor      = "openai"
base_url    = "http://localhost:8000"
chat_path   = "/v1/chat/completions"
models_path = "/v1/models"
model       = "meta-llama/Llama-3.1-8B-Instruct"
auth        = { method = "none" }
```

```bash
vllm serve meta-llama/Llama-3.1-8B-Instruct --port 8000
kitsu --gateway vllm ask "hi"
```

If you launched vLLM with `--api-key <key>`, switch the auth block to
`{ method = "bearer-env", env = "VLLM_API_KEY" }` and `export VLLM_API_KEY=<key>`.

### Custom / corporate gateway

Any OpenAI-compatible gateway behind your own auth. Copy the `custom` block,
rename it, and point it at your endpoint:

```toml
[gateways.custom]
flavor      = "openai"
base_url    = "https://your-gateway.example.com"
chat_path   = "/v1/chat/completions"
models_path = ""
model       = "your-model-id"
auth        = { method = "bearer-env", env = "CUSTOM_GATEWAY_TOKEN" }
```

Gateways that expose their chat route under a non-standard path are fine — just
set `chat_path` accordingly (e.g. `/api/v1/chat/completions`).

### Hugging Face Inference Providers

Hugging Face's router is OpenAI-compatible and fans out to many inference
providers (Together, Novita, Fireworks, Cerebras, SambaNova, …).

```toml
[gateways.huggingface]
flavor      = "openai"
base_url    = "https://router.huggingface.co"
chat_path   = "/v1/chat/completions"
models_path = "/v1/models"
model       = "meta-llama/Llama-3.3-70B-Instruct"
auth        = { method = "bearer-env", env = "HF_TOKEN" }
```

```bash
export HF_TOKEN=hf_...            # huggingface.co/settings/tokens
kitsu --gateway huggingface ask "hi"
```

- **Token:** create one at <https://huggingface.co/settings/tokens> with the
  "Make calls to Inference Providers" permission.
- **HF PRO** ($9/mo) adds a monthly inference-credit allowance and higher rate
  limits, drawn down through that same token.
- **Model IDs** are `<org>/<model>`; pin a specific provider by appending
  `:<provider>` (e.g. `deepseek-ai/DeepSeek-V3:together`). Only models with a
  live provider are callable — the `/v1/models` route lists them.

### OpenAI

```toml
[gateways.openai]
flavor      = "openai"
base_url    = "https://api.openai.com"
chat_path   = "/v1/chat/completions"
models_path = "/v1/models"
model       = "gpt-4o-mini"
auth        = { method = "bearer-env", env = "OPENAI_API_KEY" }
```

```bash
export OPENAI_API_KEY=sk-...      # platform.openai.com/api-keys
kitsu --gateway openai ask "hi"
```

### Anthropic

Anthropic hosts an OpenAI-compatible endpoint, so kitsu talks to it with no
special handling.

```toml
[gateways.anthropic]
flavor      = "openai"
base_url    = "https://api.anthropic.com"
chat_path   = "/v1/chat/completions"
models_path = "/v1/models"
model       = "claude-opus-4-8"
auth        = { method = "bearer-env", env = "ANTHROPIC_API_KEY", expires_env = "" }
```

```bash
export ANTHROPIC_API_KEY=sk-ant-...   # console.anthropic.com/settings/keys
kitsu --gateway anthropic ask --max-output-tokens 1024 "hi"
```

- Requires a **developer API key** (`sk-ant-…`). A claude.ai Pro/Max
  **subscription** login does *not* grant API access — it's a separate system.
- Anthropic's endpoint requires a `max_tokens`, so pass `--max-output-tokens N`
  or the request may be rejected.

---

## Authentication

Each profile's `auth` block picks one method:

| Method | Config | Behavior |
|---|---|---|
| `none` | `{ method = "none" }` | No `Authorization` header. For local servers (Ollama, vLLM). |
| `bearer-env` | `{ method = "bearer-env", env = "MY_TOKEN" }` | Sends `Authorization: Bearer $MY_TOKEN`. **Recommended** for hosted APIs. |
| `static` | `{ method = "static", token = "..." }` | Inline token. Discouraged — keeps a secret in the file. Redacted from debug output. |

Optional `auth` fields:

- `header` — override the header name (default `Authorization`).
- `scheme` — override the scheme (default `Bearer`; set `""` for a bare token).
- `expires_env` — name of an env var holding a token-expiry timestamp; kitsu
  warns (but still sends) when expired. Set `""` to disable.
- `refresh_hint` — a message shown when the token is missing or rejected, e.g.
  how to obtain/refresh it.

Tokens are read from the environment at request time and never written to the
repo. The `static` token, if used, is redacted from any debug output.

---

## Credentials at a glance

| Gateway | Env var | Credential | Where to get it |
|---|---|---|---|
| `ollama` | — | none | run `ollama serve` |
| `vllm` | — (or `VLLM_API_KEY`) | none, unless started with `--api-key` | your vLLM launch flags |
| `custom` | `CUSTOM_GATEWAY_TOKEN` | your gateway's token | your gateway admin |
| `huggingface` | `HF_TOKEN` | `hf_…` access token | huggingface.co/settings/tokens |
| `openai` | `OPENAI_API_KEY` | `sk-…` API key | platform.openai.com/api-keys |
| `anthropic` | `ANTHROPIC_API_KEY` | `sk-ant-…` API key | console.anthropic.com/settings/keys |

---

## CLI reference

```
kitsu [GLOBAL FLAGS] <COMMAND>
```

**Commands**

| Command | Description |
|---|---|
| `ask <prompt>` | Ask a single question; stream the answer. |
| `chat [prompt]` | Interactive multi-turn REPL. An optional first prompt is queued as the first turn. With piped stdin (non-TTY), the prompt/stdin is a single turn. |

**Global flags** (apply to any command)

| Flag | Default | Description |
|---|---|---|
| `--gateway <name>` | `default_gateway` | Which `[gateways.<name>]` profile to use. |
| `--model <id>` | profile's `model` | Override the model for this call. |
| `--config <path>` | see resolution order | Path to the config file. |
| `--no-stream` | off | Request a single non-streamed completion. |
| `--temperature <f>` | unset | Sampling temperature. Omitted from the request unless set (some models reject it). |
| `--max-rounds <n>` | `10` | Max agent tool-call rounds per turn. Raise (e.g. `40`) for tool-heavy work. |
| `--timeout-secs <n>` | `60` | Per-request HTTP timeout. Raise (e.g. `300`) for slow/heavy calls. |
| `--max-output-tokens <n>` | unset (gateway default) | Cap output tokens. Required by some gateways (e.g. Anthropic); raise for long-form output. |

**Chat-only flag**

| Flag | Description |
|---|---|
| `--allow-path <PATH>` | Allow read-only filesystem tools under `PATH` (repeatable). See [Tools in chat](#tools-in-chat). |

Examples:

```bash
kitsu --gateway openai ask "Summarize the CAP theorem."
kitsu --gateway anthropic --model claude-opus-4-8 ask --max-output-tokens 2000 "..."
echo "explain this error" | kitsu --gateway ollama chat        # one-shot via stdin
kitsu --gateway ollama chat "let's pair on a bug"              # interactive
```

---

## Tools in chat

`chat` can grant the model **read-only** filesystem tools, scoped to paths you
explicitly allow:

```bash
kitsu --gateway ollama chat --allow-path ./src --allow-path ./docs
```

- Passing at least one `--allow-path` enables the read-only tool set
  (`fs_read_file`, `fs_list_dir`, `repo_grep`). Without it, no tools are
  advertised.
- Access is **deny-by-default**: reads are permitted only under an allowed root.
  Paths are canonicalized, so `..`/symlink traversal out of the allowlist is
  rejected.
- `repo_grep` is registered only when `rg` (ripgrep) is on your `PATH`; without
  it, the file-read and list-dir tools still work.

Slash commands inside the REPL:

| Command | Action |
|---|---|
| `/model <name>` | Switch model (keeps history). |
| `/clear` | Clear the conversation (keeps the system prompt). |
| `/help` | Show help. |
| `/quit` (or `/exit`) | Exit. |

`Ctrl+C` cancels an in-flight response; `Ctrl+D` exits.

---

## Notes & gotchas

- **`--max-output-tokens` for some gateways.** If a gateway requires `max_tokens`
  (Anthropic does), omitting it may 400 the request, or the gateway's own small
  default (often 4096) may silently truncate long outputs. Pass the flag for
  long-form generation.
- **`--temperature` is opt-in.** It's omitted from the request unless you set it,
  because some models reject a `temperature` field.
- **Local servers use `method = "none"`.** Ollama and vLLM (unless launched with
  `--api-key`) need no token.
- **Token usage** is emitted (when the gateway reports it) as a single
  `kitsu-usage: {...}` line on **stderr**, so stdout stays clean for piping.
- **Exit codes:** `0` success, `1` user/config/auth-missing error, `2` provider
  error (network, gateway 4xx/5xx).
