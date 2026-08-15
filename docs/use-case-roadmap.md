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
- [x] (2026-08-14) R2.1 **CSV/TSV as structured data** — shipped as the
  `fs_read_csv` builtin (commit 8566486): headers + total row count +
  per-column inferred type (int/float/bool/string, nullable-aware),
  bounded pageable window (start_row/max_rows, 500 ceiling), malformed
  rows counted and disclosed, TSV + headerless support. Pure-Rust `csv`
  crate. A live llama3.2 run hardened two small-model arg footguns
  (stringy booleans, empty delimiter).
- [x] (2026-08-14) R2.2 **PDF text extraction** — shipped as the
  `fs_read_pdf` builtin (commit 68dccc2): per-page text-layer extraction
  via pure-Rust `lopdf`, total page count, bounded page window
  (start_page/max_pages, 50 ceiling), "text-layer only (no OCR)"
  disclosure. Image-only pages flagged as no-text-layer; a page whose
  text can't be decoded discloses the specific extraction failure
  (honest, not garbage). 32 MiB file guard; same allowlist path. Known
  limitation: lopdf 0.34 fails ToUnicode CMap on some real-world PDFs
  (e.g. LibreOffice output) — degrades to an honest per-page disclosure;
  a richer extractor can follow if this bites.
- R2.3 **Image handling**: for a vision-capable gateway, pass images
  through; otherwise report dimensions/format/EXIF and decline to
  hallucinate content. *Accept:* `.png`/`.jpg` reads route to the vision
  path when the model supports it, else a factual metadata receipt.
- [x] (2026-08-14) R2.4 **xlsx cell extraction** — shipped as
  `fs_read_xlsx` (commit 63a139a): pure-Rust `calamine`, lists sheet
  names + a bounded cell window (sheet by name/index, start_row/max_rows,
  500 ceiling), wired into `read_document`'s XLSX branch (real extraction
  now, not a decline). Same allowlist guard; 32 MiB cap. `.docx`
  paragraph text is NOT yet done (calamine covers spreadsheets, not Word)
  — docx remains detected-and-declined by the dispatcher; a small
  zip+XML docx reader can follow if needed.
