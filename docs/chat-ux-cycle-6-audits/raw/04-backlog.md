# Cycle 6 — Audit 4: Backlog verification and ranked candidates

Date: 2026-08-10. Auditor: UX auditor 4.
Binary: `target/debug/kitsu` → `kitsu 0.1.0 (57889b2a0969, source commit 2026-08-10)` — matches repo HEAD `57889b2`, so cycle-5 provenance fix is holding.
Gateway: local Ollama, `qwen2.5-coder:latest` present. No repository edits were made; fixtures under `/tmp/cycle6-audit4/`.

Environment constraint: the host CPU was saturated during this window — raw Ollama could not
emit a 5-token completion in 110 s (`curl -m 110 http://localhost:11434/v1/chat/completions ...`
returned nothing), and `kitsu ask "Reply with the single word: ready"` exhausted a 280 s budget.
Live-inference verification was therefore replaced by code inspection plus the offline regression
suites (`cargo test -p kitsu-core --offline`: 79 pass; `-p kitsu-render`: 21 pass). Kitsu's own
behavior under the stall was correct: clean deadline diagnostics, `DISCARDED`/not-committed
disposition notices, and rollback messages — incidentally re-confirming cycle-5 items 4 and 6.

## A. Standing backlog (cycle-5 "Important follow-ups") — verification

### 1. Host-owned /diff requires widening model read authority — STILL TRUE (reproduced live)

Repro (fixture git repo with one dirty file):

    cd /tmp/cycle6-audit4/repo
    printf '/diff\n/quit\n' > /tmp/cycle6-audit4/diff.txt
    KITSU_CONFIG=.../kitsu.toml kitsu --gateway ollama --model qwen2.5-coder:latest \
      chat --no-session --script /tmp/cycle6-audit4/diff.txt

Result: `cannot show diff: read permission required for /tmp/cycle6-audit4/repo: policy denied:
... grant it with --allow-path <dir> (or /allow <dir> mid-session)`.

