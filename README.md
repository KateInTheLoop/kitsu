# kitsu

A small, fast Rust CLI for talking to LLMs — **hosted or local** — through one
consistent interface. kitsu speaks the OpenAI `chat/completions` wire protocol,
which means it plugs into almost anything: OpenAI, Anthropic, Hugging Face, your
own corporate gateway, or a local server like **Ollama** or **vLLM**.

Everything is driven by config. There are **no hardcoded endpoints, keys, or
providers** in the binary — you describe each endpoint as a *gateway profile* and
select it at runtime.

- `kitsu ask "..."` — one-shot question, streamed answer.
- `kitsu chat` — interactive multi-turn coding chat with permission-gated
  filesystem tools, verified diffs, and resumable sessions.

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

Input history and interactive conversation snapshots live in private Kitsu state
under `~/.kitsu/` (project-local configs get an isolated, stable versioned profile
there, never a `sessions/` directory in the project). State is created lazily, so
stateless scripted runs also work with a read-only home directory. Resume with
`kitsu chat --resume last`. Auth configuration and permission grants are not
stored, but the full conversation and tool transcript is—use `--no-session` when
that text may contain secrets. Active config/state paths are excluded from model
file tools and verified diffs.

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
| `models [filter]` | List models advertised by the selected gateway without requiring a configured model ID. |
| `chat [prompt]` | Interactive multi-turn REPL. An optional first prompt is queued as the first turn. With piped stdin (non-TTY), the prompt/stdin is a single turn. |

**Global flags** (apply to any command)

| Flag | Default | Description |
|---|---|---|
| `--gateway <name>` | `default_gateway` | Which `[gateways.<name>]` profile to use. |
| `--model <id>` | profile's `model` | Override the model for this call. |
| `--config <path>` | see resolution order | Path to the config file. |
| `--no-stream` | off | Request a single non-streamed completion. |
| `--jsonl` | off | Emit versioned `kitsu.chat.v1` JSON Lines events. Interactive TTY chat and human help/version output are rejected so stdout stays machine-only. |
| `--temperature <f>` | unset | Sampling temperature. Omitted from the request unless set (some models reject it). |
| `--max-rounds <n>` | `10` | Max agent tool-call rounds per turn (minimum `1`). Raise (e.g. `40`) for tool-heavy work. |
| `--timeout-secs <n>` | `60` | Total inference deadline, including retries, backoff, and streamed body reads. |
| `--max-output-tokens <n>` | unset (gateway default) | Cap output tokens. Required by some gateways (e.g. Anthropic); raise for long-form output. |
| `--yes` | off | Auto-approve enabled filesystem requests; it never enables tools or approves developer commands. |
| `--context-window <n>` | profile/cache, else `8192` | Override the model context window. A cold metadata lookup runs asynchronously and never delays the first inference. |
| `--compaction <trim\|summarize>` | `trim` | Choose how older context is reclaimed. |
| `--skill <name>` | unset | Preload a local skill (repeatable; accepted before or after the subcommand). |
| `--no-project-context` | off | Do not discover or send project `llms.txt`/`AGENTS.md` context or local skills. |

**Chat-only flags**

