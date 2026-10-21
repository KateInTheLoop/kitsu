# kitsu — next development plan

Updated 2026-10-20 following the plan analysis. Implementation baseline:
`55892a477ee24cc4cacf14ae19c790ee05b0a486` on `feat/tui-rendering`, 33 commits
ahead of recorded `main` at `c035bebd3b747341667e4ad8791e9bcd21ffac11`.
The worktree was clean at that baseline. Documentation edits after it do not
change the implementation checkpoint.

**Scope: local development only. Do not push to upstream, publish a PR, merge
to upstream, change authentication, or delete/reorganize branches as part of
this plan.** Roadmap implementation has started with Stage 0 validation and
formatting; Stage 1B behavior changes still wait for Stage 0 acceptance.

This is the current execution plan. It supersedes the remaining-work lists in
[tui-interactive-roadmap.md](tui-interactive-roadmap.md) and the earlier phasing
in [tui-input-box-requirements.md](tui-input-box-requirements.md). The latter's
recorded product decisions still apply. The
[branch review](review-feat-tui-rendering.md) remains historical evidence about
the code before its follow-up fixes, not a list of current defects.

## Current checkpoint

Implemented on the local branch: `--effort` presets, interactive/`--yes` cwd
read+write, macOS Seatbelt command containment, model popup picker,
Shift+Left/Right agent cycling, resume id on exit, markdown rendering, and the
framed composer. The frame is the default **within inline mode**;
`KITSU_INPUT_BOX=0` retains the older composer. Inline mode itself remains
opt-in. Keep the fallback through this plan's editor and copy-box milestones.

All ten review findings have implementation fixes. Use the review's original
IDs below; the previous plan numbered some of them differently.

| Review ID | Implemented change | Fix commit |
|---|---|---|
| F1 | Seatbelt excludes known secret paths and narrows Mach-service lookup | `c909b97` |
| F2 | Seatbelt restricts executable paths to system/toolchain and granted roots | `c909b97` |
| F3 | Disabled agent keys no longer emit cycling; fullscreen preserves the draft | `632741c` |
| F4 | Model-popup Tab and Ctrl-C reach their handlers through the byte decoder | `11f2a31` |
| F5 | Surface owns box caret placement and re-parks before transcript writes | `37439cb` |
| F6 | `cwd_grant` comments describe interactive read+write correctly | `2e708ff` |
| F7 | Status returns above the box; post-read reset collapses the composer | `e6923ac` |
| F8 | Box fitting preserves borders/caret and uses latched viewport geometry | `37439cb` |
| F9 | Markdown emphasis preserves glob paths | `af6a42a` |
| F10 | Heading styling survives inline-span resets | `af6a42a` |

`55892a4` also isolates the effort-preset test from an exported `KITSU_EFFORT`.
The review rejected F6's stale-CLI-help subclaim; that is not pending work.
The original launchd escape subclaim was plausible, not a demonstrated escape.

The checkpoint inspection observed 148 render tests and 158 TUI tests passing,
plus focused popup-decoder and agent-cycle tests. The Seatbelt test command
completed successfully, but its live tests can return early when containment
or host fixtures are unavailable. That result alone does not certify that
every runtime assertion executed. `cargo fmt --all -- --check` failed across
seven source files. Full-suite, Linux-runtime, and real-terminal acceptance
remain open; none is inferred from the targeted results.

**Seatbelt boundary:** file reads remain broadly allowed, with exclusions for
known secret locations. This is not general filesystem read isolation or
proof that every possible credential location is protected. Writes, executable
paths, and Mach services have their own restrictions. Acceptance records must
describe the tested boundaries without claiming full Linux/macOS equivalence.

## Delivery rules and milestone order

- Build pure state/layout behavior first, then wire it to terminal input and
  painting. Reuse existing editor, pane, surface, and restore mechanisms.
- Bracketed-paste payload is content. A new key must stay inert inside a paste,
  including when delimiters, escape sequences, or UTF-8 cross read boundaries.
- No mouse capture; native selection and copy-paste must keep working.
- Enable new behavior explicitly per consumer. `TypeaheadEditor` also serves
  during-turn input and overlay lines; changing it globally can change classic,
  fullscreen, and approval/filter behavior unintentionally.
