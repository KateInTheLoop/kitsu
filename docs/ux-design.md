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

  **The slash-command palette** sits above the composer whenever the draft is a
  bare single-token slash prefix. It is the **same renderer** the classic
  Rustyline prompt uses — one function owns the labels, both frame sets, the
  entry formatting, the row cap and the "N more" row — so the two surfaces
  cannot drift; a test asserts the inline rows equal the classic output. The
  composer is always the **last** row, the palette above it.

  It is **built to the height it will be granted**, never built full and then
  clipped: clipping would drop the top row, which is the frame and its label,
  leaving a box with no lid. On a short window the box is simply smaller, with
  fewer entries and an honest "N more" count that reflects what the *height*
  hid, not only the row cap. Below three rows of palette plus one of composer
  there is no palette at all rather than a partial frame.

  The palette stands down in three cases, so it never contradicts the row it
  sits above: when the draft is not a bare slash prefix, when the prefix is
  already a command's whole name (there is nothing left to offer), and when a
  completion hint is showing that is not the lead entry's own completion. The
  third case is a consequence of the during-turn hint's priority list being
  reused at the idle prompt, and is tracked rather than intended.

  Every keystroke repaints **every** pinned row, not just the composer —
  otherwise the palette above would show matches for a prefix already changed.

  **The viewport's height follows its content, clamped to what the window can
  hold.** A caller hands over the rows it wants drawn and the viewport becomes
  that tall, re-latching the region on each change — the same operation a
  terminal resize performs. The ceiling is the minimum-height rule: two
  scrolling rows must remain above the viewport, so a request taller than that
  is **shortened rather than refused**, because a palette too tall for the
  window should lose rows, not take the composer down with it. Refusing is
  reserved for a window so short that not even one row can be pinned.

  When rows must be dropped, **the topmost go first**: the composer is the
  bottom row and the palette sits above it, so the row being typed into is the
  last thing lost. The count drawn always equals the latched height — extra rows
  would land inside the scroll region and scroll away with the transcript, which
  looks like a vanishing palette rather than an off-by-one.

  **Rows are fitted to the terminal's width before they are drawn.** A pinned
  row wider than the terminal would wrap into the row below it and overwrite
  it — harmless with one row, corrupting with several, which is why this is
  settled before the composer grows. Fitting is escape-aware and shares the
  renderer's own width primitives: escape sequences cost zero columns, a wide
  character costs two and is never split across the boundary, and a row that
  already fits is passed through **byte-identically** so the output tiers stay
  byte-comparable. A truncated row that carried colour or attributes gets one
  reset appended so its styling cannot bleed into whatever is drawn next;
  a truncated plain row stays plain.

  **Resize re-pins the viewport, and releases it when the terminal is too
  short.** `DECSTBM` margins are absolute row numbers, so a resized window
  invalidates them: the region must be re-set rather than patched. A `SIGWINCH`
  handler stores into one atomic flag — the only async-signal-safe action
  available to it — and the reader, whose blocking read the signal interrupts,
  checks the flag on waking, re-latches the geometry and repaints the live
  draft (not the status, since a resize happens mid-read). A resize to the same
  dimensions emits nothing, so a spurious signal cannot flicker the row.

  Re-latching is the same pair of operations as a suspend/resume at new
  dimensions, minus the paint, which is why all four cases — grow, shrink,
  release, re-install — fall out of one hide-then-show sequence: `hide` is inert
  with no latched geometry and `show` is inert below the minimum height. That
  minimum is the same bound the renderer uses at startup: a terminal must hold
  at least two scrolling rows above the viewport, because the region's bottom
  margin has to exceed its top. **Crossing that bound downward releases the
  viewport entirely** — region reset, rows cleared, nothing pinned — rather than
  keeping a region whose margins no longer fit the screen; crossing back up
  re-installs it.

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

  **History and reverse search work; completion and the palette do not, for one
  shared reason.** Inline mode borrows rustyline's history persistence wholesale
  without ever running its composer — the file is loaded at startup, the inline
  reader is handed that same list, and submitted lines are recorded on the
  common path after the read — so Up/Down walks previous sessions. Ctrl+R is
  resolved entirely inside `TypeaheadEditor`, which emits no action for it, so
  reverse search works at the inline prompt as it does in the during-turn row.

  **The hint slot**, mirroring the during-turn row: the pinned row carries one
  hint beside the draft — the reverse-search status (`⌕ reverse-search '…'`)
  while Ctrl+R is active, otherwise the inline slash-completion suffix (typing
  `/s` hints `teer`). The two differ in how they compete for columns, exactly as
  in the classic row: the search status is **reserved** and claims its columns
  before the draft is windowed, so it survives a long draft, while the
  completion suffix is unreserved and is dropped when the row is too narrow.

  **Tab accepts the hint**, which is parity with the during-turn row and the
  same seven-line behaviour: take the draft, compute the slash-completion
  suffix, and append it if one is showing. Two details are load-bearing. The
  suffix is recomputed from the draft rather than read back out of the hint
  slot, because the slot holds the *search status* while Ctrl+R is armed and
  appending that would type `⌕ reverse-search '…'` into the prompt — reachable,
  not theoretical, since actions are handled after a whole read completes and a
  later Ctrl+R in the same read can arm the status first. And a search is not
  special-cased: the editor treats any control key in search mode as accept,
  exit, re-feed, so by the time Tab is seen the draft is the matched history
  line and no slash hint applies.

  Still absent at the inline prompt: Rustyline's completion **lists** — `@file`
  path completion and the bordered slash-command palette, which lists and
  filters commands over several rows and so needs a viewport taller than one
  row. (An earlier version of this file wrongly bundled Tab in with those; it
  is a hint, they are lists, and only the lists need the extra rows.)

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
