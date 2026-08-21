//! Closed identity and descriptor-fingerprint fixtures for external tools.

#![allow(dead_code)]

use std::collections::{HashMap, HashSet};
use std::num::NonZeroU64;

use kitsu_providers::ToolDescriptor;
use ring::digest::{digest, SHA256};
use serde_json::Value;
use thiserror::Error;

pub(crate) const EXTERNAL_IDENTITY_CONTRACT_VERSION_V1: u32 = 1;
pub(crate) const MAX_MACHINE_IDENTIFIER_BYTES: usize = 64;
const MAX_EXTERNAL_TOOLS: usize = 256;
const MAX_DESCRIPTOR_NAME_BYTES: usize = 128;
const MAX_DESCRIPTOR_DESCRIPTION_BYTES: usize = 4 * 1024;
const MAX_DESCRIPTOR_SCHEMA_BYTES: usize = 64 * 1024;
const MAX_DESCRIPTOR_SCHEMA_DEPTH: usize = 32;
const MAX_DESCRIPTOR_SCHEMA_NODES: usize = 4 * 1024;
const FINGERPRINT_DOMAIN_V1: &[u8] = b"kitsu.external-tool-descriptor-fingerprint.v1";

/// A stable source label assigned by the host, not accepted from a child.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct ExternalSourceIdV1(String);

/// One host-assigned lifetime of a source process or equivalent endpoint.
///
/// This is deliberately not an inventory revision: restarting a source and
/// publishing a new inventory are independent identity dimensions.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct ExternalSourceIncarnationV1(String);

/// A non-zero host revision within one source incarnation.
///
/// This value provides type separation and ordering. A later publication card
/// must compare it with prior state before it can claim monotonicity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct ExternalInventoryRevisionV1(NonZeroU64);

/// A stable host label for one tool within a source.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct ExternalToolIdV1(String);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct ExternalSourceIdentityV1 {
    source_id: ExternalSourceIdV1,
    incarnation: ExternalSourceIncarnationV1,
}

/// Full invocation identity. An advertised descriptor name is never an
/// identity and therefore is intentionally absent from equality semantics.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct ExternalToolIdentityV1 {
    source: ExternalSourceIdentityV1,
    inventory_revision: ExternalInventoryRevisionV1,
    tool_id: ExternalToolIdV1,
}

/// SHA-256 over one domain-separated, length-framed canonical descriptor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct DescriptorFingerprintV1([u8; 32]);

/// Inert host facts. Construction does not validate, publish, or perform I/O.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ExternalToolCandidateV1 {
    tool_id: String,
    descriptor: ToolDescriptor,
}

impl ExternalToolCandidateV1 {
    pub(crate) fn new(tool_id: impl Into<String>, descriptor: ToolDescriptor) -> Self {
        Self {
            tool_id: tool_id.into(),
            descriptor,
        }
    }
}

/// One inert host-authored inventory proposal. No child-originated fact can
/// manufacture this value through a public or configuration-facing API.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ExternalInventoryCandidateV1 {
    contract_version: u32,
    source_id: String,
    source_incarnation: String,
    inventory_revision: u64,
    tools: Vec<ExternalToolCandidateV1>,
}

