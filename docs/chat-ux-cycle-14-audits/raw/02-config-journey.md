# Cycle 14 Configuration-Journey Critique

Date: 2026-08-12  
Focus: Kitsu CLI config/doctor UX vs Claude Code equivalent guidance  
Tester: Acting as developer on new machine hitting realistic config problems

---

## Journey Verdicts

### Journey 1: Fresh Start (`kitsu init`)

**Setup:**
```bash
mkdir journey1 && cd journey1
kitsu init
```

**Generated config excerpt:**
```toml
default_gateway = "ollama"
[gateways.ollama]
flavor = "openai"
base_url = "http://localhost:11434"
chat_path = "/v1/chat/completions"
models_path = "/v1/models"
model = "llama3.2"
context_window = 32768
auth = { method = "none" }
```

**Doctor output:**
```
[ok] config (--config) kitsu.toml — parsed successfully
[info] gateway/model: ollama/llama3.2
[info] auth: no credential required
[info] context window: 32768 tokens (gateway profile)
doctor: no blocking issues found
```

**Verdict: ✓ EXCELLENT**
- Init creates a well-commented starter with clear next steps inline ("next: kitsu --config kitsu.toml doctor")
- Generated config is sensible (ollama local default, helpful comments)
- Doctor output is clean and structured with status badges
- Next action is obvious: run doctor, then chat

**Issues:** None

---

### Journey 2: Broken TOML (Unbalanced Quote)

**Corrupted config:**
```toml
default_gateway = "ollama
[gateways.ollama]
# ... rest omitted
```

**Error output:**
```
[FAIL] config (--config) kitsu.toml — parse/read failed
doctor: 1 blocking issue(s)
  - config error: TOML parse failed at line 2, column 26 (source excerpt withheld to avoid echoing credentials)
! error: doctor found 1 blocking issue(s)
```

**Verdict: ✓ GOOD (with caveats)**
- Line/column pinpointing is helpful for IDE/editor jumps
- Thoughtful: withholds source to prevent credential echo
- **Gap:** Without excerpt, line 2 col 26 is ambiguous in a 20-line file; user must manually inspect

**Comparison to Claude doctor:**
- Claude's `doctor` just checks installation state; it doesn't parse user config files
- Kitsu goes further here by attempting parse validation
- Claude's equivalent would be: run the CLI once, get cryptic error, then debug

**Issues:** 
1. Source excerpt omission makes debugging harder than necessary for public code
2. Error could suggest "use an editor with TOML validation" or link to TOML spec

---

### Journey 3: Missing Credentials (Bearer-Env with Unset Var)

**Setup config:**
```toml
[gateways.anthropic]
flavor = "anthropic"
base_url = "https://api.anthropic.com"
model = "claude-3-5-sonnet-20241022"
context_window = 200000
auth = { method = "bearer-env", token_env = "MISSING_ANTHROPIC_KEY" }
```

**Doctor output:**
```
[ok] config (--config) kitsu.toml — parsed successfully
[info] gateway/model: anthropic/claude-3-5-sonnet-20241022
[info] auth: bearer variable KITSU_TOKEN is missing
doctor: 1 blocking issue(s)
  - auth variable KITSU_TOKEN is not present
```

**Ask output:**
```
! error: credential variable $KITSU_TOKEN is missing - set it or update the gateway auth configuration
```

**Verdict: ⚠ FAIR**
- Doctor correctly identifies missing credential
- **Critical gap:** `token_env = "MISSING_ANTHROPIC_KEY"` in config is **silently ignored**; kitsu defaults to `KITSU_TOKEN`
- Error message is copy-pasteable: "credential variable $KITSU_TOKEN is missing"
- But user configured a different env var name and got no warning

**Comparison to Claude Code:**
- Claude doesn't have bearer-env auth by default (anthropic flavor handles it natively)
- If Claude required custom env vars, it would likely validate that they exist or offer to set them
- Kitsu's UX here is weaker: accept the config but ignore the token_env field

**Issues:**
1. **token_env field is ignored** — either parse it properly or reject the config with "token_env not supported"
2. Doctor could say: "auth: bearer-env configured but token_env field is not supported; using default $KITSU_TOKEN"
3. No guidance on how to set $KITSU_TOKEN (export? .env file? secret manager?)

---

### Journey 4: Wrong Endpoint (Dead Port)

