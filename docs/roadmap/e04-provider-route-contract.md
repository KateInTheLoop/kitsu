# Provider route value contract

Status: E04-S02-C1 accepted in Cycle 032; composition and physical send wiring
remain open.

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
  Cycle 033 will derive that opaque 128-bit payload from config namespace and
  profile identity; display names never become authority-bearing identity.
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

Cycle 032 defines immutable values and a pure admission operation only. It does
not:

- derive profile IDs from live configuration;
- publish or replace a live route generation;
- bind endpoint, model, auth, retry, replay, catalog, title, compaction, or
  auxiliary calls to that generation;
- alter provider construction or any physical send path;
- implement or advertise a native Messages adapter;
- expose account, billing, quota, or credential data.

`rg` confirms `kitsu-core` and the CLI do not import the route contract in this
card. E04-S02-C2 will derive profile identity and make current composition
declare compatible Chat truthfully without changing send behavior. Later E04
cards add live admission, immutable route binding, the distinct adapter, and
secret-free operational receipts.

## Acceptance and rollback

Provider-free gates:

```bash
cargo test -p kitsu-providers --test provider_route_contract
cargo test -p kitsu-providers --test provider_wire_matrix
```

Six route-contract and sixteen wire-matrix cases pass offline. Removing the
module, re-export, handoff use, tests, and this documentation is complete
rollback because no production composition or send path consumes the values.
