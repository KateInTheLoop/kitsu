# Kitsu UI Guidelines

Kitsu aims to deliver a delightful but disciplined terminal UX.

**Rewritten in T3-S13.** The previous version predated the `kitsu-render`
crate, and one of its statements was simply false — it listed `✨` as the
thinking indicator, a glyph that appears in **zero** source files, while the
four symbols the product actually uses were absent. This file now states what
the code does, and points at the documents that specify it rather than
paraphrasing them into a fourth version.

## Principles

- Cute but professional.
- Structured progress visibility.
- Minimal noise — and **never** decoration that implies an affordance that does
  not exist. A glyph hinting at a key that does nothing is the failure mode this
  project refuses by name (requirement R5 declines the `←` form until the
  left-arrow binding ships).

## The output tiers are specified elsewhere, and this file does not restate them

Styling, `--plain`, `NO_COLOR`, non-TTY and assistive output follow the
**three-tier model in `docs/ux-design.md`** (its opening section). That document
is normative; treat any summary here as a pointer, not a rule. The short
version: `--plain` is a **byte contract**, not a theme — several requirements
promise its bytes are stable, so a change that "improves" plain output is a
breaking change.

## The symbol set the code actually uses

| glyph | meaning |
|---|---|
| `🦊` | kitsu's identity, in the splash and banner |
| `⛩` | a milestone or section marker |
| `◆` | a tool call starting |
| `◇` | a tool receipt — what the call returned or cost |
| `◉` | a permission request (`◉ permission — …`) |
| `◈` | a secondary marker in agent and session surfaces |

The **thinking indicator is not a glyph** but the fox kaomoji spinner
(`( ･ᴥ･ )ﾉ`), which degrades to a static `* kitsu is thinking` under
`--reduced-motion` or a non-TTY. See `docs/ux-design.md`'s live-status-row
section.

## The two renderers

Kitsu has two, selected by `--tui <classic|inline>`, `[tui] mode`, or `/tui`
at runtime; `classic` is the default.

- **classic** — output scrolls normally; a single rewritable status row sits at
  the transcript bottom during a turn.
- **inline** — a DEC scroll region pins rows at the bottom of the terminal and
  the transcript scrolls above them. The pinned row carries the model, a
  context meter, elapsed turn time, `[N queued]` and the background-agent tag,
  and sheds those segments in a **documented order** as the terminal narrows
  (requirement R5).

**Rules that apply to anything drawn in the viewport:**

1. **Compose once, for both renderers.** The row's content lives in
   `kitsu-render` because `kitsu-render` cannot depend on the viewport crate and
   both renderers need the same text. A composer in the viewport crate could
   never serve classic. The viewport's own module is a *caller*.
2. **The pinned row's painters are relative** (`\r`, erase-line, cursor-up), so
   they act on whatever row the cursor occupies. Any writer that can finish
   **mid-row** must declare it, or the next repaint erases the transcript line
   sharing that row. This is not hypothetical — it destroyed a streamed answer's
   last line once (finding F47).
3. **Respect `--reduced-motion` as "no animation", not "frozen".** The elapsed
   segment stops its periodic refresh; the queue and background-agent tags keep
   updating, because those are **state** the user must act on. Suppressing them
   to make the row quiet would hide a blocked agent.
4. **Stay inside the frame budget** — at most 60 repaints a second, enforced in
   the painter rather than inherited from a caller's sleep. Collapsing frames
   must keep the **newest** value, never drop what arrived during the window.
5. **Restore the terminal on every exit path**, including panics, `SIGINT` and
   `SIGTERM`. A pinned scroll region left behind is worse than leaving raw mode:
   the terminal stays visually broken until `reset`.

## Layout

A turn reads top to bottom as: the prompt echo, tool calls (`◆`) and their
receipts (`◇`) interleaved with assistant text, then a turn summary line. In
`inline` the pinned status row sits below all of it and never scrolls.

Permission requests (`◉`) render as a **bottom-pane modal** in the viewport and
as a scrolling prompt in classic; either way the transcript keeps the same
`◉ permission —` receipt afterwards, so the record of what was asked and
allowed is identical (requirement R7).

## Where to look for the normative detail

- `docs/ux-design.md` — the tiers, the live status row, the surfaces.
- `docs/tui-and-agent-view-design.md` — the lettered requirements (R, A, N, B)
  and their amendments.
- `docs/functionality.md` — user-facing behaviour, commands, config keys.
