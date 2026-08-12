# Cycle 16 · audit 01 — offload-truthfulness fix verification

Date: 2026-08-12 · Auditor: Sonnet (cycle-16 rotation slot)
Worktree: `/home/mmpg/workspace/kitsu/kitsu2/kitsu/.claude/worktrees/agent-af6e853f766c9aa69`
Commit: `9b75247` (`dogfood/codex-ux-cycle-6` tip) — worktree branch
`worktree-agent-af6e853f766c9aa69` was already seeded at this commit (matches
the branch tip exactly); no reset needed, contrary to the prompt's
expectation of staleness. Binary: freshly built
`target/debug/kitsu` (155 MB, timestamped today).

Mission: re-run cycle-15's failed render-summary task against the
just-shipped offload fix (`crates/kitsu-core/src/orchestrator.rs`:
`offload_pressure` gating + honest `◇ … → context got a 500-char preview;
full text at <path>` receipt suffix) and see whether the outcome flips.

## TEST 1 — cycle-15 task replay, kitsu + HF Llama-3.3-70B, full 131k window

Command (token redacted):
```
time printf '%s' '<task: explore kitsu-render, write docs/render-notes.md 10-15 lines>' \
 | HF_TOKEN=*** target/debug/kitsu --config kitsu.toml --gateway huggingface \
   --max-rounds 30 --timeout-secs 420 --max-output-tokens 4000 --yes chat \
   --allow-read-write .
```

**Result: NOT FIXED — but for a reason the offload fix cannot touch.**

Ran twice (verbatim repeat, matching cycle-15's B/B2 methodology):

| Run | Wall | Tool calls | Result |
|---|---|---|---|
| 1 | 3.32s | Write ×1, **0 reads** | 1-line fabricated file, 381 bytes |
| 1b (repeat) | 2.17s | Write ×1, **0 reads** | 1-line fabricated file, 385 bytes |

Both runs: the model went straight to `Write` without calling `fs_list_dir`
or `fs_read_file` at all, identical to cycle-15's B/B2 (M1: "zero exploration
on the real task, both runs... reproducible, not a one-off"). Since no read
ever happened, `maybe_offload_for_tools`/`offload_pressure` never fired —
**the offload machinery was never exercised by this task**, so this run
cannot confirm or deny the fix. It only reconfirms the pre-existing,
separate model-behavior gap (M1) is still present and still reproducible on
this 70B.

Accuracy grade of the produced file (against real source,
`crates/kitsu-render/src/lib.rs`):
```
The kitsu-render crate is responsible for rendering the terminal output. It uses
a glyph vocabulary to convey information. The glyph vocabulary includes markers
such as *, -, and +, which indicate different types of information. The plain
mode degrades output by removing all glyphs, while the reduced-motion mode
degrades output by replacing all glyphs with their text equivalent [or, run 1b:
replacing all animations with static images].
```
- **Wrong glyph vocabulary**: invents generic `*`/`-`/`+` instead of the real
  `◆`/`◇`/`◉`/`◈`/`⛩` (verified `lib.rs:365-373`).
- **Wrong plain-mode behavior**: plain mode does NOT "remove all glyphs" — it
  maps each to a stable ASCII fallback (`◆→*`, `◇→>`, `◉→o`, `◈→#`, `⛩→*`,
  verified by the `own_glyph_set_maps_to_stable_ascii_in_plain_mode` test at
  `lib.rs:1690-1694`).
- **Wrong reduced-motion behavior**: run 1 claims glyphs get "text
  equivalents"; run 1b claims "animations become static images" — neither
  matches reality (spinner degrades Animated → StaticStyled → StaticPlain →
  Heartbeat, `lib.rs:712-721`; glyphs are untouched by reduced-motion).
- **Format violated**: 1 line, no trailing newline, vs. the requested 10-15
  lines (same M3 as cycle-15).
- Grade: **0/5 facts correct**, same failure class as cycle-15's B run.
  Claude's baseline (33.5s, accurate 19-line file, not re-run this cycle
  since the comparison target is unchanged) still stands as the reference.

`kitsu-usage`: `prompt_tokens: 4314-4315` both runs — small, consistent with
zero reads (no offload-starvation signature to check here; see TEST 2 for
that).