- Provider tests use local fakes/wiremock, without credentials or live models.
- Implementation cards include relevant tests, formatting, affected-crate
  checks, and documentation. Milestone exits run the full gates below.
- A terminal feature is complete only after its automated acceptance and a
  recorded Terminal.app, iTerm2, and tmux check. Record missing environments as
  pending, rather than treating them as passes.
- Keep local checkpoint receipts in this file as work progresses: source SHA,
  tree state, commands, outcomes, logs, environment, and outstanding checks.
  Do not introduce a new receipt framework for this work.

| Stage | Outcome | Prerequisite | Status |
|---|---|---|---|
| 0 | Verified stabilization baseline | Existing fixes above | Validation in progress |
| 1A | Layout, draft-state, and keyboard contracts | Stage 0 accepted before behavior changes; design can proceed while manual checks are pending | Pure layout and fixtures landed (`854c07b`) |
| 1B | Visual navigation and draft preservation | 1A contracts complete; Stage 0 accepted | Started ahead of Stage 0 acceptance by user decision, 2026-10-21; first cards landed |
| 2A | Collapsed paste blocks and lossless submission | 1B accepted | Not started |
| 2B | Popup editing and contextual footer | 2A accepted | Not started |
| Later | Diff windows, additional panes, platform extensions | Separate scoped plans | Deferred |

## Stage 0 — Finish stabilization validation

The ten findings are implemented; do not schedule their fixes again. Complete
the evidence and resolve new failures before beginning behavior changes.

1. **Formatting and regression baseline.** Apply formatting in a separate
   implementation change, then run the full gates. For any claimed pre-existing
   macOS failure, record its exact test name, failure signature, toolchain,
   environment, and reproduction against the implementation baseline above.
   To call a failure pre-branch, also reproduce it against recorded `main`;
   distinguish existing upstream failures from defects introduced by this
   branch. Historical counts such as “193 failures” are not an exemption. An
   unmatched failure remains unresolved. If needed, use an isolated local
   checkout for comparison and preserve the user's worktree.
2. **Native Linux validation.** Use a Linux host or VM, or a container running
   on Linux with usable Bubblewrap/user namespaces. A Linux Rust target on
   macOS can support a compile check but cannot supply Linux runtime evidence.
   Run the shared-code build and existing Linux containment tests, recording
   whether the backend was ready and which live assertions actually ran.
   Keep this validation local; an upstream push/PR is not a test dependency.
3. **macOS runtime evidence.** Capture Seatbelt readiness and the actual
   execution of file operations, Git, scripts, write confinement, known-secret
   denial, executable restrictions, and grant/revoke cases. Use controlled
   fixtures when existing host secret paths are absent. Record skips explicitly.
   Include controlled Mach-service/LaunchServices probes for F1, and distinguish
   a denied request from an executed escape check; do not generalize from an
   unrelated command failure. Never record real credential contents.
4. **Real-terminal checkpoint.** Test an artifact built from the recorded SHA
   and verify its source provenance; prefer an explicit local binary path over
   assuming an installed `kitsu` is current. Check the empty prompt, typing,
   wraps, status cadence, streaming/async output, popup Tab/Ctrl-C, agent keys,
   short windows, resize, overlays, suspend/resume, and terminal restoration.
   Select inline explicitly with `--tui inline`; include a smoke check with
   `KITSU_INPUT_BOX=0`.

Full automated gates, on each applicable platform:

```sh
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace --all-targets --no-fail-fast
cargo test --locked --release --workspace --all-targets --no-fail-fast
git diff --check
```

Use [invariant-tests.md](roadmap/invariant-tests.md) to select additional
authority, cancellation, JSONL, and session regressions affected by a change;
include the existing run/session boundary scripts when their boundaries change.
Record baseline warnings/failures individually as well as test failures.
Dependency changes must also respect the workspace's declared Rust version and
record any existing toolchain limitation rather than claiming an unrun MSRV
check passed. A backend-unavailable early return is not containment acceptance.

**Exit:** every review ID has its fix and appropriate executed evidence; full
gates pass or have individually reproduced baseline exceptions with no new
failures; native Linux containment runs; terminal checks are recorded. Formatting
and new regressions must be resolved, not added to a blanket failure allowance.

