# E11 / T3-S7b — the answer is read in-loop, and an unfair mutex nearly hid in it

- **UTC start/end:** 2026-10-14T18:20Z / 2026-10-14T23:20Z
- **Branch/base:** `features/tui_herdr_improvements` / `6584101`
- **Epic/story/card:** E11 / T3-S7b — the **protocol half** of T3-S7.
- **Requirement:** **R7**, which this card **amends**. Records **F55**; **closes
  F45**.

- **What shipped.** In the viewport the approval answer is read **on the
  keypress**, while the input collector still holds its raw-mode guard, so the
  kernel echoes nothing and the typed letter never reaches the transcript; the
  `> ` cue became a row of the modal. Classic's path is byte-for-byte what it
  was.

- **The protocol could only be bypassed, never deleted — confirmed by reading.**
  The collector owns raw stdin whenever stdin is a tty and stderr is styled,
  with **no condition on `[tui] mode`**, so classic's approvals genuinely need
  the claim to get canonical mode back. And there is no design where classic
  also reads in-loop without changing behaviour: its read is canonical **by
  choice**, which is what gives line editing, backspace and the long forms. So
  the claim, the lock, the four spin-waits and both top-of-loop arms all stay,
  and the bypass is gated on whether a pane is published.

- **The fix needed no echo suppression at all, which is the part worth
  remembering.** `RawStdinGuard` already clears `ICANON`, `ECHO` and `ISIG`,
  so a key read while the collector holds it is **not echoed by the kernel**.
  The stray `> y` existed only because the handoff **drops** that guard and
  restores canonical-with-echo. Stopping the handoff *is* the fix — no new
  suppression, no termios change.

- **F55: an unfair mutex starved the read, and the hazard was already written
  down.** The first implementation serialized in-loop reads on
  `APPROVAL_INPUT_LOCK` — the lock the claim path already uses, and visibly
  "the right lock". In **release only**, three approval tests (two of them
  T3-S7a's, previously passing) timed out with the modal never painted: the
  collector `try_lock`s that mutex **every 50 ms tick**, `std::sync::Mutex` is
  **unfair**, and the prompter — which deliberately publishes no claim — was
  starved indefinitely. Release went from **107.96 s with 3 failures** to
  **12.4 s with 0**.

  `ApprovalInputGuard::enter`'s own comment predicts exactly this: *"the
  type-ahead loop can observe false … and reacquire it indefinitely."* The
  implementation walked into the failure a comment in the same file exists to
  warn about, then found it, fixed it with a separate lock the collector never
  touches, and documented the measurement at the static.

  Three lessons recorded, in order of value: **a mutex touched by a polling loop
  is unavailable to anything that needs to wait** — `try_lock` in a tick is a
  repeated coin-toss a blocked waiter loses forever; **release-only concurrency
  failures are real**, since all three reproduced in release and none in debug,
  so a debug-only gate would have shipped it; and **the bounded test is what
  turned a hang into a diagnosis** — the 20-second deadline with the transcript
  attached produced three named failures instead of a suite that hung. That bound
  was worth insisting on, and this is the card the split existed to isolate.

- **Why no deadlock is possible, which is what I held acceptance for.** Across
  the publish-and-block the prompter holds **only** `APPROVAL_PANE_READ_LOCK`, a
  mutex the collector never touches; it does not hold `APPROVAL_INPUT_LOCK` and
  does not increment `APPROVAL_INPUT_ACTIVE`, so the collector never drops its
  guard and never waits on anything the prompter owns. The service arm is the
  loop's **first** arm, so a parked request is served before the handoff arm can
  give the terminal away. If the collector is gone the reader flag is false, the
  prompter's 50 ms `recv_timeout` loop breaks, and the decision is `Interrupt`;
  a sender dropped by a dying collector gives the same result. Cancellation is
  checked at **both** ends, so both unblock within ~50 ms and the pane returns to
  its status row.

- **F45 closed as a side effect, and it was a real violation.** The command
  prompter carried its **own** inline `match` on `y|yes|a|always|n|no`, with its
  own trimming and paste-marker stripping, rather than the shared parser the
  filesystem prompter and `/jobs reply` use. The test author found it while
  working out how a keypress maps onto the shared parser. It is retired, so **one
  parser now serves all three answer surfaces** and the finding that named the
  rule is finally true of the code.

- **F54 honoured: no `\x03` mapping was added.** The in-loop reader treats it
  as an ordinary character, the parser rejects it, and it takes the existing
  invalid-answer branch — the same terminal outcome as today, and **no second
  Ctrl+C path** that would have existed only because a test harness cannot
  deliver a signal.

- **Two deliberate behaviour differences, both documented in R7 rather than
  buried.** The long forms are **unreachable by keypress** in the viewport (still
  available in classic and `/jobs reply`), and a **bracketed paste** at a
  viewport approval begins with `ESC`, which the parser rejects — so a pasted
  answer is **denied** rather than accepted. Failing closed is the right
  direction for a permission prompt, but it is a difference a user could meet.

- **Acceptance commands/results:**
  ```
  cargo test -p kitsu --test tui, both profiles    32 passed each  (28 + 4)
  the ten approval tests, 10 runs per profile      20/20 green, 1.87-2.02s
  cargo test -p kitsu --bins / --release          822 passed each  (unchanged)
  cargo test -p kitsu --test cli                  245 passed       (unchanged)
  cargo test -p kitsu-tui / -render --lib          68 / 127        (unchanged)
  cargo test -p kitsu-editor / -tools --lib        39 / 413        (unchanged)
  cargo clippy --workspace --all-targets             0 warnings
  workspace total, all targets                    2150 passed, 0 failed
  ```
  Verified independently, including that **all five classic guards still pass** —
  they are this card's regression net against an over-broad bypass. The
  maintainer's `fs.rs` is at `54 20` (**F33**).

- **Documentation (standing requirement, in this commit):** **R7 amended** with
  where the answer is read, that the asymmetry is a stated contract rather than
  an implementation detail, and both behaviour differences. `CHANGELOG.md`,
  `README.md` and `docs/functionality.md` say the answer no longer appears in
  the transcript, that classic is unchanged, and that one parser serves all three
  answer surfaces. **F55** recorded, **F45 closed**.

- **Commit:** see below.
- **Residual risk:** **T3-S7c** still needs a requirement before it can be
  built. **F53** (`--plain --tui inline` reachable and unsupported), **F54**
  (the Ctrl+C question, one minute in a real terminal), **F50** (A8 untested),
  **F51** (streaming suppressed with tools enabled) and **F52**'s byte bound are
  open. **Q1's terminal matrix still needs a human.**
- **Next:** T3-S8 (resize reflow, `Ctrl+L`, and restore on panic/SIGINT/SIGTERM
  — **N5**), with five T3 stories after it.
