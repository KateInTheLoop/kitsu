# Skills & Context Provenance — Design

- **Status:** Draft / proposed
- **Date:** 2026-07-05
- **Depends on:** the context-management work
  ([design](context-management-design.md)) — reuses `ContextMeter`, the `llms.txt`
  loader (discovery + budget-gating), and the `/context` command.

Two related features:

1. **Skills** — opt-in, local, on-demand capability bundles (like the
   agent-skills / karpathy-skills idea and Claude Code's `SKILL.md`), so kitsu is
   extensible without code.
2. **Context provenance** — an honest, traceable ledger of **what** is in the
   context window, **why** it's there, its **token** cost, and the **policy** that
   admitted or trimmed it. This is the differentiator: Claude Code shows *how
   much* context is used; kitsu shows *where every token came from and why*.

---

## 1. Problem statement

- **Extensibility.** kitsu's behavior is fixed in code. Users want to teach it
  reusable workflows ("review a diff like this", "write a conventional commit")
  without patching Rust — the "skills" pattern.
- **Trust.** Skills are *instructions* injected into a tool-enabled agent with
  filesystem access. Auto-loading them from the internet is a prompt-injection
  vector. Any skills design must be **local, user-curated, opt-in** — never
  auto-fetched from GitHub.
- **Honesty about usage.** Agentic CLIs quietly stuff the window with system
  prompts, memory files, tool schemas, and (now) project context and skills. The
  user can't easily answer "*why* is 6 K of my window gone before I typed
  anything, and what policy loaded it?" `/context` today shows category totals but
  not provenance or the reason. kitsu should be auditable: every token
  attributable to a source and a reason, with the governing policy visible.

---

## 2. Skills

### 2.1 What a skill is

A directory containing a `SKILL.md` with YAML frontmatter + a markdown body:

```markdown
---
name: code-review
description: Review a diff or file for bugs, then summarize findings by severity.
when_to_use: When the user asks to review code, a diff, or a PR.
# optional:
always: false           # if true, the body is always loaded (use sparingly)
allow_paths: []          # extra read paths this skill needs (still permission-gated)
---

# Code review

Steps:
1. Read the target file(s) with fs_read_file.
2. For each issue, note file:line, severity, and a one-line fix.
3. …
```

Only the **frontmatter** (`name` + `description` + `when_to_use`) is cheap and
lives in the context "skill index". The **body** is loaded on demand.

### 2.2 Discovery (local, opt-in)

Mirrors the `llms.txt` walk-up, reusing that machinery:

1. **Project skills:** `.kitsu/skills/*/SKILL.md` from the launch dir up to the
   git/project root.
2. **User skills:** `~/.kitsu/skills/*/SKILL.md`.
3. A configured extra dir (profile/CLI).

GitHub and other remotes are a **source you clone in** (`git clone … ~/.kitsu/skills/x`),
never auto-fetched. A skill is trusted because *you placed it there* — same trust
model as `--allow-path`.

### 2.3 Loading model (budget-aware)

- At chat start, kitsu builds a **skill index**: one line per skill
  (`name — description`), injected into the system prompt. Cost = a few tokens
  per skill; this is all that's always-present.
- The model invokes a skill by calling a **`skill` tool** (`skill(name)`), or the
  user runs **`/skill <name>`**. That loads the skill's **body** (and lists its
  bundled files) into context for the rest of the session.