Diff/artifacts: `docs/render-notes.md` never committed (repo left clean);
saved copies at `cycle16-run1-render-notes.md`, raw logs at
`cycle16-run1-kitsu.log` / `cycle16-run1b-kitsu-repeat.log` in this
scratchpad dir.

## TEST 2 — offload honesty under a genuinely small window (`--context-window 8192`)

First attempt (task exactly as briefed, no explicit no-range instruction):
the model called `fs_read_file` with an implicit small line range and got
back only 4 lines — **under** the 8 KiB offload threshold, so offload never
triggered (not a bug; correctly gated on actual pressure now, but this
particular model behavior didn't generate pressure). Wall: 2.03s.

Retried with an explicit "read the entire file in one call, no
start_line/end_line" instruction to force the pressure scenario the task
wanted to probe:

```
◆ Read(…/src/lib.rs)
  ◇ 1828 lines → context got a 500-char preview; full text at
    /tmp/kitsu-offload-3473091-1786556633192942470/runtime-3473091-1786556633192942470-0/call_g9qntbwtwwwn8xt4im2jgnrw-0.txt
    (model can fs_read_file it)
```

**Result: FIXED, confirmed.**

- The new receipt suffix `→ context got a {N}-char preview; full text at
  <path> (model can fs_read_file it)` **does appear**, exactly as shipped
  (`orchestrator.rs:1379-1391`).
- The pointer path is **complete**, not elided — full absolute
  `/tmp/kitsu-offload-.../....txt`, unlike the G5 elision that bites the
  `◆ Read(…/src/lib.rs)` *header* line (that header still shows `…/src/lib.rs`,
  dropping the crate segment — G5 is unaffected by this fix, as expected,
  since it targets a different line).
- The `◇` line still leads with the honest pre-offload line count
  (`1828 lines`), matching the design intent ("compute the summary AFTER the
  offload decision, using the pre-swap content" — `orchestrator.rs:1357-1361`).
- The model's answer is now **accurate given what it actually received**: it
  described (1) the `jsonl` module declaration, (2) the `use` block, (3) the
  `ESC`/`CSI`/`OSC`/`ST` constants and `ASSISTANT_OUTPUT_WRITTEN` static —
  verified against `head -c 550 lib.rs`, which is exactly that content. This
  mirrors cycle-15's probe (B3) finding exactly, except now the receipt
  discloses the swap instead of hiding it.
- `kitsu-context: {"used":1535,"effective":4192,"window":8192}` — no false
  window-mismatch alarm at the small window either (see TEST 3a).

Source check for `offload_pressure` gating logic
(`orchestrator.rs:2843-2857`): `used.saturating_add(added).saturating_mul(2)
> effective` — offload only fires when keeping the result inline would push
past half the effective budget. Confirmed this is genuinely pressure-gated,
not a flat byte cutoff as before.

## TEST 3 — regression sweep

**(a) Window false-alarm stays gone on HF:** Confirmed both in TEST 1
(`window:131072`, no mismatch) and TEST 2 (`window:8192`, `effective:4192`,
no mismatch) — cycle-14's fix holds under both a full and an artificially
small window.

**(b) Run summary truthful on the file write:** TEST 1's summary — `1
turn(s) ok, 1 file(s) written (1 change(s)), session not saved` — is
accurate about the write itself (no false claim of reads). It still doesn't
*disclose* the zero-read count (cycle-15 gap #4, "run-summary read counter,"
is a separate, still-open item — out of scope for this fix and not
re-tested here beyond confirming it wasn't accidentally fixed as a
side-effect).

**(c) No skill-confusion receipts:** None fired in any of the 4 runs
(TEST 1 ×2, TEST 2 ×2) — expected, since the 70B emits real `tool_calls`,
consistent with cycle-15.

**Gates:** `cargo fmt --check` clean. `cargo test -p kitsu-core offload`: 5/5
pass (`maybe_offload_is_noop_without_a_dir`,
`maybe_offload_keeps_small_results_inline`,
`maybe_offload_writes_large_result_and_returns_pointer`,
`maybe_offload_never_clobbers_an_earlier_result_for_the_same_call`,
`trim_tool_results_preserves_offload_pointer_but_drops_preview`) — all
exercise the lower-level `maybe_offload`, not the new `offload_pressure`
gate itself (see gap #2 below).

## Cycle-15 vs cycle-16 outcome comparison

| Aspect | Cycle 15 | Cycle 16 |
|---|---|---|
| Offload receipt on a genuine big-read/small-window swap | Silent — `◇ 1828 lines` with no disclosure | **Discloses**: `◇ 1828 lines → context got a 500-char preview; full text at <path>` |
| Offload gating | Flat 8 KiB byte cap, fired even at 1.5k/131k tokens used | Pressure-gated (`used+added)*2 > effective`) — did not fire at all in TEST 1's full-window run since no reads happened; correctly withheld in TEST 2's first attempt (4-line read, no pressure) |
| Cycle-15's literal replayed task (write render-notes.md) | 1-line fabrication, 0 reads | **Unchanged**: 1-line fabrication, 0 reads (both runs) — model-side M1, not touched by this fix |
| Probe task (force a big read under pressure) | Wrong answer, no disclosure, blamed on "model fabrication" | Same partial-content answer (500-char preview is unavoidable content-wise) but now **honestly disclosed** — the user/auditor can now tell this is a preview, not a full read |

**Bottom line:** the offload-truthfulness fix does exactly what it was built
to do — it doesn't (and can't) make the 70B explore before writing (M1 is a
model behavior, not a context-management bug), but it closes the actual
kitsu-side bug from cycle-15: a receipt that silently described content the
model never received. That bug is fixed. Cycle-15's headline task doesn't
"flip" to a good outcome because it was never actually caused by the offload
bug in the first place — the write task never reads anything, so the
fabrication is 100% model-side and was already correctly identified as such
in cycle-15's ledger.

## Ranked top-5 remaining gaps

1. **M1 persists, still kitsu's problem to route around, not fix in the
   model:** the 70B skips exploration entirely on a "write a summary" task
   phrased with "using your file tools," 2/2 reproductions this cycle (plus
   2/2 in cycle 15 — 4/4 overall). Consider a stronger system-level nudge
   (e.g., requiring at least one read before a write on tasks that
   reference source exploration) or surfacing the zero-read count
   prominently enough that no user could miss it (ties into gap #4 below).
   `crates/kitsu-core/src/orchestrator.rs` (prompt/tool-forcing policy).
2. **No dedicated unit test for `offload_pressure` itself.** The 5 passing
   `offload`-named tests all exercise `maybe_offload` (the unconditional,
   dir-and-cap-only helper); nothing in the test suite asserts that a
   low-utilization turn *skips* offload or that a high-utilization turn
   *triggers* it at the right threshold — this fix's core new logic is
   currently verified only by hand (this audit), not by CI. `orchestrator.rs:2843-2857`.
3. **Run-summary read counter still missing (cycle-15 gap #4, confirmed
   still open):** `1 turn(s) ok, 1 file(s) written (1 change(s))` renders
   identically whether the model read zero files (TEST 1, both runs) or
   several — the already-tracked `tool_results_elided`/read-count data still
   isn't surfaced. This is exactly the receipt that would have made TEST 1's
   fabrication legible without needing a diff.
4. **G5 path elision remains unfixed** (not in this fix's scope, confirmed
   still present): `◆ Read(…/src/lib.rs)` drops the crate segment while the
   sibling offload-pointer path on the very next line renders the full
   absolute path — an inconsistent elision policy between the two
   nearly-adjacent lines of the same receipt block.
5. **Model self-limits the read range to dodge the offload threshold**
   (new observation, TEST 2's first attempt): given a small window, the 70B
   sometimes supplies `end_line` itself and reads only 4 lines rather than
   the whole file — silently under-informing itself in a different way that
   never reaches the offload path at all, so its honesty guarantee doesn't
   apply. Not a kitsu bug, but worth tracking: "did the model choose to read
   less than it needed" produces the same starved-answer risk as offload
   truncation, with no receipt of any kind to flag it.

## Artifacts

- `cycle16-run1-kitsu.log`, `cycle16-run1-render-notes.md`
- `cycle16-run1b-kitsu-repeat.log`
- `cycle16-run2-kitsu-smallwindow.log` (4-line self-limited read, no offload)
- `cycle16-run2b-kitsu-smallwindow.log` (forced full read — offload fix confirmed)

Worktree left clean at `9b75247`; no session files persisted (`--yes` runs
all reported "session not saved"); `/tmp/kitsu-offload-*` scratch dirs from
this and prior sessions are ephemeral tool artifacts outside the repo, left
as-is.
