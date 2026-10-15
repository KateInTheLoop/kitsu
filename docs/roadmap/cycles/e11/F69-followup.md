# E11 / F69 follow-up — retiring a mirror that was never looked into

- **UTC start/end:** 2026-10-15T05:00Z / 2026-10-15T08:58Z
- **Branch/base:** `features/tui_herdr_improvements` / `d62af22`
- **Epic/story/card:** E11 / **F69 follow-up**, taken before T4-S5 because that
  card adds another prompt arm to the loop the model was supposed to mirror.
- **Requirement:** none — this is a **behaviour-preserving refactor**. Sharpens
  **F69**; records **F39**'s eighth occurrence.

- **This card inverted the project's usual split, deliberately.** Its deliverable
  *is* test infrastructure, so there was no "write a failing test, then
  implement": a refactor whose contract is *no behaviour change* cannot be driven
  by a new failing test, because the existing 857 **are** the specification. The
  standing rule that implementers never touch tests was lifted and **bounded**
  instead: arms could be moved, re-targeted or deleted and tests re-pointed, but
  **no assertion weakened, strengthened or reworded**, with before/after quoted.

- **The gate that replaced TDD: break the real code on purpose.** Two one-line
  breaks in the extracted dispatch — truncating a paste to its first line, and
  dropping `n` from the action keys — **both produced failures that would have
  been invisible before**, because the model carried its own copies of both arms.
  Both reverted and re-verified.

  **A refactor that claims to remove duplication should be made to prove it, and
  breaking the surviving copy is the cheapest proof there is.** This is worth
  reusing.

- **What shipped.** `handle_overlay_input(registry, state, view_rows, cols,
  input) -> OverlayStep` is now the single dispatch; `OverlayState` holds the
  loop's six locals unchanged, and `OverlayStep` names the three things the loop
  did with control flow. `run_agent_view_overlay` keeps only what needs a
  terminal: the alternate screen, the terminal size, painting, `stdin.read`,
  **both** 50 ms graces, and the steer line's raw-byte read with its decoder
  bypass.

  Faithfulness was not asserted but **shown**: the new body was token-compared
  against `git show HEAD:…main.rs` of the old one, identical apart from the
  deleted arm. Neither hazard I flagged blocked it — the async stop path pulls
  out as-is because `overlay_block_on` already handles both runtime cases, and
  the steer line's raw read stays in the loop where the bytes are.

- **The finding got better, and it was the agent that improved it.** I wrote that
  the model's *paste-into-the-steer-draft test* described the model rather than
  the product. **There is no such test** — the paste test drives `n`, and **no
  test has ever fed `s`.** So `OverlayTextPrompt::Steer` was kept alive not by
  an assertion but by **one unexercised arm of a model that looked authoritative
  and was never executed.**

  **And there were three divergences, not one.** The model also ignored
  `Backspace` in a text prompt (the real loop pops the draft) and kept `r`'s
  decision read open on `Esc` (the real loop closes it and clears the report).
  None was asserted. The count is the lesson: **duplicated behaviour does not
  drift once and get noticed — it drifts repeatedly and silently**, because
  nothing in the design is watching.

- **A gap the experiment exposed and did not paper over.** A third attempted
  break — making a paste close `r`'s decision read — failed **nothing**. That
  arm is genuinely unasserted, because the paste test pastes while the frame is
  the row list and never after an `r`. **Pre-existing, not a regression**, and
  closing it means *adding* an assertion, which this card's own contract forbade.
  Recorded as a candidate rather than smuggled in.

- **Acceptance commands/results:**
  ```
  cargo test -p kitsu --bins, both profiles   857 passed, 0 failed   (identical)
  cargo test -p kitsu-tui, both                86 passed             (identical)
  cargo test -p kitsu-editor, both             42 passed             (identical)
  cargo test -p kitsu-render --lib            130 passed             (identical)
  cargo test -p kitsu-tools --lib             413 passed             (identical)
  cargo test -p kitsu --test tui, both         60 passed             (identical)
  cargo test -p kitsu --test cli (debug)      245 passed             (identical)
  cargo test -p kitsu --test paste_latency      4 passed             (identical)
  cargo clippy --workspace --all-targets        0 warnings
  ```
  **Every count identical, which is the whole point.** `cargo test --release -p
  kitsu --test cli` is **244 + 1**, the **F39** flake (eighth occurrence,
  confirmed by name on my own run); debug is 245/245 twice and the test is a
  scripted/JSONL byte-identity check with no connection to the overlay. Verified
  no assertion changed: the only diff line matching `assert` is a **comment**
  containing the word "asserted". The maintainer's `fs.rs` is at `54 20`
  (**F33**).

- **Documentation (standing requirement, in this commit):** **F69 sharpened**
  with the agent's correction (the arm was never run; there were three
  divergences) and with the break-it method written down as reusable. **F39's
  eighth occurrence** recorded, with the observation that it now costs a gate on
  most cards and **trains the reviewer to wave a red suite through** — and with
  the rule's missing half: *one output file per suite*, since the agent's
  evidence was destroyed by the next suite overwriting it. The plan row is
  honest; no user-facing behaviour changed, so `README`, `CHANGELOG` and
  `functionality.md` correctly say nothing.

- **Commit:** see below.
- **Residual risk:** the `r`-decision-read paste arm is **unasserted** (noted
  above). **F39's test-side fix should now be scheduled rather than deferred.**
  **F67's open question** (whether `/jobs prune` should print a result before
  discarding it) is unmade. **F62**, **F60**, **F57**, **F53**, **F54**,
  **F50**, **F51**, **F52** are open. **Q1's terminal matrix still needs a
  human.**
- **Next:** **T4-S5** — the filter line (`/`), the last key A3 lists with no
  binding, whose grammar T4-S1 already settled and pinned. It now adds its arm
  to **one** dispatch rather than two.
