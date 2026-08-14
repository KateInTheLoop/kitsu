# Cycle 1 audit 03 — Claude Code terminal DX, accessibility, and Kitsu design

- Audit window: 2026-08-10–11
- Baseline: `57889b2a096993d5fcdd20d95abb2dd13af9e0ef`
- Products: Claude Code `2.1.227`; Kitsu `0.1.0 (57889b2a0969)`
Scope: read-only PTY comparison. No product files, credential/auth files, or
account settings were opened or queried. Incidental account-bearing welcome
text is redacted. Both recreated clones were clean and detached at the baseline
after the probes.

## Verdict

Kitsu already beats Claude in several trust-critical areas: `/status`,
`/permissions`, and `/context` are unusually specific; `--no-session` is
truthful; command authority is separate; Ctrl+C rolls an incomplete model turn
back; normal terminal scrollback and stdout/stderr separation preserve copying.
Its restrained rich banner also has a promising identity.

Claude is markedly ahead in interaction design. Its welcome screen establishes
hierarchy, `/help` is a navigable surface rather than a dump, commands and models
have pickers, shortcuts remain visible at the moment they matter, the editor
supports multiline drafts and bracketed paste, and interruption restores the
draft. Kitsu exposes more capability than it teaches.

The top three Kitsu improvements are:

1. **Separate “no color” from assistive/literal input, and provide a safe
   multiline affordance in the latter.** Rich-mode bracketed paste works and is
   not broken. The documented plain/`NO_COLOR` canonical reader intentionally
   makes a multiline paste submit its first line and retain later bytes as the
   next draft. That tradeoff is too surprising for `NO_COLOR` and risky for
   pasted code.
2. **Replace the flat 40-command `/help` listing with a searchable, categorized
   command palette and stateful model/reasoning picker.** Preserve the excellent
   Tab completion as the fast path.
3. **Describe effective authority, not merely registered schemas.** A session
   with zero write roots and commands disabled announced “9 enabled” tools and
   listed write/edit tools. The banner/footer should say “read-only; writes ask;
   commands off” and cancellation copy should reflect whether writes were even
   possible.

## Evidence classes and limits

- **PTY observed** means output or input behavior was exercised in a real PTY.
- **Source-confirmed** means the behavior was checked in the baseline Rust or
  project documentation.
- **Model output** is an answer produced by Claude and is not treated as host
  truth.
- **Unverified**: the full matched Kitsu architecture answer. The restored
  localhost bridge was extremely slow, so the maintainer requested that the
  turn be stopped. Ctrl+C cancelled its HTTP 200 response, discarded provisional
  output, and rolled the turn back. Runtime UX was verified; Kitsu-versus-Claude
  answer quality was not.

Claude's budget-capped print run did finish. Its architecture summary was
competent, but its proposed “sixth UX cycle” was generic and inferred from the
commit log. That is model quality, not CLI quality. The model also called itself
Sonnet despite a Haiku selection; that contradiction is discussed separately.

## Exact invocations

The long prompt is shown once as `$TASK` for readability:

```text
Perform a read-only repository orientation. Inspect this repository and: (1)
summarize its architecture, (2) report git status and the current branch, (3)
state the working directory, model/tool/permission context you can actually
observe without reading any auth or account files, and (4) propose exactly one
next maintenance step. Do not edit or create files, do not run tests, do not
access the network, and do not inspect credentials or personal metadata. Use
only read-only filesystem tools. End after the recommendation.
```

Claude:

```bash
claude --version
claude --help
claude --safe-mode --no-chrome --model haiku --effort low \
  --permission-mode plan --tools Read,Glob,Grep,Bash
claude --safe-mode --no-chrome --ax-screen-reader --model haiku \
  --permission-mode plan --tools Read,Glob,Grep,Bash
claude -p --safe-mode --no-chrome --model haiku --effort low \
  --permission-mode plan --tools Read,Glob,Grep,Bash \
  --max-budget-usd 0.30 --no-session-persistence "$TASK"
```

Kitsu, with the contents of the ignored harness config deliberately not read or
recorded:

```bash
env -u NO_COLOR TERM=xterm-256color \
  /home/mmpg/workspace/kitsu/kitsu/target/debug/kitsu \
  --config /home/mmpg/workspace/kitsu/kitsu/tests/dogfood/results/claude-cycle-harness/kitsu.toml \
  --reasoning-effort low chat --no-session \
  --allow-path /tmp/kitsu-claude-cycle1-terminal-kitsu.uZt8lX

NO_COLOR=1 /home/mmpg/workspace/kitsu/kitsu/target/debug/kitsu \
  --config /home/mmpg/workspace/kitsu/kitsu/tests/dogfood/results/claude-cycle-harness/kitsu.toml \
  --reasoning-effort low chat --no-session \
  --allow-path /tmp/kitsu-claude-cycle1-terminal-kitsu.uZt8lX
```

The exact paste payload sent to each PTY was:

```text
ESC [ 2 0 0 ~ PASTE_PROBE_LINE_1 LF PASTE_PROBE_LINE_2 ESC [ 2 0 1 ~
```

No trailing Enter was sent. Rich Kitsu and ordinary Claude retained both lines
as one unsent draft. Plain Kitsu treated the LF as line submission; Ctrl+C was
used first to cancel that turn and again to clear the retained second line.

## Sanitized PTY observations

### 1. Startup and information hierarchy

Claude first displayed a workspace trust screen:

```text
Accessing workspace: /tmp/<clone>
Quick safety check: Is this a project you created or one you trust?
Claude Code'll be able to read, edit, and execute files here.
> 1. Yes, I trust this folder
  2. No, exit
Enter to confirm · Esc to cancel
```

This is a strong stop-before-entry pattern, but its generic “read, edit, and
execute” sentence contradicted this launch's plan mode and constrained tool
selection. The following welcome panel had strong layout, tips, news, model and
mode visibility, but also showed account/plan metadata and promotion copy. Those
fields are redacted here and should not be necessary in a repository workspace.
Safe mode was prominent, as was the live `plan mode on (shift+tab to cycle)`
footer.

Kitsu rich mode was calmer:

```text
╭─ kitsu 狐 ──────────────────────────────────────────
│   /\_/\
│  ( o.o )   multi-gateway LLM familiar
│   > ω <
│
│  gateway     claude_bridge
│  model       claude-haiku-4-5-20251001
│  directory   /tmp/<clone>
│  tools       9 enabled (use /tools)
╰──────────────────────────────────────────────────────
Enter / for commands · Tab completes prefixes · Ctrl+D exits
```

Under inherited `NO_COLOR=1`, it correctly collapsed to one clean ASCII line.
Claude did not honor that environment variable in the observed ordinary TUI;
its explicit `--ax-screen-reader` mode was needed.

Kitsu wins visual calm and pipe/scrollback discipline. Claude wins progressive
orientation. Kitsu's weak row is `tools: 9 enabled`: it compresses schema
registration, permission availability, and effective authority into one number.

### 2. Help, commands, and keyboard teaching