**Separate default-mode gate:** the
[E11 native-scrollback matrix](roadmap/cycles/e11/terminal-matrix.md) is still
unrun. Screen-state tests do not answer whether native terminal scrollback
retains region-scrolled output. Use the current `--tui inline` selector when
following that checklist; its historical `KITSU_TUI=inline` example is retired.
Record the observation during terminal checks and keep the larger matrix open
until its decision rule is satisfied. This plan does not flip the overall
default to inline.

### Local Stage 0 receipt — 2026-10-21 (unaccepted)

Source HEAD remains `55892a477ee24cc4cacf14ae19c790ee05b0a486` on
`feat/tui-rendering`; the worktree has uncommitted documentation and source
changes. No push or upstream operation was performed. Host: macOS, Homebrew
Rust 1.96.0. This shell supplied `KITSU_EFFORT=max`, `NO_COLOR=1`, and a
`/var/folders/.../T` temporary directory. These affected independent tests,
so the meaningful test invocations clear the first two and set
`TMPDIR=/private/tmp`. The mock-server suite also needs local port binding,
and was rerun outside the command sandbox.

- `cargo fmt --all -- --check`, `cargo clippy --locked --workspace
  --all-targets -- -D warnings`, and `git diff --check` pass on the current
  tree. Formatting was applied to the seven source files that failed the
  initial check. Small existing Clippy warnings and macOS-only test imports
  were corrected.
- The first full debug run with inherited settings was invalidated by local
  port and temp-path restrictions. A clean full debug run recorded 968/970
  main-binary unit tests, 256/264 CLI tests (one ignored), 48/85 PTY tests,
  and 385/389 command-tool library tests passing. Its full failure signatures
  are in `/private/tmp/kitsu-stage0-debug-clean-20261021.log`. No failure in
  that log is yet classified as an accepted baseline exception.
- The command-tool fixture corrections now pass the full 387-test library
  suite; the main-binary macOS filename correction now passes all 970 unit
  tests. The Seatbelt outside-write assertion executed and passed with an
  outside path outside the profile's explicitly permitted `/private/tmp`.
  Those focused checks used the clean environment and local execution.
- The clean release-profile full gate completed with 969/970 main-binary unit
  tests, 256/264 CLI tests (one ignored), 48/85 PTY tests, and all 387
  command-tool library tests passing. Its full signatures are in
  `/private/tmp/kitsu-stage0-release-clean-20261021.log`. The lone unit
  failure is the skills directory fixture, which passed in isolation in both
  profiles and in the subsequent complete debug unit run; concurrent
  execution remains to be investigated. The seven CLI and 37 PTY failures
  match the debug run by test name. No failing test is accepted as a baseline
  exception yet.
- One PTY case was rerun alone and still failed:
  `inline_mode_echoes_the_idle_draft_in_the_viewport_row` expects `/context`
  on the last screen row, while the current framed composer paints it on the
  content row above the bottom border. That test's single-row expectation is
  stale; the remaining PTY failures still require individual triage. This is
  evidence of an assertion mismatch, not a blanket exemption for the matrix.
- Native Linux containment, controlled macOS Seatbelt probes beyond the
  focused assertion, Terminal.app/iTerm2/tmux walkthroughs, and the E11
  scrollback matrix remain pending. Stage 0 cannot be accepted from these
  automated results.

### Failure triage — 2026-10-21 (against `9dd7bc6`)

Re-ran the suites after the `chore(stage0)` fix commit. kitsu-tools lib is
387/387 and the main binary is 970/970, which clears the receipt's
`builtins::command::*`, `skills::global_skill_count_budget`, and
`workspace::live_review_*` entries. The CLI suite still shows 7 failures and
`tests/tui.rs` 35. Setting `KITSU_INPUT_BOX=0` reduces the PTY failures from 35
to 3, which isolates the geometry-stale cases from the rest. Classification:

- **Stale box-vs-row geometry (32 PTY).** Most `inline_mode_*`/`tui_switch_*`
  cases assert the old single-row composer (region set once, composer on the
  bottom row, one pin per segment). They pass under `KITSU_INPUT_BOX=0`; the
  framed box's scroll-region re-sizing is correct. These tests must be rewritten
  to the box layout as part of resolving Stage 0, not accepted as exceptions.
