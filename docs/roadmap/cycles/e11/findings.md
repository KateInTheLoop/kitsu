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

### Truncation: half of it was closed in T0-S9, and the note above is now stale

The claim that truncation "needs a `cols` input the current API does not take"
was true when written and is only **partly** true now. T0-S9 gave the bridge
`live::compose_input_row(draft, cols)` and `live::terminal_columns()`, so the
**draft row** is composed through the classic row's own width fitting — the
same windowing, ellipsis and hint-reservation logic, not a reimplementation.
That row can no longer overflow.

What remains open is `InlineSurface::paint` itself, which still emits `ESC[2K`
plus the row text with **no width limit**. Any caller handing it a long row —
the status string today, palette rows tomorrow — can still wrap into the next
viewport row and corrupt it. So the precedent to reuse already exists and is
already wired for one row; extending it to `paint` is the remaining work.

**RESOLVED in T0-S13**, which closes F16 entirely. `kitsu-render` gained
`fit_row_to_width(row, cols)` — escape-aware, beside the width primitives it
reuses — and `paint` takes `cols` and applies it per row. The contract is six
numbered rules in application order, and the ordering is itself pinned:

1. **Byte-identity first.** A row whose visible width fits is returned
   byte-identical, with no allocation path reachable — which is what keeps every
   exact-byte assertion in `paint` true, and the output-tier discipline intact.
2. Escapes cost zero columns, so a heavily styled row is not truncated because
   its escape bytes inflated a byte count.
3. Visible columns are counted with the existing `char_display_width`, so a
   wide character costs two.
4. A wide character is never split: the fit stops before it and the leftover
   column stays **unpadded**, since `paint` has already cleared the row and a
   pad would advance the cursor past the text.
5. Exactly one reset is appended, and only when the row was **both** truncated
   **and** carried SGR. The predicate is SGR-specific rather than any-escape:
   only SGR leaves colour or attributes behind, so a reset after a cursor move
   or an OSC sequence would be bytes emitted for no reason into a stream other
   tests assert exactly.
6. Zero columns yields nothing for a row of visible width above zero, while a
   zero-width row satisfies rule 1 first and returns identical.

`cols` is a parameter rather than latched at `show`, for two stated reasons: a
second terminal probe inside `kitsu-tui` would be a second source of truth, and
a latched width would go stale after a resize exactly as the row geometry did.

The scanner is local to `fit_row_to_width` rather than reused from
`TerminalTextSanitizer`, deliberately: that type is a char-at-a-time *stripping*
machine with no notion of sequence boundaries or SGR-ness, so getting offsets
out of it would mean restructuring the code the byte-identity invariant runs
through. The helper mirrors its byte classes so the two cannot drift silently.

### Resize decision, taken in T0-S11

What happens when a resize crosses F19's `rows >= viewport_rows + 2` bound was
undecided, and a resize can cross it in either direction. The decision:

- **Below the bound, the viewport releases** — region reset, rows cleared,
  nothing pinned — and stays released while the terminal is too short.
- **Back above the bound, it re-installs.**

Rationale: a pinned row that cannot be drawn correctly is worse than no pinned
row, and the alternative — keeping a region whose margins no longer fit the
screen — is precisely the corruption F19 exists to prevent. The tall → short →
tall round trip must therefore end with a correctly installed viewport, and the
short interval must emit a real release rather than leaving a stale region.

Detection is `SIGWINCH` with a handler that does nothing but store into an
`AtomicBool`, which is the only async-signal-safe action available to it. The
reader already retries on `EINTR`, so a signal interrupting the blocking read
is the natural wake-up: on waking it checks the flag, re-latches, and repaints.
Re-latching re-sets the region rather than patching it, because DECSTBM margins
are absolute row numbers.

### RESOLVED in T0-S11 — the resize half

`plan_resize(installed, rows, viewport_rows)` is the pure decision table
(`Unchanged` / `Relatch` / `Release` / `Install`), pinned by a property test
over `rows 0..=40 × viewport_rows 0..=8` asserting that a plan pins a geometry
**iff** `show` would emit one for it, so the plan and F19's bound cannot drift
apart. `BridgeGuard::resize` applies it as a single hide-then-show pair at the
new dimensions — **not** four branches — because `hide` is inert without a
latched geometry and `show` is inert below the bound, so every arm falls out of
that one pair. It emits no paint: a resize happens mid-read, so the row's
content is the live draft and the reader draws it immediately after.

The truncation half of F16 remains open for `paint` in general (see above).

**The one property no in-repo test reaches is recorded separately as F24**: the
handler must be installed via `sigaction` with `sa_flags` omitting
`SA_RESTART`, or the interrupted read restarts and the loop never wakes.

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
at.

### Worse than first recorded: it eats transcript rows

Established in T0-S6 while writing the tests. The original note treated this as
emitting a sequence terminals ignore. That is only true of `show`. The other two
entry points are actively destructive on the same geometry:

- **`paint` overwrites the transcript.** With `bottom = 0` it positions to
  `ESC[1;1H`, clears with `ESC[2K`, and writes the status row **on top of
  transcript row 1**. Every repaint eats that row again.
- **`hide` parks the cursor on row 1**, so the closing run summary lands on the
  top of the transcript — the exact failure mode F18 was fixed to prevent,
  reached again through a different door.

So a short terminal does not merely fail to pin a viewport; it corrupts the
session's output. That is user-visible, which raises this from a tidiness item
to a defect worth fixing before the milestone closes.

### The bound, and the off-by-one I had to be corrected on

My first instinct was `rows >= viewport_rows + 1`, reasoning that the region
needs at least one scrolling row. **That is wrong**, and the test author
refused to pin it without asking. DECSTBM requires the bottom margin to
*exceed* the top, so with a top of 1 the bottom must be at least 2:

```
bottom = rows - viewport_rows >= 2   =>   rows >= viewport_rows + 2
```

At `+ 1` the region is `ESC[1;1r`, a single-row region that VT-series terminals
ignore for exactly the reason they ignore `ESC[1;0r`. The guard would then have
admitted a geometry the terminal discards — with the surface already swapped and
the classic live row already stood down, so nothing is pinned and the fallback
never happens. **A guard that admits an ignored geometry is worse than no
guard.**

### Resolution

Fixed in T0-S6 with the bound above, enforced in **both** places:

- **`install` is the policy gate**, because it is the only place that can
  decline *and* leave the live status row enabled and the surface slot
  untouched — a declined install must leave the process exactly as it found it,
  so the classic renderer takes over cleanly.
- **`show` is defensive**, because it is what emits the bytes and is `pub` on a
  library type with other potential callers, including a future resize path. Its
  defensive behaviour is to write nothing and latch nothing, which is the only
  choice that makes `paint` and `hide` fall onto their existing "no geometry"
  paths without introducing new state.

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
- ~~**persistent history across sessions**~~ — **this was false, see the
  correction below**;
- **completion** — rustyline's slash-command and path completion has no
  equivalent in the editor's keymap, so Tab does nothing;
- ~~**reverse search** (Ctrl+R)~~ — **this was false, see the correction
  below**;
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

### Scope decision: Ctrl+Z is handled, Ctrl+backslash stays inert

Taken in T0-S7, and deliberate rather than an oversight.

**Ctrl+Z is handled**, because suspending is a convention every terminal
program is expected to honour and because the naive version of it is actively
harmful: re-raising the signal with the scroll region still set hands the shell
back a terminal whose scrolling area is a subset of the screen. Releasing the
region and restoring the terminal modes *before* the process stops is most of
the work, and doing it in the wrong order is worse than not doing it.

**Ctrl+backslash stays inert.** The conventional action is to re-raise
`SIGQUIT`, whose default disposition terminates the process **and dumps core**.
This process holds API tokens and conversation text in memory, so honouring
that convention would write credentials and user content to disk because of a
single mistyped keystroke — a worse outcome, and a quieter one, than a key that
does nothing. Programs in this position commonly suppress core dumps for
exactly this reason.

That leaves the key doing nothing, which is a small deviation from convention
taken knowingly: `/quit`, Ctrl+D and Ctrl+C all remain available, so nothing is
unreachable. If this is ever revisited, the right shape is to disable core
dumps for the process (`RLIMIT_CORE`) *and then* honour the signal, not to
honour it as-is.

### Resolved in T0-S7

Ctrl+Z now suspends. The editor already emitted a distinct action for it
(`EditorAction::Suspend`), so no change was needed there — the reader was
simply swallowing it. The reader now returns `Suspended(draft)` **at that
keystroke**, carrying the draft and leaving the signal and the terminal to the
caller; a test feeds bytes after the Ctrl+Z and requires they belong to the
*resumed* read rather than this one.

The caller releases the viewport, restores terminal modes, stops, re-engages
and reinstalls, then re-enters the reader with the preserved draft through the
same replay path that carries a draft between prompts — so typing survives the
cycle.

**The ordering is the correctness property, so it is enforced behind a seam**
(`SuspendEffects` / `suspend_and_resume`) and pinned by a test with a recording
fake. The implementer flagged, correctly, that the seam initially had no test
using it and was therefore pure indirection; that gap was mine for authorising
it mid-card, and it is now closed. The test was verified to fail when the stop
is moved ahead of the release, which is the mutation that matters: a stopped
process cannot clean up after itself, so no later code can rescue a wrong
order. A second test pins that a failed re-engage stops short of reinstalling,
rather than pinning a row nothing can type into.

Still uncovered end-to-end: see **F22**.


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

## F22 — the suspend/resume cycle has no end-to-end test, and why it was deferred

Found: T0-S7. Not a defect — a deliberate coverage gap, recorded so the next
card inherits the research rather than repeating it.

Suspend is unit-tested at both ends: the reader returns `Suspended` carrying
the draft, the viewport's release and reinstall are byte-pinned and proven to
round-trip against an independently installed guard, and the call order is
pinned through the `SuspendEffects` seam. What is **not** covered is the real
cycle on a real terminal: Ctrl+Z at an inline prompt, the process actually
stopping, `fg`, and the screen recovering.

The test author probed the mechanism before declining it, and both findings are
worth keeping:

- **The stop/continue mechanics do work here.** A child sharing the test
  runner's process group raises `SIGTSTP`, reaches state `T`, and resumes on
  `SIGCONT`; the orphaned-process-group discard rule does not bite in this
  environment. So the idea is not blocked by process-group semantics.
- **The existing harness tolerates a stopped child but cannot detect one.**
  `run_styled_chat_pty`'s loop calls `try_wait`, which is
  `waitpid(WNOHANG)` **without `WUNTRACED`**, so a stopped child simply does
  not report and the loop keeps polling until it hits the 20-second deadline.
  Detecting the stop — and then sending `SIGCONT` — requires
  `waitpid(WNOHANG | WUNTRACED)`.

Two reasons it was right to defer rather than write it:

1. **Its stability could not be measured.** Before the feature existed the only
   observable run was the failing one, and that failure is a deadline timeout —
   exactly the shape that becomes an intermittent once the happy path works.
   Shipping it unmeasured would have been shipping a coin flip.
2. **It could not be built without changing shared test code.** The `WUNTRACED`
   change lands in the harness all six pseudo-terminal tests use. The
   alternative, duplicating ~120 lines of terminal setup, creates a second
   harness that drifts from the first.

**Next card:** now that the implementation exists, add it and measure it —
several consecutive runs, and dropped if it cannot be made deterministic. Until
then the gap is covered by the human terminal matrix, which should include
Ctrl+Z at an idle prompt followed by `fg`, confirming both the screen and the
shell's own scrolling area afterwards.


## F20 — correction: two of the claimed losses were never real

Established in T0-S8 by testing rather than reading. **I wrote two false claims
into this finding and then repeated them in `README.md`, `CHANGELOG.md`,
`docs/ux-design.md`, the T0-S6 go/no-go record and a production doc comment.**
Telling users a feature is missing when it works is a defect in its own right,
so the correction is recorded here in full rather than quietly edited in.

**Cross-session history works.** Inline mode borrows rustyline's persistence
wholesale while never running its composer: `main.rs:17033` loads the history
file at startup (gated on `!no_session`), `read_idle_chat_line` hands the inline
reader exactly that list, `add_history_entry` at `:17174` sits on the **common
path** after the read — outside the match on which reader produced the line — and
`:18010` saves on exit. Proven end to end: two real sessions sharing one `HOME`,
with the test also reading `$HOME/.kitsu/history` directly as independent
evidence the save ran.

A detail worth keeping, because it reads as a bug until explained: the first
version of that test pressed Up **once** and recalled `/quit`, because
`add_history_entry` is on the common path and therefore records `/quit` too. A
genuinely empty history would have had nothing to paint at all, so that failure
was itself proof the walk was reading the saved file.

**Reverse search works, but not its status line.** `TypeaheadEditor` resolves
the whole of search mode internally and emits no `EditorAction` for any of it,
so the reader has nothing to swallow — unlike `CompleteRequest`, which it
explicitly drops. Ctrl+R narrows, a second Ctrl+R cycles, Ctrl+G restores the
prior draft, and an accepted match is ordinary editable text. What is missing is
the `⌕ reverse-search '…'` indicator.

