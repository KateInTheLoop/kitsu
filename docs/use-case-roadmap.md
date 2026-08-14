# kitsu use-case roadmap

kitsu began as a code-focused agent CLI. This document widens the aim:
it enumerates the **user-facing use cases** kitsu should serve well, and
turns each into concrete, checkable requirements. The main dogfooding
loop (hourly at :17) works through these one at a time — one shippable
improvement per cycle against whichever use case is next — the same
compare → implement → validate → review → record rhythm the UI-parity
plan used, now organized by capability rather than by UI surface.

Written 2026-08-14, grounded in a full capability survey of the repo at
that date. "Current state" lines are what kitsu actually does today, not
aspiration; where a requirement is already met it is marked ✅ so the
loop skips it.

## How the loop consumes this doc

1. Pick the highest-priority use case with unmet requirements (order in
   **Sequencing** below; security and scientific-document work are
   described for completeness but deprioritized — see the note there).
2. Implement ONE requirement (or a coherent slice of one) per cycle,
   respecting the standing invariants: three-tier terminal degradation
   (`docs/ux-design.md`), sanitize round-trip, and stable machine
   records (`kitsu-usage:`, `kitsu-context:`, `result>`, `tool>`, JSONL
   `kitsu.chat.v1`) — automation contracts never shift as a side effect.
2. Validate: `cargo fmt --check`, clippy zero-warnings, `cargo test
   --workspace` (the one known PTY-environmental CLI failure allowed),
   plus a functional check through a real gateway (local ollama, or HF
   when a token is live) exercising the new behavior.
3. Commit one change on the working branch, spawn one cheap review
   agent, apply confirmed findings, then check the requirement off here
   with a dated note and record the cycle.

Requirements are written so "done" is observable — each has an
acceptance signal a review agent can verify.

---

## 1. Code analysis & code assistant  — *solid; harden the edges*

The founding use case and the most mature. Reading, grepping,
editing, diff review, syntax highlighting, subagent fan-out, and
context accounting all exist and are dogfood-hardened.

**Current state:** `fs_read_file` (ranged, capped, disclosed),
`repo_grep` (ripgrep-backed), `fs_edit_file` (precondition-guarded),
`fs_write_file` (conflict-aware), `workspace_diff` (host-verified git
diff), `review` subcommand, syntect highlighting for 16 languages.

**Requirements:**
- R1.1 ✅ Conflict-safe edits (precondition `old_text` / `expected_previous_content`).
- R1.2 ✅ Host-verified diff review that the model cannot fabricate.
- R1.3 Symbol-aware navigation: a tool to locate a definition/usages
  without a full grep sweep (LSP bridge or ctags fallback). *Accept:* a
  `find_symbol`-style tool returns definition site + references for a
  named symbol in a test repo; degrades cleanly when no indexer present.
- R1.4 Multi-file coherent edit receipts: when an edit touches N files,
  one receipt summarizes all N with per-file line deltas. *Accept:* a
  two-file edit turn emits a single grouped receipt; machine records
  unchanged.
- R1.5 Dependency/build awareness: read and summarize `Cargo.toml` /
  `package.json` / `pyproject.toml` for the model on request without it
  guessing. *Accept:* project-context discovery includes the manifest
  summary; `--no-project-context` still suppresses it.

## 2. Document reviewer (PDF / CSV / spreadsheets / images / docs) — *biggest gap*

Today kitsu reads **UTF-8 text only** (`fs.rs` truncates at the last
valid UTF-8 boundary and discloses). PDFs, CSVs as structured data,
spreadsheets, images, and Office docs are all opaque. This is the
richest greenfield lane.

**Current state:** plain-text reads; `@file` attachments UTF-8-only,
32 KiB/file. Zero document-format dependencies in any Cargo.toml.

**Requirements (ranked):**
- R2.1 **CSV/TSV as structured data**: a reader that parses delimited
  files, reports column headers + row count + inferred types, and can
  return a bounded row window rather than raw bytes. *Accept:* reading a
  1000-row CSV discloses shape (cols, rows, dtypes) and returns a capped
  preview; malformed rows disclosed, not silently dropped.
- R2.2 **PDF text extraction**: extract selectable text (pure-Rust,
  e.g. `pdf-extract`/`lopdf`), page-ranged and capped like text reads,
  with a clear "N of M pages, text-layer only, no OCR" disclosure.
  *Accept:* a text-layer PDF yields its text with page markers; a
  scanned/image-only PDF says so instead of returning garbage.
- R2.3 **Image handling**: for a vision-capable gateway, pass images
  through; otherwise report dimensions/format/EXIF and decline to
  hallucinate content. *Accept:* `.png`/`.jpg` reads route to the vision
  path when the model supports it, else a factual metadata receipt.
- R2.4 **Office/Markdown-adjacent docs**: `.docx`/`.xlsx` text and cell
  extraction (both are zip+XML; pure-Rust `calamine` covers xlsx).
  *Accept:* a `.docx` yields its paragraph text; an `.xlsx` yields
  sheet names + a bounded cell window.
