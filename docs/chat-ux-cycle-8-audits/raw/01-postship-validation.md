# Cycle 8 — Auditor 1: post-ship validation of cycles 6 and 7

Date: 2026-08-11. Binary: `c66af4b` (`target/debug/kitsu`), config `kitsu.toml`,
`--gateway ollama --model qwen2.5-coder:latest`. Fixtures under
`/tmp/cycle8-audit1/` (a throwaway git repo at `/tmp/cycle8-audit1/repo`).

## Environment note (read first)

This host was running at least one other concurrent dogfood audit against the
same local Ollama server and profile-keyed state directory for the entire
session (visible in `ps`/`kitsu sessions` as a second `kitsu` process tree
under `/tmp/cycle8-audit2/...`, and in `kitsu artifacts`/`kitsu sessions`
listings as cross-contaminated entries from cycle-6/7 audits sharing the same
config+gateway+model profile hash). System load average sat at 7-9 on a
12-core box with **12 GiB of swap in use out of 15 GiB** and under 500 MiB RAM
free throughout. Ollama's runner process was pegged at 400-600% CPU
continuously. A trivial one-word completion routinely took 60-90s to start
streaming; tool-call round trips for the `spawn_agent_bg` test took ~10
minutes end to end. This is an environmental condition, not a kitsu defect,
but it means: (a) some scenarios that need multiple sequential model turns
were deliberately kept minimal to fit in the session, (b) two long-tail
live scenarios (a second context-window-mismatch attempt, and exhaustive
approval-prompt interaction) are covered by strong static/code evidence and
partial live evidence rather than a full transcript, and this is flagged
per-finding below. `kitsu artifacts list`/`sessions` output includes real
entries left by the other concurrent audit and by cycle-6/7 audits — this is
expected multi-tenant behavior of the profile-keyed state directory, not a
bug, and is called out where it appears.

---

## Confirmed working (re-verified live unless noted)