### A second correction: the remaining losses are not all one cause

**The first version of this section was also wrong**, and I caught it while
scoping the follow-up card rather than after publishing it. It claimed Tab
completion, the palette and the reverse-search indicator shared one cause and
one fix. They do not.

What the during-turn row's **hint slot** actually carries is two things, both
one line, both sharing the draft's row:
- the **reverse-search status**, reserved, claiming its columns before the
  draft is windowed;
- the **inline slash-completion suffix** — `typeahead_slash_hint("/s")` returns
  `"teer"`, the remainder of the best match, rendered dimmed after the draft and
  dropped when the row is narrow.

Widening the inline paint seam therefore restores **those two**, and that is
T0-S9.

The other two are separate — but they are **very** different sizes, which I
also got wrong at first by assuming "completion" meant one thing:

- **Tab completion, cheap tier: DONE in T0-S10.** Tab now accepts the showing
  slash hint at the inline prompt, which is parity with the during-turn row.
  Two things made it more than a transplant. The suffix must be recomputed from
  the draft rather than read back out of the hint slot, because the slot holds
  the *search status* while Ctrl+R is armed — appending that would type
  `⌕ reverse-search '…'` into the prompt, and it is reachable rather than
  theoretical, since actions are handled after a whole read completes and a
  later Ctrl+R in the same read can arm the status first. The test author
  proved the trap bites by implementing the naive version and watching the
  draft become `"deploy the thing  ⌕ reverse-search 'dep'"`. And no
  special-case for search was needed: the editor treats any control key in
  search mode as accept-exit-refeed, so by the time Tab is seen the draft is
  the matched history line and no slash hint applies — readline's own
  "accept and act". My instinct had been to pin "Tab does nothing mid-search",
  which would have contradicted the editor's rule.

  What remains is only the **lists**: `@file` path completion and the bordered
  palette. Original note follows.

- **Tab completion has two tiers, and the cheap one is nearly free.** The
  during-turn collector's entire Tab handling is seven lines
  (`main.rs:14530-14538`): if an inline slash hint is showing, append it to the
  buffer. Reaching parity with the during-turn row at the inline prompt is
  therefore a *small* card, and a natural immediate follow-up to T0-S9 since
  the hint is now computed there anyway. What is genuinely larger is
  rustyline-level completion **lists** — `@file` path completion and the
  bordered palette — which is the next bullet's problem.
- **The bordered palette** (`╭ slash commands ╮`, filtered as you type, up to a
  dozen rows) is a rustyline *helper* that draws over several rows. It needs a
  viewport taller than one row, so it belongs with the multi-row composer and
  interacts with F16 (resize) and F19 (short-terminal clamp).

**Lesson, since this is the second over-claim in the same area:** "these share a
cause" is a claim about code, and I made it twice from reading a summary rather
than the call sites. Both times the correction came from reading what a function
actually returns — `typeahead_slash_hint` returning `"teer"` is what settled it
here.


## F22 — resolved: the suspend cycle now has end-to-end coverage

Closed in T0-S8, deliberately on the second attempt rather than the first. Both
reasons for deferring it had expired: the feature existed, so stability could be
measured, and the shared harness was open to change.

The test types a draft, sends Ctrl+Z, and from **outside** the process observes
that the child really stops (`WIFSTOPPED`, answered with one `SIGCONT`), that
the scroll region is released before the stop and set again after it, and that
the draft survives — a bare carriage return written *while the child was
stopped* submits the replayed draft.

Two environmental hazards had to be found and handled, and both generalise to
any future test of this kind:

- **An inherited `SIGTSTP` disposition silently defeats it.** `SIG_IGN`
  survives fork and exec, and a non-interactive shell ignores job-control
  signals in a forked subshell — `/proc/self/status` showed `SigIgn` bits
  20-22 set inside a command substitution but clear under a plain redirect. The
  child inherited that, so raising the stop signal was a no-op and the test
  failed 5/5 under one invocation while passing 5/5 under another. The harness
  now resets the disposition to `SIG_DFL` in `pre_exec` and calls
  `setpgid(0,0)`, since POSIX discards the stop signal in an orphaned process
  group.
- **`waitpid` needs `WUNTRACED`** to see a stopped child at all; the previous
  card had already identified this, and it is why the harness could tolerate a
  stop without detecting one.

Evidence: 40 consecutive full-suite runs in debug and 5 in release, plus 5/5
under the invocation that had failed 5/5 before the fix.


## F23 — a pseudo-terminal test was failing ~7% of the time on this branch

Found in T0-S8, in a test I committed two cards earlier, and measured properly:
on the **unmodified baseline** with the new work stashed,
`inline_mode_echoes_the_idle_draft_in_the_viewport_row` failed **2 of 30**
full-suite runs.

**Cause: a race on the very first keystroke.** The scripts triggered their first
input on the banner, which can arrive while the terminal is still canonical with
echo and signal generation on. In that window the terminal echoes the typed
characters itself, *inside* the scroll region — which is precisely what that
test asserts must not happen — and a control byte like Ctrl+Z is swallowed by
the terminal as its own suspend character rather than reaching the editor.

**Fix:** every script that types at the idle prompt now triggers on the
bracketed-paste enable sequence, which `RawStdinGuard::engage` writes
immediately after its `tcsetattr`, making it proof that raw mode is already on.
Measured 0 failures in 40 full-suite runs afterwards.

**Why this is worth a finding rather than a quiet fix.** First, it was a real
intermittent on this branch, not a hypothetical. Second, the lesson generalises:
*any* pseudo-terminal test that sends input must synchronise on evidence that
the program has taken the terminal, not on the program having merely printed
something. Triggering on output proves the program is alive, not that it is
ready. Third, it is a reminder that a green suite is not evidence of a
deterministic suite — this one had been green every time I ran it, and was
nonetheless failing one run in fifteen.

## F24 — `SA_RESTART` would make resize handling silently dead, and no in-repo test can catch it

Found: T0-S11, by the test author, as the limit of what its own tests reach.

The resize path depends on a `SIGWINCH` arriving while the reader blocks in
`read()` and surfacing as `EINTR`, so the loop wakes and checks the pending
flag. **Whether that happens depends entirely on how the handler is
installed:**

- `libc::signal` on Linux/glibc implies **`SA_RESTART`**, which restarts the
  interrupted read. The flag is set and the loop never wakes to look at it.
- `sigaction` with `sa_flags` that omit `SA_RESTART` returns `EINTR` and the
  loop wakes.

**The failure mode is the dangerous kind: invisible.** Every unit test passes
either way, because a test raises the signal from the same thread and never
blocks in `read`. The feature would look complete, ship, and do nothing — a
resized window would leave the viewport stranded exactly as F16 describes,
with no error anywhere.

So the correct installation is load-bearing and is recorded in a comment at the
call site, specifically so a later "simplification" to `libc::signal` is
recognisable as a regression rather than tidying.

**Correction, made while scoping T0-S12: "only a human can catch this" was
wrong.** A *reliable* pseudo-terminal test does catch it, and my reasoning for
saying otherwise does not hold up. The harness signals a **separate process**
that is genuinely blocked in a real `read` on a real tty — so with `SA_RESTART`
set the child would not wake, the viewport would not move, and the test would
fail. The property is only untestable *in-process*, where a raise from the same
thread never blocks in `read`; that is a limitation of unit tests, not of
testing.

The decisive design constraint, which is what makes such a test worth anything:
**no input may follow the resize before the assertion.** A keystroke after the
resize wakes the blocking read by itself, the flag is then checked, and the
viewport resizes *even with `SA_RESTART` set* — so a test that types afterwards
passes under both installations and establishes nothing.

**T0-S12 shipped that test, and proved it discriminates rather than asserting
it:** with `sa_flags` temporarily set to `SA_RESTART` all three resize tests
failed on the deadline; restored, all three pass. The stronger form was
achieved too — the gate on sending the next byte *is* the new geometry
appearing in the stream, so if the flag were only checked on the next keystroke
there would be no next keystroke and the test times out with the transcript
attached.

So this class of regression is now caught in CI. The human check below stays on
the matrix regardless, since a real terminal also exercises the window manager,
the multiplexer and fonts, none of which a pty models.

**The human check**, on the terminal matrix: resize a real window mid-draft and
confirm the pinned row follows. It was the second item there that could not be
settled from inside this repository, after Q1 — now partly reclaimed.

Related, and the reason this class of bug is worth a finding of its own:
`note_resize` may do nothing but store into an atomic, because a signal handler
runs in a context where allocation, locking and printing are all unsafe. A
handler that "just logs" would be a far more spectacular version of the same
mistake.

## F25 — the pseudo-terminal resize test was deferred, and why five runs could not settle it

Found: T0-S11. Recorded so the next card inherits the reasoning rather than
re-deriving it.

The standing rule for a new pseudo-terminal test is five consecutive green runs
or drop it. The test author could not meet that bar **and explained why the bar
was unmeetable rather than gaming it**: the implementation did not exist yet, so
the test had to fail by design, and five runs of a designed failure cannot
distinguish a flake from the intended red. The evidence the rule asks for was
unobtainable at that moment.

Two further costs it identified:

- `Step` has no action that sends no bytes, so a resize step does not fit the
  existing script shape;
- `Session`'s emulator is fixed at the harness's `ROWS × COLS`, so a resized
  screen cannot be modelled by any existing accessor. Both would change for
  every existing script.

And the one property only a real terminal adds — that `SIGWINCH` surfaces as
`EINTR` rather than restarting the read — is a property of **how the handler is
installed** (F24), which an intermittent test would not establish either.

**RESOLVED in T0-S12**, and it was worth the harness cost. Three tests, 12 in
the suite, five consecutive green runs in each profile, verified independently.
The harness gained a `Step` action that sends no bytes, a `set_window_size`
helper, and — the part that was genuinely necessary — a `Session` that replays
the byte stream in segments and calls `vt100::Parser::set_size` at exactly the
offset where the child was resized. Without that split every post-resize
absolute address is modelled against the old grid and silently discarded.

The decisive outcome: **F24 is now caught in CI rather than by a human.** See
F24's correction.


## F26 — a `SIGWINCH` arriving mid-paint is latched but not acted on until the next wake

Found: T0-S12, disclosed by the test author rather than hidden behind the
harness accommodation it required. **Real, narrow, and currently untested.**

The resize path's delivery mechanism is a signal interrupting the reader's
blocking `read`. If the signal instead lands while the reader is **mid-paint** —
between waking and blocking again — the flag latches, the reader then blocks,
and nothing wakes it. The pending resize sits unapplied until the next event:
another signal, or a keystroke.

It self-corrects on any later event, and a real window drag emits many signals,
so the realistic exposure is small. But it is genuine, and it is the one thing
a human dragging a window *slowly* might still see — a viewport that catches up
one event late.

**Why this is recorded rather than fixed:** the honest fix is not a bigger
handler (it may do nothing but an atomic store) but checking the flag at more
points, or a self-pipe so the wake is a readable descriptor rather than an
`EINTR`. That is a design change to the read loop, not a patch, and the current
behaviour is a late update rather than a wrong one.

**Why it must not be "cleaned up" out of the test harness:** the pseudo-terminal
resize steps re-deliver the signal every 100 ms while the next step's trigger is
outstanding, precisely because of this race — the trigger that fires a resize is
the child's own paint output, so the child may still be inside that paint when
the signal lands. Removing the re-arm reintroduces a narrow but real flake.
Re-delivery is safe because a resize to unchanged dimensions is `Unchanged` and
emits zero bytes, and it does **not** weaken the `SA_RESTART` discrimination,
because a resize step writes no input byte.

## The composer and palette are three cards, in a forced order

Recorded in T0-S13 so the sequence is not rediscovered. R4's remaining gap —
Rustyline's completion lists, meaning `@file` paths and the bordered
slash-command palette — cannot be done as one card, and the order is forced by
a dependency rather than by preference:

