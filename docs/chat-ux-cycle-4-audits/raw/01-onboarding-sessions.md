# Audit 1 — onboarding and session lifecycle

Baseline: `c485edb` on 2026-08-10. The auditor used disposable homes,
provider-free lifecycle commands, and a localhost no-auth mock. No repository
files or real credentials were used.

## What worked

- `init` creates a secret-free, mode-0600 configuration and refuses to clobber
  an existing file.
- Offline and JSON doctor paths are read-only and redact secrets.
- Scripted and piped durable sessions preserve successful turns, reset runtime
  grants on resume, and support provider-free list/show/fork/export/archive,
  restore, and delete operations.

## Ranked findings

1. **High — online doctor does not prove the selected model is usable.** A
   generated configuration selected `qwen2.5-coder:7b`; the reachable catalog
   exposed only `qwen2.5-coder:latest`. `doctor --online` exited successfully
   with no blockers, while the first ask failed model-not-found. Doctor merely
   accepts a successful catalog response and reports its raw length; normal
   model listing normalizes and deduplicates it.
2. **High — the printed “exact” resume command is not portable.**
   `sessions resume last` omits the saved working directory. Running it from a
   different repository immediately fails the repository guard, and JSON does
   not expose the directory needed to construct a working command.
3. **High — zero-turn snapshots can hijack `last`.** EOF at an empty prompt, a
   failed first provider turn, and an otherwise empty durable startup create a
   newer empty session. Subsequent `resume last` selects it instead of the last
   conversation containing useful work.
4. **High — the generated quick start is internally inconsistent.** `kitsu
   init` writes `./kitsu.toml`, while its advertised bare `kitsu doctor` follows
   the default user-config lookup and may inspect a different file.
5. **Medium — session help and scripting promises are uneven.** `/help
   sessions` is unsupported, several session slash commands advertised as
   scriptable are interactive-only, and child help is crowded by irrelevant
   globals.
6. **Medium — archived sessions are recoverable but hard to discover.** There
   is no ordinary archive listing surface.
7. **Medium — `doctor --online` can exit zero when the online probe is skipped.**
8. **Low — doctor repeats some failures in both detailed and summary output.**

## Top improvements

1. Validate the exact effective model against the normalized live catalog.
2. Include saved cwd in human and JSON resume output and emit a runnable command.
3. Defer durable snapshot creation until the first successful or explicit save.

