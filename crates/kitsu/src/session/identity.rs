//! Stable local identity for the private canonical file-store adapter.

use std::ffi::OsStr;
use std::io::Read;

use kitsu_core::session::StoreId;
use serde::{Deserialize, Serialize};

use super::physical::{PhysicalFileStoreV1, PhysicalReadErrorV1};
use super::{atomic_write_private_in_detailed, fresh_id, PrivateDirectory, PrivateEntryKind};

const LOCAL_STORE_IDENTITY_SCHEMA_REVISION: u32 = 1;
const MAX_LOCAL_STORE_IDENTITY_BYTES: u64 = 4 * 1024;
pub(super) const LOCAL_STORE_IDENTITY_NAME: &str = ".kitsu-store-identity-v1.json";

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LocalStoreIdentityRecordV1 {
    schema_revision: u32,
    store_id: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum LocalStoreIdentitySourceV1 {
    Existing,
    Created,
    ConcurrentWinner,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct LocalStoreIdentityReceiptV1 {
    store_id: StoreId,
    source: LocalStoreIdentitySourceV1,
    durability_warning: Option<String>,
}

impl LocalStoreIdentityReceiptV1 {
    pub(super) fn store_id(&self) -> &StoreId {
        &self.store_id
    }

    pub(super) fn source(&self) -> LocalStoreIdentitySourceV1 {
        self.source
    }

    pub(super) fn durability_warning(&self) -> Option<&str> {
        self.durability_warning.as_deref()
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum LocalStoreIdentityErrorV1 {
    UnboundPhysicalNamespace,
    UnsafeEntry,
    Malformed,
    Unsupported { observed: u32, supported: u32 },
    InvalidStoreIdentity,
    Read(String),
    Write { message: String, installed: bool },
}

/// Resolve the composition identity for live activation. If no composition
/// identity exists, any physical namespace entry is an orphan and blocks
/// publication. Supported initializers therefore converge through the
/// create-only identity before any physical store can be created.
pub(super) fn acquire_local_store_identity_for_activation_v1(
    directory: &PrivateDirectory,
) -> Result<LocalStoreIdentityReceiptV1, LocalStoreIdentityErrorV1> {
    if let Some(receipt) = open_local_store_identity_v1(directory)? {
        return Ok(receipt);
    }
    if PhysicalFileStoreV1::namespace_entry_present(directory)
        .map_err(|error: PhysicalReadErrorV1| LocalStoreIdentityErrorV1::Read(error.to_string()))?
    {
        return Err(LocalStoreIdentityErrorV1::UnboundPhysicalNamespace);
    }
    acquire_local_store_identity_v1(directory)
}

pub(super) fn open_local_store_identity_v1(
    directory: &PrivateDirectory,
) -> Result<Option<LocalStoreIdentityReceiptV1>, LocalStoreIdentityErrorV1> {
    read_local_store_identity_v1(directory).map(|identity| {
        identity.map(|store_id| LocalStoreIdentityReceiptV1 {
            store_id,
            source: LocalStoreIdentitySourceV1::Existing,
            durability_warning: None,
        })
    })
}

pub(super) fn acquire_local_store_identity_v1(
    directory: &PrivateDirectory,
) -> Result<LocalStoreIdentityReceiptV1, LocalStoreIdentityErrorV1> {
    let candidate = StoreId::new(format!("local-{}", fresh_id())).map_err(|_| {
        LocalStoreIdentityErrorV1::Write {
            message: "cannot mint a bounded local session store identity".to_string(),
            installed: false,
        }
    })?;
    acquire_local_store_identity_with_candidate_v1(directory, candidate, || Ok(()))
}

fn encode_local_store_identity_v1(store_id: &StoreId) -> Result<Vec<u8>, String> {
    serde_json::to_vec_pretty(&LocalStoreIdentityRecordV1 {
        schema_revision: LOCAL_STORE_IDENTITY_SCHEMA_REVISION,
        store_id: store_id.as_str().to_string(),
    })
    .map_err(|error| format!("cannot encode local session store identity: {error}"))
}

fn decode_local_store_identity_v1(bytes: &[u8]) -> Result<StoreId, LocalStoreIdentityErrorV1> {
    let record: LocalStoreIdentityRecordV1 =
        serde_json::from_slice(bytes).map_err(|_| LocalStoreIdentityErrorV1::Malformed)?;
    if record.schema_revision == 0 {
        return Err(LocalStoreIdentityErrorV1::Malformed);
    }
    if record.schema_revision != LOCAL_STORE_IDENTITY_SCHEMA_REVISION {
        return Err(LocalStoreIdentityErrorV1::Unsupported {
            observed: record.schema_revision,
            supported: LOCAL_STORE_IDENTITY_SCHEMA_REVISION,
        });
    }
    StoreId::new(record.store_id).map_err(|_| LocalStoreIdentityErrorV1::InvalidStoreIdentity)
}

fn read_local_store_identity_v1(
    directory: &PrivateDirectory,
) -> Result<Option<StoreId>, LocalStoreIdentityErrorV1> {
    match directory.entry_kind(OsStr::new(LOCAL_STORE_IDENTITY_NAME)) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Ok(PrivateEntryKind::File) => {}
        Ok(_) => return Err(LocalStoreIdentityErrorV1::UnsafeEntry),
        Err(error) => return Err(LocalStoreIdentityErrorV1::Read(error.to_string())),
    }
    let file = directory
        .open_regular_file(
            OsStr::new(LOCAL_STORE_IDENTITY_NAME),
            "local session store identity",
        )
        .map_err(LocalStoreIdentityErrorV1::Read)?;
    let metadata = file
        .metadata()
        .map_err(|error| LocalStoreIdentityErrorV1::Read(error.to_string()))?;
    if metadata.len() == 0 || metadata.len() > MAX_LOCAL_STORE_IDENTITY_BYTES {
        return Err(LocalStoreIdentityErrorV1::Malformed);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.uid() != unsafe { libc::geteuid() }
            || metadata.nlink() != 1
            || metadata.mode() & 0o077 != 0
        {
            return Err(LocalStoreIdentityErrorV1::UnsafeEntry);
        }
    }
    let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(0));
    file.take(MAX_LOCAL_STORE_IDENTITY_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| LocalStoreIdentityErrorV1::Read(error.to_string()))?;
    if bytes.len() as u64 > MAX_LOCAL_STORE_IDENTITY_BYTES {
        return Err(LocalStoreIdentityErrorV1::Malformed);
    }
    decode_local_store_identity_v1(&bytes).map(Some)
}

fn acquire_local_store_identity_with_candidate_v1<F>(
    directory: &PrivateDirectory,
    candidate: StoreId,
    before_create: F,
) -> Result<LocalStoreIdentityReceiptV1, LocalStoreIdentityErrorV1>
where
    F: FnOnce() -> Result<(), LocalStoreIdentityErrorV1>,
{
    if let Some(store_id) = read_local_store_identity_v1(directory)? {
        return Ok(LocalStoreIdentityReceiptV1 {
            store_id,
            source: LocalStoreIdentitySourceV1::Existing,
            durability_warning: None,
        });
    }
    let bytes = encode_local_store_identity_v1(&candidate).map_err(|message| {
        LocalStoreIdentityErrorV1::Write {
            message,
            installed: false,
        }
    })?;
    before_create()?;
    match atomic_write_private_in_detailed(
        directory,
        OsStr::new(LOCAL_STORE_IDENTITY_NAME),
        &bytes,
        false,
        "local session store identity",
    ) {
        Ok(_) => {
            let observed = read_local_store_identity_v1(directory)?.ok_or_else(|| {
                LocalStoreIdentityErrorV1::Write {
                    message: "local session store identity was installed but reloaded missing"
                        .to_string(),
                    installed: true,
                }
            })?;
            if observed != candidate {
                return Err(LocalStoreIdentityErrorV1::Write {
                    message: "local session store identity changed after installation".to_string(),
                    installed: true,
                });
            }
            Ok(LocalStoreIdentityReceiptV1 {
                store_id: candidate,
                source: LocalStoreIdentitySourceV1::Created,
                durability_warning: None,
            })
        }
        Err(failure) if failure.installed => {
            let observed = read_local_store_identity_v1(directory)?.ok_or_else(|| {
                LocalStoreIdentityErrorV1::Write {
                    message: failure.message.clone(),
                    installed: true,
                }
            })?;
            if observed != candidate {
                return Err(LocalStoreIdentityErrorV1::Write {
                    message: "installed local session store identity does not match its candidate"
                        .to_string(),
                    installed: true,
                });
            }
            Ok(LocalStoreIdentityReceiptV1 {
                store_id: candidate,
                source: LocalStoreIdentitySourceV1::Created,
                durability_warning: Some(failure.message),
            })
        }
        Err(failure) => match read_local_store_identity_v1(directory)? {
            Some(store_id) => Ok(LocalStoreIdentityReceiptV1 {
                store_id,
                source: LocalStoreIdentitySourceV1::ConcurrentWinner,
                durability_warning: None,
            }),
            None => Err(LocalStoreIdentityErrorV1::Write {
                message: failure.message,
                installed: false,
            }),
        },
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;

    use super::*;

    fn store(value: &str) -> StoreId {
        StoreId::new(value).unwrap()
    }

    #[test]
    fn local_identity_is_created_once_and_reopens_exactly_without_mounting_store() {
        let root = tempfile::tempdir().unwrap();
        let sessions = root.path().join("sessions");
        let directory = PrivateDirectory::prepare(&sessions, "identity fixture").unwrap();

        let created = acquire_local_store_identity_v1(&directory).unwrap();
        let created_id = created.store_id().clone();
        assert!(created_id.as_str().starts_with("local-s-"));
        assert_eq!(created.source(), LocalStoreIdentitySourceV1::Created);
        assert_eq!(created.durability_warning(), None);
        let exact_bytes = std::fs::read(sessions.join(LOCAL_STORE_IDENTITY_NAME)).unwrap();

        let reopened = acquire_local_store_identity_with_candidate_v1(
            &directory,
            store("store-must-not-replace"),
            || Ok(()),
        )
        .unwrap();
        assert_eq!(reopened.store_id(), &created_id);
        assert_eq!(reopened.source(), LocalStoreIdentitySourceV1::Existing);
        assert_eq!(
            std::fs::read(sessions.join(LOCAL_STORE_IDENTITY_NAME)).unwrap(),
            exact_bytes
        );
        assert!(!sessions.join(".kitsu-store-v1").exists());
    }

    #[test]
    fn local_identity_adopts_only_a_valid_concurrent_winner_and_never_repairs_corruption() {
        let root = tempfile::tempdir().unwrap();
        let sessions = root.path().join("sessions");
        let directory = PrivateDirectory::prepare(&sessions, "identity fixture").unwrap();
        let winner = store("store-winner");
        let raced = acquire_local_store_identity_with_candidate_v1(
            &directory,
            store("store-loser"),
            || {
                acquire_local_store_identity_with_candidate_v1(
                    &directory,
                    winner.clone(),
                    || Ok(()),
                )
                .map(|_| ())
            },
        )
        .unwrap();
        assert_eq!(raced.store_id(), &winner);
        assert_eq!(raced.source(), LocalStoreIdentitySourceV1::ConcurrentWinner);

        std::fs::write(sessions.join(LOCAL_STORE_IDENTITY_NAME), b"{").unwrap();
        let before = std::fs::read(sessions.join(LOCAL_STORE_IDENTITY_NAME)).unwrap();
        assert!(matches!(
            acquire_local_store_identity_with_candidate_v1(
                &directory,
                store("store-repair"),
                || Ok(())
            ),
            Err(LocalStoreIdentityErrorV1::Malformed)
        ));
        assert_eq!(
            std::fs::read(sessions.join(LOCAL_STORE_IDENTITY_NAME)).unwrap(),
            before
        );
        assert_eq!(
            directory
                .entry_kind(OsStr::new(LOCAL_STORE_IDENTITY_NAME))
                .unwrap(),
            super::super::PrivateEntryKind::File
        );

        std::fs::write(
            sessions.join(LOCAL_STORE_IDENTITY_NAME),
            br#"{"schema_revision":2,"store_id":"store-future"}"#,
        )
        .unwrap();
        assert!(matches!(
            acquire_local_store_identity_v1(&directory),
            Err(LocalStoreIdentityErrorV1::Unsupported {
                observed: 2,
                supported: 1
            })
        ));
        std::fs::write(
            sessions.join(LOCAL_STORE_IDENTITY_NAME),
            br#"{"schema_revision":1,"store_id":"store-ok","extra":true}"#,
        )
        .unwrap();
        assert!(matches!(
            acquire_local_store_identity_v1(&directory),
            Err(LocalStoreIdentityErrorV1::Malformed)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn local_identity_refuses_symlink_and_shared_or_permissive_files() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let root = tempfile::tempdir().unwrap();
        let sessions = root.path().join("sessions");
        let directory = PrivateDirectory::prepare(&sessions, "identity fixture").unwrap();
        let outside = root.path().join("outside.json");
        std::fs::write(
            &outside,
            br#"{"schema_revision":1,"store_id":"store-outside"}"#,
        )
        .unwrap();
        symlink(&outside, sessions.join(LOCAL_STORE_IDENTITY_NAME)).unwrap();
        assert!(matches!(
            acquire_local_store_identity_v1(&directory),
            Err(LocalStoreIdentityErrorV1::UnsafeEntry)
        ));
        assert_eq!(
            std::fs::read(&outside).unwrap(),
            br#"{"schema_revision":1,"store_id":"store-outside"}"#
        );

        std::fs::remove_file(sessions.join(LOCAL_STORE_IDENTITY_NAME)).unwrap();
        let identity = sessions.join(LOCAL_STORE_IDENTITY_NAME);
        std::fs::write(
            &identity,
            br#"{"schema_revision":1,"store_id":"store-permissive"}"#,
        )
        .unwrap();
        std::fs::set_permissions(&identity, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(matches!(
            acquire_local_store_identity_v1(&directory),
            Err(LocalStoreIdentityErrorV1::UnsafeEntry)
        ));

        std::fs::set_permissions(&identity, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::hard_link(&identity, root.path().join("identity-hardlink.json")).unwrap();
        assert!(matches!(
            acquire_local_store_identity_v1(&directory),
            Err(LocalStoreIdentityErrorV1::UnsafeEntry)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn local_identity_reports_installed_unconfirmed_and_stays_descriptor_pinned() {
        let root = tempfile::tempdir().unwrap();
        let sessions = root.path().join("sessions");
        let moved = root.path().join("sessions-original");
        let directory = PrivateDirectory::prepare(&sessions, "identity fixture").unwrap();
        std::fs::rename(&sessions, &moved).unwrap();
        std::fs::create_dir(&sessions).unwrap();
        std::fs::write(sessions.join(LOCAL_STORE_IDENTITY_NAME), b"replacement").unwrap();
        super::super::FAIL_NEXT_PRIVATE_DIRECTORY_SYNC.with(|fail| {
            assert!(!fail.replace(true));
        });

        let receipt = acquire_local_store_identity_with_candidate_v1(
            &directory,
            store("store-pinned"),
            || Ok(()),
        )
        .unwrap();
        assert_eq!(receipt.store_id(), &store("store-pinned"));
        assert_eq!(receipt.source(), LocalStoreIdentitySourceV1::Created);
        assert!(receipt.durability_warning().is_some());
        assert_eq!(
            decode_local_store_identity_v1(
                &std::fs::read(moved.join(LOCAL_STORE_IDENTITY_NAME)).unwrap()
            )
            .unwrap(),
            store("store-pinned")
        );
        assert_eq!(
            std::fs::read(sessions.join(LOCAL_STORE_IDENTITY_NAME)).unwrap(),
            b"replacement"
        );
    }
}
