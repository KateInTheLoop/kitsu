# Kitsu Improvement Delivery Backlog

Status: active on `improvements/harness-roadmap`

Imported: 2026-08-17

Branch base: `a2ba2d5c98b17da7290a2b6b8b24c83bc2529763`

Cycle protocol: [`cycle-protocol.md`](cycle-protocol.md)

Invariant test routing: [`invariant-tests.md`](invariant-tests.md)

Mechanism learning ledger: [`mechanism-ledger.md`](mechanism-ledger.md)

Post-E01 evidence baseline: [`post-e01-evidence-baseline.md`](post-e01-evidence-baseline.md)

Current architecture: [`current-architecture.md`](current-architecture.md)

Current comparison: [`current-comparison.md`](current-comparison.md)

Program status: [`program-status.md`](program-status.md)

## Purpose and baseline rule

This is the Kitsu-owned implementation backlog translated from the ten-harness
research. It defines milestones, epics, user stories, dependencies, acceptance
evidence, and the 30-minute delivery loop. Accepted work is recorded under
[`cycles/`](cycles/).

The research pin is `6c713333`. Committed Kitsu `main` was `a2ba2d5` when this
backlog was written, 67 commits ahead of that pin. The live Kitsu worktree also
had ten unrelated modified files. Planning may inspect them for collision risk,
but no story may treat them as shipped behavior or absorb them into the future
branch without an explicit ownership decision.

## Product outcome

Kitsu remains a small, local-first Rust CLI with explicit authority, mandatory
command containment, offline/auditable configuration, bounded model and tool
work, versioned automation, safe sessions, and exact edits. The program adds
replaceable integrations at five typed boundaries without changing those
defaults or creating a universal plugin system.

The word “user” below includes interactive users, automation operators,
maintainers, and adapter authors. A maintainer-facing story is valid only when
it protects an observable product property or removes demonstrated coupling.

## Preserved invariants

Every epic inherits these acceptance requirements:

- Filesystem reads, writes, commands, credentials, skills, and host controls
  remain separate authority domains.
- Commands remain disabled by default and fail closed without supported OS
  containment. Approval is not treated as containment.
- Config discovery remains offline and performs no implicit package, network,
  credential, or state mutation.
- Sessions never restore runtime grants, credentials, executables, or child
  processes.
- Failed and cancelled turns keep Kitsu's conversation rollback and truthful
  retained-effect reporting.
- JSONL remains versioned, identity-bearing, fail-stop, and consistent with
  process exits `0/1/2/130`.
- Optional integrations create no child, network request, state write, schema
  overhead, or startup delay while disabled.
- Model and tool outputs, context, events, pipes, files, time, retries, and
  shutdown are bounded.
- New stores, providers, sources, and hosts adapt to Kitsu-owned types. The core
  never imports their concrete protocol, config, credential, or persistence
  types.

Current core dependencies on the concrete tool registry and `kitsu-render` are
baseline coupling, not proof of the target host/tool ports. E02/E10 may extract
them only when an executed fixture proves the boundary.

## Architectural thesis, milestones, and dependency map

The program borrows mechanisms rather than plugin models. Its three integration
spines are immutable provider generations, a canonical session revision/HEAD,
and a bounded child supervisor. The detailed source-by-source transfer and
rejection decisions live in the [mechanism learning ledger](mechanism-ledger.md).

| Milestone | Epics | Outcome |
|---|---|---|
| M0 — trustworthy starting point | E00-E01 | Current evidence pin, regression envelope, Ollama doctor fix, deterministic registry admission |
| M1 — causal execution foundation | E02 | One identity-bearing run outcome across carriers and terminal phases |
| M2 — immutable provider spine | E04 | Compatible Chat plus one distinct compiled wire and one route generation across all sends |
| M3 — canonical session spine | E03 | Revisioned logical HEAD, recovery, compaction lineage, and safe branch-at-turn |
| M4 — bounded child spine | E06 | Private supervisor plus disabled-by-default, tools-only stdio MCP experiment |
| M5 — measured context and onboarding | E07, E05, E08 | Evidence-driven semantic context, transactional picker/setup, observability, and progressive docs |
| M6 — later media evidence | E09 | Bounded multimodal input only across truthful capability paths |
| M7 — replaceability certification | E10 | Boundary-by-boundary conformance decision with default-path parity and rollback evidence |

```text
E00 -> E01 -> E02
          \      \-> E04 -> E05 -> E08
           \      \-> E03
           \-------> E06 ------/
           \-------> E07
                    E04 -> E09
E01 + E02 + E03 + E04 + E05 + E06 + E07 + E08 + E09 -> E10
```