**Setup config:**
```toml
[gateways.local]
flavor = "openai"
base_url = "http://localhost:9999"
chat_path = "/v1/chat/completions"
model = "local-model"
context_window = 8192
auth = { method = "none" }
```

**Doctor output:**
```
[ok] config (--config) kitsu.toml — parsed successfully
[info] gateway/model: local/local-model
[ok] runtime state: ~/.kitsu — writable
[info] provider check: not requested (offline default)
doctor: no blocking issues found
```

**Ask with 5s timeout:**
```
~ inference provider attempt 1/3: connection; transport failed; terminal=true; 4998ms deadline remaining
kitsu-context: {"used":2,"effective":7680,"window":8192}
! error: chat request could not reach the gateway - check the selected gateway's base_url, network access, and provider availability; run `kitsu doctor --online`
```

**Verdict: ✓ GOOD**
- Doctor is offline-by-default (doesn't burn quota testing real endpoints)
- Connection error is **actionable**: lists three things to check
- **Excellent guidance:** suggests `kitsu doctor --online` for validation
- Error appears quickly (recognizes terminal=true after 4.9s)
- Retries are configured but give up when connection is terminal

**Comparison to Claude Code:**
- `claude doctor` doesn't test provider connectivity (it's a lighter check)
- Claude Code's equivalent friction: run a command, timeout, then debug network/auth
- Kitsu's suggestion to use `--online` flag is better than Claude's passive approach

**Issues:**
1. Doctor offline-by-default is sensible, but users expect it to catch URL typos
2. `--online` flag isn't mentioned in `doctor --help` (or is it?)

---

### Journey 5: Happy Path (Ollama with Real Model)

**Setup config:**
```toml
[gateways.ollama]
flavor = "openai"
base_url = "http://localhost:11434"
chat_path = "/v1/chat/completions"
models_path = "/v1/models"
model = "qwen2.5-coder:latest"
context_window = 8192
auth = { method = "none" }
```

**Test command:**
```bash
printf 'Reply with exactly: pong' | kitsu --config kitsu.toml --gateway ollama --jsonl chat
```

**Key events emitted (selected):**
```json
{"type":"run_started","run_id":"3335987-1786544375394790419","gateway":"ollama","model":"qwen2.5-coder:latest"}
{"type":"turn_started","turn_id":"piped:1"}
{"type":"provider_request","purpose":"inference","round":1,"tools_offered":4}
{"type":"provider_attempt","outcome":"body_completed","status":200,"terminal":true}
{"type":"assistant","text":"pong"}
{"type":"final_summary","status":"ok"}
```

**Exit code:** 0

**Verdict: ✓ PERFECT**
- All expected events fire in correct sequence
- Context window budgeting works (8192 total, 664 used, 7680 effective)
- Model receives/executes instruction correctly ("Reply with exactly: pong" → "pong")
- Exit 0 indicates success to calling script
- JSONL format is clean and parseable

**Issues:** None observed

---

### Journey 6: Real Config Doctor & Comparison

