# Chat UX dogfood — cycle 10 audit checkpoint (paused by spend limit)

Date: 2026-08-11

Baseline: `c54e094` on `dogfood/codex-ux-cycle-6`. Cycle 10 ran on Fable per
the model rotation: one day-in-the-life auditor (realistic 8-file OSS fixture
taken through orientation → implementation → wrap-up on both Codex CLI and
kitsu, 2 codex runs) and two fix passes. The account's monthly spend limit
ended the cycle during the second fix pass; this checkpoint records the state.

## Shipped this cycle

**Pass E (`8bb455a`) — the `workspace_diff` tool.** The cycle-9 headline gap
closed: the model can fetch bounded host-verified diff evidence in any chat
mode. Disclosure-not-read authority (no model read roots consulted, created,
or widened; kitsu-tools untouched; /diff's caps and private-path exclusions;
root never caller-nameable), the standing disclosure line, an additive JSONL
event, /permissions consumer disclosure, and an opt-out
(--no-workspace-evidence / workspace_evidence = false). Design contract in
docs/workspace-diff-tool.md. Stretch: /queue subaction suggestions and
thicker cycle-9 tests.

**Pass F (`e7a45e0`, partial) — session truth.** From the day-in-the-life
audit: rolled-back turns that retained file writes now tell the next turn
what changed on disk (G1); contained command failures carry
containment-remedy hints (G2); join heartbeats back off exponentially with
agent-state counts (G4). The maintainer completed the interrupted pass:
boxed the grown ToolFinished payload to clear the large-enum-variant lint,
formatted, and verified all gates green (228 bin / 121 CLI / 85 core / 58
provider / 137 tools / 31 render / 2 provenance passing; the known
PTY-environmental failure unchanged).

## Day-in-the-life audit highlights

Codex won both timed tasks decisively on this fixture (orientation 87s vs
502s; implementation 66s vs 548s) — dominated by the local model's agent
storms and hallucinations, kept in a separate fault ledger. Kitsu's
confinement held throughout (including an agent attempting commit+push), and
its failure accounting was rated exemplary: honest exit codes, verified
snapshot diffs that made a test-file corruption visible in-transcript, grant
disclosure, zero-prompt object-form pregrants. The ◆/◇/◈ vocabulary was
learnable in one session; ⛩ never appeared during real work; heartbeat spam
(now fixed, G4) was the identity's worst enemy.

## Queued for the next cycle (pass F remainder + audit backlog)

1. G3: per-turn spawn cap (config `agent_spawn_cap`; scripted default low) —
   cycle-9's prompt guidance demonstrably does not restrain small models.
2. G5: command receipts must never elide the artifact path mid-filename.
3. G6: `doctor --online` should report cached runtime context-window
   evidence and config/served mismatches.
4. G2b follow-up: evaluate a read-only pip user-site mount inside the
   sandbox when the interpreter is approved.
5. Standing backlog: syntect code boxes on the streaming path, agent-deck
   front door, live-turn /jobs output, deck identity round 2, screen-reader
   mode, stdout deliverable in human one-shot mode, ⛩ presence in real work.

## Report index

1. [Day in the life](chat-ux-cycle-10-audits/raw/01-day-in-life.md)

## Loop state at pause

The hourly loop and its cron job were stopped when the spend limit was hit
(second occurrence; same protocol as cycle 7). Model rotation stands at:
cycle 10 = Fable (audits done, pass E + partial pass F shipped), cycle 11 =
Opus. To resume after raising the limit: re-run the /loop prompt; the next
cycle starts with the pass F remainder above.
