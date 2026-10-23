# E11 / F51-stream — answers stream in the default configuration

- **UTC start/end:** 2026-10-17T02:40Z / 2026-10-16T19:37Z
- **Branch/base:** `features/tui_herdr_improvements` / `1eeffd0`
- **Epic/story/card:** E11 / **F51-stream**, taking F51's **first** option. Records
  **F145**. **The last card of this stretch.**

- **What shipped.** Streaming was gated on `!tools_were_offered`, so every ordinary
  round buffered — spinner, then the whole answer at once. Text is now released as
  it arrives and withheld only while it could still open a tool call.

- **Why the first option.** *Offer tools only when plausibly needed* changes model
  behaviour rather than rendering; *accept and document it* would have written down
  that the defect hit on every turn was intended.

- **The cheap fix was measured and found unavailable.** Native tool calls arrive as
  their own delta variant and never enter the text stream, so “stream freely for
  providers that support them” looked free — but **there is no capability flag**:
  the text-protocol recovery runs for **any** provider returning no structured call.
  Checked before briefing, because it would have been the easier fix.

- **The predicate.** A continuous safe-prefix scan: `{` is always a viable opener and
  holds to end of round; `<` and `` ` `` hold only while the remainder is a prefix of
  `"<tool_call>"` or `"```"`. **Worst case 11 bytes; zero for prose.** **A latch
  cannot work** — the fenced case puts narration *before* the fence, so a latch would
  suppress the narration or leak the fence. The wrapper bytes must be held because
  `extract_json_objects` scans only for `{` and would leave them streamed.

- **An existing assertion was load-bearing on the defect.** F51 said the test's
  *name* overstated what it sampled; the truth was that one *assertion* passed only
  because nothing streamed. **Verifying it changed my ruling:** the test's primary
  claim is *"a stream in flight must not reach the pinned status row"*, and there was
  **no stream in flight** — so moving the sampling point would have bought green by
  destroying the case the test exists for. Sampling unchanged; only the accidental
  claim restated, **with a scroll anchor added so nothing was deleted**. I verified
  exactly one assertion's text changed. **F51's rename is refuted** — all three
  claims in the name are now measured for real.

- **The duplication risk was the real danger.** Once text has streamed, the
  end-of-round site must print **only the withheld tail** or the answer appears twice
  in the default configuration. Escalated from a note to a requirement; the
  **prose-then-brace** shape was not covered and now is, and the mutation catches the
  subtler **head-only** duplication.

- **Recorded, not fixed:** a `needs_structured_finalization` round can stream its
  prose head before the round is classified as a provisional handoff and its text
  suppressed from committed output — so a user may see text the transcript does not
  keep, and **no test detects it**. Confined to structured-output use, committed
  output still correct, so a display inconsistency rather than data loss. **Carded,
  not blocked.**

- **A consequence the finding never mentioned:** the cell ring is fed only through
  the surface seam, and the buffered path writes straight to stdout **without
  consulting the active surface** — so ordinary answers were absent from the
  fullscreen scrollback **by construction**, and now reach it for the first time.
  Pinned. **F52 is more pressing**, measured: same push, same line bound, so a
  documented limit rather than a new unbounded one.

- **Acceptance, both profiles:**
  ```
  --workspace --exclude kitsu 1160 (28 targets) · --test tui 84 · --bins 991
  --test cli 263 · paste_latency 4 · carrier_matrix 7 · build_provenance 2
  cargo clippy --workspace --all-targets -D warnings   0 warnings
  cargo fmt -p kitsu-core --check / -p kitsu --check    clean
  ```
  Three mutations: suppress everything → two prose tests **and** the restated tui test
  fail; stream everything → all four tool-call tests fail; end-of-round reprint → both
  `--plain` byte contracts fail. The maintainer's `fs.rs` is at `54 20`.

- **Next (all gated on a human):** the force-pushed branch divergence, **Q1**'s
  terminal matrix, **R6-d**, **T5-S4**, **T5-S7**, **F72**, R1's renderer flip. Open
  engineering: **F52**, **F53**, the structured-finalization note above, the declined
  end-to-end scrollback assertion, the two title follow-ups, three flakes.
