# E11 / T0-S3a — the `Surface` seam in `kitsu-render`

- **UTC start/end:** 2026-10-13T20:07Z / 2026-10-13T20:57Z
- **Branch/base:** `features/tui_herdr_improvements` / `e325998`
- **Epic/story/card:** E11 / T0-S3a. **The plan's T0-S3 was split into S3a and
  S3b in this commit**, with the reason recorded in both rows.

- **Why the split.** T0-S3 as planned routed everything through a large trait
  at once. `kitsu-render` owns every byte kitsu prints and the acceptance here
  is byte-identity, so bundling the easy writers with the streaming stdout path
  — which owns a line-buffering styler, two output-tracking atomics and
  byte-count returns — would have risked drift in exactly the code where drift
  matters most. S3a takes the seam and the stderr line writers; S3b takes the
  streaming path, the notice path, and `render_error`.

- **Why this card is the riskiest in the milestone**, and how that was managed:
  byte-identity is achieved **structurally** rather than by careful re-typing.
  `ClassicSurface` writes nothing it formats itself — it suspends the live row
  and prints the string it is handed, and every byte still comes from the
  existing pure formatters. The 82 pre-existing render tests are the net and
  were not touched.

- **Evidence and expected failure:** 11 tests written first. Ten compile errors
  naming exactly the three pieces of production work required:

  ```
  error[E0432]: unresolved import `super::surface`
  error[E0432]: unresolved import `super::tool_result_line`
  error[E0425]: cannot find function `suspend_depth` in module `live`   (x8)
  ```

  The author verified all 11 against a throwaway full implementation (93
  passing) and reverted it, so the specification was known satisfiable before
  the implementer started.

- **In scope:** `Fd`, a one-method `Surface`, `ClassicSurface`, the global slot
  and its lock discipline, the `tool_result_line` extraction, a test-visible
  suspension-depth reader, and routing `tool_activity`,
  `tool_result_with_weight` and `milestone`.
- **Out of scope, all deferred to S3b:** `print_fragment`, `finish_line`, the
  notice path, and `render_error`.

- **The lock discipline is the substance of the card and is structural.**
  `active_surface` clones the `Arc` out of a short-lived read guard, drops the
  guard explicitly, and returns a value that borrows nothing from the lock — a
  caller holds a refcount, never a lock, so it cannot hold what it was never
  given. This matters because the live-row suspension is deliberately
  reentrant, async notices nest inside it, approval prompters run synchronously
  under `spawn_blocking`, and the orchestrator calls these from tokio tasks. A
  guard held across a call while a writer waited would deadlock. **Two tests
  bound that case with deadlines**, so a regression fails rather than hanging
  the suite — a hanging test being worse than a failing one.

- **Three findings from the test author that changed the card:**
  1. **`render_error` cannot be routed per line, and it refused to.** Its body
     prints N lines under **one** suspension; N single-line calls would open N
     suspensions, and with a drawn row the outermost would clear and repaint
     between every hint line, changing the emitted ANSI bytes — the very thing
     this card defends. It needs a `transcript_lines` or suspension-group
     method, now recorded in S3b's row.
  2. **The result writer had no pure formatter**, so the only available
     assertion sanitized away the styling that distinguishes the ordinary and
     error arms, leaving the ember-red error path the least-covered byte path
     in a card about bytes. I required the extraction rather than accepting
     that: `tool_result_line(summary, is_error, styled, plain)` now mirrors
     `tool_activity_line`'s shape, six literal-byte cases are pinned across
     both arms and all three tiers, and `tool_result_error` has its own routing
     test. All three routed writers are now uniform.
  3. **A leaked suspension would have been invisible** — the depth counter is
     private with no accessor, so a `ClassicSurface` that leaked one would
     leave the live row permanently lifted while every test passed. Three
     lines of production code closed it: a test-visible depth reader, with
     tests asserting balance after each routed writer and, in the reentrant
     case, that a nested call is genuinely deeper than its caller and returns
     to the original depth.

  Its other two judgement calls I accepted as-is: `Fd::Stdout` implemented
  symmetrically rather than `unreachable!()`, because the renderer is the one
  component that must never take the process down and a panic-unwind path could
  reach a stdout call; and its own scoped mutex plus an RAII restore guard
  instead of adding a dev-dependency, which also keeps the serialization to
  these 11 tests rather than the whole suite.

- **Byte-identity verified by comparison, not by claim.** I diffed the
  extracted `tool_result_line` against the original body from `HEAD` and
  checked all four tier-and-arm combinations map exactly, including the subtle
  part: the plain branch uses `sanitize_plain_inline` while the body uses
  `sanitize_terminal_inline`, and a plain-but-TTY session still takes the plain
  branch. The only behavioural difference is that the plain case no longer
  computes a line it never used.

- **Acceptance commands/results:**
  ```
  cargo test -p kitsu-render             93 passed  (82 baseline + 11)
  cargo test -p kitsu-render --release   93 passed
  cargo test -p kitsu --bins            637 passed  (unchanged)
  cargo test -p kitsu --bins --release  637 passed
  cargo test -p kitsu --test cli        229 passed  (unchanged)
  cargo clippy --workspace --all-targets  0 warnings
  cargo fmt -p kitsu-render -- --check  clean
  workspace total, all targets         1815  (1804 + 11, nothing lost)
  ```

- **Review findings and fixes:**
  1. One formatting diff in production code the implementer reported clean — a
     single line-length rewrap in the extracted formatter. Applied by the
     reviewer and re-verified; pure whitespace, and the 93 tests still pass.
  2. Confirmed by diff that no deferred function lost a line, so
     `print_fragment`, `finish_line`, `render_error` and the notice path are
     provably untouched rather than merely claimed to be.

- **Documentation (standing requirement, in this commit):** `design.md`
  describes the seam and the lock discipline under `crates/kitsu-render`; the
  plan's T0-S3 row is split into S3a and S3b with each card's gate and the
  `render_error` requirement; the progress table records S3a done. **No
  `CHANGELOG.md` entry and no `docs/ux-design.md` change, deliberately:** every
  byte kitsu prints is identical, so there is nothing user-visible to record
  and nothing in the documented surfaces has changed. That is the whole point
  of the card.

- **Commit:** see below.
- **Residual risk:** real stderr bytes still cannot be captured from a unit
  test, because the routed writers print straight at the process descriptor;
  the tests assert the seam carries the formatter's bytes verbatim and lean on
  the untouched 82 for the formatters, which is the best available short of
  sink injection at the `Surface` level. A `Surface` implementation that called
  `set_surface` from inside `transcript_line` would self-deadlock on the
  non-reentrant write lock; that is guarded by a doc note on the trait rather
  than a test.
- **Next card:** T0-S3b — route the streaming stdout path, the notice path, and
  `render_error` via a multi-line or suspension-group method, against the same
  byte-identity gate.