1. **T0-S13, escape-aware row fitting for `paint`** (F16's truncation half).
   This must come first. With a single viewport row an over-long row merely
   wraps untidily; with several rows **it wraps into and overwrites the row
   below**. Building a multi-row composer on an unbounded `paint` would mean
   debugging corruption that has nothing to do with the composer.
2. **T0-S14, a variable viewport height.** The bridge installs with
   `viewport_rows = 1` hardcoded. A palette that grows and shrinks as you type
   needs the height to change at runtime, which means re-latching the region
   on every change — the same operation T0-S11's resize already does, so this
   is more plumbing than new mechanism. It also interacts with F19's minimum
   height: a palette that would not fit must be truncated or suppressed rather
   than pushing the region to an invalid geometry.
3. **T0-S15, the palette itself** — matching, filtering as you type, the
   border, and `Tab` semantics against a list rather than a single hint. Split
   again on contact: **S15a** the rows as data, **S16** the wiring (named S16
   rather than S15b to match the `S16-*` references in the committed tests).

### The drop-from-the-top rule collides with a bordered box

Found while scoping T0-S15a, and it is a collision between two decisions I made
in earlier cards rather than a defect in either.

T0-S14 established that when the viewport cannot fit every requested row, the
**topmost** rows are dropped so the composer survives. That is correct for a
list. It is **wrong for a bordered box**: the top row is the box's top border
and its ` slash commands ` label, so dropping it leaves a frame with no lid —
and dropping two leaves a stray `│` column with no context at all.

The resolution is not to change the rule, which is right for the general case,
but to **build the palette to the height it will be granted** rather than
building it at full size and letting the clamp cut it. Hence
`granted_rows(requested)` as a query the caller consults *before* laying out —
which is exactly the shape recorded above, and arrived at twice from different
directions: once to let a palette mark its hidden entries without a repaint
flicker, and once here to keep its frame intact.

A consequence for the "N more" row: the existing `slash_palette` already emits
one when matches exceed `SLASH_PALETTE_ROWS`, but the **height** can hide
entries too. A count that reflects only the row cap would under-report whenever
the window is short, which is the kind of wrong that looks right — so it is
pinned against a prefix matching many commands at a small height.

Worth stating plainly, because it is the reason this card is small: the classic
palette renderer already does matching, both frame styles, the row cap, the
"N more" row, the no-match case and per-row width fitting with ellipsis. The
inline palette **reuses** it. Two palettes that drifted would be worse than one
palette with an awkward call site.

### T0-S14's two decisions, taken rather than deferred

**A requested height too tall for the window is clamped, not refused.** F19's
bound still rules — the region needs two scrolling rows above the viewport — so
the usable maximum is `terminal_rows - 2`. Refusing is the right answer only
when *not even one row* can be pinned, which is T0-S11's release case and is
already built. When some rows fit, a palette too tall for the window should be
shortened rather than making the whole viewport vanish, because the composer
lives in that viewport.

**When rows must be dropped, the topmost ones go.** The composer is the bottom
row and the palette sits above it, so dropping from the top keeps the row the
user is actually typing into. Losing the thing you are typing into to make room
for a list of suggestions would be the wrong trade, and it is the kind of
default that is easy to get backwards and hard to notice in a test that only
counts rows — so it is pinned by asserting the surviving rows are the **last**
of the requested set, in order.

A consequence worth stating: the row count painted must always equal the latched
height. Painting more rows than the viewport is tall would put the extras inside
the scroll region, where they scroll away with the transcript — which looks like
a disappearing palette rather than an off-by-one.

### Two things T0-S14 deliberately left to T0-S15, with the answers

**1. Who learns the granted height.** `paint_rows` returns only whether a
viewport is pinned, so a caller cannot find out *before* painting how many rows
it was granted. The palette will need that: when rows are dropped it should
mark its top visible row ("N more") rather than silently truncate, and it has
to know the count while laying out, not after.

The test author offered two shapes and declined to choose. **Decided: add
`BridgeGuard::granted_rows(requested) -> u16`**, delegating to the same pure
`clamp_viewport_rows`, and leave `paint_rows`'s return as it is. Reasoning:
changing `paint_rows` to return the height tells the caller only *after* it has
painted, so a palette wanting a marker would paint, discover it was clamped,
and repaint — a visible flicker. A query answered before painting avoids that.
And routing the query through the guard rather than letting the caller call the
pure clamp itself keeps one source of truth for the terminal height, which is
the whole reason `paint_rows` is a single operation.

**1a. My "by construction" shape was wrong, and the implementer was right to
refuse it.** I specified that `paint_draft` should route through `paint_rows`,
so that the painted row count could not disagree with the latched height. Two
**pre-existing** tests forbid it, and I verified both rather than taking the
report:

- one installs a 3-row viewport, resizes to `(40, 3)`, then calls `paint_draft`
  and asserts exactly a **one-row** paint into a viewport that stays **3 tall**;
- another resizes to `(2, 1)` — below the bound — then calls `paint_draft` and
  asserts **zero** bytes.

Through `paint_rows` a one-row draft clamps to height 1 and re-latches, which
directly contradicts the first, and the second would emit bytes. So the two are
genuinely different operations: **`paint_draft` draws into whatever viewport
exists, while `paint_rows` lets the content choose the height.** The
by-construction property therefore holds for every multi-row caller and only
trivially for `paint_draft`.

That is not a permanent answer. With a palette open, a keystroke must repaint
*all* the rows, not just the composer — so `paint_draft`'s single-row semantics
becomes wrong exactly when the palette lands, and reconciling them is T0-S15's
job. It is the same shape of question as the hardcoded `1` below.

**1b. A bug my specified shape would have introduced.** The implementer added an
early return when the clamp is zero, which I had not asked for and which is
necessary: `resize(2, 0)` is `plan_resize(None, 2, 0)` = `Install`, and
`show(2, 0)` would latch a **full-screen** region — the clamp returning "no rows
fit" would have been turned into "the region is the whole screen". Zero means
leave the latched geometry alone, so a released viewport stays released and
emits nothing. It is also the right answer for empty content.

**2. `on_resize`'s hardcoded row count.** `main.rs` still calls
`bridge.resize(rows, 1)` when the terminal changes size. That is correct while
nothing requests more than one row, and **becomes a bug the moment the palette
does**: a terminal resize would silently reset the viewport to one row,
collapsing an open palette. The fix belongs to whichever card first requests a
taller viewport — either `resize` should keep the guard's current
`viewport_rows` when the caller does not specify one, or the caller should
re-`paint_rows` after a resize. Left unpinned on purpose, because pinning it
before a second row exists would have meant inventing the caller.

`@file` path completion is a fourth, independent piece: it shares the "lists"
label but not the geometry problem, since it can reuse whatever T0-S15 builds.

## F27 — the obvious test for viewport row overflow is vacuous

Found: T0-S13, by the test author, while trying to write the test the card was
*about*. Worth recording because the trap is not obvious and the next person to
test viewport painting will walk into it.

The natural test for "a row wider than the terminal corrupts the row below" is:
paint a two-row viewport with an over-long first row, then assert the second row
is intact. **That test passes even with no width fitting at all.** `paint`
writes each row as `ESC[{line};1H ESC[2K {row}`, so the over-long first row does
smear into the second row's cells — and then the second row's own `ESC[2K`
erases the smear before writing. Within a single full repaint the corruption is
always repaired by the very next row.

Two shapes do observe it, and both are in the suite:

- **A partial repaint.** Redrawing only the composer row — which is what a
  keystroke does — leaves the row below unrepaired, because nothing clears it.
  This is the realistic case: full repaints are the exception, per-keystroke
  redraws are the norm.
- **An overflow on the last pinned row.** There is no row below it to repair
  anything, so the overflow autowraps off the bottom of the screen and scrolls
  the transcript *and* the viewport up by a line. The symptom is not a corrupt
  row but a scrolled screen, which is worse and harder to attribute.

**The lesson generalises:** a repaint-based renderer repairs its own damage on
the next full pass, so any test that asserts a full repaint looks clean when it
is not. Tests of painting defects must use a partial repaint, or the last row,
or both. A green full-repaint test is weak evidence about a renderer of this
shape.


## F28 — an unrelated transaction test flaked once and has not reproduced

Reported by the T0-S14 implementer, recorded so the next person to see it knows
it is not new and not from the viewport work.

`session::transaction::tests::branch_transaction_retries_only_exact_verified_residue`
failed once during a parallel `cargo test -p kitsu --bins` run, and passed on
every rerun and in isolation. It touches no viewport, render or terminal code.

**I could not reproduce it:** 8 further full-suite parallel runs and 5 isolated
runs, all green. So the honest status is *seen once, unreproduced in 13
attempts* — not dismissed, not confirmed.

The implementer's guess, untested, is a `#[serial]` or global-state gap in that
module. Worth bearing in mind that F23 established this suite had a test failing
roughly 7% of the time while appearing green on every manual run, so "I ran it
and it passed" is weak evidence here. If it recurs, the cheap first step is a
repeat-run loop on that module alone rather than the whole suite.

### CONFIRMED in T0-S17, and narrowed

It recurred — a different agent hit it, and this time I reproduced it: **1
failure in 12** consecutive full-suite `cargo test -p kitsu --bins` runs, which
matches the original single sighting at roughly 8%.

Then I ran the loop this finding itself recommended, and the result is a clear
signature:

| How it was run | Failures |
|---|---|
| the test alone, 15 runs | **0** |
| its own module in parallel, 8 runs | **0** |
| the whole `--bins` suite in parallel, 12 runs | **1** |

So it is **cross-module interference**, not anything inside
`session::transaction`: some other test elsewhere in the binary mutates shared
state this one depends on. That rules out the implementer's guess of a gap
*within* the module, and it rules out the test being inherently racy.

**Next step, for whoever picks it up:** bisect by module rather than by test —
run `session::transaction` together with each other top-level module group until
the pair reproduces. The suspects are the globals this binary already serialises
elsewhere (the render surface, the live-row state, the inference carrier, the
process environment), since a test that mutates one without `#[serial]` is
exactly this shape. Note it has never been observed outside a full parallel run,
so a fix must be verified the same way — at least a dozen full-suite runs, not
an isolated one.

A second flake was reported in the same card — `paint_rows_growing_relatches_…`,
from T0-S14 — and did **not** reproduce in those 12 runs. Status: seen once,
unreproduced. Recorded here rather than given its own number precisely because
one sighting is not yet a finding.

### RECLASSIFIED in T0-S18: this was never a flaky test. It is a production bug.

**The test was a true positive all along**, and both I and an earlier agent
spent three cards treating it as noise. My own narrowing — "cross-module
interference, some other test mutates shared state" — was right in *shape* and
wrong in *kind*: the interfering writer is not a test and not shared test
state. It is **any subprocess spawn anywhere in the binary**, and the defect is
in production code.

**The mechanism.** `PhysicalFileStoreV1::acquire_writer_fence`
(`crates/kitsu/src/session/physical.rs` ~1305-1375) takes
`flock(LOCK_EX | LOCK_NB)` on `.locks/<session>.lock` and treats the **first**
`EWOULDBLOCK`/`EAGAIN` as proof the session is held elsewhere. But `flock` locks
live on the **open file description**, not the descriptor. The lock fd is
`O_CLOEXEC` — and `O_CLOEXEC` closes at `exec`, **not** at `fork`. So while any
other thread sits between `fork` and `execve`, the child holds a duplicate of
our lock fd and therefore holds the lock. A fence taken in that window fails
spuriously: no other process is contending, and the previous holder has already
closed its own fd.

Proven independently of kitsu with a standalone probe — one thread repeatedly
flocking a file nobody else touched while another spawned a trivial command:
**4973 spurious `EWOULDBLOCK` in 90533 attempts, 5.5%**.

**Why it matters outside the suite.** kitsu spawns tool subprocesses, agent
hooks and `git` on other threads as a matter of course. So a real session commit
can report `physical session '<id>' is already active in this or another
process` when nothing of the kind is true. That is a user-facing failure on a
core path, not a test artifact.

**Scale, measured rather than estimated.** Four runs of
`cargo test -p kitsu --bins -- --test-threads=256` gave **17, 16, 28 and 7**
failures, unioning to **46 distinct tests — every one in `session::*`, and none
anywhere else**. (The "~70" figure elsewhere is a union over many more runs; per
run it is a varying subset, which is why no single run looks alarming.) The test
that kept failing merely takes the fence eleven times, so it trips first.
**`#[serial]` — the fix both earlier guesses pointed at — would have fixed none
of them** and left the production bug untouched.

**There are TWO sites, not one.** `SessionLease::acquire`
(`crates/kitsu/src/session.rs` ~1406, the `flock` at ~1478) makes the identical
inference and emits `session '<id>' is already active in this or another
process`. It is the path most of the 46 actually go through. This was proven,
not assumed: with a retry added to `acquire_writer_fence` **alone**, the suite
still failed in `session::tests`. A one-site fix would have made the new
physical-fence regression test pass while leaving most of the exposure in place
— i.e. it would have looked like success.

**Why the apparently-better fix is not available.** Switching to `fcntl`
`F_SETLK` record locks would dodge fd inheritance entirely, because record locks
are **not** inherited across `fork`. But they are **per-process**, and
same-process detection here is load-bearing and tested:
`head_cas_requires_the_exact_fence_and_revision_then_advances_old_to_new`
(`physical.rs` ~3441) takes a fence and then takes it **again on the same thread
of the same process**, asserting the second fails. Record locks would let that
succeed. `F_OFD_SETLK` has the identical inheritance problem and is not an
escape. So the fix is a bounded retry, and it is a **mitigation**: under a
sustained fork storm the window may never be empty. The complete fix — not
leaking the lock fd across `fork` — would require every spawn site in the
process, including ones inside dependencies, to cooperate.

**Cost to be careful of:** roughly 40 call sites assert the *contended* case, so
each pays the whole retry budget before getting its error. A crude 100 × 1 ms
stand-in took `-p kitsu --bins` from 0.16 s to 0.78 s. The fork-to-exec window
is microseconds, so a budget of a few milliseconds total is ample, and the
exhaustion message must stay byte-identical because ~40 assertions match
`"already active"`.

**Deterministic reproduction**, verified by me at 20 failures in 20 runs:

```
cargo test --release -p kitsu --bins -- --test-threads=16 \
  session::transaction::tests::branch_transaction_retries_only_exact_verified_residue workspace
```

**Recommended fix:** retry the non-blocking `flock` a bounded number of times
with a short sleep — a fork-to-exec window is well under a millisecond — and
only report "already active" if it still fails. That keeps the non-blocking
contract and the honest contention error while removing the false positive.
Converting to a blocking `LOCK_EX` would also work but would turn genuine
contention into a hang.

**RESOLVED in T0-S18**, at both sites, with a bounded retry behind one shared
`flock_exclusive_with_fork_retry` helper whose doc comment records the mechanism,
the measured spurious rate, that it is a **mitigation not a guarantee**, and an
explicit instruction not to simplify it to a single attempt or switch to
`F_SETLK`/`F_OFD_SETLK`.

The budget was chosen **empirically, after a first attempt failed**: a flat
64 × 50 µs (3.2 ms) still left session failures at 256 threads (runs of 4, 9 and
2). The shipped budget is 32 × 50 µs then 47 × 500 µs — about 25 ms worst case —
chosen so the cheap phase clears an ordinary single fork window and the longer
phase exists only for overlapping windows from several spawning threads.

Verified independently by me rather than taken from the report: **6 runs at
`--test-threads=256`, zero failures each**, against my own before-figures of
17/16/28/7. `-p kitsu --bins` is 726/0 in both profiles. Suite wall time moved
0.19 s → 0.30 s, which is the ~40 contended-case assertions each paying the
budget — the cost of the fix landing on the tests that *want* the error.

**The lesson, and it is the sharpest in this file.** An intermittent failure is
evidence of *something*, and the cheap reading — "flaky test, needs isolation" —
was wrong three times in a row here. F23 was a real race in test
synchronisation; this is a real race in production. Neither was noise. The
pattern to distrust is not the flaky test but the explanation that lets you stop
looking at it.

## F31 — `agent_hook`'s null-stdio test reads the child's output too early

Found incidentally during T0-S18's diagnosis, in 4 failures out of 500 runs, and
**unrelated** to F28's production race.

`agent_hook::tests::fire_gives_the_child_null_stdin_stdout_and_stderr`
(`crates/kitsu/src/agent_hook.rs:650`) reads one line where it expects three: it
inspects the child's output before the child has finished writing it.
Reproducible with `--test-threads=256 agent_hook`.

Unlike F28 this **is** a test defect rather than a production one — the
production path gives the child null stdio correctly; only the test's
observation of it is racy. The fix is to wait for the child to exit before
reading, rather than to sleep or retry.

## F29 — the inline palette duplicates the classic one's frame and labels

Found while reviewing T0-S15a. **Accepted deliberately, on evidence, with the
factoring recorded rather than deferred silently.**

`palette_rows` reuses the classic renderer's *pieces* — `matching_slash_commands`,
the entry format, `fit_palette_text` and `fit_palette_text_with_padding` — but it
necessarily re-derives the row-selection step, because `slash_palette` has **no
height parameter**: its `SLASH_PALETTE_ROWS` cap is unconditional, and the whole
point of the inline palette is to be built to a granted height so its frame
survives.

That re-derivation drags along copies of the two labels (` slash commands `,
` type to filter · Tab to complete `), both frame-character sets, and the
`no matching command` text. Duplication in a renderer is precisely how this
project has been bitten before — it is the reasoning that made T0-S9 share
`compose_input_row` instead of reimplementing the row composer.

**Why accepted anyway: the drift is provably caught, not hopefully caught.** A
test asserts the inline rows equal `slash_palette`'s own output, for two widths
and both frames, at the exact height. I verified the net bites rather than
assuming it: changing the label in `slash_palette` **alone** turns that test
red. So the cost of the duplication is a double edit that fails loudly, not a
silent divergence.

**The factoring, for T0-S15b to do first**, since that card touches this code
anyway: give the classic renderer the height parameter —
`slash_palette_rows(prefix, rich, inner_width, max_rows) -> Vec<String>` — and
make `slash_palette` a thin join over it with the classic height, exactly as
`compose_input_row` is a thin wrapper over `compose_row`. Then the labels, the
frames and the selection logic exist once, the parity test becomes trivially
true, and the inline path stops carrying copies. It was not done in S15a because
that card was scoped to tests-then-implementation of the data, and refactoring
the classic Rustyline hint path mid-card would have put a shared, separately
tested surface at risk for no test-visible gain.

**RESOLVED in T0-S16, as that card's first step.** `slash_palette_rows(prefix,
rich, inner_width, max_rows)` now owns the labels, both frame sets, the entry
formatting, the row cap and the "N more" step, and `slash_palette` is a thin
join over it — exactly the `compose_input_row`/`compose_row` shape. The
implementer ran the suite between the refactor and the wiring and saw the
identical 6 + 1 failures and nothing new, which is what "behaviour-preserving"
has to mean in practice.

Verified by classifying every occurrence rather than taking the report: the
` slash commands ` label exists **once** in production, four lines into
`slash_palette_rows`; the two other non-comment hits are assertions in the
classic path's own tests, and `tui_bridge.rs` retains only a doc comment. The
inline palette now contains no palette text at all.


## F30 — the during-turn hint's priority list is reused at the idle prompt

Found in T0-S16, and the cause is **mine**: T0-S9 and T0-S10 reused
`typeahead_slash_hint` verbatim at the inline idle prompt. That function ranks
matches through `MID_TURN_HINT_PRIORITY`, whose own doc comment says
*"Commands that act on the RUNNING turn, in preference order. While a turn is
live these outrank plain registry order"*.

At an **idle** prompt no turn is running, so that ranking is wrong on its own
terms. Concretely: registry order for `s` is `save`, `sessions`, `skill`,
`skills`, `status`, but the priority list puts `steer` first — so typing `/s`
hints `teer` while a palette would lead with `save`.

The implementer hit this while wiring the palette and handled it locally, with
a third stand-down clause: the palette hides when a hint is showing that is not
the lead entry's own completion. That is defensible — the hint is the
keystroke's answer and the palette is its menu, so a menu contradicting the
answer is worse than no menu — and it is documented at the function. But it is
**symptom-level**: it suppresses a correct palette because an inappropriate
hint disagrees with it.

**The principled fix** is to rank by registry order at the idle prompt and keep
the mid-turn priority for the during-turn row, after which hint and palette
agree by construction and the third clause can go. It is a real card, not a
drive-by: it changes what `/s` hints at idle (user-visible), and it would turn
the `/s` case in T0-S16's regression test from one row into a palette, so that
test changes with it.

**RESOLVED in T0-S17.** `idle_slash_hint` ranks by registry order; the
during-turn row keeps `typeahead_slash_hint`. Both are thin wrappers over one
`slash_hint_ranked` holding the shared structural rules, so only the ranking
differs — and **the function name is how a call site declares its policy**,
since there is no longer a default to inherit silently. That was the whole
defect. The third stand-down clause is gone, pinned as *removed* rather than
merely unreachable by a test that passes the old disagreeing hint explicitly and
still requires a palette.

The exact-match case is handled structurally rather than by special-casing: the
length test moved out of the ranking into the shared tail, so when the lead
entry *is* the prefix the hint empties instead of the ranking stepping past it
to a longer sibling. `/agent` and `/save` both yield nothing, matching the
during-turn path's own behaviour for its exact matches.

**Four tests had to change, and they were pinning the defect.** The inline
reader's hint and Tab group expected `/s` to hint `teer` and Tab to type
`/steer` — one of them said so in its own prose: *"Tab at an idle `/s` types
`/steer`"*. That is F30's symptom written down as an expectation, so no
implementation could satisfy both it and the fix. The implementer correctly
refused to touch them and escalated; I made the change, keeping `/s` as the
input so the tests now assert the corrected behaviour rather than dodging the
interesting case, and noting the cursor column moved with the shorter word.

Worth noting as a general lesson: reusing a function across contexts carried
its *policy* along with its mechanics, and the policy was named for the context
it came from. The name said `MID_TURN` and was reused where no turn exists.

### A second defect in the same area, found by the property rather than by examples

While writing T0-S17's tests the author pinned the invariant the stand-down
clause had been compensating for — *for any slash prefix with a match,
`prefix + hint` equals the lead palette entry's name* — and it failed first not
on `/s` but on **`/agent`**, which neither I nor T0-S16 had noticed.

`typeahead_slash_hint` selects with
`find(|name| name.starts_with(rest) && name.len() > rest.len())`, which **skips
an exact match**. Both `agent` and `agents` are registered, so `/agent` hints
`s`, completing `agents` — while registry order's lead for `agent` is `agent`
itself. The correct idle hint there is **empty**, which is also what the
during-turn path does for its own exact matches (`typeahead_slash_hint("/steer")`
is empty) and what the palette's second stand-down clause already assumes.

**The lesson is about the instrument, not the bug.** Example-based tests had
covered this area for three cards — `/s`, `/co`, `/cont`, `/context`, `/mod` —
and none of them happened to be a command name that is also a prefix of
another. A property over every proper prefix of every registered command (about
390 of them) found it on the first run. Where a relationship between two
functions is the real contract, assert the relationship, not a handful of its
instances.

### Two call sites, not one

Worth recording because a half-fix here would have been worse than the defect.
The idle prompt reads the hint in **two** places: `hint_of`, which is what the
row *displays*, and the `CompleteRequest` arm, which is what Tab *accepts*.
Both inherited the mid-turn ranking, which means the bug was never cosmetic —
**Tab at an idle `/s` typed `/steer`**. Fixing only the display would have left
the row showing one completion while the key inserted another. I verified both
sites independently of the test author, and we agreed.


## F32 — a third `flock` site has the same fork-inheritance defect

Found in T0-S18 by the implementer, **outside its scoped files**, reported
rather than quietly fixed.

`crates/kitsu/src/config/publication.rs:516` takes the identical
`flock(LOCK_EX | LOCK_NB)` and maps the first `WouldBlock` straight to
`PinTargetError::Busy`. Same mechanism as F28, so the same false positive: a
config publication can report the target busy because another thread was
mid-`fork`.

It is not hypothetical. It is the residual flake left after F28's fix:
`config::publication::tests::parallel_publication_receipts_remain_exact_until_rollback`
failed **2 of 20** runs at `--test-threads=256`, and **0 of 40** runs of that
test alone — the signature of fork interference rather than an intra-test race.
It did not appear in my own 6-run verification, so it is rarer than one in six.

**The fix is known and already written**: the same
`flock_exclusive_with_fork_retry` helper, which needs to become reachable from
`config::publication` (`pub(crate)`, or moved somewhere both modules see).

**Deliberately deferred one card** rather than fixed on the spot, to keep the
loop's discipline: a regression test comes from a different agent than the fix,
and the deterministic-reproduction pattern for this bug class is now established
(spawner threads plus a tight acquisition loop), so writing one is cheap. The
cost of waiting is one known-rare flake, already recorded; the cost of skipping
the test would be a third fix with no evidence it worked.

### It is not three sites. It is five, and I found the last two by surveying

Before briefing T0-S19 I ran an **exhaustive, untruncated** grep for every
`libc::flock` in the workspace — the discipline this file already records as a
rule, applied proactively rather than after being wrong. It found two more
production sites that nobody had looked at, because the diagnosis was scoped to
sessions and the implementer to its own two files:

| Site | Inference on first `EWOULDBLOCK` | Status |
|---|---|---|
| `session.rs` `SessionLease::acquire` | "already active" | fixed, T0-S18 |
| `session/physical.rs` `acquire_writer_fence` | "already active" | fixed, T0-S18 |
| `config/publication.rs:516` | `PinTargetError::Busy` | **open** |
| `artifacts.rs:227` `probe_lock` | `Liveness::Live` | **open, newly found** |
| `artifacts.rs:264` `hold_generation_lock` | `None` (lock not held) | **open, newly found** |

The two artifact sites have the same false-positive shape with different
consequences: `probe_lock` reports a **free** lock as **live**, so a reclaimable
artifact generation looks in use and is never reclaimed; `hold_generation_lock`
silently returns "could not acquire", so its caller skips or fails work it
should have done. Neither is as loud as a failed session commit, which is
presumably why neither was noticed.

(The sixth and last `flock` in the tree is in `tests/cli.rs`, which is a test
deliberately taking a lock to create contention — correct as written.)

**A note on why so many sites existed.** Every one of them hand-rolled the same
`flock(LOCK_EX | LOCK_NB)` → "busy" inference independently. T0-S18 introduced
the first shared helper; the lasting improvement is that there is now **one
place to be correct**, and the remaining three should adopt it rather than
anyone adding a sixth copy.

### `probe_lock` nearly proved itself innocent, and the reason generalises

Worth recording because the first attempt at its regression test **passed**, and
a passing test here would have been read as "this site is fine".

`probe_lock` takes the lock, immediately `LOCK_UN`s on success, and reports
whether it was free. A test that simply probed an untouched lock file in a loop
never failed — because `LOCK_UN` releases the **open file description**, which
is the very description a forked child inherited. So a loop of nothing but
probes **cleans up after its own leak** and never observes contention.

The insight that fixed the test: `probe_lock` is only ever wrong about a lock
that *some other code path* leaked across a fork. The working test therefore
uses the production leak source — a `hold_generation_lock` taken and dropped on
the same generation directory each attempt, which is exactly the "run finished,
directory now reclaimable" state a sweep inspects. It then `continue`s rather
than fails if that acquisition itself trips, so the two artifact defects stay
isolated and the fix order does not matter.

**The generalisable form:** when testing a race in a *reader* of shared state,
the reader may repair the very condition it is supposed to catch. Reproducing
such a bug needs the real producer of the bad state, not a synthetic stand-in.
This is the same shape as F27 — where a full repaint repaired the damage a
partial repaint would have shown — and both were found by an agent noticing its
own test passed when it should not have.

Tuning note: all three tests needed `ATTEMPTS` raised from 300 to 2000 to reach
100 failures in 100 runs; at 300 they sat at 91, 96 and 98. The cost is nil,
since each aborts at the first failure, typically within a few dozen attempts.

### RESOLVED in T0-S19 — and the structural result is the point

All three remaining sites now call the shared helper. **Verified
independently**: a grep of the whole production tree finds exactly **one**
`flock(LOCK_EX | LOCK_NB)` acquisition left, and it is inside
`flock_exclusive_with_fork_retry` itself. Five hand-rolled copies of the same
wrong inference have become one place to be correct — which matters more than
the three fixes, because it is what stops a sixth appearing.

The helper was made `pub(crate)` with **no re-export and no move**: `artifacts`,
`config` and `session` are sibling modules, so `crate::session::flock_…`
resolves directly. The implementer argued for this over a crate-root re-export
(a second name for one function, hiding where it lives) and over a new module
(which would drag the budget constants and the `cfg` block away from the fence
callers and their tests). The fully qualified path also makes each call site
self-documenting. Each site keeps a short comment saying why a plain `flock`
would be wrong, so a reader of `artifacts.rs` need not find `session.rs` first.

Verified: helper logic and budget byte-identical (only the signature line
changed); no `cfg` widened or narrowed — the `publication.rs` call stays inside
its `#[cfg(target_os = "linux")]` block while the helper remains `#[cfg(unix)]`;
`probe_lock` keeps its `LOCK_UN`, since only the failure inference was wrong.
`-p kitsu --bins` 729/0 in both profiles, six runs at `--test-threads=256` all
clean, 0 failures in 50 release runs per new test.

