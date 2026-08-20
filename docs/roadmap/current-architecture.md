# Kitsu Current Architecture

Status: source-audited from runtime-source pin
`986fba971ec99c9af5dcd8f69a48f507aae486af`, with accepted implementation
updates through Cycle 039.

The exact provenance and frozen research-input hashes are in the
[post-E01 evidence baseline](post-e01-evidence-baseline.md). This document
supersedes the frozen research architecture for current planning; it does not
rewrite or invalidate that historical source audit.

## Ownership and dependency shape

```text
                         kitsu binary
 config / composition / sessions / carriers / JSONL / skills / subagents
          |                 |                 |               |
          v                 v                 v               v
     kitsu-core       kitsu-providers    kitsu-tools     kitsu-render
   agent/context       one neutral       registry +      terminal +
       loop              trait          authority/tools  JSONL writer

kitsu-core  -> kitsu-providers + kitsu-tools + kitsu-render
kitsu-tools -> kitsu-providers::ToolDescriptor
```

The five-crate layout is unchanged. `kitsu-core` still imports the concrete
tool registry and renderer (`crates/kitsu-core/Cargo.toml`), so the current
provider trait is a useful compiled seam but the core is not yet a host-neutral
kernel.

| Area | Current owner/behavior | Evidence at the pin | Consequence |
|---|---|---|---|
| Product composition | The binary owns config, auth selection, provider construction, tool/skill/profile registries, sessions, carriers, JSONL adaptation, and user interaction. | `crates/kitsu/src/main.rs:1231,4022,7875-7970,13041`; 21,062 lines | Product invariants have one ultimate owner, but extraction must be fixture-driven because lifecycle and presentation are interleaved. |
| Agent loop | Core owns bounded context/rounds, physical provider attempts, sequential tool execution, finalization, cancellation, and runtime events. The neutral run module owns identity/outcome values and a monotonic lifecycle that rejects duplicate/out-of-order/post-terminal transitions. `AuthorityInputClosed` is wired as a loop interruption; all inference carriers publish through one process-scoped `RunLifecycle`. The interactive host aggregates successful, failed, cancelled, interrupted, retried, queued, and detached operations rather than using its last turn. Cleanup derives nonzero residue from actual join statuses, the active-session receipt separates addressability from preserved/committed/failed persistence, and an unproven fallback is explicitly `NotObserved`. One evaluator maps immutable publication to exact carrier exit. | `crates/kitsu-core/src/orchestrator.rs`; `crates/kitsu-core/src/run.rs`; `crates/kitsu/src/process_exit.rs`; `crates/kitsu/src/main.rs` | E02 is accepted without changing JSONL v1 or session files. `NotObserved` prevents an unexpected fallback from claiming clean success. X004 remains non-replaceable because no alternate runtime adapter runs the suite. |
| Provider port | `Provider` exposes health, chat stream, attempt telemetry, and model catalog operations using Kitsu request/event types. Provider-neutral declarations, immutable snapshots, current owners, model-bound handles, and catalog-only handles fence physical entry. | `crates/kitsu-providers/src/lib.rs`; `crates/kitsu-providers/src/route.rs` | This is a compiled seam with truthful current-generation admission, not a runtime ABI or proof of dialect replaceability. |
| Provider construction | Every accepted `openai`, `anthropic`, or `ollama` flavor still selects the compiled compatible-Chat adapter. Chat and explicit inference doctor use model-bound immutable generations; top-level `models` and catalog doctor use catalog-only generations with no model or inference runtime. A separate compiled native Messages text adapter exists only behind offline fixtures. | `crates/kitsu-providers/src/openai_compat.rs`; `crates/kitsu-providers/src/anthropic_messages.rs`; `crates/kitsu/src/main.rs`; provider wire/route/CLI tests | Flavor labels do not yet select native wires, but they no longer bypass immutable route identity. E04-S04 must finish native tools, faults, bounds, telemetry, and shared-route conformance before composition can truthfully select it. |
| Provider resilience | Physical attempts have deadline-bounded telemetry; transient 408/429/502-504/520-524/529 retry, while generic 500 stays final. Nested `providers[]` context discovery and a minimum floor are supported. Every current production chat/catalog/compaction/subagent/doctor send is bound to one frozen auth/model/policy generation, and JSONL attempt evidence carries its generation where emitted. Native Messages text decoding requires explicit `message_stop` and bounds response bytes, but is not production eligible yet. | `crates/kitsu-providers/src/openai_compat.rs`; `crates/kitsu-providers/src/anthropic_messages.rs`; `crates/kitsu-providers/src/route.rs`; provider/core/CLI tests | E04-S03 is accepted. Native fault/telemetry parity and complete secret-free receipts remain E04-S04/S05. |
| Tool registry | Descriptor-free and described admission share one name key, return `Result`, reject same/cross-path duplicates before per-entry mutation, and preserve the first entry. Builtin composition propagates a typed CLI user error. | `crates/kitsu-tools/src/lib.rs:69-121`; `crates/kitsu-tools/src/builtins/mod.rs`; `crates/kitsu/src/main.rs:4022-4057` | A001 is complete for deterministic per-entry admission. Batch transactionality, provenance, source identity, and lifecycle are not implied. |
| Builtin tools | Read/write/command capability is assembled by omission under separate permissions. Bounded search, symbol, outline, manifest, document, image-metadata, audit, and secret-scan readers exist. | `crates/kitsu-tools/src/builtins/mod.rs:23-137`; builtin tests | The structural-navigation gap is narrow now; semantic reference-graph ranking remains absent. |
| Command execution | Exact argv/cwd authority remains separate from filesystem roots. Bubblewrap containment, cleared/projected environment, bounds, process groups, cancellation kill, and reaping remain one-shot command behavior. | `crates/kitsu-tools/src/builtins/command.rs`; 4,900 lines | E06 may extract private lifecycle mechanisms, but a long-lived MCP child needs its own bidirectional supervisor and tests. Process separation alone is not containment. |
| Filesystem mutation | Canonical/descriptor checks, atomic install, exact preconditions, journaling, verified effects, and conflict-aware undo remain Kitsu-owned. | `crates/kitsu-tools/src/permissions.rs`; `crates/kitsu-tools/src/builtins/fs.rs` | Semantic context or external sources must use these authorization/mutation primitives, never replace them with fuzzy or in-place writes. |
| Skills | Local bounded skills are discovered as untrusted instructions/resources with path/provenance checks; effects still cross ordinary tools. | `crates/kitsu/src/skills.rs:359,824-900,1514` | This is a declarative data plane, not executable configuration or a plugin loader. |
| Events/JSONL | Core runtime events are projected by the CLI into versioned `kitsu.chat.v1`; the writer serializes whole records, flushes, and fails terminally. One neutral `RunSpec` supplies JSONL, private generations, and human completion. Human timing and JSONL `final_summary` carry the same exact run outcome/assistant tuple; provider attempts add optional route generation outside adapter-authored data. Legacy summaries remain valid and contradictions fail fixture validation. | `crates/kitsu-core/src/run.rs`; `crates/kitsu/src/main.rs`; `crates/kitsu-render/src/lib.rs`; `crates/kitsu-render/src/jsonl.rs`; `docs/schemas/kitsu-chat-v1.schema.json` | E02 is accepted for the default runtime. Complete provider route receipts remain E04-S05, and alternate host/runtime conformance remains open. |
| Sessions | A versioned `SavedSession` atomically stores one current message vector plus gateway/model/project context. Fork copies the complete snapshot/artifact tree to a fresh identity without grants. | `crates/kitsu/src/session.rs:40-105,1613-1780,2309-2470` | There is no opaque canonical revision, expected-head CAS, logical HEAD, branch-at-turn, durable canonical compaction history, or verified last-good generation. |
| External tools/hosts | No MCP dependency/client, long-lived child supervisor, bidirectional host adapter, daemon, or second principal is compiled. JSONL is one-way output. | workspace manifests and source search | X002-X004 remain failed; absence preserves the disabled path and avoids ambient authority. |

