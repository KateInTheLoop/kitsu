# Provider route value contract

Status: E04-S02 accepted through Cycle 033. Cycle 032 establishes the value
contract; Cycle 033 derives opaque profile identity and makes live composition
retain the truthful declaration. Generation publication and physical send
binding remain E04-S03 work.

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
  input.
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

The exact v1 JSON vector and all validation cuts are pinned by
`provider_route_contract.rs`. The provider wire matrix consumes the same
production capability vocabulary for its unsupported-image zero-send case.

## Boundary and non-goals

Cycle 032 defines immutable values and a pure admission operation. Cycle 033
adds bounded derivation and retains one private declaration in `ChatSetup`
before constructing the two compatible-Chat provider instances. All accepted
`openai`, `anthropic`, and `ollama` flavor labels declare the compatible-Chat
adapter/wire Kitsu actually constructs; no flavor label selects a physical
adapter. These cards still do not:

- publish or replace a live route generation;
- bind endpoint, model, auth, retry, replay, catalog, title, compaction, or
  auxiliary calls to that generation;
- alter provider construction or any physical send path;
- implement or advertise a native Messages adapter;
- expose account, billing, quota, or credential data.

`rg` confirms `kitsu-core` does not import the route contract and the CLI has no
flavor-to-adapter switch. The declaration is retained privately and is not yet
represented as a generation receipt. E04-S03 adds immutable route publication
and binding; later cards add the distinct adapter and secret-free operational
receipts.

## Acceptance and rollback

Provider-free gates:

```bash
cargo test -p kitsu-providers --test provider_route_contract
cargo test -p kitsu-providers --test provider_route_declaration
cargo test -p kitsu-providers --test provider_wire_matrix
```

Six route-contract, three declaration/derivation, two composition, and sixteen
wire-matrix cases pass offline. The derivation vector is independently checked
against `sha256sum`; symlink aliases resolve to one canonical namespace, while
different config namespaces or gateway keys do not share identity. Removing
the declaration field/helper, derivation, dependency, tests, and documentation
is complete rollback because no send path is generation-bound yet.
