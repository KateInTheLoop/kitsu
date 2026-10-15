# E11 / T5-S1c-ii — fullscreen's own composer, and the third registration gap

- **UTC start/end:** 2026-10-15T23:30Z / 2026-10-15T21:19Z
- **Branch/base:** `features/tui_herdr_improvements` / `ce6b55e`
- **Epic/story/card:** E11 / **T5-S1c-ii**, second of three. Closes **F98**;
  records **F99**.
- **Requirement:** **R1**'s fullscreen surface; **N5**'s registration completed.

- **What shipped.** `read_chat_line_fullscreen` — the inline reader reused with a
  different `paint` closure. The composer draws **only the bottom row** and
  **never a line feed**, so typing cannot scroll the frame **by construction
  rather than by repainting**. Measured: the rustyline hint row went **12 → 0**
  occurrences on the alternate screen, and frame rows after a palette-opening
  draft is typed and taken back out went **9 → 21** against 21 at the switch —
  equality, so nothing scrolls at all.

- **Measuring the seam first made the card small.** `read_line_inline_with_keys`
  is generic over the reader and takes `paint` and `on_resize` as **closures**;
  its body touches no guard, pane or surface. The guard's role is entirely in the
  *caller*. So the work was a `paint` closure plus `RawStdinGuard`, not "build a
  composer" — and the implementer confirmed it line by line before starting.

- **Two deliberate differences from inline, both for reasons.** **No slash
  palette**: the only rows it could use are the frame's. **Both agent-view key
  routes off**: they lead to a surface that wants the same alternate screen this
  segment holds — which also makes the reader's `OpenAgentView`/`Detached`
  outcomes unreachable here, handled by replaying the draft rather than falling
  into `Eof`.

- **F98 — the third registration gap, and the sharpest form of the lesson.** The
  `/tui` switch **into** fullscreen called `enter_fullscreen` plus an `eprintln!`
  **and nothing else**, so the **classic → fullscreen** route left the session on
  the **default signal disposition**. Verified. Now registered, `Once`-gated, as
  the third of three sites.

  **Every existing signal test entered fullscreen by a route that already had a
  handler** — from inline, or by starting in fullscreen per F96. **The one route
  nobody had taken was the one with the gap.** F94 taught that *exit paths are
  enumerable*; this is the other half: **the paths into a state are enumerable
  too, and a restore installed on entry is only as good as the entries that
  install it.**

  **Kept in this card rather than split**, because it becomes a **termios** hole
  as well the moment this surface takes raw mode — which is exactly what this
  card does. Splitting would have shipped a card that creates the second half of
  a defect it had already found.

- **F99 — an agent applied F97's lesson to its own tests, twice, before review.**
  Its first frame assertion was *"a keystroke must not cost the frame a row"*;
  **measured**, rustyline costs **zero** rows for a plain draft, so that test
  would have been green and proved nothing — the defect needs a **palette**. And
  its **decoder test passed on its first run**, because "a draft is painted under
  the frame" is something rustyline satisfies. Both were replaced rather than kept
  as green "failing tests".

  **The practice worth keeping: before writing a failing test, measure what the
  current code actually does to the thing you are about to assert.** A test
  derived from a *description* of the defect can be satisfied by the defect; one
  derived from a *measurement* of it cannot. I only caught this class of mistake
  **after** accepting a card (F97); this is the first time it was caught before
  hand-off.

  The discriminator is the other half: the `model … cwd` hint row has **exactly
  one producer**, whose only caller is rustyline's hinter, and the inline painter
  never writes it. **That row on the alternate screen *is* rustyline reading the
  surface** — by construction, not a substring guess (**F41**).

- **New state, and one clearer each.** Raw termios and bracketed paste:
  `RawStdinGuard` in an `Option` owned by the reader frame, dropped on every
  return, taken out before a suspend — verified by the paste-on/paste-off counts
  balancing and a post-session termios probe. The composer row:
  `clear_composer` before the reader returns. The whole screen:
  **`FullscreenSegment::drop`**, the single clearer, on a switch away, `/quit`
  and panic unwinding, with the dying signal covered by the registration above.

- **`Ctrl+Z` routed through the existing seam** rather than a second ordering, so
  *"termios canonical before the stop"* stays **one property with one
  implementation**. Honestly noted: **no test covers the suspend on this surface**
  and none was added, and the alternate screen's contents are the terminal's
  across the stop.

- **A clippy-driven change that is better anyway:** `paint_composer` first took
  five draft fields plus geometry and tripped `too_many_arguments`. Rather than
  add the first `#[allow]` in that file, it now takes the renderer's own
  `LiveDraft` — **placement is the method's business, content is the shared value
  type.**

- **Acceptance commands/results, both profiles:**
  ```
  cargo test -p kitsu --test tui, both     74 passed, 0 failed  (71 + 3)
  cargo test -p kitsu --bins, both        928 passed
  cargo test -p kitsu --test cli, both    245 passed
  cargo test -p kitsu-tui, both           114 passed
  cargo test -p kitsu-render --lib, both  138 passed
  cargo test -p kitsu-editor, both         42 passed (+9)
  cargo test -p kitsu-tools --lib, both   413 passed
  cargo test -p kitsu --test paste_latency, both   4 passed
  cargo clippy --workspace --all-targets -- -D warnings   0 warnings
  cargo clippy --workspace -- -D warnings                 0 warnings
  ```
  The last card's two guards stayed green, and so did all four T5-S2 transitions.
  No flake. The maintainer's `fs.rs` is at `54 20` (**F33**). The implementer
  noted this was *"the first brief in a while that was right end to end"* — after
  thirteen that were not.

- **Documentation (standing requirement, in this commit):** the **CHANGELOG** and
  **`docs/functionality.md`** are **re-qualified**: the prompt no longer paints
  over the frame, typing cannot scroll it, the palette is deliberately absent and
  the agent-view keys are off — **and paging is still missing**. Last card I had
  to qualify these in the other direction; saying only the good half would be the
  overselling **F97** is about. **N5** records the third registration gap and the
  entrances-are-enumerable lesson. **F98** and **F99** recorded.

- **Commit:** see below.
- **Residual risk:** **no test covers `Ctrl+Z` on this surface**, and the
  attached-agent routing and `agent N>` label are absent here (rustyline did not
  route them either, so not a regression, but a known gap). Paging is
  **T5-S1c-iii**. **T4-S9** still needs the ring. Five open flakes. **F72**, the
  ordinal chord, the `r`-decision paste arm, the mid-turn counter freeze,
  **F67**, **F62**, **F60**, **F57**, **F53**, **F54**, **F51**, **F52** are
  open. **Q1's terminal matrix still needs a human.**
- **Next:** **T5-S1c-iii** — the four paging keys on this decoder, advertised in
  `/tui`'s own `details` rather than a second table.
