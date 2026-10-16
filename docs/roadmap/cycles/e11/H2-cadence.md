# E11 / H2-cadence — the title follows the registry, and my premise was false

- **UTC start/end:** 2026-10-17T01:10Z / 2026-10-16T17:14Z
- **Branch/base:** `features/tui_herdr_improvements` / `a2f4099`
- **Epic/story/card:** E11 / **H2-cadence**. Records **F142**, which **retracts
  F140's cadence paragraph**. **H2 now has no remaining deviations.**

- **What shipped.** The state title is published on every agent-registry change, so
  it keeps up **during** a turn as agents block and finish — once per change,
  **beside** the status-row repaint rather than inside it.

- **The card rested on correcting my own shipped documentation.** F140 and H2's
  amendment both said the watch channel is subscribed only behind an off-by-default
  variable, *"so there is no general change hook in a chat session."* **False.** An
  ungated, session-long subscriber exists, its rustdoc calling itself *deliberately*
  ungated **in explicit contrast to the opt-in notifier my claim generalised from**.
  It already deduped on `(blocked, running)` and has **no turn gate**.

- **The mechanism was a curated list of size one.** I measured one subscription site
  and generalised to *"no hook exists"*; enumerating found four. **A negative claim
  derived from a single sample is a curated list of size one.**

- **Three decisions, each measured.** The **turn flag must be plumbed**, and exactly
  one case forces it: blocking and unblocking render correctly even with a wrong
  flag, because blocked outranks everything, while an agent **finishing** into zero
  agents renders `idle` where the truth is `working`. The title rides **beside** the
  repaint, because the repaint is deliberately inert in classic and folding the
  title in would leave a **styled classic** session as stale as before. And **one**
  write per change, because the twin repaints exist for a coalescing window the
  title does not have.

- **F141 closed by construction, and it bought more than it cost.** The new publish
  point carries **no gate of its own** — the trap mutation there is provably
  **undetectable in-process**, since catching it needs a registry change in a
  non-tty session and the test gateway is deliberately an invalid host. Of the
  implementer's **eight** mutations, the two that escaped are at the sites outside
  the funnel; the same two **are caught** at the shared derivation and the single
  gate evaluation. **Funnelling moved a mutation class inside the suite's reach.**

- **Two follow-ups recorded, not taken.** One gate expression now has **two** code
  sites (the title write and H2-stack's stack-push guard) — legitimate, since they
  write different bytes, but a latent F141 shape worth sharing a predicate. And the
  reused turn flag is **named for typeahead**, so its name understates what it now
  carries.

- **Acceptance, both profiles:**
  ```
  --bins 988 · --test cli 259 · --test tui 84 · --workspace --exclude kitsu 1153
  paste_latency 4 · carrier_matrix 7 · build_provenance 2
  cargo clippy --workspace --all-targets -D warnings   0 warnings
  cargo fmt -p kitsu -- --check                        clean
  ```
  Eight mutations run, plus a bare-wake control proving the dedupe path is taken
  rather than assumed. The maintainer's `fs.rs` is at `54 20`.

- **Next:** the optional **T5-S5** mouse wheel, the two gate/name follow-ups above,
  and the three open flakes. Gated on a human: **R6-d**, **T5-S7**, **T5-S4**,
  **F72**, **Q1**, R1's renderer flip.
