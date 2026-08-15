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
- [x] (2026-08-15) R1.3 **Symbol-aware navigation** — shipped as the
  `find_symbol` builtin (commit 6aa8357): recursively scans an allowed
  file or directory (skipping VCS/build dirs + binaries, bounded to 5000
  files / 64 MiB / 2 MiB per file) and returns definition sites first,
  then references, each as `path:line`. Definitions are classified
  language-agnostically — a definition keyword (`fn`/`func`/`def`/`class`/
  `struct`/`enum`/`trait`/`type`/`impl`/`const`/`let`/… incl. `let mut
  <sym>`) immediately before a WHOLE-WORD occurrence marks the line a
  definition; every other whole-word hit is a reference (`foo` never
  matches `foobar`/`foo_bar`). A heuristic textual scan — no LSP/ctags/
  index required, so it works anywhere and degrades to an honest "no
  definitions or usages found" when the symbol is absent (the
  degrade-cleanly half of the acceptance). Same allowlist guard
  (`check_read` authorizes the root; the walk never follows symlinks out
  of it; protected paths skipped). 4 tests (whole-word matching,
  cross-language classification, clean not-found render, and an e2e tree
  scan proving def + both refs found, build-dir + binary skipped, absent
  degrade, allowlist denial). Haiku review: SHIP-AS-IS — all 8 areas
  traced (boundary/char-safety, classification precision, symlink+
  protected guards, real bounds, panic-freedom, determinism, additive
  only, non-vacuous tests), no findings.
- R1.4 Multi-file coherent edit receipts: when an edit touches N files,
  one receipt summarizes all N with per-file line deltas. *Accept:* a
  two-file edit turn emits a single grouped receipt; machine records
  unchanged.
