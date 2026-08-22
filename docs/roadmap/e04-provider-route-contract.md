# Provider route value contract

Status: E04-S02/S03/S04/S05 and E04 are accepted through Cycle 049.
Cycle 032 establishes the value contract; Cycle 033 derives opaque profile
identity and makes live composition retain the truthful declaration. Cycle 034
constructs one initial immutable snapshot with both compatible-Chat runtimes
and a credential resolved once for that generation. Cycle 035 adds an atomic
current owner and bound request handles, migrates primary chat/catalog/context
calls, and pins replacement plus drain behavior. Cycle 036 makes every chat
request match an immutable model-bound handle, publishes default/resumed model
changes as exact successors, and snapshots each subagent route after profile
resolution. Cycle 037 attributes physical attempts at the immutable handle
boundary and closes the retry/replay/catalog/compaction/subagent/cancellation
mutation matrix for chat-owned routes. Cycle 038 gives management-only catalog
work a narrower no-model/no-inference generation, binds the three remaining
production sends, and closes E04-S03. Cycle 039 compiles the first native
Messages text/usage/terminal slice without selecting it in production. Cycle
040 adds exact tool-use/result translation and bounded streamed argument
correlation. Cycle 041 runs the actual native adapter through HTTP,
mid-stream/malformed, size-bound, timeout, and cancellation parity. At that
cut, attempt telemetry, route composition, and common conformance remained
E04-S04 while complete route receipts remained E04-S05. Cycle 042 adds exactly
one terminal native
attempt observation across success, rejection, decode/body failure, timeout,
send cancellation, and accepted-body cancellation. Cycle 043 publishes an
exact native declaration and immutable frozen-auth snapshot, exercises it
through the common route handle, and rejects catalog policy the adapter cannot
honor. Cycle 044 freezes bounded retry policy into that route and makes both
compiled adapters use one provider-neutral delay, deadline, status taxonomy,
and attempt-lifecycle owner.
Cycle 045 instantiates one generic route-handle conformance harness with both
compiled adapters; no production exception or dialect branch is required.
Cycle 046 adds a closed compiled provider enum and explicit adapter config,
then proves native chat and inference doctor use the exact Messages wire while
legacy flavor-only profiles stay compatible Chat and native catalog fails
before transport.
Cycle 047 defines the complete provider-neutral route receipt projected from
snapshots and bound/current handles without serializing physical runtimes.
Cycle 048 exposes that exact value additively in `/status`, all JSONL run-entry
paths, and opted-in doctor checks. Offline doctor emits an explicit
no-construction state; JSONL attempts correlate to the run receipt by immutable
generation; `/model` successor status reads the new current receipt.
Cycle 049 drives both compiled adapters through one identical product-level
success/fault/status/JSONL/doctor/offline harness and one identical SIGINT
cancellation harness. The red fixture found native Messages omitted its
required `max_tokens` when the user supplied no limit; omission now maps to a
bounded 4,096-token default while explicit zero is rejected before transport.
The final static coupling/loading/title-generation audit is clean, so E04 and
X001 are accepted.

## Decision

Kitsu owns a closed, versioned, provider-neutral identity for one immutable
provider route snapshot:

```text
{ contract_revision, adapter, profile, generation, wire_revision, capabilities }
```

It also owns a versioned diagnostic receipt:

```text
{ receipt_revision, route, model?, auth_source,
  policy: { retries, exact time budgets, catalog,
            retry_generation, semantic_replay_generation, fallback } }
```

Retry stays on the admitted bound generation; a user-level semantic replay
binds the current generation; provider fallback is explicitly disabled. Exact
durations use bounded seconds/nanoseconds pairs, avoiding overflow or lossy
millisecond conversion. Receipt decoding denies unknown/future fields, zero or
invalid durations, oversized models, and catalog capability/policy mismatch.

The serialized value is safe for receipts and deterministic fixtures. It never
contains an endpoint, credential, auth header, request/response body, model
content, provider config object, or user-facing gateway label.

The compiled IDs name compatible Chat and Anthropic Messages wire revisions.
Cycles 039-045 prove the Messages request/event implementation and shared route
conformance. Cycle 046 promotes it through an explicit closed selector; flavor
alone never changes the wire and legacy profiles remain compatible Chat.

## Values and validation

