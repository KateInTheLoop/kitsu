# Baseline review of the five dogfood fixes

Reviewed range: `513151a..dead112` on `dogfood/top10-improvements`.

## What changed

- Added commit/date provenance to long version output and `/status`.
- Made timeout and filesystem policy errors more actionable.
- Made `--allow-write` register and authorize reads under the same root.
- Added a non-TTY inference heartbeat and raised the default total inference deadline from 60 to 120 seconds.
- Added advisory file-content warnings for leaked chat-template markers and suspicious literal `\\n` content.
- Clarified that current `--yes` behavior removes filesystem confinement once a tool is registered.

## Validation

- `cargo test --workspace --all-targets -- --test-threads=1`: **362 passed**.
- `cargo clippy --workspace --all-targets -- -D warnings`: **passed**.
- `git diff --check main..HEAD`: **passed**.
- Repository Hugging Face token scan: **no match**.
- `cargo fmt --all -- --check`: **failed** in two mechanical locations (`main.rs` and `kitsu-tools/src/builtins/fs.rs`).

## Review findings

1. **High: dirty build provenance is stale under incremental builds.** `build.rs` watches `.git/HEAD` and `.git/index`, but a normal unstaged or untracked change updates neither. Reproduction: build clean, add an untracked repository file, rebuild, and observe `kitsu 0.1.0 (dead112, built 2026-08-10)` without `-dirty` while `git status` reports the file.
2. **Medium: the displayed build date is actually the commit date.** `git log -1 --format=%cs` cannot establish when the binary was compiled. Rename it to source/commit date or embed an actual reproducible build timestamp.
3. **Critical contract decision: `--yes` removes root confinement.** The new warning is honest, but the familiar flag name remains dangerously surprising. A safe default should auto-approve only within explicit roots, with a separately named global bypass.
4. **High: `--allow-write` now broadens read confidentiality and docs disagree.** Runtime behavior implies read, while README option/reference prose still calls read/write roots independent. Either retain independent capabilities with an explicit combined flag, or document and test the new confidentiality contract consistently.
5. **Medium: the warning reinjects a raw special token.** The warning text contains `<|im_start|>` and is returned in the tool result seen by the model. Refer to a generic “chat-template marker” without echoing a recognized special token.
6. **Medium: plain-mode structural output contains `⚠`.** The warning formatter adds a Unicode structural glyph not mapped by the plain sanitizer, contradicting ASCII-only structural output.
7. **Medium: timeout actionability is mode-dependent.** Interactive chat uses the new deadline/remedy formatter, while `ask`, piped chat, and JSONL still reduce it to `request timed out`.
8. **Low: heartbeat implementation lacks an output-level timing/cancellation regression.** Current tests choose the mode but do not prove first-beat cadence, repeated cadence, suppression on fast turns, or task cleanup.
9. **Metadata: all five commits use Kate as author and committer, but none includes the optional GPT 5.6 coauthor trailer requested for joint work.**

The branch was clean before audit reports were added. No newer local or remote ref existed, so a rebase at this point would have been a no-op.