E02 may start before E01 only for provider-free fixture design; production
integration waits for deterministic registry admission. After E02, the serial
queue prioritizes E04's provider spine, then E03's session spine, then E06's
supervisor/MCP spine. E06 requires E01 and the cancellation/terminal parts of
E02, even though delivery priority keeps it after E03. E07 follows the three
spines; E05/E08 then use the stabilized provider and external-tool surfaces.
E09 requires two truthful capability paths from E04. No desktop, IDE, listener,
daemon, or control plane is invented solely to improve X003 breadth.

## Definition of Ready

A story is ready only when it has:

1. one user-visible or invariant-preserving outcome;
2. an owning epic and five-boundary mapping;
3. named source files or a bounded discovery card;
4. deterministic tests that do not require credentials or billable inference;
5. explicit authority, failure, compatibility, and rollback behavior;
6. no collision with uncommitted work in the selected implementation tree;
7. a decomposition into green, independently useful 30-minute cards.

## Definition of Done

A story is done only when:

- requirements and non-goals are recorded;
- the relevant test fails for the intended reason before implementation, unless
  the card is documentation/refactoring-only;
- implementation passes targeted tests and the story's invariant regressions;
- review checks authority, failure isolation, boundedness, versioning,
  observability, compatibility, and rollback;
- formatting and warning gates pass for the affected crates;
- machine contracts and docs are updated together when externally observable;
- every accepted card has a focused commit and cycle record;
- the epic exit suite passes before the epic is marked complete.

## E00 — Program baseline and safety envelope

Status: accepted through Cycle 010; the clean base, invariant routes, mechanism
ledger, content-pinned delta audit, current architecture/comparison/program
views, and cycle-receipt check are Kitsu-owned and mutually consistent.

Outcome: maintainers can prove which Kitsu revision is being changed and which
existing strengths every later story must preserve.

| Story | User story | Requirements | Acceptance evidence | Size |
|---|---|---|---|---|
| E00-S01 — clean implementation baseline | As a maintainer, I want the improvement branch based on one clean committed Kitsu revision so research and implementation claims are reproducible. | Resolve ownership of the ten dirty files; create `improvements/harness-roadmap` only after backlog approval; record base SHA, Rust version, tree status, and research delta; never move or discard user edits implicitly. | Branch/worktree is clean at creation; recorded SHA equals its merge base; dirty `main` remains byte-for-byte and status-for-status unchanged. | 1 cycle |
| E00-S02 — invariant test manifest | As a maintainer, I want one executable list of the authority, containment, JSONL, session, edit, credential, and cancellation regressions that gate every epic. | Map existing tests before adding new ones; distinguish per-card targeted gates from milestone workspace gates; name platform-specific skips/fail-closed behavior. | Every preserved invariant maps to at least one existing or explicitly missing test; commands run provider-free; manifest links source/test owners. | 1-2 cycles |
| E00-S03 — live delta audit | As a planner, I want research rebased from `6c713333` to the branch base so completed work is not scheduled twice. | Pin the non-versioned research inputs without rewriting their frozen/historical claims; audit all five boundaries and candidate rows; identify committed retry/context/tool/recovery changes; keep source-backed and runtime-observed labels distinct. | Kitsu-owned evidence baseline, current architecture, gap status, backlog, and program status agree on the same pin; frozen research inputs retain content hashes; audit checks pass. | 2-4 cycles |
| E00-S04 — cycle records | As a reviewer, I want each 30-minute loop to leave a compact decision and verification receipt. | Use the cycle template below; keep raw output out of prose; record incomplete/rejected cards without a production commit. | A script or review check proves every improvement commit has one cycle record naming story, tests, result, and next card. | 1 cycle |
| E00-S05 — mechanism transfer ledger | As a maintainer, I want every comparative lesson translated into a narrow Kitsu mechanism or explicit rejection so ecosystem breadth cannot smuggle in a plugin model. | Map all ten sources to a gap, safe mechanism, and do-not-copy boundary; name the three spines, dependency-safe order, hard exclusions, and executable gates. | Ledger, epic requirements, milestone order, and cycle queue agree; all local links resolve; no source-backed claim is presented as executed Kitsu evidence. | 1 cycle |

Epic exit: branch baseline and research pin agree; preserved-invariant test
manifest and mechanism ledger exist; program audit and the baseline acceptance
commands pass.

## E01 — Bounded defects and deterministic tool admission

