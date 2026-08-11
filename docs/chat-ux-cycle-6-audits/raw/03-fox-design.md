# Cycle 6 — Audit 3: Visual identity & terminal aesthetics

Auditor: UX auditor 3 (design). Date: 2026-08-10.
Method: read `docs/ui_guidelines.md`, `docs/agent_deck_ui.md`, `design.md` §8, and
`crates/kitsu-render/src/lib.rs`; captured real styled output under a pty
(`script -qec`) for: chat banner, `/help`, `/context`, `/status`, one-shot `ask`
(spinner + stream), tool activity + permission prompt + denied-tool error,
provider connection error, timeout error; verified `--plain`, `NO_COLOR`, and
non-TTY pipe discipline; compared with `codex --help` (codex-cli 0.147.0) and
publicly known Codex TUI behavior. No repo files were modified.

Note: `design.md` §8 says "Full UX specification is in `docs/ux-design.md`" —
**that file does not exist**. The de-facto spec is ui_guidelines.md + §8.

---

## 1. Promised vs implemented

| Promise (docs) | Source | Implemented? |
|---|---|---|
| 🦊 fox identity symbol | ui_guidelines.md "Core symbols" | **No.** 🦊 appears only in a sanitizer unit test. Banner mascot is a **cat** (`/\_/\` + `( o.o )` + `> ω <`); spinner frames are cat kaomoji (`(=^･ω･^=)`, `ฅ(^•ω•^)ฅ`). Code comments admit "fox/cat mascot", "cat/fox kaomoji". Only real fox note is the 狐 kanji in the banner title. |
| ⛩ milestone symbol | ui_guidelines.md | **No.** Never emitted. Compaction milestone uses `✻` — Claude Code's glyph. |
| ✨ thinking symbol | ui_guidelines.md | **No.** Spinner uses kaomoji frames; static mode uses `*`. |
| Diamond tool glyphs `◆ ◇ ◈ ◉` | design.md §8 | **No.** Tool lines use `⏺` and `⎿` — Claude Code's exact glyph pair (lib.rs:725, 742). |
| Palette: FOX, amber, cream, teal, sage, ember "all defined in kitsu-render" | design.md §8 | **No.** Only `FOX = (255,140,0)` exists (lib.rs:342). Other colors are stock owo-colors green/red/cyan/yellow for diffs/notices. |
| Code blocks: rounded box borders, language tag, syntect highlighting | design.md §8 | **No.** No syntect dependency. Buffered messages dim the ``` fence line only; **streamed** output (the normal path) prints raw markdown with zero styling. |
| --plain / NO_COLOR / non-TTY discipline | ui_guidelines.md, §8 | **Yes — genuinely good.** See §3. |
| Header / progress pipeline / tool receipts / summary layout | ui_guidelines.md | Partial: banner header, `⏺/⎿` receipts, `↳` end-of-turn stats exist; no progress pipeline. |

## 2. Captured surfaces (ANSI stripped)

### 2.1 Chat banner (TTY) — the strongest surface, but it's a cat
```
  ╭─ kitsu 狐 ──────────────────────────────────────
  │   /\_/\
  │  ( o.o )   multi-gateway LLM familiar
  │   > ω <
  │
  │  gateway     ollama
  │  model       qwen2.5-coder:latest
  │  directory   /tmp/...scratchpad
  │  tools       8 enabled (use /tools)
  ╰──────────────────────────────────────────────
  Enter / for commands · Tab completes prefixes · Ctrl+D exits
```
All FOX orange + dim labels; disciplined and close to Codex's session card.
Issues: mascot is a cat, not a kitsune; box is half-open (no `╮`/`╯`, top rule
42 cells, bottom 46 — ragged right edge); no 🦊/⛩ anywhere.

### 2.2 One-shot `ask` — success-path noise glued to output
```
print(1)
```~ inference provider attempt 1/3: success; HTTP 200; body completed; terminal=true; 120306ms deadline remaining

  ↳ 42 input · 8 output · 46 / 7,680 context (0% used)
```
Two defects in one capture: (a) a provider debug line ("attempt 1/3: success;
HTTP 200 ...") prints on **every successful turn**; (b) it is written to stderr
with no leading newline while stdout's closing ```` ``` ```` has no trailing
newline, so it fuses into `` ```~ inference provider... `` on one line. The
same fusion occurs in `--plain` ("...let me know.~ inference provider attempt
1/3: success..."). The `↳` dim stats line itself is good.

### 2.3 Spinner (TTY)
`(=^･ω･^=) kitsu is thinking･･･` — orange kaomoji + dim label, `\r`-animated,
180 ms first-frame delay, erased cleanly with `\r ESC[2K`. Well engineered,
wrongly branded (cat frames, incl. `ฅ` paws). Non-TTY heartbeat is excellent:
`~ still waiting on the model (10s)...` every 15 s. Reduced-motion/plain static
line `* kitsu is thinking` stays in transcript by design.

### 2.4 Tool activity + permission prompt + tool error
```
⏺ Read(etc/hostname)                      ← ⏺ in FOX orange; note lost leading "/"
⏺ kitsu wants to read /etc/hostname       ← same ⏺ but UNCOLORED here
  [y] allow once   [a] always (/etc)   [n] deny
>
  ⎿ error: PolicyDenied — policy denied: user denied read access to '/etc/hostname'   ← dimmed, not red
```
- `⏺`/`⎿` are Claude Code's glyphs; design.md promised `◆ ◇ ◈ ◉`.
- The permission prompt — the highest-stakes moment in the product — has no
  color, no bold, no visual weight, and its glyph duplicates the tool-activity
  glyph (two different meanings, one symbol). The *command* permission prompt
  (main.rs:10479) has no glyph at all, styled or not: `command permission> run ...`
  — two approval surfaces, two different designs.
- Tool *errors* render dim — visually *de*-emphasized relative to normal text.
- Display bug: `Read(etc/hostname)` drops the leading `/` of `/etc/hostname`.

### 2.5 Error display (dead gateway, TTY)
```
~ inference provider attempt 1/3: connection; transport failed; terminal=true; 9997ms deadline remaining
  ↳ 1 / 7,680 context (0% used)
~ provisional assistant output, if any, was DISCARDED and was not committed: the model turn failed before commit
chat request could not reach the gateway - check the selected gateway's base_url, network access, and provider availability; run `kitsu doctor --online`
```
Zero styling on a TTY: no red, no marker hierarchy. Four lines where Codex
shows one crisp red-tagged line. The "DISCARDED" line is alarming and
irrelevant when nothing streamed; a context-stats line prints for a turn that
produced nothing. (The render crate *has* a styled red error notice —
`PromptSafeAsyncNotice::error` → `! error: ...` in red — the error path just
doesn't use it.) Timeout error had the same shape.

### 2.6 /help — 44-row wall
44 alphabetical rows, no grouping, no color at all on a TTY (only the two-space
indent). `/agents` sits next to `/agent` as a "backward-compatible alias";
read/write/revoke permissions commands are scattered alphabetically.

### 2.7 /context and /status
Context meter: `used 1177 / 7680 effective  [■■□□□□□□□□□□□□□□]  15%  headroom: 6503`
— bar in FOX orange, but filled ■ and empty □ get the **same color**, so fill %
reads only by glyph shape. Provenance table (source/tokens/policy/why) is
excellent content, dim header, plain rows. /status is a clean aligned key-value
sheet (nice), fully unstyled.

### 2.8 Exit-path noise
```
~ captured type-ahead: 1 / 8 prompts queued
~ end of input received; exiting after queued prompts finish
~ running queued prompt (0 remaining; 8 queue slot(s) available)
```
Internal queue bookkeeping leaks to the user at `/exit`.

## 3. Discipline check — PASS (this is kitsu's real strength)

- `NO_COLOR=1` on a pty: banner collapses to one plain line
  `kitsu - gateway: ollama | model: ... | tools: 8`, prompt `kitsu> `, no ESC bytes. ✔
- `--plain` on a pty: `* kitsu is thinking`, ASCII `->`/`|` stats markers, no ANSI. ✔
- Non-TTY pipe: heartbeat lines, `kitsu-usage:`/`kitsu-context:` JSON records,
  no ANSI, no box drawing; `ascii_structural_markers` maps `╭╮╰╯·—↳⏺⎿` to ASCII. ✔
- Streamed model output passes through a stateful terminal-escape sanitizer
  (split-CSI/OSC safe, bidi-control exposure). Better than most CLIs, Codex included. ✔

## 4. Codex comparison

`codex --help` is stock clap — kitsu's `--help` is equivalent. Codex's actual
bar is the TUI: one accent color used *systemically* (borders, active states,
approval panes), a bordered composer, one-line status/working indicator, an
approval dialog that visually interrupts (bordered, labeled, colored), grouped
slash-command popup, and silence on the success path — no transport diagnostics
ever reach the transcript. kitsu already has the single-accent idea (FOX
orange) and better pipe discipline than Codex; it loses on: success-path noise
(§2.2, §2.8), unstyled errors (§2.5), approval-moment weight (§2.4), /help
information design (§2.6), and — unlike Codex, which owns its look — kitsu's
glyph set (`⏺ ⎿ ✻`) is borrowed from Claude Code while its own documented set
(`◆ ◇ ◈ ◉ ⛩ 🦊`) sits unimplemented.

## 5. Ranked proposals (max 8)

Every proposal keeps the existing three-tier degradation: styled TTY → non-TTY
Unicode-no-ANSI → `--plain`/`NO_COLOR` ASCII. FOX = `38;2;255;140;0`.

### P1 — Silence the success path (polish, biggest Codex gap)
Gate `~ inference provider attempt N/M: ...` lines behind a `--verbose-provider`
flag (or emit only for retries/failures, never `success`), and make stderr
diagnostics after a streamed turn always start with a fresh line (emit `\n`
before the first stderr line if stdout's last byte wasn't `\n`). Also gate the
three `~ queue/type-ahead` exit lines to actual queue activity (>0 queued).
- Fallbacks: n/a (removal); behavior identical in all modes.
- Acceptance: `script -qec "kitsu ... ask 'Print a python code block with print(1). Nothing else.'" out` →
  stripped output contains a line that is exactly ```` ``` ```` and contains
  **no** `inference provider attempt` and no `attempt 1/3: success`.

### P2 — Style errors as errors
Route terminal failure lines through the existing red notice path:
`! error: chat request could not reach the gateway — ...` (red, `31m`/truecolor
ember `#E4572E` if the palette lands), keep the actionable hint sentence, drop
`provisional assistant output ... DISCARDED` unless tokens actually streamed,
and suppress the `↳ ... context` stats line when the turn failed. Render tool
failures (`⎿ error: ...`) in red, not dimmed.
- `--plain`/non-TTY: `! error: <same text>`, no ANSI (already implemented in
  `PromptSafeAsyncNotice`).
- Acceptance: `script -qec "kitsu --gateway vllm --timeout-secs 5 ask hi" out` →
  raw capture contains `[31m! error:` (or FOX/ember SGR) and does **not**
  contain `DISCARDED` or `↳`; same command piped (no pty) → `! error: ...`
  with zero ESC bytes.

### P3 — Implement the documented diamond glyph set; stop borrowing Claude Code's
Replace `⏺`→`◆` (tool call, FOX orange), `⎿`→`◇` (result, dim; red on error
per P2), `◉` reserved for the approval prompt (P5), `◈` for milestone/receipt
lines (e.g. checkpoint created). Retire `✻` (see P6). Update
`ascii_structural_markers`: `◆`→`*`, `◇`→`>`, `◉`→`o`, `◈`→`#`.
- Non-TTY: `◆ Read(/etc/hostname)` uncolored; `--plain`: existing `tool>` /
  `result>` prefixes unchanged.
- Also fix the dropped leading `/` in `Read(etc/hostname)`.
- Acceptance: pty chat triggering a read → raw capture matches
  `38;2;255;140;0m◆.*Read\(/etc/hostname\)` and contains no `⏺` or `⎿`;
  `--plain` run still prints `tool> ` / `result> `.

### P4 — Foxify the banner and spinner (kitsune, not neko)
Banner mascot becomes a fox: separated pointed ears and ᴥ muzzle, title gains
the guideline's identity emoji:
```
  ╭─ 🦊 kitsu 狐 ─────────────────────────────╮
  │   /\   /\                                 │
  │  ( ･ᴥ･ )   multi-gateway LLM familiar     │
```
(cat markers `/\_/\`, `( o.o )`, `> ω <` removed) and close the box: equal-width
top/bottom rules with `╮`/`╯`. Spinner frames drop cat kaomoji (`ω`, `ฅ` paws)
for a fixed fox face with a sweeping tail: frames
`( ･ᴥ･ )ﾉ`, `( ･ᴥ･ )～`, `( ･ᴥ･ ) ～`, `( ﾉᴥﾉ )～` + existing `･` dots, FOX
orange, label "kitsu is thinking" unchanged.
- Non-TTY banner: keep the one-line `kitsu — gateway: ...` (no emoji);
  `--plain`: keep `kitsu - gateway: ...` ASCII line; spinner fallbacks
  unchanged (`* kitsu is thinking`, heartbeat).
- Acceptance: pty `chat` capture contains `🦊 kitsu 狐` and `ᴥ` and no `ω`/`ฅ`/
  `( o.o )`; `NO_COLOR=1` pty capture still equals the single plain banner line.

### P5 — Give the approval moment visual weight, and one design for both prompts
File and command approval share one header: `◉ permission — kitsu wants to
<verb> <path>` with `◉` + verb in bold FOX orange, path in default color, and
one aligned option line `[y] allow once · [a] always (<scope>) · [n] deny`
(letters bold). Command approvals use the same header (`◉ permission — run
<argv>`) with the existing cwd/executable/timeout detail rows dimmed beneath.
- `--plain`: keep current `permission>` / `command permission>` text exactly.
- Acceptance: pty session reading outside the root → raw capture matches
  `1m.*38;2;255;140;0m.*◉ permission` and both prompts (file + command) share
  the `◉ permission —` prefix; `--plain` run shows `permission>` with no ESC.

### P6 — Torii milestones ⛩ for session-boundary events
`⛩` (FOX orange) marks moments where the session passes a gate, matching the
folklore (torii = threshold to the kami's domain): compaction
(`⛩ context compacted — older turns summarized`, replacing Claude Code's `✻`),
`/new` session start, `/resume` and checkpoint `/undo` completion. Maximum one
torii line per event; never in tool receipts (keeps "never spam emojis").
- Non-TTY: same line uncolored; `--plain`: current `* context compacted ...`
  (add `⛩`→`*` to `ascii_structural_markers`).
- Acceptance: pty chat, long history, `/compact` → capture contains
  `⛩ context compacted` and no `✻`; `--plain` `/compact` prints
  `* context compacted (older turns summarized)`.

### P7 — /help information design
Group the 44 commands under dim-orange (or bold) section headers —
`session`, `context & history`, `files, diffs & checkpoints`,
`agents & jobs`, `permissions`, `models & reasoning` — descriptions dimmed,
alias rows (`/agents`, `/quit`) folded into their parent as `(alias: /agents)`.
Optionally prefix each header with `◈`. No per-row color.
- `--plain`/non-TTY: same grouping/indentation, no ANSI.
- Acceptance: pty `/help` → capture contains `permissions` as a standalone
  header line and ≤ 6 header groups; `NO_COLOR=1` `/help` output has zero ESC
  bytes and identical row text.

### P8 — Name the palette, and make the context meter legible at a glance
Define the documented palette in kitsu-render as named constants:
`FOX (255,140,0)`, `AMBER (255,191,0)` warnings, `EMBER (228,87,46)` errors,
`SAGE (155,175,131)` success, `TEAL (54,117,136)` info/links,
`CREAM (255,253,240)` highlight text — and use them (P2 errors → EMBER,
notices → TEAL/SAGE) instead of stock red/green/yellow. Context meter: filled
`■` in FOX, empty `□` dimmed; ≥80 % utilization tints the filled cells AMBER,
≥95 % EMBER — a quiet capacity warning like Codex's context indicator.
- `--plain`: `[####------------]` unchanged; non-TTY: `■□` uncolored.
- Acceptance: pty `/context` at <80 % → `□` cells carry `2m` (dim) while `■`
  carry FOX SGR; unit test: bar at 96 % renders EMBER SGR; `--plain` output
  is ASCII `#`/`-` with no ESC.

## 6. Bugs found along the way (file separately)
1. stdout/stderr fusion: diagnostics glue to last streamed char (§2.2) — also
   affects `--plain`.
2. `Read(etc/hostname)` drops the leading `/` from the requested path (§2.4).
3. Permission-prompt glyph `⏺` is colored in tool lines but uncolored in the
   prompt; command-approval prompt has no glyph in any mode (§2.4).
4. Failed turns still print the `↳ ... context` stats line (§2.5).
5. Banner top/bottom rules differ in width; box never closes (§2.1).
6. `docs/ux-design.md` referenced by design.md §8 does not exist.
