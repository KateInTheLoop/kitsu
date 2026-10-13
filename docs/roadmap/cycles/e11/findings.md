# E11 — pre-existing findings discovered while implementing

Issues found during E11 that are **out of scope for the card that found them**.
Each is evidence-backed and none is a regression introduced by E11. Recorded
here so they are not rediscovered or silently inherited.

## F1 — `cargo fmt --all -- --check` does not pass at HEAD

Found: T1-S1. Four diffs, none in a file E11 touches:
`crates/kitsu-tools/src/permissions.rs:1809` and `:1862` (committed code, not
modified in any working tree, so necessarily pre-existing) and
`crates/kitsu-tools/src/builtins/fs.rs:397` and `:6625` (inside a maintainer's
uncommitted change).

`docs/ui-parity-plan.md` lists `cargo fmt --all -- --check` among the standing
gates that "must stay green", which is not currently true. E11 cards therefore
scope formatting to the packages they touch (`cargo fmt -p <pkg> -- --check`)
and leave the four diffs to their owner. Either the two committed diffs should
be fixed or the standing-gate claim should be corrected.

## F2 — `closest_jobs_subcommand` has already drifted from the command registry

Found: T1-S3. Its `SUBCOMMANDS` list (`crates/kitsu/src/main.rs:17751`) is
`list, show, steer, close-steering, stop, stop-all, prune` — it is missing
`close`, which the `/jobs` registry row advertises as `close <id>` and which
`handle_jobs_command` implements. A user who typos `clsoe` gets no suggestion.

One-line fix, but it needs a test to be worth anything, and E11 cards do not
write their own tests. Worth a small dedicated card that adds `close` and a
guard test asserting the suggester list and the registry row agree, which
would also stop this drift recurring.

## F3 — `◈` is already a deck row marker, contradicting a note in the design

Found: T1-S3. `docs/tui-and-agent-view-design.md` §4.4 says the agent view must
never use `◈` as a row glyph, reserving it for section headers. But the shipped
`/jobs` deck already uses it as the row marker for the result-ready state:
`DeckState::ResultReady` renders `◈` and the existing test
`the_jobs_deck_marks_each_state_for_the_active_terminal_mode` asserts
`styled.contains("◈ #")`.

`/jobs view` (T1-S3) reuses `deck_marker` unchanged, because consistency with
`/jobs list` beats the note's purity and inventing a second marker vocabulary
for the same data would be worse. T4-S1, which builds the agent view's rows,
must decide deliberately: either match the deck's existing vocabulary and amend
§4.4, or introduce distinct row markers and say why the two surfaces differ.

## F4 — `/jobs` end-to-end coverage is thinner than it looks

Found: T1-S3. There is exactly one scripted JSONL assertion for the whole
surface (`crates/kitsu/tests/cli.rs`, script at ~14814, assertions at ~14875):
`/jobs` bare, asserting `kind`, `action: "list"` and `snapshot.total == 0`.
None of `show`, `steer`, `stop`, `stop-all`, `prune`, `close`,
`close-steering` has end-to-end coverage, and the `/agents` alias has only a
`/help` registry assertion (~2636).

T1-S3 adds scripted coverage for `view` and for the alias forwarding it
widens. The remaining subcommands stay uncovered; a dedicated card could
extend the same scripted pattern to them.

## F5 — the `kitsu` package has no lib target, so `--lib` test invocations fail

Found: T1-S1. `cargo test -p kitsu --lib` errors with
`no library targets found in package 'kitsu'`; the correct invocation is
`cargo test -p kitsu --bins`. This cost one agent a wasted run. It is also the
root of design finding 1 in `reviews/tui-plan-review-1.md`: `kitsu-tui` cannot
import from the binary crate, which is why T0-S1 extracts the editor into its
own library crate.

## F6 — `docs/ux-design.md`'s live-row bullet still lags the implementation

Found: T1-S3, while documenting the T1-S2 tag. That file states "when the code
and this file disagree, fix one of them in the same change", but its
**Live status row** bullet never documented the `[N queued]` tag or the dimmed
queued-prompt stack rows, both of which shipped in earlier interactive-input
cycles (see `docs/ui-parity-plan.md` cycles 6 and 9).

T1-S3 documented both ambient tags, so the tag half is now accurate. Still
undocumented there: the queued-prompt stack rows above the status row, the
multi-line draft context rows, and the soft-wrap behaviour. A small
documentation-only card should close that, since the live region is now the
most-edited surface in `kitsu-render`.