Status: accepted through Cycle 008; Ollama alias truth, deterministic registry
admission, CLI propagation, descriptor/handler parity, and the ignored-result
CI gate are green on the improvement branch.

Outcome: local Ollama diagnostics are truthful and tool composition cannot
silently publish duplicate or mismatched handlers/schemas.

Dependencies: E00-S01 and E00-S02.

| Story | User story | Requirements | Acceptance evidence | Size |
|---|---|---|---|---|
| E01-S01 / C001 — Ollama alias truth | As a local Ollama user, I want `doctor --online` to accept the untagged model used by Kitsu's starter profile when Ollama advertises the same model as `:latest`. | Equivalence only for structurally local Ollama; untagged selected ID may match exactly one `:latest`; explicit tags and generic gateways stay exact; catalog check sends no chat request; human and JSON agree. | Five cases from `reports/kitsu-baseline.md`, plus ambiguity/empty-ID tests; exit `0` for the valid alias and `1` for genuine mismatch. | 1-2 cycles |
| E01-S02 / A001 — deterministic registry admission | As a maintainer, I want duplicate tool names rejected before publication so the model schema and invoked handler cannot diverge. | Both registration paths return `Result`; same-path and cross-path collisions fail before mutation; descriptor-free test tools remain supported; composition errors reach CLI before the first provider request. | Four duplicate-order tests preserve the first entry; descriptor/handler parity tests pass; a provider-free composition-boundary test proves the failure cannot contact a provider. | 2-3 cycles |
| E01-S03 — registry coupling guard | As an adapter author, I want CI to reject schema/handler drift and unhandled registration failures. | Check every advertised descriptor has exactly one handler; deny ignored registration results; no provenance, namespace, contributor framework, or public plugin API in this slice. | `RUSTFLAGS=-Dunused-must-use` check, focused registry tests, current capability guards, workspace check, Clippy, and format pass. | 1 cycle |

Epic exit: KB-001 is closed; A001 is integrated on the current base; no provider
or authority behavior changes except truthful preflight failure.

## E02 — One run identity and causal terminal outcome

Fixture contract: [`e02-carrier-matrix.md`](e02-carrier-matrix.md). Neutral type
contract: [`e02-run-contract.md`](e02-run-contract.md).

Status: E02-S01 fixture discovery is accepted through Cycle 014. Cycles 015-016
establish neutral identity/event/outcome values and propagate typed authority
interruption through filesystem, command, tool, and loop control. Confirmation
EOF now causes zero later provider sends or effects while explicit denial stays
recoverable. Human carriers still expose no stable run identity.

Outcome: interactive, ask, piped, script, and JSONL carriers expose the same
semantic run result even when delivery, cancellation, cleanup, or persistence
fails.

Dependencies: E00; production types after E01.

| Story | User story | Requirements | Acceptance evidence | Size |
|---|---|---|---|---|
| E02-S01 — carrier fixture | As an automation operator, I want identical semantic outcomes across Kitsu entrypoints so I never infer success from EOF or presentation. | Provider-free scripted provider/tool fixtures; sequential same-session runs; confirmation EOF; provider/tool/preflight fault; cancel before/active/queued; blocked/EPIPE sink; cleanup residue; duplicate/cross-run terminal; exact exits. | One table asserts identity, class, terminal count, committed/discarded state, persistence, delivery, and `0/1/2/130` for every carrier. | 3-5 cycles |
| E02-S02 — private run contract | As a host adapter author, I want `RunSpec`, identity-bearing `RunEvent`, and `TerminalOutcome` owned below presentation so carriers cannot reclassify results. | Extract only fixture-proven divergence; core has zero CLI/env/signal/exit/store dependencies and zero renderer/JSONL types; identity assigned before acceptance. | Compile-time coupling check plus existing behavior fixtures; changing carrier changes no agent-loop source. | 4-6 cycles |
| E02-S03 — terminal phase truth | As an operator, I want to distinguish semantic settlement, child/background join, persistence, delivery, and process exit. | Monotonic `settled -> joined -> persisted -> delivered`; delivery failure never rewrites semantic settlement; typed cleanup residue; zero work after terminal. | Fault injection at each phase yields one terminal and truthful phase receipts; duplicate publication is rejected. | 3-4 cycles |
| E02-S04 — JSONL projection parity | As a JSONL consumer, I want `kitsu.chat.v1` to remain compatible while projecting the common outcome. | Additive fields only unless a separately approved version bump; writer remains fail-stop; exact assistant disposition and final summary preserved. | Schema/examples validate; old fixtures parse; human and JSONL exits/outcomes agree; blocked writer stops later provider/tool work. | 2-3 cycles |

