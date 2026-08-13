# Chat UX dogfood — cycle 27 (claude CLI baseline; UI & functionality focus)

Date: 2026-08-13

Baseline: `00bb7f9` on `dogfood/codex-ux-cycle-6`. Cycle 27 ran the
rotation's Opus slot.

## Shipped this cycle

**Per-model cost attribution (cycle-25 gap, cross-gateway flavor).**
RunTallies bins spend per model id; `/cost` lists the bins whenever more
than one model spent in the session. Audit-verified exact on HF:
Llama-3.3-70B 56300 + gpt-oss-120b 8398 = header 64698, all three columns.
Single-model sessions stay clean.

## Audit verdicts (Opus, all PASS)

- **Attribution:** exact (above). The `/model` switch line renders honestly.
- **Inline markdown styling:** PTY capture shows real ANSI —
  `\x1b[1m**Success**\x1b[0m`, highlight-tinted backtick spans, `\x1b[2m-`
  dimmed bullets — markers retained; piped output 0 escape bytes.
- **Docs cross-reference comparison:** kitsu+HF 12.4 s (9.9 s with spawn
  cap 0) vs claude 15.2 s; 2/2 spot-checks correct both; claude more
  thorough on anchor validation.

## Ranked gaps (queued for cycle 28)

1. **`/model` rejects provider-pinned HF ids** (`model:fastest`) that
   kitsu.toml itself documents — the catalog check must accept pin
   suffixes (blocks cross-provider switching ergonomics).
2. **Successful-repeat storms**: nine byte-identical `workspace_diff`
   calls cost 6.7x tokens on a two-word turn — the ISSUE-007 breaker
   counts only FAILING repeats; extend it to identical successful calls
   (cache or refuse after N).
3. **Integer tool args reject JSON strings** (`"1"` vs `1`) with no repair
   hint — coerce numeric strings or say how to fix.
4. Carried: denial-final disposition (dedicated), capped-read true size,
   worktree gitdir binds, python_tag, /doctor endpoint, credential
   filter, /memory, timer /loop; kitsu double-printing answers on the
   subagent path (new, minor).

## Report index

1. [Attribution + markdown + comparison](chat-ux-cycle-27-audits/raw/01-attribution.md)

## Loop state

Hourly main loop (:23) + UI-parity loop (:53; items 1-4 landed, item 5
theme-aware heartbeats next). Rotation: cycle 27 = Opus (this file),
cycle 28 = Sonnet. Cycle 28 opens with the /model pin-suffix fix and the
successful-repeat guard.
