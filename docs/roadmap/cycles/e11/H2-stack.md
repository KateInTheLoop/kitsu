# E11 / H2-stack — the title stack belongs to the session

- **UTC start/end:** 2026-10-17T00:30Z / 2026-10-16T16:37Z
- **Branch/base:** `features/tui_herdr_improvements` / `9771e88`
- **Epic/story/card:** E11 / **H2-stack**, opened by **F140** one commit earlier.
  Records **F141**. **H2's deviation 2 closed, with two residuals.**

- **What shipped.** One push when a session first sets a title, one pop when it
  ends. `enter_fullscreen` pushes **only when the session has not**, and a segment
  pops **only what it pushed** — which also makes the dying-signal restore's
  long-unconditional pop the **balancing** one rather than a pop against an empty
  stack. A title set in the default modes no longer survives kitsu's exit.

- **The design was forced, not chosen.** My brief offered the double-push question
  as a free choice; an existing test pins `PUSH == 1` **and** `POP == 1` across a
  fullscreen→classic switch, so two-and-two **fails a test the implementer may not
  edit.** The author found that by mutation and said so.

- **The trap I flagged, and the detector I got wrong.** A session push must not
  reach the plain and assistive surfaces. I pointed at the wrong tests: gated on the
  key alone, the trap version **passes every pre-existing negative**, because on a
  plain *fullscreen* surface the session push merely **replaces** the segment push
  and the count stays `1`. A **new classic-surface guard** is the only thing in the
  suite that can see it. *"I warned about the trap" and "the suite detects the trap"
  are different claims.*

- **Two residuals, documented in four places rather than glossed.** A **multiplexer**
  drops the title-stack sequences, and OSC 2 has no query form, so the old title
  cannot be read back — accepted, since H2 names tmux as the audience. And a styled
  **`classic`** session killed by a **signal** still leaks, because classic installs
  no dying-signal handler, deliberately, pinned by a test asserting classic emits no
  viewport bytes. **Closing it would trade a guarantee for a cosmetic one.**

- **A measured "do nothing".** `SIGNAL_RESTORE`'s unconditional pop **stays**: no
  observable difference when it pops an empty stack, and making it conditional would
  break a byte-identity pin on the const and the prefix relationship with
  `SIGNAL_RESTORE_WITH_SCREEN`.

- **Unpinned and reported:** tuple bindings drop in reverse declaration order, so on
  a styled exit *from* fullscreen the session pop precedes the segment's
  leave-alt-screen. The segment's pop is suppressed, the counts hold at one and one,
  and **no test pins the interleaving.**

- **The fastest doc drift this epic.** H2-title's own text — *"a title set in
  `classic` or `inline` outlives the process"* — was live in **four** documents and
  falsified **two commits later** by the card it opened. All four corrected here,
  each carrying the residuals.

- **Acceptance, both profiles:**
  ```
  --bins 986 · --test cli 259 · --test tui 84 · --workspace --exclude kitsu 1153
  paste_latency 4 · carrier_matrix 7 · build_provenance 2
  cargo clippy --workspace --all-targets -D warnings   0 warnings
  cargo fmt -p kitsu -- --check                        clean
  ```
  Three mutations re-run by the implementer: no session push → 4 fail; key-gate only
  → the classic guard alone fails; unconditional fullscreen push → the `== 1` test
  fails. The maintainer's `fs.rs` is at `54 20`.

- **Next:** the title's **cadence** (H2's remaining deviation — stale mid-turn, and a
  signal problem rather than a title problem), the optional **T5-S5** mouse wheel,
  and the three open flakes. Gated on a human: **R6-d**, **T5-S7**, **T5-S4**,
  **F72**, **Q1**, R1's renderer flip.
