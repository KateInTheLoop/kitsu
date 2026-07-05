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
- [Hosted vs. local (Hugging Face vs Ollama/vLLM)](#hosted-hugging-face-vs-local-ollama--vllm)
  - [Run Ollama in Docker](#run-ollama-in-docker)
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

## Hosted (Hugging Face) vs. local (Ollama / vLLM)

kitsu treats both the same way — a gateway profile and a `--gateway` flag. The
difference is where the model runs and what it costs.

| | Hugging Face (hosted) | Local (Ollama / vLLM) |
|---|---|---|
| Runs on | HF's router + partner GPUs | your machine |
| Credentials | `HF_TOKEN` required | none |
| Cost | per-token (PRO credits or pay-as-you-go) | free (your hardware) |
| Models | huge catalog of hosted models | whatever you pull / serve |
| Speed & size | provider GPUs — large models, fast | bounded by your CPU/GPU |
| Privacy | prompt leaves your machine | stays local |
| Offline | no | yes |
| Setup | paste a token | run a server (native or Docker) |

**Use hosted (HF)** when you want a big model, no local GPU, or a model you
haven't downloaded. **Use local (Ollama/vLLM)** for privacy, offline work, zero
per-token cost, or fast iteration on a small model.

Same prompt, two backends:

```bash
# Hosted — Hugging Face
export HF_TOKEN=hf_...
kitsu --gateway huggingface ask "what does a mutex do?"

# Local — Ollama
kitsu --gateway ollama --model "qwen2.5-coder:latest" ask "what does a mutex do?"
```

### Run Ollama in Docker

If you have Docker, you don't need to install Ollama natively:

```bash
# Start Ollama in a container, published on the default port 11434
docker run -d --name ollama -p 11434:11434 ollama/ollama

# Pull a model INTO the container (small ones are great for CPU testing)
docker exec ollama ollama pull qwen2.5-coder      # or: llama3.2, qwen2.5:0.5b, ...

# See what's available (also at http://localhost:11434/v1/models)
docker exec ollama ollama list

# Point kitsu at it — no token needed (the `ollama` profile uses method = "none")
kitsu --gateway ollama --model "qwen2.5-coder:latest" ask "Write a one-line Rust hello world."
```

Notes:

- **Model names include a tag.** Ollama IDs look like `qwen2.5-coder:latest`.
  Whatever `ollama list` / `/v1/models` shows is exactly what you pass to
  `--model` (or set as the profile's `model`). The bundled profile defaults to
  `llama3.2`; override per call with `--model`, or edit the `model =` line.
- **GPU:** add `--gpus all` to `docker run` if you have an NVIDIA GPU + the
  container toolkit; otherwise it runs on CPU (fine for small models).
- **Persistence:** add `-v ollama:/root/.ollama` to keep pulled models across
  container restarts.
- kitsu reaches Ollama through its **OpenAI-compatible** route
  (`/v1/chat/completions`), so nothing kitsu-specific runs in the container.

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
| `--yes` | off | Auto-approve all tool file access without prompting (for scripts/CI). Equivalent to answering "yes" to every permission prompt — use with care. |

**Chat-only flags**

| Flag | Description |
|---|---|
| `--allow-path <PATH>` | Allow read-only filesystem tools under `PATH` (repeatable). See [Tools in chat](#tools-in-chat). |
| `--allow-write <PATH>` | Allow the model to **generate/write files** under `PATH` (repeatable) via `fs_write_file`. Deny-by-default, independent of `--allow-path`. |
| `--script <FILE>` | Run turns from a file non-interactively (deterministic multi-turn). One turn per line; `#` comments and blank lines skipped; `/slash` commands work; a `<<< … >>>` block is a single multi-line turn. |

Examples:

```bash
kitsu --gateway openai ask "Summarize the CAP theorem."
kitsu --gateway anthropic --model claude-opus-4-8 ask --max-output-tokens 2000 "..."
echo "explain this error" | kitsu --gateway ollama chat        # one-shot via stdin
kitsu --gateway ollama chat "let's pair on a bug"              # interactive
```

---

## Tools in chat

In an interactive `chat` the model always has filesystem tools (`fs_read_file`,
`fs_list_dir`, `repo_grep`, `fs_write_file`), gated by a **permission model**
similar to Claude Code — it asks before touching paths you haven't allowed:

```
⏺ kitsu wants to write /home/you/project/notes.md
  [y] allow once   [a] always (this dir)   [n] deny
>
```

**Default posture (Balanced):** reads inside the directory you launched from are
allowed automatically (explore freely); reads *outside* it, and *all* writes,
prompt for approval. `a` (always) adds that directory to the session allowlist so
it won't ask again.

- **Pre-grant** paths so they never prompt: `--allow-path <dir>` (reads),
  `--allow-write <dir>` (writes). Both repeatable and independent — e.g. read a
  whole repo but confine generated files to one scratch dir.
- **In-session grants:** `/allow <path>` and `/allow-write <path>`.
- **Non-interactive** (`--script`/piped): deny-by-default (no one to prompt) —
  pass `--yes` to auto-approve everything, or pre-grant with the flags.
- Access is **deny-by-default** at the boundary; paths are canonicalized (writes
  canonicalize the parent), so `..`/symlink traversal is rejected. Generated
  files are capped at 1 MiB. `repo_grep` needs `rg` (ripgrep) on your `PATH`.

Slash commands inside the REPL:

| Command | Action |
|---|---|
| `/model <name>` | Switch model (keeps history). |
| `/models [filter]` | List the models the gateway offers (from its `models_path`), optionally filtered by a substring — e.g. `/models qwen`. Great for exploring a big catalog like Hugging Face; the listed IDs are what you pass to `/model`. |
| `/allow <path>` | Grant read access to a path for the rest of the session (no more prompts there). |
| `/allow-write <path>` | Grant write access to a path for the rest of the session. |
| `/save <file>` | Write the last response to `<file>` — just the fenced code blocks if present, else the whole message. |
| `/clear` | Clear the conversation (keeps the system prompt). |
| `/help` | Show help. |
| `/quit` (or `/exit`) | Exit. |

### Coding workflows: paste & file generation

- **Paste** a multi-line code block or stack trace straight into the prompt — it
  is captured as one turn (bracketed paste). Non-interactively, a `<<< … >>>`
  block in a `--script` file is the equivalent single multi-line turn.
- **Generate files** two ways: let the model call `fs_write_file` (grant it with
  `--allow-write <dir>`), or ask for code and run `/save out.rs` to write the
  last response's code blocks yourself.
- kitsu recovers tool calls that a model emits **as text** (common with local
  models via Ollama), so file generation works even when the model doesn't use
  the structured tool-call field.

```bash
# let the model write files into ./out, driven by a script
kitsu --gateway ollama chat --allow-write ./out --script gen.txt
```

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