impl ExternalInventoryCandidateV1 {
    pub(crate) fn new(
        contract_version: u32,
        source_id: impl Into<String>,
        source_incarnation: impl Into<String>,
        inventory_revision: u64,
        tools: Vec<ExternalToolCandidateV1>,
    ) -> Self {
        Self {
            contract_version,
            source_id: source_id.into(),
            source_incarnation: source_incarnation.into(),
            inventory_revision,
            tools,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ValidatedExternalToolV1 {
    identity: ExternalToolIdentityV1,
    descriptor: ToolDescriptor,
    descriptor_fingerprint: DescriptorFingerprintV1,
}

impl ValidatedExternalToolV1 {
    pub(crate) fn identity(&self) -> &ExternalToolIdentityV1 {
        &self.identity
    }

    pub(crate) fn descriptor(&self) -> &ToolDescriptor {
        &self.descriptor
    }

    pub(crate) fn descriptor_fingerprint(&self) -> DescriptorFingerprintV1 {
        self.descriptor_fingerprint
    }
}

/// A complete staged result. Callers receive no partial value on failure.
#[derive(Debug, PartialEq)]
pub(crate) struct ValidatedExternalInventoryV1 {
    source: ExternalSourceIdentityV1,
    inventory_revision: ExternalInventoryRevisionV1,
    tools: Vec<ValidatedExternalToolV1>,
}

impl ValidatedExternalInventoryV1 {
    pub(crate) fn source(&self) -> &ExternalSourceIdentityV1 {
        &self.source
    }

    pub(crate) fn revision(&self) -> ExternalInventoryRevisionV1 {
        self.inventory_revision
    }

    pub(crate) fn tools(&self) -> &[ValidatedExternalToolV1] {
        &self.tools
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DescriptorSchemaErrorV1 {
    RootNotObject,
    TooDeep,
    TooManyNodes,
    TooLarge,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub(crate) enum ExternalIdentityErrorV1 {
    #[error("unsupported external identity contract version {observed}")]
    UnsupportedContractVersion { observed: u32 },
    #[error("invalid external source identifier")]
    InvalidSourceId,
    #[error("invalid external source incarnation")]
    InvalidSourceIncarnation,
    #[error("external inventory revision must be non-zero")]
    InvalidInventoryRevision,
    #[error("external inventory has {observed} tools; maximum is {max}")]
    TooManyTools { observed: usize, max: usize },
    #[error("external tool at index {index} has an invalid host identifier")]
    InvalidToolId { index: usize },
    #[error("external tool at index {index} has an invalid descriptor name")]
    InvalidDescriptorName { index: usize },
    #[error("external tool at index {index} has an invalid descriptor description")]
    InvalidDescriptorDescription { index: usize },
    #[error("external tool at index {index} has an invalid descriptor schema: {reason:?}")]
    InvalidDescriptorSchema {
        index: usize,
        reason: DescriptorSchemaErrorV1,
    },
    #[error("duplicate external tool identifier '{tool_id}'")]
    DuplicateToolId { tool_id: String },
    #[error("external descriptor name collision '{name}'")]
    DuplicateDescriptorName { name: String },
    #[error(
        "descriptor fingerprint collision between tool indexes {first_index} and {second_index}"
    )]
    DescriptorFingerprintCollision {
        first_index: usize,
        second_index: usize,
    },
}

pub(crate) fn validate_external_inventory_v1(
    candidate: ExternalInventoryCandidateV1,
) -> Result<ValidatedExternalInventoryV1, ExternalIdentityErrorV1> {
    validate_external_inventory_with_digest_impl_v1(candidate, |canonical| {
        let observed = digest(&SHA256, canonical);
        let mut bytes = [0_u8; 32];
        bytes.copy_from_slice(observed.as_ref());
        bytes
    })
}

fn validate_external_inventory_with_digest_impl_v1<D>(
    candidate: ExternalInventoryCandidateV1,
    digest_descriptor: D,
) -> Result<ValidatedExternalInventoryV1, ExternalIdentityErrorV1>
where
    D: Fn(&[u8]) -> [u8; 32],
{
    if candidate.contract_version != EXTERNAL_IDENTITY_CONTRACT_VERSION_V1 {
        return Err(ExternalIdentityErrorV1::UnsupportedContractVersion {
            observed: candidate.contract_version,
        });
    }
    let source_id = parse_machine_identifier(&candidate.source_id, MAX_MACHINE_IDENTIFIER_BYTES)
        .map(ExternalSourceIdV1)
        .ok_or(ExternalIdentityErrorV1::InvalidSourceId)?;
    let incarnation =
        parse_machine_identifier(&candidate.source_incarnation, MAX_MACHINE_IDENTIFIER_BYTES)
            .map(ExternalSourceIncarnationV1)
            .ok_or(ExternalIdentityErrorV1::InvalidSourceIncarnation)?;
    let inventory_revision = NonZeroU64::new(candidate.inventory_revision)
        .map(ExternalInventoryRevisionV1)
        .ok_or(ExternalIdentityErrorV1::InvalidInventoryRevision)?;
    if candidate.tools.len() > MAX_EXTERNAL_TOOLS {
        return Err(ExternalIdentityErrorV1::TooManyTools {
            observed: candidate.tools.len(),
            max: MAX_EXTERNAL_TOOLS,
        });
    }

    let source = ExternalSourceIdentityV1 {
        source_id,
        incarnation,
    };
    let mut tool_ids = HashSet::with_capacity(candidate.tools.len());
    let mut descriptor_names = HashSet::with_capacity(candidate.tools.len());
    let mut fingerprints: HashMap<DescriptorFingerprintV1, (usize, Vec<u8>)> =
        HashMap::with_capacity(candidate.tools.len());
    let mut tools = Vec::with_capacity(candidate.tools.len());

    for (index, candidate_tool) in candidate.tools.into_iter().enumerate() {
        let tool_id =
            parse_machine_identifier(&candidate_tool.tool_id, MAX_MACHINE_IDENTIFIER_BYTES)
                .map(ExternalToolIdV1)
                .ok_or(ExternalIdentityErrorV1::InvalidToolId { index })?;
        if !tool_ids.insert(tool_id.clone()) {
            return Err(ExternalIdentityErrorV1::DuplicateToolId {
                tool_id: candidate_tool.tool_id,
            });
        }
        validate_descriptor_v1(index, &candidate_tool.descriptor)?;
        if !descriptor_names.insert(candidate_tool.descriptor.name.clone()) {
            return Err(ExternalIdentityErrorV1::DuplicateDescriptorName {
                name: candidate_tool.descriptor.name,
            });
        }

        let canonical = canonical_descriptor_bytes_v1(&candidate_tool.descriptor);
        let fingerprint = DescriptorFingerprintV1(digest_descriptor(&canonical));
        if let Some((first_index, first_canonical)) = fingerprints.get(&fingerprint) {
            if first_canonical != &canonical {
                return Err(ExternalIdentityErrorV1::DescriptorFingerprintCollision {
                    first_index: *first_index,
                    second_index: index,
                });
            }
        } else {
            fingerprints.insert(fingerprint, (index, canonical));
        }

        tools.push(ValidatedExternalToolV1 {
            identity: ExternalToolIdentityV1 {
                source: source.clone(),
                inventory_revision,
                tool_id,
            },
            descriptor: candidate_tool.descriptor,
            descriptor_fingerprint: fingerprint,
        });
    }

    Ok(ValidatedExternalInventoryV1 {
        source,
        inventory_revision,
        tools,
    })
}

#[cfg(test)]
fn validate_external_inventory_with_digest_v1<D>(
    candidate: ExternalInventoryCandidateV1,
    digest_descriptor: D,
) -> Result<ValidatedExternalInventoryV1, ExternalIdentityErrorV1>
where
    D: Fn(&[u8]) -> [u8; 32],
{
    validate_external_inventory_with_digest_impl_v1(candidate, digest_descriptor)
}

fn parse_machine_identifier(value: &str, max_bytes: usize) -> Option<String> {
    if value.is_empty() || value.len() > max_bytes {
        return None;
    }
    let mut bytes = value.bytes();
    let first = bytes.next()?;
    if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
        return None;
    }
    if !bytes.all(|byte| {
        byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_' | b'.')
    }) {
        return None;
    }
    Some(value.to_string())
}

fn validate_descriptor_v1(
    index: usize,
    descriptor: &ToolDescriptor,
) -> Result<(), ExternalIdentityErrorV1> {
    if parse_machine_identifier(&descriptor.name, MAX_DESCRIPTOR_NAME_BYTES).is_none() {
        return Err(ExternalIdentityErrorV1::InvalidDescriptorName { index });
    }
    if descriptor.description.trim().is_empty()
        || descriptor.description.len() > MAX_DESCRIPTOR_DESCRIPTION_BYTES
        || descriptor
            .description
            .chars()
            .any(|character| character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
    {
        return Err(ExternalIdentityErrorV1::InvalidDescriptorDescription { index });
    }
    if !descriptor.parameters.is_object() {
        return Err(ExternalIdentityErrorV1::InvalidDescriptorSchema {
            index,
            reason: DescriptorSchemaErrorV1::RootNotObject,
        });
    }

    let mut nodes = 0_usize;
    validate_schema_shape_v1(&descriptor.parameters, 1, &mut nodes)
        .map_err(|reason| ExternalIdentityErrorV1::InvalidDescriptorSchema { index, reason })?;
    if canonical_json_bytes_v1(&descriptor.parameters).len() > MAX_DESCRIPTOR_SCHEMA_BYTES {
        return Err(ExternalIdentityErrorV1::InvalidDescriptorSchema {
            index,
            reason: DescriptorSchemaErrorV1::TooLarge,
        });
    }
    Ok(())
}

fn validate_schema_shape_v1(
    value: &Value,
    depth: usize,
    nodes: &mut usize,
) -> Result<(), DescriptorSchemaErrorV1> {
    if depth > MAX_DESCRIPTOR_SCHEMA_DEPTH {
        return Err(DescriptorSchemaErrorV1::TooDeep);
    }
    *nodes = nodes.saturating_add(1);
    if *nodes > MAX_DESCRIPTOR_SCHEMA_NODES {
        return Err(DescriptorSchemaErrorV1::TooManyNodes);
    }
    match value {
        Value::Array(values) => {
            for value in values {
                validate_schema_shape_v1(value, depth + 1, nodes)?;
            }
        }
        Value::Object(values) => {
            for value in values.values() {
                validate_schema_shape_v1(value, depth + 1, nodes)?;
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
    Ok(())
}

fn canonical_descriptor_bytes_v1(descriptor: &ToolDescriptor) -> Vec<u8> {
    let schema = canonical_json_bytes_v1(&descriptor.parameters);
    let mut canonical = Vec::with_capacity(
        FINGERPRINT_DOMAIN_V1.len()
            + descriptor.name.len()
            + descriptor.description.len()
            + schema.len()
            + 32,
    );
    append_framed_v1(&mut canonical, FINGERPRINT_DOMAIN_V1);
    append_framed_v1(&mut canonical, descriptor.name.as_bytes());
    append_framed_v1(&mut canonical, descriptor.description.as_bytes());
    append_framed_v1(&mut canonical, &schema);
    canonical
}

fn append_framed_v1(output: &mut Vec<u8>, value: &[u8]) {
    output.extend_from_slice(&(value.len() as u64).to_be_bytes());
    output.extend_from_slice(value);
}

fn canonical_json_bytes_v1(value: &Value) -> Vec<u8> {
    let mut output = Vec::new();
    write_canonical_json_v1(value, &mut output);
    output
}

fn write_canonical_json_v1(value: &Value, output: &mut Vec<u8>) {
    match value {
        Value::Null => output.extend_from_slice(b"null"),
        Value::Bool(value) => output.extend_from_slice(if *value { b"true" } else { b"false" }),
        Value::Number(value) => output.extend_from_slice(value.to_string().as_bytes()),
        Value::String(value) => {
            serde_json::to_writer(output, value).expect("writing JSON to Vec cannot fail");
        }
        Value::Array(values) => {
            output.push(b'[');
            for (index, value) in values.iter().enumerate() {
                if index != 0 {
                    output.push(b',');
                }
                write_canonical_json_v1(value, output);
            }
            output.push(b']');
        }
        Value::Object(values) => {
            output.push(b'{');
            let mut keys: Vec<&String> = values.keys().collect();
            keys.sort_unstable();
            for (index, key) in keys.into_iter().enumerate() {
                if index != 0 {
                    output.push(b',');
                }
                serde_json::to_writer(&mut *output, key)
                    .expect("writing JSON object key to Vec cannot fail");
                output.push(b':');
                write_canonical_json_v1(&values[key], output);
            }
            output.push(b'}');
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kitsu_providers::ToolDescriptor;

    fn descriptor(name: &str) -> ToolDescriptor {
        ToolDescriptor {
            name: name.to_string(),
            description: "Read one bounded fixture".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "limit": {"minimum": 1, "type": "integer"}
                },
                "required": ["path"]
            }),
        }
    }

    fn candidate() -> ExternalInventoryCandidateV1 {
        ExternalInventoryCandidateV1::new(
            EXTERNAL_IDENTITY_CONTRACT_VERSION_V1,
            "local.echo",
            "boot-7f3a",
            41,
            vec![ExternalToolCandidateV1::new(
                "read-fixture",
                descriptor("read_fixture"),
            )],
        )
    }

    #[test]
    fn closed_identifiers_reject_empty_unsafe_and_oversized_values() {
        for source_id in ["", ".hidden", "space name", "UPPER", "source/name"] {
            let mut raw = candidate();
            raw.source_id = source_id.to_string();
            assert_eq!(
                validate_external_inventory_v1(raw),
                Err(ExternalIdentityErrorV1::InvalidSourceId)
            );
        }

        let mut oversized = candidate();
        oversized.tools[0].tool_id = "x".repeat(MAX_MACHINE_IDENTIFIER_BYTES + 1);
        assert_eq!(
            validate_external_inventory_v1(oversized),
            Err(ExternalIdentityErrorV1::InvalidToolId { index: 0 })
        );

        let mut empty_incarnation = candidate();
        empty_incarnation.source_incarnation.clear();
        assert_eq!(
            validate_external_inventory_v1(empty_incarnation),
            Err(ExternalIdentityErrorV1::InvalidSourceIncarnation)
        );
    }

    #[test]
    fn unknown_contract_and_zero_revision_are_rejected() {
        let mut unknown = candidate();
        unknown.contract_version = EXTERNAL_IDENTITY_CONTRACT_VERSION_V1 + 1;
        assert_eq!(
            validate_external_inventory_v1(unknown),
            Err(ExternalIdentityErrorV1::UnsupportedContractVersion {
                observed: EXTERNAL_IDENTITY_CONTRACT_VERSION_V1 + 1,
            })
        );

        let mut zero_revision = candidate();
        zero_revision.inventory_revision = 0;
        assert_eq!(
            validate_external_inventory_v1(zero_revision),
            Err(ExternalIdentityErrorV1::InvalidInventoryRevision)
        );
    }

    #[test]
    fn identity_is_not_an_advertised_name_and_binds_every_generation_dimension() {
        let first = validate_external_inventory_v1(candidate()).unwrap();

        let mut other_source = candidate();
        other_source.source_id = "local.other".to_string();
        let other_source = validate_external_inventory_v1(other_source).unwrap();

        let mut other_incarnation = candidate();
        other_incarnation.source_incarnation = "boot-8b4c".to_string();
        let other_incarnation = validate_external_inventory_v1(other_incarnation).unwrap();

        let mut other_revision = candidate();
        other_revision.inventory_revision = 42;
        let other_revision = validate_external_inventory_v1(other_revision).unwrap();

        let mut other_tool = candidate();
        other_tool.tools[0].tool_id = "read-other".to_string();
        let other_tool = validate_external_inventory_v1(other_tool).unwrap();

        let identity = &first.tools()[0].identity;
        assert_eq!(first.tools()[0].descriptor.name, "read_fixture");
        assert_ne!(identity, &other_source.tools()[0].identity);
        assert_ne!(identity, &other_incarnation.tools()[0].identity);
        assert_ne!(identity, &other_revision.tools()[0].identity);
        assert_ne!(identity, &other_tool.tools()[0].identity);

        fn incarnation_is_a_distinct_type(_: &ExternalSourceIncarnationV1) {}
        fn revision_is_a_distinct_type(_: &ExternalInventoryRevisionV1) {}
        incarnation_is_a_distinct_type(&identity.source.incarnation);
        revision_is_a_distinct_type(&identity.inventory_revision);
    }

    #[test]
    fn canonical_fingerprint_ignores_only_json_object_key_order() {
        let first = validate_external_inventory_v1(candidate()).unwrap();
        let first_fingerprint = first.tools()[0].descriptor_fingerprint;

        let mut reordered = candidate();
        reordered.tools[0].descriptor.parameters = serde_json::json!({
            "required": ["path"],
            "properties": {
                "limit": {"type": "integer", "minimum": 1},
                "path": {"type": "string"}
            },
            "type": "object"
        });
        let reordered = validate_external_inventory_v1(reordered).unwrap();
        assert_eq!(
            first_fingerprint,
            reordered.tools()[0].descriptor_fingerprint
        );
    }

    #[test]
    fn every_authority_relevant_descriptor_change_changes_the_fingerprint() {
        let first = validate_external_inventory_v1(candidate()).unwrap();
        let first_fingerprint = first.tools()[0].descriptor_fingerprint;

        let mut changed_name = candidate();
        changed_name.tools[0].descriptor.name = "read_other".to_string();
        let mut changed_description = candidate();
        changed_description.tools[0].descriptor.description = "Different authority".to_string();
        let mut changed_schema = candidate();
        changed_schema.tools[0].descriptor.parameters["properties"]["path"]["maxLength"] =
            serde_json::json!(128);

        for changed in [changed_name, changed_description, changed_schema] {
            assert_ne!(
                first_fingerprint,
                validate_external_inventory_v1(changed).unwrap().tools()[0].descriptor_fingerprint
            );
        }
    }

    #[test]
    fn malformed_descriptors_are_rejected_before_a_validated_inventory_exists() {
        let mut invalid_name = candidate();
        invalid_name.tools[0].descriptor.name = "bad name".to_string();
        assert_eq!(
            validate_external_inventory_v1(invalid_name),
            Err(ExternalIdentityErrorV1::InvalidDescriptorName { index: 0 })
        );

        let mut empty_description = candidate();
        empty_description.tools[0].descriptor.description.clear();
        assert_eq!(
            validate_external_inventory_v1(empty_description),
            Err(ExternalIdentityErrorV1::InvalidDescriptorDescription { index: 0 })
        );

        let mut non_object_schema = candidate();
        non_object_schema.tools[0].descriptor.parameters = serde_json::json!([]);
        assert_eq!(
            validate_external_inventory_v1(non_object_schema),
            Err(ExternalIdentityErrorV1::InvalidDescriptorSchema {
                index: 0,
                reason: DescriptorSchemaErrorV1::RootNotObject,
            })
        );
    }

    #[test]
    fn inventory_and_schema_resource_bounds_fail_before_publication() {
        let mut too_many = candidate();
        too_many.tools = vec![too_many.tools[0].clone(); MAX_EXTERNAL_TOOLS + 1];
        assert_eq!(
            validate_external_inventory_v1(too_many),
            Err(ExternalIdentityErrorV1::TooManyTools {
                observed: MAX_EXTERNAL_TOOLS + 1,
                max: MAX_EXTERNAL_TOOLS,
            })
        );

        let mut too_deep = candidate();
        let mut schema = serde_json::json!({"type": "string"});
        for _ in 0..MAX_DESCRIPTOR_SCHEMA_DEPTH {
            schema = serde_json::json!({"nested": schema});
        }
        too_deep.tools[0].descriptor.parameters = schema;
        assert_eq!(
            validate_external_inventory_v1(too_deep),
            Err(ExternalIdentityErrorV1::InvalidDescriptorSchema {
                index: 0,
                reason: DescriptorSchemaErrorV1::TooDeep,
            })
        );

        let mut too_large = candidate();
        too_large.tools[0].descriptor.parameters = serde_json::json!({
            "description": "x".repeat(MAX_DESCRIPTOR_SCHEMA_BYTES),
            "type": "object"
        });
        assert_eq!(
            validate_external_inventory_v1(too_large),
            Err(ExternalIdentityErrorV1::InvalidDescriptorSchema {
                index: 0,
                reason: DescriptorSchemaErrorV1::TooLarge,
            })
        );
    }

    #[test]
    fn duplicate_identity_and_advertised_name_collisions_fail_closed() {
        let mut duplicate_identity = candidate();
        duplicate_identity.tools.push(ExternalToolCandidateV1::new(
            "read-fixture",
            descriptor("other_name"),
        ));
        assert_eq!(
            validate_external_inventory_v1(duplicate_identity),
            Err(ExternalIdentityErrorV1::DuplicateToolId {
                tool_id: "read-fixture".to_string(),
            })
        );

        let mut name_collision = candidate();
        name_collision.tools.push(ExternalToolCandidateV1::new(
            "other-id",
            descriptor("read_fixture"),
        ));
        assert_eq!(
            validate_external_inventory_v1(name_collision),
            Err(ExternalIdentityErrorV1::DuplicateDescriptorName {
                name: "read_fixture".to_string(),
            })
        );
    }

    #[test]
    fn digest_collision_between_distinct_canonical_descriptors_is_rejected() {
        let mut raw = candidate();
        raw.tools.push(ExternalToolCandidateV1::new(
            "other-id",
            descriptor("other_name"),
        ));
        assert_eq!(
            validate_external_inventory_with_digest_v1(raw, |_| [0x5a; 32]),
            Err(ExternalIdentityErrorV1::DescriptorFingerprintCollision {
                first_index: 0,
                second_index: 1,
            })
        );
    }
}
