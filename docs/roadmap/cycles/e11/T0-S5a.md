# E11 / T0-S5a — the bridge, the gate, and F15 settled

- **UTC start/end:** 2026-10-13T22:07Z / 2026-10-13T22:56Z
- **Branch/base:** `features/tui_herdr_improvements` / `a7da0c1`
- **Epic/story/card:** E11 / T0-S5a. **T0-S5 was split in this commit**, with
  the reason in both rows.
- **Requirement:** R1 and R2 in part, and it settles finding **F15**.

- **Why the split.** The plan's T0-S5 bundled the gate, the surface install, a
  raw-stdin input path replacing rustyline at idle, approval-pause logic,
  wrapping the slash dispatcher, status painting, a styled PTY harness and a
  paste benchmark into one row. That is not one verifiable change, and the
  harness it needed had been assigned to a much later card — the existing PTY
  harness hardcodes `--plain`, which disables styling outright and therefore
  cannot exercise inline mode at all. S5a takes the gate, the install and F15,
  all unit-testable through `InlineSurface`'s injectable sinks; S5b takes the
  harness, the end-to-end tests, the input path and the dispatcher.

- **F15 is settled: the viewport replaces the live row.** `ClassicSurface`
  wraps writes in a suspension that clears and repaints a row at the cursor,
  while `InlineSurface` pins its viewport outside the scroll region and
  suspends nothing. With both active the live row paints *inside* the region
  and either scrolls away with the transcript or fights the parked cursor. The
  viewport wins because its bottom rows are exactly where the spinner, draft,
  queue tag and ambient agent tag belong — so running both would have been
  duplication before it was a conflict. `kitsu-render` gained
  `set_live_rows_enabled`, which `live_rows_supported` now consults, defaulting
  enabled so a session without the viewport is byte-identical.

- **Evidence and expected failure:** 7 tests written first — 6 on the bridge, 1
  in the crate that owns the gate. `cannot find function set_live_rows_enabled`
  and `unresolved imports super::install, super::mode_from_value,
  super::BridgeGuard, super::TuiMode`. The author verified all seven against a
  throwaway stub (112 and 643) and reverted it.

- **The gate fails closed, deliberately.** `mode_from_value` is byte-exact and
  case-sensitive with no trimming: only `"inline"` selects the viewport, while
  `"INLINE"`, `"Inline"`, `"inline "`, the empty value and an absent variable
  are all classic. A typo must leave a user in the renderer they already had,
  and a forgiving match makes the set of values that silently switch renderers
  unbounded.

- **Two ordering constraints, pinned by tests rather than convention:** install
  does `show` then `paint` **before** `set_surface`, so no transcript line can
  interleave with the geometry bytes; and the guard's drop restores the previous
  surface **before** hiding, so the seam points away before the region reset is
  written. Either reordering breaks a test, which is the intent.

- **Review findings and fixes:**
  1. **I was wrong and the implementer was right about the platform
     attribute.** I told it the module's doc comment claimed an inner
     `#![cfg(unix)]` that was not actually present, and asked it to add the
     real one. It checked with `cat -A` rather than trusting me and reported
     the attribute was already there at line 46. It was. My grep had been piped
     through `head -3`, which showed three doc-comment mentions and hid the
     fourth match — the attribute itself. **This was the third truncation error
     of the session**, after the same mistake produced a wrong "no test calls
     this method" claim in T0-S3c. Recorded as a working rule: never assert
     absence from a truncated search, and never hand such a claim to an agent.
  2. **A dependency I authorised against my own constraint.** The implementer
     added `crossterm` to `crates/kitsu` for `terminal::size()`, flagged it
     against the card's "add no dependency" rule, and offered a `libc` ioctl
     alternative. Accepted: I had directed it to that function, crossterm 0.28
     is already in the build graph via `kitsu-tui`, and the lockfile gained
     **zero new packages** — verified. A raw ioctl would avoid the manifest
     churn at the cost of clarity, which is the worse trade.
  3. Verified by reading rather than by report: the guard stores and restores
     the **previous** surface and the **previous** live-row setting, not
     hardcoded defaults, which a test proves by installing over an already
     disabled baseline.

- **Acceptance commands/results:**
  ```
  cargo test -p kitsu-render            112 passed  (111 + 1)
  cargo test -p kitsu-render --release  112 passed
  cargo test -p kitsu --bins            643 passed  (637 + 6)
  cargo test -p kitsu --bins --release  643 passed
  cargo test -p kitsu --test cli        229 passed  (unchanged)
  cargo test -p kitsu-tui                22 passed  (unchanged)
  cargo clippy --workspace --all-targets  0 warnings
  cargo fmt -p kitsu -p kitsu-render -- --check  clean
  workspace total, all targets         1850  (1843 + 7, nothing lost)
  ```
  Inertness: the CLI suite runs scripted with `KITSU_TUI` unset and is
  unchanged at 229, so the gate is byte-identical by default.

- **Documentation (standing requirement, in this commit):** F15 marked
  resolved with the reasoning that decided it; new finding **F17** recording
  that in inline mode any writer bypassing the seam corrupts the viewport, and
  why only the next card's harness can observe it; the plan's T0-S5 split into
  S5a and S5b with the harness pulled forward and the reason stated; the
  progress table updated. **No `CHANGELOG.md` entry and no
  `docs/ux-design.md` change, deliberately:** with the variable unset nothing
  changes, and the preview renderer draws only a region and one status row, so
  there is no user-facing behaviour to describe yet.

- **Commit:** see below.
- **Residual risk:** **F17 is the one that matters.** These tests prove the
  seam *routes*, not that it is the only path to the terminal, and a stray
  `eprintln!`, a panic message, rustyline's prompt or `tracing` on stderr will
  land inside the scroll region or on top of the viewport. That is the sharpest
  form of the proof of concept's Q3, which already counts 392 direct writes in
  `main.rs`. Inline mode must stay gated and undocumented as a recommendation
  until the census. Also: inline mode currently has no during-turn input row at
  all, since the live row stands down and nothing yet paints a draft — that is
  S5b's job, and it is why this card is a preview rather than a usable mode.
- **Next card:** T0-S5b — the styled PTY harness, then `/help`, `/jobs` and
  `/status` at idle leaving the viewport intact, the idle input path, and
  wrapping the slash dispatcher.
