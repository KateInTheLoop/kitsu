# Audit 08: terminal accessibility

Baseline: `dead112` on `dogfood/top10-improvements`. Read-only audit; no hosted inference.

## Live evidence

- Non-TTY `NO_COLOR=1` chat exited 0 with no ANSI sequences; assistant output, usage, and context records were legible. Markdown markers remained literal.
- Styled PTY output rendered color and Unicode correctly, avoided a spinner flash on a fast response, and styled headings while retaining Markdown delimiters.
- A slow styled response emitted animated carriage-return frames.
- `KITSU_REDUCED_MOTION=1` emitted one static `* kitsu is thinking` line, with no animated frame changes, then cleared it before the answer.
- The explicit `KITSU_PLAIN=1` PTY attempt was interrupted during approval. `TERM=dumb`, partial-input async notifications, tool/diff output, and runtime control injection were not freshly observed because the shared mock cannot produce those payloads.

## Strengths

- Fast turns avoid spinner flash.
- Reduced motion changes animation to a visible static status.
- `NO_COLOR=1` produces compact, ANSI-free output.
- Redirected human output retains stable usage/context records.
- `KITSU_PLAIN`, `NO_COLOR`, and `TERM=dumb` share a tested plain-mode path.
- Model text sanitization preserves safe Unicode.

## Severity-ranked issues

1. **P1: plain-mode async notices can corrupt partially typed input.** Source inspection shows background-agent/write notices use concurrent `eprintln!` in plain mode, while styled mode uses Rustyline's prompt-aware external printer. This is inferred, not freshly reproduced.
2. **P2: styled Markdown leaves inline syntax audible and visible.** The PTY bolded a heading but retained `##`, `**...**`, and backticks.
3. **P2: accessibility controls are environment-only and absent from CLI help.** There is no discoverable `--plain` or `--reduced-motion` flag.
4. **P2: runtime coverage is fragmented.** The shared mock cannot exercise Unicode, ANSI/OSC controls, bidi text, or long paths, so one compatibility run cannot prove the documented matrix.
5. **P3: reduced-motion completion still uses cursor control.** The static line is erased with carriage return and CSI 2K, which some assistive terminals may announce as a redraw.

## Top three

1. Route all async notices through a prompt-safe redraw/external-printer abstraction, including a partial-input PTY regression.
2. Add discoverable `--plain` and `--reduced-motion` flags while retaining environment variables.
3. Consolidate Markdown, diff/tool status, Unicode, controls, bidi, long paths, redirected output, and partial input into one deterministic mock and golden PTY matrix.
