//! Private physical file-store v1 contract.
//!
//! This module deliberately defines records and paths without mounting them.
//! Current `SavedSession` v1 files remain authoritative until the explicit
//! migration card can construct a complete canonical logical value.

use std::path::{Path, PathBuf};

use kitsu_core::{ExpectedRevision, RevisionId, SessionId, StoreId};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

pub(super) const PHYSICAL_SCHEMA_REVISION: u32 = 1;
pub(super) const MAX_PHYSICAL_RECORD_BYTES: usize = 64 * 1024;
pub(super) const MAX_CANONICAL_PAYLOAD_BYTES: u64 = 16 * 1024 * 1024;

const ROOT_COMPONENT: &str = ".kitsu-store-v1";
const STORE_RECORD_NAME: &str = "store.json";
const REVISIONS_COMPONENT: &str = "revisions";
const HEADS_COMPONENT: &str = "heads";
const STAGING_COMPONENT: &str = "staging";
const LOCKS_COMPONENT: &str = ".locks";
const REVISION_MANIFEST_NAME: &str = "manifest.json";
const CANONICAL_PAYLOAD_NAME: &str = "canonical.json";
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
        Ok(self)
    }
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
    decode_record::<StoreRecordV1>(bytes)?.validate()
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
    let record = decode_record::<RevisionManifestV1>(bytes)?.validate()?;
    if &record.store_id != expected_store
        || &record.session_id != expected_session
        || &record.revision_id != expected_revision
    {
        return Err("revision manifest identity does not match its physical location".to_string());
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
    let record = decode_record::<HeadRecordV1>(bytes)?.validate()?;
    if &record.store_id != expected_store || &record.session_id != expected_session {
        return Err("head record identity does not match its physical location".to_string());
    }
    Ok(record)
}

#[cfg(test)]
mod tests {
    use super::*;

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
            PhysicalLayoutV1::head_install_mode(),
            PhysicalInstallMode::ReplaceAfterFenceAndRevisionRecheck
        );
    }

    #[test]
    fn physical_records_have_fixed_closed_vectors_and_no_logical_head_projection() {
        let store_record = StoreRecordV1::new(store());
        assert_eq!(
            encode_store_record(&store_record).unwrap(),
            br#"{"record_type":"store","physical_schema_revision":1,"store_id":"store-a"}
"#
        );
        assert_eq!(
            serde_json::to_value(&store_record).unwrap(),
            serde_json::json!({
                "record_type": "store",
                "physical_schema_revision": 1,
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
            123,
            digest("a"),
        )
        .unwrap();
        let manifest_value = serde_json::to_value(&manifest).unwrap();
        assert_eq!(manifest_value["record_type"], "revision_manifest");
        assert_eq!(manifest_value["canonical_bytes"], 123);
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
    }

    #[test]
    fn future_unknown_mismatched_and_malformed_records_fail_closed() {
        let manifest = RevisionManifestV1::new(
            1,
            store(),
            session(),
            revision("revision-b"),
            ExpectedRevision::Exact(revision("revision-a")),
            123,
            digest("a"),
        )
        .unwrap();
        let mut value = serde_json::to_value(&manifest).unwrap();
        value["physical_schema_revision"] = serde_json::json!(2);
        assert!(decode_revision_manifest(
            &serde_json::to_vec(&value).unwrap(),
            &store(),
            &session(),
            &revision("revision-b")
        )
        .is_err());
        value["physical_schema_revision"] = serde_json::json!(1);
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
}
