# Provider route value contract

Status: E04-S02/S03 are accepted through Cycle 038; E04-S04 is next.
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
production sends, and closes E04-S03. Native Messages remains E04-S04 and
complete route receipts remain E04-S05.

## Decision

Kitsu owns a closed, versioned, provider-neutral identity for one immutable
provider route snapshot:

```text
{ contract_revision, adapter, profile, generation, wire_revision, capabilities }
```

The serialized value is safe for receipts and deterministic fixtures. It never
contains an endpoint, credential, auth header, request/response body, model
content, provider config object, or user-facing gateway label.

The current compiled IDs reserve compatible Chat and Anthropic Messages wire
revisions. Reserving the Messages ID is not adapter promotion: composition may
publish it only after E04-S04 implements the native adapter and passes the
common wire suite. Until then, every current flavor truthfully identifies the
compatible-Chat implementation it actually constructs.

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
  rejects any inference capability before publication.
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
- `ProviderRouteSnapshot<P>` owns either a model-bound inference generation or
  a catalog-only management generation, plus secret-free auth source, policy,
  and its provider-specific runtimes. Catalog-only construction owns no model
  and no inference runtime. The runtime snapshot is intentionally neither
  serializable nor debuggable; endpoint, paths, caches, and sensitive auth
  remain inside `P`.
- `FrozenAuth` resolves a sensitive header once into zeroizing storage. All
  inference retries and catalog calls owned by the snapshot share that same
  auth object; later environment changes cannot alter an admitted generation.
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
successor rather than mutate a generation in place. These cards still do not:

- emit the complete adapter/profile/wire/capability/auth-source attempt receipt
  required by E04-S05;
- implement or advertise a native Messages adapter;
- expose provider account, billing, quota, endpoint, or credential data.

`rg` confirms `kitsu-core` does not import the route contract and the CLI has no
flavor-to-adapter switch. There is no title-generation provider call in current
Kitsu. Production composition has no raw `OpenAICompatProvider::new` call; the
constructor is confined to compiled provider factories and provider tests. The
next card begins the native Messages text/usage/terminal adapter against the
already pinned primary fixture without selecting it in live composition.

## Acceptance and rollback

Provider-free gates:

```bash
cargo test -p kitsu-providers --test provider_route_contract
cargo test -p kitsu-providers --test provider_route_declaration
cargo test -p kitsu-providers --test provider_route_snapshot
cargo test -p kitsu-providers --test provider_route_handle
cargo test -p kitsu-providers --test provider_wire_matrix
```

Six route-contract, three declaration/derivation, four snapshot, twelve
route-handle, compatible-Chat/catalog route-factory, auth-freeze/source-
selection, doctor/catalog-without-model, generation-aware
compaction/subagent/replay/JSONL, and sixteen wire-matrix cases pass offline. The
derivation vector is independently checked
against `sha256sum`; symlink aliases resolve to one canonical namespace, while
different config namespaces or gateway keys do not share identity. Reverting
the catalog-only types/factory and management composition restores the previous
three explicitly raw calls; reverting the earlier current owner/handle binder
restores the pre-E04-S03 chat path. No config, persisted session, or wire schema
migration is involved.
