# Chat UX dogfood — cycle 1

Date: 2026-08-09

Ten independent reviewers exercised Kitsu through a local OpenAI-compatible mock
gateway and isolated temporary workspaces. No hosted inference or billing was
used. Each reviewer owned a different workflow so the result was broader than
ten copies of the same smoke test.

## Feedback collected

| Reviewer focus | Highest-impact observations |
|---|---|
| 1. First-run onboarding | A missing config ended as “no gateway”; help did not provide a complete first command; loaded project context and auth readiness were hard to verify. |
| 2. Edit visibility | Write output reported only bytes. There was no authoritative diff, review entry point, or clear distinction between created, updated, and unchanged files. |
| 3. Codex comparison | Kitsu lacked Codex's core coding loop affordances: `/diff`, `/review`, `/new`, `/resume`, visible permission state, durable sessions, formatted answers, and safe interruption. |
| 4. Permissions | Runtime grants could imply tools were enabled when they were not; duplicate/nested roots obscured effective scope; tool failures hid the useful error detail. |
| 5. Interrupts | Idle Ctrl+C exited unexpectedly. A failed/cancelled turn remained in history, permission input could be swallowed, and retry behavior was absent. |
| 6. Sessions | Only editor input history survived restart. There was no conversation autosave, list, new-session boundary, or resume path. |
| 7. Error recovery | Scripted provider and slash-command failures still exited 0; lines lacked source locations; unterminated `<<<` blocks were silently accepted. |
| 8. Terminal rendering | Markdown was raw, very small nonzero context usage rendered as empty, the spinner flashed for fast responses, and plain/reduced-motion behavior was incomplete. |
| 9. Automation | Slash commands were not echoed in transcripts, success could be false-green after a failed turn, and buffered assistant output could appear after telemetry. |
| 10. End-to-end task flow | The three biggest blockers were verified edit evidence, runnable recovery after failure, and durable conversations; status needed Git/session/config truth in one place. |

The Codex comparison used the documented CLI command surface as a reference,
including `/diff`, `/resume`, and `/permissions`:
<https://learn.chatgpt.com/docs/developer-commands?surface=cli>.

## Ten urgent improvements selected

1. Add `/diff [path]` sourced directly from Git and the filesystem, including
   staged, unstaged, and untracked text changes.
2. Add `/review [path]` that supplies the model with that verified diff rather
   than trusting model-authored claims about edits.
3. Make failed and cancelled turns transactional, and add `/retry`.
4. Autosave private conversation snapshots and add `/new`, `/sessions`,
   `/resume`, and `chat --resume [id]` without persisting permissions.
5. Make scripts strict by default: line-aware errors, nonzero status, unmatched
   block detection, command echo, and an explicit `--allow-turn-errors` escape.
6. Make idle Ctrl+C clear input rather than exit, and reject invalid
   permission answers exactly once so pasted trailing text cannot authorize.
7. Tighten command discovery and validation: a single registry, bare `/` help,
   typo suggestions, unexpected-argument errors, `/tools`, and global `--skill`.
8. Disclose the exact config, sanitized endpoint, auth readiness, Git state,
   session id, and project-context source in `/status`; add
   `--no-project-context`.
9. Report created/updated/unchanged writes, surface safe tool-error detail, and
   minimize duplicate/nested permission roots.
10. Improve terminal output: buffered Markdown/diff rendering, truthful
    low-percentage context bars, delayed/reducible motion, bounded banners, and
    `NO_COLOR`/`KITSU_PLAIN` support.

## Verification contract

- Session JSON excludes auth configuration and permission grants, is written
  durably, and is private on Unix. Conversation/tool text may itself contain
  sensitive material, so `--no-session` remains the privacy boundary.
- Conversation state rolls back after a provider error or Ctrl+C. Filesystem
  operations that already completed remain on disk and are disclosed as such.
- `/diff` is labelled verified evidence and never comes from assistant text.
- Active Kitsu config/private state is omitted from model file tools and
  workspace evidence; `/review` receives the verified diff with no tools.
- `--no-project-context` also replaces saved project system context on resume.
- Script failures return `1` for input/command errors and `2` for provider
  errors unless partial success was explicitly requested.
- The full workspace test and lint suite must pass before the cycle commit.

Deferred candidates for later cycles include queued prompts during generation,
manual `/compact`, patch-level editing and undo checkpoints, session search or
fork/export, `@` file mentions, shell shortcuts, and machine-readable JSONL
automation output.
