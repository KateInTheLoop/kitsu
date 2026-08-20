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

/// Descriptor-pinned owner of the unmounted physical-v1 namespace.
///
/// Holding each directory open prevents pathname ancestor replacement from
/// redirecting later revision reads or writes on Unix. The type remains
/// private until canonical HEAD publication and migration are complete.
#[derive(Debug)]
pub(super) struct PhysicalFileStoreV1 {
    _root: PrivateDirectory,
    revisions: PrivateDirectory,
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

#[derive(Debug)]
pub(super) struct PhysicalWriterFenceV1 {
    store_id: StoreId,
    session_id: SessionId,
    #[cfg(unix)]
    _file: std::fs::File,
}

impl PhysicalWriterFenceV1 {
    fn confirm(&self, store_id: &StoreId, session_id: &SessionId) -> Result<(), String> {
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
    pub(super) fn create_or_open(session_root: &Path, store_id: StoreId) -> Result<Self, String> {
        let session_root = PrivateDirectory::prepare(session_root, "physical session root")?;
        let root = session_root.prepare_child(
            OsStr::new(ROOT_COMPONENT),
            "physical session store namespace",
        )?;
        session_root.sync("physical session root")?;

        let record = open_or_create_store_record(&root, store_id)?;
        let revisions = root.prepare_child(
            OsStr::new(REVISIONS_COMPONENT),
            "physical revision namespace",
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
    ) -> Result<Option<VerifiedRevisionHeadV1>, String> {
        let head_name = format!("{}.json", encode_component(session_id.as_str()));
        match self.heads.entry_kind(OsStr::new(&head_name)) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Ok(PrivateEntryKind::File) => {}
            Ok(_) => {
                return Err(format!(
                    "physical HEAD '{}' is not a regular file",
                    self.heads.path.join(&head_name).display()
                ));
            }
            Err(error) => {
                return Err(format!(
                    "cannot inspect physical HEAD '{}': {error}",
                    self.heads.path.join(&head_name).display()
                ));
            }
        }
        let bytes = read_bounded_regular(
            &self.heads,
            OsStr::new(&head_name),
            MAX_PHYSICAL_RECORD_BYTES as u64,
            "physical HEAD",
        )?;
        let record = decode_head_record(&bytes, self.store_id(), session_id)?;
        let candidate = self.open_revision_candidate(session_id, &record.revision_id)?;
        if candidate.manifest_sha256 != record.revision_manifest_sha256 {
            return Err(format!(
                "physical HEAD manifest digest mismatch: HEAD declares {}, observed {}",
                record.revision_manifest_sha256.as_str(),
                candidate.manifest_sha256.as_str()
            ));
        }
        Ok(Some(VerifiedRevisionHeadV1 { record, candidate }))
    }

    pub(super) fn compare_and_swap_head(
        &self,
        fence: &PhysicalWriterFenceV1,
        expected: ExpectedRevision,
        candidate: &InstalledRevisionCandidateV1,
    ) -> Result<VerifiedRevisionHeadV1, PhysicalHeadCasError> {
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
            .map_err(PhysicalHeadCasError::Candidate)?;
        if reloaded.manifest != candidate.manifest
            || reloaded.manifest_sha256 != candidate.manifest_sha256
        {
            return Err(PhysicalHeadCasError::Candidate(
                "revision candidate receipt does not match the exact installed manifest"
                    .to_string(),
            ));
        }

        let current = self
            .read_head(session_id)
            .map_err(PhysicalHeadCasError::Candidate)?;
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
        let replace = matches!(expected, ExpectedRevision::Exact(_));
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
    fn inject_next_head_sync_failure(&self) {
        self.fail_next_head_sync.set(true);
    }

    pub(super) fn install_revision_candidate(
        &self,
        logical_schema_revision: u32,
        session_id: SessionId,
        revision_id: RevisionId,
        parent_revision: ExpectedRevision,
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
    fn inject_next_target_sync_failure(&self) {
        self.fail_next_target_sync.set(true);
    }

    pub(super) fn open_revision_candidate(
        &self,
        session_id: &SessionId,
        revision_id: &RevisionId,
    ) -> Result<VerifiedRevisionCandidateV1, String> {
        let session_name = encode_component(session_id.as_str());
        let revision_name = encode_component(revision_id.as_str());
        let session_revisions = self.revisions.open_child(
            OsStr::new(&session_name),
            "physical session revision namespace",
        )?;
        let revision = session_revisions
            .open_child(OsStr::new(&revision_name), "physical revision candidate")?;
        let manifest_bytes = read_bounded_regular(
            &revision,
            OsStr::new(REVISION_MANIFEST_NAME),
            MAX_PHYSICAL_RECORD_BYTES as u64,
            "revision manifest",
        )?;
        let manifest =
            decode_revision_manifest(&manifest_bytes, self.store_id(), session_id, revision_id)?;
        let canonical_bytes = read_bounded_regular(
            &revision,
            OsStr::new(CANONICAL_PAYLOAD_NAME),
            MAX_CANONICAL_PAYLOAD_BYTES,
            "canonical payload",
        )?;
        if canonical_bytes.len() as u64 != manifest.canonical_bytes {
            return Err(format!(
                "canonical payload length mismatch: manifest declares {}, read {}",
                manifest.canonical_bytes,
                canonical_bytes.len()
            ));
        }
        let observed_digest = digest_bytes(&canonical_bytes)?;
        if observed_digest != manifest.canonical_sha256 {
            return Err(format!(
                "canonical payload digest mismatch: manifest declares {}, observed {}",
                manifest.canonical_sha256.as_str(),
                observed_digest.as_str()
            ));
        }
        let manifest_sha256 = digest_bytes(&manifest_bytes)?;
        Ok(VerifiedRevisionCandidateV1 {
            manifest,
            manifest_sha256,
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
