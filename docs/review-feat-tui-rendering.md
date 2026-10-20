# Code review: `feat/tui-rendering` vs `main`

Verification of 10 unverified reviewer findings. Each was re-located in the
current tree, read in context, and checked against code, existing tests, and
(for the Seatbelt items) a live `sandbox-exec` probe. No source was modified.

Pre-existing macOS test noise treated as expected and unrelated: 4
`kitsu-tools` `builtins::command` failures and ~193 `kitsu` bin env failures.

## Summary

| # | Title | Verdict | Severity | Introduced in | Fix direction |
|---|-------|---------|----------|---------------|---------------|
| 1 | Seatbelt `file-read*` / `mach-lookup` lets a contained command read host secrets & reach launchd | CONFIRMED (reads) / PLAUSIBLE (spawn escape) | HIGH | `dfcdc0b` | Narrow `mach-lookup` to break the launchd/LaunchServices spawn path; document the open-read ceiling as the accepted boundary |
| 2 | `(allow process*)` + argv0-only visibility gate; `sh -c tool` passes macOS, fails Linux | CONFIRMED | MEDIUM | `dfcdc0b` | Document the exec model (write-confinement only, not exec-gating); if exec-gating is wanted, it cannot be done via argv0 alone |
| 3 | Shift+Left in fullscreen ends the session and loses the draft | CONFIRMED | HIGH | `5035851` (chord) + `a36ceab` (fullscreen loop) | Add a `CycleAgent` arm to the fullscreen loop, or drop the chord when the surface can't act on it |
| 4 | Model-popup Tab & Ctrl-C arms are dead (decoder never emits 0x09/0x03) | CONFIRMED | MEDIUM | `6cd05a0` (arms) + `ebc7b39`/`175148d` (decoder) | Map `\t`→`Char('\t')` and `\x03`→`Char('\u{3}')` in `decode_overlay_keys::plain` |
| 5 | Caret parked inside the pinned box breaks `InlineSurface::paint`'s invariant; transcript/notifications land in the box | CONFIRMED | HIGH | `804aa47` | After placing the caret, re-park it on the scroll region's last row, or let `InlineSurface::paint` own the park |
| 6 | `cwd_grant` interactive write grant: stale doc comment / internal comment (behavior intentional) | CONFIRMED (docs) / "CLI help" claim REJECTED | LOW | behavior `61a1dff`; stale text `4a3c9f5`/`175148d` | Update the `cwd_grant` doc comment (main.rs 19443-19445) and the inline comment (19491-19493); CLI help is already correct |
| 7 | Box-default-on: idle status/meter never shown; 3-row empty box pinned during the turn | CONFIRMED | MEDIUM | `4794746` (default-on) + `55765bd`/`4a3c9f5` (reset) | In the box branch, show the status row for an empty draft; collapse the box after a read if a 1-row idle state is wanted |
| 8 | Caret row from fresh `terminal_rows()` + unclamped `boxed.rows.len()`, not the latched/clamped height; top-dropped rows misplace it | CONFIRMED (code) | MEDIUM | `804aa47` | Compute the caret from the clamped painted height (`granted_rows`/`paint_rows` result), accounting for `fit_viewport_rows` dropping from the top |
| 9 | Single-`*` italic has no word-boundary check; glob patterns get mangled | CONFIRMED | LOW | `cba64db` | Add a `*`-side boundary check in `emphasis_can_open`/`find_emphasis_close` (e.g. require non-alnum around `*`, or skip inside code-ish spans) |
| 10 | Heading wraps inner spans in outer truecolor+bold, but inner spans emit `\x1b[0m`; text after an inline span loses the heading style | CONFIRMED | LOW | `8dd9902` | Re-assert the heading SGR after each inner span, or style per-segment instead of wrapping the whole line |

