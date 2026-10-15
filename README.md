# kitsu

A small, fast Rust CLI for talking to LLMs — **hosted or local** — through one
consistent interface. kitsu speaks the OpenAI `chat/completions` wire protocol,
which means it plugs into almost anything: OpenAI, Anthropic, Hugging Face, your
own corporate gateway, or a local server like **Ollama** or **vLLM**.

Everything is driven by config. There are **no hardcoded endpoints, keys, or
providers** in the binary — you describe each endpoint as a *gateway profile* and
select it at runtime.

- `kitsu` — interactive multi-turn coding chat by default.
- `kitsu -p "..."` — one-shot prompt, streamed answer, then exit.
- `kitsu chat` — the explicit form of chat, with permission-gated
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

### Prebuilt packages

Every [release](https://github.com/KateInTheLoop/kitsu/releases) ships packages
for Linux (x86_64 / aarch64), macOS (Apple Silicon, Intel, universal) and
Windows (x86_64). The Linux binaries are statically linked against musl, so the
`.deb` and `.rpm` have no dependencies and install on any reasonably modern
distro.

```bash
# Debian / Ubuntu
curl -LO https://github.com/KateInTheLoop/kitsu/releases/latest/download/kitsu_0.1.0-1_amd64.deb
sudo dpkg -i kitsu_0.1.0-1_amd64.deb

# Fedora / RHEL / openSUSE
sudo dnf install https://github.com/KateInTheLoop/kitsu/releases/latest/download/kitsu-0.1.0-1.x86_64.rpm

# macOS / any Linux — tarball
curl -L https://github.com/KateInTheLoop/kitsu/releases/latest/download/kitsu-v0.1.0-universal-apple-darwin.tar.gz | tar xz
sudo install -m755 kitsu-v0.1.0-universal-apple-darwin/kitsu /usr/local/bin/kitsu
```

Windows: download the `...-x86_64-pc-windows-msvc.zip` asset, unzip it, and put
`kitsu.exe` somewhere on your `PATH`.

`SHA256SUMS` is published with every release; verify with
`sha256sum -c SHA256SUMS --ignore-missing`.

macOS builds are not code-signed, so the first run needs
`xattr -d com.apple.quarantine kitsu` (or Right-click → Open).

### Container image

Releases also publish a minimal image (distroless, nonroot, amd64 + arm64) to
GHCR, built from the same static binaries the tarballs ship:

```bash
docker run --rm -it \
  -v ~/.kitsu:/home/nonroot/.kitsu \
  ghcr.io/kateintheloop/kitsu:latest
```

Mount your config as shown; add `--network host` to reach an Ollama on the
host's localhost, and mount a workdir (e.g. `-v "$PWD":/work -w /work`) to use
the filesystem tools on your project.

### Build from source

Rust 1.80+:

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

# 2. Ask it something once (uses default_gateway = "ollama")
kitsu --config ./kitsu.toml -p "Explain a mutex in one sentence."

# 3. Or chat interactively — chat is the default command
kitsu --config ./kitsu.toml
```

For a minimal no-secret Ollama profile instead, initialize a new file and check
it before chatting:

```bash
kitsu init                         # creates ./kitsu.toml; never overwrites
kitsu --config ./kitsu.toml doctor # offline, read-only diagnostics
```

Point at a different gateway with `--gateway`:

```bash
kitsu --config ./kitsu.toml --gateway huggingface auth login
kitsu --config ./kitsu.toml --gateway huggingface ask "Write a haiku about foxes."
```

Once you copy `kitsu.toml` to `~/.kitsu/config.toml` (see
[below](#where-the-config-lives)), you can drop the `--config` flag.

---

## How it works: gateway profiles

A **gateway profile** is a named block in the config describing one endpoint:

```toml
[gateways.myprofile]
flavor      = "openai"                       # compatibility/diagnostic label
adapter     = "compatible-chat"              # optional; this is the wire selector
base_url    = "https://host.example.com"     # scheme + host (+ optional port)
chat_path   = "/v1/chat/completions"         # appended to base_url for chat
models_path = "/v1/models"                   # optional; endpoint's model list
model       = "some-model-id"                # default model for this profile
auth        = { method = "bearer-env", env = "MY_TOKEN" }
```

The request URL is simply `base_url + chat_path`. Select a profile with
`--gateway myprofile`, set the reusable `KITSU_GATEWAY` environment default, or
make it the config default:

```toml
default_gateway = "myprofile"
```

Likewise, `KITSU_MODEL` overrides the selected profile's model and `--model`
overrides the environment for one launch. During chat, `/model <id>` is the
final runtime override for subsequent turns; it does not mutate the parent
shell. The complete precedence is:

1. `/model` after chat starts (model only)
2. explicit `--gateway` / `--model`
3. `KITSU_GATEWAY` / `KITSU_MODEL`
4. `default_gateway` / the selected profile's `model`

`adapter` is a closed compiled choice: `compatible-chat` (the default when the
field is omitted) or `anthropic-messages`. `flavor` never silently changes the
wire, so existing profiles keep their current compatible-Chat behavior.
Messages requires a `max_tokens` wire field; when `--max-output-tokens` is not
set, that adapter uses Kitsu's bounded 4,096-token default. An explicit value
is preserved, while explicit zero is rejected before any request.

For example:

```bash
kitsu --gateway huggingface --model "deepseek-ai/DeepSeek-V3:together" ask "hi"
```

Adding another profile for either compiled adapter is just adding a block.
Adding a genuinely different wire dialect requires a compiled Kitsu adapter
and the shared conformance suite; configuration cannot load provider code.

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

`kitsu init` writes a credential-free starter to `./kitsu.toml` (or
`--path`, then `--config`) using no-clobber creation. `kitsu doctor`
reports config lookup and parse status, selected gateway/model,
credential-variable presence (never its value), context/cache provenance,
project context, runtime-state writability, and tool policy without changing
state or contacting a provider. It also reports the effective automatic-artifact
policy. Offline doctor explicitly reports that no provider route was
constructed. An opted-in provider check adds the bounded, secret-free compiled
adapter/profile/generation/wire/capability/policy receipt for the route it
actually binds. Add `doctor --online` to opt into a
model-catalog reachability request. That check does not call the chat route or
prove that the credential can run inference; the human and JSON reports label
this scope explicitly. Use `doctor --inference` instead to send exactly one
minimal, tool-free chat completion. That probe is explicitly labeled as
potentially billable, validates the configured chat route and model, and cannot
be combined with catalog-only `--online`.

Human `kitsu models` output on a terminal and interactive `/models` show a
12-row preview with the active model pinned and marked first, plus an exact
substring-filter hint. Redirected `kitsu models` stdout remains the complete
newline-delimited ID list for scripts; `--jsonl` likewise retains the complete
structured catalog. JSONL catalog traffic is causally framed as a logical
`provider_request` with `purpose: "model_catalog"`, followed by one
`provider_attempt` for an actual GET (and none for a cache hit), before the
catalog or typed error. Top-level lookup uses `operation_id: "models:1"`;
scripted slash lookups retain their `line` and `command` identity.

Input history and interactive conversation snapshots live in private Kitsu state
under `~/.kitsu/` (project-local configs get an isolated, stable versioned profile
there, never a `sessions/` directory in the project). State is created lazily, so
stateless scripted runs also work with a read-only home directory. Resume with
`kitsu chat --resume last`. Auth configuration and permission grants are not
stored, but the full conversation and tool transcript is—use `--no-session` when
that text may contain secrets. Active config/state paths are excluded from model
file tools and verified diffs.

Scripted and piped chat stay stateless unless continuity is explicit:
`chat --session` creates and autosaves a new conversation, while
`chat --resume <id|last>` restores and autosaves an existing one. Successful
automation turns are checkpointed as they complete; a new durable run does not
publish an empty snapshot before its first successful model turn or a slash
command that changes durable conversation/model state. Query-only scripts do
not create zero-turn sessions. `last` prefers the newest conversation with a
real user turn over legacy zero-turn snapshots.
Resume resets runtime grants, never restores saved grants, and retains only
roots explicitly supplied by the current invocation. It also rebuilds the
leading system prompt from the current model/tools/project/skill surface.
Persisted `/skill` loads carry source/resource hashes; changed loads are replaced
from the current pinned source and missing ones are removed with a disclosure.
`/clear` is a recoverable boundary: it saves the current durable transcript
under its existing ID, starts a fresh unnamed ID, resets runtime grants while
retaining invocation grants, and reports both IDs plus the preserved counts.
The old conversation remains resumable. The fresh ID stays explicitly pending
until its first successful turn; with `--no-session`, `/clear` warns that it can
only discard in-memory history. Scripted JSONL exposes the same facts in a
typed `clear_session_boundary` result.

Session inventory and lifecycle do not require a provider or even a parseable
config file; the resolved config path only selects the isolated namespace:

```bash
kitsu sessions
kitsu sessions show last --json
kitsu sessions fork last
kitsu sessions export last conversation.md
kitsu sessions archive <exact-id>
kitsu sessions restore <exact-id>
kitsu sessions delete <exact-id>
kitsu sessions resume last --json  # prints exact argv outside interactive chat
```

Archived snapshots are intentionally absent from the live inventory and cannot
be resumed until restored. Keep the exact ID from the archive receipt and run
`kitsu sessions restore <exact-id>` (or `/restore <exact-id>` inside chat)
before resuming it.

Fork receipts resolve `last` before cloning and identify the exact lineage as
`source-session-id -> fork-session-id`. JSON adds `source_session_id` alongside
the existing forked `session` and `permissions_restored` fields. The source
keeps its unique name, the fork starts unnamed, and saved permission grants are
never copied.

### Artifacts on disk

Developer-command output is captured in full into Kitsu-private *generation*
directories so a one-shot automation run can still read it after the process
exits. That retention is deliberate, which also means generations accumulate
across runs. Inspect and reclaim them without contacting a provider:

```bash
kitsu artifacts                       # list retained generations across runs
kitsu artifacts list --json           # same, for automation
kitsu artifacts prune all --dry-run   # report what would be reclaimed
kitsu artifacts prune all             # reclaim it
kitsu artifacts prune <id> --older-than 7d
```

`kitsu artifacts` is **host-only**: it runs outside any chat, no model authority
is involved, and none is created. Safety is biased toward keeping data — a
generation held by a live process (proven by an advisory lock the kernel
releases on process death, not by a reusable pid) is skipped with a named
reason, session-owned sidecars are skipped, and only exact
`runtime-<pid>-<nanos>-<gen>` and `<session-id>.artifacts` directories are ever
touched. Chat startup additionally sweeps *provably empty* orphans, which
destroys nothing of value, and prints a one-line notice — never a deletion —
when non-empty leftovers pass a threshold.

Use global `--no-artifacts` for a run that creates no new automatic command or
oversized tool-result files, or set top-level `artifact_storage = false` as the
config default. Commands still drain safely and return bounded head/tail
previews; oversized generic tool results are irreversibly reduced to a
UTF-8-safe preview with exact original/retained/omitted byte counts. Existing
artifacts are not swept, rotated, rewritten, or deleted. This policy is
orthogonal to conversation state: pair it with `chat --no-session` for
session-stateless automation. Credentials, explicit model filesystem writes,
`-o`, schemas, required-write targets, and explicit session/export operations
retain their documented behavior.

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

If a selected Ollama profile points at a loopback URL but cannot be reached,
Kitsu names local Ollama and suggests checking `ollama serve` or selecting a
different gateway. Other connection failures keep the generic gateway remedy;
neither form prints the configured URL or provider-controlled error detail.
With `--jsonl`, the local case also emits the stable
`diagnostic_code: "local_ollama_unreachable"` and bounded recovery-action
enums, while generic failures omit those optional fields.

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
kitsu --gateway huggingface doctor --inference # one potentially billable probe
kitsu --gateway huggingface ask "hi"
```

- **Token:** create one at <https://huggingface.co/settings/tokens> with the
  "Make calls to Inference Providers" permission.
- **HF PRO** ($9/mo) adds a monthly inference-credit allowance and higher rate
  limits, drawn down through that same token.
- **Model IDs** are `<org>/<model>`. Append `:fastest` (the default policy),
  `:cheapest`, or `:preferred` for server-side provider selection, or pin one
  with `:<provider>` (for example `openai/gpt-oss-120b:groq`). Only models with
  a live provider are callable; `/v1/models` lists the current routes.

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

Use Kitsu's compiled native Messages adapter explicitly:

```toml
[gateways.anthropic]
flavor      = "anthropic"
adapter     = "anthropic-messages"
base_url    = "https://api.anthropic.com"
chat_path   = "/v1/messages"
models_path = ""
model       = "claude-opus-4-8"
auth        = { method = "bearer-env", env = "ANTHROPIC_API_KEY", header = "x-api-key", scheme = "", expires_env = "" }
```

```bash
export ANTHROPIC_API_KEY=sk-ant-...   # console.anthropic.com/settings/keys
kitsu --gateway anthropic ask --max-output-tokens 1024 "hi"
```

- Requires a **developer API key** (`sk-ant-…`). A claude.ai Pro/Max
  **subscription** login does *not* grant API access — it's a separate system.
- Anthropic's endpoint requires a `max_tokens`, so pass `--max-output-tokens N`
  for ordinary chat. This adapter is streaming-only and does not expose model
  catalog, structured-output, image-input, or prompt-cache capabilities yet.

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
| `bearer-env` | `{ method = "bearer-env", env = "MY_TOKEN" }` | Sends a bearer credential from a non-empty environment value, otherwise from Kitsu's config-scoped managed store. **Recommended** for hosted APIs. |
| `static` | `{ method = "static", token = "..." }` | Inline token. Discouraged — keeps a secret in the file. Redacted from debug output. |

Optional `auth` fields:

- `header` — override the header name (default `Authorization`).
- `scheme` — override the scheme (default `Bearer`; set `""` for a bare token).
- `expires_env` — name of an env var holding a token-expiry timestamp; kitsu
  warns (but still sends) when expired. Set `""` to disable.
- `refresh_hint` — a message shown when the token is missing or rejected, e.g.
  how to obtain/refresh it.

For a `bearer-env` profile, a non-empty configured environment value takes
precedence. Otherwise `kitsu --gateway <name> auth login` reads a token with
terminal echo disabled and atomically stores it in Kitsu's private,
config-scoped state. For secret-manager pipelines, opt in explicitly with
`auth login --token-stdin`; tokens are never accepted on argv. `auth status
--json` reports only booleans and the effective source, and `auth logout`
removes only Kitsu's stored value. Login is a local operation and never claims
the token works remotely—use `doctor --inference` for that explicit check.
Login requires an absolute `HOME` and refuses Kitsu's temporary fallback state.

Credentials are never written to the repository or printed in status, JSON,
errors, or tracing. Before chat, review, or model-catalog work loads project
context or registers tools, Kitsu validates that the effective source is
locally usable. The managed namespace is protected from model filesystem tools
and cannot intersect an approved command/runtime root. The `static` token, if
used, remains redacted and is discouraged.

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
kitsu [GLOBAL FLAGS] [COMMAND]
kitsu -p <PROMPT>
```

With no command, Kitsu opens chat. `-p/--prompt` is a top-level shorthand for a
single tool-free `ask` turn and exits after the answer.

**Commands**

| Command | Description |
|---|---|
| *(none)* | Open interactive chat (`kitsu chat` remains the explicit form). |
| `init [--path PATH]` | Create a no-secret starter config without overwriting an existing file. |
| `doctor [--online\|--inference]` | Run offline diagnostics by default; opt into a catalog-only check or one potentially billable chat/inference probe. |
| `auth <login\|status\|logout>` | Provider-free managed-credential lifecycle for the selected `bearer-env` gateway; supports secret-free `--json` status and explicit `--token-stdin`. |
| `ask <prompt>` | Ask a single question; stream the answer. |
| `review [path]` | Run one tool-free review over host-verified working/staged/unstaged/base/commit evidence; supports JSONL, focus instructions, and opt-in session continuity. |
| `models [filter]` | List models advertised by the selected gateway without requiring a configured model ID. |
| `sessions [action]` | List/show/fork/export/archive/restore/delete sessions without a provider; fork receipts name the exact resolved source; `list [filter]` narrows by substring; `resume` launches chat on a TTY or emits exact argv otherwise. |
| `artifacts [list\|prune <id\|all>]` | Host-only, provider-free inventory and reclamation of command-artifact storage left by earlier runs. Live generations are never removed. |
| `agents [--all] [--json]` | Host-only, provider-free listing of the background agents every kitsu process on this machine is publishing, read from the advisory state files `[agents] state_file` writes. Dead processes are skipped unless `--all`. A file written by a newer kitsu is reported as schema-unsupported instead of dropped, and a thread record this binary cannot validate is counted rather than blanking the listing. The reader creates, modifies, and deletes nothing. |
| `chat [prompt]` | Interactive multi-turn REPL. An optional first prompt is queued as the first turn. With piped stdin (non-TTY), the prompt/stdin is a single turn. |

**Global flags** (apply to any command)

`--version` reports the package version, Git commit, and source-commit date.
The date is provenance from Git history, not the wall-clock build time.

| Flag | Default | Description |
|---|---|---|
| `--gateway <name>` | `KITSU_GATEWAY`, then `default_gateway` | Which `[gateways.<name>]` profile to use. |
| `--model <id>` | `KITSU_MODEL`, then profile's `model` | Override the model for this launch; `/model` can replace it inside chat. |
| `-p, --prompt <text>` | unset | Run one prompt non-interactively and exit; cannot be combined with a subcommand. |
| `--config <path>` | see resolution order | Path to the config file. |
| `--no-stream` | off | Request a single non-streamed completion. |
| `--jsonl` | off | Emit versioned `kitsu.chat.v1` JSON Lines events. Interactive TTY chat and human help/version output are rejected so stdout stays machine-only. |
| `-o, --output-last-message <PATH>` | unset | Atomically write the last fully committed assistant message to a host-owned file (`0600` on Unix) after non-interactive ask, review, piped chat, or scripted chat. Stdout/JSONL is unchanged; discarded or committed-partial output never replaces the file. |
| `--output-schema <PATH>` | unset | Constrain non-interactive final answers with a bounded strict JSON Schema. Kitsu sends the schema through OpenAI-compatible `response_format` and independently validates the exact assistant bytes before commit; invalid output exits `2` and cannot replace `-o`. |
| `--plain` | off | Use stable ASCII markers, disable color/animation, and select literal line-oriented input. |
| `--screen-reader` | off | Use escape-free, literal line-oriented input and plain rendering; `KITSU_SCREEN_READER` is the environment equivalent. |
| `--reduced-motion` | off | Keep styled output but replace animated waiting indicators with a static status. |
| `--tui <MODE>` | `classic` | Select the renderer: `classic` or `inline`. Overrides `[tui] mode`; `/tui` switches at runtime. |
| `--no-alt-screen` | off | Force `[tui] alternate_screen` to `never`, for multiplexers where the alternate screen misbehaves. |
| `--temperature <f>` | unset | Sampling temperature. Omitted from the request unless set (some models reject it). |
| `--reasoning-effort <level>` | unset | Forward `none`, `minimal`, `low`, `medium`, `high`, or `xhigh` to compatible gateways; omitted otherwise. `/reasoning` can change it between chat turns. |
| `--max-rounds <n>` | `10` | Max agent tool-call rounds per turn (minimum `1`). Raise (e.g. `40`) for tool-heavy work. |
| `--timeout-secs <n>` | `120` | Total inference deadline, including retries, backoff, streamed body reads, and every provider/tool round in one scripted turn. One-shot chat also charges piped-input and attachment acquisition to the same deadline; a positional prompt never waits for unrelated stdin EOF. If the deadline expires after real output already streamed, one-shot runs keep it: the answer ends with a `[stopped: deadline expired …]` marker and is labeled partial (exit code stays `2`). |
| `--agent-join-timeout <n>` | `--timeout-secs` | How long a non-interactive exit (scripted or piped chat) waits for live background agents to finish before cancelling them. |
| `--agent-spawn-cap <n>` | `8` interactive, `2` automation | Per-turn cap shared by synchronous, parallel, and background subagent spawns. `0` disables spawning and removes all four multi-agent tools from `/tools`, the system prompt, and provider requests. |
| `--max-output-tokens <n>` | unset (gateway default) | Cap output tokens. Required by some gateways (e.g. Anthropic). If the model hits an explicit cap, strict one-shot/scripted runs keep the partial text visible but do not commit the turn, exit `2`, and suggest retrying with twice the current value. |
| `--yes` | off | Never prompt for filesystem access; enabled tools remain confined to explicit/current roots and requests outside them are denied. Never enables tools or approves developer commands. |
| `--dangerously-allow-all-filesystem` | off | **DANGER:** let enabled filesystem tools access every non-protected host path without prompting. This is the explicit global bypass; it still never enables tools or approves commands. |
| `--dangerously-skip-permissions`, `--yolo` | off | **DANGER:** run fully unattended. `--yolo` is an exact alias for the long spelling: enabled filesystem tools may access every non-protected path, read/write tools are registered for automation, and commands are auto-approved only when command capability is separately enabled. Kitsu-private paths, explicit `/permissions protect` guards, command roots/validation/containment, and secret-environment filtering remain enforced. |
| `--context-window <n>` | profile/cache, else `8192` | Override the model context window. A cold metadata lookup runs asynchronously and never delays the first inference. |
| `--compaction <trim\|summarize>` | `trim` | Choose how older context is reclaimed. |
| `--skill <name>` | unset | Preload a local skill (repeatable; accepted before or after the subcommand). |
| `--no-project-context` | off | Do not discover or send project `llms.txt`/`AGENTS.md` context or project skills. User skills remain available, including explicit `--skill` preloads. |
| `--no-workspace-evidence` | off | Do not register the model-callable `workspace_diff` evidence tool for this run (see [Tools in chat](#tools-in-chat)). The config key `workspace_evidence = false` disables it permanently. |
| `--no-artifacts` | config `artifact_storage`, else off | Create no new automatic command-output or oversized tool-result artifacts. Oversized output becomes a bounded, explicitly lossy inline preview; existing artifacts and explicit destinations remain unchanged. |

**Chat-only flags**

| Flag | Description |
|---|---|
| `--allow-path <PATH>` | Allow read-only filesystem tools under `PATH` (repeatable). See [Tools in chat](#tools-in-chat). |
| `--allow-write <PATH>` | Allow the model to **generate/write files** under `PATH` (repeatable) via `fs_write_file`. Deny-by-default, independent of `--allow-path`. |
| `--allow-read-write <PATH>` | Explicitly allow both reads and writes under `PATH` (repeatable) for read-modify-write workflows. |
| `--require-write <PATH>` | Fail a scripted or piped automation run unless the exact file receives a verified model filesystem mutation and its final contents differ from process launch (repeatable). This is a completion assertion, not a grant. A new target may use `--allow-write`; an existing target also needs read authority, normally via `--allow-read-write`, so safe replacement is possible. With `--output-schema`, `--max-rounds` must be at least 2: tool rounds omit the response format and one tool-free structured finalization round is reserved. |
| `--require-write-sha256 <PATH=SHA256>` | Add an exact raw-byte SHA-256 expectation to the same verified-write/net-change contract (repeatable). Input hex is case-insensitive and normalized lowercase; parsing uses the final `=`, so paths may contain `=`. It grants no authority, and a mismatch fails before `-o` is replaced. With structured output, a descriptor-verified mismatch may use remaining bounded tool rounds for correction before finalization. |
| `--enable-commands` | Enable guarded literal-argv developer commands. Approval is independent from filesystem access and `--yes`. |
| `--command-root <PATH>` | Add an existing writable root to mandatory Linux command containment (repeatable; implies `--enable-commands`). Exact argv/cwd approval remains a separate gate. **The approved executable's own directory is a second, independent root requirement** — if a tool lives outside every `--command-root` (e.g. `~/.local/bin/pytest`), containment denies it at run time; the denial names the exact missing directory and the `--command-root` flag to add. |
| `--allow-command <JSON_ARGV>` | Pre-approve one exact argv (repeatable). `'["cargo","test"]'` binds it to the launch directory; `'{"argv":["cargo","test"],"cwd":"/abs/dir"}'` binds it to an explicit directory inside a `--command-root`. The grant is exact argv **and** exact cwd. Every pregrant is also named verbatim (argv + cwd) in the system prompt, so the model can call it directly instead of guessing. |
| `--command-env <NAME[=VALUE]>` | Opt-in, explicit environment pass-through into contained commands (repeatable), layered on top of the fixed non-secret base set (`PATH`, `HOME`, `LANG`, `LC_ALL`, `TERM`, `TMPDIR`, `USER`, `LOGNAME`). Needed for interpreter-based toolchains (`pytest`, `npm test`, ...) whose module search path is env-driven (`PYTHONPATH`, `NODE_PATH`, ...). Bare `NAME` reads the value from kitsu's own environment and is refused for names that look secret-bearing (case-insensitive substring match on `KEY`/`TOKEN`/`SECRET`/`PASSWORD`); use `NAME=value` to pass an explicit literal value regardless of name. Nothing else is inherited from the host — deny-by-default is unchanged. Disclosed in `/permissions` and JSONL (secret-shaped values are redacted there even though the contained process receives the real value). |
| `--no-tools` | Offer no model tools for the whole chat. Project context, explicit skill preloads, client-side attachments, and session continuity remain available; `--allow-path` can authorize client-side reads, while write and command capability flags conflict. |
| `--script <FILE>` | Run turns from a file non-interactively (deterministic multi-turn). One turn per line; `#` comments and blank lines skipped; `/slash` commands work; a `<<< … >>>` block is a single multi-line turn. |
| `--resume [ID]` | Resume and autosave a saved session in interactive, scripted, or piped chat; omit the ID for `last`. Saved permissions are never restored; current invocation grants remain. `--resume` already implies `--session`, so passing both explicitly is a harmless no-op confirmation, not an error. |
| `--session` | Explicitly create and autosave a new scripted or piped session. |
| `--no-session` | Disable conversation snapshots and prompt-history persistence for this chat. |
| `--allow-turn-errors` | Let a script exit 0 after failed turns/commands; strict nonzero exit is the default. Accepts *partial* success only: if every turn and command failed, the script still exits nonzero. |

Examples:

```bash
kitsu                                                        # interactive chat
kitsu -p "Summarize the CAP theorem."                       # one prompt, then exit
KITSU_GATEWAY=ollama KITSU_MODEL=qwen2.5-coder:latest kitsu # reusable local defaults
kitsu --gateway openai ask "Summarize the CAP theorem."
kitsu --gateway anthropic --model claude-opus-4-8 ask --max-output-tokens 2000 "..."
echo "explain this error" | kitsu --gateway ollama chat        # one-shot via stdin
kitsu --gateway ollama chat "let's pair on a bug"              # interactive
kitsu --gateway ollama chat --no-tools "draft release notes"  # tool-free chat
kitsu --gateway ollama chat --session --script turns.txt       # durable script
kitsu --gateway ollama chat --resume last --script more.txt    # durable continuation
kitsu --gateway ollama review --base main --instructions "focus on API compatibility"
kitsu --gateway ollama --jsonl review --staged                 # machine-framed review
kitsu sessions --json                                          # provider-free inventory
```

---

## Tools in chat

In an interactive `chat` the model has filesystem tools (`fs_read_file`,
`fs_list_dir`, `repo_grep`, `fs_edit_file`, `fs_write_file`), gated by a **permission model**
similar to Claude Code — it asks before touching paths you haven't allowed:

Use `chat --no-tools` when the task needs conversation/context but no model
tools. Kitsu then omits filesystem, command, skill-loader, and agent tool schemas
from every request. Client-side project context, explicitly preloaded skills,
attachments, and durable sessions still work. In scripted or piped chat,
`--allow-path` remains available to authorize those client-side reads without
exposing a read tool to the model.

```
◉ permission — kitsu wants to write /home/you/project/notes.md
  [y] allow once · [a] always (/home/you/project) · [n] deny
>
```

**Default posture (Balanced):** reads inside the directory you launched from are
allowed automatically (explore freely); reads *outside* it, and *all* writes,
prompt for approval. `a` (always) adds that directory to the session allowlist so
it won't ask again.

- **Pre-grant** paths so they never prompt: `--allow-path <dir>` (reads),
  `--allow-write <dir>` (writes), or `--allow-read-write <dir>` (both). They are
  repeatable. Read and write authority stay independent unless the combined
  flag is used — e.g. read a whole repo but confine generated files to one
  scratch dir.
- **Host-owned evidence is not model authority.** `/diff`, `/review`,
  `kitsu review`, and `/checkpoint <id>` are produced by Kitsu's own `git`, not
  by a model tool, so they need **no** read grant and never create one. `/diff`
  renders to your terminal only. `/review` discloses exactly one bounded diff to
  the model and says so (`~ disclosing N bytes of host-verified evidence …; no
  read root granted`) — a one-shot disclosure, not standing authority to read
  anything else. `/permissions` shows the host-evidence root under
  `host evidence`. This is deliberately different from `@file` attachments,
  which *do* consume model read authority: an attachment can name any file, and
  the grant is what bounds which files can be named, whereas evidence scope is
  fixed by the repository Kitsu was launched in.
- **`workspace_diff` — the model can request that disclosure itself.** When
  chat starts inside a Git worktree, the model gets a `workspace_diff` tool so
  "explain my uncommitted changes" works in every chat mode without pasting a
  diff or granting a read root. Each call is the same bounded, host-gathered,
  Kitsu-private-excluding evidence as `/diff` for that scope
  (working/staged/unstaged/base/commit), surfaced as a normal tool receipt and
  announced with the same `~ disclosing N bytes …; no read root granted` line
  (a `workspace_evidence_disclosure` event in `--jsonl`). It is never a
  filesystem read: no read root is consulted, created, or widened. Opt out
  with `--no-workspace-evidence` or `workspace_evidence = false` in the
  config; the tool is then not registered at all. Design:
  [docs/workspace-diff-tool.md](docs/workspace-diff-tool.md).
- **In-session grants:** `/allow <path>`, `/allow-write <path>`, and
  `/allow-read-write <path>`. `/revoke-read <exact-root>` and
  `/revoke-write <exact-root>` remove a displayed root for the current session;
  a session reset restores invocation roots but never saved-session grants.
- **Non-interactive** (`--script`/piped): capabilities are absent unless their
  paths are pre-granted (or the session is interactive). `--yes` never
  registers a tool by itself and never approves developer commands. It also
  never removes confinement: unlisted paths are denied without prompting. The
  old unrestricted behavior requires the unmistakable
  `--dangerously-allow-all-filesystem` flag and still cannot bypass Kitsu-private
  path protections. For fully unattended automation,
  `--dangerously-skip-permissions` (short alias `--yolo`) additionally registers
  read/write tools and skips command approval prompts when command capability is
  separately enabled; it does not bypass the remaining guards named above.
- **Background agents** inherit current explicit/session grants but never own
  the foreground terminal and therefore cannot open filesystem or command
  approval prompts. Pre-grant the exact authority they need; an ungranted
  background operation fails closed without consuming stdin. Results not opened
  through `/jobs show` are printed before a session switch or interactive exit,
  after live jobs have been joined. Scripted JSONL emits unopened terminal work
  as `background_result`; an unseen failed job makes strict automation nonzero.
- Because background agents run **in-process**, nothing outside kitsu can see
  them. `[agents] state_file` publishes a small advisory JSON hint — one file
  per running process — that an outside tool such as a terminal multiplexer can
  read to tell whether this kitsu is `idle`, `working`, or `blocked`, with the
  same redacted thread records `/jobs` shows. `auto` (the default) writes only
  in an interactive terminal session, so scripted, piped, `ask` and JSONL runs
  write nothing at all; `on` always writes and `off` never does. It lives at
  `$XDG_RUNTIME_DIR/kitsu/<pid>.json` (falling back to `$TMPDIR/kitsu-<uid>/`
  then `/tmp/kitsu-<uid>/`), is created `0700`/`0600`, refuses a directory it
  does not own or that others can write, never follows a symlink standing in
  for it, is replaced atomically, refreshes at turn boundaries, and is removed
  on clean exit. Treat it as a hint, never a source of truth: it may be stale,
  absent, or left by a process that died, and writing it never affects a turn.
- `[agents] blocking` decides what a background agent does when a tool of its
  own needs permission you have not granted. The default, `fail_closed`, is
  today's behaviour: the operation is refused and the agent carries on, because
  a detached agent has nobody to ask. Set it to `ask` and the agent **parks**
  instead — it reports as `blocked`, appears first in `/jobs view` under
  `needs input`, and waits for you to answer, after which `allow once` or
  `allow always` applies exactly the grant the foreground prompt would have
  applied. Nothing is refused behind your back and no new kind of permission is
  created.

  `ask` only has an effect where a human could actually answer. Scripted,
  piped and JSONL runs never reach the question at all — their permission mode
  short-circuits before the route is consulted — so setting `ask` cannot change
  what automation does, by construction rather than by policy.

  `[agents] block_timeout_secs` bounds the wait, defaulting to `1800` (thirty
  minutes). If nobody answers in that time the agent ends as failed-interrupted
  rather than waiting forever, so an unattended run can never hang on a
  question. The unit is in the key name on purpose: a bare `block_timeout` is
  exactly where seconds and minutes get confused, and that spelling is rejected
  rather than guessed at.
- `[hooks] on_agent_state` runs a local command on each agent state change,
  exporting `KITSU_AGENT_ID`, `KITSU_AGENT_STATE` and
  `KITSU_AGENT_WAITING_FOR`. A background agent parked on a permission reports
  `STATE=blocked` and a `WAITING_FOR` of `read_file`, `write_file` or
  `run_command` — the kind it asked for, **never** the path or command line,
  since a hook runs an external program. The state file and
  `kitsu agents --json` use those same three words. It is executed **directly, never through a shell,
  with no expansion**, so config text cannot turn into shell execution; use a
  script, or write `sh -c` yourself. Its output is discarded so it cannot
  disturb kitsu's own rendering, kitsu never waits for it, and a failure,
  nonzero exit or timeout is ignored — a notification never costs a turn. It
  fires once per transition and works with or without `state_file`.
- Access is **deny-by-default** at the boundary; paths are canonicalized (writes
  canonicalize the parent), so `..`/symlink traversal is rejected. Generated
  files are capped at 1 MiB. `repo_grep` is always available with read tools on
  Unix and does not require `rg`: it searches UTF-8 files through pinned,
  no-symlink directory handles with deterministic ordering. `max_count` is a
  global matching-line limit (default 100, maximum 1,000); `literal: true`
  searches exact text. File count, scanned bytes, time, and output are also
  bounded, with an explicit truncation receipt when a limit is reached.
  Hidden entries and files over 1 MiB are skipped, matching the intended
  source-search surface. Non-Unix builds currently retain the fixed-argv `rg`
  fallback and omit the tool when `rg` is unavailable.
- Kitsu's config, snapshots, history, caches, and private runtime state remain
  protected from model reads and writes even when a broader parent root or the
  dangerous global bypass is active. Narrow Kitsu-managed artifact reads are
  listed separately by `/permissions` and cannot be revoked as user grants.
- Developer commands use a separate policy, literal argv (never a shell string),
  approved working-directory roots, bounded output, and timeout/cancellation.
  Enable them explicitly with `--enable-commands`. On Linux every approved
  process must also pass a fingerprinted Bubblewrap preflight: approved command
  roots are writable mounts, required runtime paths are read-only, network/PID/
  IPC/UTS namespaces are isolated, capabilities are dropped, and the child gets
  a fixed non-secret environment (`PATH`, `HOME`, `LANG`, `LC_ALL`, `TERM`,
  `TMPDIR`, `USER`, `LOGNAME`), optionally extended with explicit
  `--command-env` pass-through variables — never anything else inherited from
  the host. Missing, changed, or unsupported containment
  fails before prompting or launching; exact approval never substitutes for it.
  **Known limitation — pip/npm user-level installs:** the host home directory is
  never mounted inside containment (`HOME` points at an empty tmpfs), so
  toolchains installed per-user (e.g. `pip install --user pytest` under
  `~/.local`) are invisible even when the identical command passes on the host,
  and typically fail with `No module named ...` / `command not found`. Kitsu
  deliberately does not auto-mount `~/.local`: it can hold sensitive
  user-service data (e.g. `~/.local/share/keyrings`), and silently widening the
  sandbox would break the deny-by-default posture. When a contained command
  fails in that shape, the receipt and the model-visible result carry a
  host-authored hint naming the remedies; the recipe is explicit opt-in:
  `--command-root ~/.local` (makes the tree visible at its real path) plus
  `--command-env PYTHONPATH=$HOME/.local/lib/pythonX.Y/site-packages` (or
  `NODE_PATH`/`PATH` equivalents) so the interpreter can find it.
  `/permissions` numbers exact argv/cwd grants with invocation/runtime
  provenance; `/revoke-command <number>` removes one for the current session.
  A session grant is bound to the observed executable identity and is revoked
  if the executable or its `PATH` resolution changes before reuse.
  Windows command execution currently fails closed before approval until Kitsu
  has an equivalent containment backend and can reproduce loader-equivalent
  extension and executable-image validation.
  The completed protected JSONL artifact is capped at 16 MiB including framing
  and encoding expansion. Kitsu keeps draining after that ceiling and reports
  raw captured bytes, exact serialized bytes, and truncation; the model and
  JSONL event receive smaller bounded head/tail previews. A sink retains at most
  32 artifacts and 256 MiB of reserved/retained serialized data, pruning the
  oldest verified artifact before admitting a new one. `/artifacts` exposes the
  exact quota, usage, IDs, and explicit pruning. With `--no-artifacts` (or
  `artifact_storage = false`) no sink or generation is created; command
  stdout/stderr keep their bounded previews and typed byte/truncation metadata.

Enter `/` to open a bounded command palette below the current input. It filters
live as you type and Tab completes the selected prefix. Rich terminals use a
colored Unicode boundary; `NO_COLOR` retains the editor with an uncolored ASCII
boundary. Explicit plain/screen-reader input remains literal and therefore uses
`/help` instead of a cursor-controlled popup. The palette, completion, and
`/help` share the same command registry, so their descriptions stay in sync.
A persistent footer below the composer shows the active model and launch
directory; it follows `/model` changes immediately. `NO_COLOR` keeps this
footer with an ASCII boundary, while literal plain/screen-reader input omits it.
The palette and footer cap at 70 columns and shrink to the current terminal
width on redraw; narrow panes use compact syntax-only command rows instead of
wrapping descriptions.
Commands reject unexpected arguments and suggest close matches.

Slash commands inside the REPL:

| Command | Action |
|---|---|
| `/model <name>` | Switch model (keeps history). |
| `/models [filter]` | Preview the models the gateway offers (from its `models_path`), optionally filtered by a substring — e.g. `/models qwen`. The active model is pinned and marked; large catalogs show 12 rows plus a narrowing hint. Listed IDs can be passed to `/model`. |
| `/reasoning [none\|minimal\|low\|medium\|high\|xhigh\|default]` | Show or change reasoning effort for subsequent turns. |
| `/status` | Show one offline typed observation of the active route/model/generation, runtime/session identity, directory, effective authority, context occupancy, and current usage coverage with explicit truth/provenance. Output adapts across terminal, plain, screen-reader, and redirected destinations. |
| `/diff [working\|staged\|unstaged\|base REV\|commit REV\|checkpoint ID] [path]` | Show bounded host-verified evidence for an explicit scope. Host-owned: needs no read grant, and the bytes never leave your terminal. |
| `/review [working\|staged\|unstaged\|base REV\|commit REV\|checkpoint ID] [path]` | Review exactly that verified evidence in a no-tools turn. Discloses one bounded diff to the model without granting a read root. The top-level `kitsu review` provides the noninteractive form. |
| `/checkpoints` / `/checkpoint <id>` | List recoverable write checkpoints or preview one verified diff. |
| `/permissions` | Show effective filesystem authority with invocation/session provenance, revoked invocation roots, protected/internal reads, protected writes, and the command non-sandbox warning. |
| `/tools` | Show enabled tools and the effective permission behavior. |
| `/jobs view` | In the inline viewport this opens the **agent view overlay** (also `Ctrl+X a`, or ← twice on an empty composer); in classic it prints the grouped text deck. In the overlay, `↑`/`↓` move the selection (which follows the agent, not the row position, so a re-group cannot move your next keystroke onto a different one), `Space` peeks the selected agent and collects its result as `/jobs show` does, `r` replies to a blocked one, `x` stops it (twice; the second press prunes an already-finished agent, but never one whose result nobody has read), `n` renames it, `p` marks its result collected, `/` filters the rows (`s:<state> n:<text> o:<text> p:<profile>` — and the filter governs which agent the keys act on, not just what you see), `?` lists the keys and `Esc` closes. Pasted text is content in a name or correction line and is ignored elsewhere — it is never run as keystrokes. |
| `/tui [classic\|inline]` | Report or switch the live renderer between turns, keeping the conversation; bare `/tui` reports the renderer actually running. Refused during a turn. |
| `/allow <path>` | Grant read access to a path for the rest of the session (no more prompts there). |
| `/allow-write <path>` | Grant write access to a path for the rest of the session. |
| `/allow-read-write <path>` | Grant both read and write access for the rest of the session. |
| `/revoke-read <exact-root>` | Revoke an exact displayed read root for this session. |
| `/revoke-write <exact-root>` | Revoke an exact displayed write root for this session. |
| `/revoke-command <number>` | Revoke a numbered exact argv/cwd grant shown by `/permissions`. |
| `/skills [filter]` | List available local skills, optionally filtered by name. |
| `/skill <name>` | Load a skill's instructions into the conversation context. |
| `/save <file>` | Write the last response to `<file>` — just the fenced code blocks if present, else the whole message. |
| `/context [refresh]` | Show detailed context provenance, or refresh unpinned model metadata without discarding the current known window on failure. |
| `/compact` | Compact eligible older exchanges now and report the actual method and savings. |
| `/profiles` | List validated background-agent profiles with their instructions, requested reasoning effort, tool grant, and session-grant inheritance. Built-ins are a read-only `reviewer` and tool-free `reasoning` worker; both inherit the invocation's reasoning-effort policy rather than requesting one, so they never add a request field a gateway may reject. |
| `/agent [any /jobs subcommand]` | Inspect one bounded, redacted agent timeline; steer or stop that thread; acknowledge and close retained results. |
| `/jobs [list\|view\|explain ID\|show ID\|reply ID y\|a\|n\|steer ID TEXT\|close-steering ID\|stop ID\|stop-all\|prune ID\|prune all]` | Inspect bounded job/thread state, targeted steering, model/usage/tool/write activity, terminal handoff, joined cancellation, and pruning. `view` groups the same deck by state, agents needing input first, so one row does not hide among many. `explain ID` answers why one agent is in its state, since when, and what it is waiting on, and is read-only: unlike `show` it never acknowledges a ready result, so diagnosing an agent cannot quietly satisfy the unobserved-result exit guard. Steering **restarts** the worker with the correction on a fresh transcript — the receipt reports the elapsed work that discards; completed file changes are kept. `/agents` remains an alias: bare lists, a numeric argument shows that thread, and any other argument is forwarded to `/jobs`. |
| `/artifacts [list\|prune ID\|prune all]` | Inspect aggregate command-artifact quota/usage and explicitly prune retained evidence. Also reports what earlier runs left on disk; reclaim that with the host-only `kitsu artifacts`. |
| `/queue [list\|add\|remove\|edit\|move\|promote\|clear]` | Inspect and edit the bounded eight-prompt FIFO; `/queue <prompt>` remains shorthand for add. A single word one edit away from a subaction (e.g. `/queue lst`) gets a close-match suggestion instead of being silently queued; `/queue add <word>` queues it verbatim. |
| `/detach` | Leave the agent you attached from the agent view and return the composer to the session, answering `detached from agent N; it keeps running`. **Detaching stops nothing** — stopping is `x` in the view or `/jobs stop <id>`. Answers `not attached to a background agent` when there is nothing to leave. `←` on an empty composer and `Ctrl+X d` do the same thing silently, since the prompt label changing is their feedback. |
| `/steer <correction>` | While a response is running on Unix, cancel its partial turn, keep/report completed file effects, and restart it immediately with the correction before queued follow-ups. |
| `/retry` | Retry the last failed or cancelled prompt. Failed turns are removed from conversation history first. |
| `/new` | Autosave the current conversation and start a fresh session. |
| `/sessions [filter]` | List saved sessions, optionally filtered by a case-insensitive substring over ID, name, or preview. |
| `/resume <id\|last>` | Resume a saved session without restoring its old permissions. |
| `/fork [id]` | Clone a saved session to a fresh ID, then switch with fresh runtime permissions. |
| `/export <file> [markdown\|json]` | Export the active session without overwriting. If it has artifacts, also create `<file>.artifacts` and rewrite exact pointers to that sidecar. |
| `/archive <id>` | Move an inactive session into recoverable private archive storage. |
| `/restore <id>` | Restore an archived session. |
| `/delete <id>` | Permanently delete an exact inactive live-session ID. |
| `/undo [id]` | Restore the latest or selected model-write checkpoint only when its files still match. |
| `/clear` | Save the current durable conversation, start a fresh session/context, and keep the prior ID resumable. With autosave disabled, clear memory with an explicit non-preservation warning. |
| `/help` | Show help. |
| `/exit [wait\|cancel]` (or `/quit`) | Exit only when jobs are done, explicitly wait, or immediately cancel the foreground plus queued/background work and join it first. |

### Coding workflows: paste & file generation

- **Paste** a multi-line code block or stack trace straight into the prompt — it
  is captured as one turn (bracketed paste). Non-interactively, a `<<< … >>>`
  block in a `--script` file is the equivalent single multi-line turn.
- **Generate files** two ways: let the model call `fs_write_file` (grant it with
  `--allow-write <dir>`), or ask for code and run `/save out.rs` to write the
  last response's code blocks yourself.
- **Edit existing files** with explicit readback authority by using
  `--allow-read-write <dir>` (or separate read and write roots).
- **Attach files** with `@path`, `@path:line`, or `@path:start-end` (also
  `@path#Lstart-Lend`); sentence punctuation and quoted paths such as
  `@"docs/my guide.md":10-20` are accepted. Expansion is
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
back, and return exit `1` (command/input/auth setup, tool failure, or local
policy denial) or `2` (provider, truncation, or exhausted rounds). A tool error
the model recovers from within the same turn (for example an edit-precondition
conflict followed by a corrected, successful edit) does not fail the turn: the
turn is judged by its final tool round, and only policy denials always fail.
An empty or whitespace-only final assistant response is also incomplete: strict
ask/review/piped/script automation discards it and exits `2` instead of claiming
success, while interactive chat rolls the turn back and keeps `/retry`
available. Completed file effects remain inspectable after either rollback.
Use `--allow-turn-errors` only when partial success is intentional; it accepts
partial success, so a script where everything failed still exits nonzero.
The human run receipt calls completed model turns and slash commands
"successful operations" and reports failures separately, so a query-only
script is never mislabeled as having completed a model turn. JSONL repeats the
exact count in `final_summary.successful_operations`; `/quit` remains control
flow rather than a successful operation, and older summaries without the
additive field mean that the count is unknown.

For automation, `--jsonl` emits one `kitsu.chat.v1` object per stdout line:
`run_started`, framed turn/command events, provider/tool/usage events, assistant
output, typed errors, and exactly one `final_summary`. Human stderr is not needed
to determine the outcome. Unknown scripted slash commands keep the safe
host-authored closest-command or `/help` hint in their typed error message.
The final summary includes `successful_operations` and `duration_ms`, measured
monotonically from process entry through summary emission, on success, error,
and cancellation. Its `*_failures` fields count failed turns/operations, while
`tool_errors`/`policy_errors` count individual calls and the corresponding
`recovered_*` fields expose errors from turns that still committed. Provider
streaming remains enabled unless
`--no-stream` is set; text arrives in ordered, JSON-escaped `assistant_delta`
records whose concatenation matches the final assistant event. Treat it as
provisional until `assistant_disposition` says `committed`; strict rollback
closes it as `discarded`. When an explicit `--max-output-tokens N` caused the
rollback, the disposition/error names `N` and suggests `2N`; a gateway-owned
limit instead asks for an explicit larger value. A deadline that expires
mid-answer in a one-shot run closes it as `committed_partial` (kept, marked,
still exit `2`). A
`context_window_mismatch` event reports runtime evidence that the server's
real context window is smaller than the resolved one. `run_started`
includes the complete bounded provider-route receipt used at run start; each
`provider_attempt.route_generation` can therefore be correlated to that
immutable identity without exposing an endpoint, header, credential, or body.
It also distinguishes disabled, pending, and
resumable session snapshots; `final_summary` repeats the terminal state and
returns a non-null session ID only for an installed, resumable snapshot.
Top-level `kitsu review --jsonl` also emits one `review_disclosure` between
`run_started` and `turn_started`, so consumers can audit the bounded evidence
scope and byte count without parsing the raw prompt; the receipt explicitly
records that no model read root was granted.
For downstream automation that needs only the final answer, add
`-o/--output-last-message <PATH>`. Kitsu atomically replaces that separate,
host-owned file with the exact bytes of the last fully committed assistant
message while leaving normal stdout or JSONL untouched. A multi-turn script
selects its last committed turn; discarded, cancelled, blank, length-limited,
and `committed_partial` output does not qualify, so an existing destination
stays unchanged when no full commit exists. The parent must already exist and
must not be symlinked; an existing destination must be a regular file. JSONL
emits one `output_last_message` receipt after the selected turn and before
`final_summary`, with destination, turn id, byte count, SHA-256,
atomic/commit facts, explicit `model_write_authority: false`, and no duplicate
message body. The destination is protected from model filesystem tools and
cannot overlap Kitsu's config or private state. If a required-write
postcondition blocks installation after a committed answer, the destination
remains unchanged and JSONL reports
`output_last_message_state: "preserved_postcondition_failure"`; a destination
that did not exist remains absent. Add
`--output-schema <PATH>` when the final answer must be machine-validated JSON.
The source must be a regular, non-symlink file no larger than 64 KiB containing
a strict object schema (`additionalProperties: false`, with every property
required). Kitsu validates the schema before provider contact, sends it as a
native `json_schema` response format, then parses and validates the exact final
assistant text locally before committing the turn or writing `-o`. Markdown
fences, malformed JSON, and schema mismatches are discarded with exit `2`; an
existing final-message destination stays unchanged. JSONL emits one body-free
`output_schema` receipt after `run_started` and before `turn_started`, while
`provider_request.structured_output` confirms which requests carried the
constraint. The schema file is excluded from filesystem and command authority;
command-enabled runs must place it outside command and runtime roots.
Inference and compaction `provider_request` records also identify the exact
`model` used for each logical request, including after `/model`; catalog
requests omit it because they are not routed to a model.
Schema-rejection errors expose the schema SHA-256, failing instance path, and
validation keyword, and `final_summary.output_schema_failures` lets summary-only
consumers distinguish local acceptance failure from provider transport failure.
For tasks whose success also requires a file effect, add repeatable
`chat --require-write <PATH>`. Kitsu discloses the exact resolved targets to the
model, but the flag grants no authority: each path must already be inside an
explicit write root. A target that exists at launch must also have explicit
read authority (normally `--allow-read-write`); Kitsu rejects an impossible
write-only replacement before contacting the provider. An absent target may
still use `--allow-write` alone. After all foreground/background work is joined, Kitsu
requires both a verified filesystem-tool journal entry and final contents that
differ from process launch and still match the latest verified write. Failure
exits `1` even under `--allow-turn-errors`, increments
`final_summary.postcondition_failures`, and leaves `-o` unchanged. JSONL emits
an armed and an evaluated `completion_postcondition` receipt; successful
evaluation is ordered before `output_last_message`.
For byte-exact artifacts, use repeatable
`--require-write-sha256 <PATH=64HEX>`. It implies the same journaled mutation,
net-change, authority, and existing-target read requirements, then compares the
exact final raw bytes—without text, newline, or encoding normalization—to the
normalized lowercase SHA-256. The final `=` is the delimiter, so the path may
contain `=`. Conflicting expectations for one resolved path, malformed digests,
and an existing target whose launch digest already equals the expectation are
rejected before provider contact. JSONL adds an `expectations` list plus
per-result `expected_sha256` and `expected_sha256_matches`; historical v1
consumers may ignore these additive fields. A mismatch is the existing typed
postcondition/user failure (exit `1`), cannot be waived by
`--allow-turn-errors`, preserves any verified on-disk effect for inspection,
and never replaces `-o`. In a durable run the already committed transcript may
remain resumable for recovery even though the run-level `final_summary` is an
error.
When `--output-schema` and either required-write flag are combined, Kitsu separates the
two jobs within each bounded model turn. Tool-capable action requests omit
`response_format`; after the model stops calling tools, Kitsu withholds tools
and reserves the next round for the provider-native strict-schema final answer.
`provider_request` therefore exposes `structured_output: false` during action
and `true` for finalization. Before spending that finalizer, an exact-SHA
mismatch already proven from the active descriptor-pinned write journal keeps
tools available for another action round whenever one repair round plus the
finalizer still fit. JSONL emits `structured_finalization_deferred` with the
expected/actual digests; its host correction grants no authority. If the budget
is exhausted, Kitsu preserves the original finalization-plus-fail-closed path.
The provisional action response is not rendered or committed; it, any host
repair instruction, and Kitsu's finalization nudge are provider-only and are
never saved or counted as another user turn.
Real tool calls/results and the committed final assistant remain in resumable
history. Both phases share the existing turn deadline/max-round budget, and the
host still rechecks every required write before installing `-o`. If the
model returns a native tool call after tools were withheld, Kitsu executes no
such call and emits the safe typed cause
`tool_call_during_tool_free_finalization` with exit `2`.
Status, permissions, context, diff, model,
reasoning, compaction, session/checkpoint inventory, skills, profiles, command
artifacts, help/tools, and job/thread slash commands emit typed
`command_result` objects.

`Ctrl+C` cancels an in-flight response and makes it available to `/retry`; at an
idle prompt it clears input. Automation emits a structured cancelled event and
exits `130`. `Ctrl+D` or `/exit` does not silently discard live background agents,
and neither does the end of a `--script` or piped run: it joins them the way
`/exit wait` does, bounded by `--agent-join-timeout` (default `--timeout-secs`).
Only agents still running when that bound expires are cancelled, and they are
reported as cancelled by shutdown policy — never as casualties of a rolled-back
turn.
Completed filesystem side effects are never pretended away after cancellation—
inspect them with `/diff` or restore a safe checkpoint with `/undo`. Interactive
type-ahead collection is Unix-only: complete lines entered during generation are
captured in an eight-prompt visible FIFO. A line beginning `/steer ` instead
targets the active turn and never leaks into a later prompt. On other platforms `/queue <prompt>`
still works, but Kitsu does not actively collect terminal type-ahead while a turn
is running.

For stable ASCII output and literal input, pass `--plain` or set
`KITSU_PLAIN=1`. For an explicit assistive-technology contract, pass
`--screen-reader` or set `KITSU_SCREEN_READER=1`. `TERM=dumb` selects the same
plain-rendering, literal-input behavior. In these modes, renderer-owned banners,
progress, tool activity, context meters, and permission prompts contain no
styling escapes, while arbitrary safe Unicode in model output and paths is
preserved. The interactive prompt is a canonical line reader with no raw mode,
cursor control, bracketed-paste negotiation, completion, or history editing;
asynchronous notices wait for the submitted line instead of splitting typed
input. Literal input is intentionally line-oriented, so each newline submits a
turn rather than preserving a pasted multi-line draft as one prompt.

If only color should be disabled, set `NO_COLOR=1`. It keeps plain rendering but
retains the rich Rustyline editor, including history, completion, and bracketed
multi-line paste. Pass `--reduced-motion` or set `KITSU_REDUCED_MOTION=1` to
replace the animated wait indicator with a visible static status.

Pass `--tui inline`, or set `[tui] mode = "inline"` in your config, to opt into
the **inline viewport**: a preview renderer that pins a status row to the bottom
of the terminal and lets the transcript scroll above it, instead of repainting
one status row in place. The flag wins over the config key, and the config key
wins over the default — **no environment variable selects it**. The default is
`classic`, and a session that does not ask for the viewport is byte-identical to
one from before it existed.

A mistyped mode is an error that names the key and lists what it accepts, rather
than quietly falling back to classic; `fullscreen` is named in the design but is
**not accepted yet**. The rest of the `[tui]` table:

| key | default | meaning |
|---|---|---|
| `mode` | `classic` | `classic` or `inline`; `--tui` overrides it |
| `alternate_screen` | `auto` | `auto`, `always` or `never`; `--no-alt-screen` forces `never` |
| `prefix` | `"ctrl+x"` | the chord prefix for viewport commands |
| `title` | `true` | set and restore the terminal window title |
| `left_arrow_opens_agents` | `true` | ← opens the agent pane when the composer is empty |
| `cell_ring_lines` | `10000` | scrollback the viewport retains, `1`–`1000000` |

Keys not yet consumed by a shipped surface are parsed and validated now so the
spelling cannot change under you later. Note that adding a `[tui]` table makes
**older** kitsu builds reject the config file, since they do not know the key.

It is a preview, but the idle prompt inside it now has most of what the classic
one does, built on kitsu's own editor rather than Rustyline: command history
persists across sessions, Ctrl+R reverse search works, Tab completes a slash
command from the hint beside your draft, and typing a slash prefix opens the
**bordered command palette** above the composer — it grows and narrows as you
type, shrinks to fit a short window, and closes when the prefix does. The pinned row itself is a live status row — model, an eight-cell context meter
with its percentage, elapsed turn time, `[N queued]`, and the background-agent
tag — refreshed while a turn runs and never wrapping, at any width from 40 to 200
columns. When the terminal is too narrow for all of it, segments drop in a fixed
order: the meter goes first, then elapsed, then the model (truncated before it is
dropped), then `[N queued]`, and the agent tag is last to go, because a blocked
agent is the one thing there you have to act on. `@file`
path completion works here too, offering the same candidates in the same order
as the classic prompt — both read from one completion source, so they cannot
drift apart. Still missing: syntax highlighting of the draft. What it does
have:
insertion and cursor motion, Home/End, Backspace/Delete, the Ctrl+U/K/W kills,
Up/Down through the current session's history, bracketed paste kept as one
draft, resizing the window (the pinned row follows, and the viewport steps
aside entirely if you shrink the terminal below three rows, coming back when
you grow it again), Ctrl+D to exit on an empty line, Ctrl+C to clear the line,
and Ctrl+Z to suspend — which releases the pinned row and hands the shell back an ordinary
terminal, then restores the row and your unsubmitted draft on `fg`. Ctrl+\ is
deliberately inert: its usual action dumps core, and this process holds your API
credentials and conversation in memory. Everything else — every tier, every byte of non-TTY, `--plain`,
assistive and JSONL output — is unaffected, because the viewport requires a
styled interactive terminal.

Background-agent notices print between prompt cycles by default: rendering
them live at an idle prompt requires a line-editor hook that measurably
degrades burst-input processing (pastes), and kitsu prioritizes input
latency. Set `KITSU_LIVE_NOTICES=1` to opt into live idle notices anyway.

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
  succeeds, the full result remains available outside the prompt. Under
  `--no-artifacts` / `artifact_storage = false`, the same boundary instead keeps
  a UTF-8-safe bounded head/tail preview with explicit byte counts and no
  recoverable pointer. When history
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
On Unix, discovery and reads are descriptor-relative and reject symlink
retargeting. Platforms without that secure traversal primitive fail closed and
do not load implicit project files.

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
  `SKILL.md` files are ignored rather than widening read authority. User skills
  win duplicate names; `/skills` reports origins, content hashes, bounded
  omissions, and ignored duplicates. Discovery admits at most 64 skills,
  256 KiB per manifest / 1 MiB of manifests globally, and a 16 KiB rendered
  index.
- **On demand:** only the index (`name — description`) is always in context; the
  model loads a skill's body by calling the `skill` tool when a task matches, or
  you run `/skill <name>`. `/skills [filter]` lists them. `--skill <name>`
  preloads one up front (works for `ask` too, for spawned agents, and with
  `--no-project-context`). A project skill's `always: true` body still requires
  an explicit load; only user-controlled always skills are injected up front.
- **Bundled files & links:** a skill dir can ship UTF-8 resource files
  (checklists, templates). Discovery records bounded identity metadata, then an
  explicit load reads through no-follow descriptors and verifies identity/drift
  before framing the content (16 files / 64 KiB per skill; 128 files / 512 KiB
  globally). SHA-256 source/resource snapshots make later changes explicit;
  loading a skill never widens filesystem authority. A skill's `links:` are
  surfaced to you but **never auto-fetched**.
- **Local only, by design:** skills are trusted because *you placed them there* —
  kitsu never fetches them from the network (injecting internet instructions into
  a tool-enabled agent is exactly what the permission model prevents). To use one
  from GitHub, `git clone` it into `~/.kitsu/skills/`. A skill's text is context,
  not privileged: every action it suggests still goes through permissions.
- Disable project context and project-skill discovery with
  `KITSU_NO_PROJECT_CONTEXT=1`; user skills remain available. Filesystem skill
  discovery currently fails closed on non-Unix platforms where Kitsu cannot pin
  every path component during the read.

---

## Notes & gotchas

- **`--max-output-tokens` for some gateways.** If a gateway requires `max_tokens`
  (Anthropic does), omitting it may 400 the request, or the gateway's own small
  default (often 4096) may truncate long outputs. Pass the flag for long-form
  generation. If an explicit cap is reached, Kitsu prints the partial answer as
  provisional, rolls the turn back, exits `2`, and gives an exact doubled retry
  value; JSONL retains the same `discarded` disposition and recovery reason.
- **`--temperature` is opt-in.** It's omitted from the request unless you set it,
  because some models reject a `temperature` field.
- **Local servers use `method = "none"`.** Ollama and vLLM (unless launched with
  `--api-key`) need no token.
- **Token usage** is shown as a compact human status line in a terminal. When
  output is redirected, it remains a stable `kitsu-usage: {...}` line on
  **stderr**, so stdout stays clean for piping and scripts remain compatible.
- **Model-run timing** ends inference-capable commands with elapsed time. A
  terminal gets a compact secondary row; redirected stderr gets stable
  `kitsu-timing: {"duration_ms":...,"status":"..."}` JSON. Status is `ok`,
  `partial`, `error`, or `cancelled`; `partial` identifies a script accepted by
  `--allow-turn-errors` after both successful and failed operations.
  Provider-free management commands do not gain an unrelated timing footer.
- **Exit codes:** `0` success (including accepted partial scripts with
  `--allow-turn-errors`), `1` CLI/input/config/missing-auth or other user-policy
  failure (including local tool failures and policy denials), `2`
  provider/HTTP/timeout or strict incomplete turn (length limit or exhausted
  rounds), and `130` Ctrl+C cancellation. A consumer that closes kitsu's stdout
  early (`kitsu sessions | head -1`) is not a failure: kitsu stops quietly and
  exits `0` instead of unwinding with the Rust panic code `101`.
- **Platform hardening:** Unix uses descriptor-bound command working directories,
  process-group cancellation, and descriptor-relative no-follow file/session
  operations. Portable branches retain validation and direct-child cancellation,
  but cannot close every pathname race or terminate an entire descendant process
  group. Implicit project context, filesystem skills, and `@file` attachment
  reads/completion fail closed when secure component traversal is unavailable;
  unsupported atomic primitives likewise fail closed.
