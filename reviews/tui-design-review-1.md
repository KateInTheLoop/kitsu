# Review 1 — `docs/tui-and-agent-view-design.md` (v1 → v2)

Date: 2026-10-12. Reviewer: an independent agent given only the document and
the repository. Every finding below was re-verified against the working tree
before disposition; "Evidence" is what the verifier checked, not what the
reviewer claimed.

| # | Finding (short) | Severity | Evidence checked | Disposition |
|---|---|---|---|---|
| 1 | B4 claimed a `phase: "blocked"` JSONL value; the only record is `background_result` with a closed `status` set, emitted for terminal results only, and B1 makes blocked unreachable under JSONL; also contradicted the "no JSONL change" non-goal | high | `main.rs` `background_result_payload` (`status` ∈ done/failed/cancelled/running); `docs/jsonl-protocol.md` §Envelope (additive events OK, meaning change = new schema) | **Applied.** B4 now states JSONL is unchanged; any future surfacing must be a new optional event type. Non-goal reworded. Open question 7 added. |
| 2 | `Blocked` breaks `AgentThreadSnapshotDto::validate()` (terminal derived from `phase != Running`), closed serde enums, `deny_unknown_fields`, and there are three status vocabularies | high | `thread.rs` `AgentThreadPhase`, `validate()`, `AGENT_THREAD_SCHEMA_VERSION = 1`; `subagent.rs` `AgentStatus`, `AgentProgressStatus`; `main.rs` `debug_assert!(validate_default())` | **Applied.** New B5 enumerates the three enums, the new lifecycle rules, schema bump to 2, optional `name`. |
| 3 | Answer channel specified in tool handlers; the deny actually happens in `Permissions::resolve` and `command.rs` `CommandMode::Ask`, gated by a `task_local!` bool; `Prompter::ask` is sync via `spawn_blocking`; no single prompt lock | high | `permissions.rs` `Mode::Ask` branch, `prompt()`; `kitsu-tools/src/lib.rs` `INTERACTIVE_PROMPTS_ALLOWED`, `scope_without_interactive_prompts`; `command.rs` gate | **Applied.** §4.4 now specifies a task-local `PromptRoute` replacing the bool, consulted by both gates; B2/B3 rewritten; glossary entry. |
| 4 | `Question` kind contradicts `SUBAGENT_SYSTEM` ("Do not ask questions"); no ask-user tool exists | medium | `subagent.rs` `SUBAGENT_SYSTEM` | **Applied.** Stage 1 scoped to permissions; free-form questions moved to non-goals + open question 5. |
| 5 | `Ctrl+B` is backward-char and `Ctrl+Z` is Suspend in `TypeaheadEditor`; attached Ctrl+C conflicted with the draft-aware rule | medium | `typeahead_editor.rs` `0x02 => move_left`, `0x1a => Suspend`, `0x03` draft rule; `0x00..=0x1f` catch-all shows Ctrl+X/O/L unbound | **Applied.** Prefix is `Ctrl+X`; `Ctrl+Z` removed from detach; attached Ctrl+C = clear draft / detach, never stop. |
| 6 | Row glyphs `◆ ◇ ◈` collide with tool-call/result/receipt meanings and plain-mode `tool>`/`result>`/`#` mappings; `/profiles` already uses `◆/◇` | medium | `docs/ux-design.md` glyph table and agent-deck entry; `lib.rs` `ascii_structural_markers` | **Applied.** Rows use state words; only `◉` for blocked; rule to update `ux-design.md` + `ascii_structural_markers` together. |
| 7 | N1 listed `--reduced-motion` as byte-identical but its gate did not exclude it; reduced motion keeps styling | medium | `cli.rs` `reduced_motion` doc; `lib.rs` `reduced_motion_mode()` private, not in `stderr_styled()` | **Applied.** Removed from N1; new N9 defines TUI behavior under reduced motion. |
| 8 | Dropping rustyline loses `ChatHelper` completer (`attachments::complete`, slash palette), hinter, highlighter; mid-turn Tab only accepts the slash hint; `↑`-on-empty is queue recall | medium | `main.rs` `impl Completer for ChatHelper`; `CompleteRequest`/`RecallRequest` handling | **Applied.** `composer.rs` deliverable in §4.1/§4.3; R4 lists the re-hosted features; A1 notes `↑` recall. |
| 9 | Transcript is two fds (assistant → stdout, chrome → stderr); doc treated scrollback as one stream and did not say which fd carries cursor control | medium-high | `lib.rs` `io::stdout()` writes for fragments; 28 `eprintln!` vs 0 `println!`; `live.rs` header | **Applied.** R3 is per fd; §4.2 specifies stderr for control, own fd for text, same interleaving as `with_live_row_suspended`. Risk row added. |
| 10 | N4/Stage 4 assumed an on-disk rendered session log; `SavedSession` stores messages only | medium | `session.rs` `SavedSession` fields | **Applied.** Cell ring (N4) replaces the log; `[` dumps the ring; `/export` for evicted content. |
| 11 | `kitsu agents --json` cannot see an in-process registry; no `agents` subcommand; DTOs never persisted; no socket allowed | high | `cli.rs` subcommand list; grep for DTO persistence in `session.rs` (none) | **Applied.** H1 reads the H2 per-process state file (now carries the DTO array); socket is open question 6. |
| 12 | Contradicts `docs/claude-code-comparison.md` §8 "don't trade [the REPL] for an alt-screen" without superseding it | medium | `claude-code-comparison.md` §8 bullet | **Applied.** "Supersedes" note in the header explaining the preserved rationale; that bullet amended in the same change. |
| 13 | "a sidebar that never hides" vs overlay design | low | internal | **Applied.** Take reworded to "state list one chord away plus always-visible counter". |
| 14 | `/agents view` collides with the `/agents` alias of `/jobs`; `/agent` (singular) not mentioned | low-medium | `main.rs` `SLASH_COMMANDS` entries for `agent`, `agents`; `/jobs` usage string | **Applied.** `view|explain|reply` are `/jobs` subcommands (alias inherits); `/agent reply` added; §2.1 lists `/agent`. |
| 15 | `[tui]/[agents]/[hooks]` tables vs flat `deny_unknown_fields` `Config` | low | `config.rs` `Config` | **Applied with a correction.** Tables kept as optional nested structs; the reviewer's "flatten" suggestion does not help forward-compat because any new key is rejected by `deny_unknown_fields`; noted in §4.8 and risks. |
| 16 | Stage 2 acceptance not testable: PTY harness runs `--plain --no-stream`; N2 has no benchmark | medium | `tests/cli.rs` `run_plain_interactive_chat_pty`; `ui-parity-plan.md` manual verification note | **Applied.** Styled streaming PTY harness is a Stage 2 deliverable; N2 is a reported benchmark, not a CI gate. |
| 17 | Ambient counter needs `blocked`/`unacknowledged`, absent from `AgentRegistrySnapshot` | low | `subagent.rs` `AgentRegistrySnapshot { generation, running, terminal, failed }` | **Applied.** Added to §4.4 and Stage 0. |
| 18 | Timeline has no live notification; `attach` name already taken; no approval timeline kind | low | `thread.rs` `AgentTimeline` (bounded buffer, mpsc only for steering); `subagent.rs` private `fn attach` | **Applied.** `tail_timeline` with capacity/lag policy; `AgentTimelineKind::Approval`. |
| 19 | A6 charged human dispatch to the model's per-turn `SpawnBudget` | low | `config.rs` `agent_spawn_cap` doc; `subagent.rs` `AgentRegistryLimits` | **Applied.** Dispatch admitted by registry limits. |
| 20a | R7 option line text wrong | low | `lib.rs` `[y] allow once · [a] always (<dir>) · [n] deny` | **Applied.** |
| 20b | Claude Code `Ctrl+B` claim unverified in repo | low | Verified externally: Claude Code interactive-mode docs list `Ctrl+B` "Background running tasks … Tmux users press twice" and the `Ctrl+X` chords | **Kept, cited** in §2.2 and §9. The prefix choice no longer depends on it. |
| 20c | N7 assigned to no stage | low | internal | **Applied.** Stage 0 and 4 cite N7. |
| 20d | `docs/functionality.md` "Styled mode uses a redraw-safe external printer" is stale | low | `main.rs` ~16401 (printer opt-in via `KITSU_LIVE_NOTICES`) | **Applied** to `functionality.md` in this change and listed in §4.6/Stage 0. |
| 20e | "one-row live region" understated | low | `live.rs` `previews` (up to three rows) | **Applied.** |

Rejected or narrowed: none rejected outright. Finding 15's suggested fix was
narrowed (see row). Finding 20b was a repo-only verification gap, closed with
the external source.

Reviewer verdict was **revise**; v2 addresses every item. The architecture
(inline viewport + native scrollback, blocked phase, agent view overlay) is
unchanged; the specification of *where* each piece plugs into existing code is
what moved.
