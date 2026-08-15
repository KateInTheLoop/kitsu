# Chat UX dogfood — cycle 22 (claude CLI baseline; UI & functionality focus)

Date: 2026-08-12

Baseline: `5f3b2bc` on `dogfood/codex-ux-cycle-6`. Cycle 22 ran the
rotation's Fable slot as a tight ship cycle at the new HOURLY cadence
(loop retimed at the maintainer's direction).

## Shipped this cycle

**Interactive `/resume` picker (the oldest open parity item, queued since
cycle 13).** Bare `/resume` on a TTY lists saved sessions numbered and
name-first (active marked `*`, capped at 9 with a /sessions pointer), then
reads one line: a number, a name, an id, `last`, `auto`, Enter for `last`,
or `q` to cancel. Non-TTY and scripted behavior unchanged (`last`), so
automation contracts hold. Registry details updated; claude's bare
`--resume` chooser now has its kitsu equivalent.

Gates: fmt clean, clippy clean, workspace green (233 bin / 121 CLI with the
known PTY-environmental failure unchanged / 92 core / 139 tools).

## Verification note

The picker is TTY-gated by design; scripted verification covers the
non-TTY fallback (unchanged tests) and next cycle's auditor gets a
PTY-driven walkthrough item for the interactive path.

## Ranked gaps (queued for cycle 23)

1. PTY verification of the picker + a `/name`-style live rename to pair
   with it.
2. In-session `/doctor`.
3. Redundant single-file delegation heuristic.
4. Denial-final disposition redesign (dedicated cycle).
5. Carried: capped-read true size, worktree gitdir binds, python_tag
   parsing, G5 elision, credential-filter redesign, /memory equivalent,
   timer-based /loop.

## Loop state

HOURLY local cron loop live (retimed this cycle). Rotation: cycle 22 =
Fable (this file), cycle 23 = Opus.
