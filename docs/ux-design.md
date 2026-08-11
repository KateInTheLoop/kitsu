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
  heartbeat.
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
- **`/help`**: grouped under six headings (session; context & history; files,
  diffs & checkpoints; agents & jobs; permissions; models & reasoning) from
  the single command registry; aliases fold into their parent row as
  `(alias: /x)`. Headings are bold on a styled TTY only — row text is
  byte-identical in every mode and in the JSONL payload.
