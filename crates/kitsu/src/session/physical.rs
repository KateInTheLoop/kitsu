//! Private physical file-store v1 contract.
//!
//! This module deliberately defines records and paths without mounting them.
//! Current `SavedSession` v1 files remain authoritative until the explicit
//! migration card can construct a complete canonical logical value.

use std::ffi::OsStr;
use std::fmt;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

#[cfg(test)]
use std::cell::Cell;
#[cfg(unix)]
use std::os::fd::AsRawFd;

use kitsu_core::{ExpectedRevision, RevisionId, SessionId, SessionLookupState, StoreId};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::{
    atomic_write_private_in_detailed, fresh_id, remove_artifact_tree, PrivateDirectory,
    PrivateEntryKind,
};

pub(super) const PHYSICAL_SCHEMA_REVISION: u32 = 2;
pub(super) const MAX_PHYSICAL_RECORD_BYTES: usize = 64 * 1024;
pub(super) const MAX_CANONICAL_PAYLOAD_BYTES: u64 = 16 * 1024 * 1024;
pub(super) const MAX_IMMUTABLE_ARTIFACT_BYTES: u64 = 16 * 1024 * 1024;

const ROOT_COMPONENT: &str = ".kitsu-store-v1";
const STORE_RECORD_NAME: &str = "store.json";
const REVISIONS_COMPONENT: &str = "revisions";
const HEADS_COMPONENT: &str = "heads";
const ARTIFACTS_COMPONENT: &str = "artifacts";
const STAGING_COMPONENT: &str = "staging";
const LOCKS_COMPONENT: &str = ".locks";
const REVISION_MANIFEST_NAME: &str = "manifest.json";
const CANONICAL_PAYLOAD_NAME: &str = "canonical.json";
const ARTIFACT_MANIFEST_NAME: &str = "manifest.json";
const ARTIFACT_CONTENT_NAME: &str = "content.bin";
const COMPONENT_PREFIX: &str = "id-";
const BASE32: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PhysicalInstallMode {
    CreateNew,
    ReplaceAfterFenceAndRevisionRecheck,
}

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct PhysicalLayoutV1;

impl PhysicalLayoutV1 {
    pub(super) fn root(session_root: &Path) -> PathBuf {
        session_root.join(ROOT_COMPONENT)
    }

    pub(super) fn store_record(session_root: &Path) -> PathBuf {
        Self::root(session_root).join(STORE_RECORD_NAME)
    }

    pub(super) fn revisions_root(session_root: &Path) -> PathBuf {
        Self::root(session_root).join(REVISIONS_COMPONENT)
    }

    pub(super) fn revision_directory(
        session_root: &Path,
        session: &SessionId,
        revision: &RevisionId,
    ) -> PathBuf {
        Self::revisions_root(session_root)
            .join(encode_component(session.as_str()))
            .join(encode_component(revision.as_str()))
    }

    pub(super) fn revision_manifest(
        session_root: &Path,
        session: &SessionId,
        revision: &RevisionId,
    ) -> PathBuf {
        Self::revision_directory(session_root, session, revision).join(REVISION_MANIFEST_NAME)
    }

    pub(super) fn canonical_payload(
        session_root: &Path,
        session: &SessionId,
        revision: &RevisionId,
    ) -> PathBuf {
        Self::revision_directory(session_root, session, revision).join(CANONICAL_PAYLOAD_NAME)
    }

    pub(super) fn head_pointer(session_root: &Path, session: &SessionId) -> PathBuf {
        Self::root(session_root)
            .join(HEADS_COMPONENT)
            .join(format!("{}.json", encode_component(session.as_str())))
    }

    pub(super) fn artifacts_root(session_root: &Path) -> PathBuf {
        Self::root(session_root).join(ARTIFACTS_COMPONENT)
    }

    pub(super) fn artifact_directory(
        session_root: &Path,
        session: &SessionId,
        artifact_id: &str,
    ) -> PathBuf {
        Self::artifacts_root(session_root)
            .join(encode_component(session.as_str()))
            .join(encode_component(artifact_id))
    }

    pub(super) fn artifact_manifest(
        session_root: &Path,
        session: &SessionId,
        artifact_id: &str,
    ) -> PathBuf {
        Self::artifact_directory(session_root, session, artifact_id).join(ARTIFACT_MANIFEST_NAME)
    }

    pub(super) fn artifact_content(
        session_root: &Path,
        session: &SessionId,
        artifact_id: &str,
    ) -> PathBuf {
        Self::artifact_directory(session_root, session, artifact_id).join(ARTIFACT_CONTENT_NAME)
    }

    pub(super) fn staging_root(session_root: &Path) -> PathBuf {
        Self::root(session_root).join(STAGING_COMPONENT)
    }

    pub(super) fn locks_root(session_root: &Path) -> PathBuf {
        Self::root(session_root).join(LOCKS_COMPONENT)
    }

    pub(super) const fn store_install_mode() -> PhysicalInstallMode {
        PhysicalInstallMode::CreateNew
    }

    pub(super) const fn revision_install_mode() -> PhysicalInstallMode {
        PhysicalInstallMode::CreateNew
    }

    pub(super) const fn artifact_install_mode() -> PhysicalInstallMode {
        PhysicalInstallMode::CreateNew
    }

    pub(super) const fn head_install_mode() -> PhysicalInstallMode {
        PhysicalInstallMode::ReplaceAfterFenceAndRevisionRecheck
    }
}

fn encode_component(value: &str) -> String {
    let mut encoded = String::with_capacity(COMPONENT_PREFIX.len() + (value.len() * 8).div_ceil(5));
    encoded.push_str(COMPONENT_PREFIX);
    let mut accumulator = 0_u32;
    let mut bits = 0_u8;
    for byte in value.bytes() {
        accumulator = (accumulator << 8) | u32::from(byte);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            let index = ((accumulator >> bits) & 0x1f) as usize;
            encoded.push(BASE32[index] as char);
        }
    }
    if bits > 0 {
        let index = ((accumulator << (5 - bits)) & 0x1f) as usize;
        encoded.push(BASE32[index] as char);
    }
    encoded
}

fn decode_component(component: &str) -> Result<String, String> {
    let payload = component
        .strip_prefix(COMPONENT_PREFIX)
        .filter(|payload| !payload.is_empty())
        .ok_or_else(|| "physical identity component has an invalid prefix".to_string())?;
    let mut decoded = Vec::with_capacity(payload.len() * 5 / 8);
    let mut accumulator = 0_u32;
    let mut bits = 0_u8;
    for byte in payload.bytes() {
        let Some(index) = BASE32.iter().position(|candidate| *candidate == byte) else {
            return Err(
                "physical identity component is not canonical lowercase base32".to_string(),
            );
        };
        accumulator = (accumulator << 5) | index as u32;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            decoded.push(((accumulator >> bits) & 0xff) as u8);
        }
    }
    if bits > 0 && accumulator & ((1_u32 << bits) - 1) != 0 {
        return Err("physical identity component has nonzero trailing bits".to_string());
    }
    let decoded = String::from_utf8(decoded)
        .map_err(|_| "physical identity component is not UTF-8".to_string())?;
    if encode_component(&decoded) != component {
        return Err("physical identity component is not canonical".to_string());
    }
    Ok(decoded)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Sha256Digest(Box<str>);

impl Sha256Digest {
    pub(super) fn new(value: impl Into<String>) -> Result<Self, String> {
        let value = value.into();
        if value.len() != 64
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
        {
            return Err(
                "SHA-256 digest must be exactly 64 lowercase hexadecimal bytes".to_string(),
            );
        }
        Ok(Self(value.into_boxed_str()))
    }

    pub(super) fn as_str(&self) -> &str {
        &self.0
    }
}

impl Serialize for Sha256Digest {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for Sha256Digest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RecordType {
    Store,
    RevisionManifest,
    ArtifactManifest,
    Head,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct StoreRecordV1 {
    record_type: RecordType,
    physical_schema_revision: u32,
    store_id: StoreId,
}

impl StoreRecordV1 {
    pub(super) fn new(store_id: StoreId) -> Self {
        Self {
            record_type: RecordType::Store,
            physical_schema_revision: PHYSICAL_SCHEMA_REVISION,
            store_id,
        }
    }

    pub(super) fn store_id(&self) -> &StoreId {
        &self.store_id
    }

