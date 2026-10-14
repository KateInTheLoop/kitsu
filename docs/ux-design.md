# Kitsu UX design — palette, glyphs, and degradation rules

Companion to `design.md` §8 and `docs/ui_guidelines.md`. This documents what
`crates/kitsu-render` actually implements; when the code and this file
disagree, fix one of them in the same change.

## Output tiers

Every decorated surface degrades through three tiers:

1. **Styled TTY** — destination is a terminal, and none of `--plain`,
   `NO_COLOR`, `KITSU_PLAIN`, `TERM=dumb` are active: truecolor palette,
   Unicode glyphs, box drawing, animation.
2. **Non-TTY Unicode** — piped/redirected: same glyphs and text, zero ANSI,
   no animation (the spinner becomes a periodic plain heartbeat), and stable
   machine records (`kitsu-usage:`, `kitsu-context:`) instead of the human
   stats line.
3. **`--plain` / `NO_COLOR` ASCII** — pure ASCII: glyphs map through
   `ascii_structural_markers`, prompts keep their `permission>` /
   `command permission>` / `tool>` / `result>` text forms, zero escape bytes.

`--reduced-motion` (or `KITSU_REDUCED_MOTION`) replaces the animated spinner
with one static styled line; JSONL mode (`kitsu.chat.v1`) suppresses all of
this and owns both channels.

## Palette (named constants in `kitsu-render`)

| Name    | RGB             | Use |
|---------|-----------------|-----|
| `FOX`   | `255, 140, 0`   | identity accent: prompt, banner, tool-call glyph, milestones, context fill |
| `AMBER` | `255, 191, 0`   | warnings; context meter fill at >=80% utilization |
| `CREAM` | `255, 253, 240` | highlight text (reserved) |
| `TEAL`  | `54, 117, 136`  | informational `~` notices |
| `SAGE`  | `155, 175, 131` | success `+` notices |
| `EMBER` | `228, 87, 46`   | errors: `! error:` notices, failed tool results, context fill at >=95% |

Diff rendering keeps conventional green/red/cyan.

## Glyphs (kitsu's own set — not Claude Code's `⏺ ⎿ ✻`)

| Glyph | Constant            | Meaning                    | `--plain` |
|-------|---------------------|----------------------------|-----------|
| `◆`   | `GLYPH_TOOL_CALL`   | tool call (FOX)            | `tool>` prefix (`*` in free text) |
| `◇`   | `GLYPH_TOOL_RESULT` | tool result (dim; EMBER on error) | `result>` prefix (`>`) |
| `◉`   | `GLYPH_APPROVAL`    | permission/approval prompt | `permission>` prefix (`o`) |
| `◈`   | `GLYPH_RECEIPT`     | receipts/section marker    | `#` |
| `⛩`   | `GLYPH_TORII`       | session-boundary milestone | `*` |
| `🦊`  | banner title        | identity (banner only)     | absent (one-line banner) |
| `狐`  | banner title        | kitsune kanji              | absent |

Torii milestones mark the session passing a gate — compaction, `/new`,
`/resume`, checkpoint `/undo` — one line per event, never in tool receipts.

## Key surfaces

