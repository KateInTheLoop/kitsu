# Audit #8 — terminal accessibility, read-only on `dead112`

Repository remained untouched. Worktree was clean on `dogfood/top10-improvements`. No hosted inference or paid tokens were used.

## Live evidence

Observed commands:

```sh
target/debug/kitsu --config /tmp/kitsu-ux-live-config.toml \
  chat --no-session "render markdown accessibility probe"
```

- Non-TTY capture under the inherited `NO_COLOR=1`.
- Exit 0.
- No ANSI sequences appeared.
- Assistant output, `kitsu-usage`, and `kitsu-context` were legible and stable.
- Markdown markers such as `##`, `**...**`, and backticks remained literal.

```sh
env -u NO_COLOR TERM=xterm-256color target/debug/kitsu \
  --config /tmp/kitsu-ux-live-config.toml \
  chat --no-session --no-project-context \
  "render markdown accessibility probe"
```

- Styled PTY banner rendered correctly with color and Unicode.
- Fast response showed no spinner flash.
- Heading received bold styling, but Markdown delimiters remained visible.

```sh
env -u NO_COLOR TERM=xterm-256color target/debug/kitsu \
  --config /tmp/kitsu-ux-live-config.toml \
  chat --no-session --no-project-context \
  "slow markdown accessibility probe"
```

- Normal motion emitted three carriage-return animation frames before the response: `(=^･ω･^=)`, `(=^･ｪ･^=)`, `(=ↀωↀ=)`.

```sh
env -u NO_COLOR TERM=xterm-256color KITSU_REDUCED_MOTION=1 \
  target/debug/kitsu --config /tmp/kitsu-ux-live-config.toml \
  chat --no-session --no-project-context \
  "slow reduced motion probe"
```

- Reduced motion worked: exactly one static `* kitsu is thinking` status appeared.
- No animated frame changes occurred.
- The static line was cleared with `\r`/CSI before the answer.

The explicit `KITSU_PLAIN=1` PTY attempt was interrupted during approval. `TERM=dumb`, async partial-input notifications, tool/diff output, and runtime Unicode/control injection were therefore not freshly observed. The shared mock itself has no Unicode/control payload.

## Strengths

- **Observed:** Fast turns avoid spinner flash.
- **Observed:** Reduced motion changes animation to one visible static status.
- **Observed:** `NO_COLOR=1` produces compact, ANSI-free output.
- **Observed:** Redirected human output retains stable usage/context records.
- **Source-confirmed:** `KITSU_PLAIN`, `NO_COLOR`, and `TERM=dumb` share the plain-mode path (`crates/kitsu-render/src/lib.rs:175-186`).
- **Source-confirmed:** model text is sanitized while safe Unicode is preserved (`crates/kitsu-render/src/lib.rs:131-136`, tests at `:899-907`).
- **Source-confirmed:** reduced-motion mode and static-status behavior have focused tests (`crates/kitsu-render/src/lib.rs:370-438`, `:1043-1063`).
- **Documented:** the accessibility contract is unusually explicit (`docs/functionality.md:193-198`, `README.md:589-593`).

## Severity-ranked issues

1. **P1 — Plain-mode asynchronous notices can corrupt partially typed input** — inferred from source, not freshly reproduced.

   `crates/kitsu/src/main.rs:3418-3436` sends background-agent/write notices using raw concurrent `eprintln!` in plain mode. Styled mode instead uses Rustyline’s prompt-aware external printer at `:3437+`. In the accessibility mode most likely to be used with a screen reader, a completion notice can splice into the visible editing buffer or be announced in an ambiguous order.

2. **P2 — Styled “Markdown” rendering leaves inline syntax audible and visible** — observed.

   The styled PTY bolded the heading but still printed `##`, `**Markdown**`, and `` `inline code` `` literally. This makes prose noisier for screen readers and gives developers a half-rendered result. Relevant renderer area: `crates/kitsu-render/src/lib.rs:522-594`.

3. **P2 — Accessibility controls are environment-only and undiscoverable in CLI help** — observed.

   `chat --help` exposes neither `--plain` nor `--reduced-motion`; users must know exact environment names. This is especially awkward when a shell already exports `NO_COLOR`, as this audit did. It also conflicts with the internal guidance “Respect --plain” (`docs/ui_guidelines.md:12`).

4. **P2 — Runtime accessibility coverage is fragmented** — observed harness gap.

   `/tmp/kitsu-ux-mock.py` covers basic Markdown and a diff after a tool call, but cannot emit Unicode, ANSI/OSC/control strings, bidi text, or long paths. Those cases exist in a separate mock on another port. Consequently, one deterministic compatibility run cannot prove the documented contract across normal/plain/dumb/reduced/redirected modes.

5. **P3 — Reduced-motion status still performs cursor control on completion** — observed.

   Animation is correctly removed, but the static status is erased using carriage return plus `CSI 2K`. Usually harmless, but some assistive terminals may announce both the status and redraw. Combining reduced motion with plain mode likely avoids styling, but that combination was not freshly verified.

## Top three urgent improvements

1. Route plain-mode asynchronous notices through a prompt-safe redraw/external-printer abstraction, with a PTY regression where a background completion arrives while input is partially typed.
2. Add discoverable `--plain` and `--reduced-motion` flags, retaining the environment variables as defaults/automation controls.
3. Consolidate Markdown, diff/tool status, Unicode, ANSI/OSC/control, bidi, long-path, redirected-output, and partial-input cases into one deterministic mock and a golden PTY matrix for normal, `KITSU_PLAIN`, `NO_COLOR`, `TERM=dumb`, and reduced-motion modes.