Counts — Verdict: 9 CONFIRMED (one with a plausible sub-claim), plus 1
sub-claim REJECTED (finding 6 "CLI help"). Severity: 2 HIGH, 1 HIGH/plausible,
4 MEDIUM, 3 LOW.

---

## Finding 1 — Seatbelt reads host secrets; mach-lookup reaches launchd

**Verdict: CONFIRMED (information disclosure) / PLAUSIBLE (spawn escape). Severity: HIGH.**

`build_seatbelt_profile` (crates/kitsu-tools/src/builtins/command.rs:1414-1438)
emits `(deny default)` then `(deny network*)`, `(allow process*)`,
`(allow sysctl-read)`, `(allow mach-lookup)`, `(allow file-read*)`, and a
tightly scoped `(allow file-write* …)` covering only the writable roots, cwd,
`/private/tmp` and the four device nodes.

Live probe (a profile byte-identical to the one built, run via
`/usr/bin/sandbox-exec -f`): a write to `$HOME` was **denied**
("Operation not permitted"), confirming write confinement holds; but
`head -c 40 ~/.zshrc` printed `READ_HOME_OK` and `ls ~/.ssh` printed
`SSH_DIR_LISTABLE`. So a contained command **can** read host secrets
(`~/.ssh`, `~/.aws`, kitsu config/credentials) — this is real and matches the
finding. It is also *deliberate*: the `ponytail:` comment at 1400-1402 and the
in-body comment at 1416-1421 state the enforced perimeter is write-confinement
+ no-network, and reads are left open because dyld's read set is SIP-hidden and
a read allow-list fails closed (SIGABRT) in the wrong direction.

The second half — using launchd/LaunchServices to spawn an *unsandboxed*
process and thereby defeat `(deny network*)` + write-confinement — is
**plausible, not proven**. `(allow mach-lookup)` is unrestricted, and under the
profile `launchctl list` succeeded (rc=0), i.e. the contained process can talk
to launchd. `launchctl submit … -- touch $HOME/...` failed (rc=1) on this macOS
(modern per-user-session bootstrap), and I did not weaponize an `open`/
`osascript`/LaunchServices spawn. The spawn-to-unsandboxed vector is the
load-bearing claim for "defeats network/write confinement" and remains
unverified; the info-disclosure on its own is the confirmed, HIGH concern
(secrets are readable and could be staged into the writable cwd for later
pickup). Note no network means direct exfil from inside the sandbox is blocked.

Fix direction: narrow `mach-lookup` to the handful of services a dynamically
linked process needs (closing the launchd/LaunchServices bootstrap path) and
explicitly document open-reads as the accepted boundary. Tightening reads is
expressly warned against by the existing comment.

## Finding 2 — `process*` + argv0-only exec gate; macOS/Linux asymmetry

**Verdict: CONFIRMED. Severity: MEDIUM.**

`authorized_launch_inputs`
(crates/kitsu-tools/src/builtins/command.rs:870-899) runs the executable-
visibility gate against **only** `executable.canonical_path` (argv[0]) — the
approved program. The Seatbelt profile then carries `(allow process*)`
(line 1423), so once `/bin/sh` is the approved program it may `exec` any
readable binary; the gate never sees the inner target. On Linux, the bwrap
backend (1113-1180) binds only the runtime roots (`--ro-bind-fd`), the bound
command roots (`--bind-fd`) and the cwd — nothing else is mounted — so
`/bin/sh -c "/opt/homebrew/bin/tool"` finds no `tool` unless it lives under a
mounted root. The asymmetry the finding describes is real: macOS runs arbitrary
readable child binaries, Linux does not. This sits within the stated
write-confinement model (child execs don't by themselves escape write/network
limits), hence MEDIUM rather than HIGH.

Fix direction: this is a model-clarity issue — document that containment gates
*writes and network*, not which binaries run, and that argv0 vetting is not an
exec allow-list. True exec-gating is not achievable via the argv0 check alone.

## Finding 3 — Shift+Left in fullscreen ends the session, loses the draft

