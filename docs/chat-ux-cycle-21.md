# Chat UX dogfood — cycle 21 (claude CLI baseline; UI & functionality focus)

Date: 2026-08-12

Baseline: `4ca7317` on `dogfood/codex-ux-cycle-6` (post-merge of PR #1; the
branch also carries the maintainer-directed feature waves: /cost, /loop,
/help <command>, /theme). Cycle 21 ran the rotation's Haiku slot.

## Shipped this cycle

**Failure-path run receipts (cycle-20 gap #1, twice promoted).** The piped
one-shot's strict-failure and unfinished-background exits now emit the full
run summary (reads, denied/failed tallies, session) BEFORE returning the
error — a failing run can no longer end with zero accounting. Verified by
the auditor in both script and piped modes: receipts truthful at all
failure boundaries.

Gates: fmt clean, clippy clean, workspace green (233 bin / 121 CLI with the
known PTY-environmental failure unchanged / 92 core / 139 tools).

## Feature audit (the maintainer-directed waves, first audit pass)

All four verified working with graceful errors: `/help <command>`
(per-command details + examples), `/theme` (six live palettes; unknown
names list the valid set), `/cost` (usage + live window position), `/loop`
(queueing works; the scripted-mode refusal is clear about why and what to
do instead).

## Help-discoverability head-to-head

Kitsu's surfaces are accurate and Session commands already lead the /help
group order — but small models gravitate to the TOOL vocabulary
(workspace_diff, spawn_agent) over session commands when asked to
summarize capabilities. This is model reading behavior, not registry
order; noted for audit-rubric design rather than a code change.

## Ranked gaps (queued for cycle 22)

1. Interactive `/resume` picker (long-standing parity item).
2. In-session `/doctor`.
3. Redundant single-file delegation heuristic.
4. Denial-final disposition redesign (dedicated cycle; touches automation
   contracts — a denied turn still discards a truthful final answer).
5. Carried: capped-read true size, worktree gitdir binds, python_tag
   parsing, G5 elision, credential-filter redesign, /memory equivalent,
   timer-based /loop.

## Report index

1. [Failure receipts + feature walkthrough + discoverability](chat-ux-cycle-21-audits/raw/01-features.md)

## Loop state

2-hour local cron loop live. Rotation: cycle 21 = Haiku (this file), cycle
22 = Fable. Cycle 22 starts with the interactive /resume picker — the
oldest open parity item, now unblocked by session naming.