- `always: true` skills load their body up front (for a project's house rules);
  discouraged unless small, and always shown in provenance so the cost is visible.
- Loaded skill bodies are budget-gated exactly like `llms.txt` `## Optional`:
  they count against the window and are trimmed first under pressure (an invoked
  skill can be re-loaded on demand).

### 2.4 Skills, files, and links (the file/link strategy)

- **Bundled files.** A skill dir may ship resource files (templates, checklists,
  examples). The skill body references them by relative path; the model reads
  them with `fs_read_file`. kitsu grants read access to the skill's own directory
  when the skill is loaded (permission-gated, like the offload dir), so bundled
  resources are reachable without a broad `--allow-path`.
- **Links.** A skill's frontmatter/body may list reference URLs. kitsu does **not**
  auto-fetch them (network + injection risk). Instead: (a) surface them to the
  user, and (b) if/when a `web_fetch` tool exists, it's opt-in and permission-
  gated like the filesystem. Links are pointers, not auto-loaded content — the
  same "pointer, re-read on demand" principle as tool-result offload.
- **Selecting the right skill.** Model-driven by the `description`/`when_to_use`
  in the index (the model calls `skill(name)` when a task matches), plus a
  `/skills [filter]` command for the user to browse. Optionally a lightweight
  keyword suggestion ("this looks like a job for `code-review` — /skill it?").

### 2.5 Security summary

- Local, user-placed only; no auto-fetch.
- A skill's text is *context*, not privileged: it can *suggest* tool calls, but
  every filesystem action still goes through the permission model.
- Loading a skill shows up in provenance (§3) with its token cost, so a bloated
  or suspicious skill is visible, not silent.

---

## 3. Context provenance — honest usage accounting

The centerpiece. Every contribution to the window is a tagged ledger entry, so
`/context` can explain **what, why, tokens, policy** — not just totals.

### 3.1 The ledger

```rust
struct ContextEntry {
    source: Source,     // where it came from
    reason: String,     // WHY it's loaded (human sentence)
    tokens: u64,        // estimated cost (usage-anchored where possible)
    policy: Policy,     // Admitted | Optional(kept|skipped) | Trimmed | Offloaded
}

enum Source {
    BasePrompt,
    ProjectContext { path, section },   // llms.txt — required vs optional
    Skill { name, invoked_at },         // a loaded skill body
    SkillIndex,                          // the always-on skill frontmatters
    ToolSchemas { tools },               // advertised tool definitions
    Conversation { role },               // user / assistant / tool messages
    Offloaded { path },                  // a pointer standing in for a big result
}
```

Each loader (base prompt, `llms.txt`, skills, tool registry, the trimming
cascade) records entries as it admits/trims content. Nothing enters the window
without a ledger entry — that invariant *is* the honesty guarantee.

### 3.2 What `/context` shows

Today's bar + per-category totals stay, but each line gains a **why** and a
**policy**, and a `/context why` (or `--verbose`) prints the full ledger:

```
kitsu> /context
model: qwen2.5-coder   window: 8192   effective: 7680   used: 2093 (27%)

source                     tokens  policy      why
─────────────────────────  ──────  ──────────  ─────────────────────────────────
base prompt                   120  admitted    core assistant instructions
skill index (3 skills)         84  admitted    always-on: names + descriptions
project (llms.txt)            620  required    ./llms.txt — required sections
  └ optional                    0  skipped     over window/4 budget on this model
skill: code-review            310  invoked     loaded at turn 3 (skill tool)
tool schemas (3)              301  admitted    fs_read_file, fs_list_dir, fs_write_file
user turns                    393  admitted    4 messages
assistant turns               339  admitted    4 messages
tool results                  476  admitted    2 kept · 1 offloaded → …/textcall-0.txt
─────────────────────────  ──────
total                        2743  headroom 4937 until trim
```

Every token is attributable. Policies are explicit: `skipped` (budget), `trimmed`
(elided/dropped), `offloaded` (on disk). This directly answers "why is my window
full and who decided that."

### 3.3 Machine + honesty guarantees

- The `kitsu-context:` stderr line gains a compact `sources` map so scripts and
  the dogfood harness can assert on provenance.
- **No hidden loads:** a debug assertion (and a test) that the ledger's total
  matches the meter's estimate within tolerance — if something entered context
  without a ledger entry, that's a bug, surfaced loudly.
- Trimming/offload/compaction update the ledger (mark entries `trimmed`/
  `offloaded`/`summarized`), so `/context` after a compaction shows what was
  reclaimed and how.

---

## 4. Non-goals

- Auto-fetching skills or link content from the internet (trust/injection).
- A skills marketplace / registry (out of scope; `git clone` is the distribution
  mechanism).
- Exact tokenization for the ledger — usage-anchored estimates (as elsewhere) are
  sufficient; the honesty is in *attribution*, not sub-token precision.

## 5. Open questions

1. Skill index always-on cost vs. lazy discovery: cap the number of indexed
   skills, or page them? (A repo with 50 skills shouldn't spend 500 tokens on the
   index.)
2. Should `always: true` skills be allowed at the user (global) level, or
   project-only (to avoid a global skill silently taxing every session)?
3. `/context why` granularity — per-source (above) vs. per-message. Start
   per-source; per-message on request.
4. Do we add a minimal `web_fetch` tool now (for skill links, permission-gated),
   or defer until there's demand?

---

## 6. Stages of development

Ordered per the plan: **skills first**, then the **file/link strategy**, then the
**context-provenance statistics**. Each stage ships working and is independently
useful.

**Effort:** S (≤ ½ day) · M (~1–2 days) · L (~3+ days).

### Stage S1 — Skill discovery + index + on-demand load  *(M · first)*
- `SKILL.md` parser (reuse the `llms.txt`/frontmatter tolerance): `name`,
  `description`, `when_to_use`, `always`, body.
- Discovery: `.kitsu/skills/*/` (project, walk-up) + `~/.kitsu/skills/*/` (user).
- Inject the **skill index** (name — description per skill) into the system
  prompt; load `always: true` bodies up front.
- A **`skill` tool** (`skill(name)` → loads that skill's body into context) and a
  **`/skills [filter]`** command to browse + a **`/skill <name>`** to load
  manually.
- Budget: index counted; invoked bodies budget-gated + trimmed first.
- **Tests:** parse frontmatter/body; discovery precedence; index injection; the
  `skill` tool loads the body; a small live check that the model invokes a skill.
- **Ship:** kitsu's own starter skill(s) under `.kitsu/skills/` (e.g. a
  `commit-message` skill) as examples.

### Stage S2 — Skill files & links (the file/link strategy)  *(M)*
- Grant read access to a loaded skill's own directory (permission-gated, like the
  offload dir) so bundled resources are reachable via `fs_read_file`.
