# E11 / F57-reach — reachability made structural, and F57's prose corrected

- **UTC start/end:** 2026-10-17T01:50Z / 2026-10-16T17:56Z
- **Branch/base:** `features/tui_herdr_improvements` / `cfb5c42`
- **Epic/story/card:** E11 / **F57-reach**, the card **F57's own closing paragraph
  asked for**. Records **F143**, which **corrects F57's second failure mode**.

- **What shipped.** Two guards that drive **every** `SLASH_COMMANDS` entry and fail
  **by name** the instant a dispatch arm goes missing. Previously a registered
  command with no arm passed every suite.

- **F57's prose is overstated, and this is the correction.** F57 says such a command
  typed mid-turn *"becomes a future prompt sent to the model."* The drain
  (`main.rs:21503-21512`) does `strip_prefix('/')` and calls `handle_slash`, so it is
  **banked and later prints `unknown command`** instead. Measured by the author,
  **verified by me**. F57 keeps its text (**F109**).

- **Mid-turn reachability is a composition**, which is why two guards and not one:
  the slash must **survive the queue verbatim** *and* the name must **have an arm**.
  Proven, not asserted — under the removed-arm mutation the mid-turn test **still
  passes**. *A failure needing two properties cannot be guarded by one test.*

- **Recognition is not separable from execution** — measured: one `async fn`, **17
  live dependencies**, arms executing inline, no recognise/run split. So the seam is
  a **process**: the real binary and real dispatcher, **one command per process in
  its own temp `HOME`**, 49 runs in ~0.86 s. Side effects real but contained, and
  `/quit` cannot hide a later command because no run has one. The same move as
  **F108**, reached independently.

- **Enumerated, not curated.** Subjects come from the registry's **own publication
  of itself** (`/help`'s JSONL command list), since the table is private to the
  binary. **No command name is typed as a subject** and **no count is asserted** —
  the registry holds **49**, not F57's **54**. Fifth wrong curated number after
  F124, F126, F131 and F142.

- **A self-proving detector.** Each run derives an unregistered sentinel and requires
  it to reach `unknown command`, so a green result **cannot** be a harness that never
  looked. Better than a mutation ritual: it cannot be forgotten.

- **No failing-first state, and that is correct.** Nothing was unreachable, so there
  was no red state and **no implementer step** — this card adds a **guard**, not a
  behaviour. The author drove all 49 entries before writing a test and **said so
  rather than manufacturing a red state** by registering a bogus command.

- **No user-visible surface changed**, so README, `functionality.md`, the CHANGELOG
  and `ux-design.md` correctly get nothing — recorded so the absence is a checked
  result, not a skipped step.

- **Acceptance, both profiles (all seven suites run in release too):**
  ```
  --bins 989 · --test cli 260 · --test tui 84 · --workspace --exclude kitsu 1153 (28)
  paste_latency 4 · carrier_matrix 7 · build_provenance 2
  cargo clippy --workspace --all-targets -D warnings   0 warnings
  cargo fmt -p kitsu -- --check                        clean
  ```
  Decisive mutation: the `detach` arm removed → *"registered but undispatched (F57):
  [\"detach\"] … All 49 registry entries were driven."* The maintainer's `fs.rs` is
  at `54 20`.

- **Next:** the optional **T5-S5** mouse wheel, the two title follow-ups (one gate
  predicate with two code sites; the turn flag named for typeahead), **F53**,
  **F67**, and the three open flakes. Gated on a human: **R6-d**, **T5-S7**,
  **T5-S4**, **F72**, **Q1**, R1's renderer flip.
