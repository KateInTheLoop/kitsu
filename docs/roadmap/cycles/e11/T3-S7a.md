# E11 / T3-S7a — the approval modal in the bottom pane

- **UTC start/end:** 2026-10-14T17:30Z / 2026-10-14T22:13Z
- **Branch/base:** `features/tui_herdr_improvements` / `6c91f42`
- **Epic/story/card:** E11 / T3-S7a — the **rendering half** of the plan's
  T3-S7, which I split at review.
- **Requirement:** **R7**, which this card **clarifies**. Records **F53**.

- **What shipped.** In the inline viewport a permission prompt renders as a
  bottom-pane modal — the same option line as classic, from the same composer,
  and for command approvals the same provenance rows — and once answered the
  viewport writes the `◉ permission —` receipt into the transcript. Classic's
  bytes are unchanged, pinned by a test.

- **The split, and why it was evidence rather than caution.** The plan's row
  bundled rendering with replacing `APPROVAL_INPUT_ACTIVE` — **13 uses, four
  spin-wait loops** where the typeahead collector yields stdin. That is a
  concurrency restructure of stdin ownership and raw-mode handoff, and it is not
  the user-visible half, so bundling meant the risky part gated the valuable
  part.

  The test author then **proved** the halves independent rather than taking my
  word for it: a process-global painter already exists for the status cadence
  (because that cadence runs on the collector's thread while the guard is a
  `run_chat` local that is neither `Send` nor `Sync`); `InlineSurface::paint`
  is write-only and knows nothing about stdin; and **by the time the modal would
  paint, the approval arm has already restored canonical mode "for the approval
  reader" and called `live_input_end()`** (`main.rs:15073`, which I verified
  myself). The collector is parked, the draft row is torn down, nothing
  contends — painting there is purely additive. Three cards: **S7a** rendering,
  **S7b** the protocol, **S7c** a new requirement (below).

- **The receipt question, which I flagged for hard scrutiny, resolved better
  than either option I expected.** The test author's labelled hypothesis was
  right: **nothing emitted a receipt after a decision** — `◉ permission —`
  existed only as the pre-read prompt, with two callers, both before the read.

  I had warned that creating one would change **classic's** bytes and collide
  with a test pinning them. The implementer found the resolution instead of the
  collision: **in classic the prompt *is* the transcript line**, printed into
  the scroll region and left there, so the record already exists. The pane modal
  is **transient**, so the viewport must author a receipt to leave the trace
  classic leaves by accident of where it prints. The receipt is therefore new in
  **inline only**, gated behind "did the pane paint succeed", which can never be
  true in classic because no pane is published. Both requirements hold, and
  neither test had to move.

- **An ordering detail that cost a cycle and is now documented in R7.** The
  receipt must be written **before** the modal is cleared. A transcript line is
  text plus CRLF with **no addressing**, so it lands at the parked cursor;
  tearing down first re-parks the cursor and the receipt ends up glued to the
  status row, on a line the byte stream never terminated. Receipt first, then
  teardown, puts it after the newline the kernel echoed with the answer.

- **The refactor that made it possible without duplicating geometry.** Rather
  than re-deriving the clamp, the implementer moved the latched geometry into a
  new `PinnedPane` holding `rows`/`viewport_rows` as **atomics**, so the
  process-global handle can call the very `paint_rows`/`granted_rows` the guard
  calls. **F19**'s short-terminal bound, **F16**'s re-latch and the viewport's
  top-drop still live in exactly one place, and the guard's methods became
  one-line delegations, so four existing paint paths are behaviourally
  untouched.

- **Judgement calls I made at review:**
  1. **`--plain` stays as it is, and R7 now says so.** The plain prompter has
     its own unstyled text and `--plain` is a **byte-contract tier**; routing it
     through the shared composer would have satisfied a parity requirement by
     breaking a stability guarantee. The implementer flagged the trade instead
     of making it, which was right. **But the combination is reachable** — the
     viewport installs on `mode` alone, with no condition on styling — so
     `--plain --tui inline` pins a scroll region and paints a status row while
     every content path takes the plain branch. **F53** records that the likely
     right fix is **mode resolution** (have `--plain` resolve to `classic`), not
     a plain-mode branch in every future TUI surface.
  2. **The `> ` cue and the echoed answer still land in the scroll region**, by
     my instruction. While the protocol stays, the approval read is canonical
     **with echo**, so the typed `y` is echoed by the **kernel** wherever the
     cursor is parked — fighting that in this card would mean fighting the
     terminal discipline. T3-S7b owns echo placement, and these tests
     deliberately assert nothing about that row.

- **T3-S7c, a card the agent identified that I had not planned.** T3-S1's parity
  gap — inline never commits the submitted prompt to the transcript — looked
  like an oversight. It is not: **classic's `kitsu> …` row is not bytes kitsu
  emits.** It is rustyline's prompt plus the kernel's cooked-mode echo. Inline
  clears echo and paints the draft on the pinned row. So parity means **authoring
  a transcript line classic never authored**, with its own text and style
  decisions — a new requirement, not a bug fix. Folding it in would have made
  R7's receipt gate depend on an undecided design question.

- **Acceptance commands/results:**
  ```
  cargo test -p kitsu-render --lib, both profiles  127 passed each  (124 + 3)
  cargo test -p kitsu --test tui, both profiles     26 passed each  (23 + 3)
  the four approval tests, 10 runs per profile      20/20 green, 0.94-1.01s
  cargo test -p kitsu --bins / --release          822 passed each  (unchanged)
  cargo test -p kitsu --test cli                  245 passed       (unchanged)
  cargo test -p kitsu-tui / -editor / -tools       68 / 39 / 413    (unchanged)
  cargo clippy --workspace --all-targets             0 warnings
  workspace total, all targets                    2144 passed, 0 failed
  ```
  Verified independently. The maintainer's `fs.rs` is at `54 20` (**F33**).

  One fixture bug caught and fixed rather than papered over: the command test
  first failed because `--enable-commands` defaults the command root to the
  launch directory, which was `HOME`, so it **intersected the managed-credential
  namespace** and errored before reaching the gate. Giving the test its own
  command root made it reach the real prompt.

- **Documentation (standing requirement, in this commit):** **R7 clarified** —
  that the receipt is viewport-only and why that is not a parity break, the
  before-teardown ordering, and the `--plain` exclusion. `CHANGELOG.md`,
  `README.md`'s inline section and `docs/functionality.md` describe the modal and
  say classic and `--plain` are unchanged. **F53** recorded. The plan row was
  **split into S7a/S7b/S7c** with the rationale, its impossible
  `kitsu-tui/src/overlay/approval.rs` file corrected, and its drifted
  `main.rs:19461,19504` anchors replaced with find-by-name references.

- **Commit:** see below.
- **Residual risk:** **T3-S7b** owns the echo placement and the protocol;
  **T3-S7c** needs a requirement before it can be built; **F53**'s plain+inline
  combination is reachable and unsupported. **F50** (A8 untested), **F51**
  (streaming suppressed with tools enabled) and **F52**'s byte bound are open.
  **Q1's terminal matrix still needs a human.**
- **Next:** **T3-S7b** — replace `APPROVAL_INPUT_ACTIVE` with an in-loop modal
  and move the echoed answer out of the scroll region.
