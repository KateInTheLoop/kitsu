# Chat UX dogfood — cycle 15 (claude CLI baseline; UI & functionality focus)

Date: 2026-08-12

Baseline: `3207684` (local; pushed tip `3207684` = origin) on
`dogfood/codex-ux-cycle-6`. Cycle 15 ran the rotation's Opus slot,
ship-first. Agents: an Opus day-task auditor (new genre — subsystem
exploration/summary — with kitsu on HOSTED HF Llama-3.3-70B, so remaining
gaps are kitsu's to own) and a Haiku help-discoverability critic.

## Shipped this cycle

**Skill-confusion recovery (cycle-14 gap #2).** A text-form call naming a
KNOWN SKILL as if it were a tool (`{"name": "code-review", …}`) is no longer
committed verbatim as the answer: the user sees an
`ignored a text-form call to skill '…'` receipt and the model receives a
targeted correction showing the real `skill`-tool invocation; the retry stays
bounded by max_rounds. Matching is exact against the session's skill names
(plumbed through interactive, piped, scripted, and subagent configs), so
arbitrary JSON in real answers is untouched.

**HF refresh_hint un-suppressed (cycle-14 HF audit #1).** The shipped
huggingface profile hint is reworded to survive kitsu's credential filter,
with a regression test that loads the real kitsu.toml and asserts the hint
renders. Wider finding queued: the filter's markers (`://`, `api_key`, bare
`token` labels) mean the openai/anthropic template hints still self-suppress
— a cycle-16 filter/wording redesign item.

**Resolved-by-inspection:** rootless write denials already teach
`--allow-write <dir>` (verified in kitsu-tools); the HF job's silent
failures were subagent-swallowed denials — queued as its own item.

Gates: fmt clean, clippy clean, workspace green (233 bin / 121 CLI with the
known PTY-environmental failure unchanged / 90 core / 138 tools).

## Task results (kitsu-render summary doc, identical prompt both runs)

- **Run A, `claude -p`:** 33.5 s, accurate 19-line summary.
- **Run B, kitsu + HF Llama-3.3-70B:** 19.9 s, 1-line fabricated file, zero
  reads (repeat B2: same shape). The probe run (B3) exposed the real cause —
  see headline.

## Headline finding: the fabrication was kitsu's fault

The 70B emitted clean structured tool calls every time (2-20 s/round). In
the read-forcing probe, kitsu read the 1828-line file, printed
`◇ 1828 lines` — and then the offload machinery replaced the entire result
in context with a ~500-char preview, at just 1.5 k of 131 k tokens used.
The model summarized the only 500 characters it was ever given, and nothing
in the transcript disclosed the swap. Cycles of "model fabrication" evidence
need re-reading in this light.

## Ranked new gaps (queued for cycle 16 — offload truthfulness first)

1. **Offload gating**: results >8 KiB are swapped for a 500-char preview
   regardless of remaining window; gate on actual context pressure and
   compute the `◇` summary AFTER offload so the receipt describes what the
   model actually received (`kitsu-core/src/orchestrator.rs`).
2. **Truncation visibility**: `◇ N lines` never says a result was capped or
   elided; surface `CappedText.truncated` and offload substitution in the
   receipt (`kitsu-tools/src/builtins/fs.rs` + orchestrator).
3. **Range reads past 64 KiB**: `start_line`/`end_line` apply after the byte
   cap, so the offload pointer's own "re-read a line range" advice is
   unreachable for exactly the results that trigger it (`fs.rs`).
4. **Run-summary read counter**: "wrote a doc about source it never opened"
   prints identically to a researched run; render the already-tracked
   `tool_results_elided` and a reads count.
5. Help polish (critic): scripted-mode banner, expanded subcommand syntax in
   /help, EXAMPLES section for chat --help; wrong noun for directory-listing
   receipts; G5 path elision (`◆ Read(…/src/lib.rs)` drops the crate
   segment) still bites.

Regression checks: cycle-14's context-window false alarm confirmed fixed
(clean `window:131072` every turn); /help ↔ functionality.md drift: none
(46/46 commands documented).

## Loop-infrastructure note

Agent worktrees spawn pinned at upstream `main` (cycle-5 era), not the
dogfood branch head — this cycle's auditor detected the stale tree and reset
it manually. Seed `git reset --hard <branch tip>` into the worktree flow, or
pass the expected HEAD in auditor prompts.

## Report index

1. [Render-subsystem day task, claude vs kitsu-on-HF](chat-ux-cycle-15-audits/raw/01-render-summary.md)
2. [Help discoverability](chat-ux-cycle-15-audits/raw/02-help-discoverability.md)

## Loop state

2-hour local cron loop live. Rotation: cycle 15 = Opus (this file), cycle 16
= Sonnet. Cycle 16 starts with offload truthfulness (gaps 1-2 together: gate
on window pressure + honest receipts) — it invalidates fabrication evidence
across earlier cycles and directly causes wrong answers on hosted models.
