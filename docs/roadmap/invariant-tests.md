# Improvement invariant test manifest

Status: E00-S02 baseline manifest at `a2ba2d5`

This manifest maps the strengths that the harness-derived roadmap must preserve
to exact existing provider-free test owners. It is a routing contract, not a
claim that the five replaceability boundaries already pass. Missing boundary
fixtures are listed explicitly below.

Use a writable disposable target outside the source tree when isolation from
another worktree matters:

```bash
export CARGO_TARGET_DIR=/tmp/kitsu-harness-roadmap-target
```

The CLI integration suite uses local loopback wiremock servers. A sandbox must
allow loopback bind; it does not require internet access, credentials, or a live
model provider.

## Per-card and epic gates

Every production card runs:

1. the new/changed test and the row(s) below affected by the slice;
2. `cargo check` or Clippy for every affected crate/target;
3. `cargo fmt --all -- --check`;
4. `git diff --check` on the complete staged change.

Every epic exit runs:

```bash
cargo test --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
git diff --cached --check
```

Milestone/rebase/merge acceptance additionally re-runs the JSONL schema/example
checks and every platform-relevant live containment test. A skipped platform
gate must be reported; absence of a backend is never counted as containment.

## Preserved invariant routing

| Invariant | Exact existing evidence | Targeted command/filter | Required when |
|---|---|---|---|
| Read authority is deny-by-default and root-bounded | `crates/kitsu-tools/src/permissions.rs`: `read_outside_root_is_denied_in_deny_mode`, `read_inside_root_is_allowed` | `cargo test -p kitsu-tools permissions::tests::read_` | Any filesystem, attachment, skill, context, MCP, or host-tool change |
| Write authority does not imply read authority | `permissions.rs`: `write_root_does_not_imply_read_authority`; CLI `test_allow_write_registers_only_write_capability` | `cargo test -p kitsu-tools write_root_does_not_imply_read_authority`; `cargo test -p kitsu --test cli test_allow_write_registers_only_write_capability -- --exact` | Any permission, registry, prompt, or tool-source change |
| Protected state overrides broad roots and auto-allow | `permissions.rs`: `protected_write_overrides_broad_root_and_auto_allow`, `private_kitsu_state_is_hidden_inside_an_allowed_root` | `cargo test -p kitsu-tools protected_write_overrides_broad_root_and_auto_allow`; `cargo test -p kitsu-tools private_kitsu_state_is_hidden_inside_an_allowed_root` | Filesystem, session, credential, artifact, skill, or bypass changes |
| Symlink/hard-link and ancestor races fail closed | `permissions.rs`: `dangling_symlink_write_escape_is_denied`, `hard_link_alias_of_a_protected_file_is_never_readable_or_writable`; `fs.rs`: pinned ancestor and checked replace tests | `cargo test -p kitsu-tools symlink_tests`; `cargo test -p kitsu-tools atomic_checked_replace_refuses_a_changed_file_and_cleans_up` | Any path authorization, read/write, export, attachment, store, or artifact change |
| Runtime grants reset while invocation grants remain | `permissions.rs`: `session_reset_revokes_runtime_grants_but_keeps_cli_roots`; `main.rs`: `session_switch_drops_live_command_grants_but_restores_cli_pregrants` | `cargo test -p kitsu-tools session_reset_revokes_runtime_grants_but_keeps_cli_roots`; `cargo test -p kitsu session_switch_drops_live_command_grants_but_restores_cli_pregrants` | Session new/resume/fork/switch, store, agent, or host work |
| Background work cannot race the interactive prompter | `permissions.rs`: `background_scope_denies_without_invoking_interactive_prompter`; `command.rs`: `background_scope_never_calls_the_interactive_command_prompter` | `cargo test -p kitsu-tools background_scope` | Subagent, MCP, supervisor, cancellation, or authority changes |
| Commands are separately authorized and exact | `command.rs`: `arguments_are_literal_and_never_shell_parsed`, `session_approval_is_exact_for_argv_and_canonical_cwd`, `allow_once_does_not_create_a_session_grant` | `cargo test -p kitsu-tools builtins::command::tests::arguments_are_literal_and_never_shell_parsed`; `cargo test -p kitsu-tools session_approval_is_exact` | Command, supervisor, MCP, host-tool, or approval changes |
| Missing command containment denies before approval/spawn | `command.rs`: `missing_containment_backend_is_typed_and_denied_before_approval` | `cargo test -p kitsu-tools missing_containment_backend_is_typed_and_denied_before_approval` | Command/supervisor/platform changes |
| Linux containment blocks external files, network, host process view, and secrets | `command.rs`: `live_containment_blocks_outside_reads_network_host_processes_and_secrets` | `cargo test -p kitsu-tools live_containment_blocks_outside_reads_network_host_processes_and_secrets` | Command/supervisor/MCP epic exit on Linux; requires usable Bubblewrap |
| Command cancellation kills and reaps descendants | `command.rs`: `dropping_caller_future_kills_and_reaps_child`, `timeout_kills_and_reaps_child_and_returns_metadata`, `dropping_runtime_immediately_after_abort_still_reaps_child` | `cargo test -p kitsu-tools kills_and_reaps`; `cargo test -p kitsu-tools still_reaps_child` | Command, supervisor, MCP, terminal-phase, or cancellation changes |
| Configuration is strict and parse errors are secret-safe | `config.rs`: `unknown_auth_key_is_rejected_with_field_names_only`, `artifact_storage_false_parses_and_misspelling_fails_closed`, `parse_error_retains_location_but_never_source_excerpt` | `cargo test -p kitsu config::tests` | Config, provider, onboarding, MCP declaration, or capability changes |
| Init is no-clobber and default doctor is read-only | CLI: `test_init_is_no_clobber_and_doctor_is_read_only_json`, `test_empty_script_does_not_require_writable_home` | `cargo test -p kitsu --test cli test_init_is_no_clobber_and_doctor_is_read_only_json -- --exact`; `cargo test -p kitsu --test cli test_empty_script_does_not_require_writable_home -- --exact` | Config/onboarding/doctor changes |
| Credentials are bounded, isolated, provider-free to manage, and never disclosed | `credential_store.rs`: bounded decode and collision-free gateway namespace; CLI managed-auth, hard-link, command-root, and secret-output tests | `cargo test -p kitsu credential_store::tests`; `cargo test -p kitsu --test cli managed_auth`; `cargo test -p kitsu --test cli test_doctor_never_echoes_static_or_malformed_config_secrets -- --exact` | Auth, provider, doctor, onboarding, JSONL, command env, or receipts |
| Tool/provider/context work is locally bounded before send | `orchestrator_tests.rs`: exact tool-schema preflight, protected token/byte overflow zero-send, round/repeat bounds; provider response and catalog hard ceilings | `cargo test -p kitsu-core request_preflight`; `cargo test -p kitsu-core protected_token_overflow`; `cargo test -p kitsu-providers hard_ceiling` | Registry, provider, context, multimodal, output-schema, or MCP changes |
| A failed event sink prevents later provider send or tool mutation | `orchestrator_tests.rs`: `failed_live_event_sink_stops_before_provider_send`, `failed_live_event_sink_stops_before_tool_mutation` | `cargo test -p kitsu-core failed_live_event_sink` | Run/outcome, JSONL, renderer, host, MCP, or tool changes |
| JSONL schema keeps a versioned bounded envelope | `kitsu-render/src/jsonl.rs`: `published_schema_keeps_command_payload_and_event_definitions_distinct`; CLI versioned/framed/schema tests | `cargo test -p kitsu-render published_schema`; `cargo test -p kitsu --test cli test_jsonl_ask_is_versioned_framed_and_parseable -- --exact` | Any event, outcome, receipt, provider, tool, session, or automation change |
| Provisional assistant output is discarded after stream failure | CLI: `test_jsonl_partial_stream_failure_discards_provisional_assistant_output`; orchestrator structured event tests | `cargo test -p kitsu --test cli test_jsonl_partial_stream_failure_discards_provisional_assistant_output -- --exact`; `cargo test -p kitsu-core structured_event_log` | Provider, run/outcome, JSONL, retry, or streaming changes |
| JSONL output failure is non-success and stops cleanly | CLI: `test_jsonl_output_failure_on_dev_full_is_nonzero_and_actionable`; unit `a_broken_output_pipe_is_not_a_crash` | `cargo test -p kitsu --test cli test_jsonl_output_failure_on_dev_full_is_nonzero_and_actionable -- --exact`; `cargo test -p kitsu a_broken_output_pipe_is_not_a_crash` | JSONL writer, host, outcome, delivery, or exit changes |
| Failed turns roll conversation back and disclose retained file effects | CLI: `test_failed_script_turn_is_transactionally_rolled_back`, `test_rolled_back_turn_briefs_next_turn_about_retained_file_changes`; core cancelled-terminal tests | `cargo test -p kitsu --test cli test_failed_script_turn_is_transactionally_rolled_back -- --exact`; `cargo test -p kitsu --test cli test_rolled_back_turn_briefs_next_turn_about_retained_file_changes -- --exact`; `cargo test -p kitsu-core dropping_in_flight` | Orchestrator, run/outcome, provider, session, tool, steering, or cancellation changes |
| Closed authority input interrupts before effects or later sends; explicit denial remains recoverable | Tool permission and command-policy interruption tests; core `authority_input_closure_interrupts_before_a_later_provider_send`; CLI confirmation-EOF and explicit-denial PTY tests | `cargo test -p kitsu-tools authority_input`; `cargo test -p kitsu-core authority_input_closure`; `cargo test -p kitsu --test cli test_interactive_confirmation_eof_interrupts_without_followup_send -- --exact`; `cargo test -p kitsu --test cli test_interactive_explicit_denial_remains_provider_recoverable -- --exact` | Permission/command prompt, tool error, orchestrator, run/outcome, terminal input, or carrier changes |
| Human completion exposes the neutral run identity, outcome, and assistant disposition without renderer inference | Core canonical-label tests; renderer fixed projection vectors; CLI sequential human identity, success/provider-fault, and human/JSONL tool-policy parity fixtures | `cargo test -p kitsu-core --test run_contract`; `cargo test -p kitsu-render run_timing_is_compact`; `cargo test -p kitsu --test cli test_model_command_timing_receipt_and_jsonl_summary -- --exact`; `cargo test -p kitsu --test cli test_jsonl_strict_script_separates_local_tool_and_policy_failures -- --exact` | Run types/labels, CLI failure/disposition classification, renderer completion receipt, JSONL outcome oracle, or carrier changes |
| Terminal phases are monotonic and later failures cannot rewrite settlement | Core lifecycle tests cover ordered publication, delivery/cleanup/persistence failure independence, no-mutation duplicate/out-of-order rejection, single publication, and post-terminal work rejection | `cargo test -p kitsu-core --test run_contract` | Run lifecycle/phase/state changes or any carrier finalization migration |
| Session snapshots exclude credentials and use fresh fork identity | `session.rs`: `saves_loads_and_lists_without_credentials`, `fork_uses_a_fresh_identity_and_preserves_snapshot_provenance` | `cargo test -p kitsu session::tests::saves_loads_and_lists_without_credentials`; `cargo test -p kitsu fork_uses_a_fresh_identity` | Store, revision/HEAD, fork, migration, export, or auth changes |
| Session leases and path lifecycle fail closed | `session.rs`: `resume_refuses_a_session_leased_by_another_handle`, symlinked snapshot/session/lock tests, lifecycle ancestor-swap tests | `cargo test -p kitsu session::tests::resume_refuses_a_session_leased_by_another_handle`; `cargo test -p kitsu symlinked_session`; `cargo test -p kitsu lifecycle_operations_ignore_ancestor_swaps` | Store, revision, lifecycle, fork, archive, or alternate adapter changes |
| Host output installation is exact, atomic, private, and no-clobber where promised | `session.rs`: `output_file_write_is_exact_atomic_and_replaces_only_a_regular_target`, export no-clobber/symlink tests | `cargo test -p kitsu output_file_write_is_exact_atomic`; `cargo test -p kitsu export_is_human_readable_atomic_and_no_clobber` | `-o`, export, store, file adapter, or completion changes |
| Model file edits require exact preconditions and reject concurrent change | `fs.rs`: `edit_file_refuses_missing_or_ambiguous_preconditions_without_writing`, `write_file_requires_an_exact_precondition_for_replacement`, atomic checked replace tests | `cargo test -p kitsu-tools edit_file_refuses_missing_or_ambiguous`; `cargo test -p kitsu-tools write_file_requires_an_exact_precondition`; `cargo test -p kitsu-tools atomic_checked_replace` | Edit/write, MCP file tools, context repair, or code workflow changes |
| Undo is conflict-aware and never hides intervening changes | `fs.rs`: later user edit, metadata-only change, recreated inode, and multi-file rollback tests | `cargo test -p kitsu-tools journal_undo_refuses`; `cargo test -p kitsu-tools multi_file_undo_rolls_back` | Journal/checkpoint/undo/edit/store changes |
| Skills are data-only, bounded, and grant no filesystem authority | `skills.rs`: no-project discovery, resource bounds/framing/drift, `load_skill_never_grants_a_symlinked_target` | `cargo test -p kitsu skills::tests`; focused `cargo test -p kitsu load_skill_never_grants_a_symlinked_target` | Skills, bundles, context, MCP declarations, onboarding, or session changes |
| Optional tool and agent surfaces disappear when disabled | CLI: `test_chat_no_tools_omits_every_model_tool`, `test_zero_spawn_cap_unregisters_agent_tools_from_slash_and_provider`; built-in registration separation tests | `cargo test -p kitsu --test cli test_chat_no_tools_omits_every_model_tool -- --exact`; `cargo test -p kitsu --test cli test_zero_spawn_cap_unregisters_agent_tools_from_slash_and_provider -- --exact` | Registry, provider body, MCP, subagent, tool tier, or disabled-path changes |

