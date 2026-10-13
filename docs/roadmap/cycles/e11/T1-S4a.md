# E11 / T1-S4 first card — the per-process agent state file writer

- **UTC start/end:** 2026-10-13T17:06Z / 2026-10-13T17:33Z
- **Branch/base:** `features/tui_herdr_improvements` / `06b2508`
- **Epic/story/card:** E11 / T1-S4 / **first card**. The story also covers the
  config key, the turn-lifecycle wiring and the terminal title; those are the
  second card. This card is the writer module alone.
- **Requirement:** H2, and N7 by construction (a local file, no network).
  kitsu runs background agents in-process, so nothing outside can observe them
  and a herder such as herdr has to classify kitsu as `unknown`. This is the
  artifact that fixes that.

- **Evidence and expected failure:** 17 tests written first into a new module
  containing nothing else, with only `mod agent_state_file;` added to
  `main.rs` so the file compiles. Observed 36 errors, among them:

  ```
  error[E0425]: cannot find type `AgentStateFile` in this scope
  error[E0425]: cannot find value `AGENT_STATE_SCHEMA` in this scope
  error[E0422]: cannot find struct, variant or union type `AgentStateCounts`
  error[E0423]: expected function, found macro `write`
  ```

  The test author also verified the whole contract with a throwaway reference
  implementation (17 passing) and reverted it, so the specification was known
  to be implementable before the implementer started.

- **In scope:** path selection, the payload shape, process-state derivation,
  the atomic private write, the directory safety check, and removal.
- **Out of scope:** the `agents.state_file` config key, any call site, the
  terminal title, and the reader (`kitsu agents --json`, story T1-S5).

- **Authority/failure/version/rollback notes:** writes one advisory file into a
  runtime directory. No authority surface, no network, no durable user data.
  The file carries its own `schema` string (`kitsu.agents.v1`) so a reader can
  refuse an unknown version. Nothing calls it yet, so rollback is deleting the
  module. Because a hint file must never break a turn, every failure is an
  `Err(String)` the caller may ignore and nothing in the module panics,
  verified: zero `unwrap`, `expect` or `panic!` in the production half.

- **Tests added or changed:** 17 added, none changed. Path selection is pinned
  as a pure function with the environment injected, so no test mutates process
  environment and none can race another: an absolute runtime directory wins,
  an empty or relative one falls through, and the fallbacks carry the uid so
  two users cannot collide in a shared `/tmp`. Also pinned: the exact JSON
  keys; that `session_id` serializes as `null` rather than being omitted; that
  every embedded thread DTO survives a serialize round trip and still passes
  `validate_default()`, so the file can never carry a record the reader would
  reject; mode 0700 on the directory and 0600 on the file; that two writes
  leave exactly one directory entry and no temporary; refusal of a
  group- or world-writable directory at three different modes; refusal of a
  regular file standing where the directory belongs; an `Err` rather than a
  panic when the directory cannot be created; and that removal is idempotent.

- **Implementation:** `write` encodes the payload before touching the
  filesystem, so an encoding failure leaves nothing behind; creates the
  directory and sets 0700 explicitly rather than trusting the umask; creates a
  `create_new` temporary with mode 0600 at creation; and renames over the
  target, removing the temporary on any failure. The safety check uses
  `symlink_metadata`, never `metadata`, so a symlink standing in for the
  directory is judged as the symlink and refused instead of followed to an
  attacker-controlled target; it then rejects a non-directory, a foreign
  owner, and `mode & 0o022`, all before anything is created.

  `session.rs`'s `atomic_write_private_*` helpers were deliberately not
  reused: they are private to that module and bound to its
  descriptor-relative machinery for durable user data under a user-controlled
  path, whereas this is a best-effort hint in a runtime directory, rewritten
  on every change and deleted on exit. The idioms were reused instead.

- **Review findings and fixes:**
  1. The test author found two constraints its own reference implementation
     hit and passed them on, which saved the implementer the same discovery:
     the check must use `symlink_metadata` to refuse a symlinked directory,
     and `create_dir_all` does not reliably honour the umask so 0700 must be
     set explicitly. Both are in the shipped code.
  2. Verified by reading rather than by report: the safety check's order
     (exists, is-dir, owner, writability) runs before any creation, so a
     refused directory receives neither a state file nor a temporary.
  3. **Accepted with an obligation attached.** The module carries a
     module-wide `#![allow(dead_code)]`, because nothing calls it yet and
     clippy must stay at zero warnings. The precedent it cites in
     `config.rs` is per-item rather than module-wide, which is narrower; nine
     per-item attributes were judged not worth the noise for a one-card
     window. The second card **must delete the attribute** and confirm clippy
     stays clean, which also proves every item actually got wired.
  4. Accepted gap: the ownership refusal has no test, because exercising it
     needs a second uid. It is nonetheless the reason the `/tmp` fallback is
     safe to use at all.

- **Acceptance commands/results:**
  ```
  cargo test -p kitsu --bins             627 passed  (610 baseline + 17)
  cargo test -p kitsu --bins --release   627 passed
  cargo test -p kitsu --test cli         222 passed  (unchanged)
  cargo clippy --workspace --all-targets   0 warnings
  cargo fmt -p kitsu -- --check          clean
  ```

- **Documentation:** none, deliberately. The file is not reachable by a user
  until the second card adds the config key and the call sites, and
  documenting an unreachable surface would be inaccurate.

- **Commit:** see below.
- **Residual risk:** on non-unix the module compiles but its confidentiality
  guarantees degrade — no ownership or writability check, no explicit modes —
  while atomic replacement still holds, so correctness survives and only
  privacy weakens. The ownership branch is untested. And nothing writes the
  file yet, so the story delivers no user-visible value until the second card.
- **Next card:** T1-S4 second card — the `agents.state_file = auto|on|off`
  config key with `auto` meaning interactive TTY sessions only so scripted,
  piped and JSONL runs write nothing (preserving the roadmap invariant that
  optional integrations make no state write while disabled), the call sites on
  registry change and clean exit, and **deletion of the module-wide
  `allow(dead_code)`**.