- `ProviderProfileId` is `profile-v1-` plus 32 lowercase hexadecimal digits.
  Cycle 033 derives that opaque 128-bit payload from a domain-separated SHA-256
  over the canonical config namespace and gateway key, truncated to 128 bits.
  Inputs are length-framed and bounded at 64 KiB/1 KiB. Endpoint, model, auth,
  flavor, and capabilities are not identity inputs.
- `ProviderRouteDeclaration` validates adapter/wire/capability truth before a
  generation exists. Today's compatible-Chat declaration advertises streaming,
  tools, structured output, and usage normalization; it advertises model
  catalog only when a non-empty `models_path` is configured, and never image
  input. Its catalog-only declaration advertises exactly `model_catalog` and
  rejects any inference capability before publication. The native Messages
  declaration advertises exactly streaming, tools, and usage: no image input,
  structured output, or catalog.
- `ProviderRouteGeneration` is a nonzero monotonic identity. Zero cannot look
  like a published generation.
- `ProviderWireRevision` names an exact compiled protocol revision and must
  match its adapter.
- `ProviderCapabilities` has a nonzero schema revision and a deterministic,
  duplicate-free set of `streaming`, `tools`, `image_input`,
  `structured_output`, `usage`, and `model_catalog`.
- Deserialization rejects unknown fields/variants, future contract revisions,
  malformed profile IDs, zero revisions/generations, duplicate capabilities,
  and adapter/wire mismatches.
- `validate_for_send` rejects a stale generation before examining capabilities;
  a missing capability also rejects. Both outcomes precede physical send.
- `ProviderRoutePolicy` freezes inference/catalog retries, their nonzero time
  budgets, the bounded advisory discovery budget, and whether a model catalog
  is configured. Catalog policy must agree exactly with the declared
  `model_catalog` capability.
- The private retry owner defines the shared transient set (408, 429,
  502-504, 520-524, and 529), exponential floor, bounded positive jitter,
  capped `Retry-After`, one end-to-end deadline, and cancellation guards.
  Authentication, deterministic client errors, 500, and admitted response-body
  failures are terminal and never retried.
- `ProviderRouteReceipt` is projected only from the immutable snapshot behind
  a snapshot/current/inference/catalog handle. Its runtime is structurally
  absent. A stale bound handle continues to report its old generation after a
  successor becomes current; the current owner reports the successor.
- `ProviderRouteSnapshot<P>` owns either a model-bound inference generation or
  a catalog-only management generation, plus secret-free auth source, policy,
  and its provider-specific runtimes. Catalog-only construction owns no model
  and no inference runtime. The runtime snapshot is intentionally neither
  serializable nor debuggable; endpoint, paths, caches, and sensitive auth
  remain inside `P`.
- `FrozenAuth` resolves a sensitive header once into zeroizing storage. All
  inference retries and catalog calls owned by the snapshot share that same
  auth object; later environment changes cannot alter an admitted generation.
- `build_anthropic_messages_route` requires the exact native declaration before
  acquiring auth, freezes the selected auth source once, constructs generation
  one with its inference retry count, and refuses any catalog policy. The same
  runtime owner occupies the snapshot's private slots, but missing catalog
  capability prevents catalog entry before either runtime can be reached.
- `ProviderRouteCurrent<P>` owns the one process-local current snapshot behind
  a bounded in-process read/write lock. `bind` returns a
  `ProviderRouteHandle<P>` tied to that immutable generation; every logical
  inference, catalog, context-discovery, compaction, foreground, or subagent
  call validates current generation and required request-shape capabilities
  before it can enter a provider runtime.
- `ProviderCatalogHandle<P>` binds the current catalog generation without a
  model and deliberately has no `Provider` implementation or chat method.
  Catalog-only successor publication stales the old handle before a send, and
  top-level JSONL attempt evidence is stamped by this handle rather than by the
  adapter.
- Replacement is expected-generation and exact-successor only. A stale
  expectation, profile mismatch, non-successor, exhausted generation, or
  poisoned owner leaves the prior snapshot installed. Once a logical request
  has been admitted, it owns the old runtime long enough to drain its internal
  retry/stream policy; a later call through that old handle fails stale.