- [~] (2026-08-15) R1.5 Dependency/build awareness — **tool half shipped**
  as the `read_manifest` builtin (commit 2932451): on request the model
  reads a project's real deps/build shape via a summary of `Cargo.toml`
  (name/version/edition, dependencies/dev/build sections, feature count,
  virtual-workspace note), `package.json` (name/version, deps/devDeps/
  peerDeps, script names), or `pyproject.toml` (PEP 621 `[project]` with a
  `[tool.poetry]` fallback, PEP 508 requirement names or the Poetry table
  minus the `python` pin, build-backend). Sorted + de-duplicated + bounded
  (default 50/section, ceiling 500) with `+N more`; pure-Rust `toml` +
  serde_json; same allowlist guard as `fs_read_file`; a malformed manifest
  is disclosed ("could not parse — …"), never guessed. 5 tests. Haiku
  review: SHIP-AS-IS. **Remaining (the acceptance's other half):**
  project-context DISCOVERY should auto-include the manifest summary, and
  `--no-project-context` still suppress it — that path lives in the kitsu
  binary (`llms.rs` `discover`/`session.rs`), a UI/main-loop surface, so
  it is deferred there like R3.4 and R2.3's vision passthrough.
- [x] (2026-08-14) R1.6 **Edit files larger than the read cap** (found by
  the Kimi loop, cycle 13) — the journaled edit/write snapshotted the
  existing file at the 64 KiB *read* cap, so any file above it (e.g.
  `orchestrator.rs` at 129 KiB) was uneditable, failing "existing file
  exceeds the 65536-byte journal limit". Silently forced supervisor
  hand-fixes for cycles 9-13. Fixed (commit 333e97b): the pre-image is
  bound by the *write* cap (1 MiB) via a new `max_write_bytes()`
  accessor; a 200 KiB file is now edit/write-able (regression-tested).
  **Validated in production 2026-08-14:** GLM edited the 63 KiB
  `openai_compat.rs` directly across 4 in-place edits with zero
  journal-limit errors (was one addition from the old wall).
- [x] (2026-08-14) R1.8 **Debris-detector false positive on Edit content**
  (found by the Kimi loop, cycle 16) — FIXED (commit dad6882): the write/
  edit content-quality heuristic (`content_quality_warning`) naively did
  `content.contains(marker)`, so writing a file that lists the chat-template
  tokens as quoted data (kitsu's own token table, debris-detector code)
  spuriously warned. Made quote-aware: `has_unquoted_chat_template_marker`
  flags a marker only when it is NOT immediately preceded by a `"`/`'` — a
  raw leaked completion has bare markers, source code quotes them. Still
  advisory (never blocks). Tests: quoted table + single-quoted → no warning;
  bare marker still warns; e2e through write_file_run on the JSON warning
  field.
- [x] (2026-08-14) R1.7 **Line-number prefixes on ranged reads** — shipped
  (commit 51fc5cf): `read_capped_line_range` prefixes each served line with
  its absolute number + tab (`42\t<code>`), cap accounting included. The
  tradeoff was handled by SCOPING the prefix to ranged reads only — a full
  read (the edit-context path) stays verbatim, so `fs_edit_file` (which
  matches against the full/raw read) is unaffected; the prefix is
  documented as display-only. Live-verified: a model citing a ranged read
  now reports the exact line. Directly fixes the ~1-line drift that hit
  every ranged-read reviewer (DeepSeek).

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
- [x] (2026-08-15) R2.3 **Image handling** (metadata half) — shipped as
  `fs_read_image` (commit 5f8fe17): reports format + pixel dimensions from
  the header via pure-Rust `imagesize` (PNG/JPEG/GIF/WEBP/BMP…), reading
  only the first 64 KiB (memory-bounded), and explicitly declines to
  interpret content ("a vision-capable model is required"). Wired into
  `read_document`'s image branch, so it yields real dimensions instead of
  a bare "detected PNG". Same allowlist guard; never decodes pixels.
  **Follow-up:** vision passthrough (routing images to a vision-capable
  gateway when the model supports it) — the other half of the acceptance,
  a larger gateway-integration task, not yet done.
- [x] (2026-08-14) R2.4 **xlsx cell extraction** — shipped as
  `fs_read_xlsx` (commit 63a139a): pure-Rust `calamine`, lists sheet
  names + a bounded cell window (sheet by name/index, start_row/max_rows,
  500 ceiling), wired into `read_document`'s XLSX branch (real extraction
  now, not a decline). Same allowlist guard; 32 MiB cap. **`.docx`
  paragraph text now DONE** (2026-08-15, commit a30464e): `fs_read_docx`
  extracts Word paragraph text via pure-Rust `zip` (deflate-only) +
  `quick-xml` — `<w:t>` runs into `<w:p>` paragraphs, `<w:tab/>`→tab,
  `<w:br/>`→newline, entity/char-ref decoding, empty paragraphs skipped;
  32 MiB file guard + 64 MiB decompressed zip-bomb cap + paragraph
  pagination; wired into `read_document`'s DOCX branch (real extraction
  now). **`.pptx` slide text also DONE** (2026-08-15, commit 3b10ece):
  `fs_read_pptx` extracts slide text via the shared `parse_ooxml_paragraphs`
  (`<a:t>`/`<a:p>`), enumerating `ppt/slides/slideN.xml` in NUMERIC order,
  grouped per slide, wired into `read_document`'s PPTX branch. So lane 2
  covers CSV/PDF/XLSX/DOCX/PPTX + image metadata + dispatch — every common
  document type, the full Office triad. Only unknown zip-based Office
  types remain detect-and-disclose.
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
- [x] (2026-08-14) R3.1 **Log-tail/scan tool** — shipped as
  `fs_scan_file` (commit 5cbc0d1): grep mode streams a file line-by-line
  and returns matched lines with line numbers + a matches-shown-of-total
  disclosure (256 MiB scan cap, per-line display cap — memory bounded
  regardless of size); tail mode does a true seek-to-end read of the last
  ≤4 MiB and returns the last N lines (correct + bounded for any size).
  Same allowlist guard. Opens the sysadmin lane.
