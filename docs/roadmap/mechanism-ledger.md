# Cross-project mechanism learning ledger

## Decision rule

Kitsu borrows narrow, testable mechanisms behind Kitsu-owned types and policy.
It does not borrow another project's plugin model, trust defaults, package
manager, persistence authority, or terminal semantics.

The research is pinned source/test evidence, not a common runtime benchmark.
It can shape requirements and fault fixtures, but it cannot by itself authorize
promotion, prove safety, or make any X001-X005 boundary replaceable. Every
borrowed mechanism must pass Kitsu's authority, failure, versioning,
observability, compatibility, rollback, and disabled-path gates.

## Transfer ledger

| Source | Narrow mechanism to adapt | Kitsu gap it addresses | Surrounding model explicitly not copied |
|---|---|---|---|
| OpenCode | Common provider events, pre-send fallback, offline/stale model catalog, and transactional selection | All current profiles still use one compatible-Chat wire; setup and model selection are manual | Executable configuration, implicit package installation, ambient hooks, or mutable provider generation |
| Codex | Writer locks, ordinals, projection checkpoints, fork reservation/CAS, and MCP lifecycle fault cases | No canonical session revision/HEAD and no MCP lifecycle owner | Responses-only coupling, a DB-only selected HEAD, or identity/class-stripping exec output |
| Pi | Distinct compiled wire adapters behind neutral types and explicit image capability | No genuinely distinct compiled Kitsu dialect; no multimodal request path | Ambient in-process JavaScript extensions, runtime provider loading, or unsandboxed execution |
| OpenHands | Revisioned event lineage, logical HEAD, leases, and validated branch points | Fork copies current history; no branch-at-turn or explicit logical HEAD | Treating Canvas, process separation, or container configuration as executor containment |
| Cline | Phase-budgeted child restart/drain and canonical-history-plus-derived-projection structure | No long-lived child supervisor; compaction history is not canonical | Ambient child authority, global remembered approvals, split session truth, or timeout-by-abandonment |
| Qwen Code | Content-hash writer fencing, no-overwrite fork publication, last-good recovery, classified retry, and safe read concurrency | Corrupted-current recovery and concurrent session publication remain narrow | Acquisition before consent, fail-open hooks, unjoined work, or carrier/HEAD divergence |
| Aider | Pure tree-sitter/reference-graph/PageRank selection under a token budget | Existing search, symbol, outline, and manifest tools do not provide semantic reference-graph selection | Fuzzy retargeting, in-place writes, unsafe edit fallback, or a LiteLLM bypass around Kitsu's provider seam |
| Goose | MCP handshake/crash/cache/secret/reap cases and read-only structural navigation ideas | No MCP; current navigation is lexical/structural but not a semantic context contributor | Auto-update/default-enable, ambient hooks, pre-admission acquisition, or EOF-as-completion |
| Crush | Offline-capable degraded model picker, staged proposals, and process-group cancellation cases | Weak first-run/model discoverability and no transactional picker | Approval-as-containment, implicit acquisition, or timeout-by-abandonment |
| DeepSeek Harness | Admission-then-acquire-validate-publish rollback, owned-effect cleanup, shared logical store suites, and explicit cancellation ownership | No alternate-store conformance and no staged contributor seam | Executable configuration, pre-admission imports, uncontained MCP, or a broad runtime plugin surface |

## Operator-supplied telemetry UX evidence

On 2026-08-19 the operator supplied current Codex `/status` and `/usage` output
plus a Claude Code `/context window` example. This evidence is intentionally not
folded into the frozen ten-harness research pin. Official OpenAI documentation
confirms the Codex command distinction: `/status` presents session telemetry,
while `/usage daily|weekly|cumulative` uses Codex account activity and may
require Codex service-account authentication. The latter is not a generic
provider wire capability. Reference:
<https://learn.chatgpt.com/docs/developer-commands?surface=cli>.

Kitsu will borrow the compact grouped status deck, focused context occupancy,
and calendar/summary presentation. E08-S05/S06 own the UI and portable
snapshots; E04 owns provider generation/capability facts, E03 owns durable
observation fencing, and E06 owns external-tool health. Kitsu will not copy
account scraping, provider-wide billing claims, ambient telemetry collectors,
or any layout that makes color/Unicode a machine contract. The detailed truth
and acceptance contract is [`e08-telemetry-decks.md`](e08-telemetry-decks.md).

## Three architectural spines

The long-term architecture is organized around three strong spines. Features
attach only after the relevant spine has executable evidence.

1. **Immutable provider generations.** One `{adapter, profile, generation}`
   identity binds wire, endpoint, model, capabilities, auth, retry, replay,
   cache, catalog, compaction, title and every auxiliary send. E04 owns this
   spine; E05 consumes it. Gate: `X001-G1-AUX`.
