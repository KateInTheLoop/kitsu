# Audit 4 — filesystem and command authority

Baseline: `c485edb`. The auditor exercised safe `--yes`, the explicit global
bypass, independent roots, runtime grant/revoke, protected paths, exact command
pregrants, and targeted alias/retarget tests. No real credentials or repository
edits were used.

## What worked

- `--yes` is scoped, does not create capabilities, and never approves commands.
- Protected paths override explicit roots, prompts, and the dangerous bypass.
- Symlink destinations are resolved, dangling write links fail closed, Unix
  writes pin their parent, and background prompts fail closed.
- Command grants bind argv, cwd, and observed executable identity and are
  revalidated immediately before spawn.
- `/permissions` distinguishes invocation, runtime, revoked, protected, and
  internal authority and warns that command cwd roots are not sandboxes.

## Ranked findings

1. **Medium — a newly created protected file can escape identity tracking
   through an unobserved hard-link/unlink sequence.** If a protected directory
   starts empty, another process can create a private file, hard-link it outside,
   and remove the protected name before Kitsu first observes an `nlink > 1`
   candidate. The remaining alias has one link and was never entered into the
   protected inode set.
2. **Medium — an unresolved exact command appears as an active grant.**
   `--allow-command '["missing-program"]'` succeeds at startup and is listed in
   `/permissions`, although execution later rejects the missing identity. The
   renderer also omits observed path, PATH resolution, and fingerprint status.
3. **Low-medium — pregrants always target launch cwd.** There is no encoded cwd
   in the grant format, so noninteractive multi-directory command automation
   requires restarting Kitsu from each directory.

## Top improvements

1. Observe private-file identities atomically when Kitsu creates them.
2. Reject unresolved command pregrants and display executable identity state.
3. Add an explicit cwd to encoded exact command grants.