The one deliberate exception is in `crates/kitsu/tests/cli.rs`, where a test
takes the lock from the *test* side to simulate a live holder and asserts on the
raw return value. Wrapping that in the retry would change what it tests, so it
stays direct.

## F33 — `cargo fmt -p kitsu-tools` rewrites the maintainer's uncommitted work

Found in T2-S1, by an agent that did it, caught it and repaired it — and said
so, which is the only reason it is recorded rather than discovered later.

`crates/kitsu-tools` is **not rustfmt-clean at HEAD**: with the maintainer's
uncommitted `fs.rs` work stashed, `cargo fmt -p kitsu-tools -- --check`
reports **4** violations, two in `builtins/fs.rs` and two in `permissions.rs`.
That is F1 narrowed to one crate.

The consequence is a trap rather than an inconvenience: `cargo fmt -p` formats
the **whole crate**, so any card that runs it in `kitsu-tools` reformats those
four places — and while the maintainer's uncommitted edit-tool changes sit in
`fs.rs`, it reformats *their* lines too. The agent's repair restored the file
byte-for-byte (verified: 54 insertions / 20 deletions in three hunks, exactly
as at session start), but the next card would hit it again.

### My first rule here was wrong, and it caused the second incident

I recorded the rule below after the first occurrence:

> format only the files you changed — `rustfmt --edition 2021 <file>`