- **Collides with the cwd read+write grant (2 CLI).**
  `test_interactive_explicit_denial_remains_provider_recoverable` and
  `test_interactive_confirmation_eof_interrupts_without_followup_send` time out
  because their guarded write now lands inside the auto-granted cwd and no
  longer prompts (the intended `61a1dff` behavior). Fix: target a path outside
  cwd so the denial/EOF path is still exercised.
- **macOS environment / path display (2 CLI).**
  `test_n1_non_tty_stays_byte_identical` diffs `…/tmp/…` abbreviation against its
  `<WORK>` substitution; `test_system_prompt_discloses_the_exact_pregranted_command`
  cannot resolve `pytest` in the sandbox. Neither is a branch regression.
- **Reproduce individually (3).** `test_durable_script_save_failure_*`
  (exit 0 vs 1, likely a macOS permission-bit difference),
  `test_a_styled_tty_run_stays_silent_*`, and
  `test_query_only_interactive_session_*` (startup/timing). Not yet classified.

No confirmed shipping-behavior regression. Resolving the stale geometry and the
two cwd-grant tests, plus reproducing the last three, remains Stage 0 exit work;
Stage 1A design may proceed meanwhile because it changes no behavior.

## Stage 1A — Define layout, draft-state, and input contracts

This stage establishes the contracts and pure fixtures before changing keys.
Inspect existing APIs before choosing a representation or adding dependencies.
The working transition, coordinate, consumer, and key decisions are recorded in
[tui-editor-contracts.md](tui-editor-contracts.md); they are not accepted until
the pure layout fixtures and dependency checks below are complete.

### Shared layout

Define a pure mapping between source positions and visual row/column positions,
used by both painting and navigation. It takes the draft, first-line prompt,
available width, and display projection. Compute the full draft layout before
viewport clipping; the first visible row is not necessarily the first draft row.

Cover explicit newlines, soft wraps, tabs, sanitized display text, wide and
combining characters, empty/trailing lines, exact wrap boundaries, and long
prompts. Record the character/grapheme policy and maintain explicit conversion
between source offsets and terminal cells. Leave an extension point for paste
labels whose display length differs from their backing text.

Choose a lower-level pure module/API without making the editor depend on the
terminal/ANSI renderer. Keep one authority for wrapping and hit positions.

### Draft ownership and preservation

Define an in-memory draft snapshot with source content, cursor, selection,
preferred vertical column, and the history/search state needed to resume
editing. Reserve representation for paste identities, backing text, and spans;
full paste-block implementation belongs to 2A.

Draft restoration must use state restoration, not replay stored text as input
keystrokes. The current `InlineRead` string payloads and `Cursor`-chained replay
in `read_chat_line_inline` are migration sites. Enumerate transcript/agent
overlays, attach/cycle/detach, suspend/resume, approval handoffs, completion,
queued recall, and idle/during-turn transitions. Define which state survives
each transition. Terminal descriptors/guards are not part of the snapshot.

Persisted conversation history remains expanded text. An unfinished draft must
retain editing metadata across supported transitions; display labels never
serve as its canonical content. Classic/fullscreen fallback must preserve the
full text even when it displays an expanded projection instead of paste tokens.
Do not add a disk format or session schema change just to preserve a live draft.

### Keyboard and consumer matrix

Record behavior for ordinary editing, history walk, reverse search, explicit
paste selection, paste popup, and bracketed-paste ingestion. Also record the
capabilities of inline idle input, during-turn input, classic, fullscreen, and
overlay/approval line editors. New box behavior is inline-only; other consumers
retain their existing contracts unless explicitly covered by a later card.

Fixed constraints:

- Ordinary draft Enter sends; Tab retains slash-completion priority. Paste
  editing requires an explicit, visible selection/edit state, not merely a
  caret touching a span boundary.
- Alt+Enter is the currently supported newline binding. Do not advertise
  Shift+Enter without separately implementing and verifying its byte sequences.
- Ctrl+Left/Right word motion, Shift+Left/Right agent cycling, Ctrl+X chords,
  Ctrl+O, and Ctrl+R retain their meanings outside the relevant existing modes.
- Up/Down navigate visual rows in an editable multiline draft; entering history
  occurs only at the corresponding full-draft boundary. Preserve the preferred
  column through short rows and restore the original draft/cursor after a walk.
- Keep `KITSU_INPUT_BOX=0`; include its empty-draft caret defect in 1B.