- `bind_model` creates an operation-scoped model binding without changing the
  process default. Every chat request must match that bound model or it fails
  locally with zero sends. This preserves explicit subagent profile models:
  the provider binder resolves after profile selection and is retained across
  every round of that child run. A concurrent default replacement lets an
  admitted request drain, then makes the old child's next request fail stale.
- `replace_model` validates the target before locking, requires the expected
  generation, treats the same model as a no-op, and otherwise publishes exactly
  one successor while reusing the unchanged frozen transport/auth/policy
  owners. `/model` and saved-session resume stage catalog/context facts first,
  publish next, then mutate live model/window/provenance state.
- Raw adapters emit transport dispositions without route identity.
  `ProviderAttemptObservation` adds an optional generation only when a current
  immutable handle decorates the telemetry sink after admission. Chat and
  catalog attempts—including retries, backoff cancellation, and terminal body
  events—therefore retain the admitted generation even if a successor is
  published while the request drains. Legacy/raw callers remain explicitly
  unattributed.
- Runtime attempt events project the generation as an additive secret-free
  integer. Foreground and catalog JSONL preserve it; child telemetry retains
  the first observed generation plus an explicit mismatch bit rather than
  silently overwriting contradictory evidence. Provider-controlled endpoint,
  auth, header, body, model content, and configuration values never enter this
  observation.
- The provider port's catalog is optional: a current route without
  `model_catalog` returns `None` without entering either runtime. Staleness is
  still checked first. Capabilities required by a concrete chat request, such
  as streaming, tools, or structured output, fail with a typed local error and
  zero sends.
- The only three provider sends that were outside chat-owned composition—top-
  level `models`, `doctor --online`, and `doctor --inference`—now construct and
  bind immutable management routes. Offline doctor still constructs no route
  and performs no provider request. Catalog-only doctor remains usable without
  a configured model; the missing chat model is reported independently.

The exact v1 JSON vector and all validation cuts are pinned by
`provider_route_contract.rs`. The provider wire matrix consumes the same
production capability vocabulary for its unsupported-image zero-send case.

## Boundary and non-goals

Cycle 032 defines immutable values and a pure admission operation. Cycle 033
adds bounded derivation and a truthful declaration. Cycle 034 replaces the two
independent chat-provider constructors with one compatible-Chat route factory.
It resolves auth exactly once, constructs inference and catalog runtimes from
one consumed endpoint/path/policy input, and retains their initial generation
in `ChatSetup`. Cycle 035 replaces those raw `Arc` fields with one current owner
and passes bound handles through ask, chat, scripted turns, foreground and
subagent inference, compaction, slash catalog/model validation, context refresh,
and resume discovery. Cycle 036 binds foreground turns once, resolves subagent
bindings once after effective-model/profile selection, and makes direct plus
resumed model changes advance the process-default generation. Cycle 037 stamps
attempt evidence outside adapter-authored events, proves admitted physical
retry/catalog work drains entirely on its old generation, proves a user-level
`/retry` binds the new current generation, and retains generation evidence for
compaction and child work. Cycle 038 introduces a capability-narrow
catalog-only declaration/snapshot/handle, freezes its auth once, migrates
top-level `models` and catalog doctor, and routes the explicit inference doctor
through a model-bound handle. All accepted `openai`, `anthropic`, and `ollama`
flavor labels still declare the compatible-Chat adapter/wire Kitsu actually
constructs; no flavor label selects a physical adapter. E04-S03 is complete for
every provider send in current production. There is no live configuration
reload surface; any future reload must construct and atomically publish a full
successor rather than mutate a generation in place. Cycle 039 adds an
adapter-local `AnthropicMessagesProvider` that emits the exact pinned
`/v1/messages` text request, fixed `anthropic-version`, incremental text,
normalized usage, and terminal reason. It requires explicit `message_stop`,
bounds event streams/error bodies, and initially rejects tools before
transport. Cycle 040 adds exact prior `tool_use` plus user `tool_result`
translation, validates prior input before send, correlates fragmented
`input_json_delta` by bounded block index, validates the completed object, and
emits one neutral call before usage/terminal output. It is compiled but
unreachable from production composition. Cycle 041 proves its typed 529 and
bounded error-body handling, malformed/error events, line and aggregate
ceilings, deadline timeout, and cancellation all fail without a second send.
Cycle 042 owns the native send and response-body lifecycles with guards: a 2xx
header is never success, dropping either phase records cancellation, and every
physical send has one terminal common event. Cycle 043 adds the exact native
streaming/tools/usage declaration and a factory that validates policy before
auth acquisition, freezes auth once, and publishes the adapter as generation
one. Common handle fixtures prove generation-attributed attempts, optional
catalog absence, and structured-output rejection without a second physical
send. Production flavor selection remains unchanged.

