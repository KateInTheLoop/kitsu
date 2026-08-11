# Audit 8 — skills, attachments, and implicit context

Baseline: clean `af20c26`. Current binary/help was paired with current-source and
regression evidence after the localhost request was interrupted.

## What worked

- Project context, skills, and attachments use no-follow/opened-inode validation
  on Unix; symlink, ancestor-retarget, and hard-link regressions exist.
- User skills win collisions; duplicate origins are disclosed; project
  `always=true` no longer auto-loads.
- `--no-project-context` retains user skills.
- Attachments support quoted paths, punctuation, multiple range syntaxes,
  length-prefixed JSON framing, read permission, UTF-8/range/aggregate bounds,
  deduplication, and safe completion.

## Findings

1. **Medium — project skill descriptions are automatic system-prompt input.** A
   body need not load: repository-controlled name/description metadata enters
   the protected system prompt through `SkillSet::index`
   (`skills.rs:167-186`, `main.rs:6861-6874`).
2. **Medium — skill discovery has no aggregate budget.** Per-manifest/resource
   caps exist, but count, total snapshot bytes, and index bytes are unbounded;
   resources are eagerly copied before explicit load (`skills.rs:302-448`).
3. **Medium — resume can advertise stale sources/capabilities.** The saved first
   system message is restored wholesale while current skills/tools are
   registered, and provenance collapses to one resumed row
   (`main.rs:2639-2674,6655-6661,6911-6972`).
4. **Low — skill resources use spoofable XML-like sentinels rather than the
   attachment JSON framing** (`skills.rs:519-539`).
5. **Low — preload counts requested, not successfully loaded, skills.**
6. **Portability — secure implicit inputs intentionally fail closed off Unix.**

Top priorities: bound/frame untrusted metadata, lazy bounded resource loading,
and structured source/hash provenance with system-prompt rebuild on resume.
