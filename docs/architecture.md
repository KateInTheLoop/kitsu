# Architecture

Kitsu is a seven-crate Rust workspace. The binary owns configuration and user
interaction; reusable crates implement the agent loop, providers, tools,
editing, and rendering.

| Crate | Responsibility |
| --- | --- |
| `kitsu` | CLI, gateway/config selection, credentials, chat, durable sessions, artifacts, and background agents |
| `kitsu-core` | Agent loop, context budgeting/compaction, neutral run values, and canonical session types |
| `kitsu-providers` | Provider interface, OpenAI-compatible and native Anthropic Messages adapters, streaming, retries, and discovery |
| `kitsu-tools` | Filesystem permissions, tools, write journals, and contained developer-command execution |
| `kitsu-render` | Sanitized terminal output, live status, diffs, usage, and JSONL emission |
| `kitsu-editor` | Shared type-ahead buffer, key handling, history navigation, and reverse search |
| `kitsu-tui` | Terminal lifecycle and inline viewport primitives |

## Request flow

The CLI resolves a gateway profile, model, credential source, and context
window. The profile's `adapter` selects the compiled wire implementation;
`flavor` is a diagnostic label. Ollama is accessed through its compatible
endpoint, not a separate native Ollama adapter.

The CLI loads bounded project context and registers tools permitted for the
run. `kitsu-core` checks the concrete request budget, calls the provider, and
executes requested tool rounds. The host owns permissions and evidence;
model-generated text cannot grant authority or establish a successful write.

`ask`, piped chat, scripted chat, and interactive chat share the core loop but
have separate input/output adapters. JSONL output reserves stdout for
versioned records; see [the protocol](jsonl-protocol.md).

## Source map

- `crates/kitsu/src/cli.rs`, `config.rs`, and `credential_store.rs`: flags,
  profile construction, and managed authentication.
- `crates/kitsu/src/main.rs`: command dispatch and chat integration.
- `crates/kitsu/src/session/`: physical session storage and transactions.
- `crates/kitsu/src/subagent/`, `subagent.rs`, `agent_state_file.rs`, and
  `agent_hook.rs`: bounded background work, local state publication, and hooks.
- `crates/kitsu/src/workspace.rs`: host-verified Git evidence.
- `crates/kitsu/src/tui_bridge.rs`: Unix inline preview integration.
- `crates/kitsu-core/src/orchestrator.rs` and `context.rs`: model/tool rounds
  and request budgeting.
- `crates/kitsu-tools/src/permissions.rs` and `builtins/`: filesystem policy
  and built-in tools.

## Boundaries to preserve

- `kitsu-core/src/run.rs` contains carrier-neutral values. Keep terminal,
  persistence, provider execution, process exit, and OS I/O in adapters.
- `kitsu-core/src/session.rs` contains canonical session values. Physical
  paths, store operations, and migrations belong in host adapters.
- Credentials and Kitsu-private state must stay out of tool reads and review
  evidence. Never log bearer values or raw credential-bearing provider data.
- Developer commands require literal argv, exact approval, and the existing
  containment backend. Preserve fail-closed behavior.
- Display host-verified diffs and journal snapshots as evidence; keep model
  claims distinct. See [verified review](verified-review.md).
- Honor plain and screen-reader modes. Sanitize untrusted terminal content,
  stop animation outside terminals, and restore terminal modes on exit.
- The inline viewport is opt-in through `KITSU_TUI=inline` on Unix. It shares
  the editor and output surface, disables the classic live row while active,
  and restores state on teardown. Resize and multi-row completion remain
  preview limitations.

Run/session boundary scripts enforce the first two boundaries. See
[Contributing](../CONTRIBUTING.md) for checks and
[context management](context.md) for project-context loading and compaction.
