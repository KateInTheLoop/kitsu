# Audit 7 — terminal safety and accessibility

Baseline: `c485edb`. Styled, plain, `NO_COLOR`, reduced-motion, malicious model
IDs/session metadata/command arguments, and split CSI/OSC/C0 sequences were
tested against a local mock. The repository remained untouched.

## What worked

- Plain and reduced-motion flags are discoverable.
- Prompt-safe notices, model/tool/session sanitization, and split-chunk CSI,
  OSC, and C0 filtering are covered by focused tests and live probes.
- Plain telemetry is compact and verified diffs/session metadata are scrubbed.

## Ranked findings

1. **High — bidi and other Unicode format controls survive terminal
   sanitization.** U+202E and related `Cf` characters are not classified by
   Rust as ordinary control characters. They can reorder or hide model IDs,
   session previews, job activity, attachment labels, and command argv without
   emitting CSI/OSC bytes.
2. **High — reduced-motion status can merge with the next notice.** The styled
   static status uses carriage-return positioning without a terminating newline;
   a provider-attempt notice can render as `* kitsu is thinking~ provider...`.
3. **Medium — interactive plain/NO_COLOR still emits Rustyline bracketed-paste
   escape sequences.** Output is colorless but not truly escape-free for screen
   readers or literal transcript consumers.
4. **Medium — Markdown punctuation remains noisy in assistive output.**
5. **Low — tables and long fields are not width-aware.**

## Top improvements

1. Strip or visibly encode bidi, zero-width, and dangerous format controls.
2. Give reduced-motion status an exclusive line with deterministic termination.
3. Add an assistive plain editor mode that disables terminal control sequences.

