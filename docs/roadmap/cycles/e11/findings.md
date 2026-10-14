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

## F7 — `AgentProgress::last_activity_ms_ago` is documented as running-only but is not

Found: T1-S3 second card. The field's doc comment in
`crates/kitsu/src/subagent.rs` reads "Milliseconds since the agent's last
recorded activity (running rows)", which reads as a contract that only running
agents carry it. In fact it is populated for terminal agents too, which
`/jobs explain` relies on for its `last activity` line on failed and cancelled
agents.

The deck never exposed the discrepancy because `agent_activity_child_row` is
only called for running rows, by design ("terminal rows carry no child — their
story is complete"). So the comment described the deck's usage, not the field.

Latent trap: if someone later narrows the field to running-only to match its
comment, `/jobs explain` loses the `last activity` line for exactly the agents
where it matters most, and only the explain tests would catch it. The comment
should be corrected to describe the field rather than one caller, in whichever
card next touches that struct.

## F8 — `reject_jsonl_for_management` guards six commands and had no test

Found: T1-S5. `crates/kitsu/src/main.rs:2101` defines the helper that refuses
the global `--jsonl` flag on a host-management command and points the user at
that command's own `--json` instead. Six management paths call it. Before this
card, nothing in `crates/kitsu/tests/` asserted either the refusal or the
message: a grep for `reserved for chat automation` finds only the format string
itself.

So any of those six could silently stop rejecting `--jsonl`, or start naming
the wrong replacement command, with no test failing. T1-S5 adds the first such
test, for `kitsu agents` only, pinning both the refusal and the exact
replacement string.

The other five (`sessions`, `artifacts`, `auth`, and the remaining call sites)
remain uncovered. A small card could extend the same assertion to each; the
pattern is now established and each addition is three lines.

**Addendum (T1-S5).** The absence of that test had a second cost beyond the
missing coverage: with no precedent to copy, the test author modelled the new
assertion on the nearest-looking rejection, `--output-last-message`, which does
**not** pass `--jsonl` and therefore lands its error on stderr in human mode.
Under `--jsonl` the error is a framed event on stdout and stderr is empty, so
the borrowed shape was exactly backwards and the test failed against correct
code. One test over any one of the six callers would have made the channel
contract obvious. That is the real argument for the follow-up card: not
coverage for its own sake, but a correct example to copy.

## F9 — the declared MSRV of 1.80 is already unachievable, independent of E11

Found: T0-S2, while setting up the plan's MSRV gate. `Cargo.toml` declares
`rust-version = "1.80"`, and `cargo +1.80.0 check --workspace --all-targets`
fails with:

```
feature `edition2024` is required
The package requires the Cargo feature called `edition2024`, but that feature
is not stabilized in this version of Cargo (1.80.0)
```

This is **not** caused by anything E11 added. At commit `7f3133c`, before the
TUI crate existed, the committed lockfile already pins these at versions whose
manifests declare `edition = "2024"`, which Cargo 1.80 cannot parse at all:

| Package | Pinned at HEAD |
|---|---|
| `clap` / `clap_builder` / `clap_derive` / `clap_lex` | 4.6.1 / 4.6.0 / 4.6.1 / 1.1.0 |
| `zeroize` | 1.9.0 |
| `indexmap` / `hashbrown` | 2.14.0 / 0.17.1 |
| `time` | 0.3.55 |
| `assert_cmd` / `wiremock` | 2.2.2 / 0.6.5 |
| `home` | 0.5.12 |

`clap` alone is a core dependency, so the floor is raised for the binary
itself and not merely for dev targets.

**Consequence for E11:** the plan's T0-S2 gate
(`cargo +1.80.0 check --workspace --all-targets` green) cannot be met and was
never meetable. T0-S2 therefore holds a narrower, honest gate: the new crate
must not *further* raise the floor, verified by pinning `ratatui` to 0.29
(1.74) rather than 0.30 (1.88) and checking the resolved versions directly.
The plan's gate text is corrected in the same commit.

**Decision owed to the maintainer**, out of scope here and deliberately not
taken by E11: either correct `rust-version` to what the dependency tree
actually supports, or pin the tree back below the edition-2024 releases. The
second is a large, unrelated change touching `clap`. Until one happens, the
declared MSRV is documentation that does not hold, and CI does not check it —
`.github/workflows/rust.yml` runs only `cargo test` on stable, so nothing
would have caught this.

## F10 — ratatui 0.29 brings three duplicate dependency versions

Found: T0-S2. Adding `ratatui 0.29` plus `crossterm 0.28` leaves the workspace
carrying two copies of three crates. `crossterm` itself unifies at exactly one
version (0.28.1, shared by `kitsu-tui` and ratatui), which was the card's gate
and it passed; these are transitive:

| Crate | Pre-existing | Added by ratatui 0.29 | Via |
|---|---|---|---|
| `rustix` | 1.1.4 (`rustyline`/`fd-lock`, `tempfile`) | 0.38.44 | `crossterm 0.28` |
| `hashbrown` | 0.17.1 (`indexmap`) | 0.15.5 | `lru 0.12` → ratatui |
| `unicode-width` | 0.1.14 | 0.2.0 (exact pin) | ratatui |

None is unifiable without moving ratatui or crossterm, and ratatui cannot move:
0.30 requires Rust 1.88. So this is the price of the pin rather than a mistake,
and it is why the `vt100` dev-dependency had to drop to 0.15 — 0.16 needs
`unicode-width ^0.2.1` against ratatui's exact `=0.2.0`.

Cost, measured: about 5 s of one-off dependency compilation on a cold build
(47 crates in the closure), and nothing on the binary today because
`crates/kitsu` does not depend on `kitsu-tui` yet. The duplicates will reach
the shipped binary when the bridge lands in T0-S5, so the figure to watch is
release binary size at that point, not now.

Revisit when ratatui's minimum Rust is reachable, which depends on the
decision F9 asks for.

## F11 — `TuiRuntime::enter` infers terminal-ness from the process, not its writer

Found: T0-S2. The type is generic over its writer so the lifecycle can be
tested against a buffer, but a `W: Write` offers no way to ask whether it is a
terminal, and adding a bound to downcast would change the pinned signature. So
`enter` asks `std::io::stderr().is_terminal()` instead and enables raw mode on
that basis, while writing its escape sequences to the injected sink.

That is correct for every intended caller, because the interactive UI is drawn
to stderr, and it is commented at the site. But the coupling is implicit: a
future caller that passes a different stream while stderr happens to be a
terminal would get raw mode enabled for a stream it is not driving. If a second
destination is ever needed, terminal-ness should become an explicit constructor
argument rather than a process-wide question.

## F12 — the `Surface` seam cannot carry stdout until `StreamBuffer` takes a writer

Found: T0-S3b, by the test author, and verified. `StreamBuffer::push` and
`StreamBuffer::flush_unstyled` write to `io::stdout()` **themselves**
(`crates/kitsu-render/src/lib.rs` ~222, ~224, ~237), not to any writer passed
in. So once `print_fragment` and `finish_line` are routed through
`Surface::write_block`:

- `print_fragment`'s styled branch hands an **empty** block to a backend — the
  bytes have already left through `StreamBuffer`'s own handle.
- `finish_line` with a buffered partial line emits that partial line through
  the real descriptor while only the trailing newline flows through the
  writer.

For `ClassicSurface` the bytes are identical: same descriptor, same order,
inside the same single suspension. So T0-S3b is byte-identity-safe and its
routing is real in the sense that the call site and the suspension decision
move into the backend. But **the seam is not yet substitutable for stdout**: a
terminal-UI backend plugged in today would receive nothing for assistant text,
which is the single most important thing it needs.

This is a trap rather than a mere gap, because the routing makes the path
*look* handled. It is therefore recorded in three places: this finding, a
dedicated plan card (T0-S3c), and — most importantly — a doc comment at both
routing sites, since that is what the next implementer actually reads.

**The fix, deliberately not done in T0-S3b:** thread the block's writer
through `StreamBuffer::push` and `flush_unstyled`. That touches the line
styler and both functions' byte-count returns, so bundling it with the routing
would have recreated exactly the drift risk that splitting T0-S3 was meant to
avoid.

**RESOLVED in T0-S3c.** Both methods now take the block's writer, both routing
sites pass it, and the `GAP` comments were rewritten rather than left to
mislead a future reader into believing a closed limitation still applies. A
source-text guard test asserts the `impl StreamBuffer` block contains no
`io::stdout()`, so the decoupling cannot silently regress. T0-S4 and T0-S5 may
now assume stdout flows through the seam. Residual, and smaller: a failed
write is still swallowed exactly as it was against stdout, and the returned
byte count is computed from the formatted text rather than from what the
writer accepted — so a short-writing sink would make the count a lie, though
`write_all` masks that and both call sites ignore the count.

## F13 — the planned T0-S4 acceptance test is impossible: vt100 discards region-scrolled rows

Found: T0-S4, before any work started, by reading the emulator's source rather
than trusting the plan. `vt100 0.15.2`'s `Grid::scroll_up` is:

```rust
let removed = self.rows.remove(usize::from(self.scroll_top));
if self.scrollback_len > 0 && !self.scroll_region_active() {
    self.scrollback.push_back(removed);
    ...
}
```

and `scroll_region_active()` is true whenever `scroll_top != 0 || scroll_bottom
!= rows - 1`. The inline viewport sets a region covering rows 1 to
`rows - viewport_rows`, so the region **is** active and every row scrolled out
of it is dropped rather than pushed to scrollback.

The plan's T0-S4 acceptance — "200 alternating stdout/stderr lines through
`vt100::Parser` → screen rows show the viewport, **scrollback holds all 200 in
order**" — therefore cannot pass. Not because the implementation would be
wrong, but because the emulator models a terminal that does not retain
region-scrolled lines. An agent given that acceptance would have spent a whole
cycle chasing it.

**This is not merely a test-harness quirk; it is the design's open question Q1
showing up early.** Whether region-scrolled lines reach native scrollback is
terminal-dependent, which is exactly why T0-S6 exists and why the design keeps
a Codex-style per-cell `insert_history` fallback. vt100 is a concrete instance
of a terminal where region flow loses the transcript.

**Consequences, applied in T0-S4:**
1. The in-process emulator test verifies the **screen** half only: the viewport
   rows stay put under absolute positioning, transcript lines scroll within the
   region, and the cursor is parked inside it. The scrollback half is asserted
   *negatively* — a test pins that the emulator discards those rows, so the
   limitation is recorded in code rather than folklore.
2. Verifying that a **real** terminal retains region-scrolled lines is T0-S6's
   live matrix, which is where it always belonged. T0-S4 must not claim it.
3. The `insert_history` fallback stays deferred until T0-S6 reports. Building
   both mechanisms before measuring which is needed is speculative work, and
   the plan's own go/no-go exists to decide it.

The plan's T0-S4 acceptance text is corrected in the same commit.

## F14 — a raw-mode backend must emit CRLF; my byte contract said LF

Found: T0-S4, by the test author, from running the emulator rather than
reasoning about it. I specified `InlineSurface::transcript_line` as
`text + "\n"`. That is wrong for this backend: `TuiRuntime::enter` enables raw
mode, which clears `ONLCR`, so a bare line feed moves down without returning
to column zero and **every transcript line staircases**.

The author demonstrated it, then compensated inside the test helper by
prefixing each line with a carriage return — and said plainly that this meant
the test no longer proved the contract sufficient. That honesty is what made
the defect visible; the worst outcome would have been a contract that looks
tested and fails on real hardware during the live smoke, with a symptom
("output marching diagonally") several layers from its cause.

**Resolved in T0-S4**: the contract is `text + "\r\n"` for `InlineSurface`, and
the emulator helper's compensation was removed so the test proves the real
contract. The principle, worth stating because it will recur: **each backend
knows its own terminal mode.** `ClassicSurface` keeps a bare line feed because
it runs in cooked mode where the kernel translates; a raw-mode backend must
supply the return itself.

**Lesson for later cards:** any byte contract written for the TUI backend must
be checked against raw mode, not against what works in a cooked-mode terminal.
`write_block` is unaffected only because the block writes its own bytes — which
means *its* callers inherit the same obligation.

## F15 — the live status row and the inline viewport will fight; someone must choose

Found: T0-S4. `ClassicSurface::transcript_line` wraps every write in
`live::with_live_row_suspended`, which clears and repaints a row at the cursor.
`InlineSurface` does no suspension at all — it does not need to, because its
viewport is pinned outside the scroll region.

If both are active, the live row paints *inside* the scroll region and scrolls
away with the transcript, or fights the viewport's parked cursor.

The resolution is almost certainly that **the viewport replaces the live row**:
its bottom rows already carry the spinner, draft, queue tag and ambient agent
tag that the live row exists to show, so running both is duplication as well as
conflict. But that is a decision for the bridge card (T0-S5), which is where
both become reachable in one process, and it must be made explicitly rather
than discovered as flicker. Recorded so it cannot be inherited silently.

## F16 — the viewport has no resize and no row truncation

Found: T0-S4, both from the test author.

**Resize.** `show(rows, viewport_rows)` latches the bottom row, so a `SIGWINCH`
leaves a stale scroll region and `paint` writes to absolute rows that may no
longer exist. The terminal clamps, so the symptom is a silently drifting
viewport rather than an error. There is no `resize` in the API.

**Truncation.** `paint` emits `ESC[2K` then the row text, with no width limit,
so a row longer than the terminal is wide wraps into the next viewport row and
corrupts it. `kitsu-render`'s live row already does careful width fitting for
exactly this reason (see its `fit_draft_window` and the width-sweep tests), and
that logic is the precedent to reuse rather than reinvent.

Neither blocks the proof of concept, which uses a fixed size and short rows, so
both were deliberately left out of T0-S4 rather than bundled into a card that
is already the first to draw. Resize reflow is already scoped as T3-S8; row
truncation needs a `cols` input the current API does not take, and belongs with
it.

**F15 RESOLVED in T0-S5a** (recorded here since F15 sits above): the viewport
replaces the live row. `kitsu-render` gained `set_live_rows_enabled`, which
`live_rows_supported` consults, and the bridge disables the live row for as
long as the viewport is installed and restores the previous setting when its
guard drops. Default enabled, so a session without the viewport is
byte-identical. The reasoning that decided it: the viewport's bottom rows are
exactly where the spinner, draft, queue tag and ambient agent tag belong, so
running both would have been duplication as well as a fight over the same
cells.

## F17 — in inline mode, any writer that bypasses the seam corrupts the viewport

Found: T0-S5a, by the test author, as the limit of what its own tests can
prove. The bridge's tests inject sinks and assert the seam **routes** to them.
They cannot assert **exclusivity**, because the real process still owns the
actual descriptors.

So anything that writes a descriptor without going through the seam still
reaches the terminal directly: a stray `eprintln!`, a panic message, rustyline's
own prompt rendering, `tracing` output on stderr. In classic mode that is
merely untidy. **In inline mode it is corruption**, because the scroll region
makes a direct write land either inside the region — where it scrolls away with
the transcript — or on top of the pinned viewport rows.

This is the sharpest form of the proof of concept's question Q3, which already
counts 392 direct `eprintln!`/`println!` sites in `main.rs` (mostly slash-command
output). T0-S3a moved the suspension decision into the backend, but it did not
make the seam the only path to the terminal, and nothing short of a styled PTY
harness can tell the difference.

**Consequences:**
- T0-S5b owns the harness and is the first card that can observe this at all.
  Its `/help`, `/jobs` and `/status` tests exist precisely to catch it.
- The stray-writer census that T0-S6 was to perform is now *more* important
  than when it was written, and its result decides whether the slash dispatcher
  needs wrapping (cheap, one site) or whether individual writers must be routed
  (expensive, hundreds).
- Until then, inline mode is correctly gated behind `KITSU_TUI=inline` and
  documented as a preview. It must not become a default, nor be recommended,
  before the census.

**MEASURED in T0-S5b, and the answer is reassuring — with one condition.** The
styled PTY harness put a real binary on a real pseudo-terminal at 24x80 with
the viewport installed, printed all ~40 lines of `/help` (every one a direct
write that bypasses the seam), and looked at the screen before teardown:

```
21: "current model: example-model-v1 — /help <command> explains one command…"
22: "kitsu> "
23: "╰ model example-model-v1 │ cwd ~                                    ╯"
24: "kitsu"            <- the status row the bridge painted, intact
```

Row 24 survived, and the test asserts at least twenty non-blank rows above it
so the assertion cannot pass vacuously. **A writer that bypasses the seam does
not corrupt the viewport; it flows into the region harmlessly.** The mechanism:
a direct write inherits the terminal's cursor, the cursor is parked inside the
DECSTBM region, and the terminal scrolls that region on a line feed regardless
of who wrote the bytes. Bypassing the seam costs the viewport's CRLF and any
future suspend/repaint coordination — not pixel correctness.

So routing hundreds of writers is **not** required for idle slash output, which
removes the most expensive branch Q3 threatened.

**The condition, and it matters:** this was measured at idle **in cooked mode**.
Nothing has entered raw mode for the session yet — rustyline enters it only
while reading a line — so `ONLCR` is still supplying the carriage return those
392 writers omit. Once T0-S5d owns stdin in raw mode for the whole session they
lose it and will staircase, exactly as F14 describes. **The harness must be
re-run after the input path lands**, and the answer above must not be quoted as
unconditional.


## F18 — inline teardown prints the closing summary after releasing the region

Found: T0-S5b, by the harness, in code accepted one card earlier. Deterministic,
not a race. Tail of a real session:

```
"kitsu> /quit\r\x1b[12C\x1b[?2004l\r\n"   ESC[r   "  ↳ completed in 25 ms · … · succeeded"
```

The session's closing summary is printed **after** `BridgeGuard::drop` released
the scroll region. Resetting DECSTBM margins homes the cursor, so that line
lands on **row 1 and overwrites the top of the transcript**: the exit screen
reads `1: "  ↳ completed in 25 ms · …"` with surviving debris from the
overwritten row beneath it. Verified against the prefix parse — the cursor is on
row 12 immediately before the release and the line renders at rows 1 and 2.

A second, smaller defect in the same place: `hide()` releases the region without
clearing the viewport rows, so the stale status text is still on row 24 after
exit.

Two fixes owed, both T0-S5c's:
1. the bridge guard must outlive the final summary line, which is an ordering
   question in `run_chat` rather than a change to the guard;
2. `hide()` should clear the viewport rows and leave the cursor somewhere sane.

T0-S5b deliberately **pins the broken behaviour** in a characterisation test —
release precedes the line, the line lands on row 1, the status row goes stale —
so the defect cannot change silently. T0-S5c inverts that test, which is what
makes it the failing test for the fix.

## F19 — `show` emits an invalid scroll region on a terminal shorter than its viewport

Found: T0-S5c, by the test author, while establishing what `hide` may assume
about geometry. **Pre-existing**, in `InlineSurface::show` since T0-S4.

`show` computes its bottom row as `rows.saturating_sub(viewport_rows)`. When the
terminal is no taller than the viewport, that saturates to **0** and the region
set becomes `ESC[1;0r` — an invalid DECSTBM, since the bottom margin must be
greater than the top. Terminals disagree on what to do with it; some ignore the
sequence entirely, which leaves no region at all while the code believes one is
installed.

The same clamp then puts the viewport rows at `1..=viewport_rows`, overlapping
the region's own top rows rather than sitting below them, so even a terminal
that tolerates the sequence paints the status over the transcript.

This is only reachable on a terminal of one or two rows with today's single-row
viewport, which is why it has not been seen. It becomes reachable in ordinary
use as soon as the viewport grows to the three or four rows the design's
composer calls for, and F16 already notes that resize is unhandled.

**Deliberately not fixed in T0-S5c**: the natural fix is clamping the bottom to
at least 1 and refusing to install at all below some minimum height, which
changes `show`'s emitted bytes and so breaks the tests that pin them. That is a
card, not a drive-by. The fix also needs a decision this project has not yet
made — whether an impossibly short terminal should fall back to the classic
renderer or run with no status row — so it is recorded here rather than guessed
at. T0-S5c's `hide` is correct for every geometry `show` can currently produce,
including the degenerate one.

## F20 — the inline idle prompt loses rustyline's history and completion

Found: T0-S5d, while scoping the idle input path. Not a defect in new code — a
consequence of the design, recorded so it is a known cost rather than a
surprise.

Inline mode replaces rustyline at the idle prompt with a raw-byte reader
feeding `kitsu_editor::TypeaheadEditor`, because the viewport owns the bottom
rows and rustyline would draw its own prompt line inside the scroll region
with no knowledge of the region at all.

What the editor already provides: insertion, cursor motion, Home/End,
Backspace/Delete, the Ctrl+U/K/W kills, Up/Down history over a list it is
handed, bracketed paste as a single draft, and Ctrl+D EOF on an empty draft.

What is lost at the idle prompt in inline mode:
- **persistent history across sessions** — the editor is handed a history
  list, but nothing yet seeds it from, or appends it to, rustyline's history
  file, so Up/Down sees only what the current session typed;
- **completion** — rustyline's slash-command and path completion has no
  equivalent in the editor's keymap, so Tab does nothing;
- **reverse search** (Ctrl+R) and the rest of rustyline's emacs bindings
  beyond the subset the editor implements;
- **the live slash-command palette** — and this is the big one, which I
  undersold when first writing this finding. T0-S5d's pseudo-terminal
  transcript shows that typing `/cont` at the idle prompt opens a bordered
  palette, filtered as you type, with `type to filter · Tab to complete`, up
  to a dozen rows of it. That is a rustyline *helper*, so it goes with
  rustyline. It is the most visible thing a user loses, far more so than
  history;
- **the composer footer** (`╰ model … │ cwd … ╯`), likewise a helper.

A geometry consequence that follows from the above: the classic composer is
already **two** rows (prompt plus footer) and the palette up to twelve, while
the bridge installs the viewport with `viewport_rows = 1`. Restoring the
palette therefore cannot be done without raising the viewport row count and
re-pinning the region arithmetic, and the existing tests derive their expected
bottom row as `height - 1`, so they change with it. That makes the palette and
the multi-line composer **one** card rather than two, and it interacts with
F16 (resize unhandled) and F19 (short-terminal clamp).

This is acceptable for a renderer gated behind `KITSU_TUI=inline` and
documented as a preview, and it is exactly the sort of regression that must not
reach a default. **Inline mode cannot become the default until history
persistence and completion are answered**, which is a milestone exit condition,
not a detail. The design's composer requirement already implies both.

Carded rather than fixed here because each is independently sizeable: seeding
and appending the history file is a small card, while completion needs a
decision about whether the editor grows a completion hook or the binary drives
it around the editor.

## F21 — the inline reader swallows Ctrl+Z and Ctrl+backslash

Found: T0-S5d, by the test author, as a gap it deliberately did not pin.

`RawStdinGuard` clears `ISIG`, so the suspend and quit characters arrive as
bytes rather than signals. The during-turn collector handles this by
re-raising the signal the kernel would have sent, and the idle reader does the
same for Ctrl+C — but `InlineRead` has no variant for suspend or quit, so the
editor sees them as unhandled bytes and the reader drops them.

The user-visible effect: **Ctrl+Z does not suspend kitsu** at an inline idle
prompt. It is not a corruption and nothing is lost, but it breaks a shell
convention every terminal program is expected to honour, and silently.

Deliberately not fixed in T0-S5d, which was already the largest card in the
milestone. The fix is small but not free: `InlineRead` grows `Suspend` (and
probably `Quit`), the caller re-raises `SIGTSTP`, **and** the viewport must
release its scroll region before the process stops and reinstall it on
`SIGCONT` — otherwise the shell inherits a terminal with a region still set,
which is the one genuinely user-hostile outcome here. That resume path is the
real work, and it belongs with the other signal handling rather than bolted
onto a reader.


## F14 — resolved: raw INPUT mode does not staircase, because output flags stay on

F14 warned that a raw-mode backend must emit CRLF, since raw mode clears
`ONLCR` and a lone line feed then moves down without returning to column 1.
That drove `InlineSurface::transcript_line` to emit `\r\n` explicitly, and it
left the open question of what would happen to the binary's several hundred
direct `println!` writers once a session held raw mode.

**Resolved in T0-S5d, and the premise was too broad.** There are two different
raw modes in this tree:

- `crossterm::terminal::enable_raw_mode`, used by `TuiRuntime`, goes through
  `cfmakeraw` and **does** clear `OPOST`/`ONLCR`. F14 is correct about this one.
- `RawStdinGuard`, the live row's own, modifies **only `c_lflag`**
  (`ICANON|ECHO|ISIG`) and leaves `c_oflag` entirely alone. `OPOST`/`ONLCR`
  stay on, so the terminal keeps turning a line feed into a carriage-return
  line feed.

The inline idle reader reuses `RawStdinGuard`, so **nothing staircases and no
writer needs routing**. Measured rather than assumed: on a real
pseudo-terminal, with the guard demonstrably engaged (the bracketed-paste
toggles bracket the lines in question), the child emits `\n` and the master
reads `\r\n`; every report line of a command run on the raw idle path starts
at column 1. A regression test holds this, and its precondition fails if
Rustyline is still running the prompt, so it cannot silently degrade into
re-measuring the cooked path.

This also completes the Q3 answer that T0-S5b could only give for cooked mode.
The remaining exposure is narrow and worth stating: anything that reaches for
crossterm's raw mode on the binary's path would clear `OPOST` and break every
one of those writers at once. `transcript_line` keeps its explicit CRLF, which
is harmless when `ONLCR` is on (a redundant carriage return) and correct if a
future path does clear `OPOST`.
