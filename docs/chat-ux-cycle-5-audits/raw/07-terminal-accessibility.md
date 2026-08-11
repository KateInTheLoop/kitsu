# Audit 7 — terminal safety and accessibility

Baseline: clean `af20c26`. A real PTY exercised Unicode/Markdown, plain static
status, usage, background completion, partial input, and quit ordering.

## What worked

- Dangerous bidi/format controls are removed while safe Unicode remains.
- Plain mode disables bracketed paste and uses an escape-free canonical reader.
- Reduced-motion statuses are complete newline-terminated records.
- A background completion did not splice into partially typed plain input; its
  notice was deferred intact.

## Findings

1. **P1 — Ctrl+C does not immediately recover the plain assistive prompt.** With
   partial input visible, Ctrl+C printed `^C` but no fresh prompt; Enter was
   required before the notice and prompt appeared.
2. **P2 — plain async completion notices are safe but stale.** Long prompt
   composition can hide a completion until the input line terminates.
3. **P2 — accessible output retains punctuation-heavy Markdown.** `##`, `**`,
   and backticks remain literal (`kitsu-render/src/lib.rs:291`).

Top priorities: atomic Ctrl+C line discard/redraw, a prompt-safe pending-notice
indicator, and a deterministic PTY/raw-byte accessibility matrix.
