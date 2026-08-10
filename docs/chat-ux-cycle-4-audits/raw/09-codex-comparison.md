# Audit 9 — Kitsu compared with Codex developer experience

Baseline: `c485edb`. Kitsu claims are from direct CLI/PTY use and source
inspection. Codex claims use only official OpenAI documentation:

- [Codex CLI](https://learn.chatgpt.com/docs/codex/cli)
- [Codex subagents](https://learn.chatgpt.com/docs/agent-configuration/subagents)
- [Agent approvals and security](https://learn.chatgpt.com/docs/agent-approvals-security)

## Where Kitsu is strong

Cycle three closed the major functional gaps: top-level session lifecycle,
scoped tool-free review, background job control, active steering plus editable
queueing, safe `--yes`, verified checkpoints/diffs/undo, resumable automation,
and accessible plain/reduced/JSONL output. Kitsu additionally offers excellent
provider portability, explicit context-window provenance, a provider-free
fork/export/archive/restore/delete session lifecycle, and native verified undo.

## Ranked residual gaps

1. **Critical parity gap — developer commands lack OS-enforced containment.**
   Exact argv/cwd approval limits accidental invocation but is not a sandbox;
   approved commands inherit ambient filesystem, environment, process, and
   network authority. Codex documents a separate OS sandbox and approval policy.
2. **High — subagents are jobs rather than inspectable threads.** Kitsu exposes
   aggregate live activity and terminal results but no thread switch or evolving
   transcript inspection comparable to Codex `/agent` workflows.
3. **High — agents cannot have heterogeneous profiles.** There is no per-agent
   model, reasoning effort, instructions, tools, or reusable specialist role.
4. **Medium-high — onboarding remains source-build/config oriented.** `init` and
   doctor are strong recovery primitives, but immediate installation/sign-in is
   less polished than the documented Codex flow.
5. **Medium — no in-session permission-mode selector or reasoning-effort choice.**
6. **Medium — active type-ahead steering is Unix-only.**
7. **Medium-low — model/session discovery is list-and-exact-ID driven rather
   than searchable and nameable.**
8. **Low — shell completion, themes, and external-editor integration are absent.**

## Top improvements

1. Put `command_exec` behind genuine OS filesystem/network/process containment.
2. Add inspectable agent threads with targeted steering and explicit lifecycle.
3. Add custom agent profiles with model, effort, instructions, and tool policy.