Close these choices in 1A before their implementation cards: how repeated
Up/Down traverse recalled multiline entries; how editing exits a history walk;
Home/End versus Ctrl+A/E semantics; the non-conflicting paste-focus entry and
cycling keys; and Ctrl-C/EOF/interruption behavior in the paste popup. Esc saves
and closes that popup, as already decided. Later cards must use this table
rather than invent bindings independently.

### Viewport and hints

Budget the status row above the box, box borders/content, palette or active
modal, and hint/footer below it. Define priorities and a compact fallback when
the terminal cannot fit the minimum frame; never drop a border or place a caret
outside the granted area. Reserve at least the existing transcript minimum.

Search status and other essential editor feedback must remain visible in box
mode. The current box content builder ignores `LiveDraft.hint`; 1B must restore
that feedback. Define the footer's layout now, implement essential feedback in
1B, then add paste-specific cues in 2B.

**Exit:** contracts, transition/consumer/key tables, and pure layout fixtures
cover the cases above; dependency direction is explicit; no terminal behavior
has changed merely to establish the contracts.

## Stage 1B — Visual navigation and draft preservation

Implement the agreed layout, snapshot restoration, and navigation policy in
small cards. Preserve existing editor normalization and use the same source-to-
display mapping for painting and movement. Update geometry on resize before
interpreting the next visual movement; keep the caret visible without changing
source content. Fix the fallback composer's empty-draft caret behavior.

**Acceptance:**

- Pure tests cover preferred-column movement, prompt offsets, short/empty rows,
  soft wraps, exact boundaries, and the agreed Unicode policy.
- Raw-byte tests cover CSI/SS3 spellings, fragmented input, bracketed-paste
  immunity, history/draft restoration, reverse search, and queued recall.
- Integration tests preserve full draft and cursor through overlays, agent
  switches, suspend/resume, approval handoffs, and idle/during-turn transitions.
  Mode/capability tests pin unchanged classic/fullscreen/overlay behavior.
- PTY tests exercise resize with a mid-draft caret, short-window shrink/grow,
  palette/modal restoration, async output, and essential search/hint visibility.
- Full gates and Terminal.app/iTerm2/tmux checks pass; the fallback remains
  usable. Record accepted results before starting 2A.

### Local Stage 1B receipt — 2026-10-21 (unaccepted)

Started before Stage 0 acceptance at the user's explicit request; Stage 0's
Linux, Seatbelt-probe and real-terminal checks are still open, so nothing here
is accepted. Landed:

- The framed box paints through `kitsu_render::layout` (`53a5825`): one
  source-to-cell authority for rows and caret, sanitized per scalar.
- ↑/↓ in the framed box move between visual rows with a preferred column and
  enter history only at the draft's first/last row; leaving history restores
  the draft's cursor. The editor takes the motion by injection
  (`TypeaheadEditor::set_vertical_motion`), so it keeps no layout dependency,
  and only the inline idle reader with the box enabled opts in. During-turn,
  fullscreen, overlay and `KITSU_INPUT_BOX=0` keep their keymaps.
- PTY suite: the harness pins `KITSU_INPUT_BOX` explicitly; 87/87 pass,
  including box-mode frame and ↑/↓ tests. Workspace gates: fmt, clippy and
  `git diff --check` clean; every suite passes except the five macOS CLI cases
  in the Stage 0 triage.

Open 1B cards: Home/End as visual-row start/end; the during-turn collector's
box motion; snapshot restoration in place of `Cursor` replay across overlays,
suspend, approval and queued recall; `LiveDraft.hint` in the box footer; the
fallback composer's empty-draft caret; resize PTY coverage with a mid-draft
caret; and the Terminal.app/iTerm2/tmux checks.

## Stage 2A — Collapsed paste blocks and lossless submission

Keep the established product choices: collapse a bracketed paste of **at least
two lines OR ten words**, use the kitsu frame style and dim token label, and
keep the status meter above the box. Shorter/unbracketed input keeps its current
text semantics. Recognize a paste by delimiters, never by timing or chunk size.

Choose the span representation from 1A's contract: expanded text plus range
metadata or explicit text/paste segments. Keeping a `Vec<char>` is an option,
not a constraint that justifies a lossy design. A label such as
`[Pasted text #N +M lines]` is display-only; identical typed text is ordinary
text, not a lookup key. Give blocks stable session-local identities.

