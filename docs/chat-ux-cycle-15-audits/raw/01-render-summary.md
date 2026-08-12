# Cycle 15 · audit 01 — subsystem exploration/summary (claude CLI vs kitsu + hosted HF Llama-3.3-70B)

Date: 2026-08-12 · Auditor slot: Opus · Focus: UI + functionality
Worktree: `/home/mmpg/workspace/kitsu/kitsu2/kitsu/.claude/worktrees/agent-a4eba0a597c72dc54`
Baseline synced to `3207684` (dogfood/codex-ux-cycle-6 head) — **the worktree arrived stale at `main` (`57889b2`, cycle-5 era), whose `kitsu-render` contains none of the ◆/◇/◈/⛩ glyphs the task asks about.** Reset to the dogfood head before either run so both CLIs saw the same real source. Loop-infra note, not a product gap.

## Task

> Explore the `crates/kitsu-render` source and write a NEW file `docs/render-notes.md` (10-15 lines) summarizing: its responsibilities, the glyph vocabulary (◆/◇/◈/⛩) and what each marks, and how plain/reduced-motion modes degrade output.

Ground truth established by hand before the runs (`crates/kitsu-render/src/lib.rs:355-373`, `:243-266`, `:712-721`, `:885-945`, `:1040-1067`): `◆` tool call (plain `tool>`), `◇` tool result (plain `result>`), `◉` approval (plain `o`), `◈` receipt/section (plain `#`), `⛩` torii session-boundary milestone (plain `*`); `plain_mode()` triggers on `--plain`/`KITSU_PLAIN`/`NO_COLOR`/`TERM=dumb`; `spinner_mode()` = Heartbeat (non-TTY) → StaticPlain (plain) → StaticStyled (reduced motion) → Animated.

## Commands

**Run A**
```
time claude -p '<task>' --permission-mode acceptEdits 2>&1 | tee …/cycle15-runA-claude.log
```

**Run B** (token redacted; supplied inline only)
```
time printf '%s' '<task, "using your file tools">' \
 | HF_TOKEN=*** /…/target/debug/kitsu --config /…/kitsu.toml --gateway huggingface \
   --max-rounds 30 --timeout-secs 420 --max-output-tokens 4000 --yes chat --allow-read-write . \
   2>&1 | tee …/cycle15-runB-kitsu.log
```
Run B repeated verbatim (`cycle15-runB2-kitsu.log`) to test variance, plus one read-forcing probe (`cycle15-runB3-probe.log`) to exercise the `◆ Read` narration the task calls for.

## Wall times

| Run | Wall | Rounds | Tool calls | Result |
|---|---|---|---|---|
| A — claude `-p` | **33.5 s** | — | Read ×2 | 19-line file, accurate |
| B — kitsu + HF Llama-3.3-70B | **19.9 s** | 1 | Write ×1, **0 reads** | 1-line file, fabricated |
| B2 — verbatim repeat | 1.9 s | 1 | Write ×1, **0 reads** | 1-line file, fabricated (different fabrication) |
| B3 — read-forcing probe | 2.6 s | 1 | List ×1, Read ×1 | wrong answer — **caused by kitsu, see K1** |

No stalls; no run approached the 420 s timeout. Hosted HF is dramatically faster per round than the local-qwen baseline of cycles 11-14 (296 s → 2-20 s).

## Per-dimension comparison

| Dimension | claude `-p` | kitsu + HF 70B | Verdict |
|---|---|---|---|
| Startup | silent until done; no context/skill preamble | `~ loaded project context … (685 tokens; disable with --no-project-context)` + `~ 2 skill(s) available` — two useful, honest lines | **kitsu** |
| Read/explore narration | not shown in `-p` mode at all | `◆ List(…/kitsu-render/src)` / `◆ Read(…/src/lib.rs)` + `◇ 1828 lines` — structured and legible **but factually misleading** (K1, K2) | kitsu on form, loses on truth |
| Progress visibility | none until the final answer (33 s of silence) | `~ still waiting on the model (15s)…` heartbeat fired at 15 s in run B; backoff working | **kitsu** |
| Write transparency | prose claim only ("wrote docs/render-notes.md, ~16 lines") | `◆ Write` + `◇ created · 338 bytes` + `◈ verified file checkpoint #1` with a host-verified snapshot diff | **kitsu, clearly** |
| Receipts / truthfulness | self-reported, unverified — its "~16 lines" was actually 19 | run summary `1 turn(s) ok, 1 file(s) written (1 change(s)), session not saved` — truthful about writes, **silent about reads and about context surgery** (K1, K4) | mixed |
| Output quality | 19 lines, every glyph and every mode correct incl. `◉` and the deck states | 1 line, no trailing newline, **every specific fact invented** | **claude, by a mile** |