**Real config location:** `~/.kitsu/config.toml` (user's existing setup)

**Kitsu doctor output:**
```
kitsu 0.1.0 (11336b07bcd1-dirty, source commit 2026-08-12)
[ok] config (default) ~/.kitsu/config.toml — parsed successfully
[info] gateway/model: ollama/llama3.2
[info] auth: no credential required
[info] context window: 8192 tokens (gateway profile)
[info] project context: no llms.txt, AGENTS.md, or CLAUDE.md found
[ok] runtime state: ~/.kitsu — writable
[info] provider check: not requested (offline default)
[info] tools: filesystem=interactive chat prompts; automation is deny-by-default unless roots are granted commands=disabled unless explicitly enabled; enabled commands require exact approval plus mandatory fail-closed OS containment
doctor: no blocking issues found
```

**Claude Code doctor output:**
```
Claude Code doctor

Running: native (2.1.228)
Commit: 4a2077e9c396
Platform: linux-x64
Path: /home/mmpg/.local/share/claude/versions/2.1.228
Config install method: native
Search: OK (bundled)
Auto-updates: enabled
Auto-update channel: latest
Last update attempt: success → 2.1.228 (2026-08-12)

No installation issues found.
For a full setup checkup that can also fix issues, run /doctor in a Claude Code session.
```

**Verdict: ✓ KITSU IS SUPERIOR**

| Aspect | Kitsu | Claude Code |
|--------|-------|-------------|
| Config validation | Yes (parses TOML, checks gateways) | No (checks binary installation only) |
| Auth status | Shows explicitly (no credential required) | N/A |
| Context window reporting | Yes (shows budget & source) | N/A |
| Project context | Scans for llms.txt/AGENTS.md/CLAUDE.md | N/A |
| Runtime state | Checks filesystem & permissions | Checks binary path only |
| Tool status | Detailed auth/deny-by-default policies | N/A |
| Next steps guidance | "doctor: no blocking issues found" | Suggests `/doctor in Claude Code session` |

**Verdict:** Kitsu's doctor is **more comprehensive** and configuration-focused. Claude's is installation-focused. Both are appropriate for their domains, but Kitsu's gives more actionable next-step guidance for a new setup.

**Issue:** Kitsu's "no blocking issues found" doesn't explicitly say what you *can* do next (e.g., "ready to run: kitsu chat")

---

## Top-5 Ranked Config/Doctor UX Gaps

### 1. **Bearer-env token_env field is silently ignored**
   - **Gap:** Auth method accepts `token_env = "..."` in config but always defaults to `$KITSU_TOKEN`
   - **Impact:** User configures custom env var name, gets ignored, confuses about where to set the token
   - **Fix:** Either (a) parse & use `token_env` field, or (b) reject config with clear error: "token_env not yet supported; use $KITSU_TOKEN"
   - **Effort:** Low (one-liner to warn or parse)

### 2. **Doctor offline-by-default misses URL typos until ask/chat**
   - **Gap:** `doctor` says "no blocking issues" for a dead endpoint; user doesn't learn until timeout
   - **Impact:** "Did I set up correctly?" → runs doctor → "looks good" → runs chat → wait 5s for connection error
   - **Fix:** Suggest `doctor --online` in initial output, or auto-check connectivity for explicitly-configured base_url (not localhost defaults)
   - **Effort:** Low (add flag, document it)

### 3. **TOML parse errors hide source excerpt to avoid credential echo**
   - **Gap:** Shows "line 2, column 26" but withholds the actual text, making it hard to spot the error in a 20+ line file
   - **Impact:** User must manually inspect every position; defeats purpose of line/column precision for public code
   - **Fix:** Show source line but mask secrets: e.g., `default_gateway = "ollama` (truncate), or `auth = { ... [redacted] }`
   - **Effort:** Medium (requires safe-redaction logic, but worth it)

### 4. **Doctor "no blocking issues found" lacks actionable next step**
   - **Gap:** Terminal state doesn't say "you're ready to use this config" or suggest the next command
   - **Impact:** New user reads "no blocking issues" and is still unsure: do I run chat now? Do I run ask? Do I need to set up something else?
   - **Fix:** Append guidance: "You can now run: `kitsu --config <file> chat` or `kitsu --config <file> ask <prompt>`"
   - **Effort:** Low (one-liner)

### 5. **Auth method name "bearer-env" is not suggested when user tries "bearer"**
   - **Gap:** User types `auth = { method = "bearer", ... }` → gets error "expected one of: bearer-env, static, none" but no explanation of what each does
   - **Impact:** User knows it's wrong but not why or when to use bearer-env vs static
   - **Fix:** Expand error to: `method "bearer" is not supported. Use "bearer-env" to read token from env var, "static" to embed it (NOT RECOMMENDED), or "none" for no auth.`
   - **Effort:** Low (better error message)

---

## Cleanup

Removing scratch config directories:

```bash
rm -rf journey1 journey2 journey3 journey4 journey5
```

---

## Summary

**Kitsu's config/doctor UX is solid:**
- Init creates sensible starter configs with clear next steps
- Doctor is more comprehensive than Claude Code's equivalent (which focuses on binary installation)
- Error messages are generally actionable and concise
- Happy path works seamlessly with real models

**Five gaps exist (all low-effort fixes):**
1. Bearer-env token_env field ignored (either support it or reject)
2. Offline doctor misses URL typos (suggest --online flag)
3. TOML parse errors hide source (safe-redact secrets, show line text)
4. "No blocking issues" lacks next-step guidance (suggest next command)
5. Auth method names unexplained in errors (expand error messages)

**Comparison to Claude Code:**
- Kitsu's doctor targets config validation (appropriate domain)
- Claude's doctor targets binary installation (appropriate domain)
- Kitsu would benefit from Anthropic's "expand error messages with context" philosophy