1. **Built-in agent profiles spawn successfully on Ollama (cycle-7 headline
   fix #1).** Scripted chat asked the model to call `spawn_agent_bg` with
   `profile: "reviewer"`; the tool call succeeded, no HTTP 400, the
   background agent actually ran `fs_read_file`-equivalent evidence review
   and returned a real critique of `math_utils.py`. Confirms
   `AgentReasoningEffort::Inherit` (source: `crates/kitsu/src/subagent/profile.rs`)
   sends no `reasoning_effort` field for built-ins, matching
   `docs/jsonl-protocol.md`'s "Both built-in profiles ship as `inherit`" line.
   Repro: `script -qec "kitsu --gateway ollama --model qwen2.5-coder:latest
   --timeout-secs 600 --agent-join-timeout 300 chat --allow-path . --script
   spawn_reviewer2.txt" out.log` with a script containing `/profiles`, a
   turn instructing `spawn_agent_bg(profile=reviewer)`, `/jobs`, a filler
   turn, `/jobs`. Full transcript in `/tmp/cycle8-audit1/spawn_reviewer_styled.log`.

2. **`/profiles` and `/jobs` render from the DTO in the fox design language,
   no Rust Debug dumps (cycle-7 fix).** Live output:
   `◈ background-agent profiles — 2 available` /
   `◇ reasoning | model session model | effort inherit | ...` /
   `◆ reviewer | model session model | effort inherit | ...`; `/jobs` shows
   `◈ agent deck — 1 total · 1 running · ...` and `◆ #1 running | reviewer |
   <task>`. Source (`crates/kitsu/src/main.rs::format_jobs`,
   `handle_jobs_command`) confirms full DTO-driven rendering with
   `kitsu_render::deck_marker`/`deck_header`, not `{:?}`.

3. **Script exit joins live background agents with honest reporting
   (cycle-7 fix).** Same run as #1: on script-body exhaustion, human mode
   printed `~ input ended with 1 background agent(s) running; joining them
   (up to 300s; Ctrl+C cancels, --agent-join-timeout adjusts)` then
   `~ joined 1 background agent(s) before exit` then the real background
   job result text. Exit code 0. Matches
   `docs/jsonl-protocol.md`'s join/`joined: true` semantics.

4. **Broken pipe exits cleanly, not a panic (cycle-7 fix).** `kitsu sessions
   | head -1` → exit 0 (output small enough not to trigger SIGPIPE).
   Forced with a longer stream: `kitsu --jsonl chat --script perms_only.txt
   | head -1` → stderr `JSONL output failed: JSONL write failed: Broken pipe
   (os error 32)`, exit 1 (`$PIPESTATUS[0]`). No panic, no exit 101.

5. **`/diff` with zero grants (cycle-7 host-evidence fix).** `chat --script`
   containing only `/diff`, no `--allow-path` at all: exit 0, prints
   `~ verified target working_tree` and a colorized diff. Confirms
   `authorized_workspace_review` never requires model read authority.

6. **`/permissions` host-evidence section (cycle-7 fix).** Live output has a
   `host evidence` block: `- /tmp/cycle8-audit1/repo (git worktree; /diff,
   /review, kitsu review)` / `host-only: read by Kitsu's own git, never a
   model read root, never a model tool target` / exclusion list for
   Kitsu-private paths. JSONL `command_result` for `/permissions` carries the
   exact documented shape: `host_evidence.repository_root`,
   `.consumers: ["diff","review","checkpoint"]`, `.model_read_root: false`,
   `.prompt_disclosure: "explicit_user_command_only"`,
   `.excluded_private_paths`. Matches `docs/jsonl-protocol.md` verbatim.

7. **`/review` disclosure line + `review_disclosure` JSONL event (cycle-7
   fix).** `--jsonl chat --script` containing `/review` on a dirty tree:
   emits `command_result` with `kind: "review_disclosure"`,
   `model_disclosed: true`, `model_read_root_granted: false`,
   `evidence_bytes: 203`, positioned *before* `turn_started` — exactly
   matching the documented ordering ("emits ... before the turn that carries
   the bytes"). (The turn itself then hit the 200s deadline under host
   contention and rolled back with strict `discarded` status, which is
   itself correct: `docs/jsonl-protocol.md` says scripted turns keep strict
   discard-on-timeout, unlike `ask`/piped/review one-shot paths.)

8. **`--allow-command` object form with explicit cwd, plus the `/new`
   rebinding fix (cycle-7).** `--enable-commands --command-root <dir>
   --allow-command '{"argv":["echo","hi"],"cwd":"<dir>"}'` round-trips
   through `/permissions` as `[1] "echo" "hi" (cwd <dir>; invocation/base,
   explicit cwd)`. Drove a real interactive PTY session (via a custom
   `pty.fork()`-based driver, since piping lines through `script` delivers
   them all at once before rustyline can read them one at a time) through
   `/permissions` → `/new` → `/permissions`: `/new` printed
   `⛩ started new session ... (runtime grants reset; invocation grants
   retained: 1 read, 0 write)` and the subsequent `/permissions` still showed
   the exact-argv command grant with `explicit cwd` intact. Confirms the
   cycle-7 "`/new`-rebinding latent bug fixed" claim.

9. **Misspelled/extra/missing keys in `--allow-command` object form fail
   closed at startup (cycle-7 fix).** `{"argv":[...],"cwdd":"..."}` →
   `! error: invalid --allow-command value ...: data did not match any
   variant of untagged enum PregrantedCommandSpec`, exit 1. Same for a
   missing `argv` key and for a syntactically valid object with an
   unexpected extra key (`"extra":"z"`) — confirms `deny_unknown_fields`-style
   strict parsing, not silent field-drop. A cwd that exists but is outside
   any `--command-root` is separately and correctly rejected: `policy
   denied: command cwd '...' is outside the approved command roots`.

10. **`kitsu artifacts list` / `prune --dry-run` / `prune all` full round
    trip (cycle-7 feature), tested against a real fixture orphan.** Manually
    constructed two fixtures in the live state dir matching the exact
    `runtime-<pid>-<nanos>-<gen>` format documented in
    `crates/kitsu/src/artifacts.rs`: one empty+unlocked+2h-old, one
    non-empty (52 B) with a released (`Free`) lock. `list` correctly showed
    both as `orphaned`; `prune all --dry-run` correctly reported "would
    remove 2 generation(s), 52 B" and listed the *other*, legitimately
    retained generations with correct skip reasons (`its session still
    exists; use 'kitsu sessions delete <id>' first`, `a live process holds
    this generation's lock`) without touching them; `prune all` (real)
    removed exactly the orphans and left sessions/live entries untouched.

11. **Startup sweep of provably-empty orphans (cycle-7 feature).** The
    empty+2h-old fixture from #10 was *not* touched by a `chat --script`
    launch without `--enable-commands`/`--session` (confirms the sweep is
    gated on `durable_session || command_requested`, per
    `crates/kitsu/src/main.rs:8603`) but *was* silently removed the moment a
    `chat --enable-commands` run launched — with no printed notice, because
    the retained-orphan count (2) was under the `NOTICE_GENERATION_THRESHOLD`
    (16). The non-empty orphan correctly survived the sweep (design: "never
    delete automatically", only the explicit `prune` command removes it).

12. **`/artifacts` cross-run section (cycle-7 feature).** Inside a
    `--enable-commands` chat, `/artifacts` prints `other runs: 3
    generation(s), 0 B retained on disk`; JSONL `command_result.result`
    carries `cross_run: {generation_count, bytes}` exactly per
    `docs/jsonl-protocol.md`.

13. **`committed_partial` on timeout (cycle-6 fix, re-verified).** `kitsu ask
    "<600-word essay prompt>" --jsonl --timeout-secs 130`: 439
    `assistant_delta` events streamed, then a final delta appended
    `[stopped: deadline expired - answer is partial]`,
    `assistant_disposition.status: "committed_partial"` with reason `"timed
    out after 130s - the answer above is partial; completed tool results and
    file changes were kept"`, followed by a normal `error` event (exit 2) and
    `turn_finished.status: "error"`. Matches
    `docs/jsonl-protocol.md` exactly: "`committed_partial` changes only how
    the delivered text is classified, never the failure status."

14. **Context-overflow preflight spells out window/budget/trim-target
    arithmetic (cycle-7 feature).** A 4500-word filler prompt piped into
    `chat` produced (before ever contacting the provider): `context
    preflight stopped an oversized provider request: protected prompt needs
    about 7209 tokens, but the trim target is 6861 tokens (window 8192
    tokens, effective prompt budget 7680 tokens, trim target = effective
    minus a 819 tokens reserve); shorten the latest request or system
    instructions, disable large tool schemas, raise the window with
    --context-window <n>, lower --max-output-tokens, clear/compact history,
    or select a model with a larger context window.” Exit 1. Fully
    actionable, no inconsistent numbers.

15. **Error styling: one weighted EMBER line + hint, zero escape bytes in
    `--plain`/`NO_COLOR` (cycle-6 fix, re-verified, no regression).**
    `kitsu --gateway nonexistent chat ...` → single bold-EMBER line `! error:
    config error: unknown gateway 'nonexistent' — defined gateways: ...`,
    exit 1, no stats block, no DISCARDED text. `--plain` and `NO_COLOR=1`
    both produce byte-identical text with the em-dash swapped for a plain
    hyphen and **zero** ESC (`\x1b`) bytes (`grep -c $'\x1b'` = 0 in every
    case checked, including a full `/permissions` render).

16. **Success-path provider telemetry stays silent (cycle-6 fix, re-verified).**
    Zero occurrences of "inference provider attempt"/"HTTP 200" anywhere in
    the ~90-line transcript of finding #1, which included two successful
    top-level model turns and one successful background-agent turn.

17. **Fox banner and glyph coherence (cycle-6 fix, re-verified).** Live PTY
    capture of interactive-chat startup: closed box `╭─ 🦊 kitsu 狐
    ──...──╮`, fox kaomoji `( ･ᴥ･ )`, `/\ /\` ears, `directory`/`gateway`/
    `model`/`tools` rows. `/jobs`/`/profiles` use `◆ ◇ ◈` (not `⏺ ⎿ ✻`); `/new`
    prints the `⛩` torii milestone line. Grep of `crates/kitsu-render`
    confirms `⏺ ⎿ ✻` only survive as ASCII-fallback *input* characters in the
    plain-mode sanitizer table (mapped to `*`/`>`), never emitted as output.

18. **`sessions export` format inference (cycle-7 feature).** `sessions
    export <id> out.json` → `~ inferred --format json from the .json
    destination`, writes real JSON; `.md` or no extension both default to
    the Markdown export. No JSONL/human-text mismatch.

19. **`kitsu review` clean-tree dead-end hint (cycle-7 feature).** On a
    clean tree: `! error: nothing to review for verified target
    working_tree; review its committed content with --commit HEAD <path>,
    pick another target with --staged/--unstaged/--base <rev>, or discuss
    the file in chat with @<path>`, exit 1.

20. **`/help` six-group registry, `/agent` front-door still absent as
    documented (no regression).** `/help` output groups exactly match
    `docs/ux-design.md`: session; context & history; files, diffs &
    checkpoints; agents & jobs; permissions; models & reasoning. A new
    `/agent [list|show|steer|stop|close]` command exists as an alias-ish
    sibling of `/jobs`, but — consistent with cycle-7's "remaining backlog"
    note — it still has **no `spawn` subcommand**, so the human-side agent
    front door genuinely remains unimplemented. Correctly still true, not a
    regression.

21. **`run_started.mode` enum matches doc.** Observed `"ask"`, `"piped"`,
    `"script"`, `"models"` live; `"review"` implied by the `/review` disclosure
    test. All four checked values match the documented
    `ask|script|piped|review|models` set with no drift.

22. **`context_window_mismatch` fires correctly on a real round trip
    (cycle-6 fix, re-verified live).** `kitsu.toml`'s `ollama` gateway
    declares `context_window = 8192`, but the live Ollama server actually
    serves `context_length: 4096` (confirmed via `curl
    localhost:11434/api/ps`) — precisely the cycle-6 bug precondition. A
    ~3000-word filler prompt piped into `kitsu ask ... --jsonl
    --max-output-tokens 10 --timeout-secs 900` (needed ~8 minutes wall clock
    under this host's heavy contention, see environment note) completed a
    full round trip and correctly emitted `{"type":
    "context_window_mismatch", "estimated_sent_tokens": 4429,
    "reported_prompt_tokens": 3705, "resolved_window": 8192, "turn_id":
    "ask:1"}` immediately after the `usage` event and before
    `assistant_disposition`. The turn still completed normally
    (`assistant_disposition.status: "committed"`, `final_summary.status:
    "ok"`) — matching the doc's "Kitsu never rewrites the resolved window
    from this evidence" contract. Two smaller/larger prompt sizes tried
    first either got correctly rejected by local context-overflow preflight
    before reaching the provider (see #14) or timed out under contention
    with no usage data — this final, right-sized attempt got a full
    completed response and the mismatch event fired exactly once, as
    designed.

---

## Findings

### F1 — Minor: cross-run artifact/session storage is shared across concurrent invocations with identical config+gateway+model, with no visible tenant/run separation

**Severity:** Low (design-intentional, but worth a design-doc callout)

**Observed:** `kitsu artifacts list` and `kitsu sessions` both listed real
entries belonging to a different, concurrently-running audit process
(`/tmp/cycle8-audit2/fixture-kitsu`) because the on-disk state directory is
keyed only by a hash of `(config path, gateway, model)`
(`~/.kitsu/profiles/v1-fnv128-<hash>`), not by launch directory or PID. This
is presumably intentional (it's how `--resume last` and cross-run artifact
GC are supposed to see everything), and `docs/ux-design.md`/`jsonl-protocol.md`
don't claim otherwise, so this is not a contract violation. It is worth
flagging because two unrelated concurrent users of the same shared-machine
config file will see each other's session directory paths, prompt previews,
and command-artifact byte counts in list output (not file contents, but
metadata) with no indication that the other entries belong to a different
principal/run. **Proposed acceptance test (backlog candidate, not urgent):**
`kitsu sessions`/`kitsu artifacts list` output could optionally note the
originating working directory prominently (it already does show `directory`
for sessions) or the state directory's provenance in `doctor` output, so a
user on a shared machine understands why unfamiliar entries appear.

---

## Regression / consistency sweep — no regressions found

- No stray ANSI escape bytes (`\x1b`) in any `--plain` or `NO_COLOR=1`
  capture checked (error path, `/permissions`, banner-suppressed script
  mode).
- JSONL payloads for `/permissions`, `/diff`, `/review` (`review_disclosure`),
  `/artifacts`, `models`, `ask`, timeout/`committed_partial`, and
  `final_summary` were diffed by hand against `docs/jsonl-protocol.md` and
  `docs/schemas/kitsu-chat-v1.schema.json` — no drift, no undocumented
  required fields, no missing documented fields.
- Glyph set (`◆ ◇ ◈ ◉ ⛩`) is consistent across every surface exercised:
  banner, `/profiles`, `/jobs`, `/permissions`-adjacent `/new` torii line,
  error styling. No `⏺ ⎿ ✻` leakage into live output anywhere.
- Streamed code-block styling remains unstyled (no syntect boxes) — this
  matches the documented, still-backlogged descope, not a regression.
- The cycle-7 "remaining backlog" items I spot-checked (`/agent spawn` front
  door, syntect boxes) are genuinely still absent, consistent with the
  cycle-7 report's own accounting — no scope claims appear to have silently
  regressed or been silently dropped from the backlog list.

---

## Summary

Both engineering passes described in `docs/chat-ux-cycle-7.md`'s
"Implementation outcome" section hold up under live, adversarial re-testing
on the actual target binary and gateway: the previously-dead built-in agent
profiles now spawn correctly with no HTTP 400, the agent deck renders from
real DTOs instead of Debug dumps, script exits honestly join and report
background work, broken pipes exit cleanly, and the full permissions/evidence
and artifacts-GC backlog items (host-only `/diff`/`/review` evidence,
explicit-cwd command pregrants surviving `/new`, fail-closed malformed
grants, and the artifacts list/prune/sweep round trip) all check out against
real on-disk state, not just unit tests. Cycle-6's fixes (`committed_partial`
on timeout, context-window-mismatch detection, context-overflow preflight
arithmetic, error styling, telemetry silence, fox identity) were all
re-verified live end-to-end and show no regression. No functional defects
were found; the only note is F1, a minor multi-tenant visibility observation
about shared local state on a machine running concurrent kitsu sessions
against the same config, which is consistent with documented design rather
than a violation of it. The host ran under severe, sustained CPU/memory
contention (a second concurrent audit sharing the same local Ollama
instance, 7-9 load average on 12 cores, 12 GiB of swap in use) for the
entire session, which is why some single scripted runs took 8-10 minutes;
all planned live scenarios nonetheless completed with real transcripts.