Epic exit: X004 can be evaluated with an independent deterministic harness.
X003 remains “prepared,” not product-certified, until a real second host/principal
exists; the fixture is not misrepresented as product demand.

## E03 — Canonical session revision, recovery, and branch-at-turn

Outcome: every restart-authoritative session change has one atomic revision and
logical HEAD; users can recover and branch without restoring authority or
cutting an incomplete exchange.

Dependencies: E02 terminal outcome model.

| Story | User story | Requirements | Acceptance evidence | Size |
|---|---|---|---|---|
| E03-S01 — canonical revision ADR | As a maintainer, I want one precise owner for transcript, terminal outcome, selected head, resume metadata, lineage, compaction binding, and artifact references. | Separate logical schema, physical store, projection/index, and export versions; define opaque revision, expected-head conflict, corrupt/future states, commit cut, migration, rollback. | ADR maps every restart-authoritative field to exactly one revision and every cache/index to disposable projection; no implementation ambiguity remains. | 2-3 cycles |
| E03-S02 — current file-store CAS | As a concurrent session user, I want stale writers rejected instead of overwriting a newer complete session. | Lease/fence plus expected revision recheck at publish; staged no-overwrite install; one revision contains a whole turn and its terminal outcome. | Stale writers receive typed conflict; crash cuts yield exactly old or new revision; current session fixtures remain compatible. | 4-7 cycles |
| E03-S03 — K01-K12 fault matrix | As an operator, I want session behavior proven at prompt, tool, terminal, compaction, fsync, artifact, and HEAD-publication boundaries. | Inject every specified cut; corrupt/future data fails closed; projection deletion/rebuild selects identical HEAD; migration/export/fork tested. | Full K01-K12 passes on current store and, before certification, one named adapter; no partial fork or mixed revision. | 5-8 cycles |
| E03-S04 — verified last-good recovery | As a user with a malformed newest snapshot, I want Kitsu to recover only to a fully verified earlier generation and explain what happened. | Never skip corrupt fragments within a canonical revision; recovery selection is deterministic; current corrupt artifact remains quarantined/recoverable; grants stay fresh. | Valid previous generation resumes with receipt; no previous generation returns typed corrupt; recovery cannot select a different logical lineage silently. | 3-4 cycles |
| E03-S05 — branch at committed turn | As a user, I want `sessions fork --at-turn N` so I can explore alternatives from an exact committed boundary. | Boundary validates complete user/tool/assistant exchange and artifact closure; lineage binds source ID/revision/HEAD; no overwrite; provider-free; fresh authority. | Valid boundary copies exact prefix/artifacts; mid-tool, missing-artifact, corrupt, stale, and destination-collision cases leave source/destination unchanged. | 4-6 cycles |
| E03-S06 — canonical compaction ledger | As a user, I want compaction to remain a validated projection of canonical history so resume/model switch cannot hide what was removed. | Bind compaction to source revision, selected model capability/tokenizer, policy, and HEAD; canonical history remains exportable; stale sidecar ignored with receipt. | Branch/retry/resume/model-switch tests preserve tool boundaries and rebuild or reject stale compaction deterministically. | 3-5 cycles |

Epic exit: X005 default store passes K01-K12. “Replaceable” still requires a
named second store adapter through the same private port; a second JSONL/file
layout is sufficient, dynamic loading is not required.

## E04 — Truthful provider dialects and immutable route generations

Outcome: Kitsu retains generic Chat Completions while proving a second genuine
wire dialect behind the same Kitsu-owned port and capability contract.

Dependencies: E02 run outcome; E00 delta audit.

Recommended distinct dialect: Anthropic Messages, because current config and
documentation already name Anthropic while live construction still uses the
compatible-chat adapter. OpenAI Responses is an acceptable product decision
before implementation. An endpoint alias, Ollama profile, or compatible proxy
does not count as a distinct wire.