Define threshold counting after the editor's existing normalization, including
CRLF, trailing newlines, and whitespace-only text. Specify label line counts,
oversized-label rendering, and when edited blocks stay collapsed. Navigation
treats a collapsed block as one logical unit while layout accounts for the
label's actual cell width. Backspace/Delete on the appropriate side removes the
whole block; insertion, word deletion, kill commands, completion, history, and
search must update or intentionally discard metadata without losing content.

**Acceptance:**

- First establish a fake-provider test proving that submission and conversation
  history contain all normalized backing text in order, with no generated
  placeholder. Preserve today's newline/control normalization without adding
  content loss. Test literal placeholder-like text as a negative control.
- Test threshold edges, multiple/adjacent blocks, insertion before/between/after
  blocks, atomic deletion, fragmented delimiters/UTF-8, and control-key payloads.
- Test snapshot restoration and all affected mutation/history/search/queue paths
  with real block metadata, rather than only plain strings.
- Adapt `crates/kitsu/tests/paste_latency.rs`: its visible tail sentinel will be
  hidden by collapse. Observe completed block display instead; independently
  verify full payload submission through the fake provider. Preserve the
  measurement's meaningful completion point and existing strict-budget option.
  Record the 10 KiB result and check larger inputs for repeated copying or
  per-byte repaint regressions.
- Full gates and real-terminal checks pass. Until popup editing lands, disclose
  that the block supports atomic navigation/deletion but not popup editing yet.

## Stage 2B — Paste popup and contextual footer

Reuse existing modal-pane painting and surface restoration. For input, reuse
the raw-text editor's UTF-8 and bracketed-paste handling with the agreed popup
key policy. The model picker's direct `decode_overlay_keys` path accepts ASCII
characters and does not preserve paste boundaries; it is not a multiline text
editor to copy wholesale.

Open only from the explicit selection/edit action defined in 1A. The popup owns
the full backing text, supports multiline navigation and resize, and does not
create recursive paste boxes. Esc saves edits and closes. Enter inside the
popup follows the editor policy, never the parent composer's send action.
Apply 1A's Ctrl-C/EOF/interruption rules and update the selected block safely
when the edit completes.

Maintain a single stdin owner. Coordinate with `APPROVAL_INPUT_ACTIVE`, the
during-turn collector, and raw-mode guards; define deferral when an approval
needs the input owner. Restore the parent draft, cursor, block selection, hints,
and viewport on every exit. Keep native text selection available.

Complete the dim footer using 1A's budget. Show only supported actions for the
current state: ordinary send/newline/completion, explicit block selection, or
popup editing with Esc save/close. Long text must truncate safely; small windows
must follow the compact fallback instead of corrupting the frame.

**Exit:** full payload remains correct after popup edits, UTF-8/multiline paste
stays content, normal Enter/Tab behavior remains intact, interrupted/short-window
paths preserve the draft and terminal, full gates pass, and the complete flow
has recorded Terminal.app/iTerm2/tmux verification.

## Later work — separate scope

Preserve this product order, with a separate plan and acceptance for each:

1. Expandable diff windows with per-file `+N/−M`, reusing `/diff` evidence.
2. Multiple panes/popups, optional fox side pane, and navigation help.
3. tmux detection/adaptation based on observed compatibility needs. tmux
   coexistence and testing already apply to every terminal milestone above.
4. Windows containment backend; WSL2 continues to use the Linux path.

Existing Linux containment regression coverage is Stage 0 work, not a deferred
feature. Additional backend parity fixtures follow the actual gaps discovered
there. No generic window framework or dependency migration is required merely
to implement one paste-edit popup.

## Local documentation and next checkpoint

Keep README, CHANGELOG, and functionality/UX documentation aligned with each
implemented behavior; reconcile existing entries rather than duplicating them.
Keep review evidence historical and update this plan's status/receipt at each
accepted stage. Preserve all existing branches and user changes.

**Next implementation step, when requested:** finish Stage 0 formatting and
baseline evidence, including native Linux and real-terminal checks. Stage 1A's
design contracts can be prepared while environment-dependent checks are pending;
Stage 1B waits for their acceptance. The known review fixes are already present.