    fn validate(self) -> Result<Self, String> {
        validate_common(
            self.record_type,
            RecordType::Store,
            self.physical_schema_revision,
        )?;
        Ok(self)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RevisionManifestV1 {
    record_type: RecordType,
    physical_schema_revision: u32,
    logical_schema_revision: u32,
    store_id: StoreId,
    session_id: SessionId,
    revision_id: RevisionId,
    parent_revision: ExpectedRevision,
    parent_manifest_sha256: Option<Sha256Digest>,
    canonical_bytes: u64,
    canonical_sha256: Sha256Digest,
}

impl RevisionManifestV1 {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        logical_schema_revision: u32,
        store_id: StoreId,
        session_id: SessionId,
        revision_id: RevisionId,
        parent_revision: ExpectedRevision,
        parent_manifest_sha256: Option<Sha256Digest>,
        canonical_bytes: u64,
        canonical_sha256: Sha256Digest,
    ) -> Result<Self, String> {
        Self {
            record_type: RecordType::RevisionManifest,
            physical_schema_revision: PHYSICAL_SCHEMA_REVISION,
            logical_schema_revision,
            store_id,
            session_id,
            revision_id,
            parent_revision,
            parent_manifest_sha256,
            canonical_bytes,
            canonical_sha256,
        }
        .validate()
    }

    fn validate(self) -> Result<Self, String> {
        validate_common(
            self.record_type,
            RecordType::RevisionManifest,
            self.physical_schema_revision,
        )?;
        if self.logical_schema_revision == 0 {
            return Err("logical schema revision must be nonzero".to_string());
        }
        if self.canonical_bytes == 0 || self.canonical_bytes > MAX_CANONICAL_PAYLOAD_BYTES {
            return Err(format!(
                "canonical payload length must be within 1..={MAX_CANONICAL_PAYLOAD_BYTES} bytes"
            ));
        }
        if matches!(
            &self.parent_revision,
            ExpectedRevision::Exact(parent) if parent == &self.revision_id
        ) {
            return Err("revision cannot name itself as its parent".to_string());
        }
        match (&self.parent_revision, &self.parent_manifest_sha256) {
            (ExpectedRevision::Missing, None) | (ExpectedRevision::Exact(_), Some(_)) => {}
            (ExpectedRevision::Missing, Some(_)) => {
                return Err("root revision cannot bind a parent manifest digest".to_string());
            }
            (ExpectedRevision::Exact(_), None) => {
                return Err(
                    "successor revision must bind its exact parent manifest digest".to_string(),
                );
            }
        }
        Ok(self)
    }

    pub(super) fn logical_schema_revision(&self) -> u32 {
        self.logical_schema_revision
    }

    pub(super) fn store_id(&self) -> &StoreId {
        &self.store_id
    }

    pub(super) fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    pub(super) fn revision_id(&self) -> &RevisionId {
        &self.revision_id
    }

    pub(super) fn parent_revision(&self) -> &ExpectedRevision {
        &self.parent_revision
    }

    pub(super) fn parent_manifest_sha256(&self) -> Option<&Sha256Digest> {
        self.parent_manifest_sha256.as_ref()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ArtifactManifestV1 {
    record_type: RecordType,
    physical_schema_revision: u32,
    artifact_schema_revision: u32,
    store_id: StoreId,
    session_id: SessionId,
    artifact_id: String,
    content_bytes: u64,
    content_sha256: Sha256Digest,
    media_type: Option<String>,
}

impl ArtifactManifestV1 {
    pub(super) fn new(
        artifact_schema_revision: u32,
        store_id: StoreId,
        session_id: SessionId,
        artifact_id: String,
        content_bytes: u64,
        content_sha256: Sha256Digest,
        media_type: Option<String>,
    ) -> Result<Self, String> {
        Self {
            record_type: RecordType::ArtifactManifest,
            physical_schema_revision: PHYSICAL_SCHEMA_REVISION,
            artifact_schema_revision,
            store_id,
            session_id,
            artifact_id,
            content_bytes,
            content_sha256,
            media_type,
        }
        .validate()
    }

    fn validate(self) -> Result<Self, String> {
        validate_common(
            self.record_type,
            RecordType::ArtifactManifest,
            self.physical_schema_revision,
        )?;
        if self.artifact_schema_revision == 0 {
            return Err("artifact schema revision must be nonzero".to_string());
        }
        validate_artifact_id(&self.artifact_id)?;
        if self.content_bytes > MAX_IMMUTABLE_ARTIFACT_BYTES {
            return Err(format!(
                "artifact content length exceeds {MAX_IMMUTABLE_ARTIFACT_BYTES} bytes"
            ));
        }
        if let Some(media_type) = &self.media_type {
            if media_type.is_empty() || media_type.len() > 256 {
                return Err("artifact media type length must be within 1..=256 bytes".to_string());
            }
            if media_type.bytes().any(|byte| byte.is_ascii_control()) {
                return Err("artifact media type contains a control character".to_string());
            }
        }
        Ok(self)
    }

    pub(super) fn artifact_schema_revision(&self) -> u32 {
        self.artifact_schema_revision
    }

    pub(super) fn artifact_id(&self) -> &str {
        &self.artifact_id
    }

    pub(super) fn content_bytes(&self) -> u64 {
        self.content_bytes
    }

    pub(super) fn content_sha256(&self) -> &Sha256Digest {
        &self.content_sha256
    }

    pub(super) fn media_type(&self) -> Option<&str> {
        self.media_type.as_deref()
    }
}

fn validate_artifact_id(value: &str) -> Result<(), String> {
    if value.is_empty() || value.len() > SessionId::MAX_BYTES {
        return Err(format!(
            "artifact identity length must be within 1..={} bytes",
            SessionId::MAX_BYTES
        ));
    }
    if let Some(index) = value.bytes().position(|byte| {
        !(byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
    }) {
        return Err(format!("artifact identity byte {index} is unsafe"));
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct HeadRecordV1 {
    record_type: RecordType,
    physical_schema_revision: u32,
    store_id: StoreId,
    session_id: SessionId,
    revision_id: RevisionId,
    revision_manifest_sha256: Sha256Digest,
}

impl HeadRecordV1 {
    pub(super) fn new(
        store_id: StoreId,
        session_id: SessionId,
        revision_id: RevisionId,
        revision_manifest_sha256: Sha256Digest,
    ) -> Self {
        Self {
            record_type: RecordType::Head,
            physical_schema_revision: PHYSICAL_SCHEMA_REVISION,
            store_id,
            session_id,
            revision_id,
            revision_manifest_sha256,
        }
    }

    fn validate(self) -> Result<Self, String> {
        validate_common(
            self.record_type,
            RecordType::Head,
            self.physical_schema_revision,
        )?;
        Ok(self)
    }
}

fn validate_common(observed: RecordType, expected: RecordType, schema: u32) -> Result<(), String> {
    if observed != expected {
        return Err(format!(
            "physical record type mismatch: expected {expected:?}, observed {observed:?}"
        ));
    }
    if schema != PHYSICAL_SCHEMA_REVISION {
        return Err(format!(
            "unsupported physical schema revision {schema}; expected {PHYSICAL_SCHEMA_REVISION}"
        ));
    }
    Ok(())
}

fn encode_record<T: Serialize>(record: &T) -> Result<Vec<u8>, String> {
    let mut bytes = serde_json::to_vec(record)
        .map_err(|error| format!("cannot encode physical session record: {error}"))?;
    bytes.push(b'\n');
    if bytes.len() > MAX_PHYSICAL_RECORD_BYTES {
        return Err("physical session record exceeds its byte limit".to_string());
    }
    Ok(bytes)
}

fn decode_record<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, String> {
    if bytes.is_empty() || bytes.len() > MAX_PHYSICAL_RECORD_BYTES {
        return Err("physical session record is empty or exceeds its byte limit".to_string());
    }
    serde_json::from_slice(bytes)
        .map_err(|error| format!("cannot decode physical session record: {error}"))
}

pub(super) fn encode_store_record(record: &StoreRecordV1) -> Result<Vec<u8>, String> {
    encode_record(record)
}

pub(super) fn decode_store_record(bytes: &[u8]) -> Result<StoreRecordV1, String> {
    let record = decode_record::<StoreRecordV1>(bytes)?.validate()?;
    Ok(record)
}

fn decode_store_record_classified(
    bytes: &[u8],
    expected_store: &StoreId,
) -> Result<StoreRecordV1, PhysicalReadErrorV1> {
    if bytes.is_empty() || bytes.len() > MAX_PHYSICAL_RECORD_BYTES {
        return Err(PhysicalReadErrorV1::Malformed(
            "physical session record is empty or exceeds its byte limit".to_string(),
        ));
    }
    let record: StoreRecordV1 = serde_json::from_slice(bytes).map_err(|error| {
        PhysicalReadErrorV1::Malformed(format!("cannot decode physical session record: {error}"))
    })?;
    if record.record_type != RecordType::Store {
        return Err(PhysicalReadErrorV1::Malformed(
            "physical store record has the wrong record type".to_string(),
        ));
    }
    if record.physical_schema_revision == 0 {
        return Err(PhysicalReadErrorV1::Malformed(
            "physical store schema must be nonzero".to_string(),
        ));
    }
    if record.physical_schema_revision != PHYSICAL_SCHEMA_REVISION {
        return Err(PhysicalReadErrorV1::UnsupportedPhysicalSchema {
            observed: record.physical_schema_revision,
            supported: PHYSICAL_SCHEMA_REVISION,
            message: format!(
                "unsupported physical store schema {}; expected {PHYSICAL_SCHEMA_REVISION}",
                record.physical_schema_revision
            ),
        });
    }
    let record = record.validate().map_err(PhysicalReadErrorV1::Malformed)?;
    if record.store_id() != expected_store {
        return Err(PhysicalReadErrorV1::IdentityMismatch(
            "physical store identity does not match the admitted store".to_string(),
        ));
    }
    Ok(record)
}

pub(super) fn encode_revision_manifest(record: &RevisionManifestV1) -> Result<Vec<u8>, String> {
    encode_record(record)
}

pub(super) fn decode_revision_manifest(
    bytes: &[u8],
    expected_store: &StoreId,
    expected_session: &SessionId,
    expected_revision: &RevisionId,
) -> Result<RevisionManifestV1, String> {
    decode_revision_manifest_classified(bytes, expected_store, expected_session, expected_revision)
        .map_err(|error| error.to_string())
}

fn decode_revision_manifest_classified(
    bytes: &[u8],
    expected_store: &StoreId,
    expected_session: &SessionId,
    expected_revision: &RevisionId,
) -> Result<RevisionManifestV1, PhysicalReadErrorV1> {
    if bytes.is_empty() || bytes.len() > MAX_PHYSICAL_RECORD_BYTES {
        return Err(PhysicalReadErrorV1::Malformed(
            "physical session record is empty or exceeds its byte limit".to_string(),
        ));
    }
    let record: RevisionManifestV1 = serde_json::from_slice(bytes).map_err(|error| {
        PhysicalReadErrorV1::Malformed(format!("cannot decode physical session record: {error}"))
    })?;
    if record.record_type != RecordType::RevisionManifest {
        return Err(PhysicalReadErrorV1::Malformed(
            "physical revision manifest has the wrong record type".to_string(),
        ));
    }
    if record.physical_schema_revision == 0 {
        return Err(PhysicalReadErrorV1::Malformed(
            "physical revision manifest schema must be nonzero".to_string(),
        ));
    }
    if record.physical_schema_revision != PHYSICAL_SCHEMA_REVISION {
        return Err(PhysicalReadErrorV1::UnsupportedPhysicalSchema {
            observed: record.physical_schema_revision,
            supported: PHYSICAL_SCHEMA_REVISION,
            message: format!(
                "unsupported physical revision manifest schema {}; expected {PHYSICAL_SCHEMA_REVISION}",
                record.physical_schema_revision
            ),
        });
    }
    let record = record.validate().map_err(PhysicalReadErrorV1::Malformed)?;
    if &record.store_id != expected_store
        || &record.session_id != expected_session
        || &record.revision_id != expected_revision
    {
        return Err(PhysicalReadErrorV1::IdentityMismatch(
            "revision manifest identity does not match its physical location".to_string(),
        ));
    }
    Ok(record)
}

pub(super) fn encode_artifact_manifest(record: &ArtifactManifestV1) -> Result<Vec<u8>, String> {
    encode_record(record)
}

pub(super) fn decode_artifact_manifest(
    bytes: &[u8],
    expected_store: &StoreId,
    expected_session: &SessionId,
    expected_artifact_id: &str,
) -> Result<ArtifactManifestV1, String> {
    validate_artifact_id(expected_artifact_id)?;
    let record = decode_record::<ArtifactManifestV1>(bytes)?.validate()?;
    if &record.store_id != expected_store
        || &record.session_id != expected_session
        || record.artifact_id != expected_artifact_id
    {
        return Err("artifact manifest identity does not match its physical location".to_string());
    }
    Ok(record)
}

pub(super) fn encode_head_record(record: &HeadRecordV1) -> Result<Vec<u8>, String> {
    encode_record(record)
}

pub(super) fn decode_head_record(
    bytes: &[u8],
    expected_store: &StoreId,
    expected_session: &SessionId,
) -> Result<HeadRecordV1, String> {
    decode_head_record_classified(bytes, expected_store, expected_session)
        .map_err(|error| error.to_string())
}

fn decode_head_record_classified(
    bytes: &[u8],
    expected_store: &StoreId,
    expected_session: &SessionId,
) -> Result<HeadRecordV1, PhysicalHeadReadErrorV1> {
    if bytes.is_empty() || bytes.len() > MAX_PHYSICAL_RECORD_BYTES {
        return Err(PhysicalHeadReadErrorV1::before_revision(
            PhysicalReadErrorV1::Malformed(
                "physical session record is empty or exceeds its byte limit".to_string(),
            ),
        ));
    }
    let record: HeadRecordV1 = serde_json::from_slice(bytes).map_err(|error| {
        PhysicalHeadReadErrorV1::before_revision(PhysicalReadErrorV1::Malformed(format!(
            "cannot decode physical session record: {error}"
        )))
    })?;
    let observed_revision = record.revision_id.clone();
    if record.record_type != RecordType::Head {
        return Err(PhysicalHeadReadErrorV1::at_revision(
            observed_revision,
            PhysicalReadErrorV1::Malformed("physical HEAD has the wrong record type".to_string()),
        ));
    }
    if record.physical_schema_revision == 0 {
        return Err(PhysicalHeadReadErrorV1::at_revision(
            observed_revision,
            PhysicalReadErrorV1::Malformed("physical HEAD schema must be nonzero".to_string()),
        ));
    }
    if record.physical_schema_revision != PHYSICAL_SCHEMA_REVISION {
        return Err(PhysicalHeadReadErrorV1::at_revision(
            observed_revision,
            PhysicalReadErrorV1::UnsupportedPhysicalSchema {
                observed: record.physical_schema_revision,
                supported: PHYSICAL_SCHEMA_REVISION,
                message: format!(
                    "unsupported physical HEAD schema {}; expected {PHYSICAL_SCHEMA_REVISION}",
                    record.physical_schema_revision
                ),
            },
        ));
    }
    let record = record.validate().map_err(|message| {
        PhysicalHeadReadErrorV1::at_revision(
            observed_revision.clone(),
            PhysicalReadErrorV1::Malformed(message),
        )
    })?;
    if &record.store_id != expected_store || &record.session_id != expected_session {
        return Err(PhysicalHeadReadErrorV1::at_revision(
            observed_revision,
            PhysicalReadErrorV1::IdentityMismatch(
                "head record identity does not match its physical location".to_string(),
            ),
        ));
    }
    Ok(record)
}

/// Descriptor-pinned owner of the unmounted physical-v1 namespace.
///
/// Holding each directory open prevents pathname ancestor replacement from
/// redirecting later revision reads or writes on Unix. The type remains
/// private until canonical HEAD publication and migration are complete.
#[derive(Debug)]
pub(super) struct PhysicalFileStoreV1 {
    _root: PrivateDirectory,
    revisions: PrivateDirectory,
    artifacts: PrivateDirectory,
    heads: PrivateDirectory,
    staging: PrivateDirectory,
    locks: PrivateDirectory,
    record: StoreRecordV1,
    #[cfg(test)]
    fail_next_target_sync: Cell<bool>,
    #[cfg(test)]
    fail_next_head_sync: Cell<bool>,
}

#[derive(Debug)]
pub(super) struct InstalledRevisionCandidateV1 {
    manifest: RevisionManifestV1,
    manifest_bytes: Vec<u8>,
    manifest_sha256: Sha256Digest,
}

#[derive(Debug)]
pub(super) struct InstalledArtifactV1 {
    manifest: ArtifactManifestV1,
    manifest_bytes: Vec<u8>,
    manifest_sha256: Sha256Digest,
}

impl InstalledArtifactV1 {
    pub(super) fn manifest(&self) -> &ArtifactManifestV1 {
        &self.manifest
    }

    pub(super) fn manifest_sha256(&self) -> &Sha256Digest {
        &self.manifest_sha256
    }
}

#[derive(Debug)]
pub(super) struct VerifiedArtifactV1 {
    manifest: ArtifactManifestV1,
    manifest_sha256: Sha256Digest,
    content_bytes: Vec<u8>,
}

#[derive(Debug)]
pub(super) enum PhysicalReadErrorV1 {
    MissingReference(String),
    Malformed(String),
    IdentityMismatch(String),
    DigestMismatch(String),
    UnsupportedPhysicalSchema {
        observed: u32,
        supported: u32,
        message: String,
    },
}

impl PhysicalReadErrorV1 {
    #[cfg(test)]
    fn contains(&self, needle: &str) -> bool {
        self.to_string().contains(needle)
    }
}

impl fmt::Display for PhysicalReadErrorV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingReference(message)
            | Self::Malformed(message)
            | Self::IdentityMismatch(message)
            | Self::DigestMismatch(message)
            | Self::UnsupportedPhysicalSchema { message, .. } => formatter.write_str(message),
        }
    }
}

impl std::error::Error for PhysicalReadErrorV1 {}

#[derive(Debug)]
pub(super) struct PhysicalHeadReadErrorV1 {
    observed_revision: Option<RevisionId>,
    source: PhysicalReadErrorV1,
}

impl PhysicalHeadReadErrorV1 {
    fn before_revision(source: PhysicalReadErrorV1) -> Self {
        Self {
            observed_revision: None,
            source,
        }
    }

    fn at_revision(revision: RevisionId, source: PhysicalReadErrorV1) -> Self {
        Self {
            observed_revision: Some(revision),
            source,
        }
    }

    pub(super) fn into_parts(self) -> (Option<RevisionId>, PhysicalReadErrorV1) {
        (self.observed_revision, self.source)
    }

    #[cfg(test)]
    fn contains(&self, needle: &str) -> bool {
        self.to_string().contains(needle)
    }
}

impl fmt::Display for PhysicalHeadReadErrorV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.source.fmt(formatter)
    }
}

impl std::error::Error for PhysicalHeadReadErrorV1 {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

#[derive(Debug)]
pub(super) enum PhysicalArtifactReadErrorV1 {
    Missing(String),
    Malformed(String),
    IdentityMismatch(String),
    DigestMismatch(String),
    UnsupportedPhysicalSchema {
        observed: u32,
        supported: u32,
        message: String,
    },
}

impl PhysicalArtifactReadErrorV1 {
    #[cfg(test)]
    fn contains(&self, needle: &str) -> bool {
        self.to_string().contains(needle)
    }
}

impl fmt::Display for PhysicalArtifactReadErrorV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing(message)
            | Self::Malformed(message)
            | Self::IdentityMismatch(message)
            | Self::DigestMismatch(message)
            | Self::UnsupportedPhysicalSchema { message, .. } => formatter.write_str(message),
        }
    }
}

impl std::error::Error for PhysicalArtifactReadErrorV1 {}

impl VerifiedArtifactV1 {
    pub(super) fn manifest(&self) -> &ArtifactManifestV1 {
        &self.manifest
    }

    pub(super) fn manifest_sha256(&self) -> &Sha256Digest {
        &self.manifest_sha256
    }

    pub(super) fn content_bytes(&self) -> &[u8] {
        &self.content_bytes
    }
}

impl InstalledRevisionCandidateV1 {
    pub(super) fn manifest(&self) -> &RevisionManifestV1 {
        &self.manifest
    }

    pub(super) fn manifest_sha256(&self) -> &Sha256Digest {
        &self.manifest_sha256
    }
}

#[derive(Debug)]
pub(super) struct VerifiedRevisionCandidateV1 {
    manifest: RevisionManifestV1,
    manifest_sha256: Sha256Digest,
    canonical_bytes: Vec<u8>,
}

/// A descriptor-pinned revision manifest whose bytes and physical identity are
/// verified, but whose canonical payload has not been trusted or decoded.
#[derive(Debug)]
pub(super) struct VerifiedRevisionEnvelopeV1 {
    directory: PrivateDirectory,
    manifest: RevisionManifestV1,
    manifest_sha256: Sha256Digest,
}

impl VerifiedRevisionEnvelopeV1 {
    pub(super) fn revision_id(&self) -> &RevisionId {
        self.manifest.revision_id()
    }

