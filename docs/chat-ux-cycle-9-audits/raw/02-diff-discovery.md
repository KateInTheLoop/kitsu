# Cycle 9 Audit 2: Diff Explain UX & Command Discovery Polish

**Scope:** Compare "explain this diff" workflow (Codex CLI vs kitsu), assess command-discovery UX polish

**Test fixture:** 2-file commit with function renames + logic change (~30 lines total)
- user.ts: `validate_user_profile` → `is_valid_user`, removed inactive-user check
- auth.ts: Updated call sites
- Pre-existing bug: UserProfile interface not exported but imported

**Tools tested:**
- codex exec --sandbox read-only
- kitsu review (host-verified evidence path)
- kitsu chat (piped multi-turn)
- kitsu interactive commands: `/help`, `/models`, `/sessions`, `/skills`, `/artifacts`, `/queue list`, `/jobs list`, `/checkpoint`, `/checkpoints`, typo handlers

---

## Diff Explain Task: codex vs kitsu comparison

### Fixture evidence
```
auth.ts | 6 +++---
user.ts | 8 +++-----
 2 files changed, 6 insertions(+), 8 deletions(-)
```

### Finding 1: kitsu review discloses evidence model unavailable in piped chat

**Repro:** Same fixture, two approaches:
1. `kitsu review` → auto-scans working tree, shows "~ disclosing 1699 bytes of host-verified evidence"
2. Piped `echo "..." | kitsu chat` → no /diff evidence, model thinks it has no FS access, spawns agent

**Observed:**
- kitsu review: Structured findings with severity levels, file/line pointers, concise descriptions
- Piped chat: Model returns generic git-status explanations, never saw the actual diff, spawned Agent to run git commands, eventually timed out after 300s

**Expected:**
- Piped chat should either (a) auto-include `/diff` evidence like review does, or (b) make it obvious to user that they need `/diff <path>` before prompting with code questions

**Severity:** High (ergonomic friction vs codex; diff-explain UX regression)

**Proposed acceptance test:**
```
cd test-repo && echo "explain what my uncommitted changes do" | kitsu chat --timeout-secs 10
# Should NOT timeout; model should reference actual diff lines or prompt for /diff
```

---

### Finding 2: codex provides code-path inspection, kitsu doesn't expose tool transparency

**Repro:** Same fixture; codex output shows explicit tool runs:
```
exec: /bin/bash -lc "rg -n 'validate_user_profile|...' . && git diff --check && tsc --noEmit ..."
```

Kitsu review output shows findings but hides the verification steps.

**Observed:**
- Codex: Shows what commands ran (ripgrep search, tsc compile check, diff validation), token usage, reasoning effort set to "max"
- Kitsu: No stdout for tool invocations; results only (arguably cleaner but less educational)

**Expected:**
- Kitsu should optionally expose (via `--verbose` or `--explain` flag) which host-verified tools it ran to validate the diff

**Severity:** Low (polish; not a blocker; codex is more transparent)

**Proposed acceptance test:**
```
kitsu review --explain-findings
# Should show: "ran git diff, scanned for call-sites, checked imports"
```

---

### Finding 3: Codex catches pre-existing TypeScript bug; kitsu misses it

**Repro:** user.ts doesn't export `UserProfile` interface, but auth.ts imports it

**Observed:**
- Codex: Flags "Pre-existing compile issue: UserProfile is imported by auth.ts but is not exported from user.ts. Standard TypeScript compilation should report that error"
- Kitsu review: Doesn't mention the import error at all

**Expected:**
- Kitsu review should run a lightweight type check (e.g., tsc in strict mode) to catch imports of unexported symbols