- [x] (2026-08-14) R3.2 **Config-diff/audit helper** — shipped as
  `fs_diff_config` (commit 647f5a5): parses two line-oriented configs
  (`key value` / `key=value` / `key: value`, # and ; comments skipped)
  and reports drift — added/removed/changed with reference→current
  values + unchanged count; `ignore_case` for sshd-style directives;
  duplicate keys disclosed. Both paths authorized. Live-verified diffing
  a default vs hardened sshd_config.
- [x] (2026-08-14) R3.3 **Structured command results** — VERIFIED
  already fully implemented (no code needed). `command_exec` serializes
  the whole `CommandOutput` struct to JSON (command.rs:3094): `exit_code`,
  `success`, `signal`, `elapsed_ms`, `stdout_truncated`/`stderr_truncated`,
  and capture-complete flags are all first-class fields. A non-zero exit
  is a completed result (Ok), not an error — pinned by
  `nonzero_exit_and_stream_bounds_are_truthful` (exit_code Some(7)) and
  `registered_handler_returns_structured_json` (the model-facing JSON
  exposes exit_code). The "confirm and formalize" was already done.
- R3.4 **Runbook mode**: `--script` already runs ordered turns; document
  and test a sysadmin runbook pattern (ordered steps, stop-on-failure,
  JSONL receipt per step). *Accept:* a 3-step runbook halts on step 2's
  failure with a clear JSONL trail. **DEFERRED from the capability loop:**
  the `--script` runner lives in `kitsu/src/main.rs` (the large
  interactive/scripted-chat loop, with `ScriptFailureKind` + JSONL
  emission already present) — that surface is owned by the UI/main loop
  and actively edited by a concurrent session, so runbook stop-on-failure
  + per-step JSONL is best done there, not from a low-cost capability
  tick. Groundwork exists (`ScriptFailureKind::exit_code`,
  `strict_jsonl_kind`, per-turn `turn_finished`/`error` JSONL events).

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
- R4.11 **Discarded turns must disclose files were modified** (found by
  the DeepSeek loop, W8): when a turn is discarded (e.g. on a
  `--timeout-secs` cancellation) after tool calls already wrote to disk,
  the transcript says "discarded" but the file edits persisted —
  "discarded" wrongly implies rollback. An unattended run can silently
  leave half-applied changes. *Accept:* a discarded turn that ran any
  write/edit tool surfaces a "N file(s) were modified before the turn was
  discarded" notice (stderr + machine record), listing the paths, so the
  operator/next-run knows the workspace changed.
- R4.12 **Don't classify a RECOVERED-tool-error turn as a failure**
  (HIGH — GLM loop W5 + Kimi loop cycles 14/16/17: FOUR sightings across
  two models, escalate): a turn that hit a tool error (e.g. a
  PolicyDenied misfire) or a self-corrected edit conflict, then RECOVERED
  and produced a complete committed deliverable, is still marked
  `DISCARDED` / `session not saved` / exit 1 because ANY tool/policy error
  in the turn trips the failure classification. This wrongly signals
  failure — a literal retry-on-failure would burn a run discarding a GOOD
  result — and (with R4.11) leaves the files on disk under a "discarded"
  banner. *Accept:* a turn that ends with a committed assistant answer is
  NOT classified a failure merely because it recovered from an earlier
  tool/policy error mid-turn; the errors are still reported (counts/JSONL)
  but the turn's disposition reflects the committed outcome. Related gap
  (Kimi cycle 17): the receipt does not distinguish "policy-poisoned but
  COMPLETE" from "policy-poisoned and INCOMPLETE (budget/round cutoff)" —
  a supervisor had to read the raw transcript to tell the turn actually
  ran out of budget mid-edit. The disposition should carry both facts:
  whether a final answer committed, AND whether the turn was cut off.
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
- [x] (2026-08-15) R5.1 **Dependency advisory read** — shipped as the
  `audit_summary` builtin (commit 41783be): parses a `cargo audit --json`
  report into a ranked risk view — vulnerabilities first (advisory id,
  package + version, title, patched-version fix or "no patched release",
  CVE aliases), then advisory warnings (unmaintained/yanked/unsound)
  grouped by kind; bounded (100/section, max 1000) with truthful elision,
  clean-report + malformed-JSON disclosed honestly. Dep-free (serde_json);
  read-only, same allowlist guard. Unambiguously defensive (summarize your
  OWN lockfile's advisories from a report you generate). Haiku review:
  SHIP-AS-IS. Note: this is the summary half; a fuller flow could also
  RUN `cargo audit` via the guarded command_exec, but the roadmap's
  acceptance ("given cargo audit JSON") is met by the summary tool.
- [x] (2026-08-15) R5.2 **Secret-in-diff guard** — shipped as the
  `scan_secrets` builtin (commit 535475c): flags lines that appear to
  hold a credential — known token prefixes (hf_/sk-/ghp_/AKIA/AIza/…),
  URL user:pass@host, secret assignments (api_key=/password:/token=/…),
  and Bearer tokens — reporting each flagged line number + kind with the
  line REDACTED (never echoing the raw secret). Point it at a saved
  `git diff`. Detection mirrors the binary's credential filter
  (`sanitize_and_redact_text`) exactly, incl. the ≥8-body-char guard that
  spares `hf_model`-style identifiers; kept as a self-contained copy
  because kitsu-tools can't depend on the binary (a future refactor could
  share a module). Streams (256 MiB budget, bounded memory), advisory
  (never blocks). Bearer redaction runs FIRST to close a real leak
  (`Authorization: Bearer <tok>` — the `authorization:` assignment rule
  would otherwise eat "Bearer" and expose the token), pinned by a
  regression test. Dep-free. Haiku (security-focused) review: SHIP-AS-IS,
  no-leak invariant verified (bearer-first, UTF-8-safe, source-faithful,
  tests assert secret ABSENCE). **Lane 5 complete.**

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
2. **System administration** (lane 3) — R3.1 log scan ✅, R3.2 config
   diff ✅. Next: R3.3 structured command results, R3.4 runbook mode.
3. **Code analysis hardening** (lane 1) — R1.3 symbol nav ✅; R1.5
   manifest **tool** ✅ (`read_manifest`). Both remaining lane-1 items now
   live in the UI/main-loop surfaces: R1.4 grouped multi-file edit
   receipts (receipt aggregation in `main.rs`) and R1.5's second half
   (manifest summary auto-wired into project-context discovery in
   `llms.rs`, suppressed by `--no-project-context`). The capability loop
   has picked the low-cost self-contained slices dry — advance to lane 4.
4. **Automation trust** (lane 4) — R4.3 budget ceiling, R4.4 run report.
5. **Security** (lane 5) and **scientific docs** (lane 6) — only
   opportunistically, per the scope notes above.

Each closed requirement gets a dated `[x]` note in its lane and a cycle
entry in the loop's record. When a whole lane's non-deprioritized
requirements are met, note it here and move down the sequence.

## Cycle log

(appended each cycle, newest last)

### Cycle 9 — 2026-08-15 — R2.3 image metadata (completes lane-2 image handling)

Shipped `fs_read_image` (commit 5f8fe17): format + pixel dimensions from
the header via pure-Rust `imagesize`, memory-bounded (64 KiB), wired into
`read_document`'s image branch — 'detected PNG' becomes 'PNG, 120x80 px'.
Metadata-only: it declines to interpret content, so a text model can't
hallucinate what an image depicts. 5 tests (formatter, e2e direct +
dispatch on an embedded real PNG, allowlist). Live-verified through
ollama: reported PNG 120x80. Picked over the higher-value-but-riskier
R4.10 (CLI+config+orchestrator plumbing) and R4.12 (main.rs, UI-loop-
owned) as the clean low-cost slice, and it completes lane 2's document
types (CSV/PDF/xlsx/image + dispatch). Vision passthrough deferred (a
gateway-integration follow-up). Haiku review: SHIP-AS-IS — all 10 cases
traced (64 KiB memory bound, authorized path, double-Result no-panic
handling, graceful non-image degradation, never decodes pixels), no
findings. Next: R4.10 or R4.12 (both need a bigger, deliberate cycle) or
lane-1 R1.4/R1.5.

### Cycle 10 — 2026-08-15 — R1.3 find_symbol (symbol navigation, lane 1)

Shipped `find_symbol` (commit 6aa8357), the next sequenced item after
lane 2 (complete) and lane 3 (complete bar the UI-loop-owned R3.4):
heuristic symbol navigation with no language-server/index dependency, so
it satisfies R1.3's "degrades cleanly when no indexer present" by design
— it IS the fallback. Recursive bounded walk of an allowed file/dir
(skips VCS/build dirs + binaries; 5000-file / 64-MiB / 2-MiB-per-file
caps; never follows symlinks out of the authorized root), returning
labeled definition sites first then references as `path:line`.
Definition vs reference is a whole-word + definition-keyword heuristic
(so `foo` never matches `foobar`; `self.parse()` / `use crate::parse` /
`return parse(x)` are references, `fn parse` / `let mut total` are
definitions). Chosen as a clean self-contained builtin (like the lane-2/3
fs tools) that needs NO main.rs/render surgery — keeping it off the
concurrently-edited UI surfaces. Functional check: the e2e test drives
the real tool path (authorize → walk → scan → render) over a temp tree
incl. binary-skip, build-dir-skip, absent-symbol degrade, and allowlist
denial (roadmap permits a direct tool test in lieu of a live gateway).
Full workspace suite green bar the known PTY cli flake. Haiku review:
SHIP-AS-IS — 8 areas traced (boundary/char-safety, classification
precision, symlink + protected-path guards, real bounds incl. the
disclosed one-file byte-cap overshoot, panic-freedom, determinism,
additive-only, non-vacuous tests), no findings. Next lane-1: R1.4
grouped multi-file edit receipts or R1.5 manifest summary; or the
higher-value R4.10 (technical read cap) / R4.12 (failure classification,
still open HIGH) when a bigger deliberate cycle is warranted.

### Cycle 11 — 2026-08-15 — R1.5 read_manifest (dependency/build summary, lane 1)

Shipped `read_manifest` (commit 2932451), the self-contained TOOL half of
R1.5: Cargo.toml / package.json / pyproject.toml summarized (name/version,
edition/build-backend, dependency names by section — sorted, de-duped,
bounded 50/section) so the model reads real deps instead of guessing.
Pure-Rust (`toml` + serde_json); PEP 508 requirement-name extraction and a
Poetry `[tool.poetry]` fallback (minus the `python` pin); same allowlist
guard; malformed input disclosed honestly, never guessed. 5 tests
(three-format rendering + requirement parsing + e2e/allowlist). Functional
check = the e2e through read_manifest_run (roadmap permits a direct tool
test). Haiku review: SHIP-AS-IS — 8 areas traced (detection routing, parse/
panic safety incl. the requirement_name char-boundary slice, honest
malformed disclosure, real bounds, extraction correctness, sandbox,
additive-only, genuine tests), no findings. This drains the last
low-cost self-contained lane-1 slice: R1.4 (grouped edit receipts,
main.rs) and R1.5's project-context wiring (llms.rs) are both UI/main-loop
surfaces. **Sequencing advanced to lane 4** (automation trust — R4.3
budget ceiling, R4.4 run report) for the next capability cycle, mindful
those touch the run layer; the fully self-contained wins are now in lane
4's tool-shaped items or opportunistic lane-5/6.

### Cycle 12 — 2026-08-15 — R2.4 docx text extraction (completes lane-2 doc types)

Shipped `fs_read_docx` (commit a30464e), closing the last common
document type left open in lane 2 (R2.4's docx follow-up). Pure-Rust
`zip` (default-features off + deflate only) + `quick-xml` 0.41 —
`word/document.xml` parsed into `<w:p>` paragraphs from `<w:t>` runs,
`<w:tab/>`→tab / `<w:br/>`→newline, the 5 predefined entities + numeric
char-refs decoded (0.41 surfaces `&ref;` as its own `GeneralRef` event),
empty paragraphs skipped, text-only disclosed. Wired into
`read_document` (DOCX now extracts, was "no extractor yet"). Bounded:
32 MiB file guard, 64 MiB decompressed zip-bomb cap on the entry stream,
paragraph pagination (default 500, max 5000, 50k collect ceiling). Zero
new crates in the lock — both deps were already transitive via calamine
— kept cheap by slimming zip to deflate-only. Functional check: e2e
builds a REAL .docx (zip writer) through read_docx_run + the
read_document dispatch, plus zip-without-document.xml honest error and
allowlist denial. Chosen over the collision-prone lane-4 run-layer items
(R4.3/R4.4/R4.10-12 all live in main.rs/orchestrator, UI/model-loop
territory) as the clean self-contained slice still on the board. Haiku
review: SHIP-AS-IS — 8 areas traced (zip-bomb bound on the decompressed
stream, panic-freedom, honest degradation, dispatch ordering, additive-
only, genuine real-zip tests), no findings. **Lane 2 is now fully
complete for common document types.** Next capability cycle: lane 4
(automation trust) needs a self-contained angle, or opportunistic
lane-5 R5.1 (cargo-audit summary — dep-free, tool-shaped).

### Cycle 13 — 2026-08-15 — R5.1 audit_summary (opens lane 5, dep-free)

Shipped `audit_summary` (commit 41783be): a `cargo audit --json` report
parsed into a ranked risk view (vulnerabilities-with-fixes first, then
warnings by kind), dep-free (serde_json), read-only, allowlist-guarded,
honest on clean/malformed input. Picked as the clean SELF-CONTAINED slice
because the document-review lane is complete and every remaining
lane-1/3/4 item now lives in the concurrently-edited run layer
(main.rs/orchestrator) — collision territory for the capability loop.
Unambiguously defensive per the lane-5 scope note (analyze your OWN
lockfile). 3 tests (ranked render, clean/no-patch/malformed honesty,
e2e + allowlist). Haiku review: SHIP-AS-IS — 8 areas traced (Option-
chain parse safety, truthful elision counts from real totals, missing-
field fallbacks, sandbox, additive/dep-free), no findings.

**Standing-loop status:** the capability loop has now drained every
cleanly self-contained kitsu-tools slice across lanes 1-3 + lane-5 R5.1.
What remains is either (a) run-layer work (R4.3/R4.4/R4.6/R4.10-12 in
main.rs/orchestrator — belongs to the UI/main loop or a coordinated
automation cycle), or (b) lane-5 R5.2 (secret-in-diff guard — reuses the
credential-filter machinery, whose home needs locating first), or (c)
lane-6 scientific-doc review (largely covered by lane 2 already). Next
tick should pick R5.2 if the credential machinery is reusable as a tool,
else flag that the loop needs a run-layer mandate coordinated with the
other sessions.

### Cycle 14 — 2026-08-15 — R5.2 scan_secrets (completes lane 5)

Shipped `scan_secrets` (commit 535475c): an advisory credential guard for
diffs/files — flags known token prefixes, url user:pass, secret
assignments, and bearer tokens, emitting each flagged line REDACTED
(never the raw secret). Detection ports the binary's credential filter
verbatim (kitsu-tools can't depend on the binary). Development found and
fixed a REAL leak: on `Authorization: Bearer <tok>` the `authorization:`
assignment rule ate the word "Bearer" and left the token exposed, so
bearer redaction now runs FIRST — pinned by a regression test asserting
the token's ABSENCE. Streams (256 MiB budget), dep-free. Security-focused
haiku review: SHIP-AS-IS — no-leak invariant verified (bearer-first,
UTF-8-safe on adversarial input, source-faithful, tests assert secret
absence not just marker presence). **Lane 5 (security tooling) complete:
R5.1 audit_summary + R5.2 scan_secrets.**

**Capability-loop runway note:** with lanes 2, 3 (bar UI-owned R3.4), and
5 complete and lane 1 down to UI-owned items, the loop has now exhausted
the cleanly SELF-CONTAINED kitsu-tools slices. Everything left is
run-layer (lane 4: R4.3/R4.4/R4.6/R4.10-12 in main.rs/orchestrator —
concurrently-edited by the UI/main loop and touched by the model-worker
loops) or lane-6 scientific docs (already covered by lane 2). The loop
needs a repointed mandate — a coordinated lane-4 assignment or a gen-2
role — rather than more standalone fs tools. Flagged for the gen-2
strategy decision.

### Cycle 15 — 2026-08-15 — fs_read_pptx (completes the Office triad, lane 2)

Shipped `fs_read_pptx` (commit 3b10ece): PowerPoint slide-text extraction,
the last common Office type. Enumerates `ppt/slides/slideN.xml`, sorts
NUMERICALLY (lexical would put slide10 before slide2), extracts `<a:t>`
runs into `<a:p>` paragraphs grouped per slide, wired into `read_document`
(PPTX now extracts). The docx parser was generalized into a shared
`parse_ooxml_paragraphs(text_tag, para_tag, tab_tag, break_tags)` — docx
now calls it with the w:-tags, pptx with the a:-tags; docx behavior
unchanged (tests pass untouched). No new crates (zip + quick-xml already
deps). Bounded (32 MiB file / 64 MiB per-slide + total zip-bomb budget /
paragraph pagination). Functional check: e2e builds a real 3-slide .pptx
added OUT OF ORDER and pins numeric slide ordering (1<2<10) through
read_pptx_run + the read_document dispatch, plus zip-without-slides honest
error and allowlist denial. Haiku review: SHIP-AS-IS — 9 areas traced
(refactor safety, numeric ordering incl. layout/master/notes exclusion,
panic/zip-bomb bounds, honest degradation, dispatch ordering, additive),
two explicitly-non-blocking extreme-edge notes (2× budget overshoot worst
case, still bounded; slides.len() vs read on the byte-capped path, already
disclosed) — not applied (gold-plating a non-defect). **Lane 2 now covers
every common document type — CSV/PDF/XLSX/DOCX/PPTX + image + dispatch.**

**Runway:** the capability loop has now completed lanes 2 (fully), 3
(bar UI-owned R3.4), and 5, and drained the self-contained kitsu-tools
slices. Remaining roadmap work is run-layer (lane 4 in main.rs/
orchestrator, concurrently-edited) or already-covered lane 6. The loop
needs a repointed/gen-2 mandate — flagged repeatedly; the next self-
contained tick would have to invent non-roadmap tool hardening.

### Cycle 16 — 2026-08-15 — fs_read_notebook (.ipynb, extends doc review / lane 6)

Shipped `fs_read_notebook` (commit a647e6a): reads a Jupyter notebook as
STRUCTURE (per-cell type/source + one-line output summaries) instead of
the raw-JSON dump `read_document` previously produced. Output summaries
never emit the full payload — stream→line count, execute_result→text/
plain first line (120 chars) or mime keys, error→`ename: evalue` — so a
notebook full of base64 plots or long tracebacks stays legible. New
`DocType::Notebook` (`.ipynb`) wired through the dispatcher; nbformat 4;
non-notebook/malformed disclosed honestly; source as string OR line
array. Dep-free (serde_json), 32 MiB guard, cells/source-lines/outputs
all bounded + cap_output backstop. Notebooks are a very common
data-science/ML document that was previously opaque — this brings them
into the document-review lane and directly serves the deprioritized
lane 6 (scientific/data-doc review) as a sub-case of lane 2, no
domain-specific tooling needed. Functional check: e2e builds a real
.ipynb through read_notebook_run + the read_document dispatch + allowlist.
Security-focused haiku review: SHIP-AS-IS — 8 areas traced (no
full-payload leak, panic-freedom on adversarial JSON, real bounds,
honest degradation, additive), no findings.

**Runway note (unchanged, now firmer):** this and the recent
audit_summary / scan_secrets / pptx / notebook cycles are all
document-review/security-lane extensions at the self-contained margin —
genuinely valuable but beyond the numbered roadmap requirements, which
for the remaining lanes (1 R1.4/R1.5-wiring, 3 R3.4, all of lane 4) live
in the concurrently-edited run layer. The capability loop keeps
delivering self-contained tool value, but the numbered roadmap needs the
run-layer surfaces — a gen-2 repoint (a coordinated lane-4 mandate, or a
fresh backlog) is the standing recommendation.

### Cycle 17 — 2026-08-15 — fs_read_html (HTML → text, extends doc review)

Shipped `fs_read_html` (commit 72e0137): best-effort HTML → readable text
instead of raw tag soup. Removes `<script>`/`<style>`/comments WHOLESALE
at the raw byte level (so `a < b` inside a script never derails tag
parsing — the crux, pinned by a test), strips remaining tags, decodes
entities (XML five via quick_xml + numeric `&#N;`/`&#xH;` + common named
like `&nbsp;`/`&mdash;`), turns block tags into line breaks, collapses
whitespace. Manual byte-index scanner that stays UTF-8-safe by only ever
advancing to ASCII delimiters (`<`/`>`/`&`/`;`/end) — `i` is always on a
char boundary. New `DocType::Html` (`.html`/`.htm`) wired through
read_document. Dep-free, 32 MiB guard, cap_output backstop. HTML (web
pages, saved articles, emails) was a very common opaque format; now in
the document-review lane. Security-focused haiku review: SHIP-AS-IS —
8 areas traced (UTF-8/panic-freedom on adversarial input confirmed,
script-skip no-leak, entity safety, bounded, honest, sandboxed), no
findings (one acceptable note: pathological-unclosed-tag worst case is
O(n²) but bounded by the 32 MiB cap).

**Document-review lane now spans:** CSV, PDF, XLSX, DOCX, PPTX,
notebooks, HTML + image metadata + type-aware dispatch — essentially
every common text-bearing document a reviewer encounters. Runway note
unchanged: numbered-roadmap work now lives in the run layer / needs a
gen-2 repoint; the recent cycles are self-contained doc-review
extensions beyond the numbered requirements.

### Cycle 8 — 2026-08-14 — R1.8 debris warning made quote-aware (lane 1)

Lane 3 is done for this loop (R3.1/R3.2/R3.3; R3.4 is the UI loop's).
Advanced to lane 1 (code-assistant hardening) with R1.8 — a real
false-positive Kimi hit editing files that contain chat-template markers
as data. `content_quality_warning` now flags a marker only when it is
UNQUOTED (a leaked completion is bare; source quotes it), via
`has_unquoted_chat_template_marker`. Advisory, never blocks. 3 tests +
an e2e through write_file_run (commit dad6882). Another dogfooding
finding (Kimi cycle 16) fixed. Haiku review: SHIP-AS-IS — all 10 cases
traced (loop advance, byte-boundary safety on multibyte-adjacent markers,
mixed quoted/bare, and confirmed still-advisory — the write commits
regardless of the warning), no findings. Next lane-1: R1.3 symbol nav or
R1.4 grouped edit receipts; or the high-value R4.10 (technical read cap,
doubly-motivated by DeepSeek).

### Cycle 7 — 2026-08-14 — R3.3 verified done + R1.7 shipped (line-number prefixes)

R3.3 (structured command results) was found ALREADY fully implemented and
tested — command_exec serializes the whole CommandOutput (exit_code,
success, elapsed_ms, truncation flags) to JSON, non-zero-exit-is-Ok pinned
by tests — so it was checked off with citations, no code. R3.4 (runbook
mode) was DEFERRED: it needs surgery in main.rs's scripted-chat loop
(owned by the UI/main loop, concurrently edited) — not a safe low-cost
capability slice. Implementation budget went to **R1.7** (commit 51fc5cf):
ranged reads now carry absolute line-number prefixes (`42\t<code>`),
scoped to ranged reads only so fs_edit_file is unaffected. 4 tests
(prefixed ranged reads + full-read-no-prefix); live-verified a model
citing the exact line. This fixes the ~1-line citation drift that hit
DeepSeek every review cycle — the dogfooding's own finding, now shipped.
Haiku review: SHIP-AS-IS — all 10 cases traced, and the load-bearing
invariant confirmed (fs_edit_file uses a separate read path,
read_existing_text → read_snapshot_limited, so the prefix never reaches
edit-matching); cap accounting, oversized-line underflow guard, and
char-boundary safety all correct; no findings.
Next: R3.4 (for the UI/main loop) or lane 1/4 hardening.

### Cycle 6 — 2026-08-14 — R3.2 config drift audit (sysadmin lane)

Shipped `fs_diff_config` (commit 647f5a5): parses two line-oriented
configs and reports added/removed/changed keys + unchanged count,
handling `key value`/`key=value`/`key: value`, comments, case-insensitive
directives, and duplicates. Both paths authorized; parse off-reactor.
5 test scenarios incl. the sshd_config case-insensitive match; live-
verified on default-vs-hardened sshd_config (correct drift). Haiku review:
SHIP-AS-IS — all 10 edge cases traced (set-op correctness, value-contains-
delimiter, case-insensitivity, dup handling, both-paths-authorized,
byte-boundary/indexing safety, debug-format escaping); one very-low
comment/behavior mismatch on the double-delimiter case, fixed in a
follow-up (ae9f2ab). Next in lane 3: R3.3 structured command results,
R3.4 runbook mode.

### Cycle 5 — 2026-08-14 — R3.1 log scan/tail (opens the sysadmin lane)

Shipped `fs_scan_file` (commit 5cbc0d1): grep mode (streaming, line
numbers, matches-shown-of-total disclosure, 256 MiB scan cap) and tail
mode (true seek-to-end, last ≤4 MiB). Memory bounded regardless of file
size — the whole point, since fs_read_file only sees the first 64 KiB.
4 test scenarios; live-verified through ollama (tail returned the real
last lines of a 20k-line/868 KiB log; grep returned the correct match
count). Haiku review: SHIP-AS-IS — all 10 points traced incl. the
memory-bound property (streaming + Read::take cap + tail seek window) and
no tail-seek underflow; empty-file/no-newline/multibyte all graceful; no
findings. (One noted non-issue: ignore_case on a pathological 256 MiB
no-newline line doubles memory to ~512 MiB — still bounded, extreme
edge.) Next in lane 3: R3.2 config drift, R3.3 structured command
results, R3.4 runbook mode.

### Cycle 4 — 2026-08-14 — R2.4 xlsx extraction (lane 2 effectively complete)

Shipped `fs_read_xlsx` (commit 63a139a) via pure-Rust `calamine`: sheet
names + a bounded cell window (sheet by name/0-based index, start_row/
max_rows, 500 ceiling, empty cells blank), wired into `read_document`'s
XLSX branch as real extraction. 32 MiB guard, same allowlist. Tests use
an embedded real .xlsx fixture (include_bytes!, no writer dep); the
read_document routing test now extracts xlsx and still declines docx.
Live-verified through ollama qwen2.5-coder both directly and via
dispatch. Haiku review: SHIP-AS-IS — all 10 edge cases traced including
the security property (authorize before any file op; the canonical
authorized path — not the raw user arg — flows through to calamine, no
TOCTOU), acceptance signal met, no findings. **Lane 2 now covers CSV,
PDF, xlsx extraction + type-aware dispatch — the common document types.
Remaining: R2.3 image metadata (opportunistic) and optional docx text.
Next: lane 3 (system administration), R3.1 log scanning.**

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