    pub(super) fn manifest(&self) -> &RevisionManifestV1 {
        &self.manifest
    }

    pub(super) fn manifest_sha256(&self) -> &Sha256Digest {
        &self.manifest_sha256
    }
}

#[derive(Debug)]
pub(super) struct PhysicalWriterFenceV1 {
    store_id: StoreId,
    session_id: SessionId,
    #[cfg(unix)]
    _file: std::fs::File,
}

impl PhysicalWriterFenceV1 {
    pub(super) fn confirm(&self, store_id: &StoreId, session_id: &SessionId) -> Result<(), String> {
        if &self.store_id != store_id {
            return Err(format!(
                "writer fence store mismatch: expected '{}', observed '{}'",
                store_id.as_str(),
                self.store_id.as_str()
            ));
        }
        if &self.session_id != session_id {
            return Err(format!(
                "writer fence session mismatch: expected '{}', observed '{}'",
                session_id.as_str(),
                self.session_id.as_str()
            ));
        }
        Ok(())
    }
}

#[derive(Debug)]
pub(super) struct VerifiedRevisionHeadV1 {
    record: HeadRecordV1,
    candidate: VerifiedRevisionCandidateV1,
}

impl VerifiedRevisionHeadV1 {
    pub(super) fn revision_id(&self) -> &RevisionId {
        &self.record.revision_id
    }

    pub(super) fn candidate(&self) -> &VerifiedRevisionCandidateV1 {
        &self.candidate
    }
}

#[derive(Debug)]
pub(super) struct VerifiedRevisionHeadEnvelopeV1 {
    record: HeadRecordV1,
    envelope: VerifiedRevisionEnvelopeV1,
}

impl VerifiedRevisionHeadEnvelopeV1 {
    pub(super) fn revision_id(&self) -> &RevisionId {
        &self.record.revision_id
    }

    pub(super) fn manifest(&self) -> &RevisionManifestV1 {
        self.envelope.manifest()
    }

    pub(super) fn envelope(&self) -> &VerifiedRevisionEnvelopeV1 {
        &self.envelope
    }
}

#[derive(Debug)]
pub(super) enum PhysicalHeadCasError {
    Fence(String),
    Conflict {
        expected: ExpectedRevision,
        observed: Option<RevisionId>,
    },
    Candidate(String),
    Publication {
        message: String,
        installed: bool,
    },
}

impl fmt::Display for PhysicalHeadCasError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Fence(message) | Self::Candidate(message) => formatter.write_str(message),
            Self::Conflict { expected, observed } => write!(
                formatter,
                "physical HEAD revision conflict: expected {expected:?}, observed {observed:?}"
            ),
            Self::Publication { message, .. } => formatter.write_str(message),
        }
    }
}

impl std::error::Error for PhysicalHeadCasError {}

impl VerifiedRevisionCandidateV1 {
    pub(super) fn manifest(&self) -> &RevisionManifestV1 {
        &self.manifest
    }

    pub(super) fn manifest_sha256(&self) -> &Sha256Digest {
        &self.manifest_sha256
    }

    pub(super) fn canonical_bytes(&self) -> &[u8] {
        &self.canonical_bytes
    }
}

#[derive(Debug)]
pub(super) struct PhysicalCandidateInstallError {
    message: String,
    installed: bool,
}

#[derive(Debug)]
pub(super) struct PhysicalArtifactInstallError {
    message: String,
    installed: bool,
}

impl PhysicalArtifactInstallError {
    fn before_install(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            installed: false,
        }
    }

    fn after_install(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            installed: true,
        }
    }

    pub(super) fn installed(&self) -> bool {
        self.installed
    }
}

impl fmt::Display for PhysicalArtifactInstallError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for PhysicalArtifactInstallError {}

impl PhysicalCandidateInstallError {
    fn before_install(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            installed: false,
        }
    }

    fn after_install(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            installed: true,
        }
    }

    pub(super) fn installed(&self) -> bool {
        self.installed
    }
}

impl fmt::Display for PhysicalCandidateInstallError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for PhysicalCandidateInstallError {}

impl PhysicalFileStoreV1 {
    /// Observe only whether any physical-store root entry exists beneath the
    /// pinned sessions directory. Local composition binding uses this before
    /// publishing its identity so an orphaned or partial physical namespace is
    /// never silently adopted or assigned a new identity.
    pub(super) fn namespace_entry_present(
        session_root: &PrivateDirectory,
    ) -> Result<bool, PhysicalReadErrorV1> {
        match session_root.entry_kind(OsStr::new(ROOT_COMPONENT)) {
            Ok(_) => Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(PhysicalReadErrorV1::Malformed(format!(
                "cannot inspect physical session store namespace '{}': {error}",
                session_root.path.join(ROOT_COMPONENT).display()
            ))),
        }
    }

    pub(super) fn create_or_open(session_root: &Path, store_id: StoreId) -> Result<Self, String> {
        let session_root = PrivateDirectory::prepare(session_root, "physical session root")?;
        let root = session_root.prepare_child(
            OsStr::new(ROOT_COMPONENT),
            "physical session store namespace",
        )?;
        Self::finish_store_initialization(&session_root, root, store_id)
    }

    /// Initialize or open the physical namespace relative to an already-pinned
    /// session directory. This is the activation-safe form: replacing the
    /// visible ancestor path cannot redirect store creation.
    pub(super) fn create_or_open_pinned(
        session_root: &PrivateDirectory,
        store_id: StoreId,
    ) -> Result<Self, String> {
        match Self::open_existing(session_root, store_id.clone()) {
            Ok(Some(store)) => return Ok(store),
            Ok(None) => {}
            Err(error) => return Err(error.to_string()),
        }
        let root = match session_root.create_child_exclusive(
            OsStr::new(ROOT_COMPONENT),
            "physical session store namespace",
        ) {
            Ok(root) => root,
            Err(create_error) => {
                // A concurrent initializer may have won after the absence
                // observation. Adopt it only if the entire closed namespace is
                // already valid; never fill in or repair a partial winner.
                return match Self::open_existing(session_root, store_id) {
                    Ok(Some(store)) => Ok(store),
                    Ok(None) => Err(create_error),
                    Err(error) => Err(error.to_string()),
                };
            }
        };
        Self::finish_store_initialization(session_root, root, store_id)
    }

    fn finish_store_initialization(
        session_root: &PrivateDirectory,
        root: PrivateDirectory,
        store_id: StoreId,
    ) -> Result<Self, String> {
        session_root.sync("physical session root")?;

        let record = open_or_create_store_record(&root, store_id)?;
        let revisions = root.prepare_child(
            OsStr::new(REVISIONS_COMPONENT),
            "physical revision namespace",
        )?;
        let artifacts = root.prepare_child(
            OsStr::new(ARTIFACTS_COMPONENT),
            "physical artifact namespace",
        )?;
        let heads = root.prepare_child(OsStr::new(HEADS_COMPONENT), "physical HEAD namespace")?;
        let staging = root.prepare_child(
            OsStr::new(STAGING_COMPONENT),
            "physical revision staging namespace",
        )?;
        let locks = root.prepare_child(OsStr::new(LOCKS_COMPONENT), "physical lock namespace")?;
        root.sync("physical session store namespace")?;

        Ok(Self {
            _root: root,
            revisions,
            artifacts,
            heads,
            staging,
            locks,
            record,
            #[cfg(test)]
            fail_next_target_sync: Cell::new(false),
            #[cfg(test)]
            fail_next_head_sync: Cell::new(false),
        })
    }

    /// Open a complete existing physical store without creating, securing,
    /// syncing, repairing, or locking any path. Absence of the disjoint store
    /// root is ordinary `None`; any partial or invalid existing namespace is a
    /// typed fail-closed read error.
    pub(super) fn open_existing(
        session_root: &PrivateDirectory,
        store_id: StoreId,
    ) -> Result<Option<Self>, PhysicalReadErrorV1> {
        let Some(root) = session_root
            .open_optional_child(
                OsStr::new(ROOT_COMPONENT),
                "physical session store namespace",
            )
            .map_err(PhysicalReadErrorV1::Malformed)?
        else {
            return Ok(None);
        };
        let bytes = read_bounded_regular(
            &root,
            OsStr::new(STORE_RECORD_NAME),
            MAX_PHYSICAL_RECORD_BYTES as u64,
            "physical store record",
        )
        .map_err(PhysicalReadErrorV1::Malformed)?;
        let record = decode_store_record_classified(&bytes, &store_id)?;
        let open_namespace = |name: &str, context: &str| {
            root.open_child(OsStr::new(name), context)
                .map_err(PhysicalReadErrorV1::Malformed)
        };
        let revisions = open_namespace(REVISIONS_COMPONENT, "physical revision namespace")?;
        let artifacts = open_namespace(ARTIFACTS_COMPONENT, "physical artifact namespace")?;
        let heads = open_namespace(HEADS_COMPONENT, "physical HEAD namespace")?;
        let staging = open_namespace(STAGING_COMPONENT, "physical revision staging namespace")?;
        let locks = open_namespace(LOCKS_COMPONENT, "physical lock namespace")?;

        Ok(Some(Self {
            _root: root,
            revisions,
            artifacts,
            heads,
            staging,
            locks,
            record,
            #[cfg(test)]
            fail_next_target_sync: Cell::new(false),
            #[cfg(test)]
            fail_next_head_sync: Cell::new(false),
        }))
    }

    pub(super) fn store_id(&self) -> &StoreId {
        self.record.store_id()
    }

