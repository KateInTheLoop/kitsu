# E11 / T0-S6 — the live terminal matrix (handover checklist)

**Status: not run. This is the one piece of T0's go/no-go that cannot be
answered from this repository**, and it is deliberately written as a checklist
for a human at a real terminal rather than guessed at.

## Why automation cannot answer it

The question (Q1) is whether a terminal's **native scrollback** retains lines
that scrolled out of a DEC scroll region (DECSTBM). Finding F13 established
that the `vt100` emulator the test suite uses **cannot** answer it: its
`Grid::scroll_up` pushes a row into scrollback only when no scroll region is
active, and the inline viewport always sets one, so region-scrolled rows are
discarded by construction. A test pins that limitation negatively so it is
recorded in code rather than folklore.

This is not a defect in `vt100` — the VT spec does not say what scrollback
should do here, so each terminal decides. That is precisely why it needs
measuring across real terminals rather than reasoning about.

## What hangs on the answer

The mechanism choice, recorded in the design's §4.2:

- **Region-flow** (what is implemented): set a scroll region, let the
  transcript scroll inside it, keep the viewport rows pinned below. Cheap, and
  every existing direct writer keeps working untouched — measured, see the
  go/no-go record's Q3.
- **`insert_history` fallback** (Codex-style, deliberately not built): no
  region; each transcript line is inserted above the viewport explicitly. This
  keeps native scrollback working **but requires every writer to go through the
  seam**, which is the several-hundred-call-site branch that Q3's answer let us
  avoid.

If a mainstream terminal loses scrollback under region-flow, users of that
terminal lose the ability to scroll back through a session — a serious
regression, and the reason this question gates making inline mode a default. It
does **not** block the preview, which is opt-in.

## How to run it

For each terminal below, with a build of this branch:

```sh
# 1. Produce more transcript than one screen, inside the region.
KITSU_TUI=inline kitsu chat --no-session
#    At the prompt, run `/help` two or three times so output exceeds the
#    window height, then `/quit`.
```

Then record, per terminal:

| Observation | How to check |
|---|---|
| **Scrollback retains the transcript** | Scroll up (mouse wheel / Shift+PageUp). Can you see output that scrolled out of the region? This is Q1. |
| **The pinned row stayed pinned** | During the run, did the status row stay on the last line while output flowed above it? |
| **No corruption on exit** | After `/quit`, is the shell prompt in a sane place, with no leftover region (type a long command — does it wrap normally)? |
| **Resize behaviour** | Resize the window mid-session, **while a draft is typed**. The pinned row must follow to the new bottom and the transcript must scroll in the full new height, with the draft intact. Shrink below three rows: the viewport must release entirely and the shell get a normal terminal back; grow again and it must come back. (F16 is handled as of T0-S11 — this row now **verifies** it.) |
| **Resize actually wakes the reader** | **Now covered in CI** by the pseudo-terminal resize tests (T0-S12), which fail if the handler is installed with `SA_RESTART` — verified by setting that flag deliberately. What is left for a human here is narrower: drag the window **slowly**, and in a multiplexer. Per F26 a signal landing mid-paint is applied one event late, so a slow drag may show the row catching up behind the cursor; and tmux/screen pass their own signal through a second tty layer that no pty test models. |
| **Short window** | Shrink to 2-3 rows. The viewport must not install at all and the classic renderer takes over (the F19 guard). |
| **Suspend and resume** | Press Ctrl+Z at an idle prompt. kitsu must stop and the shell must come back with a *normal* terminal — type a long line and confirm it wraps against the full screen, not a subset. Then `fg`: the pinned row must come back and any draft you had typed must still be there. (F22 — no automated coverage; this row is the only check.) |

## The matrix to fill in

| Terminal | Version | Scrollback retained? | Row pinned? | Clean exit? | Resize | Notes |
|---|---|---|---|---|---|---|
| xterm | | | | | | the reference implementation |
| GNOME Terminal / VTE | | | | | | |
| Alacritty | | | | | | |
| kitty | | | | | | |
| iTerm2 (macOS) | | | | | | |
| Terminal.app (macOS) | | | | | | |
| Windows Terminal | | | | | | via WSL |
| tmux (inside any of the above) | | | | | | multiplexers own the region too |
| zellij | | | | | | |
| screen | | | | | | |

**Multiplexers matter most.** tmux and zellij maintain their own scrollback and
their own notion of the scroll region, so they can differ from the terminal
hosting them. Test tmux both with and without its alt-screen handling, since
that is the plan's stated variable.

## Decision rule, fixed in advance so the result cannot be rationalised

- **All tested terminals retain scrollback** → region-flow stands; close Q1;
  `insert_history` is never built.
- **Only obscure terminals lose it** → region-flow stands; document the
  limitation and keep the preview opt-in for them.
- **A mainstream terminal or either multiplexer loses it** → region-flow cannot
  become the default. Either build the `insert_history` fallback and select it
  per terminal, or accept that inline mode stays opt-in permanently. This is the
  outcome that would reopen the several-hundred-writer question, so it must be
  recorded honestly rather than argued away.


---

## The Windows half, measured in T3-S11 (this part did not need a human)

T3-S11 bundles the live matrix above with a **Windows `cargo check`**. The
second half is automatable and has been run; only the matrix still needs you.

**Result: the renderer and viewport crates are already Windows-portable.**
`rustup target add x86_64-pc-windows-gnu` succeeds, and then:

| crate | `cargo check --target x86_64-pc-windows-gnu` |
|---|---|
| `kitsu-tui` (contains `viewport.rs`) | **clean, zero errors** |
| `kitsu-render` | **clean, zero errors** |
| `kitsu-editor` | **clean, zero errors** |

**The whole-binary check cannot run on this machine**, and the plan row's
reason was wrong. It says "(no linker needed)" — true, `cargo check` does not
link — but the check dies earlier, in a **build script**:

```
error: failed to run custom build command for `ring v0.17.14`
  CC_x86_64-pc-windows-gnu = None
```

`ring` compiles C and needs a MinGW cross-compiler for the target. It arrives
through `kitsu-providers` (TLS), not through anything the terminal work touches.
Recorded as **F59**.

**So what remains for a human, on the Windows side only:**
1. Install a MinGW cross-compiler (`x86_64-w64-mingw32-gcc`, e.g. Debian's
   `gcc-mingw-w64-x86-64`) **or** run the check on a Windows machine, then
   `cargo check --target x86_64-pc-windows-gnu -p kitsu`.
2. With a compiler in hand, narrow the `cfg(unix)` gates where the code
   genuinely compiles — **4** in `tui_bridge.rs` and **85** in `main.rs`.
   Deliberately **not** attempted here: narrowing a platform gate without a
   compiler to prove it is guessing. `viewport.rs` already has none.

Nothing above affects the matrix checklist; the DECSTBM scrollback question is
still the thing only real terminals can answer.