Regression checks on cycle-14 items: the **context-window false alarm is gone** (`kitsu-context: {"used":1362,"effective":127072,"window":131072}` on every HF turn, no mismatch warning) — cycle-14 gap #2 confirmed fixed. **No skill-confusion receipts fired** in any of the four runs (the new "ignored a text-form call to skill…" path was never exercised — the 70B emits real `tool_calls`). G5 receipt path elision still bites: `◆ Read(…/src/lib.rs)` elides the one segment that disambiguates it from four other crates' `lib.rs`.

## Evidence (trimmed)

**Run A output file** (`docs/render-notes.md`, verified line-by-line against source — correct):
```
- `◆` — a tool call (also "running" in agent-deck rows); fox-orange on a styled TTY.
- `◇` — a tool result summary (dimmed; ember-red on error); also "spent/failed/stopped" deck rows.
- `◈` — a receipt/section header (`/jobs`, `/profiles`) and "result ready" deck state.
- `⛩` (torii) — a session-boundary milestone: compaction, `/new`, `/resume`, checkpoint `/undo`.
Reduced motion (`--reduced-motion`, `KITSU_REDUCED_MOTION`) keeps styling but replaces the animated fox-kaomoji
spinner with a static "* kitsu is thinking" line written as a complete physical line.
```

