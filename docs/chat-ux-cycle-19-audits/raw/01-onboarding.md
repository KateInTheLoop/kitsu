# Cycle 19 — onboarding walkthrough validation (kitsu vs claude CLI)

Date: 2026-08-12
Baseline: `55a24a9` on `dogfood/codex-ux-cycle-6` (worktree
`/home/mmpg/workspace/kitsu/kitsu2/kitsu/.claude/worktrees/agent-a20d8488261a3869d`,
clean at start and at end).
Binary: `/home/mmpg/workspace/kitsu/kitsu2/kitsu/target/debug/kitsu`
(`kitsu 0.1.0 (55a24a9380fd-dirty, source commit 2026-08-12)`).
Prior gaps: `docs/chat-ux-cycle-18.md`. Only NEW findings are reported below.

## The task (new genre: onboarding walkthrough validation)

> This repo is `kitsu`, a Rust CLI. Follow the GET STARTED / quick-start
> instructions in README.md literally, executing them in the scratch directory
> `<SCRATCH>` (not in the repo root). A prebuilt binary already exists at
> `…/target/debug/kitsu` — use it wherever the README writes `kitsu`, and do not
> run a fresh cargo build. Note every step that fails, errors, or misleads a
> first-time user. Then write `docs/onboarding-friction.md` in this repo listing
> the top 3 friction points, each with the exact quote from README.md that
> caused it.

This genre is deliberately execution-heavy: it cannot be answered by reading.
It also has a checkable ground truth, which I established independently before
either run (`scratchpad/gt.sh`, `scratchpad/verifyA.sh`).

### Independently established ground truth

| Quick-start step | Real behaviour in an empty scratch dir |
|---|---|
| `kitsu --config ./kitsu.toml ask …` | `! error: config error: requested config './kitsu.toml' does not exist` |
| `kitsu init` | `created no-secret starter config at kitsu.toml` / `next: kitsu --config kitsu.toml doctor` |
| `kitsu init` (2nd time) | `! error: cannot initialize config 'kitsu.toml': already exists; refusing to overwrite it` |
| `kitsu --config ./kitsu.toml doctor` | all-green, ends `doctor: no blocking issues found — you're ready: run 'kitsu chat'` |
| `ask` before `ollama pull llama3.2` | `HTTP 404` → `gateway rejected the chat route or model 'llama3.2'` |
| `--gateway huggingface` on an init-made config | `! error: config error: unknown gateway 'huggingface' — defined gateways: ollama` |
| `chat` (piped, default llama3.2) | turn **discarded**, exit 1 (see gap A) |
| README heading | there is no "GET STARTED" heading; the section is `## Quick start` |

## Commands (token redacted)

Run A:

```
time claude -p '<task above>' --permission-mode acceptEdits
# cwd = worktree root; log: scratchpad/cycle19-runA-claude.log
```

Run B:

```
time printf '%s' '<task above>' \
| HF_TOKEN=*** /…/target/debug/kitsu \
    --config /home/mmpg/workspace/kitsu/kitsu2/kitsu/kitsu.toml \
    --gateway huggingface --max-rounds 25 --timeout-secs 420 \
    --max-output-tokens 3000 --yes chat \
    --allow-read-write . --enable-commands \
    --allow-command '["/…/target/debug/kitsu","init"]' \
    --allow-command '["/…/target/debug/kitsu","doctor"]' \
    --command-root .
# cwd = worktree root; log: scratchpad/cycle19-runB-kitsu.log
```

Follow-up probes (all HF Llama-3.3-70B, logs in scratchpad):
`probe_tilde.sh`, `probe_tilde2.sh`, `probe_tilde3.sh`, `probe_allowerr.sh`,
`probe_read.sh`.

## Results

| | Run A (claude) | Run B (kitsu + HF Llama-3.3-70B) |
|---|---|---|
| Wall time | **7 m 05.8 s** | **12.05 s** |
| Rounds used | n/a | 3 of 25 |
| Commands actually executed from the README | 12+ (ollama pull, ask, init, doctor, chat ×4, HF 401 path) | **0** |
| Artifact | 4.6 KB, 3 friction points + 3 minor notes, each with exact quote, error text, and a fix | 470 B, 3 bare quotes, no execution evidence |
| Grade | **A** | **F** |

### Run A — grade A

Every claim in `docs/onboarding-friction.md` reproduced when I re-ran it myself:

- `config error: requested config './kitsu.toml' does not exist` — reproduced.
- `unknown gateway 'huggingface' — defined gateways: ollama` — reproduced.
- The `chat`-discards-the-answer bug — reproduced on the first try:
  ```
  ◆ Agent(x0: )
    ◇ error: InvalidArgs — invalid arguments: invalid type: string "[\"hello world\", …
  Hello User!
  ~ provisional assistant output was DISCARDED and was not committed: 1 tool call(s) failed during the turn (0 local policy denial(s))
  ```
