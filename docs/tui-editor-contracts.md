# Inline editor contracts (Stage 1A draft)

This is the working contract for Stage 1B and the paste milestones. It
changes no terminal behavior. Stage 0 acceptance and executable layout fixtures remain open.

## Ownership and coordinates

`kitsu-editor` remains the input and draft-state authority. A pure layout
module below the terminal renderer will consume the editor's source text,
first-row prompt, cell width, and a display projection. The renderer and
vertical navigation will consume the **same** layout result. The layout must
be computed for the entire draft before clipping it to the available viewport.
Neither the editor nor this module may depend on ANSI output, `InlineSurface`,
or a terminal handle.

Source offsets are Unicode scalar (`char`) boundaries, matching the current
`TypeaheadEditor::cursor()` API. Visual positions are zero-based terminal
cells within physical rows. The projection returns source-boundary-to-cell
positions and the reverse mapping, with an explicit affinity for two source
boundaries that occupy the same cell (for example a combining mark). Wide
characters occupy two cells; a combining mark occupies zero. A requested
vertical target in the middle of a wide character lands on that character's
source boundary. This keeps source text intact while allowing later grapheme
motion without changing the snapshot format. Explicit newlines break rows;
tabs display as `⇥`, matching today's renderer, rather than expanding to an
environment-dependent tab stop. The prompt occupies cells on the first row
only. A future paste token is a projection atom with a source range, identity,
and independently measured label width, never replacement source text.

The first pure fixtures must pin: empty draft with a long prompt; a newline
at the start and end; a soft wrap at exactly the last usable cell; a character
of width two at the wrap boundary; a base character plus combining mark;
multiple wraps above the visible slice; and a paste label wider than the
viewport. Each fixture asserts both source-to-cell and cell-to-source results.

## Draft snapshot and transitions

The live snapshot owns expanded source content, scalar cursor, optional
selection, preferred vertical cell column, history index and stashed original
draft/cursor, reverse-search query and saved draft/cursor, and paste identity
plus range metadata. The decoder keeps partial UTF-8/escape/paste bytes until
a complete input event exists; no overlay transition may split one such event.
Terminal guards, file descriptors, viewport coordinates, and paint caches are
outside the snapshot. Submission and persisted history contain expanded text.

| Transition | Draft contract |
|---|---|
| Transcript or agent overlay, attach/cycle/detach | Restore the same snapshot after closing; only the prompt label/route may change. |
| Suspend/resume, resize, async transcript write | Restore content and edit state; recompute visual layout from current width. |
| Approval handoff | Park the snapshot while the approval owns stdin; restore it when input returns. |
| Idle/during-turn handoff, queued recall | Transfer the snapshot as state, without replaying text bytes. A queued prompt becomes a new editable draft. |
| Completion | Replace the requested source range, clear an active history walk, and retain unaffected paste metadata. |
| Submit or explicit discard | End the live snapshot; history stores expanded text only. |

The current `InlineRead` string payloads and `Cursor::new(carried).chain(stdin())`
sites in `read_chat_line_inline` are migration sites. During a history walk,
Up/Down first traverse visual rows of the recalled entry. Up at its first row
recalls the next older entry; Down at its last row recalls the next newer entry
or restores the original draft **and cursor**. Editing a recalled entry adopts
it as the new live draft and ends the walk; the stashed original is then
discarded by that explicit edit. Search cancellation restores its saved draft
and cursor. History and search store expanded text, not paste labels.

## Input consumers and keys

| Consumer | New behavior in these milestones |
|---|---|
| Inline idle composer | Visual Up/Down and snapshot restoration in 1B; paste selection/popup in 2A/2B. |
| Inline during-turn collector | Same draft/layout behavior when the box is active; queued recall retains priority at the empty-draft Up boundary. |
| Classic and fullscreen | Keep their current keymaps and expanded-text fallback; preserve the entire draft on handoff. |
| Approval and overlay line editors | Keep their current keymaps; a paste stays content and never invokes an inline-box action. |

Ordinary Enter submits, Tab requests slash completion, and Alt+Enter inserts a
newline. Home/End move to the start/end of the **visual row** in the inline
composer; Ctrl+A/E keep their existing full-draft start/end meaning. Up/Down
move between visual rows with a preferred cell column and enter history only
at full-draft boundaries. Ctrl+Left/Right keep word motion; Shift+Left/Right
keep agent cycling. Ctrl+O, Ctrl+R, and the configured chord prefix keep their
current actions outside a paste-selection mode. None of these bindings may
activate inside a bracketed-paste payload, even if the bytes arrive in pieces.

For 2A/2B, the configurable `<prefix> p` chord enters visible paste selection
at the nearest block and repeats to select the next block; `<prefix> b` selects
the previous block. These letters are unused by today's agent chords (`a`,
`d`, and `1`–`9`). Selection does not arise merely from caret position.
Enter opens the selected block's popup; Esc leaves selection without changing
the draft. In the popup, Enter inserts a newline, Esc saves and closes, Ctrl+C
discards popup edits and returns to the parent, and EOF closes without saving
then propagates EOF to the parent reader. A real interrupt restores terminal
state and reports interruption. This policy keeps the parent Enter submission
unreachable while the popup owns stdin.

## Viewport budget

The minimum inline frame reserves one transcript row, one status row above the
composer, two borders, one content row, and a footer below the composer when
space permits. The first row to drop is optional palette height, then the
footer's descriptive text becomes a compact essential hint. Search status and
active selection state take priority over completion suggestions. If the frame
itself cannot fit while preserving the transcript minimum, use the existing
single-row fallback and keep the caret inside that row. Painting and caret
placement must use one latched geometry per input event.
