# Audit 9 — Kitsu compared with Codex CLI

Baseline: clean `af20c26`. Kitsu evidence came from help, provider-free
init/doctor/sessions, real PTY chat, permissions, tools, steering/queue, review,
fork/resume, and JSONL. Codex facts used current official OpenAI documentation:

- [Codex CLI](https://learn.chatgpt.com/docs/codex/cli)
- [Subagents](https://learn.chatgpt.com/docs/agent-configuration/subagents)
- [Agent approvals and security](https://learn.chatgpt.com/docs/agent-approvals-security)

## Kitsu strengths

- Credible terminal loop: visible tools, context, cancellation, active steering,
  editable queued prompts, review, and continued conversation.
- Host-verified diff/checkpoint/undo and broader provider-free session lifecycle.
- Provider portability plus unusually explicit context-window provenance.
- Stateless-by-default automation, durable opt-in, typed JSONL, and failure
  categories.
- Fail-closed filesystem authority, grants reset on resume, strong init/doctor,
  normal scrollback, plain output, and reduced motion.

## Ranked gaps

1. **Critical — commands are approved but not OS sandboxed.** Cwd roots are not
   filesystem/network/process containment, whereas Codex documents an
   OS-enforced sandbox plus a separate approval layer.
2. **High — background agents are records, not inspectable threads.** There is
   no `/agent`-style thread switch, evolving transcript, or targeted worker
   steering comparable to the documented Codex flow.
3. **High — no configurable specialist agent profiles.** Kitsu exposes generic
   spawn tools but not per-agent model, reasoning, instructions, tools, or
   permission/sandbox defaults.
4. **Medium-high — no reasoning-effort control.** `/reasoning high` is unknown.
5. **Medium — onboarding still assumes local model/gateway knowledge.**
6. **Medium — models, sessions, and permissions are exact-ID/text driven rather
   than searchable interactive selectors.**
7. **Low — no shell-completion generation, external editor, or theme selector.**

Top priorities: genuine OS containment, first-class agent threads, and reusable
specialist profiles with reasoning/tool/security policy.
