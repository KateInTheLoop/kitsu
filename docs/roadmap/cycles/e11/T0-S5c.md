# E11 / T0-S5c — fix the inline teardown so later output lands below the transcript

- **UTC start/end:** 2026-10-13T23:10Z / 2026-10-13T23:52Z
- **Branch/base:** `features/tui_herdr_improvements` / `09470df`
- **Epic/story/card:** E11 / T0-S5c
- **Requirement:** R2. Fixes finding **F18**; records **F19**.

- **The defect, caught by the previous card's harness in real output.** The
  session's closing summary is printed **after** the bridge guard releases the
  scroll region, and releasing a DECSTBM region **homes the cursor** — so the
  line landed on row 1 and overwrote the top of the transcript, leaving debris
  from the clobbered row beneath it. Separately, `hide()` released the region
  without clearing the viewport rows, so stale status text survived exit.

- **Why the fix is in `hide` and not in the ordering.** I checked before
  briefing: the summary is emitted from `main` via `emit_run_timing` *after*
  `run_chat` returns, while the bridge guard is a `run_chat` local. Making the
  guard outlive the summary means hoisting it across the process-finalisation
  path, which the terminal-ownership audit describes as having a single
  carefully-owned owner. So instead `hide` leaves the cursor somewhere sane and
  a later write lands correctly **by construction rather than by luck of
  ordering** — which is the more durable property, since it holds for any future
  writer on the exit path, not just this one.

- **The sequence, and why its order is load-bearing.** For bottom `b` and `n`
  viewport rows: clear `b+1 ..= b+n` ascending, then `ESC[r`, then park on
  `b+1`. The clears must precede the release because afterwards the cursor is
  homed and the geometry is no longer latched; the park must follow the clears,
  because parking on a row that still holds painted text would merely move the
  same bug down the screen. A test pins the ordering specifically, not just the
  final bytes.

- **`Inner` now latches `geometry: Option<(u16, u16)>`** — one field rather
  than two, so the bottom row and the row count cannot desynchronise; `hide`
  must clear exactly the rows `show` pinned. `hide` with nothing shown is now
  wholly inert, where before it emitted `ESC[r` unconditionally.

- **Evidence and expected failure:** 6 new unit tests plus one inverted
  end-to-end test. The unit tests derive the expected bytes from geometry via a
  private `expected_hide(rows, viewport_rows)` helper **and** cross-check it
  against the card's literal for the 24/1 case, so the helper and the spec
  validate each other. Failures: `error[E0609]: no field 'geometry' …
  available fields are: out, err, bottom`, and behaviourally the end-to-end
  test printing the offending screen with the summary on row 1, the clobbered
  row's debris on rows 2-3, and the stale status row on 24.

- **The inversion was authorised and is audited.** The previous card
  deliberately pinned the broken behaviour in a characterisation test so it
  could not change silently; inverting it is this card's failing test. The
  author quoted the removed and added assertions rather than asking to be taken
  on trust. Two of its choices are better than what I specified: it asserts the
  summary is *below the transcript and the last content on screen* rather than
  on an exact row, because the line is 81 columns wide and wraps, so a literal
  row would really pin the length of a run id; and it asserts the transcript is
  intact *positively*, via `/help`'s own footer surviving and sitting above the
  summary, which is non-vacuous because pre-fix the summary sat above it.

- **Review findings and fixes:**
  1. **My oversight, caught by the implementer, which stopped rather than
     working around it.** `cargo test -p kitsu --bins` came back 642/643: a
     test I committed two cards ago asserted `hide`'s stderr tail was *exactly*
     `ESC[r`. That is an old-behaviour snapshot which this card necessarily
     invalidates, and my brief had even said to expect that count unchanged.
     The implementer correctly refused to edit a test and handed it back.
     Fixed as reviewer, and **not** by updating the literal: the test's own doc
     comment says its concern is that the region is released and the surface
     restored, while the exact bytes belong to `InlineSurface::hide`, which
     pins them against its own geometry. Duplicating them in the bridge test is
     what broke. It now asserts the invariant it meant to assert, with a
     comment recording why it no longer snapshots.
  2. Verified by reading rather than by report: `show`'s geometry arithmetic is
     untouched, and `show`, `paint` and `transcript_line` emit identical bytes
     — seven pre-existing tests covering them pass unmodified.

- **Acceptance commands/results:**
  ```
  cargo test -p kitsu-tui / --release        28 passed each  (22 + 6)
  cargo test -p kitsu --bins / --release    643 passed each  (restored)
  cargo test -p kitsu --test tui / --release  4 passed each  (was 3 of 4)
  cargo test -p kitsu-render                112 passed       (unchanged)
  cargo test -p kitsu --test cli            229 passed       (unchanged)
  cargo clippy --workspace --all-targets      0 warnings
  cargo fmt -p kitsu -p kitsu-tui -- --check  clean
  workspace total, all targets             1860 passed, 0 failed
  ```

- **Documentation (standing requirement, in this commit):** F18 fixed and the
  resolution recorded; **F19** opened for a pre-existing `show` bug the test
  author found while establishing what `hide` may assume (see below); the plan's
  progress table updated. **`docs/ux-design.md` gained a whole subsection** on
  the inline viewport — its own rule is that code and that file are fixed in the
  same change, and I had let it drift for two cards: nothing there said the live
  status row now stands down under the viewport, nor that the two are mutually
  exclusive. It now documents the region geometry, the exclusion and its reason,
  the measured fact that bypassing writers flow into the region harmlessly
  *with* its cooked-mode condition, and that tiering is unchanged. Still no
  `CHANGELOG.md` entry: the renderer remains gated and unreleased, so there is
  no user-visible change to announce.

- **Commit:** see below.
- **Residual risk:** **F19**, found but deliberately not fixed — when the
  terminal is no taller than the viewport, `show`'s `saturating_sub` clamps the
  bottom to 0 and emits `ESC[1;0r`, an invalid region, with the viewport rows
  then overlapping the region's top. Unreachable today at one row, reachable as
  soon as the composer grows to the three or four rows the design calls for.
  Fixing it changes `show`'s bytes and needs a decision this project has not
  made — fall back to the classic renderer on an impossibly short terminal, or
  run with no status row — so it is recorded rather than guessed at. Also: the
  cursor is left mid-screen with no trailing newline, which is fine because the
  summary path ends with CRLF, but it is a coupling.
- **Next card:** T0-S5d — the idle input path: raw stdin to the extracted
  editor, Enter submitting through the existing prompt path, and the reader
  pausing while an approval prompt owns stdin. That is also when the Q3
  measurement must be re-run, since a session holding raw mode throughout loses
  the carriage return the terminal currently supplies.