**That is unsafe, and the next agent tripped on it doing exactly what I told it
to.** `rustfmt` in write mode follows the `mod` declarations of the file it is
given, so pointing it at a **crate root** formats the entire module tree — it
is equivalent to `cargo fmt -p` for writing purposes. And `lib.rs`, the file
this card had to change, *is* the crate root. So my "safe" command reformatted
both pre-existing violations in `builtins/fs.rs` — inside the maintainer's
blast radius, taking the numstat from 54/20 to 68/25 — and both in
`permissions.rs`.

It was repaired precisely: the agent reverse-applied **only** the two
rustfmt-induced hunks in `fs.rs` with `git apply -R`, leaving the maintainer's
three hunks untouched, and restored `permissions.rs` from HEAD since it had no
local changes. I verified: numstat back to `54 20`, the same three hunk
headers, `permissions.rs` clean, and `rustfmt --check` still reporting the same
4 pre-existing violations — the crate's unclean state preserved exactly as
found.

Note also that my original diagnosis named the wrong culprit: I blamed
`cargo fmt -p` for the first incident, but the mechanism is rustfmt following
`mod` declarations, which both commands trigger.

### The actual working rule for `kitsu-tools`

**Do not run write-mode rustfmt in this crate at all while the maintainer's
`fs.rs` work is in the tree.**

- `rustfmt --check` and `cargo fmt -p kitsu-tools -- --check` are **read-only
  and safe** — use them to inspect.
- To fix formatting in one file, hand-fix it, or format a *copy* elsewhere and
  diff the result back. This rustfmt has no `--skip-children`.
- A card's gate is "the file I touched is clean", verified with `--check`, not
  "the crate is clean" — the crate is not clean, and making it clean is the
  maintainer's call because their uncommitted work sits in the blast radius.
- Pointing rustfmt at a **leaf** module file is far safer than at a crate root,
  since a leaf declares no submodules — but the root is exactly the file most
  cards need to touch, so the rule above is stated as a flat prohibition rather
  than a nuance to be remembered under pressure.

Worth noting for F1's eventual fix: the four violations are all pre-existing
committed code, so formatting them is safe **once the maintainer's `fs.rs`
work is committed or set aside** — and doing it then removes this trap
permanently.

## F34 — a blocked agent's deck marker is identical to a running one

Decided in T2-S3, recorded because it is a deliberate gap rather than an
oversight, and because the agent-view milestone is where it should be closed.

`kitsu-render`'s `DeckState` has no `Blocked` variant, and B5 scopes T2-S3 to
three vocabularies plus one validator — extending another crate would widen the
card. So a blocked agent reuses `DeckState::Running`: it is non-terminal and
still alive, `ResultReady` would imply something to collect, and the terminal
states are plainly wrong.

**The cost:** in the `/jobs` deck the glyph beside a blocked agent is the same
as beside a running one. The state is distinguished only by the word `blocked`
in the row text.

**Why that is tolerable for now, and it is a genuine question rather than an
assumption.** Three other signals already exist, all built in T1 before
anything could reach them:

- the live status row renders `[N blocked] `, and **blocked outranks running**
  in that tag, so the count is the first thing the row reports;
- `/jobs view` groups blocked agents under `needs input`, which is ordered
  **first** among the groups;
- the row text and `/jobs explain` both say `blocked`.

So a user is not relying on the glyph to notice. But the glyph is the densest
signal in the deck, and "needs your answer" is exactly the state that should be
scannable — so this should not stay as it is.

**Where it belongs:** milestone T4, the agent view, which is where blocked
agents get real visual treatment (design A-series and B4). That is a
`kitsu-render` story: add a `DeckState::Blocked` with its own marker, and the
only test that changes is T2-S3's `the_blocked_deck_row_is_marked_live_and_reads_blocked`.

### Also recorded from T2-S3

- **`AgentStatus::Blocked` is a unit variant**, unlike `Done(String)` /
  `Failed(String)` / `Cancelled(String)`. Nothing carries a reason yet, so the
  smallest honest surface wins — the same call made for T2-S1's route payload.
  If T2-S4 needs `Blocked(..)` to carry the pending request, that is **one**
  construction site to change, and it must be called out rather than reshaped
  silently.
- **`jobs_snapshot_payload`'s `"blocked"` key is deliberately unpinned.** It is
  added so the payload keeps mirroring the deck counts key-for-key, but it
  cannot be exercised until an agent can block, so T2-S4 owns its test. Added
  rather than omitted, because an inconsistent payload is worse than an untested
  key.
- **The plan's "state-file reader accepts v1 and v2" line is moot.**
  `agent_state_file.rs` has its own independent schema and never embeds
  `AgentThreadSnapshotDto`, and `kitsu agents --json` (H1) does not exist yet.
  So there is no cross-process reader of the thread DTO today and no
  v1-tolerance path to test. T2-S3 pins the strict in-process behaviour instead
  (schema 1 is rejected, naming the expected version); if a later milestone
  needs v1 tolerance it must be a deliberate accept-older-version path, and that
  test revisited on purpose.


## F35 — `interactive_prompts_allowed()` is now test-only, deliberately

Recorded in T2-S2 so it is not mistaken for dead code to delete, nor left to
rot.

Both gates now match the whole `PromptRoute` rather than asking a yes/no
question, so **no production code calls `interactive_prompts_allowed()` any
more**. Two pre-existing lib tests do, and the implementer could not touch
tests, so the function survives behind
`#[cfg_attr(not(test), allow(dead_code))]`.

That is the right outcome rather than a leftover, and the reason is worth
stating: the function now *documents an invariant* —
`interactive_prompts_allowed() ≡ matches!(prompt_route(), Foreground)` — and a
T2-S1 test pins exactly that equivalence. Keeping it means the equivalence
stays checked as the route grows variants; deleting it would remove the only
assertion that a new variant has not quietly become "allowed to prompt".

**For whoever revisits it:** either keep it as the documented equivalence (and
leave the attribute), or remove it *together with* the two tests that call it —
but not the function alone, because that silently drops the invariant check. If
a future variant should permit prompting, the equivalence test is what will
force that decision to be explicit.

### Correction: the `kitsu-tools` test stub is permanent, not S4's to delete

T2-S2's record said T2-S4 "must delete the `#[cfg(test)] registry_prompt_stub`".
**That was wrong**, and the reason is the same dependency direction that has
shaped three decisions in this milestone already: `kitsu-tools` **cannot name a
type from `kitsu`**, so the real `AgentRegistry` can never be the receiver in
`kitsu-tools`' own tests. The stub is that crate's permanent test scaffolding,
not a placeholder.

Worth stating as a pattern, because it keeps recurring: the gate lives in
`kitsu-tools` and the registry lives in `kitsu`, so **every** seam between them
is an opaque value (`u64` agent ids, a `PromptRequestSender`, a `Decision`) and
every test of the lower crate needs its own stand-in. That is a constraint of
the layering rather than a shortcoming of any card, and it is why the route
carries an integer instead of an agent handle.


## F36 — adding a non-terminal state means revisiting every "is it terminal?" decision

The lesson from T2-S4, and the semantic twin of B5's "three vocabularies must
change together".

B5 made the *labelling* consequence explicit: a new phase touches three enums
and one validator, and the compiler finds those because the matches are
exhaustive. **It said nothing about the behavioural consequence**, which is
larger and which the compiler cannot find: every place that asks "is this agent
finished?" by testing `status == Running` now silently answers **wrong** for a
blocked agent, because such tests are written as a positive match on `Running`
rather than a negative match on the terminal variants.

The implementer found ten such sites and widened each to `Running | Blocked`:
cancellation matching, `cancel`'s pre-check **and** its completion-race
re-check, capacity accounting (a blocked agent still holds a running slot),
`running()`, two prune paths, a result-acknowledgement guard, and two
`debug_assert`s.

**One of those was a real leak, found without a test asking for it.** Both prune
paths treated anything not `Running` as terminal, so a blocked record would have
been pruned — **discarding its parked reply sender**, leaving the gate awaiting
an answer that could never arrive. No test in the card covered pruning a blocked
agent; it was found by enumerating the sites rather than by the suite.

Three guards were deliberately **not** widened, and the reasoning is the useful
part: `finish_with_telemetry`, `record_progress_event` and `steer` keep their
`Running`-only checks, because resolution always restores `Running` before the
gate wakes — so a timed-out agent's own error reaches `finish` through the normal
path, which is exactly what makes the end-to-end timeout test pass. Widening
those would have been cargo-culting the pattern.

**Generalisable rule:** when a state machine gains a non-terminal state, grep
for the *positive* test (`== Running`, `matches!(.., Running)`) rather than
trusting exhaustive matches — the exhaustive ones are the safe half, and the
positive tests are where silent misbehaviour lives.

### Also from T2-S4

- **A park token defends a race nothing asked about.** Each parked request
  carries a monotonic token, so a timer that fires for an already-answered or
  re-parked request is a no-op instead of spuriously interrupting the *next*
  question on that agent. Unprompted and correct.
- **Four disjoint routes guarantee B3's no-hang property**, each resolving the
  gate's `oneshot` rather than dropping it: the human's answer, the 30-minute
  timer, cancellation or teardown, and the registry being dropped (the receiver
  loop breaks and `Drop` on the sender means `Interrupt` by contract). A request
  that cannot be parked at all — unknown, terminal or already-blocked agent — is
  resolved `Interrupt` **synchronously**, so no gate can ever wait on a request
  nobody owns.