| Story | User story | Requirements | Acceptance evidence | Size |
|---|---|---|---|---|
| E04-S01 — primary wire fixtures | As an adapter maintainer, I want protocol fixtures derived from primary specifications so wire claims are testable without live credentials. | Request, streaming text, tool use/result, usage, errors, timeout, cancellation, unsupported image, and malformed response; bound all bodies/events. | Compatible Chat control and distinct fixtures fail against the wrong adapter and run fully offline. | 3-5 cycles |
| E04-S02 — capability/revision types | As composition, I want explicit wire and model capabilities so labels cannot select hidden behavior. | Kitsu-owned adapter/profile/generation IDs; wire revision; tools, image, structured output, usage, stream, catalog capabilities; known stale/unsupported fails pre-send. | Serialization/version tests; no display-name switch in physical send paths; zero send on capability failure. | 3-4 cycles |
| E04-S03 — immutable route generation | As an operator, I want every request and auxiliary call bound to one endpoint/wire/model/auth/cache/retry/replay generation. | Freeze before awaits; same-label replacement publishes once; old work drains/cancels by declared policy; failed admission leaves prior generation active. | `X001-G1-AUX` mutates during auth, retry, replay, catalog, title, compaction, and cancellation without cross-generation sends. | 5-8 cycles |
| E04-S04 — distinct compiled adapter | As a user, I want to use the named native dialect without changing agent-loop code or losing tool/error/usage semantics. | All wire translation adapter-local; credentials provided by composition; common normalized stream; no runtime loading. | Both adapters pass the common suite; core has zero concrete wire/config/auth/catalog imports; exact rollback is source revert. | 6-10 cycles |
| E04-S05 — provider receipts and diagnostics | As an operator, I want secret-free route/capability/generation receipts so I can explain every physical attempt. | Report adapter/profile/generation, wire/capability revision, auth source class, retry/replay/fallback; never token, header, body, or secret hash. | JSONL and doctor fixtures are bounded, compatible, and correlate all sends to one generation. | 2-4 cycles |

Epic exit: X001 passes with compatible Chat plus the distinct adapter through one
port; runtime provider loading remains rejected.

## E05 — Transactional guided connection and model selection

Outcome: a fresh user can configure, inspect, select, and switch models without
hand-editing TOML or risking current working configuration.

Dependencies: E04 capability/generation model.

| Story | User story | Requirements | Acceptance evidence | Size |
|---|---|---|---|---|
| E05-S01 — no-clobber connect preset | As a new user, I want guided local/hosted/custom setup that creates a valid profile without exposing a secret or overwriting config. | Start offline; network only after explicit selection; credentials via managed hidden input/stdin, never argv/output/config; preview normalized effect; atomic no-clobber commit. | Fresh-home local and custom mock paths succeed; decline, invalid input, permission cancellation, network failure, collision, and interrupted write leave no partial config/credential. | 4-6 cycles |
| E05-S02 — unified model picker | As a user, I want one capability-aware picker across configured gateways so I can choose a valid model without memorizing IDs. | Separate configured profiles from optional catalog refresh; active model pinned; filter, recents/favorites optional only after basic selection; unsupported capabilities visible before selection. | Two-mock catalogs, offline/stale catalog, large bounded list, no-catalog gateway, current-model, redirected-output, and narrow-terminal cases agree across interactive/JSONL. | 4-7 cycles |
| E05-S03 — atomic profile/model switch | As a user, I want a failed switch to leave my current session and route usable. | Stage and validate target generation; define transcript, system prompt, context meter, compaction, subagent, and pending-turn semantics; publish once; never cross profile credentials. | Failure before publication leaves old route byte-for-byte active; success changes one generation; active/queued/background cases are deterministic. | 4-7 cycles |
| E05-S04 — onboarding readiness receipt | As an unattended operator, I want setup to finish with a truthful offline/catalog/inference readiness result. | Offline doctor default; catalog and billable inference remain separate explicit scopes; alias truth from E01; capability mismatch named. | Human/JSON reports and exits agree; no unexpected send; secret scan clean; first-success task needs no TOML editing. | 2-3 cycles |

Epic exit: controlled novice and automation fixtures complete first success and
switch recovery without implicit mutation, covering E05's applicable `PF-10`
offline/configuration/permission/terminal cases.

## E06 — Restricted stdio MCP external-tool experiment

Outcome: Kitsu can evaluate external tool demand without granting ambient
authority, adding implicit installation, or confusing process lifecycle with
containment.

Dependencies: E01 deterministic registry; E02 cancellation/terminal phases.

