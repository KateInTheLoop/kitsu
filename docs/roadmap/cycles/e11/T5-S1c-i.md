# E11 / T5-S1c-i — the frame follows the ring, which I had accepted as already true

- **UTC start/end:** 2026-10-15T22:00Z / 2026-10-15T20:36Z
- **Branch/base:** `features/tui_herdr_improvements` / `45451d5`
- **Epic/story/card:** E11 / **T5-S1c-i**, the first of three after the paging row
  split. Closes **F97**; sharpens it twice.
- **Requirement:** **R1**'s "self-scrolled transcript", finally true.

- **What shipped.** A tap over the displaced surface records the lines classic
  prints, and `FullscreenSegment` repaints the frame at the idle prompt. **A
  prompt that follows no new output writes zero bytes**, so an idle fullscreen
  session's byte stream is exactly what it was before.

- **The gap was mine, and I approved it in words that hid it.** I accepted
  *"B without paging is coherent — it shows the tail, which is strictly more than
  today"*. True of the frame **at entry**, and silent about afterwards.
  `FullscreenView` was constructed in exactly one place, which painted once and
  returned. **A one-shot paint and a live tail are different features, and "shows
  the tail" describes both.**

- **And the test I was proudest of could not catch it.** T5-S2's transitions all
  assert what is on screen *around* the switch, and every one is satisfied by a
  single paint — **including the repaint assertion that proves the ring was
  read.** The replacement is the shape to copy: **a single paint produces exactly
  two occurrences** of the retained line (the inline transcript's write, the entry
  frame), so **a third is a repaint**. *A test that proves X happened does not
  prove X keeps happening.*

- **It got worse when measured.** The frame is not merely frozen: **rustyline's
  own helper paints `kitsu>`, the hint row and the full slash-command palette
  directly onto it**, because fullscreen's idle prompt is rustyline in cooked
  mode — the inline composer is reachable only when the viewport install returns a
  guard, and that install is inert for fullscreen. **So the docs I wrote
  described a frame a user never saw intact**, and they are qualified in this
  commit rather than left flattering.

- **Which killed one of my own options.** My brief offered *"decode the paging
  keys in the composer's `KeyFilter`"* — **that surface does not exist in
  fullscreen.** Giving it one is now its own card, and a terminal-state one.
  **Thirteenth mechanism error in my briefs**, and the first where the option I
  offered was impossible rather than merely wrong about a detail.

- **The decision was argued against the requirements, not taste.** Live renderer,
  not modal viewer: R2 keeps *the conversation* and `--tui fullscreen` **starts** a
  session, so a start mode you cannot talk to is *"a pager that must be dismissed
  before kitsu is usable"*; and R1 already says **"self-scrolled transcript"**,
  which is the repaint, not a read loop. The modal shape already exists as the
  agent view, which hard-declines unless the mode is inline.

- **And it confirmed the plan row's instruction was aimed at the wrong surface.**
  I had written that the paging keys need *"a second table with its own sweep"*.
  §4.7 keeps computed-label chords out of `keys::BINDINGS` because they never
  reach the swept dispatch — and the test author added a third reason I had not
  seen: **these keys are live only while fullscreen holds the screen, so a static
  row would advertise them in inline and classic where they do nothing.** The
  honest advertisement is `/tui`'s own `details`, which R2 already pins across
  four spellings.

- **Why not the surface seam, which was the tempting wiring.** The seam is shared
  by all three renderers; repainting from inside it would put inline's transcript
  on the terminal **twice** (R3) and change **classic's** bytes. The seam is used
  only to *see* the lines. A guard test — inline and classic, two streamed turns,
  each marker reaching the terminal **exactly once** — refuses that wiring and
  stays green.

- **Three details the implementation got right unasked.** The tap is installed
  **after** the entry frame's bytes, so the frame's own rows cannot feed back into
  its ring. `write_block` runs against the inner surface's **own writer** through
  a tee that copies only accepted bytes, so classic keeps its single suspension
  and its real descriptor. And the put-back in `leave()` is guarded by
  `Arc::ptr_eq`, so it **cannot displace an incoming renderer** whatever order a
  future path drops the segment in — all state on the segment, one `Drop`
  clearing it, which is **F94**/**F96**'s lesson applied before the fact.

- **Acceptance commands/results:**
  ```
  cargo test -p kitsu --test tui, both profiles    71 passed, 0 failed
  cargo test -p kitsu --bins                      928 passed
  cargo test -p kitsu --test cli                  245 passed
  cargo test -p kitsu-tui / -render / -editor     114 / 138 / 42
  cargo test -p kitsu-tools --lib                 413 passed
  cargo test -p kitsu --test paste_latency          4 passed
  cargo clippy --workspace --all-targets -- -D warnings   0 warnings
  cargo clippy --workspace -- -D warnings                 0 warnings
  ```
  `kitsu-render` is **untouched** (empty diff), so "classic pays nothing" remains
  a compile-time property; no instrumentation was ever added, so **F92** had
  nothing to remove. Two now-inaccurate comments were corrected rather than left
  standing. The maintainer's `fs.rs` is at `54 20` (**F33**).

- **Documentation (standing requirement, in this commit):** **F97** recorded and
  sharpened twice — the snapshot, then the rustyline overpaint and the missing
  `KeyFilter`. **The CHANGELOG and `docs/functionality.md` are qualified**: the
  frame now repaints, *and* the prompt and palette still paint over it, *and*
  paging is not wired. Saying only the first would be the overselling F97 is
  about. The plan row became **three** honest rows.

- **Commit:** see below.
- **Residual risk:** the prompt still overpaints the frame until **T5-S1c-ii**,
  which is the real fix and is terminal-state work. Paging is **T5-S1c-iii**.
  **T4-S9** still needs the ring. Five open flakes. **F72**, the ordinal chord,
  the `r`-decision paste arm, the mid-turn counter freeze, **F67**, **F62**,
  **F60**, **F57**, **F53**, **F54**, **F51**, **F52** are open. **Q1's terminal
  matrix still needs a human.**
- **Next:** **T5-S1c-ii** — fullscreen's own composer, which also stops the
  prompt painting over the frame.
