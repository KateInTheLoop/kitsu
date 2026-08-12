# Cycle 18 — changelog-from-git task (command-path genre), claude CLI vs kitsu+HF Llama-3.3-70B

Date: 2026-08-12. Baseline `27abab6` on `dogfood/codex-ux-cycle-6` (worktree
`agent-ad80355fcb0c560e4`, synced from shared HEAD, left clean). Binary: fresh
`target/debug/kitsu` (Aug 12 14:38) with shared read-counter aggregation +
capped-read receipts. Known gaps from `docs/chat-ux-cycle-17.md` excluded;
NEW findings and regressions only.

## Task

"Write docs/CHANGELOG-draft.md summarizing the last 3 commits
(git log --oneline -3 plus commit messages) as three user-facing bullet
points." Ground truth: `27abab6` (read counter + ranged-read disclosure),
`a01bd14` (pressure-gated offload + disclosing receipts), `9b75247`
(skill-confusion recovery + filter-safe HF hint).

## Commands (HF token redacted as ***)

- Run A: `time claude -p '<task>' --permission-mode acceptEdits`
- Run B: `time printf '%s' '<task with pre-approved git log>' | HF_TOKEN=*** kitsu --config kitsu.toml --gateway huggingface --max-rounds 25 --timeout-secs 420 --max-output-tokens 3000 --yes chat --allow-read-write . --enable-commands --allow-command '["git","log","--oneline","-3"]'`
- Run B2: same, prompt spells out the absolute worktree cwd.
- Run B3: same, cwd = scratch git repo (3 commits with identical subjects)
  because git cannot run at all inside a linked worktree under the sandbox.

Logs: `cycle18-runA-claude.log`, `cycle18-runB-kitsu.log`,
`cycle18-runB2-kitsu.log`, `cycle18-runB3-kitsu.log` (this directory);
Run A artifact archived as `cycle18-runA-changelog.md`.

## Results

| Run | Wall time | Outcome | Accuracy |
|-----|-----------|---------|----------|
| A (claude) | 26.1 s | wrote 3-bullet draft, one pass | **A** — all 3 commits, genuinely user-facing rewrites, internal bookkeeping filtered out |
| B (kitsu) | 2.9 s | **FAILED** — PolicyDenied on the pre-approved command; turn discarded; nothing written; nonzero exit | **F** |
| B2 (kitsu, absolute cwd) | 1.7 s | command ran, git exit 128 (worktree gitdir invisible in sandbox); model narrated a next step without calling a tool; run ended "1 turn(s) ok", no file | **F** (incomplete; summary says "ok") |
| B3 (kitsu, scratch repo) | 5.4 s | command exit 0 → chained Write in the same turn; verified checkpoint diff; truthful summary | **C-** — three bullets are the commit subjects verbatim, not user-facing summaries; no trailing newline |

## Per-dimension notes

**Command receipt quality (B2/B3): good.** `◆ Command(argv=...; cwd=...)`
then `◇ exit 0 | 6 ms | stdout 269 B tail="..." | full output="<absolute
artifact path>"`. Exit code, duration, size, tail, and an **un-elided**
artifact path — the G5 concern does not reproduce on command receipts.
Failure case equally honest: `exit 128 | stderr 108 B tail="fatal: not a git
repository: ..."`.

**Command→write chaining (B3): works.** Llama chained
command→fs_write in a single turn, 5.4 s end-to-end, and the verified
checkpoint diff printed the exact file content.

**Run-summary truthfulness incl. NEW read counter: correct.** B2:
`1 turn(s) ok, 0 file(s) read, 0 file(s) written`. B3: `... 0 file(s) read,
1 file(s) written (1 change(s))` — honest zero (the source was command
stdout, not a file read), so the write-without-read signature stays
legible. Command output correctly does not count as a "read".