## Known missing conformance evidence

The following cells are deliberately **not** satisfied by the tests above:

| Boundary | Missing fixture/evidence | Owning roadmap epic |
|---|---|---|
| Neutral run-contract coupling | `scripts/check-run-boundary.sh` rejects CLI/session, provider/tool/renderer, serde/JSONL, Tokio, environment/filesystem/I/O/process/time dependencies in `kitsu-core/src/run.rs`; an alternate-source negative fixture proves the guard fails. | `bash scripts/check-run-boundary.sh`; E02 |
| Cross-cutting core coupling | The new negative check is deliberately scoped to `run.rs`. Current `kitsu-core::orchestrator` still depends on the concrete tool registry and `kitsu-render`; broader automated replacement checks remain missing and new integrations must not worsen that dependency. | E10 |
| X001 model/provider | A primary-validated distinct compiled dialect, immutable route generation across every send/auxiliary path, same-label mutation/retry/replay/withdrawal/join, and zero concrete wire imports in core | E04 |
| X002 tool/source | Fallible deterministic admission first, then frozen source identity, authority before acquisition/import/spawn, bounded supervisor, atomic withdrawal, incarnation/no-replay, disabled parity, and built-in plus real source conformance | E01 and E06 |
| X003 host/protocol | A real named second host/principal plus identity-preserving control/event/outcome port, version/delivery/auth/backpressure negotiation, disconnect behavior, and shared carrier suite | E02 and future named-host proposal |
| X004 automation/runtime | Common run/outcome types and provider-free ask/piped/script/JSONL carrier matrix covering settlement/join/persist/deliver/exit and zero post-terminal work | E02 |
| X005 persistence/session | Canonical opaque revision/logical HEAD, whole-turn publication, K01-K12 fault matrix, disposable projection rebuild, last-good recovery, and a named second store | E03 |

No epic may cite the broad workspace pass as closure of one of these missing
cells. The owning fixture must execute against both the default and named
independent adapter before the boundary can be called replaceable.

## Cycle 001 baseline receipt

At branch base `a2ba2d5`, before production improvement changes:

- `cargo test --workspace --all-targets`: exit `0` when local-loopback wiremock
  binding was permitted;
- `cargo clippy --workspace --all-targets -- -D warnings`: exit `0`;
- `cargo fmt --all -- --check`: exit `0`.

The initial sandboxed test run compiled but denied loopback binds and therefore
was not product evidence. No internet provider, credential, or billable request
was used by the accepted run.
