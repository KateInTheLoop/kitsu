# Audit 4 — filesystem and command authority

Baseline: clean `af20c26`. Live scenarios covered safe/global modes, independent
roots and revocation, protected hard-link aliases, exact commands, strict policy
failure, background delegation, and stateless artifacts.

## What worked

- `--yes` did not create capabilities; read/write/read-write stayed independent.
- Runtime revocation was immediate and provenance remained visible.
- The dangerous bypass remained explicit and could not read protected Kitsu
  state, including through a hard-link alias.
- Exact command grants bound argv, cwd, resolved executable, and fingerprint.
- Policy denial was typed, rolled back, counted locally, and exited 1.

## Findings

1. **Medium — failed turns publish an assistant answer before strict failure.**
   JSONL emitted `assistant RECOVERED_AFTER_TOOL`, then `error kind=policy` and
   `turn_finished status=error`; human output likewise left a success claim
   visible before rollback. Output is emitted around `main.rs:4707,6051`, before
   strict classification at `main.rs:6057`.
2. **Medium — stateless command artifacts have no aggregate lifecycle bound.**
   Repeated `--no-session` commands retained separate mode-0600 artifacts. The
   16 MiB limit is per file; there is no count/quota/TTL/list/prune path
   (`command.rs:47`, `main.rs:6750,6787`).
3. **Medium — reasoning-only background delegation requires an unrelated local
   capability.** Without filesystem/command authority `spawn_agent_bg` was not
   registered; adding a dummy read grant made it work (`main.rs:6703`).
4. **Medium — exact command pregrants are fixed to launch cwd.** JSON argv cannot
   encode the intended cwd, so a valid command root in another directory is
   rejected (`cli.rs:266`, `main.rs:6610-6643`).
5. **Low — human policy summaries cut mid-token without an ellipsis.**

Top priorities: transactional assistant output, artifact retention controls, and
tool-free delegation without dummy authority.
