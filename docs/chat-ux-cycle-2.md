# Chat UX dogfood — cycle 2

Date: 2026-08-10

Ten independent reviewers exercised the Kitsu binary, CLI surfaces, local
scripts, and focused source-backed probes. They used temporary workspaces and
local mock fixtures only; no hosted inference, credentials, or billable calls
were part of this review. Where the sandbox prevented a loopback server, the
review was limited to observable binary and control-flow behavior.

## Baseline findings

These were the ten urgent gaps selected before cycle-2 implementation:

1. Protected private state could still be writable under broad grants, and
   `--yes` mixed approval with capability.
2. Context fitting lacked a final invariant, manual compaction, and truthful
   compaction provenance.
3. Coding turns lacked exact-precondition edits, verified change feedback, and
   a conflict-aware undo checkpoint.
4. Models could not run guarded build/test/lint commands with trustworthy
   timeout and process metadata.
5. Automation lacked framed, versioned output and exact failure/cancellation
   semantics.
6. Session opt-out and lifecycle behavior were incomplete.
7. Prompts lacked permission-aware, bounded `@file` expansion and completion.
8. Foreground cancellation, detached-agent shutdown, and type-ahead ownership
   were inconsistent.
9. Plain/accessibility rendering and terminal-control sanitization were
   incomplete.
10. Optional model discovery and retries could add avoidable latency to healthy
    inference.

## Implemented outcomes

| Area | Cycle-2 behavior |
|---|---|
| Permissions | Config, snapshots, history, caches, and other private runtime state are model-write protected. Internal artifact reads are narrow; skill resources are captured with bounded no-follow reads and never widen filesystem authority. `--yes` approves enabled filesystem requests but enables nothing and never approves commands. |
| Context | Every concrete request gets a final fit check. The cascade offloads large tool output, elides old bodies, and trims or summarizes whole exchanges. `/compact`, `/context`, and compaction detail in `/status` expose what occurred. A cold metadata cache enforces 8192 immediately and warms asynchronously. |
| Editing | `fs_edit_file` uses exact text preconditions and concurrent-change checks. Writing turns report verified paths, byte counts, and first changed lines. `/undo` restores only the latest matching checkpoint and refuses later conflicts. |
| Commands | Opt-in `command_exec` accepts literal argv and an approved cwd, never a shell string. Command authority is separate, output and time are bounded, and results disclose exit/signal/timeout/truncation/elapsed state. `repo_grep` reports process failures truthfully. |
| Automation | `--jsonl` emits one `kitsu.chat.v1` event per stdout line with run/turn/command/provider/tool/usage/error framing and exactly one final summary. Strict scripts report incomplete turns; Ctrl+C exits 130. |
| Sessions | `--no-session` disables snapshots and prompt history. Resume starts with fresh authority; `/resume last` excludes the active session. New, fork, export, archive, restore, delete, leases, and artifact-aware lifecycle operations are implemented. |
| Attachments | `@path`, `@path:line`, and `@path:start-end` are canonical-policy checked, UTF-8/binary checked, deduplicated, summarized, and capped at 32 KiB selected text each / 64 KiB total. A small range streams from a larger file. Completion exposes only granted, non-protected regular paths and never traverses symlinks. |
| Lifecycle/queue | Cancelled turns roll conversation and context state back while retaining completed file effects for inspection/undo. Normal exit does not silently discard background agents. The visible queue is an eight-prompt FIFO. Unix captures complete type-ahead lines during generation and pauses collection for approvals. |
| Terminal | Plain mode uses ASCII structural markers without ANSI/OSC styling for Kitsu-owned UI; safe Unicode user/model/path content remains intact. Model control strings are sanitized. Reduced-motion mode uses a visible static status, and telemetry formatting follows stderr. |
| Provider latency | Cold advisory discovery has a separate two-second deadline and cannot block current inference. Catalog success/failure caches are bounded, retries honor bounded `Retry-After` within one total deadline, and top-level `models` does not require a guessed model ID. |

Session exports remain no-clobber. When the session owns artifacts, export also
creates a deterministic sibling named by appending `.artifacts` to the complete
destination filename (for example `chat.md.artifacts`) and rewrites exact stored
pointers. A missing referenced source sidecar is an error; an existing primary
or sidecar destination is never overwritten.

## Developer experience compared with Codex

The dedicated comparison identified the same six highest-impact workflow gaps:
guarded commands, JSONL automation, complete session lifecycle, `@file`
attachments, manual compaction, and steering queued work while a turn runs.
Cycle 2 now supplies those workflows, but it does not claim terminal parity:
automatic type-ahead capture and process-group cancellation are Unix-only, and
Kitsu remains a line-oriented CLI rather than Codex's complete client/runtime.

The review used the official OpenAI developer-command reference for documented
Codex behavior:
<https://learn.chatgpt.com/docs/developer-commands?surface=cli>.

## Current verification contract

Before a cycle commit, the local release gates are:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

The suite includes focused regressions for protected paths and symlinks,
edit/undo conflicts, command timeout/cancellation, JSONL framing and exits,
session leases/lifecycle/artifact export, attachments and completion, prompt
queue ordering, terminal sanitization/plain rendering, provider caching/retry,
and context preflight/manual compaction. A release audit must also inspect the
uncommitted diff and documentation claims.

Hosted inference is a separate smoke test, not a local release gate. It requires
valid credentials, available billing, reachable provider infrastructure, and a
cooperative model, so a local green suite does not claim those conditions.

## Known platform residuals

- Unix builds use descriptor-relative no-follow file/session operations,
  descriptor-bound command cwd, foreground process groups, and complete-line
  type-ahead collection.
- Portable branches still validate paths and identity and cancel/reap the direct
  child, but cannot remove every pathname race, kill an entire descendant
  process group, or collect in-flight terminal lines equivalently.
- Operations whose conflict guarantee depends on an unavailable atomic
  primitive fail closed.
- Plain mode controls Kitsu-owned structure, not arbitrary safe Unicode supplied
  by users, models, filenames, or providers.