- R2.5 **Type-aware dispatch**: `fs_read_file` (or a sibling
  `read_document` tool) detects type by extension+magic bytes and routes
  to the right extractor, always disclosing which path was taken and its
  limits. *Accept:* one entry point handles `.txt/.csv/.pdf/.png/.xlsx`
  with a per-type disclosure line; unknown types fall back to the
  UTF-8-with-truncation behavior unchanged.

## 3. System administration helper — *primitives exist, no orchestration*

`command_exec` runs single approved commands under real containment
(bubblewrap on Linux: RO runtime mounts, isolated net/PID/IPC/UTS,
scrubbed env). What's missing is the sysadmin *shape*: reading configs
and logs safely, and aggregating results.

**Current state:** single-command exec with argv-array (never shell),
approved command roots, bounded output, timeout. No log aggregation, no
templated runbooks, no fleet dispatch.

**Requirements:**
- R3.1 **Log-tail/scan tool**: bounded, ranged reads of large log files
  (follow last N KiB, or grep-within-file) without loading the whole
  file. *Accept:* scanning a 500 MiB log returns matched lines + a
  served-of-total disclosure; memory stays bounded.
- R3.2 **Config-diff/audit helper**: read a config, compare against a
  reference or a prior snapshot, report drift. *Accept:* two versions of
  an `sshd_config`-shaped file produce a structured drift report.
- R3.3 **Structured command results**: `command_exec` results expose
  exit code, timing, and truncation flags as fields the model can
  branch on (partly present — confirm and formalize). *Accept:* a
  non-zero exit is a first-class field, not just text.
- R3.4 **Runbook mode**: `--script` already runs ordered turns; document
  and test a sysadmin runbook pattern (ordered steps, stop-on-failure,
  JSONL receipt per step). *Accept:* a 3-step runbook halts on step 2's
  failure with a clear JSONL trail.

## 4. Automation / unattended agent — *strong; close the loop*

kitsu is explicitly built to run in loops, not chat. This lane is
already well-served; requirements here are about making unattended runs
trustworthy and observable.

**Current state:** `--dangerously-skip-permissions`, `/permissions
protect`, `--script`, piped/`-p` single-turn, `--jsonl` versioned
event stream, `--resume`, subagent spawn caps, `--timeout-secs`,
`--agent-join-timeout`, context compaction.

**Requirements:**
- R4.1 ✅ Unattended permission bypass with protected-path guards.
- R4.2 ✅ Versioned JSONL automation contract (terminal-on-first-failure).
- R4.3 **Cost/budget ceiling**: a `--max-spend` or token budget that
  ends a run cleanly with a disclosed receipt when hit. *Accept:* a run
  capped at N tokens stops at the cap with a truthful partial receipt
  and a distinct exit code.
- R4.4 **Durable run receipts**: machine-readable summary of an
  unattended run (turns, tools, tokens, exit reason) written to a file
  on request. *Accept:* `--run-report <path>` emits a stable-schema JSON
  summary; absent flag changes nothing.
- R4.5 **Retry/resume ergonomics**: a failed unattended run resumes from
  its last good turn. *Accept:* killing a `--script` run mid-way and
  re-running with `--resume` continues past completed steps.

## 5. Security tooling — *described, deprioritized*

> Scope note (2026-08-14, user-directed): kept in the roadmap for
> completeness but **not a focus area**. Work here stays on the
> unambiguously legitimate side — analyzing *your own* code and configs,
> reviewing logs, CTF/education, defensive hardening — and reuses lanes
> 1–3 rather than adding offensive capability. No exploit development,
> no targeting of third-party systems.

**Current state:** grep + model analysis; `review` for diffs. No SAST,
no dependency-vulnerability integration.

**Requirements (low priority):**
- R5.1 **Dependency advisory read**: summarize a lockfile against a
  local advisory source (e.g. `cargo audit` output) when the user runs
  it. *Accept:* given `cargo audit` JSON, produce a ranked risk summary.
- R5.2 **Secret-in-diff guard**: flag apparent credentials in a diff
  before commit (reuses the existing credential-filter machinery).
  *Accept:* a diff containing an API-key-shaped string is flagged.

## 6. Scientific / data document review (incl. biology) — *described, deprioritized*

> Kept for completeness; treated as a **sub-case of lane 2** (document
> review) rather than its own focus. Once CSV/PDF/spreadsheet extraction
> lands, scientific-paper and dataset review come largely for free. No
> domain-specific tooling planned for now.

---

## Sequencing

Priority order the loop follows (revisit as lanes close):

1. **Document review** (lane 2) — biggest capability gap, clear wins;
   start with R2.1 CSV, then R2.2 PDF, then R2.5 type-aware dispatch.
2. **System administration** (lane 3) — R3.1 log scanning first.
3. **Code analysis hardening** (lane 1) — R1.3 symbol nav, R1.4 grouped
   edit receipts.
4. **Automation trust** (lane 4) — R4.3 budget ceiling, R4.4 run report.
5. **Security** (lane 5) and **scientific docs** (lane 6) — only
   opportunistically, per the scope notes above.

Each closed requirement gets a dated `[x]` note in its lane and a cycle
entry in the loop's record. When a whole lane's non-deprioritized
requirements are met, note it here and move down the sequence.

## Cycle log

(appended each cycle, newest last)
