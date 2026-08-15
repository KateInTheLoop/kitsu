# Chat UX dogfood — cycle 20 (claude CLI baseline; UI & functionality focus)

Date: 2026-08-12

Baseline: `b70bffc` on `dogfood/codex-ux-cycle-6`. Cycle 20 ran the
rotation's Sonnet slot: ship-first on the honest-receipts trio, verified
live by a Sonnet auditor that also ran a fresh comparison genre
(doc-comment accuracy review).

## Shipped this cycle (auditor verdicts inline)

**Un-truncated error receipts (cycle-19 #3).** The `◇ error` line keeps the
whole detail (newlines flattened) up to 360 chars with an explicit `…` —
remedy clauses stop being eaten at 180 chars. CONFIRMED (226-char remedy
rendered whole, no false ellipsis).

**Run-summary trouble counts (cycle-19 #5).** `RunTallies` (reads / denied /
failed) generalizes the shared read counter — one Arc spanning parent and
subagents — and the receipt renders `, N denied tool call(s), M failed tool
call(s)` when non-zero. CODE CONFIRMED (script mode shows
`…, 1 denied tool call(s), session not saved`), but UNREACHABLE in
piped single-turn mode: the strict-failure branch returns before any
receipt prints — promoted to cycle-21 #1.

**Impossible pregrants fail closed at startup (cycle-19 #2).** A path-form
`--allow-command` executable outside every command/runtime root errors
immediately, naming the exact flag to fix; system binaries under the
implicit `/usr`,`/bin`,`/lib`,… containment mounts pass. CONFIRMED both
ways. (Implementation note: the first version missed the implicit system
mounts and broke six CLI tests — caught before commit.)

Gates: fmt clean, clippy clean, workspace green (233 bin / 121 CLI with the
known PTY-environmental failure unchanged / 92 core / 139 tools).

## Comparison (doc-comment accuracy review, change-nothing task)

claude 40.8 s: precise line-cited findings including one uncontestable real
bug (`MIN_OUTPUT_RESERVE` window/2 clamp). kitsu + HF Llama-70B 12.7 s:
found the same real bug shallowly, plus one outright-wrong claim; receipts
honest (0 written, 3 reads — the same file read 3× through an unnecessary
subagent hop).

## Ranked gaps (queued for cycle 21)

1. **Piped strict-failure path prints no run summary at all** — the denial
   branch returns Err before emit_run_receipt; receipts must render on
   failure exits too (fold into the denial-disposition fix: a policy-denied
   turn still discards a truthful final answer).
2. **Redundant delegation**: trivial single-file tasks get a subagent hop
   that triples reads and tokens — prompt-level guidance or a
   single-file heuristic in the spawn tools.
3. Carried: capped-read true-size disclosure, worktree gitdir binds, Llama
   python_tag parsing, G5 elision, credential-filter redesign, resume
   picker, M1 nudge.
4. Model-quality note (not tooling): Llama-70B produces confident wrong
   critiques on close-reading tasks — grading rubric point for future
   comparisons, not a kitsu fix.

## Report index

1. [Trouble-receipts verification + comparison](chat-ux-cycle-20-audits/raw/01-trouble-receipts.md)

## Loop state

2-hour local cron loop live. Rotation: cycle 20 = Sonnet (this file), cycle
21 = Haiku. Cycle 21 starts with the failure-path receipt (gap #1) — the
denial-disposition pair has now been promoted twice and blocks receipt
truthfulness exactly where it matters most.