| Story | User story | Requirements | Acceptance evidence | Size |
|---|---|---|---|---|
| E06-S01 / A004 — bounded child supervisor | As a runtime maintainer, I want one private long-lived child owner that bounds spawn, environment, pipes, descendants, cancellation, and reap. | Separate `AuthorityProfile`; `X002-ADMIT-0` rejects executable discovery/spawn before authority admission; process group; bounded stdin/stdout/stderr; timeout; idempotent shutdown; typed residue; preserve one-shot command semantics. | Real child covers block, flood, descendant pipe, ignore-cancel, crash, clean EOF, timeout, drop, reload, shutdown, env/fs/network denial and zero pre-admission spawn; command containment regression passes. | 6-10 cycles |
| E06-S02 — frozen tool-source snapshot | As the agent loop, I want an immutable advertised tool inventory so a source cannot change schemas or handlers mid-turn. | Host-authored source/tool identity, schema fingerprint, incarnation and inventory revision; collision before publication; atomic withdrawal; per-subagent authority intersection. | Built-ins plus fixture source pass `PF-06`/`PF-07` collision, snapshot, mutation, withdrawal, child-authority, lifecycle, disabled-path, and zero-provider tests. | 4-6 cycles |
| E06-S03 — tools-only stdio MCP core | As a user, I want explicitly configured local MCP tools available without package installation or unrelated MCP capabilities. | Initialize/list/call only; one namespaced echo tool first; pinned executable/config; required/optional/disabled; protocol deadline and bounds; no HTTP/resources/prompts/sampling. | Primary protocol fixtures plus real echo child; disabled never spawns; required blocks before inference; optional degrades with receipt. | 6-10 cycles |
| E06-S04 — crash, withdrawal, and no replay | As a user, I want a crashed tool server withdrawn immediately and a possibly side-effecting call never guessed safe to replay. | EOF/crash removes descriptor and handler atomically; generation/incarnation on calls; explicit new attempt only; cancel/reload/shutdown join or name residue. | Exit-before-journal, exit-after-journal, duplicate response, late response, reconnect, cancellation, and stale-incarnation tests produce no orphan call or advertised dead tool. | 4-7 cycles |
| E06-S05 — extension receipts and doctor | As an operator, I want secret-free effective authority, source identity, tools, health, and shutdown receipts without doctor starting children by default. | Doctor metadata-only unless explicit active preflight; additive JSONL inventory; path/hash disclosure policy; required/optional distinction. | Offline doctor performs no spawn/network; active fixture reports exact bounded status; old JSONL fixtures remain compatible. | 3-4 cycles |
| E06-S06 — promotion decision | As a product owner, I want evidence before MCP becomes a supported surface. | Compare task value, startup/default cost, security failures, lifecycle reliability, maintenance burden; record retain/reshape/reject. | Separate ADR; prototype success alone cannot promote; removal leaves no config/session migration burden. | 1-2 cycles |

Epic exit: B001 plus `X002-ADMIT-0`, `PF-06`, and `PF-07` acceptance matrices
pass. Same-process npm/Python, Rust dylibs, HTTP MCP, marketplaces, hooks,
providers, and public plugin APIs remain out of scope.

## E07 — Measured semantic context and structural navigation

Outcome: Kitsu adds context intelligence only when it improves representative
repository tasks over its current search, symbol, outline, and manifest tools.

Dependencies: E00 baseline. Provider-free benchmark preparation may start
early, but delivery priority keeps the experiment after E04, E03, and E06 and
it cannot displace a failing correctness or authority gate.

| Story | User story | Requirements | Acceptance evidence | Size |
|---|---|---|---|---|
| E07-S01 — repository task benchmark | As a product owner, I want a deterministic baseline for locate/explain/change tasks so new context features are evidence-driven. | Mix languages/repo sizes; fixed prompts and budgets; measure success, rounds, prompt tokens, tool bytes, latency; no live-provider dependency for core scoring. | Current tool suite produces reproducible baseline and bounded artifacts; evaluator rubric reviewed before experiments. | 3-5 cycles |
| E07-S02 — semantic repo-map experiment | As a coding user, I want high-value symbols/files ranked into a token budget when that beats manual discovery. | Pure provider-neutral tree-sitter/reference-graph/PageRank contributor; deterministic ranking/rendering; hard token/byte cap; no authority expansion; cached projection keyed by revision. | A/B against baseline; denied paths never enter graph/output; stale/corrupt cache rebuilds; stale edit targets still require Kitsu's exact mutation checks; no task regression beyond agreed threshold. | 5-8 cycles |
| E07-S03 — promote, reshape, or reject | As a maintainer, I want the experiment removed if it does not improve task outcomes enough to justify complexity. | Predeclare thresholds; inspect failure modes and language bias; keep current primitives. | Decision record with measurements; accepted implementation has ongoing regression suite, rejected one leaves no production code. | 1-2 cycles |

Epic exit: measured decision, not assumed feature parity with Aider/Goose.

## E08 — Progressive documentation and executable onboarding

Outcome: users reach first success and authoritative task guidance quickly while
Kitsu keeps its unusually precise safety and automation contracts.

Dependencies: DOC-R001 already approved; final root rewrite after E05/E06
surface stability.