Claude `/help` opened tabs for General, Commands, and Custom commands. General
put shortcuts into columns (`!` shell, `/` commands, `@` paths, `\`+Enter for a
newline, double-Esc to clear, Ctrl+O verbose, Ctrl+T tasks, Alt+P model,
Ctrl+G editor). The Commands tab was browsable and searchable. The footer
explained Escape. Its UI remained usable without flooding normal scrollback.

Kitsu `/help` printed approximately 40 alphabetized command signatures in one
screen-long list. The wording itself was good and came from the same registry as
completion. Rich-mode `/mod<Tab><Tab>` was excellent: it showed only `/model`
and `/models`, with descriptions, and completed `/model`. That interaction is a
better foundation than the help dump.

Kitsu lacks an always-discoverable newline/paste hint, does not teach Ctrl+C at
startup, and `/model` without an argument only says:

```text
usage: /model <name> (current: claude-haiku-4-5-20251001); /models to list available
```

Claude `/model` showed a picker with descriptions, current selection, default
versus session-only keys, and effort compatibility.

### 3. Prompt editor, multiline input, and interruption

Claude supported `\`+Enter multiline input and ordinary bracketed paste. A
multiline draft rendered as two aligned physical lines. One Escape during a
request interrupted and restored the full draft. Double-Esc cleared it, but the
gesture is timing-sensitive: two slow Esc presses did not clear, and a later
Enter submitted the retained text. The inline “Esc again to clear” cue helped,
but a deterministic chord or `/clear-input` equivalent would be safer.

Rich Kitsu also passed the exact bracketed-paste probe:

```text
kitsu> PASTE_PROBE_LINE_1
       PASTE_PROBE_LINE_2
```

It made no provider request, and Ctrl+C immediately produced:

```text
^C (input cleared; Ctrl+D or /quit exits)
kitsu>
```

Plain/`NO_COLOR` behavior is intentionally different. The source and README
confirm a literal canonical reader with no bracketed-paste negotiation,
completion, history, or cursor control. The live probe showed the practical
consequence: the first pasted line began a model turn and the remainder stayed
in the input buffer. This is internally consistent and escape-free, but
`NO_COLOR` conventionally means “no color,” not “replace my editor.” A user who
pastes a stack trace can accidentally create multiple turns.

During the live plain Kitsu turn, Ctrl+C was truthful and safe:

```text
~ inference provider attempt 1/3: cancelled; HTTP 200; cancelled
~ provisional assistant output ... was DISCARDED and was not committed
^C (response cancelled; turn rolled back - /retry to try again)
~ completed file changes, if any, remain on disk
kitsu>
```

Rollback and `/retry` are excellent. Four diagnostic lines are too much for an
ordinary cancellation, and the final write warning is needlessly alarming in a
session with no write roots.

### 4. Status, context, permissions, model, and reasoning

Kitsu's inspection surfaces were the strongest part of the comparison.
`/status` accurately included disabled session/autosave, gateway, exact model,
reasoning `low`, directory, host-verified clean Git state, project-context
source, context source/window/headroom, turn counts, compaction, permission-root
counts, commands disabled, and zero agents. `/context` attributed tokens to the
base prompt, `llms.txt`, skill index, tool schemas, user/assistant turns, and
tool results. `/permissions` separated invocation/runtime/protected roots and
said there were zero write roots and commands were disabled.

Claude `/permissions` was a strong tabbed rules editor, but even safe mode
surfaced pre-existing user-level rules. Exact rules are omitted here. Claude
`/context` rendered a memorable visual meter and autocompaction buffer, but
contradicted the active model:

```text
/model:   Haiku 4.5 ✔    Effort not supported for Haiku
/context: Sonnet 5      claude-sonnet-5
```

The budget-capped model response also claimed Sonnet. This is a Claude trust
defect, not evidence that Kitsu selected the wrong model. Conversely, Kitsu
accepted and displayed `reasoning: low` for Haiku without explaining whether
the gateway/model actually supports that knob. Both products need capability-
aware, single-source-of-truth model state.

One Kitsu inconsistency remains: `/context` and `/tools` listed
`fs_write_file`/`fs_edit_file` among “enabled” schemas while `/permissions`
showed no write roots. The policy is safe—use would prompt—but the language is
not precise enough for a user deciding whether a read-only audit is truly
read-only.

### 5. Jobs, sessions, and progress

Without starting work, Kitsu `/jobs` showed zero running/terminal jobs plus
running and record capacity. `/profiles` clearly distinguished a tool-free
reasoner from a read-only reviewer. `/queue` showed `0 / 8`. These are strong,
honest primitives, and `/quit [wait|cancel]` is better shutdown semantics than
silently abandoning work.

Claude's persistent footer (`← for agents`) and `/help` navigation made its job
surface easier to discover. Kitsu's aliases (`/agent`, `/agents`, `/jobs`) and
large grammar increase cognitive load. A lightweight control center should
present Jobs, Queue, Sessions, and Context without turning Kitsu into an
alternate-screen dashboard.

Claude used playful but changing wait verbs and showed retry countdown/attempts.
Kitsu's animated cat/fox spinner, delayed flash prevention, static reduced-
motion status, and plain heartbeat are thoughtfully implemented. On failures,
Kitsu becomes log-like (`attempt`, `terminal=true`, deadline milliseconds). The
human default should summarize phase, elapsed time, and recovery action; verbose
transport details belong behind a toggle or structured output.

### 6. Screen-reader and reduced-motion behavior

Claude `--ax-screen-reader` announced itself, flattened the trust screen and
help into linear text, labeled turns (`you:`), and used `$` as the prompt. Raw
capture still contained cursor/terminal control sequences, so “screen-reader
friendly” is not identical to “escape-free.” It retained a long promotional
welcome and account-bearing lines, which were noisy and are redacted here.

Kitsu `--plain`/`NO_COLOR` is genuinely literal and ASCII for renderer-owned
structure. `--reduced-motion` is separately discoverable and keeps styled output
with a static status. The problem is semantic coupling: `NO_COLOR`, stable
transcript mode, and assistive input are treated as the same need. They are not.

## Strengths worth preserving

### Claude

- Clear workspace trust gate before repository access.
- Excellent command/model discovery and contextual keyboard hints.
- Strong multiline editor, paste behavior, draft restoration, and menus.
- Information hierarchy and visual polish make a complex agent feel learnable.
- Explicit screen-reader mode rather than requiring environment folklore.

### Kitsu

- Best-in-comparison `/status`, `/permissions`, and `/context` provenance.
- Read roots, write roots, commands, sessions, and jobs are independent and
  inspectable; `--no-session` was clearly reflected.
- Normal scrollback, clean stdout, metadata on stderr, and strong plain/JSONL
  modes protect copy/paste and automation.
- Ctrl+C discards provisional assistant output, rolls back conversation state,
  exposes `/retry`, and does not pretend completed side effects vanished.
- Rich-mode Rustyline bracketed paste and Tab completion work correctly.
- The fox-orange accent, `kitsu 狐`, and “LLM familiar” are distinctive without
  decorating model text.

## Ranked Kitsu improvements with acceptance criteria

### 1. P1 — split color, transcript, and assistive input modes

Treat `NO_COLOR` as color policy, retain `KITSU_PLAIN`/`--plain` for literal
transcripts, and add a discoverable `--screen-reader` contract. Give literal
input an explicit multiline capture such as `/paste` … terminator or an
equivalent editor that cannot submit a partial paste.

Acceptance:

- A PTY matrix covers default, `NO_COLOR`, `--plain`, `TERM=dumb`,
  `--reduced-motion`, and `--screen-reader`.
- Sending the exact `ESC[200~A\nBESC[201~` payload in editor-capable modes makes
  zero requests until Enter and commits exactly one `A\nB` turn.
- Literal assistive mode offers and documents one escape-free way to enter
  `A\nB` as one turn; pasted trailing lines can never silently become later
  prompts.
- Every mode's raw output is asserted for ANSI/OSC policy, redraw behavior,
  Ctrl+C recovery, and copy/paste fidelity.

### 2. P1 — add a progressive command/control palette

Build on the shared slash registry and completion descriptions. `/help` should
start with categories and high-frequency actions, then support search/browse;
the full alphabetized list remains available for scripts and screen readers.

Acceptance:

- `/help` initially fits in 24 rows at 80 columns and exposes Search, Inspect,
  Model/context, Files, Jobs/queue, Sessions, and Exit.
- Typing `model`, `permission`, or `job` returns the same canonical commands and
  descriptions as Tab completion.
- Arrow, Tab, Enter, and Esc are labeled; screen-reader mode emits identical
  groups linearly without relying on cursor position.
- No alternate screen is required, and normal terminal scrollback survives.

### 3. P1 — show effective authority in the banner, footer, and `/tools`

Separate `registered`, `available on approval`, and `currently authorized`.
Prefer a compact statement such as `read: repo · write: ask/none · commands: off`
over `9 enabled`.

Acceptance:

- A zero-write-root startup never calls write/edit tools simply “enabled.”
- `/tools` labels each tool `authorized`, `asks`, or `disabled`, with its
  effective roots or reason.
- `/status`, `/permissions`, banner, background profiles, and JSONL derive from
  one authority snapshot and cannot contradict each other.
- Snapshot tests cover read-only, write-only, read/write, command-enabled, and
  detached-agent profiles at 40/80/120 columns.

### 4. P1 — make model and reasoning selection capability-aware

Provide a cached searchable picker with current/default/session-only semantics.
Explain unsupported or unverified reasoning settings before a request.

Acceptance:

- `/model` with no argument opens/selects from a catalog and never requires the
  user to copy an opaque ID.
- `/reasoning` shows current value, source, and `supported`, `unsupported`, or
  `gateway-unverified`; unsupported combinations cannot silently appear active.
- Banner, `/status`, `/context`, request telemetry, and saved sessions share one
  exact model ID and reasoning state.
- Offline catalog failure retains the current model and gives one actionable
  recovery path.

### 5. P1 — compress cancellation and retry feedback by effective risk

Keep Kitsu's rollback guarantees, but produce one calm human summary by default.
Detailed attempt/deadline fields remain available via verbose mode and JSONL.

Acceptance:

- A read-only cancelled turn prints one summary equivalent to `Cancelled · no
  response committed · workspace unchanged · /retry`.
- If verified writes occurred, the summary gives the exact count and one
  `/diff`/`/checkpoint` recovery action; it never says “if any.”
- Partial assistant text is visibly provisional or withheld and is never mixed
  with committed output.
- Ctrl+C at input, provider wait, streaming, tool execution, permission prompt,
  and queued work each has a deterministic PTY test.

### 6. P2 — add a compact, opt-in workspace control strip

Surface `read-only`, context percent, queue depth, and running jobs near the
prompt when nonzero or changed. Provide one shortcut/command to open details.

Acceptance:

- Default idle state adds at most one physical line and does not redraw model
  output.
- Queue/job completion updates preserve a partially typed rich draft; assistive
  mode announces changes at a safe boundary.
- The strip collapses cleanly below 60 columns and is absent from piped stdout.

### 7. P2 — write the missing UX specification and align the existing guides

`design.md` points to `docs/ux-design.md`, but that file does not exist.
`docs/ui_guidelines.md` is only a few rules and recommends `🦊`, `⛩`, and `✨`,
while the renderer uses `狐`, fox orange, diamonds, and cat kaomoji. Define one
testable system before adding more ornament.

Acceptance:

- `docs/ux-design.md` exists and specifies information hierarchy, palette and
  contrast, glyph fallbacks, motion, plain/screen-reader contracts, width
  behavior, stdout/stderr ownership, copy/paste, and tone.
- Every renderer-owned symbol has a plain fallback and snapshot coverage.
- Documentation contains no broken companion links and distinguishes normative
  rules from aspirational concepts.

### 8. P2 — refine the identity from “cute cat” to “quiet fox familiar”

The orange accent, `狐`, and “familiar” are strong. The `/\_/\` face and every
animated frame are recognizably cat-coded, so the identity currently conflicts
with its name. Express folklore through behavior—alert, quiet, reversible,
guiding at thresholds—plus one restrained fox-tail/ear mark. Do not scatter
kanji, shrine gates, masks, or “Japanese” glyphs as generic progress decoration.

Acceptance:

- One canonical small/large fox mark works at 40/80/120 columns and has a plain
  textual fallback; assistant content remains undecorated.
- `狐` appears only in the brand lockup, with an accessible “kitsu fox familiar”
  label in screen-reader output.
- Spinner/progress uses at most one brand accent and one changing field; no
  emoji or culturally specific religious symbol is required to understand
  state.
- A terminology review documents the meaning and intended use of any Japanese
  word or symbol.

## Maintainer disposition

Implement ranks 1, 3, and 5 as the immediate reliability/UX slice; they are
bounded, testable, and protect trust. Design rank 2 next on top of the existing
command registry rather than adding more commands. Land rank 7 before a broad
visual refresh. Keep rank 8 in the design backlog until the UX spec and
accessibility matrix exist; isolated mascot churn would be subjective and would
not yet justify a product commit.