- **`prompt_sender` holds a weak reference**, so handing a sender to a gate can
  never keep the registry alive.
- **The declared MSRV was respected even though F9 says it is already
  unachievable.** `Option::is_none_or` is stable only from 1.82, so the token
  comparison is a tuple `match`. Adding no *new* MSRV violation while an
  existing one stands unresolved is the right call: F9 is a release decision for
  the maintainer, not a licence to drift further.


## F37 — two test defects that a green suite would never have shown

Both found in T2-S5a by the implementer, both **proven rather than argued**, and
both instructive beyond this card.

### A positive assertion that no implementation could satisfy

`test_blocking_ask_parks_a_gated_detached_agent_and_times_it_out` asserts the
literal `needs input`, but its poll loop typed `/jobs` and `/agents`. Both print
the **agent deck**; `needs input` is a group heading produced only by
`format_jobs_view`, reached by **`/jobs view`**. So the assertion was
unreachable at any timeout.

**I had pre-authorised the wrong fix.** My brief told the implementer to raise
`block_timeout_secs` from 1 to 2 if the string was ever missed. It declined,
showed that at 2 seconds the agent sat visibly parked across about six polls and
the test still failed, and identified the real cause — a one-word change. Then
it measured: **30 of 30 passing** with `/jobs view` (10 debug at 1s, 10 release
at 1s, 10 release at 2s), and I reproduced 10 of 10 myself. An authorised
instruction is not evidence; it was right to override it.

### Two negative assertions that passed vacuously

The same mistake made the *guards* worthless: `!transcript.contains("needs
input")` in both default-behaviour tests polled a view that can never print the
string, so they would have passed however broken the feature was. Fixing the
shared helper tightened all three tests at once.

**The general shape, which is the reusable part:** a negative assertion over
captured output is only as strong as the command that produced the output. A
test asserting "X does not appear" must first establish that X *can* appear
there — otherwise it is a tautology wearing a guard's clothing. This is the
third vacuity of this family in the project, after F27 (a full repaint repairs
the damage a partial repaint would reveal) and T0-S19's `probe_lock` (a reader
that repairs the condition it is meant to catch).

### A byte-identity test that could not pass for any implementation

`test_scripted_and_jsonl_runs_are_byte_identical_under_the_default` compares
full transcripts of four runs. The JSONL `run_started` event carries
`provider_route.route.profile` — `profile-v1-` plus a 32-hex digest of the
**canonicalised config path** — and each run uses a fresh temp home, so that
field differs by construction. The normaliser collapsed digit runs, which
cannot help: a hex digest's `a`-`f` survive. The implementer measured two actual
digests rather than theorising.

Fixed by normalising the digest, not by sharing a config path: the digest is
**environmental** like `run_id` and `duration_ms`, which that test already
normalises, and it is not the behaviour under test. The plain half of the
comparison — which carries no profile field — passed throughout, so the
feature's byte-identity claim was evidenced even while the JSONL half was
unprovable.


## F38 — adding a `/jobs` subcommand touches seven sites, not three

Enumerated while scoping T2-S5b. The plan says "all three dispatch sites", and
that phrase has been repeated since T1-S3 — but it counts only the sites that
**execute** the subcommand. Four more decide whether a user can ever find it,
and a command that works while being undiscoverable is half-shipped.

**Behaviour (the three the plan means):**
1. `handle_jobs_command` — the idle path, which also emits a JSON payload
   carrying an `"action"` field;
2. `handle_active_jobs_command` — the mid-turn path, a different function with a
   different return type (`Result<String, String>`);
3. the `/agents` alias arm, which T1-S3 widened to forward verbatim.

**Discoverability (the four it does not):**
4. `closest_jobs_subcommand`'s `SUBCOMMANDS` list — the typo suggester. **This is
   F2**, already recorded as drifting from the registry, and it drifts precisely
   because it is a hand-maintained list no compiler checks;
5. the `/jobs` advertised `args:` string, which the help and palette render;
6. the `/agent` advertised `args:` string — which **already lists fewer
   subcommands than `/jobs`** (no `view`, `explain`, `stop-all`, `prune`),
   apparently left behind when T1-S3 widened the alias to forward everything.
   So the alias accepts commands it does not advertise;
7. the usage line printed on an unknown subcommand.

**Why this keeps happening:** sites 1-3 are `match` arms, and adding a
subcommand there is driven by the work itself. Sites 4-7 are **string and array
literals** that no compiler, test or type checks against the real set — the same
shape of defect as F2, and the same shape as T2-S3's label table, except that
one *was* compiler-enforced because the enums were exhaustive.

**The durable fix is not vigilance.** A single source of truth — one table of
subcommand name, arity and help text that the dispatcher, the suggester, the
`args:` strings and the usage line all read — would make sites 4-7 impossible to
forget. That is a refactor worth its own card. Until then, anyone adding a
subcommand should treat this list of seven as a checklist.

### F2 is closed by a test, not by a checklist

T2-S5b did better than the checklist above. Its test
`every_jobs_subcommand_is_its_own_nearest_suggestion` requires **all ten**
dispatched subcommands to be their own nearest suggestion — so the suggester
table can no longer drift one entry at a time, which is exactly how **F2**
arose. That converts a vigilance problem into a failing test, and it is why F2
can be closed rather than carried.

Sites 5-7 remain unguarded by anything but review, so the single-source-of-truth
refactor is still worth doing; but the worst of the three is now mechanical.

### The alias drift was in THREE places, not one

Scoping this card found `/agent` advertising five of the ten subcommands it
forwards. The test author fixed the `args:` string in code. Writing the docs I
found **the README's own command-table row carried the same stale subset**
(`list|show|steer|stop|close`), and the row's prose describes only those — a
third copy, in a third file, of a list that is already wrong twice.

I replaced the README row with **`/agent [any /jobs subcommand]`**, which is
what the `/agents` row already says and which **cannot drift again**, because it
states the relationship instead of enumerating the set. That is the cheap
version of the single-source fix: where a list is derived rather than
authoritative, say so instead of copying it.

## F39 — one unidentified CLI-suite failure, and the capture mistake that lost it

Observed in T2-S5b review. **Status: one failure in seven full-suite runs,
test unknown.** Recorded rather than dismissed, because this project has twice
punished the comfortable reading of exactly this situation.

The implementer reported `cargo test -p kitsu --test cli` at 237/0. My own
verification run returned **236 passed, 1 failed**. Six subsequent runs — three
immediately, three with logs retained — were all 237/0, and no failure named
itself in any retained log.

### The mistake is mine and it is the reusable part

When I saw `236 passed; 1 failed`, **I re-ran the suite to find out which test
it was.** That destroys the evidence: the failing run's output is the only place
the name exists, and a fresh run that passes prints nothing. I should have
captured that run's output — or run with output retained from the start, which
is what I belatedly did.

**Rule: when an intermittent appears, keep the failing run's output before
doing anything else.** A re-run is not an investigation; it is the deletion of
the only artefact that identifies the problem.

### Why it is not being dismissed

- **F23**: a pseudo-terminal test in this very suite failed ~7% of the time
  while appearing green on every manual run, and was found only by measuring
  against a stashed baseline.
- **F28**: a transaction flake that three readings called test noise — including
  mine — turned out to be a **real production bug**, a `fork`/`flock` race that
  could make a session commit falsely report "already active".

Both times "I ran it again and it passed" was the wrong conclusion. So the
honest status here is *unidentified*, not *absent*.

### For whoever sees it next

Run `cargo test -p kitsu --test cli > log 2>&1` in a loop and keep every log;
the suite takes about 43 s, so a dozen runs is ten minutes. The new
pseudo-terminal tests in this area (`test_jobs_reply_*`,
`test_blocking_ask_parks_*`) are the most likely candidates, being real
processes on real terminals with a 600-second block timeout and an exit guard
in play — but that is a guess, and the point of this finding is that a guess is
all anyone has until a failing run is captured.
## F40 — redaction is credential-shaped, and the timeline's real protection was an accident

Established while scoping T2-S6, by reading rather than by assuming.

`docs/functionality.md` states an **absolute** about the agent inspection
timeline: *"tool arguments/output and partial model bodies are never copied into
that inspection record."* An approval entry is the first thing in this epic that
runs straight into it, because a permission request **is** a tool plus its
arguments — a path, a URL, a command line.

### The protection was not the redaction

`prompt_sender` destructures only `request.agent_id` and `request.reply`;
**`request.payload` is dropped unread**, and `park_prompt`'s signature is
`(id, reply)`. So no argument *could* reach the timeline — **not because
anything removed it, but because the data never arrived.**

That is protection by accident. It holds only while nobody has a reason to
plumb the payload deeper, and T2-S6 is exactly that reason: recording *what* an
agent asked for requires the request at the recording site.

### And redaction would not have caught it

The test author proved this rather than reasoning about it:
`redaction_does_not_remove_a_path_or_an_argv_word` asserts
`sanitize_and_redact_text(x) == x` for a filesystem path, a workspace path,
`rm -rf /work/tmp` and a URL path. `sanitize_and_redact_text` strips terminal
control sequences and **credential forms** — known key prefixes, `key=value`,
`Bearer`, URL userinfo. Paths and argv words are left **completely untouched**,
and `format_agent_thread`'s `sanitize_terminal_text` does not touch them either.

So an implementation that formatted a request's arguments into an entry would
have printed them verbatim at every surface — `/jobs show`, `/jobs explain`,
and the JSONL snapshot — while passing `validate`, because the DTO's redaction
check uses the same credential-shaped function.

**The general rule: redaction in this codebase answers "is this a secret?", not
"is this private?". A path is not a credential, and nothing in the redaction
path will stop one.**

### What T2-S6 does about it

The entry carries a **kind and nothing else** — `AgentApprovalKind` is a unit
enum (`ReadFile`, `WriteFile`, `RunCommand`) with no argument-bearing field, so
the timeline can say *"waiting for permission to write a file"* and cannot say
which file. The payload is converted to a kind at the boundary and never travels
further.

The unit enum is the durable part: a future change cannot slip a path into an
approval entry **without altering a public type**, which is a review-visible
act rather than a silent one. That is the same move as T2-S3's exhaustive
`match` — making the compiler, not vigilance, the thing that notices.

### Worth checking elsewhere

Any other place that relies on "the sensitive field simply is not passed here"
has the same fragility, and the grep for it is not redaction calls but
**destructuring that drops a field**. Two candidates worth a later look: the
hook payload assembly and the state-file writer, both of which take narrow
structs today for reasons that may be incidental rather than designed.

## F41 — a bare substring ban constrains the product instead of protecting it

T2-S6's argv-absence test swept the serialized snapshot for
`["rm", "-rf", "/work/tmp", "/usr/bin/rm"]`. The implementer's first attempt
failed it — **on the word "permission"**, which contains `rm`. Nothing had
leaked; the assertion simply cannot distinguish the command `rm` from the middle
of an ordinary English word, and it equally forbids *format*, *confirm*,
*transform* and *firmware*.

The implementer worked around it by choosing wording with no argv substring
(`"approval to …"`), flagged it rather than hiding it, and correctly did not
touch the test. But that left the product's user-visible wording **constrained
by an accident of a test assertion**, which is the wrong way round.

**Fixed at review by making the assertion stronger, not weaker:** the entry text
is now pinned with `assert_eq!` to the exact expected string. If the text is
exactly `"approval to run a command requested"` and nothing else, **no argument
can be inside it, whatever the argv happened to be** — a property a substring
ban cannot express. The snapshot-wide sweep is kept, because an argument could
leak into some other field, but with tokens distinctive enough that only a real
leak matches (`-rf`, `/work/tmp`, `/usr/bin/rm`, `rm -rf`).

Verified rather than assumed: injecting `/work/tmp` into the entry text makes
the strengthened assertion fail, and removing it makes it pass.