2. **Canonical session revision and logical HEAD.** Transcript, terminal
   outcome, selected HEAD, resume data, lineage, compaction binding, and
   artifacts commit under one opaque revision. Locks, leases, ordinals, CAS,
   checkpoints, and no-overwrite publication are subordinate physical
   mechanisms. E03 owns this spine. Gate: provider-free `K01-K12` crash cuts
   with exactly the old or new revision and rebuildable projections.
3. **Bounded child supervisor.** Authority admission happens before executable
   discovery or spawn; the lifecycle owner bounds environment, pipes, process
   groups, cancellation, shutdown, and descendant reap. MCP is one consumer,
   not the supervisor's authority model. E06 owns this spine. Gates:
   `X002-ADMIT-0`, `PF-06`, and `PF-07`.

Kitsu's causal run outcome remains prerequisite infrastructure across all three
spines. It is not a fourth integration plane: E02 prevents a provider, store,
child, or carrier from redefining success through EOF, idle, UI state, or a
failed delivery.

The telemetry deck is likewise not a fourth spine. It is a read-only projection
of facts owned by E02/E03/E04/E06. A renderer, cache, aggregate, or file tail
cannot become canonical run, route, session, authority, or usage truth.

## Dependency-safe delivery order

Epic identifiers stay stable; this order controls queue selection after E01.

1. Complete E00/E01 and establish E02's provider-free causal-outcome driver.
2. Build E04's distinct compiled provider and immutable generation spine.
3. Build E03's canonical revision/HEAD, recovery, and branch-at-turn spine.
4. Build E06-S01's supervisor, then run the restricted tools-only stdio MCP
   experiment through the rest of E06.
5. Run E07's semantic context/navigation benchmark and experiment.
6. Deliver E05 transactional setup/model selection and E08 truthful telemetry,
   progressive documentation, and observability on the stabilized provider and
   MCP surfaces. E08-S05 may consume live route facts after E04; E08-S06 waits
   for E03 fencing before durable usage publication.
7. Consider E09 multimodal input only after two truthful provider capability
   paths; consider additional hosts/control planes only after a named consumer.
8. Use E10 to certify, defer, reshape, or reject each boundary from executed
   evidence.

Provider-free fixture design may overlap earlier work, but production cards do
not jump prerequisites. Multi-surface operation remains a measured gap, not a
reason to invent a desktop, IDE, listener, daemon, or control plane solely for
architecture scoring.

## Gate routing

| Gate | Owning work | Required evidence |
|---|---|---|
| `X001-G1-AUX` | E04 | Primary-spec Chat control plus one distinct wire; immutable auth/retry/replay/cache/catalog/title/compaction/auxiliary generation; replacement, withdrawal, cancellation, and join |
| `K01-K12` / X005 canonical cuts | E03 | Whole-turn and compaction crash cuts, stale writer fencing, corrupt/future state, selected-HEAD projection deletion/rebuild, atomic artifact/fork rollback, export/import, and fresh authority |
| `X002-ADMIT-0` | E06-S01-E06-S03 | Zero fetch, checkout, write, import, initializer, or spawn before artifact/effect authority admission |
| `PF-06` and `PF-07` | E06 | Tool-source lifecycle/authority, frozen inventory, EOF/crash withdrawal, no replay, cancel/reload/shutdown and descendant reap, disabled zero-effect path |
| Fixed semantic corpus | E07 | Relevance and token-budget improvement over current tools, denied-path behavior, stale projection rebuild, language bias, and stale-edit safety |
| `PF-10` | E05 and E08 | Fresh-home first success, invalid configuration, permission cancellation, offline startup, redirected output, widths 24/40/60/80/120, plain/screen-reader/JSONL parity, telemetry truth/staleness/secret safety, executable snippets, and progressive navigation |

## Hard exclusions

This program continues to reject:

- in-process npm, Python, script, Rust-dylib, or general runtime plugin loaders;
- implicit installation, update, package resolution, executable discovery, or
  network access during configuration discovery;
- name-only provider, tool, approval, session, source, or generation identity;
- broad remembered approvals across sessions, revisions, replacement, or
  principals;
- fail-open hooks, cancellation without joined cleanup, and
  timeout-by-abandonment;
- treating stdio, a child process, a container setting, or process separation
  as authority containment;
- treating idle, EOF, UI state, file tail, process exit, or delivery attempt as
  semantic success;
- treating a DB row, file tail, projection index, cache, or UI selection as the
  canonical session HEAD;
- runtime provider/TUI loading, HTTP/OAuth/marketplace MCP, lifecycle hooks,
  WASM, or a public extension ABI without their separately named consumer and
  reconsideration gate.
- generic account/billing claims, currency/credit/rate-limit guesses, or
  default network refresh from status, context, or usage commands.

The safe extension thesis is intentionally narrow: compiled adapters,
Kitsu-owned ports, inert audited data, and explicitly supervised children may
be considered independently. There is no universal plugin API.
