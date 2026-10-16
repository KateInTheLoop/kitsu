# E11 decision records (T5-S8)

ADR-style records for the three decisions the plan named. **Written late deliberately:**
each records what was *actually* decided, including one that cannot be decided yet and one
that had been decided and never closed.

---

## ADR-1 — `inline` becomes the default renderer: **decided in principle, not yet executed**

**Status:** accepted, unexecuted. **Requirement:** R1.

**Decision.** R1 states it: *"`inline` becomes the default on a styled interactive TTY one
release after Stage 2 ships; `classic` remains available forever."* So the flip is **decided**
— the question was never *whether*, only *when*, and the answer is a release boundary, not a
judgement call.

**Why it has not happened.** The gate is evidence, not code. **Q1's live terminal matrix**
(`docs/roadmap/cycles/e11/terminal-matrix.md`) is the only thing that can say whether the
inline viewport behaves on terminals nobody here can run, and **no agent can produce it** —
it needs a human at real terminals. Flipping a default on unmeasured surfaces would make
every unmeasured terminal a regression report.

**What `classic` remains available forever buys.** The flip is reversible by the user, so the
cost of being wrong is a flag away rather than a release away. That asymmetry is why the
decision could be taken before the evidence, and why executing it still cannot be.

**Consequence.** The flip is **not** blocked on engineering. It is blocked on **Q1**, which is
one of three items this epic explicitly hands to a human.

---

## ADR-2 — a blocked background agent reports `background_blocked`, **never a new `status` value**

**Status:** accepted, shipped. **Requirement:** open question 7, now closed.

**Decision.** A background agent parked on an approval answer is reported in JSONL **as its own
event** (`background_blocked`) and **never** as a new value of an existing `status` field.

**Why.** **N1** promises JSONL output stays byte-identical for the shapes that exist today. A new
`status` value mutates an existing shape — every consumer matching on `status` sees a token it
has never seen, inside a field it already parses. A new event type leaves every existing event
byte-identical, and only a consumer asserting *"exactly these types appear"* is affected. **This
is the same reasoning F120 later applied to `tool_results_dropped`**, independently: additive
event, never a mutated field.

**Closed late, and that is the finding.** The design **stated this decision in one place and
still labelled it "(open question 7)" in another** — two sentences disagreeing about whether a
decision existed. Writing this record is what forced the two together (F138). **A decision
recorded in prose beside the thing it governs, but never marked closed, is indistinguishable
from an open one** to every later reader.

---

## ADR-3 — no local socket API: **deferred, out of scope**

**Status:** deferred. **Requirement:** open question 6.

**Decision.** Kitsu ships **no** local socket API in E11. It sits in the design's own non-goals
beside multi-machine and remote attach.

**Why deferred rather than rejected.** The herdr integration (H1–H5) is served by the state file
and `agent_wait`, both of which shipped. A socket would add a second control surface with its own
lifetime, permissions and failure modes, for capability the file already provides — and **H4
remains blocked on herdr's own manifest spec**, so the integration's requirements are not yet
fully known. Designing a socket against an unknown consumer is how a second surface outlives its
reason.

**What would reopen it.** A concrete herdr requirement the state file cannot meet. Until one
exists, the state file is the contract.
