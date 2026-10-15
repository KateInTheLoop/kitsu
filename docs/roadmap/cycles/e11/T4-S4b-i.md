# E11 / T4-S4b-i — the action keys, rejected once for turning a paste into a privileged action

- **UTC start/end:** 2026-10-15T03:05Z / 2026-10-15T07:48Z
- **Branch/base:** `features/tui_herdr_improvements` / `d1ec441`
- **Epic/story/card:** E11 / T4-S4b-i (split from T4-S4b). **Rejected once; this
  is the accepted rework.**
- **Requirement:** **A3**, amended again. Records **F67**, **F68**.

- **What shipped.** `r` replies to a blocked agent, `x` stops it (twice; the
  second press prunes an already-terminal one), `n` renames it, `p` marks its
  result collected — each naming its outcome, its refusals, and the
  no-selection case. Plus the paste-aware decoder the rejection demanded.

- **I rejected the first attempt, and the defect was then reproduced (F68).**
  The implementation passed all six assigned tests. Reviewing what it *enabled*
  rather than what it was asked to do: the overlay reads raw bytes and executes
  **every key in a chunk**, and it never asked the terminal to bracket pastes.
  A test agent then confirmed it against the real code — one paste of
  `"retry the exxtract\nand fix the bug"` produced:

  ```
  cannot read e as an answer for background agent #1; press y, a or n
  answered background agent #1: allow_once
  press x again to stop background agent #1
  stopped background agent #1
  ```

  **One pasted line of prose approved a parked filesystem write and then stopped
  the agent.** 34 keys from one chunk. Before this card the overlay bound only
  arrows, `Space`, `?` and `Esc`, where a stray paste is harmless; the card
  bound keys with **effects**, so the severity class was introduced here.

  **F68 is F65's root with the opposite sign.** F65: *an arrow key is not one
  byte.* F68: *a chunk of bytes is not a sequence of intentions.* A loop that
  treats every decoded key as a deliberate keystroke is safe only while no key
  does anything.

- **The fix keeps one parser (F45).** `OverlayDecoder::feed` walks the chunk a
  byte at a time and hands each byte to the **existing** `decode_overlay_keys`,
  which remains the sole authority on the `ESC`/CSI/SS3 transitions. The
  decoder adds only what keys cannot carry: the paste block and a possibly-split
  delimiter, held in a struct rather than a `Copy` enum — `tui_bridge`'s
  `KeyFilter`, which holds `held` bytes, is the precedent.

  **The trap that cost a design iteration:** `ESC` alone is a **strict prefix**
  of `ESC[200~`, so naive prefix-holding silently kills F65's lone-`ESC` grace.
  The delimiters are recognised **inside the existing CSI state** instead, and
  every F65 control still passes.

  **Detection is two-sided** because one side is unavailable on some terminals:
  bracketed (`ESC[?2004h`, which the overlay emits itself) **and** an
  unbracketed-burst rule — two or more printable keys in one chunk is not
  typing. The second is what protects `r`+`y` on a terminal with no paste
  reporting.

- **Two judgements the implementer made well, neither of which I specified.**

  **The swallowed-burst report names the count and never the text.** The burst
  rule's false positive is a fast typist's two `x` presses, which is exactly the
  outcome a user cannot otherwise explain, so silence would read as a broken
  key (T2-S5b). But the payload is the user's **clipboard** and may hold a
  secret, so echoing it would be a new disclosure — the same reasoning as
  **F40**, where the timeline's protection had to become structural.

  **The overlay enables bracketed paste and deliberately does not disable it on
  exit.** `RawStdinGuard` owns that pair, and at the inline idle reader the
  overlay runs *inside* the caller's still-live guard — so writing `?2004l` on
  close would strip bracketed paste from the composer underneath **for the rest
  of the session**. Verified: the guard's `engage`/`Drop` pair is at
  `main.rs:16091`/`:16106`, and the overlay's enable rides on its own
  `?1049h…?25l` write.

- **F67's refusal, which I traced myself.** `prune` does `agents.remove(&id)`
  and the guard that surfaces unviewed results can only iterate records that
  exist — so pruning **discards the result and the guard cannot know**. `x`
  therefore refuses a terminal agent whose handoff is still `Ready`, says the
  result is waiting, and prunes on the identical press once collected.
  **Deliberately asymmetric with `/jobs prune <id>`**, which names an id
  explicitly and whose JSON path returns the payload: *the same registry call is
  safe behind a typed id and unsafe behind a keystroke.*

- **Custody, the gate for the fourth card running.** `r`/`x`/`n` take no
  custody of another agent's result, with `p` as the positive control. One
  exception the tests **forced** and the code documents: on the `Cancelled`
  branch `x` acknowledges **the handoff of the agent it just stopped**, because
  `finish(Cancelled, …)` sets `handoff = Ready` — so without it, stopping an
  agent would manufacture an unviewed result for an agent the user deliberately
  killed. It is custody of that row and no other, on that branch only, which is
  what keeps the `AlreadyTerminal` refusal working.

- **Acceptance commands/results:**
  ```
  cargo test -p kitsu --bins, both profiles   856 passed, 1 failed (the designated s test)
  cargo test -p kitsu --test tui, both         60 passed, 0 failed
  cargo test -p kitsu-tui, both                84 passed, 2 failed (s)
  cargo test -p kitsu-editor                   41 passed, 1 failed (s)
  cargo test -p kitsu --test cli              245 passed  (the F39 flake did not recur)
  cargo test -p kitsu --test paste_latency       4 passed
  cargo test -p kitsu-render --lib            130 passed
  cargo test -p kitsu-tools --lib             413 passed
  cargo clippy --workspace --all-targets        0 warnings  (back from 3)
  ```
  The five remaining failures are **exactly** the T4-S4b-ii `s` sub-mode tests.
  I authorised two lint fixes in test bodies (a redundant `.clone()` on a `Copy`
  type, an `assert_eq!(x, true)` becoming `assert!(x)`); **no assertion
  changed**. The maintainer's `fs.rs` is at `54 20` (**F33**).

- **Documentation (standing requirement, in this commit):** **A3** carries the
  `x` reading and its verified premise, F67's refusal and the asymmetry with
  `/jobs prune`, the no-selection rule, **F68's paste clause as a safety
  clause** with the reproduction in it, the count-not-text decision, and the
  `?2004l` asymmetry. **F67** and **F68** recorded, F68 twice — once as the
  rejection, once with the empirical confirmation. `README.md`'s `/jobs view`
  row, `CHANGELOG.md` and `docs/functionality.md` describe the keys and the
  paste rule.

- **Commit:** see below.
- **Residual risk:** **T4-S4b-ii is specified and failing** — four tests for
  `s`'s sub-mode over `kitsu-editor`, with `SteerLine`/`steer_prompt_rows`/
  `escape_pending` stubbed and one `allow(dead_code)` noting it. The burst
  heuristic's false positive is real and now visible rather than silent.
  **F67's open question** — whether `/jobs prune` should print a result before
  discarding it — is a product decision, unmade. **F62**, **F60**, F39's
  test-side fix (**seven** occurrences), **F57**, **F53**, **F54**, **F50**,
  **F51**, **F52** are open. **Q1's terminal matrix still needs a human.**
- **Next:** **T4-S4b-ii** — drive `SteerLine` from `kitsu-editor`, whose own
  tests already hold that a paste never submits; the hard part is a steerable
  agent fixture, which today exists only inside `subagent`'s test module.
