//! Admission between the current-v1 snapshot and canonical session store.
//!
//! The decision table is pure. Its observation seam performs bounded,
//! descriptor-pinned reads only: it never creates, repairs, syncs, locks,
//! migrates, selects, or removes either representation.

use std::ffi::OsStr;
use std::io::Read;

use kitsu_core::session::{LogicalHead, RevisionId, SessionId, SessionLookupState, StoreId};

use super::canonical::CanonicalSessionRevisionV1;
use super::physical::{PhysicalFileStoreV1, PhysicalReadErrorV1};
use super::transaction::lookup_canonical_mount_value_v1;
use super::{PrivateDirectory, PrivateEntryKind, SavedSession, MAX_SESSION_BYTES, SESSION_VERSION};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum CurrentV1MountFaultV1 {
    Malformed,
    IdentityMismatch,
    UnsupportedVersion { observed: u32, supported: u32 },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum CurrentV1MountObservationV1 {
    Missing,
    Present,
    Unavailable(CurrentV1MountFaultV1),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SessionMountRejectionV1 {
    UnavailableRepresentation,
    CanonicalTombstone,
    AmbiguousDualPresence,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum SessionMountAdmissionV1 {
    Empty,
    CurrentV1Only,
    CanonicalOnly {
        revision: RevisionId,
        head: LogicalHead,
    },
    Rejected {
        reason: SessionMountRejectionV1,
        current_v1: CurrentV1MountObservationV1,
        canonical: SessionLookupState,
    },
}

#[derive(Debug)]
pub(super) struct CurrentV1MountValueV1 {
    saved: SavedSession,
    source_bytes: Vec<u8>,
}

impl CurrentV1MountValueV1 {
    pub(super) fn saved(&self) -> &SavedSession {
        &self.saved
    }

    pub(super) fn source_bytes(&self) -> &[u8] {
        &self.source_bytes
    }
}

#[derive(Debug)]
pub(super) enum SessionMountSelectionV1 {
    Empty,
    CurrentV1(CurrentV1MountValueV1),
    Canonical(Box<CanonicalSessionRevisionV1>),
    Rejected(SessionMountAdmissionV1),
}

impl SessionMountSelectionV1 {
    fn into_admission(self) -> SessionMountAdmissionV1 {
        match self {
            Self::Empty => SessionMountAdmissionV1::Empty,
            Self::CurrentV1(_) => SessionMountAdmissionV1::CurrentV1Only,
            Self::Canonical(payload) => SessionMountAdmissionV1::CanonicalOnly {
                revision: payload.revision_id().clone(),
                head: payload.selected_head().clone(),
            },
            Self::Rejected(admission) => admission,
        }
    }
}

#[derive(Debug)]
struct CurrentV1MountReadV1 {
    observation: CurrentV1MountObservationV1,
    value: Option<CurrentV1MountValueV1>,
}

pub(super) fn admit_session_mount_v1(
    current_v1: CurrentV1MountObservationV1,
    canonical: SessionLookupState,
) -> SessionMountAdmissionV1 {
    if matches!(current_v1, CurrentV1MountObservationV1::Unavailable(_))
        || matches!(
            canonical,
            SessionLookupState::Corrupt { .. }
                | SessionLookupState::Unsupported { .. }
                | SessionLookupState::Conflict { .. }
        )
    {
        return SessionMountAdmissionV1::Rejected {
            reason: SessionMountRejectionV1::UnavailableRepresentation,
            current_v1,
            canonical,
        };
    }

    match (current_v1, canonical) {
        (
            current_v1,
            canonical @ SessionLookupState::Missing {
                tombstone_revision: Some(_),
            },
        ) => SessionMountAdmissionV1::Rejected {
            reason: SessionMountRejectionV1::CanonicalTombstone,
            current_v1,
            canonical,
        },
        (CurrentV1MountObservationV1::Present, canonical @ SessionLookupState::Present { .. }) => {
            SessionMountAdmissionV1::Rejected {
                reason: SessionMountRejectionV1::AmbiguousDualPresence,
                current_v1: CurrentV1MountObservationV1::Present,
                canonical,
            }
        }
        (
            CurrentV1MountObservationV1::Missing,
            SessionLookupState::Missing {
                tombstone_revision: None,
            },
        ) => SessionMountAdmissionV1::Empty,
        (
            CurrentV1MountObservationV1::Present,
            SessionLookupState::Missing {
                tombstone_revision: None,
            },
        ) => SessionMountAdmissionV1::CurrentV1Only,
        (CurrentV1MountObservationV1::Missing, SessionLookupState::Present { revision, head }) => {
            SessionMountAdmissionV1::CanonicalOnly { revision, head }
        }
        (CurrentV1MountObservationV1::Unavailable(_), _)
        | (_, SessionLookupState::Corrupt { .. })
        | (_, SessionLookupState::Unsupported { .. })
        | (_, SessionLookupState::Conflict { .. }) => {
            unreachable!("unavailable representations return before clean mount routing")
        }
    }
}

pub(super) fn observe_session_mount_v1(
    session_root: &PrivateDirectory,
    expected_store: StoreId,
    session_id: &SessionId,
) -> SessionMountAdmissionV1 {
    select_session_mount_v1(session_root, expected_store, session_id).into_admission()
}

pub(super) fn select_session_mount_v1(
    session_root: &PrivateDirectory,
    expected_store: StoreId,
    session_id: &SessionId,
) -> SessionMountSelectionV1 {
    let current_v1 = observe_current_v1(session_root, session_id);
    let (canonical_state, canonical_value) =
        match PhysicalFileStoreV1::open_existing(session_root, expected_store) {
            Ok(None) => (
                SessionLookupState::Missing {
                    tombstone_revision: None,
                },
                None,
            ),
            Ok(Some(store)) => lookup_canonical_mount_value_v1(&store, session_id),
            Err(error) => (physical_store_error_state(session_id, error), None),
        };
    let admission = admit_session_mount_v1(current_v1.observation.clone(), canonical_state);
    match admission {
        SessionMountAdmissionV1::Empty => SessionMountSelectionV1::Empty,
        SessionMountAdmissionV1::CurrentV1Only => SessionMountSelectionV1::CurrentV1(
            current_v1
                .value
                .expect("current-v1-only admission always retains its exact parsed value"),
        ),
        SessionMountAdmissionV1::CanonicalOnly { .. } => {
            SessionMountSelectionV1::Canonical(Box::new(
                canonical_value
                    .expect("canonical-only admission always retains its exact verified payload"),
            ))
        }
        rejected @ SessionMountAdmissionV1::Rejected { .. } => {
            SessionMountSelectionV1::Rejected(rejected)
        }
    }
}

fn observe_current_v1(
    session_root: &PrivateDirectory,
    session_id: &SessionId,
) -> CurrentV1MountReadV1 {
    let name = format!("{}.json", session_id.as_str());
    match session_root.entry_kind(OsStr::new(&name)) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return CurrentV1MountReadV1 {
                observation: CurrentV1MountObservationV1::Missing,
                value: None,
            };
        }
        Ok(PrivateEntryKind::File) => {}
        Ok(_) | Err(_) => {
            return CurrentV1MountReadV1 {
                observation: CurrentV1MountObservationV1::Unavailable(
                    CurrentV1MountFaultV1::Malformed,
                ),
                value: None,
            };
        }
    }

    let observed = (|| {
        let file = session_root
            .open_regular_file(OsStr::new(&name), "current-v1 session snapshot")
            .map_err(|_| CurrentV1MountFaultV1::Malformed)?;
        let metadata = file
            .metadata()
            .map_err(|_| CurrentV1MountFaultV1::Malformed)?;
        if !metadata.file_type().is_file() || metadata.len() > MAX_SESSION_BYTES {
            return Err(CurrentV1MountFaultV1::Malformed);
        }
        let mut bytes = Vec::with_capacity(metadata.len() as usize);
        file.take(MAX_SESSION_BYTES.saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(|_| CurrentV1MountFaultV1::Malformed)?;
        if bytes.len() as u64 > MAX_SESSION_BYTES {
            return Err(CurrentV1MountFaultV1::Malformed);
        }
        let saved: SavedSession =
            serde_json::from_slice(&bytes).map_err(|_| CurrentV1MountFaultV1::Malformed)?;
        if saved.version != SESSION_VERSION {
            return Err(CurrentV1MountFaultV1::UnsupportedVersion {
                observed: saved.version,
                supported: SESSION_VERSION,
            });
        }
        if saved.id != session_id.as_str() || !super::valid_id(&saved.id) {
            return Err(CurrentV1MountFaultV1::IdentityMismatch);
        }
        Ok(CurrentV1MountValueV1 {
            saved,
            source_bytes: bytes,
        })
    })();

    match observed {
        Ok(value) => CurrentV1MountReadV1 {
            observation: CurrentV1MountObservationV1::Present,
            value: Some(value),
        },
        Err(error) => CurrentV1MountReadV1 {
            observation: CurrentV1MountObservationV1::Unavailable(error),
            value: None,
        },
    }
}

fn physical_store_error_state(
    session_id: &SessionId,
    error: PhysicalReadErrorV1,
) -> SessionLookupState {
    match error {
        PhysicalReadErrorV1::UnsupportedPhysicalSchema {
            observed,
            supported,
            ..
        } => SessionLookupState::Unsupported {
            dimension: kitsu_core::session::SchemaDimension::Physical,
            observed_revision: observed,
            supported_revision: supported,
        },
        PhysicalReadErrorV1::MissingReference(_) => SessionLookupState::Corrupt {
            session: session_id.clone(),
            observed_revision: None,
            reason: kitsu_core::session::CorruptSessionReason::MissingReference,
        },
        PhysicalReadErrorV1::Malformed(_) => SessionLookupState::Corrupt {
            session: session_id.clone(),
            observed_revision: None,
            reason: kitsu_core::session::CorruptSessionReason::Malformed,
        },
        PhysicalReadErrorV1::IdentityMismatch(_) => SessionLookupState::Corrupt {
            session: session_id.clone(),
            observed_revision: None,
            reason: kitsu_core::session::CorruptSessionReason::IdentityMismatch,
        },
        PhysicalReadErrorV1::DigestMismatch(_) => SessionLookupState::Corrupt {
            session: session_id.clone(),
            observed_revision: None,
            reason: kitsu_core::session::CorruptSessionReason::DigestMismatch,
        },
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use kitsu_core::session::{
        CorruptSessionReason, ExpectedRevision, LogicalHead, RevisionId, SchemaDimension,
        SessionId, SessionLookupState, StoreId, TurnId,
    };

    use super::{
        admit_session_mount_v1, observe_session_mount_v1, select_session_mount_v1,
        CurrentV1MountFaultV1, CurrentV1MountObservationV1, SessionMountAdmissionV1,
        SessionMountRejectionV1, SessionMountSelectionV1,
    };

    fn revision(value: &str) -> RevisionId {
        RevisionId::new(value).unwrap()
    }

    fn canonical_present() -> SessionLookupState {
        SessionLookupState::Present {
            revision: revision("rev-a"),
            head: LogicalHead::Turn(TurnId::new("turn-a").unwrap()),
        }
    }

    fn current_v1_bytes(version: u32, id: &str) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "version": version,
            "id": id,
            "created_at": "2026-08-20T10:00:00.000Z",
            "updated_at": "2026-08-20T10:01:00.000Z",
            "directory": "/work/tree",
            "gateway": "openai",
            "model": "gpt-test",
            "name": null,
            "project_context": null,
            "messages": []
        }))
        .unwrap()
    }

    fn tree_snapshot(root: &Path) -> Vec<(PathBuf, Option<Vec<u8>>)> {
        fn visit(root: &Path, path: &Path, output: &mut Vec<(PathBuf, Option<Vec<u8>>)>) {
            let mut entries = std::fs::read_dir(path)
                .unwrap()
                .map(|entry| entry.unwrap())
                .collect::<Vec<_>>();
            entries.sort_by_key(|entry| entry.file_name());
            for entry in entries {
                let path = entry.path();
                let relative = path.strip_prefix(root).unwrap().to_path_buf();
                let metadata = std::fs::symlink_metadata(&path).unwrap();
                if metadata.file_type().is_dir() {
                    output.push((relative, None));
                    visit(root, &path, output);
                } else if metadata.file_type().is_file() {
                    output.push((relative, Some(std::fs::read(&path).unwrap())));
                } else {
                    output.push((relative, Some(Vec::new())));
                }
            }
        }

        let mut output = Vec::new();
        visit(root, root, &mut output);
        output
    }

    #[test]
    fn clean_mount_table_admits_exactly_one_representation() {
        let missing = SessionLookupState::Missing {
            tombstone_revision: None,
        };

        assert_eq!(
            admit_session_mount_v1(CurrentV1MountObservationV1::Missing, missing.clone()),
            SessionMountAdmissionV1::Empty
        );
        assert_eq!(
            admit_session_mount_v1(CurrentV1MountObservationV1::Present, missing),
            SessionMountAdmissionV1::CurrentV1Only
        );
        assert_eq!(
            admit_session_mount_v1(CurrentV1MountObservationV1::Missing, canonical_present()),
            SessionMountAdmissionV1::CanonicalOnly {
                revision: revision("rev-a"),
                head: LogicalHead::Turn(TurnId::new("turn-a").unwrap()),
            }
        );

        let canonical = canonical_present();
        assert_eq!(
            admit_session_mount_v1(CurrentV1MountObservationV1::Present, canonical.clone()),
            SessionMountAdmissionV1::Rejected {
                reason: SessionMountRejectionV1::AmbiguousDualPresence,
                current_v1: CurrentV1MountObservationV1::Present,
                canonical,
            }
        );
    }

    #[test]
    fn unavailable_or_future_representation_never_collapses_to_missing() {
        let missing = SessionLookupState::Missing {
            tombstone_revision: None,
        };
        for fault in [
            CurrentV1MountFaultV1::Malformed,
            CurrentV1MountFaultV1::IdentityMismatch,
            CurrentV1MountFaultV1::UnsupportedVersion {
                observed: 2,
                supported: 1,
            },
        ] {
            let current_v1 = CurrentV1MountObservationV1::Unavailable(fault);
            assert_eq!(
                admit_session_mount_v1(current_v1.clone(), missing.clone()),
                SessionMountAdmissionV1::Rejected {
                    reason: SessionMountRejectionV1::UnavailableRepresentation,
                    current_v1,
                    canonical: missing.clone(),
                }
            );
        }

        let canonical_states = [
            SessionLookupState::Corrupt {
                session: SessionId::new("session-a").unwrap(),
                observed_revision: Some(revision("rev-a")),
                reason: CorruptSessionReason::DigestMismatch,
            },
            SessionLookupState::Unsupported {
                dimension: SchemaDimension::Physical,
                observed_revision: 3,
                supported_revision: 2,
            },
            SessionLookupState::conflict(
                ExpectedRevision::Exact(revision("rev-a")),
                Some(revision("rev-b")),
            )
            .unwrap(),
        ];
        for canonical in canonical_states {
            assert_eq!(
                admit_session_mount_v1(CurrentV1MountObservationV1::Missing, canonical.clone()),
                SessionMountAdmissionV1::Rejected {
                    reason: SessionMountRejectionV1::UnavailableRepresentation,
                    current_v1: CurrentV1MountObservationV1::Missing,
                    canonical,
                }
            );
        }

        let current_v1 =
            CurrentV1MountObservationV1::Unavailable(CurrentV1MountFaultV1::UnsupportedVersion {
                observed: 2,
                supported: 1,
            });
        let canonical = canonical_present();
        assert_eq!(
            admit_session_mount_v1(current_v1.clone(), canonical.clone()),
            SessionMountAdmissionV1::Rejected {
                reason: SessionMountRejectionV1::UnavailableRepresentation,
                current_v1,
                canonical,
            }
        );
    }

    #[test]
    fn canonical_tombstone_blocks_empty_and_current_v1_routes() {
        let canonical = SessionLookupState::Missing {
            tombstone_revision: Some(revision("rev-deleted")),
        };
        for current_v1 in [
            CurrentV1MountObservationV1::Missing,
            CurrentV1MountObservationV1::Present,
        ] {
            assert_eq!(
                admit_session_mount_v1(current_v1.clone(), canonical.clone()),
                SessionMountAdmissionV1::Rejected {
                    reason: SessionMountRejectionV1::CanonicalTombstone,
                    current_v1,
                    canonical: canonical.clone(),
                }
            );
        }
    }

    #[test]
    fn observation_of_missing_state_creates_nothing() {
        let root = tempfile::tempdir().unwrap();
        let session_root = root.path().join("sessions");
        std::fs::create_dir(&session_root).unwrap();
        let directory =
            super::super::PrivateDirectory::open(&session_root, "mount fixture").unwrap();
        let before = std::fs::read_dir(&session_root)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();

        let admission = observe_session_mount_v1(
            &directory,
            kitsu_core::session::StoreId::new("store-a").unwrap(),
            &SessionId::new("session-a").unwrap(),
        );

        assert_eq!(admission, SessionMountAdmissionV1::Empty);
        assert_eq!(
            std::fs::read_dir(&session_root)
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect::<Vec<_>>(),
            before
        );
        assert!(!session_root.join(".kitsu-store-v1").exists());
    }

    #[test]
    fn current_v1_observation_is_exact_bounded_and_byte_preserving() {
        let root = tempfile::tempdir().unwrap();
        let session_root = root.path().join("sessions");
        std::fs::create_dir(&session_root).unwrap();
        let directory =
            super::super::PrivateDirectory::open(&session_root, "mount fixture").unwrap();
        let store = StoreId::new("store-a").unwrap();
        let session = SessionId::new("session-a").unwrap();
        let path = session_root.join("session-a.json");

        let cases = [
            (
                current_v1_bytes(1, "session-a"),
                SessionMountAdmissionV1::CurrentV1Only,
            ),
            (
                b"{".to_vec(),
                SessionMountAdmissionV1::Rejected {
                    reason: SessionMountRejectionV1::UnavailableRepresentation,
                    current_v1: CurrentV1MountObservationV1::Unavailable(
                        CurrentV1MountFaultV1::Malformed,
                    ),
                    canonical: SessionLookupState::Missing {
                        tombstone_revision: None,
                    },
                },
            ),
            (
                current_v1_bytes(2, "session-a"),
                SessionMountAdmissionV1::Rejected {
                    reason: SessionMountRejectionV1::UnavailableRepresentation,
                    current_v1: CurrentV1MountObservationV1::Unavailable(
                        CurrentV1MountFaultV1::UnsupportedVersion {
                            observed: 2,
                            supported: 1,
                        },
                    ),
                    canonical: SessionLookupState::Missing {
                        tombstone_revision: None,
                    },
                },
            ),
            (
                current_v1_bytes(1, "session-other"),
                SessionMountAdmissionV1::Rejected {
                    reason: SessionMountRejectionV1::UnavailableRepresentation,
                    current_v1: CurrentV1MountObservationV1::Unavailable(
                        CurrentV1MountFaultV1::IdentityMismatch,
                    ),
                    canonical: SessionLookupState::Missing {
                        tombstone_revision: None,
                    },
                },
            ),
        ];

        for (bytes, expected) in cases {
            std::fs::write(&path, &bytes).unwrap();
            assert_eq!(
                observe_session_mount_v1(&directory, store.clone(), &session),
                expected
            );
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
            assert!(!session_root.join(".kitsu-store-v1").exists());
        }
    }

    #[test]
    fn canonical_observation_routes_present_and_refuses_dual_presence_without_effects() {
        let root = tempfile::tempdir().unwrap();
        let session_root = root.path().join("sessions");
        let store_id = StoreId::new("store-a").unwrap();
        let session = SessionId::new("session-a").unwrap();
        let source = current_v1_bytes(1, session.as_str());
        let store = super::super::physical::PhysicalFileStoreV1::create_or_open(
            &session_root,
            store_id.clone(),
        )
        .unwrap();
        super::super::transaction::publish_current_v1_mount_fixture_v1(
            &store,
            &source,
            &session,
            revision("rev-a"),
            "2026-08-20T10:02:00.000Z".to_string(),
        )
        .unwrap();
        drop(store);
        let directory =
            super::super::PrivateDirectory::open(&session_root, "mount fixture").unwrap();
        let before = tree_snapshot(&session_root);

        assert_eq!(
            observe_session_mount_v1(&directory, store_id.clone(), &session),
            SessionMountAdmissionV1::CanonicalOnly {
                revision: revision("rev-a"),
                head: LogicalHead::Genesis,
            }
        );
        assert_eq!(tree_snapshot(&session_root), before);

        std::fs::write(session_root.join("session-a.json"), &source).unwrap();
        let before_dual = tree_snapshot(&session_root);
        assert!(matches!(
            observe_session_mount_v1(&directory, store_id, &session),
            SessionMountAdmissionV1::Rejected {
                reason: SessionMountRejectionV1::AmbiguousDualPresence,
                current_v1: CurrentV1MountObservationV1::Present,
                canonical: SessionLookupState::Present { ref revision, ref head },
            } if revision.as_str() == "rev-a" && head == &LogicalHead::Genesis
        ));
        assert_eq!(tree_snapshot(&session_root), before_dual);
    }

    #[test]
    fn canonical_store_corruption_is_typed_and_never_repaired() {
        let root = tempfile::tempdir().unwrap();
        let session_root = root.path().join("sessions");
        let store_id = StoreId::new("store-a").unwrap();
        let session = SessionId::new("session-a").unwrap();
        drop(
            super::super::physical::PhysicalFileStoreV1::create_or_open(
                &session_root,
                store_id.clone(),
            )
            .unwrap(),
        );
        let directory =
            super::super::PrivateDirectory::open(&session_root, "mount fixture").unwrap();
        let record = super::super::physical::PhysicalLayoutV1::store_record(&session_root);
        let mut value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&record).unwrap()).unwrap();
        value["physical_schema_revision"] = serde_json::json!(3);
        std::fs::write(&record, serde_json::to_vec(&value).unwrap()).unwrap();
        let before = tree_snapshot(&session_root);

        assert!(matches!(
            observe_session_mount_v1(&directory, store_id, &session),
            SessionMountAdmissionV1::Rejected {
                reason: SessionMountRejectionV1::UnavailableRepresentation,
                current_v1: CurrentV1MountObservationV1::Missing,
                canonical: SessionLookupState::Unsupported {
                    dimension: SchemaDimension::Physical,
                    observed_revision: 3,
                    supported_revision: 2,
                },
            }
        ));
        assert_eq!(tree_snapshot(&session_root), before);

        let expected_other = StoreId::new("store-other").unwrap();
        value["physical_schema_revision"] = serde_json::json!(2);
        std::fs::write(&record, serde_json::to_vec(&value).unwrap()).unwrap();
        let identity_before = tree_snapshot(&session_root);
        assert!(matches!(
            observe_session_mount_v1(&directory, expected_other, &session),
            SessionMountAdmissionV1::Rejected {
                canonical: SessionLookupState::Corrupt {
                    reason: CorruptSessionReason::IdentityMismatch,
                    observed_revision: None,
                    ..
                },
                ..
            }
        ));
        assert_eq!(tree_snapshot(&session_root), identity_before);

        value["physical_schema_revision"] = serde_json::json!(2);
        std::fs::write(&record, serde_json::to_vec(&value).unwrap()).unwrap();
        let staging = super::super::physical::PhysicalLayoutV1::staging_root(&session_root);
        std::fs::remove_dir(&staging).unwrap();
        let partial_before = tree_snapshot(&session_root);
        assert!(matches!(
            observe_session_mount_v1(&directory, StoreId::new("store-a").unwrap(), &session),
            SessionMountAdmissionV1::Rejected {
                canonical: SessionLookupState::Corrupt {
                    reason: CorruptSessionReason::Malformed,
                    observed_revision: None,
                    ..
                },
                ..
            }
        ));
        assert_eq!(tree_snapshot(&session_root), partial_before);
        assert!(!staging.exists());
    }

    #[cfg(unix)]
    #[test]
    fn observation_refuses_symlinked_current_snapshot_without_reading_target() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let session_root = root.path().join("sessions");
        std::fs::create_dir(&session_root).unwrap();
        let outside = root.path().join("outside.json");
        let bytes = current_v1_bytes(1, "session-a");
        std::fs::write(&outside, &bytes).unwrap();
        symlink(&outside, session_root.join("session-a.json")).unwrap();
        let directory =
            super::super::PrivateDirectory::open(&session_root, "mount fixture").unwrap();

        assert!(matches!(
            observe_session_mount_v1(
                &directory,
                StoreId::new("store-a").unwrap(),
                &SessionId::new("session-a").unwrap()
            ),
            SessionMountAdmissionV1::Rejected {
                current_v1: CurrentV1MountObservationV1::Unavailable(
                    CurrentV1MountFaultV1::Malformed
                ),
                ..
            }
        ));
        assert_eq!(std::fs::read(&outside).unwrap(), bytes);
        assert!(!session_root.join(".kitsu-store-v1").exists());
    }

    #[cfg(unix)]
    #[test]
    fn observation_refuses_symlinked_canonical_root_without_traversal() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let session_root = root.path().join("sessions");
        let outside = root.path().join("outside-store");
        std::fs::create_dir(&session_root).unwrap();
        std::fs::create_dir(&outside).unwrap();
        let marker = outside.join("marker");
        std::fs::write(&marker, b"untouched").unwrap();
        symlink(&outside, session_root.join(".kitsu-store-v1")).unwrap();
        let directory =
            super::super::PrivateDirectory::open(&session_root, "mount fixture").unwrap();

        assert!(matches!(
            observe_session_mount_v1(
                &directory,
                StoreId::new("store-a").unwrap(),
                &SessionId::new("session-a").unwrap()
            ),
            SessionMountAdmissionV1::Rejected {
                canonical: SessionLookupState::Corrupt {
                    reason: CorruptSessionReason::Malformed,
                    observed_revision: None,
                    ..
                },
                ..
            }
        ));
        assert_eq!(std::fs::read(&marker).unwrap(), b"untouched");
        assert!(
            std::fs::symlink_metadata(session_root.join(".kitsu-store-v1"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    #[test]
    fn selection_retains_exact_current_value_from_the_observed_descriptor() {
        let root = tempfile::tempdir().unwrap();
        let session_root = root.path().join("sessions");
        std::fs::create_dir(&session_root).unwrap();
        let path = session_root.join("session-a.json");
        let expected = current_v1_bytes(1, "session-a");
        std::fs::write(&path, &expected).unwrap();
        let directory =
            super::super::PrivateDirectory::open(&session_root, "mount fixture").unwrap();

        let selection = select_session_mount_v1(
            &directory,
            StoreId::new("store-a").unwrap(),
            &SessionId::new("session-a").unwrap(),
        );
        std::fs::write(&path, current_v1_bytes(1, "session-new")).unwrap();

        let SessionMountSelectionV1::CurrentV1(value) = selection else {
            panic!("expected selected current-v1 value")
        };
        assert_eq!(value.saved().id, "session-a");
        assert_eq!(value.source_bytes(), expected);
    }

    #[test]
    fn selection_retains_verified_canonical_value_and_rejection_retains_none() {
        let root = tempfile::tempdir().unwrap();
        let session_root = root.path().join("sessions");
        let store_id = StoreId::new("store-a").unwrap();
        let session = SessionId::new("session-a").unwrap();
        let revision = revision("rev-a");
        let source = current_v1_bytes(1, session.as_str());
        let store = super::super::physical::PhysicalFileStoreV1::create_or_open(
            &session_root,
            store_id.clone(),
        )
        .unwrap();
        super::super::transaction::publish_current_v1_mount_fixture_v1(
            &store,
            &source,
            &session,
            revision.clone(),
            "2026-08-20T10:02:00.000Z".to_string(),
        )
        .unwrap();
        drop(store);
        let directory =
            super::super::PrivateDirectory::open(&session_root, "mount fixture").unwrap();

        let selected = select_session_mount_v1(&directory, store_id.clone(), &session);
        let SessionMountSelectionV1::Canonical(payload) = selected else {
            panic!("expected selected canonical value")
        };
        std::fs::write(
            super::super::physical::PhysicalLayoutV1::canonical_payload(
                &session_root,
                &session,
                &revision,
            ),
            b"{",
        )
        .unwrap();
        assert_eq!(payload.session_id(), &session);
        assert_eq!(payload.revision_id(), &revision);
        assert_eq!(payload.selected_head(), &LogicalHead::Genesis);
        assert!(matches!(
            select_session_mount_v1(&directory, store_id.clone(), &session),
            SessionMountSelectionV1::Rejected(SessionMountAdmissionV1::Rejected {
                reason: SessionMountRejectionV1::UnavailableRepresentation,
                ..
            })
        ));

        let store = super::super::physical::PhysicalFileStoreV1::create_or_open(
            &root.path().join("dual-sessions"),
            store_id.clone(),
        )
        .unwrap();
        super::super::transaction::publish_current_v1_mount_fixture_v1(
            &store,
            &source,
            &session,
            revision,
            "2026-08-20T10:02:00.000Z".to_string(),
        )
        .unwrap();
        drop(store);
        let dual_root = root.path().join("dual-sessions");
        std::fs::write(dual_root.join("session-a.json"), source).unwrap();
        let dual_directory =
            super::super::PrivateDirectory::open(&dual_root, "mount fixture").unwrap();
        assert!(matches!(
            select_session_mount_v1(&dual_directory, store_id, &session),
            SessionMountSelectionV1::Rejected(SessionMountAdmissionV1::Rejected {
                reason: SessionMountRejectionV1::AmbiguousDualPresence,
                ..
            })
        ));
    }
}