| Flag | Description |
|---|---|
| `--allow-path <PATH>` | Allow read-only filesystem tools under `PATH` (repeatable). See [Tools in chat](#tools-in-chat). |
| `--allow-write <PATH>` | Allow the model to **generate/write files** under `PATH` (repeatable) via `fs_write_file`. Deny-by-default, independent of `--allow-path`. |
| `--enable-commands` | Enable guarded literal-argv developer commands. Approval is independent from filesystem access and `--yes`. |
| `--command-root <PATH>` | Restrict command working directories to an existing root (repeatable; implies `--enable-commands`). |
| `--allow-command <JSON_ARGV>` | Pre-approve one exact argv in the launch directory, e.g. `'["cargo","test"]'` (repeatable). |
| `--script <FILE>` | Run turns from a file non-interactively (deterministic multi-turn). One turn per line; `#` comments and blank lines skipped; `/slash` commands work; a `<<< … >>>` block is a single multi-line turn. |
| `--resume [ID]` | Resume an autosaved interactive session; omit the ID for `last`. Filesystem permissions are not restored. |
| `--no-session` | Disable conversation snapshots and prompt-history persistence for this chat. |
| `--allow-turn-errors` | Let a script exit 0 after failed turns/commands; strict nonzero exit is the default. |

Examples:

```bash
kitsu --gateway openai ask "Summarize the CAP theorem."
kitsu --gateway anthropic --model claude-opus-4-8 ask --max-output-tokens 2000 "..."
echo "explain this error" | kitsu --gateway ollama chat        # one-shot via stdin
kitsu --gateway ollama chat "let's pair on a bug"              # interactive
```

---

## Tools in chat

In an interactive `chat` the model has filesystem tools (`fs_read_file`,
`fs_list_dir`, `repo_grep`, `fs_edit_file`, `fs_write_file`), gated by a **permission model**
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
- **Non-interactive** (`--script`/piped): capabilities are absent unless their
  paths are pre-granted. `--yes` changes approval behavior only; it does not
  create read, write, or command authority.
- Access is **deny-by-default** at the boundary; paths are canonicalized (writes
  canonicalize the parent), so `..`/symlink traversal is rejected. Generated
  files are capped at 1 MiB. `repo_grep` needs `rg` (ripgrep) on your `PATH`.
- Kitsu's config, snapshots, history, caches, and private runtime state remain
  protected from model writes even when a broader parent root is granted.
- Developer commands use a separate policy, literal argv (never a shell string),
  approved working-directory roots, bounded output, and timeout/cancellation.
  Enable them explicitly with `--enable-commands`.

Enter `/` to show all commands, or type a slash prefix and press Tab to complete
it. Completion and `/help` share the same command registry, so their descriptions
stay in sync. Commands reject unexpected arguments and suggest close matches.

Slash commands inside the REPL:

| Command | Action |
|---|---|
| `/model <name>` | Switch model (keeps history). |
| `/models [filter]` | List the models the gateway offers (from its `models_path`), optionally filtered by a substring — e.g. `/models qwen`. Great for exploring a big catalog like Hugging Face; the listed IDs are what you pass to `/model`. |
| `/status` | Show the active gateway/model, directory, context usage, enabled tools, permissions, background agents, and cumulative/last compaction details. |
| `/diff [path]` | Show the verified staged, unstaged, and untracked workspace diff. This is read from Git/the filesystem, not assistant prose. |
| `/review [path]` | Ask the model to review that verified diff in a no-tools turn (it cannot edit or inspect beyond the supplied evidence). |
| `/permissions` | Show the current permission mode and canonical read/write roots. |
| `/tools` | Show enabled tools and the effective permission behavior. |
| `/allow <path>` | Grant read access to a path for the rest of the session (no more prompts there). |
| `/allow-write <path>` | Grant write access to a path for the rest of the session. |
| `/skills [filter]` | List available local skills, optionally filtered by name. |
| `/skill <name>` | Load a skill's instructions into the conversation context. |
| `/save <file>` | Write the last response to `<file>` — just the fenced code blocks if present, else the whole message. |
| `/context` | Show detailed context utilization and provenance. |
| `/compact` | Compact eligible older exchanges now and report the actual method and savings. |
| `/agents [id]` | List background agents, or show one agent's result. |
| `/queue [prompt]` | Show the bounded eight-prompt FIFO or append a prompt. On Unix, complete lines submitted during generation are captured and run in order. |
| `/retry` | Retry the last failed or cancelled prompt. Failed turns are removed from conversation history first. |
| `/new` | Autosave the current conversation and start a fresh session. |
| `/sessions` | List saved sessions. |
| `/resume <id\|last>` | Resume a saved session without restoring its old permissions. |
| `/fork [id]` | Clone a saved session to a fresh ID, then switch with fresh runtime permissions. |
| `/export <file> [markdown\|json]` | Export the active session without overwriting. If it has artifacts, also create `<file>.artifacts` and rewrite exact pointers to that sidecar. |
| `/archive <id>` | Move an inactive session into recoverable private archive storage. |
| `/restore <id>` | Restore an archived session. |
| `/delete <id>` | Permanently delete an exact inactive live-session ID. |
| `/undo` | Restore the latest model-write checkpoint only when its files still match. |
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
- **Attach files** with `@path`, `@path:line`, or `@path:start-end`. Expansion is
  permission checked, no-follow, UTF-8 only, deduplicated, and bounded to 32 KiB
  of selected content per file / 64 KiB total; a small line range can come from a
  larger file without buffering the whole file. Tab completion only exposes
  already granted, non-protected regular paths and does not traverse symlinks.
  Kitsu prints the exact attachment summary.
- **Edit precisely** with `fs_edit_file`: its old text is an exact precondition,
  so a stale edit fails instead of overwriting newer work. Writing turns report
  verified files/first changed lines and create a conflict-aware `/undo` checkpoint.
- kitsu recovers tool calls that a model emits **as text** (common with local
  models via Ollama), so file generation works even when the model doesn't use
  the structured tool-call field.

```bash
# let the model write files into ./out, driven by a script
kitsu --gateway ollama chat --allow-write ./out --script gen.txt
```

Script failures include the file and line, roll the failed conversation turn
back, and return exit `1` (command/input/auth setup) or `2` (provider,
truncation, exhausted rounds, or a failed tool call). Use `--allow-turn-errors`
only when partial success is intentional.

For automation, `--jsonl` emits one `kitsu.chat.v1` object per stdout line:
`run_started`, framed turn/command events, provider/tool/usage events, assistant
output, typed errors, and exactly one `final_summary`. Human stderr is not needed
to determine the outcome. JSONL deliberately uses non-streaming provider
responses so a partial text fragment can never corrupt event framing.

`Ctrl+C` cancels an in-flight response and makes it available to `/retry`; at an
idle prompt it clears input. Automation emits a structured cancelled event and
exits `130`. `Ctrl+D` or `/quit` does not silently discard live background agents.
Completed filesystem side effects are never pretended away after cancellation—
inspect them with `/diff` or restore a safe checkpoint with `/undo`. Interactive
type-ahead collection is Unix-only: complete lines entered during generation are
captured in an eight-prompt visible FIFO. On other platforms `/queue <prompt>`
still works, but Kitsu does not actively collect terminal type-ahead while a turn
is running.

For simple or accessible terminal output, set `NO_COLOR=1`, `KITSU_PLAIN=1`, or
`TERM=dumb`. Renderer-owned banners, progress, tool activity, context meters, and
permission prompts then use stable ASCII without styling escapes; arbitrary
Unicode in model output and paths is preserved. Set `KITSU_REDUCED_MOTION=1` to
replace the animated wait indicator with a visible static status.

---

## Context window

kitsu budgets the conversation against a **resolved context window** so long
sessions do not blindly grow past it — important for small local models.

- **Set the window the model actually serves:** `context_window` on the gateway
  profile, or `--context-window <tokens>` per run. Otherwise Kitsu uses a fresh
  24-hour discovery cache when available. On a cold cache it immediately enforces
  the conservative 8192-token default and probes `/v1/models` asynchronously for
  a later run, so optional metadata cannot delay healthy inference. Advisory
  discovery has a separate two-second deadline; one provider instance reuses a
  successful catalog for 60 seconds and negative-caches eligible failures for
  five seconds. Ollama's OpenAI route often reports no window and may serve 4096,
  so configure it explicitly.
- **How the budget works:** `effective = window − reply reservation`
  (from `--max-output-tokens`, clamped to leave room for the prompt). Large tool
  results are **offloaded to a private artifact** as they arrive, leaving a small
  pointer + preview the model can re-read with `fs_read_file`; when persistence
  succeeds, the full result remains available outside the prompt. When history
  still exceeds the trim target, kitsu **elides the oldest tool-result bodies**, then **drops the
  oldest whole exchanges** — always preserving the system prompt, the most recent
  turns, and the last user message, and never orphaning a tool result. Token
  counts are estimated but **anchored on real `usage` when the provider reports
  it**, improving the estimate without requiring a local tokenizer.
- **See utilization + provenance:** `/context` prints model · window ·
  used/effective with a bar and headroom, then an honest **provenance table** —
  every context source with its `tokens`, `policy`, and `why` (base prompt,
  `project (llms.txt)`, `skill index`, `tool schemas`, and the conversation).
  Nothing loads into your window without a line here. Interactive turns end in
  one compact usage/context summary; redirected runs retain the machine-readable
  `kitsu-context: {"used":…,"effective":…,"window":…}` stderr record (companion to
  `kitsu-usage:`).

Every provider request gets a final checked preflight. If protected recent
context cannot fit, Kitsu fails locally with an actionable overflow error instead
of sending an oversized request. `--compaction summarize` uses a separately
bounded summarizer request and falls back safely to trimming; `/compact` runs the
same machinery manually. `/context` and `/status` disclose compaction counts,
tokens saved, removed exchanges, tool-result elisions, and fallback reasons. See
[docs/context-management-design.md](docs/context-management-design.md).

### Project context (`llms.txt`)

On `chat` start, kitsu loads curated project context from an
[`llms.txt`](https://llmstxt.org/) — the open standard for LLM-friendly project
context — found in the launch directory or up at the project root, and injects it
into the system prompt. So the model knows what your project *is* without you
pasting docs, and without slurping every markdown file. The `## Optional` section
is loaded only when the window has room (it's the first thing skipped on small
models). Markdown links are left as an index for the model; Kitsu does not follow
or fetch them. If there's no `llms.txt`, kitsu falls back to an
`AGENTS.md`/`CLAUDE.md`.
Kitsu prints the exact loaded source path at startup and includes it in `/status`
and `/context`. Pass `--no-project-context` (or set
`KITSU_NO_PROJECT_CONTEXT=1`) to disable, including when resuming a session (the
saved project-bearing system message is replaced). kitsu ships its own `llms.txt`, so any
`llms.txt`-aware tool — including kitsu — can understand the repo.

---

## Skills

Skills are **local, opt-in** capability bundles — reusable instructions the model
loads on demand. A skill is a directory with a `SKILL.md`:

```markdown
---
name: code-review
description: Review a file or diff for bugs and report findings by severity.
when_to_use: When the user asks to review code.
always: false
---
# Code review
1. Read the target file(s) with fs_read_file.
2. Report each finding as `file:line — [High|Med|Low] …`.
...
```

- **Discovery:** `.kitsu/skills/*/SKILL.md` (project, walking up to the git root)
  and `~/.kitsu/skills/*/SKILL.md` (user). kitsu ships a couple under
  `.kitsu/skills/` as examples. Symlinked skill roots, directories, and
  `SKILL.md` files are ignored rather than widening read authority.
- **On demand:** only the index (`name — description`) is always in context; the
  model loads a skill's body by calling the `skill` tool when a task matches, or
  you run `/skill <name>`. `/skills [filter]` lists them. `--skill <name>`
  preloads one up front (works for `ask` too, and for spawned agents).
- **Bundled files & links:** a skill dir can ship UTF-8 resource files
  (checklists, templates). Kitsu captures them through no-follow file
  descriptors during discovery (32 KiB each / 64 KiB total) and includes that
  bounded snapshot when the skill loads; loading a skill never widens filesystem
  authority. A skill's `links:` are surfaced to you but **never auto-fetched**.
- **Local only, by design:** skills are trusted because *you placed them there* —
  kitsu never fetches them from the network (injecting internet instructions into
  a tool-enabled agent is exactly what the permission model prevents). To use one
  from GitHub, `git clone` it into `~/.kitsu/skills/`. A skill's text is context,
  not privileged: every action it suggests still goes through permissions.
- Disable auto-loading (skills + `llms.txt`) with `KITSU_NO_PROJECT_CONTEXT=1`.

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
- **Token usage** is shown as a compact human status line in a terminal. When
  output is redirected, it remains a stable `kitsu-usage: {...}` line on
  **stderr**, so stdout stays clean for piping and scripts remain compatible.
- **Exit codes:** `0` success (including accepted partial scripts with
  `--allow-turn-errors`), `1` CLI/input/config/missing-auth or other user-policy
  failure, `2` provider/HTTP/timeout or strict incomplete turn (length limit,
  exhausted rounds, failed tool call), and `130` Ctrl+C cancellation.
- **Platform hardening:** Unix uses descriptor-bound command working directories,
  process-group cancellation, and descriptor-relative no-follow file/session
  operations. Portable branches retain validation and direct-child cancellation,
  but cannot close every pathname race or terminate an entire descendant process
  group; unsupported atomic primitives fail closed.