Cycle 044 extracts the proven compatible-Chat retry mechanism without changing
its behavior and binds native retries to the immutable route policy. Offline
cuts prove 529-to-success retains generation one across both attempts, an
oversized `Retry-After` loses to the request deadline with one send, cancellation
during backoff closes the scheduled attempt without sending it, and a malformed
accepted body is terminal even when retries remain.

Cycle 045 proves both adapters produce the same neutral text/tool deltas and
generation-attributed body terminal, advertise the same required common
capabilities, preserve optional-catalog absence without a send, publish an
exact model successor, and reject the stale handle before a third send. Cycle
049 adds the product-level route receipt, success, HTTP-fault, cancellation,
status, JSONL, doctor, and offline cuts. Intentional truthful limitations remain:

- the Messages adapter has no model catalog;
- Kitsu does not expose provider account, billing, quota, endpoint, or
  credential data.

`rg` confirms production `kitsu-core` does not import either concrete adapter,
provider config, auth implementation, or catalog implementation; its test-only
route fixture uses provider-neutral values. The CLI has no flavor-to-adapter
switch. There is no title-generation provider call in current Kitsu.
Production composition has no raw concrete-provider constructor or dynamic
provider loader. `CompiledProvider` is a closed enum, and only the typed
`adapter` field selects its variant. X001 passes without adding a plugin
surface. Provider work can now yield to E03's canonical session spine.

## Acceptance and rollback

Provider-free gates:

```bash
cargo test -p kitsu-providers --test provider_route_contract
cargo test -p kitsu-providers --test provider_route_declaration
cargo test -p kitsu-providers --test provider_route_snapshot
cargo test -p kitsu-providers --test provider_route_handle
cargo test -p kitsu-providers --test provider_route_conformance
cargo test -p kitsu-providers --test provider_wire_matrix
cargo test -p kitsu --test cli both_compiled_adapters_
```

Six route-contract, four declaration/derivation, four snapshot, twelve
route-handle, compatible-Chat/catalog route-factory, auth-freeze/source-
selection, doctor/catalog-without-model, generation-aware
compaction/subagent/replay/JSONL, and thirty-one wire-matrix cases pass offline.
The native slices prove exact distinct text and tool request/header shapes,
normalized text/tool/usage/terminal output, EOF-as-failure, pre-send malformed
tool-input rejection, and the 1 MiB streamed argument bound. Native fault cases
additionally prove typed/bounded HTTP failures,
malformed/mid-stream rejection, independent line/aggregate ceilings, timeout,
and cancellation with exactly one send. Native attempt fixtures add terminal
body success, HTTP rejection,
decode failure, header timeout/cancellation, and accepted-body cancellation.
The native route fixture additionally proves exact capability truth,
once-frozen auth, typed unsupported-policy errors, common-handle generation
attribution, catalog absence, and structured-output zero-send admission.
Native retry fixtures add generation-stable transient retry-to-success,
deadline refusal, backoff cancellation closure, and admitted-body no-retry.
Two generic route-conformance instantiations add identical neutral text/tool,
capability, attempt-generation, catalog-absence, successor, and stale zero-send
evidence for compatible Chat and native Messages.
Two product-level CLI cases add identical route-receipt correlation across
success, bounded/redacted HTTP failure, SIGINT cancellation, status, JSONL,
inference doctor, and offline doctor. The native default-token regression pins
omitted `max_tokens` to 4,096 and explicit zero to a pre-send error. Together
with the retry/replay/catalog/compaction/subagent/cancellation mutation cases,
these execute `X001-G1-AUX`; no title-generation call exists to bind.
The derivation vector is independently checked
against `sha256sum`; symlink aliases resolve to one canonical namespace, while
different config namespaces or gateway keys do not share identity. Reverting
the catalog-only types/factory and management composition restores the previous
three explicitly raw calls; reverting the earlier current owner/handle binder
restores the pre-E04-S03 chat path. No config, persisted session, or wire schema
migration is involved.
