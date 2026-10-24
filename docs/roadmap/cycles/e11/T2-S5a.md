# E11 / T2-S5a — blocking becomes real, behind an opt-in

- **UTC start/end:** 2026-10-14T09:24Z / 2026-10-14T10:45Z
- **Branch/base:** `features/tui_herdr_improvements` / `cdac378`
- **Epic/story/card:** E11 / T2-S5a — the config and wiring half of plan row
  **T2-S5**; `/jobs reply` is **T2-S5b**.
- **Requirement:** **B1**, which this card **amends**. Records **F37**.

- **What shipped.** `[agents] blocking = ask|fail_closed` (default
  `fail_closed`), `[agents] block_timeout_secs` (default 1800), and the spawn
  wiring: under `ask` a detached agent's tool work is scoped with a registry
  prompt route, so a tool of its own that needs an ungranted permission **parks**
  the agent as `blocked` instead of being refused. Four cards of machinery
  became observable in this one.

- **The shape is as safe as it could be, and that was the plan's choice rather
  than mine.** The default is `fail_closed` **everywhere** — no mode, tier or
  flag changes it — so nothing changes unless you opt in. The card's headline
  assertion is byte-identity under the default, pinned at three levels: the
  config's own default across absent/empty/explicit tables, the route level (a
  detached agent still gets the verbatim fail-closed message), and full
  transcript comparison in plain and JSONL.

- **B1 amended, because the truth is stronger than the requirement.** B1 said
  the default *stays* `fail_closed` in classic, piped, scripted and JSONL runs.
  The test author found that in scripted, piped and JSONL runs **asking is
  structurally impossible**: `interactive = stdin_is_tty() && !scripted &&
  !forced_one_shot`, and a non-interactive run resolves permissions in
  `Mode::Deny`, whose arm returns `self.denied(..)` **before** `prompt_route()`
  is ever consulted. So no value of `agents.blocking` can make automation park
  an agent. I verified both halves by reading the code. That makes B4's "JSONL
  unchanged" and N1 enforced **by structure, not by configuration discipline** —
  and it is why this milestone's end-to-end proof has to run on a
  pseudo-terminal rather than under `--script`.

- **The end-to-end test this milestone has owed since T2-S2 now exists.** Every
  earlier test covered one half, because the gate's entry point is
  `pub(crate)` in `kitsu-tools` and `kitsu` cannot call it. Making the timeout
  configurable unlocked the whole path: with `ask` and a one-second timeout, a
  pseudo-terminal run spawns an agent that hits a **real** permission gate,
  parks it, and watches the **real** timer end it as failed-interrupted —
  through the real gate, route, registry and timer, with no `/jobs reply`.
  Measured 30/30 by the implementer across both profiles and two timeout values;
  I reproduced 10/10.

- **Three traps paid for in this card, each verified rather than reasoned
  about:**
  1. **`#[serde(default = …)]` is not enough.** Every caller reads the table as
     `agents.unwrap_or_default()`, which goes through the **derived** `Default`
     and never through serde — yielding a **zero-second** block timeout on every
     config without an `[agents]` table. `AgentsConfig` needs a hand-written
     `impl Default`. The implementer reproduced the trap by swapping the derive
     back and watching `left: 0, right: 1800`.
  2. **`answer.await` makes a paused-time test vacuous**, because tokio's paused
     clock auto-advances to whatever timer is pending — so an `await` version
     passes even against the hard-coded thirty minutes. The tests use
     `try_recv` in a yield loop instead.
  3. **A seconds-wide field truncates a 250 ms test timeout to zero**, so the
     registry stores nanoseconds internally while the config key stays seconds.

- **Review findings and fixes — the notable one is that the implementer
  overrode an instruction of mine, correctly.**
  1. I pre-authorised raising the e2e timeout if `needs input` was ever missed.
     **It declined and proved the raise was the wrong fix**: the poll loop typed
     `/jobs`, which prints the agent **deck**, while `needs input` is a heading
     only `format_jobs_view` emits via **`/jobs view`** — so the assertion was
     unreachable at any timeout. At two seconds the agent sat visibly parked
     across about six polls and the test still failed. An authorised instruction
     is not evidence.
  2. **The same mistake made two negative guards vacuous.**
     `!transcript.contains("needs input")` in both default-behaviour tests
     polled a view that can never print the string, so they would have passed
     however broken the feature was. One word in the shared helper fixed all
     three tests. Recorded in **F37** with the general shape: a negative
     assertion is only as strong as the command that produced the output, and
     this is the third vacuity of that family in this project.
  3. **A byte-identity test that no implementation could satisfy.** The JSONL
     `run_started` event carries a `profile-v1-` digest of the canonicalised
     **config path**, and each run uses a fresh temp home; collapsing digit runs
     cannot hide a hex digest. Fixed by normalising the digest rather than
     sharing a path — it is environmental like `run_id`, which that test already
     normalises, and is not the behaviour under test. The plain half passed
     throughout, so the feature's claim was evidenced even while the JSONL half
     was unprovable.
  4. **One production change the card did not list**, required by a test and
     accepted: an interrupted subagent's failure now says *why*
     (`…: authority input closed before a decision`) where it previously stopped
     at the first clause. The old text remains the prefix, so nothing that
     matched before stops matching — and it earns a changelog line, because it
     is user-visible.

- **Acceptance commands/results:**
  ```
  cargo test -p kitsu --bins / --release    760 passed each  (751 + 9)
  cargo test -p kitsu --test cli           233 passed        (229 + 4)
  end-to-end test, 10 runs (mine)            0 failures
  cargo test -p kitsu --test tui             13 passed       (unchanged)
  cargo test -p kitsu-tools --lib           413 passed       (unchanged)
  cargo test -p kitsu-tui / -p kitsu-render  48 / 124        (unchanged)
  cargo clippy --workspace --all-targets      0 warnings
  ```
  The maintainer's `fs.rs` is at `54 20`, verified, with no write-mode
  formatter run in that crate (**F33**).

- **Documentation (standing requirement, in this commit):** the two config keys
  are user-visible, so all four surfaces changed and each leads with the safety
  property rather than the feature. `README.md` and `CHANGELOG.md` state that
  the default is today's behaviour and that `ask` cannot affect automation **by
  construction**. **`docs/functionality.md` had drifted and I only found it by
  checking**: it asserted flatly that "ungranted background work fails closed",
  which this card makes conditional — that sentence would have been wrong the
  moment the feature landed, and I would have missed it had I stopped at the
  README. **B1 amended** in the design with the structural guarantee. **F37**
  records both test defects. The key-name reasoning is documented too:
  `block_timeout_secs` carries its unit because a bare `block_timeout = 30` is
  exactly where seconds and minutes get confused, and the unit-less spelling is
  **rejected** rather than guessed at.

- **Commit:** see below.
- **Residual risk:** one `allow(dead_code)` survives, on
  `AgentRegistry::answer` — the honest state of the card: the route **into** the
  registry is wired, the human answer back **out** is not. That is T2-S5b, and
  until it lands a parked agent can only be released by the timeout or by
  cancellation.
- **Next card:** **T2-S5b** — `/jobs reply <id> <y|a|n>` and `/agent reply` in
  all three dispatch sites.