- **Banner**: closed `╭─ 🦊 kitsu 狐 …╮ / ╰…╯` box (56 inner cells), fox
  mascot (`/\   /\` ears, `( ･ᴥ･ )` face). A value row wider than the box
  keeps its left bar and overflows without truncation. Non-TTY: one `kitsu —
  gateway: …` line; plain: same in ASCII.
- **Spinner**: fox kaomoji frames `( ･ᴥ･ )ﾉ`, `( ･ᴥ･ )～`, `( ･ᴥ･ ) ～`,
  `( ﾉᴥﾉ )～` in FOX, padded to one width; reduced-motion/plain: static
  `* kitsu is thinking`; non-TTY: `~ still waiting on the model (Ns)...`
  heartbeat. The animated frames render through the shared live status row
  (below), never via bare `\r` writes.
- **Live status row** (`kitsu-render::live`): during a foreground turn on a
  styled interactive TTY, one rewritable stderr row at the transcript bottom
  holds the spinner segment, two dimmed ambient tags, and a live type-ahead
  draft — `kitsu> <text>` with a real terminal cursor. The tags sit leftmost
  in the order background agents then queue: `[N blocked]` or `[N agent(s)]`
  (blocked outranks running, hidden when both are zero, refreshed from the
  registry watch once per input poll tick) followed by `[N queued]`. Both tag
  widths participate in the row's fit math, so the cursor column accounts for
  them and the spinner is still the first segment dropped on a narrow
  terminal. stdin drops to raw mode on the **input** flags only
  (`ICANON|ECHO|ISIG` off in `c_lflag`, `c_oflag` untouched so `OPOST|ONLCR`
  keep supplying the carriage return every ordinary `println!` relies on) and
  bytes feed the pure
  `TypeaheadEditor` keymap: printable insert, ←/→/Home/End, Backspace/Delete,
  ↑/↓ history (seeded from the rustyline history, with a draft stash),
  Ctrl+U/K/W kills, Ctrl+D EOF on an empty draft. Ctrl+C is deliberately
  **not** left to the kernel: clearing `ISIG` delivers it as a byte, so a
  non-empty draft is cleared rather than the turn cancelled, and only on an
  empty draft does the collector `raise(SIGINT)` itself — the same signal the
  kernel would have sent, so cancellation is unchanged. Approval handoffs
  restore the original termios, so approval prompts keep kernel Ctrl+C. Raw mode enables
  bracketed paste (`ESC[?2004h`, off again on release): a pasted block stays
  ONE draft — its newlines insert literally (`\r\n` collapsed, tabs kept)
  and render as `⏎`/`⇥` placeholders on the row — and Alt+Enter inserts a
  manual newline; terminals without bracketed paste keep line-at-a-time
  submission. Enter routes exactly like canonical type-ahead (`/steer`,
  `/jobs`, `/queue`, `/quit cancel` immediate; everything else queues) and
  prints an immediate teal `~ queued: <text>` receipt. Transcript writers wrap output in
  `with_live_row_suspended` (clear row → write line → repaint), an
  over-wide draft windows around the cursor with `…`, a too-narrow terminal
  drops the spinner segment first, and approval prompts hide the row and
  restore canonical echo while they own stdin. An unsubmitted draft is
  carried into the next live editor or pre-filled into the next idle prompt.
  Non-TTY, `--plain`, and assistive sessions keep the canonical
  line-collector loop with byte-identical output.
- **Inline viewport** (`kitsu-tui`, preview, gated behind `KITSU_TUI=inline`):
  an alternative to the live status row that keeps its rows **outside** a DEC
  scroll region instead of repainting one row at the transcript bottom. On
  entry it sets the region to rows `1..=height - viewport_rows`, parks the
  cursor on the region's last row, and paints the status rows below it; on exit
  it clears those rows, releases the region, and leaves the cursor on the first
  freed row so later output lands below the transcript rather than over its
  top.

  **The two are mutually exclusive, and the viewport wins.** Both want the
  bottom rows, and a live row painted while a region is active lands *inside*
  it and scrolls away with the transcript. So installing the viewport sets
  `kitsu_render::live::set_live_rows_enabled(false)`, which
  `live_rows_supported()` consults alongside the existing styled-stderr and
  plain checks. It defaults enabled, so a session without the viewport is
  byte-identical to the pre-viewport renderer.

  Because the region scrolls on a line feed regardless of which writer produced
  it, transcript output that bypasses the render seam entirely — the binary's
  several hundred direct prints, including all slash-command output — still
  flows into the region correctly and leaves the status rows intact. This is
  measured, not assumed, by the styled pseudo-terminal tests in
  `crates/kitsu/tests/tui.rs`. The measurement holds **at idle in cooked
  mode**, where the terminal still supplies the carriage return those writers
  omit; a session that holds raw mode throughout needs them re-measured.

  **What the pinned row shows.** It is the status row while the draft is empty
  and the composer (`kitsu> <draft>`) as soon as anything is typed, rather than
  an always-on composer. An empty draft showing `kitsu> ` would claim the row
  for an editor that has nothing in it, and the row is the only place a status
  can live once the live status row stands down.

  **The idle prompt in inline mode is kitsu's own editor, not Rustyline**,
  because Rustyline would draw its prompt line inside the scroll region with no
  knowledge of the region. stdin uses the same input-flags-only raw mode as the
  during-turn type-ahead row — `ICANON|ECHO|ISIG` cleared in `c_lflag`,
  `c_oflag` untouched — which is what lets ordinary `println!` output keep
  working while the reader holds the terminal. The editor is the same pure
  `TypeaheadEditor`, so the keymap matches the during-turn row. One divergence
  from that row: Ctrl+C on an empty draft clears the line and keeps the session
  rather than re-raising `SIGINT`, because at an idle prompt there is no turn to
  cancel and the raise would take the default disposition and exit.

  **Suspension releases the terminal before stopping.** Ctrl+Z at the inline
  prompt clears the pinned rows, releases the scroll region and restores
  canonical terminal modes, *then* raises the stop signal, so the shell is
  handed an ordinary full-screen terminal rather than one whose scrolling area
  is a subset of the screen. On resume the region and status row are
  reinstalled and the unsubmitted draft replays through the same mechanism that
  carries a draft between prompts, so typing survives the cycle. The order is
  the correctness property — a stopped process cannot clean up after itself —
  so it is enforced behind a seam and pinned by a test rather than by review.

  Ctrl+backslash is deliberately inert rather than raising its usual signal,
  whose default action terminates *and dumps core*: this process holds API
  tokens and conversation text in memory, so honouring the convention would
  write credentials to disk on a stray keystroke.

  Not yet present at the inline idle prompt, and tracked: the slash-command
  palette, Tab completion, and cross-session history.

  Tiering is unchanged: the viewport requires a styled interactive TTY, so
  non-TTY, `--plain`, assistive and JSONL sessions never enter it and stay
  byte-identical.
- **Errors**: one weighted `! error: <message>` first line (EMBER, bold on a
  styled TTY) plus untinted hint lines; identical text with zero escape bytes
  elsewhere. Failed turns print no human stats line and no DISCARDED notice
  unless provisional output actually streamed.
- **Provider telemetry**: first-attempt success is silent; retries, failures,
  and successes after retries report as `~` lines (JSONL keeps every event).
- **Approval prompts**: file and command approval share
  `◉ permission — <request>` (bold FOX header) and the option line
  `[y] allow once · [a] <scope> · [n] deny` (bold letters); command approvals
  add dimmed provenance rows. `--plain` keeps the historical `permission>` /
  `command permission>` byte-exact forms.
- **Context meter**: filled `■` cells in FOX (AMBER >=80%, EMBER >=95%),
  empty `□` dimmed; plain `[####----]`.
- **Agent deck**: `/jobs` and `/agent` share one `◈ agent deck — N total · N
  running · N done · N failed · N cancelled · N running slot(s) free · N
  record slot(s) free` receipt line, then per-row status, or `no background
  agents` when empty. `/jobs view` renders that same data grouped by state —
  the header lead becomes `agent deck by state —`, and the rows sit under
  unglyphed headings `  <state> (N)` at two-space indent (rows 4, continuation
  8; the flat deck stays 2 and 6) in the fixed order `needs input`, `working`,
  `done, result ready`, `done, collected`, `failed`, `cancelled`, with empty
  groups omitted. Those headings bypass `deck_header`, so they are pure ASCII
  in every tier; both views build their counts from one shared expression and
  their rows from one shared helper, so they cannot drift. `/jobs explain <id>`
  renders one agent as a `deck_marker` header `agent #N explained | <phase>`
  followed by `  <label>: <value>` lines in a fixed order — `since`,
  `waiting on`, `because`, `last activity`, `steering`, `handoff` — each
  present only when it applies. Separators are ASCII `|` and the static text
  is pure ASCII, since these lines bypass `sanitize_plain_inline` too. Two
  ages, two formats, deliberately: `since` is a run duration at one decimal
  like the rest of this file, while an activity age uses
  `format_wait_elapsed`, matching the deck's own activity line.
  `/profiles` uses the same `◈` receipt header with rows
  prefixed `◆` (tool-capable / inherits session grants) or `◇` (reasoning-only
  / no tools), reusing the tool-call/tool-result glyphs to signal capability
  level rather than tool activity.
- **`/help`**: grouped under six headings (session; context & history; files,
  diffs & checkpoints; agents & jobs; permissions; models & reasoning) from
  the single command registry; aliases fold into their parent row as
  `(alias: /x)`. Headings are bold on a styled TTY only — row text is
  byte-identical in every mode and in the JSONL payload.