- [x] (2026-08-14) R2.5 **Type-aware dispatch** — shipped as the
  `read_document` tool (commit 1d01628): detects type by magic bytes
  then extension, routes `.csv/.tsv`→fs_read_csv, `.pdf`/%PDF→fs_read_pdf,
  plain/unknown→UTF-8 text read (fallback preserved), and detects+discloses
  images (PNG/JPEG/GIF/WEBP) and Office (xlsx/docx/pptx) with an honest
  "no extractor yet" decline instead of binary garbage. Per-type disclosure
  line on every route; delegates reuse the same allowlist guard. Image and
  xlsx *extraction* (R2.3/R2.4) plug into this dispatcher next.

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
- R4.6 **Reasoning-exhaustion warning** (surfaced by both Kimi and
  DeepSeek loops): when a turn returns empty `content` with
  `finish_reason:length` and reasoning tokens at/near the output cap, the
  whole budget was silently consumed by hidden reasoning — an unattended
  run just burns tokens producing nothing. kitsu should detect this and
  emit an actionable warning ("output budget consumed by hidden
  reasoning; raise --max-output-tokens or lower --reasoning-effort"),
  ideally as a distinct machine-record field. *Accept:* a forced
  reasoning-overflow turn emits the warning on stderr and a flag in the
  JSONL/usage record; a normal turn does not.
- R4.7 **Subcommand-flag hint** (surfaced by the DeepSeek loop): top-level
  `kitsu --allow-path/--allow-write/--agent-spawn-cap …` fails because
  those are `chat` subcommand flags, with no hint pointing there. *Accept:*
  the error names the flag and suggests it belongs under `chat`.
- R4.8 **`--agent-spawn-cap 0` must not crash the turn** (real bug,
  DeepSeek cycle 2): when the cap is 0 and the model attempts a spawn,
  kitsu denies it and withholds the spawn tools mid-turn, after which the
  next provider request is rejected `HTTP 400` (a request-shape break
  from mutating the tool list mid-conversation). `cap 0` should mean the
  spawn tools are never advertised in the first place, so there is
  nothing to withhold. *Accept:* a run with `--agent-spawn-cap 0` never
  advertises spawn_agent* tools; a model that would otherwise spawn
  simply doesn't see them, and no mid-turn tool-list mutation or HTTP 400
  occurs. (Affects both HF worker loops.)
- R4.9 **Disclosing response-parse errors** (surfaced by the GLM loop):
  `invalid response; HTTP 200; body failed` does not say which parse
  step or chunk failed, so a transport/format failure can't be
  root-caused from the receipt alone. *Accept:* a malformed provider
  response surfaces a message naming the failing stage (e.g. "usage
  chunk", "tool_call arguments JSON", "empty choices") and a bounded
  snippet of the offending body, without leaking secrets.
- R4.10 **Technical read/tool-call cap per turn** (surfaced by the
  DeepSeek loop, W6): kitsu has a spawn-count cap (`--agent-spawn-cap`)
  but no analogous cap on reads or total tool calls per turn. A model
  that compulsively over-reads (DeepSeek: 15-28 reads, never writing,
  until the 300s timeout discards the whole turn) cannot be reined in by
  prose alone — a prose read-cap is 0-for-3 against it. *Accept:* a
  `--max-tool-calls-per-turn` (or `--max-reads-per-turn`) lever that,
  when exceeded, returns a tool error nudging the model to act on what it
  has, the same way the spawn cap does. Would also bound cost on
  runaway turns.

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

1. **Document review** (lane 2) — R2.1 CSV ✅, R2.2 PDF ✅, R2.5 dispatch
   ✅, R2.4 xlsx ✅. Only R2.3 (image metadata) remains, and docx text as
   an optional follow-up. Lane 2 is effectively complete for the common
   document types — **advance to lane 3 next** (do R2.3 opportunistically).
2. **System administration** (lane 3) — R3.1 log scanning first. NEXT LANE.
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

### Cycle 3 — 2026-08-14 — R2.5 type-aware dispatch (document-review lane core complete)

Shipped `read_document` (commit 1d01628): magic-bytes-then-extension
detection routing to fs_read_csv / fs_read_pdf / fs_read_file, with
honest detect-and-decline for images and Office docs (no binary garbage)
and a per-type disclosure line on every route. 7 tests (detection matrix
+ e2e routing CSV/text/PDF/PNG/xlsx + allowlist). Live-verified through
ollama qwen2.5-coder across CSV/PDF/text — correct routing, honest
CMap-failure disclosure on the PDF. Haiku review: SHIP-AS-IS — all 10
edge cases traced (magic-before-extension precedence, delegates
re-authorizing, WEBP `magic[8..12]` length guard, authorization before
magic read, no double-prepend, no raw-bytes path for image/office),
acceptance signal met, no findings. **Lane 2's sequenced core (R2.1,
R2.2, R2.5) is complete.** Next: R2.4 xlsx (calamine) plugs a real
extractor into the dispatcher, then R2.3 image; then lane 3 (sysadmin).

### Cycle 1 — 2026-08-14 — R2.1 CSV structured reading (document-review lane)

Shipped `fs_read_csv` (commit 8566486). Document review's first
capability: structured shape + type inference + bounded windowing +
malformed-row disclosure, pure-Rust `csv` crate, same permission/byte-cap
path as `fs_read_file`. 11 tests (formatter, e2e authorization, lenient
args). A live llama3.2 gateway run was the load-bearing functional check
— it caught two real small-model arg footguns that unit tests alone
missed (stringy `no_header:"false"` → `lenient_bool`; empty `delimiter:""`
→ default comma), both fixed and pinned. Verified end-to-end: the model
called the tool and correctly reported `id:int, name:string, price:float,
in_stock:bool` including a nullable price column. Haiku review:
SHIP-AS-IS — all 9 negative cases traced concretely (nullable folding,
malformed-row disclosure, 500-row ceiling, paging saturating arithmetic,
delimiter validation, lenient_bool, truncation honesty, authorization
path, panic-freedom), acceptance signal met, no findings.
Next in lane 2: R2.2 PDF text extraction.

### Cycle 2 — 2026-08-14 — R2.2 PDF text extraction (document-review lane)

Shipped `fs_read_pdf` (commit 68dccc2): per-page text-layer extraction
via pure-Rust `lopdf`, page count, bounded window, image-only + decode
-failure disclosure, 32 MiB guard, same permission path. 6 tests
including an e2e that builds a real 2-page PDF with lopdf. The live
gateway run (ollama qwen2.5-coder) confirmed the tool is registered,
invoked, policy-enforced, and parses page count correctly — and
usefully exposed that lopdf 0.34 can't decode some LibreOffice PDFs'
ToUnicode CMap, which the tool discloses honestly per page rather than
emitting garbage (the acceptance signal's intent). Haiku review:
SHIP-AS-IS — all 10 edge cases traced (per-page error isolation,
half-open paging bounds, 1-based numbering, size guard before parse,
corrupt-input handling, spawn_blocking, panic-freedom), acceptance
signal met, no findings.
Next in lane 2: R2.3 image handling, then R2.4 docx/xlsx, then R2.5
type-aware dispatch — after which lane 2's core is complete.