**General rule: to assert that something private is absent, pin what the output
*is*, not a list of things it is not.** An exact match is both stronger and
immune to coincidence; a substring blocklist is weaker and fires on innocent
text. This is the fifth entry in this project's vacuity-and-assertion-quality
family (F27, F32, F37's two, now this).

## F42 — `git checkout <file>` destroyed uncommitted work, and the mistake was mine

**Recorded because it cost real work and the lesson is operational, not
intellectual.**

While verifying F41's strengthened assertion, I injected a path into an entry
text in `crates/kitsu/src/subagent/thread.rs` to prove the assertion failed on a
real leak. It did. I then reverted that one-line injection with
`git checkout crates/kitsu/src/subagent/thread.rs`.

That file held **two agents' uncommitted T2-S6 work** — the test author's enums,
stubs and six tests, and the implementer's wiring on top. `git checkout` reverts
to the index, and nothing in that file had been staged, so it reverted to HEAD
and **wiped all of it**. `subagent.rs` (+219) and `main.rs` (+183) survived and
referenced the now-missing types, leaving 21 compile errors.

Recovery was complete rather than reconstructive, because both agents still held
their contributions and restored them in sequence — the test author its stubs and
tests, then the implementer its wiring — so the TDD record stayed honest and
nothing was rewritten from memory. The cost was time, not work.

**The rules this produces:**
1. **Never use `git checkout <path>` to undo a deliberate edit in a file that
   holds uncommitted work.** Re-apply the inverse edit, or make the experiment
   in a copy under the scratchpad and never in the live file.
2. **Before any destructive git operation on a path, check
   `git diff --numstat <path>`** — exactly the discipline already applied all
   session to the maintainer's `fs.rs`, which stayed untouched at `54 20`
   throughout. The protection existed; I simply did not extend it two files
   over.
3. **Verification experiments belong in a throwaway copy.** Proving an assertion
   has teeth is valuable (F41 is better for it), but the proof should not
   require mutating the file under review.

## F43 — CORRECTED: a cancelled approval is recorded as nothing at all

**My original text said a cancelled approval is recorded as "timed out". That is
wrong, and the truth is worse. The correction and its lesson are below; the
original reasoning is kept after it so the mistake is legible.**

### What is actually true, verified by reading every call site

**Cancellation never reaches `resolve_prompt`.** There is one cancel path,
`cancel_matching` — the single implementation behind `cancel`, `cancel_all`,
`cancel_all_with_reason` and `cancel_started_after*` — and inside the registry
lock it takes the parked prompt **itself**, calls `thread.unblock()`, sets
`AgentStatus::Cancelled`, and sends `Decision::Interrupt` straight to the
waiting gate's channel. It never calls the shared resolution path, so **no
outcome is recorded at all**.

What a cancelled park actually leaves behind is a **dangling request with no
outcome**, proven from a real `/jobs show`:

```
  bounded redacted timeline:
    [1 +0.0s user_task] cancelled while blocked
    [2 +0.0s approval] approval to write a file requested
    [3 +0.0s lifecycle] agent cancelled; result ready for handoff
```

The agent asked, and the record never says what happened. The still-armed block
timer that fires later finds no park and records nothing either.

**So T2-S6's claimed invariant — every resolution leaves a record — was already
broken when I accepted that card**, for one of its five cases. Four of five
resolutions were recorded; the fifth recorded nothing. And
`approval_outcome(Decision::Interrupt) => TimedOut` turns out to be *correct*,
because the only `Interrupt` that reaches `resolve_prompt` really is the timer.

### The lesson, which is about me

I wrote F43 from the implementer's report, which said *"`Interrupt` also arrives
from a cancellation of a blocked agent, which is likewise recorded as a timeout"*
— and I recorded that as a finding **without tracing the call path myself**. One
grep for `pending_prompt` would have shown four touch points and that
cancellation was not among the recorders.

**A claim about a code path is not verified until the path is read.** I had
already written F39's rule — a re-run is not an investigation — and then made the
same class of error one level up: a plausible mechanism, reported confidently by
an agent whose other work was excellent, propagated into a finding and from
there into a card brief. The agent that fixed it checked, found the premise
false, and said so instead of implementing what I asked for.

**Two things follow for the rest of this epic:** when a report explains *why*
something happens, the mechanism is a hypothesis until someone reads the path;
and findings written from a report, rather than from code, should say so, so the
next reader knows what has actually been verified.

### The fix this card takes

Add a fifth `AgentApprovalOutcome::Cancelled` and record it in `cancel_matching`,
where the parked prompt is already taken under the lock. The shared-path
guarantee is **not** weakened, because cancellation was never on the shared path
— it is strengthened, from four of five resolutions recorded to five of five,
with the two recording sites exhaustive and pinned by a test rather than assumed
by construction.

**No machine vocabulary is needed**, verified: `AgentApprovalOutcome` derives
only `Clone, Copy, Debug, Eq, PartialEq` — no `Serialize`, no `wire()`, no DTO
field of its own — and reaches machine surfaces only as the free text of a
timeline entry. `AgentApprovalKind` needs `wire()` because that kind *is* a DTO
field and a hook variable; this outcome is not. Adding `wire()` here would invent
a machine vocabulary nothing consumes, which **F45**'s own rule forbids.

---

### Original text, kept for the record (premise incorrect)

## F43 (as first written) — a cancelled approval is recorded as "timed out"

Noticed while reviewing T2-S6 and **not fixed in it**, because the fix is a
design change rather than a line.

`resolve_prompt` is the single resolution path, and `approval_outcome` maps
`Decision::Interrupt` to `AgentApprovalOutcome::TimedOut`. The B3 timer resolves
with `Interrupt` — correct. But **cancelling a blocked agent also resolves with
`Interrupt`**, so a human cancellation is recorded in the timeline as
`"approval to write a file timed out"`.

The whole purpose of this card is a truthful record of what an agent asked and
how it was decided, so a record that reports a deliberate human cancellation as
an unattended timeout is a correctness defect in the record, not a cosmetic one.
No test pins the cancellation case either way, which is why it passed review
green.

**Why it is not a one-liner:** the two cases arrive at `resolve_prompt` carrying
the *same* `Decision` value, so they are indistinguishable at the recording
site. Fixing it means either a fifth `AgentApprovalOutcome` plus a way to tell
the caller's intent apart (a parameter, or a distinct `Decision`), or recording
the outcome at the two call sites instead of in the shared path. That is a small
card, not a patch — and it should also pin the cancellation case, which nothing
does today.

### F40 follow-up — the hook path checked, and it is clean today

F40 left two candidates open: the state-file writer and the hook payload
assembly, both of which "take narrow structs today for reasons that may be
incidental rather than designed". Checked by reading before T2-S7 was briefed.

**The hook's `KITSU_AGENT_WAITING_FOR` already exists** — this card does not add
it — and today it is populated from `progress.telemetry.last_activity`, passed
through `safe_inline_label`.

Two things follow:

1. **There is no pre-existing leak.** Every activity string is built from a tool
   *name* via a `safe_activity_name` helper, or is a fixed phrase: `"started
   tool read"`, `"tool read ok"`, `"provider request round 3"`, `"usage
   updated"`, `"started"`. No argument, path or argv word is ever formatted into
   one. And the helper's existence suggests this was deliberate rather than
   accidental — unlike the timeline's protection, which F40 found was an
   accident of a dropped field.
2. **But the filter would not stop one.** `safe_inline_label` is
   `sanitize_and_redact_text` plus whitespace collapsing and truncation, so it
   inherits exactly F40's limitation: credential-shaped, not privacy-shaped. A
   path formatted into an activity string would reach the hook's environment
   **verbatim**, and a hook runs an **external program** — strictly worse
   exposure than the timeline, where a leak would at least stay inside kitsu's
   own output.

So the invariant to preserve in T2-S7 is: `waiting_for` may name the permission
**kind** when an agent is blocked, and must never name its argument. The
protection has to be the same structural one as T2-S6 — derive from
`AgentApprovalKind`, never format from the payload — because nothing downstream
will catch the mistake.

The state-file writer remains unchecked at the time of writing; T2-S7 touches it
and should answer the same question.

## F44 — F40's two candidates, answered: the hook was the fragile one

T2-S7 touches both places F40 flagged as possibly relying on "the sensitive
field simply is not passed here". Both were read. The answers differ, and the
hook's is the one that mattered.

### The hook payload assembly was never narrow

`AgentObservation` and `HookEvent` already carry `waiting_for` as a **free-form
`String`**, assembled from `progress.telemetry.last_activity` and protected only
by `safe_inline_label` — bounding, whitespace collapsing, and
`sanitize_and_redact_text`, which by T2-S6's own test leaves paths and argv words
**untouched**.

So an argument could have reached an **external program** today **with no type
change, no signature change and no public API change at all**. An implementer
writing the natural phrase *"waiting for permission to write
/work/notes.txt"* would have needed to change nothing, and nothing downstream
would have caught it. That is F40's "protection by accident", one layer further
out: the activity label is argument-free only because every call site happens to
format a tool *name* (via `safe_activity_name`) and nobody ever plumbed
arguments in.

**This is the highest-risk line in milestone T2**, and it is riskier than the
timeline it came from, because a hook leaves the process.

**Mitigated structurally rather than by care:** `AgentProgress` and the thread
snapshot DTO now carry `waiting_for: Option<AgentApprovalKind>` — a **unit
enum** — so the blocked reason travels to the assembly sites in a type that
cannot hold an argument, and the assembly only calls `.wire()`. The remaining
`String` in `AgentObservation` is legitimate (it carries activity labels for
*running* agents) so the guard there is an exact-equality test.

### The state-file writer is a narrow struct wrapped around a wide one

`AgentStateFile`'s own fields are fixed and safe, but it embeds the full thread
snapshot, which carries free-form `task`, `model` and every timeline
`entry.text`. So it is not narrow either; what protects it is one layer down —
and that protection is **T2-S6's structural guarantee in the timeline**, not the
wrapper and not `validate`, which runs the same credential-shaped redactor.
Making the new field a typed kind extends that guarantee rather than leaning on
the wrapper.

### A real defect this card fixes, found by the tests

A parked agent's `KITSU_AGENT_WAITING_FOR` reads the stale activity label
**`"started"`** — the hook correctly reports `STATE=blocked` and then gives a
nonsense reason, because `park_prompt` records no telemetry activity. The
tempting fix (call `record_activity` with the request) **is the leak path**; the
reason has to travel as the typed kind instead.

## F45 — one machine vocabulary, one human vocabulary, one source each

The three machine surfaces did **not** agree on how to spell a waiting reason,
and before T2-S7 the divergence was invisible because only one of them had the
field at all: the hook's free-form activity string. The state file and
`kitsu agents --json` had nothing.

Left alone, the natural implementations would have diverged — snake_case serde on
the DTO side, the English `label()` ("write a file") on the hook side — and the
mismatch would have been discovered by whoever first wrote a tool against both.

**Resolved and approved at review:**
- **`AgentApprovalKind::wire()`** → `read_file` / `write_file` / `run_command`
  is the single **machine** vocabulary, shared by the state file, the hook
  environment and `kitsu agents --json`, and equal to the serde spelling.
- **`label()`** → "read a file" / "write a file" / "run a command" stays the
  **human** text `/jobs` prints.

One source per audience, and a test asserts the DTO spelling *is* `wire()`, so a
second machine vocabulary cannot appear later. This is the same discipline as
T2-S3's label table and F2's suggester test: the spelling lives in one place and
a test fails if a copy appears.

The general point: **a human label and a machine identifier are different
things, and a surface that needs both should derive each from one source rather
than reusing whichever exists.** Reusing the human label as a wire value is how
`"write a file"` ends up in someone's JSON parser.

## F46 — the PTY harness has two opposite polling traps, not one

T2-S5a established: **poll `/jobs view`, never bare `/jobs`**, because
`needs input` is a heading only `format_jobs_view` emits, and getting that wrong
made a positive assertion unreachable at any timeout and two negative guards
vacuous (F37).

T2-S7's end-to-end test hit the **mirror image**. After replying to a parked
agent, its first run parked correctly, captured both surfaces, replied, released
the agent — and then died on the harness deadline, because the settle signal
(`CHILD_SAW`, which proves the released agent's work actually ran) reaches the
transcript only through the agent **deck**. `/jobs view` never prints a finished
agent's result, so polling it alone made the completion signal unreachable.

**The rule is therefore two-sided, and both halves are now paid for:**

| what you are waiting for | poll |
|---|---|
| an agent to **park** (`needs input`) | `/jobs view` — the deck does not group by state |
| an agent's **result** after release (`CHILD_SAW`) | the deck (`/agents`) — `/jobs view` omits finished work |

The safe default is to poll **both** (`/jobs view\n/agents\n`), which is what
the surviving drivers do.

**The deeper point, now three findings old (F37, F39, this):** a pseudo-terminal
test that "times out" is almost never a timing problem. In every case here it
was a **surface problem** — the assertion was watching a view that could not
print the thing it waited for. Raising the timeout would have hidden each one.
When a PTY test reaches its deadline, the first question is *"can this command
ever print what I am waiting for?"*, not *"is the machine slow?"*.

## F47 — a streamed answer's last line was being erased, and the first hypothesis was wrong

Found by T3-S1's new streaming PTY harness, on its first real turn. **The TUI
tests had never talked to a reachable gateway** — `write_config` pointed at
`https://gateway.invalid` — so in thirteen tests nothing had ever rendered a
streaming turn or a tool call. The defect was waiting there the whole time.

### The defect

In **classic styled mode**, when an answer's final line carries **no trailing
newline** — the usual provider shape — that line was written to the terminal and
then **erased**. It appeared on no screen, final or mid-session. Inline mode was
unaffected, which is what localised it.

Deterministic: 20 runs, both profiles, no flake.

### The first hypothesis was wrong, and that is the reusable part

The test author proposed that `finish_line`'s two separate writes (the partial
line, then its newline) left a window in which a live-row writer landed — and
**labelled it a hypothesis** rather than asserting it, which is what made the
next step cheap.

It was wrong in its central claim. The fixer instrumented `kitsu-render`, ran
the failing test, and found **`print_fragment` never appears in the trace at
all**: in classic mode with tools advertised, deltas are **buffered rather than
streamed live** (`orchestrator.rs:1725-1731`), and the whole answer is printed
in one shot by `print_message` (`orchestrator.rs:1869-1871`). The streaming path
the hypothesis blamed was not running.

**The verified cause:** `print_message` renders inside
`live::with_live_row_suspended`, and for text not ending in a newline the
suspension **closes with the cursor mid-row** on the row holding the last line.
The guard's `Drop` calls `redraw` → `render_region`, which paints with no
absolute positioning — **on that same row** — and `finish_line`'s following
`clear_row` wipes the remainder. `SUSPEND_DEPTH` and `write_block` are innocent;
the spinner thread never raced; the whole thing is single-threaded.

Two lessons, and the first is the same one F43 taught at my expense: **a
mechanism is a hypothesis until someone reads the path, and the cheapest way to
settle it is instrumentation rather than argument.** The second: a label of
"hypothesis" is not a weakness in a report — it is what tells the next person
the claim still needs settling, and it saved this fix from being applied to the
wrong writer.

### The invariant that was never written down

The region's painters are **relative** (`\r\x1b[2K`, `\x1b[1A\x1b[2K`), so each
acts on whatever row the cursor occupies. The unwritten contract is: *when the
region takes a fresh row, the cursor is at column 0 of a row the region owns.*
Nothing in `docs/ux-design.md` stated it, which is why a writer could violate it
silently. The fix makes it explicit — writers that may end mid-row declare it,
and the region steps down one row before taking ownership — and the invariant is
now documented in `ux-design.md` as load-bearing, with the requirement that any
new writer able to finish without a newline must declare it too.

### Still uncovered, flagged by the fixer

Three other writers can leave the cursor mid-row: the credential prompt
(`main.rs:2306`, mid-row **by design**) and two bare `print!` calls
(`main.rs:2967`, `main.rs:9982`). All three are non-chat paths where no live row
is drawn, so they are theoretical **today** and would become real the moment a
live row were painted concurrently. No test covers any of them. A regression
test for the credential prompt under a drawn live row is the gap worth taking
next.

## F48 — policy: a value from argv is echoed, a value from the config file is not

Settled in T3-S2 while making `[tui]` errors name their key. Recorded because it
is a **policy future cards need**, not a detail of one table, and because the two
cases differ in kind rather than degree.

**The rule.** An error about a value the user typed on the command line may
quote that value. An error about a value read from the config file names the
**key and the permitted set**, never the value.

**Why the config side suppresses.** The threat is **adjacency**, not the value
itself. The file holds gateway tokens the user did not type in this breath; a
parse error can quote a line they never meant to expose; and the error
**travels** — into `doctor --json`, logs, CI output, and pasted bug reports. The
user cannot predict which line the parser will quote, so the safe default is
that no config value crosses the boundary.

**Why the flag side echoes.** A flag value was typed on the line the user is
looking at, is one argument long, and is already in their shell history and
terminal scrollback, so echoing adds **no disclosure**. Withholding it costs
real diagnostic value: `--tui inlien`, `--tui ìnline` (a Unicode look-alike) and
a shell-mangled `--tui $MODE` that expanded to empty are **indistinguishable**
without the value. The empty case is why that flag needed a hand-written parser
at all — clap's own message for an empty value says "a value is required … but
none was supplied", which is actively misleading when the user did supply one.
It is also what every other flag in the binary already does, so suppressing only
one would read as a bug rather than a policy.

**The exception to apply if it ever arises:** if a flag's value is itself a
credential — a `--token`-shaped argument — suppress **at that flag**, not
everywhere. None exists today.

**How the config side is enforced**, since "remember not to" is not an
enforcement mechanism: each `[tui]` field's `deserialize_with` **drops** serde's
error with `.map_err(|_| …)` rather than wrapping it, and substitutes a message
the codebase owns. There is no `other =>` binding anywhere in that path, so no
code **can** interpolate the input. The error allow-list then admits exactly two
classes — serde's `unknown field` (which enumerates key *names*) and the local
`invalid value for `[tui]` key …` prefix.

The decisive property, verified by running the real loader: a typo and a
credential in the same key produce **byte-identical messages**, differing only
in the reported column. A message that cannot depend on the value cannot leak
it — which is a stronger guarantee than one that merely tries not to.

### F20 third pass — what R4 actually left undone, and what T3-S3 closes

F20 has now been corrected twice and clarified a third time, so here is the
settled list. Of the four things R4 was thought to have left behind:

- **cross-session history** — the original claim was **false**; it works, by
  borrowing rustyline's persistence;
- **reverse search** — also **false**; only the `⌕ reverse-search '…'` indicator
  was missing, and T0-S9 restored it;
- **Tab accepting the showing slash suffix** — **done** in T0-S10;
- **the lists** — `@file` path completion and the bordered palette — **the
  genuine remainder.**

**T3-S3 closes the `@file` half**, through one shared completion source rather
than a second implementation, and re-hosts the highlighter.

**It does not close the bordered palette**, and that distinction matters: the
palette's *content* is already solved (`tui_bridge::palette_rows` is a tested
thin join over the one renderer), so what remains is purely its **height and
placement** — a geometry problem entangled with **F16** (resize) and **F19**
(the short-terminal clamp), not a completion problem. Anyone reading "T3-S3
done" should not read it as "F20 closed".