Re-running with `--allow-path /tmp/cycle6-audit4/repo` renders the diff, but `/permissions` in the
same session then lists the repo as a model-facing read root ("invocation/base, launch-directory
read"). Code: `authorized_workspace_review` (crates/kitsu/src/main.rs:3031) calls
`perms.check_read(&root)` — the host-owned evidence path is gated on the model's read-root policy.
Inconsistency: standalone `kitsu review` from the same directory verifies the working tree with
NO grant at all (`~ verified target working_tree` printed before the model call). So the same
host-verified evidence is free in one entry point and costs whole-repo model read authority in
the other.

### 2. Exact command pregrants lack an explicit cwd — STILL TRUE (reproduced live, sharper than filed)

`kitsu chat --help`: `--allow-command <JSON_ARGV>` — "Pre-approve one exact argv **in the launch
directory**". No cwd parameter exists; there is `/revoke-command` but no `/allow-command` slash.
Code: crates/kitsu/src/main.rs:7318 `policy.grant_command(argv, &launch_dir)` and
`reset_command_session` (main.rs:2813) re-grants against the launch cwd only.

Sharp edge, reproduced:

    cd /tmp/cycle6-audit4
    kitsu ... chat --no-session --command-root /tmp/cycle6-audit4/repo \
      --allow-command '["pwd"]' "..."
    => invalid --allow-command: policy denied: command cwd '/tmp/cycle6-audit4' is outside the
       approved command roots

i.e. combining `--allow-command` with a `--command-root` that differs from the launch directory
fails closed at startup — non-interactive command automation only works if you happen to launch
from inside the root.

### 3. Stateless command artifacts lack a retention UX — PARTIALLY FIXED; cross-run gap remains (reproduced live)

Fixed since cycle 5: in-session `/artifacts [list|prune <id|all>]` exists with per-sink quotas
(32 slots / 256 MiB, crates/kitsu/src/command_artifact.rs).

Still true: retention is per-process only. Stateless `--enable-commands` runs write generations
under `~/.kitsu/profiles/<hash>/artifacts/runtime-*` (main.rs:7493: command runs use
`state_dir.join("artifacts")`; the `OffloadDirGuard` cleanup applies only when commands are
disabled). Observed: orphaned `runtime-*` dirs with `command-output-N.txt` files from earlier
cycles persist on disk; both of today's exited runs left new `runtime-*` dirs behind; and a fresh
session's `/artifacts` reports `0 retained ... none` — it cannot see, count, or prune any of them.
There is no `kitsu artifacts` subcommand (top-level help: init/doctor/ask/review/models/sessions/
chat) and no startup GC. Retained command output can contain secrets, so this is disk hygiene AND
a quiet privacy liability.

### 4. Model/session selection is not searchable — PARTIALLY FIXED

Fixed: `kitsu models [FILTER]` (case-insensitive substring; verified live: `kitsu models qwen` →
one hit; `models nomatch` → guided empty-state) and slash `/models [filter]`. `/model <name>` is
now catalog-validated (`model 'tiny-fast-thing' is not in the gateway catalog - run /models`).
Still true: sessions are not searchable — `kitsu sessions list` takes no filter argument,
`/sessions` takes no args, `/resume <id|last>` and `sessions show/resume/fork/...` need exact ids.
No fuzzy pick or completion for `/model` either.

### 5. Plain Markdown lacks a screen-reader-oriented rendering mode — STILL TRUE

crates/kitsu-render/src/lib.rs `format_message`: with styling off (`--plain`/non-TTY) the text is
returned verbatim — raw `#`, `**`, ``` fences reach the screen reader; styled mode only bolds
headings/colors diff lines and deliberately "retains markers". No mode strips markers or
verbalizes structure (headings/lists/code boundaries). The a11y-positive pieces from cycle 5
(`--plain`, `--reduced-motion`, newline-stable output) are present, but no SR rendering mode.

## B. docs/issues/ISSUE-*.md status against current binary

| Issue | Claim in doc | Current-binary verdict |
|---|---|---|
| 001 text-protocol tool calls not executed | RESOLVED | Fixed. `parse_text_tool_call` in kitsu-core; `test_text_protocol_{bare,fenced,tagged,ignores_prose,unknown_tool_ignored}` all pass. (Live model run infeasible this window — see constraint.) |
| 002 raw tool-call JSON echoed | RESOLVED | Fixed. Suppression gated on synthesized flag; `structured_event_log_never_emits_synthesized_text_tool_json` passes. |
| 003 usage line runs into answer | RESOLVED | Fixed. Usage/context records go to stderr, stdout carries assistant text only; live outputs this audit showed `kitsu-context:` cleanly separated on its own line. |
| 004 invalid model → raw repeated 404s | RESOLVED | Fixed — re-verified live this audit: `/model tiny-fast-thing` rejected with "not in the gateway catalog - run /models"; a 404 turn renders "gateway rejected the chat route or model 'X' (HTTP 404) - verify the catalog with `kitsu models` ... /model <valid-id> ... check chat_path", plus rollback disclosure. No raw JSON wall. |
| 006 only first text-protocol call runs | RESOLVED | Fixed. `test_text_protocol_multiple_calls_in_one_turn` passes. |
| 007 repeated identical failures burn all rounds | RESOLVED | Fixed. Loop-breaker at orchestrator.rs:790 (`consecutive_failures`, REPEAT_FAIL_LIMIT); `test_loop_breaker_on_repeated_failure` passes. |
| 008 buffering over-suppressed real answers | RESOLVED | Fixed. Suppression keyed to `synthesized_call` only; covered with 002's invariants in the 79-test core suite. |

(There is no ISSUE-005 file.) All seven filed issues are genuinely resolved in the current tree;
none reproduces at code/test level, and 003/004 were re-verified live.

## C. Ranked top 3 for this cycle (Codex-CLI-class daily UX, effort vs impact)

### Rank 1 — Decouple host-owned /diff and /review evidence from model read authority (backlog #1)

Impact: high — reviewing your own working tree is the everyday inner loop for a Codex-CLI-class
user, and today it either fails (scripted, deny-by-default) or forces you to hand the model
whole-repo read authority to look at your own diff. It is also a consent anti-pattern: the
permission prompt trains users to widen model authority for a host-only operation. The standalone
`kitsu review` already proves the evidence path can be host-authorized. Effort: medium —
`authorized_workspace_review` is a single choke point; replace `perms.check_read(root)` with a
host-evidence authorization (session-scoped user consent that never lands in model read roots),
keeping private-path exclusions.

Acceptance test:
1. In a git repo with one dirty tracked file, run
   `kitsu chat --no-session --script s.txt` (s.txt = `/diff`, `/permissions`, `/quit`) with NO
   `--allow-path`. Expect: the working-tree diff renders, exit code 0, and `/permissions` shows
   `read roots  none` (besides Kitsu-managed runtime dirs).
2. In the same session shape, a model turn that calls `fs_read_file` on that file is still denied.
3. `--jsonl` scripted variant emits a paired `diff` command result whose payload contains the
   diff and no new grant events. Interactive variant must not raise a filesystem-grant prompt.

### Rank 2 — Give --allow-command an explicit cwd (backlog #2)

Impact: high for automation — the flag exists precisely for non-interactive scripts, yet its
implicit launch-directory cwd makes `--allow-command` + `--command-root <other-dir>` fail closed
at startup ("command cwd ... is outside the approved command roots"), so pregranted commands
cannot target the project root unless the script cd's there first. Effort: low/medium — accept an
object form (`{"argv":[...],"cwd":"..."}`) alongside the current array form, validate cwd against
command roots at startup, and thread it through `grant_command`/`reset_command_session`.

Acceptance test:
1. From /tmp/launch (outside the root), start
   `kitsu chat --no-session --command-root /tmp/proj --allow-command '{"argv":["pwd"],"cwd":"/tmp/proj"}' --script s.txt`.
   Startup must succeed; a turn in which the model runs argv `["pwd"]` with cwd `/tmp/proj`
   executes without prompting and reports `/tmp/proj`.
2. The same argv requested with cwd `/tmp/launch` is denied (exact-cwd grant, not argv-wide).
3. Legacy `--allow-command '["pwd"]'` still means launch cwd; when the launch dir is outside all
   command roots the startup error names the new object syntax as the fix.

### Rank 3 — Cross-run retention UX for command artifacts (backlog #3, remaining half)

Impact: medium, but effort is low and it has a privacy edge: retained command output (which can
embed secrets) accumulates silently in `~/.kitsu/profiles/*/artifacts/runtime-*` after every
stateless `--enable-commands` run, invisible to `/artifacts` in any later session and to any CLI.
Finishing this closes a cycle-5 item that is already half-built (quotas + in-session prune
exist). Effort: low — a `kitsu artifacts [list|prune]` subcommand over the profile's artifact
namespace, or startup GC of orphaned `runtime-*` generations with a one-line disclosure.

Acceptance test:
1. Run a stateless `--enable-commands` chat that retains one command output; exit. Verify a
   `runtime-*` dir with `command-output-0.txt` exists under the active profile's `artifacts/`.
2. `kitsu artifacts list` (no chat) shows that generation with id, age, and byte size;
   `kitsu artifacts prune all` removes it and prints what was reclaimed; the directory is empty
   afterward (in-flight generations of a live process are refused with a clear message).
3. If startup GC is chosen instead: the next `kitsu chat` prints a single disclosure line naming
   the pruned orphan count/bytes, and `/artifacts` agrees with the on-disk state.

### Not ranked this cycle
- Backlog #4 (searchable selection): models filtering already shipped; remaining session search is
  a convenience — add a `FILTER` arg to `sessions list`/`/sessions` when convenient.
- Backlog #5 (screen-reader Markdown mode): real need, narrower audience, and it deserves a
  dedicated a11y design pass (marker stripping + spoken structure), not a quick patch.

## D. Fixture inventory
- /tmp/cycle6-audit4/repo — git repo, one commit, dirty `lib.rs` (for /diff, review probes)
- /tmp/cycle6-audit4/{diff.txt,diff2.txt,m.txt,a.txt} — chat scripts used above
- No repository files were modified; all chat runs used `--no-session` or failed before commit.
