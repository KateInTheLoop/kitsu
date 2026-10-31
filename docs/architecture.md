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
- `crates/kitsu/src/tui_bridge.rs`: Unix inline/fullscreen integration,
  composer input, overlays, and terminal restoration.
- `crates/kitsu-render/src/layout.rs` and `live.rs`: source-to-cell layout
  and the framed composer.
- `crates/kitsu-editor/src/lib.rs`: input decoding, draft state, history,
  and reverse search without terminal I/O.
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
  containment backend: Bubblewrap on Linux or Seatbelt on macOS. Session
  command auto mode can waive foreground prompts; it preserves containment
  and does not grant background agents the same approval. Preserve fail-closed
  behavior and network isolation unless `--allow-network` is explicit.
- Display host-verified diffs and journal snapshots as evidence; keep model
  claims distinct. See [verified review](verified-review.md).
- Honor plain and screen-reader modes. Sanitize untrusted terminal content,
  stop animation outside terminals, and restore terminal modes on exit.
- On Unix, inline is the default for interactive terminals. `--tui` overrides
  `[tui] mode`; `--no-tui` selects classic. An unnamed mode falls back to
  classic for non-terminal and plain output. The bridge disables the classic
  live row while the viewport is active and restores it on teardown.

Run/session boundary scripts enforce the first two boundaries. See
[Contributing](../CONTRIBUTING.md) for checks and
[context management](context.md) for project-context loading and compaction.

## Editor and layout

`kitsu-editor` owns the source text and scalar cursor. The terminal collector
feeds bytes into it and handles the resulting submit, completion, interrupt,
and navigation actions. Bracketed-paste newlines stay inside one draft;
submission and history contain the source text.

`kitsu-render::layout` computes the entire draft before viewport clipping.
Source offsets are Unicode scalar boundaries; visual positions are terminal
cells. The layout accounts for wide and combining characters, explicit
newlines, a first-row prompt, and tabs displayed as `⇥`. Painting and injected
vertical motion share this mapping and its display-width policy. Up/Down move
through visual rows before crossing into history.

Frame builders return rows and caret coordinates without terminal I/O. The
bridge owns scrolling, resize, overlays, raw mode, and signal restoration.
Keep draft state separate from file descriptors and paint caches. Preserve
bracketed-paste decoding across fragmented reads, and test layout geometry and
key decoding independently from PTY behavior. Terminal integration tests live
in `crates/kitsu/tests/tui.rs`.
