# Chat UX dogfood — cycle 13 (claude CLI baseline; UI & functionality focus)

Date: 2026-08-12

Baseline: `2712147` on `dogfood/codex-ux-cycle-6`. Cycle 13 ran the rotation's
Haiku slot with fixes shipped BEFORE the audit so the auditors exercised them
live. Agents: a Haiku day-task auditor (error-message improvement task +
`cargo check`, both CLIs) and a Haiku session-lifecycle critic (kitsu
`sessions`/`--resume` vs claude `--resume`/`-c`).

## Shipped this cycle (both verified live by the auditors)

**Toolchain access in command containment (cycle-12 gap #1).** New repeatable
`--command-runtime-root <dir>` chat flag mounts toolchain directories
read-only inside bubblewrap containment, with a startup disclosure line per
root ("contents readable by approved commands"). Each root's `bin/` joins the
fixed containment PATH; `.cargo`/`.rustup` roots also set
CARGO_HOME/RUSTUP_HOME so rustup shims resolve without the (deliberately
absent) real $HOME. Implies --enable-commands. Verified: `cargo --version`
runs contained in 67 ms where cycle 12 burned a full 600 s deadline; the
auditor confirmed disclosures and receipts, no regressions.

**Escape-tolerant text-protocol recovery (cycle-12 gap #2 + cycle-11 #4).**
`relax_json_escapes` repairs the common small-model fault (`\'`) inside
otherwise well-shaped tool-call JSON so the call executes; an attempt too
broken to repair is no longer committed as the answer — the user sees an
`ignored N malformed text-form tool call(s)` receipt, the model receives
exact corrective feedback, the retry stays bounded by max_rounds, and the
attempt is accounted as a tool error. (Deliberately NOT charged against the
spawn cap: the cap's error message claims "already started N", and counting
never-started attempts would make it lie; visibility was the actual ask.)

Gates: fmt clean, clippy clean, workspace green (230 bin / 121 CLI with the
known PTY-environmental failure unchanged / 89 core / 138 tools).

## Task results (error-message task, identical prompt both runs)

- **Run A, `claude -p`:** 81.0 s, edit made and `cargo check` genuinely passed.
- **Run B, kitsu + qwen:** 87.8 s, failed — but for the first time the
  failure was purely a model-capability ceiling (qwen invoked the code-review
  skill instead of editing), not a kitsu defect. Kitsu's new receipts
  truthfully reported the no-op.

## Ranked new gaps (queued for cycle 14)

Session lifecycle (critic — the strongest new theme):
1. **No interactive session picker** — `--resume` demands an exact 32-char
   id; claude's bare `--resume` opens a picker. Add no-arg picker over the
   session list.
2. **Session ids are not human-memorable** and there is no naming — add an
   optional name (claude `--name` equivalent), show it in listings, accept it
   in `--resume`, plus a `/name` rename command.
3. **No cwd-scoped continue** — claude `-c` resumes the latest session in
   this directory; kitsu's `--resume last` is global.
4. **Fork surface is split** (flag + subcommand) — pick one canonical path.

Day-task auditor:
5. **Skill disclosure misleads small models** in piped automation (qwen
   grabbed the code-review skill instead of editing) — consider gating the
   skill index by flag in scripted mode.
6. **Deadline-burn visibility** (carried from cycle 12): heartbeat should
   show `[45s / 600s]` style budget against --timeout-secs.
7. Run-summary could distinguish "task incomplete" from "nothing to do"
   (auditor #5; needs care — kitsu cannot judge task semantics, only report
   state changes).

## Standing backlog (unchanged)

Cycle-12 #4 usage totals in final_summary, cycle-11 #2 piped-session hint /
#3 subagent narration on stdout / #6 help fatigue, Haiku cycle-11
branding/examples/init items, G5 receipt elision, G6 doctor --online.

## Report index

1. [Error-message day task](chat-ux-cycle-13-audits/raw/01-error-msg-task.md)
2. [Session lifecycle vs claude](chat-ux-cycle-13-audits/raw/02-session-ux.md)

## Loop state

2-hour local cron loop live. Rotation: cycle 13 = Haiku (this file), cycle 14
= Fable. Cycle 14 starts with the session-picker + session-naming pair — the
critic ranked them the two highest-impact parity gaps, and they compound
(a picker is only as good as the names it shows).