The lesson F20 keeps teaching, now for the third time: **a list of "what is
missing" written from memory of a design is unreliable.** Two of its four items
were features that already worked. Each correction came from reading the code or
running the binary, never from re-reading the design.

## F49 — T3-S4 needed no production code, and the plan row would have made the product worse

The plan told T3-S4 to have the TUI composer "load/save the rustyline history
file format". Investigated before implementing, and the answer is that **history
is already shared, by delegation to a single owner**, and the row's literal
instruction would have created a **second parser for a format rustyline owns and
a second writer competing for one file on disk**.

**What the reading established** (all in `main.rs`): rustyline loads the file
(`:17433`), the inline reader is handed a **snapshot** of its entries
(`:6155`, taken inside `read_idle_chat_line`, which runs **per loop iteration**,
so it is fresh every turn), every submitted line is recorded on the path
**common to both readers** (`:17576`, outside any branch on which reader
produced it), and rustyline saves the file (`:18412`). `kitsu-tui` contains the
string "history" **nowhere**; the walk lives in `kitsu-editor` over a `Vec` it is
*handed*. So there is exactly one implementation and one file owner.

Order, de-duplication and blank handling were checked against **rustyline
14.0.0's own source**, not assumed: `iter()` is oldest-first and both walks take
the newest on the first ↑; the dedup policy is rustyline's default
`IgnoreConsecutive`, which kitsu does not override and the editor matches; blank
lines never reach either walk because `:17573` continues first. The first-↑
divergence that would otherwise exist is already closed deliberately
(`tui_bridge.rs:651` declines `RecallRequest` at the idle prompt).

**So the card shipped two tests and a plan amendment, and no code.** That is the
right outcome, and the general rule is worth keeping: **a plan row describes an
implementation that was plausible when it was written. If the system has since
solved the same problem a better way, satisfying the row makes the product worse
while making the plan look right.** Amend the plan.

### Two latent risks found on the way, neither this card's to fix

1. **A dropped cargo feature would turn a tolerated error into a panic.**
   `DefaultHistory` is `FileHistory` only because rustyline's
   `with-file-history` is a **default** feature. `MemHistory`'s `save` and
   `load` are `unimplemented!()`, so `let _ = rl.load_history(&path)` — written
   to tolerate a missing file — would **panic** rather than return `Err` if that
   feature were ever disabled, including by someone adding
   `default-features = false` for an unrelated reason. The `let _ =` makes it
   look safe. Worth a compile-time assertion or a comment at the call site.

2. **↑ on a multi-line draft diverges between the renderers.** Classic binds ↑
   to `LineUpOrPreviousHistory`, which moves the cursor *within* a multi-line
   draft and only walks history when there is no line above;
   `TypeaheadEditor` has no intra-draft line motion and always walks. This is
   **reachable** — a bracketed paste submits a multi-line draft as one entry,
   and rustyline escapes the newline into the file — so multi-line history
   entries are real. The test author deliberately wrote **no** failing test,
   because the inline viewport is **one row** and cannot display a multi-line
   draft at all, and pinned the boundary in a doc comment so nobody extends the
   walk by one ↑ and gets a false pass. This belongs with the bordered palette
   under **F16**/**F19** — height and placement — not with history.

   Labelled as hypothesis, per F43/F47: the *mechanism* was established by
   reading; whether a user's real history file contains such an entry was not
   tested through a terminal.

## F39 second occurrence — and both times our own tooling destroyed the evidence

F39 recorded one unidentified `--test cli` failure in seven full-suite runs,
whose name I lost **by re-running instead of saving the failing output**.

T3-S4 produced a second, in a different suite: the first
`cargo test -p kitsu --test tui -- --test-threads=1` of that session reported
**18 passed, 1 failed**, and four consecutive re-runs afterwards were 19/19
green. The agent reported it rather than burying it, and saved output on every
run after that — but the name was already gone, because **that run had been
piped through `tail -4`**.

**Two occurrences, two different suites, and in both cases the evidence was
destroyed by how we were looking at it** — once by re-running, once by
truncating the output through `tail`. That is the same family as this project's
standing rule never to assert absence from a `head`-truncated search: the habit
that makes output readable is the habit that throws away the part that matters.

**The rule, now with two data points behind it:** when running a suite that
could plausibly fail, write the full output to a file **first** and read the file
— never pipe the only copy through `head`, `tail` or `grep`. Both of this
project's unidentified failures would have been diagnosable under that rule.

Neither occurrence has recurred, neither is characterised, and both remain open.

## F50 — A8's 250 ms latency requirement has no test, and "shipped" was true only of the mechanism

Found while scoping T3-S5, by searching rather than assuming. Requirement **A8**
says the ambient background-agent counter "MUST update within 250 ms of a
registry change". That requirement is **satisfied in practice and asserted
nowhere.**

- The mechanism: `sync_agent_counts` runs once per input poll tick
  (`main.rs:15071`), and the tick is `libc::poll(..., 50)` (`main.rs:15084`) —
  50 ms, comfortably inside the budget.
- The coverage: searches for `250 ms`, `250ms` and `A8` across `crates/` return
  **nothing**. The five `agents_tag_*` tests pin the tag's **content** — the
  truth table, blocked outranking running, hidden when both are zero — and not
  its **latency**. No test asserts the poll interval, and nothing fails if
  someone raises it to a second.

**The distinction worth keeping:** "A8 is shipped" was true of the mechanism and
false of the test, and only a search established which. A requirement whose
number appears nowhere in the test suite is a requirement nobody is holding.

This is the same shape as **F49**'s latent `load_history` panic and **F47**'s
erased line: a property that holds today by a construction nobody wrote a test
against. The cheap fix is a unit assertion on the poll interval constant plus a
comment tying it to A8, so raising the interval fails loudly rather than
silently breaking a numbered requirement. Not taken in T3-S5, which was told not
to re-test A8; worth a small card.

## F51 — with tools enabled, an ordinary answer does not stream; it arrives in one burst

Measured during the status-row wiring card, then traced. The agent observed that
in inline mode a streamed answer reaches the terminal as a **single burst after
the turn ends** rather than progressively, and **labelled it an open question**
rather than guessing a cause. I read the path.

**The mechanism is F47's**, at `orchestrator.rs:1728`:

```rust
if cfg.stream && !tools_were_offered {
    kitsu_render::print_fragment(&t);
}
```

with a comment explaining that a streaming round can emit a tool call as JSON,
"we suppress that and only print real answers", and that "the final round
withholds tools, so final answers still stream live".

**The consequence, which is probably not what anyone intended.** That last
sentence is true of a round that *follows* a tool call. But an ordinary
single-round answer with tools enabled — **the default configuration** — has
tools offered in that very round, so `tools_were_offered` is true and **nothing
streams**. The user types a question, sees a spinner, and then the whole answer
appears at once. Visible streaming happens only *after* a tool call, in the
follow-up round that withholds tools.

So the product streams least in the common case and most in the uncommon one.

**It is pre-existing, deliberate at the line level, and identical in classic
mode** — this is not a regression and not the TUI's doing. F47 found the same
branch from the other side: it is why a classic streamed answer's last line was
being erased by `print_message` rather than by the fragment path.

**Not fixed here**, because the suppression exists for a real reason (tool-call
JSON must not leak into the transcript) and the fix is a product decision rather
than a bug fix. The options worth weighing, none of them this card's:
- stream fragments while **withholding** text that parses as a tool call, which
  needs incremental detection and is the only option that keeps the default
  fast-feeling;
- offer tools only on rounds where they are plausibly needed, which changes
  model behaviour, not just rendering;
- accept it and say so in the documentation, so "streaming" is not promised
  where it does not happen.

**It also means one existing test's name overstates what it measures:**
`inline_mode_wide_stream_wraps_and_scrolls_without_touching_the_status_row`
samples what it calls mid-stream, and with tools enabled there is no mid-stream
to sample — the burst arrives whole. The assertion is still valid (a wide
transcript must not reach the pinned row) but it is not testing the in-flight
case its name implies. Worth renaming when someone next touches it.