**Verdict: CONFIRMED. Severity: HIGH.**

`KeyFilter::feed` emits `KeyItem::CycleAgent(-1)` whenever the byte stream
starts with `SHIFT_LEFT` (crates/kitsu/src/tui_bridge.rs:2689-2696),
unconditionally — it is not guarded by `InlineKeys` (the `keys` field only
gates the chord `prefix` at 2652 and the `←` route). The reader turns it into
`InlineRead::CycleAgent(delta, draft)` at 3165-3166 regardless of
`InlineKeys::disabled()`. The fullscreen read loop
(crates/kitsu/src/main.rs:7332-7438) has arms for Suspended, OpenTranscript,
OpenAgentView/Detached, and everything else falls to `other => break other`
(7437). `read_chat_line_inline_outcome` maps
`InlineRead::CycleAgent(_, _) => ChatReadline::Eof`
(crates/kitsu/src/main.rs:6615). `ChatReadline::Eof` ends the session, and the
carried `draft` string in the `CycleAgent` payload is discarded (never replayed
like the OpenAgentView/OpenTranscript drafts are). So Shift+Left (or
Shift+Right) in the fullscreen view ends the session and loses the draft.

Fix direction: add a `CycleAgent` arm to the fullscreen loop (cycle, or no-op
and replay the draft via `carried`), or have `KeyFilter` not emit `CycleAgent`
when the caller can't act on it.

## Finding 4 — Model-popup Tab and Ctrl-C arms are unreachable

**Verdict: CONFIRMED. Severity: MEDIUM.**

`model_popup_step` has arms `OverlayKey::Char('\t')`
(crates/kitsu/src/main.rs:6806) and `OverlayKey::Char('\u{3}')` (6821). The live
driver feeds keys from `decode_overlay_keys` (6973). Its `plain` helper maps
only `\r`/`\n`→Enter, `0x7f`/`0x08`→Backspace, and `0x20..=0x7e`→`Char`
(crates/kitsu/src/main.rs:8222-8229); `0x09` (Tab) and `0x03` (Ctrl-C) fall
through to `None` and in `OverlayPending::None` produce no key (8264-8267). So
the decoder can never emit `Char('\t')` or `Char('\u{3}')`, and both arms are
dead on the real path. Additionally `RawStdinGuard` clears `ISIG`
(documented at crates/kitsu/src/main.rs:6338-6345), so Ctrl-C arrives as byte
`0x03` rather than a signal — and that byte is dropped. The unit test
`model_popup_step_filters_moves_completes_picks_and_cancels` (30521-30559)
constructs `OverlayKey::Char('\t')` / `Char('\u{3}')` by hand (30551, 30556),
so it passes while exercising code the decoder never reaches. Esc (StepBack)
still cancels and Enter picks, so the popup remains usable — hence MEDIUM.

Fix direction: in `decode_overlay_keys::plain`, map `0x09`→`Char('\t')` and
`0x03`→`Char('\u{3}')` (or add a Tab/Ctrl-C `OverlayKey` variant).

## Finding 5 — Caret parked inside the pinned box breaks the paint invariant

**Verdict: CONFIRMED. Severity: HIGH.**

`InlineSurface` pins its rows *outside* the DECSTBM scroll region and relies on
the cursor being parked **inside** the region: `paint` ends with
`write!(inner.err, "\x1b[{bottom};1H")` (crates/kitsu-tui/src/viewport.rs:372),
and the module docs (viewport.rs:6-18) and `transcript_line`/`write_block`
(381-435) write transcript text at the current cursor so new lines scroll the
transcript. The box branch of `PinnedPane::paint_composer` (tui_bridge.rs:1728-
1766) paints via `paint_rows` (which re-parks correctly) and then **overrides**
the cursor with `\x1b[{caret_row};{caret_col}H` where `caret_row` is inside the
box, i.e. inside the pinned region (1755-1764). After that, any text written
through the surface while the reader blocks — async agent notifications,
`ExternalPrinter` lines, streamed turn output — lands at that caret, inside the
box, instead of scrolling the transcript. This compounds with finding 7 (the
box is pinned during the turn). The non-box `paint_draft` path explicitly notes
it cannot place a caret for exactly this reason (tui_bridge.rs:1670-1673).