**Run B failure anatomy (the headline).** Kitsu's own startup banner
advertises the workspace tilde-elided (`~ loaded project context from
~/workspace/...`). Llama echoed that form into the required `cwd` parameter;
`resolve_cwd` (crates/kitsu-tools/src/builtins/command.rs, ~line 1578) calls
`std::fs::canonicalize` with **no tilde expansion** → ENOENT → PolicyDenied
for a command the user explicitly pre-approved. The model's retry came out
as raw `<|python_tag|>fs_list_dir()` text (unparsed Llama tool-call syntax),
and the run then hard-failed with "provisional assistant output was
DISCARDED ... 1 tool call(s) were denied by local policy during the turn".
One spurious denial = total run loss in 2.9 s, despite `--max-rounds 25`.

**Run B2 failure anatomy.** With an absolute cwd the sandbox itself is the
blocker: bubblewrap binds only the approved command roots, and a linked
worktree's `.git` file points at the parent repo's
`.git/worktrees/<id>` outside the binds → `fatal: not a git repository`.
Any git command in any linked worktree is dead on arrival — a mainstream
setup for exactly the users who enable the command tool.

## Regression sweep (from the B logs)

- **Window false alarm:** absent. Only `kitsu-context` telemetry lines
  mention "window"; used 1.2k–1.7k of 128k effective, no pressure messages.
- **Ranged-read disclosures:** not exercised — zero `fs_read_file` calls in
  all three B runs; no spurious "(ranged read ...)" text appeared.
- **Capped-read suffix:** correctly absent (no reads, cap never hit; no
  stray suffix anywhere in the logs).
- **Subagent read aggregation (new in this binary):** not exercised (no
  subagents spawned); parent counters were honest for direct reads (0).

## Model-fault ledger (Llama-3.3-70B, not kitsu)

1. Echoed the tilde path into `cwd` (shared fault: kitsu advertised it).
2. Emitted `<|python_tag|>fs_list_dir()` as raw text after the denial
   instead of a well-formed tool call.
3. B2: narrated "Let's try to list the directory" and stopped without
   calling any tool — run ended incomplete but labeled "1 turn(s) ok".
4. B3: ignored "user-facing" — pasted commit subjects verbatim as the
   bullets; also wrote no trailing newline.

## Ranked top-5 NEW gaps (one-line fixes)

1. **Tilde cwd self-sabotage** — kitsu advertises `~/...` paths, then
   `resolve_cwd` rejects them (ENOENT → PolicyDenied of a pre-approved
   command). Fix: expand a leading `~/` in `resolve_cwd` (and/or present
   absolute paths to the model; make `cwd` optional, defaulting to the
   first command root).
2. **Sandbox breaks git in linked worktrees** — gitdir lives outside the
   bind roots, every git command exits 128. Fix: when cwd/.git is a file,
   parse its `gitdir:` pointer and bind that directory (plus `commondir`)
   read-only into the sandbox.
3. **One spurious PolicyDenied kills the whole run** — provisional output
   discarded and hard error despite 24 unused rounds. Fix: treat
   environment-shaped denials (ENOENT cwd) as recoverable receipts that
   feed back to the model; reserve turn-discard for genuine containment
   denials.
4. **Denial receipt gives no path forward** — it echoes the bad cwd but
   never names the approved command roots. Fix: append "approved roots:
   <list>" to the cwd PolicyDenied message.
5. **Llama `python_tag` tool-call syntax unparsed** — becomes raw
   assistant text and wastes a round. Fix: parse `<|python_tag|>name(...)`
   in the HF adapter (or issue the same corrective receipt the
   skill-confusion recovery uses).

Honorable mention: B2's `1 turn(s) ok` summary for a run that completed no
part of the task reads as success; consider counting turns that end in
narrated-intent-without-tool-call distinctly.

## Verdict

The receipts stack (command receipts, checkpoint diffs, read counter) is in
the best shape it has been — every number printed in B2/B3 was true. But
the command path itself fails closed twice before a user in a normal git
worktree can run the single most likely command: once on kitsu's own
tilde-elided path, once on the sandbox/gitdir interaction. claude CLI won
this genre 26 s vs never-completes-in-situ; kitsu's 5.4 s B3 win only
materializes in a plain (non-worktree) repo with an absolute cwd.

Worktree left clean (verified `git status` empty after each run).
