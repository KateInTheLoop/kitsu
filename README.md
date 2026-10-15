# kitsu

A Rust terminal coding agent for hosted gateways and local model servers.
Use interactive chat, one-shot prompts, or scripts with permission-gated tools,
resumable sessions, and background agents. Connect to **Hugging Face,
Anthropic, OpenAI-compatible gateways, Ollama, or vLLM** using gateway profiles.

## Choose your provider

| Service | Connection | Setup |
| --- | --- | --- |
| Hugging Face (HF) | Inference Providers router | [HF token and profile](docs/usage.md#hugging-face-inference-providers) |
| Anthropic | Native Messages API | [API key and profile](docs/usage.md#anthropic) |
| OpenAI or a compatible gateway | Chat Completions API | [OpenAI](docs/usage.md#openai) · [Custom gateway](docs/usage.md#custom--corporate-gateway) |
| Ollama | Local compatible API | [Start a local model](docs/usage.md#ollama-local) |
| vLLM | Local or self-hosted compatible API | [Server setup](docs/usage.md#vllm-local--self-hosted) |

The example [`kitsu.toml`](kitsu.toml) includes profiles for all five.
Select one with `--gateway huggingface`, `anthropic`, `openai`, `ollama`, or
`vllm`. Model availability and features depend on the selected server.

## Install

Download a package for Linux, macOS, or Windows from
[GitHub Releases](https://github.com/KateInTheLoop/kitsu/releases).
Releases include checksums; macOS binaries are unsigned. See the
[installation guide](docs/usage.md#install) for packages, containers, and setup.

To build from source with a current stable Rust toolchain:

```bash
cargo build --release --locked
# Run target/release/kitsu, or put it on your PATH.
```

## Quick start

Start a local model server, then use the repository's example config:

```bash
ollama serve
# In another terminal:
ollama pull llama3.2
kitsu --config ./kitsu.toml -p "Explain a mutex in one sentence."
kitsu --config ./kitsu.toml
```

For your own configuration, `kitsu init` creates `./kitsu.toml` without
replacing an existing file. Check it with `kitsu --config ./kitsu.toml doctor`.
The default config location is `~/.kitsu/config.toml`; `--config` or
`KITSU_CONFIG` selects another file.

`--gateway local` discovers servers on standard local ports. For hosted
services, follow the provider setup above. Keep credentials in the configured
environment variable or `kitsu auth login`, never in Git.

## Everyday commands

These examples use `~/.kitsu/config.toml`; add `--config ./kitsu.toml` to use
the repository config instead.

```bash
kitsu                                  # interactive chat
kitsu -p "Explain this error"           # one-shot answer
kitsu chat --resume last               # resume a saved conversation
kitsu review                           # review host-verified Git changes
kitsu models                           # list the selected gateway's models
kitsu sessions list                    # inspect saved sessions
kitsu agents                           # inspect published local agent state
kitsu --help                           # commands and flags
```

In chat, `/help` lists commands, `/jobs` shows background work, and `/context`
shows context usage. Reads in the launch directory are allowed in interactive
chat; writes and other paths ask for permission. Developer commands are opt-in
and require Linux containment. See [tools and permissions](docs/usage.md#tools-in-chat).

`--plain` and `--screen-reader` select literal input and plain output.
`NO_COLOR` disables styling while retaining interactive editing.
`KITSU_TUI=inline` enables the Unix-only inline viewport preview; terminal
resize and completion menus remain limitations of that preview.

## Documentation

| Document | Use it for |
| --- | --- |
| [User guide](docs/usage.md) | Installation, configuration, providers, auth, CLI flags, chat, sessions, and skills |
| [Context management](docs/context.md) | Model windows, compaction, and project instructions |
| [JSONL protocol](docs/jsonl-protocol.md) | Automation events and compatibility; includes the JSON schema |
| [Verified review](docs/verified-review.md) | Diff scopes, checkpoints, and evidence boundaries |
| [Workspace diff tool](docs/workspace-diff-tool.md) | Model-requested Git evidence and disclosure |
| [Architecture](docs/architecture.md) | Source map and implementation boundaries |
| [Contributing](CONTRIBUTING.md) | Development and checks |
| [Release guide](docs/releasing.md) | Packaging and publishing |

[CHANGELOG.md](CHANGELOG.md) records shipped changes.
[AGENTS.md](AGENTS.md) gives coding agents repository instructions;
[llms.txt](llms.txt) provides a small project context index.

## License

[MIT](LICENSE).