    pub(super) fn acquire_writer_fence(
        &self,
        session_id: &SessionId,
    ) -> Result<PhysicalWriterFenceV1, String> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let lock_name = format!("{}.lock", encode_component(session_id.as_str()));
            let lock_path = self.locks.path.join(&lock_name);
            let file = super::openat_file(
                &self.locks.file,
                OsStr::new(&lock_name),
                libc::O_RDWR
                    | libc::O_CREAT
                    | libc::O_NOFOLLOW
                    | libc::O_NONBLOCK
                    | libc::O_CLOEXEC,
                0o600,
            )
            .map_err(|error| {
                if error.raw_os_error() == Some(libc::ELOOP) {
                    format!(
                        "physical writer lock '{}' is not a regular file",
                        lock_path.display()
                    )
                } else {
                    format!(
                        "cannot open physical writer lock '{}': {error}",
                        lock_path.display()
                    )
                }
            })?;
            let metadata = file.metadata().map_err(|error| {
                format!(
                    "cannot inspect physical writer lock '{}': {error}",
                    lock_path.display()
                )
            })?;
            if !metadata.is_file() {
                return Err(format!(
                    "physical writer lock '{}' is not a regular file",
                    lock_path.display()
                ));
            }
            let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
            if result != 0 {
                let error = std::io::Error::last_os_error();
                if error.raw_os_error() == Some(libc::EWOULDBLOCK)
                    || error.raw_os_error() == Some(libc::EAGAIN)
                {
                    return Err(format!(
                        "physical session '{}' is already active in this or another process",
                        session_id.as_str()
                    ));
                }
                return Err(format!(
                    "cannot acquire physical writer lock for session '{}': {error}",
                    session_id.as_str()
                ));
            }
            file.set_permissions(std::fs::Permissions::from_mode(0o600))
                .map_err(|error| {
                    format!(
                        "cannot secure physical writer lock '{}': {error}",
                        lock_path.display()
                    )
                })?;
            self.locks.sync("physical lock namespace")?;
            Ok(PhysicalWriterFenceV1 {
                store_id: self.store_id().clone(),
                session_id: session_id.clone(),
                _file: file,
            })
        }

        #[cfg(not(unix))]
        {
            let _ = session_id;
            Err("physical writer fencing is unavailable on this platform".to_string())
        }
    }

    pub(super) fn read_head(
        &self,
        session_id: &SessionId,
    ) -> Result<Option<VerifiedRevisionHeadV1>, PhysicalHeadReadErrorV1> {
        let Some(head) = self.read_head_envelope(session_id)? else {
            return Ok(None);
        };
        let revision_id = head.revision_id().clone();
        let candidate = self
            .verify_revision_candidate(head.envelope())
            .map_err(|error| PhysicalHeadReadErrorV1::at_revision(revision_id, error))?;
        Ok(Some(VerifiedRevisionHeadV1 {
            record: head.record,
            candidate,
        }))
    }

    pub(super) fn read_head_envelope(
        &self,
        session_id: &SessionId,
    ) -> Result<Option<VerifiedRevisionHeadEnvelopeV1>, PhysicalHeadReadErrorV1> {
        let head_name = format!("{}.json", encode_component(session_id.as_str()));
        match self.heads.entry_kind(OsStr::new(&head_name)) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Ok(PrivateEntryKind::File) => {}
            Ok(_) => {
                return Err(PhysicalHeadReadErrorV1::before_revision(
                    PhysicalReadErrorV1::Malformed(format!(
                        "physical HEAD '{}' is not a regular file",
                        self.heads.path.join(&head_name).display()
                    )),
                ));
            }
            Err(error) => {
                return Err(PhysicalHeadReadErrorV1::before_revision(
                    PhysicalReadErrorV1::Malformed(format!(
                        "cannot inspect physical HEAD '{}': {error}",
                        self.heads.path.join(&head_name).display()
                    )),
                ));
            }
        }
        let bytes = read_bounded_regular(
            &self.heads,
            OsStr::new(&head_name),
            MAX_PHYSICAL_RECORD_BYTES as u64,
            "physical HEAD",
        )
        .map_err(|message| {
            PhysicalHeadReadErrorV1::before_revision(PhysicalReadErrorV1::Malformed(message))
        })?;
        let record = decode_head_record_classified(&bytes, self.store_id(), session_id)?;
        let envelope = self
            .open_revision_envelope(session_id, &record.revision_id)
            .map_err(|error| {
                PhysicalHeadReadErrorV1::at_revision(record.revision_id.clone(), error)
            })?;
        if envelope.manifest_sha256 != record.revision_manifest_sha256 {
            return Err(PhysicalHeadReadErrorV1::at_revision(
                record.revision_id.clone(),
                PhysicalReadErrorV1::DigestMismatch(format!(
                    "physical HEAD manifest digest mismatch: HEAD declares {}, observed {}",
                    record.revision_manifest_sha256.as_str(),
                    envelope.manifest_sha256.as_str()
                )),
            ));
        }
        Ok(Some(VerifiedRevisionHeadEnvelopeV1 { record, envelope }))
    }

    pub(super) fn compare_and_swap_head(
        &self,
        fence: &PhysicalWriterFenceV1,
        expected: ExpectedRevision,
        candidate: &InstalledRevisionCandidateV1,
    ) -> Result<VerifiedRevisionHeadV1, PhysicalHeadCasError> {
        let session_id = &candidate.manifest.session_id;
        self.validate_head_candidate(fence, candidate)?;

        let current = self
            .read_head(session_id)
            .map_err(|error| PhysicalHeadCasError::Candidate(error.to_string()))?;
        let observed = current.as_ref().map(VerifiedRevisionHeadV1::revision_id);
        match expected.admit_observed(observed) {
            Ok(()) => {}
            Err(SessionLookupState::Conflict { expected, observed }) => {
                return Err(PhysicalHeadCasError::Conflict { expected, observed });
            }
            Err(other) => {
                return Err(PhysicalHeadCasError::Candidate(format!(
                    "physical HEAD admission returned an invalid state: {other:?}"
                )));
            }
        }
        if candidate.manifest.parent_revision != expected {
            return Err(PhysicalHeadCasError::Candidate(format!(
                "revision candidate parent {:?} does not match admitted HEAD expectation {expected:?}",
                candidate.manifest.parent_revision
            )));
        }
        if let ExpectedRevision::Exact(_) = &expected {
            let observed_parent_manifest = current
                .as_ref()
                .expect("exact expected revision was admitted against a present HEAD")
                .candidate()
                .manifest_sha256();
            if candidate.manifest.parent_manifest_sha256() != Some(observed_parent_manifest) {
                return Err(PhysicalHeadCasError::Candidate(
                    "revision candidate parent digest does not match the exact admitted HEAD manifest"
                        .to_string(),
                ));
            }
        }

        self.publish_candidate_head(
            fence,
            candidate,
            matches!(expected, ExpectedRevision::Exact(_)),
        )
    }

    pub(super) fn compare_and_swap_head_against_envelope(
        &self,
        fence: &PhysicalWriterFenceV1,
        selected: &VerifiedRevisionHeadEnvelopeV1,
        candidate: &InstalledRevisionCandidateV1,
    ) -> Result<VerifiedRevisionHeadV1, PhysicalHeadCasError> {
        let session_id = &candidate.manifest.session_id;
        self.validate_head_candidate(fence, candidate)?;
        if selected.manifest().store_id() != self.store_id()
            || selected.manifest().session_id() != session_id
        {
            return Err(PhysicalHeadCasError::Candidate(
                "selected recovery envelope does not belong to the target store/session"
                    .to_string(),
            ));
        }
        let current = self
            .read_head_envelope(session_id)
            .map_err(|error| PhysicalHeadCasError::Candidate(error.to_string()))?;
        let Some(current) = current else {
            return Err(PhysicalHeadCasError::Conflict {
                expected: ExpectedRevision::Exact(selected.revision_id().clone()),
                observed: None,
            });
        };
        if current.revision_id() != selected.revision_id() {
            return Err(PhysicalHeadCasError::Conflict {
                expected: ExpectedRevision::Exact(selected.revision_id().clone()),
                observed: Some(current.revision_id().clone()),
            });
        }
        if current.envelope().manifest != selected.envelope().manifest
            || current.envelope().manifest_sha256 != selected.envelope().manifest_sha256
        {
            return Err(PhysicalHeadCasError::Candidate(
                "selected recovery HEAD envelope changed before publication".to_string(),
            ));
        }
        if candidate.manifest.parent_revision
            != ExpectedRevision::Exact(selected.revision_id().clone())
            || candidate.manifest.parent_manifest_sha256()
                != Some(selected.envelope().manifest_sha256())
        {
            return Err(PhysicalHeadCasError::Candidate(
                "recovery candidate is not bound to the exact selected HEAD envelope".to_string(),
            ));
        }
        self.publish_candidate_head(fence, candidate, true)
    }

    fn validate_head_candidate(
        &self,
        fence: &PhysicalWriterFenceV1,
        candidate: &InstalledRevisionCandidateV1,
    ) -> Result<(), PhysicalHeadCasError> {
        let session_id = &candidate.manifest.session_id;
        fence
            .confirm(self.store_id(), session_id)
            .map_err(PhysicalHeadCasError::Fence)?;
        if &candidate.manifest.store_id != self.store_id() {
            return Err(PhysicalHeadCasError::Candidate(format!(
                "revision candidate store mismatch: expected '{}', observed '{}'",
                self.store_id().as_str(),
                candidate.manifest.store_id.as_str()
            )));
        }
        let reloaded = self
            .open_revision_candidate(session_id, &candidate.manifest.revision_id)
            .map_err(|error| PhysicalHeadCasError::Candidate(error.to_string()))?;
        if reloaded.manifest != candidate.manifest
            || reloaded.manifest_sha256 != candidate.manifest_sha256
        {
            return Err(PhysicalHeadCasError::Candidate(
                "revision candidate receipt does not match the exact installed manifest"
                    .to_string(),
            ));
        }
        Ok(())
    }

    fn publish_candidate_head(
        &self,
        fence: &PhysicalWriterFenceV1,
        candidate: &InstalledRevisionCandidateV1,
        replace: bool,
    ) -> Result<VerifiedRevisionHeadV1, PhysicalHeadCasError> {
        let session_id = &candidate.manifest.session_id;
        fence
            .confirm(self.store_id(), session_id)
            .map_err(PhysicalHeadCasError::Fence)?;
        let head = HeadRecordV1::new(
            self.store_id().clone(),
            session_id.clone(),
            candidate.manifest.revision_id.clone(),
            candidate.manifest_sha256.clone(),
        );
        let bytes = encode_head_record(&head).map_err(PhysicalHeadCasError::Candidate)?;
        let head_name = format!("{}.json", encode_component(session_id.as_str()));

        #[cfg(all(test, unix))]
        if self.fail_next_head_sync.replace(false) {
            super::FAIL_NEXT_PRIVATE_DIRECTORY_SYNC.with(|failure| failure.set(true));
        }
        match atomic_write_private_in_detailed(
            &self.heads,
            OsStr::new(&head_name),
            &bytes,
            replace,
            "physical HEAD",
        ) {
            Ok(_) => {}
            Err(failure) => {
                return Err(PhysicalHeadCasError::Publication {
                    message: failure.message,
                    installed: failure.installed,
                });
            }
        }

        let installed =
            self.read_head(session_id)
                .map_err(|message| PhysicalHeadCasError::Publication {
                    message: format!(
                    "physical HEAD was installed, but exact reload verification failed: {message}"
                ),
                    installed: true,
                })?;
        let Some(installed) = installed else {
            return Err(PhysicalHeadCasError::Publication {
                message: "physical HEAD was installed, but exact reload observed it missing"
                    .to_string(),
                installed: true,
            });
        };
        if installed.revision_id() != &candidate.manifest.revision_id {
            return Err(PhysicalHeadCasError::Publication {
                message: "physical HEAD was installed, but exact reload selected another revision"
                    .to_string(),
                installed: true,
            });
        }
        Ok(installed)
    }

    #[cfg(test)]
    pub(super) fn inject_next_head_sync_failure(&self) {
        self.fail_next_head_sync.set(true);
    }

    pub(super) fn install_artifact(
        &self,
        artifact_schema_revision: u32,
        session_id: SessionId,
        artifact_id: String,
        media_type: Option<String>,
        content_bytes: &[u8],
    ) -> Result<InstalledArtifactV1, PhysicalArtifactInstallError> {
        let content_len = u64::try_from(content_bytes.len()).map_err(|_| {
            PhysicalArtifactInstallError::before_install(
                "artifact content length cannot be represented as u64",
            )
        })?;
        let content_sha256 =
            digest_bytes(content_bytes).map_err(PhysicalArtifactInstallError::before_install)?;
        let manifest = ArtifactManifestV1::new(
            artifact_schema_revision,
            self.store_id().clone(),
            session_id.clone(),
            artifact_id.clone(),
            content_len,
            content_sha256,
            media_type,
        )
        .map_err(PhysicalArtifactInstallError::before_install)?;
        let manifest_bytes = encode_artifact_manifest(&manifest)
            .map_err(PhysicalArtifactInstallError::before_install)?;
        let manifest_sha256 =
            digest_bytes(&manifest_bytes).map_err(PhysicalArtifactInstallError::before_install)?;

        let session_name = encode_component(session_id.as_str());
        let session_artifacts = self
            .artifacts
            .prepare_child(
                OsStr::new(&session_name),
                "physical session artifact namespace",
            )
            .map_err(PhysicalArtifactInstallError::before_install)?;
        self.artifacts
            .sync("physical artifact namespace")
            .map_err(PhysicalArtifactInstallError::before_install)?;

        let staging_name = format!("artifact-{}", fresh_id());
        let staged = self
            .staging
            .create_child_exclusive(OsStr::new(&staging_name), "physical artifact candidate")
            .map_err(PhysicalArtifactInstallError::before_install)?;
        let prepare = (|| -> Result<(), String> {
            write_new_synced(
                &staged,
                OsStr::new(ARTIFACT_CONTENT_NAME),
                content_bytes,
                "artifact content",
            )?;
            write_new_synced(
                &staged,
                OsStr::new(ARTIFACT_MANIFEST_NAME),
                &manifest_bytes,
                "artifact manifest",
            )?;
            staged.sync("physical artifact candidate")
        })();
        if let Err(error) = prepare {
            return Err(self.rollback_staged_artifact(&staging_name, &staged, error));
        }

        let artifact_name = encode_component(&artifact_id);
        if let Err(error) = rename_directory_no_replace(
            &self.staging,
            OsStr::new(&staging_name),
            &session_artifacts,
            OsStr::new(&artifact_name),
        ) {
            let message = if error.kind() == std::io::ErrorKind::AlreadyExists {
                format!(
                    "physical artifact '{}' already exists; refusing to overwrite it",
                    session_artifacts.path.join(&artifact_name).display()
                )
            } else {
                format!(
                    "cannot install physical artifact '{}': {error}",
                    session_artifacts.path.join(&artifact_name).display()
                )
            };
            return Err(self.rollback_staged_artifact(&staging_name, &staged, message));
        }

        #[cfg(test)]
        if self.fail_next_target_sync.replace(false) {
            return Err(PhysicalArtifactInstallError::after_install(
                "physical artifact was installed, but target durability is unconfirmed: injected failure",
            ));
        }
        if let Err(error) = session_artifacts.sync("physical session artifact namespace") {
            return Err(PhysicalArtifactInstallError::after_install(format!(
                "physical artifact was installed, but target durability is unconfirmed: {error}"
            )));
        }
        if let Err(error) = self.staging.sync("physical artifact staging namespace") {
            return Err(PhysicalArtifactInstallError::after_install(format!(
                "physical artifact was installed, but staging durability is unconfirmed: {error}"
            )));
        }

        Ok(InstalledArtifactV1 {
            manifest,
            manifest_bytes,
            manifest_sha256,
        })
    }

    pub(super) fn copy_artifact_no_overwrite(
        &self,
        source_session: &SessionId,
        destination_session: &SessionId,
        artifact_id: &str,
    ) -> Result<InstalledArtifactV1, PhysicalArtifactInstallError> {
        if source_session == destination_session {
            return Err(PhysicalArtifactInstallError::before_install(
                "artifact copy destination session must be fresh",
            ));
        }

        let source = self
            .open_artifact(source_session, artifact_id)
            .map_err(|error| {
                PhysicalArtifactInstallError::before_install(format!(
                    "cannot verify source artifact before copy: {error}"
                ))
            })?;
        let manifest = source.manifest();
        self.install_artifact(
            manifest.artifact_schema_revision(),
            destination_session.clone(),
            manifest.artifact_id().to_string(),
            manifest.media_type().map(str::to_string),
            source.content_bytes(),
        )
    }

    pub(super) fn confirm_existing_artifact(
        &self,
        fence: &PhysicalWriterFenceV1,
        session_id: &SessionId,
        artifact_id: &str,
        expected_manifest_sha256: &str,
    ) -> Result<VerifiedArtifactV1, String> {
        fence.confirm(self.store_id(), session_id)?;
        let verified = self
            .open_artifact(session_id, artifact_id)
            .map_err(|error| error.to_string())?;
        if verified.manifest_sha256().as_str() != expected_manifest_sha256 {
            return Err(format!(
                "physical artifact '{artifact_id}' changed before durability confirmation"
            ));
        }

        let session_name = encode_component(session_id.as_str());
        let session_artifacts = self.artifacts.open_child(
            OsStr::new(&session_name),
            "physical session artifact namespace",
        )?;
        session_artifacts.sync("physical session artifact namespace")?;
        self.artifacts.sync("physical artifact namespace")?;
        self.staging.sync("physical artifact staging namespace")?;

        let confirmed = self
            .open_artifact(session_id, artifact_id)
            .map_err(|error| error.to_string())?;
        if confirmed.manifest_sha256().as_str() != expected_manifest_sha256 {
            return Err(format!(
                "physical artifact '{artifact_id}' changed during durability confirmation"
            ));
        }
        Ok(confirmed)
    }

    pub(super) fn open_artifact(
        &self,
        session_id: &SessionId,
        artifact_id: &str,
    ) -> Result<VerifiedArtifactV1, PhysicalArtifactReadErrorV1> {
        validate_artifact_id(artifact_id).map_err(PhysicalArtifactReadErrorV1::Malformed)?;
        let session_name = encode_component(session_id.as_str());
        let artifact_name = encode_component(artifact_id);
        match self.artifacts.entry_kind(OsStr::new(&session_name)) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(PhysicalArtifactReadErrorV1::Missing(format!(
                    "physical session artifact namespace '{}' is missing",
                    self.artifacts.path.join(&session_name).display()
                )))
            }
            Ok(PrivateEntryKind::Directory) => {}
            Ok(_) => {
                return Err(PhysicalArtifactReadErrorV1::Malformed(format!(
                    "physical session artifact namespace '{}' is not a directory",
                    self.artifacts.path.join(&session_name).display()
                )))
            }
            Err(error) => {
                return Err(PhysicalArtifactReadErrorV1::Malformed(format!(
                    "cannot inspect physical session artifact namespace '{}': {error}",
                    self.artifacts.path.join(&session_name).display()
                )))
            }
        }
        let session_artifacts = self
            .artifacts
            .open_child(
                OsStr::new(&session_name),
                "physical session artifact namespace",
            )
            .map_err(PhysicalArtifactReadErrorV1::Malformed)?;
        match session_artifacts.entry_kind(OsStr::new(&artifact_name)) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(PhysicalArtifactReadErrorV1::Missing(format!(
                    "physical artifact '{}' is missing",
                    session_artifacts.path.join(&artifact_name).display()
                )))
            }
            Ok(PrivateEntryKind::Directory) => {}
            Ok(_) => {
                return Err(PhysicalArtifactReadErrorV1::Malformed(format!(
                    "physical artifact '{}' is not a directory",
                    session_artifacts.path.join(&artifact_name).display()
                )))
            }
            Err(error) => {
                return Err(PhysicalArtifactReadErrorV1::Malformed(format!(
                    "cannot inspect physical artifact '{}': {error}",
                    session_artifacts.path.join(&artifact_name).display()
                )))
            }
        }
        let artifact = session_artifacts
            .open_child(OsStr::new(&artifact_name), "physical artifact")
            .map_err(PhysicalArtifactReadErrorV1::Malformed)?;
        let require_regular = |name: &OsStr, context: &str| match artifact.entry_kind(name) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Err(PhysicalArtifactReadErrorV1::Missing(format!(
                    "{context} '{}' is missing",
                    artifact.path.join(name).display()
                )))
            }
            Ok(PrivateEntryKind::File) => Ok(()),
            Ok(_) => Err(PhysicalArtifactReadErrorV1::Malformed(format!(
                "cannot open {context} '{}': not a regular file",
                artifact.path.join(name).display()
            ))),
            Err(error) => Err(PhysicalArtifactReadErrorV1::Malformed(format!(
                "cannot inspect {context} '{}': {error}",
                artifact.path.join(name).display()
            ))),
        };
        require_regular(OsStr::new(ARTIFACT_MANIFEST_NAME), "artifact manifest")?;
        let manifest_bytes = read_bounded_regular(
            &artifact,
            OsStr::new(ARTIFACT_MANIFEST_NAME),
            MAX_PHYSICAL_RECORD_BYTES as u64,
            "artifact manifest",
        )
        .map_err(PhysicalArtifactReadErrorV1::Malformed)?;
        let manifest: ArtifactManifestV1 =
            serde_json::from_slice(&manifest_bytes).map_err(|error| {
                PhysicalArtifactReadErrorV1::Malformed(format!(
                    "cannot decode physical artifact manifest: {error}"
                ))
            })?;
        if manifest.record_type != RecordType::ArtifactManifest {
            return Err(PhysicalArtifactReadErrorV1::Malformed(
                "physical artifact manifest has the wrong record type".to_string(),
            ));
        }
        if manifest.physical_schema_revision == 0 {
            return Err(PhysicalArtifactReadErrorV1::Malformed(
                "physical artifact manifest schema must be nonzero".to_string(),
            ));
        }
        if manifest.physical_schema_revision != PHYSICAL_SCHEMA_REVISION {
            return Err(
                PhysicalArtifactReadErrorV1::UnsupportedPhysicalSchema {
                    observed: manifest.physical_schema_revision,
                    supported: PHYSICAL_SCHEMA_REVISION,
                    message: format!(
                        "unsupported physical artifact manifest schema {}; expected {PHYSICAL_SCHEMA_REVISION}",
                        manifest.physical_schema_revision
                    ),
                },
            );
        }
        let manifest = manifest
            .validate()
            .map_err(PhysicalArtifactReadErrorV1::Malformed)?;
        if manifest.store_id != *self.store_id()
            || manifest.session_id != *session_id
            || manifest.artifact_id != artifact_id
        {
            return Err(PhysicalArtifactReadErrorV1::IdentityMismatch(
                "artifact manifest identity does not match its physical location".to_string(),
            ));
        }
        require_regular(OsStr::new(ARTIFACT_CONTENT_NAME), "artifact content")?;
        let content_bytes = read_bounded_regular(
            &artifact,
            OsStr::new(ARTIFACT_CONTENT_NAME),
            MAX_IMMUTABLE_ARTIFACT_BYTES,
            "artifact content",
        )
        .map_err(PhysicalArtifactReadErrorV1::Malformed)?;
        if content_bytes.len() as u64 != manifest.content_bytes {
            return Err(PhysicalArtifactReadErrorV1::DigestMismatch(format!(
                "artifact content length mismatch: manifest declares {}, read {}",
                manifest.content_bytes,
                content_bytes.len()
            )));
        }
        let observed_digest =
            digest_bytes(&content_bytes).map_err(PhysicalArtifactReadErrorV1::Malformed)?;
        if observed_digest != manifest.content_sha256 {
            return Err(PhysicalArtifactReadErrorV1::DigestMismatch(format!(
                "artifact content digest mismatch: manifest declares {}, observed {}",
                manifest.content_sha256.as_str(),
                observed_digest.as_str()
            )));
        }
        let manifest_sha256 =
            digest_bytes(&manifest_bytes).map_err(PhysicalArtifactReadErrorV1::Malformed)?;
        Ok(VerifiedArtifactV1 {
            manifest,
            manifest_sha256,
            content_bytes,
        })
    }

    pub(super) fn install_revision_candidate(
        &self,
        logical_schema_revision: u32,
        session_id: SessionId,
        revision_id: RevisionId,
        parent_revision: ExpectedRevision,
        canonical_bytes: &[u8],
    ) -> Result<InstalledRevisionCandidateV1, PhysicalCandidateInstallError> {
        let parent_manifest_sha256 = match &parent_revision {
            ExpectedRevision::Missing => None,
            ExpectedRevision::Exact(parent) => Some(
                self.open_revision_candidate(&session_id, parent)
                    .map_err(|error| {
                        PhysicalCandidateInstallError::before_install(format!(
                            "cannot bind exact parent revision '{}': {error}",
                            parent.as_str()
                        ))
                    })?
                    .manifest_sha256
                    .clone(),
            ),
        };
        self.install_revision_candidate_bound(
            logical_schema_revision,
            session_id,
            revision_id,
            parent_revision,
            parent_manifest_sha256,
            canonical_bytes,
        )
    }

    pub(super) fn install_revision_candidate_against_envelope(
        &self,
        logical_schema_revision: u32,
        session_id: SessionId,
        revision_id: RevisionId,
        parent: &VerifiedRevisionEnvelopeV1,
        canonical_bytes: &[u8],
    ) -> Result<InstalledRevisionCandidateV1, PhysicalCandidateInstallError> {
        if parent.manifest.store_id() != self.store_id()
            || parent.manifest.session_id() != &session_id
        {
            return Err(PhysicalCandidateInstallError::before_install(
                "recovery parent envelope does not belong to the target store/session",
            ));
        }
        let reloaded = self
            .open_revision_envelope(&session_id, parent.revision_id())
            .map_err(|error| {
                PhysicalCandidateInstallError::before_install(format!(
                    "cannot reverify recovery parent envelope: {error}"
                ))
            })?;
        if reloaded.manifest != parent.manifest
            || reloaded.manifest_sha256 != parent.manifest_sha256
        {
            return Err(PhysicalCandidateInstallError::before_install(
                "recovery parent envelope changed before candidate installation",
            ));
        }
        self.install_revision_candidate_bound(
            logical_schema_revision,
            session_id,
            revision_id,
            ExpectedRevision::Exact(parent.revision_id().clone()),
            Some(parent.manifest_sha256.clone()),
            canonical_bytes,
        )
    }

    fn install_revision_candidate_bound(
        &self,
        logical_schema_revision: u32,
        session_id: SessionId,
        revision_id: RevisionId,
        parent_revision: ExpectedRevision,
        parent_manifest_sha256: Option<Sha256Digest>,
        canonical_bytes: &[u8],
    ) -> Result<InstalledRevisionCandidateV1, PhysicalCandidateInstallError> {
        let canonical_len = u64::try_from(canonical_bytes.len()).map_err(|_| {
            PhysicalCandidateInstallError::before_install(
                "canonical payload length cannot be represented as u64",
            )
        })?;
        let canonical_sha256 =
            digest_bytes(canonical_bytes).map_err(PhysicalCandidateInstallError::before_install)?;
        let manifest = RevisionManifestV1::new(
            logical_schema_revision,
            self.store_id().clone(),
            session_id.clone(),
            revision_id.clone(),
            parent_revision,
            parent_manifest_sha256,
            canonical_len,
            canonical_sha256,
        )
        .map_err(PhysicalCandidateInstallError::before_install)?;
        let manifest_bytes = encode_revision_manifest(&manifest)
            .map_err(PhysicalCandidateInstallError::before_install)?;
        let manifest_sha256 =
            digest_bytes(&manifest_bytes).map_err(PhysicalCandidateInstallError::before_install)?;

        let session_name = encode_component(session_id.as_str());
        let session_revisions = self
            .revisions
            .prepare_child(
                OsStr::new(&session_name),
                "physical session revision namespace",
            )
            .map_err(PhysicalCandidateInstallError::before_install)?;
        self.revisions
            .sync("physical revision namespace")
            .map_err(PhysicalCandidateInstallError::before_install)?;

        let staging_name = format!("candidate-{}", fresh_id());
        let staged = self
            .staging
            .create_child_exclusive(OsStr::new(&staging_name), "physical revision candidate")
            .map_err(PhysicalCandidateInstallError::before_install)?;

        let prepare = (|| -> Result<(), String> {
            write_new_synced(
                &staged,
                OsStr::new(CANONICAL_PAYLOAD_NAME),
                canonical_bytes,
                "canonical payload",
            )?;
            write_new_synced(
                &staged,
                OsStr::new(REVISION_MANIFEST_NAME),
                &manifest_bytes,
                "revision manifest",
            )?;
            staged.sync("physical revision candidate")
        })();
        if let Err(error) = prepare {
            return Err(self.rollback_staged(&staging_name, &staged, error));
        }

        let revision_name = encode_component(revision_id.as_str());
        if let Err(error) = rename_directory_no_replace(
            &self.staging,
            OsStr::new(&staging_name),
            &session_revisions,
            OsStr::new(&revision_name),
        ) {
            let message = if error.kind() == std::io::ErrorKind::AlreadyExists {
                format!(
                    "physical revision candidate '{}' already exists; refusing to overwrite it",
                    session_revisions.path.join(&revision_name).display()
                )
            } else {
                format!(
                    "cannot install physical revision candidate '{}': {error}",
                    session_revisions.path.join(&revision_name).display()
                )
            };
            return Err(self.rollback_staged(&staging_name, &staged, message));
        }

        #[cfg(test)]
        if self.fail_next_target_sync.replace(false) {
            return Err(PhysicalCandidateInstallError::after_install(
                "physical revision candidate was installed, but target durability is unconfirmed: injected failure",
            ));
        }
        if let Err(error) = session_revisions.sync("physical session revision namespace") {
            return Err(PhysicalCandidateInstallError::after_install(format!(
                "physical revision candidate was installed, but target durability is unconfirmed: {error}"
            )));
        }
        if let Err(error) = self.staging.sync("physical revision staging namespace") {
            return Err(PhysicalCandidateInstallError::after_install(format!(
                "physical revision candidate was installed, but staging durability is unconfirmed: {error}"
            )));
        }

        Ok(InstalledRevisionCandidateV1 {
            manifest,
            manifest_bytes,
            manifest_sha256,
        })
    }

    #[cfg(test)]
    pub(super) fn inject_next_target_sync_failure(&self) {
        self.fail_next_target_sync.set(true);
    }

    pub(super) fn revision_candidate_is_absent(
        &self,
        session_id: &SessionId,
        revision_id: &RevisionId,
    ) -> Result<bool, String> {
        let session_name = encode_component(session_id.as_str());
        let revision_name = encode_component(revision_id.as_str());
        match self.revisions.entry_kind(OsStr::new(&session_name)) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(true),
            Ok(PrivateEntryKind::Directory) => {
                let session_revisions = self.revisions.open_child(
                    OsStr::new(&session_name),
                    "physical session revision namespace",
                )?;
                match session_revisions.entry_kind(OsStr::new(&revision_name)) {
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(true),
                    Ok(_) => Ok(false),
                    Err(error) => Err(format!(
                        "cannot inspect physical revision candidate '{}': {error}",
                        session_revisions.path.join(&revision_name).display()
                    )),
                }
            }
            Ok(_) => Err(format!(
                "physical session revision namespace '{}' is not a directory",
                self.revisions.path.join(&session_name).display()
            )),
            Err(error) => Err(format!(
                "cannot inspect physical session revision namespace '{}': {error}",
                self.revisions.path.join(&session_name).display()
            )),
        }
    }

    pub(super) fn adopt_revision_candidate_exact(
        &self,
        fence: &PhysicalWriterFenceV1,
        expected: &VerifiedRevisionCandidateV1,
        canonical_bytes: &[u8],
    ) -> Result<InstalledRevisionCandidateV1, String> {
        let session_id = expected.manifest().session_id();
        let revision_id = expected.manifest().revision_id();
        fence.confirm(self.store_id(), session_id)?;
        let candidate = self
            .open_revision_candidate(session_id, revision_id)
            .map_err(|error| error.to_string())?;
        if candidate.manifest() != expected.manifest()
            || candidate.manifest_sha256() != expected.manifest_sha256()
            || candidate.canonical_bytes() != canonical_bytes
            || expected.canonical_bytes() != canonical_bytes
        {
            return Err(
                "physical revision candidate does not exactly match the requested adoption"
                    .to_string(),
            );
        }

        let session_name = encode_component(session_id.as_str());
        let session_revisions = self.revisions.open_child(
            OsStr::new(&session_name),
            "physical session revision namespace",
        )?;
        session_revisions.sync("physical session revision namespace")?;
        self.revisions.sync("physical revision namespace")?;
        self.staging.sync("physical revision staging namespace")?;

        let confirmed = self
            .open_revision_candidate(session_id, revision_id)
            .map_err(|error| error.to_string())?;
        if confirmed.manifest() != candidate.manifest()
            || confirmed.manifest_sha256() != candidate.manifest_sha256()
            || confirmed.canonical_bytes() != canonical_bytes
        {
            return Err(
                "physical revision candidate changed during durability confirmation".to_string(),
            );
        }
        let manifest = confirmed.manifest().clone();
        let manifest_bytes = encode_revision_manifest(&manifest)?;
        let manifest_sha256 = digest_bytes(&manifest_bytes)?;
        if &manifest_sha256 != expected.manifest_sha256() {
            return Err(
                "physical revision candidate manifest digest changed during adoption".to_string(),
            );
        }
        Ok(InstalledRevisionCandidateV1 {
            manifest,
            manifest_bytes,
            manifest_sha256,
        })
    }

    pub(super) fn open_revision_candidate(
        &self,
        session_id: &SessionId,
        revision_id: &RevisionId,
    ) -> Result<VerifiedRevisionCandidateV1, PhysicalReadErrorV1> {
        let envelope = self.open_revision_envelope(session_id, revision_id)?;
        self.verify_revision_candidate(&envelope)
    }

    pub(super) fn open_revision_envelope(
        &self,
        session_id: &SessionId,
        revision_id: &RevisionId,
    ) -> Result<VerifiedRevisionEnvelopeV1, PhysicalReadErrorV1> {
        let session_name = encode_component(session_id.as_str());
        let revision_name = encode_component(revision_id.as_str());
        match self.revisions.entry_kind(OsStr::new(&session_name)) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(PhysicalReadErrorV1::MissingReference(format!(
                    "physical session revision namespace '{}' is missing",
                    self.revisions.path.join(&session_name).display()
                )))
            }
            Ok(PrivateEntryKind::Directory) => {}
            Ok(_) => {
                return Err(PhysicalReadErrorV1::Malformed(format!(
                    "physical session revision namespace '{}' is not a directory",
                    self.revisions.path.join(&session_name).display()
                )))
            }
            Err(error) => {
                return Err(PhysicalReadErrorV1::Malformed(format!(
                    "cannot inspect physical session revision namespace '{}': {error}",
                    self.revisions.path.join(&session_name).display()
                )))
            }
        }
        let session_revisions = self
            .revisions
            .open_child(
                OsStr::new(&session_name),
                "physical session revision namespace",
            )
            .map_err(PhysicalReadErrorV1::Malformed)?;
        match session_revisions.entry_kind(OsStr::new(&revision_name)) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(PhysicalReadErrorV1::MissingReference(format!(
                    "physical revision candidate '{}' is missing",
                    session_revisions.path.join(&revision_name).display()
                )))
            }
            Ok(PrivateEntryKind::Directory) => {}
            Ok(_) => {
                return Err(PhysicalReadErrorV1::Malformed(format!(
                    "physical revision candidate '{}' is not a directory",
                    session_revisions.path.join(&revision_name).display()
                )))
            }
            Err(error) => {
                return Err(PhysicalReadErrorV1::Malformed(format!(
                    "cannot inspect physical revision candidate '{}': {error}",
                    session_revisions.path.join(&revision_name).display()
                )))
            }
        }
        let revision = session_revisions
            .open_child(OsStr::new(&revision_name), "physical revision candidate")
            .map_err(PhysicalReadErrorV1::Malformed)?;
        let require_regular = |name: &OsStr, context: &str| match revision.entry_kind(name) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Err(PhysicalReadErrorV1::MissingReference(format!(
                    "{context} '{}' is missing",
                    revision.path.join(name).display()
                )))
            }
            Ok(PrivateEntryKind::File) => Ok(()),
            Ok(_) => Err(PhysicalReadErrorV1::Malformed(format!(
                "cannot open {context} '{}': not a regular file",
                revision.path.join(name).display()
            ))),
            Err(error) => Err(PhysicalReadErrorV1::Malformed(format!(
                "cannot inspect {context} '{}': {error}",
                revision.path.join(name).display()
            ))),
        };
        require_regular(OsStr::new(REVISION_MANIFEST_NAME), "revision manifest")?;
        let manifest_bytes = read_bounded_regular(
            &revision,
            OsStr::new(REVISION_MANIFEST_NAME),
            MAX_PHYSICAL_RECORD_BYTES as u64,
            "revision manifest",
        )
        .map_err(PhysicalReadErrorV1::Malformed)?;
        let manifest = decode_revision_manifest_classified(
            &manifest_bytes,
            self.store_id(),
            session_id,
            revision_id,
        )?;
        let manifest_sha256 =
            digest_bytes(&manifest_bytes).map_err(PhysicalReadErrorV1::Malformed)?;
        Ok(VerifiedRevisionEnvelopeV1 {
            directory: revision,
            manifest,
            manifest_sha256,
        })
    }

    pub(super) fn verify_revision_candidate(
        &self,
        envelope: &VerifiedRevisionEnvelopeV1,
    ) -> Result<VerifiedRevisionCandidateV1, PhysicalReadErrorV1> {
        if envelope.manifest.store_id() != self.store_id() {
            return Err(PhysicalReadErrorV1::IdentityMismatch(
                "revision envelope belongs to a different physical store".to_string(),
            ));
        }
        let revision = &envelope.directory;
        match revision.entry_kind(OsStr::new(CANONICAL_PAYLOAD_NAME)) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(PhysicalReadErrorV1::MissingReference(format!(
                    "canonical payload '{}' is missing",
                    revision.path.join(CANONICAL_PAYLOAD_NAME).display()
                )));
            }
            Ok(PrivateEntryKind::File) => {}
            Ok(_) => {
                return Err(PhysicalReadErrorV1::Malformed(format!(
                    "cannot open canonical payload '{}': not a regular file",
                    revision.path.join(CANONICAL_PAYLOAD_NAME).display()
                )));
            }
            Err(error) => {
                return Err(PhysicalReadErrorV1::Malformed(format!(
                    "cannot inspect canonical payload '{}': {error}",
                    revision.path.join(CANONICAL_PAYLOAD_NAME).display()
                )));
            }
        }
        let canonical_bytes = read_bounded_regular(
            revision,
            OsStr::new(CANONICAL_PAYLOAD_NAME),
            MAX_CANONICAL_PAYLOAD_BYTES,
            "canonical payload",
        )
        .map_err(PhysicalReadErrorV1::Malformed)?;
        if canonical_bytes.len() as u64 != envelope.manifest.canonical_bytes {
            return Err(PhysicalReadErrorV1::DigestMismatch(format!(
                "canonical payload length mismatch: manifest declares {}, read {}",
                envelope.manifest.canonical_bytes,
                canonical_bytes.len()
            )));
        }
        let observed_digest =
            digest_bytes(&canonical_bytes).map_err(PhysicalReadErrorV1::Malformed)?;
        if observed_digest != envelope.manifest.canonical_sha256 {
            return Err(PhysicalReadErrorV1::DigestMismatch(format!(
                "canonical payload digest mismatch: manifest declares {}, observed {}",
                envelope.manifest.canonical_sha256.as_str(),
                observed_digest.as_str()
            )));
        }
        Ok(VerifiedRevisionCandidateV1 {
            manifest: envelope.manifest.clone(),
            manifest_sha256: envelope.manifest_sha256.clone(),
            canonical_bytes,
        })
    }

    fn rollback_staged(
        &self,
        staging_name: &str,
        staged: &PrivateDirectory,
        primary: impl Into<String>,
    ) -> PhysicalCandidateInstallError {
        let primary = primary.into();
        match remove_artifact_tree(
            &self.staging,
            OsStr::new(staging_name),
            staged,
            "physical revision candidate rollback",
        ) {
            Ok(()) => PhysicalCandidateInstallError::before_install(primary),
            Err(rollback) => PhysicalCandidateInstallError::before_install(format!(
                "{primary}; staged candidate cleanup also failed: {rollback}"
            )),
        }
    }

    fn rollback_staged_artifact(
        &self,
        staging_name: &str,
        staged: &PrivateDirectory,
        primary: impl Into<String>,
    ) -> PhysicalArtifactInstallError {
        let primary = primary.into();
        match remove_artifact_tree(
            &self.staging,
            OsStr::new(staging_name),
            staged,
            "physical artifact candidate rollback",
        ) {
            Ok(()) => PhysicalArtifactInstallError::before_install(primary),
            Err(rollback) => PhysicalArtifactInstallError::before_install(format!(
                "{primary}; staged artifact cleanup also failed: {rollback}"
            )),
        }
    }
}

