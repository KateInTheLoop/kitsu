# Product Roadmap

Status: written 2026-08-25, two days after the v0.1.0 release, from the first
week of live operation — the release pipeline's first runs, the recurring
verified smoke loop, and the dogfooding transcripts. This is the
**use-case-driven product view**; the engineering spine (epics E00–E10, cycle
ledger, replaceability gates) stays in [`program-status.md`](program-status.md)
and this document defers to it on sequencing within an epic.

The three product use cases, in the priority the user stories below reflect:

1. **Programming** — coding chat with tools against hosted or local models.
2. **System administration** — inspect/report/automate on a host, unattended
   runs included.
3. **Reverse engineering** — binary triage and code archaeology under strict
   read authority.

## What the first live week taught us

Empirical findings that reprioritize the roadmap, each with evidence:

- **Kitsu's success report cannot be trusted without effect verification.**
  Two bugs made runs report `outcome: succeeded` while writing nothing or
  writing corrupted bytes (#19 unbalanced text-form tool calls committed as
  answers; #20 control-byte content written verbatim). Both are fixed, but the
  class is the lesson: *verification belongs in the product, not just in the
  smoke harness*.
- **Small-model robustness is a product feature, not a nicety.** The entire
  first-session failure cascade (#18 tilde paths, #19, #20, quoted scalars,
  double-escaped newlines before them) came from one source: small local
  models emitting almost-correct tool calls. Every recovery that converts a
  silent failure into one instructive retry round directly widens the set of
  models kitsu works with.
- **There is a model floor, and naming it saves users pain.** llama3.2-3b
  cannot reliably push file content through JSON tool args even with perfect
  corrective feedback; qwen2.5-coder (4.7 GB, CPU) completes every smoke case.
  Kitsu currently lets users discover this by failure.
- **Platform breaks accumulate invisibly.** Five macOS/Windows/musl compile
  breaks were caught only by the release pipeline's first dry run, because CI
  builds one target. The release profile had its own invisible bug class
  (`debug_assert!` side effects) until CI ran both profiles.

## Now — 0.1.1 (patch, this week)

Ship the live-operation fixes and close the gaps that let them accumulate.

- **Merge PR #21** (tilde expansion, unbalanced text-call recovery,
  control-byte refusal) — the "first real user session" fixes.
- **Cross-platform `cargo check` in PR CI** (x86_64/aarch64 musl, macOS,
  Windows targets). Root cause of all five release-pipeline breaks; checks are
  cheap, the release dry run should never be the first compiler run for a
  target again.
- **Deflake the remaining known flakes** so release CI red always means a real
  bug: `session::transaction::…candidate_collision` (observed ~1/4 under full
  parallel release load; the inode-reuse flake is already fixed).
- **Tag v0.1.1** through the now-proven pipeline.

## Next — 0.2 (use-case driven)

### Programming

- *As a developer on a laptop, I can point kitsu at whatever local model I
  have and get either useful work or an honest early "this model is below the
  floor" —* **model capability floor in `doctor` and at chat start**: a tiny
  built-in probe (one text-form tool round) that classifies the selected
  model's tool-call reliability, instead of letting the user discover #19-class
  failures one burnt round at a time. Builds on the lean-tier/served-window
  machinery from 0.1.0.
- *As a developer, the model finds the right code without me pasting paths —*
  **mount the E07 semantic selector** (compiled and measured since Cycle 128;
  the frozen benchmark shows 5→3 rounds, 1,456→1,134 bytes on the fixed
  corpus). First user-visible mount behind a flag, off by default, per E07's
  acceptance gates.
- *As a developer with an editor, kitsu's tools reach my language servers and
  project tooling —* **E06 MCP client productization**: the sealed stdio peer,
  serial child owner, and source-admission path exist through Cycle 134;
  what's missing is the product mount (config source, registry integration,
  authority derivation). This is the single biggest capability gap vs. peer
  harnesses per [`current-comparison.md`](current-comparison.md).

### System administration

- *As an operator, I can run kitsu unattended in cron/CI and trust the exit
  status —* **effect-verified turns**: an opt-in `--verify-effects` mode where
  a turn that claims file effects re-reads and reports them in the final
  summary (the smoke loop does this externally today; the product should offer
  it). Complements the JSONL event stream and `--yolo` (landed in 0.1.0).
- *As an operator, I approve a command class once and it stays bounded —*
  continue **command containment breadth**: the Landlock/bubblewrap paths are
  Linux-proven; macOS containment posture needs an honest story (even if it is
  an explicit "unconfined, here is what that means" doctor line).
- *As an operator, I can hand a teammate a working setup in one command —*
  **E05 guided onboarding**: transactional setup, model picker, and rollback
  truth are built through Cycle 132 but unmounted. Mounting `kitsu init`'s
  interactive successor is the highest-leverage adoption item in the backlog.

### Reverse engineering

- *As an RE, I can triage an unknown binary without granting write anywhere —*
  **binary-aware read tools**: today `fs_read_file` is text-oriented and the
  smoke RE case succeeds only because ELF magic survives lossy reads. Add a
  bounded `hexdump` read tool (offset + length, capped, read-authority-gated)
  and file-type identification in `scan_file`.
- *As an RE, I can ask about a large decompiled unit —* this is the **hosted
  large-window path**: the `[gateways.qwen]` profile (262k window, full tool
  tier) covers it; the roadmap item is documenting the RE recipe and making
  window-driven tier selection visible in `/context` (done in 0.1.0) part of
  the guided setup story.

### Cross-cutting for 0.2

- **Session lineage (E03)** continues per the program — resumable, branchable
  sessions are load-bearing for all three use cases.
- **Smoke loop hardening**: add a lean-tier case (llama3.2 on the 4k window)
  so the small-window path stays exercised, and a hosted-gateway case so
  subagent fan-out is covered; today's loop tests one model on one gateway.

## Later — 0.3+

- **E09 multimodal** (screenshots into chat — sysadmin and RE both want it)
  once the two truthful capability paths exist; stays deferred per program
  rules until then.
- **X003 multi-surface** (a second host/principal) remains prepared-only until
  real demand names one.
- **E10 certification** unlocks as X002–X005 close; 1/5 today (X001 provider).

## Standing invariants (do not trade away)

The comparison doc says kitsu leads on explicit authority, OS-backed
containment, offline/auditable config, fail-stop JSONL, atomic sessions, and
failed-turn truth. Every roadmap item above is subordinate to keeping those
true — a feature that needs ambient authority or an untruthful success report
is out of scope by definition, and both cargo profiles stay green in CI
because release-only bugs are real (`debug_assert!`, #19's silent success).
