# User guide

Kitsu supports OpenAI-compatible chat completions and the native Anthropic
Messages API through configured gateway profiles. Use `kitsu --help` and
`kitsu chat --help` for the flags available in your installed version.

[Quick start and documentation index](../README.md).

## Contents

- [Install](#install)
- [Quick start](#quick-start)
- [Gateway profiles](#how-it-works-gateway-profiles)
- [Configuration and state](#where-the-config-lives)
- [Provider recipes](#provider-recipes)
- [Authentication](#authentication)
- [CLI reference](#cli-reference)
- [Tools and permissions](#tools-in-chat)
- [Automation](#automation)
- [Terminal modes](#terminal-modes)
- [Context window](#context-window)
- [Skills](#skills)
- [Troubleshooting and exit codes](#troubleshooting-and-exit-codes)

## Install

### Prebuilt packages

Every [release](https://github.com/KateInTheLoop/kitsu/releases) ships packages
for Linux (x86_64 / aarch64), macOS (Apple Silicon, Intel, universal) and
Windows (x86_64). The Linux binaries are statically linked against musl, so the
`.deb` and `.rpm` have no dependencies and install on any reasonably modern
distro.

The following examples install v0.2.0. Choose the matching version and
architecture from the release assets when installing another release.

```bash
# Debian / Ubuntu
curl -LO https://github.com/KateInTheLoop/kitsu/releases/download/v0.2.0/kitsu_0.2.0-1_amd64.deb
sudo dpkg -i kitsu_0.2.0-1_amd64.deb

# Fedora / RHEL / openSUSE
sudo dnf install https://github.com/KateInTheLoop/kitsu/releases/download/v0.2.0/kitsu-0.2.0-1.x86_64.rpm

# macOS / any Linux — tarball
curl -L https://github.com/KateInTheLoop/kitsu/releases/download/v0.2.0/kitsu-v0.2.0-universal-apple-darwin.tar.gz | tar xz
sudo install -m755 kitsu-v0.2.0-universal-apple-darwin/kitsu /usr/local/bin/kitsu
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

Use a current stable Rust toolchain:

```bash
cargo build --release --locked
# binary at target/release/kitsu
```

For the examples below, either put `kitsu` on your `PATH` or run
`target/release/kitsu`.

---

## Quick start

kitsu ships with a ready-to-use [`kitsu.toml`](../kitsu.toml) that defines several
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

Adding another profile for either compiled adapter is just adding a block.
Adding a genuinely different wire dialect requires a compiled Kitsu adapter
and the shared conformance suite; configuration cannot load provider code.

---

## Where the config lives

Config lookup uses `--config PATH`, then `KITSU_CONFIG`, then
`~/.kitsu/config.toml`. To install the repository's example config:

```bash
mkdir -p ~/.kitsu
cp kitsu.toml ~/.kitsu/config.toml
```

`kitsu init` creates a credential-free starter without replacing an existing
file. Use `--path` or `--config` to select its destination.

### Diagnostics and model discovery

`kitsu doctor` checks configuration, credential presence, context provenance,
state-directory access, and tool policy locally. It changes no state and
prints no credential values.

- `doctor --online` checks the model catalog; it does not verify inference.
- `doctor --inference` sends one minimal, tool-free completion and may incur
  provider charges. It cannot be combined with `--online`.
- `kitsu models [filter]` and `/models [filter]` list advertised model IDs.
  Terminal output previews 12 rows with the active model pinned; redirected
  stdout and JSONL contain the full catalog.

### Saved sessions

Interactive chat saves history and conversation snapshots under private
`~/.kitsu/` state. A project-local config selects an isolated profile there.
The full conversation and tool transcript are saved; use `--no-session` when
that text should not persist. Config and private state are excluded from model
file tools and verified diffs.

Scripted and piped chat are stateless unless `--session` creates a new
conversation or `--resume ID` restores one. Successful turns are saved as they
complete; a fresh session is not published before durable work exists.
`--resume last` prefers the newest conversation with a real user turn.

Resume rebuilds system context from current configuration and tools, refreshes
saved skill loads, and uses current invocation grants rather than saved
permissions. `/clear` saves the old durable conversation and starts a fresh
one; with `--no-session`, it only clears memory.

Session management works without a provider or a parseable config:

```bash
kitsu sessions
kitsu sessions show last --json
kitsu sessions fork last
kitsu sessions export last conversation.md
kitsu sessions archive <exact-id>
kitsu sessions restore <exact-id>
kitsu sessions delete <exact-id>
kitsu sessions resume last --json  # prints resume argv outside a TTY
```

Archived sessions leave the live inventory and must be restored before resume.
Forking creates a new ID, leaves the source intact, starts unnamed, and copies
no permission grants.

### Artifacts on disk

Command output and offloaded tool results can remain in private artifact
storage after a run. Inspect or reclaim retained generations without a provider:

```bash
kitsu artifacts list --json
kitsu artifacts prune all --dry-run
kitsu artifacts prune all
kitsu artifacts prune <id> --older-than 7d
```

Pruning skips live generations and session-owned sidecars. Startup removes only
provably empty orphan generations and reports accumulated non-empty ones.

`--no-artifacts` (or top-level `artifact_storage = false`) disables new
automatic artifacts. Oversized results then become bounded, lossy inline
previews with byte counts and no recoverable pointer. Existing artifacts are
left intact. This does not disable sessions, explicit model writes, exports,
schemas, or `-o`; add `chat --no-session` for session-stateless automation.

## Provider recipes

The bundled [`kitsu.toml`](../kitsu.toml) contains these profiles. Choose a
model served by the endpoint; `kitsu --gateway NAME models` lists IDs when the
profile has a catalog route.

| Profile | Base URL | Authentication |
| --- | --- | --- |
| `ollama` | `http://localhost:11434` | None |
| `vllm` | `http://localhost:8000` | None, unless the server requires an API key |
| `huggingface`, `qwen` | `https://router.huggingface.co` | `HF_TOKEN` |
| `openai` | `https://api.openai.com` | `OPENAI_API_KEY` |
| `anthropic` | `https://api.anthropic.com` | `ANTHROPIC_API_KEY` |
| `custom` | Your endpoint | `CUSTOM_GATEWAY_TOKEN` |

The compatible profiles use `/v1/chat/completions`. For another endpoint,
copy the [gateway profile example](#how-it-works-gateway-profiles), set its
base URL, paths, model, and authentication, then select it with `--gateway`.
Use an empty `models_path` when no catalog route exists.

Store a hosted credential with `kitsu --gateway NAME auth login`, or set its
configured environment variable. Provider pricing, model availability, and
account access are managed by the provider.

### Local model discovery

With no `[gateways.local]` profile, `--gateway local` probes Ollama (11434),
LM Studio (1234), llama.cpp (8080), and vLLM (8000), in that order. It uses the
first server that lists models and defaults to its first model.

```bash
kitsu --gateway local models
kitsu --gateway local --model MODEL_ID
kitsu --gateway remote              # select default_gateway
```

Without `--gateway`, `--model` routes to a local model on an exact or unique
substring match, otherwise to the default gateway. Ambiguous substrings fail
with the matching IDs. A configured `[gateways.local]` profile takes precedence
and disables this automatic model routing. Use the exact ID reported by the
server, including any tag.

If local Ollama is unreachable, check `ollama serve`, the selected profile,
and its port. Use `doctor --online` to check catalog reachability.

### Native Anthropic Messages

Select the compiled Messages adapter explicitly. Replace `MODEL_ID` with a
model available to your API account:

```toml
[gateways.anthropic]
flavor = "anthropic"
adapter = "anthropic-messages"
base_url = "https://api.anthropic.com"
chat_path = "/v1/messages"
models_path = ""
model = "MODEL_ID"
auth = { method = "bearer-env", env = "ANTHROPIC_API_KEY", header = "x-api-key", scheme = "", expires_env = "" }
```

```bash
kitsu --gateway anthropic auth login
kitsu --gateway anthropic ask --max-output-tokens 2000 "Explain a mutex."
```

The adapter supplies the required `max_tokens` field: an explicit
`--max-output-tokens` value, or 4096 when unset. It supports streaming
Messages; model catalogs, structured output, image input, and prompt caching
are not exposed by this adapter.

### Ollama in Docker

```bash
docker run -d --name ollama -p 11434:11434 -v ollama:/root/.ollama ollama/ollama
docker exec ollama ollama pull llama3.2
kitsu --gateway local --model llama3.2 ask "Explain a mutex."
```

The volume retains downloaded models. Kitsu connects through Ollama's
compatible API; no Kitsu component runs inside the Ollama container.

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
| `agents [--all] [--json]` | List published per-process agent state without a provider. `--all` includes dead processes. Unsupported schemas and invalid records are reported; the reader changes no state. |
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
| `--tui <classic\|inline\|fullscreen>` | `[tui] mode`, else inline on Unix interactive terminals | Choose the terminal renderer. Without an explicit choice, plain and non-terminal output use classic. See [Terminal modes](#terminal-modes). |
| `--no-tui` | off | Select the classic line renderer; shorthand for `--tui classic`. |
| `--theme <NAME>` | config `theme`, else `kitsune` | Choose a color theme; `/theme` lists available themes and changes it during chat. |
| `--temperature <f>` | unset | Sampling temperature. Omitted from the request unless set (some models reject it). |
| `--reasoning-effort <level>` | unset | Forward `none`, `minimal`, `low`, `medium`, `high`, or `xhigh` to compatible gateways; omitted otherwise. `/reasoning` can change it between chat turns. |
| `--effort <low\|medium\|high\|max>` | `KITSU_EFFORT`, else unset | Set round, deadline, and output-token budgets together. Explicit budget flags take precedence. This is separate from `--reasoning-effort`; see [Effort presets](#effort-presets). |
| `--max-rounds <n>` | `10` | Max agent tool-call rounds per turn (minimum `1`). Raise (e.g. `40`) for tool-heavy work. |
| `--timeout-secs <n>` | `120` | Inference deadline including retries, backoff, and body reads. Scripted/piped turns share it across provider/tool rounds and input acquisition. A deadline-partial answer may remain visible, but the run exits `2`. |
| `--agent-join-timeout <n>` | `--timeout-secs` | How long a non-interactive exit (scripted or piped chat) waits for live background agents to finish before cancelling them. |
| `--agent-spawn-cap <n>` | `8` interactive, `2` automation | Per-turn cap shared by synchronous, parallel, and background spawns. `0` disables agent tools. |
| `--max-output-tokens <n>` | Gateway default; Messages adapter `4096` | Cap response tokens. A strict run that reaches the limit keeps text provisional, rolls back the turn, and exits `2`; increase the cap and retry. |
| `--yes` | off | Grant the launch directory read+write and enable chat filesystem tools without prompts. Requests outside current roots are denied. Command approval remains separate. |
| `--dangerously-allow-all-filesystem` | off | **DANGER:** let enabled filesystem tools access every non-protected host path without prompting. This is the explicit global bypass; it still never enables tools or approves commands. |
| `--dangerously-skip-permissions`, `--yolo` | off | **DANGER:** enable unattended filesystem work and auto-approve enabled commands. Filesystem tools gain access to non-protected host paths; private-state exclusions, explicit protection guards, argv validation, and command containment remain enforced. |
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
| `--require-write <PATH>` | Require a verified model write whose final contents differ from launch (repeatable). Grants no authority; see [Automation](#automation) for required roots and structured finalization. |
| `--require-write-sha256 <PATH=SHA256>` | Add an exact raw-byte SHA-256 expectation to the required-write contract. Split on the final `=`; a mismatch fails the run and preserves `-o`. See [Automation](#automation). |
| `--enable-commands` | Enable guarded literal-argv developer commands. Approval is independent from filesystem access and `--yes`. |
| `--command-root <PATH>` | Add an existing writable directory to OS command containment (repeatable; implies `--enable-commands`). With no explicit root, commands use the launch directory. Exact argv/cwd approval remains separate; toolchains outside the standard runtime paths may need a read-only runtime root. |
| `--command-runtime-root <PATH>` | Make an extra toolchain directory readable inside containment without granting writes (repeatable; implies `--enable-commands`). Each root's `bin/` joins `PATH`; `.cargo` and `.rustup` roots also set `CARGO_HOME` and `RUSTUP_HOME`. |
| `--allow-network` | Allow contained commands to use the network; implies `--enable-commands`. Commands still require approval and remain confined to command roots. SSH host configuration becomes readable; private keys remain excluded. |
| `--allow-command <JSON_ARGV>` | Pre-approve exact argv and cwd (repeatable). Use `'["cargo","test"]'` for the launch directory or `'{"argv":["cargo","test"],"cwd":"/abs/dir"}'` for another command root. Grants are disclosed to the model. |
| `--command-env <NAME[=VALUE]>` | Add an environment variable to the contained command (repeatable). Bare `NAME` inherits a non-secret host value; names containing `KEY`, `TOKEN`, `SECRET`, or `PASSWORD` require an explicit `NAME=value`. Matching is case-insensitive. Secret-shaped values are redacted in permissions/JSONL output. |
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
kitsu --gateway anthropic ask --max-output-tokens 2000 "Explain a mutex."
echo "explain this error" | kitsu --gateway ollama chat        # one-shot via stdin
kitsu --gateway ollama chat "let's pair on a bug"              # interactive
kitsu --gateway ollama chat --no-tools "draft release notes"  # tool-free chat
kitsu --gateway ollama chat --session --script turns.txt       # durable script
kitsu --gateway ollama chat --resume last --script more.txt    # durable continuation
kitsu --gateway ollama review --base main --instructions "focus on API compatibility"
kitsu --gateway ollama --jsonl review --staged                 # machine-framed review
kitsu sessions --json                                          # provider-free inventory
```

### Effort presets

`--effort` changes execution budgets, while `--reasoning-effort` sends a
reasoning hint to the provider. The budget presets are:

| Preset | Tool rounds | Timeout (seconds) | Output tokens |
| --- | ---: | ---: | ---: |
| `low` | 10 | 120 | Gateway default |
| `medium` | 20 | 180 | 8192 |
| `high` | 40 | 300 | 16000 |
| `max` | 100 | 600 | 32000 |

`low` leaves the ordinary defaults unchanged. Set `KITSU_EFFORT` for a
reusable preset; explicit `--max-rounds`, `--timeout-secs`, and
`--max-output-tokens` override their respective preset values.

---

## Tools in chat

Interactive `chat` offers filesystem tools (`fs_read_file`, `fs_list_dir`,
`repo_grep`, `fs_edit_file`, `fs_write_file`). The launch directory is granted
read+write access; ungranted paths ask for permission.

The `git` tool is available by default in interactive chat. It can inspect
status/diffs and create branches or commits after exact argv/cwd approval.
`--no-tools` disables it; `--enable-commands` also exposes the general command
runner. Git uses the same OS containment as developer commands; network
access requires `--allow-network`. In automation, pre-approve exact invocations
with `--allow-command`; filesystem grants alone do not authorize Git.

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

Reads and writes inside the launch directory are allowed automatically.
Requests outside current roots prompt for approval. `a` (always) adds the
requested grant directory to the session allowlist for that access kind.

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
  config; the tool is then not registered at all. See
  [verified review](verified-review.md#workspace-diff-tool).
- **In-session grants:** `/allow <path>`, `/allow-write <path>`, and
  `/allow-read-write <path>`. `/revoke-read <exact-root>` and
  `/revoke-write <exact-root>` remove a displayed root for the current session;
  a session reset restores invocation roots but never saved-session grants.
- **Non-interactive** (`--script`/piped): filesystem tools require explicit
  roots, `--yes`, or the unattended bypass. `--yes` enables chat filesystem
  tools and grants the launch directory read+write; requests outside current
  roots are denied without prompting. It does not approve commands.
  `--dangerously-allow-all-filesystem` widens enabled filesystem tools to
  non-protected host paths without registering tools by itself. For fully
  unattended automation,
  `--dangerously-skip-permissions` (short alias `--yolo`) additionally registers
  read/write tools and skips command approval prompts when command capability is
  separately enabled; it does not bypass the remaining guards named above.
- **Background agents** inherit current explicit/session grants but never own
  the foreground terminal and therefore cannot open filesystem or command
  approval prompts. By default, an ungranted operation fails closed. Set
  `[agents] blocking = "ask"` to park an agent until `/jobs reply ID y|a|n`
  answers its request or `block_timeout_secs` expires (default 1800 seconds).
  Foreground command auto mode does not approve background requests.
  Results not opened through `/jobs show` are printed before a session switch
  or interactive exit,
  after live jobs have been joined. Scripted JSONL emits unopened terminal work
  as `background_result`; an unseen failed job makes strict automation nonzero.
- `[agents] state_file` publishes a small advisory JSON hint — one file
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
- `[hooks] on_agent_state` runs a local command on each agent state change,
  exporting `KITSU_AGENT_ID`, `KITSU_AGENT_STATE` and
  `KITSU_AGENT_WAITING_FOR`. It is executed **directly, never through a shell,
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
  Enable general commands explicitly with `--enable-commands`. On Linux,
  every approved process must also pass a fingerprinted Bubblewrap preflight:
  approved command roots are writable mounts, required runtime paths are
  read-only, PID/IPC/UTS namespaces are isolated, capabilities are dropped,
  and the child gets
  a fixed non-secret environment (`PATH`, `HOME`, `LANG`, `LC_ALL`, `TERM`,
  `TMPDIR`, `USER`, `LOGNAME`), optionally extended with explicit
  `--command-env` pass-through variables — never anything else inherited from
  the host. On macOS, commands use Apple Seatbelt through `sandbox-exec`, with
  writes restricted to approved roots and temporary storage. Network access
  is disabled on both platforms unless `--allow-network` is supplied.
  Missing, changed, or unsupported containment
  fails before prompting or launching; exact approval never substitutes for it.
  User-installed toolchains may need explicit read-only runtime roots. For
  example, use `--command-runtime-root ~/.cargo --command-runtime-root
  ~/.rustup` for a Rustup toolchain. Add only the directories the command
  needs: their contents become readable by approved commands. Interpreter
  module paths can be supplied separately with `--command-env PYTHONPATH=...`
  or `NODE_PATH=...`.
  `/permissions` numbers exact argv/cwd grants with invocation/runtime
  provenance; `/revoke-command <number>` removes one for the current session.
  A session grant is bound to the observed executable identity and is revoked
  if the executable or its `PATH` resolution changes before reuse.
  At a foreground command prompt, `[u]` enables session command auto mode;
  `/auto on` and `/auto off` switch it between turns. Auto mode skips later
  foreground command prompts while preserving argv checks and containment.
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
Their layout adapts to terminal width; narrow panes use compact command rows.
Commands reject unexpected arguments and suggest close matches.

Slash commands inside the REPL:

| Command | Action |
|---|---|
| `/model [name]` | Switch model while keeping history; without a name, open the interactive model picker. |
| `/models [filter]` | Preview the models the gateway offers (from its `models_path`), optionally filtered by a substring — e.g. `/models qwen`. The active model is pinned and marked; large catalogs show 12 rows plus a narrowing hint. Listed IDs can be passed to `/model`. |
| `/reasoning [none\|minimal\|low\|medium\|high\|xhigh\|default]` | Show or change reasoning effort for subsequent turns. |
| `/status` | Show one offline typed observation of the active route/model/generation, runtime/session identity, directory, effective authority, context occupancy, and current usage coverage with explicit truth/provenance. Output adapts across terminal, plain, screen-reader, and redirected destinations. |
| `/diff [working\|staged\|unstaged\|base REV\|commit REV\|checkpoint ID] [path]` | Show bounded host-verified evidence for an explicit scope. Host-owned: needs no read grant, and the bytes never leave your terminal. |
| `/review [working\|staged\|unstaged\|base REV\|commit REV\|checkpoint ID] [path]` | Review exactly that verified evidence in a no-tools turn. Discloses one bounded diff to the model without granting a read root. The top-level `kitsu review` provides the noninteractive form. |
| `/checkpoints` / `/checkpoint <id>` | List recoverable write checkpoints or preview one verified diff. |
| `/permissions` | Show current filesystem grants, protected paths, command approval policy, containment status, and network policy. |
| `/auto [on\|off]` | Show or change foreground command auto mode. Containment and filesystem grants remain enforced. |
| `/tools` | Show enabled tools and the effective permission behavior. |
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
| `/agent [list\|show ID\|steer ID TEXT\|stop ID\|close ID]` | Inspect one bounded, redacted agent timeline; steer or stop that thread; acknowledge and close retained results. |
| `/jobs [action]` | Inspect jobs with `list`, `view`, `explain ID`, or `show ID`; `explain` does not acknowledge results. `steer ID TEXT` restarts a worker with the correction, retaining completed file effects. Use `close-steering`, `stop`, `stop-all`, or `prune` to manage work; `/agents` is an alias. |
| `/jobs reply ID y\|a\|n` | Answer a parked background-agent permission request: allow once, always for the requested grant, or deny. Requires `[agents] blocking = "ask"`. |
| `/detach` | Return input to the foreground conversation while the attached agent keeps running. |
| `/artifacts [list\|prune ID\|prune all]` | Inspect aggregate command-artifact quota/usage and explicitly prune retained evidence. Also reports what earlier runs left on disk; reclaim that with the host-only `kitsu artifacts`. |
| `/queue [list\|add\|remove\|edit\|move\|promote\|clear]` | Inspect and edit the bounded eight-prompt FIFO; `/queue <prompt>` remains shorthand for add. A single word one edit away from a subaction (e.g. `/queue lst`) gets a close-match suggestion instead of being silently queued; `/queue add <word>` queues it verbatim. |
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
| `/tui [classic\|inline\|fullscreen]` | Report or change the Unix terminal renderer between turns. |
| `/theme [name]` | List color themes or apply one for the current session. |
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

## Automation

Use `--jsonl` for a stream of versioned `kitsu.chat.v1` records on stdout.
The [JSONL protocol](jsonl-protocol.md) and its
[schema](schemas/kitsu-chat-v1.schema.json) define event fields, ordering,
compatibility, and failure handling. A final summary reports the outcome
while the output sink remains writable. Streamed assistant text is provisional
until its disposition is committed.

For a separate final-answer file, add `-o PATH`. Kitsu atomically writes the
last fully committed assistant message. Cancelled, discarded, blank, or partial
answers leave an existing destination unchanged. The parent directory must
already exist; symlinked paths and non-regular destinations are refused. This
host-owned destination is protected from model tools and private-state overlap.

Add `--output-schema PATH` to require a JSON answer. The file must contain a
strict object schema, fit within 64 KiB, and be a regular non-symlink file.
Kitsu validates it before provider contact, sends it as a native JSON Schema
response format, and validates the final answer locally before committing.
Malformed JSON, Markdown fences, or schema mismatches fail with exit `2` and
leave `-o` unchanged. Keep the schema outside command and runtime roots.

Use `chat --require-write PATH` when success requires a file change. It checks
for a verified filesystem-tool mutation and final contents that differ from
launch and still match the journal. It grants no authority: use
`--allow-write` for a new file or `--allow-read-write` for an existing file.
`--require-write-sha256 PATH=64HEX` additionally requires the exact raw-byte
digest. These run-level checks cannot be waived by `--allow-turn-errors`;
failure exits `1`, preserves completed file effects for inspection, and leaves
`-o` unchanged.

When combining a schema with required writes, allow at least two model rounds.
Kitsu runs tool-capable actions first, then reserves a tool-free round for the
structured final answer. Both phases share the turn deadline and round budget.
The host checks required writes again before installing the final-answer file.

## Terminal modes

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

If only color should be disabled, set `NO_COLOR=1`. It selects plain rendering
and defaults to the classic renderer while retaining the rich Rustyline
editor, including history, completion, and bracketed multi-line paste.
Pass `--reduced-motion` or set `KITSU_REDUCED_MOTION=1` to replace animated
waiting indicators with static status.

On Unix, a styled interactive terminal defaults to the **inline viewport**:
a framed input box with a status row above it and a scrolling transcript.
The box expands for multiline drafts and uses the same visual-row layout for
painting and Up/Down navigation. History persists across sessions, Ctrl+R
searches it, and Tab completes slash commands and granted file paths. A
bracketed paste stays editable as expanded text and submits as one prompt.

Choose a renderer with `--tui classic|inline|fullscreen`, or persist it in
configuration:

```toml
[tui]
mode = "inline"
title = true
```

The flag overrides the config; `--no-tui` is shorthand for `--tui classic`.
Use `/tui` to report the active renderer or `/tui fullscreen` to switch between
turns. Without an explicit choice, piped, scripted, JSONL, and plain output
use classic. Windows uses the classic renderer. `KITSU_TUI` is retired.

Fullscreen uses the terminal's alternate screen and displays retained
transcript output. `PgUp`/`PgDn` scroll it; `Ctrl+Home`/`Ctrl+End` jump to the
oldest/newest retained output. Leaving fullscreen restores the previous
screen. This version refuses `--no-alt-screen` and `[tui] alternate_screen =
"never"`, because overlays still need the alternate screen.

Additional keys in the Unix inline and fullscreen composers:

| Key | Action |
| --- | --- |
| `Alt+Enter` | Insert a newline in the draft. |
| `Ctrl+L` | Repaint the terminal. |
| `Ctrl+O` | Open the transcript overlay; `/` searches, `n`/`N` walk matches, and `q` or `Esc` closes it. |
| `Ctrl+X a` | Open the agent view to inspect, steer, stop, or attach to an agent. |
| `Ctrl+X 1` through `9` | Attach by the agent's displayed position. |
| `Ctrl+X d` | Detach and return input to the foreground conversation. |
| `Shift+Left` / `Shift+Right` | Cycle the attached agent. |
| `Ctrl+Z` | Suspend, restoring the terminal; `fg` restores the viewport. |

`[tui] prefix` changes the chord prefix from `ctrl+x`. The `title` setting
controls the terminal window title, which reports session state and the agent
count. It does not change the draft or viewport status row.

In classic mode, background-agent notices print between prompt cycles by
default. Set `KITSU_LIVE_NOTICES=1` to enable live idle notices through the
line editor. Inline mode updates its ambient agent status independently.

---

## Context window

Set `--context-window TOKENS` or the gateway's `context_window` to match the
model server. Otherwise Kitsu uses fresh discovery metadata or a conservative
8192-token fallback. `/context` shows utilization and source estimates;
`/status` includes compaction outcomes.

Kitsu first offloads large tool results, then elides old result bodies and
compacts whole older exchanges while preserving system and recent context.
`--compaction trim` is the default; `summarize` uses a bounded model request
and falls back to trimming. `/compact` runs compaction manually.

See [Context management](context.md) for window precedence, token accounting,
artifact retention, limits, and telemetry.

### Project context (`llms.txt`)

Kitsu loads a discovered `llms.txt`, falling back to `AGENTS.md` or
`CLAUDE.md`, and reports the source in `/status` and `/context`. Links are
not followed automatically. `--no-project-context` or
`KITSU_NO_PROJECT_CONTEXT=1` disables loading, including saved project text
on resume. See [project-context loading](context.md#project-context-llmstxt)
for discovery, size budgets, and platform restrictions.

## Skills

A local skill is a directory containing a `SKILL.md` with metadata and
instructions:

```markdown
---
name: code-review
description: Review a file or diff for bugs.
when_to_use: When the user asks for a review.
---
Read the requested files and report findings by severity with file references.
```

Kitsu discovers project skills in `.kitsu/skills/*/SKILL.md` up to the Git
root, and user skills in `~/.kitsu/skills/*/SKILL.md`. User skills win duplicate
names. `/skills [filter]` lists available skills and their origins; discovery
admits at most 64 manifests with a 16 KiB rendered index.

The index is available in context. Load instructions with `/skill NAME`, the
model's `skill` tool, or repeatable `--skill NAME` at launch. Preloads also
work for `ask` and spawned agents. A project skill's `always: true` still
requires explicit loading; user-controlled always skills can load at startup.

Bundled UTF-8 resources are read through no-follow descriptors and checked
against discovery identity. Loads are bounded to 16 files / 64 KiB per skill
and 128 files / 512 KiB globally. Symlinked skill paths are ignored; drift and
duplicate sources are reported. Skill links are displayed but never fetched.

Skill text grants no authority; its actions remain subject to permissions.
`--no-project-context` disables project discovery while leaving user skills
available. Filesystem discovery fails closed on platforms without secure
component traversal.

## Troubleshooting and exit codes

- If local discovery fails, check that the server is running and try
  `kitsu --gateway local models`. `doctor --online` checks catalog access;
  `doctor --inference` tests the configured chat route.
- If an answer reaches its output limit, increase `--max-output-tokens` and
  retry. Strict runs retain the partial text for inspection but do not commit
  the turn.
- If a contained command cannot find a user-installed toolchain, add its
  directory with `--command-runtime-root`; use `--command-env` for required
  interpreter paths.

| Exit code | Meaning |
| --- | --- |
| `0` | Success, including mixed-success scripts accepted by `--allow-turn-errors` |
| `1` | CLI, input, configuration, authentication setup, tool, policy, or required-write failure |
| `2` | Provider/HTTP/timeout failure or incomplete/invalid model output |
| `130` | Cancellation by Ctrl+C |

Redirected human output keeps stdout available for answers. Stderr carries
`kitsu-usage:` and `kitsu-context:` records plus
`kitsu-timing: {"duration_ms":...,"status":"..."}` for inference-capable
commands. Timing status is `ok`, `partial`, `error`, or `cancelled`;
`partial` identifies a script accepted after mixed results. Use
[JSONL](jsonl-protocol.md) for the versioned automation contract.

Human inventory pipelines such as `kitsu sessions | head -1` exit quietly
when the downstream reader closes. JSONL write failures follow the protocol's
separate output-reliability rules.

Implicit project context, filesystem skills, and file-attachment reads and
completion fail closed where secure path-component traversal is unavailable.
Developer commands require a supported containment backend.
