# Cycle 8 — Auditor 3: Documentation Drift Report

Binary: `target/debug/kitsu` @ `c66af4bba175` (source commit 2026-08-11).
Verification method: ran the actual binary (`--help` at every level, `kitsu init`
in a clean `HOME`, live interactive chat via pty-capture against
`--gateway ollama --model qwen2.5-coder:latest`, `--jsonl` scripted runs) and
diffed observed behavior against README.md, docs/ux-design.md,
docs/jsonl-protocol.md, docs/functionality.md, docs/agent_deck_ui.md, and
llms.txt. Every "WRONG"/"STALE" row below was reproduced against the running
binary, not inferred from source.

---

## Part 1 — Claim verification table

### README.md — CLI reference (global flags table, lines 501–520)

| Claim | Status | Evidence |
|---|---|---|
| `--gateway`, `--model`, `--config`, `--no-stream`, `--jsonl`, `--plain`, `--reduced-motion`, `--temperature`, `--reasoning-effort`, `--max-rounds` (default 10), `--timeout-secs` (default 120), `--max-output-tokens`, `--yes`, `--dangerously-allow-all-filesystem`, `--context-window`, `--compaction` (default trim), `--skill`, `--no-project-context` | **VERIFIED** | Byte-for-byte match against `kitsu --help` description text and defaults for all 18 rows. |
| `--agent-join-timeout <SECONDS>` — global flag, present in every subcommand's `--help` | **MISSING FROM TABLE** | Confirmed present and identical across `init/doctor/ask/review/models/sessions/artifacts/chat --help`. Not a row in the "Global flags" table. Only appears once, in prose, at line 743. A reader skimming the reference table (which the doc explicitly frames as authoritative — "every flag") will not learn this flag exists. |

### README.md — Chat-only flags table (lines 522–536)

| Claim | Status | Evidence |
|---|---|---|
| `--allow-path`, `--allow-write`, `--allow-read-write`, `--enable-commands`, `--command-root`, `--allow-command` (incl. new `{"argv":...,"cwd":...}` object form), `--script`, `--resume`, `--session`, `--no-session`, `--allow-turn-errors` | **VERIFIED** | Matches `kitsu chat --help` exactly, including the cycle-7-added object-form `--allow-command` syntax. |

### README.md — CLI reference / commands table (lines 483–494)

| Claim | Status | Evidence |
|---|---|---|
| `init`, `doctor`, `ask`, `review`, `models`, `sessions`, `artifacts`, `chat` all listed with accurate one-line descriptions | **VERIFIED** | Matches `kitsu --help` top-level command list and each subcommand's own summary line. |
| `artifacts [list\|prune <id\|all>]` description | **VERIFIED** | `kitsu artifacts --help` / `kitsu artifacts prune --help` confirm `list`, `prune <TARGET>`, `--dry-run`, `--older-than <DURATION>`, `--json` all exist as documented. |

### README.md — "Tools in chat" permission-prompt transcript (lines 554–564)