## What changed from the frozen research pin

- The 67 committed base delta materially improves retry classification,
  context-window discovery, timeout transcript truth, bounded context offload,
  command-containment portability, and read/code-navigation breadth.
- E01 closes the loopback Ollama `:latest` diagnostic defect and the bounded
  deterministic registry-admission transfer, including CLI propagation and CI
  coupling guards.
- E02 makes seven inference carriers publish one causal outcome and exact
  terminal phases; E04-S01/S02/S03 pin two wire shapes and bind every current
  production provider send to an immutable compatible-Chat or catalog-only
  generation.
- These changes close no complete X001-X005 gate. They are prerequisites and
  regression baselines for later conformance fixtures.

## Current extension planes

| Need | Current mechanism | Trust/process boundary | Status |
|---|---|---|---|
| Native behavior | Compiled Rust modules registered by product composition | Same process, binary authority | Retain; no dylib/runtime loader |
| Static instructions/resources | Local skills | Untrusted data; no inherent effect authority | Retain and reuse provenance rules |
| Provider dialect | Compiled `Provider` trait, one concrete compatible-chat adapter | Same process; network/auth provider-owned | Prepared, not replaceable |
| External runtime tools | None | None | E06 experiment only after E02 lifecycle gates |
| Host/UI protocol | CLI carriers plus one-way JSONL | Consumer cannot service synchronous calls | No named second host; X003/X004 fail |
| Persistence adapter | Private file snapshot implementation | Local private state | No port/revision/HEAD conformance; X005 fails |

## Three spines and extraction order

1. E02 first establishes causal run identity/outcome and monotonic terminal
   phases with provider-free fault fixtures.
2. E04 binds provider/auth/retry/catalog/compaction work to immutable route
   generations and proves one distinct compiled wire.
3. E03 makes one canonical session revision and logical HEAD authoritative.
4. E06 extracts a bounded child lifecycle owner and evaluates tools-only stdio
   MCP, disabled by default.

Semantic context, onboarding, multimodal input, and any additional host consume
those spines. They do not create alternate authority, mutation, session, or
terminal systems.

## Current non-claims

- A trait or two flavor labels do not make providers replaceable.
- Deterministic registry admission does not make external tool sources safe.
- JSONL does not constitute a bidirectional host or shared run contract.
- Atomic snapshots and whole-history fork do not constitute canonical HEAD.
- Search/symbol/outline tools do not constitute semantic repository mapping.
- Process separation, user approval, idle, or EOF do not prove containment or
  terminal success.
