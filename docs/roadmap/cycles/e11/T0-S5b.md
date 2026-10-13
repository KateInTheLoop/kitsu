# E11 / T0-S5b — the styled PTY harness, and the answer to Q3

- **UTC start/end:** 2026-10-13T22:37Z / 2026-10-13T23:21Z
- **Branch/base:** `features/tui_herdr_improvements` / `e6bcc4e`
- **Epic/story/card:** E11 / T0-S5b. **T0-S5b was split again in this commit**
  into the harness and measurement (this card), acting on the measurement
  (S5c), and the idle input path (S5d).
- **Requirement:** none directly. This card's deliverable is **infrastructure
  plus an answer**, which is why it has no implementation half.

- **Why a measurement card.** Finding F17 said the seam's unit tests prove
  routing but not exclusivity, and `main.rs` holds 392 direct writes that
  bypass the seam entirely. Nobody knew whether a stray write corrupts the
  viewport or flows harmlessly into the scroll region — that is the proof of
  concept's **Q3**, and the expensive branch (routing hundreds of writers) hung
  on the answer. The existing PTY harness could not tell us, because it
  hardcodes `--plain`, which disables styling and the viewport outright. So the
  author was told to **measure and report rather than force a pass**, and I
  deliberately did not tell it what I expected.

- **The answer: a bypassing writer does not corrupt the viewport.** A real
  binary on a real pseudo-terminal at 24x80, viewport installed, all ~40 lines
  of `/help` printed through direct writes, screen sampled **before** teardown:

  ```
  21: "current model: example-model-v1 — /help <command> explains one command…"
  22: "kitsu> "
  23: "╰ model example-model-v1 │ cwd ~                                    ╯"
  24: "kitsu"            <- the status row the bridge painted, intact
  ```

  The mechanism is the elegance of the region approach: a direct write inherits
  the terminal's cursor, the cursor is parked **inside** the DECSTBM region, and
  the terminal scrolls that region on a line feed regardless of who wrote the
  bytes. Bypassing the seam costs the viewport's CRLF and any future
  suspend/repaint coordination — not pixel correctness. **So S5c does not need
  to route hundreds of writers**, which removes the worst branch Q3 threatened.

  **With one condition that must not be dropped when this is quoted:** it was
  measured at idle **in cooked mode**. Nothing has entered raw mode for the
  session yet — rustyline enters it only while reading a line — so `ONLCR` is
  still supplying the carriage return those writers omit. Once S5d owns stdin in
  raw mode they lose it and will staircase, exactly as F14 describes. The
  harness must be re-run then.

- **A second finding the harness produced, in code I accepted one card ago**
  (**F18**, deterministic rather than racy): the session's closing summary line
  is printed **after** the bridge guard releases the scroll region. Resetting
  the DECSTBM margins homes the cursor, so that line lands on **row 1 and
  overwrites the top of the transcript**, leaving surviving debris from the
  overwritten row beneath it. The author verified it against the prefix parse —
  cursor on row 12 immediately before the release, line rendered at rows 1 and
  2. A smaller defect alongside: `hide()` releases the region without clearing
  the viewport rows, so stale status text remains after exit.

  The card **pins the broken behaviour** in a characterisation test rather than
  fixing it, so it cannot change silently; S5c inverts that test, which is what
  makes it the failing test for the fix.

- **The harness.** `crates/kitsu/tests/tui.rs`, unix-only, with `vt100` as a
  `kitsu` dev-dependency (0.15 to match, since 0.16 conflicts with ratatui's
  exact `unicode-width` pin; zero new packages in the lockfile, verified).
  Design points worth keeping:
  - It sets the window size with `TIOCSWINSZ` **before** spawning, so the
    child's own size probe reports 24x80 and the viewport arithmetic is
    deterministic rather than inherited or an accidental fallback.
  - It runs **without** `--plain` and **without** `--no-stream`, so styling,
    the banner, rustyline and the viewport are all live — the thing the old
    harness could never do.
  - Input is **trigger-driven, not timed**: each step waits for a substring of
    real output before sending the next line, with one global deadline that
    kills the child and panics with the step and full transcript. No fixed
    sleeps for synchronisation.
  - `screen_at(mark)` replays the byte prefix into a fresh parser, giving the
    screen **mid-session**. That is essential here, because the exit screen is
    rewritten by the F18 teardown bug; any viewport assertion must use it.
  - The row-24 assertion is guarded by requiring at least twenty non-blank rows
    above it, so it cannot pass vacuously on an empty screen.
  - No scrollback modelling, deliberately: per F13 the emulator discards
    region-scrolled rows anyway, so scrollback assertions need a real terminal
    matrix rather than this harness.

- **Acceptance commands/results:**
  ```
  cargo test -p kitsu --test tui             4 passed  (new)
  cargo test -p kitsu --test tui --release   4 passed
  cargo test -p kitsu --bins               643 passed  (unchanged)
  cargo test -p kitsu --test cli           229 passed  (unchanged)
  cargo test -p kitsu-render               112 passed  (unchanged)
  cargo test -p kitsu-tui                   22 passed  (unchanged)
  ```
  Verified independently: **no production code changed at all**, and the
  dependency added zero new packages. The author ran the file five times with
  no flakes and nothing had to be marked unreliable.

- **Review findings and fixes:** none needed. The author confirmed the
  classic-mode test passed **before** interpreting the inline measurement,
  which is the right discipline for a card whose whole output is an
  observation — an unvalidated harness would have made every conclusion
  worthless.

- **Documentation (standing requirement, in this commit):** F17 answered with
  the evidence and the cooked-mode condition stated; F18 recorded with both
  fixes it owes; the plan's T0-S5b split three ways with the reason, and its
  progress table updated. **No `CHANGELOG.md` entry and no
  `docs/ux-design.md` change, deliberately:** this card adds tests only and
  changes no behaviour.

- **Commit:** see below.
- **Residual risk:** the Q3 answer is conditional on cooked mode and must be
  re-measured after S5d. F18 is a real user-visible defect in inline mode
  today, which is one more reason the preview stays gated. The harness's
  triggers are user-visible strings, so rewording the banner or the help footer
  makes it time out — with a clear message rather than a hang, but it is a
  coupling a later card should know about.
- **Next card:** T0-S5c — fix F18 by ordering the guard to outlive the closing
  summary and making `hide()` clear the viewport rows, inverting the
  characterisation test.
