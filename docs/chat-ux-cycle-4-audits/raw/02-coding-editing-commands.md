# Audit 2 — coding, editing, review, undo, and commands

Baseline: `c485edb`. A disposable Git repository and localhost model mock drove
read/edit/write, scoped review, checkpoint, undo, command failure, and SIGINT
scenarios. No Kitsu source was changed by the auditor.

## What worked

- Exact file edits and creates produced verified diffs and checkpoints.
- Working, staged, unstaged, base, commit, and checkpoint review scopes used
  host-derived evidence and disabled tools.
- Command execution retained bounded head/tail output, termination metadata,
  protected full-output artifacts, and safely reaped processes on SIGINT.
- Conflict-aware undo refused to overwrite later user changes.

## Ranked findings

1. **High — `/undo` repairs disk but not the conversation.** The next provider
   request still contains successful edit/write tool results and assistant
   claims about the now-undone change. No system/tool marker tells the model
   which checkpoint was reversed, so subsequent reasoning operates on stale
   repository state.
2. **High — cancelled commands report invented empty output.** A command that
   flushed a diagnostic before cancellation was closed with stdout/stderr byte
   counts of zero, empty tails, and no observed executable path. The process is
   safely killed, but already-known evidence is lost at the most important
   failure boundary.
3. **High — natural attachment punctuation breaks the turn.** Text such as
   `please inspect @app.py:1-2.` treats the period as part of the path and makes
   expansion fail. Quotes, parentheses, and common sentence punctuation remain
   similarly awkward.
4. **Medium-high — checkpoint diff headers are noisy and can disclose absolute
   paths.** Some reports render headers such as `a//tmp/...` instead of stable
   repository-relative labels.
5. **Medium-high — a nonzero command can still yield scripted success.** A tool
   result with exit 7 is treated as ordinary model-visible output; if the model
   answers confidently afterward, strict automation may exit zero.
6. **Medium — provider-attempt notices are noisy on routine successful turns.**
7. **Medium — command artifacts are durable but lack retention/discovery UX.**
8. **Low — some scripted review and mixed-stream transcript lines are duplicated
   or reordered; command tails can be visually double-escaped.**

## Top improvements

1. Append a trusted undo event to model-visible history and invalidate affected
   mutation claims.
2. Carry partial command capture and native cancellation metadata through the
   drop path.
3. Parse natural attachment grammar and use non-spoofable framing.

