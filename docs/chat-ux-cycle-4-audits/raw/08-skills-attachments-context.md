# Audit 8 — project context, skills, and attachments

Baseline: `c485edb`. The auditor inspected discovery/loading code and used a
localhost mock to validate request payloads, no-project-context behavior,
attachment permissions/ranges, and adversarial filenames/content. No Kitsu
repository files were changed.

## What worked

- Explicit attachments enforce read permission, byte and UTF-8 limits,
  descriptor-based no-follow reads, canonical deduplication, and line ranges.
- Skill manifests/resources are bounded, UTF-8 checked, and no-follow on Unix.
- `--no-project-context` disables automatic context discovery; deterministic
  precedence makes behavior reproducible.

## Ranked findings

1. **High — automatic project context follows a repository symlink and bypasses
   read permissions.** In a live repro, `project/llms.txt` linked to an outside
   secret. Kitsu sent the secret to the provider without a matching read grant
   while disclosing only the apparent in-project path. `llms.txt`, `AGENTS.md`,
   `CLAUDE.md`, and configured context files share path-based discovery/open
   behavior.
2. **Medium-high — project skills silently shadow user skills and can auto-inject
   content.** Duplicate names select the project version without showing origin.
   A project manifest with `always=true` injects its body/resources before user
   approval, and resources are eagerly snapshotted.
3. **Medium — resume resends saved automatic context without its original source
   notice.** Provenance collapses after snapshotting even though
   `--no-project-context` correctly prevents fresh discovery.
4. **Medium — `--no-project-context --skill NAME` cannot honor explicit skill
   selection.** The same switch disables project context and discovery of user
   skills, contradicting the separate explicit preload promise.
5. **Low — attachment delimiters can be spoofed by file contents, and natural
   grammar around quotes, spaces, punctuation, and ranges is narrow.**
6. **Low — the preload summary counts requested skill names rather than only
   successfully loaded skills.**

## Top improvements

1. Use descriptor-pinned no-follow reads for all implicit context and show its
   resolved origin/provenance.
2. Separate project context, project skills, and user-skill policy; require
   explicit consent before project `always` injection.
3. Use robust attachment parsing plus unambiguous structured framing.

