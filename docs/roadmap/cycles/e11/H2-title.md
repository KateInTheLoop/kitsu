# E11 / H2-title — the terminal title, specified since the start and never built

- **UTC start/end:** 2026-10-16T23:55Z / 2026-10-16T15:55Z
- **Branch/base:** `features/tui_herdr_improvements` / `7680af4`
- **Epic/story/card:** E11 / **H2-title**, opened by F139 after my previous card
  was refuted. Records **F140**. **H2 amended with two deviations.**

- **What shipped.** The terminal window title now reads `kitsu · idle · 0 agents`,
  `kitsu · working · 1 agent`, `kitsu · blocked · 3 agents` — so a multiplexer or
  herder can classify windows by title, which is what H2 asks for by name.
  **`[tui] title` finally has a consumer**, and `false` suppresses the title **and**
  the push/pop of the title stack.

- **The requirement was half-built and the key gated neither half.** `tui_bridge`
  pushed and popped the title *stack* in production **without consulting
  `tui.title`**, and the OSC 2 string was written **nowhere**, because the only
  code that would write it has no non-test caller — deliberately, since routing
  through it also enables raw mode and would staircase several hundred direct
  writers (**F93**). So the bytes are written where `tui_bridge` already writes
  them.

- **One state source.** The state word comes from
  `agent_state_file::process_state` **verbatim**; the title function takes the word
  and never re-derives it, so *blocked outranks working* stays in one home. Count
  is `running + blocked`, documented as disjoint.

- **What H2 left as alternation, decided with reasons.** `0 agents` is **never
  omitted** — H2's audience is a parser, and a title that changes shape at zero
  makes *"no agents"* and *"stale title"* indistinguishable from outside. `1 agent`
  is singular because people read it too.

- **The authorised test edit strengthened the test it touched.** A six-surface PTY
  matrix asserted **no OSC anywhere**, and two of its cases are exactly where H2
  requires a title. It now asserts that on a styled surface **every OSC introducer
  opens the state title**, and nothing at all on the four non-styled ones — which
  **pins what the output is**, where before it only pinned an absence (**F41**).
  The author proposed that shape and rejected a blanket allowance as weakening.

- **Two deviations, stated in H2 rather than hidden.** The title rides three publish
  sites, because the registry's watch channel is subscribed only behind an
  off-by-default variable — so it is **accurate at the prompt and at turn start and
  goes stale during a long turn.** And only fullscreen pushes the stack, so a title
  set in `classic`/`inline` **outlives the process on every terminal**, with the
  dying-signal restore already containing the **pop** — an inline session writes a
  pop having never pushed. **Carded as H2-stack, deliberately undecided.**

- **F140's three findings.** A **documentation failure mode I had not met**: the
  CHANGELOG's *"gives your window title back"* was **true and meaningless**, because
  nothing had ever set a title — an **idle** claim, which neither the phrase sweep
  nor the reference check can find, and which only implementing the feature
  revealed. An **absence** the positive check caught: `functionality.md` described
  the title **nowhere**, a MUST with a config key and no user prose. And a **third
  fmt debt** — six diffs at hand-off, one the author correctly declined to fix
  because it was in test code, five from the two preceding cards; verified inert
  (an import reorder, a match arm collapsed, a call joined) before accepting.

- **Acceptance, both profiles:**
  ```
  --bins 986 · --test cli 259 · --test tui 78 · --workspace --exclude kitsu 1153
  paste_latency 4 · carrier_matrix 7 · build_provenance 2
  cargo clippy --workspace, both, -D warnings   0 warnings
  cargo fmt -p kitsu -- --check                 clean
  ```
  Both mutations re-run: removing the push/pop gate fails the two `title = false`
  tests; replacing the surface gate with `true` fails **both** N1 negatives. The
  maintainer's `fs.rs` is at `54 20`.

- **Next:** **H2-stack** (needs a decision first), the optional **T5-S5** mouse
  wheel, and the three open flakes. Everything else is gated on a human: **R6-d**,
  **T5-S7**, **T5-S4**, **F72**, **Q1** and R1's renderer flip.
