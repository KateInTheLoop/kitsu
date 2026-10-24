# E11 / F39 — the byte-identity flake, scheduled rather than deferred a third time

- **UTC start/end:** 2026-10-15T05:45Z / 2026-10-15T09:34Z
- **Branch/base:** `features/tui_herdr_improvements` / `8fafb03`
- **Epic/story/card:** E11 / **F39's test-side fix**, taken ahead of T4-S5.
- **Requirement:** none — a test defect. Resolves **F39**.

- **Why it jumped the queue.** I had twice written that this should be
  "scheduled rather than deferred". **The cost was no longer the minute it
  wasted but the review discipline it eroded:** eight failures had made a red
  gate normal, and a reviewer who sees a failing suite every card learns to wave
  failing suites through — the exact habit **F43** and **F64** are about. Writing
  it a third time would have been the deferral I had just criticised in writing.

- **The diagnosis was inherited, so it was verified, not trusted.** In the code,
  `join_background_agents_at_exit` takes **one snapshot** of
  `registry.running()` and hangs both narrations off it; in a captured failure
  the transcript diff is *exactly* those two lines, with N1's
  `background_result` identical on both sides. Both narrations are correct for
  their timing — a **synchronisation** defect, not a product one.

- **It was failing 11 times in 20, not eight times in the epic.** Run alone in
  release the reproduction rate is **55%**; the suite had been hiding it behind
  serial ordering and load. **A flake's observed rate is a property of how you
  run it, not of the flake** — worth remembering before calling anything rare.

- **What was impossible is the interesting part.** The obvious fix — script
  `/quit wait` so the agent drains — was tried and **measured failing 7 of 20**:
  `await_background_agents` has the *same* early exit on the *same* snapshot, so
  the race just moves to its own narration line. **Every wait primitive the
  product exposes narrates conditionally on that one snapshot**, and
  `agent_result` does not block, so **no script-only sequence can make entry
  into a wait deterministic.**

- **The ruling I was asked for, and why I accepted a time margin after
  forbidding sleep-based fixes.** The two roles are separate, and only one is
  time:
  - **The synchronisation is condition-based and is pre-existing product code** —
    the join already does `registry.subscribe()` and
    `while changes.borrow().running > 0 { … changes.changed() … }`. I verified
    this by reading, because it is what makes the fix legitimate.
  - **The 1 s mock delay only decides which of two valid orderings occurs**, so
    the path that waits on the condition is always entered. It is not
    `sleep(); assert`.

  Safe rather than merely convenient for three reasons: the margin is ~200×
  (1 s against an end-of-input arriving in single-digit ms); it sits well under
  the 15 s first join heartbeat, which would otherwise inject a variable
  `still waiting` line; and the adjacent test already uses the same device for
  the same purpose (`set_delay(30s)`).

  **But the decisive mitigation is an added assertion, not the delay.** The fix
  *adds* a guard that both scripted runs must contain both join lines, so if the
  ordering ever inverts the test **says so** rather than silently comparing
  transcripts that no longer cover the join path. It would have failed about half
  the runs before the fix — independent confirmation of the diagnosis. **A time
  margin policed by an assertion is a different thing from one that is trusted.**

  The alternative — piped stderr, a watcher thread, a gated mock response and a
  runtime-flavour change — is a rewrite of the test's I/O shape whose own
  machinery would introduce new failure modes. Available as its own card if the
  guard ever fires.

- **Nothing was weakened.** Both compared assertions are **byte-for-byte
  unchanged** (verified: the diff contains no `assert_eq!(transcripts…` line on
  either side), the `clean()` normaliser is untouched, and the two join lines
  are still compared **as product output** — normalising them out would have
  deleted coverage of exactly the lines that differ.

- **Acceptance commands/results:**
  ```
  release --test cli, 3 full suites       245 passed each
  the test alone, release                  40 runs, 0 failures (was 9/20 pass)
  the test alone, debug                    10 runs, 0 failures
  my own independent check                  6 release runs green; full release cli 245
  cargo test -p kitsu --bins, both        857 passed
  cargo test -p kitsu --test tui, both     60 passed
  kitsu-tui 86 · kitsu-editor 42 · kitsu-render 130 · kitsu-tools 413 · paste_latency 4
  cargo clippy --workspace --all-targets    0 warnings
  ```
  Every run went to its **own numbered file** — the half of F39's own rule that
  the eighth occurrence added, after an agent's evidence was destroyed by the next
  suite overwriting it. Suite cost: +4 s (release `--test cli` 47 s → 51 s). The
  maintainer's `fs.rs` is at `54 20` (**F33**).

- **Documentation (standing requirement, in this commit):** **F39 marked
  resolved**, with the 55% standalone rate, the measured failure of the obvious
  fix, why no script-only fix exists, and the margin-versus-synchronisation
  distinction. The plan row is honest. No user-facing behaviour changed, so
  `README`, `CHANGELOG` and `functionality.md` correctly say nothing.

- **Commit:** see below.
- **Residual risk:** the margin is policed but is still a margin; if the guard
  fires, option (a) is specified and ready. The `r`-decision-read paste arm
  remains **unasserted** (F69 follow-up). **F67's open question** is unmade.
  **F62**, **F60**, **F57**, **F53**, **F54**, **F50**, **F51**, **F52** are
  open. **Q1's terminal matrix still needs a human.**
- **Next:** **T4-S5** — the filter line (`/`), the last key A3 lists with no
  binding, now adding its arm to one dispatch rather than two.
