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
  terminal. stdin drops to raw mode (`ICANON|ECHO` off,
  `ISIG` kept so Ctrl+C still cancels) and bytes feed the pure
  `TypeaheadEditor` keymap: printable insert, ←/→/Home/End, Backspace/Delete,
  ↑/↓ history (seeded from the rustyline history, with a draft stash),
  Ctrl+U/K/W kills, Ctrl+D EOF on an empty draft. Raw mode enables
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
  their rows from one shared helper, so they cannot drift.
  `/profiles` uses the same `◈` receipt header with rows
  prefixed `◆` (tool-capable / inherits session grants) or `◇` (reasoning-only
  / no tools), reusing the tool-call/tool-result glyphs to signal capability
  level rather than tool activity.
- **`/help`**: grouped under six headings (session; context & history; files,
  diffs & checkpoints; agents & jobs; permissions; models & reasoning) from
  the single command registry; aliases fold into their parent row as
  `(alias: /x)`. Headings are bold on a styled TTY only — row text is
  byte-identical in every mode and in the JSONL payload.