**Severity:** Medium (silent type error; codex's advantage in static checking; kitsu focused on behavior change, not structural issues)

**Proposed acceptance test:**
```
# In a review where an import references an unexported symbol:
kitsu review | grep -i "export\|import"
# Should surface the error
```

---

### Finding 4: Evidence framing: codex shows diffs inline, kitsu abstracts them

**Repro:** Both tools asked to explain changes

**Observed:**
- Codex: Includes full diffs in the transcript (`git diff -- auth.ts user.ts`), then file contents; user can see exactly what was provided
- Kitsu review: Abstraction layer ("~ disclosing 1699 bytes") hides evidence content from user; user sees only findings

**Expected:**
- For transparency, kitsu review should optionally echo the diff it's sending (e.g., `--show-evidence`) so user can verify what the model saw

**Severity:** Low (polish; kitsu's approach is less noisy; tradeoff between transparency and conciseness)

**Proposed acceptance test:**
```
kitsu review --show-evidence | grep -A5 "^diff --git"
# Should show the actual diff being sent to the model
```

---

## Command Discovery UX Polish (kitsu only)

### Finding 5: Inconsistent empty-state messaging phrasing

**Repro:** In fresh kitsu session, run:
- `/sessions` → "no saved sessions"
- `/skills` → "no skills found - add SKILL.md dirs under .kitsu/skills/ or ~/.kitsu/skills/"
- `/artifacts` → "command artifacts are disabled because the command tool is not enabled"
- `/checkpoints` → "no recoverable file checkpoints in this session"
- `/queue list` → "~ 0 / 8 prompts queued" (not an empty state; shows metric)
- `/jobs list` → "# agent deck - 0 total ... | no background agents"

**Observed:**
- Verb tense inconsistency: "saved" (past) vs "found" (past) vs "disabled" (past participle) vs "recoverable" (adjective)
- Some include guidance (skills), some don't (/sessions, /artifacts, /checkpoints)
- Capitalization varies: "no saved sessions" vs "no skills found" (consistent lowercase)
- `/jobs list` header is verbose (deck metaphor with metrics) but body is terse

**Expected:**
- All empty states use consistent verb form: "no X" (present tense abstraction, e.g., "no sessions found", "no checkpoints available")
- All include guidance: "no sessions — start one with /new" or similar
- Capitalization and punctuation rules applied uniformly

**Severity:** Medium (consistency debt; affects discoverability for new users)

**Proposed acceptance test:**
```
# Fresh session:
kitsu chat --help >/dev/null && kitsu chat <<< "/sessions" | grep "^no"
# Should be: "no saved sessions — start one with /new or use /resume"
```

---

### Finding 6: Typo suggestions work but fallback varies by command

**Repro:** Typos in a fresh session:
- `/hlep` → "unknown command: /hlep - did you mean /help?" ✓
- `/sesions` → "unknown command: /sesions - did you mean /sessions?" ✓
- `/jobs lst` → "usage: /jobs [list|show <id>|steer ...]" (no suggestion)
- `/queue foobar` → (not tested; likely similar)

**Observed:**
- Levenshtein-distance suggestion works for single-letter typos
- But `/jobs lst` (wrong subcommand) doesn't suggest `/jobs list`; shows usage instead
- Inconsistency: some commands suggest, some show usage

**Expected:**
- All misdirected commands suggest closest valid subcommand before usage
- Or: usage always includes note "did you mean: /jobs list?"

**Severity:** Low (polish; current behavior is acceptable)

**Proposed acceptance test:**
```
echo "/jobs lst" | kitsu chat --plain | grep -i "did you mean\|suggest"
# Should suggest "/jobs list"
```

---

### Finding 7: Missing identity markers in command-list headers

**Repro:** Compare help output headers:
- `/help` → "commands:" (plain list)
- `/models` → "~ 1 model(s) available:" (uses ~ prefix, count in header)
- `/jobs list` → "# agent deck - 0 total | ..." (uses # prefix, deck metaphor, metrics)
- `/queue list` → "~ 0 / 8 prompts queued" (uses ~ prefix, metric)
- `/checkpoints` → "no recoverable file checkpoints in this session" (no header; just message)

**Observed:**
- Inconsistent glyph use: "~" vs "#" vs none
- Some headers include counts, some don't
- `/jobs list` uses thematic "deck" metaphor but other commands don't (no torii/diamond)
- Ux docs likely specify a marker style (torii ⛩ or diamond ◆) that's not applied uniformly

**Expected:**
- All list-style outputs use consistent leading glyph (e.g., `~ 0 / 8 prompts` or `◆ no background agents`)
- All include a metric or status summary in the header
- Commands that belong to a conceptual "group" (agents/jobs/queue) share identity markers

**Severity:** Medium (visual consistency; affects brand/polish)

**Proposed acceptance test:**
```
kitsu chat --plain <<< "/help" | grep "^commands:" | head -1
# Compare glyph style to /jobs list, /queue list outputs
# Should all use the same leading marker
```

---

### Finding 8: `/help` command-list column alignment varies

**Repro:** Run `/help` and check alignment:
```
  /archive <id>                                                    archive an inactive saved conversation
  /delete <id>                                                     permanently delete an inactive live session
  /checkpoint <id>                                                 preview one recoverable verified file checkpoint
```

**Observed:**
- Commands with variable-length arguments (`<id>`, `<name>`, `[markdown|json]`) cause columns to misalign
- Some descriptions start at column ~92, others at ~80
- Descriptions aren't right-padded; they flow into trailing whitespace
- Readable but not "polished"

**Expected:**
- Column alignment: command + args padded to fixed width, then description starts
- E.g., `  /archive <id>` (padded to 40 chars) then description
- Alternatively, use table-like formatting or separate lines

**Severity:** Low (polish; readability is OK)

**Proposed acceptance test:**
```
kitsu chat --plain <<< "/help" | grep "^  /" | awk '{print length($0), $0}' | sort -n | tail -3
# Should show consistent max column width for all commands
```

---

## Parity-or-better notes (diff explain task)

1. **kitsu review > codex:** Built-in flow doesn't require piping git output; one command gets the job done
2. **codex > kitsu:** Transparent tool invocation (visible exec calls); user knows exactly what was checked
3. **kitsu = codex (roughly):** Behavioral risk detection quality; codex found DoS path, kitsu focused on logic change
4. **kitsu + codex:** kitsu review + codex together cover both high-level behavior AND deep static checks

---

## Top-5 findings ranked by impact

| # | Finding | Severity | Impact |
|---|---------|----------|--------|
| 1 | Piped chat lacks auto-evidence; timeouts on diff-explain | High | Regression vs codex; UX friction |
| 2 | Inconsistent empty-state messaging (verb tense, guidance) | Medium | Polish; affects discoverability |
| 3 | Codex catches type errors kitsu misses | Medium | Static analysis gap |
| 4 | List-output glyphs/headers inconsistent | Medium | Visual polish; brand consistency |
| 5 | Help command column alignment varies | Low | Readable but unpolished |

---

## Summary (10 lines)

Kitsu review cleanly beats piped chat for diff explaining (auto-evidence, no flags needed, structured output). However, piped chat lacks `/diff` evidence by default and times out on the same task—regression vs codex. Codex shows tool invocations transparently; kitsu abstracts them (trades UX visibility for brevity). Codex catches pre-existing type errors; kitsu doesn't. Command discovery is functional but inconsistent: empty states use mixed verb tenses, help columns misalign, list-output glyphs vary (no torii/diamond applied uniformly). Typo suggestions work but only some subcommands fallback with suggestions. Overall: kitsu review is solid; interactive command polish needs consistency pass.
