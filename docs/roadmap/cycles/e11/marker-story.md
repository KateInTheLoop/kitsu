# E11 / the marker story — a blocked agent gets its own marker

- **UTC start/end:** 2026-10-15T00:05Z / 2026-10-15T04:26Z
- **Branch/base:** `features/tui_herdr_improvements` / `76bfc7c`
- **Epic/story/card:** E11 / an unnumbered card taken **between T4-S1 and
  T4-S2**, on T4-S1's own recommendation.
- **Requirement:** amends design **§4.4**. **Closes F34 and F3.**

- **Why out of sequence.** T4-S1 built the agent view's model and found it could
  not decide a glyph question, because a pure model carries no glyphs. It
  recommended the marker gap close **before** the overlay renders rows, or the
  overlay would ship the same defect. That was right.

- **What shipped.** `DeckState::Blocked` with `◉` styled, cream, and `[blk]`
  under `--plain`; `AgentViewState::deck_state()` as the **one** bridge from
  A2's six groups to the deck's vocabulary; and all **four** blocked mappings
  pointed at it.

- **F34: a blocked agent no longer looks like a busy one.** The glyph choices
  were argued, not picked: `◉` because it is already the approval glyph and
  §4.4 itself nominated it — and a blocked agent **is** a pending approval, so
  the vocabulary was already correct. **Cream** because it is the palette's
  "look here" slot and the only one free; reusing the accent would have left
  blocked **colour-identical to running**, curing exactly half the finding.

- **F3: the design was amended rather than obeyed, and the reason was read
  first.** §4.4 banned `◆`/`◇`/`◈` as row glyphs so a plain row could never
  render as `tool>` or `result>` — a real hazard, since the structural-marker
  mapping turns those into `*`, `>` and `#`, and `#` is what a plain section
  header prints.

  The implementer read that reason before agreeing with my lean, and found the
  ban was **the wrong instrument**: `deck_marker` checks `plain` **first** and
  returns a bracketed token, so a row glyph never reaches the mapper at all. The
  hazard is prevented **structurally**; the prohibition was redundant — and the
  shipped deck had already broken it, using `◈` for result-ready rows since D1.

  The sharing is implemented as **one function, not two tables that agree
  today**, which is the distinction this epic has five findings about.

- **The finding's own scope was wrong, and catching that is the card's main
  value.** F34 said *"the only test that changes is T2-S3's"*. The test author
  **counted and found four**: `format_agent_thread` and `format_agent_explain`
  each mapped blocked to running independently, each pinned by a test that
  strips the running marker off the output.

  **It stopped and asked rather than pre-empting me**, which was correct — and
  the answer mattered, because closing only the deck path would have **moved**
  the defect rather than fixed it: `◉` in `/jobs list` and `/jobs view`, `◆`
  still in `/jobs show` and `/jobs explain`. **Inconsistency across subcommands
  is worse than a uniform uninformative marker**, because it teaches a user to
  distrust the glyph everywhere. I extended the authorisation to all four tests;
  each pinned the **deferral**, not a behaviour — the same justification as the
  rename.

  Verified after the fact: **zero** remaining blocked-to-running mappings, four
  sites returning `Blocked`.

- **A trap flagged in advance and then hit.** Those explain tests assert the
  body `is_ascii()` **after** stripping the marker. `◉` is non-ASCII, so it
  satisfies that assertion *only because it is stripped* — changing the enum
  without the `strip_prefix` fails on an ASCII assertion that looks entirely
  unrelated. The implementer confirmed it hit exactly that and that the warning
  was what made it obvious.

- **Plain mode is a byte contract, and it held.** `[blk]` is **added**; no
  shipped marker moved, pinned by a dedicated test so a sixth state cannot
  renumber the five. The only observable change is that a blocked row emits
  `[blk]` where it emitted `[run]` — which is the defect, not a contract.

- **Acceptance commands/results:**
  ```
  cargo test -p kitsu-render --lib, both profiles  130 passed each  (127 + 3)
  cargo test -p kitsu --bins, both profiles        837 passed each  (834 + 3)
  cargo test -p kitsu --test cli                   245 passed       (unchanged)
  cargo test -p kitsu --test tui                    46 passed       (unchanged)
  cargo test -p kitsu --test paste_latency           4 passed       (unchanged)
  cargo test -p kitsu-tui / -editor / -tools        78 / 41 / 413   (unchanged)
  cargo clippy --workspace --all-targets              0 warnings
  ```
  **Counts reconciled honestly rather than assumed:** my brief predicted 131 and
  ~838; the real numbers are **130** and **837**, because the test author added
  three tests, not four. The maintainer's `fs.rs` is at `54 20` (**F33**).

- **Documentation (standing requirement, in this commit):** **§4.4 amended** —
  the ban replaced with the structural argument, why `◈` sharing is safe, and
  that `kitsu-render` now has a blocked state. **`ux-design.md`** gains the full
  marker vocabulary across all three tiers, that `--plain` is checked first, and
  that adding a state means a glyph, a colour **and** a plain token.
  `CHANGELOG.md` and `docs/functionality.md` say what a user sees. **F34 and F3
  closed**, with a note that F34's text references the old test name.

- **Commit:** see below.
- **Residual risk:** none from this card. The overlay (T4-S2) can now render
  rows without inheriting the gap. **F60**, F39's test-side fix, **F57**,
  **F53**, **F54**, **F50**, **F51**, **F52** are open. **Q1's terminal matrix
  still needs a human.**
- **Next:** **T4-S2** — the overlay itself: list, `↑/↓`, `?`, `Esc`, opened by
  `Ctrl+X a`, `/jobs view` in TUI mode, or `←` twice on an empty composer.
