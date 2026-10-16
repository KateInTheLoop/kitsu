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

**The original condition is DISCHARGED, and replaced — corrected in T3-S10.**

The caveat used to read: this was measured at idle in cooked mode, so the
harness must be re-run once the input path owns raw mode, and the answer must
not be quoted as unconditional.

**It was re-run, and this finding did not know for seven cards.** The test is
`inline_mode_raw_idle_input_does_not_staircase_command_output`
(`crates/kitsu/tests/tui.rs`, "Test 6. The staircase question, re-asked for raw
input mode"). The mechanism: `RawStdinGuard::engage` clears only `c_lflag`
(`ICANON`, `ECHO`, `ISIG`) and **leaves `c_oflag` alone**, so `OPOST`/`ONLCR`
still supplies the carriage return the direct writers omit. The answer may now
be quoted for **both cooked and raw idle input**.

Verified in T3-S10 that every `tcsetattr` in the workspace preserves that:
`RawStdinGuard::engage` (`c_lflag` only), `read_secret_tty` (`ECHO` only), the
dying-signal handler (restores a snapshot taken *before* raw mode), and the
in-loop approval read, which reuses `RawStdinGuard` and has no termios of its
own.

**The condition that IS still live:** the answer depends on nothing in `kitsu`
ever entering **crossterm's** raw mode, which *does* clear `OPOST`.
`kitsu_tui::TuiRuntime::enter` calls `crossterm::enable_raw_mode()` and has **no
caller in `kitsu`** — a standing fact already recorded in `tests/tui.rs`. **The
day someone wires `TuiRuntime` in, the staircase question reopens for every
direct writer**, which is also the day N5's title and alt-screen clauses stop
being vacuous. Both are T5's problem, and they arrive together.

**The count drifts upward and no test asserts it:** 392 when this finding was
written, **421** in T3-S10 (`eprintln!` 366, `println!` 43, `eprint!` 10,
`print!` 2, tests excluded). A count assertion was deliberately **not** written —
see the reasoning in N4's limitation list, since 132 of those sites are on paths
where no viewport can exist.


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

## F52 — N4 said "memory" and specified lines; a single huge line is still unbounded

Found by the T3-S6 test author while deciding what the ring stores, and raised
rather than quietly patched.

**N4 reads "Memory in `fullscreen` MUST be bounded by a configurable cell ring
(default 10 000 lines, oldest evicted)".** Those two clauses are not the same
claim. The ring caps **lines**; memory is lines × line length, and nothing
bounds the second factor. Ten thousand lines of unbounded width is unbounded
memory, so the requirement's guarantee held only under an unstated assumption
about line length.

Storing **styled** bytes makes each retained line longer than its plain form
too, which is the right call for other reasons (the ring is the only copy, and
styling can be stripped but never reconstructed) but does widen the gap between
"10 000 lines" and any particular byte figure.

**Why it was not simply fixed.** A per-line byte clamp is cheap to write and
would make the bound literal — but the ring is **the sole source for the
transcript overlay and the `[` dump**, so clamping truncates content nothing
else retains. That is a product trade: a hard memory ceiling bought with silent
loss of the widest lines, which are often exactly the ones worth reading
(wide tables, long tool output). It deserves a decision, not a patch slipped
into an unrelated card.

**Amended rather than papered over:** N4 now says the bound is in lines, states
that a pathological line still costs what it costs, and names the clamp as a
deliberate trade rather than an oversight.

**Options, for whoever takes it:**
- a per-line byte clamp with a visible marker on truncated rows, so the overlay
  shows that something was cut rather than silently shortening it;
- a byte-budget ring that evicts on total bytes rather than line count, which
  makes the bound literal and the line count variable — closer to what N4's
  first clause promises;
- leave it, and rely on the line bound plus the fact that terminal-destined
  output is usually already wrapped to the terminal's width.

**Two cautions for whoever writes the clamp**, from the implementer that
deliberately did not:
- **Truncating styled bytes can cut an SGR sequence in half**, leaving the
  terminal in whatever colour the severed escape implied. A clamped line must
  either be cut at a character boundary **and** have a reset appended, or be
  sanitized before truncation — which forfeits the "styling can be stripped but
  never reconstructed" property, though only for over-long lines.
- **The clamp must be reported.** An overlay that silently shows a shortened
  line is exactly the kind of quiet loss N4 exists to prevent. The assertion
  worth writing is *"a clamped line is marked"*, not merely
  `len() <= cap` — a length test passes for an implementation that lies about
  what it kept.

The general lesson is the one this epic keeps producing: **a requirement that
states a guarantee and a mechanism in one sentence can be wrong about the
guarantee while being right about the mechanism**, and only someone implementing
the mechanism notices.


### Updated when prompts entered the ring: F52 now has an adversarial input

Every line in the ring used to originate in **kitsu's own renderer**, so its width
was bounded by what kitsu chose to print. Feeding the user's prompts changes the
input class: **the ring accepts user paste, which is the one input nothing in the
process limits.**

Splitting a prompt on `\n` (required anyway, because both frames are one
`fit_row_to_width` per cell) handles a many-line paste. It does **not** handle **a
single multi-megabyte line with no newline**, which becomes one unbounded cell
inside a ring whose bound is counted in **lines**.

So F52's open question is no longer theoretical, and the counter-example is now
reachable from the keyboard rather than requiring a pathological session. **Still
needs a human**, but it should be decided before the bound is relied on.
## F53 — `--plain --tui inline` installs a viewport that plain output cannot use

Found by the T3-S7a implementer, which asked rather than deciding for itself.

The inline viewport is installed on **mode alone**: the block at
`main.rs:16585` resolves `[tui] mode` and calls `tui_bridge::install`, with no
condition on styling, `--plain`, or assistive output. So `--plain --tui inline`
really does install the viewport, pin a scroll region, and paint a status row —
while every *content* path takes the plain, unstyled branch.

For approvals that means **R7 is not satisfied in that combination**: the plain
prompter keeps its own `permission> …` text on stderr and never paints the pane
modal. Routing it through the shared composer would have "fixed" R7 by changing
plain's bytes, and `--plain` is a **byte-contract tier** — a weaker parity
requirement must not be bought with a stronger stability guarantee. The
implementer flagged it instead of trading them, which was right.

**The combination is incoherent beyond approvals.** `--plain` exists for
screen readers and byte-stable capture; a pinned pane, a DEC scroll region and a
repainting status row are the opposite of that. The viewport's own status row
*does* get painted in this mode, which is already questionable on the same
grounds.

**The likely right fix is mode resolution, not more branches:** have `--plain`
(and assistive output) resolve to `classic` regardless of `[tui] mode`, so the
contradiction cannot be configured. That would make R7 vacuously satisfied
there, delete a reachable-but-unsupported combination, and remove the need for
every future TUI surface to carry a plain-mode branch. It is a small card and it
should come with a test that `--plain --tui inline` is byte-identical to
`--plain` alone.

Recorded rather than fixed because it is a **mode-resolution decision** with its
own acceptance, and T3-S7a had no mandate to change which renderer a flag
selects. R7 has been clarified to state the exclusion explicitly rather than
leave it implied.

## F54 — "Ctrl+C is swallowed at an approval prompt" is measured, but its cause is probably the harness

The T3-S7b test author measured, honestly and reproducibly, that sending `\x03`
at an approval prompt does **nothing**: it is echoed as the two characters `^C`
and left in the line buffer, so `\x03` followed by `/quit` is read as the single
answer `^C/quit`, rejected as invalid, and denied. It reported this as a
pre-existing UX hole identical in classic and inline.

**The measurement is real. The stated cause is probably wrong, and I nearly let
it through as a product defect.**

The code's intent is explicit, at `main.rs:14668-14673`:

> ISIG is cleared so Ctrl+C reaches the editor as a byte … **Approval handoffs
> restore the original termios (ISIG on), so approval prompts keep kernel
> Ctrl+C.**

And `RawStdinGuard::drop` really does `tcsetattr(…, &self.original)` — the
snapshot taken before `ICANON|ECHO|ISIG` were cleared. So at the approval read
`ISIG` should be **on**, and `\x03` should raise `SIGINT`.

**The likely explanation is the test harness, not the product.** The observed
combination — `^C` **echoed** (so `ECHO` on) but **no signal** (so no SIGINT
delivered) — is exactly what happens when the terminal discipline is restored
correctly but the child **is not in the pseudo-terminal's foreground process
group**. The kernel delivers `SIGINT` to the foreground group of the controlling
terminal; a child that never called `setsid`/`tcsetpgrp` on the slave does not
receive it, and `\x03` stays an ordinary byte. The harness spawns the child with
the slave on all three descriptors and does not establish a session.

**Two competing explanations, neither yet settled:**
1. **Harness artefact** (my hypothesis, from the echo-without-signal signature
   and the absence of any session setup): real users at a real terminal *do* get
   Ctrl+C at approvals, exactly as the comment claims, and nothing is broken.
2. **Real defect**: something between the handoff and the read leaves `ISIG`
   off, and the comment documents an intent the code does not achieve.

**Why this matters more than the bug would.** If (1) is true and someone
"fixes" the hole — by mapping `\x03` in the reader, say — they would be adding a
second Ctrl+C path that only exists because a test could not deliver a signal,
and real terminals would then have two. Worse, a test asserting the mapped
behaviour would **lock the artefact in**.

**How to settle it, cheaply:** check whether the harness's child is the
foreground process group of the slave (`tcgetpgrp` on the slave versus the
child's pgid), or run kitsu by hand in a real terminal and press Ctrl+C at a
permission prompt. One minute of either answers it. **Nobody should write code
against this until that is done.**

The general lesson, and it is the mirror of F43: **a measurement is evidence of
behaviour, not evidence of cause.** F43 recorded a mechanism nobody had traced;
this is a trace that stopped one layer short — the agent proved *what* happened
inside its harness and then attributed it to the product, which is a different
claim requiring different evidence.

## F55 — an unfair mutex starved the in-loop approval read, and the hazard was already documented

Hit and fixed inside T3-S7b. Worth recording because the bug was **predicted by a
comment already in the file**, and because it is the exact failure the
rendering/protocol split was created to isolate.

**The bug.** The first version of the in-loop approval read serialized on
`APPROVAL_INPUT_LOCK` — the mutex the existing claim path uses. In **release
only**, three approval tests (two of them T3-S7a's, i.e. previously passing)
timed out at the first script step with the modal never painted. Cause: the
typeahead collector `try_lock`s that mutex **every 50 ms tick**, and
`std::sync::Mutex` is **unfair**, so the prompter — which deliberately publishes
no claim — was starved indefinitely. Release suite: **107.96 s with 3 failures**
before, **12.4 s with 0** after.

**The hazard was already written down.** `ApprovalInputGuard::enter`'s own
comment describes precisely this: "the type-ahead loop can observe false … and
reacquire it indefinitely". The first implementation reused the lock *because*
it looked like the right lock, and walked into the failure the comment exists to
warn about.

**The fix** is a separate `APPROVAL_PANE_READ_LOCK` that the collector never
touches, documented at the static with the measurement. Across the
publish-and-block the prompter holds **only** that mutex, does not hold
`APPROVAL_INPUT_LOCK`, and does not increment `APPROVAL_INPUT_ACTIVE` — so the
collector never drops its guard and never waits on anything the prompter owns.

**Three lessons, in order of how much they will save someone:**
1. **A lock that "is clearly the right lock" may be the wrong one when one side
   polls it.** `try_lock` in a tick loop is not a fair queue; it is a repeated
   coin-toss that a blocked waiter loses forever. Any mutex touched by a polling
   loop should be treated as unavailable to anything that needs to *wait*.
2. **Release-only concurrency failures are real and cheap to miss.** All three
   failures reproduced in release and none in debug — the tick loop simply wins
   the race more reliably when the code is fast. A card gated only on a debug
   run would have shipped this.
3. **The bounded test is what turned a hang into a diagnosis.** Every PTY test
   in this suite dies at a 20-second deadline with the whole transcript
   attached, so the starvation surfaced as three named failures with evidence
   rather than as a suite that hung. That bound was worth insisting on.

## F45 closed — the duplicate command answer parser is retired

F45's one-parser rule had a violation nobody had noticed: `CliCommandPrompter`
carried its **own** inline `match` on `y|yes|a|always|n|no`, with its own
trimming, bracketed-paste stripping and lowercasing, rather than calling the
shared `parse_permission_answer` that the filesystem prompter and `/jobs reply`
both use. The T3-S7b test author found it while checking how a keypress should
map onto the shared parser.

It is now retired: the command prompter calls `parse_permission_answer` and maps
`Decision → CommandDecision`, with the invalid-answer branch unchanged.
Behaviour-identical by inspection — same trims, same lowercase, same three
answers — and confirmed by the unchanged binary, CLI and command-modal suites.

**One parser now serves all three answer surfaces**: the filesystem approval, the
command approval, and `/jobs reply`. The finding that named the rule is finally
true of the code.

## F56 — a signal handler cannot share the restore writer, so the bytes are duplicated — and now compared

T3-S8's handler needs the restore sequences as a **precomposed `const`**,
because a signal handler may not write through `io::Write` (it locks and may
allocate) or call into crossterm. The restore helper that the panic path uses
writes the same four sequences through a `Write`. So the byte string exists
**twice**, by necessity rather than carelessness.

The implementer flagged exactly this and declined to add the test itself, being
restricted to production code. It was right to flag it: **two copies of one byte
sequence with nothing comparing them is this project's most-repeated defect
shape** — F2's suggester table, F38's four unchecked string literals, F45's two
vocabularies, and the `/agent` args string that was stale in three files at
once.

**Closed at review** with a test asserting the handler's `const` equals the
helper's output byte-for-byte, and **verified to have teeth**: dropping the
title-pop sequence from the handler made it fail with both strings printed, and
restoring the file by checksum made it pass again.

The failure it prevents is specific and nasty: a sequence added to the helper
and forgotten in the handler would mean **a signal exit restores less than a
panic does**, which nobody would notice until a user's terminal came back
half-fixed from one path and whole from another.

**The general rule, now with five instances behind it:** when duplication is
genuinely forced — different calling conventions, different safety constraints —
the duplication is acceptable **only if a test compares the copies.** The copies
cannot be eliminated; the drift can.

## F57 — a registered-but-undispatched top-level command is silently wrong twice, and the suite says nothing

Measured in T3-S9, not reasoned about. The author added a `SLASH_COMMANDS`
entry with **no dispatch arm** and ran the suites: the binary prints
`unknown command: /tui - did you mean /quit?` and **all 824 binary tests and
245 CLI tests pass.** Nothing asserts that a registered command is reachable.

**The second failure mode is worse than the first.** The mid-turn dispatcher's
default fall-through is `queue.enqueue(line)`, so a registered-but-undispatched
command typed **during a turn** does not merely fail — it becomes **a future
prompt sent to the model.** The user types `/tui inline`, nothing visibly
happens, and the model is later asked to respond to the literal text
`/tui inline`.

### Why the F2-style guard cannot be copied here

F38's `/jobs` fix worked because `closest_jobs_subcommand` kept its **own
hardcoded table**, so "every dispatched subcommand is its own nearest
suggestion" catches a table that drifted from the dispatcher.

`closest_slash_command` **reads `SLASH_COMMANDS` directly**. So the analogous
sweep — every registry name is its own nearest suggestion — **passes vacuously**:
it compares the registry with itself. The registry being the single source,
which is normally the fix, is exactly what makes this guard impossible.

**The two halves therefore need separate guards**, which T3-S9 adds:
- a missing *entry* is caught by the registry tests and the suggester test;
- a missing *arm* is caught by behavioural tests asserting
  `unknown command: /tui` does **not** appear, and by a unit test asserting the
  mid-turn queue stays **empty**.

### The general lesson

**A single source of truth guarantees that readers agree with it; it guarantees
nothing about whether anyone acts on what they read.** Six cards in this epic
converged on "one source, many readers" as the fix for drift — and this is its
blind spot: the registry can be perfectly consistent with every surface that
*displays* it while nothing dispatches it. Reachability is a different property
from consistency and needs its own test.

Worth a later card: a test that drives **every** registry entry through the
dispatcher and requires none of them to reach the unknown-command fallback. That
would make reachability structural for all 54 commands rather than only the one
this card happened to add.

## F58 — an UNSATISFIABLE test nearly changed the product, which is the mirror of a vacuous one

T3-S9's `every_registry_fed_surface_shows_the_tui_switch` asserted:

```rust
let palette = slash_palette("tu", false, 80);
assert!(palette.contains(" /tui [classic|inline]"), …);
```

At width 80 the palette takes its **wide** branch, which renders
`" /{name:<22} {description}"` — the bare **name** and the description. The
asserted substring is the **compact** branch's shape, used only below a
40-column inner width. So **no registry entry whatsoever could satisfy that
assertion at width 80.** It was not failing because `/tui` was missing; it was
unsatisfiable by configuration.

**The implementer could not edit tests, so the only path open to it was to
change the product** — and it did, making the wide branch show syntax in the
label slot when it fits. That change was careful and well-reasoned, and it
**altered the palette for every short-argument command** (`/model <name>`,
`/theme [name]`, `/usage [session]`, `/undo [id]`) in a card about a renderer
switch. It flagged the deviation rather than burying it, which is why this is a
finding and not a regression.

**Reverted at review; the test was corrected instead** to assert the wide
branch's real shape (the name and the description both present). The intent —
"the palette offers `/tui` for the prefix `tu`" — is served exactly as well, and
the palette is byte-for-byte what it was.

### Why this is worth a finding of its own

This project has **seven** findings about **vacuous** assertions — tests that
can never fail. This is the mirror image: an assertion that can never *pass*.
The failure modes are opposite but the cause is identical — **an expectation
written from a mental model of the output rather than from the output** — and
the consequences differ in an important way:

- a **vacuous** test fails to protect, and is discovered late or never;
- an **unsatisfiable** test is discovered immediately, but **it exerts pressure
  on the product**, because the implementer's job is to make it pass and the
  test is the specification.

So an unsatisfiable test is the more dangerous of the two in a TDD loop. The
protection is the same one that catches vacuity: **derive the expectation from a
real sample of the output**, not from what the output ought to look like. T3-S9
did that correctly for its transcript markers — the author verified the marker's
exact bytes and its 76-column width against a real run — and then did not do it
for this one line.

**Rule: when a test demands something the design does not do, the test or the
requirement is amended deliberately — the product is not changed incidentally
to satisfy it.** An implementer forbidden from touching tests must flag the
conflict, as this one did, rather than resolve it in the product.

## F39 third occurrence — and the first one with a name and a shape

The two earlier occurrences were unidentified, and in both cases the evidence
was destroyed by how we looked at the output: once by re-running (my mistake),
once by piping the only copy through `tail`.

T3-S10's implementer hit a third, **and this one is identified**:
`test_scripted_and_jsonl_runs_are_byte_identical_under_the_default`
(`crates/kitsu/tests/cli.rs`) failed once in a workspace run, with one side
producing only `"FOREGROUND_DONE\n"` instead of the full scripted transcript.

**It is a shape, not a mystery.** One side finished and reported before the
background agent's output had joined, so the comparison ran against a truncated
transcript — a **join-timing** difference, not a byte-content difference. That is
a far more actionable description than either earlier occurrence got.

**It handled it correctly**, which is why the diagnosis exists: it did not
re-run to make it pass. It verified the test passes standalone, verified a
workspace run with its change **stashed** also passed, and reasoned that the
code it touched is reachable only from tests (`with_cell_ring` has no production
caller) so it cannot influence the CLI harness at all.

**What this suggests about the family.** All three occurrences are in suites that
drive real child processes and wait on asynchronous completion, and this one's
shape — comparing output before a background contributor has joined — would
explain an intermittent byte-identity failure without any product defect. The
earlier two may well be the same thing wearing different names.

**The actionable fix is in the test, not the product:** the byte-identity
comparison should wait on a completion signal from the background agent rather
than on the foreground's own done-marker. Worth a small card, and worth checking
whether the other byte-identity tests share the pattern.

## F59 — `cargo check` runs build scripts, so "no linker needed" was wrong

The T3-S11 plan row asserts the Windows check needs no toolchain beyond the
target:

> `rustup target add x86_64-pc-windows-gnu && cargo check --target x86_64-pc-windows-gnu -p kitsu` (**no linker needed**)

**Measured in T3-S11: the target installs fine, and the check fails.** Not at
link time — `cargo check` indeed does not link — but at **build-script** time:

```
error: failed to run custom build command for `ring v0.17.14`
  CC_x86_64-pc-windows-gnu = None
```

`ring` compiles C, so its build script needs a MinGW cross-compiler
(`x86_64-w64-mingw32-gcc`), which is not installed here. `cargo check` skips
**linking**, not **building dependencies' build scripts** — those still run, and
a build script that invokes a C compiler needs that compiler for the target.
`ring` arrives via `kitsu-providers`, i.e. through TLS, not through anything the
TUI touches.

### But the part N6 actually cares about is green, and that was worth measuring

The renderer and viewport crates do not depend on `ring`, and all three
**compile cleanly for `x86_64-pc-windows-gnu`, zero errors**:

| crate | `cargo check --target x86_64-pc-windows-gnu` |
|---|---|
| `kitsu-tui` (holds `viewport.rs`) | **clean** |
| `kitsu-render` | **clean** |
| `kitsu-editor` | **clean** |

So **the viewport code is already Windows-portable**, and what blocks the
whole-binary check is a TLS dependency's C build script — nothing in the
terminal work. That is a much more useful statement than "the Windows check
fails".

### Consequences

- The plan row's parenthetical should read **"needs a MinGW cross-compiler for
  `ring`'s build script"**, not "no linker needed".
- The `cfg(unix)` narrowing the row also asks for (4 gates in `tui_bridge.rs`,
  85 in `main.rs`) **cannot be verified here**, because `tui_bridge.rs` lives in
  the crate that cannot be checked. Narrowing a platform gate without a
  compiler to prove it is guessing, so it was **not attempted** — it belongs
  with whoever has the cross-compiler, or with CI.
- `viewport.rs` has **zero** `cfg(unix)` gates already, which is consistent with
  it checking clean.

**The reusable lesson:** `cargo check` is not a pure type-check across targets.
It is a type-check *plus* every dependency build script, and a build script is
arbitrary code with arbitrary toolchain needs. A cross-target check is only as
portable as the least portable build script in the tree.

## F39 fourth occurrence — the shape is now confirmed, with the exact extra lines

T3-S10 recorded the third occurrence and proposed a shape: a byte-identity
comparison running before a background agent had joined. T3-S12 hit the **same
test** again under full-workspace load and produced the detail that confirms it.

`test_scripted_and_jsonl_runs_are_byte_identical_under_the_default` failed once
with the scripted run carrying **`joining them`** and **`joined 1 background
agent`** lines the JSONL run did not. Under load the detached agent had not
finished by input end, so one side joined and narrated it while the other had
already finished.

**So this is no longer a mystery and no longer a product concern.** It is a test
that compares two runs whose **background-agent lifetimes are not
synchronised**, and under load their join points diverge. The two unidentified
earlier occurrences are plausibly the same thing; both were in suites driving
real child processes.

**It passed alone and the clean full-workspace re-run was 2178/0**, and the
implementer did not re-run to make it pass — it diagnosed first, which is how
the detail exists.

**The fix is in the test, and it is now specific:** either join the background
agent deterministically before comparing, or exclude the join narration from the
comparison. Worth a small card, and worth checking the other byte-identity tests
for the same pattern — four occurrences across three cards is frequent enough to
cost someone an afternoon eventually.

## F60 — T3's exit gate has a clause with no test, and the harness cannot express it

Found while running T3's exit gate clause by clause in T3-S13. The gate reads:

> per-fd sanitize equality on the streamed two-tool PTY run; all fixtures
> byte-identical in classic/plain/JSONL; both profiles; `cargo test --locked`;
> Windows `cargo check`.

**The first clause cannot be run, for two independent reasons.**

1. **No two-tool-call streamed PTY run exists.** The nearest test streams
   **one** tool call, in **classic only**, and asserts screen order and a
   request body — not cross-renderer equality. `tool_call(` appears at four
   sites in the terminal suite, none of them two calls in one streamed turn.
2. **Per-fd separation is structurally impossible in the harness as built.**
   `run_styled_chat_pty_core` hands the child **three `dup`s of one pty slave**,
   so stdout and stderr *are the same descriptor* and the harness reads one
   interleaved stream. There is no "inline's stdout" to compare against
   "classic's stdout".

**What passes and must not be mistaken for it:**
`inline_mode_commits_the_same_permission_receipt_as_classic` asserts sanitize
equality of the `◉ permission —` receipt between renderers. That is **R7's**
risk gate — one line, on one fd-blind stream. It is good evidence that the two
renderers agree about *that* receipt, and it is not this clause.

**The card to write**, specified so nobody has to rediscover it: allocate **two**
ptys, stdout on one and stderr on the other, each `TIOCSWINSZ`'d and both real
ttys so the inline gate still opens; add a streamed turn with **two** tool calls
to the streaming harness; then assert sanitized stdout and sanitized stderr
**separately**, classic against inline.

**Why this was worth finding rather than assuming.** An exit gate is the thing
that decides a milestone is done. Four of its five clauses ran and passed; this
one would have been read as passing because adjacent tests with similar names
do. The lesson is the same one F57 produced from the other direction: **a gate
is only as real as the test that implements it, and "there is a test about
this area" is not that test.**

## F61 — F58's second instance: a test unsatisfiable because it contradicted an invariant pinned elsewhere

**F58** recorded an assertion that could never pass, satisfied by changing the
product. T4-S1 produced the same family in a sharper form, and this time the
implementer **stopped** instead of resolving it.

The test tabulated all **fifteen** `(phase, handoff)` pairs and, for each,
asserted both the classifier's answer *and* that a fixture DTO for that pair
**validates**. But `AgentThreadSnapshotDto::validate` requires
`terminal == timeline.closed`, a handoff **iff** terminal, and no open steering
once terminal — so only **eight** of the fifteen are representable at all. And
an existing, green test, `validate_treats_blocked_as_non_terminal`, pins exactly
that: *"a blocked thread has no handoff"*.

**So the new test and an existing test made contradictory demands of the same
function**, and the existing one was in the must-stay-green set. No
implementation could satisfy both. The implementer diagnosed it precisely,
named the conflicting test, said the fix belonged in the test rather than the
invariant, and **stopped** — which is the behaviour the brief asks for and the
reason this cost one review instead of a loosened invariant.

### The fix, and why it is narrower than shrinking the table

Two different properties were tangled in one loop:

- **the classifier is a total function over two enums**, and asserting all
  fifteen is *correct* — defensive totality is cheap and right;
- **the DTO forbids seven of them**, so a fixture cannot be valid for those.

So the classifier assertions stayed at fifteen and only the **fixture validity
and row mapping** were confined to the eight representable pairs, with the count
asserted (`checked_dtos == 8`) so the guard cannot silently skip everything, and
a second assertion that those eight still reach **all six** A2 groups — through
a DTO the validator accepts, not merely through the classifier.

**The fixture was also wrong independently**: it hard-coded `steering_open: true`
and `closed: false`, so it was only a valid DTO for non-terminal phases. It now
derives both from the phase. That is the deeper bug — a fixture that cannot
represent the states its own test enumerates.

### The lesson, beyond this instance

**An unsatisfiable test has two sources, and the second is worse.** F58's was a
mental model of the output. This one is a **conflict between two
specifications**: a new test asserting what an older test forbids. The first is
caught by deriving expectations from real samples. The second is only caught by
noticing that the invariant is *already pinned somewhere* — which is why
"stop and report" must stay available to an implementer, and why forbidding test
edits is what made the conflict visible instead of silently resolved in the
product.

The arithmetic also deserves a note: the first estimate of representable pairs
was **eleven**, by counting only the non-terminal-with-handoff cases and missing
terminal-without-handoff. The real count is **eight**. A guard built on the wrong
number would have skipped nothing and still failed; the count assertion is what
makes the number checkable rather than assumed.


## F34 and F3 CLOSED — the blocked marker, and one vocabulary for both surfaces

Both closed in the marker story, taken between T4-S1 and T4-S2 because the gap
had to shut **before** the agent-view overlay rendered rows or the overlay would
ship it too.

**F34 — a blocked agent no longer looks like a busy one.** `DeckState::Blocked`
exists, with `◉` in the styled tier (the approval glyph, which §4.4 itself had
nominated, and apt because a blocked agent **is** a pending approval), cream in
colour (the "look here" slot — reusing the accent would have left blocked
*colour-identical* to running and cured only half the finding), and `[blk]`
under `--plain`.

**F3 — one vocabulary, and §4.4 amended rather than obeyed.** The design banned
`◆`/`◇`/`◈` as row glyphs so a plain row could never read as `tool>` or
`result>`. The implementer read that reason before agreeing it should change,
and found the ban was **the wrong instrument**: `deck_marker` checks `plain`
**first** and returns a bracketed token, so a row glyph never reaches
`ascii_structural_markers`. The hazard is prevented structurally; the
prohibition was redundant — and the shipped deck had already broken it, using
`◈` for result-ready rows since D1. The view now reaches the deck's vocabulary
through `AgentViewState::deck_state()`: **one function, not two tables that
agree today.**

### The finding's own scope was wrong, and that is the lesson

F34 said *"the only test that changes is T2-S3's"*. The test author **counted
and found four**: two further production sites mapped blocked to running
independently — `format_agent_thread` and `format_agent_explain` — each pinned
by an existing test that strips the running marker off the output.

Closing only the deck path would have moved the defect rather than fixed it:
`/jobs list` and `/jobs view` showing `◉` while `/jobs show` and `/jobs explain`
still showed `◆`. **Inconsistency across subcommands is worse than a uniform
uninformative marker**, because it teaches a user to distrust the glyph
everywhere. The authorisation was extended to all four tests; each pinned the
**deferral**, not a behaviour.

Verified after the fact: **zero** remaining blocked-to-running mappings, and all
four sites return `Blocked`.

**A trap worth remembering**, flagged in advance and then hit: those explain
tests assert the body `is_ascii()` **after** stripping the marker. `◉` is
non-ASCII, so it satisfies that assertion *only because it is stripped* —
changing the enum without the `strip_prefix` fails the test on an ASCII
assertion that looks entirely unrelated to the change.

**Plain-mode bytes:** `[blk]` is added and **no shipped marker moved** —
pinned by a dedicated test so a sixth state cannot renumber the five.

**Loose end:** F34's text names the old test by its pre-rename name
(`the_blocked_deck_row_is_marked_live_and_reads_blocked`), which no longer
exists; the rename was correct, since "marked_live" was the claim being
retracted. Anyone following that reference should look for
`the_blocked_deck_row_is_marked_blocked_and_reads_blocked`.

## F62 — `[tui] alternate_screen` is the last key without a consumer, and the overlay bypasses it

T4-S2's overlay takes the **alternate screen unconditionally**. So
`[tui] alternate_screen = never` — a documented, parsed, validated value — has
no effect on the one surface it was introduced for.

**Why it happened, and why I accepted it.** The alternate screen is what makes
the close assertion true *by construction*: the transcript stays on the primary
screen, so the restore is the terminal's own and the closed screen equals the
pre-open screen row for row, while the open screen necessarily differs. An
in-viewport overlay would have to repaint the region itself, and its restore
would come from **the cell ring** — which is not fed in production (**N4**,
**F52**) and carries a documented limitation list. So the `never` path depends
on a reader that does not exist yet.

**The honest status**, rather than calling the key done: of the six `[tui]`
keys, `mode` (T3-S2), `prefix` and `left_arrow_opens_agents` (this card) have
consumers; `cell_ring_lines` has a ring that nothing reads until R6; `title` and
`alternate_screen` have none. Three cards have now bound keys "so they are not
silently unreachable" and left them inert — a pattern worth watching, because a
validated key that changes nothing is indistinguishable from a broken one from
the user's side.

**What closing it needs:** an in-viewport overlay path, which wants the ring fed
(R6) so the restore has a source. That makes it T5 work, not a small follow-up.
Until then the design says plainly that the `never` path is unexercised.

## F63 — three T4-S2 behaviours the requirement did not specify, decided and recorded

A1 specified *that* `←` opens the view on a second press. It did not say what
**disarms** the first press, what happens to a `←` **inside a paste**, or what a
**malformed `[tui] prefix`** does. All three are user-visible and all three were
decided in T4-S2:

1. **Any keystroke other than `←` disarms.** The alternative — arming until the
   view opens — makes a keystroke's meaning depend on something the user did
   minutes ago and can no longer see. Typing and deleting back to empty requires
   two presses again.
2. **A `←` or a prefix byte inside bracketed-paste content is content.** The
   byte filter tracks the paste delimiters for exactly this reason, and holds a
   sequence split across two reads so the editor sees the same stream either
   way. Without that, pasting text containing the chord byte would open the
   overlay mid-paste.
3. **An unrecognisable prefix spelling yields no chord route at all**, rather
   than falling back to the default. A silent rebinding to a chord the user did
   not ask for is worse than a chord that does nothing, because the user cannot
   tell the config was rejected.

Recorded because each is the kind of decision that gets re-litigated by the next
person to touch the reader, and because the *reasons* are the durable part — the
behaviours are easy to re-derive once the reasoning is written down.

## F64 — I have now propagated an unverified mechanism into a brief twice, and an agent caught it both times

**F43** recorded the first instance: I wrote that a cancelled approval was "recorded as timed out", taking the mechanism from an implementer's report without tracing the path. The agent I briefed read the code, found cancellation never reaches that path at all, and said so instead of building what I asked for.

**T4-S3 is the second, and the shape is identical.** The plan row's acceptance says *"unviewed failed agent still refuses"* and I repeated it in the brief as "the guard refuses to quit while a finished agent's result is unobserved". **Verified false:** the only refusal on the quit path counts `Running | Blocked` agents, so a finished-but-unviewed agent does **not** block `/quit`. What protects an unobserved result is **surfacing** — the loop prints `~ surfaced N previously unopened background result(s) before exit` on the way out.

The agent **stopped rather than writing the test**, because a test asserting `/quit` refuses would have contradicted the quit path and the exit tests that pass today — an **F61**-shaped conflict, recognised as one.

### Why the error is easy to make here, which is the useful part

**The codebase itself calls this "the exit guard on unviewed results".** A doc comment on `agent_explain_view` says `/jobs explain` "never calls `result`, which acknowledges too, so diagnosing an agent cannot take custody of its result and defeat the exit guard on unviewed results." So the **name** is accurate and long-established; only the **mechanism** differs from what the name suggests. "Guard" implies refusal; this one surfaces.

That is a better explanation than carelessness, and it generalises: **a well-named mechanism invites you to assume its implementation.** The name was right, the inference from the name was wrong, and nothing in the row or the doc comment would have corrected it.

### The rule, now with two instances

**A mechanism named in a plan row, a requirement, or another agent's report is a hypothesis until someone reads the path** — and that applies to *my* briefs, which are the specification an implementer builds to. Both times the cost was one review round because the agent stopped; the counterfactual is a product change built to a false premise, which is what F58 and F61 are about.

Practically: when a brief asserts how something behaves, it should either cite the line that shows it or mark the claim as needing verification. This epic's briefs now routinely say "verified by me" against specific line numbers; the two failures are both places where I did not.

**The plan row's acceptance needs amending** from "an unviewed failed agent still refuses" to "an unviewed failed result is still surfaced at exit, and a peeked one is not" — or the quit path changes, which would be a different card with its own requirement.

## F65 — an arrow key is not one byte, and the overlay's close key ate the first one

**Found by a test, not asserted from a brief**, which is the part worth keeping.

T4-S4's PTY test pressed `↓` in the agent view and failed with *"`↓` is move, not an exit"*, dumping the pre-open banner. Cause: the overlay's byte loop matched `0x1b` as **close**, and an arrow key is **`ESC [ B`** — three bytes. So the first arrow **closed the view and fed `[B` into the composer underneath.** The view had been shipped (T4-S2) and peeked in (T4-S3) without this surfacing, because every key either card used is a single byte.

**This is the mirror image of T4-S2's trap.** There, at the idle composer, a lone `ESC` stays *pending* in the editor's escape state machine until the next byte, which is then consumed as part of the sequence — so `Esc` then `/quit\r` submits `quit` as a prompt. Same ambiguity, opposite failure: the editor waits where the overlay acted.

**The fix has to carry state across reads**, because a read can split a sequence: a pending-state decoder (`None`/`Escape`/`Csi`/`Ss3`), with `ESC` alone never acting, `[`/`O` opening a sequence consumed to its final byte, and a **50 ms grace** resolving a lone `ESC` as the `Esc` key when no byte follows — matching the editor collector's own poll tick. The `poll` retries on `EINTR`, so **a SIGWINCH arriving during an arrow is not read as a close**. Non-key bytes are dropped rather than forwarded, so no `[B` can reach the composer.

**The lesson for the remaining key cards (T4-S4b, T4-S5…):** every multi-byte key is a latent version of this, and single-byte keys will not reveal it. A card that adds a key should ask what else starts with the same byte.

## F66 — the agent-name limit is a renderer constant, and that is deliberate

`AgentRegistry::NAME_MAX_CHARS` is defined as `kitsu_tui::agents_view::AgentRow::TASK_HEAD_CHARS` (48), with the rationale stated where it is defined: *a name stands in for the task head on A2's row, so the budget is the head's.* The dependency direction is legal (`kitsu` → `kitsu-tui`), and the reasoning is sound — a name that fits the row and a task head that fits it are the same width.

**Recorded because it has a consequence a later reader should see coming:** a **validation** bound is now a **display** constant, so changing the row's head width silently changes **what names are legal**, and the `TooLong { max }` message reports a number that came from a renderer. Names stored before a narrowing stay stored and render truncated by the normal width fitting — they do not break — but they can no longer be re-entered as typed.

Not changed, because the coupling is intentional and documented, and splitting it would need a reason this card does not have. The note is here so that a future row-width change is recognised as a **product change to name validation**, not a layout tweak.

## F67 — pruning destroys an unviewed result, and the exit guard cannot know

**Read, not taken from a report** (F43/F64 discipline — the mechanism was handed to me by an agent and I traced it):

- `AgentRegistry::prune` does `agents.remove(&id)` once the record is non-terminal-free.
- `take_unobserved_terminal_results` iterates `self.agents`, so **a removed record is invisible to it**.
- Therefore pruning a finished agent whose handoff is still `Ready` **discards its result and the exit guard never mentions it** — there is nothing left to surface.

**The nuance that keeps this from being a plain bug, and which the first telling of it missed.** The two prune surfaces differ:

- **`kitsu agents --json` / automation** gets `terminal_job_payload(&status)` in the response, so the result **survives** in the reply.
- **An interactive human** gets only `~ pruned background job #{id}` on stderr. The result is **gone**, unprinted.

So the loss is **interactive-only**, and `/jobs prune <id>` is an explicit command naming a specific id — a user who types it has arguably asked for exactly this. That is why this is recorded as a **product question**, not filed as a defect.

### What is clearly right, and is pinned

**The `x` key must not inherit the behaviour**, because the second press of an arming is *not* an explicit id. T4-S4b therefore refuses to prune a terminal agent whose result is still unviewed, reports that the result is waiting, and prunes on the identical press once `Space` or `p` has collected it. **This asymmetry is the finding's practical content: the same registry call is safe behind a typed id and unsafe behind a keystroke.**

### The open question for `/jobs prune`

Three options, none taken here: leave it (explicit command, automation keeps the payload); **print the result before discarding it** (cheapest, and makes the interactive path match the JSON one); or refuse unless `--force`, matching the key. My own lean is the second — it costs one line, loses nothing, and removes the only path by which a result silently disappears — but it changes the output of an existing command, so it is a decision rather than a fix, and `--plain`'s byte contract makes any output change a breaking one.

## F68 — REJECTED: a pasted correction executes the rest of itself as overlay keys

**T4-S4b-i was rejected on review for this.** Found by reading the loop, not by a test, and not claimed from a report — the path is quoted below.

The overlay reads raw bytes and processes a whole chunk at once:

```rust
let count = match stdin.read(&mut buffer) { … };
for key in decode_overlay_keys(&buffer[..count], &mut pending) { … }
```

**The overlay does not enable bracketed paste** — that is a rustyline setting, and the overlay does not run rustyline. So a paste is indistinguishable from typing, and every byte of it is a key.

Paste `fix the bug\nand retry` into the new steer or rename line and: the first line fills the draft, the `\n` **submits** it and closes the prompt, and the remaining bytes of **the same chunk** are then executed as overlay keys — `a`, `n` (which opens the rename prompt), `d`, ` `, `r`, … 

### Why this is a rejection and not a note

Before this card the overlay bound `↑`/`↓`, `Space`, `?` and `Esc`; a stray paste could scroll or peek, which is harmless. This card bound keys with **effects**:

- **`r` then `y` answers a pending permission.** A pasted correction containing the letters `r` and `y` in sequence can **approve a filesystem write** on a blocked agent.
- **`x` twice stops an agent**, and on a terminal one prunes it.

So the card turns pasted text into privileged actions. That is a different severity class from anything the overlay did before, and it is **introduced by this card**, not pre-existing.

### What makes it a clean fix rather than a redesign

**This is exactly what `kitsu-editor` already solves** — it handles bracketed paste, split UTF-8 and the `ESC` ambiguity, and documents that a paste never submits. T4-S4b-ii was already scoped to route `s` through it. The hazard exists because this card bound text prompts to a **naive** line of its own instead, to satisfy the help test's "an advertised key must be bound" rule.

Two defensible resolutions, for the rework to choose with evidence:
1. **Guard the chunk** — once a prompt submits or an action key fires, the rest of that chunk must not be executed as commands.
2. **Leave `s` and `n` unbound** until they can use the editor, and remove them from the help; then also guard `r`'s decision read, since `r`+`y` inside a paste is the sharp case and needs no text prompt at all.

### The lesson, which is F65's generalised

**F65** was *an arrow key is not one byte*. This is the same root with the opposite sign: **a chunk of bytes is not a sequence of intentions.** A loop that treats every decoded key as a deliberate keystroke is safe only while no key does anything. The moment a key acts, the loop needs to know where the human's intent ended — and that is precisely the knowledge a line editor carries and a raw byte loop does not.

### F68, confirmed empirically — and it was worse than the reading suggested

I rejected the card from reading the loop. The test author then **reproduced it against the real code** (a temporary pass-through over today's `decode_overlay_keys`, since reverted). One paste of `"retry the exxtract\nand fix the bug"` as a single chunk produced, in order, from the loop's own arms:

```
cannot read e as an answer for background agent #1; press y, a or n
answered background agent #1: allow_once
press x again to stop background agent #1
stopped background agent #1
background agent #1 is not waiting on an answer
```

**One pasted correction approved a parked filesystem request and then stopped the agent.** `registry.answer(id, Deny)` afterwards returned `NotBlocked` and the row went `Working → Cancelled`. The decoder returned **34 keys** for that chunk. In the rename prompt the same paste left the draft as `"d fix the bug"` — the `\n` submitted, `a`/`n` reopened rename, the rest typed into it. The PTY half reproduced too: a bracketed paste containing the word "quit" **closed the overlay**.

So the severity was not hypothetical, and `allow_once` is the worst available outcome: a **write approval granted by text the user pasted as prose.**

### The resolution, and the one judgement left open

**Nothing in a paste is ever a key.** Inside a text prompt the whole payload including newlines is content and the prompt stays open; over the row list and inside `r`'s decision read a paste is **dropped entirely**. This keeps one answer to "what is a byte inside a paste?" across the binary — `kitsu-editor` already holds that a paste never submits, and T4-S2 decided a prefix byte inside a paste is content.

Paste is detected two ways because one is not always available: **bracketed** (the overlay owns the terminal, so it emits `ESC[?2004h` itself, as `tui_bridge::KeyFilter` does without rustyline) and **unbracketed burst** (a chunk yielding more than one printable key is not typing) — the latter being what actually blocks `r`+`y` on a terminal that does not report pastes.

**The burst rule is a heuristic and therefore has a false positive:** a fast typist, or a terminal that batches bytes, can put two printable keys in one read, so two quick `x` presses over the row list could be dropped. It **fails safe**, which is the right direction, but it collides with T2-S5b's standing rule that *a human must be able to tell "nothing happened" from "it worked"* — so a swallowed burst should say so rather than be silent. Left to the implementer with reasoning required.

### A trap worth keeping

**`ESC` alone is a strict prefix of `ESC[200~`.** A naive "hold the partial delimiter" implementation **silently kills F65's lone-`ESC` grace** — `escape_pending()` goes false and `Esc` stops working. Recognising `200~`/`201~` inside the existing CSI state is cleaner than prefix-holding. This cost the test author a design iteration and is pinned by explicit controls.

## F69 — a test model that mirrors a loop stops mirroring it, and nothing fails

Flagged by the implementer rather than hidden, which is the only reason it is visible.

The overlay's byte loop cannot be called without a tty, so F68's registry-level tests drive it through `OverlayUnderTest` (`main.rs` ~30800), a test-local model that mirrors `match prompt.take()` **arm for arm** — and whose own comment says that is what it is for.

T4-S4b-ii then changed the real `s`: an open steer line **bypasses `OverlayDecoder` entirely** and feeds raw bytes to the editor. The model still routes `s` to the naive `OverlayPrompt::Text { kind: Steer }` it replaced. So **the model's paste-into-the-steer-draft test now describes the model, not the product.**

### Why this is not a false alarm, and not a reject either

The behaviour it claims — a pasted newline is content and does not submit — **is still true of the real path, by a stronger route**: the editor enforces it as the one parser that owns bracketed paste, pinned by `pasted.feed(b"\x1b[200~one\ntwo\x1b[201~")` answering `Editing`. So coverage did not vanish; it **moved**, and the product is correct. Nothing was accepted on the strength of a test that passes while the product is wrong.

But the model is now a **mirror that no longer reflects**, and the next person to change `s` will read it as authoritative. The failure mode is specific: a test model diverging from the code it models **cannot fail** when they disagree — agreement is not among the things it checks.

### The family this belongs to

**F57**: *a single source of truth guarantees readers agree with it; it guarantees nothing about whether anyone acts on what they read.* This is the same hole with the pieces swapped — the model and the loop are two readers of one design, and nothing compares them. The only honest fixes are to make the model reachable from the loop (so divergence is a compile error) or to delete the model and pin the loop through the PTY, which a fixture session with zero agents cannot currently do.

**A nearby symptom, same cause:** `OverlayTextPrompt::Steer` is no longer constructed by production code at all — only by the model — so it now carries `#[cfg_attr(not(test), allow(dead_code))]`. **A production enum variant kept alive only by a test is a smell, not a fix.** Deleting it requires editing the model, so both belong to one follow-up card.

### Also recorded: my sixth brief error (F64's tally)

My brief said an empty correction is the registry's own `Empty` refusal **and not** a key-level guard. `agent_view_steer` **does** have a `correction.trim().is_empty()` guard — it just constructs the registry's own error so the wording is identical. Pre-existing, pinned by a passing test, left alone. The agent checked rather than believing me, which is now the sixth time across five cards.

### F69, sharpened by the refactor — the arm was never even run, and there were three of them

My write-up said the model's *paste-into-the-steer-draft test* now described the model rather than the product. **There is no such test.** The refactoring agent grepped every call site: the paste test drives `n`, the **rename** line, and **no test has ever fed `s`** to the model at all.

So `OverlayTextPrompt::Steer` was not kept alive by an assertion. It was kept alive by **one unexercised arm of a model that looked authoritative and was never executed.** That makes the diagnosis stronger, not weaker: a mirror that cannot fail is bad, and a mirror with arms nobody runs is worse, because reading it is the only way it ever gets used — and reading it is exactly what misleads.

**And there were two further divergences nobody had noticed**, both erased by the extraction and neither asserted:

- inside a text prompt the model **ignored `Backspace`**, where the real loop does `draft.pop()`;
- inside `r`'s decision read the model **kept the read open on `Esc`**, where the real loop closes it and clears the report.

Three divergences, zero assertions touching any of them. The count is the point: **duplicated behaviour does not drift once and get noticed, it drifts repeatedly and silently**, because nothing in the design is watching.

### The structural fix, and the gap it exposed

The loop's input dispatch is now one function, `handle_overlay_input`, that both the loop and the tests call; `run_agent_view_overlay` keeps only what needs a terminal (alternate screen, size, painting, `stdin.read`, the two 50 ms graces, and the steer line's raw-byte read with its decoder bypass). Faithfulness was established by **token-comparing the new body against `git show HEAD:…` of the old one** — identical apart from the deleted arm.

**The break-it experiment is the part worth keeping as a method.** Two deliberate one-line breaks in the real dispatch — truncating a paste to its first line, and dropping `n` from the action keys — both produced failures that *would have been invisible before*. A third attempted break did **not** fail anything: making a paste close `r`'s decision read is genuinely unasserted, because the paste test pastes while the frame is the row list and never after an `r`. That gap is **pre-existing**, not a regression, and closing it means adding an assertion — so it is recorded here as a candidate rather than smuggled into a refactor whose contract was that no assertion changes.

**A refactor that claims to remove duplication should be made to prove it**, and breaking the surviving copy is the cheapest proof there is.

## F39 — eighth occurrence, and it is now costing a gate every card

`test_scripted_and_jsonl_runs_are_byte_identical_under_the_default` failed again, in **release** `--test cli` (244 passed, 1 failed), while debug was 245/245 twice. Same signature as the seven before: the two runs' background-agent join points diverge, one narrating `joining them` / `joined 1 background agent` where the other does not. **A test synchronisation defect, not a product one**, and unrelated to the overlay work it interrupted.

Recorded because the pattern has changed character: it is no longer an occasional nuisance but **a red gate on most cards**, which trains the reviewer to wave a failing suite through — exactly the habit F43 and F64 are about. The test-side fix has been "open" for many cards and should be scheduled rather than deferred again.

One process note from this occurrence, reported by the agent rather than hidden: its per-suite output file was **overwritten by the next suite in the same loop**, so it could not name the failing test. F39's own rule is *write full output to a file first and read the file* — the rule needs the second half said out loud: **one file per suite**, or the evidence destroys itself just as surely as a re-run does.

## F39 — RESOLVED, and it was failing far more often than eight times

Scheduled after the eighth occurrence, on the grounds that its cost was no longer the minute it wasted but the review discipline it eroded.

### The diagnosis was right, and verified two ways

**In the code:** `join_background_agents_at_exit` (`main.rs:14066`) takes **one snapshot** — `let running = registry.running(); if running == 0 { return }` — and both narrations (`~ input ended with N background agent(s) running; joining them` and `~ joined N background agent(s) before exit`) hang off it. **In a captured failure:** the transcript diff is *exactly* those two lines and nothing else, with N1's `background_result` present and identical on both sides.

The race is structural: the detached agent needs one round trip to the mock, the foreground needs one more after `spawn_agent_bg` returns, and whichever lands first decides. Both narrations are correct for their timing — **a test synchronisation defect, not a product one.**

### It was 11 failures in 20 standalone runs

Run alone in release, the test failed **11 times out of 20**. The suite had been hiding the frequency behind serial ordering and load, so "eight occurrences" understated it by a wide margin. **A flake's observed rate is a property of how you run it, not of the flake** — which is worth remembering before calling anything rare.

### What was actually impossible, which is the interesting part

The obvious fix — drive the script through `/quit wait` so the agent drains deterministically — **does not work, and was measured failing 7 times in 20.** `await_background_agents` (`main.rs:14018`) has the *same* `if running == 0 { return }` early exit on the *same* snapshot, so the race simply moves to its own narration line.

**Every wait primitive the product exposes narrates conditionally on that one snapshot**, and `agent_result` does not block. So **no script-only sequence can make entry into a wait deterministic.** That left two options: rewrite the test's whole I/O shape (piped stderr, a watcher thread, a gated mock response, and a multi-thread runtime flavour because the detached request precedes the foreground's), or push the detached completion behind end-of-input with a margin.

### Why the margin was accepted, having forbidden sleep-based fixes

My brief said no sleep-based fix, and that a bounded wait must be bounded on an observable condition rather than on time. The accepted fix satisfies that, because the two roles are separate:

- **The synchronisation is condition-based and pre-existing product code** — the join already does `let mut changes = registry.subscribe(); while changes.borrow().running > 0 { … changes.changed() … }`. Verified by reading.
- **The 1 s mock delay only decides which of two valid orderings occurs**, so the code path that waits on the condition is always entered. It is not `sleep(); assert`.

Three things make it safe rather than merely convenient: the margin is ~200× (1 s against an end-of-input arriving in single-digit milliseconds); it sits well under the 15 s first join heartbeat, which would otherwise inject a variable `still waiting` line; and the test next door already uses the same device for the same purpose (`set_delay(30s)` to guarantee an agent is still running).

**The decisive mitigation is a new assertion, not the delay.** The fix *adds* a guard that both scripted runs must contain both join lines — so if the ordering ever inverts, the test **says so** instead of silently comparing two transcripts that no longer cover the join path. That guard would have failed about half the runs before the fix, which is independent confirmation of the diagnosis. **A time margin that is policed by an assertion is a different thing from a time margin that is trusted.**

Neither compared assertion changed by a byte; nothing was normalised out of the comparison.

### Tally

Baseline release 9/20 pass. `/quit wait` experiment 13/20. **Fix: 40 release and 10 debug runs, 0 failures**, each to its own numbered file, plus three full release suites at 245. My own independent check: 6 release runs green, full release `--test cli` **245**.

## F70 — the `n` key silently drops a name the registry would accept

**I hypothesised this in a brief and labelled it a hypothesis; the test author confirmed it with live evidence.** Both halves matter: the guess was right, and it was still checked before being believed.

- `OverlayKey::Char` is **ASCII-only by construction**: `plain()` maps `0x20..=0x7e` and **drops** every other byte.
- The `n` key fills its draft solely from `OverlayKey::Char(typed) => draft.push(typed)`.
- `AgentRegistry::rename` rejects only empty, over-length and `char::is_control` names. **`café` is accepted** — asserted and passing.

**Observed, not reasoned:** typing `n:café` over the row list produced the report

```
named background agent #1 :caf (was café)
```

The `n` key opened the rename prompt, `:caf` entered the draft, **the two bytes of `é` were silently dropped**, and the agent was renamed to `:caf`.

### Why this slipped through a card I accepted

T4-S4a wrote `n`'s legality rules into A3 — *"non-ASCII is legal — the rule is control characters, not ASCII-only"* — and the registry implements exactly that, pinned by its own tests. **The rule and its enforcement agreed; nothing tested whether the only path a user has could reach it.** That is **F57**'s hole once more (*a single source of truth guarantees readers agree with it; it guarantees nothing about whether anyone acts on what they read*), and it is the third time this epic has paid for it — F57 itself, F69's unreflecting mirror, and now a legality rule no keystroke can satisfy.

**The pattern worth naming: a validation rule is only as wide as the narrowest input path that feeds it.** Writing "non-ASCII is legal" in a requirement and implementing it in the registry proves nothing about the keyboard.

### Not fixed here, deliberately

T4-S5 needed the same decision for its own filter line — `n:café` is a plausible *and legal* filter — and resolved it by reading the filter line's bytes through `kitsu-editor`, which handles non-ASCII, split multi-byte characters and bracketed paste. **`n`'s fix is the same shape**, and doing it inside T4-S5 would have meant an implementer changing behaviour no test of that card covered.

**It is its own card**, and it has a pleasing side effect: `n` is the last consumer of the decoded text prompt, so routing it through the editor **removes that prompt entirely** — one fewer parser of the same bytes (**F45**), and one fewer surface where an input path can be narrower than the rule it feeds.

### Also recorded: an unidentified `--test tui` flake

One debug `--test tui -- --test-threads=1` run came back **59 passed, 1 failed**; the immediate re-run and the release run were both 60/60. **The failing test's name was not captured — grep consumed the output.**

That is the same evidence loss F39's eighth occurrence warned about, and the rule it added (**one output file per suite**) exists precisely for this. An unidentified flake is strictly worse than a characterised one: F39 took **eight** occurrences to pin because the first two destroyed their own evidence. Probing deferred until the tree is quiet, since running these suites concurrently with another agent's can itself produce failures.

## F71 — a message the test can read and the user can never see

Caught by the implementer, **beyond its brief**, and it is the most valuable thing in T4-S5.

T4-S5 decided that an invalid filter term is refused with the parser's own message. The obvious implementation puts that message in `state.report`, which the tests collect and assert — and the tests would have passed.

**But the overlay's frame chain puts the open line innermost.** A report row painted under the row list is **not on screen while the filter line is open**. So the refusal would have been:

- **observable to the test**, which reads `state.report` directly, and
- **invisible to the user**, who sees the filter frame and nothing else.

The card's own decision — *tell the user which character is wrong instead of showing an empty view* — would have shipped as a message nobody could read, with a green suite.

### Why this is its own finding and not a detail

Every "is this visible?" failure in this epic so far has been about the **product** lying to the user. This one is about the **test** being satisfied by something the user cannot reach: the test and the user were reading **different surfaces**, and only one of them is the product.

**The rule: a test that asserts a message exists has not asserted that the message is on screen.** Collecting from state is the convenient thing to assert and the wrong thing to trust, and the gap is invisible precisely because the assertion passes.

This is the same family as **F57** (readers agree with the source; nobody checks whether anyone acts on it), **F69** (a mirror that no longer reflects cannot fail) and **F70** (a legality rule no keystroke can satisfy) — four occurrences in one epic of *the thing we checked was not the thing that matters*. The fix here was to paint the refusal **on the filter frame itself** (`filter_prompt_rows` takes the report), and to clear a stale report when `/` opens, so the frame only ever shows a refusal of what was typed into it.

### The unidentified `--test tui` flake did not reproduce

Five green runs across both profiles — three of mine with one output file each, two of the implementer's. **That is not proof of absence**, and the original failing test's name was never captured, so it stays open. Recorded so the next occurrence is the second and not the first.

## F72 — an unbracketed paste submits early in all three editor lines, and the heuristic that used to cover it is out of the path

Raised by the test author as a blocker, and the safety half **traced by me before authorising anything**, because the shape resembles the defect that cost this epic a rejection.

### The safety question, answered: no, F68 is not re-opened

An open editor line (`s`, `/`, and `n` after this card) **bypasses the decoder entirely**, and `overlay_feed_chunk` ends that branch with `return false` — so the **whole chunk** is consumed by the line and the decoder is never reached. Verified by reading. Therefore when an unbracketed paste makes a line submit at its first newline, **the tail cannot become keys**: there is no path from it to `handle_overlay_input`. The `r`+`y` and double-`x` hazards of **F68** stay closed.

### The real gap

On a terminal that **does not report pastes**, a pasted multi-line value is not content — the editor submits at the first newline (its own documented rule for unbracketed input, pinned by `unbracketed_newlines_still_submit_each_line`) and the rest stays in the line. So the paste rule A3 states — *a paste is content, newlines included, and the line waits for your own `Enter`* — holds **only where the terminal brackets pastes.**

**And the protection that used to cover this is now structurally absent.** F68 added the unbracketed-burst heuristic precisely because some terminals do not bracket; but that heuristic lives in the decoder, and these three lines bypass the decoder by design — for good reasons (non-ASCII, split multi-byte characters, and the editor's own paste rule). **The fix for one hazard removed the mitigation for another**, which is worth stating plainly: the bypass was introduced in T4-S4b-ii to *protect* the paste rule, and it simultaneously dropped the fallback that made the rule hold without terminal cooperation.

This is not new with `n` — `s` and `/` already have it. `n` only makes it three, and removes the burst heuristic's incidental coverage of the rename path.

### Why it is recorded rather than fixed here

The honest fix is for the editor to learn an "unbracketed burst" rule of its own, or for the overlay to bracket-wrap what it hands a line. Either is a change to a parser three surfaces now depend on, with F68's rejection as the precedent for how badly that can go — so it is a card, not a side effect of a rename fix.

**The consequence for this card:** the existing test `a_multi_line_paste_into_an_overlay_text_prompt_is_content_and_its_tail_never_runs` passes today only because the burst heuristic turns its **unbracketed** chunk into one paste. Once `n` is on the bypass that premise is gone and both its assertions fail. **Re-expressing it with a bracketed paste is authorised** — the overlay enables `ESC[?2004h` on entry, so a real paste in this surface *is* bracketed, which makes the bracketed form the more faithful test, not the weaker one. Its intent — a paste is content and its tail never runs — must survive unchanged.

## F73 — `overlay_visible_rows` is registry order, not the order the user sees

Found by an agent **implementing** a test rather than reading about it: its first custody test asserted a row order taken from `overlay_visible_rows` and failed on **its own precondition**.

`overlay_visible_rows` returns `agent_view_rows(registry)` filtered — verified, it is the **registry's** order. **A2's display order is computed elsewhere**, inside the selection, via `agents_view::group_rows`/`display_order`. So the function whose name says "visible" does not yield the sequence the user sees.

**Anyone writing a row-order expectation from it will get this wrong the same way.** The honest fix in a test is to derive the order through `group_rows`, which is what the passing version does.

Worth noting *why* the naming misleads: the function is the single place the **filter** is applied (T4-S5), which is what "visible" refers to — membership, not sequence. Both facts are true and the name only carries one of them.

## F74 — `StatusFields` has twelve construction sites and not one uses `..Default::default()`

Raised as a blocker rather than worked around: A5's `attached · agent N` status segment needs a new field on `kitsu_render::live::StatusFields`, and **adding a field is a compile error in nine existing test modules** across `kitsu-tui` and `tui_bridge`.

Verified by counting: **12 sites** (`kitsu-tui/src/status.rs` 8, `kitsu-render/src/live.rs` 1, `kitsu/src/tui_bridge.rs` 3), and **zero** of them use `..Default::default()`.

**So every future field on this struct is a cross-crate, multi-test mechanical edit** — which is a standing tax on exactly the kind of change this epic keeps making (T3-S5 added the status row, T4 keeps adding things to it). It also pushes an implementer toward the wrong instinct: a struct that is painful to extend invites a second struct beside it, which is the duplication **F45** was closed by removing.

**Not fixed here**, and the status segment is **its own card**: there is no failing test driving the field yet, so authorising the mechanical edit now would mean an implementer changing nine test modules with nothing red to justify it. The card writes the test first, then takes the edit with that authorisation. The prompt label carries the legibility argument for the `←` rule on its own meanwhile.

The cheap mitigation when that card runs is to give the struct a `Default` and convert the sites to `..Default::default()`, so the *next* field costs one line instead of twelve.

## The two rulings that go with T4-S6a

**`Ctrl+X 1..9` is correctly out of scope, and the reason is a design constraint worth keeping.** §4.7 says "attach agent N", and the digit **cannot be an id**: ids pass 9 after nine spawns, so a digit-keyed id binding would be dead for `#10` — and *worse than dead*, it would be **wrong** once `#1` is pruned and `#11` exists. So the digit must be a **1-based ordinal over A2's display order**, which means resolving it against the registry at the composer's byte filter, a point that **has no registry**. That coupling is its own card.

**The `⛩` milestone seam was rejected as a card, correctly.** It would pin two format strings over a `milestone_line` helper that already exists — so asserting the wording proves nothing — and **F71** says a visibility claim must go through the surface that paints it. The attach milestone is only observable where something can attach, and the detach-recap milestone is the recap's header and meaningless without the buffer. Neither was pinned, and the test banner says so. **Declining to write a test that would pass by construction is the right outcome**, and this is the fourth time in this epic.

## F75 — REJECTED: `Enter` closes the agent view and attaches nothing, because the attachment has no reader

**T4-S6a was rejected on review for this, and the gap is mine as much as the agents'.**

`OverlayState::attaching` is **written** in the overlay's `Enter`/`→` arm (`main.rs:7956`) and **never read by production code** — verified: every other occurrence in the file is a test. Meanwhile `read_chat_line_inline` hardcodes `let attached: Option<u64> = None;` (`main.rs:6292`), so `composer_prompt` never shows a label and `read_line_inline_with_keys` can never see an attachment, which means **`←` and `Ctrl+X d` can never detach in production** either.

All twelve tests pass, because they call the reader with `attached: Some(3)` directly and the overlay's state directly. **The two halves are each tested and nothing tests that they are connected.**

### Why this is a rejection and not a follow-up

Before this card, `Enter` and `→` were **not decoded at all** — pressing them did nothing. After it, `Enter` **closes the view** and still does nothing. That is **worse than the starting state**: closing the view is a visible response that implies the action succeeded, so the user believes they attached and then finds no label, no stream and no detach. A key that silently does nothing is a missing feature; a key that *appears* to work is a defect.

This project has rejected "half-shipped" twice already on weaker grounds — **F38**'s lesson that a command which works but cannot be found is half-shipped, and the two cards that added a key to the help because a working-but-unadvertised key is a defect a user meets. This is the same standard pointed the other way.

### How it got through, which is the part worth keeping

**The test author designed the gap in, flagged it honestly, and gave a reason I should have challenged:** that production passes `None` because "nothing can attach yet, and inventing a carrier for session state would be a second source of truth". **I read that and briefed the implementer without questioning it**, so the implementer built exactly what it was asked for and reported exactly what it did.

The reason does not hold. Attachment must **outlive** the overlay — `OverlayState` is per-invocation — so the composer loop's session state is not a *second* home for it, it is the **only** home it can have. Calling a necessary carrier a second source of truth is how a real wiring gap gets a principled-sounding justification.

**This is the F57 family again, and the fifth time in this epic**: *a single source of truth guarantees readers agree with it; it guarantees nothing about whether anyone acts on what they read.* The variants so far — a registered command nobody dispatched, a mirror that no longer reflects, a legality rule no keystroke could satisfy, a message only the test could see, and now **a state nobody reads.**

### What the rework must deliver

Carry the attachment **out of the overlay and into the composer loop**, so the three keys work end to end: `Enter`/`→` attaches and the prompt says so, `←` on an empty composer detaches, `Ctrl+X d` detaches. The seam is the implementer's to find — I am not specifying a mechanism I have not read — but **if it genuinely cannot be done without duplicating state, that is a stop-and-report, not a workaround.**

**And it needs a test that fails when the halves are disconnected**, which is the assertion this card never had. The twelve existing tests stay as they are.

### F75 resolved, and the rework proved it the hard way

The attachment now has a reader. It lives as **one local in `run_interactive`'s composer loop**, borrowed down into the inline reader, with two production carriers connecting the halves: the overlay runner returns `AgentViewExit { shown, attached }`, and `apply_agent_view_exit` applies it. Verified: `apply_agent_view_exit` is called from production (`main.rs:6382`), the session binding exists (`:19761`) and is passed down (`:19844`), and the hardcoded `let attached: Option<u64> = None;` is **gone** — zero occurrences.

**Why that is the only honest home, which is the answer the first attempt got wrong:** `OverlayState` is per-invocation *and attach closes the view*, so by the time a composer could read `state.attaching` the state holding it no longer exists; and `read_chat_line_inline` returns on every submitted line, so a local there cannot hold it either. Everything else is **derived** from it per read — the prompt label and the reader's key routes — so there is no second field to drift.

A distinction the rework added that nobody asked for and that is right: **leaving the view is not detaching.** A view closed with `q`/`Esc`, or never shown, leaves an existing attachment alone.

### The demonstration is the part worth reusing

I asked for a test that fails when the halves are disconnected. The agent produced **two** demonstrations and said which was weaker:

1. Against a mechanically reconstructed pre-wiring tree: **six compile errors.** It labelled this the weaker proof itself — a test that fails to compile has not shown that it *notices* the defect.
2. With the carriers **present but no-op'd** (`apply_agent_view_exit` body replaced by `let _ = (exit, attached);`) — *exactly* F75's shape — it fails at runtime on the load-bearing assertion:

```
assertion `left == right` failed: F75: the agent the view attached to must reach
the composer's binding, which is the only home that outlives the overlay
  left: None
 right: Some(1)
```

**That second construction is the real gate**, and it is the "break the surviving copy on purpose" method from the F69 follow-up, applied here without being asked for. The general rule: **when a test exists to catch a wiring gap, re-create the gap rather than deleting the wiring** — deletion proves the test compiles against the wiring, not that it detects its absence.

## F76 — `Ctrl+X d` discards a non-empty draft

Flagged by the implementer for my attention rather than silently shipped or silently fixed.

`InlineRead::Detached` carries no draft back, and its doc comment says it is "only ever reached on an empty one" — **true of `←`, which requires an empty composer, and not true of the chord.** `Ctrl+X d` has no such precondition, so pressing it with text typed clears the line: the loop's detach arm has nothing to replay and sets the carried draft to empty.

**Not fixed in T4-S6a**, because carrying the draft means changing the `Detached` variant, which changes the twelve tests written for this card — an authorised test edit, which belongs in a card with a failing test of its own rather than in a rework whose contract was one added test.

The shape is familiar: **a variant documented by the narrowest route that reaches it.** `←`'s empty-composer precondition got written into the type's comment, and then a second route was given the same variant without the precondition. The comment was accurate when written and became wrong when the second caller arrived — which is the same mechanism as **F69**'s mirror and **F70**'s unreachable rule, one level down in the type system rather than in a test or a validator.

## F77 — `/detach` never detaches, and I documented that it does

**The worst failure of this epic's review process so far, and it is mine.**

T4-S6a's `/detach` arm (`main.rs:22195`) is **unconditional**:

```rust
"detach" => {
    eprintln!("not attached to a background agent");
    return SlashResult::Error;
}
```

Its own comment explains why: *"Nothing in this card can attach, so there is one answer to give."* That was **true when written** — the first attempt had no working attachment. Then I rejected that attempt for **F75**, the rework wired attachment end to end, and **this arm was never revisited.** The slash dispatcher has no access to the composer's session binding, so wiring it means threading that binding through `handle_slash_command`'s call sites — a signature change, not a one-line fix.

### What makes this worse than F75

**I shipped documentation claiming it works.** `README.md`'s command table said `/detach` would "Leave an agent you attached … and return the composer to the session", `CHANGELOG.md` listed it among the three ways to return, and `docs/functionality.md` said "`Ctrl+X d` and `/detach` also detach". **All three were false when I committed them.** Corrected in this card's commit.

So the standing requirement that documentation ships with the feature was satisfied *procedurally* — three files updated in the same commit — and **violated in substance**, because what they said was not true. **A doc update is only worth what its claims are worth**, and a reviewer writing docs from a brief rather than from the code is how a false claim gets the authority of a committed file.

### And it is F75 repeating inside the card that fixed F75

F75 was *a state nobody reads*. I rejected it, demanded the wiring, verified the chain overlay → `AgentViewExit` → `apply_agent_view_exit` → session binding → composer — and **never checked the third consumer of that binding.** I verified the route I had been thinking about and treated the card as whole.

**Sixth occurrence in this epic** of *the thing we checked was not the thing that matters*: a registered command nobody dispatched (**F57**), a mirror that stopped reflecting (**F69**), a legality rule no keystroke could satisfy (**F70**), a message only the test could see (**F71**), a state nobody read (**F75**), and now **a command whose arm predates the feature it names.**

The specific lesson, which is narrower and more useful than "check more": **when a rework makes something possible that was previously impossible, every arm that was written while it was impossible is now suspect.** That comment — "nothing in this card can attach" — was a precise, dated statement of an assumption, and it is exactly the thing a rework invalidates. It should have been a grep.

### Carded, not fixed here

Wiring it is a signature change across the dispatch sites, and this card is the status segment. The test author correctly pinned detach tracking for `←` and `Ctrl+X d` only and wrote **no** test asserting `/detach` clears the segment, rather than writing one that would have to fail.

## F74 — corrected: nine constructions, three exhaustive, and `Default` was already derived

**My count was wrong, and the way it was wrong matters.** I wrote that `StatusFields` has "12 construction sites, none using `..Default::default()`" and authorised adding a `Default` impl.

Verified by the agent and confirmed by me: the struct **already derives `Default`** (`live.rs:254`, `#[derive(Clone, Debug, Default)]`), there are **nine literal constructions**, and **six already use functional-update syntax** (`..busy()` / `..fields`), so adding a field broke **exactly two** places — quoted as two `E0063` errors — not nine test modules. `kitsu-render/src/live.rs` has **zero** constructions.

**I produced that number with a grep and trusted its shape.** `grep -c` over a type name counts *mentions in text*, not constructions in code, and summing per-file counts compounded it. The resulting finding overstated a real but much smaller tax, and the authorisation I wrote on top of it asked for work already done.

**A count is evidence about text until someone reads the code.** This is the same class as F43 and F64 — a mechanism (here, a number) taken from a cheap signal and promoted to a finding without the read that would have corrected it.

The underlying tax is real and now paid: three exhaustive sites converted, with one case left partly explicit because the values it spells out are what it documents. Note `clippy::needless_update` forbids a fully exhaustive literal plus `..Default::default()`, so a converted site must omit at least one field.

## F78 — the status row's drop order is not monotonic in width

Measured, not reasoned, while proving an assertion reachable: **a clipped model segment reappears at 44, 43 and 42 columns** once the row is busier, because `compose_status_row` offers each segment what the higher-ranked ones left, and dropping a thirteen-column queue tag frees enough room for a segment that was previously truncated.

So the intuitive property — *narrower never shows more* — is **false**, and a test asserting a prefix or no-revive relationship across widths cannot hold. The test author had written exactly that assertion first and discovered it was unsatisfiable (the **sixth** unsatisfiable assertion in this epic, after F58, F61 and the two in F70's card).

Related measurement from the same sweep: "if the queue tag survives, so does the attachment" is **false below 32 columns**, where the ten-column tag fits and the eighteen-column label does not. The surviving test therefore pins the **claim order exactly** — `painted == (cols >= tag + sep + label)` across 20..=200, derived from the tag's own text — and says nothing about the model's revival in either direction.

**No future card should assert a prefix or no-revive property on this row.**

## F79 — `/jobs view` discards the attachment: F75 verbatim, third consumer

Found by the test author while writing F77's tests, and verified by me.

`run_agent_view_overlay` has **two** call sites:

- `main.rs:6377` — the inline reader: `let exit = run_agent_view_overlay(registry, false);` and it applies the whole thing.
- `main.rs:14030` — `/jobs view` / `/agents view`: `let shown = run_agent_view_overlay(registry, true).shown;` — **the attachment is thrown away.**

So in interactive inline mode, opening the view with `/jobs view`, selecting a row and pressing `Enter` **attaches to nothing**: the prompt label and the status row never change, and the detach keys stay inert. Attaching only works if you reached the view by the chord or the arrow route.

**This is F75 word for word — *a state nobody reads* — in the third consumer of the same exit value.** F75 was the first consumer (the composer binding did not exist), F77 was the second (`/detach`'s arm predated the feature), and this is the third. I rejected a card for the first, documented the second as my worst review failure, and **the third was sitting in the same function the whole time.**

### The lesson, which is sharper than the previous two

After F75 I verified *the chain I had been thinking about* — overlay → carrier → binding → composer — and declared the card whole. After F77 I wrote that *"when a rework makes something possible that was previously impossible, every arm written while it was impossible is suspect"*. Both were right and both were too narrow.

**The actual rule: when a function starts returning something new, every caller is a suspect, and callers are enumerable.** `run_agent_view_overlay` changed from `-> bool` to `-> AgentViewExit` in T4-S6a's rework. That change has a complete, mechanical list of places to check — two — and nobody checked the list. **A type change is the cheapest possible audit trigger and it was available for free.**

### Folded into F77's card, deliberately

Normally an adjacent defect gets its own card. This one does not, because:

- It is the **same mechanism and the same seam**: the `&mut Option<u64>` parameter F77 needs for `detach` is exactly what `jobs view` needs to call `apply_agent_view_exit(&exit, attached)`. Two arms of one dispatcher, one parameter.
- Leaving it would ship an **incoherent product**: `/detach` would work only if you attached by the chord, so "three ways to return" stays conditional on *how you attached* — and the README, CHANGELOG and `functionality.md` claims corrected last card would still be conditionally false.
- **Shipping a known instance of F75, after rejecting one card for it and recording another as a review failure, would be indefensible.**

The implementer is authorised to add the one test this needs, as an explicit exception to the test-first split, for those reasons.

### F79's fix, and a test-only shim I removed on review

The implementer found my brief wrong again (the tenth mechanism correction in this epic): the `/jobs view` arm is **not** in `handle_slash` but in `handle_jobs_command`, a separate function with **five existing unit-test call sites**. Threading the binding there meant either editing those five tests — which implementers may not do — or keeping the old three-argument name available. It chose the latter: a `#[cfg(test)]` shim named `handle_jobs_command` delegating to `handle_jobs_command_attaching(.., &mut None)`, and said so plainly rather than burying it.

**I removed the shim on review**, and the reason is this epic's own history. A `#[cfg(test)]` function **shadowing a production name** means the tests call something production never calls — which is **F69** exactly, a test-only path that looks authoritative. The five sites now name the production function and pass `&mut None` explicitly, which *documents* that those tests do not drive the view instead of hiding it behind an alias. Five one-line edits, no assertion touched, 889 still passing.

**The seam the implementer added is the right kind.** `run_agent_view_overlay` declines outside inline mode on a tty, so the arm cannot be entered from a test at all — which is **exactly why the discarded attachment survived review**. It therefore extracted `apply_jobs_view_exit(&exit, attached) -> bool`: the one call F79 needs plus the `shown` the text-deck fallback depends on, in one testable place. Making the unreachable thing reachable is the lesson of **F71** and **F75** applied before the fact rather than after.

**Both traps were re-proved by prototype**, a method now used on four cards: a body applying the exit to a *local copy* leaves `shown == true` and still fails on the painted row, and `apply_detach_command` clearing a copy returns `Detached(1)` and still fails. Both prototypes reverted from byte copies.

**No fourth consumer.** `run_agent_view_overlay` has exactly two call sites and `apply_agent_view_exit` exactly two production callers — checked, because F79's lesson is that the list is enumerable.

## F80 — a fourth flake: the approval modal paints nothing in release, once

> **RETRACTED by F112. This was never a flake.** The capture is a misfiled
> **failing-first run of T3-S7a**: `approval_modal` unconditionally pushes its
> header, so `left: []` is unreachable for it; the three tests read **no**
> global; and the capture's binary ran **127** tests where the clean runs ran
> **130**. Do not count this as a flake or as evidence about the surface slot.

Seen once by the F76 test author and **captured to its own file** rather than lost:
`scratchpad/flake-kitsu-render-lib-release-approval_modal.txt`.

`cargo test --release -p kitsu-render --lib` failed three tests together —
`approval_modal_paints_header_then_details_then_the_option_line`,
`approval_modal_keeps_every_provenance_row_in_order`,
`approval_modal_option_line_is_the_shared_composer_byte_for_byte` — all with `left: []` or `left: None`, i.e. **the modal painted nothing at all.**

Three things make the diagnosis likely without chasing it: the three tests fail *together*; the third names **"the shared composer"**; and the failure is *empty output* rather than wrong output. That reads as a **shared global not yet installed** when those tests run — cross-test ordering, not a defect in the modal.

**It appeared during a run concurrent with another `cargo test`**, and seven later release runs plus the debug run were 130/130.

**The concurrency is partly my doing**, and worth recording as a cause rather than a coincidence: earlier in this session I ran suites myself while an agent was running its own, which is the condition under which this appeared. Later briefs now say **do not run suites concurrently**, and the following card's agent ran them one at a time and saw nothing.

So the standing flake list is now four: the two characterised ones, the **unidentified `--test tui`** (twelve runs clean, name never captured), and this. **F39 took eight occurrences to pin because its first two destroyed their own evidence** — this one has its evidence on disk from occurrence one, which is the whole point of the one-file-per-suite rule.

## F81 — A8 is violated at the TUI idle composer, and the codebase already contained the argument

I asked whether T4-S8 was test-only, since **F50** had established that A8's 250 ms budget was satisfied by a 50 ms poll tick and asserted nowhere. The answer is **no**: A8 is **broken**, at a path nobody had looked at.

### The violation, verified

- `sync_agent_counts` and `repaint_status_row` are called from **exactly one loop** — `run_live_typeahead`, the *during-a-turn* typeahead. `repaint_status_row` has only two other production callers, one of them the overlay handing the terminal back.
- **At the idle composer there is no cadence at all.** `read_line_inline_with_keys` blocks in `input.read(&mut buffer)` with **no timeout** (verified: no poll above it); its only non-byte wake is an interrupted read plus a pending `SIGWINCH`, and a wake with no resize is explicitly retried **without a frame**.
- **The idle composer's bottom row does carry the ambient tag** — `status_row()` → `StatusSource::compose` → `compose_status_row` (verified). So the counter is **on screen and indefinitely stale**.
- The counts are a **pull**, read through a closure at paint time, so nothing needs pushing. **The entire gap is that at idle nobody calls paint.**

**So a background agent that blocks while the user sits at the prompt changes nothing on screen until a keystroke, a terminal resize, or a submitted line.** That is precisely what the whole T2 blocked-agent surfacing exists to prevent.

### A8's carve-out does not cover it

A8 grants the "idle prompt keeps reporting at the next prompt cycle" latitude to **`classic`**, by its own sentence. The TUI idle composer gets no such grant, so this is an unqualified violation rather than an under-specified corner.

### The part worth keeping: the argument was already written down

Two comments in the tree bear on this, and they disagree:

- `crates/kitsu/tests/tui.rs:2842` records the rationale for having no idle cadence: *"`read_line_inline` is deliberately not a cadence … between turns R5's elapsed segment is `None` anyway, so the idle prompt has nothing to tick."* **True of the clock. False of the ambient tag.**
- `crates/kitsu/src/tui_bridge.rs:450`, in the reduced-motion logic, states the counter-argument exactly: the queue and ambient tags stay in the key *"because they are state and not animation: **freezing them would hide a blocked agent**."*

**The same reasoning, about the same segment, written by the same project — applied to reduced motion and not to idle.** Nobody had to discover that freezing the tag hides a blocked agent; it was already a sentence in the file. The gap was that the rationale for no-idle-cadence reasoned about *elapsed time* and silently generalised to *everything on the row.*

**The lesson: a justification that is true of one segment is not true of the row.** "Nothing to tick" was a claim about a clock, and it was allowed to excuse a surface that also carries state.

### A second, smaller gap

Mid-turn, `run_live_typeahead`'s approval arms `continue` **before** `sync_agent_counts`, so while an approval modal is up — which can be minutes — the ambient counter is frozen, and the modal paints a status row of its own, so the stale count is visible. Noted, not tested (adjacent to F54's territory).

### Two corrections to my own records

- **F50's proposed cheap fix described a constant that did not exist.** It said "a unit assertion on the poll interval constant"; the poll was a **bare literal `50`**. There was nothing to assert until this card introduced `AMBIENT_TICK_POLL_MS`. An F64-family slip in a finding of mine.
- **The plan row's `Files: status.rs` is wrong.** `crates/kitsu-tui/src/status.rs` is pure painting — no clock, no cadence. The mechanism lives in `main.rs` and `tui_bridge.rs`.

### And the row's "watch-driven" is right for the wrong reason

It is needed not to tighten 50 ms to something smaller, but to give the **idle** composer a cadence it has never had. `registry.subscribe()` already exists, and because the counts are a pull the implementer needs **a caller, not a rewrite**. The classic-mode notifier is not that caller: it is gated on an opt-in environment variable and an assistive-input check, so a default TUI session has **no subscriber at all**.

### The vacuous-pass trap, flagged by the test author in its own seam

`idle_ambient_refresh_interval() -> Option<Duration>` can be made `Some(..)` **without wiring anything**, which would be *exactly* F50's failure — a requirement passing a test while nothing holds it. The seam's doc comment says so, and the implementer owes a **behavioural** test that an idle registry change reaches the surface **with no byte read**. That test could not be written in advance because it needs whichever wake mechanism the implementer chooses.

### No sleeping test

Both new tests are structural. The during-turn guard reads the named constant so raising the tick past 250 ms **fails by name** — F50's loud guard, finally built. The idle test asserts the painted row through the surface. The only sleep is the existing frame-budget helper that clears N3's coalescing window, which is the in-tree idiom and not a latency measurement. With four open flakes, waiting out a 250 ms budget on a loaded machine was not worth it.

### F81's resolution, and the acceptance test that would have passed a lie

The idle composer now has a cadence, driven by **`registry.subscribe()`** rather than a timeout on the idle read. The reason for that choice is worth keeping: `read_line_inline_with_keys` is **generic over `R: std::io::Read`** and is driven by in-memory cursors throughout the editor and composer tests, so giving it an fd to poll would mean a signature change to **the one function that owns the decoder and the three editor lines' byte bypass** — the path **F68** and **F72** say not to disturb. Because the counts are a pull, a subscriber only needs to *trigger a paint*: the composer stays blocked in `read()` and the row is repainted around it. **A caller, not a rewrite.**

**A bare wake still paints nothing** — verified by reading: the step awaits `changed()`, compares the projected `(blocked, running)` against what it last showed, and **returns without painting** when they match. So a generation-only bump emits no bytes and reduced motion and N3's frame budget are untouched.

### The result that matters most

The implementer prototyped the broken shape I asked for — interval returning `Some(..)`, both repaints removed — and the outcome was:

- **the test author's acceptance test PASSED**, and
- the implementer's own behavioural test **failed** at the painted-row assertion (`left: ""`).

**So the acceptance test alone would have accepted an implementation that paints nothing.** That is *exactly* **F50**'s failure mode — a requirement passing a test while nothing holds it — reproduced inside the card written to fix F50, and caught only because the test author flagged the trap in its own seam's doc comment and I required a behavioural test from the implementer.

**The general lesson: a test that asserts a *capability exists* is not a test that the capability *does anything*.** `idle_ambient_refresh_interval() == Some(..)` is a claim about configuration; "an idle registry change reaches the screen with no byte read" is a claim about behaviour. The first is cheap to satisfy falsely. This is the seventh variant in this epic of *the thing we checked was not the thing that matters* — after F57, F69, F70, F71, F75 and F79 — and the first one caught **before** shipping rather than after.

### A hazard found while implementing

`repaint_status_row` **drops a paint inside N3's 1/60 s window without recording the key**, so a change arriving in that window could wait for a registry event that never comes while an agent sits blocked. The step therefore repaints, sleeps one frame interval, and repaints again; the dedupe makes the second call free when the first landed. **A rate limiter that silently discards rather than defers is a correctness problem for anything event-driven behind it.**

### The mid-turn gap stays open

`run_live_typeahead`'s approval arm still `continue`s **before** `sync_agent_counts`, so while an approval modal is up the counter is frozen and the modal's own row shows a stale count. Out of scope here (adjacent to **F54**, whose signal path is still unexplained). The implementer notes the new cadence may now refresh that row **incidentally**, since a registry change wakes the task during a turn too — but **asserted nothing about it and changed nothing in that arm**, which is the right call: an accidental fix is not a tested one.

## F82 — three keys the view always accepted and never advertised, found by sweeping the keyboard

T4-S10's job was to collapse three independent enumerations of the agent view's keys: the help's literal, the dispatch's `match` arms, and **a test's own list** (which meant adding a key to the product and the help but not to that list silently narrowed the test).

**The design question was whether the table could be the dispatch source. It cannot:** `OverlayKey` is private to the binary, and `match` arms are not data, so `kitsu-tui` can neither be the match nor enumerate it. **An exhaustiveness test would have fallen into F57's trap** — a guard that reads the dispatch's own source cannot fail when the dispatch disagrees with it, which is precisely why F57's own guard was unwritable.

### The guard that worked: enumerate the keyboard, not the code

The test presses **every key the decoder can produce** — all of `0x20..=0x7e`, plus `↑ ↓ → Enter Backspace` and a quiet-tick-resolved `Esc` — on a fresh registry and view per key, and asks **the painted frame** whether anything happened (**F71**: never a state field). Then both directions: everything that changes the frame or closes the view must be a table row, and every row must change it.

**It reads neither source, so it cannot agree with the dispatch by construction.** That is F57's trap actually solved rather than worked around.

### What it found

**Fifteen presses are dispatched; the help advertised twelve.** Missing: **`q`** (closes the view), and **`Enter`/`→`** (T4-S6a's attach).

So the shipped view had three keys a user could press and could not discover — **the exact defect T4-S3, T4-S4b, T4-S5 and T4-S7 each closed once, open in triplicate.** Every one of those cards applied the rule *a key that works while unadvertised is half-shipped*, and every one applied it **only to the key it was adding.** Four correct applications of a rule left three violations standing, because nobody enumerated.

**The lesson: a rule applied per-change is not a rule enforced.** The fix was never more diligence per card; it was one test that asks the question for all keys at once.

### What the sweep deliberately does not cover, stated by its author

- **A row wired to the wrong action.** It asks *did something happen*, not *did the right thing happen*; the per-key behaviour tests from the earlier cards remain the only guard on **what** each key does, and they were left untouched.
- **Keys that act only from a non-fresh state** — inside `r`'s decision read, inside an open line, on `x`'s second press, under the help or a peek panel.
- **Anything that never reaches the overlay's dispatch**, i.e. the whole composer chord surface.

Naming the gaps is part of the guard being trustworthy: a sweep described as total would invite exactly the false confidence F57, F69 and F75 are about.

### Two corrections to my brief, and one of the agent's own

- **`/help keys` is not a new command.** `/help` is registered and its arm already forwards any argument, so F57's unregistered-arm hazard could not arise. The half that did matter was **registration**: the entry's `args` was `""`, so nothing offered `keys` in `/help`'s listing, the palette or either completion host. Now `"[command|keys]"`.
- The brief's "three enumerations" undercounted the problem: the collapse **closes three live defects**, not just tidies duplication.
- **The agent reported that `Ctrl+L` does not exist. It does**, in both halves, citing R8 — I verified before passing the brief on. Three of the four bindings it flagged as phantom are genuinely missing (`Ctrl+X o`, `Ctrl+X ?`, `Ctrl+X 1..9`); that one is not.

### The vacuity guard carried forward unprompted

Last card's result — an acceptance test passing an implementation that painted nothing — shaped this card's main assertion without being asked for. The help-equals-table test adds `!painted.is_empty()`, because **the one way an equality holds vacuously is both sides empty.** The table's rows are also asserted to *have consequences*: a `BindingKind` that is only a struct field fails, since `x`'s text must say "twice", `r`'s must name `y`/`a`/`n`, and the line-openers must carry their grammars.

## F83 — the fixture that blocked two cards was already in the tree, and a comment went stale the moment this card shipped

**Two things from T4-S6b, both about inherited beliefs rather than code.**

### The blocker had dissolved and nobody checked

T4-S4b and T4-S6a both deferred work on the grounds that *a steerable agent needs a live thread with an open correction channel, built from helpers that live in `subagent`'s own test module and are unreachable from `main.rs`'s.* That was true and it was recorded twice.

**But the helpers were reachable all along — from `subagent.rs`.** `EchoProvider`, `ProviderBinder::fixed` and `empty_profile_runtime` are there, and **a passing test already drove a steerable agent end to end** (`background_steering_restarts_only_that_thread_and_handoff_is_explicit`). T4-S7 had already proved the pattern by writing its registry-level tests in that module and splitting three ways.

So the blocker was never "a steerable agent is unreachable"; it was **"a steerable agent is unreachable *from where I was standing*"** — and the fix was to stand somewhere else. **A deferral records where someone was when they hit the wall, and gets re-read later as a property of the wall.** Worth checking any inherited "cannot be done" against the tree before building around it.

### A comment went stale the instant the card landed — F77 again, caught this time

The composer loop carried a comment from T4-S6a: *"the `⛩` milestone and the recap are **not** this card's (design §4.5: they land with T4-S6b and T4-S6c)."* **T4-S6b is this card, and it shipped the milestone** — so the comment became wrong at the moment of the commit that made it wrong, three lines above the arm that did it.

That is **F77** exactly: `/detach`'s arm said *"nothing in this card can attach"*, true when written and false after the rework, and it cost three false documentation claims. **Found here only because I had promised to read the arm by eye** — no test touches a comment.

**The pattern, now twice: a comment that names a future card dates itself.** "Not this card's — it lands with X" is true until X lands, and nothing fails when it stops being true. Corrected in place, with the reason recorded in the comment itself.

### The route's enumeration question, answered better than I asked it

**F82** established that *a rule applied per-change is not a rule enforced*, and I asked what enumerates a new user-visible route. The answer: **this route adds no key — it changes what `Enter` means — so the only thing that can enumerate it is the moment it begins.** Hence the milestone names the *route*, not just the agent. A key table cannot advertise a route that has no key, and bolting it on would have been the wrong fix for the right rule.

### And the race was dissolved rather than decided

A5 did not say whether routing reads the agent's state when the line is opened or when it is submitted. Both readings can be wrong — open-time can send a steer the registry must refuse, submit-time can hand a typed correction to a parser expecting `y`/`a`/`n`. The resolution was not to pick the less-bad one but to **make being wrong cheap**: submit-time routing, the registry as final adjudicator, and **every refusal sends nothing and hands the line back**, so the worst case is one extra keystroke with the user's words intact. In the implementation "refused" and "keeps the line" are the **same boolean**, so they cannot drift apart.

## F84 — the transition that is not a route: enumerating the keys missed the one without a key

My brief for T4-S6c said *"the three detach routes must all replay"* and cited **F79** (*callers are enumerable*). The test author enumerated them — and then found **a fourth way an attachment ends, which no key triggers**.

`apply_agent_view_exit` **re-binds to a different agent**. No detach key is pressed, no detach arm runs, and whatever the first attachment buffered has nowhere to go. Its first prototype **dropped it silently.**

### How it surfaced, which is the uncomfortable part

It did not fail a test of its own. It announced itself as **cross-test interference**: a *different, existing* test (`the_attach_milestone_is_painted_once_per_change_of_binding`) left a stale buffer in a process-global slot, and the multithreaded run of the new route test failed because of it. **The bug was found by luck through a shared global, which is the kind of luck that does not repeat.**

It is now pinned deliberately: `re_attaching_to_another_agent_replays_the_first_ones_recap_first` requires the previous agent's recap **under that agent's header, before the new agent's attach milestone** — order is the assertion, because the other order **attributes one agent's turn to another**.

### The lesson, which is narrower and more useful than F79's

**F79** says *when a function starts returning something new, every caller is a suspect, and callers are enumerable.* That is about **callers**. This is about **states**: I enumerated the ways a user *asks* to leave an attachment and missed the way an attachment *ends without being left*.

**Enumerating the routes out of a state is not the same as enumerating the transitions out of it.** A route is something a user does; a transition is anything that changes the state, including being replaced. The three keys were the routes. The re-bind was a transition with no route, and a brief organised around user actions cannot see it.

§4.5 does not cover it either, so the decision was the test author's and it is recorded in the requirement now.

## F84b — the byte bound makes an old caution real, and it caught its own prototype

**F52** carried two cautions. The first — *lines are the wrong unit for a memory bound* — is why this card does not reuse the cell ring. The second had never mattered: *truncating styled bytes can cut an SGR sequence in half.*

**A byte bound makes it real for the first time in this epic**, and the test author pinned it unprompted: a clamped styled line must be cut on a `char` boundary **and end `\x1b[0m`**, which is the rule `fit_row` already follows — the renderer's existing rule, not a new one. **That test failed against the author's own first prototype**, which cut on a boundary but did not reset. So it is catching something real rather than restating the cap, and without it a severed escape would have coloured the rest of the session.

### And the reason the ring was wrong is structural, not just unit-mismatch

I argued from F52's units. The stronger argument is the dependency graph, verified: **`Surface` lives in `kitsu-render`; `CellRing` lives in `kitsu-tui`; and `kitsu-render` does not depend on `kitsu-tui`.** A buffer that decorates `Surface` must live beside the trait, so **the ring is unreachable from it regardless of how it is bounded.** The split that created this card feared a deferral past R6 if the implementer "reached for the ring" — it cannot be reached, so there is nothing to defer.

**`cell_ring_lines` still has no reader.** This card does not become one, and **F52 stays open** for the overlay and the `[` dump; it is decided only for this buffer (byte-budgeted, oldest evicted, loss noted, clamped lines reset).

## F85 — a plan row's named acceptance test already existed, and writing it again would have been a flake

I challenged T5-S1's row on the grounds that a **"5 000-line RSS test"** would be flaky and would assert a property **F52** explicitly leaves open. The test author agreed and then produced a better objection I had missed.

**The test already exists.** `crates/kitsu-tui/src/cells.rs:177`, `cell_ring_bound_holds_under_far_more_pushes_than_the_limit` — **5 000 pushes behind a bound of 64**, written in T3-S6. Verified.

So the row's acceptance criterion had been **satisfied two milestones before the card ran**, and anyone following the row literally would have written a **duplicate** of a passing test — in the one form (RSS) that is both flaky and false, since one pathological line defeats a byte bound by design.

**The lesson: a plan row's acceptance can be satisfied before its card runs, and the row will not say so.** The row is a plan written once; the suite moves every card. Checking whether the named test *already passes* is cheap and was not in anyone's routine, including mine — I questioned the test's *shape* and never asked whether it existed.

### The byte assertion that is achievable, which I had written off

I said the honest claim is structural — the ring's retained count is independent of conversation length — and that bytes were off the table while F52 is open. **Half right.** A byte assertion *is* available, just not from the allocator: **the sum of the retained lines' own byte lengths is identical after 10 000 pushes and after 5 000.** Computed from the ring's contents, so it is deterministic, allocator-independent and parallelism-independent, and it pins exactly what RSS was reaching for — *the evicted lines' bytes are not retained anywhere.*

What remains untestable while F52 is open is only the form "retained bytes ≤ K for fixed K". **"No byte-level claim is testable" was my overcorrection**, and N4 should record the distinction so the next reader does not inherit it.

## F86 — the feed must land after the reader, and N4 already said so

T5-S1 splits three ways, and the ordering is the interesting part.

**The ring going live is a separate card, and it is *not* first.** N4: the ring is *"not fed in production until R6's overlay reads it — feeding an unread ring would spend up to ten thousand styled lines of memory for no benefit."*

**The precondition is a reader.** Shipping liveness first would spend that memory during the window before the renderer lands — **the exact expenditure the requirement forbids**. So the order is **reader → feed**, and I had it backwards in my brief: I suggested "the ring becomes live" might be the natural *first* piece because it is the bigger change. Bigger, but second.

**CORRECTED IN T5-S2 — the sentence below was false, and it is the most damaging
kind of error this file can hold.** `TuiRuntime::enter` writes `PUSH_TITLE`, the
optional OSC 2 title and raw mode. **It does not write `ESC[?1049h`, and never
has** — verified: `1049` appears only in `main.rs`, in `run_agent_view_overlay`,
which has taken the alternate screen since T4-S2. So `enter`/`leave` deliver **one**
of N5's two clauses (the title), and **the alternate screen is code someone must
write.** The original claim, kept for the record:

> **The alternate screen needs no code at all.** `TuiRuntime::enter`/`leave` already do the alt-screen, title and raw-mode lifecycle, panic restore and signal restore already cover **N5**, and all of it is tested. What is missing is a **caller** — and the only thing that can select fullscreen is **T5-S2's switch**, which owns that wiring. So **N5's "vacuous today" note is slightly wrong**: it says the clauses become real "when T5's fullscreen renderer uses that runtime", but the renderer is **pure by design** and never touches a terminal; it is the *switch* that calls `enter`.

**How it got in:** I recorded it from an agent's report, which had read it off a doc
comment's summary rather than the function body — the same shape as **F90**, where a
deliberate refusal was read as an absence. It then travelled: into my brief, and from
there into the next agent's own report (*"nothing emits `?1049h` today"* — it is
emitted today, by the overlay).

**The lesson, and it is about this file rather than the code: a findings entry is read
by every later card as settled fact, so a false one is worse than a false brief.** A
brief is questioned by the agent it is handed to; this file is quoted. Entries that
assert *"X already does Y"* need the line that shows it, or they are hearsay with a
number on it.

Keeping the renderer pure is what lets follow and paging be asserted through painted rows rather than a pty (**F71**).

## F84 applied before the bug, not after it

**F84** (*enumerating the routes out of a state is not enumerating the transitions out of it*) was one card old, and the test author answered it structurally: an **eleven-row transition table** against four keys, with **three transitions no key triggers** — the auto-follow **re-latch** (paging down onto the newest line resumes following), **append while following**, and the one the ring forces: **eviction past the anchor**.

That third one changed the design: `Anchor::Pinned` stores an **absolute** line ordinal (`evicted() + index`) rather than an index into the retained lines, because **an index would drift under eviction while the anchor looked unchanged.** That is the same class of bug as F84's silently-dropped buffer — state that moves underneath something that believes it is still pointing at the same thing — and it was designed out rather than discovered.

**Auto-follow and the paging keys are one card, not two**, for the same reason: they are one state machine with two states, the re-latch is a transition belonging to *neither* half, and splitting them would put one `Anchor` under two cards and give the first a state it cannot reach.

## F87 — `install` has two production callers, and I missed the one that mattered

I corrected the previous card's claim that `tui_bridge::install` *"has dozens of call sites across `tui_bridge.rs`'s own test module"* — a **name-match count**, the same error class as my own **F74** (where I recorded "12 construction sites" for a struct by grepping its type name and the real number was two). I counted **three**: one production caller and two tests.

**I was wrong. There are four, and the one I missed is a second production caller:** `main.rs:20650`, the **`/tui inline` re-install** inside the renderer switch, after a classic segment. Verified.

**The consequence, had it stayed unthreaded:** `/tui classic` then `/tui inline` would reinstall a **ring-less** viewport, and the fullscreen overlay would go blind **for the rest of the session** — invisible until someone opened the overlay *after* a renderer switch, which is exactly the kind of state nobody tests by hand.

### Why I made it, having just warned about the opposite error

I grepped `install(` across the crate, filtered the obvious impostors (`SurfaceProbe::install`, `before_install`, `install_panic_restore`), and **read the surviving list** — which was correct as far as it went, but `main.rs:20650` appeared in my own output as a truncated line (`tui_bridge_gu…`) and I did not expand it. **I warned about counting name matches and then undercounted by trusting a truncated line.** This project already has a finding on exactly that: *never assert absence from a truncated search.*

**The durable rule: a count is not finished until every match has been read in full.** Filtering a list is not reading it, and a column-truncated grep is a filtered list.

### The obligation this creates

**Any future parameter added to `install` has a two-site obligation**, and the second site is a re-install that only runs after a mode switch — so forgetting it produces a defect that is **silent until a specific sequence of user actions**. Worth a note wherever `install`'s signature is documented.

### And a gap the author flagged rather than papered over

**Nothing asserts that `main.rs` passes the *configured* value rather than a literal.** There is no seam in `run_chat` to observe it, so the install-level test proves the parameter is **honoured** and the two call sites are **eyeball-verified only**. That is the honest position, stated — not a test pretending to cover it.

## F88 — a fifth flake, from a card of my own

`tests::the_recap_holds_only_what_the_foreground_produced_while_attached` — from **T4-S6c**, two cards ago — failed **once** in a parallel debug `--bins` run, then passed alone and in four of four subsequent full runs. **Captured to its own file from occurrence one** (`scratchpad/flake-the_recap_holds_only_what_the_foreground_produced_while_attached.txt`), per the rule F39's eighth occurrence added.

It is **not** one of the four already on the books. The shape is suggestive: T4-S6c's buffer lives behind a **process-global slot**, and **F84** records that a stale buffer in that same slot caused cross-test interference during that card's own development. So this is plausibly the same global, surfacing under parallelism rather than ordering — which would make it a **test-isolation** defect in the card I accepted, not a product defect.

Not chased, per the rule. Recorded so occurrence two is the second and not the first.

## F89 — REJECTED: the feed landed before a *reachable* reader, and the conflation was mine

T5-S1b is correct code, fully tested, every gate green — and **rejected**, because it spends memory N4 forbids spending.

### What N4 actually says

> the ring is **not fed in production until R6's overlay reads it** — feeding an unread ring would spend up to ten thousand styled lines of memory for no benefit.

**R6's overlay** is T4-S9's transcript overlay. **It does not exist.**

### What I wrote in the brief

> *"T5-S1a shipped the reader (`2cd3559`, the pure fullscreen view). **It is the reader N4 required before the feed.**"*

**That sentence is wrong, and it is mine.** T5-S1a shipped a **pure view with no caller** — deliberately so, and I accepted it on that basis, noting in its own cycle record that *"no user-visible behaviour ships here — the view has no caller yet."* Verified now: `FullscreenView` is constructed **only inside test modules**, and every reader of the ring outside its own definition is a test.

So after T5-S1b, every inline session retains up to **ten thousand styled lines** that **nothing in production can display** — N4's forbidden state, reached by a card whose brief cited N4 as its authority.

### The distinction I lost

**A reader that exists is not a reader that reads.** F86 argued the ordering correctly — reader before feed — and I turned "a reader exists" into "the precondition is met" without asking whether anything *reaches* it. The pure view's purity, which was its virtue in T5-S1a, is exactly what makes it not a reader: it holds no terminal, has no key, and nobody calls it.

**This is the seventh variant in this epic of *the thing we checked was not the thing that matters*** — after F57 (a registered command nobody dispatched), F69 (a mirror that stopped reflecting), F70 (a rule no keystroke could satisfy), F71 (a message only the test could see), F75 (a state nobody read) and F79 (a caller nobody enumerated). **And it is the first where I was the one who wrote the false premise into the brief**, rather than inheriting or failing to catch it.

It is also the same shape as two cards I **rejected**: a key that *appears* to work, and a state nobody reads. **Memory nobody can read is that again.** Accepting it would make the standard I enforce on agents one I do not apply to myself.

### The rework

**The feed must land with, or after, a reader a user can reach** — not merely a type that compiles. So T5-S1b merges with the wiring: the fullscreen view needs a production caller before the ring is fed. The implementation itself is sound and should be reused verbatim; only its **order** is wrong.

Concretely, one of:
- **wire the view** (T5-S2's `/tui fullscreen` switch, which also owns the alternate screen N5 needs) and feed the ring in the same card; or
- **gate the feed** on the renderer actually being fullscreen, so inline-only sessions pay nothing — a product change, and the weaker option, since N4 names the ring as inline's scrollback.

**N4 also needs amending** either way: it names "R6's overlay" as the reader, and there are now **two** candidate readers (the transcript overlay and the fullscreen view). Whichever lands first should be named, because this ambiguity is what let the conflation through.

## F90 — `fullscreen` was never missing: it was a tested refusal, and my brief called it a gap

The rework agent **stopped without writing a test** and was right to. Two things it found, both verified by me.

### The premise was false, again, and in a new way

My brief said: *"`fullscreen` is the one spelling it lacks."* **It is not lacked. It is present as a deliberate, multiply-pinned refusal.**

`main.rs:22842` has a third `/tui` arm whose own comment reads *"`fullscreen` is a mode the design's grammar has, and it is inert here"*, printing `/tui fullscreen is not available yet; accepted: classic, inline`. Around it: twelve call sites of the accepted-modes literals, **four test functions** asserting the refusal, and a `/help` details string saying *"`fullscreen` is not available yet: it arrives with its own renderer"* — itself asserted.

**So this card's cost is not "add a variant". It is retiring a boundary that four tests and a help string exist to hold.** That is a different card shape, and my brief priced it as the cheap one. Eleventh mechanism error in my briefs, and the first where the mistake was reading a **deliberate, documented state as an absence**.

**The lesson: "X is missing" and "X is refused" look identical from the outside and are opposite facts.** A grep for the accepted list finds the former; only reading the arm finds the latter. I have now made the inverse error too (**F87**: reading a truncated line as the whole list).

### A test that passes by luck

`tui_mode_rejects_unknown_values_instead_of_falling_back_to_classic` requires the error message to contain `` "`classic`, `inline`" ``. Verified: `TUI_MODES` is exactly that string, and `"expected one of `classic`, `inline`, `fullscreen`"` **still contains it**. So that test survives this card **by luck, not by design.**

**That is F41's hazard running backwards.** F41 banned substring *bans* because they constrain the product; this is a substring **require** that keeps passing after the thing it describes has changed. A requirement-shaped assertion that cannot fail when its subject changes is as empty as a ban that forbids the wrong things. **It should be strengthened in the same card**, not left to pass silently.

### And one correction to the agent's framing

It wrote that *"the tree is already in N4's forbidden state, so 'never feed an unread ring' is no longer achievable by ordering; only shortening the window is."* True of the **working tree** — and not of **HEAD**, which is why the rejection mattered. The feed is uncommitted, so **committed history never enters the forbidden state**, and ordering is still fully achievable: the feed and a reachable reader land in one commit, or neither does.

**Worth stating because it is the point of rejecting rather than following up:** a follow-up card would have left a commit in history where the ring is fed and unreadable. The reject keeps that window from ever existing.

## F91 — a false entry in this file is worse than a false brief

**F86 contained a flatly false sentence for three cards**: *"The alternate screen needs no code at all. `TuiRuntime::enter`/`leave` already do the alt-screen, title and raw-mode lifecycle."*

Verified false: `enter` writes `PUSH_TITLE`, the optional OSC 2 title and raw mode — **no `ESC[?1049h`**. That sequence appears **only** in `main.rs`, in `run_agent_view_overlay`, which has taken the alternate screen since **T4-S2**. So the runtime delivers **one** of N5's two clauses, and the alternate screen is code someone must write. Corrected in place at F86, with the original kept.

### How it travelled, which is the point

1. An agent read it off a **doc comment's summary** rather than the function body, and reported it.
2. **I recorded it in this file without checking** — the one step where checking was cheapest and mattered most.
3. It went into my next brief as established fact.
4. The agent receiving that brief **repeated a corollary of it** in its own report (*"nothing emits `?1049h` today"* — it is emitted today, by the overlay), having taken it from the brief without grepping.

Four hops, one unverified read, and at no point did anything fail. **The only thing that stopped it was an agent reading `lib.rs:109` while writing a test against bytes.**

### The rule

**A findings entry is read by every later card as settled fact, so a false one is worse than a false brief.** A brief is handed to an agent who questions it — that is what the eleven corrections to my briefs are. This file gets **quoted**, including by me, including into briefs.

**So: an entry that asserts *"X already does Y"* needs the line that shows it, or it is hearsay with a number on it.** Every claim in this file about what the code *does* — as opposed to what a card *decided* — should carry a file and line, and the ones that already do are the ones that have never misled anyone.

### Related, and why this one is not F90

**F90** was reading a **deliberate refusal** as an absence — the code said one thing and I read another. **This** is reading a **summary** as the implementation: the doc comment was not wrong, it was a summary, and the summary was over-read. Both produce a confident false claim about existing code; the first is cured by reading the arm, the second by reading the body rather than the comment above it.

The test author's response to finding it is the part worth copying: it had already written the assertions **against bytes rather than call sites**, so the error changed nothing it had to redo and left the implementer free to source the alternate screen from anywhere. **A test that pins an observable byte survives being wrong about which function emits it.**

## F92 — the cell ring does not record the binary's diagnostics, and nothing had ever said so

The implementer stopped on `tui_switch_from_inline_into_fullscreen_repaints_the_ring` and was right to. **The test's premise is false, and I specified it.**

**Measured, not inferred.** It temporarily printed the retained count inside the alternate screen so it landed in the panic's tail:

```
DEBUGRING retained=0
```

**Why:** the binary's own `eprintln!` is a `macro_rules!` at `main.rs:221` that expands to **`std::eprintln!`** — straight to fd 2. The ring is fed **only** by surface-routed writers (`InlineSurface::transcript_line`/`write_block`, reached from `print_fragment`, `finish_line`, `tool_activity`, `tool_result_error`, `milestone`, `render_error`). `TUI_MARK_1` is `/theme zz1`'s error, printed through that macro, so **it never enters the ring in any mode.**

The feed is live and the reader reads it. **They simply never meet on a session with no turn and no tool call** — which is every script in that test.

### The requirements fact this exposes

**N4 says the ring is "the sole source for the transcript overlay and the `[` dump" and never says what it is a record *of*.** It turns out to be **the rendered conversation** — model output, tool activity, milestones — and **not** the binary's diagnostics, receipts or command errors. That distinction was never written down, and it is load-bearing: it decides what a user sees when they scroll back in fullscreen.

### The decision, and why the cheap option is the wrong one

Two ways to make the assertion satisfiable:

- **(a) Use a surface-routed marker** — a streamed turn, a tool result, or a `kitsu_render::milestone` call. **Taken.** The assertion keeps its full force: a second occurrence after the `?1049h` offset still can only come from a reader that read the ring.
- **(b) Route the binary's diagnostics into the ring.** **Rejected.** It looks like a one-place change to the `eprintln!` macro, which is exactly what makes it dangerous: it would re-route several hundred call sites onto `transcript_line`/`write_block`, change **inline** line endings (`transcript_line` writes its own `\r\n` while `ONLCR` is on), change **classic** bytes too (`ClassicSurface::transcript_line` wraps writes in `with_live_row_suspended`), reorder diagnostics against `println!`, and make the ring far fuller than N4's sizing assumed. **309 tests pin those bytes.**

**(b) is a requirements change about what the ring records, disguised as a wiring fix.** The implementer refused to take it and asked, which is the correct instinct: *a one-line change with a three-hundred-test blast radius is not a small change.*

### Method worth copying

**It settled the question by measuring rather than reasoning** — a temporary print inside the alternate screen, placed where the failure's own output would carry it out. Then removed, with `grep -c` to prove it. Three cards have now been decided by a disposable measurement rather than an argument.

## F93 — why the alternate screen is not taken through `TuiRuntime`, which vindicates writing it directly

**F91** corrected my false claim that `TuiRuntime::enter` already takes the alternate screen. It does not — but there is a second, stronger reason not to route fullscreen through it, which the implementer found and which belongs beside the correction:

**`TuiRuntime::enter` also calls crossterm's `enable_raw_mode`, which clears `OPOST`.** This binary has **several hundred direct `println!`/`eprintln!` writers**, and `run_agent_view_overlay`'s own comment already records the consequence: *"Enabling a second, fuller raw mode here (crossterm's, which clears OPOST) would staircase all of them down the screen."* It would also nest a second raw mode under `RawStdinGuard`, whose dying-signal restore snapshots termios.

So the fullscreen segment writes the four sequences itself (`?1049h`+clear+home, `?1049l`, `22;2t`, `23;2t`) and **leaves termios alone** — raw mode in this process stays `RawStdinGuard`'s. The tests assert bytes, so this was permitted, and it is the right answer rather than a workaround.

**The general shape: a runtime that bundles two capabilities is unusable by a caller that wants one of them.** N5's title clause and its alt-screen clause looked like one lifecycle; they are separable, and one of them drags termios with it.

## F94 — a signal or panic in fullscreen leaves the alternate screen taken

Flagged by the implementer rather than widened into this card. **`install_exit_restore` gives back the inline region only**, and nothing restores the alternate screen on a dying signal or a panic while fullscreen is active.

**N5 requires restore on every exit path including panics and signals.** The inline path has it; the new mode does not. It is a real hole and it is **not** what this card's tests pin — so it is carded, not quietly patched, and N5 should record that its signal clause is met for inline and open for fullscreen rather than reading as satisfied.

### F92 sharpened — there are TWO bypasses, and what lands in the ring depends on whether tools were offered

Option (a) was authorised and **its obvious form failed too.** Re-scripted onto a plain streamed answer, the ring held **one line, and it was empty**:

```
DEBUGRING n=1 lines=[""]
```

A **second** bypass, independent of `eprintln!`:

- `orchestrator.rs:1727` — *when tools are advertised this round, text is **not** streamed live; it is buffered.* Interactive chat advertises six tools by default, so `print_fragment` is never called on a single-turn answer.
- The buffered answer is printed by **`kitsu_render::print_message`**, which does `let mut out = io::stdout(); out.write_all(…)` **directly**, inside `with_live_row_suspended`, **never through the surface seam**. Verified at `kitsu-render/src/lib.rs:439`.
- The one line the ring did get came from `finish_line`'s trailing newline: `"\n".lines()` is `[""]`.

**So the ring carries the *streamed* conversation, not the rendered conversation** — and **what lands in it depends on whether tools were offered that round.** Nobody would guess that from N4, and it is the fact a user scrolling back in fullscreen will meet: *the same answer is in the ring or not depending on whether the model was offered tools.*

This also explains **F51** from the other side: with tools enabled an ordinary answer does not stream, which was recorded as a product decision about *visible* behaviour. It turns out to decide what the scrollback **contains** as well.

**The test ships with `--no-tools`**, which makes the answer stream through `print_fragment` → `write_block` → the ring, using harness machinery that already existed.

**A third discovery, written into the script so nobody re-derives it:** triggering `/tui` on the turn-done marker is **not** enough. That summary line is printed *by* the turn, so a line sent the instant it appears still reaches the mid-turn collector and is refused with *"~ /tui is accepted only between turns"*. `/quit` is not gated that way, which is why every other streaming script can trigger on that marker and this one cannot; it uses the harness's own bounded pause instead.

**Non-vacuity here is a measurement, not an argument.** This one assertion **failed twice today for two different reasons** — ring empty via `eprintln!`, ring empty via buffered `print_message` — and passes only with a surface-routed line present **and** a reader that pulls it out. A view that merely compiles fails it; a fed ring with no reader fails it.

**And the discipline worth copying:** it passed once *with* the second debug print still in place, so the agent re-ran after removing it rather than trusting that green. **A pass obtained with instrumentation in the tree is not a pass.**

## F95 — my own heredoc ate a word out of a cycle record

Small, mine, and worth one line because it is a tooling trap I have been walking past for fifty commits.

Cycle records are written with `cat > file <<EOF` — an **unquoted** delimiter, because the template interpolates a timestamp. That means **backticks inside the body are executed as command substitution**, and this project's prose is full of backticked identifiers. I have been escaping each one by hand; this time one escaped my escaping, and `` `fullscreen` `` was run as a command. Bash said `fullscreen: command not found`, the word was replaced with nothing, and the sentence shipped as *"brief said \" is the one spelling it lacks.\""* — in the very paragraph about reading code carefully.

Repaired, and the commit amended (local and unpushed).

**The fix for next time is not more care, it is a different tool:** compute the timestamp into the file afterwards, or use a quoted delimiter and substitute the one variable with `sed`/Python. **A hand-escaping discipline applied to dozens of occurrences per file will fail on one of them** — which is the same argument as **F82**'s (*a rule applied per-change is not a rule enforced*), in my own toolchain rather than the product's.

Nothing else was affected: the cycle record's remaining backticks were escaped correctly, the findings file uses a **quoted** delimiter throughout, and no source file is written this way.

### F94 corrected — the hole is not fullscreen's, it is the overlay's, and it is five cards old

I recorded F94 as *"a dying signal or panic in **fullscreen** leaves the alternate screen taken"*. **The scope was wrong, and understating it made it look like a new mode's teething problem.**

Verified: `ESC[?1049l` appears in exactly **three** places — the fullscreen `LEAVE_ALT_SCREEN` const, one doc comment, and the **agent-view overlay's normal-path restore** (`main.rs:8870`). The dying-signal handler writes `SIGNAL_RESTORE`:

```
b"\x1b[r\x1b[?25h\x1b[?2004l\x1b[23;2t"
```

— reset scroll region, show cursor, bracketed paste off, **pop title**. It gives back the title and **not the alternate screen.**

**So a `SIGINT` or `SIGTERM` while the agent view is open has left users in the alternate screen since T4-S2** — five cards before fullscreen existed. Fullscreen did not introduce this hole; **it inherited it, and made it longer-lived**, because a fullscreen segment lasts until the user switches away while an overlay is usually seconds.

**And my N5 amendment is therefore wrong too**: I wrote that the signal clause is *"met for inline, open for fullscreen"*. Correct reading: **met for inline** (inline never takes the alternate screen, so there is nothing to give back), **open for the overlay since T4-S2**, and **open for fullscreen since T5-S2**.

### Why it went unnoticed, which is the useful part

`SIGNAL_RESTORE` already pops the title — so the handler *looks* like it handles screen-level state, and it was written before anything in this binary took the alternate screen. **The overlay added a new terminal mode and did not revisit the one place that undoes terminal modes on the way out.** Nothing failed, because no test kills the process mid-overlay.

**That is F79's rule in reverse.** F79: *when a function starts returning something new, every caller is a suspect.* Here: **when a surface starts taking new terminal state, every exit path is a suspect — and exit paths are enumerable too** (normal return, `/quit`, a dying signal, a panic). The overlay covered one of four.

### The constraint any fix must respect

The handler's body is **deliberately async-signal-safe**, and says so: an atomic load, one `write(2)` of a `const` buffer, a `tcsetattr`, no allocation and no locking — explicitly *not* reusing the panic helper because that one writes through a locked `Stderr`. **A fix may add bytes to a `const` and may read an `AtomicBool`** — the handler already loads two atomics — **and may not allocate, lock, or format.**

## F96 — a session started in fullscreen installs no dying-signal handler at all

Found by the F94 test author while deciding how to enter fullscreen in a script, and **it is strictly worse than F94.** Verified by me:

- `install_exit_restore()` is called **only** inside `if installed.is_some()` (`main.rs:19478`).
- `install` returns `None` for `TuiMode::Classic | TuiMode::Fullscreen` (`tui_bridge.rs:155`) — fullscreen pins no region, which is deliberate.
- So **`--tui fullscreen` / `[tui] mode = "fullscreen"` never installs the handler.**

A `SIGINT` or `SIGTERM` in that session therefore gets the **default disposition and no restore of any kind**: no scroll-region reset, **no `tcsetattr` back to canonical termios**, no title pop, no alternate-screen release.

**The termios half is the serious part.** F94 leaves a user on the alternate screen — recoverable with `reset`, confusing until then. **This leaves the terminal in raw mode as well**: no echo, no line editing, `Ctrl+C` inert. The user's shell is effectively unusable until they type `reset` blind.

**And it shipped in T5-S2, one card ago** — mine, accepted by me.

### Why neither the card nor its review caught it

T5-S2's four transition tests all **switch into** fullscreen from another mode, so the inline segment had already installed the handler. The two scripts that start in fullscreen (`--tui fullscreen`) assert the alternate screen is taken **before any input** — they exercise startup and then exit normally, which is the one exit path that does not need the handler.

**The gap is the intersection of two correct decisions:** fullscreen pins no region (so `install` is inert, which is right), and the handler is installed where the region is (which was right when the region was the only thing needing restoring). **Nothing is wrong with either; the conjunction is wrong**, and conjunctions are what neither a per-card test nor a per-requirement review looks at.

That is the same shape as **F94** — a surface took new terminal state without revisiting the exit paths — one level up: here a surface took new terminal state while **declining the mechanism that owns the exit paths.**

### Scope decision

**Folded into F94's card**, for the reasons that folded F79 into F77's: same mechanism (what the dying handler owes and when it exists), same files, and shipping a known "your terminal is left unusable" defect while fixing the lesser one beside it would be indefensible. The implementer is authorised one additional test for it.

**Note the asymmetry it creates for the fix:** F94 needs the handler to *write more*; F96 needs the handler to *exist at all*. A gated flag solves the first and does nothing for the second.

### F96's termios half had a second cause, found by the new test's first run

My record said the terminal is left raw because the handler is never installed. **True, and insufficient.** Installing it was not enough, and the implementer's first run of the new test proved it: the handler ran and wrote the full restore — so F94's gating worked — while the pty still read `ICANON=false ECHO=false ISIG=false`.

**Why:** `CANONICAL_TERMIOS` is published only by `RawStdinGuard::engage`, and a session started in fullscreen installs no inline viewport. **Its raw mode is rustyline's own**, so nothing in this process holds the modes rustyline displaced, and the handler's `valid` load is false. The handler had a snapshot to restore in every case anyone had tested, and **none** in the case F96 describes.

The fix gained an `else` branch: `tcgetattr`, OR `ICANON|ECHO|ISIG` back on, `tcsetattr` — both on POSIX's async-signal-safe list, the struct a stack local, and a failing `tcgetattr` leaving the terminal exactly as the signal found it. **It is not the shell's full termios — nobody in this process knows that — but it is the difference between a usable shell and `reset` typed blind.**

**The lesson: "the handler is missing" and "the handler has nothing to restore" are different defects with the same symptom**, and fixing the first leaves the second looking fixed. The only thing that separated them was a test that asserted the **terminal's observable state** rather than the bytes the handler wrote — the bytes were right and the terminal was still broken.

### And two things the implementation got right that I had not specified

**The flag's asymmetry is deliberate:** set **before** the take, cleared **after** the give, so no window can read "free" while the screen is held. The reverse window costs at most one stray leave on the primary screen; **the window it excludes is the defect itself.**

**`SIGNAL_RESTORE_WITH_SCREEN` is composed at compile time** from the two existing constants rather than written out — so there is no third copy of either sequence and no new drift surface, and `SIGNAL_RESTORE` stays a **prefix**, which is what keeps an existing `contains` assertion and a byte-parity test green without being touched.

**The double-restore case was handled too:** a main-thread panic in fullscreen would have had both `Drop` and the panic hook writing a leave, so `write_restore_sequences` **clears the flag after writing** — one take still yields one leave. The implementer also verified the overlay **cannot nest inside** fullscreen, so a single boolean has no nesting case to get wrong today.

## F97 — fullscreen paints one frame and never repaints, and my docs oversold it

Found while scoping the paging card, by asking where fullscreen reads input. **It does not read input, and it does not repaint.**

Verified: `FullscreenView` is constructed in exactly **one** production place — inside `enter_fullscreen` (`tui_bridge.rs:334`) — which paints a single frame and **returns**. `FULLSCREEN_ACTIVE` is read only by `live_mode()` and the exit restore. There is no loop, no repaint, and no output routing.

**So `/tui fullscreen` currently means: take the alternate screen, clear it, print the ring's tail once, and then carry on writing to that screen normally.** The frame is a **snapshot**, and the conversation scrolls over it from the moment the next line is printed.

### What I wrote about it, which is true and incomplete

The CHANGELOG says it *"takes the terminal's alternate screen and paints the viewport's scrollback"*. Accurate — **once**. `docs/functionality.md` says it *"paints the retained lines"* and that *"paging is not wired, so the view shows the newest lines that fit"* — which reads as *the view keeps showing the newest lines*, when in fact it shows the lines that fitted **at the moment you switched**.

**I accepted "B without paging is coherent — it shows the tail, which is strictly more than today."** That was true of the frame at entry, and I did not ask what happened to it afterwards. **A one-shot paint and a live tail are different features, and "shows the tail" describes both.**

### Why no test caught it

T5-S2's four transition tests all assert what is on screen **around the switch** — the alternate screen is taken, the marker is repainted, the screen is given back. **Every one of them is satisfied by a single paint.** The repaint assertion I was most proud of — the marker appearing again after the alternate screen opens — is *also* satisfied by one frame. **A test that proves the ring was read does not prove the ring is being read.**

### Scope consequence for T5-S1c

The paging card is therefore **not** "decode four keys". Paging is meaningless without something that repaints on demand, and a mode that freezes is worth fixing whether or not paging lands. **And there is a prior architectural question I had not seen:** with no input loop, fullscreen leaves the composer reading underneath — so paging keys have nowhere to live yet.

Two shapes, and the choice decides what fullscreen *is*:

- **A modal viewer** with its own read loop, like the agent-view overlay. Paging is natural; but **R2 calls `/tui` a renderer switch that keeps the conversation**, and a modal viewer you cannot type into is a different thing.
- **A live renderer**: the composer keeps reading, the frame repaints as the ring grows, and the paging keys are decoded in the composer's key filter beside the existing chords. This matches "renderer" — and note those chord keys are **deliberately outside** `keys::BINDINGS` because their labels are not static (§4.7), which may mean my instruction to build "a second table with its own sweep" was aimed at the wrong surface.

**Not decided here.** The next card's test author should settle it with the requirement in hand, and the docs need qualifying either way.

### F97 sharpened — the frame is not merely frozen, rustyline paints over it

I recorded fullscreen as painting one frame that *"the conversation scrolls over"*. **Measured by the next test author, it is worse:** the failure dump shows **rustyline's own `ChatHelper`** painting `kitsu>`, the `╰ model … ╯` hint row and the **full `╭ slash commands ╮` palette** directly onto the frame.

**Why:** `read_idle_chat_line` reaches the inline composer only inside `if let Some(bridge) = bridge`, and `install` returns `None` for `TuiMode::Fullscreen`. So **a fullscreen session's idle prompt is rustyline, in cooked mode** — no `RawStdinGuard`, no `KeyFilter`, no chord decoder. Verified structurally.

**Two consequences.**

**My own brief's second option was impossible.** I offered *"the paging keys are decoded in the composer's `KeyFilter` beside the existing `Ctrl+X` chords"* — **that surface does not exist in fullscreen.** Giving fullscreen a raw composer is a prerequisite card in its own right, and a terminal-state one: raw termios, bracketed paste, masked `ISIG`, `Ctrl+C`-as-byte, and a new exit path per **F94**/**F96**.

**And the docs I wrote are not merely incomplete, they are wrong about the experience.** "Paints the viewport's scrollback" describes a frame a user never sees intact.

### The decision, and what it kills

**Live renderer, not modal viewer** — argued against the requirements rather than taste: R2 says the switch keeps *the conversation*, and `--tui fullscreen` **starts** a session, so *"a start mode you cannot talk to is not a renderer, it is a pager that must be dismissed before kitsu is usable"*. R1 already calls fullscreen a **"self-scrolled transcript"** — self-scrolled is the repaint, not a read loop. And the modal shape already exists as the agent view, which hard-declines unless the mode is inline.

**So the plan row's "a second table with its own sweep" is not to be built**, which confirms what I suspected when I wrote it. §4.7 keeps the chords out of `keys::BINDINGS` because they never reach the swept dispatch and their labels are computed from a configurable prefix; paging keys on that surface inherit both, **plus a third reason — they are live only while fullscreen holds the screen, so a static row would advertise them in inline and classic where they do nothing.** The honest advertisement is `/tui`'s own `details` string, which R2 already pins across four spellings and T4-S10's test already asserts.

### The assertion that catches F97, which is the shape to copy

`raw_count(RING_ALPHA) >= 3`. **A single paint can produce exactly two** — the inline transcript's write, and the entry frame. Nothing else in the process holds that line. **A third occurrence is a repaint.** It is deliberately silent about *how* the new line arrives, so both plausible wirings pass and the implementation stays open.

Paired with a guard that inline and classic paint each line **exactly once**, which refuses the tempting wiring: repainting at the **surface seam** would duplicate inline's transcript and break classic's byte-identity.

**This is F97's own lesson turned on the test that missed it:** the existing assertion proves the ring **was** read; the count-of-three proves it **keeps** being read.

## F98 — a third variant of the restore hole: the `/tui fullscreen` arm registers nothing

Found by T5-S1c-ii's test author while enumerating exit paths, and verified by me: the `TuiRendererRequest::Fullscreen` arm is `enter_fullscreen` plus an `eprintln!` — **and nothing else.** It does not call `install_exit_restore`.

So the **classic → fullscreen** route leaves the session holding the alternate screen on the **default signal disposition**. A `SIGTERM` there writes no restore of any kind.

### Why three cards in a row each found one of these

- **F94**: the dying handler wrote the title pop but not the alternate screen — open for the **agent view since T4-S2**.
- **F96**: a session **started** in fullscreen installed no handler, because the handler was installed where the pinned region was and fullscreen pins none.
- **F98**: the **`/tui` switch into** fullscreen installs no handler either, for the same reason one level along.

**Every existing signal test enters fullscreen by a route that happens to have a handler already** — from inline (the inline segment installed it) or by starting in fullscreen (F96's fix installs it for the mode). **The one route nobody had taken is the one with the gap.**

That is the enumeration lesson biting a third time, and the sharpest statement of it so far: **exit paths are enumerable, but so are the paths *into* a state — and a restore installed on entry is only as good as the entries that install it.** F94 taught me to enumerate exits. F98 is about enumerating **entrances**, because each one decides whether the exit machinery exists at all.

### Kept in T5-S1c-ii rather than carded

Three reasons: it is the **same mechanism** (exit-restore registration), it is **one test**, and **it becomes a termios hole as well** the moment that card gives fullscreen raw mode — which is precisely what it does. Splitting it would mean shipping a card that *creates* the second half of a defect it had already found.

## F99 — an agent applied F97's lesson to its own tests, twice, before review

Worth recording as a method observation rather than a defect.

T5-S1c-ii's test author **stopped twice on its own work**, both times because a test would have been **green and proved nothing**:

1. Its first frame assertion was *"a keystroke must not cost the frame a row."* **Measured:** rustyline costs **zero** rows for a plain draft, because it rewrites prompt and hint in place. The defect only bites when a **palette** opens — so the test gained a slash prefix, and asserts **after** dismissal.
2. Its **decoder test passed on its first run.** Asserting only that a draft is painted under the frame is something **rustyline satisfies perfectly well**. It replaced that with a per-keystroke window assertion tying the paint to *not rustyline*.

**Both were discarded rather than kept as green "failing tests"** — which is the trap **F97** named and which I only caught *after* accepting a card. This is the first time an agent caught it in its own tests before handing them over, and it did so by **measuring the current behaviour first** rather than reasoning about what ought to fail.

**The generalisable practice: before writing a failing test, measure what the current code actually does to the thing you are about to assert.** A test derived from a description of the defect can be satisfied by the defect; a test derived from a measurement of it cannot.

The discriminator it chose is the other half of the lesson: the `model … cwd` hint row has **exactly one producer**, whose only caller is rustyline's hinter, and the inline painter never writes it. **That row on the alternate screen *is* rustyline reading the surface** — a by-construction fact rather than a substring guess (**F41**).

## F100 — `Ctrl+Home` is already `Home`, and that makes the inline negative load-bearing

My brief for T5-S1c-iii warned that `Ctrl+Home`/`Ctrl+End` *could* collide with plain `Home`/`End`. **Measured, it is not a risk — they are already the same key.** Verified at `kitsu-editor/src/lib.rs`:

```
b'C' => match self.csi_params.as_slice() { b"1;5" | b"1;3" => move_word_right, … }
b'D' => match self.csi_params.as_slice() { b"1;5" | b"1;3" => move_word_left,  … }
b'H' => self.move_home(outcome),      // no parameter match
b'F' => self.move_end(outcome),       // no parameter match
```

`C` and `D` **inspect the parameters** to distinguish a word jump from a plain arrow. `H` and `F` **do not**, so `ESC[1;5H` and `ESC[H` are indistinguishable to the editor: **`Ctrl+Home` moves the caret home today, in every mode.**

And `ESC[5~`/`ESC[6~` are **swallowed** — the `b'~'` arm has no `5` or `6` parameter and falls through — so **`PgUp` currently does nothing at all.**

### The consequence, which is the finding

**An implementer who recognises these four sequences *unconditionally* in the shared reader would pass every positive test on this card and silently break `Ctrl+Home` for every inline user.** The inline negative — *these keys do nothing new in inline and classic, and `Ctrl+Home` still moves the caret* — is therefore **load-bearing rather than decorative**, which is not how a "nothing regresses" guard usually reads.

**And "PgUp does not disturb the draft" is green today**, because the editor already ignores it. **F99**'s trap on this very card: the test author measured first, found that assertion vacuous, and made every red test pin *delivery* or the caret instead. A card that asserted only inertness would have shipped nothing and passed.

### Two corrections to my brief

- **The comments I cited** as the reader's keymap documentation are in the **test** `inline_read_delivers_the_editor_keymap`, not in the reader. Substance right, location wrong.
- **"The view's anchor is already there" was false.** `FullscreenSegment` held **no** view, and `fullscreen_frame` constructs `FullscreenView::new()` **fresh on every paint**, so no anchor survived a repaint. Paging state is **new** state on the segment, not relocated state. Verified.

**Fourteenth and fifteenth mechanism errors in my briefs.** Both reported rather than worked around, and neither invalidated the card — but the second one matters, because "relocate existing state" and "introduce state with a lifetime" are different jobs and I had described the harder one as the easier.

### One design choice worth keeping

Paging delivers through a **sink closure**, not a new reader outcome, and the reasoning was measured rather than argued: **an outcome ends the read**, and re-entry replays the draft as *bytes* through a cursor, which leaves the caret at the end. So paging-by-outcome would **jerk the caret to the end on every press**, and hand each press a fresh editor, a fresh key filter and a lost reverse-search. A sink keeps one read, one editor and one draft across a whole run — pinned as a **count**: ten keystrokes, ten presses, in order (**F97**'s shape).

### F100's resolution — and the best non-vacuity proof this epic has produced

The anchor test's middle assertion was **arithmetically unsatisfiable**: 60 lines in a 60-limit ring retains all 60, window 23, `follow_top = 37`, `page_step(23) = 22`, so one `Page::Up` lands on ROW_16..ROW_38. Reaching ROW_01 needs a **1.6-window jump** — `Page::Oldest`'s transition, not `Up`'s — and one-window-per-press is pinned **green** in `kitsu-tui`. **Satisfying it would have meant redefining the key and breaking two passing tests.** Reproduced independently by me. The implementer stopped rather than bend the product: the **ninth** unsatisfiable assertion in this epic.

**I rejected both fixes it offered, and the reasons generalise.**

- *Assert the row the step actually reaches* — it would **hard-code `page_step` into a second place** when `kitsu-tui` already pins it. Two homes for one arithmetic fact is this epic's oldest failure mode.
- *Shrink the ring so one press does reach the oldest* — it makes the claim true **by fixture**, and the test would then read as *"one PgUp reaches the oldest line"*, which is false of the product at any realistic size. **A test that is only true for its fixture is worse than one that is wrong, because it looks like a specification.**

**The authorised fix asserted the intent instead** — the anchor's *lifetime*, not the step's size: the press moved the frame off the newest line, the paged frame **differs from the entry frame by value**, and then the claim the test's own name promises and nothing else covered — **after a repaint, the same frame is painted again.**

### The proof, which is the part worth copying

The implementer made that assertion **non-vacuous by mutation**: it reverted `repaint_if_the_ring_grew` to the `FullscreenView::new()` form it had before the card, re-ran, and got

```
assertion `left == right` failed: a repaint must come from the segment's anchor
and not from a fresh view: same anchor, same rows.
```

then restored it. **So the assertion demonstrably covers the one-line change it exists for** — a change that was previously untested, because the fresh-view construction had passed every earlier test.

**And the seam it needed already existed.** I had said "if pinning this needs a seam you do not have, stop again rather than weakening it". It didn't: `through_the_seam` writes to `active_surface()`, which **while the segment lives *is* its own tap** — so the ring grows and the repaint fires through **production's own gate**, with no test-only hook. That let the assertion pin the **whole frame equal by value** rather than the top row I had asked for, which is stronger and still names no number.

It also added an assertion nobody asked for: `!segment.is_following()` after the growth, pinning that **append-while-following is suppressed while pinned** — transition 10's negative, which the view's own table describes and nothing had checked from the production side.

**Three design choices recorded as right:** the **sink** rather than a reader outcome (an outcome ends the read, and re-entry replays the draft as bytes, so paging-by-outcome would jerk the caret to the end on every press and hand each one a fresh editor and a lost reverse-search); **the gate being the sink's presence** rather than a flag — *a surface with nowhere to send a press has no business lifting one out of the stream*; and **matching the full byte string including parameters**, which is the only thing keeping `Ctrl+Home` separable from `Home` given that the editor cannot tell them apart.

## F101 — the plan's "files" column named a directory that never existed, in four rows across three stories, and the one catch never propagated

Nothing checks the plan's file column against the tree, so it drifted into fiction unnoticed:

| Row | Named | Reality |
|---|---|---|
| T3-S7 | `kitsu-tui/src/overlay/approval.rs` | **caught at review**, recorded as *"impossible"* — `kitsu-tui` sees only `kitsu-render` and cannot name `PermissionRequest` |
| T4-S4/S4b | `overlay/agents.rs` | **shipped as `agents_view.rs`** — flat, and never corrected |
| T4-S9, T5-S3 | `overlay/transcript.rs` | unbuilt; **no `overlay/` directory has ever existed** |

`crates/kitsu-tui/src/` is and always was flat: `agents_view.rs`, `cells.rs`, `composer.rs`, `fullscreen.rs`, `keys.rs`, `lib.rs`, `status.rs`, `viewport.rs`.

### The finding is not the wrong path, it is that finding it once fixed one row

T3-S7's review **did** catch this, in the strongest terms — *"the original row's `kitsu-tui/src/overlay/approval.rs` was **impossible**"* — and wrote the reason into the plan. **Then I shipped the agent view into a differently-named file and left its row saying `overlay/agents.rs`,** and briefed two later cards off rows naming the same fictional directory.

**This is F91's shape from the other direction.** F91 was *a false entry travelling four hops*; this is **a true correction failing to travel at all**. The same cause: a document is corrected where the eye is, and its siblings keep the old claim. Two instances in two cards — the surviving `TuiRuntime::enter` claim in a struck-through row, and this.

**Cheap guard, applied now:** every `` `dir/file.rs` `` in the plan, resolved against `find crates -path`. Two misses, both `overlay/`. It is one command and it had never been run.

**And note which way the error cuts.** A plan that names a file which does not exist invites an implementer to *create* it — so a wrong path does not fail loudly, it **grows a directory the crate's convention does not use.** The agent view escaped only because whoever wrote it ignored the row.

## F102 — the ring holds no prompts, so the transcript overlay would show one speaker

Verified independently. `CellLine` carries **only** `fd: Fd` and `styled: String` — no role or kind marker — and production pushes into a `CellRing` from exactly two `transcript_line` paths (`viewport.rs` for inline, `FullscreenTap` for fullscreen); the remaining `push` sites are test fixtures or `frame_ring` rebuilding a ring from lines it already holds.

**The user's submitted line never becomes a transcript line.** The composer paints it and then `clear_composer` erases it. So the ring contains assistant stream on `Stdout` and tools/diagnostics on `Stderr`, **and nothing the user typed.**

### Why this is a requirement finding and not a card detail

**N4** makes the ring *"the sole source for the transcript overlay and the `[` dump"*. So:

- **`{`/`}` prompt jumps are unsatisfiable** — there are no prompt boundaries to find. R6-c is **blocked on a feed change**, on the surface T5-S1b owns, not on overlay work.
- **The whole overlay shows half a conversation with one speaker missing.** That is a user-visible surprise, not a docs footnote, and it is **wider than F92 said**: F92 recorded that diagnostics and tool-deferred answers are absent; prompts are absent too, and they are the user's own words.
- **Inferring prompts from `Fd` is refused.** `Stderr` is tools and diagnostics, not the user. A heuristic here would be a rule whose narrowest input path defeats it (**F70**'s shape).

**The test author declined to write a `{`/`}` test**, on the grounds that the only way to make one pass today is to push a fake `"> prompt"` line in the fixture — *the "true only for its fixture, so it looks like a specification" failure*, which is now the second card running where that rule did work.

## F103 — `[` dumps to the one thing the alternate screen does not have

R6 says `[` dumps the ring *to native scrollback*, **fullscreen only**. Those two clauses are nearly contradictory: **bytes written while `ESC[?1049h` is in effect are discarded by the terminal on `ESC[?1049l`.** A dump performed while fullscreen holds the screen therefore goes **nowhere**.

So "fullscreen only" cannot mean *performed while fullscreen holds the screen*. It can only mean the key is **offered** only there, and the dump **leaves the alternate screen first** or defers to the leave. **That makes `[` a terminal-state hand-off, not a write** — the same kind of thing as `v` spawning `$EDITOR`, which is why grouping them in one card is right rather than merely convenient.

**And `[` is where the ring's gaps stop being a display matter.** A dump into native scrollback *looks* like a transcript the user can scroll back through and trust. With prompts absent (**F102**) and diagnostics absent (**F92**), its output should say what it is.

## F104 — a tenth flake, named: the attach-recap test

`tests::the_recap_holds_only_what_the_foreground_produced_while_attached` (`crates/kitsu/src/main.rs`, `--bins`) failed **once** in a full run and then passed **13 consecutive times**; it passes in isolation, so it is order/parallelism-sensitive — consistent with the **process-global active-surface slot** the recap/attach path reads.

Proved not to belong to the card running at the time: **6 consecutive full `--bins` runs skipping that card's tests were 937/0**, and 8 runs with them were exactly 937 + the card's 2.

This is **F88**'s test under a second, independent sighting, so it is now listed under its own name rather than folded into the earlier count. **Ten open flakes.** T4-S6c introduced the process-global slot this one touches, and two of the ten now point at it — the strongest signal yet that the slot, not the tests, is the defect.

### F101 confirmed by its own guard, twice in one sitting

Having recorded that *a true correction fails to travel*, I restructured R6 into four rows and then deliberately looked for the siblings. Both existed, in the requirement-to-story map at the foot of the plan:

- `| R6 | T4-S9, T5-S3 |` — pointing at **both rows I had just retired**.
- `| R2 | T3-S9, T5-S2 |` — pointing at a row I **deleted earlier the same session** as a stale duplicate.

So the plan had a dangling reference for hours, created by my own correction, and a second one created minutes after writing the finding that predicts it. **The sweep is now a command and not an intention:** resolve every story reference in the map against the rows that actually exist. Its first run found one more thing — a reference I had just written myself, spelled `T5-S1b+T5-S2` where the row says `T5-S1b + T5-S2`.

**The lesson sharpens.** F101 said a correction does not propagate. The sharper version: **a correction *creates* dangling references, so the act of fixing a document is itself the thing most likely to break its siblings.** Editing is not a safe operation on a cross-referenced document, and the only guard that holds is a reference check that runs after the edit.

## F105 (REJECTION) — `Esc` did not close the overlay, and the sweep that should have caught it tested the wrong thing

R6-a was rejected on a defect the implementer had flagged as a judgement call but described more mildly than it was: *"ESC resolves as close only when nothing follows it in the chunk."* The partial-hold runs **before** the ESC arm, and `KeyFilter::is_partial` is `data.len() < pattern.len() && pattern.starts_with(data)` — so a bare `ESC` is a strict prefix of all four paging sequences:

1. `page_key(b"\x1b")` → `None`
2. `partial_page_key(b"\x1b")` → **`true`** → `held = [0x1b]`, `break`
3. the loop returns to `stdin.read(...)` and **blocks**

**`Esc` closed nothing until a second keypress arrived**, and the footer advertised `q/Esc close`. **F75**'s second kind: *a key that appears to work is a defect* — and advertised by the very in-situ footer chosen over a global table.

### Why the test that looked like it covered this did not

`q_and_esc_close_the_overlay_and_no_other_press_does` sweeps the **entire** key space — and it tests `kitsu_tui::transcript::closes(press)`, the predicate in the view crate. **The byte loop that must hand it an `Esc` was never exercised.** A total enumeration of the keyboard, proving the predicate answers `Esc`, while the thing that produces an `Esc` never ran.

**That is F69 verbatim**, and the strongest instance yet: F69's mirror diverged three times; here the test was not a mirror but a different *layer*, which looks even more like coverage. **A sweep over a key space is only as good as the layer it sweeps.**

### The fix, and the proof shape worth copying

The loop was extracted as `run_transcript_key_loop` returning `TranscriptExit { Closed, Drained }` (**F69**'s remedy: the test drives the real decoder), and the lone `ESC` is resolved by **the agent view's own `stdin_waiting(ESC_SEQUENCE_GRACE)`** — the existing function and the existing 50 ms constant, verified defined **once** in the tree. The honest signal for *"lone `ESC` or the first byte of a sequence?"* is time, not a chunk boundary; the chunk boundary is not a signal at all, as the trace shows.

**Bidirectional non-vacuity, which is the thing to copy:**
- pre-fix decoder restored → the two lone-`ESC` tests fail `left: Drained, right: Closed`
- `ESC [` skip disabled → the split-arrow test fails `left: Closed, right: Drained`

So one test fails if `Esc` does not close, another fails if an arrow *does*. **Asserting on the exit reason means the defect's own value cannot satisfy either** — the two failure modes are opposite and both are pinned, which a single-direction proof cannot do.

## F106 (second rejection, same card) — the key that opens the overlay was advertised nowhere

Every `Ctrl+O` in the tree was a comment or a test string. **F82** verbatim — its sweep found `q`, `Enter` and `→` dispatched and advertised nowhere, and its lesson was *a rule applied per-change is not a rule enforced*.

**The footer does not cover this**, and the distinction is the finding: the footer advertises the keys live *inside* the overlay, so it is **only visible once the overlay is already open.** An in-situ advertisement cannot advertise its own entrance.

The location needed no new decision, only reading: `agent_view_keys_help` prints *"the agent view's keys (/jobs view, or ctrl+x a):"* and closes with *"the composer's own chords are listed by /help tui."* **So composer chords already had an established home**, and `Ctrl+O` is a composer chord — read by the one inline reader both surfaces use, exactly like the four paging keys T5-S1c-iii put in `/tui`'s `details`. Not `keys::BINDINGS`, which is the agent view's table.

The test restates the footer's own guard on the new string: the details must not contain `search`, `dump`, `editor`, `scrollback` or `jump`, so **the advertisement cannot promise R6-b/c/d either** — the failure mode of the three documents once committed saying `/detach` worked (**F77**).

## F101, third instance — and this one was mine, in the file I had just edited

`docs/functionality.md` still read **"Paging is not wired, so the view shows the newest lines that fit"** — ten lines below the paragraph I added last card describing paging working, in the same file, in the same edit session.

So the count is: a false claim surviving in a struck-through plan row; a fictional directory in four rows with one catch that never travelled; two dangling map references created *by* my corrections; and now **a sentence contradicting its own neighbour because I added the new text instead of searching for what it falsified.**

**The sharpened rule, now applied:** after changing a document, grep it for the claim the change makes false — not just the place the new text goes. Adding is not the same operation as correcting, and I keep performing the first while believing I have done the second.

### F101's sweep has one known false positive — recorded so it is not re-chased

The reference check reports `T5-S2` dangling. It is not: the row exists as the **compound** `| T5-S1b + T5-S2 |`, and the sweep extracts `T5-S2` from the map's `T5-S1b + T5-S2 (merged)` as if it were a standalone reference, then fails to match it against a row key that *contains* rather than *starts with* it.

Left as-is rather than fixed: the sweep's job is to be run after every edit, and a known, documented false positive is cheaper than a cleverer matcher that might mask a real one. **Noted here so the next run does not spend a card on it.**

## F107 — the flakes' shared cause, named: one process-global surface slot with sixteen readers

> **CORRECTED by F112, in three places.** `tui_bridge.rs:966` is `fn invalidate`,
> not the slot — the real one is `kitsu-render/src/surface.rs:162`, a different
> crate and never a `Mutex<Option<..>>`; the "16 readers" figure counts **nine
> test-module readers** and omits **twelve** in `kitsu-render/src/lib.rs`; and the
> recap test **was already `#[serial]`** when this was written, so the fix
> proposed here was already in the tree. **It is test-only** — no production
> defect, and the refactors contemplated here are all unjustified.

Read-only diagnosis, recorded for a future card rather than acted on, because the files are in flight.

**Two of the ten open flakes point at the same thing**, and it now has a name: `tui_bridge.rs:966` holds the installed surface in a **`std::sync::Mutex<Option<..>>` process global**, read through `active_surface()` at **16 sites** (11 in `tui_bridge.rs`, 5 in `main.rs`).

`cargo test` runs a crate's tests **in threads of one process**, so every test that installs a surface writes the **same slot** every other test reads. That is sufficient on its own to explain:

- **F88/F104** — `the_recap_holds_only_what_the_foreground_produced_while_attached`, which fails under parallelism, passes in isolation, and has now been sighted **twice independently**, in two different cards' runs.
- **F80** — three `approval_modal_*` tests in `kitsu-render --lib` release painting nothing (`left: []`), during a run **concurrent with another cargo test**.

### Why this is worth a card and not a `--test-threads=1`

The suite is already run single-threaded where it has to be (`--test tui`), and reaching for that flag more widely **hides the defect and costs wall-clock on every future run**. The honest fixes are to scope the slot per-test or to make installation explicit in the tests that need it — and the second is already the better-tested shape: the most recent cards drive `install_inline_with_ring` and `through_the_seam` deliberately, which is why their assertions are reliable while the recap test's are not.

**It also explains why the recap test alone keeps surfacing:** T4-S6c introduced the foreground/recap path, which *reads* the slot to decide what the foreground produced. A test whose subject is "what was installed at the time" is maximally sensitive to a global that another thread can rewrite.

**Not a card yet, deliberately** — it touches `tui_bridge.rs` and `main.rs`, which the prompts-in-the-ring work holds. Carded next, with the measurement above as its starting point rather than a fresh hunt.

### A third global, found the same day, and this one has a test that panics on purpose

`kitsu-tui`'s `ALTERNATE_SCREEN_TAKEN` (`lib.rs:215`) is a process-global `AtomicBool`, and the restore path does:

```rust
if alternate_screen_taken() { out.write_all(LEAVE_ALT_SCREEN)?; note_alternate_screen(false); }
```

Two tests in that module share it and **neither was serialised**:

- `the_restore_stream_gives_the_alternate_screen_back_only_when_it_is_taken` sets the flag, then asserts the restore stream carries `ESC[?1049l`.
- `install_panic_restore_chains_the_previous_hook` **panics deliberately** — that is its whole subject — and the installed hook **clears the flag**.

So the panicking test clears the other's flag between its set and its assertion, and the count goes `1 -> 0`. **The failing test's own doc comment already noticed the hazard** (*"the flag is process-global, so it is put back before returning"*) **and guarded the wrong end of it**: restoring a global on the way out does nothing about a concurrent writer.

**Proved latent rather than introduced**, by the right instrument: **3 sightings in ~145 runs** with new unrelated tests present, **0 in 120 runs at pristine HEAD** in a detached read-only worktree, **0 in 40 runs** single-threaded. New tests that touch no shared state perturbed the **scheduling** of a race that was already there.

### What this does to the finding

**Three process-globals, three flake families, one shape.** The pattern is not an unlucky static; it is that this binary keeps per-process state for things a test wants to own, and `cargo test`'s threads make every such static a shared mutable. A test-local mutex is going in on the two tests above as a **mitigation, named as one in a comment** — the production global remains the defect.

**And a lesson about the fix, not the bug.** The obvious remedy, `#[serial]`, **would not compile**: `serial_test` is not a dependency of `kitsu-tui`, whose `[dev-dependencies]` is `vt100` alone. The belief that it was available came from grepping for the **word** `serial`, which matched a prose comment about a lock — **F74** applied to a word match. A fix proposed from a grep is a hypothesis about the build, and this one was false.

## F108 — the end-to-end link no in-process test can see, proved by mutating a live binary

**F102 is resolved** and the mechanics are in the cycle record. What belongs here is **how one claim got proved that nobody had been able to prove.**

`an_injected_priority_command_is_never_marked_the_users_prompt` asserts that **no** retained line is marked. That is satisfied by *"retained unmarked"* **and** by *"not retained at all"* **and** — the problem — by *a feed that never fires*. The test author said so explicitly and declined to claim more: the `PromptOrigin` plumbing *could not* be mutation-proven before the feed existed, because the origin's journey from its capture point to the feed crosses a `match` that builds the line and is not observable from any seam a unit test can reach.

**The implementer proved it in a live process, in two halves:**

1. **Both arms are load-bearing, bidirectionally.** Making the `Injected` arm feed → exactly **one** failure, that test. Making the `User` arm inert → exactly **six**, the feed tests. So neither arm is dead and the two are not swapped.
2. **The capture point really determines what arrives.** With a temporary probe, a real pty session logged `User /help`, `User /quit`. Then the single binding at the capture site was mutated to `= true`, the binary **rebuilt**, and the *identical* session logged `Injected /help`, `Injected /quit`. **The text was byte-identical across both runs**, so the origin demonstrably does not come from the text — it comes from the binding. Probe and mutation reverted; the file's diff is byte-identical to the author's.

### Why this is a finding and not just diligence

**A unit test can only prove what a seam exposes.** This project's strongest assertions have come from driving the real path rather than a predicate beside it (**F69**, **F105**) — but there are claims where *no* in-process seam exists, and the honest options are to under-claim or to leave the process. **Rebuilding the binary with one line changed and re-running an identical session is the cheapest instrument that spans a link a seam cannot**, and it produced a sharper result than any test on this card: a two-run comparison in which everything but one binding is held constant.

**And the under-claiming was correct.** The author's *"I am not claiming more than I measured"* is what made the gap visible and handed it to someone who could close it. A report that had asserted the plumbing worked would have buried it.

## F109 — a living document must be corrected; a cycle record must not

Resolving F102 falsified claims I had written **one commit earlier**, and the sweep found them in six places. The useful part is that they split cleanly into two kinds:

- **Living documents** — `CHANGELOG.md`'s `[Unreleased]` entry (*"your own prompts are absent from it"*), `docs/functionality.md` (*"It also does not hold your own prompts"*), R6's amendment, and the plan's R6-c row. **All corrected**, because they describe what the software does now and a reader has no way to date them.
- **The cycle record** `docs/roadmap/cycles/e11/R6-a.md`, which says *"the ring holds no prompts at all -- the overlay shows one speaker"*. **Left exactly as written.** It is a point-in-time record of what was true and what was decided at that card; editing it would destroy the only honest account of why R6-a shipped a one-speaker overlay and why R6-c was blocked.

**The distinction is the finding.** F101's rule — *after changing a document, grep for the claim the change falsifies* — is incomplete without it, because applied naively it would have rewritten history. **The test is not "is this sentence now false?" but "does this document claim to describe the present?"** A changelog entry in an unreleased section does; a dated cycle record does not, and its value is precisely that it still says what was believed then.

## F110 — F101's fifth instance: I fixed the sentence and not its twin in another file

Last card I found `docs/functionality.md` still reading *"Paging is not wired, so the view shows the newest lines that fit"* ten lines below the paragraph I had just added describing paging working. I fixed it, recorded it as F101's third instance, and wrote down the rule: **after changing a document, grep it for the claim the change falsifies.**

**I then did not apply that rule to the other document containing the same sentence.** `CHANGELOG.md` read, in the same entry, immediately after the paragraph describing the paging keys:

> Paging keys are not wired yet, so it shows the most recent lines.

Found three cards later, while pre-locating what R6-c would falsify — i.e. by a sweep run for a *different* purpose, not by the rule I had just written.

### The rule was wrong, not just unapplied

F101 said *grep **it***. Singular. **That is the bug.** The same claim is written in several documents because each audience needs it, so falsifying it falsifies every copy, and the file I happen to be editing is the one I am least likely to miss. The corrected rule:

> **After changing behaviour, grep every live document for the claim the change falsifies — not the document you are editing.**

Run as a family rather than a string: `not wired`, `is not wired`, `are not wired` across `CHANGELOG.md`, `README.md` and `docs/*.md` now returns nothing, where a sweep of only the file in hand would have returned nothing on the day I introduced this.

### The tally, which is the argument for automating it

Five instances in this epic, every one a *correction* that failed to reach a sibling:

1. A struck-through plan row still asserting the alternate-screen claim F91 had just disproved.
2. A fictional `overlay/` directory in four rows across three stories, one of which had been caught and documented as *"impossible"*.
3. Two dangling map references created **by** my own corrections, minutes after writing the finding that predicts them.
4. `functionality.md`'s *"paging is not wired"*, ten lines from its contradiction.
5. **This one** — the same sentence as (4), in the file I did not grep.

**The cost is not the wrong sentence; it is that a reader cannot tell which sentence to trust.** Two documents in this repo disagreed about whether paging existed, and both were committed by me, in the same release, for three cards.

**Carded:** a check that greps the live documents for a small set of negative-capability phrases (`not wired`, `not yet`, `still missing`, `is refused`) and lists them with their line numbers, so each is re-read rather than assumed at every card. Not a test — a sweep whose output a human reads, like the reference check F101 already produced.

## F111 — R6-c shipped; the limitation, and a third sighting of the recap flake

**R6-c is done.** `{`/`}` jump between coalesced prompt blocks, through a new shared seam `pin_to(ordinal)` that **R6-b's `/` will reuse** with a match ordinal — one anchor, one clamp, no second anchor concept. The pinned transition table grew to **twelve** rows and its header says so.

### The accepted limitation, with its two homes named

**Two submissions with nothing printed between them are one jump stop.** Adjacency is the only rule a `bool` per cell affords; distinguishing them needs a block id. Impact is small — a submission that prints nothing at all is rare — and it has **exactly one home in the code** (`CellRing::prompt_blocks`) **and one in the tests** (`cells::tests::two_prompts_with_nothing_between_them_are_one_block`), so the docs point at both rather than restating the rule a third time.

### The overrule, and why it generalises

The author's first answer let `}` pin onto a block already at the frame's top: the block *is* below the cursor, so reporting "nowhere to go" looked like a lie. **Overruled on the author's own principle** — they had already decided that *a jump with no target leaves `is_following()` alone, because pinning before discovering it has no target breaks the follow for nothing.* A target that clamps onto the cursor **cannot move the frame either**, so it is the same trade with the same answer.

**What makes it more than a preference:** the pinning answer is **invisible at the keypress and visible much later** — the user presses a key, sees nothing change, and new output silently stops arriving. **F75**'s second kind with a *delayed* symptom, which is harder to attribute than an immediate one. The author then wrote the assertion that catches it: after a declined jump, an appended line **must still appear**.

And the accepted rule turned out to be **direction-symmetric by construction** — a `Prev` target is strictly below the cursor and block ordinals are never evicted, so `Prev` can never clamp onto the cursor and needs no arm of its own. A rule that needs no special case is usually the right rule.

### Two implementation details worth keeping

**`pin_to` clamps *through* `top`** rather than writing the arithmetic again, by constructing a throwaway view at the raw ordinal and asking it for its top. So the clamp has **one** home, and the stored ordinal is clamped at call time — a raw pin would be re-clamped against a moving `follow_top` on every repaint. The author had already routed `frame` through `top` for the same reason, **unprompted**, which is the first time this epic's oldest failure mode was prevented rather than repaired.

**`prompt_blocks` gets partial eviction for free:** it emits `evicted() + index` for a marked cell whose predecessor is unmarked, and the first retained row is a head whenever it is marked — so a block whose head was evicted is reported at its oldest surviving row with **no special case**.

### Third sighting of the recap flake, captured

`tests::the_recap_holds_only_what_the_foreground_produced_while_attached` failed once in my verification run (`956 passed; 1 failed`), **passed in isolation**, and passed `957/0` on three consecutive full runs. Captured to its own file (**F39**).

**That is the third independent sighting**, in a third card, by a third party — the author of the card did not see it, the implementer did not, and I did. It remains exactly consistent with **F107**: a test whose subject is *what was installed at the time* reading a process-global that another thread can rewrite. **Three globals, three flake families, and this one test has now cost four verification runs.** F107's production fix is the card I keep deferring, and its interest is compounding.

### F110's sweep, first run: three more stale claims, one an internal contradiction

Built and ran as a command over the live documents — `not wired`, `not yet`, `still missing`, `is refused`, `cannot yet`, `does not yet`, `no support`. Most hits are legitimate descriptions of **deliberate** refusals, which is the point: the sweep lists candidates for a human to re-read, it does not decide. Three were genuinely false:

1. **`docs/ux-design.md`** — *"the bounded fd-tagged cell ring … **not yet fed in production because R6's overlay is its only reader**"*. **Doubly false:** the ring has been fed in production since T5-S1b (inline surface and fullscreen tap), and the overlay is **not** its only reader — the fullscreen frame reads it too. This is in a file whose **own rule** is that code and it are fixed in the same change, and it had drifted across several cards.

2. **`docs/functionality.md`** — *"`fullscreen` is refused until that renderer exists"*, **134 lines below the same file's own description of `--tui fullscreen` starting a session there.** An internal contradiction inside one document, which is the worst version of this: a reader comparing two files can at least date them, but two sentences in one file give no way to choose.

3. **`docs/tui-and-agent-view-design.md`** — the refusal pin itself, *"`fullscreen` is refused until T5's renderer exists"*. **Amended, not deleted**, and **F90** is the reason: *"X is missing" and "X is refused" look identical from outside and are opposite facts.* Deleting the pin would erase the only trace that the absence was deliberate and multiply enforced; the amendment records that it was lifted and preserves the rule that outlived it — a renderer kitsu does not have is refused **by name** rather than coerced.

**The sweep's value is not the hits, it is the ratio.** Of roughly twenty matches, three were wrong — so a human must still read each one, and any attempt to make this a failing test would be wrong, because *"is refused"* is usually the documentation of a correct decision. It belongs in the same family as F101's reference check: **a command whose output a person reads each card**, not a gate.

**And it found drift in a file no card this epic had touched.** The negative-capability phrasing is where documentation rots, because it is written once as true and nothing about shipping the capability makes anyone revisit the sentence that denied it.

## F112 — F107 was wrong in three places, one flake family never existed, and my "smaller fix" was already in the tree

The F107 card was assessed and **correctly returned as "do not build it as scoped"**. Every claim below is one I verified myself before correcting the record.

### 1. It is test-only. The globals are correct in production.

The surface slot has **six production writers and all are on `run_chat`'s own thread** (`install`, `BridgeGuard::drop`, `enter_fullscreen`, `FullscreenSegment::leave`, and the two foreground-buffering calls). Everything on another thread — the cadence painter, the approval prompter, orchestrator tasks — **only reads**, through `active_surface()`, which clones the `Arc` and drops the guard before returning. One process, one terminal, one `run_chat`. `subagent.rs`'s two `set_surface` calls are **test-only**, which had to be checked because concurrent subagents would have made this a real defect.

`ALTERNATE_SCREEN_TAKEN` is the same answer for a different reason: its two production restorers run **sequentially on the one panicking thread**, which is what the clearing read exists for.

**So there is no production defect, no user-visible entry, and the three refactors F107 contemplated — scope per test, thread it through, restructure installation — are all refactors of a correct global justified by a bug that cannot happen.** Threading it would touch 19 call sites across three crates.

### 2. Three factual errors in F107, which would have sent an implementer to the wrong file

- **`tui_bridge.rs:966` is `fn invalidate`, not the slot.** The real slot is `crates/kitsu-render/src/surface.rs:162` — `static SLOT: OnceLock<RwLock<Arc<dyn Surface>>>`, **a different crate, and never a `Mutex<Option<..>>`.** The `Mutex<Option<..>>`s near that line are `STATUS_PAINTER` (990) and `MODAL_PANE` (1001) — **two other globals that I conflated into one.** It matters for blast radius: the real slot is shared by **four test binaries**, not one.
- **"16 readers" is an exact count and a wrong claim.** **Nine of the sixteen are inside `mod tests`**; production readers are seven — and the figure omits **twelve more in `kitsu-render/src/lib.rs`**, which is the actual hot path. **F74 again, and this time in my own finding**: I counted name matches and reported them as call sites.
- **"The recap test does not drive installation deliberately" is false.** It calls `SurfaceProbe::install()` and **has been `#[serial]` since the commit that introduced it** — nine commits before its third sighting. **So the "smaller fix" I proposed in the brief was already in the tree when the flake was sighted three times.** That is why the card as scoped would have fixed nothing.

### 3. The actual cause: `#[serial]` excludes other `#[serial]` tests, not plain ones

**Four `#[tokio::test]`s reach the slot through *production* code** rather than through `SurfaceProbe` — painting the attach row *and* installing a `BufferedSurface` over whatever is there — and **none was serialised.** All 87 `#[serial]` tests and every `SurfaceProbe` user were already mutually excluded; the gap was exclusion against tests that install **through production**.

They reach it via module-local `one_read` helpers, **which is why a call-site grep misses them**. Found with **F108**'s instrument: one temporary line logging `std::thread::current().name()`, which under `cargo test` **is the test's name**. The failing run's log shows three different tests attaching concurrently, and the smoking gun is an id in the output that belongs to none of them — `agent 1`, where the test uses 7 and 9, is another test's fixture leaking through the shared slot.

**`SurfaceProbe`'s own doc comment states the invariant as "Tests using it are `#[serial]`" — true and insufficient** (**F77**: a comment is worth what its claims are worth). `tui_bridge.rs:3090` has the same blind spot, reasoning about *"two bridge tests overlapping"*.

### 4. F80 was never a flake. Remove it.

The `approval_modal_*` capture is a **misfiled FAILING-FIRST run of T3-S7a**, and every step is checkable at HEAD:

- `approval_modal` is a **pure function that unconditionally pushes the header**, verified — so `left: []` is **unreachable** for it.
- Its three tests pass an explicit `false` and **read no global at all**, which is why they are the only slot-free tests in that file taking no `surface_slot_lock()`. *"A shared global not yet installed"* cannot explain a test that reads no global.
- The capture shows `running 127 tests` with the three new tests failing **together**, every assertion empty — a stub. `git log -S"pub fn approval_modal"` returns **one** commit, which adds the function complete with its three tests. The "clean" comparison runs reported **130** tests: **a different binary.**

**So: three globals, but two flake families — and one of the two I attributed to the surface slot never existed.** I filed a card's own red run as evidence of a race, and it sat in the findings for many cards being counted.

### 5. A fifth guard, found by reading rather than by a sighting

`tui_bridge.rs:7331 the_handler_byte_string_equals_the_restore_helper_output` is a **non-serial mutating reader of `ALTERNATE_SCREEN_TAKEN` in the `kitsu` bin binary**: it asserts the signal string equals `write_restore_sequences`' output, which holds only while the flag is `false` — and the serial fullscreen tests set it `true`. **Bidirectional**: it also *clears* the flag, so it can make a fullscreen test's `leave` skip its `ESC[?1049l`. **Zero sightings in 821 runs** because the window is one synchronous enter→leave. Guarded anyway: a latent race derived by reading is cheaper to close than to wait for.

### The lesson, which is about findings files and not about mutexes

**A findings file is read by every later card as settled fact, so a wrong entry in it is worse than a wrong brief** — that was **F91**, written about someone else's error travelling four hops. **F107 was mine, and it travelled further**: it set a card's scope, it was cited in four cycle records, it counted a non-existent flake family for ten cards, and I recommended it as "the most valuable card left" one commit ago.

**What would have caught it: the same rule I apply to code.** F91 says an entry asserting *"X already does Y"* needs the line that shows it. **F107 asserted five such things and cited line numbers for none that I had re-read.** The figures were plausible, internally consistent, and three of them were wrong.

### F112's verification: the recap flake is closed, and an eleventh flake surfaced

**Measured after the five guards: 150 full `--bins` runs, debug, zero sightings of `the_recap_holds_only_what_the_foreground_produced_while_attached`** — against **3 failures in 421 unguarded runs** before. Combined with the assessor's 400 guarded runs, the recap flake is closed and **comes off the list**.

**But one run of the 150 failed, on a test nobody had named:** `subagent::tests::spawn_agent_runs_nested_loop_and_returns_only_the_child_answer`. Not previously recorded, passes in isolation, `#[tokio::test]` with **no** guard, and **its body does not reach `set_surface` or the slot** — `subagent.rs`'s only two `set_surface` calls are in a different test. So it is probably **not** this family, and I am not claiming it is.

**My capture was truncated and that is my error, not an agent's.** The loop appended `grep -A2 "^failures:" | head -3`, which caught the test's name and discarded its assertion — the one thing needed to classify it. **F39 exists because a flake without its output is a rumour**, and I wrote a capture that produced exactly that. The loop should have saved the whole failing run to its own file, as every agent-run capture this epic has done.

Recorded as the **eleventh flake, mechanism unknown**, to be captured properly when next seen.

### The corrected flake ledger

Ten became **seven**, in both directions:

- **Removed:** the three `approval_modal_*` entries (**never flakes** — F80 retracted), and the recap test (**fixed and measured**).
- **Added:** `spawn_agent_runs_nested_loop_and_returns_only_the_child_answer` (new), and the latent `the_handler_byte_string_equals_the_restore_helper_output` (**guarded pre-emptively**, 0 sightings in 821 runs, derived by reading).

**The ledger was wrong in both directions at once**, which is the argument for treating it as evidence rather than as a count: it carried three entries that were never flakes **and** missed one that had been failing all along.

## F113 — R6-b's ground, measured before the card is briefed

Read-only groundwork so the search card's brief is not another of the fifteen that were wrong.

**What is ready.** `pin_to(ordinal, ring, rows)` exists and is deliberately **not** prompt-aware, exactly so `/` can call it with a match ordinal — built in R6-c for this purpose, so R6-b adds a *policy*, not an anchor. `CellLine::plain()` exists (`cells.rs:65`). **No search state exists anywhere** in `transcript.rs` or `fullscreen.rs`.

**One authorised test edit will be needed, and it is the same shape as R6-c's.** The footer guard still forbids `["search", "dump", "editor", "scrollback"]`. R6-b removes **`search` only** and requires the footer to advertise its keys; `dump`, `editor`, `scrollback` and the literal `[` **stay forbidden** while R6-d is gated. That guard is what has kept the footer honest through two cards and must not be relaxed wholesale.

**A performance question the card must measure rather than assume.** `plain()` is `sanitize_terminal_text(&self.styled)` — it **allocates a fresh `String` per call**. The ring holds **10 000 lines by default**, so a naive incremental search that re-scans on every keystroke is **10 000 allocations per keypress**, and **F52**'s unbounded pasted cell would be copied on each one. The honest options are to search lazily, to search the styled bytes with the stripper applied only to candidates, or to cache. **Measure the cost before choosing** — and note the project already has a `paste_latency` suite, which is where a latency claim belongs rather than in a unit test.

**A correctness subtlety worth deciding deliberately.** `plain()`'s own comment records that `sanitize_terminal_text` is the crate's one stripper and that **`ascii_structural_markers` is deliberately not applied**. So search over `plain()` searches *sanitised* text, and a user searching for something they can see styled on screen may or may not match it. That is a **user-visible decision**, not an implementation detail: say what is searched, and make the answer the same thing the overlay paints, or say why not.

**And the prompts are searchable now**, which R6-a's version of this card could not have offered — the ring retains the user's lines, so `/` searches both speakers. Worth saying in the docs, because *"search the transcript"* meant half a transcript two cards ago.

### F112's release count, closed

The guarded **release** count F107-a left open is done: **0 failures in 150 full `--bins` release runs**, against an unguarded release rate of roughly **1 in 200**. With the debug figures (0 in 150 of mine, 0 in 400 of the assessor's, against 3 in 421 unguarded), the five guards are confirmed in **both** profiles and the recap flake is closed.

The capture was written properly this time — whole failing run to its own timestamped file — and **nothing was captured, because nothing failed.** Worth stating plainly: the eleventh flake (`spawn_agent_runs_nested_loop_and_returns_only_the_child_answer`) **did not appear in these 150 release runs either**, so its one debug sighting remains its only one, and it is still uncharacterised rather than closed. A single sighting is a rumour until it reproduces; it stays on the ledger as exactly that.

## F114 — README went three cards without the feature I had been documenting everywhere else

The standing requirement warns that README's tables *"drift silently, so re-read the affected row rather than assuming"*. I checked it for the first time since R6-a and found two things:

1. **README did not mention the `Ctrl+O` transcript overlay at all** — not a row, not a clause — across the three commits that built it (`53be36b`, `e6c2255`, `0920e91`). Meanwhile the **older** agent-view overlay *is* documented, on `/jobs view`'s row. So the file documented one overlay and not the newer one, which is the worst shape for a reader: the omission looks like a statement that no such thing exists.
2. **The `cell_ring_lines` row was stale in two ways.** It described the ring as *"scrollback the viewport retains and `fullscreen` paints"* — the overlay reads it too now — and it listed what the ring does **not** hold without mentioning that it **now holds the user's own prompts**, which the prompts card added two commits ago. A config row that enumerates exclusions is exactly where a new inclusion needs adding.

### Why my own sweeps missed it

**F110**'s sweep looks for negative-capability phrasing — *"not wired"*, *"still missing"*. **This was not a false claim; it was an absent one.** Nothing in README said anything wrong about the overlay, so no phrase search could flag it, and the `cell_ring_lines` row's staleness was a sentence that had quietly become **incomplete** rather than untrue.

**So the two sweeps I built cover false statements and dangling references, and neither covers silence.** The standing requirement already names the remedy and I had not been applying it: *re-read the affected row*. The affected rows are findable mechanically — a card that touches a config key or a user-visible key should grep README for that key's name — but what is missing cannot be found by grepping for what is missing.

**Added to the per-card checks:** for every user-visible key, command or config key a card touches, grep README for it **by name** and confirm a row exists and says the current thing. That is a positive check, not a phrase sweep, and it is the only one of the three that would have caught this.

### F114's check, refined on its second use: grep the bare word, not the `/cmd subcmd` literal

Applied the new positive check retroactively over everything this epic shipped — `/detach`, `/jobs view`, `/jobs show`, `/jobs prune`, `/tui`, `/help keys`, `/help tui`, `cell_ring_lines`, `Ctrl+O`. It reported **`/jobs prune` absent**.

**It is not absent, and the check was wrong.** README writes subcommands inside a bracketed enumeration:

```
| `/jobs [list|view|explain ID|show ID|reply ID y|a|n|steer ID TEXT|close-steering ID|stop ID|stop-all|prune ID|prune all]` |
```

So the literal string `/jobs prune` never appears, and a literal match on it is a **false negative**. The bare word `prune` is there, twice in that row.

**Refined:** grep the **bare subcommand word**, then read the row. Had I acted on the check's first answer I would have added a **duplicate row** for a documented command — which is worse than the gap it was hunting, because two rows for one command is drift that a reader cannot resolve.

**Both of the mechanical checks I have built have now produced a false hit on an early run** — F101's reference sweep on a compound row name, and this one on a bracketed enumeration. Neither is a reason to drop them: each has also found a real defect (two dangling references; a wholly undocumented overlay). The rule they share is the one that matters: **the sweep nominates, the human adjudicates.** Every one of these must stay a command whose output is read, never a gate, and never a thing to act on without opening the file.

## F115 — R6-b shipped; and markdown emphasis defeats my own sweep

**R6-b is done.** `/` searches the overlay, `n`/`N` walk the matches. **R6 is now complete except R6-d, which stays gated.**

### The measurement that chose the design, and why it is pinned behaviourally

A full `plain()` pass over a 10 000-line ring: **4.1 ms release / 13.7 ms debug**, and **9.8 ms / 45.4 ms** with one 1 MB pasted cell (**F52**'s input). Scanning `styled()` without the stripper is **195 µs** — so the cost is **essentially all allocation**. A 20-character query typed at speed is ~0.9 s of scanning in debug; three pasted characters alone would blow **N2**'s 100 ms budget.

**So search runs on submit.** `Enter` scans once and stores absolute ordinals; `n`/`N` are `cursor ± 1`, zero allocations. Pinned **behaviourally, not by timing** — a line appended after the submit must not join the walk — so there is no flaky timing test and **nothing was added to `paste_latency`**, because no latency claim is being made: the per-keystroke cost was removed instead.

### My F113 worry was wrong, in the useful direction

`plain()` is **exactly the glyphs of the painted row** — the only thing `sanitize_terminal_text` removes is what the terminal does not display. So searching it matches what the user sees **better** than searching the styled bytes would, and `ascii_structural_markers` staying unapplied is **correct**: it is a `--plain` tier transform, and applying it would make a character the user can see fail to match itself.

**The real divergence is width, not styling.** `fit_row_to_width` cuts at `cols`, so text past the right edge is searched but off-screen. **A match there still counts**, deliberately: width-dependent matches would mean one query finding different things in two window sizes. And an unexpected find — **`push` does not split on newlines**, so a line kitsu writes can hold an embedded `\n` in one cell, where `push_prompt` splits. Both are now in the docs.

### The paste fix, and the first draft that was rejected for the right reason

`b"/find me\nq"` **closed the overlay** and searched nothing. The signal was already on the wire and being discarded: the alternate-screen route writes `ESC[?2004h`, and the loop **swallowed `ESC[200~`/`ESC[201~` whole** in its generic sequence arm. Fixed on the markers, so a newline inside a paste is **content**.

**The rejected first draft matters more than the fix:** *discard the rest of the chunk after any newline* resolves the ambiguity from a **chunk boundary**, which this loop's own header calls wrong (**F65**), and would eat a `q` typed fast enough to share a read with the `Enter`. The test is bidirectional on exactly that — **outside** a paste, `b"/find me\rq"` must still close — and the implementer's mutation D2 proved the two rules are distinguishable.

**Residue, left open deliberately:** a bare LF with **no** markers — an unbracketed paste, or the inline route, which never writes `?2004h`. That is **F72** proper.

### The footer trade, measured

`Ctrl+Home/End` **lost its `oldest/newest` gloss**. With it the row is 91 columns and **at 80 the fit cuts `q/Esc close` clean off the end**; without it the row is 77 and keeps the exit key with three columns spare. Verified both. **Losing the key that leaves the surface is worse than an unglossed key pair.**

It does weaken something I required on T5-S1c-iii — that naming those keys without saying they move the *frame* advertises an ambiguity. Mitigated rather than ignored: inside the overlay the query line supports only character-insert and backspace, so **those keys have no competing caret meaning there**, and `/tui`'s details still carry the full explanation.

### And my own sweep has a third quirk: markdown

**F110**'s sweep greps for `not wired`. The docs now read:

> Dumping the overlay to scrollback and opening it in `$EDITOR` are `**not** wired yet`

**The literal pattern finds zero; a markdown-tolerant `not\*{0,2} wired` finds it.** The emphasis I add for readers defeats the search I rely on — and this sentence is one that *must* be found when R6-d finally ships.

**Fixed in the sweep's pattern.** And this is the third false reading from my two mechanical checks (a compound row name, a bracketed enumeration, now markdown emphasis). The ratio is still worth it — between them they found a wholly undocumented overlay, two dangling references and three stale claims — but it confirms the rule: **these nominate, a human adjudicates, and a clean sweep is not evidence of a clean document.**

## F116 — a comment I shipped one commit ago was backwards twice, and it shrinks F72

Measured while scoping F72 as the next card. The comment, committed in `1f2af45`:

> with no paste markers this loop cannot tell a pasted newline from a pressed `Enter` at all, which is **F72** proper and stays open — **the inline route never writes `?2004h`**

**Both halves of that attribution are wrong.**

1. **The enable sits in `run_transcript_overlay`'s `take_alternate_screen` branch**, which writes `ESC[?1049h ESC[2J ESC[H ESC[?25l ESC[?2004h`. The **inline** call site passes `true` — inline has no alternate screen, so the overlay takes it — and the **fullscreen** call site passes `false`, because its segment already holds the screen. **So the inline route is the only one that writes `?2004h`; the comment names it as the one that does not.**
2. **It does not matter anyway, because a `RawStdinGuard` is held at both call sites.** `engage()` writes `?2004h` and its drop writes `?2004l`, with the stated reason *"so the editor can keep a paste as a single draft"*. There is even a comment at the inline site warning that the overlay **must not engage a second one**. So bracketed paste is on in both routes on any terminal that supports it.

### What this does to F72

**F72's residue on this surface is much narrower than my own comment claimed.** It is not "the inline route has no markers"; it is **"a terminal that *ignores* `ESC[?2004h` sends no markers"** — and then, and only then, the loop cannot distinguish a pasted newline from a pressed `Enter`.

That changes the card. The fix is not "enable bracketed paste on the route that lacks it" (no such route) and not "add burst detection to this loop" as a default — it is a fallback for terminals without bracketed-paste support, which is a **much rarer** case and must be weighed against its cost. **`OverlayDecoder` already implements exactly that fallback** (**F68**: bracketed *plus* unbracketed-burst detection), and it is **deliberately excluded** from `SteerLine::feed`, whose own comment explains why: feeding unbracketed text to the editor *"would submit on that newline, which is F68 again one layer down."*

**So F72 is a smaller and sharper card than its entry suggests**, and the thing to decide is whether terminals without bracketed paste are worth a burst heuristic on surfaces that currently rely on markers. **Q1's terminal matrix is the evidence that question needs, and it still needs a human.**

### The pattern, and it is now the sixth

I wrote a false attribution into a comment **while shipping the feature it described**, and it would have sent the next card at the wrong file — exactly what **F112**'s three errors did. The shape is the same every time: **a plausible mechanism, internally consistent, asserted without re-reading the line.** F91 named the rule (*an entry asserting "X already does Y" needs the line that shows it*), and this is the sixth instance of me breaking it. The comment is corrected **in place**, with the measurement, rather than deleted — **F90**: *"X is missing" and "X is refused" look identical from outside*, and so do *"the route lacks the enable"* and *"the route has it from its caller."*

## F117 — a CLI test is a tool-schema tripwire, and registering `agent_wait` tripped it

**T5-S6 shipped.** The part worth recording is not the tool but what registering it revealed.

`cli::test_context_command_surfaces_window_runtime_evidence` (`tests/cli.rs:16218`) **failed deterministically** the moment the tool was registered — `left: Some(1) right: Some(0)`, i.e. the binary **exited 1**. The implementer confirmed it was theirs by stashing (passes at baseline) and reproduced the stderr by hand:

> `context preflight stopped an oversized provider request: protected prompt needs about 6882 tokens, but the trim target is 6861 tokens (window 8192 …)`

That test drives an **8192-token window with a ~23k-character prompt, and at baseline it clears the trim target by about 90 tokens.** The first `agent_wait` descriptor cost **217** tokens of schema, which put the protected prompt **21 tokens over**.

### The fix was production code, not the test — correctly

The descriptor was trimmed instead: shorter description, **no prose on `until`** (the closed `enum` already says everything), one-line `timeout_secs`. Cost fell to **148** tokens, clearing the target with ~48 to spare. **Every descriptor assertion still holds**, including the one requiring the description to contain *"does not count against"*. No test was touched.

**That is the right instinct and worth naming:** the failing test was not wrong, and the tool's descriptor was genuinely verbose. A card that "fixed" the test's window or prompt would have destroyed a real budget guard to make room for prose a closed enum already conveys.

### But the test is a tripwire, and that needs a card

**Any next tool registered in the non-lean tier will break it again, at any size**, because the margin is ~48 tokens and the test asserts a **startup exit code**. So the failure says `left: Some(1) right: Some(0)` and **nothing about why** — the implementer only found the cause by reproducing the binary's stderr by hand.

**Carded:** either the test should assert the preflight *message* rather than only the exit code, so the next breakage explains itself, or the margin should be made explicit and asserted, so that *"this change costs more schema than the window has"* is the failure a reader sees. The present shape makes a budget regression indistinguishable from any other startup failure.

### H5's real cost is 151 tokens, not 148

Registering the tool also moved the reported **base prompt** from 155 to 158 tokens — the tool's **name reaches the system prompt** as well as the schema list. Expected, but it means the honest figure for the requirement is **148 schema + 3 system prompt = 151**. Recorded because the next person to weigh a tool against a small window will want the whole number, not the schema half.

### Two stale code comments fixed alongside

`subagent/thread.rs:230` claimed *"No agent can enter this phase yet; a later story populates it"* — false since the park path landed. And the authorised test edit replaced a doc comment asserting `blocked` was unreachable, with the assertions left **byte-identical** and the test renamed from `blocked_is_always_zero_until_a_later_story_populates_it` to `blocked_is_zero_for_every_agent_this_test_never_parks`. **Both are F110's family** — a true statement that silently became false when a capability shipped, and neither was found by a sweep: one was found by the test author reading the state machine, the other by needing to use it.

## F118 — my brief would have shipped a fix that printed nothing, and the agent measured instead of trusting me

F117's tripwire is fixed: the four bare `assert_eq!(output.status.code(), Some(0));` sites in `tests/cli.rs` now carry a message naming the binary's own refusal. **Before and after, verbatim:**

```
left: Some(1)
right: Some(0)
```

```
assertion `left == right` failed: kitsu must exit 0; stderr: ; stdout error events:
{"exit_code":1,...,"message":"context preflight stopped an oversized provider request:
protected prompt needs about 7192 tokens, but the trim target is 6861 tokens (window 8192
tokens, effective prompt budget 7680 tokens, trim target = effective minus a 819 tokens
reserve); shorten the latest request or system instructions, disable large tool schemas,
raise the window with --context-window <n>, ...","type":"error"}
  left: Some(1)
 right: Some(0)
```

A reader now gets required versus target, the window, the budget, the reserve and the remedies — **including *"disable large tool schemas"*, which is the actual cause** — without touching the binary.

### The brief's central premise was wrong, and wrong in the way that matters

I wrote: *"the preflight error already states the margin itself, so **surfacing stderr gives the diagnosis for free**."* **It does not.** The target test runs `--jsonl chat --script`, and **in JSONL mode the preflight failure is a JSONL `{"type":"error"}` event on stdout, not stderr.** A stderr-only message renders:

```
assertion `left == right` failed: kitsu must exit 0; stderr: 
```

**Empty.** The agent captured that intermediate state rather than reasoning about it, and widened the message to carry stderr **and** the stdout error events.

**This is the sixteenth wrong brief, and the most consequential kind:** not an error that stops work, but one that would have shipped **a fix that looks like a fix and does nothing**. A card closed on it would have left the next tool's failure exactly as blind, with a message in the assertion making it look handled. **That is F75's shape — *a thing that appears to work is worse than one that plainly does not* — applied to my own tooling rather than to a key.**

**What saved it was the instruction, not the instinct.** I had told the agent: *"if a site's stderr is empty or already consumed, say so instead of writing a message that would print nothing."* It hit exactly that case and did exactly that. **The guard worked because it was written down before the premise was tested** — which is the argument for putting the honest-failure clause in every brief, not just the ones where I suspect trouble.

### The per-site answer, which only measurement could give

Probed by flipping each expected value so the assertion fires on the **success** path:

| site | stderr reachable? |
|---|---|
| `test_context_window_mismatch_warns_on_stderr` (non-JSONL) | **yes**, non-empty |
| `test_jsonl_context_window_mismatch_emits_typed_event` | **no** — the very next line asserts `output.stderr.is_empty()`, **by design** |
| `test_context_command_surfaces_window_runtime_evidence` | **no** — stdout carries it; proven under real failure |
| `test_piped_chat_system_prompt_names_cwd_and_granted_roots` (non-JSONL) | **yes**, non-empty |

**Two of four reach stderr; two are JSONL and reach the diagnosis only via stdout** — and one of those *asserts stderr is empty on the next line*, so a stderr-only message there was never going to work. One combined message covers both shapes, which is why all four got the same one.

### A second tripwire, found by the inflation

Under the inflated descriptor, **`test_no_artifacts_bounds_large_tool_result_and_saves_session_without_sidecar` also failed** — `assertion failed: tool_content.contains("bytes omitted by artifact-free policy")` — **the same blind-failure shape, outside this card's four sites.** Green again after revert. **Carded.** The inflation lever is now a known instrument for finding these: inflate a tool schema and see which tests fail without saying why.

## F119 — the schema-regression exposure is two tests, not a family; and underneath it, a run that drops a tool result and reports success

Swept systematically with the inflation lever rather than waiting for each tripwire to bite. **13 suites, both profiles, two inflation sizes, every run captured to its own file.**

### The ratio, which is the card's deliverable

| inflation | tests failing | blind |
|---|---|---|
| **large** (12 lorem copies, **+331 tokens** over target) | **2** | **1** (50%) |
| **small** (2 copies, **+21 tokens** — the exact margin F117 tripped) | **1** | **0** |
| union, after this card | **2** | **0** |

**The exposure is smaller than feared, and that is a real result.** **No test outside `--test cli` fails under either inflation, in either profile.** The tripwire surface is two tests in one suite, not a family spread across the workspace — so the speculative sweep I was half-expecting to need is not needed.

**F118's fix is working:** the context test, which was the original blind failure, now reports the preflight refusal verbatim at both inflation sizes.

### The two tests fail by different mechanisms at different distances

This is what the **small** inflation bought, and it is why asking for two sizes mattered:

- **At +21 tokens**, the context test trips the **preflight** — an explicit refusal with an exit code.
- **At +331**, the artifact-free test fails because the **history trimmer elides an entire tool message**, and the budget has to be squeezed much further before that happens.

So they are not two instances of one defect. The realistic case — a normal tool, not lorem — hits only the first, which already explains itself.

### The real finding, and it is production-side

The artifact-free failure is the serious one, and the agent measured why a message was needed rather than stderr: that test **asserts `output.stderr.is_empty()`**, consumes stdout into its event vector, and — measured against the real failure — **there is no trim, budget or context event on stdout at all.** Every JSONL event reports `status: "ok"`, `turn_finished` and `final_summary` included.

**So a run can silently drop an entire tool result and report success.** The tool ran, bounded its own output correctly (`artifact_status "suppressed_by_policy"`, `output_bytes 12030`, `output_retained_bytes 7936`), and then history trimming elided the message before the provider saw it — leaving `[older tool output elided]`, 26 bytes, and a run that claims it went fine.

**The test now names it. A reader of the binary's output would see nothing.** That is observability in production code, out of this card's scope by its own constraint, and **carded**: a turn that drops a tool result should say so in its event stream, not only in a test's assertion message.

### Two process notes worth keeping

**The agent narrowed its own message after measuring the render.** Its first draft embedded the whole `tool_finished` event and produced **a 12 KB wall of filler that buried the finding**; it cut back to four policy fields and 300 characters of content *because the render was measured, not assumed*. A diagnostic that is too verbose to read is as useless as one that is empty — and only rendering it tells you which you have written.

**The honest-failure clause earned its place a second time.** Carried from F118 into this brief, it correctly kept stderr **out** of the one message written here. Two cards running, that clause has been the difference between a diagnostic and a decoration.

### F120 refined, after nearly refuting it

I went to card F120 and first tried to disprove it, because its evidence was *"no trim, budget or context event on stdout"* — **an assertion of absence from a narrow search**, which is the error pattern that has bitten this project repeatedly (and my own standing note says never to assert absence from one).

**And the search was indeed too narrow: the vocabulary F120 says is missing already exists.**

- `CompactionOutcome::ToolResultsElided` (`orchestrator.rs:931`), named `"tool_results_elided"` by `compaction_outcome_name` (`main.rs:4982`).
- `compaction_report_payload` (`main.rs:5004`) emits `"outcome"`, `"tokens_before"/"after"`, `"messages_before"/"after"` **and a dedicated `"tool_results_elided"` count.**
- `--offload-cap`'s own help documents the trade-off precisely: lowering it *"makes trimming lossless: an evicted result leaves an `fs_read_file` pointer the model can recall, instead of a dead `[older tool output elided]`."*

**So the system knows about this loss, has a name for it, has a count for it, and has a flag to avoid it.**

### But the absence is real, and I verified it the non-narrow way

Instead of grepping for event names I suspected, I **enumerated every event type** in the agent's captured failing run. The complete set is: `run_started`, `turn_started`, `provider_request`, `provider_attempt`, `tool_started`, `tool_finished`, `assistant`, `assistant_disposition`, `turn_finished`, `final_summary`. **There is no compaction event, and `tool_results_elided` appears zero times.**

**That is the honest shape of F120**, and it is a better finding than the original:

> **The elision happened on a path that produced no compaction report at all.** The reporting vocabulary exists and did not fire — so the card is *"make the existing report fire on this path, or establish why it cannot"*, **not** *"design an event for an unreported loss"*.

`elided_tool_stub` has exactly **one** call site (`orchestrator.rs:3917`), while the outcome is recorded at **3246** and **3336** — so the question is concrete: whether 3917's caller is inside a path that records a `CompactionRecord`, and if not, why.

### The method note, which is the transferable part

**Two absence claims about the same thing were made in two different ways, and only one is trustworthy.** Grepping for *"trim|budget|context"* and concluding "no event" is a guess about naming. **Enumerating every `"type"` in the stream and reading the list is a measurement** — it cannot miss an event whose name I failed to predict. When the claim is *"nothing reports X"*, **enumerate the channel, do not search it.**

## F121 — F120's refined hypothesis was also wrong: the record is created, counted, and then thrown away

I refined F120 once (the vocabulary exists, it just didn't fire) and the test agent **refuted that too, by probe rather than by reading.** It put a *file-backed* probe — deliberately not stderr, which would have tripped the target test's own `assert!(output.stderr.is_empty())` before reaching the real failure — at both outcome sites and inside `record_progress`. The failing run printed:

```
PROBE_WINDOW elided=1
PROBE_RECORD outcome=ToolResultsElided elided=1
```

**So the record IS created.** `trim_tool_results_counted` (the only caller of `elided_tool_stub`) has exactly two non-test callers and **both** set `ToolResultsElided`; `record_progress` is reached, passes its `?`, and pushes a real `CompactionRecord` into the ledger. There is no third unrecorded path.

### The actual defect: there is no transport

Verified independently:

- **`compaction_report_payload` has exactly one call site** — the `/compact` slash command. Nothing else in the binary renders it.
- **`orchestrator.rs:1564` calls `enforce_context_budget_recording(...).await?;` and discards the returned `CompactionReport`** — the call ends unbound, confirmed to the semicolon.
- **`AgentRuntimeEvent` has no compaction variant at all**, enumerated over the whole enum rather than searched: `ProviderRequest`, `StructuredFinalizationDeferred`, `ProviderAttempt`, `AssistantDelta`, `Usage`, `ToolStarted`, `ToolFinished`, `TurnFinished`.
- `final_summary`'s payload carries no compaction or elision field.

**The loss is detected, named, counted, recorded in a ledger — and reaches only two surfaces, both user-initiated after the fact (`/compact`'s result and `/context`'s ledger JSON).** A `--jsonl` run that drops a tool result mid-turn has no surface whatsoever.

**So the card as I briefed it is not executable.** *"Make the existing report fire on this path"* requires a transport that does not exist, and the agent **stopped on exactly the condition I gave it** — add a new event type or a new outcome name and stop — rather than designing one unasked.

### The catch that changes the fix's shape

**The existing count does not distinguish lossy from lossless.** With a low `--offload-cap`, `elided_tool_stub` keeps the offload-pointer line and drops only the preview body — a **recallable** elision, which is the documented *good* case — and `trim_tool_results_counted` **still counts it** and still sets `ToolResultsElided`.

So a naive *"warn when `tool_results_elided > 0`"* fix **warns on the lossless case** and becomes the noise I warned the brief against. The split has to be computed inside `trim_tool_results_counted`, i.e. **a second counter**. That is materially more than "fire the existing report", and it is the honest scope.

### My ruling on the channel, which was mine to give

**N1 is not a blanket ban, and reading it as one was my error.** Quoted: *"Non-TTY, `--plain`/`NO_COLOR`, assistive (`--screen-reader`), and JSONL output MUST stay byte-identical to today."* **An interactive TTY run is not in that list.** So:

- **JSONL: one new additive event `"type"`, emitted only on the lossy path.** A new type leaves every existing event's bytes untouched — strictly less exposure than mutating `turn_finished` or `final_summary`, and far less than touching `status`. Only a consumer asserting *"exactly these types appear"* is affected, and a run that currently lies is not a shape N1 was written to protect.
- **Interactive TTY: a human line is permissible**, because N1 does not cover it. Scoped as a follow-up, not this card.
- **Non-TTY, `--plain`, assistive: nothing new**, to honour N1 exactly.

The conditional-field precedent the agent found (`final_summary` adds `output_last_message_state` only when `Some`) is noted as the fallback if a field is ever preferred to an event.

### And a brief error of my own, the seventeenth

My brief pointed the agent at *"`scratchpad/f119/` … `run.sh` and `inflate.py`"* as prior captures. **Those are in *my* session scratchpad, not the agent's** — each has its own — so the path resolved to nothing. The agent rebuilt the instrument from the brief's prose and reproduced the documented failure exactly, so nothing was lost. **Lesson: never point a brief at a session-local artifact without an absolute path, and prefer describing the instrument to citing a file.** The prose was what actually transferred.

### F120's doc prep: the JSONL event vocabulary is not enumerated anywhere in the docs

Applied **F114**'s positive check ahead of the commit — *for every user-visible thing a card touches, grep the docs for it by name* — to see which list a new `"type"` should join. **There is no list.** Counted per type across `README.md` and `docs/functionality.md`:

| event type | README | functionality.md |
|---|---|---|
| `final_summary` | 7 | 7 |
| `run_started` | 4 | 4 |
| `provider_attempt` | 2 | 1 |
| `assistant_disposition` | 1 | 1 |
| **`tool_started`** | **0** | **0** |
| **`tool_finished`** | **0** | **0** |
| **`turn_finished`** | **0** | **0** |

So the types that appear do so **because some feature's prose happens to name them**, not because the stream is documented. **Three of the ten are absent entirely** — including `tool_finished`, which is the event F120's own test asserts the honesty of.

**Consequences, both worth stating:**

1. **For this card:** a new `"type"` has no enumeration to join, so the honest obligation is a CHANGELOG entry plus a sentence where the JSONL stream is described — not an edit to a list that does not exist. That is a smaller doc change than F114's rule would suggest, and the rule's *answer* here is "there is nowhere to add it", which is itself the finding.
2. **Pre-existing and larger:** **an integrator cannot learn the stream's vocabulary from the documentation.** They must read `AgentRuntimeEvent`. Given **N1** promises JSONL output stays *byte-identical*, that promise is being made about a surface the docs never fully describe — the contract is stricter than the documentation of it. **Noted, not carded in this epic**, since it is squarely outside E11's TUI scope, but it belongs in whatever cycle owns the JSONL contract.

## F123 — my gate list omitted the crate the change lived in, and the implementer added it unasked

**F120 shipped.** The event is `tool_results_dropped { round, dropped }`, emitted at the elision site from `cfg.event_log` and `round` — so `CompactionReport`, `compaction_report_payload` and the discarded call are all untouched, and `crates/kitsu/src/main.rs` has **zero diff**.

### The gate gap, which is mine

**Every brief this epic has carried the same baseline gate list, and `kitsu-core` was never in it.** The F120 change lives in `crates/kitsu-core/src/orchestrator.rs`, and that crate's `orchestrator_tests` module — including `trim_tool_results_preserves_offload_pointer_but_drops_preview`, which is *directly* about the behaviour this card changed — **compiles into `kitsu-core --lib`, not into `kitsu --bins`.**

So the gate list I have been pasting into briefs for a dozen cards **would have let a change to the trimmer ship without running the trimmer's own tests.** The implementer ran it **on its own initiative**, having noticed its change was in a crate the list omitted: **160 passed, 0 failed**, both profiles. I verified it independently.

**New baseline, to carry forward: `kitsu-core` 160 (plus 9, 7, 3, 2, 2, 0 in its other targets).**

**The lesson is not "add a crate to the list".** It is that **the list was assembled from the crates this epic's cards had touched** — a TUI epic, so TUI crates — and then reused as if it were the workspace. A gate list that is not derived from the workspace is a gate list that silently excludes whatever the next card reaches. **The honest form is to derive it: `cargo test --workspace`, or at minimum check that the crate you changed is in the list before trusting it.**

### Two smaller things

**The implementer corrected my arithmetic**: I called the pinned event sequence "13-element"; it is **12**, and the tests were right. Cosmetic, reported rather than silently followed — which is the behaviour I want, since the alternative is an agent quietly "fixing" a test to match a number in my brief.

**The two no-op arms are the right shape and are commented as decisions**, not omissions: *"Matched explicitly rather than swept into a wildcard so adding an activity line here stays a decision, not an accident."* The TTY surface is now **F122**, open — because N1 does not cover interactive TTY runs (clarified in the design doc this commit), so a human line there is permissible but ought to be chosen deliberately.

## F124 — the gate list, derived from the workspace at last. It was missing two whole crates.

**F123 said `kitsu-core` was absent from the baseline gate list I have pasted into a dozen briefs. It is worse than that.** Derived properly from `cargo metadata --no-deps`, then run:

| crate | targets my list ran | targets it did **not** |
|---|---|---|
| `kitsu` | `--bins` + all 5 integration targets | — |
| **`kitsu-core`** | **none** | **lib (160) + `run_contract` 9, `session_contract` 7, `session_k04` 3, `session_k07` 2, `session_k08` 2** |
| `kitsu-editor` | lib + `public_api` (as `-p kitsu-editor`) | — |
| **`kitsu-providers`** | **none** | **lib (120) + 7 route/wire targets: 2, 6, 4, 12, 4, 4, 31** |
| `kitsu-render` | lib | — |
| `kitsu-tools` | lib (413) | **`semantic_evidence` 15, `structural_context` 5** |
| `kitsu-tui` | lib | — |

**Two entire crates and four further targets — 1 140 tests across 21 targets in the non-`kitsu` half alone, of which my list reached 745.** All green; `cargo test --workspace --exclude kitsu` exits 0.

### The canonical gate, to use in every brief from here

```
cargo test --workspace --exclude kitsu          # 21 targets, 1140 tests
cargo test -p kitsu --bins                      # 975
cargo test -p kitsu --test cli                  # 248
cargo test -p kitsu --test tui -- --test-threads=1   # 75
cargo test -p kitsu --test paste_latency        # 4
cargo test -p kitsu --test carrier_matrix       # 7
cargo test -p kitsu --test build_provenance     # 2
cargo clippy --workspace --all-targets -- -D warnings
cargo clippy --workspace -- -D warnings
```

`kitsu` stays enumerated because **`--test tui` needs `--test-threads=1`**, which is why the workspace form cannot simply replace everything — and that constraint is almost certainly how the list came to be hand-written in the first place.

### Why this happened, which is the transferable part

**The list was assembled from the crates this epic's cards happened to touch** — a TUI epic, so `kitsu`, `kitsu-tui`, `kitsu-render`, `kitsu-editor`, `kitsu-tools` — **and then reused as if it were the workspace.** Nothing ever compared it against the member list. It was accurate for its first card and silently wrong for every card that reached further.

**A gate list that is not derived is a gate list that excludes whatever you did not think of.** And the failure is invisible in exactly the wrong way: every brief reported *"all green, both profiles"*, truthfully, about a subset I had chosen without knowing I had chosen it.

**It nearly cost something real.** F120 changed the history trimmer in `kitsu-core`, whose own test — `trim_tool_results_preserves_offload_pointer_but_drops_preview` — is *directly* about that behaviour and lives in a target my list never ran. The implementer ran it on initiative. **Had it not, the card would have shipped "fully verified" with the relevant test unexecuted.**

**The same shape as F114**, one layer up: there I could not grep for a README row that was missing; here I could not notice a crate I had never listed. **Absence is the thing checklists cannot see, and the remedy is the same both times — derive the list from the system, do not curate it from memory.**

## F125 — N1 never said which file descriptor, and three inline composites answered differently

Flagged by F122's test author and verified: **N1 says "Non-TTY" without naming a descriptor**, and the system does not treat them as one thing.

- `kitsu_render::is_tty()` is `io::stdout().is_terminal()` — **stdout**.
- `stderr_is_tty()` is a **separate** function — **stderr**.
- N1's own parenthetical says *"stdout is a TTY"*.
- **The three inline composites in the binary all key on `stdin_is_tty()`** — verified, three occurrences, none of them stdout and none carrying a `jsonl` term.

So the requirement, its own example, and the code each pick a different descriptor. **They differ for a real invocation**: `kitsu chat <prompt> | tee` has a terminal stdin and stderr but a piped stdout.

### The resolution, now in the design doc

**N1 protects consumers, and a consumer can redirect either descriptor independently.** So *"non-TTY"* means **per-descriptor**: when a given descriptor is not a terminal, *that* descriptor's bytes stay identical. It is not one global condition, which is why no single predicate was ever going to express it.

F122's human line is therefore gated on **`stderr_styled() && is_tty()`** — both terminals — which is **stricter than the minimum** (stderr alone would satisfy the paragraph). Deliberate: a run with stdout redirected is being consumed by something, and a mid-turn notice is noise to it. **The stricter gate cannot break a consumer that exists today; the looser one might, and can be adopted later if a case appears.**

### Why this is worth a finding rather than a quiet edit

**I cited those three composites to the agent as *"possibly N1's predicate, measure it"*** — and the useful part is that the hedge was right for a reason I had not guessed. I expected "one predicate spelled three ways, so extract it". The answer was **four distinct conditions for four distinct jobs**, which is why the agent's recommendation was *do not extract* and why extraction would not have been behaviour-preserving.

**An invariant that names a property without naming the thing it holds of is not yet an invariant.** N1 had been cited in a dozen cards and amended twice before anyone needed to know which descriptor it meant — and the moment a gate depended on it, three different answers were already in the tree.

### Also now biting: F121's documentation gap

`test_n1_jsonl_gains_nothing_from_the_human_report_of_a_dropped_result` is the **second** test hard-coding the full ordered JSONL `"type"` sequence, and there is still **no documented list** for either to reference. Two tests now encode a contract the documentation does not state — so a change to the stream breaks them with no doc to consult. Recorded under F121; still outside this epic's scope, but the cost is now concrete rather than hypothetical.

## F126 — F121 was wrong, and two commits ago I shipped a contract gap because of it

**RETRACTION.** F121 recorded that *"the JSONL event vocabulary is enumerated nowhere in the docs"* and that *"an integrator cannot learn the stream's vocabulary from the documentation."* **Both are false.**

**There is a published JSON Schema** — `docs/schemas/kitsu-chat-v1.schema.json`, 42 `$defs`, with an event-shaped definition for **every one** of the eleven types: `run_started`, `turn_started`, `turn_finished`, `final_summary`, `tool_started`, `tool_finished`, `assistant`, `assistant_disposition`, `provider_request`, `provider_attempt`, `error`. **And the code already pins against it**, via `include_str!("../../../docs/schemas/kitsu-chat-v1.schema.json")` in `kitsu-render/src/jsonl.rs`.

**How I got it wrong: I searched `README.md` and `docs/functionality.md` for type names, found three absent, and concluded the vocabulary was undocumented.** The documentation exists in a different *form* — a formal schema, not prose. **That is the third time this session I have asserted absence from a search whose shape could not have found the thing** (F116's backwards paste attribution, F120's two wrong hypotheses, now this), and it is the error my own standing note warns about.

### The consequence, which is a real defect in a commit I accepted

**`tool_results_dropped` is NOT in the schema.** Commit `2d1785c` added a new JSONL event type and never updated the published contract, so **the schema no longer describes the stream it documents.** I accepted that card — twice over, since F122 extended the same feature and I checked the docs again without finding this.

### Why the existing guard did not catch it, which is the fix worth building

`published_schema_keeps_command_payload_and_event_definitions_distinct` asserts that **specific** `$defs` exist and that **specific** `$ref`s point where expected. **It never requires that every `AgentRuntimeEvent` variant has a definition.** The schema is pinned for **structure**, not for **coverage** — so a new event type slips through in silence, which is exactly what happened.

**The card is therefore two things**, and the second matters more than the first:

1. Add `tool_results_dropped` to the schema — the gap I shipped.
2. **Add a coverage test: every `AgentRuntimeEvent` variant must have a schema definition.** That converts *"remember to update the schema"* into an enforced invariant, which is the only form that has held up in this session.

**Where it must live, measured:** `kitsu-render` has **no `kitsu-*` dependency** — load-bearing, it is what makes "classic pays nothing" a compile-time property — so the existing schema test **cannot see the enum**. The coverage test belongs in **`kitsu-core`**, where `AgentRuntimeEvent` is defined, and can reach the schema at `../../docs/schemas/kitsu-chat-v1.schema.json`. Verified both.

### The pattern, stated once more because it keeps recurring

**F124** found a gate list curated from memory rather than derived from the workspace. **F114** found a README row that was missing, which no grep could reveal. **This** is the same shape a third time: a contract pinned by a test that checks *the parts someone thought of* rather than *the whole set*. **Every one was invisible precisely because the check was authored from the same mental model as the thing it checked.** The remedy is identical each time — enumerate the system, do not curate a list from what you remember of it.

### F126's exposure, bounded before briefing the fix

Applied this card's own lesson one level up: **are there other published contracts pinned for structure but not coverage?** Measured — `docs/schemas/` contains **exactly one** file, `kitsu-chat-v1.schema.json`, referenced from **one** source file.

**So the exposure is one schema, not a family.** Same shape of result as F119's sweep: a measured *"smaller than feared"*, which is worth having explicitly rather than leaving as an open worry. The coverage test this card adds therefore closes the whole class, not a sample of it.

### F126 corrected — the finding about curated lists contained a curated list, and it was wrong

**F126, and the brief I wrote from it, both listed eleven event types as having schema definitions:** `run_started`, `turn_started`, `turn_finished`, `final_summary`, `tool_started`, `tool_finished`, `assistant`, `assistant_disposition`, `provider_request`, `provider_attempt`, `error`. **Measured by the test author and verified by me, that list is wrong in both directions.**

**`AgentRuntimeEvent` has nine variants:** `ProviderRequest`, `StructuredFinalizationDeferred`, `ProviderAttempt`, `AssistantDelta`, `Usage`, `ToolStarted`, `ToolFinished`, `TurnFinished`, `ToolResultsDropped`.

- **Six of my eleven are not variants at all** — `run_started`, `turn_started`, `final_summary`, `assistant`, `assistant_disposition` and `error` are **frontend-level** events emitted elsewhere, which is why the schema has defs for them.
- **Three real variants were missing from my list**: `structured_finalization_deferred`, `assistant_delta`, `usage`.

**I had assembled it from the event types I had seen in a captured stream**, not from the enum — the identical method whose failure F126 exists to record. **The finding about curating lists was itself curated.** The card's conclusion survives untouched: the coverage hole is real and is exactly `tool_results_dropped`, at **8 of 9**.

### And a second half of the same gap, which nobody had reported

`$defs/forward_compatible_event` carries a **hand-maintained `type.not.enum` of 26 names**, and **`tool_results_dropped` is absent from it too** — verified. So the published contract today says a `tool_results_dropped` line is an *unknown forward-compatible event*, and **a schema-validating consumer accepts it as something it need not understand.** Commit `2d1785c` missed **both** places, and the fix is two schema edits, not one.

**That is the sharper version of the defect.** The missing definition means the contract is silent about the event; the missing exclusion means the contract **actively mis-describes** it. A consumer doing the right thing — validating against the published schema — is told to ignore a line reporting data loss.

### The enumeration the author built, which is the part to keep

Rejected: a literal name array (the failure this card exists to fix), `strum::EnumIter` (no non-unit variants, and a production derive), `mem::Discriminant` (not enumerable), and parsing the enum out of source (*"a reimplemented Rust parser is a worse oracle than the compiler"*).

Chosen: **one declarative macro whose single list expands into both a sample-value function and an exhaustive `match`** — so an arm and its sample come from the same entry and cannot drift. A plain samples-list-plus-match can be satisfied by adding an arm and forgetting the sample; this cannot.

**And the tripwire was verified, not asserted:** transiently adding a tenth variant produced `error[E0004]: non-exhaustive patterns`, whose only repair is a new macro entry, which supplies a sample, which the coverage test walks, which then demands the `$defs` entry. **No wire name is typed anywhere in the file** — every one is read back out of `serde_json::to_value(&sample)["event"]`.

### F126 closed — and the closing line is the agent's, because it is better than mine

**Three dimensions, and two were found after my brief.** A contract entry is not one place: `tool_results_dropped` needed a **`$defs` definition** pinning its `type` (my brief), **absence from `forward_compatible_event`'s exclusion list** (found by the test author), and **a branch of the closed root `oneOf`** (found by the implementer). Verified: the root's keys are exactly `$schema, $id, title, description, oneOf, $defs` — **nothing else**, so it is genuinely closed, and **the two edits I prescribed would have made the schema reject a line the tool emits.** Strictly worse than the defect.

**The third edit was not test-driven and the implementer said so** rather than presenting three edits as if the tests demanded all three. It was then **made** test-driven: the branch was dropped from a scratch copy (28 → 27, anchored), `include_str!` repointed, and the assertion failed naming the variant — a structural mutation standing in for validation the host cannot run.

**Folded into one test, for a reason worth keeping.** The three are **one question** — *does the contract accept this variant's line?* — and the folded message says **`defined and pinned 9/9`** *and then* names the branch, so a reader is told the definition is right and the root is the problem. Three separate tests would have reported the branch gap with no way to see that the other two were fine.

**One invariant added that I would not have thought of:** exactly one root branch pins no `type` constant — `not_event_shaped == ["forward_compatible_event"]`. **A second catch-all branch would silently reopen the closed root and void the whole dimension.** Asserted from the measurement, not assumed.

**And one surface deliberately left unpinned, with its reason:** every event def having a branch. *"The schema author's structural choice, not an invariant the enum can speak to"* — and its home is `kitsu-render`'s structural test, not a coverage test driven by the Rust enum. Declining to assert something the oracle cannot speak to is the same discipline as declining to write the `--max-rounds` assertion two cards ago.

### The closing line, verbatim, because it generalises past this schema

> *A contract has as many coverage dimensions as it has places a name must appear, and enumerating those is the same problem one level up.*

That is the honest end of the thread that runs **F114 → F124 → F126**: a README row that no grep could reveal, a gate list curated from the crates one epic touched, and a contract check that pinned the parts someone thought of. Each was invisible because the check shared a mental model with the thing checked. **This card fixed the third and named the fourth** — the enumeration of the dimensions themselves — rather than pretending the regress had bottomed out.

## F127 — T5-S7 is blocked on an external spec, and saying so is the deliverable

Took T5-S7 (the herdr integration manifest) as the next numbered story and measured it before briefing. **It cannot be built verifiably here.**

**H4 reads:** *"Kitsu SHOULD ship a herdr integration manifest (`herdr-plugin.toml` or screen manifest) under `integrations/herdr/` so kitsu is detected natively."* It names **two** alternatives and specifies **neither**.

**And that sentence is the only description of the format in the entire repository.** Every other `herdr` occurrence — the design doc, the plan, nine cycle records — discusses the integration *conceptually*. There is no schema, no example, and no herdr installation to test detection against.

**So a manifest written now would be guesswork asserting a third-party schema nobody in this repo can check** — an artifact that *looks* like an integration and may not be one. That is the shape this session has refused repeatedly: the unverifiable claim that reads as settled. And **H4 is a SHOULD**, so declining to ship it violates no requirement.

**Recorded with what it actually needs** — herdr's plugin/manifest schema, from a human or from herdr's own documentation — after which it is a small card. That is the third item now explicitly blocked on external input, alongside **Q1**'s live terminal matrix and **R6-d**'s dump-exposure decision.

**The general point, since this is the fourth time this epic:** *"this card needs information that does not exist in the repository"* is a finding, not a failure to deliver. Writing the artifact anyway would convert a known unknown into an unknown unknown — a file future readers would reasonably trust.

## F128 — F107-b done: only two of four tests leaked, and both leaked two slots

**The brief said four tests leak `FOREGROUND_RECAP`. Two do.** Captured, not reasoned:

```
BEFORE:  the_jobs_view_command_…              recap_is_some=false
         the_status_row_and_the_prompt_label_… recap_is_some=true
         the_detach_command_…                 recap_is_some=false
         the_views_attach_…                   recap_is_some=true
```

The two clean ones end with `apply_detach_command`, which calls `replay_foreground_recap` — **they already detach through production, which is exactly the shape the card asked for.** The agent left them alone rather than "fixing" them to make the count four. **Twentieth wrong brief.**

### A second leak the same two carried, which the brief did not mention

Probing `Arc::ptr_eq` on `active_surface()` at entry versus exit showed both leaking tests also left **the surface slot itself** holding the `BufferedSurface` that `begin_foreground_buffering` installed. **Strictly worse than the recap slot alone**, and the same cleanup closes it.

And `the_status_row…` needed **two** cleanup calls, not one: it attaches, clears the binding cell directly (*which production never does* — the real routes go through `replay_foreground_recap`), then **attaches again**. Two buffers, one overwrite, and the first becomes unreachable through the global — so the cleanup had to balance each attachment where it ends, not all at the end.

### The hypothetical is constructible, and it reproduced the assessor's sentence verbatim

A throwaway detach-first test, run against the leak:

```
assertion `left == right` failed: the detach-first test's own probe must still be installed
  left: []
 right: ["after the detach-first"]
```

`left: []` because the detach-first call took **the other test's leaked buffer** and `set_surface`'d its inner surface over the live probe — the line went to real stderr instead. With the cleanup: passes. **Then deleted** — evidence, not a deliverable.

**Production paths only**, chosen per test with a measured reason: `apply_inline_detach` (the composer loop's own arm, where `←` and `Ctrl+X d` land) for one; `replay_foreground_recap` for the other, because its binding is already `None` and the keystroke routes would short-circuit on `take()` and replay nothing. **No test-only teardown, no `#[cfg(test)]` reset.**

**Verification proportionate to the subject:** the card is about cross-test contamination, so a single green run proves little. **61 debug + 61 release `--bins` runs, zero failures.**

## F129 — my own committed code was fmt-dirty for two cards, and my gate said clean

`cargo fmt -p kitsu -- --check` reported **six deviations in `crates/kitsu/tests/cli.rs`** — a file **unmodified in the working tree**, so the dirt was **committed**, by me, in the two `tool_results_dropped` cards. Flagged by the F107-b agent as *"not mine, but the next card to run fmt will sweep it into its diff."*

**Fixed in this commit**, and verified semantically inert before accepting: the token streams are **identical modulo trailing commas** (which rustfmt drops when joining a multi-line call onto one line, and which are meaningless in Rust), the suite is green at 256, and `--check` is now clean. A naive string-literal comparison initially flagged a difference; it was a regex artifact — a quote inside a doc comment ran the match on into code — and both files have identical literal counts.

**Why the gate missed it:** my gate list carries `cargo fmt -p kitsu -- --check`, agents reported it clean, and it was not. **The most likely explanation is that it was run before the final test edits**, which is the one ordering a per-card checklist cannot express. Noted rather than solved: **a gate that runs before the last edit is not a gate**, and the fix is to run fmt last, after the suites, not alongside them.

## F130 — a twelfth flake, captured whole this time: ETXTBSY staging an executable

During my post-fmt re-run, `kitsu-tools --lib` failed **412 passed / 1 failed** on `external::backend::tests::dropping_live_supervisor_transfers_and_cleans_all_owned_workers`. **In a crate my change does not touch.** Three re-runs: 413, 413, 413. In isolation: passes.

**Captured whole to its own file** (1 142 lines) — and the assertion is the diagnosis:

```
cannot stage immutable executable: Device or resource busy (os error 16)
```

**ETXTBSY**, not disk space (`/tmp` at 20%). A test tried to write an executable **another process has open for execution** — parallel tests contending for one staging path.

**So it is the same shape as F107, in a different medium:** shared mutable state across parallel tests, where the shared thing is a **filesystem path** rather than a process-global. Not fixed here — different crate, and `kitsu-tools` holds the maintainer's uncommitted work — but **carded with its mechanism named**, which is more than the eleventh flake has after six sightings' worth of runs.

**And this is why the capture rule exists.** My own truncated capture two cards ago left the eleventh flake unidentifiable to this day. This one took one `grep -o` to diagnose because the whole run was kept.

### F130 corrected — I named its mechanism from the errno, and the implementation refutes it

I wrote that F130 was *"parallel tests contending for one staging path … a filesystem path rather than a process-global."* **That is wrong, and reading the implementation refutes it.**

`stage_executable_image` (`kitsu-tools/src/external/backend.rs:1555`) stages through **`memfd_create`** — an **anonymous in-memory file with no filesystem path at all.** There is no path to contend for. I inferred "path contention" from `ETXTBSY` + the words *"stage immutable executable"*, which is **the same error I have now made four times this session: a mechanism asserted from a plausible reading instead of from the code.**

### What is actually established, and what is not

**Established:** the error is formatted in **production** code — `executable_sha256` wraps `stage_executable_image` — so this is not a test-only path; the errno is `16`; and it is not disk space (`/tmp` at 20%).

**Not established:** *which operation failed.* The function has **six** fallible steps — `source.metadata()`, `source.seek()`, the `memfd_create` syscall, the header `read`, the `write_all` into the memfd, and the streaming `read` loop — and they all propagate through one `?` into one message:

```rust
format!("cannot stage immutable executable: {error}")
```

**The message names the activity and loses the operation.** `memfd_create` does not document `EBUSY`; a `read` returning it would be unusual. So I cannot say what happened, and the capture cannot tell me — there was no backtrace, and the wrapper discards the context.

### Which makes the card a better one than "fix the flake"

**This is F117's shape in production:** a failure that reports *that* something went wrong and not *what*. There, a bare exit-code assertion cost a card's worth of debugging until the message was made to name the refusal. Here, an `io::Error` from one of six syscalls is flattened into one string, so the **next** sighting will be exactly as undiagnosable as this one.

**So the next card is diagnosis before repair: make the staging error name the operation that failed.** That is a small production change, it is testable (each failure mode should name its own step), and it converts a flake nobody can explain into one whose next occurrence explains itself. **Fixing the contention I guessed at would have been fixing something I had not shown to exist.**

**And the ledger stays honest:** F130 remains **open with its mechanism unknown**, not "diagnosed". The eleventh flake is unidentifiable because I truncated its capture; the twelfth is unidentifiable because the *product* truncates its own error. Those are different failures and only the second is fixable in code.

## F132 — the derived gate list needed `--no-fail-fast`, and its absence had already misled me once

**F124** replaced a curated gate list with one derived from `cargo metadata`. **It was still wrong in a way that only shows up when something fails.**

`cargo test --workspace --exclude kitsu` **halts at the first failing target** and never runs the rest. So a run with one failure reports a **partial** count, and the partial looks like a result.

**It misled me the same day.** Verifying a formatting fix, I read *"18 ok-lines, failures: 2"* where the baseline is 28 targets, and my first thought was that my own change had broken ten targets in a crate it does not touch. It had not: one `kitsu-tools` test failed (**F130**), the run stopped there, and the remaining ten targets were never executed. **The number was not a smaller result; it was a truncated run** — and I nearly diagnosed a phantom regression from it.

**Corrected in the gate list:** `cargo test --no-fail-fast --workspace --exclude kitsu`, so a failure costs one target's green and not the visibility of every target after it.

### Why this belongs with F114, F124 and F126 rather than being a flag tip

Those three were about **absence**: a README row that was missing, crates never listed, schema entries nothing required. **This is about a count that is present and means something other than what it appears to.** A gate that reports `18` when the baseline is `28` is not silent — it is *wrong in a way that invites a wrong diagnosis*, which is worse than silence, because silence prompts a question and a plausible number does not.

**The generalisation for any gate I write from here:** a gate must report either the whole truth or an obvious failure, never a plausible fraction. `--no-fail-fast` buys that for cargo; the equivalent question is worth asking of every command on the list.

### F101's sweep, this card's run: one real dangling reference, two false positives, both adjudicated

Ran both doc sweeps as the discipline requires. **The reference sweep nominated three; exactly one was real.**

- **`R6-d` — REAL.** The requirement map still cites it and **the row was gone.** I added it when splitting R6 four ways, then lost it in a later restructure of the same table. **Restored, with its gate and the reason intact.** Third time a correction of mine has created a dangling reference, and the third time this sweep has caught one.
- **`F102` — false positive, and mine.** I widened the regex's prefix class to include `F` this run, so it now treats a **finding citation** (*"R6-c (blocked, F102)"*) as a missing story row. Findings are not plan rows. **The widening was a mistake**: it buys nothing and manufactures noise.
- **`T5-S2` — the documented false positive**, from the compound row name `T5-S1b + T5-S2`. Unchanged, still known, still not worth a cleverer matcher.

**The phrase sweep's five hits were all legitimate** — `@file` path completion and draft syntax highlighting really are unshipped, and the markdown-tolerant `not wired` pattern now returns **zero**, which is the first clean run since F110 made it tolerant.

**So: 1 defect, 2 false positives, 5 correct negatives.** That ratio is the argument for keeping these as commands a person reads rather than gates — **and the argument against my own widening.** A sweep that nominates finding-numbers as missing rows trains its reader to skim, which is how the real `R6-d` hit would eventually be missed.

## F133 — a thirteenth flake, and it is the mechanism I wrongly assigned to F130

**F131 shipped** (below). During my acceptance run a **different** `kitsu-tools` test failed:

```
---- builtins::command::tests::replacing_an_approved_executable_forces_fresh_approval ----
called `Result::unwrap()` on an `Err` value: ExecError("command failed: Text file busy (os error 26)")
```

**Captured whole to its own file** (1 374 lines) before anything else. Three re-runs: 416, 416, 416. The workspace re-ran at **1146 passed, 0 failed**.

### The two flakes are different, and the errno is what separates them

| | F130 | F133 |
|---|---|---|
| errno | **16** — `EBUSY`, *"Device or resource busy"* | **26** — `ETXTBSY`, *"Text file busy"* |
| site | `stage_executable_image`, via `executable_sha256` | `replacing_an_approved_executable_forces_fresh_approval` |
| mechanism | unknown; the **only** exit documenting `EBUSY` is `F_ADD_SEALS` | **`ETXTBSY` is definitionally "wrote a file being executed"** |

**`ETXTBSY` is exactly the mechanism I guessed for F130 and was wrong about** — writing an executable that something currently has open for execution. The guess was not nonsense; it was **attached to the wrong flake.** F130 stages through `memfd_create`, which has no path to contend for; F133's test **replaces an executable by name**, which is precisely what `ETXTBSY` reports.

**So the right conclusion is one I could not have reached by reasoning harder about F130:** there are two parallelism-sensitive executable flakes in this crate, one of which really is path contention. Distinguishing them took **reading the errno**, which is why F131's whole premise — *make the failure say which operation* — is worth the eighteen edits.

**F133 is carded, not fixed.** Its mechanism is named, its capture is complete, and it is one re-run from being characterised — unlike the **eleventh** flake, which remains unidentifiable because I truncated its capture.

## F131 — the staging failure names its step; 16 steps wired, 2 provable

My brief's step count was right this time (18 exits / 16 steps) because the test author had **enumerated it from the function** after my first attempt listed eight and omitted the entire sealing tail.

**The restructuring chosen, with its reason:** per-site `map_err` into one formatting helper, rather than an inner function returning `Result<_, (Step, io::Error)>`. The latter *"would have required wrapping the whole body in a nested function and threading the `PendingExecutableImageV1` construction through it — a real control-flow edit on a function that owns raw descriptors (`File::from_raw_fd` on two fds)."* The per-site form is pure expression substitution: **no new scope, no change to where the owned `File`s drop**, and the step is named at the exit that produced it rather than inferred by a caller. It keeps `error.kind()`, so the OS error text survives verbatim after the label — which is the entire point.

**All 16 wired, including the 14 nothing can test** — notably `AddSeals`, the `EBUSY` exit that is F130's most plausible source. **A fix labelling only the two provable steps would have left F130 exactly as undiagnosable as before**, which is why "wire the untestable ones" was the brief's load-bearing instruction.

**The mutation was sharper than I asked for:** remapping exit #2 to a neighbouring step made the *right* test fail *naming the wrong label*, while its sibling stayed green — so the two tests discriminate **per-exit**, not merely per-step.

**And two distinctions the author drew that I had not:** the function's two `.expect(..)` sites are **panics, not error exits**, so they are neither counted nor wired; and the non-x86_64 stub returns a fixed `Unsupported` error and is outside the step vocabulary. Both correct, both unprompted.

## F134 — F133's mechanism was wrong too, and I made the same mistake twice in two cards

**RETRACTION.** F133 records that *"F133's test replaces an executable by name, which is precisely what `ETXTBSY` reports."* **False.** Verified against the capture:

- The panic is at **`command.rs:3524:10`** — the `.unwrap()` on the **first** `execute_command` (the call spans 3515–3524).
- `std::fs::rename` is at **3526**, two lines later.

**At the moment of the ETXTBSY the executable had not been replaced at all.** I read the test's *name* — `replacing_an_approved_executable_forces_fresh_approval` — added the errno, and wrote down a mechanism. **That is the identical error I had corrected one card earlier on F130**, where I called a `memfd` path "path contention". Two flakes, two mechanism guesses, both wrong, both from the same habit: **the errno narrows, the test name suggests, and neither names.**

### The real mechanism, which no amount of staring at the test would have produced

1. `write_test_executable` opens the executable `O_WRONLY|O_CREAT|O_CLOEXEC`, writes, **closes**. Window: microseconds.
2. Concurrently, one of the other 415 tests in the same binary **forks**. The child's fd table is a **copy of the parent's — including that write fd.**
3. **`O_CLOEXEC` does not save you:** the kernel closes CLOEXEC descriptors *inside* `execve`, **after** `deny_write_access()` has already failed.
4. Linux's `deny_write_access()` checks **`inode->i_writecount`, which is process-independent.** So between a sibling's `fork` and its child's `execve`, this inode has a writer — someone else's.
5. The test's own `execve` → **ETXTBSY**, wrapped by `command_failed` into `ExecError("command failed: …")`.

**So my ruled-out cause #3 was half right in the worst way:** the *fact* survives — `std::fs::write` does close the fd — but **the inference does not.** Closing it *in this process* is not enough. **The writer that defeats you is your own descriptor, surviving in somebody else's child.**

**And cause #1 survives for a reason better than mine:** unique `tempfile::tempdir()` paths do not help because **the kernel check is on the inode, not the path.**

### Evidence, labelled honestly

**Reproduced outside the suite, not inside it.** A C harness with unique paths, `O_CLOEXEC`, the fd closed and `chmod`'d before exec, and sibling threads forking: **4/500** at zero linger, **84/500** with a 5 ms linger. A Rust harness in exactly the test's shape (`std::fs::write` + `set_permissions` + `Command::new`): **13/400**. **In-suite: 0 reproductions in 35 runs**, including 15 at `--test-threads=32` under eight CPU burners.

So: **mechanism derived, independently reproduced in a harness, not reproduced in `kitsu-tools`.** One sighting in ~60 runs plus zero in 35 is consistent with the rate and is **not** evidence for the mechanism — the harness is.

### A fix refuted by measurement before anyone proposed it

`run_supervised_command` registers `pre_exec` **unconditionally**, even when `inherited_paths` is empty, which forces std off `posix_spawn` for every spawn. *"Make `pre_exec` conditional so std can use `posix_spawn`"* reads as a free win. **The harness says it would make this worse:** `posix_spawn` mode reproduced **13/400**, fork-plus-`pre_exec` **0/800**, at only 1.4× the spawn rate — so not a throughput artefact. **Do not take that fix.** And the agent declined to assert *why* `posix_spawn`'s window is wider, having not measured it — which is the right place to stop.

### The finding that outranks the flake

**This is a production defect, not a test defect.** A user replacing a tool between invocations hits exactly this, and so does a user whose editor or build is mid-write on it. Kitsu reports a flat `command failed: Text file busy (os error 26)` with **no indication that the condition is transient** — and `ETXTBSY` at `execve` is *definitionally* transient.

**Carded, not fixed:** a bounded retry around the spawn in `run_supervised_command`. It is safe with respect to the security story — `cwd` revalidation, root revalidation and `observe_executable` all run before the spawn, so a retry around the spawn alone reopens no check-to-exec window that is not already documented. But it needs a bound, a backoff, a decision on re-running revalidation per attempt, and a test — **so it is a test-first card, and a defensible alternative is to keep failing and say the condition is transient.**

**Test-side mitigation does not exist**, and that is worth recording: the F133 test's own hygiene is already correct, the writer comes from siblings it cannot control, no copy-or-rename trick changes the inode's writecount, and `#[serial]` would not help because the `rg`/`sh` spawners are not serial.

## F136 — F135 shipped as the message, not the retry; and a lint exception I refused

**Design (B) chosen over a bounded retry, on measurement.** The test author read what each pre-spawn check pins, and **the answer decided the card**:

- **`observe_executable` pins a path and a stat snapshot** — `canonical_path, length, readonly, mtime, dev, ino, mode, uid, gid, ctime` — **no descriptor, no content hash**, with `identity.pinned` hard-coded `false`. **Verified by me.**
- `AuthorizedCwd::revalidate` and `BoundCommandRoot::revalidate` **are** descriptor-anchored: a held directory `File` compared against the path's `dev`/`ino`.

**So F134's safety claim did not survive.** It held that a retry *"reopens no check-to-exec window that is not already documented"* — true in **kind**, false in **extent**. Without per-attempt revalidation, the exec's distance from its last validation becomes **the sum of all prior attempts plus the backoff**, strictly wider than the single documented stat-to-exec gap — and **externally inducible**, since ETXTBSY can be provoked by holding a write descriptor, making kitsu loop, and swapping the file. With per-attempt revalidation the window stays today's size, but the semantics invert: **the editor-mid-write case is exactly where the fingerprint differs**, so a later attempt turns a transient `ExecError` into a permanent `PolicyDenied` and a re-prompt.

**And the measurement that settled it:** the real ETXTBSY windows are **microseconds to milliseconds**, while the user-facing cases the card names are **human-scale**. No defensible bound rescues them. **A retry would have spent a new security decision to fix the flake-shaped case and still printed a message for the user-shaped case.**

### The MSRV blocker, and why I refused the obvious fix

`std::io::ErrorKind::ExecutableFileBusy` is the natural discriminator and is **stable only since 1.83**; this workspace declares **`rust-version = "1.80"`**. It compiles on the local 1.93 toolchain, so **every test passed** — and `clippy::incompatible_msrv`, implied by `-D warnings`, rejected it. **My brief named a discriminator that could not satisfy my own gate.**

Three resolutions were offered and **I rejected two**:

- **Raising the workspace MSRV** is a promise to every consumer and a maintainer's policy decision. Taking it to improve one error string's discriminator is the tail wagging the dog.
- **`#[allow(clippy::incompatible_msrv)]`** I rejected on substance, not taste: **the lint is correct** — that code genuinely would not compile on 1.80 — so silencing it would make the declared `rust-version` **false**. The repo would claim support it does not have, and this would have been the **first such allow in the tree**, establishing the precedent on cosmetic grounds. **A lint exception that makes a stated guarantee untrue costs more than a nicer type name is worth.**
- **`libc::ETXTBSY`** costs nothing: `libc` is already a `kitsu-tools` dependency, used **190 times** in a sibling module of the very file being edited, and `ETXTBSY` is a **named POSIX constant**, not a magic `26`.

**With a required comment naming the MSRV reason and the migration trigger**, because without it a future reader sees `libc` where `std` would do, "fixes" it, and walks straight back into the lint.

### Three things the agent did that I am recording as right

**It distinguished "unavailable on the toolchain" from "unavailable under MSRV".** My brief said to stop if the kind was unavailable on its toolchain — it **was** available, on 1.93. A letter-of-the-law reading would have shipped it green locally and broken the clippy gate for everyone.

**It re-ran the mutation after switching discriminators**, *"since the guard is no longer the one I first tested."* Exactly right: the earlier mutation proved the **old** guard, and swapping the condition invalidates the proof. Nobody asked for that.

**It declined to run the release profile** while the discriminator was still in question, rather than burning two minutes of pty suites on code that might change — and said so, which beats both quietly doing it and quietly skipping it.

## F137 — T4-S6-ordinal shipped, and F106's defect recurred exactly

**`Ctrl+X 1..9` attaches by displayed position, never by id.** The last fully-buildable plan row; everything else remaining is gated on a human decision, an external spec, or the live terminal matrix.

### Rejected once, for the same reason as `Ctrl+O`

The implementation was complete and green — and the chord was **advertised nowhere a user could see.** All three occurrences in `main.rs` were a code comment, a function doc comment, and a test's doc comment. **`/tui`'s `details` string had zero mentions**, confirmed by the new test's first failure echoing the entire pre-change string.

**That is F106 verbatim**, and the second time this epic a working chord reached acceptance unannounced. **F82**'s rule — *a rule applied per-change is not a rule enforced* — was itself applied per-change and not enforced.

**And the error was partly mine:** I told the agent I would write the advertisement. Wrong. **The string is production code, and when `Ctrl+O` shipped it came with a test**; an unguarded string drifts, which is the whole reason that test exists. Corrected by handing it back for test-then-string.

**The new test asserts the negative too**, over `["wrap", "ctrl+x 10", "two digits", "modulo"]` — because *"a user who believed in either would address the wrong agent in silence."* Multi-digit and wrapping are both decided against, so the advertisement must not imply them.

### The design decisions worth keeping

**`nth_displayed(rows, ordinal)` rather than a public `display_order() -> Vec`.** Handing out the vector hands out **a second indexing convention**: every call site would decide for itself whether the user's `1` means `[0]`, and whether out-of-range clamps, wraps or does nothing. One function owns the 1-based convention and the bounds — `checked_sub` answers `0` *before* the subtraction, and `get` rather than an index makes past-the-end `None` instead of a clamp or a panic.

**It is built over the existing private `display_order`, the same one `sync`/`move_by` walk** (4 call sites, verified). That is load-bearing: **the ordinal can never disagree with the row the selection mark sits on**, and a test drives `AgentSelection` itself to prove it rather than comparing against a copy of the order.

**My brief said no display-order accessor existed. One did** — twenty-seventh wrong brief, and the correction improved the design.

### The reasons behind the inert cases, which outlive the card

- **`Ctrl+X 0` is inert and the digit stays undrafted.** Not an alias for the first row (a second spelling of one action wastes a key) and **not a prefix of `10`** — multi-digit entry would make `Ctrl+X 1`'s meaning **wait for the next keystroke**, which is **F75**'s invisible-at-the-keypress shape, the defect that got a card's answer overruled earlier this epic.
- **Past the end is `None` and must not clamp:** *"acting on an agent the user did not count off the screen is worse than doing nothing."*
- **Twelve agents get nine digits, with no wrap and no modulo**, both of which attach to the wrong agent in silence. **The cap lives in the key decoder, not the accessor** — my ruling: nine-wide is a property of *the chord*, there being nine digits, not of display order, and a future caller wanting position twelve should not work around a keyboard limit encoded in a data accessor.

### Two things the author did unasked

**It refined its own mutation claim downward.** A *partial* id-first-then-position mutation fails only **three** of the four tests, because the twelve-agent fixture uses ids `100..` so no id can equal an ordinal and it falls through to correct behaviour. **That exposes a real gap in that test's discriminating power**, and reporting it stops a future reader over-trusting the suite.

**It made re-attaching idempotent**, by routing the switch through the existing `apply_agent_view_exit` seam, which acts only on a **change** of binding — so the chord typed on the already-attached agent is a no-op rather than a duplicate milestone. I asked only whether switching worked.

**And the fixture is the anti-vacuity shape I asked for, exceeded:** ids `7, 3, 11, 2, 5, 9`, a filter leaving four, display order `#3, #7, #11, #5` — so **ordinal 1 selects id 3**, **id 2 exists but is filtered out**, and the test asserts *"no ordinal equals the id it selects"* **first**, with `assert_ne!`, before any behaviour.

## F138 — writing the decision records found a decision that had been made and never closed

**T5-S8 shipped**: three ADRs in `decisions.md`, and a **derived** requirement-traceability table appended to the design. **No test phase — the deliverable is prose, and saying so beats inventing an assertion.**

### The catch: open question 7 was decided and still labelled open

The design **stated the decision in one place** — *"(`background_blocked`), never as a new `status` value"* — and **still called it "(open question 7)" in another.** Two sentences disagreeing about whether a decision existed.

**And the decision was right for a reason the epic later re-derived independently.** ADR-2's argument is that a new `status` value mutates a shape every consumer already parses, while a new event type leaves existing events byte-identical — **which is exactly the reasoning F120 applied to `tool_results_dropped` months of cards later**, arrived at from scratch. The decision was sound and its rationale was *in the file*; what was missing was the one word that said it was settled.

**The lesson generalises past this epic:** a decision recorded in prose beside the thing it governs, but never **marked closed**, is **indistinguishable from an open one** to every later reader. Closing it is not bookkeeping — it is the difference between a reader applying the decision and re-litigating it. Now closed, pointing at ADR-2.

### The traceability table, and the column I refused to add

Derived by matching each requirement id the design **defines** (35) against every plan row that **cites** it. **All 35 are cited** — a clean negative, and the kind of result worth having explicitly.

**I tried to add a "shipped" column and it was a fabrication.** A regex over the plan's prose reported **17 requirements unshipped**, including `B1`–`B5`, `H1`–`H3`, `R4` and `R8`. False, for two compounding reasons found by checking one case:

1. **The plan has two tables.** Statuses live in a summary row (`T2-S1 … | done | c4bd57b`); requirement citations live in a detail row hundreds of lines away. Matching within a row can never see both.
2. **The status vocabulary collides with the domain vocabulary.** `T2-S1`'s detail row contains the word **"Blocked"** — as a *request state*, `Registry{agent_id, sender}`'s sibling — and my exclusion treated it as a status marker.

**So requirement→row is derivable; requirement→status is not.** I shipped the table without the column and said why, in the table. **This is the fourth time this session a derived artefact beat a curated one and the first time a derivation had to be abandoned** — which is the honest other half of F124's lesson: *derive rather than curate, and when the derivation cannot be trusted, publish the part that can and name the part that cannot.*

### The ADRs record two things that are not "we decided X"

**ADR-1** records a decision that is **accepted and unexecuted**: R1's renderer flip was never a question of *whether*, only *when*, and the answer is a release boundary. What blocks it is **Q1's live terminal matrix**, which no agent can produce. Worth stating plainly: **the flip is not blocked on engineering.**

**ADR-3** records a **deferral with its reopening condition** — no socket API, because the state file and `agent_wait` already serve the integration, and **H4 is itself blocked on herdr's manifest spec**, so designing a socket against an unknown consumer is how a second control surface outlives its reason. *"Until one exists, the state file is the contract."*

### F62's doc sweep: ten sites, not three — and two of them disagree about what the flag does

The test author named three documentation sites for `--no-alt-screen` / `alternate_screen`. **Sweeping every live document found ten**, which is the F110 discipline paying off again: *fix the siblings, not the sites you were handed.*

**User-facing:** `README.md:663` (the flag row), `README.md:1173` (the key row), `docs/functionality.md:573-574` (the prose), `CHANGELOG.md:380` (a key list).
**Design:** `:132`, `:173`, `:480-481`, `:1408`, `:1561`, `:1896`, `:2041`.

### Two of them describe the flag differently

- **`README.md:663`** — *"Force `[tui] alternate_screen` to **`never`**"*
- **`docs/tui-and-agent-view-design.md:1408`** — *"`--no-alt-screen` forces **`inline`**"*

**One says the flag sets a screen value; the other says it sets a renderer mode.** Those are different claims about the same switch, and they have coexisted long enough that the flag's own doc comment (*"a one-way force: it beats any `[tui] alternate_screen` value"*) matches only the first. **Neither is currently true**, since the flag is honoured by nothing — but when the in-viewport path exists, somebody will have to pick, and the two sentences will not pick the same thing.

**Worth noting which is more defensible:** `inline` is a **renderer**, and forcing it would also disable fullscreen, which is a much larger claim than *"do not take the alternate screen"*. The screen-value reading is the narrower and more likely intent. **Recorded, not resolved** — T5-S4 owns the fallback and should settle it.

### And the design already recorded the gap honestly

`:480-481` has said all along: *"`[tui] alternate_screen = never` does not yet get the in-viewport overlay this design describes elsewhere. That path is unexercised and `alternate_screen` remains a key without a consumer."*

**So the design was honest and the README was not.** The gap was documented where implementers read and promised where users read — which is the worse direction, and exactly the asymmetry **F114** found with the undocumented overlay, inverted: there the capability existed and the README omitted it; here it does not exist and the README promises it.

**The use case makes it sharper.** README:663 names the scenario: *"for multiplexers where the alternate screen misbehaves."* So the user most misled is the one the row was written for — in tmux, watching the alternate screen misbehave, passing the flag, and getting nothing. **A promise is most costly to the reader who needs it most.**

### F62 closed by refusal — and the implementer kept the variant for F90's reason

Shipped as ruled: both doors refused, `auto`/`always` untouched. Two details worth keeping.

**It avoided a substring to keep an existing test honest.** The argv message deliberately does **not** contain `invalid value`, because `test_no_alt_screen_is_a_bare_switch_that_composes_with_tui` asserts stderr carries neither `unexpected argument` nor `invalid value` for that flag. So **the refusal fires and that test still passes untouched** — the flag still *parses*, it is then refused, which is exactly the distinction the older test was written to protect.

**And it hit the consequence I had only recorded, then reasoned past it correctly.** Once nothing constructs `AlternateScreen::Never`, the compiler warns and `-D warnings` fails. The obvious fix is to delete the variant. **It kept the variant with a dead-code allowance instead**, on the grounds that deleting it would turn *"recognised, not yet available"* into *"not a value"* — **F90's inversion, applied unprompted to a type rather than to prose.** That is the difference between a user being told to wait and a user being told they imagined the feature.

**The practice that made this card cheap** was the test author proving mutation A — *the intended refusal makes all five file-side tests green* — **before** hand-off. Eleven unsatisfiable assertions this epic were found in review or by an implementer hitting a wall; this is the first card where reachability was demonstrated in advance. **Worth making standard: a test author who has built the fix once knows the tests are satisfiable, and nobody else can know it as cheaply.**