Fix direction: after computing the visual caret, re-park the hardware cursor on
the scroll region's last row (as `InlineSurface::paint` does) and let the
terminal's own cursor-show handle the draft caret, or move the caret placement
into `InlineSurface` so one owner maintains the invariant.

## Finding 6 — `cwd_grant` write grant: stale docs (behavior intentional)

**Verdict: CONFIRMED for stale docs; "CLI help" sub-claim REJECTED. Severity: LOW.**

The behavior change is intentional (user-requested): `cwd_grant` grants
`read: true, write: true` for `yes || interactive`
(crates/kitsu/src/main.rs:19447-19454, body introduced by `61a1dff`). The
finding's real content is stale docs, and that is confirmed: the `cwd_grant`
doc comment still says "Interactive-without-`--yes` keeps the historical
read-only cwd grant" (19443-19444, written by `4a3c9f5` before the interactive
change and never updated), directly contradicting the body. The inline comment
in `run_chat` also says interactive gets "read access to the launch directory
by default" (19491-19493, from `175148d`) — stale. However the **CLI help is
already correct**: the `--yes` flag doc (cli.rs:250-257) and the `after_help`
FILESYSTEM CAPABILITIES block (cli.rs:546) both describe read+write. So the
finding's "CLI help still describes read-only" is wrong; only the two in-source
comments are stale.

Fix direction: update the `cwd_grant` doc comment and the `run_chat` inline
comment to say interactive sessions get cwd read+write (prompting only for
writes outside the cwd). Doc-only.

## Finding 7 — Box default-on: idle status/meter lost; box pinned during turn

**Verdict: CONFIRMED. Severity: MEDIUM.**

Part 1: the non-box branch of `paint_composer` paints `self.status_row()` for an
empty/hintless draft (crates/kitsu/src/tui_bridge.rs:1786-1792), matching
`paint_draft` (1660-1661). The box branch (1728-1766) has no such case — it
always calls `live::compose_input_box` with the draft as-is, and
`compose_input_box` always returns top border + ≥1 content row + bottom border
(crates/kitsu-render/src/live.rs:353-361), so an empty draft renders a 3-row
empty box and the status/meter row is never shown at idle with the box on
(the default since `4794746`).

Part 2: the post-read reset calls
`bridge.paint_composer(prompt, "", 0, "", false, rich)`
(crates/kitsu/src/main.rs:6484), whose own comment says this is "the call that
must take the viewport back down to one row". With the box default-on it
instead paints the 3-row empty box and leaves it pinned through the turn — and,
via finding 5, with the caret inside it.

Fix direction: in the box branch, render the status row for an empty draft
(mirroring 1786-1792), or collapse to one row after a read if a 1-row idle
state is intended.

## Finding 8 — Caret math uses unlatched height; wrong on short terminals

**Verdict: CONFIRMED (from code). Severity: MEDIUM.**

