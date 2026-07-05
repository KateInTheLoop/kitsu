# ISSUE-003 — Streamed answer runs into the `kitsu-usage` line (no separation)

- **Status:** OPEN
- **Severity:** Low (cosmetic; interactive only)
- **Found by:** dogfooding, Stage 1 (every streamed turn)
- **Area:** `kitsu-core` (usage emit ordering) + `kitsu-render`

## Symptom
A streamed answer and the machine-readable usage line render with no separation:

```
OKkitsu-usage: {"prompt_tokens":439,"completion_tokens":2,"total_tokens":441}
```

## Root cause
Order of writes for a streamed turn: stdout gets the text (no trailing newline) →
`run_agent_turn` emits `kitsu-usage:` to **stderr** → the caller then writes the
finishing newline to stdout. On a TTY both streams share the screen, so the
stderr usage line lands between the answer text and its newline.

## Proposed fix
Emit the finishing newline on stdout **before** the usage line, i.e. flush a
newline after the streamed text at end-of-stream (in the streaming path) and emit
usage after. Alternatively, suppress the `kitsu-usage` stderr line when stdout is
an interactive TTY (it exists for external consumers, not humans) and keep it for
non-TTY/piped runs.

## Verification
- A streamed interactive turn shows the answer on its own line, then the usage
  line (or no usage line on a TTY, per the chosen fix).
- `--no-stream` and piped/non-TTY runs are unaffected (usage still emitted for
  external consumers).