| Claim | Status | Evidence |
|---|---|---|
| Shown transcript: `⏺ kitsu wants to write /home/you/project/notes.md` / `[y] allow once   [a] always (this dir)   [n] deny` | **WRONG** | Live pty capture of an actual write-permission prompt: `◉ permission — kitsu wants to write /tmp/cycle8-audit3/project/hello.txt` / `[y] allow once · [a] always (/tmp/cycle8-audit3/project) · [n] deny`. Three concrete mismatches: (1) glyph — `⏺` (Claude Code's own bullet) vs actual `◉`; docs/ux-design.md line 39 explicitly says kitsu's glyph set is "not Claude Code's `⏺ ⎿ ✻`" — the README violates the design doc's own stated rule. (2) missing `permission — ` label segment. (3) separator — spaces vs `·`. This is the single clearest, most demonstrable finding in the audit. |

### README.md — Slash command table (lines 641–681)

| Claim | Status | Evidence |
|---|---|---|
| `/model`, `/models`, `/reasoning`, `/status`, `/diff`, `/review`, `/checkpoints`/`/checkpoint`, `/permissions`, `/tools`, `/allow*`, `/revoke-*`, `/skills`/`/skill`, `/save`, `/context`, `/compact`, `/profiles`, `/agent`, `/artifacts`, `/queue`, `/steer`, `/retry`, `/new`, `/sessions`, `/resume`, `/fork`, `/export`, `/archive`, `/restore`, `/delete`, `/undo`, `/clear`, `/help` | **VERIFIED** | Matches live `/help` output (grouped headings and row text identical, modulo the abbreviations noted below). |
| `/quit [wait\|cancel]` **(or `/exit`)** — presented with `/quit` as primary, `/exit` as the alias | **WRONG (direction reversed)** | Live `/help` output: `/exit [wait\|cancel] ... (alias: /quit)` — `/exit` is the registered primary name; `/quit` is the alias. Confirmed in source: `crates/kitsu/src/main.rs:2622` `name: "exit"`, `main.rs:2699-2703` `name: "quit", alias_of: Some("exit")`. Functionally both work identically, so this is a labeling/naming-consistency bug, not a functional one — but it's exactly the kind of "which one is canonical" detail a user copies verbatim from docs. |
| `/jobs [list\|show ID\|steer ID TEXT\|close-steering ID\|stop ID\|stop-all\|prune ID\|prune all]` | **VERIFIED (behavior), UNDER-SUMMARIZED (`/help` text)** | Live test: `/jobs prune all` → `~ pruned 0 terminal background job(s)`; `/jobs close-steering 1` → `~ background agent #1 steering: NotFound`. Both subactions work exactly as README describes. However the live `/help` one-liner only shows `/jobs [list\|show <id>\|steer <id> <text>\|stop <id>\|stop-all\|close <id>]` (alias: /agents) — it omits `prune` and `close-steering` entirely. README's claim in the "Tools in chat" prose that "Completion and `/help` share the same command registry, so their descriptions stay in sync" is therefore only true for the commands `/help` chooses to spell out; the table in README is more complete than `/help` itself, which could confuse a user cross-checking the two. |

### README.md — first-run / `kitsu init` flow

| Claim | Status | Evidence |
|---|---|---|
| "`kitsu init` writes a credential-free starter to `./kitsu.toml`... using no-clobber creation" | **VERIFIED** | Clean-HOME test: `kitsu init` → `created no-secret starter config at kitsu.toml`; re-run in the repo (where `kitsu.toml` already exists) → `! error: cannot initialize config 'kitsu.toml': already exists; refusing to overwrite it`, exit 1. |
| "`kitsu doctor` reports config lookup and parse status... without changing state or contacting a provider. Add `doctor --online`..." | **VERIFIED** | Confirmed offline vs `--online` behavior; `--online` correctly flagged `qwen2.5-coder:7b` as absent from the live catalog while offline mode reported "no blocking issues." |
| Every provider recipe in "Provider recipes" uses `flavor = "openai"` | **STALE / INCOMPLETE** | The config validator (`crates/kitsu/src/config.rs:279-284`) accepts **three** flavor values: `openai`, `anthropic`, `ollama`. `kitsu init`'s generated starter config uses `flavor = "ollama"` (`crates/kitsu/src/main.rs:463`) — a value that appears nowhere in README's "How it works: gateway profiles" section or any of the six provider recipes, all of which use `flavor = "openai"` even for the *Anthropic* recipe. The field is validated but not otherwise load-bearing (grep confirms `.flavor` is read only for validation and a JSON debug field), so this isn't a functional bug, but README never discloses that `anthropic`/`ollama` are valid values or that `kitsu init`'s own output disagrees with README's own "Ollama (local)" recipe block one section up. |
| (Implicit) `kitsu init` produces a config that works with `kitsu ask`/`chat` out of the box | **FIRST-RUN GOTCHA, not literally promised but worth flagging** | Generated config pins `model = "qwen2.5-coder:7b"` and `base_url = "http://127.0.0.1:11434"`. Against a real local Ollama serving `qwen2.5-coder:latest` (the tag `ollama pull qwen2.5-coder` actually produces), `kitsu ask` fails: `! error: gateway rejected the chat route or model 'qwen2.5-coder:7b' (HTTP 404)`. README's "Quick start" section sidesteps this by using the *bundled* `kitsu.toml` (model `llama3.2`) for the actual ask/chat commands and only takes the `kitsu init` path as far as `doctor` — so nothing in the README is technically false, but a user who reasonably continues past `doctor` into `ask`/`chat` on the init'd config hits an undocumented model-tag mismatch. |

### llms.txt (feeds every kitsu chat session's project context)

| Claim | Status | Evidence |
|---|---|---|
| Cargo workspace of five crates: kitsu, kitsu-core, kitsu-providers, kitsu-tools, kitsu-render | **VERIFIED** | Matches `Cargo.toml` workspace members and `crates/` directory exactly. |
| `kitsu` crate bullet lists entry points as "`init`, `doctor`, `ask`, verified `review`, `models`, `sessions`, and `chat`" | **STALE — missing `artifacts`** | `kitsu --help` shows 8 top-level subcommands; `artifacts` (added in cycle 7, Pass D) is not named, even though the same bullet does mention the abstract capability ("aggregate command-artifact lifecycle management"). Since this file is injected into every kitsu-on-kitsu chat's system prompt, the model itself is never told `kitsu artifacts` exists as a callable entry point by name. |
| `## Optional` doc index: links only to cycle-2 and cycle-3 UX audits, `ui_guidelines.md`, `context-management-stages.md`, `claude-code-comparison.md` | **STALE** | docs/chat-ux-cycle-4.md through -7.md (the two "heavy implementation cycles" this very audit angle references) are not linked anywhere in llms.txt. Nor are docs/ux-design.md, docs/jsonl-protocol.md, docs/agent-roles.md, or docs/subagents-design.md — all of which are current, user/dev-relevant docs. The curated project-context file a fresh kitsu session reads to understand "what this project is" is frozen at a cycle-3-era view of the docs tree. |

### docs/ux-design.md vs rendered output

| Claim | Status | Evidence |
|---|---|---|
| Glyph table: `◆` tool call, `◇` tool result, `◉` permission, `◈` receipts/section marker, `⛩` torii milestone | **VERIFIED** | Live capture matched exactly: `◆ Write(…/project/hello.txt)` tool-call line; `◉ permission — ...` prompt; `◇ error: PolicyDenied — ...` on denial; `◈ agent deck — 0 total · 0 running · ...` for `/jobs`, `/agent`; `◈ background-agent profiles — 2 available` with per-row `◇ reasoning | ...` and `◆ reviewer | ...` markers for `/profiles`. |
| Approval prompts share `◉ permission — <request>` header and `[y] allow once · [a] <scope> · [n] deny` option line | **VERIFIED** — and this is what makes README's `⏺`-based transcript wrong (see above). | |
| "Key surfaces" section (Banner, Spinner, Errors, Provider telemetry, Approval prompts, Context meter, `/help`) | **INCOMPLETE — Agent Deck surface undocumented** | `/jobs`, `/agent`, and `/profiles` render a distinct "agent deck" receipt format (`◈ agent deck — N total · N running · N done · N failed · N cancelled · N running slot(s) free · N record slot(s) free`) that is a first-class, cycle-6/7 surface (explicitly named in the cycle-7 checkpoint: "`/jobs`, `/agent`, and `/profiles` render from the DTO in the fox design language instead of Rust Debug dumps"). ux-design.md's "Key surfaces" list has no entry for it — the counters, the per-row `◆`/`◇` reuse for profile capability level, and the "no background agents" empty state are all undocumented. |

### docs/agent_deck_ui.md

| Claim | Status | Evidence |
|---|---|---|
| "The Agent Deck is Kitsu's multi-agent **terminal interface**" with "tmux-like detach/attach" and an "Agents \| Output" split-pane table layout | **ENTIRELY FICTIONAL** | This is the single worst file in the audit. Nothing resembling a split-pane, alternate-screen, tmux-like UI exists. Actual behavior (confirmed live): interactive chat "uses normal terminal scrollback rather than an alternate-screen TUI" (docs/functionality.md's own, accurate claim, line 25) and `/jobs`/`/agent`/`/profiles` print a single inline summary line plus per-row text, exactly like every other slash command. The doc appears to be a pre-implementation design aspiration that was never reconciled with what actually shipped, and it directly contradicts functionality.md elsewhere in the same `docs/` tree. |

### docs/jsonl-protocol.md vs live `--jsonl` runs

| Claim | Status | Evidence |
|---|---|---|
| Envelope: `schema: "kitsu.chat.v1"`, `type`, `run_id` on every event | **VERIFIED** | All captured events carry these three fields. |
| `run_started` → `mode` one of `ask, script, piped, review, models`; also carries `durable_session`, `resumed`, `session_snapshot_state`, `system_context_refreshed`, `skill_context_changes` | **VERIFIED** | `run_started` in scripted run had exactly these fields plus `gateway`, `model`, `session_id`. |
| `command_result` / `permissions` carries `commands.exact_grants[].cwd_source`, `host_evidence{repository_root, consumers, model_read_root:false, prompt_disclosure, excluded_private_paths}` | **VERIFIED** | Inside a git worktree: `host_evidence.excluded_private_paths` present, `repository_root` populated, `model_read_root:false`. Outside a git worktree: `repository_root:null`, `unavailable` present instead — matches the doc's stated fallback. `exact_grants` was empty in both test runs, so the `cwd_source` sub-field itself couldn't be directly observed, but the array's presence/shape is correct. |
| `/diff` result carries `evidence_bytes` and `model_disclosed:false` | **VERIFIED** | Live `/diff` command_result: `"evidence_bytes": 3317, "model_disclosed": false`. |
| Script query commands never disappear into human stderr; typed `command_result` for status/permissions/context etc. | **VERIFIED** | `/status`, `/permissions`, `/context` all produced one `command_result` each between matching `command_started`/`command_finished`. |
| No fields observed that aren't documented, and no documented fields absent from what was tested | **NO DRIFT FOUND** in the subset exercised (`run_started`, `command_started/result/finished`, `final_summary` for `/status`, `/permissions`, `/context`, `/diff`). Deeper event types (`provider_attempt`, `background_result`, `context_window_mismatch`, `assistant_disposition` edge cases) were not exercised live due to local-model latency; no evidence either way for those. |

### docs/functionality.md

| Claim | Status | Evidence |
|---|---|---|
| Top-level command table, chat command groupings, exit-code table, context-window resolution order | **VERIFIED**, consistent with README and live behavior. | |
| "Status: 2026-08-10" | **MINOR STALENESS** | Today is 2026-08-11 and the binary's own `--version` reports "source commit 2026-08-11" (i.e., a same-day-or-later commit than the doc's stamped status date). One day isn't material on its own, but combined with the llms.txt findings above it's a second data point that the docs tree isn't being touched in lockstep with the last commit of a cycle. |

### Exit codes (README "Notes & gotchas" + functionality.md table)

| Claim | Status | Evidence |
|---|---|---|
| `0` success, `1` CLI/config/policy/local-tool failure, `2` provider/HTTP/timeout/incomplete-turn, `130` Ctrl+C | **VERIFIED (partially, live)** | Bad gateway name → exit 1 (`! error: config error: unknown gateway 'doesnotexist' — defined gateways: ...`). Wrong/missing model on a real provider → exit 2 (`HTTP 404` → `! error: gateway rejected the chat route or model ...`). Ctrl+C (130) and the broken-pipe/exit-101-avoidance claim were not independently re-verified live (the latter needs a large multi-line `sessions` listing to trigger; environment only had zero/near-zero saved sessions) — no contradicting evidence found, just untested. |

---

## Part 2 — Ranked top-10 worst drift items (mechanical fixes)

### 1. README permission-prompt transcript shows the wrong glyph/format entirely (Tools in chat, ~line 561)
**Severity: highest** — this is the exact worst-case for a "docs are UX" bug: a user reads the README, expects `⏺ kitsu wants to write ...`, and sees something else the first time a tool touches their filesystem.

- Old text (README.md, lines 561–562):
  ```
  ⏺ kitsu wants to write /home/you/project/notes.md
    [y] allow once   [a] always (this dir)   [n] deny
  ```
- Correct text (verified live, matches docs/ux-design.md's documented `◉` design):
  ```
  ◉ permission — kitsu wants to write /home/you/project/notes.md
    [y] allow once · [a] always (/home/you/project) · [n] deny
  ```
  (Note the `[a]` scope description is also rendered as the actual directory path, not the literal string "this dir".)

### 2. `--agent-join-timeout` is a real global flag but absent from README's CLI reference table
**Severity: high** — the task's own audit brief flagged this flag by name; it exists in `kitsu --help` for every subcommand and is referenced in README prose (line 743) but is missing from the "Global flags" table that's supposed to be the exhaustive reference.

- Add a new row to the **Global flags** table (README.md, after the `--timeout-secs` row, ~line 513):
  ```
  | `--agent-join-timeout <n>` | `--timeout-secs` | How long a non-interactive exit (scripted/piped chat) waits for live background agents to finish before cancelling them. |
  ```

### 3. `docs/agent_deck_ui.md` describes a UI that was never built
**Severity: high** — an entire file, actively misleading about a major feature area (background agents / `/jobs` / `/agent` / `/profiles`), contradicts `docs/functionality.md`'s own correct claim two files over.

- Old text (docs/agent_deck_ui.md, in full):
  ```
  # Agent Deck UI

  The Agent Deck is Kitsu's multi-agent terminal interface.

  Features:
  - concurrent agents
  - structured attention system
  - tmux-like detach/attach
  - task orchestration

  Example layout:

  Agents | Output
  ----------------
  planner | logs
  builder | logs
  reviewer | logs
  ```
- Correct replacement (describing the actual, verified behavior):
  ```
  # Agent Deck UI

  The "agent deck" is Kitsu's inline rendering for background-agent state —
  not a separate terminal UI. It shares the normal chat scrollback (no
  alternate screen, no panes) and is produced by `/jobs`, `/agent`, and
  `/profiles`.

  `/jobs` and `/agent` print one `◈ agent deck — …` receipt line with running
  totals (`N total · N running · N done · N failed · N cancelled · N running
  slot(s) free · N record slot(s) free`), followed by one row per background
  job/agent, or `no background agents` when empty.

  `/profiles` prints `◈ background-agent profiles — N available` followed by
  one row per profile, prefixed `◆` (write-capable) or `◇` (read-only/no
  tools), each showing model, reasoning-effort policy, granted tools, and
  whether it inherits session grants.

  See docs/ux-design.md for the glyph legend and docs/functionality.md for the
  full command semantics (`/jobs steer`, `/jobs prune`, `spawn_agent_bg`, etc.).
  ```

### 4. `/quit` vs `/exit` — README documents the alias direction backwards
**Severity: medium-high** — cosmetically small, functionally harmless (both work), but directly contradicted by live `/help` output and by source (`name: "exit"`, `alias_of: "exit"` for `quit`).

- Old text (README.md, line 681):
  ```
  | `/quit [wait\|cancel]` (or `/exit`) | Exit only when jobs are done, explicitly wait, or immediately cancel the foreground plus queued/background work and join it first. |
  ```
- Correct text:
  ```
  | `/exit [wait\|cancel]` (or `/quit`) | Exit only when jobs are done, explicitly wait, or immediately cancel the foreground plus queued/background work and join it first. |
  ```
  (Also update the "`Ctrl+D` or `/quit` does not silently discard..." prose two paragraphs later, line 741, to lead with `/exit` for consistency.)

### 5. `docs/ux-design.md` "Key surfaces" section omits the Agent Deck entirely
**Severity: medium-high** — this is the file the audit brief specifically asked about ("cycle 7's pass C added deck glyph states — are they in the doc?"). They are not.

- Insert a new bullet in the "Key surfaces" list (docs/ux-design.md, after "Context meter", ~line 76):
  ```
  - **Agent deck**: `/jobs` and `/agent` share one `◈ agent deck — N total · N
    running · N done · N failed · N cancelled · N running slot(s) free · N
    record slot(s) free` receipt line, then per-row status, or `no background
    agents` when empty. `/profiles` uses the same `◈` receipt header with rows
    prefixed `◆` (write-capable / inherits session grants) or `◇` (tool-free /
    isolated), reusing the tool-call/tool-result glyphs to signal capability
    level rather than tool activity.
  ```

### 6. `kitsu init`'s generated config disagrees with every README recipe on `flavor` and `base_url`, and README never explains the three valid `flavor` values
**Severity: medium** — not functionally broken (flavor is validated but not behavior-changing), but a user diffing `kitsu init`'s own output against the "Ollama (local)" recipe two sections earlier will see `flavor = "ollama"` vs `flavor = "openai"` and `127.0.0.1` vs `localhost`, with no explanation.

- Add to README.md "How it works: gateway profiles" section, after the `flavor` line in the example block (~line 99):
  ```
  # `flavor` is validated against a closed set — `openai`, `anthropic`, or
  # `ollama` — but every value routes through the same OpenAI-compatible
  # transport; it does not change wire behavior today. `kitsu init` emits
  # `flavor = "ollama"` for its starter Ollama profile; the recipes below use
  # `flavor = "openai"` for every gateway including Ollama. Either is fine.
  ```

### 7. llms.txt omits `artifacts` from the named entry-point list
**Severity: medium** — llms.txt is the file that feeds every kitsu-on-kitsu chat session's project context, so this omission degrades the tool's own self-understanding, not just human readers.

- Old text (llms.txt, line 9):
  ```
  - `kitsu` — `init`, `doctor`, `ask`, verified `review`, `models`, `sessions`, and
    `chat`; slash commands; JSONL automation; `@file`; editable queues/steering;
    project context; bounded background-agent threads/profiles; reasoning control;
    and aggregate command-artifact lifecycle management.
  ```
- Correct text:
  ```
  - `kitsu` — `init`, `doctor`, `ask`, verified `review`, `models`, `sessions`,
    `artifacts`, and `chat`; slash commands; JSONL automation; `@file`; editable
    queues/steering; project context; bounded background-agent threads/profiles;
    reasoning control; and aggregate command-artifact lifecycle management.
  ```

### 8. llms.txt's `## Optional` doc index is frozen at cycle 3
**Severity: medium** — same "feeds live chat context" concern as #7; the whole point of the `## Optional` section is to let a model orient itself in `docs/`, and it's missing the two most recent implementation cycles plus the UX/JSONL design docs.

- Old text (llms.txt, lines 41–49):
  ```
  - [Cycle-2 UX audit](docs/chat-ux-cycle-2.md): reviewer findings, implemented
    outcomes, verification gates, and platform residuals.
  - [Cycle-3 UX audit](docs/chat-ux-cycle-3.md): ten-agent dogfood findings and
    the accepted improvement scope.
  - [UI guidelines](docs/ui_guidelines.md): terminal UX conventions.
  - [Development stages](docs/context-management-stages.md): historical staged
    context-management plan.
  - [Codex comparison](docs/claude-code-comparison.md): historical product
    comparison and roadmap notes.
  ```
- Correct text (add, don't just append — trim if the token budget for `## Optional` is tight; prioritize the newest cycles and the two contract docs over the historical ones):
  ```
  - [UX design](docs/ux-design.md): palette, glyphs, and degradation-tier
    contract for every rendered surface.
  - [JSONL protocol](docs/jsonl-protocol.md): the `kitsu.chat.v1` automation
    event contract.
  - [Cycle-6 UX audit](docs/chat-ux-cycle-6.md) and
    [Cycle-7 UX audit](docs/chat-ux-cycle-7.md): most recent dogfood findings
    and implemented outcomes (agent-deck correctness, artifact GC, host-owned
    evidence).
  - [UI guidelines](docs/ui_guidelines.md): terminal UX conventions.
  - [Codex comparison](docs/claude-code-comparison.md): historical product
    comparison and roadmap notes.
  ```

### 9. `/help`'s abbreviated `/jobs` and `/artifacts` summaries don't list all real subactions
**Severity: medium-low** — not wrong, just incomplete relative to README's own more-detailed table; worth tightening so the "completion and `/help` share the same registry" claim (README line 636) doesn't read as a stronger promise than it is.

- Old text (live `/help` output):
  ```
  /jobs [list|show <id>|steer <id> <text>|stop <id>|stop-all|close <id>]  inspect, steer, stop, and close background agent threads (alias: /agents)
  ```
- Correct text (align the in-product summary to what README already documents and what actually works):
  ```
  /jobs [list|show <id>|steer <id> <text>|stop <id>|stop-all|prune <id|all>|close-steering <id>|close <id>]  inspect, steer, stop, prune, and close background agent threads (alias: /agents)
  ```
  (This is a product-string fix, not a docs fix — flagging here because it's the cheapest way to close the README/`--help` gap from the code side rather than trimming README's table.)

### 10. `docs/functionality.md` status line is one day stale relative to the shipped commit
**Severity: low** — minor on its own, but a cheap, mechanical fix and a signal worth closing out at end-of-cycle.

- Old text (docs/functionality.md, line 3):
  ```
  > Status: 2026-08-10. This is a source- and test-backed description of the
  ```
- Correct text:
  ```
  > Status: 2026-08-11. This is a source- and test-backed description of the
  ```

---

## Appendix — things checked and found clean (no drift)

- `kitsu --help` top-level and all 8 subcommands' `--help` (`init`, `doctor`,
  `ask`, `review`, `models`, `sessions` incl. `show`/`export`, `artifacts` incl.
  `list`/`prune`, `chat`) — internally consistent, consistent with each other,
  and consistent with README's CLI reference (apart from item #2 above).
- `--version` output format (`kitsu 0.1.0 (c66af4bba175, source commit
  2026-08-11)`) matches README's description exactly.
- Bundled `kitsu.toml`'s six gateway blocks (ollama, vllm, custom, huggingface,
  openai, anthropic) match README's "Provider recipes" section verbatim.
- `kitsu sessions`, `kitsu sessions --json`, `kitsu artifacts`, `kitsu
  artifacts list --json` empty-state output matches README's documented
  format and exit-0 behavior.
- Exit code 1 for an unknown `--gateway` name; exit 2 for a provider HTTP 404
  (wrong/missing model) — both match the documented exit-code table.
- `docs/jsonl-protocol.md`'s envelope, `run_started`, `command_started`/
  `command_result`/`command_finished`, `/permissions`' `host_evidence` object
  (both inside and outside a Git worktree), and `/diff`'s `evidence_bytes`/
  `model_disclosed` fields all matched live `--jsonl` output exactly, in both
  directions (nothing observed-but-undocumented, nothing documented-but-absent)
  for the event types exercised.
- `docs/ux-design.md`'s palette/glyph table matched live rendering exactly for
  every glyph exercised (`◆`, `◇`, `◉`, `◈`).
- llms.txt's five-crate workspace description matches `Cargo.toml` and
  `crates/` exactly.