| Story | User story | Requirements | Acceptance evidence | Size |
|---|---|---|---|---|
| E08-S01 — canonical ownership skeleton | As a reader, I want each subject to have one authoritative page so root and deep docs cannot drift. | Create routing index and destination skeletons; freeze content/anchor ownership map; move nothing yet. | G6 ownership/link checker passes; every old H2/subtopic has one destination and incoming route. | 3-5 cycles |
| E08-S02 — generated and executable reference | As a user, I want CLI/slash/tool/schema examples generated or tested so documentation matches the binary. | Generate Clap/slash/tool reference; validate JSONL examples; tag/test shell and TOML fences; fresh HOME/no-network fixtures. | G1-G5 and G7-G8 checks run in CI; unexpected state/network/secret fails. | 5-9 cycles |
| E08-S03 — move detailed content | As a task user, I want focused gateway/auth/authority/tool/chat/session/context/automation pages. | Populate sole owners before deleting root text; retain compatibility anchors/routes; no duplicate normative defaults. | Link/anchor/ownership checks and relevant behavioral fixtures pass after each move. | 8-14 cycles |
| E08-S04 — root rewrite | As a new user, I want scope, install, local first success, authority, common tasks, and automation visible without reading an exhaustive manual. | Follow DOC-R001 outline; preserve credential-free path and pre-authority warnings; no line-count target. | G1-G8 plus timed novice lookup tasks pass; root's first 100 lines achieve the defined first success. | 4-7 cycles |

Epic exit: DOC-R001 G1-G8 and the applicable `PF-10` fresh-home, redirected,
narrow-terminal, accessibility, snippet, and progressive-navigation cases pass;
old links are mapped and safety/automation contract coverage is not reduced.

## E09 — Bounded multimodal image input

Outcome: supported providers receive explicitly authorized PNG/JPEG content
under hard disclosure and resource bounds; unsupported models fail before send.

Dependencies: E04 two truthful capability paths and explicit product priority.

| Story | User story | Requirements | Acceptance evidence | Size |
|---|---|---|---|---|
| E09-S01 — evidence and threat contract | As a security reviewer, I want exact MIME, dimension, decompression, metadata, provider-disclosure, logging, persistence, and unsupported behavior before bytes are sent. | Second accepted provider/source; choose encode/pass-through policy; define caps and redaction; no path disclosure to provider unless intended. | ADR plus hostile corpus; zero-send unsupported and invalid cases. | 2-4 cycles |
| E09-S02 — bounded image attachment | As a user, I want to attach a permitted PNG/JPEG and see a truthful disclosure receipt. | Reuse secure attachment authority; verify magic/MIME; cap input/decoded dimensions/output; no external decoder process unless separately contained. | Valid, spoofed, truncated, huge-dimension, decompression-bomb, symlink, protected path, cancel, and duplicate cases. | 5-8 cycles |
| E09-S03 — adapter/session/JSONL integration | As an operator, I want image capability, sends, session behavior, and automation receipts consistent across adapters. | Adapter-local wire mapping; explicit session storage/export policy; body-free JSONL receipts; retry/replay remains same generation. | Two adapters run supported/unsupported fixtures; no image bytes in diagnostic/log metadata; resume/export behavior exact. | 4-7 cycles |

Epic exit: bounded image path passes two adapters. Other media and image
generation remain separate proposals.

## E10 — Five-boundary certification

Outcome: maintainers can state exactly which boundaries are replaceable, which
are prepared, and which remain product-gated, using executed evidence rather
than architecture shape alone.

Dependencies: all promoted epics.

| Story | User story | Requirements | Acceptance evidence | Size |
|---|---|---|---|---|
| E10-S01 — automated coupling checks | As a maintainer, I want CI to reject concrete integration leakage into core. | Checks for provider wire/config/auth/catalog, MCP/supervisor, renderer/JSONL/terminal, CLI/env/signal, and store/filesystem/DB types by boundary. | Intended violation fixture makes each check fail; default and independent adapters compile/run without core edits. | 3-5 cycles |
| E10-S02 — default/alternate conformance | As a product owner, I want every claimed replaceable boundary proven by the default and a named independent adapter. | Run X001-X005 suites; compiled alternatives are acceptable; no dynamic loading requirement. | Score each contract, coupling, authority, isolation, version, observability, and conformance field with linked run evidence. | 5-10 cycles |
| E10-S03 — disabled/default-path parity | As an existing user, I want no cost or behavior change when optional integrations are off. | Compare process tree, network, state, events, schemas, startup, and representative output to milestone baseline. | Byte equality where contractual; otherwise approved semantic diff; zero unexpected process/network/state/schema overhead. | 2-4 cycles |
| E10-S04 — architecture and release decision | As a release owner, I want an honest promote/defer/reject decision and rollback for each boundary. | Update target/current architecture, backlog, ADRs, migration and rollback; do not call prepared boundaries replaceable. | Independent review plus full workspace/security/release suite; research audit clean. | 2-3 cycles |