- Surface a skill's declared links to the user; do **not** auto-fetch.
- Optional lightweight suggestion ("looks like a job for `code-review`").
- **Tests:** bundled-file read is permitted only when the skill is loaded; links
  are shown, never fetched.

### Stage S3 — Context provenance ledger + `/context why`  *(L)*
- Add the `ContextEntry` ledger; have every loader (base prompt, `llms.txt`,
  skill index, skills, tool schemas, conversation, offload) record entries.
- Extend `/context` with the `tokens · policy · why` table and add `/context why`
  (full ledger). Add a `sources` map to the `kitsu-context:` line.
- Honesty invariant: a test asserting the ledger total matches the meter estimate
  within tolerance (no un-attributed context).
- Wire the trimming cascade to update entry policies (`trimmed`/`offloaded`/
  `summarized`) so post-compaction `/context` explains what was reclaimed.
- **Tests:** ledger attribution per source; policy transitions on trim/offload;
  the total-matches-meter invariant.

### Sequencing

```
S1 (skills: discover + index + load)   ← reuses llms.txt machinery
  └ S2 (skill files + links)           ← needs S1's loaded-skill state + perms
  └ S3 (provenance ledger + /context why)
         ↑ ledger also attributes base prompt, llms.txt, tool schemas, offload —
           independent of S1/S2 but most valuable once skills add sources to trace
```

- **S1** delivers extensibility (the headline).
- **S2** makes skills genuinely useful (their resources).
- **S3** delivers the honesty/transparency goal and is where the "trace what was
  loaded and why" feature lives — valuable on its own, and the natural home for
  attributing skills once they exist.

### Cross-cutting
- **Docs:** README "Skills" + "Context provenance" sections as each ships.
- **Security:** reaffirm local-only skills in the README; never auto-fetch.
- **Dogfood:** a scenario that loads a skill and asserts `/context why` attributes
  it with the right token cost and policy.