**Run B output file** (whole file, 338 bytes, no newline at EOF):
```
The kitsu-render crate is responsible for rendering the terminal output. It uses a glyph
vocabulary to convey information. The glyphs are: 📱 for success, 💨 for failure, and 💻 for
warning. The plain mode degrades the output by removing all glyphs and colors. The
reduced-motion mode degrades the output by removing all animations.
```
The verified-diff receipt is what exposed this — the streamed assistant text rendered the same sentence *without* the emoji, so only the `◈` checkpoint diff showed what actually landed on disk. (Checked: kitsu's sanitizer preserves emoji — `is_invisible_format_control` explicitly excludes them — so the divergence is the model emitting two different texts, not a render bug. Filed as M5.)

**Probe B3 — the cycle's headline finding.** Asked to read `lib.rs:350-380` and report the glyph constants:
```
◆ Read(…/src/lib.rs)
  ◇ 1828 lines
The glyph constants … are:
* `ESC`: … `\u{1b}`   * `CSI`: … `\u{009b}`   * `OSC`: … `\u{009d}`   * `ST`: … `\u{009c}`
kitsu-usage: {"prompt_tokens":5630,"completion_tokens":192,"total_tokens":5822}
```
The model reported the constants at **lines 12-15** and never mentioned `GLYPH_TOOL_CALL` at line 365. That looks like a model failure. It is not. `lib.rs` is 68 484 bytes; the read returned ~64 KiB (`fs.rs:329-354` byte cap), the receipt summarised **that** at "1828 lines" (`orchestrator.rs:1331-1337`) — and *then* `maybe_offload_for_tools` (`orchestrator.rs:1345`) replaced the entire result in context with a pointer plus a **500-character preview** (`OFFLOAD_CHAR_CAP = 8 * 1024`, `OFFLOAD_PREVIEW_CHARS = 500`, `:2768-2828`). The first 500 characters of `lib.rs` are exactly the module doc comment, the `use` block, and `const ESC/CSI/OSC/ST`. **The model answered perfectly from the only content kitsu gave it.** `prompt_tokens` rising just 4311 → 5630 across a "1828 line" read is the fingerprint. Nothing in the transcript told the user any of this happened.

## Model-fault ledger (not kitsu's)

- **M1** — Zero exploration on the real task, both runs: the 70B went straight to `Write` despite "using your file tools" in the prompt. Reproducible, not a one-off.
- **M2** — Confabulated the entire glyph vocabulary rather than declining: 📱/💨/💻 in run B, `*`/`-`/`+` in B2.
- **M3** — Ignored the 10-15 line format constraint in both runs (1 line, no trailing newline).
- **M4** — Did not use `start_line`/`end_line` in the probe even though the prompt named a line range, pulling the whole 68 KB file instead.
- **M5** — Streamed assistant text and the written file content disagreed (emoji present on disk, absent in the narration); kitsu's sanitizer is not responsible.
- Exculpated: the probe's wrong glyph answer is **kitsu's**, not the model's (K1).

## Ranked NEW gaps

1. **Large tool results are silently swapped for a 500-char preview, and the `◇` receipt describes the pre-swap content.** The user is told "1828 lines"; the model receives ~500 characters. The 8 KiB threshold is a fixed byte constant that ignores the window — here it fired with **1.5 k of 131 k tokens used**, destroying the agent's ability to answer a question it had the file for. *Fix:* gate `maybe_offload` on remaining context budget (skip entirely while utilization is low), and compute the `◇` summary **after** offload so it reads `◇ 1828 lines · offloaded, 500-char preview in context → <path>`. `crates/kitsu-core/src/orchestrator.rs:1331-1345`, `:2768-2828`.
2. **`◇ N lines` never signals truncation.** `fs_read_file` appends `...[truncated: output exceeded byte cap]` into the payload at the 64 KiB cap, but `format_tool_result`'s fallback arm just counts lines, so a clipped read is indistinguishable from a complete one. *Fix:* surface the `CappedText.truncated` flag through to the summary → `◇ 1828 lines (truncated at 64 KiB)`. `crates/kitsu-tools/src/builtins/fs.rs:326-354,461-463` + `crates/kitsu-core/src/orchestrator.rs:1745-1748`.
3. **`start_line`/`end_line` are applied *after* the 64 KiB byte cap**, so any range past ~64 KiB silently returns empty — and the offload pointer's own advice ("Re-read it (or a line range) with fs_read_file on that path") is unreachable for exactly the large results that trigger it. *Fix:* stream-scan to the requested range before capping, or cap bytes *within* the selected range. `crates/kitsu-tools/src/builtins/fs.rs:437-458`.
4. **The run summary has no read/explore counter**, so "wrote a doc about source it never opened" and "researched then wrote" print identically. `tool_results_elided` is already accumulated and never rendered anywhere. *Fix:* extend the line to `1 turn(s) ok, 0 file(s) read, 1 file(s) written (1 change(s)), 1 result(s) elided`. `crates/kitsu/src/main.rs:7923`; counter at `crates/kitsu-core/src/orchestrator.rs:566,587,617`.
5. **`◇ 2 lines` for a directory listing** — wrong noun and a meaningless unit; the fallback arm has special cases for grep, writes, and agents but not `fs_list_dir`. *Fix:* add a `"fs_list_dir" => format!("{n} entr{y}")` arm. `crates/kitsu-core/src/orchestrator.rs:1745-1748`.

Honorable mention (not top-5): `kitsu-context: {"used":1587}` against the provider's `prompt_tokens: 5630` on the same round — kitsu's own context meter under-counts by ~3.5× on the HF path, which is what makes gap #1 invisible from the user side too.

## Verdict

The model excuse is genuinely gone. Hosted Llama-3.3-70B emitted clean structured `tool_calls` every time, at 2-20 s per round versus qwen's minutes, and when it read a file it read it correctly. What remains is kitsu's: **the context-management layer silently discards 99 % of a tool result and the receipt layer reports the discarded version.** That single pair of bugs converted a correct read into a wrong answer and made the failure look like the model's. Kitsu still wins outright on write transparency (the verified checkpoint diff is the only reason run B's fabrication was visible at all), progress visibility, and startup honesty — but a receipt that describes content the model never received is worse than no receipt, because it is believed.

## Artifacts

- `cycle15-runA-claude.log`, `cycle15-runA.diff`, `cycle15-runA-render-notes.md`
- `cycle15-runB-kitsu.log`, `cycle15-runB.diff`, `cycle15-runB-render-notes.md`
- `cycle15-runB2-kitsu.log`, `cycle15-runB2-render-notes.md`
- `cycle15-runB3-probe.log`

Worktree left clean at `3207684`.