In the box branch, `paint_rows` latches a height of
`clamp_viewport_rows(self.rows(), content.len())` and paints
`fit_viewport_rows(content, self.rows())`, which **drops rows from the top**
(crates/kitsu/src/tui_bridge.rs:1933-1951, and the top-drop is documented at
1897-1900 and 1916-1917). But the caret is computed from a fresh
`live::terminal_rows()` and the **unclamped** `boxed.rows.len()`:
`top = term_rows - total + 1`, `caret_row = top + boxed.caret.0 - 1`
(1757-1760). On a terminal shorter than the box, the painted height is clamped
and the top rows (including the box's top border) are dropped, while the caret
calc still uses the full box height and full caret offset — so the two diverge
and the caret is misplaced. Provable from the arithmetic; the visible artifact
requires a live terminal shorter than the box, hence MEDIUM.

Fix direction: derive the caret from the clamped/painted height (the value
`granted_rows`/`paint_rows` latched) and account for the top-drop offset so the
caret tracks the rows actually on screen.

## Finding 9 — Single-`*` italic lacks a word boundary; globs get mangled

**Verdict: CONFIRMED. Severity: LOW.**

`emphasis_can_open` (crates/kitsu-render/src/lib.rs:575-592) only requires the
next char to be non-whitespace and not a doubled marker; the preceding-char
word-boundary guard (585-589) applies **only to `_`**, not `*`.
`find_emphasis_close` (598-615) similarly only word-boundaries the `_` close
(604). So in `edit src/*.rs and tests/*.rs`, the first `*` (followed by `.`)
opens, and the next `*` (preceded by `/`, not whitespace, not doubled) closes —
italicizing `.rs and tests/` and consuming both asterisks, exactly as the
finding states. The existing test `italic_renders_but_spares_identifiers`
(2979-3002) covers `_` identifiers and `2 * 3` (spaced) but not the glob case.
Cosmetic, LOW.

Fix direction: apply a boundary check to the `*` open/close too (e.g. require a
non-alphanumeric neighbour, or treat `*` adjacent to `/`/`.` as literal), or
decline emphasis inside path-like runs.

## Finding 10 — Heading inner spans reset the heading SGR

**Verdict: CONFIRMED. Severity: LOW.**

Headings render as `inner.truecolor(r, g, b).bold()` where
`inner = style_inline_markdown(heading, …)`
(crates/kitsu-render/src/lib.rs:724-734). The inner spans
(`style_inline_markdown`, 472-561) are each produced via owo-colors
`.bold()`/`.italic()`/`.truecolor()` (525, 539, 548), and owo-colors terminates
every styled value with a **full** `\x1b[0m` reset (confirmed in
owo-colors-4.4.0 `src/combo.rs:31,46` and its doctest fixtures,
`"…\x1b[0m"`). The outer `.truecolor().bold()` emits one combined prefix at the
start and one `\x1b[0m` at the very end — it does not re-assert after the inner
reset. So in a heading like `## See \`main\` for details`, the backtick span's
`\x1b[0m` clears the heading's truecolor+bold, and "for details" renders
unstyled. Cosmetic, LOW.

Fix direction: re-emit the heading's SGR prefix after each inner span, or build
the heading by styling each segment (border text and inline spans) explicitly
rather than wrapping an already-reset string.

---

## Recommended fix order

1. **Finding 3** (HIGH, cheap) — Shift+Left ends the fullscreen session and
   loses the draft. Add a `CycleAgent` arm (or drop the chord where it can't
   act). Biggest user-facing break, smallest diff.
2. **Finding 5** (HIGH) — re-park the cursor inside the scroll region after the
   box paint so transcript/notifications stop landing in the box. Fix with 7.
3. **Finding 7** (MEDIUM, pairs with 5) — restore the idle status row in the box
   branch and stop pinning the empty box through the turn.
4. **Findings 1 & 2** (security) — narrow `mach-lookup` to break the launchd/
   LaunchServices spawn vector; document the open-read and exec-model
   boundaries. Design decision — confirm the threat model with the owner first.
5. **Finding 4** (MEDIUM) — map `0x09`/`0x03` in `decode_overlay_keys` so the
   popup's Tab and Ctrl-C actually work.
6. **Finding 8** (MEDIUM) — compute the box caret from the clamped painted
   height so short terminals place it correctly.
7. **Findings 9 & 10** (LOW) — render cosmetics: `*` word boundary for globs;
   re-assert heading SGR after inner spans.
8. **Finding 6** (LOW, doc-only) — fix the two stale comments; CLI help already
   correct.