Important: without a real second host/principal, X003 may finish this program as
“prepared and fixture-proven,” not “replaceable.” The plan must not invent a
desktop, IDE, listener, or service solely to improve the score.

## Thirty-minute implementation loop

The normative loop, acceptance rules, cycle-record template, and commit
convention live in [`cycle-protocol.md`](cycle-protocol.md). Every accepted
cycle ends in one focused green commit; incomplete cards are split without a
red/WIP production commit.

## Initial cycle queue

This is the deterministic starting queue after backlog approval. A cycle may be
split further during its requirements phase, but later epics cannot jump their
gates merely because a card appears attractive.

| Cycle | Card | Intended green commit |
|---:|---|---|
| 001 | E00-S01-C1 | Create branch safely, import approved plan/protocol, record clean base and preserved dirty-main status |
| 002 | E00-S02-C1 | Add invariant test manifest from current tests |
| 003 | E01-S01-C1 | Add alias/mismatch/ambiguity doctor tests and minimal local-Ollama matcher |
| 004 | E01-S01-C2 | Add human/JSON doctor fixtures and close KB-001 gates |
| 005 | E01-S02-C1 | Rebase registry error/types and same-path duplicate tests |
| 006 | E00-S05-C1 | Add the mechanism ledger, three-spine thesis, hard exclusions, gates, and revised order |
| 007 | E01-S02-C2 | Add cross-path/no-mutation/composition-boundary propagation |
| 008 | E01-S03-C1 | Add descriptor/handler and unused-result coupling gates; close E01 |
| 009 | E00-S03-C1 | Pin post-E01 evidence baseline and audit committed delta |
| 010 | E00-S03-C2 | Update current architecture/comparison/backlog/status and close E00 |
| 011 | E02-S01-C1 | Define carrier matrix and provider-free deterministic driver |
| 012 | E02-S01-C2 | Cover success, provider/tool/preflight faults, and exact exits |
| 013 | E02-S01-C3 | Map current cancellation, sink, join, and terminal-uniqueness evidence; retain measured gaps |
| 014 | E02-S01-C4 | Close confirmation/queued-cancel/residue discovery and prove the minimal EOF/human-identity seams |
| 015 | E02-S02-C1 | Introduce private identity/run types for first fixture-proven divergence |
| 016 | E02-S02-C2 | Propagate authority-input EOF as a zero-later-send interruption while explicit denial remains recoverable |
| 017 | E02-S02-C3 | Project neutral run identity/outcome into human carrier receipts and close the private contract |
| 018 | E02-S03-C1 | Add monotonic settlement/join/persist/deliver phase model |
| 019 | E02-S04-C1 | Project common outcome into JSONL compatibly and close first carrier tranche |
| 020 | E04-S01-C1 | Add primary-spec control/distinct-wire provider fixtures |

After Cycle 020, the queue finishes E04's generation/adapter gates, then
advances through E03, E06, E07, E05/E08, and E09 in the dependency-safe order
above. Provider-free fixture design may overlap, but it cannot displace a
failing correctness or authority gate.

## Branch baseline receipt

Cycle 001 created `improvements/harness-roadmap` from committed `main` at
`a2ba2d5c98b17da7290a2b6b8b24c83bc2529763` in a clean linked worktree. The
original `main` worktree retained its ten pre-existing modified files and was
not stashed, reset, overwritten, or absorbed. See [`cycles/001.md`](cycles/001.md)
for the acceptance receipt.

## Deferred and rejected work

Deferred until a named consumer/evidence gate exists: bidirectional host/app
server, lifecycle hooks, WASM/WASI, a Kitsu-specific external protocol, control
plane, and further media types.

Rejected for this program: same-process full-authority npm/Python/script
plugins, a general Rust dylib ABI, runtime provider/TUI loading, implicit
marketplace install/update, name-only identity, remembered cross-session
authority, fail-open/unjoined hooks, timeout-by-abandonment, process separation
presented as containment, EOF/idle/UI state presented as success, a DB/file
tail/index presented as canonical HEAD, and any discovery path that executes or
fetches before explicit admission. The complete rationale is in the
[mechanism learning ledger](mechanism-ledger.md).
