# Cycle 6 — UX Audit 2: Real-task dogfooding (scripted/piped surface)

Auditor: UX auditor 2 · Date: 2026-08-10
Binary: `/home/mmpg/workspace/kitsu/kitsu2/kitsu/target/debug/kitsu` (invoked with `--config /home/mmpg/workspace/kitsu/kitsu2/kitsu/kitsu.toml --gateway ollama --model qwen2.5-coder:latest`)
Fixtures: `/tmp/cycle6-audit2/` (docs-fixture: 350-line README in a git repo; bugfix: Python statlib with a failing `test_median_even`; scripts/*.script)
Environment note: Ollama runs 100% CPU here; cold model load ~3 min, generation ~2–4 tok/s. All timings below include that handicap. `ollama ps` reports the serving context as **4096** while kitsu's shipped config sets `context_window = 8192` — this mismatch matters (F3).

Model-fault ledger (NOT kitsu's fault, excluded from ranking): qwen2.5-coder:7B wrapped one answer in an invented `{"response":{"type":"Markdown"...}}` JSON envelope (no such envelope exists in kitsu's source); twice emitted an edit whose `new_text` dropped the `return` keyword; once gave up after a single edit conflict and asked the user to paste the file. kitsu's edit-precondition tool, verified diff, and text-tool-call recovery all behaved correctly around these model failures.

---

## Friction points

### F1 — Total deadline kills an actively streaming turn; stdout keeps the partial text but kitsu claims it was "DISCARDED" and exits 2
(a) Repro:
```
cd /tmp/cycle6-audit2/docs-fixture
kitsu --config .../kitsu.toml --gateway ollama --model qwen2.5-coder:latest --timeout-secs 360 \
  review --commit HEAD README.md --instructions "Give a structured documentation review..." \
  > out.txt 2> err.txt; echo $?
```
(b) Observed: the model streamed ~90% of a good review to stdout over 6 minutes, then hit the 360s total deadline mid-body. stderr: `attempt 1/3: timeout; HTTP 200; body failed`, then `provisional assistant output, if any, was DISCARDED and was not committed`. Exit 2. `out.txt` contains the near-complete review, truncated mid-sentence, with no truncation marker in the stream.
Expected: a stream that is actively producing tokens should not be killed by a wall-clock total; an idle/stall timeout (no token for N s) is the right guard. Failing that: (1) don't print "DISCARDED" when the bytes were already delivered to a pipe — say "partial output above is incomplete and was not saved"; (2) append a visible `[truncated: deadline]` marker to stdout so downstream consumers can detect it.
Related: the default `--timeout-secs 120` failed both cold-load runs; the `--help` text itself admits local models "routinely exceed even this default". A default the help calls insufficient for a first-class supported backend (Ollama) is a bad default. The retry hint helpfully escalates (360 → 1080) but each retry re-pays minutes of inference.
(c) Severity: **High** — wastes the longest, most expensive runs exactly when they were succeeding; misleading state report; exit code contradicts visible output.
(d) Acceptance test: mock a gateway that streams 1 token per 2s indefinitely; run with `--timeout-secs 10`. Assert (i) turn is not aborted while inter-token gap < stall threshold, or if the total-deadline design is kept, (ii) stdout ends with an explicit truncation marker and stderr does not use the word "DISCARDED" for bytes already emitted.

### F2 — Strict scripted mode: a *recovered* tool error fails the whole turn; disk keeps the edit, session/checkpoint are rolled back, `/undo` becomes unreachable
(a) Repro:
```
cd /tmp/cycle6-audit2/bugfix   # buggy median, ambiguous old_text likely
kitsu ... --timeout-secs 600 --max-rounds 20 chat --script scripts/bugfix.script --session \
  --allow-read-write /tmp/cycle6-audit2/bugfix; echo $?
```
(b) Observed: round 1 Edit failed (`edit conflict: old_text matched 2 times`), round 2 Edit succeeded (`updated · 488 bytes`), verified checkpoint #1 was printed with "`/undo 1` restores this checkpoint". Then: `strict scripted turn failed: 1 tool call(s) failed during the turn`, conversation rolled back, assistant message discarded, **no session saved** (`kitsu sessions` shows nothing for the run), exit 1 — while `git diff` confirms the file WAS modified on disk ("failed turn rolled back; completed file changes, if any, remain on disk").
Expected: an edit-conflict that the model immediately self-corrects is the normal agentic retry pattern (kitsu's own precondition design *induces* it); it should not fail the turn. Turn failure should hinge on the final state, or at minimum policy denials only. And whenever disk changes survive, the checkpoint must survive too — the current combination advertises `/undo 1` in the same breath as making it permanently unreachable (no session persisted, no later process can undo).
Corollary trap: `--allow-turn-errors` flips to exit **0 even when every turn and command failed** ("script summary: 2 failure(s)", exit 0) — the two modes bracket the useful behavior without providing it.
(c) Severity: **High** — scripted CI-style use gets: failure exit for successful work, orphaned disk mutations, dead undo path, lost session.
(d) Acceptance tests: (i) scripted turn where tool call N fails and N+1 succeeds → exit 0, session saved, checkpoint resumable, `chat --resume last` then `/undo 1` restores the file; (ii) if any file change survives a rolled-back turn, a checkpoint referencing it must be reachable from some persisted session; (iii) `--allow-turn-errors` with 100% failures → nonzero exit or a distinct "all turns failed" code.

### F3 — Silent context truncation: kitsu budgets against configured 8192 while Ollama serves 4096; usage line proves truncation but no warning is raised
(a) Repro:
```
cd /tmp/cycle6-audit2/docs-fixture
echo 'Review @README.md ...' | kitsu ... --timeout-secs 600 chat --allow-path /tmp/cycle6-audit2/docs-fixture
# stderr: kitsu-usage {"prompt_tokens":4096,...}  kitsu-context {"used":4299,"effective":7680,"window":8192}
# ollama ps → CONTEXT 4096
```
(b) Observed: kitsu computed `used: 4299` and sent the full prompt; Ollama reported `prompt_tokens: 4096` — exactly its num_ctx — i.e., the head of the attached README was silently dropped before the model saw it. The review answer was generic partly because the model never saw the whole file. kitsu printed both numbers on adjacent stderr lines and drew no conclusion.
Expected: kitsu already has both facts in hand. When provider-reported `prompt_tokens` < tokens kitsu believes it sent (especially when it equals a round power-of-two), emit a loud warning: "provider accepted only 4096 of ~4299 prompt tokens — your context_window setting likely exceeds the server's num_ctx; input was truncated". Bonus: for the Ollama flavor, query the real serving window (`/api/show` / `ollama ps` equivalent) instead of trusting config, or at least at `doctor` time.
(c) Severity: **High** — silent quality degradation; the shipped config's own comment block warns about exactly this Ollama behavior yet ships 8192 unverified.
(d) Acceptance test: mock gateway that reports `usage.prompt_tokens` = min(sent, 4096); send a 5000-token prompt with `context_window=8192`; assert a truncation warning appears on stderr (and in `--jsonl` as a typed event).

### F4 — `kitsu review` cannot review a file as-is, and the empty-target error doesn't teach the escape hatch
(a) Repro:
```
cd /tmp/cycle6-audit2/docs-fixture   # README.md committed, tree clean
kitsu ... review README.md; echo $?   # → "nothing to review for verified target working_tree", exit 1
```
(b) Observed: review is diff-only (working tree/staged/base/commit). For the bread-and-butter task "review this document/file", a clean tree yields `nothing to review for verified target working_tree` with no hint that `--commit HEAD <path>`, `--base`, or `--staged` exist, nor that chat-with-`@file` is the intended path for content (not diff) review. I only found `--commit HEAD` by reading `--help`.
Expected: either support content review (`review --file README.md` or auto-fallback "path is tracked and clean — reviewing its committed content"), or make the error say: "the working tree has no changes for README.md; use --commit <rev> to review its committed content, --staged/--base for other targets, or attach it in chat with @README.md".
(c) Severity: **Medium** — first-run dead end on a common task; recovery exists but is undiscoverable from the failure.
(d) Acceptance test: in a repo with a clean tracked file, `review <path>` exit-1 message must name at least `--commit` and the `@file` chat alternative (string assertion).

### F5 — Session export ignores the destination's extension: markdown written into `.json` with no warning
(a) Repro:
```
kitsu ... sessions export last /tmp/cycle6-audit2/session-export.json
head -1 /tmp/cycle6-audit2/session-export.json   # → "# Kitsu session export"
```
(b) Observed: default `--format markdown` silently produces a Markdown document at a `.json` path (819 bytes starting `# Kitsu session export`). Any tool consuming the file by extension breaks. `--format json` exists but nothing connects the dots at run time.
Expected: infer format from a `.json` extension, or warn: "writing markdown to a .json destination; pass --format json for the JSON snapshot".
(c) Severity: **Low-Medium** — data is intact, but the automation-facing surface hands you a mislabeled file.
(d) Acceptance test: `sessions export last out.json` without `--format` must either produce valid JSON or print an extension-mismatch warning.

### F6 (minor) — Context preflight numbers are three-way inconsistent and don't name the fix flag
(a) Repro: `kitsu ... ask "$(cat /tmp/cycle6-audit2/huge-prompt.txt)"` (12k-word prompt).
(b) Observed: exit 1 before contacting the provider (good) with: needs "about 27227 tokens", "configured target is 6861", while the adjacent `kitsu-context` line says `effective:7680, window:8192`. Three different budget numbers, none explained; the remedy list is good but omits the most direct lever it documents elsewhere, `--context-window`.
(c) Severity: Low. (d) Acceptance test: preflight error mentions `--context-window` and uses the same "effective" figure as `kitsu-context`.

### F7 (minor) — `sessions list` "last" and fork interplay
`sessions fork last` then `sessions export last` silently exported the *fork* (it became most-recent). Defensible, but a one-line "last = s-…(fork of s-…)" annotation would prevent wrong-target exports. Severity: Low.

---

## Top 5 ranking
1. **F2** — strict-turn rollback vs. surviving disk edits vs. unreachable `/undo` (integrity story contradicts itself in scripted mode).
2. **F1** — total deadline aborts actively-streaming output; "DISCARDED" claim contradicts delivered stdout; default 120s indefensible for local gateways.
3. **F3** — silent prompt truncation despite kitsu holding the evidence (usage vs. context lines) on adjacent stderr lines.
4. **F4** — `review` dead-ends on clean files without teaching `--commit`/`@file`.
5. **F5** — export format/extension mismatch (with F6/F7 as adjacent paper cuts).

## Three things that worked notably well
1. **Write verification & undo pipeline**: exact-precondition edits (`old_text matched 2 times ... file was not modified`), snapshot-derived unified diff with byte counts and first-changed-line, numbered checkpoints, and a `/undo 1` that verifiably restored the file — best-in-class transparency for model-driven edits (when the turn commits; see F2).
2. **stream/stderr discipline for automation**: clean answer text on stdout; heartbeats ("still waiting on the model (10s)…"), attachment summaries ("attached 1 file(s): … (full file)"), `kitsu-usage`/`kitsu-context` JSON lines, and provider-attempt telemetry all on stderr; `--jsonl` emits a versioned `kitsu.chat.v1` event stream with run/turn ids. Piped consumers get exactly what they need.
3. **Actionable, differentiated errors**: unknown gateway lists the defined gateways; 404 model points at `kitsu models` / `/model`; missing config names the exact path; context preflight fails *before* burning provider time; `sessions resume` outside a TTY prints the exact `cd … && kitsu … --resume <id>` argv; config errors exit 1 vs runtime errors exit 2. Session continuity itself (scripted `--session` → list → show → piped `--resume last` remembering gateway/model → fork with "permissions were not copied") was coherent end-to-end.