- All three README quotes are byte-exact (verified by grep at README lines 58,
  262, 735).

It went beyond the letter of the task in useful ways: it exercised the HF-401
path, tried both piped and PTY `chat`, and separated README defects from kitsu
defects. The one soft spot is that its friction #2 is really a kitsu bug rather
than a README quote problem — but it is the most valuable finding of the cycle,
so this is a feature.

### Run B — grade F

The model never ran a single quick-start command. Round 1 it delegated the
whole prompt verbatim to a subagent; the subagent tried one `bash -c "echo … >
docs/onboarding-friction.md"` (not in the allowlist) with the scratch dir as
`cwd`, was denied, and returned one line. The main agent then *read README.md
and stopped executing*, and produced three quotes lifted by eye:

```
1. "kitsu ships with a ready-to-use `[kitsu.toml](kitsu.toml)` that defines several example gateways. …"
2. "To use kitsu without passing `--config` every time: … mkdir -p ~/.kitsu / cp kitsu.toml ~/.kitsu/config.toml"
3. "kitsu treats both the same way — a gateway profile and a `--gateway` flag. The difference is where the model runs and what it costs."
```

Accuracy of the friction report itself: **0/3 usable.**
- #1 is a real friction area but the quote is **not exact** — README writes
  ``[`kitsu.toml`](kitsu.toml)``, the report writes `` `[kitsu.toml](kitsu.toml)` ``,
  and no failure is demonstrated.
- #2 and #3 are quoted from *outside* the Quick start section
  ("Where the config lives", "Hosted vs local").
- #3 is not friction at all; the reasoning given ("does not provide a clear
  explanation of the differences") contradicts the quote it cites.
- Zero of the eight real ground-truth failures were found.

**Two pre-approved commands (`kitsu init`, `kitsu doctor`) were handed to it and
neither could ever have run** — see gap 2. So even a perfectly behaved model was
capped at partial credit under the brief's own flags.

## KEY EVIDENCE — the cycle-18 fixes

| Cycle-18 gap | Status | Evidence |
|---|---|---|
| #1 tilde cwd self-sabotage | **FIXED** | `probe_tilde3.sh`: with a *literal* directory named `~` inside the command root, `cwd="~"` resolved to `/home/mmpg`, not `./~`. Expansion is real, not display elision. |
| #2 sandbox vs linked worktrees | not exercised (no git this cycle) | — |
| #3 environment denials are fatal | **HALF-FIXED** | Recoverable *in-loop*: an inaccessible cwd (`ExecError — command cwd '/home/mmpg/tmp-does-not-exist-xyz' is not accessible`) was fed back and the model retried in the next round. Still fatal *at commit*: see gap 1. |
| #4 denials should name the approved roots | **FIXED for the model, BROKEN for the human** | The model quoted the full root list back verbatim ("The approved command roots are limited to '…/probe4'"), so the text reaches the model. The terminal receipt is cut at 180 chars mid-path: `… is outside the approved command roots ('/tmp/claude-1000/…/scratc` — see gap 3. |
| #5 Llama `<\|python_tag\|>` syntax | not reproduced this cycle | Llama-3.3-70B emitted structured tool calls throughout. |

Run-summary truthfulness (Run B): `~ run summary: 1 turn(s) ok, 1 file(s) read,
1 file(s) written (1 change(s)), session not saved` — the read and write counts
are **correct** (main agent read README once, subagent read nothing, one file
written). The `1 turn(s) ok` is where it misleads: see gap 5.

## Regression sweep

- **Window false alarm: absent.** Every HF run reported
  `{"used":…,"effective":128072–130472,"window":131072}` with no mismatch notice.
- **Ranged read receipt: sane** — `◇ 20 lines (ranged read 55-75)`. Minor nit:
  `55-75` reads as 21 inclusive lines but 20 were returned, so the endpoint
  convention is ambiguous.
- **Capped read receipt: present and honest in kind** — `◇ 1731 lines, capped at
  the byte limit` (cycle-18 fix holds, no off-by-one marker line). But it omits
  the total; see gap 4.
- **No skill-confusion misfires.** `~ 2 skill(s) available` in every run, never
  invoked spuriously.
- **New-ish**: `--no-subagents` does not exist (`tip: a similar argument exists:
  '--no-session'`). Not a regression, but there is no way to force single-agent
  execution, which is what made Run B's round-1 self-delegation unpreventable.

## Model-fault ledger (Run B)

