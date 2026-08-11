# Agent Deck UI

The "agent deck" is Kitsu's inline rendering for background-agent state —
not a separate terminal UI. It shares the normal chat scrollback (no
alternate screen, no panes) and is produced by `/jobs`, `/agent`, and
`/profiles`.

`/jobs` and `/agent` print one `◈ agent deck — …` receipt line with running
totals (`N total · N running · N done · N failed · N cancelled · N running
slot(s) free · N record slot(s) free`), followed by one row per background
job/agent, or `no background agents` when empty.

`/profiles` prints `◈ background-agent profiles — N available` followed by
one row per profile, prefixed `◆` (tool-capable) or `◇` (reasoning-only, no
tools), each showing model, reasoning-effort policy, granted tools, and
whether it inherits session grants.

See docs/ux-design.md for the glyph legend and docs/functionality.md for the
full command semantics (`/jobs steer`, `/jobs prune`, `spawn_agent_bg`, etc.).

## Future ideas (not implemented)

An earlier design sketch explored a tmux-like detach/attach terminal UI with
a persistent `Agents | Output` split-pane layout for concurrently running
agents. That design was not built — the shipped implementation is the inline
receipt-line rendering described above. The split-pane concept remains a
plausible direction for a future cycle if concurrent-agent output-watching
becomes a priority.
