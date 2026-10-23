# E11 / T5-S1b + T5-S2 — fullscreen becomes reachable, after two stops and a rejection

- **UTC start/end:** 2026-10-15T18:00Z / 2026-10-15T19:40Z
- **Branch/base:** `features/tui_herdr_improvements` / `2cd3559`
- **Epic/story/card:** E11 / **T5-S1b merged with T5-S2**, after I rejected the
  feed alone. Records **F90**–**F94**; sharpens **F92**; **R2 complete.**
- **Requirement:** **R2** complete, **N4** and **N5** amended.

- **What shipped.** `/tui fullscreen`, `--tui fullscreen` and
  `[tui] mode = "fullscreen"` all work; the alternate screen is taken and given
  back; the title is pushed and popped; the view is painted from the cell ring —
  **fed and read in one commit.**

- **Why it was one card.** I rejected T5-S1b for feeding a ring nothing could
  read (**F89**), and **the premise was mine**: I wrote that T5-S1a *"shipped the
  reader"* when it had shipped a **pure view with no caller**, which I had
  accepted on exactly that basis. **A reader that exists is not a reader that
  reads.** HEAD never entered that state, which is the point of rejecting rather
  than following up: a follow-up would have left a commit where the ring is fed
  and unreadable.

- **The card's real cost was retiring a boundary, not filling a gap (F90).** My
  brief said *"`fullscreen` is the one spelling it lacks."* False: it was a
  **deliberate, multiply-pinned refusal** — an arm calling it *"a mode the
  design's grammar has"*, twelve literal call sites, four tests, and an asserted
  `/help` string saying it was unavailable. **"X is missing" and "X is refused"
  look identical from outside and are opposite facts.**

- **Two stops, both correct, both cheap.** Seven test collisions in total; six
  authorised by name. The seventh was found *after* everything else was
  implemented and was not a collision at all but a **false premise in the
  acceptance test** — see below.

- **F91 — a false entry in my own findings file, which travelled four hops.** F86
  said *"the alternate screen needs no code at all; `TuiRuntime::enter`/`leave`
  already do the alt-screen, title and raw-mode lifecycle."* **False**: `enter`
  writes the title push, the OSC title and raw mode; `ESC[?1049h` appears only
  in the agent-view overlay, since T4-S2. An agent read it off a doc comment's
  **summary**, I recorded it unverified, it went into my brief, and the next
  agent repeated a corollary. **A findings entry is read by every later card as
  settled fact, so a false one is worse than a false brief** — a brief gets
  questioned; this file gets quoted. Corrected in place.

- **F92 — the ring does not record what anyone assumed, and it took two
  measurements.** The acceptance test required a marker written while inline to
  reappear after the alternate screen opens. It failed **twice, for two different
  reasons**, each settled by a disposable print rather than an argument:
  1. `eprintln!` expands to `std::eprintln!` — straight to fd 2, never through
     the surface. Ring retained **0**.
  2. Re-scripted onto a streamed answer, the ring held **one empty line**: with
     tools advertised (**six by default**) text is **buffered**, and
     `print_message` writes **directly to stdout** inside the live-row
     suspension.

  **So the ring holds the *streamed* conversation, and what lands in it depends
  on whether tools were offered that round.** Nobody would guess that from N4,
  and it is what a user scrolling back actually meets. It sharpens **F51** from
  the other side: buffering was recorded as a decision about *visible* behaviour,
  and it also decides what the scrollback **contains**.

  **The cheap fix was refused, correctly.** Routing diagnostics into the ring
  looks like a one-place macro change; it would re-route several hundred call
  sites, change inline **and** classic bytes, reorder diagnostics against
  `println!`, and overfill the ring — with **309 tests** pinning those bytes.
  *A one-line change with a three-hundred-test blast radius is not a small
  change*, and it is a requirements change disguised as a wiring fix. The test
  ships with `--no-tools` instead.

  **A third discovery, written into the script:** triggering `/tui` on the
  turn-done marker is refused as mid-turn, because that marker is printed *by*
  the turn. `/quit` is not gated that way, which is why every other streaming
  script can trigger on it and this one cannot.

- **F93 — why not `TuiRuntime`, which is stronger than my correction.** `enter`
  also calls crossterm's `enable_raw_mode`, clearing `OPOST`, and the overlay's
  own comment records that this would **staircase several hundred direct
  writers** down the screen; it would also nest a second raw mode under a guard
  whose dying-signal restore snapshots termios. So the segment writes the four
  sequences itself. **A runtime that bundles two capabilities is unusable by a
  caller that wants one of them.**

- **F94 — a hole in the mode this card ships, flagged not widened.** A dying
  signal or panic in fullscreen **leaves the alternate screen taken**;
  `install_exit_restore` gives back the inline region only. N5 demands restore on
  every exit path including panics. **Met for inline, open for fullscreen** —
  carded, and N5 now says so rather than reading as satisfied.

- **Acceptance commands/results:**
  ```
  cargo test -p kitsu --bins, both profiles   928 passed, 0 failed
  cargo test -p kitsu --test tui, both         64 passed, 0 failed
  cargo test -p kitsu --test cli              245 passed
  cargo test -p kitsu-tui / -render / -editor 113 / 138 / 42 (+9)
  cargo test -p kitsu-tools --lib             413 passed
  cargo test -p kitsu --test paste_latency       4 passed
  cargo clippy --workspace --all-targets -- -D warnings   0 warnings
  cargo clippy --workspace -- -D warnings                 0 warnings
  ```
  **Non-vacuity here is a measurement, not an argument:** the repaint assertion
  failed twice today for two different reasons and passes only with a
  surface-routed line present **and** a reader that pulls it out. One more
  discipline worth copying: it passed once **with** instrumentation still in the
  tree, so the agent re-ran after removing it — **a pass obtained with a debug
  print in place is not a pass.** The maintainer's `fs.rs` is at `54 20`
  (**F33**).

- **Documentation (standing requirement, in this commit):** **R2 marked
  complete**, recording that fullscreen was a refusal rather than a gap and that
  the three spellings cross together deliberately. **N4** now states **what the
  ring is a record of** — the streamed conversation, not diagnostics, not a
  buffered answer — and that the macro re-route was rejected. **N5** states the
  per-clause status (title met, alt screen met on a normal switch, signal clause
  **open** for fullscreen) and why `TuiRuntime` is not used. **F90–F94**
  recorded, **F92** sharpened, **F86** corrected in place. `README.md`'s `/tui`
  row and `cell_ring_lines` row, `CHANGELOG.md` and `docs/functionality.md` all
  describe the mode **and** the scrollback's real contents, including the
  `--no-tools` consequence — because a user who scrolls back and finds their
  answer missing deserves the reason in the docs rather than in a findings file.

- **Commit:** see below.
- **Residual risk:** **F94** (signal/panic in fullscreen) is carded and real.
  **T5-S1c**, the paging decoder, is the next card and needs **its own**
  advertisement table and sweep — not `keys::BINDINGS`, which is the agent
  view's. **T4-S9** still needs the ring. Five open flakes. **F72**, the ordinal
  chord, the `r`-decision paste arm, the mid-turn counter freeze, **F67**,
  **F62**, **F60**, **F57**, **F53**, **F54**, **F51**, **F52** are open.
  **Q1's terminal matrix still needs a human.**
- **Next:** **T5-S1c** (paging) or **F94** (the restore hole). F94 is smaller and
  closes a gap in a shipped mode; paging is the feature.