| # | Fault | Kitsu could mitigate? |
|---|---|---|
| M1 | Delegated the entire verbatim prompt to a subagent on round 1 instead of executing | Partly — no `--no-subagents` escape hatch exists |
| M2 | Reached for `bash -c` (never allow-listed) when `fs_write_file` was granted | Yes — denial named the cwd, not the missing grant (gap 5 note) |
| M3 | Passed a `cwd` outside `--command-root .` | No, correctly denied |
| M4 | After one denial, abandoned execution entirely with 22 rounds left; never tried the two pre-approved commands | Partly — but those commands were unrunnable anyway (gap 2) |
| M5 | Misquoted the markdown link while claiming an "exact quote" | No |
| M6 | Sourced 2 of 3 quotes from outside the section the task named | No |

Split: Run B's failure is ~80% model. Kitsu's 20% is real though — the
allowlist it was given could not execute, and nothing said so until runtime.

## RANKED top-5 NEW gaps

1. **A policy-denied turn destroys a correct final answer, and there is no
   escape hatch.** In `probe_allowerr.sh` the model produced a textbook
   explanation (naming the roots and the tilde expansion) and kitsu printed
   `~ provisional assistant output was DISCARDED and was not committed`.
   `--allow-turn-errors` only flips the exit code (EXIT=0) — the text is still
   destroyed. Deadline-expired output gets `CommittedPartial` treatment; denial
   output does not.
   *Fix:* in `crates/kitsu/src/main.rs:6428 finish_assistant_disposition`, route
   denial-final answers to `CommittedPartial` (print, labelled advisory /
   uncommitted) instead of `Discarded`.

2. **Startup accepts an `--allow-command` whose executable lives outside every
   command root, so the grant can never execute.** Run B was launched with two
   pre-approved `…/target/debug/kitsu` commands and `--command-root .`; the
   banner was silent, and only a runtime attempt reveals
   `PolicyDenied — mandatory command containment (ExecutableOutsideVisibleRoots)
   is unavailable`.
   *Fix:* extend `pregrant_error` (`crates/kitsu/src/main.rs:3512`) — it already
   root-checks the *cwd* — to root-check the executable and fail fast with
   `--command-root <exe dir>`.

3. **Tool-error receipts hard-truncate at 180 chars, first line only, with no
   ellipsis — silently eating the remedy text shipped for cycle-18 gap #4.**
   `crates/kitsu-core/src/orchestrator.rs:1777`:
   `let detail: String = detail.chars().take(180).collect();` after
   `.lines().next()`. The human sees `… approved command roots ('/tmp/…/scratc`
   and cannot tell it was cut.
   *Fix:* append `…` when truncated and keep the remedy clause (or raise the cap
   for `PolicyDenied` and point at a `/errors` full-text view).

4. **Capped reads never disclose the true size, so a model can read 11% of a
   file and believe it is done.** `probe_read.sh`: `◇ 1731 lines, capped at the
   byte limit` for a 15,682-line `main.rs`; the model then answered `DONE`.
   *Fix:* render `1731 of 15682 line(s), capped at the byte limit` and put the
   same total in the tool result the model sees.

5. **The run summary reports `N turn(s) ok` while hiding denied/failed tool
   calls, especially in subagents.** Run B printed `1 turn(s) ok, 1 file(s)
   read, 1 file(s) written` on a run whose only command attempt was
   `PolicyDenied`. Counts are truthful; the verdict is not.
   *Fix:* add `, 1 denied / 0 failed tool call(s)` to the summary whenever
   either is non-zero, counting subagent calls (the read counter already spans
   subagents).

### Also observed (below the cut)

- **Denial precedence misdirects repair.** `resolve_cwd` runs before the argv
  allowlist check, so Run B's un-allowlisted `bash -c` was told to fix its
  *cwd*. A correct cwd would still have been denied. Check argv approval first,
  or name both problems.
- Ranged-receipt endpoint convention (`55-75` for 20 lines) is ambiguous;
  cycle-18's carried "range-total disclosure" gap is still open.
- README defects Run A surfaced are real and worth fixing in the repo:
  Quick start never says "run these from the checkout"; `kitsu init`'s config
  cannot serve the very next `--gateway huggingface` block;
  `doctor`'s success hint drops `--config`; and there is no
  "GET STARTED" heading to follow.

## Loop state

Worktree ends clean (`git status --porcelain` empty; both runs' artifacts saved
to `scratchpad/runA-onboarding-friction.md` and
`scratchpad/runB-onboarding-friction.md`). Rotation: cycle 19 = Opus (this
file); cycle 20 = Sonnet. Suggested cycle-20 opener: gaps 1 + 3 — both are
few-line changes and both were the direct cause of a session producing nothing
usable this cycle.