fn open_or_create_store_record(
    root: &PrivateDirectory,
    expected_store: StoreId,
) -> Result<StoreRecordV1, String> {
    match root.entry_kind(OsStr::new(STORE_RECORD_NAME)) {
        Ok(PrivateEntryKind::File) => return read_and_validate_store_record(root, &expected_store),
        Ok(_) => {
            return Err(format!(
                "physical store record '{}' is not a regular file",
                root.path.join(STORE_RECORD_NAME).display()
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(format!(
                "cannot inspect physical store record '{}': {error}",
                root.path.join(STORE_RECORD_NAME).display()
            ));
        }
    }

    let bytes = encode_store_record(&StoreRecordV1::new(expected_store.clone()))?;
    match atomic_write_private_in_detailed(
        root,
        OsStr::new(STORE_RECORD_NAME),
        &bytes,
        false,
        "physical store record",
    ) {
        Ok(_) => read_and_validate_store_record(root, &expected_store),
        Err(failure)
            if !failure.installed
                && matches!(
                    root.entry_kind(OsStr::new(STORE_RECORD_NAME)),
                    Ok(PrivateEntryKind::File)
                ) =>
        {
            read_and_validate_store_record(root, &expected_store)
        }
        Err(failure) => Err(failure.message),
    }
}

fn read_and_validate_store_record(
    root: &PrivateDirectory,
    expected_store: &StoreId,
) -> Result<StoreRecordV1, String> {
    let bytes = read_bounded_regular(
        root,
        OsStr::new(STORE_RECORD_NAME),
        MAX_PHYSICAL_RECORD_BYTES as u64,
        "physical store record",
    )?;
    let record = decode_store_record(&bytes)?;
    if record.store_id() != expected_store {
        return Err(format!(
            "physical store identity mismatch: expected '{}', observed '{}'",
            expected_store.as_str(),
            record.store_id().as_str()
        ));
    }
    Ok(record)
}

fn write_new_synced(
    directory: &PrivateDirectory,
    name: &OsStr,
    bytes: &[u8],
    context: &str,
) -> Result<(), String> {
    let path = directory.path.join(name);
    let mut file = directory.create_regular_file(name, context)?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|error| format!("cannot write {context} file '{}': {error}", path.display()))
}

fn read_bounded_regular(
    directory: &PrivateDirectory,
    name: &OsStr,
    max_bytes: u64,
    context: &str,
) -> Result<Vec<u8>, String> {
    let path = directory.path.join(name);
    let file = directory.open_regular_file(name, context)?;
    let metadata = file.metadata().map_err(|error| {
        format!(
            "cannot inspect opened {context} file '{}': {error}",
            path.display()
        )
    })?;
    if metadata.len() > max_bytes {
        return Err(format!(
            "{context} file '{}' is {} bytes; limit is {max_bytes}",
            path.display(),
            metadata.len()
        ));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| format!("cannot read {context} file '{}': {error}", path.display()))?;
    if bytes.len() as u64 > max_bytes {
        return Err(format!(
            "{context} file '{}' grew beyond the {max_bytes} byte limit while it was read",
            path.display()
        ));
    }
    Ok(bytes)
}

fn digest_bytes(bytes: &[u8]) -> Result<Sha256Digest, String> {
    Sha256Digest::new(crate::skills::content_sha256_hex(bytes))
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn rename_directory_no_replace(
    source_parent: &PrivateDirectory,
    source_name: &OsStr,
    target_parent: &PrivateDirectory,
    target_name: &OsStr,
) -> std::io::Result<()> {
    let source_name = super::c_name(source_name)?;
    let target_name = super::c_name(target_name)?;
    let result = unsafe {
        libc::renameat2(
            source_parent.file.as_raw_fd(),
            source_name.as_ptr(),
            target_parent.file.as_raw_fd(),
            target_name.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(target_vendor = "apple")]
fn rename_directory_no_replace(
    source_parent: &PrivateDirectory,
    source_name: &OsStr,
    target_parent: &PrivateDirectory,
    target_name: &OsStr,
) -> std::io::Result<()> {
    let source_name = super::c_name(source_name)?;
    let target_name = super::c_name(target_name)?;
    let result = unsafe {
        libc::renameatx_np(
            source_parent.file.as_raw_fd(),
            source_name.as_ptr(),
            target_parent.file.as_raw_fd(),
            target_name.as_ptr(),
            libc::RENAME_EXCL,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(all(
    unix,
    not(any(target_os = "linux", target_os = "android")),
    not(target_vendor = "apple")
))]
fn rename_directory_no_replace(
    _source_parent: &PrivateDirectory,
    _source_name: &OsStr,
    _target_parent: &PrivateDirectory,
    _target_name: &OsStr,
) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "atomic no-replace directory installation is unavailable on this platform",
    ))
}

#[cfg(not(unix))]
fn rename_directory_no_replace(
    _source_parent: &PrivateDirectory,
    _source_name: &OsStr,
    _target_parent: &PrivateDirectory,
    _target_name: &OsStr,
) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "atomic no-replace directory installation is unavailable on this platform",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn store() -> StoreId {
        StoreId::new("store-a").unwrap()
    }

    fn session() -> SessionId {
        SessionId::new("s-alpha").unwrap()
    }

    fn revision(value: &str) -> RevisionId {
        RevisionId::new(value).unwrap()
    }

    fn digest(byte: &str) -> Sha256Digest {
        Sha256Digest::new(byte.repeat(64)).unwrap()
    }

    #[test]
    fn layout_is_namespaced_reversible_and_declares_no_clobber_boundaries() {
        let root = Path::new("/state/sessions");
        let session = session();
        let revision = revision("revision-a");
        assert_eq!(encode_component(session.as_str()), "id-omwwc3dqnbqq");
        assert_eq!(
            decode_component("id-omwwc3dqnbqq").unwrap(),
            session.as_str()
        );
        assert_eq!(encode_component("."), "id-fy");
        assert_eq!(encode_component(".."), "id-fyxa");
        assert!(decode_component("id-FY").is_err());
        assert!(decode_component("id-fz").is_err());
        assert_ne!(encode_component("A"), encode_component("a"));
        assert_eq!(
            PhysicalLayoutV1::store_record(root),
            Path::new("/state/sessions/.kitsu-store-v1/store.json")
        );
        assert_eq!(
            PhysicalLayoutV1::revision_manifest(root, &session, &revision),
            Path::new("/state/sessions/.kitsu-store-v1/revisions/id-omwwc3dqnbqq/id-ojsxm2ltnfxw4llb/manifest.json")
        );
        assert_eq!(
            PhysicalLayoutV1::canonical_payload(root, &session, &revision),
            Path::new("/state/sessions/.kitsu-store-v1/revisions/id-omwwc3dqnbqq/id-ojsxm2ltnfxw4llb/canonical.json")
        );
        assert_eq!(
            PhysicalLayoutV1::head_pointer(root, &session),
            Path::new("/state/sessions/.kitsu-store-v1/heads/id-omwwc3dqnbqq.json")
        );
        assert_eq!(
            PhysicalLayoutV1::artifact_manifest(root, &session, "artifact-a"),
            Path::new("/state/sessions/.kitsu-store-v1/artifacts/id-omwwc3dqnbqq/id-mfzhi2lgmfrxillb/manifest.json")
        );
        assert_eq!(
            PhysicalLayoutV1::artifact_content(root, &session, "artifact-a"),
            Path::new("/state/sessions/.kitsu-store-v1/artifacts/id-omwwc3dqnbqq/id-mfzhi2lgmfrxillb/content.bin")
        );
        assert_eq!(
            PhysicalLayoutV1::staging_root(root),
            Path::new("/state/sessions/.kitsu-store-v1/staging")
        );
        assert_eq!(
            PhysicalLayoutV1::locks_root(root),
            Path::new("/state/sessions/.kitsu-store-v1/.locks")
        );
        assert_eq!(
            PhysicalLayoutV1::store_install_mode(),
            PhysicalInstallMode::CreateNew
        );
        assert_eq!(
            PhysicalLayoutV1::revision_install_mode(),
            PhysicalInstallMode::CreateNew
        );
        assert_eq!(
            PhysicalLayoutV1::artifact_install_mode(),
            PhysicalInstallMode::CreateNew
        );
        assert_eq!(
            PhysicalLayoutV1::head_install_mode(),
            PhysicalInstallMode::ReplaceAfterFenceAndRevisionRecheck
        );
    }

    #[cfg(unix)]
    #[test]
    fn pinned_store_initialization_cannot_be_redirected_by_ancestor_replacement() {
        let root = tempfile::tempdir().unwrap();
        let visible = root.path().join("sessions");
        let original = root.path().join("sessions-original");
        std::fs::create_dir(&visible).unwrap();
        let pinned = PrivateDirectory::open(&visible, "pinned store fixture").unwrap();
        std::fs::rename(&visible, &original).unwrap();
        std::fs::create_dir(&visible).unwrap();

        drop(PhysicalFileStoreV1::create_or_open_pinned(&pinned, store()).unwrap());

        assert!(original.join(ROOT_COMPONENT).is_dir());
        assert!(!visible.join(ROOT_COMPONENT).exists());
    }

    #[test]
    fn physical_records_have_fixed_closed_vectors_and_no_logical_head_projection() {
        let store_record = StoreRecordV1::new(store());
        assert_eq!(
            encode_store_record(&store_record).unwrap(),
            br#"{"record_type":"store","physical_schema_revision":2,"store_id":"store-a"}
"#
        );
        assert_eq!(
            serde_json::to_value(&store_record).unwrap(),
            serde_json::json!({
                "record_type": "store",
                "physical_schema_revision": 2,
                "store_id": "store-a"
            })
        );
        assert_eq!(
            decode_store_record(&encode_store_record(&store_record).unwrap()).unwrap(),
            store_record
        );

        let manifest = RevisionManifestV1::new(
            1,
            store(),
            session(),
            revision("revision-b"),
            ExpectedRevision::Exact(revision("revision-a")),
            Some(digest("c")),
            123,
            digest("a"),
        )
        .unwrap();
        let manifest_value = serde_json::to_value(&manifest).unwrap();
        assert_eq!(manifest_value["record_type"], "revision_manifest");
        assert_eq!(manifest_value["canonical_bytes"], 123);
        assert_eq!(manifest_value["parent_manifest_sha256"], "c".repeat(64));
        assert!(manifest_value.get("logical_head").is_none());
        assert_eq!(
            decode_revision_manifest(
                &encode_revision_manifest(&manifest).unwrap(),
                &store(),
                &session(),
                &revision("revision-b"),
            )
            .unwrap(),
            manifest
        );

        let head = HeadRecordV1::new(store(), session(), revision("revision-b"), digest("b"));
        let head_value = serde_json::to_value(&head).unwrap();
        assert_eq!(head_value["record_type"], "head");
        assert!(head_value.get("logical_head").is_none());
        assert_eq!(
            decode_head_record(&encode_head_record(&head).unwrap(), &store(), &session()).unwrap(),
            head
        );

        let artifact = ArtifactManifestV1::new(
            1,
            store(),
            session(),
            "artifact-a".to_string(),
            7,
            digest("c"),
            Some("text/plain".to_string()),
        )
        .unwrap();
        let artifact_value = serde_json::to_value(&artifact).unwrap();
        assert_eq!(artifact_value["record_type"], "artifact_manifest");
        assert_eq!(artifact_value["artifact_id"], "artifact-a");
        assert_eq!(artifact_value["content_bytes"], 7);
        assert_eq!(
            decode_artifact_manifest(
                &encode_artifact_manifest(&artifact).unwrap(),
                &store(),
                &session(),
                "artifact-a",
            )
            .unwrap(),
            artifact
        );
        let mut artifact_future = artifact_value.clone();
        artifact_future["physical_schema_revision"] = serde_json::json!(3);
        assert!(decode_artifact_manifest(
            &serde_json::to_vec(&artifact_future).unwrap(),
            &store(),
            &session(),
            "artifact-a",
        )
        .is_err());
        assert!(decode_artifact_manifest(
            &encode_artifact_manifest(&artifact).unwrap(),
            &store(),
            &session(),
            "artifact-other",
        )
        .is_err());
        assert!(ArtifactManifestV1::new(
            1,
            store(),
            session(),
            "../escape".to_string(),
            1,
            digest("d"),
            None,
        )
        .is_err());
    }

    #[test]
    fn future_unknown_mismatched_and_malformed_records_fail_closed() {
        let manifest = RevisionManifestV1::new(
            1,
            store(),
            session(),
            revision("revision-b"),
            ExpectedRevision::Exact(revision("revision-a")),
            Some(digest("c")),
            123,
            digest("a"),
        )
        .unwrap();
        let mut value = serde_json::to_value(&manifest).unwrap();
        value["physical_schema_revision"] = serde_json::json!(3);
        assert!(decode_revision_manifest(
            &serde_json::to_vec(&value).unwrap(),
            &store(),
            &session(),
            &revision("revision-b")
        )
        .is_err());
        value["physical_schema_revision"] = serde_json::json!(2);
        value["ambient_head"] = serde_json::json!("tail");
        assert!(decode_revision_manifest(
            &serde_json::to_vec(&value).unwrap(),
            &store(),
            &session(),
            &revision("revision-b")
        )
        .is_err());
        assert!(decode_revision_manifest(
            &encode_revision_manifest(&manifest).unwrap(),
            &store(),
            &session(),
            &revision("revision-other")
        )
        .is_err());
        assert!(Sha256Digest::new("A".repeat(64)).is_err());
        assert!(RevisionManifestV1::new(
            0,
            store(),
            session(),
            revision("revision-b"),
            ExpectedRevision::Missing,
            None,
            123,
            digest("a"),
        )
        .is_err());
        assert!(RevisionManifestV1::new(
            1,
            store(),
            session(),
            revision("revision-b"),
            ExpectedRevision::Exact(revision("revision-b")),
            Some(digest("b")),
            123,
            digest("a"),
        )
        .is_err());
        assert!(RevisionManifestV1::new(
            1,
            store(),
            session(),
            revision("revision-b"),
            ExpectedRevision::Missing,
            Some(digest("b")),
            123,
            digest("a"),
        )
        .is_err());
        assert!(RevisionManifestV1::new(
            1,
            store(),
            session(),
            revision("revision-b"),
            ExpectedRevision::Exact(revision("revision-a")),
            None,
            123,
            digest("a"),
        )
        .is_err());
        assert!(decode_store_record(&vec![b' '; MAX_PHYSICAL_RECORD_BYTES + 1]).is_err());
    }

    #[test]
    fn maximal_identity_component_stays_below_portable_filename_limit() {
        let identity = "x".repeat(SessionId::MAX_BYTES);
        let encoded = encode_component(&identity);
        assert_eq!(decode_component(&encoded).unwrap(), identity);
        assert!(encoded.len() <= 255);

        let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789._:-";
        for length in 1..=SessionId::MAX_BYTES {
            let value: String = (0..length)
                .map(|index| alphabet[index % alphabet.len()] as char)
                .collect();
            let encoded = encode_component(&value);
            assert_eq!(decode_component(&encoded).unwrap(), value);
            assert!(encoded.len() <= 255);
        }
    }

    #[test]
    fn store_create_open_is_no_clobber_and_rejects_a_different_identity() {
        let temp = TempDir::new().unwrap();
        let session_root = temp.path().join("sessions");
        let expected = store();

        let created = PhysicalFileStoreV1::create_or_open(&session_root, expected.clone()).unwrap();
        assert_eq!(created.store_id(), &expected);
        let original = std::fs::read(PhysicalLayoutV1::store_record(&session_root)).unwrap();

        let reopened =
            PhysicalFileStoreV1::create_or_open(&session_root, expected.clone()).unwrap();
        assert_eq!(reopened.store_id(), &expected);
        assert_eq!(
            std::fs::read(PhysicalLayoutV1::store_record(&session_root)).unwrap(),
            original
        );

        let other = StoreId::new("store-other").unwrap();
        let error = PhysicalFileStoreV1::create_or_open(&session_root, other).unwrap_err();
        assert!(error.contains("store identity"), "{error}");
        assert_eq!(
            std::fs::read(PhysicalLayoutV1::store_record(&session_root)).unwrap(),
            original
        );
    }

    #[test]
    fn store_open_rejects_corruption_without_rewriting_it() {
        let temp = TempDir::new().unwrap();
        let session_root = temp.path().join("sessions");
        drop(PhysicalFileStoreV1::create_or_open(&session_root, store()).unwrap());
        let path = PhysicalLayoutV1::store_record(&session_root);
        let corrupt =
            b"{\"record_type\":\"store\",\"physical_schema_revision\":99,\"store_id\":\"store-a\"}\n";
        std::fs::write(&path, corrupt).unwrap();

        let error = PhysicalFileStoreV1::create_or_open(&session_root, store()).unwrap_err();
        assert!(error.contains("unsupported physical schema"), "{error}");
        assert_eq!(std::fs::read(path).unwrap(), corrupt);
    }

    #[test]
    fn revision_candidate_round_trips_exact_bytes_and_digests() {
        let temp = TempDir::new().unwrap();
        let session_root = temp.path().join("sessions");
        let store = PhysicalFileStoreV1::create_or_open(&session_root, store()).unwrap();
        let session = session();
        let revision = revision("revision-a");
        let canonical = b"{\"messages\":[{\"role\":\"user\",\"text\":\"hello\"}]}\n";

        let installed = store
            .install_revision_candidate(
                1,
                session.clone(),
                revision.clone(),
                ExpectedRevision::Missing,
                canonical,
            )
            .unwrap();
        assert_eq!(installed.manifest().session_id, session);
        assert_eq!(installed.manifest().revision_id, revision);
        assert!(installed.manifest().parent_manifest_sha256().is_none());
        assert_eq!(
            installed.manifest().canonical_sha256.as_str(),
            crate::skills::content_sha256_hex(canonical)
        );
        assert_eq!(
            installed.manifest_sha256().as_str(),
            crate::skills::content_sha256_hex(&installed.manifest_bytes)
        );

        let opened = store.open_revision_candidate(&session, &revision).unwrap();
        assert_eq!(opened.manifest(), installed.manifest());
        assert_eq!(opened.manifest_sha256(), installed.manifest_sha256());
        assert_eq!(opened.canonical_bytes(), canonical);
        assert!(
            std::fs::read_dir(PhysicalLayoutV1::staging_root(&session_root))
                .unwrap()
                .next()
                .is_none()
        );
    }

    #[test]
    fn artifact_install_read_collision_and_missing_are_exact() {
        let temp = TempDir::new().unwrap();
        let session_root = temp.path().join("sessions");
        let store = PhysicalFileStoreV1::create_or_open(&session_root, store()).unwrap();
        let session = session();
        let first = b"immutable artifact bytes";

        let installed = store
            .install_artifact(
                1,
                session.clone(),
                "artifact-a".to_string(),
                Some("text/plain".to_string()),
                first,
            )
            .unwrap();
        assert_eq!(installed.manifest().artifact_id, "artifact-a");
        assert_eq!(
            installed.manifest().content_sha256.as_str(),
            crate::skills::content_sha256_hex(first)
        );
        let opened = store.open_artifact(&session, "artifact-a").unwrap();
        assert_eq!(opened.manifest(), installed.manifest());
        assert_eq!(opened.manifest_sha256(), installed.manifest_sha256());
        assert_eq!(
            installed.manifest_sha256().as_str(),
            crate::skills::content_sha256_hex(&installed.manifest_bytes)
        );
        assert_eq!(opened.content_bytes(), first);

        let collision = store
            .install_artifact(
                1,
                session.clone(),
                "artifact-a".to_string(),
                Some("text/plain".to_string()),
                b"different bytes",
            )
            .unwrap_err();
        assert!(!collision.installed());
        assert!(collision.to_string().contains("refusing to overwrite"));
        assert_eq!(
            store
                .open_artifact(&session, "artifact-a")
                .unwrap()
                .content_bytes(),
            first
        );
        assert!(store.open_artifact(&session, "missing").is_err());
        assert!(
            std::fs::read_dir(PhysicalLayoutV1::staging_root(&session_root))
                .unwrap()
                .next()
                .is_none()
        );
    }

    #[test]
    fn artifact_copy_is_verified_destination_scoped_and_no_overwrite() {
        let temp = TempDir::new().unwrap();
        let session_root = temp.path().join("sessions");
        let store = PhysicalFileStoreV1::create_or_open(&session_root, store()).unwrap();
        let source = SessionId::new("s-source").unwrap();
        let destination = SessionId::new("s-destination").unwrap();
        let content = b"branch artifact bytes";
        store
            .install_artifact(
                1,
                source.clone(),
                "artifact-a".to_string(),
                Some("text/plain".to_string()),
                content,
            )
            .unwrap();

        let copied = store
            .copy_artifact_no_overwrite(&source, &destination, "artifact-a")
            .unwrap();

        assert_eq!(&copied.manifest().session_id, &destination);
        assert_eq!(copied.manifest().artifact_id(), "artifact-a");
        let opened = store.open_artifact(&destination, "artifact-a").unwrap();
        assert_eq!(opened.content_bytes(), content);
        assert_eq!(
            opened.manifest().content_sha256(),
            copied.manifest().content_sha256()
        );
        assert_eq!(opened.manifest().media_type(), Some("text/plain"));
        let collision = store
            .copy_artifact_no_overwrite(&source, &destination, "artifact-a")
            .unwrap_err();
        assert!(!collision.installed());
        assert!(collision.to_string().contains("refusing to overwrite"));
        assert_eq!(
            store
                .open_artifact(&destination, "artifact-a")
                .unwrap()
                .content_bytes(),
            content
        );
        assert!(store
            .copy_artifact_no_overwrite(&source, &source, "artifact-a")
            .is_err());

        assert!(store
            .copy_artifact_no_overwrite(&source, &destination, "missing")
            .is_err());
        assert!(store.open_artifact(&destination, "missing").is_err());

        store
            .install_artifact(
                1,
                source.clone(),
                "artifact-b".to_string(),
                None,
                b"original",
            )
            .unwrap();
        std::fs::write(
            PhysicalLayoutV1::artifact_content(&session_root, &source, "artifact-b"),
            b"tampered",
        )
        .unwrap();
        assert!(store
            .copy_artifact_no_overwrite(&source, &destination, "artifact-b")
            .is_err());
        assert!(store.open_artifact(&destination, "artifact-b").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn artifact_read_refuses_digest_retarget_and_symlink_tampering() {
        use std::os::unix::fs::symlink;

        let temp = TempDir::new().unwrap();
        let session_root = temp.path().join("sessions");
        let store = PhysicalFileStoreV1::create_or_open(&session_root, store()).unwrap();
        let session = session();
        for artifact in ["artifact-a", "artifact-b", "artifact-c"] {
            store
                .install_artifact(
                    1,
                    session.clone(),
                    artifact.to_string(),
                    None,
                    artifact.as_bytes(),
                )
                .unwrap();
        }

        std::fs::write(
            PhysicalLayoutV1::artifact_content(&session_root, &session, "artifact-a"),
            b"tampered-a",
        )
        .unwrap();
        let digest_error = store.open_artifact(&session, "artifact-a").unwrap_err();
        assert!(digest_error.contains("digest mismatch"), "{digest_error}");

        let manifest_b = std::fs::read(PhysicalLayoutV1::artifact_manifest(
            &session_root,
            &session,
            "artifact-b",
        ))
        .unwrap();
        std::fs::write(
            PhysicalLayoutV1::artifact_manifest(&session_root, &session, "artifact-a"),
            manifest_b,
        )
        .unwrap();
        let identity_error = store.open_artifact(&session, "artifact-a").unwrap_err();
        assert!(
            identity_error.contains("identity does not match"),
            "{identity_error}"
        );

        let content_c = PhysicalLayoutV1::artifact_content(&session_root, &session, "artifact-c");
        std::fs::remove_file(&content_c).unwrap();
        symlink("/etc/passwd", &content_c).unwrap();
        let symlink_error = store.open_artifact(&session, "artifact-c").unwrap_err();
        assert!(
            symlink_error.contains("cannot open artifact content"),
            "{symlink_error}"
        );
    }

    #[test]
    fn artifact_post_install_sync_failure_keeps_verifiable_unconfirmed_bytes() {
        let temp = TempDir::new().unwrap();
        let session_root = temp.path().join("sessions");
        let store = PhysicalFileStoreV1::create_or_open(&session_root, store()).unwrap();
        let session = session();
        let content = b"installed artifact";
        store.inject_next_target_sync_failure();

        let error = store
            .install_artifact(1, session.clone(), "artifact-a".to_string(), None, content)
            .unwrap_err();
        assert!(error.installed());
        assert!(error.to_string().contains("durability is unconfirmed"));
        assert_eq!(
            store
                .open_artifact(&session, "artifact-a")
                .unwrap()
                .content_bytes(),
            content
        );
    }

    #[test]
    fn revision_collision_preserves_the_first_candidate_exactly() {
        let temp = TempDir::new().unwrap();
        let session_root = temp.path().join("sessions");
        let store = PhysicalFileStoreV1::create_or_open(&session_root, store()).unwrap();
        let session = session();
        let revision = revision("revision-a");
        let first = b"{\"value\":\"first\"}\n";
        let second = b"{\"value\":\"second\"}\n";

        store
            .install_revision_candidate(
                1,
                session.clone(),
                revision.clone(),
                ExpectedRevision::Missing,
                first,
            )
            .unwrap();
        let error = store
            .install_revision_candidate(
                1,
                session.clone(),
                revision.clone(),
                ExpectedRevision::Missing,
                second,
            )
            .unwrap_err();
        assert!(!error.installed());
        assert!(error.to_string().contains("refusing to overwrite"));
        assert_eq!(
            store
                .open_revision_candidate(&session, &revision)
                .unwrap()
                .canonical_bytes(),
            first
        );
        assert!(
            std::fs::read_dir(PhysicalLayoutV1::staging_root(&session_root))
                .unwrap()
                .next()
                .is_none()
        );
    }

    #[test]
    fn post_install_sync_failure_reports_installed_and_keeps_verifiable_bytes() {
        let temp = TempDir::new().unwrap();
        let session_root = temp.path().join("sessions");
        let store = PhysicalFileStoreV1::create_or_open(&session_root, store()).unwrap();
        let session = session();
        let revision = revision("revision-a");
        let canonical = b"{\"value\":\"installed-unconfirmed\"}\n";
        store.inject_next_target_sync_failure();

        let error = store
            .install_revision_candidate(
                1,
                session.clone(),
                revision.clone(),
                ExpectedRevision::Missing,
                canonical,
            )
            .unwrap_err();
        assert!(error.installed());
        assert!(error.to_string().contains("durability is unconfirmed"));
        assert_eq!(
            store
                .open_revision_candidate(&session, &revision)
                .unwrap()
                .canonical_bytes(),
            canonical
        );
    }

    #[test]
    fn exact_read_rejects_payload_tampering_and_manifest_retargeting() {
        let temp = TempDir::new().unwrap();
        let session_root = temp.path().join("sessions");
        let store = PhysicalFileStoreV1::create_or_open(&session_root, store()).unwrap();
        let session = session();
        let revision_a = revision("revision-a");
        let revision_b = revision("revision-b");
        for revision in [&revision_a, &revision_b] {
            store
                .install_revision_candidate(
                    1,
                    session.clone(),
                    revision.clone(),
                    ExpectedRevision::Missing,
                    b"{\"value\":1}\n",
                )
                .unwrap();
        }

        std::fs::write(
            PhysicalLayoutV1::canonical_payload(&session_root, &session, &revision_a),
            b"{\"value\":2}\n",
        )
        .unwrap();
        let error = store
            .open_revision_candidate(&session, &revision_a)
            .unwrap_err();
        assert!(error.contains("digest mismatch"), "{error}");

        let manifest_b = std::fs::read(PhysicalLayoutV1::revision_manifest(
            &session_root,
            &session,
            &revision_b,
        ))
        .unwrap();
        std::fs::write(
            PhysicalLayoutV1::revision_manifest(&session_root, &session, &revision_a),
            manifest_b,
        )
        .unwrap();
        let error = store
            .open_revision_candidate(&session, &revision_a)
            .unwrap_err();
        assert!(error.contains("identity does not match"), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn exact_read_refuses_symlinked_revision_content() {
        use std::os::unix::fs::symlink;

        let temp = TempDir::new().unwrap();
        let session_root = temp.path().join("sessions");
        let store = PhysicalFileStoreV1::create_or_open(&session_root, store()).unwrap();
        let session = session();
        let revision = revision("revision-a");
        store
            .install_revision_candidate(
                1,
                session.clone(),
                revision.clone(),
                ExpectedRevision::Missing,
                b"{\"value\":1}\n",
            )
            .unwrap();

        let canonical = PhysicalLayoutV1::canonical_payload(&session_root, &session, &revision);
        std::fs::remove_file(&canonical).unwrap();
        symlink("/etc/passwd", &canonical).unwrap();
        let error = store
            .open_revision_candidate(&session, &revision)
            .unwrap_err();
        assert!(error.contains("cannot open canonical payload"), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn head_cas_requires_the_exact_fence_and_revision_then_advances_old_to_new() {
        let temp = TempDir::new().unwrap();
        let session_root = temp.path().join("sessions");
        let store = PhysicalFileStoreV1::create_or_open(&session_root, store()).unwrap();
        let session = session();
        let other_session = SessionId::new("s-other").unwrap();
        let fence = store.acquire_writer_fence(&session).unwrap();
        let busy = store.acquire_writer_fence(&session).unwrap_err();
        assert!(busy.contains("already active"), "{busy}");
        let other_fence = store.acquire_writer_fence(&other_session).unwrap();

        let revision_a = revision("revision-a");
        let candidate_a = store
            .install_revision_candidate(
                1,
                session.clone(),
                revision_a.clone(),
                ExpectedRevision::Missing,
                b"{\"value\":\"a\"}\n",
            )
            .unwrap();
        assert!(matches!(
            store.compare_and_swap_head(&other_fence, ExpectedRevision::Missing, &candidate_a,),
            Err(PhysicalHeadCasError::Fence(_))
        ));
        assert!(store.read_head(&session).unwrap().is_none());

        store
            .compare_and_swap_head(&fence, ExpectedRevision::Missing, &candidate_a)
            .unwrap();
        assert_eq!(
            store.read_head(&session).unwrap().unwrap().revision_id(),
            &revision_a
        );

        let wrong_parent = store
            .install_revision_candidate(
                1,
                session.clone(),
                revision("revision-wrong-parent"),
                ExpectedRevision::Missing,
                b"{\"value\":\"wrong-parent\"}\n",
            )
            .unwrap();
        assert!(matches!(
            store.compare_and_swap_head(
                &fence,
                ExpectedRevision::Exact(revision_a.clone()),
                &wrong_parent,
            ),
            Err(PhysicalHeadCasError::Candidate(_))
        ));
        assert_eq!(
            store.read_head(&session).unwrap().unwrap().revision_id(),
            &revision_a
        );

        let revision_b = revision("revision-b");
        let candidate_b = store
            .install_revision_candidate(
                1,
                session.clone(),
                revision_b.clone(),
                ExpectedRevision::Exact(revision_a.clone()),
                b"{\"value\":\"b\"}\n",
            )
            .unwrap();
        assert_eq!(
            candidate_b.manifest().parent_manifest_sha256(),
            Some(candidate_a.manifest_sha256())
        );
        store
            .compare_and_swap_head(
                &fence,
                ExpectedRevision::Exact(revision_a.clone()),
                &candidate_b,
            )
            .unwrap();

        let stale = store
            .install_revision_candidate(
                1,
                session.clone(),
                revision("revision-stale"),
                ExpectedRevision::Exact(revision_a.clone()),
                b"{\"value\":\"stale\"}\n",
            )
            .unwrap();
        assert!(matches!(
            store.compare_and_swap_head(
                &fence,
                ExpectedRevision::Exact(revision_a),
                &stale,
            ),
            Err(PhysicalHeadCasError::Conflict { observed: Some(observed), .. })
                if observed == revision_b
        ));
        assert_eq!(
            store.read_head(&session).unwrap().unwrap().revision_id(),
            &revision_b
        );
    }

    #[cfg(unix)]
    #[test]
    fn recovery_envelope_binds_an_unreadable_parent_and_stale_cas_fails() {
        let temp = TempDir::new().unwrap();
        let session_root = temp.path().join("sessions");
        let store = PhysicalFileStoreV1::create_or_open(&session_root, store()).unwrap();
        let session = session();
        let revision_a = revision("revision-a");
        let candidate_a = store
            .install_revision_candidate(
                1,
                session.clone(),
                revision_a.clone(),
                ExpectedRevision::Missing,
                b"{\"value\":\"a\"}\n",
            )
            .unwrap();
        let fence = store.acquire_writer_fence(&session).unwrap();
        store
            .compare_and_swap_head(&fence, ExpectedRevision::Missing, &candidate_a)
            .unwrap();
        let selected_a = store.read_head_envelope(&session).unwrap().unwrap();
        std::fs::remove_file(PhysicalLayoutV1::canonical_payload(
            &session_root,
            &session,
            &revision_a,
        ))
        .unwrap();
        assert!(store.read_head(&session).is_err());

        let revision_b = revision("revision-b");
        let candidate_b = store
            .install_revision_candidate_against_envelope(
                1,
                session.clone(),
                revision_b.clone(),
                selected_a.envelope(),
                b"{\"value\":\"recovered\"}\n",
            )
            .unwrap();
        assert_eq!(
            candidate_b.manifest().parent_manifest_sha256(),
            Some(selected_a.envelope().manifest_sha256())
        );
        store
            .compare_and_swap_head_against_envelope(&fence, &selected_a, &candidate_b)
            .unwrap();

        let revision_c = revision("revision-c");
        let stale = store
            .install_revision_candidate_against_envelope(
                1,
                session.clone(),
                revision_c,
                selected_a.envelope(),
                b"{\"value\":\"stale\"}\n",
            )
            .unwrap();
        assert!(matches!(
            store.compare_and_swap_head_against_envelope(&fence, &selected_a, &stale),
            Err(PhysicalHeadCasError::Conflict {
                observed: Some(observed),
                ..
            }) if observed == revision_b
        ));
        assert_eq!(
            store.read_head(&session).unwrap().unwrap().revision_id(),
            &revision_b
        );
    }

    #[cfg(unix)]
    #[test]
    fn post_install_head_sync_failure_reports_new_visible_head() {
        let temp = TempDir::new().unwrap();
        let session_root = temp.path().join("sessions");
        let store = PhysicalFileStoreV1::create_or_open(&session_root, store()).unwrap();
        let session = session();
        let revision_a = revision("revision-a");
        let candidate_a = store
            .install_revision_candidate(
                1,
                session.clone(),
                revision_a.clone(),
                ExpectedRevision::Missing,
                b"{\"value\":\"a\"}\n",
            )
            .unwrap();
        let fence = store.acquire_writer_fence(&session).unwrap();
        store
            .compare_and_swap_head(&fence, ExpectedRevision::Missing, &candidate_a)
            .unwrap();
        let revision_b = revision("revision-b");
        let candidate_b = store
            .install_revision_candidate(
                1,
                session.clone(),
                revision_b.clone(),
                ExpectedRevision::Exact(revision_a.clone()),
                b"{\"value\":\"b\"}\n",
            )
            .unwrap();
        store.inject_next_head_sync_failure();

        let error = store
            .compare_and_swap_head(&fence, ExpectedRevision::Exact(revision_a), &candidate_b)
            .unwrap_err();
        assert!(matches!(
            error,
            PhysicalHeadCasError::Publication {
                installed: true,
                ..
            }
        ));
        assert_eq!(
            store.read_head(&session).unwrap().unwrap().revision_id(),
            &revision_b
        );
    }

    #[cfg(unix)]
    #[test]
    fn head_read_rejects_manifest_digest_tampering_and_symlinks() {
        use std::os::unix::fs::symlink;

        let temp = TempDir::new().unwrap();
        let session_root = temp.path().join("sessions");
        let store = PhysicalFileStoreV1::create_or_open(&session_root, store()).unwrap();
        let session = session();
        let revision = revision("revision-a");
        let candidate = store
            .install_revision_candidate(
                1,
                session.clone(),
                revision,
                ExpectedRevision::Missing,
                b"{\"value\":\"a\"}\n",
            )
            .unwrap();
        let fence = store.acquire_writer_fence(&session).unwrap();
        store
            .compare_and_swap_head(&fence, ExpectedRevision::Missing, &candidate)
            .unwrap();

        let head_path = PhysicalLayoutV1::head_pointer(&session_root, &session);
        let mut value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&head_path).unwrap()).unwrap();
        value["revision_manifest_sha256"] = serde_json::json!("f".repeat(64));
        std::fs::write(&head_path, serde_json::to_vec(&value).unwrap()).unwrap();
        let error = store.read_head(&session).unwrap_err();
        assert!(error.contains("manifest digest mismatch"), "{error}");

        std::fs::remove_file(&head_path).unwrap();
        symlink("/etc/passwd", &head_path).unwrap();
        let error = store.read_head(&session).unwrap_err();
        assert!(error.contains("not a regular file"), "{error}");
    }
}
